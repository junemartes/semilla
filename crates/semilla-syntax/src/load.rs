//! Loads a Cargo workspace into the entity tree: crates, modules, and items.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use syn::spanned::Spanned;

use crate::tree::{self, Node, tokens};

/// One member crate of the workspace.
#[derive(Clone, Debug, Serialize)]
pub struct CrateInfo {
    /// The crate name, with `-` replaced by `_`.
    pub name: String,
    /// The package folder, relative to the workspace root.
    pub dir: String,
    /// The names of the other member crates that this crate depends on.
    pub deps: Vec<String>,
}

/// The kind of an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Fn,
    Struct,
    Enum,
    Trait,
    Impl,
    Const,
    Static,
    Type,
    Use,
    Mod,
    Macro,
    /// A field of a struct or of a variant: `shop::Account.balance`, `shop::Pair.0`,
    /// `shop::Event::Close.reason`.
    Field,
    /// A variant of an enum: `shop::Event::Open`.
    Variant,
    Other,
}

/// One item of the workspace.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    /// The item path, for example `cart::Cart::total`. Methods in a trait impl get
    /// `cart::<Cart as Display>::fmt`. `use` items get a path with their position.
    ///
    /// Twins (two items of one module with the same path, for example `cfg` variants, or two
    /// `impl` blocks of one type) get the suffix `#N`, with N the position of the twin in the
    /// source: `sys::sep#0`, `sys::sep#1`, and `sys::imp#1::open` in the second of two twin
    /// modules. An item without a twin keeps the plain path.
    pub path: String,
    pub kind: ItemKind,
    pub crate_name: String,
    /// The file, relative to the workspace root.
    pub file: String,
    pub line_start: usize,
    pub line_end: usize,
    /// The column of the first character of the item in its first line, and the column after
    /// its last character in its last line. A column is a count of characters from 0.
    #[serde(skip)]
    pub col_start: usize,
    #[serde(skip)]
    pub col_end: usize,
    /// The visibility text (`pub`, `pub(crate)`, or empty).
    pub vis: String,
    /// The token text of the item without its body, doc comments, and visibility.
    pub signature: String,
    /// The doc comments and the plain comments inside the item.
    pub docs: Vec<String>,
    /// The attributes, except doc comments.
    pub attrs: Vec<String>,
    /// The fields of a struct or the variants of an enum.
    pub members: Vec<String>,
    /// The body of a function.
    pub body: Option<Node>,
    /// The signature of a function, with its parts as children (`sig/param[a]`, `sig/ret`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<Node>,
    /// The full token text, for equality.
    pub text: String,
    /// The source text, for views.
    pub source: String,
    /// True for test code: `#[test]` functions, `#[cfg(test)]` modules, and files in `tests/`.
    pub test: bool,
}

/// The loaded workspace.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Workspace {
    pub crates: Vec<CrateInfo>,
    /// The items by path.
    pub items: BTreeMap<String, Item>,
    /// All files that are not Rust files, by relative path, with their content (UTF-8 files only).
    /// A Rust file is a `.rs` file, or a file that a target or a module names ([`Workspace::is_rust_file`]).
    pub other_files: BTreeMap<String, String>,
    /// All Rust files, by relative path, with their content (UTF-8 files only).
    pub rust_files: BTreeMap<String, String>,
    /// All entries of the tree (each file, also a binary file, a symbolic link, or a submodule),
    /// by relative path, with their mode and content hash. The diff compares them, so that each
    /// changed path gives an edit. [`Workspace::load`] reads them from the folder. The app
    /// replaces them with the entries of the git tree.
    pub files: BTreeMap<String, FileEntry>,
    /// The Rust files that the loader parsed. The syntax diff decides about their content.
    pub parsed_files: BTreeSet<String>,
    /// The cargo targets of the member packages (from `cargo metadata` in the app).
    pub targets: Vec<Target>,
    /// The files that are Rust code, but that the loader does not parse into items: the root and
    /// the module files of a build script, an example, and a bench, the files that they include
    /// with `include!`, and the target files of a package of the tree that is not a member. The
    /// name of a file does not decide this (brief 5h, A4).
    pub code_files: BTreeMap<String, CodeFile>,
    /// The inner attributes (`#![…]`) of each crate root and module file, by module path, with
    /// the file. An inline module has them in its own attributes.
    pub inner_attrs: BTreeMap<String, (String, Vec<String>)>,
}

/// One cargo target of a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Target {
    /// The crate name of the package (with `-` replaced by `_`).
    pub package: String,
    /// The target name.
    pub name: String,
    /// `lib`, `bin`, `test`, `example`, `bench`, or `custom-build` (a build script).
    pub kind: String,
    /// The root file, relative to the workspace root.
    pub file: String,
}

/// A file with Rust code that the loader does not parse into items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CodeFile {
    /// The crate name of the package, or the package folder for a package that is not a member.
    pub package: String,
    /// The kind of the target: `custom-build`, `example`, `bench`, or `lib`, `bin`, `test` for a
    /// package that is not a member.
    pub kind: String,
    /// The target name.
    pub target: String,
    /// True for a package of the tree that is not a member of the workspace.
    pub outside: bool,
}

impl CodeFile {
    /// True for a file of a build script.
    pub fn is_build_script(&self) -> bool {
        self.kind == "custom-build"
    }

    /// What the file is, for the reviewer.
    pub fn role(&self) -> String {
        let package = if self.outside { format!("the package in `{}` (not a workspace member)", self.package) } else { format!("`{}`", self.package) };
        match self.kind.as_str() {
            "custom-build" => format!("the build script of {package}"),
            kind => format!("the {kind} target `{}` of {package}", self.target),
        }
    }
}

/// One package for the loader: the folder, the crate name, the member crates that it depends
/// on, and the targets (kind, name, root file).
struct Package {
    dir: PathBuf,
    name: String,
    deps: Vec<String>,
    targets: Vec<(String, String, PathBuf)>,
}

/// One entry of a tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    /// The git mode: `100644` (a file), `100755` (an executable file), `120000` (a symbolic
    /// link), or `160000` (a submodule).
    pub mode: String,
    /// The content hash (the git object hash in the app). Equal content gives an equal hash.
    pub hash: String,
    /// The size in bytes (0 for a submodule).
    pub size: u64,
}

impl Workspace {
    /// Loads the workspace at `root`. The packages and the targets come from the manifests, with
    /// the rules of cargo for the default targets. The app uses [`Workspace::load_with_metadata`].
    pub fn load(root: &Path) -> Result<Workspace> {
        let mut packages = Vec::new();
        let found = find_packages(root)?;
        let names: Vec<String> = found.iter().map(|(_, name, _)| name.clone()).collect();
        for (dir, name, manifest) in found {
            let deps = manifest.get("dependencies").and_then(|deps| deps.as_table()).map(|deps| deps.keys().map(|key| key.replace('-', "_")).filter(|key| names.contains(key)).collect()).unwrap_or_default();
            let targets = manifest_targets(&dir, &manifest);
            packages.push(Package { dir, name, deps, targets });
        }
        Workspace::load_packages(root, packages)
    }

