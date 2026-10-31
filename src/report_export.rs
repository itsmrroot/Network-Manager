//! The network report (Markdown, see [`crate::report`]) as a web page and as a
//! PDF document, to hand to someone who does not read Markdown.

/// Escapes the characters that mean something in HTML.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `**bold**` and `` `code` `` in text that is already escaped.
fn inline(s: &str) -> String {
    let mut out = String::new();
    let mut bold = false;
    let mut code = false;
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("**") {
            out.push_str(if bold { "</strong>" } else { "<strong>" });
            bold = !bold;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('`') {
            out.push_str(if code { "</code>" } else { "<code>" });
            code = !code;
            rest = r;
        } else {
            let mut chars = rest.chars();
            if let Some(c) = chars.next() {
                out.push(c);
            }
            rest = chars.as_str();
        }
    }
    if bold {
        out.push_str("</strong>");
    }
    if code {
        out.push_str("</code>");
    }
    out
}

fn cells(line: &str) -> Vec<&str> {
    line.trim().trim_matches('|').split('|').map(str::trim).collect()
}

const STYLE: &str = "body{font:15px/1.5 -apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#1b2430;max-width:820px;margin:32px auto;padding:0 20px}\
h1{font-size:26px;margin:0 0 16px}h2{font-size:19px;margin:28px 0 8px;border-bottom:1px solid #d8dee6;padding-bottom:4px}h3{font-size:16px}\
table{border-collapse:collapse;margin:8px 0;width:100%}td,th{border:1px solid #d8dee6;padding:5px 9px;text-align:left;vertical-align:top}th{background:#f1f4f8}\
ul{margin:4px 0 4px 20px;padding:0}li{margin:2px 0}code{background:#f1f4f8;padding:1px 4px;border-radius:3px;font-size:13px}\
footer{margin-top:32px;color:#6b7785;font-size:12px}@media print{body{margin:0}}";

/// The report as one self-contained web page (no scripts, no outside files).
pub fn to_html(markdown: &str) -> String {
    let mut body = String::new();
    let mut in_table = false;
    let mut header_done = false;
    let mut list_depth = 0usize;
    let close_list = |body: &mut String, depth: &mut usize, to: usize| {
        while *depth > to {
            body.push_str("</ul>");
            *depth -= 1;
        }
    };
    for line in markdown.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with('|') {
            close_list(&mut body, &mut list_depth, 0);
            let row = cells(trimmed);
            if row.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':'))) {
                continue;
            }
            if !in_table {
                body.push_str("<table>");
                in_table = true;
                header_done = false;
            }
            // The first row is the header; the report's own tables have an empty one.
            let first = !header_done;
            header_done = true;
            if first && row.iter().all(|c| c.is_empty()) {
                continue;
            }
            let tag = if first { "th" } else { "td" };
            body.push_str("<tr>");
            for c in row {
                body.push_str(&format!("<{tag}>{}</{tag}>", inline(&escape(c))));
            }
            body.push_str("</tr>");
            continue;
        }
        if in_table {
            body.push_str("</table>");
            in_table = false;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        let text = trimmed.trim_start();
        if let Some(item) = text.strip_prefix("- ") {
            let depth = if indent >= 2 { 2 } else { 1 };
            while list_depth < depth {
                body.push_str("<ul>");
                list_depth += 1;
            }
            close_list(&mut body, &mut list_depth, depth);
            body.push_str(&format!("<li>{}</li>", inline(&escape(item))));
            continue;
        }
        close_list(&mut body, &mut list_depth, 0);
        if let Some(h) = text.strip_prefix("### ") {
            body.push_str(&format!("<h3>{}</h3>", inline(&escape(h))));
        } else if let Some(h) = text.strip_prefix("## ") {
            body.push_str(&format!("<h2>{}</h2>", inline(&escape(h))));
        } else if let Some(h) = text.strip_prefix("# ") {
            body.push_str(&format!("<h1>{}</h1>", inline(&escape(h))));
        } else if !text.is_empty() {
            body.push_str(&format!("<p>{}</p>", inline(&escape(text))));
        }
    }
    if in_table {
        body.push_str("</table>");
    }
    close_list(&mut body, &mut list_depth, 0);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Network report</title><style>{STYLE}</style></head><body>{body}<footer>{}</footer></body></html>\n",
        escape(crate::POWERED_BY)
    )
}

