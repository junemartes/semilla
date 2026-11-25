//! The inputs of the build and the owner of a derived line (batch 11e, design 73, rule G3).
//!
//! A derived line is a line that Semilla derives from the compiler data, not from an edit at its
//! place: a changed expansion, a reader of a changed constant or static, an affected body, a
//! "through" line, an indirect change. The rule has three parts:
//!
//! 1. [R73.1-1] Each input of the build has an identity that is a hash of its content: a file of
//!    the tree (a source file, a file that the code reads at build time, a file of a path
//!    dependency in the tree) has the content hash of its tree entry; a package of a registry or
//!    of a repository has its entry in `Cargo.lock`; a macro outside the tree has the identity of
//!    its record (R65.1-3); the tool chain has its configuration files. A changed input is an
//!    input whose hash changes in the step.
//! 2. [R73.2-1] The owner of a derived line is the set of the changed inputs that reach the line
//!    backward along the edges of the compiler data, with the nearest facts of each one: the
//!    facts of the item at the place of the input, else the facts of its file.
//! 3. [R73.3-1] A negative sentence ("no changed definition, …") needs full data: it shows only
//!    when Semilla has the identity of each input. Else the line says "the cause is not known"
//!    and names the inputs that Semilla did not see.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::{Fact, Position};
use semilla_syntax::{Item, Workspace};

/// The state of one input of a derived line in the step (R73.1-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// The two trees have the same identity.
    Same,
    /// The identity changes in the step.
    Changed,
    /// Semilla has no identity of the input on a side. The text names the input.
    NotSeen(String),
}

/// True for a path inside the tree: a relative path with no `..` part.
pub fn in_tree(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|part| part == "..")
}

/// R73.1-1: the state of a file of the tree, from the content hash of its tree entry on each side.
/// A path outside the tree, or a path that neither tree has (a file that the build makes), has no
/// identity in the step.
pub fn file_state(trees: [&Workspace; 2], path: &str) -> State {
    if !in_tree(path) {
        return State::NotSeen(format!("a file outside the tree (`…/{}`)", short_path(path)));
    }
    let [old, new] = trees.map(|ws| ws.files.get(path).map(|entry| entry.hash.as_str()));
    match (old, new) {
        (None, None) => State::NotSeen(format!("the file `{path}`, which no tree of the step has")),
        _ if old == new => State::Same,
        _ => State::Changed,
    }
}

/// The last two parts of a path outside the tree: the full path names a folder of this machine.
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// The definition place of a record (`src/lib.rs:2`): the file and the line. `None` for a
/// definition outside the tree (`crate NAME`) or with no place.
pub fn place_of(def: &str) -> Option<(&str, usize)> {
    if def.starts_with("crate ") {
        return None;
    }
    let (file, line) = def.rsplit_once(':')?;
    in_tree(file).then_some(())?;
    Some((file, line.parse().ok()?))
}

/// The item of a tree that holds a line of a file: the smallest one.
pub fn item_at<'a>(ws: &'a Workspace, file: &str, line: usize) -> Option<&'a Item> {
    ws.items.values().filter(|item| item.file == file && item.line_start <= line && line <= item.line_end).min_by_key(|item| (item.line_end - item.line_start, item.path.len()))
}

/// True if a position of the list is on the side and in the line range of the file.
pub fn holds(positions: &[Position], old: bool, file: &str, start: usize, end: usize) -> bool {
    positions.iter().any(|position| position.file == file && position.old == old && position.start <= end && start <= position.end)
}

/// R73.2-1: the facts of a changed file: the facts of the whole file (a fact with no item, as a
/// file that Semilla does not parse), else each fact with a line in the file. An indirect fact
/// is a derived fact, never the fact of an input.
pub fn file_facts(facts: &[Fact], file: &str) -> Vec<usize> {
    let input = |fact: &&Fact| fact.part != "indirect change";
    let whole: Vec<usize> = facts.iter().enumerate().filter(|(_, fact)| input(fact) && fact.at.is_none() && fact.file.as_deref() == Some(file)).map(|(index, _)| index).collect();
    if !whole.is_empty() {
        return whole;
    }
    facts.iter().enumerate().filter(|(_, fact)| input(fact) && fact.positions.iter().any(|position| position.file == file)).map(|(index, _)| index).collect()
}

/// R73.2-1: the nearest facts of a changed place of one side (the definition of a macro): the
/// facts with a line in the item that holds the place. With no item at the place (a file that
/// Semilla does not parse), the facts of the file. An item with no fact at its lines did not
/// change: no fact.
pub fn place_facts(facts: &[Fact], ws: &Workspace, old: bool, file: &str, line: usize) -> Vec<usize> {
    match item_at(ws, file, line) {
        Some(item) => facts.iter().enumerate().filter(|(_, fact)| fact.part != "indirect change" && holds(&fact.positions, old, file, item.line_start, item.line_end)).map(|(index, _)| index).collect(),
        None => file_facts(facts, file),
    }
}

