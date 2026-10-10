//! Small servers network engineers need on a laptop:
//!
//! * a TFTP server (RFC 1350, with the block size, transfer size and
//!   timeout options of RFC 2347–2349) to copy firmware and configurations
//!   to and from switches, routers and phones;
//! * a syslog receiver (RFC 3164 and 5424) to see what devices log;
//! * a throughput test between two computers running Network Manager.
//!
//! TFTP (port 69) and syslog (port 514) use privileged ports: Windows and
//! macOS allow them to every user; Linux needs administrator rights, or a
//! port of 1024 and above.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

// ---------------------------------------------------------------------------
// TFTP

#[derive(Debug, Clone, Serialize)]
pub struct Transfer {
    pub id: u64,
    pub peer: SocketAddr,
    pub file: String,
    /// true: the device uploads to this computer.
    pub upload: bool,
    pub bytes: u64,
    /// The size, when known.
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
    #[serde(skip)]
    pub started: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct TftpOptions {
    pub root: PathBuf,
    pub port: u16,
    /// The address to listen on: one adapter's, or 0.0.0.0 for every network.
    pub listen: Ipv4Addr,
    /// Let devices upload files (configuration backups).
    pub allow_upload: bool,
    /// Let uploads replace existing files.
    pub overwrite: bool,
}

/// The transfers of a running server, shared with whoever shows them.
pub type Transfers = Arc<Mutex<Vec<Transfer>>>;

static TRANSFER_ID: AtomicU64 = AtomicU64::new(1);

const RRQ: u16 = 1;
const WRQ: u16 = 2;
const DATA: u16 = 3;
const ACK: u16 = 4;
const ERROR: u16 = 5;
const OACK: u16 = 6;

/// The file a request names, inside `root`: no absolute paths, no `..`.
pub fn safe_path(root: &Path, requested: &str) -> Result<PathBuf> {
    let rel = Path::new(requested.trim_start_matches(['/', '\\']));
    let mut out = root.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => bail!("access outside the TFTP folder is not allowed"),
        }
    }
    ensure!(out != root, "no file name");
    Ok(out)
}

/// A TFTP request: opcode, file name, mode, options.
/// A parsed request: opcode, file name, mode, options.
pub type Request = (u16, String, String, Vec<(String, String)>);

