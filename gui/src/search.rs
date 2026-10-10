//! Search (⌘K / Ctrl+K): every page, tab, tool and action by name, in the
//! interface language and by common English words ("password", "ztp").

use eframe::egui::{self, Key, RichText};
use egui_phosphor::regular as icon;

use crate::app::Page;
use crate::i18n::tr;
use crate::menu::Command;
use crate::theme::Palette;
use crate::tools;

/// Where an entry leads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Page(Page),
    Tool(tools::Tab),
    /// A page and one of its tabs, by name.
    Tab(Page, &'static str),
    Command(Command),
}

struct Entry {
    glyph: &'static str,
    label: String,
    /// "Servers", "Tools › Test": where it is.
    place: String,
    /// More words it is found by (English, lower case).
    words: &'static str,
    target: Target,
}

#[derive(Default)]
pub struct Search {
    pub open: bool,
    query: String,
    selected: usize,
    focus: bool,
}

fn entries() -> Vec<Entry> {
    let e = |glyph, label: &str, place: &str, words, target| Entry {
        glyph,
        label: label.to_string(),
        place: place.to_string(),
        words,
        target,
    };
    let mut v = vec![
        e(icon::GAUGE, tr("Overview"), "", "home status connection ip address public", Target::Page(Page::Overview)),
        e(
            icon::PLUGS_CONNECTED,
            tr("Adapters"),
            "",
            "ip mac change static dhcp interface ethernet",
            Target::Page(Page::Adapters),
        ),
        e(
            icon::KEY,
            tr("Saved networks"),
            tr("Wi-Fi"),
            "wifi password key psk qr share",
            Target::Tab(Page::Wifi, "saved"),
        ),
        e(
            icon::BROADCAST,
            tr("Nearby networks"),
            tr("Wi-Fi"),
            "wifi scan channel ssid",
            Target::Tab(Page::Wifi, "nearby"),
        ),
        e(
            icon::WAVE_SINE,
            tr("Signal and roaming"),
            tr("Wi-Fi"),
            "wifi signal roam rssi heat",
            Target::Tab(Page::Wifi, "signal"),
        ),
        e(icon::DEVICES, tr("Devices"), "", "scan who lan clients", Target::Tab(Page::Devices, "list")),
        e(
            icon::TREE_STRUCTURE,
            tr("Map"),
            tr("Devices"),
            "map topology diagram lldp cdp",
            Target::Tab(Page::Devices, "map"),
        ),
        e(
            icon::CHECK_SQUARE,
            tr("Free addresses"),
            tr("Devices"),
            "free ip conflict duplicate ipam",
            Target::Tab(Page::Devices, "free"),
        ),
        e(
            icon::BROADCAST,
            tr("Services (Bonjour)"),
            tr("Devices"),
            "bonjour mdns airplay printer chromecast",
            Target::Tab(Page::Devices, "bonjour"),
        ),
        e(
            icon::TREE_STRUCTURE,
            tr("Switch port"),
            "",
            "lldp cdp vlan port poe dhcp rogue",
            Target::Page(Page::SwitchPort),
        ),
        e(
            icon::HEARTBEAT,
            tr("Ping monitor"),
            tr("Monitor"),
            "monitor ping alert uptime",
            Target::Tab(Page::Monitor, "hosts"),
        ),
        e(
            icon::PATH,
            tr("Path analysis (MTR)"),
            tr("Monitor"),
            "mtr pingplotter path loss",
            Target::Tab(Page::Monitor, "path"),
        ),
        e(
            icon::CHART_BAR,
            tr("Traffic per program"),
            tr("Monitor"),
            "bandwidth program app usage nettop",
            Target::Tab(Page::Monitor, "programs"),
        ),
        e(
            icon::FILE_MAGNIFYING_GLASS,
            tr("Packet capture"),
            tr("Monitor"),
            "capture pcap wireshark tcpdump sniff",
            Target::Tab(Page::Monitor, "capture"),
        ),
        e(icon::STACK, tr("Profiles"), "", "profile office home proxy printer", Target::Page(Page::Profiles)),
        e(
            icon::HARD_DRIVES,
            tr("TFTP server"),
            tr("Servers"),
            "tftp firmware backup",
            Target::Tab(Page::Servers, "tftp"),
        ),
        e(icon::GLOBE, "HTTP", tr("Servers"), "http web file server firmware", Target::Tab(Page::Servers, "http")),
        e(icon::LIST_BULLETS, tr("Syslog server"), tr("Servers"), "syslog log", Target::Tab(Page::Servers, "syslog")),
        e(icon::BELL_RINGING, tr("SNMP traps"), tr("Servers"), "snmp trap inform", Target::Tab(Page::Servers, "traps")),
        e(
            icon::TREE_STRUCTURE,
            "DHCP",
            tr("Servers"),
            "dhcp server ztp zero touch option 66 67 150 43 provisioning",
            Target::Tab(Page::Servers, "dhcp"),
        ),
        e(icon::CLOCK, tr("Time (NTP)"), tr("Servers"), "ntp time server", Target::Tab(Page::Servers, "time")),
        e(
            icon::GAUGE,
            tr("Throughput test"),
            tr("Servers"),
            "iperf throughput bandwidth speed",
            Target::Tab(Page::Servers, "throughput"),
        ),
        e(
            icon::USB,
            tr("Serial console"),
            tr("Console"),
            "console serial cable com rommon",
            Target::Tab(Page::Console, "serial"),
        ),
        e(
            icon::TERMINAL_WINDOW,
            tr("SSH and Telnet"),
            tr("Console"),
            "ssh telnet putty terminal session",
            Target::Tab(Page::Console, "remote"),
        ),
        e(
            icon::CLOUD_ARROW_DOWN,
            tr("Backups"),
            tr("Console"),
            "backup config running diff oxidized",
            Target::Tab(Page::Console, "backups"),
        ),
        e(icon::GEAR_SIX, tr("Settings"), "", "settings preferences language theme", Target::Page(Page::Settings)),
        e(icon::QUESTION, tr("Help"), "", "help guide how", Target::Page(Page::Help)),
        e(icon::INFO, tr("About"), "", "about version update", Target::Page(Page::About)),
        e(
            icon::STETHOSCOPE,
            tr("Check connection"),
            tr("Network"),
            "diagnose internet down fix",
            Target::Command(Command::CheckConnection),
        ),
        e(
            icon::ARROWS_CLOCKWISE,
            tr("Renew IP"),
            tr("Network"),
            "renew dhcp release",
            Target::Command(Command::RenewIp),
        ),
        e(icon::BROOM, tr("Flush DNS"), tr("Network"), "flush dns cache", Target::Command(Command::FlushDns)),
        e(
            icon::GAUGE,
            tr("Speed test"),
            tr("Network"),
            "speed test bandwidth internet",
            Target::Command(Command::SpeedTest),
        ),
        e(
            icon::MAGNIFYING_GLASS,
            tr("Scan for devices"),
            tr("Network"),
            "scan lan devices",
            Target::Command(Command::Scan),
        ),
        e(
            icon::FILE_TEXT,
            tr("Network report"),
            tr("Network"),
            "report support ticket",
            Target::Command(Command::Report),
        ),
    ];
    for (tab, glyph, label, _) in tools::tool_list() {
        let words = match tab {
            tools::Tab::Ping => "ping icmp latency",
            tools::Tab::Trace => "traceroute tracert hops",
            tools::Tab::Ports => "port scan open nmap tcp",
            tools::Tab::Mtu => "mtu fragment mss vpn",
            tools::Tab::Time => "ntp clock time",
            tools::Tab::Wol => "wake on lan wol magic packet",
            tools::Tab::Dns => "dns lookup nslookup dig record",
            tools::Tab::Whois => "whois rdap owner domain asn",
            tools::Tab::MacLookup => "mac vendor oui manufacturer",
            tools::Tab::Web => "tls ssl certificate https redirect",
            tools::Tab::Subnet => "subnet cidr mask calculator",
            tools::Tab::Planner => "vlsm plan vlan subnet design rooms",
            tools::Tab::Snmp => "snmp walk interface counters",
            tools::Tab::Connections => "netstat connections listening ports",
            tools::Tab::Routes => "route table static gateway",
            tools::Tab::Hosts => "hosts file etc",
        };
        v.push(Entry {
            glyph,
            label: label.to_string(),
            place: tr("Tools").to_string(),
            words,
            target: Target::Tool(tab),
        });
    }
    v
}

