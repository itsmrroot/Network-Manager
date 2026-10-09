//! Network planner: gives every group of devices (rooms, IoT, guests, …) its
//! own subnet, sized for its devices plus room to grow, and lays them out in an
//! address space without gaps (VLSM). Optionally a /64 per network from an
//! IPv6 prefix, and the configuration for the switch or router.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, Ipv6Addr};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::subnet::Subnet;

/// Networks of the same kind: `count` of them with `devices` devices each,
/// e.g. 12 rooms of 25 PCs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    pub count: u32,
    pub devices: u32,
}

impl Group {
    /// Parses `Rooms=12x25` (12 networks of 25 devices) or `IoT=150`.
    pub fn parse(s: &str) -> Result<Self> {
        let (name, size) =
            s.rsplit_once('=').with_context(|| format!("\"{s}\": write NAME=DEVICES or NAME=COUNTxDEVICES"))?;
        let (count, devices) = match size.split_once(['x', 'X', '*']) {
            Some((c, d)) => (c.trim().parse()?, d.trim().parse()?),
            None => (1, size.trim().parse().with_context(|| format!("\"{size}\" is not a number of devices"))?),
        };
        ensure!(!name.trim().is_empty(), "\"{s}\": give the network a name");
        Ok(Self { name: name.trim().to_string(), count, devices })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// Extra addresses for growth, in percent of the devices.
    pub growth: u32,
    /// VLAN of the first network; 0 for none.
    pub first_vlan: u16,
    /// Between the VLANs of consecutive networks.
    pub vlan_step: u16,
}

impl Default for Options {
    fn default() -> Self {
        Self { growth: 20, first_vlan: 10, vlan_step: 10 }
    }
}

/// Everything the planner was given, as saved by the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Input {
    /// Empty: the smallest 10.x.x.x block that fits.
    pub space: String,
    /// Empty: IPv4 only.
    pub ipv6: String,
    pub groups: Vec<Group>,
    pub options: Options,
}

/// One planned network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Network {
    pub name: String,
    /// 0 when VLANs are not numbered.
    pub vlan: u16,
    pub devices: u32,
    /// Addresses it needs: devices, growth and the gateway.
    pub needed: u64,
    pub subnet: Subnet,
    /// The first usable address.
    pub gateway: Ipv4Addr,
    /// Addresses left over after the devices and the gateway.
    pub spare: u64,
    /// Its /64, when an IPv6 prefix was given.
    pub ipv6: Option<Ipv6Addr>,
}

impl Network {
    /// Addresses DHCP can hand out: everything after the gateway.
    pub fn dhcp_range(&self) -> Option<(Ipv4Addr, Ipv4Addr)> {
        let first = u32::from(self.gateway) + 1;
        (first <= u32::from(self.subnet.last)).then(|| (first.into(), self.subnet.last))
    }

