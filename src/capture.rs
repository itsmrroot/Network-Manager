//! Packet capture: every frame on an adapter, written to a pcap file that
//! Wireshark opens, and a short description of each packet for the app.
//!
//! Capturing needs administrator rights, so the app runs `netmgr capture`
//! with them in the background; it writes the file as packets arrive and
//! stops when a stop file appears (or the app is gone).

use std::fmt::Write as _;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

/// The frames' format in the file (pcap's LINKTYPE numbers).
pub const LINK_ETHERNET: u32 = 1;
/// IPv4 packets without an Ethernet header (Windows' raw sockets).
pub const LINK_IPV4: u32 = 228;

const MAGIC: u32 = 0xa1b2_c3d4;
pub const SNAPLEN: u32 = 65535;

pub fn pcap_header(link: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(24);
    h.extend_from_slice(&MAGIC.to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&4u16.to_le_bytes());
    h.extend_from_slice(&0i32.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&SNAPLEN.to_le_bytes());
    h.extend_from_slice(&link.to_le_bytes());
    h
}

pub fn pcap_record(sec: u32, usec: u32, frame: &[u8], orig_len: usize) -> Vec<u8> {
    let mut r = Vec::with_capacity(16 + frame.len());
    r.extend_from_slice(&sec.to_le_bytes());
    r.extend_from_slice(&usec.to_le_bytes());
    r.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    r.extend_from_slice(&(orig_len as u32).to_le_bytes());
    r.extend_from_slice(frame);
    r
}

/// A packet read back from a pcap file.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Seconds since 1970.
    pub time: f64,
    pub len: u32,
    pub data: Vec<u8>,
}

/// Reads a pcap file as it grows: call [`Reader::poll`] again for more.
pub struct Reader {
    file: std::fs::File,
    buf: Vec<u8>,
    pub link: Option<u32>,
    little: bool,
    nanos: bool,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self { file: std::fs::File::open(path)?, buf: Vec::new(), link: None, little: true, nanos: false })
    }

    /// The packets written since the last call.
    pub fn poll(&mut self) -> Result<Vec<Frame>> {
        use std::io::Read;
        let mut chunk = Vec::new();
        self.file.read_to_end(&mut chunk)?;
        self.buf.extend(chunk);
        let mut p = 0;
        if self.link.is_none() {
            if self.buf.len() < 24 {
                return Ok(Vec::new());
            }
            let m = u32::from_le_bytes(self.buf[0..4].try_into()?);
            (self.little, self.nanos) = match m {
                0xa1b2_c3d4 => (true, false),
                0xa1b2_3c4d => (true, true),
                0xd4c3_b2a1 => (false, false),
                0x4d3c_b2a1 => (false, true),
                _ => bail!("not a pcap file"),
            };
            self.link = Some(self.u32_at(20) & 0x0fff_ffff);
            p = 24;
        }
        let mut out = Vec::new();
        while p + 16 <= self.buf.len() {
            let (sec, frac, cap, orig) = (self.u32_at(p), self.u32_at(p + 4), self.u32_at(p + 8), self.u32_at(p + 12));
            ensure!(cap <= 262_144, "damaged pcap file");
            let end = p + 16 + cap as usize;
            if end > self.buf.len() {
                break;
            }
            let div = if self.nanos { 1e9 } else { 1e6 };
            out.push(Frame { time: sec as f64 + frac as f64 / div, len: orig, data: self.buf[p + 16..end].to_vec() });
            p = end;
        }
        self.buf.drain(..p);
        Ok(out)
    }

    fn u32_at(&self, i: usize) -> u32 {
        let b: [u8; 4] = self.buf[i..i + 4].try_into().unwrap_or_default();
        if self.little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) }
    }
}

/// Every packet in a pcap file, and its link type.
pub fn read_file(path: &Path) -> Result<(u32, Vec<Frame>)> {
    let mut r = Reader::open(path)?;
    let frames = r.poll()?;
    Ok((r.link.context("not a pcap file")?, frames))
}

/// Writes `frames` as a pcap file.
pub fn write_file(path: &Path, link: u32, frames: &[&Frame]) -> Result<()> {
    let mut out = pcap_header(link);
    for f in frames {
        let sec = f.time.trunc() as u32;
        let usec = (f.time.fract() * 1e6) as u32;
        out.extend(pcap_record(sec, usec, &f.data, f.len as usize));
    }
    std::fs::write(path, out)?;
    Ok(())
}

