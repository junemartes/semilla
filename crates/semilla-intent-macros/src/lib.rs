//! The claim attributes of the intent language (design 78, R78.1-3).
//!
//! Each attribute keeps its function as it is and adds one hidden function with a doc link for
//! each path of the scope of the claim. The driver turns on the doc-link resolution of rustc in
//! the intent compile: the resolver of rustc resolves each path with no type check (a method
//! `Type::method` through its type), and the driver gives the definition of each path (also
//! through a re-export). Semilla gives an error for a path with no definition (S0112). Semilla reads the meaning of the attribute (the kind,
//! the options) from the source text; it never reads it from this crate.
//!
//! The scope forms and the code that rustc resolves (`V` is `before` or `after`):
//!
//! - a path with a version prefix (`after::shop::label`): `use after::shop::label as _;` (an
//!   import resolves each kind of item with no type inference);
//! - a path with no prefix (`shop::label`): the same for each version of the kind (`same`,
//!   `change`, `different`: the two versions; `new`: `after`; `removed`: `before`);
//! - `impl PATH` (each method of a type) and a glob `PATH::*`: `use V::PATH as _;`; in
//!   `impl TRAIT for TYPE`, the trait path is a path of the intent file (`core::fmt::Display`);
//! - `callers(PATH)`: as `PATH`;
//! - a qualified path `<T as Trait>::item` (an associated item, also a method `<T>::len`):
//!   `use` of the type (with the prefixes of the kind) and of the trait (as written);
//! - the two types of `relation` and the type of `invariant`: `use TYPE as _;`
//! - a text literal (a record name), an option `key = value`, and `*`: no code.
//!
//! The crate has no dependency: it reads the tokens itself.

use proc_macro::{Delimiter, Group, Ident, Span, TokenStream, TokenTree};

/// The versions that a path with no prefix names, for each kind of claim.
#[derive(Clone, Copy)]
enum Versions {
    Both,
    Before,
    After,
    /// The paths have their own prefix (a type of `relation`, `invariant`, `contract`).
    Written,
    /// The attribute has no path (`lemma`, `assume`, `loop_relation`).
    None,
}

