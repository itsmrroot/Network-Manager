//! A manual IP address on Wi-Fi belongs to the adapter on macOS and
//! Windows, not to the network: after joining another Wi-Fi network it is
//! still there and nothing works. The app remembers the network a manual
//! address works on (by its router), offers automatic settings elsewhere,
//! and the manual address again when that network is back. (Linux keeps
//! settings per network itself.)

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use egui_phosphor::regular as icon;
use netmgr::adapters::{Adapter, Kind};
use netmgr::config::{self, IpSettings, Ipv4Mode};
use serde::{Deserialize, Serialize};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// The Wi-Fi network a manual address was seen working on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WifiManual {
    pub adapter: String,
    /// The router's MAC address on that network.
    pub router: String,
    /// The network's name, when the system tells it.
    pub ssid: Option<String>,
    pub settings: IpSettings,
}

/// How long a manual address may go without the router answering before
/// the app asks.
const SILENT: Duration = Duration::from_secs(20);

enum Ask {
    /// The manual address does not work here: switch to automatic?
    Automatic(Adapter),
    /// Back on the network the manual address was for: use it again?
    Manual(Adapter, WifiManual),
}

#[derive(Default)]
pub struct WifiIp {
    silent_since: Option<Instant>,
    /// What was asked already (not asked again while nothing changes).
    asked: Option<String>,
    ask: Option<Ask>,
    job: Option<Job<String>>,
    /// The router last seen answering the manual address.
    manual_router: Option<String>,
    /// Pinging the router before asking: its MAC address can be unknown
    /// while it answers (macOS hides MAC addresses without the Local
    /// Network permission).
    probe: Option<Job<bool>>,
    probed: Option<Adapter>,
    /// The router answered a ping: check again at this time.
    next_probe: Option<Instant>,
}

fn is_manual(a: &Adapter) -> bool {
    matches!(IpSettings::current(a).mode, Ipv4Mode::Static { .. })
}

impl WifiIp {
    /// Watches the Wi-Fi adapter (called every frame; cheap).
    pub fn tick(&mut self, ctx: &egui::Context, p: &Palette, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(m) => sh.toast(m),
                Err(e) => sh.fail(trl("The adapter could not be changed."), &e),
            }
            sh.refresh = true;
            sh.refresh_wifi = true;
        }
        if self.ask.is_some() {
            self.dialog(ctx, p);
            return;
        }
        if let Some(answers) = jobs::finished(&mut self.probe) {
            let a = self.probed.take();
            if answers.unwrap_or(false) {
                // Works, though its MAC address is hidden: look again later,
                // in case another network is joined meanwhile.
                self.next_probe = Some(Instant::now() + Duration::from_secs(60));
            } else if let Some(a) = a {
                // No answer from the router either: ask (once).
                self.asked = Some(format!("auto {}", a.id));
                self.ask = Some(Ask::Automatic(a));
                return;
            }
        }
        if !(cfg!(target_os = "macos") || cfg!(windows)) || self.job.is_some() || self.probe.is_some() {
            return;
        }
        let Some(a) = sh.adapters.iter().find(|a| a.kind == Kind::WiFi && a.up && !a.ipv4.is_empty()).cloned() else {
            self.silent_since = None;
            return;
        };
        let router = a.gateway_mac.map(|m| m.to_string());
        let ssid = sh.wifi.first().and_then(|w| w.ssid.clone());
        if is_manual(&a) {
            if let Some(router) = router {
                // It works here: remember this network.
                self.silent_since = None;
                self.asked = None;
                self.next_probe = None;
                self.manual_router = Some(router.clone());
                let m = WifiManual { adapter: a.id.clone(), router, ssid, settings: IpSettings::current(&a) };
                if sh.settings.wifi_manual.as_ref() != Some(&m) {
                    sh.settings.wifi_manual = Some(m);
                }
            } else if a.gateway.is_some() || sh.settings.wifi_manual.is_some() {
                // The router does not answer: probably another network.
                let since = *self.silent_since.get_or_insert_with(Instant::now);
                let key = format!("auto {}", a.id);
                let probe_due = self.next_probe.is_none_or(|t| Instant::now() >= t);
                if since.elapsed() > SILENT && probe_due && self.asked.as_deref() != Some(&key) {
                    match a.gateway {
                        Some(gw) => {
                            self.probed = Some(a);
                            self.probe = Some(Job::spawn(ctx, move |_, _| {
                                Ok((0..3)
                                    .any(|_| netmgr::icmp::ping(gw, Duration::from_secs(1)).is_ok_and(|r| r.is_some())))
                            }));
                        }
                        None => {
                            self.asked = Some(key);
                            self.ask = Some(Ask::Automatic(a));
                        }
                    }
                } else {
                    ctx.request_repaint_after(Duration::from_secs(2));
                }
            }
            return;
        }
        self.silent_since = None;
        // Switched to automatic on the very network the manual address
        // worked on: the person's own choice, so it is forgotten.
        // (Decided once the router is known again after the change.)
        if let Some(now) = &router
            && let Some(r) = self.manual_router.take()
            && &r == now
        {
            sh.settings.wifi_manual = None;
            return;
        }
        // Automatic now: back on the network of the manual address?
        if let (Some(m), Some(router)) = (sh.settings.wifi_manual.clone(), router)
            && m.adapter == a.id
            && m.router == router
        {
            let key = format!("manual {router}");
            if self.asked.as_deref() != Some(&key) {
                self.asked = Some(key);
                self.ask = Some(Ask::Manual(a, m));
            }
        }
    }

    fn dialog(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(ask) = &self.ask else { return };
        let (title, text, yes) = match ask {
            Ask::Automatic(a) => (
                tr("This Wi-Fi network needs automatic settings"),
                trf(
                    "{name} still uses the manual address {address}, but the router does not answer: the address was probably set for another network. Use automatic settings (DHCP) here?",
                    &[
                        ("name", &a.name),
                        ("address", &a.main_ipv4().map_or(String::new(), |(ip, pfx)| format!("{ip}/{pfx}"))),
                    ],
                ),
                tr("Use automatic settings"),
            ),
            Ask::Manual(_, m) => (
                tr("Back on the network of your manual address"),
                trf(
                    "You are back on {network}, where the manual address ({summary}) worked. Use it again?",
                    &[
                        ("network", &m.ssid.clone().unwrap_or_else(|| tr("Wi-Fi").to_string())),
                        ("summary", &m.settings.summary()),
                    ],
                ),
                tr("Use the manual address"),
            ),
        };
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::new("wifi-ip")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WIFI_HIGH).size(24.0).color(p.accent));
                ui.label(theme::semibold(title, 17.0).color(p.text));
            });
            ui.add_space(8.0);
            theme::paragraph(ui, &text, 14.0, p.text);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, p, &icon_label(icon::CHECK, yes), true).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr("Keep as it is")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(false);
        }
        let Some(yes) = answer else { return };
        let ask = self.ask.take().expect("shown");
        if !yes {
            return;
        }
        let (a, s) = match ask {
            Ask::Automatic(a) => {
                // Manual DNS servers stay: they work on any network.
                let dns = IpSettings::current(&a).dns;
                (a, IpSettings { dns, ..IpSettings::dhcp() })
            }
            Ask::Manual(a, m) => (a, m.settings),
        };
        self.job = Some(Job::spawn(ctx, move |_, _| {
            config::apply(&a, &s)?;
            Ok(trf("{name} was changed.", &[("name", &a.name)]))
        }));
    }
}