// ---------------------------------------------------------------- decoding

/// What a packet is, in one line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub source: String,
    pub destination: String,
    /// "TCP", "DNS", "ARP", …
    pub protocol: String,
    pub info: String,
    /// The ports, for filtering.
    pub ports: Vec<u16>,
}

fn mac(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

pub fn decode(link: u32, data: &[u8]) -> Summary {
    let s = match link {
        LINK_ETHERNET => ethernet(data),
        LINK_IPV4 => ipv4(data, None),
        _ => None,
    };
    s.unwrap_or_else(|| Summary { protocol: "?".into(), info: format!("{} bytes", data.len()), ..Default::default() })
}

fn ethernet(b: &[u8]) -> Option<Summary> {
    let (dst, src) = (mac(b.get(0..6)?), mac(b.get(6..12)?));
    let mut kind = be16(b, 12)?;
    let mut at = 14;
    let mut vlan = None;
    while kind == 0x8100 || kind == 0x88a8 {
        vlan = Some(be16(b, at)? & 0x0fff);
        kind = be16(b, at + 2)?;
        at += 4;
    }
    let body = b.get(at..)?;
    let mut s = match kind {
        0x0800 => ipv4(body, Some((&src, &dst)))?,
        0x86dd => ipv6(body)?,
        0x0806 => arp(body)?,
        0x88cc => Summary { protocol: "LLDP".into(), info: "Switch announcement".into(), ..Default::default() },
        0x888e => Summary { protocol: "EAPOL".into(), info: "802.1X authentication".into(), ..Default::default() },
        k if k <= 1500 => {
            // 802.3 with LLC: STP and CDP.
            let llc = body;
            if llc.get(0..2) == Some(&[0x42, 0x42]) {
                Summary { protocol: "STP".into(), info: "Spanning tree".into(), ..Default::default() }
            } else if b.get(0..6) == Some(&[0x01, 0x00, 0x0c, 0xcc, 0xcc, 0xcc]) {
                Summary { protocol: "CDP".into(), info: "Cisco switch announcement".into(), ..Default::default() }
            } else {
                Summary { protocol: "LLC".into(), info: format!("{} bytes", llc.len()), ..Default::default() }
            }
        }
        k => Summary { protocol: format!("0x{k:04x}"), info: format!("{} bytes", body.len()), ..Default::default() },
    };
    if s.source.is_empty() {
        s.source = src;
        s.destination = if dst == "ff:ff:ff:ff:ff:ff" { "Broadcast".into() } else { dst };
    }
    if let Some(v) = vlan {
        s.info = format!("VLAN {v} · {}", s.info);
    }
    Some(s)
}

fn arp(b: &[u8]) -> Option<Summary> {
    let op = be16(b, 6)?;
    let sha = mac(b.get(8..14)?);
    let spa = Ipv4Addr::from(be32(b, 14)?);
    let tpa = Ipv4Addr::from(be32(b, 24)?);
    let info = match op {
        1 if spa == tpa => format!("Gratuitous ARP for {spa}"),
        1 => format!("Who has {tpa}? Tell {spa}"),
        2 => format!("{spa} is at {sha}"),
        _ => format!("ARP operation {op}"),
    };
    Some(Summary { source: spa.to_string(), destination: tpa.to_string(), protocol: "ARP".into(), info, ports: vec![] })
}

fn ipv4(b: &[u8], _macs: Option<(&str, &str)>) -> Option<Summary> {
    let ihl = (*b.first()? & 0x0f) as usize * 4;
    if *b.first()? >> 4 != 4 || ihl < 20 {
        return None;
    }
    let total = (be16(b, 2)? as usize).min(b.len()).max(ihl);
    let proto = *b.get(9)?;
    let src = Ipv4Addr::from(be32(b, 12)?).to_string();
    let dst = Ipv4Addr::from(be32(b, 16)?).to_string();
    let frag = be16(b, 6)? & 0x1fff;
    let body = b.get(ihl..total)?;
    let mut s = if frag != 0 {
        Summary { protocol: "IPv4".into(), info: "Fragment".into(), ..Default::default() }
    } else {
        transport(proto, body, false)
    };
    s.source = src;
    s.destination = dst;
    Some(s)
}

fn ipv6(b: &[u8]) -> Option<Summary> {
    if *b.first()? >> 4 != 6 {
        return None;
    }
    let len = be16(b, 4)? as usize;
    let mut next = *b.get(6)?;
    let src: [u8; 16] = b.get(8..24)?.try_into().ok()?;
    let dst: [u8; 16] = b.get(24..40)?.try_into().ok()?;
    let mut at = 40;
    // Skip extension headers: hop-by-hop, routing, destination options.
    while matches!(next, 0 | 43 | 60) {
        next = *b.get(at)?;
        at += (*b.get(at + 1)? as usize + 1) * 8;
    }
    let body = b.get(at..(40 + len).min(b.len()))?;
    let mut s = transport(next, body, true);
    s.source = Ipv6Addr::from(src).to_string();
    s.destination = Ipv6Addr::from(dst).to_string();
    Some(s)
}

/// Well-known ports: the protocol people know them by.
fn service(port: u16) -> Option<&'static str> {
    Some(match port {
        20 | 21 => "FTP",
        22 => "SSH",
        23 => "Telnet",
        25 | 587 => "SMTP",
        53 => "DNS",
        67 | 68 => "DHCP",
        69 => "TFTP",
        80 | 8080 => "HTTP",
        110 => "POP3",
        123 => "NTP",
        137..=139 => "NetBIOS",
        143 => "IMAP",
        161 | 162 => "SNMP",
        389 => "LDAP",
        443 | 8443 => "TLS",
        445 => "SMB",
        514 => "Syslog",
        546 | 547 => "DHCPv6",
        1900 => "SSDP",
        3389 => "RDP",
        5353 => "mDNS",
        5355 => "LLMNR",
        _ => return None,
    })
}

