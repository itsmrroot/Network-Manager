//! Helpers for the terminals of the Console: colouring of what network
//! engineers look for in output (addresses, up and down, errors), and a log
//! of a session as clean text with the time of each line.

use std::io::Write;

/// What a stretch of text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An IPv4 address (with a mask length or a port).
    Address,
    /// A MAC address, in the forms of Cisco, Windows and Linux.
    Mac,
    /// up, connected, established, ok…
    Good,
    /// down, error, failed, err-disabled…
    Bad,
    /// warning, shutdown, notconnect, blocking…
    Warn,
    /// An interface name: Gi1/0/1, Ethernet1, ge-0/0/0, Vlan10.
    Interface,
}

/// Words that mean good news, bad news and "look at this".
const GOOD: &[&str] = &["up", "connected", "established", "forwarding", "active", "ok", "full", "success", "enabled"];
const BAD: &[&str] = &[
    "down",
    "error",
    "errors",
    "err-disabled",
    "errdisabled",
    "failed",
    "failure",
    "fail",
    "denied",
    "invalid",
    "unreachable",
    "refused",
    "timeout",
    "crc",
    "collisions",
    "flapping",
    "disabled",
    "fatal",
];
const WARN: &[&str] =
    &["warning", "warn", "shutdown", "notconnect", "blocking", "blocked", "administratively", "learning", "inactive"];

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// IPv4: four groups of 1 to 3 digits (each at most 255), optionally followed
/// by `/length` or `:port`. Returns the length in characters.
fn address_at(line: &[char], at: usize) -> Option<usize> {
    let mut i = at;
    for group in 0..4 {
        let start = i;
        while i < line.len() && line[i].is_ascii_digit() && i - start < 3 {
            i += 1;
        }
        if i == start {
            return None;
        }
        let value: u32 = line[start..i].iter().collect::<String>().parse().ok()?;
        if value > 255 {
            return None;
        }
        if group < 3 {
            if line.get(i) != Some(&'.') {
                return None;
            }
            i += 1;
        }
    }
    // Not the middle of a longer number (1.2.3.4.5, 1.2.3.456).
    if line
        .get(i)
        .is_some_and(|c| c.is_ascii_digit() || *c == '.' && line.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
    {
        return None;
    }
    // /24 or :22
    if let Some(&sep) = line.get(i)
        && (sep == '/' || sep == ':')
    {
        let digits = line[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..=5).contains(&digits) {
            i += 1 + digits;
        }
    }
    Some(i - at)
}

/// A MAC address: aa:bb:cc:dd:ee:ff, aa-bb-cc-dd-ee-ff or aabb.ccdd.eeff.
fn mac_at(line: &[char], at: usize) -> Option<usize> {
    let hex = |c: &char| c.is_ascii_hexdigit();
    let take = |from: usize, n: usize| line.get(from..from + n).is_some_and(|s| s.iter().all(hex));
    // Six pairs with : or -
    if take(at, 2) {
        let sep = *line.get(at + 2)?;
        if (sep == ':' || sep == '-')
            && (1..6).all(|k| take(at + k * 3, 2) && (k == 5 || line.get(at + k * 3 + 2) == Some(&sep)))
        {
            let end = at + 17;
            if !line.get(end).is_some_and(hex) {
                return Some(17);
            }
        }
    }
    // Three groups of four with dots
    if take(at, 4)
        && line.get(at + 4) == Some(&'.')
        && take(at + 5, 4)
        && line.get(at + 9) == Some(&'.')
        && take(at + 10, 4)
    {
        let end = at + 14;
        if !line.get(end).is_some_and(hex) {
            return Some(14);
        }
    }
    None
}

/// An interface name: letters, then a digit or a slash form (Gi1/0/1, ge-0/0/0, Eth1/1, Vlan10, ae0).
fn interface_at(line: &[char], at: usize) -> Option<usize> {
    const PREFIXES: &[&str] = &[
        "gigabitethernet",
        "tengigabitethernet",
        "fastethernet",
        "twentyfivegige",
        "hundredgige",
        "ethernet",
        "port-channel",
        "loopback",
        "vlan",
        "gi",
        "te",
        "fa",
        "eth",
        "et",
        "ge-",
        "xe-",
        "et-",
        "ae",
        "po",
        "lo",
        "mgmt",
        "irb.",
        "tw",
        "hu",
    ];
    let rest: String = line[at..].iter().take(20).collect::<String>().to_lowercase();
    let prefix = PREFIXES.iter().filter(|p| rest.starts_with(**p)).max_by_key(|p| p.len())?;
    let mut i = at + prefix.chars().count();
    let digits_from = i;
    while i < line.len()
        && (line[i].is_ascii_digit() || line[i] == '/' || line[i] == '.' || (line[i] == '-' && prefix.ends_with('-')))
    {
        i += 1;
    }
    // Needs at least one digit; a trailing dot or slash is not part of it.
    while i > digits_from && matches!(line[i - 1], '/' | '.' | '-') {
        i -= 1;
    }
    if i == digits_from || !line[digits_from..i].iter().any(|c| c.is_ascii_digit()) {
        return None;
    }
    if line.get(i).is_some_and(|c| is_word_char(*c) && !c.is_ascii_digit()) {
        return None;
    }
    Some(i - at)
}

/// What deserves a colour in one line of text, as (first character, length, kind).
pub fn highlights(line: &[char]) -> Vec<(usize, usize, Kind)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        let boundary = i == 0 || !is_word_char(line[i - 1]);
        if c.is_ascii_digit()
            && (i == 0 || !(line[i - 1].is_ascii_digit() || line[i - 1] == '.'))
            && let Some(n) = address_at(line, i)
        {
            out.push((i, n, Kind::Address));
            i += n;
            continue;
        }
        if c.is_ascii_hexdigit()
            && boundary
            && let Some(n) = mac_at(line, i)
        {
            out.push((i, n, Kind::Mac));
            i += n;
            continue;
        }
        if c.is_ascii_alphabetic() && boundary {
            if let Some(n) = interface_at(line, i) {
                out.push((i, n, Kind::Interface));
                i += n;
                continue;
            }
            let start = i;
            let mut end = i;
            while end < line.len() && is_word_char(line[end]) {
                end += 1;
            }
            let word: String = line[start..end].iter().collect::<String>().to_lowercase();
            let kind = if GOOD.contains(&word.as_str()) {
                Some(Kind::Good)
            } else if BAD.contains(&word.as_str()) {
                Some(Kind::Bad)
            } else if WARN.contains(&word.as_str()) {
                Some(Kind::Warn)
            } else {
                None
            };
            if let Some(kind) = kind {
                out.push((start, end - start, kind));
            }
            i = end.max(i + 1);
            continue;
        }
        i += 1;
    }
    out
}

