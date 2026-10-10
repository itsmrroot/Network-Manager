//! A DHCP server for labs and staging: hands out addresses from a pool,
//! fixed addresses by MAC, and the options devices use to find their
//! software and configuration (TFTP server 66 and 150, boot file 67, vendor
//! options 43) — the way switches, routers, phones and access points are set
//! up automatically ("zero-touch provisioning").
//!
//! Only for networks where no other DHCP server answers: two servers hand
//! out conflicting addresses.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::mac::Mac;

const MAGIC: [u8; 4] = [99, 130, 83, 99];

// Message types (option 53).
pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const DECLINE: u8 = 4;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;
pub const RELEASE: u8 = 7;
pub const INFORM: u8 = 8;

/// An address always given to one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Reservation {
    pub mac: String,
    pub ip: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// This computer's address on the network served; it is the server ID.
    pub server: Ipv4Addr,
    pub first: Ipv4Addr,
    pub last: Ipv4Addr,
    pub mask: Ipv4Addr,
    /// The default gateway handed out; unset for none.
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: String,
    pub lease_secs: u32,
    /// Option 66: TFTP server name or address.
    pub tftp_server: String,
    /// Option 67: boot or configuration file.
    pub bootfile: String,
    /// Option 150: TFTP servers (Cisco phones and switches).
    pub tftp_150: Vec<Ipv4Addr>,
    /// Option 43: vendor-specific information, as hex ("0104c0a8010a").
    pub vendor_hex: String,
    pub reservations: Vec<Reservation>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: Ipv4Addr::new(192, 168, 50, 1),
            first: Ipv4Addr::new(192, 168, 50, 100),
            last: Ipv4Addr::new(192, 168, 50, 199),
            mask: Ipv4Addr::new(255, 255, 255, 0),
            router: None,
            dns: Vec::new(),
            domain: String::new(),
            lease_secs: 3600,
            tftp_server: String::new(),
            bootfile: String::new(),
            tftp_150: Vec::new(),
            vendor_hex: String::new(),
            reservations: Vec::new(),
        }
    }
}

impl Config {
    fn net(&self, a: Ipv4Addr) -> u32 {
        u32::from(a) & u32::from(self.mask)
    }

    /// Mistakes that would make the server hand out unusable addresses.
    pub fn check(&self) -> Result<()> {
        ensure!(crate::adapters::mask_to_prefix(self.mask).is_some(), "{} is not a valid subnet mask", self.mask);
        ensure!(u32::from(self.first) <= u32::from(self.last), "the pool's first address is after its last");
        let n = self.net(self.server);
        ensure!(
            self.net(self.first) == n && self.net(self.last) == n,
            "the pool {} – {} is not in this computer's network ({}/{})",
            self.first,
            self.last,
            Ipv4Addr::from(n),
            crate::adapters::mask_to_prefix(self.mask).unwrap_or(0)
        );
        let bc = n | !u32::from(self.mask);
        ensure!(
            u32::from(self.first) > n && u32::from(self.last) < bc,
            "the pool must not include the network or broadcast address"
        );
        if let Some(r) = self.router {
            ensure!(self.net(r) == n, "the router {r} is not in this network");
        }
        for r in &self.reservations {
            let _: Mac = r.mac.parse().with_context(|| format!("\"{}\" is not a MAC address", r.mac))?;
            let ip: Ipv4Addr = r.ip.parse().with_context(|| format!("\"{}\" is not an IPv4 address", r.ip))?;
            ensure!(self.net(ip) == n, "the reserved address {ip} is not in this network");
        }
        hex(&self.vendor_hex)?;
        Ok(())
    }

    pub fn pool_size(&self) -> u32 {
        u32::from(self.last).saturating_sub(u32::from(self.first)) + 1
    }
}

fn hex(s: &str) -> Result<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace() && *c != ':').collect();
    ensure!(s.len().is_multiple_of(2), "option 43 must be pairs of hexadecimal digits");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("option 43 must be hexadecimal"))
        .collect()
}

/// A device and the address it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lease {
    pub mac: Mac,
    pub ip: Ipv4Addr,
    pub hostname: String,
    /// "Offered", "Leased", "Released", "Declined".
    pub state: &'static str,
    pub expires: SystemTime,
    /// What the device said it is (option 60): "Cisco Systems, Inc.", "MSFT 5.0".
    pub vendor: String,
}

