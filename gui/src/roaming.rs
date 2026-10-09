//! Wi-Fi → Signal and roaming: the signal over the last minutes and every
//! move from one access point to another, while the page is open.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Sense, Stroke, Ui, Vec2, pos2};
use netmgr::wifi::{self, Connection};

use crate::i18n::{tr, trf};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette};

/// How long the graph looks back.
const SPAN: Duration = Duration::from_secs(600);
const EVERY: Duration = Duration::from_secs(3);

struct Sample {
    at: Instant,
    quality: Option<u8>,
    rssi: Option<i32>,
    /// The access point: its BSSID, or (macOS hides it) the channel.
    ap: Option<String>,
}

struct Roam {
    clock: String,
    from: String,
    to: String,
    /// Signal just before and after.
    before: Option<i32>,
    after: Option<i32>,
}

#[derive(Default)]
pub struct Roaming {
    samples: VecDeque<Sample>,
    roams: Vec<Roam>,
    job: Option<Job<Vec<Connection>>>,
    last: Option<Instant>,
}

fn ap_of(c: &Connection) -> Option<String> {
    match (&c.bssid, c.channel) {
        (Some(b), Some(ch)) => Some(format!("{b} · {ch}")),
        (Some(b), None) => Some(b.clone()),
        (None, Some(ch)) => Some(trf("channel {n}", &[("n", &ch)])),
        (None, None) => None,
    }
}

