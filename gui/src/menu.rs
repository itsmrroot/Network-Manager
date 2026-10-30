//! The menu bar on macOS (App, Edit, View, Go, Network, Window, Help, as
//! Apple's guidelines ask). On Windows and Linux the app draws its own menu
//! bar (File, Edit, View, Go, Network, Console, Help, as VS Code has it) with
//! the same commands, and the shortcuts with Ctrl.
// Windows and Linux do not use the macOS-only parts (icons, Dock).
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use eframe::egui;
use egui_phosphor::regular as icon;

use crate::app::Page;
use crate::settings::ThemeChoice;

/// Something the menu bar or a shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Page(Page),
    Search,
    CheckUpdates,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ZoomIn,
    /// Hide or show the list of pages on the left.
    ToggleSidebar,
    ZoomOut,
    ZoomReset,
    Theme(ThemeChoice),
    CheckConnection,
    RenewIp,
    FlushDns,
    SpeedTest,
    Scan,
    Report,
    Open(&'static str),
    /// The Adapters page with the IP settings of the main adapter open.
    IpSettings,
    /// The SSH and Telnet page, with the address field ready.
    NewConnection,
    /// A saved session, by its place in the list.
    OpenSession(usize),
    /// A tab of the Console: serial, remote, adb, guide, backups, push.
    ConsoleTab(&'static str),
    /// The terminal that is shown, in a window of its own.
    PopOut,
    /// The window over the whole screen, or back. Windows and Linux: their menu bar and F11.
    /// (macOS has them in its own menus, which the system handles.)
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    FullScreen,
    /// Closes the window (and with it the app). Windows and Linux: the File menu.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Quit,
}

/// The icons the menus use (to check that the font has all of them).
// Only the tests read it (the translation check stops at the first test marker).
#[allow(dead_code)]
pub const ICONS: &[&str] = &[
    icon::INFO,
    icon::DOWNLOAD_SIMPLE,
    icon::GEAR_SIX,
    icon::ARROW_COUNTER_CLOCKWISE,
    icon::ARROW_CLOCKWISE,
    icon::SCISSORS,
    icon::COPY,
    icon::CLIPBOARD_TEXT,
    icon::SELECTION_ALL,
    icon::MAGNIFYING_GLASS,
    icon::MAGNIFYING_GLASS_PLUS,
    icon::MAGNIFYING_GLASS_MINUS,
    icon::SIDEBAR_SIMPLE,
    icon::DESKTOP,
    icon::SUN,
    icon::MOON,
    icon::MOON_STARS,
    icon::GAUGE,
    icon::PLUGS_CONNECTED,
    icon::WIFI_HIGH,
    icon::DEVICES,
    icon::TREE_STRUCTURE,
    icon::HEARTBEAT,
    icon::STACK,
    icon::TOOLBOX,
    icon::HARD_DRIVES,
    icon::TERMINAL_WINDOW,
    icon::QUESTION,
    icon::CHECK_CIRCLE,
    icon::ARROWS_CLOCKWISE,
    icon::BROOM,
    icon::FILE_TEXT,
    icon::PLUS,
    icon::USB,
    icon::CLOUD_ARROW_DOWN,
    icon::PAPER_PLANE_TILT,
    icon::BOOK_OPEN,
    icon::DEVICE_MOBILE,
    icon::ARROW_SQUARE_OUT,
    icon::SPARKLE,
    icon::GITHUB_LOGO,
    icon::BUG,
];

pub const PROJECT: &str = "https://github.com/itsmrroot/Network-Manager-releases";
const RELEASES: &str = "https://github.com/itsmrroot/Network-Manager-releases/releases";
const ISSUES: &str = "https://github.com/itsmrroot/Network-Manager-releases/issues";

/// The pages in sidebar order, with their ⌘/Ctrl digit.
pub const PAGES: [(Page, &str); 11] = [
    (Page::Overview, "Overview"),
    (Page::Adapters, "Adapters"),
    (Page::Wifi, "Wi-Fi"),
    (Page::Vpn, "VPN"),
    (Page::Devices, "Devices"),
    (Page::SwitchPort, "Switch port"),
    (Page::Monitor, "Monitor"),
    (Page::Profiles, "Profiles"),
    (Page::Tools, "Tools"),
    (Page::Servers, "Servers"),
    (Page::Console, "Console"),
];

/// The icon of a page, as in the sidebar.
pub fn page_glyph(page: Page) -> &'static str {
    match page {
        Page::Overview => icon::GAUGE,
        Page::Adapters => icon::PLUGS_CONNECTED,
        Page::Wifi => icon::WIFI_HIGH,
        Page::Vpn => icon::SHIELD_CHECK,
        Page::Devices => icon::DEVICES,
        Page::SwitchPort => icon::TREE_STRUCTURE,
        Page::Monitor => icon::HEARTBEAT,
        Page::Profiles => icon::STACK,
        Page::Tools => icon::TOOLBOX,
        Page::Servers => icon::HARD_DRIVES,
        Page::Console => icon::TERMINAL_WINDOW,
        Page::Settings => icon::GEAR_SIX,
        Page::Help => icon::QUESTION,
        Page::About => icon::INFO,
    }
}

