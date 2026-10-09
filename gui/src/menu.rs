//! The menu bar on macOS (App, Edit, View, Go, Network, Window, Help, as
//! Apple's guidelines ask), and the same page shortcuts with Ctrl on Windows
//! and Linux, which have no menu bar.
// Windows and Linux use only the page shortcuts.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use eframe::egui;

use crate::app::Page;
use crate::settings::ThemeChoice;

/// Something the menu bar or a shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Page(Page),
    CheckUpdates,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ZoomIn,
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
}

pub const PROJECT: &str = "https://github.com/itsmrroot/Network-Manager";
const RELEASES: &str = "https://github.com/itsmrroot/Network-Manager/releases";
const ISSUES: &str = "https://github.com/itsmrroot/Network-Manager/issues";

/// The pages in sidebar order, with their ⌘/Ctrl digit.
pub const PAGES: [(Page, &str); 10] = [
    (Page::Overview, "Overview"),
    (Page::Adapters, "Adapters"),
    (Page::Wifi, "Wi-Fi"),
    (Page::Devices, "Devices"),
    (Page::SwitchPort, "Switch port"),
    (Page::Monitor, "Monitor"),
    (Page::Profiles, "Profiles"),
    (Page::Tools, "Tools"),
    (Page::Servers, "Servers"),
    (Page::Console, "Console"),
];

