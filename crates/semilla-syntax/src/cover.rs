//! The tokens of a Rust file with their lines (design 55.2). The verifier gets the position of
//! each fact and the cover check from them. The tokens come from the syntax tree: no part of
//! this module searches a source text.

use std::hash::{DefaultHasher, Hash, Hasher};

use ra_ap_syntax::ast::{self, HasName};
use ra_ap_syntax::{AstNode, Edition, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::rs::{self, Lines};
use crate::tree::Pos;
use crate::tt;

/// The class of a token: the part of an item that owns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    Code,
    Visibility,
    Attribute,
    Doc,
    Comment,
}

/// One token of a file, or one plain comment.
#[derive(Clone, Debug)]
pub struct LineToken {
    /// The text that the compare text has: each CRLF is LF. A plain comment has the text of
    /// the loader (design 39.1, rule 4).
    pub text: String,
    /// The line (from 1) and the column (in characters, from 0) of the first character.
    pub start: Pos,
    /// The line of the last character.
    pub end_line: usize,
    pub class: TokenClass,
    /// True for a token of an inner attribute or of an inner doc comment of the file.
    pub top: bool,
    /// True for a token with no meaning in the grammar (design 52.2, E1 to E4).
    pub void: bool,
}

/// One item of a module (not an item of a body, an `impl`, or a trait), for the rule of the
/// item order (design 55.5).
#[derive(Clone, Debug)]
pub struct ModuleItem {
    /// The names of the modules around the item in the file, with `::` between them.
    pub container: String,
    /// The text of each token and each plain comment of the item, in order.
    pub key: String,
    /// The first and the last line of the item, with the plain comments before it.
    pub lines: (usize, usize),
    /// True for a `use` item.
    pub is_use: bool,
    /// True for an item that can give a macro to the items after it: a `macro_rules!`
    /// definition, an item with one in it, an item with a `macro_use` attribute, or a macro
    /// call (its expansion can have a definition).
    pub source: bool,
    /// A hash of the keys of the `source` items before the item in its container. A macro has
    /// a textual scope, so an item can move only between the same `source` items.
    pub barrier: u64,
}

/// The tokens and the module items of one file.
#[derive(Clone, Debug, Default)]
pub struct Shape {
    pub tokens: Vec<LineToken>,
    pub items: Vec<ModuleItem>,
}

/// The tokens and the plain comments of a file, in source order. A file with a syntax error
/// gives the tokens of the tree that the parser makes: each character of the file that is not
/// white space is in a token.
pub fn line_tokens(text: &str, edition: Edition) -> Vec<LineToken> {
    shape(text, edition).tokens
}

/// The tokens and the module items of a file.
pub fn shape(text: &str, edition: Edition) -> Shape {
    let (file, _) = rs::parse_checked(text, edition);
    let lines = Lines::new(text);
    let mut tokens = Vec::new();
    let mut offsets = Vec::new();
    for token in file.syntax().descendants_with_tokens().filter_map(|element| element.into_token()) {
        let kind = token.kind();
        if kind == SyntaxKind::WHITESPACE {
            continue;
        }
        let range = token.text_range();
        let start = lines.pos(usize::from(range.start()));
        let end_line = lines.pos(usize::from(range.end()).saturating_sub(1).max(usize::from(range.start()))).0;
        let (text, class, top, void) = if kind == SyntaxKind::COMMENT {
            let comment = token.text();
            let shown = if comment.starts_with("//") { comment.trim().to_string() } else { comment.split_whitespace().collect::<Vec<_>>().join(" ") };
            (shown, TokenClass::Comment, false, false)
        } else {
            let (class, top) = class(&token);
            let in_tree = token.parent_ancestors().any(|node| node.kind() == SyntaxKind::TOKEN_TREE);
            match tt::flat(&token, in_tree) {
                Some(flat) => (flat.text, class, top, flat.void),
                // A token that the compare text does not have (a shebang line).
                None => (token.text().replace("\r\n", "\n"), class, top, false),
            }
        };
        offsets.push(usize::from(range.start()));
        tokens.push(LineToken { text, start, end_line, class, top, void });
    }
    let mut items = Vec::new();
    module_items(file.syntax(), "", &tokens, &offsets, &mut items);
    Shape { tokens, items }
}

