//! Servers → HTTP, SNMP traps, DHCP and Time (NTP).

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::dhcpd::{self, Lease, Reservation};
use netmgr::snmp::{self, Trap};

use crate::app::{Nav, Page, Shared};
use crate::i18n::{tr, trf, trl};
use crate::servers::{Running, listen_address, listen_picker, my_addresses};
use crate::theme::{self, Palette, icon_label};

fn clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// The first address devices can reach this computer at.
fn my_ip(sh: &Shared) -> String {
    my_addresses(sh).first().and_then(|a| a.split(' ').next()).unwrap_or("this-computer").to_string()
}

/// Commands to copy, with what they do.
fn examples(ui: &mut Ui, p: &Palette, sh: &mut Shared, list: &[(&str, String)]) {
    ui.label(RichText::new(tr("Example commands")).color(p.weak).size(12.5));
    for (what, command) in list {
        ui.horizontal(|ui| {
            ui.label(RichText::new(*what).color(p.weak).size(12.5));
            let r =
                ui.add(egui::Label::new(RichText::new(command).monospace().color(p.text)).sense(egui::Sense::click()));
            if r.on_hover_text(tr("Click to copy")).clicked() {
                ui.ctx().copy_text(command.clone());
                sh.toast(tr("Copied."));
            }
        });
    }
}

fn start_stop(ui: &mut Ui, p: &Palette, running: &mut Option<Running>, start: impl FnOnce() -> Option<Running>) {
    if running.is_some() {
        if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop server")).clicked()
            && let Some(r) = running.take()
        {
            r.stop.store(true, Ordering::Relaxed);
        }
        ui.spinner();
        ui.label(RichText::new(tr("Running")).color(p.success));
    } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start server"), true).clicked() {
        *running = start();
    }
}

/// Takes a failed server down and shows why.
fn check_failed(sh: &mut Shared, running: &mut Option<Running>, what: &str) {
    if let Some(e) = running.as_ref().and_then(Running::failed) {
        *running = None;
        sh.error = Some(format!("{what}\n\n{e}"));
    }
}

// ---------------------------------------------------------------- HTTP

pub struct HttpTab {
    port: u16,
    upload: bool,
    overwrite: bool,
    server: Option<Running>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Default for HttpTab {
    fn default() -> Self {
        Self { port: 8080, upload: false, overwrite: false, server: None, log: Default::default() }
    }
}

impl HttpTab {
    pub fn stop(&mut self) {
        if let Some(r) = self.server.take() {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        check_failed(sh, &mut self.server, trl("The HTTP server could not start."));
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Serves the TFTP folder over HTTP: switches and routers download firmware many times faster than over TFTP, and can resume. With uploads on, devices can also save their configuration here (HTTP PUT).",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            let running = self.server.is_some();
            listen_picker(ui, p, sh, !running);
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("http-settings").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
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
                    ui.add(egui::DragValue::new(&mut self.port).range(1..=65535));
                    ui.end_row();
                    ui.label(tr("Uploads"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.upload, tr("Let devices upload files (configuration backups)"));
                        if self.upload {
                            ui.checkbox(&mut self.overwrite, tr("Replace existing files"));
                        }
                    });
                    ui.end_row();
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let (port, upload, overwrite, log) = (self.port, self.upload, self.overwrite, self.log.clone());
                let root = PathBuf::from(sh.settings.tftp_folder.trim());
                let listen = listen_address(sh);
                start_stop(ui, p, &mut self.server, || {
                    Some(Running::spawn(move |stop| {
                        netmgr::httpd::serve(
                            netmgr::httpd::Options { root, port, listen, allow_upload: upload, overwrite },
                            stop,
                            Arc::new(move |e| {
                                if let Ok(mut v) = log.lock() {
                                    v.push(format!(
                                        "{}  {}  {} {}  {}  {}",
                                        clock(),
                                        e.from,
                                        e.method,
                                        e.path,
                                        e.status,
                                        netmgr::adapters::format_bytes(e.bytes)
                                    ));
                                }
                            }),
                        )
                    }))
                });
                if ui.button(icon_label(icon::FOLDER_OPEN, "Open folder")).clicked() {
                    let _ = std::fs::create_dir_all(sh.settings.tftp_folder.trim());
                    crate::app::open_path(sh.settings.tftp_folder.trim());
                }
            });
            if self.server.is_some() {
                let ip = my_ip(sh);
                let base = if self.port == 80 { format!("http://{ip}") } else { format!("http://{ip}:{}", self.port) };
                ui.add_space(8.0);
                let r = ui.add(
                    egui::Label::new(RichText::new(&base).monospace().color(p.accent)).sense(egui::Sense::click()),
                );
                if r.on_hover_text(tr("Open in the browser")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&base));
                }
                ui.add_space(4.0);
                examples(
                    ui,
                    p,
                    sh,
                    &[
                        (tr("Cisco IOS — firmware to the switch"), format!("copy {base}/c9300-universalk9.bin flash:")),
                        (tr("Cisco IOS — back up the configuration"), format!("copy running-config {base}/sw1-confg")),
                        (tr("Aruba AOS-CX"), format!("copy {base}/ArubaOS-CX.swi primary")),
                        (tr("Juniper"), format!("request system software add {base}/junos-install.tgz")),
                    ],
                );
            }
        });
        log_card(ui, p, &self.log, tr("Transfers"));
    }
}

