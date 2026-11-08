//! Search (design section 11.7).
//!
//! | Query | Result |
//! |---|---|
//! | `total` | fuzzy search on symbol names |
//! | `"qty <= max"` | text search on labels and source |
//! | `changed:`, `class:logic`, `step:3`, `group:G2` | filters by diff status |
//! | `kind:panic`, `kind:unsafe`, `kind:err` | panic sites, `unsafe` blocks, error exits |
//! | `callers:Cart::total`, `callees:…` | graph neighbors |
//! | `path:main -> Cart::total` | call paths |
//!
//! All the terms of a query must match (AND). `path:` takes the rest of the query.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use semilla_core::Class;
use semilla_core::view::Status;

use crate::ctx::{Ctx, contains, item_of, last_segment};
use crate::graph;

/// The maximum number of results.
pub const MAX_RESULTS: usize = 100;

/// The kind of a code site for `kind:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteKind {
    Panic,
    Unsafe,
    Err,
}

/// One term of a query.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "term", content = "value", rename_all = "snake_case")]
pub enum Term {
    Fuzzy(String),
    Text(String),
    /// `changed:` with an optional node kind, for example `changed:fn`.
    Changed(Option<String>),
    Class(Class),
    Step(usize),
    Group(String),
    Kind(SiteKind),
    Callers(String),
    Callees(String),
    Path {
        from: String,
        to: String,
    },
}

/// A parsed query.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Query {
    pub terms: Vec<Term>,
    pub errors: Vec<String>,
}

const KEYS: &[&str] = &["changed", "class", "step", "group", "kind", "callers", "callees", "path"];

/// Parses a query. Unknown `key:` prefixes are plain words, so `Cart::total` is a symbol.
pub fn parse(text: &str) -> Query {
    let mut query = Query::default();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index].is_whitespace() {
            index += 1;
            continue;
        }
        if chars[index] == '"' {
            let (value, next, closed) = read_quoted(&chars, index + 1);
            if !closed {
                query.errors.push("a quote is not closed".to_string());
            }
            if !value.is_empty() {
                query.terms.push(Term::Text(value));
            }
            index = next;
            continue;
        }
        let start = index;
        let mut token = String::new();
        while index < chars.len() && !chars[index].is_whitespace() {
            if chars[index] == '"' {
                let (value, next, closed) = read_quoted(&chars, index + 1);
                if !closed {
                    query.errors.push("a quote is not closed".to_string());
                }
                token.push_str(&value);
                index = next;
            } else {
                token.push(chars[index]);
                index += 1;
            }
        }
        let (key, value) = match token.split_once(':') {
            Some((key, value)) if KEYS.contains(&key) && !value.starts_with(':') => (key, value.to_string()),
            _ => {
                query.terms.push(Term::Fuzzy(token));
                continue;
            }
        };
        match key {
            "path" => {
                let rest: String = chars[start + "path:".len()..].iter().collect();
                index = chars.len();
                match rest.split_once("->") {
                    Some((from, to)) if !from.trim().is_empty() && !to.trim().is_empty() => {
                        query.terms.push(Term::Path { from: unquote(from), to: unquote(to) })
                    }
                    _ => query.errors.push("path: needs the form `path:A -> B`".to_string()),
                }
            }
            "changed" => query.terms.push(Term::Changed((!value.is_empty()).then_some(value))),
            "class" => match parse_class(&value) {
                Some(class) => query.terms.push(Term::Class(class)),
                None => query.errors.push(format!("unknown class `{value}`: use neutral, logic, ctrl_flow, or other")),
            },
            "step" => match value.parse() {
                Ok(step) => query.terms.push(Term::Step(step)),
                Err(_) => query.errors.push(format!("step: needs a number, not `{value}`")),
            },
            "group" if !value.is_empty() => query.terms.push(Term::Group(value)),
            "kind" => match value.to_ascii_lowercase().as_str() {
                "panic" => query.terms.push(Term::Kind(SiteKind::Panic)),
                "unsafe" => query.terms.push(Term::Kind(SiteKind::Unsafe)),
                "err" | "error" => query.terms.push(Term::Kind(SiteKind::Err)),
                _ => query.errors.push(format!("unknown kind `{value}`: use panic, unsafe, or err")),
            },
            "callers" if !value.is_empty() => query.terms.push(Term::Callers(value)),
            "callees" if !value.is_empty() => query.terms.push(Term::Callees(value)),
            _ => query.errors.push(format!("{key}: needs a value")),
        }
    }
    query
}

