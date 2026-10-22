//! The CLI guide on screen: in a tab of its own in Console, docked beside
//! (or below) the console so that both can be read at once, or in a window
//! of its own that can be moved to a second screen.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, ScrollArea, Stroke, Ui, Vec2,
    text::{LayoutJob, TextFormat},
};
use egui_phosphor::regular as icon;
use serde::{Deserialize, Serialize};

use crate::guide::{self, Block, Book, Command};
use crate::i18n::{tr, trf, trl};
use crate::theme::{self, Palette, icon_label};

/// Where the docked guide sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Side {
    Left,
    #[default]
    Right,
    Bottom,
}

/// How the guide is set up: kept between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuideSettings {
    /// Beside the console instead of in its own tab.
    pub docked: bool,
    pub side: Side,
    /// Size of the docked panel (width at the sides, height below).
    pub width: f32,
    pub height: f32,
    /// Docked, but folded away to a narrow strip.
    pub collapsed: bool,
    /// Docked, and filling the page.
    pub maximized: bool,
    /// Text size, 1.0 is normal.
    pub zoom: f32,
    /// The window of its own stays above other windows.
    pub on_top: bool,
    /// What is shown: the book, and its topic (0 is "About this book").
    pub book: usize,
    pub topic: usize,
}

impl Default for GuideSettings {
    fn default() -> Self {
        Self {
            docked: false,
            side: Side::Right,
            width: 460.0,
            height: 320.0,
            collapsed: false,
            maximized: false,
            zoom: 1.0,
            on_top: false,
            book: 0,
            topic: 0,
        }
    }
}

/// What the guide shows while the app runs.
#[derive(Default)]
pub struct GuideState {
    /// Open in a window of its own (not kept: the app starts without it).
    pub popped: bool,
    search: String,
    all_books: bool,
    /// The last text copied and when: its button shows a check mark for a moment.
    copied: Option<(String, Instant)>,
    /// What the window of its own was last told about staying on top.
    on_top_sent: Option<bool>,
}

impl GuideState {
    /// Development aid: a search typed in.
    #[cfg(debug_assertions)]
    pub fn demo_search(&mut self, query: &str, all_books: bool) {
        self.search = query.to_string();
        self.all_books = all_books;
    }
}

/// Where the guide is being drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Tab,
    Dock,
    Window,
}

/// What the guide asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Show Console → CLI guide.
    OpenTab,
    /// Show Console, so that the docked guide is beside it.
    OpenConsole,
}

const MAX_HITS: usize = 150;

// ---------------------------------------------------------------- places

/// The guide's own tab in Console.
pub fn tab(ui: &mut Ui, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings) -> Option<Action> {
    if g.popped {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("The guide is open in its own window"), 16.0).color(p.text));
            ui.add_space(4.0);
            theme::paragraph(
                ui,
                trl(
                    "Move that window to another screen to read it beside the console. Close it, or bring it back here.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            if theme::primary_button(ui, p, &icon_label(icon::ARROWS_IN_SIMPLE, "Bring it back here"), true).clicked() {
                g.popped = false;
            }
        });
        return None;
    }
    show(ui, p, g, cfg, Where::Tab)
}

