//! The token trees of a Rust text (design 39.3).
//!
//! A token tree is a list of tokens with groups for the delimiters `()`, `[]`, and `{}`. The
//! types have the form of the `proc_macro` types: a group, an identifier, a punctuation mark,
//! and a literal. The tokens come from the lossless tree of `ra_ap_syntax`
//! ([`TokenStream::from_node`]) or from a text ([`str::parse`]).
//!
//! The rules of the tokens:
//! - Spaces and plain comments are not tokens. A doc comment is the attribute
//!   `#[doc = "…"]` (or `#![doc = "…"]`), as for the compiler.
//! - A word (a name, a keyword, `_`) is an identifier. A lifetime `'a` is the mark `'` and the
//!   identifier `a`.
//! - Each character of an operator is one punctuation mark. A mark is "joint" if the next
//!   character is part of the same operator. In a macro input (a token tree of the source), a
//!   mark is joint if the next token starts at once with a mark, as in the source.
//!
//! The text form ([`TokenStream`] as `Display`) puts one space between two tokens, except after
//! a joint mark, inside `()` and `[]`, and around nothing in an empty group. This text is the
//! compare text of a node: two nodes are the same if their compare texts are equal.

use std::fmt;
use std::str::FromStr;

use ra_ap_syntax::{AstNode, Edition, NodeOrToken, SyntaxKind, SyntaxNode, SyntaxToken, WalkEvent, ast};

use crate::rs::Lines;

/// A line (from 1) and a column (in characters, from 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineColumn {
    pub line: usize,
    pub column: usize,
}

/// The place of a token: its start and its end. A token without a known place has (0, 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    start: LineColumn,
    end: LineColumn,
}

impl Span {
    pub fn start(&self) -> LineColumn {
        self.start
    }

