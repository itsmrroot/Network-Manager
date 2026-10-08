//! `netmgr`: the command line of Network Manager — Powered by Bashar Salmo.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use netmgr::adapters::{self, Adapter, format_bytes, format_speed};
use netmgr::config::{self, IpSettings, Ipv4Mode};
use netmgr::dns::{self, RecordType};
use netmgr::mac::Mac;
use netmgr::profiles::{self, Profile};
use netmgr::subnet::Subnet;
use netmgr::{internet, scan, tools, wifi};

#[derive(Parser)]
#[command(
    name = "netmgr",
    version,
    about = "Network Manager: IP and MAC addresses, Wi-Fi passwords, devices on your network and network tools.\nPowered by Bashar Salmo.",
    after_help = "Changing settings needs administrator rights: an administrator terminal on Windows; \
                  on macOS and Linux the system asks for your password."
)]
struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List network adapters (the default).
    Adapters {
        /// Include virtual, VPN and loopback adapters.
        #[arg(long)]
        all: bool,
    },
    /// Show everything about one adapter.
    Show { adapter: String },
    /// Set the IP address: "dhcp" or ADDRESS/PREFIX (e.g. 192.168.1.50/24).
    SetIp {
        adapter: String,
        address: String,
        /// The router (default gateway).
        #[arg(long, short)]
        gateway: Option<Ipv4Addr>,
        /// DNS servers, comma-separated, or "auto".
        #[arg(long, short)]
        dns: Option<String>,
    },
    /// Set the DNS servers: comma-separated addresses, or "auto".
    SetDns { adapter: String, servers: String },
    /// Show or change the MAC address: "random", "restore" or an address.
    Mac { adapter: String, address: Option<String> },
    /// Turn an adapter on.
    Enable { adapter: String },
    /// Turn an adapter off.
    Disable { adapter: String },
    /// Ask the router for a new IP address (DHCP).
    Renew { adapter: Option<String> },
    /// Empty the DNS cache.
    FlushDns,
    /// Reset the network stack (Windows: Winsock and TCP/IP).
    ResetNetwork,
    /// Wi-Fi: the current network, saved passwords, nearby networks.
    Wifi {
        #[command(subcommand)]
        command: Option<WifiCmd>,
    },
    /// Find the devices on the local network.
    Devices {
        /// Scan from this adapter instead of the default one.
        #[arg(long)]
        adapter: Option<String>,
        /// Do not try ports (faster).
        #[arg(long)]
        no_ports: bool,
    },
    /// Saved IP settings, applied in one go.
    Profile {
        #[command(subcommand)]
        command: ProfileCmd,
    },
    /// Ping a host.
    Ping {
        host: String,
        /// Number of pings (0 = until Ctrl+C).
        #[arg(short, long, default_value_t = 4)]
        count: u32,
    },
    /// Show the route to a host.
    Trace { host: String },
    /// Look up DNS records, optionally at a given server, or compare servers.
    Lookup {
        name: String,
        #[arg(default_value = "A")]
        kind: String,
        /// The DNS server to ask (default: this computer's).
        #[arg(long, short)]
        server: Option<IpAddr>,
        /// Ask Cloudflare, Google, Quad9 and OpenDNS too.
        #[arg(long)]
        compare: bool,
    },
    /// Check which TCP ports of a host are open.
    Ports {
        host: String,
        /// e.g. "22,80,443" or "1-1024" (default: common ports).
        ports: Option<String>,
    },
    /// Subnet calculator: 192.168.1.10/24 or "192.168.1.10 255.255.255.0".
    Subnet {
        cidr: Vec<String>,
        /// List the subnets of this length, e.g. 26.
        #[arg(long)]
        split: Option<u8>,
    },
    /// Wake a computer with Wake-on-LAN.
    Wol { mac: String },
    /// Who makes a device, from its MAC address.
    Vendor { mac: String },
    /// This connection's public IP address and provider.
    PublicIp,
    /// Measure download and upload speed.
    Speedtest,
    /// Find out why the internet does not work, step by step.
    Diagnose,
}

