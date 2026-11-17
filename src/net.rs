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
//! That host route goes through the gateway of the network the device is on,
//! so it is the one thing a change of network breaks: `follow` moves it.
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
pub fn command(program: &str) -> Command {
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

/// Keeps the proxy server's own route on a gateway that exists. Another Wi-Fi
/// network, a cable, or waking from sleep replaces the gateway or takes the
/// route away with the old address; the tunnel's own packets to the proxy
/// then fall into the tunnel and nothing gets out until a reconnect. The
/// helper calls this every few seconds while a tunnel is up; it reads the
/// routing table and changes nothing while the route is as it should be.
// ponytail: polled. Both systems can say when the routes change
// (SCDynamicStore keys, NotifyRouteChange2); listen instead if three seconds
// without the tunnel after a network change prove too long.
pub fn follow(tun: &str, proxy: IpAddr) {
    // As in `apply`: a proxy on the local network has no route of its own.
    if is_local(proxy) || proxy.is_ipv6() {
        return;
    }
    let (gateways, own) = os::routes(&proxy.to_string(), tun);
    if let Some(gateway) = reroute(&gateways, own.as_deref()) {
        log::warn!("the network changed: the proxy is now reached through {gateway}");
        if let Err(error) = os::add_host_route(proxy, gateway) {
            log::warn!("{error:#}");
        }
    }
}

/// The gateway the proxy's route has to move to, if it has to: `gateways` are
/// those of the network's own default routes, best first, and `own` is the
/// one the proxy's route uses now. None while that one is still among them
/// (two networks at once: stay on the one in use), and while there is no
/// network at all.
fn reroute<'a>(gateways: &'a [String], own: Option<&str>) -> Option<&'a str> {
    match own {
        Some(current) if gateways.iter().any(|gateway| gateway == current) => None,
        _ => gateways.first().map(String::as_str),
    }
}

/// macOS, `netstat -rn -f inet`: the gateways of the default routes that are
/// not the tunnel's, and the gateway of the route to `proxy` if it has one.
/// While the tunnel is up the system's own default route is the tunnel (our
/// service is the primary one) and the network's default is a second line,
/// scoped to its interface: that one is what we are after.
#[cfg(any(target_os = "macos", test))]
fn read_netstat(table: &str, proxy: &str, tun: &str) -> (Vec<String>, Option<String>) {
    let (mut gateways, mut own) = (vec![], None);
    for line in table.lines() {
        let mut columns = line.split_whitespace();
        let (Some(destination), Some(gateway), Some(_flags), Some(interface)) = (columns.next(), columns.next(), columns.next(), columns.next()) else {
            continue;
        };
        // A gateway that is no address is a link (`link#14`), not a way out.
        if interface == tun || gateway.parse::<Ipv4Addr>().is_err() {
            continue;
        }
        match destination {
            "default" => gateways.push(gateway.to_string()),
            _ if destination == proxy => own = Some(gateway.to_string()),
            _ => {}
        }
    }
    (gateways, own)
}

