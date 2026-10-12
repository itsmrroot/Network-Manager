//! SNMPv3's user-based security (RFC 3414, 3826 and 7860): keys made from
//! passwords, signed and encrypted messages, and the v3 message format.

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::snmp::{Oid, Reader, int, read_int, read_uint, tlv};

/// How messages are signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Auth {
    /// Not signed (noAuthNoPriv).
    #[default]
    None,
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl Auth {
    pub const ALL: [Auth; 7] =
        [Auth::None, Auth::Md5, Auth::Sha1, Auth::Sha224, Auth::Sha256, Auth::Sha384, Auth::Sha512];

    pub fn label(self) -> &'static str {
        match self {
            Auth::None => "None",
            Auth::Md5 => "MD5",
            Auth::Sha1 => "SHA",
            Auth::Sha224 => "SHA-224",
            Auth::Sha256 => "SHA-256",
            Auth::Sha384 => "SHA-384",
            Auth::Sha512 => "SHA-512",
        }
    }

    /// "md5", "sha", "sha1", "sha256", "SHA-256", …
    pub fn parse(s: &str) -> Option<Auth> {
        let s = s.to_lowercase().replace(['-', '_'], "");
        Some(match s.as_str() {
            "none" => Auth::None,
            "md5" => Auth::Md5,
            "sha" | "sha1" => Auth::Sha1,
            "sha224" => Auth::Sha224,
            "sha256" => Auth::Sha256,
            "sha384" => Auth::Sha384,
            "sha512" => Auth::Sha512,
            _ => return None,
        })
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        use sha2::Digest;
        match self {
            Auth::None => Vec::new(),
            Auth::Md5 => md5::Md5::digest(data).to_vec(),
            Auth::Sha1 => sha1::Sha1::digest(data).to_vec(),
            Auth::Sha224 => sha2::Sha224::digest(data).to_vec(),
            Auth::Sha256 => sha2::Sha256::digest(data).to_vec(),
            Auth::Sha384 => sha2::Sha384::digest(data).to_vec(),
            Auth::Sha512 => sha2::Sha512::digest(data).to_vec(),
        }
    }

    fn hmac(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        macro_rules! hmac {
            ($d:ty) => {{
                use hmac::Mac;
                let mut h = <hmac::Hmac<$d> as hmac::digest::KeyInit>::new_from_slice(key).expect("any key length");
                h.update(data);
                h.finalize().into_bytes().to_vec()
            }};
        }
        match self {
            Auth::None => Vec::new(),
            Auth::Md5 => hmac!(md5::Md5),
            Auth::Sha1 => hmac!(sha1::Sha1),
            Auth::Sha224 => hmac!(sha2::Sha224),
            Auth::Sha256 => hmac!(sha2::Sha256),
            Auth::Sha384 => hmac!(sha2::Sha384),
            Auth::Sha512 => hmac!(sha2::Sha512),
        }
    }

    /// The bytes of the signature a message carries.
    fn mac_len(self) -> usize {
        match self {
            Auth::None => 0,
            Auth::Md5 | Auth::Sha1 => 12,
            Auth::Sha224 => 16,
            Auth::Sha256 => 24,
            Auth::Sha384 => 32,
            Auth::Sha512 => 48,
        }
    }
}

/// How messages are encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Privacy {
    #[default]
    None,
    Des,
    Aes128,
    Aes192,
    Aes256,
    /// AES-192 with the key extension Cisco uses (net-snmp's "AES-192-C").
    Aes192Cisco,
    Aes256Cisco,
}

