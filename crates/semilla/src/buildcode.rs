//! The test "the tree has changed build code" (design section 28).
//!
//! **Build code** is each program that runs on the machine during a check build, and each setting
//! that names such a program:
//!
//! - a build script, with each file of its module tree and each file that it includes;
//! - a proc macro crate, and each crate that a proc macro crate or a build script depends on (a
//!   **host package**);
//! - a cargo or rustup configuration file in the tree.
//!
//! Build code can change each record of its build. The build code of the base tree is trusted.
//! The test compares a tree with the base tree of its change set. It is an allow list (I3): the
//! tree is clean only if each changed path is on the list of [`changed_build_code`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use semilla_syntax::Workspace;
use serde::{Deserialize, Serialize};

/// The cause of a changed file of a host package.
pub const HOST_FILE: &str = "a file of a host package (a proc macro crate, or a crate that a proc macro crate or a build script uses)";

/// The cause of a changed configuration file, and of a file that a configuration file names.
/// With one of these parts, the test stops: the list of the parts is then not complete.
const CONFIG_FILE: &str = "a cargo or rustup configuration file";
const CONFIG_NAME: &str = "a configuration file of the tree has this file name";

/// One changed part of the build code: the path, and why the path is build code.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Part {
    pub path: String,
    pub why: String,
    /// True if the path alone tells the reviewer what the part is (a build script, a file of a
    /// host package, a configuration file). Else a message prints the cause too.
    pub plain: bool,
}

impl Part {
    fn file(path: &str, why: impl Into<String>) -> Part {
        Part { path: path.to_string(), why: why.into(), plain: true }
    }

    pub(crate) fn other(path: &str, why: impl Into<String>) -> Part {
        Part { path: path.to_string(), why: why.into(), plain: false }
    }

    /// The part in a message.
    pub fn label(&self) -> String {
        if self.plain { self.path.clone() } else { format!("{} ({})", self.path, self.why) }
    }
}

/// Cargo cannot read the dependency graph of a tree (batch 5n, A6). This is not a decision about
/// build code: the cause can be a package that is not in the cargo cache (cargo runs with
/// `--offline`). The command stops with its own message, and nothing goes into the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphFailure {
    /// True: the base tree. False: the tree of the step.
    pub base: bool,
    /// The first error line of cargo.
    pub line: String,
}

impl std::fmt::Display for GraphFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cargo cannot read the dependency graph of {}: {}", if self.base { "the base tree" } else { "the tree" }, self.line)
    }
}

impl std::error::Error for GraphFailure {}

/// The parts in a message: at most four, then the count of the others.
pub fn labels(parts: &[Part]) -> String {
    let mut seen = BTreeSet::new();
    let labels: Vec<String> = parts.iter().map(Part::label).filter(|label| seen.insert(label.clone())).collect();
    let more = labels.len().saturating_sub(4);
    let mut text = labels.into_iter().take(4).collect::<Vec<_>>().join(", ");
    if more > 0 {
        text.push_str(&format!(", and {more} more"));
    }
    text
}

/// One tree for the test: its snapshot folder, and its workspace (with the entries of the git
/// tree in `files`).
pub struct Side<'a> {
    pub dir: &'a Path,
    pub ws: &'a Workspace,
}

/// The host packages of a tree, from `cargo metadata` with the dependency graph.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Each host package (name, version, and source or folder), with its checksum from the lock
    /// file and its features.
    pub packages: BTreeMap<String, (Option<String>, Vec<String>)>,
    /// The build script path of each package that has a build script, relative to the package.
    pub scripts: BTreeMap<String, String>,
    /// The folder of each package of the tree (relative to the root; empty for the root), and
    /// true for a host package.
    pub dirs: BTreeMap<String, bool>,
    /// The root file of each target of a host package of the tree, relative to the root.
    pub roots: BTreeSet<String>,
    /// A package whose folder is outside the tree.
    pub outside: Vec<String>,
}

impl HostInfo {
    /// Reads the host packages from the output of `cargo metadata` (with the dependency graph)
    /// and from the text of the lock file of the tree.
    pub fn from_metadata(metadata: &serde_json::Value, lock: Option<&str>) -> Result<HostInfo> {
        let root = metadata.get("workspace_root").and_then(|r| r.as_str()).context("the cargo metadata has no workspace root")?;
        let packages = metadata.get("packages").and_then(|p| p.as_array()).context("the cargo metadata has no package list")?;
        let nodes = metadata.get("resolve").and_then(|r| r.get("nodes")).and_then(|n| n.as_array()).context("the cargo metadata has no dependency graph")?;
        let text = |value: &serde_json::Value, key: &str| value.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let relative = |path: &str| Path::new(path).strip_prefix(root).ok().map(|rel| rel.to_string_lossy().replace('\\', "/"));
        // The checksum of each registry package, from the lock file.
        let mut checksums: BTreeMap<String, String> = BTreeMap::new();
        if let Some(lock) = lock.and_then(|text| text.parse::<toml::Table>().ok()) {
            for package in lock.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
                let field = |key: &str| package.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
                if package.get("checksum").is_some() {
                    checksums.insert(format!("{} {} {}", field("name"), field("version"), field("source")), field("checksum"));
                }
            }
        }
        let mut info = HostInfo::default();
        // The key, the folder in the tree, and the target lists of each package, by its id.
        let mut keys: BTreeMap<String, String> = BTreeMap::new();
        let mut folders: BTreeMap<String, String> = BTreeMap::new();
        let mut seeds: Vec<String> = Vec::new();
        let mut with_script: BTreeSet<String> = BTreeSet::new();
        let mut roots: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for package in packages {
            let id = text(package, "id");
            let manifest = text(package, "manifest_path");
            let source = package.get("source").and_then(|s| s.as_str());
            let folder = Path::new(&manifest).parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
            let key = match source {
                Some(source) => format!("{} {} {source}", text(package, "name"), text(package, "version")),
                None => match relative(&folder) {
                    Some(dir) => {
                        folders.insert(id.clone(), dir.clone());
                        format!("{} {} path:{dir}", text(package, "name"), text(package, "version"))
                    }
                    None => {
                        info.outside.push(format!("{} ({folder})", text(package, "name")));
                        format!("{} {} outside:{folder}", text(package, "name"), text(package, "version"))
                    }
                },
            };
            for target in package.get("targets").and_then(|t| t.as_array()).into_iter().flatten() {
                let kinds: Vec<&str> = target.get("kind").and_then(|k| k.as_array()).into_iter().flatten().filter_map(|k| k.as_str()).collect();
                let file = text(target, "src_path");
                if kinds.contains(&"proc-macro") {
                    seeds.push(id.clone());
                }
                if kinds.contains(&"custom-build") {
                    with_script.insert(id.clone());
                    let path = Path::new(&file).strip_prefix(&folder).map(|rel| rel.to_string_lossy().replace('\\', "/")).unwrap_or(file.clone());
                    info.scripts.insert(key.clone(), path);
                }
                if let Some(file) = relative(&file).filter(|_| source.is_none()) {
                    roots.entry(id.clone()).or_default().push(file);
                }
            }
            keys.insert(id, key);
        }
        // The edges of the graph that a build of the host side follows: the normal dependencies
        // and the build dependencies. A graph with no dependency kinds gives each edge.
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut features: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for node in nodes {
            let id = text(node, "id");
            let mut list: Vec<String> = node.get("features").and_then(|f| f.as_array()).into_iter().flatten().filter_map(|f| f.as_str().map(str::to_string)).collect();
            list.sort();
            features.insert(id.clone(), list);
            for dep in node.get("deps").and_then(|d| d.as_array()).into_iter().flatten() {
                let kinds: Option<Vec<Option<&str>>> = dep.get("dep_kinds").and_then(|k| k.as_array()).map(|kinds| kinds.iter().map(|kind| kind.get("kind").and_then(|k| k.as_str())).collect());
                let package = text(dep, "pkg");
                let (build, normal) = match &kinds {
                    Some(kinds) => (kinds.contains(&Some("build")), kinds.contains(&None)),
                    None => (true, true),
                };
                // The build dependency of a package with a build script is a host package.
                if build && with_script.contains(&id) {
                    seeds.push(package.clone());
                }
                if build || normal {
                    edges.entry(id.clone()).or_default().push(package);
                }
            }
        }
        let mut host: BTreeSet<String> = BTreeSet::new();
        while let Some(id) = seeds.pop() {
            if host.insert(id.clone()) {
                seeds.extend(edges.get(&id).cloned().unwrap_or_default());
            }
        }
        for (id, key) in &keys {
            let is_host = host.contains(id);
            if is_host {
                info.packages.insert(key.clone(), (checksums.get(key).cloned(), features.get(id).cloned().unwrap_or_default()));
                info.roots.extend(roots.get(id).cloned().unwrap_or_default());
            }
            if let Some(dir) = folders.get(id) {
                let entry = info.dirs.entry(dir.clone()).or_default();
                *entry = *entry || is_host;
            }
        }
        Ok(info)
    }

    /// The folder of the package that has a path: the longest package folder that contains it.
    fn package_of<'a>(&'a self, path: &str) -> Option<(&'a str, bool)> {
        self.dirs
            .iter()
            .filter(|(dir, _)| dir.is_empty() || path.strip_prefix(dir.as_str()).is_some_and(|rest| rest.starts_with('/')))
            .max_by_key(|(dir, _)| dir.len())
            .map(|(dir, host)| (dir.as_str(), *host))
    }
}

