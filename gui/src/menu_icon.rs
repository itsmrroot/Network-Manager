//! Pictures for the macOS menus: the app's own icons (the Phosphor font that
//! the pages use), drawn into a small bitmap, because a native menu cannot
//! show a font. Grey, so that they read in both the light and the dark menu.
// Only the macOS menu bar draws these.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

/// A bitmap of `side` x `side` pixels, RGBA, straight (not premultiplied) alpha.
pub struct Bitmap {
    pub side: u32,
    pub rgba: Vec<u8>,
}

/// The outline of a glyph as closed polygons.
#[derive(Default)]
struct Outline {
    polygons: Vec<Vec<(f32, f32)>>,
    current: Vec<(f32, f32)>,
    last: (f32, f32),
}

impl Outline {
    fn flush(&mut self) {
        if self.current.len() >= 3 {
            self.polygons.push(std::mem::take(&mut self.current));
        } else {
            self.current.clear();
        }
    }
}

impl OutlinePen for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.current.push((x, y));
        self.last = (x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.current.push((x, y));
        self.last = (x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (x0, y0) = self.last;
        for i in 1..=8 {
            let t = i as f32 / 8.0;
            let u = 1.0 - t;
            self.current.push((u * u * x0 + 2.0 * u * t * cx + t * t * x, u * u * y0 + 2.0 * u * t * cy + t * t * y));
        }
        self.last = (x, y);
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (x0, y0) = self.last;
        for i in 1..=12 {
            let t = i as f32 / 12.0;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.current.push((a * x0 + b * c1x + c * c2x + d * x, a * y0 + b * c1y + c * c2y + d * y));
        }
        self.last = (x, y);
    }

    fn close(&mut self) {
        self.flush();
    }
}

/// An icon of the Phosphor font (`glyph` as in `egui_phosphor::regular::GEAR`)
/// in a `side` pixel square, with `color`.
pub fn render(glyph: &str, side: u32, color: [u8; 3]) -> Option<Bitmap> {
    let ch = glyph.chars().next()?;
    let font = FontRef::new(egui_phosphor::Variant::Regular.font_bytes()).ok()?;
    let id = font.charmap().map(ch)?;
    // Glyph 0 is the box that stands for a character the font does not have.
    if id.to_u32() == 0 {
        return None;
    }
    let mut outline = Outline::default();
    font.outline_glyphs()
        .get(id)?
        .draw(DrawSettings::unhinted(Size::new(1000.0), LocationRef::default()), &mut outline)
        .ok()?;
    outline.flush();
    if outline.polygons.is_empty() {
        return None;
    }
    // Fit the drawing in the square, with a little room around it.
    let points = outline.polygons.iter().flatten();
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for &(x, y) in points {
        (min_x, max_x, min_y, max_y) = (min_x.min(x), max_x.max(x), min_y.min(y), max_y.max(y));
    }
    let (w, h) = ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0));
    let pad = side as f32 * 0.06;
    let scale = (side as f32 - 2.0 * pad) / w.max(h);
    let (off_x, off_y) = ((side as f32 - w * scale) / 2.0, (side as f32 - h * scale) / 2.0);
    // To pixels, with y pointing down.
    let polygons: Vec<Vec<(f32, f32)>> = outline
        .polygons
        .iter()
        .map(|poly| poly.iter().map(|&(x, y)| (off_x + (x - min_x) * scale, off_y + (max_y - y) * scale)).collect())
        .collect();

    // Scanlines: four sub-rows for each pixel row; between the crossings of a
    // row the inside is where the winding number is not zero.
    let side_us = side as usize;
    let mut coverage = vec![0f32; side_us * side_us];
    const SUB: usize = 4;
    for sub_row in 0..side_us * SUB {
        let y = (sub_row as f32 + 0.5) / SUB as f32;
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        for poly in &polygons {
            for i in 0..poly.len() {
                let (x0, y0) = poly[i];
                let (x1, y1) = poly[(i + 1) % poly.len()];
                if (y0 <= y) != (y1 <= y) {
                    let x = x0 + (y - y0) / (y1 - y0) * (x1 - x0);
                    crossings.push((x, if y1 > y0 { 1 } else { -1 }));
                }
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let row = sub_row / SUB;
        let mut winding = 0;
        for pair in crossings.windows(2) {
            winding += pair[0].1;
            if winding == 0 {
                continue;
            }
            let (from, to) = (pair[0].0.max(0.0), pair[1].0.min(side as f32));
            if to <= from {
                continue;
            }
            for px in (from.floor() as usize)..(to.ceil() as usize).min(side_us) {
                let overlap = (to.min(px as f32 + 1.0) - from.max(px as f32)).max(0.0);
                coverage[row * side_us + px] += overlap / SUB as f32;
            }
        }
    }
    let mut rgba = Vec::with_capacity(side_us * side_us * 4);
    for c in coverage {
        rgba.extend_from_slice(&[color[0], color[1], color[2], (c.clamp(0.0, 1.0) * 255.0).round() as u8]);
    }
    Some(Bitmap { side, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_phosphor::regular as icon;

    #[test]
    fn an_icon_is_drawn_inside_its_square() {
        for glyph in [icon::GEAR_SIX, icon::ARROWS_CLOCKWISE, icon::TERMINAL_WINDOW, icon::HARD_DRIVES, icon::BROADCAST]
        {
            let b = render(glyph, 36, [140, 140, 145]).expect("the glyph is in the font");
            assert_eq!(b.rgba.len(), 36 * 36 * 4);
            let alpha: Vec<u8> = b.rgba.chunks(4).map(|p| p[3]).collect();
            let solid = alpha.iter().filter(|a| **a > 200).count();
            let some = alpha.iter().filter(|a| **a > 0).count();
            // Something is drawn, not all of the square, and it reaches both edges.
            assert!(solid > 40 && some < 36 * 36, "{glyph:?}: {solid} solid, {some} touched");
            let row_has = |r: usize| alpha[r * 36..(r + 1) * 36].iter().any(|a| *a > 0);
            let col_has = |c: usize| (0..36).any(|r| alpha[r * 36 + c] > 0);
            // The longer side fills the square.
            let tall = (0..4).any(row_has) && (32..36).any(row_has);
            let wide = (0..4).any(col_has) && (32..36).any(col_has);
            assert!(tall || wide, "{glyph:?} does not fill the square");
        }
        assert!(render("\u{10fffd}", 36, [0, 0, 0]).is_none(), "a character the font does not have gives nothing");
    }

    /// The icons the menus use all exist in the font and are drawn.
    #[test]
    fn every_menu_icon_can_be_drawn() {
        for glyph in crate::menu::ICONS {
            assert!(render(glyph, 36, [140, 140, 145]).is_some(), "{glyph:?}");
        }
        for (key, glyph) in crate::console::SESSION_ICONS {
            assert!(render(glyph, 36, [140, 140, 145]).is_some(), "{key}");
        }
    }
}
