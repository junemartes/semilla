//! The rules of design 70 (batch 11b): the names and the identity of each fact and each spotlight
//! come from the name record of the compiler, never from the text.
//!
//! The syntax tree only finds the place of an edit: the file and the byte range of a node or of
//! an item. The record of the tree joins that place to the data of the compiler (R70.1-1). A
//! place that the compiler did not build (code under a false `cfg`, a macro definition, a tree
//! with no record) has no answer of the record: the text rules decide there (R70.1-1). When the
//! record and the text disagree, the line shows (the safe side), with the cause.

use semilla_core::Anchor;
use semilla_core::namerec::{Def, NameIndex, Target};
use semilla_syntax::{Edit, Item, Workspace};

/// The file and the byte range of an item of the syntax tree.
pub(crate) fn item_place(workspace: &Workspace, item: &Item) -> Option<(String, usize, usize)> {
    let text = workspace.rust_files.get(&item.file)?;
    let start = semilla_core::namerec::offset(text, item.line_start, item.col_start)?;
    let end = semilla_core::namerec::offset(text, item.line_end, item.col_end)?;
    Some((item.file.clone(), start, end))
}

/// The file and the byte range of the place of an anchor: its node, or its item.
pub(crate) fn anchor_place(workspace: &Workspace, anchor: &Anchor) -> Option<(String, usize, usize)> {
    let item = workspace.items.get(&anchor.item)?;
    if !anchor.path.is_empty()
        && let Some(node) = semilla_syntax::resolve_node(workspace, anchor)
    {
        return Some((item.file.clone(), node.span.0, node.span.1));
    }
    item_place(workspace, item)
}

/// The last name of the path of an item, with no twin suffix (`m::T.f#1` → `f`).
fn own_name(path: &str) -> &str {
    let last = path.rsplit(['.', ':']).next().unwrap_or(path);
    last.split('#').next().unwrap_or(last)
}

/// The definition of the record for an item of the syntax tree (R70.1-1): the definition with
/// the name of the item whose name is in the place of the item. An impl and each item with no
/// name of its own: the outermost impl whose place starts in the place of the item.
pub(crate) fn item_def(workspace: &Workspace, names: &NameIndex, path: &str) -> Option<Def> {
    let item = workspace.items.get(path)?;
    let (file, start, end) = item_place(workspace, item)?;
    let impl_item = item.kind == semilla_syntax::ItemKind::Impl;
    names.def_of(&file, start, end, own_name(path), impl_item).cloned()
}

/// The place of an edit in one tree, for a spotlight: the node of a statement edit (the item
/// for an edit of a full body); the full item of an added or a removed item; the head of the
/// item (its outer attributes, its qualifiers, and its name, not its body) for an edit of the
/// attributes, the visibility, the signature, or the members of the item. A comment and a doc
/// text are no code: `None` (the text rule).
fn edit_place(workspace: &Workspace, edit: &Edit, anchor: &Anchor, names: &NameIndex) -> Option<(String, usize, usize)> {
    use semilla_syntax::EditKind;
    match edit.kind {
        EditKind::Comment | EditKind::Docs => None,
        EditKind::Attrs | EditKind::Visibility | EditKind::Signature | EditKind::Members if anchor.path.is_empty() => {
            let item = workspace.items.get(&anchor.item)?;
            let (file, start, end) = item_place(workspace, item)?;
            let def = item_def(workspace, names, &anchor.item)?;
            let head = def.ident.or(def.span.map(|(s, _)| (s, s + 1)))?.1;
            Some((file, start, head.min(end)))
        }
        _ => anchor_place(workspace, anchor),
    }
}

/// The answer of the record of one tree for the place of an edit: `None` if the tree has no
/// record, or if the compiler did not build code at the place.
fn side_answer(workspace: &Workspace, edit: &Edit, anchor: Option<&Anchor>, ask: &dyn Fn(&NameIndex, &str, usize, usize) -> bool) -> Option<bool> {
    let names = workspace.names.get()?;
    let (file, start, end) = edit_place(workspace, edit, anchor?, &names)?;
    if !names.built(&file, start, end) && !names.built(&file, start, start + 1) && !names.built(&file, end.saturating_sub(1), end) {
        return None;
    }
    Some(ask(&names, &file, start, end))
}

