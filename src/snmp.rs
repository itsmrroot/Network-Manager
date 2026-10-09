//! SNMP v1 and v2c: reading a switch's or router's name, uptime and the
//! traffic, errors and state of every interface, and walking any part of
//! its MIB.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, Default)]
pub enum Version {
    V1,
    #[default]
    V2c,
}

/// An object identifier, such as 1.3.6.1.2.1.1.5.0.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid(pub Vec<u32>);

impl Oid {
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim().trim_start_matches('.');
        let parts: Result<Vec<u32>, _> = s.split('.').map(str::parse).collect();
        let parts =
            parts.ok().filter(|p| p.len() >= 2).with_context(|| format!("\"{s}\" is not an OID like 1.3.6.1.2.1.1"))?;
        Ok(Oid(parts))
    }

    pub fn starts_with(&self, prefix: &Oid) -> bool {
        self.0.starts_with(&prefix.0)
    }

    pub fn child(&self, n: u32) -> Oid {
        let mut v = self.0.clone();
        v.push(n);
        Oid(v)
    }

    /// The last number: the row index of a table column.
    pub fn last(&self) -> u32 {
        self.0.last().copied().unwrap_or(0)
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(u32::to_string).collect();
        f.write_str(&parts.join("."))
    }
}

impl Serialize for Oid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// A value in an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Value {
    Integer(i64),
    Text(Vec<u8>),
    Null,
    Oid(Oid),
    IpAddress(Ipv4Addr),
    Counter32(u32),
    Gauge32(u32),
    TimeTicks(u32),
    Opaque(Vec<u8>),
    Counter64(u64),
    NoSuchObject,
    NoSuchInstance,
    EndOfMibView,
}

impl Value {
    /// The type, as MIB browsers name it.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Integer(_) => "INTEGER",
            Value::Text(_) => "STRING",
            Value::Null => "NULL",
            Value::Oid(_) => "OID",
            Value::IpAddress(_) => "IpAddress",
            Value::Counter32(_) => "Counter32",
            Value::Gauge32(_) => "Gauge32",
            Value::TimeTicks(_) => "Timeticks",
            Value::Opaque(_) => "Opaque",
            Value::Counter64(_) => "Counter64",
            Value::NoSuchObject => "noSuchObject",
            Value::NoSuchInstance => "noSuchInstance",
            Value::EndOfMibView => "endOfMibView",
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Integer(i) => u64::try_from(i).ok(),
            Value::Counter32(v) | Value::Gauge32(v) | Value::TimeTicks(v) => Some(v as u64),
            Value::Counter64(v) => Some(v),
            _ => None,
        }
    }

    /// Text values as text; bytes that are not text as hex.
    pub fn as_text(&self) -> String {
        match self {
            Value::Text(b) | Value::Opaque(b) => {
                let printable = b.iter().all(|&c| c == b'\r' || c == b'\n' || c == b'\t' || (0x20..0x7f).contains(&c));
                if printable || std::str::from_utf8(b).is_ok_and(|s| !s.chars().any(char::is_control)) {
                    String::from_utf8_lossy(b).trim_end_matches('\0').to_string()
                } else {
                    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
                }
            }
            other => other.to_string(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Integer(i) => write!(f, "{i}"),
            Value::Text(_) | Value::Opaque(_) => f.write_str(&self.as_text()),
            Value::Null => f.write_str(""),
            Value::Oid(o) => write!(f, "{o}"),
            Value::IpAddress(a) => write!(f, "{a}"),
            Value::Counter32(v) | Value::Gauge32(v) => write!(f, "{v}"),
            Value::TimeTicks(t) => f.write_str(&uptime(*t)),
            Value::Counter64(v) => write!(f, "{v}"),
            Value::NoSuchObject | Value::NoSuchInstance | Value::EndOfMibView => f.write_str(self.kind()),
        }
    }
}

