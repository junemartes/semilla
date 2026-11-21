//! The sites of a rename (design section 51).
//!
//! The map of a rename is a map of the references to the item. A token with the old name is in
//! one of four classes:
//!
//! 1. never a site: a literal, a comment, a lifetime, the declaration of a different thing;
//! 2. a sure site: the declaration of the item, or a path that the syntax tree resolves to it;
//! 3. a sure other thing: a path that resolves to a different item, to a local, or to an
//!    external crate;
//! 4. not sure (by name): a member after `.`, a path that the tree cannot resolve, a token in
//!    the input of a macro that the syntax phase cannot read.
//!
//! The map changes a sure site. It changes a by-name site only if the name is free (rule R2).
//! Each other token stays, so the compare gives its difference a fact.

use std::collections::{BTreeMap, BTreeSet};

use semilla_syntax::sites::{Place, PlaceKind, ROOT, places};
use semilla_syntax::{Item, ItemKind, Renames, SiteCount, Workspace};

use crate::identity::{module_of, owner, target_in, use_names};

/// One renamed item with a new last name.
struct Rule {
    old_path: String,
    new_path: String,
    new: String,
    kind: ItemKind,
    /// A function of an impl or of a trait.
    method: bool,
}

/// The kind of a name that the tree cannot resolve (rule R2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NameKind {
    /// The name of a method call (`x.name(…)`).
    Method,
    /// The name of a field (`x.name`, or a label of a struct literal).
    Field,
    /// A name in a path: a value or a type.
    Path,
    /// The name of a macro call.
    Macro,
    /// A token in a macro input: each kind of name.
    Any,
}

enum Class {
    Never,
    /// The index of the rule in the rules of the name.
    Sure(usize),
    Other,
    ByName(NameKind),
}

#[derive(Clone, PartialEq, Eq)]
enum Lookup {
    Item(String),
    External,
    Unknown,
    /// Two places of one name in one item give different things (design 52.3). The site
    /// table has no place of a token, so no place of the name is a site.
    Mixed,
}

/// A `use` line in a block of the body of an item (design 52.3).
struct BlockUse {
    /// The offsets of the block in the compare text of the body.
    open: usize,
    close: usize,
    /// Each name that the line gives, with its path. The path of a line with a leading `::`
    /// starts with [`ROOT`].
    names: Vec<(String, Vec<String>)>,
    /// The path of each glob import of the line.
    globs: Vec<Vec<String>>,
    /// False for a line that the parser cannot read.
    clear: bool,
}

fn block_uses(item: &Item) -> Vec<BlockUse> {
    let Some(body) = &item.body else { return Vec::new() };
    let lines = semilla_syntax::sites::block_uses(&body.text).into_iter();
    lines
        .map(|line| {
            let mut out = BlockUse { open: line.open, close: line.close, names: Vec::new(), globs: Vec::new(), clear: false };
            let Some(parsed) = semilla_syntax::rs::parse_use(&line.text) else { return out };
            let root: Vec<String> = parsed.leading_colon.then(|| ROOT.to_string()).into_iter().collect();
            use_names(&parsed.tree, &root, &mut out.names);
            glob_prefixes(&parsed.tree, &mut root.clone(), &mut out.globs);
            out.clear = true;
            out
        })
        .collect()
}

/// The last name of an item path, with no twin number and no `!`.
fn name_of(path: &str) -> &str {
    let last = crate::checks::last_segment(path);
    let last = last.split('#').next().unwrap_or(last);
    last.strip_suffix('!').unwrap_or(last)
}

fn without_twin(path: &str) -> &str {
    crate::identity::twin_base(path).unwrap_or(path)
}

fn is_method(path: &str, workspace: &Workspace) -> bool {
    workspace.items.get(path).is_some_and(|item| item.kind == ItemKind::Fn) && module_of(path, workspace) != Some(owner(path))
}

/// True if a declaration of the kind `kind` can have the name of a by-name place.
fn of_kind(kind: NameKind, item: &Item, workspace: &Workspace) -> bool {
    match kind {
        NameKind::Method => is_method(&item.path, workspace),
        NameKind::Field => item.kind == ItemKind::Field,
        NameKind::Path => !matches!(item.kind, ItemKind::Field | ItemKind::Macro | ItemKind::Use | ItemKind::Impl),
        NameKind::Macro => item.kind == ItemKind::Macro,
        NameKind::Any => !matches!(item.kind, ItemKind::Use | ItemKind::Impl),
    }
}

/// The compare texts of an item that the diff maps: the signature, the body, and the members
/// of an impl or a trait (`true` for a member).
fn texts(item: &Item) -> Vec<(&str, bool)> {
    let mut out = vec![(item.signature.as_str(), false)];
    if let Some(body) = &item.body {
        out.push((body.text.as_str(), false));
    }
    if matches!(item.kind, ItemKind::Impl | ItemKind::Trait) {
        out.extend(item.members.iter().map(|member| (member.as_str(), true)));
    }
    out
}

