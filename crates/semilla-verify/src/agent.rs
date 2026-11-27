//! The facts of the agent (design 54): the check of each `same` and each `change` fact against
//! the facts of the syntax tree, the origin of each fact, and the gap list.
//!
//! The general rule: the tag of the agent must agree with the edit that Semilla sees. This
//! module makes no fact and removes no fact. It only puts a fact below a fact of the agent
//! (the origin `stated`), so no rule of it can hide an edit.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::{AgentFact, Gap};
use semilla_core::{Anchor, Category, Fact};
use semilla_syntax::{Edit, ItemKind, Node, Workspace};

use crate::identity::Candidate;
use crate::paths::Index;
use crate::{StepInput, StepOutcome};

/// The message for a kind of `same` claim that has no check yet.
const LATER: &str = "has no check yet (a later batch adds it). Give no fact for it: Semilla shows each edit of it as a fact, and the gap list names the items";

/// The forms of a `same` claim.
const SAME_FORMS: &str = "the forms are `rename OLD NEW`, `move FROM TO`, and `rewrite FROM => TO`";

/// One parsed `same` claim.
#[derive(Clone, Debug, PartialEq)]
enum Same {
    Rename(String, String),
    Move(String, String),
    Rewrite(String, String),
}

/// Parses the text of a `--same` option.
fn parse_same(text: &str) -> Result<Same, String> {
    let text = text.trim();
    let (kind, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let rest = rest.trim();
    // The two sides: around `=>`, or two words.
    let sides = |arrow: bool| -> Option<(String, String)> {
        if let Some((from, to)) = rest.split_once(" => ") {
            return (!from.trim().is_empty() && !to.trim().is_empty()).then(|| (from.trim().to_string(), to.trim().to_string()));
        }
        // The words. The target of a path (` (test "list")`, design 56.1) is a part of its word.
        let mut words: Vec<String> = Vec::new();
        let (mut at, mut open) = (0, false);
        while at < rest.len() {
            let next = rest[at..].chars().next().unwrap_or(' ');
            match semilla_syntax::load::target_end(&rest[at..]).filter(|_| open) {
                Some(end) => {
                    words.last_mut().unwrap().push_str(&rest[at..at + end]);
                    at += end;
                }
                None if next.is_whitespace() => {
                    open = false;
                    at += next.len_utf8();
                }
                None => {
                    if !open {
                        words.push(String::new());
                        open = true;
                    }
                    words.last_mut().unwrap().push(next);
                    at += next.len_utf8();
                }
            }
        }
        (!arrow && words.len() == 2).then(|| (words[0].clone(), words[1].clone()))
    };
    match kind {
        "rename" => sides(false).map(|(old, new)| Same::Rename(old, new)).ok_or_else(|| format!("`--same '{text}'`: write `rename OLD NEW` (for a path with a space: `rename OLD => NEW`)")),
        "move" => sides(false).map(|(old, new)| Same::Move(old, new)).ok_or_else(|| format!("`--same '{text}'`: write `move FROM TO` (for a path with a space: `move FROM => TO`)")),
        "rewrite" => {
            let (from, to) = sides(true).ok_or_else(|| format!("`--same '{text}'`: write `rewrite FROM => TO`, with a space on each side of `=>`"))?;
            // Design 54.16: a form error gives the correct form, if a repair of the text has one.
            if let Err(error) = Pattern::parse(&from, &to) {
                return Err(match repair(&from, &to) {
                    Some((from, to, cause)) => format!("{error}. {cause}. The correct form: `--same {}`", shell_quote(&format!("rewrite {from} => {to}"))),
                    None => error,
                });
            }
            Ok(Same::Rewrite(from, to))
        }
        "extract" | "inline" => Err(format!("`--same '{text}'`: the kind `{kind}` {LATER}")),
        _ => Err(format!("`--same '{text}'`: `{kind}` is not a kind of a `same` fact; {SAME_FORMS}")),
    }
}

/// The text of an option value for a shell: in single quote marks.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The repair of a pattern with a form error (design 54.16): the two sides that parse, and
/// the cause. The repairs are general: a side that starts with `.` gets a variable for the
/// receiver, and a side that ends with `;` loses it (a pattern is an expression). `None` if
/// the repaired text is not a pattern.
fn repair(from: &str, to: &str) -> Option<(String, String, String)> {
    // A variable name that the pattern does not have.
    let name = ('a'..='z').map(|c| format!("${c}")).find(|name| !from.contains(name.as_str()) && !to.contains(name.as_str()))?;
    let mut causes: Vec<String> = Vec::new();
    let mut side = |text: &str| {
        let mut text = text.trim().to_string();
        if text.ends_with(';') {
            text = text.trim_end_matches(';').trim_end().to_string();
            causes.push("A pattern has no `;` at the end".to_string());
        }
        if text.starts_with('.') && !text.starts_with("..") {
            text = format!("{name}{text}");
            causes.push(format!("A side that starts with `.` has no receiver: write a variable (`{name}`) for the receiver"));
        }
        text
    };
    let (new_from, new_to) = (side(from), side(to));
    causes.dedup();
    (!causes.is_empty() && Pattern::parse(&new_from, &new_to).is_ok()).then(|| (new_from, new_to, causes.join(". ")))
}

/// The text of a pattern side with the variable `$name` in place of the name `name`
/// (design 54.16). A name after `.`, `::`, or `$`, and a name before `::`, `(`, `!`, or `:`
/// stays: it is a field, a path part, a function, a macro, or a field name.
fn with_variable(text: &str, name: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let (mut out, mut at) = (String::new(), 0);
    while let Some(found) = text[at..].find(name) {
        let (start, end) = (at + found, at + found + name.len());
        let before = text[..start].trim_end();
        let after = text[end..].trim_start();
        let inside = text[..start].chars().next_back().is_some_and(word) || text[end..].chars().next().is_some_and(word);
        let fixed = before.ends_with('.') || before.ends_with("::") || before.ends_with('$') || after.starts_with("::") || after.starts_with('(') || after.starts_with('!') || (after.starts_with(':') && !after.starts_with("::"));
        out.push_str(&text[at..start]);
        if !inside && !fixed {
            out.push('$');
        }
        out.push_str(name);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// The plain names of a pattern side that can be a variable: a path of one part that starts
/// with a small letter.
fn plain_names(node: &Node, out: &mut Vec<String>) {
    let name = node.text.as_str();
    if node.kind == "path" && name.chars().next().is_some_and(|c| c.is_lowercase() || c == '_') && name.chars().all(|c| c.is_alphanumeric() || c == '_') && !matches!(name, "self" | "true" | "false" | "_") && !out.iter().any(|other| other == name) {
        out.push(name.to_string());
    }
    node.children.iter().for_each(|child| plain_names(child, out));
}

/// Parses the text of a `--change` option: the items and the sentence.
fn parse_change(text: &str) -> Result<(Vec<String>, String), String> {
    let form = || format!("`--change '{text}'`: write `ITEM[,ITEM]=TEXT`, with one sentence that tells how the behavior changes");
    let (items, sentence) = text.split_once('=').ok_or_else(form)?;
    // A comma in the generic arguments of a path is not a separator.
    let mut list = vec![String::new()];
    let mut depth = 0usize;
    for c in items.chars() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                list.push(String::new());
                continue;
            }
            _ => {}
        }
        list.last_mut().unwrap().push(c);
    }
    let list: Vec<String> = list.iter().map(|item| item.trim().to_string()).collect();
    if list.iter().any(String::is_empty) || sentence.trim().is_empty() {
        return Err(form());
    }
    Ok((list, sentence.trim().to_string()))
}

/// Checks the form of each agent fact before a step runs. The error is the message for the
/// agent.
pub fn check_form(facts: &semilla_core::AgentFacts) -> Result<(), String> {
    for text in &facts.same {
        parse_same(text)?;
    }
    for text in &facts.change {
        parse_change(text)?;
    }
    // Design 67: the parse errors of each intent file, each with its place.
    if let Err(errors) = crate::intent::parse_files(&facts.intent) {
        return Err(format!("the intent file has errors:\n{}", errors.iter().map(|error| format!("  {error}")).collect::<Vec<_>>().join("\n")));
    }
    Ok(())
}

/// The form of a path for a loose compare: no `crate::` at the start, and one space for each
/// run of white space.
fn plain(path: &str) -> String {
    let path = path.split_whitespace().collect::<Vec<_>>().join(" ");
    path.strip_prefix("crate::").unwrap_or(&path).to_string()
}

/// The other forms of an item path that an agent can write: without the twin numbers (`#N`),
/// and with `Type` in place of `<Type as Trait>`.
fn forms(path: &str) -> Vec<String> {
    let mut forms = vec![path.to_string()];
    let mut no_twin = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek().is_some_and(char::is_ascii_digit) {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        } else {
            no_twin.push(c);
        }
    }
    if no_twin != path {
        forms.push(no_twin.clone());
    }
    if let Some((before, ty, _, _, after)) = crate::impl_name_parts(&no_twin) {
        forms.push(format!("{before}{ty}{after}"));
    }
    forms
}

/// True if the loose path `query` names the full path `path`: the same path, or the last
/// segments of it.
fn loose(path: &str, query: &str) -> bool {
    let query = plain(query);
    forms(path).iter().any(|form| *form == query || form.ends_with(&format!("::{query}")))
}