impl Privacy {
    pub const ALL: [Privacy; 7] = [
        Privacy::None,
        Privacy::Des,
        Privacy::Aes128,
        Privacy::Aes192,
        Privacy::Aes256,
        Privacy::Aes192Cisco,
        Privacy::Aes256Cisco,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Privacy::None => "None",
            Privacy::Des => "DES",
            Privacy::Aes128 => "AES-128",
            Privacy::Aes192 => "AES-192",
            Privacy::Aes256 => "AES-256",
            Privacy::Aes192Cisco => "AES-192 (Cisco)",
            Privacy::Aes256Cisco => "AES-256 (Cisco)",
        }
    }

    /// "des", "aes", "aes128", "aes256", "aes256c", "AES-256-C", …
    pub fn parse(s: &str) -> Option<Privacy> {
        let s = s.to_lowercase().replace(['-', '_'], "");
        Some(match s.as_str() {
            "none" => Privacy::None,
            "des" => Privacy::Des,
            "aes" | "aes128" => Privacy::Aes128,
            "aes192" => Privacy::Aes192,
            "aes256" => Privacy::Aes256,
            "aes192c" | "aes192cisco" => Privacy::Aes192Cisco,
            "aes256c" | "aes256cisco" => Privacy::Aes256Cisco,
            _ => return None,
        })
    }

    /// Bytes of key material needed (DES: key and pre-IV).
    fn key_len(self) -> usize {
        match self {
            Privacy::None => 0,
            Privacy::Des | Privacy::Aes128 => 16,
            Privacy::Aes192 | Privacy::Aes192Cisco => 24,
            Privacy::Aes256 | Privacy::Aes256Cisco => 32,
        }
    }
}

/// An SNMPv3 user, as configured on the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct User {
    pub name: String,
    pub auth: Auth,
    pub auth_password: String,
    pub privacy: Privacy,
    pub privacy_password: String,
}

impl User {
    /// "noAuthNoPriv", "authNoPriv" or "authPriv".
    pub fn level(&self) -> &'static str {
        match (self.auth, self.privacy) {
            (Auth::None, _) => "noAuthNoPriv",
            (_, Privacy::None) => "authNoPriv",
            _ => "authPriv",
        }
    }

    /// The messages are meant for people.
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.trim().is_empty(), "Enter the SNMPv3 user name.");
        ensure!(
            self.privacy == Privacy::None || self.auth != Auth::None,
            "Encryption needs authentication too: choose an authentication protocol."
        );
        if self.auth != Auth::None {
            ensure!(self.auth_password.len() >= 8, "The authentication password must have at least 8 characters.");
        }
        if self.privacy != Privacy::None {
            ensure!(self.privacy_password.len() >= 8, "The encryption password must have at least 8 characters.");
        }
        Ok(())
    }
}

/// RFC 3414 A.2: a password stretched to 1 MB, hashed.
pub fn password_to_key(auth: Auth, password: &[u8]) -> Vec<u8> {
    if password.is_empty() || auth == Auth::None {
        return Vec::new();
    }
    const MB: usize = 1 << 20;
    let mut buf = Vec::with_capacity(MB + password.len());
    while buf.len() < MB {
        buf.extend_from_slice(password);
    }
    buf.truncate(MB);
    auth.digest(&buf)
}

/// The key for one engine (device): H(Ku ‖ engine ID ‖ Ku).
pub fn localize(auth: Auth, ku: &[u8], engine: &[u8]) -> Vec<u8> {
    auth.digest(&[ku, engine, ku].concat())
}

/// A user's keys, before they are tied to an engine (hashing a password
/// takes a few milliseconds, so it is done once).
#[derive(Clone)]
pub struct Master {
    pub user: User,
    auth_ku: Vec<u8>,
    priv_ku: Vec<u8>,
}

impl Master {
    pub fn new(user: &User) -> Self {
        Self {
            user: user.clone(),
            auth_ku: password_to_key(user.auth, user.auth_password.as_bytes()),
            priv_ku: if user.privacy == Privacy::None {
                Vec::new()
            } else {
                password_to_key(user.auth, user.privacy_password.as_bytes())
            },
        }
    }

    /// The keys for messages to and from `engine`.
    pub fn localize(&self, engine: &[u8]) -> Keys {
        let (auth, privacy) = (self.user.auth, self.user.privacy);
        let auth_key = if auth == Auth::None { Vec::new() } else { localize(auth, &self.auth_ku, engine) };
        let mut priv_key = if privacy == Privacy::None { Vec::new() } else { localize(auth, &self.priv_ku, engine) };
        // AES-192 and AES-256 need longer keys than MD5 and SHA give.
        while priv_key.len() < privacy.key_len() {
            let more = match privacy {
                // Reeder: the key so far as a password, localized again.
                Privacy::Aes192Cisco | Privacy::Aes256Cisco => {
                    localize(auth, &password_to_key(auth, &priv_key), engine)
                }
                // Blumenthal: the hash of the key so far.
                _ => auth.digest(&priv_key),
            };
            if more.is_empty() {
                break;
            }
            priv_key.extend(more);
        }
        Keys { auth, privacy, auth_key, priv_key }
    }
}

