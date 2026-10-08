//! Overview: the connection at a glance, live traffic, a step-by-step
//! connection check and a speed test.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Layout, RichText, Ui, Vec2};
use egui_phosphor::regular as icon;
use netmgr::adapters::{Kind, format_speed};
use netmgr::config;
use netmgr::internet::{self, Check, SpeedPhase, SpeedResult};
use netmgr::wifi;

use crate::app::{Nav, Page, Shared};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

#[derive(Default)]
pub struct Overview {
    diagnose: Option<Job<Vec<Check>>>,
    checks: Arc<Mutex<Vec<Check>>>,
    checked: bool,
    speed: Option<Job<SpeedResult>>,
    speed_live: Arc<Mutex<(Option<SpeedPhase>, f64, SpeedResult)>>,
    speed_result: Option<SpeedResult>,
    fix: Option<Job<&'static str>>,
    report: Option<Job<String>>,
}

impl Overview {
    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        self.poll(sh);
        ui.horizontal(|ui| {
            ui.vertical(|ui| theme::page_title(ui, p, "Overview", "Your connection at a glance."));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if let Some(job) = &self.report {
                    ui.spinner();
                    ui.label(RichText::new(job.progress.snapshot().message).color(p.weak).size(12.5));
                } else if theme::secondary_button(ui, &icon_label(icon::FILE_TEXT, "Network report"))
                    .on_hover_text("Save everything about this connection to a file, e.g. for a support ticket")
                    .clicked()
                {
                    let public = sh.settings.lookup_public_ip;
                    self.report = Some(Job::spawn(ui.ctx(), move |progress, _| {
                        Ok(netmgr::report::build(public, &|s| progress.set(0.0, s)))
                    }));
                }
            });
        });
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if sh.lan_denied {
                local_network_notice(ui, p);
                ui.add_space(12.0);
            }
            self.hero(ui, p, sh);
            ui.add_space(14.0);
            tiles(ui, p, sh);
            ui.add_space(14.0);
            ui.columns(2, |cols| {
                traffic(&mut cols[0], p, sh);
                self.speed_card(&mut cols[1], p);
            });
            if self.diagnose.is_some() || self.checked {
                ui.add_space(14.0);
                self.checks_card(ui, p);
            }
            ui.add_space(20.0);
        });
    }

    fn poll(&mut self, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.report) {
            match r {
                Ok(text) => {
                    let name = format!("network-report-{}.md", chrono::Local::now().format("%Y-%m-%d-%H%M"));
                    if let Some(path) =
                        rfd::FileDialog::new().add_filter("Markdown", &["md"]).set_file_name(name).save_file()
                    {
                        match std::fs::write(&path, text) {
                            Ok(()) => {
                                sh.toast(format!("Report saved to {}.", path.display()));
                                crate::app::open_path(&path.display().to_string());
                            }
                            Err(e) => sh.fail("The report could not be saved.", &e.into()),
                        }
                    }
                }
                Err(e) => sh.fail("The report could not be made.", &e),
            }
        }
        if let Some(Ok(_)) = jobs::finished(&mut self.diagnose) {
            self.checked = true;
        }
        if let Some(r) = jobs::finished(&mut self.speed) {
            match r {
                Ok(r) => self.speed_result = Some(r),
                Err(e) if e.to_string() == "stopped" => {}
                Err(e) => sh.fail("The speed test could not be completed.", &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.fix) {
            match r {
                Ok(msg) => {
                    sh.toast(msg);
                    sh.refresh = true;
                    sh.refresh_wifi = true;
                }
                Err(e) => sh.fail("That did not work.", &e),
            }
        }
    }

    fn hero(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let adapter = sh.default_adapter().cloned();
        let wifi = sh.wifi.first().cloned();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (glyph, color, title, sub) = match &adapter {
                    None if !sh.adapters_loaded => {
                        (icon::CIRCLE_NOTCH, p.weak, "Looking at your network…".to_string(), String::new())
                    }
                    None => (
                        icon::WIFI_SLASH,
                        p.danger,
                        "Not connected".to_string(),
                        "No adapter has a connection to a network.".to_string(),
                    ),
                    Some(a) => {
                        let ip = a.main_ipv4().map(|(ip, p)| format!("{ip}/{p}")).unwrap_or_default();
                        if a.kind == Kind::WiFi {
                            let name = wifi.as_ref().and_then(|w| w.ssid.clone());
                            let title = match name {
                                Some(n) => format!("Connected to {n}"),
                                None => "Connected to Wi-Fi".into(),
                            };
                            (icon::WIFI_HIGH, p.success, title, format!("{} · {ip}", a.name))
                        } else {
                            (
                                icon::PLUGS_CONNECTED,
                                p.success,
                                "Connected by cable".into(),
                                format!("{} · {ip}", a.name),
                            )
                        }
                    }
                };
                theme::icon_badge(ui, p, glyph, color, 64.0);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.label(theme::semibold(&title, 22.0).color(p.text));
                    ui.label(RichText::new(sub).color(p.weak).size(14.5));
                    if let Some(w) = &wifi
                        && adapter.as_ref().is_some_and(|a| a.kind == Kind::WiFi)
                    {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if let Some(q) = w.quality() {
                                theme::signal_bars(ui, p, q, 16.0);
                                let dbm = w.rssi.map_or(String::new(), |r| format!(" · {r} dBm"));
                                ui.label(
                                    RichText::new(format!("{} signal{dbm}", wifi::quality_label(q)))
                                        .color(p.weak)
                                        .size(13.0),
                                );
                            }
                            if let (Some(ch), Some(b)) = (w.channel, &w.band) {
                                ui.label(RichText::new(format!("· Channel {ch} ({b})")).color(p.weak).size(13.0));
                            }
                        });
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let busy = self.diagnose.is_some();
                    if theme::primary_button(ui, p, &icon_label(icon::STETHOSCOPE, "Check connection"), !busy).clicked()
                    {
                        self.start_diagnose(ui.ctx());
                    }
                    let fixing = self.fix.is_some();
                    if fixing {
                        ui.spinner();
                    }
                    ui.add_enabled_ui(!fixing, |ui| {
                        if theme::secondary_button(ui, &icon_label(icon::BROOM, "Flush DNS"))
                            .on_hover_text("Forget looked-up names, so they are looked up again")
                            .clicked()
                        {
                            self.fix = Some(Job::spawn(ui.ctx(), |_, _| {
                                config::flush_dns().map(|()| "The DNS cache was emptied.")
                            }));
                        }
                        if let Some(a) = adapter.clone()
                            && theme::secondary_button(ui, &icon_label(icon::ARROWS_CLOCKWISE, "Renew IP"))
                                .on_hover_text("Ask the router for a new address")
                                .clicked()
                        {
                            self.fix = Some(Job::spawn(ui.ctx(), move |_, _| {
                                config::renew(&a).map(|()| "A new address was requested.")
                            }));
                        }
                    });
                });
            });
        });
    }

    fn start_diagnose(&mut self, ctx: &egui::Context) {
        self.checks = Arc::new(Mutex::new(Vec::new()));
        self.checked = false;
        let checks = self.checks.clone();
        self.diagnose = Some(Job::spawn(ctx, move |progress, _| {
            Ok(internet::diagnose(&|c| {
                if let Ok(mut v) = checks.lock() {
                    v.push(c.clone());
                }
                progress.set(0.0, c.title);
            }))
        }));
    }

    fn checks_card(&mut self, ui: &mut Ui, p: &Palette) {
        let checks = self.checks.lock().map(|c| c.clone()).unwrap_or_default();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::STETHOSCOPE, "Connection check");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.diagnose.is_none() && ui.button("Close").clicked() {
                        self.checked = false;
                    }
                });
            });
            for c in &checks {
                ui.horizontal(|ui| {
                    let (g, color) = if c.ok { (icon::CHECK_CIRCLE, p.success) } else { (icon::X_CIRCLE, p.danger) };
                    ui.label(RichText::new(g).size(20.0).color(color));
                    ui.label(theme::semibold(c.title, 14.5).color(p.text));
                    ui.label(RichText::new(&c.detail).color(p.weak).size(14.0));
                });
                if let Some(a) = &c.advice {
                    ui.horizontal(|ui| {
                        ui.add_space(30.0);
                        theme::notice(ui, p, p.warning, icon::LIGHTBULB, a);
                    });
                }
            }
            if self.diagnose.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Checking…").color(p.weak));
                });
            } else if checks.iter().all(|c| c.ok) && !checks.is_empty() {
                ui.add_space(4.0);
                theme::notice(
                    ui,
                    p,
                    p.success,
                    icon::CHECK_CIRCLE,
                    "Everything works: this computer is connected to the internet.",
                );
            }
        });
    }

    fn speed_card(&mut self, ui: &mut Ui, p: &Palette) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(170.0);
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::SPEEDOMETER, "Speed test");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(job) = &self.speed {
                        if ui.button(icon_label(icon::STOP, "Stop")).clicked() {
                            job.stop();
                        }
                    } else if ui
                        .button(icon_label(icon::PLAY, if self.speed_result.is_some() { "Again" } else { "Start" }))
                        .clicked()
                    {
                        let live = Arc::new(Mutex::new((None, 0.0, SpeedResult::default())));
                        self.speed_live = live.clone();
                        self.speed = Some(Job::spawn(ui.ctx(), move |_, cancel| {
                            internet::speed_test(cancel, &|phase, v| {
                                if let Ok(mut l) = live.lock() {
                                    l.0 = Some(phase);
                                    l.1 = v;
                                    match phase {
                                        SpeedPhase::Ping => l.2.ping_ms = Some(v),
                                        SpeedPhase::Download => l.2.download_mbps = Some(v),
                                        SpeedPhase::Upload => l.2.upload_mbps = Some(v),
                                    }
                                }
                            })
                        }));
                    }
                });
            });
            let (phase, shown) = if self.speed.is_some() {
                let l = self.speed_live.lock().map(|l| (l.0, l.2)).unwrap_or_default();
                (l.0, Some(l.1))
            } else {
                (None, self.speed_result)
            };
            match shown {
                None => {
                    ui.add_space(8.0);
                    theme::paragraph(
                        ui,
                        "Measures how fast this connection downloads and uploads, using Cloudflare's speed test servers. Takes about 20 seconds.",
                        13.5,
                        p.weak,
                    );
                }
                Some(r) => {
                    ui.add_space(8.0);
                    ui.columns(3, |c| {
                        let fmt =
                            |v: Option<f64>, unit: &str| v.map_or("—".to_string(), |v| format!("{v:.0} {unit}"));
                        speed_stat(
                            &mut c[0],
                            p,
                            icon::TIMER,
                            "Ping",
                            &fmt(r.ping_ms, "ms"),
                            phase == Some(SpeedPhase::Ping),
                        );
                        speed_stat(
                            &mut c[1],
                            p,
                            icon::DOWNLOAD_SIMPLE,
                            "Download",
                            &fmt(r.download_mbps, "Mbit/s"),
                            phase == Some(SpeedPhase::Download),
                        );
                        speed_stat(
                            &mut c[2],
                            p,
                            icon::UPLOAD_SIMPLE,
                            "Upload",
                            &fmt(r.upload_mbps, "Mbit/s"),
                            phase == Some(SpeedPhase::Upload),
                        );
                    });
                    if self.speed.is_some() {
                        ui.add_space(6.0);
                        ui.add(egui::ProgressBar::new(0.0).animate(true).desired_height(6.0).fill(p.accent));
                    } else if let Some(j) = r.jitter_ms {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("Jitter {j:.1} ms — lower is better for calls and games."))
                                .color(p.weak)
                                .size(12.5),
                        );
                    }
                }
            }
        });
    }
}

