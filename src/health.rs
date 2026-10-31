//! The connection's health over time: now and then the router and the
//! internet are pinged, a name is looked up and the Wi-Fi signal is read, and
//! the result is kept in a file on this computer, so that "it was slow last
//! night" can be looked at later.
//!
//! Nothing runs in the background of the system: the snapshots are taken while
//! the app is open and the history is turned on.

use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What the internet is pinged at: Cloudflare's resolver, which answers pings.
pub const INTERNET_HOST: [u8; 4] = [1, 1, 1, 1];
/// What is looked up by name.
const LOOKUP_NAME: &str = "example.com";
/// The history keeps this many snapshots (a month at one every five minutes is 8,640).
const MAX_SNAPSHOTS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unix seconds.
    pub time: i64,
    /// The usual delay to the router, in milliseconds; `None`: no answer, or no router.
    pub gateway_ms: Option<f64>,
    /// The same to the internet.
    pub internet_ms: Option<f64>,
    /// How long looking up a name took; `None`: it failed.
    pub lookup_ms: Option<f64>,
    /// The Wi-Fi signal in percent, on Wi-Fi.
    pub wifi_signal: Option<u8>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The middle of three pings (the lost ones count as the slowest).
fn ping3(ip: IpAddr, timeout: Duration) -> Option<f64> {
    let mut got: Vec<f64> = (0..3)
        .filter_map(|_| crate::icmp::ping(ip, timeout).ok().flatten().map(|d| d.as_secs_f64() * 1000.0))
        .collect();
    got.sort_by(|a, b| a.total_cmp(b));
    // Two answers of three: the delay is the slower of them.
    (got.len() >= 2).then(|| got[got.len() / 2])
}

/// Measures now. Takes a few seconds; run it in a thread of its own.
pub fn take(gateway: Option<IpAddr>) -> Snapshot {
    let internet = IpAddr::from(INTERNET_HOST);
    let started = Instant::now();
    let lookup_ms = crate::tools::resolve(LOOKUP_NAME, 0).ok().map(|_| started.elapsed().as_secs_f64() * 1000.0);
    Snapshot {
        time: now(),
        gateway_ms: gateway.and_then(|g| ping3(g, Duration::from_millis(1000))),
        internet_ms: ping3(internet, Duration::from_millis(2000)),
        lookup_ms,
        wifi_signal: crate::wifi::current().ok().and_then(|c| c.first().and_then(|c| c.signal)),
    }
}

fn file() -> PathBuf {
    crate::profiles::config_dir().join("health.jsonl")
}

/// Adds a snapshot to the history file (readable by this user only), and keeps the file small.
pub fn append(s: &Snapshot) -> std::io::Result<()> {
    let path = file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&path)?;
    writeln!(f, "{}", serde_json::to_string(s).unwrap_or_default())?;
    drop(f);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 2_000_000) {
        let all = load_all();
        if all.len() > MAX_SNAPSHOTS {
            let keep = &all[all.len() - MAX_SNAPSHOTS..];
            let text: String =
                keep.iter().map(|s| format!("{}\n", serde_json::to_string(s).unwrap_or_default())).collect();
            crate::cmd::write_private(&path, text)?;
        }
    }
    Ok(())
}

/// Everything in the history file, oldest first (lines that cannot be read are skipped).
pub fn load_all() -> Vec<Snapshot> {
    parse(&std::fs::read_to_string(file()).unwrap_or_default())
}

pub fn parse(text: &str) -> Vec<Snapshot> {
    let mut v: Vec<Snapshot> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    v.sort_by_key(|s| s.time);
    v
}

