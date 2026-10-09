//! Devices → Map, Free addresses and Services (Bonjour).

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use eframe::egui::{self, RichText, Sense, Stroke, Ui, Vec2, pos2};
use egui_phosphor::regular as icon;
use netmgr::mdns::{self, Service};
use netmgr::scan::{self, AddressCheck, Device, Range};
use netmgr::snmp::{self, Switch, Version};

use crate::app::Shared;
use crate::i18n::{tr, tr_dyn, trf, trl, trlf};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

// ---------------------------------------------------------------- Bonjour

#[derive(Default)]
pub struct Bonjour {
    job: Option<Job<Vec<Service>>>,
    list: Option<Vec<Service>>,
    filter: String,
}

impl Bonjour {
    /// Development aid: sample services for README screenshots.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        let s = |name: &str, kind: &str, host: &str, port: u16, ip: &str, details: &[&str]| Service {
            name: name.into(),
            kind: kind.into(),
            host: host.into(),
            port,
            addresses: vec![ip.parse().expect("valid")],
            details: details.iter().map(|d| d.to_string()).collect(),
        };
        self.list = Some(vec![
            s(
                "Office Printer",
                "_ipp._tcp",
                "brw-office.local",
                631,
                "192.168.1.31",
                &["ty=Brother HL-L3270CDW", "Color=T", "Duplex=T"],
            ),
            s("Office Printer", "_scanner._tcp", "brw-office.local", 80, "192.168.1.31", &["ty=Brother HL-L3270CDW"]),
            s("Living Room", "_airplay._tcp", "living-room.local", 7000, "192.168.1.60", &["model=AppleTV14,1"]),
            s(
                "Living Room TV",
                "_googlecast._tcp",
                "chromecast.local",
                8009,
                "192.168.1.61",
                &["md=Chromecast", "fn=Living Room TV"],
            ),
            s("nas", "_smb._tcp", "nas.local", 445, "192.168.1.50", &[]),
            s("nas", "_http._tcp", "nas.local", 5000, "192.168.1.50", &["path=/"]),
            s("pi-hole", "_ssh._tcp", "pi-hole.local", 22, "192.168.1.20", &[]),
            s("Hue Bridge", "_hap._tcp", "hue-bridge.local", 8080, "192.168.1.70", &["md=BSB002"]),
        ]);
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(v) => self.list = Some(v),
                Err(e) => sh.fail(trl("The services could not be listed."), &e),
            }
        }
        if self.list.is_none() && self.job.is_none() {
            self.job = Some(Job::spawn(ui.ctx(), |_, _| mdns::browse(Duration::from_secs(4))));
        }
        ui.horizontal_wrapped(|ui| {
            theme::paragraph(
                ui,
                trl("What devices announce on the network (Bonjour / mDNS): printers and scanners, AirPlay and Chromecast, file shares, smart-home hubs."),
                14.0,
                p.weak,
            );
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(tr("Filter")).desired_width(220.0));
            if self.job.is_some() {
                ui.spinner();
                ui.label(RichText::new(tr("Listening…")).color(p.weak));
            } else if ui.button(icon_label(icon::ARROWS_CLOCKWISE, "Look again")).clicked() {
                self.list = None;
            }
        });
        ui.add_space(8.0);
        let Some(list) = &self.list else { return };
        if list.is_empty() {
            theme::notice(
                ui,
                p,
                p.weak,
                icon::INFO,
                trl(
                    "No services answered. On macOS, allow Network Manager in System Settings → Privacy & Security → Local Network.",
                ),
            );
            return;
        }
        let f = self.filter.to_lowercase();
        let mut groups: BTreeMap<(bool, String), Vec<&Service>> = BTreeMap::new();
        for s in list.iter().filter(|s| {
            f.is_empty()
                || s.name.to_lowercase().contains(&f)
                || s.kind.contains(&f)
                || mdns::describe(&s.kind).to_lowercase().contains(&f)
                || s.addresses.iter().any(|a| a.to_string().contains(&f))
        }) {
            let what = mdns::describe(&s.kind);
            let label = if what.is_empty() { s.kind.clone() } else { tr_dyn(what) };
            groups.entry((what.is_empty(), label)).or_default().push(s);
        }
        egui::ScrollArea::vertical().id_salt("bonjour").auto_shrink(false).show(ui, |ui| {
            for ((_, label), items) in groups {
                theme::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::semibold(format!("{label}  ·  {}", items.len()), 15.0).color(p.text));
                    ui.add_space(4.0);
                    egui::Grid::new(("svc", &label)).num_columns(4).spacing([20.0, 5.0]).striped(true).show(ui, |ui| {
                        for s in items {
                            ui.label(RichText::new(&s.name).color(p.text));
                            let addr = s.addresses.first().map(|a| a.to_string()).unwrap_or_default();
                            ui.label(RichText::new(&addr).monospace().color(p.text)).on_hover_text(
                                s.addresses.iter().map(|a| a.to_string()).collect::<Vec<_>>().join("\n"),
                            );
                            ui.label(RichText::new(format!("{}:{}", s.host, s.port)).monospace().color(p.weak));
                            let details = s
                                .details
                                .iter()
                                .filter(|d| !d.is_empty())
                                .take(4)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("  ");
                            let r =
                                ui.add(egui::Label::new(RichText::new(details).size(12.0).color(p.weak)).truncate());
                            if !s.details.is_empty() {
                                r.on_hover_text(s.details.join("\n"));
                            }
                            ui.end_row();
                        }
                    });
                });
                ui.add_space(8.0);
            }
        });
    }
}

