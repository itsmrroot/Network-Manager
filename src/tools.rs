//! Diagnostic tools: ping, traceroute, port check and Wake-on-LAN.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::cmd;
use crate::mac::Mac;

/// One line of ping output, understood.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PingLine {
    /// A reply, with its round-trip time in milliseconds.
    Reply(f64),
    /// No reply (timed out or unreachable).
    Lost,
    /// Anything else (headers, summaries).
    Other,
}

/// The system's `ping` for `host`: `count` probes, or until stopped with 0.
pub fn ping_command(host: &str, count: u32) -> (String, Vec<String>) {
    let host = host.trim().to_string();
    let v6 = host.parse::<IpAddr>().is_ok_and(|a| a.is_ipv6());
    if cfg!(windows) {
        let mut args = if count == 0 { vec!["-t".to_string()] } else { vec!["-n".to_string(), count.to_string()] };
        args.extend(["-w".into(), "2000".into(), host]);
        ("ping".into(), args)
    } else {
        let mut args = Vec::new();
        if count > 0 {
            args.extend(["-c".to_string(), count.to_string()]);
        }
        // macOS ping waits in ms with -W; Linux in seconds.
        if cfg!(target_os = "linux") {
            args.extend(["-W".into(), "2".into()]);
        } else {
            args.extend(["-W".into(), "2000".into()]);
        }
        args.push(host);
        let prog = if v6 && cfg!(target_os = "macos") { "ping6" } else { "ping" };
        (prog.into(), args)
    }
}

/// Reads a ping line in any of the systems' formats and languages:
/// a reply has a time in ms (`time=12.3 ms`, `time<1ms`, `Zeit=12ms`).
pub fn parse_ping_line(line: &str) -> PingLine {
    let l = line.to_lowercase();
    if l.contains("timeout")
        || l.contains("timed out")
        || l.contains("unreachable")
        || l.contains("zeitüberschreitung")
        || l.contains("nicht erreichbar")
        || l.contains("no answer")
        || l.contains("general failure")
    {
        return PingLine::Lost;
    }
    // A summary line ("min/avg/max", "Minimum = …") is not a reply.
    if l.contains("min/") || l.contains("minimum") || l.contains("average") || l.contains("statistics") {
        return PingLine::Other;
    }
    for key in
        ["time=", "time<", "zeit=", "zeit<", "temps=", "temps<", "tiempo=", "tiempo<", "время=", "süre=", "durata="]
    {
        if let Some(pos) = l.find(key) {
            let rest = &l[pos + key.len()..];
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',').collect();
            if let Ok(v) = num.replace(',', ".").parse::<f64>() {
                // "time<1ms" counts as 0.5 ms.
                return PingLine::Reply(if key.ends_with('<') { v / 2.0 } else { v });
            }
        }
    }
    PingLine::Other
}

/// Ping statistics as lines come in.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PingStats {
    pub sent: u32,
    pub received: u32,
    pub times: Vec<f64>,
}

impl PingStats {
    pub fn add(&mut self, line: PingLine) {
        match line {
            PingLine::Reply(ms) => {
                self.sent += 1;
                self.received += 1;
                self.times.push(ms);
            }
            PingLine::Lost => self.sent += 1,
            PingLine::Other => {}
        }
    }

    pub fn loss_percent(&self) -> f64 {
        if self.sent == 0 { 0.0 } else { 100.0 * (self.sent - self.received) as f64 / self.sent as f64 }
    }

    pub fn min(&self) -> Option<f64> {
        self.times.iter().copied().reduce(f64::min)
    }

    pub fn max(&self) -> Option<f64> {
        self.times.iter().copied().reduce(f64::max)
    }

    pub fn avg(&self) -> Option<f64> {
        (!self.times.is_empty()).then(|| self.times.iter().sum::<f64>() / self.times.len() as f64)
    }

    /// Jitter: the mean difference between consecutive replies.
    pub fn jitter(&self) -> Option<f64> {
        (self.times.len() > 1)
            .then(|| self.times.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (self.times.len() - 1) as f64)
    }
}

