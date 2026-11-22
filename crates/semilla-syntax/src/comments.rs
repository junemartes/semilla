//! The comments of a node of the lossless syntax tree (design 58.1). Each text comes from the
//! tokens of the tree: no part of this module searches a source text.
//!
//! - A unit is the docs of one node, one block of plain comment lines, or one plain comment
//!   at the end of a line.
//! - A unit is attached to a node as its docs, in its attributes, above it, or at the end of
//!   its first or its last line.

use std::collections::BTreeMap;

use ra_ap_syntax::{ast, AstNode, Edition, NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize};

use crate::rs;
use crate::tree::Pos;

/// The kind of a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    /// The doc comments and the `#[doc = "…"]` attributes of one node.
    Docs,
    /// A block of plain comments: each comment starts its line, and no empty line is between
    /// two comments of the block.
    Block,
    /// A plain comment after a token that is not a comment on the same line. No token is
    /// after it on the line.
    LineEnd,
}

/// One unit of the comments of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub kind: UnitKind,
    /// The text of the tokens, as written. Each token of a block is one line (or more for a
    /// `/* */` comment). Each CRLF is LF.
    pub text: String,
    /// The byte offset of the first token.
    pub offset: usize,
    /// The first line and the last line (from 1).
    pub lines: (usize, usize),
    /// The parts of the unit: one part for each doc comment line, each `#[doc = "…"]`
    /// attribute, and each plain comment token. The text of the unit is the parts with one
    /// LF between two parts.
    pub parts: Vec<Part>,
}

/// One part of a unit: the text of one token (of one attribute for `#[doc = "…"]`), and its
/// first line and last line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub lines: (usize, usize),
}

/// How a unit is attached to a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attach {
    /// The docs of the node.
    Docs,
    /// A plain comment between the first attribute or doc line of the node and its first
    /// token that is not in an attribute.
    Attributes,
    /// The block of plain comments directly above the first line of the node.
    Above,
    /// The plain comment at the end of the first line of the code of the node, or at the end
    /// of its last line.
    LineEnd,
}

/// One node of the path from a changed element up to its item.
#[derive(Clone, Debug)]
pub struct Step {
    /// The first line of the code of the node (after its attributes and docs).
    pub line: usize,
    /// The byte offset of the first token of the node that is not a plain comment.
    pub offset: usize,
    /// True for the item at the end of the path (the last node of a path).
    pub item: bool,
    /// The units of the node: the index in [`FileComments::units`].
    pub units: Vec<(Attach, usize)>,
}

/// The comments of one file.
pub struct FileComments {
    root: SyntaxNode,
    text: String,
    starts: Vec<usize>,
    /// Each unit of the file, in source order.
    pub units: Vec<Unit>,
    /// The docs unit of a node: (the offset of the node, inner docs) → the unit.
    docs: BTreeMap<(usize, bool), usize>,
    /// The block unit by its last line.
    blocks: BTreeMap<usize, usize>,
    /// The line-end unit by its first line.
    line_ends: BTreeMap<usize, usize>,
}

/// The text of a comment token or of a doc attribute, with LF line ends.
fn shown(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end_matches('\r').to_string()
}

