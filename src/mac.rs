//! MAC addresses: parsing, random addresses and manufacturer names.

use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::str::FromStr;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    pub fn is_zero(&self) -> bool {
        self.0 == [0; 6]
    }

    /// What macOS reports instead of the real address to apps without
    /// Local Network access.
    pub fn is_placeholder(&self) -> bool {
        self.0 == [2, 0, 0, 0, 0, 0]
    }

    pub fn is_broadcast(&self) -> bool {
        self.0 == [0xff; 6]
    }

    pub fn is_multicast(&self) -> bool {
        self.0[0] & 1 == 1
    }

    /// Set by the owner rather than burned in by the manufacturer — phones
    /// and computers use such addresses for privacy ("private Wi-Fi address").
    pub fn is_local(&self) -> bool {
        self.0[0] & 2 == 2
    }

    /// A random, locally administered unicast address. Windows only accepts
    /// such addresses for Wi-Fi adapters (second digit 2, 6, A or E).
    pub fn random() -> Mac {
        let mut b = random_bytes();
        b[0] = (b[0] & 0xfc) | 0x02;
        Mac(b)
    }

    /// A random address that looks like one made by `vendor` (its first
    /// three bytes).
    pub fn random_with_prefix(prefix: [u8; 3]) -> Mac {
        let r = random_bytes();
        Mac([prefix[0], prefix[1], prefix[2], r[3], r[4], r[5]])
    }

    /// `AA:BB:CC:DD:EE:FF`.
    pub fn colons(&self) -> String {
        self.join(":")
    }

    /// `AA-BB-CC-DD-EE-FF`, as Windows writes them.
    pub fn dashes(&self) -> String {
        self.join("-")
    }

    /// `AABBCCDDEEFF`.
    pub fn plain(&self) -> String {
        self.join("")
    }

    fn join(&self, sep: &str) -> String {
        self.0.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(sep)
    }

    /// The manufacturer, from the IEEE registry.
    pub fn vendor(&self) -> Option<&'static str> {
        if self.is_local() {
            return None;
        }
        vendor(&[self.0[0], self.0[1], self.0[2]])
    }
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.colons())
    }
}

impl FromStr for Mac {
    type Err = anyhow::Error;

    /// Accepts `aa:bb:cc:dd:ee:ff`, `aa-bb-…`, `aabb.ccdd.eeff` (Cisco),
    /// `aabbccddeeff`, and the shortened `0:94:ec:3e:41:45` of macOS `arp`.
    fn from_str(s: &str) -> Result<Mac> {
        let s = s.trim();
        let parts: Vec<&str> = s.split([':', '-']).collect();
        let hex: String = if parts.len() == 6 {
            if parts.iter().any(|p| p.is_empty() || p.len() > 2) {
                bail!("not a MAC address: {s}");
            }
            parts.iter().map(|p| format!("{p:0>2}")).collect()
        } else {
            s.chars().filter(|c| *c != '.').collect()
        };
        if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("not a MAC address: {s}");
        }
        let mut b = [0u8; 6];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
        }
        Ok(Mac(b))
    }
}

impl Serialize for Mac {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.colons())
    }
}

impl<'de> Deserialize<'de> for Mac {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Random bytes from the hasher keys the standard library seeds from the
/// operating system — plenty for MAC addresses and DNS query ids.
pub fn random_bytes() -> [u8; 6] {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    let v = h.finish().to_le_bytes();
    [v[0], v[1], v[2], v[3], v[4], v[5]]
}

/// IEEE MA-L assignments, `scripts/update-oui.py` rebuilds the file.
static OUI: LazyLock<Vec<(u32, &'static str)>> = LazyLock::new(|| {
    include_str!("../data/oui.tsv")
        .lines()
        .filter_map(|l| {
            let (prefix, name) = l.split_once('\t')?;
            Some((u32::from_str_radix(prefix, 16).ok()?, name))
        })
        .collect()
});

/// The manufacturer that owns the first three bytes of an address.
pub fn vendor(prefix: &[u8; 3]) -> Option<&'static str> {
    let key = u32::from_be_bytes([0, prefix[0], prefix[1], prefix[2]]);
    let table = &*OUI;
    table.binary_search_by_key(&key, |(k, _)| *k).ok().map(|i| table[i].1)
}

/// Manufacturers whose prefix can be borrowed for a random address.
pub fn vendor_prefixes(query: &str, limit: usize) -> Vec<([u8; 3], &'static str)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    OUI.iter()
        .filter(|(_, n)| n.to_lowercase().contains(&q))
        .take(limit)
        .map(|(k, n)| {
            let b = k.to_be_bytes();
            ([b[1], b[2], b[3]], *n)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_notation() {
        let want = Mac([0x00, 0x94, 0xec, 0x3e, 0x41, 0x45]);
        for s in ["00:94:EC:3E:41:45", "00-94-ec-3e-41-45", "0094.ec3e.4145", "0094ec3e4145", "0:94:ec:3e:41:45"] {
            assert_eq!(s.parse::<Mac>().unwrap(), want, "{s}");
        }
        for bad in ["", "00:94:ec:3e:41", "00:94:ec:3e:41:4g", "000:94:ec:3e:41:45", "0094ec3e41"] {
            assert!(bad.parse::<Mac>().is_err(), "{bad}");
        }
        assert_eq!(want.to_string(), "00:94:EC:3E:41:45");
        assert_eq!(want.dashes(), "00-94-EC-3E-41-45");
    }

    #[test]
    fn random_addresses_are_local_unicast() {
        for _ in 0..50 {
            let m = Mac::random();
            assert!(m.is_local() && !m.is_multicast());
            assert!(matches!(m.0[0] & 0x0f, 0x2 | 0x6 | 0xa | 0xe));
        }
        assert_ne!(Mac::random(), Mac::random());
    }

    #[test]
    fn vendors_are_found() {
        assert_eq!("B8:27:EB:00:00:01".parse::<Mac>().unwrap().vendor(), Some("Raspberry Pi Foundation"));
        assert_eq!(vendor(&[0x5c, 0x9b, 0xa6]), Some("Apple"));
        assert_eq!(Mac::random().vendor(), None);
        assert!(!vendor_prefixes("raspberry", 5).is_empty());
    }
}
