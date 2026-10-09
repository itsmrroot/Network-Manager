//! Adapters: every network adapter with its settings, and changing its IP
//! address, DNS servers and MAC address.

use std::net::{IpAddr, Ipv4Addr};

use eframe::egui::{self, Align, Color32, Layout, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::adapters::{Adapter, Kind, format_bytes, format_speed, mask_to_prefix, prefix_to_mask};
use netmgr::config::{self, DNS_PRESETS, IpSettings, Ipv4Mode};
use netmgr::mac::{self, Mac};
use netmgr::profiles::{self, Profile};

use crate::app::{Nav, Shared};
use crate::i18n::{tr, tr_dyn, trf, trl, trlf};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// What finished jobs should do.
enum Done {
    /// IP settings applied; keep these to undo.
    Applied(Box<(Adapter, IpSettings)>),
    Message(String),
}

#[derive(Default)]
pub struct AdaptersPage {
    selected: Option<String>,
    ip: Option<IpEditor>,
    mac: Option<MacEditor>,
    job: Option<Job<Done>>,
    /// The adapter and the settings it had before the last change.
    undo: Option<(Adapter, IpSettings)>,
    permanent: Option<Job<(String, Option<Mac>)>>,
}

pub fn kind_icon(k: Kind) -> &'static str {
    match k {
        Kind::WiFi => icon::WIFI_HIGH,
        Kind::Ethernet => icon::PLUGS_CONNECTED,
        Kind::Cellular => icon::CELL_SIGNAL_FULL,
        Kind::Vpn => icon::LOCK,
        Kind::Bridge => icon::TREE_STRUCTURE,
        Kind::Loopback => icon::ARROWS_CLOCKWISE,
        Kind::Virtual | Kind::Other => icon::CPU,
    }
}

pub fn status_color(p: &Palette, a: &Adapter) -> Color32 {
    if a.disabled {
        p.weak
    } else if !a.up {
        p.warning
    } else if a.self_assigned() {
        p.danger
    } else {
        p.success
    }
}

