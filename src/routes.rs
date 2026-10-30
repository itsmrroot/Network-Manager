//! The IPv4 routing table, in a form that can be told in plain words: where
//! the internet traffic goes, which local networks are reached directly, and
//! which networks are sent into a VPN tunnel.

use std::net::{IpAddr, Ipv4Addr};

use anyhow::Result;

use crate::adapters::{Adapter, Kind};
use crate::cmd;

/// One line of the routing table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub dest: Ipv4Addr,
    pub prefix: u8,
    /// The next router; none for a network that is reached directly.
    pub gateway: Option<IpAddr>,
    /// The device (`en0`, `utun8`), or on Windows the address of the adapter.
    pub device: String,
    /// Lower wins when several routes cover the same addresses (0 when the
    /// system does not say, as on macOS).
    pub metric: u32,
}

impl Route {
    pub fn is_default(&self) -> bool {
        self.prefix == 0
    }

    /// "192.168.178.0/24".
    pub fn net(&self) -> String {
        format!("{}/{}", self.dest, self.prefix)
    }
}

/// A network, and the adapter that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Via {
    pub net: String,
    pub device: String,
    /// The adapter's name as people see it ("Wi-Fi").
    pub name: String,
    pub kind: Kind,
}

/// The routing table in plain words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Where the traffic for the internet goes (every address that no other
    /// route covers).
    pub internet: Option<Via>,
    /// Networks that are reached directly, on a local adapter.
    pub local: Vec<Via>,
    /// Networks that are sent into a VPN tunnel (besides the internet).
    pub tunnel: Vec<Via>,
}

/// The routing table of this computer.
pub fn table(adapters: &[Adapter]) -> Result<Vec<Route>> {
    #[cfg(target_os = "macos")]
    {
        let _ = adapters;
        Ok(parse_netstat(&cmd::run("netstat", &["-rn", "-f", "inet"])?))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = adapters;
        Ok(parse_ip_route(&cmd::run("ip", &["-4", "route", "show"])?))
    }
    #[cfg(windows)]
    {
        Ok(parse_route_print(&cmd::run("route", &["print", "-4"])?, adapters))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = adapters;
        anyhow::bail!("the routing table is not read on this system")
    }
}

/// A destination as netstat prints it: `default`, `10`, `192.168.31`,
/// `100.64/10`, `10.1.2.3`. Missing parts are zeros; without a length, each
/// part given is 8 bits.
fn netstat_dest(s: &str) -> Option<(Ipv4Addr, u8)> {
    if s == "default" {
        return Some((Ipv4Addr::UNSPECIFIED, 0));
    }
    let (addr, len) = match s.split_once('/') {
        Some((a, l)) => (a, Some(l.parse::<u8>().ok()?)),
        None => (s, None),
    };
    let parts: Vec<u8> = addr.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut o = [0u8; 4];
    o[..parts.len()].copy_from_slice(&parts);
    Some((Ipv4Addr::from(o), len.unwrap_or(8 * parts.len() as u8)))
}

/// `netstat -rn -f inet` (macOS).
pub fn parse_netstat(text: &str) -> Vec<Route> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let Some((dest, prefix)) = netstat_dest(f[0]) else { continue };
        let flags = f[2];
        // Interface-scoped copies (flag I) are for programs that ask for one
        // interface; the ordinary table is the one without them.
        if flags.contains('I') {
            continue;
        }
        let gateway = f[1].parse::<IpAddr>().ok();
        out.push(Route { dest, prefix, gateway, device: f[3].to_string(), metric: 0 });
    }
    out
}

/// `ip -4 route show` (Linux).
pub fn parse_ip_route(text: &str) -> Vec<Route> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = f.first() else { continue };
        let dest = if *first == "default" {
            Some((Ipv4Addr::UNSPECIFIED, 0))
        } else if let Some((a, l)) = first.split_once('/') {
            a.parse().ok().zip(l.parse().ok())
        } else {
            first.parse().ok().map(|a| (a, 32))
        };
        let Some((dest, prefix)) = dest else { continue };
        let after = |key: &str| f.iter().position(|w| *w == key).and_then(|i| f.get(i + 1)).copied();
        let Some(device) = after("dev") else { continue };
        out.push(Route {
            dest,
            prefix,
            gateway: after("via").and_then(|g| g.parse().ok()),
            device: device.to_string(),
            metric: after("metric").and_then(|m| m.parse().ok()).unwrap_or(0),
        });
    }
    out
}

