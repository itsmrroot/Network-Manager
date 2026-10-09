//! Console → Backups: saves the configuration of every saved SSH session
//! with a backup command, keeps a copy only when something changed, and
//! shows what changed.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::backup::{self, Line, Outcome, Version};
use netmgr::remote::Saved;

use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// Each device's result as the run goes: name, outcome or error.
type Results = Arc<Mutex<Vec<(String, Result<Outcome, String>)>>>;

#[derive(Default)]
pub struct Backups {
    password: String,
    job: Option<Job<()>>,
    results: Results,
    selected: Option<String>,
    versions: Vec<Version>,
    /// The two copies compared: newer, older.
    pair: Option<(usize, usize)>,
    diff: Vec<Line>,
}

impl Backups {
    fn select(&mut self, name: &str) {
        self.selected = Some(name.to_string());
        self.versions = backup::versions(name);
        self.pair = (self.versions.len() >= 2).then_some((0, 1));
        self.compare();
    }

    fn compare(&mut self) {
        self.diff = match self.pair {
            Some((new, old)) => {
                let read = |i: usize| {
                    self.versions.get(i).and_then(|v| std::fs::read_to_string(&v.path).ok()).unwrap_or_default()
                };
                backup::diff(&read(old), &read(new), 3)
            }
            None => Vec::new(),
        };
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            if let Err(e) = r {
                sh.fail(trl("The backups could not be made."), &e);
            }
            if let Some(name) = self.selected.clone() {
                self.select(&name);
            }
        }
        let devices: Vec<Saved> =
            sh.settings.sessions.iter().filter(|s| !s.backup_command.trim().is_empty()).cloned().collect();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Saves the configuration of every saved SSH session that has a backup command (Edit session → Backup). A new copy is kept only when something changed, so the list shows when the configuration changed and what changed.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Password")).color(p.weak));
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text(tr("if the devices ask; not stored"))
                        .desired_width(200.0),
                );
                let busy = self.job.is_some();
                if theme::primary_button(
                    ui,
                    p,
                    &format!("{}  {}", icon::CLOUD_ARROW_DOWN, trf("Back up {n} devices", &[("n", &devices.len())])),
                    !busy && !devices.is_empty(),
                )
                .clicked()
                {
                    let list = devices.clone();
                    let password = Some(self.password.clone()).filter(|p| !p.is_empty());
                    let results = self.results.clone();
                    if let Ok(mut r) = results.lock() {
                        r.clear();
                    }
                    self.job = Some(Job::spawn(ui.ctx(), move |progress, cancel| {
                        for (i, d) in list.iter().enumerate() {
                            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                            progress.set(i as f32 / list.len() as f32, &d.name);
                            let r = backup::fetch(d, password.as_deref())
                                .and_then(|text| backup::store(&d.name, &text, SystemTime::now()))
                                .map_err(|e| format!("{e:#}"));
                            if let Ok(mut all) = results.lock() {
                                all.push((d.name.clone(), r));
                            }
                        }
                        Ok(())
                    }));
                }
                if let Some(j) = &self.job {
                    ui.spinner();
                    ui.label(RichText::new(j.progress.snapshot().message).color(p.weak));
                    if ui.button(icon_label(icon::STOP, "Stop")).clicked() {
                        j.stop();
                    }
                }
                if ui.button(icon_label(icon::FOLDER_OPEN, "Open folder")).clicked() {
                    let dir = backup::folder();
                    let _ = std::fs::create_dir_all(&dir);
                    crate::app::open_path(&dir.display().to_string());
                }
            });
            if devices.is_empty() {
                ui.add_space(6.0);
                theme::notice(
                    ui,
                    p,
                    p.accent,
                    icon::INFO,
                    trl(
                        "No session has a backup command yet: in SSH and Telnet, edit a saved session and choose its kind of device under Backup.",
                    ),
                );
            }
            let results = self.results.lock().map(|r| r.clone()).unwrap_or_default();
            if !results.is_empty() {
                ui.add_space(8.0);
                egui::Grid::new("backup-results").num_columns(2).spacing([20.0, 4.0]).show(ui, |ui| {
                    for (name, r) in &results {
                        ui.label(RichText::new(name).color(p.text));
                        let (text, color) = match r {
                            Ok(Outcome::First) => (tr("first copy saved").to_string(), p.success),
                            Ok(Outcome::Unchanged) => (tr("unchanged").to_string(), p.weak),
                            Ok(Outcome::Changed { added, removed }) => (
                                trf("changed: +{added} −{removed} lines", &[("added", added), ("removed", removed)]),
                                p.warning,
                            ),
                            Err(e) => (e.clone(), p.danger),
                        };
                        ui.label(RichText::new(text).color(color));
                        ui.end_row();
                    }
                });
            }
        });
        ui.add_space(10.0);
        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(230.0, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                theme::card(ui, p, |ui| {
                    ui.set_width(210.0);
                    ui.set_min_height(height - 30.0);
                    ui.label(theme::semibold(tr("Devices"), 15.0).color(p.text));
                    let mut pick = None;
                    for d in &devices {
                        let n = backup::versions(&d.name).len();
                        let selected = self.selected.as_deref() == Some(d.name.as_str());
                        if ui.selectable_label(selected, format!("{}  ·  {n}", d.name)).clicked() {
                            pick = Some(d.name.clone());
                        }
                    }
                    if let Some(n) = pick {
                        self.select(&n);
                    }
                });
            });
            ui.vertical(|ui| self.versions_ui(ui, p, sh));
        });
    }

    fn versions_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            let Some(name) = self.selected.clone() else {
                ui.label(RichText::new(tr("Choose a device to see its copies and what changed.")).color(p.weak));
                return;
            };
            ui.label(theme::semibold(&name, 15.0).color(p.text));
            if self.versions.is_empty() {
                ui.label(RichText::new(tr("No copies yet.")).color(p.weak));
                return;
            }
            let label = |v: &Version| v.stamp.clone();
            let mut changed = false;
            ui.horizontal_wrapped(|ui| {
                let (mut new, mut old) = self.pair.unwrap_or((0, 0));
                ui.label(RichText::new(tr("Compare")).color(p.weak));
                egui::ComboBox::from_id_salt("bk-new").selected_text(label(&self.versions[new])).show_ui(ui, |ui| {
                    for (i, v) in self.versions.iter().enumerate() {
                        changed |= ui.selectable_value(&mut new, i, label(v)).changed();
                    }
                });
                ui.label(RichText::new(tr("with")).color(p.weak));
                egui::ComboBox::from_id_salt("bk-old")
                    .selected_text(label(&self.versions[old.min(self.versions.len() - 1)]))
                    .show_ui(ui, |ui| {
                        for (i, v) in self.versions.iter().enumerate() {
                            changed |= ui.selectable_value(&mut old, i, label(v)).changed();
                        }
                    });
                if changed {
                    self.pair = Some((new, old));
                }
                if ui.button(icon_label(icon::FILE_TEXT, "Open")).on_hover_text(tr("The newer copy")).clicked() {
                    crate::app::open_path(&self.versions[new].path.display().to_string());
                }
                if ui.button(icon_label(icon::COPY, "Copy")).on_hover_text(tr("The newer copy")).clicked()
                    && let Ok(text) = std::fs::read_to_string(&self.versions[new].path)
                {
                    ui.ctx().copy_text(text);
                    sh.toast(tr("Copied."));
                }
            });
            if changed {
                self.compare();
            }
            ui.label(RichText::new(tr("Times are in UTC.")).color(p.weak).size(12.0));
            ui.add_space(6.0);
            if self.versions.len() < 2 {
                ui.label(
                    RichText::new(tr("Only one copy so far: changes appear after the next backup that finds any."))
                        .color(p.weak),
                );
                return;
            }
            if self.diff.iter().all(|l| matches!(l, Line::Same(_) | Line::Skipped(_))) {
                ui.label(RichText::new(tr("No differences.")).color(p.weak));
                return;
            }
            egui::ScrollArea::both().id_salt("diff").auto_shrink(false).show(ui, |ui| {
                for l in &self.diff {
                    let (prefix, text, color, bg) = match l {
                        Line::Same(t) => (" ", t.as_str(), p.weak, None),
                        Line::Added(t) => ("+", t.as_str(), p.success, Some(p.success.gamma_multiply(0.12))),
                        Line::Removed(t) => ("−", t.as_str(), p.danger, Some(p.danger.gamma_multiply(0.12))),
                        Line::Skipped(_) => {
                            ui.label(RichText::new("⋯").color(p.weak));
                            continue;
                        }
                    };
                    let line = RichText::new(format!("{prefix} {text}")).monospace().size(12.5).color(color);
                    match bg {
                        Some(bg) => {
                            egui::Frame::new().fill(bg).show(ui, |ui| ui.label(line));
                        }
                        None => {
                            ui.label(line);
                        }
                    }
                }
            });
        });
    }
}
