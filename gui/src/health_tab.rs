//! Monitor → Connection history: snapshots of the router, the internet, a name
//! lookup and the Wi-Fi signal over time, as a graph, with a summary of outages.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Sense, Stroke, Ui, Vec2, pos2};
use egui_phosphor::regular as icon;
use netmgr::health::{self, Alert, Snapshot};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::theme::{self, Palette, icon_label};

/// The choices for how often a snapshot is taken: minutes, 0 for never.
const EVERY: [u32; 5] = [0, 5, 15, 30, 60];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Range {
    Day,
    Week,
    Month,
}

impl Range {
    fn seconds(self) -> i64 {
        match self {
            Range::Day => 24 * 3600,
            Range::Week => 7 * 24 * 3600,
            Range::Month => 30 * 24 * 3600,
        }
    }
}

pub struct HealthTab {
    snapshots: Vec<Snapshot>,
    loaded: bool,
    range: Range,
    /// A snapshot is being taken.
    busy: Arc<AtomicBool>,
    /// What the thread that takes them hands over.
    fresh: Arc<Mutex<Vec<Snapshot>>>,
    next_due: Option<Instant>,
    /// The kind of the alert that is on now (so that it is told once).
    alerted: Option<std::mem::Discriminant<Alert>>,
}

impl Default for HealthTab {
    fn default() -> Self {
        Self {
            snapshots: Vec::new(),
            loaded: false,
            range: Range::Day,
            busy: Arc::default(),
            fresh: Arc::default(),
            next_due: None,
            alerted: None,
        }
    }
}

fn clock(time: i64) -> String {
    chrono::DateTime::from_timestamp(time, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%d.%m. %H:%M").to_string())
        .unwrap_or_default()
}

impl HealthTab {
    fn take(&mut self, ctx: &egui::Context, sh: &Shared) {
        if self.busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let gateway = sh.default_adapter().and_then(|a| a.gateway);
        let (busy, fresh, ctx) = (self.busy.clone(), self.fresh.clone(), ctx.clone());
        std::thread::spawn(move || {
            let s = health::take(gateway);
            let _ = health::append(&s);
            if let Ok(mut f) = fresh.lock() {
                f.push(s);
            }
            busy.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Takes the snapshots that are due and tells about trouble, on any page.
    pub fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if !self.loaded {
            self.snapshots = health::load_all();
            self.loaded = true;
        }
        let fresh: Vec<Snapshot> = self.fresh.lock().map(|mut f| std::mem::take(&mut *f)).unwrap_or_default();
        for s in fresh {
            match health::alert(&self.snapshots, &s) {
                Some(a) if self.alerted != Some(std::mem::discriminant(&a)) => {
                    self.alerted = Some(std::mem::discriminant(&a));
                    let text = match a {
                        Alert::InternetDown => tr("The internet is not reachable.").to_string(),
                        Alert::Slow(ms) => trf("The internet is slow: {ms} ms.", &[("ms", &(ms.round() as u32))]),
                    };
                    if sh.settings.notifications && sh.settings.notify_slow {
                        crate::notify::send(tr("Network Manager"), &text);
                    }
                    sh.toast(text);
                }
                Some(_) => {}
                None => self.alerted = None,
            }
            self.snapshots.push(s);
        }
        let minutes = sh.settings.health_minutes;
        if minutes == 0 {
            self.next_due = None;
            return;
        }
        let every = Duration::from_secs(u64::from(minutes) * 60);
        match self.next_due {
            Some(t) if Instant::now() < t => ctx.request_repaint_after(t - Instant::now()),
            _ => {
                if !self.busy.load(Ordering::Relaxed) {
                    self.take(ctx, sh);
                    self.next_due = Some(Instant::now() + every);
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::CHART_LINE, tr("Connection history"));
            theme::paragraph(
                ui,
                trl(
                    "While the app is open, the router and the internet are pinged now and then, a name is looked up and the Wi-Fi signal is read. Everything stays on this computer.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Take a snapshot every"));
                let label = |m: u32| match m {
                    0 => tr("Off").to_string(),
                    60 => tr("1 hour").to_string(),
                    m => trf("{n} minutes", &[("n", &m)]),
                };
                egui::ComboBox::from_id_salt("health-every").selected_text(label(sh.settings.health_minutes)).show_ui(
                    ui,
                    |ui| {
                        for m in EVERY {
                            ui.selectable_value(&mut sh.settings.health_minutes, m, label(m));
                        }
                    },
                );
                if theme::secondary_button(ui, &icon_label(icon::CAMERA, tr("Take one now"))).clicked() {
                    self.take(ui.ctx(), sh);
                }
                if theme::secondary_button(ui, &icon_label(icon::TRASH, tr("Delete history"))).clicked()
                    && health::clear().is_ok()
                {
                    self.snapshots.clear();
                    self.alerted = None;
                }
                if self.busy.load(Ordering::Relaxed) {
                    ui.spinner();
                }
            });
        });
        ui.add_space(12.0);

        let now = self.snapshots.last().map_or(0, |s| s.time).max(chrono::Utc::now().timestamp());
        ui.horizontal(|ui| {
            for (r, label) in [
                (Range::Day, tr("Last 24 hours")),
                (Range::Week, tr("Last 7 days")),
                (Range::Month, tr("Last 30 days")),
            ] {
                if ui.selectable_label(self.range == r, label).clicked() {
                    self.range = r;
                }
            }
        });
        let from = now - self.range.seconds();
        let shown: Vec<Snapshot> = self.snapshots.iter().filter(|s| s.time >= from).cloned().collect();
        if shown.is_empty() {
            ui.add_space(8.0);
            theme::paragraph(ui, trl("No snapshots yet. Choose how often above, or take one now."), 14.5, p.weak);
            return;
        }
        let summary = health::summarize(&shown);
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let ms = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{} ms", v.round() as u32));
            theme::stat(ui, p, tr("Reachable"), &format!("{:.1} %", summary.reachable_percent));
            ui.add_space(20.0);
            theme::stat(ui, p, tr("Typical delay"), &ms(summary.median_ms));
            ui.add_space(20.0);
            theme::stat(ui, p, tr("Worst delay"), &ms(summary.worst_ms));
            ui.add_space(20.0);
            theme::stat(ui, p, tr("Internet outages"), &summary.outages.len().to_string());
        });
        ui.add_space(8.0);
        graph(ui, p, &shown, from, now);
        if !summary.outages.is_empty() {
            ui.add_space(8.0);
            for (start, n) in summary.outages.iter().rev().take(5) {
                ui.label(
                    egui::RichText::new(trf(
                        "{start}: {n} snapshots in a row without internet",
                        &[("start", &clock(*start)), ("n", n)],
                    ))
                    .color(p.warning),
                );
            }
        }
    }
}