/// Each pair "`OLD` → `NEW`" in the text of a fact.
fn pairs(text: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = text.split('`').collect();
    let mut out = Vec::new();
    // The parts with an odd index are the quoted texts.
    let mut index = 1;
    while index + 2 < parts.len() {
        if parts[index + 1] == " → " {
            out.push((parts[index].to_string(), parts[index + 2].to_string()));
        }
        index += 2;
    }
    out
}

/// Each quoted text of the text of a fact.
fn quoted(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

/// A pattern of the syntax tree (design 54.3): two expressions with variables (`$name`).
pub(crate) struct Pattern {
    from: Node,
    to: Node,
}

impl Pattern {
    /// Parses the two sides. The error is the message for the agent.
    pub(crate) fn parse(from: &str, to: &str) -> Result<Pattern, String> {
        let side = |text: &str| crate::pattern::parse_pattern(text).ok_or_else(|| format!("`{text}` is not an expression of Rust with variables (`$name`): a `rewrite` pattern is an expression on each side of `=>`"));
        let pattern = Pattern { from: side(from)?, to: side(to)? };
        if pattern.from.text == pattern.to.text {
            return Err(format!("the two sides of the pattern are equal (`{from}`): the pattern states no change"));
        }
        if variable(&pattern.from).is_some() {
            return Err(format!("the side `{from}` of the pattern is only a variable: it names each expression. Give the form of the old code"));
        }
        let (mut old, mut new) = (BTreeSet::new(), BTreeSet::new());
        names(&pattern.from, &mut old);
        names(&pattern.to, &mut new);
        if let Some(name) = new.difference(&old).next() {
            return Err(format!("the variable `${name}` is only in the new side of the pattern: each variable of `{to}` must have a value from the old code (`{from}`)"));
        }
        Ok(pattern)
    }

    /// True if the full difference between the old node and the new node is the pattern
    /// (design 54.3): the pair is a site, or each child of the pair is equal or covered.
    /// `sites` counts the sites.
    pub(crate) fn covers(&self, old: &Node, new: &Node, sites: &mut usize) -> bool {
        let (mut old_values, mut new_values) = (BTreeMap::new(), BTreeMap::new());
        if bind(&self.from, old, &mut old_values) && bind(&self.to, new, &mut new_values) {
            // One variable has one value at one site: the value of the old side is the value
            // of the new side, or the pattern covers the difference of the two values.
            let mut inner = 0;
            // [R77.3-6] Two values are equal by the syntax tree: the parentheses do not count.
            if new_values.iter().all(|(name, new)| old_values.get(name).is_some_and(|old| old.text == new.text || same_form(old, new) || self.covers(old, new, &mut inner))) {
                *sites += 1 + inner;
                return true;
            }
        }
        if old.kind != new.kind || old.attr != new.attr || old.children.is_empty() || old.children.len() != new.children.len() || old.own_text(&[]) != new.own_text(&[]) {
            return false;
        }
        let mut inner = 0;
        let covered = old.children.iter().zip(&new.children).all(|(a, b)| a.role == b.role && (a.text == b.text || self.covers(a, b, &mut inner)));
        *sites += if covered { inner } else { 0 };
        covered && inner > 0
    }
}

/// The name of a variable node of a pattern.
fn variable(node: &Node) -> Option<&str> {
    // [R77.3-6] `($a)` is the variable `$a`: the parentheses are not a part of the form.
    let text = without_parens(&node.text);
    (node.kind == "path" && !text.contains(' ')).then(|| text.strip_prefix(crate::pattern::META)).flatten()
}

/// The names of the variables of a pattern.
fn names(node: &Node, out: &mut BTreeSet<String>) {
    if let Some(name) = variable(node) {
        out.insert(name.to_string());
    }
    node.children.iter().for_each(|child| names(child, out));
}

/// Matches a node with one side of a pattern. A variable matches one node. A variable with two
/// places must have the same text at each place.
fn bind<'a>(pattern: &Node, node: &'a Node, values: &mut BTreeMap<String, &'a Node>) -> bool {
    if let Some(name) = variable(pattern) {
        return match values.get(name) {
            // [R77.3-6] Two values of one variable are the same expression by the syntax tree: the
            // parentheses of an expression are not a part of its form.
            Some(value) => same_form(value, node),
            None => {
                values.insert(name.to_string(), node);
                true
            }
        };
    }
    if pattern.kind != node.kind || pattern.attr != node.attr || pattern.children.len() != node.children.len() {
        return false;
    }
    if pattern.children.is_empty() {
        return without_parens(&pattern.text) == without_parens(&node.text);
    }
    pattern.children.iter().zip(&node.children).all(|(p, n)| p.role == n.role && bind(p, n, values))
}

/// [R77.3-6] The text of a leaf with no parentheses around it.
fn without_parens(text: &str) -> &str {
    let mut text = text.trim();
    while let Some(inner) = text.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
        text = inner.trim();
    }
    text
}

/// [R77.3-6] True if two nodes are one expression by the syntax tree (the kinds, the roles, and
/// the leaf texts; the parentheses do not count).
fn same_form(a: &Node, b: &Node) -> bool {
    if a.kind != b.kind || a.attr != b.attr || a.children.len() != b.children.len() {
        return false;
    }
    if a.children.is_empty() {
        return without_parens(&a.text) == without_parens(&b.text);
    }
    a.children.iter().zip(&b.children).all(|(x, y)| x.role == y.role && same_form(x, y))
}

/// [R77.3-6] True if an expression of this kind needs parentheses in the place of a child of a
/// node of the kind `parent` (a receiver, a field base, an index base, an operand).
fn needs_parens(kind: &str, parent: Option<&str>) -> bool {
    // The precedence of the Rust grammar, from the highest: a postfix operation (a method call,
    // a field, an index, `?`, `.await`), a unary operation and `&`, `as`, then the binary
    // operations. A child of a lower rank than its parent needs parentheses (a binary child of a
    // binary operation too: the safe side for two ranks of binary operations).
    let rank = |kind: &str| -> u8 {
        match kind {
            "method_call" | "field" | "index" | "try" | "await" => 4,
            "unary" | "ref" => 3,
            "cast" => 2,
            "binary" => 1,
            "assign" => 0,
            _ => 5,
        }
    };
    let Some(parent) = parent else { return false };
    let (own, outer) = (rank(kind), rank(parent));
    outer < 5 && (own < outer || (own == 1 && outer == 1))
}

/// The pattern that one edit gives (design 54.4): the old text and the new text, with a
/// variable in place of each part of the old node that the new node has too. `None` if the
/// texts give no pattern that covers the edit.
fn generalize(old: &Node, new: &Node, old_file: &str, new_file: &str) -> Option<(String, String)> {
    let source = |file: &'_ str, node: &Node| file.get(node.span.0..node.span.1).map(str::to_string);
    let (old_source, new_source) = (source(old_file, old)?, source(new_file, new)?);
    // The parts of the new node, by text.
    let mut new_parts: Vec<&Node> = Vec::new();
    new.walk(&mut Vec::new(), &mut |_, node| new_parts.push(node));
    // The parts of the old node that become variables: the outermost first. A literal stays.
    fn pick<'a>(node: &'a Node, root: bool, new_parts: &[&Node], out: &mut Vec<&'a Node>) {
        if !root && node.kind != "lit" && node.span.1 > node.span.0 && new_parts.iter().any(|part| part.text == node.text && part.kind == node.kind) {
            out.push(node);
            return;
        }
        node.children.iter().for_each(|child| pick(child, false, new_parts, out));
    }
    let mut picked: Vec<&Node> = Vec::new();
    pick(old, true, &new_parts, &mut picked);
    let mut texts: Vec<&str> = Vec::new();
    for node in &picked {
        if !texts.contains(&node.text.as_str()) {
            texts.push(&node.text);
        }
    }
    let name = |text: &str| texts.iter().position(|t| *t == text).map(|index| format!("${}", (b'a' + (index % 26) as u8) as char));
    // The text of a side, with the name of the variable in place of each picked part.
    // [R77.3-6] A variable in the place of a receiver or an operand gets parentheses when its
    // value is an expression of a lower rank (a binary operation, a cast): the text of the
    // pattern is then valid Rust for each value.
    fn replace(node: &Node, root: bool, source: &str, start: usize, name: &dyn Fn(&str) -> Option<String>, out: &mut Vec<(usize, usize, String)>, parent: Option<&str>) {
        if let Some(name) = name(&node.text).filter(|_| !root && node.span.1 > node.span.0) {
            let text = if needs_parens(&node.kind, parent) { format!("({name})") } else { name };
            out.push((node.span.0.saturating_sub(start), node.span.1.saturating_sub(start), text));
            return;
        }
        node.children.iter().for_each(|child| replace(child, false, source, start, name, out, Some(&*node.kind)));
    }
    let side = |node: &Node, text: &str| -> Option<String> {
        let mut changes = Vec::new();
        replace(node, true, text, node.span.0, &name, &mut changes, None);
        changes.sort();
        let (mut out, mut at) = (String::new(), 0);
        for (start, end, name) in changes {
            if start < at || end > text.len() {
                return None;
            }
            out.push_str(text.get(at..start)?);
            out.push_str(&name);
            at = end;
        }
        out.push_str(text.get(at..)?);
        Some(semilla_syntax::tree::one_line(&out))
    };
    let check = |from: String, to: String| Pattern::parse(&from, &to).ok().filter(|pattern| pattern.covers(old, new, &mut 0)).map(|_| (from, to));
    side(old, &old_source)
        .zip(side(new, &new_source))
        .and_then(|(from, to)| check(from, to))
        .or_else(|| check(semilla_syntax::tree::one_line(&old_source), semilla_syntax::tree::one_line(&new_source)))
}

