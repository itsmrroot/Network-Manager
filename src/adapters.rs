//! The computer's network adapters and their current settings.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Result, anyhow};
use netdev::interface::state::OperState;
use netdev::interface::types::InterfaceType;
use serde::{Deserialize, Serialize};

use crate::cmd;
use crate::mac::Mac;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Ethernet,
    WiFi,
    Cellular,
    Vpn,
    Bridge,
    Virtual,
    Loopback,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ethernet => "Ethernet",
            Kind::WiFi => "Wi-Fi",
            Kind::Cellular => "Cellular",
            Kind::Vpn => "VPN / tunnel",
            Kind::Bridge => "Bridge",
            Kind::Virtual => "Virtual",
            Kind::Loopback => "Loopback",
            Kind::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Adapter {
    /// The system's name: `en0`, `eth0`, `wlan0`; the adapter GUID on Windows.
    pub id: String,
    /// The name people see: "Wi-Fi", "Ethernet 2", "USB 10/100/1000 LAN".
    pub name: String,
    /// The device name (`en0`, `wlp2s0`), or on Windows the adapter model.
    pub device: String,
    pub description: Option<String>,
    pub kind: Kind,
    pub mac: Option<Mac>,
    /// Address and prefix length.
    pub ipv4: Vec<(Ipv4Addr, u8)>,
    pub ipv6: Vec<(Ipv6Addr, u8)>,
    pub gateway: Option<IpAddr>,
    pub gateway_mac: Option<Mac>,
    pub dns: Vec<IpAddr>,
    /// Whether the IPv4 address comes from DHCP, when the system says.
    pub dhcp: Option<bool>,
    /// Connected (link up).
    pub up: bool,
    /// Turned off (Windows "Disabled", macOS service off, Linux link down).
    pub disabled: bool,
    pub speed_bps: Option<u64>,
    pub mtu: Option<u32>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
    /// Carries the default route.
    pub default: bool,
    /// macOS: the network service, as `networksetup` names it.
    pub service: Option<String>,
    /// Linux: the NetworkManager connection in use.
    pub connection: Option<String>,
    /// A physical adapter (or Wi-Fi), not a tunnel, bridge or VM adapter.
    pub physical: bool,
}

impl Adapter {
    /// The first IPv4 address that is not link-local (169.254.x.x).
    pub fn main_ipv4(&self) -> Option<(Ipv4Addr, u8)> {
        self.ipv4.iter().find(|(a, _)| !a.is_link_local()).or(self.ipv4.first()).copied()
    }

    /// No DHCP server answered: Windows and macOS give themselves 169.254.x.x.
    pub fn self_assigned(&self) -> bool {
        !self.ipv4.is_empty() && self.ipv4.iter().all(|(a, _)| a.is_link_local())
    }

    pub fn status(&self) -> &'static str {
        if self.disabled {
            "Disabled"
        } else if !self.up {
            "Not connected"
        } else if self.self_assigned() {
            "No network address"
        } else {
            "Connected"
        }
    }
}

/// Every adapter, the one with the default route first. Loopback and
/// virtual adapters are left out unless `all`.
pub fn list(all: bool) -> Result<Vec<Adapter>> {
    let interfaces = netdev::get_interfaces();
    let extra = Extra::load();
    let mut out: Vec<Adapter> = interfaces.into_iter().map(|i| from_netdev(i, &extra)).collect();
    out.retain(|a| all || (a.physical && a.kind != Kind::Loopback));
    #[cfg(windows)]
    windows_disabled(&mut out, all);
    out.sort_by_key(|a| (!a.default, !a.up, a.kind != Kind::Ethernet && a.kind != Kind::WiFi, a.name.to_lowercase()));
    Ok(out)
}

/// The adapter called `name` (its name, device or id).
pub fn find(name: &str) -> Result<Adapter> {
    let all = list(true)?;
    let lower = name.to_lowercase();
    all.iter()
        .find(|a| a.id == name || a.device == name || a.name == name)
        .or_else(|| all.iter().find(|a| a.name.to_lowercase() == lower || a.device.to_lowercase() == lower))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<_> = all.iter().filter(|a| a.physical).map(|a| a.name.clone()).collect();
            anyhow!("no adapter called \"{name}\" (adapters: {})", names.join(", "))
        })
}

/// The adapter with the default route, if any.
pub fn default_adapter() -> Option<Adapter> {
    list(false).ok()?.into_iter().find(|a| a.default)
}

