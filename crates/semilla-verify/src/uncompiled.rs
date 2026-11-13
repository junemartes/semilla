//! An edit in code that no checked configuration compiles (batch 5w, part A).
//!
//! The cause is trial 7 on `humantime`: a constant in a module with a `cfg` attribute that is
//! false in each checked configuration got the class `neutral (proved)`. "No body changed" is
//! evidence only for code that a checked configuration compiles.
//!
//! The rule. The pass compares the tokens of each Rust file on the two sides. A changed token
//! is dead on its side if no checked unit of that side compiles it:
//!
//! 1. No checked unit reads the file (`#[cfg(windows)] mod win;`, a `path` attribute under
//!    `cfg_attr`), or
//! 2. The token is in the range of a `cfg` attribute, and no unit that reads the file makes
//!    each condition around the token true.
//!
//! The fact of a dead token is never neutral. It gets the spotlight "build configuration",
//! and it needs a reason. A comment is not a token, so a comment edit is free. A doc comment
//! keeps its own rule: the pass skips its tokens.
//!
//! Soundness. The pass only adds a spotlight, and it only removes the class `neutral`. A
//! token in the input of a macro call has no syntax tree. If the input has a `cfg` condition,
//! the token is dead, unless the input has one condition and the checked units make it true
//! and false (then each part of the input is in a checked unit).

use std::collections::BTreeSet;

use proc_macro2::{Delimiter, LineColumn, Spacing, TokenStream, TokenTree};
use semilla_syntax::Workspace;
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::cfgcond::{self, Pred};
use crate::checks::BodyIndex;
use crate::ConfigTruth;

/// The start of the text of the spotlight.
pub(crate) const START: &str = "build configuration: ";

/// The clause for code that no checked configuration compiles.
const NOT_COMPILED: &str = "no checked configuration compiles the changed code";

/// The clause for the input of a macro call with a `cfg` condition.
const UNSEEN: &str = "Semilla cannot see if a checked configuration compiles the changed code";

/// A position in a file: the line (from 1) and the column (from 0).
type At = (usize, usize);

fn at(position: LineColumn) -> At {
    (position.line, position.column)
}

/// One token of a file, with its start.
struct Token {
    text: String,
    at: At,
}

fn is_punct(tree: Option<&TokenTree>, c: char) -> bool {
    matches!(tree, Some(TokenTree::Punct(p)) if p.as_char() == c)
}

/// The flat token list of a token stream. A group gives its two delimiters. The tokens of a
/// doc comment (`#[doc = "…"]`) are not in the list.
fn flatten(stream: TokenStream, out: &mut Vec<Token>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let mut index = 0;
    while index < trees.len() {
        if is_punct(trees.get(index), '#') {
            let group = if is_punct(trees.get(index + 1), '!') { index + 2 } else { index + 1 };
            if let Some(TokenTree::Group(found)) = trees.get(group) {
                let content: Vec<TokenTree> = found.stream().into_iter().take(2).collect();
                let doc = found.delimiter() == Delimiter::Bracket && matches!(content.first(), Some(TokenTree::Ident(name)) if name == "doc") && is_punct(content.get(1), '=');
                if doc {
                    index = group + 1;
                    continue;
                }
            }
        }
        match &trees[index] {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                if !open.is_empty() {
                    out.push(Token { text: open.into(), at: at(group.span_open().start()) });
                }
                flatten(group.stream(), out);
                if !close.is_empty() {
                    out.push(Token { text: close.into(), at: at(group.span_close().start()) });
                }
            }
            // The spacing is a part of the token: a macro can see it.
            TokenTree::Punct(punct) => out.push(Token { text: format!("{}{}", punct.as_char(), if punct.spacing() == Spacing::Joint { "~" } else { "" }), at: at(punct.span().start()) }),
            tree => out.push(Token { text: tree.to_string(), at: at(tree.span().start()) }),
        }
        index += 1;
    }
}