    pub fn end(&self) -> LineColumn {
        self.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delimiter {
    Parenthesis,
    Brace,
    Bracket,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spacing {
    Alone,
    Joint,
}

#[derive(Clone, Debug)]
pub struct Group {
    delimiter: Delimiter,
    stream: TokenStream,
    open: Span,
    close: Span,
}

impl Group {
    pub fn new(delimiter: Delimiter, stream: TokenStream) -> Group {
        Group { delimiter, stream, open: Span::default(), close: Span::default() }
    }

    pub fn delimiter(&self) -> Delimiter {
        self.delimiter
    }

    pub fn stream(&self) -> TokenStream {
        self.stream.clone()
    }

    pub fn span_open(&self) -> Span {
        self.open
    }

    pub fn span_close(&self) -> Span {
        self.close
    }

    pub fn span(&self) -> Span {
        Span { start: self.open.start, end: self.close.end }
    }
}

#[derive(Clone, Debug)]
pub struct Ident {
    text: String,
    span: Span,
}

impl Ident {
    pub fn span(&self) -> Span {
        self.span
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for Ident {
    fn eq(&self, other: &T) -> bool {
        self.text == other.as_ref()
    }
}

#[derive(Clone, Debug)]
pub struct Punct {
    ch: char,
    spacing: Spacing,
    span: Span,
}

impl Punct {
    pub fn as_char(&self) -> char {
        self.ch
    }

    pub fn spacing(&self) -> Spacing {
        self.spacing
    }

    pub fn span(&self) -> Span {
        self.span
    }
}

#[derive(Clone, Debug)]
pub struct Literal {
    text: String,
    span: Span,
}

impl Literal {
    pub fn span(&self) -> Span {
        self.span
    }
}

#[derive(Clone, Debug)]
pub enum TokenTree {
    Group(Group),
    Ident(Ident),
    Punct(Punct),
    Literal(Literal),
}

impl TokenTree {
    pub fn span(&self) -> Span {
        match self {
            TokenTree::Group(group) => group.span(),
            TokenTree::Ident(ident) => ident.span,
            TokenTree::Punct(punct) => punct.span,
            TokenTree::Literal(literal) => literal.span,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TokenStream(Vec<TokenTree>);

impl TokenStream {
    pub fn new() -> TokenStream {
        TokenStream::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The tokens of a node of a lossless tree. The places are unknown.
    pub fn from_node(node: &SyntaxNode) -> TokenStream {
        build(&flat_tokens(node), None, 0)
    }

    /// The tokens of a node, with the places from the line index of its text. `base` is the
    /// byte offset of the tree text in the indexed text.
    pub fn from_node_at(node: &SyntaxNode, lines: &Lines, base: usize) -> TokenStream {
        build(&flat_tokens(node), Some(lines), base)
    }
}

impl IntoIterator for TokenStream {
    type Item = TokenTree;
    type IntoIter = std::vec::IntoIter<TokenTree>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl FromIterator<TokenTree> for TokenStream {
    fn from_iter<I: IntoIterator<Item = TokenTree>>(iter: I) -> Self {
        TokenStream(iter.into_iter().collect())
    }
}

impl FromIterator<TokenStream> for TokenStream {
    fn from_iter<I: IntoIterator<Item = TokenStream>>(iter: I) -> Self {
        TokenStream(iter.into_iter().flat_map(|stream| stream.0).collect())
    }
}

impl Extend<TokenTree> for TokenStream {
    fn extend<I: IntoIterator<Item = TokenTree>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl fmt::Display for TokenStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut joint = false;
        for (index, tree) in self.0.iter().enumerate() {
            if index != 0 && !joint {
                f.write_str(" ")?;
            }
            joint = matches!(tree, TokenTree::Punct(punct) if punct.spacing == Spacing::Joint);
            write!(f, "{tree}")?;
        }
        Ok(())
    }
}

impl fmt::Display for TokenTree {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TokenTree::Group(group) => write!(f, "{group}"),
            TokenTree::Ident(ident) => write!(f, "{ident}"),
            TokenTree::Punct(punct) => write!(f, "{punct}"),
            TokenTree::Literal(literal) => write!(f, "{literal}"),
        }
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (open, close) = match self.delimiter {
            Delimiter::Parenthesis => ("(", ")"),
            Delimiter::Brace => ("{ ", "}"),
            Delimiter::Bracket => ("[", "]"),
            Delimiter::None => ("", ""),
        };
        f.write_str(open)?;
        write!(f, "{}", self.stream)?;
        if self.delimiter == Delimiter::Brace && !self.stream.is_empty() {
            f.write_str(" ")?;
        }
        f.write_str(close)
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl fmt::Display for Punct {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.ch)
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// A text that is not a list of token trees (an open delimiter, a bad literal).
#[derive(Debug)]
pub struct LexError;

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("cannot parse string into token stream")
    }
}

impl std::error::Error for LexError {}

impl FromStr for TokenStream {
    type Err = LexError;

    /// The token trees of a text, with no places (see [`TokenStream::with_places`]).
    fn from_str(text: &str) -> Result<TokenStream, LexError> {
        thread_local! {
            /// The token lists of the texts of this thread: the checks read one text many times.
            static STREAMS: std::cell::RefCell<std::collections::HashMap<String, Option<TokenStream>>> = Default::default();
        }
        if let Some(found) = STREAMS.with(|cache| cache.borrow().get(text).cloned()) {
            return found.ok_or(LexError);
        }
        let found = lex(text, false).ok();
        STREAMS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() > 50_000 {
                cache.clear();
            }
            cache.insert(text.to_string(), found.clone());
        });
        found.ok_or(LexError)
    }
}

impl TokenStream {
    /// The token trees of a text. The places are lines and columns in the text.
    pub fn with_places(text: &str) -> Result<TokenStream, LexError> {
        lex(text, true)
    }
}

/// The token trees of a text, through the parse of a macro call `_m!{TEXT}`.
fn lex(text: &str, places: bool) -> Result<TokenStream, LexError> {
    {
        const HEAD: &str = "_m!{\n";
        let wrapped = format!("{HEAD}{text}\n}}");
        let (file, errors) = crate::rs::parse_checked(&wrapped, Edition::Edition2021);
        if !errors.is_empty() {
            return Err(LexError);
        }
        let Some(tree) = file.syntax().descendants().find_map(ast::TokenTree::cast) else { return Err(LexError) };
        if usize::from(tree.syntax().text_range().end()) != wrapped.len() || file.syntax().children().count() != 1 {
            return Err(LexError);
        }
        let lines = places.then(|| Lines::new(text));
        let mut tokens = flat_tokens(tree.syntax());
        // The outer braces are not in the text.
        tokens.remove(0);
        tokens.pop();
        for token in &mut tokens {
            token.start -= HEAD.len();
            token.end -= HEAD.len();
        }
        Ok(build(&tokens, lines.as_ref(), 0))
    }
}

/// One token of a tree, with its byte range in the tree text and its context.
#[derive(Clone, Debug)]
pub(crate) struct Flat {
    pub kind: SyntaxKind,
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// True for a token of a token tree of the source (a macro input, an attribute input).
    pub in_tree: bool,
    /// True for a token with no meaning in the grammar (design 52.2): an optional comma, an
    /// empty statement. The compare text does not have it. The display text has it.
    pub void: bool,
}

/// The tokens of a node in source order, without spaces and plain comments.
pub(crate) fn flat_tokens(node: &SyntaxNode) -> Vec<Flat> {
    let mut depth = node.ancestors().skip(1).filter(|ancestor| ancestor.kind() == SyntaxKind::TOKEN_TREE).count();
    let mut out = Vec::new();
    for event in node.preorder_with_tokens() {
        match event {
            WalkEvent::Enter(NodeOrToken::Node(inner)) if inner.kind() == SyntaxKind::TOKEN_TREE => depth += 1,
            WalkEvent::Leave(NodeOrToken::Node(inner)) if inner.kind() == SyntaxKind::TOKEN_TREE => depth -= 1,
            WalkEvent::Enter(NodeOrToken::Token(token)) => {
                if let Some(flat) = flat(&token, depth > 0) {
                    out.push(flat);
                }
            }
            _ => {}
        }
    }
    out
}

fn flat(token: &SyntaxToken, in_tree: bool) -> Option<Flat> {
    if token.kind().is_trivia() || token.kind() == SyntaxKind::SHEBANG {
        return None;
    }
    let range = token.text_range();
    // `rustc` changes each CRLF to LF before it lexes a file (design 39.4): a literal or a doc
    // comment has the text that `rustc` reads.
    let text = if token.text().contains('\r') { token.text().replace("\r\n", "\n") } else { token.text().to_string() };
    Some(Flat { kind: token.kind(), text, start: range.start().into(), end: range.end().into(), in_tree, void: !in_tree && no_meaning(token) })
}

/// True for a token that the grammar of Rust permits and that has no meaning (design 52.2):
/// - E1: the comma before the closing delimiter of its list (`f(a, b,)`, `[a, b,]`, `S { a, }`,
///   `<T, U,>`, `|a, b,|`, the comma after the last arm of a `match`). The comma of a tuple
///   with one element is not optional: `(a,)` is a tuple and `(a)` is not.
/// - E2: the comma after a `match` arm whose body is an expression with a block.
/// - E3: an empty statement: a `;` in a block that is not a part of a statement.
/// - E4: the comma after the last predicate of a `where` clause.
///
/// A token in a token tree (a macro input, an attribute input) is not in this rule: the macro
/// can read it.
fn no_meaning(token: &SyntaxToken) -> bool {
    use SyntaxKind::*;
    if token.kind() == SEMICOLON {
        // The `;` of a statement is in the node of the statement. A `;` that the block has
        // comes after a full statement, after an item, or at the start of the block.
        let before = token.siblings_with_tokens(ra_ap_syntax::Direction::Prev).skip(1).find(|element| !element.kind().is_trivia());
        return token.parent().is_some_and(|list| list.kind() == STMT_LIST)
            && before.is_some_and(|element| match element {
                NodeOrToken::Token(token) => matches!(token.kind(), L_CURLY | SEMICOLON),
                NodeOrToken::Node(node) => match node.kind() {
                    MACRO_CALL | MACRO_EXPR => false,
                    EXPR_STMT => node.last_token().is_some_and(|last| last.kind() == SEMICOLON),
                    _ => true,
                },
            });
    }
    if token.kind() != COMMA {
        return false;
    }
    let Some(list) = token.parent() else { return false };
    if list.kind() == WHERE_CLAUSE {
        return token.siblings_with_tokens(ra_ap_syntax::Direction::Next).skip(1).all(|element| element.kind() != WHERE_PRED);
    }
    let next = std::iter::successors(token.next_token(), |next| next.next_token()).find(|next| !next.kind().is_trivia());
    let Some(next) = next else { return false };
    if list.kind() == MATCH_ARM {
        // The comma is the last token of its arm.
        let with_block = match ast::MatchArm::cast(list.clone()).and_then(|arm| arm.expr()) {
            Some(ast::Expr::BlockExpr(block)) => matches!(block.modifier(), None | Some(ast::BlockModifier::Label(_)) | Some(ast::BlockModifier::Unsafe(_)) | Some(ast::BlockModifier::Const(_))),
            Some(ast::Expr::IfExpr(_) | ast::Expr::MatchExpr(_) | ast::Expr::LoopExpr(_) | ast::Expr::WhileExpr(_) | ast::Expr::ForExpr(_)) => true,
            _ => false,
        };
        return with_block || (next.kind() == R_CURLY && next.parent() == list.parent());
    }
    let close = match next.kind() {
        R_PAREN | R_BRACK | R_CURLY | R_ANGLE => true,
        PIPE => list.kind() == PARAM_LIST,
        _ => false,
    };
    if !close || next.parent().as_ref() != Some(&list) {
        return false;
    }
    let commas = list.children_with_tokens().filter(|element| element.kind() == COMMA).count();
    !(matches!(list.kind(), TUPLE_EXPR | TUPLE_TYPE | TUPLE_PAT) && commas == 1)
}

/// The operators of more than one character of the lexer of `rustc`.
const COMPOUND: [&str; 22] = ["<<=", ">>=", "...", "..=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "^=", "&=", "|=", ".."];
const SHIFTS: [&str; 2] = ["<<", ">>"];

/// For each token: true if it is joint with the next token (design 39.2). In a token tree of
/// the source, the lexer gives one token for each mark. Marks that touch make one operator of
/// `rustc` (`=>`, `::`, `..=`), with the longest operator first. Each other pair of marks is two
/// tokens, also if they touch (`=-` is `=` and `-`). So the compare text of a token tree does
/// not depend on spaces or line breaks.
fn joins(tokens: &[Flat]) -> Vec<bool> {
    let mut out = vec![false; tokens.len()];
    let mark = |token: &Flat| token.in_tree && token.text.chars().count() == 1 && !is_literal(token.kind) && !is_word(&token.text) && token.kind != SyntaxKind::LIFETIME_IDENT && !matches!(token.text.as_str(), "(" | ")" | "[" | "]" | "{" | "}") && doc_comment(token.kind, &token.text).is_none();
    let mut index = 0;
    while index < tokens.len() {
        if !mark(&tokens[index]) {
            index += 1;
            continue;
        }
        // The run of marks that touch.
        let mut end = index + 1;
        while end < tokens.len() && mark(&tokens[end]) && tokens[end].start == tokens[end - 1].end {
            end += 1;
        }
        let mut at = index;
        while at < end {
            let text = |len: usize| tokens[at..at + len].iter().map(|token| token.text.as_str()).collect::<String>();
            let len = [3, 2].into_iter().find(|len| at + len <= end && (COMPOUND.contains(&text(*len).as_str()) || SHIFTS.contains(&text(*len).as_str()))).unwrap_or(1);
            for position in at..at + len - 1 {
                out[position] = true;
            }
            at += len;
        }
        index = end;
    }
    out
}

fn is_literal(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, INT_NUMBER | FLOAT_NUMBER | STRING | BYTE_STRING | C_STRING | CHAR | BYTE)
}

fn is_word(text: &str) -> bool {
    text.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// The doc text of a doc comment token: (inner, text). `None` for a plain comment.
pub(crate) fn doc_comment(kind: SyntaxKind, text: &str) -> Option<(bool, String)> {
    use SyntaxKind::*;
    let inner = match kind {
        INNER_DOC_COMMENT => true,
        OUTER_DOC_COMMENT => false,
        _ => return None,
    };
    // The text that `rustc` reads: each CRLF is LF (design 39.4).
    let text = text.replace("\r\n", "\n");
    let value = if text.starts_with("/*") { text.get(3..text.len().saturating_sub(2)).unwrap_or("") } else { text.get(3..).unwrap_or("").trim_end_matches('\r') };
    Some((inner, value.to_string()))
}

/// A string literal with the escapes of `proc_macro`.
pub fn string_literal(value: &str) -> String {
    let mut repr = String::with_capacity(value.len() + 2);
    repr.push('"');
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\0' {
            repr.push_str(if chars.peek().is_some_and(|next| ('0'..='7').contains(next)) { r"\x00" } else { r"\0" });
        } else if c == '\'' {
            repr.push(c);
        } else {
            repr.extend(c.escape_debug());
        }
    }
    repr.push('"');
    repr
}

enum Part {
    Open(Delimiter, Span),
    Close(Delimiter, Span),
    Tree(TokenTree),
}

fn build(tokens: &[Flat], lines: Option<&Lines>, base: usize) -> TokenStream {
    let place = |offset: usize| -> LineColumn {
        match lines {
            Some(lines) => {
                let (line, column) = lines.pos(offset + base);
                LineColumn { line, column }
            }
            None => LineColumn::default(),
        }
    };
    let span = |start: usize, end: usize| Span { start: place(start), end: place(end) };
    let joined = joins(tokens);
    let mut parts: Vec<Part> = Vec::with_capacity(tokens.len() + 4);
    for (index, token) in tokens.iter().enumerate() {
        let text = token.text.as_str();
        if token.void {
            continue;
        }
        if let Some((inner, value)) = doc_comment(token.kind, text) {
            let at = span(token.start, token.end);
            parts.push(Part::Tree(TokenTree::Punct(Punct { ch: '#', spacing: Spacing::Alone, span: at })));
            if inner {
                parts.push(Part::Tree(TokenTree::Punct(Punct { ch: '!', spacing: Spacing::Alone, span: at })));
            }
            let stream = TokenStream(vec![
                TokenTree::Ident(Ident { text: "doc".into(), span: at }),
                TokenTree::Punct(Punct { ch: '=', spacing: Spacing::Alone, span: at }),
                TokenTree::Literal(Literal { text: string_literal(&value), span: at }),
            ]);
            parts.push(Part::Tree(TokenTree::Group(Group { delimiter: Delimiter::Bracket, stream, open: at, close: at })));
            continue;
        }
        let delimiter = match text {
            "(" | ")" => Some(Delimiter::Parenthesis),
            "[" | "]" => Some(Delimiter::Bracket),
            "{" | "}" => Some(Delimiter::Brace),
            _ => None,
        };
        if let Some(delimiter) = delimiter.filter(|_| !is_literal(token.kind)) {
            let at = span(token.start, token.end);
            parts.push(if matches!(text, "(" | "[" | "{") { Part::Open(delimiter, at) } else { Part::Close(delimiter, at) });
        } else if is_literal(token.kind) {
            parts.push(Part::Tree(TokenTree::Literal(Literal { text: text.to_string(), span: span(token.start, token.end) })));
        } else if token.kind == SyntaxKind::LIFETIME_IDENT {
            parts.push(Part::Tree(TokenTree::Punct(Punct { ch: '\'', spacing: Spacing::Joint, span: span(token.start, token.start + 1) })));
            parts.push(Part::Tree(TokenTree::Ident(Ident { text: text[1..].to_string(), span: span(token.start + 1, token.end) })));
        } else if is_word(text) {
            parts.push(Part::Tree(TokenTree::Ident(Ident { text: text.to_string(), span: span(token.start, token.end) })));
        } else {
            // An operator: one mark for each character.
            let next_joins = joined[index];
            let count = text.chars().count();
            for (position, (offset, c)) in text.char_indices().enumerate() {
                let spacing = if position + 1 < count || next_joins { Spacing::Joint } else { Spacing::Alone };
                parts.push(Part::Tree(TokenTree::Punct(Punct { ch: c, spacing, span: span(token.start + offset, token.start + offset + c.len_utf8()) })));
            }
        }
    }
    // The groups: a stack of open delimiters. A close delimiter without its open delimiter is a
    // mark (this is not possible in a tree with no error).
    let mut stack: Vec<(Delimiter, Span, Vec<TokenTree>)> = vec![(Delimiter::None, Span::default(), Vec::new())];
    for part in parts {
        match part {
            Part::Open(delimiter, at) => stack.push((delimiter, at, Vec::new())),
            Part::Close(delimiter, at) if stack.len() > 1 && stack.last().is_some_and(|(open, _, _)| *open == delimiter) => {
                let (delimiter, open, trees) = stack.pop().expect("an open group");
                let group = Group { delimiter, stream: TokenStream(trees), open, close: at };
                stack.last_mut().expect("the outer list").2.push(TokenTree::Group(group));
            }
            Part::Close(delimiter, at) => {
                let ch = match delimiter {
                    Delimiter::Parenthesis => ')',
                    Delimiter::Bracket => ']',
                    _ => '}',
                };
                stack.last_mut().expect("the outer list").2.push(TokenTree::Punct(Punct { ch, spacing: Spacing::Alone, span: at }));
            }
            Part::Tree(tree) => stack.last_mut().expect("the outer list").2.push(tree),
        }
    }
    // An open group without its close delimiter: its tokens go to the outer list.
    while stack.len() > 1 {
        let (_, _, trees) = stack.pop().expect("an open group");
        stack.last_mut().expect("the outer list").2.extend(trees);
    }
    TokenStream(stack.pop().map(|(_, _, trees)| trees).unwrap_or_default())
}

/// The compare text of a node: its tokens in the text form of [`TokenStream`].
pub fn compare_text(node: &SyntaxNode) -> String {
    flat_text(&flat_tokens(node))
}

/// The compare text of a node without the tokens in the ranges.
pub fn text_without(node: &SyntaxNode, skip: &[ra_ap_syntax::TextRange]) -> String {
    let tokens: Vec<Flat> = flat_tokens(node).into_iter().filter(|token| !skip.iter().any(|range| usize::from(range.start()) <= token.start && token.end <= usize::from(range.end()))).collect();
    flat_text(&tokens)
}

/// The display text of a list of tokens (design 39.2): the source text of each token, with one
/// space where the source has space or a comment between two tokens. So a list of the tokens
/// of one node is its source text on one line, without its plain comments.
pub(crate) fn flat_display(tokens: &[Flat]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 && tokens[index - 1].end != token.start {
            out.push(' ');
        }
        out.push_str(&token.text);
    }
    out
}

/// The display text of a node: its source text on one line, without its plain comments.
pub fn display_text(node: &SyntaxNode) -> String {
    flat_display(&flat_tokens(node))
}

/// The display text of a node without the tokens in the ranges.
pub fn display_without(node: &SyntaxNode, skip: &[ra_ap_syntax::TextRange]) -> String {
    let tokens: Vec<Flat> = flat_tokens(node).into_iter().filter(|token| !skip.iter().any(|range| usize::from(range.start()) <= token.start && token.end <= usize::from(range.end()))).collect();
    flat_display(&tokens)
}

/// The compare text of a list of tokens: the text form of [`TokenStream`], with no tree. One
/// space goes between two tokens, except after a joint mark, after `(` and `[`, and before `)`
/// and `]`. This is the text of the groups of [`TokenStream`] (a list of tokens of a tree with
/// no error has its delimiters in pairs).
pub(crate) fn flat_text(tokens: &[Flat]) -> String {
    flat_layout(tokens, None)
}

/// The compare text of a list of tokens. `ranges` gets the byte range of each token in the
/// text (an empty range for a token that the text does not have).
pub(crate) fn flat_layout(tokens: &[Flat], mut ranges: Option<&mut Vec<(usize, usize)>>) -> String {
    let joined = joins(tokens);
    let mut out = String::with_capacity(tokens.iter().map(|token| token.text.len() + 1).sum());
    // True if the last character of `out` is a joint mark or an open delimiter.
    let mut glue = true;
    let push = |out: &mut String, text: &str, close: bool, glue_next: bool, glue: &mut bool| {
        if !*glue && !close {
            out.push(' ');
        }
        out.push_str(text);
        *glue = glue_next;
    };
    for (index, token) in tokens.iter().enumerate() {
        let text = token.text.as_str();
        if token.void {
            if let Some(ranges) = ranges.as_deref_mut() {
                ranges.push((out.len(), out.len()));
            }
            continue;
        }
        // The token starts after the space that comes before it.
        let from = if glue || matches!(text, ")" | "]") && !is_literal(token.kind) { out.len() } else { out.len() + 1 };
        if let Some((inner, value)) = doc_comment(token.kind, text) {
            push(&mut out, "#", false, false, &mut glue);
            if inner {
                push(&mut out, "!", false, false, &mut glue);
            }
            push(&mut out, "[", false, true, &mut glue);
            push(&mut out, "doc", false, false, &mut glue);
            push(&mut out, "=", false, false, &mut glue);
            push(&mut out, &string_literal(&value), false, false, &mut glue);
            push(&mut out, "]", true, false, &mut glue);
        } else if is_literal(token.kind) || is_word(text) {
            push(&mut out, text, false, false, &mut glue);
        } else if token.kind == SyntaxKind::LIFETIME_IDENT {
            push(&mut out, text, false, false, &mut glue);
        } else if matches!(text, "(" | "[" | "{") {
            push(&mut out, text, false, text != "{", &mut glue);
        } else if matches!(text, ")" | "]" | "}") {
            push(&mut out, text, text != "}", false, &mut glue);
        } else {
            // An operator: its characters are joint. Its last character is joint before a mark
            // in a token tree of the source.
            let next_joins = joined[index];
            push(&mut out, text, false, next_joins, &mut glue);
        }
        if let Some(ranges) = ranges.as_deref_mut() {
            ranges.push((from, out.len()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Design 52.2: a token that the grammar permits and that has no meaning is not in the
    /// compare text. A token that looks the same and has a meaning stays.
    #[test]
    fn a_token_with_no_meaning_is_not_in_the_compare_text() {
        let text = |source: &str| compare_text(crate::rs::parse_file(source, Edition::Edition2021).unwrap().syntax());
        let same = [
            // E1: the comma before the closing delimiter of a list.
            ("fn f(a: u32, b: u32,) -> (u32, u32,) { g::<u8,>(a, b,); [a, b,]; S { a, b, }; let (x, y,) = (a, b,); let P(z,) = p; |p, q,| p }", "fn f(a: u32, b: u32) -> (u32, u32) { g::<u8>(a, b); [a, b]; S { a, b }; let (x, y) = (a, b); let P(z) = p; |p, q| p }"),
            ("struct S<T, U,> { a: T, b: U, } enum E { A, B, } struct P(u32,); use m::{a, b,};", "struct S<T, U> { a: T, b: U } enum E { A, B } struct P(u32); use m::{a, b};"),
            // E1 for the last arm, and E2: the comma after an arm with a block.
            ("fn f() { match a { 1 => { 2 }, 2 => if c { 3 } else { 4 }, 3 => unsafe { 5 }, _ => 6, } }", "fn f() { match a { 1 => { 2 } 2 => if c { 3 } else { 4 } 3 => unsafe { 5 } _ => 6 } }"),
            // E3: an empty statement.
            ("fn f() { ; let a = 1;; g();; fn h() {}; }", "fn f() { let a = 1; g(); fn h() {} }"),
            // E4: the comma after the last predicate of a `where` clause.
            ("fn f<T, U>() where T: Clone, U: Copy, {}", "fn f<T, U>() where T: Clone, U: Copy {}"),
        ];
        for (with, without) in same {
            assert_eq!(text(with), text(without), "{with}");
        }
        let different = [
            // A tuple with one element.
            ("fn f() { (a,) }", "fn f() { (a) }"),
            ("fn f() -> (u8,) {}", "fn f() -> (u8) {}"),
            ("fn f() { let (a,) = b; }", "fn f() { let (a) = b; }"),
            // A token tree: the macro can read each token.
            ("fn f() { m!(a, b,); }", "fn f() { m!(a, b); }"),
            ("#[derive(A, B,)] struct S;", "#[derive(A, B)] struct S;"),
            ("fn f() { m! { fn g() {}; } }", "fn f() { m! { fn g() {} } }"),
            // The `;` of a statement.
            ("fn f() { if c { 1 } else { 2 }; }", "fn f() { if c { 1 } else { 2 } }"),
            ("fn f() { m! {}; a }", "fn f() { m! {} a }"),
            ("fn f() { g(); }", "fn f() { g() }"),
        ];
        for (with, without) in different {
            assert_ne!(text(with), text(without), "{with}");
        }
    }

    fn text(source: &str) -> String {
        source.parse::<TokenStream>().unwrap().to_string()
    }

    #[test]
    fn the_text_form_has_the_spaces_of_proc_macro() {
        assert_eq!(text("#[cfg(feature = \"std\")]"), "# [cfg (feature = \"std\")]");
        assert_eq!(text("pub(crate) fn f<'a>(x: &'a u8) -> u8 { x::y }"), "pub (crate) fn f < 'a > (x : & 'a u8) -> u8 { x :: y }");
        assert_eq!(text("a: :b {}"), "a : : b { }");
        assert_eq!(text("/// one\nfn f() {}"), "# [doc = \" one\"] fn f () { }");
        assert!("(a".parse::<TokenStream>().is_err());
    }

    #[test]
    fn the_places_are_lines_and_columns_of_the_text() {
        let stream = TokenStream::with_places("a\n  (b)").unwrap();
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        assert_eq!(trees[1].span().start(), LineColumn { line: 2, column: 2 });
        let TokenTree::Group(group) = &trees[1] else { panic!() };
        assert_eq!(group.span_close().start(), LineColumn { line: 2, column: 4 });
    }

    #[test]
    fn the_flat_text_is_the_text_of_the_token_tree() {
        let source = "#[cfg(x)] /// one\nfn f<'a>(x: &'a u8) -> u8 { m!(a::b, $c:expr) + g() } struct S {}";
        let file = crate::rs::parse_file(source, Edition::Edition2021).unwrap();
        for node in file.syntax().descendants() {
            assert_eq!(compare_text(&node), TokenStream::from_node(&node).to_string(), "{node:?}");
        }
    }

    #[test]
    fn the_display_text_keeps_the_spaces_of_the_source() {
        let file = crate::rs::parse_file("#[cfg( feature=\"std\" )]\nfn f() {\n    // a note\n    g(1,2)\n}\n", Edition::Edition2021).unwrap();
        let function = file.syntax().descendants().find(|node| node.kind() == SyntaxKind::FN).unwrap();
        assert_eq!(display_text(&function), "#[cfg( feature=\"std\" )] fn f() { g(1,2) }");
        assert_eq!(compare_text(&function), "# [cfg (feature = \"std\")] fn f () { g (1 , 2) }");
    }

    #[test]
    fn a_node_outside_a_macro_has_alone_marks() {
        let file = crate::rs::parse_file("fn f() { let a = -x; m!(a=-x); }", Edition::Edition2021).unwrap();
        let body = file.syntax().descendants().find(|node| node.kind() == SyntaxKind::STMT_LIST).unwrap();
        assert_eq!(compare_text(&body), "{ let a = - x ; m ! (a = - x) ; }");
    }
}