/// The decision of a spotlight from the record and from the text (R70.2-1 (a)).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// No line.
    No,
    /// The line, from the record or from the text where the tree has no record.
    Yes,
    /// The line, though the record of the built code has no such code: the text has it. The
    /// line keeps the safe side and says why.
    Text,
}

/// Decides a spotlight of an edit: the record of each tree for the place of the edit, and the
/// text answer of each tree (`text[0]`: before, `text[1]`: after). A tree whose place has no
/// record uses its text answer.
pub(crate) fn spotlight(edit: &Edit, trees: [&Workspace; 2], text: [bool; 2], ask: &dyn Fn(&NameIndex, &str, usize, usize) -> bool) -> Decision {
    let answers = [side_answer(trees[0], edit, edit.before.as_ref(), ask), side_answer(trees[1], edit, edit.after.as_ref(), ask)];
    if answers.contains(&Some(true)) {
        return Decision::Yes;
    }
    if answers.iter().zip(text).any(|(answer, text)| answer.is_none() && text) {
        return Decision::Yes;
    }
    if text.contains(&true) {
        return Decision::Text;
    }
    Decision::No
}

/// R70.2-1 (a): the `unsafe` state of a place: an `unsafe` block that the author wrote, or the name
/// of an `unsafe fn`, an `unsafe impl`, or an `unsafe trait`.
pub(crate) fn unsafe_at(names: &NameIndex, file: &str, start: usize, end: usize) -> bool {
    names.unsafe_in(file, start, end)
}

/// R70.2-1 (a): a `cfg` or a `cfg_attr` attribute that the compiler evaluated at a place. A
/// `cfg(test)` only selects test code: it is not a part of the rule.
pub(crate) fn cfg_at(names: &NameIndex, file: &str, start: usize, end: usize) -> bool {
    names.cfgs_in(file, start, end).any(|cfg| cfg.text != "test")
}

/// R70.2-1 (a): the `unsafe` state of an item with a body, for the line of a fact (design 36.5):
/// `Some` with the answer of the record, `None` if the tree has no record of the item.
pub(crate) fn item_unsafe(workspace: &Workspace, path: &str) -> Option<bool> {
    let names = workspace.names.get()?;
    let item = workspace.items.get(path)?;
    let (file, start, end) = item_place(workspace, item)?;
    if !names.built(&file, start, end) && item_def(workspace, &names, path).is_none() {
        return None;
    }
    Some(names.unsafe_in(&file, start, end))
}

/// R70.2-1 (b): the effective visibility of an item by the compiler: `Some(true)` if a different
/// crate can use it, `Some(false)` if no different crate can, `None` if a unit of a library of
/// the tree does not have it (no record, or code that the configuration does not build).
pub(crate) fn item_public(workspace: &Workspace, path: &str) -> Option<bool> {
    let names = workspace.names.get()?;
    let def = item_def(workspace, &names, path)?;
    def.in_library.then_some(def.public)
}

/// The names that a false `cfg` removes in the tree, among `names` (the safe side of R70.2-1 (b) and
/// R70.2-1 (c)): an item with one of these names exists in another configuration.
pub(crate) fn removed_name<'a>(workspace: &Workspace, names: impl IntoIterator<Item = &'a String>) -> Option<&'a String> {
    let index = workspace.names.get()?;
    names.into_iter().find(|name| index.removes(name))
}

/// R70.2-1 (d): the trait reference of an impl of the syntax tree, by the compiler
/// (`<X as core::ops::arith::Add>`): the identity of the impl.
pub(crate) fn impl_trait_ref(workspace: &Workspace, path: &str) -> Option<String> {
    let names = workspace.names.get()?;
    item_def(workspace, &names, path)?.trait_ref
}

