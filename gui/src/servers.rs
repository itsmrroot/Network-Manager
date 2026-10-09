//! Servers: TFTP for firmware and configurations, a syslog receiver, and a
//! throughput test between two computers.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Align, Color32, Layout, RichText, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use netmgr::servers::{self as ns, Direction, SyslogMessage, Transfers};

use crate::app::Shared;
use crate::i18n::{tr, tr_dyn, trf, trl, trlf};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Tftp,
    Syslog,
    Throughput,
}

/// A running server: its stop switch, its log, and the error it ended with.
struct Running {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

impl Running {
    fn spawn(work: impl FnOnce(Arc<AtomicBool>) -> anyhow::Result<()> + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (s, e) = (stop.clone(), error.clone());
        std::thread::spawn(move || {
            if let Err(err) = work(s)
                && let Ok(mut e) = e.lock()
            {
                *e = Some(format!("{err:#}"));
            }
        });
        Self { stop, error }
    }

    fn failed(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
}

pub struct Servers {
    tab: Tab,
    tftp_port: u16,
    tftp_upload: bool,
    tftp_overwrite: bool,
    tftp: Option<Running>,
    transfers: Transfers,
    tftp_log: Arc<Mutex<Vec<String>>>,
    syslog_port: u16,
    syslog: Option<Running>,
    messages: Arc<Mutex<VecDeque<SyslogMessage>>>,
    min_severity: u8,
    filter: String,
    tp_server: Option<Running>,
    tp_log: Arc<Mutex<Vec<String>>>,
    tp_host: String,
    tp_download: bool,
    tp_seconds: u64,
    tp_job: Option<Job<f64>>,
    tp_samples: Arc<Mutex<Vec<f64>>>,
    tp_result: Option<(Direction, f64)>,
}

impl Default for Servers {
    fn default() -> Self {
        Self {
            tab: Tab::Tftp,
            tftp_port: 69,
            tftp_upload: true,
            tftp_overwrite: false,
            tftp: None,
            transfers: Default::default(),
            tftp_log: Default::default(),
            syslog_port: 514,
            syslog: None,
            messages: Default::default(),
            min_severity: 7,
            filter: String::new(),
            tp_server: None,
            tp_log: Default::default(),
            tp_host: String::new(),
            tp_download: false,
            tp_seconds: 10,
            tp_job: None,
            tp_samples: Default::default(),
            tp_result: None,
        }
    }
}

/// The addresses devices can reach this computer at.
fn my_addresses(sh: &Shared) -> Vec<String> {
    sh.visible_adapters()
        .iter()
        .filter(|a| a.up)
        .flat_map(|a| {
            a.ipv4.iter().filter(|(ip, _)| !ip.is_link_local()).map(move |(ip, _)| format!("{ip} ({})", a.name))
        })
        .collect()
}

fn severity_color(p: &Palette, s: u8) -> Color32 {
    match s {
        0..=3 => p.danger,
        4 => p.warning,
        5 => p.accent,
        _ => p.weak,
    }
}

impl Servers {
    /// Development aid: sample traffic for README screenshots.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        let msg = |sev: u8, from: [u8; 4], text: &str| SyslogMessage {
            received: std::time::SystemTime::now(),
            from: from.into(),
            facility: 23,
            severity: sev,
            text: text.into(),
            host: None,
        };
        if let Ok(mut m) = self.messages.lock() {
            m.extend([
                msg(6, [10, 0, 0, 2], "%SYS-6-LOGGINGHOST_STARTSTOP: Logging to host 192.168.1.10 port 514 started"),
                msg(5, [10, 0, 0, 2], "%LINEPROTO-5-UPDOWN: Line protocol on Interface GigabitEthernet1/0/12, changed state to up"),
                msg(3, [10, 0, 0, 3], "%LINK-3-UPDOWN: Interface GigabitEthernet0/7, changed state to down"),
                msg(4, [10, 0, 0, 2], "%SW_MATM-4-MACFLAP_NOTIF: Host 0011.2233.4455 in vlan 20 is flapping between port Gi1/0/3 and port Gi1/0/12"),
                msg(2, [10, 0, 0, 9], "%PM-2-ERR_DISABLE: bpduguard error detected on Gi0/4, putting Gi0/4 in err-disable state"),
                msg(5, [10, 0, 0, 3], "%SYS-5-CONFIG_I: Configured from console by admin on vty0 (192.168.1.10)"),
            ]);
        }
        self.syslog = Some(Running::spawn(|stop| {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(())
        }));
        self.tab = Tab::Syslog;
    }

