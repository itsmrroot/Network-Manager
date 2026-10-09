//! Monitor: many hosts pinged continuously (with up/down alerts), and a
//! path monitor that watches every router on the way to a host, like MTR.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use netmgr::monitor::{State, Stats};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf, trn};
use crate::theme::{self, Palette, icon_label};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Hosts,
    Path,
    Programs,
    Capture,
}

/// A watched host.
#[derive(Clone)]
struct Host {
    label: String,
    ip: IpAddr,
    stats: Stats,
}

/// A router on the path.
#[derive(Clone, Default)]
struct Hop {
    n: u32,
    ip: Option<IpAddr>,
    name: Option<String>,
    stats: Stats,
}

#[derive(Default)]
struct PathState {
    target: Option<IpAddr>,
    hops: Vec<Hop>,
    /// Still finding the routers.
    discovering: bool,
    rounds: u64,
    error: Option<String>,
}

pub struct Monitor {
    tab: Tab,
    programs: crate::programs::Programs,
    capture: crate::capture_page::Capture,
    input: String,
    /// Seconds the history bars show.
    window: u64,
    hosts: Arc<Mutex<Vec<Host>>>,
    running: Option<Arc<AtomicBool>>,
    interval: Arc<AtomicU64>,
    /// State changes: time, host, now up.
    events: Arc<Mutex<Vec<(String, String, bool)>>>,
    seen_events: usize,
    path_input: String,
    path: Arc<Mutex<PathState>>,
    path_stop: Option<Arc<AtomicBool>>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            tab: Tab::Hosts,
            window: 300,
            programs: Default::default(),
            capture: Default::default(),
            input: String::new(),
            hosts: Default::default(),
            running: None,
            interval: Arc::new(AtomicU64::new(1)),
            events: Default::default(),
            seen_events: 0,
            path_input: String::new(),
            path: Default::default(),
            path_stop: None,
        }
    }
}

fn clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn state_color(p: &Palette, s: State) -> Color32 {
    match s {
        State::Up => p.success,
        State::Unstable => p.warning,
        State::Down => p.danger,
        State::Unknown => p.weak,
    }
}

fn state_label(s: State) -> &'static str {
    match s {
        State::Up => tr("Up"),
        State::Unstable => tr("Unstable"),
        State::Down => tr("Down"),
        State::Unknown => tr("Waiting"),
    }
}

fn ms(v: Option<f64>) -> String {
    v.map_or("—".into(), |v| if v < 10.0 { format!("{v:.1} ms") } else { format!("{v:.0} ms") })
}

/// Latency bars with losses in red, newest on the right: the last `count`
/// results (0: one per bar), several to a bar when they do not fit.
fn history(ui: &mut Ui, p: &Palette, h: &std::collections::VecDeque<Option<f64>>, size: Vec2, count: usize) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(4), p.card_alt);
    let n = ((size.x / 3.0) as usize).max(1);
    let per = if count == 0 { 1 } else { count.div_ceil(n).max(1) };
    let window: Vec<Option<f64>> = h.iter().rev().take(n * per).rev().copied().collect();
    // A bar is red when any result in it was lost, else its slowest reply.
    let recent: Vec<Option<f64>> = window
        .rchunks(per)
        .rev()
        .map(|c| if c.iter().any(Option::is_none) { None } else { c.iter().flatten().copied().reduce(f64::max) })
        .collect();
    let top = recent.iter().flatten().copied().fold(1.0, f64::max);
    let w = rect.width() / n as f32;
    let start = rect.right() - recent.len() as f32 * w;
    for (i, v) in recent.iter().enumerate() {
        let x = start + i as f32 * w;
        let (height, color) = match v {
            Some(v) => ((rect.height() - 2.0) * (*v / top) as f32, p.accent),
            None => (rect.height(), p.danger),
        };
        let r = egui::Rect::from_min_max(
            egui::pos2(x, rect.bottom() - height.max(1.5)),
            egui::pos2(x + w - 0.5, rect.bottom()),
        );
        ui.painter().rect_filled(r, 0.0, color);
    }
}

impl Monitor {
    /// Development aid: switches to the path analysis for screenshots.
    #[cfg(debug_assertions)]
    pub fn show_path(&mut self) {
        self.tab = Tab::Path;
    }