/// R70.2-1 (e): the item of the trait that an item of a trait impl gives, by the compiler: the
/// path of the item of the trait, and true if it has a default. `Ok(None)`: the item is not an
/// item of a trait impl. `Err`: the tree has no record of the item (the text rule decides).
pub(crate) fn trait_item_of(workspace: &Workspace, path: &str) -> Result<Option<(String, bool)>, ()> {
    let names = workspace.names.get().ok_or(())?;
    let def = item_def(workspace, &names, path).ok_or(())?;
    Ok(def.of.map(|of| (of, def.of_default)))
}

/// R70.2-1 (f): the last names of the attributes that the compiler keeps on the item of each anchor
/// with no meaning of its own (a derive helper attribute, a tool attribute): such an attribute
/// is no macro call. `None`: no tree has a record of the item (the name list decides).
pub(crate) fn inert_attributes(trees: [&Workspace; 2], anchors: [Option<&Anchor>; 2]) -> Option<std::collections::BTreeSet<String>> {
    let mut found = std::collections::BTreeSet::new();
    let mut answered = false;
    for (workspace, anchor) in trees.into_iter().zip(anchors) {
        let Some(anchor) = anchor else { continue };
        let Some(names) = workspace.names.get() else { continue };
        let Some(def) = item_def(workspace, &names, &anchor.item) else { continue };
        answered = true;
        found.extend(def.inert);
    }
    answered.then_some(found)
}

/// R70.2-1 (e): true if the item of a trait with the def path has a default in the tree, by the
/// record. `None`: the record has no such definition with a place (a trait of another crate).
pub(crate) fn trait_default(workspace: &Workspace, def_path: &str) -> Option<bool> {
    let names = workspace.names.get()?;
    names.defs.iter().find(|def| def.path == def_path && def.span.is_some()).map(|def| def.of_default)
}

/// The item of the syntax tree of a definition of the record, by its def path: the smallest item
/// with the name of the definition whose place has the name of the definition.
pub(crate) fn item_of_def(workspace: &Workspace, def_path: &str) -> Option<String> {
    let names = workspace.names.get()?;
    let def = names.defs.iter().find(|def| def.path == def_path && def.ident.is_some())?;
    let (file, (start, end)) = (def.file.as_ref()?, def.ident?);
    workspace
        .items
        .values()
        .filter(|item| &item.file == file && own_name(&item.path) == def.name)
        .filter(|item| item_place(workspace, item).is_some_and(|(_, s, e)| s <= start && end <= e))
        .min_by_key(|item| item.line_end - item.line_start)
        .map(|item| item.path.clone())
}

/// The count of the identifier tokens `word` in a token text (a comment and a doc comment are
/// no identifier). `None` if the text does not parse as tokens.
fn ident_count(text: &str, word: &str) -> Option<usize> {
    use semilla_syntax::tt::{TokenStream, TokenTree};
    fn walk(stream: TokenStream, word: &str) -> usize {
        stream
            .into_iter()
            .map(|tree| match tree {
                TokenTree::Ident(ident) => usize::from(ident.to_string().trim_start_matches("r#") == word),
                TokenTree::Group(group) => walk(group.stream(), word),
                _ => 0,
            })
            .sum()
    }
    Some(walk(text.parse::<TokenStream>().ok()?, word))
}

/// R70.2-1 (c): one use of the note "this body uses …, which Semilla did not check" of the item
/// `path` (design 35), by the name record of its tree. The note names the code under a `cfg`
/// that a body uses. A use that resolves to a definition that the configuration builds is code
/// that the check of this configuration has: it gives no note (`None`). A definition that the
/// configuration does not build cannot be the target of a resolved path, so a use of the text
/// stays only if the record does not resolve each use of its name in the item; the use then
/// says so. With no record, each use stays (the text rule, R70.1-1).
///
/// The uses with a name: "`NAME` (CAUSE)" and "static `NAME` (CAUSE)". A path with `::` comes
/// from the driver (the compiler resolved it in this configuration). A use with no name (a `cfg`
/// attribute or a macro call of the body, "code that …") stays: it is code of the body itself.
pub(crate) fn cfg_use(trees: [&Workspace; 2], path: &str, entry: &str) -> Option<String> {
    // The uses come from the two versions of the body: the tree whose item has the name in its
    // text decides (the tree after the step first).
    let [first, second] = trees;
    let named = |workspace: &Workspace| -> bool {
        let rest = entry.strip_prefix("static ").unwrap_or(entry);
        let name = rest.strip_prefix('`').and_then(|tail| tail.split_once("` (")).map(|(name, _)| name).unwrap_or("");
        workspace.items.get(path).is_some_and(|item| ident_count(&item.text, name).unwrap_or(0) > 0)
    };
    let workspace = if !named(first) && named(second) { second } else { first };
    cfg_use_in(workspace, path, entry)
}

