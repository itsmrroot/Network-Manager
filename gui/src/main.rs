//! Desktop app for Network Manager — Powered by Bashar Salmo.

// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod adapters_page;
mod app;
mod console;
mod devices;
mod help;
mod i18n;
mod inspect;
mod jobs;
mod monitor;
mod overview;
mod planner;
mod profiles;
mod servers;
mod settings;
mod switchport;
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
        ..Default::default()
    };
    eframe::run_native("Network Manager", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