/// Runs an edit command on the focused text field. The menu takes the key
/// press itself, so the field gets the same event it would have.
pub fn edit(ctx: &egui::Context, c: Command) {
    let key = |key: egui::Key, shift: bool| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers { shift, ..egui::Modifiers::COMMAND },
    };
    match c {
        Command::Cut => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut),
        Command::Copy => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy),
        Command::Paste => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
        Command::Undo => ctx.input_mut(|i| i.events.push(key(egui::Key::Z, false))),
        Command::Redo => ctx.input_mut(|i| i.events.push(key(egui::Key::Z, true))),
        Command::SelectAll => ctx.input_mut(|i| i.events.push(key(egui::Key::A, false))),
        _ => {}
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

    use super::*;
    use crate::i18n::{Lang, tr};

    #[derive(Default)]
    pub struct MenuBar {
        menu: Option<Menu>,
        lang: Option<Lang>,
        ids: HashMap<MenuId, Command>,
        events: Arc<Mutex<Vec<MenuId>>>,
        handler: bool,
    }

    fn acc(mods: Modifiers, code: Code) -> Option<Accelerator> {
        Some(Accelerator::new(Some(mods), code))
    }

    impl MenuBar {
        /// The commands chosen since the last call; builds the menu bar on
        /// the first call and again when the language changes.
        pub fn commands(&mut self, ctx: &egui::Context, lang: Lang) -> Vec<Command> {
            if !self.handler {
                self.handler = true;
                let (events, ctx) = (self.events.clone(), ctx.clone());
                MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                    if let Ok(mut v) = events.lock() {
                        v.push(e.id);
                    }
                    ctx.request_repaint();
                }));
            }
            if self.lang != Some(lang) {
                self.lang = Some(lang);
                if let Err(e) = self.build() {
                    log::warn!("menu bar: {e}");
                }
            }
            let ids: Vec<MenuId> = self.events.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
            ids.iter().filter_map(|id| self.ids.get(id).copied()).collect()
        }

        fn item(&mut self, text: &str, c: Command, a: Option<Accelerator>) -> MenuItem {
            let item = MenuItem::new(text, true, a);
            self.ids.insert(item.id().clone(), c);
            item
        }

        fn build(&mut self) -> muda::Result<()> {
            self.ids.clear();
            let cmd = Modifiers::SUPER;
            let cmd_shift = Modifiers::SUPER | Modifiers::SHIFT;
            let sep = || PredefinedMenuItem::separator();
            let name = "Network Manager";

            let about = self.item(tr("About Network Manager"), Command::Page(Page::About), None);
            let updates = self.item(tr("Check for Updates…"), Command::CheckUpdates, None);
            let settings =
                self.item(&format!("{}…", tr("Settings")), Command::Page(Page::Settings), acc(cmd, Code::Comma));
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

            let undo = self.item(tr("Undo"), Command::Undo, acc(cmd, Code::KeyZ));
            let redo = self.item(tr("Redo"), Command::Redo, acc(cmd_shift, Code::KeyZ));
            let cut = self.item(tr("Cut"), Command::Cut, acc(cmd, Code::KeyX));
            let copy = self.item(tr("Copy"), Command::Copy, acc(cmd, Code::KeyC));
            let paste = self.item(tr("Paste"), Command::Paste, acc(cmd, Code::KeyV));
            let all = self.item(tr("Select All"), Command::SelectAll, acc(cmd, Code::KeyA));
            let edit = Submenu::with_items(tr("Edit"), true, &[&undo, &redo, &sep(), &cut, &copy, &paste, &all])?;

            let actual = self.item(tr("Actual Size"), Command::ZoomReset, acc(cmd, Code::Digit0));
            let zin = self.item(tr("Zoom In"), Command::ZoomIn, acc(cmd, Code::Equal));
            let zout = self.item(tr("Zoom Out"), Command::ZoomOut, acc(cmd, Code::Minus));
            let themes = [
                (ThemeChoice::System, tr("System")),
                (ThemeChoice::Light, tr("Light")),
                (ThemeChoice::Dark, tr("Dark")),
                (ThemeChoice::Midnight, tr("Midnight")),
            ];
            let theme_items: Vec<MenuItem> =
                themes.iter().map(|(t, l)| self.item(l, Command::Theme(*t), None)).collect();
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
            let pages: Vec<MenuItem> = PAGES
                .iter()
                .enumerate()
                .map(|(i, (page, label))| {
                    self.item(tr(label), Command::Page(*page), digits.get(i).and_then(|d| acc(cmd, *d)))
                })
                .collect();
            let page_refs: Vec<&dyn muda::IsMenuItem> = pages.iter().map(|i| i as &dyn muda::IsMenuItem).collect();
            let go = Submenu::with_items(tr("Go"), true, &page_refs)?;

            let check = self.item(tr("Check connection"), Command::CheckConnection, acc(cmd_shift, Code::KeyK));
            let renew = self.item(tr("Renew IP"), Command::RenewIp, None);
            let flush = self.item(tr("Flush DNS"), Command::FlushDns, None);
            let speed = self.item(tr("Speed test"), Command::SpeedTest, None);
            let scan = self.item(tr("Scan for devices"), Command::Scan, acc(cmd, Code::KeyR));
            let report = self.item(&format!("{}…", tr("Network report")), Command::Report, None);
            let network = Submenu::with_items(
                tr("Network"),
                true,
                &[&check, &renew, &flush, &speed, &sep(), &scan, &sep(), &report],
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

            let help_item =
                self.item(tr("Network Manager Help"), Command::Page(Page::Help), acc(cmd_shift, Code::Slash));
            let news = self.item(tr("What's new"), Command::Open(RELEASES), None);
            let project = self.item(tr("Project page"), Command::Open(PROJECT), None);
            let issue = self.item(tr("Report a problem"), Command::Open(ISSUES), None);
            let help = Submenu::with_items(tr("Help"), true, &[&help_item, &sep(), &news, &project, &issue])?;

            let menu = Menu::with_items(&[&app, &edit, &view, &go, &network, &window, &help])?;
            menu.init_for_nsapp();
            window.set_as_windows_menu_for_nsapp();
            help.set_as_help_menu_for_nsapp();
            self.menu = Some(menu);
            Ok(())
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    use crate::i18n::Lang;

    /// No menu bar: the same shortcuts with Ctrl.
    #[derive(Default)]
    pub struct MenuBar;

    impl MenuBar {
        pub fn commands(&mut self, ctx: &egui::Context, _lang: Lang) -> Vec<Command> {
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
                if i.consume_key(Modifiers::NONE, Key::F1) {
                    out.push(Command::Page(Page::Help));
                }
            });
            out
        }
    }
}

pub use imp::MenuBar;