/// The tokens of a file text. `None`: the text is not a token list.
fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    flatten(text.parse::<TokenStream>().ok()?, &mut out);
    Some(out)
}

/// The token diff of two files: for each token, true if the edit changed it. The diff has two
/// levels (the lines, then the tokens of each changed line group), so that two edits with a
/// long distance do not mark the tokens between them.
fn diff(old: &[Token], new: &[Token]) -> (Vec<bool>, Vec<bool>) {
    // The token ranges of the lines that have a token.
    let lines = |list: &[Token]| -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (index, token) in list.iter().enumerate() {
            match out.last_mut() {
                Some(last) if list[last.0].at.0 == token.at.0 => last.1 = index + 1,
                _ => out.push((index, index + 1)),
            }
        }
        out
    };
    let key = |list: &[Token], range: &(usize, usize)| -> String { list[range.0..range.1].iter().map(|token| token.text.as_str()).collect::<Vec<_>>().join("\u{1}") };
    let (old_lines, new_lines) = (lines(old), lines(new));
    let old_keys: Vec<String> = old_lines.iter().map(|range| key(old, range)).collect();
    let new_keys: Vec<String> = new_lines.iter().map(|range| key(new, range)).collect();
    let (old_moved, new_moved) = crate::opaque::token_diff(&old_keys, &new_keys, |a, b| a == b);
    let mut old_changed = vec![false; old.len()];
    let mut new_changed = vec![false; new.len()];
    let (mut row, mut column) = (0, 0);
    while row < old_lines.len() || column < new_lines.len() {
        if row < old_lines.len() && column < new_lines.len() && !old_moved[row] && !new_moved[column] {
            row += 1;
            column += 1;
            continue;
        }
        let (first_row, first_column) = (row, column);
        while row < old_lines.len() && old_moved[row] {
            row += 1;
        }
        while column < new_lines.len() && new_moved[column] {
            column += 1;
        }
        if first_row == row && first_column == column {
            // The two lists do not agree (this cannot occur). Mark the rest.
            old_changed.iter_mut().skip(old_lines.get(row).map_or(old.len(), |range| range.0)).for_each(|flag| *flag = true);
            new_changed.iter_mut().skip(new_lines.get(column).map_or(new.len(), |range| range.0)).for_each(|flag| *flag = true);
            break;
        }
        // The tokens of the line group.
        let span = |lines: &[(usize, usize)], first: usize, end: usize, len: usize| -> (usize, usize) {
            if first == end {
                let at = lines.get(first).map_or(len, |range| range.0);
                (at, at)
            } else {
                (lines[first].0, lines[end - 1].1)
            }
        };
        let (old_span, new_span) = (span(&old_lines, first_row, row, old.len()), span(&new_lines, first_column, column, new.len()));
        let (a, b) = crate::opaque::token_diff(&old[old_span.0..old_span.1], &new[new_span.0..new_span.1], |a, b| a.text == b.text);
        for (offset, flag) in a.into_iter().enumerate() {
            old_changed[old_span.0 + offset] = flag;
        }
        for (offset, flag) in b.into_iter().enumerate() {
            new_changed[new_span.0 + offset] = flag;
        }
    }
    (old_changed, new_changed)
}

/// The range of one `cfg` attribute: the code that exists only if the condition is true.
struct Region {
    start: At,
    end: At,
    /// The range of the attribute itself. The parent compiles these tokens.
    attr: (At, At),
    /// The conditions. An attribute has one. The input of a macro call can have more.
    preds: Vec<Pred>,
    /// True for the input of a macro call: Semilla cannot see which code a condition selects.
    macro_input: bool,
    /// The text of the attribute (or the name of the macro) for a message.
    text: String,
}

