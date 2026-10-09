//! Tools → Network planner: one subnet per group of devices (rooms, IoT,
//! guests, …), sized with room to grow, with VLANs, IPv6 and switch
//! configuration. The plan is kept between runs, and plans can be saved.

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::plan::{self, Group, Input, Plan, PlanError, Vendor};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf};
use crate::theme::{self, Palette, icon_label};

#[derive(Default)]
pub struct Planner {
    save_name: String,
}

/// What the planner starts with.
fn starting_plan() -> Input {
    Input {
        groups: vec![
            Group { name: tr("Room").into(), count: 10, devices: 25 },
            Group { name: tr("IoT").into(), count: 1, devices: 100 },
            Group { name: tr("Guest Wi-Fi").into(), count: 1, devices: 200 },
            Group { name: tr("Management").into(), count: 1, devices: 20 },
        ],
        ..Default::default()
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
    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut input = sh.settings.planner.clone().unwrap_or_else(starting_plan);
        egui::ScrollArea::vertical().id_salt("planner").show(ui, |ui| {
            self.options_card(ui, p, sh, &mut input);
            ui.add_space(10.0);
            groups_card(ui, p, &mut input);
            ui.add_space(10.0);
            result_card(ui, p, sh, &mut input);
        });
        if sh.settings.planner.as_ref() != Some(&input) {
            sh.settings.planner = Some(input);
        }
    }

    fn options_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared, input: &mut Input) {
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
                    egui::TextEdit::singleline(&mut input.space)
                        .hint_text(tr("Automatic (the smallest block that fits)"))
                        .desired_width(300.0),
                );
                ui.end_row();
                ui.label(tr("IPv6 prefix"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut input.ipv6)
                            .hint_text(tr("Optional, e.g. 2001:db8:abcd::/48"))
                            .desired_width(300.0),
                    );
                    if ui
                        .button(tr("Make a private one"))
                        .on_hover_text(tr(
                            "A random unique local prefix (fd…/48) for networks without IPv6 from the provider",
                        ))
                        .clicked()
                    {
                        input.ipv6 = plan::random_ula();
                    }
                });
                ui.end_row();
                ui.label(tr("Room to grow"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 240.0;
                    ui.add(egui::Slider::new(&mut input.options.growth, 0..=100).suffix(" %"));
                });
                ui.end_row();
                ui.label(tr("VLANs"));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr("first")).color(p.weak));
                    ui.add(egui::DragValue::new(&mut input.options.first_vlan).range(0..=4094));
                    ui.add_space(8.0);
                    ui.label(RichText::new(tr("step")).color(p.weak));
                    ui.add(egui::DragValue::new(&mut input.options.vlan_step).range(1..=100));
                    ui.add_space(8.0);
                    ui.label(RichText::new(tr("0 = no VLANs")).color(p.weak).size(12.5));
                });
                ui.end_row();
                ui.label(tr("Saved plans"));
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_name)
                            .hint_text(tr("Name, e.g. School building"))
                            .desired_width(200.0),
                    );
                    let name = self.save_name.trim().to_string();
                    if ui
                        .add_enabled(!name.is_empty(), egui::Button::new(icon_label(icon::FLOPPY_DISK, "Save")))
                        .clicked()
                    {
                        sh.settings.saved_plans.insert(name.clone(), input.clone());
                        sh.toast(trf("Plan “{name}” saved.", &[("name", &name)]));
                    }
                    let mut delete = None;
                    egui::ComboBox::from_id_salt("saved-plans").selected_text(tr("Open…")).width(160.0).show_ui(
                        ui,
                        |ui| {
                            if sh.settings.saved_plans.is_empty() {
                                ui.label(RichText::new(tr("No saved plans yet")).color(p.weak));
                            }
                            for (name, saved) in &sh.settings.saved_plans {
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(false, name).clicked() {
                                        *input = saved.clone();
                                        self.save_name = name.clone();
                                    }
                                    if ui.small_button(icon::TRASH).on_hover_text(tr("Delete")).clicked() {
                                        delete = Some(name.clone());
                                    }
                                });
                            }
                        },
                    );
                    if let Some(name) = delete {
                        sh.settings.saved_plans.remove(&name);
                    }
                    if ui.button(icon_label(icon::FILE_PLUS, "New plan")).clicked() {
                        *input = starting_plan();
                        self.save_name.clear();
                    }
                });
                ui.end_row();
            });
        });
    }
}

