//! The Help page: short step-by-step guides for common situations.

use eframe::egui::{self, RichText, Ui, collapsing_header::CollapsingState};
use egui_phosphor::regular as icon;

use crate::theme::{self, Palette};

struct Guide {
    icon: &'static str,
    title: &'static str,
    steps: Vec<&'static str>,
}

fn guides() -> Vec<Guide> {
    let rights = if cfg!(windows) {
        "Windows asks for administrator rights when the app starts: click Yes. Without them, settings can be looked at but not changed."
    } else if cfg!(target_os = "macos") {
        "macOS asks for your password when you change a setting, and remembers it for a few minutes."
    } else {
        "Changes go through NetworkManager. If it does not allow your user, the system asks for your password."
    };
    let mut access = vec![rights];
    if cfg!(target_os = "macos") {
        access.push(
            "macOS hides MAC addresses and the devices on your network until you allow it: turn on Network Manager in \
             System Settings → Privacy & Security → Local Network, then restart the app.",
        );
        access.push(
            "macOS also hides the name of your Wi-Fi network from apps. Click \"Show name\" on the Wi-Fi page and enter your password to see it.",
        );
    }
    vec![
        Guide {
            icon: icon::ROCKET_LAUNCH,
            title: "Getting started",
            steps: vec![
                "Overview shows how you are connected: your addresses, the router, DNS, the Wi-Fi signal and live traffic.",
                "Something does not work? Click \"Check connection\": it tests each step and says what to do.",
                "Adapters lists every network adapter. Select one to see and change its settings.",
                "Devices shows who is on your network. Wi-Fi shows saved networks and their passwords.",
            ],
        },
        Guide { icon: icon::SHIELD_CHECK, title: "Permissions", steps: access },
        Guide {
            icon: icon::PENCIL_SIMPLE,
            title: "Changing the IP address",
            steps: vec![
                "Open Adapters, select the adapter and click \"Change IP settings\".",
                "Automatic (DHCP) lets the router choose. Manual sets a fixed address: enter the address, the subnet (255.255.255.0 or 24) and usually the router as gateway.",
                "To reach a device that has a fixed address (a switch, a camera, a printer), give your computer an address in the same subnet, e.g. 192.168.1.50 for a device at 192.168.1.1. A gateway is not needed for that.",
                "Click Apply. If something stops working, click Undo to go back to the previous settings.",
                "Tick \"Also save as a profile\" to switch to these settings in one click later.",
            ],
        },
        Guide {
            icon: icon::LIST_MAGNIFYING_GLASS,
            title: "Changing DNS servers",
            steps: vec![
                "In \"Change IP settings\", set DNS servers to Manual and choose a service, such as Cloudflare or Quad9 (which blocks harmful sites).",
                "After a change, click \"Flush DNS\" on the Overview so that names are looked up again.",
                "Tools → DNS lookup compares what different DNS servers answer — useful after changing a website's records.",
            ],
        },
        Guide {
            icon: icon::FINGERPRINT,
            title: "Changing the MAC address",
            steps: vec![
                "Open Adapters, select the adapter and click \"Change MAC address\". Click Random for a private address, or type one.",
                "The connection drops for a moment while the adapter restarts.",
                "\"Restore original\" brings back the address the adapter was made with. The app also remembers the original before the first change.",
                "Windows only accepts Wi-Fi addresses whose second digit is 2, 6, A or E: Random always makes such an address.",
                "Only change MAC addresses on networks you own or may test.",
            ],
        },
        Guide {
            icon: icon::KEY,
            title: "Saved Wi-Fi passwords",
            steps: vec![
                "Open Wi-Fi → Saved networks. Click the eye to show a password, the copy button to copy it.",
                "The QR code button shows a code that phones scan with their camera to join the network.",
                "Export saves all networks and passwords to a CSV file. Keep that file private.",
                "Only networks this computer has joined are listed: passwords of other networks cannot be shown.",
            ],
        },
        Guide {
            icon: icon::DEVICES,
            title: "Finding devices on your network",
            steps: vec![
                "Open Devices: the network is scanned automatically. Click \"Scan again\" to refresh.",
                "Each device shows its address, maker and name. A device marked New was not there in an earlier scan.",
                "\"Private address\" means the device (often a phone) hides its real address for privacy, so the maker is unknown.",
                "Select a device to give it a name, ping it, check its ports, open its web page or wake it up.",
            ],
        },
        Guide {
            icon: icon::STETHOSCOPE,
            title: "When the internet does not work",
            steps: vec![
                "Click \"Check connection\" on the Overview and follow its advice.",
                "\"No network address\" (169.254.x.x): the router did not answer. Restart it, then click \"Renew IP\".",
                "Names do not work but addresses do: set a public DNS server (Cloudflare 1.1.1.1) and click \"Flush DNS\".",
                "A login page is in the way: open any website in your browser to sign in (hotels, airports, cafés).",
                "Windows: Adapters has no fix for a damaged network stack, but the command line does: netmgr reset-network (then restart).",
            ],
        },
        Guide {
            icon: icon::TREE_STRUCTURE,
            title: "Which switch port is this cable on?",
            steps: vec![
                "Plug in the cable, open Switch port, choose the wired adapter and click Listen.",
                "Within a minute the switch's name, the port (e.g. Gi1/0/12), the VLAN, the voice VLAN, PoE and its management address appear. Nothing is sent: the app only listens to what the switch announces (LLDP, CDP).",
                "Nothing heard? LLDP/CDP may be off on that switch, or it is an unmanaged switch.",
                "Listening to the cable needs administrator rights: macOS and Linux ask for your password; Windows uses its built-in packet monitor (pktmon).",
            ],
        },
        Guide {
            icon: icon::BROADCAST,
            title: "Finding DHCP servers (and rogue ones)",
            steps: vec![
                "On the Switch port page, click \"Find DHCP servers\". Every server that answers is listed with the address, router and DNS it hands out.",
                "Two servers usually mean a rogue one — a home router or a virtual machine host plugged in by mistake. Its address tells you where to look.",
                "Only a request for an offer is sent: no address is taken and nothing changes on this computer.",
            ],
        },
        Guide {
            icon: icon::HEARTBEAT,
            title: "Watching hosts and paths",
            steps: vec![
                "Monitor → Ping monitor: add servers, switches or the internet. Each is pinged continuously; you are told when one goes down and comes back. Export the results as CSV.",
                "Monitor → Path analysis: enter a host to see every router on the way, with its loss and delay, updated every second (like MTR or PingPlotter).",
                "Loss that starts at one router and continues to the end is real, and shows where the problem is. Loss at a single router only is that router ignoring pings: not a problem.",
            ],
        },
        Guide {
            icon: icon::HARD_DRIVES,
            title: "TFTP, syslog and throughput",
            steps: vec![
                "Servers → TFTP: choose a folder and click Start. Switches and routers can then copy firmware from it and back up their configuration to it. Example commands are shown.",
                "Servers → Syslog: click Start and point devices to this computer (logging host <address>). Messages appear live; filter them by severity or text and export them.",
                "Servers → Throughput: start the server on one computer and the test on another to measure the real speed of a cable, a switch or a Wi-Fi link.",
                "Allow the app in the firewall when the system asks. On Linux, ports below 1024 (TFTP 69, syslog 514) need administrator rights.",
            ],
        },
        Guide {
            icon: icon::TERMINAL_WINDOW,
            title: "Console cables, SSH and Telnet",
            steps: vec![
                "Plug in the console cable, open Console, choose the port and click Connect. Most devices use 9600 baud, 8N1, no flow control.",
                "Click the black screen and type: every key goes to the device, so Tab completion, ? help and Ctrl+Shift+6 work as on a real terminal.",
                "Send break enters ROMMON for password recovery. Paste sends a whole configuration. Log to file keeps a copy of the session.",
                "Linux: if the port cannot be opened, add yourself to the dialout group. Windows and macOS may need the cable maker's driver.",
                "SSH and Telnet open in the system's terminal.",
            ],
        },
        Guide {
            icon: icon::LOCK,
            title: "Web, WHOIS, connections, routes and hosts",
            steps: vec![
                "Tools → Web & TLS check: a certificate's issuer, names, expiry and trust, the TLS version, every redirect and the security headers.",
                "Tools → WHOIS: who owns a domain, an IP address or an AS number, with the abuse contact.",
                "Tools → Connections: which programs listen on which ports, and what they are connected to.",
                "Tools → Routes: the route table; add a static route to a lab network or delete one.",
                "Tools → Hosts file: add, turn off or remove entries. A backup of the original file is kept.",
            ],
        },
        Guide {
            icon: icon::FILE_TEXT,
            title: "A report for a support ticket",
            steps: vec![
                "On the Overview, click \"Network report\". After a few seconds of tests, save the file.",
                "It contains the computer, the adapters, Wi-Fi, routes, DNS timing, the connection check and how well the router and the internet answer — never Wi-Fi passwords.",
            ],
        },
        Guide {
            icon: icon::TERMINAL_WINDOW,
            title: "The command line",
            steps: vec![
                "Every download includes netmgr, for scripts and the keyboard. Run netmgr --help to see everything.",
                "netmgr set-ip Ethernet 192.168.1.50/24 --gateway 192.168.1.1 --dns 1.1.1.1",
                "netmgr profile apply Office · netmgr wifi passwords · netmgr devices · netmgr diagnose",
                "netmgr switch-port · netmgr dhcp-test · netmgr mtr google.com · netmgr monitor 10.0.0.1 1.1.1.1",
                "netmgr tftp-server ~/TFTP --allow-upload · netmgr syslog-server · netmgr tls example.com · netmgr report -o report.md",
                "Add --json to get results for other programs.",
            ],
        },
    ]
}

pub fn page(ui: &mut Ui, p: &Palette) {
    theme::page_title(ui, p, "Help", "Step-by-step guides for common tasks.");
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for (i, g) in guides().into_iter().enumerate() {
            theme::card(ui, p, |ui| {
                ui.set_width(ui.available_width());
                let id = ui.make_persistent_id(("guide", i));
                CollapsingState::load_with_default_open(ui.ctx(), id, i == 0)
                    .show_header(ui, |ui| {
                        ui.label(RichText::new(g.icon).size(18.0).color(p.accent));
                        ui.label(theme::semibold(g.title, 16.0).color(p.text));
                    })
                    .body(|ui| {
                        for (n, step) in g.steps.iter().enumerate() {
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new(format!("{}.", n + 1)).color(p.accent).strong());
                                theme::paragraph(ui, step, 14.5, p.text);
                            });
                        }
                    });
            });
            ui.add_space(10.0);
        }
    });
}