/// A saved session as the menus need it: its name and its icon.
pub type SessionEntry = (String, String);

/// Runs an edit command on the focused text field (of the window `window`). The menu takes the key
/// press itself, so the field gets the same event it would have.
pub fn edit(ctx: &egui::Context, c: Command, window: egui::ViewportId) {
    let key = |key: egui::Key, shift: bool| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers { shift, ..egui::Modifiers::COMMAND },
    };
    match c {
        // To the window that has the keyboard: a terminal in a window of its own
        // is not the main window.
        Command::Cut => ctx.send_viewport_cmd_to(window, egui::ViewportCommand::RequestCut),
        Command::Copy => ctx.send_viewport_cmd_to(window, egui::ViewportCommand::RequestCopy),
        Command::Paste => ctx.send_viewport_cmd_to(window, egui::ViewportCommand::RequestPaste),
        // Key presses, for the field that has the keyboard: written into the input of the
        // window they are for (the window draws them next frame, see `take_edits`).
        Command::Undo => queue_edit(ctx, window, key(egui::Key::Z, false)),
        Command::Redo => queue_edit(ctx, window, key(egui::Key::Z, true)),
        Command::SelectAll => queue_edit(ctx, window, key(egui::Key::A, false)),
        _ => {}
    }
}

/// Edit key presses that wait for their window to draw.
static EDITS: std::sync::Mutex<Vec<(egui::ViewportId, egui::Event)>> = std::sync::Mutex::new(Vec::new());

fn queue_edit(ctx: &egui::Context, window: egui::ViewportId, event: egui::Event) {
    if let Ok(mut v) = EDITS.lock() {
        v.push((window, event));
    }
    ctx.request_repaint_of(window);
}