// ---------------------------------------------------------------- free addresses

#[derive(Default)]
pub struct FreeAddresses {
    check: String,
    job: Option<Job<AddressCheck>>,
    result: Option<AddressCheck>,
}

impl FreeAddresses {
    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared, devices: &[Device], range: Option<&Range>) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(v) => self.result = Some(v),
                Err(e) => sh.fail(trl("The address could not be checked."), &e),
            }
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("Is this address free?"), 15.0).color(p.text));
            theme::paragraph(
                ui,
                trl(
                    "Before giving a device a fixed address: checks that nothing answers on it, and whether two devices already share it (an IP conflict).",
                ),
                13.5,
                p.weak,
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r =
                    ui.add(egui::TextEdit::singleline(&mut self.check).hint_text("192.168.1.50").desired_width(180.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ip: Option<Ipv4Addr> = self.check.trim().parse().ok();
                let busy = self.job.is_some();
                if (theme::primary_button(ui, p, &icon_label(icon::MAGNIFYING_GLASS, "Check"), ip.is_some() && !busy)
                    .clicked()
                    || enter)
                    && let Some(ip) = ip
                    && !busy
                {
                    self.result = None;
                    self.job = Some(Job::spawn(ui.ctx(), move |_, _| scan::check_address(ip)));
                }
                if busy {
                    ui.spinner();
                }
            });
            if let Some(r) = &self.result {
                ui.add_space(8.0);
                if r.conflict() {
                    let macs: Vec<String> = r.macs.iter().map(|m| m.to_string()).collect();
                    theme::notice(
                        ui,
                        p,
                        p.danger,
                        icon::WARNING_CIRCLE,
                        &trlf(
                            "IP conflict: {count} devices answer for {ip} ({macs}). Give one of them another address.",
                            &[("count", &r.macs.len()), ("ip", &r.ip), ("macs", &macs.join(", "))],
                        ),
                    );
                } else if r.free() {
                    theme::notice(
                        ui,
                        p,
                        p.success,
                        icon::CHECK_CIRCLE,
                        &trlf(
                            "{ip} looks free: nothing answered. (A device that is switched off or ignores everything cannot be seen.)",
                            &[("ip", &r.ip)],
                        ),
                    );
                } else {
                    let who = [r.name.clone(), r.vendor.clone(), r.macs.first().map(|m| m.to_string())]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let mut text = trlf("{ip} is in use", &[("ip", &r.ip)]);
                    if !who.is_empty() {
                        text = format!("{text}: {who}");
                    }
                    if !r.open_ports.is_empty() {
                        let ports: Vec<String> = r.open_ports.iter().map(u16::to_string).collect();
                        text = format!("{text} — {}", trf("open ports {ports}", &[("ports", &ports.join(", "))]));
                    }
                    theme::notice(ui, p, p.warning, icon::WARNING, &text);
                }
            }
        });
        ui.add_space(10.0);
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::semibold(tr("Free addresses"), 15.0).color(p.text));
            let Some(r) = range else {
                ui.label(RichText::new(tr("Scan the network first (Devices → Scan).")).color(p.weak));
                return;
            };
            let used: Vec<Ipv4Addr> = devices.iter().map(|d| d.ip).collect();
            let free = scan::free_ranges(r.network, r.prefix, &used);
            let count: u64 = free.iter().map(|(a, b)| (u32::from(*b) - u32::from(*a) + 1) as u64).sum();
            theme::paragraph(
                ui,
                &trlf(
                    "{count} addresses in {network} did not answer the last scan. Addresses the router hands out automatically (its DHCP range) may still be given to devices that are off: choose fixed addresses outside that range.",
                    &[("count", &count), ("network", &format!("{}/{}", r.network, r.prefix))],
                ),
                13.5,
                p.weak,
            );
            ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt("free").max_height(320.0).show(ui, |ui| {
                egui::Grid::new("free-grid").num_columns(3).spacing([20.0, 4.0]).striped(true).show(ui, |ui| {
                    for (a, b) in &free {
                        let n = u32::from(*b) - u32::from(*a) + 1;
                        let text = if a == b { a.to_string() } else { format!("{a} – {b}") };
                        ui.label(RichText::new(&text).monospace().color(p.text));
                        ui.label(RichText::new(trf("{n} free", &[("n", &n)])).color(p.weak));
                        if ui.small_button(tr("Check")).on_hover_text(tr("Is this address free?")).clicked() {
                            self.check = a.to_string();
                        }
                        ui.end_row();
                    }
                });
            });
        });
    }
}

