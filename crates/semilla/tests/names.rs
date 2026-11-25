//! The registry of the name lists (batch 10n, design 65.5).
//!
//! A name list decides a result by a name: a constant `&[&str]` or `[&str; N]`, and a
//! `matches!` with three or more text patterns. The user rule (2026-11-24): no special case for
//! a set of crates, macros, or symbols if a general rule exists. So each name list of
//! `crates/*/src` must have an entry in `docs/name-lists.md` with its reason:
//!
//! - `language`: a fact of the Rust language (keywords, tokens, built-in attributes);
//! - `tool`: a fact of a tool (cargo keys, git, rustc, the own data of Semilla: the kinds of its
//!   syntax nodes, its options, its categories);
//! - `default mode`: an approximation of the default mode, which has no data of the compiler;
//!   the reason names the rule of the compiled mode.
//!
//! The test fails for a list with no entry, for an entry with no list, and for an entry with no
//! reason or with a reason of an unknown kind.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The kinds of a reason.
const KINDS: &[&str] = &["language", "tool", "default mode"];

/// One name list of the source: the file (from the repository root) and its key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct List {
    file: String,
    key: String,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The end of a string literal that starts at `at` (the index after the closing quote).
fn string_end(bytes: &[u8], at: usize) -> usize {
    let mut i = at + 1;
    while i < bytes.len() && bytes[i] != b'"' {
        if bytes[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    i + 1
}

/// The text patterns of a `matches!` pattern that a `|` joins: `"a" | "b" | "c"` gives three.
fn joined_texts(pattern: &str) -> Vec<String> {
    let bytes = pattern.as_bytes();
    let mut texts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // A char literal `'"'` is not a string.
        if bytes[i] == b'\'' && i + 2 < bytes.len() && bytes[i + 2] == b'\'' {
            i += 3;
            continue;
        }
        if bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            let end = string_end(bytes, i);
            texts.push((i, end.min(bytes.len())));
            i = end;
            continue;
        }
        i += 1;
    }
    let joined = |a: usize, b: usize| pattern.get(a..b).is_some_and(|between| between.trim() == "|" || between.trim().trim_matches(['(', ')', '&']).trim() == "|");
    let mut out = Vec::new();
    for (index, (start, end)) in texts.iter().enumerate() {
        let before = index > 0 && joined(texts[index - 1].1, *start);
        let after = index + 1 < texts.len() && joined(*end, texts[index + 1].0);
        if before || after {
            out.push(pattern[*start..*end].to_string());
        }
    }
    out
}

/// The name of the function around a byte position.
fn function_at(text: &str, at: usize) -> String {
    let head = &text[..at];
    let mut found = String::from("?");
    for (index, _) in head.match_indices("fn ") {
        if index > 0 && head.as_bytes()[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let name: String = head[index + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            found = name;
        }
    }
    found
}

/// The name lists of one source text.
fn lists_of(file: &str, text: &str) -> Vec<List> {
    let mut out = Vec::new();
    // A constant or a static with the type `&[&str]` or `[&str; N]`.
    for line in text.lines() {
        let trimmed = line.trim_start();
        let rest = ["pub(crate) const ", "pub const ", "const ", "pub(crate) static ", "pub static ", "static "].iter().find_map(|start| trimmed.strip_prefix(start));
        let Some(rest) = rest else { continue };
        let Some((name, ty)) = rest.split_once(':') else { continue };
        let ty: String = ty.split('=').next().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        let ty = ty.replace("'static", "");
        if ty == "&[&str]" || (ty.starts_with("[&str;") && ty.ends_with(']')) {
            out.push(List { file: file.to_string(), key: format!("const {}", name.trim()) });
        }
    }
    // A `matches!` with three or more text patterns.
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(found) = text[from..].find("matches!(") {
        let start = from + found + "matches!(".len();
        let mut depth = 1;
        let mut i = start;
        let mut comma = None;
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'"' => {
                    i = string_end(bytes, i);
                    continue;
                }
                b'\'' if i + 2 < bytes.len() && bytes[i + 2] == b'\'' => {
                    i += 3;
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 1 && comma.is_none() => comma = Some(i),
                _ => {}
            }
            i += 1;
        }
        let end = i.saturating_sub(1).max(start);
        if let Some(comma) = comma {
            let pattern = &text[comma + 1..end];
            let pattern = pattern.split(" if ").next().unwrap_or(pattern);
            let texts = joined_texts(pattern);
            if texts.len() >= 3 {
                out.push(List { file: file.to_string(), key: format!("fn {} {}", function_at(text, from + found), texts[0]) });
            }
        }
        from = start;
    }
    out
}