/// Hundredths of a second as "12 d 03:04:05".
pub fn uptime(ticks: u32) -> String {
    let s = ticks as u64 / 100;
    let (d, h, m, s) = (s / 86400, s / 3600 % 24, s / 60 % 60, s % 60);
    if d > 0 { format!("{d} d {h:02}:{m:02}:{s:02}") } else { format!("{h:02}:{m:02}:{s:02}") }
}

// ---------------------------------------------------------------- BER

fn push_len(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = (len as u32).to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (4 - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
}

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    push_len(&mut out, body.len());
    out.extend_from_slice(body);
    out
}

fn int(v: i64) -> Vec<u8> {
    let bytes = v.to_be_bytes();
    let mut i = 0;
    // Shortest two's-complement form.
    while i < 7 && ((bytes[i] == 0 && bytes[i + 1] & 0x80 == 0) || (bytes[i] == 0xff && bytes[i + 1] & 0x80 != 0)) {
        i += 1;
    }
    tlv(0x02, &bytes[i..])
}

fn oid(o: &Oid) -> Vec<u8> {
    let p = &o.0;
    let mut body = vec![(p[0] * 40 + p[1]) as u8];
    for &n in &p[2..] {
        let mut chunk = vec![(n & 0x7f) as u8];
        let mut n = n >> 7;
        while n > 0 {
            chunk.push(0x80 | (n & 0x7f) as u8);
            n >>= 7;
        }
        chunk.reverse();
        body.extend(chunk);
    }
    tlv(0x06, &body)
}

const GET: u8 = 0xa0;
const GET_NEXT: u8 = 0xa1;
const RESPONSE: u8 = 0xa2;
const GET_BULK: u8 = 0xa5;

/// A request message. For GetBulk, `a` and `b` are non-repeaters and
/// max-repetitions; otherwise both 0.
pub fn encode(version: Version, community: &str, pdu: u8, id: i32, a: i64, b: i64, oids: &[Oid]) -> Vec<u8> {
    let varbinds: Vec<u8> = oids.iter().flat_map(|o| tlv(0x30, &[oid(o), vec![0x05, 0x00]].concat())).collect();
    let body = [int(id as i64), int(a), int(b), tlv(0x30, &varbinds)].concat();
    let msg = [
        int(match version {
            Version::V1 => 0,
            Version::V2c => 1,
        }),
        tlv(0x04, community.as_bytes()),
        tlv(pdu, &body),
    ]
    .concat();
    tlv(0x30, &msg)
}

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn next(&mut self) -> Result<(u8, &'a [u8])> {
        ensure!(self.b.len() >= 2, "short answer");
        let tag = self.b[0];
        let (len, head) = if self.b[1] & 0x80 == 0 {
            (self.b[1] as usize, 2)
        } else {
            let n = (self.b[1] & 0x7f) as usize;
            ensure!((1..=4).contains(&n) && self.b.len() >= 2 + n, "bad length");
            (self.b[2..2 + n].iter().fold(0usize, |a, &x| a << 8 | x as usize), 2 + n)
        };
        ensure!(self.b.len() >= head + len, "truncated answer");
        let body = &self.b[head..head + len];
        self.b = &self.b[head + len..];
        Ok((tag, body))
    }

    fn expect(&mut self, tag: u8) -> Result<&'a [u8]> {
        let (t, body) = self.next()?;
        ensure!(t == tag, "unexpected field {t:#x} (wanted {tag:#x})");
        Ok(body)
    }
}

fn read_int(b: &[u8]) -> i64 {
    let mut v: i64 = if b.first().is_some_and(|x| x & 0x80 != 0) { -1 } else { 0 };
    for &x in b.iter().take(8) {
        v = v << 8 | x as i64;
    }
    v
}

fn read_uint(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, &x| a << 8 | x as u64)
}

fn read_oid(b: &[u8]) -> Result<Oid> {
    ensure!(!b.is_empty(), "empty OID");
    let mut out = vec![(b[0] / 40) as u32, (b[0] % 40) as u32];
    let mut n: u32 = 0;
    for &x in &b[1..] {
        n = n.checked_shl(7).context("OID too long")? | (x & 0x7f) as u32;
        if x & 0x80 == 0 {
            out.push(n);
            n = 0;
        }
    }
    Ok(Oid(out))
}

