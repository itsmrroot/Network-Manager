//! Watching hosts over time: ping statistics, and the routers on the way to
//! a host for a path monitor (like MTR or PingPlotter).
//!
//! The path is found once with the system's traceroute; then every router
//! on it is pinged directly, over and over. Loss and delay that start at one
//! router and continue to the end show where a problem is. (Routers that
//! give pings a low priority can show loss of their own that does not
//! continue: that is not a problem.)

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

/// Statistics of one watched host.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub sent: u64,
    pub received: u64,
    /// Milliseconds of the last reply, `None` when the last probe was lost.
    pub last: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    sum: f64,
    jitter_sum: f64,
    jitter_n: u64,
    /// Recent results, newest last: milliseconds, or `None` for a loss.
    #[serde(skip)]
    pub history: VecDeque<Option<f64>>,
    /// Probes lost in a row right now.
    pub lost_in_row: u32,
}

/// How many results [`Stats::history`] keeps.
pub const HISTORY: usize = 600;

impl Stats {
    pub fn add(&mut self, rtt: Option<Duration>) {
        self.sent += 1;
        let ms = rtt.map(|d| d.as_secs_f64() * 1000.0);
        if let Some(v) = ms {
            if let Some(prev) = self.history.iter().rev().flatten().next() {
                self.jitter_sum += (v - prev).abs();
                self.jitter_n += 1;
            }
            self.received += 1;
            self.sum += v;
            self.min = Some(self.min.map_or(v, |m| m.min(v)));
            self.max = Some(self.max.map_or(v, |m| m.max(v)));
            self.lost_in_row = 0;
        } else {
            self.lost_in_row += 1;
        }
        self.last = ms;
        self.history.push_back(ms);
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }

    pub fn loss_percent(&self) -> f64 {
        if self.sent == 0 { 0.0 } else { 100.0 * (self.sent - self.received) as f64 / self.sent as f64 }
    }

    pub fn avg(&self) -> Option<f64> {
        (self.received > 0).then(|| self.sum / self.received as f64)
    }

    pub fn jitter(&self) -> Option<f64> {
        (self.jitter_n > 0).then(|| self.jitter_sum / self.jitter_n as f64)
    }

    /// Up, down (3 losses in a row) or not known yet.
    pub fn state(&self) -> State {
        if self.sent == 0 {
            State::Unknown
        } else if self.lost_in_row >= 3 {
            State::Down
        } else if self.lost_in_row > 0 || self.loss_percent() >= 5.0 {
            State::Unstable
        } else {
            State::Up
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum State {
    Unknown,
    Up,
    Unstable,
    Down,
}

/// One line of traceroute, tracert or tracepath output: the hop number and
/// the router's address (`None` when it did not answer).
pub fn parse_hop_line(line: &str) -> Option<(u32, Option<IpAddr>)> {
    let mut words = line.split_whitespace();
    let first = words.next()?;
    let hop: u32 = first.trim_end_matches(':').trim_end_matches('?').trim_end_matches(':').parse().ok()?;
    if line.contains("[LOCALHOST]") {
        return None;
    }
    for w in words {
        let w = w.trim_matches(|c| matches!(c, '(' | ')' | '[' | ']'));
        if let Ok(ip) = w.parse::<IpAddr>() {
            return Some((hop, Some(ip)));
        }
    }
    Some((hop, None))
}

/// Finds the routers on the way to `host` with the system's traceroute,
/// handing each hop to `hop` as it is found. Ends at the host itself.
pub fn discover_path(host: &str, cancel: &AtomicBool, mut hop: impl FnMut(u32, Option<IpAddr>)) -> Result<IpAddr> {
    let target = crate::tools::resolve(host, 0)?.ip();
    let mut last = 0;
    crate::tools::traceroute(&target.to_string(), cancel, |line| {
        if let Some((n, ip)) = parse_hop_line(line)
            && n > last
        {
            last = n;
            hop(n, ip);
        }
    })?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_lines() {
        let cases = [
            (" 1  192.168.8.1  3.112 ms", Some((1, Some("192.168.8.1")))),
            (" 2  * * *", Some((2, None))),
            ("  3    12 ms    11 ms    12 ms  10.0.0.1", Some((3, Some("10.0.0.1")))),
            ("  4     *        *        *     Request timed out.", Some((4, None))),
            (" 5  router.isp.net (203.0.113.5)  9.1 ms", Some((5, Some("203.0.113.5")))),
            (" 1?: [LOCALHOST]                      pmtu 1500", None),
            (" 6:  198.51.100.7                     4.5ms", Some((6, Some("198.51.100.7")))),
            ("traceroute to 1.1.1.1 (1.1.1.1), 30 hops max", None),
            ("Tracing route to one.one.one.one [1.1.1.1]", None),
        ];
        for (line, want) in cases {
            let want = want.map(|(n, ip): (u32, Option<&str>)| (n, ip.map(|i| i.parse().unwrap())));
            assert_eq!(parse_hop_line(line), want, "{line}");
        }
    }

    #[test]
    fn stats() {
        let mut s = Stats::default();
        for r in [Some(10), None, Some(14), Some(12)] {
            s.add(r.map(Duration::from_millis));
        }
        assert_eq!((s.sent, s.received), (4, 3));
        assert_eq!(s.loss_percent(), 25.0);
        assert_eq!(s.avg(), Some(12.0));
        assert_eq!(s.min, Some(10.0));
        assert_eq!(s.jitter(), Some(3.0));
        assert_eq!(s.state(), State::Unstable);
        for _ in 0..3 {
            s.add(None);
        }
        assert_eq!(s.state(), State::Down);
    }
}
