//! Traffic per program: bytes each program has sent and received, read
//! from the system (macOS: `nettop`; Linux: `ss`; Windows: TCP statistics).
//! Rates come from comparing two readings.

use std::collections::HashMap;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

/// Bytes counted for one program (macOS) or one connection (Linux,
/// Windows) since it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counter {
    /// What stays the same between readings: the program and its process ID,
    /// or the connection.
    pub key: String,
    pub program: String,
    pub pid: Option<u32>,
    pub received: u64,
    pub sent: u64,
}

/// What each program used between two readings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub program: String,
    pub pids: Vec<u32>,
    pub received: u64,
    pub sent: u64,
    /// Connections (Linux, Windows) or 0.
    pub connections: usize,
}

/// The counters now.
pub fn read() -> Result<Vec<Counter>> {
    imp::read()
}

/// Calls `sample` with fresh counters every `every` until `stop` is set or
/// `sample` returns false.
pub fn watch(every: Duration, stop: &AtomicBool, mut sample: impl FnMut(Vec<Counter>) -> bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        // One nettop reading takes 5 seconds; a running nettop prints one
        // every `every`.
        use std::io::BufRead;
        let secs = every.as_secs().max(1).to_string();
        // In a pseudo-terminal nettop prints each reading at once; into a
        // pipe it holds them back.
        let pty = portable_pty::native_pty_system().openpty(portable_pty::PtySize {
            rows: 50,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = portable_pty::CommandBuilder::new("nettop");
        cmd.args(["-P", "-L", "0", "-s", &secs, "-x", "-J", "bytes_in,bytes_out"]);
        let mut child = pty.slave.spawn_command(cmd)?;
        drop(pty.slave);
        let out = pty.master.try_clone_reader()?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line.trim_end_matches('\r').to_string()).is_err() {
                    break;
                }
            }
        });
        let mut block = String::new();
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    // Every reading starts with the header line.
                    if line.starts_with(",bytes_in") && !block.is_empty() {
                        if !sample(parse_nettop(&block)) {
                            break Ok(());
                        }
                        block.clear();
                    }
                    block += &line;
                    block.push('\n');
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break Err(anyhow::anyhow!("nettop stopped")),
            }
        };
        let _ = child.kill();
        let _ = child.wait();
        drop(pty.master);
        result
    }
    #[cfg(not(target_os = "macos"))]
    {
        while !stop.load(Ordering::Relaxed) {
            if !sample(read()?) {
                break;
            }
            let until = std::time::Instant::now() + every;
            while std::time::Instant::now() < until && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    }
}

/// Shared between [`watch`] in the background and a reader.
pub type Latest = Arc<std::sync::Mutex<Vec<Counter>>>;

/// What changed from `before` to `now`, by program, busiest first.
/// Counters that are new are counted from zero only when `count_new`
/// (connections opened between readings); otherwise they start here.
pub fn compare(before: &[Counter], now: &[Counter], count_new: bool) -> Vec<Usage> {
    let old: HashMap<&str, &Counter> = before.iter().map(|c| (c.key.as_str(), c)).collect();
    let mut by: HashMap<String, Usage> = HashMap::new();
    for c in now {
        let (rx, tx) = match old.get(c.key.as_str()) {
            Some(o) => (c.received.saturating_sub(o.received), c.sent.saturating_sub(o.sent)),
            None if count_new => (c.received, c.sent),
            None => (0, 0),
        };
        let u =
            by.entry(c.program.clone()).or_insert_with(|| Usage { program: c.program.clone(), ..Default::default() });
        u.received += rx;
        u.sent += tx;
        u.connections += 1;
        if let Some(pid) = c.pid
            && !u.pids.contains(&pid)
        {
            u.pids.push(pid);
        }
    }
    let mut out: Vec<Usage> = by.into_values().collect();
    out.sort_by(|a, b| (b.received + b.sent).cmp(&(a.received + a.sent)).then(a.program.cmp(&b.program)));
    out
}

/// Are new counters connections that opened since the last reading (and so
/// count fully)? On macOS a new key is a program seen for the first time,
/// whose total is mostly from before.
pub const NEW_COUNTERS_ARE_NEW: bool = !cfg!(target_os = "macos");