fn read_value(tag: u8, b: &[u8]) -> Result<Value> {
    Ok(match tag {
        0x02 => Value::Integer(read_int(b)),
        0x04 => Value::Text(b.to_vec()),
        0x05 => Value::Null,
        0x06 => Value::Oid(read_oid(b)?),
        0x40 if b.len() == 4 => Value::IpAddress(Ipv4Addr::new(b[0], b[1], b[2], b[3])),
        0x41 => Value::Counter32(read_uint(b) as u32),
        0x42 => Value::Gauge32(read_uint(b) as u32),
        0x43 => Value::TimeTicks(read_uint(b) as u32),
        0x44 => Value::Opaque(b.to_vec()),
        0x46 => Value::Counter64(read_uint(b)),
        0x80 => Value::NoSuchObject,
        0x81 => Value::NoSuchInstance,
        0x82 => Value::EndOfMibView,
        _ => Value::Opaque(b.to_vec()),
    })
}

/// A response: its request ID, error status and index, and variable bindings.
pub struct Response {
    pub id: i32,
    pub error: i64,
    pub index: i64,
    pub varbinds: Vec<(Oid, Value)>,
}

pub fn decode(msg: &[u8]) -> Result<Response> {
    let mut top = Reader { b: msg };
    let mut m = Reader { b: top.expect(0x30)? };
    m.expect(0x02)?;
    m.expect(0x04)?;
    let mut pdu = Reader { b: m.expect(RESPONSE)? };
    let id = read_int(pdu.expect(0x02)?) as i32;
    let error = read_int(pdu.expect(0x02)?);
    let index = read_int(pdu.expect(0x02)?);
    let mut list = Reader { b: pdu.expect(0x30)? };
    let mut varbinds = Vec::new();
    while !list.b.is_empty() {
        let mut vb = Reader { b: list.expect(0x30)? };
        let o = read_oid(vb.expect(0x06)?)?;
        let (tag, body) = vb.next()?;
        varbinds.push((o, read_value(tag, body)?));
    }
    Ok(Response { id, error, index, varbinds })
}

/// What SNMP's error-status numbers mean.
fn error_text(e: i64) -> &'static str {
    match e {
        1 => "the answer would be too big",
        2 => "no such name (the device does not have this object)",
        3 => "bad value",
        4 => "read only",
        5 => "general error",
        6 => "no access (check the community)",
        _ => "error",
    }
}

// ---------------------------------------------------------------- client

pub struct Client {
    sock: UdpSocket,
    community: String,
    pub version: Version,
    retries: u32,
    next_id: std::cell::Cell<i32>,
}

