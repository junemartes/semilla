//! The lossless parse of Rust code (design 39).
//!
//! Semilla parses each Rust file with `ra_ap_syntax`, the syntax tree of rust-analyzer. The
//! tree is lossless: its text is equal to the file, byte for byte. Each node keeps its byte
//! range, and the spaces and the comments are tokens of the tree (trivia).
//!
//! A file whose tree has an error (an error node or a message of the parser) does not parse
//! (design 39.4). Semilla never compares a tree with an error.

use std::ops::Range;

pub use ra_ap_syntax::{AstNode, AstToken, Direction, Edition, NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, T, TextRange, TextSize, WalkEvent, ast};

use crate::tt;

/// The start of each line of a text, for the line and the column of a byte offset.
#[derive(Clone, Debug, Default)]
pub struct Lines {
    text: String,
    starts: Vec<usize>,
}

impl Lines {
    pub fn new(text: &str) -> Lines {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        Lines { text: text.to_string(), starts }
    }

    /// The line (from 1) and the column (in characters, from 0) of a byte offset.
    pub fn pos(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let start = self.starts[line];
        let column = self.text.get(start..offset).map_or(offset - start, |part| part.chars().count());
        (line + 1, column)
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The edition of a package from the value of its `edition` key (2015 if there is none).
pub fn edition(value: Option<&str>) -> Edition {
    value.and_then(|value| value.parse::<Edition>().ok()).unwrap_or(Edition::Edition2015)
}

/// Parses a Rust file losslessly (design 39.1, 39.4). The result is the tree, or the reason
/// for the first error with its line and column: an error node, an error of the parser, or a
/// tree text that is not equal to the file.
///
/// One error of the parser is not a stop (design 39.4): a parameter with no name in a trait
/// method of a 2015 package (`fn f(u8);`). The compiler accepts it in the 2015 edition. The tree
/// is complete: the parameter has one pattern, which is the type.
pub fn parse_file(text: &str, edition: Edition) -> Result<ast::SourceFile, String> {
    let (file, errors) = parse_checked(text, edition);
    let lines = Lines::new(text);
    let at = |offset: usize, reason: &str| {
        let (line, column) = lines.pos(offset);
        format!("{reason} (line {line}, column {})", column + 1)
    };
    if file.syntax().text() != text {
        return Err(at(0, "the text of the syntax tree is not equal to the file"));
    }
    let mut first: Option<(usize, String)> = None;
    let mut note = |offset: usize, reason: String| {
        if first.as_ref().is_none_or(|(at, _)| offset < *at) {
            first = Some((offset, reason));
        }
    };
    if let Some(error) = file.syntax().descendants().find(|node| node.kind() == SyntaxKind::ERROR) {
        let offset = usize::from(error.text_range().start());
        let shown: String = error.text().to_string().chars().take(20).collect();
        note(offset, format!("unexpected `{}`", shown.trim()));
    }
    for (offset, error) in errors {
        if edition == Edition::Edition2015 && error == "missing type for function parameter" && anonymous_parameter(&file, offset) {
            continue;
        }
        note(offset, error);
    }
    match first {
        Some((offset, reason)) => Err(at(offset, &reason)),
        None => Ok(file),
    }
}

/// Parses a text, and gives the tree of the text as written and the errors that `rustc` sees
/// (design 39.4). `rustc` changes each CRLF to LF before it lexes a file, so a `\` before a
/// CRLF in a string continues the string, and a CR before LF is not in a doc comment. The
/// errors come from the parse of the text with LF, with the offsets of the text as written.
pub fn parse_checked(text: &str, edition: Edition) -> (ast::SourceFile, Vec<(usize, String)>) {
    let parse = ast::SourceFile::parse(text, edition);
    let file = parse.tree();
    if !text.contains("\r\n") {
        let errors = parse.errors().into_iter().map(|error| (usize::from(error.range().start()), error.to_string())).collect();
        return (file, errors);
    }
    // The offsets (in the text with LF) of each LF that had a CR before it.
    let normal = text.replace("\r\n", "\n");
    let mut removed = Vec::new();
    let mut count = 0;
    for (at, _) in text.match_indices("\r\n") {
        removed.push(at - count);
        count += 1;
    }
    let original = |offset: usize| offset + removed.partition_point(|at| *at < offset);
    let checked = ast::SourceFile::parse(&normal, edition);
    let mut errors: Vec<(usize, String)> = checked.errors().into_iter().map(|error| (original(usize::from(error.range().start())), error.to_string())).collect();
    if let Some(error) = checked.tree().syntax().descendants().find(|node| node.kind() == SyntaxKind::ERROR) {
        errors.push((original(usize::from(error.text_range().start())), "unexpected text".to_string()));
    }
    (file, errors)
}

/// True if the place is the end of a parameter with one pattern of a trait method.
fn anonymous_parameter(file: &ast::SourceFile, offset: usize) -> bool {
    let Some(token) = file.syntax().token_at_offset(TextSize::from(offset as u32)).left_biased() else { return false };
    let Some(param) = token.parent_ancestors().find_map(ast::Param::cast) else { return false };
    let in_trait = param.syntax().ancestors().find_map(ast::Fn::cast).and_then(|function| function.syntax().parent()).and_then(|list| list.parent()).is_some_and(|owner| owner.kind() == SyntaxKind::TRAIT);
    in_trait && param.pat().is_some() && param.ty().is_none() && usize::from(param.syntax().text_range().end()) == offset
}

/// The byte range of a node without its leading spaces and plain comments. The tree puts the
/// comments before an item into the item; the code of the item starts after them. A doc
/// comment is code (an attribute).
pub fn code_range(node: &SyntaxNode) -> Range<usize> {
    let range = node.text_range();
    let inside = |token: &SyntaxToken| range.contains_range(token.text_range());
    // The first and the last token of the node that is not trivia: a walk from the ends.
    let mut first = node.first_token();
    while let Some(token) = first.clone().filter(|token| token.kind().is_trivia() && inside(token)) {
        first = token.next_token();
    }
    let mut last = node.last_token();
    while let Some(token) = last.clone().filter(|token| token.kind().is_trivia() && inside(token)) {
        last = token.prev_token();
    }
    let start = first.filter(|token| inside(token)).map_or(range.start(), |token| token.text_range().start());
    let end = last.filter(|token| inside(token)).map_or(range.end(), |token| token.text_range().end());
    usize::from(start)..usize::from(end).max(usize::from(start))
}

/// The compare text of a node (design 39.2): its tokens without trivia.
pub fn compare(node: &impl AstNode) -> String {
    tt::compare_text(node.syntax())
}

/// The compare text of an optional node (empty if there is none).
pub fn compare_opt(node: Option<&impl AstNode>) -> String {
    node.map(compare).unwrap_or_default()
}

/// The display text of a node (design 39.2): its source text as written, on one line.
pub fn display(node: &impl AstNode) -> String {
    display_syntax(node.syntax())
}

/// The display text of a syntax node.
pub fn display_syntax(node: &SyntaxNode) -> String {
    let range = code_range(node);
    let start = usize::from(node.text_range().start());
    let text = node.text().to_string();
    crate::tree::one_line(&text[range.start - start..range.end - start])
}

/// The statements of a statement list in order, with the tail expression at the end.
pub fn statements(list: &ast::StmtList) -> Vec<SyntaxNode> {
    let mut out: Vec<SyntaxNode> = list.statements().map(|stmt| stmt.syntax().clone()).collect();
    out.extend(list.tail_expr().map(|expr| expr.syntax().clone()));
    out
}

/// The editions for a text with no package (a text of a fact or of a claim): the edition of
/// most code first.
pub(crate) const EDITIONS: [Edition; 3] = [Edition::Edition2021, Edition::Edition2024, Edition::Edition2015];

thread_local! {
    /// The parsed fragments of this thread: (head, text, tail) → the green tree of the found
    /// node. The checks parse one text many times (the text of an item for each rule).
    static FRAGMENTS: std::cell::RefCell<std::collections::HashMap<(&'static str, String, &'static str), Option<ra_ap_syntax::GreenNode>>> = Default::default();
}

/// Parses a text with a wrapper, and finds a node of the result that `accept` accepts. The
/// wrapper has the text at one place. The parse has no error, and the node has the full text.
/// Each edition of [`EDITIONS`] is tried in order. The result is the root of its own tree.
fn fragment<N: AstNode>(text: &str, head: &'static str, tail: &'static str, accept: impl Fn(&N) -> bool) -> Option<N> {
    let key = (head, text.to_string(), tail);
    if let Some(found) = FRAGMENTS.with(|cache| cache.borrow().get(&key).cloned()) {
        return found.and_then(|green| N::cast(SyntaxNode::new_root(green)));
    }
    let found = parse_fragment(text, head, tail, accept).map(|node: N| node.syntax().green().to_owned());
    FRAGMENTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 50_000 {
            cache.clear();
        }
        cache.insert(key, found.clone());
    });
    found.and_then(|green| N::cast(SyntaxNode::new_root(green)))
}

