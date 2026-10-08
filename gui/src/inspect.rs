//! More tools: web check (TLS certificate and HTTP), WHOIS, connections and
//! listening ports, the route table, and the hosts file.

use std::net::IpAddr;

use eframe::egui::{self, Align, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use netmgr::system::{self, Connection, HostsLine, Route};
use netmgr::web::{self, HttpHop, TlsReport, Whois};

use crate::app::Shared;
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// The certificate check and the HTTP check, each with its error.
type WebResult = (Result<TlsReport, String>, Result<Vec<HttpHop>, String>);

#[derive(Default)]
pub struct Inspect {
    web_target: String,
    web: Option<Job<WebResult>>,
    web_result: Option<WebResult>,
    whois_query: String,
    whois: Option<Job<Whois>>,
    whois_result: Option<Whois>,
    conns: Option<Job<Vec<Connection>>>,
    conns_result: Option<Vec<Connection>>,
    listening_only: bool,
    conn_filter: String,
    routes: Option<Job<Vec<Route>>>,
    routes_result: Option<Vec<Route>>,
    route_net: String,
    route_gw: String,
    route_persistent: bool,
    route_job: Option<Job<String>>,
    hosts: Option<Vec<HostsLine>>,
    hosts_dirty: bool,
    host_ip: String,
    host_name: String,
    hosts_job: Option<Job<()>>,
}

impl Inspect {
    pub fn running(&self) -> bool {
        self.web.is_some()
            || self.whois.is_some()
            || self.conns.is_some()
            || self.routes.is_some()
            || self.route_job.is_some()
            || self.hosts_job.is_some()
    }

    pub fn poll(&mut self, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.web) {
            self.web_result = r.ok();
        }
        if let Some(r) = jobs::finished(&mut self.whois) {
            match r {
                Ok(w) => self.whois_result = Some(w),
                Err(e) => sh.fail("The WHOIS lookup failed.", &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.conns) {
            match r {
                Ok(c) => self.conns_result = Some(c),
                Err(e) => sh.fail("The connections could not be read.", &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.routes) {
            match r {
                Ok(c) => self.routes_result = Some(c),
                Err(e) => sh.fail("The routes could not be read.", &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.route_job) {
            match r {
                Ok(m) => {
                    sh.toast(m);
                    self.routes_result = None;
                }
                Err(e) => sh.fail("The route could not be changed.", &e),
            }
        }
        if let Some(r) = jobs::finished(&mut self.hosts_job) {
            match r {
                Ok(()) => {
                    sh.toast("The hosts file was saved.");
                    self.hosts_dirty = false;
                }
                Err(e) => sh.fail("The hosts file could not be saved.", &e),
            }
        }
    }

    pub fn open_web(&mut self, target: &str) {
        self.web_target = target.to_string();
    }

    pub fn open_whois(&mut self, q: &str) {
        self.whois_query = q.to_string();
    }

    // -------------------------------------------------------------------

    pub fn web_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut go = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.web_target)
                    .hint_text("example.com, https://example.com/path or host:8443")
                    .desired_width(340.0),
            );
            go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if self.web.is_some() {
                ui.spinner();
            } else if theme::primary_button(
                ui,
                p,
                &icon_label(icon::SHIELD_CHECK, "Check"),
                !self.web_target.trim().is_empty(),
            )
            .clicked()
            {
                go = true;
            }
        });
        if go && self.web.is_none() && !self.web_target.trim().is_empty() {
            let t = self.web_target.trim().to_string();
            sh.settings.remember_host(&t);
            self.web = Some(Job::spawn(ui.ctx(), move |_, _| {
                let host = t
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let tls = web::tls(&host).map_err(|e| format!("{e:#}"));
                let http = web::http(&t).map_err(|e| format!("{e:#}"));
                Ok((tls, http))
            }));
        }
        ui.add_space(10.0);
        let Some((tls, http)) = &self.web_result else {
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                theme::paragraph(
                    ui,
                    "Checks a web server: its certificate (who issued it, for which names, when it expires, whether it is trusted), the TLS version, and every redirect with the security headers.",
                    14.0,
                    p.weak,
                );
            });
            return;
        };
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                theme::section_title(ui, p, icon::LOCK, "Certificate");
                match tls {
                    Err(e) => theme::notice(ui, p, p.danger, icon::WARNING_CIRCLE, e),
                    Ok(r) => {
                        match &r.problem {
                            None => theme::notice(
                                ui,
                                p,
                                p.success,
                                icon::CHECK_CIRCLE,
                                &format!("Trusted — {} with {}", r.version, r.cipher),
                            ),
                            Some(e) => theme::notice(
                                ui,
                                p,
                                p.danger,
                                icon::WARNING,
                                &format!("{e} ({}, {})", r.version, r.cipher),
                            ),
                        }
                        for (i, c) in r.chain.iter().enumerate() {
                            ui.add_space(8.0);
                            egui::Frame::new().fill(p.card_alt).corner_radius(10).inner_margin(12).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let title = if i == 0 { "Server certificate" } else { "Issuer certificate" };
                                ui.label(theme::semibold(title, 14.5).color(p.text));
                                let days = c.days_left();
                                egui::Grid::new(("cert", i)).num_columns(2).spacing([20.0, 5.0]).show(ui, |ui| {
                                    theme::info_row(ui, p, "Subject", &c.subject, false);
                                    theme::info_row(ui, p, "Issued by", &c.issuer, false);
                                    ui.label(RichText::new("Valid").color(p.weak).size(13.5));
                                    let color = if days < 0 {
                                        p.danger
                                    } else if days < 21 {
                                        p.warning
                                    } else {
                                        p.success
                                    };
                                    ui.label(
                                        RichText::new(format!(
                                            "{} to {} — {}",
                                            web::date(c.not_before),
                                            web::date(c.not_after),
                                            if days < 0 {
                                                format!("expired {} days ago", -days)
                                            } else {
                                                format!("{days} days left")
                                            }
                                        ))
                                        .color(color),
                                    );
                                    ui.end_row();
                                    if !c.names.is_empty() {
                                        let names = if c.names.len() > 12 {
                                            format!("{} … ({} names)", c.names[..12].join(", "), c.names.len())
                                        } else {
                                            c.names.join(", ")
                                        };
                                        theme::info_row(ui, p, "Names", &names, false);
                                    }
                                    theme::info_row(ui, p, "Key", &format!("{} · {}", c.key, c.signature), false);
                                    theme::info_row(ui, p, "Serial", &c.serial, true);
                                });
                            });
                        }
                    }
                }
            });
            ui.add_space(10.0);
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                theme::section_title(ui, p, icon::GLOBE, "HTTP");
                match http {
                    Err(e) => theme::notice(ui, p, p.danger, icon::WARNING_CIRCLE, e),
                    Ok(hops) => {
                        for h in hops {
                            ui.horizontal(|ui| {
                                let c = match h.status {
                                    200..=299 => p.success,
                                    300..=399 => p.accent,
                                    _ => p.danger,
                                };
                                theme::pill(ui, p, &h.status.to_string(), c);
                                ui.label(RichText::new(&h.url).monospace().color(p.text));
                                ui.label(RichText::new(format!("{} ms", h.millis)).color(p.weak).size(12.5));
                            });
                            for (k, v) in &h.headers {
                                ui.horizontal(|ui| {
                                    ui.add_space(52.0);
                                    ui.label(RichText::new(format!("{k}:")).color(p.weak).size(12.5));
                                    ui.add(egui::Label::new(RichText::new(v).size(12.5).color(p.text)).truncate());
                                });
                            }
                            ui.add_space(4.0);
                        }
                        if let Some(last) = hops.last() {
                            let missing: Vec<&str> =
                                ["strict-transport-security", "content-security-policy", "x-content-type-options"]
                                    .into_iter()
                                    .filter(|h| {
                                        last.url.starts_with("https") && !last.headers.iter().any(|(k, _)| k == h)
                                    })
                                    .collect();
                            if !missing.is_empty() {
                                theme::paragraph(
                                    ui,
                                    &format!("Security headers not sent: {}.", missing.join(", ")),
                                    12.5,
                                    p.warning,
                                );
                            }
                        }
                    }
                }
            });
        });
    }

    pub fn whois_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut go = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.whois_query)
                    .hint_text("example.com, 8.8.8.8 or AS13335")
                    .desired_width(300.0),
            );
            go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if self.whois.is_some() {
                ui.spinner();
            } else if theme::primary_button(
                ui,
                p,
                &icon_label(icon::MAGNIFYING_GLASS, "Look up"),
                !self.whois_query.trim().is_empty(),
            )
            .clicked()
            {
                go = true;
            }
            if let Some(Ok(info)) = &sh.public
                && ui.button(format!("My public IP ({})", info.ip)).clicked()
            {
                self.whois_query = info.ip.clone();
                go = true;
            }
        });
        if go && self.whois.is_none() {
            let q = self.whois_query.trim().to_string();
            self.whois = Some(Job::spawn(ui.ctx(), move |_, _| web::whois(&q)));
        }
        ui.add_space(10.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            let Some(w) = &self.whois_result else {
                theme::paragraph(
                    ui,
                    "Who owns a domain, an IP address or an AS number: the registrar, the network and its range, the organisation, the dates and the abuse contact. Asked from the registries' RDAP service.",
                    14.0,
                    p.weak,
                );
                return;
            };
            ui.label(theme::semibold(w.name.clone().unwrap_or_else(|| self.whois_query.clone()), 19.0).color(p.text));
            ui.label(RichText::new(&w.kind).color(p.weak));
            ui.add_space(8.0);
            let mut copied = false;
            egui::Grid::new("whois").num_columns(2).spacing([24.0, 7.0]).show(ui, |ui| {
                let rows = [
                    ("Registrar", w.registrar.clone()),
                    ("Organisation", w.organisation.clone()),
                    ("Range", w.range.clone()),
                    ("Handle", w.handle.clone()),
                    ("Country", w.country.clone()),
                    ("Registered", w.registered.clone()),
                    ("Last changed", w.changed.clone()),
                    ("Expires", w.expires.clone()),
                    ("Name servers", (!w.nameservers.is_empty()).then(|| w.nameservers.join("\n"))),
                    ("Status", (!w.status.is_empty()).then(|| w.status.join(", "))),
                    ("Abuse contact", w.abuse.clone()),
                ];
                for (k, v) in rows {
                    if let Some(v) = v {
                        copied |= theme::info_row(ui, p, k, &v, true);
                    }
                }
            });
            if copied {
                sh.toast("Copied.");
            }
        });
    }

    pub fn connections_tab(&mut self, ui: &mut Ui, p: &Palette) {
        if self.conns_result.is_none() && self.conns.is_none() {
            self.conns = Some(Job::spawn(ui.ctx(), |_, _| system::connections()));
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.listening_only, true, "Listening ports");
            ui.selectable_value(&mut self.listening_only, false, "All connections");
            ui.add(
                egui::TextEdit::singleline(&mut self.conn_filter)
                    .hint_text(format!("{}  Filter by program, port or address", icon::MAGNIFYING_GLASS))
                    .desired_width(260.0),
            );
            if self.conns.is_some() {
                ui.spinner();
            } else if ui.button(icon_label(icon::ARROWS_CLOCKWISE, "Refresh")).clicked() {
                self.conns = Some(Job::spawn(ui.ctx(), |_, _| system::connections()));
            }
        });
        ui.add_space(8.0);
        let q = self.conn_filter.to_lowercase();
        let list: Vec<Connection> = self
            .conns_result
            .iter()
            .flatten()
            .filter(|c| !self.listening_only || c.listening())
            .filter(|c| {
                q.is_empty()
                    || c.process.as_deref().unwrap_or("").to_lowercase().contains(&q)
                    || c.local_port.is_some_and(|p| p.to_string() == q)
                    || c.local.contains(&q)
                    || c.remote.contains(&q)
            })
            .cloned()
            .collect();
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{} shown", list.len())).color(p.weak).size(12.5));
            let table_height = ui.available_height() - 40.0;
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .max_scroll_height(table_height)
                .column(Column::exact(50.0))
                .column(Column::exact(70.0))
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::exact(110.0))
                .column(Column::remainder().at_least(140.0).clip(true))
                .header(24.0, |mut h| {
                    for t in ["", "Port", "Local address", "Remote address", "State", "Program"] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).color(p.weak).size(13.0));
                        });
                    }
                })
                .body(|body| {
                    body.rows(26.0, list.len(), |mut row| {
                        let c = &list[row.index()];
                        row.col(|ui| {
                            ui.label(RichText::new(&c.proto).size(12.5).color(p.weak));
                        });
                        row.col(|ui| {
                            let port = c.local_port.map_or("*".into(), |p| p.to_string());
                            ui.label(RichText::new(port).monospace().color(p.text))
                                .on_hover_text(c.local_port.map(netmgr::scan::service_name).unwrap_or(""));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&c.local).monospace().size(12.5));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&c.remote).monospace().size(12.5).color(p.weak));
                        });
                        row.col(|ui| {
                            if !c.state.is_empty() {
                                let color = if c.state == "LISTEN" {
                                    p.accent
                                } else if c.state == "ESTABLISHED" {
                                    p.success
                                } else {
                                    p.weak
                                };
                                theme::pill(ui, p, &c.state, color);
                            }
                        });
                        row.col(|ui| {
                            let text = match (&c.process, c.pid) {
                                (Some(n), Some(pid)) => format!("{n} ({pid})"),
                                (None, Some(pid)) => format!("pid {pid}"),
                                (Some(n), None) => n.clone(),
                                (None, None) => "—".into(),
                            };
                            ui.label(RichText::new(text).size(13.0));
                        });
                    });
                });
            if cfg!(not(windows)) {
                theme::paragraph(ui, "Programs of other users are shown with administrator rights only.", 12.0, p.weak);
            }
        });
    }

    pub fn routes_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if self.routes_result.is_none() && self.routes.is_none() {
            self.routes = Some(Job::spawn(ui.ctx(), |_, _| system::routes()));
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Add a route").color(p.text));
                ui.add(
                    egui::TextEdit::singleline(&mut self.route_net)
                        .hint_text("network, e.g. 10.20.0.0/16")
                        .desired_width(180.0),
                );
                ui.label(RichText::new("via").color(p.weak));
                ui.add(
                    egui::TextEdit::singleline(&mut self.route_gw)
                        .hint_text("gateway, e.g. 192.168.1.254")
                        .desired_width(170.0),
                );
                if cfg!(windows) {
                    ui.checkbox(&mut self.route_persistent, "Keep after restart");
                }
                let gw = self.route_gw.trim().parse::<IpAddr>();
                if ui
                    .add_enabled(
                        self.route_job.is_none() && gw.is_ok() && !self.route_net.trim().is_empty(),
                        egui::Button::new(icon_label(icon::PLUS, "Add")),
                    )
                    .clicked()
                    && let Ok(gw) = gw
                {
                    let (net, persistent) = (self.route_net.trim().to_string(), self.route_persistent);
                    self.route_job = Some(Job::spawn(ui.ctx(), move |_, _| {
                        system::add_route(&net, gw, persistent)?;
                        Ok(format!("Route to {net} via {gw} added."))
                    }));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.routes.is_some() || self.route_job.is_some() {
                        ui.spinner();
                    } else if ui.button(icon_label(icon::ARROWS_CLOCKWISE, "Refresh")).clicked() {
                        self.routes = Some(Job::spawn(ui.ctx(), |_, _| system::routes()));
                    }
                });
            });
        });
        ui.add_space(8.0);
        let routes = self.routes_result.clone().unwrap_or_default();
        let mut delete = None;
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            let table_height = ui.available_height() - 10.0;
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .max_scroll_height(table_height)
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::remainder().at_least(180.0).clip(true))
                .column(Column::exact(140.0))
                .column(Column::exact(70.0))
                .column(Column::exact(90.0))
                .column(Column::exact(40.0))
                .header(24.0, |mut h| {
                    for t in ["Destination", "Gateway", "Interface", "Metric", "Kind", ""] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).color(p.weak).size(13.0));
                        });
                    }
                })
                .body(|body| {
                    body.rows(26.0, routes.len(), |mut row| {
                        let r = &routes[row.index()];
                        let default = r.destination == "default"
                            || r.destination.starts_with("0.0.0.0/0")
                            || r.destination == "::/0";
                        row.col(|ui| {
                            ui.label(RichText::new(&r.destination).monospace().color(if default {
                                p.accent
                            } else {
                                p.text
                            }));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&r.gateway).monospace().size(12.5));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&r.interface).size(13.0).color(p.weak));
                        });
                        row.col(|ui| {
                            ui.label(r.metric.map(|m| m.to_string()).unwrap_or_default());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&r.flags).size(12.0).color(p.weak));
                        });
                        row.col(|ui| {
                            if !default
                                && ui
                                    .add(egui::Button::new(RichText::new(icon::TRASH).color(p.weak)).frame(false))
                                    .on_hover_text("Delete this route")
                                    .clicked()
                            {
                                delete = Some(r.destination.clone());
                            }
                        });
                    });
                });
        });
        if let Some(d) = delete
            && self.route_job.is_none()
        {
            self.route_job = Some(Job::spawn(ui.ctx(), move |_, _| {
                system::delete_route(&d)?;
                Ok(format!("Route to {d} deleted."))
            }));
        }
        let _ = sh;
    }

    pub fn hosts_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if self.hosts.is_none() {
            match system::read_hosts() {
                Ok(h) => self.hosts = Some(h),
                Err(e) => {
                    self.hosts = Some(Vec::new());
                    sh.fail("The hosts file could not be read.", &e);
                }
            }
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                &format!(
                    "Names this computer resolves itself, before asking DNS — {}.",
                    system::hosts_path().display()
                ),
                13.0,
                p.weak,
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.host_ip).hint_text("IP address").desired_width(150.0));
                ui.add(
                    egui::TextEdit::singleline(&mut self.host_name)
                        .hint_text("name(s), e.g. nas nas.lab")
                        .desired_width(240.0),
                );
                let ok = self.host_ip.trim().parse::<IpAddr>().is_ok() && !self.host_name.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new(icon_label(icon::PLUS, "Add"))).clicked()
                    && let Some(h) = self.hosts.as_mut()
                {
                    h.push(HostsLine::Entry {
                        enabled: true,
                        ip: self.host_ip.trim().to_string(),
                        names: self.host_name.split_whitespace().map(str::to_string).collect(),
                        comment: "added by Network Manager".into(),
                    });
                    self.host_ip.clear();
                    self.host_name.clear();
                    self.hosts_dirty = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.hosts_job.is_some() {
                        ui.spinner();
                    } else if theme::primary_button(ui, p, &icon_label(icon::FLOPPY_DISK, "Save"), self.hosts_dirty)
                        .clicked()
                        && let Some(h) = self.hosts.clone()
                    {
                        self.hosts_job = Some(Job::spawn(ui.ctx(), move |_, _| system::write_hosts(&h)));
                    }
                    if ui.button(icon_label(icon::ARROW_COUNTER_CLOCKWISE, "Reload")).clicked() {
                        self.hosts = None;
                        self.hosts_dirty = false;
                    }
                });
            });
        });
        ui.add_space(8.0);
        let mut remove = None;
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let Some(lines) = self.hosts.as_mut() else { return };
                let mut changed = false;
                egui::Grid::new("hosts").num_columns(5).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
                    for (i, l) in lines.iter_mut().enumerate() {
                        if let HostsLine::Entry { enabled, ip, names, comment } = l {
                            changed |= ui.checkbox(enabled, "").on_hover_text("Turn this entry on or off").changed();
                            ui.label(RichText::new(ip.as_str()).monospace().color(if *enabled {
                                p.text
                            } else {
                                p.weak
                            }));
                            ui.label(RichText::new(names.join(" ")).color(if *enabled { p.text } else { p.weak }));
                            ui.label(RichText::new(comment.as_str()).color(p.weak).size(12.5));
                            if ui
                                .add(egui::Button::new(RichText::new(icon::TRASH).color(p.weak)).frame(false))
                                .clicked()
                            {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                    }
                });
                if changed {
                    self.hosts_dirty = true;
                }
            });
        });
        if let Some(i) = remove
            && let Some(h) = self.hosts.as_mut()
        {
            h.remove(i);
            self.hosts_dirty = true;
        }
        if self.hosts_dirty {
            ui.label(
                RichText::new(
                    "Not saved yet. Saving asks for administrator rights and keeps a backup of the original file.",
                )
                .color(p.warning)
                .size(12.5),
            );
        }
    }
}
