//! A network report to attach to a support ticket or keep as a record:
//! the computer, its adapters, Wi-Fi, routes, DNS, the connection check and
//! how well the router and the internet answer. Wi-Fi passwords are never
//! included.

use std::fmt::Write;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::adapters::{self, format_bytes, format_speed, prefix_to_mask};
use crate::monitor::Stats;
use crate::{dns, icmp, internet, scan, system, wifi};

/// The operating system and its version.
pub fn os_name() -> String {
    let version = if cfg!(target_os = "macos") {
        crate::cmd::run("sw_vers", &["-productVersion"]).map(|v| format!("macOS {}", v.trim())).ok()
    } else if cfg!(windows) {
        crate::cmd::run("cmd", &["/c", "ver"]).map(|v| v.trim().to_string()).ok()
    } else {
        std::fs::read_to_string("/etc/os-release").ok().and_then(|t| {
            t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        })
    };
    format!("{} ({})", version.unwrap_or_else(|| std::env::consts::OS.to_string()), std::env::consts::ARCH)
}

fn ping_stats(ip: IpAddr, n: usize) -> Stats {
    let mut s = Stats::default();
    for _ in 0..n {
        s.add(icmp::ping(ip, Duration::from_secs(1)).ok().flatten());
    }
    s
}

fn stats_line(s: &Stats) -> String {
    match (s.avg(), s.min, s.max) {
        (Some(avg), Some(min), Some(max)) => format!(
            "{} of {} answered ({:.0} % loss), {min:.1} / {avg:.1} / {max:.1} ms (min / avg / max), jitter {:.1} ms",
            s.received,
            s.sent,
            s.loss_percent(),
            s.jitter().unwrap_or(0.0)
        ),
        _ => format!("no answer to {} pings", s.sent),
    }
}

