//! The internet side: public address, speed test and a step-by-step check
//! of why a connection does not work.

use std::io::Read;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::adapters::{self, Adapter};
use crate::cmd;
use crate::dns::{self, RecordType};

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(format!("netmgr/{}", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(8)))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// This connection as the internet sees it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublicInfo {
    pub ip: String,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    /// Internet provider ("AS8447 A1 Telekom Austria AG").
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
}

impl PublicInfo {
    /// The provider without its AS number.
    pub fn provider(&self) -> Option<String> {
        let org = self.org.as_deref()?;
        Some(match org.split_once(' ') {
            Some((asn, name)) if asn.starts_with("AS") => name.to_string(),
            _ => org.to_string(),
        })
    }

    /// "Vienna, Austria".
    pub fn place(&self) -> Option<String> {
        let parts: Vec<&str> = [self.city.as_deref(), self.region.as_deref(), self.country.as_deref()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        let mut seen = Vec::new();
        for p in parts {
            if !seen.contains(&p) {
                seen.push(p);
            }
        }
        (!seen.is_empty()).then(|| seen.join(", "))
    }
}

/// The public address, with the provider and the place when available.
pub fn public_info() -> Result<PublicInfo> {
    let a = agent(Duration::from_secs(10));
    if let Ok(mut r) = a.get("https://ipinfo.io/json").header("Accept", "application/json").call()
        && r.status() == 200
        && let Ok(text) = r.body_mut().read_to_string()
        && let Ok(info) = serde_json::from_str::<PublicInfo>(&text)
        && !info.ip.is_empty()
    {
        return Ok(info);
    }
    let ip = a
        .get("https://api.ipify.org")
        .call()
        .context("the internet could not be reached")?
        .body_mut()
        .read_to_string()?;
    let ip = ip.trim().to_string();
    ip.parse::<IpAddr>().context("unexpected answer")?;
    Ok(PublicInfo { ip, ..Default::default() })
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct SpeedResult {
    pub ping_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub download_mbps: Option<f64>,
    pub upload_mbps: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedPhase {
    Ping,
    Download,
    Upload,
}

const SPEED_HOST: &str = "https://speed.cloudflare.com";

/// Measures latency, download and upload speed against Cloudflare's speed
/// test servers. `live` gets the phase and the current value (ms or Mbit/s).
pub fn speed_test(cancel: &AtomicBool, live: &dyn Fn(SpeedPhase, f64)) -> Result<SpeedResult> {
    let mut result = SpeedResult::default();
    let a = agent(Duration::from_secs(30));

    let mut pings = Vec::new();
    for _ in 0..8 {
        if cancel.load(Ordering::Relaxed) {
            bail!("stopped");
        }
        let t = Instant::now();
        let mut r = a
            .get(format!("{SPEED_HOST}/__down?bytes=0"))
            .call()
            .context("the speed test server could not be reached")?;
        let _ = r.body_mut().read_to_vec();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        pings.push(ms);
        live(SpeedPhase::Ping, ms);
    }
    // The first request also opens the connection: leave it out.
    pings.remove(0);
    pings.sort_by(|a, b| a.total_cmp(b));
    result.ping_ms = Some(pings[pings.len() / 2]);
    result.jitter_ms = Some(pings.windows(2).map(|w| w[1] - w[0]).sum::<f64>() / (pings.len() - 1) as f64);

    // Download: bigger and bigger files until 8 seconds have passed.
    let started = Instant::now();
    let mut total = 0u64;
    let mut size = 1_000_000u64;
    let mut buf = vec![0u8; 64 * 1024];
    'down: while started.elapsed() < Duration::from_secs(8) {
        let mut r = a.get(format!("{SPEED_HOST}/__down?bytes={size}")).call()?;
        let mut body = r.body_mut().with_config().limit(size + 1024).reader();
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("stopped");
            }
            let n = body.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            let secs = started.elapsed().as_secs_f64();
            if secs > 0.2 {
                live(SpeedPhase::Download, total as f64 * 8.0 / secs / 1e6);
            }
            if started.elapsed() > Duration::from_secs(12) {
                break 'down;
            }
        }
        size = (size * 2).min(100_000_000);
    }
    result.download_mbps = Some(total as f64 * 8.0 / started.elapsed().as_secs_f64() / 1e6);