/// The panel beside (or below) the console. Call it before the central panel.
pub fn dock(ui: &mut Ui, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings) -> Option<Action> {
    let free = ui.available_rect_before_wrap();
    let horizontal = cfg.side != Side::Bottom;
    let room = if horizontal { free.width() } else { free.height() };
    let frame = Frame::new().fill(p.bg).stroke(Stroke::new(1.0, p.border)).inner_margin(Margin::symmetric(14, 12));
    let panel = |id: &'static str| match cfg.side {
        Side::Left => egui::Panel::left(id),
        Side::Right => egui::Panel::right(id),
        Side::Bottom => egui::Panel::bottom(id),
    };
    if cfg.collapsed {
        let mut open = false;
        panel("guide-dock-folded")
            .exact_size(if horizontal { 46.0 } else { 40.0 })
            .resizable(false)
            .frame(frame.inner_margin(Margin::symmetric(6, 8)))
            .show(ui, |ui| {
                let (glyph, hint) = match cfg.side {
                    Side::Left => (icon::CARET_RIGHT, tr("Show the guide")),
                    Side::Right => (icon::CARET_LEFT, tr("Show the guide")),
                    Side::Bottom => (icon::CARET_UP, tr("Show the guide")),
                };
                let content = |ui: &mut Ui| {
                    ui.label(RichText::new(icon::BOOK_OPEN).size(20.0).color(p.accent));
                    if ui
                        .add(egui::Button::new(RichText::new(glyph).size(16.0)).frame(false))
                        .on_hover_text(hint)
                        .clicked()
                    {
                        open = true;
                    }
                    if !horizontal {
                        ui.label(RichText::new(tr("CLI guide")).color(p.weak));
                    }
                };
                if horizontal {
                    ui.vertical_centered(content);
                } else {
                    ui.horizontal(content);
                }
            });
        if open {
            cfg.collapsed = false;
        }
        return None;
    }
    let min = if horizontal { 320.0 } else { 170.0 };
    let max = (room * 0.8).max(min);
    let wanted = if horizontal { cfg.width } else { cfg.height };
    let panel = if cfg.maximized {
        panel("guide-dock-full").exact_size((room - if horizontal { 150.0 } else { 90.0 }).max(min)).resizable(false)
    } else {
        panel("guide-dock").default_size(wanted.clamp(min, max)).size_range(min..=max).resizable(true)
    };
    let shown = panel.frame(frame).show(ui, |ui| show(ui, p, g, cfg, Where::Dock));
    // The size the person dragged it to is kept.
    let size = if horizontal { shown.response.rect.width() } else { shown.response.rect.height() };
    if !cfg.maximized && size > 1.0 {
        let slot = if horizontal { &mut cfg.width } else { &mut cfg.height };
        if (*slot - size).abs() > 1.0 {
            *slot = size;
        }
    }
    shown.inner
}

/// The window of its own. Call it every frame while `popped` is set.
pub fn window(ctx: &egui::Context, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings) -> Option<Action> {
    let mut builder = egui::ViewportBuilder::default()
        .with_title(format!("{} — Network Manager", tr("CLI guide")))
        .with_inner_size([640.0, 820.0])
        .with_min_inner_size([360.0, 280.0]);
    if cfg.on_top {
        builder = builder.with_always_on_top();
    }
    let mut action = None;
    let mut closed = false;
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("cli-guide-window"), builder, |ui, _| {
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            closed = true;
        }
        // Stays on top (or not) as the person chooses, also while it is open.
        if g.on_top_sent.is_some_and(|sent| sent != cfg.on_top) {
            let level = if cfg.on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
        }
        g.on_top_sent = Some(cfg.on_top);
        egui::CentralPanel::default().frame(Frame::new().fill(p.bg).inner_margin(Margin::symmetric(16, 12))).show(
            ui,
            |ui| {
                action = show(ui, p, g, cfg, Where::Window);
            },
        );
    });
    if closed {
        g.popped = false;
        g.on_top_sent = None;
    }
    action
}

// ---------------------------------------------------------------- the guide

/// The guide: the header, then the topics of a book or the search results.
pub fn show(ui: &mut Ui, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings, at: Where) -> Option<Action> {
    let books = guide::books();
    cfg.book = cfg.book.min(books.len().saturating_sub(1));
    cfg.zoom = cfg.zoom.clamp(0.8, 1.8);
    let z = cfg.zoom;
    let action = header(ui, p, g, cfg, at);
    ui.add_space(6.0);
    if g.search.trim().is_empty() {
        reading(ui, p, g, cfg, z);
    } else {
        results(ui, p, g, cfg, z);
    }
    action
}