// ---------------------------------------------------------------- map

#[derive(Default)]
pub struct NetMap {
    community: String,
    job: Option<Job<Vec<Switch>>>,
    switches: Vec<Switch>,
    seeds: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Internet,
    Router,
    Switch,
    Device,
}

struct Node {
    label: String,
    detail: String,
    kind: Kind,
    glyph: &'static str,
    /// Index of the node it hangs from, and the port label.
    parent: Option<(usize, String)>,
}

impl NetMap {
    /// Development aid: sample switches for README screenshots.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        use netmgr::snmp::{Neighbor, System};
        let sys = |name: &str, d: &str| System { name: name.into(), description: d.into(), ..Default::default() };
        let nb = |local: &str, name: &str, port: &str, ip: &str| Neighbor {
            local_port: local.into(),
            name: name.into(),
            port: port.into(),
            address: ip.parse().ok(),
            protocol: "LLDP",
            ..Default::default()
        };
        let m = |mac: &str, port: &str| (mac.to_string(), port.to_string());
        self.switches = vec![
            Switch {
                address: "192.168.1.2".parse().expect("valid"),
                system: sys("core-sw1", "Cisco IOS-XE C9300"),
                neighbors: vec![
                    nb("Gi1/0/48", "access-sw-2F", "Gi0/1", "192.168.1.3"),
                    nb("Gi1/0/47", "office-ap", "eth0", "192.168.1.4"),
                ],
                macs: vec![m("B8:27:EB:44:55:66", "Gi1/0/5"), m("00:11:32:AB:CD:EF", "Gi1/0/12")],
            },
            Switch {
                address: "192.168.1.3".parse().expect("valid"),
                system: sys("access-sw-2F", "Aruba 2930F"),
                neighbors: vec![nb("Gi0/1", "core-sw1", "Gi1/0/48", "192.168.1.2")],
                macs: vec![m("3C:2A:F4:12:34:56", "1/1/7"), m("F0:18:98:11:22:33", "1/1/14")],
            },
            Switch {
                address: "192.168.1.4".parse().expect("valid"),
                system: sys("office-ap", "UniFi U6 Pro"),
                neighbors: vec![nb("eth0", "core-sw1", "Gi1/0/47", "192.168.1.2")],
                macs: vec![m("8E:1F:3A:77:88:99", "wlan0"), m("F0:EF:86:01:02:03", "wlan0")],
            },
        ];
    }

    fn nodes(&self, sh: &Shared, devices: &[Device]) -> Vec<Node> {
        let mut nodes = vec![Node {
            label: tr("Internet").into(),
            detail: String::new(),
            kind: Kind::Internet,
            glyph: icon::GLOBE,
            parent: None,
        }];
        let gw = sh.default_adapter().and_then(|a| a.gateway);
        let router_dev = devices.iter().find(|d| d.is_gateway || Some(IpAddr::V4(d.ip)) == gw);
        nodes.push(Node {
            label: router_dev.and_then(|d| d.name.clone()).unwrap_or_else(|| tr("Router").into()),
            detail: gw.map(|g| g.to_string()).unwrap_or_default(),
            kind: Kind::Router,
            glyph: icon::BROADCAST,
            parent: Some((0, String::new())),
        });
        // Switches: each hangs from the switch or router that lists it as a
        // neighbor, else from the router.
        let mut index_of: HashMap<IpAddr, usize> = HashMap::new();
        let start = nodes.len();
        for s in &self.switches {
            index_of.insert(s.address, nodes.len());
            let name = if s.system.name.is_empty() { s.address.to_string() } else { s.system.name.clone() };
            nodes.push(Node {
                label: name,
                detail: format!("{}\n{}", s.address, s.system.description.lines().next().unwrap_or("")),
                kind: Kind::Switch,
                glyph: icon::TREE_STRUCTURE,
                parent: Some((1, String::new())),
            });
        }
        let names: HashMap<String, usize> =
            self.switches.iter().enumerate().map(|(i, s)| (s.system.name.to_lowercase(), start + i)).collect();
        for (i, s) in self.switches.iter().enumerate() {
            for n in &s.neighbors {
                let target = n
                    .address
                    .and_then(|a| index_of.get(&a).copied())
                    .or_else(|| names.get(&n.name.to_lowercase()).copied());
                if let Some(t) = target
                    && t != start + i
                    && t > start + i
                    && nodes[t].parent.as_ref().is_some_and(|(p, _)| *p == 1)
                {
                    nodes[t].parent = Some((start + i, n.local_port.clone()));
                }
            }
        }
        // Devices: on the switch port where their MAC was learned, when that
        // port has no switch behind it; else under the router.
        let mut port_of: HashMap<String, (usize, String)> = HashMap::new();
        for (i, s) in self.switches.iter().enumerate() {
            let uplinks: Vec<&str> = s.neighbors.iter().map(|n| n.local_port.as_str()).collect();
            for (mac, port) in &s.macs {
                if !uplinks.contains(&port.as_str()) {
                    port_of.entry(mac.clone()).or_insert((start + i, port.clone()));
                }
            }
        }
        for d in devices.iter().filter(|d| !(d.is_gateway || Some(IpAddr::V4(d.ip)) == gw)) {
            if self.switches.iter().any(|s| s.address == IpAddr::V4(d.ip)) {
                continue;
            }
            let parent = d.mac.and_then(|m| port_of.get(&m.to_string()).cloned()).unwrap_or((1, String::new()));
            let name = sh
                .settings
                .device_labels
                .get(&d.mac.map(|m| m.to_string()).unwrap_or_default())
                .cloned()
                .or_else(|| d.name.clone())
                .or_else(|| d.vendor.clone())
                .unwrap_or_else(|| d.ip.to_string());
            nodes.push(Node {
                label: if d.is_self { format!("{name} ({})", tr("this computer")) } else { name },
                detail: [Some(d.ip.to_string()), d.vendor.clone(), d.mac.map(|m| m.to_string())]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n"),
                kind: Kind::Device,
                glyph: crate::devices::kind_icon(d.kind()),
                parent: Some(parent),
            });
        }
        nodes
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared, devices: &[Device]) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(v) => {
                    if v.is_empty() {
                        sh.toast(tr("No switch answered over SNMP with this community."));
                    }
                    self.switches = v;
                }
                Err(e) => sh.fail(trl("The switches could not be read."), &e),
            }
        }
        if self.community.is_empty() {
            self.community = "public".into();
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "The devices from the last scan under your router. To see switches and which port each device is on, read them over SNMP: the app follows their LLDP and CDP neighbors from switch to switch.",
                ),
                13.5,
                p.weak,
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr("Start at")).color(p.weak));
                ui.add(
                    egui::TextEdit::singleline(&mut self.seeds)
                        .hint_text(tr("switch addresses, e.g. 10.0.0.2 10.0.0.3"))
                        .desired_width(240.0),
                );
                ui.label(RichText::new(tr("Community")).color(p.weak));
                ui.add(egui::TextEdit::singleline(&mut self.community).password(true).desired_width(110.0));
                let busy = self.job.is_some();
                if theme::primary_button(ui, p, &icon_label(icon::TREE_STRUCTURE, "Read switches"), !busy).clicked() {
                    let mut seeds: Vec<IpAddr> =
                        self.seeds.split([' ', ',']).filter_map(|s| s.trim().parse().ok()).collect();
                    if let Some(g) = sh.default_adapter().and_then(|a| a.gateway) {
                        seeds.push(g);
                    }
                    // Devices that look like network gear.
                    for d in devices {
                        let v = d.vendor.as_deref().unwrap_or("").to_lowercase();
                        if [
                            "cisco", "juniper", "aruba", "hewlett", "mikrotik", "ubiquiti", "netgear", "tp-link",
                            "zyxel", "d-link", "arista", "extreme", "fortinet",
                        ]
                        .iter()
                        .any(|n| v.contains(n))
                        {
                            seeds.push(IpAddr::V4(d.ip));
                        }
                    }
                    let community = self.community.clone();
                    self.job = Some(Job::spawn(ui.ctx(), move |progress, cancel: &AtomicBool| {
                        Ok(snmp::crawl(&seeds, &community, Version::V2c, 64, cancel, |s| {
                            progress.line(&s.system.name);
                        }))
                    }));
                }
                if let Some(j) = &self.job {
                    ui.spinner();
                    let found = j.progress.snapshot().lines.len();
                    ui.label(RichText::new(trf("{n} found", &[("n", &found)])).color(p.weak));
                }
            });
        });
        ui.add_space(8.0);
        let nodes = self.nodes(sh, devices);
        ui.horizontal(|ui| {
            if ui
                .button(icon_label(icon::COPY, "Copy as Mermaid"))
                .on_hover_text(tr("A diagram for documentation (GitHub, Confluence, Notion and others draw it)"))
                .clicked()
            {
                ui.ctx().copy_text(mermaid(&nodes));
                sh.toast(tr("Copied."));
            }
        });
        ui.add_space(4.0);
        egui::ScrollArea::both().id_salt("map").auto_shrink(false).show(ui, |ui| draw(ui, p, &nodes));
    }
}