pub fn parse_request(p: &[u8]) -> Option<Request> {
    let op = u16::from_be_bytes([*p.first()?, *p.get(1)?]);
    if op != RRQ && op != WRQ {
        return None;
    }
    let mut parts = p[2..].split(|b| *b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
    let file = parts.next()?;
    let mode = parts.next()?.to_lowercase();
    let mut opts = Vec::new();
    while let (Some(k), Some(v)) = (parts.next(), parts.next()) {
        if !k.is_empty() {
            opts.push((k.to_lowercase(), v));
        }
    }
    Some((op, file, mode, opts))
}

fn error_packet(code: u16, msg: &str) -> Vec<u8> {
    let mut p = ERROR.to_be_bytes().to_vec();
    p.extend_from_slice(&code.to_be_bytes());
    p.extend_from_slice(msg.as_bytes());
    p.push(0);
    p
}

/// Runs a TFTP server until `stop` is set. `log` gets a line per event.
pub fn tftp_serve(
    opts: TftpOptions,
    transfers: Transfers,
    stop: Arc<AtomicBool>,
    log: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<()> {
    ensure!(opts.root.is_dir(), "the folder {} does not exist", opts.root.display());
    let sock = UdpSocket::bind((opts.listen, opts.port)).with_context(|| port_hint(opts.port))?;
    sock.set_read_timeout(Some(Duration::from_millis(300)))?;
    log(format!("TFTP server listening on {}:{}, folder {}", opts.listen, opts.port, opts.root.display()));
    let mut buf = [0u8; 1500];
    while !stop.load(Ordering::Relaxed) {
        let Ok((n, peer)) = sock.recv_from(&mut buf) else { continue };
        let Some((op, file, mode, options)) = parse_request(&buf[..n]) else { continue };
        let (opts, transfers, log, stop) = (opts.clone(), transfers.clone(), log.clone(), stop.clone());
        std::thread::spawn(move || {
            let id = TRANSFER_ID.fetch_add(1, Ordering::Relaxed);
            let upload = op == WRQ;
            log(format!("{peer} {} {file} ({mode})", if upload { "uploads" } else { "downloads" }));
            if let Ok(mut t) = transfers.lock() {
                t.push(Transfer {
                    id,
                    peer,
                    file: file.clone(),
                    upload,
                    bytes: 0,
                    total: None,
                    done: false,
                    error: None,
                    started: Some(Instant::now()),
                });
            }
            let update = |f: &dyn Fn(&mut Transfer)| {
                if let Ok(mut t) = transfers.lock()
                    && let Some(x) = t.iter_mut().find(|x| x.id == id)
                {
                    f(x);
                }
            };
            let result = transfer(&opts, peer, upload, &file, &mode, &options, &stop, &update);
            match &result {
                Ok(bytes) => log(format!("{peer} {file}: done, {}", crate::adapters::format_bytes(*bytes))),
                Err(e) => log(format!("{peer} {file}: {e:#}")),
            }
            update(&|t| {
                t.done = true;
                t.error = result.as_ref().err().map(|e| format!("{e:#}"));
            });
        });
    }
    log("TFTP server stopped".into());
    Ok(())
}

/// Changes the transfer shown to people.
type Update<'a> = dyn Fn(&dyn Fn(&mut Transfer)) + 'a;

#[allow(clippy::too_many_arguments)]
fn transfer(
    opts: &TftpOptions,
    peer: SocketAddr,
    upload: bool,
    file: &str,
    mode: &str,
    options: &[(String, String)],
    stop: &AtomicBool,
    update: &Update,
) -> Result<u64> {
    // A new port for this transfer (the "transfer identifier").
    let sock = UdpSocket::bind((opts.listen, 0))?;
    sock.connect(peer)?;
    let fail = |code: u16, msg: String| -> Result<u64> {
        let _ = sock.send(&error_packet(code, &msg));
        bail!(msg)
    };
    if mode != "octet" && mode != "netascii" {
        return fail(4, format!("mode {mode} is not supported"));
    }
    let path = match safe_path(&opts.root, file) {
        Ok(p) => p,
        Err(e) => return fail(2, e.to_string()),
    };
    let mut blksize = 512usize;
    let mut timeout = Duration::from_secs(3);
    let mut oack: Vec<(String, String)> = Vec::new();
    for (k, v) in options {
        match k.as_str() {
            "blksize" => {
                if let Ok(b) = v.parse::<usize>() {
                    blksize = b.clamp(8, 65464);
                    oack.push((k.clone(), blksize.to_string()));
                }
            }
            "timeout" => {
                if let Ok(t) = v.parse::<u64>()
                    && (1..=255).contains(&t)
                {
                    timeout = Duration::from_secs(t);
                    oack.push((k.clone(), t.to_string()));
                }
            }
            "tsize" if !upload => {
                if let Ok(m) = std::fs::metadata(&path) {
                    oack.push((k.clone(), m.len().to_string()));
                }
            }
            "tsize" => {
                if let Ok(t) = v.parse::<u64>() {
                    update(&|x| x.total = Some(t));
                    oack.push((k.clone(), t.to_string()));
                }
            }
            _ => {}
        }
    }
    sock.set_read_timeout(Some(timeout))?;
    let oack_packet = (!oack.is_empty()).then(|| {
        let mut p = OACK.to_be_bytes().to_vec();
        for (k, v) in &oack {
            p.extend_from_slice(k.as_bytes());
            p.push(0);
            p.extend_from_slice(v.as_bytes());
            p.push(0);
        }
        p
    });
    let mut buf = vec![0u8; blksize + 4];

    // Sends `packet` until the peer acknowledges `block`.
    let send_wait_ack = |packet: &[u8], block: u16, buf: &mut [u8]| -> Result<()> {
        for _ in 0..6 {
            if stop.load(Ordering::Relaxed) {
                bail!("the server was stopped");
            }
            sock.send(packet)?;
            let start = Instant::now();
            while start.elapsed() < timeout {
                let Ok(n) = sock.recv(buf) else { break };
                if n >= 4 {
                    let op = u16::from_be_bytes([buf[0], buf[1]]);
                    let b = u16::from_be_bytes([buf[2], buf[3]]);
                    if op == ACK && b == block {
                        return Ok(());
                    }
                    if op == ERROR {
                        bail!("the device cancelled: {}", String::from_utf8_lossy(&buf[4..n]).trim_end_matches('\0'));
                    }
                }
            }
        }
        bail!("the device stopped answering")
    };

    if !upload {
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(_) => return fail(1, format!("{file} was not found")),
        };
        let total = f.metadata().map(|m| m.len()).ok();
        update(&|x| x.total = total);
        if let Some(p) = &oack_packet {
            send_wait_ack(p, 0, &mut buf)?;
        }
        let mut block: u16 = 1;
        let mut sent = 0u64;
        let mut data = vec![0u8; blksize];
        loop {
            let mut n = 0;
            while n < blksize {
                let r = f.read(&mut data[n..])?;
                if r == 0 {
                    break;
                }
                n += r;
            }
            let mut p = DATA.to_be_bytes().to_vec();
            p.extend_from_slice(&block.to_be_bytes());
            p.extend_from_slice(&data[..n]);
            send_wait_ack(&p, block, &mut buf)?;
            sent += n as u64;
            update(&|x| x.bytes = sent);
            if n < blksize {
                return Ok(sent);
            }
            block = block.wrapping_add(1);
        }
    }

    if !opts.allow_upload {
        return fail(2, "uploads are turned off on this server".into());
    }
    if path.exists() && !opts.overwrite {
        return fail(6, format!("{file} already exists"));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("part");
    let mut f = File::create(&tmp)?;
    let first = oack_packet.unwrap_or_else(|| [ACK.to_be_bytes(), 0u16.to_be_bytes()].concat());
    sock.send(&first)?;
    let mut expect: u16 = 1;
    let mut got = 0u64;
    let mut last_ack = first;
    let mut retries = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&tmp);
            bail!("the server was stopped");
        }
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            Err(_) => {
                retries += 1;
                if retries > 6 {
                    let _ = std::fs::remove_file(&tmp);
                    bail!("the device stopped sending");
                }
                sock.send(&last_ack)?;
                continue;
            }
        };
        if n < 4 {
            continue;
        }
        let op = u16::from_be_bytes([buf[0], buf[1]]);
        let block = u16::from_be_bytes([buf[2], buf[3]]);
        if op == ERROR {
            let _ = std::fs::remove_file(&tmp);
            bail!("the device cancelled");
        }
        if op != DATA {
            continue;
        }
        if block == expect {
            f.write_all(&buf[4..n])?;
            got += (n - 4) as u64;
            update(&|x| x.bytes = got);
            expect = expect.wrapping_add(1);
            retries = 0;
        }
        last_ack = [ACK.to_be_bytes(), block.to_be_bytes()].concat();
        sock.send(&last_ack)?;
        if block == expect.wrapping_sub(1) && n - 4 < blksize {
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &path)?;
            return Ok(got);
        }
    }
}

