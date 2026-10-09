//! Tools → Network planner: one subnet per group of devices (rooms, IoT,
//! guests, …), sized with room to grow, with VLANs and switch configuration.

use std::net::Ipv4Addr;

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::plan::{self, Group, Options, Plan, PlanError};
use netmgr::subnet::Subnet;

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf};
use crate::theme::{self, Palette, icon_label};

pub struct Planner {
    /// Empty: the smallest 10.x.x.x block that fits.
    space: String,
    groups: Vec<Group>,
    opt: Options,
}

impl Default for Planner {
    fn default() -> Self {
        Self {
            space: String::new(),
            groups: vec![
                Group { name: tr("Room").into(), count: 10, devices: 25 },
                Group { name: tr("IoT").into(), count: 1, devices: 100 },
                Group { name: tr("Guest Wi-Fi").into(), count: 1, devices: 200 },
                Group { name: tr("Management").into(), count: 1, devices: 20 },
            ],
            opt: Options::default(),
        }
    }
}

/// Quick-add choices: name, how many networks, devices in each.
fn presets() -> [(String, u32, u32); 10] {
    [
        (tr("Room").into(), 10, 25),
        (tr("Office").into(), 1, 50),
        (tr("IoT").into(), 1, 100),
        (tr("Cameras").into(), 1, 30),
        (tr("Guest Wi-Fi").into(), 1, 200),
        (tr("IP phones").into(), 1, 50),
        (tr("Printers").into(), 1, 10),
        (tr("Servers").into(), 1, 20),
        (tr("Management").into(), 1, 20),
        (tr("Link").into(), 4, 1),
    ]
}

impl Planner {
    fn space(&self) -> Result<Subnet, String> {
        if self.space.trim().is_empty() {
            let prefix = plan::size(&self.groups, &self.opt).1.max(8);
            return Subnet::new(Ipv4Addr::new(10, 0, 0, 0), prefix).map_err(|e| format!("{e:#}"));
        }
        Subnet::parse(&self.space).map(|s| Subnet::new(s.network, s.prefix).unwrap_or(s)).map_err(|e| format!("{e:#}"))
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        egui::ScrollArea::vertical().id_salt("planner").show(ui, |ui| {
            self.options_card(ui, p);
            ui.add_space(10.0);
            self.groups_card(ui, p);
            ui.add_space(10.0);
            self.result_card(ui, p, sh);
        });
    }

