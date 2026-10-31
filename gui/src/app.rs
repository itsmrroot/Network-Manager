//! Application state, navigation and the glue between pages and jobs.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2};
use egui_phosphor::regular as icon;
use netmgr::adapters::{self, Adapter};
use netmgr::internet::PublicInfo;
use netmgr::wifi;

use crate::i18n::{self, tr, tr_dyn, trf, trl};
use crate::jobs::{self, Job};
use crate::settings::{self, Settings};
use crate::theme::{self, Palette};
use crate::update::Updater;
use crate::{
    about, adapters_page, console, devices, help, monitor, overview, profiles, servers, switchport, tools, vpn_page,
    wifi_page,
};
use crate::{menu, search};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Adapters,
    Wifi,
    Vpn,
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
    /// An SSH session to this address, in the Console.
    Ssh(String),
    /// Watch a host in the ping monitor.
    Monitor(String),
    /// A tab of a page (the page's own name for it).
    Tab(Page, &'static str),
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
    /// What others do with this computer (Servers → Activity).
    pub activity: crate::activity::Activity,
    /// Console → CLI guide, in its tab, docked or in its own window.
    pub guide: crate::guide_view::GuideState,
    /// Development aid: where the last splitter was drawn (the tour drags it).
    #[cfg(debug_assertions)]
    pub debug_edge: f32,
}

impl Shared {
    /// The adapters to show, following the "virtual adapters" setting.
    pub fn visible_adapters(&self) -> Vec<&Adapter> {
        self.adapters
            .iter()
            .filter(|a| {
                self.settings.show_virtual
                    || (a.physical && a.kind != adapters::Kind::Loopback)
                    // A VPN that is up is shown even when virtual adapters are hidden.
                    || netmgr::vpn::is_active_tunnel(a)
            })
            .collect()
    }

    /// The adapter of the local network. Under a VPN that carries everything
    /// the default route is the tunnel; the settings, the scans and the
    /// address shown mean the adapter under it.
    pub fn default_adapter(&self) -> Option<&Adapter> {
        let route = self.adapters.iter().find(|a| a.default);
        match route {
            Some(a) if netmgr::vpn::is_active_tunnel(a) => netmgr::vpn::local_adapter(&self.adapters).or(route),
            _ => route,
        }
    }

