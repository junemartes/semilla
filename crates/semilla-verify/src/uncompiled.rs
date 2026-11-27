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

use semilla_syntax::tt::{Delimiter, LineColumn, Spacing, TokenStream, TokenTree};
use semilla_syntax::Workspace;

use crate::cfgcond::{self, Pred};
use crate::checks::BodyIndex;
use crate::ConfigTruth;

/// The start of the text of the spotlight.
pub(crate) const START: &str = "inactive code: ";

/// The start of the note of a fact with the result `no compare`.
pub(crate) const NO_COMPARE: &str = "no compare:";

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
    flatten(TokenStream::with_places(text).ok()?, &mut out);
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

/// The condition and the text of an attribute that removes its node: `cfg(P)`, or
/// `cfg_attr(P, cfg(Q))` (the node exists if `P` is false or `Q` is true). `content` is the
/// list of the tokens in the brackets of the attribute.
fn condition(content: &[TokenTree]) -> Option<(Pred, String)> {
    let [TokenTree::Ident(path), TokenTree::Group(list)] = content else { return None };
    if list.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    let text = |name: &str| format!("#[{name}({})]", cfgcond::show(&crate::srcconfig::compact(&list.stream().to_string())));
    if *path == "cfg" {
        let trees: Vec<TokenTree> = list.stream().into_iter().collect();
        return Some((cfgcond::parse(&trees), text("cfg")));
    }
    if *path != "cfg_attr" {
        return None;
    }
    let mut parts = cfgcond::parts(list.stream()).into_iter();
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

/// The `cfg` ranges of a file text. A text that is not a Rust file has no range.
///
/// The range of an attribute is the range of its node (an item, a field, a statement, an
/// expression, …). An inner attribute of the file applies to the full file. An inner attribute
/// of a module, a function, or a block applies to that node.
fn regions(text: &str) -> Vec<Region> {
    use semilla_syntax::rs::{AstNode, Lines, SyntaxKind, ast, code_range};
    let parsed = [semilla_syntax::rs::Edition::Edition2021, semilla_syntax::rs::Edition::Edition2024, semilla_syntax::rs::Edition::Edition2015].into_iter().find_map(|edition| semilla_syntax::rs::parse_file(text, edition).ok());
    let Some(file) = parsed else { return Vec::new() };
    let lines = Lines::new(text);
    let pos = |offset: usize| lines.pos(offset);
    let span = |node: &semilla_syntax::rs::SyntaxNode| {
        let range = code_range(node);
        (pos(range.start), pos(range.end))
    };
    let mut out = Vec::new();
    for node in file.syntax().descendants() {
        if node.kind() == SyntaxKind::ATTR {
            let stream = TokenStream::from_node(&node);
            let content: Vec<TokenTree> = stream.into_iter().find_map(|tree| match tree {
                TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket => Some(group.stream().into_iter().collect()),
                _ => None,
            }).unwrap_or_default();
            let Some((pred, shown)) = condition(&content) else { continue };
            let Some(mut owner) = node.parent() else { continue };
            // An inner attribute in a list or a block applies to the node of the list.
            if matches!(owner.kind(), SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST | SyntaxKind::EXTERN_ITEM_LIST | SyntaxKind::MATCH_ARM_LIST | SyntaxKind::STMT_LIST) {
                owner = owner.parent().unwrap_or(owner);
                if owner.kind() == SyntaxKind::BLOCK_EXPR && owner.parent().is_some_and(|parent| parent.kind() == SyntaxKind::FN) {
                    owner = owner.parent().unwrap_or(owner);
                }
            }
            let (start, end) = if owner.kind() == SyntaxKind::SOURCE_FILE { ((0, 0), (usize::MAX, usize::MAX)) } else { span(&owner) };
            out.push(Region { start, end, attr: span(&node), preds: vec![pred], macro_input: false, text: shown });
            continue;
        }
        // The syntax tree does not go into the input of a macro call. A `cfg` condition in
        // the input can remove a part of it.
        let (first, tree, name) = if let Some(call) = ast::MacroCall::cast(node.clone()) {
            let name = call.path().and_then(|path| path.segment()).and_then(|segment| segment.name_ref()).map(|name| name.text().to_string()).unwrap_or_default();
            (call.path().map(|path| path.syntax().text_range().start()), call.token_tree(), name)
        } else if let Some(definition) = ast::MacroRules::cast(node.clone()) {
            (definition.macro_rules_token().map(|token| token.text_range().start()), definition.token_tree(), "macro_rules".to_string())
        } else {
            continue;
        };
        let (Some(first), Some(tree)) = (first, tree) else { continue };
        let input: TokenStream = TokenStream::from_node(tree.syntax()).into_iter().flat_map(|tree| match tree {
            TokenTree::Group(group) => group.stream(),
            _ => TokenStream::new(),
        }).collect();
        let mut preds: Vec<Pred> = Vec::new();
        for condition in cfgcond::conditions(input) {
            let pred = cfgcond::parse(&condition.into_iter().collect::<Vec<_>>());
            if !preds.contains(&pred) {
                preds.push(pred);
            }
        }
        if !preds.is_empty() {
            let (start, end) = (pos(usize::from(first)), pos(usize::from(tree.syntax().text_range().end())));
            out.push(Region { start, end, attr: ((0, 0), (0, 0)), preds, macro_input: true, text: name });
        }
    }
    out
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
    /// True if no checked unit reads the file (design 37.3): the file gets one spotlight.
    pub whole: bool,
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

/// [R85.4-1] The clause that names what checks code under the conditions `around` (design 85):
/// the option of the first configuration outside the list whose predicted units that read the
/// file make each condition true, else "not on this target" if the target makes a condition
/// false, else "no option of Semilla switches the condition". `None` with the full list (no
/// other configuration).
fn check(around: &[&Region], file: &str, side: usize, others: &[crate::OtherConfig]) -> Option<String> {
    if others.is_empty() {
        return None;
    }
    // A feature that the conditions name comes first: `--features x` for `feature = "x"`.
    let mut named = BTreeSet::new();
    around.iter().flat_map(|region| &region.preds).for_each(|pred| cfgcond::atoms(pred, &mut named));
    let first: Vec<String> = named.iter().filter_map(|atom| atom.strip_prefix("feature=\"")?.strip_suffix('"').map(|name| format!("--features {name}"))).collect();
    let order = others.iter().filter(|other| first.contains(&other.option)).chain(others.iter().filter(|other| !first.contains(&other.option)));
    for other in order {
        let units: Vec<&crate::Unit> = other.truths[side].units.iter().filter(|unit| unit.files.contains(file)).collect();
        if units.iter().any(|unit| around.iter().all(|region| region.compiled(unit, &units))) {
            return Some(format!("`{}` checks it", other.option));
        }
    }
    let preds: Vec<&Pred> = around.iter().flat_map(|region| &region.preds).collect();
    Some(no_option(&preds, others, side))
}

/// The clause for conditions that no configuration of Semilla makes true.
fn no_option(preds: &[&Pred], others: &[crate::OtherConfig], side: usize) -> String {
    let host: BTreeSet<String> = others.first().and_then(|other| other.truths[side].units.first()).map(|unit| unit.cfg.clone()).unwrap_or_default();
    if preds.iter().any(|pred| target_value(pred, &host) == Some(false)) { "not on this target".to_string() } else { "no option of Semilla switches the condition".to_string() }
}

/// [R85.4-1] The clause for a file that no checked unit reads: the `cfg` attributes of the
/// modules of the file, with the units of each configuration outside the list that read the
/// file of the module.
fn whole_check(workspace: &Workspace, file: &str, side: usize, others: &[crate::OtherConfig]) -> Option<String> {
    if others.is_empty() {
        return None;
    }
    let modules: Vec<&semilla_syntax::Item> = workspace.items.values().filter(|item| item.kind == semilla_syntax::ItemKind::Mod && item.file != file && workspace.items.values().any(|child| child.file == file && crate::checks::is_child(&child.path, &item.path))).collect();
    // A target with `required-features` (a bench, an example, a test, a binary).
    if let Some(other) = others.iter().find(|other| other.roots[side].contains(file)) {
        return Some(format!("`{}` checks it", other.option));
    }
    let preds: Vec<(Pred, &str)> = modules.iter().flat_map(|module| module.attrs.iter().flat_map(|attr| cfgcond::own_conditions(attr)).map(|pred| (pred, module.file.as_str()))).collect();
    if preds.is_empty() {
        return Some("Semilla finds no option that checks it".to_string());
    }
    for other in others {
        let true_in = |pred: &Pred, owner: &str| other.truths[side].units.iter().filter(|unit| unit.files.contains(owner)).any(|unit| cfgcond::eval(pred, &unit.cfg) == Some(true));
        if preds.iter().all(|(pred, owner)| true_in(pred, owner)) {
            return Some(format!("`{}` checks it", other.option));
        }
    }
    let list: Vec<&Pred> = preds.iter().map(|(pred, _)| pred).collect();
    Some(no_option(&list, others, side))
}

/// [R85.4-2] The note of a step with code that no checked configuration compiles and that a
/// configuration outside the list compiles: the options of the command line that check each
/// such piece. One configuration gives its words of cargo; more give one `--configuration NAME`
/// for each.
pub(crate) fn check_note(dead: &[Dead], others: &[crate::OtherConfig]) -> Option<String> {
    let used: Vec<&crate::OtherConfig> = others.iter().filter(|other| dead.iter().any(|entry| entry.cause.contains(&format!("`{}` checks it", other.option)))).collect();
    let options = match used.as_slice() {
        [] => return None,
        [one] => one.option.clone(),
        more => more.iter().map(|other| format!("--configuration '{}'", other.name)).collect::<Vec<_>>().join(" "),
    };
    Some(format!("to check the changed code that no checked configuration compiles, add `{options}` to the command (or `--configuration full`)"))
}

/// [R85.4-4] The subject of a key of a manifest: a dependency (`dependencies.NAME…`,
/// `dev-dependencies.NAME…`, `build-dependencies.NAME…`) or a feature (`features.NAME`).
enum Subject {
    Dependency(String),
    Feature(String),
}

fn subject(part: &str) -> Option<Subject> {
    let mut words = part.split('.');
    match (words.next()?, words.next()) {
        ("dependencies" | "dev-dependencies" | "build-dependencies", Some(name)) => Some(Subject::Dependency(name.split([':', ' ']).next()?.trim_matches('"').to_string())),
        ("features", Some(name)) => Some(Subject::Feature(name.split([':', ' ']).next()?.trim_matches('"').to_string())),
        _ => None,
    }
}

/// True if a feature state uses the subject in a package folder of one tree: a feature that is
/// on, or a dependency that is not optional or that a feature of the state turns on (`NAME`,
/// `dep:NAME`, `NAME/x`).
fn uses(subject: &Subject, state: Option<&BTreeSet<String>>, dependencies: Option<&std::collections::BTreeMap<String, bool>>) -> bool {
    let on = |name: &str| state.is_some_and(|state| state.contains(name));
    match subject {
        Subject::Feature(name) => on(name),
        Subject::Dependency(name) => match dependencies.and_then(|list| list.get(name)) {
            None => false,
            Some(false) => true,
            Some(true) => on(name) || on(&format!("dep:{name}")) || state.is_some_and(|state| state.iter().any(|entry| entry.strip_prefix(name.as_str()).is_some_and(|rest| rest.starts_with('/')))),
        },
    }
}

/// [R85.4-4] Each fact of a key of a manifest (a dependency or a feature) that no checked
/// configuration uses, and that a configuration outside the list uses: the fact is not `no
/// code`. It gets `no compare` with a note that names the option (design 85).
pub(crate) fn manifest_checks(report: &mut semilla_core::StepReport, table: &crate::FeatureTable, others: &[crate::OtherConfig]) {
    use semilla_core::{Category, Compare};
    for fact in report.facts.iter_mut().filter(|fact| fact.category == Category::Manifest && fact.compare == Compare::NoCode) {
        // The part of a manifest fact is `FOLDER/Cargo.toml:KEY …`.
        let Some((file, key)) = fact.part.split_once("Cargo.toml:") else { continue };
        let Some(subject) = subject(key) else { continue };
        let folder = file.trim_end_matches('/');
        let used = |states: &[std::collections::BTreeMap<String, BTreeSet<String>>; 2]| (0..2).any(|side| uses(&subject, states[side].get(folder), table.dependencies[side].get(folder)));
        if used(&table.checked) {
            continue;
        }
        // The option of the name of the subject comes first: `--features x` for `x`.
        let own = match &subject {
            Subject::Dependency(name) | Subject::Feature(name) => format!("--features {name}"),
        };
        let Some(other) = others.iter().filter(|other| other.option == own).chain(others.iter()).find(|other| used(&other.states)) else { continue };
        let what = match &subject {
            Subject::Dependency(name) => format!("the dependency `{name}`"),
            Subject::Feature(name) => format!("the feature `{name}`"),
        };
        fact.compare = Compare::NoCompare;
        let text = format!("{NO_COMPARE} no checked configuration uses {what}; `{}` checks it", other.option);
        fact.note = Some(match fact.note.take() {
            Some(old) => format!("{old}; {text}"),
            None => text,
        });
    }
}

/// A cause with the clause of [`check`] in its last parentheses.
fn with_check(cause: String, check: Option<String>) -> String {
    match (check, cause.strip_suffix(')')) {
        (Some(check), Some(head)) => format!("{head}; {check})"),
        (Some(check), None) => format!("{cause} ({check})"),
        (None, _) => cause,
    }
}

/// Each changed line of the step that no checked configuration compiles.
///
/// `truths` has the units of the checked configurations of the two sides. An empty list
/// gives no result: the oracle has no data. A configuration with no unit record gives the
/// careful rule: a token in a `cfg` range is dead if its item has no body record.
pub(crate) fn dead_edits(sides: [&Workspace; 2], truths: &[Vec<ConfigTruth>; 2], bodies: &BodyIndex, others: &[crate::OtherConfig]) -> Vec<Dead> {
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
                let cause = with_check(unread(sides[side], file), whole_check(sides[side], file, side, others));
                for (token, _) in list.iter().zip(&changed).filter(|(_, flag)| **flag) {
                    out.insert(Dead { file: file.to_string(), side, line: token.at.0, cause: cause.clone(), whole: true });
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
                    false => format!("{NOT_COMPILED} ({}{})", attrs.join(", "), check(&around, file, side, others).map(|text| format!("; {text}")).unwrap_or_default()),
                    true => format!("{UNSEEN} ({})", macros.join(", ")),
                };
                out.insert(Dead { file: file.to_string(), side, line: token.at.0, cause, whole: false });
            }
        }
    }
    out.into_iter().collect()
}

