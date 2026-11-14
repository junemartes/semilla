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

use std::collections::BTreeMap;

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};

use semilla_core::{Anchor, View};

use crate::diff::{Edit, EditKind, Renames};
use crate::{Item, ItemKind, Workspace};

/// One token of a file, with the place of its start.
#[derive(Clone)]
struct Token {
    text: String,
    at: (usize, usize),
    /// True if the next token has no space before it (`::`, `'a`).
    joint: bool,
}

/// The tokens of the files of one side. A file that is not Rust text has no tokens.
#[derive(Default)]
pub(crate) struct Files(BTreeMap<String, Vec<Token>>);

impl Files {
    fn tokens(&mut self, workspace: &Workspace, file: &str) -> &[Token] {
        self.0.entry(file.to_string()).or_insert_with(|| {
            let mut out = Vec::new();
            if let Some(stream) = workspace.rust_files.get(file).and_then(|text| text.parse::<TokenStream>().ok()) {
                flatten(stream, &mut out);
            }
            out
        })
    }
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
                let start = group.span_open().start();
                out.push(Token { text: open.to_string(), at: (start.line, start.column), joint: false });
                flatten(group.stream(), out);
                let end = group.span_close().start();
                out.push(Token { text: close.to_string(), at: (end.line, end.column), joint: false });
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
                let start = punct.span().start();
                out.push(Token { text: punct.as_char().to_string(), at: (start.line, start.column), joint: punct.spacing() == Spacing::Joint });
            }
            other => {
                let start = other.span().start();
                out.push(Token { text: other.to_string(), at: (start.line, start.column), joint: false });
            }
        }
    }
}

fn span(item: &Item) -> ((usize, usize), (usize, usize)) {
    ((item.line_start, item.col_start), (item.line_end, item.col_end))
}

/// The tokens of an item that belong to no nested item and to no function body.
fn own_tokens(files: &mut Files, workspace: &Workspace, item: &Item) -> Vec<Token> {
    let (start, end) = span(item);
    let nested: Vec<((usize, usize), (usize, usize))> = workspace
        .items
        .values()
        .filter(|other| other.file == item.file && other.path != item.path)
        .map(span)
        .filter(|(from, to)| *from >= start && *to <= end && (*from, *to) != (start, end))
        .collect();
    let tokens = files.tokens(workspace, &item.file);
    let mut out: Vec<Token> = tokens.iter().filter(|token| token.at >= start && token.at < end && !nested.iter().any(|(from, to)| token.at >= *from && token.at < *to)).cloned().collect();
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

fn join(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        out.push_str(&token.text);
        let open = matches!(token.text.as_str(), "(" | "[");
        let close = tokens.get(index + 1).is_some_and(|next| matches!(next.text.as_str(), ")" | "]"));
        if !token.joint && !open && !close && index + 1 < tokens.len() {
            out.push(' ');
        }
    }
    out
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && *c != ',').collect()
}

/// The associated constants and types of an impl: the kind, the name, the visibility, and the
/// attributes (no doc text). The member list of the impl has the other parts.
fn associated(item: &Item) -> Vec<(&'static str, String, String, String)> {
    let Ok(parsed) = syn::parse_str::<syn::ItemImpl>(&item.source) else { return Vec::new() };
    let text = |vis: &syn::Visibility, attrs: &[syn::Attribute], default: bool| {
        let attrs: Vec<String> = attrs.iter().filter(|attr| !matches!(&attr.meta, syn::Meta::NameValue(value) if value.path.is_ident("doc"))).map(|attr| quote::quote!(#attr).to_string()).collect();
        (format!("{}{}", if default { "default " } else { "" }, quote::quote!(#vis)).trim().to_string(), attrs.join("\n"))
    };
    parsed
        .items
        .iter()
        .filter_map(|member| match member {
            syn::ImplItem::Const(constant) => {
                let (vis, attrs) = text(&constant.vis, &constant.attrs, constant.defaultness.is_some());
                Some(("constant", constant.ident.to_string(), vis, attrs))
            }
            syn::ImplItem::Type(ty) => {
                let (vis, attrs) = text(&ty.vis, &ty.attrs, ty.defaultness.is_some());
                Some(("type", ty.ident.to_string(), vis, attrs))
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
        let same = |list: &[(&'static str, String, String, String)], name: &str| list.iter().filter(|(k, n, _, _)| k == kind && n == name).count() == 1;
        let Some((_, _, new_vis, new_attrs)) = new.iter().find(|(k, n, _, _)| k == kind && *n == name_after).filter(|_| same(&old, name) && same(&new, &name_after)) else { continue };
        for (part, a, b) in [("visibility", old_vis, new_vis), ("attributes", old_attrs, new_attrs)] {
            if renames.apply(&before.path, a) != *b {
                let mut edit = text_edit(before, after, a.clone(), b.clone(), after.line_start);
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
    let old = own_tokens(&mut files.0, before_ws, before);
    let new = own_tokens(&mut files.1, after_ws, after);
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
    let renamed: Vec<String> = old_texts.iter().map(|text| if *text == old_name { new_name.clone() } else { renames.apply(&before.path, text) }).collect();
    let with_name: Vec<Token> = old.iter().zip(&renamed).map(|(token, text)| Token { text: text.clone(), ..token.clone() }).collect();
    if renamed == new_texts || renames.apply(&before.path, &join(&with_name)) == join(&new) {
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
    Some(text_edit(before, after, join(&old), join(&new), new.get(head).map(|token| token.at.0).unwrap_or(after.line_start)))
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
