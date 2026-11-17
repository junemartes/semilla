//! `rewrite-pattern`: one rule that explains many sites.
//!
//! A pattern is a Rust expression with metavariables, for example `$c.total()`. A metavariable
//! matches any expression; the same name must match the same text. The verifier explains each
//! edit whose site matches `from` before and equals `to` (with the same bindings) after. With
//! `sites: "all"` (the default), every site that matches `from` in the scope must be rewritten.

use std::collections::BTreeMap;

use semilla_core::{Anchor, Claim, ClaimStatus, Segment};
use semilla_syntax::Node;

use crate::ExprOracle;
use crate::checks::{Context, Outcome};

const META: &str = "__semilla_meta_";

/// Parses a pattern into a node tree. `$name` becomes a special identifier.
fn parse_pattern(text: &str) -> Option<Node> {
    let mut source = String::new();
    let mut chars = text.chars().peekable();
    for c in chars.by_ref() {
        if c == '$' {
            source.push_str(META);
        } else {
            source.push(c);
        }
    }
    let _ = chars.peek();
    semilla_syntax::tree::expr_text_node("pattern", &source)
}

/// Matches a node against a pattern, and records the bindings.
fn matches(pattern: &Node, node: &Node, bindings: &mut BTreeMap<String, String>) -> bool {
    if pattern.kind == "path" && pattern.text.starts_with(META) && !pattern.text.contains(' ') {
        let name = pattern.text.trim_start_matches(META).to_string();
        return if let Some(bound) = bindings.get(&name) {
            bound == &node.text
        } else {
            bindings.insert(name, node.text.clone());
            true
        };
    }
    if pattern.kind != node.kind || pattern.attr != node.attr || pattern.children.len() != node.children.len() {
        return false;
    }
    if pattern.children.is_empty() {
        return pattern.text == node.text;
    }
    pattern
        .children
        .iter()
        .zip(&node.children)
        .all(|(p, n)| p.role == n.role && matches(p, n, bindings))
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
    let expr = semilla_syntax::rs::parse_expr(&text)?;
    Some(strip_parens(&semilla_syntax::rs::compare(&expr)))
}

/// Removes the parentheses that `instantiate` adds around single tokens, so the text compares.
fn strip_parens(text: &str) -> String {
    let node = semilla_syntax::tree::expr_text_node("x", text);
    node.map_or_else(|| text.to_string(), |n| canonical(&n))
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
    let text = std::iter::once(item.to_string())
        .chain(path.iter().cloned())
        .collect::<Vec<_>>()
        .join("/");
    text.parse().unwrap_or_else(|_| Anchor::item(item))
}

pub fn check<E: ExprOracle>(context: &Context<'_, E>, claim: &Claim) -> Outcome {
    let (Some(from), Some(to)) = (claim.arg_str("from"), claim.arg_str("to")) else {
        return outcome(
            ClaimStatus::Malformed {
                reason: "`rewrite-pattern` needs `from` and `to`".into(),
            },
            Vec::new(),
        );
    };
    let Some(pattern) = parse_pattern(from) else {
        return outcome(
            ClaimStatus::Malformed {
                reason: format!("`{from}` is not an expression pattern"),
            },
            Vec::new(),
        );
    };
    if parse_pattern(to).is_none() {
        return outcome(
            ClaimStatus::Malformed {
                reason: format!("`{to}` is not an expression pattern"),
            },
            Vec::new(),
        );
    }
    let rewrite = Rewrite {
        context,
        pattern,
        to,
        scope: claim.at.as_ref().map(|a| a.item.clone()),
    };
    // `sites`: `all` (the default), or the exact list of the site anchors.
    let listed: Option<Vec<String>> = match claim.args.get("sites") {
        None => None,
        Some(serde_json::Value::String(text)) if text == "all" => None,
        Some(_) => Some(claim.arg_list("sites")),
    };
    let all_sites = listed.is_none();

    let (explains, rewritten): (Vec<usize>, Vec<Anchor>) = context
        .edits
        .iter()
        .enumerate()
        .filter_map(|(index, edit)| Some((index, rewrite.site(edit)?)))
        .unzip();
    if explains.is_empty() {
        return outcome(ClaimStatus::Empty, explains);
    }

    if all_sites {
        let missed = rewrite.missed(&rewritten);
        if !missed.is_empty() {
            let mut result = outcome(
                ClaimStatus::Refuted {
                    reason: format!("{} site(s) match `{from}` but were not rewritten: {}", missed.len(), missed.join(", ")),
                },
                Vec::new(),
            );
            result.hint = Some("Rewrite the missed sites, or use `sites: [..]` with the exact list.".into());
            return result;
        }
    }
    if let Some(listed) = &listed {
        let mut done: Vec<String> = rewritten.iter().map(std::string::ToString::to_string).collect();
        let mut wanted = listed.clone();
        done.sort();
        wanted.sort();
        if done != wanted {
            return outcome(
                ClaimStatus::Refuted {
                    reason: format!("`sites` lists {}, but the rewritten sites are {}", wanted.join(", "), done.join(", ")),
                },
                Vec::new(),
            );
        }
    }
    let mut result = outcome(
        ClaimStatus::Verified {
            proof: format!("pattern ({} sites)", rewritten.len()),
        },
        explains,
    );
    result.before = Some(from.to_string());
    result.after = Some(to.to_string());
    result
}

