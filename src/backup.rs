//! Configuration backups: logs in to switches and routers over SSH, saves
//! their configuration with the date, keeps a new copy only when something
//! changed, and shows what changed between two copies.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::Serialize;

use crate::remote::Saved;

/// Commands that print the configuration: label, command.
pub const COMMANDS: [(&str, &str); 8] = [
    ("Cisco IOS / IOS-XE / NX-OS", "show running-config"),
    ("Arista EOS", "show running-config"),
    ("Aruba AOS-CX", "show running-config"),
    ("Juniper Junos", "show configuration | display set | no-more"),
    ("MikroTik RouterOS", "/export"),
    ("FortiGate", "show"),
    ("Ubiquiti EdgeOS / VyOS", "show configuration commands"),
    ("Linux", "cat /etc/network/interfaces"),
];

/// Lines that change without anyone changing the configuration; they are
/// left out when deciding whether a copy is new.
const VOLATILE: [&str; 7] = [
    "! Last configuration change at",
    "! NVRAM config last updated at",
    "Current configuration :",
    "ntp clock-period",
    "## Last commit:",
    "# by RouterOS",
    "#config-version=",
];

pub fn folder() -> PathBuf {
    crate::profiles::config_dir().join("backups")
}

/// A folder name from a session name.
fn folder_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    if s.is_empty() { "device".into() } else { s }
}

/// Days since 1970 to (year, month, day) — Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// "2026-10-09 14-03-27" in UTC: sorts in time order and is a valid file name.
pub fn stamp(t: SystemTime) -> String {
    let s = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (y, m, d) = civil(s.div_euclid(86_400));
    let r = s.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}-{:02}-{:02}", r / 3600, r / 60 % 60, r % 60)
}

/// Runs `cmd` in a pseudo-terminal, answers password prompts with
/// `password`, and returns everything it printed.
pub fn run_in_terminal(cmd: CommandBuilder, password: Option<&str>, timeout: Duration) -> Result<String> {
    let pty = native_pty_system().openpty(PtySize { rows: 50, cols: 250, pixel_width: 0, pixel_height: 0 })?;
    let mut child = pty.slave.spawn_command(cmd).context("could not start ssh")?;
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader()?;
    let mut writer = pty.master.take_writer()?;
    let (tx, rx) = channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let started = Instant::now();
    let mut out: Vec<u8> = Vec::new();
    let mut answered = 0;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                if chunk.windows(crate::remote::CURSOR_QUESTION.len()).any(|w| w == crate::remote::CURSOR_QUESTION) {
                    writer.write_all(crate::remote::CURSOR_ANSWER)?;
                    writer.flush()?;
                }
                out.extend(chunk);
                let tail = String::from_utf8_lossy(&out[out.len().saturating_sub(200)..]).to_lowercase();
                let tail = tail.trim_end();
                if tail.ends_with("password:") || tail.ends_with("passphrase:") || tail.ends_with("'s password:") {
                    let Some(p) = password else {
                        let _ = child.kill();
                        bail!("the device asks for a password: enter it, or use an SSH key");
                    };
                    answered += 1;
                    if answered > 1 {
                        let _ = child.kill();
                        bail!("the password was not accepted");
                    }
                    writer.write_all(format!("{p}\r").as_bytes())?;
                    writer.flush()?;
                    // Forget the prompt, so the next look at the end is fresh.
                    out.extend_from_slice(b"\n");
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Ok(Some(_)) = child.try_wait() {
            // Collect what is still buffered.
            while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                out.extend(chunk);
            }
            break;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            bail!("no complete answer within {} seconds", timeout.as_secs());
        }
    }
    let status = child.wait().ok();
    let text = String::from_utf8_lossy(&out).replace("\r\n", "\n").replace('\r', "");
    if status.is_some_and(|s| !s.success()) {
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ssh failed").trim();
        bail!("{last}");
    }
    Ok(text)
}

/// Removes terminal control sequences (ESC [ … letter, ESC ] … BEL).
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() || d == '~' {
                        break;
                    }
                }
            }
            Some(']') => {
                for d in chars.by_ref() {
                    if d == '\x07' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Removes what is not configuration: password prompts, ssh messages,
/// terminal control sequences.
pub fn clean(text: &str) -> String {
    let text = strip_escapes(text);
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            let t = l.trim().to_lowercase();
            !(t.ends_with("password:")
                || t.starts_with("warning: permanently added")
                || t.starts_with("connection to ") && t.ends_with(" closed."))
        })
        .collect();
    let start = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(lines.len());
    let end = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(start, |e| e + 1);
    let mut s = lines[start..end].join("\n");
    s.push('\n');
    s
}

/// The configuration without the lines that change by themselves.
fn comparable(text: &str) -> String {
    text.lines().filter(|l| !VOLATILE.iter().any(|v| l.trim_start().starts_with(v))).collect::<Vec<_>>().join("\n")
}

/// Asks `s` for its configuration over SSH.
pub fn fetch(s: &Saved, password: Option<&str>) -> Result<String> {
    if s.backup_command.trim().is_empty() {
        bail!("no backup command is set for {}", s.name);
    }
    if !crate::remote::ssh_available() {
        bail!("the ssh client is missing");
    }
    let mut cmd = CommandBuilder::new("ssh");
    cmd.env("TERM", "dumb");
    cmd.args(["-o", "StrictHostKeyChecking=accept-new", "-o", "ConnectTimeout=10", "-o", "NumberOfPasswordPrompts=1"]);
    if password.is_none() {
        cmd.args(["-o", "BatchMode=yes"]);
    }
    if s.port() != 22 {
        cmd.args(["-p", &s.port().to_string()]);
    }
    cmd.arg(if s.user.is_empty() { s.host.clone() } else { format!("{}@{}", s.user, s.host) });
    cmd.arg(s.backup_command.trim());
    let text = clean(&run_in_terminal(cmd, password, Duration::from_secs(90))?);
    if text.trim().len() < 20 {
        bail!("the device returned almost nothing: check the backup command");
    }
    Ok(text)
}