    /// The VPN that is up, if any.
    pub fn vpn(&self) -> Option<netmgr::vpn::Status> {
        netmgr::vpn::detect(&self.adapters)
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
    /// Desktop notifications for Servers → Activity.
    notifier: crate::activity::Notifier,
    shared: Shared,
    /// Development aid: frames since the tour began dragging the guide's edge.
    #[cfg(debug_assertions)]
    dock_drag: Option<u32>,
    #[cfg(debug_assertions)]
    dock_drag_edge: f32,
    #[cfg(debug_assertions)]
    dock_drag_split: bool,
    applied: Option<(theme::Accent, f32, settings::ThemeChoice)>,
    /// The icon shown in the Dock or task bar.
    icon_shown: Option<theme::AppIcon>,
    /// Closing waits for an answer: save this session's server logs?
    ask_save_logs: bool,
    /// The answer was given: closing may go ahead.
    close_ok: bool,
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
    vpn_page: vpn_page::VpnPage,
    wifi_page: wifi_page::WifiPage,
    wifi_ip: crate::wifi_ip::WifiIp,
    devices: devices::Devices,
    profiles: profiles::Profiles,
    tools: tools::Tools,
    switchport: switchport::SwitchPort,
    monitor: monitor::Monitor,
    servers: servers::Servers,
    console: console::Console,
    /// The app was minimised or hidden when a Dock menu item woke it: frames since then.
    /// Once the session window draws, the main window goes away again.
    away: Option<u32>,
    /// The main window is invisible while only terminal windows of their own are wanted.
    #[cfg(target_os = "macos")]
    faded: bool,
    #[cfg(debug_assertions)]
    tour: Option<tour::Tour>,
}

const SETTINGS_KEY: &str = "settings";

/// The window size on first start, in points.
pub const DEFAULT_SIZE: [f32; 2] = [1240.0, 800.0];
/// The smallest window in which every page still works.
pub const MIN_SIZE: [f32; 2] = [860.0, 560.0];
/// The height of the menu bar that the themes without a header have (Windows, Linux).
const MENU_STRIP: f32 = 30.0;

/// Is this the first start (no window place saved)? Set by `main`.
pub static FIRST_RUN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a window cannot use of a screen's height: title bar and the menu bar, Dock or taskbar.
const SCREEN_MARGIN: f32 = if cfg!(target_os = "macos") {
    130.0
} else if cfg!(windows) {
    90.0
} else {
    100.0
};

/// The size a window of `size` gets on a screen of `monitor` points: the same
/// everywhere (DEFAULT_SIZE) as long as it fits with its title bar and the
/// taskbar or Dock, smaller only on a screen that is too small for it, and not
/// under the smallest size.
pub fn fitted_size(size: egui::Vec2, monitor: egui::Vec2) -> egui::Vec2 {
    let max = egui::vec2(monitor.x * 0.95, monitor.y - SCREEN_MARGIN);
    let min = egui::vec2(MIN_SIZE[0], MIN_SIZE[1]).min(max);
    size.min(max).max(min)
}

/// Where the top-left corner of a window of `fitted` goes on a screen of
/// `monitor` points: centred, but never low enough for its bottom edge to
/// reach into the SCREEN_MARGIN reserved for the title bar and the taskbar
/// or Dock (`monitor` is the whole screen, taskbar and Dock included).
fn safe_corner(monitor: egui::Vec2, fitted: egui::Vec2) -> egui::Vec2 {
    let centered = ((monitor - fitted) / 2.0).max(egui::Vec2::ZERO);
    let max_y = (monitor.y - SCREEN_MARGIN - fitted.y).max(0.0);
    egui::vec2(centered.x, centered.y.min(max_y))
}

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
        let logo = theme::icon_texture(&cc.egui_ctx, settings.app_icon);
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
        let activity_log = crate::activity::Activity::default();
        crate::activity::watch_ssh(activity_log.clone());
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
                activity: activity_log,
                guide: Default::default(),
                #[cfg(debug_assertions)]
                debug_edge: 0.0,
            },
            notifier: Default::default(),
            applied: None,
            icon_shown: None,
            ask_save_logs: false,
            close_ok: false,
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
            vpn_page: Default::default(),
            wifi_page: Default::default(),
            wifi_ip: Default::default(),
            devices: Default::default(),
            profiles: Default::default(),
            tools: Default::default(),
            switchport: Default::default(),
            monitor: Default::default(),
            servers: Default::default(),
            console: Default::default(),
            away: None,
            #[cfg(target_os = "macos")]
            faded: false,
            #[cfg(debug_assertions)]
            tour,
            #[cfg(debug_assertions)]
            dock_drag: None,
            #[cfg(debug_assertions)]
            dock_drag_edge: 0.0,
            #[cfg(debug_assertions)]
            dock_drag_split: false,
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
        // Every command takes what the wake-up found, so that it cannot wait for a later one.
        #[cfg(target_os = "macos")]
        let was_away = crate::dock::take_away();
        match c {
            C::Page(p) => self.page = p,
            C::Search => self.search.show(),
            C::CheckUpdates => {
                self.page = Page::About;
                self.updater.check(ctx, true);
            }
            C::Undo | C::Redo | C::Cut | C::Copy | C::Paste | C::SelectAll => {
                menu::edit(ctx, c, self.console.focused_window(ctx));
                // The main window takes its own key presses now; a window of its own, in its next frame.
                menu::take_edits(ctx);
            }
            C::ToggleSidebar => self.shared.settings.sidebar_hidden = !self.shared.settings.sidebar_hidden,
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
                } else {
                    self.shared.toast(tr("No network adapter is connected."));
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
            C::IpSettings => {
                self.page = Page::Adapters;
                if let Some(adapter) = self.shared.default_adapter().cloned() {
                    self.adapters_page.edit_ip(&adapter);
                } else {
                    self.shared.toast(tr("No network adapter is connected."));
                }
            }
            C::NewConnection => {
                self.page = Page::Console;
                self.console.new_connection();
            }
            C::OpenSession(i) => {
                // From a menu a session opens in a window of its own; "Bring back" puts it in the app.
                // A list that changed since the menu was made: nothing is opened, nothing popped out.
                if self.console.open_saved(i, &mut self.shared) {
                    self.console.pop_out();
                    // The Dock menu woke the app up; if the main window was away, only the session window stays.
                    #[cfg(target_os = "macos")]
                    if was_away {
                        self.away = Some(0);
                    }
                }
                ctx.request_repaint();
            }
            C::ConsoleTab(tab) => {
                self.page = Page::Console;
                self.console.open_tab(tab);
            }
            C::PopOut => {
                // In a window of its own already: the same key brings it back.
                let window = self.console.focused_window(ctx);
                if window != egui::ViewportId::ROOT {
                    self.console.bring_back(window);
                } else {
                    self.page = Page::Console;
                    self.console.pop_out();
                }
            }
            C::FullScreen => {
                let full = ctx.input(|i| i.viewport().fullscreen).unwrap_or(false);
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
            }
            // Through the same way as the close button, which asks about unsaved logs.
            C::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// The menu bar of Windows and Linux (File, Edit, View, …): what is chosen is done at once.
    /// `room` is how much width the menus may take.
    #[cfg(not(target_os = "macos"))]
    fn menu_bar(&mut self, ui: &mut Ui, text: Color32, room: f32) {
        let sessions: Vec<menu::SessionEntry> =
            self.shared.settings.sessions.iter().map(|s| (s.name.clone(), s.icon.clone())).collect();
        let state = menu::BarState { sessions: &sessions, theme: self.shared.settings.theme, text, room };
        let ctx = ui.ctx().clone();
        for c in menu::bar(ui, &state) {
            self.run(&ctx, c);
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
        if self.icon_shown != Some(s.app_icon) {
            show_icon(ctx, s.app_icon);
            // Also when the app is closed (and again after an update put
            // the app's own icon back). The blue one is the app's own.
            if self.icon_shown.is_some() || s.app_icon != theme::AppIcon::Blue {
                crate::appicon::keep(s.app_icon);
            }
            self.logo = theme::icon_texture(ctx, s.app_icon);
            self.icon_shown = Some(s.app_icon);
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

    /// A safety net: shrinks and centres the window when it is bigger than the
    /// screen (a window that was left on a bigger one). The right size is
    /// worked out before the window exists (see [`fitted_size`]), so that it
    /// opens in its place and does not jump; and a window that was left
    /// maximized or over the whole screen stays so.
    fn fit_to_screen(&mut self, ctx: &egui::Context) {
        if self.fitted {
            return;
        }
        let (monitor, inner, maximized, full) = ctx.input(|i| {
            let v = i.viewport();
            (v.monitor_size, v.inner_rect, v.maximized.unwrap_or(false), v.fullscreen.unwrap_or(false))
        });
        let (Some(monitor), Some(inner)) = (monitor, inner) else { return };
        self.fitted = true;
        let size = inner.size();
        // The first start has no place to preserve, so it is always placed safely, even
        // when the default size already fits: the window system's own placement does not
        // know about the taskbar or Dock (see `safe_corner`). A later start only touches
        // a window left bigger than this screen.
        let needs_fit = if FIRST_RUN.load(std::sync::atomic::Ordering::Relaxed) {
            true
        } else {
            size.x > monitor.x || size.y > monitor.y
        };
        if maximized || full || !needs_fit {
            return;
        }
        let fitted = fitted_size(size, monitor);
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(MIN_SIZE[0], MIN_SIZE[1]).min(fitted)));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(fitted));
        let corner = safe_corner(monitor, fitted);
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
                    RichText::new(format!("● {}", text.trim_end_matches(['.', '。']))).color(p.success).size(13.0),
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
        // Arabic: the name on the right, the status on the left.
        let (outer, inner) = if i18n::is_rtl() {
            (Layout::right_to_left(Align::Center), Layout::left_to_right(Align::Center))
        } else {
            (Layout::left_to_right(Align::Center), Layout::right_to_left(Align::Center))
        };
        ui.with_layout(outer, |ui| {
            self.sidebar_button(ui, white);
            ui.add_space(6.0);
            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(Vec2::splat(30.0)));
            ui.add_space(4.0);
            // Windows and Linux: the menus (File, Edit, …) as VS Code has them,
            // in place of the name, which the title of the window says already.
            // The status on the right keeps its room (more with a VPN or servers).
            #[cfg(not(target_os = "macos"))]
            {
                let extra = if self.shared.vpn().is_some() { 170.0 } else { 0.0 }
                    + if self.servers.running_count() > 0 { 190.0 } else { 0.0 };
                let room = (ui.available_width() - 400.0 - extra).max(0.0);
                self.menu_bar(ui, white, room);
            }
            // The name reads "Network Manager" either way (this row runs from
            // the right for Arabic, so "Manager" comes first there).
            #[cfg(target_os = "macos")]
            {
                let network = theme::semibold("Network", 17.0).color(white);
                let manager = RichText::new("Manager").color(white.gamma_multiply(0.75)).size(17.0);
                if i18n::is_rtl() {
                    ui.label(manager);
                    ui.label(network);
                } else {
                    ui.label(network);
                    ui.label(manager);
                }
            }
            ui.with_layout(inner, |ui| {
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
                if let Some(v) = self.shared.vpn() {
                    ui.add_space(10.0);
                    let name = tr_dyn(v.product.label());
                    let text = if v.product == netmgr::vpn::Product::Other {
                        name.clone()
                    } else {
                        trf("VPN · {name}", &[("name", &name)])
                    };
                    let hint = if v.full {
                        trf("All internet traffic goes through the VPN ({name}).", &[("name", &name)])
                    } else {
                        trf(
                            "Only traffic for the VPN's own networks goes through {name}; the rest of the internet uses your local network.",
                            &[("name", &name)],
                        )
                    };
                    let color = if v.full { p.success } else { p.warning };
                    ui.horizontal(|ui| {
                        theme::pill(ui, p, &format!("{}  {text}", icon::SHIELD_CHECK), color).on_hover_text(hint);
                    });
                }
            });
        });
    }

    /// The button that hides the list of pages on the left, and brings it back.
    fn sidebar_button(&mut self, ui: &mut Ui, color: Color32) {
        let hidden = self.shared.settings.sidebar_hidden;
        let button = egui::Button::new(RichText::new(icon::SIDEBAR_SIMPLE).size(19.0).color(color)).frame(false);
        let key = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
        let hint = format!("{} ({key}+\\)", tr("Hide or show sidebar"));
        let resp = ui.add(button.selected(hidden)).on_hover_text(hint);
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, true, hidden, tr("Hide or show sidebar"))
        });
        if resp.clicked() {
            self.shared.settings.sidebar_hidden = !hidden;
        }
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
                ui.with_layout(Layout::right_to_left(Align::TOP), |ui| self.sidebar_button(ui, p.weak));
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
        let vpn_badge = self.shared.vpn().is_some().then(|| "●".to_string());
        let items: [(&str, &str, Page, Option<String>); 14] = [
            (icon::GAUGE, tr("Overview"), Page::Overview, None),
            (icon::PLUGS_CONNECTED, tr("Adapters"), Page::Adapters, None),
            (icon::WIFI_HIGH, tr("Wi-Fi"), Page::Wifi, None),
            (icon::SHIELD_CHECK, tr("VPN"), Page::Vpn, vpn_badge),
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
        // Room above the footer, as tall as it was last time.
        let footer_id = egui::Id::new("sidebar-footer");
        let footer: f32 = ui.data(|d| d.get_temp(footer_id)).unwrap_or(110.0);
        let height = (ui.available_height() - footer - 8.0).max(120.0);
        // Simple mode hides the pages for network engineers (unless one of
        // them is running something).
        let simple = self.shared.settings.simple_mode;
        // Rows a little lower when the window is short, so that all fit
        // (a server notice in the sidebar takes room too).
        // Per row: its height, 2 points of space and egui's own spacing;
        // plus the separator before Settings.
        let gap = 2.0 + ui.spacing().item_spacing.y;
        // 20 points still fit the 15-point labels, and let all 14 rows fit on
        // a 768-pixel screen (Help and About were hidden below the fold).
        let row_h = ((height - 16.0 - 2.0 * gap) / items.len() as f32 - gap).clamp(20.0, 36.0);
        egui::ScrollArea::vertical().id_salt("nav").max_height(height).show(ui, |ui| {
            for (i, (glyph, label, target, badge)) in items.into_iter().enumerate() {
                let engineer = matches!(target, Page::SwitchPort | Page::Monitor | Page::Servers | Page::Console);
                if simple && engineer && badge.is_none() && self.page != target {
                    continue;
                }
                if i == 11 {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                }
                if nav_item(ui, p, glyph, label, self.page == target, badge.as_deref(), row_h).clicked() {
                    self.page = target;
                }
            }
        });

        let side = if i18n::is_rtl() { Align::Max } else { Align::Min };
        ui.with_layout(Layout::bottom_up(side), |ui| {
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
            let used = ui.min_rect().height();
            ui.data_mut(|d| d.insert_temp(footer_id, used));
        });
    }

    fn content(&mut self, ui: &mut Ui, p: &Palette) {
        let sh = &mut self.shared;
        match self.page {
            Page::Overview => self.overview.ui(ui, p, sh),
            Page::Adapters => self.adapters_page.ui(ui, p, sh),
            Page::Wifi => self.wifi_page.ui(ui, p, sh),
            Page::Vpn => self.vpn_page.ui(ui, p, sh),
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
        self.console.poll(&ctx, sh);
        self.wifi_ip.tick(&ctx, p, sh);
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
                Nav::Ssh(host) => {
                    self.console.open_host(&host);
                    self.page = Page::Console;
                }
                Nav::Monitor(host) => {
                    self.monitor.add(sh, &host);
                    self.page = Page::Monitor;
                }
                Nav::Tab(page, id) => self.go(&ctx, search::Target::Tab(page, id)),
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
            // Not for a server's port: its message says what to do.
            if (lower.contains("denied") || lower.contains("administrator") || lower.contains("elevation") || lower.contains("access is"))
                && !lower.contains("port ")
            {
                ui.add_space(8.0);
                let hint = if cfg!(windows) {
                    trl("Changing network settings needs administrator rights: close the app, right-click it and choose \"Run as administrator\".")
                } else {
                    trl("Changing network settings needs your password: try again and enter it when the system asks.")
                };
                theme::paragraph(ui, hint, 14.5, p.weak);
            }
            ui.add_space(12.0);
            let clicked = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| theme::primary_button(ui, p, &format!("  {}  ", tr("OK")), true).clicked())
                .inner;
            // egui's Modal closes on Escape and a backdrop click on its own, but not
            // on Enter: without this the OK button looks like a default action but
            // only the mouse can press it.
            clicked || ui.input(|i| i.key_pressed(egui::Key::Enter))
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
    /// Development aid: the tour can drag the guide's edge with the mouse.
    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        // NETMGR_TOUR_CLICK=x,y: a click there, 40 frames into a step.
        if let (Some(t), Ok(at)) = (&self.tour, std::env::var("NETMGR_TOUR_CLICK"))
            && t.frames == 40
            && let Some((x, y)) =
                at.split_once(',').and_then(|(x, y)| Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?)))
        {
            let pos = egui::Pos2::new(x, y);
            raw.events.push(egui::Event::PointerMoved(pos));
            for pressed in [true, false] {
                raw.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        let Some(n) = self.dock_drag.as_mut() else { return };
        *n += 1;
        let by: f32 = std::env::var("NETMGR_TOUR_DRAG").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        if *n == 5 {
            self.dock_drag_edge =
                if self.dock_drag_split { self.shared.debug_edge } else { self.shared.guide.dock_edge };
        }
        let edge = if *n >= 5 { self.dock_drag_edge } else { self.shared.guide.dock_edge };
        let at = |dx: f32| egui::Pos2::new(edge + dx, 400.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        match *n {
            5 => raw.events.push(egui::Event::PointerMoved(at(0.0))),
            6 => raw.events.push(button(at(0.0), true)),
            7..=12 => raw.events.push(egui::Event::PointerMoved(at(by * (*n - 6) as f32 / 6.0))),
            13 => raw.events.push(button(at(by), false)),
            _ => {}
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fit_to_screen(&ctx);
        self.apply_settings(&ctx);
        // A Paste button next to a session that is not in a window of its own queues its
        // edit for this, the main window; unlike the menu and its shortcuts, it has not
        // already drained the queue itself (see `menu::edit`), so this does it instead.
        menu::take_edits(&ctx);
        let sessions: Vec<menu::SessionEntry> =
            self.shared.settings.sessions.iter().map(|s| (s.name.clone(), s.icon.clone())).collect();
        for c in self.menu.commands(&ctx, i18n::current(), &sessions) {
            self.run(&ctx, c);
        }
        self.refresh(&ctx);
        // What others did meanwhile: noted, and notified as chosen.
        self.servers.watch(&self.shared);
        crate::notify::set_sound(self.shared.settings.notify_sound);
        crate::logstore::configure(self.shared.settings.keep_logs, &self.shared.settings.logs_folder());
        let events = self.shared.activity.take_pending();
        self.notifier.handle(events, &self.shared.settings);
        // Server logs that are in no file (not kept): ask before they go.
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_ok && crate::logstore::unsaved() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.ask_save_logs = true;
        }
        if self.ask_save_logs {
            self.save_logs_dialog(&ctx);
        }
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
        // The other themes have no header: the menus get a slim bar of their own.
        #[cfg(not(target_os = "macos"))]
        if p.header.is_none() {
            let layout = if i18n::is_rtl() {
                Layout::right_to_left(Align::Center)
            } else {
                Layout::left_to_right(Align::Center)
            };
            egui::Panel::top("menubar")
                .exact_size(MENU_STRIP)
                .resizable(false)
                .show_separator_line(true)
                .frame(Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(8, 0)))
                .show(ui, |ui| {
                    ui.with_layout(layout, |ui| {
                        let room = ui.available_width();
                        self.menu_bar(ui, p.text, room);
                    })
                });
        }
        // Arabic reads from the right: the sidebar is there too.
        let sidebar = if i18n::is_rtl() { egui::Panel::right("sidebar") } else { egui::Panel::left("sidebar") };
        if !self.shared.settings.sidebar_hidden {
            sidebar
                .exact_size(236.0)
                .resizable(false)
                .frame(
                    Frame::new()
                        .fill(p.sidebar)
                        .stroke(Stroke::new(1.0, p.border))
                        .inner_margin(Margin::symmetric(12, 0)),
                )
                .show(ui, |ui| self.sidebar(ui, &p));
        }
        // The CLI guide beside the console, or in a window of its own.
        let guide_action = {
            let (g, cfg) = (&mut self.shared.guide, &mut self.shared.settings.guide);
            let docked = self.page == Page::Console && cfg.docked && !g.popped && !self.console.showing_guide();
            let a = if docked { crate::guide_view::dock(ui, &p, g, cfg) } else { None };
            let b = if g.popped { crate::guide_view::window(&ctx, &p, g, cfg) } else { None };
            a.or(b)
        };
        // Sessions and the serial console that were opened in their own windows.
        self.console.windows(&ctx, &p, &mut self.shared);
        if let Some(frames) = self.away {
            // The main window must stay shown, or the windows of the sessions stop answering
            // (they are drawn by it): it is made invisible instead of minimised.
            if frames >= 5 && self.console.has_window() {
                #[cfg(target_os = "macos")]
                {
                    crate::dock::fade_main(true);
                    self.faded = true;
                }
                self.away = None;
            } else if frames >= 120 {
                self.away = None;
            } else {
                self.away = Some(frames + 1);
                ctx.request_repaint();
            }
        }
        // The last window of its own is gone: the main window is there again.
        #[cfg(target_os = "macos")]
        if self.faded && !self.console.has_window() {
            crate::dock::fade_main(false);
            self.faded = false;
        }
        match guide_action {
            Some(crate::guide_view::Action::OpenTab) => self.shared.nav = Some(Nav::Tab(Page::Console, "guide")),
            Some(crate::guide_view::Action::OpenConsole) => self.shared.nav = Some(Nav::Tab(Page::Console, "serial")),
            None => {}
        }
        // Without the header there is no button for the sidebar once it is hidden.
        if self.shared.settings.sidebar_hidden && p.header.is_none() {
            let at = if i18n::is_rtl() { egui::Align2::RIGHT_TOP } else { egui::Align2::LEFT_TOP };
            // Below the menu strip, where there is one.
            let strip = if cfg!(target_os = "macos") { 0.0 } else { MENU_STRIP };
            egui::Area::new(egui::Id::new("sidebar-button"))
                .anchor(at, egui::vec2(if i18n::is_rtl() { -10.0 } else { 10.0 }, 8.0 + strip))
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| self.sidebar_button(ui, p.weak));
        }
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

fn nav_item(
    ui: &mut Ui,
    p: &Palette,
    glyph: &str,
    label: &str,
    active: bool,
    badge: Option<&str>,
    height: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    // Painted by hand: screen readers get the page's name this way.
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if active {
            painter.rect_filled(rect, CornerRadius::same(10), p.tint(p.accent));
            let x = if i18n::is_rtl() { rect.right() - 3.0 } else { rect.left() };
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, rect.top() + 10.0), Vec2::new(3.0, rect.height() - 20.0)),
                CornerRadius::same(2),
                p.accent,
            );
        } else if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(10), p.card_alt);
        }
        let color = if active { p.text } else { p.weak };
        let y = rect.center().y;
        // Right to left (Arabic): icon and text from the right, badge on the left.
        let rtl = i18n::is_rtl();
        let (start, toward, align, end_align) = if rtl {
            (rect.right(), -1.0, egui::Align2::RIGHT_CENTER, egui::Align2::LEFT_CENTER)
        } else {
            (rect.left(), 1.0, egui::Align2::LEFT_CENTER, egui::Align2::RIGHT_CENTER)
        };
        painter.text(
            egui::pos2(start + toward * 16.0, y),
            align,
            glyph,
            egui::FontId::proportional(19.0),
            if active { p.accent } else { color },
        );
        painter.text(
            egui::pos2(start + toward * 46.0, y),
            align,
            label,
            egui::FontId::new(15.0, egui::FontFamily::Name(theme::SEMIBOLD.into())),
            color,
        );
        if let Some(b) = badge {
            let c = if b == "●" { p.accent } else { p.weak };
            let end = if rtl { rect.left() + 12.0 } else { rect.right() - 12.0 };
            painter.text(egui::pos2(end, y), end_align, b, egui::FontId::proportional(12.5), c);
        }
    }
    ui.add_space(2.0);
    resp
}

