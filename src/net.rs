//! The system side of the tunnel: routes into the tun device and DNS through
//! it, set up by the elevated helper (`apply`), plus a check and a repair for
//! what older versions could leave behind.
//!
//! Everything `apply` changes is undone by the operating system on its own
//! when the helper goes away, crash or `kill -9` included, so a lost tunnel
//! can't leave the device without internet:
//! - It never touches the system's default route. Two half-routes,
//!   0.0.0.0/1 and 128.0.0.0/1, point into the tun device and win by being
//!   more specific; they vanish with the device.
//! - DNS points at the tunnel only through settings that live as long as the
//!   helper: on macOS keys in a SystemConfiguration session (removed when the
//!   session closes), on Windows the tun adapter's own DNS (removed with the
//!   adapter). Nothing is written to the network service's saved settings.
//! - The one route that can outlive a crash is the host route that keeps the
//!   proxy server itself outside the tunnel. It only ever covers that address
//!   and is replaced on the next connect.
//!
//! Versions 0.1.0 and 0.2.0 used tproxy-config, which wrote DNS into the saved
//! settings (`networksetup`) and swapped the default route; a crash left
//! those in place. `leftovers` spots that and `repair` undoes it.
use std::{
    net::{IpAddr, Ipv4Addr},
    process::Command,
};

use anyhow::{Context, Result, bail};

/// The tun device's address and its peer. TEST-NET-2 (RFC 5737): never used
/// by a real network, so it can't shadow the user's own LAN the way 10.0.0.x did.
pub const TUN_ADDR: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 1);
/// The far end of the tunnel, and the DNS server the system is pointed at:
/// queries to it travel into the tunnel, where tun2proxy answers them.
pub const TUN_PEER: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 2);
pub const TUN_MASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);
/// Where 0.1.0 and 0.2.0 pointed DNS and routes: seen anywhere, it is ours.
const LEGACY_TUNNEL: &str = "10.0.0.1";
/// The halves of the IPv4 space that together beat the default route.
const HALVES: [(&str, &str); 2] = [("0.0.0.0", "128.0.0.0"), ("128.0.0.0", "128.0.0.0")];

#[allow(unused_mut)] // mutated on Windows only
fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    // The app has no console on Windows: don't flash one for every command.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
    command
}

