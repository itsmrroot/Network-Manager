//! Devices: who is on the network — every device with its address, maker
//! and name, new devices marked, and quick actions for each.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use netmgr::scan::{self, Device, DeviceKind, Range};

use crate::app::{Nav, Shared};
use crate::i18n::{tr, tr_dyn, trf, trl, trn};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};
use crate::tools::Tab;

#[derive(Default)]
pub struct Devices {
    devices: Vec<Device>,
    live: Arc<Mutex<Vec<Device>>>,
    job: Option<Job<(Range, Vec<Device>)>>,
    range: Option<Range>,
    /// MAC addresses not seen before this scan.
    new: HashSet<String>,
    selected: Option<Ipv4Addr>,
    search: String,
    label: String,
    scanned: bool,
    view: View,
    map: crate::lan_extra::NetMap,
    free: crate::lan_extra::FreeAddresses,
    bonjour: crate::lan_extra::Bonjour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum View {
    #[default]
    List,
    Map,
    Free,
    Services,
}

pub fn kind_icon(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Router => icon::BROADCAST,
        DeviceKind::Computer => icon::DESKTOP,
        DeviceKind::Phone => icon::DEVICE_MOBILE,
        DeviceKind::Tv => icon::TELEVISION,
        DeviceKind::Printer => icon::PRINTER,
        DeviceKind::Speaker => icon::SPEAKER_HIGH,
        DeviceKind::Console => icon::GAME_CONTROLLER,
        DeviceKind::Camera => icon::CAMERA,
        DeviceKind::Storage => icon::HARD_DRIVES,
        DeviceKind::SmartHome => icon::LIGHTBULB,
        DeviceKind::Unknown => icon::CPU,
    }
}

