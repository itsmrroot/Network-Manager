//! Tools → Time (NTP) and Tools → MTU.

use std::time::Duration;

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::ntp::{self, Reading};
use netmgr::tools::{self as nt, Fits, Mtu};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl, trlf};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

type TimeResults = Vec<(String, String, Result<Reading, String>)>;

#[derive(Default)]
pub struct TimeTool {
    started: bool,
    extra: String,
    job: Option<Job<TimeResults>>,
    results: TimeResults,
}

impl TimeTool {
    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(v) => self.results = v,
                Err(e) => sh.fail(trl("The time servers could not be asked."), &e),
            }
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Compares this computer's clock with time servers (NTP). A clock that is off by minutes breaks Windows logins (Kerberos), certificates and the order of log messages.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.extra)
                        .hint_text(tr("Also ask, e.g. your domain controller or router"))
                        .desired_width(300.0),
                );
                let busy = self.job.is_some();
                let first = !self.started;
                self.started = true;
                if theme::primary_button(ui, p, &icon_label(icon::CLOCK, "Check"), !busy).clicked() || first {
                    let mut servers: Vec<(String, String)> =
                        ntp::SERVERS.iter().map(|(l, h)| (l.to_string(), h.to_string())).collect();
                    if let Some(g) = sh.default_adapter().and_then(|a| a.gateway) {
                        servers.insert(0, (tr("Router").into(), g.to_string()));
                    }
                    for h in self.extra.split([',', ' ']).map(str::trim).filter(|h| !h.is_empty()) {
                        servers.insert(0, (h.to_string(), h.to_string()));
                    }
                    self.job = Some(Job::spawn(ui.ctx(), move |_, _| {
                        Ok(std::thread::scope(|s| {
                            let handles: Vec<_> = servers
                                .into_iter()
                                .map(|(label, host)| {
                                    s.spawn(move || {
                                        let r = ntp::query(&host, Duration::from_secs(3)).map_err(|e| format!("{e:#}"));
                                        (label, host, r)
                                    })
                                })
                                .collect();
                            handles.into_iter().filter_map(|h| h.join().ok()).collect()
                        }))
                    }));
                }
                if busy {
                    ui.spinner();
                }
            });
        });
        if self.results.is_empty() {
            return;
        }
        ui.add_space(10.0);
        // The verdict from the servers that answered best (lowest stratum).
        let good: Vec<&Reading> =
            self.results.iter().filter_map(|(_, _, r)| r.as_ref().ok()).filter(|r| !r.unsynchronised).collect();
        if !good.is_empty() {
            let mut offsets: Vec<f64> = good.iter().map(|r| r.offset).collect();
            offsets.sort_by(|a, b| a.total_cmp(b));
            let median = offsets[offsets.len() / 2];
            let (color, glyph, text) = if median.abs() < 1.0 {
                (
                    p.success,
                    icon::CHECK_CIRCLE,
                    trlf(
                        "This computer's clock is right (within {offset}).",
                        &[("offset", &ntp::describe_offset(median))],
                    ),
                )
            } else if median.abs() < 60.0 {
                (
                    p.warning,
                    icon::WARNING,
                    trlf(
                        "This computer's clock is {offset} off. Turn on automatic time in the system's date settings.",
                        &[("offset", &ntp::describe_offset(median))],
                    ),
                )
            } else {
                (
                    p.danger,
                    icon::WARNING_CIRCLE,
                    trlf(
                        "This computer's clock is {offset} off: logins and secure websites may fail. Turn on automatic time in the system's date settings.",
                        &[("offset", &ntp::describe_offset(median))],
                    ),
                )
            };
            theme::notice(ui, p, color, glyph, &text);
            ui.add_space(8.0);
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("ntp").num_columns(5).spacing([22.0, 6.0]).striped(true).show(ui, |ui| {
                for h in [tr("Server"), tr("This clock"), tr("Delay"), tr("Stratum"), tr("Reference")] {
                    ui.label(RichText::new(h).color(p.weak).size(12.5));
                }
                ui.end_row();
                for (label, host, r) in &self.results {
                    ui.label(RichText::new(label).color(p.text)).on_hover_text(host);
                    match r {
                        Ok(r) => {
                            let c = if r.offset.abs() < 1.0 { p.text } else if r.offset.abs() < 60.0 { p.warning } else { p.danger };
                            ui.label(RichText::new(ntp::describe_offset(r.offset)).monospace().color(c));
                            ui.label(RichText::new(format!("{:.0} ms", r.delay * 1000.0)).monospace().color(p.weak));
                            ui.label(RichText::new(r.stratum.to_string()).monospace().color(p.weak))
                                .on_hover_text(tr("1: the server has its own reference clock (GPS, atomic); 2: it follows a stratum 1 server; and so on."));
                            let mut reference = r.reference.clone();
                            if r.unsynchronised {
                                reference = format!("{reference} · {}", tr("not synchronised"));
                            }
                            ui.label(RichText::new(reference).color(p.weak));
                        }
                        Err(e) => {
                            ui.label(RichText::new(e).color(p.weak).size(12.5));
                            ui.label("");
                            ui.label("");
                            ui.label("");
                        }
                    }
                    ui.end_row();
                }
            });
        });
    }
}

