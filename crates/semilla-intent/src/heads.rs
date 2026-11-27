//! The meaning of the claim attributes (design 78, R78.3-2): the errors of Semilla, each with a
//! code (`S0101`), after the errors of rustc.
//!
//! Rustc checks the names, the types, and the scope paths of each claim (the intent crate,
//! design 78). This module reads only the head of each claim (the attribute, its options, and
//! the signature) and checks what rustc cannot know: the meaning of the attribute.

use ra_ap_syntax::ast::{self, HasAttrs, HasModuleItem, HasName};
use ra_ap_syntax::{AstNode, Edition, TextRange};
use serde::Serialize;

/// The kind of the scope of a claim attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One or more items (`shop::parse`, `impl shop::Cart`, `callers(shop::parse)`).
    Items,
    /// Exactly two paths (`rename(A, B)`, `relation(before::T, after::T)`).
    Two,
    /// No scope (`lemma`), or options only (`loop_relation(at = …)`), or a text (`assume`).
    None,
}

/// The form of the body of a claim attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// A property: a body that returns `bool`.
    Property,
    /// An empty body or a property (`same`).
    EmptyOrProperty,
    /// An empty body (a hint for the pairing).
    Empty,
}

/// One attribute of the language.
#[derive(Clone, Copy, Debug)]
pub struct Kind {
    pub name: &'static str,
    pub scope: Scope,
    pub body: Body,
    /// The batch that checks the kind, if the model cannot check it now (design 78, part D).
    pub later: Option<&'static str>,
}

/// The attributes of the language (draft 3 of the intent language). The crate
/// `semilla-intent-macros` defines one attribute for each row (a test checks it).
pub const KINDS: &[Kind] = &[
    Kind { name: "same", scope: Scope::Items, body: Body::EmptyOrProperty, later: None },
    Kind { name: "change", scope: Scope::Items, body: Body::Property, later: None },
    Kind { name: "different", scope: Scope::Items, body: Body::Property, later: None },
    Kind { name: "new", scope: Scope::Items, body: Body::Property, later: Some("13d") },
    Kind { name: "removed", scope: Scope::Items, body: Body::Property, later: Some("13d") },
    Kind { name: "invariant", scope: Scope::Items, body: Body::Property, later: Some("13d") },
    Kind { name: "relation", scope: Scope::Two, body: Body::Property, later: None },
    Kind { name: "contract", scope: Scope::Items, body: Body::Property, later: Some("13d") },
    Kind { name: "lemma", scope: Scope::None, body: Body::Property, later: Some("13d") },
    Kind { name: "loop_relation", scope: Scope::None, body: Body::Property, later: Some("13d") },
    Kind { name: "assume", scope: Scope::None, body: Body::Property, later: Some("13d") },
    Kind { name: "rename", scope: Scope::Two, body: Body::Empty, later: None },
    Kind { name: "moved", scope: Scope::Two, body: Body::Empty, later: None },
    Kind { name: "rewrite", scope: Scope::Two, body: Body::Empty, later: None },
];

/// The options of each claim attribute (`loop_relation` has `at` too).
const OPTIONS: [&str; 3] = ["cfg", "reason", "except"];

/// One code of Semilla: the code, the short title, and the long text with an example.
pub struct Code {
    pub code: &'static str,
    pub title: &'static str,
    pub long: &'static str,
}