/// One character in the PDF's built-in font (Latin-1), or a stand-in.
fn latin1(c: char) -> u8 {
    match c {
        '✅' | '✓' => b'+',
        '❌' | '✗' => b'x',
        '—' | '–' => b'-',
        '•' => b'*',
        '…' => b'.',
        '‘' | '’' => b'\'',
        '“' | '”' => b'"',
        c if (c as u32) < 0x100 && (c as u32 >= 0x20) => c as u32 as u8,
        _ => b'?',
    }
}

/// Pads a buffered table's rows into aligned columns (no `|` left in them) and
/// appends them to `out`, in the plain font. Does nothing for an empty table.
fn flush_table(out: &mut Vec<(bool, String)>, table: &mut Vec<Vec<String>>) {
    let cols = table.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in table.iter() {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    for row in table.drain(..) {
        let mut line = String::new();
        for (i, c) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("   ");
            }
            line.push_str(c);
            if i + 1 < cols {
                line.push_str(&" ".repeat(widths[i].saturating_sub(c.chars().count())));
            }
        }
        out.push((false, line));
    }
}

/// The report's text without the Markdown marks, wrapped to `width` characters,
/// each paired with whether it is a heading (shown in the PDF's bold font).
fn plain_lines(markdown: &str, width: usize) -> Vec<(bool, Vec<u8>)> {
    let mut lines: Vec<(bool, String)> = Vec::new();
    let mut table: Vec<Vec<String>> = Vec::new();
    for raw in markdown.lines() {
        let row = raw.trim_start();
        if row.starts_with('|') {
            let cells = cells(raw);
            if cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':'))) {
                continue;
            }
            if cells.iter().all(|c| c.is_empty()) {
                continue;
            }
            table.push(cells.into_iter().map(str::to_string).collect());
            continue;
        }
        flush_table(&mut lines, &mut table);
        let indent = raw.len() - row.len();
        let heading = row.starts_with('#');
        let body = row.trim_start_matches('#').trim_start();
        let body = if let Some(item) = body.strip_prefix("- ") { format!("- {item}") } else { body.to_string() };
        lines.push((heading, format!("{}{body}", " ".repeat(indent))));
    }
    flush_table(&mut lines, &mut table);

    let mut out = Vec::new();
    for (heading, text) in lines {
        let text = text.replace("**", "").replace('`', "");
        let bytes: Vec<u8> = text.chars().map(latin1).collect();
        if bytes.is_empty() {
            out.push((heading, bytes));
            continue;
        }
        for chunk in bytes.chunks(width) {
            out.push((heading, chunk.to_vec()));
        }
    }
    out
}