/// A request the server read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: u8,
    pub xid: u32,
    pub flags: u16,
    pub ciaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    pub mac: Mac,
    pub requested: Option<Ipv4Addr>,
    pub server_id: Option<Ipv4Addr>,
    pub hostname: String,
    pub vendor: String,
}

pub fn parse(p: &[u8]) -> Option<Message> {
    if p.len() < 240 || p[0] != 1 || p[1] != 1 || p[2] != 6 || p[236..240] != MAGIC {
        return None;
    }
    let ip = |i: usize| Ipv4Addr::new(p[i], p[i + 1], p[i + 2], p[i + 3]);
    let mut m = Message {
        kind: 0,
        xid: u32::from_be_bytes(p[4..8].try_into().ok()?),
        flags: u16::from_be_bytes([p[10], p[11]]),
        ciaddr: ip(12),
        giaddr: ip(24),
        mac: Mac(p[28..34].try_into().ok()?),
        requested: None,
        server_id: None,
        hostname: String::new(),
        vendor: String::new(),
    };
    let mut i = 240;
    while i < p.len() {
        let code = p[i];
        if code == 255 {
            break;
        }
        if code == 0 {
            i += 1;
            continue;
        }
        let len = *p.get(i + 1)? as usize;
        let v = p.get(i + 2..i + 2 + len)?;
        match code {
            53 if len == 1 => m.kind = v[0],
            50 if len == 4 => m.requested = Some(Ipv4Addr::new(v[0], v[1], v[2], v[3])),
            54 if len == 4 => m.server_id = Some(Ipv4Addr::new(v[0], v[1], v[2], v[3])),
            12 => m.hostname = String::from_utf8_lossy(v).trim_end_matches('\0').to_string(),
            60 => m.vendor = String::from_utf8_lossy(v).to_string(),
            _ => {}
        }
        i += 2 + len;
    }
    (m.kind != 0).then_some(m)
}

/// The reply to `m`: an offer, acknowledgement or refusal.
pub fn reply(cfg: &Config, m: &Message, kind: u8, yiaddr: Ipv4Addr) -> Result<Vec<u8>> {
    let mut p = vec![0u8; 240];
    p[0] = 2; // reply
    p[1] = 1;
    p[2] = 6;
    p[4..8].copy_from_slice(&m.xid.to_be_bytes());
    p[10..12].copy_from_slice(&m.flags.to_be_bytes());
    p[12..16].copy_from_slice(&m.ciaddr.octets());
    p[16..20].copy_from_slice(&yiaddr.octets());
    p[20..24].copy_from_slice(&cfg.server.octets());
    p[24..28].copy_from_slice(&m.giaddr.octets());
    p[28..34].copy_from_slice(&m.mac.0);
    if !cfg.tftp_server.is_empty() && cfg.tftp_server.len() < 64 {
        p[44..44 + cfg.tftp_server.len()].copy_from_slice(cfg.tftp_server.as_bytes());
    }
    if !cfg.bootfile.is_empty() && cfg.bootfile.len() < 128 {
        p[108..108 + cfg.bootfile.len()].copy_from_slice(cfg.bootfile.as_bytes());
    }
    p[236..240].copy_from_slice(&MAGIC);
    let mut opt = |code: u8, v: &[u8]| {
        // Long values are split into several options of the same code (RFC 3396).
        for chunk in v.chunks(255) {
            p.push(code);
            p.push(chunk.len() as u8);
            p.extend_from_slice(chunk);
        }
    };
    opt(53, &[kind]);
    opt(54, &cfg.server.octets());
    if kind != NAK {
        if kind != ACK || yiaddr != Ipv4Addr::UNSPECIFIED {
            opt(51, &cfg.lease_secs.to_be_bytes());
            opt(58, &(cfg.lease_secs / 2).to_be_bytes());
            opt(59, &(cfg.lease_secs / 8 * 7).to_be_bytes());
        }
        opt(1, &cfg.mask.octets());
        let bc = (u32::from(cfg.server) & u32::from(cfg.mask)) | !u32::from(cfg.mask);
        opt(28, &Ipv4Addr::from(bc).octets());
        if let Some(r) = cfg.router {
            opt(3, &r.octets());
        }
        if !cfg.dns.is_empty() {
            opt(6, &cfg.dns.iter().flat_map(|d| d.octets()).collect::<Vec<u8>>());
        }
        if !cfg.domain.is_empty() {
            opt(15, cfg.domain.as_bytes());
        }
        if !cfg.tftp_server.is_empty() {
            opt(66, cfg.tftp_server.as_bytes());
        }
        if !cfg.bootfile.is_empty() {
            opt(67, cfg.bootfile.as_bytes());
        }
        if !cfg.tftp_150.is_empty() {
            opt(150, &cfg.tftp_150.iter().flat_map(|d| d.octets()).collect::<Vec<u8>>());
        }
        let vendor = hex(&cfg.vendor_hex)?;
        if !vendor.is_empty() {
            opt(43, &vendor);
        }
    }
    p.push(255);
    // Some old clients need at least 300 bytes.
    if p.len() < 300 {
        p.resize(300, 0);
    }
    Ok(p)
}

