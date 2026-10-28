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

use anyhow::{Context, Result, bail, ensure};

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
    // The desktop app ignores the hang-up signal; a program it starts must not
    // inherit that (an ignored signal stays ignored through exec).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `signal` is async-signal-safe, and nothing else is done
        // between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGHUP, libc::SIG_DFL);
                Ok(())
            });
        }
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
    Ok(Output { success: out.status.success(), stdout: text(&out.stdout), stderr: text(&out.stderr) })
}

/// Like [`output`], but a program that has not finished after `limit` is
/// stopped: a hung helper must not hang the app.
pub fn output_timeout(program: &str, args: &[&str], limit: std::time::Duration) -> Result<Output> {
    use std::io::Read;
    use std::time::Instant;
    let mut child = command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {program}"))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} did not answer within {} seconds", limit.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    Ok(Output {
        success: status.success(),
        stdout: text(&out.join().unwrap_or_default()),
        stderr: text(&err.join().unwrap_or_default()),
    })
}

/// [`run`] with a deadline.
pub fn run_timeout(program: &str, args: &[&str], limit: std::time::Duration) -> Result<String> {
    let out = output_timeout(program, args, limit)?;
    if !out.success {
        bail!("{}", nonempty(out.message(), program));
    }
    Ok(out.stdout)
}

/// What a program printed, as text. Windows' own tools (netsh, ping,
/// ipconfig) print in the console's code page (850, 866, 936…), not UTF-8;
/// PowerShell is told to print UTF-8.
pub fn text(bytes: &[u8]) -> String {
    #[cfg(windows)]
    if std::str::from_utf8(bytes).is_err() {
        use windows_sys::Win32::Globalization::{CP_OEMCP, MultiByteToWideChar};
        let Ok(len) = i32::try_from(bytes.len()) else { return String::from_utf8_lossy(bytes).into_owned() };
        // SAFETY: the buffers and their lengths match; the first call only
        // measures.
        unsafe {
            let n = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
            if n > 0 {
                let mut wide = vec![0u16; n as usize];
                let n = MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), n);
                if n > 0 {
                    return String::from_utf16_lossy(&wide[..n as usize]);
                }
            }
        }
    }
    String::from_utf8_lossy(bytes).trim_start_matches('\u{feff}').to_string()
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
    // Our own helper's messages already start with "error: ".
    let msg = msg.trim_start_matches("error: ");
    if msg.is_empty() { format!("{program} failed") } else { msg.to_string() }
}

/// Whether this process has administrator (root) rights.
pub fn is_admin() -> bool {
    #[cfg(unix)]
    // SAFETY: geteuid has no preconditions and cannot fail.
    return unsafe { libc::geteuid() } == 0;
    #[cfg(windows)]
    {
        // Asked once: the process's rights do not change while it runs.
        static ADMIN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        return *ADMIN.get_or_init(|| {
            use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
            use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
            // SAFETY: the token handle is closed after use; the buffer is a
            // TOKEN_ELEVATION of the size given.
            unsafe {
                let mut token: HANDLE = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                    return false;
                }
                let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
                let mut len = 0u32;
                let ok = GetTokenInformation(
                    token,
                    TokenElevation,
                    (&raw mut e).cast(),
                    std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                    &mut len,
                );
                CloseHandle(token);
                ok != 0 && e.TokenIsElevated != 0
            }
        });
    }
    #[allow(unreachable_code)]
    false
}

/// The command line `netmgr` next to this program (the desktop app runs it
/// with administrator rights for work that needs them).
pub fn helper_exe() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) { "netmgr.exe" } else { "netmgr" };
    if exe.file_name().is_some_and(|n| n == name) {
        return Ok(exe);
    }
    let sibling = exe.with_file_name(name);
    ensure!(sibling.is_file(), "{} is missing next to the app: reinstall Network Manager", name);
    // Inside an AppImage the files live on a FUSE mount that root cannot
    // read: hand root a copy.
    #[cfg(unix)]
    if std::env::var_os("APPIMAGE").is_some() {
        use std::os::unix::fs::PermissionsExt;
        static COPY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        if let Some(copy) = COPY.get() {
            return Ok(copy.clone());
        }
        let copy = private_dir("netmgr-helper")?.join(name);
        std::fs::copy(&sibling, &copy)?;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755))?;
        return Ok(COPY.get_or_init(|| copy).clone());
    }
    Ok(sibling)
}

/// A new, empty temporary folder that only this user can open, with a name
/// nobody can guess. Never reuses a folder that already exists (`/tmp` is
/// shared on Linux: another user could have prepared one with links in it).
pub fn private_dir(prefix: &str) -> Result<std::path::PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    for _ in 0..16 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{:016x}", std::process::id(), h.finish()));
        #[allow(unused_mut)]
        let mut b = std::fs::DirBuilder::new();
        // On Windows the temporary folder is the user's own already.
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        match b.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("could not create {}", dir.display())),
        }
    }
    bail!("could not create a temporary folder")
}