macro_rules! claim_attributes {
    ($($(#[$doc:meta])* $name:ident => $versions:expr;)*) => {
        $(
            $(#[$doc])*
            #[proc_macro_attribute]
            pub fn $name(attr: TokenStream, item: TokenStream) -> TokenStream {
                claim(stringify!($name), $versions, attr, item)
            }
        )*
    };
}

claim_attributes! {
    /// `#[same(SCOPE)]`: the behavior of the scope stays the same.
    same => Versions::Both;
    /// `#[change(SCOPE)]`: the exact new behavior, as a relation to the old behavior.
    change => Versions::Both;
    /// `#[different(SCOPE)]`: one example input of a change (no parameters).
    different => Versions::Both;
    /// `#[new(ITEM)]`: the behavior of an added item.
    new => Versions::After;
    /// `#[removed(ITEM)]`: what replaces a removed item.
    removed => Versions::Before;
    /// `#[invariant(VERSION::TYPE)]`: a property of each value of a type of one version.
    invariant => Versions::Written;
    /// `#[relation(before::T, after::T)]`: how an old value and a new value correspond.
    relation => Versions::Written;
    /// `#[contract(VERSION::FN)]`: the contract of one function.
    contract => Versions::Written;
    /// `#[lemma]`: a proved fact that other claims can use.
    lemma => Versions::None;
    /// `#[loop_relation(at = PLACE)]`: how two loops correspond.
    loop_relation => Versions::None;
    /// `#[assume("TEXT")]`: a fact that Semilla cannot check.
    assume => Versions::None;
    /// `#[rename(A, B)]`: a hint for the pairing (an old name and a new name).
    rename => Versions::Both;
    /// `#[moved(A, B)]`: a hint for the pairing (an old place and a new place).
    moved => Versions::Both;
    /// `#[rewrite(A, B)]`: a hint for the pairing (text patterns, not paths).
    rewrite => Versions::None;
    /// `#[extract(before::BODY, into = after::CALLEE)]`: a region of the old body is the new
    /// callee (design 94).
    extract => Versions::Written;
    /// `#[inlined(after::BODY, from = before::CALLEE)]`: the new body has the code of the old
    /// callee (design 94). `inline` is an attribute of the language, so the name is `inlined`.
    inlined => Versions::Written;
}

fn claim(kind: &str, versions: Versions, attr: TokenStream, item: TokenStream) -> TokenStream {
    let name = function_name(&item).unwrap_or_else(|| "item".to_string());
    let mut body = Vec::new();
    let args = split_commas(attr);
    for (index, arg) in args.iter().enumerate() {
        // `rename(A, B)` and `moved(A, B)`: the first path is old, the second path is new.
        let versions = match (kind, index) {
            ("rename" | "moved" | "extract", 0) | ("inlined", 1) => Versions::Before,
            ("rename" | "moved" | "extract", _) | ("inlined", _) => Versions::After,
            _ => versions,
        };
        // The second path of `extract` (`into = PATH`) and of `inlined` (`from = PATH`).
        let named = matches!((kind, arg.first()), ("extract", Some(TokenTree::Ident(ident))) if ident.to_string() == "into") || matches!((kind, arg.first()), ("inlined", Some(TokenTree::Ident(ident))) if ident.to_string() == "from");
        let arg = if named && matches!(arg.get(1), Some(TokenTree::Punct(p)) if p.as_char() == '=') { &arg[2..] } else { &arg[..] };
        scope_code(kind, versions, arg, &mut body);
    }
    let mut out: Vec<TokenTree> = item.into_iter().collect();
    if !body.is_empty() {
        // `#[doc(hidden)] #[allow(…)] fn __semilla_scope_KIND_NAME() { … }`. Each path of the
        // scope is a doc link of the function: the resolver of rustc resolves it with no type
        // check (also a method `Type::method` through the type), and the driver gives its
        // definition (R78.1-3).
        let links = links_of(&body);
        for link in &links {
            out.extend(parse(&format!("#[doc = {:?}]", format!("[`{link}`]"))));
        }
        let body: Vec<TokenTree> = Vec::new();
        out.extend(parse("#[doc(hidden)] #[allow(unused, non_snake_case, clippy::all)]"));
        out.push(TokenTree::Ident(Ident::new("fn", Span::call_site())));
        out.push(TokenTree::Ident(Ident::new(&format!("__semilla_scope_{kind}_{name}"), Span::call_site())));
        out.push(TokenTree::Group(Group::new(Delimiter::Parenthesis, TokenStream::new())));
        out.push(TokenTree::Group(Group::new(Delimiter::Brace, body.into_iter().collect())));
    }
    out.into_iter().collect()
}

/// The path of each `use PATH as _;` of the scope code, as text with no space.
fn links_of(body: &[TokenTree]) -> Vec<String> {
    let mut links = Vec::new();
    let mut current: Option<String> = None;
    for token in body {
        match token {
            TokenTree::Ident(ident) if ident.to_string() == "use" && current.is_none() => current = Some(String::new()),
            TokenTree::Ident(ident) if ident.to_string() == "as" => {
                if let Some(path) = current.take() {
                    links.push(path);
                }
            }
            other => {
                if let Some(path) = current.as_mut() {
                    path.push_str(&other.to_string());
                }
            }
        }
    }
    links.retain(|link| !link.is_empty());
    links.dedup();
    links
}

/// The name of the function of the item.
fn function_name(item: &TokenStream) -> Option<String> {
    let mut tokens = item.clone().into_iter();
    while let Some(token) = tokens.next() {
        if let TokenTree::Ident(ident) = &token
            && ident.to_string() == "fn"
            && let Some(TokenTree::Ident(name)) = tokens.next()
        {
            return Some(name.to_string());
        }
    }
    None
}

/// The arguments of the attribute, split at each comma that is not in a group.
fn split_commas(attr: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut args = vec![Vec::new()];
    let mut depth = 0usize;
    for token in attr {
        match &token {
            TokenTree::Punct(punct) if punct.as_char() == ',' && depth == 0 => {
                args.push(Vec::new());
                continue;
            }
            TokenTree::Punct(punct) if punct.as_char() == '<' => depth += 1,
            TokenTree::Punct(punct) if punct.as_char() == '>' && depth > 0 => depth -= 1,
            _ => {}
        }
        if let Some(last) = args.last_mut() {
            last.push(token);
        }
    }
    args.retain(|arg| !arg.is_empty());
    args
}

/// Adds the code that makes rustc resolve one argument of the scope.
fn scope_code(kind: &str, versions: Versions, arg: &[TokenTree], body: &mut Vec<TokenTree>) {
    let is_punct = |token: Option<&TokenTree>, c: char| matches!(token, Some(TokenTree::Punct(p)) if p.as_char() == c);
    let first_word = match arg.first() {
        Some(TokenTree::Ident(ident)) => Some(ident.to_string()),
        _ => None,
    };
    // An option `key = value`, a text literal, or `*`: no code.
    if arg.len() >= 2 && matches!(&arg[0], TokenTree::Ident(_)) && is_punct(arg.get(1), '=') && !is_punct(arg.get(2), '=') {
        return;
    }
    if matches!(arg.first(), Some(TokenTree::Literal(_))) || matches!(versions, Versions::None) || kind == "rewrite" {
        return;
    }
    // `callers(PATH)`: the path inside.
    if first_word.as_deref() == Some("callers")
        && let Some(TokenTree::Group(group)) = arg.get(1)
    {
        let inner: Vec<TokenTree> = group.stream().into_iter().collect();
        return scope_code(kind, versions, &inner, body);
    }
    // `impl PATH`: the type of each version. `impl TRAIT for TYPE`: the trait as written (a
    // path of the intent file, for example `core::fmt::Display`), and the type of each version
    // (`*` names no type).
    if first_word.as_deref() == Some("impl") {
        let parts = split_word(&arg[1..], "for");
        if let [trait_path, type_path] = parts.as_slice() {
            type_code(Versions::Written, trait_path, body);
            type_code(versions, type_path, body);
        } else if let Some(type_path) = parts.first() {
            type_code(versions, type_path, body);
        }
        return;
    }
    // A qualified path `<T as Trait>::item` (an associated item): the type and the trait as
    // written. Semilla checks the item against the bodies of the version: a path expression of
    // a generic item with bounds has no type that rustc can infer here.
    if is_punct(arg.first(), '<') {
        let mut depth = 0usize;
        let mut inner = Vec::new();
        for token in &arg[1..] {
            match token {
                TokenTree::Punct(p) if p.as_char() == '<' => depth += 1,
                TokenTree::Punct(p) if p.as_char() == '>' && depth == 0 => break,
                TokenTree::Punct(p) if p.as_char() == '>' => depth -= 1,
                _ => {}
            }
            inner.push(token.clone());
        }
        let parts = split_word(&inner, "as");
        // The item after `>::`: the link is `TYPE::item` (the resolver resolves the type, the
        // driver finds the item of the type).
        let close = arg.iter().position(|token| matches!(token, TokenTree::Punct(p) if p.as_char() == '>')).unwrap_or(arg.len());
        let item: Vec<TokenTree> = arg.iter().skip(close + 1).cloned().collect();
        if let Some(type_path) = parts.first() {
            let mut full: Vec<TokenTree> = type_path.iter().take_while(|token| !matches!(token, TokenTree::Punct(p) if p.as_char() == '<')).cloned().collect();
            full.extend(item);
            type_code(versions, &full, body);
        }
        if let Some(trait_path) = parts.get(1) {
            type_code(Versions::Written, trait_path, body);
        }
        return;
    }
    // A glob `PATH::*`: the module as a type path.
    if is_punct(arg.last(), '*') {
        let module = &arg[..arg.len().saturating_sub(3)];
        return type_code(versions, module, body);
    }
    // The types of `relation` and `invariant`.
    if matches!(kind, "relation" | "invariant") {
        return type_code(versions, arg, body);
    }
    type_code(versions, arg, body);
}

/// `use V::PATH as _;` for each version, with no generic arguments.
fn type_code(versions: Versions, path: &[TokenTree], body: &mut Vec<TokenTree>) {
    let plain: Vec<TokenTree> = path.iter().take_while(|token| !matches!(token, TokenTree::Punct(p) if p.as_char() == '<')).cloned().collect();
    if plain.is_empty() || matches!(plain.as_slice(), [TokenTree::Punct(p)] if p.as_char() == '*') {
        return;
    }
    let span = plain.first().map(TokenTree::span).unwrap_or_else(Span::call_site);
    let spanned = |text: &str| -> Vec<TokenTree> {
        parse(text)
            .into_iter()
            .map(|mut token| {
                token.set_span(span);
                token
            })
            .collect()
    };
    for prefix in prefixes(versions, &plain) {
        body.extend(spanned("use"));
        body.extend(prefix);
        body.extend(plain.iter().cloned());
        body.extend(spanned("as _;"));
    }
}

/// The version prefixes for a path: none if the path has its own prefix.
fn prefixes(versions: Versions, path: &[TokenTree]) -> Vec<Vec<TokenTree>> {
    let own = matches!(path.first(), Some(TokenTree::Ident(ident)) if matches!(ident.to_string().as_str(), "before" | "after"));
    if own {
        return vec![Vec::new()];
    }
    let words: &[&str] = match versions {
        Versions::Both => &["before", "after"],
        Versions::Before => &["before"],
        Versions::After => &["after"],
        Versions::Written | Versions::None => &[""],
    };
    // The prefix has the place of the path, so an error of rustc marks the path in the
    // attribute.
    let span = path.first().map(TokenTree::span).unwrap_or_else(Span::call_site);
    words
        .iter()
        .map(|word| {
            if word.is_empty() {
                return Vec::new();
            }
            parse(&format!("{word}::"))
                .into_iter()
                .map(|mut token| {
                    token.set_span(span);
                    token
                })
                .collect()
        })
        .collect()
}

/// The parts of a token list split at a word.
fn split_word(tokens: &[TokenTree], word: &str) -> Vec<Vec<TokenTree>> {
    let mut parts = vec![Vec::new()];
    for token in tokens {
        if matches!(token, TokenTree::Ident(ident) if ident.to_string() == word) {
            parts.push(Vec::new());
        } else if let Some(last) = parts.last_mut() {
            last.push(token.clone());
        }
    }
    parts
}

fn parse(text: &str) -> Vec<TokenTree> {
    text.parse::<TokenStream>().map(|stream| stream.into_iter().collect()).unwrap_or_default()
}
