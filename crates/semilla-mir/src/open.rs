//! The visibility override of the intent compile (design 78.2, R78.2-1).
//!
//! The two trees compile exactly as normal. Only the compile of the intent crate opens their
//! private items: the driver changes the visibility that this one compile reads from the metadata
//! of the workspace crates of the two trees. The variable `SEMILLA_INTENT_OPEN` gives the folders
//! of the metadata files of these crates (one folder for each line). A crate whose metadata file
//! is in one of these folders is an **open crate**. Each other crate (a dependency of the
//! registry, the standard library) keeps its visibility.
//!
//! The compiler reads a visibility from three places, and the driver changes each one:
//!
//! - the extern provider of the query `visibility` (the privacy check of paths, types, methods,
//!   and associated items): `Public` for each definition of an open crate;
//! - the extern provider of the query `module_children` (the name resolution of the paths into an
//!   open crate, also through a re-export and a glob import): each child is `Public`;
//! - the field data of the ADT definitions (the field access, the struct expression, the
//!   pattern, the exhaustiveness check): each field of an ADT of an open crate is `Public`.
//!
//! The limits: the ambiguous glob children of a module (`ambig_module_children`, two glob
//! imports that give one name) are not a query, so they keep their visibility; a `macro_rules!`
//! macro with no `#[macro_export]` is not in the metadata.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use rustc_middle::middle::resolve::ModChild;
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::def_id::{CrateNum, DefId, ModId};

/// The variable with the folders of the open crates.
pub const OPEN_VARIABLE: &str = "SEMILLA_INTENT_OPEN";

type VisibilityFn = for<'tcx> fn(TyCtxt<'tcx>, DefId) -> ty::Visibility<ModId>;
type ChildrenFn = for<'tcx> fn(TyCtxt<'tcx>, DefId) -> &'tcx [ModChild];
type AdtFn = for<'tcx> fn(TyCtxt<'tcx>, DefId) -> ty::AdtDef<'tcx>;

struct Defaults {
    visibility: VisibilityFn,
    module_children: ChildrenFn,
    adt_def: AdtFn,
}

static DEFAULTS: OnceLock<Defaults> = OnceLock::new();
static FOLDERS: OnceLock<Vec<PathBuf>> = OnceLock::new();
static OPEN: Mutex<Option<HashMap<CrateNum, bool>>> = Mutex::new(None);
/// The query override of the other parts of the driver (`config::install`).
static OTHER: OnceLock<Option<fn(&rustc_session::Session, &mut rustc_middle::util::Providers)>> = OnceLock::new();

/// The open folders of the variable, or `None` when the variable is not set (each compile of a
/// tree: no change).
pub fn folders_from_env() -> Option<Vec<PathBuf>> {
    let text = std::env::var(OPEN_VARIABLE).ok()?;
    Some(text.lines().filter(|line| !line.trim().is_empty()).map(|line| canonical(PathBuf::from(line.trim()))).collect())
}

fn canonical(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// [R78.2-1] Installs the three overrides, after the query override that `config` has already.
pub fn install(config: &mut rustc_interface::interface::Config, folders: Vec<PathBuf>) {
    let _ = FOLDERS.set(folders);
    // [R78.1-3] The resolver resolves each doc link: the scope paths of the claims.
    config.opts.resolve_doc_links = rustc_session::config::ResolveDocLinks::All;
    let _ = OTHER.set(config.override_queries);
    config.override_queries = Some(|session, providers| {
        if let Some(Some(other)) = OTHER.get() {
            other(session, providers);
        }
        let _ = DEFAULTS.set(Defaults {
            visibility: providers.extern_queries.visibility,
            module_children: providers.extern_queries.module_children,
            adt_def: providers.extern_queries.adt_def,
        });
        providers.extern_queries.visibility = visibility;
        providers.extern_queries.module_children = module_children;
        providers.extern_queries.adt_def = adt_def;
    });
}

fn defaults() -> &'static Defaults {
    DEFAULTS.get().expect("the default providers are stored")
}