    /// The router's IPv6 address: `::1` in the /64.
    pub fn gateway6(&self) -> Option<Ipv6Addr> {
        self.ipv6.map(|n| Ipv6Addr::from(u128::from(n) | 1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub space: Subnet,
    /// In address order, largest first.
    pub networks: Vec<Network>,
    /// Addresses taken by the networks (including network and broadcast).
    pub used: u64,
    /// The rest of the space, as the largest blocks possible.
    pub free: Vec<Subnet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Nothing to plan.
    Empty,
    /// The networks need `needed` addresses: a /`prefix`.
    TooSmall { needed: u64, prefix: u8 },
    /// The IPv6 prefix has room for only `room` /64 networks.
    Ipv6TooSmall { room: u64 },
}

/// Most networks one plan makes.
pub const MAX_NETWORKS: u64 = 4096;

/// The prefix of the smallest subnet with room for `needed` addresses
/// (devices and gateway); at most /30, so there is always a broadcast.
pub fn prefix_for(needed: u64) -> u8 {
    (0..=30u8).rev().find(|&p| (1u64 << (32 - p)) - 2 >= needed).unwrap_or(0)
}

/// Addresses a network of `devices` needs: devices, `growth` percent more,
/// and one for the gateway.
pub fn needed(devices: u32, growth: u32) -> u64 {
    let d = devices as u64;
    d + d * growth as u64 / 100 + 1
}

/// The networks of `groups`, named and numbered, before they get addresses.
fn expand(groups: &[Group], opt: &Options) -> Vec<(String, u16, u32, u64, u8)> {
    let mut vlan = opt.first_vlan as u32;
    let mut out = Vec::new();
    for g in groups.iter().filter(|g| g.count > 0 && g.devices > 0) {
        for i in 1..=g.count.min(MAX_NETWORKS as u32 + 1) {
            let name = if g.count == 1 { g.name.clone() } else { format!("{} {i}", g.name) };
            let n = needed(g.devices, opt.growth);
            let v = if opt.first_vlan == 0 || vlan > 4094 { 0 } else { vlan as u16 };
            out.push((name, v, g.devices, n, prefix_for(n)));
            vlan += opt.vlan_step.max(1) as u32;
        }
    }
    out
}

/// Addresses all the networks take, and the prefix of the smallest block
/// that holds them.
pub fn size(groups: &[Group], opt: &Options) -> (u64, u8) {
    let total: u64 = expand(groups, opt).iter().map(|n| 1u64 << (32 - n.4)).sum();
    let prefix = (0..=32u8).rev().find(|&p| (1u64 << (32 - p)) >= total).unwrap_or(0);
    (total, prefix)
}

/// Parses an IPv6 prefix of /64 or shorter, such as `2001:db8:abcd::/48`.
pub fn parse_ipv6(s: &str) -> Result<(Ipv6Addr, u8)> {
    let (a, p) =
        s.trim().split_once('/').with_context(|| format!("\"{s}\": write the prefix, e.g. 2001:db8:abcd::/48"))?;
    let addr: Ipv6Addr = a.trim().parse().with_context(|| format!("\"{a}\" is not an IPv6 address"))?;
    let prefix: u8 = p
        .trim()
        .parse()
        .ok()
        .filter(|p| *p <= 64)
        .with_context(|| format!("\"/{p}\": the prefix must be /64 or shorter"))?;
    let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
    Ok((Ipv6Addr::from(u128::from(addr) & mask), prefix))
}

/// A random unique local prefix (RFC 4193): fdXX:XXXX:XXXX::/48.
pub fn random_ula() -> String {
    let b = crate::mac::random_bytes();
    format!("fd{:02x}:{:02x}{:02x}:{:02x}{:02x}::/48", b[0], b[1], b[2], b[3], b[4])
}

/// The /64 of every network: with a /48 or shorter and numbered VLANs, the
/// VLAN number reads in the address (VLAN 120 → `…:120::/64`); otherwise
/// the networks are numbered in order from 1.
fn assign_ipv6(nets: &mut [Network], base: Ipv6Addr, prefix: u8) -> Result<(), PlanError> {
    let bits = 64 - prefix as u32;
    let room = if bits >= 63 { u64::MAX } else { 1u64 << bits };
    let by_vlan = prefix <= 48 && nets.iter().all(|n| n.vlan != 0);
    let ids: Vec<u64> = if by_vlan {
        nets.iter().map(|n| u64::from_str_radix(&n.vlan.to_string(), 16).unwrap_or(0)).collect()
    } else {
        (1..=nets.len() as u64).collect()
    };
    if ids.iter().any(|&id| id >= room) {
        return Err(PlanError::Ipv6TooSmall { room: room.saturating_sub(1) });
    }
    for (n, id) in nets.iter_mut().zip(ids) {
        n.ipv6 = Some(Ipv6Addr::from(u128::from(base) | ((id as u128) << 64)));
    }
    Ok(())
}

/// Lays the networks out in `space`, largest first so that every one is
/// aligned and nothing is wasted between them.
pub fn plan(space: &Subnet, groups: &[Group], opt: &Options) -> Result<Plan, PlanError> {
    let mut nets = expand(groups, opt);
    if nets.is_empty() {
        return Err(PlanError::Empty);
    }
    let (total, prefix) = size(groups, opt);
    let room = 1u64 << (32 - space.prefix);
    if total > room || nets.len() as u64 > MAX_NETWORKS {
        return Err(PlanError::TooSmall { needed: total, prefix });
    }
    // Stable: equal sizes keep their order (Room 1, Room 2, …).
    nets.sort_by_key(|n| n.4);
    let base = u32::from(space.network) as u64;
    let mut at = base;
    let networks = nets
        .into_iter()
        .map(|(name, vlan, devices, needed, prefix)| {
            let subnet = Subnet::new(Ipv4Addr::from(at as u32), prefix).expect("prefix ≤ 32");
            at += 1u64 << (32 - prefix);
            let spare = subnet.hosts - needed;
            Network { name, vlan, devices, needed, gateway: subnet.first, spare, subnet, ipv6: None }
        })
        .collect();
    Ok(Plan { space: space.clone(), networks, used: at - base, free: blocks(at, base + room) })
}

/// [`plan`] with a /64 per network from `ipv6` (`2001:db8:abcd::/48`).
pub fn plan_dual(space: &Subnet, ipv6: &str, groups: &[Group], opt: &Options) -> Result<Result<Plan, PlanError>> {
    let mut p = match plan(space, groups, opt) {
        Ok(p) => p,
        Err(e) => return Ok(Err(e)),
    };
    if !ipv6.trim().is_empty() {
        let (base, prefix) = parse_ipv6(ipv6)?;
        if let Err(e) = assign_ipv6(&mut p.networks, base, prefix) {
            return Ok(Err(e));
        }
    }
    Ok(Ok(p))
}

impl Input {
    /// The address space: as typed, or the smallest 10.x.x.x block that fits.
    pub fn space(&self) -> Result<Subnet> {
        if self.space.trim().is_empty() {
            let prefix = size(&self.groups, &self.options).1.max(8);
            return Subnet::new(Ipv4Addr::new(10, 0, 0, 0), prefix);
        }
        let s = Subnet::parse(&self.space)?;
        Subnet::new(s.network, s.prefix)
    }

    pub fn plan(&self) -> Result<Result<Plan, PlanError>> {
        plan_dual(&self.space()?, &self.ipv6, &self.groups, &self.options)
    }
}

/// `[from, to)` as the fewest aligned blocks.
fn blocks(mut from: u64, to: u64) -> Vec<Subnet> {
    let mut out = Vec::new();
    while from < to {
        let align = if from == 0 { 32 } else { from.trailing_zeros().min(32) };
        let mut bits = align;
        while from + (1u64 << bits) > to {
            bits -= 1;
        }
        out.push(Subnet::new(Ipv4Addr::from(from as u32), (32 - bits) as u8).expect("prefix ≤ 32"));
        from += 1u64 << bits;
    }
    out
}

/// A name a switch accepts: letters, digits, `-` and `_`.
fn config_name(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let s: String = s.trim_matches('-').chars().take(32).collect();
    if s.is_empty() { "net".into() } else { s }
}

/// Switch and router makers the planner writes configuration for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Vendor {
    #[default]
    Cisco,
    Juniper,
    Aruba,
    MikroTik,
}

impl Vendor {
    pub const ALL: [Vendor; 4] = [Vendor::Cisco, Vendor::Juniper, Vendor::Aruba, Vendor::MikroTik];

    pub fn label(self) -> &'static str {
        match self {
            Vendor::Cisco => "Cisco IOS / IOS-XE",
            Vendor::Juniper => "Juniper Junos (EX)",
            Vendor::Aruba => "Aruba / HP (ArubaOS-Switch)",
            Vendor::MikroTik => "MikroTik RouterOS",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "cisco" | "ios" => Vendor::Cisco,
            "juniper" | "junos" => Vendor::Juniper,
            "aruba" | "hp" | "procurve" => Vendor::Aruba,
            "mikrotik" | "routeros" => Vendor::MikroTik,
            _ => bail!("\"{s}\": choose cisco, juniper, aruba or mikrotik"),
        })
    }
}

