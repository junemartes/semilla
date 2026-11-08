//! `rewrite-pattern`: one rule that explains many sites.
//!
//! A pattern is a Rust expression with metavariables, for example `$c.total()`. A metavariable
//! matches any expression; the same name must match the same text. The verifier explains each
//! edit whose site matches `from` before and equals `to` (with the same bindings) after. With
//! `sites: "all"` (the default), every site that matches `from` in the scope must be rewritten.

use std::collections::BTreeMap;

use semilla_core::{Anchor, Claim, ClaimStatus, Segment};
use semilla_syntax::Node;

use crate::checks::{Context, Outcome};

const META: &str = "__semilla_meta_";

/// Parses a pattern into a node tree. `$name` becomes a special identifier.
fn parse_pattern(text: &str) -> Option<Node> {
    let mut source = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            source.push_str(META);
        } else {
            source.push(c);
        }
    }
    let expr: syn::Expr = syn::parse_str(&source).ok()?;
    let _ = chars.peek();
    Some(semilla_syntax::tree::expr_node("pattern", &expr))
}

/// Matches a node against a pattern, and records the bindings.
fn matches(pattern: &Node, node: &Node, bindings: &mut BTreeMap<String, String>) -> bool {
    if pattern.kind == "path" && pattern.text.starts_with(META) && !pattern.text.contains(' ') {
        let name = pattern.text.trim_start_matches(META).to_string();
        return match bindings.get(&name) {
            Some(bound) => bound == &node.text,
            None => {
                bindings.insert(name, node.text.clone());
                true
            }
        };
    }
    if pattern.kind != node.kind || pattern.attr != node.attr || pattern.children.len() != node.children.len() {
        return false;
    }
    if pattern.children.is_empty() {
        return pattern.text == node.text;
    }
    pattern.children.iter().zip(&node.children).all(|(p, n)| p.role == n.role && matches(p, n, bindings))
}

/// Builds the text of `to` with the bindings, in normalized token form.
fn instantiate(to: &str, bindings: &BTreeMap<String, String>) -> Option<String> {
    let mut text = to.to_string();
    // Longer names first, so that `$ab` is not replaced as `$a` + `b`.
    let mut names: Vec<&String> = bindings.keys().collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    for name in names {
        text = text.replace(&format!("${name}"), &format!("({})", bindings[name]));
    }
    let expr: syn::Expr = syn::parse_str(&text).ok()?;
    Some(strip_parens(&quote::ToTokens::to_token_stream(&expr).to_string()))
}

/// Removes the parentheses that `instantiate` adds around single tokens, so the text compares.
fn strip_parens(text: &str) -> String {
    let node = syn::parse_str::<syn::Expr>(text).ok().map(|expr| semilla_syntax::tree::expr_node("x", &expr));
    node.map(|n| canonical(&n)).unwrap_or_else(|| text.to_string())
}

/// A text form without parentheses: the kinds, attributes, and leaf texts.
fn canonical(node: &Node) -> String {
    if node.children.is_empty() {
        // A leaf inside parentheses keeps them in its text; the parentheses do not count.
        let mut text = node.text.trim();
        while let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            text = inner.trim();
        }
        return text.to_string();
    }
    let children: Vec<String> = node.children.iter().map(|c| format!("{}={}", c.role, canonical(c))).collect();
    format!("{}[{}]({})", node.kind, node.attr.as_deref().unwrap_or(""), children.join(","))
}

/// Finds all sites in a node tree that match the pattern.
fn sites<'a>(pattern: &Node, root: &'a Node, out: &mut Vec<(Vec<String>, &'a Node, BTreeMap<String, String>)>) {
    let mut path = Vec::new();
    root.walk(&mut path, &mut |path, node| {
        let mut bindings = BTreeMap::new();
        if matches(pattern, node, &mut bindings) {
            out.push((path.to_vec(), node, bindings));
        }
    });
}

fn to_anchor(item: &str, path: &[String]) -> Anchor {
    let text = std::iter::once(item.to_string()).chain(path.iter().cloned()).collect::<Vec<_>>().join("/");
    text.parse().unwrap_or_else(|_| Anchor::item(item))
}

