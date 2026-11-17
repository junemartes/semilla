//! The map from each MIR element that differs to a node of the lossless tree, and to the edit
//! that owns it (design 40.2 and 40.3).
//!
//! The driver records the source range of each MIR element. The map gives each range the
//! smallest node of the lossless tree of its side (old MIR to the old tree, new MIR to the new
//! tree) that contains the full range. A range that does not start and end at a token boundary
//! of the tree is a map error.
//!
//! The owner of an element that differs:
//! 1. the edit whose node contains the node of the element, on its side (the smallest such
//!    edit);
//!    1b. else, each edit whose node is inside the node of the element (a call with a changed
//!    argument: the MIR statement of the call has the range of the full call; a block with a
//!    deleted statement). A glue element (a `goto`, a `drop`) whose node is a block needs no
//!    owner, so this rule does not give it to the edits of the block;
//! 2. else, the edit whose node contains the source range of the tokens of the element, when the
//!    tokens are outside the macro call (an element from a `macro_rules` body, or from a field
//!    that a derive reads);
//! 3. else, the link (design 44): each edit that changes a symbol that the element names (a
//!    changed definition of the step, see `links.rs`); a link also adds a cause to a changed
//!    element with a local owner, and an element at the receiver of a method call takes the
//!    links of the call;
//! 4. else, no owner.
//!
//! A moved statement (design 41.7) owns the elements of its nodes on the two sides. An element
//! whose smallest owner is a move also goes to each edit inside its node: a change inside a
//! moved statement keeps its own result. An element with no owner by the rules above, whose
//! node is in a statement that the move passes over (with no edit in it), goes to the move.

use std::collections::{BTreeMap, BTreeSet};

use semilla_syntax::rs::{self, AstNode, TextRange, TextSize};
use semilla_syntax::tree::Pos;
use semilla_syntax::{Edit, EditKind, ItemKind, Workspace};

use crate::{MirDiff, MirElement, MirPlace};

/// The node of a source range.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Mapped {
    pub file: String,
    pub start: Pos,
    pub end: Pos,
    /// The display text of the node (design 39.2), on one line.
    pub text: String,
    /// True if the node contains no statement list.
    pub block_free: bool,
    /// The display text of the method call whose receiver is the node (fix round 2, R3): the
    /// auto-ref, the temporary, and the drop of a receiver belong to the call.
    pub call: Option<String>,
}

/// The lossless tree of one file of one side, with its line index.
struct FileTree {
    lines: rs::Lines,
    root: rs::SyntaxNode,
}

/// The place of an edit node on one side.
#[derive(Clone)]
struct EditPlace {
    edit: usize,
    start: Pos,
    end: Pos,
    /// True for a moved statement (design 41.7).
    wide: bool,
    /// True for a statement that a move passes over: the place owns only an element with no
    /// other owner.
    jump: bool,
}

/// The trees and the edit places of the two sides of a step. The trees are made when a body
/// needs them.
pub struct MirMap<'a> {
    sides: [&'a Workspace; 2],
    trees: [BTreeMap<String, Option<FileTree>>; 2],
    /// The edit places of each side, by file.
    places: [BTreeMap<String, Vec<EditPlace>>; 2],
    /// The count of the map errors, and the first examples.
    pub errors: usize,
    pub examples: Vec<String>,
    /// The counts of the step (design 40.3).
    pub counts: semilla_core::report::MirMapCounts,
    /// The edits that own an element that differs, in a body.
    pub owning: BTreeSet<usize>,
    /// The edits of a changed body that own no element that differs: with each element owned
    /// (`code equal`), and with an element with no owner (the doubt rule).
    pub idle_equal: BTreeSet<usize>,
    pub idle_doubt: BTreeSet<usize>,
    /// The symbol links of the step (design 44).
    links: Option<&'a crate::links::Links<'a>>,
}

/// The most examples of map errors that the report keeps.
const EXAMPLES: usize = 5;