impl Plan {
    /// One network per line, aligned, for the clipboard or a terminal.
    pub fn table(&self) -> String {
        let w = self.networks.iter().map(|n| n.name.chars().count()).max().unwrap_or(4).max(4);
        let v6 = self.networks.iter().any(|n| n.ipv6.is_some());
        let mut out = format!(
            "{:<w$}  {:>4}  {:>7}  {:<18}  {:<15}  {:<15}  {:<33}  {:<15}",
            "Name", "VLAN", "Devices", "Network", "Mask", "Gateway", "Usable", "Broadcast"
        );
        out += if v6 { "  IPv6\n" } else { "\n" };
        for n in &self.networks {
            let vlan = if n.vlan == 0 { String::new() } else { n.vlan.to_string() };
            let _ = write!(
                out,
                "{:<w$}  {:>4}  {:>7}  {:<18}  {:<15}  {:<15}  {:<33}  {:<15}",
                n.name,
                vlan,
                n.devices,
                format!("{}/{}", n.subnet.network, n.subnet.prefix),
                n.subnet.mask.to_string(),
                n.gateway.to_string(),
                format!("{} – {}", n.subnet.first, n.subnet.last),
                n.subnet.broadcast.to_string(),
            );
            match n.ipv6 {
                Some(a) => {
                    let _ = writeln!(out, "  {a}/64");
                }
                None => out.push('\n'),
            }
        }
        out
    }