/// One place of a fact: an edit of the diff, or the place of a fact with no edit.
struct Unit {
    fact: usize,
    /// The path of the item in the old tree and in the new tree.
    items: Vec<String>,
    file: Option<String>,
    place: String,
    edit: Option<usize>,
}

/// The item or the file that one text of a `change` fact names.
struct Target {
    name: String,
    /// The paths of the item and of each item in it, in the two trees.
    scope: BTreeSet<String>,
    file: Option<String>,
    /// The item is only in the new tree, or only in the old tree.
    one_side: Option<&'static str>,
    /// The paths in the old tree, for the sites of a rename.
    old: Vec<String>,
    /// A text for the result line. The path names two items or more that a path cannot tell
    /// apart (design 54.19), or it names a file that Semilla does not parse (design 54.20).
    mark: Option<String>,
}

struct Check<'a> {
    before: &'a Workspace,
    after: &'a Workspace,
    edits: &'a [Edit],
    facts: &'a [Fact],
    renames: &'a semilla_syntax::Renames,
    units: Vec<Unit>,
    /// The parts of each item path of the old tree and of the new tree (design 54.10).
    old_paths: std::cell::OnceCell<Index<'a>>,
    new_paths: std::cell::OnceCell<Index<'a>>,
    /// The cause for each stated rename or move that is no fact (design 54.11).
    stated_causes: &'a BTreeMap<usize, String>,
}