/// The names that an item binds: a local, a parameter, a generic parameter, an inner item.
/// `patterns` has the names that only an identifier pattern binds. `marked` has the names of
/// the identifier patterns with `ref`, `mut`, or `@`.
#[derive(Default)]
struct Bound {
    names: BTreeSet<String>,
    patterns: BTreeSet<String>,
    marked: BTreeSet<String>,
}

fn bound(item: &Item) -> Bound {
    use semilla_syntax::rs::{AstNode, ast};
    use ast::HasName;
    let mut out = Bound::default();
    let Some(parsed) = semilla_syntax::rs::parse_item(&item.source) else { return out };
    let root = parsed.syntax().clone();
    let mut other = BTreeSet::new();
    for node in root.descendants() {
        if let Some(pat) = ast::IdentPat::cast(node.clone()) {
            let name = pat.name().map(|name| name.text().to_string());
            if pat.ref_token().is_some() || pat.mut_token().is_some() || pat.at_token().is_some() {
                out.marked.extend(name.clone());
            }
            out.patterns.extend(name);
        } else if let Some(param) = ast::TypeParam::cast(node.clone()) {
            other.extend(param.name().map(|name| name.text().to_string()));
        } else if let Some(param) = ast::ConstParam::cast(node.clone()) {
            other.extend(param.name().map(|name| name.text().to_string()));
        } else if node != root
            && item.kind == ItemKind::Fn
            && let Some(inner) = ast::Item::cast(node)
        {
            // An item in a body (`fn`, `struct`, `const`, …) is a local declaration.
            let name = match &inner {
                ast::Item::Fn(it) => it.name(),
                ast::Item::Struct(it) => it.name(),
                ast::Item::Enum(it) => it.name(),
                ast::Item::Const(it) => it.name(),
                ast::Item::Static(it) => it.name(),
                ast::Item::TypeAlias(it) => it.name(),
                ast::Item::Trait(it) => it.name(),
                ast::Item::Module(it) => it.name(),
                ast::Item::Union(it) => it.name(),
                _ => None,
            };
            other.extend(name.map(|name| name.text().to_string()));
        }
    }
    out.patterns.retain(|name| !other.contains(name));
    out.names = other;
    out.names.extend(out.patterns.iter().cloned());
    out
}

/// The scope of one item of the "before" tree.
struct Scope<'a> {
    workspace: &'a Workspace,
    item: &'a Item,
    module: Option<&'a str>,
    bound: Option<Bound>,
    /// The `use` lines in the blocks of the body.
    blocks: Option<Vec<BlockUse>>,
}

impl<'a> Scope<'a> {
    fn new(item: &'a Item, workspace: &'a Workspace) -> Self {
        let module = if item.kind == ItemKind::Use { Some(owner(&item.path)) } else { module_of(&item.path, workspace) };
        Scope { workspace, item, module, bound: None, blocks: None }
    }

    fn bound(&mut self) -> &Bound {
        let item = self.item;
        self.bound.get_or_insert_with(|| bound(item))
    }

    fn is_module(&self, path: &str) -> bool {
        self.workspace.items.get(path).is_some_and(|item| item.kind == ItemKind::Mod) || self.workspace.crates.iter().any(|krate| krate.name == path)
    }

