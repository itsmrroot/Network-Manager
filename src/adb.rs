//! Android devices through the system's own `adb` (Android Debug Bridge): the
//! list of devices, wireless pairing, a shell, logcat and a few everyday
//! actions. `adb` belongs to Google's platform-tools, whose license does not
//! allow handing it out with another program, so it is looked for on the
//! computer and the page says how to install it when it is missing.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::remote::Remote;

/// Where `adb` is, when it is installed: on the PATH, or in the places the
/// installers and Android Studio use (a program started from the Finder or
/// the Start menu does not always see the PATH of a terminal).
pub fn find() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "adb.exe" } else { "adb" };
    if let Some(path) = std::env::var_os("PATH")
        && let Some(found) = std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.is_file())
    {
        return Some(found);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(sdk) = std::env::var_os(var) {
            dirs.push(Path::new(&sdk).join("platform-tools"));
        }
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = Path::new(&home);
        dirs.push(home.join("Library/Android/sdk/platform-tools"));
        dirs.push(home.join("Android/Sdk/platform-tools"));
        dirs.push(home.join("AppData/Local/Android/Sdk/platform-tools"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(Path::new(&local).join("Android/Sdk/platform-tools"));
    }
    for fixed in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/snap/bin", "/opt/android-sdk/platform-tools"] {
        dirs.push(fixed.into());
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}

/// What `adb devices` says about a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Ready.
    Ready,
    /// The phone has not accepted this computer's key yet.
    Unauthorized,
    /// Found, but not answering (cable, sleep, a stale wireless link).
    Offline,
    /// Linux: the user may not use the USB device (udev rules).
    NoPermissions,
    /// Recovery, sideload, bootloader…
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: State,
    pub model: String,
    pub product: String,
}

impl Device {
    /// Connected over Wi-Fi (an address and port, or a service name).
    pub fn wireless(&self) -> bool {
        self.serial.contains(':') || self.serial.contains("._adb-tls-")
    }

    /// "Pixel 8" when the device told its model, else the serial number.
    pub fn name(&self) -> String {
        if self.model.is_empty() { self.serial.clone() } else { self.model.replace('_', " ") }
    }
}

/// Reads the output of `adb devices -l`.
pub fn parse_devices(text: &str) -> Vec<Device> {
    let mut list = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // The header, and the server's own messages ("* daemon started …").
        if line.is_empty() || line.starts_with("List of devices") || line.starts_with('*') {
            continue;
        }
        let Some((serial, rest)) = line.split_once(char::is_whitespace) else { continue };
        let rest = rest.trim_start();
        let state = if rest.starts_with("no permissions") {
            State::NoPermissions
        } else {
            match rest.split_whitespace().next().unwrap_or("") {
                "device" => State::Ready,
                "unauthorized" => State::Unauthorized,
                "offline" => State::Offline,
                other => State::Other(other.to_string()),
            }
        };
        let field = |key: &str| {
            rest.split_whitespace()
                .find_map(|w| w.strip_prefix(key).and_then(|v| v.strip_prefix(':')))
                .unwrap_or("")
                .to_string()
        };
        list.push(Device { serial: serial.to_string(), state, model: field("model"), product: field("product") });
    }
    list
}

/// A port is added to a bare address: wireless debugging's old default is 5555.
pub fn with_port(address: &str, default: u16) -> String {
    let a = address.trim();
    if a.rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok()) {
        a.to_string()
    } else {
        format!("{a}:{default}")
    }
}

/// Text that finds itself in logcat's regular-expression search.
pub fn escape_regex(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// What logcat should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logcat {
    /// V, D, I, W, E or F: this level and above.
    pub level: char,
    /// Only lines containing this text (case does not matter).
    pub text: String,
    /// Only this app (package name): the app has to be running.
    pub package: String,
    /// "" for the usual buffers, or main, system, crash, events, radio, all.
    pub buffer: String,
}

impl Default for Logcat {
    fn default() -> Self {
        Self { level: 'V', text: String::new(), package: String::new(), buffer: String::new() }
    }
}

pub const LEVELS: [(char, &str); 6] =
    [('V', "Verbose"), ('D', "Debug"), ('I', "Info"), ('W', "Warning"), ('E', "Error"), ('F', "Fatal")];