/// macOS: whether the app is denied Local Network access, which hides MAC
/// addresses and the devices on the network (System Settings → Privacy &
/// Security → Local Network).
pub fn local_network_denied() -> bool {
    cfg!(target_os = "macos")
        && netdev::get_default_interface()
            .ok()
            .and_then(|i| i.mac_addr)
            .is_some_and(|m| Mac(m.octets()).is_placeholder())
}

/// What the system's own tools add to netdev's view.
#[derive(Default)]
struct Extra {
    /// macOS: (device, hardware port, service, enabled).
    services: Vec<(String, String, String, bool)>,
    /// Linux: (device, type, state, connection).
    nm_devices: Vec<(String, String, String, String)>,
}

impl Extra {
    fn load() -> Self {
        let mut e = Extra::default();
        if cfg!(target_os = "macos")
            && let Ok(text) = cmd::run("networksetup", &["-listnetworkserviceorder"])
        {
            e.services = parse_service_order(&text);
        }
        if cfg!(target_os = "linux")
            && let Ok(text) = cmd::run("nmcli", &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"])
        {
            e.nm_devices = text
                .lines()
                .filter_map(|l| {
                    let f = split_terse(l);
                    (f.len() >= 4).then(|| (f[0].clone(), f[1].clone(), f[2].clone(), f[3].clone()))
                })
                .collect();
        }
        e
    }
}

/// `networksetup -listnetworkserviceorder`:
///
/// ```text
/// (1) USB 10/100/1000 LAN
/// (Hardware Port: USB 10/100/1000 LAN, Device: en7)
/// (*) Thunderbolt Bridge
/// (Hardware Port: Thunderbolt Bridge, Device: bridge0)
/// ```
pub fn parse_service_order(text: &str) -> Vec<(String, String, String, bool)> {
    let mut out = Vec::new();
    let mut service: Option<(String, bool)> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("(Hardware Port: ") {
            let rest = rest.trim_end_matches(')');
            if let (Some((port, device)), Some((name, enabled))) = (rest.split_once(", Device: "), service.take())
                && !device.is_empty()
            {
                out.push((device.to_string(), port.to_string(), name, enabled));
            }
        } else if let Some(rest) = line.strip_prefix("(*) ") {
            service = Some((rest.to_string(), false));
        } else if line.starts_with('(')
            && let Some((num, name)) = line[1..].split_once(") ")
            && num.chars().all(|c| c.is_ascii_digit())
        {
            service = Some((name.to_string(), true));
        }
    }
    out
}

/// Splits a line of `nmcli -t` output: fields are separated by `:`, and
/// `:` and `\` inside a field are escaped with `\`.
pub fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    fields.last_mut().unwrap().push(n);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

