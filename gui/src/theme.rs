//! Look and feel: palettes, fonts, egui style, and small reusable widgets.
//! The same design as Deleted Files Recovery.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, InnerResponse, Margin, Response,
    RichText, Sense, Shadow, Stroke, TextStyle, Theme, Ui, Vec2, Visuals,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Accent {
    Blue,
    Violet,
    Emerald,
    Orange,
    Rose,
}

impl Accent {
    pub const ALL: [Accent; 5] = [Accent::Blue, Accent::Violet, Accent::Emerald, Accent::Orange, Accent::Rose];

    pub fn color(self) -> Color32 {
        match self {
            Accent::Blue => Color32::from_rgb(59, 130, 246),
            Accent::Violet => Color32::from_rgb(139, 92, 246),
            Accent::Emerald => Color32::from_rgb(16, 185, 129),
            Accent::Orange => Color32::from_rgb(249, 115, 22),
            Accent::Rose => Color32::from_rgb(244, 63, 94),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Accent::Blue => "Blue",
            Accent::Violet => "Violet",
            Accent::Emerald => "Emerald",
            Accent::Orange => "Orange",
            Accent::Rose => "Rose",
        }
    }
}

/// Colours for the current theme.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub card_alt: Color32,
    pub border: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub deep: Color32,
    /// The top bar of the Midnight theme.
    pub header: Option<Color32>,
}

impl Palette {
    /// `midnight` only applies to the dark palette.
    pub fn new(dark: bool, midnight: bool, accent: Accent) -> Self {
        if dark && midnight {
            // Near-black surfaces under a deep blue top bar.
            Self {
                dark,
                bg: Color32::from_rgb(11, 12, 14),
                sidebar: Color32::from_rgb(17, 18, 21),
                card: Color32::from_rgb(22, 23, 27),
                card_alt: Color32::from_rgb(30, 32, 37),
                border: Color32::from_rgb(42, 45, 52),
                text: Color32::from_rgb(236, 238, 242),
                weak: Color32::from_rgb(148, 153, 165),
                accent: accent.color(),
                success: Color32::from_rgb(34, 197, 94),
                warning: Color32::from_rgb(245, 158, 11),
                danger: Color32::from_rgb(239, 68, 68),
                deep: Color32::from_rgb(167, 139, 250),
                header: Some(Color32::from_rgb(14, 40, 120)),
            }
        } else if dark {
            Self {
                dark,
                bg: Color32::from_rgb(15, 17, 23),
                sidebar: Color32::from_rgb(19, 22, 30),
                card: Color32::from_rgb(24, 28, 38),
                card_alt: Color32::from_rgb(31, 36, 48),
                border: Color32::from_rgb(41, 47, 62),
                text: Color32::from_rgb(230, 233, 239),
                weak: Color32::from_rgb(139, 147, 167),
                accent: accent.color(),
                success: Color32::from_rgb(34, 197, 94),
                warning: Color32::from_rgb(245, 158, 11),
                danger: Color32::from_rgb(239, 68, 68),
                deep: Color32::from_rgb(167, 139, 250),
                header: None,
            }
        } else {
            Self {
                dark,
                bg: Color32::from_rgb(244, 246, 250),
                sidebar: Color32::from_rgb(255, 255, 255),
                card: Color32::from_rgb(255, 255, 255),
                card_alt: Color32::from_rgb(241, 244, 249),
                border: Color32::from_rgb(226, 231, 239),
                text: Color32::from_rgb(17, 24, 39),
                weak: Color32::from_rgb(100, 110, 128),
                accent: accent.color(),
                success: Color32::from_rgb(22, 163, 74),
                warning: Color32::from_rgb(217, 119, 6),
                danger: Color32::from_rgb(220, 38, 38),
                deep: Color32::from_rgb(124, 58, 237),
                header: None,
            }
        }
    }

    /// A soft background tint of `c` that works on cards.
    pub fn tint(&self, c: Color32) -> Color32 {
        c.gamma_multiply(if self.dark { 0.16 } else { 0.10 })
    }
}

