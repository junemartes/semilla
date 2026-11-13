//! The build configuration in the source text of a changed body (batch 5k, A1).
//!
//! The compiler removes the code of a false `cfg` attribute before the MIR. The MIR of the
//! check configuration does not show this code, so a proof from the MIR does not hold for each
//! build. This module reads the token text of a changed item. It uses allow lists: an
//! attribute or a macro call that is not in a list makes the body not free.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use semilla_syntax::{Item, ItemKind, Workspace};

use crate::{BodyComparison, BodyStatus};

/// The mark of a use that Semilla cannot see into (a macro or an attribute of another crate).
pub(crate) const UNSEEN: &str = " (not known)";

/// The attributes that do not select code by the build configuration.
const ATTR_FREE: [&str; 21] = [
    "allow", "expect", "warn", "deny", "forbid", "doc", "inline", "cold", "must_use", "test", "should_panic", "ignore", "derive", "repr", "non_exhaustive", "track_caller", "rustfmt::skip",
    "deprecated", "automatically_derived", "default", "macro_export",
];

/// The macros of the standard library with an expansion that has no `cfg` attribute. The MIR
/// driver makes the values of `cfg!`, `env!`, and `option_env!` opaque.
pub(crate) const MACRO_FREE: [&str; 33] = [
    "println", "print", "eprintln", "eprint", "format", "format_args", "write", "writeln", "vec", "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne",
    "matches", "panic", "unreachable", "unimplemented", "todo", "concat", "stringify", "line", "column", "file", "module_path", "include_str", "include_bytes", "cfg", "env", "option_env",
    "compile_error", "dbg",
];

/// The keywords of each edition of Rust (the strict keywords and the reserved keywords of the
/// 2015 edition). A keyword is never the name of a macro: `if !(a)` is the operator `!` after
/// `if`. A keyword of a later edition only (`async`, `await`, `dyn`, `try`, `gen`) is not in the
/// list: it is a macro name in an earlier edition. A raw identifier (`r#try!`) is not a keyword.
pub(crate) const KEYWORDS: [&str; 47] = [
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro", "override", "priv", "typeof",
    "unsized", "virtual", "yield",
];

const STD: [&str; 3] = ["std", "core", "alloc"];
const LOCAL: [&str; 3] = ["crate", "self", "super"];

/// The uses of the build configuration in one token text.
#[derive(Clone, Default)]
pub(crate) struct Found {
    /// The text of each use, for example `#[cfg(unix)]`.
    pub(crate) config: BTreeSet<String>,
    /// Each macro call and each attribute that Semilla cannot see into.
    pub(crate) unseen: BTreeSet<String>,
    /// Each macro call that Semilla cannot see into, with a name of the rename map in its
    /// tokens (batch 5x, part A). The text is a cause for the reader.
    pub(crate) named: BTreeSet<String>,
}

pub(crate) struct Scan<'a> {
    /// The `macro_rules!` items of the two trees, by name.
    macros: BTreeMap<String, Vec<&'a Item>>,
    /// The names of the workspace crates.
    crates: BTreeSet<String>,
    /// The files with a glob import from a crate that is not in the workspace: a macro of such
    /// a crate can have the name of a standard macro.
    foreign: BTreeSet<String>,
    /// The crate roots with `#[macro_use]` on an item (batch 5x, part A). The scope of such an
    /// import is its crate root: a different target of the same package does not get it.
    macro_use: BTreeSet<String>,
    /// The packages with `#[macro_use]` in a file with no known crate root (the wide rule).
    macro_use_wide: BTreeSet<String>,
    /// The names that an import from a crate outside the workspace gives, each with its scope:
    /// a crate root, or [`EACH`] for each crate (batch 5x, part A).
    shadowed: BTreeSet<(String, String)>,
    /// The crate roots of each file: the root file of each target that compiles the file.
    roots: BTreeMap<String, BTreeSet<String>>,
    /// The names of a rename map. `call` keeps each macro call that Semilla cannot see into
    /// and that has one of the names in its tokens (batch 5x, part A).
    watch: BTreeSet<String>,
    memo: BTreeMap<String, Option<Found>>,
}

/// The scope "each crate" of a shadowed name.
const EACH: &str = "";