fn header(ui: &mut Ui, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings, at: Where) -> Option<Action> {
    let books = guide::books();
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(icon::BOOK_OPEN).size(18.0).color(p.accent));
        let before = cfg.book;
        egui::ComboBox::from_id_salt(("guide-book", at as u8))
            .selected_text(books[cfg.book].title.clone())
            .width(210.0)
            .show_ui(ui, |ui| {
                for (i, b) in books.iter().enumerate() {
                    ui.selectable_value(&mut cfg.book, i, b.title.clone());
                }
            });
        if cfg.book != before {
            cfg.topic = 0;
        }
        theme::field(
            ui,
            egui::TextEdit::singleline(&mut g.search).hint_text(tr("Search the commands…")).desired_width(210.0),
        );
        if !g.search.is_empty() && ui.add(egui::Button::new(icon::X).frame(false)).on_hover_text(tr("Clear")).clicked()
        {
            g.search.clear();
        }
        if !g.search.trim().is_empty() {
            ui.checkbox(&mut g.all_books, tr("All books"));
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let tool = |ui: &mut Ui, glyph: &str, hint: &str| {
            ui.add(egui::Button::new(RichText::new(glyph).size(16.0)).frame(false)).on_hover_text(hint).clicked()
        };
        if at == Where::Dock {
            if tool(ui, icon::MINUS, tr("Fold the panel away")) {
                cfg.collapsed = true;
            }
            let (glyph, hint) = if cfg.maximized {
                (icon::ARROWS_IN_SIMPLE, tr("Back to the normal size"))
            } else {
                (icon::ARROWS_OUT_SIMPLE, tr("Make the panel fill the page"))
            };
            if tool(ui, glyph, hint) {
                cfg.maximized = !cfg.maximized;
            }
            if tool(ui, icon::ARROW_SQUARE_OUT, tr("Open in its own window: move it to another screen")) {
                g.popped = true;
            }
        }
        if let Some(a) = layout_menu(ui, g, cfg, at) {
            action = Some(a);
        }
        if at == Where::Window {
            let hint = if cfg.on_top { tr("Stop keeping the window on top") } else { tr("Keep the window on top") };
            if ui
                .add(
                    egui::Button::new(RichText::new(icon::PUSH_PIN).size(16.0).color(if cfg.on_top {
                        p.accent
                    } else {
                        p.weak
                    }))
                    .frame(false),
                )
                .on_hover_text(hint)
                .clicked()
            {
                cfg.on_top = !cfg.on_top;
            }
        }
        ui.add_space(6.0);
        if ui
            .add(egui::Button::new(RichText::new("A−").size(14.0)).frame(false))
            .on_hover_text(tr("Smaller text"))
            .clicked()
        {
            cfg.zoom = (cfg.zoom - 0.1).max(0.8);
        }
        if ui
            .add(egui::Button::new(RichText::new("A+").size(16.0)).frame(false))
            .on_hover_text(tr("Larger text"))
            .clicked()
        {
            cfg.zoom = (cfg.zoom + 0.1).min(1.8);
        }
        if at == Where::Dock {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if tool(ui, icon::X, tr("Close the panel: the guide stays in its tab")) {
                    cfg.docked = false;
                    action = Some(Action::OpenTab);
                }
            });
        }
    });
    action
}

/// The menu that moves the guide: its own tab, beside the console, a window.
fn layout_menu(ui: &mut Ui, g: &mut GuideState, cfg: &mut GuideSettings, at: Where) -> Option<Action> {
    let mut action = None;
    ui.menu_button(icon_label(icon::SQUARE_HALF, "Layout"), |ui| {
        ui.set_min_width(260.0);
        if ui.selectable_label(at == Where::Tab, tr("In its own tab")).clicked() {
            cfg.docked = false;
            g.popped = false;
            action = Some(Action::OpenTab);
            ui.close();
        }
        for (side, label) in [
            (Side::Right, tr("Beside the console, on the right")),
            (Side::Left, tr("Beside the console, on the left")),
            (Side::Bottom, tr("Below the console")),
        ] {
            if ui.selectable_label(at == Where::Dock && cfg.side == side, label).clicked() {
                cfg.docked = true;
                cfg.side = side;
                cfg.collapsed = false;
                g.popped = false;
                action = Some(Action::OpenConsole);
                ui.close();
            }
        }
        if ui.selectable_label(at == Where::Window, tr("In its own window")).clicked() {
            g.popped = true;
            ui.close();
        }
    });
    action
}

/// A book's topics and the one chosen.
fn reading(ui: &mut Ui, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings, z: f32) {
    let book = &guide::books()[cfg.book];
    cfg.topic = cfg.topic.min(book.topics.len());
    if ui.available_width() >= 700.0 {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(210.0, ui.available_height()), Layout::top_down(Align::Min), |ui| {
                topic_list(ui, p, cfg, book, z)
            });
            ui.add_space(6.0);
            ui.vertical(|ui| content(ui, p, g, cfg, book, z));
        });
    } else {
        topic_bar(ui, cfg, book);
        ui.add_space(6.0);
        content(ui, p, g, cfg, book, z);
    }
}

