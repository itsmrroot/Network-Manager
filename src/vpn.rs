//! VPN tunnels: which one is up, whether it carries all the traffic, and a
//! check that the traffic, the DNS lookups and IPv6 really leave through it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::adapters::{Adapter, Kind};
use crate::dns::{self, RecordType};
use crate::internet::{self, Check, PublicInfo};

/// The program or protocol behind a tunnel, as far as the adapter shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    Tailscale,
    WireGuard,
    OpenVpn,
    Ipsec,
    Other,
}

impl Product {
    /// A fixed English name, for the translated texts of the app.
    pub fn label(self) -> &'static str {
        match self {
            Product::Tailscale => "Tailscale",
            Product::WireGuard => "WireGuard",
            Product::OpenVpn => "OpenVPN",
            Product::Ipsec => "IPsec / PPP",
            Product::Other => "VPN",
        }
    }
}

/// A VPN that is up.
#[derive(Debug, Clone)]
pub struct Status {
    pub tunnel: Adapter,
    pub product: Product,
    /// The tunnel carries the default route: all internet traffic goes
    /// through it (for Tailscale: an exit node is in use). Otherwise only the
    /// VPN's own networks do (a split tunnel).
    pub full: bool,
    /// The adapter of the local network (Wi-Fi, Ethernet) under the tunnel.
    pub local: Option<Adapter>,
}

/// Tailscale gives its devices addresses from 100.64.0.0/10.
fn in_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xc0) == 0x40
}

fn product_of(a: &Adapter) -> Product {
    let names = format!("{} {} {}", a.name, a.device, a.description.as_deref().unwrap_or_default()).to_lowercase();
    let device = a.device.to_lowercase();
    if names.contains("tailscale")
        || (a.ipv4.iter().any(|(ip, _)| in_cgnat(*ip)) && (device.starts_with("utun") || device.starts_with("tun")))
    {
        Product::Tailscale
    } else if names.contains("wireguard") || device.starts_with("wg") {
        Product::WireGuard
    } else if names.contains("openvpn") || names.contains("tap-windows") || names.contains("wintun") {
        Product::OpenVpn
    } else if names.contains("ipsec") || device.starts_with("ppp") {
        Product::Ipsec
    } else {
        Product::Other
    }
}

/// A tunnel that is up and has an address of its own. (macOS has several
/// `utun` adapters of its own, with nothing but a link-local IPv6 address.)
pub fn is_active_tunnel(a: &Adapter) -> bool {
    a.kind == Kind::Vpn
        && a.up
        && !a.disabled
        && a.ipv4.iter().any(|(ip, _)| !ip.is_link_local() && !ip.is_loopback() && !ip.is_unspecified())
}

/// The adapter of the local network (Wi-Fi, Ethernet) that has an address.
pub fn local_adapter(adapters: &[Adapter]) -> Option<&Adapter> {
    adapters.iter().find(|a| {
        a.physical && a.kind != Kind::Loopback && a.up && !a.disabled && a.main_ipv4().is_some() && !a.self_assigned()
    })
}

/// The VPN that is up among `adapters` (the one with the default route if
/// there are several), if any.
pub fn detect(adapters: &[Adapter]) -> Option<Status> {
    let tunnel = adapters.iter().filter(|a| is_active_tunnel(a)).max_by_key(|a| a.default)?.clone();
    let local = local_adapter(adapters).cloned();
    Some(Status { product: product_of(&tunnel), full: tunnel.default, tunnel, local })
}

/// What the three checks found, before it is turned into sentences.
#[derive(Debug, Clone, Serialize)]
pub struct Leaks {
    pub public: PublicInfo,
    /// The IPv6 address the internet sees, when IPv6 reaches it.
    pub ipv6: Option<PublicInfo>,
    pub dns: Vec<DnsProbe>,
}

/// One DNS server of this computer, and the address its answers come from.
#[derive(Debug, Clone, Serialize)]
pub struct DnsProbe {
    pub server: IpAddr,
    /// Where the server's own requests come from, as the test name reports it.
    pub seen: Option<PublicInfo>,
}

/// A name whose TXT answer is the address of the resolver that asked
/// (Google's authoritative servers answer it, whatever resolver is used).
const WHO_ASKED: &str = "o-o.myaddr.l.google.com";

/// The first IP address in the text of a TXT answer (`"203.0.113.7"`).
pub fn address_in(txt: &str) -> Option<IpAddr> {
    txt.split(|c: char| c == '"' || c.is_whitespace() || c == ',').find_map(|w| w.parse::<IpAddr>().ok())
}