/// Runs a command; its output, or why it failed.
fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = command(program).args(args).output().with_context(|| format!("can't run {program}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        bail!("{program} {}: {}", args.join(" "), why.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// For undo and repair steps: a failure is logged and the next step still runs.
fn try_run(program: &str, args: &[&str]) {
    if let Err(error) = run(program, args) {
        log::debug!("{error:#}");
    }
}

/// On the local network: reached directly, so no route is needed to keep it
/// out of the tunnel.
fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// The network changes of one connection. Dropping it undoes them.
pub struct Network {
    tun: String,
    /// The proxy server's address, routed around the tunnel.
    bypass: Option<IpAddr>,
    halves: bool,
    #[cfg(target_os = "macos")]
    dns: Option<os::Session>,
}

impl Network {
    /// Sends everything except the proxy server itself into the tun device
    /// `tun`, and DNS with it. A step that fails undoes the ones before it.
    pub fn apply(tun: &str, proxy: IpAddr) -> Result<Network> {
        let mut network = Network {
            tun: tun.into(),
            bypass: None,
            halves: false,
            #[cfg(target_os = "macos")]
            dns: None,
        };
        if !is_local(proxy) {
            let gateway = os::default_gateway().context("no default route: is the device online?")?;
            os::add_host_route(proxy, &gateway)?;
            network.bypass = Some(proxy);
        }
        for (net, mask) in HALVES {
            os::add_half(net, mask, tun)?;
            network.halves = true;
        }
        // Without it DNS still works, it just isn't tunnelled: worth a warning, not a failure.
        #[cfg(target_os = "macos")]
        match os::dns_session(tun) {
            Ok(store) => network.dns = Some(store),
            Err(error) => log::warn!("DNS is not tunnelled: {error:#}"),
        }
        #[cfg(windows)]
        os::prefer_interface(tun);
        os::flush_dns();
        Ok(network)
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        drop(self.dns.take()); // closing the session removes its keys
        if self.halves {
            for (net, mask) in HALVES {
                os::remove_half(net, mask, &self.tun);
            }
        }
        if let Some(proxy) = self.bypass {
            os::remove_host_route(proxy);
        }
        os::flush_dns();
    }
}

/// Settings an older version left behind that can keep the device offline.
/// Reads only; any user can run it. Empty when all is well.
pub fn leftovers() -> Vec<String> {
    os::leftovers()
}

/// Undoes what `leftovers` finds. Needs administrator rights (the helper).
pub fn repair() {
    for found in os::leftovers() {
        log::warn!("repairing: {found}");
    }
    os::repair();
    os::flush_dns();
}

// ----------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod os {
    use super::*;
    use system_configuration::{
        core_foundation::{
            array::CFArray,
            base::{CFType, TCFType},
            boolean::CFBoolean,
            dictionary::CFDictionary,
            string::CFString,
        },
        dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder},
    };

    /// Our service in the dynamic store. Its keys belong to the helper's
    /// session, so configd deletes them when the helper exits.
    const SERVICE: &str = "State:/Network/Service/bkn-proxy-tunnel";

    /// `scutil`'s `show KEY`, as `name : value` pairs; array items come as `0 : value`.
    fn scutil_show(key: &str) -> Vec<(String, String)> {
        let Ok(child) = command("scutil")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            return vec![];
        };
        let mut child = child;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = writeln!(stdin, "show {key}");
        }
        let Ok(out) = child.wait_with_output() else { return vec![] };
        parse_scutil(&String::from_utf8_lossy(&out.stdout))
    }

    pub fn parse_scutil(text: &str) -> Vec<(String, String)> {
        text.lines()
            .filter_map(|line| {
                let (name, value) = line.split_once(" : ")?;
                Some((name.trim().to_string(), value.trim().to_string()))
            })
            .collect()
    }

    fn value(pairs: &[(String, String)], name: &str) -> Option<String> {
        pairs.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }

    /// `route -n get default`: the gateway, or None without a default route.
    fn route_default() -> Option<String> {
        let out = run("route", &["-n", "get", "default"]).ok()?;
        out.lines().find_map(|l| l.trim().strip_prefix("gateway:").map(|g| g.trim().to_string()))
    }

    /// The router of the primary network service, as the system knows it.
    fn primary() -> (Option<String>, Option<String>, Option<String>) {
        let global = scutil_show("State:/Network/Global/IPv4");
        (value(&global, "Router"), value(&global, "PrimaryInterface"), value(&global, "PrimaryService"))
    }

    pub fn default_gateway() -> Option<String> {
        route_default().filter(|g| g != LEGACY_TUNNEL).or_else(|| primary().0)
    }

    pub fn add_host_route(ip: IpAddr, gateway: &str) -> Result<()> {
        let ip = ip.to_string();
        // A stale one from a crash: replace it, the gateway may have changed.
        try_run("route", &["-n", "delete", "-host", &ip]);
        run("route", &["-n", "add", "-host", &ip, gateway]).map(drop)
    }

    pub fn remove_host_route(ip: IpAddr) {
        try_run("route", &["-n", "delete", "-host", &ip.to_string()]);
    }

    pub fn add_half(net: &str, mask: &str, tun: &str) -> Result<()> {
        run("route", &["-n", "add", "-net", net, "-netmask", mask, "-interface", tun]).map(drop)
    }

    pub fn remove_half(net: &str, mask: &str, tun: &str) {
        // Usually already gone with the device; this covers the device outliving us.
        try_run("route", &["-n", "delete", "-net", net, "-netmask", mask, "-interface", tun]);
    }

    /// Publishes our tunnel as a network service that takes over as primary
    /// (OverridePrimary, as a VPN set to send all traffic does), with the
    /// tunnel's peer as its DNS server.
    pub fn dns_session(tun: &str) -> Result<Session> {
        let store = SCDynamicStoreBuilder::new("bkn-proxy").session_keys(true).build();
        let s = |v: &str| CFString::new(v);
        let list = |v: &str| CFArray::from_CFTypes(&[CFString::new(v)]).as_CFType();
        let (addr, peer) = (TUN_ADDR.to_string(), TUN_PEER.to_string());
        let ipv4: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[
            (s("Addresses"), list(&addr)),
            (s("DestAddresses"), list(&peer)),
            (s("SubnetMasks"), list(&TUN_MASK.to_string())),
            (s("Router"), s(&peer).as_CFType()),
            (s("InterfaceName"), s(tun).as_CFType()),
            (s("OverridePrimary"), CFBoolean::true_value().as_CFType()),
        ]);
        let dns: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[(s("ServerAddresses"), list(&peer))]);
        if !store.set(format!("{SERVICE}/IPv4").as_str(), ipv4.to_untyped()) || !store.set(format!("{SERVICE}/DNS").as_str(), dns.to_untyped()) {
            bail!("the dynamic store refused our service");
        }
        Ok(Session(store))
    }

    /// The dynamic store session holding our service's keys; dropping it
    /// closes the session, and configd removes the keys.
    pub struct Session(#[allow(dead_code)] SCDynamicStore);

    // SAFETY: a CoreFoundation object may be released on any thread, and the
    // session is never used after it is set up: it is only kept and dropped.
    unsafe impl Send for Session {}

    pub fn flush_dns() {
        try_run("killall", &["-HUP", "mDNSResponder"]);
    }

    /// Network services whose saved DNS is the old tunnel's address.
    fn pinned_services() -> Vec<String> {
        let Ok(list) = run("networksetup", &["-listallnetworkservices"]) else { return vec![] };
        list.lines()
            .skip(1) // "An asterisk (*) denotes that a network service is disabled."
            .map(|l| l.trim_start_matches('*').trim().to_string())
            .filter(|svc| !svc.is_empty())
            .filter(|svc| run("networksetup", &["-getdnsservers", svc]).is_ok_and(|out| out.lines().any(|l| l.trim() == LEGACY_TUNNEL)))
            .collect()
    }

    /// The primary service's live DNS still points at the old tunnel.
    fn live_dns_pinned(service: &str) -> bool {
        scutil_show(&format!("State:/Network/Service/{service}/DNS")).iter().any(|(_, v)| v == LEGACY_TUNNEL)
    }

    pub fn leftovers() -> Vec<String> {
        let mut found: Vec<String> = pinned_services().into_iter().map(|svc| format!("{svc}: DNS set to {LEGACY_TUNNEL}")).collect();
        let (router, _, service) = primary();
        if let Some(service) = &service
            && live_dns_pinned(service)
        {
            found.push(format!("current DNS is {LEGACY_TUNNEL}"));
        }
        match route_default() {
            Some(gateway) if gateway == LEGACY_TUNNEL => found.push(format!("default route via {LEGACY_TUNNEL}")),
            None if router.is_some() => found.push("no default route".into()),
            _ => {}
        }
        found
    }

    pub fn repair() {
        for svc in pinned_services() {
            try_run("networksetup", &["-setdnsservers", &svc, "Empty"]);
        }
        let (router, interface, service) = primary();
        if route_default().as_deref() == Some(LEGACY_TUNNEL) {
            try_run("route", &["-n", "delete", "default", LEGACY_TUNNEL]);
        }
        if route_default().is_none()
            && let Some(router) = &router
        {
            try_run("route", &["-n", "add", "default", router]);
        }
        // The live DNS was overwritten in place; a DHCP service gets its own back
        // by asking the DHCP server again. A manual service is left alone.
        if let (Some(service), Some(interface)) = (&service, &interface)
            && live_dns_pinned(service)
            && value(&scutil_show(&format!("Setup:/Network/Service/{service}/IPv4")), "ConfigMethod").as_deref() == Some("DHCP")
        {
            try_run("ipconfig", &["set", interface, "DHCP"]);
        }
    }
}