/// The codes of Semilla for the intent files (`semilla intent explain CODE`).
pub const CODES: &[Code] = &[
    Code { code: "S0101", title: "an item that an intent file cannot have", long: "An intent file has claims (a function with a claim attribute), relations (a function with no attribute), `use` items, `const` items, and `intent!(\"TEXT\")`. A type, an `impl`, a trait, a `static`, a module, or a macro definition belongs to the code of the user, not to an intent file.\n\nWrong:\n\n    struct Helper;\n\nRight: name the type of a version, `after::shop::Cart`." },
    Code { code: "S0102", title: "a claim attribute with no scope", long: "A `same`, `change`, `different`, `new`, `removed`, `invariant`, or `contract` claim names its scope: an item (`shop::parse`), a list, `impl shop::Cart`, `shop::fmt::*`, or `callers(shop::parse)`.\n\nWrong:\n\n    #[same]\n    fn f() {}\n\nRight:\n\n    #[same(shop::parse)]\n    fn f() {}" },
    Code { code: "S0103", title: "an attribute that needs two paths", long: "`rename(A, B)`, `moved(A, B)`, `rewrite(A, B)`, and `relation(before::T, after::T)` have two arguments: the old one first, the new one second.\n\nRight:\n\n    #[relation(before::shop::Cart, after::shop::Cart)]\n    fn cart(b: &before::shop::Cart, a: &after::shop::Cart) -> bool { a.len == b.items.len() }" },
    Code { code: "S0104", title: "a `different` claim with parameters", long: "A `different` claim is one example input: the input is the literals of the body, so the function has no parameters.\n\nRight:\n\n    #[different(shop::label)]\n    fn large_price() -> bool { before::shop::label(1_000_000) != after::shop::label(1_000_000) }" },
    Code { code: "S0105", title: "a property claim with an empty body", long: "Only `same` (the full compare of its scope) and the hints `rename`, `moved`, `rewrite` can have an empty body. Each other claim is a property: a body that returns `bool`, true for each value of the parameters.\n\nRight:\n\n    #[change(shop::label)]\n    fn group_mark(cents: u32) -> bool { cents < 1_000_000 || after::shop::label(cents).contains(',') }" },
    Code { code: "S0106", title: "a property with no `-> bool`", long: "The body of a property claim is a `bool` expression, so the function returns `bool`. The fix adds `-> bool`.\n\nRight:\n\n    #[same(shop::parse)]\n    fn parse(x: u32) -> bool { before::shop::parse(x) == after::shop::parse(x) }" },
    Code { code: "S0107", title: "an option that the attribute does not have", long: "The options of a claim attribute are `cfg = \"PREDICATE\"` (the matching configurations only), `reason = \"TEXT\"` (shown), and `except = [NAME, …]` (an observation that the claim ignores). `loop_relation` has `at = PLACE` too.\n\nRight:\n\n    #[same(shop::parse, cfg = \"debug_assertions\", reason = \"the release build has no check\")]" },
    Code { code: "S0108", title: "an observation that the model cannot ignore yet", long: "`except = [panic_message]` is the one observation that the check can ignore now. Another name is in the language, but the model cannot check it yet: the claim is \"claimed\" with this line, never \"proved\"." },
    Code { code: "S0109", title: "a `cfg` predicate that does not parse", long: "A `cfg` option has the form of the Rust `cfg` attribute: a name, `key = \"value\"`, `all(…)`, `any(…)`, `not(…)`.\n\nRight:\n\n    #[same(shop::parse, cfg = \"all(unix, not(debug_assertions))\")]" },
    Code { code: "S0110", title: "a claim kind that the model cannot check yet", long: "The kind is in the language, but this version of Semilla cannot check it: the step shows the claim as \"claimed\" with the batch that checks it. It is not an error: the file is clean. Write the claim now; a later version checks it." },
    Code { code: "S0112", title: "a scope path with no definition", long: "Rustc resolves each path of a scope (also a method `shop::Bag::new` through its type, and an item of a re-export), and the path names no item of the version. The candidates are the names next to the last part that resolves.\n\nWrong:\n\n    #[same(shop::Bag::nw)]\n\nRight:\n\n    #[same(shop::Bag::new)]" },
    Code { code: "S0120", title: "an anchor of a claim that does not fit the syntax tree", long: "The step checks the anchors of each claim against the syntax trees of the two versions (the AST pass, design 79): the item that rustc resolves for the scope must have a node in the tree of its version, and its kind must fit the claim. `#[new(ITEM)]` needs an item that the step adds; `#[removed(ITEM)]` an item that the step removes; `#[same]`, `#[change]`, and `#[different]` one kind of item in the two versions; `#[relation(before::T, after::T)]` two types. The step shows the error at the place of the scope path, with the expected kind, and the claim is an error.\n\nWrong (the step does not add `shop::parse`):\n\n    #[new(shop::parse)]\n    fn parse_new(x: u32) -> bool { after::shop::parse(x) }\n\nRight:\n\n    #[change(shop::parse)]\n    fn parse_new(x: u32) -> bool { after::shop::parse(x) || !before::shop::parse(x) }" },
    Code { code: "S0111", title: "a hint with a body", long: "`rename`, `moved`, and `rewrite` are hints for the pairing of the old and the new code: the function has an empty body.\n\nRight:\n\n    #[rename(shop::total, shop::sum)]\n    fn renamed() {}" },
];

