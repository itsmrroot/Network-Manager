//! Tools → SNMP: a switch's or router's name and uptime, every interface
//! with its state, speed, live traffic and errors, and walks of any OID.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::adapters::{format_bytes, format_speed};
use netmgr::snmp::{self, Client, Interface, Oid, System, Version};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

struct Reading {
    system: System,
    interfaces: Vec<Interface>,
    at: Instant,
}

type Walk = Vec<(String, &'static str, String)>;

pub struct SnmpTool {
    host: String,
    community: String,
    version: Version,
    read: Option<Job<Reading>>,
    last: Option<Reading>,
    /// Bits per second in and out, by interface index.
    rates: BTreeMap<u32, (u64, u64)>,
    live: bool,
    show_down: bool,
    walk_oid: String,
    walk: Option<Job<Walk>>,
    walk_rows: Walk,
    walk_filter: String,
}

impl Default for SnmpTool {
    fn default() -> Self {
        Self {
            host: String::new(),
            community: "public".into(),
            version: Version::V2c,
            read: None,
            last: None,
            rates: BTreeMap::new(),
            live: true,
            show_down: true,
            walk_oid: snmp::SYSTEM.into(),
            walk: None,
            walk_rows: Vec::new(),
            walk_filter: String::new(),
        }
    }
}

impl SnmpTool {
    #[cfg(debug_assertions)]
    pub fn demo(&mut self, ctx: &egui::Context, host: &str, community: &str) {
        self.host = host.into();
        self.community = community.into();
        self.start_read(ctx);
    }

    pub fn open(&mut self, host: &str) {
        self.host = host.to_string();
    }

    fn start_read(&mut self, ctx: &egui::Context) {
        let (host, community, version) = (self.host.trim().to_string(), self.community.clone(), self.version);
        self.read = Some(Job::spawn(ctx, move |_, _| {
            let c = Client::new(&host, &community, version)?;
            let system = snmp::system(&c)?;
            let interfaces = snmp::interfaces(&c)?;
            Ok(Reading { system, interfaces, at: Instant::now() })
        }));
    }

    pub fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.read) {
            match r {
                Ok(new) => {
                    if let Some(old) = &self.last {
                        let dt = new.at.duration_since(old.at).as_secs_f64();
                        for i in &new.interfaces {
                            if let Some(o) = old.interfaces.iter().find(|o| o.index == i.index)
                                && dt > 0.5
                            {
                                let rate =
                                    |now: u64, before: u64| (now.saturating_sub(before) as f64 * 8.0 / dt) as u64;
                                self.rates.insert(
                                    i.index,
                                    (rate(i.in_octets, o.in_octets), rate(i.out_octets, o.out_octets)),
                                );
                            }
                        }
                    }
                    self.last = Some(new);
                }
                Err(e) => {
                    self.live = false;
                    sh.fail(trl("The device did not answer over SNMP."), &e);
                }
            }
        }
        if let Some(r) = jobs::finished(&mut self.walk) {
            match r {
                Ok(rows) => self.walk_rows = rows,
                Err(e) => sh.fail(trl("The walk failed."), &e),
            }
        }
        // Live: read again every 5 seconds for traffic rates.
        if self.live
            && self.read.is_none()
            && let Some(l) = &self.last
        {
            let wait = Duration::from_secs(5).saturating_sub(l.at.elapsed());
            if wait.is_zero() {
                self.start_read(ctx);
            } else {
                ctx.request_repaint_after(wait);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        self.poll(ui.ctx(), sh);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Reads a switch, router, firewall or printer over SNMP (v1 or v2c): its name and uptime, and every port with its state, speed, live traffic and errors.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                crate::tools::host_picker(ui, sh, &mut self.host, "snmp-host");
                ui.label(RichText::new(tr("Community")).color(p.weak));
                ui.add(egui::TextEdit::singleline(&mut self.community).password(true).desired_width(120.0));
                ui.selectable_value(&mut self.version, Version::V2c, "v2c");
                ui.selectable_value(&mut self.version, Version::V1, "v1");
                let busy = self.read.is_some();
                if theme::primary_button(
                    ui,
                    p,
                    &icon_label(icon::PLUGS_CONNECTED, "Read"),
                    !busy && !self.host.trim().is_empty(),
                )
                .clicked()
                {
                    self.last = None;
                    self.rates.clear();
                    self.live = true;
                    sh.settings.remember_host(self.host.trim());
                    self.start_read(ui.ctx());
                }
                if busy {
                    ui.spinner();
                }
                if self.last.is_some() {
                    ui.checkbox(&mut self.live, tr("Live (every 5 s)"));
                }
            });
        });
        ui.add_space(10.0);
        egui::ScrollArea::vertical().id_salt("snmp").show(ui, |ui| {
            if let Some(r) = self.last.take() {
                system_card(ui, p, sh, &r.system);
                ui.add_space(10.0);
                self.interfaces_card(ui, p, &r.interfaces);
                ui.add_space(10.0);
                self.last = Some(r);
            }
            self.walk_card(ui, p, sh);
        });
    }

    fn interfaces_card(&mut self, ui: &mut Ui, p: &Palette, list: &[Interface]) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::PLUGS, tr("Interfaces"));
                ui.add_space(12.0);
                ui.checkbox(&mut self.show_down, tr("Show ports that are down"));
            });
            let up = list.iter().filter(|i| i.oper_up).count();
            ui.label(
                RichText::new(trf("{up} of {total} up", &[("up", &up), ("total", &list.len())]))
                    .color(p.weak)
                    .size(12.5),
            );
            ui.add_space(6.0);
            egui::ScrollArea::horizontal().id_salt("snmp-if").show(ui, |ui| {
                egui::Grid::new("snmp-if-grid").num_columns(8).spacing([18.0, 5.0]).striped(true).show(ui, |ui| {
                    for h in [
                        tr("Port"),
                        tr("State"),
                        tr("Speed"),
                        tr("In"),
                        tr("Out"),
                        tr("Errors"),
                        tr("Discards"),
                        tr("Description"),
                    ] {
                        ui.label(RichText::new(h).color(p.weak).size(12.5));
                    }
                    ui.end_row();
                    for i in list.iter().filter(|i| self.show_down || i.oper_up) {
                        let (state, color) = match (i.admin_up, i.oper_up) {
                            (false, _) => (tr("Off"), p.weak),
                            (true, true) => (tr("Up"), p.success),
                            (true, false) => (tr("Down"), p.danger),
                        };
                        ui.label(RichText::new(&i.name).monospace().color(p.text))
                            .on_hover_text(format!("{}\nifIndex {}\n{}", i.description, i.index, i.mac));
                        ui.label(RichText::new(format!("● {state}")).color(color));
                        ui.label(
                            RichText::new(if i.speed > 0 { format_speed(i.speed) } else { String::new() })
                                .color(p.text),
                        );
                        let (rin, rout) = self.rates.get(&i.index).copied().unwrap_or_default();
                        let traffic = |rate: u64, total: u64| {
                            if self.rates.contains_key(&i.index) { format_speed(rate) } else { format_bytes(total) }
                        };
                        ui.label(RichText::new(traffic(rin, i.in_octets)).monospace().color(p.text));
                        ui.label(RichText::new(traffic(rout, i.out_octets)).monospace().color(p.text));
                        let errors = i.in_errors + i.out_errors;
                        ui.label(RichText::new(errors.to_string()).monospace().color(if errors > 0 {
                            p.warning
                        } else {
                            p.weak
                        }));
                        let discards = i.in_discards + i.out_discards;
                        ui.label(RichText::new(discards.to_string()).monospace().color(if discards > 0 {
                            p.warning
                        } else {
                            p.weak
                        }));
                        ui.label(RichText::new(&i.alias).color(p.weak));
                        ui.end_row();
                    }
                });
            });
            if self.rates.is_empty() && self.live {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(tr("Traffic rates appear after the second reading (5 seconds)."))
                        .color(p.weak)
                        .size(12.5),
                );
            }
        });
    }

    fn walk_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::TREE_VIEW, tr("Walk"));
            theme::paragraph(ui, trl("Lists every value under an OID, as snmpwalk does."), 13.5, p.weak);
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.walk_oid).hint_text("1.3.6.1.2.1.1").desired_width(240.0));
                egui::ComboBox::from_id_salt("walk-presets").selected_text(tr("Presets")).width(120.0).show_ui(
                    ui,
                    |ui| {
                        for (label, oid) in snmp::WALK_PRESETS {
                            if ui.selectable_label(false, format!("{}  ({oid})", crate::i18n::tr_dyn(label))).clicked()
                            {
                                self.walk_oid = oid.into();
                            }
                        }
                    },
                );
                let ok = Oid::parse(&self.walk_oid).is_ok() && !self.host.trim().is_empty();
                if self.walk.is_some() {
                    ui.spinner();
                } else if ui.add_enabled(ok, egui::Button::new(icon_label(icon::PLAY, "Walk"))).clicked() {
                    let (host, community, version) =
                        (self.host.trim().to_string(), self.community.clone(), self.version);
                    let root = Oid::parse(&self.walk_oid).expect("checked");
                    self.walk = Some(Job::spawn(ui.ctx(), move |_, _| {
                        let c = Client::new(&host, &community, version)?;
                        Ok(c.walk(&root, 20_000)?
                            .into_iter()
                            .map(|(o, v)| (o.to_string(), v.kind(), v.to_string()))
                            .collect())
                    }));
                }
                if !self.walk_rows.is_empty() {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.walk_filter).hint_text(tr("Filter")).desired_width(160.0),
                    );
                    if ui.button(icon_label(icon::COPY, "Copy")).clicked() {
                        let text: String = self.walk_rows.iter().map(|(o, k, v)| format!("{o} = {k}: {v}\n")).collect();
                        ui.ctx().copy_text(text);
                        sh.toast(tr("Copied."));
                    }
                }
            });
            if !self.walk_rows.is_empty() {
                ui.add_space(6.0);
                let f = self.walk_filter.to_lowercase();
                let rows: Vec<&(String, &str, String)> = self
                    .walk_rows
                    .iter()
                    .filter(|(o, _, v)| f.is_empty() || o.contains(&f) || v.to_lowercase().contains(&f))
                    .collect();
                ui.label(RichText::new(trf("{n} values", &[("n", &rows.len())])).color(p.weak).size(12.5));
                egui::ScrollArea::both().id_salt("walk-rows").max_height(420.0).show(ui, |ui| {
                    egui::Grid::new("walk-grid").num_columns(3).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
                        for (o, k, v) in rows.iter().take(5000) {
                            ui.label(RichText::new(o.as_str()).monospace().size(12.5).color(p.weak));
                            ui.label(RichText::new(*k).size(12.0).color(p.weak));
                            ui.label(RichText::new(v.as_str()).monospace().size(12.5).color(p.text));
                            ui.end_row();
                        }
                    });
                });
            }
        });
    }
}

fn system_card(ui: &mut Ui, p: &Palette, sh: &mut Shared, s: &System) {
    theme::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        theme::section_title(ui, p, icon::HARD_DRIVES, if s.name.is_empty() { tr("Device") } else { &s.name });
        let mut copied = false;
        egui::Grid::new("snmp-sys").num_columns(2).spacing([28.0, 6.0]).show(ui, |ui| {
            copied |= theme::info_row(ui, p, tr("Description"), s.description.lines().next().unwrap_or(""), true);
            if let Some(t) = s.uptime {
                copied |= theme::info_row(ui, p, tr("Uptime"), &snmp::uptime(t), false);
            }
            if !s.location.is_empty() {
                copied |= theme::info_row(ui, p, tr("Location"), &s.location, true);
            }
            if !s.contact.is_empty() {
                copied |= theme::info_row(ui, p, tr("Contact"), &s.contact, true);
            }
            copied |= theme::info_row(ui, p, tr("Object ID"), &s.object_id, true);
        });
        if copied {
            sh.toast(tr("Copied."));
        }
    });
}