/// The addresses handed out, shared with the app.
pub type Leases = Arc<Mutex<Vec<Lease>>>;

/// What happened, for the app's log.
pub type Log = Arc<dyn Fn(String) + Send + Sync>;

/// Decides the answer to a message and updates the leases. Returns the
/// reply type and address, or `None` when the message is not for us.
pub fn handle(cfg: &Config, leases: &mut Vec<Lease>, m: &Message, now: SystemTime) -> Option<(u8, Ipv4Addr)> {
    let reserved: HashMap<Mac, Ipv4Addr> = cfg
        .reservations
        .iter()
        .filter_map(|r| Some((r.mac.parse::<Mac>().ok()?, r.ip.parse::<Ipv4Addr>().ok()?)))
        .collect();
    let lease_for = |l: &Vec<Lease>, mac: Mac| l.iter().position(|x| x.mac == mac);
    let taken = |l: &Vec<Lease>, ip: Ipv4Addr, mac: Mac| {
        l.iter().any(|x| x.ip == ip && x.mac != mac && x.expires > now && x.state != "Released")
            || reserved.iter().any(|(rm, rip)| *rip == ip && *rm != mac)
            || ip == cfg.server
            || Some(ip) == cfg.router
    };
    let expires = now + Duration::from_secs(cfg.lease_secs as u64);
    let in_pool = |ip: Ipv4Addr| (u32::from(cfg.first)..=u32::from(cfg.last)).contains(&u32::from(ip));
    match m.kind {
        DISCOVER => {
            let ip = reserved
                .get(&m.mac)
                .copied()
                .or_else(|| lease_for(leases, m.mac).map(|i| leases[i].ip).filter(|ip| !taken(leases, *ip, m.mac)))
                .or_else(|| m.requested.filter(|ip| in_pool(*ip) && !taken(leases, *ip, m.mac)))
                .or_else(|| {
                    (u32::from(cfg.first)..=u32::from(cfg.last))
                        .map(Ipv4Addr::from)
                        .find(|ip| {
                            !taken(leases, *ip, m.mac)
                                && !leases.iter().any(|x| x.ip == *ip && x.mac != m.mac && x.state != "Released")
                        })
                        .or_else(|| {
                            (u32::from(cfg.first)..=u32::from(cfg.last))
                                .map(Ipv4Addr::from)
                                .find(|ip| !taken(leases, *ip, m.mac))
                        })
                })?;
            let lease = Lease {
                mac: m.mac,
                ip,
                hostname: m.hostname.clone(),
                state: "Offered",
                expires: now + Duration::from_secs(60),
                vendor: m.vendor.clone(),
            };
            match lease_for(leases, m.mac) {
                Some(i) => {
                    leases[i] = Lease {
                        hostname: if m.hostname.is_empty() { leases[i].hostname.clone() } else { m.hostname.clone() },
                        ..lease
                    }
                }
                None => leases.push(lease),
            }
            Some((OFFER, ip))
        }
        REQUEST => {
            // Selecting another server's offer: forget ours.
            if let Some(s) = m.server_id
                && s != cfg.server
            {
                if let Some(i) = lease_for(leases, m.mac).filter(|&i| leases[i].state == "Offered") {
                    leases.remove(i);
                }
                return None;
            }
            let want = m.requested.unwrap_or(m.ciaddr);
            let ok_ip = reserved.get(&m.mac).map_or(in_pool(want), |r| *r == want);
            if want == Ipv4Addr::UNSPECIFIED || !ok_ip || taken(leases, want, m.mac) {
                return Some((NAK, Ipv4Addr::UNSPECIFIED));
            }
            let lease = Lease {
                mac: m.mac,
                ip: want,
                hostname: m.hostname.clone(),
                state: "Leased",
                expires,
                vendor: m.vendor.clone(),
            };
            match lease_for(leases, m.mac) {
                Some(i) => {
                    let name = if m.hostname.is_empty() { leases[i].hostname.clone() } else { m.hostname.clone() };
                    let vendor = if m.vendor.is_empty() { leases[i].vendor.clone() } else { m.vendor.clone() };
                    leases[i] = Lease { hostname: name, vendor, ..lease };
                }
                None => leases.push(lease),
            }
            Some((ACK, want))
        }
        RELEASE => {
            if let Some(i) = lease_for(leases, m.mac) {
                leases[i].state = "Released";
                leases[i].expires = now;
            }
            None
        }
        DECLINE => {
            // The address is in use by something else: keep it away.
            if let Some(ip) = m.requested
                && let Some(i) = lease_for(leases, m.mac)
            {
                leases[i] = Lease {
                    mac: Mac([0; 6]),
                    ip,
                    hostname: String::new(),
                    state: "Declined",
                    expires: now + Duration::from_secs(600),
                    vendor: String::new(),
                };
            }
            None
        }
        INFORM => Some((ACK, Ipv4Addr::UNSPECIFIED)),
        _ => None,
    }
}

