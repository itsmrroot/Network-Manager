//! Finding the devices on the local network.
//!
//! Every address of the subnet is sent a small UDP packet. To deliver it, the
//! computer first asks on the network who has that address (ARP), and every
//! device answers that, even one that ignores pings. The answers end up in
//! the system's neighbour table, which is then read. A few TCP ports are
//! tried as well, to tell what kind of device each one is. No administrator
//! rights are needed.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::adapters::{self, Adapter};
use crate::cmd;
use crate::dns;
use crate::mac::Mac;

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub ip: Ipv4Addr,
    pub mac: Option<Mac>,
    pub vendor: Option<String>,
    pub name: Option<String>,
    /// TCP ports that accepted a connection.
    pub open_ports: Vec<u16>,
    pub is_self: bool,
    pub is_gateway: bool,
}

impl Device {
    /// A best guess at what the device is.
    pub fn kind(&self) -> DeviceKind {
        let name = self.name.as_deref().unwrap_or("").to_lowercase();
        let vendor = self.vendor.as_deref().unwrap_or("").to_lowercase();
        let has = |p: u16| self.open_ports.contains(&p);
        let any = |words: &[&str]| words.iter().any(|w| name.contains(w) || vendor.contains(w));
        if self.is_self {
            DeviceKind::Computer
        } else if self.is_gateway {
            DeviceKind::Router
        } else if has(9100)
            || has(631)
            || has(515)
            || any(&["printer", "epson", "canon", "brother", "lexmark", "kyocera", "xerox", "hp-"])
        {
            DeviceKind::Printer
        } else if has(62078)
            || any(&[
                "iphone",
                "ipad",
                "android",
                "galaxy",
                "pixel",
                "phone",
                "xiaomi",
                "oneplus",
                "oppo",
                "redmi",
                "huawei device",
            ])
        {
            DeviceKind::Phone
        } else if any(&["tv", "roku", "chromecast", "firetv", "fire tv", "bravia", "webos", "tizen", "shield"]) {
            DeviceKind::Tv
        } else if any(&["sonos", "echo", "speaker", "homepod", "bose", "nest-audio", "google home"]) {
            DeviceKind::Speaker
        } else if any(&["playstation", "xbox", "nintendo", "sony interactive"]) {
            DeviceKind::Console
        } else if any(&["camera", "cam", "hikvision", "dahua", "reolink", "axis", "ring"]) {
            DeviceKind::Camera
        } else if has(5000) && has(5001) || any(&["synology", "qnap", "nas", "western digital"]) {
            DeviceKind::Storage
        } else if any(&[
            "raspberry",
            "espressif",
            "tuya",
            "shelly",
            "sonoff",
            "tp-link",
            "philips",
            "signify",
            "ikea",
            "nest",
            "ecobee",
        ]) {
            DeviceKind::SmartHome
        } else if has(445)
            || has(3389)
            || has(22)
            || any(&[
                "macbook",
                "imac",
                "desktop",
                "laptop",
                "pc",
                "intel",
                "dell",
                "lenovo",
                "asus",
                "micro-star",
                "gigabyte",
            ])
        {
            DeviceKind::Computer
        } else {
            DeviceKind::Unknown
        }
    }

    /// The address was made up by the device (privacy) rather than burned in.
    pub fn private_address(&self) -> bool {
        self.mac.is_some_and(|m| m.is_local())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DeviceKind {
    Router,
    Computer,
    Phone,
    Tv,
    Printer,
    Speaker,
    Console,
    Camera,
    Storage,
    SmartHome,
    Unknown,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            DeviceKind::Router => "Router",
            DeviceKind::Computer => "Computer",
            DeviceKind::Phone => "Phone or tablet",
            DeviceKind::Tv => "TV or streamer",
            DeviceKind::Printer => "Printer",
            DeviceKind::Speaker => "Speaker",
            DeviceKind::Console => "Game console",
            DeviceKind::Camera => "Camera",
            DeviceKind::Storage => "Network storage",
            DeviceKind::SmartHome => "Smart home",
            DeviceKind::Unknown => "Device",
        }
    }
}