    // Upload.
    let started = Instant::now();
    let mut total = 0u64;
    let mut size = 500_000usize;
    while started.elapsed() < Duration::from_secs(8) {
        if cancel.load(Ordering::Relaxed) {
            bail!("stopped");
        }
        let body = vec![b'0'; size];
        a.post(format!("{SPEED_HOST}/__up")).send(&body[..])?;
        total += size as u64;
        live(SpeedPhase::Upload, total as f64 * 8.0 / started.elapsed().as_secs_f64() / 1e6);
        size = (size * 2).min(25_000_000);
    }
    result.upload_mbps = Some(total as f64 * 8.0 / started.elapsed().as_secs_f64() / 1e6);
    Ok(result)
}

/// One step of the connection check. Its texts are fixed English sentences
/// (with `{v}` for a value), so that the desktop app can translate them.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub title: &'static str,
    pub ok: bool,
    /// What was found: a sentence with `{v}` standing for [`Check::value`].
    pub template: &'static str,
    pub value: String,
    /// What to do about it, when it failed.
    pub advice: Option<&'static str>,
}

impl Check {
    /// What was found, in English.
    pub fn detail(&self) -> String {
        self.template.replace("{v}", &self.value)
    }
}

fn check(
    title: &'static str,
    ok: bool,
    template: &'static str,
    value: impl ToString,
    advice: Option<&'static str>,
) -> Check {
    Check { title, ok, template, value: value.to_string(), advice }
}

