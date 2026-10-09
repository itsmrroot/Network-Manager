//! The About page.

use eframe::egui::{self, Align, Layout, RichText, Ui, Vec2};
use egui_phosphor::regular as icon;
use netmgr::{POWERED_BY, REPO};

use crate::i18n::{tr, trf, trl};
use crate::theme::{self, Palette, icon_label};
use crate::update::Updater;

pub fn page(ui: &mut Ui, p: &Palette, logo: &egui::TextureHandle, updater: &mut Updater) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(logo).fit_to_exact_size(Vec2::splat(88.0)));
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.label(theme::semibold(tr("Network Manager"), 24.0).color(p.text));
                    ui.label(RichText::new(trf("Version {version}", &[("version", &env!("CARGO_PKG_VERSION"))])).color(p.weak));
                    ui.add_space(4.0);
                    theme::pill(ui, p, POWERED_BY, p.accent);
                });
            });
            ui.add_space(10.0);
            updater.status(ui, p);
            ui.add_space(12.0);
            theme::paragraph(
                ui,
                trl("Manages your network adapters — IP addresses, DNS servers and MAC addresses — shows your Wi-Fi network and saved Wi-Fi passwords, finds the devices on your network, and brings the everyday tools of network engineers together in one app. On Windows, macOS and Linux."),
                14.5,
                p.text,
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.hyperlink_to(icon_label(icon::GITHUB_LOGO, "Project page"), REPO);
                ui.add_space(12.0);
                ui.hyperlink_to(icon_label(icon::DOWNLOAD_SIMPLE, "Latest version"), format!("{REPO}/releases/latest"));
                ui.add_space(12.0);
                ui.hyperlink_to(icon_label(icon::SCALES, "MIT License"), format!("{REPO}/blob/main/LICENSE"));
            });
        });
        ui.add_space(14.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::SHIELD_CHECK, tr("Privacy"));
            ui.add_space(4.0);
            for line in [
                trl("Settings are changed with the system's own tools (netsh, networksetup, nmcli): nothing is installed in the background."),
                trl("Wi-Fi passwords are only read when you ask, and never sent anywhere."),
                trl("The device scan stays on your own network. Maker names come from a list built into the app."),
                trl("Only the public IP lookup (ipinfo.io), the speed test (Cloudflare) and the update check (GitHub) use the internet; each can be avoided."),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::CHECK).color(p.success));
                    theme::paragraph(ui, line, 14.5, p.text);
                });
            }
        });
        ui.add_space(14.0);
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.label(RichText::new(format!("© 2026 Bashar Salmo · {POWERED_BY}")).color(p.weak).size(12.5));
        });
    });
}
