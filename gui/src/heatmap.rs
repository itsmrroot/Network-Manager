//! Wi-Fi → Heat map: a floor plan, the Wi-Fi signal measured at the places
//! clicked, and the colours in between, so weak spots show at a glance.

use std::path::Path;

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, Ui, Vec2, pos2};
use egui_phosphor::regular as icon;
use netmgr::wifi;
use serde::{Deserialize, Serialize};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// A value at a place: x and y from 0 to 1, the value.
type Sample = (f32, f32, f32);

/// A measurement at one place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Where on the plan, from 0 to 1 across and down.
    pub x: f32,
    pub y: f32,
    /// Signal strength, dBm.
    pub rssi: i32,
    pub rate: Option<f64>,
    pub ssid: Option<String>,
    /// The access point: its BSSID, or (macOS hides it) the channel.
    pub ap: Option<String>,
    pub channel: Option<u32>,
    pub band: Option<String>,
    pub noise: Option<i32>,
    /// Unix seconds.
    pub when: i64,
}

/// A survey: the plan and the measurements, kept in one file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Survey {
    /// The floor plan as PNG or JPEG, base64; empty for a blank grid.
    pub plan: String,
    pub points: Vec<Point>,
}

/// What the colours show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    Signal,
    Rate,
}

pub struct Heatmap {
    survey: Survey,
    loaded: bool,
    plan: Option<TextureHandle>,
    /// The plan's width over its height.
    aspect: f32,
    overlay: Option<TextureHandle>,
    /// The overlay must be drawn again.
    dirty: bool,
    show: Show,
    /// Only the points measured on this access point.
    ap: Option<String>,
    /// Where a measurement is going on.
    reading: Option<Pos2>,
    job: Option<Job<wifi::Connection>>,
    undo: Vec<Survey>,
    /// Sample data: nothing is saved.
    demo: bool,
}

impl Default for Heatmap {
    fn default() -> Self {
        Self {
            survey: Survey::default(),
            loaded: false,
            plan: None,
            aspect: 4.0 / 3.0,
            overlay: None,
            dirty: true,
            show: Show::Signal,
            ap: None,
            reading: None,
            job: None,
            undo: Vec::new(),
            demo: false,
        }
    }
}

fn autosave_path() -> std::path::PathBuf {
    netmgr::profiles::config_dir().join("heatmap.json")
}

// ---------------------------------------------------------------- base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = (acc << 6) | B64.iter().position(|&b| b == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- colours

/// Signal strength to colour: green is strong, red is weak.
pub fn signal_color(dbm: f32) -> Color32 {
    const STOPS: [(f32, [u8; 3]); 5] = [
        (-45.0, [22, 163, 74]),
        (-60.0, [132, 204, 22]),
        (-67.0, [250, 204, 21]),
        (-75.0, [249, 115, 22]),
        (-85.0, [220, 38, 38]),
    ];
    if dbm >= STOPS[0].0 {
        let [r, g, b] = STOPS[0].1;
        return Color32::from_rgb(r, g, b);
    }
    for w in STOPS.windows(2) {
        let ((hi, a), (lo, b)) = (w[0], w[1]);
        if dbm >= lo {
            let t = (hi - dbm) / (hi - lo);
            let mix = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8;
            return Color32::from_rgb(mix(0), mix(1), mix(2));
        }
    }
    let [r, g, b] = STOPS[4].1;
    Color32::from_rgb(r, g, b)
}