/// Serves DHCP on UDP port 67 until `stop` is set. `iface` is the adapter
/// (its device name) that faces the devices; answers go out through it.
pub fn serve(cfg: Config, iface: &str, leases: Leases, stop: Arc<AtomicBool>, log: Log) -> Result<()> {
    cfg.check()?;
    let sock = socket(cfg.server, iface)?;
    sock.set_read_timeout(Some(Duration::from_millis(300)))?;
    log(format!("DHCP server on {} for {} – {}", cfg.server, cfg.first, cfg.last));
    let mut buf = [0u8; 1500];
    while !stop.load(Ordering::Relaxed) {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let Some(m) = parse(&buf[..n]) else { continue };
        let now = SystemTime::now();
        let answer = {
            let mut l = leases.lock().map_err(|_| anyhow::anyhow!("lease table poisoned"))?;
            handle(&cfg, &mut l, &m, now)
        };
        let what = match m.kind {
            DISCOVER => "DISCOVER",
            REQUEST => "REQUEST",
            RELEASE => "RELEASE",
            DECLINE => "DECLINE",
            INFORM => "INFORM",
            _ => "message",
        };
        let Some((kind, ip)) = answer else {
            log(format!("{what} from {}", m.mac));
            continue;
        };
        let packet = reply(&cfg, &m, kind, ip)?;
        // Through a relay: back to it. A client with an address: to it.
        // Otherwise broadcast, since the client has no address yet.
        let to: SocketAddr = if m.giaddr != Ipv4Addr::UNSPECIFIED {
            (m.giaddr, 67).into()
        } else if m.ciaddr != Ipv4Addr::UNSPECIFIED && kind != NAK {
            (m.ciaddr, 68).into()
        } else {
            (Ipv4Addr::BROADCAST, 68).into()
        };
        let _ = sock.send_to(&packet, to);
        let name = if m.hostname.is_empty() { String::new() } else { format!(" ({})", m.hostname) };
        let _ = from;
        log(match kind {
            OFFER => format!("{what} from {}{name} → OFFER {ip}", m.mac),
            ACK if ip == Ipv4Addr::UNSPECIFIED => format!("INFORM from {}{name} → ACK", m.mac),
            ACK => format!("{what} from {}{name} → ACK {ip}", m.mac),
            _ => format!("{what} from {}{name} → NAK", m.mac),
        });
    }
    Ok(())
}