/// True for a doc comment node, or for an attribute `#[doc = …]` (the second value is true
/// for inner docs).
fn doc_part(node: &SyntaxNode) -> Option<bool> {
    match node.kind() {
        SyntaxKind::DOC_COMMENT => Some(node.first_token().is_some_and(|token| token.kind() == SyntaxKind::INNER_DOC_COMMENT)),
        SyntaxKind::ATTR => {
            let compact: String = node.text().to_string().chars().filter(|c| !c.is_whitespace()).collect();
            if compact.starts_with("#[doc=") {
                Some(false)
            } else if compact.starts_with("#![doc=") {
                Some(true)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// True for the kind of an item of the lossless tree: a function, a type, a trait, an `impl`,
/// a module, a constant, a `use` line, a macro definition, or a macro call at the place of an
/// item. A field and a variant are parts of their item.
pub fn is_item(node: &SyntaxNode) -> bool {
    if node.kind() == SyntaxKind::MACRO_CALL {
        return !node.parent().is_some_and(|parent| matches!(parent.kind(), SyntaxKind::MACRO_EXPR | SyntaxKind::MACRO_TYPE | SyntaxKind::MACRO_PAT));
    }
    ast::Item::can_cast(node.kind())
}

/// The first token of an element that is not white space and not a plain comment.
fn first_token(element: &SyntaxElement) -> Option<SyntaxToken> {
    match element {
        NodeOrToken::Token(token) => Some(token.clone()),
        NodeOrToken::Node(node) => {
            let range = node.text_range();
            let mut token = node.first_token();
            while let Some(now) = token.clone().filter(|now| now.kind().is_trivia()) {
                token = now.next_token();
            }
            token.filter(|token| range.contains_range(token.text_range()))
        }
    }
}

/// The last token of an element that is not white space and not a plain comment.
fn last_token(element: &SyntaxElement) -> Option<SyntaxToken> {
    match element {
        NodeOrToken::Token(token) => Some(token.clone()),
        NodeOrToken::Node(node) => {
            let range = node.text_range();
            let mut token = node.last_token();
            while let Some(now) = token.clone().filter(|now| now.kind().is_trivia()) {
                token = now.prev_token();
            }
            token.filter(|token| range.contains_range(token.text_range()))
        }
    }
}

/// The first token of the code of a node: the first token that is not in an attribute or a
/// doc comment of the node.
fn code_token(element: &SyntaxElement) -> Option<SyntaxToken> {
    let NodeOrToken::Node(node) = element else { return first_token(element) };
    let child = node.children_with_tokens().find(|child| !child.kind().is_trivia() && !matches!(child.kind(), SyntaxKind::ATTR | SyntaxKind::DOC_COMMENT))?;
    first_token(&child)
}

/// True if no token is before this token on its line.
fn starts_line(token: &SyntaxToken) -> bool {
    match token.prev_token() {
        None => true,
        Some(before) => before.kind() == SyntaxKind::WHITESPACE && before.text().contains('\n'),
    }
}

/// The first token of the line of a token that is not white space.
fn line_first(token: &SyntaxToken) -> SyntaxToken {
    let mut first = token.clone();
    let mut now = token.clone();
    while let Some(before) = now.prev_token() {
        if before.kind() == SyntaxKind::WHITESPACE && before.text().contains('\n') {
            break;
        }
        if before.kind() != SyntaxKind::WHITESPACE {
            first = before.clone();
        }
        now = before;
    }
    first
}

/// True if no token is after this token on its last line.
fn ends_line(token: &SyntaxToken) -> bool {
    match token.next_token() {
        None => true,
        Some(after) => after.kind() == SyntaxKind::WHITESPACE && after.text().contains('\n'),
    }
}

impl FileComments {
    /// The units of a file. A file with a syntax error gives the units of the tree that the
    /// parser makes.
    pub fn new(text: &str, edition: Edition) -> FileComments {
        let (file, _) = rs::parse_checked(text, edition);
        let root = file.syntax().clone();
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        let mut comments = FileComments { root: root.clone(), text: text.to_string(), starts, units: Vec::new(), docs: BTreeMap::new(), blocks: BTreeMap::new(), line_ends: BTreeMap::new() };
        let mut units: Vec<(Unit, Option<(usize, bool)>)> = Vec::new();
        // The docs of each node. A doc comment in a token tree is a token of a macro input.
        for node in root.descendants().filter(|node| node.kind() != SyntaxKind::TOKEN_TREE) {
            for inner in [false, true] {
                let parts: Vec<SyntaxNode> = node.children().filter(|child| doc_part(child) == Some(inner)).collect();
                let (Some(first), Some(last)) = (parts.first(), parts.last()) else { continue };
                let line_of = |part: &SyntaxNode| {
                    let start = usize::from(part.text_range().start());
                    (comments.line(start), comments.line(usize::from(part.text_range().end()).saturating_sub(1).max(start)))
                };
                let parts: Vec<Part> = parts.iter().map(|part| Part { text: shown(&part.text().to_string()), lines: line_of(part) }).collect();
                let text = parts.iter().map(|part| part.text.as_str()).collect::<Vec<_>>().join("\n");
                let offset = usize::from(first.text_range().start());
                let lines = (comments.line(offset), comments.line(usize::from(last.text_range().end()).saturating_sub(1).max(offset)));
                units.push((Unit { kind: UnitKind::Docs, text, offset, lines, parts }, Some((usize::from(node.text_range().start()), inner))));
            }
        }
        // The plain comments, in source order.
        let mut block: Option<Unit> = None;
        for token in root.descendants_with_tokens().filter_map(|element| element.into_token()).filter(|token| token.kind() == SyntaxKind::COMMENT) {
            let offset = usize::from(token.text_range().start());
            let lines = (comments.line(offset), comments.line(usize::from(token.text_range().end()).saturating_sub(1).max(offset)));
            let text = shown(token.text());
            if !starts_line(&token) {
                // A comment after a token on its line. With no token after it, it is a
                // line-end unit. A comment between two tokens of a line is a unit of its own.
                units.extend(block.take().map(|unit| (unit, None)));
                units.push((Unit { kind: if ends_line(&token) { UnitKind::LineEnd } else { UnitKind::Block }, parts: vec![Part { text: text.clone(), lines }], text, offset, lines }, None));
                continue;
            }
            // The comment starts its line. It continues the block if the block ends on the
            // line directly before it.
            let between = token.prev_token().map(|space| space.text().matches('\n').count()).unwrap_or(0);
            match &mut block {
                Some(unit) if unit.lines.1 + 1 == lines.0 && between == 1 && unit.kind == UnitKind::Block => {
                    unit.text.push('\n');
                    unit.text.push_str(&text);
                    unit.lines.1 = lines.1;
                    unit.parts.push(Part { text, lines });
                }
                _ => {
                    units.extend(block.take().map(|unit| (unit, None)));
                    block = Some(Unit { kind: UnitKind::Block, parts: vec![Part { text: text.clone(), lines }], text, offset, lines });
                }
            }
            // A comment with a token after it on its line ends the block.
            if !ends_line(&token) {
                units.extend(block.take().map(|unit| (unit, None)));
            }
        }
        units.extend(block.take().map(|unit| (unit, None)));
        units.sort_by_key(|(unit, _)| unit.offset);
        for (index, (unit, owner)) in units.into_iter().enumerate() {
            match (unit.kind, owner) {
                (UnitKind::Docs, Some(owner)) => {
                    comments.docs.insert(owner, index);
                }
                (UnitKind::Block, _) => {
                    comments.blocks.insert(unit.lines.1, index);
                }
                (UnitKind::LineEnd, _) => {
                    comments.line_ends.insert(unit.lines.0, index);
                }
                _ => {}
            }
            comments.units.push(unit);
        }
        comments
    }

    /// The line (from 1) of a byte offset.
    fn line(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset)
    }

    /// The position of a byte offset: the line (from 1) and the column in characters (from 0).
    pub fn pos(&self, offset: usize) -> Pos {
        let line = self.line(offset);
        let start = self.starts[line - 1];
        (line, self.text.get(start..offset).map_or(offset - start, |part| part.chars().count()))
    }

    /// The byte offset of a position (a line from 1, a column in characters from 0).
    fn offset(&self, pos: Pos) -> Option<usize> {
        let start = *self.starts.get(pos.0.checked_sub(1)?)?;
        let end = self.starts.get(pos.0).copied().unwrap_or(self.text.len());
        let row = self.text.get(start..end)?;
        Some(start + row.char_indices().nth(pos.1).map_or(row.len(), |(at, _)| at))
    }

    /// The smallest element (a node or a token) that has the range from `start` to `end`.
    pub fn element(&self, start: Pos, end: Pos) -> Option<SyntaxElement> {
        let (start, end) = (self.offset(start)?, self.offset(end)?);
        if start >= end || end > self.text.len() {
            return None;
        }
        let range = TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32));
        if !self.root.text_range().contains_range(range) {
            return None;
        }
        let found = self.root.covering_element(range);
        // The tokens of a macro input have no nodes. A range of tokens in a token tree gives
        // its first token: the line of that token is the place of the range.
        if found.kind() == SyntaxKind::TOKEN_TREE && found.text_range() != range {
            let token = self.root.token_at_offset(range.start()).right_biased().filter(|token| !token.kind().is_trivia())?;
            return Some(NodeOrToken::Token(token));
        }
        Some(found)
    }

    /// The largest node that starts with the token at this position. For the first position
    /// of an item of the loader (its first attribute, doc comment, or token), it is the node
    /// of the item.
    pub fn node_at(&self, start: Pos) -> Option<SyntaxElement> {
        let offset = self.offset(start)?;
        let token = self.root.token_at_offset(TextSize::new(offset as u32)).right_biased()?;
        if token.kind().is_trivia() {
            return None;
        }
        let mut found: Option<SyntaxNode> = None;
        for node in token.parent_ancestors() {
            if node.kind() == SyntaxKind::SOURCE_FILE || first_token(&NodeOrToken::Node(node.clone())).as_ref() != Some(&token) {
                break;
            }
            found = Some(node);
        }
        Some(found.map_or(NodeOrToken::Token(token), NodeOrToken::Node))
    }

    /// The units that are attached to one element (design 58.1), in source order.
    pub fn attached(&self, element: &SyntaxElement) -> Vec<(Attach, usize)> {
        let mut out: Vec<(Attach, usize)> = Vec::new();
        let Some(first) = first_token(element) else { return out };
        let code = code_token(element);
        let add = |out: &mut Vec<(Attach, usize)>, attach: Attach, unit: usize| {
            if !out.iter().any(|(_, known)| *known == unit) {
                out.push((attach, unit));
            }
        };
        // The block directly above the first line of the node. A token of a macro input is in
        // no node of its own: its line is its place, so the rule uses the first token of the
        // line.
        let in_macro = matches!(element, NodeOrToken::Token(token) if token.parent().is_some_and(|parent| parent.kind() == SyntaxKind::TOKEN_TREE));
        let first = if in_macro { line_first(&first) } else { first };
        if starts_line(&first) {
            let line = self.line(usize::from(first.text_range().start()));
            if let Some(unit) = line.checked_sub(1).and_then(|above| self.blocks.get(&above)) {
                // The block is the last thing before the node: one line end is between them.
                let between = first.prev_token().filter(|space| space.text().matches('\n').count() == 1).and_then(|space| space.prev_token());
                if between.is_some_and(|token| token.kind() == SyntaxKind::COMMENT) {
                    add(&mut out, Attach::Above, *unit);
                }
            }
        }
        if let NodeOrToken::Node(node) = element {
            // The docs of the node, and the inner docs of its list of items or statements.
            let start = usize::from(node.text_range().start());
            if let Some(unit) = self.docs.get(&(start, false)) {
                add(&mut out, Attach::Docs, *unit);
            }
            let lists = node.children().flat_map(|child| if child.kind() == SyntaxKind::BLOCK_EXPR { child.children().collect::<Vec<_>>() } else { vec![child] });
            for list in lists.filter(|list| matches!(list.kind(), SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST | SyntaxKind::EXTERN_ITEM_LIST | SyntaxKind::STMT_LIST)) {
                if let Some(unit) = self.docs.get(&(usize::from(list.text_range().start()), true)) {
                    add(&mut out, Attach::Docs, *unit);
                }
            }
            // The plain comments in the attributes of the node.
            if let Some(code) = code.as_ref().filter(|code| **code != first) {
                let (from, to) = (usize::from(first.text_range().start()), usize::from(code.text_range().start()));
                for (index, _) in self.units.iter().enumerate().filter(|(_, unit)| unit.kind != UnitKind::Docs && from < unit.offset && unit.offset < to) {
                    add(&mut out, Attach::Attributes, index);
                }
            }
        }
        // The comment at the end of the first line of the code, and of the last line.
        for token in [code, last_token(element)].into_iter().flatten() {
            let line = self.line(usize::from(token.text_range().end()).saturating_sub(1).max(usize::from(token.text_range().start())));
            if let Some(unit) = self.line_ends.get(&line) {
                if self.units[*unit].offset > usize::from(token.text_range().start()) {
                    add(&mut out, Attach::LineEnd, *unit);
                }
            }
        }
        out.sort_by_key(|(_, unit)| self.units[*unit].offset);
        out
    }

    /// The inner docs of the file (the docs of the module of the file).
    pub fn inner_docs(&self) -> Option<usize> {
        self.docs.get(&(usize::from(self.root.text_range().start()), true)).copied()
    }

    /// The path from an element up to the item that has it (the item is the last step).
    /// `end` is the position of the first token of the item of Semilla that has the element:
    /// an item in a body (a local `use` line, a local function) is a statement of that body
    /// and a node of the path. With no `end`, and with an `end` that is not above the element,
    /// the path ends at the innermost item of the syntax tree. `None` if no item has the
    /// element.
    pub fn path(&self, element: &SyntaxElement, end: Option<Pos>) -> Option<Vec<Step>> {
        let mut out = Vec::new();
        // The count of the steps up to the innermost item of the syntax tree.
        let mut innermost: Option<usize> = None;
        let mut found = false;
        let mut now = Some(element.clone());
        while let Some(element) = now.filter(|_| !found) {
            let item = matches!(&element, NodeOrToken::Node(node) if is_item(node));
            if let Some(first) = first_token(&element) {
                let code = code_token(&element).unwrap_or_else(|| first.clone());
                let offset = usize::from(first.text_range().start());
                out.push(Step { line: self.line(usize::from(code.text_range().start())), offset, item: false, units: self.attached(&element) });
                if item {
                    innermost.get_or_insert(out.len());
                    found = end.is_none_or(|end| self.pos(offset) == end);
                }
            }
            now = element.parent().map(NodeOrToken::Node);
        }
        // No item starts at `end`: the path ends at the innermost item.
        if !found {
            out.truncate(innermost?);
        }
        out.last_mut()?.item = true;
        Some(out)
    }

    /// Each unit of an item, in source order: the units in the item, the block above it, and
    /// the comment at the end of its last line. `offset` is the offset of the item step.
    pub fn item_units(&self, item: &Step) -> Vec<usize> {
        let Some(token) = self.root.token_at_offset(TextSize::new(item.offset as u32)).right_biased() else { return Vec::new() };
        let Some(node) = token.parent_ancestors().filter(|node| is_item(node)).find(|node| first_token(&NodeOrToken::Node(node.clone())).as_ref() == Some(&token)) else { return Vec::new() };
        let range = node.text_range();
        let (from, to) = (usize::from(range.start()), usize::from(range.end()));
        let mut out: Vec<usize> = self.units.iter().enumerate().filter(|(_, unit)| from <= unit.offset && unit.offset < to).map(|(index, _)| index).collect();
        out.extend(item.units.iter().map(|(_, unit)| *unit));
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "//! Inner.\n\n// lead\n// two\n/// Doc.\n// mid\n#[inline] // why\npub fn f(x: u32) -> u32 { // open\n    // above\n    let t = x / 10; // ten\n\n    // far\n\n    m!(1, // in\n 2);\n    x\n    // below\n} // close\nmod a {\n    //! In a.\n    struct S {\n        /// d\n        x: u8, // e\n    }\n}\n";

    fn file() -> FileComments {
        FileComments::new(TEXT, Edition::Edition2021)
    }

    fn texts(file: &FileComments, element: &SyntaxElement) -> Vec<(Attach, String)> {
        file.attached(element).into_iter().map(|(attach, unit)| (attach, file.units[unit].text.clone())).collect()
    }

    // rule+ R58.1-1
    // rule+ R58.1-3
    // rule- R58.1-3
    // rule+ R58.1-4
    // rule+ R58.1-5
    #[test]
    fn an_item_has_its_docs_its_block_above_and_the_comments_of_its_lines() {
        let file = file();
        let item = file.node_at((5, 0)).unwrap();
        let found = texts(&file, &item);
        assert_eq!(
            found,
            [
                (Attach::Above, "// lead\n// two".to_string()),
                (Attach::Docs, "/// Doc.".to_string()),
                (Attach::Attributes, "// mid".to_string()),
                (Attach::Attributes, "// why".to_string()),
                (Attach::LineEnd, "// open".to_string()),
                (Attach::LineEnd, "// close".to_string()),
            ]
        );
    }

    // rule- R58.1-4
    // rule+ R58.1-5
    // rule+ R58.1-8
    #[test]
    fn a_comment_below_a_node_and_a_comment_after_an_empty_line_are_not_attached() {
        let file = file();
        // The `let` statement: the block above it and the comment at the end of its line.
        let statement = file.element((10, 4), (10, 19)).unwrap();
        assert_eq!(texts(&file, &statement), [(Attach::Above, "// above".to_string()), (Attach::LineEnd, "// ten".to_string())]);
        // The macro call: an empty line is between `// far` and the call. The comment in its
        // input is at the end of its first line.
        let call = file.element((14, 4), (15, 4)).unwrap();
        assert_eq!(texts(&file, &call), [(Attach::LineEnd, "// in".to_string())]);
        // The tail `x`: `// below` is below it, so it is not a comment of `x`.
        let tail = file.element((16, 4), (16, 5)).unwrap();
        assert_eq!(texts(&file, &tail), []);
    }

    // rule+ R58.3-1
    // rule+ R58.3-2
    // rule- R58.3-2
    #[test]
    fn a_path_ends_at_its_item() {
        let file = file();
        // The literal `10`: the literal, the expression, the statement, the body, the function.
        let literal = file.element((10, 16), (10, 18)).unwrap();
        let path = file.path(&literal, None).unwrap();
        assert!(path.last().unwrap().item && path.iter().filter(|step| step.item).count() == 1);
        let all: Vec<&str> = path.iter().flat_map(|step| step.units.iter()).map(|(_, unit)| file.units[*unit].text.as_str()).collect();
        assert!(all.contains(&"// above") && all.contains(&"/// Doc.") && !all.contains(&"//! Inner."), "{all:?}");
        // The type of the field: the field, then the struct. The module is not in the path.
        let field = file.element((23, 11), (23, 13)).unwrap();
        let path = file.path(&field, None).unwrap();
        let all: Vec<&str> = path.iter().flat_map(|step| step.units.iter()).map(|(_, unit)| file.units[*unit].text.as_str()).collect();
        assert!(all.contains(&"/// d") && all.contains(&"// e") && !all.contains(&"//! In a."), "{all:?}");
        // The path with an end goes up through an item in a body to the item at the end. An
        // end that is not above the element gives the innermost item.
        let local = FileComments::new("/// Out.\nfn out() {\n    /// In.\n    fn inner() {\n        // step\n        let a = 1;\n    }\n}\n", Edition::Edition2021);
        let texts = |end: Option<Pos>| -> Vec<String> {
            let path = local.path(&local.element((6, 16), (6, 17)).unwrap(), end).unwrap();
            assert!(path.last().unwrap().item && path.iter().filter(|step| step.item).count() == 1);
            path.iter().flat_map(|step| step.units.iter()).map(|(_, unit)| local.units[*unit].text.clone()).collect()
        };
        assert_eq!(texts(Some((1, 0))), ["// step", "/// In.", "/// Out."]);
        assert_eq!(texts(None), ["// step", "/// In."]);
        assert_eq!(texts(Some((6, 8))), ["// step", "/// In."]);
        // The file has no item around its inner docs.
        assert_eq!(file.inner_docs().map(|unit| file.units[unit].text.as_str()), Some("//! Inner."));
    }

    // rule+ R58.1-2
    #[test]
    fn a_module_has_its_inner_docs() {
        let file = file();
        let module = file.node_at((19, 0)).unwrap();
        assert_eq!(texts(&file, &module), [(Attach::Docs, "//! In a.".to_string())]);
    }

    // rule+ R58.1-7
    // rule- R58.1-7
    #[test]
    fn the_text_of_a_unit_is_the_text_of_its_tokens() {
        let text = "/** One.\r\n    Two. */\r\n#[doc = \"Three.\"]\r\nfn f() {}\r\n/* a\n   b */\nfn g() {}\n";
        let file = FileComments::new(text, Edition::Edition2021);
        let found: Vec<&str> = file.units.iter().map(|unit| unit.text.as_str()).collect();
        assert_eq!(found, ["/** One.\n    Two. */\n#[doc = \"Three.\"]", "/* a\n   b */"]);
        // A text in a string that looks like a comment is no comment.
        let file = FileComments::new("fn f() -> &'static str {\n    \"// no comment\"\n}\n", Edition::Edition2021);
        assert!(file.units.is_empty());
    }
}