/// What a value is worth on the signal scale: link rates are spread over
/// it, from the lowest to the highest measured.
fn as_signal(v: f32, show: Show, range: (f32, f32)) -> f32 {
    match show {
        Show::Signal => v,
        Show::Rate => {
            let (lo, hi) = range;
            let t = if hi > lo { ((v - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 1.0 };
            -85.0 + t * 40.0
        }
    }
}

/// Inverse-distance weighting: the value at each cell of a `w` × `h` grid
/// from `samples` (x, y from 0 to 1, value), and how sure it is (1 near a
/// sample, 0 far from all).
pub fn field(samples: &[(f32, f32, f32)], w: usize, h: usize, aspect: f32) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(w * h);
    // Distances measured with the plan's own proportions.
    let (sx, sy) = if aspect >= 1.0 { (1.0, 1.0 / aspect) } else { (aspect, 1.0) };
    for j in 0..h {
        for i in 0..w {
            let (x, y) = ((i as f32 + 0.5) / w as f32, (j as f32 + 0.5) / h as f32);
            let (mut num, mut den, mut nearest) = (0.0f32, 0.0f32, f32::MAX);
            for &(px, py, v) in samples {
                let d2 = ((x - px) * sx).powi(2) + ((y - py) * sy).powi(2);
                nearest = nearest.min(d2.sqrt());
                let wgt = 1.0 / (d2 + 1e-5);
                num += wgt * v;
                den += wgt;
            }
            let sure = if samples.is_empty() { 0.0 } else { (1.0 - (nearest - 0.12) / 0.2).clamp(0.0, 1.0) };
            out.push((if den > 0.0 { num / den } else { 0.0 }, sure));
        }
    }
    out
}

impl Heatmap {
    /// Development aid: a floor plan with two access points' signal.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self, ctx: &egui::Context) {
        use image::{Rgba, RgbaImage};
        self.demo = true;
        self.loaded = true;
        let (w, h) = (1200u32, 760u32);
        let mut img = RgbaImage::from_pixel(w, h, Rgba([250, 250, 248, 255]));
        let mut wall = |x0: u32, y0: u32, x1: u32, y1: u32| {
            for x in x0.min(x1)..=x0.max(x1) {
                for y in y0.min(y1)..=y0.max(y1) {
                    img.put_pixel(x.min(w - 1), y.min(h - 1), Rgba([70, 74, 82, 255]));
                }
            }
        };
        // Outer walls, rooms along the top and bottom, a corridor between.
        wall(20, 20, 1180, 26);
        wall(20, 734, 1180, 740);
        wall(20, 20, 26, 740);
        wall(1174, 20, 1180, 740);
        wall(20, 300, 520, 306);
        wall(600, 300, 1180, 306);
        wall(20, 470, 380, 476);
        wall(460, 470, 1180, 476);
        for x in [300, 620, 900] {
            wall(x, 20, x + 6, 240);
        }
        for x in [400, 760] {
            wall(x, 530, x + 6, 740);
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        let _ = image::DynamicImage::ImageRgba8(img).write_to(&mut bytes, image::ImageFormat::Png);
        self.survey.plan = base64(bytes.get_ref());
        let aps = [(0.22f32, 0.35f32, "B4:FB:E4:10:20:31"), (0.8, 0.62, "B4:FB:E4:10:20:41")];
        self.survey.points.clear();
        for j in 0..5 {
            for i in 0..8 {
                let (x, y) = (0.07 + i as f32 * 0.123, 0.1 + j as f32 * 0.2);
                let (best, ap) = aps
                    .iter()
                    .map(|(ax, ay, name)| {
                        let d = ((x - ax) * 1.58).hypot(y - ay);
                        (-36.0 - 52.0 * (1.0 + d * 6.0).log10() - 6.0 * ((i * 7 + j * 3) % 3) as f32, *name)
                    })
                    .fold((f32::MIN, ""), |a, b| if b.0 > a.0 { b } else { a });
                self.survey.points.push(Point {
                    x,
                    y,
                    rssi: best as i32,
                    rate: Some((1200.0 * ((best + 90.0) / 55.0).clamp(0.05, 1.0)) as f64),
                    ssid: Some("Office".into()),
                    ap: Some(ap.into()),
                    channel: Some(36),
                    band: Some("5 GHz".into()),
                    noise: Some(-94),
                    when: 0,
                });
            }
        }
        self.load_plan(ctx);
    }

    fn load(&mut self, ctx: &egui::Context) {
        self.loaded = true;
        if let Ok(text) = std::fs::read_to_string(autosave_path())
            && let Ok(s) = serde_json::from_str::<Survey>(&text)
        {
            self.survey = s;
            self.load_plan(ctx);
        }
    }

    fn autosave(&self) {
        if self.demo {
            return;
        }
        let path = autosave_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(&self.survey) {
            let _ = std::fs::write(path, text);
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.autosave();
    }

    fn remember(&mut self) {
        self.undo.push(self.survey.clone());
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
    }

    /// The plan's picture, made smaller when it is very large.
    fn load_plan(&mut self, ctx: &egui::Context) {
        self.plan = None;
        self.aspect = 4.0 / 3.0;
        self.dirty = true;
        let Some(bytes) = unbase64(&self.survey.plan).filter(|b| !b.is_empty()) else { return };
        let Ok(img) = image::load_from_memory(&bytes) else { return };
        let img = if img.width().max(img.height()) > 2400 { img.thumbnail(2400, 2400) } else { img };
        let rgba = img.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        self.aspect = size[0] as f32 / size[1].max(1) as f32;
        let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
        self.plan = Some(ctx.load_texture("floor-plan", image, egui::TextureOptions::LINEAR));
    }

    fn open_plan(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        let Some(path) = rfd::FileDialog::new().add_filter(tr("Floor plan"), &["png", "jpg", "jpeg"]).pick_file()
        else {
            return;
        };
        match std::fs::read(&path) {
            Ok(bytes) if image::load_from_memory(&bytes).is_ok() => {
                self.remember();
                let keep = !self.survey.points.is_empty() && self.survey.plan.is_empty();
                self.survey.plan = base64(&bytes);
                if !keep {
                    self.survey.points.clear();
                }
                self.load_plan(ctx);
                self.changed();
            }
            Ok(_) => sh.toast(tr("This picture could not be read: use PNG or JPEG.")),
            Err(e) => sh.fail(trl("The floor plan could not be opened."), &e.into()),
        }
    }

    /// The values shown (x, y, value), and their lowest and highest.
    fn samples(&self) -> (Vec<Sample>, (f32, f32)) {
        let points: Vec<&Point> = self.survey.points.iter().filter(|p| self.ap.is_none() || p.ap == self.ap).collect();
        let values: Vec<(f32, f32, f32)> = points
            .iter()
            .filter_map(|p| {
                let v = match self.show {
                    Show::Signal => Some(p.rssi as f32),
                    Show::Rate => p.rate.map(|r| r as f32),
                };
                v.map(|v| (p.x, p.y, v))
            })
            .collect();
        let lo = values.iter().map(|s| s.2).fold(f32::MAX, f32::min);
        let hi = values.iter().map(|s| s.2).fold(f32::MIN, f32::max);
        (values, (lo, hi))
    }

    /// The coloured layer, `w` cells across.
    fn overlay_image(&self, w: usize, alpha: f32) -> ColorImage {
        let h = ((w as f32 / self.aspect).round() as usize).max(1);
        let (samples, range) = self.samples();
        let cells = field(&samples, w, h, self.aspect);
        let mut img = ColorImage::new([w, h], vec![Color32::TRANSPARENT; w * h]);
        for (px, (v, sure)) in img.pixels.iter_mut().zip(cells) {
            let c = signal_color(as_signal(v, self.show, range));
            *px = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (sure * alpha * 255.0) as u8);
        }
        img
    }

    fn start_reading(&mut self, ctx: &egui::Context, at: Pos2) {
        self.reading = Some(at);
        self.job = Some(Job::spawn(ctx, |_, _| {
            // Two readings, averaged: one alone jumps around.
            let mut got: Vec<wifi::Connection> = Vec::new();
            for i in 0..2 {
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                }
                if let Some(c) = wifi::current()?.into_iter().next() {
                    got.push(c);
                }
            }
            let mut c = got.pop().ok_or_else(|| anyhow::anyhow!("this computer is not connected to Wi-Fi"))?;
            let rssi: Vec<i32> = std::iter::once(&c)
                .chain(got.iter())
                .filter_map(|c| c.rssi.or_else(|| c.signal.map(|q| i32::from(q) / 2 - 100)))
                .collect();
            if rssi.is_empty() {
                anyhow::bail!("the system did not report the signal strength");
            }
            c.rssi = Some(rssi.iter().sum::<i32>() / rssi.len() as i32);
            Ok(c)
        }));
    }

    fn poll(&mut self, sh: &mut Shared) {
        let Some(r) = jobs::finished(&mut self.job) else { return };
        let Some(at) = self.reading.take() else { return };
        match r {
            Ok(c) => {
                self.remember();
                self.survey.points.push(Point {
                    x: at.x,
                    y: at.y,
                    rssi: c.rssi.unwrap_or(-90),
                    rate: c.rate_mbps,
                    ssid: c.ssid.clone(),
                    ap: match (&c.bssid, c.channel) {
                        (Some(b), _) => Some(b.clone()),
                        (None, Some(ch)) => Some(trf("channel {n}", &[("n", &ch)])),
                        (None, None) => None,
                    },
                    channel: c.channel,
                    band: c.band.clone(),
                    noise: c.noise,
                    when: chrono::Local::now().timestamp(),
                });
                self.changed();
            }
            Err(e) => sh.fail(trl("The Wi-Fi signal could not be measured here."), &e),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if !self.loaded {
            self.load(ui.ctx());
        }
        self.poll(sh);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Open a floor plan, walk around with this computer and click where you stand: the Wi-Fi signal is measured there. The colours fill in between, so weak spots show at a glance.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            self.toolbar(ui, p, sh);
        });
        ui.add_space(10.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            self.canvas(ui, p);
            ui.add_space(8.0);
            legend(ui, p, self.show, self.samples().1);
        });
    }

    fn toolbar(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        ui.horizontal_wrapped(|ui| {
            if ui.button(icon_label(icon::IMAGE, "Open floor plan…")).clicked() {
                self.open_plan(ui.ctx(), sh);
            }
            if ui.button(icon_label(icon::FOLDER_OPEN, "Open survey…")).clicked() {
                self.open_survey(ui.ctx(), sh);
            }
            if ui.button(icon_label(icon::FLOPPY_DISK, "Save survey…")).clicked() {
                self.save_survey(sh);
            }
            if ui
                .add_enabled(
                    !self.survey.points.is_empty(),
                    egui::Button::new(icon_label(icon::EXPORT, "Export picture…")),
                )
                .clicked()
            {
                self.export(sh);
            }
            if ui
                .add_enabled(
                    !self.undo.is_empty(),
                    egui::Button::new(icon_label(icon::ARROW_COUNTER_CLOCKWISE, "Undo")),
                )
                .clicked()
                && let Some(prev) = self.undo.pop()
            {
                let plan_changed = prev.plan != self.survey.plan;
                self.survey = prev;
                if plan_changed {
                    self.load_plan(ui.ctx());
                }
                self.changed();
            }
            if ui
                .add_enabled(!self.survey.points.is_empty(), egui::Button::new(icon_label(icon::TRASH, "Clear points")))
                .clicked()
            {
                self.remember();
                self.survey.points.clear();
                self.changed();
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tr("Show")).color(p.weak));
            let before = (self.show, self.ap.clone());
            ui.selectable_value(&mut self.show, Show::Signal, tr("Signal strength"));
            ui.selectable_value(&mut self.show, Show::Rate, tr("Link speed"));
            let mut aps: Vec<String> = self.survey.points.iter().filter_map(|p| p.ap.clone()).collect();
            aps.sort();
            aps.dedup();
            if aps.len() > 1 {
                ui.label(RichText::new(tr("Access point")).color(p.weak));
                egui::ComboBox::from_id_salt("heat-ap")
                    .selected_text(self.ap.clone().unwrap_or_else(|| tr("All").to_string()))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.ap, None, tr("All"));
                        for a in aps {
                            ui.selectable_value(&mut self.ap, Some(a.clone()), a);
                        }
                    });
            }
            if (self.show, self.ap.clone()) != before {
                self.dirty = true;
            }
            let n = self.survey.points.len();
            ui.label(RichText::new(trf("{n} points", &[("n", &n)])).color(p.weak).size(12.5));
            if self.reading.is_some() {
                ui.spinner();
                ui.label(RichText::new(tr("Measuring…")).color(p.weak));
            }
        });
    }

    fn canvas(&mut self, ui: &mut Ui, p: &Palette) {
        let width = ui.available_width();
        let height = (width / self.aspect).min(720.0);
        let width = height * self.aspect;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
        let painter = ui.painter_at(rect);
        match &self.plan {
            Some(t) => {
                painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                // A blank grid: a survey works without a plan too.
                painter.rect_filled(rect, 6.0, p.card_alt);
                let step = rect.width() / 16.0;
                let stroke = Stroke::new(1.0, p.weak.gamma_multiply(0.18));
                for i in 1..16 {
                    let x = rect.left() + step * i as f32;
                    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], stroke);
                }
                let mut y = rect.top() + step;
                while y < rect.bottom() {
                    painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
                    y += step;
                }
                if self.survey.points.is_empty() {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        tr("Open a floor plan, or click on the grid to measure"),
                        egui::FontId::proportional(15.0),
                        p.weak,
                    );
                }
            }
        }
        if self.dirty {
            self.dirty = false;
            self.overlay = (!self.survey.points.is_empty()).then(|| {
                ui.ctx().load_texture("heat-overlay", self.overlay_image(160, 0.55), egui::TextureOptions::LINEAR)
            });
        }
        if let Some(t) = &self.overlay {
            painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        let to_screen = |x: f32, y: f32| pos2(rect.left() + x * rect.width(), rect.top() + y * rect.height());
        let mut remove = None;
        for (i, pt) in self.survey.points.iter().enumerate() {
            if self.ap.is_some() && pt.ap != self.ap {
                continue;
            }
            let at = to_screen(pt.x, pt.y);
            painter.circle(at, 6.0, signal_color(pt.rssi as f32), Stroke::new(1.5, Color32::WHITE));
            let label = match self.show {
                Show::Signal => format!("{}", pt.rssi),
                Show::Rate => pt.rate.map(|r| format!("{r:.0}")).unwrap_or_default(),
            };
            painter.text(
                at + Vec2::new(0.0, -9.0),
                egui::Align2::CENTER_BOTTOM,
                label,
                egui::FontId::monospace(11.0),
                Color32::BLACK,
            );
            let hit = ui.interact(
                Rect::from_center_size(at, Vec2::splat(16.0)),
                ui.id().with(("heat-pt", i)),
                Sense::click(),
            );
            let mut tip = format!("{} dBm", pt.rssi);
            if let Some(r) = pt.rate {
                tip += &format!(" · {r:.0} Mbit/s");
            }
            if let Some(n) = pt.noise {
                tip += &format!(" · {} {n} dBm", tr("noise"));
            }
            for x in [&pt.ssid, &pt.ap, &pt.band].into_iter().flatten() {
                tip += &format!("\n{x}");
            }
            let hit = hit.on_hover_text(tip);
            hit.context_menu(|ui| {
                if ui.button(icon_label(icon::TRASH, "Remove")).clicked() {
                    remove = Some(i);
                    ui.close();
                }
            });
        }
        if let Some(i) = remove {
            self.remember();
            self.survey.points.remove(i);
            self.changed();
        }
        if let Some(at) = &self.reading {
            let c = to_screen(at.x, at.y);
            painter.circle_stroke(c, 9.0, Stroke::new(2.0, p.accent));
        }
        if response.clicked()
            && self.reading.is_none()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let at = pos2((pos.x - rect.left()) / rect.width(), (pos.y - rect.top()) / rect.height());
            self.start_reading(ui.ctx(), at);
        }
        if self.reading.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
        response.on_hover_cursor(egui::CursorIcon::Crosshair);
    }

    fn save_survey(&self, sh: &mut Shared) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(tr("Wi-Fi survey"), &["netsurvey"])
            .set_file_name("wifi-survey.netsurvey")
            .save_file()
        else {
            return;
        };
        match serde_json::to_string(&self.survey)
            .map_err(anyhow::Error::from)
            .and_then(|t| Ok(std::fs::write(&path, t)?))
        {
            Ok(()) => sh.toast(tr("Saved.")),
            Err(e) => sh.fail(trl("The survey could not be saved."), &e),
        }
    }

    fn open_survey(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        let Some(path) = rfd::FileDialog::new().add_filter(tr("Wi-Fi survey"), &["netsurvey", "json"]).pick_file()
        else {
            return;
        };
        match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|t| Ok(serde_json::from_str::<Survey>(&t)?))
        {
            Ok(s) => {
                self.remember();
                self.survey = s;
                self.ap = None;
                self.load_plan(ctx);
                self.changed();
            }
            Err(e) => sh.fail(trl("The survey could not be opened."), &e),
        }
    }

    /// The plan with the colours and points, as a PNG.
    fn export(&self, sh: &mut Shared) {
        let Some(path) =
            rfd::FileDialog::new().add_filter("PNG", &["png"]).set_file_name("wifi-heatmap.png").save_file()
        else {
            return;
        };
        match self.render(&path) {
            Ok(()) => sh.toast(tr("Saved.")),
            Err(e) => sh.fail(trl("The picture could not be saved."), &e),
        }
    }

    fn render(&self, path: &Path) -> anyhow::Result<()> {
        use image::{Rgba, RgbaImage, imageops};
        let base = unbase64(&self.survey.plan).and_then(|b| image::load_from_memory(&b).ok());
        let mut out: RgbaImage = match base {
            Some(img) => {
                let img = if img.width() > 2000 { img.resize(2000, 2000, imageops::FilterType::Triangle) } else { img };
                img.to_rgba8()
            }
            None => RgbaImage::from_pixel(1600, (1600.0 / self.aspect) as u32, Rgba([255, 255, 255, 255])),
        };
        let (w, h) = out.dimensions();
        let layer = self.overlay_image(320, 0.55);
        let small = RgbaImage::from_fn(layer.size[0] as u32, layer.size[1] as u32, |x, y| {
            let c = layer.pixels[y as usize * layer.size[0] + x as usize].to_srgba_unmultiplied();
            Rgba(c)
        });
        let big = imageops::resize(&small, w, h, imageops::FilterType::Triangle);
        imageops::overlay(&mut out, &big, 0, 0);
        // The points: a white ring around the measured colour.
        let r = (w.max(h) as f32 / 160.0).max(5.0);
        for pt in self.survey.points.iter().filter(|p| self.ap.is_none() || p.ap == self.ap) {
            let (cx, cy) = (pt.x * w as f32, pt.y * h as f32);
            let c = signal_color(pt.rssi as f32);
            let rr = r as i32 + 2;
            for dy in -rr..=rr {
                for dx in -rr..=rr {
                    let d = ((dx * dx + dy * dy) as f32).sqrt();
                    let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                    if d > r + 2.0 || x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                        continue;
                    }
                    let px = if d <= r { Rgba([c.r(), c.g(), c.b(), 255]) } else { Rgba([255, 255, 255, 255]) };
                    out.put_pixel(x as u32, y as u32, px);
                }
            }
        }
        out.save(path)?;
        Ok(())
    }
}