fn from_netdev(i: netdev::Interface, extra: &Extra) -> Adapter {
    let mut kind = match i.if_type {
        InterfaceType::Wireless80211 => Kind::WiFi,
        InterfaceType::Ethernet
        | InterfaceType::FastEthernetT
        | InterfaceType::FastEthernetFx
        | InterfaceType::GigabitEthernet
        | InterfaceType::Ethernet3Megabit => Kind::Ethernet,
        InterfaceType::Loopback => Kind::Loopback,
        InterfaceType::Tunnel | InterfaceType::Ppp => Kind::Vpn,
        InterfaceType::Bridge => Kind::Bridge,
        InterfaceType::Wwan | InterfaceType::Wwanpp | InterfaceType::Wwanpp2 => Kind::Cellular,
        InterfaceType::ProprietaryVirtual => Kind::Virtual,
        _ => Kind::Other,
    };
    if i.is_loopback() {
        kind = Kind::Loopback;
    }
    if i.is_tun() || ["utun", "tun", "tap", "wg", "ipsec", "ppp", "gif", "stf"].iter().any(|p| i.name.starts_with(p)) {
        kind = Kind::Vpn;
    }
    let mut name = i.friendly_name.clone().unwrap_or_else(|| i.name.clone());
    let device = if cfg!(windows) { i.description.clone().unwrap_or_default() } else { i.name.clone() };
    let mut service = None;
    let mut connection = None;
    let mut disabled = false;
    let mut physical = !matches!(kind, Kind::Loopback | Kind::Vpn | Kind::Bridge | Kind::Virtual);

    if let Some((_, port, svc, enabled)) = extra.services.iter().find(|(d, ..)| *d == i.name) {
        name = svc.clone();
        service = Some(svc.clone());
        disabled = !enabled;
        if port == "Wi-Fi" || port == "AirPort" {
            kind = Kind::WiFi;
        } else if kind == Kind::Other && (port.contains("Ethernet") || port.contains("LAN")) {
            kind = Kind::Ethernet;
        }
        if port.contains("Bridge") {
            kind = Kind::Bridge;
            physical = false;
        }
    } else if cfg!(target_os = "macos") {
        // Not a network service: awdl, llw, anpi, ap1 and the like.
        physical = false;
    }

    if cfg!(target_os = "linux") {
        let sys = std::path::Path::new("/sys/class/net").join(&i.name);
        if sys.join("wireless").exists() || sys.join("phy80211").exists() {
            kind = Kind::WiFi;
        }
        if !sys.join("device").exists() {
            // docker0, veth*, virbr0, br-…: no hardware behind it.
            physical = false;
            if kind == Kind::Ethernet || kind == Kind::Other {
                kind =
                    if i.name.starts_with("br") || i.name.starts_with("virbr") { Kind::Bridge } else { Kind::Virtual };
            }
        }
        if let Some((_, ty, state, conn)) = extra.nm_devices.iter().find(|(d, ..)| *d == i.name) {
            if ty == "wifi" {
                kind = Kind::WiFi;
            }
            if !conn.is_empty() && conn != "--" {
                connection = Some(conn.clone());
            }
            disabled = state == "unavailable" || state == "unmanaged" && !i.is_up();
        }
    }

    if cfg!(windows) {
        let d = i.description.clone().unwrap_or_default().to_lowercase();
        if [
            "virtual",
            "hyper-v",
            "vmware",
            "virtualbox",
            "tap-",
            "wan miniport",
            "bluetooth",
            "loopback",
            "pseudo",
            "teredo",
            "isatap",
            "wintun",
            "wireguard",
            "vpn",
        ]
        .iter()
        .any(|w| d.contains(w))
        {
            physical = false;
            if kind == Kind::Ethernet || kind == Kind::Other {
                kind = Kind::Virtual;
            }
        }
    }

    let up = i.oper_state == OperState::Up || (i.oper_state == OperState::Unknown && i.is_running());
    let gateway = i
        .gateway
        .as_ref()
        .and_then(|g| g.ipv4.first().copied().map(IpAddr::V4).or_else(|| g.ipv6.first().copied().map(IpAddr::V6)));
    let gateway_mac = i.gateway.as_ref().map(|g| Mac(g.mac_addr.octets())).filter(|m| !m.is_zero());
    Adapter {
        id: i.name.clone(),
        name,
        device,
        description: i.description.clone(),
        kind,
        // macOS hides the address (02:00:00:00:00:00) from apps that have no
        // Local Network access.
        mac: i.mac_addr.map(|m| Mac(m.octets())).filter(|m| !m.is_zero() && !m.is_placeholder()),
        ipv4: i.ipv4.iter().map(|n| (n.addr(), n.prefix_len())).collect(),
        ipv6: i.ipv6.iter().map(|n| (n.addr(), n.prefix_len())).collect(),
        gateway,
        gateway_mac,
        dns: i.dns_servers.clone(),
        dhcp: i.dhcp_v4_enabled,
        up,
        disabled,
        speed_bps: i.transmit_speed.or(i.receive_speed).filter(|s| *s > 0),
        mtu: i.mtu,
        rx_bytes: i.stats.as_ref().map(|s| s.rx_bytes),
        tx_bytes: i.stats.as_ref().map(|s| s.tx_bytes),
        default: i.default,
        service,
        connection,
        physical,
    }
}