impl Region {
    /// True if the unit compiles the code of the range. For the input of a macro call, the
    /// answer is the same for each unit: true only if the input has one condition, and the
    /// units that read the file make it true and false. Then each part of the input is in a
    /// checked unit.
    fn compiled(&self, unit: &crate::Unit, units: &[&crate::Unit]) -> bool {
        match (self.macro_input, self.preds.as_slice()) {
            (false, [pred]) => cfgcond::eval(pred, &unit.cfg) == Some(true),
            (true, [pred]) => {
                let values: Vec<Option<bool>> = units.iter().map(|unit| cfgcond::eval(pred, &unit.cfg)).collect();
                values.contains(&Some(true)) && values.contains(&Some(false)) && !values.contains(&None)
            }
            _ => false,
        }
    }
}

/// Finds each `cfg` range of a file.
struct Finder {
    /// The ranges of the nodes around the current node that can have an attribute.
    stack: Vec<(At, At)>,
    out: Vec<Region>,
}

impl Finder {
    fn within<T: Spanned>(&mut self, node: &T, visit: impl FnOnce(&mut Self)) {
        let span = node.span();
        self.stack.push((at(span.start()), at(span.end())));
        visit(self);
        self.stack.pop();
    }
}

/// The condition and the text of an attribute that removes its node: `cfg(P)`, or
/// `cfg_attr(P, cfg(Q))` (the node exists if `P` is false or `Q` is true).
fn condition(attr: &syn::Attribute) -> Option<(Pred, String)> {
    let syn::Meta::List(list) = &attr.meta else { return None };
    let text = |name: &str| format!("#[{name}({})]", cfgcond::show(&crate::srcconfig::compact(&list.tokens.to_string())));
    if list.path.is_ident("cfg") {
        let trees: Vec<TokenTree> = list.tokens.clone().into_iter().collect();
        return Some((cfgcond::parse(&trees), text("cfg")));
    }
    if !list.path.is_ident("cfg_attr") {
        return None;
    }
    let mut parts = cfgcond::parts(list.tokens.clone()).into_iter();
    let first = cfgcond::parse(&parts.next()?);
    let mut found: Option<Pred> = None;
    for part in parts {
        let inner = match (part.first(), part.get(1), part.len()) {
            (Some(TokenTree::Ident(name)), Some(TokenTree::Group(group)), 2) if name == "cfg" && group.delimiter() == Delimiter::Parenthesis => cfgcond::parse(&group.stream().into_iter().collect::<Vec<_>>()),
            // A `cfg_attr` in a `cfg_attr` can have a `cfg`: Semilla does not evaluate it.
            (Some(TokenTree::Ident(name)), _, _) if name == "cfg_attr" => Pred::Other,
            _ => continue,
        };
        found = Some(match found {
            Some(old) => Pred::All(vec![old, inner]),
            None => inner,
        });
    }
    Some((Pred::Any(vec![Pred::Not(Box::new(first)), found?]), text("cfg_attr")))
}

