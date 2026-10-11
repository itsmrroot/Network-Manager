//! What a profile can switch besides IP settings: the proxy, the default
//! printer and network drives — the things that change with the place,
//! like the office's proxy and printer and the file server's shares.

use anyhow::{Context, Result, bail, ensure};
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

/// Characters a Windows share path may have here: letters and digits of any
/// language, and `\\ ._-$#()+,@~[]`. Nothing that quotes or chains commands
/// in cmd or PowerShell (the paths come from profile files).
fn plain_share(unc: &str) -> bool {
    unc.chars().all(|c| c.is_alphanumeric() || " \\._-$#()+,@~[]".contains(c))
}

/// Windows: runs `net use` through a one-off scheduled task as the user whose
/// desktop this app is on, without administrator rights, so that the drive
/// appears in Explorer. `letter` is "X:" and `unc` passed [`plain_share`].
fn map_as_user(letter: &str, unc: &str) -> Result<()> {
    let ps = format!(
        r#"$ErrorActionPreference = 'Stop'
# The user who sees this session's desktop: the owner of its explorer.exe.
$session = (Get-Process -Id $PID).SessionId
$explorer = Get-CimInstance Win32_Process -Filter "Name='explorer.exe' AND SessionId=$session" | Select-Object -First 1
if (-not $explorer) {{ throw 'no signed-in user' }}
$sid = (Invoke-CimMethod -InputObject $explorer -MethodName GetOwnerSid).Sid
if (-not $sid) {{ throw 'no signed-in user' }}
# A file that user may write, whoever this administrator is.
$out = Join-Path $env:PUBLIC ('netmgr-drive-' + [guid]::NewGuid().ToString() + '.txt')
New-Item -ItemType File -Path $out | Out-Null
icacls $out /grant ('*' + $sid + ':M') | Out-Null
$cmd = '/c net use {letter} /delete /y >nul 2>&1 & net use {letter} "{unc}" /persistent:no <nul >"' + $out + '" 2>&1'
$name = 'Network Manager drive ' + [guid]::NewGuid().ToString()
$action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument $cmd
$principal = New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 2)
Register-ScheduledTask -TaskName $name -Action $action -Principal $principal -Settings $settings | Out-Null
try {{
    Start-ScheduledTask -TaskName $name
    for ($i = 0; $i -lt 240; $i++) {{
        Start-Sleep -Milliseconds 250
        $info = Get-ScheduledTaskInfo -TaskName $name
        if ((Get-ScheduledTask -TaskName $name).State -ne 'Running' -and $info.LastTaskResult -ne 267011) {{ break }}
    }}
}} finally {{
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
}}
$text = if (Test-Path $out) {{ "$(Get-Content $out -Raw -Encoding Oem)".Trim() }} else {{ '' }}
Remove-Item $out -ErrorAction SilentlyContinue
if ($info.LastTaskResult -ne 0) {{ throw ('{{0}} (0x{{1:X}})' -f $text, $info.LastTaskResult) }}"#
    );
    cmd::powershell(&ps)?;
    Ok(())
}

