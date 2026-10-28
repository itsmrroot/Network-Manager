//! The VPN connections the system itself keeps (macOS network services,
//! NetworkManager on Linux, Windows' built-in VPN), and their switches.
//! Tailscale has its own module; WireGuard and others that run as programs
//! show up here only when the system lists them.

use anyhow::Result;

use crate::cmd;

/// How long a system tool may take.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
const LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// The name it has in the system ("Work VPN").
    pub name: String,
    /// "IKEv2", "L2TP", "WireGuard", "OpenVPN", "VPN".
    pub kind: String,
    pub connected: bool,
    /// Connecting…, Disconnecting… and the like, when the system says.
    pub busy: bool,
}

/// `scutil --nc list` (macOS).
pub fn parse_scutil(text: &str) -> Vec<Connection> {
    let mut out = Vec::new();
    for line in text.lines() {
        // * (Connected)      1234-ABCD VPN (com.apple.neplugin) "Work VPN"   [VPN:com.apple.neplugin.IKEv2]
        let Some(open) = line.find('(') else { continue };
        let Some(close) = line[open..].find(')') else { continue };
        let state = &line[open + 1..open + close];
        let Some(q1) = line.find('"') else { continue };
        let Some(q2) = line.rfind('"').filter(|q| *q > q1) else { continue };
        let name = line[q1 + 1..q2].to_string();
        // `[VPN:com.apple.neplugin.IKEv2]` for the VPN services, `[PPP:L2TP]` for
        // the older PPP ones (PPPoE is a DSL login, not a VPN).
        let tag =
            line[q2..].split('[').nth(1).map(|t| t.trim_end().trim_end_matches(']').to_lowercase()).unwrap_or_default();
        if tag.starts_with("ppp:pppoe") || tag.starts_with("ppp:") && !tag.contains("l2tp") && !tag.contains("pptp") {
            continue;
        }
        // Tailscale has its own panel.
        if tag.contains("tailscale") {
            continue;
        }
        let kind = if tag.contains("wireguard") {
            "WireGuard"
        } else if tag.contains("ikev2") {
            "IKEv2"
        } else if tag.contains("l2tp") {
            "L2TP"
        } else if tag.contains("pptp") {
            "PPTP"
        } else if tag.contains("openvpn") {
            "OpenVPN"
        } else if tag.contains("cisco") || tag.contains("anyconnect") {
            "Cisco"
        } else {
            "VPN"
        };
        out.push(Connection {
            name,
            kind: kind.to_string(),
            connected: state.eq_ignore_ascii_case("connected"),
            busy: state.eq_ignore_ascii_case("connecting") || state.eq_ignore_ascii_case("disconnecting"),
        });
    }
    out
}

/// A field of `nmcli -t` output: `\:` is a colon in a name.
fn nmcli_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ':' => out.push(String::new()),
            // A backslash makes the next character an ordinary one.
            '\\' => {
                if let (Some(n), Some(last)) = (chars.next(), out.last_mut()) {
                    last.push(n);
                }
            }
            c => {
                if let Some(last) = out.last_mut() {
                    last.push(c);
                }
            }
        }
    }
    out
}

/// `nmcli -t -f NAME,TYPE,ACTIVE connection show` (Linux).
pub fn parse_nmcli(text: &str) -> Vec<Connection> {
    text.lines()
        .filter_map(|line| {
            let f = nmcli_fields(line);
            let [name, kind, active] = f.as_slice() else { return None };
            let kind = match kind.as_str() {
                "vpn" => "VPN",
                "wireguard" => "WireGuard",
                _ => return None,
            };
            Some(Connection { name: name.clone(), kind: kind.into(), connected: active == "yes", busy: false })
        })
        .collect()
}

/// The JSON of `Get-VpnConnection` (Windows): one object or a list.
pub fn parse_windows(json: &str) -> Vec<Connection> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let items = match v {
        serde_json::Value::Array(a) => a,
        o @ serde_json::Value::Object(_) => vec![o],
        _ => Vec::new(),
    };
    items
        .iter()
        .filter_map(|i| {
            let name = i.get("Name")?.as_str()?.to_string();
            let status = i.get("ConnectionStatus").map(|s| s.to_string().to_lowercase()).unwrap_or_default();
            let kind = i.get("TunnelType").and_then(|t| t.as_str()).unwrap_or("VPN");
            Some(Connection {
                name,
                kind: if kind.is_empty() || kind.eq_ignore_ascii_case("automatic") {
                    "VPN".into()
                } else {
                    kind.into()
                },
                connected: status.contains("connected") && !status.contains("disconnected"),
                busy: false,
            })
        })
        .collect()
}