impl Search {
    /// Development aid: open with `query` typed in.
    #[cfg(debug_assertions)]
    pub fn demo(&mut self, query: &str) {
        self.show();
        self.query = query.into();
        self.focus = false;
    }

    pub fn show(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.focus = true;
    }

    /// The search window; returns where to go when something is chosen.
    pub fn ui(&mut self, ctx: &egui::Context, p: &Palette) -> Option<Target> {
        if !self.open {
            return None;
        }
        let q = self.query.trim().to_lowercase();
        let all = entries();
        let mut found: Vec<(u32, &Entry)> = all
            .iter()
            .filter_map(|e| {
                if q.is_empty() {
                    return Some((0, e));
                }
                let label = e.label.to_lowercase();
                // Best: the name starts with it; then the name, the place or
                // the extra words contain every word typed.
                let score = if label.starts_with(&q) {
                    3
                } else if label.contains(&q) {
                    2
                } else if q
                    .split_whitespace()
                    .all(|w| label.contains(w) || e.place.to_lowercase().contains(w) || e.words.contains(w))
                {
                    1
                } else {
                    return None;
                };
                Some((score, e))
            })
            .collect();
        found.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        let found: Vec<&Entry> = found.into_iter().map(|(_, e)| e).take(12).collect();
        self.selected = self.selected.min(found.len().saturating_sub(1));
        let (up, down, enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
            )
        });
        if down && self.selected + 1 < found.len() {
            self.selected += 1;
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        let mut chosen = None;
        let modal = egui::Modal::new(egui::Id::new("search")).show(ctx, |ui| {
            ui.set_width(520.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(format!("{}  {}", icon::MAGNIFYING_GLASS, tr("Search pages, tools and actions")))
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Heading),
            );
            if self.focus {
                r.request_focus();
                self.focus = false;
            }
            if r.changed() {
                self.selected = 0;
            }
            ui.add_space(6.0);
            if found.is_empty() {
                ui.label(RichText::new(tr("Nothing found.")).color(p.weak));
            }
            for (i, e) in found.iter().enumerate() {
                let selected = i == self.selected;
                let text = if e.place.is_empty() { e.label.clone() } else { format!("{}   ·  {}", e.label, e.place) };
                let b =
                    egui::Button::new(RichText::new(format!("{}   {text}", e.glyph)).size(14.0).color(if selected {
                        p.text
                    } else {
                        p.weak
                    }))
                    .fill(if selected { p.tint(p.accent) } else { egui::Color32::TRANSPARENT })
                    .min_size(egui::vec2(ui.available_width(), 30.0));
                let r = ui.add(b);
                if r.hovered() {
                    self.selected = i;
                }
                if r.clicked() {
                    chosen = Some(e.target);
                }
            }
            ui.add_space(4.0);
            ui.label(RichText::new(tr("↑ ↓ to choose, Enter to open, Esc to close")).color(p.weak).size(12.0));
        });
        if enter && let Some(e) = found.get(self.selected) {
            chosen = Some(e.target);
        }
        if chosen.is_some() || esc || modal.should_close() {
            self.open = false;
        }
        chosen
    }
}