    /// Loads the workspace at `root` with the packages and the targets that cargo reports
    /// (`cargo metadata --no-deps --format-version 1`). So Semilla and cargo agree on each root
    /// file (invariant I4): `lib.path`, `[[bin]] path`, `package.build`, and each default target.
    pub fn load_with_metadata(root: &Path, metadata: &serde_json::Value) -> Result<Workspace> {
        let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        // A path of the metadata, as a path below `root`. `None` for a path outside the tree.
        let inside = |path: &str| -> Option<PathBuf> {
            let path = Path::new(path);
            let rel = path.strip_prefix(&real_root).or_else(|_| path.strip_prefix(root)).ok()?;
            Some(root.join(rel))
        };
        let list = metadata.get("packages").and_then(|p| p.as_array()).context("the cargo metadata has no package list")?;
        let crate_name = |package: &serde_json::Value| package.get("name").and_then(|n| n.as_str()).unwrap_or("").replace('-', "_");
        let names: Vec<String> = list.iter().map(crate_name).collect();
        let mut packages = Vec::new();
        for package in list {
            let Some(dir) = package.get("manifest_path").and_then(|p| p.as_str()).and_then(inside).and_then(|path| path.parent().map(Path::to_path_buf)) else { continue };
            let deps: Vec<String> = package
                .get("dependencies")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
                .filter(|dep| dep.get("kind").is_none_or(|kind| kind.is_null()))
                .filter_map(|dep| dep.get("name").and_then(|n| n.as_str()).map(|n| n.replace('-', "_")))
                .filter(|name| names.contains(name))
                .collect();
            let mut targets = Vec::new();
            for target in package.get("targets").and_then(|t| t.as_array()).into_iter().flatten() {
                let kinds: Vec<&str> = target.get("kind").and_then(|k| k.as_array()).into_iter().flatten().filter_map(|k| k.as_str()).collect();
                let kind = match kinds.first().copied() {
                    Some(kind @ ("bin" | "test" | "example" | "bench" | "custom-build")) => kind,
                    // `lib`, `rlib`, `dylib`, `cdylib`, `staticlib`, and `proc-macro`.
                    _ => "lib",
                };
                let name = target.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                if let Some(file) = target.get("src_path").and_then(|p| p.as_str()).and_then(inside) {
                    targets.push((kind.to_string(), name, file));
                }
            }
            packages.push(Package { dir, name: crate_name(package), deps, targets });
        }
        packages.sort_by(|a, b| a.dir.cmp(&b.dir));
        Workspace::load_packages(root, packages)
    }

    fn load_packages(root: &Path, packages: Vec<Package>) -> Result<Workspace> {
        let mut workspace = Workspace::default();
        let mut items = Vec::new();
        for package in &packages {
            workspace.crates.push(CrateInfo { name: package.name.clone(), dir: relative(root, &package.dir), deps: package.deps.clone() });
            for (kind, name, file) in &package.targets {
                workspace.targets.push(Target { package: package.name.clone(), name: name.clone(), kind: kind.clone(), file: relative(root, file) });
            }
        }
        // The targets that the loader parses into items: each library, binary, test, example,
        // and bench target. These are the targets that `cargo check --all-targets` compiles, so
        // each MIR body of the workspace has a source item (batch 5n, A4). The file name does
        // not decide: `[[bin]] path = "LICENSE.txt"` is Rust code.
        let parsed_kind = |kind: &str| matches!(kind, "lib" | "bin" | "test" | "example" | "bench");
        for package in &packages {
            for (kind, name, file) in &package.targets {
                if !parsed_kind(kind) || workspace.parsed_files.contains(&relative(root, file)) || !file.exists() {
                    continue;
                }
                // The module path of the root. The library and `src/main.rs` are the crate. Each
                // other target is a module of the package with its name. A target can have the
                // name of the package (`benches/hex.rs` of the package `hex`).
                let main = *file == package.dir.join("src/main.rs");
                let module = if kind == "lib" || main { package.name.clone() } else { format!("{}::{}", package.name, name.replace('-', "_")) };
                let mut loader = Loader { root, items: &mut items, parsed: &mut workspace.parsed_files, inner_attrs: &mut workspace.inner_attrs, crate_name: &package.name };
                loader.load_file(file, &module, matches!(kind.as_str(), "test" | "bench"))?;
            }
        }
        // The other targets (the build scripts) are Rust code that the loader does not parse
        // into items.
        for package in &packages {
            for (kind, name, file) in &package.targets {
                if !parsed_kind(kind) {
                    let code = CodeFile { package: package.name.clone(), kind: kind.clone(), target: name.clone(), outside: false };
                    walk_code(root, file, &code, &mut workspace);
                }
            }
        }
        // A package of the tree that is not a member (a `path` dependency, a `[patch]` source):
        // cargo can build its targets, so each target file is Rust code.
        let members: BTreeSet<PathBuf> = packages.iter().map(|package| package.dir.clone()).collect();
        let mut manifests = Vec::new();
        find_manifests(root, root, &mut manifests);
        for dir in manifests.into_iter().filter(|dir| !members.contains(dir)) {
            let Ok(manifest) = read_manifest(&dir.join("Cargo.toml")) else { continue };
            if !manifest.contains_key("package") {
                continue;
            }
            for (kind, name, file) in manifest_targets(&dir, &manifest) {
                let code = CodeFile { package: relative(root, &dir), kind, target: name, outside: true };
                walk_code(root, &file, &code, &mut workspace);
            }
        }
        workspace.items = unique_paths(items);
        collect_files(root, root, &mut workspace)?;
        Ok(workspace)
    }

    /// True if the file has Rust code in this tree: a `.rs` file, a file that the loader parsed,
    /// or a file of a target that the loader does not parse.
    pub fn is_rust_file(&self, file: &str) -> bool {
        file.ends_with(".rs") || self.parsed_files.contains(file) || self.code_files.contains_key(file)
    }

    /// The items of one crate.
    pub fn crate_items<'a>(&'a self, crate_name: &'a str) -> impl Iterator<Item = &'a Item> + 'a {
        self.items.values().filter(move |item| item.crate_name == crate_name)
    }

    /// Finds the function whose body contains a file position.
    pub fn item_at(&self, file: &str, line: usize) -> Option<&Item> {
        self.items
            .values()
            .filter(|item| item.file == file && item.line_start <= line && line <= item.line_end)
            .min_by_key(|item| item.line_end - item.line_start)
    }
}

/// Returns (folder, crate name, manifest) for each package of the workspace.
fn find_packages(root: &Path) -> Result<Vec<(PathBuf, String, toml::Table)>> {
    let manifest = read_manifest(&root.join("Cargo.toml"))?;
    let mut dirs = Vec::new();
    if let Some(members) = manifest.get("workspace").and_then(|ws| ws.get("members")).and_then(|m| m.as_array()) {
        for member in members.iter().filter_map(|member| member.as_str()) {
            if let Some(prefix) = member.strip_suffix("/*") {
                let base = root.join(prefix);
                if let Ok(entries) = std::fs::read_dir(&base) {
                    let mut found: Vec<PathBuf> =
                        entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).filter(|path| path.join("Cargo.toml").exists()).collect();
                    found.sort();
                    dirs.extend(found);
                }
            } else {
                dirs.push(root.join(member));
            }
        }
    }
    if manifest.contains_key("package") {
        dirs.insert(0, root.to_path_buf());
    }
    let mut packages = Vec::new();
    for dir in dirs {
        let manifest = read_manifest(&dir.join("Cargo.toml"))?;
        let Some(name) = manifest.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) else {
            continue;
        };
        packages.push((dir, name.replace('-', "_"), manifest));
    }
    Ok(packages)
}