/// A user's keys for one engine.
#[derive(Clone, Default)]
pub struct Keys {
    pub auth: Auth,
    pub privacy: Privacy,
    auth_key: Vec<u8>,
    priv_key: Vec<u8>,
}

impl Keys {
    fn flags(&self) -> u8 {
        let mut f = 0;
        if self.auth != Auth::None {
            f |= AUTH;
        }
        if self.privacy != Privacy::None {
            f |= PRIV;
        }
        f
    }
}

pub const AUTH: u8 = 1;
pub const PRIV: u8 = 2;
pub const REPORTABLE: u8 = 4;

// ---------------------------------------------------------------- privacy

/// Encrypts one block in place.
type BlockFn = Box<dyn Fn(&mut [u8; 16])>;

fn aes_encryptor(key: &[u8]) -> Result<BlockFn> {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    macro_rules! make {
        ($c:ty) => {{
            let c = <$c>::new_from_slice(key).map_err(|_| anyhow!("bad AES key"))?;
            Box::new(move |b: &mut [u8; 16]| {
                let mut x = aes::cipher::Block::<$c>::default();
                x.copy_from_slice(b);
                c.encrypt_block(&mut x);
                b.copy_from_slice(&x);
            }) as BlockFn
        }};
    }
    Ok(match key.len() {
        16 => make!(aes::Aes128),
        24 => make!(aes::Aes192),
        32 => make!(aes::Aes256),
        n => bail!("an AES key of {n} bytes"),
    })
}

/// AES in 128-bit cipher feedback mode (RFC 3826).
fn aes_cfb(key: &[u8], iv: [u8; 16], data: &[u8], decrypt: bool) -> Result<Vec<u8>> {
    let enc = aes_encryptor(key)?;
    let mut feedback = iv;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut stream = feedback;
        enc(&mut stream);
        let start = out.len();
        out.extend(chunk.iter().zip(stream).map(|(a, b)| a ^ b));
        // The next block's feedback is this block's ciphertext.
        let cipher = if decrypt { chunk } else { &out[start..] };
        feedback[..cipher.len()].copy_from_slice(cipher);
    }
    Ok(out)
}

/// DES in cipher block chaining mode (RFC 3414 8.1.1).
fn des_cbc(key: &[u8], iv: [u8; 8], data: &[u8], decrypt: bool) -> Result<Vec<u8>> {
    use des::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
    let c = des::Des::new_from_slice(key).map_err(|_| anyhow!("bad DES key"))?;
    ensure!(!decrypt || data.len().is_multiple_of(8), "the encrypted part is not whole DES blocks");
    let mut prev = iv;
    let mut out = Vec::with_capacity(data.len() + 8);
    for chunk in data.chunks(8) {
        let mut block = [0u8; 8];
        block[..chunk.len()].copy_from_slice(chunk);
        let mut x = des::cipher::Block::<des::Des>::default();
        if decrypt {
            x.copy_from_slice(&block);
            c.decrypt_block(&mut x);
            out.extend(x.iter().zip(prev).map(|(a, b)| a ^ b));
            prev = block;
        } else {
            for (i, b) in block.iter_mut().enumerate() {
                *b ^= prev[i];
            }
            x.copy_from_slice(&block);
            c.encrypt_block(&mut x);
            prev.copy_from_slice(&x);
            out.extend_from_slice(&x);
        }
    }
    Ok(out)
}

/// Encrypts a scoped PDU; returns it and the privacy parameters (the salt).
fn encrypt(keys: &Keys, boots: u32, time: u32, salt: u64, plain: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let k = &keys.priv_key;
    ensure!(k.len() >= keys.privacy.key_len(), "the privacy key is too short");
    match keys.privacy {
        Privacy::None => Ok((plain.to_vec(), Vec::new())),
        Privacy::Des => {
            let mut s = [0u8; 8];
            s[..4].copy_from_slice(&boots.to_be_bytes());
            s[4..].copy_from_slice(&(salt as u32).to_be_bytes());
            let mut iv = [0u8; 8];
            for i in 0..8 {
                iv[i] = k[8 + i] ^ s[i];
            }
            Ok((des_cbc(&k[..8], iv, plain, false)?, s.to_vec()))
        }
        p => {
            let s = salt.to_be_bytes();
            let n = p.key_len();
            Ok((aes_cfb(&k[..n], aes_iv(boots, time, &s), plain, false)?, s.to_vec()))
        }
    }
}

