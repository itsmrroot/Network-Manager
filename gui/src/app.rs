//! Application state, navigation and the glue between pages and jobs.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icon;
use netmgr::adapters::{self, Adapter};
use netmgr::internet::PublicInfo;
use netmgr::wifi;

use crate::i18n::{self, tr, trl};
use crate::jobs::{self, Job};
use crate::settings::{self, Settings};
use crate::theme::{self, Palette};
use crate::update::Updater;
use crate::{
    about, adapters_page, console, devices, help, monitor, overview, profiles, servers, switchport, tools, wifi_page,
};
use crate::{menu, search};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Adapters,
    Wifi,
    Devices,
    SwitchPort,
    Monitor,
    Profiles,
    Tools,
    Servers,
    Console,
    Settings,
    Help,
    About,
}

/// A request from one page to show another.
pub enum Nav {
    Page(Page),
    /// The Adapters page with this adapter (id) selected.
    Adapter(String),
    /// A tool with an address filled in.
    Tool(tools::Tab, String),
    /// Watch a host in the ping monitor.
    Monitor(String),
}

/// Transfer rates of the default adapter, sampled every second.
#[derive(Default)]
pub struct Traffic {
    pub adapter: String,
    last: Option<(Instant, u64, u64)>,
    /// Bits per second, received and sent.
    pub down: VecDeque<f64>,
    pub up: VecDeque<f64>,
}

impl Traffic {
    const POINTS: usize = 60;

    fn add(&mut self, id: &str, rx: u64, tx: u64) {
        if id != self.adapter {
            *self = Traffic { adapter: id.to_string(), ..Default::default() };
        }
        let now = Instant::now();
        if let Some((t, r0, t0)) = self.last {
            let secs = now.duration_since(t).as_secs_f64().max(0.2);
            self.down.push_back(rx.saturating_sub(r0) as f64 * 8.0 / secs);
            self.up.push_back(tx.saturating_sub(t0) as f64 * 8.0 / secs);
            while self.down.len() > Self::POINTS {
                self.down.pop_front();
                self.up.pop_front();
            }
        }
        self.last = Some((now, rx, tx));
    }
}

/// What every page can read and change.
pub struct Shared {
    pub settings: Settings,
    /// Every adapter, virtual ones included.
    pub adapters: Vec<Adapter>,
    pub adapters_loaded: bool,
    pub wifi: Vec<wifi::Connection>,
    pub wifi_loaded: bool,
    pub public: Option<Result<PublicInfo, String>>,
    pub public_loading: bool,
    /// macOS hides MAC addresses and devices from the app.
    pub lan_denied: bool,
    pub error: Option<String>,
    pub toast: Option<(String, Instant)>,
    /// Reload adapters now (after a change).
    pub refresh: bool,
    pub refresh_public: bool,
    pub refresh_wifi: bool,
    pub nav: Option<Nav>,
    pub traffic: Traffic,
}

impl Shared {
    /// The adapters to show, following the "virtual adapters" setting.
    pub fn visible_adapters(&self) -> Vec<&Adapter> {
        self.adapters
            .iter()
            .filter(|a| self.settings.show_virtual || (a.physical && a.kind != adapters::Kind::Loopback))
            .collect()
    }

    pub fn default_adapter(&self) -> Option<&Adapter> {
        self.adapters.iter().find(|a| a.default)
    }

    /// A short confirmation at the bottom of the window.
    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /// Shows the error of a job, unless the password dialog was cancelled.
    pub fn fail(&mut self, what: &str, e: &anyhow::Error) {
        if netmgr::cmd::is_cancelled(e) {
            self.toast(tr("Cancelled: nothing was changed."));
        } else {
            self.error = Some(format!("{what}\n\n{}", jobs::describe(e)));
        }
    }
}

pub struct App {
    shared: Shared,
    applied: Option<(theme::Accent, f32, settings::ThemeChoice)>,
    language: Option<i18n::Lang>,
    page: Page,
    menu: menu::MenuBar,
    search: search::Search,
    logo: egui::TextureHandle,
    updater: Updater,
    fitted: bool,

    adapters_job: Option<Job<(Vec<Adapter>, bool)>>,
    adapters_at: Option<Instant>,
    wifi_job: Option<Job<Vec<wifi::Connection>>>,
    wifi_at: Option<Instant>,
    public_job: Option<Job<PublicInfo>>,
    traffic_job: Option<Job<Vec<(String, u64, u64)>>>,
    traffic_at: Option<Instant>,

