//! Profiles: saved IP settings ("Office", "Home", "Lab switch") applied to
//! an adapter in one click. Shared with the command line (`netmgr profile`).

use eframe::egui::{self, Align, Layout, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::config::{IpSettings, Ipv4Mode};
use netmgr::profiles::{self, Profile};

use crate::adapters_page::IpForm;
use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

#[derive(Default)]
pub struct Profiles {
    list: Option<Vec<Profile>>,
    editor: Option<Editor>,
    job: Option<Job<String>>,
    /// The adapter chosen for each profile, by profile name.
    targets: std::collections::HashMap<String, String>,
    confirm_delete: Option<String>,
}

struct Editor {
    /// The name it had, when editing an existing profile.
    original: Option<String>,
    name: String,
    adapter: String,
    note: String,
    form: IpForm,
    error: Option<String>,
}

impl Profiles {
    fn reload(&mut self, sh: &mut Shared) {
        match profiles::load() {
            Ok(l) => self.list = Some(l),
            Err(e) => {
                self.list = Some(Vec::new());
                sh.fail(trl("The profiles could not be read."), &e);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if self.list.is_none() {
            self.reload(sh);
        }
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(m) => sh.toast(m),
                Err(e) => sh.fail(trl("The profile could not be applied."), &e),
            }
            sh.refresh = true;
            sh.refresh_wifi = true;
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                theme::page_title(
                    ui,
                    p,
                    tr("Profiles"),
                    tr("Saved settings for the networks you use, applied in one click."),
                )
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if theme::primary_button(ui, p, &icon_label(icon::PLUS, "New profile"), true).clicked() {
                    let a = sh.default_adapter().cloned();
                    self.editor = Some(Editor {
                        original: None,
                        name: String::new(),
                        adapter: a.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                        note: String::new(),
                        form: IpForm::new(&IpSettings { mode: Ipv4Mode::Dhcp, dns: Vec::new() }, a.as_ref()),
                        error: None,
                    });
                }
                if let Some(a) = sh.default_adapter().cloned()
                    && theme::secondary_button(ui, &icon_label(icon::FLOPPY_DISK, "Save current settings"))
                        .on_hover_text(trf("Save how {name} is set up now", &[("name", &a.name)]))
                        .clicked()
                {
                    self.editor = Some(Editor {
                        original: None,
                        name: String::new(),
                        adapter: a.name.clone(),
                        note: String::new(),
                        form: IpForm::new(&IpSettings::current(&a), Some(&a)),
                        error: None,
                    });
                }
            });
        });
        let list = self.list.clone().unwrap_or_default();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if list.is_empty() {
                theme::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical_centered(|ui| {
                        ui.add_space(16.0);
                        theme::icon_badge(ui, p, icon::STACK, p.accent, 72.0);
                        ui.add_space(8.0);
                        ui.label(theme::semibold(tr("No profiles yet"), 19.0).color(p.text));
                        theme::paragraph(
                            ui,
                            trl("A profile remembers IP settings — for example a fixed address for configuring a switch, the office network, or automatic settings for home — and applies them in one click."),
                            14.0,
                            p.weak,
                        );
                        ui.add_space(16.0);
                    });
                });
                return;
            }
            let adapters: Vec<String> = sh.visible_adapters().iter().map(|a| a.name.clone()).collect();
            for prof in &list {
                theme::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let glyph = if prof.settings.mode == Ipv4Mode::Dhcp { icon::HOUSE } else { icon::PUSH_PIN };
                        theme::icon_badge(ui, p, glyph, p.accent, 44.0);
                        ui.vertical(|ui| {
                            ui.label(theme::semibold(&prof.name, 17.0).color(p.text));
                            ui.label(RichText::new(summary(&prof.settings)).color(p.weak).size(13.5));
                            if !prof.note.is_empty() {
                                ui.label(RichText::new(&prof.note).color(p.weak).size(12.5).italics());
                            }
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(egui::Button::new(icon::TRASH).frame(false)).on_hover_text(tr("Delete")).clicked() {
                                self.confirm_delete = Some(prof.name.clone());
                            }
                            if ui.add(egui::Button::new(icon::PENCIL_SIMPLE).frame(false)).on_hover_text(tr("Edit")).clicked() {
                                let a = sh.adapters.iter().find(|a| a.name == prof.adapter);
                                self.editor = Some(Editor {
                                    original: Some(prof.name.clone()),
                                    name: prof.name.clone(),
                                    adapter: prof.adapter.clone(),
                                    note: prof.note.clone(),
                                    form: IpForm::new(&prof.settings, a),
                                    error: None,
                                });
                            }
                            let busy = self.job.is_some();
                            if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Apply"), !busy).clicked() {
                                let target = self.targets.get(&prof.name).cloned().unwrap_or_else(|| prof.adapter.clone());
                                match sh.adapters.iter().find(|a| a.name == target).cloned() {
                                    Some(a) => {
                                        let prof = prof.clone();
                                        self.job = Some(Job::spawn(ui.ctx(), move |_, _| {
                                            prof.apply(&a)?;
                                            Ok(trf("\"{profile}\" was applied to {adapter}.", &[("profile", &prof.name), ("adapter", &a.name)]))
                                        }));
                                    }
                                    None => sh.error = Some(trlf("The adapter \"{name}\" was not found. Choose another one.", &[("name", &target)])),
                                }
                            }
                            let target = self.targets.entry(prof.name.clone()).or_insert_with(|| {
                                if adapters.contains(&prof.adapter) { prof.adapter.clone() } else { adapters.first().cloned().unwrap_or_default() }
                            });
                            egui::ComboBox::from_id_salt(("target", &prof.name)).selected_text(target.as_str()).width(170.0).show_ui(ui, |ui| {
                                for a in &adapters {
                                    ui.selectable_value(target, a.clone(), a);
                                }
                            });
                            ui.label(RichText::new(tr("on")).color(p.weak));
                            if busy {
                                ui.spinner();
                            }
                        });
                    });
                });
                ui.add_space(10.0);
            }
            ui.add_space(4.0);
            theme::paragraph(
                ui,
                &trlf("Profiles are kept in {file} and can also be applied from the command line: netmgr profile apply <name>", &[("file", &profiles::config_dir().join("profiles.json").display())]),
                12.5,
                p.weak,
            );
        });
        self.editor_dialog(ui.ctx(), p, sh);
        self.delete_dialog(ui.ctx(), p, sh);
    }

    fn editor_dialog(&mut self, ctx: &egui::Context, p: &Palette, sh: &mut Shared) {
        let Some(ed) = self.editor.as_mut() else { return };
        let adapters: Vec<String> = sh.visible_adapters().iter().map(|a| a.name.clone()).collect();
        let mut close = false;
        let mut save = None;
        let modal = egui::Modal::new(egui::Id::new("profile-editor")).show(ctx, |ui| {
            ui.set_width(520.0);
            let title = if ed.original.is_some() { tr("Edit profile") } else { tr("New profile") };
            ui.label(theme::semibold(title, 18.0).color(p.text));
            ui.add_space(10.0);
            egui::Grid::new("profile-fields").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr("Name"));
                ui.add(egui::TextEdit::singleline(&mut ed.name).hint_text(tr("e.g. Office")).desired_width(260.0));
                ui.end_row();
                ui.label(tr("Adapter"));
                egui::ComboBox::from_id_salt("profile-adapter")
                    .selected_text(ed.adapter.as_str())
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for a in &adapters {
                            ui.selectable_value(&mut ed.adapter, a.clone(), a);
                        }
                    });
                ui.end_row();
                ui.label(tr("Note"));
                ui.add(egui::TextEdit::singleline(&mut ed.note).hint_text(tr("optional")).desired_width(260.0));
                ui.end_row();
            });
            ui.add_space(10.0);
            ed.form.show(ui, p);
            if let Some(e) = &ed.error {
                ui.add_space(6.0);
                theme::notice(ui, p, p.danger, icon::WARNING_CIRCLE, e);
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::primary_button(ui, p, &format!("  {}  ", tr("Save")), true).clicked() {
                    if ed.name.trim().is_empty() {
                        ed.error = Some(trl("Give the profile a name.").into());
                    } else {
                        match ed.form.settings() {
                            Ok(s) => {
                                save = Some(Profile {
                                    name: ed.name.trim().to_string(),
                                    adapter: ed.adapter.clone(),
                                    settings: s,
                                    note: ed.note.trim().to_string(),
                                })
                            }
                            Err(e) => ed.error = Some(e),
                        }
                    }
                }
                if theme::secondary_button(ui, tr("Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if let Some(prof) = save {
            let original = ed.original.clone();
            let result = (|| {
                if let Some(o) = original.filter(|o| !o.eq_ignore_ascii_case(&prof.name)) {
                    profiles::remove(&o)?;
                }
                profiles::upsert(prof.clone())
            })();
            match result {
                Ok(()) => {
                    sh.toast(trf("Profile \"{name}\" saved.", &[("name", &prof.name)]));
                    self.editor = None;
                    self.reload(sh);
                }
                Err(e) => {
                    if let Some(ed) = self.editor.as_mut() {
                        ed.error = Some(format!("{e:#}"));
                    }
                }
            }
        } else if close {
            self.editor = None;
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context, p: &Palette, sh: &mut Shared) {
        let Some(name) = self.confirm_delete.clone() else { return };
        let mut done = false;
        let modal = egui::Modal::new(egui::Id::new("profile-delete")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(theme::semibold(trf("Delete \"{name}\"?", &[("name", &name)]), 17.0).color(p.text));
            ui.add_space(6.0);
            theme::paragraph(ui, trl("The adapter's settings are not changed."), 14.0, p.weak);
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::danger_button(ui, p, tr("Delete")).clicked() {
                    if let Err(e) = profiles::remove(&name) {
                        sh.fail(trl("The profile could not be deleted."), &e);
                    }
                    done = true;
                }
                if theme::secondary_button(ui, tr("Cancel")).clicked() {
                    done = true;
                }
            });
        });
        if done || modal.should_close() {
            self.confirm_delete = None;
            self.reload(sh);
        }
    }
}

/// "Automatic (DHCP)" or "192.168.1.10/24 via 192.168.1.1", with the DNS servers.
fn summary(s: &IpSettings) -> String {
    let ip = match &s.mode {
        Ipv4Mode::Dhcp => tr("Automatic (DHCP)").to_string(),
        Ipv4Mode::Static { address, prefix, gateway: Some(g) } => {
            trf("{address} via {gateway}", &[("address", &format!("{address}/{prefix}")), ("gateway", g)])
        }
        Ipv4Mode::Static { address, prefix, gateway: None } => {
            trf("{address}, no gateway", &[("address", &format!("{address}/{prefix}"))])
        }
    };
    if s.dns.is_empty() {
        ip
    } else {
        let dns: Vec<String> = s.dns.iter().map(std::net::IpAddr::to_string).collect();
        format!("{ip} · {}", trf("DNS {servers}", &[("servers", &dns.join(", "))]))
    }
}