/// One saved copy.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Version {
    /// "2026-10-09 14-03-27" (UTC).
    pub stamp: String,
    pub path: PathBuf,
}

/// The copies of `name`, newest first.
pub fn versions(name: &str) -> Vec<Version> {
    let dir = folder().join(folder_name(name));
    let mut out: Vec<Version> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let stem = p.file_stem()?.to_str()?.to_string();
            (p.extension()? == "txt").then_some(Version { stamp: stem, path: p })
        })
        .collect();
    out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    out
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Outcome {
    /// A new copy was saved; lines added and removed since the last one.
    Changed {
        added: usize,
        removed: usize,
    },
    /// The first copy.
    First,
    Unchanged,
}

/// Saves `text` as the newest copy of `name` unless it is the same as the
/// last one.
pub fn store(name: &str, text: &str, now: SystemTime) -> Result<Outcome> {
    store_in(&folder(), name, text, now)
}

fn store_in(root: &Path, name: &str, text: &str, now: SystemTime) -> Result<Outcome> {
    let dir = root.join(folder_name(name));
    std::fs::create_dir_all(&dir)?;
    let mut list: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    list.sort();
    let outcome = match list.last() {
        None => Outcome::First,
        Some(last) => {
            let old = std::fs::read_to_string(last).unwrap_or_default();
            if comparable(&old) == comparable(text) {
                return Ok(Outcome::Unchanged);
            }
            let (added, removed) = count_changes(&old, text);
            Outcome::Changed { added, removed }
        }
    };
    std::fs::write(dir.join(format!("{}.txt", stamp(now))), text)?;
    Ok(outcome)
}

/// A line of a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Line {
    Same(String),
    Added(String),
    Removed(String),
    /// Unchanged lines left out between changes.
    Skipped(usize),
}

/// What changed from `old` to `new`, with `context` unchanged lines around
/// each change.
pub fn diff(old: &str, new: &str, context: usize) -> Vec<Line> {
    let d = similar::TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for group in d.grouped_ops(context) {
        if let Some(first) = group.first()
            && first.old_range().start > 0
            && out.is_empty()
        {
            out.push(Line::Skipped(first.old_range().start));
        } else if !out.is_empty() {
            out.push(Line::Skipped(0));
        }
        for op in group {
            for change in d.iter_changes(&op) {
                let text = change.value().trim_end_matches('\n').to_string();
                out.push(match change.tag() {
                    similar::ChangeTag::Equal => Line::Same(text),
                    similar::ChangeTag::Insert => Line::Added(text),
                    similar::ChangeTag::Delete => Line::Removed(text),
                });
            }
        }
    }
    out
}

fn count_changes(old: &str, new: &str) -> (usize, usize) {
    let d = similar::TextDiff::from_lines(old, new);
    let (mut a, mut r) = (0, 0);
    for c in d.iter_all_changes() {
        match c.tag() {
            similar::ChangeTag::Insert => a += 1,
            similar::ChangeTag::Delete => r += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (a, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps() {
        assert_eq!(stamp(UNIX_EPOCH), "1970-01-01 00-00-00");
        assert_eq!(stamp(UNIX_EPOCH + Duration::from_secs(1_760_018_607)), "2025-10-09 14-03-27");
        assert_eq!(stamp(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29 00-00-00");
    }

    #[test]
    fn keeps_only_changes() {
        let root = std::env::temp_dir().join(format!("netmgr-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let t = |s: u64| UNIX_EPOCH + Duration::from_secs(1_760_000_000 + s);
        let v1 = "! Last configuration change at 10:00\nhostname sw1\ninterface Gi1/0/1\n description Uplink\n";
        let v1b = "! Last configuration change at 11:30\nhostname sw1\ninterface Gi1/0/1\n description Uplink\n";
        let v2 = "! Last configuration change at 12:00\nhostname sw1\ninterface Gi1/0/1\n description Uplink to core\n";
        assert_eq!(store_in(&root, "core sw/1", v1, t(0)).unwrap(), Outcome::First);
        assert_eq!(store_in(&root, "core sw/1", v1b, t(60)).unwrap(), Outcome::Unchanged);
        assert_eq!(store_in(&root, "core sw/1", v2, t(120)).unwrap(), Outcome::Changed { added: 2, removed: 2 });
        let files = std::fs::read_dir(root.join("core_sw_1")).unwrap().count();
        assert_eq!(files, 2);
        let d = diff(v1, v2, 1);
        assert!(d.contains(&Line::Removed(" description Uplink".into())));
        assert!(d.contains(&Line::Added(" description Uplink to core".into())));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleans_output() {
        let raw = "\x1b[6n\x1b]0;ssh\x07admin@10.0.0.2's password: \n\nhostname \x1b[1msw1\x1b[0m\n!\nend\nConnection to 10.0.0.2 closed.\n";
        assert_eq!(clean(raw), "hostname sw1\n!\nend\n");
    }

    #[test]
    #[cfg(unix)]
    fn answers_the_password_prompt() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "printf 'Password: '; read p; echo \"got $p\"; echo 'hostname sw1'"]);
        let out = run_in_terminal(cmd, Some("s3cret"), Duration::from_secs(10)).unwrap();
        assert!(out.contains("got s3cret") && out.contains("hostname sw1"), "{out}");
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", "printf 'Password: '; read p"]);
        assert!(run_in_terminal(cmd, None, Duration::from_secs(10)).is_err());
    }
}