/// The lines for a path that does not name one item of a tree: the candidates in the order of
/// the parts that agree, and the macro calls that can make the item.
///
/// The note of a macro call prints only if no item candidate has the name that the input of
/// the call has (design 54.21): with such a candidate, the item is in the syntax tree. The
/// note prints one time for each macro name, with the count of the calls.
fn candidate_lines(query: &str, trees: &[&Index]) -> Vec<String> {
    let mut near: Vec<(usize, String)> = Vec::new();
    for tree in trees {
        for path in tree.near(query) {
            let label = tree.label(path);
            if !near.iter().any(|(_, other)| *other == label) {
                near.push((tree.rank(query, path), label));
            }
        }
    }
    near.sort_by(|a, b| b.0.cmp(&a.0));
    let labels: Vec<String> = near.into_iter().map(|(_, label)| label).collect();
    let mut lines = Vec::new();
    if !labels.is_empty() {
        lines.push(format!("candidates: {}", crate::paths::list(&labels)));
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let has = |label: &String, name: &str| label.match_indices(name).any(|(at, _)| !label[..at].chars().last().is_some_and(word) && !label[at + name.len()..].chars().next().is_some_and(word));
    // The calls of each macro name: the first call, the name in its input, and the count.
    let mut makers: Vec<(String, &str, String, usize)> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for tree in trees {
        for (call, name) in tree.makers(query) {
            if seen.contains(&call) || labels.iter().any(|label| has(label, &name)) {
                continue;
            }
            seen.push(call);
            let mac = crate::paths::no_twin(call.rsplit("::").next().unwrap_or(call));
            match makers.iter_mut().find(|(other, ..)| *other == mac) {
                Some(group) => group.3 += 1,
                None => makers.push((mac, call, name, 1)),
            }
        }
    }
    for (mac, call, name, count) in makers.iter().take(3) {
        let module = call.rsplit_once("::").map(|(module, _)| module).unwrap_or(call);
        lines.push(match count {
            1 => format!("an item that a macro call makes has no path in the syntax tree. The input of the macro call `{call}` has `{name}`: give the macro call `{call}` or its module `{module}`"),
            _ => format!("an item that a macro call makes has no path in the syntax tree. The input of {count} calls of the macro `{mac}` has `{name}`: give one macro call (the first is `{call}`) or its module `{module}`"),
        });
    }
    lines
}

impl<'a> Check<'a> {
    fn old_paths(&self) -> &Index<'a> {
        self.old_paths.get_or_init(|| Index::new(self.before))
    }

    fn new_paths(&self) -> &Index<'a> {
        self.new_paths.get_or_init(|| Index::new(self.after))
    }

    fn item<'b>(&'b self, path: &str) -> Option<&'b semilla_syntax::Item> {
        self.after.items.get(path).or_else(|| self.before.items.get(path))
    }

    /// True if the item is in a test or in a bench.
    fn is_test(&self, path: &str) -> bool {
        self.item(path).is_some_and(|item| item.test || bench_file(&item.file) || item.attrs.iter().any(|attr| attr.replace(' ', "") == "#[bench]"))
    }

    /// True if a unit is in a test or in a bench.
    fn test_unit(&self, unit: &Unit) -> bool {
        if unit.items.is_empty() {
            return unit.file.as_deref().is_some_and(|file| bench_file(file) || file.split('/').any(|part| part == "tests"));
        }
        unit.items.iter().all(|item| self.is_test(item))
    }

    /// True if the item of a unit is in the old tree and in the new tree.
    fn existing(&self, unit: &Unit) -> bool {
        let old = |path: &str| self.before.items.contains_key(path) || self.renames.items.values().any(|new| new == path);
        let new = |path: &str| self.after.items.contains_key(path) || self.renames.items.contains_key(path);
        // R68.5-1: an override of a default method is existing code: the impl had the method
        // with the default body.
        if self.facts.get(unit.fact).is_some_and(|fact| fact.part == crate::overrides::OVERRIDE_PART) {
            return true;
        }
        !unit.items.is_empty() && unit.items.iter().all(|item| old(item) && new(item))
    }

    /// The target for a file of the old tree or of the new tree.
    fn file_target(&self, file: &str) -> Target {
        let mut scope = BTreeSet::new();
        for ws in [self.before, self.after] {
            scope.extend(ws.items.values().filter(|item| item.file == file).map(|item| item.path.clone()));
        }
        let one_side = match (self.before.files.contains_key(file), self.after.files.contains_key(file)) {
            (false, true) => Some("added"),
            (true, false) => Some("removed"),
            _ => None,
        };
        Target { name: file.to_string(), scope, file: Some(file.to_string()), one_side, old: Vec::new(), mark: None }
    }

    /// The file of a target that Semilla does not parse (design 54.20). The first segment of
    /// the path is the name of the target: the path names the file, because the file has no
    /// item in the syntax tree. `None`: no such target has the name.
    fn target_file(&self, query: &str, name: &str) -> Option<Result<Target, Vec<String>>> {
        let first = name.split("::").next()?.trim();
        let mut files: BTreeMap<&str, &semilla_syntax::CodeFile> = BTreeMap::new();
        for ws in [self.before, self.after] {
            for (file, code) in &ws.code_files {
                if !code.is_build_script() && code.target.replace('-', "_") == first {
                    files.insert(file, code);
                }
            }
        }
        // A target with two files or more: the path names the one file that the step changes.
        let changed: Vec<&str> = files.keys().copied().filter(|file| self.units.iter().any(|unit| unit.file.as_deref() == Some(*file))).collect();
        let (file, code) = match (files.len(), &changed[..]) {
            (0, _) => return None,
            (1, _) => files.iter().next()?,
            (_, [file]) => files.get_key_value(file)?,
            (count, _) => {
                let role = files.values().next()?.role();
                let list: Vec<String> = files.keys().map(|file| file.to_string()).collect();
                return Some(Err(vec![
                    format!("`{query}` starts with the name of {role}. Semilla does not parse the {count} files of this target, and the step changes {} of them: give the path of the file", changed.len()),
                    format!("candidates: {}", crate::paths::list(&list)),
                ]));
            }
        };
        let mut target = self.file_target(file);
        target.mark = Some(format!("Semilla does not parse `{file}`, a file of {}: the fact covers the full file", code.role()));
        Some(Ok(target))
    }

    /// Resolves the loose path of a `change` item. The error has the lines for the agent.
    fn resolve(&self, query: &str) -> Result<Target, Vec<String>> {
        let name = plain(query);
        let file = name.strip_prefix("./").unwrap_or(&name);
        // A loose file path: the last parts of one file path of the two trees.
        let files: BTreeSet<&String> = if name.ends_with(".rs") || name.ends_with(".toml") || name.contains('/') {
            self.before.files.keys().chain(self.after.files.keys()).filter(|path| path.as_str() == file || path.ends_with(&format!("/{file}"))).collect()
        } else {
            BTreeSet::new()
        };
        if files.len() > 1 && !files.iter().any(|path| path.as_str() == file) {
            let mut lines = vec![format!("`{query}` names {} files: give more of the path", files.len())];
            lines.extend(files.iter().map(|path| format!("candidate: `{path}`")));
            return Err(lines);
        }
        let file = files.iter().find(|path| path.as_str() == file).or(files.iter().next()).map(|path| path.as_str()).unwrap_or(file);
        if self.before.files.contains_key(file) || self.after.files.contains_key(file) {
            return Ok(self.file_target(file));
        }
        // Each item of the two trees. An item with a rename or a move is one item with two paths.
        let new_of = |old: &str| self.renames.items.get(old).cloned();
        let mut found: Vec<(Option<String>, Option<String>)> = Vec::new();
        fn add(found: &mut Vec<(Option<String>, Option<String>)>, pair: (Option<String>, Option<String>)) {
            if !found.contains(&pair) {
                found.push(pair);
            }
        }
        // The exact path in one of the two trees, else each item that agrees part by part
        // (design 54.10).
        let exact = |ws: &Workspace| ws.items.get(&name).is_some_and(|item| item.kind != ItemKind::Use);
        let (old_found, new_found): (Vec<&str>, Vec<&str>) = if exact(self.before) || exact(self.after) {
            (self.before.items.get_key_value(&name).map(|(path, _)| path.as_str()).into_iter().collect(), self.after.items.get_key_value(&name).map(|(path, _)| path.as_str()).into_iter().collect())
        } else {
            (self.old_paths().find(&name), self.new_paths().find(&name))
        };
        for path in old_found {
            let new = new_of(path).or_else(|| self.after.items.contains_key(path).then(|| path.to_string()));
            add(&mut found, (Some(path.to_string()), new));
        }
        for path in new_found {
            let old = self.renames.items.iter().find(|(_, new)| new.as_str() == path).map(|(old, _)| old.clone()).or_else(|| self.before.items.contains_key(path).then(|| path.to_string()));
            add(&mut found, (old, Some(path.to_string())));
        }
        // A candidate shows the generic arguments of its `impl` block: a twin number alone
        // (`#0`, `#1`) does not tell which block it is.
        let label = |(old, new): &(Option<String>, Option<String>)| match (old, new) {
            (Some(old), Some(new)) if old != new => format!("`{}` (before the step: `{}`)", self.new_paths().label(new), self.old_paths().label(old)),
            (_, Some(new)) => format!("`{}`", self.new_paths().label(new)),
            (Some(old), None) => format!("`{}` (removed)", self.old_paths().label(old)),
            (None, None) => String::new(),
        };
        let same_form = || {
            let form = |(old, new): &(Option<String>, Option<String>)| new.as_ref().and_then(|path| self.new_paths().normal(path)).or_else(|| old.as_ref().and_then(|path| self.old_paths().normal(path)));
            let first = form(&found[0]);
            first.is_some() && found.iter().all(|pair| form(pair) == first)
        };
        match found.len() {
            0 => {
                if let Some(target) = self.target_file(query, &name) {
                    return target;
                }
                let mut lines = vec![format!("`{query}` names no item, no module, and no file of the old tree or the new tree")];
                lines.extend(candidate_lines(&name, &[self.new_paths(), self.old_paths()]));
                Err(lines)
            }
            // One item. Or items that a path cannot tell apart (design 54.19): each
            // candidate has the same path with no twin mark. The path names each of them, as
            // a module names each item in it.
            count if count == 1 || same_form() => {
                let mut scope = BTreeSet::new();
                for (ws, path) in found.iter().flat_map(|(old, new)| [(self.before, old), (self.after, new)]) {
                    let Some(root) = path.as_ref().and_then(|path| ws.items.get(path)) else { continue };
                    for item in ws.items.values() {
                        let inside = item.path == root.path
                            || [("::"), ("."), ("#")].iter().any(|sep| item.path.starts_with(&format!("{}{sep}", root.path)))
                            || (matches!(root.kind, ItemKind::Impl | ItemKind::Mod | ItemKind::Trait) && item.file == root.file && item.line_start >= root.line_start && item.line_end <= root.line_end);
                        if inside {
                            scope.insert(item.path.clone());
                        }
                    }
                }
                let one_side = if found.iter().all(|(old, _)| old.is_none()) {
                    Some("added")
                } else if found.iter().all(|(_, new)| new.is_none()) {
                    Some("removed")
                } else {
                    None
                };
                let (old, new) = &found[0];
                let name = new.clone().or(old.clone()).unwrap_or_default();
                let name = if count == 1 { name } else { crate::paths::no_twin(&name) };
                Ok(Target { name, scope, file: None, one_side, old: found.iter().filter_map(|(old, _)| old.clone()).collect(), mark: (count > 1).then(|| format!("{count} items with this path")) })
            }
            count => {
                // The best candidate first: the item with the most parts that agree.
                let rank = |(old, new): &(Option<String>, Option<String>)| new.as_ref().map(|path| self.new_paths().rank(&name, path)).or_else(|| old.as_ref().map(|path| self.old_paths().rank(&name, path))).unwrap_or(0);
                found.sort_by_key(|pair| std::cmp::Reverse(rank(pair)));
                let mut list = found.iter().take(8).map(label).collect::<Vec<_>>().join(", ");
                if count > 8 {
                    list.push_str(&format!(", and {} more", count - 8));
                }
                Err(vec![format!("`{query}` names {count} items: give more of the path"), format!("candidates: {list}")])
            }
        }
    }

    /// The units of a target.
    fn units_of(&self, target: &Target) -> Vec<usize> {
        (0..self.units.len())
            .filter(|&index| {
                let unit = &self.units[index];
                unit.items.iter().any(|item| target.scope.contains(item)) || (target.file.is_some() && unit.file == target.file)
            })
            .collect()
    }

    /// The short text of a fact for a message.
    fn fact_text(&self, fact: usize) -> String {
        let fact = &self.facts[fact];
        format!("{} {} [{}]", fact.id, fact.description, fact.category.name())
    }

    /// The lines for a path of a `rename` or a `move` that does not name one item of a tree.
    fn path_lines(&self, query: &str, found: &[&str], tree: &Index, side: &str) -> Vec<String> {
        match found.len() {
            1 => Vec::new(),
            0 => {
                let mut lines = vec![format!("`{query}` names no item of the {side} tree")];
                lines.extend(candidate_lines(query, &[tree]));
                lines
            }
            count => {
                let labels: Vec<String> = found.iter().map(|path| tree.label(path)).collect();
                vec![format!("`{query}` names {count} items of the {side} tree: give more of the path"), format!("candidates: {}", crate::paths::list(&labels))]
            }
        }
    }

    /// The check of a `rename` or a `move` claim: the fact must exist. Semilla derives it
    /// alone, or it is a stated pair that passes the check of the rename rule (design 54.11).
    fn identity(&self, option: usize, category: Category, old: &str, new: &str, result: &mut AgentFact) -> Vec<usize> {
        let word = if category == Category::Rename { "rename" } else { "move" };
        let parent = |path: &str| path.rsplit_once("::").map(|(module, _)| module.to_string()).unwrap_or_default();
        // The items that the two paths name, part by part (design 54.10).
        let (olds, news) = (self.old_paths().find(old), self.new_paths().find(new));
        let old_is = |path: &str| olds.contains(&path) || loose(path, old);
        let new_is = |path: &str| news.contains(&path) || loose(path, new);
        let holds = |fact: &Fact| fact.category == category && pairs(&fact.description).iter().any(|(a, b)| old_is(a) && (new_is(b) || (category == Category::Move && new_is(&parent(b)))));
        let found: Vec<usize> = (0..self.facts.len()).filter(|&index| holds(&self.facts[index])).collect();
        if found.is_empty() {
            result.result = format!("false: Semilla derives no {word} `{old}` → `{new}` from the syntax tree");
            result.notes.extend(self.path_lines(old, &olds, self.old_paths(), "old"));
            // The new path of a move can be the new module.
            if !(category == Category::Move && self.after.crates.iter().any(|krate| krate.name == plain(new))) {
                result.notes.extend(self.path_lines(new, &news, self.new_paths(), "new"));
            }
            if let Some(cause) = self.stated_causes.get(&option) {
                result.notes.push(format!("the stated pair is no fact: {cause}"));
            }
            // Design 54.16: an identity fact of one of the two items is a fact that holds.
            let mut near: Vec<String> = Vec::new();
            for fact in self.facts.iter().filter(|fact| matches!(fact.category, Category::Rename | Category::Move)) {
                for (a, b) in pairs(&fact.description).into_iter().filter(|(a, b)| old_is(a) || new_is(b)) {
                    let kind = if fact.category == Category::Rename { "rename" } else { "move" };
                    let sides = if a.contains(' ') || b.contains(' ') { format!("{a} => {b}") } else { format!("{a} {b}") };
                    let line = format!("a fact that holds: `--same {}`", shell_quote(&format!("{kind} {sides}")));
                    if !near.contains(&line) {
                        near.push(line);
                    }
                }
            }
            result.notes.extend(near.into_iter().take(3));
            // The cause: the facts that Semilla derives for the two names.
            let names = |fact: &Fact| quoted(&fact.description).iter().any(|text| old_is(text) || new_is(text)) || fact.at.as_ref().is_some_and(|at| old_is(&at.item) || new_is(&at.item));
            let seen: Vec<usize> = (0..self.facts.len()).filter(|&index| names(&self.facts[index])).collect();
            for &fact in seen.iter().take(6) {
                result.notes.push(format!("Semilla sees: {}", self.fact_text(fact)));
            }
            if seen.len() > 6 {
                result.notes.push(format!("and {} more fact(s) with one of the two names", seen.len() - 6));
            }
            if seen.is_empty() {
                let side = |tree: &Index, query: &str, word: &str| if !tree.find(query).is_empty() { format!("the {word} tree has `{query}`") } else { format!("the {word} tree has no item `{query}`") };
                result.notes.push(format!("Semilla sees no fact with one of the two names: {}, {}, {}, {}", side(self.old_paths(), old, "old"), side(self.new_paths(), old, "new"), side(self.old_paths(), new, "old"), side(self.new_paths(), new, "new")));
            }
            return Vec::new();
        }
        result.holds = true;
        result.result = format!("holds: Semilla derives the {word} from the syntax tree");
        // The fact, and the note of its public API change: the note names only the paths of
        // the pair (design 31.8), so it is text about the same identity fact.
        let names: BTreeSet<String> = found.iter().flat_map(|&index| pairs(&self.facts[index].description)).flat_map(|(a, b)| [a, b]).collect();
        let note = |fact: &Fact| fact.is_api_note() && {
            // The text in parentheses at the end is advice, not a path.
            let paths = quoted(fact.description.split(" (").next().unwrap_or_default());
            !paths.is_empty() && paths.iter().all(|path| names.contains(*path))
        };
        (0..self.units.len()).filter(|&index| found.contains(&self.units[index].fact) || note(&self.facts[self.units[index].fact])).collect()
    }

    /// The nodes of the two sides of an edit, at the edit or at `up` levels above it.
    fn nodes(&self, edit: &Edit, up: usize) -> Option<(Anchor, Anchor, &Node, &Node)> {
        let (mut old, mut new) = (edit.before.clone()?, edit.after.clone()?);
        for _ in 0..up {
            if old.path.len() <= 1 || new.path.len() <= 1 {
                return None;
            }
            (old, new) = (old.parent()?, new.parent()?);
        }
        if old.path.is_empty() || new.path.is_empty() {
            return None;
        }
        let (a, b) = (semilla_syntax::resolve_node(self.before, &old)?, semilla_syntax::resolve_node(self.after, &new)?);
        Some((old, new, a, b))
    }

    /// The sites of a pattern: for each edit, the lowest node at the edit or above it that the
    /// pattern covers, with the count of the sites in it.
    fn sites(&self, pattern: &Pattern) -> Vec<(Anchor, Anchor, usize)> {
        let mut sites: Vec<(Anchor, Anchor, usize)> = Vec::new();
        // A site is an edit in place. A statement with a new position has a difference that the
        // pattern does not state (the position), so a node with a moved statement is not a site.
        let moved = |old: &Anchor| self.edits.iter().any(|edit| edit.kind == semilla_syntax::EditKind::Moved && edit.before.as_ref().is_some_and(|at| old.contains(at)));
        for edit in self.edits {
            if edit.before.as_ref().is_some_and(|at| sites.iter().any(|(old, _, _)| old.contains(at))) {
                continue;
            }
            for up in 0..4 {
                let Some((old, new, a, b)) = self.nodes(edit, up) else { break };
                if moved(&old) {
                    break;
                }
                let mut count = 0;
                if pattern.covers(a, b, &mut count) && count > 0 {
                    sites.push((old, new, count));
                    break;
                }
            }
        }
        sites
    }

    /// The line of a pattern that holds near a false pattern (design 54.16): a complete
    /// option, with the count of its sites. `None` if the pattern does not hold, if it is the
    /// false pattern, or if its text is long.
    fn near_option(&self, from: &str, to: &str, stated: (&str, &str)) -> Option<String> {
        if (from, to) == stated || from.len() + to.len() > 200 {
            return None;
        }
        let pattern = Pattern::parse(from, to).ok()?;
        let count: usize = self.sites(&pattern).iter().map(|(_, _, count)| count).sum();
        (count > 0).then(|| format!("a fact that holds: `--same {}` ({count} site(s))", shell_quote(&format!("rewrite {from} => {to}"))))
    }

    /// The check of a `rewrite` claim (design 54.3).
    fn rewrite(&self, from: &str, to: &str, result: &mut AgentFact) -> Vec<usize> {
        let pattern = match Pattern::parse(from, to) {
            Ok(pattern) => pattern,
            Err(error) => {
                result.result = format!("false: {error}");
                return Vec::new();
            }
        };
        let sites = self.sites(&pattern);
        let count: usize = sites.iter().map(|(_, _, count)| count).sum();
        if count == 0 {
            result.result = format!("false: no changed place has the old form `{from}` and the new form `{to}`");
            self.rewrite_cause(&pattern, from, to, result);
            return Vec::new();
        }
        result.holds = true;
        // The words say what Semilla checks (design 54.12): the form of the edit only.
        result.result = format!("the form holds: {count} site(s) have exactly this edit. Semilla does not check that the behavior is the same");
        // Each edit in a site: the pattern is the full difference of the site.
        let inside = |edit: &Edit| sites.iter().any(|(old, new, _)| edit.before.as_ref().is_some_and(|at| old.contains(at)) || edit.after.as_ref().is_some_and(|at| new.contains(at)));
        let covered: Vec<usize> = (0..self.units.len()).filter(|&index| self.units[index].edit.is_some_and(|edit| inside(&self.edits[edit]))).collect();
        // A site whose edit is below a claim of the step has no fact: the claim shows it.
        let of_claim = sites.iter().filter(|(old, new, _)| !covered.iter().any(|&index| self.units[index].edit.is_some_and(|edit| self.edits[edit].before.as_ref().is_some_and(|at| old.contains(at)) || self.edits[edit].after.as_ref().is_some_and(|at| new.contains(at))))).count();
        if of_claim > 0 {
            result.notes.push(format!("    {of_claim} of the site(s) are below a claim of the step: the claim shows them"));
        }
        covered
    }

    /// The cause of a `rewrite` claim with no site: the nearest edits that do not match.
    fn rewrite_cause(&self, pattern: &Pattern, from: &str, to: &str, result: &mut AgentFact) {
        let has = |side: &Node, root: &Node| {
            let mut found = false;
            root.walk(&mut Vec::new(), &mut |_, node| found |= bind(side, node, &mut BTreeMap::new()));
            found
        };
        // Design 54.16: a plain name of the pattern matches only that name. If the pattern with
        // a variable in place of the name holds, the line gives it as a complete option.
        let mut options: Vec<String> = Vec::new();
        let mut plain = Vec::new();
        plain_names(&pattern.from, &mut plain);
        plain.truncate(4);
        let mut sets: Vec<Vec<&String>> = if plain.len() > 1 { vec![plain.iter().collect()] } else { Vec::new() };
        sets.extend(plain.iter().map(|name| vec![name]));
        for set in sets {
            let side = |text: &str| set.iter().fold(text.to_string(), |text, name| with_variable(&text, name));
            let (new_from, new_to) = (side(from), side(to));
            let Ok(variant) = Pattern::parse(&new_from, &new_to) else { continue };
            let list = set.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
            let variables = set.iter().map(|name| format!("`${name}`")).collect::<Vec<_>>().join(", ");
            let hint = format!("a name in a pattern matches only that name ({list}). For a variable, write {variables}");
            if let Some(option) = self.near_option(&new_from, &new_to, (from, to)) {
                result.notes.push(hint);
                result.notes.push(option.clone());
                options.push(option);
                break;
            }
            // The pattern with the variable has the old form at an edit, and the stated pattern
            // does not: the name is the cause.
            let at_edit = |side: &Node| self.edits.iter().any(|edit| self.nodes(edit, 0).is_some_and(|(_, _, a, _)| has(side, a)));
            if set.len() == 1 && at_edit(&variant.from) && !at_edit(&pattern.from) {
                result.notes.push(hint);
                break;
            }
        }
        let mut near = 0;
        for (index, edit) in self.edits.iter().enumerate() {
            let Some((old_at, new_at, a, b)) = self.nodes(edit, 0) else { continue };
            let cause = match (has(&pattern.from, a), has(&pattern.to, b)) {
                (true, true) => format!("the old code has the form `{from}` and the new code has the form `{to}`, but the edit changes more than the pattern"),
                (true, false) => format!("the old code has the form `{from}`, but the new code does not have the form `{to}`"),
                (false, true) => format!("the new code has the form `{to}`, but the old code does not have the form `{from}`"),
                (false, false) => continue,
            };
            near += 1;
            if near <= 3 {
                let fact = self.units.iter().find(|unit| unit.edit == Some(index)).map(|unit| format!(" ({})", self.facts[unit.fact].id)).unwrap_or_default();
                result.notes.push(format!("nearest edit at `{new_at}`{fact}: {cause}"));
                result.notes.push(format!("    - {}", semilla_syntax::tree::one_line(edit.source_before.as_deref().unwrap_or(&a.text))));
                result.notes.push(format!("    + {}", semilla_syntax::tree::one_line(edit.source_after.as_deref().unwrap_or(&b.text))));
                // The pattern of the near edit, if it holds (design 54.16).
                let file = |ws: &'_ Workspace, at: &Anchor| ws.items.get(&at.item).and_then(|item| ws.rust_files.get(&item.file)).cloned();
                let option = file(self.before, &old_at)
                    .zip(file(self.after, &new_at))
                    .and_then(|(old_file, new_file)| generalize(a, b, &old_file, &new_file))
                    .and_then(|(near_from, near_to)| self.near_option(&near_from, &near_to, (from, to)));
                if let Some(option) = option.filter(|option| !options.contains(option)) {
                    result.notes.push(format!("    {option}"));
                    options.push(option);
                }
            }
        }
        if near > 3 {
            result.notes.push(format!("and {} more edit(s) that are near the pattern", near - 3));
        }
        if near == 0 {
            // The old form at places with no edit.
            let mut count = 0;
            for item in self.before.items.values() {
                if let Some(body) = &item.body {
                    body.walk(&mut Vec::new(), &mut |_, node| count += usize::from(bind(&pattern.from, node, &mut BTreeMap::new())));
                }
            }
            result.notes.push(format!("no edit of the step has the old form or the new form; the old tree has the form `{from}` at {count} place(s)"));
        }
    }

    /// The check of a `change` fact (design 54.2): the tag must agree with the edit that
    /// Semilla sees in each item.
    fn change(&self, items: &[String], result: &mut AgentFact) -> Vec<usize> {
        let mut covered = Vec::new();
        let mut held = 0;
        // The marks of the paths: items that a path cannot tell apart (design 54.19), and a
        // file that Semilla does not parse (design 54.20).
        let mut marks: Vec<String> = Vec::new();
        for query in items {
            let target = match self.resolve(query) {
                Ok(target) => target,
                Err(lines) => {
                    result.notes.extend(lines);
                    continue;
                }
            };
            if let Some(mark) = &target.mark {
                marks.push(if items.len() == 1 { mark.clone() } else { format!("`{}`: {mark}", plain(query)) });
            }
            let units = self.units_of(&target);
            let behavior: Vec<usize> = units.iter().copied().filter(|&unit| self.facts[self.units[unit].fact].category.can_change_behavior()).collect();
            let sided = |category: Category| units.iter().any(|&unit| self.facts[self.units[unit].fact].category == category);
            if !behavior.is_empty() {
                held += 1;
                covered.extend(behavior);
            } else if let Some(side) = target.one_side.or(sided(Category::Added).then_some("added")).or(sided(Category::Removed).then_some("removed")) {
                // No reject: the code is new, or it is gone. It is not existing code.
                held += 1;
                result.notes.push(format!("note: Semilla shows `{}` as {side}: the fact covers no existing code", target.name));
                // [R92.5-1] The item is a place of the fact: the fact names the entry of the
                // addition or the removal (design 90, R90.5-2).
                if !result.places.contains(&target.name) {
                    result.places.push(target.name.clone());
                }
            } else {
                let mut facts: Vec<usize> = units.iter().map(|&unit| self.units[unit].fact).collect();
                facts.dedup();
                let sites: usize = target.old.iter().filter_map(|old| self.renames.sites.as_ref().and_then(|sites| sites.get(old))).map(|sites| sites.len()).sum();
                if facts.is_empty() && sites == 0 {
                    result.notes.push(format!("Semilla sees no edit in `{}`", target.name));
                } else {
                    result.notes.push(format!("Semilla sees only edits that keep the behavior in `{}`:", target.name));
                    if sites > 0 {
                        result.notes.push(format!("    {sites} site(s) of a rename (the name changes, the item that the name gives does not)"));
                    }
                    for fact in facts.iter().take(6) {
                        result.notes.push(format!("    {}", self.fact_text(*fact)));
                    }
                    if facts.len() > 6 {
                        result.notes.push(format!("    and {} more fact(s)", facts.len() - 6));
                    }
                }
            }
        }
        result.holds = held == items.len();
        result.result = if result.holds {
            "holds: the behavior of existing code can change in each item".to_string()
        } else if items.len() == 1 {
            "false: the tag `change` does not agree with the edit that Semilla sees".to_string()
        } else {
            format!("false: the tag `change` does not agree with the edit that Semilla sees in {} of {} items", items.len() - held, items.len())
        };
        if covered.is_empty() && result.holds {
            result.result = "holds: no reject".to_string();
        }
        if !marks.is_empty() {
            result.result.push_str(&format!(" ({})", marks.join("; ")));
        }
        covered
    }
}