/// The VPN connections of the system.
pub fn list() -> Result<Vec<Connection>> {
    #[cfg(target_os = "macos")]
    {
        Ok(parse_scutil(&cmd::run_timeout("scutil", &["--nc", "list"], LIMIT)?))
    }
    #[cfg(target_os = "linux")]
    {
        if !cmd::exists("nmcli") {
            return Ok(Vec::new());
        }
        Ok(parse_nmcli(&cmd::run_timeout("nmcli", &["-t", "-f", "NAME,TYPE,ACTIVE", "connection", "show"], LIMIT)?))
    }
    #[cfg(windows)]
    {
        let out = cmd::powershell(
            "Get-VpnConnection | Select-Object Name,TunnelType,ConnectionStatus | ConvertTo-Json -Compress",
        )?;
        Ok(parse_windows(&out))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Ok(Vec::new())
    }
}

/// Connects (`on`) or disconnects a connection.
pub fn set(c: &Connection, on: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        cmd::run_timeout("scutil", &["--nc", if on { "start" } else { "stop" }, &c.name], LIMIT).map(drop)
    }
    #[cfg(target_os = "linux")]
    {
        cmd::run_timeout("nmcli", &["connection", if on { "up" } else { "down" }, "id", &c.name], LIMIT).map(drop)
    }
    #[cfg(windows)]
    {
        if on {
            cmd::run_timeout("rasdial", &[&c.name], LIMIT).map(drop)
        } else {
            cmd::run_timeout("rasdial", &[&c.name, "/disconnect"], LIMIT).map(drop)
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (c, on);
        anyhow::bail!("VPN connections cannot be switched on this system")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_vpn_services_are_listed_without_tailscale() {
        let text = "Available network connection services in the current set (*=enabled):
* (Disconnected)   1B2C-0001 VPN (com.apple.neplugin) \"Work VPN\"   [VPN:com.apple.neplugin.IKEv2]
* (Connected)      1B2C-0002 VPN (com.wireguard.macos) \"Home (WireGuard)\"   [VPN:com.wireguard.macos.network-extension]
* (Disconnected)   1B2C-0003 VPN (io.tailscale.ipn.macos) \"Tailscale\"   [VPN:io.tailscale.ipn.macos.network-extension]
* (Connecting)     1B2C-0004 VPN (com.apple.neplugin) \"Office\"   [VPN:com.apple.neplugin.L2TP]
* (Disconnected)   1B2C-0005 PPP --> en0 \"DSL\"   [PPP:PPPoE]
* (Connected)      1B2C-0006 PPP --> en0 \"Old VPN\"   [PPP:L2TP]
";
        let list = parse_scutil(text);
        assert_eq!(
            list.iter().map(|c| (c.name.as_str(), c.kind.as_str(), c.connected, c.busy)).collect::<Vec<_>>(),
            [
                ("Work VPN", "IKEv2", false, false),
                ("Home (WireGuard)", "WireGuard", true, false),
                ("Office", "L2TP", false, true),
                ("Old VPN", "L2TP", true, false),
            ]
        );
    }

    #[test]
    fn network_manager_vpns_are_listed() {
        let text =
            "Wired connection 1:802-3-ethernet:yes\nWork\\: Vienna:vpn:no\nhome-wg:wireguard:yes\nlo:loopback:yes\n";
        assert_eq!(
            parse_nmcli(text).iter().map(|c| (c.name.as_str(), c.kind.as_str(), c.connected)).collect::<Vec<_>>(),
            [("Work: Vienna", "VPN", false), ("home-wg", "WireGuard", true)]
        );
    }

    #[test]
    fn windows_vpns_are_listed_one_or_many() {
        let one = r#"{"Name":"Work","TunnelType":"Ikev2","ConnectionStatus":"Connected"}"#;
        let many = r#"[{"Name":"Work","TunnelType":"Automatic","ConnectionStatus":"Disconnected"},{"Name":"Lab","TunnelType":"Sstp","ConnectionStatus":"Connected"}]"#;
        assert_eq!(parse_windows(one).len(), 1);
        assert!(parse_windows(one)[0].connected);
        let list = parse_windows(many);
        assert_eq!((list[0].kind.as_str(), list[0].connected), ("VPN", false));
        assert_eq!((list[1].kind.as_str(), list[1].connected), ("Sstp", true));
        assert!(parse_windows("nonsense").is_empty());
    }
}
