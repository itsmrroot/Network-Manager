//! Background work (system commands, scans, tests) with progress that the
//! UI can read at any time, so the window never freezes.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use eframe::egui;

/// What a job reports while it runs.
#[derive(Default)]
pub struct Progress {
    state: Mutex<State>,
    ctx: Mutex<Option<egui::Context>>,
}

#[derive(Default, Clone)]
pub struct State {
    pub fraction: Option<f32>,
    pub message: String,
    /// Output lines (ping, traceroute).
    pub lines: Vec<String>,
}

impl Progress {
    pub fn set(&self, fraction: f32, message: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.fraction = Some(fraction.clamp(0.0, 1.0));
            message.clone_into(&mut s.message);
        }
        self.repaint();
    }

    pub fn line(&self, line: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.lines.push(line.to_string());
            // Continuous pings would grow forever.
            if s.lines.len() > 5000 {
                s.lines.drain(..1000);
            }
        }
        self.repaint();
    }

    pub fn snapshot(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn repaint(&self) {
        if let Ok(c) = self.ctx.lock()
            && let Some(c) = c.as_ref()
        {
            c.request_repaint();
        }
    }
}

/// A unit of background work producing a `T`.
pub struct Job<T> {
    pub progress: Arc<Progress>,
    pub cancel: Arc<AtomicBool>,
    rx: Receiver<Result<T>>,
}

impl<T: Send + 'static> Job<T> {
    pub fn spawn(ctx: &egui::Context, work: impl FnOnce(&Progress, &AtomicBool) -> Result<T> + Send + 'static) -> Self {
        let progress = Arc::new(Progress::default());
        if let Ok(mut c) = progress.ctx.lock() {
            *c = Some(ctx.clone());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (p, c, ctx) = (progress.clone(), cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| work(&p, &c)))
                .unwrap_or_else(|_| Err(anyhow!("an internal error occurred (please report it)")));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        Self { progress, cancel, rx }
    }

    /// The result, once the work has finished.
    pub fn poll(&self) -> Option<Result<T>> {
        self.rx.try_recv().ok()
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn stopping(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Polls `job`; when it is done, clears it and returns the result.
pub fn finished<T: Send + 'static>(job: &mut Option<Job<T>>) -> Option<Result<T>> {
    let r = job.as_ref()?.poll()?;
    *job = None;
    Some(r)
}

/// An error for people: the cancelled password dialog says so plainly.
pub fn describe(e: &anyhow::Error) -> String {
    if netmgr::cmd::is_cancelled(e) { "Cancelled: the password was not entered.".into() } else { format!("{e:#}") }
}
