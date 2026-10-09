//! Monitor → Traffic per program: which programs use the network, how
//! fast, and how much since watching began.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::adapters::{format_bytes, format_speed};
use netmgr::traffic::{self, Counter, Usage};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::theme::{self, Palette, icon_label};

enum Msg {
    Sample(Vec<Counter>),
    Failed(anyhow::Error),
}

#[derive(Default)]
pub struct Programs {
    stop: Option<Arc<AtomicBool>>,
    rx: Option<Receiver<Msg>>,
    last: Option<(Vec<Counter>, Instant)>,
    /// Bytes per second over the last reading.
    rates: Vec<Usage>,
    /// Bytes since watching began, by program.
    totals: HashMap<String, (u64, u64)>,
    since: Option<Instant>,
    filter: String,
    started_once: bool,
}

impl Drop for Programs {
    fn drop(&mut self) {
        self.pause();
    }
}

impl Programs {
    pub fn running(&self) -> bool {
        self.stop.is_some()
    }

    fn start(&mut self, ctx: &egui::Context) {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (s, ctx) = (stop.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = traffic::watch(Duration::from_secs(2), &s, |c| {
                let ok = tx.send(Msg::Sample(c)).is_ok();
                ctx.request_repaint();
                ok
            });
            if let Err(e) = r {
                let _ = tx.send(Msg::Failed(e));
                ctx.request_repaint();
            }
        });
        self.stop = Some(stop);
        self.rx = Some(rx);
        self.since.get_or_insert_with(Instant::now);
    }

    fn pause(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        self.rx = None;
        self.last = None;
    }

    fn poll(&mut self, sh: &mut Shared) {
        let Some(rx) = &self.rx else { return };
        let mut failed = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Sample(now) => {
                    let at = Instant::now();
                    if let Some((before, then)) = &self.last {
                        let secs = at.duration_since(*then).as_secs_f64().max(0.5);
                        let used = traffic::compare(before, &now, traffic::NEW_COUNTERS_ARE_NEW);
                        for u in &used {
                            let t = self.totals.entry(u.program.clone()).or_default();
                            t.0 += u.received;
                            t.1 += u.sent;
                        }
                        self.rates = used
                            .into_iter()
                            .map(|mut u| {
                                u.received = (u.received as f64 / secs) as u64;
                                u.sent = (u.sent as f64 / secs) as u64;
                                u
                            })
                            .collect();
                    }
                    self.last = Some((now, at));
                }
                Msg::Failed(e) => failed = Some(e),
            }
        }
        if let Some(e) = failed {
            self.pause();
            sh.fail(trl("Traffic per program could not be read."), &e);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if !self.started_once {
            self.started_once = true;
            self.start(ui.ctx());
        }
        self.poll(sh);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                if cfg!(windows) {
                    trl("Which programs use the network and how fast, from Windows' TCP statistics (IPv4 connections).")
                } else if cfg!(target_os = "linux") {
                    trl(
                        "Which programs use the network and how fast, from the system's TCP statistics. Other users' programs appear only with administrator rights.",
                    )
                } else {
                    trl("Which programs use the network and how fast, as macOS counts it.")
                },
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if self.running() {
                    if ui.button(icon_label(icon::PAUSE, "Pause")).clicked() {
                        self.pause();
                    }
                    ui.spinner();
                } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Watch"), true).clicked() {
                    self.start(ui.ctx());
                }
                if ui.button(icon_label(icon::ARROW_COUNTER_CLOCKWISE, "Reset totals")).clicked() {
                    self.totals.clear();
                    self.since = Some(Instant::now());
                }
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(tr("Filter")).desired_width(180.0));
                if let Some(since) = self.since {
                    let m = since.elapsed().as_secs() / 60;
                    ui.label(RichText::new(trf("Totals for the last {n} min", &[("n", &m)])).color(p.weak).size(12.5));
                }
            });
        });
        ui.add_space(8.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            if self.rates.is_empty() && self.totals.is_empty() {
                ui.label(
                    RichText::new(if self.running() {
                        tr("Measuring… the first figures appear in a few seconds.")
                    } else {
                        tr("Click Watch to start.")
                    })
                    .color(p.weak),
                );
                return;
            }
            // Programs with a rate now, then the rest by total.
            let mut rows: Vec<(String, u64, u64, u64, u64, usize)> = self
                .totals
                .iter()
                .map(|(name, (rx, tx))| {
                    let r = self.rates.iter().find(|u| &u.program == name);
                    (
                        name.clone(),
                        r.map_or(0, |u| u.received),
                        r.map_or(0, |u| u.sent),
                        *rx,
                        *tx,
                        r.map_or(0, |u| u.connections),
                    )
                })
                .filter(|r| r.1 + r.2 + r.3 + r.4 > 0)
                .collect();
            let f = self.filter.to_lowercase();
            rows.retain(|r| f.is_empty() || r.0.to_lowercase().contains(&f));
            rows.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)).then((b.3 + b.4).cmp(&(a.3 + a.4))));
            let peak = rows.iter().map(|r| r.1 + r.2).max().unwrap_or(1).max(1);
            egui::ScrollArea::vertical().id_salt("programs").auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("programs-grid").num_columns(6).spacing([22.0, 6.0]).striped(true).show(ui, |ui| {
                    for h in [tr("Program"), tr("Download"), tr("Upload"), "", tr("Received"), tr("Sent")] {
                        ui.label(RichText::new(h).color(p.weak).size(12.5));
                    }
                    ui.end_row();
                    for (name, rx, tx, trx, ttx, _) in rows.iter().take(300) {
                        ui.label(RichText::new(name).color(p.text));
                        ui.label(RichText::new(format_speed(rx * 8)).monospace().color(if *rx > 0 {
                            p.text
                        } else {
                            p.weak
                        }));
                        ui.label(RichText::new(format_speed(tx * 8)).monospace().color(if *tx > 0 {
                            p.text
                        } else {
                            p.weak
                        }));
                        ui.add(
                            egui::ProgressBar::new((rx + tx) as f32 / peak as f32)
                                .desired_width(120.0)
                                .desired_height(6.0),
                        );
                        ui.label(RichText::new(format_bytes(*trx)).monospace().color(p.weak));
                        ui.label(RichText::new(format_bytes(*ttx)).monospace().color(p.weak));
                        ui.end_row();
                    }
                });
            });
        });
    }
}