/// The result of the owner rule for one body.
#[derive(Clone, Debug, Default)]
pub struct Ownership {
    /// The edits that own an element that differs, with the elements of each side.
    pub owns: BTreeMap<usize, [Vec<MirElement>; 2]>,
    /// The nodes of the elements that differ with no owner, with their side (0: old, 1: new).
    pub unowned: BTreeSet<(usize, Mapped)>,
    /// The count of the elements that differ with no owner and no node.
    pub lost: usize,
    /// The count of the `goto` elements with no owner (they need no owner).
    pub glue: usize,
    /// True if a move or a variable took an element by its region (design 41.7). Then an edit
    /// of the body that is not a refactoring never gets `code equal` from the owner rule.
    pub by_region: bool,
    /// The count of the elements that a symbol link owns (design 44).
    pub linked: usize,
    /// The symbols of the links of each edit (design 44).
    pub through: BTreeMap<usize, BTreeSet<String>>,
    /// The edits that own an element by its place (rules 1, 1b, and 2).
    pub positional: BTreeSet<usize>,
    /// The edits of the changed symbol of the function of the body, for a changed part that
    /// is not an element (`other`: the signature line), with the symbols (design 44).
    pub other_links: BTreeMap<usize, BTreeSet<String>>,
}

impl Ownership {
    /// True if each element that differs has an owner, and no other part of the text differs.
    pub fn complete(&self, diff: &MirDiff) -> bool {
        self.unowned.is_empty() && self.lost == 0 && !diff.other && (!diff.old.is_empty() || !diff.new.is_empty())
    }
}