impl Devices {
    /// Development aid: sample devices for README screenshots.
    #[cfg(debug_assertions)]
    /// Development aid: shows one of the tabs for screenshots.
    #[cfg(debug_assertions)]
    pub fn show(&mut self, tab: &str) {
        self.view = match tab {
            "map" => View::Map,
            "free" => View::Free,
            "bonjour" => View::Services,
            _ => View::List,
        };
    }

    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        let d = |ip: &str, mac: &str, name: Option<&str>, ports: &[u16], me: bool, gw: bool| {
            let mac: netmgr::mac::Mac = mac.parse().unwrap_or(netmgr::mac::Mac([2, 0, 0, 0, 0, 1]));
            Device {
                ip: ip.parse().unwrap_or(Ipv4Addr::LOCALHOST),
                vendor: mac.vendor().map(str::to_string),
                mac: Some(mac),
                name: name.map(str::to_string),
                open_ports: ports.to_vec(),
                is_self: me,
                is_gateway: gw,
            }
        };
        self.devices = vec![
            d("192.168.1.1", "B4:FB:E4:10:20:30", Some("router"), &[53, 80, 443], false, true),
            d("192.168.1.10", "F0:18:98:11:22:33", Some("MacBook-Pro"), &[], true, false),
            d("192.168.1.20", "B8:27:EB:44:55:66", Some("pi-hole"), &[22, 53, 80], false, false),
            d("192.168.1.31", "3C:2A:F4:12:34:56", Some("BRW-Office-Printer"), &[80, 631, 9100], false, false),
            d("192.168.1.42", "8E:1F:3A:77:88:99", Some("Annas-iPhone"), &[62078], false, false),
            d("192.168.1.50", "00:11:32:AB:CD:EF", Some("nas"), &[443, 445, 5000, 5001], false, false),
            d("192.168.1.66", "F0:EF:86:01:02:03", None, &[], false, false),
        ];
        self.range = Some(Range {
            adapter: "Wi-Fi".into(),
            local: [192, 168, 1, 10].into(),
            network: [192, 168, 1, 0].into(),
            prefix: 24,
            limited: false,
        });
        self.scanned = true;
        self.new.insert("F0:EF:86:01:02:03".into());
        self.map.demo();
        self.bonjour.demo();
        self.selected = Some([192, 168, 1, 50].into());
    }

    pub fn count(&self) -> Option<usize> {
        self.scanned.then_some(self.devices.len())
    }

    pub fn scanning(&self) -> bool {
        self.job.is_some()
    }

    pub fn poll(&mut self, _ctx: &egui::Context, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok((range, devices)) => {
                    self.range = Some(range);
                    self.devices = devices;
                    self.scanned = true;
                    // Devices seen for the first time; the first scan ever
                    // marks nothing (everything would be new).
                    let first = sh.settings.known_devices.is_empty();
                    self.new.clear();
                    for d in &self.devices {
                        if let Some(m) = d.mac {
                            let key = m.to_string();
                            if !first && !d.is_self && !sh.settings.known_devices.contains(&key) {
                                self.new.insert(key.clone());
                            }
                            sh.settings.known_devices.insert(key);
                        }
                    }
                    if !self.new.is_empty() {
                        let text = trn(
                            self.new.len() as u64,
                            "1 new device on your network.",
                            "{n} new devices on your network.",
                        );
                        if sh.settings.notifications {
                            crate::notify::send(tr("Network Manager"), &text);
                        }
                        sh.toast(text);
                    }
                }
                Err(e) => sh.fail(trl("The network could not be scanned."), &e),
            }
        } else if self.job.is_some()
            && let Ok(l) = self.live.lock()
            && !l.is_empty()
        {
            self.devices = l.clone();
        }
    }

    /// Scans again (from the menu bar), unless a scan is running.
    pub fn scan(&mut self, ctx: &egui::Context, sh: &Shared) {
        if self.job.is_none() && sh.default_adapter().is_some() {
            self.view = View::List;
            self.start(ctx, sh);
        }
    }

    fn start(&mut self, ctx: &egui::Context, sh: &Shared) {
        let opts = scan::Options { adapter: None, ports: sh.settings.scan_ports, names: sh.settings.scan_names };
        let live = Arc::new(Mutex::new(Vec::new()));
        self.live = live.clone();
        self.job = Some(Job::spawn(ctx, move |progress, cancel| {
            scan::scan(&opts, cancel, &|f, m| progress.set(f, m), &|list| {
                if let Ok(mut l) = live.lock() {
                    *l = list.to_vec();
                }
            })
        }));
    }

    fn display_name(&self, sh: &Shared, d: &Device) -> String {
        d.mac
            .and_then(|m| sh.settings.device_labels.get(&m.to_string()).cloned())
            .or_else(|| d.name.clone())
            .or_else(|| d.is_self.then(|| tr("This computer").to_string()))
            .or_else(|| d.vendor.clone().map(|v| trf("{maker} device", &[("maker", &v)])))
            .unwrap_or_else(|| tr_dyn(d.kind().label()))
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if !self.scanned && self.job.is_none() && sh.adapters_loaded && sh.default_adapter().is_some() {
            self.start(ui.ctx(), sh);
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let sub = match &self.range {
                    Some(r) => trf(
                        "{n} devices on {network} ({adapter})",
                        &[
                            ("n", &self.devices.len()),
                            ("network", &format!("{}/{}", r.network, r.prefix)),
                            ("adapter", &r.adapter),
                        ],
                    ),
                    None => tr("Every device connected to your network.").to_string(),
                };
                theme::page_title(ui, p, tr("Devices"), &sub);
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if let Some(job) = &self.job {
                    if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked() {
                        job.stop();
                    }
                } else if theme::primary_button(
                    ui,
                    p,
                    &icon_label(icon::MAGNIFYING_GLASS, if self.scanned { tr("Scan again") } else { tr("Scan") }),
                    sh.default_adapter().is_some(),
                )
                .clicked()
                {
                    self.start(ui.ctx(), sh);
                }
                if self.scanned && theme::secondary_button(ui, &icon_label(icon::EXPORT, "Export…")).clicked() {
                    self.export(sh);
                }
            });
        });
        if sh.lan_denied {
            crate::overview::local_network_notice(ui, p);
            ui.add_space(10.0);
        }
        if let Some(job) = &self.job {
            let st = job.progress.snapshot();
            ui.add(
                egui::ProgressBar::new(st.fraction.unwrap_or(0.0)).desired_height(8.0).corner_radius(4).fill(p.accent),
            );
            ui.label(
                RichText::new(if st.message.is_empty() { tr("Starting…").to_string() } else { st.message })
                    .color(p.weak)
                    .size(12.5),
            );
            ui.add_space(6.0);
        }
        if let Some(r) = &self.range
            && r.limited
        {
            theme::notice(
                ui,
                p,
                p.accent,
                icon::INFO,
                trl("This network is large: only the 1,022 addresses around this computer were scanned."),
            );
            ui.add_space(8.0);
        }
        if sh.default_adapter().is_none() && sh.adapters_loaded {
            theme::notice(
                ui,
                p,
                p.warning,
                icon::WARNING,
                trl("Not connected to a network: there is nothing to scan."),
            );
            return;
        }
        theme::tabs(
            ui,
            p,
            &mut self.view,
            &[
                (View::List, icon::LIST, tr("Devices")),
                (View::Map, icon::TREE_STRUCTURE, tr("Map")),
                (View::Free, icon::CHECK_SQUARE, tr("Free addresses")),
                (View::Services, icon::BROADCAST, tr("Services (Bonjour)")),
            ],
        );
        ui.add_space(8.0);
        match self.view {
            View::List => {}
            View::Map => return self.map.ui(ui, p, sh, &self.devices),
            View::Free => return self.free.ui(ui, p, sh, &self.devices, self.range.as_ref()),
            View::Services => return self.bonjour.ui(ui, p, sh),
        }

        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Search by name, address or maker")))
                .desired_width(320.0),
        );
        ui.add_space(8.0);

        let q = self.search.to_lowercase();
        let rows: Vec<(Device, String)> = self
            .devices
            .iter()
            .map(|d| (d.clone(), self.display_name(sh, d)))
            .filter(|(d, name)| {
                q.is_empty()
                    || name.to_lowercase().contains(&q)
                    || d.ip.to_string().contains(&q)
                    || d.mac.is_some_and(|m| m.to_string().to_lowercase().contains(&q))
                    || d.vendor.as_deref().unwrap_or("").to_lowercase().contains(&q)
            })
            .collect();
        let selected = self.selected.and_then(|ip| self.devices.iter().find(|d| d.ip == ip).cloned());
        let height = ui.available_height();
        let detail_w = if selected.is_some() { 300.0 } else { 0.0 };
        ui.horizontal_top(|ui| {
            // The detail card adds its margins (2 × 18) and border to `detail_w`.
            let table_w = ui.available_width() - if selected.is_some() { detail_w + 12.0 + 40.0 } else { 0.0 };
            ui.allocate_ui_with_layout(egui::vec2(table_w, height), Layout::top_down(Align::Min), |ui| {
                theme::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(height - 40.0);
                    if rows.is_empty() {
                        let msg = if self.job.is_some() {
                            tr("Looking for devices…")
                        } else if self.scanned {
                            tr("No devices match.")
                        } else {
                            tr("Click Scan to find the devices on your network.")
                        };
                        ui.label(RichText::new(msg).color(p.weak));
                        return;
                    }
                    let mut clicked = None;
                    TableBuilder::new(ui)
                        .cell_layout(Layout::left_to_right(Align::Center))
                        .striped(true)
                        .sense(egui::Sense::click())
                        .max_scroll_height(height - 80.0)
                        .column(Column::exact(34.0))
                        .column(Column::remainder().at_least(160.0).clip(true))
                        .column(Column::exact(120.0))
                        .column(Column::exact(150.0))
                        .column(Column::remainder().at_least(120.0).clip(true))
                        .header(26.0, |mut h| {
                            for t in ["", tr("Name"), tr("IP address"), tr("MAC address"), tr("Maker")] {
                                h.col(|ui| {
                                    ui.label(RichText::new(t).color(p.weak).size(13.0));
                                });
                            }
                        })
                        .body(|body| {
                            body.rows(36.0, rows.len(), |mut row| {
                                let (d, name) = &rows[row.index()];
                                row.set_selected(self.selected == Some(d.ip));
                                let is_new = d.mac.is_some_and(|m| self.new.contains(&m.to_string()));
                                row.col(|ui| {
                                    ui.label(RichText::new(kind_icon(d.kind())).size(19.0).color(if is_new {
                                        p.warning
                                    } else {
                                        p.accent
                                    }));
                                });
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(name).color(p.text));
                                        if d.is_self {
                                            theme::pill(ui, p, tr("You"), p.accent);
                                        }
                                        if d.is_gateway {
                                            theme::pill(ui, p, tr("Router"), p.deep);
                                        }
                                        if is_new {
                                            theme::pill(ui, p, tr("New"), p.warning);
                                        }
                                    });
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(d.ip.to_string()).monospace().color(p.text));
                                });
                                row.col(|ui| {
                                    let m = d.mac.map_or("—".into(), |m| m.to_string());
                                    ui.label(RichText::new(m).monospace().size(12.5).color(p.weak));
                                });
                                row.col(|ui| {
                                    let v = if d.private_address() {
                                        tr("Private address").to_string()
                                    } else {
                                        d.vendor.clone().unwrap_or_else(|| "—".into())
                                    };
                                    ui.label(RichText::new(v).color(p.weak).size(13.0));
                                });
                                if row.response().clicked() {
                                    clicked = Some(d.ip);
                                }
                            });
                        });
                    if let Some(ip) = clicked {
                        self.selected = if self.selected == Some(ip) { None } else { Some(ip) };
                        self.label = self
                            .devices
                            .iter()
                            .find(|d| d.ip == ip)
                            .and_then(|d| d.mac)
                            .and_then(|m| sh.settings.device_labels.get(&m.to_string()).cloned())
                            .unwrap_or_default();
                    }
                });
            });
            if let Some(d) = selected {
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.set_width(detail_w);
                    self.detail(ui, p, sh, &d);
                });
            }
        });
    }

    fn detail(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared, d: &Device) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                theme::icon_badge(ui, p, kind_icon(d.kind()), p.accent, 64.0);
                ui.add_space(6.0);
                ui.label(theme::semibold(self.display_name(sh, d), 18.0).color(p.text));
                ui.label(RichText::new(tr_dyn(d.kind().label())).color(p.weak).size(13.0));
            });
            ui.add_space(10.0);
            let mut copied = false;
            egui::Grid::new("device-grid").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                copied |= theme::info_row(ui, p, "IP", &d.ip.to_string(), true);
                if let Some(m) = d.mac {
                    copied |= theme::info_row(ui, p, "MAC", &m.to_string(), true);
                }
                let maker = if d.private_address() {
                    tr("Private address: the device hides its real one").to_string()
                } else {
                    d.vendor.clone().unwrap_or_else(|| tr("Unknown").into())
                };
                copied |= theme::info_row(ui, p, tr("Maker"), &maker, false);
                if let Some(n) = &d.name {
                    copied |= theme::info_row(ui, p, tr("Host name"), n, true);
                }
                if !d.open_ports.is_empty() {
                    let ports: Vec<String> = d
                        .open_ports
                        .iter()
                        .map(|p| match scan::service_name(*p) {
                            "" => p.to_string(),
                            s => format!("{p} {}", tr_dyn(s)),
                        })
                        .collect();
                    copied |= theme::info_row(ui, p, tr("Services"), &ports.join("\n"), false);
                }
            });
            if copied {
                sh.toast(tr("Copied."));
            }
            if let Some(m) = d.mac {
                ui.add_space(8.0);
                ui.label(RichText::new(tr("Your name for it")).color(p.weak).size(13.0));
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.label)
                            .hint_text(tr("e.g. Anna's laptop"))
                            .desired_width(180.0),
                    );
                    let save = ui.button(tr("Save")).clicked()
                        || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if save {
                        let key = m.to_string();
                        if self.label.trim().is_empty() {
                            sh.settings.device_labels.remove(&key);
                        } else {
                            sh.settings.device_labels.insert(key, self.label.trim().to_string());
                        }
                        sh.toast(tr("Name saved."));
                    }
                });
            }
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                let ip = d.ip.to_string();
                if ui.button(icon_label(icon::PULSE, "Ping")).clicked() {
                    sh.nav = Some(Nav::Tool(Tab::Ping, ip.clone()));
                }
                if ui.button(icon_label(icon::DOOR_OPEN, "Ports")).clicked() {
                    sh.nav = Some(Nav::Tool(Tab::Ports, ip.clone()));
                }
                if ui
                    .button(icon_label(icon::HEARTBEAT, "Watch"))
                    .on_hover_text(tr("Add to the ping monitor"))
                    .clicked()
                {
                    sh.nav = Some(Nav::Monitor(ip.clone()));
                }
                let web = [443u16, 80, 8080, 8443, 5000, 5001].into_iter().find(|p| d.open_ports.contains(p));
                if let Some(port) = web
                    && ui.button(icon_label(icon::ARROW_SQUARE_OUT, "Web page")).clicked()
                {
                    let scheme = if port == 443 || port == 8443 || port == 5001 { "https" } else { "http" };
                    crate::app::open_path(&format!("{scheme}://{ip}:{port}"));
                }
                if let Some(m) = d.mac
                    && !d.is_self
                    && ui
                        .button(icon_label(icon::POWER, "Wake"))
                        .on_hover_text(tr("Send a Wake-on-LAN packet"))
                        .clicked()
                {
                    match netmgr::tools::wake(m, None) {
                        Ok(()) => sh.toast(trf("Wake-on-LAN sent to {mac}.", &[("mac", &m)])),
                        Err(e) => sh.fail(trl("The packet could not be sent."), &e),
                    }
                }
            });
        });
    }

    fn export(&self, sh: &mut Shared) {
        let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("devices.csv").save_file()
        else {
            return;
        };
        let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let mut out = String::from("Name,IP address,MAC address,Maker,Type,Open ports\n");
        for d in &self.devices {
            let ports: Vec<String> = d.open_ports.iter().map(u16::to_string).collect();
            out += &format!(
                "{},{},{},{},{},{}\n",
                esc(&self.display_name(sh, d)),
                d.ip,
                d.mac.map_or(String::new(), |m| m.to_string()),
                esc(d.vendor.as_deref().unwrap_or("")),
                d.kind().label(),
                esc(&ports.join(" "))
            );
        }
        match std::fs::write(&path, out) {
            Ok(()) => sh.toast(trf("Saved to {file}.", &[("file", &path.display())])),
            Err(e) => sh.fail(trl("The file could not be saved."), &e.into()),
        }
    }
}