/// Ports tried on each device: they tell what it is and what it offers.
pub const PROBE_PORTS: &[u16] = &[22, 53, 80, 443, 445, 515, 631, 3389, 5000, 5001, 8080, 9100, 62078];

/// The well-known service on a port.
pub fn service_name(port: u16) -> &'static str {
    match port {
        20 | 21 => "FTP",
        22 => "SSH",
        23 => "Telnet",
        25 => "SMTP",
        53 => "DNS",
        67 | 68 => "DHCP",
        80 => "HTTP",
        110 => "POP3",
        123 => "NTP",
        135 => "Windows RPC",
        139 => "NetBIOS",
        143 => "IMAP",
        161 => "SNMP",
        389 => "LDAP",
        443 => "HTTPS",
        445 => "File sharing (SMB)",
        465 | 587 => "SMTP (mail submission)",
        515 => "Printer (LPD)",
        548 => "AFP (Mac file sharing)",
        631 => "Printer (IPP)",
        636 => "LDAPS",
        993 => "IMAPS",
        995 => "POP3S",
        1080 => "SOCKS proxy",
        1194 => "OpenVPN",
        1433 => "SQL Server",
        1521 => "Oracle",
        1883 => "MQTT",
        1900 => "UPnP",
        2049 => "NFS",
        3000 => "Web (dev)",
        3306 => "MySQL",
        3389 => "Remote Desktop",
        5000 | 5001 => "Web admin / NAS",
        5060 => "SIP (VoIP)",
        5432 => "PostgreSQL",
        5900 => "VNC",
        6379 => "Redis",
        8000 | 8008 | 8080 | 8888 => "Web (alternative)",
        8443 => "HTTPS (alternative)",
        9100 => "Printer (raw)",
        27017 => "MongoDB",
        51820 => "WireGuard",
        62078 => "Apple device sync",
        _ => "",
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The adapter to scan from; the default one when `None`.
    pub adapter: Option<String>,
    /// Try [`PROBE_PORTS`] on each device.
    pub ports: bool,
    /// Look up device names.
    pub names: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { adapter: None, ports: true, names: true }
    }
}

/// What a scan covers.
#[derive(Debug, Clone, Serialize)]
pub struct Range {
    pub adapter: String,
    pub local: Ipv4Addr,
    pub network: Ipv4Addr,
    pub prefix: u8,
    /// The scan is limited to the /22 around this computer on larger networks.
    pub limited: bool,
}

/// The adapter and the addresses a scan with `opts` covers.
pub fn range(opts: &Options) -> Result<(Adapter, Range, Vec<Ipv4Addr>)> {
    let adapter = match &opts.adapter {
        Some(name) => adapters::find(name)?,
        None => adapters::default_adapter().context("not connected to a network")?,
    };
    let (local, prefix) = adapter
        .ipv4
        .iter()
        .find(|(a, _)| !a.is_link_local() && !a.is_loopback())
        .copied()
        .with_context(|| format!("{} has no IPv4 address", adapter.name))?;
    ensure!(prefix < 31, "{} is on a point-to-point link: there is nothing to scan", adapter.name);
    // Large networks are scanned in the 1022 addresses around this computer.
    let scan_prefix = prefix.max(22);
    let mask = u32::MAX << (32 - scan_prefix);
    let net = u32::from(local) & mask;
    let hosts: Vec<Ipv4Addr> = (net + 1..net + !mask).map(Ipv4Addr::from).collect();
    let r = Range {
        adapter: adapter.name.clone(),
        local,
        network: Ipv4Addr::from(u32::from(local) & (u32::MAX.checked_shl(32 - prefix as u32).unwrap_or(0))),
        prefix,
        limited: scan_prefix != prefix,
    };
    Ok((adapter, r, hosts))
}

