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
        let words: Vec<&str> = rest.split_whitespace().collect();
        (!arrow && words.len() == 2).then(|| (words[0].to_string(), words[1].to_string()))
    };
    match kind {
        "rename" => sides(false).map(|(old, new)| Same::Rename(old, new)).ok_or_else(|| format!("`--same '{text}'`: write `rename OLD NEW` (for a path with a space: `rename OLD => NEW`)")),
        "move" => sides(false).map(|(old, new)| Same::Move(old, new)).ok_or_else(|| format!("`--same '{text}'`: write `move FROM TO` (for a path with a space: `move FROM => TO`)")),
        "rewrite" => {
            let (from, to) = sides(true).ok_or_else(|| format!("`--same '{text}'`: write `rewrite FROM => TO`, with a space on each side of `=>`"))?;
            let pattern = Pattern::parse(&from, &to)?;
            drop(pattern);
            Ok(Same::Rewrite(from, to))
        }
        "extract" | "inline" => Err(format!("`--same '{text}'`: the kind `{kind}` {LATER}")),
        _ => Err(format!("`--same '{text}'`: `{kind}` is not a kind of a `same` fact; {SAME_FORMS}")),
    }
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
            if new_values.iter().all(|(name, new)| old_values.get(name).is_some_and(|old| old.text == new.text || self.covers(old, new, &mut inner))) {
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
    (node.kind == "path" && !node.text.contains(' ')).then(|| node.text.strip_prefix(crate::pattern::META)).flatten()
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
            Some(value) => value.text == node.text,
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
        return pattern.text == node.text;
    }
    pattern.children.iter().zip(&node.children).all(|(p, n)| p.role == n.role && bind(p, n, values))
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
    fn replace(node: &Node, root: bool, source: &str, start: usize, name: &dyn Fn(&str) -> Option<String>, out: &mut Vec<(usize, usize, String)>) {
        if let Some(name) = name(&node.text).filter(|_| !root && node.span.1 > node.span.0) {
            out.push((node.span.0.saturating_sub(start), node.span.1.saturating_sub(start), name));
            return;
        }
        node.children.iter().for_each(|child| replace(child, false, source, start, name, out));
    }
    let side = |node: &Node, text: &str| -> Option<String> {
        let mut changes = Vec::new();
        replace(node, true, text, node.span.0, &name, &mut changes);
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
    /// The path in the old tree, for the sites of a rename.
    old: Option<String>,
}

struct Check<'a> {
    before: &'a Workspace,
    after: &'a Workspace,
    edits: &'a [Edit],
    facts: &'a [Fact],
    renames: &'a semilla_syntax::Renames,
    units: Vec<Unit>,
}