fn read_quoted(chars: &[char], mut index: usize) -> (String, usize, bool) {
    let mut value = String::new();
    while index < chars.len() {
        match chars[index] {
            '"' => return (value, index + 1, true),
            '\\' if index + 1 < chars.len() => {
                value.push(chars[index + 1]);
                index += 2;
            }
            c => {
                value.push(c);
                index += 1;
            }
        }
    }
    (value, index, false)
}

fn unquote(text: &str) -> String {
    text.trim().trim_matches('"').trim().to_string()
}

fn parse_class(text: &str) -> Option<Class> {
    match text.to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
        "neutral" => Some(Class::Neutral),
        "logic" => Some(Class::Logic),
        "control_flow" | "ctrl_flow" | "ctrl" | "controlflow" | "cf" => Some(Class::ControlFlow),
        "other" => Some(Class::Other),
        _ => None,
    }
}

/// One search result: a node of the graph, the change tree, a flow view, or the sources.
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub id: String,
    pub label: String,
    /// The graph node kind (`function`, `crate`, …), or `statement` or `item` for tree nodes.
    pub kind: String,
    pub status: Option<Status>,
    pub class: Option<Class>,
    /// The claims on this node and inside it.
    pub claims: Vec<String>,
    pub score: u32,
    /// Why the node matches, for example `name`, `source after`, or `label`.
    pub matched: Vec<String>,
    /// The function of the flow view that shows this node.
    pub flow: Option<String>,
}

/// The search response.
#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub query: Query,
    pub total: usize,
    pub results: Vec<Hit>,
    pub paths: Vec<Vec<String>>,
    pub truncated: bool,
}

/// A node that search can find.
struct Entity {
    id: String,
    label: String,
    kind: String,
    status: Option<Status>,
    class: Option<Class>,
    labels: Vec<String>,
    tree_claims: Vec<String>,
    badges: Vec<String>,
}

fn changed(status: Option<Status>) -> bool {
    status.is_some_and(|status| status != Status::Same)
}

fn entities(ctx: &Ctx) -> BTreeMap<String, Entity> {
    fn add<'a>(out: &'a mut BTreeMap<String, Entity>, id: &str, label: &str, kind: &str, status: Option<Status>) -> &'a mut Entity {
        let entity = out.entry(id.to_string()).or_insert_with(|| Entity {
            id: id.to_string(),
            label: label.to_string(),
            kind: kind.to_string(),
            status: None,
            class: None,
            labels: Vec::new(),
            tree_claims: Vec::new(),
            badges: Vec::new(),
        });
        if !label.is_empty() && !entity.labels.iter().any(|known| known == label) {
            entity.labels.push(label.to_string());
        }
        if entity.status.is_none() || (!changed(entity.status) && changed(status)) {
            entity.status = status.or(entity.status);
        }
        entity
    }
    let mut out: BTreeMap<String, Entity> = BTreeMap::new();
    for node in &ctx.data.graph.nodes {
        let kind = serde_json::to_value(node.kind).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_default();
        let entity = add(&mut out, &node.id, &node.label, &kind, Some(node.status));
        entity.badges = node.badges.clone();
    }
    let mut stack = vec![&ctx.data.tree];
    while let Some(node) = stack.pop() {
        let kind = if node.id.contains('/') { "statement" } else { "item" };
        let entity = add(&mut out, &node.id, &node.label, kind, Some(node.status));
        entity.class = entity.class.or(node.class);
        if let Some(diff) = &node.diff {
            entity.labels.push(diff.clone());
        }
        entity.tree_claims.extend(node.claims.iter().cloned());
        stack.extend(node.children.iter());
    }
    for flow in &ctx.data.flows {
        for node in &flow.nodes {
            let id = node.anchor.clone().unwrap_or_else(|| flow.function.clone());
            let kind = if id.contains('/') { "statement" } else { "item" };
            add(&mut out, &id, &node.label, kind, None);
        }
    }
    for key in ctx.data.sources.keys() {
        let kind = if key.contains('/') { "statement" } else { "item" };
        add(&mut out, key, last_segment(key), kind, None);
    }
    for (_, claim) in ctx.all_claims() {
        if let Some(at) = &claim.at {
            let id = at.to_string();
            let kind = if id.contains('/') { "statement" } else { "item" };
            add(&mut out, &id, last_segment(&id), kind, None);
        }
    }
    out
}

