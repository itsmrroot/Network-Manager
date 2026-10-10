//! Time check (SNTP): how far this computer's clock is from time servers,
//! and how good those servers are. A clock that is off breaks logins
//! (Kerberos), certificates and the order of log messages.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

/// Well-known public time servers: label, address.
pub const SERVERS: [(&str, &str); 6] = [
    ("NTP Pool", "pool.ntp.org"),
    ("Cloudflare", "time.cloudflare.com"),
    ("Google", "time.google.com"),
    ("Apple", "time.apple.com"),
    ("Microsoft", "time.windows.com"),
    ("NIST", "time.nist.gov"),
];

/// Seconds between 1900 (NTP) and 1970 (Unix).
const EPOCH_DELTA: u64 = 2_208_988_800;

#[derive(Debug, Clone, Serialize)]
pub struct Reading {
    pub server: SocketAddr,
    /// How far ahead (+) or behind (−) this computer's clock is, in seconds.
    pub offset: f64,
    /// Round trip to the server, in seconds.
    pub delay: f64,
    /// 1: the server has its own reference clock (GPS, atomic); 2: it asks a
    /// stratum 1 server; and so on.
    pub stratum: u8,
    /// "GPS", "PPS", or the address of the server it follows.
    pub reference: String,
    /// The server says its own clock is not synchronised.
    pub unsynchronised: bool,
}

fn to_ntp(t: SystemTime) -> u64 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() + EPOCH_DELTA;
    let frac = ((d.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

fn from_ntp(v: u64) -> f64 {
    (v >> 32) as f64 - EPOCH_DELTA as f64 + (v & 0xffff_ffff) as f64 / 4_294_967_296.0
}

pub fn request(now: SystemTime) -> [u8; 48] {
    let mut p = [0u8; 48];
    p[0] = 0x23; // no leap warning, version 4, client
    p[40..48].copy_from_slice(&to_ntp(now).to_be_bytes());
    p
}

/// Reads a server's answer to `request(sent)` received at `received`.
pub fn parse(server: SocketAddr, reply: &[u8], sent: SystemTime, received: SystemTime) -> Result<Reading> {
    ensure!(reply.len() >= 48, "short answer");
    let mode = reply[0] & 7;
    ensure!(mode == 4 || mode == 5, "not a server answer");
    let stratum = reply[1];
    let u64_at = |i: usize| u64::from_be_bytes(reply[i..i + 8].try_into().unwrap_or_default());
    let origin = u64_at(24);
    if origin != to_ntp(sent) {
        bail!("the answer does not belong to this request");
    }
    let refid = &reply[12..16];
    if stratum == 0 {
        // Kiss-o'-death: the server asks to be left alone.
        bail!("the server refused: {}", String::from_utf8_lossy(refid).trim_end_matches('\0'));
    }
    let reference = if stratum == 1 {
        String::from_utf8_lossy(refid).trim_end_matches('\0').trim().to_string()
    } else if server.is_ipv4() {
        std::net::Ipv4Addr::new(refid[0], refid[1], refid[2], refid[3]).to_string()
    } else {
        refid.iter().map(|b| format!("{b:02x}")).collect()
    };
    let t1 = from_ntp(to_ntp(sent));
    let t2 = from_ntp(u64_at(32));
    let t3 = from_ntp(u64_at(40));
    let t4 = from_ntp(to_ntp(received));
    // Positive when the server is ahead: this computer is then behind,
    // so the offset of *this* clock is the negative.
    let server_ahead = ((t2 - t1) + (t3 - t4)) / 2.0;
    Ok(Reading {
        server,
        offset: -server_ahead,
        delay: ((t4 - t1) - (t3 - t2)).max(0.0),
        stratum,
        reference,
        unsynchronised: reply[0] >> 6 == 3,
    })
}

/// Asks `host` (a name or address, port 123 unless given) for the time.
pub fn query(host: &str, timeout: Duration) -> Result<Reading> {
    let with_port = if host.parse::<SocketAddr>().is_ok() || host.contains("]:") {
        host.to_string()
    } else if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:123")
    } else if host.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        host.to_string()
    } else {
        format!("{host}:123")
    };
    let server = with_port.to_socket_addrs()?.next().with_context(|| format!("{host} was not found"))?;
    let sock = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    sock.connect(server)?;
    sock.set_read_timeout(Some(timeout))?;
    let sent = SystemTime::now();
    let started = Instant::now();
    sock.send(&request(sent))?;
    let mut buf = [0u8; 512];
    let n = match sock.recv(&mut buf) {
        Ok(n) => n,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
            bail!("no answer (UDP port 123): it may not be a time server")
        }
        Err(e) => bail!("{host} refused: {e}"),
    };
    // The monotonic clock for the round trip: the wall clock may jump.
    let received = sent + started.elapsed();
    parse(server, &buf[..n], sent, received)
}