    /// The self type of the impl of the item: the item of the type, if the tree gives one.
    fn self_type(&mut self) -> Option<String> {
        let module = self.module?;
        let path = without_twin(&self.item.path);
        let tail = path.strip_prefix(module)?.strip_prefix("::")?;
        let block = match self.item.kind {
            ItemKind::Impl => tail.strip_prefix("impl ")?,
            // A method: `Type::name`, or `<Type as Trait>::name`.
            ItemKind::Fn => {
                let block = owner(tail);
                if block.is_empty() || self.workspace.items.get(&format!("{module}::{block}")).is_some_and(|item| item.kind == ItemKind::Trait) {
                    return None;
                }
                block
            }
            _ => return None,
        };
        let block = without_twin(block);
        let name = match block.strip_prefix('<') {
            Some(rest) => rest.split_once(" as ")?.0,
            None => block,
        };
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':') {
            return None;
        }
        let segments: Vec<&str> = name.split("::").collect();
        match self.resolve(&segments) {
            Lookup::Item(path) if self.workspace.items.get(&path).is_some_and(|item| matches!(item.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Other)) => Some(path),
            _ => None,
        }
    }

    /// The item that the name `name` gives in the module `module`: an item of the module, a
    /// `use` line with a full path, or a glob import of a module or an enum of the workspace.
    fn in_module(&self, module: &str, name: &str, depth: usize) -> Lookup {
        let workspace = self.workspace;
        if let Some(target) = target_in(module, name, workspace, 8) {
            return Lookup::Item(target);
        }
        if depth == 0 {
            return Lookup::Unknown;
        }
        // Twins of the name (two `cfg` forms) give no single item.
        let twin = format!("{module}::{name}#");
        if workspace.items.range(twin.clone()..).next().is_some_and(|(path, _)| path.starts_with(&twin)) {
            return Lookup::Unknown;
        }
        let mut found: BTreeSet<String> = BTreeSet::new();
        for import in workspace.items.values().filter(|item| item.kind == ItemKind::Use && owner(&item.path) == module) {
            let Some(parsed) = semilla_syntax::rs::parse_use(&import.source) else { return Lookup::Unknown };
            // A `use` line with the name and no clear target is stronger than a glob import.
            let mut names = Vec::new();
            use_names(&parsed.tree, &[], &mut names);
            if names.iter().any(|(local, _)| local == name) {
                return Lookup::Unknown;
            }
            let mut globs = Vec::new();
            glob_prefixes(&parsed.tree, &mut Vec::new(), &mut globs);
            for prefix in globs.iter().filter(|_| !parsed.leading_colon) {
                let segments: Vec<&str> = prefix.iter().map(String::as_str).collect();
                let Lookup::Item(source) = self.resolve_from(module, &segments, depth - 1) else { continue };
                let reads = self.is_module(&source) || workspace.items.get(&source).is_some_and(|item| item.kind == ItemKind::Enum);
                if !reads {
                    continue;
                }
                let target = match workspace.items.get(&format!("{source}::{name}")) {
                    Some(item) if !matches!(item.kind, ItemKind::Use | ItemKind::Impl) => Lookup::Item(item.path.clone()),
                    _ if self.is_module(&source) => self.in_module(&source, name, depth - 1),
                    _ => Lookup::Unknown,
                };
                if let Lookup::Item(target) = target {
                    // A glob import gives a private item only to the module and its children.
                    let open = workspace.items.get(&target).is_some_and(|item| item.vis.starts_with("pub") || item.kind == ItemKind::Variant);
                    if open || module == source || module.starts_with(&format!("{source}::")) {
                        found.insert(target);
                    }
                }
            }
        }
        match found.len() {
            1 => Lookup::Item(found.pop_first().unwrap()),
            _ => Lookup::Unknown,
        }
    }

    /// The thing that the first name of a path gives at its places in the body (design
    /// 52.3). The scope of a place is the chain of its blocks, then the module. `None`: no
    /// `use` line of a block gives the name at a place, and the scope of the module decides.
    fn in_blocks(&mut self, name: &str) -> Option<Lookup> {
        let item = self.item;
        if self.blocks.get_or_insert_with(|| block_uses(item)).is_empty() {
            return None;
        }
        let body = item.body.as_ref()?;
        let mut found: Option<Option<Lookup>> = None;
        for place in places(&body.text, &|word| word == name) {
            let first = matches!(&place.kind, PlaceKind::Path { qualifier: Some(qualifier), .. } if qualifier.is_empty());
            if place.opaque || !first {
                continue;
            }
            let here = self.at_place(name, place.start);
            match &found {
                None => found = Some(here),
                Some(other) if *other == here => {}
                Some(_) => return Some(Lookup::Mixed),
            }
        }
        found.flatten()
    }

    /// The thing that the `use` lines of the blocks give for a name at the offset `at` of the
    /// body. An inner block comes before an outer block. In one block, a line with the name
    /// comes before a glob import.
    fn at_place(&self, name: &str, at: usize) -> Option<Lookup> {
        let (module, blocks) = (self.module?, self.blocks.as_ref()?);
        let mut opens: Vec<usize> = blocks.iter().filter(|line| line.open < at && at < line.close).map(|line| line.open).collect();
        opens.sort_unstable_by(|a, b| b.cmp(a));
        opens.dedup();
        for open in opens {
            let lines = || blocks.iter().filter(move |line| line.open == open);
            let mut named = lines().flat_map(|line| &line.names).filter(|(local, _)| local == name);
            if let Some((_, path)) = named.next() {
                // Two lines with the name (two name spaces, or a `cfg`): no clear target.
                return Some(if named.next().is_some() { Lookup::Unknown } else { self.use_path(module, path) });
            }
            if lines().any(|line| !line.clear) {
                return Some(Lookup::Unknown);
            }
            let mut found = BTreeSet::new();
            for prefix in lines().flat_map(|line| &line.globs) {
                // A source that the tool cannot list (an external crate, a path that is not
                // sure): each bare name of the block is by name.
                let Lookup::Item(source) = self.use_path(module, prefix) else { return Some(Lookup::Unknown) };
                match self.from_glob(module, &source, name, 4) {
                    Some(Lookup::Item(target)) => {
                        found.insert(target);
                    }
                    Some(other) => return Some(other),
                    None => {}
                }
            }
            match found.len() {
                0 => {}
                1 => return found.pop_first().map(Lookup::Item),
                _ => return Some(Lookup::Unknown),
            }
        }
        None
    }

    /// The item of the path of a `use` line of a block. A first segment that is a name comes
    /// from the module. It is not sure if a `use` line of a block can give this name.
    fn use_path(&self, module: &str, path: &[String]) -> Lookup {
        let segments: Vec<&str> = path.iter().map(String::as_str).collect();
        let Some(first) = segments.first().copied() else { return Lookup::Unknown };
        if !matches!(first, "crate" | "self" | "super") && first != ROOT {
            let blocks = self.blocks.as_deref().unwrap_or_default();
            let named = blocks.iter().flat_map(|line| &line.names).any(|(local, _)| local == first);
            let glob = blocks.iter().flat_map(|line| &line.globs).any(|prefix| prefix != path);
            if named || glob || blocks.iter().any(|line| !line.clear) {
                return Lookup::Unknown;
            }
        }
        self.resolve_from(module, &segments, 4)
    }

    /// The thing that a glob import of `source` gives to the module `module` for a name
    /// (design 52.3). `None`: `source` is a module or an enum of the workspace, and the list of
    /// its names does not have the name. `Unknown`: the tool cannot list the names of
    /// `source` (a macro call can make an item, a glob import of an external crate).
    fn from_glob(&self, module: &str, source: &str, name: &str, depth: usize) -> Option<Lookup> {
        let workspace = self.workspace;
        let direct = format!("{source}::{name}");
        if workspace.items.get(source).is_some_and(|item| item.kind == ItemKind::Enum) {
            return workspace.items.get(&direct).is_some_and(|item| item.kind == ItemKind::Variant).then_some(Lookup::Item(direct));
        }
        if !self.is_module(source) || depth == 0 {
            return Some(Lookup::Unknown);
        }
        // A glob import gives a private item only to the module and its children.
        let inside = module == source || module.starts_with(&format!("{source}::"));
        if let Some(item) = workspace.items.get(&direct).filter(|item| !matches!(item.kind, ItemKind::Use | ItemKind::Impl)) {
            return (inside || item.vis.starts_with("pub")).then_some(Lookup::Item(direct));
        }
        // Twins of the name (two `cfg` forms) give no single item.
        let twin = format!("{direct}#");
        if workspace.items.range(twin.clone()..).next().is_some_and(|(path, _)| path.starts_with(&twin)) {
            return Some(Lookup::Unknown);
        }
        let mut found = BTreeSet::new();
        for item in workspace.items.values().filter(|item| owner(&item.path) == source) {
            // A macro call in the module can make an item with the name.
            if name_of_call(item).is_some() {
                return Some(Lookup::Unknown);
            }
            if item.kind != ItemKind::Use || !(inside || item.vis.starts_with("pub")) {
                continue;
            }
            let Some(parsed) = semilla_syntax::rs::parse_use(&item.source) else { return Some(Lookup::Unknown) };
            let mut names = Vec::new();
            use_names(&parsed.tree, &[], &mut names);
            if names.iter().any(|(local, _)| local == name) {
                return Some(target_in(source, name, workspace, 8).map_or(Lookup::Unknown, Lookup::Item));
            }
            let mut globs = Vec::new();
            glob_prefixes(&parsed.tree, &mut Vec::new(), &mut globs);
            for prefix in globs {
                let segments: Vec<&str> = prefix.iter().map(String::as_str).collect();
                let next = if parsed.leading_colon { Lookup::Unknown } else { self.resolve_from(source, &segments, depth - 1) };
                let Lookup::Item(next) = next else { return Some(Lookup::Unknown) };
                match self.from_glob(module, &next, name, depth - 1) {
                    Some(Lookup::Item(target)) => {
                        found.insert(target);
                    }
                    Some(other) => return Some(other),
                    None => {}
                }
            }
        }
        match found.len() {
            0 => None,
            1 => found.pop_first().map(Lookup::Item),
            _ => Some(Lookup::Unknown),
        }
    }

    /// The item of a path from the place of the path in the item: the `use` lines of the
    /// blocks of the body, then the module of the item.
    fn resolve(&mut self, segments: &[&str]) -> Lookup {
        let Some(module) = self.module else { return Lookup::Unknown };
        let Some(first) = segments.first().copied() else { return Lookup::Unknown };
        if first == "Self" {
            let Some(mut current) = self.self_type() else { return Lookup::Unknown };
            for segment in &segments[1..] {
                match self.step(&current, segment, 4) {
                    Lookup::Item(next) => current = next,
                    other => return other,
                }
            }
            return Lookup::Item(current);
        }
        // A local or a generic parameter (`T::name`): the tree does not give its type.
        if !matches!(first, "crate" | "self" | "super") && first != ROOT {
            if self.bound().names.contains(first) {
                return Lookup::Unknown;
            }
            match self.in_blocks(first) {
                Some(Lookup::Item(mut current)) => {
                    for segment in &segments[1..] {
                        match self.step(&current, segment, 4) {
                            Lookup::Item(next) => current = next,
                            other => return other,
                        }
                    }
                    return Lookup::Item(current);
                }
                Some(other) => return other,
                None => {}
            }
        }
        self.resolve_from(module, segments, 4)
    }

    fn resolve_from(&self, module: &str, segments: &[&str], depth: usize) -> Lookup {
        let workspace = self.workspace;
        let Some((first, rest)) = segments.split_first() else { return Lookup::Unknown };
        let is_crate = |name: &str| workspace.crates.iter().any(|krate| krate.name == name);
        let (mut current, rest): (String, &[&str]) = match *first {
            "crate" => (module.split("::").next().unwrap_or(module).to_string(), rest),
            "self" => (module.to_string(), rest),
            "super" => (owner(module).to_string(), rest),
            "Self" => return Lookup::Unknown,
            ROOT => match rest.split_first() {
                Some((name, tail)) if is_crate(name) => (name.to_string(), tail),
                _ => return Lookup::External,
            },
            name => match self.in_module(module, name, depth) {
                Lookup::Item(path) => (path, rest),
                _ if is_crate(name) => (name.to_string(), rest),
                _ if matches!(name, "std" | "core" | "alloc") => return Lookup::External,
                _ => return Lookup::Unknown,
            },
        };
        if current.is_empty() {
            return Lookup::Unknown;
        }
        for segment in rest {
            match self.step(&current, segment, depth) {
                Lookup::Item(next) => current = next,
                other => return other,
            }
        }
        Lookup::Item(current)
    }

    /// The item `segment` of the item `current`.
    fn step(&self, current: &str, segment: &str, depth: usize) -> Lookup {
        if segment == "super" {
            let up = owner(current);
            return if self.is_module(current) && !up.is_empty() { Lookup::Item(up.to_string()) } else { Lookup::Unknown };
        }
        if self.workspace.items.get(current).is_some_and(|item| item.kind == ItemKind::Type) {
            return Lookup::Unknown;
        }
        match self.workspace.items.get(&format!("{current}::{segment}")) {
            Some(item) if !matches!(item.kind, ItemKind::Use | ItemKind::Impl) => Lookup::Item(item.path.clone()),
            _ if self.is_module(current) => self.in_module(current, segment, depth),
            _ => Lookup::Unknown,
        }
    }

    /// The class of a place for the rules of its name.
    fn classify(&mut self, place: &Place, rules: &[Rule]) -> Class {
        let name = place.name.as_str();
        let sure = |target: &str| match rules.iter().position(|rule| rule.old_path == target) {
            Some(index) => Class::Sure(index),
            None => Class::Other,
        };
        let fields = rules.iter().any(|rule| rule.kind == ItemKind::Field);
        let methods = rules.iter().any(|rule| rule.method);
        let macros = rules.iter().any(|rule| rule.kind == ItemKind::Macro);
        let paths = rules.iter().any(|rule| !matches!(rule.kind, ItemKind::Field | ItemKind::Macro));
        if place.opaque {
            return Class::ByName(NameKind::Any);
        }
        match &place.kind {
            PlaceKind::Alias => Class::Never,
            PlaceKind::Decl => {
                if let Some(index) = rules.iter().position(|rule| rule.old_path == self.item.path) {
                    return Class::Sure(index);
                }
                // A member of an impl or of a trait: the item `Owner::name`.
                if matches!(self.item.kind, ItemKind::Impl | ItemKind::Trait) {
                    let block = without_twin(&self.item.path);
                    let child = match block.rsplit_once("::impl ") {
                        Some((module, tail)) if self.item.kind == ItemKind::Impl => format!("{module}::{tail}::{name}"),
                        _ => format!("{block}::{name}"),
                    };
                    if let Some(index) = rules.iter().position(|rule| without_twin(&rule.old_path) == child) {
                        return Class::Sure(index);
                    }
                }
                Class::Never
            }
            PlaceKind::MacroName => {
                if macros {
                    Class::ByName(NameKind::Macro)
                } else {
                    Class::Never
                }
            }
            PlaceKind::Member { call, on_self } => {
                if !(if *call { methods } else { fields }) {
                    return Class::Never;
                }
                // `self.name` in an impl of a type of the workspace: the member of the type.
                if *on_self && let Some(owner) = self.self_type() {
                    let member = if *call { format!("{owner}::{name}") } else { format!("{owner}.{name}") };
                    if self.workspace.items.contains_key(&member) {
                        return sure(&member);
                    }
                }
                Class::ByName(if *call { NameKind::Method } else { NameKind::Field })
            }
            PlaceKind::Key { owner, .. } => {
                if !fields {
                    return Class::Never;
                }
                let Some(owner) = owner else { return Class::ByName(NameKind::Field) };
                let segments: Vec<&str> = owner.split("::").collect();
                match self.resolve(&segments) {
                    Lookup::Item(path) if self.workspace.items.get(&path).is_some_and(|item| item.kind == ItemKind::Type) => Class::ByName(NameKind::Field),
                    Lookup::Item(path) => sure(&format!("{path}.{name}")),
                    Lookup::External | Lookup::Mixed => Class::Other,
                    Lookup::Unknown => Class::ByName(NameKind::Field),
                }
            }
            PlaceKind::Path { qualifier: None, .. } => {
                if paths {
                    Class::ByName(NameKind::Path)
                } else {
                    Class::Never
                }
            }
            PlaceKind::Path { qualifier: Some(qualifier), .. } => {
                if !paths {
                    return Class::Never;
                }
                if qualifier.is_empty() {
                    // The header of a variant has the name of the variant.
                    if self.item.kind == ItemKind::Variant && name_of(&self.item.path) == name {
                        return match rules.iter().position(|rule| rule.old_path == self.item.path) {
                            Some(index) => Class::Sure(index),
                            None => Class::Never,
                        };
                    }
                    // A pattern with the name of an item that a pattern can name is a use of
                    // it (design 52.2). A pattern with each other name binds a local.
                    let constant = rules.iter().all(|rule| self.workspace.items.get(&rule.old_path).is_some_and(pattern_item));
                    let bound = self.bound();
                    if bound.names.contains(name) && !(constant && bound.patterns.contains(name)) {
                        return Class::Never;
                    }
                    let Some(module) = self.module else { return Class::ByName(NameKind::Path) };
                    return match self.in_blocks(name).unwrap_or_else(|| self.in_module(module, name, 4)) {
                        Lookup::Item(path) => sure(&path),
                        Lookup::External | Lookup::Mixed => Class::Other,
                        Lookup::Unknown => Class::ByName(NameKind::Path),
                    };
                }
                let mut segments: Vec<&str> = qualifier.split("::").collect();
                segments.push(name);
                match self.resolve(&segments) {
                    Lookup::Item(path) => sure(&path),
                    Lookup::External | Lookup::Mixed => Class::Other,
                    Lookup::Unknown => Class::ByName(NameKind::Path),
                }
            }
        }
    }
}