/// Pings `host` with the system's ping; each line goes to `line` with its
/// meaning.
pub fn ping(host: &str, count: u32, cancel: &AtomicBool, mut line: impl FnMut(&str, PingLine)) -> Result<()> {
    ensure!(!host.trim().is_empty(), "enter an address or a name");
    let (prog, args) = ping_command(host, count);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    cmd::stream(&prog, &refs, cancel, |l| line(l, parse_ping_line(l)))?;
    Ok(())
}

/// Runs the system's traceroute to `host`, line by line.
pub fn traceroute(host: &str, cancel: &AtomicBool, line: impl FnMut(&str)) -> Result<()> {
    let host = host.trim();
    ensure!(!host.is_empty(), "enter an address or a name");
    let v6 = host.parse::<IpAddr>().is_ok_and(|a| a.is_ipv6());
    let (prog, args): (&str, Vec<&str>) = if cfg!(windows) {
        ("tracert", vec!["-d", "-w", "1500", "-h", "30", host])
    } else if cfg!(target_os = "macos") {
        (if v6 { "traceroute6" } else { "traceroute" }, vec!["-n", "-w", "2", "-q", "1", "-m", "30", host])
    } else if cmd::exists("traceroute") {
        ("traceroute", vec!["-n", "-w", "2", "-q", "1", "-m", "30", host])
    } else {
        // Part of iputils, which most distributions install.
        ("tracepath", vec!["-n", host])
    };
    cmd::stream(prog, &args, cancel, line)?;
    Ok(())
}

/// Resolves a name or address to one socket address.
pub fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    let host = host.trim().trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/');
    if let Ok(ip) = host.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let mut addrs: Vec<SocketAddr> =
        (host, port).to_socket_addrs().with_context(|| format!("{host} was not found"))?.collect();
    addrs.sort_by_key(|a| a.is_ipv6());
    addrs.into_iter().next().with_context(|| format!("{host} has no address"))
}

#[derive(Debug, Clone, Serialize)]
pub struct PortResult {
    pub port: u16,
    pub open: bool,
    pub millis: Option<u128>,
    pub service: &'static str,
}

/// Port lists people pick from.
pub const PORT_PRESETS: &[(&str, &str)] = &[
    ("Common", "21,22,23,25,53,80,110,143,443,445,587,993,995,3306,3389,5432,5900,8080,8443"),
    ("Web", "80,443,8000,8008,8080,8443,8888"),
    ("Remote access", "22,23,3389,5900,5938"),
    ("Mail", "25,110,143,465,587,993,995"),
    ("File sharing", "21,139,445,548,2049"),
    ("Databases", "1433,1521,3306,5432,6379,27017"),
];

/// Parses "22, 80, 8000-8010" into ports (at most 2048).
pub fn parse_ports(s: &str) -> Result<Vec<u16>> {
    let mut out = Vec::new();
    for part in s.split([',', ' ', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (u16, u16) = (
                a.trim().parse().with_context(|| format!("\"{a}\" is not a port"))?,
                b.trim().parse().with_context(|| format!("\"{b}\" is not a port"))?,
            );
            ensure!(a <= b && a > 0, "\"{part}\" is not a port range");
            out.extend(a..=b);
        } else {
            let p: u16 = part.parse().with_context(|| format!("\"{part}\" is not a port"))?;
            ensure!(p > 0, "port 0 cannot be checked");
            out.push(p);
        }
    }
    out.sort_unstable();
    out.dedup();
    ensure!(!out.is_empty(), "enter at least one port");
    ensure!(out.len() <= 2048, "at most 2048 ports at a time");
    Ok(out)
}

/// Checks which `ports` of `host` accept a TCP connection.
pub fn check_ports(
    host: &str,
    ports: &[u16],
    timeout: Duration,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize) + Sync),
) -> Result<(IpAddr, Vec<PortResult>)> {
    let addr = resolve(host, 0)?;
    let results = Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..64.min(ports.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= ports.len() || cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let p = ports[i];
                    let t = Instant::now();
                    let open = TcpStream::connect_timeout(&SocketAddr::new(addr.ip(), p), timeout).is_ok();
                    if let Ok(mut r) = results.lock() {
                        r.push(PortResult {
                            port: p,
                            open,
                            millis: open.then(|| t.elapsed().as_millis()),
                            service: crate::scan::service_name(p),
                        });
                        progress(r.len());
                    }
                }
            });
        }
    });
    let mut r = results.into_inner().unwrap_or_default();
    r.sort_by_key(|p| p.port);
    Ok((addr.ip(), r))
}