/// Scans the network. `progress` gets the fraction done and what is
/// happening; `found` gets the list so far, so that it can be shown live.
pub fn scan(
    opts: &Options,
    cancel: &AtomicBool,
    progress: &(dyn Fn(f32, &str) + Sync),
    found: &(dyn Fn(&[Device]) + Sync),
) -> Result<(Range, Vec<Device>)> {
    let (adapter, range, hosts) = range(opts)?;
    progress(0.02, "Asking every address on the network");
    // Bound to this adapter's address, so the packets leave through it.
    let sock = UdpSocket::bind(SocketAddr::new(IpAddr::V4(range.local), 0))?;
    for round in 0..2 {
        for (i, h) in hosts.iter().enumerate() {
            if *h == range.local {
                continue;
            }
            // Port 9 is "discard": nothing is expected back.
            let _ = sock.send_to(&[0], (*h, 9));
            if i % 64 == 63 {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress(0.1 + round as f32 * 0.1, "Waiting for answers");
        std::thread::sleep(Duration::from_millis(900));
    }
    let table = neighbours().unwrap_or_default();
    let in_range = |ip: &Ipv4Addr| hosts.contains(ip);
    let mut devices: Vec<Device> = table
        .into_iter()
        .filter(|(ip, mac)| in_range(ip) && !mac.is_broadcast() && !mac.is_multicast() && !mac.is_zero())
        .map(|(ip, mac)| Device {
            ip,
            vendor: mac.vendor().map(str::to_string),
            mac: Some(mac),
            name: None,
            open_ports: Vec::new(),
            is_self: false,
            is_gateway: false,
        })
        .collect();
    devices.sort_by_key(|d| d.ip);
    devices.dedup_by_key(|d| d.ip);
    // This computer is never in its own table.
    devices.push(Device {
        ip: range.local,
        mac: adapter.mac,
        vendor: adapter.mac.and_then(|m| m.vendor()).map(str::to_string),
        name: hostname(),
        open_ports: Vec::new(),
        is_self: true,
        is_gateway: false,
    });
    let gateway = match adapter.gateway {
        Some(IpAddr::V4(g)) => Some(g),
        _ => None,
    };
    for d in &mut devices {
        d.is_gateway = Some(d.ip) == gateway;
    }
    devices.sort_by_key(|d| d.ip);
    found(&devices);

    // Names and ports, many devices at a time.
    let shared = Arc::new(Mutex::new(devices));
    let total = shared.lock().map(|d| d.len()).unwrap_or(0);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let dns_servers = adapter.dns.clone();
    progress(0.3, "Finding names and services");
    std::thread::scope(|s| {
        for _ in 0..24.min(total.max(1)) {
            s.spawn(|| {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((ip, is_self, has_name)) =
                        shared.lock().ok().and_then(|d| d.get(i).map(|d| (d.ip, d.is_self, d.name.is_some())))
                    else {
                        return;
                    };
                    let name = if opts.names && !has_name { dns::device_name(ip, &dns_servers) } else { None };
                    let ports = if opts.ports && !is_self {
                        open_ports(ip, PROBE_PORTS, Duration::from_millis(400))
                    } else {
                        Vec::new()
                    };
                    if let Ok(mut list) = shared.lock() {
                        if let Some(d) = list.get_mut(i) {
                            if name.is_some() {
                                d.name = name;
                            }
                            d.open_ports = ports;
                        }
                        found(&list);
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress(0.3 + 0.7 * n as f32 / total.max(1) as f32, "Finding names and services");
                }
            });
        }
    });
    let devices = Arc::try_unwrap(shared).map(|m| m.into_inner().unwrap_or_default()).unwrap_or_default();
    Ok((range, devices))
}

/// The ports of `ip` that accept a TCP connection.
pub fn open_ports(ip: Ipv4Addr, ports: &[u16], timeout: Duration) -> Vec<u16> {
    let open = Mutex::new(Vec::new());
    let open_ref = &open;
    std::thread::scope(|s| {
        for chunk in ports.chunks(8) {
            s.spawn(move || {
                for &p in chunk {
                    if TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(ip), p), timeout).is_ok()
                        && let Ok(mut o) = open_ref.lock()
                    {
                        o.push(p);
                    }
                }
            });
        }
    });
    let mut v = open.into_inner().unwrap_or_default();
    v.sort_unstable();
    v
}