impl<'a> MirMap<'a> {
    pub fn new(before: &'a Workspace, after: &'a Workspace, edits: &[Edit]) -> MirMap<'a> {
        Self::with_refactoring(before, after, edits, &BTreeSet::new())
    }

    /// The map with the edits of the refactoring facts (a move, a variable, a rename of a
    /// local): such an edit in a statement does not stop the region of a move (design 41.7).
    pub fn with_refactoring(before: &'a Workspace, after: &'a Workspace, edits: &[Edit], refactoring: &BTreeSet<usize>) -> MirMap<'a> {
        let sides = [before, after];
        let mut places: [BTreeMap<String, Vec<EditPlace>>; 2] = [BTreeMap::new(), BTreeMap::new()];
        for (index, edit) in edits.iter().enumerate() {
            if matches!(edit.kind, EditKind::FileAdded | EditKind::FileRemoved | EditKind::FileChanged | EditKind::Docs | EditKind::Comment) {
                continue;
            }
            for (side, anchor) in [&edit.before, &edit.after].into_iter().enumerate() {
                let Some(anchor) = anchor else { continue };
                let ws = sides[side];
                let Some(item) = ws.items.get(&anchor.item) else { continue };
                // Since design 44, a constant owns the elements of its users by a symbol link.
                let whole = matches!(item.kind, ItemKind::Const | ItemKind::Static) || (item.kind == ItemKind::Macro && item.source.contains("macro_rules!"));
                let (start, end) = match semilla_syntax::resolve(ws, anchor) {
                    Some(semilla_syntax::Resolved::Node(_, node)) => (node.start, node.end),
                    // The place of an edit of a whole item: an added or a removed item, or the
                    // value of a constant or a macro definition. An edit of the header of a
                    // container (its generics, its attributes) does not contain its bodies.
                    Some(semilla_syntax::Resolved::Item(item)) if whole || matches!(edit.kind, EditKind::ItemAdded | EditKind::ItemRemoved) => ((item.line_start, item.col_start), (item.line_end, item.col_end)),
                    // A changed attribute list (fix round 2, R4): the place of each attribute of
                    // the side, so that the code of a derive belongs to the edit of its list.
                    Some(semilla_syntax::Resolved::Item(item)) if edit.kind == EditKind::Attrs => {
                        let text = if side == 0 { edit.source_before.as_deref() } else { edit.source_after.as_deref() };
                        for (start, end) in attribute_places(ws, item, text.unwrap_or("")) {
                            places[side].entry(item.file.clone()).or_default().push(EditPlace { edit: index, start, end, wide: false, jump: false });
                        }
                        continue;
                    }
                    _ => continue,
                };
                let wide = matches!(edit.refactor, Some(semilla_syntax::Refactor::Moved { .. }));
                places[side].entry(item.file.clone()).or_default().push(EditPlace { edit: index, start, end, wide, jump: false });
            }
        }
        // The statements that a move passes over, with no other edit in them (design 41.7). The
        // `let` of an extracted or an inlined variable: the statements after it in its block.
        for (index, edit) in edits.iter().enumerate() {
            let region: [Vec<(Pos, Pos)>; 2] = match &edit.refactor {
                Some(semilla_syntax::Refactor::Moved { jumped }) => jumped.clone(),
                Some(semilla_syntax::Refactor::Extract { region, .. } | semilla_syntax::Refactor::Inline { region, .. }) => region.clone(),
                _ => continue,
            };
            for (side, anchor) in [&edit.before, &edit.after].into_iter().enumerate() {
                let Some(file) = anchor.as_ref().or(edit.before.as_ref()).or(edit.after.as_ref()).and_then(|anchor| sides[side].items.get(&anchor.item)).map(|item| item.file.clone()) else { continue };
                let list = places[side].entry(file).or_default();
                for &(start, end) in &region[side] {
                    let other = list.iter().any(|place| !place.jump && place.edit != index && !refactoring.contains(&place.edit) && start <= place.start && place.end <= end);
                    if !other {
                        list.push(EditPlace { edit: index, start, end, wide: true, jump: true });
                    }
                }
            }
        }
        MirMap { sides, trees: [BTreeMap::new(), BTreeMap::new()], places, errors: 0, examples: Vec::new(), counts: Default::default(), owning: BTreeSet::new(), idle_equal: BTreeSet::new(), idle_doubt: BTreeSet::new(), links: None }
    }

    /// The map with the symbol links of the step (design 44).
    pub fn with_links(mut self, links: &'a crate::links::Links<'a>) -> MirMap<'a> {
        self.links = Some(links);
        self
    }

    /// The node of a source range on one side. `None` for a file that the side does not have,
    /// and for a map error (counted).
    pub fn node(&mut self, side: usize, place: &MirPlace) -> Option<Mapped> {
        let (file, start, end) = (&place.0, place.1, place.2);
        let ws = self.sides[side];
        let tree = self.trees[side].entry(file.clone()).or_insert_with(|| {
            let text = ws.rust_files.get(file)?;
            // The edition of the package of the file (design 40; FIX 1 of design 44).
            let parsed = rs::ast::SourceFile::parse(text, ws.editions.get(file).copied().unwrap_or(rs::Edition::Edition2021)).tree();
            Some(FileTree { lines: rs::Lines::new(text), root: parsed.syntax().clone() })
        });
        let tree = tree.as_ref()?;
        let length = tree.lines.text().len();
        let boundary = |offset: usize| -> bool {
            if offset == 0 || offset == length {
                return true;
            }
            let Ok(at) = u32::try_from(offset) else { return false };
            let at = TextSize::from(at);
            let found = tree.root.token_at_offset(at);
            let ends = |token: Option<rs::SyntaxToken>| token.is_some_and(|token| token.text_range().start() == at || token.text_range().end() == at);
            ends(found.clone().left_biased()) || ends(found.right_biased())
        };
        // A range inside one token is the range of the token if it is in a string literal (a
        // placeholder of a format string), or if it is the last character of the token (the
        // `end_point` of a span in `rustc`).
        let (start, end) = match u32::try_from(start).ok().and_then(|at| tree.root.token_at_offset(TextSize::from(at)).right_biased()) {
            Some(token) if !boundary(start) && usize::from(token.text_range().end()) >= end => {
                let (first, last) = (usize::from(token.text_range().start()), usize::from(token.text_range().end()));
                let last_char = end == last && tree.lines.text().get(start..end).is_some_and(|part| part.chars().count() == 1);
                // A byte string and a C string too: a 2015 file can split `c"…"` into two tokens
                // that the lossless tree keeps as one (design 44).
                let string = matches!(token.kind(), rs::SyntaxKind::STRING | rs::SyntaxKind::BYTE_STRING | rs::SyntaxKind::C_STRING);
                if string || last_char { (first, last) } else { (start, end) }
            }
            _ => (start, end),
        };
        if end > length || start > end || !boundary(start) || !boundary(end) {
            self.errors += 1;
            if self.examples.len() < EXAMPLES {
                let at = tree.lines.pos((start).min(length));
                self.examples.push(format!("{file}:{}:{} (bytes {start}..{end})", at.0, at.1));
            }
            return None;
        }
        let range = TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32));
        // An empty range (the end of a block, the unit value of an `if` with no `else`) belongs
        // to the token that ends at it, if that token is not a space or a comment.
        let left = (start == end).then(|| tree.root.token_at_offset(range.start()).left_biased()).flatten().filter(|token| !token.kind().is_trivia() && token.text_range().end() == range.start());
        let node = match left {
            Some(token) => token.parent()?,
            None => match tree.root.covering_element(range) {
                rs::NodeOrToken::Node(node) => node,
                rs::NodeOrToken::Token(token) => token.parent()?,
            },
        };
        let block_free = node.kind() != rs::SyntaxKind::STMT_LIST && !node.descendants().any(|inner| inner.kind() == rs::SyntaxKind::STMT_LIST);
        let code = rs::code_range(&node);
        // A node with only trivia in the range keeps the range itself.
        let (from, to) = if code.start <= start && end <= code.end { (code.start, code.end) } else { (start, end) };
        let text = semilla_syntax::tree::short(&rs::display_syntax(&node), 60);
        let call = node.parent().and_then(rs::ast::MethodCallExpr::cast).filter(|call| call.receiver().is_some_and(|receiver| receiver.syntax() == &node)).map(|call| semilla_syntax::tree::short(&rs::display_syntax(call.syntax()), 60));
        Some(Mapped { file: file.clone(), start: tree.lines.pos(from), end: tree.lines.pos(to), text, block_free, call })
    }

    /// The smallest edit whose node contains a node on one side.
    fn owner(&self, side: usize, node: &Mapped) -> Option<usize> {
        self.owner_place(side, node).map(|place| place.edit)
    }

    fn owner_place(&self, side: usize, node: &Mapped) -> Option<&EditPlace> {
        let places = self.places[side].get(&node.file)?;
        places
            .iter()
            .filter(|place| !place.jump)
            .filter(|place| place.start <= node.start && node.end <= place.end)
            .max_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)))
    }

    /// The move whose passed-over statement contains a node (design 41.7).
    fn jump_owner(&self, side: usize, node: &Mapped) -> Option<usize> {
        let places = self.places[side].get(&node.file)?;
        places.iter().filter(|place| place.jump && place.start <= node.start && node.end <= place.end).max_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end))).map(|place| place.edit)
    }

    /// The edits whose nodes are inside a node on one side.
    fn inner(&self, side: usize, node: &Mapped) -> Vec<usize> {
        let Some(places) = self.places[side].get(&node.file) else { return Vec::new() };
        let mut found: Vec<usize> = places.iter().filter(|place| !place.jump && node.start <= place.start && place.end <= node.end).map(|place| place.edit).collect();
        found.sort();
        found.dedup();
        found
    }

    /// The owner of each element that differs in a body.
    pub fn own(&mut self, diff: &MirDiff) -> Ownership {
        let mut out = Ownership::default();
        // Pass 1: the local causes (rules 1, 1b, 2, the region of a move, and the waivers of a
        // jump and of glue). A link never removes a local cause (design 44).
        // Each element with its node, its local owners, and true if it is new or deleted code at
        // the place of an edit (rule 1: the node of the edit contains the node of the element;
        // rule 2: the tokens of the element are at the place of the edit).
        let mut checked: Vec<(usize, MirElement, Option<Mapped>, Vec<usize>, bool)> = Vec::new();
        for (side, elements) in [&diff.old, &diff.new].into_iter().enumerate() {
            for element in elements {
                // The place of the element in the body: its tokens if they are in the body
                // (a macro input), else the outermost macro call.
                let inside = |tokens: &MirPlace, call: &MirPlace| tokens.0 == call.0 && call.1 <= tokens.1 && tokens.2 <= call.2;
                let (own, definition) = match (&element.tokens, &element.call) {
                    (Some(tokens), Some(call)) if !inside(tokens, call) => (Some(call), Some(tokens)),
                    (Some(tokens), _) => (Some(tokens), None),
                    (None, Some(call)) => (Some(call), None),
                    (None, None) => (None, None),
                };
                let node = own.and_then(|place| self.node(side, place));
                let first = node.as_ref().and_then(|node| self.owner_place(side, node)).map(|place| (place.edit, place.wide));
                let mut owners: Vec<usize> = first.map(|(edit, _)| edit).into_iter().collect();
                // A move owns the elements of its statement, and each change inside the node of
                // the element owns them too (design 41.7).
                if let (Some((edit, true)), Some(node)) = (first, &node) {
                    owners.extend(self.inner(side, node).into_iter().filter(|inner| *inner != edit));
                }
                // A glue element with no owner, whose node is a block or the body, follows a
                // change at a different place (see below).
                let glue = owners.is_empty() && element.glue && node.as_ref().is_none_or(|node| !node.block_free);
                // Rule 1b: a node that contains edits (a call with a changed argument, a block
                // with a deleted statement): each edit in the node owns the element.
                if owners.is_empty()
                    && !glue
                    && let Some(node) = &node
                {
                    owners = self.inner(side, node);
                }
                // A jump with no value and no owner (a `goto`, a `return`) has no value of the
                // source: its target follows a change at a different place, and that change has
                // its own elements (fix round 2).
                if owners.is_empty() && element.jump {
                    out.glue += 1;
                    continue;
                }
                // Rule 2: the tokens of the element are at the place of an edit (a `macro_rules`
                // body, a field of a type that a derive reads). Since design 44, each edit counts:
                // the element comes from the text of the edit.
                let mut at_edit = first.is_some();
                if owners.is_empty()
                    && let Some(place) = definition
                    && let Some(found) = self.node(side, place)
                {
                    owners.extend(self.owner(side, &found));
                    at_edit |= !owners.is_empty();
                }
                // An element in a statement that a move passes over (design 41.7).
                if owners.is_empty()
                    && let Some(node) = &node
                {
                    owners.extend(self.jump_owner(side, node));
                    out.by_region |= !owners.is_empty();
                }
                // A glue element (a `goto`, a `return`, a `drop` at the end of a scope) with no
                // owner, whose node is a block or the body, follows a change at a different place
                // (a new exit joins the end of the body, a new local gets its drop): that change
                // has its own elements.
                if owners.is_empty() && element.glue && node.as_ref().is_none_or(|node| !node.block_free) {
                    out.glue += 1;
                    continue;
                }
                out.positional.extend(owners.iter().copied());
                checked.push((side, element.clone(), node, owners, at_edit));
            }
        }
        // Pass 2 (design 44): the links. A link adds a cause to a changed element that the two
        // sides have at one place: an element with no local owner, or with an edit inside its
        // node (rule 1b), or in the region of a move. New code or deleted code at the place of an
        // edit (rules 1 and 2) gets its cause from that edit only. An element with a local owner
        // keeps only the symbols that a changed element of the other side names too: a symbol of
        // the new code inside it (`s.v` in `s.v + 1`) is no cause of the element.
        let named: [BTreeSet<String>; 2] = [0, 1].map(|side| checked.iter().filter(|(found, ..)| *found == side).flat_map(|(_, element, ..)| element.symbols.iter().map(|symbol| symbol.path.clone())).collect());
        // The links of each side by node text, for the counterpart rule.
        let mut linked_at: [BTreeMap<String, BTreeMap<usize, BTreeSet<String>>>; 2] = [BTreeMap::new(), BTreeMap::new()];
        let mut deferred: Vec<(usize, MirElement, Option<Mapped>)> = Vec::new();
        for (side, element, node, mut owners, at_edit) in checked {
            if let Some(links) = self.links.filter(|_| !at_edit) {
                let mut kept = element.clone();
                if !owners.is_empty() {
                    kept.symbols.retain(|symbol| named[1 - side].contains(&symbol.path));
                }
                let found = links.element(side, &kept, diff.evaluated);
                if !found.is_empty() {
                    out.linked += 1;
                    for (edit, symbols) in found {
                        if let Some(node) = &node {
                            linked_at[side].entry(bare(&node.text).to_string()).or_default().entry(edit).or_default().extend(symbols.iter().cloned());
                        }
                        if !owners.contains(&edit) {
                            owners.push(edit);
                        }
                        out.through.entry(edit).or_default().extend(symbols);
                    }
                }
            }
            if owners.is_empty() {
                deferred.push((side, element, node));
                continue;
            }
            for edit in owners {
                out.owns.entry(edit).or_default()[side].push(element.clone());
            }
        }
        // A changed part that is not an element (`other`), or a body that differs with no element
        // that differs (the order of its blocks): the function of the body names the types of
        // the signature line (design 44).
        if (diff.other || (diff.old.is_empty() && diff.new.is_empty()))
            && let Some(links) = self.links
        {
            for (side, own) in &diff.own {
                for edit in links.edits_of(*side, own) {
                    out.other_links.entry(edit).or_default().insert(own.path.clone());
                }
            }
        }
        // The counterpart (design 44): a changed element with no cause whose counterpart on the
        // other side (the same node text) has a link takes that link (a new drop adds an unwind
        // edge to the element before it; the old element names no changed symbol).
        for (side, element, node) in deferred {
            // A receiver (an auto-ref, a temporary, a drop) takes the links of its method call.
            let counterpart = node.as_ref().and_then(|node| linked_at[1 - side].get(bare(&node.text)).or_else(|| node.call.as_ref().and_then(|call| linked_at[1 - side].get(bare(call)).or_else(|| linked_at[side].get(bare(call))))));
            match (counterpart, node) {
                (Some(edits), _) => {
                    out.linked += 1;
                    for (edit, symbols) in edits {
                        out.owns.entry(*edit).or_default()[side].push(element.clone());
                        out.through.entry(*edit).or_default().extend(symbols.iter().cloned());
                    }
                }
                (None, Some(node)) => {
                    out.unowned.insert((side, node));
                }
                (None, None) => out.lost += 1,
            }
        }
        out
    }

}