/// Delay to the internet and to the router over the time span; a red mark at the
/// bottom where the internet did not answer.
fn graph(ui: &mut Ui, p: &Palette, list: &[Snapshot], from: i64, to: i64) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 180.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 8.0, p.card_alt);
    let top = list.iter().flat_map(|s| [s.internet_ms, s.gateway_ms]).flatten().fold(50.0_f64, f64::max).min(2000.0);
    let span = (to - from).max(1) as f32;
    let x_of = |t: i64| rect.left() + rect.width() * (t - from) as f32 / span;
    let y_of = |ms: f64| rect.bottom() - 14.0 - (rect.height() - 24.0) * (ms / top).clamp(0.0, 1.0) as f32;
    for q in [0.5, 1.0] {
        let y = y_of(top * q);
        painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, p.border));
        // The top label sits under its line, so that it is not cut off by the frame.
        let (at, align) = if q >= 1.0 {
            (pos2(rect.left() + 6.0, y + 2.0), egui::Align2::LEFT_TOP)
        } else {
            (pos2(rect.left() + 6.0, y - 2.0), egui::Align2::LEFT_BOTTOM)
        };
        painter.text(at, align, format!("{} ms", (top * q).round() as u32), egui::FontId::proportional(11.0), p.weak);
    }
    // A gap of more than three snapshot intervals breaks the line (the app was closed).
    let gap = if list.len() > 2 {
        let mut d: Vec<i64> = list.windows(2).map(|w| w[1].time - w[0].time).collect();
        d.sort_unstable();
        (d[d.len() / 2] * 3).max(600)
    } else {
        i64::MAX
    };
    for (color, get) in [
        (p.weak, (|s: &Snapshot| s.gateway_ms) as fn(&Snapshot) -> Option<f64>),
        (p.accent, |s: &Snapshot| s.internet_ms),
    ] {
        let mut prev: Option<(i64, egui::Pos2)> = None;
        for s in list {
            let Some(ms) = get(s) else {
                prev = None;
                continue;
            };
            let pt = pos2(x_of(s.time), y_of(ms));
            match prev {
                Some((t, a)) if s.time - t <= gap => {
                    painter.line_segment([a, pt], Stroke::new(1.8, color));
                }
                _ => {
                    painter.circle_filled(pt, 1.8, color);
                }
            }
            prev = Some((s.time, pt));
        }
    }
    for s in list.iter().filter(|s| s.internet_ms.is_none()) {
        let x = x_of(s.time);
        painter.line_segment([pos2(x, rect.bottom() - 12.0), pos2(x, rect.bottom() - 3.0)], Stroke::new(2.0, p.danger));
    }
    painter.text(
        pos2(rect.left() + 6.0, rect.bottom() - 1.0),
        egui::Align2::LEFT_BOTTOM,
        clock(from),
        egui::FontId::proportional(10.5),
        p.weak,
    );
    painter.text(
        pos2(rect.right() - 6.0, rect.bottom() - 1.0),
        egui::Align2::RIGHT_BOTTOM,
        clock(to),
        egui::FontId::proportional(10.5),
        p.weak,
    );
    // The values under the pointer.
    if let Some(pos) = response.hover_pos() {
        let t = from + ((pos.x - rect.left()) / rect.width() * span) as i64;
        if let Some(s) = list.iter().min_by_key(|s| (s.time - t).abs()) {
            let ms = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{} ms", v.round() as u32));
            response.on_hover_text(format!(
                "{}\n{}: {}\n{}: {}\n{}: {}{}",
                clock(s.time),
                tr("Internet"),
                ms(s.internet_ms),
                tr("Router"),
                ms(s.gateway_ms),
                tr("Name lookup"),
                ms(s.lookup_ms),
                s.wifi_signal.map_or(String::new(), |q| format!("\nWi-Fi: {q} %")),
            ));
        }
    }
}
