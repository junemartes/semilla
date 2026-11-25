//! R68.5-1: a method of a trait impl that overrides a default method of the trait (batch 10u,
//! U5).
//!
//! A trait method with a default body gives that body to each impl that has no method of the
//! name. So a new method in an existing impl does not add an item to the API: the impl had the
//! method before, with the default body. The behavior of the impl changes. The fact compares
//! the new body with the old default body. A removed override is the same change in the other
//! direction: the impl now uses the default body.

use semilla_syntax::{ItemKind, Workspace};

/// The method of an impl method path `m::<T as Trait>::f`: the impl path `m::impl <T as
/// Trait>`, the last name of the trait, and the name of the method.
fn parts(method: &str) -> Option<(String, String, String)> {
    let (module, rest) = method.split_once("::<")?;
    let at = rest.rfind(">::")?;
    let (header, name) = (&rest[..at], &rest[at + 3..]);
    let (_, trait_path) = header.rsplit_once(" as ")?;
    let short = trait_path.rsplit("::").next()?.split('<').next()?.trim().to_string();
    (!name.contains("::") && !short.is_empty()).then(|| (format!("{module}::impl <{header}>"), short, name.to_string()))
}

/// The path of the default method of the trait that the impl method `method` of `ws` can use:
/// a trait of the workspace with the name of the trait of the impl, and a method of the name
/// with a body. `None` if no trait or two traits or more of the workspace have such a method
/// (Semilla does not decide between them), or if the impl is not in `ws`.
pub(crate) fn default_method(ws: &Workspace, method: &str) -> Option<String> {
    let (impl_path, short, name) = parts(method)?;
    let item = ws.items.get(&impl_path)?;
    let crate_name = &item.crate_name;
    let mut found = ws.items.values().filter(|other| other.kind == ItemKind::Trait && &other.crate_name == crate_name && other.path.rsplit("::").next().and_then(|own| own.split('#').next()) == Some(short.as_str())).filter_map(|tr| {
        let path = format!("{}::{name}", tr.path);
        ws.items.get(&path).filter(|found| found.kind == ItemKind::Fn && found.body.is_some()).map(|_| path)
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The fact text of a new or a removed override, and the path of the default method, or
/// `None` if the edit is not an override: the impl must be in the two trees, and the trait of
/// the other tree must have a default body for the method.
pub(crate) fn override_text(before: &Workspace, after: &Workspace, method: &str, added: bool) -> Option<(String, String)> {
    let (impl_path, _, _) = parts(method)?;
    if !before.items.contains_key(&impl_path) || !after.items.contains_key(&impl_path) {
        return None;
    }
    // The default body of the tree without the method: the impl used it there.
    let (with, without) = if added { (after, before) } else { (before, after) };
    let default = default_method(without, method)?;
    let own = with.items.get(method)?;
    let base = without.items.get(&default)?;
    let (old, new) = if added { (&base.source, &own.source) } else { (&own.source, &base.source) };
    let sides = crate::edits::sides_text(old, new);
    let text = if added {
        format!("`{method}` now overrides the default body of `{default}`: {sides}")
    } else {
        format!("`{method}` is removed: the impl now uses the default body of `{default}`: {sides}")
    };
    Some((text, default))
}

/// The part of an override fact.
pub(crate) const OVERRIDE_PART: &str = "override";
