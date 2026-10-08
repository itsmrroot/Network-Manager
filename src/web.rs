//! Checks of web servers and internet resources: TLS certificates, HTTP
//! answers and redirects, and WHOIS (RDAP) for domains, IP addresses and
//! AS numbers.

use std::io::Write;
use std::net::{IpAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme};
use serde::Serialize;

// ---------------------------------------------------------------------------
// TLS

#[derive(Debug, Clone, Serialize)]
pub struct Certificate {
    pub subject: String,
    pub issuer: String,
    /// DNS names and addresses the certificate is valid for.
    pub names: Vec<String>,
    pub not_before: i64,
    pub not_after: i64,
    pub serial: String,
    pub signature: String,
    pub key: String,
}

impl Certificate {
    pub fn days_left(&self) -> i64 {
        (self.not_after - now()) / 86400
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TlsReport {
    pub host: String,
    pub address: String,
    pub version: String,
    pub cipher: String,
    /// The server's certificate first, then the chain it sent.
    pub chain: Vec<Certificate>,
    /// `None` when trusted; otherwise why not.
    pub problem: Option<String>,
    pub millis: u128,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `YYYY-MM-DD` of a Unix time (UTC).
pub fn date(ts: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let z = ts.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

/// Accepts any certificate, so that bad ones can be looked at too; trust
/// is checked separately.
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Why a certificate is not trusted, for people.
fn explain(e: rustls::Error) -> String {
    use rustls::CertificateError as C;
    match e {
        rustls::Error::InvalidCertificate(c) => match c {
            C::Expired | C::ExpiredContext { .. } => "The certificate has expired.".into(),
            C::NotValidYet | C::NotValidYetContext { .. } => {
                "The certificate is not valid yet (check the clock).".into()
            }
            C::NotValidForName | C::NotValidForNameContext { .. } => {
                "The certificate is not valid for this name.".into()
            }
            C::UnknownIssuer => {
                "Issued by an authority that is not trusted (self-signed, private CA or missing intermediate).".into()
            }
            C::Revoked => "The certificate was revoked.".into(),
            C::BadSignature => "The certificate's signature is wrong.".into(),
            other => format!("The certificate is not valid ({other:?})."),
        },
        e => e.to_string(),
    }
}

fn describe(der: &[u8]) -> Result<Certificate> {
    use x509_parser::prelude::*;
    let (_, c) = parse_x509_certificate(der).map_err(|e| anyhow::anyhow!("unreadable certificate: {e}"))?;
    let mut names = Vec::new();
    if let Ok(Some(san)) = c.subject_alternative_name() {
        for n in &san.value.general_names {
            match n {
                GeneralName::DNSName(s) => names.push(s.to_string()),
                GeneralName::IPAddress(b) if b.len() == 4 => {
                    names.push(IpAddr::from(<[u8; 4]>::try_from(*b)?).to_string())
                }
                GeneralName::IPAddress(b) if b.len() == 16 => {
                    names.push(IpAddr::from(<[u8; 16]>::try_from(*b)?).to_string())
                }
                _ => {}
            }
        }
    }
    let oid = c.signature_algorithm.algorithm.to_id_string();
    let signature = match oid.as_str() {
        "1.2.840.113549.1.1.11" => "SHA-256 with RSA",
        "1.2.840.113549.1.1.12" => "SHA-384 with RSA",
        "1.2.840.113549.1.1.13" => "SHA-512 with RSA",
        "1.2.840.113549.1.1.5" => "SHA-1 with RSA (weak)",
        "1.2.840.10045.4.3.2" => "ECDSA with SHA-256",
        "1.2.840.10045.4.3.3" => "ECDSA with SHA-384",
        "1.3.101.112" => "Ed25519",
        _ => oid.as_str(),
    }
    .to_string();
    let key = match c.public_key().parsed() {
        Ok(x509_parser::public_key::PublicKey::RSA(k)) => format!("RSA {} bits", k.key_size()),
        Ok(x509_parser::public_key::PublicKey::EC(k)) => format!("EC {} bits", k.key_size()),
        Ok(_) => "Other".into(),
        Err(_) => "Unknown".into(),
    };
    Ok(Certificate {
        subject: c.subject().to_string(),
        issuer: c.issuer().to_string(),
        names,
        not_before: c.validity().not_before.timestamp(),
        not_after: c.validity().not_after.timestamp(),
        serial: c.raw_serial_as_string(),
        signature,
        key,
    })
}

/// Connects to `host` (with an optional `:port`, 443 by default) and reads
/// its certificates and TLS settings.
pub fn tls(target: &str) -> Result<TlsReport> {
    let target = target.trim().trim_start_matches("https://").split('/').next().unwrap_or("").to_string();
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') || h.ends_with(']') => {
            (h.trim_matches(['[', ']']).to_string(), p.parse().unwrap_or(443))
        }
        _ => (target.trim_matches(['[', ']']).to_string(), 443),
    };
    anyhow::ensure!(!host.is_empty(), "enter a name, e.g. example.com");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider.clone())))
        .with_no_client_auth();
    let name = ServerName::try_from(host.clone()).context("not a valid name")?;
    let addr = crate::tools::resolve(&host, port)?;
    let started = Instant::now();
    let mut tcp =
        TcpStream::connect_timeout(&addr, Duration::from_secs(6)).with_context(|| format!("{addr} does not answer"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(6)))?;
    let mut conn = ClientConnection::new(Arc::new(config), name.clone())?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp).context("the TLS handshake failed")?;
    }
    let millis = started.elapsed().as_millis();
    let _ = conn.writer().flush();
    let certs: Vec<CertificateDer<'static>> =
        conn.peer_certificates().unwrap_or_default().iter().map(|c| c.clone().into_owned()).collect();
    if certs.is_empty() {
        bail!("the server sent no certificate");
    }
    let roots: RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider).build()?;
    let problem = verifier.verify_server_cert(&certs[0], &certs[1..], &name, &[], UnixTime::now()).err().map(explain);
    let chain = certs.iter().filter_map(|c| describe(c).ok()).collect();
    Ok(TlsReport {
        host,
        address: addr.to_string(),
        version: conn.protocol_version().map(|v| format!("{v:?}").replace("TLSv1_", "TLS 1.")).unwrap_or_default(),
        cipher: conn.negotiated_cipher_suite().map(|s| format!("{:?}", s.suite())).unwrap_or_default(),
        chain,
        problem,
        millis,
    })
}

