//! Remote sessions inside the app: SSH through the system's own `ssh`
//! client in a pseudo-terminal, and Telnet built in (Windows no longer
//! ships a Telnet client). Saved sessions are kept by the app.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Protocol {
    #[default]
    Ssh,
    Telnet,
}

impl Protocol {
    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh => 22,
            Protocol::Telnet => 23,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ssh => "SSH",
            Protocol::Telnet => "Telnet",
        }
    }
}

/// A saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Saved {
    pub name: String,
    /// "Core switches", "Building A", …; empty for none.
    pub group: String,
    pub protocol: Protocol,
    pub host: String,
    /// 0: the protocol's usual port.
    pub port: u16,
    pub user: String,
    /// The command that prints the configuration ("show running-config");
    /// empty: not backed up.
    pub backup_command: String,
}

impl Saved {
    pub fn port(&self) -> u16 {
        if self.port == 0 { self.protocol.default_port() } else { self.port }
    }

    /// "admin@10.0.0.1" (and ":2222" for another port).
    pub fn target(&self) -> String {
        let mut s = if self.user.is_empty() { self.host.clone() } else { format!("{}@{}", self.user, self.host) };
        if self.port != 0 && self.port != self.protocol.default_port() {
            s += &format!(":{}", self.port);
        }
        s
    }

    /// Parses "admin@10.0.0.1", "10.0.0.1:2222" or "admin@sw1:22".
    pub fn parse(protocol: Protocol, s: &str) -> Saved {
        let s = s.trim();
        let (user, rest) = match s.rsplit_once('@') {
            Some((u, r)) => (u.to_string(), r),
            None => (String::new(), s),
        };
        // A port only after a name or IPv4 address (IPv6 has colons).
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap_or(0)),
            _ => (rest.trim_matches(['[', ']']).to_string(), 0),
        };
        Saved { name: host.clone(), protocol, host, port, user, ..Default::default() }
    }
}

fn sessions_file() -> std::path::PathBuf {
    crate::profiles::config_dir().join("sessions.json")
}

/// The saved sessions, shared by the app and the command line.
pub fn load() -> Result<Vec<Saved>> {
    match std::fs::read_to_string(sessions_file()) {
        Ok(text) => serde_json::from_str(&text).context("sessions.json is damaged"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(list: &[Saved]) -> Result<()> {
    let path = sessions_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(list)?)?;
    Ok(())
}

enum Link {
    Pty { master: Box<dyn MasterPty + Send>, child: Box<dyn Child + Send + Sync> },
    Telnet { stream: TcpStream },
}

/// An open session: what the device sends arrives on `received`.
pub struct Remote {
    pub received: Receiver<Vec<u8>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    link: Link,
    ended: Arc<AtomicBool>,
    telnet: bool,
}

/// Is the system's `ssh` client there?
pub fn ssh_available() -> bool {
    crate::cmd::exists("ssh")
}

impl Remote {
    pub fn open(s: &Saved, rows: u16, cols: u16) -> Result<Remote> {
        match s.protocol {
            Protocol::Ssh => Self::ssh(s, rows, cols),
            Protocol::Telnet => Self::telnet(&s.host, s.port()),
        }
    }

    fn ssh(s: &Saved, rows: u16, cols: u16) -> Result<Remote> {
        if !ssh_available() {
            bail!("the ssh client is missing (Windows: Settings → Optional features → OpenSSH Client)");
        }
        let pty = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("could not open a terminal")?;
        let mut cmd = CommandBuilder::new("ssh");
        cmd.env("TERM", "xterm-256color");
        if s.port() != 22 {
            cmd.args(["-p", &s.port().to_string()]);
        }
        // Keeps idle sessions through firewalls that drop quiet connections.
        cmd.args(["-o", "ServerAliveInterval=30"]);
        let target = if s.user.is_empty() { s.host.clone() } else { format!("{}@{}", s.user, s.host) };
        cmd.arg(target);
        let child = pty.slave.spawn_command(cmd).context("could not start ssh")?;
        drop(pty.slave);
        let reader = pty.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pty.master.take_writer()?));
        let ended = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        pump(reader, tx, ended.clone(), writer.clone(), false);
        Ok(Remote { received: rx, writer, link: Link::Pty { master: pty.master, child }, ended, telnet: false })
    }

    fn telnet(host: &str, port: u16) -> Result<Remote> {
        let addr = (host, port).to_socket_addrs()?.next().with_context(|| format!("{host} was not found"))?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(8))
            .with_context(|| format!("{host} did not accept a Telnet connection on port {port}"))?;
        stream.set_nodelay(true)?;
        let writer: Box<dyn Write + Send> = Box::new(stream.try_clone()?);
        let writer = Arc::new(Mutex::new(writer));
        let ended = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        pump(Box::new(stream.try_clone()?), tx, ended.clone(), writer.clone(), true);
        Ok(Remote { received: rx, writer, link: Link::Telnet { stream }, ended, telnet: true })
    }

    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut out = Vec::with_capacity(bytes.len());
        if self.telnet {
            for &b in bytes {
                match b {
                    0xff => out.extend_from_slice(&[0xff, 0xff]),
                    // NVT: a carriage return is followed by a line feed.
                    b'\r' => out.extend_from_slice(b"\r\n"),
                    _ => out.push(b),
                }
            }
        } else {
            out.extend_from_slice(bytes);
        }
        let mut w = self.writer.lock().map_err(|_| anyhow::anyhow!("session closed"))?;
        w.write_all(&out)?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        if let Link::Pty { master, .. } = &self.link {
            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    /// Has the other side closed the session (or ssh exited)?
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        match &mut self.link {
            Link::Pty { child, .. } => {
                let _ = child.kill();
            }
            Link::Telnet { stream } => {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
}

/// Asked by Windows' pseudo-terminal before it shows anything: "where is
/// the cursor?" (ESC [ 6 n). It waits for the answer.
pub const CURSOR_QUESTION: &[u8] = b"\x1b[6n";
pub const CURSOR_ANSWER: &[u8] = b"\x1b[1;1R";

/// Reads until the end, passing data on; Telnet option negotiation and the
/// terminal's cursor question are answered through `writer`.
fn pump(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<Vec<u8>>,
    ended: Arc<AtomicBool>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    telnet: bool,
) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 16384];
        let mut nvt = Nvt::default();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let data = if telnet {
                let (data, reply) = nvt.filter(&buf[..n]);
                if !reply.is_empty()
                    && let Ok(mut w) = writer.lock()
                {
                    let _ = w.write_all(&reply);
                }
                data
            } else {
                let data = &buf[..n];
                if data.windows(CURSOR_QUESTION.len()).any(|w| w == CURSOR_QUESTION)
                    && let Ok(mut w) = writer.lock()
                {
                    let _ = w.write_all(CURSOR_ANSWER);
                    let _ = w.flush();
                }
                data.to_vec()
            };
            if !data.is_empty() && tx.send(data).is_err() {
                break;
            }
        }
        ended.store(true, Ordering::Relaxed);
    });
}

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const ECHO: u8 = 1;
const SGA: u8 = 3;

