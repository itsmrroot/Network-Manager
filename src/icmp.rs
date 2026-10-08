//! Pings without starting a `ping` process for each one, so that dozens of
//! hosts can be watched every second.
//!
//! * macOS and Linux: an unprivileged ICMP socket (`SOCK_DGRAM`,
//!   `IPPROTO_ICMP`), which needs no administrator rights. Linux only allows
//!   it to the groups in `net.ipv4.ping_group_range`, which most desktop
//!   distributions open to everyone.
//! * Windows: `IcmpSendEcho` from the IP Helper API.
//!
//! Where neither works (IPv6 on Windows, a locked-down Linux), the system's
//! `ping` is used for that probe.

use std::net::IpAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

/// Sequence numbers, shared by every probe of this process.
static SEQ: AtomicU16 = AtomicU16::new(1);

/// One echo request: the round-trip time, or `None` when no reply came.
pub fn ping(ip: IpAddr, timeout: Duration) -> Result<Option<Duration>> {
    match imp::ping(ip, timeout) {
        Ok(r) => Ok(r),
        // No ICMP socket allowed: the system's ping still works.
        Err(_) => system_ping(ip, timeout),
    }
}

/// One probe with the system's `ping`.
pub fn system_ping(ip: IpAddr, timeout: Duration) -> Result<Option<Duration>> {
    let ms = timeout.as_millis().max(1000);
    let host = ip.to_string();
    let (prog, args): (&str, Vec<String>) = if cfg!(windows) {
        ("ping", vec!["-n".into(), "1".into(), "-w".into(), ms.to_string(), host])
    } else if cfg!(target_os = "macos") {
        (if ip.is_ipv6() { "ping6" } else { "ping" }, vec!["-c".into(), "1".into(), "-W".into(), ms.to_string(), host])
    } else {
        ("ping", vec!["-c".into(), "1".into(), "-W".into(), (ms / 1000).max(1).to_string(), host])
    };
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = crate::cmd::output(prog, &refs)?;
    Ok(out.stdout.lines().find_map(|l| match crate::tools::parse_ping_line(l) {
        crate::tools::PingLine::Reply(ms) => Some(Duration::from_secs_f64(ms / 1000.0)),
        _ => None,
    }))
}

/// The Internet checksum (RFC 1071).
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in data.chunks(2) {
        let word =
            if chunk.len() == 2 { u16::from_be_bytes([chunk[0], chunk[1]]) } else { u16::from_be_bytes([chunk[0], 0]) };
        sum += word as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// An echo request: type, code, checksum, identifier, sequence, payload.
pub fn echo_request(v6: bool, id: u16, seq: u16) -> Vec<u8> {
    let mut p = vec![if v6 { 128 } else { 8 }, 0, 0, 0];
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(b"netmgr-ping-0123456789abcdefghij");
    // ICMPv6 checksums cover a pseudo-header the kernel fills in.
    if !v6 {
        let c = checksum(&p);
        p[2..4].copy_from_slice(&c.to_be_bytes());
    }
    p
}

/// Finds the echo reply with `seq` in a received datagram (with or without
/// an IPv4 header in front, as macOS delivers it).
pub fn is_reply(buf: &[u8], v6: bool, seq: u16) -> bool {
    let mut b = buf;
    if !v6 && b.first().is_some_and(|x| x >> 4 == 4) {
        let ihl = ((b[0] & 0x0f) as usize) * 4;
        if b.len() < ihl + 8 {
            return false;
        }
        b = &b[ihl..];
    }
    b.len() >= 8 && b[0] == if v6 { 129 } else { 0 } && u16::from_be_bytes([b[6], b[7]]) == seq
}

#[cfg(unix)]
mod imp {
    use super::*;
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    use std::mem::MaybeUninit;
    use std::net::SocketAddr;

    pub fn ping(ip: IpAddr, timeout: Duration) -> Result<Option<Duration>> {
        let v6 = ip.is_ipv6();
        let (domain, proto) = if v6 { (Domain::IPV6, Protocol::ICMPV6) } else { (Domain::IPV4, Protocol::ICMPV4) };
        let sock = Socket::new(domain, Type::DGRAM, Some(proto))?;
        let addr = SockAddr::from(SocketAddr::new(ip, 0));
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let id = std::process::id() as u16;
        let packet = echo_request(v6, id, seq);
        let started = Instant::now();
        sock.send_to(&packet, &addr)?;
        let mut buf = [MaybeUninit::<u8>::uninit(); 1500];
        loop {
            let left = timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Ok(None);
            }
            sock.set_read_timeout(Some(left))?;
            let (n, from) = match sock.recv_from(&mut buf) {
                Ok(r) => r,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            };
            // SAFETY: recv_from initialised the first `n` bytes.
            let data: Vec<u8> = buf[..n].iter().map(|b| unsafe { b.assume_init() }).collect();
            if from.as_socket().map(|s| s.ip()) == Some(ip) && is_reply(&data, v6, seq) {
                return Ok(Some(started.elapsed()));
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ICMP_ECHO_REPLY, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho,
    };

    pub fn ping(ip: IpAddr, timeout: Duration) -> Result<Option<Duration>> {
        let IpAddr::V4(v4) = ip else { anyhow::bail!("IPv6 uses the system's ping") };
        let data = b"netmgr-ping-0123456789abcdefghij";
        let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + data.len() + 64];
        let started = Instant::now();
        // SAFETY: the handle is checked and closed; the buffers outlive the
        // call and their sizes are passed along.
        unsafe {
            let handle = IcmpCreateFile();
            if handle.is_null() || handle as isize == -1 {
                anyhow::bail!("ICMP is not available");
            }
            let n = IcmpSendEcho(
                handle,
                u32::from_ne_bytes(v4.octets()),
                data.as_ptr().cast(),
                data.len() as u16,
                std::ptr::null(),
                reply.as_mut_ptr().cast(),
                reply.len() as u32,
                timeout.as_millis().clamp(1, u32::MAX as u128) as u32,
            );
            IcmpCloseHandle(handle);
            let _ = SEQ.load(Ordering::Relaxed);
            if n == 0 {
                return Ok(None);
            }
            let r = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
            if r.Status != 0 {
                return Ok(None);
            }
            // RoundTripTime has millisecond resolution; under 1 ms counts as the measured time.
            let rtt = Duration::from_millis(r.RoundTripTime as u64);
            Ok(Some(if rtt.is_zero() { started.elapsed().min(Duration::from_millis(1)) } else { rtt }))
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;
    pub fn ping(_: IpAddr, _: Duration) -> Result<Option<Duration>> {
        anyhow::bail!("not supported")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_and_packets() {
        // RFC 1071 example.
        assert_eq!(checksum(&[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7]), !0xddf2);
        let p = echo_request(false, 0x1234, 7);
        assert_eq!(checksum(&p), 0);
        let mut reply = p.clone();
        reply[0] = 0;
        assert!(is_reply(&reply, false, 7));
        assert!(!is_reply(&reply, false, 8));
        let mut with_ip = vec![0x45; 1];
        with_ip.extend_from_slice(&[0; 19]);
        with_ip.extend_from_slice(&reply);
        assert!(is_reply(&with_ip, false, 7));
    }

    #[test]
    fn pings_localhost() {
        let r = ping("127.0.0.1".parse().unwrap(), Duration::from_secs(2)).unwrap();
        assert!(r.is_some());
    }
}
