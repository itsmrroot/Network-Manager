//! Running the system's own network tools, with and without administrator
//! rights.
//!
//! Settings are changed through the tools every system already has
//! (`netsh` and PowerShell on Windows, `networksetup` and `ifconfig` on
//! macOS, `nmcli` and `ip` on Linux), so that the system stays the owner of
//! its configuration.
//!
//! Administrator rights:
//! * Windows: the desktop app asks for them when it starts (see its
//!   manifest); the command line has to run in an administrator terminal.
//! * macOS: each change asks for the password through the system dialog
//!   (`osascript … with administrator privileges`); macOS remembers it for a
//!   few minutes.
//! * Linux: changes go through NetworkManager, which usually allows the
//!   desktop user; otherwise `pkexec` asks for the password.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

/// What a finished command printed.
#[derive(Debug, Clone)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// stderr, or stdout when stderr is empty: where tools put their errors.
    pub fn message(&self) -> String {
        let msg = if self.stderr.trim().is_empty() { &self.stdout } else { &self.stderr };
        msg.trim().to_string()
    }
}

/// A command without a console window flashing up on Windows.
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs `program` and collects its output, whether it succeeded or not.
pub fn output(program: &str, args: &[&str]) -> Result<Output> {
    let out = command(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {program}"))?;
    Ok(Output {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Runs `program` and returns what it printed; fails with its error message.
pub fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = output(program, args)?;
    if !out.success {
        bail!("{}", nonempty(out.message(), program));
    }
    Ok(out.stdout)
}

/// Whether `program` can be found.
pub fn exists(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        let p = dir.join(program);
        p.is_file() || (cfg!(windows) && p.with_extension("exe").is_file())
    })
}

fn nonempty(msg: String, program: &str) -> String {
    if msg.is_empty() { format!("{program} failed") } else { msg }
}

/// Whether this process has administrator (root) rights.
pub fn is_admin() -> bool {
    #[cfg(unix)]
    // SAFETY: geteuid has no preconditions and cannot fail.
    return unsafe { libc::geteuid() } == 0;
    #[cfg(windows)]
    {
        // `net session` only works for administrators.
        return output("net", &["session"]).is_ok_and(|o| o.success);
    }
    #[allow(unreachable_code)]
    false
}

/// Single-quotes `s` for `sh`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Single-quotes `s` for PowerShell.
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Runs a PowerShell script (Windows) and returns its output.
pub fn powershell(script: &str) -> Result<String> {
    // UTF-8 output, and errors as plain text instead of red records.
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8\n$ErrorActionPreference = 'Stop'\n\
         try {{\n{script}\n}} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"
    );
    run("powershell", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
}

/// The user closed the password dialog.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled: the password was not entered")
    }
}

impl std::error::Error for Cancelled {}

/// Runs the `sh` script with administrator rights (macOS and Linux), asking
/// for the password through the system dialog unless already root.
pub fn admin_sh(script: &str) -> Result<String> {
    if is_admin() {
        return run("/bin/sh", &["-c", script]);
    }
    if cfg!(target_os = "macos") {
        // The script travels as an argument, so it needs no AppleScript quoting.
        let out = output(
            "osascript",
            &[
                "-e",
                "on run argv",
                "-e",
                "do shell script (item 1 of argv) with administrator privileges",
                "-e",
                "end run",
                script,
            ],
        )?;
        if !out.success {
            let msg = out.message();
            if msg.contains("-128") || msg.contains("User canceled") {
                return Err(Cancelled.into());
            }
            // "execution error: <message> (1)"
            let msg = msg.split_once("execution error: ").map_or(msg.as_str(), |(_, m)| m);
            let msg = msg.rsplit_once(" (").map_or(msg, |(m, _)| m);
            bail!("{}", nonempty(msg.trim().to_string(), "the command"));
        }
        return Ok(out.stdout);
    }
    if cfg!(target_os = "linux") {
        if !exists("pkexec") {
            bail!("administrator rights are needed: run this with sudo");
        }
        let out = output("pkexec", &["/bin/sh", "-c", script])?;
        if !out.success {
            // pkexec: 126 = the dialog was dismissed, 127 = not authorised.
            if out.stderr.trim().is_empty() || out.stderr.contains("dismissed") {
                return Err(Cancelled.into());
            }
            bail!("{}", nonempty(out.message(), "the command"));
        }
        return Ok(out.stdout);
    }
    bail!("not supported on this system")
}

/// Whether `e` means the user cancelled the password dialog.
pub fn is_cancelled(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Cancelled>().is_some()
}

/// Runs `program`, handing each line it prints to `line` as it comes, until
/// it ends or `cancel` is set (then it is stopped). Used for ping and
/// traceroute.
pub fn stream(program: &str, args: &[&str], cancel: &AtomicBool, mut line: impl FnMut(&str)) -> Result<bool> {
    let mut child = command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {program}"))?;
    let stdout = child.stdout.take().context("no output")?;
    let stderr = child.stderr.take().context("no output")?;
    // stderr in the background, so that neither pipe can fill up and block.
    let errors = std::thread::spawn(move || BufReader::new(stderr).lines().map_while(Result::ok).collect::<Vec<_>>());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
            let text = String::from_utf8_lossy(&buf).trim_end().to_string();
            if tx.send(text).is_err() {
                break;
            }
            buf.clear();
        }
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(text) => line(&text),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait()?;
    for e in errors.join().unwrap_or_default() {
        if !e.trim().is_empty() {
            line(&e);
        }
    }
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(ps_quote("it's"), "'it''s'");
    }
}
