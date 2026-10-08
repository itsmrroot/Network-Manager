//! A small DNS client: asks one server directly, so that resolvers can be
//! compared, and finds the names of devices on the network (reverse DNS,
//! multicast DNS and NetBIOS).

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

use crate::mac::random_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecordType {
    A,
    Aaaa,
    Cname,
    Mx,
    Ns,
    Txt,
    Ptr,
    Soa,
    Srv,
    Caa,
}

impl RecordType {
    pub const ALL: [RecordType; 10] = [
        RecordType::A,
        RecordType::Aaaa,
        RecordType::Cname,
        RecordType::Mx,
        RecordType::Ns,
        RecordType::Txt,
        RecordType::Soa,
        RecordType::Srv,
        RecordType::Caa,
        RecordType::Ptr,
    ];

    pub fn code(self) -> u16 {
        match self {
            RecordType::A => 1,
            RecordType::Ns => 2,
            RecordType::Cname => 5,
            RecordType::Soa => 6,
            RecordType::Ptr => 12,
            RecordType::Mx => 15,
            RecordType::Txt => 16,
            RecordType::Aaaa => 28,
            RecordType::Srv => 33,
            RecordType::Caa => 257,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RecordType::A => "A",
            RecordType::Aaaa => "AAAA",
            RecordType::Cname => "CNAME",
            RecordType::Mx => "MX",
            RecordType::Ns => "NS",
            RecordType::Txt => "TXT",
            RecordType::Ptr => "PTR",
            RecordType::Soa => "SOA",
            RecordType::Srv => "SRV",
            RecordType::Caa => "CAA",
        }
    }

    fn from_code(c: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.code() == c)
    }
}

impl std::str::FromStr for RecordType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(s))
            .with_context(|| format!("unknown record type {s} (A, AAAA, CNAME, MX, NS, TXT, SOA, SRV, CAA, PTR)"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub name: String,
    pub kind: String,
    pub ttl: u32,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub server: SocketAddr,
    pub records: Vec<Record>,
    /// NOERROR, NXDOMAIN, SERVFAIL, …
    pub status: &'static str,
    pub millis: u128,
    pub authoritative: bool,
}

/// Well-known public resolvers, for comparisons.
pub const PUBLIC_RESOLVERS: &[(&str, &str)] =
    &[("Cloudflare", "1.1.1.1"), ("Google", "8.8.8.8"), ("Quad9", "9.9.9.9"), ("OpenDNS", "208.67.222.222")];

/// The DNS servers this computer uses (of the default adapter, else any).
pub fn system_servers() -> Vec<IpAddr> {
    let adapters = crate::adapters::list(false).unwrap_or_default();
    let mut servers: Vec<IpAddr> = adapters.iter().filter(|a| a.default).flat_map(|a| a.dns.clone()).collect();
    if servers.is_empty() {
        servers = adapters.iter().flat_map(|a| a.dns.clone()).collect();
    }
    // Link-local IPv6 servers need a scope id that IpAddr cannot carry.
    servers.retain(|s| !matches!(s, IpAddr::V6(v) if v.is_unicast_link_local()));
    servers.sort_by_key(|s| s.is_ipv6());
    servers.dedup();
    servers
}

/// For a PTR lookup: `1.2.3.4` → `4.3.2.1.in-addr.arpa`.
pub fn reverse_name(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0])
        }
        IpAddr::V6(v) => {
            let mut s = String::new();
            for b in v.octets().iter().rev() {
                s += &format!("{:x}.{:x}.", b & 0xf, b >> 4);
            }
            s + "ip6.arpa"
        }
    }
}

