//! A terminal screen (xterm-compatible, with colours and full-screen
//! programs) for SSH and Telnet sessions.

use eframe::egui::{self, Color32, FontId, Key, Rect, Sense, Stroke, Ui, Vec2, text::LayoutJob};
use egui_phosphor::regular as icon;

use crate::i18n::tr;
use crate::term_extras::{self, Kind};

pub struct Term {
    parser: vt100::Parser,
    focused: bool,
    id: egui::Id,
    /// What the mouse selected: where it began and where it is now (row, column).
    selection: Option<((u16, u16), (u16, u16))>,
    /// The mouse is down on a selection: output does not clear it meanwhile.
    dragging: bool,
    /// Frames to keep the keyboard after Paste was clicked: the clipboard's
    /// text arrives a little later, and the click took the focus away.
    paste_wait: u8,
    /// Colour addresses, interface names and words like up, down and error.
    highlight: bool,
    /// The find bar, when it is open.
    find: Option<Find>,
    /// The key that closed the find bar is not for the device.
    skip_keys: bool,
}

/// One match: where it starts (line, column), how many cells it covers, and
/// the same for the rest of it when it goes on in the next screen row.
struct Match {
    line: usize,
    col: usize,
    len: usize,
    tail: Vec<(usize, usize, usize)>,
}

/// A line of the screen as lower-case characters, each with the column it starts in.
type Line = (Vec<char>, Vec<usize>);

/// The find bar: what is searched, and where it was found.
struct Find {
    text: String,
    /// (line, first column, length) of each match: line 0 is the oldest one
    /// kept, and the last `rows` lines are the screen.
    matches: Vec<Match>,
    /// More were found than are kept: the oldest are left out.
    capped: bool,
    current: usize,
    /// How many lines are kept above the screen.
    history: usize,
    /// New output came in since the matches were looked for.
    dirty: bool,
    want_focus: bool,
    /// When the matches were last looked for.
    searched: std::time::Instant,
}

/// What happened to the terminal this frame.
#[derive(Default)]
pub struct Out {
    /// Typed or pasted: to send to the device.
    pub input: Vec<u8>,
    /// The new size in rows and columns, when it changed.
    pub resized: Option<(u16, u16)>,
    /// Text was copied (selected with the mouse, or by the copy keys).
    pub copied: bool,
}

const FONT: f32 = 13.0;
const BG: Color32 = Color32::from_rgb(8, 10, 14);
const FG: Color32 = Color32::from_rgb(220, 226, 235);

/// Gives the keyboard to the terminal `id` for this frame: egui's keyboard
/// focus must sit on it, or its Tab and arrow-key navigation keeps running
/// (the filter below only applies to the focused widget) and moves the focus
/// to other widgets, so that the next Enter presses one of them.
pub fn capture_keys(ctx: &egui::Context, id: egui::Id) {
    ctx.memory_mut(|m| {
        m.request_focus(id);
        m.set_focus_lock_filter(
            id,
            egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
        )
    });
}