impl AdaptersPage {
    pub fn select(&mut self, id: &str) {
        self.selected = Some(id.to_string());
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        self.poll(sh);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                theme::page_title(
                    ui,
                    p,
                    tr("Adapters"),
                    tr("Change IP and MAC addresses, DNS servers, and turn adapters on or off."),
                )
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if theme::secondary_button(ui, &icon_label(icon::ARROWS_CLOCKWISE, "Refresh")).clicked() {
                    sh.refresh = true;
                }
            });
        });
        if sh.lan_denied {
            crate::overview::local_network_notice(ui, p);
            ui.add_space(10.0);
        }
        if let Some((a, _)) = &self.undo {
            let name = a.name.clone();
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::CHECK_CIRCLE).color(p.success).size(18.0));
                    ui.label(
                        RichText::new(trf("The settings of {name} were changed.", &[("name", &name)])).color(p.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(tr("Keep")).clicked() {
                            self.undo = None;
                        } else if ui
                            .add_enabled(
                                self.job.is_none(),
                                egui::Button::new(icon_label(icon::ARROW_COUNTER_CLOCKWISE, tr("Undo"))),
                            )
                            .on_hover_text(tr("Go back to the settings it had before"))
                            .clicked()
                            && let Some((a, prev)) = self.undo.take()
                        {
                            self.job = Some(Job::spawn(ui.ctx(), move |_, _| {
                                config::apply(&a, &prev)?;
                                Ok(Done::Message(trf("{name} has its previous settings again.", &[("name", &a.name)])))
                            }));
                        }
                    });
                });
            });
            ui.add_space(10.0);
        }

        let list: Vec<Adapter> = sh.visible_adapters().into_iter().cloned().collect();
        if list.is_empty() {
            if sh.adapters_loaded {
                theme::notice(ui, p, p.warning, icon::WARNING, trl("No network adapters were found."));
            } else {
                ui.spinner();
            }
            return;
        }
        if self.selected.as_ref().is_none_or(|id| !list.iter().any(|a| &a.id == id)) {
            self.selected = Some(list[0].id.clone());
        }
        let selected = list.iter().find(|a| Some(&a.id) == self.selected.as_ref()).cloned();

        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(290.0);
                egui::ScrollArea::vertical().id_salt("adapter-list").max_height(height).show(ui, |ui| {
                    for a in &list {
                        let id = egui::Id::new(("adapter", &a.id));
                        let r = theme::selectable_card(ui, p, id, self.selected.as_ref() == Some(&a.id), true, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(kind_icon(a.kind)).size(22.0).color(status_color(p, a)));
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::Label::new(theme::semibold(&a.name, 15.0).color(p.text)).truncate(),
                                        );
                                        if a.default {
                                            ui.label(RichText::new(icon::STAR).size(12.0).color(p.accent))
                                                .on_hover_text(tr("Default route: internet traffic goes here"));
                                        }
                                    });
                                    let ip = a
                                        .main_ipv4()
                                        .map(|(ip, p)| format!("{ip}/{p}"))
                                        .unwrap_or_else(|| tr_dyn(a.status()));
                                    ui.label(RichText::new(ip).size(12.5).color(p.weak));
                                });
                            });
                        });
                        if r.clicked() {
                            self.selected = Some(a.id.clone());
                        }
                        ui.add_space(6.0);
                    }
                });
            });
            ui.add_space(8.0);
            ui.vertical(|ui| {
                if let Some(a) = &selected {
                    egui::ScrollArea::vertical()
                        .id_salt("adapter-detail")
                        .max_height(height)
                        .show(ui, |ui| self.details(ui, p, sh, a));
                }
            });
        });

        self.ip_dialog(ui.ctx(), p, sh);
        self.mac_dialog(ui.ctx(), p, sh);
    }

    fn poll(&mut self, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(Done::Applied(b)) => {
                    let (a, prev) = *b;
                    sh.toast(trf("{name} was changed.", &[("name", &a.name)]));
                    self.undo = Some((a, prev));
                }
                Ok(Done::Message(m)) => sh.toast(m),
                Err(e) => sh.fail(trl("The adapter could not be changed."), &e),
            }
            sh.refresh = true;
            sh.refresh_wifi = true;
        }
        if let Some(Ok((id, m))) = jobs::finished(&mut self.permanent)
            && let Some(ed) = self.mac.as_mut()
            && ed.adapter.id == id
        {
            ed.original = m;
            ed.original_loaded = true;
        }
    }

    fn details(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared, a: &Adapter) {
        let busy = self.job.is_some();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::icon_badge(ui, p, kind_icon(a.kind), status_color(p, a), 48.0);
                ui.vertical(|ui| {
                    ui.label(theme::semibold(&a.name, 20.0).color(p.text));
                    ui.horizontal(|ui| {
                        theme::pill(ui, p, &tr_dyn(a.status()), status_color(p, a));
                        ui.label(RichText::new(tr_dyn(a.kind.label())).color(p.weak).size(13.0));
                        if a.device != a.name && !a.device.is_empty() {
                            ui.label(RichText::new(format!("· {}", a.device)).color(p.weak).size(13.0));
                        }
                    });
                });
            });
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!busy && !a.disabled, |ui| {
                    if theme::primary_button(ui, p, &icon_label(icon::PENCIL_SIMPLE, "Change IP settings"), true)
                        .clicked()
                    {
                        self.ip = Some(IpEditor::new(a));
                    }
                    if theme::secondary_button(ui, &icon_label(icon::FINGERPRINT, "Change MAC address")).clicked() {
                        self.open_mac(ui.ctx(), a);
                    }
                    if a.up && theme::secondary_button(ui, &icon_label(icon::ARROWS_CLOCKWISE, "Renew IP")).clicked() {
                        let a = a.clone();
                        self.job = Some(Job::spawn(ui.ctx(), move |_, _| {
                            config::renew(&a)?;
                            Ok(Done::Message(trf("{name} asked the router for a new address.", &[("name", &a.name)])))
                        }));
                    }
                });
                ui.add_enabled_ui(!busy, |ui| {
                    let (label, on) = if a.disabled { (tr("Turn on"), true) } else { (tr("Turn off"), false) };
                    if theme::secondary_button(ui, &icon_label(icon::POWER, label)).clicked() {
                        let a = a.clone();
                        self.job = Some(Job::spawn(ui.ctx(), move |_, _| {
                            config::set_enabled(&a, on)?;
                            Ok(Done::Message(if on {
                                trf("{name} was turned on.", &[("name", &a.name)])
                            } else {
                                trf("{name} was turned off.", &[("name", &a.name)])
                            }))
                        }));
                    }
                });
                if busy {
                    ui.spinner();
                }
            });
            if a.default && a.up {
                ui.add_space(6.0);
                theme::paragraph(
                    ui,
                    trl("Internet traffic uses this adapter: changing it interrupts the connection for a moment."),
                    12.5,
                    p.weak,
                );
            }
        });
        ui.add_space(12.0);

        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::IDENTIFICATION_CARD, tr("Addresses"));
            let mut copied = false;
            egui::Grid::new(("addr-grid", &a.id)).num_columns(2).spacing([24.0, 8.0]).striped(false).show(ui, |ui| {
                let dash = "—".to_string();
                let how = match a.dhcp {
                    Some(true) => tr("Automatic (DHCP)"),
                    Some(false) => tr("Manual (static)"),
                    None => "—",
                };
                copied |= theme::info_row(ui, p, tr("Configured"), how, false);
                if a.ipv4.is_empty() {
                    copied |= theme::info_row(ui, p, tr("IPv4 address"), "—", false);
                }
                for (ip, prefix) in &a.ipv4 {
                    copied |= theme::info_row(ui, p, tr("IPv4 address"), &ip.to_string(), true);
                    copied |= theme::info_row(
                        ui,
                        p,
                        tr("Subnet mask"),
                        &format!("{} (/{prefix})", prefix_to_mask(*prefix)),
                        false,
                    );
                }
                copied |=
                    theme::info_row(ui, p, tr("Gateway"), &a.gateway.map_or(dash.clone(), |g| g.to_string()), true);
                let dns: Vec<String> = a.dns.iter().map(IpAddr::to_string).collect();
                copied |= theme::info_row(
                    ui,
                    p,
                    tr("DNS servers"),
                    &if dns.is_empty() { dash.clone() } else { dns.join("\n") },
                    true,
                );
                let mac = match a.mac {
                    Some(m) => m.to_string(),
                    None if sh.lan_denied => tr("Hidden by macOS").into(),
                    None => dash.clone(),
                };
                copied |= theme::info_row(ui, p, tr("MAC address"), &mac, a.mac.is_some());
                if let Some(m) = a.mac {
                    let who = if m.is_local() {
                        tr("Private address, set by software").to_string()
                    } else {
                        m.vendor().unwrap_or(tr("Unknown maker")).to_string()
                    };
                    copied |= theme::info_row(ui, p, tr("Maker"), &who, false);
                }
                let v6: Vec<String> = a.ipv6.iter().map(|(ip, p)| format!("{ip}/{p}")).collect();
                if !v6.is_empty() {
                    copied |= theme::info_row(ui, p, tr("IPv6 addresses"), &v6.join("\n"), true);
                }
            });
            if copied {
                sh.toast(tr("Copied."));
            }
        });
        ui.add_space(12.0);

        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::section_title(ui, p, icon::PULSE, tr("Link"));
            egui::Grid::new(("link-grid", &a.id)).num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                if let Some(s) = a.speed_bps {
                    theme::info_row(ui, p, tr("Speed"), &format_speed(s), false);
                }
                if let Some(m) = a.mtu {
                    theme::info_row(ui, p, "MTU", &trf("{n} bytes", &[("n", &m)]), false);
                }
                if let (Some(rx), Some(tx)) = (a.rx_bytes, a.tx_bytes) {
                    theme::info_row(ui, p, tr("Received"), &format_bytes(rx), false);
                    theme::info_row(ui, p, tr("Sent"), &format_bytes(tx), false);
                }
                if let Some(d) = &a.description
                    && d != &a.name
                {
                    theme::info_row(ui, p, tr("Hardware"), d, false);
                }
                theme::info_row(ui, p, tr("System name"), &a.id, true);
            });
            if let Some(g) = a.gateway {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.link(icon_label(icon::PULSE, "Ping the router")).clicked() {
                        sh.nav = Some(Nav::Tool(crate::tools::Tab::Ping, g.to_string()));
                    }
                    if ui.link(icon_label(icon::HEARTBEAT, "Watch the router")).clicked() {
                        sh.nav = Some(Nav::Monitor(g.to_string()));
                    }
                    if ui.link(icon_label(icon::ARROW_SQUARE_OUT, "Open the router's page")).clicked() {
                        crate::app::open_path(&format!("http://{g}"));
                    }
                });
            }
        });
    }

    fn open_mac(&mut self, ctx: &egui::Context, a: &Adapter) {
        self.mac = Some(MacEditor {
            adapter: a.clone(),
            value: a.mac.map(|m| m.to_string()).unwrap_or_default(),
            original: None,
            original_loaded: false,
            vendor: String::new(),
            error: None,
        });
        let a = a.clone();
        self.permanent = Some(Job::spawn(ctx, move |_, _| Ok((a.id.clone(), config::permanent_mac(&a)))));
    }

    fn ip_dialog(&mut self, ctx: &egui::Context, p: &Palette, sh: &mut Shared) {
        let Some(ed) = self.ip.as_mut() else { return };
        let mut close = false;
        let mut apply = None;
        let modal = egui::Modal::new(egui::Id::new("ip-editor")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::PENCIL_SIMPLE).size(24.0).color(p.accent));
                ui.label(theme::semibold(trf("IP settings of {name}", &[("name", &ed.adapter.name)]), 18.0).color(p.text));
            });
            ui.add_space(10.0);
            ed.form.show(ui, p);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut ed.save_profile, tr("Also save as a profile"));
                if ed.save_profile {
                    ui.add(
                        egui::TextEdit::singleline(&mut ed.profile_name)
                            .hint_text(tr("Name, e.g. Office"))
                            .desired_width(180.0),
                    );
                }
            });
            if let Some(e) = &ed.error {
                ui.add_space(6.0);
                theme::notice(ui, p, p.danger, icon::WARNING_CIRCLE, e);
            }
            if ed.adapter.default && ed.adapter.up {
                ui.add_space(6.0);
                theme::paragraph(
                    ui,
                    trl("The connection drops for a moment while the settings change. You can undo the change afterwards."),
                    12.5,
                    p.weak,
                );
            }
            if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
                theme::paragraph(ui, trl("The system asks for your password."), 12.5, p.weak);
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::primary_button(ui, p, &format!("  {}  ", tr("Apply")), true).clicked() {
                    match ed.form.settings() {
                        Ok(s) => apply = Some(s),
                        Err(e) => ed.error = Some(e),
                    }
                }
                if theme::secondary_button(ui, tr("Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if let Some(s) = apply {
            let ed = self.ip.take().unwrap_or_else(|| unreachable!());
            if ed.save_profile {
                let name = if ed.profile_name.trim().is_empty() {
                    ed.adapter.name.clone()
                } else {
                    ed.profile_name.trim().to_string()
                };
                match profiles::upsert(Profile {
                    name: name.clone(),
                    adapter: ed.adapter.name.clone(),
                    settings: s.clone(),
                    note: String::new(),
                    extras: Default::default(),
                }) {
                    Ok(()) => sh.toast(trf("Saved as profile \"{name}\".", &[("name", &name)])),
                    Err(e) => sh.fail(trl("The profile could not be saved."), &e),
                }
            }
            let a = ed.adapter.clone();
            let prev = IpSettings::current(&a);
            self.job = Some(Job::spawn(ctx, move |_, _| {
                config::apply(&a, &s)?;
                Ok(Done::Applied(Box::new((a, prev))))
            }));
        } else if close {
            self.ip = None;
        }
    }

    fn mac_dialog(&mut self, ctx: &egui::Context, p: &Palette, sh: &mut Shared) {
        let Some(ed) = self.mac.as_mut() else { return };
        let mut close = false;
        let mut change: Option<Option<Mac>> = None;
        let modal = egui::Modal::new(egui::Id::new("mac-editor")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::FINGERPRINT).size(24.0).color(p.accent));
                ui.label(theme::semibold(trf("MAC address of {name}", &[("name", &ed.adapter.name)]), 18.0).color(p.text));
            });
            ui.add_space(10.0);
            egui::Grid::new("mac-info").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                let now = ed.adapter.mac.map_or(tr("Unknown").to_string(), describe);
                theme::info_row(ui, p, tr("Now"), &now, false);
                let orig = match (ed.original_loaded, ed.original) {
                    (false, _) => tr("Looking…").to_string(),
                    (true, Some(m)) => describe(m),
                    (true, None) => sh
                        .settings
                        .original_macs
                        .get(&ed.adapter.id)
                        .cloned()
                        .unwrap_or_else(|| tr("Unknown").into()),
                };
                theme::info_row(ui, p, tr("Original"), &orig, false);
            });
            ui.add_space(10.0);
            ui.label(RichText::new(tr("New address")).color(p.weak).size(13.0));
            ui.horizontal(|ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut ed.value).font(egui::TextStyle::Monospace).desired_width(200.0));
                if field.changed() {
                    ed.error = None;
                }
                if ui.button(icon_label(icon::SHUFFLE, "Random")).on_hover_text(tr("A random private address")).clicked() {
                    ed.value = Mac::random().to_string();
                    ed.error = None;
                }
            });
            match ed.value.parse::<Mac>() {
                Ok(m) => {
                    let who = if m.is_local() { tr("a private address (set by software)").to_string() } else { m.vendor().map_or(tr("an unknown maker").into(), |v| trf("made by {maker}", &[("maker", &v)])) };
                    ui.label(RichText::new(format!("{m} — {who}")).color(p.weak).size(12.5));
                }
                Err(_) if !ed.value.trim().is_empty() => {
                    ui.label(RichText::new(tr("Write it as 6 pairs of hex digits, e.g. 02:1A:2B:3C:4D:5E")).color(p.warning).size(12.5));
                }
                Err(_) => {}
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Look like a device from")).color(p.weak).size(13.0));
                ui.add(egui::TextEdit::singleline(&mut ed.vendor).hint_text(tr("maker, e.g. Intel")).desired_width(160.0));
            });
            let matches = mac::vendor_prefixes(&ed.vendor, 6);
            if !matches.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (prefix, name) in matches {
                        if ui.small_button(name).clicked() {
                            ed.value = Mac::random_with_prefix(prefix).to_string();
                        }
                    }
                });
                if cfg!(windows) && ed.adapter.kind == Kind::WiFi {
                    theme::paragraph(ui, trl("Windows Wi-Fi adapters only accept private addresses: use Random instead."), 12.5, p.warning);
                }
            }
            if let Some(e) = &ed.error {
                ui.add_space(6.0);
                theme::notice(ui, p, p.danger, icon::WARNING_CIRCLE, e);
            }
            ui.add_space(8.0);
            let note = if cfg!(target_os = "macos") {
                trl("The connection drops for a moment. macOS keeps the new address until the computer restarts; Wi-Fi may refuse it.")
            } else if cfg!(windows) {
                trl("The adapter restarts, so the connection drops for a moment. The new address stays until you restore the original.")
            } else {
                trl("The connection restarts. The new address stays until you restore the original.")
            };
            theme::paragraph(ui, note, 12.5, p.weak);
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::primary_button(ui, p, &format!("  {}  ", tr("Change")), true).clicked() {
                    match ed.value.parse::<Mac>() {
                        Ok(m) if Some(m) == ed.adapter.mac => ed.error = Some(trl("This is already the adapter's address.").into()),
                        Ok(m) => change = Some(Some(m)),
                        Err(_) => ed.error = Some(trl("This is not a MAC address.").into()),
                    }
                }
                if theme::secondary_button(ui, &icon_label(icon::ARROW_COUNTER_CLOCKWISE, tr("Restore original"))).clicked() {
                    change = Some(None);
                }
                if theme::secondary_button(ui, tr("Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if let Some(target) = change {
            let ed = self.mac.take().unwrap_or_else(|| unreachable!());
            let a = ed.adapter;
            // Remember the address before the first change, to restore it.
            if let Some(m) = ed.original.or(a.mac) {
                sh.settings.original_macs.entry(a.id.clone()).or_insert_with(|| m.to_string());
            }
            let saved: Option<Mac> = sh.settings.original_macs.get(&a.id).and_then(|s| s.parse().ok());
            self.job = Some(Job::spawn(ctx, move |_, _| {
                // Restoring: the system's original, else the one remembered.
                let mac = match target {
                    None if ed.original.is_none() => saved,
                    t => t,
                };
                config::set_mac(&a, mac)?;
                Ok(Done::Message(match mac {
                    Some(m) if target.is_some() => trf("{name} now uses {mac}.", &[("name", &a.name), ("mac", &m)]),
                    _ => trf("{name} uses its original address again.", &[("name", &a.name)]),
                }))
            }));
        } else if close {
            self.mac = None;
        }
    }
}

