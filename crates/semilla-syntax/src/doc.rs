//! Doc tests (brief 5h, B1).
//!
//! A doc comment with a code block is a test: `cargo test` compiles and runs it. So a doc change
//! that changes a code block is a change of code, not of text.
//!
//! The doc text of an item is a list of lines. A doc line starts with `///` or `//!`. A doc
//! attribute with a computed text (`#[doc = concat!(…)]`) stays in its token form. A plain
//! comment (`//`, `/* */`) is not doc text.
//!
//! The code part of a doc text is a superset of the code blocks that rustdoc finds, so a
//! difference between this reader and the Markdown reader of rustdoc cannot hide a test:
//!
//! - each line from the first fence line (```` ``` ```` or `~~~`) to the end of the doc text;
//! - each line with an indent of 3 or more columns more than the smallest indent, or with a tab
//!   (an indented code block);
//! - each doc attribute with a computed text.

/// The change of the code part of a doc text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocTestChange {
    /// `added`, `changed`, or `removed`.
    pub what: &'static str,
    /// The attributes of the code blocks of the new text (of the old text for `removed`): the
    /// text after each fence, for example `no_run` or `ignore`.
    pub attributes: Vec<String>,
}

/// The content of a doc line (the text after `///` or `//!`), or `None` for another line.
fn doc_content(line: &str) -> Option<&str> {
    line.strip_prefix("///").or_else(|| line.strip_prefix("//!"))
}

/// True for a doc attribute in its token form with a text that Semilla does not know.
fn is_computed_doc(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.starts_with("#[doc=") || compact.starts_with("#![doc=")
}

/// The text after the fence characters of a fence line, or `None` for another line. The line can
/// be in a list or in a quote, so the markers before the fence do not count.
fn fence(content: &str) -> Option<&str> {
    let text = content.trim_start_matches(|c: char| c.is_whitespace() || c == '>');
    let mark = text.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = text.chars().take_while(|c| *c == mark).count();
    (run >= 3).then(|| text[run..].trim())
}

/// The code part of a doc text: the lines that can be code of a doc test, and the attributes of
/// the code blocks.
pub fn code_part(docs: &str) -> (Vec<String>, Vec<String>) {
    let lines: Vec<&str> = docs.lines().collect();
    let contents: Vec<Option<&str>> = lines.iter().map(|line| doc_content(line)).collect();
    let indent = |content: &str| content.chars().take_while(|c| *c == ' ').count();
    let smallest = contents.iter().flatten().filter(|content| !content.trim().is_empty()).map(|content| indent(content)).min().unwrap_or(0);
    let mut code = Vec::new();
    let mut attributes = Vec::new();
    let mut fenced = false;
    for (line, content) in lines.iter().zip(&contents) {
        match content {
            Some(content) => {
                if let Some(info) = fence(content) {
                    fenced = true;
                    if !info.is_empty() && !attributes.iter().any(|known| known == info) {
                        attributes.push(info.to_string());
                    }
                }
                let space: String = content.chars().take_while(|c| c.is_whitespace()).collect();
                let indented = !content.trim().is_empty() && (space.contains('\t') || indent(content) >= smallest + 3);
                if fenced || indented {
                    code.push(line.to_string());
                }
            }
            None if is_computed_doc(line) => code.push(line.to_string()),
            None => {}
        }
    }
    (code, attributes)
}

/// The doc test change between two doc texts, or `None` if the code part is equal (a change of
/// the text only).
pub fn doc_test_change(before: &str, after: &str) -> Option<DocTestChange> {
    let ((old, old_attributes), (new, attributes)) = (code_part(before), code_part(after));
    if old == new {
        return None;
    }
    Some(match (old.is_empty(), new.is_empty()) {
        (true, _) => DocTestChange { what: "added", attributes },
        (_, true) => DocTestChange { what: "removed", attributes: old_attributes },
        _ => DocTestChange { what: "changed", attributes },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_change_is_not_a_doc_test_change() {
        assert_eq!(doc_test_change("/// The fee rules.", "/// The fee rules of the ledger.\n// a plain comment"), None);
        // The text before the first code block can change.
        let old = "/// The fee.\n///\n/// ```\n/// assert_eq!(fee(1), 2);\n/// ```";
        assert_eq!(doc_test_change(old, &old.replace("The fee.", "The fee of an amount.")), None);
        // A list with a short indent is text.
        assert_eq!(doc_test_change("/// - one\n///   more", "/// - one\n///   more text"), None);
    }

    #[test]
    fn a_code_block_change_is_a_doc_test_change() {
        let text = "/// The fee.";
        let block = "/// The fee.\n///\n/// ```no_run\n/// std::fs::write(\"/tmp/x\", \"x\").unwrap();\n/// ```";
        let added = doc_test_change(text, block).unwrap();
        assert_eq!((added.what, added.attributes.as_slice()), ("added", &["no_run".to_string()][..]));
        assert_eq!(doc_test_change(block, text).unwrap().what, "removed");
        assert_eq!(doc_test_change(block, &block.replace("/tmp/x", "/tmp/y")).unwrap().what, "changed");
        // The attributes of a block decide if the test runs.
        let ignored = doc_test_change(&block.replace("no_run", "ignore"), block).unwrap();
        assert_eq!(ignored.what, "changed");
        for attribute in ["ignore", "compile_fail", "should_panic", "rust,no_run"] {
            let change = doc_test_change(text, &block.replace("no_run", attribute)).unwrap();
            assert_eq!(change.attributes, [attribute]);
        }
    }

    #[test]
    fn each_form_of_a_code_block_counts() {
        let text = "/// The fee.";
        // A tilde fence, a fence in a list, a fence in a quote, and a fence with no end.
        for block in ["/// ~~~\n/// code();\n/// ~~~", "/// - item\n///   ```\n///   code();\n///   ```", "/// > ```\n/// > code();", "/// ```\n/// code();"] {
            assert_eq!(doc_test_change(text, &format!("{text}\n{block}")).unwrap().what, "added", "{block}");
        }
        // An indented code block, with spaces or with a tab.
        assert_eq!(doc_test_change(text, "/// The fee.\n///\n///     code();").unwrap().what, "added");
        assert_eq!(doc_test_change(text, "/// The fee.\n///\n///\tcode();").unwrap().what, "added");
        // An inner doc comment and a doc attribute with a computed text.
        assert_eq!(doc_test_change("//! The crate.", "//! The crate.\n//! ```\n//! code();\n//! ```").unwrap().what, "added");
        assert_eq!(doc_test_change(text, "/// The fee.\n# [doc = concat ! (\"```\\n\" , \"code();\")]").unwrap().what, "added");
        // A change of the text after the first fence is a change of the code part: this reader
        // does not decide where a block ends.
        let old = "/// ```\n/// code();\n/// ```\n/// The end.";
        assert_eq!(doc_test_change(old, &old.replace("The end.", "The last line.")).unwrap().what, "changed");
    }
}