fn connect_drive(d: &Drive) -> Result<()> {
    let (server, share) = split_share(&d.path)?;
    if cfg!(windows) {
        let unc = format!("\\\\{server}\\{}", share.replace('/', "\\"));
        let letter = d.letter.trim().trim_end_matches('\\');
        ensure!(plain_share(&unc), "the share's name has characters that are not allowed here");
        ensure!(
            letter.is_empty()
                || (letter.len() == 2 && letter.ends_with(':') && letter.as_bytes()[0].is_ascii_alphabetic()),
            "the drive letter must look like Z:"
        );
        // The app runs as administrator, and Windows keeps an administrator's
        // drives apart from Explorer's: map it as the signed-in user instead.
        if !letter.is_empty() {
            match map_as_user(letter, &unc) {
                Ok(()) => return Ok(()),
                Err(e) if !e.to_string().contains("no signed-in user") => return Err(e),
                Err(_) => {}
            }
        }
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

#[derive(Clone, Copy)]
enum WinProxy {
    Direct,
    Manual,
    Script,
}

/// Sets Windows' proxy the way Internet Options does (every program that
/// asks Windows for it, Edge and Chrome too), and tells running programs.
/// Writing only the registry would leave them on the old setting.
fn windows_proxy(mode: WinProxy, server: &str, bypass: &str, script: &str) -> Result<()> {
    // INTERNET_PER_CONN_FLAGS: 1 direct, 2 proxy, 4 configuration script,
    // 8 detect automatically (Windows' default, kept with "no proxy").
    let flags = match mode {
        WinProxy::Direct => 9,
        WinProxy::Manual => 3,
        WinProxy::Script => 5,
    };
    let ps = format!(
        r#"$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NetMgrProxy {{
    [StructLayout(LayoutKind.Sequential)] struct Opt {{ public int Option; public IntPtr Value; }}
    [StructLayout(LayoutKind.Sequential)] struct OptList {{ public int Size; public IntPtr Connection; public int Count; public int Error; public IntPtr Options; }}
    [DllImport("wininet.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool InternetSetOption(IntPtr h, int option, IntPtr buffer, int length);
    public static void Set(int flags, string server, string bypass, string script) {{
        string[] values = {{ null, server, bypass, script }};
        int size = Marshal.SizeOf(typeof(Opt));
        IntPtr opts = Marshal.AllocCoTaskMem(size * 4);
        IntPtr[] strings = new IntPtr[4];
        for (int i = 0; i < 4; i++) {{
            Opt o = new Opt();
            o.Option = i + 1;
            if (i == 0) {{ o.Value = new IntPtr(flags); }} else {{ strings[i] = Marshal.StringToHGlobalUni(values[i]); o.Value = strings[i]; }}
            Marshal.StructureToPtr(o, IntPtr.Add(opts, i * size), false);
        }}
        OptList l = new OptList();
        l.Size = Marshal.SizeOf(typeof(OptList));
        l.Count = 4;
        l.Options = opts;
        IntPtr pl = Marshal.AllocCoTaskMem(l.Size);
        Marshal.StructureToPtr(l, pl, false);
        bool ok = InternetSetOption(IntPtr.Zero, 75, pl, l.Size);
        int err = Marshal.GetLastWin32Error();
        InternetSetOption(IntPtr.Zero, 39, IntPtr.Zero, 0);
        InternetSetOption(IntPtr.Zero, 37, IntPtr.Zero, 0);
        Marshal.FreeCoTaskMem(pl);
        Marshal.FreeCoTaskMem(opts);
        for (int i = 1; i < 4; i++) {{ Marshal.FreeHGlobal(strings[i]); }}
        if (!ok) {{ throw new System.ComponentModel.Win32Exception(err); }}
    }}
}}
'@
[NetMgrProxy]::Set({flags}, {}, {}, {})"#,
        cmd::ps_quote(server),
        cmd::ps_quote(bypass),
        cmd::ps_quote(script)
    );
    cmd::powershell(&ps).context("the proxy could not be set")?;
    Ok(())
}

fn set_proxy(a: &Adapter, e: &Extras) -> Result<()> {
    let bypass: Vec<String> =
        e.proxy_bypass.split([',', ';', ' ']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    if cfg!(windows) {
        match e.proxy {
            ProxyMode::Keep => {}
            ProxyMode::Off => windows_proxy(WinProxy::Direct, "", "", "")?,
            ProxyMode::Manual => {
                let (h, p) = split_server(&e.proxy_server)?;
                let mut over = bypass.join(";");
                if !over.contains("<local>") {
                    over = if over.is_empty() { "<local>".into() } else { format!("{over};<local>") };
                }
                windows_proxy(WinProxy::Manual, &format!("{h}:{p}"), &over, "")?;
            }
            ProxyMode::Script => windows_proxy(WinProxy::Script, "", "", e.proxy_script.trim())?,
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
            // This computer itself never goes through the proxy (GNOME's
            // own default list).
            let mut hosts: Vec<String> = ["localhost", "127.0.0.0/8", "::1"].map(String::from).to_vec();
            for b in &bypass {
                if !hosts.contains(b) {
                    hosts.push(b.clone());
                }
            }
            let list = format!("[{}]", hosts.iter().map(|b| format!("'{b}'")).collect::<Vec<_>>().join(", "));
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
    fn share_paths_cannot_carry_commands() {
        assert!(plain_share("\\\\nas\\Team files (2024)\\Ünterlagen"));
        for bad in [
            "\\\\srv\\a’;Start-Process calc;’",
            "\\\\srv\\a' x",
            "\\\\srv\\a\"&calc",
            "\\\\srv\\a%PATH%",
            "\\\\srv\\a;b",
        ] {
            assert!(!plain_share(bad), "{bad}");
        }
    }

    /// Runs on Windows in CI: the proxy as Windows reports it afterwards.
    #[cfg(windows)]
    #[test]
    fn windows_proxy_is_set_and_cleared() {
        let key = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";
        let query = |name: &str| cmd::run("reg", &["query", key, "/v", name]).unwrap_or_default();
        windows_proxy(WinProxy::Manual, "127.0.0.1:3128", "*.lab;<local>", "").unwrap();
        assert!(query("ProxyServer").contains("127.0.0.1:3128"), "{}", query("ProxyServer"));
        assert!(query("ProxyEnable").contains("0x1"), "{}", query("ProxyEnable"));
        windows_proxy(WinProxy::Direct, "", "", "").unwrap();
        assert!(query("ProxyEnable").contains("0x0"), "{}", query("ProxyEnable"));
    }

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