fn decrypt(keys: &Keys, boots: u32, time: u32, params: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let k = &keys.priv_key;
    ensure!(params.len() == 8, "the message's privacy parameters are damaged");
    ensure!(k.len() >= keys.privacy.key_len(), "the privacy key is too short");
    match keys.privacy {
        Privacy::None => bail!("the message is encrypted, but no privacy protocol is set"),
        Privacy::Des => {
            let mut iv = [0u8; 8];
            for i in 0..8 {
                iv[i] = k[8 + i] ^ params[i];
            }
            des_cbc(&k[..8], iv, data, true)
        }
        p => aes_cfb(&k[..p.key_len()], aes_iv(boots, time, params), data, true),
    }
}

fn aes_iv(boots: u32, time: u32, salt: &[u8]) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[..4].copy_from_slice(&boots.to_be_bytes());
    iv[4..8].copy_from_slice(&time.to_be_bytes());
    iv[8..].copy_from_slice(&salt[..8]);
    iv
}

// ---------------------------------------------------------------- messages

/// The header bytes of a BER field with a body of `len` bytes.
fn head(len: usize) -> usize {
    if len < 0x80 { 2 } else { 2 + (usize::BITS - len.leading_zeros()).div_ceil(8) as usize }
}

/// Everything a v3 message needs besides its scoped PDU.
pub struct Header<'a> {
    pub msg_id: i32,
    pub reportable: bool,
    pub engine: &'a [u8],
    pub boots: u32,
    pub time: u32,
    pub user: &'a str,
    /// A number used once, for encryption.
    pub salt: u64,
}

/// A v3 message carrying `scoped` (a scoped PDU), signed and encrypted as
/// `keys` say.
pub fn encode(h: &Header, keys: &Keys, scoped: &[u8]) -> Result<Vec<u8>> {
    let mut flags = keys.flags();
    if h.reportable {
        flags |= REPORTABLE;
    }
    let (data, priv_params) = if keys.privacy == Privacy::None {
        (scoped.to_vec(), Vec::new())
    } else {
        let (c, p) = encrypt(keys, h.boots, h.time, h.salt, scoped)?;
        (tlv(0x04, &c), p)
    };
    let mac_len = keys.auth.mac_len();
    let before = [tlv(0x04, h.engine), int(h.boots as i64), int(h.time as i64), tlv(0x04, h.user.as_bytes())].concat();
    let inner = [before.clone(), tlv(0x04, &vec![0; mac_len]), tlv(0x04, &priv_params)].concat();
    let sec = tlv(0x30, &inner);
    let version = int(3);
    let global = tlv(0x30, &[int(h.msg_id as i64), int(65507), tlv(0x04, &[flags]), int(3)].concat());
    let body = [version.clone(), global.clone(), tlv(0x04, &sec), data].concat();
    let mut msg = tlv(0x30, &body);
    if mac_len > 0 {
        // Signed with the signature's place filled with zeros.
        let at = head(body.len())
            + version.len()
            + global.len()
            + head(sec.len())
            + head(inner.len())
            + before.len()
            + head(mac_len);
        let mac = keys.auth.hmac(&keys.auth_key, &msg);
        msg[at..at + mac_len].copy_from_slice(&mac[..mac_len]);
    }
    Ok(msg)
}

/// A received v3 message, not yet checked or decrypted.
pub struct Incoming<'a> {
    raw: &'a [u8],
    pub msg_id: i32,
    pub flags: u8,
    pub engine: Vec<u8>,
    pub boots: u32,
    pub time: u32,
    pub user: String,
    /// Where the signature is in `raw`, and how long it is.
    auth_at: usize,
    auth_len: usize,
    privacy: Vec<u8>,
    data: &'a [u8],
}