/// "+0.012 s", "−3 min 12 s".
pub fn describe_offset(seconds: f64) -> String {
    let a = seconds.abs();
    if a < 0.0005 {
        return "0 ms".into();
    }
    let sign = if seconds < 0.0 { "−" } else { "+" };
    if a < 1.0 {
        format!("{sign}{:.0} ms", a * 1000.0)
    } else if a < 120.0 {
        format!("{sign}{a:.1} s")
    } else if a < 7200.0 {
        format!("{sign}{} min {} s", (a / 60.0) as u64, (a % 60.0) as u64)
    } else {
        format!("{sign}{:.1} h", a / 3600.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        let t = UNIX_EPOCH + Duration::new(1_760_000_000, 500_000_000);
        assert!((from_ntp(to_ntp(t)) - 1_760_000_000.5).abs() < 1e-6);
        let r = request(t);
        assert_eq!(r[0], 0x23);
    }

    #[test]
    fn offset_and_delay() {
        let server: SocketAddr = "192.0.2.1:123".parse().unwrap();
        let sent = UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        // The server's clock is 2 s ahead; 40 ms each way; 1 ms inside.
        let t2 = sent + Duration::from_millis(2040);
        let t3 = t2 + Duration::from_millis(1);
        let received = sent + Duration::from_millis(81);
        let mut reply = [0u8; 48];
        reply[0] = 0x24; // version 4, server
        reply[1] = 1;
        reply[12..16].copy_from_slice(b"GPS\0");
        reply[24..32].copy_from_slice(&to_ntp(sent).to_be_bytes());
        reply[32..40].copy_from_slice(&to_ntp(t2).to_be_bytes());
        reply[40..48].copy_from_slice(&to_ntp(t3).to_be_bytes());
        let r = parse(server, &reply, sent, received).unwrap();
        assert!((r.offset + 2.0).abs() < 0.001, "{}", r.offset);
        assert!((r.delay - 0.080).abs() < 0.001, "{}", r.delay);
        assert_eq!((r.stratum, r.reference.as_str()), (1, "GPS"));
        // Another request's answer is refused.
        assert!(parse(server, &reply, sent + Duration::from_secs(1), received).is_err());
        reply[1] = 0;
        reply[12..16].copy_from_slice(b"RATE");
        reply[24..32].copy_from_slice(&to_ntp(sent).to_be_bytes());
        assert!(parse(server, &reply, sent, received).unwrap_err().to_string().contains("RATE"));
    }

    #[test]
    fn offsets_read_well() {
        assert_eq!(describe_offset(0.0123), "+12 ms");
        assert_eq!(describe_offset(-0.0002), "0 ms");
        assert_eq!(describe_offset(-3.25), "−3.2 s");
        assert_eq!(describe_offset(-192.0), "−3 min 12 s");
    }
}

// ---------------------------------------------------------------- server

/// Answers time requests on UDP `port` with this computer's clock until
/// `stop` is set, for devices in labs without internet. `stratum` is what
/// the answers claim (one more than the source this computer follows).
pub fn serve(
    listen: std::net::Ipv4Addr,
    port: u16,
    stratum: u8,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    asked: std::sync::Arc<dyn Fn(std::net::IpAddr) + Send + Sync>,
) -> Result<()> {
    let sock = UdpSocket::bind((listen, port)).map_err(|e| match e.kind() {
        std::io::ErrorKind::AddrInUse => {
            anyhow::anyhow!("UDP port {port} is in use: the system's own time service may hold it")
        }
        std::io::ErrorKind::PermissionDenied => {
            anyhow::anyhow!("port {port} needs administrator rights on this system")
        }
        _ => anyhow::anyhow!("could not open UDP port {port}: {e}"),
    })?;
    sock.set_read_timeout(Some(Duration::from_millis(300)))?;
    let mut buf = [0u8; 512];
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let received = SystemTime::now();
        if let Some(r) = answer(&buf[..n], received, SystemTime::now(), stratum) {
            let _ = sock.send_to(&r, from);
            asked(from.ip());
        }
    }
    Ok(())
}

/// The server's answer to a client request, or `None` for anything else.
pub fn answer(req: &[u8], received: SystemTime, sent: SystemTime, stratum: u8) -> Option<[u8; 48]> {
    if req.len() < 48 || req[0] & 7 != 3 {
        return None;
    }
    let version = (req[0] >> 3) & 7;
    let mut r = [0u8; 48];
    r[0] = (version.clamp(1, 4) << 3) | 4; // no leap warning, server
    r[1] = stratum.clamp(1, 15);
    r[2] = req[2]; // poll interval, as asked
    r[3] = 0xec; // precision: about a microsecond
    r[12..16].copy_from_slice(b"LOCL");
    let now = to_ntp(sent);
    r[16..24].copy_from_slice(&now.to_be_bytes()); // reference time
    r[24..32].copy_from_slice(&req[40..48]); // originate = client's transmit
    r[32..40].copy_from_slice(&to_ntp(received).to_be_bytes());
    r[40..48].copy_from_slice(&now.to_be_bytes());
    Some(r)
}

#[cfg(test)]
mod server_tests {
    use super::*;

    #[test]
    fn answers_our_own_client() {
        let sent = UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let req = request(sent);
        let server: SocketAddr = "192.0.2.1:123".parse().unwrap();
        let at = sent + Duration::from_millis(5);
        let r = answer(&req, at, at, 3).unwrap();
        let reading = parse(server, &r, sent, sent + Duration::from_millis(10)).unwrap();
        assert_eq!(reading.stratum, 3);
        assert!(reading.offset.abs() < 0.001 && (reading.delay - 0.010).abs() < 0.001);
        assert!(answer(&r, at, at, 3).is_none(), "server answers are not requests");
    }
}
