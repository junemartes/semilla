//! The escape of text that comes from the agent or from the repository (batch 5k, A2).
//!
//! An intent, a group title, a reason, a claim argument, a path, an identifier, the literal
//! text in a fact, and each diff line are data. A terminal, a Markdown viewer, or a browser
//! must not read them as commands or as layout. Each output uses the functions of this module:
//! a hidden character shows as an escape (`\n`, `\r`, `\t`, `\u{1b}`).

/// True for a character that a terminal or a viewer does not show as a mark: the C0 and C1
/// control characters, DEL, the bidirectional controls, the zero-width characters, the line
/// and paragraph separators, and the byte order mark.
pub fn is_hidden(c: char) -> bool {
    matches!(c,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}'
        | '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        | '\u{200b}'..='\u{200d}' | '\u{2060}'
        | '\u{2028}' | '\u{2029}' | '\u{feff}')
}

fn push_escape(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
    }
}

/// Text for one line: each hidden character is an escape, also a line break. The result
/// never starts a new output line.
pub fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_hidden(c) { push_escape(&mut out, c) } else { out.push(c) }
    }
    out
}

/// Adds one line to a text of the terminal. The line can hold text of the agent or of the
/// repository: each hidden character is an escape, so no part of it starts a new line.
pub fn put_line(out: &mut String, line: &str) {
    out.push_str(&inline(line));
    out.push('\n');
}

/// Text for a block (a diff, a source text, the output of a tool): the line breaks stay, and
/// each other hidden character is an escape.
pub fn block(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c != '\n' && is_hidden(c) { push_escape(&mut out, c) } else { out.push(c) }
    }
    out
}

/// A block inside a line of a report: each line after the first one starts with `indent`, so
/// no line of the block is at the start of an output line.
pub fn block_indent(text: &str, indent: &str) -> String {
    block(text).replace('\n', &format!("\n{indent}"))
}

/// The line of each reviewer output that tells what the quote marks are (batch 5k, A3).
pub const AGENT_TEXT_NOTE: &str = "Text in quote marks is the text of the agent. Semilla does not check it.";

/// The mark of a child in the own text of a node (design 52.1, 54.7). It is not a token of Rust
/// and it is not a cut: each edit in the child has its own fact.
pub const CHILD_MARK: &str = "▫";

/// The note for the reader of an output that has the child mark.
pub const CHILD_NOTE: &str = "`▫` is in place of a child of the node (a condition, a block, an argument). It is not a cut: each edit in the child has its own fact.";

/// The line mark of a fact text (design 54.7). A fact text is one line in the record. A text
/// of code that goes in a block (a block text) starts and ends with this mark, and each line
/// end in it is this mark: `⏎if a {⏎    b();⏎}⏎`. The spaces after a mark are the indent of
/// the line. The mark is not a token of Rust.
pub const LINE_MARK: char = '⏎';

/// A full text of code that is longer than this number of characters is a block text (see
/// `semilla_syntax::tree::full`).
pub const BLOCK_MIN: usize = 80;

/// The parts of a fact text: `(false, text)` is text of the fact line, and `(true, text)` is
/// a block text without its quote marks and its two border marks. A block text is between
/// a quote mark with a line mark after it and a line mark with a quote mark after it. A line
/// of Rust code does not start with a quote mark, so a quote mark in the code is not a border.
fn parts(text: &str) -> Vec<(bool, &str)> {
    let (open, close) = (format!("`{LINE_MARK}"), format!("{LINE_MARK}`"));
    let (mut out, mut rest) = (Vec::new(), text);
    while let Some(start) = rest.find(&open) {
        let inner = &rest[start + open.len()..];
        let Some(end) = inner.find(&close) else { break };
        out.push((false, &rest[..start]));
        out.push((true, &inner[..end]));
        rest = &inner[end + close.len()..];
    }
    out.push((false, rest));
    out
}

/// A text with one space in place of each line mark. A mark at the start or at the end of the
/// text gives nothing.
fn one_row(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != LINE_MARK {
            out.push(c);
            continue;
        }
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_some() && !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out
}

/// A fact text with no line mark, for an output that has one line only. Each character of
/// the code stays.
pub fn flat_text(text: &str) -> String {
    parts(text).into_iter().map(|(block, part)| if block { format!("`{}`", one_row(part)) } else { one_row(part) }).collect()
}

/// The layout of a fact text (design 54.7): the fact line and the blocks below it. A block
/// text (a full text that is long or has more than one line) moves to a block, and the fact
/// line keeps the reference `[N]`. Each other text stays in the line. Each character of the
/// text is in the fact line or in a block: the layout hides nothing. Each line of a block is
/// a full line; no line has a cut.
pub fn fact_layout(text: &str) -> (String, Vec<String>) {
    let (mut head, mut blocks, mut number) = (String::new(), Vec::new(), 0);
    for (block, part) in parts(text) {
        if !block {
            head.push_str(&one_row(part));
            continue;
        }
        number += 1;
        head.push_str(&format!("[{number}]"));
        let pad = " ".repeat(format!("[{number}] ").len());
        for (index, line) in part.split(LINE_MARK).enumerate() {
            blocks.push(if index == 0 { format!("[{number}] {line}") } else { format!("{pad}{line}") });
        }
    }
    (head, blocks)
}