fn port_hint(port: u16) -> String {
    if port < 1024 && cfg!(target_os = "linux") {
        format!(
            "port {port} could not be opened: on Linux, ports below 1024 need administrator rights (or choose a port of 1024 or more)"
        )
    } else {
        format!("port {port} could not be opened: is another server using it?")
    }
}

// ---------------------------------------------------------------------------
// Syslog

#[derive(Debug, Clone, Serialize)]
pub struct SyslogMessage {
    pub received: SystemTime,
    pub from: IpAddr,
    pub facility: u8,
    pub severity: u8,
    /// The message without its priority (and RFC 5424 header fields).
    pub text: String,
    pub host: Option<String>,
}

pub const SEVERITIES: [&str; 8] = ["Emergency", "Alert", "Critical", "Error", "Warning", "Notice", "Info", "Debug"];

/// Reads `<PRI>…` (RFC 3164 or RFC 5424).
pub fn parse_syslog(data: &[u8], from: IpAddr) -> SyslogMessage {
    let s = String::from_utf8_lossy(data).trim_end_matches(['\0', '\n', '\r']).to_string();
    let (pri, rest) = match s.strip_prefix('<').and_then(|r| r.split_once('>')) {
        Some((p, r)) if p.len() <= 3 => (p.parse::<u16>().unwrap_or(13), r.to_string()),
        _ => (13, s.clone()),
    };
    let mut host = None;
    let mut text = rest.clone();
    // RFC 5424: "1 TIMESTAMP HOST APP PROCID MSGID SD MSG".
    if let Some(r) = rest.strip_prefix("1 ") {
        let parts: Vec<&str> = r.splitn(7, ' ').collect();
        if parts.len() == 7 {
            host = Some(parts[1].to_string()).filter(|h| h != "-");
            let msg = parts[6];
            let msg = msg.strip_prefix('-').map_or(msg, str::trim_start);
            text = format!("{}: {msg}", parts[2]);
        }
    }
    SyslogMessage {
        received: SystemTime::now(),
        from,
        facility: (pri / 8) as u8,
        severity: (pri % 8) as u8,
        text,
        host,
    }
}