fn log_card(ui: &mut Ui, p: &Palette, log: &Arc<Mutex<Vec<String>>>, title: &str) {
    let lines = log.lock().map(|l| l.clone()).unwrap_or_default();
    if lines.is_empty() {
        return;
    }
    ui.add_space(10.0);
    theme::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        ui.label(theme::semibold(title, 15.0).color(p.text));
        egui::ScrollArea::vertical().id_salt(title).max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
            for l in lines.iter().rev().take(500).rev() {
                ui.label(RichText::new(l).monospace().size(12.5).color(p.weak));
            }
        });
    });
}

// ---------------------------------------------------------------- traps

pub struct TrapTab {
    port: u16,
    server: Option<Running>,
    traps: Arc<Mutex<VecDeque<Trap>>>,
    filter: String,
}

impl Default for TrapTab {
    fn default() -> Self {
        Self { port: 162, server: None, traps: Default::default(), filter: String::new() }
    }
}

impl TrapTab {
    pub fn stop(&mut self) {
        if let Some(r) = self.server.take() {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Development aid: sample traps for screenshots.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        use netmgr::snmp::{Oid, Value};
        let t = |from: &str, name: &'static str, oid: &str, vb: Vec<(&str, Value)>| Trap {
            received: SystemTime::now(),
            from: from.parse().expect("valid"),
            kind: "v2c",
            community: "public".into(),
            oid: Oid::parse(oid).expect("valid"),
            name,
            uptime: Some(9_000_000),
            varbinds: vb.into_iter().map(|(o, v)| (Oid::parse(o).expect("valid"), v)).collect(),
        };
        if let Ok(mut v) = self.traps.lock() {
            v.extend([
                t("10.10.0.2", "coldStart", "1.3.6.1.6.3.1.1.5.1", vec![]),
                t(
                    "10.10.0.12",
                    "linkDown",
                    "1.3.6.1.6.3.1.1.5.3",
                    vec![
                        ("1.3.6.1.2.1.2.2.1.1.7", Value::Integer(7)),
                        ("1.3.6.1.2.1.2.2.1.2.7", Value::Text(b"GigabitEthernet1/0/7".to_vec())),
                    ],
                ),
                t(
                    "10.10.0.12",
                    "linkUp",
                    "1.3.6.1.6.3.1.1.5.4",
                    vec![
                        ("1.3.6.1.2.1.2.2.1.1.7", Value::Integer(7)),
                        ("1.3.6.1.2.1.2.2.1.2.7", Value::Text(b"GigabitEthernet1/0/7".to_vec())),
                    ],
                ),
                t("10.10.0.2", "authenticationFailure", "1.3.6.1.6.3.1.1.5.5", vec![]),
            ]);
        }
        self.server = Some(Running::spawn(|stop| {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(())
        }));
    }

    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        check_failed(sh, &mut self.server, trl("The trap receiver could not start."));
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Receives the notifications devices send over SNMP (traps and informs, v1 and v2c): ports going down and up, restarts, failed logins, spanning-tree changes.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            listen_picker(ui, p, sh, self.server.is_none());
            let listen = listen_address(sh);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(self.server.is_none(), |ui| {
                    ui.label(tr("UDP port"));
                    ui.add(egui::DragValue::new(&mut self.port).range(1..=65535));
                });
                let (port, traps) = (self.port, self.traps.clone());
                start_stop(ui, p, &mut self.server, || {
                    Some(Running::spawn(move |stop| {
                        snmp::trap_serve(
                            listen,
                            port,
                            stop,
                            Arc::new(move |t| {
                                if let Ok(mut v) = traps.lock() {
                                    v.push_back(t);
                                    while v.len() > 10_000 {
                                        v.pop_front();
                                    }
                                }
                            }),
                        )
                    }))
                });
                ui.add_space(12.0);
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(tr("Filter")).desired_width(180.0));
                if ui.button(icon_label(icon::TRASH, "Clear")).clicked()
                    && let Ok(mut t) = self.traps.lock()
                {
                    t.clear();
                }
                if ui.button(icon_label(icon::EXPORT, "Export…")).clicked() {
                    self.export(sh);
                }
            });
            if self.server.is_some() {
                let ip = my_ip(sh);
                ui.add_space(6.0);
                examples(
                    ui,
                    p,
                    sh,
                    &[
                        (tr("Cisco IOS"), format!("snmp-server host {ip} version 2c public")),
                        (tr("Aruba / HP"), format!("snmp-server host {ip} community public")),
                        (tr("Juniper"), format!("set snmp trap-group lab targets {ip}")),
                    ],
                );
            }
        });
        ui.add_space(10.0);
        let f = self.filter.to_lowercase();
        let list: Vec<Trap> = self
            .traps
            .lock()
            .map(|t| {
                t.iter()
                    .rev()
                    .filter(|t| {
                        f.is_empty()
                            || t.name.to_lowercase().contains(&f)
                            || t.from.to_string().contains(&f)
                            || t.oid.to_string().contains(&f)
                            || t.varbinds.iter().any(|(_, v)| v.to_string().to_lowercase().contains(&f))
                    })
                    .take(1000)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            if list.is_empty() {
                ui.label(
                    RichText::new(if self.server.is_some() {
                        tr("Waiting for traps…")
                    } else {
                        tr("Start the receiver, then point devices to this computer.")
                    })
                    .color(p.weak),
                );
                return;
            }
            egui::ScrollArea::vertical().id_salt("traps").auto_shrink(false).show(ui, |ui| {
                for t in &list {
                    let when: chrono::DateTime<chrono::Local> = t.received.into();
                    let color = match t.name {
                        "linkDown" | "authenticationFailure" | "bgpBackwardTransition" => p.danger,
                        "linkUp" | "bgpEstablished" => p.success,
                        "coldStart" | "warmStart" | "topologyChange" | "newRoot" => p.warning,
                        _ => p.accent,
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(when.format("%H:%M:%S").to_string()).monospace().color(p.weak));
                        ui.label(RichText::new(t.from.to_string()).monospace().color(p.text));
                        ui.label(
                            theme::semibold(
                                if t.name.is_empty() { t.oid.to_string() } else { t.name.to_string() },
                                14.0,
                            )
                            .color(color),
                        );
                        ui.label(RichText::new(t.kind).size(12.0).color(p.weak));
                    });
                    for (o, v) in t.varbinds.iter().take(8) {
                        ui.label(RichText::new(format!("      {o} = {v}")).monospace().size(12.0).color(p.weak));
                    }
                    ui.add_space(4.0);
                }
            });
        });
    }

    fn export(&self, sh: &mut Shared) {
        let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("snmp-traps.csv").save_file()
        else {
            return;
        };
        let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let mut out = String::from("Time,From,Type,Trap,OID,Values\n");
        for t in self.traps.lock().map(|t| t.clone()).unwrap_or_default() {
            let when: chrono::DateTime<chrono::Local> = t.received.into();
            let values: Vec<String> = t.varbinds.iter().map(|(o, v)| format!("{o}={v}")).collect();
            out += &format!(
                "{},{},{},{},{},{}\n",
                when.format("%Y-%m-%d %H:%M:%S"),
                t.from,
                t.kind,
                t.name,
                t.oid,
                esc(&values.join("; "))
            );
        }
        match std::fs::write(&path, out) {
            Ok(()) => sh.toast(trf("Saved to {file}.", &[("file", &path.display())])),
            Err(e) => sh.fail(trl("The file could not be saved."), &e.into()),
        }
    }
}

