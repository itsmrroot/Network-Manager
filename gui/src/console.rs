//! Console: a serial console for switches, routers and firewalls, and SSH
//! and Telnet sessions (saved, in tabs) in a terminal inside the app.

use std::io::Write;
use std::time::Duration;

use eframe::egui::{self, Key, RichText, Sense, Ui};
use egui_phosphor::regular as icon;
use netmgr::console::{self as nc, Flow, Parity, PortInfo, Screen, Session};
use netmgr::remote::{Protocol, Remote, Saved};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf};
use crate::theme::{self, Palette, icon_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Serial,
    Remote,
    Backups,
}

/// An SSH or Telnet session in a tab.
struct Open {
    saved: Saved,
    remote: Option<Remote>,
    term: crate::term::Term,
}

pub struct Console {
    view: View,
    backups: crate::backups::Backups,
    open: Vec<Open>,
    active: usize,
    next_id: u64,
    /// The terminal's size last time it was shown: new sessions start with it.
    size: (u16, u16),
    search: String,
    /// The session being edited: its place in the list (`None`: a new one).
    editing: Option<(Option<usize>, Saved)>,
    ports: Vec<PortInfo>,
    session: Option<Session>,
    screen: Screen,
    focused: bool,
    log: Option<std::fs::File>,
    host: String,
    telnet: bool,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            view: View::Serial,
            backups: Default::default(),
            open: Vec::new(),
            active: 0,
            next_id: 0,
            size: (24, 80),
            search: String::new(),
            editing: None,
            ports: nc::ports(),
            session: None,
            screen: Screen::default(),
            focused: false,
            log: None,
            host: String::new(),
            telnet: false,
        }
    }
}

impl Console {
    pub fn connected(&self) -> bool {
        self.session.is_some() || self.open.iter().any(|o| o.remote.is_some())
    }

    /// Takes in what the devices sent (also while another page is shown).
    pub fn poll(&mut self) {
        for o in &mut self.open {
            let Some(r) = &o.remote else { continue };
            while let Ok(bytes) = r.received.try_recv() {
                o.term.feed(&bytes);
            }
            if r.ended() && r.received.try_recv().is_err() {
                o.remote = None;
                o.term.feed(format!("\r\n\x1b[2m[{}]\x1b[0m\r\n", trl("Session ended")).as_bytes());
            }
        }
        let Some(s) = &self.session else { return };
        while let Ok(bytes) = s.received.try_recv() {
            if let Some(f) = &mut self.log {
                let _ = f.write_all(&bytes);
            }
            self.screen.feed(&bytes);
        }
    }

    fn send(&mut self, sh: &mut Shared, bytes: &[u8]) {
        if let Some(s) = &self.session
            && let Err(e) = s.send(bytes)
        {
            self.session = None;
            sh.fail(trl("The console cable stopped answering."), &e);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        self.poll();
        if self.connected() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        theme::page_title(
            ui,
            p,
            tr("Console"),
            tr("Configure switches and routers over a console cable, or open SSH and Telnet sessions."),
        );
        theme::tabs(
            ui,
            p,
            &mut self.view,
            &[
                (View::Serial, icon::USB, tr("Serial console")),
                (View::Remote, icon::TERMINAL_WINDOW, tr("SSH and Telnet")),
                (View::Backups, icon::CLOUD_ARROW_DOWN, tr("Backups")),
            ],
        );
        ui.add_space(8.0);
        match self.view {
            View::Serial => {
                self.toolbar(ui, p, sh);
                ui.add_space(8.0);
                self.terminal(ui, p, sh);
            }
            View::Remote => self.remote_ui(ui, p, sh),
            View::Backups => self.backups.ui(ui, p, sh),
        }
        self.edit_window(ui.ctx(), p, sh);
    }

    /// Development aid: the SSH and Telnet tab with sample sessions, and a
    /// Telnet session to `target` (host:port) when given.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self, sh: &mut Shared, target: Option<&str>) {
        self.view = View::Remote;
        let s = |name: &str, group: &str, protocol, host: &str, user: &str| Saved {
            name: name.into(),
            group: group.into(),
            protocol,
            host: host.into(),
            port: 0,
            user: user.into(),
            backup_command: String::new(),
        };
        sh.settings.sessions = vec![
            s("core-sw1", "Building A", Protocol::Ssh, "10.10.0.2", "admin"),
            s("access-sw-2F", "Building A", Protocol::Ssh, "10.10.0.12", "admin"),
            s("edge-router", "Datacenter", Protocol::Ssh, "10.0.0.1", "netops"),
            s("old-ups", "Datacenter", Protocol::Telnet, "10.0.0.50", ""),
            s("lab-firewall", "", Protocol::Ssh, "192.168.50.1", "root"),
        ];
        if let Some(t) = target {
            let mut saved = Saved::parse(Protocol::Telnet, t);
            saved.name = "core-sw1".into();
            self.connect(saved, sh);
        }
    }