/// `route print -4` (Windows): the interface is an address; it is turned into
/// the adapter that has it. The gateway of a directly reached network is a
/// translated word that can be two ("On-link", German "Auf Verbindung"), so
/// the interface and metric are read from the end of the line.
pub fn parse_route_print(text: &str, adapters: &[Adapter]) -> Vec<Route> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let n = f.len();
        let (Ok(dest), Ok(mask), Ok(iface)) =
            (f[0].parse::<Ipv4Addr>(), f[1].parse::<Ipv4Addr>(), f[n - 2].parse::<Ipv4Addr>())
        else {
            continue;
        };
        let Ok(metric) = f[n - 1].parse() else {
            continue;
        };
        let prefix = u32::from(mask).leading_ones() as u8;
        let gateway = if n == 5 { f[2].parse::<IpAddr>().ok() } else { None };
        let device = adapters
            .iter()
            .find(|a| a.ipv4.iter().any(|(ip, _)| *ip == iface))
            .map_or_else(|| iface.to_string(), |a| a.device.clone());
        out.push(Route { dest, prefix, gateway, device, metric });
    }
    out
}

fn is_special(r: &Route) -> bool {
    let o = r.dest.octets();
    r.dest.is_loopback()
        || r.dest.is_multicast()
        || r.dest.is_broadcast()
        || (o[0] == 169 && o[1] == 254)
        || (o[0] == 240)
}

