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
    // Never `.` or `..`, which are not folders of their own.
    let s = s.trim_start_matches('.');
    if s.is_empty() { "device".into() } else { s.to_string() }
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
                let mut param = String::new();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() || d == '~' {
                        // Windows' terminal writes runs of spaces as "move
                        // right n" (ESC [ n C): put the spaces back.
                        if d == 'C' {
                            let n = param.parse::<usize>().unwrap_or(1).min(512);
                            out.extend(std::iter::repeat_n(' ', n));
                        }
                        break;
                    }
                    param.push(d);
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
    if s.protocol == crate::remote::Protocol::Telnet {
        bail!("{} uses Telnet: backups work over SSH only", s.name);
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
    cmd.arg("--");
    cmd.arg(if s.user.is_empty() { s.host.clone() } else { format!("{}@{}", s.user, s.host) });
    cmd.arg(s.backup_command.trim());
    let text = clean(&run_in_terminal(cmd, password, Duration::from_secs(90))?);
    if text.trim().len() < 20 {
        bail!("the device returned almost nothing: check the backup command");
    }
    Ok(text)
}

/// What pushing commands to one device did.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Pushed {
    /// What the device printed, from its first prompt to the last command.
    pub transcript: String,
    /// The commands the device rejected, with what it said.
    pub errors: Vec<(String, String)>,
    /// The lines typed (fewer than given when it stopped at an error).
    pub sent: usize,
}

/// `template` with `{name}` and `{host}` replaced by the session's.
pub fn fill(template: &str, s: &Saved) -> String {
    template.replace("{name}", &s.name).replace("{host}", &s.host)
}

/// The lines of `commands` that are typed: blank lines are left out.
pub fn command_lines(commands: &str) -> Vec<String> {
    commands.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).map(String::from).collect()
}

/// Logs in to `s` over SSH and types `commands` line by line, waiting for
/// the device's prompt after each, as a person would.
pub fn push(s: &Saved, password: Option<&str>, commands: &str, stop_on_error: bool) -> Result<Pushed> {
    if s.protocol == crate::remote::Protocol::Telnet {
        bail!("{} uses Telnet: commands are sent over SSH only", s.name);
    }
    if !crate::remote::ssh_available() {
        bail!("the ssh client is missing");
    }
    let mut cmd = CommandBuilder::new("ssh");
    cmd.env("TERM", "dumb");
    cmd.args(["-tt", "-o", "StrictHostKeyChecking=accept-new", "-o", "ConnectTimeout=10"]);
    if password.is_none() {
        cmd.args(["-o", "BatchMode=yes"]);
    }
    if s.port() != 22 {
        cmd.args(["-p", &s.port().to_string()]);
    }
    cmd.arg("--");
    cmd.arg(if s.user.is_empty() { s.host.clone() } else { format!("{}@{}", s.user, s.host) });
    type_lines(cmd, password, &command_lines(&fill(commands, s)), stop_on_error)
}

/// What devices answer to a command they reject.
const REJECTED: [&str; 14] = [
    "% invalid",
    "% incomplete",
    "% ambiguous",
    "% unknown",
    "% unrecognized",
    "% bad",
    "% error",
    "invalid input",
    "syntax error",
    "unknown command",
    "error:",
    "failure:",
    "command not found",
    "bad command",
];

/// The device's answer to a command, when it is a rejection.
fn rejection(answer: &str) -> Option<String> {
    answer
        .lines()
        .map(str::trim)
        .find(|l| {
            let l = l.to_lowercase();
            REJECTED.iter().any(|r| l.contains(r))
        })
        .map(String::from)
}

/// The last line ends like a prompt: "sw1#", "sw1(config)#", "user@r1> ",
/// "[admin@MikroTik] > ", "$ ".
fn at_prompt(text: &str) -> bool {
    let last = text.rsplit('\n').next().unwrap_or("").trim_end();
    last.len() < 120 && last.ends_with(['#', '>', '$', '%', ']'])
}