/// The thing that a bare name gives in the body of an item: `Some(true)` for the item
/// `target`, `Some(false)` for a different item, a local, or an external crate, and `None` if
/// the syntax tree cannot resolve the name.
pub(crate) fn bare_name(workspace: &Workspace, item: &str, name: &str, target: &str) -> Option<bool> {
    let item = workspace.items.get(item)?;
    let mut scope = Scope::new(item, workspace);
    if scope.bound().names.contains(name) {
        return Some(false);
    }
    let module = scope.module?;
    match scope.in_blocks(name).unwrap_or_else(|| scope.in_module(module, name, 4)) {
        Lookup::Item(path) => Some(path == target),
        Lookup::External | Lookup::Mixed => Some(false),
        Lookup::Unknown => None,
    }
}

/// True for an item that an identifier pattern can name: a constant, a static, a variant, a
/// unit struct, or a tuple struct (design 52.2).
fn pattern_item(item: &Item) -> bool {
    match item.kind {
        ItemKind::Const | ItemKind::Static | ItemKind::Variant => true,
        // A struct with named fields ends with `}`.
        ItemKind::Struct => item.source.trim_end().ends_with(';'),
        _ => false,
    }
}

/// True if an identifier pattern with the name `name` in the item is a sure binding (design
/// 52.2). Condition 1: the pattern has `ref`, `mut`, or `@`. Condition 2: the name is not the
/// name of an item that a pattern can name, and its first letter is not an upper-case letter.
/// An identifier pattern that is not a sure binding is a path: a use of an item.
pub(crate) fn sure_binding(workspace: &Workspace, item: &Item, name: &str) -> bool {
    let mut scope = Scope::new(item, workspace);
    let bound = scope.bound();
    if bound.marked.contains(name) {
        return true;
    }
    // An item in the body with the name comes before the module.
    if name.chars().next().is_some_and(char::is_uppercase) || (bound.names.contains(name) && !bound.patterns.contains(name)) {
        return false;
    }
    let lookup = match scope.module {
        Some(module) => scope.in_blocks(name).unwrap_or_else(|| scope.in_module(module, name, 4)),
        None => Lookup::Unknown,
    };
    match lookup {
        Lookup::Item(path) => !workspace.items.get(&path).is_some_and(pattern_item),
        // An item of a different crate with a lower-case first letter: not a constant by the
        // name rule.
        Lookup::External => true,
        Lookup::Mixed => false,
        // The resolution is not sure: no item of these kinds in the workspace has the name.
        Lookup::Unknown => !workspace.items.values().any(|other| pattern_item(other) && name_of(&other.path) == name),
    }
}

