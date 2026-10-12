//! Console → Backups: saves the configuration of every saved SSH session
//! with a backup command, keeps a copy only when something changed, and
//! shows what changed; also on a schedule while the app runs.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local, TimeZone};
use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::backup::{self, Line, Outcome, Version};
use netmgr::remote::Saved;
use serde::{Deserialize, Serialize};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, with_icon};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// Each device's result as the run goes: name, outcome or error.
type Results = Arc<Mutex<Vec<(String, Result<Outcome, String>)>>>;

/// How often the backups run by themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Every {
    #[default]
    Off,
    Hour,
    SixHours,
    Day,
    Week,
}

/// When the backups run by themselves (while the app is open).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Schedule {
    pub every: Every,
    /// Day and Week: the hour (local time).
    pub hour: u8,
    /// Week: 0 is Monday.
    pub weekday: u8,
}

impl Schedule {
    /// The last scheduled moment at or before `now` (Day and Week).
    fn last_moment(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        let at = |date: chrono::NaiveDate| {
            Local.from_local_datetime(&date.and_hms_opt(u32::from(self.hour.min(23)), 0, 0)?).earliest()
        };
        let today = now.date_naive();
        match self.every {
            Every::Day => {
                let t = at(today)?;
                if t > now { at(today.pred_opt()?) } else { Some(t) }
            }
            Every::Week => {
                let back = (today.weekday().num_days_from_monday() + 7 - u32::from(self.weekday.min(6))) % 7;
                let day = today - chrono::Days::new(u64::from(back));
                let t = at(day)?;
                if t > now { at(day - chrono::Days::new(7)) } else { Some(t) }
            }
            _ => None,
        }
    }

    /// Whether the backups are due, after they last ran at `last` (Unix seconds).
    pub fn due(&self, last: i64, now: DateTime<Local>) -> bool {
        match self.every {
            Every::Off => false,
            Every::Hour => now.timestamp() - last >= 3600,
            Every::SixHours => now.timestamp() - last >= 6 * 3600,
            Every::Day | Every::Week => self.last_moment(now).is_some_and(|t| t.timestamp() > last),
        }
    }

    /// When they run next.
    pub fn next(&self, last: i64, now: DateTime<Local>) -> Option<DateTime<Local>> {
        let after = |secs: i64| Local.timestamp_opt(last + secs, 0).single().map(|t| t.max(now));
        match self.every {
            Every::Off => None,
            Every::Hour => after(3600),
            Every::SixHours => after(6 * 3600),
            Every::Day | Every::Week => {
                let t = self.last_moment(now)?;
                if t.timestamp() > last {
                    return Some(now);
                }
                let days = if self.every == Every::Day { 1 } else { 7 };
                Local.from_local_datetime(&(t.naive_local() + chrono::Duration::days(days))).earliest()
            }
        }
    }
}

#[derive(Default)]
pub struct Backups {
    password: String,
    /// The run going on was started by the schedule.
    scheduled: bool,
    job: Option<Job<()>>,
    results: Results,
    selected: Option<String>,
    versions: Vec<Version>,
    /// The two copies compared: newer, older.
    pair: Option<(usize, usize)>,
    diff: Vec<Line>,
}