/// The list of topics on the left (wide places).
fn topic_list(ui: &mut Ui, p: &Palette, cfg: &mut GuideSettings, book: &Book, z: f32) {
    ScrollArea::vertical().id_salt(("guide-topics", cfg.book)).auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        let mut row = |ui: &mut Ui, index: usize, title: &str| {
            let selected = cfg.topic == index;
            let text = RichText::new(title).size(14.0 * z).color(if selected { p.text } else { p.weak });
            if ui.selectable_label(selected, text).clicked() {
                cfg.topic = index;
            }
        };
        row(ui, 0, tr("About this book"));
        for (i, t) in book.topics.iter().enumerate() {
            row(ui, i + 1, &t.title);
        }
    });
}

/// The topic picker on top (narrow places): previous, a list, next.
fn topic_bar(ui: &mut Ui, cfg: &mut GuideSettings, book: &Book) {
    ui.horizontal(|ui| {
        if ui.add_enabled(cfg.topic > 0, egui::Button::new(icon::CARET_LEFT)).on_hover_text(tr("Previous")).clicked() {
            cfg.topic -= 1;
        }
        let shown =
            if cfg.topic == 0 { tr("About this book").to_string() } else { book.topics[cfg.topic - 1].title.clone() };
        egui::ComboBox::from_id_salt(("guide-topic", cfg.book))
            .selected_text(shown)
            .width((ui.available_width() - 44.0).max(120.0))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.topic, 0, tr("About this book"));
                for (i, t) in book.topics.iter().enumerate() {
                    ui.selectable_value(&mut cfg.topic, i + 1, t.title.clone());
                }
            });
        if ui
            .add_enabled(cfg.topic < book.topics.len(), egui::Button::new(icon::CARET_RIGHT))
            .on_hover_text(tr("Next"))
            .clicked()
        {
            cfg.topic += 1;
        }
    });
}

/// The chosen topic (or the book's own page), with its pager at the end.
fn content(ui: &mut Ui, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings, book: &Book, z: f32) {
    let topic = cfg.topic;
    ScrollArea::vertical().id_salt(("guide-content", cfg.book, topic)).auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        if topic == 0 {
            about(ui, p, cfg, book, z);
        } else {
            let t = &book.topics[topic - 1];
            ui.label(theme::semibold(&t.title, 22.0 * z).color(p.text));
            for (s, section) in t.sections.iter().enumerate() {
                if !section.title.is_empty() {
                    ui.add_space(8.0);
                    ui.label(theme::semibold(&section.title, 16.5 * z).color(p.accent));
                }
                for (b, block) in section.blocks.iter().enumerate() {
                    show_block(ui, p, g, block, z, &format!("{}/{topic}/{s}/{b}", book.id));
                }
            }
        }
        pager(ui, cfg, book);
        ui.add_space(24.0);
    });
}

/// The book's own page: what it is, its topics, what it was checked against.
fn about(ui: &mut Ui, p: &Palette, cfg: &mut GuideSettings, book: &Book, z: f32) {
    ui.label(theme::semibold(&book.title, 22.0 * z).color(p.text));
    ui.label(RichText::new(&book.intro).size(14.5 * z).color(p.text));
    ui.add_space(6.0);
    ui.label(theme::semibold(tr("Topics"), 16.5 * z).color(p.accent));
    for (i, t) in book.topics.iter().enumerate() {
        if ui.link(RichText::new(format!("{}. {}", i + 1, t.title)).size(14.5 * z)).clicked() {
            cfg.topic = i + 1;
        }
    }
    ui.add_space(8.0);
    theme::notice(
        ui,
        p,
        p.warning,
        icon::WARNING,
        trl(
            "Commands depend on the model and the software version. Type ? on the switch to see what it accepts, and check Cisco's, Juniper's or the maker's documentation before you change a network that people use.",
        ),
    );
    if !book.sources.is_empty() {
        ui.add_space(6.0);
        ui.label(theme::semibold(tr("Checked against"), 16.5 * z).color(p.accent));
        for (name, url) in &book.sources {
            ui.hyperlink_to(RichText::new(name).size(13.5 * z), url);
        }
    }
}

