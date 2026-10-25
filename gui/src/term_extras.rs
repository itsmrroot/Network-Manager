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
const GOOD: &[&str] = &["up", "connected", "established", "forwarding"];
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
    // Not the middle of a longer number (1.2.3.4.5, 1.2.3.456) or of a name
    // (1.2.3.4a, 1.2.3.4-5, 1.2.3.4_beta).
    if line.get(i).is_some_and(|c| {
        c.is_ascii_alphanumeric()
            || *c == '_'
            || *c == '-'
            || *c == '.' && line.get(i + 1).is_some_and(|d| d.is_ascii_digit())
    }) {
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
            let goes_on = line.get(end).is_some_and(hex)
                || (matches!(line.get(end), Some(':' | '-')) && line.get(end + 1).is_some_and(hex));
            if !goes_on {
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
        "twe",
        "fo",
        "tunnel",
        "serial",
        "management",
        "bundle-ether",
        "ether",
        "sfp-sfpplus",
        "sfp",
        "bridge",
        "wlan",
        "ens",
        "enp",
        "docker",
        "br",
        "wg",
        "tun",
        "tap",
        "veth",
        "bond",
        "vmbr",
        "virbr",
    ];
    let rest: String = line[at..].iter().take(20).collect::<String>().to_lowercase();
    let prefix = PREFIXES.iter().filter(|p| rest.starts_with(**p)).max_by_key(|p| p.len())?;
    let mut i = at + prefix.chars().count();
    // Linux names the port after the bus: enp0s31f6, ens3.
    if matches!(*prefix, "enp" | "ens") {
        let from = i;
        while i < line.len() && line[i].is_ascii_alphanumeric() {
            i += 1;
        }
        return (i > from && line[from].is_ascii_digit()).then_some(i - at);
    }
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
    // A short name followed by a long number is a part number or a year
    // (po2024), not a port.
    let tail = &line[digits_from..i];
    if !tail.iter().any(|c| matches!(c, '/' | '.')) && tail.len() > 3 && prefix.len() <= 3 {
        return None;
    }
    Some(i - at)
}

/// An IPv6 address, with its length: 2001:db8::1, fe80::1a2b:3c4d/64, ::1.
fn ipv6_at(line: &[char], at: usize) -> Option<usize> {
    let mut i = at;
    while i < line.len() && (line[i].is_ascii_hexdigit() || line[i] == ':') {
        i += 1;
    }
    let text: String = line[at..i].iter().collect();
    // A trailing colon belongs to the sentence ("is fe80::1: down").
    let text = text.trim_end_matches(':');
    let colons = text.matches(':').count();
    let doubles = text.matches("::").count();
    if colons < 2 || doubles > 1 || text.contains(":::") {
        return None;
    }
    let groups: Vec<&str> = text.split(':').filter(|g| !g.is_empty()).collect();
    if groups.iter().any(|g| g.len() > 4) || groups.len() > 8 || (doubles == 0 && groups.len() != 8) {
        return None;
    }
    // Not the middle of a longer word.
    let mut n = text.chars().count();
    if line.get(at + n).is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') {
        return None;
    }
    // /64
    if line.get(at + n) == Some(&'/') {
        let digits = line[at + n + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..=3).contains(&digits) {
            n += 1 + digits;
        }
    }
    Some(n)
}

/// The `n`-th word before `at` (1 is the one just before), in lower case,
/// without the marks around it; empty when there is none.
fn previous_word(line: &[char], at: usize, n: usize) -> String {
    let mut end = at;
    let mut word = String::new();
    for _ in 0..n {
        while end > 0 && line[end - 1].is_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && !line[start - 1].is_whitespace() {
            start -= 1;
        }
        word = line[start..end].iter().collect::<String>().to_lowercase();
        end = start;
    }
    word.trim_matches([',', ';', ':', '(', ')']).to_string()
}