impl Term {
    pub fn new(id: u64) -> Self {
        Self {
            parser: vt100::Parser::new(24, 80, 5000),
            focused: true,
            id: egui::Id::new(("term", id)),
            selection: None,
            dragging: false,
            paste_wait: 0,
            highlight: true,
            find: None,
            skip_keys: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if let Some(f) = &mut self.find {
            f.dirty = true;
        }
        // The highlight is kept to screen cells: new output would move the
        // text under it (except while the mouse is held, so a selection can be
        // made in a log that keeps scrolling).
        if !self.dragging {
            self.selection = None;
        }
        self.parser.process(bytes);
    }

    /// A new size for the screen. vt100 cuts rows off the bottom when the
    /// screen gets shorter, which takes the prompt with it and leaves the
    /// cursor on an older line. So the lines above the cursor are first
    /// scrolled up into the history, as much as the screen loses.
    fn set_size(&mut self, rows: u16, cols: u16) {
        let (old_rows, _) = self.parser.screen().size();
        let (row, col) = self.parser.screen().cursor_position();
        let lost = (row + 1).saturating_sub(rows);
        if rows < old_rows && lost > 0 && !self.parser.screen().alternate_screen() {
            let mut seq = format!("\x1b[{old_rows};1H");
            seq.push_str(&"\n".repeat(lost as usize));
            seq.push_str(&format!("\x1b[{};{}H", row - lost + 1, col + 1));
            self.parser.process(seq.as_bytes());
        }
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// Development aid: the find bar open, with this text found.
    #[cfg(debug_assertions)]
    pub fn debug_find(&mut self, text: &str) {
        self.open_find();
        if let Some(f) = &mut self.find {
            f.text = text.to_string();
            f.want_focus = false;
        }
        if self.search() > 0
            && let Some(f) = &mut self.find
        {
            f.current = 0;
        }
        self.show_match();
    }

    /// Colour addresses, interface names and the words that tell how things are.
    pub fn set_highlight(&mut self, on: bool) {
        self.highlight = on;
    }

    /// Opens the find bar (or puts the cursor in it again).
    pub fn open_find(&mut self) {
        let selected = self.selected_text();
        match &mut self.find {
            Some(f) => f.want_focus = true,
            None => {
                let text = if selected.is_empty() || selected.contains('\n') { String::new() } else { selected };
                self.find = Some(Find {
                    text,
                    matches: Vec::new(),
                    capped: false,
                    current: 0,
                    history: 0,
                    dirty: true,
                    want_focus: true,
                    searched: std::time::Instant::now() - std::time::Duration::from_secs(1),
                });
            }
        }
        // Typing goes to the find bar, not to the device.
        self.focused = false;
    }

    /// Every line kept (the oldest first) as lower-case characters, each with
    /// the screen column it starts in (a wide character takes two), and how
    /// many lines are above the screen.
    fn all_lines(&mut self) -> (usize, Vec<Line>, Vec<bool>) {
        let (rows, cols) = self.parser.screen().size();
        let saved = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let history = self.parser.screen().scrollback();
        let total = history + rows as usize;
        let mut lines = vec![(Vec::new(), Vec::new()); total];
        let mut wrapped = vec![false; total];
        let mut offset = history;
        loop {
            self.parser.screen_mut().set_scrollback(offset);
            let screen = self.parser.screen();
            for r in 0..rows {
                let (mut text, mut at) = (Vec::new(), Vec::new());
                for c in 0..cols {
                    match screen.cell(r, c) {
                        Some(cell) if cell.is_wide_continuation() => {}
                        Some(cell) if cell.has_contents() => {
                            for ch in cell.contents().chars() {
                                text.push(ch.to_lowercase().next().unwrap_or(ch));
                                at.push(c as usize);
                            }
                        }
                        _ => {
                            text.push(' ');
                            at.push(c as usize);
                        }
                    }
                }
                if let Some(slot) = lines.get_mut(history - offset + r as usize) {
                    *slot = (text, at);
                    wrapped[history - offset + r as usize] = screen.row_wrapped(r);
                }
            }
            if offset == 0 {
                break;
            }
            offset = offset.saturating_sub(rows as usize);
        }
        self.parser.screen_mut().set_scrollback(saved);
        (history, lines, wrapped)
    }

    /// Looks for the text again, in every line kept. Returns how many were found.
    fn search(&mut self) -> usize {
        let cols = self.parser.screen().size().1 as usize;
        let Some(needle) = self
            .find
            .as_ref()
            .map(|f| f.text.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect::<Vec<char>>())
        else {
            return 0;
        };
        let (history, lines, wrapped) = self.all_lines();
        let mut matches: Vec<Match> = Vec::new();
        let mut capped = false;
        if !needle.is_empty() {
            // The rows of a line that wrapped at the edge are one line.
            let mut groups: Vec<(usize, usize)> = Vec::new();
            let mut first = 0;
            for (n, wraps) in wrapped.iter().enumerate() {
                if !wraps || n + 1 == lines.len() {
                    groups.push((first, n));
                    first = n + 1;
                }
            }
            // The newest first: when there are too many, the oldest are left out.
            'groups: for &(from, to) in groups.iter().rev() {
                let (mut chars, mut place) = (Vec::new(), Vec::new());
                for (row, (text, at)) in lines.iter().enumerate().take(to + 1).skip(from) {
                    for (ch, col) in text.iter().zip(at) {
                        chars.push(*ch);
                        place.push((row, *col));
                    }
                }
                let mut found_here = Vec::new();
                let mut i = 0;
                while i + needle.len() <= chars.len() {
                    if chars[i..i + needle.len()] == needle[..] {
                        // The cells it covers in each row it is in.
                        let mut parts: Vec<(usize, usize, usize)> = Vec::new();
                        let mut k = i;
                        while k < i + needle.len() {
                            let (row, start) = place[k];
                            let mut last = k;
                            while last + 1 < i + needle.len() && place[last + 1].0 == row {
                                last += 1;
                            }
                            let end = match place.get(last + 1) {
                                Some(&(r2, c2)) if r2 == row => c2,
                                _ => cols,
                            };
                            parts.push((row, start, end.saturating_sub(start).max(1)));
                            k = last + 1;
                        }
                        let (line, col, len) = parts[0];
                        found_here.push(Match { line, col, len, tail: parts[1..].to_vec() });
                        i += needle.len();
                    } else {
                        i += 1;
                    }
                }
                for m in found_here.into_iter().rev() {
                    if matches.len() >= 5000 {
                        capped = true;
                        break 'groups;
                    }
                    matches.push(m);
                }
            }
            matches.reverse();
        }
        let found = matches.len();
        if let Some(f) = &mut self.find {
            f.history = history;
            f.matches = matches;
            f.capped = capped;
            f.current = f.current.min(found.saturating_sub(1));
            f.dirty = false;
            f.searched = std::time::Instant::now();
        }
        found
    }

    /// Scrolls back to where the current match is.
    fn show_match(&mut self) {
        let rows = self.parser.screen().size().0 as usize;
        let Some(f) = &self.find else { return };
        let Some(line) = f.matches.get(f.current).map(|m| m.line) else { return };
        let offset = (f.history + rows / 2).saturating_sub(line).min(f.history);
        self.parser.screen_mut().set_scrollback(offset);
        // The selection is on screen cells: it would cover other text now.
        self.selection = None;
    }

    /// The Paste button was clicked: the text comes in a moment, and the
    /// terminal must still take it.
    pub fn expect_paste(&mut self) {
        self.paste_wait = 90;
    }

    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Everything on the screen, as text.
    pub fn text(&self) -> String {
        self.parser.screen().contents()
    }

    /// The selection, first cell to last (the one nearer the top first).
    fn selection_range(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.selection?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected text: lines end where the text ends, as in a terminal.
    fn selected_text(&self) -> String {
        let Some((start, end)) = self.selection_range() else { return String::new() };
        let screen = self.parser.screen();
        let cols = screen.size().1;
        let mut text = String::new();
        for r in start.0..=end.0 {
            let first = if r == start.0 { start.1 } else { 0 };
            let last = if r == end.0 { end.1 } else { cols.saturating_sub(1) };
            let mut line = String::new();
            for c in first..=last {
                match screen.cell(r, c) {
                    Some(cell) if cell.is_wide_continuation() => {}
                    Some(cell) if cell.has_contents() => line.push_str(cell.contents()),
                    _ => line.push(' '),
                }
            }
            // A line that wrapped at the edge of the screen goes on in the
            // next one: it is not a line end.
            if r < end.0 && screen.row_wrapped(r) {
                text.push_str(&line);
            } else {
                text.push_str(line.trim_end());
                if r < end.0 {
                    text.push('\n');
                }
            }
        }
        text.trim_end_matches('\n').to_string()
    }

    /// The word around a cell: letters, digits and the marks that belong to
    /// paths and addresses; spaces and brackets end it.
    fn word_at(&self, row: u16, col: u16) -> ((u16, u16), (u16, u16)) {
        let screen = self.parser.screen();
        let word = |c: u16| {
            screen.cell(row, c).is_some_and(|cell| {
                // The second half of a wide (Chinese, Japanese) character is
                // part of it.
                cell.is_wide_continuation()
                    || (cell.has_contents()
                        && !cell.contents().chars().any(|ch| ch.is_whitespace() || "()[]{}<>\"'`,;".contains(ch)))
            })
        };
        let cols = screen.size().1;
        if !word(col) {
            return ((row, col), (row, col));
        }
        let (mut a, mut b) = (col, col);
        while a > 0 && word(a - 1) {
            a -= 1;
        }
        while b + 1 < cols && word(b + 1) {
            b += 1;
        }
        ((row, a), (row, b))
    }

    pub fn ui(&mut self, ui: &mut Ui, border: Color32, accent: Color32) -> Out {
        let mut out = Out::default();
        let font = FontId::monospace(FONT);
        let (cw, rh) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let avail = (ui.available_size() - Vec2::splat(4.0)).max(Vec2::splat(60.0));
        let cols = ((avail.x - 20.0) / cw).floor().clamp(20.0, 400.0) as u16;
        let rows = ((avail.y - 20.0) / rh).floor().clamp(5.0, 200.0) as u16;
        if self.parser.screen().size() != (rows, cols) {
            self.set_size(rows, cols);
            out.resized = Some((rows, cols));
            if let Some(f) = &mut self.find {
                f.dirty = true;
            }
        }
        // The area has the terminal's own id: the keyboard focus is put on
        // that id (capture_keys), and the accessibility tree must have a node
        // for it, or AccessKit panics when a screen reader is running.
        let (rect, _) = ui.allocate_exact_size(avail, Sense::hover());
        let resp = ui.interact(rect, self.id, Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, tr("Terminal")));
        let painter = ui.painter_at(rect);
        painter.rect(
            rect,
            10.0,
            BG,
            Stroke::new(if self.focused { 1.5 } else { 1.0 }, if self.focused { accent } else { border }),
            egui::StrokeKind::Inside,
        );
        let origin = rect.min + Vec2::new(10.0, 10.0);
        // New output makes the matches old: looked for again, not more often
        // than a few times a second (a device that prints a lot).
        if let Some(f) = &self.find
            && f.dirty
        {
            if f.searched.elapsed() >= std::time::Duration::from_millis(150) {
                self.search();
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
            }
        }
        let screen = self.parser.screen();
        // The line number (0 = the oldest kept) of the screen's first row.
        let first_line = self.find.as_ref().map_or(0, |f| f.history.saturating_sub(screen.scrollback()));
        for r in 0..rows {
            let mut job = LayoutJob::default();
            let y = origin.y + r as f32 * rh;
            // What to colour in this row: one character for each cell.
            let mut tint: Vec<Option<Color32>> = Vec::new();
            if self.highlight {
                let text: Vec<char> = (0..cols)
                    .map(|c| match screen.cell(r, c) {
                        Some(cell) if cell.has_contents() => cell.contents().chars().next().unwrap_or(' '),
                        _ => ' ',
                    })
                    .collect();
                tint = vec![None; cols as usize];
                for (at, len, kind) in term_extras::highlights(&text) {
                    for slot in tint.iter_mut().skip(at).take(len) {
                        *slot = Some(kind_color(kind));
                    }
                }
            }
            if let Some(f) = self.find.as_ref().filter(|f| !f.dirty) {
                let line = first_line + r as usize;
                for (i, m) in f.matches.iter().enumerate() {
                    let parts = std::iter::once((m.line, m.col, m.len)).chain(m.tail.iter().copied());
                    for (l, col, len) in parts {
                        if l != line {
                            continue;
                        }
                        let band = Rect::from_min_size(
                            egui::pos2(origin.x + col as f32 * cw, y),
                            Vec2::new(len as f32 * cw, rh),
                        );
                        let color = if i == f.current {
                            Color32::from_rgba_unmultiplied(255, 140, 0, 170)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 214, 0, 90)
                        };
                        painter.rect_filled(band, 2.0, color);
                    }
                }
            }
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let (mut fg, mut bg) = (color(cell.fgcolor(), FG, cell.bold()), color(cell.bgcolor(), BG, false));
                // Only text in the plain colour is coloured: a program that
                // sets its own colours keeps them.
                if cell.fgcolor() == vt100::Color::Default
                    && cell.bgcolor() == vt100::Color::Default
                    && !cell.inverse()
                    && let Some(Some(tinted)) = tint.get(c as usize)
                {
                    fg = *tinted;
                }
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.dim() {
                    fg = fg.gamma_multiply(0.6);
                }
                if bg != BG {
                    let x = origin.x + c as f32 * cw;
                    let w = if cell.is_wide() { cw * 2.0 } else { cw };
                    painter.rect_filled(Rect::from_min_size(egui::pos2(x, y), Vec2::new(w, rh)), 0.0, bg);
                }
                let text = if cell.has_contents() { cell.contents() } else { " " };
                job.append(
                    text,
                    0.0,
                    egui::TextFormat {
                        font_id: font.clone(),
                        color: fg,
                        underline: if cell.underline() { Stroke::new(1.0, fg) } else { Stroke::NONE },
                        italics: cell.italic(),
                        ..Default::default()
                    },
                );
            }
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(egui::pos2(origin.x, y), galley, FG);
        }
        if screen.scrollback() == 0 && !screen.hide_cursor() {
            let (cr, cc) = screen.cursor_position();
            let at = Rect::from_min_size(
                egui::pos2(origin.x + cc as f32 * cw, origin.y + cr as f32 * rh),
                Vec2::new(cw, rh),
            );
            if self.focused {
                painter.rect_filled(at, 1.0, accent.gamma_multiply(0.7));
            } else {
                painter.rect_stroke(at, 1.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
            }
        }

        // Selecting with the mouse: drag, double-click a word, triple-click a
        // line. What is selected is copied when the mouse is let go.
        let (cols_max, rows_max) = (cols.saturating_sub(1) as f32, rows.saturating_sub(1) as f32);
        let cell_at = |pos: egui::Pos2| -> (u16, u16) {
            let c = ((pos.x - origin.x) / cw).floor().clamp(0.0, cols_max) as u16;
            let r = ((pos.y - origin.y) / rh).floor().clamp(0.0, rows_max) as u16;
            (r, c)
        };
        if out.resized.is_some() {
            self.selection = None;
        }
        let mut copy_now = false;
        if let Some(pos) = resp.interact_pointer_pos() {
            let cell = cell_at(pos);
            if resp.drag_started() {
                // From where the button went down: a drag only starts after
                // the mouse has moved a little.
                let from = ui.input(|i| i.pointer.press_origin()).map_or(cell, cell_at);
                self.selection = Some((from, cell));
                self.dragging = true;
                self.focused = true;
            } else if resp.dragged() {
                // From where the button went down, as long as it is down.
                let from = ui.input(|i| i.pointer.press_origin()).map_or(cell, cell_at);
                self.selection = Some((from, cell));
            }
            if resp.triple_clicked() {
                self.selection = Some(((cell.0, 0), (cell.0, cols_max as u16)));
                copy_now = true;
            } else if resp.double_clicked() {
                self.selection = Some(self.word_at(cell.0, cell.1));
                copy_now = true;
            }
        }
        if resp.drag_stopped() {
            self.dragging = false;
            if self.selection.is_some_and(|(a, b)| a != b) {
                copy_now = true;
            }
        }
        if resp.clicked() && !resp.double_clicked() && !resp.triple_clicked() {
            self.selection = None;
        }
        if let Some((start, end)) = self.selection_range() {
            for r in start.0..=end.0.min(rows_max as u16) {
                let first = if r == start.0 { start.1 } else { 0 };
                let last = if r == end.0 { end.1 } else { cols_max as u16 };
                let x0 = origin.x + first as f32 * cw;
                let w = (last - first + 1) as f32 * cw;
                let band = Rect::from_min_size(egui::pos2(x0, origin.y + r as f32 * rh), Vec2::new(w, rh));
                painter.rect_filled(band, 0.0, accent.gamma_multiply(0.4));
            }
        }
        if copy_now {
            let text = self.selected_text();
            if !text.trim().is_empty() {
                ui.ctx().copy_text(text);
                out.copied = true;
            }
        }
        // Cmd+C (Ctrl+Shift+C) copies the selection; with a selection Ctrl+C
        // copies too, and without one it stops the program as in a terminal.
        let copy_keys = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, egui::Event::Copy)
                    || matches!(e, egui::Event::Key { key: Key::C, pressed: true, modifiers, .. } if modifiers.ctrl && modifiers.shift)
            })
        });
        let selecting = self.focused && self.selection.is_some_and(|(a, b)| a != b);
        let copy_selection = copy_keys && selecting;
        if copy_selection {
            let text = self.selected_text();
            if !text.trim().is_empty() {
                ui.ctx().copy_text(text);
                out.copied = true;
            }
            self.selection = None;
        }
        if resp.clicked() {
            self.focused = true;
        } else if ui.input(|i| i.pointer.any_click()) && !resp.hovered() {
            self.focused = false;
        }
        // Mouse wheel: look back through the scrollback.
        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                let lines = (dy / rh).round() as isize;
                let now = self.parser.screen().scrollback() as isize;
                self.parser.screen_mut().set_scrollback((now + lines).max(0) as usize);
            }
        }
        // Cmd+F (Ctrl+Shift+F elsewhere, where Ctrl+F belongs to the shell)
        // opens the find bar.
        let find_key = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, egui::Event::Key { key: Key::F, pressed: true, modifiers, .. }
                    if if cfg!(target_os = "macos") { modifiers.mac_cmd } else { modifiers.ctrl && modifiers.shift })
            })
        });
        if find_key && (self.focused || resp.hovered()) {
            self.open_find();
        }
        if self.find.is_some() {
            self.find_bar(ui, rect, accent);
        }
        if self.paste_wait > 0 {
            self.paste_wait -= 1;
            self.focused = true;
        }
        if self.focused {
            if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))) {
                self.paste_wait = 0;
            }
            out.input = if std::mem::take(&mut self.skip_keys) { Vec::new() } else { self.keys(ui, copy_selection) };
            if !out.input.is_empty() {
                self.parser.screen_mut().set_scrollback(0);
                self.selection = None;
            }
            // Keep Tab and the arrows in the terminal.
            capture_keys(ui.ctx(), self.id);
        }
        out
    }

    /// The find bar, on the top right of the terminal.
    fn find_bar(&mut self, ui: &mut Ui, area: Rect, accent: Color32) {
        let Some(mut f) = self.find.take() else { return };
        let width = 372.0_f32.min(area.width() - 16.0).max(200.0);
        let bar = Rect::from_min_size(egui::pos2(area.right() - width - 8.0, area.top() + 8.0), Vec2::new(width, 36.0));
        let mut child =
            ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let (mut close, mut changed, mut step) = (false, false, 0i32);
        let field = self.id.with("find-field");
        egui::Frame::new()
            .fill(Color32::from_rgb(28, 32, 40))
            .stroke(Stroke::new(1.0, accent.gamma_multiply(0.6)))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(&mut child, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut f.text)
                        .id(field)
                        .hint_text(tr("Find in the terminal"))
                        .desired_width(width - 150.0),
                );
                if f.want_focus {
                    r.request_focus();
                    f.want_focus = false;
                }
                changed = r.changed();
                if (r.has_focus() || r.lost_focus()) && ui.input(|i| i.key_pressed(Key::Escape)) {
                    close = true;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    step = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
                    r.request_focus();
                }
                let count = if f.text.is_empty() {
                    String::new()
                } else if f.matches.is_empty() {
                    tr("No match").to_string()
                } else {
                    format!("{}/{}{}", f.current + 1, f.matches.len(), if f.capped { "+" } else { "" })
                };
                ui.label(egui::RichText::new(count).size(12.0).color(Color32::from_rgb(170, 178, 190)));
                if ui.add(egui::Button::new(icon::CARET_UP).frame(false)).on_hover_text(tr("Previous")).clicked() {
                    step = -1;
                }
                if ui.add(egui::Button::new(icon::CARET_DOWN).frame(false)).on_hover_text(tr("Next")).clicked() {
                    step = 1;
                }
                if ui.add(egui::Button::new(icon::X).frame(false)).on_hover_text(tr("Close")).clicked() {
                    close = true;
                }
            });
        if close {
            self.focused = true;
            self.skip_keys = true;
            return;
        }
        self.find = Some(f);
        if changed {
            if self.search() > 0
                && let Some(f) = &mut self.find
            {
                // The newest match first: that is what was just printed.
                f.current = f.matches.len() - 1;
            }
            self.show_match();
        } else if step != 0
            && let Some(f) = &mut self.find
            && !f.matches.is_empty()
        {
            let n = f.matches.len() as i32;
            f.current = (f.current as i32 + step).rem_euclid(n) as usize;
            self.show_match();
        }
    }

    /// Keys as an xterm sends them.
    fn keys(&self, ui: &Ui, copy_selection: bool) -> Vec<u8> {
        let app = self.parser.screen().application_cursor();
        let bracketed = self.parser.screen().bracketed_paste();
        let mut out = Vec::new();
        let mods = ui.input(|i| i.modifiers);
        let events = ui.input(|i| i.events.clone());
        // A window of its own can run this frame's input more than once (egui's extra
        // passes): without this, a pasted event still in it is sent to the remote again.
        ui.ctx().input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Paste(_))));
        for e in events {
            match e {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                // Windows and Linux: egui turns Ctrl+C and Ctrl+X into Copy and
                // Cut (and sends no key): in a terminal they stop a program
                // (top, ping) and cut, as everywhere. macOS copies with Cmd.
                // Not Ctrl+Shift+C (copy in Linux terminals) or Ctrl+Insert.
                egui::Event::Copy if !copy_selection && !cfg!(target_os = "macos") && mods.ctrl && !mods.shift => {
                    out.push(0x03)
                }
                egui::Event::Cut if !cfg!(target_os = "macos") => {
                    if mods.ctrl && !mods.shift {
                        out.push(0x18);
                    } else if mods.shift && !mods.ctrl {
                        // Windows: Shift+Delete.
                        out.extend_from_slice(b"\x1b[3~");
                    }
                }
                egui::Event::Paste(t) => {
                    // Pasted text is only text: no escape sequences (which
                    // could end the paste early and run what follows) and no
                    // other control characters but tab and line ends.
                    let t: String = t
                        .replace("\r\n", "\r")
                        .replace('\n', "\r")
                        .chars()
                        .filter(|c| !c.is_control() || *c == '\t' || *c == '\r')
                        .collect();
                    if bracketed {
                        out.extend_from_slice(b"\x1b[200~");
                    }
                    out.extend_from_slice(t.as_bytes());
                    if bracketed {
                        out.extend_from_slice(b"\x1b[201~");
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    // Ctrl with Alt is AltGr on Windows: a character, sent as text.
                    // Ctrl+Shift+C is copy (Linux terminals): never a ^C.
                    if modifiers.ctrl && modifiers.shift && matches!(key, Key::C | Key::F) {
                        continue;
                    }
                    if modifiers.ctrl && !modifiers.alt {
                        let name = key.name();
                        if name.len() == 1
                            && let Some(c) = name.chars().next().filter(|c| c.is_ascii_alphabetic())
                        {
                            out.push((c.to_ascii_uppercase() as u8) - b'@');
                            continue;
                        }
                        if key == Key::Num6 && modifiers.shift {
                            out.push(0x1e);
                            continue;
                        }
                    }
                    // xterm's modifier number: 1 + Shift 1 + Alt 2 + Ctrl 4.
                    let m = 1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.ctrl);
                    // Arrows, Home and End: ESC [ X, ESC O X in application
                    // mode, and ESC [ 1 ; m X with Shift, Alt or Ctrl.
                    let arrow = |c: u8| {
                        if m > 1 {
                            format!("\x1b[1;{m}{}", c as char).into_bytes()
                        } else if app {
                            vec![0x1b, b'O', c]
                        } else {
                            vec![0x1b, b'[', c]
                        }
                    };
                    match key {
                        Key::Enter => out.push(b'\r'),
                        Key::Backspace => out.push(0x7f),
                        Key::Tab if modifiers.shift => out.extend_from_slice(b"\x1b[Z"),
                        Key::Tab => out.push(b'\t'),
                        Key::Escape => out.push(0x1b),
                        Key::ArrowUp => out.extend(arrow(b'A')),
                        Key::ArrowDown => out.extend(arrow(b'B')),
                        Key::ArrowRight => out.extend(arrow(b'C')),
                        Key::ArrowLeft => out.extend(arrow(b'D')),
                        Key::Home => out.extend(arrow(b'H')),
                        Key::End => out.extend(arrow(b'F')),
                        Key::Insert => out.extend_from_slice(b"\x1b[2~"),
                        Key::Delete => out.extend_from_slice(b"\x1b[3~"),
                        Key::PageUp => out.extend_from_slice(b"\x1b[5~"),
                        Key::PageDown => out.extend_from_slice(b"\x1b[6~"),
                        Key::F1 => out.extend_from_slice(b"\x1bOP"),
                        Key::F2 => out.extend_from_slice(b"\x1bOQ"),
                        Key::F3 => out.extend_from_slice(b"\x1bOR"),
                        Key::F4 => out.extend_from_slice(b"\x1bOS"),
                        Key::F5 => out.extend_from_slice(b"\x1b[15~"),
                        Key::F6 => out.extend_from_slice(b"\x1b[17~"),
                        Key::F7 => out.extend_from_slice(b"\x1b[18~"),
                        Key::F8 => out.extend_from_slice(b"\x1b[19~"),
                        Key::F9 => out.extend_from_slice(b"\x1b[20~"),
                        Key::F10 => out.extend_from_slice(b"\x1b[21~"),
                        Key::F11 => out.extend_from_slice(b"\x1b[23~"),
                        Key::F12 => out.extend_from_slice(b"\x1b[24~"),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// The colour of a kind of text (the terminal is dark in every theme).
fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Address => Color32::from_rgb(90, 200, 250),
        Kind::Mac => Color32::from_rgb(200, 150, 250),
        Kind::Interface => Color32::from_rgb(240, 200, 110),
        Kind::Good => Color32::from_rgb(80, 220, 140),
        Kind::Bad => Color32::from_rgb(255, 100, 100),
        Kind::Warn => Color32::from_rgb(255, 170, 70),
    }
}

/// xterm's 256 colours.
fn color(c: vt100::Color, default: Color32, bold: bool) -> Color32 {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i) => {
            // Bold makes the first eight colours bright, as xterm does.
            let i = if bold && i < 8 { i + 8 } else { i };
            match i {
                0..=15 => {
                    let (r, g, b) = BASE[i as usize];
                    Color32::from_rgb(r, g, b)
                }
                16..=231 => {
                    let n = i - 16;
                    let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                    Color32::from_rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
                }
                _ => {
                    let v = 8 + (i - 232) * 10;
                    Color32::from_rgb(v, v, v)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton, Pos2};

    #[derive(Default)]
    struct State {
        /// Times the first button on the page ("Overview" in the sidebar) was pressed.
        overview: u32,
        /// Connected: the terminal takes the keys (set by the click on Connect, as in the console).
        connected: bool,
        /// Where the Connect button was.
        connect_at: Pos2,
        /// Key presses that reached the terminal.
        seen: usize,
    }

    fn key(key: Key) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    /// One frame of a page built as the serial console is: a button first
    /// (the sidebar's Overview), a Connect button until connected, and the
    /// terminal. `fixed` takes the keyboard with [`capture_keys`]; the old
    /// code only set the lock filter, which applies to the focused widget.
    fn frame(ctx: &egui::Context, events: Vec<Event>, fixed: bool, st: &mut State) {
        let input = egui::RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            if ui.button("Overview").clicked() {
                st.overview += 1;
            }
            if !st.connected {
                let connect = ui.button("Connect");
                st.connect_at = connect.rect.center();
                if connect.clicked() {
                    st.connected = true;
                }
            }
            let (rect, _) = ui.allocate_exact_size(Vec2::new(400.0, 200.0), Sense::hover());
            let id = egui::Id::new("terminal-focus");
            // As in the console: the terminal's own click area.
            ui.interact(rect, id, Sense::click());
            if st.connected {
                st.seen += ui.input(|i| i.events.iter().filter(|e| matches!(e, Event::Key { .. })).count());
                if fixed {
                    capture_keys(ui.ctx(), id);
                } else {
                    ui.ctx().memory_mut(|m| {
                        m.set_focus_lock_filter(
                            id,
                            egui::EventFilter {
                                tab: true,
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                escape: true,
                            },
                        )
                    });
                }
            }
        });
        // Nothing paints these frames: the texture updates are not needed.
        output.textures_delta.clear();
    }

    /// Click Connect, then type as on a switch: the keys in `keys`.
    fn type_a_command(fixed: bool, keys: &[Key]) -> State {
        let ctx = egui::Context::default();
        let mut st = State::default();
        for _ in 0..3 {
            frame(&ctx, vec![], fixed, &mut st);
        }
        let at = st.connect_at;
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame(&ctx, vec![Event::PointerMoved(at), button(true)], fixed, &mut st);
        frame(&ctx, vec![button(false)], fixed, &mut st);
        assert!(st.connected, "the Connect button was not clicked");
        for _ in 0..3 {
            frame(&ctx, vec![], fixed, &mut st);
        }
        for &k in keys {
            frame(&ctx, vec![key(k)], fixed, &mut st);
            frame(&ctx, vec![], fixed, &mut st);
        }
        st
    }

    /// The keys a person types on a switch that move egui's focus unless the
    /// terminal holds it: Tab completion, history with the arrows, Escape.
    #[test]
    fn enter_does_not_press_other_buttons() {
        for keys in [
            vec![Key::Tab, Key::Enter],
            vec![Key::ArrowDown, Key::Enter],
            vec![Key::ArrowUp, Key::ArrowUp, Key::Enter],
            vec![Key::Tab, Key::Tab, Key::Enter],
        ] {
            let old = type_a_command(false, &keys);
            let new = type_a_command(true, &keys);
            println!("{keys:?}: old pressed Overview {} times; new {}", old.overview, new.overview);
            assert_eq!(new.overview, 0, "Enter reached the button behind the terminal after {keys:?}");
            assert_eq!(new.seen, keys.len(), "every key press reaches the terminal");
        }
        // The test must show the old bug, or it proves nothing.
        assert!(type_a_command(false, &[Key::Tab, Key::Enter]).overview > 0, "the old code no longer shows the bug");
    }

    /// Text copied during a frame of a lone terminal.
    fn frame_of(ctx: &egui::Context, term: &mut Term, events: Vec<Event>, time: f64) -> (Out, Vec<String>) {
        let input = egui::RawInput {
            events,
            time: Some(time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))),
            ..Default::default()
        };
        let mut result = None;
        let mut output = ctx.run_ui(input, |ui| {
            result = Some(term.ui(ui, Color32::GRAY, Color32::GREEN));
        });
        let copied = output
            .platform_output
            .commands
            .drain(..)
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t),
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        (result.unwrap_or_default(), copied)
    }

    fn press(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    /// A pop-out window's frame can run its UI more than once (egui's extra
    /// passes): a pasted event still in the input on the second run must not be
    /// sent to the device again.
    #[test]
    fn a_pasted_event_is_not_sent_twice_in_the_same_frame() {
        let ctx = egui::Context::default();
        let term = Term::new(1);
        let input = egui::RawInput { events: vec![Event::Paste("hello".into())], ..Default::default() };
        let mut passes = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            passes.push(term.keys(ui, false));
            passes.push(term.keys(ui, false));
        });
        output.textures_delta.clear();
        assert_eq!(passes[0], b"hello", "{passes:?}");
        assert!(passes[1].is_empty(), "the second pass sent the paste again: {passes:?}");
    }

    /// Dragging over text selects it and copies it; a double-click takes a
    /// word, a triple-click the line; a plain click selects nothing.
    #[test]
    fn the_mouse_selects_text_and_copies_it() {
        let ctx = egui::Context::default();
        let mut term = Term::new(1);
        term.feed(b"hello world foo\r\nsecond line\r\n");
        // Where a cell is: the terminal starts 10 points in, in a page's corner.
        let font = FontId::monospace(FONT);
        let (mut cw, mut rh) = (0.0, 0.0);
        let mut measured = ctx.run_ui(egui::RawInput::default(), |ui| {
            (cw, rh) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        });
        measured.textures_delta.clear();
        let at = |row: f32, col: f32| Pos2::new(10.0 + (col + 0.5) * cw, 10.0 + (row + 0.5) * rh);
        frame_of(&ctx, &mut term, vec![], 0.0);

        // Drag from "h" to "o" of the first word.
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(at(0.0, 0.0))], 1.0);
        frame_of(&ctx, &mut term, vec![press(at(0.0, 0.0), true)], 1.1);
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(at(0.0, 2.0))], 1.2);
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(at(0.0, 4.0))], 1.3);
        let (out, copied) = frame_of(&ctx, &mut term, vec![press(at(0.0, 4.0), false)], 1.4);
        assert_eq!(copied, ["hello"]);
        assert!(out.copied);

        // Across two lines.
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(at(0.0, 6.0))], 3.0);
        frame_of(&ctx, &mut term, vec![press(at(0.0, 6.0), true)], 3.1);
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(at(1.0, 5.0))], 3.2);
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(at(1.0, 5.0))], 3.3);
        let (_, copied) = frame_of(&ctx, &mut term, vec![press(at(1.0, 5.0), false)], 3.4);
        assert_eq!(copied, ["world foo\nsecond"]);

        // A double-click on "world".
        let spot = at(0.0, 7.0);
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(spot)], 5.0);
        let click = vec![press(spot, true), press(spot, false), press(spot, true), press(spot, false)];
        let (_, copied) = frame_of(&ctx, &mut term, click, 5.1);
        assert_eq!(copied, ["world"]);

        // A click somewhere else selects nothing and copies nothing.
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(at(1.0, 2.0))], 7.0);
        let (_, copied) = frame_of(&ctx, &mut term, vec![press(at(1.0, 2.0), true), press(at(1.0, 2.0), false)], 7.1);
        assert!(copied.is_empty());
        assert!(term.selection.is_none());
    }

    /// A shorter screen (a bar appears above the terminal) keeps the prompt
    /// and the cursor on it; the lines above go into the history.
    #[test]
    fn a_shorter_screen_keeps_the_prompt() {
        let mut term = Term::new(10);
        term.set_size(10, 40);
        for i in 0..8 {
            term.feed(format!("line {i}\r\n").as_bytes());
        }
        term.feed(b"Switch#");
        assert_eq!(term.parser.screen().cursor_position(), (8, 7));
        term.set_size(6, 40);
        let screen = term.parser.screen();
        assert_eq!(screen.cursor_position(), (5, 7));
        assert_eq!(screen.contents_between(5, 0, 5, 40), "Switch#");
        assert_eq!(screen.contents_between(4, 0, 4, 40), "line 7");
        term.feed(b"x");
        assert_eq!(term.parser.screen().contents_between(5, 0, 5, 40), "Switch#x");
        let (history, lines, _) = term.all_lines();
        assert_eq!(history, 3);
        let first: String = lines[0].0.iter().collect();
        assert_eq!(first.trim_end(), "line 0");
    }

    /// The terminal has the keyboard focus, and the accessibility tree has a
    /// node for it (AccessKit panicked on a focused id that is not in the tree).
    #[test]
    fn the_focused_terminal_is_in_the_accessibility_tree() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut term = Term::new(11);
        term.feed(b"Switch#");
        for t in 0..3 {
            let input = egui::RawInput {
                time: Some(t as f64),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                term.ui(ui, Color32::GRAY, Color32::GREEN);
            });
            output.textures_delta.clear();
            let update = output.platform_output.accesskit_update.expect("an accessibility tree");
            if t > 0 {
                assert_eq!(update.focus, term.id.accesskit_id());
                assert!(update.nodes.iter().any(|(id, _)| *id == update.focus));
            }
        }
    }

    /// Where cell (row, col) is on a lone terminal in the test page.
    fn cell_point(ctx: &egui::Context, row: f32, col: f32) -> Pos2 {
        let font = FontId::monospace(FONT);
        let (mut cw, mut rh) = (0.0, 0.0);
        let mut measured = ctx.run_ui(egui::RawInput::default(), |ui| {
            (cw, rh) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        });
        measured.textures_delta.clear();
        Pos2::new(10.0 + (col + 0.5) * cw, 10.0 + (row + 0.5) * rh)
    }

    fn drag(ctx: &egui::Context, term: &mut Term, from: Pos2, to: Pos2, start: f64) -> Vec<String> {
        let mut t = start;
        let mut step = |events: Vec<Event>| {
            t += 0.1;
            frame_of(ctx, term, events, t).1
        };
        step(vec![Event::PointerMoved(from)]);
        step(vec![press(from, true)]);
        step(vec![Event::PointerMoved(to)]);
        step(vec![Event::PointerMoved(to)]);
        step(vec![press(to, false)])
    }

    /// A line that wrapped at the edge of the screen is copied as one line;
    /// blank rows at the end are not copied; a double-click takes a whole
    /// Chinese word.
    #[test]
    fn copying_follows_the_text_not_the_screen() {
        let ctx = egui::Context::default();
        let mut term = Term::new(2);
        frame_of(&ctx, &mut term, vec![], 0.0);
        let cols = term.size().1 as usize;
        let long: String = (0..cols + 10).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        term.feed(format!("{long}\r\n你好世界 abc\r\n").as_bytes());
        frame_of(&ctx, &mut term, vec![], 0.5);

        // From the start of the long line to the second screen row.
        let copied = drag(&ctx, &mut term, cell_point(&ctx, 0.0, 0.0), cell_point(&ctx, 1.0, 5.0), 2.0);
        assert_eq!(copied, [long[..cols + 6].to_string()]);

        // Down to blank rows: only the text is copied.
        let copied = drag(&ctx, &mut term, cell_point(&ctx, 1.0, 0.0), cell_point(&ctx, 8.0, 3.0), 4.0);
        assert_eq!(copied.len(), 1);
        assert!(copied[0].ends_with("你好世界 abc"), "{:?}", copied[0]);

        // A double-click on the second character of the Chinese word.
        let spot = cell_point(&ctx, 2.0, 2.0);
        frame_of(&ctx, &mut term, vec![Event::PointerMoved(spot)], 9.0);
        let click = vec![press(spot, true), press(spot, false), press(spot, true), press(spot, false)];
        let (_, copied) = frame_of(&ctx, &mut term, click, 9.1);
        assert_eq!(copied, ["你好世界"]);
    }

    /// The find bar looks in the screen and in what scrolled by, shows the
    /// match, and typing in it does not reach the device.
    #[test]
    fn the_find_bar_searches_the_scrollback() {
        let ctx = egui::Context::default();
        let mut term = Term::new(3);
        frame_of(&ctx, &mut term, vec![], 0.0);
        let mut lines = String::new();
        for i in 0..300 {
            lines.push_str(&format!("row {i} marker {}\r\n", if i % 100 == 7 { "Gi1/0/7 is down" } else { "ok" }));
        }
        term.feed(lines.as_bytes());
        frame_of(&ctx, &mut term, vec![], 0.1);

        // The bar opens and takes the typing.
        term.open_find();
        let (_, _) = frame_of(&ctx, &mut term, vec![], 0.2);
        let (out, _) = frame_of(&ctx, &mut term, vec![Event::Text("is DOWN".into())], 0.3);
        assert!(out.input.is_empty(), "the typing went to the device: {:?}", out.input);
        let f = term.find.as_ref().expect("the bar is open");
        assert_eq!(f.text, "is DOWN");
        // Found in two lines, one of them long gone from the screen; the newest first.
        assert_eq!(f.matches.len(), 3);
        assert_eq!(f.current, 2);
        assert!(term.text().contains("row 207"), "the newest match is on the screen");

        // Next goes round to the oldest one, which is in the scrollback.
        term.find.as_mut().unwrap().current = 0;
        term.show_match();
        assert!(term.text().contains("row 7 marker"), "{}", term.text());
        assert!(!term.text().contains("row 207"));

        // New output does not lose the matches.
        term.feed(b"row 300 Gi1/0/9 is down\r\n");
        // Looked for again within a fraction of a second, not at every frame.
        std::thread::sleep(std::time::Duration::from_millis(200));
        frame_of(&ctx, &mut term, vec![], 0.4);
        assert_eq!(term.find.as_ref().unwrap().matches.len(), 4);
    }

    /// Wide characters take two cells: a match is where it is drawn. The Esc
    /// that closes the find bar is not sent to the device.
    #[test]
    fn find_knows_wide_characters_and_keeps_escape_to_itself() {
        let ctx = egui::Context::default();
        let mut term = Term::new(4);
        frame_of(&ctx, &mut term, vec![], 0.0);
        term.feed("日本語 abc\r\n".as_bytes());
        frame_of(&ctx, &mut term, vec![], 0.1);
        term.debug_find("abc");
        let f = term.find.as_ref().unwrap();
        assert_eq!(
            f.matches.iter().map(|m| (m.line, m.col, m.len)).collect::<Vec<_>>(),
            [(f.history, 7, 3)],
            "three wide characters and a space come first: 7 cells"
        );

        // Esc closes the bar and does not reach the device.
        term.open_find();
        frame_of(&ctx, &mut term, vec![], 0.2);
        let (out, _) = frame_of(&ctx, &mut term, vec![key(Key::Escape)], 0.3);
        assert!(term.find.is_none(), "the bar is closed");
        assert!(out.input.is_empty(), "Esc went to the device: {:?}", out.input);
        // The next Esc is the device's.
        let (out, _) = frame_of(&ctx, &mut term, vec![key(Key::Escape)], 0.4);
        assert_eq!(out.input, [0x1b]);
    }

    /// A match that wraps over two screen rows is found, and with too many
    /// matches it is the oldest that are left out.
    #[test]
    fn find_follows_wrapped_lines_and_keeps_the_newest() {
        let ctx = egui::Context::default();
        let mut term = Term::new(5);
        frame_of(&ctx, &mut term, vec![], 0.0);
        let cols = term.size().1 as usize;
        // "NEEDLE" cut by the end of the row.
        let line: String = std::iter::repeat_n('x', cols - 3).collect::<String>() + "NEEDLE tail";
        term.feed(format!("{line}\r\n").as_bytes());
        frame_of(&ctx, &mut term, vec![], 0.1);
        term.debug_find("needle");
        let f = term.find.as_ref().unwrap();
        assert_eq!(f.matches.len(), 1, "the wrapped match is found");
        assert_eq!(f.matches[0].tail.len(), 1, "and it goes on in the next row");

        // Many matches: the count stops at 5000 and the newest are kept.
        let mut many = String::new();
        for i in 0..3000 {
            many.push_str(&format!("hit hit {i}\r\n"));
        }
        term.feed(many.as_bytes());
        term.debug_find("hit");
        let f = term.find.as_ref().unwrap();
        assert_eq!(f.matches.len(), 5000);
        assert!(f.capped, "the count says there are more");
        // The last one is the newest line.
        term.find.as_mut().unwrap().current = 4999;
        term.show_match();
        assert!(term.text().contains("hit hit 2999"), "{}", term.text());
        // The first one kept is not the very first line of the log.
        term.find.as_mut().unwrap().current = 0;
        term.show_match();
        assert!(!term.text().contains("NEEDLE"), "the oldest matches were dropped");
    }
}