fn speed_stat(ui: &mut Ui, p: &Palette, glyph: &str, label: &str, value: &str, active: bool) {
    ui.vertical(|ui| {
        ui.label(RichText::new(format!("{glyph}  {label}")).size(12.5).color(if active { p.accent } else { p.weak }));
        ui.label(theme::semibold(value, 20.0).color(p.text));
    });
}

fn tiles(ui: &mut Ui, p: &Palette, sh: &mut Shared) {
    let a = sh.default_adapter().cloned();
    let dash = || "—".to_string();
    let local = a.as_ref().and_then(|a| a.main_ipv4()).map_or_else(dash, |(ip, p)| format!("{ip}/{p}"));
    let ipv6 = a
        .as_ref()
        .and_then(|a| a.ipv6.iter().find(|(ip, _)| (ip.segments()[0] & 0xe000) == 0x2000).map(|(ip, _)| ip.to_string()))
        .unwrap_or_default();
    let (public, public_sub) = match &sh.public {
        _ if sh.public_loading => ("Looking up…".to_string(), String::new()),
        Some(Ok(info)) => {
            (info.ip.clone(), [info.provider(), info.place()].into_iter().flatten().collect::<Vec<_>>().join(" · "))
        }
        Some(Err(_)) => ("Not available".to_string(), "The internet could not be reached".to_string()),
        None => ("Hidden".to_string(), "Turn on in Settings → Internet".to_string()),
    };
    let gateway = a.as_ref().and_then(|a| a.gateway).map_or_else(dash, |g| g.to_string());
    let gateway_sub = a
        .as_ref()
        .and_then(|a| a.gateway_mac)
        .map(|m| m.vendor().map_or(m.to_string(), |v| format!("{v} · {m}")))
        .unwrap_or_default();
    let dns: Vec<String> = a
        .as_ref()
        .map(|a| a.dns.iter().filter(|d| !is_link_local(d)).map(IpAddr::to_string).collect())
        .unwrap_or_default();
    let dns_sub = match a.as_ref().and_then(|a| a.dhcp) {
        Some(true) => "From the router (automatic)",
        Some(false) => "Set by hand",
        None => "",
    };
    let speed = match (&a, sh.wifi.first()) {
        (Some(a), Some(w)) if a.kind == Kind::WiFi && w.rate_mbps.is_some() => {
            format!("{:.0} Mbit/s", w.rate_mbps.unwrap_or(0.0))
        }
        (Some(a), _) => a.speed_bps.map_or_else(dash, format_speed),
        _ => dash(),
    };
    let speed_sub = match (&a, sh.wifi.first()) {
        (Some(a), Some(w)) if a.kind == Kind::WiFi => w.standard.clone().unwrap_or_default(),
        (Some(a), _) => a.kind.label().to_string(),
        _ => String::new(),
    };
    let (mac, mac_sub) = match a.as_ref().and_then(|a| a.mac) {
        Some(m) if m.is_local() => (m.to_string(), "Private address (set by software)".to_string()),
        Some(m) => (m.to_string(), m.vendor().unwrap_or("Unknown maker").to_string()),
        None if sh.lan_denied => ("Hidden by macOS".to_string(), "Allow Local Network access".to_string()),
        None => (dash(), String::new()),
    };

    let items = [
        (icon::DESKTOP, "Local IP address", local, ipv6),
        (icon::GLOBE_HEMISPHERE_WEST, "Public IP address", public, public_sub),
        (icon::BROADCAST, "Router (gateway)", gateway, gateway_sub),
        (
            icon::LIST_MAGNIFYING_GLASS,
            "DNS servers",
            if dns.is_empty() { dash() } else { dns.join(", ") },
            dns_sub.to_string(),
        ),
        (icon::LIGHTNING, "Link speed", speed, speed_sub),
        (icon::FINGERPRINT, "MAC address", mac, mac_sub),
    ];
    let cols = if ui.available_width() > 900.0 { 3 } else { 2 };
    for row in items.chunks(cols) {
        ui.columns(cols, |c| {
            for (i, (g, label, value, sub)) in row.iter().enumerate() {
                theme::tile(&mut c[i], p, g, label, value, sub);
                let r = c[i].min_rect();
                if *label == "Public IP address" && sh.public.is_some() {
                    let resp = c[i].interact(r, egui::Id::new("public-ip-tile"), egui::Sense::click());
                    if resp.on_hover_text("Click to copy").clicked() {
                        c[i].ctx().copy_text(value.clone());
                        sh.toast("Public IP address copied.");
                    }
                }
                if *label == "Local IP address"
                    && let Some(a) = &a
                {
                    let resp = c[i].interact(r, egui::Id::new("local-ip-tile"), egui::Sense::click());
                    if resp.on_hover_text("Show the adapter").clicked() {
                        sh.nav = Some(Nav::Adapter(a.id.clone()));
                    }
                }
            }
        });
        ui.add_space(8.0);
    }
    if a.is_none() && sh.adapters_loaded {
        theme::notice(
            ui,
            p,
            p.warning,
            icon::WARNING,
            "Not connected: plug in a cable or join a Wi-Fi network, then click \"Check connection\".",
        );
        if ui.link("Open the Wi-Fi page").clicked() {
            sh.nav = Some(Nav::Page(Page::Wifi));
        }
    }
}