/// True for an item that can give a macro to the items after it.
fn macro_source(item: &SyntaxNode) -> bool {
    item.kind() == SyntaxKind::MACRO_CALL
        || item.descendants().any(|node| match node.kind() {
            SyntaxKind::MACRO_RULES => true,
            SyntaxKind::ATTR => node.descendants_with_tokens().filter_map(|element| element.into_token()).find(|token| token.kind() == SyntaxKind::IDENT).is_some_and(|name| name.text() == "macro_use"),
            _ => false,
        })
}

/// Adds each item of a module, and the items of its inline modules.
fn module_items(owner: &SyntaxNode, container: &str, tokens: &[LineToken], offsets: &[usize], out: &mut Vec<ModuleItem>) {
    let mut barrier = DefaultHasher::new();
    for node in owner.children() {
        if ast::Item::cast(node.clone()).is_none() {
            continue;
        }
        let source = macro_source(&node);
        let range = node.text_range();
        let from = offsets.partition_point(|offset| *offset < usize::from(range.start()));
        let to = offsets.partition_point(|offset| *offset < usize::from(range.end()));
        if from < to {
            let key = tokens[from..to].iter().filter(|token| !token.void).map(|token| token.text.as_str()).collect::<Vec<_>>().join(" ");
            let before = barrier.finish();
            if source {
                key.hash(&mut barrier);
            }
            out.push(ModuleItem { container: container.to_string(), key, lines: (tokens[from].start.0, tokens[to - 1].end_line), is_use: node.kind() == SyntaxKind::USE, source, barrier: before });
        }
        if let Some(module) = ast::Module::cast(node.clone()) {
            if let Some(list) = module.item_list() {
                let name = module.name().map(|name| name.syntax().text().to_string()).unwrap_or_default();
                let inner = if container.is_empty() { name } else { format!("{container}::{name}") };
                module_items(list.syntax(), &inner, tokens, offsets, out);
            }
        }
    }
}