// --------------------------------------------------------------- Windows

#[cfg(windows)]
mod os {
    use super::*;

    fn powershell(script: &str) -> Result<String> {
        run("powershell", &["-NoProfile", "-NonInteractive", "-Command", script])
    }

    /// The next hop of the best default route that isn't the tunnel's.
    pub fn default_gateway() -> Option<String> {
        let script = "Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue \
            | Where-Object { $_.NextHop -ne '0.0.0.0' -and $_.NextHop -ne '10.0.0.1' } \
            | Sort-Object { $_.RouteMetric + (Get-NetIPInterface -InterfaceIndex $_.InterfaceIndex -AddressFamily IPv4).InterfaceMetric } \
            | Select-Object -First 1 -ExpandProperty NextHop";
        let out = powershell(script).ok()?;
        out.trim().parse::<Ipv4Addr>().ok().map(|ip| ip.to_string())
    }

    pub fn add_host_route(ip: IpAddr, gateway: &str) -> Result<()> {
        let ip = ip.to_string();
        try_run("route", &["delete", &ip]);
        run("route", &["add", &ip, "mask", "255.255.255.255", gateway, "metric", "1"]).map(drop)
    }

    pub fn remove_host_route(ip: IpAddr) {
        try_run("route", &["delete", &ip.to_string()]);
    }

