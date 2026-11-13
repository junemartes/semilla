//! The file facts and the manifest facts (design section 23).
//!
//! The diff of a file with no parsed Rust code determines its change, so the agent writes no
//! claim. A TOML manifest (`Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`,
//! `rust-toolchain.toml`) gives one fact for each changed key. Any other file gives one fact:
//! added, removed, or changed.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Class, Fact};
use semilla_syntax::{Edit, EditKind, Workspace};

/// One changed key of a manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyChange {
    pub key: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// True for a file that Semilla reads as a TOML manifest. The legacy names `.cargo/config` and
/// `rust-toolchain` count as `.cargo/config.toml` and `rust-toolchain.toml`: cargo and rustup
/// still read them.
pub fn is_manifest(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    matches!(name, "Cargo.toml" | "Cargo.lock") || is_tool_config(file)
}

/// True for a file that cargo or rustup reads as its configuration: each file under a `.cargo`
/// folder, and `rust-toolchain` or `rust-toolchain.toml` in each folder.
pub fn is_tool_config(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    matches!(name, "rust-toolchain" | "rust-toolchain.toml") || file.starts_with(".cargo/") || file.contains("/.cargo/")
}

/// True for a file that no tool reads to build or to test (invariant I3: an allow list). Each
/// other file is in the spotlight. A `.gitignore` or a `.gitattributes` is not on the list: an
/// ignored file is not in the tree of the step, but cargo still reads it, and a git filter can
/// change the content of a checkout.
pub fn is_inert_file(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let lower = name.to_ascii_lowercase();
    let upper = name.to_ascii_uppercase();
    let extension = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    !is_tool_config(file)
        && (matches!(extension, "md" | "txt" | "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp")
            || ["LICENSE", "COPYING", "NOTICE", "CHANGELOG", "AUTHORS"].iter().any(|prefix| upper.starts_with(prefix)))
}

/// The file text as a TOML table. A one-line `rust-toolchain` file that is not TOML is the
/// channel name (rustup reads it so).
fn parse_table(file: &str, text: &str) -> Option<toml::Table> {
    if let Ok(table) = text.parse::<toml::Table>() {
        return Some(table);
    }
    let name = file.rsplit('/').next().unwrap_or(file);
    let line = text.trim();
    (name == "rust-toolchain" && !line.is_empty() && !line.contains('\n')).then(|| {
        let mut toolchain = toml::Table::new();
        toolchain.insert("channel".into(), toml::Value::String(line.to_string()));
        let mut table = toml::Table::new();
        table.insert("toolchain".into(), toml::Value::Table(toolchain));
        table
    })
}

/// The change of a file content as text for the reviewer: a line diff with a size limit, or
/// "binary, N bytes".
pub fn content_change(old: Option<&str>, new: Option<&str>, old_size: Option<u64>, new_size: Option<u64>) -> String {
    const MAX_LINES: usize = 40;
    let side = |text: Option<&str>, size: Option<u64>| match (text, size) {
        (None, Some(size)) => Some(format!("binary, {size} bytes")),
        (None, None) => None,
        (Some(text), _) => Some(format!("text, {} bytes", text.len())),
    };
    if (old.is_none() && old_size.is_some()) || (new.is_none() && new_size.is_some()) {
        return match (side(old, old_size), side(new, new_size)) {
            (Some(a), Some(b)) => format!("{a} → {b}"),
            (a, b) => a.or(b).unwrap_or_default(),
        };
    }
    let a: Vec<&str> = old.map(|t| t.lines().collect()).unwrap_or_default();
    let b: Vec<&str> = new.map(|t| t.lines().collect()).unwrap_or_default();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let mut lines: Vec<String> = Vec::new();
    if prefix > 0 {
        lines.push(format!("@@ line {} @@", prefix + 1));
    }
    lines.extend(a[prefix..a.len() - suffix].iter().map(|l| format!("- {l}")));
    lines.extend(b[prefix..b.len() - suffix].iter().map(|l| format!("+ {l}")));
    if lines.len() > MAX_LINES {
        let more = lines.len() - MAX_LINES;
        lines.truncate(MAX_LINES);
        lines.push(format!("… {more} more line(s)"));
    }
    if lines.iter().all(|l| l.starts_with("@@")) {
        return "only the line ends or the final line break changed".into();
    }
    lines.join("\n")
}

