//! The CLI guide's content: books of commands for switches, kept as plain
//! text in `assets/guide/` and read when first shown.
//!
//! A book is a text file. Blank lines separate blocks, and a block also ends
//! when the kind of line changes (a paragraph after a table, a warning after
//! a command):
//!
//! ```text
//! @title Cisco IOS            (before the first topic: title, intro, sources)
//! @intro One or two sentences.
//! @source Name of a page | https://example.com
//!
//! # Topic
//! ## Section
//! Plain text. `code` is monospace and **bold** is bold.
//!
//! - A bullet (continued on lines that start with two spaces)
//! 1. A numbered step
//! | A table | with a header |
//! |---|---|
//! | cell | cell (write \| for a bar) |
//! > Lines of output or code
//! ! A warning
//! ~ A tip
//! @diagram console-cable | A picture (see guide_art.rs) and its caption
//!
//! [Switch(config)#] interface vlan 10     (a command: prompt and command)
//! + ip address 10.0.0.1 255.255.255.0     (more command lines)
//! What it does, in plain words.
//! > Example output
//! ```

use std::sync::LazyLock;

/// A command to type: where, what, and how it answers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Command {
    /// `Switch(config)#`: the mode the command is typed in.
    pub prompt: String,
    /// The first line is the command, the others follow it.
    pub lines: Vec<String>,
    pub what: String,
    /// An example of the answer.
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    Bullets(Vec<String>),
    /// The number of the first step, and the steps.
    Steps(u32, Vec<String>),
    /// The first row is the header.
    Table(Vec<Vec<String>>),
    Command(Command),
    Code(Vec<String>),
    Warn(String),
    Tip(String),
    /// A picture drawn by `guide_art` and its caption.
    Diagram(String, String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Section {
    /// Empty for the blocks before a topic's first section.
    pub title: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Topic {
    pub title: String,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub intro: String,
    /// Name and address of the pages the book was checked against.
    pub sources: Vec<(String, String)>,
    pub topics: Vec<Topic>,
}

/// Where a search found something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub topic: usize,
    pub section: usize,
    pub block: usize,
}

/// A book from its text. The messages say the file and line.
pub fn parse(id: &str, text: &str) -> Result<Book, String> {
    let mut book = Book { id: id.to_string(), ..Default::default() };
    let mut block: Vec<(usize, &str)> = Vec::new();
    let mut kind: Option<Kind> = None;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim_end();
        if line.is_empty() {
            flush(id, &mut book, &mut block)?;
            kind = None;
        } else if let Some(t) = line.strip_prefix("## ") {
            flush(id, &mut book, &mut block)?;
            kind = None;
            let topic = book.topics.last_mut().ok_or_else(|| format!("{id}:{n}: a section outside a topic"))?;
            topic.sections.push(Section { title: t.trim().to_string(), blocks: Vec::new() });
        } else if let Some(t) = line.strip_prefix("# ") {
            flush(id, &mut book, &mut block)?;
            kind = None;
            book.topics.push(Topic { title: t.trim().to_string(), sections: Vec::new() });
        } else if block.is_empty() && line.starts_with('@') && book.topics.is_empty() {
            meta(id, n, line, &mut book)?;
        } else {
            if !kind.is_some_and(|k| continues(k, line)) {
                flush(id, &mut book, &mut block)?;
                kind = Some(kind_of(line));
            }
            block.push((n, line));
        }
    }
    flush(id, &mut book, &mut block)?;
    if book.title.is_empty() {
        return Err(format!("{id}: no @title"));
    }
    if book.topics.is_empty() {
        return Err(format!("{id}: no topics"));
    }
    Ok(book)
}

fn meta(id: &str, n: usize, line: &str, book: &mut Book) -> Result<(), String> {
    let (key, value) = line.split_once(' ').unwrap_or((line, ""));
    let value = value.trim();
    match key {
        "@title" => book.title = value.to_string(),
        "@intro" => book.intro = value.to_string(),
        "@source" => {
            let (name, url) =
                value.split_once(" | ").ok_or_else(|| format!("{id}:{n}: @source needs `Name | address`"))?;
            book.sources.push((name.trim().to_string(), url.trim().to_string()));
        }
        _ => return Err(format!("{id}:{n}: unknown {key}")),
    }
    Ok(())
}

