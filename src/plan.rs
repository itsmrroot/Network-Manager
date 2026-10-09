//! Network planner: gives every group of devices (rooms, IoT, guests, …) its
//! own subnet, sized for its devices plus room to grow, and lays them out in an
//! address space without gaps (VLSM).

use std::fmt::Write as _;
use std::net::Ipv4Addr;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::subnet::Subnet;

/// Networks of the same kind: `count` of them with `devices` devices each,
/// e.g. 12 rooms of 25 PCs.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl Network {
    /// Addresses DHCP can hand out: everything after the gateway.
    pub fn dhcp_range(&self) -> Option<(Ipv4Addr, Ipv4Addr)> {
        let first = u32::from(self.gateway) + 1;
        (first <= u32::from(self.subnet.last)).then(|| (first.into(), self.subnet.last))
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
        for i in 1..=g.count {
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
            Network { name, vlan, devices, needed, gateway: subnet.first, spare: subnet.hosts - needed, subnet }
        })
        .collect();
    Ok(Plan { space: space.clone(), networks, used: at - base, free: blocks(at, base + room) })
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
    s.trim_matches('-').chars().take(32).collect()
}

impl Plan {
    /// One network per line, aligned, for the clipboard or a terminal.
    pub fn table(&self) -> String {
        let w = self.networks.iter().map(|n| n.name.chars().count()).max().unwrap_or(4).max(4);
        let mut out = format!(
            "{:<w$}  {:>4}  {:>7}  {:<18}  {:<15}  {:<15}  {:<33}  {:<15}\n",
            "Name", "VLAN", "Devices", "Network", "Mask", "Gateway", "Usable", "Broadcast"
        );
        for n in &self.networks {
            let vlan = if n.vlan == 0 { String::new() } else { n.vlan.to_string() };
            let _ = writeln!(
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
        }
        out
    }

    pub fn csv(&self) -> String {
        let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let mut out = String::from(
            "Name,VLAN,Devices,Addresses needed,Network,Prefix,Mask,Gateway,First usable,Last usable,Broadcast,Usable addresses,Spare\n",
        );
        for n in &self.networks {
            let s = &n.subnet;
            let vlan = if n.vlan == 0 { String::new() } else { n.vlan.to_string() };
            let _ = writeln!(
                out,
                "{},{vlan},{},{},{},/{},{},{},{},{},{},{},{}",
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
                n.spare
            );
        }
        out
    }

    /// VLANs, gateway interfaces and DHCP pools for Cisco IOS (and the many
    /// switches with the same commands).
    pub fn cisco(&self) -> String {
        let mut out = String::new();
        let mut numbered: Vec<&Network> = self.networks.iter().filter(|n| n.vlan != 0).collect();
        numbered.sort_by_key(|n| n.vlan);
        if !numbered.is_empty() {
            out += "! VLANs\n";
            for n in &numbered {
                let _ = writeln!(out, "vlan {}\n name {}", n.vlan, config_name(&n.name));
            }
            out += "!\n! Gateways (layer 3 switch or router)\n";
            for n in &numbered {
                let _ = writeln!(
                    out,
                    "interface Vlan{}\n description {}\n ip address {} {}\n no shutdown",
                    n.vlan,
                    config_name(&n.name),
                    n.gateway,
                    n.subnet.mask
                );
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
        let space = Subnet::parse("10.0.0.0/23").unwrap();
        let p = plan(&space, &groups(), &Options::default()).unwrap();
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
        let cisco = p.cisco();
        assert!(cisco.contains("vlan 10\n name Room-1\n"));
        assert!(cisco.contains("interface Vlan40\n description IoT\n ip address 10.0.0.1 255.255.255.128\n"));
        assert!(
            cisco.contains("ip dhcp pool Room-1\n network 10.0.0.128 255.255.255.192\n default-router 10.0.0.129\n")
        );
        assert!(p.csv().starts_with("Name,VLAN"));
        assert_eq!(p.table().lines().count(), 7);
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
    }
}