/// The fuzzy score of a symbol name: exact, suffix, prefix, substring, then subsequence.
pub fn fuzzy_score(query: &str, id: &str) -> u32 {
    let query = query.to_lowercase();
    let full = item_of(id).to_lowercase();
    let name = last_segment(&full).to_string();
    if query.is_empty() {
        return 0;
    }
    if name == query {
        return 100;
    }
    if full == query || full.ends_with(&format!("::{query}")) {
        return 95;
    }
    if name.starts_with(&query) {
        return 80;
    }
    if name.contains(&query) {
        return 60;
    }
    if full.contains(&query) {
        return 50;
    }
    // A subsequence match: all the characters of the query, in order.
    let mut chars = full.chars();
    let mut matched = 0;
    for wanted in query.chars() {
        if chars.by_ref().any(|c| c == wanted) {
            matched += 1;
        } else {
            return 0;
        }
    }
    10 + (matched * 20 / full.chars().count().max(1)) as u32
}

/// The source text for the `kind:` checks: the after version, else the before version.
fn site_text<'a>(ctx: &'a Ctx, entity: &'a Entity) -> Vec<(&'static str, &'a str)> {
    let mut texts = Vec::new();
    if let Some(pair) = ctx.data.sources.get(&entity.id) {
        match (&pair.after, &pair.before) {
            (Some(after), _) => texts.push(("source after", after.as_str())),
            (None, Some(before)) => texts.push(("source before", before.as_str())),
            _ => {}
        }
    }
    for label in &entity.labels {
        texts.push(("label", label.as_str()));
    }
    texts
}

/// True if the text has a site of this kind.
pub fn has_site(kind: SiteKind, text: &str) -> bool {
    match kind {
        SiteKind::Panic => ["unwrap(", "expect(", "panic!", "unreachable!", "todo!", "unimplemented!"].iter().any(|p| text.contains(p)),
        SiteKind::Unsafe => text.contains("unsafe"),
        SiteKind::Err => text.contains("Err(") || has_try(text),
    }
}