fn transport(proto: u8, b: &[u8], v6: bool) -> Summary {
    let mut s = Summary::default();
    match proto {
        6 => {
            let (Some(sp), Some(dp)) = (be16(b, 0), be16(b, 2)) else {
                s.protocol = "TCP".into();
                return s;
            };
            let flags = b.get(13).copied().unwrap_or(0);
            let names: Vec<&str> = [(0x02, "SYN"), (0x10, "ACK"), (0x01, "FIN"), (0x04, "RST"), (0x08, "PSH")]
                .iter()
                .filter(|(bit, _)| flags & bit != 0)
                .map(|(_, n)| *n)
                .collect();
            let off = (b.get(12).copied().unwrap_or(0) >> 4) as usize * 4;
            let payload = b.len().saturating_sub(off);
            s.protocol = service(sp.min(dp)).or(service(dp)).or(service(sp)).unwrap_or("TCP").into();
            s.info = format!("{sp} → {dp} [{}]", names.join(", "));
            if payload > 0 {
                let _ = write!(s.info, " {payload} bytes");
                if s.protocol == "HTTP"
                    && let Some(line) = b.get(off..).and_then(|p| p.split(|&c| c == b'\r').next())
                    && line.len() < 200
                    && line.is_ascii()
                {
                    s.info = format!("{sp} → {dp} {}", String::from_utf8_lossy(line));
                }
            }
            s.ports = vec![sp, dp];
        }
        17 => {
            let (Some(sp), Some(dp)) = (be16(b, 0), be16(b, 2)) else {
                s.protocol = "UDP".into();
                return s;
            };
            let payload = b.get(8..).unwrap_or(&[]);
            s.protocol = service(sp.min(dp)).or(service(dp)).or(service(sp)).unwrap_or("UDP").into();
            s.info = match s.protocol.as_str() {
                "DNS" | "mDNS" | "LLMNR" => dns(payload).unwrap_or_else(|| format!("{sp} → {dp}")),
                "DHCP" => dhcp(payload).unwrap_or_else(|| format!("{sp} → {dp}")),
                _ => format!("{sp} → {dp} {} bytes", payload.len()),
            };
            s.ports = vec![sp, dp];
        }
        1 | 58 => {
            let (t, c) = (b.first().copied().unwrap_or(0), b.get(1).copied().unwrap_or(0));
            s.protocol = if v6 { "ICMPv6" } else { "ICMP" }.into();
            s.info = if v6 {
                match t {
                    1 => "Destination unreachable".into(),
                    3 => "Time exceeded".into(),
                    128 => "Echo (ping) request".into(),
                    129 => "Echo (ping) reply".into(),
                    133 => "Router solicitation".into(),
                    134 => "Router advertisement".into(),
                    135 => "Neighbor solicitation".into(),
                    136 => "Neighbor advertisement".into(),
                    143 => "Multicast listener report".into(),
                    _ => format!("Type {t}"),
                }
            } else {
                match t {
                    0 => "Echo (ping) reply".into(),
                    3 => format!("Destination unreachable (code {c})"),
                    5 => "Redirect".into(),
                    8 => "Echo (ping) request".into(),
                    11 => "Time exceeded".into(),
                    _ => format!("Type {t} code {c}"),
                }
            };
        }
        2 => {
            s.protocol = "IGMP".into();
            s.info = "Multicast group membership".into();
        }
        47 => s.protocol = "GRE".into(),
        50 => s.protocol = "ESP".into(),
        89 => s.protocol = "OSPF".into(),
        112 => s.protocol = "VRRP".into(),
        p => s.protocol = format!("IP proto {p}"),
    }
    s
}