impl App {
    /// "Save the server logs?" before closing.
    fn save_logs_dialog(&mut self, ctx: &egui::Context) {
        let n = crate::logstore::unsaved();
        let mut answer = None;
        egui::Modal::new(egui::Id::new("save-logs")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(theme::semibold(tr("Save the server logs?"), 18.0));
            ui.add_space(6.0);
            theme::paragraph(
                ui,
                &i18n::trlf(
                    "This session's {n} log lines are in no file (Servers → Logs → Keep server logs is off) and are lost when the app closes.",
                    &[("n", &n)],
                ),
                14.0,
                ui.visuals().weak_text_color(),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(theme::icon_label(icon::FLOPPY_DISK, "Save…")).clicked() {
                    answer = Some(1);
                }
                if ui.button(tr("Don't save")).clicked() {
                    answer = Some(2);
                }
                if ui.button(tr("Cancel")).clicked() {
                    answer = Some(0);
                }
            });
        });
        match answer {
            Some(1) => {
                let saved = rfd::FileDialog::new()
                    .set_file_name(format!("server-logs-{}.log", chrono::Local::now().format("%Y-%m-%d")))
                    .save_file()
                    .is_some_and(|path| crate::logstore::save_unsaved(&path).is_ok());
                if saved {
                    self.close_now(ctx);
                }
            }
            Some(2) => {
                crate::logstore::discard_unsaved();
                self.close_now(ctx);
            }
            Some(_) => self.ask_save_logs = false,
            None => {}
        }
    }

    fn close_now(&mut self, ctx: &egui::Context) {
        self.ask_save_logs = false;
        self.close_ok = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// `icon` in the task bar (Windows, Linux) or the Dock (macOS).
fn show_icon(ctx: &egui::Context, icon: theme::AppIcon) {
    let Some(i) = icon.image(256) else { return };
    let data = egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() };
    ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(data))));
    #[cfg(target_os = "macos")]
    set_dock_icon(icon.png());
}

