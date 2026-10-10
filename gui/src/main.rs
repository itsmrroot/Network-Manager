//! Desktop app for Network Manager — Powered by Bashar Salmo.

// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod adapters_page;
mod app;
mod backups;
mod capture_page;
mod console;
mod devices;
mod help;
mod i18n;
mod inspect;
mod jobs;
mod lab_servers;
mod lan_extra;
mod menu;
mod monitor;
mod more_tools;
mod notify;
mod overview;
mod planner;
mod profiles;
mod programs;
mod roaming;
mod search;
mod servers;
mod settings;
mod snmp_tool;
mod switchport;
mod term;
mod theme;
mod tools;
mod translations;
mod update;
mod wifi_page;

use eframe::egui;

fn icon() -> egui::IconData {
    image::load_from_memory(include_bytes!("../assets/icon.png"))
        .map(|i| {
            let i = i.to_rgba8();
            egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() }
        })
        .unwrap_or_default()
}

fn main() -> eframe::Result {
    // Changed only by the development tour.
    #[allow(unused_mut)]
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Network Manager")
        .with_app_id("netmgr")
        .with_inner_size(app::DEFAULT_SIZE)
        .with_min_inner_size(app::MIN_SIZE)
        .with_icon(icon());
    // Development aid: the screenshot tour can use a given window size ("1240x800").
    #[allow(unused_mut)]
    let mut persist_window = true;
    #[cfg(debug_assertions)]
    if let Some((w, h)) = std::env::var("NETMGR_TOUR_SIZE").ok().and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
    }) {
        viewport = viewport.with_inner_size([w, h]).with_min_inner_size([w, h]);
        persist_window = false;
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window,
        renderer: eframe::Renderer::Wgpu,
        centered: true,
        // The app's own menu bar (menu.rs) replaces winit's default one,
        // which would otherwise be put back after launch.
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|b| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            b.with_default_menu(false);
        })),
        ..Default::default()
    };
    eframe::run_native("Network Manager", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
