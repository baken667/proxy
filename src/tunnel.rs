//! Device-wide tunnel: a TUN interface + default routes, with every packet
//! forwarded to the SOCKS5 proxy by tun2proxy.
//!
//! Creating a TUN device needs root/admin, so this same binary also runs as
//! an elevated helper. How it gets there depends on the system:
//!
//! - macOS: the first connect installs the helper as a launchd service (one
//!   password prompt, see `daemon`). After that launchd starts it whenever
//!   the app opens the service's socket, with no prompt, until an update
//!   changes the helper itself.
//! - Windows and Linux: the first connect of every launch re-runs the binary
//!   elevated as `bkn-proxy helper <port> <token-file>` (UAC or pkexec), and
//!   the helper connects back to the app over loopback.
//!
//! Either way the helper stays up while the app does, so switching proxies is
//! instant.
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
#[cfg(not(target_os = "macos"))]
use std::net::Ipv4Addr;
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};

use anyhow::{Context, Result, bail};
#[cfg(not(target_os = "macos"))]
use tokio::net::{TcpListener, TcpStream};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
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

/// What the helper is built from (build.rs). The app reinstalls the macOS
/// service when the installed one answers with another value.
#[cfg(target_os = "macos")]
const BUILD: &str = env!("BKN_HELPER_BUILD");