/// A socket on port 67 that sends broadcasts out of `iface`.
fn socket(server: Ipv4Addr, iface: &str) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    s.set_broadcast(true)?;
    #[cfg(target_os = "linux")]
    {
        let _ = s.bind_device(Some(iface.as_bytes()));
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(name) = std::ffi::CString::new(iface) {
            // SAFETY: a valid C string; the index is checked.
            let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            if let Some(i) = std::num::NonZeroU32::new(index) {
                let _ = s.bind_device_by_index_v4(Some(i));
            }
        }
    }
    let _ = iface;
    // Windows delivers broadcasts to a socket bound to the adapter's
    // address and sends them out of it; elsewhere the socket is bound to
    // the adapter itself above.
    let bind_ip = if cfg!(windows) { server } else { Ipv4Addr::UNSPECIFIED };
    s.bind(&SocketAddr::from((bind_ip, 67)).into()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!("UDP port 67 is in use: another DHCP server (Internet Sharing?) runs on this computer")
        } else if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow::anyhow!("port 67 needs administrator rights on this system")
        } else {
            anyhow::anyhow!("could not open UDP port 67: {e}")
        }
    })?;
    if bind_ip == Ipv4Addr::UNSPECIFIED && server == Ipv4Addr::UNSPECIFIED {
        bail!("choose the adapter that faces the devices");
    }
    Ok(s.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(kind: u8, mac: [u8; 6], extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut p = vec![0u8; 240];
        p[0] = 1;
        p[1] = 1;
        p[2] = 6;
        p[4..8].copy_from_slice(&0xdead_beefu32.to_be_bytes());
        p[28..34].copy_from_slice(&mac);
        p[236..240].copy_from_slice(&MAGIC);
        p.extend_from_slice(&[53, 1, kind]);
        for (c, v) in extra {
            p.push(*c);
            p.push(v.len() as u8);
            p.extend_from_slice(v);
        }
        p.push(255);
        p
    }

    fn cfg() -> Config {
        Config {
            server: Ipv4Addr::new(10, 9, 0, 1),
            first: Ipv4Addr::new(10, 9, 0, 100),
            last: Ipv4Addr::new(10, 9, 0, 102),
            mask: Ipv4Addr::new(255, 255, 255, 0),
            router: Some(Ipv4Addr::new(10, 9, 0, 1)),
            dns: vec![Ipv4Addr::new(1, 1, 1, 1)],
            tftp_server: "10.9.0.1".into(),
            bootfile: "network-confg".into(),
            tftp_150: vec![Ipv4Addr::new(10, 9, 0, 1)],
            vendor_hex: "f1 04 0a 09 00 01".into(),
            reservations: vec![Reservation {
                mac: "02:00:00:00:00:99".into(),
                ip: "10.9.0.50".into(),
                name: "core".into(),
            }],
            ..Config::default()
        }
    }

    #[test]
    fn offers_and_acknowledges() {
        let c = cfg();
        c.check().unwrap();
        let mut leases = Vec::new();
        let now = SystemTime::now();
        let d = parse(&request(
            DISCOVER,
            [2, 0, 0, 0, 0, 1],
            &[(12, b"sw-lab1".to_vec()), (60, b"Cisco Systems, Inc.".to_vec())],
        ))
        .unwrap();
        assert_eq!(d.hostname, "sw-lab1");
        assert_eq!(handle(&c, &mut leases, &d, now), Some((OFFER, Ipv4Addr::new(10, 9, 0, 100))));
        let r = parse(&request(REQUEST, [2, 0, 0, 0, 0, 1], &[(50, vec![10, 9, 0, 100]), (54, vec![10, 9, 0, 1])]))
            .unwrap();
        assert_eq!(handle(&c, &mut leases, &r, now), Some((ACK, Ipv4Addr::new(10, 9, 0, 100))));
        assert_eq!(
            (leases[0].state, leases[0].hostname.as_str(), leases[0].vendor.as_str()),
            ("Leased", "sw-lab1", "Cisco Systems, Inc.")
        );
        // A second device gets the next address; the reserved one gets its own.
        let d2 = parse(&request(DISCOVER, [2, 0, 0, 0, 0, 2], &[])).unwrap();
        assert_eq!(handle(&c, &mut leases, &d2, now), Some((OFFER, Ipv4Addr::new(10, 9, 0, 101))));
        let d3 = parse(&request(DISCOVER, [2, 0, 0, 0, 0, 0x99], &[])).unwrap();
        assert_eq!(handle(&c, &mut leases, &d3, now), Some((OFFER, Ipv4Addr::new(10, 9, 0, 50))));
        // Someone else's address is refused.
        let bad = parse(&request(REQUEST, [2, 0, 0, 0, 0, 3], &[(50, vec![10, 9, 0, 100])])).unwrap();
        assert_eq!(handle(&c, &mut leases, &bad, now), Some((NAK, Ipv4Addr::UNSPECIFIED)));
        // Another server's offer chosen: no answer.
        let other = parse(&request(REQUEST, [2, 0, 0, 0, 0, 2], &[(54, vec![10, 9, 0, 254])])).unwrap();
        assert_eq!(handle(&c, &mut leases, &other, now), None);
    }

    #[test]
    fn pool_runs_out() {
        let c = cfg();
        let mut leases = Vec::new();
        let now = SystemTime::now();
        for i in 1..=3u8 {
            let d = parse(&request(DISCOVER, [2, 0, 0, 0, 1, i], &[])).unwrap();
            let (_, ip) = handle(&c, &mut leases, &d, now).unwrap();
            let r = parse(&request(REQUEST, [2, 0, 0, 0, 1, i], &[(50, ip.octets().to_vec())])).unwrap();
            assert_eq!(handle(&c, &mut leases, &r, now).unwrap().0, ACK);
        }
        let d = parse(&request(DISCOVER, [2, 0, 0, 0, 1, 9], &[])).unwrap();
        assert_eq!(handle(&c, &mut leases, &d, now), None);
        // A released address can be given again.
        let rel = parse(&request(RELEASE, [2, 0, 0, 0, 1, 1], &[])).unwrap();
        handle(&c, &mut leases, &rel, now);
        assert_eq!(handle(&c, &mut leases, &d, now), Some((OFFER, Ipv4Addr::new(10, 9, 0, 100))));
    }

    #[test]
    fn replies_carry_the_options() {
        let c = cfg();
        let m = parse(&request(DISCOVER, [2, 0, 0, 0, 0, 1], &[])).unwrap();
        let p = reply(&c, &m, OFFER, Ipv4Addr::new(10, 9, 0, 100)).unwrap();
        // The client test parser reads it like any server's offer.
        let o = crate::dhcp::parse_offer(&p, 0xdead_beef).unwrap();
        assert_eq!(o.server, Some(Ipv4Addr::new(10, 9, 0, 1)));
        assert_eq!(o.address, Some(Ipv4Addr::new(10, 9, 0, 100)));
        assert_eq!(o.routers, [Ipv4Addr::new(10, 9, 0, 1)]);
        assert_eq!(o.dns, [Ipv4Addr::new(1, 1, 1, 1)]);
        assert_eq!((o.tftp_server.as_deref(), o.boot_file.as_deref()), (Some("10.9.0.1"), Some("network-confg")));
        let has = |code: u8, v: &[u8]| {
            p.windows(v.len() + 2).any(|w| w[0] == code && w[1] as usize == v.len() && &w[2..] == v)
        };
        assert!(has(66, b"10.9.0.1"));
        assert!(has(67, b"network-confg"));
        assert!(has(150, &[10, 9, 0, 1]));
        assert!(has(43, &[0xf1, 4, 10, 9, 0, 1]));
        assert!(p.len() >= 300);
    }

    #[test]
    fn checks_the_settings() {
        let mut c = cfg();
        c.first = Ipv4Addr::new(10, 8, 0, 1);
        assert!(c.check().is_err());
        let mut c = cfg();
        c.vendor_hex = "zz".into();
        assert!(c.check().is_err());
        let mut c = cfg();
        c.reservations[0].mac = "nope".into();
        assert!(c.check().is_err());
        assert_eq!(cfg().pool_size(), 3);
    }
}