/// "Query A example.com" / "Response A example.com".
fn dns(b: &[u8]) -> Option<String> {
    let flags = be16(b, 2)?;
    let qd = be16(b, 4)?;
    if qd == 0 {
        return Some(if flags & 0x8000 != 0 { "Response" } else { "Query" }.into());
    }
    let mut at = 12;
    let mut name = Vec::new();
    loop {
        let n = *b.get(at)? as usize;
        if n == 0 || n & 0xc0 != 0 {
            at += if n == 0 { 1 } else { 2 };
            break;
        }
        name.push(String::from_utf8_lossy(b.get(at + 1..at + 1 + n)?).to_string());
        at += n + 1;
    }
    let qtype = match be16(b, at)? {
        1 => "A".to_string(),
        2 => "NS".into(),
        5 => "CNAME".into(),
        6 => "SOA".into(),
        12 => "PTR".into(),
        15 => "MX".into(),
        16 => "TXT".into(),
        28 => "AAAA".into(),
        33 => "SRV".into(),
        65 => "HTTPS".into(),
        255 => "ANY".into(),
        t => format!("type {t}"),
    };
    let kind = if flags & 0x8000 != 0 {
        match flags & 0x000f {
            0 => "Response",
            3 => "Response: no such name",
            _ => "Response: error",
        }
    } else {
        "Query"
    };
    Some(format!("{kind} {qtype} {}", name.join(".")))
}

fn dhcp(b: &[u8]) -> Option<String> {
    let yiaddr = Ipv4Addr::from(be32(b, 16)?);
    let chaddr = mac(b.get(28..34)?);
    if b.get(236..240)? != [99, 130, 83, 99] {
        return Some("BOOTP".into());
    }
    let mut at = 240;
    while let Some(&opt) = b.get(at) {
        if opt == 255 {
            break;
        }
        if opt == 0 {
            at += 1;
            continue;
        }
        let len = *b.get(at + 1)? as usize;
        if opt == 53 {
            let t = *b.get(at + 2)?;
            return Some(match t {
                1 => format!("Discover from {chaddr}"),
                2 => format!("Offer of {yiaddr}"),
                3 => format!("Request from {chaddr}"),
                4 => "Decline".into(),
                5 => format!("ACK: {yiaddr}"),
                6 => "NAK".into(),
                7 => "Release".into(),
                8 => "Inform".into(),
                _ => format!("Message {t}"),
            });
        }
        at += 2 + len;
    }
    Some("DHCP".into())
}

impl Summary {
    /// Does the packet match a display filter? Words must all match: a
    /// protocol (dns), an address, a port (443), or text in the info.
    pub fn matches(&self, filter: &str) -> bool {
        filter.split_whitespace().all(|w| {
            let w = w.to_lowercase();
            if let Ok(port) = w.parse::<u16>()
                && self.ports.contains(&port)
            {
                return true;
            }
            self.protocol.to_lowercase() == w
                || self.source.to_lowercase().contains(&w)
                || self.destination.to_lowercase().contains(&w)
                || self.info.to_lowercase().contains(&w)
        })
    }
}

// ---------------------------------------------------------------- capture

