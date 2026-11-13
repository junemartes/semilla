//! The opaque inputs of a proof in the source text (batch 5l, made exact in batch 5o).
//!
//! A proof of a changed body uses facts from other items: the body of a callee, the target of
//! a name, the layout of a type. The compiler selects such an item by the build
//! configuration if the item is below a `cfg` or a `cfg_attr` attribute. The MIR shows only
//! the selected item. This module reads the syntax tree, which has each alternative. It
//! finds:
//!
//! - the hard names: a body that names one of them gets no proof;
//! - the opaque types: a body that uses one of them gets no proof;
//! - the opaque callees: no normal form looks into them;
//! - the soft names: a new use gets the "build configuration" spotlight.
//!
//! Batch 5o: a hard name has a place and a form. The place is the module or the type where
//! the name can mean the item. The form is the position of the token in a path. The driver
//! gives the resolved items and the types of each body (the entries of `callee`), so the text
//! of an identifier is an input only for the code that the compiler does not show.
//!
//! The default is opaque: an item is free only if no rule here applies to it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use semilla_syntax::{Item, ItemKind, Workspace};

use crate::srcconfig::{compact, Scan, KEYWORDS, MACRO_FREE};
use crate::{BodyComparison, BodyStatus};

/// Where a hard name can mean the item of the build configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    /// In each item.
    Any,
    /// In an item that sees the names of this module.
    Module(String),
    /// In a body that uses a type with this name.
    Type(String),
}

/// The position of a token that can mean the item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// Each position.
    Any,
    /// The first segment of a path, or a segment after a module that gives the name.
    Path,
    /// After `.` or `::`.
    Member,
    /// Before `::`.
    Prefix,
}

/// Why a name is hard.
#[derive(Clone, Debug)]
struct Cause {
    /// The text for the spotlight, without the name.
    text: String,
    /// The item or the module with the attribute. A reader inside it is exempt: the reader
    /// exists only together with the name.
    root: Option<String>,
    place: Place,
    form: Form,
}

/// One identifier of an item text, with its position.
#[derive(Clone, Debug, Default)]
struct Tok {
    word: String,
    /// After `.` or `::`.
    member: bool,
    /// The segment before `::`. Empty for a path that starts with `::`, `>` after generic
    /// arguments.
    qual: Option<String>,
    /// Before `::`.
    prefix: bool,
    /// In the arguments of a macro that is not in the list of the standard library: the
    /// position in the expansion is not known.
    free: bool,
    /// After `fn`: the name of a function in its definition. It is not a use of a name.
    def: bool,
    /// The token is not in the text (batch 5p). It is a word of the definition of a macro of
    /// the workspace that the text calls, or the name of a macro with an expansion that
    /// Semilla cannot read (with `!` at the end of the word).
    via: bool,
}

impl Tok {
    /// True for a macro call with an expansion that Semilla cannot read: each name can be in
    /// the expansion.
    fn wild(&self) -> bool {
        self.via && self.word.ends_with('!')
    }
}

/// The words that are not names of items.
const WORDS: [&str; 44] = [
    "pub", "use", "crate", "self", "super", "in", "as", "fn", "impl", "for", "where", "mut", "const", "static", "type", "struct", "enum", "union", "trait", "mod", "extern", "unsafe", "dyn",
    "ref", "Self", "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "async", "C",
];

/// The numeric types. An integer literal or a float literal with no suffix gets its type from
/// the trait impls that the configuration selects.
const NUMERIC: [&str; 14] = ["u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64"];

/// The types with no drop code and with a value in each configuration.
const PLAIN: [&str; 16] = ["u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "bool", "char"];

/// The content of a `cfg_attr` that cannot change the behavior (batch 5o, A5).
const CFG_ATTR_FREE: [&str; 10] = ["allow", "warn", "deny", "expect", "forbid", "doc", "inline", "cold", "must_use", "deprecated"];

/// The features of the compiler that change only the documentation. Decided by Claude.
const DOC_FEATURES: [&str; 4] = ["doc_cfg", "doc_auto_cfg", "doc_cfg_hide", "doc_notable_trait"];

/// The types of the standard library that an `impl` under `cfg` can name as its self type.
const STD_TYPES: [&str; 22] = [
    "Vec", "String", "Box", "Rc", "Arc", "Option", "Result", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "VecDeque", "BinaryHeap", "Cow", "PathBuf", "OsString", "CString", "Cell", "RefCell",
    "Mutex", "RwLock", "Duration",
];

/// A row of the table of known traits.
pub struct KnownTrait {
    /// The last segment of the path: the name in a `derive` and in an `impl` header.
    pub name: &'static str,
    /// The path of the trait, as the compiler prints it.
    pub path: &'static str,
    /// The names that the trait gives to a type: its methods, and the items of each trait
    /// that a blanket impl gives to a type with the trait (`to_string` from `Display`).
    pub names: &'static [&'static str],
    /// True if a `derive` with this name is in the allow list: a built-in `derive`, or a
    /// `derive` of `serde`. The test `trait_table` compiles each one.
    pub derive: bool,
}

/// The table of known traits: the traits of the standard library and of `serde` that give
/// only methods to a type. A trait with an operator, an associated type, or a `Drop` meaning
/// is not in the table. The test `trait_table` of the crate `semilla` reads each row from
/// the compiler (batch 5p, C2). Decided by Claude.
pub const KNOWN_TRAITS: [KnownTrait; 16] = [
    KnownTrait { name: "Display", path: "std::fmt::Display", names: &["fmt", "to_string"], derive: false },
    KnownTrait { name: "Debug", path: "std::fmt::Debug", names: &["fmt"], derive: true },
    KnownTrait { name: "Clone", path: "std::clone::Clone", names: &["clone", "clone_from", "to_owned", "clone_into", "Owned", "clone_to_uninit"], derive: true },
    KnownTrait { name: "Copy", path: "std::marker::Copy", names: &["cast_from", "cast_from_lossy"], derive: true },
    KnownTrait { name: "Eq", path: "std::cmp::Eq", names: &["assert_fields_are_eq", "assert_receiver_is_total_eq", "equivalent"], derive: true },
    KnownTrait { name: "Send", path: "std::marker::Send", names: &[], derive: false },
    KnownTrait { name: "Sync", path: "std::marker::Sync", names: &[], derive: false },
    KnownTrait { name: "PartialEq", path: "std::cmp::PartialEq", names: &["eq", "ne"], derive: true },
    KnownTrait { name: "PartialOrd", path: "std::cmp::PartialOrd", names: &["partial_cmp", "lt", "le", "gt", "ge", "__chaining_less", "__chaining_greater"], derive: true },
    KnownTrait { name: "Ord", path: "std::cmp::Ord", names: &["cmp", "max", "min", "clamp", "clamp_to"], derive: true },
    KnownTrait { name: "Hash", path: "std::hash::Hash", names: &["hash", "hash_slice"], derive: true },
    KnownTrait { name: "Default", path: "std::default::Default", names: &["default"], derive: true },
    KnownTrait { name: "From", path: "std::convert::From", names: &["from", "into", "try_from", "try_into", "Error"], derive: false },
    KnownTrait { name: "Error", path: "std::error::Error", names: &["source", "description", "cause", "provide", "type_id"], derive: false },
    KnownTrait { name: "Serialize", path: "serde::Serialize", names: &["serialize"], derive: true },
    KnownTrait { name: "Deserialize", path: "serde::Deserialize", names: &["deserialize", "deserialize_in_place"], derive: true },
];

/// The names that a known trait gives to a type. `None`: the trait is not known.
fn trait_names(name: &str) -> Option<&'static [&'static str]> {
    KNOWN_TRAITS.iter().find(|row| row.name == name).map(|row| row.names)
}

/// True for a `derive` of the allow list. Each other `derive` in a `cfg_attr` makes its item
/// an item under `cfg`: a macro that Semilla does not know can make each kind of code.
fn derive_known(name: &str) -> bool {
    KNOWN_TRAITS.iter().any(|row| row.derive && row.name == name)
}

/// The kind of a `cfg` or a `cfg_attr` attribute.
pub(crate) enum Attr {
    /// `cfg`, or a `cfg_attr` with a content that can change the behavior.
    Select,
    /// A `cfg`, or a `cfg_attr` with a content of the allow list or a `derive`, with a
    /// condition that the checked configurations cover (batch 5s, part C). The proof in each
    /// checked configuration is the check of the code that the attribute selects.
    Covered,
    /// A `cfg_attr` with a content of the allow list.
    Free,
    /// A `cfg_attr` with `derive` of these traits and no other content that can change the
    /// behavior.
    Derive(Vec<String>),
}

fn is_punct(tree: Option<&TokenTree>, c: char) -> bool {
    matches!(tree, Some(TokenTree::Punct(punct)) if punct.as_char() == c)
}

/// True for `::` at the index `at`.
fn colons(trees: &[TokenTree], at: usize) -> bool {
    matches!(trees.get(at), Some(TokenTree::Punct(punct)) if punct.as_char() == ':' && punct.spacing() == Spacing::Joint) && is_punct(trees.get(at + 1), ':')
}

fn ident_text(ident: &proc_macro2::Ident) -> String {
    ident.to_string().trim_start_matches("r#").to_string()
}

/// The kind of an attribute from the tokens between its brackets. `None`: not a `cfg` and
/// not a `cfg_attr`.
pub(crate) fn attr_kind(content: TokenStream) -> Option<Attr> {
    let trees: Vec<TokenTree> = content.into_iter().collect();
    let Some(TokenTree::Ident(name)) = trees.first() else { return None };
    if trees.len() != 2 {
        return None;
    }
    match name.to_string().as_str() {
        "cfg" => Some(match trees.get(1) {
            Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis && crate::cfgcond::covered(group.stream()) => Attr::Covered,
            _ => Attr::Select,
        }),
        "cfg_attr" => {
            let Some(TokenTree::Group(group)) = trees.get(1) else { return Some(Attr::Select) };
            let mut parts: Vec<Vec<TokenTree>> = vec![Vec::new()];
            for tree in group.stream() {
                if is_punct(Some(&tree), ',') {
                    parts.push(Vec::new());
                } else if let Some(part) = parts.last_mut() {
                    part.push(tree);
                }
            }
            let mut derives = Vec::new();
            // The first part is the condition.
            for part in parts.iter().skip(1).filter(|part| !part.is_empty()) {
                let Some(TokenTree::Ident(first)) = part.first() else { return Some(Attr::Select) };
                let first = first.to_string();
                let value = ["doc", "deprecated", "must_use"].contains(&first.as_str()) && is_punct(part.get(1), '=') && part.len() == 3;
                let list = match part.get(1) {
                    Some(TokenTree::Group(group)) if part.len() == 2 && group.delimiter() == Delimiter::Parenthesis => Some(group.stream().into_iter().collect::<Vec<_>>()),
                    _ => None,
                };
                if !(part.len() == 1 || value || list.is_some()) {
                    return Some(Attr::Select);
                }
                let list = list.unwrap_or_default();
                if CFG_ATTR_FREE.contains(&first.as_str()) {
                    continue;
                }
                let words = || list.iter().filter_map(|tree| if let TokenTree::Ident(ident) = tree { Some(ident.to_string()) } else { None });
                if first == "feature" && words().all(|word| DOC_FEATURES.contains(&word.as_str())) {
                    continue;
                }
                if first == "derive" {
                    // The last segment of each path.
                    for (at, tree) in list.iter().enumerate() {
                        if let TokenTree::Ident(ident) = tree {
                            if !colons(&list, at + 1) {
                                derives.push(ident_text(ident));
                            }
                        }
                    }
                    continue;
                }
                return Some(Attr::Select);
            }
            // A content that Semilla does not know stays `Select` above: it can be a `cfg` with
            // a condition that is not covered.
            let condition: TokenStream = parts.first().cloned().unwrap_or_default().into_iter().collect();
            Some(if crate::cfgcond::covered(condition) {
                Attr::Covered
            } else if derives.is_empty() {
                Attr::Free
            } else {
                Attr::Derive(derives)
            })
        }
        _ => None,
    }
}

/// True if each attribute of the item is a `cfg` or an attribute of the allow list. A
/// different attribute can be a macro: it can change the signature of a function, or
/// remove the function.
fn plain_attrs(item: &Item) -> bool {
    let _at = crate::cfgcond::at(&item.file);
    item.attrs.iter().all(|attr| {
        let Ok(stream) = attr.parse::<TokenStream>() else { return false };
        let Some(group) = stream.into_iter().find_map(|tree| match tree {
            TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket => Some(group),
            _ => None,
        }) else {
            return false;
        };
        let first = group.stream().into_iter().next().map(|tree| tree.to_string()).unwrap_or_default();
        match attr_kind(group.stream()) {
            Some(Attr::Free) => true,
            Some(Attr::Select) | Some(Attr::Covered) => first == "cfg",
            Some(Attr::Derive(_)) => false,
            None => first == "doc" || CFG_ATTR_FREE.contains(&first.as_str()),
        }
    })
}

/// The kind of an attribute text (`#[…]` or `#![…]`).
fn attr_of(attr: &str) -> Option<Attr> {
    let stream = match attr.parse::<TokenStream>() {
        Ok(stream) => stream,
        // Text that Semilla cannot read: the 5l rule.
        Err(_) => {
            let text = compact(attr).replace(' ', "");
            return ["#[cfg(", "#[cfg_attr(", "#![cfg(", "#![cfg_attr("].iter().any(|start| text.starts_with(start)).then_some(Attr::Select);
        }
    };
    let group = stream.into_iter().find_map(|tree| match tree {
        TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket => Some(group),
        _ => None,
    })?;
    attr_kind(group.stream())
}

