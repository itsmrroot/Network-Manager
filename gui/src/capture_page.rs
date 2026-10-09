//! Monitor → Packet capture: live packets on an adapter with a short
//! description of each, a display filter, a hex view, and pcap files for
//! Wireshark.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};
use egui_phosphor::regular as icon;
use netmgr::capture::{self, Frame, Reader, Summary};

use crate::app::Shared;
use crate::i18n::{tr, trf, trl};
use crate::jobs::{self, Job};
use crate::theme::{self, Palette, icon_label};

/// Packets kept in memory; the file has them all.
const MAX_PACKETS: usize = 200_000;

struct Running {
    dir: PathBuf,
    out: PathBuf,
    stop: PathBuf,
    reader: Option<Reader>,
    started: Instant,
    /// When Stop was clicked.
    stopping: Option<Instant>,
}

pub struct Capture {
    interface: String,
    seconds: u64,
    starting: Option<Job<()>>,
    running: Option<Running>,
    link: u32,
    packets: Vec<(Frame, Summary)>,
    first_time: f64,
    filter: String,
    selected: Option<usize>,
    /// The last capture file, kept until the next one starts.
    last_file: Option<PathBuf>,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            interface: String::new(),
            seconds: 300,
            starting: None,
            running: None,
            link: capture::LINK_ETHERNET,
            packets: Vec::new(),
            first_time: 0.0,
            filter: String::new(),
            selected: None,
            last_file: None,
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(r) = &self.running {
            let _ = std::fs::write(&r.stop, b"");
        }
    }
}

impl Capture {
    pub fn running(&self) -> bool {
        self.running.is_some() || self.starting.is_some()
    }

    fn start(&mut self, ctx: &egui::Context, sh: &Shared) {
        let Some(a) = sh.adapters.iter().find(|a| a.id == self.interface) else { return };
        let iface = if cfg!(windows) { a.name.clone() } else { a.device.clone() };
        let dir = std::env::temp_dir().join(format!(
            "netmgr-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
        ));
        if let Some(old) = self.last_file.take()
            && let Some(parent) = old.parent()
        {
            let _ = std::fs::remove_dir_all(parent);
        }
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let out = dir.join("capture.pcap");
        let stop = dir.join("stop");
        self.packets.clear();
        self.selected = None;
        let seconds = self.seconds;
        let (o, s) = (out.clone(), stop.clone());
        self.starting = Some(Job::spawn(ctx, move |_, _| capture::start(&iface, &o, &s, seconds)));
        self.running = Some(Running { dir, out, stop, reader: None, started: Instant::now(), stopping: None });
    }

