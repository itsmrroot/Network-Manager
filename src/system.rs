//! The system's own network tables: open connections and listening ports
//! (with their programs), the route table, and the hosts file.

use std::net::IpAddr;

#[cfg(not(any(target_os = "macos", windows)))]
use anyhow::bail;
use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::cmd;

// ---------------------------------------------------------------------------
// Connections

#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    /// "TCP" or "UDP".
    pub proto: String,
    pub local: String,
    pub local_port: Option<u16>,
    /// Empty for listening sockets.
    pub remote: String,
    /// "LISTEN", "ESTABLISHED", … (empty for UDP).
    pub state: String,
    pub pid: Option<u32>,
    pub process: Option<String>,
}

impl Connection {
    pub fn listening(&self) -> bool {
        self.state == "LISTEN" || (self.proto == "UDP" && (self.remote.is_empty() || self.remote.starts_with('*')))
    }
}

/// Splits `address.port` (macOS netstat, `dot`) or `address:port`.
pub fn split_endpoint(s: &str, dot: bool) -> (String, Option<u16>) {
    let s = s.trim();
    match s.rsplit_once(if dot { '.' } else { ':' }) {
        Some((a, p)) => (a.trim_matches(['[', ']']).to_string(), p.parse().ok()),
        None => (s.to_string(), None),
    }
}

/// Open connections and listening ports, with their programs where the
/// system shows them (other users' programs need administrator rights on
/// Linux).
pub fn connections() -> Result<Vec<Connection>> {
    let mut out = imp::connections()?;
    out.sort_by(|a, b| (!a.listening(), a.local_port, &a.proto).cmp(&(!b.listening(), b.local_port, &b.proto)));
    Ok(out)
}

/// Parses `netstat -anv` (macOS).
pub fn parse_netstat_macos(text: &str, proto: &str) -> Vec<Connection> {
    let mut out = Vec::new();
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() < 9 || !w[0].starts_with(&proto.to_lowercase()) {
            continue;
        }
        // tcp: proto recvq sendq local foreign state rx tx rhiwat shiwat process:pid …
        let has_state = proto == "TCP";
        let first_proc = if has_state { 10 } else { 9 };
        let Some(end) =
            w.iter().skip(first_proc).position(|t| t.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u32>().is_ok()))
        else {
            continue;
        };
        let proc_text = w[first_proc..=first_proc + end].join(" ");
        let (name, pid) =
            proc_text.rsplit_once(':').map(|(n, p)| (n.trim().to_string(), p.parse().ok())).unwrap_or_default();
        let (local, port) = split_endpoint(w[3], true);
        let remote = if w[4] == "*.*" { String::new() } else { w[4].to_string() };
        out.push(Connection {
            proto: proto.into(),
            local,
            local_port: port,
            remote,
            state: if has_state { w[5].to_string() } else { String::new() },
            pid,
            process: Some(name).filter(|n| !n.is_empty()),
        });
    }
    out
}

/// Parses `lsof -nP -iTCP -iUDP -FpcnPT` (macOS): one field per line,
/// p (pid) and c (program) start a process, f a socket of it.
pub fn parse_lsof(text: &str) -> Vec<Connection> {
    let mut out: Vec<Connection> = Vec::new();
    let (mut pid, mut process) = (None, None);
    for line in text.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = value.parse().ok(),
            "c" => process = Some(value.to_string()),
            "f" => out.push(Connection {
                proto: String::new(),
                local: String::new(),
                local_port: None,
                remote: String::new(),
                state: String::new(),
                pid,
                process: process.clone(),
            }),
            "P" => {
                if let Some(c) = out.last_mut() {
                    c.proto = value.to_string();
                }
            }
            "n" => {
                if let Some(c) = out.last_mut() {
                    let (local, remote) = value.split_once("->").unwrap_or((value, ""));
                    let (addr, port) = split_endpoint(local, false);
                    c.local = addr.trim_matches(['[', ']']).to_string();
                    c.local_port = port;
                    c.remote = remote.to_string();
                }
            }
            "T" => {
                if let (Some(c), Some(st)) = (out.last_mut(), value.strip_prefix("ST=")) {
                    c.state = st.to_string();
                }
            }
            _ => {}
        }
    }
    out.retain(|c| !c.proto.is_empty() && c.local_port.is_some());
    out.dedup_by(|a, b| {
        a.proto == b.proto
            && a.local == b.local
            && a.local_port == b.local_port
            && a.remote == b.remote
            && a.pid == b.pid
    });
    out
}

