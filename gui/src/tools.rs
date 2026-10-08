//! Tools: ping, traceroute, DNS lookup, port check, subnet calculator,
//! Wake-on-LAN and MAC address lookup.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use eframe::egui::{self, Align, Layout, RichText, Ui, Vec2};
use egui_phosphor::regular as icon;
use netmgr::dns::{self, Answer, RecordType};
use netmgr::mac::Mac;
use netmgr::subnet::Subnet;
use netmgr::tools::{self as nt, PingStats, PortResult};

use crate::app::Shared;
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// A server's label and its answer.
type DnsResult = (String, Result<Answer, String>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Ping,
    Trace,
    Dns,
    Ports,
    Subnet,
    Wol,
    MacLookup,
}

pub struct Tools {
    tab: Tab,
    host: String,
    ping: Option<Job<()>>,
    ping_lines: Vec<String>,
    trace: Option<Job<()>>,
    trace_lines: Vec<String>,
    dns_name: String,
    dns_type: RecordType,
    dns_server: String,
    dns_compare: bool,
    dns: Option<Job<Vec<DnsResult>>>,
    dns_results: Vec<DnsResult>,
    ports: String,
    port_job: Option<Job<(IpAddr, Vec<PortResult>)>>,
    port_results: Option<(IpAddr, Vec<PortResult>)>,
    show_closed: bool,
    subnet: String,
    split: u8,
    wol_mac: String,
    wol_ip: String,
    lookup: String,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tab: Tab::Ping,
            host: String::new(),
            ping: None,
            ping_lines: Vec::new(),
            trace: None,
            trace_lines: Vec::new(),
            dns_name: String::new(),
            dns_type: RecordType::A,
            dns_server: String::new(),
            dns_compare: false,
            dns: None,
            dns_results: Vec::new(),
            ports: nt::PORT_PRESETS[0].1.to_string(),
            port_job: None,
            port_results: None,
            show_closed: false,
            subnet: String::new(),
            split: 0,
            wol_mac: String::new(),
            wol_ip: String::new(),
            lookup: String::new(),
        }
    }
}

impl Tools {
    /// Opens `tab` with `host` filled in, and starts it.
    pub fn open(&mut self, tab: Tab, host: &str) {
        self.tab = tab;
        match tab {
            Tab::Dns => self.dns_name = host.to_string(),
            Tab::Subnet => self.subnet = host.to_string(),
            Tab::Wol => self.wol_mac = host.to_string(),
            Tab::MacLookup => self.lookup = host.to_string(),
            _ => self.host = host.to_string(),
        }
    }

    pub fn running(&self) -> bool {
        self.ping.is_some() || self.trace.is_some() || self.port_job.is_some() || self.dns.is_some()
    }