pub fn helper_main(args: &[String]) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        // macOS: launchd accepted the app's connection and handed it to us as
        // stdin (see `daemon`). First of all, say which build this is.
        #[cfg(target_os = "macos")]
        let (read, mut write) = {
            let _ = args;
            // SAFETY: fd 0 is ours alone; nothing else in the helper reads stdin.
            let stream = unsafe { <std::os::unix::net::UnixStream as std::os::fd::FromRawFd>::from_raw_fd(0) };
            stream.set_nonblocking(true)?;
            let (read, mut write) = tokio::net::UnixStream::from_std(stream).context("the helper is started by launchd, not by hand")?.into_split();
            write.write_all(format!("hello {BUILD}\n").as_bytes()).await?;
            (read, write)
        };
        #[cfg(not(target_os = "macos"))]
        let (read, mut write) = {
            let [port, token_file] = args else { bail!("usage: bkn-proxy helper <port> <token-file>") };
            let token = std::fs::read_to_string(token_file).context("read token file")?;
            let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port.parse::<u16>()?)).await?;
            let (read, mut write) = stream.into_split();
            write.write_all(format!("{}\n", token.trim()).as_bytes()).await?;
            (read, write)
        };

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

    /// Starts the elevated helper and waits until the app is connected to
    /// it. Every state line it reports afterwards is forwarded to `states`.
    /// `prompt` is what the system's password dialog says, when there is one.
    pub async fn launch(states: mpsc::UnboundedSender<String>, prompt: &str) -> Result<Tunnel> {
        #[cfg(target_os = "macos")]
        let connected = daemon::connect(prompt).await;
        #[cfg(not(target_os = "macos"))]
        let connected = {
            let _ = prompt; // UAC and pkexec word their own dialogs
            elevated().await.map(|stream| {
                let (read, write) = stream.into_inner().into_split();
                (BufReader::new(read), write)
            })
        };
        let (read, mut write) = connected.inspect_err(|error| tracing::error!("tunnel helper did not start: {error:#}"))?;
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
            let mut lines = read.lines();
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

/// Windows and Linux: asks for administrator rights, runs the helper and
/// waits for it to connect back.
#[cfg(not(target_os = "macos"))]
async fn elevated() -> Result<BufReader<TcpStream>> {
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
            // Windows: the launcher exits as soon as the prompt is answered.
            // Linux: pkexec stays for the helper's lifetime.
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
    accepted
}

/// First connection whose first line is our token wins.
#[cfg(not(target_os = "macos"))]
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

#[cfg(not(target_os = "macos"))]
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
#[cfg(not(target_os = "macos"))]
fn elevate(port: u16, token_file: &Path) -> Result<Command> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().context("non-UTF-8 executable path")?;
    let file = token_file.to_str().context("non-UTF-8 temp path")?;

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

// ------------------------------------------------------- macOS: the service

/// The helper as a launchd service. Installing it takes one password prompt.
/// From then on launchd listens on a socket and, for every connection the app
/// opens, runs one helper as root with that connection as its stdin. The
/// helper exits when the app closes the connection, as it always did.
///
/// Who may connect: only the user who installed it (the socket is theirs,
/// mode 0600). Any program of that user can therefore route the device
/// through a proxy of its choice without a prompt; that is the price of not
/// being asked every time, and what the line protocol lets them do ends
/// there. What runs as root is a copy of this binary that only root can
/// replace.
#[cfg(target_os = "macos")]
mod daemon {
    use tokio::net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    };

    use super::*;

    /// A debug build gets a service of its own: `cargo run` must never replace
    /// the installed app's helper, which may be carrying a tunnel.
    const LABEL: &str = if cfg!(debug_assertions) { "kz.bkn.proxy.helper.dev" } else { "kz.bkn.proxy.helper" };

    fn socket() -> String {
        format!("/var/run/{LABEL}.sock")
    }

    type Link = (BufReader<OwnedReadHalf>, OwnedWriteHalf);

    /// Connects to the installed helper; installs it first if it is missing
    /// or built from other sources than this app.
    pub async fn connect(prompt: &str) -> Result<Link> {
        match hello().await {
            Ok(link) => Ok(link),
            Err(error) => {
                tracing::info!("installing the tunnel helper (asks for administrator access): {error:#}");
                install(prompt).await?;
                hello().await.context("helper_not_started") // a code: see Strings::message
            }
        }
    }

    async fn hello() -> Result<Link> {
        let (read, write) = UnixStream::connect(socket()).await?.into_split();
        let mut read = BufReader::new(read);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(15), read.read_line(&mut line)).await.context("the helper did not answer")??;
        if line.trim_end() != format!("hello {BUILD}") {
            bail!("the installed helper is another build"); // dropping the connection ends it
        }
        Ok((read, write))
    }

    unsafe extern "C" {
        safe fn getuid() -> u32;
    }

    async fn install(prompt: &str) -> Result<()> {
        let exe = std::env::current_exe()?;
        let script = install_script(exe.to_str().context("non-UTF-8 executable path")?, getuid());
        let quoted = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("do shell script \"{}\" with prompt \"{}\" with administrator privileges", quoted(&script), quoted(prompt));
        let out = Command::new("osascript").args(["-e", &script]).output().await.context("start elevation prompt")?;
        if out.status.success() {
            return Ok(());
        }
        let why = String::from_utf8_lossy(&out.stderr);
        if why.contains("-128") {
            bail!("elevation_denied"); // Cancel in the password dialog
        }
        Err(anyhow::anyhow!("{}", why.trim())).context("helper_not_started")
    }

    /// Where root's copy of the helper lives. A bare binary (`cargo run`) is
    /// copied as it is. An installed app is copied whole, as `<this>.app`:
    /// the signature of a signed app's executable is bound to the bundle
    /// around it (its Info.plist), and macOS kills the executable at launch
    /// when it is run without one. That is what 0.3.0 did, so its helper
    /// never started.
    fn home() -> String {
        format!("/Library/PrivilegedHelperTools/{LABEL}")
    }

    /// The helper's program as launchd runs it, for the app at `exe`.
    fn program(exe: &str) -> String {
        let exe = Path::new(exe);
        match crate::update::bundle_of(exe).and_then(|bundle| exe.strip_prefix(bundle).ok()) {
            Some(inside) => format!("{}.app/{}", home(), inside.display()), // .../Contents/MacOS/bkn-proxy
            None => home(),
        }
    }

    /// What runs as root, once: one line of /bin/sh.
    fn install_script(exe: &str, uid: u32) -> String {
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
        let (home, plist_file) = (home(), format!("/Library/LaunchDaemons/{LABEL}.plist"));
        let (source, copy) = match crate::update::bundle_of(Path::new(exe)) {
            Some(bundle) => (bundle.display().to_string(), format!("{home}.app")),
            None => (exe.to_string(), home.clone()),
        };
        [
            "set -e".to_string(),
            "mkdir -p /Library/PrivilegedHelperTools".into(),
            // Copied aside, then moved into place: never a half-written helper.
            // -X: without the download quarantine mark of the app.
            format!("rm -rf {copy}.new"),
            format!("cp -RX {} {copy}.new", quote(&source)),
            // Root's own, and nobody else's to change: this is what runs as root.
            format!("chown -R root:wheel {copy}.new"),
            format!("chmod -R go-w {copy}.new"),
            // Both shapes of an earlier copy go; a helper that is running keeps its open files.
            format!("rm -rf {home} {home}.app"),
            format!("mv {copy}.new {copy}"),
            format!("printf %s {} > {plist_file}", quote(&plist(uid, &program(exe)))),
            format!("chown root:wheel {plist_file}"),
            format!("chmod 644 {plist_file}"),
            format!("launchctl bootout system/{LABEL} 2>/dev/null || true"),
            // bootout returns before launchd has let go of the old service.
            format!("n=0; until launchctl bootstrap system {plist_file}; do n=$((n+1)); [ $n -lt 10 ] || exit 1; sleep 0.5; done"),
        ]
        .join("; ")
    }

    /// The service: launchd owns the socket, accepts on it and starts one
    /// helper per connection (`inetdCompatibility`, the way macOS runs sshd).
    /// 384 is mode 0600; a property list has no octal.
    fn plist(uid: u32, program: &str) -> String {
        format!(
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#,
                r#"<plist version="1.0"><dict>"#,
                "<key>Label</key><string>{label}</string>",
                "<key>ProgramArguments</key><array><string>{program}</string><string>helper</string></array>",
                "<key>Sockets</key><dict><key>Listeners</key><dict>",
                "<key>SockPathName</key><string>{socket}</string>",
                "<key>SockPathMode</key><integer>384</integer>",
                "<key>SockPathOwner</key><integer>{uid}</integer>",
                "</dict></dict>",
                "<key>inetdCompatibility</key><dict><key>Wait</key><false/></dict>",
                // The helper's stderr must not end up in the connection.
                "<key>StandardErrorPath</key><string>/dev/null</string>",
                "</dict></plist>"
            ),
            label = LABEL,
            program = program,
            socket = socket(),
            uid = uid
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The two things root is handed are well-formed: launchd's own tool
        /// reads the property list, and sh parses the script without running it.
        #[test]
        fn service_files_are_well_formed() {
            // An installed app is copied whole and run from inside the copy; a bare binary is copied alone.
            let installed = "/Applications/it's here/bkn-proxy.app/Contents/MacOS/bkn-proxy";
            assert_eq!(program(installed), format!("{}.app/Contents/MacOS/bkn-proxy", home()));
            assert_eq!(program("/code/target/debug/bkn-proxy"), home());
            assert!(install_script(installed, 501).contains("cp -RX '/Applications/it'\\''s here/bkn-proxy.app' "));
            assert!(install_script("/code/target/debug/bkn-proxy", 501).contains("cp -RX '/code/target/debug/bkn-proxy' "));

            let file = std::env::temp_dir().join(format!("bkn-proxy-test-{}.plist", std::process::id()));
            std::fs::write(&file, plist(501, &program(installed))).unwrap();
            let lint = std::process::Command::new("plutil").arg("-lint").arg(&file).output().unwrap();
            let _ = std::fs::remove_file(&file);
            assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));

            for exe in [installed, "/code/target/debug/bkn-proxy"] {
                let script = install_script(exe, 501);
                assert!(!script.contains('\n'), "an AppleScript string is one line");
                let parsed = std::process::Command::new("sh").args(["-n", "-c", &script]).output().unwrap();
                assert!(parsed.status.success(), "{}", String::from_utf8_lossy(&parsed.stderr));
            }
        }
    }
}