/// True if the metadata file of the crate is in an open folder.
pub fn is_open(tcx: TyCtxt<'_>, krate: CrateNum) -> bool {
    // [R79.2-1] The local crate (the intent crate) has no metadata file: it is never open.
    if FOLDERS.get().is_none() || krate == rustc_span::def_id::LOCAL_CRATE {
        return false;
    }
    if let Some(known) = OPEN.lock().unwrap_or_else(|error| error.into_inner()).as_ref().and_then(|map| map.get(&krate)) {
        return *known;
    }
    let folders = FOLDERS.get().map(Vec::as_slice).unwrap_or(&[]);
    let source = tcx.used_crate_source(krate);
    let open = [&source.rmeta, &source.rlib, &source.dylib].into_iter().flatten().any(|file| {
        let file = canonical(file.clone());
        file.parent().is_some_and(|parent| folders.iter().any(|folder| folder == parent))
    });
    OPEN.lock().unwrap_or_else(|error| error.into_inner()).get_or_insert_with(HashMap::new).insert(krate, open);
    open
}

fn visibility(tcx: TyCtxt<'_>, def_id: DefId) -> ty::Visibility<ModId> {
    if is_open(tcx, def_id.krate) {
        return ty::Visibility::Public;
    }
    (defaults().visibility)(tcx, def_id)
}

fn module_children<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> &'tcx [ModChild] {
    let children = (defaults().module_children)(tcx, def_id);
    if !is_open(tcx, def_id.krate) {
        return children;
    }
    tcx.arena.alloc_from_iter(children.iter().map(|child| ModChild {
        ident: child.ident,
        res: child.res,
        vis: ty::Visibility::Public,
        reexport_chain: child.reexport_chain.clone(),
    }))
}

/// The field visibility is a value of the ADT data, not a query. The extern provider makes the
/// ADT data of one definition one time in a compile, and the interner keeps one value for each
/// definition. So the driver writes `Public` into each field of this new value, before the query
/// gives it to a reader. The value lives in the arena of this compile only: the compile of a tree
/// never sees it.
#[allow(invalid_reference_casting)]
fn adt_def<'tcx>(tcx: TyCtxt<'tcx>, def_id: DefId) -> ty::AdtDef<'tcx> {
    let adt = (defaults().adt_def)(tcx, def_id);
    if is_open(tcx, def_id.krate) {
        for variant in adt.variants() {
            for field in variant.fields.iter() {
                // SAFETY: the intent compile runs on one thread, no reader has the value yet, and
                // the write keeps a valid value of the field type.
                unsafe { std::ptr::write(&field.vis as *const ty::Visibility<ModId> as *mut ty::Visibility<ModId>, ty::Visibility::Public) };
            }
        }
    }
    adt
}

/// The variable with the folder of the prelude crate of the intent language.
pub const PRELUDE_VARIABLE: &str = "SEMILLA_INTENT_PRELUDE";

/// The index of the open folder of the crate (0: the old tree, 1: the new tree).
fn side_of(tcx: TyCtxt<'_>, krate: CrateNum) -> Option<usize> {
    let folders = FOLDERS.get()?;
    // [R79.2-1] A call of a function of the intent crate (a relation that a claim calls): the
    // local crate has no metadata file and no tree.
    if krate == rustc_span::def_id::LOCAL_CRATE {
        return None;
    }
    let source = tcx.used_crate_source(krate);
    [&source.rmeta, &source.rlib, &source.dylib].into_iter().flatten().find_map(|file| {
        let file = canonical(file.clone());
        folders.iter().position(|folder| file.parent() == Some(folder.as_path()))
    })
}

/// [R78.4-1] The name of a callee of the intent crate: for a definition of an open crate, the
/// facade of its tree and its definition path (`before::shop::inner::helper`: the name of its
/// record in that tree, also for an item that a re-export or a glob import gives); `None` for
/// each other definition.
pub fn callee_name(tcx: TyCtxt<'_>, def_id: DefId) -> Option<String> {
    FOLDERS.get()?;
    let side = side_of(tcx, def_id.krate)?;
    let path = rustc_middle::ty::print::with_no_visible_paths!(rustc_middle::ty::print::with_no_trimmed_paths!(tcx.def_path_str(def_id)));
    Some(format!("{}::{path}", if side == 0 { "before" } else { "after" }))
}