/// Text for Mermaid (`graph TD`), the diagram format many wikis draw.
fn mermaid(nodes: &[Node]) -> String {
    let mut out = String::from("graph TD\n");
    let q = |s: &str| s.replace('"', "'");
    for (i, n) in nodes.iter().enumerate() {
        let first = n.detail.lines().next().unwrap_or("");
        let label = if first.is_empty() { q(&n.label) } else { format!("{}<br/>{}", q(&n.label), q(first)) };
        out += &format!("  n{i}[\"{label}\"]\n");
    }
    for (i, n) in nodes.iter().enumerate() {
        if let Some((parent, port)) = &n.parent {
            if port.is_empty() {
                out += &format!("  n{parent} --- n{i}\n");
            } else {
                out += &format!("  n{parent} ---|\"{}\"| n{i}\n", q(port));
            }
        }
    }
    out
}

/// A tree, top to bottom: Internet, router, switches, devices. Devices of
/// one parent are laid out in rows of up to 8.
fn draw(ui: &mut Ui, p: &Palette, nodes: &[Node]) {
    const W: f32 = 170.0;
    const H: f32 = 46.0;
    const GAP_X: f32 = 14.0;
    const GAP_Y: f32 = 46.0;
    const PER_ROW: usize = 8;
    let children: Vec<Vec<usize>> = (0..nodes.len())
        .map(|i| (0..nodes.len()).filter(|&c| nodes[c].parent.as_ref().is_some_and(|(p, _)| *p == i)).collect())
        .collect();
    // Width each subtree needs.
    fn width(i: usize, nodes: &[Node], children: &[Vec<usize>]) -> f32 {
        let (infra, leaves): (Vec<usize>, Vec<usize>) =
            children[i].iter().partition(|&&c| nodes[c].kind != Kind::Device);
        let leaf_w = if leaves.is_empty() { 0.0 } else { leaves.len().min(PER_ROW) as f32 * (W + GAP_X) };
        let infra_w: f32 = infra.iter().map(|&c| width(c, nodes, children)).sum();
        (leaf_w + infra_w).max(W + GAP_X)
    }
    fn height(i: usize, nodes: &[Node], children: &[Vec<usize>]) -> f32 {
        let (infra, leaves): (Vec<usize>, Vec<usize>) =
            children[i].iter().partition(|&&c| nodes[c].kind != Kind::Device);
        let leaf_h = leaves.len().div_ceil(PER_ROW) as f32 * (H + 18.0);
        let infra_h = infra.iter().map(|&c| height(c, nodes, children)).fold(0.0, f32::max);
        H + GAP_Y + leaf_h.max(infra_h)
    }
    let total = Vec2::new(width(0, nodes, &children) + 40.0, height(0, nodes, &children) + 40.0);
    let (rect, _) = ui.allocate_exact_size(total, Sense::hover());
    let painter = ui.painter_at(rect);
    let mut pos: Vec<egui::Pos2> = vec![rect.min; nodes.len()];
    // Places node `i` with its subtree in [left, left + width(i)] at `top`.
    fn place(i: usize, left: f32, top: f32, nodes: &[Node], children: &[Vec<usize>], pos: &mut Vec<egui::Pos2>) {
        let w = width(i, nodes, children);
        pos[i] = pos2(left + w / 2.0 - W / 2.0, top);
        let (infra, leaves): (Vec<usize>, Vec<usize>) =
            children[i].iter().partition(|&&c| nodes[c].kind != Kind::Device);
        let mut x = left;
        let below = top + H + GAP_Y;
        for &c in &infra {
            place(c, x, below, nodes, children, pos);
            x += width(c, nodes, children);
        }
        for (k, &c) in leaves.iter().enumerate() {
            let (row, col) = (k / PER_ROW, k % PER_ROW);
            pos[c] = pos2(x + col as f32 * (W + GAP_X), below + row as f32 * (H + 18.0));
        }
    }
    place(0, rect.left() + 20.0, rect.top() + 20.0, nodes, &children, &mut pos);
    // Lines first, then boxes on top.
    for (i, n) in nodes.iter().enumerate() {
        if let Some((parent, port)) = &n.parent {
            let a = pos[*parent] + Vec2::new(W / 2.0, H);
            let b = pos[i] + Vec2::new(W / 2.0, 0.0);
            let mid = (a.y + b.y) / 2.0;
            let stroke = Stroke::new(1.2, p.border.gamma_multiply(1.6));
            painter.line_segment([a, pos2(a.x, mid)], stroke);
            painter.line_segment([pos2(a.x, mid), pos2(b.x, mid)], stroke);
            painter.line_segment([pos2(b.x, mid), b], stroke);
            if !port.is_empty() {
                painter.text(
                    b - Vec2::new(-4.0, 4.0),
                    egui::Align2::LEFT_BOTTOM,
                    port,
                    egui::FontId::monospace(10.5),
                    p.accent,
                );
            }
        }
    }
    for (i, n) in nodes.iter().enumerate() {
        let r = egui::Rect::from_min_size(pos[i], Vec2::new(W, H));
        let (fill, edge) = match n.kind {
            Kind::Internet | Kind::Router => (p.tint(p.accent), p.accent),
            Kind::Switch => (p.tint(p.success), p.success),
            Kind::Device => (p.card, p.border),
        };
        painter.rect(r, 8.0, fill, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        painter.text(
            r.left_center() + Vec2::new(10.0, 0.0),
            egui::Align2::LEFT_CENTER,
            n.glyph,
            egui::FontId::proportional(18.0),
            edge,
        );
        let label = clip(&n.label, 18);
        let first = n.detail.lines().next().unwrap_or("");
        painter.text(
            r.left_top() + Vec2::new(36.0, 8.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::proportional(13.0),
            p.text,
        );
        painter.text(
            r.left_top() + Vec2::new(36.0, 26.0),
            egui::Align2::LEFT_TOP,
            clip(first, 22),
            egui::FontId::monospace(11.0),
            p.weak,
        );
        let resp = ui.interact(r, egui::Id::new(("map-node", i)), Sense::hover());
        if !n.detail.is_empty() {
            resp.on_hover_text(format!("{}\n{}", n.label, n.detail));
        }
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n - 1).collect::<String>()) } else { s.to_string() }
}
