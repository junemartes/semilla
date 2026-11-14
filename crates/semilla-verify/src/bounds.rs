//! The normal form of the bounds of a declaration (batch 6b, part 3; design 34.3).
//!
//! A bound list is a set of predicates. A predicate is a pair: the bounded type or lifetime,
//! and one bound. `T: A + B` gives the pairs (`T`, `A`) and (`T`, `B`). A bound in the
//! parameter list and the same bound in the `where` clause give the same pair. `?Sized` and a
//! `for<'a>` binder are a part of the text of the bound. A `for<'a>` binder before the bounded
//! type is a part of the text of the bounded type.
//!
//! The compare is a compare of token text. There is no name resolution: `T: fmt::Debug` and
//! `T: Debug` are different pairs.
//!
//! Each other part of the declaration stays in its order in the text of the rest: the list of
//! the generic parameters with each default, the supertrait list, the parameters, the return
//! type, and the qualifiers.

use std::collections::BTreeSet;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use semilla_syntax::{Item, ItemKind, Workspace};

type Pair = (String, String);

/// The part name of the fact for a change with an equal set of predicates.
pub(crate) const PART: &str = "bounds";

struct Form {
    /// The declaration with no bound in the parameter list and with no `where` clause.
    rest: String,
    /// Each pair, with each duplicate.
    all: Vec<Pair>,
    /// The pairs of the `where` clause.
    in_where: BTreeSet<Pair>,
}

fn text(tokens: &impl ToTokens) -> String {
    tokens.to_token_stream().to_string()
}

/// Takes the bounds out of the generics. `None` for a predicate of a kind that Semilla does
/// not know.
fn take(generics: &mut syn::Generics) -> Option<(Vec<Pair>, BTreeSet<Pair>)> {
    let mut all = Vec::new();
    let mut in_where = BTreeSet::new();
    for param in generics.params.iter_mut() {
        match param {
            syn::GenericParam::Lifetime(param) => {
                let name = text(&param.lifetime);
                all.extend(param.bounds.iter().map(|bound| (name.clone(), text(bound))));
                param.bounds.clear();
                param.colon_token = None;
            }
            syn::GenericParam::Type(param) => {
                let name = param.ident.to_string();
                all.extend(param.bounds.iter().map(|bound| (name.clone(), text(bound))));
                param.bounds.clear();
                param.colon_token = None;
            }
            syn::GenericParam::Const(_) => {}
        }
    }
    if let Some(clause) = generics.where_clause.take() {
        for predicate in &clause.predicates {
            let (bounded, bounds): (String, Vec<String>) = match predicate {
                syn::WherePredicate::Lifetime(predicate) => (text(&predicate.lifetime), predicate.bounds.iter().map(text).collect()),
                syn::WherePredicate::Type(predicate) => {
                    let binder = predicate.lifetimes.as_ref().map(|binder| format!("{} ", text(binder))).unwrap_or_default();
                    (format!("{binder}{}", text(&predicate.bounded_ty)), predicate.bounds.iter().map(text).collect())
                }
                _ => return None,
            };
            // A predicate with no bound (`where Vec<T>:`) says that the type is well formed.
            // It is a pair with an empty bound.
            let bounds = if bounds.is_empty() { vec![String::new()] } else { bounds };
            for bound in bounds {
                all.push((bounded.clone(), bound.clone()));
                in_where.insert((bounded.clone(), bound));
            }
        }
    }
    Some((all, in_where))
}

/// The form of a function, a method, a struct, an enum, a union, a trait, or an `impl`.
fn form(item: &Item) -> Option<Form> {
    let (rest, (all, in_where)) = if item.kind == ItemKind::Fn {
        let mut signature: syn::Signature = syn::parse_str(&item.signature).ok()?;
        let pairs = take(&mut signature.generics)?;
        (text(&signature), pairs)
    } else {
        let mut parsed: syn::Item = syn::parse_str(&item.signature).ok()?;
        let generics = match &mut parsed {
            syn::Item::Struct(value) => &mut value.generics,
            syn::Item::Enum(value) => &mut value.generics,
            syn::Item::Union(value) => &mut value.generics,
            syn::Item::Trait(value) => &mut value.generics,
            syn::Item::Impl(value) => &mut value.generics,
            _ => return None,
        };
        let pairs = take(generics)?;
        (text(&parsed), pairs)
    };
    Some(Form { rest, all, in_where })
}

/// True if a macro can read the bounds of the item as tokens, and make different code for a
/// different order: an attribute macro on the item or on an item around it, a `cfg_attr`, or a
/// `derive` that is not a `derive` of the standard library. The MIR compare sees the bodies
/// that such a macro makes. It does not see a declaration that the macro makes.
fn has_reader(item: &Item, workspace: &Workspace) -> bool {
    if crate::srcconfig::token_reader(item, workspace) {
        return true;
    }
    item.attrs.iter().any(|attr| {
        let Ok(stream) = attr.parse::<TokenStream>() else { return true };
        let Some(inner) = stream.into_iter().find_map(|tree| match tree {
            TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket => Some(group.stream()),
            _ => None,
        }) else {
            return true;
        };
        let mut trees = inner.into_iter();
        match trees.next() {
            Some(TokenTree::Ident(name)) if name == "cfg_attr" => true,
            Some(TokenTree::Ident(name)) if name == "derive" => {
                let Some(TokenTree::Group(list)) = trees.next() else { return true };
                // The last name of each path: `derive(Clone, std::fmt::Debug)`.
                let mut last: Option<String> = None;
                let mut names: Vec<String> = Vec::new();
                for tree in list.stream() {
                    match tree {
                        TokenTree::Ident(word) => last = Some(word.to_string()),
                        TokenTree::Punct(mark) if mark.as_char() == ',' => names.extend(last.take()),
                        _ => {}
                    }
                }
                names.extend(last);
                names.iter().any(|name| !crate::opaque::derive_known(name))
            }
            _ => false,
        }
    })
}

/// The text of a change of the bounds of a declaration with an equal set of predicates and
/// with each other part of the declaration equal: "order only", "moved to the `where` clause",
/// and so on. `None` for each other change. The caller makes the fact neutral only if each
/// body of the workspace that uses the item has an equal MIR.
pub(crate) fn same_set(old: &Item, new: &Item, before: &Workspace, after: &Workspace) -> Option<&'static str> {
    if old.kind != new.kind || old.signature == new.signature {
        return None;
    }
    let (a, b) = (form(old)?, form(new)?);
    let set = |form: &Form| form.all.iter().cloned().collect::<BTreeSet<Pair>>();
    if a.rest != b.rest || set(&a) != set(&b) {
        return None;
    }
    if has_reader(old, before) || has_reader(new, after) {
        return None;
    }
    let sorted = |form: &Form| {
        let mut list = form.all.clone();
        list.sort();
        list
    };
    Some(if a.in_where != b.in_where {
        match (b.in_where.is_superset(&a.in_where), a.in_where.is_superset(&b.in_where)) {
            (true, _) => "moved to the `where` clause",
            (_, true) => "moved out of the `where` clause",
            _ => "moved between the parameter list and the `where` clause",
        }
    } else if sorted(&a) != sorted(&b) {
        "a duplicate bound only"
    } else {
        "order only"
    })
}
