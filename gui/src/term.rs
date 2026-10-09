//! A terminal screen (xterm-compatible, with colours and full-screen
//! programs) for SSH and Telnet sessions.

use eframe::egui::{self, Color32, FontId, Key, Rect, Sense, Stroke, Ui, Vec2, text::LayoutJob};

pub struct Term {
    parser: vt100::Parser,
    focused: bool,
    id: egui::Id,
}

/// What happened to the terminal this frame.
#[derive(Default)]
pub struct Out {
    /// Typed or pasted: to send to the device.
    pub input: Vec<u8>,
    /// The new size in rows and columns, when it changed.
    pub resized: Option<(u16, u16)>,
}

const FONT: f32 = 13.0;
const BG: Color32 = Color32::from_rgb(8, 10, 14);
const FG: Color32 = Color32::from_rgb(220, 226, 235);

impl Term {
    pub fn new(id: u64) -> Self {
        Self { parser: vt100::Parser::new(24, 80, 5000), focused: true, id: egui::Id::new(("term", id)) }
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

    pub fn ui(&mut self, ui: &mut Ui, border: Color32, accent: Color32) -> Out {
        let mut out = Out::default();
        let font = FontId::monospace(FONT);
        let (cw, rh) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let avail = ui.available_size() - Vec2::splat(4.0);
        let cols = ((avail.x - 20.0) / cw).floor().clamp(20.0, 400.0) as u16;
        let rows = ((avail.y - 20.0) / rh).floor().clamp(5.0, 200.0) as u16;
        if self.parser.screen().size() != (rows, cols) {
            self.parser.screen_mut().set_size(rows, cols);
            out.resized = Some((rows, cols));
        }
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click());
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
            out.input = self.keys(ui);
            if !out.input.is_empty() {
                self.parser.screen_mut().set_scrollback(0);
            }
            // Keep Tab and the arrows in the terminal.
            ui.ctx().memory_mut(|m| {
                m.request_focus(self.id);
                m.set_focus_lock_filter(
                    self.id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
        }
        out
    }

    /// Keys as an xterm sends them.
    fn keys(&self, ui: &Ui) -> Vec<u8> {
        let app = self.parser.screen().application_cursor();
        let bracketed = self.parser.screen().bracketed_paste();
        let mut out = Vec::new();
        for e in ui.input(|i| i.events.clone()) {
            match e {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Paste(t) => {
                    let t = t.replace("\r\n", "\r").replace('\n', "\r");
                    if bracketed {
                        out.extend_from_slice(b"\x1b[200~");
                    }
                    out.extend_from_slice(t.as_bytes());
                    if bracketed {
                        out.extend_from_slice(b"\x1b[201~");
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if modifiers.ctrl {
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