/// Reads a v3 message's header.
pub fn parse(msg: &[u8]) -> Result<Incoming<'_>> {
    let mut top = Reader { b: msg };
    let mut m = Reader { b: top.expect(0x30)? };
    ensure!(read_int(m.expect(0x02)?) == 3, "not an SNMPv3 message");
    let mut global = Reader { b: m.expect(0x30)? };
    let msg_id = read_int(global.expect(0x02)?) as i32;
    global.expect(0x02)?;
    let flags = *global.expect(0x04)?.first().context("no message flags")?;
    ensure!(read_int(global.expect(0x02)?) == 3, "not the user-based security model");
    let mut octet = Reader { b: m.expect(0x04)? };
    let mut sec = Reader { b: octet.expect(0x30)? };
    let engine = sec.expect(0x04)?.to_vec();
    let boots = read_uint(sec.expect(0x02)?) as u32;
    let time = read_uint(sec.expect(0x02)?) as u32;
    let user = String::from_utf8_lossy(sec.expect(0x04)?).to_string();
    let auth = sec.expect(0x04)?;
    let privacy = sec.expect(0x04)?.to_vec();
    Ok(Incoming {
        raw: msg,
        msg_id,
        flags,
        engine,
        boots,
        time,
        user,
        auth_at: auth.as_ptr() as usize - msg.as_ptr() as usize,
        auth_len: auth.len(),
        privacy,
        data: m.b,
    })
}

impl Incoming<'_> {
    /// Checks the signature, when the message has one.
    pub fn verify(&self, keys: &Keys) -> Result<()> {
        if self.flags & AUTH == 0 {
            return Ok(());
        }
        ensure!(keys.auth != Auth::None, "the message is signed, but no authentication protocol is set");
        let n = keys.auth.mac_len();
        ensure!(self.auth_len == n, "the signature does not fit the authentication protocol");
        let mut copy = self.raw.to_vec();
        copy[self.auth_at..self.auth_at + n].fill(0);
        let mac = keys.auth.hmac(&keys.auth_key, &copy);
        let got = &self.raw[self.auth_at..self.auth_at + n];
        // Every byte compared, so timing tells nothing.
        let diff = mac[..n].iter().zip(got).fold(0u8, |d, (a, b)| d | (a ^ b));
        ensure!(diff == 0, "wrong signature: the authentication password or protocol differs");
        Ok(())
    }

    /// The scoped PDU, decrypted when needed.
    pub fn scoped(&self, keys: &Keys) -> Result<Vec<u8>> {
        if self.flags & PRIV == 0 {
            return Ok(self.data.to_vec());
        }
        let enc = Reader { b: self.data }.expect(0x04)?;
        decrypt(keys, self.boots, self.time, &self.privacy, enc)
    }
}

/// A scoped PDU's context engine ID, context name, PDU type and PDU body.
pub type Scoped = (Vec<u8>, Vec<u8>, u8, Vec<u8>);

pub fn scoped_pdu(scoped: &[u8]) -> Result<Scoped> {
    let mut r = Reader { b: scoped };
    let mut s = Reader {
        b: r.expect(0x30).map_err(|_| anyhow!("the message could not be decrypted: check the privacy password"))?,
    };
    let engine = s.expect(0x04)?.to_vec();
    let name = s.expect(0x04)?.to_vec();
    let (tag, body) = s.next()?;
    Ok((engine, name, tag, body.to_vec()))
}

/// The usmStats counters a device reports problems with.
pub const NOT_IN_TIME_WINDOW: &str = "1.3.6.1.6.3.15.1.1.2.0";
pub const UNKNOWN_ENGINE_ID: &str = "1.3.6.1.6.3.15.1.1.4.0";

/// What a report means.
pub fn report_text(oid: &Oid) -> String {
    match oid.to_string().as_str() {
        "1.3.6.1.6.3.15.1.1.1.0" => "this user may not use this security level (check authentication and privacy)",
        "1.3.6.1.6.3.15.1.1.2.0" => "the device's clock check failed",
        "1.3.6.1.6.3.15.1.1.3.0" => "the device does not know this user",
        "1.3.6.1.6.3.15.1.1.4.0" => "the device did not recognise its own engine ID",
        "1.3.6.1.6.3.15.1.1.5.0" => "wrong authentication password or protocol",
        "1.3.6.1.6.3.15.1.1.6.0" => "wrong privacy password or protocol",
        "1.3.6.1.6.3.11.2.1.3.0" => "the device cannot handle this request",
        "1.3.6.1.6.3.12.1.4.0" | "1.3.6.1.6.3.12.1.5.0" => "the device does not know this context",
        o => return format!("report {o}"),
    }
    .to_string()
}

