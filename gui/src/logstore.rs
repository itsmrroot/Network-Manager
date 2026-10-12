//! The servers' logs, kept in files: one per day in a folder of the user's
//! choice (Servers → Logs), so that they survive closing the app. When the
//! user turns that off, this session's lines stay in memory and the app
//! offers to save them when it closes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime};

/// The servers, as written in the files (the same in every language).
pub const SERVERS: [&str; 7] = ["TFTP", "HTTP", "Syslog", "SNMP traps", "DHCP", "NTP", "Throughput"];

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub when: NaiveDateTime,
    pub server: String,
    pub text: String,
}

impl Record {
    fn line(&self) -> String {
        format!("{}\t{}\t{}", self.when.format("%Y-%m-%d %H:%M:%S"), self.server, self.text)
    }

    fn parse(line: &str) -> Option<Record> {
        let mut parts = line.splitn(3, '\t');
        let when = NaiveDateTime::parse_from_str(parts.next()?, "%Y-%m-%d %H:%M:%S").ok()?;
        Some(Record { when, server: parts.next()?.to_string(), text: parts.next().unwrap_or("").to_string() })
    }
}

/// The most lines kept in memory when logs are not kept in files.
const IN_MEMORY: usize = 100_000;

struct Store {
    keep: bool,
    dir: PathBuf,
    /// Lines of this session not in a file (when not kept).
    unsaved: Vec<Record>,
    /// Today's file, open.
    file: Option<(NaiveDate, std::fs::File)>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);
/// Lines recorded so far (the Logs tab reloads when it changes).
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// `Documents/Network Manager/Logs`.
pub fn default_dir() -> PathBuf {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
    home.join("Documents").join("Network Manager").join("Logs")
}

/// Follows the settings (called every frame; cheap when nothing changed).
pub fn configure(keep: bool, dir: &Path) {
    let Ok(mut s) = STORE.lock() else { return };
    match s.as_mut() {
        Some(st) if st.keep == keep && st.dir == dir => {}
        Some(st) => {
            st.keep = keep;
            st.dir = dir.to_path_buf();
            st.file = None;
        }
        None => *s = Some(Store { keep, dir: dir.to_path_buf(), unsaved: Vec::new(), file: None }),
    }
}

/// One line of `server` (from the servers' threads).
pub fn record(server: &str, text: &str) {
    let r = Record {
        when: Local::now().naive_local(),
        server: server.to_string(),
        // One line each: a device's line breaks would start made-up records.
        text: text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect(),
    };
    let Ok(mut s) = STORE.lock() else { return };
    let Some(st) = s.as_mut() else { return };
    WRITTEN.fetch_add(1, Ordering::Relaxed);
    if !st.keep {
        st.unsaved.push(r);
        if st.unsaved.len() > IN_MEMORY {
            st.unsaved.drain(..st.unsaved.len() - IN_MEMORY);
        }
        return;
    }
    let day = r.when.date();
    if st.file.as_ref().is_none_or(|(d, _)| *d != day) {
        let _ = std::fs::create_dir_all(&st.dir);
        let path = st.dir.join(file_name(day));
        st.file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(|f| (day, f));
    }
    if let Some((_, f)) = st.file.as_mut() {
        let _ = writeln!(f, "{}", r.line());
    }
}

/// Lines recorded so far.
pub fn written() -> u64 {
    WRITTEN.load(Ordering::Relaxed)
}

/// This session's lines that are in no file (logs not kept).
pub fn unsaved() -> usize {
    STORE.lock().ok().and_then(|s| s.as_ref().map(|st| st.unsaved.len())).unwrap_or(0)
}

/// Forgets this session's unsaved lines.
pub fn discard_unsaved() {
    if let Ok(mut s) = STORE.lock()
        && let Some(st) = s.as_mut()
    {
        st.unsaved.clear();
    }
}

/// This session's unsaved lines, saved to `path`.
pub fn save_unsaved(path: &Path) -> std::io::Result<()> {
    let lines = STORE.lock().ok().and_then(|s| s.as_ref().map(|st| st.unsaved.clone())).unwrap_or_default();
    write(path, &lines)?;
    discard_unsaved();
    Ok(())
}