/// Public resolvers: their requests come from their own network, whichever
/// way the traffic goes, so where they are seen says nothing here.
fn is_public_resolver(ip: IpAddr) -> bool {
    const MORE: [&str; 6] =
        ["8.8.4.4", "1.0.0.1", "149.112.112.112", "208.67.220.220", "2001:4860:4860::8888", "2606:4700:4700::1111"];
    dns::PUBLIC_RESOLVERS.iter().any(|(_, a)| a.parse::<IpAddr>() == Ok(ip))
        || MORE.iter().any(|a| a.parse::<IpAddr>() == Ok(ip))
}

/// Asks the traffic, IPv6 and every DNS server of this computer where they
/// leave from. Needs the internet; it takes a few seconds.
pub fn check() -> Result<Leaks> {
    let public = internet::public_info()?;
    let ipv6 = internet::public_info_v6();
    let mut dns_probes = Vec::new();
    for server in dns::system_servers().into_iter().take(3) {
        let seen = dns::query(SocketAddr::new(server, 53), WHO_ASKED, RecordType::Txt, Duration::from_secs(4))
            .ok()
            .and_then(|a| a.records.iter().find_map(|r| address_in(&r.data)))
            .map(|ip| {
                internet::info_of(&ip.to_string()).unwrap_or(PublicInfo { ip: ip.to_string(), ..Default::default() })
            });
        dns_probes.push(DnsProbe { server, seen });
    }
    if dns_probes.is_empty() {
        bail!("this computer has no DNS server to test");
    }
    Ok(Leaks { public, ipv6, dns: dns_probes })
}

/// The number of an AS ("AS8447 A1 Telekom" → "AS8447"), or the whole text.
fn network_of(info: &PublicInfo) -> Option<String> {
    let org = info.org.as_deref()?;
    Some(org.split_whitespace().next().unwrap_or(org).to_lowercase())
}

/// Whether two addresses are in the same provider's network; `None` when the
/// provider of one is not known.
fn same_network(a: &PublicInfo, b: &PublicInfo) -> Option<bool> {
    if a.ip == b.ip {
        return Some(true);
    }
    Some(network_of(a)? == network_of(b)?)
}

