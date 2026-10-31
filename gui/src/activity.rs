//! What others do with this computer: SSH sign-ins, files fetched or sent
//! through the servers, devices using a service. Shown in Servers →
//! Activity and, as chosen in Settings, as desktop notifications. Every
//! event is also written to a file (`activity.log` in the settings folder),
//! so the list is still there after the app is closed, until the user
//! deletes entries.

use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDateTime, TimeZone};

/// What happened, for the icon and the notification setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Someone signed in to this computer over SSH.
    Ssh,
    /// A file was fetched from or sent to one of the servers.
    File,
    /// A device used a service (syslog, traps, time, DHCP).
    Service,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub when: chrono::DateTime<chrono::Local>,
    pub kind: Kind,
    pub text: String,
}

/// The most events kept in memory.
const KEEP: usize = 20_000;
/// The most lines the file keeps (older ones are dropped when the app starts).
const FILE_KEEP: usize = 20_000;

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Ssh => "ssh",
            Kind::File => "file",
            Kind::Service => "service",
        }
    }

    fn from_word(w: &str) -> Option<Kind> {
        Some(match w {
            "ssh" => Kind::Ssh,
            "file" => Kind::File,
            "service" => Kind::Service,
            _ => return None,
        })
    }
}

impl Event {
    /// The line in the file: time, kind and text, separated by tabs.
    fn line(&self) -> String {
        format!(
            "{}\t{}\t{}",
            self.when.format("%Y-%m-%d %H:%M:%S"),
            self.kind.word(),
            // One line each: a device's line breaks would start made-up events.
            self.text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>()
        )
    }

    fn parse(line: &str) -> Option<Event> {
        let mut parts = line.splitn(3, '\t');
        let naive = NaiveDateTime::parse_from_str(parts.next()?, "%Y-%m-%d %H:%M:%S").ok()?;
        let when = Local.from_local_datetime(&naive).earliest()?;
        Some(Event { when, kind: Kind::from_word(parts.next()?)?, text: parts.next()?.to_string() })
    }
}

/// Which events a deletion (or a count) means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// The last `n` hours.
    LastHours(u32),
    Today,
    Yesterday,
    /// Everything older than `n` days.
    OlderThanDays(u32),
    Between(NaiveDateTime, NaiveDateTime),
    All,
}

impl Span {
    pub fn contains(self, when: chrono::DateTime<Local>, now: chrono::DateTime<Local>) -> bool {
        let day = |t: chrono::DateTime<Local>| t.date_naive();
        match self {
            Span::LastHours(n) => when >= now - chrono::Duration::hours(i64::from(n)),
            Span::Today => day(when) == day(now),
            Span::Yesterday => Some(day(when)) == day(now).pred_opt(),
            Span::OlderThanDays(n) => when < now - chrono::Duration::days(i64::from(n)),
            Span::Between(a, b) => when.naive_local() >= a && when.naive_local() <= b,
            Span::All => true,
        }
    }
}

#[derive(Default)]
struct Inner {
    events: VecDeque<Event>,
    /// Not yet looked at by the app (for notifications).
    pending: Vec<Event>,
    /// Events added since the Activity tab was last shown.
    unread: usize,
    /// Service events are noted once per device and service.
    seen: HashSet<(String, IpAddr)>,
    /// Where the events are kept (none: in memory only).
    file: Option<PathBuf>,
}

/// The activity log, shared with the servers' threads.
#[derive(Clone, Default)]
pub struct Activity(Arc<Mutex<Inner>>);

impl Activity {
    /// The activity kept in `path`: what earlier runs wrote is loaded, and new events are
    /// added to the file.
    pub fn open(path: &Path) -> Activity {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut events: VecDeque<Event> = text.lines().filter_map(Event::parse).collect();
        if text.lines().count() > FILE_KEEP {
            while events.len() > FILE_KEEP {
                events.pop_front();
            }
            let _ = write_file(path, &events);
        }
        while events.len() > KEEP {
            events.pop_front();
        }
        Activity(Arc::new(Mutex::new(Inner { events, file: Some(path.to_path_buf()), ..Inner::default() })))
    }