/// Parses `nettop -P -L 1 -x -J bytes_in,bytes_out`.
pub fn parse_nettop(text: &str) -> Vec<Counter> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 3 {
            continue;
        }
        let (Ok(rx), Ok(tx)) = (f[1].trim().parse::<u64>(), f[2].trim().parse::<u64>()) else { continue };
        let (name, pid) = match f[0].rsplit_once('.') {
            Some((n, p)) if p.parse::<u32>().is_ok() => (n.to_string(), p.parse().ok()),
            _ => (f[0].to_string(), None),
        };
        out.push(Counter { key: f[0].to_string(), program: name, pid, received: rx, sent: tx });
    }
    out
}

/// Parses `ss -tinpH` (and `-uinpH`): a line per socket, then its details.
pub fn parse_ss(text: &str) -> Vec<Counter> {
    let mut out: Vec<Counter> = Vec::new();
    let mut current: Option<Counter> = None;
    for line in text.lines() {
        if !line.starts_with([' ', '\t']) {
            if let Some(c) = current.take() {
                out.push(c);
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 5 {
                continue;
            }
            let (program, pid) = match line.split_once("users:((\"") {
                Some((_, rest)) => {
                    let name = rest.split('"').next().unwrap_or("").to_string();
                    let pid = rest.split_once("pid=").and_then(|(_, p)| p.split([',', ')']).next()?.parse().ok());
                    (name, pid)
                }
                None => ("?".to_string(), None),
            };
            current = Some(Counter { key: format!("{} {}", f[3], f[4]), program, pid, received: 0, sent: 0 });
        } else if let Some(c) = &mut current {
            for w in line.split_whitespace() {
                if let Some(v) = w.strip_prefix("bytes_received:") {
                    c.received = v.parse().unwrap_or(0);
                } else if let Some(v) = w.strip_prefix("bytes_sent:") {
                    c.sent = v.parse().unwrap_or(0);
                }
            }
        }
    }
    if let Some(c) = current {
        out.push(c);
    }
    out
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub fn read() -> Result<Vec<Counter>> {
        let text = crate::cmd::run("nettop", &["-P", "-L", "1", "-x", "-J", "bytes_in,bytes_out"])?;
        Ok(parse_nettop(&text))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use anyhow::bail;

    pub fn read() -> Result<Vec<Counter>> {
        if !crate::cmd::exists("ss") {
            bail!("the ss command (iproute2) is needed");
        }
        let text = crate::cmd::run("ss", &["-tinpH"])?;
        Ok(parse_ss(&text))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::bail;
    use std::net::Ipv4Addr;
    use windows_sys::Win32::Foundation::{CloseHandle, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetPerTcpConnectionEStats, MIB_TCP_STATE_ESTAB, MIB_TCPROW_LH, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, SetPerTcpConnectionEStats, TCP_ESTATS_DATA_ROD_v0, TCP_ESTATS_DATA_RW_v0,
        TCP_TABLE_OWNER_PID_ALL, TcpConnectionEstatsData,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    fn program(pid: u32, cache: &mut HashMap<u32, String>) -> String {
        if pid == 0 || pid == 4 {
            return "System".into();
        }
        cache
            .entry(pid)
            .or_insert_with(|| {
                // SAFETY: the handle is checked and closed; the buffer size is passed.
                unsafe {
                    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                    if h.is_null() {
                        return format!("PID {pid}");
                    }
                    let mut buf = [0u16; 1024];
                    let mut len = buf.len() as u32;
                    let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0;
                    CloseHandle(h);
                    if !ok {
                        return format!("PID {pid}");
                    }
                    let path = String::from_utf16_lossy(&buf[..len as usize]);
                    path.rsplit('\\').next().unwrap_or(&path).trim_end_matches(".exe").to_string()
                }
            })
            .clone()
    }

    pub fn read() -> Result<Vec<Counter>> {
        if !crate::cmd::is_admin() {
            bail!("traffic per program needs administrator rights");
        }
        // SAFETY: the table is read with the size Windows asks for; each row
        // is passed to the statistics calls with correctly sized buffers.
        unsafe {
            let mut size = 0u32;
            GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_ALL, 0);
            let mut buf = vec![0u8; size as usize + 4096];
            size = buf.len() as u32;
            let r =
                GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_ALL, 0);
            if r != NO_ERROR {
                bail!("could not read the connection table (error {r})");
            }
            let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
            let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
            let mut names = HashMap::new();
            let mut out = Vec::new();
            for r in rows.iter().filter(|r| r.dwState == MIB_TCP_STATE_ESTAB as u32) {
                let row = row_lh(r);
                let rw = TCP_ESTATS_DATA_RW_v0 { EnableCollection: true };
                SetPerTcpConnectionEStats(
                    &row,
                    TcpConnectionEstatsData,
                    &rw as *const _ as *const u8,
                    0,
                    std::mem::size_of::<TCP_ESTATS_DATA_RW_v0>() as u32,
                    0,
                );
                let mut rod: TCP_ESTATS_DATA_ROD_v0 = std::mem::zeroed();
                let ok = GetPerTcpConnectionEStats(
                    &row,
                    TcpConnectionEstatsData,
                    std::ptr::null_mut(),
                    0,
                    0,
                    std::ptr::null_mut(),
                    0,
                    0,
                    &mut rod as *mut _ as *mut u8,
                    0,
                    std::mem::size_of::<TCP_ESTATS_DATA_ROD_v0>() as u32,
                ) == NO_ERROR;
                if !ok {
                    continue;
                }
                let ep = |addr: u32, port: u32| {
                    format!("{}:{}", Ipv4Addr::from(u32::from_be(addr)), u16::from_be(port as u16))
                };
                out.push(Counter {
                    key: format!("{} {}", ep(r.dwLocalAddr, r.dwLocalPort), ep(r.dwRemoteAddr, r.dwRemotePort)),
                    program: program(r.dwOwningPid, &mut names),
                    pid: Some(r.dwOwningPid),
                    received: rod.DataBytesIn,
                    sent: rod.DataBytesOut,
                });
            }
            Ok(out)
        }
    }

    fn row_lh(r: &MIB_TCPROW_OWNER_PID) -> MIB_TCPROW_LH {
        // SAFETY: plain data; every field is set below.
        let mut row: MIB_TCPROW_LH = unsafe { std::mem::zeroed() };
        row.Anonymous.dwState = r.dwState;
        row.dwLocalAddr = r.dwLocalAddr;
        row.dwLocalPort = r.dwLocalPort;
        row.dwRemoteAddr = r.dwRemoteAddr;
        row.dwRemotePort = r.dwRemotePort;
        row
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::*;
    use anyhow::bail;
    pub fn read() -> Result<Vec<Counter>> {
        bail!("not supported on this system")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nettop() {
        let c = parse_nettop(",bytes_in,bytes_out,\nGoogle Chrome H.812,1000,200,\nmDNSResponder.648,5,6,\nbad line\n");
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].program.as_str(), c[0].pid, c[0].received, c[0].sent),
            ("Google Chrome H", Some(812), 1000, 200)
        );
    }

    #[test]
    fn ss() {
        let text = "ESTAB 0 0 192.168.1.5:43210 1.2.3.4:443 users:((\"firefox\",pid=1234,fd=55))\n\t cubic wscale:7,7 bytes_sent:1500 bytes_acked:1500 bytes_received:90000 segs_out:10\nESTAB 0 0 192.168.1.5:22 192.168.1.9:50000\n\t bytes_received:10\n";
        let c = parse_ss(text);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].program.as_str(), c[0].pid, c[0].received, c[0].sent), ("firefox", Some(1234), 90000, 1500));
        assert_eq!(c[0].key, "192.168.1.5:43210 1.2.3.4:443");
        assert_eq!((c[1].program.as_str(), c[1].received), ("?", 10));
    }

    #[test]
    fn compares() {
        let c = |key: &str, prog: &str, rx, tx| Counter {
            key: key.into(),
            program: prog.into(),
            pid: Some(1),
            received: rx,
            sent: tx,
        };
        let before = [c("a", "firefox", 100, 10), c("b", "ssh", 5, 5)];
        let now = [c("a", "firefox", 1100, 60), c("c", "firefox", 300, 30), c("b", "ssh", 5, 5)];
        let u = compare(&before, &now, true);
        assert_eq!((u[0].program.as_str(), u[0].received, u[0].sent, u[0].connections), ("firefox", 1300, 80, 2));
        assert_eq!((u[1].received, u[1].sent), (0, 0));
        let u = compare(&before, &now, false);
        assert_eq!(u[0].received, 1000);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn reads_this_computer() {
        assert!(!read().unwrap().is_empty());
        let stop = AtomicBool::new(false);
        let mut readings = 0;
        watch(Duration::from_secs(1), &stop, |c| {
            assert!(!c.is_empty());
            readings += 1;
            readings < 2
        })
        .unwrap();
        assert_eq!(readings, 2);
    }
}
