//! Loads a Cargo workspace into the entity tree: crates, modules, and items.

use std::collections::BTreeMap;
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
    Other,
}

/// One item of the workspace.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    /// The item path, for example `cart::Cart::total`. Methods in a trait impl get
    /// `cart::<Cart as Display>::fmt`. `use` items get a path with their position.
    pub path: String,
    pub kind: ItemKind,
    pub crate_name: String,
    /// The file, relative to the workspace root.
    pub file: String,
    pub line_start: usize,
    pub line_end: usize,
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
    /// All files that are not Rust files, by relative path, with their content.
    pub other_files: BTreeMap<String, String>,
    /// All Rust files, by relative path, with their content.
    pub rust_files: BTreeMap<String, String>,
}

impl Workspace {
    /// Loads the workspace at `root`.
    pub fn load(root: &Path) -> Result<Workspace> {
        let mut workspace = Workspace::default();
        let packages = find_packages(root)?;
        let names: Vec<String> = packages.iter().map(|(_, name, _)| name.clone()).collect();
        for (dir, name, manifest) in &packages {
            let deps = manifest
                .get("dependencies")
                .and_then(|deps| deps.as_table())
                .map(|deps| {
                    deps.keys().map(|key| key.replace('-', "_")).filter(|key| names.contains(key)).collect()
                })
                .unwrap_or_default();
            let rel_dir = relative(root, dir);
            workspace.crates.push(CrateInfo { name: name.clone(), dir: rel_dir, deps });
            for (file, test) in crate_roots(dir, manifest) {
                let mut loader = Loader { root, workspace: &mut workspace, crate_name: name };
                loader.load_file(&file, name, test)?;
            }
        }
        collect_files(root, root, &mut workspace)?;
        Ok(workspace)
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

/// The root files of a package: `src/lib.rs`, `src/main.rs`, and the test targets (test code).
///
/// Cargo finds `tests/*.rs` automatically, unless `autotests = false`. Then only the `[[test]]`
/// targets of the manifest are tests, and the other files in `tests/` do not compile.
fn crate_roots(dir: &Path, manifest: &toml::Table) -> Vec<(PathBuf, bool)> {
    let mut roots = Vec::new();
    for name in ["src/lib.rs", "src/main.rs"] {
        let path = dir.join(name);
        if path.exists() {
            roots.push((path, false));
        }
    }
    let autotests = manifest.get("package").and_then(|p| p.get("autotests")).and_then(|v| v.as_bool()).unwrap_or(true);
    let listed: Vec<PathBuf> = manifest
        .get("test")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter_map(|target| {
            let path = target.get("path").and_then(|p| p.as_str()).map(str::to_string);
            let name = target.get("name").and_then(|n| n.as_str()).map(|n| format!("tests/{n}.rs"));
            path.or(name).map(|p| dir.join(p))
        })
        .filter(|path| path.exists())
        .collect();
    if !autotests {
        roots.extend(listed.into_iter().map(|path| (path, true)));
        return roots;
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("tests")) {
        let mut tests: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "rs")).collect();
        tests.sort();
        roots.extend(tests.into_iter().map(|path| (path, true)));
    }
    roots
}

fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
    if rel.is_empty() { ".".to_string() } else { rel }
}

/// Collects all files of the workspace, except `target`, `.git`, and `.semilla`.
fn collect_files(root: &Path, dir: &Path, workspace: &mut Workspace) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with(".semilla") {
            continue;
        }
        if path.is_dir() {
            if !matches!(name.as_ref(), "target" | ".git" | ".semilla") {
                collect_files(root, &path, workspace)?;
            }
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            let rel = relative(root, &path);
            if rel.ends_with(".rs") {
                workspace.rust_files.insert(rel, text);
            } else {
                workspace.other_files.insert(rel, text);
            }
        }
    }
    Ok(())
}

