//! Checking GitHub for a newer release and installing it.
//!
//! The installer for this system and this kind of installation is
//! downloaded, checked against the SHA-256 digest GitHub publishes, and
//! installed after the app has closed; the new version then starts:
//!
//! * Windows (installed with setup): the setup runs silently (`/S`) and
//!   starts the app again (`/R`).
//! * macOS (.app): the bundle is replaced from the disk image.
//! * Linux: an AppImage replaces itself; .deb and .rpm installs go through
//!   the package manager after the system's password dialog.
//!
//! Portable copies only get a link to the download page.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use eframe::egui::{self, Align, Layout, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::adapters::format_bytes as format_size;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::i18n::{tr, trf, trl, trlf, with_icon};
use crate::jobs::{Job, Progress};
use crate::theme::{self, Palette, icon_label};

const LATEST: &str = "https://api.github.com/repos/itsmrroot/Network-Manager-releases/releases/latest";

/// The version of this program.
pub fn current() -> &'static str {
    // Development aid: pretend to be an older version to try updating.
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("NETMGR_PRETEND_VERSION") {
        return Box::leak(v.into_boxed_str());
    }
    env!("CARGO_PKG_VERSION")
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
    /// The notes of the release (what is new), in Markdown.
    #[serde(default)]
    body: Option<String>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    /// `sha256:<hex>`.
    digest: Option<String>,
}

/// A newer release.
#[derive(Clone)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    pub page: String,
    /// What is new in it (the release's notes).
    pub notes: String,
    /// The installer for this system, if it can be installed from the app
    /// (and has already been published).
    asset: Option<Asset>,
}

#[derive(Clone)]
struct Asset {
    name: String,
    url: String,
    size: u64,
    sha256: Option<String>,
}

impl Release {
    /// Whether "Update" can install it, rather than open the download page.
    pub fn installable(&self) -> bool {
        self.asset.is_some()
    }
}

/// How this copy of the app was installed.
#[derive(Clone, Debug, PartialEq)]
// Each system uses only some of these.
#[allow(dead_code)]
enum Kind {
    /// Windows, installed by the setup program into this folder.
    Setup,
    /// macOS, this .app bundle.
    Bundle(PathBuf),
    /// Linux, this AppImage file.
    AppImage(PathBuf),
    Deb,
    Rpm,
    /// Unpacked from an archive: updated by hand.
    Portable,
}