/// Splits a TCP syslog stream into messages: "LEN MSG" (octet counting,
/// RFC 6587) or one message per line. Returns the messages and what is
/// left for the next read.
pub fn split_tcp_syslog(buf: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let mut out = Vec::new();
    let mut at = 0;
    while at < buf.len() {
        let rest = &buf[at..];
        let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && rest.get(digits) == Some(&b' ') {
            let len: usize = std::str::from_utf8(&rest[..digits]).ok().and_then(|d| d.parse().ok()).unwrap_or(0);
            let start = digits + 1;
            if rest.len() < start + len {
                break;
            }
            out.push(rest[start..start + len].to_vec());
            at += start + len;
        } else if let Some(nl) = rest.iter().position(|&c| c == b'\n') {
            if nl > 0 {
                out.push(rest[..nl].to_vec());
            }
            at += nl + 1;
        } else {
            break;
        }
    }
    (out, at)
}

/// Receives syslog messages on UDP and TCP `port` until `stop` is set.
pub fn syslog_serve(
    listen: Ipv4Addr,
    port: u16,
    stop: Arc<AtomicBool>,
    message: Arc<dyn Fn(SyslogMessage) + Send + Sync>,
) -> Result<()> {
    let sock = UdpSocket::bind((listen, port)).with_context(|| port_hint(port))?;
    sock.set_read_timeout(Some(Duration::from_millis(300)))?;
    // TCP too, for devices that must not lose messages; optional, since
    // another program may hold the TCP port.
    if let Ok(listener) = std::net::TcpListener::bind((listen, port)) {
        let (stop, message) = (stop.clone(), message.clone());
        let _ = listener.set_nonblocking(true);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut conn, from)) => {
                        let (stop, message) = (stop.clone(), message.clone());
                        std::thread::spawn(move || {
                            use std::io::Read;
                            let _ = conn.set_nonblocking(false);
                            let _ = conn.set_read_timeout(Some(Duration::from_millis(500)));
                            let mut pending: Vec<u8> = Vec::new();
                            let mut buf = [0u8; 8192];
                            while !stop.load(Ordering::Relaxed) {
                                match conn.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        pending.extend_from_slice(&buf[..n]);
                                        let (msgs, used) = split_tcp_syslog(&pending);
                                        pending.drain(..used);
                                        for m in msgs {
                                            message(parse_syslog(&m, from.ip()));
                                        }
                                        if pending.len() > 1 << 20 {
                                            pending.clear();
                                        }
                                    }
                                    Err(e)
                                        if matches!(
                                            e.kind(),
                                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                        ) => {}
                                    Err(_) => break,
                                }
                            }
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(150)),
                }
            }
        });
    }
    let mut buf = vec![0u8; 8192];
    while !stop.load(Ordering::Relaxed) {
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            message(parse_syslog(&buf[..n], from.ip()));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Throughput test

/// The port the throughput server listens on.
pub const THROUGHPUT_PORT: u16 = 5299;
const HELLO: &str = "NETMGR-TP1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Direction {
    /// This computer sends.
    Upload,
    /// This computer receives.
    Download,
}