/// The long text of a code of Semilla.
pub fn explain(code: &str) -> Option<String> {
    CODES.iter().find(|row| row.code.eq_ignore_ascii_case(code)).map(|row| format!("{}: {}\n\n{}\n", row.code, row.title, row.long))
}

/// One finding of the check of the heads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub code: &'static str,
    /// True for an error; false for a line "the model cannot check this yet".
    pub error: bool,
    pub file: String,
    pub line: usize,
    pub column: usize,
    /// The byte range of the place in the file.
    pub start: usize,
    pub end: usize,
    pub message: String,
    /// The text to write, if the finding has one exact fix: the byte range and the text.
    pub fix: Option<(usize, usize, String)>,
}

/// The head of one claim (one attribute of one function).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Head {
    /// The name of the function.
    pub function: String,
    pub kind: String,
    /// The text of each argument of the attribute that is not an option.
    pub scope: Vec<String>,
    /// The place of each scope argument: the line, the column, and the byte range.
    pub scope_at: Vec<(usize, usize, usize, usize)>,
    pub cfg: Option<String>,
    pub reason: Option<String>,
    pub except: Vec<String>,
    pub empty_body: bool,
    pub params: usize,
    /// The text of each parameter: the name and the type.
    pub signature: Vec<(String, String)>,
    pub line: usize,
    /// The batch that checks the kind, if the model cannot check it now.
    pub later: Option<String>,
}

struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    fn new(text: &str) -> Lines {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        Lines { starts }
    }

    fn at(&self, text: &str, offset: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|start| *start <= offset).max(1);
        let start = self.starts[line - 1];
        (line, text.get(start..offset.min(text.len())).map(|part| part.chars().count()).unwrap_or(0) + 1)
    }
}

