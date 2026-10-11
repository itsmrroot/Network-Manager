//! This computer's own SSH server, so that other devices can sign in to it:
//! Remote Login on macOS, OpenSSH Server on Windows, sshd on Linux. It is a
//! service of the system and keeps running after the app is closed.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::cmd;

pub const PORT: u16 = 22;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// The SSH server is installed (macOS always has one).
    pub installed: bool,
    /// An SSH server answers on port 22 of this computer.
    pub running: bool,
    /// The account to sign in with.
    pub user: String,
}

/// What the SSH server is doing now.
pub fn status() -> Status {
    Status { installed: installed(), running: answers(), user: user() }
}

/// Does an SSH server answer on this computer ("SSH-2.0-…")?
pub fn answers() -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PORT));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(800)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 8];
    s.read(&mut buf).is_ok_and(|n| buf[..n].starts_with(b"SSH-"))
}

/// The name to type before the @: this account's.
pub fn user() -> String {
    let name = std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default();
    if name.is_empty() { "user".into() } else { name }
}

fn installed() -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    if cfg!(windows) {
        return cmd::powershell("[bool](Get-Service sshd -ErrorAction SilentlyContinue)")
            .is_ok_and(|o| o.trim().eq_ignore_ascii_case("true"));
    }
    ["/usr/sbin/sshd", "/usr/bin/sshd", "/sbin/sshd"].iter().any(|p| std::path::Path::new(p).exists())
}

/// Turns the SSH server on or off (asks for administrator rights), and
/// checks that it worked.
pub fn set(on: bool) -> Result<()> {
    imp::set(on)?;
    // Services take a moment to start and stop.
    let deadline = Instant::now() + Duration::from_secs(if on { 15 } else { 8 });
    while answers() != on {
        if Instant::now() > deadline {
            if cfg!(target_os = "macos") {
                bail!(
                    "the SSH server did not {}: turn Remote Login {} in System Settings → General → Sharing",
                    if on { "start" } else { "stop" },
                    if on { "on" } else { "off" }
                );
            }
            bail!("the SSH server did not {}", if on { "start" } else { "stop" });
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

/// Opens the system's own setting for it (macOS: Sharing).
pub fn open_settings() -> Result<()> {
    if cfg!(target_os = "macos") {
        cmd::command("open").arg("x-apple.systempreferences:com.apple.Sharing-Settings.extension").spawn()?;
    } else if cfg!(windows) {
        cmd::command("explorer").arg("ms-settings:optionalfeatures").spawn()?;
    } else {
        bail!("not available on this system");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub fn set(on: bool) -> Result<()> {
        // The same switch as Remote Login in System Settings.
        let script = if on {
            "launchctl enable system/com.openssh.sshd; launchctl bootstrap system /System/Library/LaunchDaemons/ssh.plist 2>/dev/null; true"
        } else {
            "launchctl bootout system/com.openssh.sshd 2>/dev/null; launchctl disable system/com.openssh.sshd; true"
        };
        cmd::admin_sh(script)?;
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    pub fn set(on: bool) -> Result<()> {
        let script = if on {
            // The first time, Windows downloads OpenSSH Server (a few minutes).
            "$ErrorActionPreference = 'Stop'
if (-not (Get-Service sshd -ErrorAction SilentlyContinue)) { Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null }
Set-Service sshd -StartupType Automatic
Start-Service sshd
$rule = Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue
if ($rule) { Set-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -Enabled True -Profile Any } else { New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22 -Profile Any | Out-Null }"
        } else {
            "$ErrorActionPreference = 'Stop'
Stop-Service sshd
Set-Service sshd -StartupType Manual
Disable-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue"
        };
        cmd::powershell(script).map_err(|e| {
            if e.to_string().contains("0x800f0954") {
                anyhow::anyhow!(
                    "Windows could not download OpenSSH Server: optional features on this PC are managed by your organisation (0x800f0954)"
                )
            } else {
                e
            }
        })?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    /// "ssh" on Debian and Ubuntu, "sshd" elsewhere.
    fn service() -> &'static str {
        let debian = ["/lib/systemd/system/ssh.service", "/usr/lib/systemd/system/ssh.service", "/etc/init.d/ssh"]
            .iter()
            .any(|p| std::path::Path::new(p).exists());
        if debian || cmd::exists("apt-get") { "ssh" } else { "sshd" }
    }

    pub fn set(on: bool) -> Result<()> {
        let svc = service();
        let script = if on {
            let install = if installed() {
                String::new()
            } else if cmd::exists("apt-get") {
                // Fresh package lists; waits for an update running in the background.
                "export DEBIAN_FRONTEND=noninteractive; apt-get -o DPkg::Lock::Timeout=120 update && \
                 apt-get -o DPkg::Lock::Timeout=120 install -y openssh-server && "
                    .into()
            } else if cmd::exists("dnf") {
                "dnf install -y openssh-server && ".into()
            } else if cmd::exists("zypper") {
                "zypper -n install openssh-server && ".into()
            } else if cmd::exists("pacman") {
                "pacman -Sy --noconfirm openssh && ".into()
            } else {
                bail!("install the openssh-server package first");
            };
            // Let it through the firewall: ufw keeps the rule even when off
            // (harmless); firewalld now and after restarts, without a reload
            // that would drop the user's own temporary rules.
            format!(
                "{install}systemctl enable --now {svc} && \
                 {{ ! command -v ufw >/dev/null || ufw allow 22/tcp >/dev/null; }} && \
                 {{ ! command -v firewall-cmd >/dev/null || ! firewall-cmd --state >/dev/null 2>&1 || \
                 {{ firewall-cmd --add-service=ssh >/dev/null && firewall-cmd --permanent --add-service=ssh >/dev/null; }}; }}"
            )
        } else {
            // Closes the firewall again too.
            format!(
                "systemctl disable --now {svc}.socket 2>/dev/null; systemctl disable --now {svc}; \
                 {{ ! command -v ufw >/dev/null || ufw delete allow 22/tcp >/dev/null 2>&1; }}; \
                 {{ ! command -v firewall-cmd >/dev/null || ! firewall-cmd --state >/dev/null 2>&1 || \
                 {{ firewall-cmd --remove-service=ssh >/dev/null 2>&1; firewall-cmd --permanent --remove-service=ssh >/dev/null 2>&1; }}; }}; true"
            )
        };
        cmd::admin_sh(&script)?;
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod imp {
    use super::*;

    pub fn set(_: bool) -> Result<()> {
        bail!("not supported on this system")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_banner() {
        // Whatever this computer runs: the answer is a plain yes or no, quickly.
        let t = Instant::now();
        let _ = answers();
        assert!(t.elapsed() < Duration::from_secs(4));
        assert!(!user().is_empty());
    }
}