    pub fn csv(&self) -> String {
        let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let mut out = String::from(
            "Name,VLAN,Devices,Addresses needed,Network,Prefix,Mask,Gateway,First usable,Last usable,Broadcast,Usable addresses,Spare,IPv6 network,IPv6 gateway\n",
        );
        for n in &self.networks {
            let s = &n.subnet;
            let vlan = if n.vlan == 0 { String::new() } else { n.vlan.to_string() };
            let _ = writeln!(
                out,
                "{},{vlan},{},{},{},/{},{},{},{},{},{},{},{},{},{}",
                esc(&n.name),
                n.devices,
                n.needed,
                s.network,
                s.prefix,
                s.mask,
                n.gateway,
                s.first,
                s.last,
                s.broadcast,
                s.hosts,
                n.spare,
                n.ipv6.map(|a| format!("{a}/64")).unwrap_or_default(),
                n.gateway6().map(|a| a.to_string()).unwrap_or_default(),
            );
        }
        out
    }

    pub fn config(&self, vendor: Vendor) -> String {
        match vendor {
            Vendor::Cisco => self.cisco(),
            Vendor::Juniper => self.juniper(),
            Vendor::Aruba => self.aruba(),
            Vendor::MikroTik => self.mikrotik(),
        }
    }

    /// Networks with a VLAN, in VLAN order.
    fn numbered(&self) -> Vec<&Network> {
        let mut v: Vec<&Network> = self.networks.iter().filter(|n| n.vlan != 0).collect();
        v.sort_by_key(|n| n.vlan);
        v
    }

    /// VLANs, gateway interfaces and DHCP pools for Cisco IOS (and the many
    /// switches with the same commands).
    pub fn cisco(&self) -> String {
        let mut out = String::new();
        let numbered = self.numbered();
        if !numbered.is_empty() {
            out += "! VLANs\n";
            for n in &numbered {
                let _ = writeln!(out, "vlan {}\n name {}", n.vlan, config_name(&n.name));
            }
            out += "!\n! Gateways (layer 3 switch or router)\n";
            if numbered.iter().any(|n| n.ipv6.is_some()) {
                out += "ipv6 unicast-routing\n";
            }
            for n in &numbered {
                let _ = writeln!(
                    out,
                    "interface Vlan{}\n description {}\n ip address {} {}",
                    n.vlan,
                    config_name(&n.name),
                    n.gateway,
                    n.subnet.mask
                );
                if let Some(g) = n.gateway6() {
                    let _ = writeln!(out, " ipv6 address {g}/64");
                }
                out += " no shutdown\n";
            }
            out += "!\n";
        }
        out += "! DHCP\n";
        for n in &self.networks {
            let _ = writeln!(out, "ip dhcp excluded-address {}", n.gateway);
        }
        for n in &self.networks {
            let _ = writeln!(
                out,
                "ip dhcp pool {}\n network {} {}\n default-router {}",
                config_name(&n.name),
                n.subnet.network,
                n.subnet.mask,
                n.gateway
            );
        }
        out
    }