/// Previous and next topic, at the end of a topic.
fn pager(ui: &mut Ui, cfg: &mut GuideSettings, book: &Book) {
    ui.add_space(10.0);
    ui.separator();
    ui.horizontal(|ui| {
        if cfg.topic > 0 {
            let title = if cfg.topic == 1 {
                tr("About this book").to_string()
            } else {
                book.topics[cfg.topic - 2].title.clone()
            };
            if ui.button(format!("{}  {title}", icon::CARET_LEFT)).clicked() {
                cfg.topic -= 1;
            }
        }
        if cfg.topic < book.topics.len() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(format!("{}  {}", book.topics[cfg.topic].title, icon::CARET_RIGHT)).clicked() {
                    cfg.topic += 1;
                }
            });
        }
    });
}

/// What the search found, in this book or in all of them.
fn results(ui: &mut Ui, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings, z: f32) {
    let books = guide::books();
    let words = guide::words(&g.search);
    let which: Vec<usize> = if g.all_books { (0..books.len()).collect() } else { vec![cfg.book] };
    let mut shown = 0;
    let mut jump: Option<(usize, usize)> = None;
    ScrollArea::vertical().id_salt("guide-results").auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        for bi in which {
            let book = &books[bi];
            let mut last = (usize::MAX, usize::MAX);
            for hit in book.search(&words) {
                if shown >= MAX_HITS {
                    break;
                }
                shown += 1;
                let topic = &book.topics[hit.topic];
                let section = &topic.sections[hit.section];
                if (hit.topic, hit.section) != last || shown == 1 {
                    last = (hit.topic, hit.section);
                    let place = if section.title.is_empty() {
                        format!("{} › {}", book.title, topic.title)
                    } else {
                        format!("{} › {} › {}", book.title, topic.title, section.title)
                    };
                    if ui
                        .link(RichText::new(place).size(12.5 * z).color(p.accent))
                        .on_hover_text(tr("Open this topic"))
                        .clicked()
                    {
                        jump = Some((bi, hit.topic + 1));
                    }
                }
                show_block(
                    ui,
                    p,
                    g,
                    &section.blocks[hit.block],
                    z,
                    &format!("{}/{}/{}/{}", book.id, hit.topic, hit.section, hit.block),
                );
            }
        }
        if shown == 0 {
            ui.label(RichText::new(trf("Nothing found for “{query}”.", &[("query", &g.search.trim())])).color(p.weak));
        } else if shown >= MAX_HITS {
            ui.label(
                RichText::new(tr("Only the first results are shown: add a word to narrow the search.")).color(p.weak),
            );
        }
    });
    if let Some((book, topic)) = jump {
        cfg.book = book;
        cfg.topic = topic;
        g.search.clear();
    }
}

// ---------------------------------------------------------------- blocks

fn code_background(p: &Palette) -> Color32 {
    if p.dark { Color32::from_rgb(7, 8, 10) } else { Color32::from_rgb(238, 241, 246) }
}

fn code_text(p: &Palette) -> Color32 {
    if p.dark { Color32::from_rgb(214, 224, 240) } else { Color32::from_rgb(28, 40, 66) }
}

/// Text with `code` and **bold** parts, wrapped to `width`.
fn inline_job(p: &Palette, text: &str, size: f32, color: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    let body = FontId::proportional(size);
    let strong = FontId::new(size, theme::semibold_family());
    let mono = FontId::monospace(size * 0.92);
    let background = if p.dark { Color32::from_white_alpha(16) } else { Color32::from_black_alpha(14) };
    let (mut in_code, mut in_bold) = (false, false);
    let mut rest = text;
    while !rest.is_empty() {
        let tick = rest.find('`');
        let bold = if in_code { None } else { rest.find("**") };
        let cut = match (tick, bold) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => rest.len(),
        };
        let (piece, tail) = rest.split_at(cut);
        if !piece.is_empty() {
            let format = if in_code {
                TextFormat { font_id: mono.clone(), color: code_text(p), background, ..Default::default() }
            } else if in_bold {
                TextFormat { font_id: strong.clone(), color, ..Default::default() }
            } else {
                TextFormat { font_id: body.clone(), color, ..Default::default() }
            };
            job.append(piece, 0.0, format);
        }
        if let Some(t) = tail.strip_prefix('`') {
            in_code = !in_code;
            rest = t;
        } else if let Some(t) = tail.strip_prefix("**") {
            in_bold = !in_bold;
            rest = t;
        } else {
            rest = tail;
        }
    }
    job
}

