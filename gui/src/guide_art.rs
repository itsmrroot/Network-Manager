//! Pictures for the CLI guide, drawn with the app's own colours (so they
//! follow the dark and light themes, stay sharp at any size and add nothing
//! to the download). A book asks for one with `@diagram name | Caption`.

use eframe::egui::{self, Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::Palette;

/// The names a book can use.
pub const NAMES: &[&str] =
    &["console-cable", "trunk", "interface-name", "stp", "subnet-bits", "layers", "router-on-a-stick"];

/// The drawing area, in the picture's own units: the width is the same for
/// every picture and `s` scales it to the room.
const WIDTH: f32 = 640.0;

struct Canvas<'a> {
    painter: Painter,
    origin: Pos2,
    s: f32,
    p: &'a Palette,
}

impl Canvas<'_> {
    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.s
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.at(x, y), Vec2::new(w, h) * self.s)
    }

    /// A rounded box with a tinted fill and a border of `color`.
    fn boxed(&self, x: f32, y: f32, w: f32, h: f32, color: Color32) -> Rect {
        let r = self.rect(x, y, w, h);
        self.painter.rect(r, 6.0 * self.s, self.p.tint(color), Stroke::new(1.2, color), StrokeKind::Inside);
        r
    }

    fn text(&self, x: f32, y: f32, align: Align2, s: &str, size: f32, color: Color32) {
        // Never smaller than can be read.
        let font = FontId::proportional((size * self.s).max(9.5));
        self.painter.text(self.at(x, y), align, s, font, color);
    }

    fn mono(&self, x: f32, y: f32, align: Align2, s: &str, size: f32, color: Color32) {
        let font = FontId::monospace((size * self.s).max(9.0));
        self.painter.text(self.at(x, y), align, s, font, color);
    }

    fn line(&self, from: (f32, f32), to: (f32, f32), color: Color32, width: f32) {
        self.painter.line_segment([self.at(from.0, from.1), self.at(to.0, to.1)], Stroke::new(width, color));
    }

    fn dashed(&self, from: (f32, f32), to: (f32, f32), color: Color32, width: f32) {
        let (a, b) = (self.at(from.0, from.1), self.at(to.0, to.1));
        let n = ((b - a).length() / (9.0 * self.s)).max(1.0) as usize;
        for i in 0..n {
            if i % 2 == 0 {
                let t0 = i as f32 / n as f32;
                let t1 = (i + 1) as f32 / n as f32;
                self.painter.line_segment([a.lerp(b, t0), a.lerp(b, t1)], Stroke::new(width, color));
            }
        }
    }

    fn dot(&self, x: f32, y: f32, r: f32, color: Color32) {
        self.painter.circle_filled(self.at(x, y), r * self.s, color);
    }

    /// A device: a box with its name, and a small second line.
    fn device(&self, [x, y, w, h]: [f32; 4], color: Color32, name: &str, sub: &str) {
        self.boxed(x, y, w, h, color);
        let cx = x + w / 2.0;
        if sub.is_empty() {
            self.text(cx, y + h / 2.0, Align2::CENTER_CENTER, name, 13.5, self.p.text);
        } else {
            self.text(cx, y + h / 2.0 - 8.0, Align2::CENTER_CENTER, name, 13.5, self.p.text);
            self.text(cx, y + h / 2.0 + 9.0, Align2::CENTER_CENTER, sub, 11.0, self.p.weak);
        }
    }

    fn port(&self, x: f32, y: f32, w: f32, h: f32, fill: Color32, edge: Color32) {
        self.painter.rect(self.rect(x, y, w, h), 2.0 * self.s, fill, Stroke::new(1.0, edge), StrokeKind::Inside);
    }
}

/// The height each picture needs, in its own units.
fn height(name: &str) -> f32 {
    match name {
        "console-cable" => 170.0,
        "trunk" => 230.0,
        "interface-name" => 218.0,
        "stp" => 238.0,
        "subnet-bits" => 175.0,
        "layers" => 250.0,
        "router-on-a-stick" => 225.0,
        _ => 0.0,
    }
}

/// Draws a picture, or a note when the book names one that does not exist.
pub fn draw(ui: &mut Ui, p: &Palette, name: &str) {
    let h = height(name);
    if h == 0.0 {
        ui.label(egui::RichText::new(format!("[{name}]")).color(p.weak));
        return;
    }
    let width = ui.available_width().min(760.0);
    let s = (width / WIDTH).clamp(0.45, 1.25);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h * s), Sense::hover());
    let c = Canvas { painter: ui.painter_at(rect), origin: rect.min + Vec2::new((width - WIDTH * s) / 2.0, 0.0), s, p };
    c.painter.rect(rect, 8.0, p.card, Stroke::new(1.0, p.border), StrokeKind::Inside);
    match name {
        "console-cable" => console_cable(&c),
        "trunk" => trunk(&c),
        "interface-name" => interface_name(&c),
        "stp" => stp(&c),
        "subnet-bits" => subnet_bits(&c),
        "layers" => layers(&c),
        _ => router_on_a_stick(&c),
    }
}