/// An interactive session in a pseudo-terminal.
struct Typist {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    out: Vec<u8>,
    password: Option<String>,
    answered: usize,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Typist {
    fn write(&mut self, s: &str) -> Result<()> {
        self.writer.write_all(s.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// The output from `from` on, without terminal codes.
    fn text(&self, from: usize) -> String {
        strip_escapes(&String::from_utf8_lossy(&self.out[from.min(self.out.len())..]))
            .replace("\r\n", "\n")
            .replace('\r', "")
    }

    /// Reads until the device shows a prompt and then stays quiet, paging
    /// through "--More--" and answering password and confirm questions.
    /// False when the session ended.
    fn wait_prompt(&mut self, from: usize, timeout: Duration) -> Result<bool> {
        let started = Instant::now();
        let mut quiet_since = Instant::now();
        loop {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => {
                    if chunk.windows(crate::remote::CURSOR_QUESTION.len()).any(|w| w == crate::remote::CURSOR_QUESTION)
                    {
                        self.writer.write_all(crate::remote::CURSOR_ANSWER)?;
                        self.writer.flush()?;
                    }
                    self.out.extend(chunk);
                    quiet_since = Instant::now();
                    let text = self.text(from.max(self.out.len().saturating_sub(300))).to_lowercase();
                    let tail = text.trim_end();
                    if tail.ends_with("password:") || tail.ends_with("passphrase:") {
                        let Some(p) = self.password.clone() else {
                            bail!("the device asks for a password: enter it, or use an SSH key");
                        };
                        self.answered += 1;
                        if self.answered > 3 {
                            bail!("the password was not accepted");
                        }
                        self.write(&format!("{p}\r"))?;
                        self.out.extend_from_slice(b"\n");
                    } else if tail.ends_with("--more--")
                        || tail.ends_with("-- more --")
                        || tail.ends_with("<--- more --->")
                    {
                        self.write(" ")?;
                    } else if tail.ends_with("[confirm]") || tail.ends_with("]?") {
                        // "Destination filename [startup-config]?": the default.
                        self.write("\r")?;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(false),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Ok(Some(_)) = self.child.try_wait() {
                while let Ok(chunk) = self.rx.recv_timeout(Duration::from_millis(200)) {
                    self.out.extend(chunk);
                }
                return Ok(false);
            }
            if quiet_since.elapsed() > Duration::from_millis(500) && at_prompt(&self.text(from)) {
                return Ok(true);
            }
            if started.elapsed() > timeout {
                bail!("the device did not answer within {} seconds", timeout.as_secs());
            }
        }
    }
}

impl Drop for Typist {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Starts `cmd` in a pseudo-terminal, waits for the first prompt and types
/// `lines`, each when the prompt is back.
pub fn type_lines(
    cmd: CommandBuilder,
    password: Option<&str>,
    lines: &[String],
    stop_on_error: bool,
) -> Result<Pushed> {
    let pty = native_pty_system().openpty(PtySize { rows: 50, cols: 250, pixel_width: 0, pixel_height: 0 })?;
    let child = pty.slave.spawn_command(cmd).context("could not start ssh")?;
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader()?;
    let writer = pty.master.take_writer()?;
    let (tx, rx) = channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut t = Typist {
        child,
        writer,
        rx,
        out: Vec::new(),
        password: password.map(String::from),
        answered: 0,
        _master: pty.master,
    };
    if !t.wait_prompt(0, Duration::from_secs(45))? {
        let text = t.text(0);
        let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("the session ended").trim();
        bail!("{last}");
    }
    let start = t.out.len();
    let mut result = Pushed::default();
    for line in lines {
        let from = t.out.len();
        t.write(&format!("{line}\r"))?;
        result.sent += 1;
        let alive = t.wait_prompt(from, Duration::from_secs(90))?;
        // The answer, without the echoed command.
        let answer = t.text(from);
        let answer = answer.split_once('\n').map_or("", |(_, rest)| rest);
        if let Some(e) = rejection(answer) {
            result.errors.push((line.clone(), e));
            if stop_on_error {
                break;
            }
        }
        if !alive {
            break;
        }
    }
    result.transcript = t.text(start).trim_start_matches('\n').to_string();
    Ok(result)
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
    crate::cmd::write_private(&dir.join(format!("{}.txt", stamp(now))), text)?;
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
    fn folder_names_stay_inside() {
        assert_eq!(folder_name(".."), "device");
        assert_eq!(folder_name("../../etc"), "_.._etc");
        assert_eq!(folder_name("sw1.lab"), "sw1.lab");
    }

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
        // Windows' terminal: spaces sent as cursor moves.
        assert_eq!(
            clean("interfaces {\n\x1b[4Cge-0/0/0 {\n\x1b[8Xunit 0;\n"),
            "interfaces {\n    ge-0/0/0 {\nunit 0;\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn types_commands_at_the_prompt() {
        let device = "printf 'Password: '; read p; [ \"$p\" = s3cret ] || exit 1; \
            while printf 'sw1(config)# '; read l; do case \"$l\" in bad*) echo '% Invalid input detected at marker.';; \
            *) echo \"ok $l\";; esac; done";
        let sh = || {
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.args(["-c", device]);
            cmd
        };
        let lines: Vec<String> = ["hostname sw1", "bad command", "end"].map(String::from).to_vec();
        let r = type_lines(sh(), Some("s3cret"), &lines, false).unwrap();
        assert_eq!(r.sent, 3);
        assert_eq!(r.errors, vec![("bad command".to_string(), "% Invalid input detected at marker.".to_string())]);
        assert!(r.transcript.contains("ok hostname sw1") && r.transcript.contains("ok end"), "{}", r.transcript);
        let r = type_lines(sh(), Some("s3cret"), &lines, true).unwrap();
        assert_eq!((r.sent, r.errors.len()), (2, 1));
        assert!(type_lines(sh(), Some("wrong"), &lines, false).is_err());
    }

    #[test]
    fn prompts_and_rejections() {
        assert!(at_prompt("banner\nsw1#"));
        assert!(at_prompt("x\nadmin@r1> "));
        assert!(at_prompt("[admin@MikroTik] > "));
        assert!(!at_prompt("Building configuration..."));
        assert_eq!(
            rejection("     ^\n% Invalid input detected at '^' marker.").as_deref(),
            Some("% Invalid input detected at '^' marker.")
        );
        assert_eq!(rejection("ok"), None);
        let s = Saved { name: "sw1".into(), host: "10.0.0.2".into(), ..Default::default() };
        assert_eq!(fill("hostname {name}\n! {host}", &s), "hostname sw1\n! 10.0.0.2");
        assert_eq!(command_lines("conf t\n\n  \nend\n"), vec!["conf t", "end"]);
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