/// The arrays whose order has no meaning: their elements are compared as a set.
fn is_set_array(key: &str) -> bool {
    matches!(key, "workspace.members" | "workspace.exclude" | "workspace.default-members") || key.starts_with("features.") || key.ends_with(".features")
}

fn flatten(prefix: &str, value: &toml::Value, out: &mut BTreeMap<String, String>) {
    match value {
        toml::Value::Table(table) => {
            if table.is_empty() && !prefix.is_empty() {
                out.insert(prefix.to_string(), "{}".into());
            }
            for (key, value) in table {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                flatten(&path, value, out);
            }
        }
        toml::Value::Array(items) if is_set_array(prefix) => {
            if items.is_empty() {
                out.insert(prefix.to_string(), "[]".into());
            }
            for item in items {
                out.insert(format!("{prefix}: {item}"), "present".into());
            }
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

/// The changed keys of a TOML file, or `None` if a side does not parse. An empty list means that
/// only the format, the key order, or a comment changed.
pub fn key_changes(file: &str, old: Option<&str>, new: Option<&str>) -> Option<Vec<KeyChange>> {
    let parse = |text: Option<&str>| -> Option<toml::Value> {
        match text {
            None => Some(toml::Value::Table(Default::default())),
            Some(text) => parse_table(file, text).map(toml::Value::Table),
        }
    };
    let (old, new) = (parse(old)?, parse(new)?);
    if file.ends_with("Cargo.lock") {
        return Some(lock_changes(&old, &new));
    }
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    flatten("", &old, &mut a);
    flatten("", &new, &mut b);
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    Some(
        keys.into_iter()
            .filter(|key| a.get(*key) != b.get(*key))
            .map(|key| KeyChange { key: key.clone(), old: a.get(key).cloned(), new: b.get(key).cloned() })
            .collect(),
    )
}

/// The package changes of a `Cargo.lock`: added, removed, a new version, or the same version with
/// a new source or checksum.
fn lock_changes(old: &toml::Value, new: &toml::Value) -> Vec<KeyChange> {
    let packages = |value: &toml::Value| -> BTreeMap<String, BTreeMap<String, String>> {
        let mut map: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for package in value.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
            let field = |name: &str| package.get(name).and_then(|v| v.as_str()).unwrap_or("").to_string();
            map.entry(field("name")).or_default().insert(field("version"), format!("{} {}", field("source"), field("checksum")));
        }
        map
    };
    let (a, b) = (packages(old), packages(new));
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut changes = Vec::new();
    for name in names {
        let (old_versions, new_versions) = (a.get(name).cloned().unwrap_or_default(), b.get(name).cloned().unwrap_or_default());
        let gone: Vec<&String> = old_versions.keys().filter(|v| !new_versions.contains_key(*v)).collect();
        let added: Vec<&String> = new_versions.keys().filter(|v| !old_versions.contains_key(*v)).collect();
        if gone.len() == 1 && added.len() == 1 {
            changes.push(KeyChange { key: format!("package {name}"), old: Some(gone[0].clone()), new: Some(added[0].clone()) });
        } else {
            changes.extend(gone.iter().map(|v| KeyChange { key: format!("package {name}"), old: Some((*v).clone()), new: None }));
            changes.extend(added.iter().map(|v| KeyChange { key: format!("package {name}"), old: None, new: Some((*v).clone()) }));
        }
        for (version, source) in &old_versions {
            if new_versions.get(version).is_some_and(|other| other != source) {
                changes.push(KeyChange { key: format!("package {name} {version} source"), old: Some(source.clone()), new: new_versions.get(version).cloned() });
            }
        }
    }
    changes
}

/// The dependency name of a key under a dependency table (`dependencies.serde.version` gives
/// `serde`), with the table path.
fn dependency(key: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = key.split('.').collect();
    let at = parts.iter().position(|p| matches!(*p, "dependencies" | "dev-dependencies" | "build-dependencies"))?;
    Some((parts[..=at].join("."), parts.get(at + 1)?.to_string()))
}

/// The descriptive package keys: they do not change the build.
const DESCRIPTIVE: &[&str] = &["description", "authors", "license", "license-file", "repository", "readme", "keywords", "categories", "homepage", "documentation"];

/// True if the manifest fact with this place (`FILE:key`) is a build input (brief 5f, B2): each
/// key change except a descriptive package key. `package.version` is a build input: it is a part
/// of the crate hash, so it can change the MIR of bodies with no edit.
pub fn is_build_key(place: &str) -> bool {
    let key = place.split_once(':').map(|(_, key)| key).unwrap_or("");
    for table in ["workspace.package", "package"] {
        if let Some(name) = key.strip_prefix(table).and_then(|rest| rest.strip_prefix('.')) {
            let name = name.split([':', '.']).next().unwrap_or(name);
            return !DESCRIPTIVE.contains(&name);
        }
    }
    true
}

/// The member crate whose build script has this file: the root file of the `custom-build`
/// target (cargo names it: `package.build`, else `build.rs`), a module file of the build script,
/// or a file that the build script includes with `include!`. The file name does not decide.
pub fn build_script_of(workspace: &Workspace, file: &str) -> Option<String> {
    workspace.code_files.get(file).filter(|code| code.is_build_script() && !code.outside).map(|code| code.package.clone())
}

/// The code file entry of a file on one of the two sides: Rust code that the loader does not
/// parse into items (a build script, an example, a bench, a package that is not a member).
pub fn code_file<'a>(before: &'a Workspace, after: &'a Workspace, file: &str) -> Option<&'a semilla_syntax::CodeFile> {
    let found = |ws: &'a Workspace| ws.code_files.get(file);
    // A build script on one side has priority: the reviewer must see this role.
    [after, before].into_iter().filter_map(found).find(|code| code.is_build_script()).or_else(|| found(after)).or_else(|| found(before))
}

/// Why a manifest key change needs the reviewer's attention (design 23, rule 6), or `None`.
///
/// This is an allow list (invariant I3): a key needs no reason only if it is `package.version`, a
/// descriptive package key, or a version change of an existing registry dependency. Each other
/// key is in the spotlight. A deny list misses the next key.
fn spotlight(file: &str, change: &KeyChange, all: &[KeyChange], manifest: Option<&toml::Table>) -> Option<&'static str> {
    let key = change.key.as_str();
    if is_tool_config(file) {
        return Some("a key of the cargo or toolchain configuration");
    }
    if file.ends_with("Cargo.lock") {
        return key.ends_with(" source").then_some("the same package version with a new source or checksum");
    }
    for table in ["package", "workspace.package"] {
        if let Some(name) = key.strip_prefix(table).and_then(|rest| rest.strip_prefix('.')) {
            let name = name.split([':', '.']).next().unwrap_or(name);
            if name == "version" || DESCRIPTIVE.contains(&name) {
                return None;
            }
        }
    }
    if let Some((table, name)) = dependency(key) {
        let prefix = format!("{table}.{name}");
        let existed = all.iter().filter(|c| c.key == prefix || c.key.starts_with(&format!("{prefix}."))).all(|c| c.old.is_some()) && change.old.is_some();
        if !existed {
            return Some("a new dependency");
        }
        if [".git", ".path", ".registry", ".branch", ".rev", ".tag", ".package"].iter().any(|s| key.ends_with(s)) {
            return Some("a changed source of a dependency");
        }
        if key.ends_with(".features") || key.contains(".features:") || key.ends_with(".default-features") {
            return Some("the features of a dependency");
        }
        // A version change of an existing registry dependency (not a path or git dependency).
        let version = key == prefix || key == format!("{prefix}.version");
        let entry = manifest.and_then(|m| lookup(m, &prefix));
        let registry = entry.is_none_or(|value| value.as_table().is_none_or(|t| !t.contains_key("path") && !t.contains_key("git")));
        if version && change.new.is_some() && registry {
            return None;
        }
        return Some("a dependency setting other than the version");
    }
    let first = key.split(['.', ':']).next().unwrap_or("");
    if matches!(first, "profile" | "patch" | "replace" | "features" | "lints" | "lib" | "bin" | "target" | "test" | "bench" | "example") {
        return Some("a build setting (profile, patch, features, lints, or a target table)");
    }
    if matches!(key, "package.edition" | "package.build" | "package.links" | "package.rust-version") || key.starts_with("workspace.lints") || key.starts_with("workspace.package.edition") || key.starts_with("workspace.package.rust-version") {
        return Some("a package setting that changes the build");
    }
    if key.starts_with("workspace.") {
        return Some("a workspace setting: it can change which crates and tests cargo builds");
    }
    Some("a manifest key that is not in the allow list")
}

/// The value at a dotted key path of a table.
fn lookup<'a>(table: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut parts = path.split('.');
    let mut value = table.get(parts.next()?)?;
    for part in parts {
        value = value.as_table()?.get(part)?;
    }
    Some(value)
}