    // Non-persistent routes through the adapter's peer: Windows drops them
    // when the adapter goes away.
    pub fn add_half(net: &str, mask: &str, _tun: &str) -> Result<()> {
        run("route", &["add", net, "mask", mask, &TUN_PEER.to_string(), "metric", "1"]).map(drop)
    }

    pub fn remove_half(net: &str, mask: &str, _tun: &str) {
        try_run("route", &["delete", net, "mask", mask, &TUN_PEER.to_string()]);
    }

    /// Windows asks the DNS servers of the interface with the lowest metric
    /// first: make that the tunnel, whose DNS server is the tunnel's peer.
    pub fn prefer_interface(tun: &str) {
        try_run("netsh", &["interface", "ipv4", "set", "interface", tun, "metric=1"]);
    }

    pub fn flush_dns() {
        try_run("ipconfig", &["/flushdns"]);
    }

    pub fn leftovers() -> Vec<String> {
        // `route print` lists active and persistent routes: any via the old tunnel is ours.
        let out = run("route", &["print", "-4", "0.0.0.0"]).unwrap_or_default();
        let mut found = vec![];
        if out.split_whitespace().any(|word| word == LEGACY_TUNNEL) {
            found.push(format!("default route via {LEGACY_TUNNEL}"));
        }
        found
    }

    pub fn repair() {
        try_run("route", &["-p", "delete", "0.0.0.0", "mask", "0.0.0.0", LEGACY_TUNNEL]);
        try_run("route", &["delete", "0.0.0.0", "mask", "0.0.0.0", LEGACY_TUNNEL]);
        // 0.1.0/0.2.0 deleted the DHCP default route on disconnect: asking DHCP
        // again puts it back.
        if default_gateway().is_none() {
            try_run("ipconfig", &["/renew"]);
        }
    }
}

// ------------------------------------------------------------------ Linux

// Linux still uses tun2proxy's own setup (see tunnel.rs); nothing to check here.
#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    use super::*;
    pub fn default_gateway() -> Option<String> {
        None
    }
    pub fn add_host_route(_: IpAddr, _: &str) -> Result<()> {
        bail!("not used on this system")
    }
    pub fn remove_host_route(_: IpAddr) {}
    pub fn add_half(_: &str, _: &str, _: &str) -> Result<()> {
        bail!("not used on this system")
    }
    pub fn remove_half(_: &str, _: &str, _: &str) {}
    pub fn flush_dns() {}
    pub fn leftovers() -> Vec<String> {
        vec![]
    }
    pub fn repair() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_skip_the_bypass_route() {
        for ip in ["192.168.1.10", "10.0.0.5", "172.20.10.1", "127.0.0.1", "169.254.1.1", "fd00::1", "fe80::1"] {
            assert!(is_local(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["203.0.113.7", "8.8.8.8", "2001:db8::1"] {
            assert!(!is_local(ip.parse().unwrap()), "{ip}");
        }
        // The tunnel's own subnet is outside every private range a LAN uses.
        assert!(!TUN_ADDR.is_private() && !TUN_PEER.is_private());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_scutil_output() {
        let text = "<dictionary> {\n  PrimaryInterface : en0\n  Router : 172.20.10.1\n  ServerAddresses : <array> {\n    0 : 10.0.0.1\n  }\n}\n";
        let pairs = os::parse_scutil(text);
        assert!(pairs.contains(&("Router".into(), "172.20.10.1".into())));
        assert!(pairs.contains(&("0".into(), "10.0.0.1".into())));
    }

    /// Runs the read-only check against this computer: it must not fail, and
    /// on a healthy machine it should find nothing.
    #[test]
    fn leftovers_check_runs() {
        let found = leftovers();
        eprintln!("leftovers on this machine: {found:?}");
    }
}
