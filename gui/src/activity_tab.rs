//! Servers → Activity: what others did with this computer, kept between runs, with
//! a search, "Save as…" and a dialog that deletes a part of it (the last hours,
//! today, older than some days, between two times, or everything), after saying how
//! many entries that is and, if wanted, saving a copy first.

use chrono::{Local, NaiveDateTime};
use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;

use crate::activity::{self, Kind, Span};
use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::theme::{self, Palette, icon_label};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Hours,
    Today,
    Yesterday,
    Older,
    Between,
    All,
}

/// The delete dialog.
struct Delete {
    choice: Choice,
    hours: u32,
    days: u32,
    from: String,
    to: String,
    save_first: bool,
}

impl Delete {
    fn new() -> Self {
        let now = Local::now();
        let time = |t: chrono::DateTime<Local>| t.format("%Y-%m-%d %H:%M").to_string();
        Self {
            choice: Choice::Hours,
            hours: 1,
            days: 30,
            from: time(now - chrono::Duration::hours(1)),
            to: time(now),
            save_first: false,
        }
    }

    /// The span the choice means; `None` when "between" has times that cannot be read.
    fn span(&self) -> Option<Span> {
        Some(match self.choice {
            Choice::Hours => Span::LastHours(self.hours),
            Choice::Today => Span::Today,
            Choice::Yesterday => Span::Yesterday,
            Choice::Older => Span::OlderThanDays(self.days),
            Choice::All => Span::All,
            Choice::Between => {
                let read = |s: &str| NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M").ok();
                // "To" includes its whole minute.
                Span::Between(read(&self.from)?, read(&self.to)? + chrono::Duration::seconds(59))
            }
        })
    }
}

#[derive(Default)]
pub struct ActivityTab {
    search: String,
    delete: Option<Delete>,
}

fn kind_glyph(p: &Palette, kind: Kind) -> (&'static str, egui::Color32) {
    match kind {
        Kind::Ssh => (icon::TERMINAL_WINDOW, p.warning),
        Kind::File => (icon::FILE_ARROW_DOWN, p.accent),
        Kind::Service => (icon::PLUGS_CONNECTED, p.success),
    }
}

/// Saves `events` where the user chooses; the kind of file follows the name.
fn save_as(sh: &mut Shared, events: &[activity::Event]) -> bool {
    let name = format!("activity-{}.csv", Local::now().format("%Y-%m-%d"));
    let Some(path) = rfd::FileDialog::new()
        .add_filter("CSV", &["csv"])
        .add_filter("Text", &["log", "txt"])
        .set_file_name(name)
        .save_file()
    else {
        return false;
    };
    match activity::export(&path, events) {
        Ok(()) => {
            sh.toast(trf("Activity saved to {file}.", &[("file", &path.display())]));
            true
        }
        Err(e) => {
            sh.fail(trl("The activity could not be saved."), &e.into());
            false
        }
    }
}