/// The manifest keys that name no program and no file that a program runs (rule (c)). The
/// dependency tables, the features, and the targets are in the comparison of the host packages.
fn is_plain_manifest_key(key: &str) -> bool {
    let mut parts = key.split([':', '.']).map(str::trim);
    let table = parts.next().unwrap_or("");
    match table {
        "package" | "lib" | "bin" | "test" | "example" | "bench" | "dependencies" | "dev-dependencies" | "build-dependencies" | "target" | "features" | "workspace" | "lints" | "badges" | "patch" | "replace" => true,
        // A profile key from a fixed list. `rustflags` and `codegen-backend` can name a program.
        "profile" => {
            let name = key.rsplit('.').next().unwrap_or("");
            matches!(name, "opt-level" | "debug" | "split-debuginfo" | "strip" | "debug-assertions" | "overflow-checks" | "lto" | "panic" | "incremental" | "codegen-units" | "rpath" | "inherits")
        }
        _ => false,
    }
}

fn is_manifest_or_lock(path: &str) -> bool {
    matches!(path.rsplit('/').next().unwrap_or(path), "Cargo.toml" | "Cargo.lock")
}

/// The test "the tree has changed build code" (A1). An empty list means "clean".
///
/// The tree is clean only if each changed path between the base and the tree is one of these:
///
/// - (a) a Rust source file that only targets outside the host packages own (by the module
///   trees), and that is not a file of a build script;
/// - (b) a file on the neutral file allow list that no build code names;
/// - (c) a manifest or a lock file, when the host packages (name, version, source, checksum),
///   their features, and the build script path of each package stay equal, and each changed
///   manifest key is on the list of [`is_plain_manifest_key`];
/// - (d) each other file, but only when neither tree has build code in the tree.
///
/// `full` gives the output of `cargo metadata` with the dependency graph for a snapshot folder.
/// That command starts the compiler, and thus each program that a configuration file names. So
/// the test calls it on the tree only after it knows that no configuration file changed, and
/// that no changed file has a name that is in a configuration file.
///
/// A failed read of the dependency graph is not a decision about build code (batch 5n, A6): the
/// cause can be a package that is not in the cargo cache. The result is then a [`GraphFailure`],
/// and the caller must not store it.
pub fn changed_build_code(base: &Side, tree: &Side, full: &mut dyn FnMut(&Path) -> Result<HostInfo>) -> Result<Vec<Part>, GraphFailure> {
    let paths: BTreeSet<&String> = base.ws.files.keys().chain(tree.ws.files.keys()).collect();
    let changed: Vec<&str> = paths.into_iter().filter(|path| base.ws.files.get(*path) != tree.ws.files.get(*path)).map(|path| path.as_str()).collect();
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let mut parts: Vec<Part> = Vec::new();
    let name_of = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    let is_config = semilla_verify::files::is_tool_config;
    let read = |side: &Side, path: &str| std::fs::read_to_string(side.dir.join(path)).unwrap_or_default();
    // The text of each file of a list, on each side where the file exists.
    let texts = |files: &BTreeSet<String>| -> Vec<String> {
        files.iter().flat_map(|file| [base, tree].into_iter().filter(|side| side.ws.files.contains_key(file)).map(|side| read(side, file)).collect::<Vec<_>>()).collect()
    };
    let names = |texts: &[String], path: &str| texts.iter().any(|text| text.contains(&name_of(path)));

    // 1. The configuration files, and each file whose name is in a configuration file.
    let configs: BTreeSet<String> = base.ws.files.keys().chain(tree.ws.files.keys()).filter(|path| is_config(path)).cloned().collect();
    let config_texts = texts(&configs);
    for path in &changed {
        if is_config(path) {
            parts.push(Part::file(path, CONFIG_FILE));
        } else if names(&config_texts, path) {
            parts.push(Part::other(path, CONFIG_NAME));
        }
    }
    if !parts.is_empty() {
        // No `cargo metadata` with the graph on this tree: it starts the programs of the
        // configuration.
        return Ok(parts);
    }

    // 2. The files of the build scripts, by the module trees of the two sides.
    let script_of = |path: &str| [tree, base].into_iter().find_map(|side| side.ws.code_files.get(path).filter(|code| code.is_build_script()));
    let scripts: BTreeSet<String> = [base, tree].into_iter().flat_map(|side| side.ws.code_files.iter().filter(|(_, code)| code.is_build_script()).map(|(file, _)| file.clone())).collect();
    for path in &changed {
        if let Some(code) = script_of(path) {
            parts.push(Part::file(path, format!("a file of {}", code.role())));
        }
    }

    // 3. The host packages. The base tree is trusted, so its graph comes first.
    let manifest_changed = changed.iter().any(|path| is_manifest_or_lock(path));
    let host_base = match full(base.dir) {
        Ok(host) => host,
        Err(error) => return Err(GraphFailure { base: true, line: first_line(&error) }),
    };
    let host_tree = if manifest_changed {
        match full(tree.dir) {
            Ok(host) => host,
            Err(error) => return Err(GraphFailure { base: false, line: first_line(&error) }),
        }
    } else {
        // Each manifest, each lock file, and each configuration file is equal, so the packages
        // and the explicit targets are equal too. A new default target is in a package folder.
        host_base.clone()
    };
    if manifest_changed {
        let manifest = changed.iter().find(|path| path.ends_with("Cargo.toml")).or_else(|| changed.iter().find(|path| is_manifest_or_lock(path))).copied().unwrap_or(changed[0]);
        for (key, new) in &host_tree.packages {
            match host_base.packages.get(key) {
                None => parts.push(Part::other(manifest, format!("a new host package: {}", short_key(key)))),
                Some(old) if old.0 != new.0 => parts.push(Part::other(manifest, format!("another checksum of the host package {}", short_key(key)))),
                Some(old) if old.1 != new.1 => parts.push(Part::other(manifest, format!("other features of the host package {}: {}", short_key(key), feature_change(&old.1, &new.1)))),
                Some(_) => {}
            }
        }
        for key in host_base.packages.keys().filter(|key| !host_tree.packages.contains_key(*key)) {
            parts.push(Part::other(manifest, format!("a removed host package: {}", short_key(key))));
        }
        let keys: BTreeSet<&String> = host_base.scripts.keys().chain(host_tree.scripts.keys()).collect();
        for key in keys {
            match (host_base.scripts.get(key), host_tree.scripts.get(key)) {
                (old, new) if old == new => {}
                (None, Some(new)) => parts.push(Part::other(manifest, format!("a new build script: `{new}` of {}", short_key(key)))),
                (Some(old), None) => parts.push(Part::other(manifest, format!("a removed build script: `{old}` of {}", short_key(key)))),
                (Some(old), Some(new)) => parts.push(Part::other(manifest, format!("another build script of {}: `{old}` → `{new}`", short_key(key)))),
                _ => {}
            }
        }
        for package in &host_tree.outside {
            parts.push(Part::other(manifest, format!("Semilla cannot decide: the package {package} is outside the tree")));
        }
        // The keys of each changed manifest.
        for path in changed.iter().filter(|path| path.ends_with("Cargo.toml")) {
            let old = base.ws.files.contains_key(*path).then(|| read(base, path));
            let new = tree.ws.files.contains_key(*path).then(|| read(tree, path));
            match semilla_verify::files::key_changes(path, old.as_deref(), new.as_deref()) {
                None => parts.push(Part::other(path, "Semilla cannot decide: the manifest does not parse")),
                Some(changes) => {
                    for change in changes.iter().filter(|change| !is_plain_manifest_key(&change.key)) {
                        parts.push(Part::other(path, format!("the manifest key `{}` is not on the list of keys that name no program", change.key)));
                    }
                }
            }
        }
    }

    // 4. The files of the host packages of the tree: each file in the folder of a host package,
    // and each file of the module tree of a host target, on the two sides.
    let mut host_files: BTreeSet<String> = BTreeSet::new();
    for (side, roots) in [(base, &host_base.roots), (tree, &host_base.roots), (tree, &host_tree.roots)] {
        for root in roots {
            host_files.extend(semilla_syntax::load::module_files(side.dir, &side.dir.join(root)));
        }
    }
    let in_host_folder = |path: &str| [&host_base, &host_tree].into_iter().any(|host| host.package_of(path).is_some_and(|(_, is_host)| is_host));
    let known: BTreeSet<String> = parts.iter().map(|part| part.path.clone()).collect();
    for path in changed.iter().filter(|path| !known.contains(**path)) {
        if in_host_folder(path) || host_files.contains(*path) {
            parts.push(Part::file(path, HOST_FILE));
        }
    }

    // 5. Each other changed path must be on the allow list.
    let has_build_code = |side: &Side, host: &HostInfo| {
        let mut kinds = Vec::new();
        if side.ws.code_files.values().any(|code| code.is_build_script()) {
            kinds.push("a build script");
        }
        if host.dirs.values().any(|is_host| *is_host) {
            kinds.push("a host package");
        }
        if side.ws.files.keys().any(|path| is_config(path)) {
            kinds.push("a cargo or rustup configuration file");
        }
        kinds
    };
    let mut in_tree = has_build_code(base, &host_base);
    for kind in has_build_code(tree, &host_tree) {
        if !in_tree.contains(&kind) {
            in_tree.push(kind);
        }
    }
    let mut code_files = scripts;
    code_files.extend(host_files.iter().cloned());
    for dir in [&host_base, &host_tree].into_iter().flat_map(|host| host.dirs.iter().filter(|(_, is_host)| **is_host).map(|(dir, _)| dir.clone())) {
        let inside = |path: &&String| dir.is_empty() || path.strip_prefix(dir.as_str()).is_some_and(|rest| rest.starts_with('/'));
        code_files.extend(base.ws.files.keys().chain(tree.ws.files.keys()).filter(inside).filter(|path| path.ends_with(".rs") || path.ends_with("Cargo.toml")).cloned());
    }
    let code_texts = texts(&code_files);
    let known: BTreeSet<String> = parts.iter().map(|part| part.path.clone()).collect();
    for path in changed.iter().filter(|path| !known.contains(**path)) {
        let link = [base, tree].into_iter().any(|side| side.ws.files.get(*path).is_some_and(|entry| entry.mode != "100644" && entry.mode != "100755"));
        let owned = |side: &Side| side.ws.parsed_files.contains(*path) || side.ws.code_files.contains_key(*path);
        let exists = |side: &Side| side.ws.files.contains_key(*path);
        // (a) Rust code of a target on each side where the file exists.
        if !link && [base, tree].into_iter().all(|side| !exists(side) || owned(side)) {
            continue;
        }
        // (c) The comparison above found no difference for this manifest or lock file.
        if !link && is_manifest_or_lock(path) {
            continue;
        }
        // (b) A neutral file, if no build code has its name.
        if !link && semilla_verify::files::is_inert_file(path) {
            if names(&code_texts, path) {
                parts.push(Part::other(path, "build code of the tree has this file name"));
            }
            continue;
        }
        // (d) Each other file, if the trees have no build code.
        if !in_tree.is_empty() {
            parts.push(Part::other(path, format!("the tree has {}, and build code can read or run this file", in_tree.join(" and "))));
        }
    }
    parts.sort();
    parts.dedup();
    Ok(parts)
}