fn read_manifest(path: &Path) -> Result<toml::Table> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// The targets of a package from its manifest, with the rules of cargo for the default targets:
/// (kind, name, root file). Only a file that exists is a target.
///
/// - The library: `lib.path`, else `src/lib.rs`.
/// - The binaries: `src/main.rs`, `src/bin/*.rs`, `src/bin/*/main.rs` (unless `autobins = false`),
///   and each `[[bin]]`.
/// - The tests, the examples, and the benches: the files of `tests/`, `examples/`, `benches/`
///   (unless `autotests`, `autoexamples`, or `autobenches` is false), and each table of the kind.
/// - The build script: `package.build`, else `build.rs`.
fn manifest_targets(dir: &Path, manifest: &toml::Table) -> Vec<(String, String, PathBuf)> {
    let package = manifest.get("package");
    let package_name = package.and_then(|p| p.get("name")).and_then(|n| n.as_str()).unwrap_or("").to_string();
    let mut targets: Vec<(String, String, PathBuf)> = Vec::new();
    let lib = manifest.get("lib");
    let lib_path = lib.and_then(|l| l.get("path")).and_then(|p| p.as_str()).map(|p| dir.join(p)).unwrap_or_else(|| dir.join("src/lib.rs"));
    let lib_name = lib.and_then(|l| l.get("name")).and_then(|n| n.as_str()).unwrap_or(&package_name).to_string();
    targets.push(("lib".into(), lib_name, lib_path));
    for (kind, table, folder, auto) in [("bin", "bin", "src/bin", "autobins"), ("test", "test", "tests", "autotests"), ("example", "example", "examples", "autoexamples"), ("bench", "bench", "benches", "autobenches")] {
        let auto = package.and_then(|p| p.get(auto)).and_then(|v| v.as_bool()).unwrap_or(true);
        let mut found: Vec<(String, PathBuf)> = Vec::new();
        if auto {
            if kind == "bin" {
                found.push((package_name.clone(), dir.join("src/main.rs")));
            }
            if let Ok(entries) = std::fs::read_dir(dir.join(folder)) {
                let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
                paths.sort();
                for path in paths {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    if path.is_dir() {
                        found.push((stem, path.join("main.rs")));
                    } else if path.extension().is_some_and(|x| x == "rs") {
                        found.push((stem, path));
                    }
                }
            }
        }
        for target in manifest.get(table).and_then(|t| t.as_array()).into_iter().flatten() {
            let name = target.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let path = match target.get("path").and_then(|p| p.as_str()) {
                Some(path) => dir.join(path),
                None => {
                    let candidates = [dir.join(folder).join(format!("{name}.rs")), dir.join(folder).join(&name).join("main.rs"), dir.join("src/main.rs")];
                    candidates.iter().take(if kind == "bin" && name == package_name { 3 } else { 2 }).find(|path| path.exists()).cloned().unwrap_or_else(|| candidates[0].clone())
                }
            };
            found.retain(|(other, file)| *other != name && *file != path);
            found.push((name, path));
        }
        targets.extend(found.into_iter().map(|(name, path)| (kind.to_string(), name, path)));
    }
    match package.and_then(|p| p.get("build")) {
        Some(toml::Value::String(path)) => targets.push(("custom-build".into(), "build-script-build".into(), dir.join(path))),
        Some(toml::Value::Boolean(false)) => {}
        _ => targets.push(("custom-build".into(), "build-script-build".into(), dir.join("build.rs"))),
    }
    targets.retain(|(_, _, path)| path.is_file());
    targets
}

/// Collects each folder below `dir` that has a `Cargo.toml`, except `.git`, `.semilla`, and the
/// cargo build folders.
fn find_manifests(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let link = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink());
        if link || name.starts_with(".semilla") || name == ".git" {
            continue;
        }
        if path.is_dir() {
            if !((dir == root && name == "target") || path.join("CACHEDIR.TAG").exists()) {
                find_manifests(root, &path, found);
            }
        } else if name == "Cargo.toml" {
            found.push(dir.to_path_buf());
        }
    }
}

/// Records the root file of a target that the loader does not parse into items, its module
/// files, and the files that it includes with `include!`. A file that does not parse is still a
/// code file: the compiler reads it.
fn walk_code(root: &Path, file: &Path, code: &CodeFile, workspace: &mut Workspace) {
    fn modules(root: &Path, items: &[syn::Item], file: &Path, dir: &Path, top: bool, code: &CodeFile, workspace: &mut Workspace) {
        for item in items {
            let syn::Item::Mod(item_mod) = item else { continue };
            let name = item_mod.ident.to_string();
            match &item_mod.content {
                Some((_, content)) => modules(root, content, file, &dir.join(&name), false, code, workspace),
                None => {
                    let candidates = match path_attr(&item_mod.attrs) {
                        Some(target) => vec![if top { file.parent().unwrap_or(Path::new(".")).join(target) } else { dir.join(target) }],
                        None => vec![dir.join(format!("{name}.rs")), dir.join(&name).join("mod.rs")],
                    };
                    if let Some(found) = candidates.iter().find(|candidate| candidate.is_file()) {
                        let child_dir = if path_attr(&item_mod.attrs).is_some() { found.parent().map(Path::to_path_buf).unwrap_or_default() } else { module_dir(found) };
                        visit(root, found, &child_dir, code, workspace);
                    }
                }
            }
        }
    }
    fn visit(root: &Path, file: &Path, child_dir: &Path, code: &CodeFile, workspace: &mut Workspace) {
        let rel = relative(root, file);
        if workspace.parsed_files.contains(&rel) || workspace.code_files.contains_key(&rel) || !file.is_file() {
            return;
        }
        workspace.code_files.insert(rel, code.clone());
        let Ok(text) = std::fs::read_to_string(file) else { return };
        // `include!("…")` with a literal path: the file is code of this target.
        for (start, _) in text.match_indices("include!") {
            let boundary = text[..start].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            let rest = text[start + "include!".len()..].trim_start().trim_start_matches(['(', '[', '{']).trim_start();
            if let Some(path) = rest.strip_prefix('"').and_then(|rest| rest.split('"').next()).filter(|_| boundary) {
                let included = file.parent().unwrap_or(Path::new(".")).join(path);
                let dir = included.parent().map(Path::to_path_buf).unwrap_or_default();
                visit(root, &included, &dir, code, workspace);
            }
        }
        if let Ok(parsed) = syn::parse_file(&text) {
            modules(root, &parsed.items, file, child_dir, true, code, workspace);
        }
    }
    // The child modules of a root file are in the folder of the root file.
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    visit(root, file, &dir, code, workspace);
}

/// The files of the module tree of one target root, relative to `root`: the root file, each
/// module file (also with `#[path]`), and each file that `include!` reads. The list does not
/// depend on the other targets of the tree. A file that does not exist is not in the list.
pub fn module_files(root: &Path, file: &Path) -> BTreeSet<String> {
    let mut workspace = Workspace::default();
    let code = CodeFile { package: String::new(), kind: String::new(), target: String::new(), outside: false };
    walk_code(root, file, &code, &mut workspace);
    workspace.code_files.into_keys().collect()
}

/// The lines of a doc text in the source form: each line with the marker `///` or `//!`.
fn doc_lines(marker: &str, text: &str) -> String {
    if text.is_empty() {
        return marker.to_string();
    }
    text.lines().map(|line| format!("{marker}{line}")).collect::<Vec<_>>().join("\n")
}

fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
    if rel.is_empty() { ".".to_string() } else { rel }
}

/// Collects all files of the workspace, except `.git`, `.semilla`, and the cargo build folders
/// (`target` at the root, or a folder with a `CACHEDIR.TAG` file).
fn collect_files(root: &Path, dir: &Path, workspace: &mut Workspace) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with(".semilla") || name == ".git" {
            continue;
        }
        let rel = relative(root, &path);
        let link = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink());
        if link {
            let target = std::fs::read_link(&path).map(|target| target.to_string_lossy().into_owned()).unwrap_or_default();
            workspace.files.insert(rel.clone(), FileEntry { mode: "120000".into(), hash: content_hash(target.as_bytes()), size: target.len() as u64 });
        }
        if path.is_dir() {
            let build_folder = (dir == root && name == "target") || path.join("CACHEDIR.TAG").exists();
            if !link && !build_folder {
                collect_files(root, &path, workspace)?;
            }
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if !link {
            workspace.files.insert(rel.clone(), FileEntry { mode: file_mode(&path), hash: content_hash(&bytes), size: bytes.len() as u64 });
        }
        if let Ok(text) = String::from_utf8(bytes) {
            if workspace.is_rust_file(&rel) {
                workspace.rust_files.insert(rel, text);
            } else {
                workspace.other_files.insert(rel, text);
            }
        }
    }
    Ok(())
}

/// The git mode of a file in a folder.
fn file_mode(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0) {
            return "100755".into();
        }
    }
    let _ = path;
    "100644".into()
}

/// The content hash of a file in a folder: the length and a 64-bit hash. This is enough for tests
/// and for a folder without git. The app uses the git object hashes.
fn content_hash(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{}:{:016x}", bytes.len(), hasher.finish())
}

/// Puts the items in a map by path. Two items with the same path (twins that the loader did not
/// see, for example a binary and a test target with one name) get the suffix `#N` in their source order, so
/// that no item hides another.
fn unique_paths(items: Vec<Item>) -> BTreeMap<String, Item> {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for item in &items {
        *count.entry(item.path.clone()).or_default() += 1;
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut map = BTreeMap::new();
    for mut item in items {
        if count[&item.path] > 1 {
            let index = seen.entry(item.path.clone()).or_default();
            item.path = format!("{}#{index}", item.path);
            *index += 1;
        }
        map.insert(item.path.clone(), item);
    }
    map
}

/// The name that an item gets in its module (the last path segment), and the names of the
/// methods of an impl (with their owner). Twins have the same name.
fn sibling_names(item: &syn::Item) -> Vec<String> {
    match item {
        syn::Item::Mod(value) => vec![value.ident.to_string()],
        syn::Item::Fn(value) => vec![value.sig.ident.to_string()],
        syn::Item::Impl(value) => {
            let owner = impl_owner(value);
            let mut names = vec![format!("impl {owner}")];
            names.extend(value.items.iter().filter_map(|it| match it {
                syn::ImplItem::Fn(method) => Some(format!("{owner}::{}", method.sig.ident)),
                _ => None,
            }));
            names
        }
        syn::Item::Struct(value) => vec![value.ident.to_string()],
        syn::Item::Enum(value) => vec![value.ident.to_string()],
        syn::Item::Trait(value) => vec![value.ident.to_string()],
        syn::Item::Const(value) => vec![value.ident.to_string()],
        syn::Item::Static(value) => vec![value.ident.to_string()],
        syn::Item::Type(value) => vec![value.ident.to_string()],
        // A macro definition (`macro_rules! name`). A macro call is not a twin of a definition
        // (batch 6b, part 2): the loader gives each call its own number.
        syn::Item::Macro(value) => value.ident.as_ref().map(|ident| format!("{ident}!")).into_iter().collect(),
        other => other_name(other).into_iter().collect(),
    }
}

/// The name of an item of the kind [`ItemKind::Other`], from its text: `extern crate rand`,
/// `extern C` (a block of foreign items), or the name of a union or of a trait alias. `None` for
/// an item with no name of its own: the loader gives it `item#N`, where N is its index among
/// such items of the module.
fn other_name(item: &syn::Item) -> Option<String> {
    match item {
        syn::Item::ExternCrate(value) => Some(format!("extern crate {}", value.ident)),
        syn::Item::ForeignMod(value) => Some(format!("extern {}", value.abi.name.as_ref().map(|name| name.value()).unwrap_or_else(|| "C".to_string()))),
        syn::Item::Union(value) => Some(value.ident.to_string()),
        syn::Item::TraitAlias(value) => Some(value.ident.to_string()),
        _ => None,
    }
}

/// The owner text of an impl: `Cart`, or `<Cart as Display>` for a trait impl.
///
/// The generic arguments are not in the owner (`impl<T> Wrapper<T>` gives `Wrapper`), so an impl
/// keeps its identity when its generics or bounds change. The header change is a signature edit
/// of the impl. Two impls of one type (`impl W<u8>` and `impl W<u16>`) are twins.
fn impl_owner(item_impl: &syn::ItemImpl) -> String {
    let self_ty = match item_impl.self_ty.as_ref() {
        syn::Type::Path(type_path) if type_path.qself.is_none() => without_arguments(&type_path.path),
        other => tokens(other).replace(' ', ""),
    };
    match &item_impl.trait_ {
        Some((_, trait_path, _)) => format!("<{self_ty} as {}>", without_arguments(trait_path)),
        None => self_ty,
    }
}

/// A path without its generic arguments: `std::fmt::Display`, `Wrapper`.
fn without_arguments(path: &syn::Path) -> String {
    let mut path = path.clone();
    for segment in &mut path.segments {
        segment.arguments = syn::PathArguments::None;
    }
    tokens(&path).replace(' ', "")
}

/// Gives each twin name of one item list its suffix (`#0`, `#1`, …) in source order.
struct Twins {
    count: BTreeMap<String, usize>,
    seen: BTreeMap<String, usize>,
}

impl Twins {
    fn new(items: &[syn::Item]) -> Twins {
        Twins::from_names(items.iter().flat_map(sibling_names))
    }

    fn from_names(names: impl Iterator<Item = String>) -> Twins {
        let mut count = BTreeMap::new();
        for name in names {
            *count.entry(name).or_default() += 1;
        }
        Twins { count, seen: BTreeMap::new() }
    }

    /// The name with its twin suffix, if it has twins. Call it once for each item, in order.
    fn name(&mut self, name: &str) -> String {
        if self.count.get(name).copied().unwrap_or(0) < 2 {
            return name.to_string();
        }
        let index = self.seen.entry(name.to_string()).or_default();
        let named = format!("{name}#{index}");
        *index += 1;
        named
    }
}

struct Loader<'a> {
    root: &'a Path,
    /// The items in load order. [`unique_paths`] puts them in the map.
    items: &'a mut Vec<Item>,
    parsed: &'a mut BTreeSet<String>,
    inner_attrs: &'a mut BTreeMap<String, (String, Vec<String>)>,
    crate_name: &'a str,
}

