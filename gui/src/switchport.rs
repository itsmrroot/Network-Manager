//! Switch port: which switch, port and VLAN this computer is plugged into
//! (LLDP and CDP), and which DHCP servers answer on the network.

use std::net::Ipv4Addr;
use std::time::Instant;

use eframe::egui::{self, Align, Layout, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::adapters::Kind;
use netmgr::dhcp::TestResult;
use netmgr::discovery::Neighbor;

use crate::app::{Nav, Shared};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

const LISTEN_SECONDS: u64 = 65;

#[derive(Default)]
pub struct SwitchPort {
    adapter: Option<String>,
    listen: Option<(Job<Vec<Neighbor>>, Instant)>,
    neighbors: Option<Vec<Neighbor>>,
    dhcp: Option<Job<TestResult>>,
    dhcp_result: Option<TestResult>,
}

impl SwitchPort {
    /// Development aid: sample results for README screenshots.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        self.neighbors = Some(vec![Neighbor {
            protocol: "LLDP".into(),
            system_name: Some("core-sw-01.lab".into()),
            chassis_id: Some("00:1E:BD:12:34:00".into()),
            port_id: Some("Gi1/0/12".into()),
            port_description: Some("Desk 4.12 — Engineering".into()),
            description: Some("Cisco IOS Software, C2960X Software (C2960X-UNIVERSALK9-M), Version 15.2(7)E9".into()),
            platform: None,
            management: vec!["10.0.0.2".into()],
            vlan: Some(20),
            voice_vlan: Some(30),
            vlan_names: vec![(20, "Users".into()), (30, "Voice".into())],
            capabilities: vec!["Bridge".into(), "Router".into()],
            duplex: None,
            vtp_domain: None,
            poe: Some("Class 4, 25.5 W".into()),
            link: Some("1 Gbit/s full duplex".into()),
            ttl: Some(120),
            source: None,
        }]);
        let offer = |s: [u8; 4], a: [u8; 4], ms: u128| netmgr::dhcp::Offer {
            server: Some(s.into()),
            address: Some(a.into()),
            mask: Some([255, 255, 255, 0].into()),
            routers: vec![[192, 168, 1, 1].into()],
            dns: vec![[192, 168, 1, 1].into()],
            domain: Some("lab".into()),
            lease_seconds: Some(86400),
            millis: ms,
            ..Default::default()
        };
        self.dhcp_result = Some(TestResult {
            mac: "F0:18:98:11:22:33".into(),
            offers: vec![
                offer([192, 168, 1, 1], [192, 168, 1, 77], 4),
                offer([192, 168, 1, 250], [192, 168, 1, 120], 11),
            ],
        });
    }

    pub fn busy(&self) -> bool {
        self.listen.is_some() || self.dhcp.is_some()
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(r) = self.listen.as_ref().and_then(|(j, _)| j.poll()) {
            self.listen = None;
            match r {
                Ok(n) => self.neighbors = Some(n),
                Err(e) => sh.fail("Listening for the switch did not work.", &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.dhcp) {
            match r {
                Ok(t) => self.dhcp_result = Some(t),
                Err(e) => sh.fail("The DHCP test did not work.", &e),
            }
        }
        if self.busy() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
        theme::page_title(
            ui,
            p,
            "Switch port",
            "Which switch, port and VLAN this cable is plugged into, and which DHCP servers answer.",
        );
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            self.lldp_card(ui, p, sh);
            ui.add_space(14.0);
            self.dhcp_card(ui, p, sh);
            ui.add_space(20.0);
        });
    }

    fn lldp_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        // Wired adapters that are connected come first.
        let mut adapters: Vec<_> = sh.visible_adapters().into_iter().filter(|a| a.up).cloned().collect();
        adapters.sort_by_key(|a| (a.kind != Kind::Ethernet, !a.default));
        if self.adapter.as_ref().is_none_or(|id| !adapters.iter().any(|a| &a.id == id)) {
            self.adapter = adapters.first().map(|a| a.id.clone());
        }
        let chosen = adapters.iter().find(|a| Some(&a.id) == self.adapter.as_ref()).cloned();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::TREE_STRUCTURE, "Switch port (LLDP / CDP)");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some((job, started)) = &self.listen {
                        if ui.button(icon_label(icon::STOP, "Stop")).clicked() {
                            job.stop();
                        }
                        let left = LISTEN_SECONDS.saturating_sub(started.elapsed().as_secs());
                        ui.label(RichText::new(format!("up to {left} s")).color(p.weak));
                        ui.spinner();
                    } else if theme::primary_button(ui, p, &icon_label(icon::EAR, "Listen"), chosen.is_some()).clicked()
                        && let Some(a) = chosen.clone()
                    {
                        let iface = if cfg!(windows) { a.name.clone() } else { a.device.clone() };
                        self.neighbors = None;
                        self.listen = Some((
                            Job::spawn(ui.ctx(), move |_, _| netmgr::discovery::listen(&iface, LISTEN_SECONDS)),
                            Instant::now(),
                        ));
                    }
                    egui::ComboBox::from_id_salt("lldp-adapter")
                        .selected_text(chosen.as_ref().map_or("No connected adapter".into(), |a| a.name.clone()))
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for a in &adapters {
                                ui.selectable_value(
                                    &mut self.adapter,
                                    Some(a.id.clone()),
                                    format!("{} ({})", a.name, a.kind.label()),
                                );
                            }
                        });
                });
            });
            if chosen.as_ref().is_some_and(|a| a.kind == Kind::WiFi) {
                theme::paragraph(
                    ui,
                    "This is a Wi-Fi adapter: switches announce themselves on cables. Most access points do not pass the announcements on.",
                    12.5,
                    p.warning,
                );
            }
            match &self.neighbors {
                None if self.listen.is_some() => {
                    ui.add_space(6.0);
                    theme::paragraph(
                        ui,
                        "Listening… Switches announce themselves every 30 seconds (LLDP) or 60 seconds (CDP), so this can take up to a minute. Nothing is sent.",
                        14.0,
                        p.weak,
                    );
                }
                None => {
                    ui.add_space(6.0);
                    let rights = if cfg!(windows) {
                        "It uses Windows' built-in packet monitor (pktmon)."
                    } else {
                        "The system asks for your password: listening to the cable needs administrator rights."
                    };
                    theme::paragraph(
                        ui,
                        &format!(
                            "Plug in the network cable and click Listen to see the switch's name, the port, the VLAN, the voice VLAN, PoE and the switch's management address. {rights}"
                        ),
                        14.0,
                        p.weak,
                    );
                }
                Some(list) if list.is_empty() => {
                    ui.add_space(6.0);
                    theme::notice(
                        ui,
                        p,
                        p.warning,
                        icon::INFO,
                        "No announcement was heard. LLDP and CDP may be turned off on this switch (or on its port), the device may be a simple unmanaged switch, or the cable goes to a router or a wall socket without a switch behind it.",
                    );
                }
                Some(list) => {
                    for n in list.clone() {
                        ui.add_space(8.0);
                        neighbor(ui, p, sh, &n);
                    }
                }
            }
        });
    }

    fn dhcp_card(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::section_title(ui, p, icon::BROADCAST, "DHCP servers");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.dhcp.is_some() {
                        ui.spinner();
                    } else if theme::primary_button(
                        ui,
                        p,
                        &icon_label(icon::MAGNIFYING_GLASS, "Find DHCP servers"),
                        true,
                    )
                    .clicked()
                    {
                        let iface = self.adapter.clone().and_then(|id| {
                            sh.adapters
                                .iter()
                                .find(|a| a.id == id)
                                .map(|a| if cfg!(windows) { a.name.clone() } else { a.device.clone() })
                        });
                        self.dhcp =
                            Some(Job::spawn(ui.ctx(), move |_, _| netmgr::dhcp::test_elevated(iface.as_deref(), 5)));
                    }
                });
            });
            let Some(r) = &self.dhcp_result else {
                theme::paragraph(
                    ui,
                    "Asks the network for an address and lists every DHCP server that answers, with what it offers. Two servers usually mean a rogue one — the classic cause of computers getting wrong addresses. Nothing is changed on this computer.",
                    14.0,
                    p.weak,
                );
                return;
            };
            let servers = r.servers();
            ui.add_space(4.0);
            match servers.len() {
                0 => theme::notice(
                    ui,
                    p,
                    p.danger,
                    icon::X_CIRCLE,
                    "No DHCP server answered. Devices on this network will not get an address automatically.",
                ),
                1 => theme::notice(
                    ui,
                    p,
                    p.success,
                    icon::CHECK_CIRCLE,
                    &format!("One DHCP server answered: {}.", servers[0]),
                ),
                n => theme::notice(
                    ui,
                    p,
                    p.danger,
                    icon::WARNING,
                    &format!(
                        "{n} DHCP servers answered: {}. Normally there is one; another one is probably a rogue server (a home router or a VM host plugged in by mistake).",
                        servers.iter().map(Ipv4Addr::to_string).collect::<Vec<_>>().join(", ")
                    ),
                ),
            }
            for o in &r.offers {
                ui.add_space(8.0);
                egui::Frame::new().fill(p.card_alt).corner_radius(10).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        theme::semibold(format!("Server {}", o.server.map_or("?".into(), |s| s.to_string())), 15.0)
                            .color(p.text),
                    );
                    let list = |v: &[Ipv4Addr]| {
                        if v.is_empty() {
                            "—".into()
                        } else {
                            v.iter().map(Ipv4Addr::to_string).collect::<Vec<_>>().join(", ")
                        }
                    };
                    egui::Grid::new(("offer", o.server)).num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                        let rows = [
                            (
                                "Offers",
                                format!(
                                    "{}{}",
                                    o.address.map_or("—".into(), |a| a.to_string()),
                                    o.mask.map(|m| format!(" / {m}")).unwrap_or_default()
                                ),
                            ),
                            ("Router", list(&o.routers)),
                            ("DNS", list(&o.dns)),
                            ("Domain", o.domain.clone().unwrap_or_else(|| "—".into())),
                            (
                                "Lease",
                                o.lease_seconds.map_or("—".into(), |l| {
                                    if l >= 7200 {
                                        format!("{} hours", l / 3600)
                                    } else {
                                        format!("{} minutes", l / 60)
                                    }
                                }),
                            ),
                            ("Answered in", format!("{} ms", o.millis)),
                        ];
                        for (k, v) in rows {
                            theme::info_row(ui, p, k, &v, false);
                        }
                        if let Some(r) = o.relay {
                            theme::info_row(ui, p, "Through relay", &r.to_string(), false);
                        }
                        if !o.ntp.is_empty() {
                            theme::info_row(ui, p, "Time servers", &list(&o.ntp), false);
                        }
                        if o.tftp_server.is_some() || o.boot_file.is_some() {
                            let boot = format!(
                                "{} {}",
                                o.tftp_server.clone().unwrap_or_default(),
                                o.boot_file.clone().unwrap_or_default()
                            );
                            theme::info_row(ui, p, "Boot server", boot.trim(), false);
                        }
                    });
                });
            }
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Asked for hardware address {}", r.mac)).color(p.weak).size(12.0));
        });
    }
}

