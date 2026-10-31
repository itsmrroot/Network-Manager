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
                ui.hyperlink_to(icon_label(icon::SCALES, "PolyForm Noncommercial License"), format!("{REPO}/blob/main/LICENSE"));
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
                trl("The connection history, when you turn it on, pings your router and 1.1.1.1 and looks up example.com now and then; the results stay on this computer."),
                trl("Only the public IP lookup (ipinfo.io), the VPN leak check (ipinfo.io and your DNS servers), the speed test (Cloudflare) and the update check (GitHub) use the internet; each can be avoided."),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::CHECK).color(p.success));
                    theme::paragraph(ui, line, 14.5, p.text);
                });
            }
        });
        ui.add_space(14.0);
        legal_card(ui, p);
        ui.add_space(14.0);
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.label(RichText::new(format!("© 2026 Bashar Salmo · {POWERED_BY}")).color(p.weak).size(12.5));
        });
    });
}

/// The open-source software inside the app (made by `scripts/third-party.py`).
const THIRD_PARTY: &str = include_str!("../assets/third-party.txt");

/// What goes with a problem report: the version, the system and the end of the problem
/// log, without the addresses of this computer and its network.
fn diagnostics() -> String {
    let dir = netmgr::profiles::config_dir();
    let log = std::fs::read_to_string(dir.join("problems.log")).unwrap_or_default();
    let text = format!(
        "Network Manager {}\nSystem: {}\nOpenGL instead of wgpu: {}\nSettings folder: {}\n\nProblem log (last 30 lines):\n{}\n",
        env!("CARGO_PKG_VERSION"),
        netmgr::report::os_name(),
        if dir.join("use-opengl").exists() { "yes" } else { "no" },
        dir.display(),
        netmgr::diag::tail(&log, 30),
    );
    let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok();
    netmgr::diag::redact(&text, home.as_deref())
}

/// Licences and what to send with a report.
fn legal_card(ui: &mut Ui, p: &Palette) {
    let list_id = egui::Id::new("third-party-open");
    let copied_id = egui::Id::new("diagnostics-copied");
    theme::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        theme::section_title(ui, p, icon::SCALES, tr("Legal and support"));
        ui.add_space(4.0);
        theme::paragraph(
            ui,
            trl(
                "Network Manager is free for noncommercial use under the PolyForm Noncommercial License. It contains open-source software and fonts that keep their own licences.",
            ),
            14.5,
            p.text,
        );
        theme::paragraph(
            ui,
            trl(
                "Diagnostic information is the version, your system and the end of the problem log, with IP and MAC addresses and your user folder hidden. It is copied only when you click, and sent nowhere.",
            ),
            14.5,
            p.weak,
        );
        ui.add_space(6.0);
        let now = ui.input(|i| i.time);
        let copied = ui.data(|d| d.get_temp::<f64>(copied_id)).is_some_and(|t| now - t < 3.0);
        ui.horizontal_wrapped(|ui| {
            if theme::secondary_button(ui, &icon_label(icon::FILE_TEXT, tr("Open-source licences"))).clicked() {
                ui.data_mut(|d| d.insert_temp(list_id, true));
            }
            let label = if copied { tr("Copied.") } else { tr("Copy diagnostic information") };
            if theme::secondary_button(ui, &icon_label(icon::CLIPBOARD_TEXT, label)).clicked() {
                ui.ctx().copy_text(diagnostics());
                ui.data_mut(|d| d.insert_temp(copied_id, now));
            }
        });
        if copied {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
    });
    let mut open = ui.data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false);
    if open {
        let lines: Vec<&str> = THIRD_PARTY.lines().collect();
        egui::Window::new(tr("Open-source licences")).open(&mut open).default_size([760.0, 520.0]).show(
            ui.ctx(),
            |ui| {
                egui::ScrollArea::both().auto_shrink(false).show_rows(ui, 16.0, lines.len(), |ui, rows| {
                    for line in &lines[rows] {
                        ui.add(
                            egui::Label::new(RichText::new(*line).monospace().size(12.0))
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    }
                });
            },
        );
        ui.data_mut(|d| d.insert_temp(list_id, open));
    }
}

#[cfg(test)]
mod tests {
    /// Every crate in Cargo.lock is in the list of open-source software.
    #[test]
    fn the_third_party_list_is_current() {
        let lock = include_str!("../../Cargo.lock");
        let list = super::THIRD_PARTY;
        let mut missing = Vec::new();
        let mut name = None;
        for line in lock.lines() {
            if let Some(n) = line.strip_prefix("name = \"") {
                name = Some(n.trim_end_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("version = \"") {
                let (Some(n), v) = (name.take(), v.trim_end_matches('"')) else { continue };
                // The app's own crates are not third-party software.
                if !n.starts_with("netmgr") && !list.contains(&format!("\n{n} {v} ")) {
                    missing.push(format!("{n} {v}"));
                }
            }
        }
        assert!(missing.is_empty(), "run scripts/third-party.py: {missing:?}");
    }

    /// `LICENSE` (Markdown, as GitHub shows it) as plain text, for the licence page of the
    /// installers, which show the file as it is: no `#`, `<…>`, `[…](#…)` or `**`.
    fn plain_license(markdown: &str) -> String {
        let mut out = String::new();
        for line in markdown.lines() {
            let mut line = line.trim_end().to_string();
            if let Some(heading) = line.strip_prefix("## ") {
                line = heading.to_uppercase();
            } else if let Some(title) = line.strip_prefix("# ") {
                line = title.to_string();
            } else if let Some(quote) = line.strip_prefix("> ") {
                line = format!("    {quote}");
            } else if line.starts_with("<http") && line.ends_with('>') {
                line = line[1..line.len() - 1].to_string();
            }
            // [text](#anchor) is just text.
            while let Some(at) = line.find("](#") {
                let (Some(open), Some(close)) = (line[..at].rfind('['), line[at..].find(')')) else { break };
                let text = line[open + 1..at].to_string();
                line.replace_range(open..at + close + 1, &text);
            }
            let line = line.replace("***", "").replace("**", "").replace('`', "");
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// The licence the installers show is made from `LICENSE`, so that the two cannot drift
    /// apart. After changing `LICENSE`: `NETMGR_WRITE_LICENSE=1 cargo test -p netmgr-gui licence`.
    #[test]
    fn the_installers_show_the_licence_as_plain_text() {
        let plain = plain_license(&include_str!("../../LICENSE").replace("\r\n", "\n"));
        if std::env::var_os("NETMGR_WRITE_LICENSE").is_some() {
            std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/license.txt"), &plain).expect("writable");
        }
        let shown = include_str!("../assets/license.txt").replace("\r\n", "\n");
        assert_eq!(shown, plain, "assets/license.txt is out of date");
        for markup in ["# ", "](#", "**", "`", "<http"] {
            assert!(!shown.contains(markup), "Markdown left in the licence: {markup:?}");
        }
        // Nothing of the terms is lost.
        for words in [
            "Required Notice: Copyright (c) 2026 Bashar Salmo",
            "PolyForm Noncommercial License 1.0.0",
            "No Liability",
            "Use means anything you do",
        ] {
            assert!(shown.to_lowercase().contains(&words.to_lowercase()), "{words}");
        }
    }
}