/// [R78.3-2] Reads the heads of the claims of one file and checks the meaning of each attribute.
/// A file that rustc cannot parse gives no finding here: rustc gives the error.
pub fn check(path: &str, text: &str) -> (Vec<Head>, Vec<Finding>) {
    let parse = ast::SourceFile::parse(text, Edition::Edition2024);
    let lines = Lines::new(text);
    let mut heads = Vec::new();
    let mut findings = Vec::new();
    let mut add = |code: &'static str, error: bool, range: TextRange, message: String, fix: Option<(usize, usize, String)>| {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let (line, column) = lines.at(text, start);
        findings.push(Finding { code, error, file: path.to_string(), line, column, start, end, message, fix });
    };
    if !parse.errors().is_empty() {
        return (heads, Vec::new());
    }
    for item in parse.tree().items() {
        let function = match item {
            ast::Item::Fn(function) => function,
            ast::Item::Use(_) | ast::Item::Const(_) | ast::Item::MacroCall(_) => continue,
            other => {
                let first = other.syntax().first_token().map(|token| token.text_range()).unwrap_or(other.syntax().text_range());
                add("S0101", true, first, "an intent file has claims, relations, `use` items, `const` items, and `intent!(\"TEXT\")` only".into(), None);
                continue;
            }
        };
        let name = function.name().map(|name| name.text().to_string()).unwrap_or_default();
        let body_empty = function.body().is_none_or(|body| body.statements().next().is_none() && body.tail_expr().is_none());
        let params = function.param_list().map(|list| list.params().count()).unwrap_or(0);
        let signature: Vec<(String, String)> = function.param_list().into_iter().flat_map(|list| list.params()).map(|param| (param.pat().map(|pat| pat.syntax().text().to_string()).unwrap_or_default(), param.ty().map(|ty| ty.syntax().text().to_string()).unwrap_or_default())).collect();
        let returns_bool = function.ret_type().and_then(|ret| ret.ty()).is_some_and(|ty| ty.syntax().text() == "bool");
        for attr in function.attrs() {
            let Some(path) = attr.path() else { continue };
            let attr_name = path.syntax().text().to_string();
            let attr_name = attr_name.rsplit("::").next().unwrap_or("").to_string();
            let Some(kind) = KINDS.iter().find(|kind| kind.name == attr_name) else { continue };
            let mut head = Head { function: name.clone(), kind: kind.name.to_string(), scope: Vec::new(), scope_at: Vec::new(), cfg: None, reason: None, except: Vec::new(), empty_body: body_empty, params, signature: signature.clone(), line: lines.at(text, usize::from(attr.syntax().text_range().start())).0, later: kind.later.map(str::to_string) };
            let args = attr.syntax().descendants().find_map(ast::TokenTree::cast).map(|tree| arguments(&tree)).unwrap_or_default();
            for (arg, range) in &args {
                match arg.split_once('=').map(|(key, value)| (key.trim(), value.trim())) {
                    Some((key, value)) if !value.starts_with('=') && !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_') => {
                        let key_range = TextRange::at(range.start(), ra_ap_syntax::TextSize::from(key.len() as u32));
                        match key {
                            "cfg" => {
                                let predicate = value.trim_matches('"').to_string();
                                if let Err(cause) = crate::cfg_holds(&predicate, &Default::default()) {
                                    add("S0109", true, *range, format!("the `cfg` predicate `{predicate}` does not parse: {cause}"), None);
                                }
                                head.cfg = Some(predicate);
                            }
                            "reason" => head.reason = Some(value.trim_matches('"').to_string()),
                            "except" => {
                                let names: Vec<String> = value.trim_start_matches('[').trim_end_matches(']').split(',').map(|name| name.trim().to_string()).filter(|name| !name.is_empty()).collect();
                                for name in &names {
                                    if !crate::prelude::EXCEPT.contains(&name.as_str()) {
                                        add("S0108", false, *range, format!("the model cannot ignore the observation `{name}` yet: the claim stays \"claimed\" (only `panic_message` now)"), None);
                                    }
                                }
                                head.except = names;
                            }
                            "at" if kind.name == "loop_relation" => {}
                            other => {
                                let best = OPTIONS.iter().min_by_key(|option| crate::distance_of(other, option)).copied().unwrap_or("reason");
                                add("S0107", true, key_range, format!("`#[{}]` has no option `{other}`; the options are `cfg = \"…\"`, `reason = \"…\"`, `except = [NAME, …]`", kind.name), Some((usize::from(key_range.start()), usize::from(key_range.end()), best.to_string())));
                            }
                        }
                    }
                    _ => {
                        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
                        let (line, column) = lines.at(text, start);
                        head.scope.push(arg.clone());
                        head.scope_at.push((line, column, start, end));
                    }
                }
            }
            let attr_range = attr.syntax().text_range();
            match kind.scope {
                Scope::Items if head.scope.is_empty() => add("S0102", true, attr_range, format!("`#[{}]` names no scope: give an item, for example `#[{}(shop::parse)]`", kind.name, kind.name), None),
                Scope::Two if head.scope.len() != 2 => add("S0103", true, attr_range, format!("`#[{}]` has {} path(s); it needs two: `#[{}(OLD, NEW)]`", kind.name, head.scope.len(), kind.name), None),
                _ => {}
            }
            if kind.name == "different" && params > 0 {
                let range = function.param_list().map(|list| list.syntax().text_range()).unwrap_or(attr_range);
                add("S0104", true, range, format!("a `different` claim has one input, so `fn {name}()` has no parameter"), None);
            }
            match kind.body {
                Body::Property if body_empty => add("S0105", true, function.name().map(|name| name.syntax().text_range()).unwrap_or(attr_range), format!("a `{}` claim is a property: the body is a `bool` expression", kind.name), None),
                Body::Empty if !body_empty => add("S0111", true, function.name().map(|name| name.syntax().text_range()).unwrap_or(attr_range), format!("`#[{}]` is a hint: `fn {name}()` has an empty body", kind.name), None),
                _ => {}
            }
            if !body_empty && kind.body != Body::Empty && !returns_bool {
                // The fix: `-> bool` after the parameter list.
                let fix = match (function.ret_type(), function.param_list()) {
                    (None, Some(list)) => Some((usize::from(list.syntax().text_range().end()), usize::from(list.syntax().text_range().end()), " -> bool".to_string())),
                    _ => None,
                };
                add("S0106", true, function.name().map(|name| name.syntax().text_range()).unwrap_or(attr_range), format!("the property `fn {name}` returns `bool`: write `-> bool`"), fix);
            }
            if let Some(batch) = kind.later {
                add("S0110", false, attr_range, format!("the model cannot check `#[{}]` yet: it comes in batch {batch}; the step shows the claim as \"claimed\"", kind.name), None);
            }
            heads.push(head);
        }
    }
    (heads, findings)
}

