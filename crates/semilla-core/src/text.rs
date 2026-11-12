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