fn kind() -> Kind {
    let Ok(exe) = std::env::current_exe() else { return Kind::Portable };
    #[cfg(windows)]
    if exe.parent().is_some_and(|d| d.join("uninstall.exe").is_file()) {
        return Kind::Setup;
    }
    #[cfg(target_os = "macos")]
    if let Some(app) = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))
        && exe.parent().is_some_and(|d| d.ends_with("Contents/MacOS"))
    {
        return Kind::Bundle(app.to_path_buf());
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(file) = std::env::var_os("APPIMAGE") {
            return Kind::AppImage(PathBuf::from(file));
        }
        let owned_by = |cmd: &str, arg: &str| {
            std::process::Command::new(cmd)
                .args([arg, &exe.display().to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if owned_by("dpkg-query", "-S") {
            return Kind::Deb;
        }
        if owned_by("rpm", "-qf") {
            return Kind::Rpm;
        }
    }
    let _ = exe;
    Kind::Portable
}

/// Name of the release file that updates `kind` on this system.
fn asset_name(kind: &Kind) -> Option<String> {
    let arch = |x64: &'static str, arm: &'static str| if cfg!(target_arch = "aarch64") { arm } else { x64 };
    Some(match kind {
        Kind::Setup => format!("netmgr-windows-{}-setup.exe", arch("x64", "arm64")),
        Kind::Bundle(_) => format!("netmgr-macos-{}.dmg", arch("intel", "apple-silicon")),
        Kind::AppImage(_) => format!("netmgr-linux-{}.AppImage", arch("x86_64", "arm64")),
        Kind::Deb => format!("netmgr-linux-{}.deb", arch("x86_64", "arm64")),
        Kind::Rpm => format!("netmgr-linux-{}.rpm", arch("x86_64", "arm64")),
        Kind::Portable => return None,
    })
}

/// `1.2.3` or `v1.2.3` as numbers; anything after the numbers is ignored.
fn parse_version(v: &str) -> Option<[u64; 3]> {
    let mut parts = v.trim().trim_start_matches('v').split(['.', '-', '+']);
    let mut out = [0; 3];
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    Some(out)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(format!("netmgr-gui/{}", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Asks GitHub for the latest release. `None` means this is the newest.
pub fn check() -> Result<Option<Release>> {
    let text = agent()
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("could not reach GitHub")?
        .body_mut()
        .read_to_string()?;
    let gh: GhRelease = serde_json::from_str(&text).context("unexpected answer from GitHub")?;
    let version = gh.tag_name.trim_start_matches('v').to_string();
    if gh.draft || gh.prerelease || !is_newer(&version, current()) {
        return Ok(None);
    }
    let asset = asset_name(&kind()).and_then(|name| {
        let a = gh.assets.into_iter().find(|a| a.name == name)?;
        Some(Asset {
            name: a.name,
            url: a.browser_download_url,
            size: a.size,
            sha256: a.digest.and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase)),
        })
    });
    let notes = gh.body.unwrap_or_default().chars().take(4000).collect();
    Ok(Some(Release { version, page: gh.html_url, notes, asset }))
}

/// Removes the folders that earlier updates downloaded their installer into
/// (the installer replaces the app while it runs, so it cannot remove its
/// own folder). Only folders older than an hour: an update may be running.
pub fn clean_old_downloads() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    let hour = std::time::Duration::from_secs(3600);
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > hour));
        let ours = e.file_name().to_string_lossy().starts_with("netmgr-update-");
        if ours && old && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Downloads the installer of `release` into a new temporary folder and
/// checks it. Returns the file and its SHA-256, which [`install`] checks
/// again right before running it.
pub fn download(release: &Release, progress: &Progress, cancel: &AtomicBool) -> Result<(PathBuf, String)> {
    let Some(asset) = &release.asset else { bail!("no installer for this system") };
    // GitHub publishes a checksum for every file; without one there is
    // nothing to check the download against.
    ensure!(asset.sha256.is_some(), "the update has no checksum");
    let dir = netmgr::cmd::private_dir("netmgr-update")?;
    let path = dir.join(&asset.name);

    progress.set(0.0, tr("Downloading the update"));
    let mut response = agent().get(&asset.url).call().context("could not download the update")?;
    let mut body = response.body_mut().with_config().limit(asset.size + 1).reader();
    let mut file = File::create(&path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 256 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let n = body.read(&mut buf).context("the download was interrupted")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hash.update(&buf[..n]);
        total += n as u64;
        progress.set(total as f32 / asset.size.max(1) as f32, tr("Downloading the update"));
    }
    file.sync_all()?;
    drop(file);
    ensure!(total == asset.size, "the download is incomplete");
    let want = asset.sha256.clone().unwrap_or_default();
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    ensure!(got == want, "the download is damaged (checksum mismatch)");
    Ok((path, want))
}

/// The SHA-256 of what `file` reads, in hex.
#[cfg_attr(not(windows), allow(dead_code))]
fn sha256_hex(file: &mut impl Read) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buf = vec![0; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Shell-quotes `s` for `sh`.
#[cfg(unix)]
fn quote(s: &Path) -> String {
    format!("'{}'", s.display().to_string().replace('\'', r"'\''"))
}

/// Runs `script` with `sh` once this process has exited, detached from it.
#[cfg(unix)]
fn after_exit(script: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let pid = std::process::id();
    let script = format!("while kill -0 {pid} 2>/dev/null; do sleep 0.3; done\n{script}");
    std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .context("could not start the installer")?;
    Ok(())
}

/// Starts installing the downloaded `file`. On success the app must close
/// right away: the installation continues without it and starts the new
/// version.
pub fn install(file: &Path, sha256: &str) -> Result<()> {
    let _ = sha256;
    match kind() {
        #[cfg(windows)]
        Kind::Setup => {
            use std::os::windows::fs::OpenOptionsExt;
            use std::os::windows::process::CommandExt;
            // DETACHED_PROCESS: the setup outlives the app it replaces.
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const FILE_SHARE_READ: u32 = 1;
            // The setup runs as administrator from the user's temporary
            // folder: held open so that nothing can change it (others may
            // only read), and checked again, until it has started.
            let mut held = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(file)
                .context("could not open the downloaded setup")?;
            ensure!(sha256_hex(&mut held)? == sha256, "the setup changed after it was downloaded");
            std::process::Command::new(file)
                .args(["/S", "/R"])
                .creation_flags(DETACHED_PROCESS)
                .spawn()
                .context("could not start the setup")?;
            drop(held);
            Ok(())
        }
        #[cfg(target_os = "macos")]
        Kind::Bundle(app) => {
            let parent = app.parent().context("unexpected app location")?;
            if !writable(parent) {
                // No rights to replace the app: let the user drag it over.
                std::process::Command::new("open").arg(file).spawn()?;
                return Ok(());
            }
            let (dmg, app, staged) = (quote(file), quote(&app), quote(&app.with_extension("app.new")));
            // The disk image shows the licence, which `yes` accepts. The old
            // app is only removed once the new one is fully copied; if
            // anything fails, the disk image is opened for the user.
            after_exit(&format!(
                r#"mnt=$(mktemp -d) || exit 1
fail() {{ hdiutil detach "$mnt" -quiet 2>/dev/null; open {dmg}; exit 1; }}
yes | PAGER=cat hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" {dmg} >/dev/null 2>&1 || fail
src=$(ls -d "$mnt"/*.app | head -n 1)
[ -n "$src" ] || fail
rm -rf {staged}
ditto "$src" {staged} || fail
hdiutil detach "$mnt" -quiet
rm -rf {app} && mv {staged} {app} && open {app}"#
            ))
        }
        #[cfg(target_os = "linux")]
        Kind::AppImage(path) => {
            use std::os::unix::fs::PermissionsExt;
            let dir = path.parent().context("unexpected AppImage location")?;
            ensure!(writable(dir), "no permission to replace {}", path.display());
            // Copied next to it first, so that the switch is a rename.
            let staged = dir.join(format!(".{}.new", path.file_name().unwrap_or_default().to_string_lossy()));
            std::fs::copy(file, &staged)?;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
            std::fs::rename(&staged, &path)?;
            after_exit(&quote(&path))
        }
        #[cfg(target_os = "linux")]
        k @ (Kind::Deb | Kind::Rpm) => {
            use std::os::unix::fs::PermissionsExt;
            // The package manager reads it as root and as its own user.
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644))?;
            if let Some(dir) = file.parent() {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
            }
            // As root: a copy in root's own folder, checked against the
            // release's checksum, is what gets installed (the download sits
            // in a folder of the user's, which other programs could change).
            let install = if k == Kind::Deb {
                "apt-get install -y --allow-downgrades \"$p\""
            } else {
                "if command -v dnf >/dev/null; then dnf install -y \"$p\"; elif command -v zypper >/dev/null; then zypper --non-interactive install --allow-unsigned-rpm \"$p\"; else rpm -U \"$p\"; fi"
            };
            let install = format!(
                "d=$(mktemp -d) && p=\"$d/$(basename \"$1\")\" && cp \"$1\" \"$p\" && \
                 echo \"$2  $p\" | sha256sum -c --quiet && {{ {install}; }}; r=$?; rm -rf \"$d\"; exit $r"
            );
            let sudo = if netmgr::cmd::is_admin() { "" } else { "pkexec " };
            let exe = std::env::current_exe()?;
            after_exit(&format!(
                "{sudo}/bin/sh -c {} sh {} {} >/dev/null 2>&1; {} &",
                quote(Path::new(&install)),
                quote(file),
                quote(Path::new(sha256)),
                quote(&exe)
            ))
        }
        _ => bail!("this copy of the app has to be updated by hand"),
    }
}

/// Whether this user may create files in `dir`.
#[cfg(unix)]
fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

// ---------------------------------------------------------------------------
// In the app

/// Update checks and installs, and what the app shows of them.
#[derive(Default)]
pub struct Updater {
    checking: Option<(Job<Option<Release>>, bool)>,
    /// The newer release found, if any.
    release: Option<Release>,
    /// Checked: this is the newest version.
    latest: bool,
    download: Option<Job<(PathBuf, String)>>,
    error: Option<String>,
    dialog: bool,
}

impl Updater {
    /// Asks GitHub in the background. A `manual` check shows its result; a
    /// check at start only shows the update button if there is one.
    pub fn check(&mut self, ctx: &egui::Context, manual: bool) {
        if self.checking.is_some() || self.download.is_some() {
            return;
        }
        self.error = None;
        self.latest = false;
        self.checking = Some((Job::spawn(ctx, |_, _| check()), manual));
    }

    /// Takes in finished work. Returns true when the app must close so that
    /// the update can be installed.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        if let Some((job, manual)) = &self.checking
            && let Some(result) = job.poll()
        {
            let manual = *manual;
            self.checking = None;
            match result {
                Ok(Some(r)) => {
                    self.release = Some(r);
                    self.dialog |= manual;
                }
                Ok(None) => self.latest = true,
                Err(e) if manual => {
                    self.error = Some(trf("Could not check for updates: {error}", &[("error", &format!("{e:#}"))]))
                }
                Err(e) => log::info!("update check failed: {e:#}"),
            }
        }
        if let Some(job) = &self.download {
            match job.poll() {
                None => ctx.request_repaint_after(Duration::from_millis(150)),
                Some(result) => {
                    let cancelled = job.stopping();
                    self.download = None;
                    match result.and_then(|(file, sha256)| install(&file, &sha256)) {
                        Ok(()) => return true,
                        Err(_) if cancelled => {}
                        Err(e) => {
                            self.error =
                                Some(trf("The update could not be installed: {error}", &[("error", &format!("{e:#}"))]))
                        }
                    }
                }
            }
        }
        false
    }

    /// The version a newer release has, once one was found.
    pub fn available(&self) -> Option<&str> {
        self.release.as_ref().map(|r| r.version.as_str())
    }

    /// The button that announces an update, for the top bar or the sidebar.
    pub fn button(&mut self, ui: &mut Ui, p: &Palette) {
        let Some(version) = self.available() else { return };
        let label = with_icon(icon::ARROW_CIRCLE_UP, &trf("Update to {version}", &[("version", &version)]));
        if theme::primary_button(ui, p, &label, true).clicked() {
            self.dialog = true;
        }
    }

    /// "Check for updates" with its result, for the About page.
    pub fn status(&mut self, ui: &mut Ui, p: &Palette) {
        ui.horizontal_wrapped(|ui| {
            let busy = self.checking.is_some();
            if ui
                .add_enabled(!busy, egui::Button::new(icon_label(icon::ARROWS_CLOCKWISE, "Check for updates")))
                .clicked()
            {
                self.check(ui.ctx(), true);
            }
            if busy {
                ui.spinner();
                ui.label(RichText::new(tr("Checking for updates…")).color(p.weak));
            } else if let Some(version) = self.available() {
                let text = trf("Version {version} is available.", &[("version", &version)]);
                if ui.link(RichText::new(text).color(p.accent)).clicked() {
                    self.dialog = true;
                }
            } else if self.latest {
                ui.label(
                    RichText::new(format!("{}  {}", icon::CHECK_CIRCLE, "You have the latest version."))
                        .color(p.success),
                );
            } else if let Some(e) = &self.error
                && !self.dialog
            {
                ui.label(RichText::new(e).color(p.danger));
            }
        });
    }

    /// Development aid: the dialog, with the notes of the changelog's newest version.
    #[cfg(debug_assertions)]
    pub fn demo_dialog(&mut self) {
        let log = include_str!("../../CHANGELOG.md");
        let start = log.find("## 0.9.8").unwrap_or(0);
        let notes: String = log[start..].chars().take(2600).collect();
        self.release = Some(Release {
            version: "0.9.9".into(),
            page: "https://github.com/itsmrroot/Network-Manager-releases/releases/latest".into(),
            notes,
            asset: None,
        });
        self.dialog = true;
    }

    /// The update dialog, while open. `busy`: a scan or save is running.
    pub fn dialog(&mut self, ctx: &egui::Context, p: &Palette, busy: bool) {
        if !self.dialog {
            return;
        }
        let Some(release) = self.release.clone() else {
            self.dialog = false;
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("update")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::ARROW_CIRCLE_UP).size(26.0).color(p.accent));
                ui.label(theme::semibold(tr("Update available"), 18.0).color(p.text));
            });
            ui.add_space(8.0);
            theme::paragraph(
                ui,
                &trlf("Version {version} is available. You have version {current}.", &[("version", &release.version), ("current", &current())]),
                14.5,
                p.text,
            );
            ui.add_space(4.0);
            ui.label(theme::semibold(tr("What's new"), 15.0).color(p.accent));
            if !release.notes.trim().is_empty() {
                egui::ScrollArea::vertical().id_salt("update-notes").max_height(230.0).auto_shrink([false, true]).show(
                    ui,
                    |ui| notes_ui(ui, p, &release.notes),
                );
            }
            ui.hyperlink_to(icon_label(icon::LIST_BULLETS, "Open the release page"), &release.page);
            ui.add_space(8.0);

            if let Some(job) = &self.download {
                let state = job.progress.snapshot();
                let bar = match state.fraction {
                    Some(f) => egui::ProgressBar::new(f).show_percentage(),
                    None => egui::ProgressBar::new(0.0).animate(true),
                };
                ui.add(bar);
                ui.label(
                    RichText::new(match release.asset.as_ref() {
                        Some(a) => trf("Downloading… {done} of {total}", &[("done", &format_size((state.fraction.unwrap_or(0.0) as f64 * a.size as f64) as u64)), ("total", &format_size(a.size))]),
                        None => tr("Downloading…").to_string(),
                    })
                    .color(p.weak),
                );
                ui.add_space(8.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::secondary_button(ui, tr("Cancel")).clicked() {
                        job.stop();
                    }
                });
                return;
            }

            if !release.installable() {
                theme::paragraph(
                    ui,
                    trl("This copy of the app was not installed with an installer, so it is updated by downloading the new version."),
                    14.5,
                    p.weak,
                );
            } else {
                theme::paragraph(ui, trl("The app will close, install the update and start again."), 14.5, p.weak);
                if cfg!(target_os = "macos") {
                    theme::paragraph(ui, trl("macOS may ask for Local Network access again after the update."), 14.5, p.weak);
                }
            }
            if busy {
                ui.add_space(4.0);
                theme::paragraph(ui, trl("Finish the current task first."), 14.5, p.warning);
            }
            if let Some(e) = &self.error {
                ui.add_space(4.0);
                theme::paragraph(ui, e, 14.5, p.danger);
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if release.installable() {
                    if theme::primary_button(ui, p, &format!("  {}  ", tr("Update now")), !busy).clicked() {
                        self.error = None;
                        let r = release.clone();
                        self.download = Some(Job::spawn(ui.ctx(), move |progress, cancel| download(&r, progress, cancel)));
                    }
                } else if theme::primary_button(ui, p, &format!("  {}  ", tr("Open download page")), true).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page));
                    self.dialog = false;
                }
                if theme::secondary_button(ui, tr("Later")).clicked() {
                    self.dialog = false;
                }
            });
        });
        // Closing from outside is "Later", except while downloading.
        if modal.should_close() && self.download.is_none() {
            self.dialog = false;
        }
    }
}

/// A line of release notes.
#[derive(Debug, PartialEq)]
enum Note {
    Heading(String),
    Bullet(String),
    Text(String),
    Gap,
}

/// Release notes (Markdown as GitHub shows it) as lines to draw: headings,
/// bullets and text, without the marks (`**`, backticks, links).
fn parse_notes(text: &str) -> Vec<Note> {
    let clean = |s: &str| {
        let mut out = s.replace("**", "").replace('`', "");
        // [text](address) → text
        while let (Some(a), Some(b)) = (out.find("]("), out.find('[')) {
            let Some(end) = out[a..].find(')') else { break };
            if b >= a {
                break;
            }
            let label = out[b + 1..a].to_string();
            out.replace_range(b..a + end + 1, &label);
        }
        out.trim().to_string()
    };
    let mut notes = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            if !matches!(notes.last(), Some(Note::Gap) | None) {
                notes.push(Note::Gap);
            }
        } else if let Some(h) = line.trim_start_matches('#').strip_prefix(' ').filter(|_| line.starts_with('#')) {
            // The version heading repeats what the dialog says.
            let h = clean(h);
            if !h.chars().next().is_some_and(|c| c.is_ascii_digit() || c == 'v') {
                notes.push(Note::Heading(h));
            }
        } else if let Some(b) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            notes.push(Note::Bullet(clean(b)));
        } else if let Some(Note::Bullet(last)) = notes.last_mut() {
            // A bullet written over several lines is one bullet.
            last.push(' ');
            last.push_str(&clean(line));
        } else {
            notes.push(Note::Text(clean(line)));
        }
    }
    notes
}

fn notes_ui(ui: &mut Ui, p: &Palette, text: &str) {
    for note in parse_notes(text) {
        match note {
            Note::Gap => ui.add_space(4.0),
            Note::Heading(h) => {
                ui.add_space(2.0);
                ui.label(theme::semibold(h, 14.5).color(p.text));
            }
            Note::Bullet(b) => {
                ui.horizontal_top(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("•").color(p.accent));
                    ui.add(egui::Label::new(RichText::new(b).color(p.text).size(13.5)).wrap());
                });
            }
            Note::Text(t) => {
                ui.add(egui::Label::new(RichText::new(t).color(p.weak).size(13.5)).wrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("v0.3.10", "0.3.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.0", "0.3.1"));
        assert!(!is_newer("garbage", "0.3.1"));
        assert_eq!(parse_version("v1.2.3-beta.1"), Some([1, 2, 3]));
    }

    #[test]
    fn installers_match_the_release_files() {
        let names: Vec<String> =
            [Kind::Setup, Kind::Bundle(PathBuf::new()), Kind::AppImage(PathBuf::new()), Kind::Deb, Kind::Rpm]
                .iter()
                .filter_map(asset_name)
                .collect();
        let workflow = include_str!("../../.github/workflows/release.yml");
        for name in names {
            // The workflow names them netmgr-<label>(-setup).<ext>.
            let label = name.trim_start_matches("netmgr-").split(['.']).next().unwrap().trim_end_matches("-setup");
            assert!(workflow.contains(&format!("label: {label} ")), "{name}");
        }
        assert_eq!(asset_name(&Kind::Portable), None);
    }
}

#[cfg(test)]
mod notes_tests {
    use super::*;

    #[test]
    fn release_notes_become_lines() {
        let text = "## 0.9.8\n\n### New\n- **Find** in the terminal (see [the guide](https://example.com/guide))\n  and everything that scrolled by\n- Colours: `up` and `down`\n\nA closing remark.";
        assert_eq!(
            parse_notes(text),
            [
                Note::Heading("New".into()),
                Note::Bullet("Find in the terminal (see the guide) and everything that scrolled by".into()),
                Note::Bullet("Colours: up and down".into()),
                Note::Gap,
                Note::Text("A closing remark.".into()),
            ]
        );
        assert!(parse_notes("").is_empty());
    }

    /// A version is never released without notes: its section must be in
    /// CHANGELOG.md, and the release script finds it.
    #[test]
    fn the_changelog_has_the_current_version() {
        let log = include_str!("../../CHANGELOG.md");
        let version = env!("CARGO_PKG_VERSION");
        let heading = format!("## {version}");
        let at = log.lines().position(|l| l.trim() == heading);
        let at = at.unwrap_or_else(|| panic!("CHANGELOG.md has no section \"{heading}\""));
        let section: Vec<&str> = log.lines().skip(at + 1).take_while(|l| !l.starts_with("## ")).collect();
        assert!(section.iter().any(|l| l.starts_with("- ")), "the section of {version} has no bullet points");
    }
}