impl Check<'_> {
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
        !unit.items.is_empty() && unit.items.iter().all(|item| old(item) && new(item))
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
            let mut scope = BTreeSet::new();
            for ws in [self.before, self.after] {
                scope.extend(ws.items.values().filter(|item| item.file == file).map(|item| item.path.clone()));
            }
            let one_side = match (self.before.files.contains_key(file), self.after.files.contains_key(file)) {
                (false, true) => Some("added"),
                (true, false) => Some("removed"),
                _ => None,
            };
            return Ok(Target { name: file.to_string(), scope, file: Some(file.to_string()), one_side, old: None });
        }
        // Each item of the two trees. An item with a rename or a move is one item with two paths.
        let new_of = |old: &str| self.renames.items.get(old).cloned();
        let mut found: Vec<(Option<String>, Option<String>)> = Vec::new();
        fn add(found: &mut Vec<(Option<String>, Option<String>)>, pair: (Option<String>, Option<String>)) {
            if !found.contains(&pair) {
                found.push(pair);
            }
        }
        let usable = |item: &semilla_syntax::Item| item.kind != ItemKind::Use;
        let matches = |exact: bool, path: &str| if exact { path == name } else { loose(path, &name) };
        for exact in [true, false] {
            for item in self.before.items.values().filter(|item| usable(item) && matches(exact, &item.path)) {
                let new = new_of(&item.path).or_else(|| self.after.items.contains_key(&item.path).then(|| item.path.clone()));
                add(&mut found, (Some(item.path.clone()), new));
            }
            for item in self.after.items.values().filter(|item| usable(item) && matches(exact, &item.path)) {
                let old = self.renames.items.iter().find(|(_, new)| **new == item.path).map(|(old, _)| old.clone()).or_else(|| self.before.items.contains_key(&item.path).then(|| item.path.clone()));
                add(&mut found, (old, Some(item.path.clone())));
            }
            if !found.is_empty() {
                break;
            }
        }
        let label = |(old, new): &(Option<String>, Option<String>)| match (old, new) {
            (Some(old), Some(new)) if old != new => format!("`{new}` (before the step: `{old}`)"),
            (_, Some(new)) => format!("`{new}`"),
            (Some(old), None) => format!("`{old}` (removed)"),
            (None, None) => String::new(),
        };
        match found.len() {
            0 => {
                // The candidates: each item with the same last name.
                let last = name.rsplit("::").next().unwrap_or(&name).to_string();
                let mut near: Vec<String> = Vec::new();
                for ws in [self.after, self.before] {
                    for item in ws.items.values().filter(|item| usable(item)) {
                        let own = item.path.rsplit("::").next().unwrap_or(&item.path);
                        if (own == last || own.rsplit('.').next() == Some(&last)) && !near.contains(&item.path) {
                            near.push(item.path.clone());
                        }
                    }
                }
                let mut lines = vec![format!("`{query}` names no item, no module, and no file of the old tree or the new tree")];
                if !near.is_empty() {
                    lines.push(format!("candidates: {}", near.iter().take(8).map(|path| format!("`{path}`")).collect::<Vec<_>>().join(", ")));
                }
                Err(lines)
            }
            1 => {
                let (old, new) = found.remove(0);
                let mut scope = BTreeSet::new();
                for (ws, path) in [(self.before, &old), (self.after, &new)] {
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
                let one_side = match (&old, &new) {
                    (None, _) => Some("added"),
                    (_, None) => Some("removed"),
                    _ => None,
                };
                Ok(Target { name: new.clone().or(old.clone()).unwrap_or_default(), scope, file: None, one_side, old })
            }
            _ => Err(vec![format!("`{query}` names {} items: give more of the path", found.len()), format!("candidates: {}", found.iter().map(label).collect::<Vec<_>>().join(", "))]),
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

    /// The check of a `rename` or a `move` claim: Semilla must derive the same fact.
    fn identity(&self, category: Category, old: &str, new: &str, result: &mut AgentFact) -> Vec<usize> {
        let word = if category == Category::Rename { "rename" } else { "move" };
        let parent = |path: &str| path.rsplit_once("::").map(|(module, _)| module.to_string()).unwrap_or_default();
        let holds = |fact: &Fact| fact.category == category && pairs(&fact.description).iter().any(|(a, b)| loose(a, old) && (loose(b, new) || (category == Category::Move && loose(&parent(b), new))));
        let found: Vec<usize> = (0..self.facts.len()).filter(|&index| holds(&self.facts[index])).collect();
        if found.is_empty() {
            result.result = format!("false: Semilla derives no {word} `{old}` → `{new}` from the syntax tree");
            // The cause: the facts that Semilla derives for the two names.
            let names = |fact: &Fact| quoted(&fact.description).iter().any(|text| loose(text, old) || loose(text, new)) || fact.at.as_ref().is_some_and(|at| loose(&at.item, old) || loose(&at.item, new));
            let seen: Vec<usize> = (0..self.facts.len()).filter(|&index| names(&self.facts[index])).collect();
            for &fact in seen.iter().take(6) {
                result.notes.push(format!("Semilla sees: {}", self.fact_text(fact)));
            }
            if seen.len() > 6 {
                result.notes.push(format!("and {} more fact(s) with one of the two names", seen.len() - 6));
            }
            if seen.is_empty() {
                let has = |ws: &Workspace, query: &str| ws.items.keys().any(|path| loose(path, query));
                let side = |ws: &Workspace, query: &str, tree: &str| if has(ws, query) { format!("the {tree} tree has `{query}`") } else { format!("the {tree} tree has no item `{query}`") };
                result.notes.push(format!("Semilla sees no fact with one of the two names: {}, {}, {}, {}", side(self.before, old, "old"), side(self.after, old, "new"), side(self.before, new, "old"), side(self.after, new, "new")));
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

    /// The check of a `rewrite` claim (design 54.3).
    fn rewrite(&self, from: &str, to: &str, result: &mut AgentFact) -> Vec<usize> {
        let pattern = match Pattern::parse(from, to) {
            Ok(pattern) => pattern,
            Err(error) => {
                result.result = format!("false: {error}");
                return Vec::new();
            }
        };
        // The sites: for each edit, the lowest node at the edit or above it that the pattern covers.
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
        let count: usize = sites.iter().map(|(_, _, count)| count).sum();
        if count == 0 {
            result.result = format!("false: no changed place has the old form `{from}` and the new form `{to}`");
            self.rewrite_cause(&pattern, from, to, result);
            return Vec::new();
        }
        result.holds = true;
        result.result = format!("holds: {count} site(s)");
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
        let mut near = 0;
        for (index, edit) in self.edits.iter().enumerate() {
            let Some((_, new_at, a, b)) = self.nodes(edit, 0) else { continue };
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
        for query in items {
            let target = match self.resolve(query) {
                Ok(target) => target,
                Err(lines) => {
                    result.notes.extend(lines);
                    continue;
                }
            };
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
            } else {
                let mut facts: Vec<usize> = units.iter().map(|&unit| self.units[unit].fact).collect();
                facts.dedup();
                let sites = target.old.as_ref().and_then(|old| self.renames.sites.as_ref().and_then(|sites| sites.get(old))).map(|sites| sites.len()).unwrap_or(0);
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
        covered
    }
}

/// The text of a gap line with the edits of one item that are not equal.
const DIFFERENT: &str = "different edits";

/// True for a file of a bench.
fn bench_file(file: &str) -> bool {
    file.split('/').any(|part| part == "benches")
}

/// Checks the facts of the agent of a step, sets the origin of each fact, and makes the gap
/// list (design 54). The pass is the last pass of a step check.
pub(crate) fn apply(input: &StepInput, outcome: &mut StepOutcome) {
    let StepOutcome { report, edits, fact_of, renames, .. } = outcome;
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
    let check = Check { before: input.before, after: input.after, edits, facts: &facts, renames, units };

    // The agent facts. Two `change` facts with one item set are one fact with two sentences.
    let mut results: Vec<(AgentFact, Vec<usize>)> = Vec::new();
    for text in &input.step.agent.same {
        let mut result = AgentFact { kind: "same".into(), text: text.trim().to_string(), options: vec![text.clone()], ..Default::default() };
        let covered = match parse_same(text) {
            Ok(Same::Rename(old, new)) => check.identity(Category::Rename, &old, &new, &mut result),
            Ok(Same::Move(old, new)) => check.identity(Category::Move, &old, &new, &mut result),
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
        if covered_by[index].is_some() || !fact.category.agent_states() || check.test_unit(unit) || !check.existing(unit) {
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
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
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
        // The edits of one item that are in no group of equal edits make one line: one
        // `change` fact of the item states them all.
        let single = |gap: &Gap| gap.places.len() == 1 && gap.option.starts_with("--change ");
        match gaps.iter_mut().find(|other| single(&gap) && other.items == gap.items && other.option == gap.option && (single(other) || other.text.ends_with(DIFFERENT))) {
            Some(other) => {
                other.places.extend(gap.places);
                other.facts.extend(gap.facts.into_iter().filter(|fact| !other.facts.contains(fact)).collect::<Vec<_>>());
                other.text = format!("{} {DIFFERENT}", other.places.len());
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
    report.gaps = gaps;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forms_of_a_same_claim() {
        assert_eq!(parse_same("rename a::Old New"), Ok(Same::Rename("a::Old".into(), "New".into())));
        assert_eq!(parse_same("move <A as B>::f => m::<A as B>::f"), Ok(Same::Move("<A as B>::f".into(), "m::<A as B>::f".into())));
        assert_eq!(parse_same("rewrite $c.inner => $c.raw()"), Ok(Same::Rewrite("$c.inner".into(), "$c.raw()".into())));
        // The negative cases: each one has a message for the agent.
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