fn console_cable(c: &Canvas) {
    let p = c.p;
    c.device([20.0, 40.0, 130.0, 70.0], p.accent, "Your computer", "Network Manager");
    c.device([215.0, 55.0, 120.0, 40.0], p.weak, "USB-serial", "adapter");
    c.line((150.0, 75.0), (215.0, 75.0), p.text, 2.0);
    c.text(182.0, 64.0, Align2::CENTER_CENTER, "USB", 11.0, p.weak);
    // The switch with its ports and the console port.
    c.boxed(430.0, 30.0, 190.0, 90.0, p.success);
    c.text(525.0, 44.0, Align2::CENTER_CENTER, "Switch", 13.5, p.text);
    for i in 0..8 {
        c.port(445.0 + i as f32 * 14.0, 62.0, 11.0, 11.0, p.card_alt, p.weak);
    }
    c.port(562.0, 58.0, 52.0, 20.0, p.tint(p.warning), p.warning);
    c.text(588.0, 68.0, Align2::CENTER_CENTER, "CONSOLE", 8.0, p.warning);
    c.text(525.0, 112.0, Align2::CENTER_CENTER, "look for the port marked “Console”", 10.5, p.weak);
    // The cable runs under the ports and ends in the console port.
    for (a, b) in [((335.0, 75.0), (400.0, 94.0)), ((400.0, 94.0), (588.0, 94.0)), ((588.0, 94.0), (588.0, 78.0))] {
        c.line(a, b, p.warning, 3.0);
    }
    c.text(
        420.0,
        150.0,
        Align2::CENTER_CENTER,
        "Console cable: RJ-45 “rollover” (light blue), or a USB console cable",
        11.5,
        p.weak,
    );
    c.line((420.0, 138.0), (420.0, 98.0), p.border, 1.0);
    c.text(20.0, 150.0, Align2::LEFT_CENTER, "9600 baud, 8N1", 11.5, p.weak);
}

fn trunk(c: &Canvas) {
    let p = c.p;
    let (v10, v20) = (p.accent, p.warning);
    c.device([50.0, 18.0, 200.0, 48.0], p.success, "Switch A", "");
    c.device([390.0, 18.0, 200.0, 48.0], p.success, "Switch B", "");
    c.line((250.0, 42.0), (390.0, 42.0), p.text, 4.0);
    c.text(320.0, 26.0, Align2::CENTER_CENTER, "Trunk", 12.5, p.text);
    c.text(320.0, 58.0, Align2::CENTER_CENTER, "VLAN 10 + 20, tagged", 10.5, p.weak);
    for (x, color, name, vlan) in [
        (60.0, v10, "PC 1", "VLAN 10"),
        (160.0, v20, "PC 2", "VLAN 20"),
        (400.0, v10, "PC 3", "VLAN 10"),
        (500.0, v20, "PC 4", "VLAN 20"),
    ] {
        let sw_x = if x < 300.0 { 150.0 } else { 490.0 };
        c.line((sw_x, 66.0), (x + 40.0, 150.0), color, 2.0);
        c.device([x, 150.0, 80.0, 44.0], color, name, vlan);
    }
    c.text(150.0, 212.0, Align2::CENTER_CENTER, "Access ports: one VLAN, untagged", 11.0, p.weak);
    c.text(490.0, 212.0, Align2::CENTER_CENTER, "PC 1 and PC 3 talk; PC 2 and PC 4 talk", 11.0, p.weak);
    c.dot(270.0, 100.0, 4.0, v10);
    c.text(280.0, 100.0, Align2::LEFT_CENTER, "VLAN 10", 11.0, p.weak);
    c.dot(270.0, 118.0, 4.0, v20);
    c.text(280.0, 118.0, Align2::LEFT_CENTER, "VLAN 20", 11.0, p.weak);
}