/// Windows: an inbound firewall rule for this program on one port, for as
/// long as a server runs (removed when dropped). A lab cable counts as a
/// "Public" network, where Windows' firewall otherwise drops what the
/// server waits for. `protocol` is "UDP" or "TCP"; "any" with port 0 is the
/// whole program (packet capture). Nothing on other systems.
pub struct FirewallHole {
    /// Adding the rule (netsh is slow): the server does not wait for it.
    #[cfg(windows)]
    adding: Option<std::thread::JoinHandle<Option<String>>>,
}

impl FirewallHole {
    pub fn open(protocol: &str, port: u16) -> Self {
        #[cfg(windows)]
        {
            let protocol = protocol.to_string();
            let adding = std::thread::spawn(move || {
                let exe = std::env::current_exe().ok()?;
                clear_blocks(&exe);
                let name = if port == 0 {
                    "name=Network Manager capture".to_string()
                } else {
                    format!("name=Network Manager {protocol} {port}")
                };
                let _ = output("netsh", &["advfirewall", "firewall", "delete", "rule", &name]);
                let program = format!("program={}", exe.display());
                let (proto, local) = (format!("protocol={protocol}"), format!("localport={port}"));
                let mut args = vec![
                    "advfirewall",
                    "firewall",
                    "add",
                    "rule",
                    &name,
                    "dir=in",
                    "action=allow",
                    &program,
                    "profile=any",
                ];
                if port != 0 {
                    args.extend([proto.as_str(), local.as_str()]);
                }
                output("netsh", &args).is_ok_and(|o| o.success).then_some(name)
            });
            Self { adding: Some(adding) }
        }
        #[cfg(not(windows))]
        {
            let _ = (protocol, port);
            Self {}
        }
    }
}

impl Drop for FirewallHole {
    fn drop(&mut self) {
        // Waits for the rule to be there, then removes it.
        #[cfg(windows)]
        if let Some(Ok(Some(name))) = self.adding.take().map(std::thread::JoinHandle::join) {
            let _ = output("netsh", &["advfirewall", "firewall", "delete", "rule", &name]);
        }
    }
}

/// Windows' first-run prompt ("Allow access") ticks private networks only and
/// adds a rule that blocks this program on public ones, which outranks any
/// rule that allows it: removed once per run, when a server starts.
#[cfg(windows)]
fn clear_blocks(exe: &std::path::Path) {
    static DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    DONE.get_or_init(|| {
        let _ = powershell(&format!(
            "Get-NetFirewallApplicationFilter -Program {} -ErrorAction SilentlyContinue | Get-NetFirewallRule | \
             Where-Object {{ $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }} | Remove-NetFirewallRule",
            ps_quote(&exe.display().to_string())
        ));
    });
}

/// Single-quotes `s` for `sh`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Single-quotes `s` for PowerShell, which also takes the curly quotes
/// (‘ ’ ‚ ‛) as single quotes.
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '‘' | '’' | '‚' | '‛') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Writes a file only this user can read (passwords, device configurations).
pub fn write_private(path: &std::path::Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(path)?;
    f.write_all(data.as_ref())
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
    let errors = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut { stderr }, &mut out);
        text(&out).lines().map(str::to_string).collect::<Vec<_>>()
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
            let text = text(&buf).trim_end().to_string();
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
        // PowerShell ends a string at a curly quote too.
        assert_eq!(ps_quote("a’; calc; ’"), "'a’’; calc; ’’'");
    }

    /// Runs on Windows in CI (as administrator): the rule is there while the
    /// hole is open, for this port only, and gone afterwards.
    #[cfg(windows)]
    #[test]
    fn firewall_hole_opens_and_closes() {
        let name = "name=Network Manager UDP 47123";
        let show = || output("netsh", &["advfirewall", "firewall", "show", "rule", name, "verbose"]).unwrap();
        let hole = FirewallHole::open("UDP", 47123);
        // Added in the background.
        let mut out = show();
        for _ in 0..40 {
            if out.success {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            out = show();
        }
        assert!(out.success && out.stdout.contains("47123"), "{}", out.stdout);
        drop(hole);
        assert!(!show().success);
    }

    #[test]
    fn private_dirs_are_new_and_closed() {
        let a = private_dir("netmgr-test").unwrap();
        let b = private_dir("netmgr-test").unwrap();
        assert_ne!(a, b);
        assert!(std::fs::read_dir(&a).unwrap().next().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
            write_private(&a.join("secret"), "x").unwrap();
            assert_eq!(std::fs::metadata(a.join("secret")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_hangs_is_stopped() {
        use std::time::{Duration, Instant};
        let started = Instant::now();
        let e = output_timeout("sleep", &["30"], Duration::from_millis(300)).expect_err("it must not finish");
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
        // One that answers is read in full, with its failure.
        let ok =
            output_timeout("sh", &["-c", "echo out; echo err >&2; exit 3"], Duration::from_secs(10)).expect("runs");
        assert!(!ok.success && ok.stdout.trim() == "out" && ok.stderr.trim() == "err");
    }
}