/// The text of the spotlight for a list of causes.
pub(crate) fn spotlight_text(causes: &[String]) -> String {
    format!("{START}{}, so Semilla did not compare the edit", causes.join("; "))
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
    use semilla_core::{Anchor, Category, ClaimStatus, Compare};
    if dead.is_empty() {
        return;
    }
    let hit = |side: usize, span: &(String, usize, usize)| dead.iter().find(|entry| entry.side == side && entry.file == span.0 && span.1 <= entry.line && entry.line <= span.2);
    // 1. The identity claims.
    // A derived identity (design 38.10) is no claim of the agent: its facts get `no compare`
    // below, and the rename or the move stays one fact.
    for claim in step.claims.iter().filter(|claim| matches!(claim.op.as_str(), "rename" | "move-item") && !claim.id.starts_with("derived-identity-")) {
        let Some(at) = &claim.at else { continue };
        let new = renames.items.get(&at.item).cloned().unwrap_or_else(|| at.item.clone());
        let found = span_of(sides[0], &Anchor::item(&at.item)).and_then(|span| hit(0, &span)).or_else(|| span_of(sides[1], &Anchor::item(&new)).and_then(|span| hit(1, &span)));
        let Some(found) = found else { continue };
        for result in report.claims.iter_mut().filter(|result| result.claim == claim.id && result.status.is_verified()) {
            result.status = ClaimStatus::Refuted { reason: format!("{}, so Semilla cannot compare the code of `{}`", found.cause, at.item) };
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
    let mut quiet: BTreeSet<usize> = BTreeSet::new();
    let mut whole_files: BTreeSet<String> = BTreeSet::new();
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
        // A file that no checked unit reads (design 37.3): the file gets one spotlight. Each
        // fact of the file keeps its text and gets the compare result `no compare`.
        if entry.whole {
            quiet.extend(owners.iter().copied());
            let list = free.entry((None, entry.file.clone())).or_default();
            if !list.contains(&entry.cause) {
                list.push(entry.cause.clone());
            }
            whole_files.insert(entry.file.clone());
            continue;
        }
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
    let no_compare = |fact: &mut semilla_core::Fact| {
        let place = fact.at.as_ref().map(|at| at.item.clone()).or_else(|| fact.file.clone()).unwrap_or_default();
        let note = format!("{NO_COMPARE} no checked configuration compiles `{place}`");
        fact.compare = Compare::NoCompare;
        fact.note = Some(match fact.note.take() {
            Some(old) if old.contains(&note) => old,
            Some(old) => format!("{old}; {note}"),
            None => note,
        });
    };
    for index in quiet.iter().copied().filter(|index| !causes.contains_key(index)) {
        let fact = &mut report.facts[index];
        // The spotlight of the file has the cause of this spotlight.
        if fact.spotlight_why.as_deref() == Some(crate::facts::INACTIVE) {
            fact.spotlight = false;
            fact.spotlight_why = None;
        }
        if fact.affected.is_empty() {
            no_compare(fact);
        }
    }
    for (index, list) in causes {
        let fact = &mut report.facts[index];
        let text = spotlight_text(&list);
        fact.spotlight_why = Some(match fact.spotlight_why.take() {
            // The old text of a fact in a function with no body record says less.
            Some(why) if why == crate::facts::INACTIVE => text,
            Some(why) => format!("{why}; {text}"),
            None => text,
        });
        fact.spotlight = true;
        // The compare has no data for this code (design 35). A fact with a changed body keeps
        // its result.
        if fact.affected.is_empty() {
            no_compare(fact);
        }
        if fact.reason.is_none() {
            fact.reason = reason_of(fact);
        }
    }
    // The spotlight list of the report has one line for each file that no unit reads.
    if !whole_files.is_empty() {
        let in_whole = |line: &str| {
            let item = line.strip_prefix(START).and_then(|rest| rest.rsplit_once("(no checked configuration compiles `")).and_then(|(_, rest)| rest.strip_suffix("`)"));
            item.is_some_and(|item| sides.iter().any(|side| side.items.get(item).is_some_and(|found| whole_files.contains(&found.file))))
        };
        report.spotlight.retain(|line| !in_whole(line));
        report.spotlight.extend(whole_files.iter().map(|file| format!("{START}{file} (no checked unit reads the file)")));
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
                None if whole_files.contains(&file) => format!("{file}: each edit of this file is in code that no checked configuration compiles"),
                None => "an edit in code that no checked configuration compiles".into(),
            },
            status: "derived".into(),
            category: Category::Text,
            compare: Compare::NoCompare,
            note: Some(format!("{NO_COMPARE} no checked configuration compiles `{}`", item.clone().unwrap_or_else(|| file.clone()))),
            spotlight: true,
            spotlight_why: Some(spotlight_text(&list)),
            ..Default::default()
        };
        fact.reason = reason_of(&fact);
        report.facts.push(fact);
    }
}