#[derive(Subcommand)]
enum WifiCmd {
    /// The network each Wi-Fi adapter is joined to.
    Status,
    /// Saved networks and their passwords.
    Passwords {
        /// Linux: read with administrator rights (asks for the password).
        #[arg(long)]
        admin: bool,
    },
    /// Networks around, with signal and channel.
    Nearby,
    /// A QR code that phones scan to join a saved network.
    Qr { ssid: String },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// List saved profiles.
    List,
    /// Save an adapter's current settings as a profile.
    Save {
        name: String,
        #[arg(long)]
        from: String,
    },
    /// Apply a profile to its adapter (or the one given).
    Apply {
        name: String,
        #[arg(long)]
        adapter: Option<String>,
    },
    /// Delete a profile.
    Delete { name: String },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        if netmgr::cmd::is_cancelled(&e) {
            eprintln!("Cancelled.");
        } else {
            eprintln!("error: {e:#}");
        }
        std::process::exit(1);
    }
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.command.unwrap_or(Cmd::Adapters { all: false }) {
        Cmd::Adapters { all } => {
            let list = adapters::list(all)?;
            if json {
                return print_json(&list);
            }
            for a in &list {
                let ip = a.main_ipv4().map_or("—".to_string(), |(ip, p)| format!("{ip}/{p}"));
                let star = if a.default { "*" } else { " " };
                println!(
                    "{star} {:<26} {:<9} {:<15} {:<19} {}",
                    a.name,
                    a.kind.label(),
                    a.status(),
                    ip,
                    a.mac.map_or("".into(), |m| m.to_string())
                );
            }
            println!("\n* = default route. `netmgr show <adapter>` for details.");
        }
        Cmd::Show { adapter } => {
            let a = adapters::find(&adapter)?;
            if json {
                return print_json(&a);
            }
            show(&a);
        }
        Cmd::SetIp { adapter, address, gateway, dns } => {
            let a = adapters::find(&adapter)?;
            let mode = if address.eq_ignore_ascii_case("dhcp") || address.eq_ignore_ascii_case("auto") {
                Ipv4Mode::Dhcp
            } else {
                let s = Subnet::parse(&address)?;
                if !address.contains(['/', ' ']) {
                    bail!("give the subnet too, e.g. {address}/24");
                }
                Ipv4Mode::Static { address: s.address, prefix: s.prefix, gateway }
            };
            let dns = match dns {
                Some(d) => parse_dns(&d)?,
                None if mode == Ipv4Mode::Dhcp => Vec::new(),
                // Keep the servers set by hand.
                None => IpSettings::current(&a).dns,
            };
            let s = IpSettings { mode, dns };
            config::apply(&a, &s)?;
            println!("{}: {}", a.name, s.summary());
        }
        Cmd::SetDns { adapter, servers } => {
            let a = adapters::find(&adapter)?;
            let mut s = IpSettings::current(&a);
            s.dns = parse_dns(&servers)?;
            config::apply(&a, &s)?;
            println!("{}: {}", a.name, s.summary());
        }
        Cmd::Mac { adapter, address } => {
            let a = adapters::find(&adapter)?;
            match address.as_deref() {
                None => {
                    let perm = config::permanent_mac(&a);
                    println!("Current:  {}", describe_mac(a.mac));
                    println!("Original: {}", describe_mac(perm));
                }
                Some("restore") | Some("original") => {
                    config::set_mac(&a, None)?;
                    println!("{} uses its original MAC address again.", a.name);
                }
                Some(other) => {
                    let m = if other == "random" { Mac::random() } else { other.parse()? };
                    config::set_mac(&a, Some(m))?;
                    println!("{} now uses {m}.", a.name);
                }
            }
        }
        Cmd::Enable { adapter } => {
            config::set_enabled(&adapters::find(&adapter)?, true)?;
            println!("Turned on.");
        }
        Cmd::Disable { adapter } => {
            config::set_enabled(&adapters::find(&adapter)?, false)?;
            println!("Turned off.");
        }
        Cmd::Renew { adapter } => {
            let a = match adapter {
                Some(n) => adapters::find(&n)?,
                None => adapters::default_adapter().context("not connected to a network")?,
            };
            config::renew(&a)?;
            println!("{} asked the router for a new address.", a.name);
        }
        Cmd::FlushDns => {
            config::flush_dns()?;
            println!("The DNS cache is empty.");
        }
        Cmd::ResetNetwork => println!("{}", config::reset_network()?),
        Cmd::Wifi { command } => wifi_cmd(command.unwrap_or(WifiCmd::Status), json)?,
        Cmd::Devices { adapter, no_ports } => {
            let opts = scan::Options { adapter, ports: !no_ports, names: true };
            let cancel = AtomicBool::new(false);
            let (range, devices) =
                scan::scan(&opts, &cancel, &|f, _| eprint!("\rScanning… {:3.0} %", f * 100.0), &|_| {})?;
            eprintln!("\r{:30}\r", "");
            if json {
                return print_json(&devices);
            }
            println!(
                "{} devices on {}/{} ({}){}\n",
                devices.len(),
                range.network,
                range.prefix,
                range.adapter,
                if range.limited { " — only the /22 around this computer" } else { "" }
            );
            for d in &devices {
                let mut tags = Vec::new();
                if d.is_self {
                    tags.push("this computer".to_string());
                }
                if d.is_gateway {
                    tags.push("router".to_string());
                }
                if d.private_address() {
                    tags.push("private address".to_string());
                }
                if !d.open_ports.is_empty() {
                    tags.push(format!(
                        "ports {}",
                        d.open_ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
                    ));
                }
                println!(
                    "{:<15} {:<17} {:<24} {:<22} {}",
                    d.ip,
                    d.mac.map_or("".into(), |m| m.to_string()),
                    d.name.as_deref().unwrap_or("—"),
                    d.vendor.as_deref().unwrap_or(d.kind().label()),
                    tags.join(" · ")
                );
            }
        }
        Cmd::Profile { command } => profile_cmd(command, json)?,
        Cmd::Ping { host, count } => {
            let cancel = AtomicBool::new(false);
            let mut stats = tools::PingStats::default();
            tools::ping(&host, count, &cancel, |line, kind| {
                println!("{line}");
                stats.add(kind);
            })?;
        }
        Cmd::Trace { host } => {
            let cancel = AtomicBool::new(false);
            tools::traceroute(&host, &cancel, |l| println!("{l}"))?;
        }
        Cmd::Lookup { name, kind, server, compare } => {
            let kind: RecordType = kind.parse()?;
            let mut servers: Vec<(String, IpAddr)> = match server {
                Some(s) => vec![(s.to_string(), s)],
                None => dns::system_servers().into_iter().take(1).map(|s| ("This computer".to_string(), s)).collect(),
            };
            if servers.is_empty() {
                servers.push(("Cloudflare".into(), "1.1.1.1".parse()?));
            }
            if compare {
                for (n, ip) in dns::PUBLIC_RESOLVERS {
                    servers.push((n.to_string(), ip.parse()?));
                }
            }
            let mut answers = Vec::new();
            for (label, ip) in servers {
                let a = dns::query(SocketAddr::new(ip, 53), &name, kind, Duration::from_secs(3));
                match &a {
                    Ok(a) if !json => {
                        println!("{label} ({ip}) — {} in {} ms", a.status, a.millis);
                        for r in &a.records {
                            println!("  {:<6} {:>6}s  {}", r.kind, r.ttl, r.data);
                        }
                        if a.records.is_empty() {
                            println!("  (no records)");
                        }
                    }
                    Err(e) if !json => println!("{label} ({ip}) — {e:#}"),
                    _ => {}
                }
                answers.extend(a.ok());
            }
            if json {
                return print_json(&answers);
            }
        }
        Cmd::Ports { host, ports } => {
            let list = tools::parse_ports(ports.as_deref().unwrap_or(tools::PORT_PRESETS[0].1))?;
            let cancel = AtomicBool::new(false);
            let (ip, results) = tools::check_ports(&host, &list, Duration::from_millis(1500), &cancel, &|_| {})?;
            if json {
                return print_json(&results);
            }
            println!("{host} ({ip}): {} of {} ports open", results.iter().filter(|r| r.open).count(), results.len());
            for r in results.iter().filter(|r| r.open) {
                println!("  {:>5}  open   {}", r.port, r.service);
            }
        }
        Cmd::Subnet { cidr, split } => {
            let s = Subnet::parse(&cidr.join(" "))?;
            if json {
                return print_json(&s);
            }
            println!("Address      {}/{}  ({}, class {})", s.address, s.prefix, s.scope(), s.class());
            println!("Network      {}", s.network);
            println!("Netmask      {}  = {}", s.mask, s.mask_binary());
            println!("Wildcard     {}", s.wildcard);
            println!("Broadcast    {}", s.broadcast);
            println!("Hosts        {} – {}  ({} usable)", s.first, s.last, s.hosts);
            if let Some(p) = split {
                println!();
                for (i, sub) in s.split(p, 256)?.iter().enumerate() {
                    println!("{:>4}. {}/{:<3} {} – {}", i + 1, sub.network, sub.prefix, sub.first, sub.last);
                }
            }
        }
        Cmd::Wol { mac } => {
            let m: Mac = mac.parse()?;
            tools::wake(m, None)?;
            println!("Wake-on-LAN packet sent to {m}.");
        }
        Cmd::Vendor { mac } => {
            let m: Mac = mac.parse()?;
            println!("{}", describe_mac(Some(m)));
        }
        Cmd::PublicIp => {
            let info = internet::public_info()?;
            if json {
                return print_json(&info);
            }
            println!("{}", info.ip);
            if let Some(p) = info.provider() {
                println!("Provider  {p}");
            }
            if let Some(p) = info.place() {
                println!("Location  {p}");
            }
        }
        Cmd::Speedtest => {
            let cancel = AtomicBool::new(false);
            let r = internet::speed_test(&cancel, &|phase, v| {
                let what = match phase {
                    internet::SpeedPhase::Ping => format!("Ping      {v:6.1} ms  "),
                    internet::SpeedPhase::Download => format!("Download  {v:6.1} Mbit/s"),
                    internet::SpeedPhase::Upload => format!("Upload    {v:6.1} Mbit/s"),
                };
                eprint!("\r{what}");
            })?;
            eprintln!("\r{:40}\r", "");
            if json {
                return print_json(&r);
            }
            println!("Ping      {:.0} ms (jitter {:.1} ms)", r.ping_ms.unwrap_or(0.0), r.jitter_ms.unwrap_or(0.0));
            println!("Download  {:.1} Mbit/s", r.download_mbps.unwrap_or(0.0));
            println!("Upload    {:.1} Mbit/s", r.upload_mbps.unwrap_or(0.0));
        }
        Cmd::Diagnose => {
            let checks = internet::diagnose(&|c| {
                if !json {
                    println!("{} {:<12} {}", if c.ok { "✔" } else { "✘" }, c.title, c.detail);
                    if let Some(a) = &c.advice {
                        println!("  → {a}");
                    }
                }
            });
            if json {
                return print_json(&checks);
            }
        }
    }
    Ok(())
}