    /// Junos on EX switches (ELS): VLANs with IRB interfaces and the DHCP
    /// server.
    pub fn juniper(&self) -> String {
        let mut out = String::new();
        let numbered = self.numbered();
        for n in &numbered {
            let name = config_name(&n.name);
            let _ = writeln!(out, "set vlans {name} vlan-id {}", n.vlan);
            let _ = writeln!(out, "set vlans {name} l3-interface irb.{}", n.vlan);
            let _ = writeln!(
                out,
                "set interfaces irb unit {} family inet address {}/{}",
                n.vlan, n.gateway, n.subnet.prefix
            );
            if let Some(g) = n.gateway6() {
                let _ = writeln!(out, "set interfaces irb unit {} family inet6 address {g}/64", n.vlan);
            }
        }
        for n in &self.networks {
            let name = config_name(&n.name);
            let pool = format!("set access address-assignment pool {name} family inet");
            let _ = writeln!(out, "{pool} network {}/{}", n.subnet.network, n.subnet.prefix);
            if let Some((a, b)) = n.dhcp_range() {
                let _ = writeln!(out, "{pool} range hosts low {a} high {b}");
            }
            let _ = writeln!(out, "{pool} dhcp-attributes router {}", n.gateway);
        }
        if !numbered.is_empty() {
            for n in &numbered {
                let _ = writeln!(out, "set system services dhcp-local-server group LAN interface irb.{}", n.vlan);
            }
        } else {
            out +=
                "# then: set system services dhcp-local-server group LAN interface <the interface of each network>\n";
        }
        out
    }

    /// ArubaOS-Switch (HP ProCurve, 2530/2930 and others).
    pub fn aruba(&self) -> String {
        let mut out = String::new();
        for n in &self.numbered() {
            let _ = writeln!(out, "vlan {}", n.vlan);
            let _ = writeln!(out, "   name \"{}\"", config_name(&n.name));
            let _ = writeln!(out, "   ip address {} {}", n.gateway, n.subnet.mask);
            if let Some(g) = n.gateway6() {
                let _ = writeln!(out, "   ipv6 address {g}/64");
            }
            out += "   dhcp-server\n   exit\n";
        }
        for n in &self.networks {
            let _ = writeln!(out, "dhcp-server pool \"{}\"", config_name(&n.name));
            let _ = writeln!(out, "   network {} {}", n.subnet.network, n.subnet.mask);
            let _ = writeln!(out, "   default-router {}", n.gateway);
            if let Some((a, b)) = n.dhcp_range() {
                let _ = writeln!(out, "   range {a} {b}");
            }
            out += "   exit\n";
        }
        out += "ip routing\ndhcp-server enable\n";
        out
    }

