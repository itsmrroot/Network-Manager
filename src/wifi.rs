//! Wi-Fi: the network the computer is joined to, the networks it has saved
//! (with their passwords), and the networks around it.

#[cfg(any(windows, target_os = "macos"))]
use anyhow::Context;
use anyhow::Result;
use serde::Serialize;

#[cfg(target_os = "linux")]
use crate::adapters::split_terse;
use crate::cmd;

/// The Wi-Fi network an adapter is joined to.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Connection {
    pub interface: String,
    /// `None` when the system hides it (macOS, see [`reveal_ssid`]).
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    /// Signal quality, 0–100.
    pub signal: Option<u8>,
    /// Signal strength and noise in dBm.
    pub rssi: Option<i32>,
    pub noise: Option<i32>,
    pub channel: Option<u32>,
    /// "2.4 GHz", "5 GHz", "6 GHz".
    pub band: Option<String>,
    pub width: Option<String>,
    pub security: Option<String>,
    /// "802.11ax (Wi-Fi 6)".
    pub standard: Option<String>,
    pub rate_mbps: Option<f64>,
}

impl Connection {
    /// Quality from 0 to 100, from the percentage or the dBm value.
    pub fn quality(&self) -> Option<u8> {
        self.signal.or_else(|| self.rssi.map(dbm_to_percent))
    }
}

/// A network this computer has joined before.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SavedNetwork {
    pub ssid: String,
    pub security: Option<String>,
    /// `None` when it has not been read (macOS asks per network), or when
    /// the network has no password.
    pub password: Option<String>,
    /// Whether [`SavedNetwork::password`] was read (it may still be `None`
    /// for an open network).
    pub password_read: bool,
    pub hidden: bool,
    /// Linux: the NetworkManager connection.
    #[serde(skip)]
    pub id: String,
}

/// A network the adapter can see.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Nearby {
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub signal: Option<u8>,
    pub channel: Option<u32>,
    pub band: Option<String>,
    pub security: Option<String>,
    pub connected: bool,
}

/// dBm to a 0–100 quality: −100 dBm is 0, −50 dBm and better is 100.
pub fn dbm_to_percent(dbm: i32) -> u8 {
    ((dbm + 100) * 2).clamp(0, 100) as u8
}

/// What a quality figure means to people.
pub fn quality_label(q: u8) -> &'static str {
    match q {
        80.. => "Excellent",
        60..=79 => "Good",
        40..=59 => "Fair",
        _ => "Weak",
    }
}

/// The band a channel belongs to.
pub fn band_of(channel: u32, hint: &str) -> &'static str {
    if hint.contains('6') && hint.contains("GHz") && !hint.contains("2.4") && !hint.contains('5') {
        return "6 GHz";
    }
    match channel {
        1..=14 => "2.4 GHz",
        _ if hint.contains("6GHz") || hint.contains("6 GHz") => "6 GHz",
        _ => "5 GHz",
    }
}

/// The text of a Wi-Fi QR code; phones join the network when they scan it.
pub fn qr_text(ssid: &str, security: Option<&str>, password: Option<&str>, hidden: bool) -> String {
    fn esc(s: &str) -> String {
        s.chars().fold(String::new(), |mut out, c| {
            if matches!(c, '\\' | ';' | ',' | ':' | '"') {
                out.push('\\');
            }
            out.push(c);
            out
        })
    }
    let sec = security.unwrap_or_default().to_uppercase();
    let kind = match password {
        None | Some("") => "nopass",
        Some(_) if sec.contains("WEP") => "WEP",
        Some(_) => "WPA",
    };
    let mut s = format!("WIFI:T:{kind};S:{};", esc(ssid));
    if let Some(p) = password.filter(|p| !p.is_empty()) {
        s += &format!("P:{};", esc(p));
    }
    if hidden {
        s += "H:true;";
    }
    s + ";"
}

/// The QR code of `text` as rows of dark (true) and light modules.
pub fn qr_modules(text: &str) -> Result<Vec<Vec<bool>>> {
    let code = qrcode::QrCode::new(text.as_bytes())?;
    let w = code.width();
    let colors = code.to_colors();
    Ok(colors.chunks(w).map(|row| row.iter().map(|c| *c == qrcode::Color::Dark).collect()).collect())
}