impl Client {
    /// `host` is an address or name, with `:port` when it is not 161.
    pub fn new(host: &str, community: &str, version: Version) -> Result<Self> {
        let target: SocketAddr = if let Ok(a) = host.parse::<SocketAddr>() {
            a
        } else if let Ok(ip) = host.parse::<IpAddr>() {
            SocketAddr::new(ip, 161)
        } else {
            let with_port = if host.contains(':') { host.to_string() } else { format!("{host}:161") };
            with_port.to_socket_addrs()?.next().with_context(|| format!("{host} was not found"))?
        };
        let bind: SocketAddr = if target.is_ipv4() { "0.0.0.0:0".parse()? } else { "[::]:0".parse()? };
        let sock = UdpSocket::bind(bind)?;
        sock.connect(target)?;
        sock.set_read_timeout(Some(Duration::from_millis(1500)))?;
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos());
        Ok(Self {
            sock,
            community: community.to_string(),
            version,
            retries: 2,
            next_id: std::cell::Cell::new((seed & 0x3fff_ffff) as i32),
        })
    }

    fn request(&self, pdu: u8, a: i64, b: i64, oids: &[Oid]) -> Result<Vec<(Oid, Value)>> {
        let id = self.next_id.get().wrapping_add(1) & 0x7fff_ffff;
        self.next_id.set(id);
        let msg = encode(self.version, &self.community, pdu, id, a, b, oids);
        let mut buf = vec![0u8; 65535];
        for _ in 0..=self.retries {
            self.sock.send(&msg)?;
            loop {
                let n = match self.sock.recv(&mut buf) {
                    Ok(n) => n,
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        break;
                    }
                    Err(e) => return Err(e).context("the device refused SNMP (is it enabled?)"),
                };
                let Ok(r) = decode(&buf[..n]) else { continue };
                if r.id != id {
                    continue; // a late answer to an earlier try
                }
                if r.error != 0 {
                    bail!("the device answered: {}", error_text(r.error));
                }
                return Ok(r.varbinds);
            }
        }
        bail!("no answer: check the address, the community and that SNMP is enabled and allowed from this computer")
    }

    pub fn get(&self, oids: &[Oid]) -> Result<Vec<(Oid, Value)>> {
        self.request(GET, 0, 0, oids)
    }

    /// Everything under `root`, in order (at most `limit` values).
    pub fn walk(&self, root: &Oid, limit: usize) -> Result<Vec<(Oid, Value)>> {
        let mut out = Vec::new();
        let mut at = root.clone();
        while out.len() < limit {
            let page = match self.version {
                Version::V2c => self.request(GET_BULK, 0, 25, std::slice::from_ref(&at))?,
                Version::V1 => match self.request(GET_NEXT, 0, 0, std::slice::from_ref(&at)) {
                    Ok(v) => v,
                    // v1 ends a walk with noSuchName.
                    Err(e) if e.to_string().contains("no such name") => break,
                    Err(e) => return Err(e),
                },
            };
            let mut done = page.is_empty();
            for (o, v) in page {
                if !o.starts_with(root) || matches!(v, Value::EndOfMibView) || o <= at {
                    done = true;
                    break;
                }
                at = o.clone();
                out.push((o, v));
            }
            if done {
                break;
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------- MIB-II

pub const SYSTEM: &str = "1.3.6.1.2.1.1";
pub const IF_TABLE: &str = "1.3.6.1.2.1.2.2.1";
pub const IFX_TABLE: &str = "1.3.6.1.2.1.31.1.1.1";
pub const LLDP_REMOTE: &str = "1.0.8802.1.1.2.1.4.1.1";

/// Starting points for walks: label, OID.
pub const WALK_PRESETS: [(&str, &str); 7] = [
    ("System", SYSTEM),
    ("Interfaces", "1.3.6.1.2.1.2"),
    ("Interface names and 64-bit counters", "1.3.6.1.2.1.31.1.1"),
    ("IP addresses", "1.3.6.1.2.1.4.20"),
    ("ARP table", "1.3.6.1.2.1.4.22"),
    ("LLDP neighbors", LLDP_REMOTE),
    ("Everything (MIB-2)", "1.3.6.1.2.1"),
];

#[derive(Debug, Clone, Default, Serialize)]
pub struct System {
    pub name: String,
    pub description: String,
    pub uptime: Option<u32>,
    pub contact: String,
    pub location: String,
    pub object_id: String,
}

pub fn system(c: &Client) -> Result<System> {
    let base = Oid::parse(SYSTEM)?;
    let oids: Vec<Oid> = [1, 2, 3, 4, 5, 6].iter().map(|&n| base.child(n).child(0)).collect();
    let got = c.get(&oids)?;
    let mut s = System::default();
    for (o, v) in got {
        let text = v.as_text();
        match o.0.get(7) {
            Some(1) => s.description = text,
            Some(2) => s.object_id = text,
            Some(3) => s.uptime = v.as_u64().map(|t| t as u32),
            Some(4) => s.contact = text,
            Some(5) => s.name = text,
            Some(6) => s.location = text,
            _ => {}
        }
    }
    Ok(s)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Interface {
    pub index: u32,
    /// ifName (Gi1/0/1) or else ifDescr.
    pub name: String,
    pub description: String,
    /// ifAlias: the description set on the port.
    pub alias: String,
    pub admin_up: bool,
    pub oper_up: bool,
    /// Bits per second.
    pub speed: u64,
    pub mac: String,
    pub in_octets: u64,
    pub out_octets: u64,
    pub in_errors: u64,
    pub out_errors: u64,
    pub in_discards: u64,
    pub out_discards: u64,
}

pub fn interfaces(c: &Client) -> Result<Vec<Interface>> {
    use std::collections::BTreeMap;
    let mut rows: BTreeMap<u32, Interface> = BTreeMap::new();
    let table = Oid::parse(IF_TABLE)?;
    let xtable = Oid::parse(IFX_TABLE)?;
    fn walk_column(
        c: &Client,
        rows: &mut BTreeMap<u32, Interface>,
        root: Oid,
        set: &mut dyn FnMut(&mut Interface, &Value),
    ) -> Result<()> {
        for (o, v) in c.walk(&root, 10_000)? {
            let row = rows.entry(o.last()).or_insert_with(|| Interface { index: o.last(), ..Default::default() });
            set(row, &v);
        }
        Ok(())
    }
    macro_rules! column {
        ($base:expr, $col:expr, $f:expr) => {
            walk_column(c, &mut rows, $base.child($col), &mut $f)
        };
    }
    column!(table, 2, |r, v| r.description = v.as_text())?;
    if rows.is_empty() {
        bail!("the device has no interface table (IF-MIB)");
    }
    column!(table, 5, |r, v| r.speed = v.as_u64().unwrap_or(0))?;
    column!(table, 6, |r, v| {
        if let Value::Text(b) = v
            && b.len() == 6
        {
            r.mac = b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(":");
        }
    })?;
    column!(table, 7, |r, v| r.admin_up = v.as_u64() == Some(1))?;
    column!(table, 8, |r, v| r.oper_up = v.as_u64() == Some(1))?;
    column!(table, 10, |r, v| r.in_octets = v.as_u64().unwrap_or(0))?;
    column!(table, 13, |r, v| r.in_discards = v.as_u64().unwrap_or(0))?;
    column!(table, 14, |r, v| r.in_errors = v.as_u64().unwrap_or(0))?;
    column!(table, 16, |r, v| r.out_octets = v.as_u64().unwrap_or(0))?;
    column!(table, 19, |r, v| r.out_discards = v.as_u64().unwrap_or(0))?;
    column!(table, 20, |r, v| r.out_errors = v.as_u64().unwrap_or(0))?;
    // IF-MIB extensions: short names, 64-bit counters, fast speeds, port
    // descriptions. Older devices may not have them.
    if c.version == Version::V2c {
        let _ = column!(xtable, 1, |r, v| r.name = v.as_text());
        let _ = column!(xtable, 6, |r, v| r.in_octets = v.as_u64().unwrap_or(r.in_octets));
        let _ = column!(xtable, 10, |r, v| r.out_octets = v.as_u64().unwrap_or(r.out_octets));
        let _ = column!(xtable, 15, |r, v| {
            if let Some(mbps) = v.as_u64().filter(|m| *m > 0) {
                r.speed = mbps * 1_000_000;
            }
        });
    }
    let _ = column!(xtable, 18, |r, v| r.alias = v.as_text());
    Ok(rows
        .into_values()
        .map(|mut r| {
            if r.name.is_empty() {
                r.name = r.description.clone();
            }
            r
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_net_snmp() {
        // snmpget -v2c -c public HOST 1.3.6.1.2.1.1.5.0 with request ID 0x1234.
        let m = encode(Version::V2c, "public", GET, 0x1234, 0, 0, &[Oid::parse("1.3.6.1.2.1.1.5.0").unwrap()]);
        let want = [
            0x30, 0x27, 0x02, 0x01, 0x01, 0x04, 0x06, b'p', b'u', b'b', b'l', b'i', b'c', 0xa0, 0x1a, 0x02, 0x02, 0x12,
            0x34, 0x02, 0x01, 0x00, 0x02, 0x01, 0x00, 0x30, 0x0e, 0x30, 0x0c, 0x06, 0x08, 0x2b, 0x06, 0x01, 0x02, 0x01,
            0x01, 0x05, 0x00, 0x05, 0x00,
        ];
        assert_eq!(m, want);
    }

    #[test]
    fn integers_and_oids() {
        assert_eq!(int(0), [2, 1, 0]);
        assert_eq!(int(127), [2, 1, 127]);
        assert_eq!(int(128), [2, 2, 0, 128]);
        assert_eq!(int(-1), [2, 1, 0xff]);
        assert_eq!(read_int(&[0x00, 0x80]), 128);
        assert_eq!(read_int(&[0xff]), -1);
        let o = Oid::parse("1.3.6.1.4.1.9.9.46.1.3.1.1.4.1.300").unwrap();
        let enc = oid(&o);
        assert_eq!(read_oid(&enc[2..]).unwrap(), o);
        let big = Oid::parse("1.0.8802.1.1.2").unwrap();
        assert_eq!(read_oid(&oid(&big)[2..]).unwrap(), big);
        assert!(Oid::parse("1.3.x").is_err());
        assert_eq!(Oid::parse(".1.3.6").unwrap().to_string(), "1.3.6");
    }

    #[test]
    fn decodes_a_response() {
        // Response: sysName.0 = "core-sw1", sysUpTime.0 = 123456 ticks,
        // ifHCInOctets.3 = 2^40.
        let vb = |o: &str, v: Vec<u8>| tlv(0x30, &[oid(&Oid::parse(o).unwrap()), v].concat());
        let list = [
            vb("1.3.6.1.2.1.1.5.0", tlv(0x04, b"core-sw1")),
            vb("1.3.6.1.2.1.1.3.0", tlv(0x43, &[0x01, 0xe2, 0x40])),
            vb("1.3.6.1.2.1.31.1.1.1.6.3", tlv(0x46, &[0x01, 0, 0, 0, 0, 0])),
            vb("1.3.6.1.2.1.1.9.0", vec![0x81, 0x00]),
        ]
        .concat();
        let pdu = [int(77), int(0), int(0), tlv(0x30, &list)].concat();
        let msg = tlv(0x30, &[int(1), tlv(0x04, b"public"), tlv(RESPONSE, &pdu)].concat());
        let r = decode(&msg).unwrap();
        assert_eq!(r.id, 77);
        assert_eq!(r.varbinds[0].1.as_text(), "core-sw1");
        assert_eq!(r.varbinds[1].1, Value::TimeTicks(123456));
        assert_eq!(r.varbinds[1].1.to_string(), "00:20:34");
        assert_eq!(r.varbinds[2].1.as_u64(), Some(1 << 40));
        assert_eq!(r.varbinds[2].0.last(), 3);
        assert_eq!(r.varbinds[3].1, Value::NoSuchInstance);
        assert!(decode(&msg[..msg.len() - 3]).is_err());
    }

    #[test]
    fn long_lengths() {
        let body = vec![b'a'; 300];
        let t = tlv(0x04, &body);
        assert_eq!(&t[..4], &[0x04, 0x82, 0x01, 0x2c]);
        let mut r = Reader { b: &t };
        assert_eq!(r.next().unwrap().1.len(), 300);
    }

    #[test]
    fn text_or_hex() {
        assert_eq!(Value::Text(b"Gi1/0/1".to_vec()).as_text(), "Gi1/0/1");
        assert_eq!(Value::Text(vec![0x00, 0x1a, 0x2b]).as_text(), "00:1a:2b");
        assert_eq!(uptime(100 * 86400 * 3 + 100 * 61), "3 d 00:01:01");
    }
}