    pub fn poll(&mut self, sh: &mut Shared) {
        if let Some(job) = &self.ping {
            self.ping_lines = job.progress.snapshot().lines;
        }
        if let Some(r) = jobs::finished(&mut self.ping)
            && let Err(e) = r
        {
            sh.fail("Ping could not run.", &e);
        }
        if let Some(job) = &self.trace {
            self.trace_lines = job.progress.snapshot().lines;
        }
        if let Some(r) = jobs::finished(&mut self.trace)
            && let Err(e) = r
        {
            sh.fail("Traceroute could not run.", &e);
        }
        if let Some(r) = jobs::finished(&mut self.dns) {
            match r {
                Ok(v) => self.dns_results = v,
                Err(e) => sh.fail("The lookup failed.", &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.port_job) {
            match r {
                Ok(v) => self.port_results = Some(v),
                Err(e) => sh.fail("The ports could not be checked.", &e),
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::page_title(ui, p, "Tools", "Everyday network tools, in one place.");
        theme::tabs(
            ui,
            p,
            &mut self.tab,
            &[
                (Tab::Ping, icon::PULSE, "Ping"),
                (Tab::Trace, icon::PATH, "Traceroute"),
                (Tab::Dns, icon::LIST_MAGNIFYING_GLASS, "DNS lookup"),
                (Tab::Ports, icon::DOOR_OPEN, "Port check"),
                (Tab::Subnet, icon::CALCULATOR, "Subnet calculator"),
                (Tab::Wol, icon::POWER, "Wake-on-LAN"),
                (Tab::MacLookup, icon::FINGERPRINT, "MAC lookup"),
            ],
        );
        ui.add_space(10.0);
        match self.tab {
            Tab::Ping => self.ping_tab(ui, p, sh),
            Tab::Trace => self.trace_tab(ui, p, sh),
            Tab::Dns => self.dns_tab(ui, p, sh),
            Tab::Ports => self.ports_tab(ui, p, sh),
            Tab::Subnet => self.subnet_tab(ui, p, sh),
            Tab::Wol => self.wol_tab(ui, p, sh),
            Tab::MacLookup => self.mac_tab(ui, p),
        }
    }

    /// The address field with the recent hosts and quick picks.
    fn host_field(&mut self, ui: &mut Ui, sh: &Shared, hint: &str) -> bool {
        let r = ui.add(egui::TextEdit::singleline(&mut self.host).hint_text(hint).desired_width(280.0));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut picks: Vec<(String, String)> = Vec::new();
        if let Some(g) = sh.default_adapter().and_then(|a| a.gateway) {
            picks.push((format!("Router ({g})"), g.to_string()));
        }
        picks.push(("Cloudflare (1.1.1.1)".into(), "1.1.1.1".into()));
        picks.push(("Google (google.com)".into(), "google.com".into()));
        for h in &sh.settings.recent_hosts {
            picks.push((h.clone(), h.clone()));
        }
        egui::ComboBox::from_id_salt("host-picks").selected_text("Recent").width(110.0).show_ui(ui, |ui| {
            for (label, value) in picks {
                if ui.selectable_label(false, label).clicked() {
                    self.host = value;
                }
            }
        });
        enter
    }

    fn ping_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut start = false;
        ui.horizontal(|ui| {
            start = self.host_field(ui, sh, "Address or name, e.g. 192.168.1.1");
            ui.add(egui::DragValue::new(&mut sh.settings.ping_count).range(0..=1000).prefix("Count: "))
                .on_hover_text("0 pings until you click Stop");
            if let Some(job) = &self.ping {
                if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked() {
                    job.stop();
                }
            } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Ping"), !self.host.trim().is_empty())
                .clicked()
            {
                start = true;
            }
        });
        if start && self.ping.is_none() && !self.host.trim().is_empty() {
            sh.settings.remember_host(&self.host);
            let (host, count) = (self.host.trim().to_string(), sh.settings.ping_count);
            self.ping_lines.clear();
            self.ping = Some(Job::spawn(ui.ctx(), move |progress, cancel| {
                nt::ping(&host, count, cancel, |l, _| progress.line(l))
            }));
        }
        ui.add_space(10.0);
        let mut stats = PingStats::default();
        for l in &self.ping_lines {
            stats.add(nt::parse_ping_line(l));
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 40.0;
                let ms = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("{v:.1} ms"));
                theme::stat(ui, p, "Sent", &stats.sent.to_string());
                theme::stat(ui, p, "Received", &stats.received.to_string());
                theme::stat(ui, p, "Lost", &format!("{:.0} %", stats.loss_percent()));
                theme::stat(ui, p, "Fastest", &ms(stats.min()));
                theme::stat(ui, p, "Average", &ms(stats.avg()));
                theme::stat(ui, p, "Slowest", &ms(stats.max()));
                theme::stat(ui, p, "Jitter", &ms(stats.jitter()));
            });
            ui.add_space(8.0);
            let times: Vec<f64> = stats.times.iter().rev().take(120).rev().copied().collect();
            let w = ui.available_width();
            theme::sparkline(ui, &times, p.accent, Vec2::new(w, 70.0), None);
        });
        ui.add_space(10.0);
        console(ui, p, &self.ping_lines, self.ping.is_some(), "Results appear here.");
    }

    fn trace_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut start = false;
        ui.horizontal(|ui| {
            start = self.host_field(ui, sh, "Address or name, e.g. google.com");
            if let Some(job) = &self.trace {
                if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked() {
                    job.stop();
                }
            } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Trace route"), !self.host.trim().is_empty())
                .clicked()
            {
                start = true;
            }
        });
        if start && self.trace.is_none() && !self.host.trim().is_empty() {
            sh.settings.remember_host(&self.host);
            let host = self.host.trim().to_string();
            self.trace_lines.clear();
            self.trace =
                Some(Job::spawn(ui.ctx(), move |progress, cancel| nt::traceroute(&host, cancel, |l| progress.line(l))));
        }
        ui.add_space(6.0);
        theme::paragraph(
            ui,
            "Shows every router between this computer and the address, and how long each takes to answer. * means a router did not answer, which is often normal.",
            12.5,
            p.weak,
        );
        ui.add_space(8.0);
        console(ui, p, &self.trace_lines, self.trace.is_some(), "The route appears here, one router per line.");
    }

    fn dns_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let system: Vec<IpAddr> = sh
            .default_adapter()
            .map(|a| {
                a.dns.iter().copied().filter(|d| !matches!(d, IpAddr::V6(v) if v.is_unicast_link_local())).collect()
            })
            .unwrap_or_default();
        let mut start = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.dns_name)
                    .hint_text("Name, e.g. example.com (or an IP for PTR)")
                    .desired_width(280.0),
            );
            start |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            egui::ComboBox::from_id_salt("dns-type").selected_text(self.dns_type.name()).width(80.0).show_ui(
                ui,
                |ui| {
                    for t in RecordType::ALL {
                        ui.selectable_value(&mut self.dns_type, t, t.name());
                    }
                },
            );
            ui.label(RichText::new("at").color(p.weak));
            let shown =
                if self.dns_server.is_empty() { "This computer's DNS".to_string() } else { self.dns_server.clone() };
            egui::ComboBox::from_id_salt("dns-server").selected_text(shown).width(170.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.dns_server, String::new(), "This computer's DNS");
                for (n, ip) in dns::PUBLIC_RESOLVERS {
                    ui.selectable_value(&mut self.dns_server, ip.to_string(), format!("{n} ({ip})"));
                }
            });
            ui.checkbox(&mut self.dns_compare, "Compare with public DNS")
                .on_hover_text("Ask Cloudflare, Google, Quad9 and OpenDNS too — handy after changing a record");
            if self.dns.is_some() {
                ui.spinner();
            } else if theme::primary_button(
                ui,
                p,
                &icon_label(icon::MAGNIFYING_GLASS, "Look up"),
                !self.dns_name.trim().is_empty(),
            )
            .clicked()
            {
                start = true;
            }
        });
        if start && self.dns.is_none() && !self.dns_name.trim().is_empty() {
            let mut servers: Vec<(String, IpAddr)> = Vec::new();
            match self.dns_server.parse::<IpAddr>() {
                Ok(ip) => servers.push((ip.to_string(), ip)),
                Err(_) => {
                    if let Some(s) = system.first() {
                        servers.push((format!("This computer ({s})"), *s));
                    } else {
                        servers.push((
                            "Cloudflare (1.1.1.1)".into(),
                            "1.1.1.1".parse().unwrap_or(IpAddr::from([1, 1, 1, 1])),
                        ));
                    }
                }
            }
            if self.dns_compare {
                for (n, ip) in dns::PUBLIC_RESOLVERS {
                    if let Ok(ip) = ip.parse::<IpAddr>()
                        && !servers.iter().any(|(_, s)| *s == ip)
                    {
                        servers.push((format!("{n} ({ip})"), ip));
                    }
                }
            }
            let (name, kind) = (self.dns_name.trim().to_string(), self.dns_type);
            self.dns = Some(Job::spawn(ui.ctx(), move |_, _| {
                let results = std::thread::scope(|s| {
                    let handles: Vec<_> = servers
                        .iter()
                        .map(|(label, ip)| {
                            let name = name.clone();
                            s.spawn(move || {
                                (
                                    label.clone(),
                                    dns::query(SocketAddr::new(*ip, 53), &name, kind, Duration::from_secs(3))
                                        .map_err(|e| format!("{e:#}")),
                                )
                            })
                        })
                        .collect();
                    handles.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>()
                });
                Ok(results)
            }));
        }
        ui.add_space(10.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.dns_results.is_empty() {
                theme::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    theme::paragraph(ui, "Looks up the records of a name directly at a DNS server: addresses (A, AAAA), mail servers (MX), text records (TXT), name servers (NS) and more.", 14.0, p.weak);
                });
            }
            let mut copied = false;
            for (label, r) in &self.dns_results {
                theme::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(theme::semibold(label, 15.0).color(p.text));
                        match r {
                            Ok(a) => {
                                let color = if a.status == "NOERROR" { p.success } else { p.warning };
                                theme::pill(ui, p, a.status, color);
                                ui.label(RichText::new(format!("{} ms", a.millis)).color(p.weak).size(13.0));
                            }
                            Err(_) => {
                                theme::pill(ui, p, "No answer", p.danger);
                            }
                        }
                    });
                    match r {
                        Ok(a) if a.records.is_empty() => {
                            ui.label(RichText::new(if a.status == "NXDOMAIN" { "This name does not exist." } else { "No records of this type." }).color(p.weak));
                        }
                        Ok(a) => {
                            egui::Grid::new(("dns", label)).num_columns(3).spacing([18.0, 4.0]).show(ui, |ui| {
                                for rec in &a.records {
                                    ui.label(RichText::new(&rec.kind).monospace().color(p.accent));
                                    ui.label(RichText::new(format!("{}s", rec.ttl)).color(p.weak).size(12.5));
                                    let resp = ui.add(egui::Label::new(RichText::new(&rec.data).monospace().color(p.text)).sense(egui::Sense::click()));
                                    if resp.on_hover_text("Click to copy").clicked() {
                                        ui.ctx().copy_text(rec.data.clone());
                                        copied = true;
                                    }
                                    ui.end_row();
                                }
                            });
                        }
                        Err(e) => {
                            ui.label(RichText::new(e).color(p.danger));
                        }
                    }
                });
                ui.add_space(8.0);
            }
            if copied {
                sh.toast("Copied.");
            }
        });
    }

    fn ports_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut start = false;
        ui.horizontal(|ui| {
            start = self.host_field(ui, sh, "Address or name");
            if let Some(job) = &self.port_job {
                if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked() {
                    job.stop();
                }
            } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Check"), !self.host.trim().is_empty())
                .clicked()
            {
                start = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Ports").color(p.weak));
            ui.add(
                egui::TextEdit::singleline(&mut self.ports).hint_text("22, 80, 443, 8000-8100").desired_width(360.0),
            );
            egui::ComboBox::from_id_salt("port-presets").selected_text("Presets").width(110.0).show_ui(ui, |ui| {
                for (name, list) in nt::PORT_PRESETS {
                    if ui.selectable_label(false, *name).clicked() {
                        self.ports = list.to_string();
                    }
                }
                if ui.selectable_label(false, "First 1024").clicked() {
                    self.ports = "1-1024".into();
                }
            });
        });
        if start && self.port_job.is_none() && !self.host.trim().is_empty() {
            match nt::parse_ports(&self.ports) {
                Ok(list) => {
                    sh.settings.remember_host(&self.host);
                    let host = self.host.trim().to_string();
                    let total = list.len();
                    self.port_results = None;
                    self.port_job = Some(Job::spawn(ui.ctx(), move |progress, cancel| {
                        nt::check_ports(&host, &list, Duration::from_millis(1200), cancel, &|n| {
                            progress.set(n as f32 / total as f32, "")
                        })
                    }));
                }
                Err(e) => sh.error = Some(format!("{e:#}")),
            }
        }
        ui.add_space(10.0);
        if let Some(job) = &self.port_job {
            let st = job.progress.snapshot();
            ui.add(egui::ProgressBar::new(st.fraction.unwrap_or(0.0)).show_percentage().fill(p.accent));
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            let Some((ip, results)) = &self.port_results else {
                theme::paragraph(
                    ui,
                    "Checks which services of a device or server accept connections (TCP). Only check devices you own or may test.",
                    14.0,
                    p.weak,
                );
                return;
            };
            let open: Vec<&PortResult> = results.iter().filter(|r| r.open).collect();
            ui.horizontal(|ui| {
                ui.label(
                    theme::semibold(format!("{} of {} ports open on {ip}", open.len(), results.len()), 16.0)
                        .color(p.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.checkbox(&mut self.show_closed, "Show closed");
                });
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(ui.available_height() - 10.0).show(ui, |ui| {
                egui::Grid::new("ports").num_columns(4).spacing([24.0, 6.0]).striped(true).show(ui, |ui| {
                    for r in results.iter().filter(|r| r.open || self.show_closed) {
                        ui.label(RichText::new(r.port.to_string()).monospace().color(p.text));
                        if r.open {
                            theme::pill(ui, p, "Open", p.success);
                        } else {
                            theme::pill(ui, p, "Closed", p.weak);
                        }
                        ui.label(RichText::new(r.service).color(p.weak));
                        ui.label(
                            RichText::new(r.millis.map_or(String::new(), |m| format!("{m} ms")))
                                .color(p.weak)
                                .size(12.5),
                        );
                        ui.end_row();
                    }
                });
            });
        });
    }

    fn subnet_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if self.subnet.is_empty()
            && let Some((ip, prefix)) = sh.default_adapter().and_then(|a| a.main_ipv4())
        {
            self.subnet = format!("{ip}/{prefix}");
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.subnet)
                    .hint_text("192.168.1.10/24 or 10.0.0.5 255.255.0.0")
                    .desired_width(320.0),
            );
        });
        ui.add_space(10.0);
        let parsed = Subnet::parse(&self.subnet);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            match &parsed {
                Err(e) => {
                    ui.label(RichText::new(format!("{e:#}")).color(p.warning));
                }
                Ok(s) => {
                    let mut copied = false;
                    egui::Grid::new("subnet").num_columns(2).spacing([28.0, 8.0]).show(ui, |ui| {
                        copied |= theme::info_row(ui, p, "Network", &format!("{}/{}", s.network, s.prefix), true);
                        copied |= theme::info_row(ui, p, "Subnet mask", &s.mask.to_string(), true);
                        copied |= theme::info_row(ui, p, "Wildcard", &s.wildcard.to_string(), true);
                        copied |= theme::info_row(ui, p, "Broadcast", &s.broadcast.to_string(), true);
                        copied |=
                            theme::info_row(ui, p, "Usable addresses", &format!("{} – {}", s.first, s.last), true);
                        copied |= theme::info_row(ui, p, "Number of hosts", &s.hosts.to_string(), false);
                        copied |= theme::info_row(ui, p, "Mask in binary", &s.mask_binary(), true);
                        copied |= theme::info_row(ui, p, "Kind", &format!("{}, class {}", s.scope(), s.class()), false);
                    });
                    if copied {
                        sh.toast("Copied.");
                    }
                }
            }
        });
        if let Ok(s) = parsed
            && s.prefix < 32
        {
            ui.add_space(10.0);
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                if self.split <= s.prefix {
                    self.split = (s.prefix + 2).min(32);
                }
                ui.horizontal(|ui| {
                    ui.label(theme::semibold("Split into", 15.0).color(p.text));
                    ui.add(egui::Slider::new(&mut self.split, (s.prefix + 1)..=32).prefix("/"));
                    let count = 1u64 << (self.split - s.prefix);
                    let each = Subnet::new(s.network, self.split).map(|x| x.hosts).unwrap_or(0);
                    ui.label(RichText::new(format!("{count} subnets of {each} hosts")).color(p.weak));
                });
                if let Ok(parts) = s.split(self.split, 256) {
                    egui::ScrollArea::vertical().max_height(ui.available_height().max(120.0)).show(ui, |ui| {
                        egui::Grid::new("split").num_columns(3).spacing([24.0, 4.0]).striped(true).show(ui, |ui| {
                            for x in &parts {
                                ui.label(
                                    RichText::new(format!("{}/{}", x.network, x.prefix)).monospace().color(p.text),
                                );
                                ui.label(RichText::new(format!("{} – {}", x.first, x.last)).monospace().color(p.weak));
                                ui.label(
                                    RichText::new(format!("broadcast {}", x.broadcast))
                                        .monospace()
                                        .color(p.weak)
                                        .size(12.5),
                                );
                                ui.end_row();
                            }
                        });
                        if (1u64 << (self.split - s.prefix)) > 256 {
                            ui.label(RichText::new("The first 256 are shown.").color(p.weak).size(12.5));
                        }
                    });
                }
            });
        }
    }

    fn wol_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                "Turns on a computer over the network. Wake-on-LAN must be enabled in its BIOS/UEFI and network adapter settings, and it must be connected by cable.",
                14.0,
                p.weak,
            );
            ui.add_space(10.0);
            egui::Grid::new("wol").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("MAC address");
                ui.add(
                    egui::TextEdit::singleline(&mut self.wol_mac).hint_text("AA:BB:CC:DD:EE:FF").desired_width(220.0),
                );
                ui.end_row();
                ui.label("Send to");
                ui.add(
                    egui::TextEdit::singleline(&mut self.wol_ip)
                        .hint_text("optional: broadcast or host IP")
                        .desired_width(220.0),
                );
                ui.end_row();
            });
            ui.add_space(10.0);
            let mac = self.wol_mac.parse::<Mac>();
            if theme::primary_button(ui, p, &icon_label(icon::POWER, "Wake up"), mac.is_ok()).clicked()
                && let Ok(m) = mac
            {
                let target = self.wol_ip.trim().parse().ok();
                match nt::wake(m, target) {
                    Ok(()) => sh.toast(format!("Wake-on-LAN sent to {m}.")),
                    Err(e) => sh.fail("The packet could not be sent.", &e),
                }
            }
        });
    }

    fn mac_tab(&mut self, ui: &mut Ui, p: &Palette) {
        ui.add(
            egui::TextEdit::singleline(&mut self.lookup)
                .hint_text("A MAC address, e.g. B8:27:EB:12:34:56")
                .desired_width(320.0),
        );
        ui.add_space(10.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            match self.lookup.parse::<Mac>() {
                Err(_) if self.lookup.trim().is_empty() => theme::paragraph(
                    ui,
                    "Finds the maker of a network device from the first half of its MAC address (IEEE registry, built in — nothing is sent anywhere).",
                    14.0,
                    p.weak,
                ),
                Err(_) => {
                    ui.label(RichText::new("Write it as 6 pairs of hex digits.").color(p.warning));
                }
                Ok(m) => {
                    egui::Grid::new("maclookup").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                        theme::info_row(ui, p, "Address", &m.to_string(), true);
                        let maker = if m.is_local() {
                            "None: a private or made-up address".to_string()
                        } else {
                            m.vendor().unwrap_or("Not in the registry").to_string()
                        };
                        theme::info_row(ui, p, "Maker", &maker, false);
                        theme::info_row(
                            ui,
                            p,
                            "Kind",
                            if m.is_multicast() { "Multicast (a group, not a device)" } else { "Unicast (one device)" },
                            false,
                        );
                        theme::info_row(
                            ui,
                            p,
                            "Assigned",
                            if m.is_local() { "Locally, by software" } else { "By the manufacturer" },
                            false,
                        );
                        theme::info_row(
                            ui,
                            p,
                            "Other forms",
                            &format!("{}\n{}\n{}", m.dashes(), m.plain(), cisco(m)),
                            true,
                        );
                    });
                }
            }
        });
    }
}

/// `aabb.ccdd.eeff`, as Cisco writes MAC addresses.
fn cisco(m: Mac) -> String {
    let h = m.plain().to_lowercase();
    format!("{}.{}.{}", &h[0..4], &h[4..8], &h[8..12])
}

/// Command output in a dark, monospaced box that follows new lines.
fn console(ui: &mut Ui, p: &Palette, lines: &[String], running: bool, empty: &str) {
    theme::card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            if running {
                ui.spinner();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !lines.is_empty() && ui.button(icon_label(icon::COPY, "Copy")).clicked() {
                    ui.ctx().copy_text(lines.join("\n"));
                }
            });
        });
        egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(8).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .max_height(ui.available_height().max(160.0) - 10.0)
                .auto_shrink(false)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if lines.is_empty() {
                        ui.label(RichText::new(empty).color(p.weak));
                    }
                    for l in lines {
                        let color = match nt::parse_ping_line(l) {
                            nt::PingLine::Lost => p.danger,
                            _ => p.text,
                        };
                        ui.label(RichText::new(l).monospace().size(12.5).color(color));
                    }
                });
        });
    });
}