/// The tokens of an item text. `macros`: the macros of the workspace (a name of the standard
/// list is then not the standard macro).
fn tokens(text: &str, macros: &BTreeSet<String>) -> Vec<Tok> {
    fn walk(stream: TokenStream, free: bool, outer: Option<&str>, macros: &BTreeSet<String>, out: &mut Vec<Tok>) {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        let mut next_free: Option<bool> = None;
        for (index, tree) in trees.iter().enumerate() {
            let before = |back: usize| index.checked_sub(back).and_then(|at| trees.get(at));
            match tree {
                TokenTree::Ident(ident) => {
                    // A lifetime is not a name of an item.
                    if is_punct(before(1), '\'') {
                        continue;
                    }
                    let word = ident_text(ident);
                    let after_colons = index >= 2 && colons(&trees, index - 2);
                    let after_dot = is_punct(before(1), '.') && !is_punct(before(2), '.');
                    let qual = if after_colons {
                        Some(match before(3) {
                            Some(TokenTree::Ident(owner)) => ident_text(owner),
                            Some(TokenTree::Punct(punct)) if punct.as_char() == '>' => ">".to_string(),
                            _ => String::new(),
                        })
                    } else if after_dot {
                        None
                    } else {
                        outer.map(str::to_string)
                    };
                    // A macro call: `name!(…)`, `macro_rules! name { … }`.
                    if is_punct(trees.get(index + 1), '!') && !is_punct(trees.get(index + 2), '=') && !KEYWORDS.contains(&word.as_str()) {
                        let group = |at: usize| matches!(trees.get(at), Some(TokenTree::Group(_)));
                        if group(index + 2) || (matches!(trees.get(index + 2), Some(TokenTree::Ident(_))) && group(index + 3)) {
                            next_free = Some(!MACRO_FREE.contains(&word.as_str()) || macros.contains(&word));
                        }
                    }
                    let def = !free && index.checked_sub(1).and_then(|at| trees.get(at)).is_some_and(|tree| matches!(tree, TokenTree::Ident(ident) if ident == "fn"));
                    out.push(Tok { member: after_colons || after_dot || qual.is_some(), prefix: colons(&trees, index + 1), word, qual, free, def, via: false });
                }
                TokenTree::Group(group) => {
                    // An attribute. The condition of a `cfg` is not a name. A `cfg_attr` of the
                    // allow list and a `doc` have no name of an item.
                    let attr = group.delimiter() == Delimiter::Bracket && (is_punct(before(1), '#') || (is_punct(before(1), '!') && is_punct(before(2), '#')));
                    if attr {
                        let first = group.stream().into_iter().next().map(|tree| tree.to_string()).unwrap_or_default();
                        if first == "doc" || matches!(attr_kind(group.stream()), Some(Attr::Free)) || (first == "cfg" && attr_kind(group.stream()).is_some()) {
                            continue;
                        }
                        if let Some(Attr::Derive(names)) = attr_kind(group.stream()) {
                            if names.iter().all(|name| derive_known(name)) {
                                continue;
                            }
                        }
                    }
                    // The braces of a `use` tree: `a::{b, c}`.
                    let owner = match (group.delimiter() == Delimiter::Brace && index >= 2 && colons(&trees, index - 2), before(3)) {
                        (true, Some(TokenTree::Ident(owner))) => Some(ident_text(owner)),
                        (true, _) => Some(String::new()),
                        _ => None,
                    };
                    walk(group.stream(), free || next_free.take().unwrap_or(false), owner.as_deref(), macros, out);
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    match text.parse::<TokenStream>() {
        Ok(stream) => walk(stream, false, None, macros, &mut out),
        // Text that Semilla cannot read: each word, with no position.
        Err(_) => out.extend(idents(text).map(|word| Tok { word: word.to_string(), free: true, ..Tok::default() })),
    }
    out
}

/// The name of the macro call that starts at the tree `at`: `name!(…)`, `name! word { … }`.
fn call_at(trees: &[TokenTree], at: usize) -> Option<String> {
    let Some(TokenTree::Ident(ident)) = trees.get(at) else { return None };
    let word = ident_text(ident);
    if !is_punct(trees.get(at + 1), '!') || is_punct(trees.get(at + 2), '=') || KEYWORDS.contains(&word.as_str()) {
        return None;
    }
    let group = |at: usize| matches!(trees.get(at), Some(TokenTree::Group(_)));
    (group(at + 2) || (matches!(trees.get(at + 2), Some(TokenTree::Ident(_))) && group(at + 3))).then_some(word)
}

/// The names of the macros that a text calls, at each depth.
fn macro_calls(text: &str) -> BTreeSet<String> {
    fn walk(stream: TokenStream, out: &mut BTreeSet<String>) {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        for (index, tree) in trees.iter().enumerate() {
            out.extend(call_at(&trees, index));
            if let TokenTree::Group(group) = tree {
                walk(group.stream(), out);
            }
        }
    }
    let mut out = BTreeSet::new();
    if let Ok(stream) = text.parse::<TokenStream>() {
        walk(stream, &mut out);
    }
    out
}

/// Each token of a text (a name, a literal, a punctuation mark, a delimiter), with the name of
/// the outermost macro call that it is a part of. `None`: Semilla cannot read the text.
fn leaves(text: &str) -> Option<Vec<(String, Option<String>)>> {
    fn walk(stream: TokenStream, mac: Option<&str>, out: &mut Vec<(String, Option<String>)>) {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        for (index, tree) in trees.iter().enumerate() {
            // The parts of a call: the name, `!`, a word (`macro_rules! name`), the group.
            let back = |count: usize| index.checked_sub(count).and_then(|at| call_at(&trees, at));
            let word_at = |at: usize| matches!(trees.get(at), Some(TokenTree::Ident(_)));
            let own = mac
                .map(str::to_string)
                .or_else(|| back(0))
                .or_else(|| back(1))
                .or_else(|| back(2))
                .or_else(|| back(3).filter(|_| word_at(index - 1)));
            match tree {
                TokenTree::Group(group) => {
                    let (open, close) = match group.delimiter() {
                        Delimiter::Parenthesis => ("(", ")"),
                        Delimiter::Brace => ("{", "}"),
                        Delimiter::Bracket => ("[", "]"),
                        Delimiter::None => ("", ""),
                    };
                    out.push((open.to_string(), own.clone()));
                    walk(group.stream(), own.as_deref(), out);
                    out.push((close.to_string(), own));
                }
                _ => out.push((tree.to_string(), own)),
            }
        }
    }
    let mut out = Vec::new();
    walk(text.parse::<TokenStream>().ok()?, None, &mut out);
    Some(out)
}

/// The token diff of two texts (batch 5p, C1): for each token of `old` and of `new`, true if
/// the token is not in a longest common sequence of the two lists. A token that the edit
/// moves is not in the sequence. Two long lists with a long changed part get the range
/// rule: each token between the common start and the common end is changed.
pub(crate) fn token_diff<T>(old: &[T], new: &[T], same: impl Fn(&T, &T) -> bool) -> (Vec<bool>, Vec<bool>) {
    let most = old.len().min(new.len());
    let prefix = (0..most).take_while(|at| same(&old[*at], &new[*at])).count();
    let suffix = (0..most - prefix).take_while(|at| same(&old[old.len() - 1 - at], &new[new.len() - 1 - at])).count();
    let (rows, columns) = (old.len() - prefix - suffix, new.len() - prefix - suffix);
    let mut old_changed: Vec<bool> = (0..old.len()).map(|at| at >= prefix && at < prefix + rows).collect();
    let mut new_changed: Vec<bool> = (0..new.len()).map(|at| at >= prefix && at < prefix + columns).collect();
    if rows == 0 || columns == 0 || rows.saturating_mul(columns) > 4_000_000 {
        return (old_changed, new_changed);
    }
    // The length of the longest common sequence of each pair of ends.
    let width = columns + 1;
    let mut table = vec![0u32; (rows + 1) * width];
    for row in (0..rows).rev() {
        for column in (0..columns).rev() {
            table[row * width + column] = if same(&old[prefix + row], &new[prefix + column]) {
                table[(row + 1) * width + column + 1] + 1
            } else {
                table[(row + 1) * width + column].max(table[row * width + column + 1])
            };
        }
    }
    let (mut row, mut column) = (0, 0);
    while row < rows && column < columns {
        if same(&old[prefix + row], &new[prefix + column]) && table[row * width + column] == table[(row + 1) * width + column + 1] + 1 {
            old_changed[prefix + row] = false;
            new_changed[prefix + column] = false;
            row += 1;
            column += 1;
        } else if table[(row + 1) * width + column] >= table[row * width + column + 1] {
            row += 1;
        } else {
            column += 1;
        }
    }
    (old_changed, new_changed)
}

/// The segments before `::*` in a `use` item: the sources of its glob imports.
fn glob_sources(text: &str) -> Vec<String> {
    fn walk(stream: TokenStream, outer: Option<String>, out: &mut Vec<String>) {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        for (index, tree) in trees.iter().enumerate() {
            let owner = || match index.checked_sub(3).and_then(|at| trees.get(at)) {
                Some(TokenTree::Ident(owner)) if index >= 2 && colons(&trees, index - 2) => Some(ident_text(owner)),
                _ if index >= 2 && colons(&trees, index - 2) => Some(String::new()),
                _ => None,
            };
            match tree {
                TokenTree::Punct(punct) if punct.as_char() == '*' => out.push(owner().or_else(|| outer.clone()).unwrap_or_default()),
                TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => walk(group.stream(), owner().or_else(|| outer.clone()), out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    match text.parse::<TokenStream>() {
        Ok(stream) => walk(stream, None, &mut out),
        Err(_) => out.push(String::new()),
    }
    out
}

/// The header of an item: its signature without the attributes.
fn header(item: &Item) -> String {
    let Ok(stream) = item.signature.parse::<TokenStream>() else { return item.signature.clone() };
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let mut at = 0;
    while is_punct(trees.get(at), '#') && matches!(trees.get(at + 1), Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Bracket) {
        at += 2;
    }
    trees[at..].iter().map(|tree| tree.to_string()).collect::<Vec<_>>().join(" ")
}

/// The parts of an `impl` item.
#[derive(Default)]
struct ImplParts {
    /// The last segment of the self type, if the self type is a path.
    head: Option<String>,
    /// True if the path of the self type has more than one segment.
    qualified: Option<String>,
    /// The last segment of the trait.
    of_trait: Option<String>,
    /// The type parameters of the `impl`.
    params: Vec<String>,
    /// True if Semilla can read the item.
    parsed: bool,
}

fn impl_parts(item: &Item) -> ImplParts {
    let Ok(value) = syn::parse_str::<syn::ItemImpl>(&item.text) else {
        let (head, of_trait) = impl_self(item);
        return ImplParts { head, of_trait: of_trait.then(|| "?".to_string()), ..ImplParts::default() };
    };
    let (head, qualified) = match &*value.self_ty {
        syn::Type::Path(path) if path.qself.is_none() => {
            (path.path.segments.last().map(|segment| ident_text(&segment.ident)), (path.path.segments.len() > 1).then(|| path.path.segments.first().map(|segment| ident_text(&segment.ident))).flatten())
        }
        _ => (None, None),
    };
    ImplParts {
        head,
        qualified,
        of_trait: value.trait_.as_ref().map(|(_, path, _)| path.segments.last().map(|segment| ident_text(&segment.ident)).unwrap_or_else(|| "?".to_string())),
        params: value.generics.type_params().map(|param| ident_text(&param.ident)).collect(),
        parsed: true,
    }
}

/// The words of a type, and of them the words that are only behind a pointer. A pointer
/// keeps the layout and the drop code of the pointee out of the owner.
fn type_words(ty: &syn::Type, behind: bool, direct: &mut BTreeSet<String>, shielded: &mut BTreeSet<String>) {
    use quote::ToTokens;
    let pointer = match ty {
        syn::Type::Reference(_) | syn::Type::Ptr(_) => true,
        syn::Type::Path(path) => path.qself.is_none() && path.path.segments.last().is_some_and(|segment| segment.ident == "NonNull"),
        _ => false,
    };
    let text = ty.to_token_stream().to_string();
    let target = if behind || pointer { shielded } else { direct };
    target.extend(idents(&text).map(str::to_string));
}

/// True if the type has no drop code and has a value in each configuration. `calm`: the
/// types of the workspace with this property. `shadow`: the names that the workspace gives a
/// different meaning.
fn plain_data(ty: &syn::Type, calm: &BTreeSet<String>, shadow: &BTreeSet<String>) -> bool {
    match ty {
        syn::Type::Reference(_) | syn::Type::Ptr(_) => true,
        syn::Type::Array(array) => plain_data(&array.elem, calm, shadow),
        syn::Type::Tuple(tuple) => tuple.elems.iter().all(|elem| plain_data(elem, calm, shadow)),
        syn::Type::Paren(paren) => plain_data(&paren.elem, calm, shadow),
        syn::Type::Path(path) if path.qself.is_none() => {
            let Some(last) = path.path.segments.last() else { return false };
            let name = ident_text(&last.ident);
            match &last.arguments {
                syn::PathArguments::None => (PLAIN.contains(&name.as_str()) && !shadow.contains(&name)) || (path.path.segments.len() == 1 && calm.contains(&name)),
                syn::PathArguments::AngleBracketed(args) => {
                    name == "Option" && !shadow.contains(&name) && args.args.len() == 1 && matches!(args.args.first(), Some(syn::GenericArgument::Type(inner)) if plain_data(inner, calm, shadow))
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// True for a type that has a size and a value in each configuration.
fn anchor(ty: &syn::Type, shadow: &BTreeSet<String>) -> bool {
    match ty {
        syn::Type::Reference(_) | syn::Type::Ptr(_) => true,
        syn::Type::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
            let name = ident_text(&path.path.segments[0].ident);
            PLAIN.contains(&name.as_str()) && !shadow.contains(&name)
        }
        _ => false,
    }
}

fn selects(attrs: &[syn::Attribute]) -> bool {
    use quote::ToTokens;
    attrs.iter().any(|attr| attr_of(&attr.to_token_stream().to_string()).is_some_and(|kind| !matches!(kind, Attr::Covered)))
}

/// The fields of a struct or an enum: for each one, true if the field or its variant is
/// below `cfg`, and the type. Then the count of the variants with no `cfg` (1 for a struct).
fn fields_of(item: &Item) -> Option<(Vec<(bool, syn::Type)>, usize, bool)> {
    let _at = crate::cfgcond::at(&item.file);
    let mut out = Vec::new();
    let mut add = |fields: &syn::Fields, below: bool| {
        for field in fields {
            out.push((below || selects(&field.attrs), field.ty.clone()));
        }
    };
    match syn::parse_str::<syn::Item>(&item.text).ok()? {
        syn::Item::Struct(value) => {
            add(&value.fields, false);
            Some((out, 1, true))
        }
        syn::Item::Enum(value) => {
            let mut count = 0;
            for variant in &value.variants {
                let below = selects(&variant.attrs);
                count += usize::from(!below);
                add(&variant.fields, below);
            }
            Some((out, count, false))
        }
        _ => None,
    }
}

/// The identifiers of a token text.
fn idents(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|token| !token.is_empty() && !token.starts_with(|c: char| c.is_ascii_digit()))
}

/// The tokens of a token text: each identifier, and each other character that is not a space.
fn words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (at, c) in text.char_indices() {
        let word = c.is_alphanumeric() || c == '_';
        if let Some(from) = start.filter(|_| !word) {
            out.push(&text[from..at]);
            start = None;
        }
        if word {
            start.get_or_insert(at);
        } else if !c.is_whitespace() {
            out.push(&text[at..at + c.len_utf8()]);
        }
    }
    out.extend(start.map(|from| &text[from..]));
    out
}

/// The name pairs of a rename map: the last name of each old path and of each new path.
pub(crate) fn rename_pairs(renames: &BTreeMap<String, String>) -> BTreeSet<(String, String)> {
    let name_of = |path: &str| {
        let last = segments(path).last().copied().unwrap_or("");
        plain(last.rsplit('.').next().unwrap_or(last)).to_string()
    };
    renames.iter().map(|(old, new)| (name_of(old), name_of(new))).collect()
}

/// True if the two token texts differ only by the names of a rename map: each token is the
/// same, or it is the old name and the new name of one pair. Such a text has no edit for the
/// rules of the build configuration (`Opaque::apply` and `srcconfig::apply` use this one
/// function).
pub(crate) fn same_text(old: &str, new: &str, pairs: &BTreeSet<(String, String)>) -> bool {
    let (old, new) = (words(old), words(new));
    old.len() == new.len() && old.iter().zip(&new).all(|(a, b)| a == b || pairs.contains(&(a.to_string(), b.to_string())))
}

/// The segments of an item path. A `::` inside `<…>` does not split.
fn segments(path: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => {
                out.push(&path[start..index]);
                index += 1;
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    out.push(&path[start..]);
    out
}

/// A path segment without the twin suffix (`#N`) and without the `!` of a macro.
fn plain(segment: &str) -> &str {
    let segment = match segment.rfind('#') {
        Some(at) if segment[at + 1..].chars().all(|c| c.is_ascii_digit()) && at + 1 < segment.len() => &segment[..at],
        _ => segment,
    };
    segment.trim_end_matches('!')
}

/// The name of an item: the last segment of its path. For a field: the name after the dot.
fn short(item: &Item) -> String {
    let last = segments(&item.path).last().copied().unwrap_or("");
    let last = if item.kind == ItemKind::Field { last.rsplit('.').next().unwrap_or(last) } else { last };
    let last = plain(last);
    // A macro call with a path: `cfg_if :: cfg_if!`.
    last.rsplit(':').next().unwrap_or(last).trim().to_string()
}

/// True if an attribute of the list can select code: a `cfg`, or a `cfg_attr` with a content
/// that is not in the allow list. A `derive` of known traits does not make the item a root:
/// the names of the trait methods get the rule of a trait `impl` under `cfg`.
/// `hidden`: true for a `derive` name that can have a different meaning in this crate (batch
/// 5q, part C2).
fn selects_any<'t>(attrs: impl IntoIterator<Item = &'t String>, derive_ok: bool, hidden: &dyn Fn(&str) -> bool) -> bool {
    attrs.into_iter().any(|attr| match attr_of(attr) {
        Some(Attr::Select) => true,
        Some(Attr::Derive(names)) => !derive_ok || names.iter().any(|name| !derive_known(name) || hidden(name)),
        Some(Attr::Free) | Some(Attr::Covered) | None => false,
    })
}

fn own_cfg(item: &Item, shadow: &Shadow) -> bool {
    let _at = crate::cfgcond::at(&item.file);
    selects_any(&item.attrs, matches!(item.kind, ItemKind::Struct | ItemKind::Enum), &|name| shadow.hides(item, name))
}

/// The crates whose `derive` names have the meaning of their name.
const TRUSTED_CRATES: [&str; 5] = ["std", "core", "alloc", "serde", "serde_derive"];

/// The `derive` names of the allow list that can have a different meaning (batch 5q, part
/// C2). The name of a derive macro is a short name. The compiler looks for it in this order:
/// the names of the module (a `use`, then a glob `use`), the macros of each
/// `#[macro_use] extern crate`, then the prelude of the standard library. Thus a `use`, a
/// glob `use`, or a `macro_use` can give the name `Clone` to a macro of a different crate.
/// That macro can make each kind of item.
#[derive(Default, Clone)]
pub(crate) struct Shadow {
    /// A `use` of the workspace gives this name a meaning from a crate that is not in the
    /// trusted list. A `use` of a different module can pass the name on. Thus the rule
    /// applies to each crate of the workspace.
    names: BTreeSet<String>,
    /// The crates with a `#[macro_use] extern crate` of a crate that is not in the trusted
    /// list, or with a `use` that Semilla cannot read.
    crates: BTreeSet<String>,
    /// The modules with a glob `use` of a crate that is not in the trusted list, or with a
    /// glob `use` of a module of this set. A glob `use` gives its names only to its module.
    modules: BTreeSet<String>,
}

/// One import of a `use` item.
pub(crate) struct Import {
    /// The first segment of the path.
    pub(crate) first: String,
    /// The name that the import gives. `None`: a glob.
    pub(crate) leaf: Option<String>,
    /// The last segment of the path. A `use` with `as` gives it a different name.
    source: String,
    /// The segments before the name. For a glob: each segment.
    prefix: Vec<String>,
    /// The path starts with `::`. Its first segment is the name of a crate.
    rooted: bool,
}

impl Shadow {
    /// A scope in an item (a function body) sees the names of each module above it. Thus
    /// the test uses each path above the item.
    fn hides(&self, item: &Item, name: &str) -> bool {
        let path = plain_path(&item.path);
        let parts = segments(&path);
        self.names.contains(name) || self.crates.contains(parts[0]) || (1..parts.len()).any(|count| self.modules.contains(&parts[..count].join("::")))
    }

    pub(crate) fn of(before: &Workspace, after: &Workspace) -> Shadow {
        let all = || [before, after].into_iter().flat_map(|ws| ws.items.values());
        let module_of = |item: &Item| {
            let path = plain_path(&item.path);
            let parts = segments(&path);
            parts[..parts.len() - 1].join("::")
        };
        // The paths of the items and the modules of the workspace.
        let mut paths: BTreeSet<String> = BTreeSet::new();
        let mut crates: BTreeSet<String> = BTreeSet::new();
        // The imports of each module.
        let mut imports: BTreeMap<String, Vec<Import>> = BTreeMap::new();
        let mut shadow = Shadow::default();
        for item in all() {
            let path = plain_path(&item.path);
            let parts = segments(&path);
            crates.insert(parts[0].to_string());
            match item.kind {
                ItemKind::Use => match use_imports(&item.text) {
                    Some(found) => imports.entry(module_of(item)).or_default().extend(found),
                    // A text that Semilla cannot read: the safe side.
                    None => drop(shadow.crates.insert(parts[0].to_string())),
                },
                ItemKind::Other => {}
                _ => {
                    for count in 1..=parts.len() {
                        paths.insert(parts[..count].join("::"));
                    }
                }
            }
        }
        // A first segment is foreign if it can be the name of a crate that is not in the
        // trusted list. An item of the same module, a crate of the workspace, and a name
        // that a `use` of the same module gives from a path that is not foreign are not.
        // A trusted name that an item or a `use` of the workspace gives is not trusted: a
        // glob `use` can pass that name to a different module.
        // `use serde::{self, Serialize}` gives the name `serde` from the path `serde`: the
        // name keeps its meaning.
        fn gives(import: &Import, name: &str) -> bool {
            import.leaf.as_deref() == Some(name) && import.first != name
        }
        let masked: BTreeSet<&str> = TRUSTED_CRATES.into_iter().filter(|name| paths.iter().any(|path| last_name(path) == *name) || imports.values().flatten().any(|import| gives(import, name))).collect();
        let crates: BTreeSet<String> = crates.into_iter().collect();
        let trusted: BTreeSet<String> = TRUSTED_CRATES.into_iter().filter(|name| !masked.contains(name)).map(str::to_string).chain(crates.iter().cloned()).collect();
        fn foreign(module: &str, first: &str, depth: usize, paths: &BTreeSet<String>, trusted: &BTreeSet<String>, imports: &BTreeMap<String, Vec<Import>>) -> bool {
            if ["crate", "self", "super"].contains(&first) || paths.contains(&format!("{module}::{first}")) {
                return false;
            }
            let given: Vec<&String> = imports.get(module).into_iter().flatten().filter(|import| gives(import, first)).map(|import| &import.first).collect();
            if !given.is_empty() {
                return depth == 0 || given.iter().any(|from| foreign(module, from, depth - 1, paths, trusted, imports));
            }
            !trusted.contains(first)
        }
        // The scopes of the workspace that the prefix of an import can mean. `None`: a
        // trusted crate that is not of the workspace, or a prefix that Semilla cannot
        // follow (`unsure`). An empty list: the prefix is not a path of the workspace
        // (an `extern crate` item, an import with a new name). That import has the rule
        // of a foreign import.
        // A test target is a module of its package in the paths of Semilla. Thus `crate`
        // can be each module above the import.
        struct Scopes<'a> {
            paths: &'a BTreeSet<String>,
            crates: &'a BTreeSet<String>,
            imports: &'a BTreeMap<String, Vec<Import>>,
        }
        impl Scopes<'_> {
            fn targets(&self, module: &str, first: &str, prefix: &[String], rooted: bool, depth: usize, unsure: &mut bool) -> Option<Vec<String>> {
                let parts = segments(module);
                let above = |count: usize| parts[..count].join("::");
                let join = |base: String, rest: &[String]| std::iter::once(base).chain(rest.iter().cloned()).collect::<Vec<_>>().join("::");
                let found: Vec<String> = match first {
                    _ if rooted && self.crates.contains(first) => vec![prefix.join("::")],
                    _ if rooted => return None,
                    "crate" => (1..=parts.len()).map(|count| join(above(count), &prefix[1..])).collect(),
                    "self" => vec![join(module.to_string(), &prefix[1..])],
                    "super" => {
                        let supers = prefix.iter().take_while(|part| *part == "super").count();
                        if supers >= parts.len() {
                            return Some(Vec::new());
                        }
                        vec![join(above(parts.len() - supers), &prefix[supers..])]
                    }
                    _ if self.paths.contains(&format!("{module}::{first}")) => vec![join(module.to_string(), prefix)],
                    // A `use` of the same module gives the first segment: follow its path.
                    _ if self.imports.get(module).into_iter().flatten().any(|other| gives(other, first)) => {
                        if depth == 0 {
                            *unsure = true;
                            return None;
                        }
                        let mut found = Vec::new();
                        for other in self.imports.get(module).into_iter().flatten().filter(|other| gives(other, first)) {
                            let full: Vec<String> = other.prefix.iter().chain([&other.source]).chain(prefix.iter().skip(1)).cloned().collect();
                            found.extend(self.targets(module, &other.first, &full, other.rooted, depth - 1, unsure)?);
                        }
                        return Some(found);
                    }
                    _ if self.crates.contains(first) => vec![prefix.join("::")],
                    _ => return None,
                };
                Some(found.into_iter().filter(|path| self.paths.contains(path) || self.crates.contains(path)).collect())
            }
        }
        let scopes_of = Scopes { paths: &paths, crates: &crates, imports: &imports };
        // A glob `use` of a scope of the workspace, and an import of a name from it.
        let mut globs: Vec<(String, Vec<String>)> = Vec::new();
        let mut named: Vec<(Vec<String>, String)> = Vec::new();
        // An import with a prefix that Semilla cannot follow: a glob `use` in the module,
        // or the name that the import gives.
        let mut loose: (BTreeSet<String>, BTreeSet<String>) = Default::default();
        for (module, found) in &imports {
            for import in found {
                let outside = if import.rooted { !trusted.contains(&import.first) } else { foreign(module, &import.first, 8, &paths, &trusted, &imports) };
                let mut unsure = false;
                let scopes = if outside { Some(Vec::new()) } else { scopes_of.targets(module, &import.first, &import.prefix, import.rooted, 8, &mut unsure) };
                match (&import.leaf, scopes) {
                    (Some(leaf), Some(scopes)) if scopes.is_empty() => drop(shadow.names.insert(leaf.clone())),
                    (Some(leaf), Some(scopes)) => named.push((scopes, leaf.clone())),
                    (Some(leaf), None) if unsure => drop(loose.1.insert(leaf.clone())),
                    (None, Some(scopes)) if scopes.is_empty() => drop(shadow.modules.insert(module.clone())),
                    (None, Some(scopes)) => globs.push((module.clone(), scopes)),
                    (None, None) if unsure => drop(loose.0.insert(module.clone())),
                    (_, None) => {}
                }
            }
        }
        // A `macro_use` of a different crate of the workspace.
        let mut edges: Vec<(String, String)> = Vec::new();
        for item in all().filter(|item| item.kind == ItemKind::Other) {
            let own = segments(&item.path)[0].to_string();
            let Ok(syn::Item::ExternCrate(value)) = syn::parse_str::<syn::Item>(&item.text) else { continue };
            let name = value.ident.to_string();
            let macro_use = value.attrs.iter().any(|attr| attr.path().is_ident("macro_use")) || item.attrs.iter().any(|attr| attr.contains("macro_use"));
            // `extern crate x as serde;` gives a trusted name to a different crate.
            let renamed = value.rename.as_ref().is_some_and(|(_, rename)| *rename != name && TRUSTED_CRATES.contains(&rename.to_string().as_str()));
            if renamed || (macro_use && !TRUSTED_CRATES.contains(&name.as_str()) && !crates.contains(&name)) {
                shadow.crates.insert(own);
            } else if macro_use && crates.contains(&name) && name != own {
                edges.push((own, name));
            }
        }
        // A `macro_use` gives the macros of the root of the crate. A glob `use` and an
        // import of a name pass on the names of their scope.
        loop {
            let full = |shadow: &Shadow, scope: &String| shadow.modules.contains(scope) || shadow.crates.contains(segments(scope)[0]);
            let any = !shadow.modules.is_empty() || !shadow.crates.is_empty() || !shadow.names.is_empty();
            let more_crates: Vec<String> = edges.iter().filter(|(from, to)| full(&shadow, to) && !shadow.crates.contains(from)).map(|(from, _)| from.clone()).collect();
            let more_modules: Vec<String> = globs
                .iter()
                .filter(|(_, scopes)| scopes.iter().any(|scope| full(&shadow, scope)))
                .map(|(module, _)| module)
                .chain(loose.0.iter().filter(|_| any))
                .filter(|module| !shadow.modules.contains(*module))
                .cloned()
                .collect();
            let more_names: Vec<String> = named
                .iter()
                .filter(|(scopes, _)| scopes.iter().any(|scope| full(&shadow, scope)))
                .map(|(_, leaf)| leaf)
                .chain(loose.1.iter().filter(|_| any))
                .filter(|leaf| !shadow.names.contains(*leaf))
                .cloned()
                .collect();
            if more_crates.is_empty() && more_modules.is_empty() && more_names.is_empty() {
                break;
            }
            shadow.crates.extend(more_crates);
            shadow.modules.extend(more_modules);
            shadow.names.extend(more_names);
        }
        shadow
    }
}

/// The names of the traits in the bounds of a trait: its supertraits, the bounds of its
/// `where` clause and of its parameters, and the bounds of its associated types. A marker
/// trait of the compiler (`Sized`, `Send`) is not in the list: the kinds `auto` and `open`
/// cover it. `None`: Semilla cannot read the text.
fn trait_bounds(text: &str) -> Option<Vec<String>> {
    fn add(bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>, out: &mut Vec<String>) -> Option<()> {
        for bound in bounds {
            match bound {
                syn::TypeParamBound::Trait(bound) => {
                    let name = bound.path.segments.last()?.ident.to_string();
                    if !matches!(bound.modifier, syn::TraitBoundModifier::Maybe(_)) && !["Sized", "Send", "Sync", "Unpin"].contains(&name.as_str()) {
                        out.push(name);
                    }
                }
                syn::TypeParamBound::Lifetime(_) => {}
                _ => return None,
            }
        }
        Some(())
    }
    fn generics(generics: &syn::Generics, out: &mut Vec<String>) -> Option<()> {
        for param in generics.type_params() {
            add(&param.bounds, out)?;
        }
        for predicate in generics.where_clause.iter().flat_map(|clause| &clause.predicates) {
            match predicate {
                syn::WherePredicate::Type(predicate) => add(&predicate.bounds, out)?,
                syn::WherePredicate::Lifetime(_) => {}
                _ => return None,
            }
        }
        Some(())
    }
    let item = syn::parse_str::<syn::ItemTrait>(text).ok()?;
    let mut out = Vec::new();
    add(&item.supertraits, &mut out)?;
    generics(&item.generics, &mut out)?;
    for member in &item.items {
        match member {
            syn::TraitItem::Type(member) => {
                add(&member.bounds, &mut out)?;
                generics(&member.generics, &mut out)?;
            }
            syn::TraitItem::Fn(_) | syn::TraitItem::Const(_) => {}
            // A macro call or an unknown member can make an associated type.
            _ => return None,
        }
    }
    Some(out)
}

/// The imports of a `use` item. `None`: Semilla cannot read the text.
pub(crate) fn use_imports(text: &str) -> Option<Vec<Import>> {
    fn walk(tree: &syn::UseTree, prefix: &mut Vec<String>, rooted: bool, out: &mut Vec<Import>) {
        let name = |ident: &syn::Ident| ident.to_string().trim_start_matches("r#").to_string();
        let first = |prefix: &[String], leaf: &str| prefix.first().cloned().unwrap_or_else(|| leaf.to_string());
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(name(&path.ident));
                walk(&path.tree, prefix, rooted, out);
                prefix.pop();
            }
            // `use a::b::{self}` gives the name `b` from the scope `a`.
            syn::UseTree::Name(leaf) if leaf.ident == "self" && !prefix.is_empty() => {
                let given = prefix.last().cloned().unwrap_or_default();
                out.push(Import { first: prefix[0].clone(), leaf: Some(given.clone()), source: given, prefix: prefix[..prefix.len() - 1].to_vec(), rooted });
            }
            syn::UseTree::Name(leaf) => {
                let leaf = name(&leaf.ident);
                out.push(Import { first: first(prefix, &leaf), leaf: Some(leaf.clone()), source: leaf, prefix: prefix.clone(), rooted });
            }
            syn::UseTree::Rename(leaf) => out.push(Import { first: first(prefix, &name(&leaf.ident)), leaf: Some(name(&leaf.rename)), source: name(&leaf.ident), prefix: prefix.clone(), rooted }),
            syn::UseTree::Glob(_) => out.push(Import { first: first(prefix, ""), leaf: None, source: String::new(), prefix: prefix.clone(), rooted }),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    walk(tree, prefix, rooted, out);
                }
            }
        }
    }
    let item = syn::parse_str::<syn::ItemUse>(text).ok()?;
    let mut out = Vec::new();
    walk(&item.tree, &mut Vec::new(), item.leading_colon.is_some(), &mut out);
    Some(out)
}

/// The name of the derive macro that an attribute `proc_macro_derive(Name, …)` defines.
fn derive_macro(attr: &str) -> Option<String> {
    let (_, rest) = attr.split_once("proc_macro_derive")?;
    let name: String = rest.trim_start().strip_prefix('(')?.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// The traits of a `derive` in a `cfg_attr` of the item, if each one is a known trait.
fn derived(item: &Item, shadow: &Shadow) -> Vec<String> {
    if own_cfg(item, shadow) {
        return Vec::new();
    }
    let _at = crate::cfgcond::at(&item.file);
    item.attrs.iter().filter_map(|attr| if let Some(Attr::Derive(names)) = attr_of(attr) { Some(names) } else { None }).flatten().collect()
}

/// An item path without the twin suffixes.
fn plain_path(path: &str) -> String {
    segments(path).into_iter().map(plain).collect::<Vec<_>>().join("::")
}

/// The last segment of a type path, without generic arguments.
fn last_name(path: &str) -> &str {
    let path = path.split('<').next().unwrap_or(path);
    path.rsplit("::").next().unwrap_or(path).trim()
}

/// The self type of an `impl` item, and true for a trait impl. The self type is `None` if it
/// is not a plain path.
fn impl_self(item: &Item) -> (Option<String>, bool) {
    let last = segments(&item.path).last().copied().unwrap_or("");
    let owner = plain(last).trim_start_matches("impl ").trim();
    let (text, of_trait) = match owner.strip_prefix('<').and_then(|rest| rest.strip_suffix('>')) {
        Some(inner) => (inner.split(" as ").next().unwrap_or(""), true),
        None => (owner, false),
    };
    let path = !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':');
    (path.then(|| text.rsplit("::").next().unwrap_or(text).to_string()), of_trait)
}

/// The kind and the name of each member of an `impl` or a trait (`fn name`, `const NAME`).
fn members(item: &Item) -> Vec<(String, String)> {
    item.members
        .iter()
        .filter_map(|member| {
            let mut words = idents(member);
            let kind = words.next()?;
            ["fn", "const", "type"].contains(&kind).then(|| words.next().map(|name| (kind.to_string(), name.to_string()))).flatten()
        })
        .collect()
}

/// The associated constants and the associated types of an `impl` or a trait that have a
/// value: the name, true for a type, and the text of the value (batch 5p). An item that
/// Semilla cannot read gives its full text for each member.
fn member_values(item: &Item) -> Vec<(String, bool, String)> {
    use quote::ToTokens;
    let text = |tokens: &dyn ToTokens| tokens.to_token_stream().to_string();
    if !idents(&item.text).any(|word| word == "const" || word == "type") {
        return Vec::new();
    }
    if let Ok(value) = syn::parse_str::<syn::ItemImpl>(&item.text) {
        return value
            .items
            .iter()
            .filter_map(|member| match member {
                syn::ImplItem::Const(constant) => Some((ident_text(&constant.ident), false, format!("{} = {}", text(&constant.ty), text(&constant.expr)))),
                syn::ImplItem::Type(ty) => Some((ident_text(&ty.ident), true, text(&ty.ty))),
                _ => None,
            })
            .collect();
    }
    if let Ok(value) = syn::parse_str::<syn::ItemTrait>(&item.text) {
        return value
            .items
            .iter()
            .filter_map(|member| match member {
                syn::TraitItem::Const(constant) => constant.default.as_ref().map(|(_, expr)| (ident_text(&constant.ident), false, format!("{} = {}", text(&constant.ty), text(expr)))),
                syn::TraitItem::Type(ty) => ty.default.as_ref().map(|(_, value)| (ident_text(&ty.ident), true, text(value))),
                _ => None,
            })
            .collect();
    }
    members(item).into_iter().filter(|(kind, _)| kind != "fn").map(|(kind, name)| (name, kind == "type", item.text.clone())).collect()
}

/// The names that a `use` item gives, and true for a glob import.
fn use_leaves(text: &str) -> (Vec<String>, bool) {
    fn walk(stream: TokenStream, prefix: Option<String>, out: &mut Vec<String>, glob: &mut bool) {
        let mut last: Option<String> = None;
        let mut rename = false;
        for tree in stream {
            match tree {
                TokenTree::Ident(ident) => {
                    let name = ident.to_string();
                    match name.as_str() {
                        "pub" | "use" | "in" => {}
                        "as" => rename = true,
                        "self" if prefix.is_some() => last = prefix.clone(),
                        _ => {
                            last = (name != "_" || !rename).then(|| name.trim_start_matches("r#").to_string());
                            rename = false;
                        }
                    }
                }
                TokenTree::Punct(punct) if punct.as_char() == '*' => {
                    *glob = true;
                    last = None;
                }
                TokenTree::Punct(punct) if punct.as_char() == ',' => out.extend(last.take()),
                TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
                    walk(group.stream(), last.take(), out, glob);
                }
                _ => {}
            }
        }
        out.extend(last);
    }
    let (mut out, mut glob) = (Vec::new(), false);
    match text.parse::<TokenStream>() {
        Ok(stream) => walk(stream, None, &mut out, &mut glob),
        // Text that Semilla cannot read: each word is a name.
        Err(_) => {
            out.extend(idents(text).map(str::to_string));
            glob = true;
        }
    }
    out.retain(|name| !["crate", "super", "self"].contains(&name.as_str()));
    (out, glob)
}

/// The names before `as` in a `use` item: the items that get a new name.
fn use_sources(text: &str) -> Vec<String> {
    fn walk(stream: TokenStream, out: &mut Vec<String>) {
        let mut last: Option<String> = None;
        for tree in stream {
            match tree {
                TokenTree::Ident(ident) if ident.to_string() == "as" => out.extend(last.take()),
                TokenTree::Ident(ident) => last = Some(ident.to_string().trim_start_matches("r#").to_string()),
                TokenTree::Group(group) => {
                    last = None;
                    walk(group.stream(), out);
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(stream) = text.parse::<TokenStream>() {
        walk(stream, &mut out);
    }
    out.retain(|name| !["crate", "super", "self"].contains(&name.as_str()));
    out
}

/// True for the definition of a macro (`macro_rules! name { … }`), false for a macro call.
fn is_definition(text: &str) -> bool {
    let Ok(stream) = text.parse::<TokenStream>() else { return false };
    let first = stream.into_iter().find(|tree| !matches!(tree, TokenTree::Punct(_)) && !matches!(tree, TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket));
    matches!(first, Some(TokenTree::Ident(ident)) if ident == "macro_rules")
}

/// The names of the items that a token text makes: each word after an item keyword.
fn made_names(text: &str) -> Vec<String> {
    let words: Vec<&str> = idents(text).filter(|word| !["mut", "unsafe", "async", "extern"].contains(word)).collect();
    let keywords = ["fn", "const", "static", "type", "struct", "enum", "union", "trait", "mod", "macro_rules"];
    words.windows(2).filter(|pair| keywords.contains(&pair[0]) && !WORDS.contains(&pair[1])).map(|pair| pair[1].to_string()).collect()
}

/// The module of an item: the path without the last segment.
/// True if a `const` parameter of the item has a default that is not a literal.
fn const_default(item: &Item) -> bool {
    let kinds = [ItemKind::Struct, ItemKind::Enum, ItemKind::Trait, ItemKind::Type];
    if !kinds.contains(&item.kind) && item.kind != ItemKind::Other {
        return false;
    }
    let generics = match syn::parse_str::<syn::Item>(&item.text) {
        Ok(syn::Item::Struct(value)) => value.generics,
        Ok(syn::Item::Enum(value)) => value.generics,
        Ok(syn::Item::Union(value)) => value.generics,
        Ok(syn::Item::Trait(value)) => value.generics,
        Ok(syn::Item::TraitAlias(value)) => value.generics,
        Ok(syn::Item::Type(value)) => value.generics,
        Ok(_) => return false,
        // No syntax tree: the default is not known.
        Err(_) => return kinds.contains(&item.kind) && item.text.contains("const"),
    };
    fn literal(expr: &syn::Expr) -> bool {
        match expr {
            syn::Expr::Lit(_) => true,
            syn::Expr::Unary(unary) => matches!(unary.op, syn::UnOp::Neg(_)) && literal(&unary.expr),
            syn::Expr::Block(block) => match block.block.stmts.as_slice() {
                [syn::Stmt::Expr(inner, None)] => block.label.is_none() && literal(inner),
                _ => false,
            },
            _ => false,
        }
    }
    generics.const_params().any(|param| param.default.as_ref().is_some_and(|default| !literal(default)))
}

fn module_of(item: &Item) -> String {
    let parts = segments(&item.path);
    parts[..parts.len().saturating_sub(1)].join("::")
}

/// The traits of the standard library that the code uses with no name (an operator, a drop,
/// a loop, a call), or that have an associated type. An `impl` of one of them gets the
/// coarse rule.
const SILENT_TRAITS: [&str; 44] = [
    "Drop", "Deref", "DerefMut", "Index", "IndexMut", "Iterator", "IntoIterator", "DoubleEndedIterator", "ExactSizeIterator", "Future", "IntoFuture", "Fn", "FnMut", "FnOnce", "Add", "Sub", "Mul",
    "Div", "Rem", "Neg", "Not", "BitAnd", "BitOr", "BitXor", "Shl", "Shr", "AddAssign", "SubAssign", "MulAssign", "DivAssign", "RemAssign", "BitAndAssign", "BitOrAssign", "BitXorAssign",
    "ShlAssign", "ShrAssign", "FromStr", "TryFrom", "TryInto", "Borrow", "AsRef", "ToOwned", "FromIterator", "Extend",
];

/// The functions of the prelude. A call of a function with one of these names can mean the
/// prelude item in a different configuration.
const PRELUDE_FNS: [&str; 5] = ["drop", "Some", "None", "Ok", "Err"];

/// The kind of the self type of an `impl`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Head {
    /// A struct or an enum of the workspace.
    Local(String),
    /// A type of the standard library.
    Std(String),
    /// A type of a crate outside the workspace: the foreign rule stops each body with it.
    Foreign(String),
}

impl Head {
    fn name(&self) -> &str {
        match self {
            Head::Local(name) | Head::Std(name) | Head::Foreign(name) => name,
        }
    }
}

/// One name of the expanded code of an item, by the driver.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Expanded {
    word: String,
    member: bool,
    qual: Option<String>,
    prefix: bool,
    /// The outermost macro call that gives the name.
    mac: Option<String>,
    line: usize,
    /// A call or a path with a rival (batch 5q).
    rival: Option<String>,
}

impl Expanded {
    fn tok(&self) -> Tok {
        Tok { word: self.word.clone(), member: self.member, qual: self.qual.clone(), prefix: self.prefix, ..Tok::default() }
    }
}

/// The hard names of an item for a reader (batch 5p). The rule is the union: a name is a use
/// if the tokens of the text give it or the expanded code gives it.
#[derive(Default)]
struct Hard {
    /// By the tokens of the text of the item: the count of each name.
    text: BTreeMap<String, usize>,
    /// By the expanded code: the count of each name, and the macro calls that give it
    /// (`None`: the name is not from an expansion).
    expanded: BTreeMap<String, (usize, BTreeSet<Option<String>>)>,
}

impl Hard {
    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.expanded.is_empty()
    }
}

/// The opaque inputs of a step.
pub(crate) struct Opaque<'a> {
    sides: [&'a Workspace; 2],
    /// The hard names, each with its causes.
    hard: BTreeMap<String, Vec<Cause>>,
    /// The soft names, each with the text for the spotlight.
    soft: BTreeMap<String, String>,
    /// The opaque types: the plain paths, and the short names for a path that the workspace
    /// does not define.
    types: BTreeSet<String>,
    type_names: BTreeSet<String>,
    /// The types with a layout that the configuration selects, but with a size, a value, and
    /// the same drop code in each configuration. A body that only moves such a value, names a
    /// field, or builds it with each field is free. A body that reads a part of it is not.
    hold: BTreeSet<String>,
    hold_names: BTreeSet<String>,
    /// The types of the standard library with a trait `impl` under `cfg` of the coarse rule.
    heads: BTreeSet<String>,
    /// The plain paths of the structs and the enums of the workspace.
    defined: BTreeSet<String>,
    /// The modules with names that Semilla cannot list: a glob import or a macro call that
    /// the configuration selects.
    globs: BTreeSet<String>,
    /// The functions that the configuration selects, for each side.
    selected: [BTreeSet<String>; 2],
    /// The functions under `cfg` whose call can stay a node (batch 5o, A3): the plain paths.
    exempt: BTreeSet<String>,
    /// The text for the spotlight of a new call of such a function.
    node_text: BTreeMap<String, String>,
    /// The root above each item, for each side: a reader inside its root is exempt.
    roots: [BTreeMap<String, String>; 2],
    /// The plain paths of the modules and the crates.
    modules: BTreeSet<String>,
    /// The sources of the glob imports of each module (the segment before `::*`).
    glob_from: BTreeMap<String, BTreeSet<String>>,
    /// The glob imports of each module that are not private.
    glob_pub: BTreeMap<String, BTreeSet<String>>,
    /// The names that an `as` gives.
    renames: BTreeSet<String>,
    /// The `derive` names that can have a different meaning (batch 5q, part C2).
    shadow: Shadow,
    /// The macros of the workspace.
    macros: BTreeSet<String>,
    /// For each macro of the workspace: the words of its definitions, and the macros that
    /// its definitions call (batch 5p).
    macro_words: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)>,
    /// The tokens of each text with the tokens of the macros that the text calls.
    wide: RefCell<HashMap<(usize, usize), Rc<Vec<Tok>>>>,
    /// The names of the expanded code of each item, by the driver, for each side, with the
    /// count of each name (batch 5p).
    expanded: RefCell<[BTreeMap<String, BTreeMap<Expanded, usize>>; 2]>,
    /// The scope of the members of each `impl`: the key is the plain path of the owner.
    member_scope: BTreeMap<String, (Place, Form)>,
    /// The paths of the traits.
    traits: BTreeSet<String>,
    toks: RefCell<HashMap<(usize, usize), Rc<Vec<Tok>>>>,
    memo: RefCell<BTreeMap<(bool, String), bool>>,
    /// The bodies that an entry of the driver stops, for each side.
    layouts: RefCell<[BTreeSet<String>; 2]>,
    aliases: RefCell<[BTreeMap<String, BTreeSet<String>>; 2]>,
    /// The short names of the types of each item, by the driver, for each side.
    entries: RefCell<[BTreeMap<String, BTreeSet<String>>; 2]>,
    /// The opaque types of each body, by the driver, with the text for the spotlight.
    type_uses: RefCell<[BTreeMap<String, BTreeSet<String>>; 2]>,
    /// The items under `cfg` that each item uses, by the driver, for each side.
    cfg_uses: RefCell<[BTreeMap<String, BTreeSet<String>>; 2]>,
    /// The items below a `cfg` attribute with a covered condition, and below no other `cfg`
    /// attribute, for each side (batch 5s, part C): the plain paths. The driver names such an
    /// item as an item under `cfg`. Its use does not stop a proof.
    covered: [BTreeSet<String>; 2],
    /// The trait facts that the configuration can change (batch 5q).
    facts: Facts,
}

/// The trait facts of the workspace that the build configuration can change (batch 5q). A
/// call with a rival is an input of the configuration only if a fact of its kind exists.
#[derive(Default)]
struct Facts {
    /// A trait fact can change: a trait `impl` under `cfg`, a `derive` in a `cfg_attr`, a
    /// `use` under `cfg`, a trait under `cfg` or with a `cfg` in it, an item macro with
    /// `cfg`.
    some: bool,
    /// Semilla cannot list the facts: an item macro with `cfg`, a glob `use` under `cfg`, an
    /// `impl` that Semilla cannot read, an unknown `derive`, a trait under `cfg` with a
    /// bound that is not a trait of the workspace.
    any: bool,
    /// A field or a variant under `cfg`: an auto trait (`Send`) can change.
    fields: bool,
    /// For each trait name: the names of the self types of the impls that the configuration
    /// selects. `*`: a type with no simple name.
    traits: BTreeMap<String, BTreeSet<String>>,
}

impl Facts {
    fn of_trait(&mut self, name: &str, head: &str) {
        self.some = true;
        self.traits.entry(name.to_string()).or_default().insert(head.to_string());
    }

    /// True if the rival of a call can win in a different configuration. `kinds` is the
    /// first part of the entry of the driver.
    fn input(&self, kinds: &str) -> bool {
        kinds.split(' ').any(|kind| match kind {
            "open" => self.some,
            "auto" => self.some || self.fields,
            // A crate outside the workspace can add the `impl`. A feature attribute can make
            // the item stable. Semilla does not read these.
            "foreign" | "unstable" => true,
            _ => match kind.strip_prefix("closed:").and_then(|rest| rest.split_once(':')) {
                Some((name, head)) => self.any || self.traits.get(name).is_some_and(|heads| head == "*" || heads.contains("*") || heads.contains(head)),
                // A new kind: the safe side.
                None => true,
            },
        })
    }
}

impl<'a> Opaque<'a> {
    /// The tokens of a text of an item (the text or the signature).
    fn toks(&self, text: &str) -> Rc<Vec<Tok>> {
        let key = (text.as_ptr() as usize, text.len());
        if let Some(found) = self.toks.borrow().get(&key) {
            return found.clone();
        }
        let made = Rc::new(tokens(text, &self.macros));
        self.toks.borrow_mut().insert(key, made.clone());
        made
    }

    /// The tokens of a text of an item that is not a body with a list of the driver (a
    /// signature, a type, a declaration, the value of a constant), with the names that its
    /// macro calls can give (batch 5p). A macro of the workspace gives each word of its
    /// definitions, and of the definitions that they call, with no position. A different
    /// macro that is not in the list of the standard library gives a token for each name
    /// (`Tok::wild`).
    fn wide(&self, text: &str) -> Rc<Vec<Tok>> {
        let key = (text.as_ptr() as usize, text.len());
        if let Some(found) = self.wide.borrow().get(&key) {
            return found.clone();
        }
        let made = Rc::new(self.widen(&self.toks(text), text));
        self.wide.borrow_mut().insert(key, made.clone());
        made
    }

    /// The tokens `narrow` of a text, with the names that the macro calls of the text can
    /// give (see `wide`).
    fn widen(&self, narrow: &[Tok], text: &str) -> Vec<Tok> {
        let mut made: Vec<Tok> = narrow.to_vec();
        let extra = |word: String| Tok { word, free: true, via: true, ..Tok::default() };
        let mut wild: BTreeSet<String> = BTreeSet::new();
        let mut note_calls = |calls: &BTreeSet<String>, queue: &mut Vec<String>| {
            for name in calls {
                if self.macro_words.contains_key(name) {
                    queue.push(name.clone());
                } else if !MACRO_FREE.contains(&name.as_str()) && name != "macro_rules" {
                    wild.insert(name.clone());
                }
            }
        };
        // A word of the text with the name of a macro can be a call after the expansion
        // (`outer!(inner)`).
        let mut queue: Vec<String> = narrow.iter().filter(|tok| self.macro_words.contains_key(&tok.word)).map(|tok| tok.word.clone()).collect();
        note_calls(&macro_calls(text), &mut queue);
        let mut seen = BTreeSet::new();
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some((words, calls)) = self.macro_words.get(&name) else { continue };
            note_calls(calls, &mut queue);
            for word in words {
                if self.macro_words.contains_key(word) {
                    queue.push(word.clone());
                }
                made.push(extra(word.clone()));
            }
        }
        made.extend(wild.into_iter().map(|name| extra(format!("{name}!"))));
        made
    }

    /// The module of an item path: the longest part of the path that is a module.
    fn module(&self, path: &str) -> String {
        let mut parts: Vec<&str> = segments(path).into_iter().map(plain).collect();
        while parts.len() > 1 && !self.modules.contains(&parts.join("::")) {
            parts.pop();
        }
        parts.join("::")
    }

    /// True if an item of the module `from` can use a name of the module `target` with no
    /// path: the same module, or a chain of glob imports. A glob import with a source that is
    /// not a known module (`super`, `self`, `crate`, a name of an `as`) sees each module.
    fn sees(&self, from: &str, target: &str) -> bool {
        let (mut seen, mut queue) = (BTreeSet::new(), vec![from.to_string()]);
        while let Some(module) = queue.pop() {
            if module == target {
                return true;
            }
            if !seen.insert(module.clone()) {
                continue;
            }
            for source in self.glob_from.get(&module).into_iter().flatten() {
                if ["", "self", "super", "crate"].contains(&source.as_str()) || self.renames.contains(source) {
                    return true;
                }
                queue.extend(self.modules.iter().filter(|known| known.rsplit("::").next() == Some(source.as_str())).cloned());
            }
        }
        false
    }

    /// The modules whose names an item of the module `from` can use with no path: the
    /// module and the sources of its glob imports. A glob import gives only the names
    /// that the importer can see: a private glob import of a source counts only for an
    /// importer in or below that source. `None`: a source is not a known module.
    fn visible(&self, from: &str) -> Option<BTreeSet<String>> {
        let (mut seen, mut queue) = (BTreeSet::new(), vec![(from.to_string(), true)]);
        while let Some((module, private)) = queue.pop() {
            if !seen.insert((module.clone(), private)) {
                continue;
            }
            let sources = if private { self.glob_from.get(&module) } else { self.glob_pub.get(&module) };
            for source in sources.into_iter().flatten() {
                let found: Vec<String> = self.modules.iter().filter(|known| known.rsplit("::").next() == Some(source.as_str())).cloned().collect();
                if found.is_empty() || ["", "self", "super", "crate"].contains(&source.as_str()) || self.renames.contains(source) {
                    return None;
                }
                queue.extend(found.into_iter().map(|next| {
                    let inside = module == next || module.starts_with(&format!("{next}::"));
                    (next, inside)
                }));
            }
        }
        Some(seen.into_iter().map(|(module, _)| module).collect())
    }

    /// True if the body of the item can use a type with the name `head`. The driver gives
    /// the types of each body. An item with no entry of the driver has no answer: true.
    fn uses_type(&self, item: &Item, head: &str) -> bool {
        // A `use` cannot import a member of a type.
        if item.kind == ItemKind::Use {
            return false;
        }
        let entries = self.entries.borrow();
        let found: Vec<&BTreeSet<String>> = entries.iter().filter_map(|side| side.get(&item.path)).collect();
        if found.is_empty() || found.iter().any(|names| names.contains(head) || names.contains("?")) {
            return true;
        }
        // A path with the type (`T::name`), and a member of the type (`Self::name`).
        segments(&item.path).iter().any(|part| idents(part).any(|word| word == head)) || self.toks(&item.text).iter().any(|tok| tok.word == head)
    }

    /// True if the token of the reader `item` can mean the item of the cause.
    fn hit(&self, item: &Item, tok: &Tok, cause: &Cause) -> bool {
        if tok.def {
            return false;
        }
        if let Some(root) = &cause.root {
            if item.path == *root || item.path.starts_with(&format!("{root}::")) {
                return false;
            }
        }
        let place = || match &cause.place {
            Place::Any => true,
            Place::Module(module) => tok.free || self.sees(&self.module(&item.path), module),
            Place::Type(head) => self.uses_type(item, head),
        };
        match (cause.form, &cause.place) {
            (Form::Any, _) => place(),
            (Form::Member, _) => (tok.member || tok.free) && place(),
            (Form::Prefix, _) => tok.prefix || tok.free,
            (Form::Path, Place::Module(module)) => match &tok.qual {
                _ if tok.free => true,
                None => !tok.member && self.sees(&self.module(&item.path), module),
                // `crate::name`: the root module. `self::name`: the module of the reader.
                // `super::name`: a module above the reader (`super::super::name` too).
                Some(owner) if owner == "crate" => self.sees(segments(&item.path)[0], module),
                Some(owner) if owner == "self" => self.sees(&self.module(&item.path), module),
                Some(owner) if owner == "super" => {
                    let reader = self.module(&item.path);
                    let parts = segments(&reader);
                    (1..parts.len()).any(|count| self.sees(&parts[..count].join("::"), module))
                }
                Some(owner) => {
                    owner.is_empty()
                        || self.renames.contains(owner)
                        || self.modules.iter().any(|known| known.rsplit("::").next() == Some(owner.as_str()) && self.sees(known, module))
                }
            },
            (Form::Path, _) => place(),
        }
    }

    /// True if the token is a hard name for the reader `item`.
    fn reads(&self, item: &Item, tok: &Tok) -> bool {
        tok.wild() || self.hard.get(&tok.word).is_some_and(|causes| causes.iter().any(|cause| self.hit(item, tok, cause)))
    }

    /// The first hard name in a text of the item that `test` accepts. The text is not a body:
    /// the names that its macro calls can give count (batch 5p).
    fn first_hit(&self, item: &Item, text: &str, test: &dyn Fn(&str) -> bool) -> Option<String> {
        self.wide(text).iter().find(|tok| (tok.via || test(&tok.word)) && self.reads(item, tok)).map(|tok| tok.word.clone())
    }

    /// The hard names of an item of one side, with the count of each one: in the tokens of
    /// its text, and in its expanded code (the list of the driver). A macro puts names into
    /// a body that the text of the body does not have (batch 5p).
    fn hard_names(&self, side: usize, item: &Item) -> Hard {
        let mut found = Hard::default();
        for tok in self.toks(&item.text).iter() {
            if self.reads(item, tok) {
                *found.text.entry(tok.word.clone()).or_default() += 1;
            }
        }
        let expanded = self.expanded.borrow();
        for (name, count) in expanded[side].get(&item.path).into_iter().flatten() {
            if (self.hard.contains_key(&name.word) && self.reads(item, &name.tok())) || self.rival(name).is_some() {
                let entry = found.expanded.entry(name.word.clone()).or_default();
                entry.0 += count;
                entry.1.insert(name.mac.clone());
            }
        }
        found
    }

    /// The message of a call or a path with a rival that can win in a different
    /// configuration (batch 5q). Such a name is hard for each reader.
    fn rival<'e>(&self, name: &'e Expanded) -> Option<&'e str> {
        // One line for each entry: the kinds, then the message.
        name.rival.as_deref()?.lines().find_map(|line| {
            let (kinds, text) = line.split_once('|').unwrap_or(("?", line));
            self.facts.input(kinds).then_some(text)
        })
    }

    fn add(&mut self, name: &str, text: &str, root: Option<String>, scope: (Place, Form)) {
        if name.is_empty() {
            return;
        }
        let causes = self.hard.entry(name.to_string()).or_default();
        if !causes.iter().any(|cause| cause.root == root && cause.place == scope.0 && cause.form == scope.1) {
            causes.push(Cause { text: text.to_string(), root, place: scope.0, form: scope.1 });
        }
    }

    /// True if the name is hard in this scope (or in each scope) for each reader.
    fn known(&self, name: &str, scope: &(Place, Form)) -> bool {
        self.hard.get(name).is_some_and(|causes| causes.iter().any(|cause| cause.root.is_none() && ((cause.place == scope.0 && cause.form == scope.1) || (cause.place == Place::Any && cause.form == Form::Any))))
    }

    /// The scope of the name of an item: where the name can mean the item.
    fn scope(&self, item: &Item) -> (Place, Form) {
        let parts = segments(&item.path);
        let parent = parts[..parts.len().saturating_sub(1)].iter().map(|part| plain(part)).collect::<Vec<_>>().join("::");
        if self.modules.contains(&parent) {
            // A path of the module: `name`, `self::name`, `module::name`.
            (Place::Module(parent), Form::Path)
        } else if self.traits.contains(&parent) {
            (Place::Any, Form::Member)
        } else {
            // A member of an `impl`. An item in a function has no known scope.
            self.member_scope.get(&parent).cloned().unwrap_or((Place::Any, Form::Any))
        }
    }

    fn opaque_item(&mut self, item: &Item) {
        self.types.insert(plain_path(&item.path));
        self.type_names.insert(short(item));
        self.hold.remove(&plain_path(&item.path));
    }

    /// Makes each type with this name opaque.
    fn opaque_name(&mut self, name: &str) {
        let paths: Vec<String> = self.defined.iter().filter(|path| last_name(path) == name).cloned().collect();
        self.types.extend(paths);
        self.type_names.insert(name.to_string());
    }

    /// True if the type of an entry of the driver is opaque. `touch`: the body reads a part
    /// of the value.
    fn opaque_type(&self, path: &str, touch: bool) -> bool {
        let path = path.split('<').next().unwrap_or(path).trim();
        if self.types.contains(path) || (touch && self.hold.contains(path)) {
            return true;
        }
        if self.defined.contains(path) {
            return false;
        }
        let name = last_name(path);
        self.type_names.contains(name) || (touch && self.hold_names.contains(name))
    }
}