/// The digest of the build code state of a tree (design 28.9, batch 5v). The input is each
/// entry of the tree (the path, the mode, and the content hash) that the test above does not
/// permit to change with rule (a): each entry that is not a Rust source file of a target
/// outside the host packages. The build scripts, the files of the host packages, the manifests,
/// the lock file, the configuration files, and each other file are in the digest. Two trees
/// with the same digest have the same build code.
pub fn state_digest(side: &Side, host: &HostInfo) -> String {
    let mut host_files: BTreeSet<String> = BTreeSet::new();
    for root in &host.roots {
        host_files.extend(semilla_syntax::load::module_files(side.dir, &side.dir.join(root)));
    }
    let mut text = String::from("build code state 1\n");
    for (path, entry) in &side.ws.files {
        let plain = entry.mode == "100644" || entry.mode == "100755";
        let owned = path.ends_with(".rs") && (side.ws.parsed_files.contains(path) || side.ws.code_files.contains_key(path));
        let script = side.ws.code_files.get(path).is_some_and(|code| code.is_build_script());
        let of_host = host.package_of(path).is_some_and(|(_, is_host)| is_host) || host_files.contains(path);
        if plain && owned && !script && !of_host {
            continue;
        }
        text.push_str(&format!("{path}\0{}\0{}\n", entry.mode, entry.hash));
    }
    semilla_normal::hash::sha(&text)[..32].to_string()
}

