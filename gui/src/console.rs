//! Console: a serial console for switches, routers and firewalls, and
//! opening SSH or Telnet sessions in the system's terminal.

use std::io::Write;
use std::time::Duration;

use eframe::egui::{self, Key, RichText, Sense, Ui};
use egui_phosphor::regular as icon;
use netmgr::console::{self as nc, Flow, Parity, PortInfo, Screen, Session};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf};
use crate::theme::{self, Palette, icon_label};

pub struct Console {
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
        self.session.is_some()
    }

    /// Takes in what the device sent (also while another page is shown).
    pub fn poll(&mut self) {
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
        if self.session.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        theme::page_title(
            ui,
            p,
            tr("Console"),
            tr("Configure switches and routers over a console cable, or open SSH and Telnet sessions."),
        );
        self.toolbar(ui, p, sh);
        ui.add_space(8.0);
        self.terminal(ui, p, sh);
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
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::TERMINAL_WINDOW).color(p.accent));
                ui.add(
                    egui::TextEdit::singleline(&mut self.host)
                        .hint_text(tr("user@192.168.1.1 or a name"))
                        .desired_width(220.0),
                );
                ui.selectable_value(&mut self.telnet, false, "SSH");
                ui.selectable_value(&mut self.telnet, true, tr("Telnet"));
                if ui
                    .add_enabled(
                        !self.host.trim().is_empty(),
                        egui::Button::new(icon_label(icon::ARROW_SQUARE_OUT, "Open in terminal")),
                    )
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