/// `records` to `path`, one per line.
pub fn write(path: &Path, records: &[Record]) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for r in records {
        writeln!(f, "{}", r.line())?;
    }
    f.flush()
}

fn file_name(day: NaiveDate) -> String {
    format!("servers-{}.log", day.format("%Y-%m-%d"))
}

/// The days that have a log file in `dir`, newest first.
pub fn days(dir: &Path) -> Vec<NaiveDate> {
    let mut out: Vec<NaiveDate> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            NaiveDate::parse_from_str(name.strip_prefix("servers-")?.strip_suffix(".log")?, "%Y-%m-%d").ok()
        })
        .collect();
    out.sort_unstable_by(|a, b| b.cmp(a));
    out
}

/// Which days a search covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    LastMonth,
    Day(NaiveDate),
    All,
}

impl Period {
    pub fn contains(self, day: NaiveDate, today: NaiveDate) -> bool {
        let first_of = |d: NaiveDate| d.with_day(1).unwrap_or(d);
        match self {
            Period::Today => day == today,
            Period::Yesterday => Some(day) == today.pred_opt(),
            Period::ThisWeek => {
                let monday = today - chrono::Days::new(u64::from(today.weekday().num_days_from_monday()));
                day >= monday && day <= today
            }
            Period::ThisMonth => first_of(day) == first_of(today),
            Period::LastMonth => {
                let last = first_of(today).pred_opt().map(first_of);
                Some(first_of(day)) == last
            }
            Period::Day(d) => day == d,
            Period::All => true,
        }
    }
}

/// What the Logs tab shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filter {
    /// None: every server.
    pub server: Option<String>,
    pub period: Period,
    pub text: String,
}

/// The records `filter` lets through, newest first, at most `limit`: from
/// the files in `dir`, or this session's lines when logs are not kept.
pub fn search(dir: &Path, filter: &Filter, limit: usize) -> Vec<Record> {
    let today = Local::now().date_naive();
    let text = filter.text.trim().to_lowercase();
    let wanted = |r: &Record| {
        filter.server.as_ref().is_none_or(|s| *s == r.server)
            && filter.period.contains(r.when.date(), today)
            && (text.is_empty() || r.text.to_lowercase().contains(&text) || r.server.to_lowercase().contains(&text))
    };
    let unsaved = STORE.lock().ok().and_then(|s| s.as_ref().filter(|st| !st.keep).map(|st| st.unsaved.clone()));
    if let Some(list) = unsaved {
        return list.into_iter().rev().filter(wanted).take(limit).collect();
    }
    let mut out = Vec::new();
    for day in days(dir).into_iter().filter(|d| filter.period.contains(*d, today)) {
        let Ok(content) = std::fs::read_to_string(dir.join(file_name(day))) else { continue };
        out.extend(content.lines().rev().filter_map(Record::parse).filter(wanted));
        if out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let r = Record {
            when: NaiveDate::from_ymd_opt(2026, 10, 12).unwrap().and_hms_opt(9, 5, 7).unwrap(),
            server: "TFTP".into(),
            text: "10.0.0.2 downloads fw.bin\twith a tab".into(),
        };
        assert_eq!(Record::parse(&r.line()), Some(r));
    }

    #[test]
    fn periods() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let today = d(10, 14); // a Wednesday
        assert!(Period::Today.contains(d(10, 14), today));
        assert!(Period::Yesterday.contains(d(10, 13), today));
        assert!(Period::ThisWeek.contains(d(10, 12), today) && !Period::ThisWeek.contains(d(10, 11), today));
        assert!(Period::ThisMonth.contains(d(10, 1), today) && !Period::ThisMonth.contains(d(9, 30), today));
        assert!(Period::LastMonth.contains(d(9, 30), today) && !Period::LastMonth.contains(d(10, 1), today));
        // Across the new year: in January, last month is December before.
        let january = NaiveDate::from_ymd_opt(2027, 1, 3).unwrap();
        assert!(Period::LastMonth.contains(d(12, 5), january));
    }
}