/// The words of the agent, in quote marks (batch 5k, A3). A quote mark and a backslash in the
/// text get a backslash, so the text cannot close the quote.
pub fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if is_hidden(c) => push_escape(&mut out, c),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The escape form of a name with characters outside ASCII: `сharge` gives `\u{441}harge`.
/// `None` for a name of ASCII characters only.
pub fn ascii_form(name: &str) -> Option<String> {
    if name.is_ascii() {
        return None;
    }
    Some(name.chars().map(|c| if c.is_ascii() { c.to_string() } else { format!("\\u{{{:x}}}", c as u32) }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Design 54.7: a block text moves to a block. The fact line keeps a reference. Each
    /// character is in the line or in a block. The negative tests: a text with no border
    /// marks stays in the line, and a quote mark in the code is not a border.
    #[test]
    fn the_layout_of_a_fact_text_hides_nothing() {
        let long = format!("let total = {};", vec!["value"; 20].join(" + "));
        let text = format!("changed `let` `⏎{long}⏎` → `let total = 0;` at `m::f/S1`");
        let (line, blocks) = fact_layout(&text);
        assert_eq!(line, "changed `let` [1] → `let total = 0;` at `m::f/S1`");
        assert_eq!(blocks, vec![format!("[1] {long}")]);
        assert_eq!(flat_text(&text), format!("changed `let` `{long}` → `let total = 0;` at `m::f/S1`"));
        let (line, blocks) = fact_layout("added loop `⏎for a in b {⏎    c(a);⏎}⏎` at `m::f/S2`, removed `⏎if x {⏎    y();⏎}⏎`");
        assert_eq!(line, "added loop [1] at `m::f/S2`, removed [2]");
        assert_eq!(blocks, vec!["[1] for a in b {", "        c(a);", "    }", "[2] if x {", "        y();", "    }"]);
        // Negative: a short text and a long text with no border marks (a path) stay in the line.
        assert_eq!(fact_layout("added statement `a();` at `m::f/S1`"), ("added statement `a();` at `m::f/S1`".to_string(), Vec::new()));
        assert_eq!(fact_layout(&format!("removed `{long}`")), (format!("removed `{long}`"), Vec::new()));
        // A quote mark in the code is not a border of a block: a line that ends with a quote
        // mark, and a short text with quote marks before a block.
        let docs = "added docs `⏎/// The value is `true`⏎/// if `a` is `b`.⏎` at `m::f`, comment `// see `x``, removed `⏎a(⏎    b,⏎)⏎`";
        let (line, blocks) = fact_layout(docs);
        assert_eq!(line, "added docs [1] at `m::f`, comment `// see `x``, removed [2]");
        assert_eq!(blocks, vec!["[1] /// The value is `true`", "    /// if `a` is `b`.", "[2] a(", "        b,", "    )"]);
        assert_eq!(flat_text(docs), "added docs `/// The value is `true` /// if `a` is `b`.` at `m::f`, comment `// see `x``, removed `a( b, )`");
        // A text with marks and no quote marks (the text of a gap line).
        assert_eq!(flat_text("⏎for a in b {⏎    c(a);⏎}⏎"), "for a in b { c(a); }");
    }

    #[test]
    fn hidden_characters_show_as_escapes() {
        let text = "a\nresult: accepted\u{1b}[8m\r\t\u{7f}\u{85}\u{202e}\u{200b}\u{2028}\u{2029}\u{feff}\u{61c}\u{2066}é·";
        let line = inline(text);
        assert_eq!(line, "a\\nresult: accepted\\u{1b}[8m\\r\\t\\u{7f}\\u{85}\\u{202e}\\u{200b}\\u{2028}\\u{2029}\\u{feff}\\u{61c}\\u{2066}é·");
        assert!(!line.chars().any(is_hidden));
        // A block keeps its lines.
        assert_eq!(block("a\n\u{1b}b\r\n"), "a\n\\u{1b}b\\r\n");
        assert_eq!(block_indent("error\nresult: accepted", "    "), "error\n    result: accepted");
        // The text cannot close its quote.
        assert_eq!(quoted("a\" · 0 to read\n\\"), "\"a\\\" · 0 to read\\n\\\\\"");
        assert_eq!(ascii_form("сharge").as_deref(), Some("\\u{441}harge"));
        assert_eq!(ascii_form("charge"), None);
    }
}