fn describe(file: &str, change: &KeyChange) -> String {
    match (&change.old, &change.new) {
        (None, Some(new)) if new == "present" => format!("{file}: {} added", change.key),
        (Some(old), None) if old == "present" => format!("{file}: {} removed", change.key),
        (None, Some(new)) => format!("{file}: {} added ({new})", change.key),
        (Some(old), None) => format!("{file}: {} removed ({old})", change.key),
        (Some(old), Some(new)) => format!("{file}: {} {old} → {new}", change.key),
        (None, None) => format!("{file}: {}", change.key),
    }
}

/// True for a new path dependency on a crate of the workspace: the forced edit of a move to
/// another crate (design 23, rule 8).
pub fn is_workspace_path_dependency(changes: &[KeyChange], before: &Workspace) -> bool {
    !changes.is_empty()
        && changes.iter().all(|change| {
            change.old.is_none()
                && dependency(&change.key).is_some_and(|(table, name)| {
                    table == "dependencies" && before.crates.iter().any(|krate| krate.name == name.replace('-', "_")) && (change.key.ends_with(".path") || change.key.ends_with(".version") || change.key == format!("dependencies.{name}"))
                })
        })
        && changes.iter().any(|change| change.key.ends_with(".path"))
}

/// True if a Rust file includes the file in a doc attribute (`#![doc = include_str!("…")]`).
fn doc_include(before: &Workspace, after: &Workspace, file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    [before, after].iter().flat_map(|ws| ws.rust_files.values()).any(|text| text.lines().any(|line| line.contains("doc") && line.contains("include_") && line.contains(name)))
}