    /// Development aid: sample hosts for README screenshots.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self) {
        let fill = |base: f64, spread: f64, loss: u32, n: usize| {
            let mut s = Stats::default();
            for i in 0..n {
                let lost = loss > 0 && (i as u32 * 7919) % 100 < loss;
                let v = base + spread * (((i * 37) % 17) as f64 / 17.0);
                s.add((!lost).then(|| Duration::from_secs_f64(v / 1000.0)));
            }
            s
        };
        let h = |label: &str, ip: &str, s: Stats| Host {
            label: label.into(),
            ip: ip.parse().unwrap_or(IpAddr::from([127, 0, 0, 1])),
            stats: s,
        };
        let mut down = fill(2.0, 1.0, 0, 150);
        for _ in 0..5 {
            down.add(None);
        }
        if let Ok(mut v) = self.hosts.lock() {
            *v = vec![
                h("Router", "192.168.1.1", fill(1.2, 1.5, 0, 180)),
                h("core-sw-01.lab", "10.0.0.2", fill(2.5, 2.0, 0, 180)),
                h("nas", "192.168.1.50", down),
                h("1.1.1.1", "1.1.1.1", fill(14.0, 6.0, 0, 180)),
                h("google.com", "142.250.185.78", fill(18.0, 9.0, 3, 180)),
                h("vpn.example.com", "203.0.113.10", fill(42.0, 30.0, 8, 180)),
            ];
        }
        if let Ok(mut e) = self.events.lock() {
            e.push(("14:02:11".into(), "nas".into(), false));
        }
        self.seen_events = 1;
        let hop = |n: u32, ip: &str, name: Option<&str>, s: Stats| Hop {
            n,
            ip: ip.parse().ok(),
            name: name.map(str::to_string),
            stats: s,
        };
        if let Ok(mut p) = self.path.lock() {
            p.hops = vec![
                hop(1, "192.168.1.1", Some("router.lab"), fill(1.1, 1.0, 0, 120)),
                hop(2, "100.64.0.1", None, fill(8.0, 3.0, 0, 120)),
                hop(3, "", None, Stats::default()),
                hop(4, "198.51.100.17", Some("ae1.core1.ams.example.net"), fill(11.0, 4.0, 40, 120)),
                hop(5, "198.51.100.41", Some("be2.edge2.fra.example.net"), fill(19.0, 5.0, 0, 120)),
                hop(6, "1.1.1.1", Some("one.one.one.one"), fill(20.0, 4.0, 0, 120)),
            ];
            p.rounds = 120;
        }
        self.path_input = "1.1.1.1".into();
    }

    #[cfg(debug_assertions)]
    pub fn show_programs(&mut self) {
        self.tab = Tab::Programs;
    }

    #[cfg(debug_assertions)]
    pub fn show_capture(&mut self, sh: &mut Shared, pcap: Option<&str>) {
        self.tab = Tab::Capture;
        if let Some(path) = pcap {
            self.capture.demo(std::path::Path::new(path), sh);
        }
    }

    pub fn running(&self) -> bool {
        self.running.is_some() || self.path_stop.is_some() || self.capture.running()
    }

    /// Adds a host (name or address) to the ping monitor.
    pub fn add(&mut self, sh: &mut Shared, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match netmgr::tools::resolve(text, 0) {
            Ok(addr) => {
                if let Ok(mut h) = self.hosts.lock() {
                    if h.iter().any(|x| x.ip == addr.ip()) {
                        return;
                    }
                    h.push(Host { label: text.to_string(), ip: addr.ip(), stats: Stats::default() });
                }
                sh.settings.remember_host(text);
            }
            Err(e) => sh.fail(trl("The host could not be added."), &e),
        }
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        self.running = Some(stop.clone());
        let (hosts, interval, events) = (self.hosts.clone(), self.interval.clone(), self.events.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let started = std::time::Instant::now();
                let targets: Vec<IpAddr> = hosts.lock().map(|h| h.iter().map(|x| x.ip).collect()).unwrap_or_default();
                let results: Vec<(IpAddr, Option<Duration>)> = std::thread::scope(|s| {
                    let hs: Vec<_> = targets
                        .iter()
                        .map(|ip| {
                            s.spawn(move || (*ip, netmgr::icmp::ping(*ip, Duration::from_millis(1500)).ok().flatten()))
                        })
                        .collect();
                    hs.into_iter().filter_map(|h| h.join().ok()).collect()
                });
                if let Ok(mut h) = hosts.lock() {
                    for (ip, r) in results {
                        if let Some(x) = h.iter_mut().find(|x| x.ip == ip) {
                            let before = x.stats.state();
                            x.stats.add(r);
                            let after = x.stats.state();
                            if before != after
                                && (after == State::Down || (before == State::Down && after != State::Down))
                                && let Ok(mut e) = events.lock()
                            {
                                e.push((clock(), x.label.clone(), after != State::Down));
                            }
                        }
                    }
                }
                let every = Duration::from_secs(interval.load(Ordering::Relaxed).max(1));
                std::thread::sleep(every.saturating_sub(started.elapsed()));
            }
        });
    }

    fn stop(&mut self) {
        if let Some(s) = self.running.take() {
            s.store(true, Ordering::Relaxed);
        }
    }

    /// Alerts when a host goes down or comes back, on any page.
    pub fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        let new_events: Vec<(String, String, bool)> =
            self.events.lock().map(|e| e.iter().skip(self.seen_events).cloned().collect()).unwrap_or_default();
        self.seen_events += new_events.len();
        for (_, host, up) in &new_events {
            let text = if *up {
                trf("{host} is reachable again.", &[("host", host)])
            } else {
                trf("{host} is down.", &[("host", host)])
            };
            if sh.settings.notifications {
                crate::notify::send(tr("Network Manager"), &text);
            }
            sh.toast(text);
        }
        if self.running.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if self.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        theme::page_title(ui, p, tr("Monitor"), tr("Watch hosts and the routers on the way to them, live."));
        theme::tabs(
            ui,
            p,
            &mut self.tab,
            &[
                (Tab::Hosts, icon::HEARTBEAT, tr("Ping monitor")),
                (Tab::Path, icon::PATH, tr("Path analysis (MTR)")),
                (Tab::Programs, icon::CHART_BAR, tr("Traffic per program")),
                (Tab::Capture, icon::FILE_MAGNIFYING_GLASS, tr("Packet capture")),
            ],
        );
        ui.add_space(10.0);
        match self.tab {
            Tab::Hosts => self.hosts_tab(ui, p, sh),
            Tab::Path => self.path_tab(ui, p, sh),
            Tab::Programs => self.programs.ui(ui, p, sh),
            Tab::Capture => self.capture.ui(ui, p, sh),
        }
    }

    fn hosts_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut add = None;
        let mut add_all: Vec<String> = Vec::new();
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text(tr("Address or name to watch"))
                    .desired_width(260.0),
            );
            if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button(icon_label(icon::PLUS, "Add")).clicked()
            {
                add = Some(std::mem::take(&mut self.input));
            }
            egui::ComboBox::from_id_salt("quick-hosts").selected_text(tr("Quick add")).width(120.0).show_ui(ui, |ui| {
                let mine = sh.settings.saved_hosts.clone();
                if mine.len() > 1
                    && ui.selectable_label(false, format!("{}  {}", icon::STAR, tr("All my hosts"))).clicked()
                {
                    add_all = mine.iter().map(|h| h.address.clone()).collect();
                }
                for h in &mine {
                    if ui.selectable_label(false, format!("{}  {} ({})", icon::STAR, h.name, h.address)).clicked() {
                        add = Some(h.address.clone());
                    }
                }
                if let Some(g) = sh.default_adapter().and_then(|a| a.gateway)
                    && ui.selectable_label(false, trf("Router ({address})", &[("address", &g)])).clicked()
                {
                    add = Some(g.to_string());
                }
                for (n, h) in [
                    (tr("Cloudflare DNS"), "1.1.1.1"),
                    (tr("Google DNS"), "8.8.8.8"),
                    (tr("Quad9"), "9.9.9.9"),
                    ("google.com", "google.com"),
                ] {
                    if ui.selectable_label(false, format!("{n} ({h})")).clicked() {
                        add = Some(h.to_string());
                    }
                }
                for h in sh.settings.recent_hosts.clone() {
                    if ui.selectable_label(false, &h).clicked() {
                        add = Some(h);
                    }
                }
            });
            ui.add_space(8.0);
            let mut secs = self.interval.load(Ordering::Relaxed);
            egui::ComboBox::from_id_salt("interval")
                .selected_text(trf("Every {n} s", &[("n", &secs)]))
                .width(100.0)
                .show_ui(ui, |ui| {
                    for s in [1, 2, 5, 10, 30] {
                        ui.selectable_value(&mut secs, s, trf("Every {n} s", &[("n", &s)]));
                    }
                });
            self.interval.store(secs, Ordering::Relaxed);
            let label = |w: u64| {
                if w >= 3600 {
                    trf("Last {n} h", &[("n", &(w / 3600))])
                } else {
                    trf("Last {n} min", &[("n", &(w / 60))])
                }
            };
            egui::ComboBox::from_id_salt("window").selected_text(label(self.window)).width(110.0).show_ui(ui, |ui| {
                for w in [300, 1800, 7200] {
                    ui.selectable_value(&mut self.window, w, label(w));
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let has = self.hosts.lock().map(|h| !h.is_empty()).unwrap_or(false);
                if self.running.is_some() {
                    if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked() {
                        self.stop();
                    }
                } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start"), has).clicked() {
                    self.start();
                }
                if has && ui.button(icon_label(icon::EXPORT, "Export…")).clicked() {
                    self.export(sh);
                }
                if has
                    && ui.button(icon_label(icon::ARROW_COUNTER_CLOCKWISE, tr("Reset"))).clicked()
                    && let Ok(mut h) = self.hosts.lock()
                {
                    for x in h.iter_mut() {
                        x.stats = Stats::default();
                    }
                }
            });
        });
        for text in add_all {
            self.add(sh, &text);
        }
        if let Some(text) = add {
            self.add(sh, &text);
            if self.running.is_none() {
                self.start();
            }
        }
        ui.add_space(10.0);
        let hosts: Vec<Host> = self.hosts.lock().map(|h| h.clone()).unwrap_or_default();
        if hosts.is_empty() {
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    ui.add_space(14.0);
                    theme::icon_badge(ui, p, icon::HEARTBEAT, p.accent, 64.0);
                    ui.add_space(6.0);
                    ui.label(theme::semibold(tr("Watch your important hosts"), 18.0).color(p.text));
                    theme::paragraph(
                        ui,
                        trl("Add servers, switches, printers or the internet: each one is pinged continuously, with its delay, loss and jitter. You are told when one goes down and when it comes back."),
                        14.0,
                        p.weak,
                    );
                    ui.add_space(14.0);
                });
            });
            return;
        }
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let cols = if ui.available_width() > 1000.0 { 3 } else { 2 };
            for row in hosts.chunks(cols) {
                ui.columns(cols, |c| {
                    for (i, h) in row.iter().enumerate() {
                        let ui = &mut c[i];
                        theme::card(ui, p, |ui| {
                            ui.set_width(ui.available_width());
                            let st = h.stats.state();
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(icon::CIRCLE).size(12.0).color(state_color(p, st)));
                                ui.add(egui::Label::new(theme::semibold(&h.label, 15.5).color(p.text)).truncate());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .add(egui::Button::new(RichText::new(icon::X).color(p.weak)).frame(false))
                                        .on_hover_text(tr("Stop watching"))
                                        .clicked()
                                    {
                                        remove = Some(h.ip);
                                    }
                                    theme::pill(ui, p, state_label(st), state_color(p, st));
                                });
                            });
                            if h.label != h.ip.to_string() {
                                ui.label(RichText::new(h.ip.to_string()).color(p.weak).size(12.5));
                            }
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 18.0;
                                theme::stat(ui, p, tr("Last"), &ms(h.stats.last));
                                theme::stat(ui, p, tr("Average"), &ms(h.stats.avg()));
                                theme::stat(ui, p, tr("Loss"), &format!("{:.0} %", h.stats.loss_percent()));
                                theme::stat(ui, p, tr("Jitter"), &ms(h.stats.jitter()));
                            });
                            ui.add_space(6.0);
                            let w = ui.available_width();
                            let every = self.interval.load(Ordering::Relaxed).max(1);
                            history(ui, p, &h.stats.history, Vec2::new(w, 34.0), (self.window / every) as usize);
                        });
                    }
                });
                ui.add_space(8.0);
            }
            let events = self.events.lock().map(|e| e.clone()).unwrap_or_default();
            if !events.is_empty() {
                theme::card(ui, p, |ui| {
                    ui.set_width(ui.available_width());
                    theme::section_title(ui, p, icon::LIST_BULLETS, tr("Events"));
                    for (t, host, up) in events.iter().rev().take(50) {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t).monospace().color(p.weak));
                            let (g, c, what) = if *up {
                                (icon::CHECK_CIRCLE, p.success, trf("{host} is reachable again.", &[("host", host)]))
                            } else {
                                (icon::X_CIRCLE, p.danger, trf("{host} went down.", &[("host", host)]))
                            };
                            ui.label(RichText::new(g).color(c));
                            ui.label(RichText::new(what).color(p.text));
                        });
                    }
                });
            }
        });
        if let Some(ip) = remove {
            let empty = self
                .hosts
                .lock()
                .map(|mut h| {
                    h.retain(|x| x.ip != ip);
                    h.is_empty()
                })
                .unwrap_or(false);
            if empty {
                self.stop();
            }
        }
    }

    fn export(&self, sh: &mut Shared) {
        let Some(path) =
            rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("ping-monitor.csv").save_file()
        else {
            return;
        };
        let mut out = String::from("Host,Address,Sent,Received,Loss %,Min ms,Average ms,Max ms,Jitter ms\n");
        for h in self.hosts.lock().map(|h| h.clone()).unwrap_or_default() {
            let f = |v: Option<f64>| v.map_or(String::new(), |v| format!("{v:.2}"));
            out += &format!(
                "\"{}\",{},{},{},{:.1},{},{},{},{}\n",
                h.label.replace('"', "\"\""),
                h.ip,
                h.stats.sent,
                h.stats.received,
                h.stats.loss_percent(),
                f(h.stats.min),
                f(h.stats.avg()),
                f(h.stats.max),
                f(h.stats.jitter())
            );
        }
        match std::fs::write(&path, out) {
            Ok(()) => sh.toast(trf("Saved to {file}.", &[("file", &path.display())])),
            Err(e) => sh.fail(trl("The file could not be saved."), &e.into()),
        }
    }

    fn start_path(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        let host = self.path_input.trim().to_string();
        if host.is_empty() {
            return;
        }
        sh.settings.remember_host(&host);
        let stop = Arc::new(AtomicBool::new(false));
        self.path_stop = Some(stop.clone());
        let state = Arc::new(Mutex::new(PathState { discovering: true, ..Default::default() }));
        self.path = state.clone();
        let dns: Vec<IpAddr> = sh.default_adapter().map(|a| a.dns.clone()).unwrap_or_default();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // 1. The routers on the way.
            let found = netmgr::monitor::discover_path(&host, &stop, |n, ip| {
                if let Ok(mut s) = state.lock() {
                    s.hops.push(Hop { n, ip, ..Default::default() });
                }
                ctx.request_repaint();
            });
            let target = match found {
                Ok(t) => t,
                Err(e) => {
                    if let Ok(mut s) = state.lock() {
                        s.error = Some(format!("{e:#}"));
                        s.discovering = false;
                    }
                    return;
                }
            };
            if let Ok(mut s) = state.lock() {
                s.target = Some(target);
                s.discovering = false;
                // The trace may have stopped before reaching the host.
                if s.hops.last().is_none_or(|h| h.ip != Some(target)) {
                    let n = s.hops.last().map_or(1, |h| h.n + 1);
                    s.hops.push(Hop { n, ip: Some(target), ..Default::default() });
                }
            }
            // Names of the routers, in the background.
            let names = state.clone();
            std::thread::spawn(move || {
                let ips: Vec<IpAddr> =
                    names.lock().map(|s| s.hops.iter().filter_map(|h| h.ip).collect()).unwrap_or_default();
                for ip in ips {
                    let servers: Vec<IpAddr> =
                        dns.iter().copied().filter(|d| d.is_ipv4()).chain(["1.1.1.1".parse().unwrap_or(ip)]).collect();
                    for s in servers.iter().take(2) {
                        if let Ok(a) = netmgr::dns::query(
                            std::net::SocketAddr::new(*s, 53),
                            &ip.to_string(),
                            netmgr::dns::RecordType::Ptr,
                            Duration::from_secs(2),
                        ) && let Some(r) = a.records.iter().find(|r| r.kind == "PTR")
                        {
                            if let Ok(mut st) = names.lock()
                                && let Some(h) = st.hops.iter_mut().find(|h| h.ip == Some(ip))
                            {
                                h.name = Some(r.data.trim_end_matches('.').to_string());
                            }
                            break;
                        }
                    }
                }
            });
            // 2. Every router, every second.
            while !stop.load(Ordering::Relaxed) {
                let started = std::time::Instant::now();
                let ips: Vec<Option<IpAddr>> =
                    state.lock().map(|s| s.hops.iter().map(|h| h.ip).collect()).unwrap_or_default();
                let results: Vec<Option<Duration>> = std::thread::scope(|s| {
                    let hs: Vec<_> = ips
                        .iter()
                        .map(|ip| {
                            s.spawn(move || {
                                ip.and_then(|ip| netmgr::icmp::ping(ip, Duration::from_millis(1500)).ok().flatten())
                            })
                        })
                        .collect();
                    hs.into_iter().map(|h| h.join().ok().flatten()).collect()
                });
                if let Ok(mut s) = state.lock() {
                    for (h, r) in s.hops.iter_mut().zip(results) {
                        if h.ip.is_some() {
                            h.stats.add(r);
                        }
                    }
                    s.rounds += 1;
                }
                ctx.request_repaint();
                std::thread::sleep(Duration::from_secs(1).saturating_sub(started.elapsed()));
            }
        });
    }

    fn path_tab(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let mut start = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.path_input)
                    .hint_text(tr("Address or name, e.g. google.com"))
                    .desired_width(280.0),
            );
            start = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if let Some(stop) = &self.path_stop {
                if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked() {
                    stop.store(true, Ordering::Relaxed);
                    self.path_stop = None;
                }
            } else if theme::primary_button(ui, p, &icon_label(icon::PLAY, "Start"), !self.path_input.trim().is_empty())
                .clicked()
            {
                start = true;
            }
        });
        if start && self.path_stop.is_none() {
            self.start_path(ui.ctx(), sh);
        }
        ui.add_space(6.0);
        theme::paragraph(
            ui,
            trl(
                "Finds every router between this computer and the host, then pings each of them every second. Loss or delay that starts at one router and continues to the end shows where a problem is.",
            ),
            12.5,
            p.weak,
        );
        ui.add_space(8.0);
        let (hops, discovering, rounds, error) =
            self.path.lock().map(|s| (s.hops.clone(), s.discovering, s.rounds, s.error.clone())).unwrap_or_default();
        if let Some(e) = error {
            theme::notice(ui, p, p.danger, icon::WARNING_CIRCLE, &e);
            self.path_stop = None;
            return;
        }
        if let Some(text) = verdict(&hops)
            && rounds >= 10
        {
            let (color, glyph) = if text.0 { (p.warning, icon::WARNING) } else { (p.success, icon::CHECK_CIRCLE) };
            theme::notice(ui, p, color, glyph, &text.1);
            ui.add_space(8.0);
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            if hops.is_empty() {
                let msg = if discovering {
                    tr("Finding the routers on the way…")
                } else {
                    tr("Enter a host and click Start.")
                };
                ui.horizontal(|ui| {
                    if discovering {
                        ui.spinner();
                    }
                    ui.label(RichText::new(msg).color(p.weak));
                });
                return;
            }
            if discovering {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(tr("Finding the routers on the way…")).color(p.weak));
                });
            } else {
                ui.label(RichText::new(trn(rounds, "1 round", "{n} rounds")).color(p.weak).size(12.5));
            }
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(34.0))
                .column(Column::remainder().at_least(220.0).clip(true))
                .column(Column::exact(70.0))
                .column(Column::exact(60.0))
                .column(Column::exact(80.0))
                .column(Column::exact(80.0))
                .column(Column::exact(80.0))
                .column(Column::exact(80.0))
                .column(Column::exact(150.0))
                .header(26.0, |mut h| {
                    for t in [
                        "#",
                        tr("Router"),
                        tr("Loss"),
                        tr("Sent"),
                        tr("Last"),
                        tr("Average"),
                        tr("Worst"),
                        tr("Jitter"),
                        tr("History"),
                    ] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).color(p.weak).size(13.0));
                        });
                    }
                })
                .body(|body| {
                    body.rows(30.0, hops.len(), |mut row| {
                        let h = &hops[row.index()];
                        let s = &h.stats;
                        row.col(|ui| {
                            ui.label(RichText::new(h.n.to_string()).color(p.weak));
                        });
                        row.col(|ui| match h.ip {
                            Some(ip) => {
                                ui.label(RichText::new(ip.to_string()).monospace().color(p.text));
                                if let Some(n) = &h.name {
                                    ui.label(RichText::new(n).color(p.weak).size(12.5));
                                }
                            }
                            None => {
                                ui.label(RichText::new(tr("No answer (router hides itself)")).color(p.weak).italics());
                            }
                        });
                        let loss = s.loss_percent();
                        row.col(|ui| {
                            let c = if s.sent == 0 {
                                p.weak
                            } else if loss >= 20.0 {
                                p.danger
                            } else if loss > 0.0 {
                                p.warning
                            } else {
                                p.success
                            };
                            ui.label(
                                RichText::new(if s.sent == 0 { "—".into() } else { format!("{loss:.0} %") }).color(c),
                            );
                        });
                        row.col(|ui| {
                            ui.label(s.sent.to_string());
                        });
                        for v in [s.last, s.avg(), s.max, s.jitter()] {
                            row.col(|ui| {
                                ui.label(ms(v));
                            });
                        }
                        row.col(|ui| {
                            history(ui, p, &s.history, Vec2::new(140.0, 20.0), 0);
                        });
                    });
                });
        });
    }
}