fn parse_dns(s: &str) -> Result<Vec<IpAddr>> {
    if s.eq_ignore_ascii_case("auto") || s.eq_ignore_ascii_case("dhcp") {
        return Ok(Vec::new());
    }
    s.split([',', ' '])
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.trim().parse().with_context(|| format!("\"{p}\" is not an IP address")))
        .collect()
}

fn describe_mac(m: Option<Mac>) -> String {
    match m {
        None => "unknown".into(),
        Some(m) if m.is_local() => format!("{m} (set by software — private or changed address)"),
        Some(m) => format!("{m} ({})", m.vendor().unwrap_or("unknown manufacturer")),
    }
}

fn show(a: &Adapter) {
    let row = |k: &str, v: String| println!("{k:<14}{v}");
    row("Name", a.name.clone());
    row("Device", a.device.clone());
    row("Type", a.kind.label().into());
    row("Status", a.status().into());
    row("MAC address", describe_mac(a.mac));
    for (ip, p) in &a.ipv4 {
        row("IPv4", format!("{ip}/{p}  (mask {})", adapters::prefix_to_mask(*p)));
    }
    for (ip, p) in &a.ipv6 {
        row("IPv6", format!("{ip}/{p}"));
    }
    row("Gateway", a.gateway.map_or("—".into(), |g| g.to_string()));
    row(
        "DNS",
        if a.dns.is_empty() {
            "—".into()
        } else {
            a.dns.iter().map(IpAddr::to_string).collect::<Vec<_>>().join(", ")
        },
    );
    row(
        "Configured",
        match a.dhcp {
            Some(true) => "automatically (DHCP)".into(),
            Some(false) => "by hand (static)".into(),
            None => "—".into(),
        },
    );
    if let Some(s) = a.speed_bps {
        row("Link speed", format_speed(s));
    }
    if let Some(m) = a.mtu {
        row("MTU", m.to_string());
    }
    if let (Some(rx), Some(tx)) = (a.rx_bytes, a.tx_bytes) {
        row("Traffic", format!("{} received, {} sent", format_bytes(rx), format_bytes(tx)));
    }
}