/// The parts of a `rewrite-pattern` claim: the pattern, the text after the rewrite, and the
/// scope.
struct Rewrite<'c, 'a, E: ExprOracle> {
    context: &'c Context<'a, E>,
    pattern: Node,
    to: &'c str,
    /// The scope: an item, or a module or a crate (all its items). `None`: the whole
    /// workspace.
    scope: Option<String>,
}

impl<E: ExprOracle> Rewrite<'_, '_, E> {
    fn in_scope(&self, item: &str) -> bool {
        self.scope
            .as_ref()
            .is_none_or(|scope| item == scope || item.starts_with(&format!("{scope}::")))
    }

    /// The site of an edit that the claim rewrites: the edit walks up to the site that matches
    /// the pattern, and the site after the step must be the instance of `to`.
    fn site(&self, edit: &semilla_syntax::Edit) -> Option<Anchor> {
        let (Some(before_anchor), Some(after_anchor)) = (&edit.before, &edit.after) else {
            return None;
        };
        if !self.in_scope(&before_anchor.item) {
            return None;
        }
        let mut site = before_anchor.clone();
        let mut after_site = after_anchor.clone();
        loop {
            if let Some(found) = self.matched(&site, &after_site) {
                return found.then_some(site);
            }
            match (site.parent(), after_site.parent()) {
                (Some(parent), Some(after_parent))
                    if !parent.path.is_empty() && !matches!(parent.path.last(), Some(Segment::Stmt(_))) || site.path.len() > 1 =>
                {
                    site = parent;
                    after_site = after_parent;
                }
                _ => return None,
            }
            if site.path.is_empty() {
                return None;
            }
        }
    }

    /// `None` if the site does not match the pattern. Otherwise true if the site after the
    /// step is the instance of `to`.
    fn matched(&self, site: &Anchor, after_site: &Anchor) -> Option<bool> {
        let node = semilla_syntax::resolve_node(self.context.before, site)?;
        let mut bindings = BTreeMap::new();
        if !matches(&self.pattern, node, &mut bindings) {
            return None;
        }
        let expected = instantiate(self.to, &bindings);
        let actual = semilla_syntax::resolve_node(self.context.after, after_site).map(canonical);
        Some(expected.is_some() && expected == actual)
    }

    /// Each site in the scope that matches the pattern and that is not in `rewritten`.
    fn missed(&self, rewritten: &[Anchor]) -> Vec<String> {
        let found = self
            .context
            .before
            .items
            .values()
            .filter(|item| self.in_scope(&item.path))
            .flat_map(|item| {
                let mut found = Vec::new();
                if let Some(body) = &item.body {
                    sites(&self.pattern, body, &mut found);
                }
                found.into_iter().map(|(path, _, _)| to_anchor(&item.path, &path))
            });
        found
            .filter(|anchor| {
                !rewritten
                    .iter()
                    .any(|done| done == anchor || done.contains(anchor) || anchor.contains(done))
            })
            .map(|anchor| anchor.to_string())
            .collect()
    }
}

fn outcome(status: ClaimStatus, explains: Vec<usize>) -> Outcome {
    Outcome {
        status,
        explains,
        category: None,
        effect: None,
        witness: None,
        hint: None,
        before: None,
        after: None,
        self_evident: false,
        complete_calls: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_binds_metavariables() {
        let pattern = parse_pattern("$c.total()").unwrap();
        let node = semilla_syntax::tree::expr_text_node("x", "cart.items.total()").unwrap();
        let mut bindings = BTreeMap::new();
        assert!(matches(&pattern, &node, &mut bindings));
        assert_eq!(bindings["c"], "cart . items");
        assert_eq!(instantiate("$c.total()?", &bindings).unwrap(), strip_parens("cart.items.total()?"));
    }

    #[test]
    fn same_name_needs_same_text() {
        let pattern = parse_pattern("$a + $a").unwrap();
        let yes = semilla_syntax::tree::expr_text_node("x", "x + x").unwrap();
        let no = semilla_syntax::tree::expr_text_node("x", "x + y").unwrap();
        assert!(matches(&pattern, &yes, &mut BTreeMap::new()));
        assert!(!matches(&pattern, &no, &mut BTreeMap::new()));
    }
}