/// R68.12-1: true for an atom of a property of the target (`target_feature="sse2"`,
/// `target_os="linux"`, `unix`, `windows`). The build check builds for one target, so the target
/// decides these atoms. Each other atom (a feature, `test`, `debug_assertions`) is not known here.
pub(crate) fn target_atom(atom: &str) -> bool {
    let name = atom.split('=').next().unwrap_or(atom);
    name.starts_with("target_") || name == "unix" || name == "windows"
}

/// R68.12-1: the value of a condition with the target atoms of `host` and each other atom
/// unknown: `Some(false)` only if no value of the other atoms makes the condition true.
fn target_value(pred: &Pred, host: &BTreeSet<String>) -> Option<bool> {
    match pred {
        Pred::Atom(atom) if target_atom(atom) => Some(host.contains(atom)),
        Pred::Atom(_) | Pred::Other => None,
        Pred::Not(inner) => target_value(inner, host).map(|value| !value),
        Pred::All(list) => {
            let values: Vec<Option<bool>> = list.iter().map(|pred| target_value(pred, host)).collect();
            if values.contains(&Some(false)) { Some(false) } else if values.iter().all(|value| *value == Some(true)) { Some(true) } else { None }
        }
        Pred::Any(list) => {
            let values: Vec<Option<bool>> = list.iter().map(|pred| target_value(pred, host)).collect();
            if values.contains(&Some(true)) { Some(true) } else if values.iter().all(|value| *value == Some(false)) { Some(false) } else { None }
        }
    }
}