/// The crate roots of each file of a workspace: the root file of each target that compiles
/// the file. A file with a module of the target is a file of the target. The result can have
/// more roots for a file than the compiler has (a module and a target with the same path),
/// never fewer: a file that this function does not find has no entry, and the caller uses the
/// wide rule for it.
pub(crate) fn file_roots(workspace: &Workspace, out: &mut BTreeMap<String, BTreeSet<String>>) {
    // The files of the modules below each file.
    let mut below: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for module in workspace.items.values().filter(|item| item.kind == ItemKind::Mod) {
        // A module file that two targets read has two module items, with a twin suffix at the
        // end of the path. The items of the file have the path with no suffix.
        let plain = match module.path.rfind('#') {
            Some(at) if !module.path[at..].contains("::") => &module.path[..at],
            _ => module.path.as_str(),
        };
        for prefix in [format!("{}::", module.path), format!("{plain}::")] {
            for (_, item) in workspace.items.range(prefix.clone()..).take_while(|(path, _)| path.starts_with(&prefix)) {
                if item.file != module.file {
                    below.entry(module.file.as_str()).or_default().insert(item.file.as_str());
                }
            }
        }
    }
    for target in &workspace.targets {
        let mut todo = vec![target.file.as_str()];
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        while let Some(file) = todo.pop() {
            if seen.insert(file) {
                out.entry(file.to_string()).or_default().insert(target.file.clone());
                todo.extend(below.get(file).into_iter().flatten().copied());
            }
        }
    }
}

