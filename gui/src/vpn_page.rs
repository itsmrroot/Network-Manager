//! VPN: what is on, where the traffic goes, a leak check, Tailscale (devices,
//! exit node) and the VPN connections the system keeps.

use eframe::egui::{self, Align, Layout, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::internet::Check;
use netmgr::routes::{self, Summary};
use netmgr::tailscale;
use netmgr::vpns::{self, Connection};

use crate::app::{Nav, Shared};
use crate::i18n::{tr, tr_dyn, trf, trl, trlf};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

#[derive(Default)]
pub struct VpnPage {
    loaded: bool,
    leaks: Option<Job<Vec<Check>>>,
    leak_checks: Vec<Check>,
    routes: Option<Job<Summary>>,
    summary: Option<Result<Summary, String>>,
    ts: Option<Job<tailscale::Status>>,
    ts_status: Option<Result<tailscale::Status, String>>,
    conns: Option<Job<Vec<Connection>>>,
    connections: Vec<Connection>,
    /// A switch that is being turned: the message when it worked.
    action: Option<Job<String>>,
}

impl VpnPage {
    /// Reads the routing table, Tailscale and the system's VPNs again.
    pub fn refresh(&mut self, ctx: &egui::Context, sh: &Shared) {
        self.loaded = true;
        let adapters = sh.adapters.clone();
        if self.routes.is_none() {
            self.routes = Some(Job::spawn(ctx, move |_, _| {
                let table = routes::table(&adapters)?;
                Ok(routes::summarize(&table, &adapters))
            }));
        }
        if self.ts.is_none() && tailscale::binary().is_some() {
            self.ts = Some(Job::spawn(ctx, |_, _| tailscale::status()));
        }
        if self.conns.is_none() {
            self.conns = Some(Job::spawn(ctx, |_, _| vpns::list()));
        }
    }

    fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.leaks) {
            match r {
                Ok(checks) => self.leak_checks = checks,
                Err(e) => sh.fail(trl("The VPN check could not be completed."), &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.routes) {
            self.summary = Some(r.map_err(|e| jobs::describe(&e)));
        }
        if let Some(r) = jobs::finished(&mut self.ts) {
            self.ts_status = Some(r.map_err(|e| jobs::describe(&e)));
        }
        if let Some(r) = jobs::finished(&mut self.conns) {
            self.connections = r.unwrap_or_default();
        }
        if let Some(r) = jobs::finished(&mut self.action) {
            match r {
                Ok(msg) => sh.toast(msg),
                Err(e) => sh.fail(trl("That did not work."), &e),
            }
            // What was switched shows in the lists and in the adapters.
            sh.refresh = true;
            self.refresh(ctx, sh);
        }
    }

    /// Development aid: sample data for the tour.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        use netmgr::internet::PublicInfo;
        use netmgr::vpn::{DnsProbe, Leaks, findings};
        self.loaded = true;
        let info = |ip: &str, city: &str, org: &str| PublicInfo {
            ip: ip.into(),
            city: Some(city.into()),
            country: Some("AT".into()),
            org: Some(org.into()),
            ..Default::default()
        };
        let public = info("198.51.100.9", "Vienna", "AS64500 Home Lab ISP");
        self.leak_checks = findings(&Leaks {
            public: public.clone(),
            ipv6: None,
            dns: vec![
                DnsProbe { server: "100.100.100.100".parse().unwrap(), seen: Some(public) },
                DnsProbe {
                    server: "192.168.178.1".parse().unwrap(),
                    seen: Some(info("203.0.113.7", "Graz", "AS8447 A1 Telekom")),
                },
            ],
        });
        let via = |net: &str, device: &str, name: &str, kind| netmgr::routes::Via {
            net: net.into(),
            device: device.into(),
            name: name.into(),
            kind,
        };
        self.summary = Some(Ok(Summary {
            internet: Some(via("0.0.0.0/0", "utun99", "utun99", netmgr::adapters::Kind::Vpn)),
            local: vec![via("192.168.178.0/24", "en0", "Wi-Fi", netmgr::adapters::Kind::WiFi)],
            tunnel: vec![via("100.64.0.0/10", "utun99", "utun99", netmgr::adapters::Kind::Vpn)],
        }));
        let device = |name: &str, ip: &str, os: &str, online: bool, exit: bool, owner: &str| tailscale::Device {
            name: name.into(),
            dns: format!("{name}.tail1234.ts.net"),
            ips: vec![ip.parse().unwrap()],
            os: os.into(),
            online,
            exit_in_use: false,
            exit_option: exit,
            owner: owner.into(),
        };
        let mut proxmox = device("proxmox-server-homelab", "100.69.73.40", "linux", true, true, "itsmrroot@github");
        proxmox.exit_in_use = true;
        self.ts_status = Some(Ok(tailscale::Status {
            state: "Running".into(),
            tailnet: Some("itsmrroot@github".into()),
            version: "1.80.0".into(),
            me: device("macbook-pro-von-mbp", "100.92.141.34", "macOS", true, false, "itsmrroot@github"),
            peers: vec![
                proxmox,
                device("nas-garage", "100.64.10.5", "linux", true, true, "itsmrroot@github"),
                device("iphone-16-pro", "100.72.196.70", "iOS", false, false, "itsmrroot@github"),
            ],
            allow_lan: Some(true),
        }));
        self.connections = vec![
            Connection { name: "Work VPN".into(), kind: "IKEv2".into(), connected: false, busy: false },
            Connection { name: "Office".into(), kind: "L2TP".into(), connected: true, busy: false },
        ];
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let ctx = ui.ctx().clone();
        if !self.loaded {
            self.refresh(&ctx, sh);
        }
        self.poll(&ctx, sh);
        let busy = self.action.is_some();
        theme::title_row(ui, |ui| {
            ui.vertical(|ui| {
                theme::page_title(ui, p, tr("VPN"), tr("What is on, where your traffic goes, and the switches."))
            });
            ui.with_layout(theme::far_side(Align::Min), |ui| {
                if theme::secondary_button(ui, &icon_label(icon::ARROWS_CLOCKWISE, "Refresh")).clicked() {
                    self.refresh(&ctx, sh);
                }
            });
        });
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            self.status_card(ui, p, sh);
            ui.add_space(14.0);
            self.routes_card(ui, p, sh);
            if self.ts_status.is_some() || self.ts.is_some() {
                ui.add_space(14.0);
                self.tailscale_card(ui, p, sh, busy);
            }
            ui.add_space(14.0);
            self.connections_card(ui, p, sh, busy);
            ui.add_space(20.0);
        });
    }

    /// Whether a VPN is on, what it carries and what websites see, with a
    /// check that nothing leaves around it.
    fn status_card(&mut self, ui: &mut Ui, p: &Palette, sh: &Shared) {
        let Some(v) = sh.vpn() else {
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                theme::section_title(ui, p, icon::SHIELD_SLASH, tr("No VPN is on"));
                ui.label(
                    RichText::new(tr("Your traffic goes straight out through your local network."))
                        .color(p.weak)
                        .size(13.5),
                );
            });
            return;
        };
        let name = tr_dyn(v.product.label());
        let address = match &sh.public {
            Some(Ok(info)) if !sh.public_loading => {
                let more = [info.place(), info.provider()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                Some(if more.is_empty() { info.ip.clone() } else { format!("{} ({more})", info.ip) })
            }
            _ => None,
        };
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::SHIELD_CHECK, tr("VPN is on"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.leaks.is_some() {
                        ui.spinner();
                    } else {
                        let hint = if v.full {
                            tr("Looks up where your traffic, IPv6 and DNS lookups leave from. It asks ipinfo.io and your DNS servers.")
                        } else {
                            tr("Only for a VPN that carries all internet traffic.")
                        };
                        if ui
                            .add_enabled(v.full, egui::Button::new(icon_label(icon::SHIELD_WARNING, "Check for leaks")))
                            .on_hover_text(hint)
                            .clicked()
                        {
                            self.leak_checks.clear();
                            self.leaks = Some(Job::spawn(ui.ctx(), |_, _| {
                                Ok(netmgr::vpn::findings(&netmgr::vpn::check()?))
                            }));
                        }
                    }
                });
            });
            let ip = v.tunnel.main_ipv4().map(|(ip, _)| ip.to_string()).unwrap_or_default();
            ui.label(RichText::new(format!("{name} · {} · {ip}", v.tunnel.device)).color(p.text).size(14.0));
            ui.label(
                RichText::new(if v.full {
                    tr("All internet traffic goes through the VPN.")
                } else {
                    tr("A split tunnel: only the VPN's own networks go through it. The rest of the internet uses your local network.")
                })
                .color(p.weak)
                .size(13.5),
            );
            if let Some(local) = &v.local {
                let lip = local.main_ipv4().map(|(ip, p)| format!("{ip}/{p}")).unwrap_or_default();
                ui.label(
                    RichText::new(trf("Local network: {adapter} · {ip}", &[("adapter", &local.name), ("ip", &lip)]))
                        .color(p.weak)
                        .size(13.5),
                );
            }
            ui.label(
                RichText::new(match (&address, v.full) {
                    (Some(a), true) => trf(
                        "Websites see this address: {address}. This is where the VPN leaves the internet, not your local network.",
                        &[("address", a)],
                    ),
                    (Some(a), false) => {
                        trf("Websites see your local network's address: {address}.", &[("address", a)])
                    }
                    (None, _) if sh.public_loading => tr("Looking up…").to_string(),
                    (None, _) => tr(
                        "Turn on the public address lookup in Settings → Internet to see what websites see.",
                    )
                    .to_string(),
                })
                .color(p.weak)
                .size(13.5),
            );
            if !self.leak_checks.is_empty() || self.leaks.is_some() {
                ui.add_space(8.0);
                crate::overview::check_rows(ui, p, &self.leak_checks);
                if self.leaks.is_none() {
                    if self.leak_checks.iter().all(|c| c.ok) {
                        theme::notice(
                            ui,
                            p,
                            p.success,
                            icon::CHECK_CIRCLE,
                            trl(
                                "No leaks found: your traffic, IPv6 and DNS lookups all leave through the VPN's network.",
                            ),
                        );
                    }
                    if ui.button(tr("Close")).clicked() {
                        self.leak_checks.clear();
                    }
                }
            }
        });
    }

    /// The routing table in plain words.
    fn routes_card(&mut self, ui: &mut Ui, p: &Palette, sh: &Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::SIGNPOST, tr("Where your traffic goes"));
                if self.routes.is_some() {
                    ui.spinner();
                }
            });
            let s = match &self.summary {
                Some(Ok(s)) => s,
                Some(Err(_)) => {
                    ui.label(RichText::new(tr("The routing table could not be read.")).color(p.weak).size(13.5));
                    return;
                }
                None => return,
            };
            if let Some(v) = &s.internet {
                let through_vpn = v.kind == netmgr::adapters::Kind::Vpn;
                let (glyph, color) =
                    if through_vpn { (icon::SHIELD_CHECK, p.success) } else { (icon::ARROW_RIGHT, p.weak) };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(glyph).color(color).size(16.0));
                    ui.label(
                        RichText::new(if through_vpn {
                            trf("All other traffic goes through {name}, the VPN.", &[("name", &v.name)])
                        } else {
                            trf("All other traffic goes straight out through {name}.", &[("name", &v.name)])
                        })
                        .color(p.text)
                        .size(14.0),
                    );
                });
            }
            for (title, list) in
                [(tr("Sent through the VPN"), &s.tunnel), (tr("Reached directly, not through the VPN"), &s.local)]
            {
                if list.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                ui.label(RichText::new(title).color(p.weak).size(12.5));
                for v in list {
                    ui.label(RichText::new(format!("{}  →  {}", v.net, v.name)).color(p.text).size(14.0).monospace());
                }
            }
            let full = sh.vpn().is_some_and(|v| v.full);
            if full {
                ui.add_space(8.0);
                if s.local.is_empty() {
                    theme::notice(
                        ui,
                        p,
                        p.warning,
                        icon::INFO,
                        trl(
                            "While the VPN is on, no direct route to your local network is set, so it may not be reachable.",
                        ),
                    );
                } else {
                    theme::notice(
                        ui,
                        p,
                        p.accent,
                        icon::INFO,
                        trl(
                            "Your local network stays reachable while the VPN is on. In Tailscale this is the setting Allow local network access.",
                        ),
                    );
                }
            }
        });
    }

    fn tailscale_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared, busy: bool) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            let status = match &self.ts_status {
                Some(Ok(s)) => Some(s.clone()),
                _ => None,
            };
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::GRAPH, tr("Tailscale"));
                if self.ts.is_some() || busy {
                    ui.spinner();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let Some(s) = &status else { return };
                    let (label, color) = match s.state.as_str() {
                        "Running" => (tr("Connected"), p.success),
                        "NeedsLogin" | "NeedsMachineAuth" => (tr("Not logged in"), p.warning),
                        "Starting" => (tr("Starting…"), p.warning),
                        _ => (tr("Disconnected"), p.weak),
                    };
                    if s.running() {
                        if ui.add_enabled(!busy, egui::Button::new(icon_label(icon::POWER, "Disconnect"))).clicked() {
                            self.action = Some(Job::spawn(ui.ctx(), |_, _| {
                                tailscale::down()?;
                                Ok(tr("Tailscale is disconnected.").to_string())
                            }));
                        }
                    } else if s.state == "Stopped"
                        && ui
                            .add_enabled(!busy, egui::Button::new(icon_label(icon::PLUGS_CONNECTED, "Connect")))
                            .clicked()
                    {
                        self.action = Some(Job::spawn(ui.ctx(), |_, _| {
                            tailscale::up()?;
                            Ok(tr("Tailscale is connected.").to_string())
                        }));
                    }
                    theme::pill(ui, p, label, color);
                });
            });
            let s = match (&self.ts_status, status) {
                (Some(Err(e)), _) => {
                    ui.label(
                        RichText::new(trf("Tailscale did not answer: {error}", &[("error", e)]))
                            .color(p.weak)
                            .size(13.5),
                    );
                    return;
                }
                (_, Some(s)) => s,
                _ => return,
            };
            let mine = s.me.ip().map(|i| i.to_string()).unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                let tailnet = s.tailnet.clone().unwrap_or_default();
                ui.label(RichText::new(format!("{tailnet}  ·  {}  {mine}", s.me.name)).color(p.text).size(14.0));
                if !mine.is_empty() && ui.small_button(icon::COPY).on_hover_text(tr("Copy")).clicked() {
                    ui.ctx().copy_text(mine.clone());
                    sh.toast(tr("Copied."));
                }
            });
            if s.state == "NeedsLogin" || s.state == "NeedsMachineAuth" {
                ui.add_space(6.0);
                theme::notice(
                    ui,
                    p,
                    p.warning,
                    icon::WARNING,
                    trl("Log in with the Tailscale app first, then click Refresh."),
                );
                return;
            }
            if !s.running() {
                return;
            }
            ui.add_space(8.0);
            // The exit node: all internet traffic goes through that device.
            let options = s.exit_options();
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Exit node")).color(p.weak).size(13.5));
                let current = s.exit_node();
                let shown = current.map_or_else(|| tr("None").to_string(), |d| d.name.clone());
                let mut choice: Option<Option<std::net::IpAddr>> = None;
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt("exit-node").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                        if ui.selectable_label(current.is_none(), tr("None")).clicked() {
                            choice = Some(None);
                        }
                        for d in &options {
                            let label =
                                if d.online { d.name.clone() } else { format!("{} ({})", d.name, tr("Offline")) };
                            if ui.selectable_label(current.is_some_and(|c| c.name == d.name), label).clicked() {
                                choice = Some(d.ip());
                            }
                        }
                    });
                });
                if options.is_empty() {
                    ui.label(RichText::new(tr("No device offers to be an exit node.")).color(p.weak).size(12.5));
                }
                if let Some(ip) = choice {
                    let name = current.map(|d| d.name.clone());
                    let target = ip.and_then(|ip| options.iter().find(|d| d.ip() == Some(ip)).map(|d| d.name.clone()));
                    let _ = name;
                    self.action = Some(Job::spawn(ui.ctx(), move |_, _| {
                        tailscale::set_exit_node(ip)?;
                        Ok(match target {
                            Some(n) => trf("The exit node is {name}.", &[("name", &n)]),
                            None => tr("No exit node is used now.").to_string(),
                        })
                    }));
                }
            });
            if let Some(allow) = s.allow_lan {
                let mut on = allow;
                ui.add_enabled_ui(!busy, |ui| {
                    if ui
                        .checkbox(&mut on, tr("Allow local network access"))
                        .on_hover_text(tr("Keeps your local network (printers, the router, other computers) reachable while an exit node is used."))
                        .changed()
                    {
                        self.action = Some(Job::spawn(ui.ctx(), move |_, _| {
                            tailscale::set_allow_lan(on)?;
                            Ok(tr("The setting was changed.").to_string())
                        }));
                    }
                });
            }
            ui.add_space(8.0);
            ui.label(RichText::new(tr("Devices")).color(p.weak).size(12.5));
            let mut ssh = None;
            for d in &s.peers {
                ui.horizontal(|ui| {
                    let dot = RichText::new("●").size(10.0).color(if d.online { p.success } else { p.weak });
                    ui.label(dot).on_hover_text(if d.online { tr("Connected") } else { tr("Offline") });
                    ui.label(RichText::new(&d.name).color(if d.online { p.text } else { p.weak }).size(14.0));
                    let ip = d.ip().map(|i| i.to_string()).unwrap_or_default();
                    ui.label(RichText::new(&ip).color(p.weak).size(13.0).monospace());
                    ui.label(RichText::new(&d.os).color(p.weak).size(12.5));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                d.online && !ip.is_empty(),
                                egui::Button::new(icon_label(icon::TERMINAL_WINDOW, "SSH")),
                            )
                            .on_hover_text(tr("Open an SSH session to this device in the Console"))
                            .clicked()
                        {
                            ssh = Some(ip.clone());
                        }
                        if ui.small_button(icon::COPY).on_hover_text(tr("Copy")).clicked() {
                            ui.ctx().copy_text(ip.clone());
                            sh.toast(tr("Copied."));
                        }
                    });
                });
            }
            if let Some(host) = ssh {
                sh.nav = Some(Nav::Ssh(host));
            }
        });
    }

    fn connections_card(&mut self, ui: &mut Ui, p: &Palette, _sh: &mut Shared, busy: bool) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::LOCK_KEY, tr("VPN connections of the system"));
                if self.conns.is_some() {
                    ui.spinner();
                }
            });
            if self.connections.is_empty() && self.conns.is_none() {
                ui.label(
                    RichText::new(tr("No VPN connections are set up in the system. Programs like WireGuard that run on their own show up on the Adapters page while they are on."))
                        .color(p.weak)
                        .size(13.5),
                );
                return;
            }
            let mut switch = None;
            for c in &self.connections {
                ui.horizontal(|ui| {
                    let dot = RichText::new("●").size(10.0).color(if c.connected { p.success } else { p.weak });
                    ui.label(dot);
                    ui.label(RichText::new(&c.name).color(p.text).size(14.0));
                    ui.label(RichText::new(&c.kind).color(p.weak).size(12.5));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if c.connected {
                            icon_label(icon::POWER, "Disconnect")
                        } else {
                            icon_label(icon::PLUGS_CONNECTED, "Connect")
                        };
                        if ui.add_enabled(!busy && !c.busy, egui::Button::new(label)).clicked() {
                            switch = Some((c.clone(), !c.connected));
                        }
                    });
                });
            }
            if let Some((c, on)) = switch {
                self.action = Some(Job::spawn(ui.ctx(), move |_, _| {
                    vpns::set(&c, on)?;
                    Ok(if on {
                        trlf("{name} is connecting…", &[("name", &c.name)])
                    } else {
                        trlf("{name} was disconnected.", &[("name", &c.name)])
                    })
                }));
            }
        });
    }
}