impl Logcat {
    /// The arguments after `adb -s SERIAL`; `pid` is the app's process.
    pub fn args(&self, pid: Option<u32>) -> Vec<String> {
        let mut a: Vec<String> = ["logcat", "-v", "threadtime"].map(String::from).into();
        if !self.buffer.is_empty() {
            a.extend(["-b".to_string(), self.buffer.clone()]);
        }
        if let Some(pid) = pid {
            a.push(format!("--pid={pid}"));
        }
        if !self.text.trim().is_empty() {
            a.extend(["-e".to_string(), format!("(?i){}", escape_regex(self.text.trim()))]);
        }
        if self.level != 'V' {
            a.push(format!("*:{}", self.level));
        }
        a
    }
}

/// A usable `adb`.
#[derive(Debug, Clone)]
pub struct Adb {
    path: PathBuf,
}

impl Adb {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn program(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    /// `adb [-s SERIAL] args…`
    fn args(serial: Option<&str>, args: &[&str]) -> Vec<String> {
        let mut a = Vec::new();
        if let Some(s) = serial {
            a.extend(["-s".to_string(), s.to_string()]);
        }
        a.extend(args.iter().map(|s| s.to_string()));
        a
    }

    /// Runs adb and returns what it printed (stdout and stderr together: adb
    /// reports many failures on stdout with a success exit code).
    pub fn run(&self, serial: Option<&str>, args: &[&str]) -> Result<String> {
        let all = Self::args(serial, args);
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let out = crate::cmd::output(&self.program(), &refs)?;
        let mut text = out.stdout.trim().to_string();
        if !out.stderr.trim().is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text += out.stderr.trim();
        }
        if !out.success {
            bail!("{}", if text.is_empty() { "adb failed".to_string() } else { text });
        }
        Ok(text)
    }

    pub fn version(&self) -> Result<String> {
        Ok(self.run(None, &["version"])?.lines().next().unwrap_or("").to_string())
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        Ok(parse_devices(&self.run(None, &["devices", "-l"])?))
    }

    /// `adb connect`: adb says "failed to connect" with a success exit code.
    pub fn connect(&self, address: &str) -> Result<String> {
        let text = self.run(None, &["connect", &with_port(address, 5555)])?;
        if text.contains("failed") || text.contains("cannot") || text.contains("unable") {
            bail!("{text}");
        }
        Ok(text)
    }

