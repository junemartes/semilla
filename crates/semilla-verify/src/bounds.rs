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

use ast::{HasGenericParams, HasName, HasTypeBounds};
use semilla_syntax::rs::{AstNode, Direction, SyntaxKind, SyntaxNode, TextRange, ast, compare, compare_opt};
use semilla_syntax::tt::{Delimiter, TokenStream, TokenTree};
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

/// Takes the bounds out of the generics of a declaration: the text of the declaration with no
/// bound in the parameter list and with no `where` clause, the pairs, and the pairs of the
/// `where` clause. `None` for a predicate of a kind that Semilla does not know.
fn take(node: &SyntaxNode, params: Option<&ast::GenericParamList>, clause: Option<&ast::WhereClause>) -> Option<(String, Vec<Pair>, BTreeSet<Pair>)> {
    let mut all = Vec::new();
    let mut in_where = BTreeSet::new();
    let mut skip: Vec<TextRange> = Vec::new();
    // A bound list with the `:` before it.
    let mut drop_bounds = |bounds: Option<ast::TypeBoundList>| {
        if let Some(bounds) = bounds {
            let colon = bounds
                .syntax()
                .siblings_with_tokens(Direction::Prev)
                .skip(1)
                .find(|element| !element.kind().is_trivia())
                .filter(|element| element.kind() == SyntaxKind::COLON);
            skip.push(colon.map_or(bounds.syntax().text_range(), |colon| {
                colon.text_range().cover(bounds.syntax().text_range())
            }));
        }
    };
    for param in params.into_iter().flat_map(semilla_syntax::rs::ast::GenericParamList::generic_params) {
        match &param {
            ast::GenericParam::LifetimeParam(param) => {
                let name = compare_opt(param.lifetime().as_ref());
                all.extend(
                    param
                        .type_bound_list()
                        .into_iter()
                        .flat_map(|list| list.bounds())
                        .map(|bound| (name.clone(), compare(&bound))),
                );
                drop_bounds(param.type_bound_list());
            }
            ast::GenericParam::TypeParam(param) => {
                let name = param.name().map_or_default(|name| name.text().to_string());
                all.extend(
                    param
                        .type_bound_list()
                        .into_iter()
                        .flat_map(|list| list.bounds())
                        .map(|bound| (name.clone(), compare(&bound))),
                );
                drop_bounds(param.type_bound_list());
            }
            ast::GenericParam::ConstParam(_) => {}
        }
    }
    if let Some(clause) = clause {
        skip.push(clause.syntax().text_range());
        for predicate in clause.predicates() {
            let bounded = match (predicate.lifetime(), predicate.ty()) {
                (Some(lifetime), None) => compare(&lifetime),
                (None, Some(ty)) => {
                    let binder = predicate.for_binder().map_or_default(|binder| format!("{} ", compare(&binder)));
                    format!("{binder}{}", compare(&ty))
                }
                _ => return None,
            };
            let bounds: Vec<String> = predicate
                .type_bound_list()
                .into_iter()
                .flat_map(|list| list.bounds())
                .map(|bound| compare(&bound))
                .collect();
            // A predicate with no bound (`where Vec<T>:`) says that the type is well formed.
            // It is a pair with an empty bound.
            let bounds = if bounds.is_empty() { vec![String::new()] } else { bounds };
            for bound in bounds {
                all.push((bounded.clone(), bound.clone()));
                in_where.insert((bounded.clone(), bound));
            }
        }
    }
    Some((semilla_syntax::tt::text_without(node, &skip), all, in_where))
}

/// The form of a function, a method, a struct, an enum, a union, a trait, or an `impl`.
fn form(item: &Item) -> Option<Form> {
    let (rest, all, in_where) = if item.kind == ItemKind::Fn {
        let signature = semilla_syntax::rs::parse_signature(&item.signature)?;
        // The parse adds a `;` to the signature text.
        let (rest, all, in_where) = take(
            signature.syntax(),
            signature.generic_param_list().as_ref(),
            signature.where_clause().as_ref(),
        )?;
        (rest.strip_suffix(" ;").map(str::to_string).unwrap_or(rest), all, in_where)
    } else {
        let parsed = semilla_syntax::rs::parse_item(&item.signature)?;
        let (params, clause) = match &parsed {
            ast::Item::Struct(value) => (value.generic_param_list(), value.where_clause()),
            ast::Item::Enum(value) => (value.generic_param_list(), value.where_clause()),
            ast::Item::Union(value) => (value.generic_param_list(), value.where_clause()),
            ast::Item::Trait(value) if value.eq_token().is_none() => (value.generic_param_list(), value.where_clause()),
            ast::Item::Impl(value) => (value.generic_param_list(), value.where_clause()),
            _ => return None,
        };
        take(parsed.syntax(), params.as_ref(), clause.as_ref())?
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
        let Ok(stream) = attr.parse::<TokenStream>() else {
            return true;
        };
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
                let Some(TokenTree::Group(list)) = trees.next() else {
                    return true;
                };
                // The last name of each path: `derive(Clone, std::fmt::Debug)`.
                let mut latest: Option<String> = None;
                let mut derived: Vec<String> = Vec::new();
                for tree in list.stream() {
                    match tree {
                        TokenTree::Ident(word) => latest = Some(word.to_string()),
                        TokenTree::Punct(mark) if mark.as_char() == ',' => derived.extend(latest.take()),
                        _ => {}
                    }
                }
                derived.extend(latest);
                derived.iter().any(|name| !crate::opaque::derive_known(name))
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