impl<'ast> Visit<'ast> for Finder {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        let Some(&(start, end)) = self.stack.last() else { return };
        if let Some((pred, text)) = condition(attr) {
            let span = attr.span();
            self.out.push(Region { start, end, attr: (at(span.start()), at(span.end())), preds: vec![pred], macro_input: false, text });
        }
    }
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // The syntax tree does not go into the input of a macro call. A `cfg` condition in
        // the input can remove a part of it.
        let mut preds: Vec<Pred> = Vec::new();
        for condition in cfgcond::conditions(node.tokens.clone()) {
            let pred = cfgcond::parse(&condition.into_iter().collect::<Vec<_>>());
            if !preds.contains(&pred) {
                preds.push(pred);
            }
        }
        if !preds.is_empty() {
            let span = node.span();
            let name = node.path.segments.last().map(|segment| segment.ident.to_string()).unwrap_or_default();
            self.out.push(Region { start: at(span.start()), end: at(span.end()), attr: ((0, 0), (0, 0)), preds, macro_input: true, text: name });
        }
        syn::visit::visit_macro(self, node);
    }
    fn visit_file(&mut self, node: &'ast syn::File) {
        // An inner attribute of the file applies to the full file.
        self.stack.push(((0, 0), (usize::MAX, usize::MAX)));
        syn::visit::visit_file(self, node);
        self.stack.pop();
    }
    fn visit_item(&mut self, node: &'ast syn::Item) {
        self.within(node, |this| syn::visit::visit_item(this, node));
    }
    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        self.within(node, |this| syn::visit::visit_impl_item(this, node));
    }
    fn visit_trait_item(&mut self, node: &'ast syn::TraitItem) {
        self.within(node, |this| syn::visit::visit_trait_item(this, node));
    }
    fn visit_foreign_item(&mut self, node: &'ast syn::ForeignItem) {
        self.within(node, |this| syn::visit::visit_foreign_item(this, node));
    }
    fn visit_field(&mut self, node: &'ast syn::Field) {
        self.within(node, |this| syn::visit::visit_field(this, node));
    }
    fn visit_variant(&mut self, node: &'ast syn::Variant) {
        self.within(node, |this| syn::visit::visit_variant(this, node));
    }
    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        self.within(node, |this| syn::visit::visit_arm(this, node));
    }
    fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
        self.within(node, |this| syn::visit::visit_stmt(this, node));
    }
    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        self.within(node, |this| syn::visit::visit_expr(this, node));
    }
    fn visit_field_value(&mut self, node: &'ast syn::FieldValue) {
        self.within(node, |this| syn::visit::visit_field_value(this, node));
    }
    fn visit_field_pat(&mut self, node: &'ast syn::FieldPat) {
        self.within(node, |this| syn::visit::visit_field_pat(this, node));
    }
    fn visit_fn_arg(&mut self, node: &'ast syn::FnArg) {
        self.within(node, |this| syn::visit::visit_fn_arg(this, node));
    }
    fn visit_bare_fn_arg(&mut self, node: &'ast syn::BareFnArg) {
        self.within(node, |this| syn::visit::visit_bare_fn_arg(this, node));
    }
    fn visit_generic_param(&mut self, node: &'ast syn::GenericParam) {
        self.within(node, |this| syn::visit::visit_generic_param(this, node));
    }
}

/// The `cfg` ranges of a file text. A text that is not a Rust file has no range.
fn regions(text: &str) -> Vec<Region> {
    let Ok(file) = syn::parse_file(text) else { return Vec::new() };
    let mut finder = Finder { stack: Vec::new(), out: Vec::new() };
    finder.visit_file(&file);
    finder.out
}

/// One changed line that no checked configuration compiles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Dead {
    pub file: String,
    /// 0: the tree before the step. 1: the tree after the step.
    pub side: usize,
    pub line: usize,
    /// The cause for a message: a clause with the attribute and its place, or with the file
    /// that no unit reads.
    pub cause: String,
}

/// The text for a file that no checked unit reads: the `cfg` attributes of the modules of the
/// file, if the syntax has them.
fn unread(workspace: &Workspace, file: &str) -> String {
    let mut modules: Vec<&semilla_syntax::Item> = workspace.items.values().filter(|item| item.kind == semilla_syntax::ItemKind::Mod && item.file != file).collect();
    modules.retain(|module| workspace.items.values().any(|item| item.file == file && crate::checks::is_child(&item.path, &module.path)));
    let attrs: Vec<String> = modules
        .iter()
        .flat_map(|module| module.attrs.iter().filter(|attr| attr.contains("cfg")).map(|attr| format!("`{}` at `{}`", cfgcond::show(&crate::srcconfig::compact(attr)), module.path)))
        .collect();
    match attrs.is_empty() {
        true => format!("{NOT_COMPILED} (no checked unit reads the file `{file}`)"),
        false => format!("{NOT_COMPILED} (no checked unit reads the file `{file}`: {})", attrs.join(", ")),
    }
}