/// True if the struct or the enum has a size, a value, and the same drop code in each
/// configuration. `dependent`: the words of the field types that the configuration selects.
/// Each field that the configuration selects must be plain data. A struct needs a field
/// with no `cfg` of a pointer type or a primitive type: the struct is never a type with the
/// size zero. An enum needs two variants with no `cfg`, and only plain data.
fn holdable(item: &Item, dependent: &BTreeSet<String>, calm: &BTreeSet<String>, shadow: &BTreeSet<String>, droppers: &BTreeSet<String>) -> bool {
    let Some((fields, count, is_struct)) = fields_of(item) else { return false };
    if droppers.contains(&short(item)) {
        return false;
    }
    let mut anchored = !is_struct && count >= 2;
    for (below, ty) in &fields {
        let (mut direct, mut shielded) = (BTreeSet::new(), BTreeSet::new());
        type_words(ty, false, &mut direct, &mut shielded);
        let selected = *below || !is_struct || direct.iter().any(|word| dependent.contains(word));
        if selected && !plain_data(ty, calm, shadow) {
            return false;
        }
        if is_struct && !below && anchor(ty, shadow) {
            anchored = true;
        }
    }
    anchored
}

/// The words of the field types of a struct or an enum that are not only behind a pointer.
/// `None`: Semilla cannot read the item, so each word counts.
fn direct_words(item: &Item) -> Option<BTreeSet<String>> {
    let (fields, _, _) = fields_of(item)?;
    let (mut direct, mut shielded) = (BTreeSet::new(), BTreeSet::new());
    for (_, ty) in &fields {
        type_words(ty, false, &mut direct, &mut shielded);
    }
    Some(direct)
}

