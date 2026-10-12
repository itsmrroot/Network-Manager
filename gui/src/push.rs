//! Console → Push: sends the same commands to several saved SSH sessions,
//! line by line as if typed, and shows what each device answered.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::backup::{self, Pushed};
use netmgr::remote::{Protocol, Saved};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, with_icon};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// Each device's result as the run goes: name, what it did or the error.
type Results = Arc<Mutex<Vec<(String, Result<Pushed, String>)>>>;

pub struct Push {
    commands: String,
    password: String,
    /// The sessions chosen, by name.
    chosen: BTreeSet<String>,
    stop_on_error: bool,
    backup_first: bool,
    confirm: bool,
    job: Option<Job<()>>,
    results: Results,
    /// The device whose answers are shown.
    shown: Option<String>,
}

impl Default for Push {
    fn default() -> Self {
        Self {
            commands: String::new(),
            password: String::new(),
            chosen: BTreeSet::new(),
            stop_on_error: true,
            backup_first: true,
            confirm: false,
            job: None,
            results: Default::default(),
            shown: None,
        }
    }
}

/// Ready-made commands: name, lines ({ip} is this computer's address).
const TEMPLATES: [(&str, &str); 5] = [
    ("Save the configuration (Cisco)", "write memory"),
    ("Syslog to this computer (Cisco)", "configure terminal\nlogging host {ip}\nend\nwrite memory"),
    (
        "SNMP traps to this computer (Cisco)",
        "configure terminal\nsnmp-server host {ip} version 2c public\nsnmp-server enable traps\nend\nwrite memory",
    ),
    ("Time from this computer (Cisco)", "configure terminal\nntp server {ip}\nend\nwrite memory"),
    ("Device name as hostname (Cisco)", "configure terminal\nhostname {name}\nend\nwrite memory"),
];