/// Builds the report (Markdown). Takes several seconds: it pings and looks
/// things up. `step` gets what is being done.
pub fn build(include_public_ip: bool, step: &dyn Fn(&str)) -> String {
    let mut r = String::new();
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let time = format!("{} {:02}:{:02} UTC", crate::web::date(now), (now % 86400) / 3600, (now % 3600) / 60);
    let _ = writeln!(r, "# Network report\n");
    let _ = writeln!(r, "| | |\n|---|---|");
    let _ = writeln!(r, "| Created | {time} |");
    let _ = writeln!(r, "| Computer | {} |", scan::hostname().unwrap_or_default());
    step("Reading the system");
    let _ = writeln!(r, "| System | {} |", os_name());
    let _ = writeln!(r, "| Made with | Network Manager {} — {} |\n", env!("CARGO_PKG_VERSION"), crate::POWERED_BY);

    step("Checking the connection");
    let _ = writeln!(r, "## Connection check\n");
    for c in internet::diagnose(&|_| {}) {
        let _ = writeln!(r, "- {} **{}**: {}", if c.ok { "✅" } else { "❌" }, c.title, c.detail());
        if let Some(a) = c.advice {
            let _ = writeln!(r, "  - {a}");
        }
    }

    step("Reading the adapters");
    let _ = writeln!(r, "\n## Adapters\n");
    let list = adapters::list(false).unwrap_or_default();
    for a in &list {
        let _ = writeln!(r, "### {}{}\n", a.name, if a.default { " (default route)" } else { "" });
        let _ = writeln!(r, "| | |\n|---|---|");
        let _ = writeln!(r, "| Type | {} — {} |", a.kind.label(), a.status());
        if let Some(d) = &a.description {
            let _ = writeln!(r, "| Hardware | {d} |");
        }
        if let Some(m) = a.mac {
            let _ = writeln!(
                r,
                "| MAC | {m} ({}) |",
                if m.is_local() { "private" } else { m.vendor().unwrap_or("unknown maker") }
            );
        }
        let how = match a.dhcp {
            Some(true) => "automatic (DHCP)",
            Some(false) => "manual (static)",
            None => "—",
        };
        let _ = writeln!(r, "| Configured | {how} |");
        for (ip, p) in &a.ipv4 {
            let _ = writeln!(r, "| IPv4 | {ip}/{p} (mask {}) |", prefix_to_mask(*p));
        }
        for (ip, p) in &a.ipv6 {
            let _ = writeln!(r, "| IPv6 | {ip}/{p} |");
        }
        if let Some(g) = a.gateway {
            let _ = writeln!(r, "| Gateway | {g}{} |", a.gateway_mac.map(|m| format!(" ({m})")).unwrap_or_default());
        }
        if !a.dns.is_empty() {
            let _ = writeln!(r, "| DNS | {} |", a.dns.iter().map(IpAddr::to_string).collect::<Vec<_>>().join(", "));
        }
        if let Some(s) = a.speed_bps {
            let _ = writeln!(r, "| Link speed | {} |", format_speed(s));
        }
        if let Some(m) = a.mtu {
            let _ = writeln!(r, "| MTU | {m} |");
        }
        if let (Some(rx), Some(tx)) = (a.rx_bytes, a.tx_bytes) {
            let _ = writeln!(r, "| Traffic | {} received, {} sent |", format_bytes(rx), format_bytes(tx));
        }
        let _ = writeln!(r);
    }

    step("Reading Wi-Fi");
    if let Ok(conns) = wifi::current()
        && !conns.is_empty()
    {
        let _ = writeln!(r, "## Wi-Fi\n");
        for c in conns {
            let _ =
                writeln!(r, "- **{}** on {}", c.ssid.as_deref().unwrap_or("(name hidden by the system)"), c.interface);
            if let Some(q) = c.quality() {
                let dbm = c.rssi.map(|v| format!(", {v} dBm")).unwrap_or_default();
                let noise = c.noise.map(|v| format!(", noise {v} dBm")).unwrap_or_default();
                let _ = writeln!(r, "- Signal: {q} % ({}{dbm}{noise})", wifi::quality_label(q));
            }
            if let Some(ch) = c.channel {
                let _ = writeln!(r, "- Channel: {ch} {} {}", c.band.unwrap_or_default(), c.width.unwrap_or_default());
            }
            for (k, v) in [("Security", c.security), ("Standard", c.standard), ("Access point", c.bssid)] {
                if let Some(v) = v {
                    let _ = writeln!(r, "- {k}: {v}");
                }
            }
            if let Some(rate) = c.rate_mbps {
                let _ = writeln!(r, "- Link rate: {rate:.0} Mbit/s");
            }
        }
        let _ = writeln!(r);
    }

    let _ = writeln!(r, "## Reachability\n");
    if let Some(g) = list.iter().find(|a| a.default).and_then(|a| a.gateway) {
        step("Pinging the router");
        let _ = writeln!(r, "- Router {g}: {}", stats_line(&ping_stats(g, 10)));
    }
    step("Pinging the internet");
    for (name, ip) in [("Cloudflare", "1.1.1.1"), ("Google", "8.8.8.8")] {
        if let Ok(ip) = ip.parse::<IpAddr>() {
            let _ = writeln!(r, "- {name} {ip}: {}", stats_line(&ping_stats(ip, 5)));
        }
    }
    step("Timing DNS");
    let mut servers: Vec<(String, IpAddr)> =
        dns::system_servers().into_iter().take(2).map(|s| (format!("This computer ({s})"), s)).collect();
    for (n, ip) in dns::PUBLIC_RESOLVERS.iter().take(2) {
        if let Ok(ip) = ip.parse() {
            servers.push((format!("{n} ({ip})"), ip));
        }
    }
    for (label, ip) in servers {
        let answer = dns::query(SocketAddr::new(ip, 53), "www.google.com", dns::RecordType::A, Duration::from_secs(3));
        let _ = writeln!(
            r,
            "- DNS {label}: {}",
            match answer {
                Ok(a) => format!("{} in {} ms", a.status, a.millis),
                Err(e) => format!("{e:#}"),
            }
        );
    }
    if include_public_ip {
        step("Looking up the public address");
        if let Ok(p) = internet::public_info() {
            let _ = writeln!(
                r,
                "- Public address: {}{}{}",
                p.ip,
                p.provider().map(|v| format!(" — {v}")).unwrap_or_default(),
                p.place().map(|v| format!(" — {v}")).unwrap_or_default()
            );
        }
    }

    step("Reading the routes");
    if let Ok(routes) = system::routes() {
        let _ = writeln!(r, "\n## Routes\n\n| Destination | Gateway | Interface | Metric |\n|---|---|---|---|");
        for rt in routes.iter().take(60) {
            let _ = writeln!(
                r,
                "| {} | {} | {} | {} |",
                rt.destination,
                rt.gateway,
                rt.interface,
                rt.metric.map(|m| m.to_string()).unwrap_or_default()
            );
        }
    }
    let _ = writeln!(r, "\n---\n_{}_", crate::POWERED_BY);
    r
}