/// Deletes the history.
pub fn clear() -> std::io::Result<()> {
    match std::fs::remove_file(file()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What the history shows for a time span.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub snapshots: usize,
    /// Of the snapshots, how many percent reached the internet.
    pub reachable_percent: f64,
    pub median_ms: Option<f64>,
    pub worst_ms: Option<f64>,
    /// Stretches without internet while the router answered (or there is no router): the
    /// start (Unix seconds) and the number of snapshots in a row.
    pub outages: Vec<(i64, usize)>,
}

pub fn summarize(list: &[Snapshot]) -> Summary {
    let mut s = Summary { snapshots: list.len(), ..Summary::default() };
    if list.is_empty() {
        return s;
    }
    let mut times: Vec<f64> = list.iter().filter_map(|x| x.internet_ms).collect();
    s.reachable_percent = 100.0 * times.len() as f64 / list.len() as f64;
    times.sort_by(|a, b| a.total_cmp(b));
    s.median_ms = times.get(times.len() / 2).copied();
    s.worst_ms = times.last().copied();
    let mut run: Option<(i64, usize)> = None;
    for x in list {
        if x.internet_ms.is_none() {
            run = Some(run.map_or((x.time, 1), |(t, n)| (t, n + 1)));
        } else if let Some(r) = run.take() {
            s.outages.push(r);
        }
    }
    s.outages.extend(run);
    s
}

/// Is the newest snapshot much slower than the ones before it, or is the internet gone
/// while the router answers? The words of the alert, in English, with a number.
pub fn alert(before: &[Snapshot], new: &Snapshot) -> Option<Alert> {
    if new.internet_ms.is_none() && new.gateway_ms.is_some() {
        return Some(Alert::InternetDown);
    }
    let usual = |f: fn(&Snapshot) -> Option<f64>| {
        let mut v: Vec<f64> = before.iter().rev().take(30).filter_map(f).collect();
        (v.len() >= 6).then(|| {
            v.sort_by(|a, b| a.total_cmp(b));
            v[v.len() / 2]
        })
    };
    if let (Some(ms), Some(usual)) = (new.internet_ms, usual(|s| s.internet_ms))
        && ms >= (3.0 * usual).max(150.0)
    {
        return Some(Alert::Slow(ms));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Alert {
    InternetDown,
    /// Milliseconds.
    Slow(f64),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(time: i64, gw: Option<f64>, net: Option<f64>) -> Snapshot {
        Snapshot { time, gateway_ms: gw, internet_ms: net, lookup_ms: Some(20.0), wifi_signal: None }
    }

    #[test]
    fn a_line_per_snapshot_is_read_back_and_bad_lines_are_skipped() {
        let a = snap(100, Some(2.0), Some(20.0));
        let b = snap(50, None, None);
        let text =
            format!("{}\nnot json\n{}\n\n", serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
        let v = parse(&text);
        assert_eq!(v, vec![b, a]);
    }

    #[test]
    fn the_summary_counts_outages_and_delays() {
        let list = vec![
            snap(0, Some(1.0), Some(10.0)),
            snap(60, Some(1.0), None),
            snap(120, Some(1.0), None),
            snap(180, Some(1.0), Some(30.0)),
            snap(240, Some(1.0), Some(20.0)),
            snap(300, Some(1.0), None),
        ];
        let s = summarize(&list);
        assert_eq!(s.snapshots, 6);
        assert!((s.reachable_percent - 50.0).abs() < 0.01);
        assert_eq!((s.median_ms, s.worst_ms), (Some(20.0), Some(30.0)));
        assert_eq!(s.outages, vec![(60, 2), (300, 1)]);
        assert_eq!(summarize(&[]), Summary::default());
    }

    #[test]
    fn alerts_for_a_slowdown_and_for_no_internet() {
        let calm: Vec<Snapshot> = (0..10).map(|i| snap(i * 60, Some(2.0), Some(20.0))).collect();
        assert_eq!(alert(&calm, &snap(600, Some(2.0), Some(25.0))), None);
        assert_eq!(alert(&calm, &snap(600, Some(2.0), Some(400.0))), Some(Alert::Slow(400.0)));
        assert_eq!(alert(&calm, &snap(600, Some(2.0), None)), Some(Alert::InternetDown));
        // The router is gone too: that is the cable or the Wi-Fi, not the internet.
        assert_eq!(alert(&calm, &snap(600, None, None)), None);
        // Too little history to know what is usual.
        assert_eq!(alert(&calm[..3], &snap(600, Some(2.0), Some(400.0))), None);
    }
}