/// Accepts throughput tests until `stop` is set. `log` gets a line per test.
pub fn throughput_serve(
    listen: Ipv4Addr,
    port: u16,
    stop: Arc<AtomicBool>,
    log: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<()> {
    let listener = TcpListener::bind((listen, port)).with_context(|| port_hint(port))?;
    listener.set_nonblocking(true)?;
    log(format!("Throughput server listening on port {port}"));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let (log, stop) = (log.clone(), stop.clone());
                std::thread::spawn(move || {
                    let r = serve_one(stream, &stop);
                    match r {
                        Ok((dir, bytes, secs)) => log(format!(
                            "{peer}: {} {:.1} Mbit/s ({})",
                            if dir == Direction::Upload { "received" } else { "sent" },
                            bytes as f64 * 8.0 / secs / 1e6,
                            crate::adapters::format_bytes(bytes)
                        )),
                        Err(e) => log(format!("{peer}: {e:#}")),
                    }
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    Ok(())
}

/// One test, as the server: returns the client's direction, bytes, seconds.
fn serve_one(mut s: TcpStream, stop: &AtomicBool) -> Result<(Direction, u64, f64)> {
    s.set_nonblocking(false)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while s.read(&mut b)? == 1 && b[0] != b'\n' && line.len() < 64 {
        line.push(b[0]);
    }
    let hello = String::from_utf8_lossy(&line).to_string();
    let parts: Vec<&str> = hello.split_whitespace().collect();
    ensure!(parts.first() == Some(&HELLO) && parts.len() == 3, "not a throughput test");
    let secs: u64 = parts[2].parse::<u64>()?.clamp(1, 60);
    let started = Instant::now();
    let limit = Duration::from_secs(secs);
    let mut buf = vec![0u8; 128 * 1024];
    let mut total = 0u64;
    if parts[1] == "up" {
        s.set_read_timeout(Some(Duration::from_secs(3)))?;
        loop {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => total += n as u64,
                Err(_) => break,
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
        let secs = started.elapsed().as_secs_f64();
        let _ = writeln!(s, "{total}");
        Ok((Direction::Upload, total, secs))
    } else {
        while started.elapsed() < limit && !stop.load(Ordering::Relaxed) {
            s.write_all(&buf)?;
            total += buf.len() as u64;
        }
        Ok((Direction::Download, total, started.elapsed().as_secs_f64()))
    }
}

/// Measures throughput to a computer running the throughput server.
/// `sample` gets the current Mbit/s about four times a second.
pub fn throughput_test(
    host: &str,
    port: u16,
    dir: Direction,
    seconds: u64,
    cancel: &AtomicBool,
    sample: &dyn Fn(f64),
) -> Result<f64> {
    let addr = crate::tools::resolve(host, port)?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .with_context(|| format!("{addr} does not answer: start the throughput server on that computer first"))?;
    s.set_nodelay(true)?;
    writeln!(s, "{HELLO} {} {seconds}", if dir == Direction::Upload { "up" } else { "down" })?;
    let started = Instant::now();
    let limit = Duration::from_secs(seconds);
    let mut buf = vec![0u8; 128 * 1024];
    let mut total = 0u64;
    let mut mark = (Instant::now(), 0u64);
    let mut tick = |total: u64| {
        let el = mark.0.elapsed();
        if el >= Duration::from_millis(250) {
            sample((total - mark.1) as f64 * 8.0 / el.as_secs_f64() / 1e6);
            mark = (Instant::now(), total);
        }
    };
    match dir {
        Direction::Upload => {
            while started.elapsed() < limit && !cancel.load(Ordering::Relaxed) {
                s.write_all(&buf)?;
                total += buf.len() as u64;
                tick(total);
            }
            s.shutdown(std::net::Shutdown::Write)?;
            // What the server counted: the bytes still in flight are not
            // counted twice.
            s.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut answer = String::new();
            s.read_to_string(&mut answer)?;
            let counted: u64 = answer.trim().parse().unwrap_or(total);
            Ok(counted as f64 * 8.0 / started.elapsed().as_secs_f64() / 1e6)
        }
        Direction::Download => {
            s.set_read_timeout(Some(Duration::from_secs(5)))?;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                match s.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        total += n as u64;
                        tick(total);
                    }
                    Err(_) => break,
                }
            }
            Ok(total as f64 * 8.0 / started.elapsed().as_secs_f64() / 1e6)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside() {
        let root = Path::new("/srv/tftp");
        assert_eq!(safe_path(root, "/fw/c2960.bin").unwrap(), root.join("fw/c2960.bin"));
        assert_eq!(safe_path(root, "\\config.txt").unwrap(), root.join("config.txt"));
        assert!(safe_path(root, "../etc/passwd").is_err());
        assert!(safe_path(root, "a/../../b").is_err());
        assert!(safe_path(root, "").is_err());
    }

    #[test]
    fn requests() {
        let mut p = vec![0, 1];
        p.extend_from_slice(b"fw.bin\x00octet\x00blksize\x001428\x00tsize\x000\x00");
        let (op, file, mode, opts) = parse_request(&p).unwrap();
        assert_eq!((op, file.as_str(), mode.as_str()), (1, "fw.bin", "octet"));
        assert_eq!(opts, vec![("blksize".into(), "1428".into()), ("tsize".into(), "0".into())]);
        assert!(parse_request(&[0, 3, 0, 1]).is_none());
    }

    #[test]
    fn syslog_messages() {
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        let m = parse_syslog(b"<189>52: *Mar  1 00:01:02: %LINK-3-UPDOWN: Interface Gi0/1, changed state to up", ip);
        assert_eq!((m.facility, m.severity), (23, 5));
        assert!(m.text.contains("%LINK-3-UPDOWN"));
        let m = parse_syslog(b"<34>1 2026-10-08T22:14:15.003Z sw1 sshd 812 ID47 - Login failed", ip);
        assert_eq!((m.facility, m.severity), (4, 2));
        assert_eq!(m.host.as_deref(), Some("sw1"));
        assert_eq!(m.text, "sshd: Login failed");
        let m = parse_syslog(b"no priority", ip);
        assert_eq!(m.severity, 5);
    }

    /// A real transfer through the server, both ways, with options.
    #[test]
    fn tftp_round_trip() {
        let dir = std::env::temp_dir().join(format!("netmgr-tftp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let content: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("fw.bin"), &content).unwrap();
        // Find a free port.
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let opts =
            TftpOptions { root: dir.clone(), port, listen: Ipv4Addr::LOCALHOST, allow_upload: true, overwrite: false };
        let transfers: Transfers = Default::default();
        let s = stop.clone();
        let server = std::thread::spawn(move || tftp_serve(opts, transfers, s, Arc::new(|_| {})));
        std::thread::sleep(Duration::from_millis(200));

        // Download with blksize 1024.
        let c = UdpSocket::bind("127.0.0.1:0").unwrap();
        c.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut buf = [0u8; 2048];
        // The server may still be starting on a busy machine: ask again.
        let (n, peer) = (0..10)
            .find_map(|_| {
                c.send_to(b"\x00\x01fw.bin\x00octet\x00blksize\x001024\x00", ("127.0.0.1", port)).unwrap();
                c.recv_from(&mut buf).ok()
            })
            .expect("no answer from the TFTP server");
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        assert_eq!(&buf[..2], &[0, 6], "OACK expected, got {:?}", &buf[..n]);
        c.send_to(&[0, 4, 0, 0], peer).unwrap();
        let mut got = Vec::new();
        loop {
            let (n, _) = c.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..2], &[0, 3]);
            got.extend_from_slice(&buf[4..n]);
            c.send_to(&[0, 4, buf[2], buf[3]], peer).unwrap();
            if n - 4 < 1024 {
                break;
            }
        }
        assert_eq!(got, content);

        // Upload without options.
        c.send_to(b"\x00\x02backup.cfg\x00octet\x00", ("127.0.0.1", port)).unwrap();
        let (_, peer) = c.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..4], &[0, 4, 0, 0]);
        let mut pkt = vec![0, 3, 0, 1];
        pkt.extend_from_slice(b"hostname sw1\n");
        c.send_to(&pkt, peer).unwrap();
        let (_, _) = c.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..4], &[0, 4, 0, 1]);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(std::fs::read(dir.join("backup.cfg")).unwrap(), b"hostname sw1\n");

        // Escaping the folder is refused.
        c.send_to(b"\x00\x01../secret\x00octet\x00", ("127.0.0.1", port)).unwrap();
        let (_, _) = c.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..2], &[0, 5]);

        stop.store(true, Ordering::Relaxed);
        server.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn throughput_loopback() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let server = std::thread::spawn(move || throughput_serve(Ipv4Addr::LOCALHOST, port, s, Arc::new(|_| {})));
        std::thread::sleep(Duration::from_millis(200));
        let cancel = AtomicBool::new(false);
        let up = throughput_test("127.0.0.1", port, Direction::Upload, 1, &cancel, &|_| {}).unwrap();
        let down = throughput_test("127.0.0.1", port, Direction::Download, 1, &cancel, &|_| {}).unwrap();
        assert!(up > 10.0 && down > 10.0, "{up} {down}");
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap().unwrap();
    }
}

#[cfg(test)]
mod tcp_syslog_tests {
    use super::*;

    #[test]
    fn frames() {
        // A whole counted message, then one still arriving.
        let msg = b"<13>1 - host app - - - hello world";
        let mut data = format!("{} ", msg.len()).into_bytes();
        data.extend_from_slice(msg);
        data.extend_from_slice(b"20 <14>partial");
        let (m, used) = split_tcp_syslog(&data);
        assert_eq!(m, [msg.to_vec()]);
        assert_eq!(used, 3 + msg.len());
        let (m, used) = split_tcp_syslog(b"<13>line one\n<14>line two\n<15>part");
        assert_eq!((m.len(), used), (2, 26));
    }
}
