//! The items of the expanded code, with the place of the macro call that makes each one
//! (batch 6b, part 2; design 34.2).
//!
//! The syntax tree of Semilla does not expand a macro call. The compiler does: after the
//! expansion, each item has a span, and the span of an item from a macro has the place of the
//! outermost call. The driver writes each item with that place. Semilla puts an item under a
//! macro call if the place is in the source range of the call.

use rustc_middle::ty::TyCtxt;
use rustc_span::def_id::LocalDefId;

/// One item of the expanded code.
#[derive(Clone, serde::Serialize)]
pub struct MadeItem {
    /// The file of the place, relative to the workspace root.
    pub file: String,
    /// The line (from 1) and the column (from 0, in characters) of the place: the start of the
    /// outermost macro call, or the start of the item for an item with no macro.
    pub line: usize,
    pub col: usize,
    /// True if the span of the item comes from a macro expansion.
    pub expanded: bool,
    /// The module path of the item in the crate (`a::b`; empty for the crate root).
    pub module: String,
    /// The source text of the item after the expansion. A module has no content here: each
    /// item of the module has its own record.
    pub text: String,
    /// True if the item is a `#[test]` function.
    pub test: bool,
    /// True if a different crate can reach the item (the effective visibility).
    pub public: bool,
}

/// The place of a span: the start of the outermost macro call of its expansion, in a real file
/// of the local crate. `None` for code that the compiler adds with no source place.
pub fn place(tcx: TyCtxt<'_>, span: rustc_span::Span) -> Option<(String, usize, usize)> {
    let site = span.source_callsite();
    if site.is_dummy() {
        return None;
    }
    let position = tcx.sess.source_map().lookup_char_pos(site.lo());
    if position.file.cnum != rustc_span::def_id::LOCAL_CRATE {
        return None;
    }
    let rustc_span::FileName::Real(name) = &position.file.name else { return None };
    let path = name.local_path()?;
    Some((crate::workspace_path(path), position.line, position.col.0))
}

/// Reads the items after the macro expansion. Call it before the compiler lowers the syntax
/// tree. The `public` value is not known at this time: [`finish`] sets it.
pub fn collect(tcx: TyCtxt<'_>) -> Vec<(Option<LocalDefId>, MadeItem)> {
    let (resolver, krate) = tcx.resolver_for_lowering();
    let (resolver, krate) = (resolver.borrow(), krate.borrow());
    let mut out = Vec::new();
    walk(tcx, &|id| resolver.owners.get(&id).map(|owner| owner.def_id), &krate.items, "", &mut out);
    out
}

fn walk(tcx: TyCtxt<'_>, def_id: &dyn Fn(rustc_ast::NodeId) -> Option<LocalDefId>, items: &[Box<rustc_ast::Item>], module: &str, out: &mut Vec<(Option<LocalDefId>, MadeItem)>) {
    use rustc_ast::{ItemKind, ModKind};
    // The test harness makes a constant for each `#[test]` function, with the same name.
    let marker = |item: &rustc_ast::Item| item.attrs.iter().any(|attr| attr.has_name(rustc_span::sym::rustc_test_marker));
    let tests: Vec<rustc_span::Symbol> = items.iter().filter(|item| marker(item)).filter_map(|item| item.kind.ident()).map(|ident| ident.name).collect();
    for item in items {
        if marker(item) {
            continue;
        }
        // The expansion of `#[test]` adds `extern crate test;` next to the function.
        if !tests.is_empty() && item.span.from_expansion() && matches!(item.kind, ItemKind::ExternCrate(..)) && item.kind.ident().is_some_and(|ident| ident.name == rustc_span::sym::test) {
            continue;
        }
        let Some((file, line, col)) = place(tcx, item.span) else { continue };
        let inner = match &item.kind {
            ItemKind::Mod(_, ident, ModKind::Loaded(inner, ..)) => Some((ident.to_string(), inner)),
            _ => None,
        };
        let text = match &inner {
            Some(_) => {
                let mut copy = (**item).clone();
                if let ItemKind::Mod(_, _, ModKind::Loaded(inner, ..)) = &mut copy.kind {
                    inner.clear();
                }
                rustc_ast_pretty::pprust::item_to_string(&copy)
            }
            None => rustc_ast_pretty::pprust::item_to_string(item),
        };
        let test = matches!(item.kind, ItemKind::Fn(_)) && item.kind.ident().is_some_and(|ident| tests.contains(&ident.name));
        out.push((def_id(item.id), MadeItem { file, line, col, expanded: item.span.from_expansion(), module: module.to_string(), text, test, public: false }));
        if let Some((name, inner)) = inner {
            let path = if module.is_empty() { name } else { format!("{module}::{name}") };
            walk(tcx, def_id, inner, &path, out);
        }
    }
}

/// Sets the `public` value of each item, after the analysis.
pub fn finish(tcx: TyCtxt<'_>, items: Vec<(Option<LocalDefId>, MadeItem)>) -> Vec<MadeItem> {
    let visibilities = tcx.effective_visibilities(());
    items
        .into_iter()
        .map(|(def_id, mut item)| {
            // With no def id, the answer is not known: the safe answer is "public".
            item.public = def_id.is_none_or(|def_id| visibilities.is_reachable(def_id));
            item
        })
        .collect()
}