fn describe(m: Mac) -> String {
    if m.is_local() {
        trf("{mac} (private)", &[("mac", &m)])
    } else {
        format!("{m} ({})", m.vendor().unwrap_or(tr("unknown maker")))
    }
}

struct IpEditor {
    adapter: Adapter,
    form: IpForm,
    save_profile: bool,
    profile_name: String,
    error: Option<String>,
}

impl IpEditor {
    fn new(a: &Adapter) -> Self {
        Self {
            adapter: a.clone(),
            form: IpForm::new(&IpSettings::current(a), Some(a)),
            save_profile: false,
            profile_name: String::new(),
            error: None,
        }
    }
}

/// The fields of IP settings, as typed; shared with the Profiles page.
#[derive(Clone)]
pub struct IpForm {
    pub dhcp: bool,
    pub address: String,
    pub subnet: String,
    pub gateway: String,
    pub dns_auto: bool,
    pub dns1: String,
    pub dns2: String,
}

impl IpForm {
    /// The form for `s`; the adapter's current address fills in a DHCP form,
    /// so that switching to Manual starts from it.
    pub fn new(s: &IpSettings, a: Option<&Adapter>) -> Self {
        let (ip, prefix, gateway) = match &s.mode {
            Ipv4Mode::Static { address, prefix, gateway } => {
                (*address, *prefix, gateway.map(|g| g.to_string()).unwrap_or_default())
            }
            Ipv4Mode::Dhcp => {
                let (ip, p) = a.and_then(Adapter::main_ipv4).unwrap_or((Ipv4Addr::UNSPECIFIED, 24));
                let g = match a.and_then(|a| a.gateway) {
                    Some(IpAddr::V4(g)) => g.to_string(),
                    _ => String::new(),
                };
                (ip, p, g)
            }
        };
        let dns: Vec<String> = s.dns.iter().map(IpAddr::to_string).collect();
        Self {
            dhcp: s.mode == Ipv4Mode::Dhcp,
            address: if ip.is_unspecified() || ip.is_link_local() { String::new() } else { ip.to_string() },
            subnet: prefix_to_mask(prefix).to_string(),
            gateway,
            dns_auto: dns.is_empty(),
            dns1: dns.first().cloned().unwrap_or_default(),
            dns2: dns.get(1).cloned().unwrap_or_default(),
        }
    }