/// [R78.4-2] True for a definition of the prelude crate of the intent language (its metadata
/// file is in the folder of the variable `SEMILLA_INTENT_PRELUDE`): Semilla gives it its meaning.
pub fn is_prelude(tcx: TyCtxt<'_>, krate: CrateNum) -> bool {
    // Only the intent compile has the open folders. [R79.2-1] The local crate is not the prelude.
    if FOLDERS.get().is_none() || krate == rustc_span::def_id::LOCAL_CRATE {
        return false;
    }
    let Some(folder) = std::env::var_os(PRELUDE_VARIABLE).map(|folder| canonical(PathBuf::from(folder))) else { return false };
    let source = tcx.used_crate_source(krate);
    [&source.rmeta, &source.rlib, &source.dylib].into_iter().flatten().any(|file| canonical(file.clone()).parent() == Some(folder.as_path()))
}

/// [R78.2-1] Makes the ADT data of each struct, enum, and union of each open crate through the
/// query, right after the expansion. The decoder of a type of the metadata interns the ADT data
/// that it decodes, with no query, and the interner keeps the first value for each definition:
/// so the query must run first, or a type that the type check decodes first keeps its private
/// fields. A decoded value of the same definition then gives the value of the query.
pub fn open_adts(tcx: TyCtxt<'_>) {
    if FOLDERS.get().is_none() {
        return;
    }
    for &krate in tcx.crates(()) {
        if !is_open(tcx, krate) {
            continue;
        }
        let count = rustc_metadata::creader::CStore::from_tcx(tcx).num_def_ids_untracked(krate);
        for index in 0..count {
            let def_id = DefId { krate, index: rustc_span::def_id::DefIndex::from_usize(index) };
            let kind = rustc_metadata::creader::CStore::from_tcx(tcx).def_kind_untracked(def_id);
            if matches!(kind, rustc_hir::def::DefKind::Struct | rustc_hir::def::DefKind::Enum | rustc_hir::def::DefKind::Union) {
                let _ = tcx.adt_def(def_id);
            }
        }
    }
}

/// [R78.1-3] One scope path of a claim (a doc link of its hidden function) and its definition.
#[derive(serde::Serialize)]
pub struct Link {
    pub path: String,
    /// The record name of the definition with the facade (`before::shop::inner::len`), `None`
    /// if the resolver of rustc and the items of the resolved type give no definition.
    pub resolved: Option<String>,
    /// The names next to the last resolved part of the path (for the candidates of an error).
    pub near: Vec<String>,
    /// [R80.3-1] The items of the definition for a scope: for a type, each function of its
    /// inherent impls; for a module, each function and each method of a type that the module
    /// defines with the visibility `pub` (the visibility of the metadata, not the override); for a
    /// trait, each method of each impl of the trait in an open crate.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
    /// [R87.2-1] For a type-relative path: each item of that name of the type (its inherent
    /// impls and its trait impls). A qualified path `<T as Trait>::item` names one of them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
}

/// [R80.3-1] One item of a scope: the record name with the facade, and for a method of an impl
/// the type of the impl (its record name with the facade).
#[derive(serde::Serialize)]
pub struct Member {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The name with the visible paths of the other crates (`<shop::Cart as std::fmt::Display>::
    /// fmt`), when it is not `name`: a record of a tree names a trait of another crate by its
    /// visible path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<String>,
}