impl MirMap<'_> {
    /// The file and the positions of a source range on one side.
    fn positions(&mut self, side: usize, place: &MirPlace) -> Option<(String, Pos, Pos)> {
        let ws = self.sides[side];
        let tree = self.trees[side].entry(place.0.clone()).or_insert_with(|| {
            let text = ws.rust_files.get(&place.0)?;
            let parsed = rs::ast::SourceFile::parse(text, ws.editions.get(&place.0).copied().unwrap_or(rs::Edition::Edition2021)).tree();
            Some(FileTree { lines: rs::Lines::new(text), root: parsed.syntax().clone() })
        });
        let tree = tree.as_ref()?;
        let length = tree.lines.text().len();
        (place.2 <= length && place.1 <= place.2).then(|| (place.0.clone(), tree.lines.pos(place.1), tree.lines.pos(place.2)))
    }

    /// The MIR check of each statement pair of the mapper in a body (design 41.8): the aligned
    /// elements of the old statement must go to the new statement, and the reverse. A pair
    /// with an aligned element in a different node does not agree. A pair with no aligned
    /// element (a moved statement, a changed statement) has no proof either way.
    pub fn check_maps(&mut self, body: &str, maps: &semilla_syntax::BodyMaps, diff: &MirDiff) {
        let mine: Vec<&semilla_syntax::StatementMap> = maps.maps.iter().filter(|map| map.after.item == body).collect();
        if mine.is_empty() {
            return;
        }
        let pairs: Vec<((String, Pos, Pos), (String, Pos, Pos))> = diff.pairs.iter().filter_map(|(a, b)| Some((self.positions(0, a)?, self.positions(1, b)?))).collect();
        let differ: [Vec<(String, Pos, Pos)>; 2] = [
            diff.old.iter().filter_map(|element| element.tokens.as_ref()).filter_map(|place| self.positions(0, place)).collect(),
            diff.new.iter().filter_map(|element| element.tokens.as_ref()).filter_map(|place| self.positions(1, place)).collect(),
        ];
        let inside = |(file, start, end): &(String, Pos, Pos), range: &(String, Pos, Pos)| *file == range.0 && range.1 <= *start && *end <= range.2;
        for map in mine {
            let range = |side: usize, anchor: &semilla_core::Anchor| -> Option<(String, Pos, Pos)> {
                let ws = self.sides[side];
                let node = semilla_syntax::resolve_node(ws, anchor)?;
                Some((ws.items.get(&anchor.item)?.file.clone(), node.start, node.end))
            };
            let (Some(old), Some(new)) = (range(0, &map.before), range(1, &map.after)) else { continue };
            let old_in: Vec<&((String, Pos, Pos), (String, Pos, Pos))> = pairs.iter().filter(|(a, _)| inside(a, &old)).collect();
            let new_in: Vec<&((String, Pos, Pos), (String, Pos, Pos))> = pairs.iter().filter(|(_, b)| inside(b, &new)).collect();
            let any_old = !old_in.is_empty() || differ[0].iter().any(|place| inside(place, &old));
            let any_new = !new_in.is_empty() || differ[1].iter().any(|place| inside(place, &new));
            if !any_old && !any_new {
                continue;
            }
            self.counts.checked += 1;
            let elsewhere = old_in.iter().any(|(_, b)| !inside(b, &new)) || new_in.iter().any(|(a, _)| !inside(a, &old));
            if elsewhere {
                self.counts.disagree += 1;
                if self.counts.disagree_examples.len() < EXAMPLES {
                    self.counts.disagree_examples.push(format!("`{}` → `{}`{}", map.before, map.after, if map.moved { " (moved)" } else { "" }));
                }
            } else if old_in.is_empty() && new_in.is_empty() {
                self.counts.unaligned += 1;
            }
        }
    }
}