impl ActivityTab {
    /// Development aid: the delete dialog open (screenshot tour).
    #[cfg(debug_assertions)]
    pub fn demo_delete(&mut self) {
        self.delete = Some(Delete::new());
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        sh.activity.mark_read();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "What others do with this computer: SSH sign-ins while the app runs, files fetched or sent through the servers, and the first time a device uses a service. It is saved on this computer until you delete it.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Notify me about")).color(p.weak));
                ui.checkbox(&mut sh.settings.notify_ssh, tr("SSH sign-ins"));
                ui.checkbox(&mut sh.settings.notify_files, tr("File transfers"));
                ui.checkbox(&mut sh.settings.notify_services, tr("Other services"));
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                theme::field(
                    ui,
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(crate::i18n::with_icon(icon::MAGNIFYING_GLASS, tr("Search activity")))
                        .desired_width(220.0),
                );
                if theme::secondary_button(ui, &icon_label(icon::FLOPPY_DISK, tr("Save as…"))).clicked() {
                    save_as(sh, &sh.activity.all());
                }
                if theme::secondary_button(ui, &icon_label(icon::TRASH, tr("Delete…"))).clicked() {
                    self.delete = Some(Delete::new());
                }
            });
        });
        ui.add_space(10.0);

        let needle = self.search.trim().to_lowercase();
        let events: Vec<activity::Event> = sh
            .activity
            .newest(2000)
            .into_iter()
            .filter(|e| needle.is_empty() || e.text.to_lowercase().contains(&needle))
            .collect();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            if events.is_empty() {
                ui.label(RichText::new(tr("Nothing yet.")).color(p.weak));
                return;
            }
            let today = Local::now().date_naive();
            egui::ScrollArea::vertical().id_salt("activity-list").auto_shrink([false, true]).show(ui, |ui| {
                let mut day = None;
                for e in &events {
                    if day != Some(e.when.date_naive()) {
                        day = Some(e.when.date_naive());
                        ui.add_space(4.0);
                        let heading = if e.when.date_naive() == today {
                            tr("Today").to_string()
                        } else {
                            e.when.format("%Y-%m-%d").to_string()
                        };
                        ui.label(RichText::new(heading).strong().color(p.weak).size(12.5));
                    }
                    ui.horizontal(|ui| {
                        let (g, c) = kind_glyph(p, e.kind);
                        ui.label(
                            RichText::new(e.when.format("%H:%M:%S").to_string()).monospace().color(p.weak).size(12.5),
                        );
                        ui.label(RichText::new(g).color(c).size(16.0));
                        ui.label(RichText::new(crate::i18n::visual(&e.text)).color(p.text));
                    });
                }
            });
        });
        self.delete_dialog(ui.ctx(), p, sh);
    }

    fn delete_dialog(&mut self, ctx: &egui::Context, p: &Palette, sh: &mut Shared) {
        let Some(d) = self.delete.as_mut() else { return };
        let span = d.span();
        let count = span.map(|s| sh.activity.count(s));
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("activity-delete")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(theme::semibold(tr("Delete activity"), 19.0).color(p.text));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.choice, Choice::Hours, tr("The last"));
                if ui.add(egui::DragValue::new(&mut d.hours).range(1..=720).speed(0.2)).changed() {
                    d.choice = Choice::Hours;
                }
                ui.label(tr("hours"));
            });
            ui.radio_value(&mut d.choice, Choice::Today, tr("Today"));
            ui.radio_value(&mut d.choice, Choice::Yesterday, tr("Yesterday"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.choice, Choice::Older, tr("Older than"));
                if ui.add(egui::DragValue::new(&mut d.days).range(1..=3650).speed(0.3)).changed() {
                    d.choice = Choice::Older;
                }
                ui.label(tr("days"));
            });
            ui.radio_value(&mut d.choice, Choice::Between, tr("Between two times"));
            if d.choice == Choice::Between {
                ui.horizontal(|ui| {
                    ui.add_space(26.0);
                    ui.label(tr("From"));
                    ui.add(egui::TextEdit::singleline(&mut d.from).desired_width(130.0).hint_text("YYYY-MM-DD HH:MM"));
                    ui.label(tr("to"));
                    ui.add(egui::TextEdit::singleline(&mut d.to).desired_width(130.0).hint_text("YYYY-MM-DD HH:MM"));
                });
            }
            ui.radio_value(&mut d.choice, Choice::All, tr("Everything"));
            ui.add_space(8.0);
            ui.checkbox(&mut d.save_first, tr("Save a copy first…"));
            ui.add_space(6.0);
            match count {
                Some(n) => {
                    let text = if n == 0 {
                        tr("Nothing to delete.").to_string()
                    } else {
                        trf("{n} entries will be deleted.", &[("n", &n)])
                    };
                    ui.label(RichText::new(text).color(if n == 0 { p.weak } else { p.warning }));
                }
                None => {
                    ui.label(RichText::new(tr("The times are not valid (YYYY-MM-DD HH:MM).")).color(p.danger));
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, p, &format!("  {}  ", tr("Delete")), count.is_some_and(|n| n > 0))
                    .clicked()
                {
                    action = Some(true);
                }
                if theme::secondary_button(ui, tr("Cancel")).clicked() {
                    action = Some(false);
                }
            });
        });
        let confirmed = action == Some(true);
        if confirmed && let Some(span) = span {
            // The copy comes first: a deletion that was not saved cannot be undone.
            let events: Vec<activity::Event> = {
                let now = Local::now();
                sh.activity.all().into_iter().filter(|e| span.contains(e.when, now)).collect()
            };
            let go = !d.save_first || save_as(sh, &events);
            if go {
                let n = sh.activity.delete(span);
                sh.toast(trf("{n} entries deleted.", &[("n", &n)]));
                self.delete = None;
            }
        } else if action == Some(false) || modal.should_close() {
            self.delete = None;
        }
    }
}