/// The text of a session written to a file: no colour codes or cursor
/// movements, one line per line, each with the time when asked.
pub struct SessionLog {
    out: Box<dyn Write + Send>,
    stamps: bool,
    /// The line being assembled.
    line: String,
    /// What a split escape sequence or character left for the next piece.
    pending: Vec<u8>,
    state: Esc,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Esc {
    Text,
    /// After ESC.
    Start,
    /// ESC [ … until a letter.
    Csi,
    /// ESC ] … until BEL or ESC \.
    Osc,
    /// Inside an OSC, after an ESC.
    OscEnd,
}

impl SessionLog {
    pub fn new(out: Box<dyn Write + Send>, stamps: bool) -> Self {
        Self { out, stamps, line: String::new(), pending: Vec::new(), state: Esc::Text }
    }

    /// Takes what the device sent.
    pub fn write(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let data = std::mem::take(&mut self.pending);
        let mut start = 0;
        // Whole characters only: a character cut in two waits for its other
        // half, and bytes that are no text are left out.
        loop {
            match std::str::from_utf8(&data[start..]) {
                Ok(text) => {
                    text.chars().for_each(|c| self.feed(c));
                    start = data.len();
                    break;
                }
                Err(e) => {
                    let valid = start + e.valid_up_to();
                    std::str::from_utf8(&data[start..valid]).unwrap_or_default().chars().for_each(|c| self.feed(c));
                    match e.error_len() {
                        Some(skip) => start = valid + skip,
                        None => {
                            start = valid;
                            break;
                        }
                    }
                }
            }
        }
        self.pending = data[start..].to_vec();
    }

    fn feed(&mut self, c: char) {
        match self.state {
            Esc::Text => match c {
                '\x1b' => self.state = Esc::Start,
                '\n' => self.end_line(),
                '\r' => {}
                '\x08' => {
                    self.line.pop();
                }
                c if c.is_control() && c != '\t' => {}
                c => self.line.push(c),
            },
            Esc::Start => {
                self.state = match c {
                    '[' => Esc::Csi,
                    ']' => Esc::Osc,
                    _ => Esc::Text,
                }
            }
            Esc::Csi => {
                // Parameters and intermediates end with a byte from @ to ~.
                if ('@'..='~').contains(&c) {
                    self.state = Esc::Text;
                }
            }
            Esc::Osc => match c {
                '\x07' => self.state = Esc::Text,
                '\x1b' => self.state = Esc::OscEnd,
                _ => {}
            },
            Esc::OscEnd => self.state = Esc::Text,
        }
    }