/// The class of a token that is not a plain comment, and true for a token of an inner
/// attribute or an inner doc comment of the file.
fn class(token: &SyntaxToken) -> (TokenClass, bool) {
    if let Some((inner, _)) = tt::doc_comment(token.kind(), token.text()) {
        // A doc comment in a token tree is a token of the macro input, or of the macro body.
        if token.parent_ancestors().any(|node| node.kind() == SyntaxKind::TOKEN_TREE) {
            return (TokenClass::Code, false);
        }
        let of_file = token.parent().and_then(|doc| doc.parent()).is_some_and(|owner| owner.kind() == SyntaxKind::SOURCE_FILE);
        return (TokenClass::Doc, inner && of_file);
    }
    for node in token.parent_ancestors() {
        match node.kind() {
            SyntaxKind::ATTR => {
                let of_file = node.parent().is_some_and(|owner| owner.kind() == SyntaxKind::SOURCE_FILE);
                let inner = node.children_with_tokens().any(|element| element.kind() == SyntaxKind::BANG);
                // A doc attribute with a text (`#[doc = …]`) is a part of the docs, as a doc
                // comment is. `#[doc(hidden)]` is an attribute.
                let compact: String = node.text().to_string().chars().filter(|c| !c.is_whitespace()).collect();
                let class = if compact.starts_with("#[doc=") || compact.starts_with("#![doc=") { TokenClass::Doc } else { TokenClass::Attribute };
                return (class, inner && of_file);
            }
            SyntaxKind::VISIBILITY => return (TokenClass::Visibility, false),
            _ => {}
        }
    }
    (TokenClass::Code, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_token_has_its_line_and_its_class() {
        let text = "//! Top.\n#![allow(unused)]\n// A note.\n/// Doc.\n#[inline]\npub(crate) fn f(a: u8,) {\n    g(a);;\n}\n";
        let tokens = line_tokens(text, Edition::Edition2021);
        let find = |text: &str| tokens.iter().find(|token| token.text == text).unwrap_or_else(|| panic!("{text}"));
        assert!(find("//! Top.").top && find("//! Top.").class == TokenClass::Doc);
        assert!(find("allow").top && find("allow").class == TokenClass::Attribute);
        assert_eq!((find("// A note.").class, find("// A note.").start), (TokenClass::Comment, (3, 0)));
        assert!(!find("/// Doc.").top && find("/// Doc.").class == TokenClass::Doc);
        assert!(!find("inline").top && find("inline").class == TokenClass::Attribute);
        assert_eq!(find("crate").class, TokenClass::Visibility);
        assert_eq!((find("fn").class, find("fn").start), (TokenClass::Code, (6, 11)));
        // The comma before `)` and the second `;` have no meaning (E1, E3).
        assert_eq!(tokens.iter().filter(|token| token.void).map(|token| token.text.as_str()).collect::<Vec<_>>(), [",", ";"]);
        // Negative: the `;` of the statement has a meaning.
        assert_eq!(tokens.iter().filter(|token| token.text == ";" && !token.void).count(), 1);
    }

    #[test]
    fn a_token_of_many_lines_has_its_last_line() {
        let tokens = line_tokens("const A: &str = \"a\r\nb\";\r\n/* c\n d */\n", Edition::Edition2021);
        let literal = tokens.iter().find(|token| token.text.starts_with('"')).unwrap();
        assert_eq!((literal.text.as_str(), literal.start.0, literal.end_line), ("\"a\nb\"", 1, 2));
        let comment = tokens.iter().find(|token| token.class == TokenClass::Comment).unwrap();
        assert_eq!((comment.text.as_str(), comment.start.0, comment.end_line), ("/* c d */", 3, 4));
    }

    #[test]
    fn a_doc_comment_in_a_token_tree_is_code_and_a_doc_attribute_with_a_text_is_docs() {
        let text = "macro_rules! m {\n    () => {\n        /// In a macro.\n        fn f() {}\n    };\n}\n#[doc = \"Text.\"]\n#[doc(hidden)]\n/// Of an item.\nfn g() {}\n";
        let tokens = line_tokens(text, Edition::Edition2021);
        let class_at = |line: usize| tokens.iter().find(|token| token.start.0 == line).map(|token| token.class).unwrap();
        assert_eq!(class_at(3), TokenClass::Code);
        assert_eq!(class_at(7), TokenClass::Doc);
        // Negative: `#[doc(hidden)]` is an attribute, and a doc comment of an item is docs.
        assert_eq!(class_at(8), TokenClass::Attribute);
        assert_eq!(class_at(9), TokenClass::Doc);
    }

    #[test]
    fn a_file_gives_its_module_items() {
        let shape = shape("use a::b;\n// Note.\nfn f() {\n    fn inner() {}\n}\nmod m {\n    use c::d;\n}\n", Edition::Edition2021);
        let items: Vec<(&str, (usize, usize), bool)> = shape.items.iter().map(|item| (item.container.as_str(), item.lines, item.is_use)).collect();
        // The comment before `f` is in the lines of `f`. Negative: `inner` is no module item.
        assert_eq!(items, [("", (1, 1), true), ("", (2, 5), false), ("", (6, 8), false), ("m", (7, 7), true)]);
        assert!(shape.items[1].key.starts_with("// Note. fn f"));
        assert!(shape.items.iter().all(|item| !item.source && item.barrier == shape.items[0].barrier));
    }

    #[test]
    fn an_item_knows_the_macro_sources_before_it() {
        let shape = super::shape("mod a;\n#[macro_use]\nmod m;\nmod b;\nmod c;\nmacro_rules! n { () => {} }\nmod d;\nlocal! {}\nmod e;\nfn f() { call!(); }\n", Edition::Edition2021);
        let sources: Vec<bool> = shape.items.iter().map(|item| item.source).collect();
        // Negative: a macro call in a body (`f`) is not a source.
        assert_eq!(sources, [false, true, false, false, true, false, true, false, false]);
        let barrier: Vec<u64> = shape.items.iter().map(|item| item.barrier).collect();
        // `b` and `c` are between the same sources. Negative: `a`, `d`, and `e` are not.
        assert_eq!(barrier[2], barrier[3]);
        assert_eq!(barrier[7], barrier[8]);
        assert!(barrier[0] != barrier[2] && barrier[3] != barrier[5] && barrier[5] != barrier[7]);
    }
}