/// Captures on `interface` into `out` until `stop` exists, `parent` (a
/// process ID) is gone, `seconds` have passed or the file reaches
/// `max_bytes`. Needs administrator rights.
pub fn run(interface: &str, out: &Path, stop: &Path, parent: Option<u32>, seconds: u64, max_bytes: u64) -> Result<()> {
    ensure!(crate::cmd::is_admin(), "capturing packets needs administrator rights");
    let mut sniffer = imp::Sniffer::open(interface)?;
    let mut file = std::fs::File::create(out).with_context(|| format!("could not create {}", out.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Written as root, read by the app.
        let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o644));
    }
    file.write_all(&pcap_header(imp::LINK))?;
    file.flush()?;
    let started = Instant::now();
    let mut written = 24u64;
    let mut frames = Vec::new();
    while started.elapsed() < Duration::from_secs(seconds)
        && written < max_bytes
        && !stop.exists()
        && parent_alive(parent)
    {
        frames.clear();
        sniffer.read(&mut frames)?;
        for (t, data, orig) in &frames {
            let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
            let rec = pcap_record(d.as_secs() as u32, d.subsec_micros(), data, *orig);
            written += rec.len() as u64;
            file.write_all(&rec)?;
        }
        if !frames.is_empty() {
            file.flush()?;
        }
    }
    Ok(())
}

fn parent_alive(pid: Option<u32>) -> bool {
    let Some(pid) = pid else { return true };
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: the handle is checked and closed.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
            CloseHandle(h);
            ok
        }
    }
}