/// Tells the table in plain words, using the adapters to name the devices.
pub fn summarize(routes: &[Route], adapters: &[Adapter]) -> Summary {
    // A route of Windows names its adapter by an address (see `parse_route_print`), which is
    // all there is when the adapters were not known yet as the table was read.
    let adapter = |device: &str| {
        adapters
            .iter()
            .find(|a| a.device == device || a.id == device || a.ipv4.iter().any(|(ip, _)| ip.to_string() == device))
    };
    let via = |r: &Route, net: String| {
        let a = adapter(&r.device);
        Via {
            net,
            device: r.device.clone(),
            name: a.map_or_else(|| r.device.clone(), |a| a.name.clone()),
            kind: a.map_or(Kind::Other, |a| a.kind),
        }
    };
    let mut s = Summary::default();
    // A VPN that sends the internet through itself often does it with two
    // routes, 0.0.0.0/1 and 128.0.0.0/1, that win over the default route.
    let halves: Vec<&Route> =
        routes.iter().filter(|r| r.prefix == 1 && (r.dest.octets()[0] == 0 || r.dest.octets()[0] == 128)).collect();
    if halves.len() == 2 && halves[0].device == halves[1].device {
        s.internet = Some(via(halves[0], "0.0.0.0/0".into()));
    } else if let Some(r) = routes.iter().filter(|r| r.is_default()).min_by_key(|r| r.metric) {
        s.internet = Some(via(r, "0.0.0.0/0".into()));
    }
    // More than one route can lead to a network (a VPN that offers the network this
    // computer is on, say): the one with the lowest metric is the one that is used.
    let mut best: Vec<&Route> = Vec::new();
    for r in routes {
        if r.is_default() || is_special(r) || r.prefix == 32 || (r.prefix == 1 && halves.len() == 2) {
            continue;
        }
        match best.iter_mut().find(|b| b.dest == r.dest && b.prefix == r.prefix) {
            Some(b) if r.metric < b.metric => *b = r,
            Some(_) => {}
            None => best.push(r),
        }
    }
    for r in best {
        let v = via(r, r.net());
        if v.kind == Kind::Vpn {
            if !s.tunnel.contains(&v) {
                s.tunnel.push(v);
            }
        } else if r.gateway.is_none() && !s.local.contains(&v) {
            s.local.push(v);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: &str = "Routing tables

Internet:
Destination        Gateway            Flags               Netif Expire
default            192.168.178.1      UGScg                 en0
default            link#25            UGScIg              utun8
100.64/10          link#25            UCS                 utun8
100.100.100.100/32 link#25            UCS                 utun8
127                127.0.0.1          UCS                   lo0
127.0.0.1          127.0.0.1          UH                    lo0
169.254            link#14            UCS                   en0      !
192.168.178        link#14            UCS                   en0      !
192.168.178.1/32   link#14            UCS                   en0      !
224.0.0/4          link#14            UmCS                  en0      !
255.255.255.255/32 link#14            UCS                   en0      !
";

    #[test]
    fn netstat_destinations_are_expanded() {
        assert_eq!(netstat_dest("default"), Some((Ipv4Addr::UNSPECIFIED, 0)));
        assert_eq!(netstat_dest("127"), Some(("127.0.0.0".parse().unwrap(), 8)));
        assert_eq!(netstat_dest("192.168.178"), Some(("192.168.178.0".parse().unwrap(), 24)));
        assert_eq!(netstat_dest("100.64/10"), Some(("100.64.0.0".parse().unwrap(), 10)));
        assert_eq!(netstat_dest("10.1.2.3"), Some(("10.1.2.3".parse().unwrap(), 32)));
        assert_eq!(netstat_dest("Destination"), None);
    }

    fn adapter(device: &str, name: &str, kind: Kind) -> Adapter {
        Adapter {
            id: device.into(),
            name: name.into(),
            device: device.into(),
            description: None,
            kind,
            mac: None,
            ipv4: Vec::new(),
            ipv6: Vec::new(),
            gateway: None,
            gateway_mac: None,
            dns: Vec::new(),
            dhcp: None,
            up: true,
            disabled: false,
            speed_bps: None,
            mtu: None,
            rx_bytes: None,
            tx_bytes: None,
            default: false,
            service: None,
            connection: None,
            physical: kind == Kind::WiFi,
        }
    }

    #[test]
    fn a_mac_with_a_tailscale_exit_node_in_plain_words() {
        // Ordinary routes: the internet goes through Wi-Fi; Tailscale's own
        // network goes into the tunnel; the local network is direct.
        let adapters = [adapter("en0", "Wi-Fi", Kind::WiFi), adapter("utun8", "utun8", Kind::Vpn)];
        let s = summarize(&parse_netstat(MAC), &adapters);
        assert_eq!(s.internet.as_ref().map(|v| v.device.as_str()), Some("en0"));
        assert_eq!(s.tunnel.iter().map(|v| v.net.as_str()).collect::<Vec<_>>(), ["100.64.0.0/10"]);
        assert_eq!(s.local.iter().map(|v| v.net.as_str()).collect::<Vec<_>>(), ["192.168.178.0/24"]);
    }

    #[test]
    fn a_vpn_with_two_half_routes_takes_the_internet() {
        let table = "default via 192.168.1.1 dev wlan0 proto dhcp metric 600
0.0.0.0/1 dev wg0 scope link
128.0.0.0/1 dev wg0 scope link
10.8.0.0/24 dev wg0 proto kernel scope link src 10.8.0.2
192.168.1.0/24 dev wlan0 proto kernel scope link src 192.168.1.20 metric 600
";
        let adapters = [adapter("wlan0", "Wi-Fi", Kind::WiFi), adapter("wg0", "wg0", Kind::Vpn)];
        let s = summarize(&parse_ip_route(table), &adapters);
        let internet = s.internet.expect("the internet has a route");
        assert_eq!((internet.device.as_str(), internet.kind), ("wg0", Kind::Vpn));
        assert_eq!(s.tunnel.iter().map(|v| v.net.as_str()).collect::<Vec<_>>(), ["10.8.0.0/24"]);
        assert_eq!(s.local.iter().map(|v| v.net.as_str()).collect::<Vec<_>>(), ["192.168.1.0/24"]);
    }

    #[test]
    fn windows_interfaces_become_adapters() {
        let text = "Active Routes:
Network Destination        Netmask          Gateway       Interface  Metric
          0.0.0.0          0.0.0.0     192.168.1.1    192.168.1.20     35
         10.8.0.0    255.255.255.0         On-link        10.8.0.2    281
";
        let mut wifi = adapter("wlan", "Wi-Fi", Kind::WiFi);
        wifi.ipv4 = vec![("192.168.1.20".parse().unwrap(), 24)];
        let mut tun = adapter("wintun", "WireGuard", Kind::Vpn);
        tun.ipv4 = vec![("10.8.0.2".parse().unwrap(), 24)];
        let routes = parse_route_print(text, &[wifi.clone(), tun.clone()]);
        assert_eq!(routes.len(), 2);
        let s = summarize(&routes, &[wifi, tun]);
        assert_eq!(s.internet.map(|v| v.device), Some("wlan".to_string()));
        assert_eq!(s.tunnel.len(), 1);
    }

    #[test]
    fn a_german_windows_lists_the_local_network() {
        let text = "Aktive Routen:
     Netzwerkziel    Netzwerkmaske          Gateway    Schnittstelle Metrik
          0.0.0.0          0.0.0.0     192.168.31.1   192.168.31.111     36
        127.0.0.0        255.0.0.0   Auf Verbindung         127.0.0.1    331
     192.168.31.0    255.255.255.0   Auf Verbindung    192.168.31.111    291
   192.168.31.111  255.255.255.255   Auf Verbindung    192.168.31.111    291
         10.8.0.0    255.255.255.0   Auf Verbindung          10.8.0.2    281
Ständige Routen:
  Netzwerkadresse          Netzmaske  Gatewayadresse  Metrik
          0.0.0.0          0.0.0.0     192.168.31.1  Standard
";
        let mut wifi = adapter("wlan", "Wi-Fi", Kind::WiFi);
        wifi.ipv4 = vec![("192.168.31.111".parse().unwrap(), 24)];
        let mut tun = adapter("wintun", "WireGuard", Kind::Vpn);
        tun.ipv4 = vec![("10.8.0.2".parse().unwrap(), 24)];
        let routes = parse_route_print(text, &[wifi.clone(), tun.clone()]);
        assert_eq!(routes.len(), 5);
        assert!(routes[2].gateway.is_none());
        let s = summarize(&routes, &[wifi, tun]);
        assert_eq!(s.internet.map(|v| v.device), Some("wlan".to_string()));
        assert_eq!(s.local.iter().map(|v| v.net.as_str()).collect::<Vec<_>>(), ["192.168.31.0/24"]);
        assert_eq!(s.tunnel.iter().map(|v| v.net.as_str()).collect::<Vec<_>>(), ["10.8.0.0/24"]);
    }

    #[test]
    fn a_network_that_a_vpn_offers_too_goes_through_the_vpn() {
        // Tailscale offers 192.168.31.0/24, the network this computer is on, with a lower metric.
        let text = "Aktive Routen:
     Netzwerkziel    Netzwerkmaske          Gateway    Schnittstelle Metrik
          0.0.0.0          0.0.0.0     192.168.31.1   192.168.31.111     36
     192.168.31.0    255.255.255.0   Auf Verbindung   192.168.31.111    291
     192.168.31.0    255.255.255.0  100.100.100.100     100.66.225.98      5
       10.10.10.0    255.255.255.0  100.100.100.100     100.66.225.98      5
";
        let mut wifi = adapter("Qualcomm", "WLAN", Kind::WiFi);
        wifi.ipv4 = vec![("192.168.31.111".parse().unwrap(), 24)];
        let mut ts = adapter("Tailscale Tunnel", "Tailscale", Kind::Vpn);
        ts.ipv4 = vec![("100.66.225.98".parse().unwrap(), 32)];
        let adapters = [wifi, ts];
        let s = summarize(&parse_route_print(text, &adapters), &adapters);
        assert_eq!(s.internet.map(|v| v.name), Some("WLAN".to_string()));
        assert_eq!(
            s.tunnel.iter().map(|v| (v.net.as_str(), v.name.as_str())).collect::<Vec<_>>(),
            [("192.168.31.0/24", "Tailscale"), ("10.10.10.0/24", "Tailscale")]
        );
        assert!(s.local.is_empty(), "{:?}", s.local);
    }

    #[test]
    fn adapters_that_were_not_known_when_the_table_was_read_are_named_later() {
        let text = "Aktive Routen:
     Netzwerkziel    Netzwerkmaske          Gateway    Schnittstelle Metrik
          0.0.0.0          0.0.0.0     192.168.31.1   192.168.31.111     36
     192.168.31.0    255.255.255.0   Auf Verbindung   192.168.31.111    291
";
        let routes = parse_route_print(text, &[]);
        let mut wifi = adapter("Qualcomm", "WLAN", Kind::WiFi);
        wifi.ipv4 = vec![("192.168.31.111".parse().unwrap(), 24)];
        let s = summarize(&routes, &[wifi]);
        assert_eq!(s.internet.map(|v| v.name), Some("WLAN".to_string()));
        assert_eq!(s.local.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["WLAN"]);
    }

    #[test]
    fn the_default_route_with_the_lowest_metric_wins() {
        let table = "default via 10.0.0.1 dev eth0 metric 100
default via 192.168.1.1 dev wlan0 metric 600
default dev wg0 scope link metric 50
";
        let adapters = [adapter("eth0", "Ethernet", Kind::Ethernet), adapter("wg0", "wg0", Kind::Vpn)];
        let s = summarize(&parse_ip_route(table), &adapters);
        assert_eq!(s.internet.map(|v| v.device), Some("wg0".to_string()));
    }
}
