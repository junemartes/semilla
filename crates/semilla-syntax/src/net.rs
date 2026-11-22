//! The net for a changed source text with no edit (batch 6a, A6).
//!
//! The diff of an item pair compares the parts that it knows: the visibility, the signature,
//! the attributes, the docs, the member list, and the body. A part that it does not know (for
//! example the visibility of an associated constant) gave no edit. The net compares the tokens
//! of the two items. Each changed token range must be in the text of an edit of the pair. If
//! one is not, the net adds an edit with the part `text`.
//!
//! The limits of the net:
//! - A comment, a doc text (`///`, `#[doc = "…"]`), and a comma are no tokens for the net.
//! - The body of a function is not in the net. The statement diff compares it.
//! - A token of a nested item (a method, a field, a variant) belongs to the nested item.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::tt::{Delimiter, TokenStream, TokenTree};

use semilla_core::{Anchor, View};

use crate::diff::{Edit, EditKind, Renames};
use crate::{Item, ItemKind, Workspace};

/// One token of a file, with the place of its start.
#[derive(Clone)]
struct Token {
    text: String,
    at: (usize, usize),
    /// The place after its end.
    end: (usize, usize),
}

/// The tokens of the files of one side. A file that is not Rust text has no tokens.
#[derive(Default)]
pub(crate) struct Files {
    tokens: BTreeMap<String, Rc<FileTokens>>,
    /// The span of each item of each file. `None` before the first use.
    spans: Option<BTreeMap<String, Vec<(Span, String)>>>,
}

/// The place of the start and the place after the end.
type Span = ((usize, usize), (usize, usize));

/// The tokens of one file. `sorted`: the tokens are in the order of their places, thus a
/// binary search finds the tokens of a span.
struct FileTokens {
    list: Vec<Token>,
    sorted: bool,
}

impl Files {
    /// The tokens of a file whose start is in `start..end`.
    fn tokens(&mut self, workspace: &Workspace, file: &str, start: (usize, usize), end: (usize, usize)) -> impl Iterator<Item = &Token> {
        let tokens = self.tokens.entry(file.to_string()).or_insert_with(|| file_tokens(workspace.rust_files.get(file).map(|text| text.as_str()).unwrap_or("")));
        let range = match tokens.sorted {
            true => tokens.list.partition_point(|token| token.at < start)..tokens.list.partition_point(|token| token.at < end),
            false => 0..tokens.list.len(),
        };
        tokens.list[range].iter().filter(move |token| token.at >= start && token.at < end)
    }

    /// The span and the path of each item of a file.
    fn spans(&mut self, workspace: &Workspace, file: &str) -> &[(Span, String)] {
        let spans = self.spans.get_or_insert_with(|| {
            let mut map: BTreeMap<String, Vec<(Span, String)>> = BTreeMap::new();
            for item in workspace.items.values() {
                map.entry(item.file.clone()).or_default().push((span(item), item.path.clone()));
            }
            map
        });
        spans.get(file).map(|list| list.as_slice()).unwrap_or(&[])
    }
}

thread_local! {
    /// The tokens of the last texts (batch 9b3, G6). The rename search does the diff of the same
    /// two trees one time for each map. The tokens are a function of the text only.
    static LAST_TOKENS: RefCell<(usize, HashMap<String, Rc<FileTokens>>)> = RefCell::new((0, HashMap::new()));
}

/// The limit of the kept texts, in bytes.
const KEPT_BYTES: usize = 8 << 20;

/// The tokens of a file text. A text that is not Rust text has no tokens.
fn file_tokens(text: &str) -> Rc<FileTokens> {
    if let Some(found) = LAST_TOKENS.with(|last| last.borrow().1.get(text).cloned()) {
        return found;
    }
    let mut list = Vec::new();
    if let Ok(stream) = TokenStream::with_places(text) {
        flatten(stream, &mut list);
    }
    let sorted = list.windows(2).all(|pair| pair[0].at <= pair[1].at);
    let tokens = Rc::new(FileTokens { list, sorted });
    LAST_TOKENS.with(|last| {
        let (bytes, kept) = &mut *last.borrow_mut();
        if *bytes + text.len() > KEPT_BYTES {
            kept.clear();
            *bytes = 0;
        }
        *bytes += text.len();
        kept.insert(text.to_string(), tokens.clone());
    });
    tokens
}

fn is_doc_text(tree: Option<&TokenTree>) -> bool {
    let Some(TokenTree::Group(group)) = tree else { return false };
    let inner: Vec<TokenTree> = group.stream().into_iter().collect();
    group.delimiter() == Delimiter::Bracket
        && matches!(inner.as_slice(), [TokenTree::Ident(name), TokenTree::Punct(equal), TokenTree::Literal(_)] if name == "doc" && equal.as_char() == '=')
}