fn legend(ui: &mut Ui, p: &Palette, show: Show, range: (f32, f32)) {
    ui.horizontal_wrapped(|ui| {
        let stops: Vec<(f32, String)> = match show {
            Show::Signal => [-45, -60, -67, -75, -85].iter().map(|d| (*d as f32, format!("{d} dBm"))).collect(),
            Show::Rate if range.0 <= range.1 => (0..5)
                .map(|i| {
                    let v = range.1 - (range.1 - range.0) * i as f32 / 4.0;
                    (as_signal(v, show, range), format!("{v:.0} Mbit/s"))
                })
                .collect(),
            Show::Rate => Vec::new(),
        };
        for (v, label) in stops {
            let (r, _) = ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
            ui.painter().rect_filled(r, 3.0, signal_color(v));
            ui.label(RichText::new(label).color(p.weak).size(12.5));
            ui.add_space(6.0);
        }
        if show == Show::Signal {
            ui.label(RichText::new(tr("−67 dBm or better is enough for calls and video.")).color(p.weak).size(12.5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(unbase64(&base64(data)).unwrap(), data);
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
    }

    #[test]
    fn colours_and_interpolation() {
        assert_eq!(signal_color(-40.0), Color32::from_rgb(22, 163, 74));
        assert_eq!(signal_color(-95.0), Color32::from_rgb(220, 38, 38));
        // Between two samples, the value is between theirs; far away, unsure.
        let f = field(&[(0.1, 0.5, -50.0), (0.9, 0.5, -80.0)], 10, 1, 1.0);
        assert!((f[0].0 - -50.0).abs() < 3.0 && (f[9].0 - -80.0).abs() < 3.0);
        assert!(f[5].0 < -50.0 && f[5].0 > -80.0);
        assert!(f[0].1 > 0.9);
        let far = field(&[(0.0, 0.0, -50.0)], 4, 4, 1.0);
        assert!(far[15].1 < 0.1);
    }
}
