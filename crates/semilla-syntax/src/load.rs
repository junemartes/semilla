//! Loads a Cargo workspace into the entity tree: crates, modules, and items.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::rs::{self, AstNode, Edition, Lines, SyntaxKind, SyntaxNode, SyntaxToken, T, TextRange, ast, compare_opt};
use crate::tree::{self, Node};
use crate::tt::{Delimiter, Flat, TokenStream, TokenTree, compare_text, display_text, flat_display, flat_text, flat_tokens};
use ast::{HasGenericArgs, HasModuleItem, HasName};

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
    /// The display texts of the parts (design 39.2): the source text as written, on one line.
    #[serde(skip)]
    pub display: ItemDisplay,
    /// True for the definition of a macro (`macro_rules! name { … }`). The kind of the syntax
    /// node gives the value (design 57.2): a search in the source text does not.
    #[serde(skip)]
    pub macro_definition: bool,
}

/// The display texts of the parts of an item, in the order of the compare texts.
#[derive(Clone, Debug, Default)]
pub struct ItemDisplay {
    pub vis: String,
    pub signature: String,
    pub attrs: Vec<String>,
    pub members: Vec<String>,
}

/// A compare text and its display text.
type Text = (String, String);

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
    /// The display texts of the inner attributes, in the same order (design 39.2).
    #[serde(skip)]
    pub inner_attrs_display: BTreeMap<String, Vec<String>>,
    /// The file of each crate root and of each module with a file, by module path (design
    /// 56.3). An inline module is not in the map: its place is its item.
    #[serde(default)]
    pub module_files: BTreeMap<String, String>,
    /// The modules of a `mod NAME;` line in the input of a macro call (design 59, A-a), by
    /// module path. Semilla does not expand the macro: it does not know if the build has the
    /// module.
    #[serde(default)]
    pub macro_modules: BTreeMap<String, MacroModule>,
    /// The files of the parsed targets that the loader cannot parse, with the reason (design
    /// 37.5). The file is not in `parsed_files`.
    pub parse_errors: BTreeMap<String, String>,
    /// The items with a body by file ([`Workspace::body_items`]), made at the first use.
    #[serde(skip)]
    pub body_index: BodyIndex,
    /// The edition of the package of each parsed file (design 39.1). The MIR map parses a
    /// file again with it (design 44, FIX 1).
    #[serde(skip)]
    pub editions: BTreeMap<String, Edition>,
}

/// The source of a module whose `mod NAME;` line is in the input of a macro call (design 59).
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct MacroModule {
    /// The path of the macro call item.
    pub call: String,
    /// The file of the module.
    pub file: String,
    /// The file and the line of the `mod` token of the declaration.
    pub at: (String, usize),
    /// The text of the declaration (`mod unix;`).
    pub text: String,
}

/// One function, constant, or static of a file, for [`Workspace::body_items`].
#[derive(Clone, Debug)]
pub struct BodyItem {
    pub line_start: usize,
    pub line_end: usize,
    pub kind: ItemKind,
    pub path: String,
}

/// The functions, the constants, and the statics of a workspace by file, with the answers of
/// the searches that used them.
#[derive(Debug, Default)]
pub struct BodyItems {
    /// The items of each file, in the order of their paths.
    pub by_file: BTreeMap<String, Vec<BodyItem>>,
    /// The answers that the search gave: the name of a body, then each span with its answer.
    pub found: std::sync::Mutex<FoundItems>,
}

/// The answers of the search for the item of a body, by the name and the span of the body.
pub type FoundItems = std::collections::HashMap<String, Vec<(Option<(String, usize)>, Option<String>)>>;

/// The index of [`Workspace::body_items`]. A copy of a workspace starts with no index, so a
/// changed copy makes its own index.
#[derive(Default)]
pub struct BodyIndex {
    /// The count of the items at the time of the index, and the index.
    cell: std::sync::OnceLock<(usize, BodyItems)>,
}

impl Clone for BodyIndex {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for BodyIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BodyIndex")
    }
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
/// on, the targets (kind, name, root file), and the edition (design 39.1).
struct Package {
    dir: PathBuf,
    name: String,
    deps: Vec<String>,
    targets: Vec<(String, String, PathBuf)>,
    edition: Edition,
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
    /// The functions, constants, and statics of each file, in the order of their paths (design
    /// 37.20). The search for the item of a compiled body reads only the items of one file.
    /// `None` if the items changed after the first use: then the caller reads each item.
    pub fn body_items(&self) -> Option<&BodyItems> {
        let (count, index) = self.body_index.cell.get_or_init(|| {
            let mut index = BodyItems::default();
            for item in self.items.values().filter(|item| matches!(item.kind, ItemKind::Fn | ItemKind::Const | ItemKind::Static)) {
                index.by_file.entry(item.file.clone()).or_default().push(BodyItem { line_start: item.line_start, line_end: item.line_end, kind: item.kind, path: item.path.clone() });
            }
            (self.items.len(), index)
        });
        (*count == self.items.len()).then_some(index)
    }