fn flatten(stream: TokenStream, out: &mut Vec<Token>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let mut index = 0;
    while index < trees.len() {
        let tree = &trees[index];
        index += 1;
        match tree {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                let (start, after) = (group.span_open().start(), group.span_open().end());
                out.push(Token { text: open.to_string(), at: (start.line, start.column), end: (after.line, after.column) });
                flatten(group.stream(), out);
                let (end, after) = (group.span_close().start(), group.span_close().end());
                out.push(Token { text: close.to_string(), at: (end.line, end.column), end: (after.line, after.column) });
            }
            TokenTree::Punct(punct) => {
                if punct.as_char() == ',' {
                    continue;
                }
                // A doc text: `#[doc = "…"]` or `#![doc = "…"]`.
                if punct.as_char() == '#' {
                    let bang = matches!(trees.get(index), Some(TokenTree::Punct(next)) if next.as_char() == '!');
                    if is_doc_text(trees.get(index + usize::from(bang))) {
                        index += 1 + usize::from(bang);
                        continue;
                    }
                }
                let (start, after) = (punct.span().start(), punct.span().end());
                out.push(Token { text: punct.as_char().to_string(), at: (start.line, start.column), end: (after.line, after.column) });
            }
            other => {
                let (start, after) = (other.span().start(), other.span().end());
                out.push(Token { text: other.to_string(), at: (start.line, start.column), end: (after.line, after.column) });
            }
        }
    }
}

fn span(item: &Item) -> Span {
    ((item.line_start, item.col_start), (item.line_end, item.col_end))
}

