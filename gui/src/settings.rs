//! User settings, persisted between runs, and the Settings page.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use serde::{Deserialize, Serialize};

use crate::theme::{self, Accent, Palette, icon_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
    /// Near-black with a deep blue top bar.
    Midnight,
}

impl ThemeChoice {
    pub fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark | ThemeChoice::Midnight => egui::ThemePreference::Dark,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Appearance
    pub theme: ThemeChoice,
    pub accent: Accent,
    pub ui_scale: f32,
    // Start
    pub lookup_public_ip: bool,
    pub check_updates: bool,
    // Adapters
    pub show_virtual: bool,
    // Devices
    pub scan_ports: bool,
    pub scan_names: bool,
    /// Names given to devices, by MAC address.
    pub device_labels: BTreeMap<String, String>,
    /// Every device seen before, by MAC address: others are marked "New".
    pub known_devices: BTreeSet<String>,
    // Tools
    /// 0: until stopped.
    pub ping_count: u32,
    /// Hosts typed into the tools, newest first.
    pub recent_hosts: Vec<String>,
    // Safety
    /// The MAC address each adapter had before this app first changed it.
    pub original_macs: BTreeMap<String, String>,
    // Servers and console
    /// Empty: a "TFTP" folder in the home folder.
    pub tftp_folder: String,
    pub serial_port: String,
    pub serial: netmgr::console::LineSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Midnight,
            accent: Accent::Blue,
            ui_scale: 1.0,
            lookup_public_ip: true,
            check_updates: true,
            show_virtual: false,
            scan_ports: true,
            scan_names: true,
            device_labels: BTreeMap::new(),
            known_devices: BTreeSet::new(),
            ping_count: 4,
            recent_hosts: Vec::new(),
            original_macs: BTreeMap::new(),
            tftp_folder: String::new(),
            serial_port: String::new(),
            serial: Default::default(),
        }
    }
}

impl Settings {
    /// Remembers a host typed into a tool.
    pub fn remember_host(&mut self, host: &str) {
        let host = host.trim();
        if host.is_empty() {
            return;
        }
        self.recent_hosts.retain(|h| h != host);
        self.recent_hosts.insert(0, host.to_string());
        self.recent_hosts.truncate(12);
    }
}

/// A setting: title and help on the left, the control on the right.
pub fn row(ui: &mut Ui, p: &Palette, title: &str, help: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 300.0).max(260.0));
            ui.label(RichText::new(title).color(p.text).size(14.5));
            if !help.is_empty() {
                theme::paragraph(ui, help, 12.5, p.weak);
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);
}

fn toggle(ui: &mut Ui, on: &mut bool) {
    let label = if *on { "On" } else { "Off" };
    ui.checkbox(on, label);
}

/// Draws the Settings page.
pub fn page(ui: &mut Ui, p: &Palette, s: &mut Settings) {
    theme::page_title(ui, p, "Settings", "Saved automatically.");

    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::PALETTE, "Appearance");
            ui.add_space(6.0);
            row(ui, p, "Theme", "Follow the system, or always light or dark.", |ui| {
                for (choice, label) in [
                    (ThemeChoice::Midnight, "Midnight"),
                    (ThemeChoice::Dark, "Dark"),
                    (ThemeChoice::Light, "Light"),
                    (ThemeChoice::System, "System"),
                ] {
                    ui.selectable_value(&mut s.theme, choice, label);
                }
            });
            row(ui, p, "Accent colour", "", |ui| {
                for a in Accent::ALL.iter().rev() {
                    let selected = s.accent == *a;
                    let text = RichText::new(if selected { icon::CHECK_CIRCLE } else { icon::CIRCLE })
                        .size(22.0)
                        .color(a.color());
                    if ui.add(egui::Button::new(text).frame(false)).on_hover_text(a.name()).clicked() {
                        s.accent = *a;
                    }
                }
            });
            row(ui, p, "Interface size", "Make everything larger or smaller (also Ctrl/⌘ + and −).", |ui| {
                // Applied when the mouse button is released: resizing during a
                // drag would move the slider away under the mouse.
                let held_id = egui::Id::new("interface-size-held");
                let current = (s.ui_scale * 100.0).round() as i32;
                let mut pct = ui.data(|d| d.get_temp::<i32>(held_id)).unwrap_or(current);
                ui.spacing_mut().slider_width = 240.0;
                let slider = ui.add(egui::Slider::new(&mut pct, 80..=150).step_by(10.0).suffix(" %"));
                if slider.is_pointer_button_down_on() {
                    ui.data_mut(|d| d.insert_temp(held_id, pct));
                } else {
                    ui.data_mut(|d| d.remove::<i32>(held_id));
                    if pct != current {
                        s.ui_scale = pct as f32 / 100.0;
                    }
                }
            });
        });
        ui.add_space(14.0);

        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::PLUGS_CONNECTED, "Adapters and devices");
            ui.add_space(6.0);
            row(
                ui,
                p,
                "Show virtual adapters",
                "Also list VPN, virtual machine, bridge and loopback adapters.",
                |ui| toggle(ui, &mut s.show_virtual),
            );
            row(
                ui,
                p,
                "Find device names",
                "Ask the router and the devices themselves for their names during a scan.",
                |ui| toggle(ui, &mut s.scan_names),
            );
            row(
                ui,
                p,
                "Check device services",
                "Try a few common ports on each device to tell printers, computers and phones apart. Slower.",
                |ui| toggle(ui, &mut s.scan_ports),
            );
            row(
                ui,
                p,
                "Forget known devices",
                "Every device is marked \"New\" until it was seen once. Names you gave are kept.",
                |ui| {
                    let n = s.known_devices.len();
                    if ui.add_enabled(n > 0, egui::Button::new(format!("Forget {n}"))).clicked() {
                        s.known_devices.clear();
                    }
                },
            );
        });
        ui.add_space(14.0);

        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::TOOLBOX, "Tools");
            ui.add_space(6.0);
            row(ui, p, "Pings per test", "0 keeps pinging until you click Stop.", |ui| {
                ui.add(egui::DragValue::new(&mut s.ping_count).range(0..=1000));
            });
            row(ui, p, "Recent addresses", "The hosts you typed into the tools.", |ui| {
                let n = s.recent_hosts.len();
                if ui.add_enabled(n > 0, egui::Button::new(format!("Clear {n}"))).clicked() {
                    s.recent_hosts.clear();
                }
            });
        });
        ui.add_space(14.0);

        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::GLOBE, "Internet");
            ui.add_space(6.0);
            row(
                ui,
                p,
                "Show the public IP address",
                "Asks ipinfo.io for this connection's public address, provider and location when the app starts.",
                |ui| toggle(ui, &mut s.lookup_public_ip),
            );
            row(
                ui,
                p,
                "Check for updates at start",
                "Asks GitHub for the latest version when the app starts. Nothing about you or your network is sent.",
                |ui| toggle(ui, &mut s.check_updates),
            );
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, &icon_label(icon::ARROW_COUNTER_CLOCKWISE, "Reset to defaults")).clicked() {
                // Device names and original MAC addresses are the user's data.
                *s = Settings {
                    device_labels: std::mem::take(&mut s.device_labels),
                    known_devices: std::mem::take(&mut s.known_devices),
                    original_macs: std::mem::take(&mut s.original_macs),
                    ..Settings::default()
                };
            }
        });
        ui.add_space(20.0);
    });
}