/// R68.12-1: one `cfg` attribute of the new tree whose condition the target of the build check
/// makes false, around changed code: the check did not build that code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TargetHidden {
    pub file: String,
    /// The line of the attribute.
    pub line: usize,
    /// The text of the attribute: `#[cfg(not(target_feature = "sse2"))]`.
    pub attr: String,
    /// The changed lines of the new tree in the range of the attribute.
    pub changed: Vec<usize>,
}

impl TargetHidden {
    /// The line of the step output.
    pub fn text(&self, target: &str) -> String {
        let lines: Vec<String> = self.changed.iter().map(|line| format!("{}:{line}", self.file)).collect();
        format!("! build check: not built: the check did not build the changed code under `{}` at {}:{} (changed lines: {}): the target of the check ({target}) makes the condition false", self.attr, self.file, self.line, lines.join(", "))
    }
}

/// R68.12-1: each `cfg` attribute of the new tree that hides changed code from the build check:
/// its condition is false for each value of the atoms that are not properties of the target
/// (`host` has the true atoms of the target of the check, from `rustc --print cfg`). The input
/// of a macro call is not a part of the rule: Semilla cannot see which code its condition
/// selects. The rule only adds lines.
pub fn target_hidden(sides: [&Workspace; 2], host: &BTreeSet<String>) -> Vec<TargetHidden> {
    let mut found: Vec<TargetHidden> = Vec::new();
    for (file, text) in &sides[1].rust_files {
        let old = sides[0].rust_files.get(file);
        if old == Some(text) || !text.contains("cfg") {
            continue;
        }
        let Some(new) = tokens(text) else { continue };
        let old = old.and_then(|old| tokens(old)).unwrap_or_default();
        let (_, changed) = diff(&old, &new);
        let regions: Vec<Region> = regions(text).into_iter().filter(|region| !region.macro_input && region.preds.len() == 1).collect();
        for (token, _) in new.iter().zip(&changed).filter(|(_, flag)| **flag) {
            let inside = |range: (At, At)| range.0 <= token.at && token.at < range.1;
            for region in regions.iter().filter(|region| inside((region.start, region.end)) && !inside(region.attr)) {
                if target_value(&region.preds[0], host) != Some(false) {
                    continue;
                }
                let line = region.attr.0.0;
                match found.iter_mut().find(|entry| entry.file == *file && entry.line == line) {
                    Some(entry) => {
                        if !entry.changed.contains(&token.at.0) {
                            entry.changed.push(token.at.0);
                        }
                    }
                    None => found.push(TargetHidden { file: file.clone(), line, attr: region.text.clone(), changed: vec![token.at.0] }),
                }
            }
        }
    }
    found.sort();
    found
}

/// R68.12-1: the true atoms of the output of `rustc --print cfg` (`target_os="linux"`, `unix`).
pub fn print_cfg_atoms(text: &str) -> BTreeSet<String> {
    text.lines().map(str::trim).filter(|line| !line.is_empty()).map(|line| line.split_whitespace().collect::<String>()).collect()
}