/// Ends the block being collected and files it in the current section.
fn flush(id: &str, book: &mut Book, block: &mut Vec<(usize, &str)>) -> Result<(), String> {
    if block.is_empty() {
        return Ok(());
    }
    let parsed = classify(id, block)?;
    let n = block[0].0;
    block.clear();
    let topic = book.topics.last_mut().ok_or_else(|| format!("{id}:{n}: text before the first topic"))?;
    if topic.sections.is_empty() {
        topic.sections.push(Section::default());
    }
    if let Some(section) = topic.sections.last_mut() {
        section.blocks.push(parsed);
    }
    Ok(())
}

/// What a block is, from its first line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Bullets,
    Steps,
    Table,
    Code,
    Warn,
    Tip,
    Command,
    Diagram,
}

fn kind_of(line: &str) -> Kind {
    if line.starts_with("@diagram ") {
        Kind::Diagram
    } else if line.starts_with('|') {
        Kind::Table
    } else if line.starts_with("- ") {
        Kind::Bullets
    } else if step(line).is_some() {
        Kind::Steps
    } else if line == ">" || line.starts_with("> ") {
        Kind::Code
    } else if line.starts_with("! ") {
        Kind::Warn
    } else if line.starts_with("~ ") {
        Kind::Tip
    } else if line.starts_with('[') {
        Kind::Command
    } else {
        Kind::Text
    }
}

/// Whether `line` belongs to the block of `kind` that is being collected.
fn continues(kind: Kind, line: &str) -> bool {
    let indented = line.starts_with("  ");
    let own = kind_of(line);
    match kind {
        // After the command: more lines of it, its explanation, an example.
        Kind::Command => own == Kind::Text || own == Kind::Code,
        Kind::Bullets | Kind::Steps | Kind::Warn | Kind::Tip => own == kind || indented,
        Kind::Table | Kind::Code | Kind::Text => own == kind,
        // One line each.
        Kind::Diagram => false,
    }
}

/// The number at the start of "12. text", and the text.
fn step(line: &str) -> Option<(u32, &str)> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let (number, rest) = line.split_at(digits);
    let text = rest.strip_prefix(". ")?;
    Some((number.parse().ok()?, text))
}

fn classify(id: &str, lines: &[(usize, &str)]) -> Result<Block, String> {
    let err = |n: usize, m: &str| format!("{id}:{n}: {m}");
    let (n0, first) = lines[0];
    let joined =
        |skip: &str| lines.iter().map(|(_, l)| l.strip_prefix(skip).unwrap_or(l).trim()).collect::<Vec<_>>().join(" ");
    if let Some(rest) = first.strip_prefix("@diagram ") {
        let (name, caption) = rest.split_once(" | ").ok_or_else(|| err(n0, "@diagram needs `name | caption`"))?;
        if !crate::guide_art::NAMES.contains(&name.trim()) {
            return Err(err(n0, &format!("no picture called {:?}", name.trim())));
        }
        return Ok(Block::Diagram(name.trim().to_string(), caption.trim().to_string()));
    }
    if let Some(rest) = first.strip_prefix('[') {
        let (prompt, command) = rest.split_once("] ").ok_or_else(|| err(n0, "a command needs `[prompt] command`"))?;
        let mut c = Command {
            prompt: prompt.trim().to_string(),
            lines: vec![command.trim().to_string()],
            ..Default::default()
        };
        let mut what: Vec<&str> = Vec::new();
        for &(_, l) in &lines[1..] {
            if let Some(more) = l.strip_prefix("+ ") {
                c.lines.push(more.trim_end().to_string());
            } else if l == ">" || l.starts_with("> ") {
                c.output.push(l.strip_prefix("> ").or_else(|| l.strip_prefix('>')).unwrap_or(l).to_string());
            } else {
                what.push(l.trim());
            }
        }
        c.what = what.join(" ");
        if c.prompt.is_empty() || c.lines.iter().any(|l| l.trim().is_empty()) {
            return Err(err(n0, "an empty prompt or command"));
        }
        return Ok(Block::Command(c));
    }
    if first.starts_with("- ") {
        let mut items: Vec<String> = Vec::new();
        for &(_, l) in lines {
            match (l.strip_prefix("- "), items.last_mut()) {
                (Some(t), _) => items.push(t.trim().to_string()),
                (None, Some(last)) => {
                    last.push(' ');
                    last.push_str(l.trim());
                }
                (None, None) => {}
            }
        }
        return Ok(Block::Bullets(items));
    }
    if let Some((first_number, _)) = step(first) {
        let mut items: Vec<String> = Vec::new();
        for &(_, l) in lines {
            match (step(l), items.last_mut()) {
                (Some((_, t)), _) => items.push(t.trim().to_string()),
                (None, Some(last)) => {
                    last.push(' ');
                    last.push_str(l.trim());
                }
                (None, None) => {}
            }
        }
        return Ok(Block::Steps(first_number, items));
    }
    if first.starts_with('|') {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for &(n, l) in lines {
            let cells = cells(l).ok_or_else(|| err(n, "a table row starts and ends with `|`"))?;
            let separator = cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'));
            if separator {
                continue;
            }
            if let Some(header) = rows.first()
                && header.len() != cells.len()
            {
                return Err(err(
                    n,
                    &format!(
                        "{} cells, the header has {} (write a bar inside a cell as \\|)",
                        cells.len(),
                        header.len()
                    ),
                ));
            }
            rows.push(cells);
        }
        if rows.len() < 2 {
            return Err(err(n0, "a table needs a header and at least one row"));
        }
        return Ok(Block::Table(rows));
    }
    if first == ">" || first.starts_with("> ") {
        let out = lines
            .iter()
            .map(|(_, l)| l.strip_prefix("> ").or_else(|| l.strip_prefix('>')).unwrap_or(l).to_string())
            .collect();
        return Ok(Block::Code(out));
    }
    if first.starts_with("! ") {
        return Ok(Block::Warn(joined("! ")));
    }
    if first.starts_with("~ ") {
        return Ok(Block::Tip(joined("~ ")));
    }
    Ok(Block::Text(joined("")))
}