/// The width of a text without its markup, in characters.
fn plain_len(text: &str) -> usize {
    text.replace("**", "").chars().filter(|c| *c != '`').count()
}

/// Wrapped to the room that is left: a row of widgets would otherwise
/// let its labels grow as wide as their text and push the page wider.
fn text(ui: &mut Ui, p: &Palette, s: &str, size: f32, color: Color32) {
    let job = inline_job(p, s, size, color, ui.available_width());
    ui.add(egui::Label::new(job).selectable(true).wrap());
}

fn show_block(ui: &mut Ui, p: &Palette, g: &mut GuideState, b: &Block, z: f32, key: &str) {
    match b {
        Block::Text(t) => text(ui, p, t, 14.5 * z, p.text),
        Block::Bullets(items) => {
            for it in items {
                ui.horizontal_top(|ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new("•").size(14.5 * z).color(p.accent));
                    text(ui, p, it, 14.5 * z, p.text);
                });
            }
        }
        Block::Steps(first, items) => {
            for (i, it) in items.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    ui.add_space(4.0);
                    ui.label(theme::semibold(format!("{}.", *first as usize + i), 14.5 * z).color(p.accent));
                    text(ui, p, it, 14.5 * z, p.text);
                });
            }
        }
        Block::Table(rows) => table(ui, p, rows, z, key),
        Block::Command(c) => command(ui, p, g, c, z, key),
        Block::Code(lines) => {
            Frame::new()
                .fill(code_background(p))
                .stroke(Stroke::new(1.0, p.border))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        copy_button(ui, p, g, &lines.join("\n"), key);
                    });
                    ScrollArea::horizontal().id_salt((key, "code")).auto_shrink([false, true]).show(ui, |ui| {
                        code_lines(ui, p, lines, z);
                    });
                });
        }
        Block::Warn(t) => callout(ui, p, p.warning, icon::WARNING, t, z),
        Block::Tip(t) => callout(ui, p, p.accent, icon::LIGHTBULB, t, z),
    }
}

fn code_lines(ui: &mut Ui, p: &Palette, lines: &[String], z: f32) {
    for l in lines {
        ui.add(
            egui::Label::new(
                RichText::new(if l.is_empty() { " " } else { l }).monospace().size(13.0 * z).color(code_text(p)),
            )
            .selectable(true)
            .extend(),
        );
    }
}

fn callout(ui: &mut Ui, p: &Palette, color: Color32, glyph: &str, s: &str, z: f32) {
    Frame::new()
        .fill(p.tint(color))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(glyph).size(16.0 * z).color(color));
                text(ui, p, s, 14.0 * z, p.text);
            });
        });
}

/// A command: where it is typed, the lines, what it does, an example answer.
fn command(ui: &mut Ui, p: &Palette, g: &mut GuideState, c: &Command, z: f32, key: &str) {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&c.prompt).monospace().size(11.5 * z).color(p.weak));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    copy_button(ui, p, g, &c.lines.join("\n"), key)
                });
            });
            Frame::new()
                .fill(code_background(p))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for l in &c.lines {
                        ui.add(
                            egui::Label::new(RichText::new(l).monospace().size(13.5 * z).color(code_text(p)))
                                .selectable(true)
                                .wrap(),
                        );
                    }
                });
            if !c.what.is_empty() {
                ui.add_space(2.0);
                text(ui, p, &c.what, 14.0 * z, p.text);
            }
            if !c.output.is_empty() {
                ui.add_space(2.0);
                Frame::new()
                    .fill(code_background(p))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::horizontal().id_salt((key, "out")).auto_shrink([false, true]).show(
                            ui,
                            |ui| {
                                code_lines(ui, p, &c.output, z * 0.95);
                            },
                        );
                    });
            }
        });
}

fn copy_button(ui: &mut Ui, p: &Palette, g: &mut GuideState, what: &str, key: &str) {
    let done = g.copied.as_ref().is_some_and(|(k, at)| k == key && at.elapsed() < Duration::from_millis(1400));
    let glyph = if done { icon::CHECK } else { icon::COPY };
    let r = ui
        .add(
            egui::Button::new(RichText::new(glyph).size(15.0).color(if done { p.success } else { p.weak }))
                .frame(false),
        )
        .on_hover_text(if done { tr("Copied.") } else { tr("Copy") });
    if r.clicked() {
        ui.ctx().copy_text(what.to_string());
        g.copied = Some((key.to_string(), Instant::now()));
    }
    if done {
        ui.ctx().request_repaint_after(Duration::from_millis(1500));
    }
}