/// The item of a path whose first name comes from a `use` line in a block of the body of an
/// item (design 52.3). `None`: no such line gives the first name at a place of the body.
/// `Some(None)`: a line gives the name, and the syntax tree cannot resolve the path.
pub(crate) fn block_path(workspace: &Workspace, item: &str, segments: &[&str]) -> Option<Option<String>> {
    let item = workspace.items.get(item)?;
    let mut scope = Scope::new(item, workspace);
    let first = segments.first().copied()?;
    if matches!(first, "crate" | "self" | "super") || scope.bound().names.contains(first) {
        return None;
    }
    scope.in_blocks(first)?;
    Some(match scope.resolve(segments) {
        Lookup::Item(path) => Some(path),
        _ => None,
    })
}

/// The item that a path gives in an item of a tree. `None` if the syntax tree cannot resolve
/// the path to an item of the workspace.
pub(crate) fn path_item(workspace: &Workspace, item: &str, segments: &[&str]) -> Option<String> {
    let item = workspace.items.get(item)?;
    match Scope::new(item, workspace).resolve(segments) {
        Lookup::Item(path) => Some(path),
        _ => None,
    }
}

/// The name of a macro whose call in a module of the tree has the word `name` in its input.
/// The call can make an item with the name, and the syntax phase cannot list this item.
pub(crate) fn macro_call_with<'a>(workspace: &'a Workspace, name: &str) -> Option<&'a str> {
    workspace.items.values().find_map(|item| {
        let call = name_of_call(item)?;
        places(&item.source, &|word| word == name).iter().any(|place| place.name == name).then_some(call)
    })
}