struct Loader<'a> {
    root: &'a Path,
    workspace: &'a mut Workspace,
    crate_name: &'a str,
}

impl Loader<'_> {
    fn load_file(&mut self, file: &Path, module: &str, test: bool) -> Result<()> {
        let source = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let parsed = syn::parse_file(&source).with_context(|| format!("parse {}", file.display()))?;
        let rel = relative(self.root, file);
        // Files in `tests/` are their own crates in Cargo; here they are modules of the package.
        let module = if test && file.parent().is_some_and(|p| p.ends_with("tests")) {
            format!("{module}::{}", file.file_stem().unwrap().to_string_lossy())
        } else {
            module.to_string()
        };
        let child_dir = module_dir(file);
        self.load_items(&parsed.items, &module, &rel, &source, &child_dir, test)
    }

    fn load_items(&mut self, items: &[syn::Item], module: &str, file: &str, source: &str, dir: &Path, test: bool) -> Result<()> {
        let mut use_index = 0;
        for item in items {
            match item {
                syn::Item::Mod(item_mod) => {
                    let name = item_mod.ident.to_string();
                    let path = format!("{module}::{name}");
                    let is_test = test || is_cfg_test(&item_mod.attrs);
                    if let Some((_, content)) = &item_mod.content {
                        self.load_items(content, &path, file, source, &dir.join(&name), is_test)?;
                    } else {
                        let candidates = [dir.join(format!("{name}.rs")), dir.join(&name).join("mod.rs")];
                        if let Some(found) = candidates.iter().find(|candidate| candidate.exists()) {
                            let text = std::fs::read_to_string(found)?;
                            let parsed = syn::parse_file(&text).with_context(|| format!("parse {}", found.display()))?;
                            let rel = relative(self.root, found);
                            self.load_items(&parsed.items, &path, &rel, &text, &module_dir(found), is_test)?;
                        }
                    }
                    self.add(path, ItemKind::Mod, item, &item_mod.vis, &item_mod.attrs, None, Vec::new(), file, source, is_test);
                }
                syn::Item::Fn(item_fn) => {
                    let path = format!("{module}::{}", item_fn.sig.ident);
                    let body = tree::block("body", &item_fn.block);
                    let is_test = test || has_attr(&item_fn.attrs, "test");
                    self.add(path, ItemKind::Fn, item, &item_fn.vis, &item_fn.attrs, Some(body), Vec::new(), file, source, is_test);
                }
                syn::Item::Impl(item_impl) => {
                    let self_ty = tokens(&item_impl.self_ty).replace(' ', "");
                    let owner = match &item_impl.trait_ {
                        Some((_, trait_path, _)) => format!("<{self_ty} as {}>", tokens(trait_path).replace(' ', "")),
                        None => self_ty.clone(),
                    };
                    for impl_item in &item_impl.items {
                        if let syn::ImplItem::Fn(method) = impl_item {
                            let path = format!("{module}::{owner}::{}", method.sig.ident);
                            let body = tree::block("body", &method.block);
                            let is_test = test || has_attr(&method.attrs, "test");
                            self.add_parts(path, ItemKind::Fn, impl_item, &tokens(&method.vis), &method.attrs, Some(body), Vec::new(), file, source, is_test, signature_of_method(method));
                        }
                    }
                    let path = format!("{module}::impl {owner}");
                    let members = item_impl.items.iter().map(|it| impl_member_name(it)).collect();
                    self.add(path, ItemKind::Impl, item, &syn::Visibility::Inherited, &item_impl.attrs, None, members, file, source, test);
                }
                syn::Item::Struct(item_struct) => {
                    let members = item_struct.fields.iter().enumerate().map(|(index, field)| field_text(index, field)).collect();
                    let path = format!("{module}::{}", item_struct.ident);
                    self.add(path, ItemKind::Struct, item, &item_struct.vis, &item_struct.attrs, None, members, file, source, test);
                }
                syn::Item::Enum(item_enum) => {
                    let members = item_enum.variants.iter().map(|variant| tokens(variant)).collect();
                    let path = format!("{module}::{}", item_enum.ident);
                    self.add(path, ItemKind::Enum, item, &item_enum.vis, &item_enum.attrs, None, members, file, source, test);
                }
                syn::Item::Trait(item_trait) => {
                    let members = item_trait.items.iter().map(tokens).collect();
                    let path = format!("{module}::{}", item_trait.ident);
                    self.add(path, ItemKind::Trait, item, &item_trait.vis, &item_trait.attrs, None, members, file, source, test);
                }
                syn::Item::Const(item_const) => {
                    let path = format!("{module}::{}", item_const.ident);
                    self.add(path, ItemKind::Const, item, &item_const.vis, &item_const.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Static(item_static) => {
                    let path = format!("{module}::{}", item_static.ident);
                    self.add(path, ItemKind::Static, item, &item_static.vis, &item_static.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Type(item_type) => {
                    let path = format!("{module}::{}", item_type.ident);
                    self.add(path, ItemKind::Type, item, &item_type.vis, &item_type.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Use(item_use) => {
                    let path = format!("{module}::use#{use_index}");
                    use_index += 1;
                    self.add(path, ItemKind::Use, item, &item_use.vis, &item_use.attrs, None, Vec::new(), file, source, test);
                }
                syn::Item::Macro(item_macro) => {
                    let name = item_macro.ident.as_ref().map(|ident| ident.to_string()).unwrap_or_else(|| tokens(&item_macro.mac.path));
                    let path = format!("{module}::{name}!");
                    self.add(path, ItemKind::Macro, item, &syn::Visibility::Inherited, &item_macro.attrs, None, Vec::new(), file, source, test);
                }
                other => {
                    let path = format!("{module}::item@{}", other.span().start().line);
                    self.add(path, ItemKind::Other, item, &syn::Visibility::Inherited, &[], None, Vec::new(), file, source, test);
                }
            }
        }
        Ok(())
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
        let text_source = source_lines(source, line_start, line_end);
        let mut docs: Vec<String> = attrs.iter().filter(|attr| attr.path().is_ident("doc")).map(tokens).collect();
        docs.extend(plain_comments(&text_source));
        let attrs = attrs.iter().filter(|attr| !attr.path().is_ident("doc")).map(tokens).collect();
        let item = Item {
            path: path.clone(),
            kind,
            crate_name: self.crate_name.to_string(),
            file: file.to_string(),
            line_start,
            line_end,
            vis: vis.to_string(),
            signature,
            docs,
            attrs,
            members,
            body,
            text: tokens(value),
            source: text_source,
            test,
        };
        self.workspace.items.insert(path, item);
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

fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| attr.path().segments.last().is_some_and(|segment| segment.ident == name))
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("cfg") && tokens(attr).contains("test"))
}

fn field_text(index: usize, field: &syn::Field) -> String {
    let name = field.ident.as_ref().map(|ident| ident.to_string()).unwrap_or_else(|| index.to_string());
    format!("{} {name}: {}", tokens(&field.vis), tokens(&field.ty)).trim().to_string()
}

fn impl_member_name(item: &syn::ImplItem) -> String {
    match item {
        syn::ImplItem::Fn(method) => format!("fn {}", method.sig.ident),
        syn::ImplItem::Const(constant) => format!("const {}", constant.ident),
        syn::ImplItem::Type(ty) => format!("type {}", ty.ident),
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
        assert_eq!(ws.items["core_a::cart::Cart"].members, ["pub items: Vec < u32 >"]);
        assert!(ws.other_files.contains_key("crates/app/Cargo.toml"));
    }

    #[test]
    fn comment_scanner_skips_strings() {
        let comments = plain_comments("let a = \"// not\"; // yes\n/* block */ /// doc");
        assert_eq!(comments, ["// yes", "/* block */"]);
    }
}