    pub fn add(&self, kind: Kind, text: String) {
        let Ok(mut a) = self.0.lock() else { return };
        let e = Event { when: Local::now(), kind, text };
        if let Some(path) = &a.file {
            let _ = append_file(path, &e);
        }
        a.pending.push(e.clone());
        if a.pending.len() > 100 {
            a.pending.remove(0);
        }
        a.events.push_back(e);
        while a.events.len() > KEEP {
            a.events.pop_front();
        }
        a.unread += 1;
    }

    /// A service event, only the first time `from` uses `service` (a
    /// device asking for the time every minute is noted once).
    pub fn first_use(&self, service: &str, from: IpAddr, text: String) {
        let new = self.0.lock().is_ok_and(|mut a| a.seen.insert((service.to_string(), from)));
        if new {
            self.add(Kind::Service, text);
        }
    }

    /// The newest events first.
    pub fn newest(&self, n: usize) -> Vec<Event> {
        self.0.lock().map(|a| a.events.iter().rev().take(n).cloned().collect()).unwrap_or_default()
    }

    pub fn unread(&self) -> usize {
        self.0.lock().map(|a| a.unread).unwrap_or(0)
    }

    pub fn mark_read(&self) {
        if let Ok(mut a) = self.0.lock() {
            a.unread = 0;
        }
    }

    /// How many events `span` covers.
    pub fn count(&self, span: Span) -> usize {
        let now = Local::now();
        self.0.lock().map(|a| a.events.iter().filter(|e| span.contains(e.when, now)).count()).unwrap_or(0)
    }

    /// Deletes the events `span` covers, from the list and from the file. Returns how many.
    pub fn delete(&self, span: Span) -> usize {
        let now = Local::now();
        let Ok(mut a) = self.0.lock() else { return 0 };
        let before = a.events.len();
        a.events.retain(|e| !span.contains(e.when, now));
        let removed = before - a.events.len();
        if matches!(span, Span::All) {
            a.unread = 0;
            a.seen.clear();
        }
        if removed > 0
            && let Some(path) = a.file.clone()
        {
            let _ = write_file(&path, &a.events);
        }
        removed
    }

    /// Every event, oldest first.
    pub fn all(&self) -> Vec<Event> {
        self.0.lock().map(|a| a.events.iter().cloned().collect()).unwrap_or_default()
    }

    /// Events the app has not handled yet (for notifications).
    pub fn take_pending(&self) -> Vec<Event> {
        self.0.lock().map(|mut a| std::mem::take(&mut a.pending)).unwrap_or_default()
    }
}

/// Adds one line to the file (readable by this user only: it holds the addresses of other devices).
fn append_file(path: &Path, e: &Event) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    writeln!(options.open(path)?, "{}", e.line())
}

fn write_file(path: &Path, events: &VecDeque<Event>) -> std::io::Result<()> {
    let text: String = events.iter().map(|e| format!("{}\n", e.line())).collect();
    netmgr::cmd::write_private(path, text)
}

/// `events` as a file the user keeps: CSV (time, kind, text) for a ".csv" name, else one
/// line per event.
pub fn export(path: &Path, events: &[Event]) -> std::io::Result<()> {
    let csv = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv"));
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    if csv {
        writeln!(out, "time,kind,text")?;
    }
    for e in events {
        let time = e.when.format("%Y-%m-%d %H:%M:%S");
        if csv {
            // A field with a comma, quote or the start of a formula is quoted, and cannot run in a spreadsheet.
            let text = e.text.replace('"', "\"\"");
            let quote = text.contains([',', '"']) || text.starts_with(['=', '+', '-', '@']);
            let text = if text.starts_with(['=', '+', '-', '@']) { format!("'{text}") } else { text };
            if quote {
                writeln!(out, "{time},{},\"{text}\"", e.kind.word())?;
            } else {
                writeln!(out, "{time},{},{text}", e.kind.word())?;
            }
        } else {
            writeln!(out, "{time}  {:8} {}", e.kind.word(), e.text)?;
        }
    }
    out.flush()
}