/// The name of the macro of a macro call in a module. `None` for each other item.
fn name_of_call(item: &Item) -> Option<&str> {
    let (call, _) = item.path.rsplit("::").next()?.split_once('!')?;
    (!crate::owners::is_macro_definition(item)).then_some(call.trim())
}

/// Rule R2 for a name in a path: true if no other declaration of a value or a type has the
/// name of `old_path` or of `new_path`, in the tree before the step and in the tree after it.
pub(crate) fn path_name_free(before: &Workspace, after: &Workspace, old_path: &str, new_path: &str) -> bool {
    let (old, new) = (name_of(old_path), name_of(new_path));
    let taken = |workspace: &Workspace, own: &str| workspace.items.values().any(|item| item.path != own && of_kind(NameKind::Path, item, workspace) && [old, new].contains(&name_of(&item.path)));
    !taken(before, old_path) && !taken(after, new_path)
}

/// The path prefix of each glob import of a `use` tree.
fn glob_prefixes(tree: &semilla_syntax::rs::UseTree, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    use semilla_syntax::rs::UseTree;
    match tree {
        UseTree::Path(ident, rest) => {
            prefix.push(ident.clone());
            glob_prefixes(rest, prefix, out);
            prefix.pop();
        }
        UseTree::Glob => out.push(prefix.clone()),
        UseTree::Group(items) => items.iter().for_each(|tree| glob_prefixes(tree, prefix, out)),
        UseTree::Name(_) | UseTree::Rename(_, _) => {}
    }
}

