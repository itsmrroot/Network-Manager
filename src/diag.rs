//! Diagnostic information that can be sent along with a problem report: the
//! version, the system, and the end of the problem log, with the addresses
//! of this computer and its network made unreadable first.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Hides what identifies a network or a computer: IPv4 addresses keep their
/// first two numbers (`192.168.x.x`), IPv6 addresses go, MAC addresses keep
/// the maker's part (`F0:18:98:xx:xx:xx`), and the home folder is `~`.
pub fn redact(text: &str, home: Option<&str>) -> String {
    let text = match home.filter(|h| h.len() > 3) {
        Some(h) => text.replace(h, "~"),
        None => text.to_string(),
    };
    let is_part = |c: char| c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '-');
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.is_empty() {
            return;
        }
        // Punctuation around an address belongs to the sentence.
        let core = run.trim_matches(['.', ':', '-']);
        let start = run.find(core).unwrap_or(0);
        out.push_str(&run[..start]);
        out.push_str(&hide(core));
        out.push_str(&run[start + core.len()..]);
        run.clear();
    };
    for c in text.chars() {
        if is_part(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn hide(word: &str) -> String {
    if let Ok(ip) = word.parse::<Ipv4Addr>() {
        let o = ip.octets();
        return format!("{}.{}.x.x", o[0], o[1]);
    }
    if word.contains(':') && word.parse::<Ipv6Addr>().is_ok() {
        return "<ipv6>".into();
    }
    let groups: Vec<&str> = word.split([':', '-']).collect();
    if groups.len() == 6 && groups.iter().all(|g| g.len() == 2 && g.chars().all(|c| c.is_ascii_hexdigit())) {
        return format!("{}:{}:{}:xx:xx:xx", groups[0], groups[1], groups[2]).to_uppercase().replace("XX", "xx");
    }
    word.to_string()
}

/// The last `n` lines of `text`.
pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_hidden() {
        let t = redact(
            "up on 192.168.31.128/24 gw 10.0.0.1, mac f0:18:98:11:22:33 and F0-18-98-11-22-33; v6 2001:db8::10. ok 1.2.3",
            None,
        );
        assert_eq!(
            t,
            "up on 192.168.x.x/24 gw 10.0.x.x, mac F0:18:98:xx:xx:xx and F0:18:98:xx:xx:xx; v6 <ipv6>. ok 1.2.3"
        );
    }

    #[test]
    fn words_and_versions_stay() {
        assert_eq!(
            redact("v0.9.26 netmgr-gui face dead-beef 2026-10-31 20:34:12", None),
            "v0.9.26 netmgr-gui face dead-beef 2026-10-31 20:34:12"
        );
    }

    #[test]
    fn the_home_folder_is_a_tilde() {
        assert_eq!(redact("/Users/mbp/Library/x failed", Some("/Users/mbp")), "~/Library/x failed");
    }

    #[test]
    fn the_end_of_a_log() {
        assert_eq!(tail("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(tail("a", 5), "a");
    }
}
