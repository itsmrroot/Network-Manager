//! Changing an adapter's settings: IP addresses (IPv4 and IPv6), DNS
//! servers, MAC address, on/off, and the usual quick fixes.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[cfg(any(windows, target_os = "macos"))]
use anyhow::Context;
#[cfg(not(windows))]
use anyhow::bail;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::adapters::{Adapter, Kind, prefix_to_mask};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use crate::cmd;
#[cfg(windows)]
use crate::cmd::ps_quote;
#[cfg(target_os = "macos")]
use crate::cmd::{self, sh_quote};
use crate::mac::Mac;

/// How the adapter gets its IPv4 address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ipv4Mode {
    /// Automatically, from the router (DHCP).
    Dhcp,
    Static {
        address: Ipv4Addr,
        prefix: u8,
        gateway: Option<Ipv4Addr>,
    },
}

/// How the adapter gets its IPv6 address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Ipv6Mode {
    /// Left as it is.
    #[default]
    Unchanged,
    /// From the router (SLAAC or DHCPv6).
    Automatic,
    Static {
        address: Ipv6Addr,
        prefix: u8,
        gateway: Option<Ipv6Addr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpSettings {
    pub mode: Ipv4Mode,
    /// More IPv4 addresses on the same adapter, also next to DHCP: to reach
    /// a device on another subnet without losing this network.
    #[serde(default)]
    pub extra: Vec<(Ipv4Addr, u8)>,
    #[serde(default)]
    pub ipv6: Ipv6Mode,
    /// Empty: automatic (from DHCP).
    pub dns: Vec<IpAddr>,
}

/// At most this many extra addresses.
pub const MAX_EXTRA: usize = 16;

/// The name of the macOS network service that carries extra address `n`
/// (1, 2, …) of service `svc`.
pub fn alias_service(svc: &str, n: usize) -> String {
    format!("{svc} alias {n}")
}

/// Whether a macOS network service only carries an extra address.
pub fn is_alias_service(name: &str) -> bool {
    name.rsplit_once(" alias ").is_some_and(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

impl IpSettings {
    pub fn dhcp() -> Self {
        Self { mode: Ipv4Mode::Dhcp, extra: Vec::new(), ipv6: Ipv6Mode::Unchanged, dns: Vec::new() }
    }

    /// What the adapter uses now.
    pub fn current(a: &Adapter) -> Self {
        let mode = match (a.dhcp, a.main_ipv4()) {
            (Some(false), Some((address, prefix))) => Ipv4Mode::Static {
                address,
                prefix,
                gateway: match a.gateway {
                    Some(IpAddr::V4(g)) => Some(g),
                    _ => None,
                },
            },
            _ => Ipv4Mode::Dhcp,
        };
        // DNS servers handed out by DHCP are not "manual".
        let dns = if mode == Ipv4Mode::Dhcp { Vec::new() } else { a.dns.clone() };
        let main = a.main_ipv4();
        let extra =
            a.ipv4.iter().filter(|(ip, _)| !ip.is_link_local() && Some(*ip) != main.map(|m| m.0)).copied().collect();
        Self { mode, extra, ipv6: Ipv6Mode::Unchanged, dns }
    }

    /// One line: "Automatic (DHCP)" or "192.168.1.10/24 via 192.168.1.1".
    pub fn summary(&self) -> String {
        let ip = match &self.mode {
            Ipv4Mode::Dhcp => "Automatic (DHCP)".to_string(),
            Ipv4Mode::Static { address, prefix, gateway: Some(g) } => format!("{address}/{prefix} via {g}"),
            Ipv4Mode::Static { address, prefix, gateway: None } => format!("{address}/{prefix}, no gateway"),
        };
        let mut out = ip;
        if !self.extra.is_empty() {
            let more: Vec<String> = self.extra.iter().map(|(a, p)| format!("{a}/{p}")).collect();
            out += &format!(" + {}", more.join(", "));
        }
        match &self.ipv6 {
            Ipv6Mode::Unchanged => {}
            Ipv6Mode::Automatic => out += " · IPv6 automatic",
            Ipv6Mode::Static { address, prefix, gateway: Some(g) } => {
                out += &format!(" · IPv6 {address}/{prefix} via {g}")
            }
            Ipv6Mode::Static { address, prefix, gateway: None } => out += &format!(" · IPv6 {address}/{prefix}"),
        }
        if !self.dns.is_empty() {
            let dns: Vec<String> = self.dns.iter().map(IpAddr::to_string).collect();
            out += &format!(" · DNS {}", dns.join(", "));
        }
        out
    }

    /// Checks the settings before they are applied. The messages are meant
    /// for people.
    pub fn validate(&self) -> Result<()> {
        if let Ipv4Mode::Static { address, prefix, gateway } = &self.mode {
            let (address, prefix) = (*address, *prefix);
            usable(address, prefix)?;
            if let Some(g) = gateway {
                let mask = u32::from(prefix_to_mask(prefix));
                ensure!(*g != address, "The gateway cannot be this computer's own address.");
                ensure!(
                    u32::from(*g) & mask == u32::from(address) & mask,
                    "The gateway {g} is not in the subnet {}/{prefix}: it must be reachable directly.",
                    Ipv4Addr::from(u32::from(address) & mask)
                );
            }
        }
        ensure!(self.extra.len() <= MAX_EXTRA, "At most {MAX_EXTRA} more addresses can be added.");
        let main = match self.mode {
            Ipv4Mode::Static { address, .. } => Some(address),
            Ipv4Mode::Dhcp => None,
        };
        for (i, (a, p)) in self.extra.iter().enumerate() {
            usable(*a, *p)?;
            ensure!(Some(*a) != main && !self.extra[..i].iter().any(|(b, _)| b == a), "{a} is listed twice.");
        }
        if let Ipv6Mode::Static { address, prefix, gateway } = &self.ipv6 {
            ensure!((1..=128).contains(prefix), "The IPv6 prefix must be between /1 and /128.");
            ensure!(
                !address.is_unspecified() && !address.is_loopback() && !address.is_multicast(),
                "{address} cannot be used as the address of a computer."
            );
            if let Some(g) = gateway {
                ensure!(g != address, "The gateway cannot be this computer's own address.");
                ensure!(!g.is_multicast() && !g.is_unspecified(), "{g} cannot be a gateway.");
            }
        }
        ensure!(self.dns.len() <= 4, "At most 4 DNS servers can be set.");
        Ok(())
    }
}

/// `address`/`prefix` can be a computer's IPv4 address.
fn usable(address: Ipv4Addr, prefix: u8) -> Result<()> {
    ensure!((1..=32).contains(&prefix), "The subnet must be between /1 and /32.");
    ensure!(
        !address.is_unspecified() && !address.is_broadcast() && !address.is_multicast() && !address.is_loopback(),
        "{address} cannot be used as the address of a computer."
    );
    if prefix <= 30 {
        let mask = u32::from(prefix_to_mask(prefix));
        let ip = u32::from(address);
        ensure!(ip & !mask != 0, "{address} is the network address of /{prefix}: choose another address.");
        ensure!(ip & !mask != !mask, "{address} is the broadcast address of /{prefix}: choose another address.");
    }
    Ok(())
}

/// Well-known public DNS services: name, servers.
pub const DNS_PRESETS: &[(&str, &[&str])] = &[
    ("Cloudflare", &["1.1.1.1", "1.0.0.1"]),
    ("Cloudflare (blocks malware)", &["1.1.1.2", "1.0.0.2"]),
    ("Cloudflare (family)", &["1.1.1.3", "1.0.0.3"]),
    ("Google", &["8.8.8.8", "8.8.4.4"]),
    ("Quad9 (blocks malware)", &["9.9.9.9", "149.112.112.112"]),
    ("OpenDNS", &["208.67.222.222", "208.67.220.220"]),
    ("AdGuard (blocks ads)", &["94.140.14.14", "94.140.15.15"]),
];

/// Applies `s` to the adapter.
pub fn apply(a: &Adapter, s: &IpSettings) -> Result<()> {
    s.validate()?;
    imp::apply(a, s)
}

/// Gives the adapter the MAC address `mac`, or its own again with `None`.
pub fn set_mac(a: &Adapter, mac: Option<Mac>) -> Result<()> {
    if let Some(m) = mac {
        ensure!(!m.is_multicast(), "{m} is a multicast address: the first byte must be even.");
        ensure!(!m.is_zero() && !m.is_broadcast(), "{m} cannot be used as an adapter's address.");
        if cfg!(windows) && a.kind == Kind::WiFi {
            ensure!(
                m.is_local(),
                "Windows only accepts Wi-Fi addresses whose second digit is 2, 6, A or E (for example 02:…). \
                 Use \"Random\" or change the second digit."
            );
        }
    }
    imp::set_mac(a, mac)
}

/// The address the manufacturer gave the adapter, when the system knows it.
pub fn permanent_mac(a: &Adapter) -> Option<Mac> {
    imp::permanent_mac(a)
}

/// Turns the adapter on or off.
pub fn set_enabled(a: &Adapter, on: bool) -> Result<()> {
    imp::set_enabled(a, on)
}

/// Asks the router for a new address (DHCP release and renew).
pub fn renew(a: &Adapter) -> Result<()> {
    imp::renew(a)
}

/// Empties the system's DNS cache.
pub fn flush_dns() -> Result<()> {
    imp::flush_dns()
}

/// Resets the network stack (Windows: Winsock and TCP/IP, then a restart is
/// needed; Linux: restarts NetworkManager). Returns what to tell the user.
pub fn reset_network() -> Result<&'static str> {
    imp::reset_network()
}

// ---------------------------------------------------------------------------
// Windows: netsh and PowerShell (the app runs as administrator).

#[cfg(windows)]
mod imp {
    use super::*;

    fn netsh(args: &[&str]) -> Result<()> {
        cmd::run("netsh", args).map(drop)
    }

    pub fn apply(a: &Adapter, s: &IpSettings) -> Result<()> {
        let name = format!("name={}", a.name);
        match &s.mode {
            Ipv4Mode::Dhcp => {
                // Fails harmlessly when DHCP is already on.
                let _ = netsh(&["interface", "ipv4", "set", "address", &name, "source=dhcp"]);
                // Addresses added by hand earlier stay next to DHCP's.
                cmd::powershell(&format!(
                    "Get-NetIPAddress -InterfaceAlias {} -AddressFamily IPv4 -PrefixOrigin Manual -ErrorAction SilentlyContinue | \
                     Remove-NetIPAddress -Confirm:$false",
                    ps_quote(&a.name)
                ))?;
                if !s.extra.is_empty() {
                    // Windows 10 2004 and later keep manual addresses next to DHCP.
                    netsh(&[
                        "interface",
                        "ipv4",
                        "set",
                        "interface",
                        &format!("interface={}", a.name),
                        "dhcpstaticipcoexistence=enabled",
                    ])
                    .context("this version of Windows cannot add addresses next to DHCP: choose Manual")?;
                }
            }
            Ipv4Mode::Static { address, prefix, gateway } => {
                let gw = gateway.map_or("gateway=none".to_string(), |g| format!("gateway={g}"));
                netsh(&[
                    "interface",
                    "ipv4",
                    "set",
                    "address",
                    &name,
                    "source=static",
                    &format!("address={address}"),
                    &format!("mask={}", prefix_to_mask(*prefix)),
                    &gw,
                ])?;
            }
        }
        for (address, prefix) in &s.extra {
            netsh(&[
                "interface",
                "ipv4",
                "add",
                "address",
                &name,
                &format!("address={address}"),
                &format!("mask={}", prefix_to_mask(*prefix)),
            ])?;
        }
        if s.ipv6 != Ipv6Mode::Unchanged {
            let set = match &s.ipv6 {
                Ipv6Mode::Static { address, prefix, gateway } => format!(
                    "New-NetIPAddress -InterfaceAlias $n -AddressFamily IPv6 -IPAddress '{address}' -PrefixLength {prefix}{} | Out-Null",
                    gateway.map(|g| format!(" -DefaultGateway '{g}'")).unwrap_or_default()
                ),
                _ => "Set-NetIPInterface -InterfaceAlias $n -AddressFamily IPv6 -RouterDiscovery Enabled -Dhcp Enabled"
                    .to_string(),
            };
            // The addresses and default route set by hand before go first.
            cmd::powershell(&format!(
                "$ErrorActionPreference = 'Stop'; $n = {}\n\
                 Get-NetIPAddress -InterfaceAlias $n -AddressFamily IPv6 -PrefixOrigin Manual -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false\n\
                 Get-NetRoute -InterfaceAlias $n -DestinationPrefix '::/0' -ErrorAction SilentlyContinue | Where-Object {{ $_.Protocol -eq 'NetMgmt' }} | Remove-NetRoute -Confirm:$false\n\
                 {set}",
                ps_quote(&a.name)
            ))
            .context("the IPv6 settings could not be applied")?;
        }
        for (family, servers) in [
            ("ipv4", s.dns.iter().filter(|d| d.is_ipv4()).collect::<Vec<_>>()),
            ("ipv6", s.dns.iter().filter(|d| d.is_ipv6()).collect::<Vec<_>>()),
        ] {
            if servers.is_empty() {
                if family == "ipv4" || s.dns.is_empty() {
                    let _ = netsh(&["interface", family, "set", "dnsservers", &name, "source=dhcp"]);
                }
                continue;
            }
            for (i, d) in servers.iter().enumerate() {
                if i == 0 {
                    netsh(&[
                        "interface",
                        family,
                        "set",
                        "dnsservers",
                        &name,
                        "source=static",
                        &format!("address={d}"),
                        "register=primary",
                        "validate=no",
                    ])?;
                } else {
                    netsh(&[
                        "interface",
                        family,
                        "add",
                        "dnsservers",
                        &name,
                        &format!("address={d}"),
                        &format!("index={}", i + 1),
                        "validate=no",
                    ])?;
                }
            }
        }
        Ok(())
    }

    /// The adapter's key under the network adapter class, found by its GUID.
    const FIND_KEY: &str = "$a = Get-NetAdapter -Name $name\n\
        $class = 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e972-e325-11ce-bfc1-08002be10318}'\n\
        $key = Get-ChildItem $class -ErrorAction SilentlyContinue | Where-Object {\n\
          (Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue).NetCfgInstanceId -eq $a.InterfaceGuid } | Select-Object -First 1\n\
        if (-not $key) { throw 'The settings of this adapter were not found in the registry.' }\n";

    pub fn set_mac(a: &Adapter, mac: Option<Mac>) -> Result<()> {
        let change = match mac {
            Some(m) => format!("Set-ItemProperty -Path $key.PSPath -Name NetworkAddress -Value '{}'", m.plain()),
            None => "Remove-ItemProperty -Path $key.PSPath -Name NetworkAddress -ErrorAction SilentlyContinue".into(),
        };
        let script = format!(
            "$name = {}\n{FIND_KEY}{change}\nRestart-NetAdapter -Name $name -Confirm:$false",
            ps_quote(&a.name)
        );
        cmd::powershell(&script).context("the MAC address could not be changed")?;
        Ok(())
    }

    pub fn permanent_mac(a: &Adapter) -> Option<Mac> {
        let script = format!("(Get-NetAdapter -Name {}).PermanentAddress", ps_quote(&a.name));
        cmd::powershell(&script).ok()?.trim().parse().ok()
    }

    pub fn set_enabled(a: &Adapter, on: bool) -> Result<()> {
        let state = if on { "admin=enabled" } else { "admin=disabled" };
        netsh(&["interface", "set", "interface", &format!("name={}", a.name), state])
    }

    pub fn renew(a: &Adapter) -> Result<()> {
        let _ = cmd::run("ipconfig", &["/release", &a.name]);
        cmd::run("ipconfig", &["/renew", &a.name]).map(drop)
    }

    pub fn flush_dns() -> Result<()> {
        cmd::run("ipconfig", &["/flushdns"]).map(drop)
    }

    pub fn reset_network() -> Result<&'static str> {
        cmd::run("netsh", &["winsock", "reset"])?;
        let _ = cmd::run("netsh", &["int", "ip", "reset"]);
        let _ = cmd::run("ipconfig", &["/flushdns"]);
        Ok("The network settings were reset. Restart the computer to finish.")
    }
}

// ---------------------------------------------------------------------------
// macOS: networksetup and ifconfig, with the password dialog.

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    fn service(a: &Adapter) -> Result<&str> {
        a.service.as_deref().context("this adapter is not a network service in System Settings")
    }

    pub fn apply(a: &Adapter, s: &IpSettings) -> Result<()> {
        cmd::admin_sh(&script(service(a)?, s)?)?;
        Ok(())
    }

    /// The shell commands that apply `s` to network service `name`.
    pub fn script(name: &str, s: &IpSettings) -> Result<String> {
        let svc = sh_quote(name);
        let ip = match &s.mode {
            Ipv4Mode::Dhcp => format!("networksetup -setdhcp {svc}"),
            Ipv4Mode::Static { address, prefix, gateway } => {
                let mask = prefix_to_mask(*prefix);
                match gateway {
                    Some(g) => format!("networksetup -setmanual {svc} {address} {mask} {g}"),
                    None => format!("networksetup -setmanual {svc} {address} {mask}"),
                }
            }
        };
        let dns = if s.dns.is_empty() {
            "Empty".to_string()
        } else {
            s.dns.iter().map(IpAddr::to_string).collect::<Vec<_>>().join(" ")
        };
        // More addresses: a copy of the service per address, as System
        // Settings does it. The old copies go first.
        let mut aliases = format!(
            "networksetup -listallnetworkservices | tail -n +2 | sed 's/^\\*//' | while IFS= read -r s; do \
             case \"$s\" in ({}[0-9]*) networksetup -removenetworkservice \"$s\" 2>&1;; esac; done;",
            sh_quote(&format!("{name} alias "))
        );
        for (i, (address, prefix)) in s.extra.iter().enumerate() {
            let alias = sh_quote(&alias_service(name, i + 1));
            aliases += &format!(
                " networksetup -duplicatenetworkservice {svc} {alias} 2>&1; \
                 networksetup -setmanual {alias} {address} {} 2>&1; networksetup -setv6off {alias} 2>&1;",
                prefix_to_mask(*prefix)
            );
        }
        let v6 = match &s.ipv6 {
            Ipv6Mode::Unchanged => String::new(),
            Ipv6Mode::Automatic => format!("networksetup -setv6automatic {svc} 2>&1;"),
            Ipv6Mode::Static { address, prefix, gateway: Some(g) } => {
                format!("networksetup -setv6manual {svc} {address} {prefix} {g} 2>&1;")
            }
            Ipv6Mode::Static { gateway: None, .. } => bail!("macOS needs an IPv6 gateway for a manual IPv6 address"),
        };
        // networksetup prints its errors but exits with 0: fail on any output.
        Ok(format!(
            "out=$({ip} 2>&1; networksetup -setdnsservers {svc} {dns} 2>&1; {aliases} {v6}); \
             if [ -n \"$out\" ]; then echo \"$out\" >&2; exit 1; fi"
        ))
    }

    pub fn set_mac(a: &Adapter, mac: Option<Mac>) -> Result<()> {
        let target = match mac.or_else(|| permanent_mac(a)) {
            Some(m) => m,
            None => bail!("the original address of {} is not known", a.name),
        };
        let dev = sh_quote(&a.device);
        let m = target.to_string().to_lowercase();
        // Wi-Fi only accepts a new address while it is not joined to a
        // network: turn it off and on, and change it before it joins again.
        let script = if a.kind == Kind::WiFi {
            format!(
                "networksetup -setairportpower {dev} off; networksetup -setairportpower {dev} on; \
                 i=0; while [ $i -lt 20 ]; do ifconfig {dev} ether {m} 2>/dev/null && break; sleep 0.1; i=$((i+1)); done; \
                 ifconfig {dev} ether {m}"
            )
        } else {
            format!("ifconfig {dev} ether {m}")
        };
        cmd::admin_sh(&script).context("the MAC address could not be changed")?;
        // macOS may silently keep the old address.
        let now = cmd::run("ifconfig", &[&a.device]).unwrap_or_default();
        let applied = now
            .lines()
            .filter_map(|l| l.trim().strip_prefix("ether "))
            .any(|v| v.split_whitespace().next().and_then(|t| t.parse::<Mac>().ok()) == Some(target));
        ensure!(
            applied,
            "macOS did not accept the new address for {}. For Wi-Fi, you can also turn on \
             \"Private Wi-Fi address\" in System Settings → Wi-Fi → Details.",
            a.name
        );
        Ok(())
    }

    pub fn permanent_mac(a: &Adapter) -> Option<Mac> {
        // `networksetup -getmacaddress` reports the hardware address.
        let text = cmd::run("networksetup", &["-getmacaddress", &a.device]).ok()?;
        text.split_whitespace().find_map(|w| w.parse().ok())
    }

    pub fn set_enabled(a: &Adapter, on: bool) -> Result<()> {
        let state = if on { "on" } else { "off" };
        if a.kind == Kind::WiFi {
            // Allowed without a password.
            return cmd::run("networksetup", &["-setairportpower", &a.device, state]).map(drop);
        }
        let svc = sh_quote(service(a)?);
        cmd::admin_sh(&format!("networksetup -setnetworkserviceenabled {svc} {state}")).map(drop)
    }

    pub fn renew(a: &Adapter) -> Result<()> {
        let dev = sh_quote(&a.device);
        cmd::admin_sh(&format!("ipconfig set {dev} BOOTP && ipconfig set {dev} DHCP")).map(drop)
    }

    pub fn flush_dns() -> Result<()> {
        cmd::admin_sh("dscacheutil -flushcache; killall -HUP mDNSResponder").map(drop)
    }

    pub fn reset_network() -> Result<&'static str> {
        bail!("macOS has no network reset: turn the adapter off and on, or renew its address")
    }
}