/// The key of the folder set of a tree with changed build code (design 28.9, batch 5v). Two
/// trees have the same key only if they have the same base tree, the same changed parts with
/// the same content, and the same manifests and lock files. Each changed build code file is a
/// part, so the two trees have the same build code. The key is the tree itself (`strict`) if
/// the base is not trusted, if Semilla cannot read the entries of the tree, or if the test
/// stopped at a configuration file: the list of the parts is then not complete.
pub fn marked_key(base: &str, tree: &str, parts: &[Part], files: Option<&BTreeMap<String, semilla_syntax::load::FileEntry>>, strict: bool) -> String {
    let files = match files {
        Some(files) if !strict && !parts.iter().any(|part| part.why == CONFIG_FILE || part.why == CONFIG_NAME) => files,
        _ => return format!("tree-{tree}"),
    };
    let entry = |path: &str| files.get(path).map(|entry| format!("{}\0{}", entry.mode, entry.hash)).unwrap_or_else(|| "-".to_string());
    let mut text = format!("changed build code 1\n{base}\n");
    for part in parts {
        text.push_str(&format!("{}\0{}\0{}\n", part.path, part.why, entry(&part.path)));
    }
    for path in files.keys().filter(|path| is_manifest_or_lock(path)) {
        text.push_str(&format!("{path}\0{}\n", entry(path)));
    }
    semilla_normal::hash::sha(&text)[..32].to_string()
}

fn first_line(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let line = lines.iter().find(|line| line.starts_with("error")).or(lines.last()).copied().unwrap_or("no message");
    line.chars().take(200).collect()
}

/// A package key for a message: the name and the version.
fn short_key(key: &str) -> String {
    let mut words = key.split(' ');
    format!("`{} {}`", words.next().unwrap_or(""), words.next().unwrap_or(""))
}

fn feature_change(old: &[String], new: &[String]) -> String {
    let added: Vec<String> = new.iter().filter(|f| !old.contains(f)).map(|f| format!("+{f}")).collect();
    let removed: Vec<String> = old.iter().filter(|f| !new.contains(f)).map(|f| format!("-{f}")).collect();
    added.into_iter().chain(removed).collect::<Vec<_>>().join(" ")
}

/// What a command does with the build code that the change set changes (design 28).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The default of each command. The command runs the changed build code in the sandbox and
    /// in separate folders. Each proof that depends on it gets the mark (invariant I6).
    #[default]
    #[serde(alias = "agent", alias = "reviewer_trust")]
    Run,
    /// A command with `--no-build-code`. It never runs build code that the change set changes,
    /// and it reads no record and no result of such a run.
    #[serde(alias = "reviewer")]
    NoBuildCode,
}

impl Mode {
    pub fn runs_build_code(self) -> bool {
        self == Mode::Run
    }

    /// The suffix of a queue file of this mode. A job of one mode never replaces a job of
    /// the other mode.
    pub fn suffix(self) -> &'static str {
        match self {
            Mode::Run => "",
            Mode::NoBuildCode => ".no-build-code",
        }
    }
}

/// The bound of the base line (design 28.5): the number of commits before a change set that
/// Semilla examines for a change of build code.
pub const BASE_WALK: usize = 20;

/// The line about the build code of the base (design 28.5). `commits` are the commits before the
/// change set that changed build code, from the newest commit back, each with its files.
/// `stopped` is true if the history has more commits than the bound. No line: no commit changed
/// build code, and Semilla examined each commit after the first commit of the history.
pub fn base_code_line(commits: &[(String, Vec<String>)], stopped: bool) -> Option<String> {
    let list = |items: Vec<String>, sep: &str| {
        let more = items.len().saturating_sub(3);
        let mut text = items.into_iter().take(3).collect::<Vec<_>>().join(sep);
        if more > 0 {
            text.push_str(&format!("{sep}and {more} more"));
        }
        text
    };
    if commits.is_empty() {
        return stopped.then(|| format!("base build code: no change in the {BASE_WALK} commits before this change set (Semilla examines no more commits)"));
    }
    let found = list(commits.iter().map(|(commit, files)| format!("`{commit}` ({})", list(files.clone(), ", "))).collect(), "; ");
    let limit = if stopped { format!(" Semilla examined only the {BASE_WALK} commits before this change set.") } else { String::new() };
    Some(format!("! the base has build code that a recent commit changed: {found}. Each check of this change set ran this code. No mark shows this, because the change is before the base.{limit}"))
}

/// The option of a reviewer command that runs changed build code with a limit of the sandbox
/// missing (design 28.2).
pub const ANYWAY_OPTION: &str = "--build-code-no-sandbox";

/// The mode of a reviewer command (design 28.2). The sandbox is the one protection for the
/// default. A reviewer command runs the changed build code of a different person only with the
/// full sandbox: the write limit, the read limit, and the network limit. With a limit missing
/// (an old kernel, a different operating system, the variable `SEMILLA_NO_SANDBOX`), the mode is
/// that of `--no-build-code`, and the line says why. The option [`ANYWAY_OPTION`] (`anyway`)
/// runs the code with the limits that the machine has. A variable does not have that effect:
/// the agent can control the environment of a command.
pub fn sandbox_guard(mode: Mode, missing: Option<&semilla_normal::sandbox::Missing>, anyway: bool) -> (Mode, Option<String>) {
    match missing {
        Some(missing) if mode == Mode::Run && !anyway => {
            let verb = if missing.limits.len() == 1 { "is" } else { "are" };
            (
                Mode::NoBuildCode,
                Some(format!(
                    "The build sandbox is not complete: {} {verb} missing ({}). The command does not run changed build code. To run it with this sandbox, use the option {ANYWAY_OPTION}.",
                    missing.names(),
                    missing.cause
                )),
            )
        }
        _ => (mode, None),
    }
}