fn wifi_cmd(c: WifiCmd, json: bool) -> Result<()> {
    match c {
        WifiCmd::Status => {
            let list = wifi::current()?;
            if json {
                return print_json(&list);
            }
            if list.is_empty() {
                println!("Not connected to a Wi-Fi network.");
            }
            for c in list {
                let ssid = c.ssid.clone().unwrap_or_else(|| "(name hidden by the system)".into());
                println!("{}: {ssid}", c.interface);
                if let Some(q) = c.quality() {
                    let dbm = c.rssi.map_or(String::new(), |r| format!(", {r} dBm"));
                    println!("  Signal    {q} % ({}{dbm})", wifi::quality_label(q));
                }
                if let Some(ch) = c.channel {
                    println!("  Channel   {ch} ({})", c.band.clone().unwrap_or_default());
                }
                for (k, v) in [("Security", c.security), ("Standard", c.standard), ("BSSID", c.bssid)] {
                    if let Some(v) = v {
                        println!("  {k:<9} {v}");
                    }
                }
                if let Some(r) = c.rate_mbps {
                    println!("  Rate      {r} Mbit/s");
                }
            }
        }
        WifiCmd::Passwords { admin } => {
            let mut list = if admin { wifi::saved_as_admin()? } else { wifi::saved()? };
            if cfg!(target_os = "macos") {
                eprintln!("macOS asks for your password before it shows each Wi-Fi password.");
                for n in &mut list {
                    n.password = wifi::password(n).unwrap_or(None);
                    n.password_read = true;
                }
            }
            if json {
                return print_json(&list);
            }
            for n in &list {
                let pw = match (&n.password, n.password_read) {
                    (Some(p), _) => p.clone(),
                    (None, true) => "(none)".into(),
                    (None, false) => "(hidden — try --admin)".into(),
                };
                println!("{:<32} {:<16} {pw}", n.ssid, n.security.as_deref().unwrap_or(""));
            }
        }
        WifiCmd::Nearby => {
            let list = wifi::nearby()?;
            if json {
                return print_json(&list);
            }
            for n in list {
                println!(
                    "{} {:<32} {:>4} %  ch {:<4} {:<8} {}",
                    if n.connected { "*" } else { " " },
                    n.ssid.as_deref().unwrap_or("(hidden)"),
                    n.signal.unwrap_or(0),
                    n.channel.map_or("".into(), |c| c.to_string()),
                    n.band.as_deref().unwrap_or(""),
                    n.security.as_deref().unwrap_or("")
                );
            }
        }
        WifiCmd::Qr { ssid } => {
            let n = wifi::saved()?
                .into_iter()
                .find(|n| n.ssid == ssid)
                .with_context(|| format!("\"{ssid}\" is not a saved network"))?;
            let pw = if n.password_read { n.password.clone() } else { wifi::password(&n)? };
            let text = wifi::qr_text(&n.ssid, n.security.as_deref(), pw.as_deref(), n.hidden);
            let rows = wifi::qr_modules(&text)?;
            // Two rows per line with half blocks, with a light border.
            let w = rows.len() + 4;
            let at = |y: isize, x: isize| -> bool {
                y >= 2
                    && x >= 2
                    && (y as usize - 2) < rows.len()
                    && (x as usize - 2) < rows.len()
                    && rows[y as usize - 2][x as usize - 2]
            };
            for y in (0..w as isize).step_by(2) {
                let line: String = (0..w as isize)
                    .map(|x| match (at(y, x), at(y + 1, x)) {
                        (false, false) => '█',
                        (true, false) => '▄',
                        (false, true) => '▀',
                        (true, true) => ' ',
                    })
                    .collect();
                println!("{line}");
            }
            println!("Scan with a phone camera to join \"{}\".", n.ssid);
        }
    }
    Ok(())
}