/// A frame: when it arrived, its bytes (at most [`SNAPLEN`]) and its length.
type Captured = (SystemTime, Vec<u8>, usize);

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub const LINK: u32 = LINK_ETHERNET;

    pub struct Sniffer {
        fd: i32,
        buf: Vec<u8>,
    }

    impl Sniffer {
        pub fn open(interface: &str) -> Result<Self> {
            // SAFETY: socket calls with checked results on zeroed structures.
            unsafe {
                let proto = (libc::ETH_P_ALL as u16).to_be() as i32;
                let fd = libc::socket(libc::AF_PACKET, libc::SOCK_RAW, proto);
                ensure!(fd >= 0, "could not open a packet socket: {}", std::io::Error::last_os_error());
                let s = Sniffer { fd, buf: vec![0u8; 65536] };
                let name = std::ffi::CString::new(interface)?;
                let index = libc::if_nametoindex(name.as_ptr());
                ensure!(index != 0, "no interface called {interface}");
                let mut sll: libc::sockaddr_ll = std::mem::zeroed();
                sll.sll_family = libc::AF_PACKET as u16;
                sll.sll_protocol = proto as u16;
                sll.sll_ifindex = index as i32;
                ensure!(
                    libc::bind(
                        fd,
                        &sll as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_ll>() as u32
                    ) == 0,
                    "could not listen on {interface}: {}",
                    std::io::Error::last_os_error()
                );
                let mut mr: libc::packet_mreq = std::mem::zeroed();
                mr.mr_ifindex = index as i32;
                mr.mr_type = libc::PACKET_MR_PROMISC as u16;
                libc::setsockopt(
                    fd,
                    libc::SOL_PACKET,
                    libc::PACKET_ADD_MEMBERSHIP,
                    &mr as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::packet_mreq>() as u32,
                );
                let tv = libc::timeval { tv_sec: 0, tv_usec: 250_000 };
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVTIMEO,
                    &tv as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::timeval>() as u32,
                );
                Ok(s)
            }
        }

        pub fn read(&mut self, out: &mut Vec<Captured>) -> Result<()> {
            // Waits at most the receive timeout for one frame.
            // SAFETY: the buffer is ours and its length is passed.
            let n = unsafe { libc::recv(self.fd, self.buf.as_mut_ptr().cast(), self.buf.len(), libc::MSG_TRUNC) };
            if n > 0 {
                let orig = n as usize;
                let cap = orig.min(self.buf.len());
                out.push((SystemTime::now(), self.buf[..cap].to_vec(), orig));
            }
            Ok(())
        }
    }

    impl Drop for Sniffer {
        fn drop(&mut self) {
            // SAFETY: owned descriptor.
            unsafe { libc::close(self.fd) };
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub const LINK: u32 = LINK_ETHERNET;

    pub struct Sniffer {
        fd: i32,
        buf: Vec<u8>,
    }

    impl Sniffer {
        pub fn open(interface: &str) -> Result<Self> {
            // SAFETY: ioctl calls on a BPF device we opened, with correctly
            // sized arguments.
            unsafe {
                let mut fd = -1;
                for i in 0..256 {
                    let path = std::ffi::CString::new(format!("/dev/bpf{i}"))?;
                    fd = libc::open(path.as_ptr(), libc::O_RDONLY);
                    if fd >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EBUSY) {
                        break;
                    }
                }
                ensure!(fd >= 0, "could not open a packet capture device: {}", std::io::Error::last_os_error());
                let mut len: libc::c_uint = 1 << 20;
                libc::ioctl(fd, libc::BIOCSBLEN, &mut len);
                libc::ioctl(fd, libc::BIOCGBLEN, &mut len);
                let mut s = Sniffer { fd, buf: Vec::new() };
                let mut ifr: libc::ifreq = std::mem::zeroed();
                for (i, b) in interface.bytes().take(libc::IFNAMSIZ - 1).enumerate() {
                    ifr.ifr_name[i] = b as libc::c_char;
                }
                ensure!(
                    libc::ioctl(fd, libc::BIOCSETIF, &ifr) == 0,
                    "could not listen on {interface}: {}",
                    std::io::Error::last_os_error()
                );
                let one: libc::c_uint = 1;
                libc::ioctl(fd, libc::BIOCIMMEDIATE, &one);
                // Also the packets this computer sends.
                libc::ioctl(fd, libc::BIOCSSEESENT, &one);
                libc::ioctl(fd, libc::BIOCPROMISC as libc::c_ulong, std::ptr::null_mut::<libc::c_void>());
                let tv = libc::timeval { tv_sec: 0, tv_usec: 250_000 };
                libc::ioctl(fd, libc::BIOCSRTIMEOUT, &tv);
                s.buf = vec![0u8; len.max(4096) as usize];
                Ok(s)
            }
        }

        pub fn read(&mut self, out: &mut Vec<Captured>) -> Result<()> {
            // SAFETY: the buffer is ours and its length is passed.
            let n = unsafe { libc::read(self.fd, self.buf.as_mut_ptr().cast(), self.buf.len()) };
            if n <= 0 {
                return Ok(());
            }
            let n = n as usize;
            let mut p = 0;
            // Each packet: bpf_hdr (timeval32, caplen, datalen, hdrlen), the
            // frame, padded to 4 bytes.
            while p + 18 <= n {
                let sec = u32::from_ne_bytes(self.buf[p..p + 4].try_into()?);
                let usec = u32::from_ne_bytes(self.buf[p + 4..p + 8].try_into()?);
                let caplen = u32::from_ne_bytes(self.buf[p + 8..p + 12].try_into()?) as usize;
                let datalen = u32::from_ne_bytes(self.buf[p + 12..p + 16].try_into()?) as usize;
                let hdrlen = u16::from_ne_bytes(self.buf[p + 16..p + 18].try_into()?) as usize;
                let Some(frame) = self.buf.get(p + hdrlen..p + hdrlen + caplen) else { break };
                let t = UNIX_EPOCH + Duration::new(sec as u64, usec * 1000);
                out.push((t, frame.to_vec(), datalen));
                p += (hdrlen + caplen + 3) & !3;
            }
            Ok(())
        }
    }

    impl Drop for Sniffer {
        fn drop(&mut self) {
            // SAFETY: owned descriptor.
            unsafe { libc::close(self.fd) };
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, INVALID_SOCKET, IPPROTO_IP, SIO_RCVALL, SO_RCVTIMEO, SOCK_RAW, SOCKADDR, SOCKADDR_IN, SOCKET,
        SOL_SOCKET, WSADATA, WSAIoctl, WSAStartup, bind, closesocket, recv, setsockopt, socket,
    };

    /// Windows has no Ethernet capture without a driver: raw sockets give
    /// the IPv4 packets of one adapter, sent and received.
    pub const LINK: u32 = LINK_IPV4;

    pub struct Sniffer {
        s: SOCKET,
        buf: Vec<u8>,
    }

    impl Sniffer {
        pub fn open(interface: &str) -> Result<Self> {
            let addr: Ipv4Addr = match interface.parse() {
                Ok(a) => a,
                Err(_) => crate::adapters::list(true)?
                    .into_iter()
                    .find(|a| a.name == interface || a.id == interface || a.description.as_deref() == Some(interface))
                    .and_then(|a| a.main_ipv4().map(|(ip, _)| ip))
                    .with_context(|| format!("{interface} has no IPv4 address to capture on"))?,
            };
            // SAFETY: Winsock calls with checked results; structures are
            // zeroed and filled before use.
            unsafe {
                let mut wsa: WSADATA = std::mem::zeroed();
                WSAStartup(0x0202, &mut wsa);
                let s = socket(AF_INET as i32, SOCK_RAW, IPPROTO_IP);
                ensure!(s != INVALID_SOCKET, "could not open a raw socket: {}", std::io::Error::last_os_error());
                let sn = Sniffer { s, buf: vec![0u8; 65536] };
                let mut sa: SOCKADDR_IN = std::mem::zeroed();
                sa.sin_family = AF_INET;
                sa.sin_addr.S_un.S_addr = u32::from_ne_bytes(addr.octets());
                ensure!(
                    bind(s, &sa as *const _ as *const SOCKADDR, std::mem::size_of::<SOCKADDR_IN>() as i32) == 0,
                    "could not listen on {addr}: {}",
                    std::io::Error::last_os_error()
                );
                let on: u32 = 1; // RCVALL_ON
                let mut ret = 0u32;
                ensure!(
                    WSAIoctl(
                        s,
                        SIO_RCVALL,
                        &on as *const _ as *const _,
                        4,
                        std::ptr::null_mut(),
                        0,
                        &mut ret,
                        std::ptr::null_mut(),
                        None
                    ) == 0,
                    "could not capture on {addr}: {}",
                    std::io::Error::last_os_error()
                );
                let timeout: u32 = 250;
                setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &timeout as *const _ as *const u8, 4);
                Ok(sn)
            }
        }

        pub fn read(&mut self, out: &mut Vec<Captured>) -> Result<()> {
            for _ in 0..256 {
                // SAFETY: the buffer is ours and its length is passed.
                let n = unsafe { recv(self.s, self.buf.as_mut_ptr(), self.buf.len() as i32, 0) };
                if n <= 0 {
                    break;
                }
                out.push((SystemTime::now(), self.buf[..n as usize].to_vec(), n as usize));
            }
            Ok(())
        }
    }

    impl Drop for Sniffer {
        fn drop(&mut self) {
            // SAFETY: owned socket.
            unsafe { closesocket(self.s) };
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::*;
    pub const LINK: u32 = LINK_ETHERNET;
    pub struct Sniffer;
    impl Sniffer {
        pub fn open(_: &str) -> Result<Self> {
            bail!("not supported on this system")
        }
        pub fn read(&mut self, _: &mut Vec<Captured>) -> Result<()> {
            Ok(())
        }
    }
}

/// Starts [`run`] in the background with administrator rights (the system
/// asks for the password on macOS and Linux; on Windows the app already
/// has them). Returns at once; the capture writes `out` as it goes.
pub fn start(interface: &str, out: &Path, stop: &Path, seconds: u64) -> Result<()> {
    let exe = crate::cmd::helper_exe()?;
    let pid = std::process::id();
    if cfg!(windows) {
        std::process::Command::new(&exe)
            .args(["capture", "--interface", interface, "--out"])
            .arg(out)
            .arg("--stop")
            .arg(stop)
            .args(["--parent", &pid.to_string(), "--seconds", &seconds.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("could not start the capture")?;
        return Ok(());
    }
    let q = |s: &str| crate::cmd::sh_quote(s);
    let log = out.with_extension("log");
    let script = format!(
        "{} capture --interface {} --out {} --stop {} --parent {pid} --seconds {seconds} > {} 2>&1 &",
        q(&exe.display().to_string()),
        q(interface),
        q(&out.display().to_string()),
        q(&stop.display().to_string()),
        q(&log.display().to_string()),
    );
    crate::cmd::admin_sh(&script)?;
    Ok(())
}

/// What the background capture reported when it could not start.
pub fn start_error(out: &Path) -> Option<String> {
    let log = std::fs::read_to_string(out.with_extension("log")).ok()?;
    let msg = log.trim().trim_start_matches("error: ").trim().to_string();
    (!msg.is_empty()).then_some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eth(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut f = vec![0xff; 6];
        f.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        f.extend_from_slice(&kind.to_be_bytes());
        f.extend_from_slice(body);
        f
    }

    fn ip4(proto: u8, src: [u8; 4], dst: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut h = vec![0x45, 0, 0, 0, 0, 0, 0x40, 0, 64, proto, 0, 0];
        h[2..4].copy_from_slice(&((20 + body.len()) as u16).to_be_bytes());
        h.extend_from_slice(&src);
        h.extend_from_slice(&dst);
        h.extend_from_slice(body);
        h
    }

    #[test]
    fn decodes_tcp_syn() {
        let mut tcp = vec![0xc3, 0x50, 0x01, 0xbb, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x02, 0xff, 0xff, 0, 0, 0, 0];
        tcp[12] = 0x50;
        let f = eth(0x0800, &ip4(6, [192, 168, 1, 10], [93, 184, 216, 34], &tcp));
        let s = decode(LINK_ETHERNET, &f);
        assert_eq!((s.source.as_str(), s.destination.as_str()), ("192.168.1.10", "93.184.216.34"));
        assert_eq!(s.protocol, "TLS");
        assert_eq!(s.info, "50000 → 443 [SYN]");
        assert!(s.matches("tls 443"));
        assert!(s.matches("93.184"));
        assert!(!s.matches("dns"));
    }

    #[test]
    fn decodes_dns_query() {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for part in ["example", "com"] {
            q.push(part.len() as u8);
            q.extend_from_slice(part.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 28, 0, 1]);
        let mut udp = vec![0xd4, 0x31, 0, 53, 0, 0, 0, 0];
        udp.extend(q);
        let s = decode(LINK_IPV4, &ip4(17, [10, 0, 0, 5], [1, 1, 1, 1], &udp));
        assert_eq!(s.protocol, "DNS");
        assert_eq!(s.info, "Query AAAA example.com");
    }

    #[test]
    fn decodes_arp_and_vlan() {
        let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
        arp.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 192, 168, 1, 10]);
        arp.extend_from_slice(&[0; 6]);
        arp.extend_from_slice(&[192, 168, 1, 1]);
        let mut tagged = vec![0x00, 0x14];
        tagged.extend_from_slice(&0x0806u16.to_be_bytes());
        tagged.extend(arp);
        let s = decode(LINK_ETHERNET, &eth(0x8100, &tagged));
        assert_eq!(s.protocol, "ARP");
        assert_eq!(s.info, "VLAN 20 · Who has 192.168.1.1? Tell 192.168.1.10");
        assert_eq!(s.destination, "192.168.1.1");
        assert!(decode(LINK_ETHERNET, &[1, 2, 3]).protocol == "?");
    }

    #[test]
    fn pcap_header_like_tcpdump() {
        // The header tcpdump and Wireshark read as "Ethernet".
        let want = [0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 1, 0, 0, 0];
        assert_eq!(pcap_header(LINK_ETHERNET), want);
    }

    #[test]
    fn pcap_round_trip() {
        let dir = std::env::temp_dir().join(format!("netmgr-pcap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.pcap");
        let mut data = pcap_header(LINK_ETHERNET);
        std::fs::write(&path, &data).unwrap();
        let mut r = Reader::open(&path).unwrap();
        assert!(r.poll().unwrap().is_empty());
        assert_eq!(r.link, Some(LINK_ETHERNET));
        data.extend(pcap_record(1_700_000_000, 500_000, &[1, 2, 3, 4], 60));
        let rec = pcap_record(1_700_000_001, 0, &[9; 10], 10);
        data.extend_from_slice(&rec[..12]);
        std::fs::write(&path, &data).unwrap();
        let got = r.poll().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].len, got[0].data.as_slice()), (60, &[1u8, 2, 3, 4][..]));
        assert!((got[0].time - 1_700_000_000.5).abs() < 1e-6);
        // The rest of the second record arrives later.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&rec[12..]).unwrap();
        assert_eq!(r.poll().unwrap().len(), 1);
        let (link, all) = read_file(&path).unwrap();
        assert_eq!((link, all.len()), (LINK_ETHERNET, 2));
        let out = dir.join("copy.pcap");
        write_file(&out, link, &all.iter().collect::<Vec<_>>()).unwrap();
        assert_eq!(read_file(&out).unwrap().1.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
