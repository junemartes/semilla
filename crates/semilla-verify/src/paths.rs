//! The normal form of an item path (design 54.10). An agent writes the path of an item in many
//! forms. This module compares two paths part by part.
//!
//! The rules:
//!
//! - The parts of the path that the agent gives must agree with the last parts of the path of
//!   the item.
//! - `<TYPE as TRAIT>` and `<impl TRAIT for TYPE>` are the same part.
//! - A path in a part (`TYPE`, `TRAIT`) agrees if the two paths name the same item of the
//!   workspace (design 54.18). The name lookup of the rename map gives the item (`crate`,
//!   `self`, `super`, `use` lines, re-exports). With no answer of the lookup, the path agrees
//!   if its last segments agree. The module path of the type can be before the `<…>` or in it.
//! - Generic arguments are optional. If the two sides give them, they must be equal.
//! - White space and lifetimes are not important.
//!
//! The rule gives a set of items. The caller decides: one item is the answer, and each other
//! count is an error with the candidates.

use std::collections::BTreeMap;

use semilla_syntax::{Item, ItemKind, Workspace};

/// The path of a type or of a trait in a part.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Name {
    /// The segments of the path. A type that is not a path (`[T;N]`, `&T`) is one segment.
    segments: Vec<String>,
    /// The generic arguments at the end, with no white space.
    arguments: Option<String>,
    /// The words `crate` and `self` at the start of the path. They are no segments for the
    /// compare. The name lookup needs them.
    start: Vec<String>,
}

impl Name {
    fn last(&self) -> &str {
        self.segments.last().map(String::as_str).unwrap_or("")
    }

    fn text(&self) -> String {
        match &self.arguments {
            Some(arguments) => format!("{}<{arguments}>", self.segments.join("::")),
            None => self.segments.join("::"),
        }
    }
}

/// One part of a path.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Part {
    /// A name: a crate, a module, an item, a field (`.name`), a macro (`name!`).
    Plain { name: String, arguments: Option<String>, twin: Option<String> },
    /// `<TYPE as TRAIT>`, `<impl TRAIT for TYPE>`, `<TYPE>`. With `block`: the `impl` block.
    Impl { ty: Name, tr: Option<Name>, block: bool, twin: Option<String> },
}

/// True if the shorter list is the end of the longer list.
fn ends_agree(a: &[String], b: &[String]) -> bool {
    let count = a.len().min(b.len());
    count > 0 && a[a.len() - count..] == b[b.len() - count..]
}

/// Generic arguments are optional: they must be equal only if the two sides give them.
fn arguments_agree(a: &Option<String>, b: &Option<String>) -> bool {
    a.is_none() || b.is_none() || a == b
}