/// Each changed line of the step that no checked configuration compiles.
///
/// `truths` has the units of the checked configurations of the two sides. An empty list
/// gives no result: the oracle has no data. A configuration with no unit record gives the
/// careful rule: a token in a `cfg` range is dead if its item has no body record.
pub(crate) fn dead_edits(sides: [&Workspace; 2], truths: &[Vec<ConfigTruth>; 2], bodies: &BodyIndex) -> Vec<Dead> {
    let mut out: BTreeSet<Dead> = BTreeSet::new();
    if truths.iter().all(|side| side.is_empty()) {
        return Vec::new();
    }
    let files: BTreeSet<&String> = sides.iter().flat_map(|side| side.parsed_files.iter()).collect();
    for file in files {
        let texts = [sides[0].rust_files.get(file), sides[1].rust_files.get(file)];
        if texts[0] == texts[1] {
            continue;
        }
        let lists = [texts[0].map(|text| tokens(text)), texts[1].map(|text| tokens(text))];
        // A side with no file has no token. A text that is not a token list has one token.
        let fallback = |index: usize| -> Vec<Token> {
            match &lists[index] {
                Some(None) => vec![Token { text: texts[index].cloned().unwrap_or_default(), at: (1, 0) }],
                _ => Vec::new(),
            }
        };
        let (own_old, own_new) = (fallback(0), fallback(1));
        let old = lists[0].as_ref().and_then(|list| list.as_ref()).unwrap_or(&own_old);
        let new = lists[1].as_ref().and_then(|list| list.as_ref()).unwrap_or(&own_new);
        let (old_changed, new_changed) = diff(old, new);
        for (side, (list, changed)) in [(old, old_changed), (new, new_changed)].into_iter().enumerate() {
            if !changed.iter().any(|flag| *flag) {
                continue;
            }
            let known = !truths[side].is_empty() && truths[side].iter().all(|truth| truth.known);
            let units: Vec<&crate::Unit> = truths[side].iter().flat_map(|truth| truth.units.iter()).filter(|unit| unit.files.contains(file.as_str())).collect();
            if known && units.is_empty() {
                let cause = unread(sides[side], file);
                for (token, _) in list.iter().zip(&changed).filter(|(_, flag)| **flag) {
                    out.insert(Dead { file: file.to_string(), side, line: token.at.0, cause: cause.clone() });
                }
                continue;
            }
            let regions = texts[side].map(|text| regions(text)).unwrap_or_default();
            if regions.is_empty() {
                continue;
            }
            for (token, _) in list.iter().zip(&changed).filter(|(_, flag)| **flag) {
                let inside = |range: (At, At)| range.0 <= token.at && token.at < range.1;
                let around: Vec<&Region> = regions.iter().filter(|region| inside((region.start, region.end)) && !inside(region.attr)).collect();
                if around.is_empty() {
                    continue;
                }
                let live = match known {
                    true => units.iter().any(|unit| around.iter().all(|region| region.compiled(unit, &units))),
                    false => sides[side].item_at(file, token.at.0).is_some_and(|item| bodies.has_body(&item.path)),
                };
                if live {
                    continue;
                }
                // The conditions that no unit makes true. If each one is true in a unit, the
                // cause is the set of the conditions.
                let mut blocked: Vec<&&Region> = around.iter().filter(|region| !known || !units.iter().any(|unit| region.compiled(unit, &units))).collect();
                if blocked.is_empty() {
                    blocked = around.iter().collect();
                }
                let mut attrs: Vec<String> = Vec::new();
                let mut macros: Vec<String> = Vec::new();
                for region in blocked {
                    let (list, text) = match region.macro_input {
                        true => (&mut macros, format!("the input of `{}!` at {file}:{} has a `cfg` condition", region.text, region.start.0)),
                        false => (&mut attrs, format!("`{}` at {file}:{}", region.text, region.attr.0.0)),
                    };
                    if !list.contains(&text) {
                        list.push(text);
                    }
                }
                let cause = match attrs.is_empty() {
                    false => format!("{NOT_COMPILED} ({})", attrs.join(", ")),
                    true => format!("{UNSEEN} ({})", macros.join(", ")),
                };
                out.insert(Dead { file: file.to_string(), side, line: token.at.0, cause });
            }
        }
    }
    out.into_iter().collect()
}

