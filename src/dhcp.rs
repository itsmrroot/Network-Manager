//! DHCP test: asks the network for an address (DHCPDISCOVER) and lists
//! every DHCP server that offers one, with what it offers. More than one
//! server usually means a rogue one (a home router plugged into an office
//! network, a misconfigured VM host) — the classic cause of "some PCs get
//! a wrong address".
//!
//! Only a DISCOVER is sent, never a REQUEST: no address is taken and the
//! computer's own settings stay as they are.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::mac::{Mac, random_bytes};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    /// The DHCP server (its identifier, else the sender).
    pub server: Option<Ipv4Addr>,
    /// The address it offers this computer.
    pub address: Option<Ipv4Addr>,
    pub mask: Option<Ipv4Addr>,
    pub routers: Vec<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    pub lease_seconds: Option<u32>,
    pub ntp: Vec<Ipv4Addr>,
    /// Boot server and file (PXE, phones, switches).
    pub tftp_server: Option<String>,
    pub boot_file: Option<String>,
    /// The relay the offer came through (a router's `ip helper-address`).
    pub relay: Option<Ipv4Addr>,
    pub millis: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    /// The hardware address the request was sent for.
    pub mac: String,
    pub offers: Vec<Offer>,
}

impl TestResult {
    /// The different servers that answered.
    pub fn servers(&self) -> Vec<Ipv4Addr> {
        let mut s: Vec<Ipv4Addr> = self.offers.iter().filter_map(|o| o.server).collect();
        s.sort();
        s.dedup();
        s
    }
}

pub fn discover_packet(xid: u32, mac: Mac) -> Vec<u8> {
    let mut p = vec![0u8; 240];
    p[0] = 1; // BOOTREQUEST
    p[1] = 1; // Ethernet
    p[2] = 6;
    p[4..8].copy_from_slice(&xid.to_be_bytes());
    p[10] = 0x80; // answer by broadcast: this computer has no address to answer to
    p[28..34].copy_from_slice(&mac.0);
    p[236..240].copy_from_slice(&[99, 130, 83, 99]);
    p.extend_from_slice(&[53, 1, 1]); // DHCPDISCOVER
    p.extend_from_slice(&[61, 7, 1]);
    p.extend_from_slice(&mac.0);
    p.extend_from_slice(&[55, 11, 1, 3, 6, 15, 42, 51, 54, 58, 59, 66, 67]);
    p.extend_from_slice(&[12, 6]);
    p.extend_from_slice(b"netmgr");
    p.push(255);
    p.resize(300, 0);
    p
}

fn ips(v: &[u8]) -> Vec<Ipv4Addr> {
    v.as_chunks::<4>().0.iter().map(|c| Ipv4Addr::from(*c)).collect()
}

fn text(v: &[u8]) -> String {
    String::from_utf8_lossy(v).trim_matches('\0').trim().to_string()
}

/// Reads a DHCPOFFER answering `xid`.
pub fn parse_offer(p: &[u8], xid: u32) -> Option<Offer> {
    if p.len() < 240 || p[0] != 2 || p[4..8] != xid.to_be_bytes() || p[236..240] != [99, 130, 83, 99] {
        return None;
    }
    let ip = |i: usize| Some(Ipv4Addr::new(p[i], p[i + 1], p[i + 2], p[i + 3])).filter(|a| !a.is_unspecified());
    let mut o = Offer { address: ip(16), relay: ip(24), ..Default::default() };
    let siaddr = ip(20);
    let (sname, file) = (text(&p[44..108]), text(&p[108..236]));
    let mut kind = 0;
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
            53 if len == 1 => kind = v[0],
            54 if len == 4 => o.server = ips(v).first().copied(),
            1 if len == 4 => o.mask = ips(v).first().copied(),
            3 => o.routers = ips(v),
            6 => o.dns = ips(v),
            15 => o.domain = Some(text(v)).filter(|s| !s.is_empty()),
            42 => o.ntp = ips(v),
            51 if len == 4 => o.lease_seconds = Some(u32::from_be_bytes([v[0], v[1], v[2], v[3]])),
            66 => o.tftp_server = Some(text(v)).filter(|s| !s.is_empty()),
            67 => o.boot_file = Some(text(v)).filter(|s| !s.is_empty()),
            _ => {}
        }
        i += 2 + len;
    }
    if kind != 2 {
        return None;
    }
    o.tftp_server = o.tftp_server.or(Some(sname).filter(|s| !s.is_empty())).or(siaddr.map(|a| a.to_string()));
    o.boot_file = o.boot_file.or(Some(file).filter(|s| !s.is_empty()));
    Some(o)
}