    /// RouterOS: VLAN interfaces on the bridge, addresses, pools and DHCP
    /// servers.
    pub fn mikrotik(&self) -> String {
        let mut out = String::new();
        let iface = |n: &Network| {
            if n.vlan == 0 { "bridge".to_string() } else { format!("vlan{}-{}", n.vlan, config_name(&n.name)) }
        };
        let numbered = self.numbered();
        if !numbered.is_empty() {
            out += "/interface vlan\n";
            for n in &numbered {
                let _ = writeln!(out, "add interface=bridge name={} vlan-id={}", iface(n), n.vlan);
            }
        }
        out += "/ip address\n";
        for n in &self.networks {
            let _ = writeln!(out, "add address={}/{} interface={}", n.gateway, n.subnet.prefix, iface(n));
        }
        if self.networks.iter().any(|n| n.ipv6.is_some()) {
            out += "/ipv6 address\n";
            for n in &self.networks {
                if let Some(g) = n.gateway6() {
                    let _ = writeln!(out, "add address={g}/64 interface={} advertise=yes", iface(n));
                }
            }
        }
        out += "/ip pool\n";
        for n in &self.networks {
            if let Some((a, b)) = n.dhcp_range() {
                let _ = writeln!(out, "add name={} ranges={a}-{b}", config_name(&n.name));
            }
        }
        out += "/ip dhcp-server\n";
        for n in &self.networks {
            let name = config_name(&n.name);
            let _ = writeln!(out, "add address-pool={name} interface={} name={name}", iface(n));
        }
        out += "/ip dhcp-server network\n";
        for n in &self.networks {
            let _ = writeln!(out, "add address={}/{} gateway={}", n.subnet.network, n.subnet.prefix, n.gateway);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<Group> {
        vec![
            Group { name: "Room".into(), count: 3, devices: 25 },
            Group { name: "IoT".into(), count: 1, devices: 100 },
            Group { name: "Links".into(), count: 2, devices: 1 },
        ]
    }

    fn sample() -> Plan {
        plan(&Subnet::parse("10.0.0.0/23").unwrap(), &groups(), &Options::default()).unwrap()
    }

    #[test]
    fn sizes() {
        assert_eq!(needed(25, 20), 31);
        assert_eq!(prefix_for(31), 26);
        assert_eq!(prefix_for(30), 27);
        assert_eq!(prefix_for(2), 30);
        assert_eq!(prefix_for(1), 30);
        assert_eq!(prefix_for(254), 24);
        assert_eq!(prefix_for(255), 23);
    }

    #[test]
    fn too_small() {
        let space = Subnet::parse("10.0.0.0/24").unwrap();
        assert_eq!(plan(&space, &groups(), &Options::default()), Err(PlanError::TooSmall { needed: 328, prefix: 23 }));
        assert_eq!(plan(&space, &[], &Options::default()), Err(PlanError::Empty));
    }

    #[test]
    fn fits() {
        let p = sample();
        let rows: Vec<String> = p
            .networks
            .iter()
            .map(|n| format!("{} {}/{} {}", n.name, n.subnet.network, n.subnet.prefix, n.vlan))
            .collect();
        assert_eq!(
            rows,
            [
                "IoT 10.0.0.0/25 40",
                "Room 1 10.0.0.128/26 10",
                "Room 2 10.0.0.192/26 20",
                "Room 3 10.0.1.0/26 30",
                "Links 1 10.0.1.64/30 50",
                "Links 2 10.0.1.68/30 60",
            ]
        );
        assert_eq!(p.used, 328);
        let free: Vec<String> = p.free.iter().map(|s| format!("{}/{}", s.network, s.prefix)).collect();
        assert_eq!(free, ["10.0.1.72/29", "10.0.1.80/28", "10.0.1.96/27", "10.0.1.128/25"]);
        let room = &p.networks[1];
        assert_eq!(room.gateway.to_string(), "10.0.0.129");
        assert_eq!(room.spare, 62 - 31);
        assert_eq!(room.dhcp_range().map(|(a, b)| format!("{a}-{b}")).as_deref(), Some("10.0.0.130-10.0.0.190"));
        assert!(p.csv().starts_with("Name,VLAN"));
        assert_eq!(p.table().lines().count(), 7);
    }

    #[test]
    fn configs() {
        let p = sample();
        let cisco = p.cisco();
        assert!(cisco.find("vlan 10\n").unwrap() < cisco.find("vlan 40\n").unwrap());
        assert!(cisco.contains("vlan 10\n name Room-1\n"));
        assert!(
            cisco.contains("interface Vlan40\n description IoT\n ip address 10.0.0.1 255.255.255.128\n no shutdown\n")
        );
        assert!(
            cisco.contains("ip dhcp pool Room-1\n network 10.0.0.128 255.255.255.192\n default-router 10.0.0.129\n")
        );
        let junos = p.juniper();
        assert!(junos.contains("set vlans IoT vlan-id 40\nset vlans IoT l3-interface irb.40\n"));
        assert!(junos.contains("set interfaces irb unit 40 family inet address 10.0.0.1/25\n"));
        assert!(junos.contains(
            "set access address-assignment pool Room-1 family inet range hosts low 10.0.0.130 high 10.0.0.190\n"
        ));
        let aruba = p.aruba();
        assert!(aruba.contains(
            "vlan 10\n   name \"Room-1\"\n   ip address 10.0.0.129 255.255.255.192\n   dhcp-server\n   exit\n"
        ));
        assert!(aruba.contains("   range 10.0.0.130 10.0.0.190\n"));
        let mt = p.mikrotik();
        assert!(mt.contains("add interface=bridge name=vlan10-Room-1 vlan-id=10\n"));
        assert!(mt.contains("add address=10.0.0.129/26 interface=vlan10-Room-1\n"));
        assert!(mt.contains("add name=Room-1 ranges=10.0.0.130-10.0.0.190\n"));
        assert!(mt.contains("add address=10.0.0.128/26 gateway=10.0.0.129\n"));
    }

    #[test]
    fn ipv6_by_vlan() {
        let space = Subnet::parse("10.0.0.0/23").unwrap();
        let p = plan_dual(&space, "2001:db8:abcd:ff00::/48", &groups(), &Options::default()).unwrap().unwrap();
        let room1 = p.networks.iter().find(|n| n.name == "Room 1").unwrap();
        assert_eq!(room1.ipv6.unwrap().to_string(), "2001:db8:abcd:10::");
        assert_eq!(room1.gateway6().unwrap().to_string(), "2001:db8:abcd:10::1");
        assert!(p.cisco().contains("interface Vlan10\n description Room-1\n ip address 10.0.0.129 255.255.255.192\n ipv6 address 2001:db8:abcd:10::1/64\n"));
        assert!(p.table().lines().next().unwrap().ends_with("IPv6"));
    }

    #[test]
    fn ipv6_in_order() {
        let space = Subnet::parse("10.0.0.0/23").unwrap();
        let p = plan_dual(&space, "2001:db8:0:ab00::/56", &groups(), &Options::default()).unwrap().unwrap();
        let ids: Vec<String> = p.networks.iter().map(|n| n.ipv6.unwrap().to_string()).collect();
        assert_eq!(ids[0], "2001:db8:0:ab01::");
        assert_eq!(ids[5], "2001:db8:0:ab06::");
        let tight = plan_dual(&space, "2001:db8::/62", &groups(), &Options::default()).unwrap();
        assert_eq!(tight, Err(PlanError::Ipv6TooSmall { room: 3 }));
        assert!(plan_dual(&space, "2001:db8::/80", &groups(), &Options::default()).is_err());
        assert!(random_ula().starts_with("fd") && parse_ipv6(&random_ula()).is_ok());
    }

    #[test]
    fn parses_groups() {
        assert_eq!(Group::parse("Rooms=12x25").unwrap(), Group { name: "Rooms".into(), count: 12, devices: 25 });
        assert_eq!(Group::parse("IoT = 150").unwrap(), Group { name: "IoT".into(), count: 1, devices: 150 });
        assert!(Group::parse("150").is_err());
        assert!(Group::parse("=150").is_err());
    }

    #[test]
    fn no_vlans() {
        let space = Subnet::parse("192.168.0.0/16").unwrap();
        let opt = Options { first_vlan: 0, ..Options::default() };
        let p = plan(&space, &groups(), &opt).unwrap();
        assert!(p.networks.iter().all(|n| n.vlan == 0));
        assert!(!p.cisco().contains("vlan"));
        assert!(p.mikrotik().contains("interface=bridge"));
    }
}