fn interface_name(c: &Canvas) {
    let p = c.p;
    c.boxed(20.0, 14.0, 600.0, 92.0, p.success);
    // 24 ports in two rows: odd numbers above, even below, as on a real switch.
    for col in 0..12 {
        for row in 0..2 {
            let n = col * 2 + row + 1;
            let chosen = n == 3;
            let (x, y) = (36.0 + col as f32 * 34.0, 28.0 + row as f32 * 36.0);
            c.port(x, y, 26.0, 24.0, if chosen { p.accent } else { p.card_alt }, if chosen { p.text } else { p.weak });
            c.text(
                x + 13.0,
                y + 12.0,
                Align2::CENTER_CENTER,
                &n.to_string(),
                10.0,
                if chosen { p.on_accent } else { p.weak },
            );
        }
    }
    for i in 0..4 {
        let x = 470.0 + i as f32 * 36.0;
        c.port(x, 40.0, 28.0, 28.0, p.tint(p.warning), p.warning);
        c.text(x + 14.0, 54.0, Align2::CENTER_CENTER, &(i + 1).to_string(), 10.0, p.warning);
    }
    c.text(542.0, 28.0, Align2::CENTER_CENTER, "Uplinks: Gi1/1/1 – 4", 10.5, p.weak);
    // The name of port 3, taken apart.
    c.mono(320.0, 128.0, Align2::CENTER_CENTER, "GigabitEthernet 1 / 0 / 3", 17.0, p.text);
    for (i, (key, meaning)) in [
        ("GigabitEthernet", "the kind of port (short: Gi)"),
        ("1", "switch number in the stack"),
        ("0", "module: 0 for the built-in ports"),
        ("3", "port number (the highlighted one)"),
    ]
    .iter()
    .enumerate()
    {
        let y = 150.0 + i as f32 * 15.0;
        c.mono(170.0, y, Align2::LEFT_CENTER, key, 11.0, p.accent);
        c.text(300.0, y, Align2::LEFT_CENTER, meaning, 11.0, p.weak);
    }
}

fn stp(c: &Canvas) {
    let p = c.p;
    c.device([250.0, 14.0, 140.0, 48.0], p.success, "Root bridge", "lowest bridge ID");
    c.device([50.0, 150.0, 140.0, 48.0], p.accent, "Switch B", "");
    c.device([450.0, 150.0, 140.0, 48.0], p.accent, "Switch C", "");
    c.line((290.0, 62.0), (130.0, 150.0), p.success, 2.5);
    c.line((350.0, 62.0), (510.0, 150.0), p.success, 2.5);
    c.text(180.0, 96.0, Align2::CENTER_CENTER, "forwarding", 11.0, p.success);
    c.text(460.0, 96.0, Align2::CENTER_CENTER, "forwarding", 11.0, p.success);
    c.dashed((190.0, 174.0), (450.0, 174.0), p.danger, 2.5);
    c.text(320.0, 160.0, Align2::CENTER_CENTER, "this link would make a loop", 11.0, p.weak);
    // The blocked end: a cross on the link.
    c.line((392.0, 166.0), (408.0, 182.0), p.danger, 3.0);
    c.line((392.0, 182.0), (408.0, 166.0), p.danger, 3.0);
    c.text(400.0, 196.0, Align2::CENTER_CENTER, "blocked port", 11.0, p.danger);
    c.text(320.0, 224.0, Align2::CENTER_CENTER, "STP keeps the link, but only as a spare", 10.5, p.weak);
}

fn subnet_bits(c: &Canvas) {
    let p = c.p;
    let octets = ["192", "168", "1", "37"];
    c.text(
        320.0,
        16.0,
        Align2::CENTER_CENTER,
        "192.168.1.37 / 24 : the mask 255.255.255.0 written in bits",
        12.5,
        p.text,
    );
    let (x0, cell, gap) = (46.0, 16.0, 10.0);
    for (o, octet) in octets.iter().enumerate() {
        let ox = x0 + o as f32 * (8.0 * cell + gap);
        c.text(ox + 4.0 * cell, 40.0, Align2::CENTER_CENTER, octet, 14.0, p.text);
        for b in 0..8 {
            let net = o < 3;
            let color = if net { p.accent } else { p.warning };
            let x = ox + b as f32 * cell;
            c.painter.rect(
                c.rect(x, 54.0, cell - 1.0, 26.0),
                2.0 * c.s,
                p.tint(color),
                Stroke::new(1.0, color),
                StrokeKind::Inside,
            );
            c.mono(x + cell / 2.0, 67.0, Align2::CENTER_CENTER, if net { "1" } else { "0" }, 11.0, color);
        }
        c.text(ox + 4.0 * cell, 94.0, Align2::CENTER_CENTER, if o < 3 { "255" } else { "0" }, 12.5, p.weak);
    }
    let net_end = x0 + 3.0 * (8.0 * cell + gap) - gap;
    let host_start = net_end + gap;
    let host_end = host_start + 8.0 * cell;
    c.line((x0, 112.0), (net_end, 112.0), p.accent, 2.5);
    c.line((host_start, 112.0), (host_end, 112.0), p.warning, 2.5);
    c.text(
        (x0 + net_end) / 2.0,
        130.0,
        Align2::CENTER_CENTER,
        "Network part: 24 bits (the ones), written /24",
        12.0,
        p.accent,
    );
    c.text((host_start + host_end) / 2.0, 130.0, Align2::CENTER_CENTER, "Host part: 8 bits", 12.0, p.warning);
    c.text(
        320.0,
        156.0,
        Align2::CENTER_CENTER,
        "2 to the power of 8 = 256 addresses, minus the network and broadcast address = 254 hosts",
        11.0,
        p.weak,
    );
}