// ---------------------------------------------------------------------------
// HTTP

#[derive(Debug, Clone, Serialize)]
pub struct HttpHop {
    pub url: String,
    pub status: u16,
    pub millis: u128,
    /// Headers worth seeing (server, location, security headers, …).
    pub headers: Vec<(String, String)>,
}

const SHOWN_HEADERS: &[&str] = &[
    "server",
    "location",
    "content-type",
    "content-length",
    "strict-transport-security",
    "content-security-policy",
    "x-frame-options",
    "x-content-type-options",
    "referrer-policy",
    "cache-control",
    "age",
    "via",
    "x-cache",
    "cf-ray",
    "alt-svc",
];

/// Requests `url` and follows its redirects (at most 10), one hop at a time.
pub fn http(url: &str) -> Result<Vec<HttpHop>> {
    let mut url = url.trim().to_string();
    anyhow::ensure!(!url.is_empty(), "enter an address, e.g. example.com");
    if !url.contains("://") {
        url = format!("https://{url}");
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(12)))
        .user_agent(format!("netmgr/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut hops = Vec::new();
    for _ in 0..10 {
        let started = Instant::now();
        let r = agent.get(&url).call().with_context(|| format!("{url} could not be reached"))?;
        let millis = started.elapsed().as_millis();
        let status = r.status().as_u16();
        let headers: Vec<(String, String)> = SHOWN_HEADERS
            .iter()
            .filter_map(|h| r.headers().get(*h).map(|v| (h.to_string(), v.to_str().unwrap_or("").to_string())))
            .collect();
        let location = r.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        hops.push(HttpHop { url: url.clone(), status, millis, headers });
        match (status, location) {
            (300..=399, Some(loc)) => url = join_url(&url, &loc),
            _ => break,
        }
    }
    Ok(hops)
}

/// A redirect target, made absolute.
pub fn join_url(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let host = rest.split('/').next().unwrap_or("");
    if let Some(l) = location.strip_prefix("//") {
        format!("{scheme}://{l}")
    } else if location.starts_with('/') {
        format!("{scheme}://{host}{location}")
    } else {
        let dir = base.rsplit_once('/').map(|(d, _)| d).filter(|d| d.len() > scheme.len() + 3).unwrap_or(base);
        format!("{dir}/{location}")
    }
}

// ---------------------------------------------------------------------------
// WHOIS (RDAP)

#[derive(Debug, Clone, Default, Serialize)]
pub struct Whois {
    /// "Domain", "IP network" or "Autonomous system".
    pub kind: String,
    pub name: Option<String>,
    pub handle: Option<String>,
    pub registrar: Option<String>,
    pub organisation: Option<String>,
    pub country: Option<String>,
    /// Address range or prefixes of an IP network.
    pub range: Option<String>,
    pub status: Vec<String>,
    pub nameservers: Vec<String>,
    pub registered: Option<String>,
    pub changed: Option<String>,
    pub expires: Option<String>,
    pub abuse: Option<String>,
}

/// Looks up a domain, an IP address or an AS number ("AS13335") in the
/// registries' RDAP service.
pub fn whois(query: &str) -> Result<Whois> {
    let q = query.trim().trim_end_matches('.');
    anyhow::ensure!(!q.is_empty(), "enter a domain, an IP address or an AS number");
    let (path, kind) = if let Ok(ip) = q.parse::<IpAddr>() {
        (format!("ip/{ip}"), "IP network")
    } else if let Some(n) = q.to_uppercase().strip_prefix("AS").and_then(|n| n.parse::<u32>().ok()) {
        (format!("autnum/{n}"), "Autonomous system")
    } else {
        (format!("domain/{}", q.to_lowercase()), "Domain")
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut r = agent
        .get(format!("https://rdap.org/{path}"))
        .header("Accept", "application/rdap+json, application/json")
        .call()
        .context("the registry could not be reached")?;
    match r.status().as_u16() {
        200 => {}
        404 => bail!("{q} is not registered, or its registry has no RDAP service"),
        s => bail!("the registry answered {s}"),
    }
    let v: serde_json::Value = serde_json::from_str(&r.body_mut().read_to_string()?).context("unexpected answer")?;
    Ok(parse_rdap(&v, kind))
}

pub fn parse_rdap(v: &serde_json::Value, kind: &str) -> Whois {
    let s = |k: &str| v[k].as_str().map(str::to_string).filter(|s| !s.is_empty());
    let mut w = Whois {
        kind: kind.to_string(),
        name: s("ldhName").map(|n| n.to_lowercase()).or_else(|| s("name")),
        handle: s("handle"),
        country: s("country"),
        ..Default::default()
    };
    w.status = v["status"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect();
    w.nameservers = v["nameservers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["ldhName"].as_str().map(|s| s.to_lowercase()))
        .collect();
    for e in v["events"].as_array().into_iter().flatten() {
        let when = e["eventDate"].as_str().map(|d| d.chars().take(10).collect::<String>());
        match e["eventAction"].as_str() {
            Some("registration") => w.registered = when,
            Some("last changed") => w.changed = when,
            Some("expiration") => w.expires = when,
            _ => {}
        }
    }
    if let (Some(a), Some(b)) = (s("startAddress"), s("endAddress")) {
        w.range = Some(format!("{a} – {b}"));
    }
    let cidrs: Vec<String> = v["cidr0_cidrs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let p = c["v4prefix"].as_str().or(c["v6prefix"].as_str())?;
            Some(format!("{p}/{}", c["length"].as_u64()?))
        })
        .collect();
    if !cidrs.is_empty() {
        w.range = Some(cidrs.join(", "));
    }
    // Entities: registrar, registrant organisation, abuse contact.
    fn vcard(e: &serde_json::Value, field: &str) -> Option<String> {
        e["vcardArray"][1].as_array()?.iter().find(|f| f[0] == field).and_then(|f| f[3].as_str()).map(str::to_string)
    }
    fn walk(e: &serde_json::Value, w: &mut Whois) {
        let roles: Vec<&str> = e["roles"].as_array().into_iter().flatten().filter_map(|r| r.as_str()).collect();
        let name = vcard(e, "fn").filter(|n| !n.is_empty());
        if roles.contains(&"registrar") && w.registrar.is_none() {
            w.registrar = name.clone();
        }
        if (roles.contains(&"registrant") || roles.contains(&"administrative")) && w.organisation.is_none() {
            w.organisation = name.clone();
        }
        if roles.contains(&"abuse") && w.abuse.is_none() {
            w.abuse = vcard(e, "email");
        }
        for sub in e["entities"].as_array().into_iter().flatten() {
            walk(sub, w);
        }
    }
    for e in v["entities"].as_array().into_iter().flatten() {
        walk(e, &mut w);
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_791_331_200), "2026-10-07");
        assert_eq!(date(951_782_400), "2000-02-29");
    }

    #[test]
    fn redirects() {
        assert_eq!(join_url("http://a.com/x/y", "https://b.com/"), "https://b.com/");
        assert_eq!(join_url("http://a.com/x/y", "/z"), "http://a.com/z");
        assert_eq!(join_url("http://a.com/x/y", "//c.com/q"), "http://c.com/q");
        assert_eq!(join_url("http://a.com/x/y", "z"), "http://a.com/x/z");
    }

    #[test]
    fn rdap_domain() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"objectClassName":"domain","ldhName":"EXAMPLE.COM","status":["client transfer prohibited"],
                "events":[{"eventAction":"registration","eventDate":"1995-08-14T04:00:00Z"},
                          {"eventAction":"expiration","eventDate":"2027-08-13T04:00:00Z"}],
                "nameservers":[{"ldhName":"A.IANA-SERVERS.NET"}],
                "entities":[{"roles":["registrar"],"vcardArray":["vcard",[["version",{},"text","4.0"],["fn",{},"text","RESERVED-IANA"]]],
                  "entities":[{"roles":["abuse"],"vcardArray":["vcard",[["email",{},"text","abuse@iana.org"]]]}]}]}"#,
        )
        .unwrap();
        let w = parse_rdap(&v, "Domain");
        assert_eq!(w.name.as_deref(), Some("example.com"));
        assert_eq!(w.registrar.as_deref(), Some("RESERVED-IANA"));
        assert_eq!(w.expires.as_deref(), Some("2027-08-13"));
        assert_eq!(w.nameservers, vec!["a.iana-servers.net"]);
        assert_eq!(w.abuse.as_deref(), Some("abuse@iana.org"));
    }
}