    fn end_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        let line = line.trim_end();
        let result = if self.stamps {
            writeln!(self.out, "[{}] {line}", chrono::Local::now().format("%H:%M:%S"))
        } else {
            writeln!(self.out, "{line}")
        };
        if result.is_ok() {
            let _ = self.out.flush();
        }
    }

    /// The unfinished line (the prompt) is written too.
    pub fn finish(&mut self) {
        if !self.line.trim().is_empty() {
            self.end_line();
        }
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn kinds(text: &str) -> Vec<(String, Kind)> {
        let chars: Vec<char> = text.chars().collect();
        highlights(&chars).into_iter().map(|(a, n, k)| (chars[a..a + n].iter().collect(), k)).collect()
    }

    #[test]
    fn addresses_macs_and_interfaces_are_found() {
        let got = kinds(
            "Gi1/0/1 is up, line protocol is up (connected) 10.0.0.1/24 via 192.168.1.254:22 aa:bb:cc:dd:ee:ff 0011.2233.4455",
        );
        let has = |s: &str, k: Kind| got.iter().any(|(t, kk)| t == s && *kk == k);
        assert!(has("Gi1/0/1", Kind::Interface), "{got:?}");
        assert!(has("up", Kind::Good));
        assert!(has("connected", Kind::Good));
        assert!(has("10.0.0.1/24", Kind::Address));
        assert!(has("192.168.1.254:22", Kind::Address));
        assert!(has("aa:bb:cc:dd:ee:ff", Kind::Mac));
        assert!(has("0011.2233.4455", Kind::Mac));
    }

    #[test]
    fn problems_stand_out_and_look_alikes_do_not() {
        let got = kinds("Interface Et1 is down (err-disabled), 12 CRC errors; port is administratively down");
        let has = |s: &str, k: Kind| got.iter().any(|(t, kk)| t == s && *kk == k);
        assert!(has("Et1", Kind::Interface));
        assert!(has("down", Kind::Bad));
        assert!(has("err-disabled", Kind::Bad));
        assert!(has("CRC", Kind::Bad));
        assert!(has("errors", Kind::Bad));
        assert!(has("administratively", Kind::Warn));
        // Not addresses: too big a group, version numbers, five groups, and
        // words that only contain a keyword.
        assert!(
            kinds("300.1.1.1 1.2.3 1.2.3.4.5 version 15.2.7").iter().all(|(_, k)| *k != Kind::Address),
            "{:?}",
            kinds("300.1.1.1 1.2.3 1.2.3.4.5")
        );
        assert!(kinds("update upload download gigabit").is_empty());
        // Bare letters are no MAC; a long hex word is none either.
        assert!(kinds("deadbeef cafe").is_empty());
    }

    #[test]
    fn the_scanner_copes_with_odd_input() {
        for text in [
            "",
            "/",
            ":",
            "1.",
            "1.2.3.",
            "aa:bb",
            "Gi",
            "Gi/",
            "Gi1/",
            "ge-",
            "....",
            "\u{4f60}\u{597d} 10.0.0.1",
            "1.1.1.1/",
        ] {
            let chars: Vec<char> = text.chars().collect();
            for (a, n, _) in highlights(&chars) {
                assert!(a + n <= chars.len());
            }
        }
        assert_eq!(kinds("\u{4f60}\u{597d} 10.0.0.1"), [("10.0.0.1".to_string(), Kind::Address)]);
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_log_is_clean_text_with_times() {
        let sink = Sink::default();
        let mut log = SessionLog::new(Box::new(sink.clone()), true);
        log.write(b"\x1b[32mSwitch#\x1b[0m show ver\r\nCisco \x1b]0;title\x07IOS\r\n\x1b[2K\rline2   \r\n");
        // A character split between two pieces, and a line without an end.
        log.write(&"héllo\n".as_bytes()[..2]);
        log.write(&"héllo\n".as_bytes()[2..]);
        log.write(b"Switch#");
        log.finish();
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let strip = |l: &str| l.split_once("] ").map(|(_, r)| r.to_string()).unwrap_or_default();
        assert!(lines.iter().all(|l| l.starts_with('[') && l.as_bytes()[3] == b':'), "{text}");
        assert_eq!(
            lines.iter().map(|l| strip(l)).collect::<Vec<_>>(),
            ["Switch# show ver", "Cisco IOS", "line2", "h\u{e9}llo", "Switch#"]
        );
    }

    #[test]
    fn a_log_without_times_is_plain() {
        let sink = Sink::default();
        let mut log = SessionLog::new(Box::new(sink.clone()), false);
        log.write(b"abc\x08d\r\n");
        drop(log);
        assert_eq!(String::from_utf8(sink.0.lock().unwrap().clone()).unwrap(), "abd\n");
    }
}
