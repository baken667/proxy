//! Device-wide tunnel: a TUN interface + default routes, with every packet
//! forwarded to the SOCKS5 proxy by tun2proxy.
//!
//! Creating a TUN device needs root/admin, so this same binary is re-launched
//! elevated as `bkn-proxy helper <port> <token-file>` on the first connect
//! (one OS password prompt). The helper stays up and connects back to the app
//! over loopback, so switching proxies afterwards is instant and prompt-free.
//!
//!   app -> helper:  `connect <socks5 url>` | `disconnect` | `repair`
//!   helper -> app:  `up` | `down [reason]` | `repaired`
//!
//!                   `traffic <sent> <received>`  (bytes so far, once a second while up)
//!                   `log <LEVEL> <message>`  (warnings and errors from tun2proxy)
//!
//! The helper never writes a log file: it runs as root, and a root process
//! writing to a predictable path is how local privilege escalations happen.
//! Its log lines travel over the same connection and the app prints them.
//!
//! The helper exits when that connection closes (app quit or crashed), which
//! tears the tunnel down. How the routes and DNS are set, and why the system
//! undoes them by itself even if the helper dies: see net.rs.
use std::{
    net::Ipv4Addr,
    path::Path,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::mpsc,
    task::JoinHandle,
};
use tun2proxy::{ArgDns, Args, CancellationToken, ProxyParameters};

// ------------------------------------------------------------- helper (root)

/// Bytes through the tunnel since the helper started, kept current by tun2proxy.
static SENT: AtomicU64 = AtomicU64::new(0);
static RECEIVED: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn on_traffic(status: *const tun2proxy::TrafficStatus, _: *mut std::ffi::c_void) {
    // SAFETY: tun2proxy passes a pointer to a live struct, or null.
    if let Some(status) = unsafe { status.as_ref() } {
        SENT.store(status.tx, Relaxed);
        RECEIVED.store(status.rx, Relaxed);
    }
}

pub fn helper_main(port: &str, token_file: &str) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        let token = std::fs::read_to_string(token_file).context("read token file")?;
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port.parse::<u16>()?)).await?;
        let (read, mut write) = stream.into_split();
        write.write_all(format!("{}\n", token.trim()).as_bytes()).await?;

        let mut commands = BufReader::new(read).lines();
        let (states, mut state_rx) = mpsc::unbounded_channel::<String>();
        // Warn and above only: at info tun2proxy logs every connection's
        // destination, which is the user's browsing history.
        if log::set_boxed_logger(Box::new(Forward(states.clone()))).is_ok() {
            log::set_max_level(log::LevelFilter::Warn);
        }
        // SAFETY: a plain function and no context pointer, so nothing can dangle.
        unsafe { tun2proxy::tun2proxy_set_traffic_status_callback(1, Some(on_traffic), std::ptr::null_mut()) };
        // tun2proxy only calls back while bytes flow; our own tick is what
        // lets the app draw an idle second as zero.
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        // The tick is only polled while a tunnel runs; never replay the seconds in between.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut running: Option<(CancellationToken, JoinHandle<()>)> = None;
        loop {
            tokio::select! {
                line = commands.next_line() => {
                    let Ok(Some(line)) = line else { break }; // app is gone
                    stop(&mut running).await;
                    match line.strip_prefix("connect ") {
                        Some(url) => running = Some(start(url.to_string(), states.clone())),
                        None if line == "repair" => {
                            let _ = tokio::task::spawn_blocking(crate::net::repair).await;
                            let _ = states.send("repaired".into());
                        }
                        None => { let _ = states.send("down".into()); }
                    }
                }
                Some(state) = state_rx.recv() => {
                    if write.write_all(format!("{}\n", state.replace('\n', " ")).as_bytes()).await.is_err() {
                        break;
                    }
                }
                _ = tick.tick(), if running.as_ref().is_some_and(|(_, task)| !task.is_finished()) => {
                    let line = format!("traffic {} {}\n", SENT.load(Relaxed), RECEIVED.load(Relaxed));
                    if write.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
            }
        }
        stop(&mut running).await;
        Ok(())
    })
}

/// Sends tun2proxy's `log` records to the app as `log <LEVEL> <message>` lines.
struct Forward(mpsc::UnboundedSender<String>);

impl log::Log for Forward {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let _ = self.0.send(format!("log {} {}", record.level(), record.args()));
        }
    }
    fn flush(&self) {}
}