// ---------------------------------------------------------------- time

pub struct NtpTab {
    port: u16,
    stratum: u8,
    server: Option<Running>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Default for NtpTab {
    fn default() -> Self {
        Self { port: 123, stratum: 3, server: None, log: Default::default() }
    }
}

impl NtpTab {
    pub fn stop(&mut self) {
        if let Some(r) = self.server.take() {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        check_failed(sh, &mut self.server, trl("The time server could not start."));
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Gives devices the time from this computer's clock (NTP), for labs and staging networks without internet: logs and certificates need the right time.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            listen_picker(ui, p, sh, self.server.is_none());
            let listen = listen_address(sh);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(self.server.is_none(), |ui| {
                    ui.label(tr("UDP port"));
                    ui.add(egui::DragValue::new(&mut self.port).range(1..=65535));
                    ui.label(tr("Stratum"));
                    ui.add(egui::DragValue::new(&mut self.stratum).range(1..=15))
                        .on_hover_text(tr("1: the server has its own reference clock (GPS, atomic); 2: it follows a stratum 1 server; and so on."));
                });
                let (port, stratum, log) = (self.port, self.stratum, self.log.clone());
                start_stop(ui, p, &mut self.server, || {
                    Some(Running::spawn(move |stop| {
                        netmgr::ntp::serve(
                            listen,
                            port,
                            stratum,
                            stop,
                            Arc::new(move |ip| {
                                if let Ok(mut v) = log.lock() {
                                    v.push(format!("{}  {ip}", clock()));
                                }
                            }),
                        )
                    }))
                });
            });
            if self.server.is_some() {
                let ip = my_ip(sh);
                ui.add_space(6.0);
                examples(
                    ui,
                    p,
                    sh,
                    &[
                        (tr("Cisco IOS"), format!("ntp server {ip}")),
                        (tr("Aruba / HP"), format!("ntp server {ip} iburst")),
                        (tr("Juniper"), format!("set system ntp server {ip}")),
                        (tr("MikroTik"), format!("/system ntp client set enabled=yes servers={ip}")),
                    ],
                );
            }
        });
        log_card(ui, p, &self.log, tr("Requests"));
    }
}