pub fn check(context: &Context, claim: &Claim) -> Outcome {
    let (Some(from), Some(to)) = (claim.arg_str("from"), claim.arg_str("to")) else {
        return outcome(ClaimStatus::Malformed { reason: "`rewrite-pattern` needs `from` and `to`".into() }, Vec::new());
    };
    let Some(pattern) = parse_pattern(from) else {
        return outcome(ClaimStatus::Malformed { reason: format!("`{from}` is not an expression pattern") }, Vec::new());
    };
    if parse_pattern(to).is_none() {
        return outcome(ClaimStatus::Malformed { reason: format!("`{to}` is not an expression pattern") }, Vec::new());
    }
    // The scope: one item (the anchor), or the whole workspace.
    let scope = claim.at.as_ref().map(|a| a.item.clone());
    let all_sites = claim.arg_str("sites").is_none_or(|sites| sites == "all");

    let mut explains = Vec::new();
    let mut rewritten = Vec::new();
    for (index, edit) in context.edits.iter().enumerate() {
        let (Some(before_anchor), Some(after_anchor)) = (&edit.before, &edit.after) else { continue };
        if scope.as_ref().is_some_and(|scope| &before_anchor.item != scope) {
            continue;
        }
        // Walk up from the edit to the site that matches the pattern.
        let mut site = before_anchor.clone();
        let mut after_site = after_anchor.clone();
        loop {
            if let Some(node) = semilla_syntax::resolve_node(context.before, &site) {
                let mut bindings = BTreeMap::new();
                if matches(&pattern, node, &mut bindings) {
                    let expected = instantiate(to, &bindings);
                    let actual = semilla_syntax::resolve_node(context.after, &after_site).map(canonical);
                    if expected.is_some() && expected == actual {
                        explains.push(index);
                        rewritten.push(site.clone());
                    }
                    break;
                }
            }
            match (site.parent(), after_site.parent()) {
                (Some(parent), Some(after_parent)) if !parent.path.is_empty() && !matches!(parent.path.last(), Some(Segment::Stmt(_))) || site.path.len() > 1 => {
                    site = parent;
                    after_site = after_parent;
                }
                _ => break,
            }
            if site.path.is_empty() {
                break;
            }
        }
    }
    if explains.is_empty() {
        return outcome(ClaimStatus::Empty, explains);
    }

    if all_sites {
        let mut missed = Vec::new();
        for item in context.before.items.values().filter(|item| scope.as_ref().is_none_or(|scope| &item.path == scope)) {
            let Some(body) = &item.body else { continue };
            let mut found = Vec::new();
            sites(&pattern, body, &mut found);
            for (path, _, _) in found {
                let anchor = to_anchor(&item.path, &path);
                if !rewritten.iter().any(|done| done == &anchor || done.contains(&anchor) || anchor.contains(done)) {
                    missed.push(anchor.to_string());
                }
            }
        }
        if !missed.is_empty() {
            let mut result = outcome(ClaimStatus::Refuted { reason: format!("{} site(s) match `{from}` but were not rewritten: {}", missed.len(), missed.join(", ")) }, Vec::new());
            result.hint = Some("Rewrite the missed sites, or use `sites: [..]` with the exact list.".into());
            return result;
        }
    }
    let mut result = outcome(ClaimStatus::Verified { proof: format!("pattern ({} sites)", rewritten.len()) }, explains);
    result.before = Some(from.to_string());
    result.after = Some(to.to_string());
    result
}

fn outcome(status: ClaimStatus, explains: Vec<usize>) -> Outcome {
    Outcome {
        status,
        explains,
        class: None,
        effect: None,
        witness: None,
        hint: None,
        before: None,
        after: None,
        needs_body_proof: false,
        self_evident: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_binds_metavariables() {
        let pattern = parse_pattern("$c.total()").unwrap();
        let expr: syn::Expr = syn::parse_str("cart.items.total()").unwrap();
        let node = semilla_syntax::tree::expr_node("x", &expr);
        let mut bindings = BTreeMap::new();
        assert!(matches(&pattern, &node, &mut bindings));
        assert_eq!(bindings["c"], "cart . items");
        assert_eq!(instantiate("$c.total()?", &bindings).unwrap(), strip_parens("cart.items.total()?"));
    }

    #[test]
    fn same_name_needs_same_text() {
        let pattern = parse_pattern("$a + $a").unwrap();
        let yes = semilla_syntax::tree::expr_node("x", &syn::parse_str("x + x").unwrap());
        let no = semilla_syntax::tree::expr_node("x", &syn::parse_str("x + y").unwrap());
        assert!(matches(&pattern, &yes, &mut BTreeMap::new()));
        assert!(!matches(&pattern, &no, &mut BTreeMap::new()));
    }
}

