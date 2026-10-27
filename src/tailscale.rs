//! Tailscale, through its own command-line program: which devices are in the
//! tailnet, which exit node is used, and the switches to change that.
//!
//! The program is `tailscale` (on a Mac also the one inside the Tailscale
//! app). `NETMGR_TAILSCALE` names another one (for tests).

use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::cmd;

/// The `tailscale` program, if this computer has it.
pub fn binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NETMGR_TAILSCALE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if cmd::exists("tailscale") {
        return Some(PathBuf::from("tailscale"));
    }
    [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "/usr/bin/tailscale",
        r"C:\Program Files\Tailscale\tailscale.exe",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// A device in the tailnet (also this one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The name people gave it ("proxmox-server-homelab").
    pub name: String,
    /// "proxmox-server-homelab.tail1234.ts.net".
    pub dns: String,
    pub ips: Vec<IpAddr>,
    pub os: String,
    pub online: bool,
    /// This computer uses it as its exit node now.
    pub exit_in_use: bool,
    /// It offers to be an exit node.
    pub exit_option: bool,
    /// The owner's login ("me@github").
    pub owner: String,
}

impl Device {
    /// The Tailscale IPv4 address, else the first one.
    pub fn ip(&self) -> Option<IpAddr> {
        self.ips.iter().find(|ip| ip.is_ipv4()).or(self.ips.first()).copied()
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    /// Running, Stopped, NeedsLogin, Starting…
    pub state: String,
    pub tailnet: Option<String>,
    pub version: String,
    pub me: Device,
    pub peers: Vec<Device>,
    /// Allow reaching the local network while an exit node is used.
    pub allow_lan: Option<bool>,
}

impl Status {
    pub fn running(&self) -> bool {
        self.state == "Running"
    }

    /// The exit node in use.
    pub fn exit_node(&self) -> Option<&Device> {
        self.peers.iter().find(|p| p.exit_in_use)
    }