/// The count of each key of the places of an item, for the words `wanted`.
fn key_counts(item: &Item, wanted: &dyn Fn(&str) -> bool) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for (text, _) in texts(item) {
        for place in places(text, wanted) {
            *out.entry(place.key()).or_insert(0) += 1;
        }
    }
    out
}

/// Makes the site table of a rename map (design section 51). Each rule is an entry of
/// `renames.items` with a new last name. The table has the sure sites, and the by-name sites
/// whose name is free.
pub fn resolve(renames: &mut Renames, before: &Workspace, after: &Workspace) {
    let mut rules: BTreeMap<String, Vec<Rule>> = BTreeMap::new();
    for (old_path, new_path) in &renames.items {
        let Some(item) = before.items.get(old_path) else { continue };
        let (old, new) = (name_of(old_path), name_of(new_path));
        if matches!(item.kind, ItemKind::Impl | ItemKind::Use) || old == new || old.is_empty() || new.is_empty() {
            continue;
        }
        rules.entry(old.to_string()).or_default().push(Rule { old_path: old_path.clone(), new_path: new_path.clone(), new: new.to_string(), kind: item.kind, method: is_method(old_path, before) });
    }
    let mut sites: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut counts: BTreeMap<String, SiteCount> = BTreeMap::new();
    if rules.is_empty() {
        renames.sites = Some(sites);
        return;
    }
    let new_names: BTreeSet<&str> = rules.values().flatten().map(|rule| rule.new.as_str()).collect();
    // The declarations of each tree with an old name or a new name (rule R2).
    let declared = |workspace: &'_ Workspace| -> Vec<(String, String)> {
        workspace.items.values().filter(|item| !matches!(item.kind, ItemKind::Use | ItemKind::Impl)).map(|item| (name_of(&item.path).to_string(), item.path.clone())).filter(|(name, _)| rules.contains_key(name) || new_names.contains(name.as_str())).collect()
    };
    let (declared_before, declared_after) = (declared(before), declared(after));
    // True if no other declaration of the kind has the old name or the new name.
    let free = |kind: NameKind, old: &str, rules: &[&Rule]| -> bool {
        let Some(new) = rules.first().map(|rule| rule.new.as_str()) else { return false };
        if rules.iter().any(|rule| rule.new != new) {
            return false;
        }
        let taken = |declared: &[(String, String)], workspace: &Workspace, own: &dyn Fn(&str) -> bool| declared.iter().any(|(name, path)| (name == old || name == new) && !own(path) && workspace.items.get(path).is_some_and(|item| of_kind(kind, item, workspace)));
        !taken(&declared_before, before, &|path| rules.iter().any(|rule| rule.old_path == path)) && !taken(&declared_after, after, &|path| rules.iter().any(|rule| rule.new_path == path))
    };
    let wanted_old = |word: &str| rules.contains_key(word);
    let wanted_new = |word: &str| new_names.contains(word);
    for item in before.items.values().filter(|item| item.kind != ItemKind::Mod) {
        let item_texts = texts(item);
        if !rules.keys().any(|name| item_texts.iter().any(|(text, _)| text.contains(name.as_str()))) {
            continue;
        }
        let mut scope = Scope::new(item, before);
        let after_item = after.items.get(renames.items.get(&item.path).unwrap_or(&item.path));
        // The keys of the new item with an old name, and of the old item with a new name: a
        // by-name place with such a key did not follow the rename.
        let mut stays: Option<(BTreeMap<String, usize>, BTreeMap<String, usize>)> = None;
        let mut after_bound: Option<Bound> = None;
        // The decision of each key: the new name, and the rules of the count.
        let mut decided: BTreeMap<String, (Option<String>, Vec<usize>, bool)> = BTreeMap::new();
        let mut open: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
        for (text, member) in item_texts {
            for place in places(text, &wanted_old) {
                let Some(list) = rules.get(&place.name) else { continue };
                let key = place.key();
                if !decided.contains_key(&key) {
                    let decision = match scope.classify(&place, list) {
                        Class::Never | Class::Other => (None, Vec::new(), false),
                        Class::Sure(index) => (Some(list[index].new.clone()), vec![index], true),
                        Class::ByName(kind) => {
                            let candidates: Vec<usize> = (0..list.len())
                                .filter(|&index| match kind {
                                    NameKind::Method => list[index].method,
                                    NameKind::Field => list[index].kind == ItemKind::Field,
                                    NameKind::Path => !matches!(list[index].kind, ItemKind::Field | ItemKind::Macro),
                                    NameKind::Macro => list[index].kind == ItemKind::Macro,
                                    NameKind::Any => true,
                                })
                                .collect();
                            let chosen: Vec<&Rule> = candidates.iter().map(|&index| &list[index]).collect();
                            let mut is_free = free(kind, &place.name, &chosen);
                            let new = chosen.first().map(|rule| rule.new.clone()).unwrap_or_default();
                            // A local of the body with the old name or the new name counts.
                            let bare = matches!(&place.kind, PlaceKind::Path { qualifier: Some(qualifier), .. } if qualifier.is_empty());
                            if is_free && (place.opaque || bare) {
                                let after_bound = after_bound.get_or_insert_with(|| after_item.map(bound).unwrap_or_default());
                                let old_bound = scope.bound();
                                is_free = !(place.opaque && old_bound.names.contains(&place.name)) && !old_bound.names.contains(&new) && !after_bound.names.contains(&new) && !after_bound.names.contains(&place.name);
                            }
                            // A by-name place that keeps its name is not a site (rule R4).
                            if is_free {
                                let (kept, early) = stays.get_or_insert_with(|| (after_item.map(|item| key_counts(item, &wanted_old)).unwrap_or_default(), key_counts(item, &wanted_new)));
                                let new_key = format!("{}{new}", &key[..key.len() - place.name.len()]);
                                is_free = !kept.contains_key(&key) && !early.contains_key(&new_key);
                            }
                            if is_free { (Some(new), candidates, false) } else { (None, candidates, false) }
                        }
                    };
                    decided.insert(key.clone(), decision);
                }
                let (new, indexes, is_sure) = &decided[&key];
                // The member list of an impl or a trait names each method again: no count.
                let counted = !(member && place.kind == PlaceKind::Decl);
                match new {
                    Some(new) => {
                        sites.entry(item.path.clone()).or_default().insert(key.clone(), new.clone());
                        for &index in indexes.iter().filter(|_| counted) {
                            let count = counts.entry(list[index].old_path.clone()).or_default();
                            if *is_sure {
                                count.sure += 1;
                            } else {
                                count.by_name += 1;
                            }
                        }
                    }
                    None if !indexes.is_empty() => {
                        let entry = open.entry(key.clone()).or_insert_with(|| (0, indexes.iter().map(|&index| list[index].old_path.clone()).collect()));
                        entry.0 += 1;
                    }
                    None => {}
                }
            }
        }
        // The by-name places that the rename does not have: the count of the changed ones.
        if !open.is_empty() {
            let kept = after_item.map(|item| key_counts(item, &wanted_old)).unwrap_or_default();
            for (key, (old_count, paths)) in open {
                let changed = old_count.saturating_sub(kept.get(&key).copied().unwrap_or(0));
                for path in paths {
                    counts.entry(path).or_default().own += changed;
                }
            }
        }
    }
    renames.sites = Some(sites);
    renames.counts = counts;
}

/// The line of a rename fact with the count of its by-name sites (rule R3), or `None` if the
/// fact has no such site. With the variable `SEMILLA_SITE_COUNTS`, each rename fact has a line
/// with the count of each class (for a measure).
pub fn count_line(renames: &Renames, old_path: &str) -> Option<String> {
    let count = renames.counts.get(old_path).copied().unwrap_or_default();
    if std::env::var_os("SEMILLA_SITE_COUNTS").is_some() {
        return Some(format!("sites: {} sure, {} by name, {} with their own fact", count.sure, count.by_name, count.own));
    }
    (count.by_name > 0).then(|| format!("{} site(s) by name (the syntax tree does not give the type of the receiver or the target of the path)", count.by_name))
}