/// Telnet's in-band commands, which may be split across reads.
#[derive(Default)]
struct Nvt {
    state: u8,
    verb: u8,
}

impl Nvt {
    /// The data without commands, and what to answer.
    fn filter(&mut self, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (mut data, mut reply) = (Vec::new(), Vec::new());
        for &b in input {
            match self.state {
                0 if b == IAC => self.state = 1,
                0 => data.push(b),
                1 => match b {
                    IAC => {
                        data.push(IAC);
                        self.state = 0;
                    }
                    WILL | WONT | DO | DONT => {
                        self.verb = b;
                        self.state = 2;
                    }
                    SB => self.state = 3,
                    _ => self.state = 0,
                },
                2 => {
                    // The server echoes and suppresses go-ahead: yes. We
                    // offer nothing ourselves.
                    match self.verb {
                        WILL if b == ECHO || b == SGA => reply.extend_from_slice(&[IAC, DO, b]),
                        WILL => reply.extend_from_slice(&[IAC, DONT, b]),
                        DO if b == SGA => reply.extend_from_slice(&[IAC, WILL, b]),
                        DO => reply.extend_from_slice(&[IAC, WONT, b]),
                        _ => {}
                    }
                    self.state = 0;
                }
                // Subnegotiation: skip to IAC SE.
                3 if b == IAC => self.state = 4,
                3 => {}
                4 if b == SE => self.state = 0,
                4 => self.state = 3,
                _ => self.state = 0,
            }
        }
        (data, reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        let s = Saved::parse(Protocol::Ssh, "admin@10.0.0.1:2222");
        assert_eq!((s.user.as_str(), s.host.as_str(), s.port), ("admin", "10.0.0.1", 2222));
        assert_eq!(s.target(), "admin@10.0.0.1:2222");
        let s = Saved::parse(Protocol::Telnet, "sw1");
        assert_eq!((s.user.as_str(), s.host.as_str(), s.port()), ("", "sw1", 23));
        let s = Saved::parse(Protocol::Ssh, "root@fe80::1");
        assert_eq!((s.host.as_str(), s.port), ("fe80::1", 0));
    }

    #[test]
    fn telnet_negotiation() {
        let mut n = Nvt::default();
        // WILL ECHO, DO TERMINAL-TYPE, "Hi", an escaped 0xff, a split DO.
        let (d, r) = n.filter(&[IAC, WILL, ECHO, IAC, DO, 24, b'H', b'i', IAC, IAC, IAC]);
        assert_eq!(d, [b'H', b'i', 0xff]);
        assert_eq!(r, [IAC, DO, ECHO, IAC, WONT, 24]);
        let (d, r) = n.filter(&[DO, SGA, IAC, SB, 24, 1, IAC, SE, b'!']);
        assert_eq!(d, [b'!']);
        assert_eq!(r, [IAC, WILL, SGA]);
    }

    #[test]
    fn telnet_end_to_end() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = server.accept().unwrap();
            c.write_all(&[IAC, WILL, ECHO]).unwrap();
            c.write_all(b"User Access Verification\r\n\r\nUsername: ").unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            while !got.ends_with(b"\r\n") {
                let n = c.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            got
        });
        let r = Remote::telnet("127.0.0.1", port).unwrap();
        let mut text = Vec::new();
        while !text.ends_with(b"Username: ") {
            text.extend(r.received.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        assert!(text.starts_with(b"User Access"));
        r.send(b"admin\r").unwrap();
        let got = t.join().unwrap();
        // Our answer to WILL ECHO, then the line.
        assert_eq!(got, [&[IAC, DO, ECHO][..], b"admin\r\n"].concat());
    }

    #[test]
    fn ssh_runs_in_a_terminal() {
        if !ssh_available() {
            return;
        }
        // Nothing listens on port 1: ssh says so and exits.
        let s = Saved { host: "127.0.0.1".into(), port: 1, ..Default::default() };
        let r = Remote::open(&s, 24, 80).unwrap();
        let mut text = Vec::new();
        while let Ok(d) = r.received.recv_timeout(Duration::from_secs(10)) {
            text.extend(d);
        }
        let text = String::from_utf8_lossy(&text);
        assert!(text.contains("port 1"), "{text}");
        std::thread::sleep(Duration::from_millis(200));
        assert!(r.ended());
    }
}