/// The spotlight text of a file mode change.
const MODE_WHY: &str = "a file mode (a tool can run or skip a file by its mode)";

/// Why a file fact with no key list needs a reason, or `None`. Each cause that applies is in the
/// text, and no other cause: the text must fit the fact (brief 5g, item 4).
///
/// - `note` is the note of the file edit (a mode change, a link, a submodule, a binary file).
/// - `content_changed`: the file is new, removed, or has another content.
/// - `parsed`: the loader parses the file on each side where it exists (Rust files only).
///
/// - `rust`: the file is Rust code on one side: a `.rs` file, or a file that a target, a module,
///   or an `include!` of a target names. The file name does not decide (brief 5h, A4).
/// - `code`: the target that has the file, if the loader does not parse the target.
pub(crate) fn file_spotlight(file: &str, note: &str, content_changed: bool, parsed: bool, rust: bool, code: Option<&semilla_syntax::CodeFile>, doc_include: impl Fn() -> bool) -> Option<String> {
    let mut causes: Vec<String> = Vec::new();
    if note.contains("mode ") {
        causes.push(MODE_WHY.to_string());
    }
    if note.contains("symbolic link") {
        causes.push("a symbolic link (the content comes from another path)".to_string());
    }
    if note.contains("submodule") {
        causes.push("a submodule (the code of another repository)".to_string());
    }
    // The cause from the content. It applies when the content changed. It also applies when no
    // other cause is known: a file fact of these kinds never goes without a reason.
    let content: Option<String> = if let Some(code) = code.filter(|code| code.is_build_script()) {
        // The allow list of files does not apply: cargo compiles and runs this file.
        Some(format!("build script: this file is code of {}. Cargo runs it before each build of the package, and it can change the code and the result of the build", code.role()))
    } else if rust && parsed {
        // The syntax diff decides about the content of a Rust file that the loader parses.
        None
    } else if let Some(code) = code {
        Some(format!("Rust code that Semilla does not parse: this file is code of {}", code.role()))
    } else if rust {
        Some("Rust code that Semilla does not parse (a `.rs` file that no target of a member package names, or a file that `include!` reads)".to_string())
    } else if is_manifest(file) {
        Some("a manifest whose keys Semilla cannot compare (it does not parse, or it is not a usual file)".to_string())
    } else if !is_inert_file(file) {
        Some("a file that is not on the allow list of files that no tool reads to build or to test (docs, license texts, images)".to_string())
    } else if doc_include() {
        Some("a doc attribute includes this file (`#[doc = include_str!(…)]`): its doc tests run in `cargo test`".to_string())
    } else {
        None
    };
    if content_changed || causes.is_empty() {
        causes.extend(content);
    }
    if causes.is_empty() && rust {
        causes.push("a file edit of a Rust file with no known cause".to_string());
    }
    (!causes.is_empty()).then(|| causes.join("; "))
}