/// Sends a DHCPDISCOVER from `interface` (its device name) and collects
/// the offers for `seconds`.
pub fn test(interface: Option<&str>, seconds: u64) -> Result<TestResult> {
    let adapter = match interface {
        Some(i) => crate::adapters::find(i)?,
        None => crate::adapters::default_adapter().context("not connected to a network")?,
    };
    // The adapter's own address, so that a reservation shows; a random
    // private one when the system hides it.
    let mac = adapter.mac.unwrap_or_else(Mac::random);
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_broadcast(true)?;
    // Out of the chosen adapter, not whichever has the default route.
    #[cfg(target_os = "macos")]
    if let Ok(name) = std::ffi::CString::new(adapter.device.clone()) {
        // SAFETY: if_nametoindex reads a NUL-terminated string.
        let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if let Some(i) = std::num::NonZeroU32::new(index) {
            let _ = sock.bind_device_by_index_v4(Some(i));
        }
    }
    #[cfg(target_os = "linux")]
    let _ = sock.bind_device(Some(adapter.device.as_bytes()));
    sock.bind(&SockAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 68)))
        .context("port 68 (DHCP client) is not available")?;
    let r = random_bytes();
    let xid = u32::from_be_bytes([r[0], r[1], r[2], r[3]]);
    let packet = discover_packet(xid, mac);
    let to = SockAddr::from(SocketAddr::from((Ipv4Addr::BROADCAST, 67)));
    let started = Instant::now();
    sock.send_to(&packet, &to)?;
    let mut offers: Vec<Offer> = Vec::new();
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 1500];
    let deadline = Duration::from_secs(seconds.max(1));
    let mut resent = false;
    while started.elapsed() < deadline {
        // Ask once more halfway: a lost packet should not hide a server.
        if !resent && started.elapsed() > deadline / 2 {
            let _ = sock.send_to(&packet, &to);
            resent = true;
        }
        sock.set_read_timeout(Some(Duration::from_millis(250)))?;
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        // SAFETY: the first `n` bytes were written by recv_from.
        let data: Vec<u8> = buf[..n].iter().map(|b| unsafe { b.assume_init() }).collect();
        if let Some(mut o) = parse_offer(&data, xid) {
            o.millis = started.elapsed().as_millis();
            if o.server.is_none() {
                o.server = from.as_socket_ipv4().map(|s| *s.ip());
            }
            if !offers.iter().any(|x| x.server == o.server && x.address == o.address) {
                offers.push(o);
            }
        }
    }
    Ok(TestResult { mac: mac.to_string(), offers })
}

/// [`test`], with administrator rights when port 68 needs them (Linux).
pub fn test_elevated(interface: Option<&str>, seconds: u64) -> Result<TestResult> {
    match test(interface, seconds) {
        Ok(r) => Ok(r),
        Err(e) if !crate::cmd::is_admin() && cfg!(unix) => {
            let exe = crate::cmd::helper_exe().map_err(|_| e)?;
            let iface = interface.map(|i| format!(" --interface {}", crate::cmd::sh_quote(i))).unwrap_or_default();
            let script = format!(
                "{} dhcp-test{iface} --seconds {seconds} --json",
                crate::cmd::sh_quote(&exe.display().to_string())
            );
            let out = crate::cmd::admin_sh(&script)?;
            serde_json::from_str(out.trim()).context("unexpected answer from the DHCP test")
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_round_trip() {
        let mac: Mac = "02:11:22:33:44:55".parse().unwrap();
        let mut p = discover_packet(0xdeadbeef, mac);
        assert_eq!(p.len(), 300);
        // Turn it into an offer.
        p[0] = 2;
        p[16..20].copy_from_slice(&[192, 168, 1, 77]);
        p.truncate(240);
        p.extend_from_slice(&[53, 1, 2, 54, 4, 192, 168, 1, 1, 1, 4, 255, 255, 255, 0, 3, 4, 192, 168, 1, 1]);
        p.extend_from_slice(&[6, 8, 1, 1, 1, 1, 8, 8, 8, 8, 51, 4, 0, 1, 81, 128, 15, 4, b'l', b'a', b'b', 0, 255]);
        let o = parse_offer(&p, 0xdeadbeef).unwrap();
        assert_eq!(o.address, Some(Ipv4Addr::new(192, 168, 1, 77)));
        assert_eq!(o.server, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(o.mask, Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(o.dns.len(), 2);
        assert_eq!(o.lease_seconds, Some(86400));
        assert_eq!(o.domain.as_deref(), Some("lab"));
        assert!(parse_offer(&p, 1).is_none());
    }
}