/// A table whose columns share the width by their content, with wrapping
/// cells; a wide table in a narrow place scrolls sideways.
fn table(ui: &mut Ui, p: &Palette, rows: &[Vec<String>], z: f32, key: &str) {
    let cols = rows[0].len();
    let (pad, gap, min_col) = (10.0, 12.0, 96.0);
    let available = ui.available_width();
    let total = available.max(min_col * cols as f32 + gap * (cols as f32 - 1.0) + 2.0 * pad);
    let usable = total - gap * (cols as f32 - 1.0) - 2.0 * pad;
    let weights: Vec<f32> =
        (0..cols).map(|c| rows.iter().map(|r| plain_len(&r[c])).max().unwrap_or(1).clamp(6, 64) as f32).collect();
    let sum: f32 = weights.iter().sum();
    let widths: Vec<f32> = weights.iter().map(|w| (usable * w / sum).max(min_col * 0.55)).collect();
    ScrollArea::horizontal().id_salt((key, "table")).auto_shrink([false, true]).show(ui, |ui| {
        ui.set_min_width(total);
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, row) in rows.iter().enumerate() {
            let fill = if i == 0 {
                p.card_alt
            } else if i % 2 == 0 {
                p.card_alt.gamma_multiply(0.45)
            } else {
                Color32::TRANSPARENT
            };
            Frame::new()
                .fill(fill)
                .corner_radius(CornerRadius::same(if i == 0 { 6 } else { 0 }))
                .inner_margin(Margin::symmetric(pad as i8, 5))
                .show(ui, |ui| {
                    ui.set_width(total - 2.0 * pad);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for (c, cell) in row.iter().enumerate() {
                            ui.allocate_ui_with_layout(Vec2::new(widths[c], 1.0), Layout::top_down(Align::Min), |ui| {
                                ui.set_width(widths[c]);
                                if i == 0 {
                                    ui.label(theme::semibold(cell, 13.5 * z).color(p.weak));
                                } else {
                                    text(ui, p, cell, 13.5 * z, p.text);
                                }
                            });
                        }
                    });
                });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Accent;

    /// One frame of the guide in a space `width` wide.
    fn frame(ctx: &egui::Context, p: &Palette, g: &mut GuideState, cfg: &mut GuideSettings, width: f32, at: Where) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, 900.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            show(ui, p, g, cfg, at);
        });
        output.textures_delta.clear();
    }

    /// Every topic of every book can be drawn, in a narrow place (the docked
    /// panel) and a wide one (the tab), and so can the search.
    #[test]
    fn every_topic_can_be_drawn() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, crate::i18n::Lang::En);
        let p = Palette::new(true, true, Accent::Blue);
        let (mut g, mut cfg) = (GuideState::default(), GuideSettings::default());
        for (b, book) in guide::books().iter().enumerate() {
            for topic in 0..=book.topics.len() {
                for (width, at) in [(360.0, Where::Dock), (1200.0, Where::Tab)] {
                    cfg.book = b;
                    cfg.topic = topic;
                    frame(&ctx, &p, &mut g, &mut cfg, width, at);
                }
            }
        }
        for query in ["vlan", "show ip", "trunk allowed", "zzzz nothing", "PoE"] {
            for all in [false, true] {
                g.demo_search(query, all);
                frame(&ctx, &p, &mut g, &mut cfg, 500.0, Where::Window);
            }
        }
    }

    #[test]
    fn settings_survive_a_save_and_old_ones_load() {
        let cfg =
            GuideSettings { docked: true, side: Side::Bottom, width: 512.0, book: 2, topic: 4, ..Default::default() };
        let text = serde_json::to_string(&cfg).unwrap();
        assert_eq!(serde_json::from_str::<GuideSettings>(&text).unwrap(), cfg);
        // Settings saved before the guide existed have none of its fields.
        assert_eq!(serde_json::from_str::<GuideSettings>("{}").unwrap(), GuideSettings::default());
    }
}