/// The folder of the package of a file of the tree: the nearest folder above it with a
/// `Cargo.toml` in one of the trees (`""` for the root).
pub fn package_dir(trees: [&Workspace; 2], file: &str) -> Option<String> {
    let mut dir = file;
    while let Some((parent, _)) = dir.rsplit_once('/') {
        if trees.iter().any(|ws| ws.files.contains_key(&format!("{parent}/Cargo.toml"))) {
            return Some(parent.to_string());
        }
        dir = parent;
    }
    trees.iter().any(|ws| ws.files.contains_key("Cargo.toml")).then(String::new)
}

/// The changed files of a folder of the tree (each file of the folder and of its sub folders),
/// in the order of their paths.
pub fn changed_files(trees: [&Workspace; 2], dir: &str) -> Vec<String> {
    let inside = |path: &&String| dir.is_empty() || path.starts_with(&format!("{dir}/"));
    let paths: BTreeSet<&String> = trees.iter().flat_map(|ws| ws.files.keys()).filter(inside).collect();
    paths.into_iter().filter(|path| file_state(trees, path) == State::Changed).cloned().collect()
}

/// R73.1-1: the identity of each package of `Cargo.lock` (R65.10-1), by its crate name (`-` gives
/// `_`): the set of its versions with the source and the checksum. A package with no source (a
/// package of the workspace or a path package) has the entry `path VERSION`: its files are its
/// identity. `None`: the tree has no readable `Cargo.lock`.
pub fn lock_entries(ws: &Workspace) -> Option<BTreeMap<String, BTreeSet<String>>> {
    let text = ws.other_files.get("Cargo.lock")?;
    let table = text.parse::<toml::Table>().ok()?;
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in table.get("package").and_then(|value| value.as_array()).into_iter().flatten() {
        let field = |name: &str| package.get(name).and_then(|value| value.as_str()).unwrap_or("").to_string();
        let entry = if field("source").is_empty() { format!("path {}", field("version")) } else { format!("{} {} {}", field("version"), field("source"), field("checksum")) };
        out.entry(field("name").replace('-', "_")).or_default().insert(entry);
    }
    Some(out)
}

/// The kind of a crate outside the tree for the rule R73.1-1, from the `Cargo.lock` of the two
/// trees: a package of a registry or of a repository (its entry has a source), a path package
/// outside the tree (its entry has no source), or a crate with no entry (a crate of the tool
/// chain). `None`: a tree has no `Cargo.lock`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outside {
    Registry,
    Path,
    ToolChain,
}

pub fn outside_kind(locks: &[Option<BTreeMap<String, BTreeSet<String>>>; 2], name: &str) -> Option<Outside> {
    let [Some(old), Some(new)] = locks else { return None };
    let entries: Vec<&String> = [old.get(name), new.get(name)].into_iter().flatten().flatten().collect();
    if entries.is_empty() {
        Some(Outside::ToolChain)
    } else if entries.iter().all(|entry| entry.starts_with("path ")) {
        Some(Outside::Path)
    } else {
        Some(Outside::Registry)
    }
}

/// The facts of the tool chain and of its configuration (R73.1-1): the facts of each changed
/// `rust-toolchain` file and cargo configuration file.
pub fn tool_facts(facts: &[Fact]) -> Vec<usize> {
    facts.iter().enumerate().filter(|(_, fact)| fact.at.is_none() && fact.part != "indirect change" && fact.file.as_deref().is_some_and(crate::files::is_tool_config)).map(|(index, _)| index).collect()
}

/// Adds the indices of a list to an owner list, with no repeat.
pub fn extend(owners: &mut Vec<usize>, more: impl IntoIterator<Item = usize>) {
    for index in more {
        if !owners.contains(&index) {
            owners.push(index);
        }
    }
}

/// R73.3-1: the cause of a derived line with no owner. With an input that Semilla did not see,
/// "the cause is not known" and the list of these inputs; with full data, the negative sentence.
pub fn no_owner_cause(not_seen: &[String], negative: &str) -> String {
    if not_seen.is_empty() {
        return negative.to_string();
    }
    let limit = semilla_core::report::REACH_FOLD_NAMES;
    let mut list = not_seen.iter().take(limit).cloned().collect::<Vec<_>>().join("; ");
    if not_seen.len() > limit {
        list.push_str(&format!("; and {} more", not_seen.len() - limit));
    }
    format!("{NOT_KNOWN}: Semilla does not see {list}")
}

/// The start of the cause of a derived line whose inputs Semilla did not see each (R73.3-1).
pub const NOT_KNOWN: &str = "the cause is not known";