/// macOS: the Dock shows the app's own icon, not the window's.
#[cfg(target_os = "macos")]
fn set_dock_icon(png: &[u8]) {
    use objc2::AnyThread;
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let data = objc2_foundation::NSData::with_bytes(png);
    let Some(image) = objc2_app_kit::NSImage::initWithData(objc2_app_kit::NSImage::alloc(), &data) else { return };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    // SAFETY: on the main thread (checked above), with a valid image.
    unsafe { app.setApplicationIconImage(Some(&image)) };
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
        /// The step the tour began at (NETMGR_TOUR_FROM).
        pub start: usize,
        /// NETMGR_TOUR_FROM: the name (its start) of the first step to visit.
        pub from: Option<String>,
        pub frames: u32,
        pub waiting: Option<String>,
    }

    impl Tour {
        pub fn from_env() -> Option<Self> {
            let dir = PathBuf::from(std::env::var_os("NETMGR_TOUR_DIR")?);
            std::fs::create_dir_all(&dir).ok()?;
            Some(Self {
                dir,
                step: 0,
                start: 0,
                from: std::env::var("NETMGR_TOUR_FROM").ok(),
                frames: 0,
                waiting: None,
            })
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
        // Pages that wait for nothing ask for no repaint: keep the tour going.
        ctx.request_repaint();
        let pages: [(Page, &str, u32); 58] = [
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
            (Page::Servers, "27-ssh", 30),
            (Page::Servers, "28-activity", 30),
            (Page::Servers, "29-logs", 30),
            (Page::Console, "30-push", 30),
            (Page::Console, "31-backups", 30),
            (Page::Wifi, "32-heatmap", 40),
            (Page::Tools, "33-snmpv3", 400),
            (Page::Servers, "34-traps-v3", 40),
            (Page::Adapters, "35-ip", 40),
            (Page::Console, "36-guide", 30),
            (Page::Console, "37-guide-dock", 30),
            (Page::Console, "38-guide-search", 30),
            (Page::Console, "39-guide-table", 30),
            (Page::Console, "40-guide-window", 80),
            (Page::Console, "41-adb-shell", 60),
            (Page::Console, "42-adb-log", 60),
            (Page::Console, "43-adb-actions", 40),
            (Page::Console, "44-art-0", 30),
            (Page::Console, "45-art-2", 30),
            (Page::Console, "46-art-4", 30),
            (Page::Console, "47-art-6", 30),
            (Page::Console, "48-dock-drag", 60),
            (Page::Console, "49-split-drag", 60),
            (Page::Console, "50-zoom-serial", 30),
            (Page::Console, "51-zoom-remote", 30),
            (Page::Console, "52-term-find", 90),
            (Page::Console, "53-dock-menu", 40),
            (Page::Console, "54-menus-hold", 2400),
            (Page::Console, "55-popout", 700),
            (Page::Vpn, "57-vpn", 60),
            (Page::Monitor, "58-history", 60),
            (Page::Overview, "56-update", 40),
            (Page::Overview, "26-search", 30),
        ];
        // NETMGR_TOUR_FROM=36-guide: begin at that step.
        if t.frames == 1
            && let Some(prefix) = t.from.take()
            && let Some(i) = pages.iter().position(|(_, n, _)| n.starts_with(prefix.as_str()))
        {
            (t.step, t.start) = (i, i);
        }
        // NETMGR_TOUR_SIDEBAR=hide: the list of pages hidden.
        if t.frames == 1 && t.step == t.start && std::env::var("NETMGR_TOUR_SIDEBAR").as_deref() == Ok("hide") {
            self.shared.settings.sidebar_hidden = true;
        }
        // NETMGR_TOUR_LIGHT: the light theme instead.
        if t.frames == 1 && t.step == t.start && std::env::var_os("NETMGR_TOUR_LIGHT").is_some() {
            self.shared.settings.theme = settings::ThemeChoice::Light;
        }
        // NETMGR_TOUR_ICON=violet|graphite|kiwi: that icon and its colours.
        if t.frames == 1 && t.step == t.start {
            let icon = match std::env::var("NETMGR_TOUR_ICON").as_deref() {
                Ok("violet") => Some(theme::AppIcon::Violet),
                Ok("graphite") => Some(theme::AppIcon::Graphite),
                Ok("kiwi") => Some(theme::AppIcon::Kiwi),
                _ => None,
            };
            if let Some(i) = icon {
                self.shared.settings.app_icon = i;
                self.shared.settings.accent = i.accent();
            }
        }
        // NETMGR_TOUR_SCALE=1.5: the interface size (narrow layouts).
        if t.frames == 1
            && t.step == t.start
            && let Some(scale) = std::env::var("NETMGR_TOUR_SCALE").ok().and_then(|s| s.parse::<f32>().ok())
        {
            self.shared.settings.ui_scale = scale;
        }
        // NETMGR_TOUR_LANG=de|ar|es|fr|ru|zh|tr: that language instead.
        if t.frames == 1 && t.step == t.start {
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
            if t.frames == 1 && t.step == t.start {
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
                if a.default && a.kind != adapters::Kind::Vpn {
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
            // NETMGR_TOUR_VPN=full|split: a VPN tunnel is up (Tailscale's kind of address).
            if let Ok(mode) = std::env::var("NETMGR_TOUR_VPN")
                && !self.shared.adapters.iter().any(|a| a.device == "utun99")
                && let Some(base) = self.shared.adapters.iter().find(|a| a.physical && a.up).cloned()
            {
                let full = mode == "full";
                let mut tunnel = base.clone();
                tunnel.id = "utun99".into();
                tunnel.name = "utun99".into();
                tunnel.device = "utun99".into();
                tunnel.kind = adapters::Kind::Vpn;
                tunnel.physical = false;
                tunnel.ipv4 = vec![([100, 92, 141, 34].into(), 32)];
                tunnel.ipv6 = Vec::new();
                tunnel.gateway = None;
                tunnel.dns = Vec::new();
                tunnel.mac = None;
                tunnel.default = full;
                for a in &mut self.shared.adapters {
                    if a.id == base.id {
                        a.default = !full;
                        a.ipv4 = vec![([192, 168, 178, 128].into(), 24)];
                    }
                }
                self.shared.adapters.push(tunnel);
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
            if name == "27-ssh" {
                self.servers.show("ssh");
            }
            if name == "29-logs" && t.frames == 1 {
                // Sample lines, in a folder of the tour's own.
                self.shared.settings.logs_dir = t.dir.join("logs").display().to_string();
                crate::logstore::configure(true, &self.shared.settings.logs_folder());
                for (server, line) in [
                    ("TFTP", "10.0.0.2:51234 downloads c2960x-universalk9.bin (octet)"),
                    ("TFTP", "10.0.0.2:51234 c2960x-universalk9.bin: done, 26.4 MB"),
                    ("Syslog", "10.0.0.3 Warning %LINEPROTO-5-UPDOWN: Line protocol on Gi1/0/7, changed state to down"),
                    ("SNMP traps", "10.0.0.3 v2c linkDown ifIndex.10107 = 10107"),
                    ("DHCP", "REQUEST from 00:1e:bd:12:34:56 (Switch) → ACK 192.168.1.100"),
                    ("HTTP", "10.0.0.4 GET /images/c9300.bin 200 512.0 MB"),
                    ("NTP", "10.0.0.2 asked for the time"),
                ] {
                    crate::logstore::record(server, line);
                }
                self.servers.open_tab("logs");
            }
            if name == "28-activity" && t.frames == 1 {
                use crate::activity::Kind;
                let a = &self.shared.activity;
                a.add(Kind::Ssh, i18n::trlf("SSH sign-in from {address}", &[("address", &"192.168.1.42")]));
                for (who, file, size) in
                    [("10.0.0.2", "c2960x-universalk9.bin", "26.4 MB"), ("10.0.0.3", "sw2-confg", "8.1 KB")]
                {
                    a.add(
                        Kind::File,
                        i18n::trlf(
                            "{address} downloaded {file} ({size}) over TFTP",
                            &[("address", &who), ("file", &file), ("size", &size)],
                        ),
                    );
                }
                a.first_use(
                    "ntp",
                    "10.0.0.2".parse().unwrap(),
                    i18n::trlf("{address} asked this computer for the time", &[("address", &"10.0.0.2")]),
                );
                self.servers.open_tab("activity");
            }
            if name == "20-map" {
                self.devices.show("map");
            }
            if name == "36-guide" {
                let g = &mut self.shared.settings.guide;
                (g.book, g.topic, g.docked) = (1, 5, false);
                self.console.show(&mut self.shared, "guide");
            }
            if name == "37-guide-dock" {
                let g = &mut self.shared.settings.guide;
                (g.book, g.topic, g.docked, g.side, g.width) = (1, 2, true, crate::guide_view::Side::Right, 480.0);
                self.console.show(&mut self.shared, "dock");
            }
            if name == "38-guide-search" {
                let g = &mut self.shared.settings.guide;
                (g.book, g.topic, g.docked) = (1, 1, false);
                self.shared.guide.demo_search("trunk allowed", true);
                self.console.show(&mut self.shared, "guide");
            }
            if name == "40-guide-window" {
                self.shared.guide.popped = true;
                self.console.show(&mut self.shared, "guide");
            }
            if name == "39-guide-table" {
                let g = &mut self.shared.settings.guide;
                (g.book, g.topic, g.docked) = (7, 3, false);
                self.shared.guide.demo_search("", false);
                self.console.show(&mut self.shared, "guide");
            }
            if let Some(sub) = name.strip_prefix("4").and_then(|n| n.split_once("-adb-")).map(|(_, s)| s) {
                self.shared.settings.guide.docked = false;
                self.console.show(&mut self.shared, &format!("adb-{sub}"));
            }
            if let Some(n) = name.strip_prefix("4").and_then(|n| n.split_once("-art-")).map(|(_, n)| n) {
                self.shared.settings.guide.docked = false;
                self.console.show(&mut self.shared, &format!("art-{n}"));
            }
            if name == "48-dock-drag" {
                let g = &mut self.shared.settings.guide;
                (g.docked, g.side, g.width, g.collapsed, g.maximized) =
                    (true, crate::guide_view::Side::Right, 400.0, false, false);
                self.console.show(&mut self.shared, "dock");
                self.dock_drag = Some(0);
            }
            if matches!(name, "49-split-drag" | "50-zoom-serial" | "51-zoom-remote") {
                self.shared.settings.guide.docked = false;
                self.shared.settings.console_list_width = 250.0;
                self.console.show(
                    &mut self.shared,
                    match name {
                        "49-split-drag" => "split",
                        "50-zoom-serial" => "zoom-serial",
                        _ => "zoom-remote",
                    },
                );
                if name == "49-split-drag" {
                    self.dock_drag_split = true;
                    self.dock_drag = Some(0);
                }
            }
            if name == "57-vpn" {
                self.vpn_page.demo();
            }
            if name == "58-history" {
                self.monitor.open_tab("history");
            }
            if name == "56-update" {
                self.updater.demo_dialog();
            }
            if name == "55-popout" {
                self.shared.settings.guide.docked = false;
                let target = std::env::var("NETMGR_TOUR_TELNET").ok();
                self.console.demo(&mut self.shared, target.as_deref());
                self.console.show(&mut self.shared, "popout");
            }
            if name == "54-menus-hold" {
                self.shared.settings.guide.docked = false;
                self.console.demo(&mut self.shared, None);
            }
            if name == "53-dock-menu" {
                self.shared.settings.guide.docked = false;
                self.console.demo(&mut self.shared, None);
                #[cfg(target_os = "macos")]
                {
                    eprintln!("dock menu: {}", crate::dock::describe());
                    eprintln!("menu bar:\n{}", crate::dock::describe_menu_bar());
                }
            }
            if name == "52-term-find" {
                self.shared.settings.guide.docked = false;
                let target = std::env::var("NETMGR_TOUR_TELNET").ok();
                self.console.demo(&mut self.shared, target.as_deref());
                self.console.show(&mut self.shared, "find-is down");
            }
            if name == "30-push" {
                self.console.show(&mut self.shared, "push");
            }
            if name == "31-backups" {
                self.console.show(&mut self.shared, "backups");
            }
            if name == "32-heatmap" {
                self.wifi_page.show_heatmap(ctx);
            }
            if name == "34-traps-v3" {
                self.servers.show("traps3");
            }
            if name == "35-ip" {
                self.adapters_page.demo_ip(&self.shared);
            }
            if name == "33-snmpv3" {
                self.tools.open(tools::Tab::Snmp, "");
                // NETMGR_TOUR_SNMP3=host,user,auth password,privacy password (SHA, AES).
                if let Ok(v) = std::env::var("NETMGR_TOUR_SNMP3") {
                    let f: Vec<&str> = v.split(',').collect();
                    if f.len() == 4 {
                        self.tools.snmp_demo_v3(ctx, f[0], f[1], f[2], f[3]);
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_opens_at_the_default_size_where_it_fits() {
        let default: egui::Vec2 = DEFAULT_SIZE.into();
        // A MacBook, a Full HD screen at 100 % and at 125 %: the same size.
        for monitor in [egui::vec2(1512.0, 982.0), egui::vec2(1920.0, 1080.0), egui::vec2(1536.0, 864.0)] {
            let fitted = fitted_size(default, monitor);
            assert!(fitted.x == default.x && fitted.y >= 700.0, "{monitor:?} -> {fitted:?}");
        }
        // A small screen shrinks it, but never under the smallest size.
        let small = fitted_size(default, egui::vec2(1366.0, 768.0));
        assert!(small.y < default.y && small.x >= MIN_SIZE[0] && small.y >= MIN_SIZE[1]);
    }

    #[test]
    fn the_window_never_reaches_into_the_taskbar_margin() {
        let fitted: egui::Vec2 = DEFAULT_SIZE.into();
        // A screen only a little taller than the window plus its margin (the taskbar
        // takes a real bite out of a laptop screen): the previous, naive centering put
        // the window's bottom edge under the taskbar here.
        let monitor = egui::vec2(1512.0, 949.0);
        let corner = safe_corner(monitor, fitted);
        assert!(corner.y + fitted.y <= monitor.y - SCREEN_MARGIN, "{corner:?} + {fitted:?} on {monitor:?}");
        assert!(corner.x >= 0.0 && corner.y >= 0.0);
    }
}