    pub fn running(&self) -> bool {
        self.tftp.is_some() || self.syslog.is_some() || self.tp_server.is_some() || self.tp_job.is_some()
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if self.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(400));
        }
        theme::page_title(
            ui,
            p,
            tr("Servers"),
            tr("TFTP and syslog servers for network devices, and a throughput test between two computers."),
        );
        let badge =
            |on: bool, label: &'static str| if on { format!("{} ●", tr(label)) } else { tr(label).to_string() };
        let (t, s, tp) = (
            badge(self.tftp.is_some(), tr("TFTP server")),
            badge(self.syslog.is_some(), tr("Syslog server")),
            badge(self.tp_server.is_some(), tr("Throughput test")),
        );
        theme::tabs(
            ui,
            p,
            &mut self.tab,
            &[
                (Tab::Tftp, icon::HARD_DRIVES, &t),
                (Tab::Syslog, icon::LIST_BULLETS, &s),
                (Tab::Throughput, icon::GAUGE, &tp),
            ],
        );
        ui.add_space(10.0);
        match self.tab {
            Tab::Tftp => self.tftp_tab(ui, p, sh),
            Tab::Syslog => self.syslog_tab(ui, p, sh),
            Tab::Throughput => self.throughput_tab(ui, p, sh),
        }
    }

    fn tftp_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if sh.settings.tftp_folder.is_empty() {
            let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .unwrap_or_default();
            sh.settings.tftp_folder = home.join("TFTP").display().to_string();
        }
        if let Some(e) = self.tftp.as_ref().and_then(Running::failed) {
            self.tftp = None;
            sh.error = Some(format!("{}\n\n{e}", trl("The TFTP server could not start.")));
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            let running = self.tftp.is_some();
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("tftp-settings").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    ui.label(tr("Folder"));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut sh.settings.tftp_folder).desired_width(360.0));
                        if ui.button(icon_label(icon::FOLDER_OPEN, "Choose…")).clicked()
                            && let Some(d) = rfd::FileDialog::new().pick_folder()
                        {
                            sh.settings.tftp_folder = d.display().to_string();
                        }
                    });
                    ui.end_row();
                    ui.label(tr("Port"));
                    ui.add(egui::DragValue::new(&mut self.tftp_port).range(1..=65535));
                    ui.end_row();
                    ui.label(tr("Uploads"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.tftp_upload, tr("Let devices upload files (configuration backups)"));
                        if self.tftp_upload {
                            ui.checkbox(&mut self.tftp_overwrite, tr("Replace existing files"));
                        }
                    });
                    ui.end_row();
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if running {
                    if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop server")).clicked()
                        && let Some(r) = self.tftp.take()
                    {
                        r.stop.store(true, Ordering::Relaxed);
                    }
                    ui.spinner();
                    ui.label(RichText::new(tr("Running")).color(p.success));
                } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start server"), true).clicked() {
                    let root = PathBuf::from(sh.settings.tftp_folder.trim());
                    let _ = std::fs::create_dir_all(&root);
                    let opts = ns::TftpOptions {
                        root,
                        port: self.tftp_port,
                        allow_upload: self.tftp_upload,
                        overwrite: self.tftp_overwrite,
                    };
                    let (transfers, log) = (self.transfers.clone(), self.tftp_log.clone());
                    self.tftp = Some(Running::spawn(move |stop| {
                        ns::tftp_serve(
                            opts,
                            transfers,
                            stop,
                            Arc::new(move |l| {
                                if let Ok(mut v) = log.lock() {
                                    v.push(format!("{}  {l}", chrono::Local::now().format("%H:%M:%S")));
                                }
                            }),
                        )
                    }));
                }
                if ui.button(icon_label(icon::FOLDER_OPEN, "Open folder")).clicked() {
                    let _ = std::fs::create_dir_all(sh.settings.tftp_folder.trim());
                    crate::app::open_path(sh.settings.tftp_folder.trim());
                }
            });
            if running {
                let addrs = my_addresses(sh);
                let ip = addrs.first().and_then(|a| a.split(' ').next()).unwrap_or("this-computer").to_string();
                ui.add_space(8.0);
                theme::paragraph(
                    ui,
                    &trlf("Devices reach this server at {addresses}.", &[("addresses", &addrs.join(", "))]),
                    14.0,
                    p.text,
                );
                ui.add_space(4.0);
                ui.label(RichText::new(tr("Example commands")).color(p.weak).size(12.5));
                let examples = [
                    (
                        tr("Cisco IOS — firmware to the switch"),
                        format!("copy tftp://{ip}/c2960x-universalk9-mz.152-7.E9.bin flash:"),
                    ),
                    (tr("Cisco IOS — back up the configuration"), format!("copy running-config tftp://{ip}/sw1-confg")),
                    (tr("Aruba / HP"), format!("copy running-config tftp {ip} sw1.cfg")),
                    (tr("Juniper"), format!("file copy /config/juniper.conf.gz tftp://{ip}/")),
                ];
                for (what, command) in examples {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(what).color(p.weak).size(12.5));
                        let r = ui.add(
                            egui::Label::new(RichText::new(&command).monospace().color(p.text))
                                .sense(egui::Sense::click()),
                        );
                        if r.on_hover_text(tr("Click to copy")).clicked() {
                            ui.ctx().copy_text(command.clone());
                            sh.toast(tr("Copied."));
                        }
                    });
                }
            } else {
                ui.add_space(6.0);
                let note = if cfg!(target_os = "linux") {
                    trl(
                        "Ports below 1024 need administrator rights on Linux: start the app with sudo, or use a port of 1024 or more if the device can be told which port to use.",
                    )
                } else if cfg!(windows) {
                    trl("Windows Firewall may ask whether to allow the server: allow it for the networks you use.")
                } else {
                    trl("macOS may ask whether to accept incoming connections: allow them.")
                };
                theme::paragraph(ui, note, 12.5, p.weak);
            }
        });
        ui.add_space(10.0);
        let transfers = self.transfers.lock().map(|t| t.clone()).unwrap_or_default();
        if !transfers.is_empty() {
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::section_title(ui, p, icon::ARROWS_DOWN_UP, tr("Transfers"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(tr("Clear finished")).clicked()
                            && let Ok(mut t) = self.transfers.lock()
                        {
                            t.retain(|x| !x.done);
                        }
                    });
                });
                for t in transfers.iter().rev().take(20) {
                    ui.horizontal(|ui| {
                        let (g, c) = match (&t.error, t.done) {
                            (Some(_), _) => (icon::X_CIRCLE, p.danger),
                            (None, true) => (icon::CHECK_CIRCLE, p.success),
                            (None, false) => {
                                (if t.upload { icon::UPLOAD_SIMPLE } else { icon::DOWNLOAD_SIMPLE }, p.accent)
                            }
                        };
                        ui.label(RichText::new(g).color(c).size(17.0));
                        ui.label(RichText::new(&t.file).color(p.text));
                        ui.label(
                            RichText::new(format!("{} {}", if t.upload { tr("from") } else { tr("to") }, t.peer.ip()))
                                .color(p.weak)
                                .size(12.5),
                        );
                        let frac = t.total.filter(|x| *x > 0).map(|x| t.bytes as f32 / x as f32);
                        let mut bar = egui::ProgressBar::new(frac.unwrap_or(if t.done { 1.0 } else { 0.0 }))
                            .desired_width(180.0)
                            .fill(c);
                        if !t.done && frac.is_none() {
                            bar = bar.animate(true);
                        }
                        ui.add(bar);
                        let speed =
                            t.started.map(|s| t.bytes as f64 / s.elapsed().as_secs_f64().max(0.1)).unwrap_or(0.0);
                        ui.label(
                            RichText::new(match &t.error {
                                Some(e) => e.clone(),
                                None => format!(
                                    "{} · {}/s",
                                    netmgr::adapters::format_bytes(t.bytes),
                                    netmgr::adapters::format_bytes(speed as u64)
                                ),
                            })
                            .color(p.weak)
                            .size(12.5),
                        );
                    });
                }
            });
            ui.add_space(10.0);
        }
        log_card(ui, p, tr("Log"), &self.tftp_log);
    }

    fn syslog_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(e) = self.syslog.as_ref().and_then(Running::failed) {
            self.syslog = None;
            sh.error = Some(format!("{}\n\n{e}", trl("The syslog server could not start.")));
        }
        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.syslog.is_none(), |ui| {
                ui.label(tr("UDP port"));
                ui.add(egui::DragValue::new(&mut self.syslog_port).range(1..=65535));
            });
            if self.syslog.is_some() {
                if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked()
                    && let Some(r) = self.syslog.take()
                {
                    r.stop.store(true, Ordering::Relaxed);
                }
                ui.spinner();
            } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start"), true).clicked() {
                let (port, messages) = (self.syslog_port, self.messages.clone());
                self.syslog = Some(Running::spawn(move |stop| {
                    ns::syslog_serve(
                        port,
                        stop,
                        Arc::new(move |m| {
                            if let Ok(mut v) = messages.lock() {
                                v.push_back(m);
                                while v.len() > 20_000 {
                                    v.pop_front();
                                }
                            }
                        }),
                    )
                }));
            }
            ui.add_space(12.0);
            egui::ComboBox::from_id_salt("severity")
                .selected_text(trf(
                    "{severity} and worse",
                    &[("severity", &tr_dyn(ns::SEVERITIES[self.min_severity as usize]))],
                ))
                .width(150.0)
                .show_ui(ui, |ui| {
                    for (i, s) in ns::SEVERITIES.iter().enumerate().rev() {
                        ui.selectable_value(
                            &mut self.min_severity,
                            i as u8,
                            trf("{severity} and worse", &[("severity", &tr_dyn(s))]),
                        );
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Filter")))
                    .desired_width(180.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(icon_label(icon::TRASH, "Clear")).clicked()
                    && let Ok(mut m) = self.messages.lock()
                {
                    m.clear();
                }
                if ui.button(icon_label(icon::EXPORT, "Export…")).clicked() {
                    self.export_syslog(sh);
                }
            });
        });
        if self.syslog.is_some() {
            ui.label(
                RichText::new(trf(
                    "Point devices to {addresses} (for example: logging host {address}).",
                    &[
                        ("addresses", &my_addresses(sh).join(", ")),
                        ("address", &my_addresses(sh).first().and_then(|a| a.split(' ').next()).unwrap_or("?")),
                    ],
                ))
                .color(p.weak)
                .size(12.5),
            );
        }
        ui.add_space(8.0);
        let q = self.filter.to_lowercase();
        let msgs: Vec<SyslogMessage> = self
            .messages
            .lock()
            .map(|m| {
                m.iter()
                    .rev()
                    .filter(|x| {
                        x.severity <= self.min_severity
                            && (q.is_empty() || x.text.to_lowercase().contains(&q) || x.from.to_string().contains(&q))
                    })
                    .take(2000)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            if msgs.is_empty() {
                let text = if self.syslog.is_some() {
                    tr("Waiting for messages…")
                } else {
                    tr("Start the server, then tell your devices to send their logs to this computer.")
                };
                ui.label(RichText::new(text).color(p.weak));
                return;
            }
            let table_height = ui.available_height() - 10.0;
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .max_scroll_height(table_height)
                .column(Column::exact(80.0))
                .column(Column::exact(130.0))
                .column(Column::exact(96.0))
                .column(Column::remainder().clip(true))
                .header(24.0, |mut h| {
                    for t in [tr("Time"), tr("From"), tr("Severity"), tr("Message")] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).color(p.weak).size(13.0));
                        });
                    }
                })
                .body(|body| {
                    body.rows(26.0, msgs.len(), |mut row| {
                        let m = &msgs[row.index()];
                        row.col(|ui| {
                            let t: chrono::DateTime<chrono::Local> = m.received.into();
                            ui.label(
                                RichText::new(t.format("%H:%M:%S").to_string()).monospace().size(12.5).color(p.weak),
                            );
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(m.host.clone().unwrap_or_else(|| m.from.to_string())).size(13.0));
                        });
                        row.col(|ui| {
                            theme::pill(
                                ui,
                                p,
                                &tr_dyn(ns::SEVERITIES[m.severity as usize % 8]),
                                severity_color(p, m.severity),
                            );
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&m.text).monospace().size(12.5).color(p.text))
                                .on_hover_text(&m.text);
                        });
                    });
                });
        });
    }

    fn export_syslog(&self, sh: &mut Shared) {
        let Some(path) =
            rfd::FileDialog::new().add_filter("Text", &["log", "txt"]).set_file_name("syslog.log").save_file()
        else {
            return;
        };
        let text: String = self
            .messages
            .lock()
            .map(|m| {
                m.iter()
                    .map(|x| {
                        let t: chrono::DateTime<chrono::Local> = x.received.into();
                        format!(
                            "{} {} {} {}\n",
                            t.format("%Y-%m-%d %H:%M:%S"),
                            x.from,
                            ns::SEVERITIES[x.severity as usize % 8],
                            x.text
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        match std::fs::write(&path, text) {
            Ok(()) => sh.toast(trf("Saved to {file}.", &[("file", &path.display())])),
            Err(e) => sh.fail(trl("The file could not be saved."), &e.into()),
        }
    }

    fn throughput_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(e) = self.tp_server.as_ref().and_then(Running::failed) {
            self.tp_server = None;
            sh.error = Some(format!("{}\n\n{e}", trl("The throughput server could not start.")));
        }
        if let Some(r) = jobs::finished(&mut self.tp_job) {
            match r {
                Ok(v) => {
                    self.tp_result = Some((if self.tp_download { Direction::Download } else { Direction::Upload }, v))
                }
                Err(e) => sh.fail(trl("The throughput test did not work."), &e),
            }
        }
        theme::paragraph(
            ui,
            trl(
                "Measures the real speed of the network between two computers — a cable run, a switch, a Wi-Fi access point — without the internet in between. Start the server on one computer and the test on the other.",
            ),
            14.0,
            p.weak,
        );
        ui.add_space(10.0);
        ui.columns(2, |c| {
            theme::card(&mut c[0], p, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(240.0);
                theme::section_title(ui, p, icon::BROADCAST, tr("This computer as the server"));
                ui.add_space(4.0);
                if self.tp_server.is_some() {
                    ui.horizontal(|ui| {
                        if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop server")).clicked()
                            && let Some(r) = self.tp_server.take()
                        {
                            r.stop.store(true, Ordering::Relaxed);
                        }
                        ui.spinner();
                    });
                    theme::paragraph(ui, &trlf("On the other computer, test to {addresses} (port {port}).", &[("addresses", &my_addresses(sh).join(", ")), ("port", &ns::THROUGHPUT_PORT)]), 13.5, p.text);
                } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start server"), true).clicked() {
                    let log = self.tp_log.clone();
                    self.tp_server = Some(Running::spawn(move |stop| {
                        ns::throughput_serve(
                            ns::THROUGHPUT_PORT,
                            stop,
                            Arc::new(move |l| {
                                if let Ok(mut v) = log.lock() {
                                    v.push(format!("{}  {l}", chrono::Local::now().format("%H:%M:%S")));
                                }
                            }),
                        )
                    }));
                }
                ui.add_space(6.0);
                for l in self.tp_log.lock().map(|v| v.iter().rev().take(6).cloned().collect::<Vec<_>>()).unwrap_or_default() {
                    ui.label(RichText::new(l).monospace().size(12.0).color(p.weak));
                }
            });
            theme::card(&mut c[1], p, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(240.0);
                theme::section_title(ui, p, icon::GAUGE, tr("Test to another computer"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.tp_host).hint_text(tr("Its address, e.g. 192.168.1.20")).desired_width(200.0));
                    ui.add(egui::Slider::new(&mut self.tp_seconds, 3..=30).suffix(" s"));
                });
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tp_download, false, tr("Upload (this → other)"));
                    ui.selectable_value(&mut self.tp_download, true, tr("Download (other → this)"));
                });
                ui.add_space(4.0);
                if let Some(job) = &self.tp_job {
                    if ui.button(icon_label(icon::STOP, "Stop")).clicked() {
                        job.stop();
                    }
                } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start test"), !self.tp_host.trim().is_empty()).clicked() {
                    let (host, secs) = (self.tp_host.trim().to_string(), self.tp_seconds);
                    let dir = if self.tp_download { Direction::Download } else { Direction::Upload };
                    let samples = Arc::new(Mutex::new(Vec::new()));
                    self.tp_samples = samples.clone();
                    self.tp_result = None;
                    sh.settings.remember_host(&host);
                    self.tp_job = Some(Job::spawn(ui.ctx(), move |_, cancel| {
                        ns::throughput_test(&host, ns::THROUGHPUT_PORT, dir, secs, cancel, &|v| {
                            if let Ok(mut s) = samples.lock() {
                                s.push(v);
                            }
                        })
                    }));
                }
                let samples = self.tp_samples.lock().map(|s| s.clone()).unwrap_or_default();
                if let Some(v) = samples.last().filter(|_| self.tp_job.is_some()) {
                    ui.label(theme::semibold(format!("{v:.0} Mbit/s"), 24.0).color(p.accent));
                }
                if let Some((dir, v)) = self.tp_result {
                    let what = if dir == Direction::Upload { tr("Upload") } else { tr("Download") };
                    ui.label(theme::semibold(format!("{what}: {v:.0} Mbit/s"), 24.0).color(p.success));
                    let hint = match v {
                        v if v > 2000.0 => trl("Faster than 2.5 Gbit/s: a multi-gigabit link."),
                        v if v > 850.0 => trl("Gigabit speed: the cable and switch work at full speed."),
                        v if v > 85.0 => trl("About 100 Mbit/s or a good Wi-Fi link. On a cable this often means a damaged or 4-wire cable, or a 100 Mbit/s port."),
                        _ => trl("Slow: check the cable, the port speed, or the Wi-Fi signal."),
                    };
                    theme::paragraph(ui, hint, 12.5, p.weak);
                }
                if samples.len() > 1 {
                    let w = ui.available_width();
                    theme::sparkline(ui, &samples, p.accent, Vec2::new(w, 60.0), None);
                }
            });
        });
    }
}

fn log_card(ui: &mut Ui, p: &Palette, title: &str, log: &Arc<Mutex<Vec<String>>>) {
    let lines = log.lock().map(|l| l.clone()).unwrap_or_default();
    if lines.is_empty() {
        return;
    }
    theme::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        theme::section_title(ui, p, icon::LIST_BULLETS, title);
        egui::ScrollArea::vertical().max_height(220.0).stick_to_bottom(true).show(ui, |ui| {
            for l in lines.iter().rev().take(500).rev() {
                ui.label(RichText::new(l).monospace().size(12.5).color(p.text));
            }
        });
    });
}