/// The connections of every Wi-Fi adapter.
pub fn current() -> Result<Vec<Connection>> {
    imp::current()
}

/// The saved networks. On Windows and Linux the passwords come with them;
/// on macOS each one is read with [`password`] (the system asks first).
pub fn saved() -> Result<Vec<SavedNetwork>> {
    imp::saved()
}

/// The password of one saved network.
pub fn password(network: &SavedNetwork) -> Result<Option<String>> {
    imp::password(network)
}

/// Linux: reads every password with administrator rights, for networks
/// NetworkManager would not show to the user.
pub fn saved_as_admin() -> Result<Vec<SavedNetwork>> {
    imp::saved_as_admin()
}

/// The networks around, strongest first.
pub fn nearby() -> Result<Vec<Nearby>> {
    let mut list = imp::nearby()?;
    list.sort_by_key(|n| std::cmp::Reverse(n.signal.unwrap_or(0)));
    Ok(list)
}

/// macOS hides the name of the current network from apps without location
/// access; `wdutil` shows it to administrators. Returns (SSID, BSSID).
pub fn reveal_ssid() -> Result<(Option<String>, Option<String>)> {
    imp::reveal_ssid()
}

/// `key : value` lines, as netsh and wdutil print them.
#[cfg(any(windows, target_os = "macos"))]
fn key_values(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// "802.11ax" → "802.11ax (Wi-Fi 6)".
pub fn standard_name(phy: &str) -> String {
    let p = phy.trim();
    let generation = if p.contains("be") {
        Some(7)
    } else if p.contains("ax") {
        Some(6)
    } else if p.contains("ac") {
        Some(5)
    } else if p.ends_with('n') || p.contains("/n") {
        Some(4)
    } else {
        None
    };
    match generation {
        Some(g) if !p.contains("Wi-Fi") => format!("{p} (Wi-Fi {g})"),
        _ => p.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Windows: netsh wlan. Field names are translated on non-English Windows, so
// values are also recognised by their shape.

#[cfg(windows)]
mod imp {
    use super::*;

    pub fn parse_interfaces(text: &str) -> Vec<Connection> {
        let mut out = Vec::new();
        let mut cur: Option<Connection> = None;
        for (k, v) in key_values(text) {
            let kl = k.to_lowercase();
            if kl == "name" || kl == "nom" {
                out.extend(cur.take());
                cur = Some(Connection { interface: v, ..Default::default() });
                continue;
            }
            let Some(c) = cur.as_mut() else { continue };
            if k == "SSID" {
                c.ssid = Some(v).filter(|s| !s.is_empty());
            } else if k == "BSSID" || k == "AP BSSID" {
                c.bssid = Some(v.to_uppercase());
            } else if v.ends_with('%') {
                c.signal = v.trim_end_matches('%').trim().parse().ok();
            } else if v.starts_with("802.11") {
                c.standard = Some(standard_name(&v));
            } else if kl.starts_with("channel") || kl.starts_with("kanal") || kl.starts_with("canal") {
                c.channel = v.parse().ok();
            } else if kl.starts_with("band") {
                c.band = Some(v.replace("GHz", " GHz").replace("  ", " "));
            } else if kl.starts_with("authentication") || kl.starts_with("authentifizierung") {
                c.security = Some(v);
            } else if kl.starts_with("receive rate") || kl.starts_with("empfangsrate") {
                c.rate_mbps = v.parse().ok();
            } else if kl.starts_with("rssi") {
                c.rssi = v.parse().ok();
            }
        }
        out.extend(cur);
        out.retain(|c| c.ssid.is_some() || c.bssid.is_some());
        for c in &mut out {
            if c.band.is_none() {
                c.band = c.channel.map(|ch| band_of(ch, "").to_string());
            }
        }
        out
    }

    pub fn current() -> Result<Vec<Connection>> {
        let text = cmd::run("netsh", &["wlan", "show", "interfaces"])?;
        Ok(parse_interfaces(&text))
    }

    /// The text between `<tag>` and `</tag>`.
    fn tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
        let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
        let end = xml[start..].find(&format!("</{tag}>"))? + start;
        Some(&xml[start..end])
    }

    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
    }

    pub fn parse_profile(xml: &str) -> Option<SavedNetwork> {
        let ssid_block = tag(xml, "SSIDConfig").unwrap_or(xml);
        let ssid = tag(ssid_block, "name").or_else(|| tag(xml, "name"))?;
        let password = tag(xml, "keyMaterial").map(unescape);
        Some(SavedNetwork {
            ssid: unescape(ssid),
            security: tag(xml, "authentication").map(|a| match a {
                "open" => "Open".to_string(),
                "WPA2PSK" => "WPA2-Personal".to_string(),
                "WPA3SAE" => "WPA3-Personal".to_string(),
                "WPAPSK" => "WPA-Personal".to_string(),
                "WPA2" => "WPA2-Enterprise".to_string(),
                "WPA3ENT" | "WPA3ENT192" => "WPA3-Enterprise".to_string(),
                other => other.to_string(),
            }),
            password_read: true,
            password,
            hidden: tag(xml, "nonBroadcast") == Some("true"),
            id: String::new(),
        })
    }

    pub fn saved() -> Result<Vec<SavedNetwork>> {
        // The profiles are exported as XML (the same in every language) into
        // a private folder that is removed right after.
        let dir = std::env::temp_dir().join(format!("netmgr-wlan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let folder = format!("folder={}", dir.display());
        let result = cmd::run("netsh", &["wlan", "export", "profile", "key=clear", &folder]).and_then(|_| {
            let mut out = Vec::new();
            for e in std::fs::read_dir(&dir)?.flatten() {
                let bytes = std::fs::read(e.path())?;
                if let Some(n) = parse_profile(&String::from_utf8_lossy(&bytes)) {
                    out.push(n);
                }
            }
            Ok(out)
        });
        let _ = std::fs::remove_dir_all(&dir);
        let mut out = result.context("the saved networks could not be read")?;
        out.sort_by_key(|n| n.ssid.to_lowercase());
        out.dedup_by(|a, b| a.ssid == b.ssid);
        Ok(out)
    }

    pub fn password(n: &SavedNetwork) -> Result<Option<String>> {
        Ok(saved()?.into_iter().find(|s| s.ssid == n.ssid).and_then(|s| s.password))
    }

    pub fn saved_as_admin() -> Result<Vec<SavedNetwork>> {
        saved()
    }

    pub fn parse_networks(text: &str) -> Vec<Nearby> {
        let mut out: Vec<Nearby> = Vec::new();
        let (mut ssid, mut security): (Option<String>, Option<String>) = (None, None);
        for (k, v) in key_values(text) {
            let kl = k.to_lowercase();
            if k.starts_with("SSID") {
                ssid = Some(v).filter(|s| !s.is_empty());
                security = None;
            } else if k.starts_with("BSSID") {
                out.push(Nearby {
                    ssid: ssid.clone(),
                    bssid: Some(v.to_uppercase()),
                    security: security.clone(),
                    ..Default::default()
                });
            } else if kl.starts_with("authentication") || kl.starts_with("authentifizierung") {
                security = Some(v);
            } else if let Some(n) = out.last_mut().filter(|n| n.ssid == ssid) {
                if v.ends_with('%') {
                    n.signal = v.trim_end_matches('%').trim().parse().ok();
                } else if kl.starts_with("channel") || kl.starts_with("kanal") || kl.starts_with("canal") {
                    n.channel = v.parse().ok();
                } else if kl.starts_with("band") {
                    n.band = Some(v.replace("GHz", " GHz").replace("  ", " "));
                }
            }
        }
        for n in &mut out {
            if n.band.is_none() {
                n.band = n.channel.map(|c| band_of(c, "").to_string());
            }
        }
        out
    }

    pub fn nearby() -> Result<Vec<Nearby>> {
        let text = cmd::run("netsh", &["wlan", "show", "networks", "mode=bssid"])?;
        let mut list = parse_networks(&text);
        let joined: Vec<String> = current().unwrap_or_default().into_iter().filter_map(|c| c.bssid).collect();
        for n in &mut list {
            n.connected = n.bssid.as_ref().is_some_and(|b| joined.contains(b));
        }
        Ok(list)
    }

    pub fn reveal_ssid() -> Result<(Option<String>, Option<String>)> {
        let c = current()?.into_iter().next();
        Ok((c.as_ref().and_then(|c| c.ssid.clone()), c.and_then(|c| c.bssid)))
    }
}

// ---------------------------------------------------------------------------
// macOS: system_profiler, networksetup, the keychain and wdutil.

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use serde_json::Value;

    fn airport() -> Result<Vec<Value>> {
        let text = cmd::run("system_profiler", &["SPAirPortDataType", "-json"])?;
        let v: Value = serde_json::from_str(&text).context("unexpected answer from system_profiler")?;
        Ok(v["SPAirPortDataType"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|e| e["spairport_airport_interfaces"].as_array().cloned().unwrap_or_default())
            .collect())
    }

    /// "spairport_security_mode_wpa2_personal" → "WPA2 Personal".
    fn security(v: &Value) -> Option<String> {
        let s = v.as_str()?.strip_prefix("spairport_security_mode_")?;
        Some(
            s.split('_')
                .map(|w| match w {
                    "wpa" | "wpa2" | "wpa3" | "wep" | "owe" => w.to_uppercase(),
                    "none" => "Open".into(),
                    w => {
                        let mut c = w.chars();
                        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    fn name(v: &Value) -> Option<String> {
        v["_name"].as_str().filter(|n| *n != "<redacted>" && !n.is_empty()).map(str::to_string)
    }

    /// "36 (5GHz, 80MHz)" → (36, "5 GHz", "80 MHz").
    pub fn channel(s: &str) -> (Option<u32>, Option<String>, Option<String>) {
        let num = s.split_whitespace().next().and_then(|n| n.parse().ok());
        let inner = s.split_once('(').map(|(_, r)| r.trim_end_matches(')')).unwrap_or("");
        let mut parts = inner.split(',').map(str::trim);
        let band = parts.next().filter(|b| !b.is_empty()).map(|b| b.replace("GHz", " GHz").replace("2 GHz", "2.4 GHz"));
        let width = parts.next().map(|w| w.replace("MHz", " MHz"));
        (num, band, width)
    }

    pub fn current() -> Result<Vec<Connection>> {
        let mut out = Vec::new();
        for i in airport()? {
            let info = &i["spairport_current_network_information"];
            if !info.is_object() || info["spairport_network_channel"].is_null() {
                continue;
            }
            let (channel, band, width) = channel(info["spairport_network_channel"].as_str().unwrap_or(""));
            // "-47 dBm / -94 dBm"
            let sn = info["spairport_signal_noise"].as_str().unwrap_or("");
            let mut nums = sn.split('/').filter_map(|p| p.trim().trim_end_matches("dBm").trim().parse::<i32>().ok());
            let (rssi, noise) = (nums.next(), nums.next());
            out.push(Connection {
                interface: i["_name"].as_str().unwrap_or("en0").to_string(),
                ssid: name(info),
                bssid: None,
                signal: rssi.map(dbm_to_percent),
                rssi,
                noise,
                channel,
                band,
                width,
                security: security(&info["spairport_security_mode"]),
                standard: info["spairport_network_phymode"].as_str().map(standard_name),
                rate_mbps: info["spairport_network_rate"].as_f64(),
            });
        }
        Ok(out)
    }

    fn wifi_device() -> String {
        let text = cmd::run("networksetup", &["-listallhardwareports"]).unwrap_or_default();
        let mut lines = text.lines();
        while let Some(l) = lines.next() {
            if l.trim() == "Hardware Port: Wi-Fi"
                && let Some(d) = lines.next().and_then(|d| d.trim().strip_prefix("Device: "))
            {
                return d.to_string();
            }
        }
        "en0".into()
    }

    pub fn saved() -> Result<Vec<SavedNetwork>> {
        let text = cmd::run("networksetup", &["-listpreferredwirelessnetworks", &wifi_device()])?;
        Ok(text
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|ssid| SavedNetwork { ssid: ssid.to_string(), ..Default::default() })
            .collect())
    }

    pub fn password(n: &SavedNetwork) -> Result<Option<String>> {
        // macOS asks for the administrator's name and password before it
        // hands out a Wi-Fi password.
        let out =
            cmd::output("security", &["find-generic-password", "-D", "AirPort network password", "-a", &n.ssid, "-w"])?;
        if out.success {
            return Ok(Some(out.stdout.trim_end_matches('\n').to_string()));
        }
        let msg = out.message();
        if msg.contains("could not be found") {
            return Ok(None);
        }
        if msg.contains("canceled") || msg.contains("cancelled") || msg.contains("User interaction") {
            return Err(cmd::Cancelled.into());
        }
        anyhow::bail!("{msg}")
    }

    pub fn saved_as_admin() -> Result<Vec<SavedNetwork>> {
        saved()
    }

    pub fn nearby() -> Result<Vec<Nearby>> {
        let mut out = Vec::new();
        for i in airport()? {
            let cur = &i["spairport_current_network_information"];
            if cur.is_object() && !cur["spairport_network_channel"].is_null() {
                out.push(entry(cur, true));
            }
            for n in i["spairport_airport_other_local_wireless_networks"].as_array().into_iter().flatten() {
                out.push(entry(n, false));
            }
        }
        Ok(out)
    }

    fn entry(n: &Value, connected: bool) -> Nearby {
        let (channel, band, _) = channel(n["spairport_network_channel"].as_str().unwrap_or(""));
        let rssi = n["spairport_signal_noise"]
            .as_str()
            .and_then(|s| s.split('/').next())
            .and_then(|s| s.trim().trim_end_matches("dBm").trim().parse::<i32>().ok());
        Nearby {
            ssid: name(n),
            bssid: None,
            signal: rssi.map(dbm_to_percent),
            channel,
            band,
            security: security(&n["spairport_security_mode"]),
            connected,
        }
    }

    pub fn reveal_ssid() -> Result<(Option<String>, Option<String>)> {
        let text = cmd::admin_sh("/usr/bin/wdutil info")?;
        let mut ssid = None;
        let mut bssid = None;
        // The first "SSID" and "BSSID" belong to the Wi-Fi section.
        for (k, v) in key_values(&text) {
            if k == "SSID" && ssid.is_none() && v != "None" {
                ssid = Some(v);
            } else if k == "BSSID" && bssid.is_none() && v != "None" {
                bssid = Some(v.to_uppercase());
            }
        }
        Ok((ssid, bssid))
    }
}

// ---------------------------------------------------------------------------
// Linux: NetworkManager.

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::cmd::sh_quote;

    fn wifi_rows() -> Result<Vec<Vec<String>>> {
        let text = cmd::run(
            "nmcli",
            &["-t", "-f", "IN-USE,SSID,BSSID,CHAN,FREQ,RATE,SIGNAL,SECURITY,DEVICE", "device", "wifi", "list"],
        )?;
        Ok(text.lines().map(split_terse).filter(|f| f.len() >= 9).collect())
    }

    pub fn current() -> Result<Vec<Connection>> {
        Ok(wifi_rows()?
            .into_iter()
            .filter(|f| f[0] == "*")
            .map(|f| {
                let channel = f[3].parse().ok();
                Connection {
                    interface: f[8].clone(),
                    ssid: Some(f[1].clone()).filter(|s| !s.is_empty()),
                    bssid: Some(f[2].to_uppercase()),
                    signal: f[6].parse().ok(),
                    channel,
                    band: Some(freq_band(&f[4], channel)),
                    security: Some(f[7].clone()).filter(|s| !s.is_empty() && s != "--"),
                    rate_mbps: f[5].split_whitespace().next().and_then(|r| r.parse().ok()),
                    ..Default::default()
                }
            })
            .collect())
    }

    /// "5180 MHz" → "5 GHz".
    fn freq_band(freq: &str, channel: Option<u32>) -> String {
        match freq.split_whitespace().next().and_then(|f| f.parse::<u32>().ok()) {
            Some(f) if f >= 5925 => "6 GHz".into(),
            Some(f) if f >= 4900 => "5 GHz".into(),
            Some(_) => "2.4 GHz".into(),
            None => channel.map_or_else(String::new, |c| band_of(c, "").into()),
        }
    }

    pub fn nearby() -> Result<Vec<Nearby>> {
        Ok(wifi_rows()?
            .into_iter()
            .map(|f| {
                let channel = f[3].parse().ok();
                Nearby {
                    ssid: Some(f[1].clone()).filter(|s| !s.is_empty()),
                    bssid: Some(f[2].to_uppercase()),
                    signal: f[6].parse().ok(),
                    channel,
                    band: Some(freq_band(&f[4], channel)),
                    security: Some(f[7].clone()).filter(|s| !s.is_empty() && s != "--"),
                    connected: f[0] == "*",
                }
            })
            .collect())
    }

    /// (name, uuid) of every saved Wi-Fi connection.
    fn connections() -> Result<Vec<(String, String)>> {
        let text = cmd::run("nmcli", &["-t", "-f", "NAME,UUID,TYPE", "connection", "show"])?;
        Ok(text
            .lines()
            .map(split_terse)
            .filter(|f| f.len() >= 3 && f[2] == "802-11-wireless")
            .map(|f| (f[0].clone(), f[1].clone()))
            .collect())
    }

    const FIELDS: &str =
        "802-11-wireless.ssid,802-11-wireless-security.key-mgmt,802-11-wireless.hidden,802-11-wireless-security.psk";

    fn network(uuid: &str, name: &str, text: &str) -> SavedNetwork {
        let lines: Vec<&str> = text.lines().collect();
        let get = |i: usize| lines.get(i).map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
        let key = get(1);
        SavedNetwork {
            ssid: get(0).unwrap_or_else(|| name.to_string()),
            security: key.as_deref().map(|k| match k {
                "wpa-psk" => "WPA2-Personal".to_string(),
                "sae" => "WPA3-Personal".to_string(),
                "wpa-eap" => "Enterprise".to_string(),
                "none" => "WEP".to_string(),
                "owe" => "Enhanced Open".to_string(),
                o => o.to_string(),
            }),
            password_read: get(3).is_some() || key.is_none(),
            password: get(3),
            hidden: get(2).as_deref() == Some("yes"),
            id: uuid.to_string(),
        }
    }

    pub fn saved() -> Result<Vec<SavedNetwork>> {
        let mut out = Vec::new();
        for (name, uuid) in connections()? {
            // -s shows secrets when NetworkManager allows this user to see them.
            let text = cmd::run("nmcli", &["-s", "-g", FIELDS, "connection", "show", &uuid]).unwrap_or_default();
            out.push(network(&uuid, &name, &text));
        }
        out.sort_by_key(|n| n.ssid.to_lowercase());
        Ok(out)
    }

    pub fn saved_as_admin() -> Result<Vec<SavedNetwork>> {
        let conns = connections()?;
        if conns.is_empty() {
            return Ok(Vec::new());
        }
        let script: Vec<String> = conns
            .iter()
            .map(|(_, uuid)| format!("nmcli -s -g {FIELDS} connection show {}; echo '@@netmgr@@'", sh_quote(uuid)))
            .collect();
        let text = cmd::admin_sh(&script.join("; "))?;
        let mut out: Vec<SavedNetwork> = conns
            .iter()
            .zip(text.split("@@netmgr@@\n"))
            .map(|((name, uuid), block)| network(uuid, name, block))
            .collect();
        out.sort_by_key(|n| n.ssid.to_lowercase());
        Ok(out)
    }

    pub fn password(n: &SavedNetwork) -> Result<Option<String>> {
        let text = cmd::run("nmcli", &["-s", "-g", "802-11-wireless-security.psk", "connection", "show", &n.id])?;
        let p = text.trim();
        if !p.is_empty() {
            return Ok(Some(p.to_string()));
        }
        Ok(saved_as_admin()?.into_iter().find(|s| s.id == n.id).and_then(|s| s.password))
    }

    pub fn reveal_ssid() -> Result<(Option<String>, Option<String>)> {
        let c = current()?.into_iter().next();
        Ok((c.as_ref().and_then(|c| c.ssid.clone()), c.and_then(|c| c.bssid)))
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    use super::*;

    pub fn current() -> Result<Vec<Connection>> {
        Ok(Vec::new())
    }
    pub fn saved() -> Result<Vec<SavedNetwork>> {
        Ok(Vec::new())
    }
    pub fn saved_as_admin() -> Result<Vec<SavedNetwork>> {
        Ok(Vec::new())
    }
    pub fn password(_: &SavedNetwork) -> Result<Option<String>> {
        Ok(None)
    }
    pub fn nearby() -> Result<Vec<Nearby>> {
        Ok(Vec::new())
    }
    pub fn reveal_ssid() -> Result<(Option<String>, Option<String>)> {
        Ok((None, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_codes() {
        assert_eq!(qr_text("Home", Some("WPA2-Personal"), Some("secret"), false), "WIFI:T:WPA;S:Home;P:secret;;");
        assert_eq!(qr_text("Café;1", None, None, true), "WIFI:T:nopass;S:Café\\;1;H:true;;");
        assert_eq!(qr_text("a", Some("WEP"), Some(r#"p:"x\"#), false), r#"WIFI:T:WEP;S:a;P:p\:\"x\\;;"#);
        let m = qr_modules("WIFI:T:WPA;S:Home;P:secret;;").unwrap();
        assert!(m.len() >= 21 && m.iter().all(|r| r.len() == m.len()));
    }

    #[test]
    fn signal() {
        assert_eq!(dbm_to_percent(-47), 100);
        assert_eq!(dbm_to_percent(-75), 50);
        assert_eq!(dbm_to_percent(-110), 0);
        assert_eq!(quality_label(85), "Excellent");
        assert_eq!(standard_name("802.11ax"), "802.11ax (Wi-Fi 6)");
        assert_eq!(standard_name("802.11g/n"), "802.11g/n (Wi-Fi 4)");
        assert_eq!(band_of(6, ""), "2.4 GHz");
        assert_eq!(band_of(36, ""), "5 GHz");
    }

    #[cfg(windows)]
    #[test]
    fn windows_parsing() {
        let text = "There is 1 interface on the system:\n\n    Name                   : Wi-Fi\n    Description            : Intel(R) Wi-Fi 6 AX201\n    State                  : connected\n    SSID                   : Home 5G\n    AP BSSID               : aa:bb:cc:dd:ee:ff\n    Band                   : 5 GHz\n    Channel                : 36\n    Radio type             : 802.11ax\n    Authentication         : WPA2-Personal\n    Receive rate (Mbps)    : 866.7\n    Signal                 : 92%\n";
        let c = &imp::parse_interfaces(text)[0];
        assert_eq!(c.ssid.as_deref(), Some("Home 5G"));
        assert_eq!(c.signal, Some(92));
        assert_eq!(c.channel, Some(36));
        let xml = "<WLANProfile><name>Home</name><SSIDConfig><SSID><name>Home &amp; Co</name></SSID><nonBroadcast>true</nonBroadcast></SSIDConfig><MSM><security><authEncryption><authentication>WPA2PSK</authentication></authEncryption><sharedKey><keyMaterial>pa&lt;ss</keyMaterial></sharedKey></security></MSM></WLANProfile>";
        let n = imp::parse_profile(xml).unwrap();
        assert_eq!((n.ssid.as_str(), n.password.as_deref(), n.hidden), ("Home & Co", Some("pa<ss"), true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_channels() {
        assert_eq!(imp::channel("36 (5GHz, 80MHz)"), (Some(36), Some("5 GHz".into()), Some("80 MHz".into())));
        assert_eq!(imp::channel("10 (2GHz, 20MHz)").1.as_deref(), Some("2.4 GHz"));
    }
}