    fn options_card(&mut self, ui: &mut Ui, p: &Palette) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Enter what you have — rooms, PCs, IoT devices, guests — and get a subnet for each, sized with room to grow, with gateways, VLANs and switch configuration.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(10.0);
            egui::Grid::new("plan-options").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                ui.label(tr("Address space"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.space)
                        .hint_text(tr("Automatic (the smallest block that fits)"))
                        .desired_width(300.0),
                );
                ui.end_row();
                ui.label(tr("Room to grow"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 240.0;
                    ui.add(egui::Slider::new(&mut self.opt.growth, 0..=100).suffix(" %"));
                });
                ui.end_row();
                ui.label(tr("VLANs"));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr("first")).color(p.weak));
                    ui.add(egui::DragValue::new(&mut self.opt.first_vlan).range(0..=4094));
                    ui.add_space(8.0);
                    ui.label(RichText::new(tr("step")).color(p.weak));
                    ui.add(egui::DragValue::new(&mut self.opt.vlan_step).range(1..=100));
                    ui.add_space(8.0);
                    ui.label(RichText::new(tr("0 = no VLANs")).color(p.weak).size(12.5));
                });
                ui.end_row();
            });
        });
    }

    fn groups_card(&mut self, ui: &mut Ui, p: &Palette) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::BUILDINGS, tr("Networks"));
            theme::paragraph(
                ui,
                trl("One subnet per line. For several networks of the same size, such as 12 classrooms, set how many."),
                13.5,
                p.weak,
            );
            ui.add_space(8.0);
            let mut remove = None;
            egui::Grid::new("plan-groups").num_columns(5).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new(tr("Name")).color(p.weak));
                ui.label(RichText::new(tr("How many")).color(p.weak));
                ui.label(RichText::new(tr("Devices in each")).color(p.weak));
                ui.label(RichText::new(tr("Each gets")).color(p.weak));
                ui.label("");
                ui.end_row();
                for (i, g) in self.groups.iter_mut().enumerate() {
                    ui.add_sized([200.0, 26.0], egui::TextEdit::singleline(&mut g.name));
                    ui.add(egui::DragValue::new(&mut g.count).range(1..=1000));
                    ui.add(egui::DragValue::new(&mut g.devices).range(1..=1_000_000));
                    let prefix = plan::prefix_for(plan::needed(g.devices, self.opt.growth));
                    let hosts = (1u64 << (32 - prefix)) - 2;
                    ui.label(
                        RichText::new(trf("/{prefix} · {hosts} addresses", &[("prefix", &prefix), ("hosts", &hosts)]))
                            .monospace()
                            .color(p.text),
                    );
                    if ui.button(icon::TRASH).on_hover_text(tr("Delete")).clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if let Some(i) = remove {
                self.groups.remove(i);
            }
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Add:")).color(p.weak));
                for (name, count, devices) in presets() {
                    if ui.button(format!("{}  {name}", icon::PLUS)).clicked() {
                        self.groups.push(Group { name, count, devices });
                    }
                }
            });
        });
    }

    fn result_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let space = self.space();
        let result = space.as_ref().map_err(Clone::clone).map(|s| plan::plan(s, &self.groups, &self.opt));
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::TREE_STRUCTURE, tr("Plan"));
            match result {
                Err(e) => {
                    ui.label(RichText::new(e).color(p.warning));
                }
                Ok(Err(PlanError::Empty)) => {
                    ui.label(RichText::new(tr("Add at least one network.")).color(p.weak));
                }
                Ok(Err(PlanError::TooSmall { needed, prefix })) => {
                    let s = space.expect("checked");
                    let have = 1u64 << (32 - s.prefix);
                    theme::paragraph(
                        ui,
                        &trlf(
                            "These networks need {needed} addresses (a /{prefix}), but {space} has only {have}.",
                            &[
                                ("needed", &needed),
                                ("prefix", &prefix),
                                ("space", &format!("{}/{}", s.network, s.prefix)),
                                ("have", &have),
                            ],
                        ),
                        14.0,
                        p.warning,
                    );
                    if needed <= 1 << 24 {
                        // The same start, widened; or a 10.x block when it does not line up.
                        let mask = u32::MAX.checked_shl(32 - prefix as u32).unwrap_or(0);
                        let base = u32::from(s.network) & mask;
                        let wider =
                            if base == u32::from(s.network) { base } else { u32::from(Ipv4Addr::new(10, 0, 0, 0)) };
                        let suggestion = format!("{}/{prefix}", Ipv4Addr::from(wider));
                        ui.add_space(6.0);
                        if ui.button(trf("Use {space}", &[("space", &suggestion)])).clicked() {
                            self.space = suggestion;
                        }
                    }
                }
                Ok(Ok(plan)) => self.show_plan(ui, p, sh, &plan),
            }
        });
    }

    fn show_plan(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared, plan: &Plan) {
        let room = 1u64 << (32 - plan.space.prefix);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(trf(
                    "{n} networks in {space}: {used} of {total} addresses used ({pct} %).",
                    &[
                        ("n", &plan.networks.len()),
                        ("space", &format!("{}/{}", plan.space.network, plan.space.prefix)),
                        ("used", &plan.used),
                        ("total", &room),
                        ("pct", &(plan.used * 100 / room)),
                    ],
                ))
                .color(p.text),
            );
        });
        ui.add(egui::ProgressBar::new(plan.used as f32 / room as f32).desired_height(6.0));
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button(icon_label(icon::COPY, "Copy table")).clicked() {
                ui.ctx().copy_text(plan.table());
                sh.toast(tr("Copied."));
            }
            if ui.button(icon_label(icon::FILE_CSV, "Export…")).clicked() {
                export(plan, sh);
            }
            if ui
                .button(icon_label(icon::TERMINAL_WINDOW, "Copy switch configuration"))
                .on_hover_text(tr("VLANs, gateway interfaces and DHCP pools for Cisco IOS and compatible switches"))
                .clicked()
            {
                ui.ctx().copy_text(plan.cisco());
                sh.toast(tr("Copied."));
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::horizontal().id_salt("plan-table").show(ui, |ui| {
            egui::Grid::new("plan").num_columns(9).spacing([18.0, 5.0]).striped(true).show(ui, |ui| {
                for h in [
                    tr("Name"),
                    "VLAN",
                    tr("Devices"),
                    tr("Network"),
                    tr("Subnet mask"),
                    tr("Gateway"),
                    tr("Usable addresses"),
                    tr("Broadcast"),
                    tr("Spare"),
                ] {
                    ui.label(RichText::new(h).color(p.weak).size(12.5));
                }
                ui.end_row();
                for n in &plan.networks {
                    let mono = |s: String| RichText::new(s).monospace().color(p.text);
                    ui.label(RichText::new(&n.name).color(p.text));
                    ui.label(mono(if n.vlan == 0 { String::new() } else { n.vlan.to_string() }));
                    ui.label(mono(n.devices.to_string()));
                    ui.label(mono(format!("{}/{}", n.subnet.network, n.subnet.prefix)));
                    ui.label(mono(n.subnet.mask.to_string()));
                    ui.label(mono(n.gateway.to_string()));
                    ui.label(mono(format!("{} – {}", n.subnet.first, n.subnet.last)));
                    ui.label(mono(n.subnet.broadcast.to_string()));
                    ui.label(RichText::new(n.spare.to_string()).monospace().color(p.weak));
                    ui.end_row();
                }
            });
            if !plan.free.is_empty() {
                ui.add_space(8.0);
                let free: Vec<String> = plan.free.iter().map(|s| format!("{}/{}", s.network, s.prefix)).collect();
                ui.label(
                    RichText::new(trf("Free for later: {blocks}", &[("blocks", &free.join(", "))]))
                        .color(p.weak)
                        .size(12.5),
                );
            }
        });
    }
}

fn export(plan: &Plan, sh: &mut Shared) {
    let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("network-plan.csv").save_file()
    else {
        return;
    };
    match std::fs::write(&path, plan.csv()) {
        Ok(()) => sh.toast(trf("Saved to {file}.", &[("file", &path.display())])),
        Err(e) => sh.fail(trl("The file could not be saved."), &e.into()),
    }
}
