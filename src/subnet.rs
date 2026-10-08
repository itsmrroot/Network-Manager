//! IPv4 subnet calculator.

use std::net::Ipv4Addr;

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

use crate::adapters::{mask_to_prefix, prefix_to_mask};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Subnet {
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub network: Ipv4Addr,
    pub broadcast: Ipv4Addr,
    pub mask: Ipv4Addr,
    pub wildcard: Ipv4Addr,
    pub first: Ipv4Addr,
    pub last: Ipv4Addr,
    /// Usable addresses for devices.
    pub hosts: u64,
}

impl Subnet {
    pub fn new(address: Ipv4Addr, prefix: u8) -> Result<Self> {
        ensure!(prefix <= 32, "the prefix must be between 0 and 32");
        let mask = u32::from(prefix_to_mask(prefix));
        let net = u32::from(address) & mask;
        let bc = net | !mask;
        let (first, last, hosts) = match prefix {
            32 => (net, net, 1),
            // RFC 3021: both addresses of a /31 are usable.
            31 => (net, bc, 2),
            _ => (net + 1, bc - 1, (1u64 << (32 - prefix)) - 2),
        };
        Ok(Self {
            address,
            prefix,
            network: net.into(),
            broadcast: bc.into(),
            mask: mask.into(),
            wildcard: (!mask).into(),
            first: first.into(),
            last: last.into(),
            hosts,
        })
    }

    /// Parses `192.168.1.10/24`, `192.168.1.10 255.255.255.0`,
    /// `192.168.1.10/255.255.255.0` or a bare address (as /24 for private
    /// addresses, /32 otherwise).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let (addr, rest) = match s.split_once(['/', ' ']) {
            Some((a, r)) => (a.trim(), Some(r.trim())),
            None => (s, None),
        };
        let address: Ipv4Addr = addr.parse().with_context(|| format!("\"{addr}\" is not an IPv4 address"))?;
        let prefix = match rest {
            None | Some("") => {
                if address.is_private() {
                    24
                } else {
                    32
                }
            }
            Some(r) if r.contains('.') => {
                let mask: Ipv4Addr = r.parse().with_context(|| format!("\"{r}\" is not a subnet mask"))?;
                match mask_to_prefix(mask) {
                    Some(p) => p,
                    None => bail!("{mask} is not a valid subnet mask (its ones must come first)"),
                }
            }
            Some(r) => {
                r.parse().ok().filter(|p| *p <= 32).with_context(|| format!("\"/{r}\" is not a prefix (0 to 32)"))?
            }
        };
        Self::new(address, prefix)
    }

    /// "Private (RFC 1918)", "Public", …
    pub fn scope(&self) -> &'static str {
        let a = self.address;
        if a.is_private() {
            "Private (RFC 1918)"
        } else if a.is_loopback() {
            "Loopback"
        } else if a.is_link_local() {
            "Link-local (no DHCP answer)"
        } else if a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64 {
            "Carrier-grade NAT (RFC 6598)"
        } else if a.is_multicast() {
            "Multicast"
        } else if matches!(a.octets(), [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]) {
            "Documentation"
        } else {
            "Public"
        }
    }

    /// The historical class (A–E).
    pub fn class(&self) -> char {
        match self.address.octets()[0] {
            0..=127 => 'A',
            128..=191 => 'B',
            192..=223 => 'C',
            224..=239 => 'D',
            _ => 'E',
        }
    }

    /// The mask in binary, with dots: 11111111.11111111.11111111.00000000.
    pub fn mask_binary(&self) -> String {
        self.mask.octets().iter().map(|o| format!("{o:08b}")).collect::<Vec<_>>().join(".")
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & u32::from(self.mask) == u32::from(self.network)
    }

    /// The subnets of length `prefix` this network splits into (at most
    /// `limit` of them).
    pub fn split(&self, prefix: u8, limit: usize) -> Result<Vec<Subnet>> {
        ensure!(prefix >= self.prefix && prefix <= 32, "choose a prefix between /{} and /32", self.prefix);
        let count = 1u64 << (prefix - self.prefix);
        let step = 1u64 << (32 - prefix);
        (0..count.min(limit as u64))
            .map(|i| Subnet::new(Ipv4Addr::from((u32::from(self.network) as u64 + i * step) as u32), prefix))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates() {
        let s = Subnet::parse("192.168.1.130/26").unwrap();
        assert_eq!(s.network.to_string(), "192.168.1.128");
        assert_eq!(s.broadcast.to_string(), "192.168.1.191");
        assert_eq!(s.first.to_string(), "192.168.1.129");
        assert_eq!(s.last.to_string(), "192.168.1.190");
        assert_eq!(s.mask.to_string(), "255.255.255.192");
        assert_eq!(s.wildcard.to_string(), "0.0.0.63");
        assert_eq!(s.hosts, 62);
        assert_eq!(s.class(), 'C');
        assert_eq!(s.scope(), "Private (RFC 1918)");
        assert!(s.contains("192.168.1.150".parse().unwrap()));
        assert!(!s.contains("192.168.1.10".parse().unwrap()));
    }

    #[test]
    fn notations() {
        assert_eq!(Subnet::parse("10.1.2.3 255.255.0.0").unwrap().prefix, 16);
        assert_eq!(Subnet::parse("10.1.2.3/255.255.0.0").unwrap().prefix, 16);
        assert_eq!(Subnet::parse("10.1.2.3").unwrap().prefix, 24);
        assert_eq!(Subnet::parse("8.8.8.8").unwrap().prefix, 32);
        assert!(Subnet::parse("10.1.2.3/33").is_err());
        assert!(Subnet::parse("10.1.2.3 255.0.255.0").is_err());
        assert!(Subnet::parse("10.1.2").is_err());
    }

    #[test]
    fn edges() {
        assert_eq!(Subnet::parse("10.0.0.0/31").unwrap().hosts, 2);
        assert_eq!(Subnet::parse("10.0.0.5/32").unwrap().hosts, 1);
        assert_eq!(Subnet::parse("0.0.0.0/0").unwrap().hosts, (1u64 << 32) - 2);
        assert_eq!(Subnet::parse("100.64.1.1/10").unwrap().scope(), "Carrier-grade NAT (RFC 6598)");
    }

    #[test]
    fn splits() {
        let s = Subnet::parse("192.168.0.0/24").unwrap();
        let parts = s.split(26, 100).unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[3].network.to_string(), "192.168.0.192");
        assert!(s.split(20, 10).is_err());
        assert_eq!(s.split(32, 10).unwrap().len(), 10);
    }
}