    /// Pairing for wireless debugging (Android 11 and newer): the address and
    /// port shown in "Pair device with pairing code", and its six digits.
    pub fn pair(&self, address: &str, code: &str) -> Result<String> {
        let code = code.trim();
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            bail!("the pairing code is six digits");
        }
        let a = address.trim();
        if with_port(a, 0).ends_with(":0") {
            bail!("pairing needs the address and port shown on the phone, like 192.168.1.20:37099");
        }
        let text = self.run(None, &["pair", a, code])?;
        if !text.contains("Successfully paired") {
            bail!("{text}");
        }
        Ok(text)
    }

    pub fn restart_server(&self) -> Result<String> {
        let _ = self.run(None, &["kill-server"]);
        self.run(None, &["start-server"])
    }

    /// A command in the device's shell, with its output.
    pub fn shell(&self, serial: &str, command: &str) -> Result<String> {
        self.run(Some(serial), &["shell", command])
    }

    /// The process number of a running app.
    pub fn pid_of(&self, serial: &str, package: &str) -> Result<u32> {
        if package.is_empty() || !package.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
            bail!("{package:?} is not a package name (like com.example.app)");
        }
        let out = self.shell(serial, &format!("pidof {package}")).unwrap_or_default();
        out.split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .with_context(|| format!("{package} is not running on the device: start the app first"))
    }

    /// The screen as a PNG.
    pub fn screenshot(&self, serial: &str) -> Result<Vec<u8>> {
        let out = crate::cmd::command(&self.program())
            .args(["-s", serial, "exec-out", "screencap", "-p"])
            .stdin(Stdio::null())
            .output()
            .context("could not run adb")?;
        if !out.status.success() || !out.stdout.starts_with(b"\x89PNG") {
            bail!("{}", crate::cmd::text(&out.stderr).trim());
        }
        Ok(out.stdout)
    }

    pub fn install(&self, serial: &str, apk: &Path) -> Result<String> {
        self.run(Some(serial), &["install", "-r", &apk.to_string_lossy()])
    }

    pub fn push(&self, serial: &str, local: &Path, remote: &str) -> Result<String> {
        self.run(Some(serial), &["push", &local.to_string_lossy(), remote])
    }

    pub fn pull(&self, serial: &str, remote: &str, local: &Path) -> Result<String> {
        self.run(Some(serial), &["pull", remote, &local.to_string_lossy()])
    }

    /// An interactive shell in a terminal.
    pub fn shell_session(&self, serial: &str, rows: u16, cols: u16) -> Result<Remote> {
        Remote::program(&self.program(), &Self::args(Some(serial), &["shell"]), rows, cols)
    }

    /// The device's log, as it is written.
    pub fn logcat_session(&self, serial: &str, filter: &Logcat, rows: u16, cols: u16) -> Result<Remote> {
        let pid =
            if filter.package.trim().is_empty() { None } else { Some(self.pid_of(serial, filter.package.trim())?) };
        let mut args = Self::args(Some(serial), &[]);
        args.extend(filter.args(pid));
        Remote::program(&self.program(), &args, rows, cols)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_device_list() {
        let text = "* daemon not running; starting now at tcp:5037\n* daemon started successfully\n\
List of devices attached\n\
R58M123ABC             device usb:1-1 product:beyond1 model:SM_G973F device:beyond1 transport_id:3\n\
192.168.1.20:5555      offline product:x model:Pixel_8 transport_id:5\n\
0123456789ABCDEF       unauthorized usb:2-1 transport_id:7\n\
emulator-5554          device product:sdk_gphone model:sdk_gphone_x86 device:generic transport_id:1\n\
ZX1                    no permissions (user in plugdev group; are your udev rules wrong?); see [http://developer.android.com/tools/device.html] usb:3-1\n\
abc                    recovery transport_id:9\n\n";
        let d = parse_devices(text);
        assert_eq!(d.len(), 6);
        assert_eq!(
            (d[0].state.clone(), d[0].model.as_str(), d[0].name().as_str()),
            (State::Ready, "SM_G973F", "SM G973F")
        );
        assert!(!d[0].wireless());
        assert_eq!((d[1].state.clone(), d[1].wireless(), d[1].name().as_str()), (State::Offline, true, "Pixel 8"));
        assert_eq!((d[2].state.clone(), d[2].name().as_str()), (State::Unauthorized, "0123456789ABCDEF"));
        assert_eq!(d[3].product, "sdk_gphone");
        assert_eq!(d[4].state, State::NoPermissions);
        assert_eq!(d[5].state, State::Other("recovery".into()));
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }

    #[test]
    fn addresses_get_a_port() {
        assert_eq!(with_port("192.168.1.20", 5555), "192.168.1.20:5555");
        assert_eq!(with_port(" 192.168.1.20:41234 ", 5555), "192.168.1.20:41234");
        assert_eq!(with_port("phone.local", 5555), "phone.local:5555");
    }

    #[test]
    fn logcat_filters_become_arguments() {
        assert_eq!(Logcat::default().args(None), ["logcat", "-v", "threadtime"]);
        let f = Logcat { level: 'W', text: "Wi-Fi (x)".into(), package: String::new(), buffer: "crash".into() };
        assert_eq!(
            f.args(Some(4242)),
            ["logcat", "-v", "threadtime", "-b", "crash", "--pid=4242", "-e", r"(?i)Wi-Fi \(x\)", "*:W"]
        );
    }

    #[test]
    fn pairing_input_is_checked_before_adb_is_asked() {
        let adb = Adb::new("adb-that-does-not-exist".into());
        assert!(adb.pair("192.168.1.20:37099", "12345").unwrap_err().to_string().contains("six digits"));
        assert!(adb.pair("192.168.1.20", "123456").unwrap_err().to_string().contains("port"));
        assert!(adb.pid_of("x", "a; reboot").unwrap_err().to_string().contains("not a package name"));
    }
}