/// The places of the attributes of an item on one side: each line of `text` that the source of
/// the file has in the range of the item.
fn attribute_places(ws: &Workspace, item: &semilla_syntax::Item, text: &str) -> Vec<(Pos, Pos)> {
    let Some(source) = ws.rust_files.get(&item.file) else { return Vec::new() };
    let lines = rs::Lines::new(source);
    let (from, to) = ((item.line_start, item.col_start), (item.line_end, item.col_end));
    let mut out = Vec::new();
    for attribute in text.lines().map(str::trim).filter(|line| line.starts_with("#[")) {
        let found = source.match_indices(attribute).map(|(at, _)| (lines.pos(at), lines.pos(at + attribute.len()))).find(|(start, end)| from <= *start && *end <= to);
        out.extend(found);
    }
    out
}

/// A node text with no outer parentheses: `(x as u32)` and `x as u32` are one place.
fn bare(text: &str) -> &str {
    let mut text = text.trim();
    while text.starts_with('(') && text.ends_with(')') && text.len() > 1 {
        text = text[1..text.len() - 1].trim();
    }
    text
}

/// The change of the branch structure of the elements of an edit (design 40.4): the text of
/// the mark, or `None` if the structure is the same.
pub fn branch_mark(elements: &[Vec<MirElement>; 2]) -> Option<String> {
    let count = |list: &[MirElement]| -> (usize, usize, usize) {
        let switches = list.iter().filter(|element| element.targets.is_some()).count();
        let targets = list.iter().filter_map(|element| element.targets).sum();
        let exits = list.iter().filter(|element| element.exit).count();
        (switches, targets, exits)
    };
    let (old, new) = (count(&elements[0]), count(&elements[1]));
    if old == new {
        return None;
    }
    let mut parts = Vec::new();
    if (old.0, old.1) != (new.0, new.1) {
        parts.push(format!("{} → {} branch point(s), {} → {} targets", old.0, new.0, old.1, new.1));
    }
    if old.2 != new.2 {
        parts.push(format!("{} → {} exit(s)", old.2, new.2));
    }
    Some(format!("the compiled code of this edit changes its branches ({})", parts.join("; ")))
}