/// Each name list of `crates/*/src`.
fn source_lists(root: &Path) -> Vec<List> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        rust_files(&entry.path().join("src"), &mut files);
    }
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let file = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&path).unwrap();
        out.extend(lists_of(&file, &text));
    }
    out
}

/// The entries of the registry: `| file | key | kind | reason |`.
fn registry(text: &str) -> Result<Vec<(List, String, String)>, Vec<String>> {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for line in text.lines() {
        let Some(row) = line.strip_prefix("| `crates/") else { continue };
        let full = format!("`crates/{row}");
        let cells: Vec<String> = full.trim_end().trim_end_matches('|').split(" | ").map(|cell| cell.trim().to_string()).collect();
        if cells.len() != 4 {
            problems.push(format!("a row of the registry does not have 4 cells: {line}"));
            continue;
        }
        let unquote = |cell: &str| cell.trim_matches('`').to_string();
        let (file, key, kind, reason) = (unquote(&cells[0]), unquote(&cells[1]), cells[2].clone(), cells[3].clone());
        if !KINDS.contains(&kind.as_str()) {
            problems.push(format!("{file} {key}: the kind `{kind}` is not one of {KINDS:?}"));
        }
        if reason.len() < 12 {
            problems.push(format!("{file} {key}: the reason is too short"));
        }
        if kind == "default mode" && !reason.contains("compiled") {
            problems.push(format!("{file} {key}: a default-mode list must name the rule of the compiled mode"));
        }
        out.push((List { file, key }, kind, reason));
    }
    if problems.is_empty() { Ok(out) } else { Err(problems) }
}

/// The problems of the registry for the lists of the source.
fn check(lists: &[List], text: &str) -> Vec<String> {
    let entries = match registry(text) {
        Ok(entries) => entries,
        Err(problems) => return problems,
    };
    let known: BTreeSet<&List> = entries.iter().map(|(list, _, _)| list).collect();
    let found: BTreeSet<&List> = lists.iter().collect();
    let mut problems = Vec::new();
    for list in &found {
        if !known.contains(list) {
            problems.push(format!("a name list with no entry in docs/name-lists.md: | `{}` | `{}` | KIND | REASON |", list.file, list.key));
        }
    }
    for list in &known {
        if !found.contains(list) {
            problems.push(format!("an entry of docs/name-lists.md with no list in the source: {} {}", list.file, list.key));
        }
    }
    problems
}

// rule+ R65.5-1
// rule+ R66.10-2
#[test]
fn each_name_list_of_the_source_has_an_entry_with_its_reason() {
    let root = repository();
    let lists = source_lists(&root);
    assert!(lists.len() > 50, "the scan finds the lists: {}", lists.len());
    let text = std::fs::read_to_string(root.join("docs/name-lists.md")).unwrap();
    let problems = check(&lists, &text);
    assert!(problems.is_empty(), "{} problem(s):\n{}", problems.len(), problems.join("\n"));
}

// rule- R65.5-1
// rule- R66.10-2
#[test]
fn a_new_list_with_no_entry_fails_and_an_entry_with_no_list_fails() {
    let text = "| `crates/x/src/a.rs` | `const KNOWN` | tool | the names of the options of a tool |\n";
    let source = "const KNOWN: &[&str] = &[\"a\"];\nconst NEW: [&str; 3] = [\"a\", \"b\", \"c\"];\nfn f(x: &str) -> bool { matches!(x, \"p\" | \"q\" | \"r\") }\nfn g(x: &str) -> bool { matches!(x, \"p\" | \"q\") || x.contains(\"a\") && x == \"b\" }\n";
    let lists = lists_of("crates/x/src/a.rs", source);
    assert_eq!(lists.iter().map(|list| list.key.as_str()).collect::<Vec<_>>(), ["const KNOWN", "const NEW", "fn f \"p\""]);
    let problems = check(&lists, text);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].contains("const NEW") || problems[1].contains("const NEW"));
    // An entry whose list is not in the source, and a default-mode entry with no compiled rule.
    let stale = "| `crates/x/src/a.rs` | `const GONE` | language | the keywords of the language |\n| `crates/x/src/a.rs` | `const KNOWN` | default mode | an approximation |\n";
    let problems = check(&lists_of("crates/x/src/a.rs", "const KNOWN: &[&str] = &[];"), stale);
    assert!(problems.iter().any(|problem| problem.contains("must name the rule of the compiled mode")), "{problems:?}");
    let problems = check(&lists_of("crates/x/src/a.rs", "const KNOWN: &[&str] = &[];"), "| `crates/x/src/a.rs` | `const GONE` | language | the keywords of the language |\n| `crates/x/src/a.rs` | `const KNOWN` | tool | the names of the options of a tool |\n");
    assert!(problems.iter().any(|problem| problem.contains("const GONE")), "{problems:?}");
}