/// What the path shows: (a problem?, explanation). Loss that starts at a
/// router and continues to the host is real; loss at a router alone is
/// that router ignoring pings.
fn verdict(hops: &[Hop]) -> Option<(bool, String)> {
    let last = hops.iter().rev().find(|h| h.ip.is_some() && h.stats.sent > 0)?;
    let end_loss = last.stats.loss_percent();
    if end_loss < 3.0 {
        let noisy: Vec<String> = hops
            .iter()
            .filter(|h| h.ip.is_some() && h.stats.sent > 0 && h.stats.loss_percent() >= 10.0 && h.n != last.n)
            .map(|h| h.n.to_string())
            .collect();
        let extra = if noisy.is_empty() {
            String::new()
        } else {
            format!(
                " {}",
                trlf(
                    "Router(s) {routers} answer pings slowly or not at all, but traffic passes them without loss: that is normal.",
                    &[("routers", &noisy.join(", "))]
                )
            )
        };
        return Some((
            false,
            format!(
                "{}{extra}",
                trlf(
                    "The path is healthy: the host answers with {loss} % loss.",
                    &[("loss", &format!("{end_loss:.0}"))]
                )
            ),
        ));
    }
    // The first router from which loss continues to the end.
    let mut start = last;
    for h in hops.iter().rev().filter(|h| h.ip.is_some() && h.stats.sent > 0) {
        if h.stats.loss_percent() >= end_loss * 0.6 {
            start = h;
        } else {
            break;
        }
    }
    let place = match start.n {
        1 => trl("The loss starts at your own router or Wi-Fi: check the cable or the signal, or restart the router.")
            .to_string(),
        2 | 3 => trlf(
            "The loss starts at router {n} — usually your internet provider: contact them with this result.",
            &[("n", &start.n)],
        ),
        n => trlf(
            "The loss starts at router {n}, further away on the internet (a provider or the host's network).",
            &[("n", &n)],
        ),
    };
    Some((
        true,
        format!("{} {place}", trlf("{loss} % of pings to the host are lost.", &[("loss", &format!("{end_loss:.0}"))])),
    ))
}