impl Push {
    /// Development aid: commands for a group, as sent.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self, sessions: &[Saved]) {
        self.chosen = sessions.iter().filter(|s| s.group == "Building A").map(|s| s.name.clone()).collect();
        self.commands = TEMPLATES[1].1.replace("{ip}", "10.10.0.50");
        let ok = |sent| {
            Pushed {
            transcript: "core-sw1#configure terminal\nEnter configuration commands, one per line.  End with CNTL/Z.\ncore-sw1(config)#logging host 10.10.0.50\ncore-sw1(config)#end\ncore-sw1#write memory\nBuilding configuration...\n[OK]\ncore-sw1#".into(),
            errors: Vec::new(),
            sent,
        }
        };
        if let Ok(mut r) = self.results.lock() {
            *r = vec![
                ("core-sw1".into(), Ok(ok(4))),
                (
                    "access-sw-2F".into(),
                    Ok(Pushed {
                        transcript: String::new(),
                        errors: vec![(
                            "logging host 10.10.0.50".into(),
                            "% Invalid input detected at '^' marker.".into(),
                        )],
                        sent: 2,
                    }),
                ),
            ];
        }
        self.shown = Some("core-sw1".into());
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job)
            && let Err(e) = r
        {
            sh.fail(trl("The commands could not be sent."), &e);
        }
        let sessions: Vec<Saved> =
            sh.settings.sessions.iter().filter(|s| s.protocol == Protocol::Ssh).cloned().collect();
        self.chosen.retain(|n| sessions.iter().any(|s| &s.name == n));
        let lines = backup::command_lines(&self.commands).len();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Sends the same commands to several devices over SSH, line by line as if typed, and waits for the prompt after each. {name} and {host} are replaced by each device's name and address.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            if sessions.is_empty() {
                theme::notice(
                    ui,
                    p,
                    p.accent,
                    icon::INFO,
                    trl("Save the devices first in SSH and Telnet: they appear here to choose from."),
                );
                return;
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(240.0);
                    self.devices_ui(ui, p, &sessions);
                });
                ui.add_space(12.0);
                ui.vertical(|ui| self.commands_ui(ui, p, sh));
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.stop_on_error, tr("Stop at the first error"));
                ui.checkbox(&mut self.backup_first, tr("Back up first"))
                    .on_hover_text(tr("Devices with a backup command are backed up before the commands are sent."));
                ui.label(RichText::new(tr("Password")).color(p.weak));
                crate::theme::field(
                    ui,
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text(tr("if the devices ask; not stored"))
                        .desired_width(180.0),
                );
                let busy = self.job.is_some();
                if theme::primary_button(
                    ui,
                    p,
                    &with_icon(icon::PAPER_PLANE_TILT, &trf("Send to {n} devices", &[("n", &self.chosen.len())])),
                    !busy && !self.chosen.is_empty() && lines > 0,
                )
                .clicked()
                {
                    self.confirm = true;
                }
                if let Some(j) = &self.job {
                    ui.spinner();
                    ui.label(RichText::new(j.progress.snapshot().message).color(p.weak));
                    if ui.button(icon_label(icon::STOP, "Stop")).clicked() {
                        j.stop();
                    }
                }
            });
        });
        if self.confirm {
            self.confirm_dialog(ui.ctx(), p, &sessions, lines);
        }
        ui.add_space(10.0);
        self.results_ui(ui, p, sh);
    }

    fn devices_ui(&mut self, ui: &mut Ui, p: &Palette, sessions: &[Saved]) {
        ui.horizontal(|ui| {
            ui.label(theme::semibold(tr("Devices"), 15.0).color(p.text));
            if ui.small_button(tr("All")).clicked() {
                self.chosen = sessions.iter().map(|s| s.name.clone()).collect();
            }
            if ui.small_button(tr("None")).clicked() {
                self.chosen.clear();
            }
        });
        let mut groups: Vec<&str> = sessions.iter().map(|s| s.group.as_str()).collect();
        groups.sort_unstable();
        groups.dedup();
        egui::ScrollArea::vertical().id_salt("push-devices").max_height(260.0).show(ui, |ui| {
            for g in groups {
                let members: Vec<&Saved> = sessions.iter().filter(|s| s.group == g).collect();
                if !g.is_empty() {
                    let all = members.iter().all(|s| self.chosen.contains(&s.name));
                    let mut on = all;
                    if ui.checkbox(&mut on, theme::semibold(g, 13.5)).changed() {
                        for s in &members {
                            if on {
                                self.chosen.insert(s.name.clone());
                            } else {
                                self.chosen.remove(&s.name);
                            }
                        }
                    }
                }
                for s in members {
                    ui.horizontal(|ui| {
                        if !g.is_empty() {
                            ui.add_space(16.0);
                        }
                        let mut on = self.chosen.contains(&s.name);
                        if ui.checkbox(&mut on, &s.name).on_hover_text(s.target()).changed() {
                            if on {
                                self.chosen.insert(s.name.clone());
                            } else {
                                self.chosen.remove(&s.name);
                            }
                        }
                    });
                }
            }
        });
    }

    fn commands_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        ui.horizontal(|ui| {
            ui.label(theme::semibold(tr("Commands"), 15.0).color(p.text));
            egui::ComboBox::from_id_salt("push-templates").selected_text(tr("Templates")).width(150.0).show_ui(
                ui,
                |ui| {
                    for (name, lines) in TEMPLATES {
                        if ui.selectable_label(false, crate::i18n::tr_dyn(name)).clicked() {
                            self.commands = lines.replace("{ip}", &crate::lab_servers::my_ip(sh));
                        }
                    }
                },
            );
        });
        crate::theme::field(
            ui,
            egui::TextEdit::multiline(&mut self.commands)
                .code_editor()
                .hint_text("configure terminal\nntp server 10.0.0.1\nend\nwrite memory")
                .desired_rows(10)
                .desired_width(ui.available_width()),
        );
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, p: &Palette, sessions: &[Saved], lines: usize) {
        let mut start = false;
        let modal = egui::Modal::new(egui::Id::new("push-confirm")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(theme::semibold(tr("Send the commands?"), 17.0).color(p.text));
            ui.add_space(6.0);
            theme::paragraph(
                ui,
                &trf(
                    "{lines} lines go to {n} devices and run as typed. Check them first: a wrong command can cut off the connection.",
                    &[("lines", &lines), ("n", &self.chosen.len())],
                ),
                14.0,
                p.text,
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, p, &icon_label(icon::PAPER_PLANE_TILT, "Send"), true).clicked() {
                    start = true;
                }
                if ui.button(tr("Cancel")).clicked() {
                    self.confirm = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm = false;
        }
        if start {
            self.confirm = false;
            let list: Vec<Saved> = sessions.iter().filter(|s| self.chosen.contains(&s.name)).cloned().collect();
            self.start(ctx, list);
        }
    }

    fn start(&mut self, ctx: &egui::Context, list: Vec<Saved>) {
        let password = Some(self.password.clone()).filter(|p| !p.is_empty());
        let (commands, stop_on_error, backup_first) = (self.commands.clone(), self.stop_on_error, self.backup_first);
        let results = self.results.clone();
        if let Ok(mut r) = results.lock() {
            r.clear();
        }
        self.shown = None;
        self.job = Some(Job::spawn(ctx, move |progress, cancel| {
            for (i, d) in list.iter().enumerate() {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                progress.set(i as f32 / list.len() as f32, &d.name);
                let r = (|| {
                    if backup_first && !d.backup_command.trim().is_empty() {
                        let text = backup::fetch(d, password.as_deref())?;
                        backup::store(&d.name, &text, SystemTime::now())?;
                    }
                    backup::push(d, password.as_deref(), &commands, stop_on_error)
                })()
                .map_err(|e| format!("{e:#}"));
                if let Ok(mut all) = results.lock() {
                    all.push((d.name.clone(), r));
                }
            }
            Ok(())
        }));
    }

    fn results_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let results = self.results.lock().map(|r| r.clone()).unwrap_or_default();
        if results.is_empty() {
            return;
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("Results"), 15.0).color(p.text));
            egui::Grid::new("push-results").num_columns(3).spacing([20.0, 4.0]).show(ui, |ui| {
                for (name, r) in &results {
                    ui.label(RichText::new(name).color(p.text));
                    let (text, color) = match r {
                        Ok(x) if x.errors.is_empty() => (trf("done: {n} lines", &[("n", &x.sent)]), p.success),
                        Ok(x) => (
                            crate::i18n::trn(x.errors.len() as u64, "1 command rejected", "{n} commands rejected"),
                            p.warning,
                        ),
                        Err(e) => (e.clone(), p.danger),
                    };
                    ui.label(RichText::new(text).color(color));
                    if r.is_ok() {
                        let open = self.shown.as_deref() == Some(name.as_str());
                        if ui.selectable_label(open, icon_label(icon::TERMINAL, "Answers")).clicked() {
                            self.shown = if open { None } else { Some(name.clone()) };
                        }
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
            let Some((name, Ok(x))) = results.iter().find(|(n, _)| Some(n) == self.shown.as_ref()) else { return };
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(theme::semibold(name, 14.0).color(p.text));
                if ui.small_button(icon_label(icon::COPY, "Copy")).clicked() {
                    ui.ctx().copy_text(x.transcript.clone());
                    sh.toast(tr("Copied."));
                }
            });
            for (line, answer) in &x.errors {
                ui.label(RichText::new(format!("{line}  →  {answer}")).monospace().size(12.5).color(p.warning));
            }
            egui::ScrollArea::both().id_salt("push-transcript").max_height(360.0).show(ui, |ui| {
                ui.label(RichText::new(&x.transcript).monospace().size(12.5).color(p.text));
            });
        });
    }
}