    overview: overview::Overview,
    adapters_page: adapters_page::AdaptersPage,
    wifi_page: wifi_page::WifiPage,
    devices: devices::Devices,
    profiles: profiles::Profiles,
    tools: tools::Tools,
    switchport: switchport::SwitchPort,
    monitor: monitor::Monitor,
    servers: servers::Servers,
    console: console::Console,
    #[cfg(debug_assertions)]
    tour: Option<tour::Tour>,
}

const SETTINGS_KEY: &str = "settings";

/// The window size on first start, in points.
pub const DEFAULT_SIZE: [f32; 2] = [1240.0, 800.0];
/// The smallest window in which every page still works.
pub const MIN_SIZE: [f32; 2] = [860.0, 560.0];

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
        // Saved sessions live in sessions.json, shared with `netmgr backup`;
        // 0.4 kept them in the settings.
        match netmgr::remote::load() {
            Ok(list) if !list.is_empty() => settings.sessions = list,
            Ok(_) if !settings.sessions.is_empty() => {
                let _ = netmgr::remote::save(&settings.sessions);
            }
            _ => {}
        }
        let lang = i18n::set_language(settings.language);
        theme::install_fonts(&cc.egui_ctx, lang);
        // Zoom shortcuts are handled as changes of the interface size setting.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let logo = {
            let img = image::load_from_memory(include_bytes!("../assets/icon.png")).map(|i| i.to_rgba8());
            let color = match img {
                Ok(i) => {
                    egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], i.as_raw())
                }
                Err(_) => egui::ColorImage::filled([1, 1], Color32::TRANSPARENT),
            };
            cc.egui_ctx.load_texture("logo", color, egui::TextureOptions::LINEAR)
        };
        #[cfg(debug_assertions)]
        let tour = tour::Tour::from_env();
        #[cfg(debug_assertions)]
        let touring = tour.is_some();
        #[cfg(not(debug_assertions))]
        let touring = false;
        let mut updater = Updater::default();
        if settings.check_updates && !touring {
            updater.check(&cc.egui_ctx, false);
        }
        let refresh_public = settings.lookup_public_ip;
        Self {
            shared: Shared {
                settings,
                adapters: Vec::new(),
                adapters_loaded: false,
                wifi: Vec::new(),
                wifi_loaded: false,
                public: None,
                public_loading: false,
                lan_denied: false,
                error: None,
                toast: None,
                refresh: true,
                refresh_public,
                refresh_wifi: true,
                nav: None,
                traffic: Traffic::default(),
            },
            applied: None,
            language: Some(lang),
            page: Page::Overview,
            menu: Default::default(),
            search: Default::default(),
            logo,
            updater,
            fitted: false,
            adapters_job: None,
            adapters_at: None,
            wifi_job: None,
            wifi_at: None,
            public_job: None,
            traffic_job: None,
            traffic_at: None,
            overview: Default::default(),
            adapters_page: Default::default(),
            wifi_page: Default::default(),
            devices: Default::default(),
            profiles: Default::default(),
            tools: Default::default(),
            switchport: Default::default(),
            monitor: Default::default(),
            servers: Default::default(),
            console: Default::default(),
            #[cfg(debug_assertions)]
            tour,
        }
    }

    /// One step bigger (1), smaller (−1), or back to 100 % (0), in the same
    /// steps and range as the Settings page.
    fn zoom(&mut self, step: i32) {
        let scale = &mut self.shared.settings.ui_scale;
        *scale = match step {
            0 => 1.0,
            s => ((*scale * 10.0).round() / 10.0 + 0.1 * s as f32).clamp(0.8, 1.5),
        };
    }

    /// Goes where a search result leads.
    fn go(&mut self, ctx: &egui::Context, t: search::Target) {
        use search::Target;
        match t {
            Target::Page(p) => self.page = p,
            Target::Tool(tab) => {
                self.page = Page::Tools;
                self.tools.select(tab);
            }
            Target::Tab(page, id) => {
                self.page = page;
                match page {
                    Page::Wifi => self.wifi_page.open_tab(id),
                    Page::Devices => self.devices.show(id),
                    Page::Monitor => self.monitor.open_tab(id),
                    Page::Servers => self.servers.open_tab(id),
                    Page::Console => self.console.open_tab(id),
                    _ => {}
                }
            }
            Target::Command(c) => self.run(ctx, c),
        }
    }

    /// Does what the menu bar or a shortcut asked for.
    fn run(&mut self, ctx: &egui::Context, c: menu::Command) {
        use menu::Command as C;
        match c {
            C::Page(p) => self.page = p,
            C::Search => self.search.show(),
            C::CheckUpdates => {
                self.page = Page::About;
                self.updater.check(ctx, true);
            }
            C::Undo | C::Redo | C::Cut | C::Copy | C::Paste | C::SelectAll => menu::edit(ctx, c),
            C::ZoomIn => self.zoom(1),
            C::ZoomOut => self.zoom(-1),
            C::ZoomReset => self.zoom(0),
            C::Theme(t) => self.shared.settings.theme = t,
            C::CheckConnection => {
                self.page = Page::Overview;
                self.overview.check_connection(ctx);
            }
            C::RenewIp => {
                if let Some(a) = self.shared.default_adapter().cloned() {
                    self.page = Page::Overview;
                    self.overview.renew(ctx, a);
                }
            }
            C::FlushDns => {
                self.page = Page::Overview;
                self.overview.flush_dns(ctx);
            }
            C::SpeedTest => {
                self.page = Page::Overview;
                self.overview.start_speed(ctx);
            }
            C::Scan => {
                self.page = Page::Devices;
                self.devices.scan(ctx, &self.shared);
            }
            C::Report => {
                self.page = Page::Overview;
                self.overview.start_report(ctx, &self.shared);
            }
            C::Open(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        // Ctrl/Cmd with +, - and 0 change the interface size too, in the same
        // steps and range as the Settings page.
        use egui::gui_zoom::kb_shortcuts as keys;
        let pressed = |k| ctx.input_mut(|i| i.consume_shortcut(&k));
        if pressed(keys::ZOOM_RESET) {
            self.zoom(0);
        } else if pressed(keys::ZOOM_IN) || pressed(keys::ZOOM_IN_SECONDARY) {
            self.zoom(1);
        } else if pressed(keys::ZOOM_OUT) {
            self.zoom(-1);
        }
        let s = &self.shared.settings;
        let want = (s.accent, s.ui_scale, s.theme);
        if self.applied != Some(want) {
            theme::apply_style(ctx, s.accent, s.theme == settings::ThemeChoice::Midnight);
            ctx.set_theme(s.theme.preference());
            ctx.set_zoom_factor(s.ui_scale);
            self.applied = Some(want);
        }
        let lang = i18n::set_language(self.shared.settings.language);
        if self.language != Some(lang) {
            theme::install_fonts(ctx, lang);
            self.language = Some(lang);
        }
    }

    /// Keeps adapters, Wi-Fi, the public address and traffic up to date.
    fn refresh(&mut self, ctx: &egui::Context) {
        let sh = &mut self.shared;
        let due = |at: Option<Instant>, every: u64| at.is_none_or(|t| t.elapsed() > Duration::from_secs(every));

        if let Some(r) = jobs::finished(&mut self.adapters_job) {
            match r {
                Ok((list, denied)) => {
                    sh.adapters = list;
                    sh.lan_denied = denied;
                }
                Err(e) => log::warn!("adapters: {e:#}"),
            }
            sh.adapters_loaded = true;
            self.adapters_at = Some(Instant::now());
        }
        if self.adapters_job.is_none() && (sh.refresh || due(self.adapters_at, 8)) {
            sh.refresh = false;
            self.adapters_job =
                Some(Job::spawn(ctx, |_, _| Ok((adapters::list(true)?, adapters::local_network_denied()))));
        }

        // Wi-Fi details are slow to read (system_profiler on macOS): only
        // while a page shows them.
        if let Some(r) = jobs::finished(&mut self.wifi_job) {
            sh.wifi = r.unwrap_or_default();
            sh.wifi_loaded = true;
            self.wifi_at = Some(Instant::now());
        }
        let wants_wifi = matches!(self.page, Page::Overview | Page::Wifi);
        if self.wifi_job.is_none() && (sh.refresh_wifi || (wants_wifi && due(self.wifi_at, 15))) {
            sh.refresh_wifi = false;
            self.wifi_job = Some(Job::spawn(ctx, |_, _| wifi::current()));
        }

        if let Some(r) = jobs::finished(&mut self.public_job) {
            sh.public = Some(r.map_err(|e| format!("{e:#}")));
            sh.public_loading = false;
        }
        if self.public_job.is_none() && sh.refresh_public {
            sh.refresh_public = false;
            sh.public_loading = true;
            self.public_job = Some(Job::spawn(ctx, |_, _| netmgr::internet::public_info()));
        }

        if let Some(Ok(counters)) = jobs::finished(&mut self.traffic_job)
            && let Some(d) = sh.adapters.iter().find(|a| a.default)
            && let Some((_, rx, tx)) = counters.iter().find(|(id, ..)| *id == d.id)
        {
            let id = d.id.clone();
            sh.traffic.add(&id, *rx, *tx);
        }
        if self.traffic_job.is_none() && self.page == Page::Overview && due(self.traffic_at, 1) {
            self.traffic_at = Some(Instant::now());
            self.traffic_job = Some(Job::spawn(ctx, |_, _| {
                Ok(netdev::get_interfaces()
                    .into_iter()
                    .filter_map(|i| i.stats.map(|s| (i.name.clone(), s.rx_bytes, s.tx_bytes)))
                    .collect())
            }));
        }
        let busy = self.adapters_job.is_some() || self.wifi_job.is_some() || self.public_job.is_some();
        ctx.request_repaint_after(Duration::from_millis(if busy || self.page == Page::Overview { 500 } else { 2000 }));
    }

    /// Shrinks and centres the window when it does not fit the screen.
    fn fit_to_screen(&mut self, ctx: &egui::Context) {
        if self.fitted {
            return;
        }
        let (monitor, inner) = ctx.input(|i| (i.viewport().monitor_size, i.viewport().inner_rect));
        let (Some(monitor), Some(inner)) = (monitor, inner) else { return };
        self.fitted = true;
        let max = egui::vec2(monitor.x * 0.94, monitor.y * 0.88);
        let size = inner.size();
        if size.x <= max.x && size.y <= max.y {
            return;
        }
        let min = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]).min(max);
        let fitted = size.min(max).max(min);
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(fitted));
        let corner = ((monitor - fitted) / 2.0).max(egui::Vec2::ZERO);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(corner.to_pos2()));
    }

    // ------------------------------------------------------------------
    // Layout

    /// The top bar of the Midnight theme: name on the left, updates on the right.
    /// "● 2 servers are running", opening the Servers page: servers are
    /// easy to forget, and they stay reachable until stopped.
    fn servers_reminder(&mut self, ui: &mut Ui, p: &Palette) {
        let n = self.servers.running_count();
        if n == 0 {
            return;
        }
        ui.add_space(14.0);
        let text = i18n::trn(n as u64, "1 server is running.", "{n} servers are running.");
        let r = ui
            .add(
                egui::Button::new(
                    RichText::new(format!("● {}", text.trim_end_matches('.'))).color(p.success).size(13.0),
                )
                .fill(p.success.gamma_multiply(0.15))
                .corner_radius(8),
            )
            .on_hover_text(tr("Open Servers to see or stop them"));
        if r.clicked() {
            self.page = Page::Servers;
        }
    }

    fn header(&mut self, ui: &mut Ui, p: &Palette) {
        let white = Color32::from_rgb(245, 247, 252);
        ui.horizontal_centered(|ui| {
            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(Vec2::splat(30.0)));
            ui.add_space(4.0);
            ui.label(theme::semibold("Network", 17.0).color(white));
            ui.label(RichText::new("Manager").color(white.gamma_multiply(0.75)).size(17.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.updater.available().is_some() {
                    self.updater.button(ui, p);
                } else {
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).color(white.gamma_multiply(0.7)));
                }
                self.servers_reminder(ui, p);
                if let Some(a) = self.shared.default_adapter() {
                    let (glyph, text) = match a.kind {
                        adapters::Kind::WiFi => (
                            icon::WIFI_HIGH,
                            self.shared.wifi.first().and_then(|w| w.ssid.clone()).unwrap_or_else(|| tr("Wi-Fi").into()),
                        ),
                        _ => (icon::PLUGS_CONNECTED, a.name.clone()),
                    };
                    ui.add_space(14.0);
                    let ip = a.main_ipv4().map(|(ip, _)| ip.to_string()).unwrap_or_default();
                    ui.label(
                        RichText::new(format!("{glyph}  {text}   {ip}")).color(white.gamma_multiply(0.8)).size(13.5),
                    );
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui, p: &Palette) {
        if p.header.is_none() {
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.add(egui::Image::new(&self.logo).fit_to_exact_size(Vec2::splat(40.0)));
                ui.vertical(|ui| {
                    ui.add_space(2.0);
                    ui.label(theme::semibold("Network", 16.0).color(p.text));
                    ui.label(RichText::new("Manager").color(p.weak).size(13.0));
                });
            });
        }
        ui.add_space(14.0);
        // Search: also ⌘K / Ctrl+K.
        let key = if cfg!(target_os = "macos") { "⌘K" } else { "Ctrl+K" };
        let search = egui::Button::new(
            RichText::new(format!("{}   {}", icon::MAGNIFYING_GLASS, tr("Search…"))).color(p.weak).size(13.5),
        )
        .right_text(RichText::new(key).color(p.weak).size(12.0))
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(8)
        .min_size(Vec2::new(ui.available_width(), 32.0));
        if ui.add(search).clicked() {
            self.search.show();
        }
        if p.header.is_none() && self.servers.running_count() > 0 {
            ui.add_space(6.0);
            ui.horizontal(|ui| self.servers_reminder(ui, p));
        }
        ui.add_space(12.0);

        let dot = |on: bool| on.then(|| "●".to_string());
        let devices_badge = dot(self.devices.scanning()).or(self.devices.count().map(|n| n.to_string()));
        let items: [(&str, &str, Page, Option<String>); 13] = [
            (icon::GAUGE, tr("Overview"), Page::Overview, None),
            (icon::PLUGS_CONNECTED, tr("Adapters"), Page::Adapters, None),
            (icon::WIFI_HIGH, tr("Wi-Fi"), Page::Wifi, None),
            (icon::DEVICES, tr("Devices"), Page::Devices, devices_badge),
            (icon::TREE_STRUCTURE, tr("Switch port"), Page::SwitchPort, dot(self.switchport.busy())),
            (icon::HEARTBEAT, tr("Monitor"), Page::Monitor, dot(self.monitor.running())),
            (icon::STACK, tr("Profiles"), Page::Profiles, None),
            (icon::TOOLBOX, tr("Tools"), Page::Tools, dot(self.tools.running())),
            (icon::HARD_DRIVES, tr("Servers"), Page::Servers, dot(self.servers.running())),
            (icon::TERMINAL_WINDOW, tr("Console"), Page::Console, dot(self.console.connected())),
            (icon::GEAR_SIX, tr("Settings"), Page::Settings, None),
            (icon::QUESTION, tr("Help"), Page::Help, None),
            (icon::INFO, tr("About"), Page::About, None),
        ];
        // Scrolls on small windows, above the "Powered by" footer.
        let height = (ui.available_height() - 86.0).max(120.0);
        // Simple mode hides the pages for network engineers (unless one of
        // them is running something).
        let simple = self.shared.settings.simple_mode;
        egui::ScrollArea::vertical().id_salt("nav").max_height(height).show(ui, |ui| {
            for (i, (glyph, label, target, badge)) in items.into_iter().enumerate() {
                let engineer = matches!(target, Page::SwitchPort | Page::Monitor | Page::Servers | Page::Console);
                if simple && engineer && badge.is_none() && self.page != target {
                    continue;
                }
                if i == 10 {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                }
                if nav_item(ui, p, glyph, label, self.page == target, badge.as_deref()).clicked() {
                    self.page = target;
                }
            }
        });

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).color(p.weak).size(12.0));
            });
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(icon::SPARKLE).color(p.accent).size(13.0));
                ui.label(theme::semibold(netmgr::POWERED_BY, 12.5).color(p.text));
            });
            if p.header.is_none() && self.updater.available().is_some() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    self.updater.button(ui, p);
                });
            }
            ui.add_space(4.0);
            ui.separator();
        });
    }

    fn content(&mut self, ui: &mut Ui, p: &Palette) {
        let sh = &mut self.shared;
        match self.page {
            Page::Overview => self.overview.ui(ui, p, sh),
            Page::Adapters => self.adapters_page.ui(ui, p, sh),
            Page::Wifi => self.wifi_page.ui(ui, p, sh),
            Page::Devices => self.devices.ui(ui, p, sh),
            Page::Profiles => self.profiles.ui(ui, p, sh),
            Page::Tools => self.tools.ui(ui, p, sh),
            Page::SwitchPort => self.switchport.ui(ui, p, sh),
            Page::Monitor => self.monitor.ui(ui, p, sh),
            Page::Servers => self.servers.ui(ui, p, sh),
            Page::Console => self.console.ui(ui, p, sh),
            Page::Settings => settings::page(ui, p, &mut sh.settings),
            Page::Help => help::page(ui, p),
            Page::About => about::page(ui, p, &self.logo, &mut self.updater),
        }
        // Background work of pages that are not shown keeps going.
        let ctx = ui.ctx().clone();
        self.devices.poll(&ctx, sh);
        self.monitor.poll(&ctx, sh);
        self.tools.poll(sh);
        self.console.poll();
        if let Some(nav) = sh.nav.take() {
            match nav {
                Nav::Page(page) => self.page = page,
                Nav::Adapter(id) => {
                    self.adapters_page.select(&id);
                    self.page = Page::Adapters;
                }
                Nav::Tool(tab, host) => {
                    self.tools.open(tab, &host);
                    self.page = Page::Tools;
                }
                Nav::Monitor(host) => {
                    self.monitor.add(sh, &host);
                    self.page = Page::Monitor;
                }
            }
        }
    }

    fn error_modal(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(msg) = self.shared.error.clone() else { return };
        let modal = egui::Modal::new(egui::Id::new("error")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING_CIRCLE).size(26.0).color(p.danger));
                ui.label(theme::semibold(tr("Something went wrong"), 18.0).color(p.text));
            });
            ui.add_space(8.0);
            theme::paragraph(ui, &msg, 14.5, p.text);
            let lower = msg.to_lowercase();
            if lower.contains("denied") || lower.contains("administrator") || lower.contains("elevation") || lower.contains("access is") {
                ui.add_space(8.0);
                let hint = if cfg!(windows) {
                    trl("Changing network settings needs administrator rights: close the app, right-click it and choose \"Run as administrator\".")
                } else {
                    trl("Changing network settings needs your password: try again and enter it when the system asks.")
                };
                theme::paragraph(ui, hint, 14.5, p.weak);
            }
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| theme::primary_button(ui, p, &format!("  {}  ", tr("OK")), true).clicked())
                .inner
        });
        if modal.inner || modal.should_close() {
            self.shared.error = None;
        }
    }

    fn toast(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some((text, at)) = &self.shared.toast else { return };
        if at.elapsed() > Duration::from_secs(4) {
            self.shared.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, [118.0, -24.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(p.card_alt)
                    .stroke(Stroke::new(1.0, p.success.gamma_multiply(0.6)))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(16, 10))
                    .shadow(ctx.global_style().visuals.popup_shadow)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icon::CHECK_CIRCLE).size(18.0).color(p.success));
                            ui.label(RichText::new(text).size(14.5).color(p.text));
                        });
                    });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fit_to_screen(&ctx);
        self.apply_settings(&ctx);
        for c in self.menu.commands(&ctx, i18n::current()) {
            self.run(&ctx, c);
        }
        self.refresh(&ctx);
        if self.updater.poll(&ctx) {
            // The installer takes over and starts the new version.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let midnight = self.shared.settings.theme == settings::ThemeChoice::Midnight;
        let p = Palette::new(ui.visuals().dark_mode, midnight, self.shared.settings.accent);
        if let Some(fill) = p.header {
            egui::Panel::top("header")
                .exact_size(56.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(Frame::new().fill(fill).inner_margin(Margin::symmetric(18, 0)))
                .show(ui, |ui| self.header(ui, &p));
        }
        egui::Panel::left("sidebar")
            .exact_size(236.0)
            .resizable(false)
            .frame(
                Frame::new().fill(p.sidebar).stroke(Stroke::new(1.0, p.border)).inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| self.sidebar(ui, &p));
        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 30, right: 30, top: 26, bottom: 22 }))
            .show(ui, |ui| self.content(ui, &p));
        if let Some(t) = self.search.ui(&ctx, &p) {
            self.go(&ctx, t);
        }
        self.error_modal(&ctx, &p);
        self.toast(&ctx, &p);
        self.updater.dialog(&ctx, &p, false);

        #[cfg(debug_assertions)]
        self.run_tour(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        #[cfg(debug_assertions)]
        if self.tour.is_some() {
            return;
        }
        eframe::set_value(storage, SETTINGS_KEY, &self.shared.settings);
    }
}