/// The text of the spotlight for a list of causes.
pub(crate) fn spotlight_text(causes: &[String]) -> String {
    format!("{START}{}, so the edit has no proof", causes.join("; "))
}

/// The file and the line range of an anchor in a tree.
fn span_of(workspace: &Workspace, anchor: &semilla_core::Anchor) -> Option<(String, usize, usize)> {
    match semilla_syntax::resolve(workspace, anchor) {
        Some(semilla_syntax::Resolved::Node(item, node)) => Some((item.file.clone(), node.start.0, node.end.0)),
        Some(semilla_syntax::Resolved::Item(item)) => Some((item.file.clone(), item.line_start, item.line_end)),
        None => {
            // A member (`k::Pair.a`) or a part of a signature: the range of the item.
            let item = workspace.items.get(&anchor.item).or_else(|| workspace.items.get(anchor.item.rsplit_once('.')?.0))?;
            Some((item.file.clone(), item.line_start, item.line_end))
        }
    }
}

/// Puts each dead edit in the spotlight (the rule of this module).
///
/// 1. An identity claim on an item with a dead edit has no proof.
/// 2. The fact with the smallest edit range around a dead line gets the spotlight, and it is
///    not neutral.
/// 3. A dead line with no fact gets a new fact.
pub(crate) fn apply(dead: &[Dead], sides: [&Workspace; 2], step: &semilla_core::Step, edits: &[semilla_syntax::Edit], fact_of: &[Option<usize>], renames: &semilla_syntax::Renames, report: &mut semilla_core::StepReport) {
    use semilla_core::{Anchor, Class, ClaimStatus};
    if dead.is_empty() {
        return;
    }
    let hit = |side: usize, span: &(String, usize, usize)| dead.iter().find(|entry| entry.side == side && entry.file == span.0 && span.1 <= entry.line && entry.line <= span.2);
    // 1. The identity claims.
    for claim in step.claims.iter().filter(|claim| matches!(claim.op.as_str(), "rename" | "move-item")) {
        let Some(at) = &claim.at else { continue };
        let new = renames.items.get(&at.item).cloned().unwrap_or_else(|| at.item.clone());
        let found = span_of(sides[0], &Anchor::item(&at.item)).and_then(|span| hit(0, &span)).or_else(|| span_of(sides[1], &Anchor::item(&new)).and_then(|span| hit(1, &span)));
        let Some(found) = found else { continue };
        for result in report.claims.iter_mut().filter(|result| result.claim == claim.id && result.status.is_verified()) {
            result.status = ClaimStatus::Unproved { reason: format!("{}, so the identity of `{}` has no proof", found.cause, at.item) };
            result.hint = Some("Remove the claim. Semilla derives the facts of the edit. They need a reason.".into());
        }
    }
    // 2. The ranges of each fact: (the fact, the side, the range).
    let mut spans: Vec<(usize, usize, (String, usize, usize))> = Vec::new();
    for (edit, fact) in edits.iter().zip(fact_of).filter_map(|(edit, fact)| fact.filter(|fact| *fact < report.facts.len()).map(|fact| (edit, fact))) {
        for (side, anchor) in [&edit.before, &edit.after].into_iter().enumerate() {
            match anchor {
                Some(anchor) => spans.extend(span_of(sides[side], anchor).map(|span| (fact, side, span))),
                None if edit.before.is_none() && edit.after.is_none() => spans.extend(edit.file.clone().map(|file| (fact, side, (file, edit.line, edit.line)))),
                None => {}
            }
        }
    }
    for (index, fact) in report.facts.iter().enumerate() {
        if spans.iter().any(|(owner, _, _)| *owner == index) {
            continue;
        }
        let Some(at) = &fact.at else { continue };
        for side in 0..2 {
            spans.extend(span_of(sides[side], at).map(|span| (index, side, span)));
        }
    }
    let mut causes: std::collections::BTreeMap<usize, Vec<String>> = std::collections::BTreeMap::new();
    // The dead lines with no fact, by the item (or the file) and the side.
    let mut free: std::collections::BTreeMap<(Option<String>, String), Vec<String>> = std::collections::BTreeMap::new();
    for entry in dead {
        let around: Vec<&(usize, usize, (String, usize, usize))> = spans.iter().filter(|(_, side, span)| *side == entry.side && span.0 == entry.file && span.1 <= entry.line && entry.line <= span.2).collect();
        let least = around.iter().map(|(_, _, span)| span.2 - span.1).min();
        let mut owners: Vec<usize> = around.iter().filter(|(_, _, span)| Some(span.2 - span.1) == least).map(|(fact, _, _)| *fact).collect();
        let item = sides[entry.side].item_at(&entry.file, entry.line).map(|item| item.path.clone());
        if owners.is_empty() {
            owners = report.facts.iter().enumerate().filter(|(_, fact)| item.is_some() && fact.at.as_ref().map(|at| &at.item) == item.as_ref()).map(|(index, _)| index).collect();
        }
        owners.sort();
        owners.dedup();
        for owner in &owners {
            let list = causes.entry(*owner).or_default();
            if !list.contains(&entry.cause) {
                list.push(entry.cause.clone());
            }
        }
        if owners.is_empty() {
            let list = free.entry((item, entry.file.clone())).or_default();
            if !list.contains(&entry.cause) {
                list.push(entry.cause.clone());
            }
        }
    }
    let reason_of = |fact: &semilla_core::Fact| match &fact.at {
        Some(at) => step.reasons.get(&at.to_string()).or_else(|| step.reasons.get(&at.item)).cloned(),
        None => step.reasons.get(&fact.part).cloned(),
    };
    for (index, list) in causes {
        let fact = &mut report.facts[index];
        let text = spotlight_text(&list);
        fact.spotlight_why = Some(match fact.spotlight_why.take() {
            // The old text of a fact in a function with no body record says less.
            Some(why) if why == crate::facts::INACTIVE => text,
            // The spotlight of a `cfg` attribute in a changed body names the condition.
            Some(why) if why.starts_with(crate::facts::CONFIG_SPOTLIGHT) => why,
            Some(why) => format!("{why}; {text}"),
            None => text,
        });
        fact.spotlight = true;
        if matches!(fact.class, Some(Class::Neutral) | Some(Class::Other)) {
            fact.class = Some(Class::Logic);
        }
        if fact.reason.is_none() {
            fact.reason = reason_of(fact);
        }
    }
    // 3. The new facts.
    for ((item, file), list) in free {
        let mut fact = semilla_core::Fact {
            id: format!("f{}", report.facts.len() + 1),
            at: item.as_ref().map(Anchor::item),
            file: Some(file.clone()),
            part: match &item {
                Some(_) => "inactive code".into(),
                None => file.clone(),
            },
            description: match &item {
                Some(item) => format!("an edit in `{item}`, in code that no checked configuration compiles"),
                None => "an edit in code that no checked configuration compiles".into(),
            },
            status: "derived".into(),
            class: Some(Class::Logic),
            spotlight: true,
            spotlight_why: Some(spotlight_text(&list)),
            ..Default::default()
        };
        fact.reason = reason_of(&fact);
        report.facts.push(fact);
    }
}