/// Called at the start of a window's frame: the edit key presses meant for it
/// become its input. (The main window does it too.)
pub fn take_edits(ctx: &egui::Context) {
    let me = ctx.viewport_id();
    let mine: Vec<egui::Event> = match EDITS.lock() {
        Ok(mut v) => {
            let (mine, rest): (Vec<_>, Vec<_>) = v.drain(..).partition(|(w, _)| *w == me);
            *v = rest;
            mine.into_iter().map(|(_, e)| e).collect()
        }
        Err(_) => return,
    };
    if !mine.is_empty() {
        ctx.input_mut(|i| i.events.extend(mine));
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{ContextMenu, IconMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

    use super::*;
    use crate::i18n::{Lang, tr};

    #[derive(Default)]
    pub struct MenuBar {
        menu: Option<Menu>,
        /// The menu of the Dock icon (right click).
        dock: Option<Menu>,
        lang: Option<Lang>,
        sessions: Vec<SessionEntry>,
        ids: HashMap<MenuId, Command>,
        events: Arc<Mutex<Vec<MenuId>>>,
        handler: bool,
        icons: HashMap<String, Option<muda::Icon>>,
    }

    impl Drop for MenuBar {
        fn drop(&mut self) {
            // The Dock must not be left with a menu that is gone.
            crate::dock::set(std::ptr::null_mut());
        }
    }

    fn acc(mods: Modifiers, code: Code) -> Option<Accelerator> {
        Some(Accelerator::new(Some(mods), code))
    }

    impl MenuBar {
        /// The commands chosen since the last call; builds the menu bar on
        /// the first call, and again when the language or the saved sessions
        /// change.
        pub fn commands(&mut self, ctx: &egui::Context, lang: Lang, sessions: &[SessionEntry]) -> Vec<Command> {
            if !self.handler {
                self.handler = true;
                let (events, ctx) = (self.events.clone(), ctx.clone());
                MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                    if let Ok(mut v) = events.lock() {
                        v.push(e.id);
                    }
                    crate::dock::wake();
                    ctx.request_repaint();
                }));
            }
            if self.lang != Some(lang) || self.sessions != sessions {
                self.lang = Some(lang);
                self.sessions = sessions.to_vec();
                if let Err(e) = self.build() {
                    log::warn!("menu bar: {e}");
                    // No Dock menu whose items nothing answers to.
                    self.dock = None;
                    crate::dock::set(std::ptr::null_mut());
                }
            }
            let ids: Vec<MenuId> = self.events.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
            ids.iter().filter_map(|id| self.ids.get(id).copied()).collect()
        }

        /// The picture of an icon, drawn once.
        fn picture(&mut self, glyph: &str) -> Option<muda::Icon> {
            self.icons
                .entry(glyph.to_string())
                .or_insert_with(|| {
                    // Black: the menu draws it as a template, in the colour that suits it.
                    let b = crate::menu_icon::render(glyph, 36, [0, 0, 0])?;
                    muda::Icon::from_rgba(b.rgba, b.side, b.side).ok()
                })
                .clone()
        }

        fn item(&mut self, glyph: &str, text: &str, c: Command, a: Option<Accelerator>) -> IconMenuItem {
            let picture = self.picture(glyph);
            let item = IconMenuItem::new(text, true, picture, a);
            self.ids.insert(item.id().clone(), c);
            item
        }

        /// The saved sessions as menu items: their pictures and names.
        fn session_items(&mut self) -> Vec<IconMenuItem> {
            let sessions = self.sessions.clone();
            sessions
                .iter()
                .enumerate()
                .map(|(i, (name, key))| {
                    self.item(crate::console::session_glyph(key), name, Command::OpenSession(i), None)
                })
                .collect()
        }

        /// A submenu of the saved sessions (or a note that there are none).
        fn saved_sessions(&mut self) -> muda::Result<Submenu> {
            let items = self.session_items();
            let menu = Submenu::new(tr("Saved sessions"), true);
            if items.is_empty() {
                menu.append(&MenuItem::new(tr("No saved sessions yet"), false, None))?;
            }
            for item in &items {
                menu.append(item)?;
            }
            Ok(menu)
        }

        /// An item for the Dock's menu, which shows text only: a symbol first.
        fn dock_item(&mut self, symbol: &str, text: &str, c: Command) -> MenuItem {
            let item = MenuItem::new(format!("{symbol}  {text}"), true, None);
            self.ids.insert(item.id().clone(), c);
            item
        }

        /// The saved sessions for the Dock's menu: the symbol of each one's icon.
        fn dock_sessions(&mut self) -> muda::Result<Submenu> {
            let menu = Submenu::new(format!("🔖  {}", tr("Saved sessions")), true);
            if self.sessions.is_empty() {
                menu.append(&MenuItem::new(tr("No saved sessions yet"), false, None))?;
            }
            let sessions = self.sessions.clone();
            for (i, (name, key)) in sessions.iter().enumerate() {
                let item = self.dock_item(crate::console::session_emoji(key), name, Command::OpenSession(i));
                menu.append(&item)?;
            }
            Ok(menu)
        }

        fn build(&mut self) -> muda::Result<()> {
            self.ids.clear();
            let cmd = Modifiers::SUPER;
            let cmd_shift = Modifiers::SUPER | Modifiers::SHIFT;
            let sep = || PredefinedMenuItem::separator();
            let name = "Network Manager";

            let about = self.item(icon::INFO, tr("About Network Manager"), Command::Page(Page::About), None);
            let updates = self.item(icon::DOWNLOAD_SIMPLE, tr("Check for Updates…"), Command::CheckUpdates, None);
            let settings = self.item(
                icon::GEAR_SIX,
                &format!("{}…", tr("Settings")),
                Command::Page(Page::Settings),
                acc(cmd, Code::Comma),
            );
            let app = Submenu::with_items(
                name,
                true,
                &[
                    &about,
                    &updates,
                    &sep(),
                    &settings,
                    &sep(),
                    &PredefinedMenuItem::services(Some(tr("Services"))),
                    &sep(),
                    &PredefinedMenuItem::hide(Some(tr("Hide Network Manager"))),
                    &PredefinedMenuItem::hide_others(Some(tr("Hide Others"))),
                    &PredefinedMenuItem::show_all(Some(tr("Show All"))),
                    &sep(),
                    &PredefinedMenuItem::quit(Some(tr("Quit Network Manager"))),
                ],
            )?;

            let undo = self.item(icon::ARROW_COUNTER_CLOCKWISE, tr("Undo"), Command::Undo, acc(cmd, Code::KeyZ));
            let redo = self.item(icon::ARROW_CLOCKWISE, tr("Redo"), Command::Redo, acc(cmd_shift, Code::KeyZ));
            let cut = self.item(icon::SCISSORS, tr("Cut"), Command::Cut, acc(cmd, Code::KeyX));
            let copy = self.item(icon::COPY, tr("Copy"), Command::Copy, acc(cmd, Code::KeyC));
            let paste = self.item(icon::CLIPBOARD_TEXT, tr("Paste"), Command::Paste, acc(cmd, Code::KeyV));
            let all = self.item(icon::SELECTION_ALL, tr("Select All"), Command::SelectAll, acc(cmd, Code::KeyA));
            let edit = Submenu::with_items(tr("Edit"), true, &[&undo, &redo, &sep(), &cut, &copy, &paste, &all])?;

            let actual =
                self.item(icon::MAGNIFYING_GLASS, tr("Actual Size"), Command::ZoomReset, acc(cmd, Code::Digit0));
            let zin = self.item(icon::MAGNIFYING_GLASS_PLUS, tr("Zoom In"), Command::ZoomIn, acc(cmd, Code::Equal));
            let zout = self.item(icon::MAGNIFYING_GLASS_MINUS, tr("Zoom Out"), Command::ZoomOut, acc(cmd, Code::Minus));
            let sidebar = self.item(
                icon::SIDEBAR_SIMPLE,
                tr("Hide or show sidebar"),
                Command::ToggleSidebar,
                acc(cmd, Code::Backslash),
            );
            let themes = [
                (ThemeChoice::System, icon::DESKTOP, tr("System")),
                (ThemeChoice::Light, icon::SUN, tr("Light")),
                (ThemeChoice::Dark, icon::MOON, tr("Dark")),
                (ThemeChoice::Midnight, icon::MOON_STARS, tr("Midnight")),
            ];
            let theme_items: Vec<IconMenuItem> =
                themes.iter().map(|(t, g, l)| self.item(g, l, Command::Theme(*t), None)).collect();
            let theme_refs: Vec<&dyn muda::IsMenuItem> =
                theme_items.iter().map(|i| i as &dyn muda::IsMenuItem).collect();
            let appearance = Submenu::with_items(tr("Theme"), true, &theme_refs)?;
            let view = Submenu::with_items(
                tr("View"),
                true,
                &[
                    &actual,
                    &zin,
                    &zout,
                    &sep(),
                    &sidebar,
                    &appearance,
                    &sep(),
                    &PredefinedMenuItem::fullscreen(Some(tr("Enter Full Screen"))),
                ],
            )?;

            let digits = [
                Code::Digit1,
                Code::Digit2,
                Code::Digit3,
                Code::Digit4,
                Code::Digit5,
                Code::Digit6,
                Code::Digit7,
                Code::Digit8,
                Code::Digit9,
            ];
            let pages: Vec<IconMenuItem> = PAGES
                .iter()
                .enumerate()
                .map(|(i, (page, label))| {
                    self.item(
                        page_glyph(*page),
                        tr(label),
                        Command::Page(*page),
                        digits.get(i).and_then(|d| acc(cmd, *d)),
                    )
                })
                .collect();
            let page_refs: Vec<&dyn muda::IsMenuItem> = pages.iter().map(|i| i as &dyn muda::IsMenuItem).collect();
            let search =
                self.item(icon::MAGNIFYING_GLASS, &format!("{}…", tr("Search")), Command::Search, acc(cmd, Code::KeyK));
            let mut go_refs: Vec<&dyn muda::IsMenuItem> = vec![&search];
            let go_sep = sep();
            go_refs.push(&go_sep);
            go_refs.extend(page_refs);
            let go = Submenu::with_items(tr("Go"), true, &go_refs)?;

            let check = self.item(
                icon::CHECK_CIRCLE,
                tr("Check connection"),
                Command::CheckConnection,
                acc(cmd_shift, Code::KeyK),
            );
            let renew = self.item(icon::ARROWS_CLOCKWISE, tr("Renew IP"), Command::RenewIp, None);
            let ip = self.item(
                icon::PLUGS_CONNECTED,
                &format!("{}…", tr("Change IP settings")),
                Command::IpSettings,
                acc(cmd_shift, Code::KeyI),
            );
            let flush = self.item(icon::BROOM, tr("Flush DNS"), Command::FlushDns, None);
            let speed = self.item(icon::GAUGE, tr("Speed test"), Command::SpeedTest, None);
            let scan = self.item(icon::DEVICES, tr("Scan for devices"), Command::Scan, acc(cmd, Code::KeyR));
            let report = self.item(icon::FILE_TEXT, &format!("{}…", tr("Network report")), Command::Report, None);
            let network = Submenu::with_items(
                tr("Network"),
                true,
                &[&check, &renew, &ip, &flush, &speed, &sep(), &scan, &sep(), &report],
            )?;

            // The Console: new and saved connections, and its tabs.
            let new = self.item(
                icon::PLUS,
                &format!("{}…", tr("New connection")),
                Command::NewConnection,
                acc(cmd, Code::KeyN),
            );
            let saved = self.saved_sessions()?;
            let serial = self.item(icon::USB, tr("Serial console"), Command::ConsoleTab("serial"), None);
            let remote = self.item(icon::TERMINAL_WINDOW, tr("SSH and Telnet"), Command::ConsoleTab("remote"), None);
            let backups = self.item(icon::CLOUD_ARROW_DOWN, tr("Backups"), Command::ConsoleTab("backups"), None);
            let push = self.item(icon::PAPER_PLANE_TILT, tr("Push configuration"), Command::ConsoleTab("push"), None);
            let guide = self.item(icon::BOOK_OPEN, tr("CLI guide"), Command::ConsoleTab("guide"), None);
            let adb = self.item(icon::DEVICE_MOBILE, tr("Android (ADB)"), Command::ConsoleTab("adb"), None);
            let pop = self.item(
                icon::ARROW_SQUARE_OUT,
                tr("Open terminal in window"),
                Command::PopOut,
                acc(cmd_shift, Code::KeyO),
            );
            let console = Submenu::with_items(
                tr("Console"),
                true,
                &[&new, &saved, &sep(), &serial, &remote, &backups, &push, &guide, &adb, &sep(), &pop],
            )?;

            let window = Submenu::with_items(
                tr("Window"),
                true,
                &[
                    &PredefinedMenuItem::minimize(Some(tr("Minimize"))),
                    &PredefinedMenuItem::maximize(Some(tr("Zoom"))),
                    &sep(),
                    &PredefinedMenuItem::bring_all_to_front(Some(tr("Bring All to Front"))),
                ],
            )?;

            let help_item = self.item(
                icon::QUESTION,
                tr("Network Manager Help"),
                Command::Page(Page::Help),
                acc(cmd_shift, Code::Slash),
            );
            let news = self.item(icon::SPARKLE, tr("What's new"), Command::Open(RELEASES), None);
            let project = self.item(icon::GITHUB_LOGO, tr("Project page"), Command::Open(PROJECT), None);
            let issue = self.item(icon::BUG, tr("Report a problem"), Command::Open(ISSUES), None);
            let help = Submenu::with_items(tr("Help"), true, &[&help_item, &sep(), &news, &project, &issue])?;

            let menu = Menu::with_items(&[&app, &edit, &view, &go, &network, &console, &window, &help])?;
            menu.init_for_nsapp();
            crate::dock::template_icons(menu.ns_menu());
            window.set_as_windows_menu_for_nsapp();
            help.set_as_help_menu_for_nsapp();
            self.menu = Some(menu);

            // The Dock's menu: the quick actions, and the connections. It shows
            // no pictures, so each item has a symbol in its title.
            let d_new = self.dock_item("➕", &format!("{}…", tr("New connection")), Command::NewConnection);
            let d_saved = self.dock_sessions()?;
            let d_serial = self.dock_item("📟", tr("Serial console"), Command::ConsoleTab("serial"));
            let d_check = self.dock_item("✅", tr("Check connection"), Command::CheckConnection);
            let d_renew = self.dock_item("🔄", tr("Renew IP"), Command::RenewIp);
            let d_ip = self.dock_item("🔌", &format!("{}…", tr("Change IP settings")), Command::IpSettings);
            let d_scan = self.dock_item("🔎", tr("Scan for devices"), Command::Scan);
            let dock = Menu::with_items(&[&d_check, &d_renew, &d_ip, &d_scan, &sep(), &d_new, &d_saved, &d_serial])?;
            // The Dock is told of the new menu before the old one goes away.
            crate::dock::set(dock.ns_menu());
            self.dock = Some(dock);
            Ok(())
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    use crate::i18n::{Lang, tr};

    /// The shortcuts with Ctrl (the menus themselves are drawn by [`bar`]).
    #[derive(Default)]
    pub struct MenuBar;

    /// What the menu bar needs to know to draw itself.
    pub struct BarState<'a> {
        pub sessions: &'a [SessionEntry],
        pub theme: ThemeChoice,
        /// The colour of the names on the bar (the header of the Midnight theme is dark).
        pub text: egui::Color32,
        /// Room for the bar, in points: the menus that do not fit go under "···".
        pub room: f32,
    }

    /// A menu entry: its text, the shortcut written at its right (if it has one),
    /// and what it does.
    fn item(ui: &mut egui::Ui, out: &mut Vec<Command>, text: &str, shortcut: &str, c: Command) {
        let mut button = egui::Button::new(text);
        if !shortcut.is_empty() {
            button = button.shortcut_text(shortcut);
        }
        if ui.add(button).clicked() {
            out.push(c);
        }
    }

    fn file(ui: &mut egui::Ui, out: &mut Vec<Command>, st: &BarState) {
        item(ui, out, &format!("{}…", tr("New connection")), "", Command::NewConnection);
        ui.menu_button(tr("Saved sessions"), |ui| {
            if st.sessions.is_empty() {
                ui.add_enabled(false, egui::Button::new(tr("No saved sessions yet")));
            }
            for (i, (name, key)) in st.sessions.iter().enumerate() {
                item(ui, out, &format!("{}  {name}", crate::console::session_glyph(key)), "", Command::OpenSession(i));
            }
        });
        ui.separator();
        item(ui, out, &format!("{}…", tr("Network report")), "", Command::Report);
        ui.separator();
        item(ui, out, tr("Settings"), "Ctrl+,", Command::Page(Page::Settings));
        ui.separator();
        item(ui, out, tr("Exit"), "Alt+F4", Command::Quit);
    }

    fn edit(ui: &mut egui::Ui, out: &mut Vec<Command>, _st: &BarState) {
        item(ui, out, tr("Undo"), "Ctrl+Z", Command::Undo);
        item(ui, out, tr("Redo"), "Ctrl+Shift+Z", Command::Redo);
        ui.separator();
        item(ui, out, tr("Cut"), "Ctrl+X", Command::Cut);
        item(ui, out, tr("Copy"), "Ctrl+C", Command::Copy);
        item(ui, out, tr("Paste"), "Ctrl+V", Command::Paste);
        item(ui, out, tr("Select All"), "Ctrl+A", Command::SelectAll);
    }

    fn view(ui: &mut egui::Ui, out: &mut Vec<Command>, st: &BarState) {
        item(ui, out, &format!("{}…", tr("Search")), "Ctrl+K", Command::Search);
        ui.separator();
        item(ui, out, tr("Hide or show sidebar"), "Ctrl+\\", Command::ToggleSidebar);
        ui.menu_button(tr("Theme"), |ui| {
            for (t, label) in [
                (ThemeChoice::System, tr("System")),
                (ThemeChoice::Light, tr("Light")),
                (ThemeChoice::Dark, tr("Dark")),
                (ThemeChoice::Midnight, tr("Midnight")),
            ] {
                if ui.add(egui::Button::selectable(st.theme == t, label)).clicked() {
                    out.push(Command::Theme(t));
                }
            }
        });
        ui.separator();
        item(ui, out, tr("Zoom In"), "Ctrl++", Command::ZoomIn);
        item(ui, out, tr("Zoom Out"), "Ctrl+-", Command::ZoomOut);
        item(ui, out, tr("Actual Size"), "Ctrl+0", Command::ZoomReset);
        ui.separator();
        item(ui, out, tr("Enter Full Screen"), "F11", Command::FullScreen);
    }

    fn go(ui: &mut egui::Ui, out: &mut Vec<Command>, _st: &BarState) {
        for (n, (page, label)) in PAGES.iter().enumerate() {
            let key = if n < 9 { format!("Ctrl+{}", n + 1) } else { String::new() };
            item(ui, out, tr(label), &key, Command::Page(*page));
        }
        ui.separator();
        item(ui, out, tr("Settings"), "Ctrl+,", Command::Page(Page::Settings));
        item(ui, out, tr("Help"), "F1", Command::Page(Page::Help));
        item(ui, out, tr("About Network Manager"), "", Command::Page(Page::About));
    }

    fn network(ui: &mut egui::Ui, out: &mut Vec<Command>, _st: &BarState) {
        item(ui, out, tr("Check connection"), "", Command::CheckConnection);
        item(ui, out, tr("Renew IP"), "", Command::RenewIp);
        item(ui, out, &format!("{}…", tr("Change IP settings")), "", Command::IpSettings);
        item(ui, out, tr("Flush DNS"), "", Command::FlushDns);
        item(ui, out, tr("Speed test"), "", Command::SpeedTest);
        ui.separator();
        item(ui, out, tr("Scan for devices"), "", Command::Scan);
        ui.separator();
        item(ui, out, &format!("{}…", tr("Network report")), "", Command::Report);
    }

    fn console(ui: &mut egui::Ui, out: &mut Vec<Command>, st: &BarState) {
        item(ui, out, &format!("{}…", tr("New connection")), "", Command::NewConnection);
        ui.menu_button(tr("Saved sessions"), |ui| {
            if st.sessions.is_empty() {
                ui.add_enabled(false, egui::Button::new(tr("No saved sessions yet")));
            }
            for (i, (name, key)) in st.sessions.iter().enumerate() {
                item(ui, out, &format!("{}  {name}", crate::console::session_glyph(key)), "", Command::OpenSession(i));
            }
        });
        ui.separator();
        item(ui, out, tr("Serial console"), "", Command::ConsoleTab("serial"));
        item(ui, out, tr("SSH and Telnet"), "", Command::ConsoleTab("remote"));
        item(ui, out, tr("Backups"), "", Command::ConsoleTab("backups"));
        item(ui, out, tr("Push configuration"), "", Command::ConsoleTab("push"));
        item(ui, out, tr("CLI guide"), "", Command::ConsoleTab("guide"));
        item(ui, out, tr("Android (ADB)"), "", Command::ConsoleTab("adb"));
        ui.separator();
        item(ui, out, tr("Open terminal in window"), "", Command::PopOut);
    }

    fn help(ui: &mut egui::Ui, out: &mut Vec<Command>, _st: &BarState) {
        item(ui, out, tr("Network Manager Help"), "F1", Command::Page(Page::Help));
        ui.separator();
        item(ui, out, tr("What's new"), "", Command::Open(RELEASES));
        item(ui, out, tr("Project page"), "", Command::Open(PROJECT));
        item(ui, out, tr("Report a problem"), "", Command::Open(ISSUES));
        ui.separator();
        item(ui, out, tr("Check for Updates…"), "", Command::CheckUpdates);
        item(ui, out, tr("About Network Manager"), "", Command::Page(Page::About));
    }

    type Content = fn(&mut egui::Ui, &mut Vec<Command>, &BarState);

    /// The menu bar, as VS Code has it: File, Edit, View, Go, Network, Console and
    /// Help in a row; the ones that do not fit are under "···". Returns what was chosen.
    pub fn bar(ui: &mut egui::Ui, st: &BarState) -> Vec<Command> {
        let menus: [(&str, Content); 7] = [
            (tr("File"), file),
            (tr("Edit"), edit),
            (tr("View"), view),
            (tr("Go"), go),
            (tr("Network"), network),
            (tr("Console"), console),
            (tr("Help"), help),
        ];
        // How many names fit: each takes its text, its button padding and the space between.
        let font = egui::TextStyle::Button.resolve(ui.style());
        let pad = 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x;
        let widths: Vec<f32> = menus
            .iter()
            .map(|(name, _)| {
                ui.fonts_mut(|f| f.layout_no_wrap((*name).to_owned(), font.clone(), st.text).size().x) + pad
            })
            .collect();
        let more = 30.0;
        let mut shown = menus.len();
        while shown > 0 && widths[..shown].iter().sum::<f32>() + if shown < menus.len() { more } else { 0.0 } > st.room
        {
            shown -= 1;
        }
        let mut out = Vec::new();
        egui::MenuBar::new().ui(ui, |ui| {
            for (name, content) in &menus[..shown] {
                ui.menu_button(egui::RichText::new(*name).color(st.text), |ui| content(ui, &mut out, st));
            }
            if shown < menus.len() {
                let r = ui.menu_button(egui::RichText::new("···").color(st.text), |ui| {
                    for (name, content) in &menus[shown..] {
                        ui.menu_button(*name, |ui| content(ui, &mut out, st));
                    }
                });
                r.response.on_hover_text(tr("More"));
            }
        });
        out
    }

    impl MenuBar {
        pub fn commands(&mut self, ctx: &egui::Context, _lang: Lang, _sessions: &[SessionEntry]) -> Vec<Command> {
            use egui::{Key, KeyboardShortcut, Modifiers};
            let digits =
                [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            let mut out = Vec::new();
            ctx.input_mut(|i| {
                for (n, key) in digits.iter().enumerate() {
                    if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, *key)) {
                        out.push(Command::Page(PAGES[n].0));
                    }
                }
                if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma)) {
                    out.push(Command::Page(Page::Settings));
                }
                if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Backslash)) {
                    out.push(Command::ToggleSidebar);
                }
                if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::K)) {
                    out.push(Command::Search);
                }
                if i.consume_key(Modifiers::NONE, Key::F1) {
                    out.push(Command::Page(Page::Help));
                }
                if i.consume_key(Modifiers::NONE, Key::F11) {
                    out.push(Command::FullScreen);
                }
            });
            out
        }
    }
}

pub use imp::MenuBar;
#[cfg(not(target_os = "macos"))]
pub use imp::{BarState, bar};