fn is_link_local(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v) if v.is_unicast_link_local())
}

fn traffic(ui: &mut Ui, p: &Palette, sh: &Shared) {
    theme::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(170.0);
        theme::section_title(ui, p, icon::PULSE, "Live traffic");
        let t = &sh.traffic;
        let down = t.down.back().copied().unwrap_or(0.0);
        let up = t.up.back().copied().unwrap_or(0.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon::DOWNLOAD_SIMPLE).color(p.accent));
            ui.label(theme::semibold(rate(down), 16.0).color(p.text));
            ui.add_space(16.0);
            ui.label(RichText::new(icon::UPLOAD_SIMPLE).color(p.deep));
            ui.label(theme::semibold(rate(up), 16.0).color(p.text));
        });
        ui.add_space(6.0);
        let down: Vec<f64> = t.down.iter().copied().collect();
        let up: Vec<f64> = t.up.iter().copied().collect();
        let top = down.iter().chain(up.iter()).copied().fold(1000.0, f64::max);
        let w = ui.available_width();
        let size = Vec2::new(w, 70.0);
        let start = ui.cursor().min;
        theme::sparkline(ui, &down, p.accent, size, Some(top));
        // The upload line over the same area.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(start, size)));
        theme::sparkline(&mut child, &up, p.deep, size, Some(top));
        if down.len() < 2 {
            ui.label(RichText::new("Measuring…").color(p.weak).size(12.5));
        }
    });
}

/// Bits per second, short.
fn rate(bps: f64) -> String {
    if bps >= 1e6 { format!("{:.1} Mbit/s", bps / 1e6) } else { format!("{:.0} kbit/s", bps / 1e3) }
}

/// macOS hides MAC addresses and devices until the app may use the local network.
pub fn local_network_notice(ui: &mut Ui, p: &Palette) {
    theme::notice(
        ui,
        p,
        p.warning,
        icon::SHIELD_WARNING,
        "macOS hides MAC addresses and the devices on your network from this app. Turn on Network Manager in \
         System Settings → Privacy & Security → Local Network, then restart the app.",
    );
    if ui.link(icon_label(icon::ARROW_SQUARE_OUT, "Open Local Network settings")).clicked() {
        crate::app::open_path("x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork");
    }
}