pub const SEMIBOLD: &str = "semibold";

fn semibold_family() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

/// System UI font (Segoe UI on Windows, SF on macOS) and Phosphor icons.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let candidates: &[(&str, &str)] = if cfg!(windows) {
        &[("system", r"C:\Windows\Fonts\segoeui.ttf"), ("system-semibold", r"C:\Windows\Fonts\seguisb.ttf")]
    } else if cfg!(target_os = "macos") {
        &[("system", "/System/Library/Fonts/SFNS.ttf")]
    } else {
        &[]
    };
    let mut loaded = Vec::new();
    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert((*name).to_string(), Arc::new(FontData::from_owned(bytes)));
            loaded.push(*name);
        }
    }
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    if loaded.contains(&"system") {
        proportional.insert(0, "system".into());
    }
    let mut semibold = proportional.clone();
    if loaded.contains(&"system-semibold") {
        semibold.insert(0, "system-semibold".into());
    }
    // Icons are inserted right after the text font of each family.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    semibold.insert(semibold.len().min(1), "phosphor".into());
    fonts.families.insert(semibold_family(), semibold);
    ctx.set_fonts(fonts);
}

pub fn apply_style(ctx: &egui::Context, accent: Accent, midnight: bool) {
    for theme in [Theme::Dark, Theme::Light] {
        let pal = Palette::new(theme == Theme::Dark, midnight, accent);
        ctx.style_mut_of(theme, |s| {
            s.visuals = visuals(&pal);
            s.spacing.item_spacing = Vec2::new(8.0, 8.0);
            s.spacing.button_padding = Vec2::new(12.0, 6.0);
            s.spacing.interact_size.y = 30.0;
            s.spacing.window_margin = Margin::same(16);
            s.text_styles = [
                (TextStyle::Heading, FontId::new(24.0, semibold_family())),
                (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(14.5, FontFamily::Proportional)),
                (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
            ]
            .into();
        });
    }
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    let r = CornerRadius::same(8);
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.window_shadow =
        Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) };
    v.popup_shadow =
        Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if p.dark { 90 } else { 30 }) };
    v.menu_corner_radius = r;
    v.extreme_bg_color = if p.header.is_some() {
        Color32::from_rgb(6, 7, 8)
    } else if p.dark {
        Color32::from_rgb(12, 14, 19)
    } else {
        Color32::WHITE
    };
    v.faint_bg_color = p.card_alt;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = blend(p.card, p.accent, if p.dark { 0.38 } else { 0.22 });
    v.selection.stroke = Stroke::new(1.0, p.text);
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.weak);
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.striped = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_fill = p.card_alt;
    w.inactive.weak_bg_fill = p.card_alt;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_fill = p.card_alt;
    w.hovered.weak_bg_fill = if p.header.is_some() {
        Color32::from_rgb(38, 40, 46)
    } else if p.dark {
        Color32::from_rgb(40, 46, 60)
    } else {
        Color32::from_rgb(232, 236, 244)
    };
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.7));
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.active.bg_fill = p.accent.gamma_multiply(0.35);
    w.active.weak_bg_fill = p.accent.gamma_multiply(0.35);
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.open = w.active;
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = r;
        s.expansion = 0.0;
    }
    v
}

/// Opaque mix of `a` towards `b` by `t`.
fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

// ---------------------------------------------------------------------------
// Widgets

pub fn semibold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(semibold_family()).size(size)
}

pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(18))
        .show(ui, add)
}

pub fn page_title(ui: &mut Ui, p: &Palette, title: &str, subtitle: &str) {
    ui.label(semibold(title, 26.0).color(p.text));
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).color(p.weak).size(15.0));
    }
    ui.add_space(14.0);
}

pub fn section_title(ui: &mut Ui, p: &Palette, icon: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).size(18.0).color(p.accent));
        ui.label(semibold(title, 16.5).color(p.text));
    });
    ui.add_space(4.0);
}

