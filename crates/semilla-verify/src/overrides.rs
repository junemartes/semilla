//! R70.2-1 (e): a method of a trait impl that overrides a default method of the trait (batch 10u,
//! U5; batch 11b: the compiler decides).
//!
//! A trait method with a default body gives that body to each impl that has no method of the
//! name. So a new method in an existing impl does not add an item to the API: the impl had the
//! method before, with the default body. The behavior of the impl changes. The name record of
//! the compiler gives the item of the trait that the method gives, and if that item has a
//! default (R70.2-1 (e)), also for a trait of another crate. The fact compares the new body with the
//! old default body when the trait is in the workspace. A removed override is the same change
//! in the other direction: the impl now uses the default body.

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

/// The text rule of the fall-back (R70.1-1; batch 10u, R68.5-1), for a method that the record
/// of its tree does not have (no record, or code that the configuration does not build): the
/// default method of a trait of the workspace with the last name of the trait of the impl. `None`
/// if no trait or two traits or more of the crate have such a method, or if the impl is not in
/// `ws`.
fn default_method(ws: &Workspace, method: &str) -> Option<String> {
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
/// `None` if the edit is not an override: the impl must be in the two trees, and the record of
/// the tree with the method must say that the method gives an item of the trait with a default.
/// A tree with no record: `None` (the method is an added or a removed item, R70.1-1).
pub(crate) fn override_text(before: &Workspace, after: &Workspace, method: &str, added: bool) -> Option<(String, String)> {
    let (impl_path, _, _) = parts(method)?;
    if !before.items.contains_key(&impl_path) || !after.items.contains_key(&impl_path) {
        return None;
    }
    let (with, without) = if added { (after, before) } else { (before, after) };
    let default = match crate::records::trait_item_of(with, method) {
        // The default of the trait in the tree without the method: the impl uses it there (a
        // step can add the default and remove the methods of the impls).
        Ok(Some((default, has))) if crate::records::trait_default(without, &default).unwrap_or(has) => default,
        Ok(_) => return None,
        // No record of the method: the text rule of the fall-back.
        Err(()) => default_method(without, method)?,
    };
    let own = with.items.get(method)?;
    // The default body of a trait of the workspace, in the tree without the method: the impl
    // used it there.
    let base = crate::records::item_of_def(without, &default).or_else(|| without.items.contains_key(&default).then(|| default.clone())).and_then(|path| without.items.get(&path));
    let text = match base {
        Some(base) => {
            let (old, new) = if added { (&base.source, &own.source) } else { (&own.source, &base.source) };
            let sides = crate::edits::sides_text(old, new);
            if added {
                format!("`{method}` now overrides the default body of `{default}`: {sides}")
            } else {
                format!("`{method}` is removed: the impl now uses the default body of `{default}`: {sides}")
            }
        }
        None => {
            let body = format!("`{}`", semilla_syntax::tree::one_line(&own.source));
            if added {
                format!("`{method}` now overrides the default body of `{default}` (a trait of another crate: Semilla does not show its default body): {body}")
            } else {
                format!("`{method}` is removed: the impl now uses the default body of `{default}` (a trait of another crate: Semilla does not show its default body); the removed body: {body}")
            }
        }
    };
    Some((text, default))
}

/// The part of an override fact.
pub(crate) const OVERRIDE_PART: &str = "override";