fn layers(c: &Canvas) {
    let p = c.p;
    let rows = [
        ("7", "Application", "Data"),
        ("6", "Presentation", "Data"),
        ("5", "Session", "Data"),
        ("4", "Transport", "Segment"),
        ("3", "Network", "Packet"),
        ("2", "Data link", "Frame"),
        ("1", "Physical", "Bits"),
    ];
    c.text(20.0, 12.0, Align2::LEFT_CENTER, "OSI model", 11.5, p.weak);
    c.text(250.0, 12.0, Align2::LEFT_CENTER, "TCP/IP", 11.5, p.weak);
    c.text(375.0, 12.0, Align2::LEFT_CENTER, "Data unit", 11.5, p.weak);
    c.text(470.0, 12.0, Align2::LEFT_CENTER, "Seen on", 11.5, p.weak);
    for (i, (n, name, unit)) in rows.iter().enumerate() {
        let y = 24.0 + i as f32 * 31.0;
        let color = match i {
            0..=2 => p.accent,
            3 => p.success,
            4 => p.warning,
            _ => p.weak,
        };
        c.boxed(20.0, y, 215.0, 27.0, color);
        c.text(34.0, y + 13.5, Align2::LEFT_CENTER, n, 13.0, color);
        c.text(56.0, y + 13.5, Align2::LEFT_CENTER, name, 13.0, p.text);
        c.text(375.0, y + 13.5, Align2::LEFT_CENTER, unit, 12.5, p.text);
    }
    c.boxed(250.0, 24.0, 110.0, 89.0, p.accent);
    c.text(305.0, 68.0, Align2::CENTER_CENTER, "Application", 12.5, p.text);
    c.boxed(250.0, 117.0, 110.0, 27.0, p.success);
    c.text(305.0, 130.0, Align2::CENTER_CENTER, "Transport", 12.5, p.text);
    c.boxed(250.0, 148.0, 110.0, 27.0, p.warning);
    c.text(305.0, 161.0, Align2::CENTER_CENTER, "Internet", 12.5, p.text);
    c.boxed(250.0, 179.0, 110.0, 58.0, p.weak);
    c.text(305.0, 208.0, Align2::CENTER_CENTER, "Network access", 12.5, p.text);
    c.text(470.0, 130.0, Align2::LEFT_CENTER, "TCP, UDP: ports", 11.5, p.success);
    c.text(470.0, 161.0, Align2::LEFT_CENTER, "Router: IP addresses", 11.5, p.warning);
    c.text(470.0, 192.0, Align2::LEFT_CENTER, "Switch: MAC addresses, VLANs", 11.5, p.text);
    c.text(470.0, 223.0, Align2::LEFT_CENTER, "Cables, hubs, Wi-Fi radio", 11.5, p.weak);
}

fn router_on_a_stick(c: &Canvas) {
    let p = c.p;
    let (v10, v20) = (p.accent, p.warning);
    c.device([170.0, 14.0, 300.0, 62.0], p.success, "Router", "Gi0/0.10 = VLAN 10     Gi0/0.20 = VLAN 20");
    c.line((320.0, 76.0), (320.0, 130.0), p.text, 4.0);
    c.text(335.0, 103.0, Align2::LEFT_CENTER, "one trunk cable", 11.5, p.text);
    c.device([225.0, 130.0, 190.0, 46.0], p.success, "Switch", "");
    c.line((225.0, 153.0), (130.0, 153.0), v10, 2.0);
    c.line((415.0, 153.0), (510.0, 153.0), v20, 2.0);
    c.device([50.0, 130.0, 80.0, 46.0], v10, "PC 1", "VLAN 10");
    c.device([510.0, 130.0, 80.0, 46.0], v20, "PC 2", "VLAN 20");
    c.text(
        320.0,
        198.0,
        Align2::CENTER_CENTER,
        "Traffic between the VLANs goes up to the router and back down the same cable",
        11.0,
        p.weak,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every picture can be drawn, in both themes and at small and large widths.
    #[test]
    fn every_picture_can_be_drawn() {
        for dark in [true, false] {
            let p = Palette::new(dark, false, crate::theme::Accent::Blue);
            for width in [220.0, 460.0, 900.0] {
                let ctx = egui::Context::default();
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 900.0))),
                        ..Default::default()
                    },
                    |ui| {
                        for name in NAMES {
                            draw(ui, &p, name);
                        }
                    },
                );
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn every_name_has_a_height() {
        for name in NAMES {
            assert!(height(name) > 0.0, "{name}");
        }
    }
}