pub fn primary_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    let fill = if enabled { p.accent } else { p.accent.gamma_multiply(0.4) };
    ui.add_enabled(
        enabled,
        egui::Button::new(semibold(text, 15.0).color(Color32::WHITE))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(10))
            .min_size(Vec2::new(0.0, 42.0)),
    )
}

pub fn secondary_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).size(14.5))
            .corner_radius(CornerRadius::same(10))
            .min_size(Vec2::new(0.0, 36.0)),
    )
}

pub fn danger_button(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.add(
        egui::Button::new(semibold(text, 14.5).color(p.danger))
            .fill(p.tint(p.danger))
            .stroke(Stroke::new(1.0, p.danger.gamma_multiply(0.5)))
            .corner_radius(CornerRadius::same(10))
            .min_size(Vec2::new(0.0, 38.0)),
    )
}

/// Small rounded status label.
pub fn pill(ui: &mut Ui, p: &Palette, text: &str, color: Color32) -> Response {
    Frame::new()
        .fill(p.tint(color))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(9, 2))
        .show(ui, |ui| ui.label(RichText::new(text).size(12.0).color(color).strong()))
        .response
}

/// A clickable card; highlighted when `selected` or hovered.
pub fn selectable_card<R>(
    ui: &mut Ui,
    p: &Palette,
    id: egui::Id,
    selected: bool,
    enabled: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> Response {
    let hovered = enabled && ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let (fill, stroke) = if selected {
        (p.tint(p.accent), Stroke::new(1.5, p.accent))
    } else if hovered {
        (p.card_alt, Stroke::new(1.0, p.accent.gamma_multiply(0.6)))
    } else {
        (p.card, Stroke::new(1.0, p.border))
    };
    let inner = Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            if !enabled {
                ui.disable();
            }
            add(ui)
        });
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(inner.response.rect, id, sense);
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A big label/value pair for statistics.
pub fn stat(ui: &mut Ui, p: &Palette, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(12.5).color(p.weak));
        ui.label(semibold(value, 18.0).color(p.text));
    });
}

/// A notice box with an icon, e.g. a warning.
pub fn notice(ui: &mut Ui, p: &Palette, color: Color32, icon: &str, text: &str) {
    Frame::new()
        .fill(p.tint(color))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).size(18.0).color(color));
                paragraph(ui, text, 14.5, p.text);
            });
        });
}

/// Text wrapped to the available width.
pub fn paragraph(ui: &mut Ui, text: &str, size: f32, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).size(size).color(color)).wrap());
}

/// "icon  text", for buttons and links.
pub fn icon_label(icon: &str, text: &str) -> String {
    format!("{icon}  {text}")
}

/// A label/value row in a details grid, with a copy button for the value.
/// Returns true when the value was copied.
pub fn info_row(ui: &mut Ui, p: &Palette, label: &str, value: &str, copy: bool) -> bool {
    let mut copied = false;
    ui.label(RichText::new(label).color(p.weak).size(13.5));
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(RichText::new(value).color(p.text).size(14.0)).wrap());
        if copy && !value.is_empty() && value != "—" {
            let b = ui
                .add(
                    egui::Button::new(RichText::new(egui_phosphor::regular::COPY).size(13.0).color(p.weak))
                        .frame(false),
                )
                .on_hover_text("Copy");
            if b.clicked() {
                ui.ctx().copy_text(value.to_string());
                copied = true;
            }
        }
    });
    ui.end_row();
    copied
}

/// A row of pill-shaped tabs; returns the clicked one.
pub fn tabs<T: Copy + PartialEq>(ui: &mut Ui, p: &Palette, current: &mut T, items: &[(T, &str, &str)]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (value, glyph, label) in items {
            let selected = *current == *value;
            let text = RichText::new(icon_label(glyph, label)).size(14.0).color(if selected { p.text } else { p.weak });
            let b = egui::Button::new(text)
                .fill(if selected { p.tint(p.accent) } else { Color32::TRANSPARENT })
                .stroke(if selected { Stroke::new(1.0, p.accent.gamma_multiply(0.6)) } else { Stroke::NONE })
                .corner_radius(CornerRadius::same(9))
                .min_size(Vec2::new(0.0, 32.0));
            if ui.add(b).clicked() {
                *current = *value;
            }
        }
    });
}