impl Loader<'_> {
    fn load_file(&mut self, file: &Path, module: &str, test: bool) -> Result<()> {
        let source = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let parsed = syn::parse_file(&source).with_context(|| format!("parse {}", file.display()))?;
        let rel = relative(self.root, file);
        self.parsed.insert(rel.clone());
        // The child modules of a root file are in the folder of the root file, whatever its name.
        let child_dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        self.record_inner_attrs(&module, &rel, &parsed.attrs);
        self.load_items(&parsed.items, &module, &rel, &source, &child_dir, test)
    }

    fn load_items(&mut self, items: &[syn::Item], module: &str, file: &str, source: &str, dir: &Path, test: bool) -> Result<()> {
        let mut use_index = 0;
        let mut other_index = 0;
        let mut twins = Twins::new(items);
        let mut call_index: BTreeMap<String, usize> = BTreeMap::new();
        for item in items {
            match item {
                syn::Item::Mod(item_mod) => {
                    let name = item_mod.ident.to_string();
                    let path = format!("{module}::{}", twins.name(&name));
                    let is_test = test || is_cfg_test(&item_mod.attrs);
                    if let Some((_, content)) = &item_mod.content {
                        self.load_items(content, &path, file, source, &dir.join(&name), is_test)?;
                    } else {
                        // `#[path = "…"]` names the file (invariant I4: the syntax tree must read the
                        // file that the compiler reads). Outside an inline module, the path is
                        // relative to the folder of the current file. Inside an inline module, it
                        // is relative to the folder of that module.
                        let candidates = match path_attr(&item_mod.attrs) {
                            Some(target) => {
                                let base = if *dir == module_dir(&self.root.join(file)) { self.root.join(file).parent().map(Path::to_path_buf).unwrap_or_default() } else { dir.to_path_buf() };
                                vec![base.join(target)]
                            }
                            None => vec![dir.join(format!("{name}.rs")), dir.join(&name).join("mod.rs")],
                        };
                        if let Some(found) = candidates.iter().find(|candidate| candidate.exists()) {
                            let text = std::fs::read_to_string(found)?;
                            let parsed = syn::parse_file(&text).with_context(|| format!("parse {}", found.display()))?;
                            let rel = relative(self.root, found);
                            self.parsed.insert(rel.clone());
                            self.record_inner_attrs(&path, &rel, &parsed.attrs);
                            // A file that `#[path]` names is a "mod-rs" file: its child modules are in its own folder.
                            let child_dir = if path_attr(&item_mod.attrs).is_some() { found.parent().map(Path::to_path_buf).unwrap_or_default() } else { module_dir(found) };
                            self.load_items(&parsed.items, &path, &rel, &text, &child_dir, is_test)?;
                        }
                    }
                    self.add(path, ItemKind::Mod, item, &item_mod.vis, &item_mod.attrs, None, Vec::new(), file, source, is_test);
                }
                syn::Item::Fn(item_fn) => {
                    let path = format!("{module}::{}", twins.name(&item_fn.sig.ident.to_string()));
                    let body = tree::block("body", &item_fn.block);
                    let is_test = test || has_attr(&item_fn.attrs, "test");
                    self.add(path, ItemKind::Fn, item, &item_fn.vis, &item_fn.attrs, Some(body), Vec::new(), file, source, is_test);
                    self.set_signature(&item_fn.sig);
                }
                syn::Item::Impl(item_impl) => {
                    let owner = impl_owner(item_impl);
                    for impl_item in &item_impl.items {
                        if let syn::ImplItem::Fn(method) = impl_item {
                            let path = format!("{module}::{}", twins.name(&format!("{owner}::{}", method.sig.ident)));
                            let body = tree::block("body", &method.block);
                            let is_test = test || has_attr(&method.attrs, "test");
                            self.add_parts(path, ItemKind::Fn, impl_item, &tokens(&method.vis), &method.attrs, Some(body), Vec::new(), file, source, is_test, signature_of_method(method));
                            self.set_signature(&method.sig);
                        }
                    }
                    let path = format!("{module}::{}", twins.name(&format!("impl {owner}")));
                    let members = item_impl.items.iter().map(|it| impl_member_name(it)).collect();
                    self.add(path, ItemKind::Impl, item, &syn::Visibility::Inherited, &item_impl.attrs, None, members, file, source, test);
                }
                syn::Item::Struct(item_struct) => {
                    let path = format!("{module}::{}", twins.name(&item_struct.ident.to_string()));
                    let members = self.add_fields(&path, &item_struct.fields, file, source, test);
                    self.add(path, ItemKind::Struct, item, &item_struct.vis, &item_struct.attrs, None, members, file, source, test);
                }
                syn::Item::Enum(item_enum) => {
                    let path = format!("{module}::{}", twins.name(&item_enum.ident.to_string()));
                    // The member list of an enum is the order of its variant names. Each variant is
                    // an item, and each variant field is an item.
                    let mut variant_twins = Twins::from_names(item_enum.variants.iter().map(|v| v.ident.to_string()));
                    for variant in &item_enum.variants {
                        let variant_path = format!("{path}::{}", variant_twins.name(&variant.ident.to_string()));
                        let fields = self.add_fields(&variant_path, &variant.fields, file, source, test);
                        let mut header = variant.clone();
                        header.attrs.clear();
                        header.fields = empty_fields(&variant.fields);
                        self.add_parts(variant_path, ItemKind::Variant, variant, "", &variant.attrs, None, fields, file, source, test, tokens(&header));
                    }
                    let members = item_enum.variants.iter().map(|variant| variant.ident.to_string()).collect();
                    self.add(path, ItemKind::Enum, item, &item_enum.vis, &item_enum.attrs, None, members, file, source, test);
                }
                syn::Item::Trait(item_trait) => {
                    let path = format!("{module}::{}", twins.name(&item_trait.ident.to_string()));
                    // A trait method is a function item: `shop::Price::label`. A default body gets
                    // statement edits, and its MIR name is the same path. The member list keeps the
                    // order of the methods and the other trait items as text.
                    let mut method_twins = Twins::from_names(item_trait.items.iter().filter_map(|it| match it {
                        syn::TraitItem::Fn(method) => Some(method.sig.ident.to_string()),
                        _ => None,
                    }));
                    let mut members = Vec::new();
                    for trait_item in &item_trait.items {
                        match trait_item {
                            syn::TraitItem::Fn(method) => {
                                let method_path = format!("{path}::{}", method_twins.name(&method.sig.ident.to_string()));
                                let body = method.default.as_ref().map(|block| tree::block("body", block));
                                let is_test = test || has_attr(&method.attrs, "test");
                                self.add_parts(method_path, ItemKind::Fn, trait_item, "", &method.attrs, body, Vec::new(), file, source, is_test, tokens(&method.sig));
                                self.set_signature(&method.sig);
                                members.push(format!("fn {}", method.sig.ident));
                            }
                            other => members.push(tokens(other)),
                        }
                    }
                    self.add(path, ItemKind::Trait, item, &item_trait.vis, &item_trait.attrs, None, members, file, source, test);
                }
                syn::Item::Const(item_const) => {
                    let path = format!("{module}::{}", twins.name(&item_const.ident.to_string()));
                    self.add(path, ItemKind::Const, item, &item_const.vis, &item_const.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Static(item_static) => {
                    let path = format!("{module}::{}", twins.name(&item_static.ident.to_string()));
                    self.add(path, ItemKind::Static, item, &item_static.vis, &item_static.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Type(item_type) => {
                    let path = format!("{module}::{}", twins.name(&item_type.ident.to_string()));
                    self.add(path, ItemKind::Type, item, &item_type.vis, &item_type.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Use(item_use) => {
                    let path = format!("{module}::use#{use_index}");
                    use_index += 1;
                    self.add(path, ItemKind::Use, item, &item_use.vis, &item_use.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Macro(item_macro) => {
                    // A definition keeps the plain name `name!`, with a twin suffix only if the
                    // module has two definitions of the name. A call always has a number:
                    // `name!#N`, in source order. So a new call at the end changes no other
                    // name (batch 6b, part 2). With two or more definitions, the numbers of the
                    // calls start after the numbers of the definitions.
                    let path = match &item_macro.ident {
                        Some(ident) => format!("{module}::{}", twins.name(&format!("{ident}!"))),
                        None => {
                            let name = format!("{}!", tokens(&item_macro.mac.path));
                            let definitions = twins.count.get(&name).copied().filter(|count| *count >= 2).unwrap_or(0);
                            let index = call_index.entry(name.clone()).or_insert(definitions);
                            *index += 1;
                            format!("{module}::{name}#{}", *index - 1)
                        }
                    };
                    self.add(path, ItemKind::Macro, item, &syn::Visibility::Inherited, &item_macro.attrs, None, Vec::new(), file, source, test);
                }
                other => {
                    // The name does not have a line number (batch 5n, A3): a new line above the
                    // item must not change its name.
                    let name = match other_name(other) {
                        Some(name) => twins.name(&name),
                        None => {
                            other_index += 1;
                            format!("item#{}", other_index - 1)
                        }
                    };
                    self.add(format!("{module}::{name}"), ItemKind::Other, item, &syn::Visibility::Inherited, &[], None, Vec::new(), file, source, test);
                }
            }
        }
        Ok(())
    }

    /// Sets the signature node of the last item (a function).
    fn set_signature(&mut self, sig: &syn::Signature) {
        if let Some(item) = self.items.last_mut() {
            item.sig = Some(tree::signature(sig));
        }
    }

    /// Records the inner attributes of a file (`#![…]`). An inner doc comment is stored as a
    /// `//!` line, as in the source. Each other attribute is stored in its token form.
    fn record_inner_attrs(&mut self, module: &str, file: &str, attrs: &[syn::Attribute]) {
        if !attrs.is_empty() {
            let text = |attr: &syn::Attribute| match &attr.meta {
                syn::Meta::NameValue(syn::MetaNameValue { value: syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(text), .. }), .. }) if attr.path().is_ident("doc") => doc_lines("//!", &text.value()),
                _ => tokens(attr),
            };
            self.inner_attrs.insert(module.to_string(), (file.to_string(), attrs.iter().map(text).collect()));
        }
    }

    /// Adds the fields of a struct or a variant as items (`owner.name`, or `owner.0` for a tuple
    /// field), and returns the member list of the owner: the field names in order.
    fn add_fields(&mut self, owner: &str, fields: &syn::Fields, file: &str, source: &str, test: bool) -> Vec<String> {
        let mut names = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            let name = field.ident.as_ref().map(|ident| ident.to_string()).unwrap_or_else(|| index.to_string());
            self.add_parts(format!("{owner}.{name}"), ItemKind::Field, field, &tokens(&field.vis), &field.attrs, None, Vec::new(), file, source, test, tokens(&field.ty));
            names.push(name);
        }
        names
    }

    #[allow(clippy::too_many_arguments)]
    fn add(&mut self, path: String, kind: ItemKind, item: &syn::Item, vis: &syn::Visibility, attrs: &[syn::Attribute], body: Option<Node>, members: Vec<String>, file: &str, source: &str, test: bool) {
        let signature = signature_of(item);
        self.add_parts(path, kind, item, &tokens(vis), attrs, body, members, file, source, test, signature);
    }

    #[allow(clippy::too_many_arguments)]
    fn add_parts(&mut self, path: String, kind: ItemKind, value: &(impl quote::ToTokens + Spanned), vis: &str, attrs: &[syn::Attribute], body: Option<Node>, members: Vec<String>, file: &str, source: &str, test: bool, signature: String) {
        let span = value.span();
        let (line_start, line_end) = (span.start().line, span.end().line);
        let (col_start, col_end) = (span.start().column, span.end().column);
        let text_source = source_lines(source, line_start, line_end);
        // Doc comments are stored as `///` lines, as in the source.
        let doc_line = |attr: &syn::Attribute| match &attr.meta {
            syn::Meta::NameValue(value) => match &value.value {
                // A block comment (`/** … */`) has more than one line: each line is a doc line.
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(text), .. }) => doc_lines("///", &text.value()),
                _ => tokens(attr),
            },
            _ => tokens(attr),
        };
        // Only doc text is a doc line: `/// text` and `#[doc = …]`. A `#[doc(KEY)]` attribute
        // (`doc(hidden)`, `doc(alias = "…")`) is an attribute (batch 6b, part 1). Before, it was
        // a line of the docs, and a change of it showed as a changed comment.
        let doc_text = |attr: &syn::Attribute| attr.path().is_ident("doc") && matches!(attr.meta, syn::Meta::NameValue(_));
        let mut docs: Vec<String> = attrs.iter().filter(|attr| doc_text(attr)).map(doc_line).collect();
        docs.extend(plain_comments(&text_source));
        let attrs = attrs.iter().filter(|attr| !doc_text(attr)).map(tokens).collect();
        let item = Item {
            path: path.clone(),
            kind,
            crate_name: self.crate_name.to_string(),
            file: file.to_string(),
            line_start,
            line_end,
            col_start,
            col_end,
            vis: vis.to_string(),
            signature,
            docs,
            attrs,
            members,
            body,
            sig: None,
            text: tokens(value),
            source: text_source,
            test,
        };
        self.items.push(item);
    }
}

/// The folder for the child modules of a file (`lib.rs` and `mod.rs`: the same folder).
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    if matches!(stem.as_ref(), "lib" | "main" | "mod") || file.parent().is_some_and(|p| p.ends_with("tests")) {
        parent
    } else {
        parent.join(stem.as_ref())
    }
}

/// The value of a `#[path = "…"]` attribute.
fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().filter(|attr| attr.path().is_ident("path")).find_map(|attr| match &attr.meta {
        syn::Meta::NameValue(syn::MetaNameValue { value: syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(text), .. }), .. }) => Some(text.value()),
        _ => None,
    })
}

fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| attr.path().segments.last().is_some_and(|segment| segment.ident == name))
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("cfg") && tokens(attr).contains("test"))
}

/// Fields of the same kind (named, tuple, or unit), without members.
fn empty_fields(fields: &syn::Fields) -> syn::Fields {
    match fields {
        syn::Fields::Named(named) => syn::Fields::Named(syn::FieldsNamed { brace_token: named.brace_token, named: Default::default() }),
        syn::Fields::Unnamed(unnamed) => syn::Fields::Unnamed(syn::FieldsUnnamed { paren_token: unnamed.paren_token, unnamed: Default::default() }),
        syn::Fields::Unit => syn::Fields::Unit,
    }
}

fn impl_member_name(item: &syn::ImplItem) -> String {
    match item {
        syn::ImplItem::Fn(method) => format!("fn {}", method.sig.ident),
        // The full text of an associated constant or type, with no attributes and no visibility
        // (batch 5n, A2): a change of its type or of its value is an edit of the member list. The
        // text starts with the kind and the name (`const NAME`, `type NAME`).
        syn::ImplItem::Const(constant) => {
            let mut member = constant.clone();
            member.attrs.clear();
            member.vis = syn::Visibility::Inherited;
            member.defaultness = None;
            tokens(&member)
        }
        syn::ImplItem::Type(ty) => {
            let mut member = ty.clone();
            member.attrs.clear();
            member.vis = syn::Visibility::Inherited;
            member.defaultness = None;
            tokens(&member)
        }
        other => tokens(other),
    }
}