/// The renames and the moves that the agent states, as candidate pairs of the rename rule
/// (design 54.11). Each pair has the index of its `same` option. A pair with a form that the
/// rename rule does not have is no candidate: the map has its cause. A path that does not
/// name one item gives no pair and no cause here: the check of the fact gives the candidates.
pub(crate) fn stated_pairs(input: &StepInput) -> (Vec<(usize, Candidate)>, BTreeMap<usize, String>) {
    use crate::identity::{kind_name, name, owner, pair_kind};
    let mut pairs: Vec<(usize, Candidate)> = Vec::new();
    let mut causes = BTreeMap::new();
    let mut trees: Option<(Index, Index)> = None;
    for (option, text) in input.step.agent.same.iter().enumerate() {
        let (op, old, new) = match parse_same(text) {
            Ok(Same::Rename(old, new)) => ("rename", old, new),
            Ok(Same::Move(old, new)) => ("move-item", old, new),
            _ => continue,
        };
        let (old_tree, new_tree) = trees.get_or_insert_with(|| (Index::new(input.before), Index::new(input.after)));
        let &[old_path] = &old_tree.find(&old)[..] else { continue };
        // The new path of a move can be the new module.
        let module = |path: &str| input.after.items.get(path).is_some_and(|item| item.kind == ItemKind::Mod);
        let new_path = match &new_tree.find(&new)[..] {
            &[path] if op == "move-item" && module(path) && name(path) != name(old_path) => format!("{path}::{}", name(old_path)),
            &[path] => path.to_string(),
            [] if op == "move-item" && input.after.crates.iter().any(|krate| krate.name == plain(&new)) => format!("{}::{}", plain(&new), name(old_path)),
            _ => continue,
        };
        let Some(old_item) = input.before.items.get(old_path) else { continue };
        let Some(new_item) = input.after.items.get(&new_path) else {
            causes.insert(option, format!("the new tree has no item `{new_path}`"));
            continue;
        };
        let cause = if let Some((_, other)) = pairs.iter().find(|(_, other)| (other.old == old_path) != (other.new == new_path)) {
            Some(format!("a different fact states `{}` → `{}`, and an item is in one pair only", other.old, other.new))
        } else if input.after.items.contains_key(old_path) {
            Some(format!("the new tree has the item `{old_path}` too"))
        } else if input.before.items.contains_key(&new_path) {
            Some(format!("the old tree has the item `{new_path}` too"))
        } else if old_item.kind != new_item.kind {
            Some(format!("`{old_path}` is a {} and `{new_path}` is a {}", kind_name(old_item.kind), kind_name(new_item.kind)))
        } else if !pair_kind(old_item.kind) || old_path.contains('<') || new_path.contains('<') {
            Some(format!("the rename rule has no pair for an `impl` block, a method of a trait impl, a macro, or a `use` line (`{old_path}`): state the rename or the move of the type, the trait, or the trait method"))
        } else if op == "rename" && name(old_path) == name(&new_path) {
            Some(format!("the two items have the name `{}`: a rename gives a new name. For a new module, write `--same 'move {old} {new}'`", name(old_path)))
        } else if op == "move-item" && name(old_path) != name(&new_path) {
            Some(format!("a move keeps the name (`{}` is not `{}`). For a new name in the same owner, write `--same 'rename {old} {new}'`", name(old_path), name(&new_path)))
        } else if op == "move-item" && (owner(old_path) == owner(&new_path) || matches!(old_item.kind, ItemKind::Field | ItemKind::Variant)) {
            Some("a move gives a new module, and a field or a variant has no move".to_string())
        } else {
            None
        };
        match cause {
            Some(cause) => {
                causes.insert(option, cause);
            }
            None if pairs.iter().any(|(_, other)| other.old == old_path) => {}
            None => pairs.push((option, Candidate { old: old_path.to_string(), new: new_path, op })),
        }
    }
    (pairs, causes)
}