    pub fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if let Some(r) = jobs::finished(&mut self.starting)
            && let Err(e) = r
        {
            if let Some(run) = self.running.take() {
                let _ = std::fs::remove_dir_all(&run.dir);
            }
            sh.fail(trl("The capture could not start."), &e);
            return;
        }
        if self.starting.is_some() {
            return;
        }
        let Some(run) = &mut self.running else { return };
        ctx.request_repaint_after(Duration::from_millis(300));
        if run.reader.is_none() && run.out.exists() {
            run.reader = Reader::open(&run.out).ok();
        }
        let mut done = false;
        match &mut run.reader {
            Some(reader) => match reader.poll() {
                Ok(frames) => {
                    if let Some(link) = reader.link {
                        self.link = link;
                    }
                    for f in frames {
                        if self.packets.len() >= MAX_PACKETS {
                            break;
                        }
                        if self.packets.is_empty() {
                            self.first_time = f.time;
                        }
                        let s = capture::decode(self.link, &f.data);
                        self.packets.push((f, s));
                    }
                }
                Err(e) => {
                    sh.fail(trl("The capture file could not be read."), &e);
                    done = true;
                }
            },
            None => {
                // The helper writes a log when it cannot start.
                if let Some(msg) = capture::start_error(&run.out) {
                    sh.fail(trl("The capture could not start."), &anyhow::anyhow!(msg));
                    done = true;
                } else if run.started.elapsed() > Duration::from_secs(20) {
                    sh.fail(trl("The capture could not start."), &anyhow::anyhow!("no capture file was written"));
                    done = true;
                }
            }
        }
        // After Stop, or when the time is up, read what is left and finish.
        let over = run.started.elapsed() > Duration::from_secs(self.seconds + 2);
        if run.stopping.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) || over || done {
            let _ = std::fs::write(&run.stop, b"");
            self.last_file = Some(run.out.clone());
            self.running = None;
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        self.poll(ui.ctx(), sh);
        if self.interface.is_empty()
            && let Some(a) = sh.default_adapter()
        {
            self.interface = a.id.clone();
        }
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            theme::paragraph(
                ui,
                if cfg!(windows) {
                    trl(
                        "Records the IPv4 packets of an adapter (Windows can only capture full Ethernet frames with an extra driver), and saves them for Wireshark. Needs administrator rights.",
                    )
                } else {
                    trl(
                        "Records every packet on an adapter, describes each one, and saves them as a pcap file for Wireshark. The system asks for your password.",
                    )
                },
                14.0,
                p.weak,
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                let busy = self.running();
                ui.add_enabled_ui(!busy, |ui| {
                    let shown = sh
                        .adapters
                        .iter()
                        .find(|a| a.id == self.interface)
                        .map_or(tr("Choose an adapter").to_string(), |a| a.name.clone());
                    egui::ComboBox::from_id_salt("cap-iface").selected_text(shown).width(200.0).show_ui(ui, |ui| {
                        for a in sh.visible_adapters() {
                            ui.selectable_value(&mut self.interface, a.id.clone(), &a.name);
                        }
                    });
                    egui::ComboBox::from_id_salt("cap-time")
                        .selected_text(duration_label(self.seconds))
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for s in [60, 300, 1800, 3600] {
                                ui.selectable_value(&mut self.seconds, s, duration_label(s));
                            }
                        });
                });
                if let Some(run) = &mut self.running {
                    if self.starting.is_some() {
                        ui.spinner();
                        ui.label(RichText::new(tr("Starting…")).color(p.weak));
                    } else {
                        if theme::danger_button(ui, p, &icon_label(icon::STOP, "Stop")).clicked()
                            && run.stopping.is_none()
                        {
                            let _ = std::fs::write(&run.stop, b"");
                            run.stopping = Some(Instant::now());
                        }
                        let left = self.seconds.saturating_sub(run.started.elapsed().as_secs());
                        ui.label(RichText::new(format!("● {}", tr("Recording"))).color(p.danger));
                        ui.label(RichText::new(trf("up to {n} s", &[("n", &left)])).color(p.weak));
                    }
                } else {
                    if theme::primary_button(
                        ui,
                        p,
                        &icon_label(icon::RECORD, "Start capture"),
                        !self.interface.is_empty(),
                    )
                    .clicked()
                    {
                        self.start(ui.ctx(), sh);
                    }
                    if ui.button(icon_label(icon::FOLDER_OPEN, "Open pcap…")).clicked() {
                        self.open_file(sh);
                    }
                }
            });
        });
        ui.add_space(8.0);
        self.packets_ui(ui, p, sh);
    }

    #[cfg(debug_assertions)]
    pub fn demo(&mut self, path: &std::path::Path, sh: &mut Shared) {
        self.load(path.to_path_buf(), sh);
        self.selected = Some(3);
    }

    fn open_file(&mut self, sh: &mut Shared) {
        let Some(path) = rfd::FileDialog::new().add_filter("pcap", &["pcap", "cap"]).pick_file() else { return };
        self.load(path, sh);
    }

    fn load(&mut self, path: PathBuf, sh: &mut Shared) {
        match capture::read_file(&path) {
            Ok((link, frames)) => {
                self.link = link;
                self.first_time = frames.first().map_or(0.0, |f| f.time);
                self.packets = frames
                    .into_iter()
                    .take(MAX_PACKETS)
                    .map(|f| {
                        let s = capture::decode(link, &f.data);
                        (f, s)
                    })
                    .collect();
                self.selected = None;
            }
            Err(e) => sh.fail(trl("The file could not be opened."), &e),
        }
    }

    fn save(&self, sh: &mut Shared, shown: &[usize]) {
        let Some(path) = rfd::FileDialog::new().add_filter("pcap", &["pcap"]).set_file_name("capture.pcap").save_file()
        else {
            return;
        };
        let frames: Vec<&Frame> = shown.iter().map(|&i| &self.packets[i].0).collect();
        match capture::write_file(&path, self.link, &frames) {
            Ok(()) => sh.toast(trf("Saved to {file}.", &[("file", &path.display())])),
            Err(e) => sh.fail(trl("The file could not be saved."), &e),
        }
    }

    fn packets_ui(&mut self, ui: &mut Ui, p: &Palette, sh: &mut Shared) {
        let shown: Vec<usize> = (0..self.packets.len())
            .filter(|&i| self.filter.trim().is_empty() || self.packets[i].1.matches(&self.filter))
            .collect();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(icon::FUNNEL).color(p.weak));
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(tr("Filter: dns, 443, 192.168.1.20, arp …"))
                    .desired_width(300.0),
            );
            ui.label(
                RichText::new(trf(
                    "{shown} of {total} packets",
                    &[("shown", &shown.len()), ("total", &self.packets.len())],
                ))
                .color(p.weak),
            );
            if !self.packets.is_empty() && !self.running() {
                if ui
                    .button(icon_label(icon::FLOPPY_DISK, "Save…"))
                    .on_hover_text(tr("All packets, as a pcap file"))
                    .clicked()
                {
                    self.save(sh, &(0..self.packets.len()).collect::<Vec<_>>());
                }
                if !self.filter.trim().is_empty() && ui.button(icon_label(icon::FUNNEL_SIMPLE, "Save shown…")).clicked()
                {
                    self.save(sh, &shown);
                }
                if ui.button(icon_label(icon::BROOM, "Clear")).clicked() {
                    self.packets.clear();
                    self.selected = None;
                }
            }
        });
        ui.add_space(6.0);
        let detail_h = if self.selected.is_some() { 170.0 } else { 0.0 };
        let list_h = (ui.available_height() - detail_h - 10.0).max(150.0);
        let row_h = 20.0;
        theme::card(ui, p, |ui| {
            ui.set_width(ui.available_width());
            let widths = [60.0, 80.0, 220.0, 220.0, 70.0, 60.0];
            let head = |ui: &mut Ui| {
                for (h, w) in [tr("No."), tr("Time"), tr("Source"), tr("Destination"), tr("Protocol"), tr("Length")]
                    .iter()
                    .zip(widths)
                {
                    ui.add_sized([w, row_h], egui::Label::new(RichText::new(*h).color(p.weak).size(12.5)));
                }
                ui.label(RichText::new(tr("Info")).color(p.weak).size(12.5));
            };
            ui.horizontal(head);
            if self.packets.is_empty() {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(if self.running() {
                        tr("Waiting for packets…")
                    } else {
                        tr("Choose an adapter and click Start capture, or open a pcap file.")
                    })
                    .color(p.weak),
                );
                return;
            }
            let follow = self.running.is_some();
            egui::ScrollArea::vertical()
                .id_salt("packets")
                .max_height(list_h)
                .auto_shrink(false)
                .stick_to_bottom(follow)
                .show_rows(ui, row_h, shown.len(), |ui, range| {
                    for &i in &shown[range] {
                        let (f, s) = &self.packets[i];
                        let selected = self.selected == Some(i);
                        let color = proto_color(p, &s.protocol);
                        let r = ui
                            .horizontal(|ui| {
                                let mono = |t: String| RichText::new(t).monospace().size(12.5);
                                ui.add_sized(
                                    [widths[0], row_h],
                                    egui::Label::new(mono((i + 1).to_string()).color(p.weak)),
                                );
                                ui.add_sized(
                                    [widths[1], row_h],
                                    egui::Label::new(mono(format!("{:.3}", f.time - self.first_time)).color(p.weak)),
                                );
                                ui.add_sized(
                                    [widths[2], row_h],
                                    egui::Label::new(mono(s.source.clone()).color(p.text)).truncate(),
                                );
                                ui.add_sized(
                                    [widths[3], row_h],
                                    egui::Label::new(mono(s.destination.clone()).color(p.text)).truncate(),
                                );
                                ui.add_sized(
                                    [widths[4], row_h],
                                    egui::Label::new(mono(s.protocol.clone()).color(color)),
                                );
                                ui.add_sized(
                                    [widths[5], row_h],
                                    egui::Label::new(mono(f.len.to_string()).color(p.weak)),
                                );
                                ui.add(egui::Label::new(RichText::new(&s.info).size(12.5).color(p.text)).truncate());
                            })
                            .response;
                        if selected {
                            ui.painter().rect_filled(r.rect, 4.0, p.tint(p.accent).gamma_multiply(0.6));
                        }
                        if ui.interact(r.rect, egui::Id::new(("pkt", i)), egui::Sense::click()).clicked() {
                            self.selected = if selected { None } else { Some(i) };
                        }
                    }
                });
        });
        if let Some(i) = self.selected.filter(|&i| i < self.packets.len()) {
            ui.add_space(6.0);
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                let (f, s) = &self.packets[i];
                ui.horizontal(|ui| {
                    ui.label(theme::semibold(format!("{} · {}", s.protocol, s.info), 14.0).color(p.text));
                    if ui.small_button(icon::COPY).on_hover_text(tr("Copy")).clicked() {
                        ui.ctx().copy_text(hexdump(&f.data));
                        sh.toast(tr("Copied."));
                    }
                });
                egui::ScrollArea::vertical().id_salt("hex").max_height(130.0).show(ui, |ui| {
                    ui.label(RichText::new(hexdump(&f.data)).monospace().size(12.0).color(p.weak));
                });
            });
        }
    }
}

fn duration_label(s: u64) -> String {
    if s >= 3600 { trf("{n} h", &[("n", &(s / 3600))]) } else { trf("{n} min", &[("n", &(s / 60))]) }
}

fn proto_color(p: &Palette, proto: &str) -> egui::Color32 {
    match proto {
        "DNS" | "mDNS" | "LLMNR" => egui::Color32::from_rgb(86, 156, 214),
        "TLS" | "HTTP" => p.success,
        "ARP" | "DHCP" | "DHCPv6" => p.warning,
        "ICMP" | "ICMPv6" => egui::Color32::from_rgb(197, 134, 192),
        "LLDP" | "CDP" | "STP" => egui::Color32::from_rgb(78, 201, 176),
        _ => p.text,
    }
}

/// Offset, hex and text, 16 bytes a line.
fn hexdump(b: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in b.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|x| format!("{x:02x}")).collect();
        let text: String = chunk.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' }).collect();
        out += &format!("{:04x}  {:<48} {text}\n", i * 16, hex.join(" "));
    }
    out
}