    pub fn show(&mut self, ui: &mut Ui, p: &Palette) {
        ui.label(RichText::new(tr("IP address")).color(p.weak).size(13.0));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.dhcp, true, tr("Automatic (DHCP)"));
            ui.selectable_value(&mut self.dhcp, false, tr("Manual"));
        });
        if !self.dhcp {
            ui.add_space(6.0);
            egui::Grid::new("ip-fields").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr("Address"));
                ui.add(egui::TextEdit::singleline(&mut self.address).hint_text("192.168.1.50").desired_width(220.0));
                ui.end_row();
                ui.label(tr("Subnet"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.subnet)
                        .hint_text(tr("255.255.255.0 or 24"))
                        .desired_width(220.0),
                );
                ui.end_row();
                ui.label(tr("Gateway"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.gateway)
                            .hint_text(tr("192.168.1.1 (optional)"))
                            .desired_width(220.0),
                    );
                    if self.gateway.trim().is_empty()
                        && let Some(g) = self.suggested_gateway()
                        && ui.small_button(trf("Use {gateway}", &[("gateway", &g)])).clicked()
                    {
                        self.gateway = g.to_string();
                    }
                });
                ui.end_row();
            });
        }
        ui.add_space(10.0);
        ui.label(RichText::new(tr("DNS servers")).color(p.weak).size(13.0));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.dns_auto, true, tr("Automatic"));
            ui.selectable_value(&mut self.dns_auto, false, tr("Manual"));
            if !self.dns_auto {
                egui::ComboBox::from_id_salt("dns-preset").selected_text(tr("Choose a service…")).width(200.0).show_ui(
                    ui,
                    |ui| {
                        for (name, servers) in DNS_PRESETS {
                            if ui
                                .selectable_label(false, format!("{}  ({})", tr_dyn(name), servers.join(", ")))
                                .clicked()
                            {
                                self.dns1 = servers[0].to_string();
                                self.dns2 = servers.get(1).map(|s| s.to_string()).unwrap_or_default();
                            }
                        }
                    },
                );
            }
        });
        if self.dhcp && self.dns_auto {
            theme::paragraph(ui, trl("The router gives this computer its address and DNS servers."), 12.5, p.weak);
        }
        if !self.dns_auto {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.dns1)
                        .hint_text(tr("Preferred, e.g. 1.1.1.1"))
                        .desired_width(200.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.dns2)
                        .hint_text(tr("Alternate (optional)"))
                        .desired_width(200.0),
                );
            });
        }
    }

    /// The .1 of the subnet being typed, as a gateway suggestion.
    fn suggested_gateway(&self) -> Option<Ipv4Addr> {
        let ip: Ipv4Addr = self.address.trim().parse().ok()?;
        let prefix = parse_subnet(&self.subnet).ok()?;
        let mask = u32::from(prefix_to_mask(prefix));
        let g = Ipv4Addr::from((u32::from(ip) & mask) + 1);
        (g != ip && prefix < 31).then_some(g)
    }

    pub fn settings(&self) -> Result<IpSettings, String> {
        let mode = if self.dhcp {
            Ipv4Mode::Dhcp
        } else {
            let address: Ipv4Addr = self
                .address
                .trim()
                .parse()
                .map_err(|_| trl("Enter an IPv4 address, e.g. 192.168.1.50.").to_string())?;
            let prefix = parse_subnet(&self.subnet)?;
            let gateway = match self.gateway.trim() {
                "" => None,
                g => Some(
                    g.parse::<Ipv4Addr>().map_err(|_| trlf("\"{value}\" is not an IPv4 address.", &[("value", &g)]))?,
                ),
            };
            Ipv4Mode::Static { address, prefix, gateway }
        };
        let mut dns = Vec::new();
        if !self.dns_auto {
            for d in [&self.dns1, &self.dns2] {
                let d = d.trim();
                if !d.is_empty() {
                    dns.push(
                        d.parse::<IpAddr>().map_err(|_| trlf("\"{value}\" is not an IP address.", &[("value", &d)]))?,
                    );
                }
            }
            if dns.is_empty() {
                return Err(trl("Enter at least one DNS server, or choose Automatic.").into());
            }
        }
        let s = IpSettings { mode, dns };
        s.validate().map_err(|e| e.to_string())?;
        Ok(s)
    }
}

/// "255.255.255.0", "24" or "/24".
fn parse_subnet(s: &str) -> Result<u8, String> {
    let s = s.trim().trim_start_matches('/');
    if let Ok(p) = s.parse::<u8>() {
        return if (1..=32).contains(&p) { Ok(p) } else { Err(trl("The prefix must be between 1 and 32.").into()) };
    }
    let mask: Ipv4Addr = s.parse().map_err(|_| trl("Enter the subnet as 255.255.255.0 or 24.").to_string())?;
    mask_to_prefix(mask)
        .filter(|p| *p > 0)
        .ok_or_else(|| trlf("{mask} is not a valid subnet mask.", &[("mask", &mask)]))
}

struct MacEditor {
    adapter: Adapter,
    value: String,
    original: Option<Mac>,
    original_loaded: bool,
    vendor: String,
    error: Option<String>,
}