/// The text of a gap line with the edits of one item that are not equal.
const DIFFERENT: &str = "different edits";

/// The largest count of characters of the text of a gap line (design 54.15).
const GAP_TEXT_MAX: usize = 80;

/// True for a file of a bench.
fn bench_file(file: &str) -> bool {
    file.split('/').any(|part| part == "benches")
}

/// Checks the facts of the agent of a step, sets the origin of each fact, and makes the gap
/// list (design 54). The pass is the last pass of a step check.
pub(crate) fn apply(input: &StepInput, outcome: &mut StepOutcome) {
    let StepOutcome { report, edits, fact_of, renames, stated_causes, explained, .. } = outcome;
    let explained = &*explained;
    // The units: each edit of a fact, and the place of each fact with no edit.
    let mut units: Vec<Unit> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let Some(fact) = fact_of.get(index).copied().flatten().filter(|fact| *fact < report.facts.len()) else { continue };
        let mut items: Vec<String> = edit.before.iter().chain(&edit.after).map(|at| at.item.clone()).collect();
        items.dedup();
        let place = edit.after.as_ref().or(edit.before.as_ref()).map(|at| at.to_string()).or_else(|| edit.file.clone()).unwrap_or_default();
        units.push(Unit { fact, items, file: edit.file.clone(), place, edit: Some(index) });
    }
    for (index, fact) in report.facts.iter().enumerate() {
        if units.iter().any(|unit| unit.fact == index) {
            continue;
        }
        let place = fact.at.as_ref().map(|at| at.to_string()).or_else(|| fact.file.clone()).unwrap_or_else(|| fact.part.clone());
        units.push(Unit { fact: index, items: fact.at.iter().map(|at| at.item.clone()).collect(), file: fact.file.clone(), place, edit: None });
    }
    let facts = std::mem::take(&mut report.facts);
    let check = Check { before: input.before, after: input.after, edits, facts: &facts, renames, units, old_paths: Default::default(), new_paths: Default::default(), stated_causes };

    // The agent facts. Two `change` facts with one item set are one fact with two sentences.
    let mut results: Vec<(AgentFact, Vec<usize>)> = Vec::new();
    for (option, text) in input.step.agent.same.iter().enumerate() {
        let mut result = AgentFact { kind: "same".into(), text: text.trim().to_string(), options: vec![text.clone()], ..Default::default() };
        let covered = match parse_same(text) {
            Ok(Same::Rename(old, new)) => check.identity(option, Category::Rename, &old, &new, &mut result),
            Ok(Same::Move(old, new)) => check.identity(option, Category::Move, &old, &new, &mut result),
            Ok(Same::Rewrite(from, to)) => {
                result.text = format!("rewrite `{from}` => `{to}`");
                check.rewrite(&from, &to, &mut result)
            }
            Err(error) => {
                result.result = format!("false: {error}");
                Vec::new()
            }
        };
        results.push((result, covered));
    }
    let mut item_sets: Vec<BTreeSet<String>> = Vec::new();
    let first_change = results.len();
    for text in &input.step.agent.change {
        let (items, sentence) = match parse_change(text) {
            Ok(parsed) => parsed,
            Err(error) => {
                results.push((AgentFact { kind: "change".into(), text: text.clone(), result: format!("false: {error}"), options: vec![text.clone()], ..Default::default() }, Vec::new()));
                item_sets.push(BTreeSet::new());
                continue;
            }
        };
        // The key of the item set: the resolved names, so two forms of one path are one item.
        let key: BTreeSet<String> = items.iter().map(|item| check.resolve(item).map(|target| target.name).unwrap_or_else(|_| plain(item))).collect();
        if let Some(index) = item_sets.iter().position(|set| *set == key) {
            let fact = &mut results[first_change + index].0;
            fact.sentences.push(sentence);
            fact.options.push(text.clone());
            continue;
        }
        let mut result = AgentFact { kind: "change".into(), text: items.iter().map(|item| format!("`{}`", plain(item))).collect::<Vec<_>>().join(", "), sentences: vec![sentence], options: vec![text.clone()], ..Default::default() };
        let covered = check.change(&items, &mut result);
        results.push((result, covered));
        item_sets.push(key);
    }

    // The origin of each fact: `stated` if the facts of the agent cover each unit of it.
    let mut covered_by: Vec<Option<usize>> = vec![None; check.units.len()];
    for (index, (_, covered)) in results.iter().enumerate() {
        for &unit in covered {
            covered_by[unit].get_or_insert(index);
        }
    }
    let mut status: Vec<&str> = vec!["derived"; facts.len()];
    for (index, fact) in facts.iter().enumerate() {
        let own: Vec<usize> = (0..check.units.len()).filter(|&unit| check.units[unit].fact == index).collect();
        let stated = !own.is_empty() && own.iter().all(|&unit| covered_by[unit].is_some());
        if stated {
            status[index] = "stated";
        }
        for (agent, (result, covered)) in results.iter_mut().enumerate() {
            let count = own.iter().filter(|unit| covered.contains(unit)).count();
            if count == 0 {
                continue;
            }
            if stated && own.iter().find_map(|&unit| covered_by[unit]) == Some(agent) {
                result.facts.push(fact.id.clone());
            } else if stated {
                result.partial.push(format!("{} (below a different fact of the agent)", fact.id));
            } else {
                result.partial.push(format!("{} ({count} of {} places)", fact.id, own.len()));
            }
        }
    }
    for (index, (result, covered)) in results.iter_mut().enumerate() {
        result.id = format!("a{}", index + 1);
        for &unit in covered.iter() {
            let place = &check.units[unit].place;
            if !result.places.contains(place) {
                result.places.push(place.clone());
            }
        }
    }

    // The gap list: each unit of existing code that can change the behavior, that is not in a
    // test or a bench, and that no fact of the agent covers.
    struct Line {
        pattern: Option<(String, String)>,
        text: String,
        /// The full old form and the full new form of the edit (the token text). Two edits
        /// with one fact text and different forms are not equal edits.
        form: (Option<String>, Option<String>),
        same_text: bool,
        units: Vec<usize>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for (index, unit) in check.units.iter().enumerate() {
        let fact = &facts[unit.fact];
        // [R77.1-3] A unit of an item whose changed bodies a proved claim explains has no line.
        if covered_by[index].is_some() || !fact.category.agent_states() || check.test_unit(unit) || !check.existing(unit) || unit.items.last().is_some_and(|item| explained.contains(item)) {
            continue;
        }
        // The edit without the place: the text of a fold, or the text of the fact.
        let text = match fact.sites.iter().find(|group| fact.folds(group)) {
            Some(group) => group.edit.clone(),
            None => fact.at.as_ref().and_then(|at| fact.description.strip_suffix(&format!(" at `{at}`"))).unwrap_or(&fact.description).to_string(),
        };
        let pattern = unit.edit.and_then(|edit| {
            let (old, new, a, b) = check.nodes(&check.edits[edit], 0)?;
            let file = |ws: &'_ Workspace, at: &Anchor| ws.items.get(&at.item).and_then(|item| ws.rust_files.get(&item.file)).cloned();
            generalize(a, b, &file(check.before, &old)?, &file(check.after, &new)?)
        });
        let form = unit.edit.map(|edit| (check.edits[edit].text_before.clone(), check.edits[edit].text_after.clone())).unwrap_or_default();
        let key = |line: &Line| match (&line.pattern, &pattern) {
            (Some(a), Some(b)) => a == b,
            (None, None) => line.text == text && line.form == form,
            _ => false,
        };
        match lines.iter_mut().find(|line| key(line)) {
            Some(line) => {
                line.same_text &= line.text == text;
                line.units.push(index);
            }
            None => lines.push(Line { pattern, text, form, same_text: true, units: vec![index] }),
        }
    }
    let mut gaps: Vec<Gap> = Vec::new();
    for line in lines {
        let mut gap = Gap::default();
        for &unit in &line.units {
            let unit = &check.units[unit];
            gap.places.push(unit.place.clone());
            let item = unit.items.last().cloned().unwrap_or_default();
            if !gap.items.contains(&item) {
                gap.items.push(item);
            }
            if !gap.facts.contains(&facts[unit.fact].id) {
                gap.facts.push(facts[unit.fact].id.clone());
            }
        }
        let quote = shell_quote;
        // R71.3-1: the concrete pattern of the edit, also for one place: the form of a line that
        // the check did not decide gives it ("if it does not change, --same 'rewrite …'").
        gap.same = line.pattern.as_ref().map(|(from, to)| format!("--same {}", quote(&format!("rewrite {from} => {to}"))));
        match line.pattern.filter(|_| line.units.len() > 1) {
            // One pattern covers each place of the group: the option that explains the most.
            Some((from, to)) => {
                gap.text = if line.same_text { line.text } else { format!("`{from}` => `{to}`") };
                gap.option = format!("--same {}", quote(&format!("rewrite {from} => {to}")));
            }
            None => {
                gap.text = line.text;
                gap.option = format!("--change {}", quote(&format!("{}=TEXT", gap.items.join(","))));
            }
        }
        // The gap line is one line: the fact below it shows the lines of a long text.
        gap.text = semilla_core::text::flat_text(&gap.text);
        // Design 54.15: a gap line stays short. A long text gives its kind (the words before
        // the code) and the reference of the fact with the full text.
        if gap.text.chars().count() > GAP_TEXT_MAX {
            let kind = gap.text.split('`').next().unwrap_or_default().trim().trim_end_matches(':').trim();
            gap.text = format!("{}, see {}", if kind.is_empty() { "edit" } else { kind }, gap.facts[0]);
        }
        // The edits of one item that are in no group of equal edits make one line: one
        // `change` fact of the item states them all.
        let single = |gap: &Gap| gap.places.len() == 1 && gap.option.starts_with("--change ");
        match gaps.iter_mut().find(|other| single(&gap) && other.items == gap.items && other.option == gap.option && (single(other) || other.text.ends_with(DIFFERENT))) {
            Some(other) => {
                other.places.extend(gap.places);
                other.facts.extend(gap.facts.into_iter().filter(|fact| !other.facts.contains(fact)).collect::<Vec<_>>());
                other.text = format!("{} {DIFFERENT}", other.places.len());
                // R71.3-1: the patterns of the edits of the line, each one time; an edit with
                // no pattern adds the general words.
                other.same = merge_patterns(other.same.take(), gap.same);
            }
            None => gaps.push(gap),
        }
    }
    // The largest group first. The order of the facts stays for two groups with one count.
    gaps.sort_by_key(|gap| std::cmp::Reverse(gap.places.len()));

    // The mark of a fact with each place in a test or in a bench.
    let tests: Vec<bool> = (0..facts.len())
        .map(|index| {
            let mut own = check.units.iter().filter(|unit| unit.fact == index).peekable();
            own.peek().is_some() && own.all(|unit| check.test_unit(unit))
        })
        .collect();
    let status: Vec<String> = status.iter().map(|text| text.to_string()).collect();
    drop(check);
    report.facts = facts;
    for (index, fact) in report.facts.iter_mut().enumerate() {
        fact.status = status[index].clone();
        fact.test = tests[index];
    }
    report.agent_facts = results.into_iter().map(|(result, _)| result).collect();
    // R71.3-1: a line whose facts each have a result of the compiled check, and no result
    // "different", is undecided: the check did not show a change of the behavior.
    for gap in &mut gaps {
        let decided_different = |line: &String| line.starts_with("different") || line.contains("; the body: different");
        gap.undecided = !gap.facts.is_empty()
            && gap.facts.iter().all(|id| report.flow.as_ref().and_then(|flow| flow.equivalence.get(id)).is_some_and(|lines| !lines.is_empty() && !lines.iter().any(decided_different)));
        if gap.option.starts_with("--same ") {
            gap.same = Some(gap.option.clone());
        }
        // A line that the check decided keeps its option: the pattern is only for the form
        // that keeps the behavior.
        if !gap.undecided && !gap.option.starts_with("--same ") {
            gap.same = None;
        }
    }
    report.gaps = gaps;
}