/// [`sandbox_guard`] with the state of this machine and this process.
pub fn reviewer_mode(mode: Mode, anyway: bool) -> (Mode, Option<String>) {
    sandbox_guard(mode, semilla_normal::sandbox::missing().as_ref(), anyway)
}

/// The build code state of one step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepCode {
    pub index: usize,
    /// True if the old tree of the step has changed build code.
    pub before: bool,
    /// The changed parts of the new tree of the step against the base tree (empty: clean).
    pub parts: Vec<Part>,
}

impl StepCode {
    /// True if the check of the step uses a tree with changed build code: its old tree or its
    /// new tree.
    pub fn uses(&self) -> bool {
        self.before || !self.parts.is_empty()
    }
}

/// The build code state of a change set: for each tree, if its build code is the build code of
/// the base tree.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildCode {
    pub steps: Vec<StepCode>,
    /// The trees with changed build code. Only the separate folder set of the change set builds
    /// them, and not for a command with `--no-build-code`.
    pub trees: BTreeSet<String>,
    /// The trees with the build code of the base. The shared folders build only these trees.
    pub clean: BTreeSet<String>,
    /// The base tree of the change set.
    pub base: String,
    /// The key of the shared folder set: the digest of the build code state of the base tree
    /// ([`state_digest`]), or `tree-<base>` if Semilla cannot compute it.
    #[serde(default)]
    pub shared: String,
    /// The key of the separate folder set of each tree with changed build code
    /// ([`marked_key`]).
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

impl BuildCode {
    /// The changed files of a step that are files of a host package: the path and the cause.
    pub fn host_files(&self, step: &semilla_core::Step) -> Vec<(String, String)> {
        let Some(code) = self.steps.iter().find(|code| code.index == step.index) else { return Vec::new() };
        code.parts.iter().filter(|part| part.why == HOST_FILE).map(|part| (part.path.clone(), part.why.clone())).collect()
    }

    /// The paths of the build code that a step can change: the changed parts of its new tree
    /// and of its old tree. A fact of the step for one of these files is a build code change.
    pub fn change_paths(&self, step: usize) -> BTreeSet<String> {
        let position = self.steps.iter().position(|code| code.index == step);
        let previous = position.and_then(|position| position.checked_sub(1));
        [position, previous].into_iter().flatten().flat_map(|position| self.steps[position].parts.iter().map(|part| part.path.clone())).collect()
    }