fn nav_item(ui: &mut Ui, p: &Palette, glyph: &str, label: &str, active: bool, badge: Option<&str>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if active {
            painter.rect_filled(rect, CornerRadius::same(10), p.tint(p.accent));
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 10.0), Vec2::new(3.0, rect.height() - 20.0)),
                CornerRadius::same(2),
                p.accent,
            );
        } else if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(10), p.card_alt);
        }
        let color = if active { p.text } else { p.weak };
        let y = rect.center().y;
        painter.text(
            egui::pos2(rect.left() + 16.0, y),
            egui::Align2::LEFT_CENTER,
            glyph,
            egui::FontId::proportional(19.0),
            if active { p.accent } else { color },
        );
        painter.text(
            egui::pos2(rect.left() + 46.0, y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::new(15.0, egui::FontFamily::Name(theme::SEMIBOLD.into())),
            color,
        );
        if let Some(b) = badge {
            let c = if b == "●" { p.accent } else { p.weak };
            painter.text(
                egui::pos2(rect.right() - 12.0, y),
                egui::Align2::RIGHT_CENTER,
                b,
                egui::FontId::proportional(12.5),
                c,
            );
        }
    }
    ui.add_space(2.0);
    resp
}

/// Opens a file, folder or web address with the system.
pub fn open_path(path: &str) {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = netmgr::cmd::command(program).arg(path).spawn();
}