impl Backups {
    /// Development aid: a schedule and the results of a run.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self, sh: &mut Shared) {
        sh.settings.backup_schedule = Schedule { every: Every::Day, hour: 2, weekday: 0 };
        sh.settings.backup_last = Local::now().timestamp();
        if let Ok(mut r) = self.results.lock() {
            *r = vec![
                ("core-sw1".into(), Ok(Outcome::Changed { added: 3, removed: 1 })),
                ("access-sw-2F".into(), Ok(Outcome::Unchanged)),
                ("edge-router".into(), Ok(Outcome::First)),
            ];
        }
    }

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

    fn start(&mut self, ctx: &egui::Context, list: Vec<Saved>, scheduled: bool) {
        let password = Some(self.password.clone()).filter(|p| !p.is_empty());
        let results = self.results.clone();
        if let Ok(mut r) = results.lock() {
            r.clear();
        }
        self.scheduled = scheduled;
        self.job = Some(Job::spawn(ctx, move |progress, cancel| {
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

    /// Runs the scheduled backups when they are due, and takes in finished
    /// runs (every frame, whichever page is shown).
    pub fn tick(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            if let Err(e) = &r
                && !self.scheduled
            {
                sh.fail(trl("The backups could not be made."), e);
            }
            if self.scheduled {
                self.report(sh);
            }
            if let Some(name) = self.selected.clone() {
                self.select(&name);
            }
        }
        let schedule = sh.settings.backup_schedule;
        if schedule.every == Every::Off || self.job.is_some() {
            return;
        }
        let now = Local::now();
        if schedule.due(sh.settings.backup_last, now) {
            sh.settings.backup_last = now.timestamp();
            let devices = devices(sh);
            if !devices.is_empty() {
                self.start(ctx, devices, true);
            }
        } else if let Some(next) = schedule.next(sh.settings.backup_last, now) {
            // Wake up for it even when nothing else happens.
            let wait = (next - now).num_seconds().clamp(1, 60) as u64;
            ctx.request_repaint_after(std::time::Duration::from_secs(wait));
        }
    }

    /// After a scheduled run: a notification when something changed or failed.
    fn report(&self, sh: &Shared) {
        let results = self.results.lock().map(|r| r.clone()).unwrap_or_default();
        let changed: Vec<&str> = results
            .iter()
            .filter(|(_, r)| matches!(r, Ok(Outcome::Changed { .. }) | Ok(Outcome::First)))
            .map(|(n, _)| n.as_str())
            .collect();
        let failed: Vec<&str> = results.iter().filter(|(_, r)| r.is_err()).map(|(n, _)| n.as_str()).collect();
        let mut lines = Vec::new();
        if !changed.is_empty() {
            lines.push(trf("Changed: {names}", &[("names", &changed.join(", "))]));
        }
        if !failed.is_empty() {
            lines.push(trf("Failed: {names}", &[("names", &failed.join(", "))]));
        }
        if !lines.is_empty() && sh.settings.notifications {
            crate::notify::send(tr("Scheduled backups"), &lines.join("\n"));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let devices = devices(sh);
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
                crate::theme::field(
                    ui,
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text(tr("if the devices ask; not stored"))
                        .desired_width(200.0),
                );
                let busy = self.job.is_some();
                if theme::primary_button(
                    ui,
                    p,
                    &with_icon(icon::CLOUD_ARROW_DOWN, &trf("Back up {n} devices", &[("n", &devices.len())])),
                    !busy && !devices.is_empty(),
                )
                .clicked()
                {
                    self.start(ui.ctx(), devices.clone(), false);
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
            ui.add_space(4.0);
            schedule_ui(ui, p, sh, !self.password.is_empty());
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

/// The saved sessions that are backed up.
fn devices(sh: &Shared) -> Vec<Saved> {
    sh.settings.sessions.iter().filter(|s| !s.backup_command.trim().is_empty()).cloned().collect()
}

/// "Automatically: every day at 02:00", and when it runs next.
fn schedule_ui(ui: &mut Ui, p: &Palette, sh: &mut Shared, has_password: bool) {
    let s = &mut sh.settings.backup_schedule;
    let before = *s;
    let label = |e: Every| match e {
        Every::Off => tr("Off"),
        Every::Hour => tr("Every hour"),
        Every::SixHours => tr("Every 6 hours"),
        Every::Day => tr("Every day"),
        Every::Week => tr("Every week"),
    };
    let days =
        [tr("Monday"), tr("Tuesday"), tr("Wednesday"), tr("Thursday"), tr("Friday"), tr("Saturday"), tr("Sunday")];
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(tr("Automatically")).color(p.weak));
        egui::ComboBox::from_id_salt("bk-every").selected_text(label(s.every)).show_ui(ui, |ui| {
            for e in [Every::Off, Every::Hour, Every::SixHours, Every::Day, Every::Week] {
                ui.selectable_value(&mut s.every, e, label(e));
            }
        });
        if s.every == Every::Week {
            egui::ComboBox::from_id_salt("bk-day").selected_text(days[usize::from(s.weekday.min(6))]).show_ui(
                ui,
                |ui| {
                    for (i, d) in days.iter().enumerate() {
                        ui.selectable_value(&mut s.weekday, i as u8, *d);
                    }
                },
            );
        }
        if matches!(s.every, Every::Day | Every::Week) {
            ui.label(RichText::new(tr("at")).color(p.weak));
            egui::ComboBox::from_id_salt("bk-hour").selected_text(format!("{:02}:00", s.hour)).show_ui(ui, |ui| {
                for h in 0..24u8 {
                    ui.selectable_value(&mut s.hour, h, format!("{h:02}:00"));
                }
            });
        }
    });
    if *s != before {
        // Counted from now: changing the schedule does not start a run.
        sh.settings.backup_last = Local::now().timestamp();
    }
    let s = sh.settings.backup_schedule;
    if let Some(next) = s.next(sh.settings.backup_last, Local::now()) {
        let mut text = trf("Next: {time}, while the app is open.", &[("time", &next.format("%Y-%m-%d %H:%M"))]);
        if !has_password {
            text += " ";
            text += tr("Devices that ask for a password need it typed above in this session; SSH keys always work.");
        }
        ui.label(RichText::new(text).color(p.weak).size(12.5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).earliest().unwrap()
    }

    #[test]
    fn schedules() {
        let hourly = Schedule { every: Every::Hour, ..Default::default() };
        let now = at(2026, 10, 14, 12, 0);
        assert!(hourly.due(now.timestamp() - 3600, now));
        assert!(!hourly.due(now.timestamp() - 3500, now));
        // Every day at 02:00: due once 02:00 has passed since the last run.
        let daily = Schedule { every: Every::Day, hour: 2, weekday: 0 };
        assert!(daily.due(at(2026, 10, 13, 23, 0).timestamp(), at(2026, 10, 14, 2, 30)));
        assert!(!daily.due(at(2026, 10, 14, 2, 5).timestamp(), at(2026, 10, 14, 9, 0)));
        assert!(!daily.due(at(2026, 10, 13, 23, 0).timestamp(), at(2026, 10, 14, 1, 0)));
        assert_eq!(
            daily.next(at(2026, 10, 14, 2, 5).timestamp(), at(2026, 10, 14, 9, 0)),
            Some(at(2026, 10, 15, 2, 0))
        );
        // Every Monday at 03:00 (14 October 2026 is a Wednesday).
        let weekly = Schedule { every: Every::Week, hour: 3, weekday: 0 };
        assert!(weekly.due(at(2026, 10, 11, 0, 0).timestamp(), at(2026, 10, 14, 12, 0)));
        assert!(!weekly.due(at(2026, 10, 12, 4, 0).timestamp(), at(2026, 10, 14, 12, 0)));
        assert_eq!(
            weekly.next(at(2026, 10, 12, 4, 0).timestamp(), at(2026, 10, 14, 12, 0)),
            Some(at(2026, 10, 19, 3, 0))
        );
        assert!(!Schedule::default().due(0, now));
    }
}