/// The arguments of an attribute, split at each comma at the top level, with their ranges.
fn arguments(tree: &ast::TokenTree) -> Vec<(String, TextRange)> {
    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut current: Option<(String, TextRange)> = None;
    let tokens: Vec<_> = tree.syntax().descendants_with_tokens().filter_map(|element| element.into_token()).collect();
    let last = tokens.len().saturating_sub(1);
    for (index, token) in tokens.iter().enumerate() {
        // The outer delimiters of the attribute.
        if index == 0 || index == last {
            continue;
        }
        let text = token.text();
        match text {
            "(" | "[" | "{" | "<" => depth += 1,
            ")" | "]" | "}" | ">" => depth -= 1,
            "," if depth == 0 => {
                args.extend(current.take());
                continue;
            }
            _ => {}
        }
        if token.kind().is_trivia() {
            if let Some((text, _)) = current.as_mut() {
                text.push(' ');
            }
            continue;
        }
        match current.as_mut() {
            Some((part, range)) => {
                part.push_str(text);
                *range = range.cover(token.text_range());
            }
            None => current = Some((text.to_string(), token.text_range())),
        }
    }
    args.extend(current);
    args.into_iter().map(|(text, range)| (text.split_whitespace().collect::<Vec<_>>().join(" ").replace(" :: ", "::").replace(":: ", "::"), range)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<(&'static str, bool)> {
        check("f.rs", text).1.into_iter().map(|finding| (finding.code, finding.error)).collect()
    }

    #[test]
    fn the_kinds_agree_with_the_attribute_crate() {
        // One source: each attribute of the crate `semilla-intent-macros` has a row, and each row
        // has an attribute.
        let source = include_str!("../../semilla-intent-macros/src/lib.rs");
        let start = source.find("claim_attributes! {").expect("the list of the attributes");
        let attributes: Vec<&str> = source[start..].lines().skip(1).take_while(|line| !line.starts_with('}')).filter_map(|line| line.trim().split_once(" => ").map(|(name, _)| name)).collect();
        let rows: Vec<&str> = KINDS.iter().map(|kind| kind.name).collect();
        assert_eq!(attributes, rows);
    }

    // rule+ R78.3-4
    // rule- R78.3-4
    #[test]
    fn each_code_has_a_long_text() {
        for code in CODES {
            assert!(explain(code.code).is_some_and(|text| text.contains(code.title)));
        }
        assert!(explain("S9999").is_none());
    }

    // rule- R78.3-2
    #[test]
    fn a_clean_file_has_no_finding_and_gives_each_head() {
        // R78.3-2 (negative): a correct claim gives no finding.
        let text = "use semilla_intent::prelude::*;\nintent!(\"x\");\n#[same(shop::parse, cfg = \"unix\", reason = \"r\")]\nfn parse(x: u32) -> bool { before::shop::parse(x) == after::shop::parse(x) }\n#[same(impl shop::Cart)]\nfn cart() {}\nfn helper(x: u32) -> u32 { x }\n";
        let (heads, findings) = check("f.rs", text);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(heads.len(), 2);
        assert_eq!(heads[0].scope, ["shop::parse"]);
        assert_eq!(heads[0].cfg.as_deref(), Some("unix"));
        assert_eq!(heads[1].scope, ["impl shop::Cart"]);
        assert!(heads[1].empty_body);
    }

    // rule+ R78.3-2
    #[test]
    fn each_rule_of_the_meaning_has_its_code() {
        // R78.3-2 (positive): each wrong head gives its code.
        assert_eq!(codes("struct S;\n"), [("S0101", true)]);
        assert_eq!(codes("#[same]\nfn f() {}\n"), [("S0102", true)]);
        assert_eq!(codes("#[rename(a::b)]\nfn f() {}\n"), [("S0103", true)]);
        assert_eq!(codes("#[different(a::b)]\nfn f(x: u32) -> bool { true }\n"), [("S0104", true)]);
        assert_eq!(codes("#[change(a::b)]\nfn f() {}\n"), [("S0105", true)]);
        assert_eq!(codes("#[change(a::b)]\nfn f(x: u32) { x > 1 }\n"), [("S0106", true)]);
        assert_eq!(codes("#[same(a::b, reson = \"x\")]\nfn f() {}\n"), [("S0107", true)]);
        assert_eq!(codes("#[same(a::b, except = [capacity])]\nfn f() {}\n"), [("S0108", false)]);
        assert_eq!(codes("#[same(a::b, cfg = \"all(unix\")]\nfn f() {}\n"), [("S0109", true)]);
        assert_eq!(codes("#[lemma]\nfn f(x: u32) -> bool { x == x }\n"), [("S0110", false)]);
        assert_eq!(codes("#[rename(a::b, a::c)]\nfn f() -> bool { true }\n"), [("S0111", true)]);
    }

    // rule+ R78.3-3
    #[test]
    fn a_fix_of_a_head_gives_a_clean_head() {
        // R78.3-3 (positive): each fix of a finding, put in place, removes the finding.
        for text in ["#[same(a::b, reson = \"x\")]\nfn f() {}\n", "#[change(a::b)]\nfn f(x: u32) { x > 1 }\n"] {
            let (_, findings) = check("f.rs", text);
            let (start, end, replacement) = findings[0].fix.clone().expect("a fix");
            let mut fixed = text.to_string();
            fixed.replace_range(start..end, &replacement);
            assert!(check("f.rs", &fixed).1.is_empty(), "{fixed}");
        }
    }
}

/// [R78.1-3] The doc links that the attribute writes for one scope argument (the rule of the
/// crate `semilla-intent-macros`): the path with the prefix of each version of the kind, with no
/// space; a qualified path `<T>::m` or `<T as Tr>::m` is `T::m`; `impl T` and a glob `P::*` name
/// the type or the module; `callers(P)` is `P`. A text, an option, and a trait path give none.
pub fn scope_links(kind: &str, index: usize, scope: &str) -> Vec<String> {
    let text: String = scope.chars().filter(|c| !c.is_whitespace()).collect();
    // A kind with no scope path (`lemma`, `assume`, `loop_relation`), and the text patterns of
    // `rewrite`, give no link.
    let no_path = KINDS.iter().find(|row| row.name == kind).is_none_or(|row| row.scope == Scope::None);
    if text.starts_with('"') || no_path || kind == "rewrite" {
        return Vec::new();
    }
    let text = match text.strip_prefix("callers(").and_then(|rest| rest.strip_suffix(')')) {
        Some(inner) => inner.to_string(),
        None => text,
    };
    let path = if let Some(rest) = text.strip_prefix("impl") {
        // `impl TRAIT for TYPE`: the type; `impl TYPE`: the type.
        rest.rsplit_once("for").map(|(_, ty)| ty.to_string()).unwrap_or_else(|| rest.to_string())
    } else if text.starts_with('<') {
        // [R79.2-1] The type of `<T as Trait>::item` ends at the word `as` (a token of the scope, as
        // in the crate of the attributes), not at the letters `as` of a name (`hashbrown`).
        let spaced = scope.trim().trim_start_matches('<');
        let Some((ty, item)) = spaced.split_once(">::") else { return Vec::new() };
        let ty = ty.split_whitespace().take_while(|word| *word != "as").collect::<String>();
        format!("{}::{}", ty.split('<').next().unwrap_or(""), item.split_whitespace().collect::<String>())
    } else {
        text.strip_suffix("::*").unwrap_or(&text).to_string()
    };
    let path = path.split('<').next().unwrap_or("").to_string();
    if path.is_empty() || path == "*" {
        return Vec::new();
    }
    if path.starts_with("before::") || path.starts_with("after::") {
        return vec![path];
    }
    let versions: &[&str] = match (kind, index) {
        ("rename" | "moved", 0) | ("removed", _) => &["before"],
        ("rename" | "moved", _) | ("new", _) => &["after"],
        ("invariant" | "relation" | "contract", _) => &[],
        _ => &["before", "after"],
    };
    versions.iter().map(|version| format!("{version}::{path}")).collect()
}

/// The record name of a scope text: `<shop::Counter>::get` is `shop::Counter::get`, and each
/// version prefix in a qualified path goes away (`<shop::S as before::shop::T>::m` is
/// `<shop::S as shop::T>::m`, the record name of a trait method).
pub fn record_text(scope: &str) -> String {
    let text = scope.replace("before::", "").replace("after::", "");
    if let Some(inner) = text.strip_prefix('<')
        && let Some((ty, rest)) = inner.split_once(">::")
        && !ty.contains(" as ")
    {
        return format!("{ty}::{rest}");
    }
    text
}

/// [R80.3-1] True for a scope of more than one item: `impl TYPE`, `impl TRAIT for TYPE`, a glob
/// `MODULE::*`, or `callers(ITEM)` (the forms of the language, not names).
pub fn is_wide(scope: &str) -> bool {
    let scope = scope.trim();
    scope.starts_with("impl ") || scope.starts_with("callers(") || scope.ends_with("::*")
}

/// [R78.3-5] The claims of a file from the heads, for a file that the parser of batch 10r1 does
/// not read (a form of the compiled language): rustc is the check of the body, so a body is a
/// mark that the claim has one. A scope of more than one item (R80.3-1) is one claim: the
/// oracle checks each item of it.
pub fn as_file(path: &str, text: &str) -> crate::File {
    let (heads, _) = check(path, text);
    let mut file = crate::File { path: path.to_string(), items: Vec::new() };
    let parse = ast::SourceFile::parse(text, Edition::Edition2024);
    for call in parse.tree().syntax().descendants().filter_map(ast::MacroCall::cast) {
        if call.path().is_some_and(|path| path.syntax().text() == "intent") {
            let inner = call.token_tree().map(|tree| tree.syntax().text().to_string()).unwrap_or_default();
            let value = inner.trim().trim_start_matches('(').trim_end_matches(')').trim().trim_matches('"').to_string();
            let place = crate::Place { file: path.to_string(), line: Lines::new(text).at(text, usize::from(call.syntax().text_range().start())).0, column: 1 };
            file.items.push(crate::Item::Intent(value, place));
        }
    }
    for head in heads {
        let place = crate::Place { file: path.to_string(), line: head.line, column: 1 };
        let kind = match head.kind.as_str() {
            "same" => crate::ClaimKind::Same,
            "change" => crate::ClaimKind::Change,
            "different" => crate::ClaimKind::Different,
            "rename" => crate::ClaimKind::Rename,
            "moved" => crate::ClaimKind::Moved,
            "rewrite" => crate::ClaimKind::Rewrite,
            _ => crate::ClaimKind::Later,
        };
        let body = (!head.empty_body).then(|| crate::Expr { kind: crate::ExprKind::Other("a body that rustc checks (design 78)".into()), place: place.clone(), text: String::new() });
        let params = head.signature.iter().map(|(name, ty)| (name.clone(), crate::Type::Other(ty.clone()), place.clone())).collect();
        let func = crate::Func { name: head.function.clone(), params, ret: None, body, place: place.clone() };
        let options = crate::Options { cfg: head.cfg.clone(), reason: head.reason.clone(), except: head.except.clone() };
        let attribute = head.kind.clone();
        let targets: Vec<crate::Target> = head.scope.iter().map(|scope| crate::Target::Text(record_text(scope), place.clone())).collect();
        if matches!(kind, crate::ClaimKind::Rename | crate::ClaimKind::Moved | crate::ClaimKind::Rewrite) || kind == crate::ClaimKind::Later {
            file.items.push(crate::Item::Claim(crate::Claim { kind, attribute, targets, options, func, place }));
        } else {
            for target in targets {
                file.items.push(crate::Item::Claim(crate::Claim { kind, attribute: attribute.clone(), targets: vec![target], options: options.clone(), func: func.clone(), place: place.clone() }));
            }
        }
    }
    file
}

#[cfg(test)]
mod file_tests {
    use super::*;

    // rule+ R79.2-1
    #[test]
    fn the_link_of_a_qualified_scope_path_ends_at_the_word_as() {
        let links = scope_links("same", 0, "<hashbrown::raw::RawTable as core::clone::Clone>::clone");
        assert_eq!(links, ["before::hashbrown::raw::RawTable::clone", "after::hashbrown::raw::RawTable::clone"]);
        assert_eq!(scope_links("new", 0, "<shop::W<u8> as shop::Named>::name"), ["after::shop::W::name"]);
        assert_eq!(scope_links("same", 0, "<shop::Counter>::get"), ["before::shop::Counter::get", "after::shop::Counter::get"]);
    }

    // rule+ R78.3-5
    #[test]
    fn a_file_of_the_compiled_language_gives_its_claims() {
        let text = "use semilla_intent::prelude::*;\nintent!(\"x\");\n#[same(<shop::Counter>::get, <shop::S as before::shop::T>::m)]\nfn f(x: u32) -> bool { <before::shop::S as before::shop::T>::m(x) }\n#[same(impl shop::Cart)]\nfn g() {}\n";
        let file = as_file("f.rs", text);
        let claims: Vec<(crate::ClaimKind, String)> = file.claims().map(|claim| (claim.kind, claim.targets[0].text())).collect();
        assert_eq!(claims, [(crate::ClaimKind::Same, "shop::Counter::get".into()), (crate::ClaimKind::Same, "<shop::S as shop::T>::m".into()), (crate::ClaimKind::Same, "impl shop::Cart".into())]);
        assert!(matches!(file.items[0], crate::Item::Intent(ref text, _) if text == "x"));
    }
}