/// Asks `server` for the `kind` records of `name`.
pub fn query(server: SocketAddr, name: &str, kind: RecordType, timeout: Duration) -> Result<Answer> {
    let name = if kind == RecordType::Ptr {
        match name.parse::<IpAddr>() {
            Ok(ip) => reverse_name(ip),
            Err(_) => name.to_string(),
        }
    } else {
        name.trim().trim_end_matches('.').to_string()
    };
    ensure!(!name.is_empty(), "enter a name to look up");
    let r = random_bytes();
    let id = u16::from_be_bytes([r[0], r[1]]);
    let packet = build_query(id, &name, kind.code(), true)?;
    let started = Instant::now();
    let local: SocketAddr = if server.is_ipv4() { "0.0.0.0:0".parse()? } else { "[::]:0".parse()? };
    let sock = UdpSocket::bind(local)?;
    sock.set_read_timeout(Some(timeout))?;
    sock.connect(server)?;
    let mut buf = vec![0u8; 4096];
    let mut reply = None;
    // One retry: a single lost packet should not fail the lookup.
    for _ in 0..2 {
        sock.send(&packet)?;
        loop {
            match sock.recv(&mut buf) {
                Ok(n) if n >= 12 && u16::from_be_bytes([buf[0], buf[1]]) == id => {
                    reply = Some(buf[..n].to_vec());
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        if reply.is_some() {
            break;
        }
    }
    let Some(mut reply) = reply else { bail!("{server} did not answer") };
    // Truncated: ask again over TCP.
    if reply[2] & 0x02 != 0 {
        reply = query_tcp(server, &packet, timeout)?;
    }
    let millis = started.elapsed().as_millis();
    let (status, authoritative, records) = parse_reply(&reply)?;
    Ok(Answer { server, records, status, millis, authoritative })
}

fn query_tcp(server: SocketAddr, packet: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let mut s = TcpStream::connect_timeout(&server, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    let mut msg = (packet.len() as u16).to_be_bytes().to_vec();
    msg.extend_from_slice(packet);
    s.write_all(&msg)?;
    let mut len = [0u8; 2];
    s.read_exact(&mut len)?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn build_query(id: u16, name: &str, qtype: u16, recursion: bool) -> Result<Vec<u8>> {
    let mut p = Vec::with_capacity(64);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[if recursion { 0x01 } else { 0x00 }, 0x00]);
    p.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.').filter(|l| !l.is_empty()) {
        ensure!(label.len() <= 63, "\"{label}\" is too long for a name");
        p.push(label.len() as u8);
        p.extend_from_slice(label.as_bytes());
    }
    p.push(0);
    p.extend_from_slice(&qtype.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    Ok(p)
}

/// Reads a (possibly compressed) name at `pos`; returns it and the position
/// after it.
fn read_name(msg: &[u8], mut pos: usize) -> Result<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos).context("truncated name")? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 == 0xc0 {
            let ptr = ((len & 0x3f) << 8) | *msg.get(pos + 1).context("truncated name")? as usize;
            end.get_or_insert(pos + 2);
            jumps += 1;
            ensure!(jumps < 64, "name loop");
            pos = ptr;
            continue;
        }
        let label = msg.get(pos + 1..pos + 1 + len).context("truncated name")?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        pos += 1 + len;
    }
    Ok((labels.join("."), end.unwrap_or(pos)))
}

fn u16_at(msg: &[u8], pos: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(msg.get(pos..pos + 2).context("truncated")?.try_into()?))
}

fn u32_at(msg: &[u8], pos: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(msg.get(pos..pos + 4).context("truncated")?.try_into()?))
}

pub fn parse_reply(msg: &[u8]) -> Result<(&'static str, bool, Vec<Record>)> {
    ensure!(msg.len() >= 12, "short answer");
    let status = match msg[3] & 0x0f {
        0 => "NOERROR",
        1 => "FORMERR",
        2 => "SERVFAIL",
        3 => "NXDOMAIN",
        4 => "NOTIMP",
        5 => "REFUSED",
        _ => "ERROR",
    };
    let authoritative = msg[2] & 0x04 != 0;
    let qd = u16_at(msg, 4)?;
    let an = u16_at(msg, 6)? as usize + u16_at(msg, 8)? as usize + u16_at(msg, 10)? as usize;
    let mut pos = 12;
    for _ in 0..qd {
        pos = read_name(msg, pos)?.1 + 4;
    }
    let mut records = Vec::new();
    for _ in 0..an {
        let (name, p) = read_name(msg, pos)?;
        let rtype = u16_at(msg, p)?;
        let ttl = u32_at(msg, p + 4)?;
        let len = u16_at(msg, p + 8)? as usize;
        let start = p + 10;
        let rdata = msg.get(start..start + len).context("truncated record")?;
        pos = start + len;
        // Only answers and the SOA of a negative answer are shown.
        let Some(kind) = RecordType::from_code(rtype) else { continue };
        let data = match kind {
            RecordType::A if len == 4 => Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]).to_string(),
            RecordType::Aaaa if len == 16 => {
                std::net::Ipv6Addr::from(<[u8; 16]>::try_from(rdata).unwrap_or([0; 16])).to_string()
            }
            RecordType::Cname | RecordType::Ns | RecordType::Ptr => read_name(msg, start)?.0,
            RecordType::Mx => format!("{} {}", u16_at(msg, start)?, read_name(msg, start + 2)?.0),
            RecordType::Txt => {
                let mut parts = Vec::new();
                let mut i = 0;
                while i < rdata.len() {
                    let l = rdata[i] as usize;
                    parts.push(String::from_utf8_lossy(rdata.get(i + 1..i + 1 + l).unwrap_or_default()).into_owned());
                    i += 1 + l;
                }
                format!("\"{}\"", parts.join(""))
            }
            RecordType::Soa => {
                let (mname, p1) = read_name(msg, start)?;
                let (rname, p2) = read_name(msg, p1)?;
                format!(
                    "{mname} {rname} serial {} refresh {} retry {} expire {} minimum {}",
                    u32_at(msg, p2)?,
                    u32_at(msg, p2 + 4)?,
                    u32_at(msg, p2 + 8)?,
                    u32_at(msg, p2 + 12)?,
                    u32_at(msg, p2 + 16)?
                )
            }
            RecordType::Srv => format!(
                "priority {} weight {} port {} {}",
                u16_at(msg, start)?,
                u16_at(msg, start + 2)?,
                u16_at(msg, start + 4)?,
                read_name(msg, start + 6)?.0
            ),
            RecordType::Caa if len >= 2 => {
                let tl = rdata[1] as usize;
                let tag = String::from_utf8_lossy(rdata.get(2..2 + tl).unwrap_or_default()).into_owned();
                let value = String::from_utf8_lossy(rdata.get(2 + tl..).unwrap_or_default()).into_owned();
                format!("{} {tag} \"{value}\"", rdata[0])
            }
            _ => continue,
        };
        records.push(Record { name, kind: kind.name().to_string(), ttl, data });
    }
    Ok((status, authoritative, records))
}

