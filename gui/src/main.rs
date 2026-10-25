//! Desktop app for Network Manager — Powered by Bashar Salmo.

// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod activity;
mod adapters_page;
mod adb_page;
mod app;
mod appicon;
mod backups;
mod capture_page;
mod console;
mod devices;
#[cfg(target_os = "macos")]
mod dock;
mod guide;
mod guide_art;
mod guide_view;
mod heatmap;
mod help;
mod i18n;
mod inspect;
mod jobs;
mod lab_servers;
mod lan_extra;
mod logstore;
mod menu;
mod menu_icon;
mod monitor;
mod more_tools;
mod notify;
mod overview;
mod planner;
mod profiles;
mod programs;
mod push;
mod roaming;
mod search;
mod servers;
mod settings;
mod snmp_tool;
mod switchport;
mod term;
mod term_extras;
mod theme;
mod tools;
mod translations;
mod update;
mod wifi_ip;
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

/// Where start-up failures and crashes are written, to send with a report.
fn problem_log() -> std::path::PathBuf {
    let dir = netmgr::profiles::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir.join("problems.log")
}

/// Adds a dated line to the problem log (kept small).
fn note_problem(what: &str) {
    use std::io::Write;
    let path = problem_log();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            f,
            "{} v{} {}: {what}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        );
    }
}

/// Messages of the app and its libraries, when started with
/// RUST_LOG=error|warn|info|debug|trace (for troubleshooting): on the
/// terminal and in debug.log next to problems.log (Windows shows no
/// terminal for the app).
struct Logger(log::LevelFilter, std::sync::Mutex<Option<std::fs::File>>);

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.0
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            let line = format!(
                "{} {:<5} {}: {}",
                chrono::Local::now().format("%H:%M:%S%.3f"),
                r.level(),
                r.target(),
                r.args()
            );
            eprintln!("{line}");
            if let Ok(mut f) = self.1.lock()
                && let Some(f) = f.as_mut()
            {
                use std::io::Write;
                let _ = writeln!(f, "{line}");
            }
        }
    }
    fn flush(&self) {}
}

fn main() -> eframe::Result {
    // A hang-up signal ends a program without a word or a crash report. A
    // desktop app has no use for it, and the programs it starts in a
    // pseudo-terminal (ssh, adb) are sent one when they are closed.
    #[cfg(unix)]
    // SAFETY: setting a signal to be ignored, before any thread exists.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    if let Some(level) = std::env::var("RUST_LOG").ok().and_then(|l| l.parse::<log::LevelFilter>().ok()) {
        let file = std::fs::File::create(problem_log().with_file_name("debug.log")).ok();
        let _ = log::set_boxed_logger(Box::new(Logger(level, std::sync::Mutex::new(file))))
            .map(|()| log::set_max_level(level));
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        note_problem(&format!("crash: {info}"));
        default_hook(info);
    }));
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
    let glow = use_glow();
    let options = eframe::NativeOptions {
        viewport,
        persist_window,
        renderer: if glow { eframe::Renderer::Glow } else { eframe::Renderer::Wgpu },
        wgpu_options: wgpu_options(),
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
    let result = eframe::run_native("Network Manager", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))));
    if let Err(e) = &result {
        note_problem(&format!("{} could not start: {e}", if glow { "OpenGL" } else { "wgpu" }));
    }
    if result.is_err() && !glow {
        // The graphics driver refused wgpu: start again with OpenGL, and
        // keep using it (a second window loop is not allowed in one process).
        let _ = std::fs::write(glow_marker(), "wgpu could not start\n");
        if let Ok(exe) = std::env::current_exe()
            && std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn().is_ok()
        {
            return Ok(());
        }
    }
    result
}

/// Where the app notes that this computer needs OpenGL.
fn glow_marker() -> std::path::PathBuf {
    let dir = netmgr::profiles::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir.join("use-opengl")
}

/// OpenGL instead of wgpu: after wgpu failed here, or when asked
/// (NETMGR_RENDERER=glow).
fn use_glow() -> bool {
    match std::env::var("NETMGR_RENDERER").as_deref() {
        Ok("glow") => true,
        Ok("wgpu") => false,
        _ => glow_marker().exists(),
    }
}

/// The built-in graphics chip rather than waking a separate one (laptops
/// with two often fail or start slowly), and on Windows DirectX 12 rather
/// than Vulkan, whose AMD and Intel drivers are the least reliable.
fn wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
    use eframe::wgpu;
    let mut config = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.power_preference = wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower);
        if cfg!(windows) && wgpu::Backends::from_env().is_none() {
            setup.instance_descriptor.backends = wgpu::Backends::DX12;
        }
    }
    config
}