/// The signature text of an item: everything except the body, the docs, and the visibility.
fn signature_of(item: &syn::Item) -> String {
    match item {
        syn::Item::Fn(item_fn) => tokens(&item_fn.sig),
        syn::Item::Impl(item_impl) => {
            let mut header = item_impl.clone();
            header.items.clear();
            header.attrs.clear();
            tokens(&header)
        }
        syn::Item::Mod(item_mod) => format!("mod {}", item_mod.ident),
        // The members (fields, variants, trait items) are items of their own, and the attributes
        // are compared on their own. The signature is the header: the generics, the bounds, and
        // the kind of the fields (named, tuple, or unit).
        syn::Item::Struct(value) => {
            let mut header = value.clone();
            header.attrs.clear();
            header.vis = syn::Visibility::Inherited;
            header.fields = empty_fields(&value.fields);
            tokens(&header)
        }
        syn::Item::Enum(value) => {
            let mut header = value.clone();
            header.attrs.clear();
            header.vis = syn::Visibility::Inherited;
            header.variants.clear();
            tokens(&header)
        }
        syn::Item::Trait(value) => {
            let mut header = value.clone();
            header.attrs.clear();
            header.vis = syn::Visibility::Inherited;
            header.items.clear();
            tokens(&header)
        }
        other => {
            let mut copy = other.clone();
            strip_docs_and_vis(&mut copy);
            tokens(&copy)
        }
    }
}

fn signature_of_method(method: &syn::ImplItemFn) -> String {
    tokens(&method.sig)
}

fn strip_docs_and_vis(item: &mut syn::Item) {
    macro_rules! strip {
        ($value:expr) => {{
            $value.attrs.retain(|attr| !attr.path().is_ident("doc"));
            $value.vis = syn::Visibility::Inherited;
        }};
    }
    match item {
        syn::Item::Struct(value) => strip!(value),
        syn::Item::Enum(value) => strip!(value),
        syn::Item::Trait(value) => strip!(value),
        syn::Item::Const(value) => strip!(value),
        syn::Item::Static(value) => strip!(value),
        syn::Item::Type(value) => strip!(value),
        syn::Item::Use(value) => strip!(value),
        _ => {}
    }
}

fn source_lines(source: &str, start: usize, end: usize) -> String {
    source.lines().skip(start.saturating_sub(1)).take(end + 1 - start.max(1)).collect::<Vec<_>>().join("\n")
}