/// Watches for SSH sign-ins to this computer. Runs for as long as the app.
/// macOS and Linux: the sessions the system lists as signed in from another
/// machine (`who`); Windows: new connections to port 22 (the app sees every
/// connection there, as administrator).
pub fn watch_ssh(activity: Activity) {
    std::thread::spawn(move || {
        let mut known: Option<HashSet<(String, String)>> = None;
        loop {
            if let Some(now) = ssh_sessions() {
                // Sessions already open when the app started are not news.
                if let Some(known) = &known {
                    for (_, host) in now.difference(known) {
                        activity.add(Kind::Ssh, crate::i18n::trlf("SSH sign-in from {address}", &[("address", host)]));
                    }
                }
                known = Some(now);
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    });
}

/// The SSH sessions now: (what tells them apart, where from).
fn ssh_sessions() -> Option<HashSet<(String, String)>> {
    if cfg!(windows) {
        let list = netmgr::system::connections().ok()?;
        return Some(
            list.iter()
                .filter(|c| {
                    c.proto == "TCP"
                        && c.local_port == Some(netmgr::sshd::PORT)
                        && c.state.eq_ignore_ascii_case("ESTABLISHED")
                        && !c.remote.is_empty()
                })
                .filter_map(|c| {
                    let ip = remote_ip(&c.remote)?;
                    (!ip.is_loopback()).then(|| (c.remote.clone(), ip.to_string()))
                })
                .collect(),
        );
    }
    let text = netmgr::cmd::run("who", &[]).ok()?;
    Some(parse_who(&text))
}

/// `who`: "mbp  ttys004  Oct 12 13:05 (192.168.1.40)" — sessions from
/// another machine have its address in brackets (local ones have none, or
/// a display such as ":0").
fn parse_who(text: &str) -> HashSet<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let host = l.rsplit_once('(')?.1.trim_end().strip_suffix(')')?.trim();
            let tty = l.split_whitespace().nth(1)?;
            let local = host.is_empty() || host.starts_with(':') || host.starts_with("tmux") || host == "localhost";
            let loopback = host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
            (!local && !loopback).then(|| (format!("{tty} {l}"), host.to_string()))
        })
        .collect()
}

/// The address in "192.168.1.5:51234", "[fe80::1]:22" or "192.168.1.5.51234".
fn remote_ip(endpoint: &str) -> Option<IpAddr> {
    let e = endpoint.trim();
    if let Ok(sa) = e.parse::<std::net::SocketAddr>() {
        return Some(sa.ip());
    }
    // netstat on macOS: address and port joined by a dot.
    let (host, _) = e.rsplit_once(['.', ':'])?;
    host.trim_matches(['[', ']']).parse().ok()
}

/// Desktop notifications for new events, as chosen in Settings, a few at a
/// time: a burst becomes one notification.
#[derive(Default)]
pub struct Notifier {
    sent: VecDeque<Instant>,
    held: usize,
}