fn groups_card(ui: &mut Ui, p: &Palette, input: &mut Input) {
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
            for (i, g) in input.groups.iter_mut().enumerate() {
                ui.add_sized([200.0, 26.0], egui::TextEdit::singleline(&mut g.name));
                ui.add(egui::DragValue::new(&mut g.count).range(1..=1000));
                ui.add(egui::DragValue::new(&mut g.devices).range(1..=1_000_000));
                let prefix = plan::prefix_for(plan::needed(g.devices, input.options.growth));
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
            input.groups.remove(i);
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tr("Add:")).color(p.weak));
            for (name, count, devices) in presets() {
                if ui.button(format!("{}  {name}", icon::PLUS)).clicked() {
                    input.groups.push(Group { name, count, devices });
                }
            }
        });
    });
}

fn result_card(ui: &mut Ui, p: &Palette, sh: &mut Shared, input: &mut Input) {
    let space = input.space();
    let result = input.plan();
    theme::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        theme::section_title(ui, p, icon::TREE_STRUCTURE, tr("Plan"));
        match result {
            Err(e) => {
                ui.label(RichText::new(format!("{e:#}")).color(p.warning));
            }
            Ok(Err(PlanError::Empty)) => {
                ui.label(RichText::new(tr("Add at least one network.")).color(p.weak));
            }
            Ok(Err(PlanError::Ipv6TooSmall { room })) => {
                theme::paragraph(
                    ui,
                    &trlf(
                        "This IPv6 prefix has room for only {room} networks of /64: use a shorter one, such as a /48 or /56.",
                        &[("room", &room)],
                    ),
                    14.0,
                    p.warning,
                );
            }
            Ok(Err(PlanError::TooSmall { needed, prefix })) => {
                let Ok(s) = space else { return };
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
                    let wider = if base == u32::from(s.network) {
                        base
                    } else {
                        u32::from(std::net::Ipv4Addr::new(10, 0, 0, 0))
                    };
                    let suggestion = format!("{}/{prefix}", std::net::Ipv4Addr::from(wider));
                    ui.add_space(6.0);
                    if ui.button(trf("Use {space}", &[("space", &suggestion)])).clicked() {
                        input.space = suggestion;
                    }
                }
            }
            Ok(Ok(plan)) => show_plan(ui, p, sh, &plan),
        }
    });
}

fn show_plan(ui: &mut Ui, p: &Palette, sh: &mut Shared, plan: &Plan) {
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
        ui.add_space(12.0);
        egui::ComboBox::from_id_salt("plan-vendor")
            .selected_text(sh.settings.plan_vendor.label())
            .width(220.0)
            .show_ui(ui, |ui| {
                for v in Vendor::ALL {
                    ui.selectable_value(&mut sh.settings.plan_vendor, v, v.label());
                }
            });
        if ui
            .button(icon_label(icon::TERMINAL_WINDOW, "Copy switch configuration"))
            .on_hover_text(tr("VLANs, gateway interfaces and DHCP pools, ready to paste"))
            .clicked()
        {
            ui.ctx().copy_text(plan.config(sh.settings.plan_vendor));
            sh.toast(tr("Copied."));
        }
    });
    ui.add_space(8.0);
    let v6 = plan.networks.iter().any(|n| n.ipv6.is_some());
    egui::ScrollArea::horizontal().id_salt("plan-table").show(ui, |ui| {
        egui::Grid::new("plan").num_columns(if v6 { 10 } else { 9 }).spacing([18.0, 5.0]).striped(true).show(
            ui,
            |ui| {
                let mut heads = vec![
                    tr("Name"),
                    "VLAN",
                    tr("Devices"),
                    tr("Network"),
                    tr("Subnet mask"),
                    tr("Gateway"),
                    tr("Usable addresses"),
                    tr("Broadcast"),
                    tr("Spare"),
                ];
                if v6 {
                    heads.push("IPv6");
                }
                for h in heads {
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
                    if let Some(a) = n.ipv6 {
                        ui.label(mono(format!("{a}/64")));
                    }
                    ui.end_row();
                }
            },
        );
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