// ---------------------------------------------------------------- DHCP

pub struct DhcpTab {
    adapter: String,
    first: String,
    last: String,
    router: String,
    dns: String,
    domain: String,
    lease_hours: u32,
    tftp: String,
    bootfile: String,
    opt150: String,
    opt43: String,
    reservations: Vec<Reservation>,
    confirmed: bool,
    server: Option<Running>,
    leases: dhcpd::Leases,
    log: Arc<Mutex<Vec<String>>>,
    loaded: bool,
}

impl Default for DhcpTab {
    fn default() -> Self {
        Self {
            adapter: String::new(),
            first: String::new(),
            last: String::new(),
            router: String::new(),
            dns: String::new(),
            domain: String::new(),
            lease_hours: 1,
            tftp: String::new(),
            bootfile: String::new(),
            opt150: String::new(),
            opt43: String::new(),
            reservations: Vec::new(),
            confirmed: false,
            server: None,
            leases: Default::default(),
            log: Default::default(),
            loaded: false,
        }
    }
}

fn ip_list(s: &str) -> anyhow::Result<Vec<Ipv4Addr>> {
    s.split([',', ' ', ';'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| x.parse().map_err(|_| anyhow::anyhow!("\"{x}\" is not an IPv4 address")))
        .collect()
}

impl DhcpTab {
    pub fn stop(&mut self) {
        if let Some(r) = self.server.take() {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Development aid: a running server with sample leases.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        self.loaded = true;
        self.first = "192.168.1.100".into();
        self.last = "192.168.1.199".into();
        self.tftp = "192.168.1.10".into();
        self.bootfile = "network-confg".into();
        self.confirmed = true;
        let now = SystemTime::now();
        let l = |ip: &str, mac: &str, name: &str, vendor: &str| Lease {
            mac: mac.parse().expect("valid"),
            ip: ip.parse().expect("valid"),
            hostname: name.into(),
            state: "Leased",
            expires: now + Duration::from_secs(3300),
            vendor: vendor.into(),
        };
        if let Ok(mut v) = self.leases.lock() {
            *v = vec![
                l("192.168.1.100", "00:1E:BD:12:34:56", "Switch", "ciscopnp"),
                l("192.168.1.101", "84:D4:7E:AB:CD:01", "AP-2F-East", "ArubaAP"),
                l("192.168.1.102", "00:1B:54:77:88:99", "SEP001B54778899", "Cisco Systems, Inc. IP Phone"),
            ];
        }
        if let Ok(mut v) = self.log.lock() {
            v.extend([
                "10:41:02  DISCOVER from 00:1e:bd:12:34:56 (Switch) → OFFER 192.168.1.100".to_string(),
                "10:41:02  REQUEST from 00:1e:bd:12:34:56 (Switch) → ACK 192.168.1.100".to_string(),
                "10:41:09  DISCOVER from 84:d4:7e:ab:cd:01 (AP-2F-East) → OFFER 192.168.1.101".to_string(),
                "10:41:09  REQUEST from 84:d4:7e:ab:cd:01 (AP-2F-East) → ACK 192.168.1.101".to_string(),
            ]);
        }
        self.server = Some(Running::spawn(|stop| {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(())
        }));
    }

    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    /// Fills the pool from the adapter's network: .100 to .199 of a /24,
    /// or the upper half of other networks.
    fn suggest(&mut self, sh: &Shared) {
        let Some((ip, prefix)) = sh.adapters.iter().find(|a| a.id == self.adapter).and_then(|a| a.main_ipv4()) else {
            return;
        };
        let Ok(s) = netmgr::subnet::Subnet::new(ip, prefix) else { return };
        let (f, l) = (u32::from(s.first), u32::from(s.last));
        let (a, b) = if prefix == 24 { (f + 99, f + 198) } else { (f + (l - f) / 2, l) };
        self.first = Ipv4Addr::from(a).to_string();
        self.last = Ipv4Addr::from(b).to_string();
        self.tftp = ip.to_string();
    }

    fn config(&self, sh: &Shared) -> anyhow::Result<(dhcpd::Config, String)> {
        let a = sh
            .adapters
            .iter()
            .find(|a| a.id == self.adapter)
            .ok_or_else(|| anyhow::anyhow!("choose the adapter that faces the devices"))?;
        let (server, prefix) = a.main_ipv4().ok_or_else(|| anyhow::anyhow!("{} has no IPv4 address", a.name))?;
        let parse = |s: &str, what: &str| -> anyhow::Result<Ipv4Addr> {
            s.trim().parse().map_err(|_| anyhow::anyhow!("{what}: \"{s}\" is not an IPv4 address"))
        };
        let cfg = dhcpd::Config {
            server,
            first: parse(&self.first, "first address")?,
            last: parse(&self.last, "last address")?,
            mask: netmgr::adapters::prefix_to_mask(prefix),
            router: if self.router.trim().is_empty() { None } else { Some(parse(&self.router, "router")?) },
            dns: ip_list(&self.dns)?,
            domain: self.domain.trim().to_string(),
            lease_secs: self.lease_hours.max(1) * 3600,
            tftp_server: self.tftp.trim().to_string(),
            bootfile: self.bootfile.trim().to_string(),
            tftp_150: ip_list(&self.opt150)?,
            vendor_hex: self.opt43.trim().to_string(),
            reservations: self.reservations.iter().filter(|r| !r.mac.trim().is_empty()).cloned().collect(),
        };
        cfg.check()?;
        let iface = if cfg!(windows) { a.name.clone() } else { a.device.clone() };
        Ok((cfg, iface))
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        check_failed(sh, &mut self.server, trl("The DHCP server could not start."));
        if !self.loaded {
            self.loaded = true;
            if let Some(c) = &sh.settings.dhcp_server {
                let list = |v: &[Ipv4Addr]| v.iter().map(Ipv4Addr::to_string).collect::<Vec<_>>().join(", ");
                self.first = c.first.to_string();
                self.last = c.last.to_string();
                self.router = c.router.map(|r| r.to_string()).unwrap_or_default();
                self.dns = list(&c.dns);
                self.domain = c.domain.clone();
                self.lease_hours = (c.lease_secs / 3600).max(1);
                self.tftp = c.tftp_server.clone();
                self.bootfile = c.bootfile.clone();
                self.opt150 = list(&c.tftp_150);
                self.opt43 = c.vendor_hex.clone();
                self.reservations = c.reservations.clone();
            }
            self.adapter = sh.settings.dhcp_adapter.clone();
        }
        if self.adapter.is_empty()
            && let Some(a) = sh.default_adapter()
        {
            self.adapter = a.id.clone();
            self.suggest(sh);
        }
        theme::notice(
            ui,
            p,
            p.warning,
            icon::WARNING,
            trl(
                "Only for a lab or staging network, such as a switch cabled straight to this computer. On a network that already has a DHCP server (every office and home network), two servers hand out conflicting addresses and devices lose their connection.",
            ),
        );
        ui.add_space(8.0);
        let running = self.server.is_some();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("dhcp-settings").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    ui.label(tr("Adapter"));
                    let shown = sh.adapters.iter().find(|a| a.id == self.adapter).map_or(tr("Choose an adapter").to_string(), |a| {
                        a.main_ipv4().map_or(a.name.clone(), |(ip, pfx)| format!("{} — {ip}/{pfx}", a.name))
                    });
                    let before = self.adapter.clone();
                    egui::ComboBox::from_id_salt("dhcp-iface").selected_text(shown).width(300.0).show_ui(ui, |ui| {
                        for a in sh.visible_adapters() {
                            if let Some((ip, pfx)) = a.main_ipv4() {
                                ui.selectable_value(&mut self.adapter, a.id.clone(), format!("{} — {ip}/{pfx}", a.name));
                            }
                        }
                    });
                    if self.adapter != before {
                        self.suggest(sh);
                    }
                    ui.end_row();
                    ui.label(tr("Addresses"));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.first).desired_width(130.0));
                        ui.label("–");
                        ui.add(egui::TextEdit::singleline(&mut self.last).desired_width(130.0));
                    });
                    ui.end_row();
                    ui.label(tr("Router (gateway)"));
                    ui.add(egui::TextEdit::singleline(&mut self.router).hint_text(tr("optional")).desired_width(180.0));
                    ui.end_row();
                    ui.label(tr("DNS servers"));
                    ui.add(egui::TextEdit::singleline(&mut self.dns).hint_text(tr("optional, e.g. 1.1.1.1, 9.9.9.9")).desired_width(280.0));
                    ui.end_row();
                    ui.label(tr("Domain"));
                    ui.add(egui::TextEdit::singleline(&mut self.domain).hint_text(tr("optional, e.g. lab.local")).desired_width(180.0));
                    ui.end_row();
                    ui.label(tr("Lease"));
                    ui.add(egui::DragValue::new(&mut self.lease_hours).range(1..=168).suffix(" h"));
                    ui.end_row();
                });
                ui.add_space(8.0);
                egui::CollapsingHeader::new(RichText::new(tr("Zero-touch provisioning (options 66, 67, 150, 43)")).color(p.text))
                    .default_open(!self.bootfile.is_empty() || !self.opt150.is_empty() || !self.opt43.is_empty())
                    .show(ui, |ui| {
                        theme::paragraph(
                            ui,
                            trl("Tell devices where to get their software or configuration when they start: Cisco switches load the file named in option 67 from the TFTP server in option 66 (or 150); Juniper, Aruba and access points use 43, 66 and 67 the same way. Put the files in the TFTP folder and start the TFTP server."),
                            13.0,
                            p.weak,
                        );
                        egui::Grid::new("dhcp-ztp").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                            ui.label(tr("TFTP server (66)"));
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut self.tftp).hint_text(tr("optional")).desired_width(180.0));
                                if ui.small_button(tr("This computer")).clicked()
                                    && let Some((ip, _)) = sh.adapters.iter().find(|a| a.id == self.adapter).and_then(|a| a.main_ipv4())
                                {
                                    self.tftp = ip.to_string();
                                }
                            });
                            ui.end_row();
                            ui.label(tr("Boot file (67)"));
                            ui.add(egui::TextEdit::singleline(&mut self.bootfile).hint_text("network-confg").desired_width(220.0));
                            ui.end_row();
                            ui.label(tr("TFTP servers (150)"));
                            ui.add(egui::TextEdit::singleline(&mut self.opt150).hint_text(tr("optional, Cisco")).desired_width(220.0));
                            ui.end_row();
                            ui.label(tr("Vendor options (43)"));
                            ui.add(egui::TextEdit::singleline(&mut self.opt43).hint_text(tr("hex, e.g. f1040a090001")).desired_width(220.0));
                            ui.end_row();
                        });
                    });
                egui::CollapsingHeader::new(RichText::new(trf("Fixed addresses ({n})", &[("n", &self.reservations.len())])).color(p.text))
                    .show(ui, |ui| {
                        let mut remove = None;
                        egui::Grid::new("dhcp-res").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
                            for (i, r) in self.reservations.iter_mut().enumerate() {
                                ui.add(egui::TextEdit::singleline(&mut r.mac).hint_text(tr("MAC address")).desired_width(150.0));
                                ui.add(egui::TextEdit::singleline(&mut r.ip).hint_text(tr("IP address")).desired_width(130.0));
                                ui.add(egui::TextEdit::singleline(&mut r.name).hint_text(tr("Name")).desired_width(130.0));
                                if ui.small_button(icon::TRASH).on_hover_text(tr("Delete")).clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                        if let Some(i) = remove {
                            self.reservations.remove(i);
                        }
                        if ui.button(icon_label(icon::PLUS, "Add")).clicked() {
                            self.reservations.push(Reservation::default());
                        }
                    });
            });
            ui.add_space(10.0);
            if !running {
                ui.checkbox(&mut self.confirmed, tr("No other DHCP server is on this network"));
                ui.horizontal(|ui| {
                    if ui.link(tr("Check with Switch port → Find DHCP servers")).clicked() {
                        sh.nav = Some(Nav::Page(Page::SwitchPort));
                    }
                });
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                if running {
                    if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop server")).clicked()
                        && let Some(r) = self.server.take()
                    {
                        r.stop.store(true, Ordering::Relaxed);
                    }
                    ui.spinner();
                    ui.label(RichText::new(tr("Running")).color(p.success));
                } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start server"), self.confirmed)
                    .clicked()
                {
                    match self.config(sh) {
                        Ok((cfg, iface)) => {
                            sh.settings.dhcp_server = Some(cfg.clone());
                            sh.settings.dhcp_adapter = self.adapter.clone();
                            let (leases, log) = (self.leases.clone(), self.log.clone());
                            self.server = Some(Running::spawn(move |stop| {
                                dhcpd::serve(
                                    cfg,
                                    &iface,
                                    leases,
                                    stop,
                                    Arc::new(move |l| {
                                        if let Ok(mut v) = log.lock() {
                                            v.push(format!("{}  {l}", clock()));
                                        }
                                    }),
                                )
                            }));
                        }
                        Err(e) => sh.fail(trl("The DHCP settings are not valid."), &e),
                    }
                }
            });
        });
        self.leases_card(ui, p);
        log_card(ui, p, &self.log, tr("Log"));
    }

    fn leases_card(&self, ui: &mut Ui, p: &Palette) {
        let leases: Vec<Lease> = self.leases.lock().map(|l| l.clone()).unwrap_or_default();
        if leases.is_empty() {
            return;
        }
        ui.add_space(10.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("Devices that got an address"), 15.0).color(p.text));
            egui::Grid::new("leases").num_columns(5).spacing([20.0, 5.0]).striped(true).show(ui, |ui| {
                for h in [tr("IP address"), tr("MAC address"), tr("Name"), tr("State"), tr("Expires")] {
                    ui.label(RichText::new(h).color(p.weak).size(12.5));
                }
                ui.end_row();
                let now = SystemTime::now();
                for l in &leases {
                    ui.label(RichText::new(l.ip.to_string()).monospace().color(p.text));
                    let maker = l.mac.vendor().unwrap_or("");
                    ui.label(RichText::new(l.mac.to_string()).monospace().color(p.text))
                        .on_hover_text(format!("{maker}\n{}", l.vendor));
                    ui.label(RichText::new(&l.hostname).color(p.text));
                    let state = match l.state {
                        "Leased" => tr("Leased"),
                        "Offered" => tr("Offered"),
                        "Released" => tr("Released"),
                        _ => tr("Declined"),
                    };
                    ui.label(RichText::new(state).color(if l.state == "Leased" { p.success } else { p.weak }));
                    let left = l.expires.duration_since(now).unwrap_or(Duration::ZERO).as_secs();
                    ui.label(
                        RichText::new(if left == 0 {
                            "—".into()
                        } else {
                            trf("{n} min", &[("n", &left.div_ceil(60))])
                        })
                        .color(p.weak),
                    );
                    ui.end_row();
                }
            });
        });
    }
}