impl Notifier {
    pub fn handle(&mut self, events: Vec<Event>, settings: &crate::settings::Settings) {
        let wanted = |k: Kind| match k {
            Kind::Ssh => settings.notify_ssh,
            Kind::File => settings.notify_files,
            Kind::Service => settings.notify_services,
        };
        self.sent.retain(|t| t.elapsed() < Duration::from_secs(10));
        for e in events.into_iter().filter(|e| wanted(e.kind)) {
            // At most three every ten seconds; the rest is counted.
            if self.sent.len() >= 3 {
                self.held += 1;
                continue;
            }
            let title = match e.kind {
                Kind::Ssh => crate::i18n::trl("SSH sign-in"),
                Kind::File => crate::i18n::trl("File transfer"),
                Kind::Service => crate::i18n::trl("Service used"),
            };
            crate::notify::send(title, &e.text);
            self.sent.push_back(Instant::now());
        }
        if self.held > 0 && self.sent.len() < 3 {
            crate::notify::send(
                "Network Manager",
                &crate::i18n::trlf("{n} more events: see Servers → Activity", &[("n", &self.held)]),
            );
            self.sent.push_back(Instant::now());
            self.held = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        assert_eq!(remote_ip("192.168.1.5:51234"), Some("192.168.1.5".parse().unwrap()));
        assert_eq!(remote_ip("192.168.1.5.51234"), Some("192.168.1.5".parse().unwrap()));
        assert_eq!(remote_ip("[fe80::1]:22"), Some("fe80::1".parse().unwrap()));
    }

    #[test]
    fn signed_in_sessions() {
        let who = "mbp      console  Oct 12 09:00\nmbp      ttys004  Oct 12 13:05 (192.168.1.40)\nroot     pts/1        2026-10-12 13:06 (10.0.0.9)\nme       pts/2        2026-10-12 13:07 (:0)\nme       pts/3        2026-10-12 13:08 (127.0.0.1)";
        let hosts: Vec<String> = parse_who(who).into_iter().map(|(_, h)| h).collect();
        assert_eq!(hosts.len(), 2);
        assert!(hosts.contains(&"192.168.1.40".to_string()) && hosts.contains(&"10.0.0.9".to_string()));
    }

    fn dir() -> PathBuf {
        let d = netmgr::cmd::private_dir("activity-test").expect("a private folder");
        let _ = std::fs::remove_file(d.join("activity.log"));
        d
    }

    #[test]
    fn events_survive_closing_the_app() {
        let d = dir();
        let path = d.join("activity.log");
        let a = Activity::open(&path);
        a.add(Kind::Ssh, "SSH sign-in from 10.0.0.9".into());
        a.add(Kind::File, "10.0.0.2 downloads fw.bin\nwith a line break".into());
        drop(a);
        let b = Activity::open(&path);
        let all = b.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].kind, Kind::Ssh);
        assert_eq!(all[1].text, "10.0.0.2 downloads fw.bin with a line break");
        // Loading is not news.
        assert_eq!(b.unread(), 0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn deleting_a_span_removes_it_from_the_file_too() {
        let d = dir();
        let path = d.join("activity.log");
        let now = Local::now();
        let line =
            |t: chrono::DateTime<Local>, text: &str| format!("{}\tfile\t{text}\n", t.format("%Y-%m-%d %H:%M:%S"));
        let text = [
            line(now - chrono::Duration::days(40), "old"),
            line(now - chrono::Duration::days(3), "three days"),
            line(now - chrono::Duration::minutes(10), "recent"),
        ]
        .concat();
        std::fs::write(&path, text).expect("written");
        let a = Activity::open(&path);
        assert_eq!(a.count(Span::All), 3);
        assert_eq!(a.count(Span::LastHours(1)), 1);
        assert_eq!(a.count(Span::OlderThanDays(30)), 1);
        assert_eq!(a.delete(Span::OlderThanDays(30)), 1);
        assert_eq!(a.delete(Span::LastHours(1)), 1);
        // What is left is in the file after a restart.
        assert_eq!(Activity::open(&path).all().iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), ["three days"]);
        assert_eq!(a.delete(Span::All), 1);
        assert!(Activity::open(&path).all().is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn spans_between_two_times() {
        let now = Local::now();
        let t = |h: i64| now - chrono::Duration::hours(h);
        let (a, b) = (t(5).naive_local(), t(2).naive_local());
        assert!(Span::Between(a, b).contains(t(3), now));
        assert!(!Span::Between(a, b).contains(t(1), now));
        assert!(!Span::Between(a, b).contains(t(6), now));
    }

    #[test]
    fn an_export_cannot_run_in_a_spreadsheet() {
        let d = dir();
        let e = Event { when: Local::now(), kind: Kind::File, text: "=HYPERLINK(\"x\"),y".into() };
        let path = d.join("a.csv");
        export(&path, std::slice::from_ref(&e)).expect("saved");
        let csv = std::fs::read_to_string(&path).expect("read");
        assert!(csv.starts_with("time,kind,text\n") && csv.contains(",\"'=HYPERLINK(\"\"x\"\"),y\""), "{csv}");
        let txt = d.join("a.txt");
        export(&txt, &[e]).expect("saved");
        assert!(std::fs::read_to_string(&txt).expect("read").contains("file     =HYPERLINK"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn services_once_per_device() {
        let a = Activity::default();
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        a.first_use("ntp", ip, "x".into());
        a.first_use("ntp", ip, "x".into());
        a.first_use("syslog", ip, "y".into());
        assert_eq!(a.newest(10).len(), 2);
        assert_eq!(a.take_pending().len(), 2);
        assert!(a.take_pending().is_empty());
    }
}