/// Sends a Wake-on-LAN "magic packet" for `mac`: to the whole network and,
/// when given, to a subnet's broadcast address or a host.
pub fn wake(mac: Mac, target: Option<Ipv4Addr>) -> Result<()> {
    let mut packet = vec![0xffu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac.0);
    }
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    let mut targets = vec![Ipv4Addr::BROADCAST];
    targets.extend(target);
    // The broadcast of each local network too: some systems do not send
    // 255.255.255.255 out of every adapter.
    for a in crate::adapters::list(false).unwrap_or_default() {
        if let Some((ip, p)) = a.main_ipv4() {
            let mask = u32::from(crate::adapters::prefix_to_mask(p));
            targets.push(Ipv4Addr::from(u32::from(ip) | !mask));
        }
    }
    targets.dedup();
    let mut sent = false;
    for t in targets {
        for port in [9, 7] {
            sent |= sock.send_to(&packet, (t, port)).is_ok();
        }
    }
    ensure!(sent, "the packet could not be sent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_lines() {
        let cases = [
            ("64 bytes from 1.1.1.1: icmp_seq=0 ttl=57 time=12.345 ms", PingLine::Reply(12.345)),
            ("Reply from 192.168.1.1: bytes=32 time<1ms TTL=64", PingLine::Reply(0.5)),
            ("Antwort von 8.8.8.8: Bytes=32 Zeit=14ms TTL=117", PingLine::Reply(14.0)),
            ("Request timeout for icmp_seq 3", PingLine::Lost),
            ("Request timed out.", PingLine::Lost),
            ("Reply from 192.168.1.5: Destination host unreachable.", PingLine::Lost),
            ("round-trip min/avg/max/stddev = 11.1/12.2/13.3/0.5 ms", PingLine::Other),
            ("    Minimum = 13ms, Maximum = 15ms, Average = 14ms", PingLine::Other),
            ("PING 1.1.1.1 (1.1.1.1): 56 data bytes", PingLine::Other),
        ];
        for (line, want) in cases {
            assert_eq!(parse_ping_line(line), want, "{line}");
        }
    }

    #[test]
    fn stats() {
        let mut s = PingStats::default();
        for l in [PingLine::Reply(10.0), PingLine::Lost, PingLine::Reply(14.0), PingLine::Other] {
            s.add(l);
        }
        assert_eq!((s.sent, s.received), (3, 2));
        assert!((s.loss_percent() - 33.3).abs() < 0.1);
        assert_eq!(s.avg(), Some(12.0));
        assert_eq!(s.jitter(), Some(4.0));
    }

    #[test]
    fn port_lists() {
        assert_eq!(parse_ports("22, 80 8000-8002,80").unwrap(), vec![22, 80, 8000, 8001, 8002]);
        assert!(parse_ports("0").is_err());
        assert!(parse_ports("10-5").is_err());
        assert!(parse_ports("http").is_err());
        assert!(parse_ports("1-65535").is_err());
        for (_, list) in PORT_PRESETS {
            parse_ports(list).unwrap();
        }
    }

    #[test]
    fn local_port_check() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (_, r) =
            check_ports("127.0.0.1", &[port], Duration::from_secs(1), &AtomicBool::new(false), &|_| {}).unwrap();
        assert!(r[0].open);
    }
}

// ---------------------------------------------------------------- MTU

/// Did a packet of that size get through?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fits {
    Yes,
    /// Too big for this computer's adapter or a router on the way.
    TooBig,
    /// No answer: too big and dropped silently, or lost.
    NoAnswer,
}

/// The system ping with "don't fragment" and `payload` bytes of data.
pub fn mtu_ping_command(ip: Ipv4Addr, payload: u16) -> (&'static str, Vec<String>) {
    let (ip, size) = (ip.to_string(), payload.to_string());
    let args: Vec<&str> = if cfg!(windows) {
        vec!["-n", "1", "-w", "1000", "-f", "-l", &size, &ip]
    } else if cfg!(target_os = "macos") {
        vec!["-c", "1", "-t", "1", "-D", "-s", &size, &ip]
    } else {
        vec!["-c", "1", "-W", "1", "-M", "do", "-s", &size, &ip]
    };
    ("ping", args.into_iter().map(String::from).collect())
}