impl Roaming {
    /// Development aid: ten minutes of samples with two roams.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        let now = Instant::now();
        for i in 0..200u64 {
            let back = Duration::from_secs((200 - i) * 3);
            let ap = if i < 80 {
                "B4:FB:E4:10:20:31 · 36"
            } else if i < 150 {
                "B4:FB:E4:10:20:41 · 149"
            } else {
                "B4:FB:E4:10:20:31 · 36"
            };
            let wave = ((i as f64) * 0.21).sin() * 8.0;
            let base = if (60..80).contains(&i) {
                -74.0 + (i - 60) as f64 * -0.6
            } else if (130..150).contains(&i) {
                -70.0
            } else {
                -52.0
            };
            let rssi = (base + wave) as i32;
            self.samples.push_back(Sample {
                at: now - back,
                quality: Some(wifi::dbm_to_percent(rssi)),
                rssi: Some(rssi),
                ap: Some(ap.into()),
            });
        }
        self.roams = vec![
            Roam {
                clock: "10:31:12".into(),
                from: "B4:FB:E4:10:20:31 · 36".into(),
                to: "B4:FB:E4:10:20:41 · 149".into(),
                before: Some(-84),
                after: Some(-55),
            },
            Roam {
                clock: "10:34:42".into(),
                from: "B4:FB:E4:10:20:41 · 149".into(),
                to: "B4:FB:E4:10:20:31 · 36".into(),
                before: Some(-71),
                after: Some(-50),
            },
        ];
        self.last = Some(now + Duration::from_secs(3600));
    }

    /// Reads the connection every few seconds while shown.
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(Ok(list)) = jobs::finished(&mut self.job) {
            let c = list.into_iter().next();
            let sample = Sample {
                at: Instant::now(),
                quality: c.as_ref().and_then(Connection::quality),
                rssi: c.as_ref().and_then(|c| c.rssi),
                ap: c.as_ref().and_then(ap_of),
            };
            if let Some(prev) = self.samples.back()
                && let (Some(a), Some(b)) = (&prev.ap, &sample.ap)
                && a != b
            {
                self.roams.push(Roam {
                    clock: chrono::Local::now().format("%H:%M:%S").to_string(),
                    from: a.clone(),
                    to: b.clone(),
                    before: prev.rssi,
                    after: sample.rssi,
                });
            }
            self.samples.push_back(sample);
            while self.samples.front().is_some_and(|s| s.at.elapsed() > SPAN) {
                self.samples.pop_front();
            }
        }
        if self.job.is_none() && self.last.is_none_or(|t| t.elapsed() >= EVERY) {
            self.last = Some(Instant::now());
            self.job = Some(Job::spawn(ctx, |_, _| wifi::current()));
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette) {
        self.poll(ui.ctx());
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                crate::i18n::trl(
                    "The signal of this connection over the last 10 minutes, read every 3 seconds while this page is open. Walk around to find weak spots; a change of access point (roaming) is marked.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            if let Some(s) = self.samples.back() {
                ui.horizontal(|ui| {
                    if let Some(q) = s.quality {
                        theme::signal_bars(ui, p, q, 16.0);
                        ui.label(theme::semibold(format!("{q} %"), 16.0).color(p.text));
                    }
                    if let Some(r) = s.rssi {
                        ui.label(RichText::new(format!("{r} dBm")).color(p.weak));
                    }
                    if let Some(ap) = &s.ap {
                        ui.label(RichText::new(ap).monospace().color(p.weak));
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(tr("Reading the signal…")).color(p.weak));
                });
            }
            ui.add_space(6.0);
            self.graph(ui, p);
        });
        ui.add_space(10.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("Access point changes"), 15.0).color(p.text));
            if self.roams.is_empty() {
                ui.label(RichText::new(tr("None yet.")).color(p.weak));
                return;
            }
            egui::Grid::new("roams").num_columns(4).spacing([18.0, 4.0]).striped(true).show(ui, |ui| {
                for r in self.roams.iter().rev().take(50) {
                    ui.label(RichText::new(&r.clock).monospace().color(p.weak));
                    ui.label(RichText::new(&r.from).monospace().color(p.weak));
                    ui.label(RichText::new(format!("→ {}", r.to)).monospace().color(p.text));
                    let sig = |v: Option<i32>| v.map_or("—".to_string(), |v| format!("{v} dBm"));
                    ui.label(RichText::new(format!("{} → {}", sig(r.before), sig(r.after))).color(p.weak));
                    ui.end_row();
                }
            });
        });
    }

    fn graph(&self, ui: &mut Ui, p: &Palette) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 160.0), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, p.card_alt);
        // Guide lines at 25 % steps, with labels.
        for q in [25, 50, 75] {
            let y = rect.bottom() - rect.height() * q as f32 / 100.0;
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, p.border));
            painter.text(
                pos2(rect.left() + 6.0, y - 2.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{q} %"),
                egui::FontId::proportional(11.0),
                p.weak,
            );
        }
        let now = Instant::now();
        let x_of = |t: Instant| {
            let back = now.duration_since(t).as_secs_f32() / SPAN.as_secs_f32();
            rect.right() - rect.width() * back
        };
        let y_of = |q: u8| rect.bottom() - rect.height() * q as f32 / 100.0;
        let color = |q: u8| match q {
            60.. => p.success,
            35..=59 => p.warning,
            _ => p.danger,
        };
        let mut prev: Option<(egui::Pos2, u8)> = None;
        for s in &self.samples {
            let Some(q) = s.quality else {
                prev = None;
                continue;
            };
            let pt = pos2(x_of(s.at), y_of(q));
            if let Some((a, qa)) = prev {
                painter.line_segment([a, pt], Stroke::new(2.0, color(qa.min(q))));
            }
            prev = Some((pt, q));
        }
        for (i, s) in self.samples.iter().enumerate().skip(1) {
            if s.ap.is_some() && self.samples[i - 1].ap.is_some() && s.ap != self.samples[i - 1].ap {
                let x = x_of(s.at);
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, p.accent));
            }
        }
        painter.text(
            pos2(rect.right() - 6.0, rect.bottom() - 4.0),
            egui::Align2::RIGHT_BOTTOM,
            tr("now"),
            egui::FontId::proportional(11.0),
            Color32::GRAY,
        );
        painter.text(
            pos2(rect.left() + 6.0, rect.bottom() - 4.0),
            egui::Align2::LEFT_BOTTOM,
            tr("10 min ago"),
            egui::FontId::proportional(11.0),
            Color32::GRAY,
        );
    }
}