impl<'a> Opaque<'a> {
    /// True if the token names an opaque type (or, with `hold`, a type of the hold set) for
    /// the reader `item`.
    fn type_hit(&self, item: &Item, tok: &Tok, hold: bool) -> bool {
        if hold && self.hold_names.contains(&tok.word) {
            return true;
        }
        if !self.type_names.contains(&tok.word) {
            return false;
        }
        let mut paths = self.types.iter().filter(|path| last_name(path) == tok.word).peekable();
        if paths.peek().is_none() {
            return true;
        }
        paths.any(|path| {
            let module = path.rsplit_once("::").map(|(module, _)| module).unwrap_or("");
            !self.modules.contains(module) || self.hit(item, tok, &Cause { text: String::new(), root: None, place: Place::Module(module.to_string()), form: Form::Path })
        })
    }

    pub(crate) fn new(before: &'a Workspace, after: &'a Workspace) -> Opaque<'a> {
        let masks = Shadow::of(before, after);
        let mut opaque = Opaque {
            sides: [before, after],
            hard: BTreeMap::new(),
            soft: BTreeMap::new(),
            types: BTreeSet::new(),
            type_names: BTreeSet::new(),
            hold: BTreeSet::new(),
            hold_names: BTreeSet::new(),
            heads: BTreeSet::new(),
            defined: BTreeSet::new(),
            globs: BTreeSet::new(),
            facts: Facts::default(),
            selected: Default::default(),
            exempt: BTreeSet::new(),
            node_text: BTreeMap::new(),
            roots: Default::default(),
            modules: BTreeSet::new(),
            glob_from: BTreeMap::new(),
            glob_pub: BTreeMap::new(),
            renames: BTreeSet::new(),
            shadow: masks.clone(),
            macros: BTreeSet::new(),
            macro_words: BTreeMap::new(),
            wide: RefCell::new(HashMap::new()),
            expanded: Default::default(),
            member_scope: BTreeMap::new(),
            traits: BTreeSet::new(),
            toks: RefCell::new(HashMap::new()),
            memo: RefCell::new(BTreeMap::new()),
            layouts: Default::default(),
            aliases: Default::default(),
            entries: Default::default(),
            type_uses: Default::default(),
            cfg_uses: Default::default(),
            covered: Default::default(),
        };
        let mut scan = Scan::new(before, after);
        let all = || [before, after].into_iter().flat_map(|ws| ws.items.values());
        // The names of the derive macros that the workspace defines.
        let own_derives: BTreeSet<String> = all().filter(|item| item.kind == ItemKind::Fn).flat_map(|item| item.attrs.iter()).filter_map(|attr| derive_macro(attr)).collect();
        let key = |item: &Item| item as *const Item as usize;

