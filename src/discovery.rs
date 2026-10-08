//! "Which switch port am I plugged into?" — LLDP and CDP.
//!
//! Switches announce themselves on every port: LLDP (all makers) about
//! every 30 seconds, CDP (Cisco) every 60. The announcement names the
//! switch, the port, its VLAN, the voice VLAN, the switch's management
//! address and software. Nothing is sent: the app only listens.
//!
//! Listening to raw Ethernet frames needs administrator rights:
//! * Linux: an `AF_PACKET` socket;
//! * macOS: a BPF device (`/dev/bpf*`);
//! * Windows: the built-in packet monitor `pktmon`, whose capture file is
//!   converted to pcapng and read here.
//!
//! On macOS and Linux the app runs `netmgr switch-port` with administrator
//! rights for this and reads its answer.

use std::time::Duration;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use anyhow::bail;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::mac::Mac;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Neighbor {
    /// "LLDP" or "CDP".
    pub protocol: String,
    /// The switch's name.
    pub system_name: Option<String>,
    pub chassis_id: Option<String>,
    /// The switch port this computer is plugged into.
    pub port_id: Option<String>,
    pub port_description: Option<String>,
    /// LLDP system description, or CDP software version.
    pub description: Option<String>,
    /// CDP platform ("cisco WS-C2960X-48FPD-L").
    pub platform: Option<String>,
    pub management: Vec<String>,
    /// The untagged (native / port) VLAN.
    pub vlan: Option<u16>,
    pub voice_vlan: Option<u16>,
    /// Named VLANs of the port (LLDP 802.1).
    pub vlan_names: Vec<(u16, String)>,
    pub capabilities: Vec<String>,
    pub duplex: Option<String>,
    pub vtp_domain: Option<String>,
    /// Power over Ethernet the port offers, as the switch describes it.
    pub poe: Option<String>,
    /// Speed and duplex the port negotiated (LLDP 802.3 MAU type).
    pub link: Option<String>,
    pub ttl: Option<u16>,
    /// The switch port's own MAC address.
    pub source: Option<Mac>,
}

impl Neighbor {
    /// The port in a short form people know, e.g. "Gi1/0/12".
    pub fn port(&self) -> Option<&str> {
        self.port_id.as_deref().or(self.port_description.as_deref())
    }
}

const LLDP_MAC: [u8; 6] = [0x01, 0x80, 0xc2, 0x00, 0x00, 0x0e];
const CDP_MAC: [u8; 6] = [0x01, 0x00, 0x0c, 0xcc, 0xcc, 0xcc];

/// Reads an Ethernet frame; `None` when it is not LLDP or CDP.
pub fn parse_frame(frame: &[u8]) -> Option<Neighbor> {
    if frame.len() < 14 {
        return None;
    }
    let src = Mac(frame[6..12].try_into().ok()?);
    let mut pos = 12;
    let mut ethertype = u16::from_be_bytes([frame[pos], frame[pos + 1]]);
    // 802.1Q tags.
    while ethertype == 0x8100 || ethertype == 0x88a8 {
        pos += 4;
        ethertype = u16::from_be_bytes([*frame.get(pos)?, *frame.get(pos + 1)?]);
    }
    let body = frame.get(pos + 2..)?;
    let mut n = if ethertype == 0x88cc {
        parse_lldp(body)?
    } else if frame[..6] == CDP_MAC && ethertype < 0x0600 {
        // 802.3 length, then LLC/SNAP: AA AA 03, OUI 00 00 0C, PID 2000.
        if body.len() < 8 || body[..6] != [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x0c] || body[6..8] != [0x20, 0x00] {
            return None;
        }
        parse_cdp(&body[8..])?
    } else {
        return None;
    };
    n.source = Some(src);
    Some(n)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
}

fn address(afi: u8, b: &[u8]) -> Option<String> {
    match (afi, b.len()) {
        (1, 4) => Some(std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string()),
        (2, 16) => Some(std::net::Ipv6Addr::from(<[u8; 16]>::try_from(b).ok()?).to_string()),
        (6, 6) => Some(Mac(b.try_into().ok()?).to_string()),
        _ => None,
    }
}

/// LLDP capability bits.
fn capabilities(bits: u16) -> Vec<String> {
    const NAMES: [&str; 11] = [
        "Other",
        "Repeater",
        "Bridge",
        "Access point",
        "Router",
        "Telephone",
        "DOCSIS",
        "Station",
        "C-VLAN",
        "S-VLAN",
        "TPMR",
    ];
    NAMES.iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, n)| n.to_string()).collect()
}