#[derive(Default)]
pub struct MtuTool {
    host: String,
    job: Option<Job<Mtu>>,
    result: Option<Mtu>,
}

impl MtuTool {
    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.job) {
            match r {
                Ok(v) => self.result = Some(v),
                Err(e) => sh.fail(trl("The MTU could not be measured."), &e),
            }
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                trl(
                    "Finds the largest packet that reaches a host without being split. A path MTU below 1500 bytes is normal through VPNs and some DSL lines; when it is lower than devices expect, large downloads or VPN connections stall.",
                ),
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if self.host.is_empty() {
                    self.host = "1.1.1.1".into();
                }
                crate::tools::host_picker(ui, sh, &mut self.host, "mtu-host");
                let busy = self.job.is_some();
                if theme::primary_button(
                    ui,
                    p,
                    &icon_label(icon::RULER, "Measure"),
                    !busy && !self.host.trim().is_empty(),
                )
                .clicked()
                {
                    let host = self.host.trim().to_string();
                    sh.settings.remember_host(&host);
                    self.result = None;
                    self.job = Some(Job::spawn(ui.ctx(), move |progress, cancel| {
                        nt::path_mtu(&host, cancel, |size, fits| {
                            let what = match fits {
                                Fits::Yes => "✓",
                                Fits::TooBig | Fits::NoAnswer => "✗",
                            };
                            progress.line(&format!("{} {what}", size + 28));
                        })
                    }));
                }
                if let Some(job) = &self.job {
                    ui.spinner();
                    if ui.button(icon_label(icon::STOP, "Stop")).clicked() {
                        job.stop();
                    }
                }
            });
            if let Some(job) = &self.job {
                let lines = job.progress.snapshot().lines;
                ui.add_space(6.0);
                ui.label(
                    RichText::new(trf("Tried: {sizes}", &[("sizes", &lines.join("  "))]))
                        .monospace()
                        .size(12.5)
                        .color(p.weak),
                );
            }
        });
        if let Some(r) = &self.result {
            ui.add_space(10.0);
            let text = if r.at_most {
                trlf(
                    "Packets of {mtu} bytes and more get through to {host}: jumbo frames work on this path.",
                    &[("mtu", &r.mtu), ("host", &r.host)],
                )
            } else if r.mtu >= 1500 {
                trlf(
                    "The path MTU to {host} is {mtu} bytes: full-size Ethernet packets get through.",
                    &[("mtu", &r.mtu), ("host", &r.host)],
                )
            } else {
                trlf(
                    "The path MTU to {host} is {mtu} bytes, less than the usual 1500: a VPN, tunnel or DSL line on the way. If large transfers stall, set the MTU of VPN or tunnel adapters to {mtu} or lower (TCP MSS {mss}).",
                    &[("mtu", &r.mtu), ("host", &r.host), ("mss", &r.mtu.saturating_sub(40))],
                )
            };
            let color = if r.mtu >= 1500 { p.success } else { p.warning };
            theme::notice(ui, p, color, icon::RULER, &text);
        }
    }
}