fn profile_cmd(c: ProfileCmd, json: bool) -> Result<()> {
    match c {
        ProfileCmd::List => {
            let all = profiles::load()?;
            if json {
                return print_json(&all);
            }
            if all.is_empty() {
                println!("No profiles yet. Save one with: netmgr profile save <name> --from <adapter>");
            }
            for p in all {
                println!("{:<20} {:<16} {}", p.name, p.adapter, p.settings.summary());
            }
        }
        ProfileCmd::Save { name, from } => {
            let a = adapters::find(&from)?;
            let p = Profile {
                name: name.clone(),
                adapter: a.name.clone(),
                settings: IpSettings::current(&a),
                note: String::new(),
            };
            println!("Saved \"{name}\": {}", p.settings.summary());
            profiles::upsert(p)?;
        }
        ProfileCmd::Apply { name, adapter } => {
            let p = profiles::find(&name)?;
            let target = adapter.unwrap_or_else(|| p.adapter.clone());
            let a = if target.is_empty() {
                adapters::default_adapter().context("say which adapter with --adapter")?
            } else {
                adapters::find(&target)?
            };
            p.apply(&a)?;
            println!("Applied \"{}\" to {}: {}", p.name, a.name, p.settings.summary());
        }
        ProfileCmd::Delete { name } => {
            if profiles::remove(&name)? {
                println!("Deleted \"{name}\".");
            } else {
                bail!("no profile called \"{name}\"");
            }
        }
    }
    Ok(())
}