    /// The note for a result, or `None` if no step uses changed build code. Each run of steps
    /// that use changed build code is one entry: its first step changes the build code. `ran` is
    /// true if the command runs the changed build code.
    pub fn note(&self, ran: bool) -> Option<semilla_core::report::BuildCodeNote> {
        let mut changes: Vec<semilla_core::report::BuildCodeChange> = Vec::new();
        let mut open = false;
        for step in &self.steps {
            if !step.uses() {
                open = false;
                continue;
            }
            if open && let Some(change) = changes.last_mut() {
                change.steps.push(step.index);
                continue;
            }
            let parts = if step.parts.is_empty() { "the tree before the step".to_string() } else { labels(&step.parts) };
            changes.push(semilla_core::report::BuildCodeChange { step: step.index, parts, steps: vec![step.index], to_end: false });
            open = true;
        }
        if let (Some(change), Some(last)) = (changes.last_mut(), self.steps.last()) {
            change.to_end = change.steps.last() == Some(&last.index);
        }
        (!changes.is_empty()).then_some(semilla_core::report::BuildCodeNote { changes, ran })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semilla_syntax::load::{CodeFile, FileEntry};

    /// A test tree: a temporary folder with the files on disk, and a workspace with the entries,
    /// the parsed files, and the code files.
    struct Tree {
        dir: tempfile::TempDir,
        ws: Workspace,
    }

    impl Tree {
        fn new() -> Tree {
            Tree { dir: tempfile::tempdir().unwrap(), ws: Workspace::default() }
        }

        /// Writes a plain file and records its entry.
        fn file(mut self, path: &str, text: &str) -> Tree {
            let full = self.dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, text).unwrap();
            self.ws.files.insert(path.to_string(), FileEntry { mode: "100644".into(), hash: hash(text), size: text.len() as u64 });
            self
        }

        /// Writes a Rust file that a target owns (the loader parses it).
        fn rust(self, path: &str, text: &str) -> Tree {
            let mut tree = self.file(path, text);
            tree.ws.parsed_files.insert(path.to_string());
            tree
        }

        /// Writes a build script file.
        fn script(self, package: &str, path: &str, text: &str) -> Tree {
            let mut tree = self.file(path, text);
            tree.ws.code_files.insert(path.to_string(), CodeFile { package: package.into(), kind: "custom-build".into(), target: "build-script-build".into(), outside: false });
            tree
        }

        fn side(&self) -> Side<'_> {
            Side { dir: self.dir.path(), ws: &self.ws }
        }
    }

    fn hash(text: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// A host info with no host package and the manifest unchanged: the default for the trees with
    /// no host code. The full metadata must never run for these cases, so the closure panics.
    fn no_full() -> impl FnMut(&std::path::Path) -> Result<HostInfo> {
        |_| panic!("the test must not run the full metadata")
    }

    /// A host info for a workspace with one path package at the root.
    fn root_host(host: bool, script: Option<&str>, features: &[&str]) -> HostInfo {
        let mut info = HostInfo::default();
        info.dirs.insert(String::new(), host);
        if host {
            info.packages.insert("p 0.1.0 path:".into(), (None, features.iter().map(|f| f.to_string()).collect()));
        }
        if let Some(script) = script {
            info.scripts.insert("p 0.1.0 path:".into(), script.to_string());
        }
        info
    }

    fn changed(base: &Tree, tree: &Tree, mut full: impl FnMut(&std::path::Path) -> Result<HostInfo>) -> Vec<Part> {
        changed_build_code(&base.side(), &tree.side(), &mut full).expect("cargo reads the dependency graph")
    }

    // Batch 5v, part F: the digest of the build code state. A change of the Rust code of a
    // target keeps the digest. A change of each other entry gives a new digest.
    #[test]
    fn the_state_digest_changes_with_each_entry_that_is_not_target_code() {
        let tree = || plain_base().file("notes.txt", "a\n").script("p", "build.rs", "fn main() {}\n");
        let host = root_host(false, Some("build.rs"), &[]);
        let digest = |tree: &Tree, host: &HostInfo| state_digest(&tree.side(), host);
        let first = digest(&tree(), &host);
        assert_eq!(first.len(), 32);
        assert_eq!(first, digest(&tree().rust("src/lib.rs", "pub fn f() -> u32 { 2 }\n"), &host), "target code is not build code");
        assert_eq!(first, digest(&tree().rust("src/new.rs", "pub fn g() {}\n"), &host), "a new file of a target is not build code");
        assert_ne!(first, digest(&tree().script("p", "build.rs", "fn main() { other(); }\n"), &host), "a build script");
        assert_ne!(first, digest(&tree().file("notes.txt", "b\n"), &host), "a file that build code can read");
        assert_ne!(first, digest(&tree().file("more.txt", "a\n"), &host), "a new file");
        assert_ne!(first, digest(&tree().file("Cargo.toml", "[package]\nname='p'\nversion='2.0.0'\n"), &host), "a manifest");
        assert_ne!(first, digest(&tree().file("stray.rs", "fn x() {}\n"), &host), "a Rust file that no target owns");
        // The code of a host package (a proc macro) is build code.
        assert_ne!(digest(&tree(), &root_host(true, None, &[])), digest(&tree().rust("src/lib.rs", "pub fn f() -> u32 { 2 }\n"), &root_host(true, None, &[])), "the code of a host package");
        // A file mode is a part of the state.
        let mut link = tree();
        link.ws.files.get_mut("src/lib.rs").unwrap().mode = "120000".into();
        assert_ne!(first, digest(&link, &host), "a link is not plain target code");
    }

    // Batch 5v, part F: the key of the folder set of a tree with changed build code.
    #[test]
    fn the_marked_key_has_the_content_of_each_changed_part() {
        let tree = |script: &str| plain_base().script("p", "build.rs", script);
        let part = |path: &str, why: &str| Part { path: path.into(), why: why.into(), plain: true };
        let parts = vec![part("build.rs", "a build script")];
        let key = |base: &str, name: &str, tree: &Tree, parts: &[Part], strict: bool| marked_key(base, name, parts, Some(&tree.ws.files), strict);
        let first = key("b1", "t1", &tree("fn main() {}\n"), &parts, false);
        assert_eq!(first.len(), 32);
        // A different tree with the same build code has the same key.
        assert_eq!(first, key("b1", "t2", &tree("fn main() {}\n").rust("src/lib.rs", "pub fn f() -> u32 { 2 }\n"), &parts, false));
        // The content of a part, the base, the list of the parts, and a manifest change the key.
        assert_ne!(first, key("b1", "t3", &tree("fn main() { other(); }\n"), &parts, false));
        assert_ne!(first, key("b2", "t1", &tree("fn main() {}\n"), &parts, false));
        assert_ne!(first, key("b1", "t1", &tree("fn main() {}\n").file("x.txt", "a\n"), &[parts[0].clone(), part("x.txt", "a file that build code can read")], false));
        assert_ne!(first, key("b1", "t1", &tree("fn main() {}\n").file("Cargo.lock", "a\n"), &parts, false));
        // The list of the parts is not complete after a configuration file: the key is the tree.
        assert_eq!(key("b1", "t1", &tree("fn main() {}\n"), &[part(".cargo/config.toml", CONFIG_FILE)], false), "tree-t1");
        assert_eq!(key("b1", "t1", &tree("fn main() {}\n"), &parts, true), "tree-t1");
        assert_eq!(marked_key("b1", "t1", &parts, None, false), "tree-t1");
    }

    // Batch 5n, A6: a failed read of the dependency graph is not "build code changed". It is a
    // failure with the error line of cargo, for each of the two trees.
    #[test]
    fn a_failed_read_of_the_dependency_graph_is_not_a_build_code_part() {
        let base = plain_base();
        let tree = plain_base().file("Cargo.toml", "[package]\nname='p'\n\n[dependencies]\n");
        let download = || anyhow::anyhow!("cargo cannot read the manifests in /x:\nerror: failed to download `hermit-abi v0.5.3`\n\nCaused by:\n  attempting to make an HTTP request, but --offline was specified");
        let mut of_base = |_: &std::path::Path| -> Result<HostInfo> { Err(download()) };
        let failure = changed_build_code(&base.side(), &tree.side(), &mut of_base).unwrap_err();
        assert_eq!(failure, GraphFailure { base: true, line: "error: failed to download `hermit-abi v0.5.3`".into() });
        let mut calls = 0;
        let mut of_tree = |_: &std::path::Path| -> Result<HostInfo> {
            calls += 1;
            if calls == 1 { Ok(root_host(false, None, &[])) } else { Err(download()) }
        };
        let failure = changed_build_code(&base.side(), &tree.side(), &mut of_tree).unwrap_err();
        assert_eq!(failure, GraphFailure { base: false, line: "error: failed to download `hermit-abi v0.5.3`".into() });
        assert_eq!(failure.to_string(), "cargo cannot read the dependency graph of the tree: error: failed to download `hermit-abi v0.5.3`");
    }

    // A tree with no build code in it. Each other file is clean (rule (d)).
    fn plain_base() -> Tree {
        Tree::new().file("Cargo.toml", "[package]\nname='p'\n").rust("src/lib.rs", "pub fn f(x: u64) -> u64 { x / 100 }\n")
    }

    #[test]
    fn clean_when_only_a_target_rust_file_changes() {
        let base = plain_base();
        let tree = Tree::new().file("Cargo.toml", "[package]\nname='p'\n").rust("src/lib.rs", "pub fn f(x: u64) -> u64 { x / 10 }\n");
        assert!(changed(&base, &tree, |_| Ok(root_host(false, None, &[]))).is_empty());
    }

    // Decision 3 of batch 5j: a change of `package.version` stays clean.
    #[test]
    fn clean_when_the_package_version_changes() {
        let manifest = |version: &str| format!("[package]\nname = 'p'\nversion = '{version}'\nedition = '2024'\n");
        let base = Tree::new().file("Cargo.toml", &manifest("0.1.0")).file("Cargo.lock", "version = 4\n\n[[package]]\nname = \"p\"\nversion = \"0.1.0\"\n").rust("src/lib.rs", "pub fn f() {}\n");
        let tree = Tree::new().file("Cargo.toml", &manifest("0.2.0")).file("Cargo.lock", "version = 4\n\n[[package]]\nname = \"p\"\nversion = \"0.2.0\"\n").rust("src/lib.rs", "pub fn f() {}\n");
        assert_eq!(changed(&base, &tree, |_| Ok(root_host(false, None, &[]))), Vec::new());
    }

    // Decision 3 of batch 5j: a new registry dependency that is not a host package and has no
    // build script stays clean.
    #[test]
    fn clean_when_a_plain_registry_dependency_comes_in() {
        let manifest = |deps: &str| format!("[package]\nname = 'p'\nversion = '0.1.0'\n{deps}");
        let lock = |more: &str| format!("version = 4\n\n[[package]]\nname = \"p\"\nversion = \"0.1.0\"\n{more}");
        let base = Tree::new().file("Cargo.toml", &manifest("")).file("Cargo.lock", &lock("")).rust("src/lib.rs", "pub fn f() {}\n");
        let tree = Tree::new()
            .file("Cargo.toml", &manifest("\n[dependencies]\nitoa = \"1\"\n"))
            .file("Cargo.lock", &lock("dependencies = [\n \"itoa\",\n]\n\n[[package]]\nname = \"itoa\"\nversion = \"1.0.15\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"4a5f13b858c8d314ee3e8f639011f7ccefe71f97f96e50151fb991f267928e2c\"\n"))
            .rust("src/lib.rs", "pub fn f() {}\n");
        assert_eq!(changed(&base, &tree, |_| Ok(root_host(false, None, &[]))), Vec::new());
        // The same dependency as a build dependency of a package with a build script is a host
        // package: the tree has changed build code.
        let mut host = root_host(false, None, &[]);
        host.packages.insert("itoa 1.0.15 registry".into(), (Some("4a5f".into()), Vec::new()));
        let mut answers = vec![host, root_host(false, None, &[])];
        let parts = changed(&base, &tree, |_| Ok(answers.pop().unwrap()));
        assert!(parts.iter().any(|part| part.why.contains("a new host package")), "{parts:?}");
    }

    #[test]
    fn clean_when_nothing_changes() {
        let base = plain_base();
        let tree = plain_base();
        assert!(changed(&base, &tree, no_full()).is_empty());
    }

    // 1. An edit of `build.rs`.
    #[test]
    fn edit_of_a_build_script() {
        let base = plain_base().script("p", "build.rs", "fn main() {}\n");
        let tree = plain_base().script("p", "build.rs", "fn main() { evil(); }\n");
        let parts = changed(&base, &tree, |_| Ok(root_host(false, Some("build.rs"), &[])));
        assert!(parts.iter().any(|p| p.path == "build.rs"), "{parts:?}");
    }

    // 2. A new `build.rs` (the manifest did not change, so the test sees the new file as code).
    #[test]
    fn new_build_script() {
        let base = plain_base();
        let tree = plain_base().script("p", "build.rs", "fn main() { evil(); }\n");
        let parts = changed(&base, &tree, |dir| {
            let host = if dir.join("build.rs").exists() { Some("build.rs") } else { None };
            Ok(root_host(false, host, &[]))
        });
        assert!(parts.iter().any(|p| p.path == "build.rs"), "{parts:?}");
    }

    // 3. `package.build = "NOTES.md"`: the manifest changes a build key.
    #[test]
    fn build_key_names_a_neutral_file() {
        let base = plain_base().file("NOTES.md", "a\n");
        let tree = plain_base().file("Cargo.toml", "[package]\nname='p'\nbuild='NOTES.md'\n").file("NOTES.md", "a\n").script("p", "NOTES.md", "echo evil\n");
        // The manifest changed, so the full metadata of the tree names the new build script.
        let parts = changed(&base, &tree, |dir| {
            let script = if dir.join("Cargo.toml").to_string_lossy().contains("build") { Some("NOTES.md") } else { None };
            let text = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
            Ok(root_host(false, if text.contains("build=") { Some("NOTES.md") } else { None }.or(script).and(Some("NOTES.md")), &[]))
        });
        assert!(!parts.is_empty(), "the new build script must give changed: {parts:?}");
    }

    // 7. A new feature of a host package.
    #[test]
    fn new_feature_of_a_host_package() {
        let base = plain_base().file("Cargo.lock", "a\n");
        let tree = plain_base().file("Cargo.lock", "b\n");
        let parts = changed(&base, &tree, |dir| {
            let lock = std::fs::read_to_string(dir.join("Cargo.lock")).unwrap_or_default();
            Ok(root_host(true, None, if lock.trim() == "b" { &["serde"] } else { &[] }))
        });
        assert!(parts.iter().any(|p| p.why.contains("features")), "{parts:?}");
    }

    // 8. A new or changed `.cargo/config.toml`.
    #[test]
    fn changed_cargo_config() {
        let base = plain_base();
        let tree = plain_base().file(".cargo/config.toml", "[build]\nrustc-wrapper='x'\n");
        let parts = changed(&base, &tree, no_full());
        assert!(parts.iter().any(|p| p.path == ".cargo/config.toml"), "{parts:?}");
    }

    // A changed file whose name a configuration file holds.
    #[test]
    fn file_named_by_a_configuration() {
        let base = plain_base().file(".cargo/config.toml", "[build]\nrustc-wrapper='tools/run.sh'\n").file("tools/run.sh", "a\n");
        let tree = plain_base().file(".cargo/config.toml", "[build]\nrustc-wrapper='tools/run.sh'\n").file("tools/run.sh", "evil\n");
        let parts = changed(&base, &tree, no_full());
        assert!(parts.iter().any(|p| p.path == "tools/run.sh"), "{parts:?}");
    }

    // 9. A changed `gen.sh` in a tree that has a build script (rule (d), the tree has build code).
    #[test]
    fn other_file_changed_with_a_build_script_present() {
        let base = plain_base().script("p", "build.rs", "fn main() {}\n").file("gen.sh", "a\n");
        let tree = plain_base().script("p", "build.rs", "fn main() {}\n").file("gen.sh", "evil\n");
        let parts = changed(&base, &tree, |_| Ok(root_host(false, Some("build.rs"), &[])));
        assert!(parts.iter().any(|p| p.path == "gen.sh"), "{parts:?}");
    }

    // The same `gen.sh` change is clean when the tree has no build code (rule (d)).
    #[test]
    fn other_file_changed_with_no_build_code() {
        let base = plain_base().file("gen.sh", "a\n");
        let tree = plain_base().file("gen.sh", "evil\n");
        let parts = changed(&base, &tree, |_| Ok(root_host(false, None, &[])));
        assert!(parts.is_empty(), "{parts:?}");
    }

    // A new proc macro dependency: the host package set grows (the manifest and the lock change).
    #[test]
    fn new_host_package() {
        let base = plain_base().file("Cargo.lock", "old\n");
        let tree = plain_base().file("Cargo.toml", "[package]\nname='p'\n[dependencies]\nserde_derive='1'\n").file("Cargo.lock", "new\n");
        let parts = changed(&base, &tree, |dir| {
            let mut info = root_host(false, None, &[]);
            let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
            if manifest.contains("serde_derive") {
                info.packages.insert("serde_derive 1.0.0 registry+https://github.com/rust-lang/crates.io-index".into(), (Some("sum".into()), vec![]));
            }
            Ok(info)
        });
        assert!(parts.iter().any(|p| p.why.contains("new host package")), "{parts:?}");
    }

    // A tree that goes back to the build code of the base is clean again.
    #[test]
    fn back_to_the_base_is_clean() {
        let base = plain_base().script("p", "build.rs", "fn main() {}\n");
        let tree = plain_base().script("p", "build.rs", "fn main() {}\n").rust("src/lib.rs", "pub fn f(x: u64) -> u64 { x / 10 }\n");
        // Only src/lib.rs changed (a target file); build.rs is equal.
        let parts = changed(&base, &tree, |_| Ok(root_host(false, Some("build.rs"), &[])));
        assert!(parts.is_empty(), "{parts:?}");
    }

    // An edit in a proc macro crate of the workspace: a file in the folder of a host package.
    #[test]
    fn edit_in_a_host_package_folder() {
        let base = Tree::new().file("Cargo.toml", "[workspace]\n").file("macros/Cargo.toml", "[package]\nname='macros'\n").rust("macros/src/lib.rs", "pub fn m() {}\n");
        let tree = Tree::new().file("Cargo.toml", "[workspace]\n").file("macros/Cargo.toml", "[package]\nname='macros'\n").rust("macros/src/lib.rs", "pub fn m() { evil() }\n");
        let host = || {
            let mut info = HostInfo::default();
            info.dirs.insert("macros".into(), true);
            info.packages.insert("macros 0.1.0 path:macros".into(), (None, vec![]));
            info.roots.insert("macros/src/lib.rs".into());
            info
        };
        let parts = changed(&base, &tree, move |_| Ok(host()));
        assert!(parts.iter().any(|p| p.path == "macros/src/lib.rs"), "{parts:?}");
    }

    #[test]
    fn the_base_line_names_the_commits_and_the_bound() {
        // Design 28.5. No commit changed build code, and the walk got to the start: no line.
        assert_eq!(base_code_line(&[], false), None);
        // The walk stopped at the bound: the output says so, with a finding and with no finding.
        assert_eq!(base_code_line(&[], true).unwrap(), "base build code: no change in the 20 commits before this change set (Semilla examines no more commits)");
        let one = [("abc1234".to_string(), vec!["crates/core/build.rs".to_string()])];
        assert_eq!(
            base_code_line(&one, false).unwrap(),
            "! the base has build code that a recent commit changed: `abc1234` (crates/core/build.rs). Each check of this change set ran this code. No mark shows this, because the change is before the base."
        );
        assert!(base_code_line(&one, true).unwrap().ends_with("because the change is before the base. Semilla examined only the 20 commits before this change set."));
        // A long list shows three entries and the number of the others.
        let files: Vec<String> = (0..5).map(|n| format!("f{n}")).collect();
        let many: Vec<(String, Vec<String>)> = (0..4).map(|n| (format!("c{n}"), files.clone())).collect();
        let line = base_code_line(&many, false).unwrap();
        assert!(line.contains("`c0` (f0, f1, f2, and 2 more); `c1` (") && line.contains("`c2` (f0, f1, f2, and 2 more); and 1 more. Each check"), "{line}");
    }

    #[test]
    fn a_reviewer_command_runs_no_changed_build_code_with_no_sandbox() {
        use semilla_normal::sandbox::{Limit, Missing};
        // Design 28.2. The full sandbox is the protection for the default.
        assert_eq!(sandbox_guard(Mode::Run, None, false), (Mode::Run, None));
        let all = Missing { limits: vec![Limit::Write, Limit::Read, Limit::Network], cause: "no Landlock".into() };
        let (mode, line) = sandbox_guard(Mode::Run, Some(&all), false);
        assert_eq!(mode, Mode::NoBuildCode);
        assert_eq!(
            line.unwrap(),
            "The build sandbox is not complete: the write limit, the read limit, and the network limit are missing (no Landlock). The command does not run changed build code. To run it with this sandbox, use the option --build-code-no-sandbox."
        );
        // One missing limit is sufficient.
        let one = Missing { limits: vec![Limit::Network], cause: "no seccomp".into() };
        let (mode, line) = sandbox_guard(Mode::Run, Some(&one), false);
        assert_eq!(mode, Mode::NoBuildCode);
        assert!(line.unwrap().starts_with("The build sandbox is not complete: the network limit is missing (no seccomp)."));
        // The option runs the code with the limits that the machine has.
        assert_eq!(sandbox_guard(Mode::Run, Some(&all), true), (Mode::Run, None));
        // The careful option needs no line.
        assert_eq!(sandbox_guard(Mode::NoBuildCode, Some(&all), false), (Mode::NoBuildCode, None));
    }

    #[test]
    fn the_note_has_one_entry_for_each_run_of_steps() {
        let part = vec![Part::file("build.rs", "a file of a build script")];
        let step = |index: usize, before: bool, parts: &[Part]| StepCode { index, before, parts: parts.to_vec() };
        // Case 7: the build code changes in step 1 and goes back in step 3.
        let code = BuildCode { steps: vec![step(0, false, &[]), step(1, false, &part), step(2, true, &part), step(3, true, &[]), step(4, false, &[])], ..Default::default() };
        let note = code.note(false).unwrap();
        assert_eq!(note.changes.len(), 1);
        assert_eq!((note.changes[0].step, note.changes[0].steps.clone(), note.changes[0].to_end), (1, vec![1, 2, 3], false));
        assert_eq!(note.not_checked(), vec![1, 2, 3]);
        assert_eq!(note.changes[0].span(), "in steps 1–3");
        // The build code changes in step 1 and stays.
        let code = BuildCode { steps: vec![step(0, false, &[]), step(1, false, &part), step(2, true, &part)], ..Default::default() };
        let note = code.note(true).unwrap();
        assert_eq!(note.changes[0].span(), "from step 1 on");
        assert!(note.not_checked().is_empty());
        assert_eq!(note.tag_text(), "build code changed from step 1");
        assert!(note.step_line(1).unwrap().starts_with("! build code changed in step 1: build.rs. It runs in the sandbox during each check, and it can change each result from step 1 on."));
        // The mark is one mark for the step: each step from step 1 on has the mark of step 1.
        assert_eq!((note.mark_of(0), note.mark_of(1), note.mark_of(2)), (None, Some(1), Some(1)));
        // A command that does not run the build code gives no mark.
        assert_eq!(code.note(false).unwrap().mark_of(2), None);
        // A fact for `build.rs` is a build code change in step 1, and in step 2 if step 2
        // changes the file again. Step 0 has no build code change.
        assert!(code.change_paths(0).is_empty());
        assert!(code.change_paths(1).contains("build.rs") && code.change_paths(2).contains("build.rs"));
        assert!(note.step_line(2).unwrap().starts_with("! build code changed in step 1 (build.rs)"));
        assert_eq!(note.step_line(0), None);
        // A clean change set has no note.
        assert_eq!(BuildCode { steps: vec![step(0, false, &[])], ..Default::default() }.note(true), None);
    }
}
