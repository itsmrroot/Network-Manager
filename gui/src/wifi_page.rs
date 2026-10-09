//! Wi-Fi: the network this computer is on, saved networks and their
//! passwords (with QR codes to share them), and the networks around.

use std::collections::{BTreeMap, HashSet};

use eframe::egui::{self, Align, CornerRadius, Layout, RichText, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use netmgr::wifi::{self, Connection, Nearby, SavedNetwork};

use crate::app::Shared;
use crate::i18n::{tr, tr_dyn, trf, trl};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

#[derive(Default)]
pub struct WifiPage {
    saved: Option<Vec<SavedNetwork>>,
    saved_job: Option<Job<Vec<SavedNetwork>>>,
    /// SSIDs whose passwords are shown.
    revealed: HashSet<String>,
    show_all: bool,
    password_job: Option<Job<(String, Option<String>)>>,
    nearby: Option<Vec<Nearby>>,
    nearby_job: Option<Job<Vec<Nearby>>>,
    reveal_job: Option<Job<(Option<String>, Option<String>)>>,
    /// The name and BSSID macOS hid, once shown.
    revealed_ssid: Option<(Option<String>, Option<String>)>,
    qr: Option<(String, Vec<Vec<bool>>, Option<String>)>,
    search: String,
    tab: Tab,
    roaming: crate::roaming::Roaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Saved,
    Nearby,
    Signal,
}

impl WifiPage {
    /// Development aid: the signal tab with sample data.
    #[cfg(debug_assertions)]
    pub fn show_signal(&mut self) {
        self.tab = Tab::Signal;
        self.roaming.demo();
    }

    /// Development aid: sample networks for README screenshots.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        let net = |ssid: &str, sec: &str, pw: Option<&str>| SavedNetwork {
            ssid: ssid.into(),
            security: Some(sec.into()),
            password: pw.map(str::to_string),
            password_read: true,
            hidden: false,
            id: String::new(),
        };
        self.saved = Some(vec![
            net("Home-5G", "WPA2-Personal", Some("correct-horse-battery")),
            net("Office", "WPA3-Personal", Some("Q3-roadmap-2026")),
            net("Lab-Switch", "WPA2-Personal", Some("lab2026!")),
            net("Cafe Guest", "Open", None),
            net("Studio", "WPA2-Personal", Some("blue-velvet-77")),
        ]);
        self.revealed.insert("Home-5G".into());
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        self.poll(sh);
        if self.saved.is_none() && self.saved_job.is_none() {
            self.saved_job = Some(Job::spawn(ui.ctx(), |_, _| wifi::saved()));
        }
        theme::page_title(
            ui,
            p,
            tr("Wi-Fi"),
            tr("The network you are on, saved networks with their passwords, and the networks around you."),
        );
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            self.current(ui, p, sh);
            ui.add_space(14.0);
            theme::tabs(
                ui,
                p,
                &mut self.tab,
                &[
                    (Tab::Saved, icon::KEY, tr("Saved networks")),
                    (Tab::Nearby, icon::BROADCAST, tr("Nearby networks")),
                    (Tab::Signal, icon::WAVE_SINE, tr("Signal and roaming")),
                ],
            );
            ui.add_space(8.0);
            match self.tab {
                Tab::Saved => self.saved_card(ui, p, sh),
                Tab::Nearby => {
                    if self.nearby.is_none() && self.nearby_job.is_none() {
                        self.nearby_job = Some(Job::spawn(ui.ctx(), |_, _| wifi::nearby()));
                    }
                    self.nearby_card(ui, p)
                }
                Tab::Signal => self.roaming.ui(ui, p),
            }
            ui.add_space(20.0);
        });
        self.qr_dialog(ui.ctx(), p);
    }

    fn poll(&mut self, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.saved_job) {
            match r {
                Ok(list) => self.saved = Some(list),
                Err(e) => {
                    self.saved = Some(Vec::new());
                    sh.fail(trl("The saved networks could not be read."), &e);
                }
            }
        }
        if let Some(r) = jobs::finished(&mut self.password_job) {
            match r {
                Ok((ssid, pw)) => {
                    if let Some(n) = self.saved.iter_mut().flatten().find(|n| n.ssid == ssid) {
                        n.password = pw;
                        n.password_read = true;
                    }
                    self.revealed.insert(ssid);
                }
                Err(e) => sh.fail(trl("The password could not be read."), &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.nearby_job) {
            match r {
                Ok(list) => self.nearby = Some(list),
                Err(e) => {
                    self.nearby = Some(Vec::new());
                    sh.fail(trl("The networks around could not be listed."), &e);
                }
            }
        }
        if let Some(r) = jobs::finished(&mut self.reveal_job) {
            match r {
                Ok(v) => self.revealed_ssid = Some(v),
                Err(e) => sh.fail(trl("The network name could not be read."), &e),
            }
        }
    }

    fn current(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let conns: Vec<Connection> = sh.wifi.clone();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            if conns.is_empty() {
                ui.horizontal(|ui| {
                    theme::icon_badge(ui, p, icon::WIFI_SLASH, p.weak, 52.0);
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        if sh.wifi_loaded {
                            ui.label(theme::semibold(tr("Not connected to Wi-Fi"), 19.0).color(p.text));
                            ui.label(RichText::new(tr("Join a network from the system's Wi-Fi menu.")).color(p.weak));
                        } else {
                            ui.label(theme::semibold(tr("Reading Wi-Fi…"), 19.0).color(p.text));
                        }
                    });
                });
                return;
            }
            for (i, c) in conns.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                let mut c = c.clone();
                if c.ssid.is_none()
                    && let Some((s, b)) = &self.revealed_ssid
                {
                    c.ssid = s.clone();
                    c.bssid = c.bssid.or(b.clone());
                }
                ui.horizontal(|ui| {
                    let q = c.quality().unwrap_or(0);
                    theme::icon_badge(
                        ui,
                        p,
                        icon::WIFI_HIGH,
                        if q >= 60 {
                            p.success
                        } else if q >= 40 {
                            p.warning
                        } else {
                            p.danger
                        },
                        56.0,
                    );
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        match &c.ssid {
                            Some(s) => {
                                ui.label(theme::semibold(s, 21.0).color(p.text));
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    ui.label(theme::semibold(tr("Name hidden by macOS"), 19.0).color(p.text));
                                    let busy = self.reveal_job.is_some();
                                    if ui
                                        .add_enabled(!busy, egui::Button::new(icon_label(icon::EYE, "Show name")))
                                        .on_hover_text(tr("macOS asks for your password"))
                                        .clicked()
                                    {
                                        self.reveal_job = Some(Job::spawn(ui.ctx(), |_, _| wifi::reveal_ssid()));
                                    }
                                    if busy {
                                        ui.spinner();
                                    }
                                });
                            }
                        }
                        ui.horizontal(|ui| {
                            if let Some(q) = c.quality() {
                                theme::signal_bars(ui, p, q, 15.0);
                                ui.label(
                                    RichText::new(format!("{} · {q} %", tr_dyn(wifi::quality_label(q))))
                                        .color(p.weak)
                                        .size(13.5),
                                );
                            }
                            ui.label(RichText::new(format!("· {}", c.interface)).color(p.weak).size(13.5));
                        });
                    });
                });
                ui.add_space(8.0);
                let mut copied = false;
                egui::Grid::new(("wifi-now", i)).num_columns(4).spacing([24.0, 8.0]).show(ui, |ui| {
                    let dash = || "—".to_string();
                    let ch = match (c.channel, &c.band, &c.width) {
                        (Some(ch), Some(b), Some(w)) => format!("{ch} · {b} · {w}"),
                        (Some(ch), Some(b), None) => format!("{ch} · {b}"),
                        (Some(ch), None, _) => ch.to_string(),
                        _ => dash(),
                    };
                    let signal = match (c.rssi, c.noise) {
                        (Some(r), Some(n)) => trf(
                            "{rssi} dBm (noise {noise} dBm, SNR {snr} dB)",
                            &[("rssi", &r), ("noise", &n), ("snr", &(r - n))],
                        ),
                        (Some(r), None) => format!("{r} dBm"),
                        _ => c.signal.map_or_else(dash, |s| format!("{s} %")),
                    };
                    let cells = [
                        (tr("Channel"), ch),
                        (tr("Signal"), signal),
                        (tr("Security"), c.security.clone().unwrap_or_else(dash)),
                        (tr("Standard"), c.standard.clone().unwrap_or_else(dash)),
                        (tr("Link rate"), c.rate_mbps.map_or_else(dash, |r| format!("{r:.0} Mbit/s"))),
                        (
                            tr("Access point"),
                            c.bssid.clone().map_or_else(dash, |b| {
                                let vendor = b.parse::<netmgr::mac::Mac>().ok().and_then(|m| m.vendor());
                                vendor.map_or(b.clone(), |v| format!("{b} ({v})"))
                            }),
                        ),
                    ];
                    for pair in cells.chunks(2) {
                        for (k, v) in pair {
                            ui.label(RichText::new(*k).color(p.weak).size(13.5));
                            let r = ui.add(egui::Label::new(RichText::new(v).color(p.text)).sense(Sense::click()));
                            if r.on_hover_text(tr("Click to copy")).clicked() {
                                ui.ctx().copy_text(v.clone());
                                copied = true;
                            }
                        }
                        ui.end_row();
                    }
                });
                if copied {
                    sh.toast(tr("Copied."));
                }
                if let Some(ssid) = c.ssid.clone()
                    && let Some(n) = self.saved.iter().flatten().find(|n| n.ssid == ssid).cloned()
                {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(icon_label(icon::QR_CODE, "Share with a QR code")).clicked() {
                            self.open_qr(ui.ctx(), &n);
                        }
                        if ui.button(icon_label(icon::KEY, "Show password")).clicked() {
                            self.show_password(ui.ctx(), &n);
                            self.tab = Tab::Saved;
                        }
                    });
                }
            }
        });
    }

    fn show_password(&mut self, ctx: &egui::Context, n: &SavedNetwork) {
        if n.password_read {
            self.revealed.insert(n.ssid.clone());
        } else if self.password_job.is_none() {
            let n = n.clone();
            self.password_job = Some(Job::spawn(ctx, move |_, _| Ok((n.ssid.clone(), wifi::password(&n)?))));
        }
    }

    fn open_qr(&mut self, ctx: &egui::Context, n: &SavedNetwork) {
        if !n.password_read && n.security.as_deref() != Some("Open") {
            // The password first; the QR code opens once it is known.
            self.show_password(ctx, n);
        }
        let text = wifi::qr_text(&n.ssid, n.security.as_deref(), n.password.as_deref(), n.hidden);
        if let Ok(m) = wifi::qr_modules(&text) {
            self.qr = Some((n.ssid.clone(), m, n.password.clone()));
        }
    }

    fn saved_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        // A QR code opened before its password was read gets it now.
        if let Some((ssid, _, None)) = &self.qr
            && let Some(n) = self.saved.iter().flatten().find(|n| &n.ssid == ssid && n.password.is_some()).cloned()
        {
            self.open_qr(ui.ctx(), &n);
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Search networks")))
                        .desired_width(240.0),
                );
                let list = self.saved.clone().unwrap_or_default();
                if !cfg!(target_os = "macos") {
                    let label = if self.show_all { tr("Hide passwords") } else { tr("Show all passwords") };
                    if ui.button(icon_label(if self.show_all { icon::EYE_SLASH } else { icon::EYE }, label)).clicked() {
                        self.show_all = !self.show_all;
                    }
                }
                if cfg!(target_os = "linux")
                    && list.iter().any(|n| !n.password_read)
                    && ui
                        .button(icon_label(icon::LOCK_OPEN, "Read with password"))
                        .on_hover_text(tr("Some passwords are only readable by administrators"))
                        .clicked()
                {
                    self.saved_job = Some(Job::spawn(ui.ctx(), |_, _| wifi::saved_as_admin()));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(!list.is_empty(), egui::Button::new(icon_label(icon::EXPORT, "Export…")))
                        .on_hover_text(tr("Save names and passwords to a CSV file"))
                        .clicked()
                    {
                        export(&list, sh);
                    }
                    if ui.button(icon_label(icon::ARROWS_CLOCKWISE, "Refresh")).clicked() {
                        self.saved_job = Some(Job::spawn(ui.ctx(), |_, _| wifi::saved()));
                    }
                });
            });
            ui.add_space(8.0);
            let Some(list) = self.saved.clone() else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(tr("Reading saved networks…")).color(p.weak));
                });
                return;
            };
            if self.saved_job.is_some() {
                ui.spinner();
            }
            if list.is_empty() {
                theme::paragraph(ui, trl("No saved Wi-Fi networks."), 14.0, p.weak);
                return;
            }
            let q = self.search.to_lowercase();
            let shown: Vec<&SavedNetwork> =
                list.iter().filter(|n| q.is_empty() || n.ssid.to_lowercase().contains(&q)).collect();
            let current = sh
                .wifi
                .iter()
                .filter_map(|c| c.ssid.clone())
                .chain(self.revealed_ssid.clone().and_then(|r| r.0))
                .collect::<Vec<_>>();
            let mut actions: Vec<(SavedNetwork, &str)> = Vec::new();
            TableBuilder::new(ui)
                .cell_layout(Layout::left_to_right(Align::Center))
                .striped(true)
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(150.0))
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(110.0))
                .header(26.0, |mut h| {
                    for t in [tr("Network"), tr("Security"), tr("Password"), ""] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).color(p.weak).size(13.0));
                        });
                    }
                })
                .body(|body| {
                    body.rows(34.0, shown.len(), |mut row| {
                        let n = shown[row.index()];
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(icon::WIFI_HIGH).color(if current.contains(&n.ssid) {
                                    p.success
                                } else {
                                    p.weak
                                }));
                                ui.label(RichText::new(&n.ssid).color(p.text));
                                if current.contains(&n.ssid) {
                                    theme::pill(ui, p, tr("Connected"), p.success);
                                }
                            });
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(n.security.as_deref().unwrap_or("—")).color(p.weak).size(13.5));
                        });
                        row.col(|ui| {
                            let visible = self.show_all || self.revealed.contains(&n.ssid);
                            let text = match (&n.password, n.password_read) {
                                (Some(pw), _) if visible => RichText::new(pw).monospace().color(p.text),
                                (Some(_), _) => RichText::new("••••••••••").color(p.weak),
                                (None, true) => RichText::new(tr("No password")).color(p.weak).italics(),
                                (None, false) => RichText::new("••••••••••").color(p.weak),
                            };
                            ui.label(text);
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let visible = self.show_all || self.revealed.contains(&n.ssid);
                                let eye = if visible && n.password.is_some() { icon::EYE_SLASH } else { icon::EYE };
                                if ui
                                    .add(egui::Button::new(eye).frame(false))
                                    .on_hover_text(tr("Show or hide the password"))
                                    .clicked()
                                {
                                    actions.push((n.clone(), "toggle"));
                                }
                                if n.password.is_some()
                                    && ui
                                        .add(egui::Button::new(icon::COPY).frame(false))
                                        .on_hover_text(tr("Copy the password"))
                                        .clicked()
                                {
                                    ui.ctx().copy_text(n.password.clone().unwrap_or_default());
                                    actions.push((n.clone(), "copied"));
                                }
                                if ui
                                    .add(egui::Button::new(icon::QR_CODE).frame(false))
                                    .on_hover_text(tr("QR code for phones"))
                                    .clicked()
                                {
                                    actions.push((n.clone(), "qr"));
                                }
                            });
                        });
                    });
                });
            for (n, what) in actions {
                match what {
                    "toggle" => {
                        if self.revealed.contains(&n.ssid) {
                            self.revealed.remove(&n.ssid);
                        } else {
                            self.show_password(ui.ctx(), &n);
                        }
                    }
                    "copied" => sh.toast(trf("Password of {network} copied.", &[("network", &n.ssid)])),
                    _ => self.open_qr(ui.ctx(), &n),
                }
            }
            if self.password_job.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let msg = if cfg!(target_os = "macos") {
                        tr("macOS asks for your name and password to show it…")
                    } else {
                        tr("Reading the password…")
                    };
                    ui.label(RichText::new(msg).color(p.weak));
                });
            }
            ui.add_space(6.0);
            let note = if cfg!(target_os = "macos") {
                trl("macOS keeps Wi-Fi passwords in its keychain and asks for your password before showing each one.")
            } else if cfg!(windows) {
                trl(
                    "Passwords are read from the Wi-Fi profiles Windows keeps. Only the networks this computer joined are listed.",
                )
            } else {
                trl("Passwords are read from NetworkManager. Some may need your password to be shown.")
            };
            theme::paragraph(ui, note, 12.5, p.weak);
        });
    }

    fn nearby_card(&mut self, ui: &mut Ui, p: &Palette) {
        let list = self.nearby.clone();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::BROADCAST, tr("Networks around you"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.nearby_job.is_some() {
                        ui.spinner();
                    } else if ui.button(icon_label(icon::ARROWS_CLOCKWISE, "Scan again")).clicked() {
                        self.nearby_job = Some(Job::spawn(ui.ctx(), |_, _| wifi::nearby()));
                    }
                });
            });
            let Some(list) = list else {
                ui.label(RichText::new(tr("Looking for networks…")).color(p.weak));
                return;
            };
            if list.is_empty() {
                theme::paragraph(ui, trl("No networks found. Is Wi-Fi turned on?"), 14.0, p.weak);
                return;
            }
            channel_chart(ui, p, &list);
            ui.add_space(10.0);
            TableBuilder::new(ui)
                .cell_layout(Layout::left_to_right(Align::Center))
                .id_salt("nearby")
                .striped(true)
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(130.0))
                .column(Column::exact(90.0))
                .column(Column::exact(210.0))
                .column(Column::remainder().at_least(120.0).clip(true))
                .header(26.0, |mut h| {
                    for t in [tr("Network"), tr("Signal"), tr("Channel"), tr("Band"), tr("Security")] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).color(p.weak).size(13.0));
                        });
                    }
                })
                .body(|body| {
                    body.rows(30.0, list.len(), |mut row| {
                        let n = &list[row.index()];
                        row.col(|ui| {
                            let name = n.ssid.clone().unwrap_or_else(|| {
                                if cfg!(target_os = "macos") {
                                    tr("Hidden by macOS").into()
                                } else {
                                    tr("Hidden network").into()
                                }
                            });
                            let color = if n.ssid.is_some() { p.text } else { p.weak };
                            ui.label(RichText::new(name).color(color));
                            if n.connected {
                                theme::pill(ui, p, tr("Connected"), p.success);
                            }
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let q = n.signal.unwrap_or(0);
                                theme::signal_bars(ui, p, q, 13.0);
                                ui.label(RichText::new(format!("{q} %")).color(p.weak).size(13.0));
                            });
                        });
                        row.col(|ui| {
                            ui.label(n.channel.map_or("—".into(), |c| c.to_string()));
                        });
                        row.col(|ui| {
                            // "5 GHz · 80 MHz · Wi-Fi 6", as far as the system tells.
                            let generation = n.standard.as_deref().map(|s| {
                                s.split_once("(Wi-Fi ")
                                    .map_or(s.to_string(), |(_, g)| format!("Wi-Fi {}", g.trim_end_matches(')')))
                            });
                            let parts: Vec<String> =
                                [n.band.clone(), n.width.clone(), generation].into_iter().flatten().collect();
                            let text = if parts.is_empty() { "—".to_string() } else { parts.join(" · ") };
                            let r = ui.label(RichText::new(text).color(p.weak).size(13.0));
                            if let Some(s) = &n.standard {
                                r.on_hover_text(s);
                            }
                        });
                        row.col(|ui| {
                            ui.label(
                                RichText::new(n.security.as_deref().unwrap_or(tr("Open"))).color(p.weak).size(13.0),
                            );
                        });
                    });
                });
        });
    }

    fn qr_dialog(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some((ssid, modules, password)) = &self.qr else { return };
        let waiting = self.password_job.is_some();
        let modal = egui::Modal::new(egui::Id::new("qr")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.vertical_centered(|ui| {
                ui.label(theme::semibold(ssid, 19.0).color(p.text));
                ui.label(RichText::new(tr("Point a phone's camera at the code to join.")).color(p.weak).size(13.0));
                ui.add_space(10.0);
                if waiting {
                    ui.add_space(100.0);
                    ui.spinner();
                    ui.label(RichText::new(tr("Reading the password…")).color(p.weak));
                    ui.add_space(100.0);
                } else {
                    theme::qr_code(ui, modules, 260.0);
                }
                ui.add_space(8.0);
                if let Some(pw) = password {
                    ui.label(RichText::new(trf("Password: {password}", &[("password", pw)])).monospace().color(p.text));
                }
                ui.add_space(10.0);
                theme::primary_button(ui, p, &format!("  {}  ", tr("Done")), true).clicked()
            })
            .inner
        });
        if modal.inner || modal.should_close() {
            self.qr = None;
        }
    }
}