/// The tokens of an item that belong to no nested item and to no function body.
fn own_tokens(files: &mut Files, workspace: &Workspace, item: &Item) -> Vec<Token> {
    let (start, end) = span(item);
    let nested: Vec<Span> = files.spans(workspace, &item.file).iter().filter(|(_, path)| *path != item.path).map(|(span, _)| *span).filter(|(from, to)| *from >= start && *to <= end && (*from, *to) != (start, end)).collect();
    let mut out: Vec<Token> = files.tokens(workspace, &item.file, start, end).filter(|token| !nested.iter().any(|(from, to)| token.at >= *from && token.at < *to)).cloned().collect();
    // The body of a function and the content of an inline module: the last brace group, which
    // ends at the end of the item.
    let has_block = (item.kind == ItemKind::Fn && item.body.is_some()) || item.kind == ItemKind::Mod;
    if has_block && out.last().is_some_and(|last| last.text == "}" && (last.at.0, last.at.1 + 1) == end) {
        let mut depth = 0usize;
        let mut open = None;
        for (index, token) in out.iter().enumerate().rev() {
            match token.text.as_str() {
                "}" => depth += 1,
                "{" => {
                    depth -= 1;
                    if depth == 0 {
                        open = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(open) = open {
            out.truncate(open);
        }
    }
    out
}

/// The text of the own tokens of an item, with the child mark (`▫`) at each place where the
/// item has text that the own tokens do not have: a nested item, a function body (design 37.1,
/// rule 3). Thus a side of the net fact is the full text of the entity or has a child mark.
fn shown(files: &mut Files, workspace: &Workspace, item: &Item, own: &[Token]) -> String {
    let (start, end) = span(item);
    let mut out: Vec<Token> = Vec::new();
    let mut next = 0;
    let mut cut = false;
    for token in files.tokens(workspace, &item.file, start, end) {
        if own.get(next).is_some_and(|own| own.at == token.at) {
            out.push(own[next].clone());
            next += 1;
            cut = false;
        } else if !cut {
            out.push(Token { text: crate::tree::CHILD_MARK.to_string(), at: token.at, end: token.at });
            cut = true;
        }
    }
    join(&out)
}

/// The display text of the tokens (design 39.2): one space where the source has space, a
/// comment, or a token that the net skips between two tokens.
fn join(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 && (tokens[index - 1].end != token.at || token.text == crate::tree::CHILD_MARK || tokens[index - 1].text == crate::tree::CHILD_MARK) {
            out.push(' ');
        }
        out.push_str(&token.text);
    }
    out
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && *c != ',').collect()
}

/// The associated constants and types of an impl: the kind, the name, the visibility, and the
/// attributes (no doc text). The member list of the impl has the other parts.
/// Each part has its compare text and its display text (design 39.2).
fn associated(item: &Item) -> Vec<(&'static str, String, (String, String), (String, String))> {
    use crate::rs::{AstNode, ast};
    use ast::{HasName, HasVisibility};
    let Some(ast::Item::Impl(parsed)) = crate::rs::parse_item(&item.source) else { return Vec::new() };
    let text = |node: &crate::rs::SyntaxNode, vis: Option<ast::Visibility>, default: bool| {
        let attrs: Vec<(String, String)> = crate::load::attributes(node).iter().filter(|attr| !attr.doc_text()).map(|attr| (attr.text.clone(), attr.display.clone())).collect();
        let default = if default { "default " } else { "" };
        let vis_display = vis.as_ref().map(|vis| crate::tt::display_text(vis.syntax())).unwrap_or_default();
        let vis = (format!("{default}{}", crate::rs::compare_opt(vis.as_ref())).trim().to_string(), format!("{default}{vis_display}").trim().to_string());
        let shown = |pick: fn(&(String, String)) -> &String| attrs.iter().map(pick).cloned().collect::<Vec<_>>().join("\n");
        (vis, (shown(|pair| &pair.0), shown(|pair| &pair.1)))
    };
    parsed
        .assoc_item_list()
        .into_iter()
        .flat_map(|list| list.assoc_items())
        .filter_map(|member| match &member {
            ast::AssocItem::Const(constant) => {
                let (vis, attrs) = text(constant.syntax(), constant.visibility(), constant.default_token().is_some());
                Some(("constant", constant.name().map(|name| name.text().to_string()).unwrap_or_else(|| "_".into()), vis, attrs))
            }
            ast::AssocItem::TypeAlias(ty) => {
                let (vis, attrs) = text(ty.syntax(), ty.visibility(), ty.default_token().is_some());
                Some(("type", ty.name().map(|name| name.text().to_string()).unwrap_or_default(), vis, attrs))
            }
            _ => None,
        })
        .collect()
}

/// The edits of the visibility and of the attributes of the associated constants and types of
/// an impl pair. The label of an edit is in `attr_after`.
fn associated_edits(before: &Item, after: &Item, renames: &Renames) -> Vec<Edit> {
    if before.kind != ItemKind::Impl || before.source == after.source {
        return Vec::new();
    }
    let (old, new) = (associated(before), associated(after));
    let mut out = Vec::new();
    for (kind, name, old_vis, old_attrs) in &old {
        let name_after = renames.apply(&before.path, name);
        type Part = (&'static str, String, (String, String), (String, String));
        let same = |list: &[Part], name: &str| list.iter().filter(|(k, n, _, _)| k == kind && n == name).count() == 1;
        let Some((_, _, new_vis, new_attrs)) = new.iter().find(|(k, n, _, _)| k == kind && *n == name_after).filter(|_| same(&old, name) && same(&new, &name_after)) else { continue };
        for (part, a, b) in [("visibility", old_vis, new_vis), ("attributes", old_attrs, new_attrs)] {
            if renames.apply(&before.path, &a.0) != b.0 {
                let mut edit = text_edit(before, after, a.0.clone(), b.0.clone(), after.line_start);
                (edit.source_before, edit.source_after) = (Some(a.1.clone()), Some(b.1.clone()));
                edit.attr_after = Some(format!("{part} of the associated {kind} `{name_after}`"));
                out.push(edit);
            }
        }
    }
    out
}

fn text_edit(before: &Item, after: &Item, old: String, new: String, line: usize) -> Edit {
    Edit {
        kind: EditKind::Signature,
        view: View::Signature,
        before: Some(Anchor::item(&before.path)),
        after: Some(Anchor::item(&after.path)),
        node_before: Some("text"),
        node_after: Some("text"),
        text_before: Some(old),
        text_after: Some(new),
        attr_before: None,
        attr_after: None,
        source_before: None,
        source_after: None,
        block_like: false,
        file: Some(after.file.clone()),
        line,
        note: None,
        refactor: None,
    }
}

/// The edits for the changed tokens of an item pair that no edit of the pair has in its text.
pub(crate) fn uncovered(files: &mut (Files, Files), before_ws: &Workspace, after_ws: &Workspace, before: &Item, after: &Item, renames: &Renames, pair_edits: &[Edit]) -> Vec<Edit> {
    let mut out = associated_edits(before, after, renames);
    let known: Vec<Edit> = pair_edits.iter().chain(&out).cloned().collect();
    out.extend(general(files, before_ws, after_ws, before, after, renames, &known));
    out
}

fn general(files: &mut (Files, Files), before_ws: &Workspace, after_ws: &Workspace, before: &Item, after: &Item, renames: &Renames, pair_edits: &[Edit]) -> Option<Edit> {
    if before.file == after.file && before_ws.rust_files.get(&before.file) == after_ws.rust_files.get(&after.file) {
        return None;
    }
    let mut old = own_tokens(&mut files.0, before_ws, before);
    let mut new = own_tokens(&mut files.1, after_ws, after);
    // The display text of each side needs the own tokens before a change. Most pairs have no
    // net fact, thus the display text comes only at the end.
    let mut popped: Option<(bool, Token)> = None;
    // A function with a body on one side only (design 37.1): the body edit of the pair has
    // the body. The `;` of the side with no body is the place of the body.
    if before.kind == ItemKind::Fn && before.body.is_some() != after.body.is_some() {
        let no_body = if before.body.is_some() { &mut new } else { &mut old };
        if no_body.last().is_some_and(|last| last.text == ";") {
            popped = no_body.pop().map(|token| (before.body.is_some(), token));
        }
    }
    if old.is_empty() || new.is_empty() {
        return None;
    }
    let texts = |tokens: &[Token]| tokens.iter().map(|token| token.text.clone()).collect::<Vec<_>>();
    let (old_texts, new_texts) = (texts(&old), texts(&new));
    if old_texts == new_texts {
        return None;
    }
    // The name of a renamed item is not in the rename map of the identifiers (a field, a
    // variant): the pair gives it.
    let own_name = |item: &Item| item.path.rsplit(['.', ':']).next().unwrap_or("").split('#').next().unwrap_or("").to_string();
    let (old_name, new_name) = (own_name(before), own_name(after));
    let renamed: Vec<String> = if renames.exact() {
        // The map of references (design section 51) needs the place of each token: it reads the
        // text of all tokens. A token that the map changes gets the new text.
        let mut starts = Vec::with_capacity(old.len());
        let mut text = String::new();
        for (index, token) in old.iter().enumerate() {
            if index > 0 && old[index - 1].end != token.at {
                text.push('\n');
            }
            starts.push(text.len());
            text.push_str(&token.text);
        }
        let mut out = old_texts.clone();
        for (start, _, new) in renames.spans(&before.path, &text) {
            if let Ok(index) = starts.binary_search(&start) {
                out[index] = new;
            }
        }
        // The declaration of the item: the first token with the old name.
        if let Some(index) = old_texts.iter().position(|text| *text == old_name).filter(|index| out[*index] == old_name) {
            out[index] = new_name.clone();
        }
        out
    } else {
        old_texts.iter().map(|text| if *text == old_name { new_name.clone() } else { renames.apply(&before.path, text) }).collect()
    };
    let with_name: Vec<Token> = old.iter().zip(&renamed).map(|(token, text)| Token { text: text.clone(), ..token.clone() }).collect();
    // A label with no `:` (`S { a }`) becomes `S { b : a }` in one token: the compare ignores
    // the white space.
    let squash = |text: String| text.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let same_text = if renames.exact() { squash(join(&with_name)) == squash(join(&new)) } else { renames.apply(&before.path, &join(&with_name)) == join(&new) };
    if renamed == new_texts || same_text {
        return None;
    }
    // The common start and the common end keep the table of the compare small.
    let head = renamed.iter().zip(&new_texts).take_while(|(a, b)| a == b).count();
    let tail = renamed[head..].iter().rev().zip(new_texts[head..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (old_mid, new_mid) = (&renamed[head..renamed.len() - tail], &new_texts[head..new_texts.len() - tail]);
    let same = crate::diff::lcs(old_mid, new_mid, |a, b| a == b);
    let runs = |length: usize, kept: Vec<usize>| -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut from = 0;
        for at in kept.into_iter().chain([length]) {
            if at > from {
                out.push((from, at));
            }
            from = at + 1;
        }
        out
    };
    // The text of a parameter order edit has only the names: the edit has the full signature.
    let order = pair_edits.iter().any(|edit| edit.node_before == Some("params"));
    let covered = |tokens: &[Token], (from, to): (usize, usize), item: &Item, text_of: &dyn Fn(&Edit) -> Option<&String>| {
        let run = compact(&join(&tokens[head + from..head + to]));
        // A plain comment is no token: only an attribute line of a comment edit is text of the
        // item. (`#[doc(hidden)]` is an attribute edit since batch 6b, part 1.)
        let text = |edit: &Edit| -> Option<String> {
            let text = text_of(edit)?;
            Some(if edit.kind == EditKind::Comment { text.lines().filter(|line| line.starts_with('#')).collect::<Vec<_>>().join("\n") } else { text.clone() })
        };
        pair_edits.iter().filter_map(text).chain(order.then(|| item.signature.clone())).any(|text| compact(&text).contains(&run))
    };
    let old_open = runs(old_mid.len(), same.iter().map(|(a, _)| *a).collect()).into_iter().any(|run| !covered(&old, run, before, &|edit| edit.text_before.as_ref()));
    let new_open = runs(new_mid.len(), same.iter().map(|(_, b)| *b).collect()).into_iter().any(|run| !covered(&new, run, after, &|edit| edit.text_after.as_ref()));
    if !old_open && !new_open {
        return None;
    }
    // The compare above uses one alignment of the tokens. With a token run that the item has two
    // times (a bound that moves), a different alignment of the same length is correct.
    if !order && renamed == old_texts && explained(&compact(&join(&old)), &compact(&join(&new)), pair_edits) {
        return None;
    }
    let full = |own: &[Token], is_new: bool| -> Vec<Token> { own.iter().cloned().chain(popped.iter().filter(|(side, _)| *side == is_new).map(|(_, token)| token.clone())).collect() };
    let (old_shown, new_shown) = (shown(&mut files.0, before_ws, before, &full(&old, false)), shown(&mut files.1, after_ws, after, &full(&new, true)));
    Some(text_edit(before, after, old_shown, new_shown, new.get(head).map(|token| token.at.0).unwrap_or(after.line_start)))
}

/// The answer to "do the texts of the edits make the new text from the old text?". Each edit
/// replaces one occurrence of its old text with one occurrence of its new text. The texts
/// have no white space and no commas. The search can skip an edit (a body edit has no text
/// here). A mark replaces each text, thus two edits cannot use the same tokens.
fn explained(old: &str, new: &str, edits: &[Edit]) -> bool {
    let texts: Vec<(String, String)> = edits
        .iter()
        .map(|edit| {
            let text = |text: &Option<String>| {
                let text = text.as_deref().unwrap_or("");
                compact(&if edit.kind == EditKind::Comment { text.lines().filter(|line| line.starts_with('#')).collect::<Vec<_>>().join("\n") } else { text.to_string() })
            };
            (text(&edit.text_before), text(&edit.text_after))
        })
        .filter(|(a, b)| a != b)
        .collect();
    if texts.len() > 6 || old.contains(['\u{0}', '\u{1}']) || new.contains(['\u{0}', '\u{1}']) {
        return false;
    }
    fn places(text: &str, part: &str) -> Vec<Option<usize>> {
        if part.is_empty() { vec![None] } else { text.match_indices(part).map(|(at, _)| Some(at)).collect() }
    }
    fn put(text: &str, at: Option<usize>, length: usize, mark: &str) -> String {
        match at {
            Some(at) => format!("{}{mark}{}", &text[..at], &text[at + length..]),
            None => text.to_string(),
        }
    }
    fn search(old: &str, new: &str, texts: &[(String, String)], index: usize, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let Some((a, b)) = texts.get(index) else {
            let plain = |text: &str| text.split('\u{0}').collect::<String>();
            return plain(old) == plain(new);
        };
        // An edit with one text only (an added or a removed part) leaves a mark with no name.
        let mark = if a.is_empty() || b.is_empty() { "\u{0}".to_string() } else { format!("\u{1}{index}\u{1}") };
        for at_old in places(old, a) {
            for at_new in places(new, b) {
                if search(&put(old, at_old, a.len(), &mark), &put(new, at_new, b.len(), &mark), texts, index + 1, budget) {
                    return true;
                }
            }
        }
        search(old, new, texts, index + 1, budget)
    }
    search(old, new, &texts, 0, &mut 4000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Batch 9b3, G6: an equal text gets the kept tokens. The negative test: a different text
    /// gets its own tokens.
    #[test]
    fn the_kept_tokens_are_the_tokens_of_their_text() {
        let texts = |tokens: &FileTokens| tokens.list.iter().map(|token| token.text.clone()).collect::<Vec<_>>();
        let first = file_tokens("fn a() -> u32 { 1 }");
        assert!(Rc::ptr_eq(&first, &file_tokens("fn a() -> u32 { 1 }")) && first.sorted);
        assert_eq!(texts(&first), ["fn", "a", "(", ")", "-", ">", "u32", "{", "1", "}"]);
        let second = file_tokens("fn a() -> u32 { 2 }");
        assert!(!Rc::ptr_eq(&first, &second) && texts(&second)[8] == "2");
        // A text that is not Rust text has no tokens.
        assert!(file_tokens("fn a( {").list.is_empty());
    }
}