fn neighbor(ui: &mut Ui, p: &Palette, sh: &mut Shared, n: &Neighbor) {
    egui::Frame::new().fill(p.card_alt).corner_radius(12).inner_margin(14).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            theme::icon_badge(ui, p, icon::TREE_STRUCTURE, p.accent, 52.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(theme::semibold(n.system_name.as_deref().unwrap_or("Switch"), 19.0).color(p.text));
                    theme::pill(ui, p, &n.protocol, p.deep);
                });
                let port = n.port().unwrap_or("?");
                let vlan = n.vlan.map(|v| format!(" · VLAN {v}")).unwrap_or_default();
                ui.label(theme::semibold(format!("Port {port}{vlan}"), 16.0).color(p.accent));
            });
        });
        ui.add_space(8.0);
        let mut copied = false;
        egui::Grid::new(("neighbor", &n.chassis_id, &n.port_id)).num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
            let vlans = if n.vlan_names.is_empty() {
                None
            } else {
                Some(n.vlan_names.iter().map(|(id, name)| format!("{id} {name}")).collect::<Vec<_>>().join(", "))
            };
            let rows: [(&str, Option<String>); 12] = [
                ("Port", n.port_id.clone()),
                ("Port description", n.port_description.clone()),
                ("VLAN (untagged)", n.vlan.map(|v| v.to_string())),
                ("Voice VLAN", n.voice_vlan.map(|v| v.to_string())),
                ("VLAN names", vlans),
                ("Management address", (!n.management.is_empty()).then(|| n.management.join(", "))),
                ("Model", n.platform.clone()),
                ("Link", n.link.clone().or_else(|| n.duplex.clone().map(|d| format!("{d} duplex")))),
                ("Power (PoE)", n.poe.clone()),
                ("Capabilities", (!n.capabilities.is_empty()).then(|| n.capabilities.join(", "))),
                ("VTP domain", n.vtp_domain.clone()),
                ("Switch ID", n.chassis_id.clone().filter(|c| Some(c) != n.system_name.as_ref())),
            ];
            for (k, v) in rows {
                if let Some(v) = v {
                    copied |= theme::info_row(ui, p, k, &v, true);
                }
            }
            if let Some(d) = &n.description {
                let short: String = d.lines().take(2).collect::<Vec<_>>().join(" ");
                copied |= theme::info_row(ui, p, "Software", &short, false);
            }
        });
        if copied {
            sh.toast("Copied.");
        }
        if let Some(ip) = n.management.iter().find(|m| m.parse::<std::net::IpAddr>().is_ok()) {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(icon_label(icon::TERMINAL_WINDOW, "SSH to the switch")).clicked()
                    && let Err(e) = netmgr::console::open_terminal(&format!("ssh {ip}"))
                {
                    sh.fail("The terminal could not be opened.", &e);
                }
                if ui.button(icon_label(icon::PULSE, "Ping")).clicked() {
                    sh.nav = Some(Nav::Tool(crate::tools::Tab::Ping, ip.clone()));
                }
                if ui.button(icon_label(icon::ARROW_SQUARE_OUT, "Web page")).clicked() {
                    crate::app::open_path(&format!("https://{ip}"));
                }
            });
        }
    });
}