/// How many networks use each channel, per band, and the least crowded of
/// the 2.4 GHz channels that do not overlap (1, 6, 11).
fn channel_chart(ui: &mut Ui, p: &Palette, list: &[Nearby]) {
    let mut bands: BTreeMap<&str, BTreeMap<u32, u32>> = BTreeMap::new();
    for n in list {
        if let (Some(ch), Some(b)) = (n.channel, n.band.as_deref()) {
            let b = if b.starts_with('2') {
                "2.4 GHz"
            } else if b.starts_with('5') {
                "5 GHz"
            } else {
                tr("6 GHz")
            };
            *bands.entry(b).or_default().entry(ch).or_default() += 1;
        }
    }
    ui.columns(bands.len().max(1), |cols| {
        for (i, (band, counts)) in bands.iter().enumerate() {
            let ui = &mut cols[i];
            ui.label(RichText::new(trf("Channels used · {band}", &[("band", band)])).color(p.weak).size(12.5));
            let channels: Vec<u32> =
                if *band == "2.4 GHz" { (1..=13).collect() } else { counts.keys().copied().collect() };
            let max = counts.values().copied().max().unwrap_or(1).max(1);
            let w = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 84.0), Sense::hover());
            let slot = rect.width() / channels.len().max(1) as f32;
            for (j, ch) in channels.iter().enumerate() {
                let n = counts.get(ch).copied().unwrap_or(0);
                let h = (rect.height() - 18.0) * n as f32 / max as f32;
                let x = rect.left() + j as f32 * slot;
                let bar = egui::Rect::from_min_max(
                    egui::pos2(x + 2.0, rect.bottom() - 16.0 - h.max(2.0)),
                    egui::pos2(x + slot - 2.0, rect.bottom() - 16.0),
                );
                let color = if n == 0 {
                    p.border
                } else if n as f32 >= max as f32 * 0.7 {
                    p.warning
                } else {
                    p.accent
                };
                ui.painter().rect_filled(bar, CornerRadius::same(3), color);
                ui.painter().text(
                    egui::pos2(x + slot / 2.0, rect.bottom() - 7.0),
                    egui::Align2::CENTER_CENTER,
                    ch.to_string(),
                    egui::FontId::proportional(10.5),
                    p.weak,
                );
            }
            if *band == "2.4 GHz" {
                let best = [1u32, 6, 11].into_iter().min_by_key(|c| {
                    // Neighbouring channels overlap: count them too.
                    (c.saturating_sub(2)..=c + 2).map(|x| counts.get(&x).copied().unwrap_or(0)).sum::<u32>()
                });
                if let Some(b) = best {
                    ui.label(
                        RichText::new(format!(
                            "{}  {}",
                            icon::LIGHTBULB,
                            trf("Least crowded: channel {n}", &[("n", &b)])
                        ))
                        .color(p.success)
                        .size(12.5),
                    );
                }
            }
        }
    });
}

/// Saves the networks and passwords to a CSV file the user picks.
fn export(list: &[SavedNetwork], sh: &mut Shared) {
    let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("wifi-passwords.csv").save_file()
    else {
        return;
    };
    let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::from("Network,Security,Password,Hidden\n");
    for n in list {
        out += &format!(
            "{},{},{},{}\n",
            esc(&n.ssid),
            esc(n.security.as_deref().unwrap_or("")),
            esc(n.password.as_deref().unwrap_or("")),
            if n.hidden { "yes" } else { "no" }
        );
    }
    match std::fs::write(&path, out) {
        Ok(()) => sh.toast(trf("Saved to {file}. Keep this file private.", &[("file", &path.display())])),
        Err(e) => sh.fail(trl("The file could not be saved."), &e.into()),
    }
}
