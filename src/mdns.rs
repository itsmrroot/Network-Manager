//! Bonjour / mDNS browser: the services devices announce on the local
//! network — printers, AirPlay and Chromecast, file shares, smart-home
//! hubs — with their names, addresses and ports.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

use crate::dns::{build_query, read_name, u16_at};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const META: &str = "_services._dns-sd._udp.local";

/// Common service types, asked for directly: some devices do not answer
/// the question "which services are there?".
pub const COMMON: [&str; 16] = [
    "_ipp._tcp.local",
    "_ipps._tcp.local",
    "_printer._tcp.local",
    "_pdl-datastream._tcp.local",
    "_airplay._tcp.local",
    "_raop._tcp.local",
    "_googlecast._tcp.local",
    "_smb._tcp.local",
    "_afpovertcp._tcp.local",
    "_ssh._tcp.local",
    "_http._tcp.local",
    "_hap._tcp.local",
    "_homekit._tcp.local",
    "_spotify-connect._tcp.local",
    "_scanner._tcp.local",
    "_device-info._tcp.local",
];

/// What a service type is, for people.
pub fn describe(kind: &str) -> &'static str {
    match kind.trim_end_matches(".local") {
        "_ipp._tcp" | "_ipps._tcp" => "Printer (IPP / AirPrint)",
        "_printer._tcp" => "Printer (LPD)",
        "_pdl-datastream._tcp" => "Printer (raw)",
        "_scanner._tcp" | "_uscan._tcp" | "_uscans._tcp" => "Scanner",
        "_airplay._tcp" => "AirPlay",
        "_raop._tcp" => "AirPlay audio",
        "_googlecast._tcp" => "Chromecast / Google Cast",
        "_smb._tcp" => "File sharing (SMB)",
        "_afpovertcp._tcp" => "File sharing (AFP)",
        "_nfs._tcp" => "File sharing (NFS)",
        "_ssh._tcp" | "_sftp-ssh._tcp" => "SSH",
        "_http._tcp" | "_https._tcp" => "Web page",
        "_hap._tcp" | "_homekit._tcp" => "HomeKit",
        "_matter._tcp" | "_matterc._udp" => "Matter (smart home)",
        "_spotify-connect._tcp" => "Spotify Connect",
        "_sonos._tcp" => "Sonos",
        "_companion-link._tcp" => "Apple device",
        "_device-info._tcp" => "Device information",
        "_rdlink._tcp" => "Apple remote",
        "_workstation._tcp" => "Computer",
        "_sleep-proxy._udp" => "Sleep proxy",
        "_adisk._tcp" => "Time Machine",
        "_daap._tcp" => "Music sharing",
        _ => "",
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Service {
    /// "Office printer", "Living Room".
    pub name: String,
    /// "_ipp._tcp".
    pub kind: String,
    /// "office-printer.local".
    pub host: String,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
    /// "key=value" pairs from the TXT record.
    pub details: Vec<String>,
}

/// Everything learned from the answers.
#[derive(Default)]
struct Cache {
    /// Service type → instances.
    ptr: BTreeMap<String, BTreeSet<String>>,
    /// Instance → (host, port).
    srv: BTreeMap<String, (String, u16)>,
    txt: BTreeMap<String, Vec<String>>,
    /// Host → addresses.
    addr: BTreeMap<String, BTreeSet<IpAddr>>,
}

impl Cache {
    fn read(&mut self, msg: &[u8]) -> Result<()> {
        let count = |i| u16_at(msg, i).map(|n| n as usize);
        let questions = count(4)?;
        let records = count(6)? + count(8)? + count(10)?;
        let mut pos = 12;
        for _ in 0..questions {
            pos = read_name(msg, pos)?.1 + 4;
        }
        for _ in 0..records {
            let (name, p) = read_name(msg, pos)?;
            let kind = u16_at(msg, p)?;
            let len = u16_at(msg, p + 8)? as usize;
            let start = p + 10;
            let Some(data) = msg.get(start..start + len) else { break };
            pos = start + len;
            let name = name.to_lowercase();
            match kind {
                12 => {
                    let target = read_name(msg, start)?.0;
                    self.ptr.entry(name).or_default().insert(target);
                }
                33 if len >= 7 => {
                    let port = u16_at(msg, start + 4)?;
                    let target = read_name(msg, start + 6)?.0.to_lowercase();
                    self.srv.insert(name, (target, port));
                }
                16 => {
                    let mut parts = Vec::new();
                    let mut i = 0;
                    while i < data.len() {
                        let l = data[i] as usize;
                        if let Some(s) = data.get(i + 1..i + 1 + l)
                            && !s.is_empty()
                        {
                            parts.push(String::from_utf8_lossy(s).into_owned());
                        }
                        i += 1 + l;
                    }
                    self.txt.insert(name, parts);
                }
                1 if len == 4 => {
                    self.addr
                        .entry(name)
                        .or_default()
                        .insert(IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3])));
                }
                28 if len == 16 => {
                    let b: [u8; 16] = data.try_into().unwrap_or([0; 16]);
                    self.addr.entry(name).or_default().insert(IpAddr::V6(Ipv6Addr::from(b)));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn services(&self) -> Vec<Service> {
        let mut out = Vec::new();
        for (kind, instances) in self.ptr.iter().filter(|(k, _)| k.as_str() != META) {
            for instance in instances {
                let key = instance.to_lowercase();
                let (host, port) = self.srv.get(&key).cloned().unwrap_or_default();
                let mut addresses: Vec<IpAddr> = self.addr.get(&host).into_iter().flatten().copied().collect();
                // IPv4 first; link-local IPv6 last.
                addresses
                    .sort_by_key(|a| (a.is_ipv6(), matches!(a, IpAddr::V6(v) if (v.segments()[0] & 0xffc0) == 0xfe80)));
                // The type, in any case, after the instance name.
                let cut = instance.len().saturating_sub(kind.len() + 1);
                let name =
                    if instance.to_lowercase().ends_with(&format!(".{kind}")) { &instance[..cut] } else { instance };
                let name = name.replace("\\032", " ").replace("\\.", ".");
                out.push(Service {
                    name,
                    kind: kind.trim_end_matches(".local").to_string(),
                    host: host.trim_end_matches('.').to_string(),
                    port,
                    addresses,
                    details: self.txt.get(&key).cloned().unwrap_or_default(),
                });
            }
        }
        out.sort_by(|a, b| {
            (describe(&a.kind).is_empty(), &a.kind, a.name.to_lowercase()).cmp(&(
                describe(&b.kind).is_empty(),
                &b.kind,
                b.name.to_lowercase(),
            ))
        });
        out
    }
}

fn ask(sock: &UdpSocket, name: &str, qtype: u16) {
    if let Ok(q) = build_query(0, name, qtype, false) {
        let _ = sock.send_to(&q, SocketAddr::from((GROUP, 5353)));
    }
}

fn listen(sock: &UdpSocket, cache: &mut Cache, until: Instant) {
    let mut buf = [0u8; 9000];
    while Instant::now() < until {
        let left = until.saturating_duration_since(Instant::now()).max(Duration::from_millis(10));
        let _ = sock.set_read_timeout(Some(left.min(Duration::from_millis(200))));
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            let _ = cache.read(&buf[..n]);
        }
    }
}

/// Asks the local network for its services and listens for `wait`.
/// Questions go from an ordinary port, so devices answer this program
/// directly (RFC 6762 "legacy unicast"), alongside the system's own mDNS.
pub fn browse(wait: Duration) -> Result<Vec<Service>> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    let _ = sock.set_multicast_ttl_v4(255);
    let mut cache = Cache::default();
    ask(&sock, META, 12);
    for kind in COMMON {
        ask(&sock, kind, 12);
    }
    listen(&sock, &mut cache, Instant::now() + wait / 2);
    // Every type found: its instances; then the details of each.
    let kinds: Vec<String> = cache.ptr.get(META).into_iter().flatten().cloned().collect();
    for kind in &kinds {
        ask(&sock, kind, 12);
    }
    listen(&sock, &mut cache, Instant::now() + wait / 4);
    let missing: Vec<String> =
        cache.ptr.values().flatten().filter(|i| !cache.srv.contains_key(&i.to_lowercase())).cloned().collect();
    for instance in missing.iter().take(100) {
        ask(&sock, instance, 33);
        ask(&sock, instance, 16);
    }
    let hosts: Vec<String> =
        cache.srv.values().map(|(h, _)| h.clone()).filter(|h| !cache.addr.contains_key(h)).collect();
    for h in hosts.iter().take(100) {
        ask(&sock, h, 1);
    }
    listen(&sock, &mut cache, Instant::now() + wait / 4);
    Ok(cache.services())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for l in n.split('.') {
            out.push(l.len() as u8);
            out.extend_from_slice(l.as_bytes());
        }
        out.push(0);
        out
    }

    fn rr(owner: &str, kind: u16, data: &[u8]) -> Vec<u8> {
        let mut r = name(owner);
        r.extend_from_slice(&kind.to_be_bytes());
        r.extend_from_slice(&[0x80, 0x01, 0, 0, 0x11, 0x94]);
        r.extend_from_slice(&(data.len() as u16).to_be_bytes());
        r.extend_from_slice(data);
        r
    }

    #[test]
    fn reads_an_announcement() {
        let mut srv = vec![0, 0, 0, 0, 0x02, 0x77];
        srv.extend(name("office-printer.local"));
        let txt = [&[6u8][..], b"ty=HP4", &[4], b"pdl="].concat();
        let records = [
            rr("_ipp._tcp.local", 12, &name("Office Printer._ipp._tcp.local")),
            rr("Office Printer._ipp._tcp.local", 33, &srv),
            rr("Office Printer._ipp._tcp.local", 16, &txt),
            rr("office-printer.local", 1, &[192, 168, 1, 50]),
        ]
        .concat();
        let mut msg = vec![0, 0, 0x84, 0, 0, 0, 0, 4, 0, 0, 0, 0];
        msg.extend(records);
        let mut c = Cache::default();
        c.read(&msg).unwrap();
        let s = c.services();
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].name.as_str(), s[0].kind.as_str(), s[0].port), ("Office Printer", "_ipp._tcp", 631));
        assert_eq!(s[0].host, "office-printer.local");
        assert_eq!(s[0].addresses, [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))]);
        assert_eq!(s[0].details, ["ty=HP4", "pdl="]);
        assert_eq!(describe(&s[0].kind), "Printer (IPP / AirPrint)");
    }
}