/// Windows leaves disabled adapters out of the list netdev reads: add them
/// from PowerShell so that they can be turned on again.
#[cfg(windows)]
fn windows_disabled(out: &mut Vec<Adapter>, all: bool) {
    let script = "Get-NetAdapter | Where-Object { $_.Status -eq 'Disabled' } | \
                  ForEach-Object { \"$($_.Name)`t$($_.InterfaceDescription)`t$($_.MacAddress)`t$($_.InterfaceGuid)`t$($_.MediaType)\" }";
    let Ok(text) = cmd::powershell(script) else { return };
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 || out.iter().any(|a| a.name == f[0]) {
            continue;
        }
        let desc = f[1].to_lowercase();
        let virtual_ =
            ["virtual", "hyper-v", "vmware", "virtualbox", "tap-", "bluetooth", "wintun", "wireguard", "vpn"]
                .iter()
                .any(|w| desc.contains(w));
        if virtual_ && !all {
            continue;
        }
        let kind = if f[4].contains("802.11") || desc.contains("wi-fi") || desc.contains("wireless") {
            Kind::WiFi
        } else {
            Kind::Ethernet
        };
        out.push(Adapter {
            id: f[3].to_string(),
            name: f[0].to_string(),
            device: f[1].to_string(),
            description: Some(f[1].to_string()),
            kind,
            mac: f[2].parse().ok(),
            ipv4: Vec::new(),
            ipv6: Vec::new(),
            gateway: None,
            gateway_mac: None,
            dns: Vec::new(),
            dhcp: None,
            up: false,
            disabled: true,
            speed_bps: None,
            mtu: None,
            rx_bytes: None,
            tx_bytes: None,
            default: false,
            service: None,
            connection: None,
            physical: !virtual_,
        });
    }
}

/// Prefix length of a dotted subnet mask (`255.255.255.0` → 24).
pub fn mask_to_prefix(mask: Ipv4Addr) -> Option<u8> {
    let bits = u32::from(mask);
    let prefix = bits.leading_ones();
    (bits.checked_shl(prefix).unwrap_or(0) == 0).then_some(prefix as u8)
}

/// The subnet mask of a prefix length (24 → `255.255.255.0`).
pub fn prefix_to_mask(prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from(if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32) as u32) })
}

/// Bytes as "1.2 GB".
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// Bits per second as "866 Mbit/s".
pub fn format_speed(bps: u64) -> String {
    let v = bps as f64;
    if v >= 1e9 {
        format!("{:.1} Gbit/s", v / 1e9).replace(".0 ", " ")
    } else if v >= 1e6 {
        format!("{:.0} Mbit/s", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.0} kbit/s", v / 1e3)
    } else {
        format!("{bps} bit/s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        assert_eq!(mask_to_prefix("255.255.255.0".parse().unwrap()), Some(24));
        assert_eq!(mask_to_prefix("255.255.252.0".parse().unwrap()), Some(22));
        assert_eq!(mask_to_prefix("0.0.0.0".parse().unwrap()), Some(0));
        assert_eq!(mask_to_prefix("255.255.255.255".parse().unwrap()), Some(32));
        assert_eq!(mask_to_prefix("255.0.255.0".parse().unwrap()), None);
        assert_eq!(prefix_to_mask(24), "255.255.255.0".parse::<Ipv4Addr>().unwrap());
        assert_eq!(prefix_to_mask(0), Ipv4Addr::UNSPECIFIED);
        assert_eq!(prefix_to_mask(32), Ipv4Addr::BROADCAST);
    }

    #[test]
    fn mac_services() {
        let text = "An asterisk (*) denotes that a network service is disabled.\n\
                    (1) USB 10/100/1000 LAN\n(Hardware Port: USB 10/100/1000 LAN, Device: en7)\n\n\
                    (*) Thunderbolt Bridge\n(Hardware Port: Thunderbolt Bridge, Device: bridge0)\n\n\
                    (3) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n\n\
                    (4) My VPN\n(Hardware Port: com.wireguard.macos, Device: )\n";
        let s = parse_service_order(text);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], ("en7".into(), "USB 10/100/1000 LAN".into(), "USB 10/100/1000 LAN".into(), true));
        assert!(!s[1].3);
        assert_eq!(s[2], ("en0".into(), "Wi-Fi".into(), "Wi-Fi".into(), true));
    }

    #[test]
    fn nmcli_terse_fields() {
        assert_eq!(split_terse(r"wlan0:wifi:connected:Home\:5G"), vec!["wlan0", "wifi", "connected", "Home:5G"]);
        assert_eq!(split_terse(r"a\\b:"), vec![r"a\b", ""]);
    }

    #[test]
    fn units() {
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_234_567), "1.2 MB");
        assert_eq!(format_speed(866_000_000), "866 Mbit/s");
        assert_eq!(format_speed(1_000_000_000), "1 Gbit/s");
        assert_eq!(format_speed(2_500_000_000), "2.5 Gbit/s");
    }
}