/// The words after the patterns of a gap line with an edit that has no pattern (R71.3-1).
pub(crate) const MORE_EDITS: &str = ", and a `--same` fact (rename, move, or rewrite) that explains each other edit";

/// The `--same` options of two gap lines that make one line (R71.3-1): each option one time,
/// in order. A side with no pattern adds the words [`MORE_EDITS`]. `None` if no side has a
/// pattern: the line has the general words.
fn merge_patterns(a: Option<String>, b: Option<String>) -> Option<String> {
    let mut options: Vec<String> = Vec::new();
    let mut more = false;
    for side in [a, b] {
        let Some(text) = side else {
            more = true;
            continue;
        };
        let list = match text.split_once(MORE_EDITS) {
            Some((list, _)) => {
                more = true;
                list.to_string()
            }
            None => text,
        };
        for (index, part) in list.split(" --same ").enumerate() {
            let option = if index == 0 { part.to_string() } else { format!("--same {part}") };
            if !options.contains(&option) {
                options.push(option);
            }
        }
    }
    if options.is_empty() {
        return None;
    }
    Some(format!("{}{}", options.join(" "), if more { MORE_EDITS } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // R71.3-1: the patterns of the edits of one gap line, each one time; an edit with no pattern
    // adds the general words. No pattern: the general words only.
    // rule+ R71.3-1
    #[test]
    fn a_gap_line_keeps_each_concrete_pattern_of_its_edits() {
        let a = Some("--same 'rewrite $a => $a + 1'".to_string());
        let b = Some("--same 'rewrite 2 => 3'".to_string());
        assert_eq!(merge_patterns(a.clone(), b.clone()).as_deref(), Some("--same 'rewrite $a => $a + 1' --same 'rewrite 2 => 3'"));
        assert_eq!(merge_patterns(merge_patterns(a.clone(), b.clone()), a.clone()).as_deref(), Some("--same 'rewrite $a => $a + 1' --same 'rewrite 2 => 3'"));
        assert_eq!(merge_patterns(a.clone(), None), Some(format!("--same 'rewrite $a => $a + 1'{MORE_EDITS}")));
    }

    // rule- R71.3-1
    #[test]
    fn a_gap_line_with_no_pattern_has_the_general_words() {
        assert_eq!(merge_patterns(None, None), None);
        let gap = semilla_core::report::Gap { items: vec!["shop::f".into()], option: "--change 'shop::f=TEXT'".into(), undecided: true, same: None, ..Default::default() };
        assert!(gap.suggestion().ends_with("if it does not change, a `--same` fact (rename, move, or rewrite) that explains the edit"), "{}", gap.suggestion());
        let gap = semilla_core::report::Gap { same: Some("--same 'rewrite 2 => 3'".into()), ..gap };
        assert!(gap.suggestion().ends_with("if it does not change, --same 'rewrite 2 => 3'"), "{}", gap.suggestion());
        // A line that the check decided keeps its option.
        let decided = semilla_core::report::Gap { undecided: false, ..gap };
        assert_eq!(decided.suggestion(), "--change 'shop::f=TEXT'");
    }

    #[test]
    fn the_forms_of_a_same_claim() {
        assert_eq!(parse_same("rename a::Old New"), Ok(Same::Rename("a::Old".into(), "New".into())));
        assert_eq!(parse_same("move <A as B>::f => m::<A as B>::f"), Ok(Same::Move("<A as B>::f".into(), "m::<A as B>::f".into())));
        assert_eq!(parse_same("rewrite $c.inner => $c.raw()"), Ok(Same::Rewrite("$c.inner".into(), "$c.raw()".into())));
        // The target of a path is a part of its word (design 56.1).
        assert_eq!(parse_same("rename depot (test \"list\")::old depot (test \"list\")::new"), Ok(Same::Rename("depot (test \"list\")::old".into(), "depot (test \"list\")::new".into())));
        // The negative cases: each one has a message for the agent.
        assert!(parse_same("rename depot (lib \"list\")::old new").unwrap_err().contains("rename OLD NEW"));
        assert!(parse_same("rename a").unwrap_err().contains("rename OLD NEW"));
        assert!(parse_same("extract f g").unwrap_err().contains("has no check yet"));
        assert!(parse_same("inline f").unwrap_err().contains("has no check yet"));
        assert!(parse_same("swap a b").unwrap_err().contains("is not a kind"));
        assert!(parse_same("rewrite $c.inner").unwrap_err().contains("FROM => TO"));
        assert!(parse_same("rewrite $c => $c.raw()").unwrap_err().contains("only a variable"));
        assert!(parse_same("rewrite $c.get() => $c.get($d)").unwrap_err().contains("`$d` is only in the new side"));
        assert!(parse_same("rewrite let x = => 1").unwrap_err().contains("is not an expression"));
    }

    #[test]
    fn the_form_of_a_change_fact() {
        assert_eq!(parse_change("a::f, <A<B, C> as T>::g = the limit is inclusive"), Ok((vec!["a::f".into(), "<A<B, C> as T>::g".into()], "the limit is inclusive".into())));
        assert!(parse_change("a::f").is_err());
        assert!(parse_change("a::f=  ").is_err());
        assert!(parse_change(",a::f=text").is_err());
    }

    #[test]
    fn a_loose_path_names_the_last_segments() {
        assert!(loose("demo::cart::Cart::total", "Cart::total"));
        assert!(loose("demo::cart::Cart::total", "crate::demo::cart::Cart::total"));
        assert!(loose("demo::cart::<Cart as Display>::fmt", "Cart::fmt"));
        assert!(loose("demo::sys#1::open", "sys::open"));
        assert!(!loose("demo::cart::Cart::total", "art::total"));
        assert!(!loose("demo::cart::Cart::total", "Cart"));
    }

    #[test]
    fn the_pairs_of_a_fact_text() {
        assert_eq!(pairs("renamed `a::b` → `a::c`, with 2 `use` line(s)"), vec![("a::b".to_string(), "a::c".to_string())]);
        assert!(pairs("removed `a::b` (a function)").is_empty());
    }

    #[test]
    fn a_pattern_covers_only_the_full_difference() {
        let node = |text: &str| semilla_syntax::tree::expr_text_node("x", text).unwrap();
        let pattern = Pattern::parse("$c.inner", "$c.raw()").unwrap();
        let mut sites = 0;
        assert!(pattern.covers(&node("cart.inner"), &node("cart.raw()"), &mut sites));
        assert_eq!(sites, 1);
        // One variable has one value at one site.
        assert!(!pattern.covers(&node("cart.inner"), &node("other.raw()"), &mut 0));
        // A site with one more edit is not covered.
        assert!(!pattern.covers(&node("a.inner + b.inner"), &node("a.raw() + b.raw() + 1"), &mut 0));
        assert!(!pattern.covers(&node("f(a.inner, 1)"), &node("f(a.raw(), 2)"), &mut 0));
        // Two sites in one edit, and a site in the value of a variable.
        let mut sites = 0;
        assert!(pattern.covers(&node("a.inner + b.inner"), &node("a.raw() + b.raw()"), &mut sites));
        assert_eq!(sites, 2);
        let mut sites = 0;
        assert!(pattern.covers(&node("a.inner.inner"), &node("a.raw().raw()"), &mut sites));
        assert_eq!(sites, 2);
        // Equal code is no site.
        assert!(!pattern.covers(&node("a.inner"), &node("a.inner"), &mut 0));
    }
}
