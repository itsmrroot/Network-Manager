//! What a profile can switch besides IP settings: the proxy, the default
//! printer and network drives — the things that change with the place,
//! like the office's proxy and printer and the file server's shares.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::adapters::Adapter;
use crate::cmd;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProxyMode {
    /// Leave the proxy as it is.
    #[default]
    Keep,
    /// No proxy.
    Off,
    /// A proxy server for web traffic.
    Manual,
    /// A configuration script (PAC) at a URL.
    Script,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Drive {
    /// Windows: the drive letter ("Z:"); empty for none. Ignored elsewhere.
    pub letter: String,
    /// `\\server\share` or `smb://server/share`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Extras {
    pub proxy: ProxyMode,
    /// "proxy.example.com:8080".
    pub proxy_server: String,
    /// Addresses that go direct: "*.local, 10.*".
    pub proxy_bypass: String,
    /// The PAC script URL.
    pub proxy_script: String,
    /// Empty: leave the default printer as it is.
    pub printer: String,
    pub drives: Vec<Drive>,
}

impl Extras {
    pub fn is_empty(&self) -> bool {
        self.proxy == ProxyMode::Keep
            && self.printer.trim().is_empty()
            && self.drives.iter().all(|d| d.path.trim().is_empty())
    }

    /// Applies everything set; returns what failed, if anything.
    pub fn apply(&self, a: &Adapter) -> Result<()> {
        let mut failed = Vec::new();
        if self.proxy != ProxyMode::Keep
            && let Err(e) = set_proxy(a, self)
        {
            failed.push(format!("proxy: {e:#}"));
        }
        if !self.printer.trim().is_empty()
            && let Err(e) = set_default_printer(self.printer.trim())
        {
            failed.push(format!("printer: {e:#}"));
        }
        for d in self.drives.iter().filter(|d| !d.path.trim().is_empty()) {
            if let Err(e) = connect_drive(d) {
                failed.push(format!("{}: {e:#}", d.path.trim()));
            }
        }
        if !failed.is_empty() {
            bail!("{}", failed.join("; "));
        }
        Ok(())
    }
}

/// "host:port" → (host, port), port 80 when missing.
pub fn split_server(s: &str) -> Result<(String, u16)> {
    let s = s.trim().trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/');
    if s.is_empty() {
        bail!("enter the proxy server, e.g. proxy.example.com:8080");
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => match p.parse() {
            Ok(port) => Ok((h.to_string(), port)),
            Err(_) => bail!("\"{p}\" is not a port"),
        },
        _ => Ok((s.to_string(), 80)),
    }
}

/// `\\server\share` and `smb://server/share` → (server, share path).
pub fn split_share(path: &str) -> Result<(String, String)> {
    let p = path.trim();
    let rest = p
        .strip_prefix("smb://")
        .or_else(|| p.strip_prefix("\\\\"))
        .or_else(|| p.strip_prefix("//"))
        .ok_or_else(|| anyhow::anyhow!("\"{p}\": write \\\\server\\share or smb://server/share"))?;
    let rest = rest.replace('\\', "/");
    match rest.split_once('/') {
        Some((server, share)) if !server.is_empty() && !share.trim_matches('/').is_empty() => {
            Ok((server.to_string(), share.trim_matches('/').to_string()))
        }
        _ => bail!("\"{p}\": the share name is missing"),
    }
}

/// The printers this computer knows.
pub fn printers() -> Vec<String> {
    let text = if cfg!(windows) {
        cmd::powershell("Get-Printer | Select-Object -ExpandProperty Name").unwrap_or_default()
    } else {
        cmd::run("lpstat", &["-e"]).unwrap_or_default()
    };
    let mut list: Vec<String> = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    list.sort_by_key(|p| p.to_lowercase());
    list.dedup();
    list
}

fn set_default_printer(name: &str) -> Result<()> {
    if cfg!(windows) {
        // Windows otherwise picks the last printer used.
        let script = format!(
            "Set-ItemProperty -Path 'HKCU:\\Software\\Microsoft\\Windows NT\\CurrentVersion\\Windows' -Name LegacyDefaultPrinterMode -Value 1; \
             $p = Get-CimInstance Win32_Printer | Where-Object {{ $_.Name -eq {} }}; \
             if (-not $p) {{ throw 'no printer with this name' }}; \
             Invoke-CimMethod -InputObject $p -MethodName SetDefaultPrinter | Out-Null",
            cmd::ps_quote(name)
        );
        cmd::powershell(&script)?;
    } else {
        cmd::run("lpoptions", &["-d", name])?;
    }
    Ok(())
}

fn connect_drive(d: &Drive) -> Result<()> {
    let (server, share) = split_share(&d.path)?;
    if cfg!(windows) {
        let unc = format!("\\\\{server}\\{}", share.replace('/', "\\"));
        let letter = d.letter.trim().trim_end_matches('\\');
        let mut args = vec!["use"];
        if !letter.is_empty() {
            // Free the letter first: it may point somewhere else.
            let _ = cmd::output("net", &["use", letter, "/delete", "/y"]);
            args.push(letter);
        }
        args.extend([unc.as_str(), "/persistent:no"]);
        let out = cmd::output("net", &args)?;
        if !out.success {
            bail!("{}", out.message());
        }
    } else if cfg!(target_os = "macos") {
        // Finder connects and asks for a password when needed.
        cmd::run("open", &[&format!("smb://{server}/{share}")])?;
    } else {
        cmd::run("gio", &["mount", &format!("smb://{server}/{share}")])?;
    }
    Ok(())
}

fn set_proxy(a: &Adapter, e: &Extras) -> Result<()> {
    let bypass: Vec<String> =
        e.proxy_bypass.split([',', ';', ' ']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    if cfg!(windows) {
        let key = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";
        let reg = |name: &str, kind: &str, value: &str| {
            cmd::run("reg", &["add", key, "/v", name, "/t", kind, "/d", value, "/f"])
        };
        match e.proxy {
            ProxyMode::Keep => {}
            ProxyMode::Off => {
                reg("ProxyEnable", "REG_DWORD", "0")?;
                let _ = cmd::run("reg", &["delete", key, "/v", "AutoConfigURL", "/f"]);
            }
            ProxyMode::Manual => {
                let (h, p) = split_server(&e.proxy_server)?;
                reg("ProxyServer", "REG_SZ", &format!("{h}:{p}"))?;
                let mut over = bypass.join(";");
                if !over.contains("<local>") {
                    over = if over.is_empty() { "<local>".into() } else { format!("{over};<local>") };
                }
                reg("ProxyOverride", "REG_SZ", &over)?;
                reg("ProxyEnable", "REG_DWORD", "1")?;
                let _ = cmd::run("reg", &["delete", key, "/v", "AutoConfigURL", "/f"]);
            }
            ProxyMode::Script => {
                reg("AutoConfigURL", "REG_SZ", e.proxy_script.trim())?;
                reg("ProxyEnable", "REG_DWORD", "0")?;
            }
        }
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let svc = cmd::sh_quote(
            a.service
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("this adapter is not a network service in System Settings"))?,
        );
        let script = match e.proxy {
            ProxyMode::Keep => return Ok(()),
            ProxyMode::Off => format!(
                "networksetup -setwebproxystate {svc} off; networksetup -setsecurewebproxystate {svc} off; networksetup -setautoproxystate {svc} off"
            ),
            ProxyMode::Manual => {
                let (h, p) = split_server(&e.proxy_server)?;
                let (h, by) = (
                    cmd::sh_quote(&h),
                    if bypass.is_empty() {
                        "Empty".to_string()
                    } else {
                        bypass.iter().map(|b| cmd::sh_quote(b)).collect::<Vec<_>>().join(" ")
                    },
                );
                format!(
                    "networksetup -setautoproxystate {svc} off; networksetup -setwebproxy {svc} {h} {p}; \
                     networksetup -setsecurewebproxy {svc} {h} {p}; networksetup -setproxybypassdomains {svc} {by}"
                )
            }
            ProxyMode::Script => format!(
                "networksetup -setwebproxystate {svc} off; networksetup -setsecurewebproxystate {svc} off; \
                 networksetup -setautoproxyurl {svc} {}",
                cmd::sh_quote(e.proxy_script.trim())
            ),
        };
        cmd::admin_sh(&script)?;
        return Ok(());
    }
    // Linux: the desktop's proxy settings (GNOME and others using gsettings).
    if !cmd::exists("gsettings") {
        bail!("gsettings is needed to change the proxy");
    }
    let set = |schema: &str, key: &str, value: &str| cmd::run("gsettings", &["set", schema, key, value]);
    match e.proxy {
        ProxyMode::Keep => {}
        ProxyMode::Off => {
            set("org.gnome.system.proxy", "mode", "none")?;
        }
        ProxyMode::Manual => {
            let (h, p) = split_server(&e.proxy_server)?;
            for scheme in ["http", "https"] {
                let schema = format!("org.gnome.system.proxy.{scheme}");
                set(&schema, "host", &h)?;
                set(&schema, "port", &p.to_string())?;
            }
            let list = format!("[{}]", bypass.iter().map(|b| format!("'{b}'")).collect::<Vec<_>>().join(", "));
            set("org.gnome.system.proxy", "ignore-hosts", &list)?;
            set("org.gnome.system.proxy", "mode", "manual")?;
        }
        ProxyMode::Script => {
            set("org.gnome.system.proxy", "autoconfig-url", e.proxy_script.trim())?;
            set("org.gnome.system.proxy", "mode", "auto")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_and_shares() {
        assert_eq!(split_server("proxy.corp:3128").unwrap(), ("proxy.corp".into(), 3128));
        assert_eq!(split_server("http://proxy.corp/").unwrap(), ("proxy.corp".into(), 80));
        assert!(split_server("proxy:abc").is_err());
        assert_eq!(split_share("\\\\files\\team\\docs").unwrap(), ("files".into(), "team/docs".into()));
        assert_eq!(split_share("smb://nas.local/backup/").unwrap(), ("nas.local".into(), "backup".into()));
        assert!(split_share("\\\\files").is_err());
        assert!(split_share("files/share").is_err());
        assert!(Extras::default().is_empty());
    }
}