/// "80 00 1f 88 …" or "0x80001f88…" to bytes.
pub fn parse_engine_id(s: &str) -> Result<Vec<u8>> {
    let hex: String = s.trim().trim_start_matches("0x").chars().filter(|c| !c.is_whitespace() && *c != ':').collect();
    ensure!(hex.len().is_multiple_of(2) && (10..=64).contains(&hex.len()), "an engine ID has 5 to 32 bytes in hex");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| anyhow!("\"{s}\" is not hexadecimal")))
        .collect()
}

pub fn engine_hex(id: &[u8]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// A new engine ID for this computer (RFC 3411: enterprise bit, format 5,
/// then 8 random bytes).
pub fn new_engine_id() -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.write_u32(std::process::id());
    [&[0x80, 0x00, 0x1f, 0x88, 0x05][..], &h.finish().to_be_bytes()].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        parse_engine_id(s).unwrap()
    }

    /// RFC 3414 A.3.1 and A.3.2.
    #[test]
    fn keys_from_passwords() {
        let engine = hex("000000000000000000000002");
        let ku = password_to_key(Auth::Md5, b"maplesyrup");
        assert_eq!(ku, hex("9faf3283884e92834ebc9847d8edd963"));
        assert_eq!(localize(Auth::Md5, &ku, &engine), hex("526f5eed9fcce26f8964c2930787d82b"));
        let ku = password_to_key(Auth::Sha1, b"maplesyrup");
        assert_eq!(ku, hex("9fb5cc0381497b3793528939ff788d5d79145211"));
        assert_eq!(localize(Auth::Sha1, &ku, &engine), hex("6695febc9288e36282235fc7151f128497b38f3f"));
    }

    #[test]
    fn ciphers_round_trip() {
        let data: Vec<u8> = (0..77u8).collect();
        for privacy in [Privacy::Des, Privacy::Aes128, Privacy::Aes192, Privacy::Aes256, Privacy::Aes256Cisco] {
            let user = User {
                name: "u".into(),
                auth: Auth::Sha1,
                auth_password: "authpass1".into(),
                privacy,
                privacy_password: "privpass1".into(),
            };
            let keys = Master::new(&user).localize(&hex("8000000001020304"));
            assert!(keys.priv_key.len() >= privacy.key_len());
            let (c, p) = encrypt(&keys, 3, 1000, 42, &data).unwrap();
            assert_ne!(c[..77], data[..]);
            let back = decrypt(&keys, 3, 1000, &p, &c).unwrap();
            assert_eq!(back[..77], data[..], "{privacy:?}");
        }
    }

    #[test]
    fn signed_messages_verify() {
        let user = User {
            name: "admin".into(),
            auth: Auth::Sha256,
            auth_password: "authpass1".into(),
            privacy: Privacy::Aes128,
            privacy_password: "privpass1".into(),
        };
        let engine = hex("80001f8805aabbccdd");
        let keys = Master::new(&user).localize(&engine);
        let scoped = tlv(
            0x30,
            &[tlv(0x04, &engine), tlv(0x04, b""), tlv(0xa0, &[int(1), int(0), int(0), tlv(0x30, &[])].concat())]
                .concat(),
        );
        let h = Header { msg_id: 9, reportable: true, engine: &engine, boots: 1, time: 50, user: "admin", salt: 7 };
        let msg = encode(&h, &keys, &scoped).unwrap();
        let inc = parse(&msg).unwrap();
        assert_eq!((inc.msg_id, inc.flags, inc.user.as_str()), (9, 7, "admin"));
        inc.verify(&keys).unwrap();
        assert_eq!(inc.scoped(&keys).unwrap(), scoped);
        // One changed byte breaks the signature.
        let mut bad = msg.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(parse(&bad).unwrap().verify(&keys).is_err());
        let other = Master::new(&User { auth_password: "different1".into(), ..user }).localize(&engine);
        assert!(inc.verify(&other).is_err());
    }
}