impl<'a> Scan<'a> {
    pub(crate) fn new(before: &'a Workspace, after: &'a Workspace) -> Scan<'a> {
        let mut scan = Scan {
            macros: BTreeMap::new(),
            crates: BTreeSet::new(),
            foreign: BTreeSet::new(),
            macro_use: BTreeSet::new(),
            macro_use_wide: BTreeSet::new(),
            shadowed: BTreeSet::new(),
            roots: BTreeMap::new(),
            watch: BTreeSet::new(),
            memo: BTreeMap::new(),
        };
        for workspace in [before, after] {
            scan.crates.extend(workspace.crates.iter().map(|c| c.name.replace('-', "_")));
            file_roots(workspace, &mut scan.roots);
        }
        for workspace in [before, after] {
            for item in workspace.items.values() {
                match item.kind {
                    ItemKind::Macro => {
                        // The path of a twin has a suffix after the mark: `name!#1`.
                        let name = item.path.rsplit("::").next().unwrap_or("").split('#').next().unwrap_or("").trim_end_matches('!').to_string();
                        scan.macros.entry(name).or_default().push(item);
                    }
                    ItemKind::Use | ItemKind::Other => {
                        // The crate roots that have the item. `None`: not known, so the scope
                        // is the wide one.
                        let roots: Option<Vec<String>> = scan.roots.get(&item.file).filter(|roots| !roots.is_empty()).map(|roots| roots.iter().cloned().collect());
                        // `#[macro_use] extern crate x;`: the macros of `x` are in each module
                        // of the crate root that has the item, and in no other crate root
                        // (batch 5x, part A: a bench target of `humantime` stopped each proof
                        // of the library).
                        if item.text.replace(' ', "").contains("#[macro_use") {
                            match &roots {
                                Some(roots) => scan.macro_use.extend(roots.iter().cloned()),
                                None => {
                                    scan.macro_use_wide.insert(item.crate_name.clone());
                                }
                            }
                        }
                        if item.kind != ItemKind::Use {
                            continue;
                        }
                        // A `pub use` can give the name to a different crate. Each other `use`
                        // stays in its crate root: a child module gets it with `use super::*`.
                        let scopes: Vec<String> = match &roots {
                            Some(roots) if item.vis != "pub" => roots.clone(),
                            _ => vec![EACH.to_string()],
                        };
                        let foreign = |first: &str| !(LOCAL.contains(&first) || STD.contains(&first) || scan.crates.contains(first));
                        match crate::opaque::use_imports(&item.text) {
                            // A name that the line can bring: the last segment, the name after
                            // `as`, each name of a group. A segment in the middle of a path
                            // brings no name (`use chrono::format::Fixed::*;` brings no macro
                            // with the name `format` by that segment).
                            Some(imports) => {
                                for import in imports.iter().filter(|import| foreign(&import.first)) {
                                    match &import.leaf {
                                        None => {
                                            scan.foreign.insert(item.file.clone());
                                        }
                                        Some(name) if MACRO_FREE.contains(&name.as_str()) => scan.shadowed.extend(scopes.iter().map(|scope| (scope.clone(), name.clone()))),
                                        Some(_) => {}
                                    }
                                }
                            }
                            // A text that Semilla cannot read: each word is a name, for each
                            // crate.
                            None => {
                                let idents = item.text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|t| !t.is_empty());
                                scan.shadowed.extend(idents.filter(|t| MACRO_FREE.contains(t)).map(|t| (EACH.to_string(), t.to_string())));
                                scan.foreign.insert(item.file.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        scan
    }

    /// True if a macro of a crate outside the workspace can have the name `name` in `item`,
    /// with no path before the name.
    fn hidden(&self, name: &str, item: &Item) -> bool {
        if self.foreign.contains(&item.file) || self.macro_use_wide.contains(&item.crate_name) || self.shadowed.contains(&(EACH.to_string(), name.to_string())) {
            return true;
        }
        match self.roots.get(&item.file).filter(|roots| !roots.is_empty()) {
            Some(roots) => roots.iter().any(|root| self.macro_use.contains(root) || self.shadowed.contains(&(root.clone(), name.to_string()))),
            // No known crate root: the wide rule (each `#[macro_use]`, each import of the name).
            None => !self.macro_use.is_empty() || self.shadowed.iter().any(|(_, shadowed)| shadowed == name),
        }
    }

    /// The uses in the definitions of the workspace macro `name`. `None`: no such macro.
    fn workspace_macro(&mut self, name: &str) -> Option<Found> {
        if let Some(done) = self.memo.get(name) {
            return done.clone();
        }
        let items = self.macros.get(name)?.clone();
        // A macro that calls itself: the first visit decides.
        self.memo.insert(name.to_string(), Some(Found::default()));
        let mut found = Found::default();
        for item in items {
            self.text(&item.text, item, false, &mut found);
        }
        self.memo.insert(name.to_string(), Some(found.clone()));
        Some(found)
    }

    /// The definitions of the workspace macro `name`.
    pub(crate) fn definitions(&self, name: &str) -> Vec<&'a Item> {
        self.macros.get(name).cloned().unwrap_or_default()
    }

    /// The uses in the text of an item. A `cfg` attribute of the item itself is not a use
    /// (see `stream`); the caller looks at the attributes of the item and its parents.
    pub(crate) fn uses(&mut self, item: &Item) -> Found {
        let mut found = Found::default();
        self.text(&item.text, item, true, &mut found);
        found
    }

    fn text(&mut self, text: &str, item: &Item, own: bool, found: &mut Found) {
        match text.parse::<TokenStream>() {
            Ok(stream) => self.stream(stream, item, own, found),
            Err(_) => {
                found.unseen.insert("text that Semilla cannot read".to_string());
            }
        }
    }

    /// `own`: the stream starts with the attributes of the item itself. A `cfg` attribute of
    /// the item selects the full item, so each build that has the body has the same body.
    fn stream(&mut self, stream: TokenStream, item: &Item, own: bool, found: &mut Found) {
        let _at = crate::cfgcond::at(&item.file);
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        let punct = |tree: Option<&TokenTree>, c: char| matches!(tree, Some(TokenTree::Punct(p)) if p.as_char() == c);
        let mut leading = own;
        let mut index = 0;
        while index < trees.len() {
            // An attribute: `#`, an optional `!`, and a group in brackets.
            if punct(trees.get(index), '#') {
                let at = if punct(trees.get(index + 1), '!') { index + 2 } else { index + 1 };
                if let Some(TokenTree::Group(group)) = trees.get(at).filter(|t| matches!(t, TokenTree::Group(g) if g.delimiter() == Delimiter::Bracket)) {
                    let name = attribute_name(group.stream());
                    let text = format!("#[{}]", compact(&group.stream().to_string()));
                    // Free: a `cfg_attr` with a content of the allow list (batch 5o, A5), and an
                    // attribute with a condition that the checked configurations cover (batch 5s).
                    let free_attr = (name == "cfg_attr" && matches!(crate::opaque::attr_kind(group.stream()), Some(crate::opaque::Attr::Free)))
                        || matches!(crate::opaque::attr_kind(group.stream()), Some(crate::opaque::Attr::Covered));
                    if ATTR_FREE.contains(&name.as_str()) || crate::facts::is_lint_tool(&name) || (leading && name == "cfg") || free_attr {
                        // A free attribute. Its arguments are not code. A `cfg_attr` is free
                        // if its content is in the allow list (batch 5o, A5).
                    } else if name == "cfg" || name == "cfg_attr" {
                        found.config.insert(text);
                    } else {
                        found.unseen.insert(text);
                    }
                    index = at + 1;
                    continue;
                }
            }
            leading = false;
            match &trees[index] {
                TokenTree::Group(group) => self.stream(group.stream(), item, false, found),
                TokenTree::Ident(ident) if punct(trees.get(index + 1), '!') && !punct(trees.get(index + 2), '=') => {
                    let name = ident.to_string();
                    // Not a macro name (batch 5l, B4): a keyword (`if !(a)`, `return !(a)`), and
                    // the name of a label (`break 'a !(b)`). The `!` is the operator.
                    let label = index >= 1 && punct(trees.get(index - 1), '\'');
                    let operator = label || KEYWORDS.contains(&name.as_str());
                    let name = name.trim_start_matches("r#");
                    // `macro_rules! name { … }`: the definition of a macro in the body.
                    let call = matches!(trees.get(index + 2), Some(TokenTree::Group(_)));
                    if name != "macro_rules" && call && !operator {
                        self.call(&trees, index, name, item, found);
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    /// One macro call: the name is the token at `index`, the path is before it.
    fn call(&mut self, trees: &[TokenTree], index: usize, name: &str, item: &Item, found: &mut Found) {
        let punct = |at: usize, c: char| matches!(trees.get(at), Some(TokenTree::Punct(p)) if p.as_char() == c);
        // The first segment of the path, and a `$` before the path (a fragment or `$crate`).
        let mut start = index;
        while start >= 3 && punct(start - 1, ':') && punct(start - 2, ':') && matches!(trees.get(start - 3), Some(TokenTree::Ident(_))) {
            start -= 3;
        }
        let global = start >= 2 && punct(start - 1, ':') && punct(start - 2, ':');
        let dollar = start >= 1 && punct(start - 1, '$');
        let first = trees[start].to_string();
        let qualified = start != index;
        let shown = format!("{name}!");
        let std_free = MACRO_FREE.contains(&name);
        let free = if dollar && !qualified {
            // `$name!(…)`: the caller of the macro gives the name.
            false
        } else if qualified && !global && STD.contains(&first.as_str()) && !self.crates.contains(&first) {
            std_free
        } else if !qualified || (!global && (LOCAL.contains(&first.as_str()) || self.crates.contains(&first))) {
            match self.workspace_macro(name) {
                Some(inner) => {
                    found.config.extend(inner.config.iter().map(|text| format!("{shown} ({text})")));
                    found.unseen.extend(inner.unseen.iter().map(|text| format!("{shown} ({text})")));
                    true
                }
                None => !qualified && std_free && !self.hidden(name, item),
            }
        } else {
            false
        };
        // A name of the rename map in the tokens of a call that Semilla cannot see into (the
        // call itself, or a macro of a different crate that a workspace macro calls). The
        // macro can make a string from the name, or a different item.
        let passes = found.unseen.iter().any(|text| text.starts_with(&format!("{shown} (")));
        if (!free || passes) && !self.watch.is_empty() {
            let mut inside = BTreeSet::new();
            if let Some(TokenTree::Group(group)) = trees.get(index + 2) {
                idents(&group.stream().into_iter().collect::<Vec<_>>(), &mut inside);
            }
            if let Some(name) = inside.iter().map(|name| name.trim_start_matches("r#")).find(|name| self.watch.contains(*name)) {
                found.named.insert(format!("`{shown}` with the name `{name}` of the rename in its tokens (Semilla cannot see into it)"));
            }
        }
        if !free {
            found.unseen.insert(shown);
        }
    }
}

/// The path of an attribute: `cfg`, `rustfmt::skip`. Empty if the path is not plain text.
fn attribute_name(stream: TokenStream) -> String {
    let mut name = String::new();
    for tree in stream {
        match tree {
            TokenTree::Ident(ident) => name.push_str(&ident.to_string()),
            TokenTree::Punct(p) if p.as_char() == ':' => name.push(':'),
            TokenTree::Group(_) => break,
            TokenTree::Punct(p) if p.as_char() == '=' => break,
            _ => return String::new(),
        }
    }
    name
}

/// The token text with no space next to a punctuation mark.
pub(crate) fn compact(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (index, c) in chars.iter().enumerate() {
        if *c == ' ' {
            let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '"');
            if !(word(index.checked_sub(1).and_then(|i| chars.get(i))) && word(chars.get(index + 1))) {
                continue;
            }
        }
        out.push(*c);
    }
    out
}

/// True if the token at `at` is the end of a statement, of a match arm, or of an argument:
/// `;`, `,`, or a block before the start of a new statement.
fn ends(trees: &[TokenTree], at: usize) -> bool {
    match &trees[at] {
        TokenTree::Punct(p) => matches!(p.as_char(), ';' | ','),
        TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => match trees.get(at + 1) {
            Some(TokenTree::Punct(p)) => p.as_char() == '#',
            Some(TokenTree::Ident(ident)) => !["else", "as"].contains(&ident.to_string().as_str()),
            Some(_) => true,
            None => false,
        },
        _ => false,
    }
}

/// The tokens of a text. Empty if Semilla cannot read the text.
fn tokens_of(text: &str) -> Vec<TokenTree> {
    text.parse::<TokenStream>().map(|stream| stream.into_iter().collect()).unwrap_or_default()
}

/// Each identifier of the tokens, at each depth.
fn idents(trees: &[TokenTree], out: &mut BTreeSet<String>) {
    for tree in trees {
        match tree {
            TokenTree::Ident(ident) => {
                out.insert(ident.to_string());
            }
            TokenTree::Group(group) => idents(&group.stream().into_iter().collect::<Vec<_>>(), out),
            _ => {}
        }
    }
}

/// The changed part of an item text, for the two sides (batch 5o, A4): the statements that
/// differ, and the full statement around each block that holds them. So an attribute or a
/// condition above the changed code is in the part, and a use in a different statement is
/// not. The last value is true if the part starts at the start of the item.
fn changed(old: Vec<TokenTree>, new: Vec<TokenTree>) -> (Vec<TokenTree>, Vec<TokenTree>, bool) {
    let text = |trees: &[TokenTree]| trees.iter().map(|tree| tree.to_string()).collect::<Vec<_>>();
    let (old_text, new_text) = (text(&old), text(&new));
    let most = old.len().min(new.len());
    let prefix = (0..most).take_while(|at| old_text[*at] == new_text[*at]).count();
    let suffix = (0..most - prefix).take_while(|at| old_text[old.len() - 1 - at] == new_text[new.len() - 1 - at]).count();
    if prefix == old.len() && prefix == new.len() {
        return (Vec::new(), Vec::new(), false);
    }
    // The start of the statement with the first changed token, and the end of the statement
    // with the last one. The tokens before `prefix` are the same on the two sides.
    let start = (0..prefix).rev().find(|at| ends(&new, *at)).map(|at| at + 1).unwrap_or(0);
    let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
    // No tail if the changed tokens of each side stop at the end of a statement.
    let whole = |side: &[TokenTree], end: usize| end == prefix || ends(side, end - 1);
    let tail = if whole(&old, old_end) && whole(&new, new_end) { 0 } else { (new_end..new.len()).find(|at| ends(&new, *at)).map(|at| at + 1 - new_end).unwrap_or(suffix) };
    if let (1, 1, Some(TokenTree::Group(a)), Some(TokenTree::Group(b))) = (old_end - prefix, new_end - prefix, old.get(prefix), new.get(prefix)) {
        if a.delimiter() == b.delimiter() {
            let (inner_old, inner_new, _) = changed(a.stream().into_iter().collect(), b.stream().into_iter().collect());
            let wrap = |side: &[TokenTree], inner: Vec<TokenTree>, end: usize| {
                let mut out: Vec<TokenTree> = side[start..prefix].to_vec();
                out.push(TokenTree::Group(proc_macro2::Group::new(a.delimiter(), inner.into_iter().collect())));
                out.extend(side[end..end + tail].iter().cloned());
                out
            };
            return (wrap(&old, inner_old, old_end), wrap(&new, inner_new, new_end), start == 0);
        }
    }
    (old[start..old_end + tail].to_vec(), new[start..new_end + tail].to_vec(), start == 0)
}

/// Changes the comparison of each body with a text edit and with code that depends on the
/// build configuration in its source text. The body is not equal for each build, so the
/// status is "different", and the comparison names the uses.
pub(crate) fn apply(bodies: &mut [BodyComparison], before: &Workspace, after: &Workspace, renames: &BTreeMap<String, String>) {
    let mut scan = Scan::new(before, after);
    let old_of: BTreeMap<&String, &String> = renames.iter().map(|(old, new)| (new, old)).collect();
    // The names of the rename map (batch 5x, part A).
    let pairs = crate::opaque::rename_pairs(renames);
    scan.watch = pairs.iter().filter(|(old, new)| old != new).flat_map(|(old, new)| [old.clone(), new.clone()]).collect();
    let mut done: BTreeMap<String, Option<(Found, Found, Option<BTreeSet<String>>)>> = BTreeMap::new();
    for body in bodies.iter_mut() {
        if !matches!(body.status, BodyStatus::Equal { .. } | BodyStatus::Different { .. }) {
            continue;
        }
        let Some(path) = crate::checks::find_item_by(&body.name, body.span.as_ref(), after) else { continue };
        let found = done.entry(path.clone()).or_insert_with(|| {
            let new = after.items.get(&path)?;
            let old = before.items.get(old_of.get(&path).copied().unwrap_or(&path));
            // The same text on the two sides: no edit of this body.
            if old.is_some_and(|old| old.text == new.text) {
                return None;
            }
            let mut found = Found::default();
            scan.text(&new.text, new, true, &mut found);
            let mut near = found.clone();
            let mut words: Option<BTreeSet<String>> = None;
            if let Some(old) = old {
                scan.text(&old.text, old, true, &mut found);
                // A text that differs only by the names of the rename map has no edit (batch
                // 5x, part A; the same rule as in `Opaque::apply`). The MIR with the rename
                // map is the proof in each checked configuration. Three cases keep the rule
                // of an edit: the compiler does not show Semilla what the new name means
                // there. (1) A name of the map in the tokens of a macro call that Semilla
                // cannot see into. (2) An attribute that Semilla cannot see into: it gets the
                // tokens of its item. (3) A `cfg` attribute in the body: the code of a
                // different configuration can give the new name a different meaning.
                let calls_only = found.unseen.iter().all(|text| !text.starts_with('#') && text.contains('!'));
                if crate::opaque::same_text(&old.text, &new.text, &pairs) && found.config.is_empty() && found.named.is_empty() && calls_only {
                    return None;
                }
                // The uses in the changed statements (batch 5o, A4). A text that Semilla
                // cannot read keeps each use.
                near = found.clone();
                if let (Ok(old_stream), Ok(new_stream)) = (old.text.parse::<TokenStream>(), new.text.parse::<TokenStream>()) {
                    let (old_part, new_part, start) = changed(old_stream.into_iter().collect(), new_stream.into_iter().collect());
                    // The same tokens: the edit is in a comment or in the space.
                    if old_part.is_empty() && new_part.is_empty() {
                        return None;
                    }
                    near = Found::default();
                    let mut all = BTreeSet::new();
                    idents(&old_part, &mut all);
                    idents(&new_part, &mut all);
                    scan.stream(new_part.into_iter().collect(), new, start, &mut near);
                    scan.stream(old_part.into_iter().collect(), old, start, &mut near);
                    let mut full = BTreeSet::new();
                    idents(&tokens_of(&old.text), &mut full);
                    idents(&tokens_of(&new.text), &mut full);
                    words = Some(full.difference(&all).cloned().collect());
                }
            }
            Some((found, near, words))
        });
        let Some((found, near, words)) = found else { continue };
        // A use that the driver gives (a macro call that reads the configuration, a static):
        // no spotlight if its name is in the text of the body, but not in the changed
        // statements (batch 5o, A4). `words`: the names only outside the changed statements.
        if let Some(words) = words {
            body.config.retain(|text| {
                let name = text.split('`').nth(1).and_then(|part| part.split(|c: char| !(c.is_alphanumeric() || c == '_')).find(|word| !word.is_empty()));
                !name.is_some_and(|name| words.contains(name))
            });
        }
        if found.config.is_empty() && found.unseen.is_empty() {
            continue;
        }
        let equal = matches!(body.status, BodyStatus::Equal { .. });
        // The proof stops for a use in each part of the body: the same MIR in the checked
        // configuration is not a proof for each build.
        if equal {
            body.status = BodyStatus::Different { cfg_changed: true };
            // The cause, for the note of a fact with no spotlight (batch 5u).
            // A call with a name of the rename map is the first cause (batch 5x, part A).
            let causes = found.named.iter().cloned().chain(found.config.iter().cloned()).chain(found.unseen.iter().map(|text| format!("{text}{UNSEEN}")));
            for text in causes {
                if !body.stopped.contains(&text) {
                    body.stopped.push(text);
                }
            }
        }
        // The spotlight is only for a use in the changed statements. A body that is different
        // in the MIR: the reader reads the edit. Only the uses that Semilla sees get the
        // spotlight.
        let uses: Vec<String> = near.config.iter().cloned().chain(near.unseen.iter().filter(|_| equal).map(|text| format!("{text}{UNSEEN}"))).collect();
        for text in uses {
            if !body.config.contains(&text) {
                body.config.push(text);
            }
        }
    }
}