/// "203.0.113.7 (Vienna, Austria · A1 Telekom)".
fn describe(info: &PublicInfo) -> String {
    let more = [info.place(), info.provider()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    if more.is_empty() { info.ip.clone() } else { format!("{} ({more})", info.ip) }
}

fn check_of(
    title: &'static str,
    ok: bool,
    template: &'static str,
    value: String,
    advice: Option<&'static str>,
) -> Check {
    Check { title, ok, template, value, advice }
}

/// The sentences of a finished check. Their texts are fixed English ones, for
/// the app to translate.
pub fn findings(l: &Leaks) -> Vec<Check> {
    let mut out =
        vec![check_of("Public address", true, "Websites see your traffic coming from {v}.", describe(&l.public), None)];
    out.push(match &l.ipv6 {
        None => check_of("IPv6", true, "IPv6 does not reach the internet, so nothing goes around the VPN over IPv6.", String::new(), None),
        Some(v6) if same_network(v6, &l.public) != Some(false) => {
            check_of("IPv6", true, "IPv6 leaves from the same network: {v}", describe(v6), None)
        }
        Some(v6) => check_of(
            "IPv6",
            false,
            "IPv6 leaves from a different network: {v}",
            describe(v6),
            Some("Part of your traffic goes around the VPN over IPv6. Turn IPv6 off for your Wi-Fi or Ethernet adapter, or use a VPN that carries IPv6."),
        ),
    });
    for probe in &l.dns {
        let name = probe.server.to_string();
        out.push(match &probe.seen {
            None => check_of("DNS lookups", false, "The DNS server {v} did not answer the test.", name, None),
            Some(_) if is_public_resolver(probe.server) => check_of(
                "DNS lookups",
                true,
                "{v} is a public DNS server, reached through the VPN.",
                name,
                None,
            ),
            Some(seen) if same_network(seen, &l.public) != Some(false) => check_of(
                "DNS lookups",
                true,
                "Names are looked up through the VPN: {v}",
                format!("{name} → {}", describe(seen)),
                None,
            ),
            Some(seen) => check_of(
                "DNS lookups",
                false,
                "Names are looked up outside the VPN: {v}",
                format!("{name} → {}", describe(seen)),
                Some("Your DNS requests go to a server outside the VPN, so the network you are on can see which sites you visit. Use the VPN's own DNS server (Tailscale: turn on Use Tailscale DNS settings), or a public one such as 1.1.1.1 in Adapters → Change IP settings."),
            ),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::Mac;

    fn adapter(name: &str, device: &str, kind: Kind, ip: &str, default: bool) -> Adapter {
        Adapter {
            id: device.into(),
            name: name.into(),
            device: device.into(),
            description: None,
            kind,
            mac: None::<Mac>,
            ipv4: vec![(ip.parse().unwrap(), 24)],
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
            default,
            service: None,
            connection: None,
            physical: matches!(kind, Kind::Ethernet | Kind::WiFi),
        }
    }

    fn info(ip: &str, org: &str) -> PublicInfo {
        PublicInfo { ip: ip.into(), org: Some(org.into()), ..Default::default() }
    }

    #[test]
    fn a_tailscale_exit_node_is_a_full_tunnel() {
        let wifi = adapter("Wi-Fi", "en0", Kind::WiFi, "192.168.178.128", false);
        let tun = adapter("utun8", "utun8", Kind::Vpn, "100.92.141.34", true);
        let s = detect(&[tun, wifi]).expect("a VPN is up");
        assert_eq!(s.product, Product::Tailscale);
        assert!(s.full);
        assert_eq!(s.local.expect("the local adapter").device, "en0");
    }

    #[test]
    fn a_tunnel_that_does_not_carry_the_default_route_is_a_split_tunnel() {
        let wifi = adapter("Wi-Fi", "en0", Kind::WiFi, "192.168.1.20", true);
        let wg = adapter("wg0", "wg0", Kind::Vpn, "10.8.0.2", false);
        let s = detect(&[wifi, wg]).expect("a VPN is up");
        assert_eq!(s.product, Product::WireGuard);
        assert!(!s.full);
    }

    #[test]
    fn tunnels_without_an_address_are_no_vpn() {
        let wifi = adapter("Wi-Fi", "en0", Kind::WiFi, "192.168.1.20", true);
        // macOS's own utun adapters: only a link-local IPv4 address, or none.
        let own = adapter("utun0", "utun0", Kind::Vpn, "169.254.3.4", false);
        let mut down = adapter("wg0", "wg0", Kind::Vpn, "10.8.0.2", false);
        down.up = false;
        assert!(detect(&[wifi, own, down]).is_none());
    }

    #[test]
    fn the_address_of_a_txt_answer_is_found() {
        assert_eq!(address_in("\"203.0.113.7\""), Some("203.0.113.7".parse().unwrap()));
        assert_eq!(
            address_in("\"2001:db8::1\" \"edns0-client-subnet 1.2.3.0/24\""),
            Some("2001:db8::1".parse().unwrap())
        );
        assert_eq!(address_in("no address here"), None);
    }

    #[test]
    fn a_dns_server_outside_the_vpn_is_a_leak() {
        let public = info("198.51.100.9", "AS64500 Exit Node ISP");
        let leak = Leaks {
            public: public.clone(),
            ipv6: None,
            dns: vec![
                DnsProbe {
                    server: "192.168.178.1".parse().unwrap(),
                    seen: Some(info("203.0.113.7", "AS8447 A1 Telekom")),
                },
                DnsProbe {
                    server: "100.100.100.100".parse().unwrap(),
                    seen: Some(info("198.51.100.77", "AS64500 Exit Node ISP")),
                },
                DnsProbe { server: "1.1.1.1".parse().unwrap(), seen: Some(info("162.158.1.1", "AS13335 Cloudflare")) },
            ],
        };
        let f = findings(&leak);
        let dns: Vec<(bool, &str)> =
            f.iter().filter(|c| c.title == "DNS lookups").map(|c| (c.ok, c.value.as_str())).collect();
        assert!(!dns[0].0 && dns[0].1.starts_with("192.168.178.1"), "{dns:?}");
        assert!(dns[1].0 && dns[2].0, "{dns:?}");
        assert!(f.iter().find(|c| c.title == "IPv6").is_some_and(|c| c.ok));
    }

    #[test]
    fn ipv6_from_another_network_is_a_leak() {
        let mut l = Leaks {
            public: info("198.51.100.9", "AS64500 Exit Node ISP"),
            ipv6: Some(info("2001:db8::5", "AS8447 A1 Telekom")),
            dns: Vec::new(),
        };
        assert!(!findings(&l).iter().find(|c| c.title == "IPv6").expect("an IPv6 finding").ok);
        l.ipv6 = Some(info("2001:db8::5", "AS64500 Exit Node ISP"));
        assert!(findings(&l).iter().find(|c| c.title == "IPv6").expect("an IPv6 finding").ok);
    }
}