/// [R80.3-1] The items of a definition for a scope (see [`Link::members`]).
fn members(tcx: TyCtxt<'_>, def_id: DefId) -> Vec<Member> {
    use rustc_hir::def::DefKind;
    let name_of = |def_id: DefId| callee_name(tcx, def_id).unwrap_or_else(|| tcx.def_path_str(def_id));
    let is_fn = |def_id: DefId| matches!(tcx.def_kind(def_id), DefKind::Fn | DefKind::AssocFn);
    // The name with the visible paths, with the facade of the tree in front only: the visible
    // path of an item of an open crate goes through its facade (`after::shop::Cart`).
    let visible = |def_id: DefId, name: &str| -> Option<String> {
        let side = name.split("::").next()?;
        let path = rustc_middle::ty::print::with_no_trimmed_paths!(tcx.def_path_str(def_id));
        let text = format!("{side}::{}", path.replace(&format!("{side}::"), ""));
        (text != name).then_some(text)
    };
    let member = |def_id: DefId, owner: Option<String>| -> Member {
        let name = name_of(def_id);
        Member { visible: visible(def_id, &name), name, owner }
    };
    // The functions of the inherent impls of a type, in the order of the definitions.
    let methods = |adt: DefId, public_only: bool| -> Vec<Member> {
        let mut found = Vec::new();
        for &impl_id in tcx.inherent_impls(adt) {
            for item in tcx.associated_items(impl_id).in_definition_order() {
                if is_fn(item.def_id) && (!public_only || (defaults().visibility)(tcx, item.def_id).is_public()) {
                    found.push(member(item.def_id, Some(name_of(adt))));
                }
            }
        }
        found
    };
    match tcx.def_kind(def_id) {
        DefKind::Struct | DefKind::Enum | DefKind::Union => methods(def_id, false),
        DefKind::Mod => {
            let mut found = Vec::new();
            for child in (defaults().module_children)(tcx, def_id) {
                let Some(child_id) = child.res.opt_def_id() else { continue };
                // The items that the module defines, with `pub` (a re-export names an item of
                // another module).
                if tcx.opt_parent(child_id) != Some(def_id) || !(defaults().visibility)(tcx, child_id).is_public() {
                    continue;
                }
                match tcx.def_kind(child_id) {
                    DefKind::Fn => found.push(member(child_id, None)),
                    DefKind::Struct | DefKind::Enum | DefKind::Union => found.extend(methods(child_id, true)),
                    _ => {}
                }
            }
            found
        }
        DefKind::Trait => {
            let mut found = Vec::new();
            for &krate in tcx.crates(()) {
                if !is_open(tcx, krate) {
                    continue;
                }
                for &impl_id in tcx.trait_impls_in_crate(krate) {
                    if tcx.impl_trait_id(impl_id) != def_id {
                        continue;
                    }
                    let owner = tcx.type_of(impl_id).instantiate_identity().skip_norm_wip().ty_adt_def().map(|adt| name_of(adt.did()));
                    for item in tcx.associated_items(impl_id).in_definition_order() {
                        if is_fn(item.def_id) {
                            found.push(member(item.def_id, owner.clone()));
                        }
                    }
                }
            }
            found
        }
        _ => Vec::new(),
    }
}