/// Four bars showing a 0–100 signal quality.
pub fn signal_bars(ui: &mut Ui, p: &Palette, quality: u8, height: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(height * 1.2, height), Sense::hover());
    let color = match quality {
        60.. => p.success,
        40..=59 => p.warning,
        _ => p.danger,
    };
    let lit = match quality {
        75.. => 4,
        50..=74 => 3,
        25..=49 => 2,
        1..=24 => 1,
        _ => 0,
    };
    let w = rect.width() / 4.0;
    for i in 0..4 {
        let h = rect.height() * (i as f32 + 1.0) / 4.0;
        let r = egui::Rect::from_min_max(
            egui::pos2(rect.left() + i as f32 * w + 1.0, rect.bottom() - h),
            egui::pos2(rect.left() + (i as f32 + 1.0) * w - 1.0, rect.bottom()),
        );
        ui.painter().rect_filled(r, CornerRadius::same(2), if i < lit { color } else { p.border });
    }
    resp
}

/// A small line chart of recent values (newest last), filled underneath.
pub fn sparkline(ui: &mut Ui, values: &[f64], color: Color32, size: Vec2, max: Option<f64>) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if values.len() < 2 {
        return;
    }
    let top = max.unwrap_or_else(|| values.iter().copied().fold(0.0, f64::max)).max(1e-9);
    let n = values.len() - 1;
    let points: Vec<egui::Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            egui::pos2(
                rect.left() + rect.width() * i as f32 / n as f32,
                rect.bottom() - rect.height() * (v / top).clamp(0.0, 1.0) as f32,
            )
        })
        .collect();
    let mut fill = points.clone();
    fill.push(egui::pos2(rect.right(), rect.bottom()));
    fill.push(egui::pos2(rect.left(), rect.bottom()));
    // A convex fill would be wrong for a jagged line: fill with thin strips.
    for w in points.windows(2) {
        ui.painter().add(egui::Shape::convex_polygon(
            vec![w[0], w[1], egui::pos2(w[1].x, rect.bottom()), egui::pos2(w[0].x, rect.bottom())],
            color.gamma_multiply(0.15),
            Stroke::NONE,
        ));
    }
    ui.painter().add(egui::Shape::line(points, Stroke::new(1.8, color)));
}

/// A QR code drawn with rectangles, `size` points wide, on white.
pub fn qr_code(ui: &mut Ui, modules: &[Vec<bool>], size: f32) {
    let n = modules.len() as f32 + 8.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), Color32::WHITE);
    let m = size / n;
    for (y, row) in modules.iter().enumerate() {
        for (x, dark) in row.iter().enumerate() {
            if *dark {
                let min = rect.min + Vec2::new((x as f32 + 4.0) * m, (y as f32 + 4.0) * m);
                painter.rect_filled(egui::Rect::from_min_size(min, Vec2::splat(m + 0.3)), 0.0, Color32::BLACK);
            }
        }
    }
}

/// A big icon in a tinted circle, for page headers and empty states.
pub fn icon_badge(ui: &mut Ui, p: &Palette, glyph: &str, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, p.tint(color));
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, glyph, FontId::proportional(size * 0.5), color);
}

/// A label/value tile, for the overview.
pub fn tile(ui: &mut Ui, p: &Palette, glyph: &str, label: &str, value: &str, sub: &str) {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(glyph).size(15.0).color(p.accent));
                ui.label(RichText::new(label).size(12.5).color(p.weak));
            });
            ui.add(egui::Label::new(semibold(value, 17.0).color(p.text)).truncate());
            if !sub.is_empty() {
                ui.add(egui::Label::new(RichText::new(sub).size(12.0).color(p.weak)).truncate());
            }
        });
}