/// [`cfg_use`] in one tree.
fn cfg_use_in(workspace: &Workspace, path: &str, entry: &str) -> Option<String> {
    let keep = || Some(entry.to_string());
    if workspace.names.get().is_none() {
        return keep();
    }
    let rest = entry.strip_prefix("static ").unwrap_or(entry);
    let Some((name, _)) = rest.strip_prefix('`').and_then(|tail| tail.split_once("` (")) else { return keep() };
    if name.is_empty() || name.contains(['!', '#', '(', '"']) {
        return keep();
    }
    let say = |why: &str| Some(match entry.rfind(')') {
        Some(at) => format!("{}; {why}){}", &entry[..at], &entry[at + 1..]),
        None => format!("{entry} ({why})"),
    });
    if name.contains("::") {
        // A definition that the driver resolved in this configuration. A type keeps its use if
        // the syntax tree has a part of it with a `cfg` that the record does not have: code
        // that this configuration does not build (an `impl Drop` under a false `cfg`).
        let names = workspace.names.get()?;
        return unbuilt_part(workspace, &names, name.split('<').next().unwrap_or(name).trim()).then(|| say("the configuration does not build a part of it")).flatten();
    }
    let said = || say("the record of the compiler does not resolve each use of the name");
    let names = workspace.names.get()?;
    let Some(item) = workspace.items.get(path) else { return keep() };
    let Some((file, start, end)) = item_place(workspace, item) else { return said() };
    // The uses of the name in the text: its identifiers less the names of the definitions in
    // the item (the name of the item itself is no use).
    let defined = names.defs_in(&file).filter(|def| def.name == name && def.ident.is_some_and(|(s, e)| start <= s && e <= end)).count();
    let count = ident_count(&item.text, name).unwrap_or(usize::MAX).saturating_sub(defined);
    if count == 0 {
        // No use in the text: the driver named code of the body (the type check or the
        // expansion gives it), so the compiler built it. The definitions of the record with the
        // name decide as a path of the driver: a part that the configuration does not build
        // keeps the use.
        let found: Vec<&Def> = names.defs.iter().filter(|def| def.name == name && def.span.is_some()).collect();
        if found.is_empty() {
            return said();
        }
        return found.iter().any(|def| unbuilt_part(workspace, &names, &def.path)).then(|| say("the configuration does not build a part of it")).flatten();
    }
    match meanings(workspace, &file, start, end, name) {
        Some(found) if found.len() >= count => None,
        _ => said(),
    }
}

/// True if the syntax tree has a part of the type `path` with a `cfg` attribute that the record
/// of the tree does not have (the configuration does not build it): an impl in the module of
/// the type whose self type has the name of the type, a field, or a variant. The syntax tree
/// only finds the place; the record decides if the compiler built it.
fn unbuilt_part(workspace: &Workspace, names: &NameIndex, path: &str) -> bool {
    let Some((module, name)) = path.rsplit_once("::") else { return false };
    let head = |impl_path: &str| -> Option<String> {
        let (owner, header) = impl_path.split_once("::impl ")?;
        let header = header.split('#').next().unwrap_or(header).trim_start_matches('<');
        let ty = header.split(" as ").next().unwrap_or(header);
        let ty = ty.split('<').next().unwrap_or(ty).trim();
        (owner == module).then(|| ty.rsplit("::").next().unwrap_or(ty).to_string())
    };
    workspace.items.values().any(|item| {
        let has_cfg = item.attrs.iter().any(|attr| crate::facts::attribute_name(attr).as_deref() == Some("cfg"));
        let part = match item.kind {
            semilla_syntax::ItemKind::Impl => head(&item.path).as_deref() == Some(name),
            _ => item.path.strip_prefix(path).is_some_and(|rest| rest.starts_with('.') || rest.starts_with("::")),
        };
        has_cfg && part && item_def(workspace, names, &item.path).is_none() && item_place(workspace, item).is_none_or(|(file, start, end)| !names.built(&file, start, end))
    })
}