/// The name of a device on the local network: asks the router's DNS
/// (PTR), the device itself over multicast DNS (Apple devices, printers,
/// Linux with Avahi) and over NetBIOS (Windows).
pub fn device_name(ip: Ipv4Addr, dns: &[IpAddr]) -> Option<String> {
    let t = Duration::from_millis(600);
    for server in dns.iter().take(2) {
        if let Ok(a) = query(SocketAddr::new(*server, 53), &ip.to_string(), RecordType::Ptr, t)
            && let Some(r) = a.records.iter().find(|r| r.kind == "PTR")
        {
            let name = r.data.trim_end_matches('.').trim_end_matches(".local").trim_end_matches(".lan").to_string();
            if !name.is_empty() && !name.contains("in-addr") {
                return Some(name);
            }
        }
    }
    // Multicast DNS, asked of the device directly (a "legacy unicast" query).
    if let Ok(a) = query(SocketAddr::new(IpAddr::V4(ip), 5353), &ip.to_string(), RecordType::Ptr, t)
        && let Some(r) = a.records.iter().find(|r| r.kind == "PTR")
    {
        return Some(r.data.trim_end_matches('.').trim_end_matches(".local").to_string());
    }
    netbios_name(ip, t)
}

/// NetBIOS node status: the computer name Windows machines answer with.
pub fn netbios_name(ip: Ipv4Addr, timeout: Duration) -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_read_timeout(Some(timeout)).ok()?;
    let r = random_bytes();
    let mut p = vec![r[0], r[1], 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0x20];
    // "*" padded with zeros, first-level encoded (each nibble + 'A').
    let mut name = [0u8; 16];
    name[0] = b'*';
    for b in name {
        p.push(b'A' + (b >> 4));
        p.push(b'A' + (b & 0xf));
    }
    p.extend_from_slice(&[0, 0, 0x21, 0, 1]);
    sock.send_to(&p, (ip, 137)).ok()?;
    let mut buf = [0u8; 1024];
    let n = sock.recv(&mut buf).ok()?;
    let msg = &buf[..n];
    // Header (12) + name (34) + type, class, ttl, length (10) = 56.
    let count = *msg.get(56)? as usize;
    for i in 0..count {
        let e = msg.get(57 + i * 18..57 + i * 18 + 18)?;
        let (suffix, flags) = (e[15], e[16]);
        // The workstation name: suffix 0x00, unique (not a group).
        if suffix == 0 && flags & 0x80 == 0 {
            let s = String::from_utf8_lossy(&e[..15]).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_names() {
        assert_eq!(reverse_name("192.168.1.20".parse().unwrap()), "20.1.168.192.in-addr.arpa");
        assert!(reverse_name("2001:db8::1".parse().unwrap()).ends_with("8.b.d.0.1.0.0.2.ip6.arpa"));
    }

    #[test]
    fn round_trip() {
        // A query for example.com, answered with one A record (compressed name).
        let mut msg = build_query(0x1234, "example.com", 1, true).unwrap();
        msg[2] |= 0x80;
        msg[7] = 1;
        msg.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0x0e, 0x10, 0, 4, 93, 184, 215, 14]);
        let (status, _, records) = parse_reply(&msg).unwrap();
        assert_eq!(status, "NOERROR");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "example.com");
        assert_eq!(records[0].data, "93.184.215.14");
        assert_eq!(records[0].ttl, 3600);
    }

    #[test]
    fn mx_and_txt() {
        let mut msg = build_query(1, "a.io", 15, true).unwrap();
        msg[7] = 2;
        // MX 10 mail.a.io (mail + pointer to a.io at offset 12)
        msg.extend_from_slice(&[0xc0, 12, 0, 15, 0, 1, 0, 0, 0, 60, 0, 9, 0, 10, 4, b'm', b'a', b'i', b'l', 0xc0, 12]);
        // TXT "v=1"
        msg.extend_from_slice(&[0xc0, 12, 0, 16, 0, 1, 0, 0, 0, 60, 0, 4, 3, b'v', b'=', b'1']);
        let (_, _, r) = parse_reply(&msg).unwrap();
        assert_eq!(r[0].data, "10 mail.a.io");
        assert_eq!(r[1].data, "\"v=1\"");
    }

    #[test]
    fn rejects_loops_and_garbage() {
        assert!(parse_reply(&[0; 5]).is_err());
        let mut msg = build_query(1, "x", 1, true).unwrap();
        msg[7] = 1;
        msg.extend_from_slice(&[0xc0, msg.len() as u8]);
        assert!(parse_reply(&msg).is_err());
    }
}