/// True if the text has a `?` operator: a `?` after an identifier, `)`, or `]`, outside a
/// string literal.
fn has_try(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if in_string {
            match c {
                '\\' => index += 1,
                '"' => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '?'
            && index > 0
            && (chars[index - 1].is_alphanumeric() || matches!(chars[index - 1], '_' | ')' | ']'))
            && chars.get(index + 1).is_none_or(|next| !next.is_alphanumeric())
        {
            return true;
        }
        index += 1;
    }
    false
}

/// Runs a query.
pub fn search(ctx: &Ctx, text: &str) -> SearchResult {
    let mut query = parse(text);
    let mut paths = Vec::new();
    let mut truncated = false;

    // Resolve the graph terms first.
    let mut allowed: Option<BTreeSet<String>> = None;
    let restrict = |set: BTreeSet<String>, allowed: &mut Option<BTreeSet<String>>| {
        *allowed = Some(match allowed.take() {
            Some(old) => old.intersection(&set).cloned().collect(),
            None => set,
        });
    };
    let only_paths = !query.terms.is_empty() && query.terms.iter().all(|term| matches!(term, Term::Path { .. }));
    for term in query.terms.clone() {
        let (callers, symbol) = match term {
            Term::Callers(symbol) => (true, symbol),
            Term::Callees(symbol) => (false, symbol),
            Term::Path { from, to } => {
                match (graph::resolve(ctx, &from), graph::resolve(ctx, &to)) {
                    (Some(from), Some(to)) => {
                        let (found, cut) = graph::call_paths(ctx, &from, &to);
                        paths.extend(found);
                        truncated |= cut;
                    }
                    (from_id, _) => {
                        let missing = if from_id.is_none() { from } else { to };
                        query.errors.push(format!("unknown symbol `{missing}`"));
                    }
                }
                continue;
            }
            _ => continue,
        };
        match graph::resolve(ctx, &symbol) {
            Some(id) => {
                let map = if callers { &ctx.callers } else { &ctx.callees };
                let set = map.get(&id).cloned().unwrap_or_default().into_iter().collect();
                restrict(set, &mut allowed);
            }
            None => {
                query.errors.push(format!("unknown symbol `{symbol}`"));
                restrict(BTreeSet::new(), &mut allowed);
            }
        }
    }

    let mut hits = Vec::new();
    if !only_paths && !query.terms.is_empty() {
        let claims: Vec<(String, String, usize, Option<Class>)> = ctx
            .all_claims()
            .filter_map(|(step, claim)| claim.at.as_ref().map(|at| (claim.id.clone(), at.to_string(), step.index, ctx.claim_class(claim))))
            .collect();
        for entity in entities(ctx).into_values() {
            if let Some(allowed) = &allowed
                && !allowed.contains(&entity.id)
            {
                continue;
            }
            let mut entity_claims: Vec<String> = entity.tree_claims.clone();
            let mut classes: Vec<Option<Class>> = entity.class.into_iter().map(Some).collect();
            let mut steps = BTreeSet::new();
            for (id, anchor, step, class) in &claims {
                if contains(&entity.id, anchor) || entity_claims.contains(id) {
                    if !entity_claims.contains(id) {
                        entity_claims.push(id.clone());
                    }
                    classes.push(*class);
                    steps.insert(*step);
                }
            }
            for id in &entity.tree_claims {
                if let Some(step) = ctx.claim_step.get(id) {
                    steps.insert(*step);
                }
            }
            let mut score = 0;
            let mut matched = Vec::new();
            let mut ok = true;
            for term in &query.terms {
                let pass = match term {
                    Term::Fuzzy(word) => {
                        let value = if entity.id.contains('/') { 0 } else { fuzzy_score(word, &entity.id) };
                        score += value;
                        if value > 0 {
                            matched.push("name".to_string());
                        }
                        value > 0
                    }
                    Term::Text(needle) => {
                        let needle = needle.to_lowercase();
                        let mut found = false;
                        for label in &entity.labels {
                            if label.to_lowercase().contains(&needle) {
                                found = true;
                                matched.push("label".to_string());
                                break;
                            }
                        }
                        if let Some(pair) = ctx.data.sources.get(&entity.id) {
                            for (name, text) in [("source before", &pair.before), ("source after", &pair.after)] {
                                if text.as_deref().is_some_and(|text| text.to_lowercase().contains(&needle)) {
                                    found = true;
                                    matched.push(name.to_string());
                                }
                            }
                        }
                        if found {
                            score += 40;
                        }
                        found
                    }
                    Term::Changed(kind) => {
                        let is_changed = changed(entity.status) || !entity_claims.is_empty();
                        is_changed && kind.as_deref().is_none_or(|kind| kind_matches(kind, &entity.kind))
                    }
                    Term::Class(class) => classes.contains(&Some(*class)),
                    Term::Step(step) => steps.contains(step),
                    Term::Group(group) => {
                        steps.iter().any(|step| ctx.step_group.get(step).is_some_and(|owner| owner.eq_ignore_ascii_case(group)))
                    }
                    Term::Kind(kind) => {
                        let badge = match kind {
                            SiteKind::Panic => "never",
                            SiteKind::Unsafe => "unsafe",
                            SiteKind::Err => "err",
                        };
                        let found = entity.badges.iter().any(|known| known == badge)
                            || site_text(ctx, &entity).iter().any(|(name, text)| {
                                let hit = has_site(*kind, text);
                                if hit {
                                    matched.push(name.to_string());
                                }
                                hit
                            });
                        found
                    }
                    Term::Callers(_) | Term::Callees(_) => {
                        matched.push("graph".to_string());
                        true
                    }
                    Term::Path { .. } => true,
                };
                if !pass {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            matched.dedup();
            hits.push(Hit {
                flow: ctx.flow_function(&entity.id),
                id: entity.id,
                label: entity.label,
                kind: entity.kind,
                status: entity.status,
                class: entity.class,
                claims: entity_claims,
                score,
                matched,
            });
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(changed(b.status).cmp(&changed(a.status)))
            .then(kind_rank(&a.kind).cmp(&kind_rank(&b.kind)))
            .then(a.id.cmp(&b.id))
    });
    let total = hits.len();
    hits.truncate(MAX_RESULTS);
    SearchResult { query, total, results: hits, paths, truncated: truncated || total > MAX_RESULTS }
}

fn kind_matches(wanted: &str, kind: &str) -> bool {
    match wanted.to_ascii_lowercase().as_str() {
        "fn" | "function" => kind == "function",
        "mod" | "module" => kind == "module",
        "crate" => kind == "crate",
        "type" | "struct" | "enum" => kind == "type",
        "stmt" | "statement" => kind == "statement",
        other => kind == other,
    }
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "function" => 0,
        "type" | "trait" | "impl" => 1,
        "item" => 2,
        "statement" => 3,
        "module" => 4,
        "crate" => 5,
        _ => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_terms() {
        let query = parse(r#"total "qty <= max" class:logic step:3 group:G2 kind:panic callers:Cart::total changed:"#);
        assert!(query.errors.is_empty(), "{:?}", query.errors);
        assert_eq!(
            query.terms,
            vec![
                Term::Fuzzy("total".into()),
                Term::Text("qty <= max".into()),
                Term::Class(Class::Logic),
                Term::Step(3),
                Term::Group("G2".into()),
                Term::Kind(SiteKind::Panic),
                Term::Callers("Cart::total".into()),
                Term::Changed(None),
            ]
        );
    }

    #[test]
    fn parse_symbols_and_paths() {
        assert_eq!(parse("Cart::total").terms, vec![Term::Fuzzy("Cart::total".into())]);
        assert_eq!(parse("path:main -> Cart::total").terms, vec![Term::Path { from: "main".into(), to: "Cart::total".into() }]);
        assert!(!parse("path:main").errors.is_empty());
        assert!(!parse("class:huge").errors.is_empty());
        assert!(!parse("step:x").errors.is_empty());
        assert!(!parse("kind:leak").errors.is_empty());
    }

    #[test]
    fn site_checks() {
        assert!(has_site(SiteKind::Panic, "let x = y.unwrap();"));
        assert!(!has_site(SiteKind::Panic, "let x = y.unwrap_or(0);"));
        assert!(has_site(SiteKind::Err, "let x = load(path)?;"));
        assert!(has_site(SiteKind::Err, "return Err(e)"));
        assert!(!has_site(SiteKind::Err, "println!(\"why?\")"));
        assert!(has_site(SiteKind::Unsafe, "unsafe { f() }"));
    }

    #[test]
    fn fuzzy_order() {
        assert!(fuzzy_score("total", "a::Cart::total") > fuzzy_score("tot", "a::Cart::total"));
        assert!(fuzzy_score("tot", "a::Cart::total") > fuzzy_score("ctl", "a::Cart::total"));
        assert!(fuzzy_score("ctl", "a::Cart::total") > 0);
        assert_eq!(fuzzy_score("zzz", "a::Cart::total"), 0);
    }
}
