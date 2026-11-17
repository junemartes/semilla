//! The symbol links of a step (design 44).
//!
//! At N0, rustc copies no body into a different body. So the compiled code of a body can change
//! with no edit at the place of an element only through a symbol that the element names (the
//! driver records them, `symbols.rs`). This module finds the changed definitions of the step and
//! the edits that change each one:
//!
//! 1. Each edit of the syntax diff changes the item of its anchor, on each side. These edits change
//!    no definition: a doc, a comment, a visibility, a file, a harmless attribute; a rename or a
//!    move that the item map derives (N1); an edit with the same meaning: a new order of the
//!    members of an `impl` or a trait, a new order of bounds ("order only", "moved to the `where`
//!    clause"), changed source text with no other fact.
//! 2. An edit in the body of a function does not change the function for its callers: at N0 the
//!    callers do not change. Two exceptions: a `const fn` that a constant evaluates (directly or
//!    through a chain of `const fn` calls), and a function with an `impl Trait` type in its
//!    signature (the type behind it comes from the body).
//! 3. A change of a field or of a variant is also a change of its type. An added or a removed
//!    member of an `impl` is also a change of the `impl`. A changed `derive` list changes the
//!    impl set of its type: only a symbol of an impl (a call that resolves to it, a drop, a
//!    projection that it normalizes) follows it, not a mention of the type.
//! 4. A definition that names a changed definition is changed through it (the definition records
//!    of the driver): `const A: u32 = B + 1;` with a changed `B`. The chain has at most
//!    [`MAX_DEPTH`] steps. The cause is the edit at the end of the chain.
//! 5. A `use` item is a cause only when the target of its name differs on the two sides after
//!    the rename and move map: a new order of a `use` list, or a new path to a moved item, is no
//!    cause. A name with no `use` item on the other side resolves to the item of the module
//!    with the name.
//!
//! The item of a symbol is the smallest item of its side whose range contains the place of the
//! definition. The rename and move map gives an old item its new path, so one definition has one
//! key on the two sides. A symbol inside an `impl` or a trait with no item of its own (an
//! associated constant or type) is changed by the edits of the container at its place.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use semilla_syntax::rs;
use semilla_syntax::tree::Pos;
use semilla_syntax::{Edit, EditKind, ItemKind, Workspace};

use crate::{DefMap, Sym};

/// A symbol of the value of a constant or a static: rustc evaluates it at compile time.
pub const EVALUATED: u8 = 1;
/// A symbol of the body of a `const fn`: it counts only when a constant evaluates the function.
pub const BODY: u8 = 2;
/// The self type of an `impl`.
pub const SELF_TYPE: u8 = 4;
/// A target of a `use` item.
pub const USE_TARGET: u8 = 8;
/// The name that a `use` item imports (a symbol with no place).
pub const USE_NAME: u8 = 16;

/// The most steps of a chain of definitions (Decided by Claude, design 44).
pub const MAX_DEPTH: usize = 4;

/// The edits that change one definition.
#[derive(Clone, Debug, Default)]
struct Change {
    /// The edits of the definition.
    edits: BTreeSet<usize>,
    /// The edits of the body of a function (rule 2).
    body: BTreeSet<usize>,
    /// The edits of the impl set of a type: a changed `derive` list (rule 3).
    impls: BTreeSet<usize>,
}