        // The modules, the types, the traits, and the macros.
        for item in all() {
            let path = plain_path(&item.path);
            opaque.modules.insert(segments(&path)[0].to_string());
            match item.kind {
                ItemKind::Mod => drop(opaque.modules.insert(path)),
                ItemKind::Struct | ItemKind::Enum => drop(opaque.defined.insert(path)),
                ItemKind::Trait => drop(opaque.traits.insert(path)),
                ItemKind::Macro if is_definition(&item.text) => {
                    opaque.macros.insert(short(item));
                    let (words, calls) = opaque.macro_words.entry(short(item)).or_default();
                    words.extend(idents(&item.text).map(str::to_string));
                    calls.extend(macro_calls(&item.text));
                }
                _ => {}
            }
        }
        // The root of a file of a different target (a test, an example) has no `mod` item.
        // A path above an item with no item of its own is such a root: it is a module.
        // The owner of the members of an `impl` is not a module.
        let mut paths: BTreeSet<String> = all().map(|item| plain_path(&item.path)).collect();
        for item in all().filter(|item| item.kind == ItemKind::Impl) {
            let last = segments(&item.path).last().copied().unwrap_or("");
            paths.insert(format!("{}::{}", plain_path(&module_of(item)), plain(last).trim_start_matches("impl ").trim()));
        }
        for path in &paths {
            let parts = segments(path);
            for count in 2..parts.len() {
                let prefix = parts[..count].join("::");
                if opaque.modules.contains(&prefix) {
                    continue;
                }
                if paths.contains(&prefix) {
                    break;
                }
                opaque.modules.insert(prefix);
            }
        }
        // The imports, the names of each kind, and the types with a `Drop` impl.
        let mut names_all: BTreeSet<String> = BTreeSet::new();
        let mut leaves_all: BTreeSet<String> = BTreeSet::new();
        let mut leaves_in: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut alias_names: BTreeSet<String> = BTreeSet::new();
        let mut alias_paths: BTreeSet<String> = BTreeSet::new();
        let mut alias_in: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        // The modules with a macro call in the place of an item.
        let mut calls_in: BTreeSet<String> = BTreeSet::new();
        let mut droppers: BTreeSet<String> = BTreeSet::new();
        let mut shapes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let shape = |item: &Item| words(&header(item)).into_iter().map(|word| if NUMERIC.contains(&word) { "_" } else { word }).collect::<Vec<_>>().join(" ");
        for item in all() {
            names_all.insert(short(item));
            if item.kind == ItemKind::Macro && !is_definition(&item.text) {
                calls_in.insert(opaque.module(&item.path));
            }
            match item.kind {
                ItemKind::Use | ItemKind::Other => {
                    let text: Vec<&str> = idents(&item.text).collect();
                    opaque.renames.extend(text.windows(2).filter(|pair| pair[0] == "as").map(|pair| pair[1].to_string()));
                    if item.kind == ItemKind::Use {
                        let module = opaque.module(&item.path);
                        let (leaves, _) = use_leaves(&item.text);
                        leaves_in.entry(module.clone()).or_default().extend(leaves.iter().cloned());
                        leaves_all.extend(leaves);
                        let sources = glob_sources(&item.text);
                        if !sources.is_empty() {
                            // A text that does not parse: not private (the safe side).
                            let private = syn::parse_str::<syn::ItemUse>(&item.text).is_ok_and(|parsed| matches!(parsed.vis, syn::Visibility::Inherited));
                            if !private {
                                opaque.glob_pub.entry(module.clone()).or_default().extend(sources.iter().cloned());
                            }
                            opaque.glob_from.entry(module).or_default().extend(sources);
                        }
                    }
                }
                ItemKind::Type => {
                    alias_names.insert(short(item));
                    alias_paths.insert(plain_path(&item.path));
                    alias_in.entry(short(item)).or_default().insert(opaque.module(&item.path));
                }
                ItemKind::Impl => {
                    let text = header(item);
                    if idents(&text).any(|word| word == "Drop") {
                        droppers.extend(idents(&text).map(str::to_string));
                    }
                    shapes.entry(shape(item)).or_default().insert(text);
                }
                _ => {}
            }
        }
        // The names that the workspace can give a different meaning.
        let shadow: BTreeSet<String> = names_all.iter().chain(&leaves_all).chain(&opaque.renames).cloned().collect();
        // The self type of each `impl`.
        let mut impls: HashMap<usize, (Option<Head>, ImplParts)> = HashMap::new();
        for item in all().filter(|item| item.kind == ItemKind::Impl) {
            let parts = impl_parts(item);
            let local = |name: &str| opaque.defined.iter().any(|path| last_name(path) == name) && !alias_names.contains(name);
            // A name with no path means an item of the module, an import of the module, or
            // a name of a glob import. An alias that the module cannot see cannot be the
            // self type. An import with the name can bring the alias, and a macro call can
            // make the alias: the safe side.
            let local_here = |name: &str| {
                let seen = opaque.visible(&opaque.module(&item.path));
                opaque.defined.iter().any(|path| last_name(path) == name)
                    && seen.is_some_and(|seen| !seen.iter().any(|module| calls_in.contains(module) || alias_in.get(name).is_some_and(|found| found.contains(module)) || leaves_in.get(module).is_some_and(|leaves| leaves.contains(name))))
            };
            let standard = |name: &str| STD_TYPES.contains(&name) && !shadow.contains(name);
            let head = parts.head.clone().filter(|name| parts.parsed && !parts.params.contains(name) && !WORDS.contains(&name.as_str()) && !opaque.renames.contains(name)).and_then(|name| match parts.qualified.as_deref() {
                None => {
                    let here = format!("{}::{name}", plain_path(&module_of(item)));
                    if (opaque.defined.contains(&here) && !alias_paths.contains(&here)) || local(&name) || local_here(&name) {
                        Some(Head::Local(name))
                    } else {
                        standard(&name).then_some(Head::Std(name))
                    }
                }
                Some("std" | "core" | "alloc") => (standard(&name) && !shadow.contains(parts.qualified.as_deref().unwrap_or(""))).then_some(Head::Std(name)),
                Some(first) if ["crate", "self", "super"].contains(&first) || opaque.modules.iter().any(|module| module.rsplit("::").next() == Some(first)) => local(&name).then_some(Head::Local(name)),
                // The first segment can only be a crate outside the workspace.
                Some(first) => (!shadow.contains(first)).then_some(Head::Foreign(name)),
            });
            // The scope of the members.
            let last = segments(&item.path).last().copied().unwrap_or("");
            let owner = format!("{}::{}", plain_path(&module_of(item)), plain(last).trim_start_matches("impl ").trim());
            let scope = match &head {
                Some(head) => (Place::Type(head.name().to_string()), Form::Member),
                None => (Place::Any, Form::Any),
            };
            let entry = opaque.member_scope.entry(owner).or_insert_with(|| scope.clone());
            if *entry != scope {
                *entry = (Place::Any, Form::Any);
            }
            impls.insert(key(item), (head, parts));
        }
        // The enums with no drop code and with a value in each configuration.
        let empty = BTreeSet::new();
        let mut calm: BTreeSet<String> = BTreeSet::new();
        let mut not_calm: BTreeSet<String> = BTreeSet::new();
        for item in all().filter(|item| matches!(item.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Type | ItemKind::Other | ItemKind::Trait)) {
            let _at = crate::cfgcond::at(&item.file);
            let good = item.kind == ItemKind::Enum
                && !droppers.contains(&short(item))
                && fields_of(item).is_some_and(|(fields, count, _)| count >= 1 && fields.iter().all(|(_, ty)| plain_data(ty, &empty, &shadow)))
                && syn::parse_str::<syn::ItemEnum>(&item.text).is_ok_and(|value| {
                    let plain_variant = |variant: &syn::Variant| !selects(&variant.attrs) && variant.fields.iter().all(|field| !selects(&field.attrs));
                    value.generics.params.is_empty() && value.variants.iter().any(plain_variant)
                });
            if good { &mut calm } else { &mut not_calm }.insert(short(item));
        }
        calm.retain(|name| !not_calm.contains(name) && !leaves_all.contains(name) && !opaque.renames.contains(name));