/// The facts of the file edits that no claim explains. Returns the facts and, for each edit index,
/// the index of its first fact in the returned list.
pub fn file_facts(step: &semilla_core::Step, edits: &[Edit], free: &[usize], before: &Workspace, after: &Workspace) -> (Vec<Fact>, BTreeMap<usize, usize>) {
    let mut facts: Vec<Fact> = Vec::new();
    let mut owner = BTreeMap::new();
    let text = |ws: &Workspace, file: &str| ws.other_files.get(file).or_else(|| ws.rust_files.get(file)).cloned();
    for &index in free {
        let edit = &edits[index];
        if !matches!(edit.kind, EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged) {
            continue;
        }
        let Some(file) = edit.file.clone() else { continue };
        let first = facts.len();
        let (old, new) = (text(before, &file), text(after, &file));
        let size = |ws: &Workspace| ws.files.get(&file).map(|entry| entry.size).or_else(|| text(ws, &file).map(|t| t.len() as u64));
        let content = content_change(old.as_deref(), new.as_deref(), size(before), size(after));
        let mut fact = |place: String, description: String, class: Class, why: Option<String>| {
            let reason = step.reasons.get(&place).cloned();
            facts.push(Fact {
                id: String::new(),
                at: None,
                file: Some(file.clone()),
                part: place.clone(),
                description,
                status: "derived".into(),
                class: Some(class),
                spotlight: why.is_some(),
                spotlight_why: why,
                reason,
                content: Some(content.clone()),
                ..Default::default()
            });
        };
        // The note of the file edit: a mode change, a link, a submodule, or a binary file.
        let note = edit.note.as_deref().unwrap_or("");
        let mode_only = note.starts_with("mode ");
        // The keys of a manifest that is a usual file. A mode change does not stop the comparison.
        let changes = if is_manifest(&file) && (note.is_empty() || mode_only) { key_changes(&file, old.as_deref(), new.as_deref()) } else { None };
        match changes {
            Some(changes) if changes.is_empty() && !mode_only => fact(format!("{file}:"), format!("{file}: format only"), Class::Neutral, None),
            Some(changes) => {
                let manifest = new.as_deref().or(old.as_deref()).and_then(|text| parse_table(&file, text));
                for change in &changes {
                    let why = spotlight(&file, change, &changes, manifest.as_ref()).map(str::to_string);
                    fact(format!("{file}:{}", change.key), describe(&file, change), Class::Other, why);
                }
                if mode_only {
                    fact(format!("{file}:"), format!("{file}: changed ({note})"), Class::Other, Some(MODE_WHY.to_string()));
                }
            }
            None => {
                let what = match edit.kind {
                    EditKind::FileAdded => "added",
                    EditKind::FileRemoved => "removed",
                    _ => "changed",
                };
                let entry = |ws: &Workspace| ws.files.get(&file).map(|entry| entry.hash.clone());
                let content_changed = edit.kind != EditKind::FileChanged || entry(before) != entry(after) || old != new;
                let exists = |ws: &Workspace| ws.files.contains_key(&file) || text(ws, &file).is_some();
                let parsed = |ws: &Workspace| !exists(ws) || ws.parsed_files.contains(&file);
                // A file that `include!` reads is Rust code, whatever its name is (batch 5w, D3).
                let rust = before.is_rust_file(&file) || after.is_rust_file(&file) || crate::owners::code_readers(before, after, &file).is_some();
                let why = file_spotlight(&file, note, content_changed, parsed(before) && parsed(after), rust, code_file(before, after, &file), || doc_include(before, after, &file));
                let note = edit.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default();
                fact(format!("{file}:"), format!("{file}: {what}{note}"), Class::Other, why);
            }
        }
        owner.insert(index, first);
    }
    (facts, owner)
}
