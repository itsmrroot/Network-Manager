//! Console → Android (ADB): the connected phones and tablets, wireless
//! pairing, a shell, logcat and everyday actions, through the system's `adb`.

use std::io::Write;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::adb::{Adb, Device, LEVELS, Logcat, State};
use netmgr::remote::Remote;

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf};
use crate::theme::{self, Palette, icon_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sub {
    Shell,
    Log,
    Actions,
}

/// What an action printed (or why it failed).
struct Outcome {
    label: String,
    ok: bool,
    text: String,
}

type Job = (String, Receiver<Result<String, String>>);

pub struct AdbPage {
    adb: Option<Adb>,
    looked: bool,
    version: String,
    devices: Vec<Device>,
    /// Why the list could not be read.
    list_error: Option<String>,
    listing: Option<Receiver<Result<Vec<Device>, String>>>,
    listed: Option<Instant>,
    selected: String,
    sub: Sub,
    // Wireless debugging.
    pair_address: String,
    pair_code: String,
    connect_address: String,
    // The shell and logcat run in terminals of their own.
    shell: Option<Remote>,
    shell_term: crate::term::Term,
    logcat: Option<Remote>,
    log_term: crate::term::Term,
    filter: Logcat,
    log_file: Option<std::fs::File>,
    size: (u16, u16),
    // Actions.
    job: Option<Job>,
    outcome: Option<Outcome>,
    remote_path: String,
    send_to: String,
    /// The height left for the shell or the log under the tabs.
    room: f32,
    /// The log is being started away from the screen.
    log_starting: Option<Receiver<Result<Remote, String>>>,
    /// The restart button that was clicked once and waits for a second click.
    confirm: Option<&'static str>,
    /// Development aid: open the shell or the log as soon as a device is ready.
    #[cfg(debug_assertions)]
    auto: Option<Sub>,
}

impl Default for AdbPage {
    fn default() -> Self {
        Self {
            adb: None,
            looked: false,
            version: String::new(),
            devices: Vec::new(),
            list_error: None,
            listing: None,
            listed: None,
            selected: String::new(),
            sub: Sub::Shell,
            pair_address: String::new(),
            pair_code: String::new(),
            connect_address: String::new(),
            shell: None,
            shell_term: crate::term::Term::new(1_000_001),
            logcat: None,
            log_term: crate::term::Term::new(1_000_002),
            filter: Logcat::default(),
            log_file: None,
            size: (24, 80),
            job: None,
            outcome: None,
            remote_path: "/sdcard/Download/".into(),
            send_to: "/sdcard/Download/".into(),
            room: 400.0,
            log_starting: None,
            confirm: None,
            #[cfg(debug_assertions)]
            auto: None,
        }
    }
}

/// The device's answer to "which address do I connect to?"
fn first_address(ip_output: &str) -> Option<String> {
    ip_output
        .split_whitespace()
        .skip_while(|w| *w != "inet")
        .nth(1)
        .map(|a| a.split('/').next().unwrap_or(a).to_string())
}

impl AdbPage {
    /// Development aid: that part of the page, with its terminal started.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self, sub: &str) {
        self.sub = match sub {
            "log" => Sub::Log,
            "actions" => Sub::Actions,
            _ => Sub::Shell,
        };
        self.auto = Some(self.sub);
    }

    pub fn connected(&self) -> bool {
        self.shell.is_some() || self.logcat.is_some()
    }

    fn selected_device(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.serial == self.selected)
    }

    /// The chosen device, when it is ready for use.
    fn ready_serial(&self) -> Option<String> {
        self.selected_device().filter(|d| d.state == State::Ready).map(|d| d.serial.clone())
    }

    fn look_for_adb(&mut self) {
        self.looked = true;
        self.adb = netmgr::adb::find().map(Adb::new);
        self.version = self.adb.as_ref().and_then(|a| a.version().ok()).unwrap_or_default();
        self.listed = None;
    }

    fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if let Some(rx) = &self.listing
            && let Ok(result) = rx.try_recv()
        {
            self.listing = None;
            self.listed = Some(Instant::now());
            match result {
                Ok(list) => {
                    self.devices = list;
                    self.list_error = None;
                    // The only device is taken when nothing was chosen. If the
                    // chosen one left, no other is taken in its place: the
                    // reboot buttons are close.
                    if !self.devices.iter().any(|d| d.serial == self.selected) {
                        let none_before = self.selected.is_empty();
                        self.selected = if none_before && self.devices.len() == 1 {
                            self.devices[0].serial.clone()
                        } else {
                            String::new()
                        };
                    }
                }
                Err(e) => self.list_error = Some(e),
            }
        }
        if self.listing.is_none()
            && self.job.is_none()
            && let Some(adb) = &self.adb
            && self.listed.is_none_or(|t| t.elapsed() > Duration::from_secs(3))
        {
            let (tx, rx) = channel();
            let (adb, ctx) = (adb.clone(), ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(adb.devices().map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
            self.listing = Some(rx);
        }
        #[cfg(debug_assertions)]
        if let Some(sub) = self.auto
            && let (Some(adb), Some(serial)) =
                (&self.adb, self.ready_serial().or_else(|| self.devices.first().map(|d| d.serial.clone())))
        {
            self.auto = None;
            self.selected = serial.clone();
            let (rows, cols) = self.size;
            match sub {
                Sub::Shell => self.shell = adb.shell_session(&serial, rows, cols).ok(),
                Sub::Log => self.logcat = adb.logcat_session(&serial, &self.filter, rows, cols).ok(),
                Sub::Actions => {}
            }
        }
        if let Some((label, rx)) = &self.job
            && let Ok(result) = rx.try_recv()
        {
            let label = label.clone();
            self.job = None;
            self.listed = None;
            self.outcome = Some(match result {
                Ok(text) => Outcome { label, ok: true, text },
                Err(text) => Outcome { label, ok: false, text },
            });
            if self.outcome.as_ref().is_some_and(|o| o.ok) {
                sh.toast(tr("Done."));
            }
        }
        self.pump(sh);
    }

    /// Takes in what the shell and the log wrote: also while another page is
    /// shown, or the unread output would pile up (and a log being saved would
    /// stop growing).
    pub fn pump(&mut self, sh: &mut Shared) {
        // The log was being started away from the screen.
        if let Some(rx) = &self.log_starting
            && let Ok(result) = rx.try_recv()
        {
            self.log_starting = None;
            match result {
                Ok(r) => {
                    self.logcat = Some(r);
                    self.log_term = crate::term::Term::new(1_000_002);
                }
                Err(e) => sh.fail(trl("The log could not be started."), &anyhow::anyhow!(e)),
            }
        }
        if let Some(r) = &self.shell {
            while let Ok(bytes) = r.received.try_recv() {
                self.shell_term.feed(&bytes);
            }
            if r.ended() {
                while let Ok(bytes) = r.received.try_recv() {
                    self.shell_term.feed(&bytes);
                }
                self.shell = None;
                self.shell_term.feed(format!("\r\n\x1b[2m[{}]\x1b[0m\r\n", trl("Session ended")).as_bytes());
            }
        }
        if let Some(r) = &self.logcat {
            while let Ok(bytes) = r.received.try_recv() {
                if let Some(f) = &mut self.log_file {
                    let _ = f.write_all(&bytes);
                }
                self.log_term.feed(&bytes);
            }
            if r.ended() {
                while let Ok(bytes) = r.received.try_recv() {
                    if let Some(f) = &mut self.log_file {
                        let _ = f.write_all(&bytes);
                    }
                    self.log_term.feed(&bytes);
                }
                self.logcat = None;
                self.log_term.feed(format!("\r\n\x1b[2m[{}]\x1b[0m\r\n", trl("Log stopped")).as_bytes());
            }
        }
    }

    /// Runs `work` away from the screen; its result appears under Actions.
    fn start(
        &mut self,
        ctx: &egui::Context,
        label: String,
        work: impl FnOnce(&Adb) -> anyhow::Result<String> + Send + 'static,
    ) {
        // One at a time: a second one would take the place of the first, and
        // its result would be lost.
        if self.job.is_some() {
            return;
        }
        let Some(adb) = self.adb.clone() else { return };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work(&adb).map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        self.job = Some((label, rx));
        self.sub = Sub::Actions;
        self.outcome = None;
        self.confirm = None;
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if !self.looked {
            self.look_for_adb();
        }
        self.poll(&ui.ctx().clone(), sh);
        ui.ctx().request_repaint_after(if self.connected() {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(1)
        });
        if self.adb.is_none() {
            self.missing(ui, p);
            return;
        }
        // A short window scrolls the page; the terminal keeps a usable height.
        let total = ui.available_height();
        egui::ScrollArea::vertical().id_salt("adb-page").auto_shrink(false).show(ui, |ui| {
            let origin = ui.cursor().top();
            self.devices_card(ui, p, sh);
            ui.add_space(8.0);
            theme::subtabs(
                ui,
                p,
                &mut self.sub,
                &[
                    (Sub::Shell, icon::TERMINAL_WINDOW, tr("Shell")),
                    (Sub::Log, icon::LIST_MAGNIFYING_GLASS, tr("Log (logcat)")),
                    (Sub::Actions, icon::LIGHTNING, tr("Actions")),
                ],
            );
            ui.add_space(6.0);
            if self.sub != Sub::Actions {
                self.confirm = None;
            }
            self.room = total - (ui.cursor().top() - origin);
            match self.sub {
                Sub::Shell => self.shell_ui(ui, p, sh),
                Sub::Log => self.log_ui(ui, p, sh),
                Sub::Actions => self.actions_ui(ui, p, sh),
            }
        });
    }

    /// The height for the terminal, below what the page has shown so far.
    fn terminal_height(&self, ui: &Ui, since: f32) -> f32 {
        (self.room - (ui.cursor().top() - since) - 8.0).max(260.0)
    }

    fn missing(&mut self, ui: &mut Ui, p: &Palette) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("adb was not found on this computer"), 16.0).color(p.text));
            ui.add_space(4.0);
            theme::paragraph(
                ui,
                trl(
                    "adb (Android Debug Bridge) is a small program from Google that talks to Android phones and tablets. It cannot be shipped inside this app (Google's license), but it is one command to install:",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(6.0);
            let commands: &[(&str, &str)] = if cfg!(target_os = "macos") {
                &[("macOS", "brew install android-platform-tools")]
            } else if cfg!(windows) {
                &[("Windows", "winget install Google.PlatformTools")]
            } else {
                &[
                    ("Debian, Ubuntu, Kali", "sudo apt install adb"),
                    ("Fedora", "sudo dnf install android-tools"),
                    ("Arch", "sudo pacman -S android-tools"),
                ]
            };
            for (system, command) in commands {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(*system).color(p.weak));
                    ui.label(RichText::new(*command).monospace());
                    if ui.small_button(icon::COPY).on_hover_text(tr("Copy")).clicked() {
                        ui.ctx().copy_text((*command).to_string());
                    }
                });
            }
            ui.add_space(4.0);
            ui.hyperlink_to(
                icon_label(icon::DOWNLOAD_SIMPLE, "Download platform-tools from Google"),
                "https://developer.android.com/tools/releases/platform-tools",
            );
            ui.add_space(6.0);
            if theme::primary_button(ui, p, &icon_label(icon::ARROWS_CLOCKWISE, "Look again"), true).clicked() {
                self.looked = false;
            }
        });
    }

    fn devices_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut restart = false;
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::semibold(tr("Devices"), 15.0).color(p.text));
                if !self.version.is_empty() {
                    ui.label(RichText::new(&self.version).color(p.weak).size(12.5));
                }
                if ui
                    .button(icon_label(icon::ARROWS_CLOCKWISE, "Restart adb"))
                    .on_hover_text(tr(
                        "Stops and starts adb's background program: fixes most “offline” and “not found” states",
                    ))
                    .clicked()
                {
                    restart = true;
                }
            });
            if let Some(e) = &self.list_error {
                ui.add_space(4.0);
                theme::paragraph(ui, e, 13.0, p.danger);
            }
            if self.devices.is_empty() && self.list_error.is_none() {
                ui.add_space(4.0);
                theme::paragraph(
                    ui,
                    trl(
                        "No device yet. On the phone: Settings → About phone → tap Build number seven times, then Settings → System → Developer options → turn on USB debugging. Plug it in with a data cable (some cables only charge) and allow this computer on the phone's screen.",
                    ),
                    13.5,
                    p.weak,
                );
            }
            let mut pick = None;
            egui::ScrollArea::vertical().id_salt("adb-devices").max_height(130.0).show(ui, |ui| {
                for d in &self.devices {
                    ui.horizontal_wrapped(|ui| {
                        let (color, state) = match &d.state {
                            State::Ready => (p.success, tr("Ready").to_string()),
                            State::Unauthorized => {
                                (p.warning, tr("Waiting for you to allow it on the phone").to_string())
                            }
                            State::Offline => (p.warning, tr("Offline").to_string()),
                            State::Busy => (p.weak, tr("Connecting…").to_string()),
                            State::NoPermissions => (p.danger, tr("No permission to use it").to_string()),
                            State::Other(s) => (p.weak, s.clone()),
                        };
                        let chosen = d.serial == self.selected;
                        if ui.selectable_label(chosen, RichText::new(d.name()).color(p.text)).clicked() {
                            pick = Some(d.serial.clone());
                        }
                        ui.label(RichText::new("●").color(color).size(10.0));
                        ui.label(RichText::new(state).color(color).size(13.0));
                        if d.name() != d.serial {
                            ui.label(RichText::new(&d.serial).color(p.weak).monospace().size(12.0));
                        }
                        if d.wireless() {
                            theme::pill(ui, p, tr("Wi-Fi"), p.accent);
                        }
                    });
                }
            });
            if let Some(serial) = pick {
                self.selected = serial;
            }
            if let Some(d) = self.selected_device() {
                let hint = match &d.state {
                    State::Ready => None,
                    State::Unauthorized => Some(trl(
                        "On the phone's screen tap Allow on “Allow USB debugging?” (tick “Always allow from this computer”). If the question never appears: unplug, then Settings → Developer options → Revoke USB debugging authorizations, and plug in again.",
                    )),
                    State::Offline => Some(trl(
                        "The device is listed but does not answer. Unlock the screen, unplug and plug it in again, or click Restart adb. A wireless device that left the network has to be connected again.",
                    )),
                    State::NoPermissions => Some(trl(
                        "Linux does not let your user use this USB device. Install the Android udev rules (package android-udev-rules or android-sdk-platform-tools-common) and make sure you are in the plugdev group, then plug it in again.",
                    )),
                    State::Busy => Some(trl("The device is being set up. Wait a moment: it should be ready soon.")),
                    State::Other(_) => Some(trl(
                        "The device is in a special mode (recovery, sideload or bootloader). The shell and log need the normal system.",
                    )),
                };
                if let Some(h) = hint {
                    ui.add_space(4.0);
                    theme::notice(ui, p, p.warning, icon::WARNING, h);
                }
            } else if self.devices.len() > 1 {
                ui.add_space(4.0);
                theme::paragraph(ui, trl("More than one device is connected: click the one to use."), 13.5, p.weak);
            }
            // After the first look at the list: it opens for a computer without devices.
            if self.listed.is_some() {
                self.wireless_ui(ui, p, sh);
            }
        });
        if restart {
            self.start(&ui.ctx().clone(), tr("Restart adb").to_string(), |adb| adb.restart_server());
        }
    }

    fn wireless_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut pair = false;
        let mut connect = false;
        egui::CollapsingHeader::new(RichText::new(icon_label(icon::WIFI_HIGH, "Connect over Wi-Fi")).color(p.text))
            .id_salt("adb-wireless")
            .default_open(self.devices.is_empty())
            .show(ui, |ui| {
                theme::paragraph(
                    ui,
                    trl(
                        "Android 11 or newer, phone and computer on the same Wi-Fi. On the phone: Developer options → Wireless debugging → turn it on. Tap “Pair device with pairing code” and type the address, port and six digits it shows below. Then connect with the address and port shown on the main Wireless debugging screen (a different port).",
                    ),
                    13.0,
                    p.weak,
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr("Pair"));
                    theme::field(
                        ui,
                        egui::TextEdit::singleline(&mut self.pair_address).hint_text("192.168.1.20:37099").desired_width(190.0),
                    );
                    let r = theme::field(
                        ui,
                        egui::TextEdit::singleline(&mut self.pair_code).hint_text(tr("six digits")).desired_width(90.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ok = !self.pair_address.trim().is_empty()
                        && self.pair_code.chars().filter(|c| c.is_ascii_digit()).count() == 6
                        && self.job.is_none();
                    if (ui.add_enabled(ok, egui::Button::new(icon_label(icon::LINK, "Pair"))).clicked() || enter) && ok {
                        pair = true;
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr("Connect"));
                    let r = theme::field(
                        ui,
                        egui::TextEdit::singleline(&mut self.connect_address)
                            .hint_text("192.168.1.20:41234")
                            .desired_width(190.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ok = !self.connect_address.trim().is_empty() && self.job.is_none();
                    if (theme::primary_button(ui, p, &icon_label(icon::PLUGS_CONNECTED, "Connect"), ok).clicked() || enter)
                        && ok
                    {
                        connect = true;
                    }
                    if let Some(d) = self.selected_device().filter(|d| d.wireless())
                        && ui.button(icon_label(icon::PLUGS, "Disconnect")).clicked()
                    {
                        let serial = d.serial.clone();
                        self.start(&ui.ctx().clone(), tr("Disconnect").to_string(), move |adb| {
                            adb.run(None, &["disconnect", &serial])
                        });
                    }
                });
            });
        let ctx = ui.ctx().clone();
        if pair {
            let (address, code) = (self.pair_address.clone(), self.pair_code.clone());
            self.start(&ctx, tr("Pair").to_string(), move |adb| adb.pair(&address, &code));
            self.pair_code.clear();
            sh.toast(tr("Pairing…"));
        }
        if connect {
            let address = self.connect_address.clone();
            // Plain names and IPv4 addresses are remembered, with no port.
            if address.matches(':').count() <= 1 && !address.contains('[') {
                sh.settings.remember_host(address.split(':').next().unwrap_or(&address));
            }
            self.start(&ctx, tr("Connect").to_string(), move |adb| adb.connect(&address));
        }
    }

    fn shell_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let since = ui.cursor().top();
        let serial = self.ready_serial();
        ui.horizontal_wrapped(|ui| {
            if self.shell.is_some() {
                if theme::danger_button(ui, p, &icon_label(icon::PLUGS, "Close shell")).clicked() {
                    self.shell = None;
                }
                if ui.button(icon_label(icon::MAGNIFYING_GLASS, "Find")).clicked() {
                    self.shell_term.open_find();
                }
                crate::console::highlight_toggle(ui, sh);
                if ui.button(icon_label(icon::COPY, "Copy screen")).clicked() {
                    ui.ctx().copy_text(self.shell_term.text());
                    sh.toast(tr("Copied."));
                }
                if ui
                    .button(icon_label(icon::CLIPBOARD_TEXT, "Paste"))
                    .on_hover_text(tr("Send the clipboard (for example a configuration)"))
                    .clicked()
                {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                }
            } else {
                let ok = serial.is_some();
                if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Open shell"), ok).clicked()
                    && let (Some(adb), Some(serial)) = (&self.adb, &serial)
                {
                    let (rows, cols) = self.size;
                    match adb.shell_session(serial, rows, cols) {
                        Ok(r) => {
                            self.shell = Some(r);
                            self.shell_term = crate::term::Term::new(1_000_001);
                        }
                        Err(e) => sh.fail(trl("The shell could not be opened."), &e),
                    }
                }
                if !ok {
                    ui.label(RichText::new(tr("Choose a device that is ready.")).color(p.weak));
                }
            }
        });
        ui.add_space(4.0);
        let height = self.terminal_height(ui, since);
        self.shell_term.set_highlight(sh.settings.term_highlight);
        let out = ui
            .allocate_ui(egui::vec2(ui.available_width(), height), |ui| self.shell_term.ui(ui, p.border, p.accent))
            .inner;
        if out.copied {
            sh.toast(tr("Copied."));
        }
        self.size = self.shell_term.size();
        if let Some(r) = &self.shell {
            if let Some((rows, cols)) = out.resized {
                r.resize(rows, cols);
            }
            if !out.input.is_empty()
                && let Err(e) = r.send(&out.input)
            {
                self.shell = None;
                sh.fail(trl("The session closed."), &e);
            }
        }
    }

    fn log_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let since = ui.cursor().top();
        let serial = self.ready_serial();
        let running = self.logcat.is_some();
        let mut start = false;
        ui.horizontal_wrapped(|ui| {
            let level = LEVELS.iter().find(|(c, _)| *c == self.filter.level).map_or("Verbose", |(_, n)| *n);
            egui::ComboBox::from_id_salt("adb-level")
                .selected_text(trf("{level} and above", &[("level", &tr_level(level))]))
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (c, name) in LEVELS {
                        ui.selectable_value(&mut self.filter.level, c, tr_level(name));
                    }
                });
            let r = theme::field(
                ui,
                egui::TextEdit::singleline(&mut self.filter.text).hint_text(tr("Text to find")).desired_width(160.0),
            );
            let r2 = theme::field(
                ui,
                egui::TextEdit::singleline(&mut self.filter.package)
                    .hint_text(tr("App: com.example.app"))
                    .desired_width(190.0),
            );
            let enter = (r.lost_focus() || r2.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
            egui::ComboBox::from_id_salt("adb-buffer")
                .selected_text(match self.filter.buffer.as_str() {
                    "" => tr("Usual buffers").to_string(),
                    "crash" => tr("Crashes").to_string(),
                    "all" => tr("All buffers").to_string(),
                    other => other.to_string(),
                })
                .width(130.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter.buffer, String::new(), tr("Usual buffers"));
                    ui.selectable_value(&mut self.filter.buffer, "crash".to_string(), tr("Crashes"));
                    ui.selectable_value(&mut self.filter.buffer, "all".to_string(), tr("All buffers"));
                });
            let label =
                if running { icon_label(icon::ARROW_CLOCKWISE, "Apply") } else { icon_label(icon::PLAY, "Start") };
            if (theme::primary_button(ui, p, &label, serial.is_some()).clicked() || (enter && serial.is_some()))
                && serial.is_some()
            {
                start = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            if running && theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked() {
                self.logcat = None;
            }
            if ui.button(icon_label(icon::BROOM, "Clear screen")).clicked() {
                self.log_term = crate::term::Term::new(1_000_002);
            }
            if ui
                .add_enabled(serial.is_some(), egui::Button::new(icon_label(icon::TRASH, "Empty the device's log")))
                .on_hover_text(tr("Deletes the log kept on the device, so that new lines are easy to find"))
                .clicked()
                && let Some(serial) = serial.clone()
            {
                self.start(&ui.ctx().clone(), tr("Empty the device's log").to_string(), move |adb| {
                    adb.run(Some(&serial), &["logcat", "-c"]).map(|_| String::new())
                });
                self.sub = Sub::Log;
            }
            if ui.button(icon_label(icon::MAGNIFYING_GLASS, "Find")).clicked() {
                self.log_term.open_find();
            }
            crate::console::highlight_toggle(ui, sh);
            if ui.button(icon_label(icon::COPY, "Copy screen")).clicked() {
                ui.ctx().copy_text(self.log_term.text());
                sh.toast(tr("Copied."));
            }
            let logging = self.log_file.is_some();
            if ui
                .button(icon_label(icon::FLOPPY_DISK, if logging { tr("Stop saving") } else { tr("Save to file…") }))
                .on_hover_text(tr("Writes every new line to a file, not only what is on the screen"))
                .clicked()
            {
                if logging {
                    self.log_file = None;
                } else if let Some(path) = rfd::FileDialog::new().set_file_name("logcat.txt").save_file() {
                    match std::fs::File::create(&path) {
                        Ok(mut f) => {
                            let _ = f.write_all(self.log_term.text().as_bytes());
                            self.log_file = Some(f);
                        }
                        Err(e) => sh.fail(trl("The log file could not be created."), &e.into()),
                    }
                }
            }
        });
        if start
            && self.log_starting.is_none()
            && let (Some(adb), Some(serial)) = (&self.adb, &serial)
        {
            // Looking up an app's process asks the device: away from the screen.
            let (rows, cols) = self.size;
            self.logcat = None;
            let (adb, serial, filter, ctx) = (adb.clone(), serial.clone(), self.filter.clone(), ui.ctx().clone());
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(adb.logcat_session(&serial, &filter, rows, cols).map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
            self.log_starting = Some(rx);
        }
        ui.add_space(4.0);
        let height = self.terminal_height(ui, since);
        self.log_term.set_highlight(sh.settings.term_highlight);
        let out = ui
            .allocate_ui(egui::vec2(ui.available_width(), height), |ui| self.log_term.ui(ui, p.border, p.accent))
            .inner;
        if out.copied {
            sh.toast(tr("Copied."));
        }
        self.size = self.log_term.size();
        if let (Some(r), Some((rows, cols))) = (&self.logcat, out.resized) {
            r.resize(rows, cols);
        }
    }

    fn actions_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let ctx = ui.ctx().clone();
        let serial = self.ready_serial();
        let busy = self.job.is_some();
        let on = serial.is_some() && !busy;
        let Some(s) = serial.clone() else {
            theme::paragraph(ui, trl("Choose a device that is ready to use these actions."), 14.0, p.weak);
            self.outcome_ui(ui, p, sh);
            return;
        };
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("About the device"), 15.0).color(p.text));
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(on, egui::Button::new(icon_label(icon::INFO, "Device information"))).clicked() {
                    let s = s.clone();
                    self.start(&ctx, tr("Device information").to_string(), move |adb| {
                        adb.shell(
                            &s,
                            "echo \"Model: $(getprop ro.product.manufacturer) $(getprop ro.product.model)\"; \
                             echo \"Android: $(getprop ro.build.version.release) (API $(getprop ro.build.version.sdk))\"; \
                             echo \"Build: $(getprop ro.build.display.id)\"; \
                             echo \"Security patch: $(getprop ro.build.version.security_patch)\"; \
                             dumpsys battery | grep -E 'level|powered|status'",
                        )
                    });
                }
                if ui.add_enabled(on, egui::Button::new(icon_label(icon::WIFI_HIGH, "IP address"))).clicked() {
                    let s = s.clone();
                    self.start(&ctx, tr("IP address").to_string(), move |adb| adb.shell(&s, "ip -o -4 addr show"));
                }
                if ui.add_enabled(on, egui::Button::new(icon_label(icon::SQUARES_FOUR, "Installed apps"))).clicked() {
                    let s = s.clone();
                    self.start(&ctx, tr("Installed apps").to_string(), move |adb| {
                        adb.shell(&s, "pm list packages -3")
                    });
                }
            });
            ui.add_space(6.0);
            ui.label(theme::semibold(tr("Screen and apps"), 15.0).color(p.text));
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(on, egui::Button::new(icon_label(icon::CAMERA, "Screenshot…"))).clicked()
                    && let Some(path) = rfd::FileDialog::new().set_file_name("screenshot.png").save_file()
                {
                    let s = s.clone();
                    self.start(&ctx, tr("Screenshot…").to_string(), move |adb| {
                        let png = adb.screenshot(&s)?;
                        std::fs::write(&path, png)?;
                        Ok(path.display().to_string())
                    });
                }
                if ui.add_enabled(on, egui::Button::new(icon_label(icon::PACKAGE, "Install an app (.apk)…"))).clicked()
                    && let Some(path) = rfd::FileDialog::new().add_filter("APK", &["apk"]).pick_file()
                {
                    let s = s.clone();
                    self.start(&ctx, tr("Install an app (.apk)…").to_string(), move |adb| adb.install(&s, &path));
                }
            });
            ui.add_space(6.0);
            ui.label(theme::semibold(tr("Files"), 15.0).color(p.text));
            ui.horizontal_wrapped(|ui| {
                theme::field(ui, egui::TextEdit::singleline(&mut self.send_to).desired_width(220.0));
                if ui.add_enabled(on, egui::Button::new(icon_label(icon::UPLOAD_SIMPLE, "Send a file…"))).clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_file()
                {
                    let (s, to) = (s.clone(), self.send_to.trim().to_string());
                    self.start(&ctx, tr("Send a file…").to_string(), move |adb| adb.push(&s, &path, &to));
                }
            });
            ui.horizontal_wrapped(|ui| {
                theme::field(
                    ui,
                    egui::TextEdit::singleline(&mut self.remote_path)
                        .hint_text("/sdcard/DCIM/Camera/photo.jpg")
                        .desired_width(220.0),
                );
                let ok = on && !self.remote_path.trim().is_empty() && !self.remote_path.ends_with('/');
                if ui.add_enabled(ok, egui::Button::new(icon_label(icon::DOWNLOAD_SIMPLE, "Get a file…"))).clicked() {
                    let from = self.remote_path.trim().to_string();
                    let name = from.rsplit('/').next().unwrap_or("file").to_string();
                    if let Some(path) = rfd::FileDialog::new().set_file_name(name).save_file() {
                        let s = s.clone();
                        self.start(&ctx, tr("Get a file…").to_string(), move |adb| adb.pull(&s, &from, &path));
                    }
                }
            });
            ui.add_space(6.0);
            ui.label(theme::semibold(tr("Connection and power"), 15.0).color(p.text));
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(on, egui::Button::new(icon_label(icon::WIFI_HIGH, "Switch to Wi-Fi")))
                    .on_hover_text(tr("For phones without Wireless debugging (before Android 11): listens on port 5555, then connect to the address shown"))
                    .clicked()
                {
                    let s = s.clone();
                    self.start(&ctx, tr("Switch to Wi-Fi").to_string(), move |adb| {
                        let address = first_address(&adb.shell(&s, "ip -o -4 addr show wlan0").unwrap_or_default());
                        adb.run(Some(&s), &["tcpip", "5555"])?;
                        Ok(match address {
                            Some(a) => format!("{a}:5555"),
                            None => String::new(),
                        })
                    });
                }
                for (label, args) in [
                    (tr("Restart the device"), &["reboot"][..]),
                    (tr("Restart into recovery"), &["reboot", "recovery"][..]),
                    (tr("Restart into bootloader"), &["reboot", "bootloader"][..]),
                ] {
                    // These restart the device: the first click only asks.
                    let armed = self.confirm == Some(label);
                    let text = if armed { tr("Click again to confirm") } else { label };
                    if ui.add_enabled(on, egui::Button::new(text)).clicked() {
                        if armed {
                            let s = s.clone();
                            self.start(&ctx, label.to_string(), move |adb| adb.run(Some(&s), args));
                        } else {
                            self.confirm = Some(label);
                        }
                    }
                }
            });
        });
        ui.add_space(8.0);
        self.outcome_ui(ui, p, sh);
    }

    fn outcome_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some((label, _)) = &self.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(label).color(p.weak));
            });
            return;
        }
        let Some(o) = &self.outcome else { return };
        // The Wi-Fi switch answers with the address to connect to.
        let text = if o.ok && o.label == tr("Switch to Wi-Fi") {
            if o.text.is_empty() {
                tr("Done. Find the phone's address in Settings → About phone → Status, then connect to it with :5555.")
                    .to_string()
            } else {
                trlf("Done. Unplug the cable and connect to {address}.", &[("address", &o.text)])
            }
        } else {
            o.text.clone()
        };
        if text.is_empty() && o.ok {
            theme::notice(ui, p, p.success, icon::CHECK_CIRCLE, trl("Done."));
            return;
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(theme::semibold(&o.label, 14.0).color(if o.ok { p.text } else { p.danger }));
                if ui.small_button(icon::COPY).on_hover_text(tr("Copy")).clicked() {
                    ui.ctx().copy_text(text.clone());
                    sh.toast(tr("Copied."));
                }
            });
            egui::ScrollArea::vertical().id_salt("adb-output").max_height(260.0).show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&text).monospace().size(12.5).color(if o.ok {
                        p.text
                    } else {
                        p.danger
                    }))
                    .selectable(true)
                    .wrap(),
                );
            });
        });
    }
}

fn tr_level(name: &str) -> String {
    match name {
        "Verbose" => tr("Verbose"),
        "Debug" => tr("Debug"),
        "Info" => tr("Info"),
        "Warning" => tr("Warning"),
        "Error" => tr("Error"),
        _ => tr("Fatal"),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phone_address_is_read_from_ip() {
        let out = "24: wlan0    inet 192.168.1.47/24 brd 192.168.1.255 scope global wlan0\\       valid_lft forever";
        assert_eq!(first_address(out).as_deref(), Some("192.168.1.47"));
        assert_eq!(first_address("nothing here"), None);
    }
}