        // The names that stand for a type or a module in a signature.
        let mut alias: BTreeSet<String> = BTreeSet::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        // The owners of a field or a variant under `cfg`.
        let mut owners: BTreeSet<String> = BTreeSet::new();
        // The functions under `cfg` whose call can stay a node, with the cause for the case
        // that the test fails later.
        let mut candidates: Vec<(&Item, String, Option<String>)> = Vec::new();

        // An `impl` with a macro call in its header that is not a macro of the workspace
        // (batch 5q, part C3). The macro can give a different type in a different
        // configuration: the `impl` has the rule of an `impl` under `cfg`.
        let own_macros = opaque.macros.clone();
        let by_macro = |item: &Item| item.kind == ItemKind::Impl && macro_calls(&item.signature).iter().any(|name| !MACRO_FREE.contains(&name.as_str()) && !own_macros.contains(name));
        let rooted = |item: &Item| own_cfg(item, &masks) || by_macro(item);
        for (side, ws) in [before, after].into_iter().enumerate() {
            // The roots: each item with its own attribute, and each module file with an inner
            // attribute.
            let roots: Vec<&Item> = ws.items.values().filter(|item| rooted(item)).collect();
            let inner: Vec<String> = ws
                .inner_attrs
                .iter()
                .filter(|(_, (file, attrs))| {
                    let _at = crate::cfgcond::at(file);
                    selects_any(attrs, false, &|_| false)
                })
                .map(|(module, _)| module.replace('-', "_"))
                .collect();
            // The root above an item, itself included. The line test finds more than the real
            // parents (two items on one line): more items are opaque, not fewer.
            let above = |item: &Item| -> Option<String> {
                if rooted(item) {
                    return Some(item.path.clone());
                }
                let lines = |root: &&&Item| root.file == item.file && root.line_start <= item.line_start && item.line_end <= root.line_end && root.path != item.path;
                let module = |root: &&&Item| root.kind == ItemKind::Mod && item.path.starts_with(&format!("{}::", root.path));
                if let Some(root) = roots.iter().find(|root| module(root) || lines(root)) {
                    return Some(root.path.clone());
                }
                inner.iter().find(|module| item.path == **module || item.path.starts_with(&format!("{module}::"))).cloned()
            };
            // A reader is exempt only inside a module root, or if it is the root itself.
            let exempt_root = |item: &Item| -> Option<String> {
                let module = roots.iter().find(|root| root.kind == ItemKind::Mod && (root.path == item.path || item.path.starts_with(&format!("{}::", root.path)))).map(|root| root.path.clone());
                module.or_else(|| inner.iter().find(|module| item.path.starts_with(&format!("{module}::"))).cloned()).or_else(|| rooted(item).then(|| item.path.clone()))
            };
            // The items below covered conditions only (batch 5s, part C).
            let covered_attr = |attrs: &[String]| attrs.iter().any(|attr| matches!(attr_of(attr), Some(Attr::Covered)));
            let covered_at = |file: &str, attrs: &[String]| {
                let _at = crate::cfgcond::at(file);
                covered_attr(attrs)
            };
            let soft_roots: Vec<&Item> = ws.items.values().filter(|item| covered_at(&item.file, &item.attrs)).collect();
            let soft_inner: Vec<String> = ws.inner_attrs.iter().filter(|(_, (file, attrs))| covered_at(file, attrs)).map(|(module, _)| module.replace('-', "_")).collect();
            let mut hard_paths = BTreeSet::new();
            for item in ws.items.values() {
                if above(item).is_some() {
                    hard_paths.insert(plain_path(&item.path));
                    continue;
                }
                let inside = |root: &&&Item| {
                    let lines = root.file == item.file && root.line_start <= item.line_start && item.line_end <= root.line_end;
                    lines || (root.kind == ItemKind::Mod && item.path.starts_with(&format!("{}::", root.path)))
                };
                if soft_roots.iter().any(|root| inside(&root)) || soft_inner.iter().any(|module| item.path == *module || item.path.starts_with(&format!("{module}::"))) {
                    opaque.covered[side].insert(plain_path(&item.path));
                }
            }
            opaque.covered[side].retain(|path| !hard_paths.contains(path));
            for item in ws.items.values() {
                let _at = crate::cfgcond::at(&item.file);
                let name = short(item);
                let root = above(item);
                let is_root = rooted(item);
                let exempt = exempt_root(item);
                if let Some(exempt) = &exempt {
                    opaque.roots[side].insert(item.path.clone(), exempt.clone());
                }
                if item.kind == ItemKind::Fn && root.is_some() {
                    opaque.selected[side].insert(item.path.clone());
                }
                if item.kind == ItemKind::Impl && root.is_some() {
                    let text = if by_macro(item) { "an `impl` with a macro call in its header" } else { "an `impl` under `cfg`" };
                    opaque.facts.some |= by_macro(item);
                    opaque.facts.any |= by_macro(item);
                    let (head, parts) = &impls[&key(item)];
                    let own = members(item);
                    let Some(of_trait) = &parts.of_trait else {
                        // An inherent `impl`: a body gets a member only by its name, with a
                        // value or a path of the type.
                        let scope = head.as_ref().map(|head| (Place::Type(head.name().to_string()), Form::Member)).unwrap_or((Place::Any, Form::Any));
                        for (kind, member) in own {
                            opaque.add(&member, text, exempt.clone(), scope.clone());
                            if kind == "type" {
                                alias.insert(member);
                            }
                        }
                        continue;
                    };
                    // A trait `impl`. The names that it can give: its members, and the members
                    // of the trait. The coarse rule applies if the code can use the `impl`
                    // with no name (a drop, an operator, an associated type or constant, the
                    // type of a number with no suffix), or if Semilla does not know the trait.
                    // The trait fact (batch 5q): the configuration selects an `impl` of this
                    // trait for this type.
                    let fact_head = parts.head.clone().filter(|name| parts.parsed && !parts.params.contains(name) && !opaque.renames.contains(name) && !alias_names.contains(name));
                    opaque.facts.of_trait(of_trait, fact_head.as_deref().unwrap_or("*"));
                    opaque.facts.any |= !parts.parsed || of_trait == "?" || opaque.renames.contains(of_trait);
                    let mut names: BTreeSet<String> = own.iter().map(|(_, member)| member.clone()).collect();
                    let sibling = shapes.get(&shape(item)).is_some_and(|headers| headers.len() > 1);
                    let mut coarse = !parts.parsed || sibling || SILENT_TRAITS.contains(&of_trait.as_str()) || own.iter().any(|(kind, _)| kind != "fn");
                    let local: Vec<&Item> = all().filter(|other| other.kind == ItemKind::Trait && short(other) == *of_trait).collect();
                    for (kind, member) in local.iter().flat_map(|other| members(other)) {
                        coarse |= kind != "fn";
                        names.insert(member);
                    }
                    match trait_names(of_trait) {
                        Some(list) => names.extend(list.iter().map(|name| name.to_string())),
                        None => coarse |= local.is_empty(),
                    }
                    if sibling {
                        // A number with no suffix gets its type from the set of the impls.
                        opaque.add(of_trait, text, None, (Place::Any, Form::Any));
                        alias.insert(of_trait.clone());
                    }
                    match (coarse, head) {
                        // The foreign rule stops each body with a value of the type.
                        (_, Some(Head::Foreign(_))) => {}
                        (false, Some(head)) => {
                            for member in &names {
                                opaque.add(member, text, exempt.clone(), (Place::Type(head.name().to_string()), Form::Member));
                            }
                        }
                        (true, Some(Head::Local(head))) => {
                            opaque.opaque_name(head);
                            let scopes: Vec<(Place, Form)> = all().filter(|other| matches!(other.kind, ItemKind::Struct | ItemKind::Enum) && short(other) == *head).map(|other| opaque.scope(other)).collect();
                            for scope in scopes {
                                opaque.add(head, text, exempt.clone(), scope);
                            }
                        }
                        (true, Some(Head::Std(head))) => {
                            opaque.heads.insert(head.clone());
                            opaque.add(head, text, exempt.clone(), (Place::Any, Form::Any));
                        }
                        (_, None) => {
                            for member in &names {
                                opaque.add(member, text, exempt.clone(), (Place::Any, Form::Any));
                            }
                            alias.extend(own.iter().filter(|(kind, _)| kind == "type").map(|(_, member)| member.clone()));
                        }
                    }
                    continue;
                }
                // A trait that the configuration selects or fills (batch 5q). Its bounds are
                // trait facts: `trait Tr: Sup` gives `T: Sup` to each type parameter with
                // `T: Tr`, and `type It: Sup` gives the fact to the associated type. No
                // `impl` shows these facts. Semilla follows the bounds through the traits of
                // the workspace. A bound with a trait that is not of the workspace has
                // bounds that Semilla does not read.
                if item.kind == ItemKind::Trait && (root.is_some() || !scan.uses(item).config.is_empty()) {
                    opaque.facts.some = true;
                    opaque.facts.any |= macro_calls(&item.text).iter().any(|name| !MACRO_FREE.contains(&name.as_str()));
                    let mut todo = vec![item];
                    let mut seen: BTreeSet<String> = BTreeSet::new();
                    while let Some(next) = todo.pop() {
                        let Some(bounds) = trait_bounds(&next.text) else {
                            opaque.facts.any = true;
                            continue;
                        };
                        for bound in bounds {
                            let found: Vec<&Item> = all().filter(|other| other.kind == ItemKind::Trait && short(other) == bound).collect();
                            if found.is_empty() || opaque.renames.contains(&bound) {
                                opaque.facts.any = true;
                            } else if seen.insert(bound.clone()) {
                                opaque.facts.of_trait(&bound, "*");
                                todo.extend(found);
                            }
                        }
                    }
                }
                if item.kind == ItemKind::Macro && !is_definition(&item.text) {
                    // A macro call that makes items. With a `cfg` in the call or in the macro,
                    // the configuration selects the items. Semilla cannot see each name, so
                    // each body of the module gets no proof.
                    let found = scan.uses(item);
                    if is_root || !found.config.is_empty() {
                        opaque.facts.some = true;
                        opaque.facts.any = true;
                        opaque.globs.insert(plain_path(&module_of(item)));
                        let texts = scan.definitions(name.as_str()).into_iter().map(|item| item.text.as_str()).chain([item.text.as_str()]);
                        for made in texts.flat_map(made_names) {
                            opaque.add(&made, "a macro with `cfg` makes it", None, (Place::Any, Form::Any));
                            alias.insert(made);
                        }
                    }
                    continue;
                }
                // A `derive` in a `cfg_attr` adds trait impls (batch 5o, A5).
                for of_trait in derived(item, &masks) {
                    opaque.facts.of_trait(&of_trait, &name);
                    for member in trait_names(&of_trait).into_iter().flatten() {
                        opaque.add(member, "a `derive` under `cfg_attr`", None, (Place::Type(name.clone()), Form::Member));
                    }
                    // A derive macro of the workspace with the name of a known trait: Semilla
                    // does not know the code that it makes (batch 5p, C2).
                    if own_derives.contains(&of_trait) {
                        opaque.add(&name, "a `derive` of a workspace macro under `cfg_attr`", None, (Place::Any, Form::Any));
                        opaque.opaque_item(item);
                    }
                }
                if !is_root && const_default(item) {
                    // The default of a `const` parameter is a constant that no body shows. The
                    // allow list has only a literal. With a different default, the name is hard.
                    opaque.add(&name, "a default of a `const` parameter", None, (Place::Any, Form::Any));
                    opaque.opaque_item(item);
                    if item.kind == ItemKind::Type {
                        alias.insert(name.clone());
                    }
                }
                if !is_root {
                    // The source rule for an item with no attribute above it.
                    if root.is_none() && matches!(item.kind, ItemKind::Fn | ItemKind::Const | ItemKind::Static) {
                        let found = scan.uses(item);
                        let first = found.config.iter().chain(found.unseen.iter()).next().cloned();
                        match (item.kind, first) {
                            (ItemKind::Fn, Some(_)) => {
                                opaque.selected[side].insert(item.path.clone());
                                if let Some(text) = found.config.iter().next() {
                                    opaque.soft.insert(name.clone(), format!("`{name}` (`{}`)", text.replace('`', "")));
                                }
                            }
                            (_, Some(text)) => {
                                let scope = opaque.scope(item);
                                opaque.add(&name, &format!("`{}`", text.replace('`', "")), None, scope);
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                // A root.
                let by_cfg_attr = !item.attrs.iter().any(|attr| compact(attr).replace(' ', "").starts_with("#[cfg("));
                // The text names the attribute with its condition (batch 5s, C3): the first
                // attribute that selects code and that the checked configurations do not cover.
                let attr = item
                    .attrs
                    .iter()
                    .find(|attr| matches!(attr_of(attr), Some(Attr::Select) | Some(Attr::Derive(_))))
                    .or_else(|| item.attrs.iter().find(|attr| matches!(attr_of(attr), Some(Attr::Free))))
                    .map(|attr| format!("`{}`", compact(attr).replace('`', "").chars().take(80).collect::<String>()))
                    .unwrap_or_else(|| if by_cfg_attr { "`cfg_attr`".to_string() } else { "`cfg`".to_string() });
                let attr = attr.as_str();
                match item.kind {
                    ItemKind::Field | ItemKind::Variant => {
                        opaque.facts.fields = true;
                        let what = if item.kind == ItemKind::Field { "field" } else { "variant" };
                        // The owner: the struct, or the enum of the variant.
                        let owner = item.path.rsplit_once(if item.kind == ItemKind::Field { "." } else { "::" }).map(|(owner, _)| owner).unwrap_or("");
                        let is_type = |found: &&Item| matches!(found.kind, ItemKind::Struct | ItemKind::Enum);
                        let found = ws.items.get(owner).filter(is_type).or_else(|| owner.rsplit_once("::").and_then(|(parent, _)| ws.items.get(parent)).filter(is_type));
                        match found {
                            // The name selects the field or the variant only with a value of
                            // the type. The loop below decides between the hold set and the
                            // opaque types.
                            Some(found) => {
                                opaque.add(&name, &format!("a {what} under {attr}"), exempt.clone(), (Place::Type(short(found)), Form::Any));
                                owners.insert(plain_path(&found.path));
                            }
                            None => {
                                opaque.add(&name, &format!("a {what} under {attr}"), exempt.clone(), (Place::Any, Form::Any));
                                for part in segments(owner).iter().rev().take(2) {
                                    let part = plain(part).to_string();
                                    opaque.opaque_name(&part);
                                    opaque.add(&part, &format!("a {what} under {attr}"), None, (Place::Any, Form::Any));
                                }
                            }
                        }
                    }
                    ItemKind::Use => {
                        let module = opaque.module(&item.path);
                        let (mut leaves, mut glob) = use_leaves(&item.text);
                        opaque.facts.some = true;
                        opaque.facts.any |= glob;
                        // A trait with a new name (`use tr::Sup as S`, `use tr::Sup as _`)
                        // gives its methods too (batch 5q).
                        let sources = use_sources(&item.text);
                        leaves.extend(sources.iter().filter(|source| !leaves.contains(source)).cloned().collect::<Vec<_>>());
                        for leaf in leaves {
                            if sources.contains(&leaf) {
                                let local: Vec<String> = all().filter(|other| other.kind == ItemKind::Trait && short(other) == leaf).flat_map(|other| members(other)).map(|(_, member)| member).collect();
                                let known = trait_names(&leaf).into_iter().flatten().map(|member| member.to_string());
                                for member in local.into_iter().chain(known) {
                                    opaque.add(&member, &format!("a `use` of a trait under {attr}"), exempt.clone(), (Place::Module(module.clone()), Form::Member));
                                }
                                glob |= leaf.starts_with(char::is_uppercase) && !all().any(|other| other.kind == ItemKind::Trait && short(other) == leaf) && trait_names(&leaf).is_none() && !STD_TYPES.contains(&leaf.as_str());
                                continue;
                            }
                            opaque.add(&leaf, &format!("a `use` under {attr}"), exempt.clone(), (Place::Module(module.clone()), Form::Path));
                            // A trait gives its methods to each body of the module.
                            let local: Vec<String> = all().filter(|other| other.kind == ItemKind::Trait && short(other) == leaf).flat_map(|other| members(other)).map(|(_, member)| member).collect();
                            let known = trait_names(&leaf).into_iter().flatten().map(|member| member.to_string());
                            for member in local.into_iter().chain(known) {
                                opaque.add(&member, &format!("a `use` of a trait under {attr}"), exempt.clone(), (Place::Module(module.clone()), Form::Member));
                            }
                            // A name that can be a trait with methods that Semilla cannot list.
                            glob |= leaf.starts_with(char::is_uppercase) && !names_all.contains(&leaf) && trait_names(&leaf).is_none() && !STD_TYPES.contains(&leaf.as_str());
                            alias.insert(leaf);
                        }
                        if glob {
                            opaque.add(last_name(&module), &format!("a glob `use` under {attr}"), None, (Place::Any, Form::Prefix));
                            opaque.globs.insert(module);
                        }
                    }
                    ItemKind::Other => {
                        opaque.facts.some = true;
                        opaque.facts.any = true;
                        // An item of a kind with no rule: each word of its signature.
                        for word in idents(&item.signature).filter(|word| !WORDS.contains(word)) {
                            opaque.add(word, &format!("under {attr}"), None, (Place::Any, Form::Any));
                            alias.insert(word.to_string());
                        }
                    }
                    // A macro: the source rule reads its definition at each call.
                    ItemKind::Macro => {}
                    _ => {
                        *counts.entry(name.clone()).or_default() += usize::from(side == 1 || !after.items.contains_key(&item.path));
                        let scope = opaque.scope(item);
                        // A call of a function under `cfg` can stay a node (batch 5o, A3) if
                        // each definition of the path has the same header, and if the name
                        // can mean no other item: no import of the name, no glob import, and
                        // no function of the prelude. Each definition has only attributes
                        // of the allow list: an attribute macro can change the signature.
                        let path = plain_path(&item.path);
                        let module = opaque.module(&item.path);
                        let node = item.kind == ItemKind::Fn
                            && matches!(scope, (Place::Module(_), Form::Path))
                            && !PRELUDE_FNS.contains(&name.as_str())
                            && !idents(&item.signature).any(|word| word == "const")
                            && !opaque.glob_from.contains_key(&module)
                            && !leaves_in.get(&module).is_some_and(|leaves| leaves.contains(&name))
                            && all().filter(|other| plain_path(&other.path) == path).all(|other| other.kind == ItemKind::Fn && header(other) == header(item) && plain_attrs(other));
                        if node {
                            candidates.push((item, format!("under {attr}"), exempt.clone()));
                            opaque.exempt.insert(path);
                        } else {
                            opaque.add(&name, &format!("under {attr}"), exempt.clone(), scope);
                        }
                        if matches!(item.kind, ItemKind::Type | ItemKind::Mod) {
                            alias.insert(name.clone());
                        }
                        if matches!(item.kind, ItemKind::Type | ItemKind::Struct | ItemKind::Enum | ItemKind::Trait) {
                            opaque.opaque_item(item);
                        }
                        // A `cfg_attr` with a content that Semilla does not know: it can add
                        // an `impl` of each trait (batch 5q).
                        if matches!(item.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Type) && by_cfg_attr {
                            opaque.facts.some = true;
                            opaque.facts.any = true;
                        }
                        if item.kind == ItemKind::Trait {
                            opaque.facts.of_trait(&name, "*");
                            // A type that has the trait gives the members with no trait name.
                            for (kind, member) in members(item) {
                                opaque.add(&member, &format!("a trait under {attr}"), exempt.clone(), (Place::Any, Form::Any));
                                if kind == "type" {
                                    alias.insert(member);
                                }
                            }
                        }
                    }
                }
            }
        }
        // "2 definitions under `cfg`" for a name with more than one root.
        for (item, text, _) in candidates.iter_mut() {
            if let Some(count) = counts.get(&short(item)).filter(|count| **count > 1) {
                *text = format!("{count} definitions, one {text}");
            }
            opaque.node_text.insert(plain_path(&item.path), text.clone());
        }
        for (name, count) in counts {
            if count > 1 {
                for cause in opaque.hard.get_mut(&name).into_iter().flatten().filter(|cause| cause.text.starts_with("under ")) {
                    cause.text = format!("{count} definitions, one {}", cause.text);
                }
            }
        }
        opaque.close(&mut alias, &owners, &calm, &shadow, &droppers);
        // A call node is not sound if the header of the function can have a different
        // meaning in a different configuration, or if its module has names that Semilla
        // cannot list. Then the name is hard.
        loop {
            let failed: Vec<usize> = candidates
                .iter()
                .enumerate()
                .filter(|(_, (item, _, _))| {
                    let module = opaque.module(&item.path);
                    opaque.exempt.contains(&plain_path(&item.path))
                        && (opaque.globs.iter().any(|glob| opaque.sees(&module, glob)) || opaque.wide(&item.signature).iter().any(|tok| tok.word != short(item) && (alias.contains(&tok.word) || opaque.reads(item, tok) || opaque.type_hit(item, tok, true))))
                })
                .map(|(index, _)| index)
                .collect();
            if failed.is_empty() {
                break;
            }
            for index in failed {
                let (item, text, exempt) = &candidates[index];
                opaque.exempt.remove(&plain_path(&item.path));
                let scope = opaque.scope(item);
                opaque.add(&short(item), text, exempt.clone(), scope);
            }
            opaque.close(&mut alias, &owners, &calm, &shadow, &droppers);
        }
        opaque
    }
}

impl<'a> Opaque<'a> {
    /// Adds the names that get their meaning from a hard name: an import of it, an item with
    /// an alias of it in the signature, a type with a field of an opaque type. The loop
    /// stops when no name is new.
    fn close(&mut self, alias: &mut BTreeSet<String>, owners: &BTreeSet<String>, calm: &BTreeSet<String>, shadow: &BTreeSet<String>, droppers: &BTreeSet<String>) {
        let [before, after] = self.sides;
        // The values of the associated constants and the associated types, with their tokens.
        let mut values: HashMap<usize, Vec<(String, bool, Vec<Tok>)>> = HashMap::new();
        for item in [before, after].into_iter().flat_map(|ws| ws.items.values()) {
            let under_cfg = self.roots.iter().any(|side| side.contains_key(&item.path));
            if matches!(item.kind, ItemKind::Impl | ItemKind::Trait) && !under_cfg {
                let found: Vec<(String, bool, Vec<Tok>)> = member_values(item).into_iter().map(|(name, is_type, text)| (name, is_type, self.widen(&tokens(&text, &self.macros), &text))).collect();
                if !found.is_empty() {
                    values.insert(item as *const Item as usize, found);
                }
            }
        }
        loop {
            let mut new: Vec<(String, String, bool, (Place, Form))> = Vec::new();
            let mut new_types: Vec<&Item> = Vec::new();
            let mut new_names: Vec<String> = Vec::new();
            let mut new_hold: Vec<&Item> = Vec::new();
            let mut new_globs: Vec<String> = Vec::new();
            for item in [before, after].into_iter().flat_map(|ws| ws.items.values()) {
                let name = short(item);
                let in_alias = |word: &str| alias.contains(word);
                // The value of an associated constant or an associated type (batch 5p): a
                // hard name in the value makes the member hard. A body that reads the member
                // has no token of the value.
                if let Some(found) = values.get(&(item as *const Item as usize)) {
                    let scope = if item.kind == ItemKind::Trait {
                        (Place::Any, Form::Member)
                    } else {
                        let last = segments(&item.path).last().copied().unwrap_or("");
                        let owner = format!("{}::{}", plain_path(&module_of(item)), plain(last).trim_start_matches("impl ").trim());
                        self.member_scope.get(&owner).cloned().unwrap_or((Place::Any, Form::Any))
                    };
                    for (member, is_type, toks) in found {
                        if self.known(member, &scope) {
                            continue;
                        }
                        // For a type, only a name that a macro call can give counts here: the
                        // compiler gives the type to each body that uses it.
                        if let Some(source) = toks.iter().find(|tok| tok.word != *member && (!*is_type || tok.via) && self.reads(item, tok)).map(|tok| tok.word.clone()) {
                            new.push((member.clone(), format!("its value uses `{source}`"), *is_type, scope.clone()));
                        }
                    }
                }
                match item.kind {
                    ItemKind::Use if !own_cfg(item, &self.shadow) => {
                        if let Some(source) = self.first_hit(item, &item.text, &|_| true) {
                            let module = self.module(&item.path);
                            let scope = (Place::Module(module.clone()), Form::Path);
                            let (leaves, glob) = use_leaves(&item.text);
                            new.extend(leaves.into_iter().filter(|leaf| !self.known(leaf, &scope)).map(|leaf| (leaf, format!("a `use` of `{source}`"), true, scope.clone())));
                            if glob && !self.globs.contains(&module) {
                                // A path `module::name` can mean a name of the glob import.
                                new.push((last_name(&module).to_string(), format!("a glob `use` of `{source}`"), false, (Place::Any, Form::Prefix)));
                                new_globs.push(module);
                            }
                        }
                    }
                    ItemKind::Fn | ItemKind::Const | ItemKind::Static | ItemKind::Type | ItemKind::Trait => {
                        let scope = self.scope(item);
                        if self.known(&name, &scope) {
                            continue;
                        }
                        let source = self.first_hit(item, &item.signature, &in_alias).or_else(|| {
                            // A type that the signature hides (`impl Trait`): the text of the
                            // item gives the type. An alias with one definition: only a hard
                            // name in its target makes it hard. The compiler gives the target
                            // type to each body that uses the alias, so a target with a part
                            // under `cfg` gets the type rule in that body (batch 5o).
                            let is_alias = item.kind == ItemKind::Type;
                            let hidden = is_alias || idents(&item.signature).any(|word| word == "impl");
                            // The body of a function has the list of the driver for its macro
                            // calls: a macro with no known expansion does not count here.
                            let is_fn = item.kind == ItemKind::Fn;
                            hidden
                                .then(|| self.wide(&item.text).iter().find(|tok| !(is_fn && tok.wild()) && (self.reads(item, tok) || (!is_alias && tok.word != name && self.type_hit(item, tok, true)))).map(|tok| tok.word.clone()))
                                .flatten()
                        });
                        if let Some(source) = source {
                            new.push((name.clone(), format!("its signature uses `{source}`"), item.kind == ItemKind::Type, scope));
                            continue;
                        }
                        // The value of a constant or a static (batch 5p). A body that reads
                        // the constant gets its value, and the body has no token of the
                        // initializer. A hard name in the initializer makes the constant
                        // hard. The names that a macro call can give count.
                        if matches!(item.kind, ItemKind::Const | ItemKind::Static) {
                            let found = self.wide(&item.text).iter().find(|tok| tok.word != name && self.reads(item, tok)).map(|tok| tok.word.clone());
                            if let Some(source) = found {
                                new.push((name.clone(), format!("its value uses `{source}`"), false, scope));
                            }
                        }
                    }
                    ItemKind::Struct | ItemKind::Enum => {
                        let path = plain_path(&item.path);
                        if self.types.contains(&path) {
                            continue;
                        }
                        // A field of a type that the configuration selects: the layout and
                        // the drop code are opaque. A name behind a pointer does not count.
                        let direct = direct_words(item);
                        // A name that a macro call can give has no known place in the
                        // declaration: the type is then opaque (batch 5p).
                        let wide = self.wide(&item.text);
                        let through: Option<String> = wide.iter().find(|tok| tok.via && (self.reads(item, tok) || self.type_hit(item, tok, true))).map(|tok| tok.word.clone());
                        let mut dependent: BTreeSet<String> = wide
                            .iter()
                            .filter(|tok| !tok.via && tok.word != name && direct.as_ref().is_none_or(|direct| direct.contains(&tok.word)))
                            .filter(|tok| (in_alias(&tok.word) && self.reads(item, tok)) || self.type_hit(item, tok, true))
                            .map(|tok| tok.word.clone())
                            .collect();
                        dependent.extend(through.clone());
                        if dependent.is_empty() && !owners.contains(&path) {
                            continue;
                        }
                        if through.is_none() && holdable(item, &dependent, calm, shadow, droppers) {
                            if !self.hold.contains(&path) {
                                new_hold.push(item);
                            }
                        } else {
                            let source = dependent.iter().next().cloned().unwrap_or_else(|| "cfg".to_string());
                            new_types.push(item);
                            new.push((name.clone(), format!("its declaration uses `{source}`"), false, self.scope(item)));
                        }
                    }
                    // An `impl` under `cfg` has the rule of its kind: its members, or the
                    // coarse rule. Its header adds no input.
                    ItemKind::Impl if self.roots.iter().any(|side| side.contains_key(&item.path)) => {}
                    ItemKind::Impl => {
                        if let Some(source) = self.first_hit(item, &item.signature, &in_alias).or_else(|| self.unpinned(item)) {
                            let last = segments(&item.path).last().copied().unwrap_or("");
                            let owner = format!("{}::{}", plain_path(&module_of(item)), plain(last).trim_start_matches("impl ").trim());
                            let scope = self.member_scope.get(&owner).cloned().unwrap_or((Place::Any, Form::Any));
                            let parts = impl_parts(item);
                            let local = parts.head.clone().filter(|head| scope.0 == Place::Type(head.clone()) && self.defined.iter().any(|path| last_name(path) == head));
                            match local {
                                // An `impl` for a type of the workspace: the type is opaque.
                                Some(head) => {
                                    if !self.type_names.contains(&head) || self.defined.iter().any(|path| last_name(path) == head && !self.types.contains(path)) {
                                        new_names.push(head.clone());
                                        for other in [before, after].into_iter().flat_map(|ws| ws.items.values()).filter(|other| matches!(other.kind, ItemKind::Struct | ItemKind::Enum) && short(other) == head) {
                                            new.push((head.clone(), format!("an `impl` uses `{source}`"), false, self.scope(other)));
                                        }
                                    }
                                }
                                None => new.extend(members(item).into_iter().filter(|(_, member)| !self.known(member, &scope)).map(|(kind, member)| (member, format!("an `impl` uses `{source}`"), kind == "type", scope.clone()))),
                            }
                        }
                    }
                    _ => {}
                }
            }
            if new.is_empty() && new_hold.is_empty() && new_types.is_empty() && new_names.is_empty() && new_globs.is_empty() {
                break;
            }
            for item in new_hold {
                self.hold.insert(plain_path(&item.path));
                self.hold_names.insert(short(item));
            }
            for item in new_types {
                self.opaque_item(item);
            }
            for name in new_names {
                self.opaque_name(&name);
            }
            self.globs.extend(new_globs);
            for (name, text, is_alias, scope) in new {
                self.add(&name, &text, None, scope);
                if is_alias {
                    alias.insert(name);
                }
            }
        }
        // A type in the two sets is opaque.
        let both: Vec<String> = self.hold.intersection(&self.types).cloned().collect();
        for path in both {
            self.hold.remove(&path);
        }
        self.hold_names = self.hold.iter().map(|path| last_name(path).to_string()).collect();
    }

    /// A name in the header of an `impl` that a glob import under `cfg` can change: the
    /// module of the `impl` can see such an import, and the name is not an item of the
    /// module. Then the self type or the trait of the `impl` can be a different item in a
    /// different configuration.
    fn unpinned(&self, item: &Item) -> Option<String> {
        let module = self.module(&item.path);
        if !self.globs.iter().any(|glob| self.sees(&module, glob)) {
            return None;
        }
        let params = impl_parts(item).params;
        let own = |word: &str| self.sides.iter().any(|ws| ws.items.contains_key(&format!("{module}::{word}")));
        self.wide(&item.signature).iter().map(|tok| tok.word.clone()).find(|word| !WORDS.contains(&word.as_str()) && word != "Self" && !params.contains(word) && !own(word))
    }

    /// True if the item gets no proof by its text or its place: it names a hard name, it can
    /// see a module with names that Semilla cannot list, or it is a method of an opaque
    /// type.
    fn blocked(&self, side: usize, item: &Item) -> bool {
        if !self.hard_names(side, item).is_empty() {
            return true;
        }
        let path = plain_path(&item.path);
        let module = self.module(&item.path);
        if self.globs.iter().any(|glob| path.starts_with(&format!("{glob}::")) || self.sees(&module, glob)) {
            return true;
        }
        let parts = segments(&item.path);
        let owner = parts.len().checked_sub(2).map(|at| plain(parts[at])).unwrap_or("");
        let owner = owner.strip_prefix('<').map(|rest| rest.split(" as ").next().unwrap_or(rest)).unwrap_or(owner);
        let owner = last_name(owner);
        let full = format!("{module}::{owner}");
        item.kind == ItemKind::Fn && (self.types.contains(&full) || (!self.defined.contains(&full) && self.type_names.contains(owner)))
    }

    /// True if an entry of the driver stops the proof of the reader (a side and an item
    /// path). The entries are the `adts` of a body record: `?`, `alias P`, `foreign P`,
    /// `cfg P` (an item that the configuration selects), `std P`, `touch P` (the body reads
    /// a part of a value), and `P` (a type of the workspace).
    fn stops(&self, reader: Option<(usize, &str)>, types: &[String]) -> bool {
        types.iter().any(|entry| {
            if entry == "?" || entry.starts_with("alias ") || entry.starts_with("foreign ") {
                return true;
            }
            if let Some(path) = entry.strip_prefix("cfg ") {
                return self.cfg_stops(reader, path);
            }
            if let Some(path) = entry.strip_prefix("std ") {
                return self.heads.contains(last_name(path));
            }
            match entry.strip_prefix("touch ") {
                Some(path) => self.opaque_type(path, true),
                None => self.opaque_type(entry, false),
            }
        })
    }

    /// True if the use of the item `path` under `cfg` stops the proof of the reader.
    fn cfg_stops(&self, reader: Option<(usize, &str)>, path: &str) -> bool {
        let path = path.split('<').next().unwrap_or(path).trim();
        let Some((side, reader)) = reader else { return true };
        if self.inside(side, reader, path) || self.covered[side].contains(path) {
            return false;
        }
        // A call of a function with the same header in each configuration stays a node. The
        // reader is in the module of the function, so the name means the function.
        !(self.exempt.contains(path) && path.rsplit_once("::").is_some_and(|(module, _)| module == self.module(reader)))
    }

    /// True if the reader is inside the root of the item `path` under `cfg`: the reader exists
    /// only together with the item.
    fn inside(&self, side: usize, reader: &str, path: &str) -> bool {
        self.roots[side].get(reader).map(|root| plain_path(root)).is_some_and(|root| path == root || path.starts_with(&format!("{root}::")))
    }

    /// True if a type of the list is opaque, for a reader with no known place.
    #[cfg(test)]
    pub(crate) fn layout(&self, types: &[String]) -> bool {
        self.stops(None, types)
    }

    /// Keeps the types and the items under `cfg` of a body, by the driver. Call this for each
    /// body of the two sides before the first call of `callee`: a closure is a part of its
    /// function.
    pub(crate) fn note(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) {
        let side = usize::from(after);
        let Some(path) = crate::checks::find_item_by(name, span, self.sides[side]) else { return };
        let mut entries = self.entries.borrow_mut();
        let names = entries[side].entry(path.clone()).or_default();
        for entry in types {
            let (kind, rest) = match entry.split_once(' ') {
                Some((kind, rest)) if ["alias", "foreign", "cfg", "std", "touch"].contains(&kind) => (kind, rest),
                _ => ("", entry.as_str()),
            };
            names.insert(last_name(rest).to_string());
            // An opaque type of the workspace or of the standard library: a new use gets a
            // text for the spotlight.
            if ["", "touch", "std"].contains(&kind) && entry != "?" && self.stops(Some((side, &path)), std::slice::from_ref(entry)) {
                let cause = if kind == "std" { "a trait `impl` under `cfg` for this type" } else { "a type with a part under `cfg`" };
                self.type_uses.borrow_mut()[side].entry(path.clone()).or_default().insert(format!("`{}` ({cause})", last_name(rest)));
            }
            if kind == "cfg" {
                self.cfg_uses.borrow_mut()[side].entry(path.clone()).or_default().insert(rest.split('<').next().unwrap_or(rest).trim().to_string());
            }
        }
    }

    /// True if no normal form can look into the body `name` (a MIR name with its span): the
    /// body has no item in the syntax tree, the configuration selects the item, the item fails
    /// the source rule, or the item gets no proof itself.
    pub(crate) fn callee(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) -> bool {
        let side = usize::from(after);
        let ws = self.sides[side];
        let path = crate::checks::find_item_by(name, span, ws);
        if self.stops(path.as_deref().map(|path| (side, path)), types) {
            if let Some(path) = &path {
                let found = types.iter().filter_map(|entry| entry.strip_prefix("alias ")).map(str::to_string);
                self.aliases.borrow_mut()[side].entry(path.clone()).or_default().extend(found);
            }
            self.layouts.borrow_mut()[side].extend(path);
            return true;
        }
        let Some(path) = path else { return true };
        if let Some(known) = self.memo.borrow().get(&(after, path.clone())) {
            return *known;
        }
        let result = self.selected[side].contains(&path) || ws.items.get(&path).is_none_or(|item| self.blocked(side, item));
        self.memo.borrow_mut().insert((after, path), result);
        result
    }

    /// The text of the cause that makes `name` hard for the reader.
    fn cause_text(&self, side: usize, item: &Item, name: &str) -> Option<String> {
        let none = Vec::new();
        let causes = self.hard.get(name).unwrap_or(&none);
        let by_text = self.toks(&item.text).iter().filter(|tok| tok.word == name).find_map(|tok| causes.iter().find(|cause| self.hit(item, tok, cause)).map(|cause| cause.text.clone()));
        if by_text.is_some() {
            return by_text;
        }
        // The name is only in the expanded code: the text has the macro call. A call with a
        // rival has the message of the driver.
        let expanded = self.expanded.borrow();
        expanded[side].get(&item.path)?.keys().filter(|found| found.word == name).find_map(|found| {
            let tok = found.tok();
            let text = match causes.iter().find(|cause| self.hit(item, &tok, cause)) {
                Some(cause) => cause.text.clone(),
                None => self.rival(found)?.to_string(),
            };
            Some(match &found.mac {
                Some(mac) => format!("{text}; the macro `{mac}!` puts the name into the code"),
                None => text,
            })
        })
    }

    /// Keeps the names of the expanded code of a body, by the driver (batch 5p). Call this
    /// for each body of the two sides before the first call of `callee`.
    pub(crate) fn names(&self, after: bool, name: &str, span: Option<&(String, usize)>, names: &[crate::ExpandedName]) {
        let side = usize::from(after);
        let Some(path) = crate::checks::find_item_by(name, span, self.sides[side]) else { return };
        let mut expanded = self.expanded.borrow_mut();
        let found = expanded[side].entry(path).or_default();
        let mut new = false;
        for name in names {
            let key = Expanded { word: name.word.clone(), member: name.member, qual: name.qual.clone(), prefix: name.prefix, mac: name.mac.clone(), line: name.line, rival: name.rival.clone() };
            let count = found.entry(key).or_default();
            if name.count > *count {
                *count = name.count;
                new = true;
            }
        }
        // An answer of `callee` from the time before the new names is not safe.
        if new {
            self.memo.borrow_mut().clear();
        }
    }

    /// Stops the proof of each body with a text edit that names a hard name or uses an opaque
    /// type. The same MIR in the checked configuration is not a proof: the edit can change
    /// the code of a different configuration (for example the place of a drop). A new use of
    /// a hard name, a soft name, or an item under `cfg` goes into the uses of the body, for
    /// the spotlight. A use that the old text has too gives no spotlight (batch 5o, A4).
    pub(crate) fn apply(&self, bodies: &mut [BodyComparison], renames: &BTreeMap<String, String>) {
        let [before, after] = self.sides;
        let old_of: BTreeMap<&String, &String> = renames.iter().map(|(old, new)| (new, old)).collect();
        // The names of the rename map: a text with only these changes has no edit.
        let pairs = rename_pairs(renames);
        let layouts = self.layouts.borrow();
        for body in bodies.iter_mut() {
            if !matches!(body.status, BodyStatus::Equal { .. } | BodyStatus::Different { .. }) {
                continue;
            }
            let Some(path) = crate::checks::find_item_by(&body.name, body.span.as_ref(), after) else { continue };
            let Some(new) = after.items.get(&path) else { continue };
            let old = before.items.get(old_of.get(&path).copied().unwrap_or(&path));
            if old.is_some_and(|old| old.text == new.text || same_text(&old.text, &new.text, &pairs)) {
                continue;
            }
            let new_names = self.hard_names(1, new);
            let old_names = old.map(|old| self.hard_names(0, old)).unwrap_or_default();
            let layout = layouts[1].contains(&path) || old.is_some_and(|old| layouts[0].contains(&old.path));
            let stop = layout || self.blocked(1, new) || old.is_some_and(|old| self.blocked(0, old));
            if stop && matches!(body.status, BodyStatus::Equal { .. }) {
                body.status = BodyStatus::Different { cfg_changed: true };
                // The cause, for the note of a fact with no spotlight (batch 5u): each hard
                // name of the two texts. A stop with no hard name has a general cause.
                let mut causes: Vec<String> = Vec::new();
                for (side, item, names) in [(1, Some(new), &new_names), (0, old, &old_names)] {
                    let Some(item) = item else { continue };
                    for name in names.text.keys().chain(names.expanded.keys()) {
                        let text = self.cause_text(side, item, name).map(|text| format!("`{name}` ({text})"));
                        causes.extend(text.filter(|text| !causes.contains(text)));
                    }
                }
                if causes.is_empty() {
                    causes.push("code that a `cfg` attribute can change".to_string());
                }
                body.stopped = causes;
            }
            if matches!(body.status, BodyStatus::Equal { .. }) {
                continue;
            }
            // The uses that the new text adds.
            let count = |item: &Item, name: &str| idents(&item.text).filter(|word| *word == name).count();
            let mut uses: Vec<String> = Vec::new();
            // The hard names in the changed tokens of the new text: a use that the edit puts
            // in the place of an old use has the same count. A token is changed if it is not
            // in a longest common sequence of the old tokens and the new tokens (batch 5p).
            let new_toks = self.toks(&new.text);
            let old_toks = old.map(|old| self.toks(&old.text));
            let same = |a: &Tok, b: &Tok| a.word == b.word && a.member == b.member && a.qual == b.qual && a.prefix == b.prefix;
            let changed: Vec<bool> = match &old_toks {
                Some(old_toks) => token_diff(old_toks, &new_toks, same).1,
                None => vec![true; new_toks.len()],
            };
            let changed_toks = || new_toks.iter().zip(&changed).filter(|(_, changed)| **changed).map(|(tok, _)| tok);
            let touched: BTreeSet<&str> = changed_toks().filter(|tok| self.reads(new, tok)).map(|tok| tok.word.as_str()).collect();
            for (name, new_count) in &new_names.text {
                if *new_count > old_names.text.get(name).copied().unwrap_or(0) || touched.contains(name.as_str()) {
                    uses.extend(self.cause_text(1, new, name).map(|text| format!("`{name}` ({text})")));
                }
            }
            // The hard names of the expanded code (batch 5p). A name from an expansion is a
            // part of its macro call: it is changed code if a token of a call of that macro is
            // changed (also a token that the edit removes), or if the new expanded code has
            // the name more times. `None`: each call is changed.
            let changed_macros: Option<BTreeSet<String>> = match (old.and_then(|old| leaves(&old.text)), leaves(&new.text)) {
                (Some(old_leaves), Some(new_leaves)) => {
                    let (old_changed, new_changed) = token_diff(&old_leaves, &new_leaves, |a, b| a.0 == b.0);
                    let of = |leaves: &[(String, Option<String>)], changed: &[bool]| -> Vec<String> { leaves.iter().zip(changed).filter(|(_, changed)| **changed).filter_map(|(leaf, _)| leaf.1.clone()).collect() };
                    Some(of(&old_leaves, &old_changed).into_iter().chain(of(&new_leaves, &new_changed)).collect())
                }
                _ => None,
            };
            let changed_words: BTreeSet<&str> = changed_toks().map(|tok| tok.word.as_str()).collect();
            for (name, (new_count, macros)) in &new_names.expanded {
                if uses.iter().any(|text| text.starts_with(&format!("`{name}` ("))) {
                    continue;
                }
                let more = *new_count > old_names.expanded.get(name).map(|(count, _)| *count).unwrap_or(0);
                let in_change = macros.iter().any(|mac| match mac {
                    Some(mac) => changed_macros.as_ref().is_none_or(|changed| mac == "?" || changed.contains(mac)),
                    None => changed_words.contains(name.as_str()),
                });
                if more || in_change {
                    uses.extend(self.cause_text(1, new, name).map(|text| format!("`{name}` ({text})")));
                }
            }
            let named = |name: &str| new_names.text.contains_key(name) || new_names.expanded.contains_key(name);
            for (name, text) in &self.soft {
                if !named(name) && count(new, name) > old.map(|old| count(old, name)).unwrap_or(0) && short(new) != *name {
                    uses.push(text.clone());
                }
            }
            // A new use of an item that the configuration selects, by the compiler.
            let cfg_uses = self.cfg_uses.borrow();
            let old_cfg = old.and_then(|old| cfg_uses[0].get(&old.path));
            for item in cfg_uses[1].get(&path).into_iter().flatten() {
                let name = last_name(item);
                if !old_cfg.is_some_and(|old| old.contains(item)) && !self.inside(1, &path, item) && !self.covered[1].contains(item) && !uses.iter().any(|text| text.starts_with(&format!("`{name}` ("))) {
                    uses.push(format!("`{name}` ({})", self.node_text.get(item).map(String::as_str).unwrap_or("under `cfg`")));
                }
            }
            // A new use of an opaque type, by the compiler.
            let type_uses = self.type_uses.borrow();
            let old_types = old.and_then(|old| type_uses[0].get(&old.path));
            for text in type_uses[1].get(&path).into_iter().flatten() {
                let start = text.split(" (").next().unwrap_or(text);
                if !old_types.is_some_and(|old| old.contains(text)) && !uses.iter().any(|known| known.starts_with(&format!("{start} ("))) {
                    uses.push(text.clone());
                }
            }
            // A new type alias of a crate outside the workspace.
            let aliases = self.aliases.borrow();
            let old_aliases = old.and_then(|old| aliases[0].get(&old.path));
            for alias in aliases[1].get(&path).into_iter().flatten() {
                if !old_aliases.is_some_and(|old| old.contains(alias)) {
                    uses.push(format!("`{alias}` (a type alias of a different crate)"));
                }
            }
            for text in uses {
                if !body.config.contains(&text) {
                    body.config.push(text);
                }
            }
        }
    }
}

impl crate::OpaqueInputs for Opaque<'_> {
    fn note(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) {
        Opaque::note(self, after, name, span, types)
    }

    fn names(&self, after: bool, name: &str, span: Option<&(String, usize)>, names: &[crate::ExpandedName]) {
        Opaque::names(self, after, name, span, names)
    }

    fn callee(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) -> bool {
        Opaque::callee(self, after, name, span, types)
    }

    fn layout(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) -> bool {
        let side = usize::from(after);
        let path = crate::checks::find_item_by(name, span, self.sides[side]);
        self.stops(path.as_deref().map(|path| (side, path)), types)
    }
}