fn twin_agrees(query: &Option<String>, item: &Option<String>) -> bool {
    query.is_none() || query == item
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The text with no lifetime, and with a space only between two words.
fn tidy(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    let mut space = false;
    while index < chars.len() {
        let c = chars[index];
        if c == '\'' && chars.get(index + 1).is_some_and(|next| word(*next)) {
            // A lifetime, with the comma after it.
            index += 1;
            while chars.get(index).is_some_and(|next| word(*next)) {
                index += 1;
            }
            while chars.get(index).is_some_and(|next| next.is_whitespace()) {
                index += 1;
            }
            if chars.get(index) == Some(&',') {
                index += 1;
            }
            space = true;
            continue;
        }
        if c.is_whitespace() {
            space = true;
        } else {
            if space && word(c) && out.chars().last().is_some_and(word) {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
        index += 1;
    }
    // A list of generic arguments that had only lifetimes.
    out.replace("<>", "")
}

/// The index of the bracket that closes the bracket at `open`.
fn close_of(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, c) in text.char_indices().skip_while(|(index, _)| *index < open) {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if text[..index].ends_with('-') => {}
            '>' | ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The start and the end of the first key word `key` of the text that is in no bracket. A key
/// word is not a part of a longer word. `{` is a key word too.
fn find_outside(text: &str, key: &str) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    for (index, c) in text.char_indices() {
        if depth == 0 && text[index..].starts_with(key) {
            let end = index + key.len();
            let alone = !key.chars().all(word) || (!text[..index].chars().last().is_some_and(word) && !text[end..].chars().next().is_some_and(word));
            if alone {
                return Some((index, end));
            }
        }
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if text[..index].ends_with('-') => {}
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// The name of a type or of a trait from its text.
fn name_of(text: &str) -> Name {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let text = text.strip_prefix("::").unwrap_or(&text);
    let (head, arguments) = match text.find('<') {
        Some(at) if text.ends_with('>') && close_of(text, at) == Some(text.len() - 1) => (&text[..at], Some(text[at + 1..text.len() - 1].to_string())),
        _ => (text, None),
    };
    let is_path = !head.is_empty() && head.split("::").all(|segment| !segment.is_empty() && segment.chars().all(word));
    if !is_path {
        return Name { segments: vec![text.to_string()], arguments: None, start: Vec::new() };
    }
    let mut segments: Vec<String> = head.split("::").map(str::to_string).collect();
    // `crate::money::Price` and `self::Price`: the start names no module of its own.
    let mut start = Vec::new();
    while segments.len() > 1 && matches!(segments[0].as_str(), "crate" | "self") {
        start.push(segments.remove(0));
    }
    Name { segments, arguments, start }
}

/// `TYPE as TRAIT`, or `TYPE`.
fn type_as_trait(text: &str) -> (Name, Option<Name>) {
    match find_outside(text, "as") {
        Some((at, end)) => (name_of(&text[..at]), Some(name_of(&text[end..]))),
        None => (name_of(text), None),
    }
}

/// The text after the word `impl`: `TRAIT for TYPE`, `TYPE`, or `<TYPE as TRAIT>`, with the
/// optional generic parameters first.
fn after_impl(text: &str) -> (Name, Option<Name>) {
    let mut rest = text.trim();
    if rest.starts_with('<') {
        match close_of(rest, 0) {
            // The form of Semilla: `impl <TYPE as TRAIT>`.
            Some(end) if end == rest.len() - 1 => return type_as_trait(&rest[1..end]),
            // The generic parameters of the block.
            Some(end) => rest = rest[end + 1..].trim(),
            None => {}
        }
    }
    if let Some((at, _)) = find_outside(rest, "where").or_else(|| find_outside(rest, "{")) {
        rest = rest[..at].trim();
    }
    for start in ["unsafe ", "const ", "!"] {
        rest = rest.strip_prefix(start).unwrap_or(rest).trim();
    }
    match find_outside(rest, "for") {
        Some((at, end)) => (name_of(&rest[end..]), Some(name_of(&rest[..at]))),
        None => (name_of(rest), None),
    }
}

/// The text after `impl`, if the text starts with the word `impl`.
fn impl_rest(text: &str) -> Option<&str> {
    text.strip_prefix("impl").filter(|rest| rest.starts_with([' ', '<', '[', '(', '&', '*']))
}

/// The type and the trait of the header of an `impl` block (`impl<T> Scale<u8> for [T; N]`).
fn header(text: &str) -> Option<(Name, Option<Name>)> {
    let text = tidy(text);
    let mut from = 0;
    while let Some(at) = text[from..].find("impl").map(|at| at + from) {
        let before = text[..at].chars().last();
        if before.is_none_or(|c| !word(c)) {
            if let Some(rest) = impl_rest(&text[at..]) {
                return Some(after_impl(rest));
            }
        }
        from = at + 4;
    }
    None
}

/// Parses a path into its parts.
pub(crate) fn parse(text: &str) -> Vec<Part> {
    let text = tidy(text);
    // The segments: the text between each `::` and before each `.` that is in no bracket.
    let mut segments: Vec<String> = vec![String::new()];
    let mut depth = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if index > 0 && chars[index - 1] == '-' => {}
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 && chars.get(index + 1) == Some(&':') => {
                segments.push(String::new());
                index += 2;
                continue;
            }
            '.' if depth == 0 && chars.get(index + 1).is_some_and(|next| word(*next)) && !segments.last().unwrap().is_empty() => {
                segments.push(String::new());
            }
            _ => {}
        }
        segments.last_mut().unwrap().push(c);
        index += 1;
    }
    let mut parts: Vec<Part> = Vec::new();
    for (position, segment) in segments.iter().enumerate() {
        let mut segment = segment.trim();
        if segment.is_empty() || (position == 0 && segment == "crate") {
            continue;
        }
        let mut twin = None;
        if let Some((rest, number)) = segment.rsplit_once('#').filter(|(_, number)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())) {
            (segment, twin) = (rest, Some(number.to_string()));
        }
        if let Some(rest) = impl_rest(segment) {
            let (ty, tr) = after_impl(rest);
            parts.push(Part::Impl { ty, tr, block: true, twin });
        } else if segment.starts_with('<') && close_of(segment, 0) == Some(segment.len() - 1) {
            let inner = segment[1..segment.len() - 1].trim();
            if let Some(rest) = impl_rest(inner) {
                let (ty, tr) = after_impl(rest);
                parts.push(Part::Impl { ty, tr, block: false, twin });
            } else if find_outside(inner, "as").is_none() && matches!(parts.last(), Some(Part::Plain { arguments: None, .. })) {
                // `Name::<T>`: the generic arguments of the name before.
                if let Some(Part::Plain { arguments, .. }) = parts.last_mut() {
                    *arguments = Some(inner.chars().filter(|c| !c.is_whitespace()).collect());
                }
            } else {
                let (ty, tr) = type_as_trait(inner);
                parts.push(Part::Impl { ty, tr, block: false, twin });
            }
        } else {
            let (name, arguments) = match segment.find('<') {
                Some(at) if at > 0 && close_of(segment, at) == Some(segment.len() - 1) => (&segment[..at], Some(segment[at + 1..segment.len() - 1].chars().filter(|c| !c.is_whitespace()).collect())),
                _ => (segment, None),
            };
            parts.push(Part::Plain { name: name.chars().filter(|c| !c.is_whitespace()).collect(), arguments, twin });
        }
    }
    parts
}

/// The name of a plain part with no field mark.
fn bare(name: &str) -> &str {
    name.strip_prefix('.').unwrap_or(name)
}

/// The text of a path with no twin mark (`#0`, `#1`).
pub(crate) fn no_twin(path: &str) -> String {
    let mut out = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek().is_some_and(char::is_ascii_digit) {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// One item of a tree with the parts of its path.
struct Entry<'a> {
    item: &'a Item,
    parts: Vec<Part>,
    /// The path with the generic arguments of the header of its `impl` block, if it has some.
    label: String,
    /// The `impl` block of the item, for an item with a part `<…>`.
    block: Option<&'a Item>,
}

/// The items of one tree, each with the parts of its path.
pub(crate) struct Index<'a> {
    entries: Vec<Entry<'a>>,
    /// The paths of the types and the traits of the tree, by the last name.
    types: BTreeMap<String, Vec<Vec<String>>>,
    workspace: &'a Workspace,
    /// The answers of the name lookup: the path of an item (empty for a path from a crate
    /// root) and the segments, with the item that the path names there.
    lookups: std::cell::RefCell<BTreeMap<(String, Vec<String>), Option<String>>>,
}

impl<'a> Index<'a> {
    pub(crate) fn new(workspace: &'a Workspace) -> Index<'a> {
        // The `impl` blocks of each file: the block of a method is the smallest block with
        // the lines of the method.
        let mut blocks: BTreeMap<&str, Vec<&Item>> = BTreeMap::new();
        for item in workspace.items.values().filter(|item| item.kind == ItemKind::Impl) {
            blocks.entry(item.file.as_str()).or_default().push(item);
        }
        let mut types: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        let mut entries = Vec::new();
        for item in workspace.items.values().filter(|item| item.kind != ItemKind::Use) {
            let mut parts = parse(&item.path);
            if matches!(item.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Type | ItemKind::Trait | ItemKind::Other) && parts.iter().all(|part| matches!(part, Part::Plain { .. })) {
                let segments: Vec<String> = parts.iter().map(|part| if let Part::Plain { name, .. } = part { name.clone() } else { String::new() }).collect();
                if let Some(last) = segments.last() {
                    types.entry(last.clone()).or_default().push(segments.clone());
                }
            }
            let mut label = item.path.clone();
            let mut own_block = None;
            if let Some(at) = parts.iter().position(|part| matches!(part, Part::Impl { .. })) {
                let block = if item.kind == ItemKind::Impl {
                    Some(item)
                } else {
                    let inside = |block: &&&Item| block.line_start <= item.line_start && block.line_end >= item.line_end;
                    blocks.get(item.file.as_str()).and_then(|list| list.iter().filter(inside).min_by_key(|block| block.line_end - block.line_start)).copied()
                };
                own_block = block;
                let found = block.and_then(|block| header(&block.display.signature).or_else(|| header(&block.signature)));
                let count = parts.len();
                if let (Some((head_ty, head_tr)), Part::Impl { ty, tr, block, twin }) = (found, &mut parts[at]) {
                    let mut changed = false;
                    if head_ty.arguments.is_some() && head_ty.last() == ty.last() {
                        (ty.arguments, changed) = (head_ty.arguments, true);
                    }
                    if let (Some(head_tr), Some(tr)) = (head_tr, tr.as_mut()) {
                        if head_tr.arguments.is_some() && head_tr.last() == tr.last() {
                            (tr.arguments, changed) = (head_tr.arguments, true);
                        }
                    }
                    if changed {
                        // The label: the path of Semilla, with the arguments in the part.
                        let part = match tr {
                            Some(tr) => format!("<{} as {}>", ty.text(), tr.text()),
                            None => ty.text(),
                        };
                        let twin = twin.as_ref().map(|number| format!("#{number}")).unwrap_or_default();
                        let segments = split_path(&item.path);
                        if segments.len() == count {
                            let mut out: Vec<String> = segments.iter().map(|segment| segment.to_string()).collect();
                            out[at] = format!("{}{part}{twin}", if *block { "impl " } else { "" });
                            label = out.join("::");
                        }
                    }
                }
            }
            entries.push(Entry { item, parts, label, block: own_block });
        }
        Index { entries, types, workspace, lookups: Default::default() }
    }

    /// The path of an item for a message: with the generic arguments of its `impl` block.
    pub(crate) fn label(&self, path: &str) -> String {
        self.entries.iter().find(|entry| entry.item.path == path).map(|entry| entry.label.clone()).unwrap_or_else(|| path.to_string())
    }

    /// The parts of the path of an item with no twin mark (`#0`, `#1`), and with the generic
    /// arguments of its `impl` block (design 54.19). Two items with equal parts are items
    /// that a path cannot tell apart.
    pub(crate) fn normal(&self, path: &str) -> Option<Vec<Part>> {
        let entry = self.entries.iter().find(|entry| entry.item.path == path)?;
        let mut parts = entry.parts.clone();
        for part in &mut parts {
            match part {
                Part::Plain { twin, .. } | Part::Impl { twin, .. } => *twin = None,
            }
        }
        Some(parts)
    }

    /// The item of the workspace that a path names in an `impl` block. `root`: the path
    /// starts at a crate root. `None`: the name lookup gives no answer.
    fn lookup(&self, block: &Item, segments: &[String], root: bool) -> Option<String> {
        if root && !self.workspace.crates.iter().any(|krate| Some(&krate.name) == segments.first()) {
            return None;
        }
        let key = (if root { String::new() } else { block.path.clone() }, segments.to_vec());
        if let Some(known) = self.lookups.borrow().get(&key) {
            return known.clone();
        }
        let path: Vec<&str> = root.then_some(semilla_syntax::sites::ROOT).into_iter().chain(segments.iter().map(String::as_str)).collect();
        let found = crate::sites::path_item(self.workspace, &block.path, &path);
        self.lookups.borrow_mut().insert(key, found.clone());
        found
    }

    /// The answer of the name lookup to "do the two paths name the same item?" (design
    /// 54.18). `item`: the path in the header of the `impl` block. `None`: the lookup gives
    /// no item for the path of the block.
    fn same_item(&self, query: &Name, item: &Name, block: Option<&Item>) -> Option<bool> {
        let block = block?;
        let written: Vec<String> = item.start.iter().chain(&item.segments).cloned().collect();
        let found = self.lookup(block, &written, false)?;
        // A query that starts with `crate` or `self` is a path in the module of the block.
        let local = || {
            let asked: Vec<String> = query.start.iter().chain(&query.segments).cloned().collect();
            (!query.start.is_empty()).then(|| self.lookup(block, &asked, false)).flatten()
        };
        Some(match self.lookup(block, &query.segments, true).or_else(local) {
            Some(wanted) => wanted == found,
            // The query does not start at a crate root. It agrees if it is the end of the
            // path of the item, or the end of the text of the header.
            None => {
                let full: Vec<String> = found.split("::").map(str::to_string).collect();
                ends_agree(&query.segments, &full) && query.segments.len() <= full.len() || ends_agree(&query.segments, &item.segments) && query.segments.len() <= item.segments.len()
            }
        })
    }

    /// True if the trait of the query agrees with the trait of a part of the item.
    fn trait_agrees(&self, query: &Name, item: &Name, block: Option<&Item>) -> bool {
        arguments_agree(&query.arguments, &item.arguments) && self.same_item(query, item, block).unwrap_or_else(|| ends_agree(&query.segments, &item.segments))
    }

    /// True if the type of the query agrees with the type of a part of the item. `module`: the
    /// plain names of the path before the part. `block`: the `impl` block of the item.
    fn type_agrees(&self, query: &Name, item: &Name, module: &[String], block: Option<&Item>) -> bool {
        if !arguments_agree(&query.arguments, &item.arguments) {
            return false;
        }
        if let Some(same) = self.same_item(query, item, block) {
            return same;
        }
        if !ends_agree(&query.segments, &item.segments) {
            return false;
        }
        if query.segments.len() <= item.segments.len() {
            return true;
        }
        // The query gives more of the module path than the `impl` block does. The type is the
        // item of the name in the module of the block. If the module has no such item, it is
        // each type of the tree with the name. A type that the tree does not have agrees.
        let mut own: Vec<String> = module.to_vec();
        own.extend(item.segments.iter().cloned());
        let Some(known) = self.types.get(item.last()) else { return true };
        let known: Vec<&Vec<String>> = known.iter().filter(|path| ends_agree(path, &item.segments)).collect();
        if known.is_empty() {
            return true;
        }
        match known.iter().find(|path| ***path == own) {
            Some(path) => ends_agree(&query.segments, path),
            None => known.iter().any(|path| ends_agree(&query.segments, path)),
        }
    }

    /// True if one part of the query agrees with one part of the item.
    fn part_agrees(&self, query: &Part, item: &Part, module: &[String], block: Option<&Item>) -> bool {
        match (query, item) {
            (Part::Plain { name, arguments, twin }, Part::Plain { name: other, arguments: other_arguments, twin: other_twin }) => bare(name) == bare(other) && arguments_agree(arguments, other_arguments) && twin_agrees(twin, other_twin),
            // `Price::fmt` for `<Price as Display>::fmt`.
            (Part::Plain { name, arguments, twin }, Part::Impl { ty, block: false, twin: other_twin, .. }) => name == ty.last() && arguments_agree(arguments, &ty.arguments) && twin_agrees(twin, other_twin),
            (Part::Impl { ty, tr, block: block_only, twin }, Part::Impl { ty: other_ty, tr: other_tr, block: other_block, twin: other_twin }) => {
                let traits = match (tr, other_tr) {
                    (Some(a), Some(b)) => self.trait_agrees(a, b, block),
                    (None, None) => true,
                    _ => false,
                };
                (!*block_only || *other_block) && traits && self.type_agrees(ty, other_ty, module, block) && twin_agrees(twin, other_twin)
            }
            // `<Price>::new` for `Price::new`.
            (Part::Impl { ty, tr: None, block: false, twin }, Part::Plain { name, arguments, twin: other_twin }) => {
                let mut own: Vec<String> = module.to_vec();
                own.push(name.clone());
                ty.last() == name && ends_agree(&ty.segments, &own) && ty.segments.len() <= own.len() && arguments_agree(&ty.arguments, arguments) && twin_agrees(twin, other_twin)
            }
            _ => false,
        }
    }

    /// The plain names of the parts before `end`.
    fn module(parts: &[Part], end: usize) -> Vec<String> {
        parts[..end].iter().filter_map(|part| if let Part::Plain { name, .. } = part { Some(name.clone()) } else { None }).collect()
    }

    /// True if each part of the query agrees with the part of the item at the same place from
    /// the end.
    fn agrees(&self, query: &[Part], entry: &Entry) -> bool {
        if query.is_empty() || query.len() > entry.parts.len() {
            return false;
        }
        let shift = entry.parts.len() - query.len();
        query.iter().enumerate().all(|(index, part)| self.part_agrees(part, &entry.parts[shift + index], &Self::module(&entry.parts, shift + index), entry.block))
    }

    /// The count of the parts of the query that agree with the item, for the order of the
    /// candidates. A part `<TYPE as TRAIT>` counts for the type and for the trait.
    fn score(&self, query: &[Part], entry: &Entry) -> usize {
        let mut score = 0;
        for (back, part) in query.iter().rev().enumerate() {
            let Some(at) = entry.parts.len().checked_sub(back + 1) else { break };
            let module = Self::module(&entry.parts, at);
            score += match (part, &entry.parts[at]) {
                (Part::Impl { ty, tr, .. }, Part::Impl { ty: other_ty, tr: other_tr, .. }) => {
                    let traits = tr.as_ref().zip(other_tr.as_ref()).is_some_and(|(a, b)| self.trait_agrees(a, b, entry.block));
                    usize::from(self.type_agrees(ty, other_ty, &module, entry.block)) + usize::from(traits)
                }
                (Part::Impl { ty, .. }, Part::Plain { name, .. }) => usize::from(ty.last() == name),
                (Part::Plain { name, .. }, Part::Impl { ty, .. }) => usize::from(name == ty.last()),
                (part, other) => usize::from(self.part_agrees(part, other, &module, entry.block)),
            };
        }
        score
    }

    /// The items that the path names: the item with the exact path, else each item that
    /// agrees with the parts. The best item is first.
    pub(crate) fn find(&self, query: &str) -> Vec<&'a str> {
        if let Some(item) = self.workspace.items.get(query.trim()).filter(|item| item.kind != ItemKind::Use) {
            return vec![item.path.as_str()];
        }
        let parts = parse(query);
        let found: Vec<&Entry<'a>> = self.entries.iter().filter(|entry| self.agrees(&parts, entry)).collect();
        // A field and a method of one type can have one name. The mark of the last part
        // selects: `Cart::total` is the method, and `Cart.total` is the field. A path with
        // the other mark names the item only if no item has the mark of the path.
        let same_mark = |entry: &&Entry| match (parts.last(), entry.parts.last()) {
            (Some(Part::Plain { name, .. }), Some(Part::Plain { name: other, .. })) => name.starts_with('.') == other.starts_with('.'),
            _ => true,
        };
        let exact: Vec<&Entry<'a>> = found.iter().copied().filter(same_mark).collect();
        self.ordered(&parts, if exact.is_empty() { found } else { exact })
    }

    /// The count of the parts of a path of the agent that agree with the item `path`.
    pub(crate) fn rank(&self, query: &str, path: &str) -> usize {
        let parts = parse(query);
        self.entries.iter().find(|entry| entry.item.path == path).map(|entry| self.score(&parts, entry)).unwrap_or(0)
    }

    fn ordered(&self, parts: &[Part], mut found: Vec<&Entry<'a>>) -> Vec<&'a str> {
        found.sort_by_key(|entry| (std::cmp::Reverse(self.score(parts, entry)), entry.item.path.as_str()));
        found.iter().map(|entry| entry.item.path.as_str()).collect()
    }

    /// The candidates for a path that names no item: each item whose last part has the last
    /// name of the query. The item with the most parts that agree is first.
    pub(crate) fn near(&self, query: &str) -> Vec<&'a str> {
        let parts = parse(query);
        let Some(last) = parts.last() else { return Vec::new() };
        let same_name = |entry: &&Entry| match (last, entry.parts.last()) {
            (Part::Plain { name, .. }, Some(Part::Plain { name: other, .. })) => bare(name) == bare(other),
            (Part::Impl { ty, tr, .. }, Some(Part::Impl { ty: other_ty, tr: other_tr, .. })) => ty.last() == other_ty.last() || tr.as_ref().zip(other_tr.as_ref()).is_some_and(|(a, b)| a.last() == b.last()),
            (Part::Impl { ty, .. }, Some(Part::Plain { name, .. })) => ty.last() == name,
            _ => false,
        };
        self.ordered(&parts, self.entries.iter().filter(same_name).collect())
    }

    /// The macro calls that can make the item of a path with no item in the tree: each call
    /// whose input has the name of the type or of the trait of the path. The result has the
    /// path of the call and the name that the input has.
    pub(crate) fn makers(&self, query: &str) -> Vec<(&'a str, String)> {
        let parts = parse(query);
        // The names of the query that can be a type or a trait: each name that is no item
        // name at the end of the path.
        let mut names: Vec<String> = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            match part {
                Part::Impl { ty, tr, .. } => {
                    names.push(ty.last().to_string());
                    names.extend(tr.iter().map(|tr| tr.last().to_string()));
                }
                Part::Plain { name, .. } if index + 2 == parts.len() || parts.len() == 1 => names.push(bare(name).to_string()),
                _ => {}
            }
        }
        names.retain(|name| !name.is_empty() && name.chars().all(word));
        let mut found = Vec::new();
        for entry in self.entries.iter().filter(|entry| entry.item.kind == ItemKind::Macro && entry.item.path.rsplit("::").next().is_some_and(|last| last.contains("!#"))) {
            let has = |name: &String| entry.item.text.match_indices(name.as_str()).any(|(at, _)| !entry.item.text[..at].chars().last().is_some_and(word) && !entry.item.text[at + name.len()..].chars().next().is_some_and(word));
            if let Some(name) = names.iter().find(|name| has(name)) {
                found.push((entry.item.path.as_str(), name.clone()));
            }
        }
        found
    }
}

/// The segments of a path of Semilla: the text between each `::` that is in no bracket.
fn split_path(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' if index > 0 && bytes[index - 1] == b'-' => {}
            b'>' | b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => {
                out.push(&path[start..index]);
                index += 2;
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    out.push(&path[start..]);
    out
}

/// At most 8 candidates as one text, then the count of the others.
pub(crate) fn list(candidates: &[String]) -> String {
    let mut text = candidates.iter().take(8).map(|path| format!("`{path}`")).collect::<Vec<_>>().join(", ");
    if candidates.len() > 8 {
        text.push_str(&format!(", and {} more", candidates.len() - 8));
    }
    text
}