/// Parses `ss -tunapH` (Linux).
pub fn parse_ss(text: &str) -> Vec<Connection> {
    text.lines()
        .filter_map(|line| {
            let w: Vec<&str> = line.split_whitespace().collect();
            if w.len() < 6 {
                return None;
            }
            let proto = w[0].to_uppercase();
            let (local, port) = split_endpoint(w[4], false);
            let remote = if w[5].starts_with('*') || w[5].ends_with(":*") { String::new() } else { w[5].to_string() };
            let users = w.get(6..).map(|r| r.join(" ")).unwrap_or_default();
            let process = users.split("((\"").nth(1).and_then(|r| r.split('"').next()).map(str::to_string);
            let pid = users.split("pid=").nth(1).and_then(|r| r.split([',', ')']).next()).and_then(|p| p.parse().ok());
            let state = match w[1] {
                "LISTEN" => "LISTEN",
                "ESTAB" => "ESTABLISHED",
                "TIME-WAIT" => "TIME_WAIT",
                "CLOSE-WAIT" => "CLOSE_WAIT",
                "SYN-SENT" => "SYN_SENT",
                "UNCONN" => "",
                other => other,
            };
            Some(Connection { proto, local, local_port: port, remote, state: state.into(), pid, process })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Routes

#[derive(Debug, Clone, Serialize)]
pub struct Route {
    pub destination: String,
    pub gateway: String,
    pub interface: String,
    pub metric: Option<u32>,
    pub flags: String,
}

pub fn routes() -> Result<Vec<Route>> {
    imp::routes()
}

/// Parses `netstat -rn` (macOS).
pub fn parse_netstat_routes(text: &str) -> Vec<Route> {
    text.lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            if w.len() < 4 || w[0] == "Destination" || l.ends_with(':') {
                return None;
            }
            Some(Route {
                destination: w[0].into(),
                gateway: w[1].into(),
                interface: w[3].into(),
                metric: None,
                flags: w[2].into(),
            })
        })
        .collect()
}

/// Adds a static route to `destination` (a CIDR network) through `gateway`.
pub fn add_route(destination: &str, gateway: IpAddr, persistent: bool) -> Result<()> {
    let net =
        crate::subnet::Subnet::parse(destination).context("write the destination as a network, e.g. 10.20.0.0/16")?;
    ensure!(destination.contains('/') || destination.contains(' '), "give the prefix too, e.g. {destination}/24");
    let cidr = format!("{}/{}", net.network, net.prefix);
    if cfg!(windows) {
        let mut args =
            vec!["add".to_string(), net.network.to_string(), "mask".into(), net.mask.to_string(), gateway.to_string()];
        if persistent {
            args.insert(0, "-p".into());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        cmd::run("route", &refs)?;
    } else if cfg!(target_os = "macos") {
        cmd::admin_sh(&format!("route -n add -net {cidr} {gateway}"))?;
    } else {
        cmd::admin_sh(&format!("ip route add {cidr} via {gateway}"))?;
    }
    Ok(())
}

pub fn delete_route(destination: &str) -> Result<()> {
    let d = destination.trim();
    ensure!(!d.is_empty() && d != "default" && d != "0.0.0.0/0", "the default route is not deleted here");
    ensure!(d.chars().all(|c| c.is_ascii_hexdigit() || ".:/".contains(c)), "not a network: {d}");
    if cfg!(windows) {
        cmd::run("route", &["delete", d.split('/').next().unwrap_or(d)])?;
    } else if cfg!(target_os = "macos") {
        cmd::admin_sh(&format!("route -n delete -net {d}"))?;
    } else {
        cmd::admin_sh(&format!("ip route del {d}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Hosts file

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum HostsLine {
    Entry {
        enabled: bool,
        ip: String,
        names: Vec<String>,
        comment: String,
    },
    /// Comments, blank lines: kept as they are.
    Other(String),
}

pub fn hosts_path() -> std::path::PathBuf {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::path::Path::new(&root).join(r"System32\drivers\etc\hosts")
    } else {
        "/etc/hosts".into()
    }
}

pub fn parse_hosts(text: &str) -> Vec<HostsLine> {
    text.lines()
        .map(|line| {
            let trimmed = line.trim();
            let (enabled, body) = match trimmed.strip_prefix('#') {
                Some(rest) => (false, rest.trim()),
                None => (true, trimmed),
            };
            let (data, comment) = match body.split_once('#') {
                Some((d, c)) => (d.trim(), c.trim().to_string()),
                None => (body, String::new()),
            };
            let mut words = data.split_whitespace();
            match (words.next().and_then(|w| w.parse::<IpAddr>().ok()), words.clone().next()) {
                (Some(ip), Some(_)) => HostsLine::Entry {
                    enabled,
                    ip: ip.to_string(),
                    names: words.map(str::to_string).collect(),
                    comment,
                },
                _ => HostsLine::Other(line.to_string()),
            }
        })
        .collect()
}

pub fn format_hosts(lines: &[HostsLine]) -> String {
    let mut out = String::new();
    for l in lines {
        match l {
            HostsLine::Other(s) => out += s,
            HostsLine::Entry { enabled, ip, names, comment } => {
                if !enabled {
                    out += "# ";
                }
                out += &format!("{ip:<16} {}", names.join(" "));
                if !comment.is_empty() {
                    out += &format!("  # {comment}");
                }
            }
        }
        out.push('\n');
    }
    out
}

pub fn read_hosts() -> Result<Vec<HostsLine>> {
    let path = hosts_path();
    let text = std::fs::read_to_string(&path).with_context(|| format!("{} could not be read", path.display()))?;
    Ok(parse_hosts(&text))
}

/// Writes the hosts file (with administrator rights), keeping a backup the
/// first time.
pub fn write_hosts(lines: &[HostsLine]) -> Result<()> {
    for l in lines {
        if let HostsLine::Entry { ip, names, .. } = l {
            ensure!(ip.parse::<IpAddr>().is_ok(), "\"{ip}\" is not an IP address");
            ensure!(!names.is_empty(), "{ip} needs at least one name");
            for n in names {
                ensure!(
                    n.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c)),
                    "\"{n}\" is not a valid host name"
                );
            }
        }
    }
    let text = format_hosts(lines);
    let path = hosts_path();
    let backup = path.with_extension("netmgr-backup");
    if cfg!(windows) {
        if !backup.exists() {
            let _ = std::fs::copy(&path, &backup);
        }
        std::fs::write(&path, text).context("the hosts file could not be saved (administrator rights are needed)")?;
        let _ = cmd::run("ipconfig", &["/flushdns"]);
        return Ok(());
    }
    let tmp = std::env::temp_dir().join(format!("netmgr-hosts-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    let (p, b, t) = (
        cmd::sh_quote(&path.display().to_string()),
        cmd::sh_quote(&backup.display().to_string()),
        cmd::sh_quote(&tmp.display().to_string()),
    );
    // `cat >` keeps the file's owner and permissions.
    let flush = if cfg!(target_os = "macos") { "; dscacheutil -flushcache; killall -HUP mDNSResponder" } else { "" };
    let r = cmd::admin_sh(&format!("[ -e {b} ] || cp -p {p} {b}; cat {t} > {p}{flush}"));
    let _ = std::fs::remove_file(&tmp);
    r.map(drop)
}

// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub fn connections() -> Result<Vec<Connection>> {
        let mut out = parse_netstat_macos(&cmd::run("netstat", &["-anv", "-p", "tcp"])?, "TCP");
        out.extend(parse_netstat_macos(&cmd::run("netstat", &["-anv", "-p", "udp"])?, "UDP"));
        if out.is_empty() {
            // Without Local Network access macOS hides the socket table;
            // lsof still shows this user's programs.
            out = parse_lsof(&cmd::run("lsof", &["-nP", "-iTCP", "-iUDP", "-FpcnPT"]).unwrap_or_default());
        }
        Ok(out)
    }

    pub fn routes() -> Result<Vec<Route>> {
        Ok(parse_netstat_routes(&cmd::run("netstat", &["-rn"])?))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub fn connections() -> Result<Vec<Connection>> {
        if cmd::exists("ss") {
            return Ok(parse_ss(&cmd::run("ss", &["-tunapH"])?));
        }
        bail!("the ss tool (iproute2) is needed")
    }

    pub fn routes() -> Result<Vec<Route>> {
        let mut out = Vec::new();
        for family in ["-4", "-6"] {
            let text = cmd::run("ip", &["-j", family, "route", "show"])?;
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            for r in v.as_array().into_iter().flatten() {
                out.push(Route {
                    destination: r["dst"].as_str().unwrap_or("").to_string(),
                    gateway: r["gateway"].as_str().unwrap_or("").to_string(),
                    interface: r["dev"].as_str().unwrap_or("").to_string(),
                    metric: r["metric"].as_u64().map(|m| m as u32),
                    flags: r["protocol"].as_str().unwrap_or("").to_string(),
                });
            }
        }
        Ok(out)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    pub fn connections() -> Result<Vec<Connection>> {
        let script = "$p = @{}; Get-Process | ForEach-Object { $p[$_.Id] = $_.ProcessName }\n\
            $t = Get-NetTCPConnection | ForEach-Object { [pscustomobject]@{ P='TCP'; L=$_.LocalAddress; LP=$_.LocalPort; R=\"$($_.RemoteAddress):$($_.RemotePort)\"; S=\"$($_.State)\"; I=$_.OwningProcess; N=$p[[int]$_.OwningProcess] } }\n\
            $u = Get-NetUDPEndpoint | ForEach-Object { [pscustomobject]@{ P='UDP'; L=$_.LocalAddress; LP=$_.LocalPort; R=''; S=''; I=$_.OwningProcess; N=$p[[int]$_.OwningProcess] } }\n\
            @($t) + @($u) | ConvertTo-Json -Compress";
        let text = cmd::powershell(script)?;
        let v: serde_json::Value = serde_json::from_str(text.trim()).context("unexpected answer from PowerShell")?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let state = match c["S"].as_str().unwrap_or("") {
                    "Listen" => "LISTEN".to_string(),
                    "Established" => "ESTABLISHED".to_string(),
                    "TimeWait" => "TIME_WAIT".to_string(),
                    "CloseWait" => "CLOSE_WAIT".to_string(),
                    "Bound" => "BOUND".to_string(),
                    s => s.to_uppercase(),
                };
                let remote = c["R"].as_str().unwrap_or("").to_string();
                Connection {
                    proto: c["P"].as_str().unwrap_or("").to_string(),
                    local: c["L"].as_str().unwrap_or("").to_string(),
                    local_port: c["LP"].as_u64().map(|p| p as u16),
                    remote: if remote.starts_with("0.0.0.0:0") || remote.starts_with("::") && remote.ends_with(":0") {
                        String::new()
                    } else {
                        remote
                    },
                    state,
                    pid: c["I"].as_u64().map(|p| p as u32),
                    process: c["N"].as_str().map(str::to_string),
                }
            })
            .collect())
    }

    pub fn routes() -> Result<Vec<Route>> {
        let text = cmd::powershell(
            "Get-NetRoute | Select-Object DestinationPrefix,NextHop,InterfaceAlias,RouteMetric,Protocol | ConvertTo-Json -Compress",
        )?;
        let v: serde_json::Value = serde_json::from_str(text.trim()).context("unexpected answer from PowerShell")?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .map(|r| Route {
                destination: r["DestinationPrefix"].as_str().unwrap_or("").to_string(),
                gateway: r["NextHop"].as_str().unwrap_or("").to_string(),
                interface: r["InterfaceAlias"].as_str().unwrap_or("").to_string(),
                metric: r["RouteMetric"].as_u64().map(|m| m as u32),
                flags: r["Protocol"].to_string().trim_matches('"').to_string(),
            })
            .collect())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    use super::*;
    pub fn connections() -> Result<Vec<Connection>> {
        bail!("not supported")
    }
    pub fn routes() -> Result<Vec<Route>> {
        bail!("not supported")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_netstat() {
        let text = "Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)          rxbytes      txbytes  rhiwat  shiwat          process:pid    state  options\n\
tcp4       0      0  192.168.8.64.49943     4.150.223.114.443      ESTABLISHED        19494         4590  131072  132104 Antigravity IDE :2086   00102 00000008\n\
tcp4       0      0  127.0.0.1.52595        *.*                    LISTEN                 0            0  131072  131072 language_server_:72514  00100 00000106\n";
        let c = parse_netstat_macos(text, "TCP");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].process.as_deref(), Some("Antigravity IDE"));
        assert_eq!(c[0].pid, Some(2086));
        assert_eq!(c[1].state, "LISTEN");
        assert!(c[1].listening());
        assert_eq!(c[1].remote, "");
    }

    #[test]
    fn macos_lsof() {
        let text = "p949\ncmongod\nf10\nPTCP\nn127.0.0.1:27017\nTST=LISTEN\nf11\nPTCP\nn[::1]:631->[::1]:50000\nTST=ESTABLISHED\np12\ncmDNSResponder\nf5\nPUDP\nn*:5353\n";
        let c = parse_lsof(text);
        assert_eq!(c.len(), 3);
        assert_eq!(
            (c[0].process.as_deref(), c[0].pid, c[0].local_port, c[0].state.as_str()),
            (Some("mongod"), Some(949), Some(27017), "LISTEN")
        );
        assert_eq!((c[1].local.as_str(), c[1].remote.as_str()), ("::1", "[::1]:50000"));
        assert!(c[2].listening());
    }

    #[test]
    fn linux_ss() {
        let text = "tcp   LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=812,fd=3))\n\
                    tcp   ESTAB  0 0   192.168.1.5:22 192.168.1.9:51234 users:((\"sshd\",pid=9000,fd=4))\n\
                    udp   UNCONN 0 0   [::]:5353 [::]:*\n";
        let c = parse_ss(text);
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].process.as_deref(), c[0].pid, c[0].local_port), (Some("sshd"), Some(812), Some(22)));
        assert_eq!(c[1].state, "ESTABLISHED");
        assert_eq!(c[2].local, "::");
        assert!(c[2].listening());
    }

    #[test]
    fn hosts_round_trip() {
        let text =
            "# The hosts file\n127.0.0.1 localhost\n# 10.0.0.5 old-server  # moved\n10.0.0.6 nas nas.lab  # storage\n";
        let lines = parse_hosts(text);
        assert_eq!(lines.len(), 4);
        assert!(matches!(&lines[0], HostsLine::Other(_)));
        assert!(matches!(&lines[2], HostsLine::Entry { enabled: false, ip, .. } if ip == "10.0.0.5"));
        assert!(
            matches!(&lines[3], HostsLine::Entry { names, comment, .. } if names.len() == 2 && comment == "storage")
        );
        let again = parse_hosts(&format_hosts(&lines));
        assert_eq!(again, lines);
    }

    #[test]
    fn mac_routes() {
        let text = "Routing tables\n\nInternet:\nDestination        Gateway            Flags               Netif Expire\ndefault            192.168.8.1        UGScg                 en0\n";
        let r = parse_netstat_routes(text);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].destination.as_str(), r[0].gateway.as_str(), r[0].interface.as_str()),
            ("default", "192.168.8.1", "en0")
        );
    }
}