fn parse_fragment<N: AstNode>(text: &str, head: &str, tail: &str, accept: impl Fn(&N) -> bool) -> Option<N> {
    let wrapped = format!("{head}{text}{tail}");
    let range = TextRange::new(TextSize::from(head.len() as u32), TextSize::from((head.len() + text.len()) as u32));
    for edition in EDITIONS {
        let parse = ast::SourceFile::parse(&wrapped, edition);
        if !parse.errors().is_empty() {
            continue;
        }
        let file = parse.tree();
        // The code of the text: from its first token to its last token, without the trivia.
        let inside = |token: &SyntaxToken| range.contains_range(token.text_range());
        let mut first = file.syntax().token_at_offset(range.start()).right_biased();
        while let Some(token) = first.clone().filter(|token| token.kind().is_trivia() || !inside(token)) {
            first = token.next_token().filter(|next| next.text_range().start() < range.end());
        }
        let mut last = file.syntax().token_at_offset(range.end()).left_biased();
        while let Some(token) = last.clone().filter(|token| token.kind().is_trivia() || !inside(token)) {
            last = token.prev_token().filter(|before| before.text_range().end() > range.start());
        }
        let (Some(first), Some(last)) = (first, last) else { return None };
        let want = usize::from(first.text_range().start())..usize::from(last.text_range().end());
        let found = file.syntax().descendants().filter(|node| range.contains_range(node.text_range())).filter_map(N::cast).find(|node| code_range(node.syntax()) == want && accept(node));
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Parses a text that is one item (with its attributes).
pub fn parse_item(text: &str) -> Option<ast::Item> {
    fragment(text, "", "\n", |_: &ast::Item| true)
}

/// Parses a text that is one expression.
///
/// A `let` expression (`let Some(x) = y`, `a && let b = c`) is correct only in a condition: its
/// text parses as the condition of an `if` (design 39.1).
pub fn parse_expr(text: &str) -> Option<ast::Expr> {
    if text.trim().is_empty() {
        return None;
    }
    let in_parens = fragment(text, "const _: () = (", "\n);", |expr: &ast::Expr| expr.syntax().parent().is_some_and(|parent| parent.kind() == SyntaxKind::PAREN_EXPR));
    in_parens.or_else(|| {
        fragment(text, "fn _f() { if ", "\n {} }", |condition: &ast::Expr| {
            let is_condition = condition.syntax().parent().is_some_and(|parent| parent.kind() == SyntaxKind::IF_EXPR);
            is_condition && condition.syntax().descendants().any(|node| node.kind() == SyntaxKind::LET_EXPR)
        })
    })
}

/// Parses a text that is one type.
pub fn parse_type(text: &str) -> Option<ast::Type> {
    fragment(text, "type _T = ", "\n;", |_: &ast::Type| true)
}

/// Parses a text that is one pattern (with no `|` at the top).
pub fn parse_pat(text: &str) -> Option<ast::Pat> {
    fragment(text, "fn _f() { let ", "\n; }", |pat: &ast::Pat| !matches!(pat, ast::Pat::OrPat(_)))
}

/// Parses the signature of a function: `fn name(…) -> T where …` with its qualifiers. The
/// result is a function item with no body.
pub fn parse_signature(text: &str) -> Option<ast::Fn> {
    let function: ast::Fn = fragment(&format!("{text};"), "trait _T { ", "\n}", |_: &ast::Fn| true)?;
    (function.body().is_none() && function.visibility().is_none() && function.attrs().next().is_none()).then_some(function)
}

/// Parses a list of generic parameters (`<T: Clone>`). An empty text is an empty list.
pub fn parse_generics(text: &str) -> Option<Option<ast::GenericParamList>> {
    if text.trim().is_empty() {
        return Some(None);
    }
    fragment(text, "fn _f", "() {}", |_: &ast::GenericParamList| true).map(Some)
}

/// Parses a block (`{ … }`).
pub fn parse_block(text: &str) -> Option<ast::BlockExpr> {
    fragment(text, "fn _f() ", "\n", |block: &ast::BlockExpr| block.syntax().parent().is_some_and(|parent| parent.kind() == SyntaxKind::FN))
}

/// The value of a string literal text (`"a\n"` gives `a` and a new line).
pub fn string_value(text: &str) -> Option<String> {
    let expr = parse_expr(text)?;
    let ast::Expr::Literal(literal) = expr else { return None };
    match literal.kind() {
        ast::LiteralKind::String(string) => string.value().ok().map(|value| value.into_owned()),
        _ => None,
    }
}

/// A `use` tree in the form of `syn` (design 39.3): a path segment with the rest, a name, a
/// name with a new name, a glob, or a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UseTree {
    Path(String, Box<UseTree>),
    Name(String),
    Rename(String, String),
    Glob,
    Group(Vec<UseTree>),
}

/// A `use` item: its visibility text, a leading `::`, and its tree.
#[derive(Clone, Debug)]
pub struct UseItem {
    pub vis: String,
    pub leading_colon: bool,
    pub tree: UseTree,
}

/// Parses the text of a `use` item.
pub fn parse_use(text: &str) -> Option<UseItem> {
    let ast::Item::Use(item) = parse_item(text)? else { return None };
    let tree = item.use_tree()?;
    // A leading `::` is the first token of the tree (`use ::a::b;` and `use ::{a, b};`).
    let first = tree.syntax().descendants_with_tokens().filter_map(|element| element.into_token()).find(|token| !token.kind().is_trivia());
    let leading_colon = first.is_some_and(|token| token.kind() == T![::]);
    Some(UseItem { vis: compare_opt(item.visibility().as_ref()), leading_colon, tree: use_tree(&tree)? })
}

/// The `use` tree of a node.
pub fn use_tree(tree: &ast::UseTree) -> Option<UseTree> {
    let leaf = if tree.star_token().is_some() {
        UseTree::Glob
    } else if let Some(list) = tree.use_tree_list() {
        UseTree::Group(list.use_trees().map(|inner| use_tree(&inner)).collect::<Option<Vec<_>>>()?)
    } else {
        let last = tree.path()?.segment()?.name_ref()?.text().to_string();
        match tree.rename() {
            Some(rename) => {
                let name = rename.name().map(|name| name.text().to_string()).unwrap_or_else(|| "_".to_string());
                return Some(wrap(tree.path().and_then(|path| path.qualifier()), UseTree::Rename(last, name)));
            }
            None => return Some(wrap(tree.path().and_then(|path| path.qualifier()), UseTree::Name(last))),
        }
    };
    Some(wrap(tree.path(), leaf))
}

/// Puts a tree under each segment of a path: `a::b` and `T` give `a::(b::(T))`.
fn wrap(path: Option<ast::Path>, mut tree: UseTree) -> UseTree {
    let mut segments: Vec<String> = Vec::new();
    let mut next = path;
    while let Some(path) = next {
        segments.push(path.segment().and_then(|segment| segment.name_ref()).map(|name| name.text().to_string()).unwrap_or_default());
        next = path.qualifier();
    }
    for segment in segments {
        tree = UseTree::Path(segment, Box::new(tree));
    }
    tree
}

use ast::{HasAttrs, HasName, HasVisibility};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_text_is_the_file() {
        let text = "// a comment\nfn f() {\n    1 + 2 // more\n}\n";
        let file = parse_file(text, Edition::Edition2021).unwrap();
        assert_eq!(file.syntax().text().to_string(), text);
    }

    #[test]
    fn a_file_with_an_error_does_not_parse() {
        let error = parse_file("pub fn f(a: u8) {\n    1 +\n}\n", Edition::Edition2021).unwrap_err();
        assert!(error.contains("line 3") || error.contains("line 2"), "{error}");
        assert!(parse_file("fn f() { let x = ; }", Edition::Edition2021).is_err());
    }

    #[test]
    fn a_file_with_crlf_line_ends_parses_as_rustc_reads_it() {
        // A `\` before a CRLF continues a string. A CR is not a part of a doc comment.
        let text = "/// One.\r\npub fn f() -> &'static str {\r\n    \"a long \\\r\n     text\"\r\n}\r\n";
        let file = parse_file(text, Edition::Edition2021).unwrap();
        assert_eq!(file.syntax().text().to_string(), text);
        let function = file.syntax().descendants().find_map(ast::Fn::cast).unwrap();
        assert_eq!(compare(&function), compare(&parse_file(&text.replace("\r\n", "\n"), Edition::Edition2021).unwrap().syntax().descendants().find_map(ast::Fn::cast).unwrap()));
        assert!(compare(&function).starts_with("# [doc = \" One.\"]"), "{}", compare(&function));
        // A bare CR in a string is an error for `rustc` too, with the place in the text as written.
        let error = parse_file("fn f() {}\r\nconst A: &str = \"a\rb\";\r\n", Edition::Edition2021).unwrap_err();
        assert!(error.contains("(line 2, column"), "{error}");
    }

    #[test]
    fn a_trait_parameter_with_no_name_parses_only_in_the_2015_edition() {
        let text = "pub trait T {\n    fn f(u8);\n    fn g(&self, &mut u8) -> bool;\n}\n";
        assert!(parse_file(text, Edition::Edition2015).is_ok());
        assert!(parse_file(text, Edition::Edition2018).is_err());
        assert!(parse_file("fn f(u8) {}", Edition::Edition2015).is_err());
    }

    #[test]
    fn the_edition_comes_from_the_manifest_value() {
        assert_eq!(edition(Some("2021")), Edition::Edition2021);
        assert_eq!(edition(None), Edition::Edition2015);
    }

    #[test]
    fn lines_and_columns_count_characters() {
        let lines = Lines::new("aé\nbc");
        assert_eq!(lines.pos(0), (1, 0));
        assert_eq!(lines.pos(3), (1, 2));
        assert_eq!(lines.pos(4), (2, 0));
        assert_eq!(lines.pos(6), (2, 2));
    }

    #[test]
    fn fragments_parse_with_the_full_text_only() {
        assert!(parse_expr("a + b").is_some());
        assert!(parse_expr("a + ").is_none());
        assert!(parse_expr("a b").is_none());
        assert!(matches!(parse_item("impl < T > A for B < T > { }"), Some(ast::Item::Impl(_))));
        assert!(parse_item("fn a() {} fn b() {}").is_none());
        assert!(parse_signature("fn f < T > (x : T) -> u8 where T : Copy").is_some());
        assert!(parse_pat("Some(x)").is_some());
        assert!(parse_pat("A | B").is_none());
        assert_eq!(string_value("\"a\\nb\"").as_deref(), Some("a\nb"));
        let item = parse_use("pub use ::a::{b as c, d::*, self};").unwrap();
        assert!(item.leading_colon && item.vis == "pub");
        assert!(!parse_use("use a::{b, c};").unwrap().leading_colon);
        assert!(!parse_use("use a::*;").unwrap().leading_colon);
        let group = UseTree::Group(vec![UseTree::Rename("b".into(), "c".into()), UseTree::Path("d".into(), Box::new(UseTree::Glob)), UseTree::Name("self".into())]);
        assert_eq!(item.tree, UseTree::Path("a".into(), Box::new(group)));
        assert!(matches!(parse_expr("let Some (x) = y"), Some(ast::Expr::LetExpr(_))));
        assert!(parse_expr("a && let Some(x) = y").is_some());
        // The 2015 edition: `async` is a name.
        assert!(parse_expr("async + 1").is_some());
    }

    #[test]
    fn the_display_text_is_the_source_on_one_line() {
        let file = parse_file("// x\n#[cfg(feature = \"std\")]\nfn f(\n    a: u8,\n) {}\n", Edition::Edition2021).unwrap();
        let function = file.syntax().descendants().find_map(ast::Fn::cast).unwrap();
        assert_eq!(display(&function.attrs().next().unwrap()), "#[cfg(feature = \"std\")]");
        assert_eq!(compare(&function.attrs().next().unwrap()), "# [cfg (feature = \"std\")]");
        assert_eq!(display(&function), "#[cfg(feature = \"std\")] fn f( a: u8, ) {}");
    }
}
