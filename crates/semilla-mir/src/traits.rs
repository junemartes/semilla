//! The trait table (batch 5p, C2): the facts of the compiler about the traits that a crate can
//! see. A test compares the table of known traits of the verifier with these facts.
//!
//! The driver writes the table only if `SEMILLA_TRAIT_TABLE` names a folder. The file is
//! `<crate>.json` with one object:
//!
//! - `traits`: for each trait of each crate that the compiler has loaded, the path, true for
//!   a trait with the visibility `pub`, the associated items (`fn name`, `const NAME`, `type Name`), and the blanket impls. A blanket
//!   impl has a type parameter as its self type. Its entry is the list of the traits in its
//!   bounds, without `Sized`.
//! - `impls`: for each `impl` of the local crate, the self type, the path of the trait, the
//!   associated items, and true if a `derive` makes the `impl`.

use rustc_hir::def::DefKind;
use rustc_middle::ty::{self, TyCtxt};
use rustc_span::def_id::{DefId, LOCAL_CRATE};
use serde::Serialize;

#[derive(Serialize)]
struct TraitFacts {
    path: String,
    public: bool,
    items: Vec<String>,
    blanket: Vec<Vec<String>>,
}

#[derive(Serialize)]
struct ImplFacts {
    self_type: String,
    of_trait: Option<String>,
    items: Vec<String>,
    derive: bool,
}

#[derive(Serialize)]
struct Table {
    traits: Vec<TraitFacts>,
    impls: Vec<ImplFacts>,
}

fn items(tcx: TyCtxt<'_>, def_id: DefId) -> Vec<String> {
    let mut list: Vec<String> = tcx
        .associated_items(def_id)
        .in_definition_order()
        .filter_map(|item| {
            let kind = match item.kind {
                ty::AssocKind::Fn { .. } => "fn",
                ty::AssocKind::Const { .. } => "const",
                ty::AssocKind::Type { .. } => "type",
            };
            // The type of an `impl Trait` in a return type has no name.
            Some(format!("{kind} {}", item.opt_name()?))
        })
        .collect();
    list.sort();
    list
}

/// Writes the trait table of the crate to the folder of `SEMILLA_TRAIT_TABLE`.
pub fn write(tcx: TyCtxt<'_>) -> std::io::Result<()> {
    let Some(folder) = std::env::var_os("SEMILLA_TRAIT_TABLE") else { return Ok(()) };
    let sized = tcx.lang_items().sized_trait();
    let mut traits = Vec::new();
    for def_id in tcx.all_traits_including_private() {
        let mut blanket = Vec::new();
        for impl_id in tcx.all_impls(def_id) {
            if !matches!(tcx.type_of(impl_id).instantiate_identity().skip_norm_wip().kind(), ty::Param(_)) {
                continue;
            }
            let mut bounds: Vec<String> = tcx
                .clauses_of(impl_id)
                .instantiate_identity(tcx)
                .clauses
                .into_iter()
                .filter_map(|clause| clause.skip_norm_wip().as_trait_clause())
                .map(|clause| clause.def_id())
                .filter(|bound| Some(*bound) != sized)
                .map(|bound| tcx.def_path_str(bound))
                .collect();
            bounds.sort();
            bounds.dedup();
            blanket.push(bounds);
        }
        blanket.sort();
        blanket.dedup();
        traits.push(TraitFacts { path: tcx.def_path_str(def_id), public: tcx.visibility(def_id).is_public(), items: items(tcx, def_id), blanket });
    }
    traits.sort_by(|a, b| a.path.cmp(&b.path));
    let mut impls = Vec::new();
    for local in tcx.hir_crate_items(()).definitions() {
        if !matches!(tcx.def_kind(local), DefKind::Impl { .. }) {
            continue;
        }
        let def_id = local.to_def_id();
        let derive = !matches!(tcx.def_span(def_id).ctxt().outer_expn_data().kind, rustc_span::ExpnKind::Root);
        let of_trait = tcx.impl_opt_trait_id(def_id).map(|trait_id| tcx.def_path_str(trait_id));
        impls.push(ImplFacts { self_type: tcx.type_of(def_id).instantiate_identity().skip_norm_wip().to_string(), of_trait, items: items(tcx, def_id), derive });
    }
    impls.sort_by(|a, b| (&a.self_type, &a.of_trait).cmp(&(&b.self_type, &b.of_trait)));
    let text = serde_json::to_string(&Table { traits, impls }).expect("the table is valid JSON");
    let file = std::path::Path::new(&folder).join(format!("{}.json", tcx.crate_name(LOCAL_CRATE)));
    crate::write_atomically(&file, &text)
}