/// IEEE 802.3 MAU types seen on switch ports (RFC 4836).
fn mau(t: u16) -> Option<&'static str> {
    Some(match t {
        10 => "10 Mbit/s half duplex",
        11 => "10 Mbit/s full duplex",
        15 => "100 Mbit/s half duplex",
        16 => "100 Mbit/s full duplex",
        29 => "1 Gbit/s half duplex",
        30 => "1 Gbit/s full duplex",
        54 => "10 Gbit/s full duplex",
        110 => "2.5 Gbit/s full duplex",
        111 => "5 Gbit/s full duplex",
        _ => return None,
    })
}

pub fn parse_lldp(mut b: &[u8]) -> Option<Neighbor> {
    let mut n = Neighbor { protocol: "LLDP".into(), ..Default::default() };
    while b.len() >= 2 {
        let head = u16::from_be_bytes([b[0], b[1]]);
        let (t, len) = ((head >> 9) as u8, (head & 0x1ff) as usize);
        let v = b.get(2..2 + len)?;
        b = &b[2 + len..];
        match t {
            0 => break,
            1 if !v.is_empty() => {
                n.chassis_id = Some(match v[0] {
                    4 if v.len() == 7 => Mac(v[1..7].try_into().ok()?).to_string(),
                    5 if v.len() > 2 => address(v[1], &v[2..]).unwrap_or_else(|| text(&v[2..])),
                    _ => text(&v[1..]),
                })
            }
            2 if !v.is_empty() => {
                n.port_id = Some(match v[0] {
                    3 if v.len() == 7 => Mac(v[1..7].try_into().ok()?).to_string(),
                    4 if v.len() > 2 => address(v[1], &v[2..]).unwrap_or_else(|| text(&v[2..])),
                    _ => text(&v[1..]),
                })
            }
            3 if v.len() >= 2 => n.ttl = Some(u16::from_be_bytes([v[0], v[1]])),
            4 => n.port_description = Some(text(v)).filter(|s| !s.is_empty()),
            5 => n.system_name = Some(text(v)).filter(|s| !s.is_empty()),
            6 => n.description = Some(text(v)).filter(|s| !s.is_empty()),
            7 if v.len() >= 4 => n.capabilities = capabilities(u16::from_be_bytes([v[2], v[3]])),
            8 if v.len() >= 2 => {
                let alen = v[0] as usize;
                if let Some(a) = v.get(2..1 + alen).and_then(|a| address(v[1], a)) {
                    n.management.push(a);
                }
            }
            127 if v.len() >= 4 => {
                let (oui, sub, d) = (&v[..3], v[3], &v[4..]);
                match (oui, sub) {
                    ([0x00, 0x80, 0xc2], 1) if d.len() >= 2 => {
                        n.vlan = Some(u16::from_be_bytes([d[0], d[1]])).filter(|v| *v != 0)
                    }
                    ([0x00, 0x80, 0xc2], 3) if d.len() >= 3 => {
                        let id = u16::from_be_bytes([d[0], d[1]]);
                        let l = d[2] as usize;
                        if let Some(name) = d.get(3..3 + l) {
                            n.vlan_names.push((id, text(name)));
                        }
                    }
                    ([0x00, 0x12, 0x0f], 1) if d.len() >= 5 => {
                        n.link = mau(u16::from_be_bytes([d[3], d[4]])).map(str::to_string)
                    }
                    ([0x00, 0x12, 0x0f], 2) if d.len() >= 3 => {
                        let class = d[2].saturating_sub(1);
                        n.poe = Some(if d.len() >= 12 {
                            // 802.3at: power allocated in 0.1 W.
                            format!("Class {class}, {:.1} W", u16::from_be_bytes([d[10], d[11]]) as f64 / 10.0)
                        } else {
                            format!("Class {class}")
                        });
                    }
                    // LLDP-MED network policy: application 1 is voice.
                    ([0x00, 0x12, 0xbb], 2) if d.len() >= 4 && d[0] == 1 => {
                        // 24 bits: U, T, X flags, a 12-bit VLAN ID, priority, DSCP.
                        let raw = u32::from_be_bytes([0, d[1], d[2], d[3]]);
                        let vlan = ((raw >> 9) & 0x0fff) as u16;
                        if vlan != 0 {
                            n.voice_vlan = Some(vlan);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    (n.chassis_id.is_some() || n.port_id.is_some()).then_some(n)
}

fn cdp_addresses(v: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(count) = v.get(..4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])) else { return out };
    let mut p = 4;
    for _ in 0..count.min(16) {
        let Some(&plen) = v.get(p + 1) else { break };
        let plen = plen as usize;
        let proto = v.get(p + 2..p + 2 + plen).unwrap_or_default();
        let q = p + 2 + plen;
        let Some(alen) = v.get(q..q + 2).map(|a| u16::from_be_bytes([a[0], a[1]]) as usize) else { break };
        let a = v.get(q + 2..q + 2 + alen).unwrap_or_default();
        if proto == [0xcc]
            && let Some(s) = address(1, a)
        {
            out.push(s);
        } else if alen == 16
            && let Some(s) = address(2, a)
        {
            out.push(s);
        }
        p = q + 2 + alen;
    }
    out
}

pub fn parse_cdp(b: &[u8]) -> Option<Neighbor> {
    ensure_opt(b.len() >= 4)?;
    let mut n = Neighbor { protocol: "CDP".into(), ttl: Some(b[1] as u16), ..Default::default() };
    let mut p = 4;
    while p + 4 <= b.len() {
        let t = u16::from_be_bytes([b[p], b[p + 1]]);
        let len = u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize;
        if len < 4 {
            break;
        }
        let v = b.get(p + 4..p + len)?;
        p += len;
        match t {
            0x0001 => n.system_name = Some(text(v)),
            0x0002 => n.management.extend(cdp_addresses(v)),
            0x0003 => n.port_id = Some(text(v)),
            0x0004 if v.len() >= 4 => {
                let bits = u32::from_be_bytes([v[0], v[1], v[2], v[3]]);
                const NAMES: [&str; 8] =
                    ["Router", "Bridge", "Source-route bridge", "Switch", "Host", "IGMP", "Repeater", "Phone"];
                n.capabilities = NAMES
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| bits & (1 << i) != 0)
                    .map(|(_, s)| s.to_string())
                    .collect();
            }
            0x0005 => n.description = Some(text(v)),
            0x0006 => n.platform = Some(text(v)),
            0x0009 => n.vtp_domain = Some(text(v)).filter(|s| !s.is_empty()),
            0x000a if v.len() >= 2 => n.vlan = Some(u16::from_be_bytes([v[0], v[1]])),
            0x000b if !v.is_empty() => n.duplex = Some(if v[0] == 1 { "Full" } else { "Half" }.into()),
            // Appliance (voice) VLAN: type, then the VLAN.
            0x000e if v.len() >= 3 => n.voice_vlan = Some(u16::from_be_bytes([v[1], v[2]])),
            0x0010 if v.len() >= 2 => {
                n.poe = Some(format!("{:.1} W", u16::from_be_bytes([v[0], v[1]]) as f64 / 1000.0))
            }
            0x0016 => {
                for a in cdp_addresses(v) {
                    if !n.management.contains(&a) {
                        n.management.push(a);
                    }
                }
            }
            _ => {}
        }
    }
    n.chassis_id = n.system_name.clone();
    Some(n)
}

fn ensure_opt(ok: bool) -> Option<()> {
    ok.then_some(())
}

/// Ethernet frames in a pcapng file (as `pktmon etl2pcap` writes them).
pub fn pcapng_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut p = 0;
    let mut little = true;
    let u32_at = |d: &[u8], i: usize, le: bool| -> Option<u32> {
        let b: [u8; 4] = d.get(i..i + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    while p + 12 <= data.len() {
        let Some(kind) = u32_at(data, p, little) else { break };
        if kind == 0x0a0d0d0a {
            little = data.get(p + 8..p + 12) == Some(&[0x4d, 0x3c, 0x2b, 0x1a]);
        }
        let Some(len) = u32_at(data, p, little).and(u32_at(data, p + 4, little)) else { break };
        let len = len as usize;
        if len < 12 || p + len > data.len() {
            break;
        }
        if kind == 6
            && let Some(cap) = u32_at(data, p + 20, little)
            && let Some(frame) = data.get(p + 28..p + 28 + cap as usize)
        {
            out.push(frame.to_vec());
        }
        p += len;
    }
    out
}

/// Listens on `interface` (its device name) for up to `seconds`. Returns
/// shortly after the first announcements arrive. Needs administrator rights.
pub fn capture(interface: &str, seconds: u64) -> Result<Vec<Neighbor>> {
    ensure!(crate::cmd::is_admin(), "listening for switches needs administrator rights");
    let frames = imp::capture(interface, Duration::from_secs(seconds))?;
    let mut out: Vec<Neighbor> = Vec::new();
    for f in frames {
        if let Some(n) = parse_frame(&f)
            && !out.iter().any(|o| o.protocol == n.protocol && o.chassis_id == n.chassis_id && o.port_id == n.port_id)
        {
            out.push(n);
        }
    }
    Ok(out)
}

/// [`capture`], asking for administrator rights first when needed (macOS
/// and Linux run `netmgr switch-port` through the password dialog).
pub fn listen(interface: &str, seconds: u64) -> Result<Vec<Neighbor>> {
    if crate::cmd::is_admin() || cfg!(windows) {
        return capture(interface, seconds);
    }
    let exe = crate::cmd::helper_exe()?;
    let script = format!(
        "{} switch-port --interface {} --seconds {seconds} --json",
        crate::cmd::sh_quote(&exe.display().to_string()),
        crate::cmd::sh_quote(interface)
    );
    let out = crate::cmd::admin_sh(&script)?;
    serde_json::from_str(out.trim()).context("unexpected answer from the listener")
}

/// Is `frame` one we wait for?
fn wanted(frame: &[u8]) -> bool {
    frame.len() > 14 && (frame[..6] == LLDP_MAC || frame[..6] == CDP_MAC || frame[12..14] == [0x88, 0xcc])
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::time::Instant;

    pub fn capture(interface: &str, timeout: Duration) -> Result<Vec<Vec<u8>>> {
        // SAFETY: plain socket calls with checked results; the structures
        // are zero-initialised and filled before use.
        unsafe {
            let proto = (libc::ETH_P_ALL as u16).to_be() as i32;
            let fd = libc::socket(libc::AF_PACKET, libc::SOCK_RAW, proto);
            ensure!(fd >= 0, "could not open a packet socket: {}", std::io::Error::last_os_error());
            let close = scopeguard(fd);
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
            // The adapter must pass these multicast addresses up.
            for mac in [LLDP_MAC, CDP_MAC] {
                let mut mr: libc::packet_mreq = std::mem::zeroed();
                mr.mr_ifindex = index as i32;
                mr.mr_type = libc::PACKET_MR_MULTICAST as u16;
                mr.mr_alen = 6;
                mr.mr_address[..6].copy_from_slice(&mac);
                libc::setsockopt(
                    fd,
                    libc::SOL_PACKET,
                    libc::PACKET_ADD_MEMBERSHIP,
                    &mr as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::packet_mreq>() as u32,
                );
            }
            let tv = libc::timeval { tv_sec: 1, tv_usec: 0 };
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as u32,
            );
            let started = Instant::now();
            let mut first: Option<Instant> = None;
            let mut out = Vec::new();
            let mut buf = vec![0u8; 9000];
            while started.elapsed() < timeout && first.is_none_or(|t| t.elapsed() < Duration::from_secs(3)) {
                let n = libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0);
                if n > 0 && wanted(&buf[..n as usize]) {
                    out.push(buf[..n as usize].to_vec());
                    first.get_or_insert_with(Instant::now);
                }
            }
            drop(close);
            Ok(out)
        }
    }

    struct Fd(i32);
    impl Drop for Fd {
        fn drop(&mut self) {
            // SAFETY: the descriptor is owned here.
            unsafe { libc::close(self.0) };
        }
    }
    fn scopeguard(fd: i32) -> Fd {
        Fd(fd)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::time::Instant;

    pub fn capture(interface: &str, timeout: Duration) -> Result<Vec<Vec<u8>>> {
        // SAFETY: ioctl calls on a BPF device we opened, with correctly
        // sized arguments; read results are bounds-checked.
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
            struct Fd(i32);
            impl Drop for Fd {
                fn drop(&mut self) {
                    // SAFETY: owned descriptor.
                    unsafe { libc::close(self.0) };
                }
            }
            let _guard = Fd(fd);
            let mut len: libc::c_uint = 0;
            libc::ioctl(fd, libc::BIOCGBLEN, &mut len);
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
            // Link-local multicast is only passed up in promiscuous mode.
            libc::ioctl(fd, libc::BIOCPROMISC as libc::c_ulong, std::ptr::null_mut::<libc::c_void>());
            let tv = libc::timeval { tv_sec: 1, tv_usec: 0 };
            libc::ioctl(fd, libc::BIOCSRTIMEOUT, &tv);
            let mut buf = vec![0u8; len.max(4096) as usize];
            let started = Instant::now();
            let mut first: Option<Instant> = None;
            let mut out = Vec::new();
            while started.elapsed() < timeout && first.is_none_or(|t| t.elapsed() < Duration::from_secs(3)) {
                let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 {
                    continue;
                }
                let n = n as usize;
                let mut p = 0;
                // Each packet: bpf_hdr (timeval32, caplen, datalen, hdrlen),
                // then the frame, padded to 4 bytes.
                while p + 18 <= n {
                    let caplen = u32::from_ne_bytes(buf[p + 8..p + 12].try_into()?) as usize;
                    let hdrlen = u16::from_ne_bytes(buf[p + 16..p + 18].try_into()?) as usize;
                    let Some(frame) = buf.get(p + hdrlen..p + hdrlen + caplen) else { break };
                    if wanted(frame) {
                        out.push(frame.to_vec());
                        first.get_or_insert_with(Instant::now);
                    }
                    p += (hdrlen + caplen + 3) & !3;
                }
            }
            Ok(out)
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::cmd;

    pub fn capture(_interface: &str, timeout: Duration) -> Result<Vec<Vec<u8>>> {
        ensure!(cmd::exists("pktmon"), "pktmon (Windows 10 version 2004 or newer) is needed");
        let dir = std::env::temp_dir().join(format!("netmgr-lldp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let etl = dir.join("capture.etl");
        let pcap = dir.join("capture.pcapng");
        let _ = cmd::run("pktmon", &["stop"]);
        let _ = cmd::run("pktmon", &["filter", "remove"]);
        cmd::run("pktmon", &["filter", "add", "NetMgrCDP", "-m", "01-00-0C-CC-CC-CC"])?;
        cmd::run("pktmon", &["filter", "add", "NetMgrLLDP", "-d", "LLDP"])?;
        let started = cmd::run("pktmon", &["start", "-c", "--pkt-size", "0", "-f", &etl.display().to_string()]);
        if let Err(e) = started {
            let _ = cmd::run("pktmon", &["filter", "remove"]);
            bail!("pktmon could not start: {e:#}");
        }
        // pktmon writes the file only when stopped: check every few seconds.
        let deadline = std::time::Instant::now() + timeout;
        let mut frames = Vec::new();
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(5));
            let n = cmd::run("pktmon", &["counters"]).unwrap_or_default();
            // Stop as soon as anything matching the filters was seen.
            if n.lines().any(|l| l.split_whitespace().any(|w| w.parse::<u64>().is_ok_and(|v| v > 0))) {
                break;
            }
        }
        let _ = cmd::run("pktmon", &["stop"]);
        let _ = cmd::run("pktmon", &["filter", "remove"]);
        if cmd::run("pktmon", &["etl2pcap", &etl.display().to_string(), "-o", &pcap.display().to_string()]).is_ok()
            && let Ok(data) = std::fs::read(&pcap)
        {
            frames = pcapng_frames(&data).into_iter().filter(|f| wanted(f)).collect();
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(frames)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::*;
    pub fn capture(_: &str, _: Duration) -> Result<Vec<Vec<u8>>> {
        bail!("not supported on this system")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(t: u8, v: &[u8]) -> Vec<u8> {
        let head = ((t as u16) << 9) | v.len() as u16;
        let mut out = head.to_be_bytes().to_vec();
        out.extend_from_slice(v);
        out
    }

    #[test]
    fn lldp_frame() {
        let mut f = LLDP_MAC.to_vec();
        f.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x88, 0xcc]);
        f.extend(tlv(1, &[4, 0x00, 0x11, 0x22, 0x33, 0x44, 0x00]));
        f.extend(tlv(2, b"\x05Gi1/0/12"));
        f.extend(tlv(3, &[0, 120]));
        f.extend(tlv(4, b"Desk 4.12"));
        f.extend(tlv(5, b"core-sw-01"));
        f.extend(tlv(6, b"Cisco IOS Software, C2960X"));
        f.extend(tlv(7, &[0, 0x14, 0, 0x14]));
        f.extend(tlv(8, &[5, 1, 10, 0, 0, 2, 2, 0, 0, 0, 0, 0]));
        f.extend(tlv(127, &[0x00, 0x80, 0xc2, 1, 0, 20]));
        f.extend(tlv(127, &[0x00, 0x80, 0xc2, 3, 0, 20, 5, b'U', b's', b'e', b'r', b's']));
        f.extend(tlv(127, &[0x00, 0x12, 0x0f, 1, 3, 0x6c, 0x01, 0, 30]));
        // MED network policy: voice, VLAN 30 (U=0 T=1 X=0, VID 30 << 1 …).
        let raw: u32 = (1 << 22) | (30 << 9);
        let b = raw.to_be_bytes();
        f.extend(tlv(127, &[0x00, 0x12, 0xbb, 2, 1, b[1], b[2], b[3]]));
        f.extend(tlv(0, &[]));
        let n = parse_frame(&f).unwrap();
        assert_eq!(n.protocol, "LLDP");
        assert_eq!(n.system_name.as_deref(), Some("core-sw-01"));
        assert_eq!(n.port_id.as_deref(), Some("Gi1/0/12"));
        assert_eq!(n.port_description.as_deref(), Some("Desk 4.12"));
        assert_eq!(n.chassis_id.as_deref(), Some("00:11:22:33:44:00"));
        assert_eq!(n.vlan, Some(20));
        assert_eq!(n.vlan_names, vec![(20, "Users".to_string())]);
        assert_eq!(n.voice_vlan, Some(30));
        assert_eq!(n.management, vec!["10.0.0.2".to_string()]);
        assert_eq!(n.capabilities, vec!["Bridge", "Router"]);
        assert_eq!(n.link.as_deref(), Some("1 Gbit/s full duplex"));
        assert_eq!(n.ttl, Some(120));
    }

    #[test]
    fn cdp_frame() {
        let mut body = vec![2, 180, 0, 0];
        let mut t = |ty: u16, v: &[u8]| {
            body.extend_from_slice(&ty.to_be_bytes());
            body.extend_from_slice(&((v.len() + 4) as u16).to_be_bytes());
            body.extend_from_slice(v);
        };
        t(1, b"access-sw-3.example.com");
        t(2, &[0, 0, 0, 1, 1, 1, 0xcc, 0, 4, 192, 168, 10, 3]);
        t(3, b"GigabitEthernet0/7");
        t(4, &[0, 0, 0, 0x28]);
        t(6, b"cisco WS-C2960X-48FPD-L");
        t(0x0a, &[0, 10]);
        t(0x0b, &[1]);
        t(0x0e, &[1, 0, 50]);
        let mut f = CDP_MAC.to_vec();
        f.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x07]);
        let len = (body.len() + 8) as u16;
        f.extend_from_slice(&len.to_be_bytes());
        f.extend_from_slice(&[0xaa, 0xaa, 0x03, 0x00, 0x00, 0x0c, 0x20, 0x00]);
        f.extend(body);
        let n = parse_frame(&f).unwrap();
        assert_eq!(n.protocol, "CDP");
        assert_eq!(n.system_name.as_deref(), Some("access-sw-3.example.com"));
        assert_eq!(n.port_id.as_deref(), Some("GigabitEthernet0/7"));
        assert_eq!(n.platform.as_deref(), Some("cisco WS-C2960X-48FPD-L"));
        assert_eq!(n.vlan, Some(10));
        assert_eq!(n.voice_vlan, Some(50));
        assert_eq!(n.duplex.as_deref(), Some("Full"));
        assert_eq!(n.management, vec!["192.168.10.3".to_string()]);
        assert_eq!(n.capabilities, vec!["Switch", "IGMP"]);
    }

    #[test]
    fn pcapng() {
        let frame = vec![1u8, 2, 3, 4, 5, 6, 7];
        let mut d = Vec::new();
        // Section header block (little-endian), 28 bytes.
        d.extend_from_slice(&0x0a0d0d0au32.to_le_bytes());
        d.extend_from_slice(&28u32.to_le_bytes());
        d.extend_from_slice(&0x1a2b3c4du32.to_le_bytes());
        d.extend_from_slice(&[1, 0, 0, 0]);
        d.extend_from_slice(&[0xff; 8]);
        d.extend_from_slice(&28u32.to_le_bytes());
        // Enhanced packet block: 28 + 8 (padded frame) + 4.
        let total = 28 + 8 + 4u32;
        d.extend_from_slice(&6u32.to_le_bytes());
        d.extend_from_slice(&total.to_le_bytes());
        d.extend_from_slice(&[0; 12]);
        d.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        d.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        d.extend_from_slice(&frame);
        d.push(0);
        d.extend_from_slice(&total.to_le_bytes());
        assert_eq!(pcapng_frames(&d), vec![frame]);
    }

    #[test]
    fn ignores_other_frames() {
        let mut f = vec![0xff; 12];
        f.extend_from_slice(&[0x08, 0x00, 0x45]);
        assert!(parse_frame(&f).is_none());
    }
}