    fn connect(&mut self, saved: Saved, sh: &mut Shared) {
        let mut term = crate::term::Term::new(self.next_id);
        let (rows, cols) = self.size;
        term.feed(format!("\x1b[2m{} {} …\x1b[0m\r\n", saved.protocol.label(), saved.target()).as_bytes());
        match Remote::open(&saved, rows, cols) {
            Ok(r) => {
                sh.settings.remember_host(&saved.host);
                self.open.push(Open { saved, remote: Some(r), term });
                self.active = self.open.len() - 1;
                self.next_id += 1;
            }
            Err(e) => sh.fail(trl("The session could not be opened."), &e),
        }
    }

    fn remote_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        // Quick connect.
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.host)
                        .hint_text(tr("user@192.168.1.1 or a name"))
                        .desired_width(240.0),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.selectable_value(&mut self.telnet, false, "SSH");
                ui.selectable_value(&mut self.telnet, true, tr("Telnet"));
                let ok = !self.host.trim().is_empty();
                let proto = if self.telnet { Protocol::Telnet } else { Protocol::Ssh };
                if (theme::primary_button(ui, p, &icon_label(icon::PLUGS_CONNECTED, "Connect"), ok).clicked() || enter)
                    && ok
                {
                    self.connect(Saved::parse(proto, &self.host), sh);
                }
                if ui
                    .add_enabled(ok, egui::Button::new(icon_label(icon::BOOKMARK_SIMPLE, "Save…")))
                    .on_hover_text(tr("Keep this session in the list"))
                    .clicked()
                {
                    self.editing = Some((None, Saved::parse(proto, &self.host)));
                }
                if ui
                    .add_enabled(ok, egui::Button::new(icon_label(icon::ARROW_SQUARE_OUT, "Open in terminal")))
                    .on_hover_text(tr("Open in the system's own terminal instead"))
                    .clicked()
                {
                    let host = self.host.trim().to_string();
                    let command = if self.telnet {
                        format!("telnet {}", host.rsplit('@').next().unwrap_or(&host))
                    } else {
                        format!("ssh {host}")
                    };
                    sh.settings.remember_host(&host);
                    if let Err(e) = nc::open_terminal(&command) {
                        sh.fail(trl("The terminal could not be opened."), &e);
                    }
                }
            });
        });
        ui.add_space(8.0);
        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(250.0, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(250.0);
                ui.set_min_height(height);
                self.session_list(ui, p, sh);
            });
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.set_min_height(height);
                self.session_tabs(ui, p, sh);
            });
        });
    }

    fn session_list(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height() - 30.0);
            ui.horizontal(|ui| {
                ui.label(theme::semibold(tr("Saved sessions"), 15.0).color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(icon::PLUS).on_hover_text(tr("New session")).clicked() {
                        self.editing = Some((None, Saved::default()));
                    }
                });
            });
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(tr("Search")).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            if sh.settings.sessions.is_empty() {
                theme::paragraph(
                    ui,
                    trl("Save the switches and routers you use often: click + or Save… after typing an address."),
                    13.0,
                    p.weak,
                );
                return;
            }
            let q = self.search.to_lowercase();
            let mut groups: Vec<String> = sh.settings.sessions.iter().map(|s| s.group.clone()).collect();
            groups.sort();
            groups.dedup();
            let mut connect = None;
            egui::ScrollArea::vertical().id_salt("sessions").show(ui, |ui| {
                for g in groups {
                    let items: Vec<(usize, &Saved)> = sh
                        .settings
                        .sessions
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.group == g)
                        .filter(|(_, s)| {
                            q.is_empty()
                                || s.name.to_lowercase().contains(&q)
                                || s.host.to_lowercase().contains(&q)
                                || s.group.to_lowercase().contains(&q)
                        })
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    let list = |ui: &mut Ui| {
                        for (i, s) in items {
                            ui.horizontal(|ui| {
                                let r = ui
                                    .add(
                                        egui::Label::new(RichText::new(&s.name).color(p.text))
                                            .sense(Sense::click())
                                            .truncate(),
                                    )
                                    .on_hover_text(format!(
                                        "{} {}\n{}",
                                        s.protocol.label(),
                                        s.target(),
                                        tr("Double-click to connect")
                                    ));
                                if r.double_clicked() {
                                    connect = Some(s.clone());
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button(icon::PENCIL_SIMPLE).on_hover_text(tr("Edit")).clicked() {
                                        self.editing = Some((Some(i), s.clone()));
                                    }
                                    if ui.small_button(icon::PLAY).on_hover_text(tr("Connect")).clicked() {
                                        connect = Some(s.clone());
                                    }
                                });
                            });
                        }
                    };
                    if g.is_empty() {
                        list(ui);
                    } else {
                        egui::CollapsingHeader::new(RichText::new(&g).color(p.weak))
                            .id_salt(("group", &g))
                            .default_open(true)
                            .show(ui, list);
                    }
                }
            });
            if let Some(s) = connect {
                self.connect(s, sh);
            }
        });
    }

    fn session_tabs(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if self.open.is_empty() {
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height() - 30.0);
                theme::paragraph(
                    ui,
                    trl(
                        "Type an address and click Connect, or double-click a saved session. Sessions open here in tabs; SSH uses the system's own ssh client, so your keys and ~/.ssh/config work as usual.",
                    ),
                    14.0,
                    p.weak,
                );
            });
            return;
        }
        self.active = self.active.min(self.open.len() - 1);
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (i, o) in self.open.iter().enumerate() {
                let live = o.remote.is_some();
                let dot = RichText::new("●").color(if live { p.success } else { p.weak }).size(10.0);
                let selected = i == self.active;
                let b = egui::Button::new(RichText::new(&o.saved.name).color(if selected { p.text } else { p.weak }))
                    .fill(if selected { p.tint(p.accent) } else { egui::Color32::TRANSPARENT })
                    .corner_radius(8);
                ui.label(dot);
                if ui.add(b).clicked() {
                    self.active = i;
                }
                if ui.small_button(icon::X).on_hover_text(tr("Close")).clicked() {
                    close = Some(i);
                }
                ui.add_space(6.0);
            }
        });
        if let Some(i) = close {
            self.open.remove(i);
            if self.open.is_empty() {
                return;
            }
            self.active = self.active.min(self.open.len() - 1);
        }
        let o = &mut self.open[self.active];
        let mut reconnect = false;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} {}", o.saved.protocol.label(), o.saved.target())).color(p.weak).size(12.5),
            );
            if o.remote.is_none() && ui.button(icon_label(icon::ARROW_CLOCKWISE, "Reconnect")).clicked() {
                reconnect = true;
            }
            if ui.button(icon_label(icon::COPY, "Copy screen")).clicked() {
                ui.ctx().copy_text(o.term.text());
                sh.toast(tr("Copied."));
            }
            if ui
                .button(icon_label(icon::CLIPBOARD_TEXT, "Paste"))
                .on_hover_text(tr("Send the clipboard (for example a configuration)"))
                .clicked()
            {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
        });
        ui.add_space(4.0);
        let out = o.term.ui(ui, p.border, p.accent);
        if let Some(r) = &o.remote {
            if let Some((rows, cols)) = out.resized {
                r.resize(rows, cols);
            }
            if !out.input.is_empty()
                && let Err(e) = r.send(&out.input)
            {
                o.remote = None;
                sh.fail(trl("The session closed."), &e);
            }
        }
        if reconnect {
            let saved = o.saved.clone();
            let (rows, cols) = o.term.size();
            match Remote::open(&saved, rows, cols) {
                Ok(r) => o.remote = Some(r),
                Err(e) => sh.fail(trl("The session could not be opened."), &e),
            }
        }
        if let Some(o) = self.open.get(self.active) {
            self.size = o.term.size();
        }
    }

    fn edit_window(&mut self, ctx: &egui::Context, p: &Palette, sh: &mut Shared) {
        let Some((index, mut s)) = self.editing.take() else { return };
        let mut keep = true;
        let mut save = false;
        let mut delete = false;
        egui::Window::new(if index.is_some() { tr("Edit session") } else { tr("New session") })
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Grid::new("session-edit").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(tr("Name"));
                    ui.add(
                        egui::TextEdit::singleline(&mut s.name).hint_text(tr("e.g. Core switch")).desired_width(240.0),
                    );
                    ui.end_row();
                    ui.label(tr("Group"));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut s.group).hint_text(tr("optional")).desired_width(160.0));
                        let mut groups: Vec<String> =
                            sh.settings.sessions.iter().map(|x| x.group.clone()).filter(|g| !g.is_empty()).collect();
                        groups.sort();
                        groups.dedup();
                        if !groups.is_empty() {
                            egui::ComboBox::from_id_salt("groups").selected_text("").width(30.0).show_ui(ui, |ui| {
                                for g in groups {
                                    if ui.selectable_label(s.group == g, &g).clicked() {
                                        s.group = g;
                                    }
                                }
                            });
                        }
                    });
                    ui.end_row();
                    ui.label(tr("Protocol"));
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.protocol, Protocol::Ssh, "SSH");
                        ui.selectable_value(&mut s.protocol, Protocol::Telnet, tr("Telnet"));
                    });
                    ui.end_row();
                    ui.label(tr("Address"));
                    ui.add(egui::TextEdit::singleline(&mut s.host).hint_text("192.168.1.1").desired_width(240.0));
                    ui.end_row();
                    ui.label(tr("Port"));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut s.port).range(0..=65535));
                        ui.label(
                            RichText::new(trf("0 = {port}", &[("port", &s.protocol.default_port())])).color(p.weak),
                        );
                    });
                    ui.end_row();
                    if s.protocol == Protocol::Ssh {
                        ui.label(tr("User"));
                        ui.add(egui::TextEdit::singleline(&mut s.user).hint_text("admin").desired_width(240.0));
                        ui.end_row();
                        ui.label(tr("Backup"));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut s.backup_command)
                                    .hint_text(tr("command that prints the configuration"))
                                    .desired_width(200.0),
                            );
                            egui::ComboBox::from_id_salt("backup-cmd").selected_text("").width(30.0).show_ui(
                                ui,
                                |ui| {
                                    for (label, command) in netmgr::backup::COMMANDS {
                                        if ui
                                            .selectable_label(
                                                s.backup_command == command,
                                                format!("{label}: {command}"),
                                            )
                                            .clicked()
                                        {
                                            s.backup_command = command.into();
                                        }
                                    }
                                },
                            );
                        });
                        ui.end_row();
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(tr(
                        "Passwords are not stored: the device asks for them in the terminal. SSH keys work as usual.",
                    ))
                    .color(p.weak)
                    .size(12.5),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let ok = !s.host.trim().is_empty();
                    if theme::primary_button(ui, p, tr("Save"), ok).clicked() {
                        save = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        keep = false;
                    }
                    if index.is_some() && theme::danger_button(ui, p, &icon_label(icon::TRASH, "Delete")).clicked() {
                        delete = true;
                    }
                });
            });
        if save {
            s.host = s.host.trim().to_string();
            if s.name.trim().is_empty() {
                s.name = s.host.clone();
            }
            match index {
                Some(i) if i < sh.settings.sessions.len() => sh.settings.sessions[i] = s,
                _ => sh.settings.sessions.push(s),
            }
            sh.settings
                .sessions
                .sort_by(|a, b| (&a.group, a.name.to_lowercase()).cmp(&(&b.group, b.name.to_lowercase())));
            if let Err(e) = netmgr::remote::save(&sh.settings.sessions) {
                sh.fail(trl("The sessions could not be saved."), &e);
            }
        } else if delete {
            if let Some(i) = index.filter(|&i| i < sh.settings.sessions.len()) {
                sh.settings.sessions.remove(i);
                if let Err(e) = netmgr::remote::save(&sh.settings.sessions) {
                    sh.fail(trl("The sessions could not be saved."), &e);
                }
            }
        } else if keep {
            self.editing = Some((index, s));
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let connected = self.session.is_some();
                ui.add_enabled_ui(!connected, |ui| {
                    let shown = if sh.settings.serial_port.is_empty() {
                        tr("Choose a port").to_string()
                    } else {
                        sh.settings.serial_port.clone()
                    };
                    egui::ComboBox::from_id_salt("serial-port").selected_text(shown).width(230.0).show_ui(ui, |ui| {
                        for port in &self.ports {
                            ui.selectable_value(
                                &mut sh.settings.serial_port,
                                port.name.clone(),
                                format!("{}  —  {}", port.name, port.description),
                            );
                        }
                        if self.ports.is_empty() {
                            ui.label(tr("No serial ports: plug in the console cable"));
                        }
                    });
                    if ui
                        .add(egui::Button::new(icon::ARROWS_CLOCKWISE).frame(false))
                        .on_hover_text(tr("Look for ports again"))
                        .clicked()
                    {
                        self.ports = nc::ports();
                    }
                    let s = &mut sh.settings.serial;
                    egui::ComboBox::from_id_salt("baud")
                        .selected_text(trf("{n} baud", &[("n", &s.baud)]))
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for b in nc::BAUD_RATES {
                                ui.selectable_value(&mut s.baud, b, trf("{n} baud", &[("n", &b)]));
                            }
                        });
                    let parity = match s.parity {
                        Parity::None => "N",
                        Parity::Odd => "O",
                        Parity::Even => "E",
                    };
                    egui::ComboBox::from_id_salt("framing")
                        .selected_text(format!("{}{}{}", s.data_bits, parity, s.stop_bits))
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            for (d, par, st, label) in [
                                (8, Parity::None, 1, "8N1"),
                                (7, Parity::Even, 1, "7E1"),
                                (7, Parity::Odd, 1, "7O1"),
                                (8, Parity::None, 2, "8N2"),
                            ] {
                                if ui.selectable_label(false, label).clicked() {
                                    s.data_bits = d;
                                    s.parity = par;
                                    s.stop_bits = st;
                                }
                            }
                        });
                    egui::ComboBox::from_id_salt("flow")
                        .selected_text(match s.flow {
                            Flow::None => tr("No flow control"),
                            Flow::Software => "XON/XOFF",
                            Flow::Hardware => "RTS/CTS",
                        })
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.flow, Flow::None, tr("No flow control"));
                            ui.selectable_value(&mut s.flow, Flow::Software, "XON/XOFF");
                            ui.selectable_value(&mut s.flow, Flow::Hardware, "RTS/CTS");
                        });
                });
                if connected {
                    if theme::danger_button(ui, p, &icon_label(icon::PLUGS, "Disconnect")).clicked() {
                        self.session = None;
                        self.log = None;
                    }
                    if ui
                        .button(icon_label(icon::LIGHTNING, "Send break"))
                        .on_hover_text(tr("For password recovery (ROMMON) on Cisco and others"))
                        .clicked()
                        && let Some(s) = &self.session
                        && let Err(e) = s.send_break()
                    {
                        sh.fail(trl("The break could not be sent."), &e);
                    }
                    if ui
                        .button(icon_label(icon::CLIPBOARD_TEXT, "Paste"))
                        .on_hover_text(tr("Send the clipboard (for example a configuration)"))
                        .clicked()
                    {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                        self.focused = true;
                    }
                } else if theme::primary_button(
                    ui,
                    p,
                    &icon_label(icon::PLUGS_CONNECTED, "Connect"),
                    !sh.settings.serial_port.is_empty(),
                )
                .clicked()
                {
                    match Session::open(&sh.settings.serial_port, sh.settings.serial) {
                        Ok(s) => {
                            self.session = Some(s);
                            self.focused = true;
                            self.screen.feed(
                                format!(
                                    "\r\n[{}]\r\n",
                                    trlf(
                                        "Connected to {port} at {baud} baud — press Enter",
                                        &[("port", &sh.settings.serial_port), ("baud", &sh.settings.serial.baud)]
                                    )
                                )
                                .as_bytes(),
                            );
                        }
                        Err(e) => sh.fail(trl("The port could not be opened."), &e),
                    }
                }
                if ui.button(icon_label(icon::BROOM, "Clear")).clicked() {
                    self.screen = Screen::default();
                }
                let logging = self.log.is_some();
                if ui
                    .button(icon_label(icon::FLOPPY_DISK, if logging { tr("Stop log") } else { tr("Log to file…") }))
                    .clicked()
                {
                    if logging {
                        self.log = None;
                    } else if let Some(path) = rfd::FileDialog::new().set_file_name("console.log").save_file() {
                        match std::fs::File::create(&path) {
                            Ok(mut f) => {
                                let _ = f.write_all(self.screen.text().as_bytes());
                                self.log = Some(f);
                            }
                            Err(e) => sh.fail(trl("The log file could not be created."), &e.into()),
                        }
                    }
                }
            });
        });
    }

    fn terminal(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let connected = self.session.is_some();
        let frame = egui::Frame::new()
            .fill(egui::Color32::from_rgb(8, 10, 14))
            .stroke(egui::Stroke::new(
                if self.focused && connected { 1.5 } else { 1.0 },
                if self.focused && connected { p.accent } else { p.border },
            ))
            .corner_radius(10)
            .inner_margin(12);
        let resp = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height().max(200.0) - 30.0);
                egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
                    if self.screen.lines.is_empty() {
                        let hint = if connected {
                            tr("Connected. Click here and type.")
                        } else if self.ports.is_empty() {
                            tr("Plug in the console cable (USB or a USB-serial adapter); install its driver if the system does not list it, then click ⟳.")
                        } else {
                            tr("Choose the port and click Connect. Most switches and routers use 9600 baud, 8N1, no flow control.")
                        };
                        ui.label(RichText::new(hint).color(p.weak));
                    }
                    let text = self.screen.text();
                    ui.add(egui::Label::new(RichText::new(text).monospace().size(13.0).color(egui::Color32::from_rgb(220, 226, 235))).extend());
                });
            })
            .response;
        let click = ui.interact(resp.rect, egui::Id::new("terminal-focus"), Sense::click());
        if click.clicked() {
            self.focused = true;
        } else if ui.input(|i| i.pointer.any_click()) && !click.hovered() {
            self.focused = false;
        }
        if !(connected && self.focused) {
            return;
        }
        // Every key goes to the device as it is typed: Tab completion, "?"
        // help and Ctrl+Shift+6 work as on a real terminal.
        let events = ui.input(|i| i.events.clone());
        let mut out: Vec<u8> = Vec::new();
        for e in events {
            match e {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Paste(t) => out.extend_from_slice(t.replace("\r\n", "\r").replace('\n', "\r").as_bytes()),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    // The physical Control key on every system (also on a Mac).
                    if modifiers.ctrl {
                        // Ctrl+letter: control characters (Ctrl+C = 0x03, Ctrl+Z = 0x1a).
                        let name = key.name();
                        if name.len() == 1
                            && let Some(c) = name.chars().next().filter(|c| c.is_ascii_alphabetic())
                        {
                            out.push((c.to_ascii_uppercase() as u8) - b'@');
                            continue;
                        }
                        if key == Key::Num6 && modifiers.shift {
                            out.push(0x1e);
                            continue;
                        }
                    }
                    match key {
                        Key::Enter => out.push(b'\r'),
                        Key::Backspace => out.push(0x08),
                        Key::Tab => out.push(b'\t'),
                        Key::Escape => out.push(0x1b),
                        Key::ArrowUp => out.extend_from_slice(b"\x1b[A"),
                        Key::ArrowDown => out.extend_from_slice(b"\x1b[B"),
                        Key::ArrowRight => out.extend_from_slice(b"\x1b[C"),
                        Key::ArrowLeft => out.extend_from_slice(b"\x1b[D"),
                        Key::Delete => out.push(0x7f),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            self.send(sh, &out);
        }
        // Keep Tab from moving the focus away.
        ui.ctx().memory_mut(|m| {
            m.set_focus_lock_filter(
                egui::Id::new("terminal-focus"),
                egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
    }
}