/// [R78.1-3] The definition of each doc link `before::…` and `after::…` of the intent crate: the
/// resolution of the resolver of rustc; for a path whose last part is an associated item, the
/// item of that name of the resolved type (its inherent impls, its trait impls) or trait.
pub fn links(tcx: TyCtxt<'_>) -> Vec<Link> {
    use rustc_hir::def::{DefKind, Namespace, Res};
    if FOLDERS.get().is_none() {
        return Vec::new();
    }
    let name_of = |def_id: DefId| callee_name(tcx, def_id).unwrap_or_else(|| tcx.def_path_str(def_id));
    let mut found = Vec::new();
    // The modules with doc links only (the query has no value for another module).
    for map in tcx.resolutions(()).doc_link_resolutions.values() {
        let get = |path: &str, ns: Namespace| -> Option<DefId> {
            match map.get(&(rustc_span::Symbol::intern(path), ns)) {
                Some(Some(Res::Def(_, def_id))) => Some(*def_id),
                _ => None,
            }
        };
        // [R80.3-1] The scope paths of the two versions, and a trait path of a scope
        // `impl TRAIT for …` (a path of the intent file).
        let is_trait = |path: &str| get(path, Namespace::TypeNS).is_some_and(|def_id| matches!(tcx.def_kind(def_id), DefKind::Trait));
        let paths: std::collections::BTreeSet<String> = map.keys().map(|(path, _)| path.to_string()).filter(|path| path.starts_with("before::") || path.starts_with("after::") || is_trait(path)).collect();
        for path in paths {
            let direct = get(&path, Namespace::ValueNS).or_else(|| get(&path, Namespace::TypeNS)).or_else(|| get(&path, Namespace::MacroNS));
            if let Some(def_id) = direct {
                found.push(Link { path, resolved: Some(name_of(def_id)), near: Vec::new(), members: members(tcx, def_id), also: Vec::new() });
                continue;
            }
            // A type-relative path: the last part is an item of the resolved type or trait.
            let Some((prefix, last)) = path.rsplit_once("::") else { continue };
            let Some(owner) = get(prefix, Namespace::TypeNS) else {
                // The names of the deepest module that resolves, for the candidates.
                let mut near = Vec::new();
                let mut part = prefix;
                while let Some((up, _)) = part.rsplit_once("::") {
                    if let Some(module) = get(up, Namespace::TypeNS).filter(|def_id| matches!(tcx.def_kind(*def_id), DefKind::Mod)) {
                        near = tcx.module_children(module).iter().map(|child| format!("{up}::{}", child.ident)).collect();
                        break;
                    }
                    part = up;
                }
                if near.is_empty()
                    && let Some(module) = get(prefix, Namespace::TypeNS)
                {
                    near = tcx.module_children(module).iter().map(|child| format!("{prefix}::{}", child.ident)).collect();
                }
                found.push(Link { path, resolved: None, near, members: Vec::new(), also: Vec::new() });
                continue;
            };
            if matches!(tcx.def_kind(owner), DefKind::Mod) {
                let near = tcx.module_children(owner).iter().map(|child| format!("{prefix}::{}", child.ident)).collect();
                found.push(Link { path, resolved: None, near, members: Vec::new(), also: Vec::new() });
                continue;
            }
            let symbol = rustc_span::Symbol::intern(last);
            let mut owners: Vec<DefId> = Vec::new();
            match tcx.def_kind(owner) {
                DefKind::Trait => owners.push(owner),
                DefKind::Struct | DefKind::Enum | DefKind::Union => {
                    owners.extend(tcx.inherent_impls(owner).iter().copied());
                    for &impl_id in tcx.trait_impls_in_crate(owner.krate) {
                        let self_ty = tcx.type_of(impl_id).instantiate_identity().skip_norm_wip();
                        if self_ty.ty_adt_def().is_some_and(|adt| adt.did() == owner) {
                            owners.push(impl_id);
                        }
                    }
                }
                _ => {}
            }
            let item = owners.iter().find_map(|owner| tcx.associated_items(*owner).filter_by_name_unhygienic(symbol).next().map(|item| item.def_id));
            let near = if item.is_none() { owners.iter().flat_map(|owner| tcx.associated_items(*owner).in_definition_order().map(|item| format!("{prefix}::{}", item.name()))).collect() } else { Vec::new() };
            let also: Vec<String> = owners.iter().flat_map(|owner| tcx.associated_items(*owner).filter_by_name_unhygienic(symbol).map(|item| item.def_id).collect::<Vec<_>>()).map(name_of).collect();
            let also = if also.len() > 1 { also } else { Vec::new() };
            found.push(Link { path, resolved: item.map(name_of), near, members: Vec::new(), also });
        }
    }
    found
}

/// [R78.1-3] Writes the definitions of the scope paths as one JSON line on the error output
/// (`$message_type` `semilla_links`), next to the messages of rustc.
pub fn print_links(tcx: TyCtxt<'_>) {
    if FOLDERS.get().is_none() {
        return;
    }
    let found = links(tcx);
    if let Ok(text) = serde_json::to_string(&serde_json::json!({"$message_type": "semilla_links", "links": found})) {
        eprintln!("{text}");
    }
}