    /// Loads the workspace at `root`. The packages and the targets come from the manifests, with
    /// the rules of cargo for the default targets. The app uses [`Workspace::load_with_metadata`].
    pub fn load(root: &Path) -> Result<Workspace> {
        let mut packages = Vec::new();
        let found = find_packages(root)?;
        let names: Vec<String> = found.iter().map(|(_, name, _)| name.clone()).collect();
        let workspace_edition = read_manifest(&root.join("Cargo.toml")).ok().and_then(|manifest| manifest.get("workspace")?.get("package")?.get("edition")?.as_str().map(str::to_string));
        for (dir, name, manifest) in found {
            let deps = manifest.get("dependencies").and_then(|deps| deps.as_table()).map(|deps| deps.keys().map(|key| key.replace('-', "_")).filter(|key| names.contains(key)).collect()).unwrap_or_default();
            let targets = manifest_targets(&dir, &manifest);
            let edition = manifest_edition(&manifest, workspace_edition.as_deref());
            packages.push(Package { dir, name, deps, targets, edition });
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
            let edition = rs::edition(package.get("edition").and_then(|e| e.as_str()));
            packages.push(Package { dir, name: crate_name(package), deps, targets, edition });
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
                // The module path of the root (design 56.1). Each target is the root of its own
                // item tree. The root of the library is the package name. The root of each
                // other target names the target in the form of cargo: `depot (test "list")`.
                // The rule does not look at the other targets of the package: `src/main.rs`
                // of a package with no library has the form `depot (bin "depot")` too.
                let module = target_root(&package.name, kind, name);
                let mut loader = Loader { root, items: &mut items, parsed: &mut workspace.parsed_files, inner_attrs: &mut workspace.inner_attrs, inner_attrs_display: &mut workspace.inner_attrs_display, module_files: &mut workspace.module_files, macro_modules: &mut workspace.macro_modules, parse_errors: &mut workspace.parse_errors, editions: &mut workspace.editions, crate_name: &package.name, edition: package.edition };
                loader.load_file(file, &module, matches!(kind.as_str(), "test" | "bench"))?;
            }
        }
        // The other targets (the build scripts) are Rust code that the loader does not parse
        // into items.
        for package in &packages {
            for (kind, name, file) in &package.targets {
                if !parsed_kind(kind) {
                    let code = CodeFile { package: package.name.clone(), kind: kind.clone(), target: name.clone(), outside: false };
                    walk_code(root, file, &code, package.edition, &mut workspace);
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
            let edition = manifest_edition(&manifest, None);
            for (kind, name, file) in manifest_targets(&dir, &manifest) {
                let code = CodeFile { package: relative(root, &dir), kind, target: name, outside: true };
                walk_code(root, &file, &code, edition, &mut workspace);
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

/// The edition of a package manifest: `package.edition`, or the edition of the workspace for
/// `edition.workspace = true`.
fn manifest_edition(manifest: &toml::Table, workspace: Option<&str>) -> Edition {
    let value = manifest.get("package").and_then(|package| package.get("edition"));
    match value {
        Some(toml::Value::String(text)) => rs::edition(Some(text)),
        Some(toml::Value::Table(table)) if table.get("workspace").and_then(|v| v.as_bool()) == Some(true) => rs::edition(workspace),
        _ => rs::edition(None),
    }
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
fn walk_code(root: &Path, file: &Path, code: &CodeFile, edition: Edition, workspace: &mut Workspace) {
    fn modules(root: &Path, items: impl Iterator<Item = ast::Item>, file: &Path, dir: &Path, top: bool, code: &CodeFile, edition: Edition, workspace: &mut Workspace) {
        for item in items {
            let ast::Item::Module(item_mod) = item else { continue };
            let name = name_of(&item_mod);
            match item_mod.item_list() {
                Some(content) => modules(root, content.items(), file, &dir.join(&name), false, code, edition, workspace),
                None => {
                    let base = if top { file.parent().unwrap_or(Path::new(".")).to_path_buf() } else { dir.to_path_buf() };
                    for (found, by_attr) in mod_files(&attributes(item_mod.syntax()), &base, dir, &name) {
                        let child_dir = if by_attr { found.parent().map(Path::to_path_buf).unwrap_or_default() } else { module_dir(&found) };
                        visit(root, &found, &child_dir, code, edition, workspace);
                    }
                }
            }
        }
    }
    fn visit(root: &Path, file: &Path, child_dir: &Path, code: &CodeFile, edition: Edition, workspace: &mut Workspace) {
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
                visit(root, &included, &dir, code, edition, workspace);
            }
        }
        let parsed = std::iter::once(edition).chain(rs::EDITIONS).find_map(|edition| rs::parse_file(&text, edition).ok());
        if let Some(parsed) = parsed {
            modules(root, parsed.items(), file, child_dir, true, code, edition, workspace);
        }
    }
    // The child modules of a root file are in the folder of the root file.
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    visit(root, file, &dir, code, edition, workspace);
}

/// The files of the module tree of one target root, relative to `root`: the root file, each
/// module file (also with `#[path]`), and each file that `include!` reads. The list does not
/// depend on the other targets of the tree. A file that does not exist is not in the list.
pub fn module_files(root: &Path, file: &Path) -> BTreeSet<String> {
    let mut workspace = Workspace::default();
    let code = CodeFile { package: String::new(), kind: String::new(), target: String::new(), outside: false };
    walk_code(root, file, &code, Edition::Edition2021, &mut workspace);
    workspace.code_files.into_keys().collect()
}

/// The lines of a doc text in the source form: each line with the marker `///` or `//!`.
fn doc_lines(marker: &str, text: &str) -> String {
    if text.is_empty() {
        return marker.to_string();
    }
    text.lines().map(|line| format!("{marker}{line}")).collect::<Vec<_>>().join("\n")
}
/// The root of the item tree of a target (design 56.1). The library is the package name. Each
/// other target has the form of cargo: `depot (test "list")`, `depot (bin "depot")`.
pub fn target_root(package: &str, kind: &str, name: &str) -> String {
    if kind == "lib" { package.to_string() } else { format!("{package} ({kind} \"{name}\")") }
}

/// The root of the item tree of a path: the text before the first `::` that is not in the
/// name of a target.
pub fn path_root(path: &str) -> &str {
    let bytes = path.as_bytes();
    let (mut index, mut quoted) = (0, false);
    while index < bytes.len() {
        match bytes[index] {
            b'"' => quoted = !quoted,
            b':' if !quoted && bytes.get(index + 1) == Some(&b':') => return &path[..index],
            b'<' if !quoted => break,
            _ => {}
        }
        index += 1;
    }
    path
}

/// The package of a path: the root of its item tree with no target (design 56.1).
/// `depot (test "list")::first` and `depot::first` are in the package `depot`.
pub fn package_of(path: &str) -> &str {
    let root = path_root(path);
    match root.split_once(" (") {
        Some((package, target)) if target.ends_with("\")") => package,
        _ => root,
    }
}

/// The name that the compiler gives to the item of a path (design 56.1). The compiler names a
/// body by the crate name of its target: `depot (test "list")::first` has the body
/// `list::first`. A path of the library stays as it is.
pub fn compiled_path(path: &str) -> String {
    let root = path_root(path);
    let name = root.strip_suffix("\")").and_then(|rest| rest.split_once(" (")).and_then(|(_, target)| target.split_once(" \"")).map(|(_, name)| name.replace('-', "_"));
    match name {
        Some(name) => format!("{name}{}", &path[root.len()..]),
        None => path.to_string(),
    }
}

/// The length of the target of a path at the start of the text: ` (KIND "NAME")`, with a
/// target kind of cargo (design 56.1). `None` if the text does not start with a target.
pub fn target_end(text: &str) -> Option<usize> {
    let rest = text.strip_prefix(" (")?;
    let kind = ["bin \"", "test \"", "example \"", "bench \""].into_iter().find(|kind| rest.starts_with(kind))?;
    let name = &rest[kind.len()..];
    let close = name.find('"')?;
    name[close..].starts_with("\")").then_some(2 + kind.len() + close + 2)
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

/// The name of a node with a name (`r#type` keeps its `r#`).
fn name_of(node: &impl ast::HasName) -> String {
    node.name().map(|name| name.text().to_string()).unwrap_or_default()
}

/// The name that an item gets in its module (the last path segment), and the names of the
/// methods of an impl (with their owner). Twins have the same name.
fn sibling_names(item: &ast::Item) -> Vec<String> {
    match item {
        ast::Item::Module(value) => vec![name_of(value)],
        ast::Item::Fn(value) => vec![name_of(value)],
        ast::Item::Impl(value) => {
            let owner = impl_owner(value);
            let mut names = vec![format!("impl {owner}")];
            names.extend(impl_methods(value).map(|method| format!("{owner}::{}", name_of(&method))));
            // Each other member with a name is an item (design 56.2).
            let members = value.assoc_item_list().into_iter().flat_map(|list| list.assoc_items());
            names.extend(members.filter(|member| !matches!(member, ast::AssocItem::Fn(method) if method.body().is_some())).filter_map(|member| member_name(member.syntax())).map(|name| format!("{owner}::{name}")));
            names
        }
        ast::Item::Struct(value) => vec![name_of(value)],
        ast::Item::Enum(value) => vec![name_of(value)],
        ast::Item::Trait(value) if value.eq_token().is_none() => vec![name_of(value)],
        ast::Item::Const(value) => vec![const_name(value)],
        ast::Item::Static(value) => vec![name_of(value)],
        ast::Item::TypeAlias(value) => vec![name_of(value)],
        // A macro definition (`macro_rules! name`). A macro call is not a twin of a definition
        // (batch 6b, part 2): the loader gives each call its own number.
        ast::Item::MacroRules(value) => vec![format!("{}!", name_of(value))],
        other => other_name(other).into_iter().collect(),
    }
}

/// The name of a member of a block that is not a macro call (design 56.2): a constant, a
/// type, a function, or a static.
fn member_name(node: &SyntaxNode) -> Option<String> {
    match node.kind() {
        SyntaxKind::CONST => ast::Const::cast(node.clone()).map(|value| const_name(&value)),
        SyntaxKind::TYPE_ALIAS => ast::TypeAlias::cast(node.clone()).map(|value| name_of(&value)),
        SyntaxKind::STATIC => ast::Static::cast(node.clone()).map(|value| name_of(&value)),
        SyntaxKind::FN => ast::Fn::cast(node.clone()).map(|value| name_of(&value)),
        _ => None,
    }
}

/// The name of a constant: its name, or `_`.
fn const_name(value: &ast::Const) -> String {
    value.name().map(|name| name.text().to_string()).unwrap_or_else(|| "_".to_string())
}

/// The methods of an impl: each function with a body.
fn impl_methods(item: &ast::Impl) -> impl Iterator<Item = ast::Fn> {
    item.assoc_item_list().into_iter().flat_map(|list| list.assoc_items()).filter_map(|member| match member {
        ast::AssocItem::Fn(method) if method.body().is_some() => Some(method),
        _ => None,
    })
}

/// The name of an item of the kind [`ItemKind::Other`], from its text: `extern crate rand`,
/// `extern C` (a block of foreign items), or the name of a union or of a trait alias. `None` for
/// an item with no name of its own: the loader gives it `item#N`, where N is its index among
/// such items of the module.
fn other_name(item: &ast::Item) -> Option<String> {
    match item {
        ast::Item::ExternCrate(value) => Some(format!("extern crate {}", value.name_ref().map(|name| name.text().to_string()).unwrap_or_default())),
        ast::Item::ExternBlock(value) => {
            let abi = value.abi().and_then(|abi| abi.abi_string()).and_then(|text| text.value().ok().map(|value| value.into_owned()));
            Some(format!("extern {}", abi.unwrap_or_else(|| "C".to_string())))
        }
        ast::Item::Union(value) => Some(name_of(value)),
        ast::Item::Trait(value) if value.eq_token().is_some() => Some(name_of(value)),
        _ => None,
    }
}

/// The owner text of an impl: `Cart`, or `<Cart as Display>` for a trait impl.
///
/// The generic arguments are not in the owner (`impl<T> Wrapper<T>` gives `Wrapper`), so an impl
/// keeps its identity when its generics or bounds change. The header change is a signature edit
/// of the impl. Two impls of one type (`impl W<u8>` and `impl W<u16>`) are twins.
///
/// The lifetimes are not in the owner (design 37.2): `impl<'a> Parse for &'a mut Node` and
/// `impl Parse for &mut Node` give `<&mutNode as Parse>`. The compiler does not permit two
/// impls of one trait that are different only in a lifetime.
fn impl_owner(item_impl: &ast::Impl) -> String {
    let self_ty = match item_impl.self_ty() {
        Some(ast::Type::PathType(path)) if path.path().is_some_and(|path| plain_path(&path)) => without_arguments(path.syntax()),
        Some(other) => without_lifetimes(other.syntax()).replace(' ', ""),
        None => String::new(),
    };
    let trait_path = item_impl.trait_().map(|ty| without_arguments(ty.syntax()));
    match trait_path {
        Some(trait_path) => format!("<{self_ty} as {trait_path}>"),
        None => self_ty,
    }
}

/// A path with no qualified start (`<T as Trait>::Name`).
fn plain_path(path: &ast::Path) -> bool {
    path.syntax().descendants().filter_map(ast::PathSegment::cast).all(|segment| segment.type_anchor().is_none())
}

/// A type without its lifetimes: `&'a mut Node<'b, T>` gives `&mut Node<T>`, and
/// `dyn Parse + 'a` gives `dyn Parse`.
fn without_lifetimes(ty: &SyntaxNode) -> String {
    let mut skip: Vec<TextRange> = Vec::new();
    for node in ty.descendants() {
        if let Some(reference) = ast::RefType::cast(node.clone()) {
            skip.extend(reference.lifetime().map(|lifetime| lifetime.syntax().text_range()));
        } else if let Some(list) = ast::GenericArgList::cast(node.clone()) {
            let args: Vec<ast::GenericArg> = list.generic_args().collect();
            if args.iter().all(|arg| matches!(arg, ast::GenericArg::LifetimeArg(_))) {
                skip.push(list.syntax().text_range());
            } else {
                skip.extend(args.iter().filter(|arg| matches!(arg, ast::GenericArg::LifetimeArg(_))).map(|arg| with_separator(arg.syntax(), T![,])));
            }
        } else if let Some(bounds) = ast::TypeBoundList::cast(node.clone()) {
            let lifetime = |bound: &ast::TypeBound| bound.lifetime().is_some() && bound.ty().is_none();
            let list: Vec<ast::TypeBound> = bounds.bounds().collect();
            if list.iter().any(|bound| !lifetime(bound)) {
                skip.extend(list.iter().filter(|bound| lifetime(bound)).map(|bound| with_separator(bound.syntax(), T![+])));
            }
        } else if node.kind() == SyntaxKind::FOR_BINDER {
            skip.push(node.text_range());
        }
    }
    let tokens: Vec<Flat> = flat_tokens(ty).into_iter().filter(|token| !skip.iter().any(|range| range.contains_range(TextRange::new((token.start as u32).into(), (token.end as u32).into())))).collect();
    flat_text(&tokens)
}

/// The range of a node with the separator after it, else with the separator before it.
fn with_separator(node: &SyntaxNode, separator: SyntaxKind) -> TextRange {
    let next = node.siblings_with_tokens(ra_ap_syntax::Direction::Next).skip(1).find(|element| !element.kind().is_trivia());
    if let Some(next) = next.filter(|element| element.kind() == separator) {
        return node.text_range().cover(next.text_range());
    }
    let before = node.siblings_with_tokens(ra_ap_syntax::Direction::Prev).skip(1).find(|element| !element.kind().is_trivia());
    match before.filter(|element| element.kind() == separator) {
        Some(before) => node.text_range().cover(before.text_range()),
        None => node.text_range(),
    }
}

/// A path without its generic arguments: `std::fmt::Display`, `Wrapper`.
fn without_arguments(path: &SyntaxNode) -> String {
    let skip: Vec<TextRange> = path
        .descendants()
        .filter_map(ast::PathSegment::cast)
        .flat_map(|segment| {
            let arguments = [segment.generic_arg_list().map(|list| list.syntax().text_range()), segment.parenthesized_arg_list().map(|list| list.syntax().text_range()), segment.ret_type().map(|ret| ret.syntax().text_range())];
            arguments.into_iter().flatten()
        })
        .collect();
    let tokens: Vec<Flat> = flat_tokens(path).into_iter().filter(|token| !skip.iter().any(|range| range.contains_range(TextRange::new((token.start as u32).into(), (token.end as u32).into())))).collect();
    flat_text(&tokens).replace(' ', "")
}

/// Gives each twin name of one item list its suffix (`#0`, `#1`, …) in source order.
struct Twins {
    count: BTreeMap<String, usize>,
    seen: BTreeMap<String, usize>,
}

impl Twins {
    fn new(items: &[ast::Item]) -> Twins {
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


/// One attribute of an item: an `#[…]` attribute or a doc comment (design 39.3).
#[derive(Clone)]
pub(crate) struct Attr {
    /// The compare text: `# [cfg (test)]`. A doc comment is `# [doc = "…"]`.
    pub(crate) text: String,
    /// The path: `doc`, `cfg`, `rustfmt::skip`.
    pub(crate) path: String,
    /// The tokens inside the brackets.
    pub(crate) content: Vec<TokenTree>,
    /// The display text: `#[cfg(test)]`, `/// x`.
    pub(crate) display: String,
    /// The value of a doc string (see [`Attr::doc_value`]).
    value: Option<String>,
}

impl Attr {
    /// A doc text: `/// x`, `#[doc = "x"]`, or `#[doc = include_str!(…)]`. A `#[doc(KEY)]`
    /// attribute (`doc(hidden)`, `doc(alias = "…")`) is not a doc text (batch 6b, part 1).
    pub(crate) fn doc_text(&self) -> bool {
        self.path == "doc" && matches!(self.content.get(1), Some(TokenTree::Punct(punct)) if punct.as_char() == '=')
    }

    /// The value of a doc string: `/// x` and `#[doc = "x"]` give ` x` and `x`.
    pub(crate) fn doc_value(&self) -> Option<String> {
        self.value.clone()
    }

    /// The value of `NAME = "…"` in the brackets.
    fn string_value(&self, name: &str) -> Option<String> {
        match self.content.as_slice() {
            [TokenTree::Ident(ident), TokenTree::Punct(punct), TokenTree::Literal(literal)] if ident == name && punct.as_char() == '=' => rs::string_value(&literal.to_string()),
            _ => None,
        }
    }
}

/// The attributes of a node: the outer ones, then the inner ones (`#![…]`, `//!`) of its
/// block, its module, or its impl.
pub(crate) fn attributes(node: &SyntaxNode) -> Vec<Attr> {
    let direct = |node: &SyntaxNode| -> Vec<Attr> { node.children().filter(|child| matches!(child.kind(), SyntaxKind::ATTR | SyntaxKind::DOC_COMMENT)).map(|child| attr(&child)).collect() };
    let mut out = direct(node);
    let inner = match node.kind() {
        SyntaxKind::FN => ast::Fn::cast(node.clone()).and_then(|function| function.body()).and_then(|body| body.stmt_list()).map(|list| list.syntax().clone()),
        SyntaxKind::MODULE | SyntaxKind::IMPL | SyntaxKind::TRAIT | SyntaxKind::EXTERN_BLOCK => {
            node.children().find(|child| matches!(child.kind(), SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST | SyntaxKind::EXTERN_ITEM_LIST))
        }
        _ => None,
    };
    if let Some(inner) = inner {
        out.extend(direct(&inner));
    }
    out
}

/// The attributes of a node as (path, compare text): the outer ones, then the inner ones.
pub fn attribute_texts(node: &SyntaxNode) -> Vec<(String, String)> {
    attributes(node).into_iter().map(|attr| (attr.path, attr.text)).collect()
}

fn attr(node: &SyntaxNode) -> Attr {
    let stream = TokenStream::from_node(node);
    let text = stream.to_string();
    let content: Vec<TokenTree> = stream.into_iter().find_map(|tree| match tree {
        TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket => Some(group.stream().into_iter().collect()),
        _ => None,
    }).unwrap_or_default();
    let path: String = content
        .iter()
        .take_while(|tree| matches!(tree, TokenTree::Ident(_)) || matches!(tree, TokenTree::Punct(punct) if punct.as_char() == ':'))
        .map(|tree| tree.to_string())
        .collect();
    // A doc comment has its value in its token. A `#[doc = "…"]` attribute has a string literal.
    let comment = node.first_token().and_then(|token| crate::tt::doc_comment(token.kind(), token.text())).map(|(_, value)| value);
    let value = comment.or_else(|| match content.as_slice() {
        [TokenTree::Ident(name), TokenTree::Punct(punct), TokenTree::Literal(literal)] if *name == "doc" && punct.as_char() == '=' => rs::string_value(&literal.to_string()),
        _ => None,
    });
    Attr { text, path, content, display: display_text(node), value }
}

/// One Rust file for the loader: its text, its line index, its edition, and its plain comments
/// (the start line and the text) from the tree (design 39.1, rule 4).
struct Source {
    rel: String,
    text: String,
    lines: Lines,
    edition: Edition,
    comments: Vec<(usize, String)>,
    /// The tokens of the file in order (for the compare texts).
    tokens: Vec<Flat>,
}

impl Source {
    fn new(rel: &str, text: &str, file: &ast::SourceFile, edition: Edition) -> Source {
        let lines = Lines::new(text);
        let mut comments = Vec::new();
        for token in file.syntax().descendants_with_tokens().filter_map(|element| element.into_token()).filter(|token| token.kind() == SyntaxKind::COMMENT) {
            let line = lines.pos(usize::from(token.text_range().start())).0;
            let comment = token.text();
            let shown = if comment.starts_with("//") { comment.trim().to_string() } else { comment.split_whitespace().collect::<Vec<_>>().join(" ") };
            comments.push((line, shown));
        }
        Source { rel: rel.to_string(), text: text.to_string(), lines, edition, comments, tokens: flat_tokens(file.syntax()) }
    }

    fn builder(&self) -> tree::Builder<'_> {
        tree::Builder::with_tokens(&self.lines, self.edition, &self.tokens)
    }
}

struct Loader<'a> {
    root: &'a Path,
    /// The items in load order. [`unique_paths`] puts them in the map.
    items: &'a mut Vec<Item>,
    parsed: &'a mut BTreeSet<String>,
    inner_attrs: &'a mut BTreeMap<String, (String, Vec<String>)>,
    inner_attrs_display: &'a mut BTreeMap<String, Vec<String>>,
    module_files: &'a mut BTreeMap<String, String>,
    macro_modules: &'a mut BTreeMap<String, MacroModule>,
    parse_errors: &'a mut BTreeMap<String, String>,
    editions: &'a mut BTreeMap<String, Edition>,
    crate_name: &'a str,
    edition: Edition,
}

impl Loader<'_> {
    fn load_file(&mut self, file: &Path, module: &str, test: bool) -> Result<()> {
        let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let rel = relative(self.root, file);
        let Some(parsed) = self.parse(&rel, &text) else { return Ok(()) };
        self.parsed.insert(rel.clone());
        // The child modules of a root file are in the folder of the root file, whatever its name.
        let child_dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let source = Source::new(&rel, &text, &parsed, self.edition);
        self.record_inner_attrs(module, &rel, parsed.syntax());
        let items: Vec<ast::Item> = parsed.items().collect();
        self.load_items(&items, module, &source, &child_dir, test)
    }

    /// Parses the text of a file. A file that does not parse does not stop the load (design
    /// 37.5, 39.4): the map of the parse errors gets the reason, and the result is `None`.
    fn parse(&mut self, rel: &str, text: &str) -> Option<ast::SourceFile> {
        self.editions.insert(rel.to_string(), self.edition);
        match rs::parse_file(text, self.edition) {
            Ok(parsed) => Some(parsed),
            Err(reason) => {
                self.parse_errors.insert(rel.to_string(), reason);
                None
            }
        }
    }

    fn load_items(&mut self, items: &[ast::Item], module: &str, source: &Source, dir: &Path, test: bool) -> Result<()> {
        let mut use_index = 0;
        let mut other_index = 0;
        let mut twins = Twins::new(items);
        let mut call_index: BTreeMap<String, usize> = BTreeMap::new();
        // Design 59, A-a: the `mod NAME;` lines in the inputs of the macro calls of the list,
        // with the path of the call, in source order.
        let mut declared: Vec<(Declaration, String)> = Vec::new();
        for item in items {
            let attrs = attributes(item.syntax());
            match item {
                ast::Item::Module(item_mod) => {
                    let name = name_of(item_mod);
                    let path = format!("{module}::{}", twins.name(&name));
                    let is_test = test || is_cfg_test(&attrs);
                    if let Some(content) = item_mod.item_list() {
                        let content: Vec<ast::Item> = content.items().collect();
                        self.load_items(&content, &path, source, &dir.join(&name), is_test)?;
                    } else {
                        // `#[path = "…"]` names the file (invariant I4: the syntax tree must read the
                        // file that the compiler reads). Outside an inline module, the path is
                        // relative to the folder of the current file. Inside an inline module, it
                        // is relative to the folder of that module.
                        // A `cfg_attr(COND, path = "…")` attribute names one more file (design
                        // 37.3). The loader reads each file that exists, also if the condition
                        // is off. With two files or more, each file is a twin module (`name#N`).
                        let file = &source.rel;
                        let base = if *dir == module_dir(&self.root.join(file)) { self.root.join(file).parent().map(Path::to_path_buf).unwrap_or_default() } else { dir.to_path_buf() };
                        let files = mod_files(&attrs, &base, dir, &name);
                        for (index, (found, by_attr)) in files.iter().enumerate() {
                            let path = if files.len() > 1 { format!("{path}#{index}") } else { path.clone() };
                            let text = std::fs::read_to_string(found)?;
                            let rel = relative(self.root, found);
                            let Some(parsed) = self.parse(&rel, &text) else { continue };
                            self.parsed.insert(rel.clone());
                            self.record_inner_attrs(&path, &rel, parsed.syntax());
                            // A file that `#[path]` names is a "mod-rs" file: its child modules are in its own folder.
                            let child_dir = if *by_attr { found.parent().map(Path::to_path_buf).unwrap_or_default() } else { module_dir(found) };
                            let inner = Source::new(&rel, &text, &parsed, self.edition);
                            let content: Vec<ast::Item> = parsed.items().collect();
                            self.load_items(&content, &path, &inner, &child_dir, is_test)?;
                        }
                    }
                    self.add(path, ItemKind::Mod, item, &vis_text(item_mod), &attrs, None, Vec::new(), source, is_test);
                }
                ast::Item::Fn(item_fn) => {
                    let path = format!("{module}::{}", twins.name(&name_of(item_fn)));
                    let body = item_fn.body().map(|body| source.builder().block("body", &body));
                    let is_test = test || has_attr(&attrs, "test");
                    self.add(path, ItemKind::Fn, item, &vis_text(item_fn), &attrs, body, Vec::new(), source, is_test);
                    self.set_signature(source, item_fn);
                }
                ast::Item::Impl(item_impl) => {
                    let owner = impl_owner(item_impl);
                    for method in impl_methods(item_impl) {
                        let path = format!("{module}::{}", twins.name(&format!("{owner}::{}", name_of(&method))));
                        let body = method.body().map(|body| source.builder().block("body", &body));
                        let method_attrs = attributes(method.syntax());
                        let is_test = test || has_attr(&method_attrs, "test");
                        let signature = texts(&tree::signature_tokens(&method));
                        self.add_parts(path, ItemKind::Fn, method.syntax(), &vis_text(&method), &method_attrs, body, Vec::new(), source, is_test, signature);
                        self.set_signature(source, &method);
                    }
                    // Each other member is an item with the path form of a method (design
                    // 56.2): a constant, a type, a macro call.
                    let mut members = Vec::new();
                    for member in item_impl.assoc_item_list().into_iter().flat_map(|list| list.assoc_items()) {
                        match &member {
                            ast::AssocItem::Fn(method) if method.body().is_some() => members.push(both(vec![format!("fn {}", name_of(method))]).remove(0)),
                            other => {
                                let mut path_of = |call: bool, name: &str| match call {
                                    true => {
                                        let index = call_index.entry(format!("{owner}::{name}")).or_insert(0);
                                        *index += 1;
                                        format!("{module}::{owner}::{name}#{}", *index - 1)
                                    }
                                    false => format!("{module}::{}", twins.name(&format!("{owner}::{name}"))),
                                };
                                members.push(self.add_member(other.syntax(), &mut path_of, source, test));
                            }
                        }
                    }
                    let path = format!("{module}::{}", twins.name(&format!("impl {owner}")));
                    self.add(path, ItemKind::Impl, item, &Text::default(), &attrs, None, members, source, test);
                }
                ast::Item::Struct(item_struct) => {
                    let path = format!("{module}::{}", twins.name(&name_of(item_struct)));
                    let members = both(self.add_fields(&path, item_struct.field_list(), source, test));
                    self.add(path, ItemKind::Struct, item, &vis_text(item_struct), &attrs, None, members, source, test);
                }
                ast::Item::Enum(item_enum) => {
                    let path = format!("{module}::{}", twins.name(&name_of(item_enum)));
                    // The member list of an enum is the order of its variant names. Each variant is
                    // an item, and each variant field is an item.
                    let variants: Vec<ast::Variant> = item_enum.variant_list().into_iter().flat_map(|list| list.variants()).collect();
                    let mut variant_twins = Twins::from_names(variants.iter().map(name_of));
                    for variant in &variants {
                        let variant_path = format!("{path}::{}", variant_twins.name(&name_of(variant)));
                        let fields = both(self.add_fields(&variant_path, variant.field_list(), source, test));
                        // The header: the name, the kind of the fields, and the discriminant.
                        let start = variant.name().map_or(0, |name| usize::from(name.syntax().text_range().start()));
                        let header = without_parts(variant.syntax(), start, &variant.field_list().map(|list| list.syntax().clone()).into_iter().collect::<Vec<_>>());
                        self.add_parts(variant_path, ItemKind::Variant, variant.syntax(), &Text::default(), &attributes(variant.syntax()), None, fields, source, test, header);
                    }
                    let members = both(variants.iter().map(name_of).collect());
                    self.add(path, ItemKind::Enum, item, &vis_text(item_enum), &attrs, None, members, source, test);
                }
                ast::Item::Trait(item_trait) if item_trait.eq_token().is_none() => {
                    let path = format!("{module}::{}", twins.name(&name_of(item_trait)));
                    // A trait method is a function item: `shop::Price::label`. A default body gets
                    // statement edits, and its MIR name is the same path. The member list keeps the
                    // order of the methods and the other trait items as text.
                    let trait_items: Vec<ast::AssocItem> = item_trait.assoc_item_list().into_iter().flat_map(|list| list.assoc_items()).collect();
                    let mut method_twins = Twins::from_names(trait_items.iter().filter_map(|it| member_name(it.syntax())));
                    let mut calls: BTreeMap<String, usize> = BTreeMap::new();
                    let mut members = Vec::new();
                    for trait_item in &trait_items {
                        match trait_item {
                            ast::AssocItem::Fn(method) => {
                                let method_path = format!("{path}::{}", method_twins.name(&name_of(method)));
                                let body = method.body().map(|block| source.builder().block("body", &block));
                                let method_attrs = attributes(method.syntax());
                                let is_test = test || has_attr(&method_attrs, "test");
                                let signature = texts(&tree::signature_tokens(method));
                                self.add_parts(method_path, ItemKind::Fn, method.syntax(), &Text::default(), &method_attrs, body, Vec::new(), source, is_test, signature);
                                self.set_signature(source, method);
                                let name = format!("fn {}", name_of(method));
                                members.push((name.clone(), name));
                            }
                            // A constant, a type, and a macro call of a trait are items
                            // (design 56.2).
                            other => {
                                let mut path_of = |call: bool, name: &str| match call {
                                    true => {
                                        let index = calls.entry(name.to_string()).or_insert(0);
                                        *index += 1;
                                        format!("{path}::{name}#{}", *index - 1)
                                    }
                                    false => format!("{path}::{}", method_twins.name(name)),
                                };
                                members.push(self.add_member(other.syntax(), &mut path_of, source, test));
                            }
                        }
                    }
                    self.add(path, ItemKind::Trait, item, &vis_text(item_trait), &attrs, None, members, source, test);
                }
                ast::Item::Const(item_const) => {
                    let path = format!("{module}::{}", twins.name(&const_name(item_const)));
                    self.add(path, ItemKind::Const, item, &vis_text(item_const), &attrs, None, Vec::new(), source, test);
                }
                ast::Item::Static(item_static) => {
                    let path = format!("{module}::{}", twins.name(&name_of(item_static)));
                    self.add(path, ItemKind::Static, item, &vis_text(item_static), &attrs, None, Vec::new(), source, test);
                }
                ast::Item::TypeAlias(item_type) => {
                    let path = format!("{module}::{}", twins.name(&name_of(item_type)));
                    self.add(path, ItemKind::Type, item, &vis_text(item_type), &attrs, None, Vec::new(), source, test);
                }
                ast::Item::Use(item_use) => {
                    let path = format!("{module}::use#{use_index}");
                    use_index += 1;
                    self.add(path, ItemKind::Use, item, &vis_text(item_use), &attrs, None, Vec::new(), source, test);
                }
                ast::Item::MacroRules(_) | ast::Item::MacroCall(_) => {
                    // A definition keeps the plain name `name!`, with a twin suffix only if the
                    // module has two definitions of the name. A call always has a number:
                    // `name!#N`, in source order. So a new call at the end changes no other
                    // name (batch 6b, part 2). With two or more definitions, the numbers of the
                    // calls start after the numbers of the definitions.
                    let path = match item {
                        ast::Item::MacroRules(definition) => format!("{module}::{}", twins.name(&format!("{}!", name_of(definition)))),
                        ast::Item::MacroCall(call) => {
                            // The path of the call has no space (design 59): `crate::table!`.
                            let name = format!("{}!", compare_opt(call.path().as_ref()).replace(' ', ""));
                            let definitions = twins.count.get(&name).copied().filter(|count| *count >= 2).unwrap_or(0);
                            let index = call_index.entry(name.clone()).or_insert(definitions);
                            *index += 1;
                            let path = format!("{module}::{name}#{}", *index - 1);
                            if let Some(tree) = call.token_tree() {
                                let mut found = Vec::new();
                                declarations(tree.syntax(), &source.text, self.edition, &[], &mut found);
                                declared.extend(found.into_iter().map(|declaration| (declaration, path.clone())));
                            }
                            path
                        }
                        _ => unreachable!(),
                    };
                    self.add(path, ItemKind::Macro, item, &Text::default(), &attrs, None, Vec::new(), source, test);
                }
                ast::Item::ExternBlock(block) => {
                    // A block of foreign items (design 56.2): each member is an item with a
                    // path in the block. The signature of the block is its header.
                    let path = format!("{module}::{}", twins.name(&other_name(item).unwrap_or_default()));
                    let list = block.extern_item_list();
                    let foreign: Vec<ast::ExternItem> = list.iter().flat_map(|list| list.extern_items()).collect();
                    let mut member_twins = Twins::from_names(foreign.iter().filter_map(|member| member_name(member.syntax())));
                    let mut calls: BTreeMap<String, usize> = BTreeMap::new();
                    let mut members = Vec::new();
                    for member in &foreign {
                        let mut path_of = |call: bool, name: &str| match call {
                            true => {
                                let index = calls.entry(name.to_string()).or_insert(0);
                                *index += 1;
                                format!("{path}::{name}#{}", *index - 1)
                            }
                            false => format!("{path}::{}", member_twins.name(name)),
                        };
                        members.push(self.add_member(member.syntax(), &mut path_of, source, test));
                    }
                    let header = without_parts(block.syntax(), plain_start(block.syntax()), &list.map(|list| list.syntax().clone()).into_iter().collect::<Vec<_>>());
                    self.add_parts(path, ItemKind::Other, item.syntax(), &Text::default(), &attrs, None, members, source, test, header);
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
                    // The docs and the attributes are parts of their own (design 56.2, rule
                    // 2): the signature is the text after them.
                    let signature = without_parts(other.syntax(), plain_start(other.syntax()), &[]);
                    self.add_parts(format!("{module}::{name}"), ItemKind::Other, item.syntax(), &Text::default(), &attrs, None, Vec::new(), source, test, signature);
                }
            }
        }
        self.load_declared(items, module, source, dir, test, declared)
    }

    /// Loads the module files of the `mod NAME;` lines in the inputs of the macro calls of one
    /// item list (design 59, A-b to A-h). The file of a declaration follows the rule of a plain
    /// `mod` line, and the parent is the module of the call. A declaration with no file gives
    /// no module. Two declarations of one path with one file give one module. Two files of one
    /// path are twin modules (`NAME#0`, `NAME#1`), as for `cfg_attr(…, path = …)`. A name of a
    /// plain `mod` line of the same list gives no module here: the plain line has the module,
    /// and a different file keeps its `file` fact.
    fn load_declared(&mut self, items: &[ast::Item], module: &str, source: &Source, dir: &Path, test: bool, declared: Vec<(Declaration, String)>) -> Result<()> {
        if declared.is_empty() {
            return Ok(());
        }
        let plain: BTreeSet<String> = items.iter().filter_map(|item| match item {
            ast::Item::Module(item_mod) => Some(name_of(item_mod)),
            _ => None,
        }).collect();
        let file = &source.rel;
        let file_base = if *dir == module_dir(&self.root.join(file)) { self.root.join(file).parent().map(Path::to_path_buf).unwrap_or_default() } else { dir.to_path_buf() };
        // The files of each path, in source order: (file, by attribute, test, declaration, call).
        let mut by_path: BTreeMap<Vec<String>, Vec<(PathBuf, bool, bool, Declaration, String)>> = BTreeMap::new();
        let mut order: Vec<Vec<String>> = Vec::new();
        for (declaration, call) in declared {
            if declaration.names.len() == 1 && plain.contains(&declaration.names[0]) {
                continue;
            }
            // A declaration in a `mod NAME { … }` of the input: its folder is below `NAME/`.
            let inner_dir = declaration.names[..declaration.names.len() - 1].iter().fold(dir.to_path_buf(), |dir, name| dir.join(name));
            let base = if declaration.names.len() == 1 { file_base.clone() } else { inner_dir.clone() };
            let name = declaration.names.last().cloned().unwrap_or_default();
            let is_test = test || is_cfg_test(&declaration.attrs);
            let list = by_path.entry(declaration.names.clone()).or_insert_with(|| {
                order.push(declaration.names.clone());
                Vec::new()
            });
            for (found, by_attr) in mod_files(&declaration.attrs, &base, &inner_dir, &name) {
                if !list.iter().any(|entry| entry.0 == found) {
                    list.push((found, by_attr, is_test, declaration.clone(), call.clone()));
                }
            }
        }
        for names in order {
            let files = &by_path[&names];
            let path = format!("{module}::{}", names.join("::"));
            for (index, (found, by_attr, is_test, declaration, call)) in files.iter().enumerate() {
                let path = if files.len() > 1 { format!("{path}#{index}") } else { path.clone() };
                let text = std::fs::read_to_string(found)?;
                let rel = relative(self.root, found);
                let Some(parsed) = self.parse(&rel, &text) else { continue };
                self.parsed.insert(rel.clone());
                self.record_inner_attrs(&path, &rel, parsed.syntax());
                self.macro_modules.insert(path.clone(), MacroModule { call: call.clone(), file: rel.clone(), at: (source.rel.clone(), declaration.line), text: declaration.text.clone() });
                let child_dir = if *by_attr { found.parent().map(Path::to_path_buf).unwrap_or_default() } else { module_dir(found) };
                let inner = Source::new(&rel, &text, &parsed, self.edition);
                let content: Vec<ast::Item> = parsed.items().collect();
                self.load_items(&content, &path, &inner, &child_dir, *is_test)?;
            }
        }
        Ok(())
    }

    /// Sets the signature node of the last item (a function).
    fn set_signature(&mut self, source: &Source, function: &ast::Fn) {
        if let Some(item) = self.items.last_mut() {
            item.sig = Some(source.builder().signature(function));
        }
    }

    /// Records the inner attributes of a file (`#![…]`). An inner doc comment is stored as a
    /// `//!` line, as in the source. Each other attribute is stored in its token form.
    fn record_inner_attrs(&mut self, module: &str, file: &str, root: &SyntaxNode) {
        self.module_files.insert(module.to_string(), file.to_string());
        let attrs = attributes(root);
        if !attrs.is_empty() {
            let text = |attr: &Attr| match attr.doc_value() {
                Some(value) => doc_lines("//!", &value),
                None => attr.text.clone(),
            };
            self.inner_attrs.insert(module.to_string(), (file.to_string(), attrs.iter().map(text).collect()));
            let shown = |attr: &Attr| match attr.doc_value() {
                Some(value) => doc_lines("//!", &value),
                None => attr.display.clone(),
            };
            self.inner_attrs_display.insert(module.to_string(), attrs.iter().map(shown).collect());
        }
    }

    /// Adds the fields of a struct or a variant as items (`owner.name`, or `owner.0` for a tuple
    /// field), and returns the member list of the owner: the field names in order.
    fn add_fields(&mut self, owner: &str, fields: Option<ast::FieldList>, source: &Source, test: bool) -> Vec<String> {
        let mut names = Vec::new();
        let ty = |ty: Option<ast::Type>| ty.map(|ty| node_texts(ty.syntax())).unwrap_or_default();
        let fields: Vec<(String, SyntaxNode, Text, Text)> = match fields {
            Some(ast::FieldList::RecordFieldList(list)) => list.fields().map(|field| (name_of(&field), field.syntax().clone(), vis_text(&field), ty(field.ty()))).collect(),
            Some(ast::FieldList::TupleFieldList(list)) => list.fields().enumerate().map(|(index, field)| (index.to_string(), field.syntax().clone(), vis_text(&field), ty(field.ty()))).collect(),
            None => Vec::new(),
        };
        for (name, node, vis, ty) in fields {
            self.add_parts(format!("{owner}.{name}"), ItemKind::Field, &node, &vis, &attributes(&node), None, Vec::new(), source, test, ty);
            names.push(name);
        }
        names
    }

    /// Adds one member of a block as an item (design 56.2): a constant or a type of an impl
    /// or of a trait, a function with no body, a static, or a macro call. `path_of` gives the
    /// path for a name; its first argument is true for a macro call. The result is the entry
    /// of the member list of the block: the kind and the name.
    fn add_member(&mut self, node: &SyntaxNode, path_of: &mut dyn FnMut(bool, &str) -> String, source: &Source, test: bool) -> Text {
        let attrs = attributes(node);
        let one = |text: String| (text.clone(), text);
        // The signature of a constant, a type, and a static: the text after the docs, the
        // attributes, and the visibility.
        let plain = |node: &SyntaxNode| {
            let skip: Vec<TextRange> = node.children().filter(|child| matches!(child.kind(), SyntaxKind::VISIBILITY | SyntaxKind::DOC_COMMENT | SyntaxKind::ATTR)).map(|child| child.text_range()).collect();
            let tokens: Vec<Flat> = flat_tokens(node).into_iter().filter(|token| !skip.iter().any(|range| usize::from(range.start()) <= token.start && token.end <= usize::from(range.end()))).collect();
            texts(&tokens)
        };
        if let Some(value) = ast::Const::cast(node.clone()) {
            let name = const_name(&value);
            self.add_parts(path_of(false, &name), ItemKind::Const, node, &vis_text(&value), &attrs, None, Vec::new(), source, test, plain(node));
            one(format!("const {name}"))
        } else if let Some(value) = ast::TypeAlias::cast(node.clone()) {
            let name = name_of(&value);
            self.add_parts(path_of(false, &name), ItemKind::Type, node, &vis_text(&value), &attrs, None, Vec::new(), source, test, plain(node));
            one(format!("type {name}"))
        } else if let Some(value) = ast::Static::cast(node.clone()) {
            let name = name_of(&value);
            self.add_parts(path_of(false, &name), ItemKind::Static, node, &vis_text(&value), &attrs, None, Vec::new(), source, test, plain(node));
            one(format!("static {name}"))
        } else if let Some(value) = ast::Fn::cast(node.clone()) {
            let name = name_of(&value);
            let body = value.body().map(|body| source.builder().block("body", &body));
            let is_test = test || has_attr(&attrs, "test");
            self.add_parts(path_of(false, &name), ItemKind::Fn, node, &vis_text(&value), &attrs, body, Vec::new(), source, is_test, texts(&tree::signature_tokens(&value)));
            self.set_signature(source, &value);
            one(format!("fn {name}"))
        } else if let Some(call) = ast::MacroCall::cast(node.clone()) {
            let name = format!("{}!", compare_opt(call.path().as_ref()));
            let path = path_of(true, &name);
            let entry = path.rsplit("::").next().unwrap_or(&name).to_string();
            self.add_parts(path, ItemKind::Macro, node, &Text::default(), &attrs, None, Vec::new(), source, test, call_texts(node));
            one(entry)
        } else {
            node_texts(node)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add(&mut self, path: String, kind: ItemKind, item: &ast::Item, vis: &Text, attrs: &[Attr], body: Option<Node>, members: Vec<Text>, source: &Source, test: bool) {
        let signature = signature_of(item);
        self.add_parts(path, kind, item.syntax(), vis, attrs, body, members, source, test, signature);
    }

    #[allow(clippy::too_many_arguments)]
    fn add_parts(&mut self, path: String, kind: ItemKind, value: &SyntaxNode, vis: &Text, attrs: &[Attr], body: Option<Node>, members: Vec<Text>, source: &Source, test: bool, signature: Text) {
        let range = rs::code_range(value);
        let (line_start, col_start) = source.lines.pos(range.start);
        let (line_end, col_end) = source.lines.pos(range.end);
        let text_source = source_lines(&source.text, line_start, line_end);
        // Doc comments are stored as `///` lines, as in the source. A block comment
        // (`/** … */`) has more than one line: each line is a doc line.
        let doc_line = |attr: &Attr| match attr.doc_value() {
            Some(value) => doc_lines("///", &value),
            None => attr.text.clone(),
        };
        let mut docs: Vec<String> = attrs.iter().filter(|attr| attr.doc_text()).map(doc_line).collect();
        // The plain comments in the lines of the item, from the tree (design 39.1, rule 4).
        docs.extend(source.comments.iter().filter(|(line, _)| (line_start..=line_end).contains(line)).map(|(_, text)| text.clone()));
        let (attrs, attr_display): (Vec<String>, Vec<String>) = attrs.iter().filter(|attr| !attr.doc_text()).map(|attr| (attr.text.clone(), attr.display.clone())).unzip();
        let (members, member_display): (Vec<String>, Vec<String>) = members.into_iter().unzip();
        let item = Item {
            path: path.clone(),
            kind,
            crate_name: self.crate_name.to_string(),
            file: source.rel.clone(),
            line_start,
            line_end,
            col_start,
            col_end,
            vis: vis.0.clone(),
            signature: signature.0,
            docs,
            attrs,
            members,
            body,
            sig: None,
            text: source.builder().text(value),
            source: text_source,
            test,
            display: ItemDisplay { vis: vis.1.clone(), signature: signature.1, attrs: attr_display, members: member_display },
            macro_definition: value.kind() == SyntaxKind::MACRO_RULES,
        };
        self.items.push(item);
    }
}

/// A `mod NAME;` line in the tokens of a macro input (design 59, A-a).
#[derive(Clone)]
struct Declaration {
    /// The names of the `mod NAME { … }` blocks of the input around the line, then the name.
    names: Vec<String>,
    /// The attributes of the line.
    attrs: Vec<Attr>,
    /// The line of the `mod` token.
    line: usize,
    /// The text of the line: `#[path = "a.rs"] pub mod a;`.
    text: String,
}

/// The `mod NAME;` lines of a token tree, at each depth (design 59, A-a). A line is this token
/// sequence: attributes, a visibility, `mod`, one identifier, `;`. The attributes and the
/// visibility are optional. `mod $name;` is not a line: `$` is not an identifier. The tokens of
/// a `macro_rules!` definition in the input are not lines. A `mod NAME { … }` block in the
/// input gives its name to the lines in its braces (A-h). `outer`: the names of the blocks
/// around the tree.
fn declarations(tree: &SyntaxNode, text: &str, edition: Edition, outer: &[String], out: &mut Vec<Declaration>) {
    use rs::NodeOrToken;
    let elements: Vec<rs::SyntaxElement> = tree.children_with_tokens().filter(|element| !element.kind().is_trivia()).collect();
    let word = |index: usize| -> Option<String> { elements.get(index).and_then(|element| element.as_token()).map(|token| token.text().to_string()) };
    let group = |index: usize, open: &str| elements.get(index).and_then(|element| element.as_node()).is_some_and(|node| node.kind() == SyntaxKind::TOKEN_TREE && node.first_token().is_some_and(|token| token.text() == open));
    let ident = |index: usize| elements.get(index).and_then(|element| element.as_token()).filter(|token| token.kind() == SyntaxKind::IDENT).map(|token| token.text().trim_start_matches("r#").to_string());
    for (index, element) in elements.iter().enumerate() {
        match element {
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::TOKEN_TREE => {
                // The body of a `macro_rules!` definition (A-f).
                if index >= 3 && word(index - 3).as_deref() == Some("macro_rules") && word(index - 2).as_deref() == Some("!") && ident(index - 1).is_some() {
                    continue;
                }
                let mut names = outer.to_vec();
                if index >= 2 && word(index - 2).as_deref() == Some("mod") && group(index, "{") {
                    names.extend(ident(index - 1));
                }
                declarations(node, text, edition, &names, out);
            }
            NodeOrToken::Token(token) if token.text() == "mod" => {
                let (Some(name), Some(";")) = (ident(index + 1), word(index + 2).as_deref()) else { continue };
                // The visibility, then the attributes, before `mod`.
                let mut start = index;
                if start >= 1 && word(start - 1).as_deref() == Some("pub") {
                    start -= 1;
                } else if start >= 2 && group(start - 1, "(") && word(start - 2).as_deref() == Some("pub") {
                    start -= 2;
                }
                while start >= 2 && group(start - 1, "[") && word(start - 2).as_deref() == Some("#") {
                    start -= 2;
                }
                let from = usize::from(elements[start].text_range().start());
                let to = usize::from(elements[index + 2].text_range().end());
                let Some(line_text) = text.get(from..to) else { continue };
                // The attributes come from a parse of the text of the line.
                let Ok(parsed) = rs::parse_file(line_text, edition) else { continue };
                let Some(ast::Item::Module(item_mod)) = parsed.items().next() else { continue };
                if item_mod.item_list().is_some() || name_of(&item_mod).trim_start_matches("r#") != name {
                    continue;
                }
                let line = text[..usize::from(token.text_range().start())].matches('\n').count() + 1;
                let mut names = outer.to_vec();
                names.push(name);
                out.push(Declaration { names, attrs: attributes(item_mod.syntax()), line, text: tree::one_line(line_text) });
            }
            _ => {}
        }
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

/// The files of a `mod name;` declaration that exist, with the answer to "does an attribute
/// name the file?". A `cfg_attr(COND, path = "…")` attribute names a file for a part of the
/// configurations (design 37.3). The list has each such file, then the file of the plain
/// `#[path]` attribute or the usual file of the module.
fn mod_files(attrs: &[Attr], base: &Path, dir: &Path, name: &str) -> Vec<(PathBuf, bool)> {
    let mut files: Vec<(PathBuf, bool)> = Vec::new();
    let mut add = |file: PathBuf, by_attr: bool| {
        if file.is_file() && !files.iter().any(|(found, _)| *found == file) {
            files.push((file, by_attr));
        }
    };
    for target in attrs.iter().flat_map(conditional_paths) {
        add(base.join(target), true);
    }
    match path_attr(attrs) {
        Some(target) => add(base.join(target), true),
        None => {
            let usual = [dir.join(format!("{name}.rs")), dir.join(name).join("mod.rs")];
            if let Some(found) = usual.into_iter().find(|candidate| candidate.is_file()) {
                add(found, false);
            }
        }
    }
    files
}

/// The values of `path = "…"` in a `cfg_attr` attribute (also in a `cfg_attr` in it).
fn conditional_paths(attr: &Attr) -> Vec<String> {
    fn of_meta(meta: &[TokenTree], out: &mut Vec<String>) {
        let [TokenTree::Ident(name), TokenTree::Group(group)] = meta else { return };
        if *name != "cfg_attr" || group.delimiter() != Delimiter::Parenthesis {
            return;
        }
        let mut parts: Vec<Vec<TokenTree>> = vec![Vec::new()];
        for tree in group.stream() {
            match &tree {
                TokenTree::Punct(punct) if punct.as_char() == ',' => parts.push(Vec::new()),
                _ => parts.last_mut().expect("a part").push(tree),
            }
        }
        for part in parts.iter().skip(1) {
            match part.as_slice() {
                [TokenTree::Ident(ident), TokenTree::Punct(punct), TokenTree::Literal(literal)] if *ident == "path" && punct.as_char() == '=' => out.extend(rs::string_value(&literal.to_string())),
                other => of_meta(other, out),
            }
        }
    }
    let mut out = Vec::new();
    of_meta(&attr.content, &mut out);
    out
}

/// The value of a `#[path = "…"]` attribute.
fn path_attr(attrs: &[Attr]) -> Option<String> {
    attrs.iter().filter(|attr| attr.path == "path").find_map(|attr| attr.string_value("path"))
}

fn has_attr(attrs: &[Attr], name: &str) -> bool {
    attrs.iter().any(|attr| attr.path.rsplit("::").next() == Some(name))
}

fn is_cfg_test(attrs: &[Attr]) -> bool {
    attrs.iter().any(|attr| attr.path == "cfg" && attr.text.contains("test"))
}

/// The visibility text of a node: `pub`, `pub (crate)`, or empty, and its display text.
fn vis_text(node: &impl ast::HasVisibility) -> Text {
    match node.visibility() {
        Some(vis) => (compare_text(vis.syntax()), display_text(vis.syntax())),
        None => Default::default(),
    }
}

/// A list of names as compare texts and display texts.
fn both(names: Vec<String>) -> Vec<Text> {
    names.into_iter().map(|name| (name.clone(), name)).collect()
}

/// The compare text and the display text of a list of tokens.
fn texts(tokens: &[Flat]) -> Text {
    (flat_text(tokens), flat_display(tokens))
}

/// The compare text and the display text of a node.
fn node_texts(node: &SyntaxNode) -> Text {
    (compare_text(node), display_text(node))
}

/// The offset of the first token of a node that is not in a doc comment or in an attribute.
fn plain_start(node: &SyntaxNode) -> usize {
    let first = node.children_with_tokens().find(|child| !matches!(child.kind(), SyntaxKind::ATTR | SyntaxKind::DOC_COMMENT | SyntaxKind::WHITESPACE | SyntaxKind::COMMENT));
    first.map_or(usize::from(node.text_range().start()), |child| usize::from(child.text_range().start()))
}

/// The compare text of a node from a start offset, with each part of `parts` replaced by its
/// delimiters: a header with its field list `{ }` or `()`.
fn without_parts(node: &SyntaxNode, start: usize, parts: &[SyntaxNode]) -> Text {
    // The range of each part, and the range of its code (from its first to its last token).
    let ranges: Vec<(TextRange, std::ops::Range<usize>)> = parts.iter().map(|part| (part.text_range(), rs::code_range(part))).collect();
    let mut tokens = Vec::new();
    for token in flat_tokens(node).into_iter().filter(|token| token.start >= start) {
        let part = ranges.iter().find(|(range, _)| usize::from(range.start()) <= token.start && token.end <= usize::from(range.end()));
        match part {
            // Only the first and the last token of the part (its delimiters) stay.
            Some((_, code)) => {
                if token.start == code.start || token.end == code.end {
                    tokens.push(token);
                }
            }
            None => tokens.push(token),
        }
    }
    texts(&tokens)
}

/// The offset of the first of the tokens.
fn first_offset(tokens: impl IntoIterator<Item = Option<SyntaxToken>>) -> usize {
    tokens.into_iter().flatten().map(|token| usize::from(token.text_range().start())).min().unwrap_or(0)
}

/// The signature text of an item: everything except the body, the docs, and the visibility.
fn signature_of(item: &ast::Item) -> Text {
    let start = |tokens: Vec<Option<SyntaxToken>>| first_offset(tokens);
    match item {
        ast::Item::Fn(item_fn) => texts(&tree::signature_tokens(item_fn)),
        ast::Item::Impl(item_impl) => {
            let from = start(vec![item_impl.default_token(), item_impl.unsafe_token(), item_impl.impl_token()]);
            let list: Vec<SyntaxNode> = item_impl.assoc_item_list().map(|list| list.syntax().clone()).into_iter().collect();
            without_parts(item_impl.syntax(), from, &list)
        }
        ast::Item::Module(item_mod) => {
            let text = format!("mod {}", name_of(item_mod));
            (text.clone(), text)
        }
        // The members (fields, variants, trait items) are items of their own, and the attributes
        // are compared on their own. The signature is the header: the generics, the bounds, and
        // the kind of the fields (named, tuple, or unit).
        ast::Item::Struct(value) => without_parts(value.syntax(), start(vec![value.struct_token()]), &value.field_list().map(|list| list.syntax().clone()).into_iter().collect::<Vec<_>>()),
        ast::Item::Enum(value) => without_parts(value.syntax(), start(vec![value.enum_token()]), &value.variant_list().map(|list| list.syntax().clone()).into_iter().collect::<Vec<_>>()),
        ast::Item::Trait(value) if value.eq_token().is_none() => {
            without_parts(value.syntax(), start(vec![value.unsafe_token(), value.auto_token(), value.trait_token()]), &value.assoc_item_list().map(|list| list.syntax().clone()).into_iter().collect::<Vec<_>>())
        }
        // A constant, a static, a type alias, a `use` line: the text without the doc attributes
        // and the visibility.
        ast::Item::Const(_) | ast::Item::Static(_) | ast::Item::TypeAlias(_) | ast::Item::Use(_) => {
            let skip: Vec<TextRange> = item
                .syntax()
                .children()
                .filter(|child| child.kind() == SyntaxKind::VISIBILITY || child.kind() == SyntaxKind::DOC_COMMENT || (child.kind() == SyntaxKind::ATTR && attr(child).path == "doc"))
                .map(|child| child.text_range())
                .collect();
            let tokens: Vec<Flat> = flat_tokens(item.syntax()).into_iter().filter(|token| !skip.iter().any(|range| usize::from(range.start()) <= token.start && token.end <= usize::from(range.end()))).collect();
            texts(&tokens)
        }
        ast::Item::MacroCall(call) => call_texts(call.syntax()),
        other => node_texts(other.syntax()),
    }
}

/// The texts of a macro call (design 57.2): each token of the call, which is the path, the `!`,
/// the delimiters, and the full input. The outer attributes and the docs of the call item are
/// not in the texts: they have their own facts (design 55.1). The compare text has the `;` of
/// the item. The display text does not have it.
fn call_texts(node: &SyntaxNode) -> Text {
    let skip: Vec<TextRange> = node.children().filter(|child| matches!(child.kind(), SyntaxKind::DOC_COMMENT | SyntaxKind::ATTR)).map(|child| child.text_range()).collect();
    let mut tokens: Vec<Flat> = flat_tokens(node).into_iter().filter(|token| !skip.iter().any(|range| usize::from(range.start()) <= token.start && token.end <= usize::from(range.end()))).collect();
    let compare = texts(&tokens).0;
    if tokens.last().is_some_and(|token| !token.in_tree && token.text == ";") {
        tokens.pop();
    }
    (compare, texts(&tokens).1)
}

fn source_lines(source: &str, start: usize, end: usize) -> String {
    source.lines().skip(start.saturating_sub(1)).take(end + 1 - start.max(1)).collect::<Vec<_>>().join("\n")
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
    fn follows_each_file_of_a_module_under_a_condition() {
        // Design 37.3: `cfg_attr(COND, path = "…")` names one file for each condition. Semilla
        // reads each file. Two files or more give twin modules.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n");
        write(root, "src/lib.rs", "#[cfg_attr(unix, path = \"sys/unix.rs\")]\n#[cfg_attr(windows, path = \"sys/windows.rs\")]\nmod sys;\n#[cfg_attr(feature = \"x\", path = \"one.rs\")]\nmod single;\n#[cfg(windows)]\nmod bad;\n");
        write(root, "src/sys/unix.rs", "pub fn open() {}\n");
        write(root, "src/sys/windows.rs", "pub fn open() {}\npub fn close() {}\n");
        write(root, "src/one.rs", "pub fn one() {}\n");
        write(root, "src/single.rs", "pub fn usual() {}\n");
        write(root, "src/bad.rs", "pub fn bad( {}\n");
        let ws = Workspace::load(root).unwrap();
        let keys = || ws.items.keys().collect::<Vec<_>>();
        assert_eq!(ws.items["demo::sys#0::open"].file, "src/sys/unix.rs", "{:?}", keys());
        assert_eq!(ws.items["demo::sys#1::open"].file, "src/sys/windows.rs", "{:?}", keys());
        assert!(ws.items.contains_key("demo::sys#1::close"), "{:?}", keys());
        // The file of the attribute and the usual file are the two files of `single`.
        assert_eq!(ws.items["demo::single#0::one"].file, "src/one.rs", "{:?}", keys());
        assert_eq!(ws.items["demo::single#1::usual"].file, "src/single.rs", "{:?}", keys());
        // A file that does not parse does not stop the load. The error is in the record.
        assert!(!ws.parsed_files.contains("src/bad.rs") && ws.parse_errors.contains_key("src/bad.rs"), "{:?}", ws.parse_errors);
    }

    #[test]
    fn parses_a_trait_method_of_the_2015_edition() {
        // Design 39.1: the parser gets the edition of the package. A parameter with no name is
        // correct only in the 2015 edition; the tree keeps the text as it is.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let text = "pub trait T {\n    fn f(u8);\n    fn g(&self, &mut u8) -> bool;\n}\n";
        write(root, "Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n");
        write(root, "src/lib.rs", text);
        let ws = Workspace::load(root).unwrap();
        assert!(ws.parse_errors.is_empty(), "{:?}", ws.parse_errors);
        assert_eq!(ws.items["demo::T::g"].signature, "fn g (& self , & mut u8) -> bool");
        write(root, "Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2018\"\n");
        let ws = Workspace::load(root).unwrap();
        assert!(ws.parse_errors["src/lib.rs"].contains("(line 2, column "), "{:?}", ws.parse_errors);
        // The edition of the workspace (`edition.workspace = true`).
        write(root, "Cargo.toml", "[workspace.package]\nedition = \"2018\"\n[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition.workspace = true\n");
        let ws = Workspace::load(root).unwrap();
        assert!(ws.parse_errors.contains_key("src/lib.rs"), "{:?}", ws.parse_errors);
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
        assert!(ws.items["app (bin \"app\")::tests::t"].test);
        assert!(!ws.items["app (bin \"app\")::main"].test);
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
        assert_eq!(ws.items["tool (bin \"license\")::helper"].file, "LICENSE.txt");
        assert!(ws.parsed_files.contains("LICENSE.txt") && ws.parsed_files.contains("code/main.txt"));
        // The build script, its module, and its include are code that the loader does not parse.
        for file in ["NOTES.md", "gen_code.rs", "table.inc"] {
            assert!(ws.code_files[file].is_build_script(), "{file}");
        }
        // Batch 5n, A4: an example and a bench are parsed as a binary and a test. Each target
        // is the root of its own item tree (design 56.1), also a target with the name of the
        // package.
        assert_eq!(ws.items["tool (example \"demo\")::main"].file, "examples/demo.rs");
        assert_eq!(ws.items["tool (bench \"tool\")::DATA"].file, "benches/tool.rs");
        assert!(ws.items["tool (bench \"tool\")::main"].test && !ws.items["tool (example \"demo\")::main"].test);
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
        assert_eq!(ws.items["a (bin \"the-tool\")::main"].file, "crates/a/LICENSE.txt");
        assert!(ws.items["a (test \"it\")::works"].test);
        assert!(ws.items.contains_key("a::lib_fn"));
        assert!(ws.code_files["crates/a/NOTES.md"].is_build_script());
        assert_eq!(ws.code_files["crates/a/NOTES.md"].role(), "the build script of `a`");
    }

    #[test]
    fn the_comments_come_from_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n");
        write(root, "src/lib.rs", "/// doc\npub fn f() {\n    let a = \"// not\"; // yes\n    /* block\n       two */\n}\n");
        let ws = Workspace::load(root).unwrap();
        assert_eq!(ws.items["demo::f"].docs, ["/// doc", "// yes", "/* block two */"]);
        // A plain comment above an item is not in the item: the item starts at its doc comment.
        write(root, "src/lib.rs", "// a note\n/// doc\npub fn f() {}\n");
        let ws = Workspace::load(root).unwrap();
        assert_eq!((ws.items["demo::f"].line_start, ws.items["demo::f"].col_start), (2, 0));
        assert_eq!(ws.items["demo::f"].text, "# [doc = \" doc\"] pub fn f () { }");
    }
}
