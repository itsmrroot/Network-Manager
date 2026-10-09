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
use netmgr::{internet, plan, scan, snmp, tools, wifi};

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
    /// Plan subnets for groups of devices: netmgr plan 10.10.0.0/16 Rooms=12x25 IoT=150 Guests=200
    Plan {
        /// The address space to divide, e.g. 10.10.0.0/16 (default: the smallest 10.x block that fits).
        #[arg(long, short)]
        space: Option<String>,
        /// NAME=DEVICES, or NAME=COUNTxDEVICES for several networks of the same size.
        #[arg(required = true)]
        groups: Vec<String>,
        /// Extra room for growth, in percent.
        #[arg(long, default_value_t = 20)]
        growth: u32,
        /// VLAN of the first network (0: no VLANs).
        #[arg(long, default_value_t = 10)]
        vlan: u16,
        /// Between consecutive VLANs.
        #[arg(long, default_value_t = 10)]
        vlan_step: u16,
        /// A /64 per network from this IPv6 prefix, e.g. 2001:db8:abcd::/48 ("ula": a random private one).
        #[arg(long)]
        ipv6: Option<String>,
        /// Print the configuration instead: cisco, juniper, aruba or mikrotik.
        #[arg(long, value_name = "VENDOR")]
        config: Option<String>,
        /// Same as --config cisco.
        #[arg(long)]
        cisco: bool,
        /// Print CSV instead.
        #[arg(long)]
        csv: bool,
    },
    /// Read a switch or router over SNMP: its name, uptime and interfaces, or walk any OID.
    Snmp {
        /// Address or name (with :port when not 161).
        host: String,
        #[arg(long, short, default_value = "public")]
        community: String,
        /// Use SNMP v1 instead of v2c.
        #[arg(long)]
        v1: bool,
        /// Walk this OID instead, e.g. 1.3.6.1.2.1.1.
        #[arg(long)]
        walk: Option<String>,
    },
    /// Capture packets on an adapter into a pcap file for Wireshark (needs administrator rights).
    Capture {
        /// The adapter, e.g. en0, eth0 (Windows: its name or IPv4 address).
        #[arg(long, short)]
        interface: String,
        #[arg(long, short, default_value = "capture.pcap")]
        out: std::path::PathBuf,
        /// Stop after this many seconds.
        #[arg(long, default_value_t = 60)]
        seconds: u64,
        /// Stop when this file appears.
        #[arg(long, hide = true)]
        stop: Option<std::path::PathBuf>,
        /// Stop when this process ends.
        #[arg(long, hide = true)]
        parent: Option<u32>,
        /// Stop when the file reaches this many megabytes.
        #[arg(long, default_value_t = 500)]
        max_mb: u64,
    },
    /// Compare this computer's clock with time servers (NTP).
    Time {
        /// Servers to ask (default: well-known public ones).
        servers: Vec<String>,
    },
    /// The largest packet that reaches a host without being split (path MTU).
    Mtu { host: String },
    /// Services devices announce on the local network (Bonjour / mDNS).
    Bonjour {
        /// Seconds to listen.
        #[arg(long, default_value_t = 4)]
        seconds: u64,
    },
    /// Back up the configuration of the saved sessions that have a backup command.
    Backup {
        /// Only these sessions (names).
        names: Vec<String>,
        /// Read the password for the devices from standard input.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Is an IPv4 address free, in use, or used by two devices (IP conflict)?
    CheckIp { address: Ipv4Addr },
    /// LLDP and CDP neighbors, and learned MAC addresses, of a switch (SNMP).
    Neighbors {
        host: String,
        #[arg(long, short, default_value = "public")]
        community: String,
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
    /// Which switch, port and VLAN this computer is plugged into (LLDP/CDP).
    SwitchPort {
        /// The adapter's device name (default: the wired default adapter).
        #[arg(long)]
        interface: Option<String>,
        /// How long to listen (CDP is sent every 60 s).
        #[arg(long, default_value_t = 65)]
        seconds: u64,
    },
    /// Find the DHCP servers on the network (and rogue ones).
    DhcpTest {
        #[arg(long)]
        interface: Option<String>,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Ping several hosts at once, continuously, with loss and jitter.
    Monitor {
        hosts: Vec<String>,
        /// Seconds between rounds.
        #[arg(long, default_value_t = 1)]
        interval: u64,
    },
    /// Path analysis like MTR: loss and delay at every router on the way.
    Mtr {
        host: String,
        /// Rounds to send (0 = until Ctrl+C).
        #[arg(short, long, default_value_t = 10)]
        count: u64,
    },
    /// Run a TFTP server for firmware and configuration files.
    TftpServer {
        /// The folder to serve.
        folder: std::path::PathBuf,
        #[arg(long, default_value_t = 69)]
        port: u16,
        /// Let devices upload (configuration backups).
        #[arg(long)]
        allow_upload: bool,
    },
    /// Receive and print syslog messages from devices.
    SyslogServer {
        #[arg(long, default_value_t = 514)]
        port: u16,
    },
    /// Throughput test between two computers: "server", or a host to test to.
    Throughput {
        target: String,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Measure from the server to this computer.
        #[arg(long)]
        download: bool,
    },
    /// Inspect a server's TLS certificate (host or host:port).
    Tls { target: String },
    /// Follow a web address's redirects and show its headers.
    Http { url: String },
    /// WHOIS (RDAP) for a domain, an IP address or an AS number.
    Whois { query: String },
    /// Open connections and listening ports, with their programs.
    Connections {
        /// Only listening ports.
        #[arg(long)]
        listening: bool,
    },
    /// The route table; add or delete static routes.
    Routes {
        #[command(subcommand)]
        command: Option<RouteCmd>,
    },
    /// Show the hosts file.
    Hosts,
    /// List serial ports (console cables).
    SerialPorts,
    /// Write a network report (Markdown) for a support ticket.
    Report {
        /// The file to write (default: print it).
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum RouteCmd {
    /// Add a static route, e.g. 10.20.0.0/16 via 192.168.1.254.
    Add {
        network: String,
        gateway: IpAddr,
        /// Keep it after a restart (Windows).
        #[arg(long)]
        persistent: bool,
    },
    /// Delete a route.
    Delete { network: String },
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
        Cmd::Plan { space, groups, growth, vlan, vlan_step, ipv6, config, cisco, csv } => {
            let groups = groups.iter().map(|g| plan::Group::parse(g)).collect::<Result<Vec<_>>>()?;
            let opt = plan::Options { growth, first_vlan: vlan, vlan_step };
            let space = match space {
                Some(s) => Subnet::parse(&s)?,
                None => Subnet::new(Ipv4Addr::new(10, 0, 0, 0), plan::size(&groups, &opt).1.max(8))?,
            };
            let ipv6 = match ipv6.as_deref() {
                Some("ula") => plan::random_ula(),
                Some(p) => p.to_string(),
                None => String::new(),
            };
            let vendor = match (config, cisco) {
                (Some(v), _) => Some(plan::Vendor::parse(&v)?),
                (None, true) => Some(plan::Vendor::Cisco),
                (None, false) => None,
            };
            let p = match plan::plan_dual(&space, &ipv6, &groups, &opt)? {
                Ok(p) => p,
                Err(plan::PlanError::Ipv6TooSmall { room }) => {
                    bail!("{ipv6} has room for only {room} /64 networks: use a shorter prefix such as /48")
                }
                Err(plan::PlanError::Empty) => bail!("give at least one network with devices, e.g. Office=40"),
                Err(plan::PlanError::TooSmall { needed, prefix }) => bail!(
                    "these networks need {needed} addresses (a /{prefix}), but {}/{} has {}",
                    space.network,
                    space.prefix,
                    1u64 << (32 - space.prefix)
                ),
            };
            if json {
                return print_json(&p);
            }
            if let Some(v) = vendor {
                print!("{}", p.config(v));
            } else if csv {
                print!("{}", p.csv());
            } else {
                print!("{}", p.table());
                let room = 1u64 << (32 - space.prefix);
                println!(
                    "\n{} networks in {}/{}: {} of {} addresses used ({}%).",
                    p.networks.len(),
                    space.network,
                    space.prefix,
                    p.used,
                    room,
                    p.used * 100 / room
                );
                if !p.free.is_empty() {
                    let free: Vec<String> = p.free.iter().map(|s| format!("{}/{}", s.network, s.prefix)).collect();
                    println!("Free: {}", free.join(", "));
                }
            }
        }
        Cmd::Snmp { host, community, v1, walk } => {
            let c = snmp::Client::new(&host, &community, if v1 { snmp::Version::V1 } else { snmp::Version::V2c })?;
            if let Some(root) = walk {
                let rows = c.walk(&snmp::Oid::parse(&root)?, 100_000)?;
                if json {
                    let list: Vec<serde_json::Value> = rows
                        .iter()
                        .map(|(o, v)| serde_json::json!({"oid": o.to_string(), "type": v.kind(), "value": v.to_string()}))
                        .collect();
                    return print_json(&list);
                }
                for (o, v) in &rows {
                    println!("{o} = {}: {v}", v.kind());
                }
                return Ok(());
            }
            let sys = snmp::system(&c)?;
            let ifaces = snmp::interfaces(&c)?;
            if json {
                return print_json(&serde_json::json!({"system": sys, "interfaces": ifaces}));
            }
            println!("Name         {}", sys.name);
            println!("Description  {}", sys.description.lines().next().unwrap_or(""));
            if let Some(t) = sys.uptime {
                println!("Uptime       {}", snmp::uptime(t));
            }
            println!("Location     {}", sys.location);
            println!("Contact      {}", sys.contact);
            println!();
            println!(
                "{:>5}  {:<14} {:<6} {:>10}  {:>14}  {:>14}  {:>8}  Description",
                "Index", "Name", "State", "Speed", "In", "Out", "Errors"
            );
            for i in &ifaces {
                let state = match (i.admin_up, i.oper_up) {
                    (false, _) => "off",
                    (true, true) => "up",
                    (true, false) => "down",
                };
                println!(
                    "{:>5}  {:<14} {:<6} {:>10}  {:>14}  {:>14}  {:>8}  {}",
                    i.index,
                    i.name,
                    state,
                    format_speed(i.speed),
                    format_bytes(i.in_octets),
                    format_bytes(i.out_octets),
                    i.in_errors + i.out_errors,
                    i.alias
                );
            }
        }
        Cmd::Capture { interface, out, seconds, stop, parent, max_mb } => {
            let stop = stop.unwrap_or_else(|| out.with_extension("stop"));
            let _ = std::fs::remove_file(&stop);
            if stop.parent() == out.parent() && parent.is_none() {
                eprintln!("Capturing on {interface} for {seconds} s into {} …", out.display());
            }
            netmgr::capture::run(&interface, &out, &stop, parent, seconds, max_mb * 1_000_000)?;
            if parent.is_none() {
                let (link, frames) = netmgr::capture::read_file(&out)?;
                println!("{} packets saved to {}", frames.len(), out.display());
                if !json {
                    for f in frames.iter().take(20) {
                        let s = netmgr::capture::decode(link, &f.data);
                        println!("  {:<6} {} → {}  {}", s.protocol, s.source, s.destination, s.info);
                    }
                }
            }
        }
        Cmd::Time { servers } => {
            let list: Vec<String> = if servers.is_empty() {
                netmgr::ntp::SERVERS.iter().map(|(_, h)| h.to_string()).collect()
            } else {
                servers
            };
            let results: Vec<(String, Result<netmgr::ntp::Reading>)> = std::thread::scope(|s| {
                let handles: Vec<_> = list
                    .iter()
                    .map(|h| (h.clone(), s.spawn(move || netmgr::ntp::query(h, Duration::from_secs(3)))))
                    .collect();
                handles
                    .into_iter()
                    .map(|(h, j)| (h, j.join().unwrap_or_else(|_| Err(anyhow::anyhow!("failed")))))
                    .collect()
            });
            if json {
                let v: Vec<serde_json::Value> = results
                    .iter()
                    .map(|(h, r)| match r {
                        Ok(r) => serde_json::json!({"server": h, "reading": r}),
                        Err(e) => serde_json::json!({"server": h, "error": format!("{e:#}")}),
                    })
                    .collect();
                return print_json(&v);
            }
            println!("{:<22} {:>14} {:>9} {:>8}  Reference", "Server", "This clock", "Delay", "Stratum");
            for (h, r) in &results {
                match r {
                    Ok(r) => println!(
                        "{:<22} {:>14} {:>7.0} ms {:>8}  {}",
                        h,
                        netmgr::ntp::describe_offset(r.offset),
                        r.delay * 1000.0,
                        r.stratum,
                        r.reference
                    ),
                    Err(e) => println!("{h:<22} {e:#}"),
                }
            }
        }
        Cmd::Mtu { host } => {
            let stop = AtomicBool::new(false);
            let r = tools::path_mtu(&host, &stop, |size, fits| {
                if !json {
                    let what = match fits {
                        tools::Fits::Yes => "fits",
                        tools::Fits::TooBig => "too big",
                        tools::Fits::NoAnswer => "no answer",
                    };
                    println!("  {:>5} bytes  {what}", size + 28);
                }
            })?;
            if json {
                return print_json(&r);
            }
            println!("Path MTU to {} ({}): {} bytes{}", host, r.host, r.mtu, if r.at_most { " or more" } else { "" });
        }
        Cmd::Bonjour { seconds } => {
            let list = netmgr::mdns::browse(Duration::from_secs(seconds))?;
            if json {
                return print_json(&list);
            }
            for s in &list {
                let what = netmgr::mdns::describe(&s.kind);
                let addrs: Vec<String> = s.addresses.iter().map(|a| a.to_string()).collect();
                println!(
                    "{:<28} {:<26} {}:{}  {}",
                    s.name,
                    if what.is_empty() { s.kind.as_str() } else { what },
                    s.host,
                    s.port,
                    addrs.join(" ")
                );
            }
            println!("{} services", list.len());
        }
        Cmd::Backup { names, password_stdin } => {
            let password = if password_stdin {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                Some(line.trim_end_matches(['\r', '\n']).to_string())
            } else {
                None
            };
            let list: Vec<_> = netmgr::remote::load()?
                .into_iter()
                .filter(|s| !s.backup_command.trim().is_empty())
                .filter(|s| names.is_empty() || names.iter().any(|n| n.eq_ignore_ascii_case(&s.name)))
                .collect();
            if list.is_empty() {
                bail!("no saved session has a backup command (set one in the app: Console → SSH and Telnet → Edit)");
            }
            let mut failed = 0;
            for s in &list {
                let r = netmgr::backup::fetch(s, password.as_deref())
                    .and_then(|text| netmgr::backup::store(&s.name, &text, std::time::SystemTime::now()));
                match r {
                    Ok(netmgr::backup::Outcome::First) => println!("{:<24} first copy saved", s.name),
                    Ok(netmgr::backup::Outcome::Unchanged) => println!("{:<24} unchanged", s.name),
                    Ok(netmgr::backup::Outcome::Changed { added, removed }) => {
                        println!("{:<24} changed: +{added} -{removed} lines", s.name)
                    }
                    Err(e) => {
                        failed += 1;
                        println!("{:<24} failed: {e:#}", s.name);
                    }
                }
            }
            println!("Copies are in {}", netmgr::backup::folder().display());
            if failed > 0 {
                bail!("{failed} of {} backups failed", list.len());
            }
        }
        Cmd::CheckIp { address } => {
            let r = scan::check_address(address)?;
            if json {
                return print_json(&r);
            }
            if r.conflict() {
                let macs: Vec<String> = r.macs.iter().map(|m| m.to_string()).collect();
                println!("IP CONFLICT: {} devices answer for {address}: {}", r.macs.len(), macs.join(", "));
            } else if r.free() {
                println!("{address} looks free: nothing answered.");
            } else {
                println!("{address} is in use.");
                if let Some(m) = r.macs.first() {
                    println!("  MAC     {m}  {}", r.vendor.as_deref().unwrap_or(""));
                }
                if let Some(n) = &r.name {
                    println!("  Name    {n}");
                }
                if !r.open_ports.is_empty() {
                    let p: Vec<String> = r.open_ports.iter().map(u16::to_string).collect();
                    println!("  Ports   {}", p.join(", "));
                }
            }
        }
        Cmd::Neighbors { host, community } => {
            let c = snmp::Client::new(&host, &community, snmp::Version::V2c)?;
            let n = snmp::neighbors(&c)?;
            let macs = snmp::mac_ports(&c).unwrap_or_default();
            if json {
                return print_json(&serde_json::json!({"neighbors": n, "macs": macs}));
            }
            println!("{:<16} {:<24} {:<18} {:<16} Protocol", "Local port", "Neighbor", "Its port", "Address");
            for x in &n {
                println!(
                    "{:<16} {:<24} {:<18} {:<16} {}",
                    x.local_port,
                    x.name,
                    x.port,
                    x.address.map(|a| a.to_string()).unwrap_or_default(),
                    x.protocol
                );
            }
            if !macs.is_empty() {
                println!();
                println!("{:<18} Port", "MAC address");
                for (m, p) in &macs {
                    println!("{m:<18} {p}");
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
                    println!("{} {:<12} {}", if c.ok { "✔" } else { "✘" }, c.title, c.detail());
                    if let Some(a) = &c.advice {
                        println!("  → {a}");
                    }
                }
            });
            if json {
                return print_json(&checks);
            }
        }
        Cmd::SwitchPort { interface, seconds } => {
            let iface = match interface {
                Some(i) => i,
                None => adapters::list(false)?
                    .into_iter()
                    .find(|a| a.up && a.kind == adapters::Kind::Ethernet)
                    .or_else(adapters::default_adapter)
                    .map(|a| if cfg!(windows) { a.name } else { a.device })
                    .context("no connected adapter")?,
            };
            if !json {
                eprintln!("Listening on {iface} for up to {seconds} s (switches announce themselves every 30–60 s)…");
            }
            let found = if netmgr::cmd::is_admin() || cfg!(windows) {
                netmgr::discovery::capture(&iface, seconds)?
            } else {
                netmgr::discovery::listen(&iface, seconds)?
            };
            if json {
                return print_json(&found);
            }
            if found.is_empty() {
                println!(
                    "No LLDP or CDP announcement was heard. The switch may have them turned off, or this is not a wired port."
                );
            }
            for n in found {
                println!("{} from {}", n.protocol, n.system_name.as_deref().unwrap_or("?"));
                for (k, v) in [
                    ("Port", n.port_id.clone()),
                    ("Port description", n.port_description.clone()),
                    ("VLAN", n.vlan.map(|v| v.to_string())),
                    ("Voice VLAN", n.voice_vlan.map(|v| v.to_string())),
                    ("Management", (!n.management.is_empty()).then(|| n.management.join(", "))),
                    ("Platform", n.platform.clone()),
                    ("Link", n.link.clone().or(n.duplex.clone())),
                    ("PoE", n.poe.clone()),
                    ("Capabilities", (!n.capabilities.is_empty()).then(|| n.capabilities.join(", "))),
                    ("Software", n.description.clone().map(|d| d.lines().next().unwrap_or("").to_string())),
                ] {
                    if let Some(v) = v {
                        println!("  {k:<17} {v}");
                    }
                }
            }
        }
        Cmd::DhcpTest { interface, seconds } => {
            let r = if netmgr::cmd::is_admin() || cfg!(windows) {
                netmgr::dhcp::test(interface.as_deref(), seconds)?
            } else {
                netmgr::dhcp::test_elevated(interface.as_deref(), seconds)?
            };
            if json {
                return print_json(&r);
            }
            let servers = r.servers();
            println!("{} DHCP server(s) answered (asked for {})", servers.len(), r.mac);
            if servers.len() > 1 {
                println!("⚠  More than one DHCP server: one of them is probably a rogue server.");
            }
            for o in &r.offers {
                println!(
                    "\nServer {} ({} ms){}",
                    o.server.map_or("?".into(), |s| s.to_string()),
                    o.millis,
                    o.relay.map(|r| format!(" via relay {r}")).unwrap_or_default()
                );
                println!(
                    "  Offers     {}{}",
                    o.address.map_or("—".into(), |a| a.to_string()),
                    o.mask.map(|m| format!(" / {m}")).unwrap_or_default()
                );
                let list = |v: &[Ipv4Addr]| v.iter().map(Ipv4Addr::to_string).collect::<Vec<_>>().join(", ");
                println!("  Router     {}", list(&o.routers));
                println!("  DNS        {}", list(&o.dns));
                if let Some(d) = &o.domain {
                    println!("  Domain     {d}");
                }
                if let Some(l) = o.lease_seconds {
                    println!("  Lease      {} h", l / 3600);
                }
                if let Some(t) = &o.tftp_server {
                    println!("  Boot       {t} {}", o.boot_file.clone().unwrap_or_default());
                }
            }
        }
        Cmd::Monitor { hosts, interval } => {
            anyhow::ensure!(!hosts.is_empty(), "give one or more hosts");
            let targets: Vec<(String, IpAddr)> =
                hosts.iter().map(|h| Ok((h.clone(), tools::resolve(h, 0)?.ip()))).collect::<Result<_>>()?;
            let mut stats = vec![netmgr::monitor::Stats::default(); targets.len()];
            loop {
                let results: Vec<_> = std::thread::scope(|s| {
                    let hs: Vec<_> = targets
                        .iter()
                        .map(|(_, ip)| s.spawn(move || netmgr::icmp::ping(*ip, Duration::from_secs(1)).ok().flatten()))
                        .collect();
                    hs.into_iter().map(|h| h.join().ok().flatten()).collect()
                });
                for (st, r) in stats.iter_mut().zip(results) {
                    st.add(r);
                }
                print!("\x1b[2J\x1b[H");
                println!("{:<28} {:>6} {:>6} {:>8} {:>8} {:>8}", "Host", "Sent", "Loss", "Last", "Avg", "Jitter");
                for ((name, _), st) in targets.iter().zip(&stats) {
                    let ms = |v: Option<f64>| v.map_or("—".into(), |v| format!("{v:.1}"));
                    println!(
                        "{name:<28} {:>6} {:>5.0}% {:>8} {:>8} {:>8}",
                        st.sent,
                        st.loss_percent(),
                        ms(st.last),
                        ms(st.avg()),
                        ms(st.jitter())
                    );
                }
                std::thread::sleep(Duration::from_secs(interval.max(1)));
            }
        }
        Cmd::Mtr { host, count } => {
            let cancel = AtomicBool::new(false);
            let mut hops: Vec<(u32, Option<IpAddr>)> = Vec::new();
            eprintln!("Finding the route…");
            let target = netmgr::monitor::discover_path(&host, &cancel, |n, ip| hops.push((n, ip)))?;
            if hops.last().is_none_or(|(_, ip)| *ip != Some(target)) {
                hops.push((hops.len() as u32 + 1, Some(target)));
            }
            let mut stats = vec![netmgr::monitor::Stats::default(); hops.len()];
            let mut round = 0;
            while count == 0 || round < count {
                round += 1;
                let results: Vec<Option<Duration>> = std::thread::scope(|s| {
                    let hs: Vec<_> = hops
                        .iter()
                        .map(|(_, ip)| {
                            s.spawn(move || {
                                ip.and_then(|ip| netmgr::icmp::ping(ip, Duration::from_secs(1)).ok().flatten())
                            })
                        })
                        .collect();
                    hs.into_iter().map(|h| h.join().ok().flatten()).collect()
                });
                for ((st, r), (_, ip)) in stats.iter_mut().zip(results).zip(&hops) {
                    if ip.is_some() {
                        st.add(r);
                    }
                }
                print!("\x1b[2J\x1b[H");
                println!("Path to {host} ({target}), round {round}\n");
                println!(
                    "{:>3}  {:<40} {:>6} {:>6} {:>8} {:>8} {:>8}",
                    "#", "Router", "Loss", "Sent", "Last", "Avg", "Worst"
                );
                for ((n, ip), st) in hops.iter().zip(&stats) {
                    let ms = |v: Option<f64>| v.map_or("—".into(), |v| format!("{v:.1}"));
                    let name = ip.map_or("(no answer)".to_string(), |i| i.to_string());
                    println!(
                        "{n:>3}  {name:<40} {:>5.0}% {:>6} {:>8} {:>8} {:>8}",
                        st.loss_percent(),
                        st.sent,
                        ms(st.last),
                        ms(st.avg()),
                        ms(st.max)
                    );
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        Cmd::TftpServer { folder, port, allow_upload } => {
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let opts = netmgr::servers::TftpOptions { root: folder, port, allow_upload, overwrite: false };
            netmgr::servers::tftp_serve(opts, Default::default(), stop, std::sync::Arc::new(|l| println!("{l}")))?;
        }
        Cmd::SyslogServer { port } => {
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            println!("Listening for syslog on UDP port {port}…");
            netmgr::servers::syslog_serve(
                port,
                stop,
                std::sync::Arc::new(move |m| {
                    if json {
                        println!("{}", serde_json::to_string(&m).unwrap_or_default());
                    } else {
                        println!(
                            "{:<15} {:<9} {}",
                            m.from,
                            netmgr::servers::SEVERITIES[m.severity as usize % 8],
                            m.text
                        );
                    }
                }),
            )?;
        }
        Cmd::Throughput { target, seconds, download } => {
            let port = netmgr::servers::THROUGHPUT_PORT;
            if target == "server" {
                let stop = std::sync::Arc::new(AtomicBool::new(false));
                netmgr::servers::throughput_serve(port, stop, std::sync::Arc::new(|l| println!("{l}")))?;
            } else {
                let dir =
                    if download { netmgr::servers::Direction::Download } else { netmgr::servers::Direction::Upload };
                let cancel = AtomicBool::new(false);
                let r = netmgr::servers::throughput_test(&target, port, dir, seconds, &cancel, &|v| {
                    eprint!("\r{v:8.1} Mbit/s")
                })?;
                eprintln!();
                println!("{} {r:.1} Mbit/s", if download { "Download" } else { "Upload" });
            }
        }
        Cmd::Tls { target } => {
            let r = netmgr::web::tls(&target)?;
            if json {
                return print_json(&r);
            }
            println!("{} ({}) — {}, {}, {} ms", r.host, r.address, r.version, r.cipher, r.millis);
            println!("{}", r.problem.as_deref().map_or("✔ Trusted".to_string(), |p| format!("✘ {p}")));
            for (i, c) in r.chain.iter().enumerate() {
                println!("\n[{i}] {}\n    issued by {}", c.subject, c.issuer);
                println!(
                    "    valid {} to {} ({} days left)",
                    netmgr::web::date(c.not_before),
                    netmgr::web::date(c.not_after),
                    c.days_left()
                );
                println!("    {} · {}", c.key, c.signature);
                if !c.names.is_empty() {
                    println!("    names: {}", c.names.join(", "));
                }
            }
        }
        Cmd::Http { url } => {
            let hops = netmgr::web::http(&url)?;
            if json {
                return print_json(&hops);
            }
            for h in hops {
                println!("{} {} ({} ms)", h.status, h.url, h.millis);
                for (k, v) in h.headers {
                    println!("    {k}: {v}");
                }
            }
        }
        Cmd::Whois { query } => {
            let w = netmgr::web::whois(&query)?;
            if json {
                return print_json(&w);
            }
            for (k, v) in [
                ("Type", Some(w.kind.clone())),
                ("Name", w.name.clone()),
                ("Handle", w.handle.clone()),
                ("Registrar", w.registrar.clone()),
                ("Organisation", w.organisation.clone()),
                ("Range", w.range.clone()),
                ("Country", w.country.clone()),
                ("Registered", w.registered.clone()),
                ("Changed", w.changed.clone()),
                ("Expires", w.expires.clone()),
                ("Name servers", (!w.nameservers.is_empty()).then(|| w.nameservers.join(", "))),
                ("Status", (!w.status.is_empty()).then(|| w.status.join(", "))),
                ("Abuse", w.abuse.clone()),
            ] {
                if let Some(v) = v {
                    println!("{k:<14}{v}");
                }
            }
        }
        Cmd::Connections { listening } => {
            let mut list = netmgr::system::connections()?;
            if listening {
                list.retain(|c| c.listening());
            }
            if json {
                return print_json(&list);
            }
            for c in list {
                println!(
                    "{:<4} {:<40} {:<40} {:<12} {}",
                    c.proto,
                    format!("{}:{}", c.local, c.local_port.map_or("*".into(), |p| p.to_string())),
                    c.remote,
                    c.state,
                    c.process.map(|p| format!("{p} ({})", c.pid.unwrap_or(0))).unwrap_or_default()
                );
            }
        }
        Cmd::Routes { command } => match command {
            None => {
                let r = netmgr::system::routes()?;
                if json {
                    return print_json(&r);
                }
                for rt in r {
                    println!(
                        "{:<40} {:<40} {:<12} {}",
                        rt.destination,
                        rt.gateway,
                        rt.interface,
                        rt.metric.map(|m| m.to_string()).unwrap_or_default()
                    );
                }
            }
            Some(RouteCmd::Add { network, gateway, persistent }) => {
                netmgr::system::add_route(&network, gateway, persistent)?;
                println!("Route to {network} via {gateway} added.");
            }
            Some(RouteCmd::Delete { network }) => {
                netmgr::system::delete_route(&network)?;
                println!("Route to {network} deleted.");
            }
        },
        Cmd::Hosts => {
            let lines = netmgr::system::read_hosts()?;
            if json {
                return print_json(&lines);
            }
            println!("{}", netmgr::system::format_hosts(&lines));
        }
        Cmd::SerialPorts => {
            let ports = netmgr::console::ports();
            if json {
                return print_json(&ports);
            }
            if ports.is_empty() {
                println!("No serial ports. Plug in the console cable (a driver may be needed).");
            }
            for p in ports {
                println!("{:<30} {}", p.name, p.description);
            }
        }
        Cmd::Report { output } => {
            let text = netmgr::report::build(true, &|s| eprintln!("{s}…"));
            match output {
                Some(path) => {
                    std::fs::write(&path, text)?;
                    println!("Saved to {}", path.display());
                }
                None => println!("{text}"),
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
                extras: Default::default(),
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