/// A PDF with the report as text in a fixed-width font on A4 pages, headings in
/// bold and table columns lined up. (Only the characters of the Western European
/// alphabet are in the font; other letters
/// show as "?". The HTML export has them all.)
pub fn to_pdf(markdown: &str) -> Vec<u8> {
    const PER_PAGE: usize = 62;
    let lines = plain_lines(markdown, 88);
    let pages: Vec<&[(bool, Vec<u8>)]> = if lines.is_empty() { vec![&[]] } else { lines.chunks(PER_PAGE).collect() };
    let mut objects: Vec<Vec<u8>> = Vec::new();
    // 1: catalog, 2: page tree, 3: plain font, 4: bold font (headings), then a
    // content stream and a page for each page.
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 6 + 2 * i)).collect();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).into_bytes());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>".to_vec());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Bold /Encoding /WinAnsiEncoding >>".to_vec());
    // 5 is the content of the first page: the numbering below puts content at 5 + 2i and page at 6 + 2i.
    for (i, page) in pages.iter().enumerate() {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"BT /F1 9 Tf 12 TL 50 792 Td\n");
        let mut bold = false;
        for (heading, line) in *page {
            if *heading != bold {
                bold = *heading;
                stream.extend_from_slice(if bold { b"/F2 9 Tf\n" } else { b"/F1 9 Tf\n" });
            }
            stream.push(b'(');
            for &b in line {
                match b {
                    b'(' | b')' | b'\\' => {
                        stream.push(b'\\');
                        stream.push(b);
                    }
                    0x20..=0x7e => stream.push(b),
                    _ => stream.extend_from_slice(format!("\\{b:03o}").as_bytes()),
                }
            }
            stream.extend_from_slice(b") Tj T*\n");
        }
        stream.extend_from_slice(b"ET\n");
        let mut content = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
        content.extend_from_slice(&stream);
        content.extend_from_slice(b"endstream");
        objects.push(content);
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * i
            )
            .into_bytes(),
        );
    }
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (n, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", n + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
    );
    pdf
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "# Network report\n\n| | |\n|---|---|\n| Created | 2026-10-31 <now> |\n\n## Connection check\n\n- ✅ **Gateway**: reachable\n  - try `ping 1.1.1.1`\n- ❌ **DNS**: slow & odd\n";

    #[test]
    fn the_html_has_headings_tables_and_lists_and_escapes_text() {
        let h = to_html(REPORT);
        assert!(h.contains("<h1>Network report</h1>") && h.contains("<h2>Connection check</h2>"));
        assert!(h.contains("<td>Created</td><td>2026-10-31 &lt;now&gt;</td>"), "{h}");
        assert!(h.contains("<li>✅ <strong>Gateway</strong>: reachable</li>"));
        assert!(h.contains("<code>ping 1.1.1.1</code>") && h.contains("slow &amp; odd"));
        // An empty header row is not drawn, and nothing is left open.
        assert!(!h.contains("<th></th>"));
        assert_eq!(h.matches("<ul>").count(), h.matches("</ul>").count());
        assert_eq!(h.matches("<table>").count(), h.matches("</table>").count());
        assert!(!h.contains("<script"));
    }

    #[test]
    fn markup_in_the_report_cannot_become_html() {
        let h = to_html("# <script>alert(1)</script>\n- <img src=x onerror=alert(1)>");
        assert!(!h.contains("<script>alert") && !h.contains("<img"));
    }

    /// Finds `n 0 obj` at the offset the cross-reference table gives.
    #[test]
    fn the_pdf_is_well_formed() {
        let pdf = to_pdf(REPORT);
        let text = String::from_utf8_lossy(&pdf).to_string();
        assert!(text.starts_with("%PDF-1.4") && text.trim_end().ends_with("%%EOF"));
        let start: usize = text
            .split("startxref\n")
            .nth(1)
            .and_then(|s| s.lines().next())
            .and_then(|s| s.parse().ok())
            .expect("startxref");
        assert!(text[start..].starts_with("xref"));
        let table: Vec<&str> = text[start..].lines().skip(3).take_while(|l| l.ends_with(" n ")).collect();
        assert!(!table.is_empty());
        for (i, entry) in table.iter().enumerate() {
            let at: usize = entry[..10].parse().expect("offset");
            assert!(text[at..].starts_with(&format!("{} 0 obj", i + 1)), "object {}", i + 1);
        }
        assert!(text.contains("(Network report) Tj") && text.contains("Gateway: reachable"));
    }

    #[test]
    fn the_pdf_bolds_headings_and_lines_up_table_columns() {
        let pdf = to_pdf(REPORT);
        let text = String::from_utf8_lossy(&pdf).to_string();
        // A heading switches to the bold font and the next, non-heading line switches back.
        assert!(text.contains("/F2 9 Tf\n(Network report) Tj"), "{text}");
        assert!(text.contains("/F2 9 Tf\n(Connection check) Tj"), "{text}");
        assert!(text.contains("/F1 9 Tf\n("), "switches back to the plain font");
        // The table's one row is padded into a column, not left with its `|` marks.
        assert!(!text.contains("| Created") && !text.contains("|  |"), "{text}");
        assert!(text.contains("(Created") && text.contains("2026-10-31"));
    }

    #[test]
    fn a_long_report_gets_more_pages_and_odd_characters_are_safe() {
        let long: String = (0..200).map(|i| format!("- line {i} (with) a \\ and ü and 漢\n")).collect();
        let pdf = to_pdf(&long);
        let text = String::from_utf8_lossy(&pdf).to_string();
        assert_eq!(text.matches("/Type /Page ").count(), 4);
        assert!(text.contains("/Count 4"));
        assert!(text.contains("\\(with\\)") && text.contains("\\\\") && text.contains("\\374"));
        assert!(!to_pdf("").is_empty());
    }
}