    /// The devices that offer to be exit nodes, those that are online first.
    pub fn exit_options(&self) -> Vec<&Device> {
        let mut v: Vec<&Device> = self.peers.iter().filter(|p| p.exit_option).collect();
        v.sort_by_key(|d| (!d.online, d.name.to_lowercase()));
        v
    }
}

fn string(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn device(v: &Value, users: Option<&Value>) -> Device {
    let ips = v
        .get("TailscaleIPs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|i| i.as_str()?.parse().ok()).collect())
        .unwrap_or_default();
    let dns = string(v, "DNSName").trim_end_matches('.').to_string();
    let name = match string(v, "HostName") {
        h if h.is_empty() => dns.split('.').next().unwrap_or_default().to_string(),
        h => h,
    };
    let owner = v
        .get("UserID")
        .and_then(Value::as_i64)
        .and_then(|id| users?.get(id.to_string())?.get("LoginName")?.as_str().map(str::to_string))
        .unwrap_or_default();
    Device {
        name,
        dns,
        ips,
        os: string(v, "OS"),
        online: flag(v, "Online"),
        exit_in_use: flag(v, "ExitNode"),
        exit_option: flag(v, "ExitNodeOption"),
        owner,
    }
}

/// The answer of `tailscale status --json`.
pub fn parse_status(json: &str) -> Result<Status> {
    let v: Value = serde_json::from_str(json).context("tailscale did not answer with JSON")?;
    let me = v.get("Self").context("no device information in the answer")?;
    let users = v.get("User");
    let mut peers: Vec<Device> = v
        .get("Peer")
        .and_then(Value::as_object)
        .map(|m| m.values().map(|p| device(p, users)).collect())
        .unwrap_or_default();
    peers.sort_by_key(|p| (!p.online, p.name.to_lowercase()));
    Ok(Status {
        state: string(&v, "BackendState"),
        tailnet: v.get("CurrentTailnet").map(|t| string(t, "Name")).filter(|n| !n.is_empty()),
        version: string(&v, "Version"),
        me: device(me, users),
        peers,
        allow_lan: None,
    })
}

/// Whether the preferences let the local network be reached through an exit
/// node (`tailscale debug prefs`).
pub fn parse_allow_lan(json: &str) -> Option<bool> {
    serde_json::from_str::<Value>(json).ok()?.get("ExitNodeAllowLANAccess")?.as_bool()
}

fn run(args: &[&str]) -> Result<String> {
    let Some(bin) = binary() else { bail!("Tailscale is not installed") };
    cmd::run(&bin.to_string_lossy(), args)
}

/// The state of the tailnet.
pub fn status() -> Result<Status> {
    let mut s = parse_status(&run(&["status", "--json"])?)?;
    s.allow_lan = run(&["debug", "prefs"]).ok().and_then(|p| parse_allow_lan(&p));
    Ok(s)
}

/// Connects to the tailnet.
pub fn up() -> Result<()> {
    run(&["up"]).map(drop)
}

/// Disconnects from the tailnet.
pub fn down() -> Result<()> {
    run(&["down"]).map(drop)
}

/// Uses `ip` (a Tailscale address) as the exit node; `None` stops using one.
pub fn set_exit_node(ip: Option<IpAddr>) -> Result<()> {
    let arg = format!("--exit-node={}", ip.map(|i| i.to_string()).unwrap_or_default());
    run(&["set", &arg]).map(drop)
}

/// Lets the local network be reached while an exit node is used, or not.
pub fn set_allow_lan(on: bool) -> Result<()> {
    run(&["set", &format!("--exit-node-allow-lan-access={on}")]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const STATUS: &str = r#"{
      "Version": "1.80.0",
      "BackendState": "Running",
      "CurrentTailnet": {"Name": "me@github", "MagicDNSSuffix": "tail1234.ts.net"},
      "Self": {"HostName": "macbook-pro-von-mbp", "DNSName": "macbook-pro-von-mbp.tail1234.ts.net.",
               "TailscaleIPs": ["100.92.141.34", "fd7a:115c:a1e0::c02b:4929"], "OS": "macOS", "Online": true, "UserID": 1},
      "Peer": {
        "nodekey:aaa": {"HostName": "proxmox-server-homelab", "DNSName": "proxmox-server-homelab.tail1234.ts.net.",
                        "TailscaleIPs": ["100.69.73.40"], "OS": "linux", "Online": true, "ExitNode": true, "ExitNodeOption": true, "UserID": 1},
        "nodekey:bbb": {"HostName": "iphone-16-pro", "DNSName": "iphone-16-pro.tail1234.ts.net.",
                        "TailscaleIPs": ["100.72.196.70"], "OS": "iOS", "Online": false, "UserID": 1}
      },
      "User": {"1": {"LoginName": "itsmrroot@github", "DisplayName": "Bashir"}}
    }"#;

    #[test]
    fn the_status_of_a_tailnet_is_read() {
        let s = parse_status(STATUS).expect("valid");
        assert!(s.running());
        assert_eq!(s.tailnet.as_deref(), Some("me@github"));
        assert_eq!(s.me.name, "macbook-pro-von-mbp");
        assert_eq!(s.me.ip(), Some("100.92.141.34".parse().unwrap()));
        assert_eq!(s.me.owner, "itsmrroot@github");
        // Online devices come first.
        assert_eq!(
            s.peers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["proxmox-server-homelab", "iphone-16-pro"]
        );
        assert_eq!(s.exit_node().map(|d| d.name.as_str()), Some("proxmox-server-homelab"));
        assert_eq!(s.exit_options().len(), 1);
        assert_eq!(s.peers[1].os, "iOS");
    }

    #[test]
    fn a_stopped_tailscale_without_peers_is_read_too() {
        let s =
            parse_status(r#"{"BackendState":"Stopped","Self":{"HostName":"mac","TailscaleIPs":[]}}"#).expect("valid");
        assert!(!s.running() && s.peers.is_empty() && s.me.ip().is_none());
        assert!(parse_status("not json").is_err());
    }

    #[test]
    fn the_lan_setting_is_read_from_the_preferences() {
        assert_eq!(parse_allow_lan(r#"{"ExitNodeAllowLANAccess": true}"#), Some(true));
        assert_eq!(parse_allow_lan(r#"{"Other": 1}"#), None);
    }

    /// A stand-in `tailscale` program: answers `status --json` and
    /// `debug prefs`, and writes down the arguments of everything else.
    #[cfg(unix)]
    #[test]
    fn the_switches_run_the_right_commands() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::cmd::private_dir("tailscale-test").expect("a private folder");
        let log = dir.join("calls.log");
        let script = dir.join("tailscale");
        let body = format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  \"status --json\") cat <<'EOF'\n{STATUS}\nEOF\n;;\n  \"debug prefs\") echo '{{\"ExitNodeAllowLANAccess\": true}}';;\n  *) echo \"$@\" >> {log}; exit 0;;\nesac\n",
            log = log.display()
        );
        std::fs::write(&script, body).expect("the script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("executable");
        // The only test that sets it.
        unsafe { std::env::set_var("NETMGR_TAILSCALE", &script) };
        let s = status().expect("the stand-in answers");
        assert_eq!(s.allow_lan, Some(true));
        assert_eq!(s.exit_node().map(|d| d.name.as_str()), Some("proxmox-server-homelab"));
        set_exit_node(Some("100.69.73.40".parse().unwrap())).expect("set");
        set_exit_node(None).expect("clear");
        set_allow_lan(false).expect("lan");
        up().expect("up");
        down().expect("down");
        let calls = std::fs::read_to_string(&log).expect("the log");
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            [
                "set --exit-node=100.69.73.40",
                "set --exit-node=",
                "set --exit-node-allow-lan-access=false",
                "up",
                "down"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