/// What deserves a colour in one line of text, as (first character, length, kind).
pub fn highlights(line: &[char]) -> Vec<(usize, usize, Kind)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        let boundary = i == 0 || !is_word_char(line[i - 1]);
        if c.is_ascii_digit()
            && (i == 0 || !(line[i - 1].is_ascii_alphanumeric() || matches!(line[i - 1], '.' | '_' | '-')))
            && let Some(n) = address_at(line, i)
        {
            out.push((i, n, Kind::Address));
            i += n;
            continue;
        }
        if c.is_ascii_hexdigit()
            && boundary
            && (i == 0 || !matches!(line[i - 1], ':' | '-' | '.'))
            && let Some(n) = mac_at(line, i)
        {
            out.push((i, n, Kind::Mac));
            i += n;
            continue;
        }
        if (c.is_ascii_hexdigit() || c == ':')
            && boundary
            && (i == 0 || line[i - 1] != ':')
            && let Some(n) = ipv6_at(line, i)
        {
            out.push((i, n, Kind::Address));
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
            // A count of zero ("0 CRC errors") is no problem, and "not connected"
            // is not good news.
            let before = previous_word(line, start, 1);
            // "0 CRC errors": the zero can be one word further back.
            let zero = |w: &str| !w.is_empty() && w.chars().all(|c| c == '0');
            let counted_zero = zero(&before) || zero(&previous_word(line, start, 2));
            let kind = match (kind, before.as_str()) {
                (Some(Kind::Bad), w) if w == "no" || w == "not" || counted_zero => None,
                (Some(Kind::Good), "not" | "no" | "never" | "isn't") => None,
                (kind, _) => kind,
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

/// A character that takes two cells on the screen.
fn wide(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1F64F
            | 0x1F900..=0x1F9FF
            | 0x20000..=0x3FFFD
    )
}

/// The text of a session written to a file: no colour codes, one line per
/// line as it looks when the device has finished it (a line that was edited
/// with the arrow keys, or drawn over with a carriage return, is logged as it
/// ended up), each with the time when asked.
pub struct SessionLog {
    out: Box<dyn Write + Send>,
    stamps: bool,
    /// The line, one entry for each cell of the screen; the second cell of a
    /// wide character is a NUL.
    cells: Vec<char>,
    cursor: usize,
    /// What a split character left for the next piece.
    pending: Vec<u8>,
    state: Esc,
    /// The parameters of the sequence being read.
    params: String,
    /// How long the sequence has been.
    seq_len: usize,
    /// Writing failed: the log is of no use any more.
    failed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Esc {
    Text,
    /// After ESC.
    Start,
    /// ESC ( B and the like: one more character belongs to it.
    Skip,
    /// ESC [ … until a letter.
    Csi,
    /// ESC ] (and P, _, ^, X) … until BEL or ESC \.
    Str,
    /// Inside such a string, after an ESC.
    StringEnd,
}

/// The longest line, and sequence, that is kept whole.
const LONGEST_LINE: usize = 64 * 1024;
const LONGEST_SEQUENCE: usize = 4096;

impl SessionLog {
    pub fn new(out: Box<dyn Write + Send>, stamps: bool) -> Self {
        Self {
            out,
            stamps,
            cells: Vec::new(),
            cursor: 0,
            pending: Vec::new(),
            state: Esc::Text,
            params: String::new(),
            seq_len: 0,
            failed: false,
        }
    }

    /// Writing to the file failed: it is no use to go on.
    pub fn failed(&self) -> bool {
        self.failed
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
            Esc::Text => self.text(c),
            Esc::Start => {
                self.state = match c {
                    '[' => {
                        self.params.clear();
                        self.seq_len = 0;
                        Esc::Csi
                    }
                    // Operating system commands and the other strings.
                    ']' | 'P' | '_' | '^' | 'X' => {
                        self.seq_len = 0;
                        Esc::Str
                    }
                    '(' | ')' | '*' | '+' | '#' | '%' => Esc::Skip,
                    '\x1b' => Esc::Start,
                    _ => Esc::Text,
                }
            }
            Esc::Skip => self.state = Esc::Text,
            Esc::Csi => {
                self.seq_len += 1;
                if ('@'..='~').contains(&c) {
                    self.csi(c);
                    self.state = Esc::Text;
                } else if c.is_control() || self.seq_len > LONGEST_SEQUENCE {
                    // Cut short: what follows is text again.
                    self.state = Esc::Text;
                    self.text(c);
                } else {
                    self.params.push(c);
                }
            }
            Esc::Str => {
                self.seq_len += 1;
                match c {
                    '\x07' => self.state = Esc::Text,
                    '\x1b' => self.state = Esc::StringEnd,
                    '\n' => {
                        self.state = Esc::Text;
                        self.text(c);
                    }
                    _ if self.seq_len > LONGEST_SEQUENCE => self.state = Esc::Text,
                    _ => {}
                }
            }
            Esc::StringEnd => {
                self.state = Esc::Text;
                if c == '\x1b' {
                    self.state = Esc::StringEnd;
                } else if c != '\\' {
                    self.text(c);
                }
            }
        }
    }

    /// A character of text, or a control character.
    fn text(&mut self, c: char) {
        match c {
            '\x1b' => self.state = Esc::Start,
            // The one-character form of ESC [.
            '\u{9b}' => {
                self.params.clear();
                self.seq_len = 0;
                self.state = Esc::Csi;
            }
            '\n' => self.end_line(),
            '\r' => self.cursor = 0,
            '\x08' => self.cursor = self.cursor.saturating_sub(1),
            c if c.is_control() && c != '\t' => {}
            c => self.put(c),
        }
    }

    /// A character at the cursor: over what is there.
    fn put(&mut self, c: char) {
        if self.cells.len() >= LONGEST_LINE {
            self.end_line();
        }
        while self.cells.len() < self.cursor {
            self.cells.push(' ');
        }
        // Writing over the second half of a wide character breaks it.
        if self.cells.get(self.cursor) == Some(&'\0') && self.cursor > 0 {
            self.cells[self.cursor - 1] = ' ';
        }
        let width = if wide(c) { 2 } else { 1 };
        for k in 0..width {
            let cell = if k == 0 { c } else { '\0' };
            match self.cells.get_mut(self.cursor + k) {
                Some(slot) => *slot = cell,
                None => self.cells.push(cell),
            }
        }
        // The half that is left of a wide character written over.
        if width == 1 && self.cells.get(self.cursor + 1) == Some(&'\0') {
            self.cells[self.cursor + 1] = ' ';
        }
        self.cursor += width;
    }

    /// A control sequence that moves the cursor or erases.
    fn csi(&mut self, last: char) {
        // Private modes (cursor visible, bracketed paste…) change nothing here.
        if self.params.starts_with(['?', '>', '=', '<']) {
            return;
        }
        let nums: Vec<usize> = self.params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        let n = nums.first().copied().unwrap_or(0).min(1000);
        let count = n.max(1);
        match last {
            'D' => self.cursor = self.cursor.saturating_sub(count),
            'C' => self.cursor = (self.cursor + count).min(self.cells.len().max(self.cursor)),
            'G' => self.cursor = count - 1,
            'H' | 'f' => self.cursor = nums.get(1).copied().unwrap_or(1).clamp(1, 1000) - 1,
            'K' => match n {
                0 => self.cells.truncate(self.cursor),
                1 => {
                    for cell in self.cells.iter_mut().take(self.cursor + 1) {
                        *cell = ' ';
                    }
                }
                _ => self.cells.clear(),
            },
            'P' => {
                let from = self.cursor.min(self.cells.len());
                let to = (from + count).min(self.cells.len());
                self.cells.drain(from..to);
            }
            '@' => {
                while self.cells.len() < self.cursor {
                    self.cells.push(' ');
                }
                for _ in 0..count {
                    self.cells.insert(self.cursor, ' ');
                }
            }
            'X' => {
                for cell in self.cells.iter_mut().skip(self.cursor).take(count) {
                    *cell = ' ';
                }
            }
            _ => {}
        }
    }

    fn end_line(&mut self) {
        let line: String = self.cells.iter().filter(|c| **c != '\0').collect();
        self.cells.clear();
        self.cursor = 0;
        let line = line.trim_end();
        let result = if self.stamps {
            writeln!(self.out, "[{}] {line}", chrono::Local::now().format("%H:%M:%S"))
        } else {
            writeln!(self.out, "{line}")
        };
        if result.and_then(|()| self.out.flush()).is_err() {
            self.failed = true;
        }
    }

    /// The unfinished line (the prompt) is written too.
    pub fn finish(&mut self) {
        if self.cells.iter().any(|c| !c.is_whitespace() && *c != '\0') {
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

    #[test]
    fn a_log_skips_character_set_escapes_and_follows_progress_lines() {
        let sink = Sink::default();
        let mut log = SessionLog::new(Box::new(sink.clone()), false);
        log.write(b"\x1b(Bhello\x1b)0 x\r\n");
        // A line that starts over with a carriage return keeps the last text; CRLF is a normal end.
        log.write(b"Progress 10%\rProgress 20%\rProgress 100%\r\ndone\r\n");
        drop(log);
        assert_eq!(String::from_utf8(sink.0.lock().unwrap().clone()).unwrap(), "hello x\nProgress 100%\ndone\n");
    }

    #[test]
    fn an_edited_line_is_logged_as_it_ended_up() {
        let sink = Sink::default();
        let mut log = SessionLog::new(Box::new(sink.clone()), false);
        // bash after "abc", Left, Left, X: it redraws the end of the line.
        log.write(b"$ abc\x08\x08Xbc\x08\x08\r\n");
        // The same with escape sequences, and a wide character.
        log.write(b"$ abc\x1b[2DXbc\x1b[2D\r\n");
        log.write("\u{4f60}\u{597d}\x08\x08x\r\n".as_bytes());
        // Erase to the end of the line, delete characters.
        log.write(b"abcdef\x1b[3D\x1b[K\r\nabcdef\x1b[4D\x1b[2Pgh\r\n");
        drop(log);
        assert_eq!(
            String::from_utf8(sink.0.lock().unwrap().clone()).unwrap(),
            "$ aXbc\n$ aXbc\n\u{4f60}x\nabc\nabgh\n"
        );
    }

    #[test]
    fn broken_sequences_do_not_swallow_the_log() {
        let sink = Sink::default();
        let mut log = SessionLog::new(Box::new(sink.clone()), false);
        // A title that never ends, a cut-off CSI, DCS and APC strings, ESC ESC [.
        log.write(b"\x1b]0;");
        log.write(b"lost\nvisible2\n");
        log.write(b"\x1b[3\nb\n");
        log.write(b"a\x1bP1$r0m\x1b\\b\x1b_xx\x1b\\c\n");
        log.write(b"\x1b\x1b[31mred\n");
        drop(log);
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(text, "\nvisible2\n\nb\nabc\nred\n");
    }

    #[test]
    fn a_failing_file_is_noticed() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut log = SessionLog::new(Box::new(Broken), false);
        log.write(b"one\n");
        assert!(log.failed());
        // A line without an end never grows without limit.
        let sink = Sink::default();
        let mut long = SessionLog::new(Box::new(sink.clone()), false);
        long.write(&vec![b'a'; 200_000]);
        assert!(sink.0.lock().unwrap().len() >= 64 * 1024, "long lines are written out in pieces");
    }

    #[test]
    fn fewer_false_colours() {
        for plain in
            ["v1.2.3.4", "fw1.2.3.4", "SW-1.2.3.4", "1.2.3.4a", "1.2.3.4-5", "1.2.3.4_beta", "po2024", "10.0.0.1x"]
        {
            assert!(
                kinds(plain).iter().all(|(_, k)| *k != Kind::Address && *k != Kind::Interface),
                "{plain}: {:?}",
                kinds(plain)
            );
        }
        // Longer hex strings are no MAC address.
        assert!(kinds("00:11:22:33:44:55:66:77").iter().all(|(_, k)| *k != Kind::Mac));
        assert!(kinds("fe80::aa:bb:cc:dd:ee:ff").iter().all(|(_, k)| *k != Kind::Mac));
        // What is new.
        let has = |t: &str, s: &str, k: Kind| kinds(t).iter().any(|(x, kk)| x == s && *kk == k);
        assert!(has("addr fe80::1a2b:3c4d/64 up", "fe80::1a2b:3c4d/64", Kind::Address));
        assert!(has("via 2001:db8::1 dev", "2001:db8::1", Kind::Address));
        assert!(has("lo ::1 ", "::1", Kind::Address));
        for name in [
            "Twe1/0/1",
            "Tunnel0",
            "Serial0/0",
            "Management1",
            "ether1",
            "sfp-sfpplus1",
            "bridge1",
            "wlan1",
            "ens3",
            "enp0s31f6",
            "docker0",
            "br0",
        ] {
            assert!(has(&format!("{name} is"), name, Kind::Interface), "{name}: {:?}", kinds(&format!("{name} is")));
        }
        // Times, dates and version numbers stay plain.
        assert!(
            kinds("12:34:56 2026-10-24 15.2(7)E3 aa:bb:cc").iter().all(|(_, k)| *k != Kind::Address && *k != Kind::Mac)
        );
    }

    #[test]
    fn counts_of_zero_and_negations_are_not_news() {
        let none = |t: &str| kinds(t).iter().all(|(w, _)| w != "errors" && w != "CRC" && w != "connected" && w != "ok");
        assert!(none("0 input errors, 0 CRC, 0 frame"));
        assert!(none("Port is not connected"));
        assert!(none("Disk full, Active Directory ok"));
        assert!(kinds("12 CRC errors").iter().any(|(w, k)| w == "errors" && *k == Kind::Bad));
        assert!(kinds("Gi1/0/1 is connected").iter().any(|(w, k)| w == "connected" && *k == Kind::Good));
    }
}