/// The cells of a table row; `\|` is a bar inside a cell.
fn cells(line: &str) -> Option<Vec<String>> {
    let inner = line.strip_prefix('|')?.strip_suffix('|')?;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cell.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cell.trim().to_string());
            cell.clear();
        } else {
            cell.push(c);
        }
    }
    cells.push(cell.trim().to_string());
    Some(cells)
}

/// The words of a block, for searching.
pub fn block_text(b: &Block) -> String {
    match b {
        Block::Text(t) | Block::Warn(t) | Block::Tip(t) => t.clone(),
        Block::Bullets(items) | Block::Steps(_, items) | Block::Code(items) => items.join(" "),
        Block::Table(rows) => rows.iter().map(|r| r.join(" ")).collect::<Vec<_>>().join(" "),
        Block::Command(c) => format!("{} {} {}", c.lines.join(" "), c.what, c.output.join(" ")),
        Block::Diagram(_, caption) => caption.clone(),
    }
}

impl Book {
    /// The blocks that contain every word (in the block, or in the title of
    /// its topic or section), in book order.
    pub fn search(&self, words: &[String]) -> Vec<Hit> {
        let mut hits = Vec::new();
        for (t, topic) in self.topics.iter().enumerate() {
            for (s, section) in topic.sections.iter().enumerate() {
                for (b, block) in section.blocks.iter().enumerate() {
                    let hay = format!("{} {} {}", topic.title, section.title, block_text(block)).to_lowercase();
                    if words.iter().all(|w| hay.contains(w.as_str())) {
                        hits.push(Hit { topic: t, section: s, block: b });
                    }
                }
            }
        }
        hits
    }
}