/// What tun2proxy is told for every connection.
fn tunnel_args(url: &str) -> Result<Args> {
    let mut args = Args::default();
    args.proxy(ProxyParameters::try_from(url)?).dns(ArgDns::Virtual); // names are resolved by the proxy: no DNS leak
    // tun2proxy's default of 200 open connections is soon reached by a browser
    // plus background apps when the whole device goes through the tunnel, and
    // it drops new connections past the limit: pages stall half-loaded.
    args.max_sessions = 4096;
    Ok(args)
}

fn start(url: String, states: mpsc::UnboundedSender<String>) -> (CancellationToken, JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        // The tunnel runs in a task of its own so that a panic inside it comes
        // back here as an error and is reported. Otherwise the app would keep
        // showing "Connected" over a tunnel that no longer exists.
        let run = tokio::spawn(run_tunnel(url, token.clone(), states.clone()));
        let reason = match run.await {
            Ok(Ok(())) => "tunnel stopped".to_string(),
            Ok(Err(error)) => format!("{error:#}"),
            Err(crash) => format!("tunnel crashed: {crash}"),
        };
        if !token.is_cancelled() {
            let _ = states.send(format!("down {reason}"));
        }
    });
    (cancel, task)
}

/// macOS and Windows: we make the tun device and the network changes
/// ourselves (net.rs), so they undo themselves if the helper dies.
#[cfg(any(target_os = "macos", windows))]
async fn run_tunnel(url: String, token: CancellationToken, states: mpsc::UnboundedSender<String>) -> Result<()> {
    let mut args = tunnel_args(&url)?;
    // Find the proxy's address now, while the system's own DNS still answers.
    let proxy = args.proxy.addr.as_ref().context("the proxy URL has no address")?.resolve_using_tokio().await?.next().context("the proxy's name doesn't resolve")?;
    args.proxy.addr = Some(proxy.into());

    // What an older version left behind would fight the new routes.
    tokio::task::spawn_blocking(crate::net::repair).await?;

    let mut config = tun::Configuration::default();
    config.address(crate::net::TUN_ADDR).netmask(crate::net::TUN_MASK).destination(crate::net::TUN_PEER).mtu(tun2proxy::DEFAULT_MTU).up();
    // A macOS utun device puts a 4-byte header on every packet, and `true` is
    // what makes the tun crate strip and add it. With `false` the tunnel comes
    // up but no packet parses, so nothing flows.
    #[cfg(target_os = "macos")]
    config.platform_config(|c| {
        c.packet_information(true);
    });
    #[cfg(windows)]
    {
        config.tun_name("bkn-proxy");
        config.platform_config(|c| {
            c.device_guid(0x6b6e_2d70_726f_7879_0000_0000_0000_0001);
            // The adapter's own DNS: it goes away with the adapter.
            c.dns_servers(&[crate::net::TUN_PEER.into()]);
            c.wait_for_interfaces(true, false, Duration::from_secs(10));
        });
    }
    let device = tun::create_as_async(&config).context("can't create the tunnel device")?;
    let name = tun::AbstractDevice::tun_name(&*device)?;
    let network = tokio::task::spawn_blocking(move || crate::net::Network::apply(&name, proxy.ip())).await??;
    let _ = states.send("up".into());

    let result = tun2proxy::run(device, tun2proxy::DEFAULT_MTU, args, token).await;
    // Undo the routes and DNS (blocking commands) before reporting anything.
    tokio::task::spawn_blocking(move || drop(network)).await?;
    result?;
    Ok(())
}

/// Linux: tun2proxy's own setup, as before.
// ponytail: unlike macOS and Windows, this doesn't undo itself if the helper
// is killed. Port net.rs to Linux (ip route, resolvectl) when Linux ships.
#[cfg(not(any(target_os = "macos", windows)))]
async fn run_tunnel(url: String, token: CancellationToken, states: mpsc::UnboundedSender<String>) -> Result<()> {
    let mut args = tunnel_args(&url)?;
    args.setup(true);
    let run = tun2proxy::general_run_async(args, tun2proxy::DEFAULT_MTU, false, token);
    tokio::pin!(run);
    // tun2proxy has no "ready" signal; setup failures return within milliseconds.
    if let Ok(early) = tokio::time::timeout(Duration::from_secs(1), &mut run).await {
        early?;
        return Ok(());
    }
    let _ = states.send("up".into());
    run.await?;
    Ok(())
}

async fn stop(running: &mut Option<(CancellationToken, JoinHandle<()>)>) {
    if let Some((cancel, task)) = running.take() {
        cancel.cancel();
        let _ = task.await; // routes are restored before we return
    }
}

