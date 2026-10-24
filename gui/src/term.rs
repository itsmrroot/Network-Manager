//! A terminal screen (xterm-compatible, with colours and full-screen
//! programs) for SSH and Telnet sessions.

use eframe::egui::{self, Color32, FontId, Key, Rect, Sense, Stroke, Ui, Vec2, text::LayoutJob};

pub struct Term {
    parser: vt100::Parser,
    focused: bool,
    id: egui::Id,
    /// What the mouse selected: where it began and where it is now (row, column).
    selection: Option<((u16, u16), (u16, u16))>,
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
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
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
        let mut lines = Vec::new();
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
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }

    /// The word around a cell: letters, digits and the marks that belong to
    /// paths and addresses; spaces and brackets end it.
    fn word_at(&self, row: u16, col: u16) -> ((u16, u16), (u16, u16)) {
        let screen = self.parser.screen();
        let word = |c: u16| {
            screen.cell(row, c).is_some_and(|cell| {
                cell.has_contents()
                    && !cell.contents().chars().any(|ch| ch.is_whitespace() || "()[]{}<>\"'`,;".contains(ch))
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
            self.parser.screen_mut().set_size(rows, cols);
            out.resized = Some((rows, cols));
        }
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect(
            rect,
            10.0,
            BG,
            Stroke::new(if self.focused { 1.5 } else { 1.0 }, if self.focused { accent } else { border }),
            egui::StrokeKind::Inside,
        );
        let origin = rect.min + Vec2::new(10.0, 10.0);
        let screen = self.parser.screen();
        for r in 0..rows {
            let mut job = LayoutJob::default();
            let y = origin.y + r as f32 * rh;
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let (mut fg, mut bg) = (color(cell.fgcolor(), FG, cell.bold()), color(cell.bgcolor(), BG, false));
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
                self.focused = true;
            } else if resp.dragged()
                && let Some((from, _)) = self.selection
            {
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
        if resp.drag_stopped() && self.selection.is_some_and(|(a, b)| a != b) {
            copy_now = true;
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
        if self.focused {
            out.input = self.keys(ui, copy_selection);
            if !out.input.is_empty() {
                self.parser.screen_mut().set_scrollback(0);
                self.selection = None;
            }
            // Keep Tab and the arrows in the terminal.
            capture_keys(ui.ctx(), self.id);
        }
        out
    }

    /// Keys as an xterm sends them.
    fn keys(&self, ui: &Ui, copy_selection: bool) -> Vec<u8> {
        let app = self.parser.screen().application_cursor();
        let bracketed = self.parser.screen().bracketed_paste();
        let mut out = Vec::new();
        let mods = ui.input(|i| i.modifiers);
        for e in ui.input(|i| i.events.clone()) {
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
                    if modifiers.ctrl && modifiers.shift && key == Key::C && copy_selection {
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
                    let arrow = |c: u8| if app { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] };
                    match key {
                        Key::Enter => out.push(b'\r'),
                        Key::Backspace => out.push(0x7f),
                        Key::Tab => out.push(b'\t'),
                        Key::Escape => out.push(0x1b),
                        Key::ArrowUp => out.extend(arrow(b'A')),
                        Key::ArrowDown => out.extend(arrow(b'B')),
                        Key::ArrowRight => out.extend(arrow(b'C')),
                        Key::ArrowLeft => out.extend(arrow(b'D')),
                        Key::Home => out.extend_from_slice(b"\x1b[H"),
                        Key::End => out.extend_from_slice(b"\x1b[F"),
                        Key::Insert => out.extend_from_slice(b"\x1b[2~"),
                        Key::Delete => out.extend_from_slice(b"\x1b[3~"),
                        Key::PageUp => out.extend_from_slice(b"\x1b[5~"),
                        Key::PageDown => out.extend_from_slice(b"\x1b[6~"),
                        Key::F1 => out.extend_from_slice(b"\x1bOP"),
                        Key::F2 => out.extend_from_slice(b"\x1bOQ"),
                        Key::F3 => out.extend_from_slice(b"\x1bOR"),
                        Key::F4 => out.extend_from_slice(b"\x1bOS"),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        out
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
}