// ---------------------------------------------------------------------------
// Linux: NetworkManager (nmcli), or `ip` when NetworkManager is not used.

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::cmd::sh_quote;

    /// Runs nmcli as the user; asks for the password if NetworkManager says
    /// the user is not allowed.
    fn nmcli(args: &[&str]) -> Result<String> {
        let out = cmd::output("nmcli", args)?;
        if out.success {
            return Ok(out.stdout);
        }
        let msg = out.message();
        if msg.contains("Not authorized") || msg.contains("permission") || msg.contains("Insufficient privileges") {
            let script =
                std::iter::once("nmcli".to_string()).chain(args.iter().map(|a| sh_quote(a))).collect::<Vec<_>>();
            return cmd::admin_sh(&script.join(" "));
        }
        bail!("{msg}")
    }

    fn uses_nm(a: &Adapter) -> bool {
        a.connection.is_some() && cmd::exists("nmcli")
    }

    pub fn apply(a: &Adapter, s: &IpSettings) -> Result<()> {
        let v4: Vec<String> = s.dns.iter().filter(|d| d.is_ipv4()).map(IpAddr::to_string).collect();
        let v6: Vec<String> = s.dns.iter().filter(|d| d.is_ipv6()).map(IpAddr::to_string).collect();
        if let (true, Some(conn)) = (uses_nm(a), &a.connection) {
            let mut args: Vec<String> = vec!["connection".into(), "modify".into(), conn.clone()];
            let extra: Vec<String> = s.extra.iter().map(|(a, p)| format!("{a}/{p}")).collect();
            match &s.mode {
                Ipv4Mode::Dhcp => {
                    // NetworkManager keeps listed addresses next to DHCP's.
                    args.extend(["ipv4.method".into(), "auto".into(), "ipv4.addresses".into(), extra.join(",")]);
                    args.extend(["ipv4.gateway".into(), String::new()]);
                }
                Ipv4Mode::Static { address, prefix, gateway } => {
                    let all: Vec<String> = std::iter::once(format!("{address}/{prefix}")).chain(extra).collect();
                    args.extend(["ipv4.method".into(), "manual".into()]);
                    args.extend(["ipv4.addresses".into(), all.join(",")]);
                    args.extend(["ipv4.gateway".into(), gateway.map(|g| g.to_string()).unwrap_or_default()]);
                }
            }
            match &s.ipv6 {
                Ipv6Mode::Unchanged => {}
                Ipv6Mode::Automatic => {
                    args.extend(["ipv6.method", "auto", "ipv6.addresses", "", "ipv6.gateway", ""].map(String::from))
                }
                Ipv6Mode::Static { address, prefix, gateway } => {
                    args.extend(["ipv6.method".into(), "manual".into()]);
                    args.extend(["ipv6.addresses".into(), format!("{address}/{prefix}")]);
                    args.extend(["ipv6.gateway".into(), gateway.map(|g| g.to_string()).unwrap_or_default()]);
                }
            }
            args.extend(["ipv4.dns".into(), v4.join(" ")]);
            args.extend(["ipv4.ignore-auto-dns".into(), if v4.is_empty() { "no" } else { "yes" }.into()]);
            args.extend(["ipv6.dns".into(), v6.join(" ")]);
            args.extend(["ipv6.ignore-auto-dns".into(), if v6.is_empty() { "no" } else { "yes" }.into()]);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            nmcli(&refs)?;
            nmcli(&["connection", "up", conn])?;
            return Ok(());
        }
        let dev = sh_quote(&a.device);
        let mut script = match &s.mode {
            Ipv4Mode::Dhcp => format!(
                "ip -4 addr flush dev {dev}; \
                 if command -v dhclient >/dev/null; then dhclient -r {dev}; dhclient {dev}; \
                 elif command -v dhcpcd >/dev/null; then dhcpcd -n {dev}; \
                 else echo 'No DHCP client (dhclient or dhcpcd) was found.' >&2; exit 1; fi"
            ),
            Ipv4Mode::Static { address, prefix, gateway } => {
                let mut s = format!("ip -4 addr flush dev {dev} && ip addr add {address}/{prefix} dev {dev}");
                if let Some(g) = gateway {
                    s += &format!(" && ip route replace default via {g} dev {dev}");
                }
                s
            }
        };
        for (address, prefix) in &s.extra {
            script += &format!(" && ip addr add {address}/{prefix} dev {dev}");
        }
        match &s.ipv6 {
            Ipv6Mode::Unchanged => {}
            Ipv6Mode::Automatic => {
                script += &format!(
                    " && ip -6 addr flush dev {dev} scope global permanent; sysctl -qw net.ipv6.conf.{}.accept_ra=1",
                    a.device
                )
            }
            Ipv6Mode::Static { address, prefix, gateway } => {
                script += &format!(" && ip -6 addr add {address}/{prefix} dev {dev}");
                if let Some(g) = gateway {
                    script += &format!(" && ip -6 route replace default via {g} dev {dev}");
                }
            }
        }
        if !s.dns.is_empty() {
            let all: Vec<String> = s.dns.iter().map(IpAddr::to_string).collect();
            script += &format!(" && resolvectl dns {dev} {}", all.join(" "));
        }
        cmd::admin_sh(&script)?;
        Ok(())
    }

    pub fn set_mac(a: &Adapter, mac: Option<Mac>) -> Result<()> {
        if let (true, Some(conn)) = (uses_nm(a), &a.connection) {
            let key = if a.kind == Kind::WiFi {
                "802-11-wireless.cloned-mac-address"
            } else {
                "802-3-ethernet.cloned-mac-address"
            };
            // "permanent" is the address the adapter was made with.
            let value = mac.map_or("permanent".to_string(), |m| m.to_string());
            nmcli(&["connection", "modify", conn, key, &value])?;
            nmcli(&["connection", "up", conn])?;
            return Ok(());
        }
        let target = match mac.or_else(|| permanent_mac(a)) {
            Some(m) => m,
            None => bail!("the original address of {} is not known", a.name),
        };
        let dev = sh_quote(&a.device);
        cmd::admin_sh(&format!(
            "ip link set dev {dev} down && ip link set dev {dev} address {target} && ip link set dev {dev} up"
        ))?;
        Ok(())
    }

    pub fn permanent_mac(a: &Adapter) -> Option<Mac> {
        // iproute2 shows "permaddr" once the address was changed.
        if let Ok(text) = cmd::run("ip", &["link", "show", &a.device]) {
            let mut words = text.split_whitespace();
            while let Some(w) = words.next() {
                if w == "permaddr" {
                    return words.next()?.parse().ok();
                }
            }
        }
        if let Ok(text) = cmd::run("ethtool", &["-P", &a.device]) {
            return text.split_whitespace().last()?.parse().ok().filter(|m: &Mac| !m.is_zero());
        }
        a.mac
    }

    pub fn set_enabled(a: &Adapter, on: bool) -> Result<()> {
        if cmd::exists("nmcli") {
            // Wi-Fi: the radio, as the system menu does; a disconnected
            // device would look the same as one without a network.
            if a.kind == Kind::WiFi && nmcli(&["radio", "wifi", if on { "on" } else { "off" }]).is_ok() {
                return Ok(());
            }
            let verb = if on { "connect" } else { "disconnect" };
            if nmcli(&["device", verb, &a.device]).is_ok() {
                return Ok(());
            }
        }
        let state = if on { "up" } else { "down" };
        cmd::admin_sh(&format!("ip link set dev {} {state}", sh_quote(&a.device))).map(drop)
    }

    pub fn renew(a: &Adapter) -> Result<()> {
        if let (true, Some(conn)) = (uses_nm(a), &a.connection) {
            return nmcli(&["connection", "up", conn]).map(drop);
        }
        let dev = sh_quote(&a.device);
        cmd::admin_sh(&format!(
            "if command -v dhclient >/dev/null; then dhclient -r {dev}; dhclient {dev}; \
             elif command -v dhcpcd >/dev/null; then dhcpcd -n {dev}; \
             else echo 'No DHCP client (dhclient or dhcpcd) was found.' >&2; exit 1; fi"
        ))
        .map(drop)
    }

    pub fn flush_dns() -> Result<()> {
        for (prog, args) in [("resolvectl", &["flush-caches"][..]), ("systemd-resolve", &["--flush-caches"][..])] {
            if cmd::exists(prog) {
                if cmd::run(prog, args).is_ok() {
                    return Ok(());
                }
                return cmd::admin_sh(&format!("{prog} {}", args.join(" "))).map(drop);
            }
        }
        if cmd::exists("nscd") {
            return cmd::admin_sh("nscd -i hosts").map(drop);
        }
        bail!("this system keeps no DNS cache (systemd-resolved or nscd), so there is nothing to empty")
    }

    pub fn reset_network() -> Result<&'static str> {
        ensure!(cmd::exists("systemctl"), "the network service could not be found");
        cmd::admin_sh("systemctl restart NetworkManager || systemctl restart systemd-networkd")?;
        Ok("The network service was restarted.")
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub fn apply(_: &Adapter, _: &IpSettings) -> Result<()> {
        bail!("not supported on this system")
    }
    pub fn set_mac(_: &Adapter, _: Option<Mac>) -> Result<()> {
        bail!("not supported on this system")
    }
    pub fn permanent_mac(_: &Adapter) -> Option<Mac> {
        None
    }
    pub fn set_enabled(_: &Adapter, _: bool) -> Result<()> {
        bail!("not supported on this system")
    }
    pub fn renew(_: &Adapter) -> Result<()> {
        bail!("not supported on this system")
    }
    pub fn flush_dns() -> Result<()> {
        bail!("not supported on this system")
    }
    pub fn reset_network() -> Result<&'static str> {
        bail!("not supported on this system")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(a: &str, p: u8, g: Option<&str>) -> IpSettings {
        IpSettings {
            mode: Ipv4Mode::Static { address: a.parse().unwrap(), prefix: p, gateway: g.map(|g| g.parse().unwrap()) },
            extra: Vec::new(),
            ipv6: Ipv6Mode::Unchanged,
            dns: Vec::new(),
        }
    }

    #[test]
    fn validation() {
        assert!(fixed("192.168.1.10", 24, Some("192.168.1.1")).validate().is_ok());
        assert!(fixed("192.168.1.10", 24, None).validate().is_ok());
        assert!(fixed("10.0.0.1", 31, None).validate().is_ok());
        assert!(fixed("192.168.1.0", 24, None).validate().is_err());
        assert!(fixed("192.168.1.255", 24, None).validate().is_err());
        assert!(fixed("192.168.1.10", 24, Some("192.168.2.1")).validate().is_err());
        assert!(fixed("192.168.1.10", 24, Some("192.168.1.10")).validate().is_err());
        assert!(fixed("0.0.0.0", 24, None).validate().is_err());
        assert!(fixed("224.0.0.1", 24, None).validate().is_err());
        assert!(fixed("192.168.1.10", 0, None).validate().is_err());
        assert!(IpSettings::dhcp().validate().is_ok());
    }

    #[test]
    fn more_addresses_and_ipv6() {
        let mut s = IpSettings::dhcp();
        s.extra = vec![("192.168.0.50".parse().unwrap(), 24), ("10.1.1.5".parse().unwrap(), 16)];
        assert!(s.validate().is_ok());
        s.extra.push(("192.168.0.50".parse().unwrap(), 24));
        assert!(s.validate().is_err());
        s.extra.pop();
        s.extra.push(("192.168.0.0".parse().unwrap(), 24));
        assert!(s.validate().is_err());
        s.extra.pop();
        s.ipv6 = Ipv6Mode::Static {
            address: "2001:db8::10".parse().unwrap(),
            prefix: 64,
            gateway: Some("fe80::1".parse().unwrap()),
        };
        assert!(s.validate().is_ok());
        assert_eq!(s.summary(), "Automatic (DHCP) + 192.168.0.50/24, 10.1.1.5/16 · IPv6 2001:db8::10/64 via fe80::1");
        s.ipv6 = Ipv6Mode::Static { address: "ff02::1".parse().unwrap(), prefix: 64, gateway: None };
        assert!(s.validate().is_err());
        // Settings saved before these existed still load.
        let old: IpSettings = serde_json::from_str(r#"{"mode":"Dhcp","dns":[]}"#).unwrap();
        assert_eq!(old, IpSettings::dhcp());
        assert!(is_alias_service(&alias_service("Ethernet", 2)) && !is_alias_service("Ethernet"));
    }

    /// The macOS commands are valid shell, with names that need quoting.
    #[test]
    #[cfg(target_os = "macos")]
    fn macos_script_is_valid_shell() {
        let mut s = fixed("192.168.1.10", 24, Some("192.168.1.1"));
        s.extra = vec![("10.0.0.5".parse().unwrap(), 8)];
        s.ipv6 = Ipv6Mode::Static {
            address: "2001:db8::5".parse().unwrap(),
            prefix: 64,
            gateway: Some("2001:db8::1".parse().unwrap()),
        };
        let script = imp::script("USB 10/100 LAN \"x\" 'y'", &s).unwrap();
        assert!(script.contains("-duplicatenetworkservice 'USB 10/100 LAN \"x\" '\\''y'\\''' 'USB 10/100 LAN"));
        let out = std::process::Command::new("sh").args(["-n", "-c", &script]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        // The pattern that finds earlier copies, run on a sample list.
        // Only the loop, with networksetup swapped for printf and echo: nothing is changed.
        let start = script.find("networksetup -listallnetworkservices").unwrap();
        let find = script[start..].split("; done;").next().unwrap().replace("networksetup -listallnetworkservices", "printf 'An asterisk\\nUSB 10/100 LAN \"x\" '\\''y'\\'' alias 1\\n*Wi-Fi\\nUSB 10/100 LAN \"x\" '\\''y'\\''\\n'").replace("networksetup -removenetworkservice", "echo removes") + "; done";
        assert!(!find.contains("networksetup"), "{find}");
        let out = std::process::Command::new("sh").args(["-c", &find]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("removes USB 10/100 LAN \"x\" 'y' alias 1") && text.lines().count() == 1, "{text}");
        s.ipv6 = Ipv6Mode::Static { address: "2001:db8::5".parse().unwrap(), prefix: 64, gateway: None };
        assert!(imp::script("Ethernet", &s).is_err());
    }

    #[test]
    fn summaries() {
        assert_eq!(IpSettings::dhcp().summary(), "Automatic (DHCP)");
        let mut s = fixed("192.168.1.10", 24, Some("192.168.1.1"));
        s.dns = vec!["1.1.1.1".parse().unwrap()];
        assert_eq!(s.summary(), "192.168.1.10/24 via 192.168.1.1 · DNS 1.1.1.1");
    }

    #[test]
    fn presets_are_addresses() {
        for (_, servers) in DNS_PRESETS {
            for s in *servers {
                s.parse::<IpAddr>().unwrap();
            }
        }
    }
}