/// Development aid (debug builds only): with `NETMGR_TOUR_DIR` set, the app
/// visits every page and saves a screenshot of each, then exits.
#[cfg(debug_assertions)]
mod tour {
    use std::path::PathBuf;

    pub struct Tour {
        pub dir: PathBuf,
        pub step: usize,
        pub frames: u32,
        pub waiting: Option<String>,
    }

    impl Tour {
        pub fn from_env() -> Option<Self> {
            let dir = PathBuf::from(std::env::var_os("NETMGR_TOUR_DIR")?);
            std::fs::create_dir_all(&dir).ok()?;
            Some(Self { dir, step: 0, frames: 0, waiting: None })
        }
    }
}

#[cfg(debug_assertions)]
impl App {
    fn run_tour(&mut self, ctx: &egui::Context) {
        let Some(t) = self.tour.as_mut() else { return };
        ctx.request_repaint();
        if let Some(name) = t.waiting.clone() {
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                let _ = image::save_buffer(
                    t.dir.join(format!("{name}.png")),
                    img.as_raw(),
                    img.width() as u32,
                    img.height() as u32,
                    image::ColorType::Rgba8,
                );
                t.waiting = None;
                t.step += 1;
                t.frames = 0;
            }
            return;
        }
        t.frames += 1;
        let pages: [(Page, &str, u32); 26] = [
            (Page::Overview, "01-overview", 140),
            (Page::Adapters, "02-adapters", 30),
            (Page::Wifi, "03-wifi", 120),
            (Page::Devices, "04-devices", 30),
            (Page::SwitchPort, "05-switch-port", 30),
            (Page::Monitor, "06-monitor", 30),
            (Page::Profiles, "07-profiles", 20),
            (Page::Tools, "08-tools", 20),
            (Page::Servers, "09-servers", 30),
            (Page::Console, "10-console", 20),
            (Page::Settings, "11-settings", 20),
            (Page::Help, "12-help", 20),
            (Page::About, "13-about", 20),
            (Page::Monitor, "14-path", 20),
            (Page::Tools, "15-planner", 20),
            (Page::Tools, "16-snmp", 400),
            (Page::Console, "17-ssh", 120),
            (Page::Monitor, "18-programs", 700),
            (Page::Monitor, "19-capture", 60),
            (Page::Devices, "20-map", 40),
            (Page::Devices, "21-bonjour", 40),
            (Page::Tools, "22-time", 400),
            (Page::Wifi, "23-signal", 60),
            (Page::Servers, "24-dhcp", 40),
            (Page::Servers, "25-traps", 40),
            (Page::Overview, "26-search", 30),
        ];
        // NETMGR_TOUR_LIGHT: the light theme instead.
        if t.frames == 1 && t.step == 0 && std::env::var_os("NETMGR_TOUR_LIGHT").is_some() {
            self.shared.settings.theme = settings::ThemeChoice::Light;
        }
        // NETMGR_TOUR_LANG=de|ar|es|fr|ru|zh|tr: that language instead.
        if t.frames == 1 && t.step == 0 {
            use i18n::Language as L;
            let lang = match std::env::var("NETMGR_TOUR_LANG").as_deref() {
                Ok("en") => Some(L::English),
                Ok("de") => Some(L::German),
                Ok("ar") => Some(L::Arabic),
                Ok("es") => Some(L::Spanish),
                Ok("fr") => Some(L::French),
                Ok("ru") => Some(L::Russian),
                Ok("zh") => Some(L::Chinese),
                Ok("tr") => Some(L::Turkish),
                _ => None,
            };
            if let Some(lang) = lang {
                self.shared.settings.language = lang;
            }
        }
        // NETMGR_TOUR_DEMO: sample data instead of this computer's networks.
        if std::env::var_os("NETMGR_TOUR_DEMO").is_some() {
            if t.frames == 1 && t.step == 0 {
                self.wifi_page.demo();
                self.devices.demo();
                self.switchport.demo();
                self.monitor.demo();
                self.servers.demo();
                self.shared.settings.lookup_public_ip = false;
                self.shared.refresh_public = false;
            }
            self.shared.public = Some(Ok(PublicInfo {
                ip: "203.0.113.24".into(),
                city: Some("Springfield".into()),
                country: Some("US".into()),
                org: Some("AS64500 Example Broadband".into()),
                ..Default::default()
            }));
            self.shared.wifi = vec![wifi::Connection {
                interface: "en0".into(),
                ssid: Some("Home-5G".into()),
                bssid: Some("B4:FB:E4:10:20:31".into()),
                signal: Some(96),
                rssi: Some(-48),
                noise: Some(-94),
                channel: Some(36),
                band: Some("5 GHz".into()),
                width: Some("80 MHz".into()),
                security: Some("WPA2 Personal".into()),
                standard: Some("802.11ax (Wi-Fi 6)".into()),
                rate_mbps: Some(1201.0),
            }];
            self.shared.wifi_loaded = true;
            self.shared.lan_denied = false;
            // Sample traffic instead of this computer's.
            self.shared.traffic.down =
                (0..60).map(|i| 4e6 + 3e6 * ((i as f64) * 0.35).sin().abs() + 1e6 * ((i * 7 % 5) as f64)).collect();
            self.shared.traffic.up = (0..60).map(|i| 0.6e6 + 0.5e6 * ((i as f64) * 0.5).cos().abs()).collect();
            for a in &mut self.shared.adapters {
                if a.default {
                    a.ipv4 = vec![([192, 168, 1, 10].into(), 24)];
                    a.ipv6 = vec![("2001:db8::10".parse().unwrap_or(std::net::Ipv6Addr::LOCALHOST), 64)];
                    a.gateway = Some([192, 168, 1, 1].into());
                    a.gateway_mac = "B4:FB:E4:10:20:30".parse().ok();
                    a.dns = vec![[192, 168, 1, 1].into()];
                    a.mac = "F0:18:98:11:22:33".parse().ok();
                    a.rx_bytes = Some(12_400_000_000);
                    a.tx_bytes = Some(2_100_000_000);
                    a.speed_bps = Some(1_201_000_000);
                }
            }
        }
        let Some((page, name, wait)) = pages.get(t.step).copied() else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        if t.frames == 1 {
            self.page = page;
            if name == "14-path" {
                self.monitor.show_path();
            }
            if name == "15-planner" {
                self.tools.open(tools::Tab::Planner, "");
            }
            if name == "26-search" {
                self.search.demo("dhcp");
            }
            if name == "24-dhcp" {
                self.servers.show("dhcp");
            }
            if name == "25-traps" {
                self.servers.show("traps");
            }
            if name == "20-map" {
                self.devices.show("map");
            }
            if name == "21-bonjour" {
                self.devices.show("bonjour");
            }
            if name == "22-time" {
                self.tools.open(tools::Tab::Time, "");
            }
            if name == "23-signal" {
                self.wifi_page.show_signal();
            }
            if name == "18-programs" {
                self.monitor.show_programs();
            }
            if name == "19-capture" {
                let pcap = std::env::var("NETMGR_TOUR_PCAP").ok();
                self.monitor.show_capture(&mut self.shared, pcap.as_deref());
            }
            if name == "17-ssh" {
                let target = std::env::var("NETMGR_TOUR_TELNET").ok();
                self.console.demo(&mut self.shared, target.as_deref());
            }
            if name == "16-snmp" {
                self.tools.open(tools::Tab::Snmp, "");
                // NETMGR_TOUR_SNMP=host,community: read that device.
                if let Ok(v) = std::env::var("NETMGR_TOUR_SNMP")
                    && let Some((host, community)) = v.split_once(',')
                {
                    self.tools.snmp_demo(ctx, host, community);
                }
            }
        }
        if t.frames > wait {
            let t = self.tour.as_mut().unwrap();
            t.waiting = Some(name.to_string());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }
}