/// The changed definitions of a step, and the edits that change each symbol.
pub struct Links<'a> {
    sides: [&'a Workspace; 2],
    edits: &'a [Edit],
    defs: [&'a DefMap; 2],
    /// The old path → the new path of each item that N1 maps, and of each derived move.
    renames: BTreeMap<String, String>,
    /// The changed definitions by their key (the path after the step).
    changed: BTreeMap<String, Change>,
    /// The functions with an `impl Trait` type in the signature, by key.
    impl_trait: BTreeSet<String>,
    /// The items of each side by file: the start, the end, and the path.
    items: [BTreeMap<String, Vec<(Pos, Pos, String)>>; 2],
    lines: RefCell<[BTreeMap<String, Option<rs::Lines>>; 2]>,
    memo: RefCell<HashMap<(usize, String, bool, usize), Vec<usize>>>,
}

/// True for an edit that can change a definition (rule 1).
fn counts(edit: &Edit) -> bool {
    match edit.kind {
        EditKind::Docs | EditKind::Comment | EditKind::Visibility | EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged => false,
        EditKind::Attrs => !crate::facts::harmless_attribute_edit(edit),
        _ => true,
    }
}

/// True for an attribute edit that changes only a `derive` list (and harmless attributes): it
/// changes the impl set of the type, not its content (rule 3).
fn derive_only(edit: &Edit) -> bool {
    let names = crate::facts::changed_attributes(edit);
    edit.kind == EditKind::Attrs && names.iter().any(|name| name == "derive") && names.iter().all(|name| name == "derive" || crate::facts::harmless_attribute_name(name))
}

/// True for the symbol of an `impl` (the driver path `<T as Tr>` or `m::<impl T>`): a call
/// that resolves to it, a drop, or a projection that it normalizes names it (rule 3).
fn is_impl(symbol: &Sym) -> bool {
    symbol.path.ends_with('>')
}

/// The module and the name of a `use` item: the def path up to `::{use#`.
fn use_module(path: &str) -> Option<&str> {
    path.find("::{use#").map(|at| &path[..at])
}

/// True for an edit in the body of a function (rule 2): its anchor has a path that is not in
/// the signature.
fn in_body(ws: &Workspace, anchor: &semilla_core::Anchor) -> bool {
    let function = ws.items.get(&anchor.item).is_some_and(|item| item.kind == ItemKind::Fn);
    function && anchor.path.first().is_some_and(|first| first.to_string() != "sig")
}

/// The parents of a member: the type of a field (`m::P.x` → `m::P`), the variant and the enum of a
/// variant field (`m::E::A.x`), the enum of a variant (`m::E::A`).
fn parents(ws: &Workspace, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = path.to_string();
    loop {
        let Some(item) = ws.items.get(&current) else { break };
        let parent = match item.kind {
            ItemKind::Field => current.rsplit_once('.').map(|(owner, _)| owner.to_string()),
            ItemKind::Variant => current.rsplit_once("::").map(|(owner, _)| owner.to_string()),
            _ => None,
        };
        let Some(parent) = parent.filter(|parent| ws.items.contains_key(parent)) else { break };
        out.push(parent.clone());
        current = parent;
    }
    out
}

impl<'a> Links<'a> {
    /// The changed definitions of a step. `refactoring`: the edits of the rename and move facts
    /// (rule 1).
    pub fn new(before: &'a Workspace, after: &'a Workspace, edits: &'a [Edit], refactoring: &BTreeSet<usize>, renames: BTreeMap<String, String>, defs: [&'a DefMap; 2]) -> Links<'a> {
        let sides = [before, after];
        let mut items: [BTreeMap<String, Vec<(Pos, Pos, String)>>; 2] = [BTreeMap::new(), BTreeMap::new()];
        for (side, ws) in sides.iter().enumerate() {
            for item in ws.items.values() {
                items[side].entry(item.file.clone()).or_default().push(((item.line_start, item.col_start), (item.line_end, item.col_end), item.path.clone()));
            }
            for list in items[side].values_mut() {
                // The smallest item first: the fewest lines, then the fewest columns.
                list.sort_by_key(|(start, end, path)| (end.0 - start.0, end.1.abs_diff(start.1), path.clone()));
            }
        }
        let mut links = Links { sides, edits, defs, renames, changed: BTreeMap::new(), impl_trait: BTreeSet::new(), items, lines: RefCell::new([BTreeMap::new(), BTreeMap::new()]), memo: RefCell::new(HashMap::new()) };
        for ws in sides {
            for item in ws.items.values().filter(|item| item.kind == ItemKind::Fn) {
                let output = item.signature.split_once("->").map(|(_, output)| output).unwrap_or("");
                if crate::hints::words(output).contains(&"impl") {
                    links.impl_trait.insert(item.path.clone());
                }
            }
        }
        for (index, edit) in edits.iter().enumerate() {
            if !counts(edit) || refactoring.contains(&index) {
                continue;
            }
            for (side, anchor) in [&edit.before, &edit.after].into_iter().enumerate() {
                let Some(anchor) = anchor else { continue };
                let ws = sides[side];
                let key = links.key(side, &anchor.item);
                let entry = links.changed.entry(key).or_default();
                if in_body(ws, anchor) {
                    entry.body.insert(index);
                    continue;
                }
                if derive_only(edit) {
                    entry.impls.insert(index);
                    continue;
                }
                entry.edits.insert(index);
                for parent in parents(ws, &anchor.item) {
                    let key = links.key(side, &parent);
                    links.changed.entry(key).or_default().edits.insert(index);
                }
                // An added or a removed member of an `impl` (rule 3).
                if matches!(edit.kind, EditKind::ItemAdded | EditKind::ItemRemoved)
                    && let Some(item) = ws.items.get(&anchor.item)
                {
                    let container = ws.items.values().filter(|other| other.kind == ItemKind::Impl && other.file == item.file && other.path != item.path && (other.line_start, other.col_start) <= (item.line_start, item.col_start) && (item.line_end, item.col_end) <= (other.line_end, other.col_end)).min_by_key(|other| other.line_end - other.line_start);
                    if let Some(container) = container {
                        let key = links.key(side, &container.path);
                        links.changed.entry(key).or_default().edits.insert(index);
                    }
                }
            }
        }
        links
    }

    /// The key of a definition: its path after the step.
    fn key(&self, side: usize, path: &str) -> String {
        if side == 0 {
            if let Some(new) = self.renames.get(path) {
                return new.clone();
            }
        }
        path.to_string()
    }

    /// The positions of a byte range of a file on one side.
    fn positions(&self, side: usize, place: &crate::MirPlace) -> Option<(Pos, Pos)> {
        let mut lines = self.lines.borrow_mut();
        let found = lines[side].entry(place.0.clone()).or_insert_with(|| self.sides[side].rust_files.get(&place.0).map(|text| rs::Lines::new(text)));
        let found = found.as_ref()?;
        (place.2 <= found.text().len() && place.1 <= place.2).then(|| (found.pos(place.1), found.pos(place.2)))
    }

    /// The path of the smallest item of a side that contains the place of a symbol.
    fn item_of(&self, side: usize, symbol: &Sym) -> Option<String> {
        let place = symbol.place.as_ref()?;
        let (start, end) = self.positions(side, place)?;
        let list = self.items[side].get(&place.0)?;
        list.iter().find(|(from, to, _)| *from <= start && end <= *to).or_else(|| list.iter().find(|(from, to, _)| *from <= start && start < *to)).map(|(_, _, path)| path.clone())
    }

    /// The edits that change a symbol on one side (with the chain of rule 4). `evaluated`: a
    /// constant evaluates the symbol, so the body of a `const fn` counts.
    pub fn edits_of(&self, side: usize, symbol: &Sym) -> Vec<usize> {
        self.edits_at(side, symbol, false, 0)
    }

    fn edits_at(&self, side: usize, symbol: &Sym, evaluated: bool, depth: usize) -> Vec<usize> {
        let memo_key = (side, format!("{}@{:?}", symbol.path, symbol.place), evaluated, depth);
        if let Some(found) = self.memo.borrow().get(&memo_key) {
            return found.clone();
        }
        // A cycle of definitions stops at the memo entry.
        self.memo.borrow_mut().insert(memo_key.clone(), Vec::new());
        let mut found: BTreeSet<usize> = BTreeSet::new();
        // Rule 5: a `use` item with the same target on the two sides is no cause.
        let same_use = use_module(&symbol.path).is_some() && self.same_use_target(side, symbol);
        if let Some(item) = self.item_of(side, symbol).filter(|_| !same_use) {
            let ws = self.sides[side];
            let container = ws.items.get(&item).is_some_and(|it| matches!(it.kind, ItemKind::Impl | ItemKind::Trait | ItemKind::Mod));
            let inside = container && symbol.place.as_ref().zip(ws.items.get(&item)).is_some_and(|(place, it)| self.positions(side, place).is_some_and(|(start, end)| (start, end) != ((it.line_start, it.col_start), (it.line_end, it.col_end))));
            if inside {
                // An associated item with no item of its own: the edits of the container at its
                // place.
                if let Some((start, end)) = symbol.place.as_ref().and_then(|place| self.positions(side, place)) {
                    for (index, edit) in self.edits.iter().enumerate() {
                        let anchor = if side == 0 { &edit.before } else { &edit.after };
                        let Some(anchor) = anchor.as_ref().filter(|anchor| anchor.item == item) else { continue };
                        if !self.changed.get(&self.key(side, &item)).is_some_and(|change| change.edits.contains(&index)) {
                            continue;
                        }
                        let place = match semilla_syntax::resolve(ws, anchor) {
                            Some(semilla_syntax::Resolved::Node(_, node)) => Some((node.start, node.end)),
                            _ => None,
                        };
                        if place.is_none_or(|(from, to)| from < end && start < to) {
                            found.insert(index);
                        }
                    }
                }
            } else if let Some(change) = self.changed.get(&self.key(side, &item)) {
                found.extend(change.edits.iter().copied());
                if is_impl(symbol) {
                    found.extend(change.impls.iter().copied());
                }
                if evaluated || self.impl_trait.contains(&self.key(side, &item)) || self.impl_trait.contains(&item) {
                    found.extend(change.body.iter().copied());
                }
            }
        }
        // Rule 4: the chain of definitions.
        if depth < MAX_DEPTH {
            for (named, flag) in self.defs[side].get(&symbol.path).into_iter().flat_map(|def| &def.named) {
                if flag & BODY != 0 && !evaluated || flag & (USE_TARGET | USE_NAME | SELF_TYPE) != 0 {
                    continue;
                }
                let next = flag & EVALUATED != 0 || (evaluated && flag & BODY != 0);
                found.extend(self.edits_at(side, named, next, depth + 1));
            }
        }
        let found: Vec<usize> = found.into_iter().collect();
        self.memo.borrow_mut().insert(memo_key, found.clone());
        found
    }

    /// True if the name of a `use` item has the same targets on the two sides, after the rename
    /// and move map (rule 5).
    fn same_use_target(&self, side: usize, symbol: &Sym) -> bool {
        let Some(module) = use_module(&symbol.path) else { return false };
        let Some(def) = self.defs[side].get(&symbol.path) else { return false };
        let Some(name) = def.named.iter().find(|(_, flag)| flag & USE_NAME != 0).map(|(name, _)| name.path.clone()) else { return false };
        let targets = |side: usize, path: &str| -> BTreeSet<String> {
            self.defs[side].get(path).into_iter().flat_map(|def| &def.named).filter(|(_, flag)| flag & USE_TARGET != 0).map(|(target, _)| self.item_of(side, target).map(|item| self.key(side, &item)).unwrap_or_else(|| self.key(side, &target.path))).collect()
        };
        let own = targets(side, &symbol.path);
        let other = 1 - side;
        // The name with no `use` item on the other side: the definition of the module with the
        // name (an added `use` line for a moved item, fix round 2, R2).
        let local = |module: &str| -> Option<String> {
            let path = format!("{module}::{name}");
            self.sides[other].items.contains_key(&path).then(|| self.key(other, &path))
        };
        let module_other = if side == 0 { self.renames.get(module).cloned().unwrap_or_else(|| module.to_string()) } else { self.renames.iter().find(|(_, new)| *new == module).map(|(old, _)| old.clone()).unwrap_or_else(|| module.to_string()) };
        let mut found = false;
        for (path, def) in self.defs[other].iter().filter(|(path, _)| use_module(path) == Some(module_other.as_str())) {
            if def.named.iter().any(|(named, flag)| flag & USE_NAME != 0 && named.path == name) {
                if targets(other, path) != own {
                    return false;
                }
                found = true;
            }
        }
        if !found && let Some(key) = local(&module_other) {
            return own.len() == 1 && own.contains(&key);
        }
        found
    }

    /// The edits that change the symbols of an element, with the symbol of each edit.
    /// `evaluated`: rustc evaluates the body of the element at compile time (a constant).
    pub fn element(&self, side: usize, element: &crate::MirElement, evaluated: bool) -> BTreeMap<usize, BTreeSet<String>> {
        let mut out: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
        for symbol in &element.symbols {
            // The text of the "through:" line: the key of the definition (one key for the two
            // sides of a rename or a move), else the def path.
            // A symbol inside an `impl`, a trait, or a module with no item of its own (an
            // associated constant) shows its def path.
            let container = |item: &String| self.sides[side].items.get(item).is_some_and(|it| matches!(it.kind, ItemKind::Impl | ItemKind::Trait | ItemKind::Mod));
            let shown = self.item_of(side, symbol).filter(|item| !container(item)).map(|item| self.key(side, &item)).unwrap_or_else(|| symbol.path.clone());
            for edit in self.edits_at(side, symbol, evaluated, 0) {
                out.entry(edit).or_default().insert(shown.clone());
            }
        }
        out
    }
}