/// Windows, `route print -4`: the same two answers. Active routes have five
/// columns (destination, mask, gateway, interface, metric); the persistent
/// ones under them have four and are not in force by themselves.
#[cfg(any(windows, test))]
fn read_route_print(table: &str, proxy: &str) -> (Vec<String>, Option<String>) {
    let (mut defaults, mut own) = (vec![], None);
    for line in table.lines() {
        let columns: Vec<&str> = line.split_whitespace().collect();
        let [destination, mask, gateway, interface, metric] = columns[..] else { continue };
        // "On-link" is no gateway; the tunnel's peer is no way out.
        if gateway.parse::<Ipv4Addr>().is_err() || interface.parse::<Ipv4Addr>().is_err() || gateway == TUN_PEER.to_string() {
            continue;
        }
        if (destination, mask) == ("0.0.0.0", "0.0.0.0") {
            defaults.push((metric.parse::<u32>().unwrap_or(u32::MAX), gateway.to_string()));
        } else if (destination, mask) == (proxy, "255.255.255.255") {
            own = Some(gateway.to_string());
        }
    }
    defaults.sort(); // the lowest metric is the route Windows uses
    (defaults.into_iter().map(|(_, gateway)| gateway).collect(), own)
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

    pub fn routes(proxy: &str, tun: &str) -> (Vec<String>, Option<String>) {
        read_netstat(&run("netstat", &["-rn", "-f", "inet"]).unwrap_or_default(), proxy, tun)
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

    /// The next hop of the best default route that isn't the tunnel's. The
    /// tunnel has one from the moment its adapter exists (the tun crate adds
    /// it, metric 0, on the fastest adapter there is) and it is the best one:
    /// taken as the gateway, it sends the proxy's own route into the tunnel,
    /// every connection to the proxy opens another one, and the tunnel fills
    /// up until it drops everything, DNS included.
    pub fn default_gateway() -> Option<String> {
        routes("", "").0.into_iter().next()
    }

    pub fn routes(proxy: &str, _tun: &str) -> (Vec<String>, Option<String>) {
        read_route_print(&run("route", &["print", "-4"]).unwrap_or_default(), proxy)
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
    pub fn routes(_: &str, _: &str) -> (Vec<String>, Option<String>) {
        (vec![], None)
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

    /// The routing table of a Mac with the tunnel up, as `netstat` printed it,
    /// plus the proxy's own route; and what Windows prints for the same.
    #[test]
    fn follows_the_network() {
        let netstat = |own: &str| {
            format!(
                "Routing tables\n\nInternet:\nDestination        Gateway            Flags               Netif Expire\n\
                 0/1                link#27            UScg                utun8       \n\
                 default            198.51.100.2       UGScg               utun8       \n\
                 default            192.168.1.1        UGScIg                en0       \n\
                 127                127.0.0.1          UCS                   lo0       \n\
                 128.0/1            link#27            USc                 utun8       \n\
                 192.168.1.1        4:25:e0:10:1e:20   UHLWIir               en0   1197\n\
                 198.51.100.2       198.51.100.1       UHr                 utun8       \n\
                 {own}"
            )
        };
        let gateways = |list: &[&str]| list.iter().map(|g| g.to_string()).collect::<Vec<_>>();
        // In place: only the network's own default counts, and nothing moves.
        let (found, own) = read_netstat(&netstat("203.0.113.7        192.168.1.1        UGHS                  en0       \n"), "203.0.113.7", "utun8");
        assert_eq!((found.clone(), own.as_deref()), (gateways(&["192.168.1.1"]), Some("192.168.1.1")));
        assert_eq!(reroute(&found, own.as_deref()), None);
        // Gone with the old address, as after a sleep: put back.
        let (found, own) = read_netstat(&netstat(""), "203.0.113.7", "utun8");
        assert_eq!(reroute(&found, own.as_deref()), Some("192.168.1.1"));
        // Another network: the route still names the old gateway.
        assert_eq!(reroute(&gateways(&["10.20.0.1"]), Some("192.168.1.1")), Some("10.20.0.1"));
        // Two networks: stay on the one in use. No network: wait.
        assert_eq!(reroute(&gateways(&["10.20.0.1", "192.168.1.1"]), Some("192.168.1.1")), None);
        assert_eq!(reroute(&[], Some("192.168.1.1")), None);

        let route_print = "IPv4 Route Table\n===========\nActive Routes:\n\
            Network Destination        Netmask          Gateway       Interface  Metric\n\
            \x20         0.0.0.0          0.0.0.0      192.168.1.1     192.168.1.78     35\n\
            \x20         0.0.0.0          0.0.0.0       10.20.0.1       10.20.0.15     25\n\
            \x20         0.0.0.0          0.0.0.0     198.51.100.2     198.51.100.1      5\n\
            \x20         0.0.0.0        128.0.0.0     198.51.100.2     198.51.100.1      1\n\
            \x20       127.0.0.0        255.0.0.0         On-link         127.0.0.1    331\n\
            \x20     203.0.113.7  255.255.255.255      192.168.1.1     192.168.1.78     36\n\
            ===========\nPersistent Routes:\n\
            \x20 Network Address          Netmask  Gateway Address  Metric\n\
            \x20         0.0.0.0          0.0.0.0         10.9.9.9  Default\n";
        // The tunnel's own default route has the lowest metric and is never a way out.
        let (found, own) = read_route_print(route_print, "203.0.113.7");
        assert_eq!((found, own.as_deref()), (gateways(&["10.20.0.1", "192.168.1.1"]), Some("192.168.1.1")));
        // What `apply` and `follow` see when the proxy's route points into the tunnel: no route at all.
        let looped = route_print.replace("203.0.113.7  255.255.255.255      192.168.1.1     192.168.1.78", "203.0.113.7  255.255.255.255     198.51.100.2     198.51.100.1");
        assert_eq!(read_route_print(&looped, "203.0.113.7").1, None);
    }

    /// Runs the read-only check against this computer: it must not fail, and
    /// on a healthy machine it should find nothing.
    #[test]
    fn leftovers_check_runs() {
        let found = leftovers();
        eprintln!("leftovers on this machine: {found:?}");
    }
}