/// Checks the connection step by step — adapter, address, router, DNS,
/// internet — and stops at the first step that fails. `step` gets each
/// result as it comes.
pub fn diagnose(step: &dyn Fn(&Check)) -> Vec<Check> {
    let mut out = Vec::new();
    let mut push = |c: Check| {
        step(&c);
        let ok = c.ok;
        out.push(c);
        ok
    };

    // 1. An adapter that is connected.
    let adapters = adapters::list(false).unwrap_or_default();
    let adapter: Option<Adapter> =
        adapters.iter().find(|a| a.default).or_else(|| adapters.iter().find(|a| a.up)).cloned();
    let Some(a) = adapter else {
        push(check(
            "Network adapter",
            false,
            "No adapter is connected to a network.",
            "",
            Some("Plug in the network cable or join a Wi-Fi network. If Wi-Fi is off, turn it on."),
        ));
        return out;
    };
    if !push(check(
        "Network adapter",
        a.up,
        "{v}",
        format!("{} ({})", a.name, a.kind.label()),
        (!a.up).then_some("The adapter is not connected: check the cable or join a Wi-Fi network."),
    )) {
        return out;
    }

    // 2. An address from the router.
    let ip = a.main_ipv4();
    let ok = ip.is_some() && !a.self_assigned();
    let (template, value) = match ip {
        Some((ip, p)) if ok => ("{v}", format!("{ip}/{p}")),
        Some((ip, _)) => ("{v} — given by the computer itself", ip.to_string()),
        None => ("No address", String::new()),
    };
    if !push(check(
        "IP address",
        ok,
        template,
        value,
        (!ok).then_some(
            "The router did not give this computer an address. Restart the router, then click \"Renew IP\". If the address was set by hand, check it.",
        ),
    )) {
        return out;
    }

    // 3. The router answers.
    let gateway = a.gateway;
    let reachable = gateway.is_some_and(reach);
    let (template, value) = match gateway {
        Some(g) if reachable => ("{v} answers", g.to_string()),
        Some(g) => ("{v} does not answer", g.to_string()),
        None => ("No router (gateway) is set", String::new()),
    };
    let advice = match (reachable, gateway) {
        (true, _) => None,
        (false, None) => Some(
            "Without a gateway, nothing outside this network can be reached. Use automatic settings (DHCP) or enter the router's address.",
        ),
        (false, Some(_)) => Some("The router does not answer. Restart it; for Wi-Fi, move closer to it."),
    };
    if !push(check("Router", reachable, template, value, advice)) {
        return out;
    }

    // 4. Names can be looked up.
    let servers = dns::system_servers();
    let resolved = servers.iter().take(2).find_map(|s| {
        dns::query(SocketAddr::new(*s, 53), "www.google.com", RecordType::A, Duration::from_secs(3))
            .ok()
            .filter(|ans| !ans.records.is_empty())
            .map(|ans| (*s, ans.millis))
    });
    let system_ok =
        std::net::ToSocketAddrs::to_socket_addrs(&("www.google.com", 443)).is_ok_and(|mut a| a.next().is_some());
    let dns_ok = resolved.is_some() || system_ok;
    let (template, value) = match resolved {
        Some((s, ms)) => ("{v} ms", format!("{s}: {ms}")),
        None if system_ok => ("Names are found", String::new()),
        None if servers.is_empty() => ("No DNS server answers", String::new()),
        None => ("No answer from {v}", servers.iter().map(IpAddr::to_string).collect::<Vec<_>>().join(", ")),
    };
    if !push(check(
        "DNS (names)",
        dns_ok,
        template,
        value,
        (!dns_ok).then_some(
            "Websites cannot be found by name. Click \"Flush DNS\", or set a public DNS server such as Cloudflare (1.1.1.1) in Adapters → Change IP settings.",
        ),
    )) {
        return out;
    }

    // 5. The internet, and whether a login page is in the way.
    let tcp = TcpStream::connect_timeout(&"1.1.1.1:443".parse().unwrap(), Duration::from_secs(4)).is_ok();
    let portal = agent(Duration::from_secs(6))
        .get("http://connectivitycheck.gstatic.com/generate_204")
        .call()
        .ok()
        .map(|r| r.status().as_u16());
    let c = match (tcp, portal) {
        (_, Some(204)) => check("Internet", true, "Connected to the internet", "", None),
        (_, Some(code)) => check(
            "Internet",
            false,
            "A login page is in the way (answer {v})",
            code,
            Some(
                "This network wants you to sign in (hotel, airport, café). Open any website in your browser to see its login page.",
            ),
        ),
        (true, None) => check("Internet", true, "Connected (web check blocked)", "", None),
        (false, None) => check(
            "Internet",
            false,
            "The internet does not answer",
            "",
            Some(
                "The router is reachable but the internet is not: restart the router or modem. If it stays like this, your provider may have an outage.",
            ),
        ),
    };
    push(c);
    out
}

/// Whether `ip` answers: a ping, or any common port that answers or refuses.
pub fn reach(ip: IpAddr) -> bool {
    let (prog, args) = crate::tools::ping_command(&ip.to_string(), 2);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if cmd::output(&prog, &refs).is_ok_and(|o| {
        o.success
            && o.stdout.lines().any(|l| matches!(crate::tools::parse_ping_line(l), crate::tools::PingLine::Reply(_)))
    }) {
        return true;
    }
    // Routers that ignore pings still answer on their web page or DNS.
    [80, 443, 53].iter().any(|p| {
        match TcpStream::connect_timeout(&SocketAddr::new(ip, *p), Duration::from_millis(800)) {
            Ok(_) => true,
            Err(e) => e.kind() == std::io::ErrorKind::ConnectionRefused,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_place() {
        let info: PublicInfo = serde_json::from_str(
            r#"{"ip":"1.2.3.4","city":"Vienna","region":"Vienna","country":"AT","org":"AS8447 A1 Telekom Austria AG"}"#,
        )
        .unwrap();
        assert_eq!(info.provider().as_deref(), Some("A1 Telekom Austria AG"));
        assert_eq!(info.place().as_deref(), Some("Vienna, AT"));
    }
}