/// This computer's name.
pub fn hostname() -> Option<String> {
    if let Some(n) = std::env::var_os("COMPUTERNAME") {
        return Some(n.to_string_lossy().into_owned());
    }
    let out = cmd::run("hostname", &[]).ok()?;
    let n = out.trim().trim_end_matches(".local").to_string();
    (!n.is_empty()).then_some(n)
}

/// The system's neighbour (ARP) table: IPv4 address and MAC address.
pub fn neighbours() -> Result<Vec<(Ipv4Addr, Mac)>> {
    if cfg!(target_os = "linux")
        && let Ok(text) = std::fs::read_to_string("/proc/net/arp")
    {
        return Ok(parse_proc_arp(&text));
    }
    let args: &[&str] = if cfg!(windows) { &["-a"] } else { &["-an"] };
    Ok(parse_arp(&cmd::run("arp", args)?))
}

/// `arp -a` (Windows) and `arp -an` (macOS, BSD): any line with an IPv4
/// address and a MAC address.
pub fn parse_arp(text: &str) -> Vec<(Ipv4Addr, Mac)> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.contains("incomplete") || line.trim_start().starts_with("Interface") {
            continue;
        }
        let mut ip = None;
        let mut mac = None;
        for word in line.split_whitespace() {
            let w = word.trim_matches(|c| c == '(' || c == ')');
            if ip.is_none() {
                ip = w.parse::<Ipv4Addr>().ok();
            } else if mac.is_none() && (w.contains(':') || w.contains('-')) {
                mac = w.parse::<Mac>().ok();
            }
        }
        if let (Some(ip), Some(mac)) = (ip, mac) {
            out.push((ip, mac));
        }
    }
    out
}

/// `/proc/net/arp` (Linux); flags 0x0 are unanswered requests.
pub fn parse_proc_arp(text: &str) -> Vec<(Ipv4Addr, Mac)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 4 || f[2] == "0x0" {
                return None;
            }
            Some((f[0].parse().ok()?, f[3].parse().ok()?))
        })
        .collect()
}

/// Remembered names for devices, by MAC address.
pub type Labels = HashMap<Mac, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arp_tables() {
        let mac = "? (192.168.8.1) at 0:94:ec:3e:41:45 on en0 ifscope [ethernet]\n\
                   ? (192.168.8.9) at (incomplete) on en0 ifscope [ethernet]\n\
                   ? (224.0.0.251) at 1:0:5e:0:0:fb on en0 ifscope permanent [ethernet]\n";
        let t = parse_arp(mac);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0], ("192.168.8.1".parse().unwrap(), "00:94:EC:3E:41:45".parse().unwrap()));
        assert!(t[1].1.is_multicast());

        let win = "\nInterface: 192.168.1.5 --- 0xb\n  Internet Address      Physical Address      Type\n  \
                   192.168.1.1           00-11-22-33-44-55     dynamic\n  192.168.1.255         ff-ff-ff-ff-ff-ff     static\n";
        let t = parse_arp(win);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].1.to_string(), "00:11:22:33:44:55");

        let linux = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.168.0.1      0x1         0x2         a4:2b:b0:11:22:33     *        wlan0\n\
                     192.168.0.7      0x1         0x0         00:00:00:00:00:00     *        wlan0\n";
        let t = parse_proc_arp(linux);
        assert_eq!(t, vec![("192.168.0.1".parse().unwrap(), "a4:2b:b0:11:22:33".parse().unwrap())]);
    }

    #[test]
    fn device_kinds() {
        let mut d = Device {
            ip: "192.168.1.20".parse().unwrap(),
            mac: None,
            vendor: Some("Apple".into()),
            name: Some("Annas-iPhone".into()),
            open_ports: vec![],
            is_self: false,
            is_gateway: false,
        };
        assert_eq!(d.kind(), DeviceKind::Phone);
        d.name = None;
        d.vendor = None;
        d.open_ports = vec![9100];
        assert_eq!(d.kind(), DeviceKind::Printer);
        d.is_gateway = true;
        assert_eq!(d.kind(), DeviceKind::Router);
    }
}