/// What one try's output means.
pub fn mtu_ping_result(success: bool, output: &str) -> Fits {
    let o = output.to_lowercase();
    if o.contains("too long") || o.contains("fragment") || o.contains("frag needed") || o.contains("too big") {
        Fits::TooBig
    } else if success && (!cfg!(windows) || o.contains("ttl=")) {
        Fits::Yes
    } else {
        Fits::NoAnswer
    }
}

fn mtu_try(ip: Ipv4Addr, payload: u16) -> Result<Fits> {
    let (program, args) = mtu_ping_command(ip, payload);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = cmd::output(program, &args)?;
    Ok(mtu_ping_result(out.success, &format!("{}\n{}", out.stdout, out.stderr)))
}

/// The result of a path MTU test.
#[derive(Debug, Clone, Serialize)]
pub struct Mtu {
    pub host: Ipv4Addr,
    /// The largest packet that gets through whole, in bytes (IPv4).
    pub mtu: u16,
    /// Did the largest tried size get through (jumbo frames)?
    pub at_most: bool,
}

/// Finds the largest packet that reaches `host` without being split, by
/// pinging with "don't fragment" (IPv4: payload + 28 bytes of headers).
/// `step` is told each size tried and whether it got through.
pub fn path_mtu(host: &str, cancel: &AtomicBool, mut step: impl FnMut(u16, Fits)) -> Result<Mtu> {
    let ip = match resolve(host, 0)?.ip() {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(_) => (host, 0)
            .to_socket_addrs()?
            .find_map(|a| match a.ip() {
                IpAddr::V4(v4) => Some(v4),
                _ => None,
            })
            .with_context(|| format!("{host} has no IPv4 address"))?,
    };
    let first = mtu_try(ip, 32)?;
    ensure!(first == Fits::Yes, "{host} does not answer ping, so its MTU cannot be measured");
    // Lost replies look like "too big": a size counts as too big only when
    // it fails twice.
    let fits = |size: u16, step: &mut dyn FnMut(u16, Fits)| -> Result<bool> {
        let mut r = mtu_try(ip, size)?;
        if r == Fits::NoAnswer {
            r = mtu_try(ip, size)?;
        }
        step(size, r);
        Ok(r == Fits::Yes)
    };
    // The usual answer first: 1500 (Ethernet) means 1472 bytes of data.
    let (mut lo, mut hi) = (32u16, 8972u16);
    if fits(1472, &mut step)? {
        if !fits(1473, &mut step)? {
            return Ok(Mtu { host: ip, mtu: 1500, at_most: false });
        }
        lo = 1473;
    } else {
        hi = 1471;
    }
    while lo < hi {
        ensure!(!cancel.load(Ordering::Relaxed), "stopped");
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid, &mut step)? {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok(Mtu { host: ip, mtu: lo + 28, at_most: lo == 8972 })
}

#[cfg(test)]
mod mtu_tests {
    use super::*;

    #[test]
    fn understands_ping() {
        assert_eq!(mtu_ping_result(false, "ping: sendto: Message too long"), Fits::TooBig);
        assert_eq!(mtu_ping_result(false, "Packet needs to be fragmented but DF set."), Fits::TooBig);
        assert_eq!(
            mtu_ping_result(false, "From 10.0.0.1 icmp_seq=1 Frag needed and DF set (mtu = 1400)"),
            Fits::TooBig
        );
        assert_eq!(mtu_ping_result(false, "1 packets transmitted, 0 received"), Fits::NoAnswer);
        if !cfg!(windows) {
            assert_eq!(mtu_ping_result(true, "64 bytes from 1.1.1.1: icmp_seq=0 ttl=57"), Fits::Yes);
        }
    }

    #[test]
    fn commands() {
        let (_, args) = mtu_ping_command(Ipv4Addr::new(192, 0, 2, 1), 1472);
        assert!(args.contains(&"1472".to_string()) && args.contains(&"192.0.2.1".to_string()));
    }
}