/// R70.2-1 (c): what each name of a range means by the compiler: for each name with the text
/// `word`, the definition or a local binding. `None`: the tree has no record of the range.
pub(crate) fn meanings(workspace: &Workspace, file: &str, start: usize, end: usize, word: &str) -> Option<Vec<Option<Def>>> {
    let names = workspace.names.get()?;
    // The place of an item of the syntax tree starts at its doc comments and attributes; the
    // place of the definition starts after them.
    let inner = names.defs_in(file).any(|def| def.kind != "module" && def.span.is_some_and(|(s, e)| start <= s && e <= end));
    if !names.built(file, start, end) && !names.built(file, start, start + 1) && !inner {
        return None;
    }
    let text = workspace.rust_files.get(file)?;
    Some(
        names
            .targets_in(file, start, end)
            .filter(|(s, e, _)| text.get(*s..*e).is_some_and(|name| name.trim_start_matches("r#") == word))
            .map(|(_, _, target)| match target {
                Target::Local => None,
                Target::Def(at) => Some(names.defs[at].clone()),
            })
            .collect(),
    )
}

/// A name record for a workspace of a test (design 70): one definition for each listed item at
/// the place of the item, with its fields.
#[cfg(test)]
pub(crate) mod testing {
    use semilla_core::namerec::{NameIndex, Record, RecordCfg, RecordDef};
    use semilla_syntax::Workspace;

    /// One definition of a test record.
    #[derive(Default)]
    pub(crate) struct TestDef<'a> {
        pub item: &'a str,
        pub public: bool,
        pub unsafe_def: bool,
        pub under_cfg: bool,
        pub trait_ref: Option<&'a str>,
        pub of: Option<(&'a str, bool)>,
        pub inert: &'a [&'a str],
    }

    /// The record of the items `defs` of `ws`; `removed`: the names of the items that a false
    /// `cfg` removes. The workspace gets the index of the record.
    pub(crate) fn give(ws: &Workspace, defs: &[TestDef], removed: &[&str]) {
        let mut record = Record::default();
        for def in defs {
            let item = &ws.items[def.item];
            let (file, start, end) = super::item_place(ws, item).expect("the place of a test item");
            let f = match record.f.iter().position(|known| *known == file) {
                Some(at) => at,
                None => {
                    record.f.push(file.clone());
                    record.f.len() - 1
                }
            } as i64;
            let kind = match item.kind {
                semilla_syntax::ItemKind::Impl => "implementation",
                semilla_syntax::ItemKind::Fn => "function",
                semilla_syntax::ItemKind::Trait => "trait",
                _ => "struct",
            };
            let mut entry = RecordDef { p: def.item.to_string(), n: super::own_name(def.item).to_string(), k: kind.to_string(), s: Some([f, start as i64, end as i64]), i: Some([f, start as i64, end as i64]), v: if def.public { "e".into() } else { String::new() }, u: def.unsafe_def, c: def.under_cfg, t: def.trait_ref.map(str::to_string), o: def.of.is_some_and(|(_, default)| default), h: def.inert.iter().map(|name| name.to_string()).collect(), ..RecordDef::default() };
            if let Some((of, _)) = def.of {
                record.d.push(RecordDef { p: of.to_string(), k: "associated function".into(), ..RecordDef::default() });
                entry.r = Some(record.d.len() - 1);
            }
            record.d.push(entry);
        }
        for name in removed {
            record.c.push(RecordCfg { r: [0, 0, 0], v: false, k: "cfg".into(), t: "unix".into(), s: name.to_string() });
        }
        if record.f.is_empty() {
            record.f.push("src/lib.rs".into());
        }
        ws.names.set(Some(std::sync::Arc::new(NameIndex::new([("demo.lib.aaaa", &record)]))));
    }
}