// ----------------------------------------------------------------- app side

pub struct Tunnel {
    commands: mpsc::UnboundedSender<String>,
}

impl Tunnel {
    /// False once the helper has exited; launch a new one.
    pub fn alive(&self) -> bool {
        !self.commands.is_closed()
    }

    pub fn connect(&self, url: &str) {
        // One command per line: a control character must never smuggle a second one.
        let _ = self.commands.send(format!("connect {}", url.replace(char::is_control, "")));
    }

    /// Undoes network settings an older version left behind; answers `repaired`.
    pub fn repair(&self) {
        let _ = self.commands.send("repair".into());
    }

    pub fn disconnect(&self) {
        let _ = self.commands.send("disconnect".into());
    }

    /// Starts the elevated helper and waits for it to connect back. Every
    /// state line it reports afterwards is forwarded to `states`.
    pub async fn launch(states: mpsc::UnboundedSender<String>) -> Result<Tunnel> {
        tracing::info!("starting the tunnel helper (asks for administrator access)");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        // Other local users can see the port but not this file, so only our
        // helper can authenticate. (Processes of the same user could; they
        // can already do everything this app can.)
        let token = format!("{:032x}{:032x}", rand::random::<u128>(), rand::random::<u128>());
        let token_file = std::env::temp_dir().join(format!("bkn-proxy-{:016x}.token", rand::random::<u64>()));
        write_private(&token_file, &token)?;

        let accepted: Result<BufReader<TcpStream>> = async {
            let mut child = elevate(port, &token_file)?.spawn().context("start elevation prompt")?;
            let accept = tokio::time::timeout(Duration::from_secs(120), accept_helper(&listener, &token));
            tokio::pin!(accept);
            let stream = tokio::select! {
                stream = &mut accept => stream,
                // macOS/Windows: the launcher exits as soon as the prompt is
                // answered. Linux: pkexec stays for the helper's lifetime.
                status = child.wait() => {
                    if !status?.success() {
                        bail!("elevation_denied"); // a code: see Strings::message
                    }
                    accept.await
                }
            };
            stream.context("helper_not_started")?
        }
        .await;
        let _ = std::fs::remove_file(&token_file);
        let (read, mut write) = accepted.inspect_err(|error| tracing::error!("tunnel helper did not start: {error:#}"))?.into_inner().into_split();
        tracing::info!("tunnel helper connected");

        let (commands, mut command_rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                if write.write_all(format!("{command}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(message) = line.strip_prefix("log ") {
                    tracing::warn!("tunnel helper: {message}");
                } else if states.send(line).is_err() {
                    return;
                }
            }
            writer.abort(); // drops the command receiver, so `alive()` turns false
            let _ = states.send("down helper_exited".into());
        });
        Ok(Tunnel { commands })
    }
}

/// First connection whose first line is our token wins.
async fn accept_helper(listener: &TcpListener, token: &str) -> Result<BufReader<TcpStream>> {
    loop {
        let (stream, _) = listener.accept().await?;
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        if stream.read_line(&mut line).await.is_ok() && line.trim() == token {
            return Ok(stream);
        }
    }
}

fn write_private(path: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // Windows: %TEMP% is already private to the user.
    Ok(options.open(path)?.write_all(content.as_bytes())?)
}

/// The command that re-runs this binary as `helper` with admin rights.
fn elevate(port: u16, token_file: &Path) -> Result<Command> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().context("non-UTF-8 executable path")?;
    let file = token_file.to_str().context("non-UTF-8 temp path")?;

    #[cfg(target_os = "macos")]
    let command = {
        let sh_quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        // `do shell script` runs /bin/sh as root; `&` detaches the helper so osascript returns.
        let sh = format!("{} helper {port} {} >/dev/null 2>&1 &", sh_quote(exe), sh_quote(file));
        let script = format!(
            "do shell script \"{}\" with prompt \"bkn-proxy needs administrator access to route this device's traffic.\" with administrator privileges",
            sh.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let mut c = Command::new("osascript");
        c.args(["-e", &script]);
        c
    };

    #[cfg(target_os = "linux")]
    let command = {
        let mut c = Command::new("pkexec");
        c.args([exe, "helper", &port.to_string(), file]);
        c
    };

    #[cfg(windows)]
    let command = {
        let ps_quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command"]).arg(format!(
            "Start-Process -FilePath {} -ArgumentList 'helper','{port}',{} -Verb RunAs -WindowStyle Hidden",
            ps_quote(exe),
            ps_quote(file)
        ));
        c
    };

    Ok(command)
}