/// The search words of what was typed: lower case, split at spaces.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// Every book, in the order of the picker.
pub fn books() -> &'static [Book] {
    static BOOKS: LazyLock<Vec<Book>> = LazyLock::new(|| {
        let sources: [(&str, Vec<&str>); 8] = [
            ("start", vec![include_str!("../assets/guide/start.txt")]),
            (
                "cisco",
                vec![
                    include_str!("../assets/guide/cisco-1.txt"),
                    include_str!("../assets/guide/cisco-2.txt"),
                    include_str!("../assets/guide/cisco-3.txt"),
                ],
            ),
            ("ccna", vec![include_str!("../assets/guide/ccna.txt")]),
            ("juniper", vec![include_str!("../assets/guide/juniper.txt")]),
            ("aruba", vec![include_str!("../assets/guide/aruba.txt")]),
            ("arista", vec![include_str!("../assets/guide/arista.txt")]),
            ("mikrotik", vec![include_str!("../assets/guide/mikrotik.txt")]),
            ("compare", vec![include_str!("../assets/guide/compare.txt")]),
        ];
        sources
            .iter()
            .map(|(id, parts)| {
                parse(id, &parts.join("\n\n")).unwrap_or_else(|e| Book {
                    id: id.to_string(),
                    title: id.to_string(),
                    intro: format!("This book could not be read: {e}"),
                    ..Default::default()
                })
            })
            .collect()
    });
    &BOOKS
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "@title Test\n@intro An intro.\n@source A page | https://example.com/a\n\n# Topic\nText before a section.\n\n## Section\nSome `code` text\ncontinued.\n\n- one\n  more of one\n- two\n\n3. third\n4. fourth\n\n| Name | Meaning |\n|---|---|\n| `a \\| b` | a bar |\n\n> out 1\n>\n> out 2\n\n! careful\n~ tip\n\n[Switch(config)#] interface vlan 10\n+ ip address 10.0.0.1 255.255.255.0\nSets the address.\n> Vlan10 up\n";

    #[test]
    fn reads_every_kind_of_block() {
        let b = parse("test", SAMPLE).unwrap();
        assert_eq!((b.title.as_str(), b.intro.as_str()), ("Test", "An intro."));
        assert_eq!(b.sources, vec![("A page".to_string(), "https://example.com/a".to_string())]);
        let t = &b.topics[0];
        // Text before the first section goes into a section without a title.
        assert_eq!(t.sections.len(), 2);
        assert_eq!(t.sections[0].title, "");
        assert_eq!(t.sections[0].blocks, vec![Block::Text("Text before a section.".into())]);
        let blocks = &t.sections[1].blocks;
        assert_eq!(blocks[0], Block::Text("Some `code` text continued.".into()));
        assert_eq!(blocks[1], Block::Bullets(vec!["one more of one".into(), "two".into()]));
        assert_eq!(blocks[2], Block::Steps(3, vec!["third".into(), "fourth".into()]));
        assert_eq!(
            blocks[3],
            Block::Table(vec![vec!["Name".into(), "Meaning".into()], vec!["`a | b`".into(), "a bar".into()]])
        );
        assert_eq!(blocks[4], Block::Code(vec!["out 1".into(), "".into(), "out 2".into()]));
        assert_eq!(blocks[5], Block::Warn("careful".into()));
        assert_eq!(blocks[6], Block::Tip("tip".into()));
        assert_eq!(
            blocks[7],
            Block::Command(Command {
                prompt: "Switch(config)#".into(),
                lines: vec!["interface vlan 10".into(), "ip address 10.0.0.1 255.255.255.0".into()],
                what: "Sets the address.".into(),
                output: vec!["Vlan10 up".into()],
            })
        );
    }

    /// A paragraph right after a table, and a warning right after a command,
    /// are blocks of their own: no blank line is needed.
    #[test]
    fn blocks_end_when_the_kind_of_line_changes() {
        let b = parse("t", "@title T\n\n# A\n## B\n| a | b |\n|--|--|\n| 1 | 2 |\nA paragraph.\n- one\nmore text\n[SW#] cmd\nExplains.\n! Careful.\n~ A tip.\n[SW#] other\nExplains too.\n").unwrap();
        let blocks = &b.topics[0].sections[0].blocks;
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                Block::Table(_) => "table",
                Block::Text(_) => "text",
                Block::Bullets(_) => "bullets",
                Block::Command(_) => "command",
                Block::Warn(_) => "warn",
                Block::Tip(_) => "tip",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["table", "text", "bullets", "text", "command", "warn", "tip", "command"]);
    }

    #[test]
    fn a_picture_is_a_block_of_its_own() {
        let b = parse("t", "@title T\n\n# A\n## B\nText.\n@diagram stp | A caption\nMore text.\n").unwrap();
        let blocks = &b.topics[0].sections[0].blocks;
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1], Block::Diagram("stp".into(), "A caption".into()));
        let e = parse("t", "@title T\n\n# A\n## B\n@diagram nope | x\n").unwrap_err();
        assert!(e.starts_with("t:5:") && e.contains("nope"), "{e}");
        assert!(parse("t", "@title T\n\n# A\n## B\n@diagram stp\n").is_err());
    }

    #[test]
    fn mistakes_name_the_line() {
        let e = |text: &str| parse("t", &format!("@title T\n\n# A\n## B\n{text}")).unwrap_err();
        assert!(
            e("| a | b |\n|---|---|\n| only one |").starts_with("t:7:"),
            "{}",
            e("| a | b |\n|---|---|\n| only one |")
        );
        assert!(e("[no prompt end").contains("prompt"));
        assert!(e("| a | b\n|---|---|").contains("starts and ends"));
        assert!(parse("t", "@title T\n\n## B\n").unwrap_err().contains("outside a topic"));
        assert!(parse("t", "# A\n").unwrap_err().contains("@title"));
        assert!(parse("t", "@title T\n@bogus x\n# A\n").unwrap_err().contains("unknown"));
    }

    #[test]
    fn searching_uses_titles_and_text() {
        let b = parse("t", SAMPLE).unwrap();
        assert_eq!(b.search(&words("vlan")).len(), 1);
        assert_eq!(b.search(&words("VLAN 10")).len(), 1);
        // The section's title counts for every block in it.
        assert!(b.search(&words("section")).len() >= 7);
        assert!(b.search(&words("nothing like this")).is_empty());
    }

    /// Every book parses, and says what every command does.
    #[test]
    fn the_books_are_well_formed() {
        let all = books();
        assert_eq!(all.len(), 8);
        let mut commands = 0;
        for book in all {
            assert!(!book.intro.starts_with("This book could not be read"), "{}: {}", book.id, book.intro);
            assert!(!book.title.is_empty() && !book.intro.is_empty(), "{}", book.id);
            let mut titles = std::collections::HashSet::new();
            for topic in &book.topics {
                assert!(titles.insert(&topic.title), "{}: topic {:?} twice", book.id, topic.title);
                assert!(!topic.sections.is_empty(), "{}: {} has no section", book.id, topic.title);
                for section in &topic.sections {
                    assert!(!section.blocks.is_empty(), "{}: {} › {} is empty", book.id, topic.title, section.title);
                    for block in &section.blocks {
                        if let Block::Command(c) = block {
                            commands += 1;
                            assert!(!c.what.is_empty(), "{}: `{}` has no explanation", book.id, c.lines[0]);
                        }
                        let text = block_text(block);
                        assert!(!text.contains('\t'), "{}: a tab in {text:?}", book.id);
                        assert!(!text.contains("TODO") && !text.contains("FIXME"), "{}: unfinished text", book.id);
                    }
                }
            }
            for (name, url) in &book.sources {
                assert!(url.starts_with("https://") && !name.is_empty(), "{}: bad source {name} {url}", book.id);
            }
        }
        let blocks: usize = all.iter().flat_map(|b| &b.topics).flat_map(|t| &t.sections).map(|s| s.blocks.len()).sum();
        println!("{} books, {commands} commands, {blocks} blocks", all.len());
        for b in all {
            println!("  {:<28} {:>2} topics, {:>3} sources", b.title, b.topics.len(), b.sources.len());
        }
        assert!(commands >= 350, "only {commands} commands");
    }

    /// A page backs several books, but is listed once in each.
    #[test]
    fn sources_are_not_repeated_in_a_book() {
        for book in books() {
            let mut seen = std::collections::HashSet::new();
            for (_, url) in &book.sources {
                assert!(seen.insert(url), "{}: {url} twice", book.id);
            }
        }
    }

    #[test]
    fn the_usual_commands_are_found() {
        let cisco = books().iter().find(|b| b.id == "cisco").unwrap();
        for q in [
            "trunk allowed vlan",
            "show ip interface brief",
            "port-security violation",
            "rapid-pvst",
            "scrypt",
            "flash_init",
        ] {
            assert!(!cisco.search(&words(q)).is_empty(), "nothing for {q}");
        }
        let any = |q: &str| books().iter().any(|b| !b.search(&words(q)).is_empty());
        assert!(any("commit confirmed") && any("vlan trunk allowed") && any("vlan-filtering") && any("PoE"));
    }
}
