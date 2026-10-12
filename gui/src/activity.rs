//! What others do with this computer: SSH sign-ins, files fetched or sent
//! through the servers, devices using a service. Shown in Servers →
//! Activity and, as chosen in Settings, as desktop notifications.

use std::collections::{HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// The most events kept.
const KEEP: usize = 1000;

#[derive(Default)]
struct Inner {
    events: VecDeque<Event>,
    /// Not yet looked at by the app (for notifications).
    pending: Vec<Event>,
    /// Events added since the Activity tab was last shown.
    unread: usize,
    /// Service events are noted once per device and service.
    seen: HashSet<(String, IpAddr)>,
}

/// The activity log, shared with the servers' threads.
#[derive(Clone, Default)]
pub struct Activity(Arc<Mutex<Inner>>);

impl Activity {
    pub fn add(&self, kind: Kind, text: String) {
        let Ok(mut a) = self.0.lock() else { return };
        let e = Event { when: chrono::Local::now(), kind, text };
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

    pub fn clear(&self) {
        if let Ok(mut a) = self.0.lock() {
            a.events.clear();
            a.unread = 0;
            a.seen.clear();
        }
    }

    /// Events the app has not handled yet (for notifications).
    pub fn take_pending(&self) -> Vec<Event> {
        self.0.lock().map(|mut a| std::mem::take(&mut a.pending)).unwrap_or_default()
    }
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
