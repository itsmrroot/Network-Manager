//! Changing an adapter's settings: IP address, DNS servers, MAC address,
//! on/off, and the usual quick fixes.

use std::net::{IpAddr, Ipv4Addr};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpSettings {
    pub mode: Ipv4Mode,
    /// Empty: automatic (from DHCP).
    pub dns: Vec<IpAddr>,
}

impl IpSettings {
    pub fn dhcp() -> Self {
        Self { mode: Ipv4Mode::Dhcp, dns: Vec::new() }
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
        Self { mode, dns }
    }

    /// One line: "Automatic (DHCP)" or "192.168.1.10/24 via 192.168.1.1".
    pub fn summary(&self) -> String {
        let ip = match &self.mode {
            Ipv4Mode::Dhcp => "Automatic (DHCP)".to_string(),
            Ipv4Mode::Static { address, prefix, gateway: Some(g) } => format!("{address}/{prefix} via {g}"),
            Ipv4Mode::Static { address, prefix, gateway: None } => format!("{address}/{prefix}, no gateway"),
        };
        if self.dns.is_empty() {
            ip
        } else {
            let dns: Vec<String> = self.dns.iter().map(IpAddr::to_string).collect();
            format!("{ip} · DNS {}", dns.join(", "))
        }
    }

    /// Checks the settings before they are applied. The messages are meant
    /// for people.
    pub fn validate(&self) -> Result<()> {
        if let Ipv4Mode::Static { address, prefix, gateway } = &self.mode {
            let (address, prefix) = (*address, *prefix);
            ensure!((1..=32).contains(&prefix), "The subnet must be between /1 and /32.");
            ensure!(
                !address.is_unspecified()
                    && !address.is_broadcast()
                    && !address.is_multicast()
                    && !address.is_loopback(),
                "{address} cannot be used as the address of a computer."
            );
            if prefix <= 30 {
                let mask = u32::from(prefix_to_mask(prefix));
                let ip = u32::from(address);
                ensure!(ip & !mask != 0, "{address} is the network address of /{prefix}: choose another address.");
                ensure!(
                    ip & !mask != !mask,
                    "{address} is the broadcast address of /{prefix}: choose another address."
                );
            }
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
        ensure!(self.dns.len() <= 4, "At most 4 DNS servers can be set.");
        Ok(())
    }
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
        let svc = sh_quote(service(a)?);
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
        // networksetup prints its errors but exits with 0: fail on any output.
        let script = format!(
            "out=$({ip} 2>&1; networksetup -setdnsservers {svc} {dns} 2>&1); \
             if [ -n \"$out\" ]; then echo \"$out\" >&2; exit 1; fi"
        );
        cmd::admin_sh(&script)?;
        Ok(())
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
            match &s.mode {
                Ipv4Mode::Dhcp => {
                    args.extend(["ipv4.method", "auto", "ipv4.addresses", "", "ipv4.gateway", ""].map(String::from))
                }
                Ipv4Mode::Static { address, prefix, gateway } => {
                    args.extend(["ipv4.method".into(), "manual".into()]);
                    args.extend(["ipv4.addresses".into(), format!("{address}/{prefix}")]);
                    args.extend(["ipv4.gateway".into(), gateway.map(|g| g.to_string()).unwrap_or_default()]);
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