/// Finds the plain comments (`//` and `/* */`, not doc comments) in a source text.
///
/// This is a small scanner: it skips string and char literals, so `"//"` is not a comment.
pub fn plain_comments(source: &str) -> Vec<String> {
    let mut comments = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '\'' if i + 2 < chars.len() && (chars[i + 2] == '\'' || chars[i + 1] == '\\') => {
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                let start = i;
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                if !text.starts_with("///") && !text.starts_with("//!") {
                    comments.push(text.trim().to_string());
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let start = i;
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
                let text: String = chars[start..i.min(chars.len())].iter().collect();
                if !text.starts_with("/**") && !text.starts_with("/*!") {
                    comments.push(text.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
            _ => i += 1,
        }
    }
    comments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    #[test]
    fn a_macro_definition_keeps_its_name_and_each_call_has_an_index() {
        // Batch 6b, part 2: the first call of `same!` had the key of the definition with the
        // suffix `#1`. A new call then looked like a second definition.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n");
        write(root, "src/lib.rs", "macro_rules! same { () => {}; }\nsame!();\nsame!();\nthing!();\nmod m {\n    #[cfg(unix)]\n    macro_rules! t { () => {}; }\n    #[cfg(not(unix))]\n    macro_rules! t { () => {}; }\n    t!();\n}\n");
        let ws = Workspace::load(root).unwrap();
        let macros: Vec<&str> = ws.items.values().filter(|item| item.kind == ItemKind::Macro).map(|item| item.path.as_str()).collect();
        // Two definitions with one name are twins. The index of a call starts after them.
        assert_eq!(macros, ["demo::m::t!#0", "demo::m::t!#1", "demo::m::t!#2", "demo::same!", "demo::same!#0", "demo::same!#1", "demo::thing!#0"]);
        assert!(ws.items["demo::same!"].source.starts_with("macro_rules!") && ws.items["demo::same!#1"].line_start == 3);
        assert!(!ws.items["demo::m::t!#2"].source.contains("macro_rules"), "the third item is the call");
    }

    #[test]
    fn follows_the_path_attribute_of_a_module() {
        // T2 (trial 2, A8a): the compiler reads `fees_v2.rs`, so Semilla must read it too.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n");
        write(root, "src/lib.rs", "#[path = \"fees_v2.rs\"]\npub mod fees;\npub mod a;\n");
        write(root, "src/fees.rs", "pub fn old() {}\n");
        write(root, "src/fees_v2.rs", "pub fn new() {}\npub mod inner;\n");
        write(root, "src/inner.rs", "pub fn deep() {}\n");
        write(root, "src/a.rs", "#[path = \"other/b.rs\"]\npub mod b;\nmod c {\n    #[path = \"d.rs\"]\n    pub mod d;\n}\n");
        write(root, "src/other/b.rs", "pub fn bee() {}\n");
        write(root, "src/a/c/d.rs", "pub fn dee() {}\n");
        let ws = Workspace::load(root).unwrap();
        assert!(ws.items.contains_key("demo::fees::new"), "{:?}", ws.items.keys().collect::<Vec<_>>());
        assert!(!ws.items.contains_key("demo::fees::old"));
        assert_eq!(ws.items["demo::fees::new"].file, "src/fees_v2.rs");
        assert!(ws.items.contains_key("demo::fees::inner::deep"), "{:?}", ws.items.keys().collect::<Vec<_>>());
        assert_eq!(ws.items["demo::a::b::bee"].file, "src/other/b.rs");
        assert_eq!(ws.items["demo::a::c::d::dee"].file, "src/a/c/d.rs");
        assert!(!ws.parsed_files.contains("src/fees.rs"));
    }

    #[test]
    fn loads_a_two_crate_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n");
        write(root, "crates/core-a/Cargo.toml", "[package]\nname = \"core-a\"\nversion = \"0.1.0\"\n");
        write(root, "crates/core-a/src/lib.rs", "pub mod cart;\npub fn top() -> u32 { 1 }\n");
        write(root, "crates/core-a/src/cart.rs", "pub struct Cart { pub items: Vec<u32> }\nimpl Cart {\n    /// Sum.\n    pub fn total(&self) -> u32 {\n        // add\n        self.items.iter().sum()\n    }\n}\n");
        write(root, "crates/app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\ncore-a = { path = \"../core-a\" }\n");
        write(root, "crates/app/src/main.rs", "fn main() { println!(\"{}\", core_a::top()); }\n#[cfg(test)]\nmod tests { #[test] fn t() {} }\n");

        let ws = Workspace::load(root).unwrap();
        let names: Vec<_> = ws.crates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["app", "core_a"]);
        assert_eq!(ws.crates[0].deps, ["core_a"]);
        let total = &ws.items["core_a::cart::Cart::total"];
        assert_eq!(total.kind, ItemKind::Fn);
        assert_eq!(total.file, "crates/core-a/src/cart.rs");
        assert!(total.docs.iter().any(|doc| doc.contains("Sum")));
        assert!(total.docs.iter().any(|doc| doc == "// add"));
        assert!(ws.items["app::tests::t"].test);
        assert!(!ws.items["app::main"].test);
        assert_eq!(ws.items["core_a::cart::Cart"].members, ["items"]);
        let field = &ws.items["core_a::cart::Cart.items"];
        assert_eq!((field.kind, field.vis.as_str(), field.signature.as_str()), (ItemKind::Field, "pub", "Vec < u32 >"));
        assert!(ws.other_files.contains_key("crates/app/Cargo.toml"));
    }

    #[test]
    fn the_name_of_an_item_has_no_line_number() {
        // Batch 5n, A3: a new line above an item does not change its name.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n");
        let source = "extern crate core as base;\nextern \"C\" { fn one(); }\nextern \"C\" { fn two(); }\npub union Bits { a: u8, b: i8 }\n";
        let names = |source: &str| {
            write(root, "src/lib.rs", source);
            let ws = Workspace::load(root).unwrap();
            let mut found: Vec<(String, usize)> = ws.items.values().filter(|item| item.kind == ItemKind::Other).map(|item| (item.path.clone(), item.line_start)).collect();
            found.sort();
            found
        };
        let before = names(source);
        let paths: Vec<&str> = before.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, ["tool::Bits", "tool::extern C#0", "tool::extern C#1", "tool::extern crate core"]);
        let after = names(&format!("// A new line.\n\n{source}"));
        assert_eq!(after.iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(), paths);
        assert!(before.iter().zip(&after).all(|(old, new)| old.1 + 2 == new.1), "{before:?} {after:?}");
        assert!(Workspace::load(root).unwrap().items.keys().all(|path| !path.contains('@')));
    }

    #[test]
    fn a_target_file_is_rust_code_whatever_its_name() {
        // Brief 5h, A4 (cheat C1): the manifest names the files, the file name decides nothing.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nbuild = \"NOTES.md\"\n[lib]\npath = \"code/main.txt\"\n[[bin]]\nname = \"license\"\npath = \"LICENSE.txt\"\n");
        write(root, "code/main.txt", "mod part;\npub fn top() -> u32 { part::one() }\n");
        write(root, "code/part.rs", "pub fn one() -> u32 { 1 }\n");
        write(root, "LICENSE.txt", "fn main() { helper(); }\nfn helper() {}\n");
        write(root, "NOTES.md", "mod gen_code;\nfn main() { include!(\"table.inc\"); }\n");
        write(root, "gen_code.rs", "pub fn make() {}\n");
        write(root, "table.inc", "let _ = 1;\n");
        write(root, "examples/demo.rs", "fn main() {}\n");
        write(root, "benches/tool.rs", "const DATA: u8 = 1;\nfn main() { let _ = DATA; }\n");
        write(root, "README.md", "# Tool\n");
        write(root, "vendor/dep/Cargo.toml", "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n[lib]\npath = \"lib.txt\"\n");
        write(root, "vendor/dep/lib.txt", "pub fn dep() {}\n");

        let ws = Workspace::load(root).unwrap();
        // The library and the binary are parsed, with their module files.
        assert_eq!(ws.items["tool::top"].file, "code/main.txt");
        assert_eq!(ws.items["tool::part::one"].file, "code/part.rs");
        assert_eq!(ws.items["tool::license::helper"].file, "LICENSE.txt");
        assert!(ws.parsed_files.contains("LICENSE.txt") && ws.parsed_files.contains("code/main.txt"));
        // The build script, its module, and its include are code that the loader does not parse.
        for file in ["NOTES.md", "gen_code.rs", "table.inc"] {
            assert!(ws.code_files[file].is_build_script(), "{file}");
        }
        // Batch 5n, A4: an example and a bench are parsed as a binary and a test. A target with
        // the name of the package is a module of the package.
        assert_eq!(ws.items["tool::demo::main"].file, "examples/demo.rs");
        assert_eq!(ws.items["tool::tool::DATA"].file, "benches/tool.rs");
        assert!(ws.items["tool::tool::main"].test && !ws.items["tool::demo::main"].test);
        assert!(ws.parsed_files.contains("examples/demo.rs") && !ws.code_files.contains_key("examples/demo.rs"));
        assert!(ws.code_files["vendor/dep/lib.txt"].outside);
        // Each of these files is a Rust file. The plain doc file is not.
        for file in ["LICENSE.txt", "code/main.txt", "NOTES.md", "table.inc", "vendor/dep/lib.txt"] {
            assert!(ws.rust_files.contains_key(file) && !ws.other_files.contains_key(file), "{file}");
        }
        assert!(ws.other_files.contains_key("README.md") && !ws.is_rust_file("README.md"));
        let kinds: Vec<(&str, &str)> = ws.targets.iter().map(|target| (target.kind.as_str(), target.file.as_str())).collect();
        assert_eq!(kinds, [("lib", "code/main.txt"), ("bin", "LICENSE.txt"), ("example", "examples/demo.rs"), ("bench", "benches/tool.rs"), ("custom-build", "NOTES.md")]);
    }

    #[test]
    fn the_targets_of_cargo_metadata_decide_the_root_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n");
        write(root, "crates/a/Cargo.toml", "[package]\nname = \"a\"\nversion = \"0.1.0\"\n");
        write(root, "crates/a/src/lib.rs", "pub fn lib_fn() {}\n");
        write(root, "crates/a/LICENSE.txt", "fn main() {}\n");
        write(root, "crates/a/NOTES.md", "fn main() {}\n");
        write(root, "crates/a/tests/it.rs", "#[test]\nfn works() {}\n");
        let path = |rel: &str| root.join(rel).to_string_lossy().into_owned();
        let target = |kind: &str, name: &str, rel: &str| serde_json::json!({ "kind": [kind], "name": name, "src_path": path(rel) });
        let metadata = serde_json::json!({ "packages": [{
            "name": "a",
            "manifest_path": path("crates/a/Cargo.toml"),
            "dependencies": [],
            "targets": [
                target("lib", "a", "crates/a/src/lib.rs"),
                target("bin", "the-tool", "crates/a/LICENSE.txt"),
                target("test", "it", "crates/a/tests/it.rs"),
                target("custom-build", "build-script-build", "crates/a/NOTES.md"),
            ],
        }] });
        let ws = Workspace::load_with_metadata(root, &metadata).unwrap();
        assert_eq!(ws.crates.len(), 1);
        assert_eq!(ws.items["a::the_tool::main"].file, "crates/a/LICENSE.txt");
        assert!(ws.items["a::it::works"].test);
        assert!(ws.items.contains_key("a::lib_fn"));
        assert!(ws.code_files["crates/a/NOTES.md"].is_build_script());
        assert_eq!(ws.code_files["crates/a/NOTES.md"].role(), "the build script of `a`");
    }

    #[test]
    fn comment_scanner_skips_strings() {
        let comments = plain_comments("let a = \"// not\"; // yes\n/* block */ /// doc");
        assert_eq!(comments, ["// yes", "/* block */"]);
    }
}
