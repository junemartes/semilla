//! Design 87 (batch 14e): a printed claim template is a checked claim template.
//!
//! Each printer of a template (the coverage of the changed bodies, the AST pass) takes the names
//! of the item from one source: the name record of the compiler for the tree (design 70), whose
//! definitions the check of the intent files resolves too (R87.2-1). Then one gate checks each
//! template of the step with the check of `semilla intent check` (R87.1-1): the step prints only
//! the templates that the check accepts, with unique function names (R87.5-1).

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::namerec::{Def, NameIndex, Target};
use semilla_intent::parse::{Named, Names};
use semilla_syntax::Workspace;

use crate::{BodyOracle, StepInput, StepOutcome};

/// [R87.2-1] The names of one item of a tree for a template.
#[derive(Clone, Debug)]
pub(crate) struct ItemNames {
    /// The scope path that the template writes (`shop::Cart::total`,
    /// `<shop::Cart as core::clone::Clone>::clone`).
    pub scope: String,
    /// The definition path of the item in the record of the tree (the name of its body).
    pub expect: String,
    /// The path of the item for a call, with no version prefix, or the cause why a call needs
    /// more (an item of a trait impl).
    pub call: Result<String, String>,
    /// The claim type of the receiver (`before::shop::Cart`), or the cause.
    pub receiver: Result<String, String>,
    /// The generic parameters of the impl block of the item.
    pub generic: Vec<String>,
    /// The file of the item, the byte offset of its text, and the text (its signature first).
    pub file: String,
    pub start: usize,
    pub text: String,
}

/// True if each segment of a path is a name of Rust (no `_`, no `{…}`, no generic arguments).
fn plain_path(path: &str) -> bool {
    !path.is_empty() && path.split("::").all(|segment| !segment.is_empty() && segment != "_" && segment.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '#'))
}

/// The self type and the trait of the text of an impl header of the record
/// (`<shop::Cart as core::hash::Hasher>`, or `<shop::W<u8>>` for an inherent impl).
fn impl_parts(header: &str) -> (String, Option<String>) {
    let inner = header.strip_prefix('<').and_then(|rest| rest.strip_suffix('>')).unwrap_or(header);
    let mut depth = 0i32;
    let bytes = inner.as_bytes();
    for at in 0..bytes.len() {
        match bytes[at] {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b' ' if depth == 0 && inner[at..].starts_with(" as ") => return (inner[..at].to_string(), Some(inner[at + 4..].to_string())),
            _ => {}
        }
    }
    (inner.to_string(), None)
}

/// The innermost impl block of the record that has the place of a definition.
fn impl_of<'a>(names: &'a NameIndex, def: &Def) -> Option<&'a Def> {
    let file = def.file.as_deref()?;
    let (start, end) = def.span?;
    names.defs_in(file).filter(|other| other.kind == "implementation" && other.span.is_some_and(|(s, e)| s <= start && end <= e && (s, e) != (start, end))).min_by_key(|other| other.span.map(|(s, e)| e - s).unwrap_or(usize::MAX))
}

/// [R87.2-1] The names of an item of a tree for a template: from the name record of the tree
/// (the definitions of the compiler), never from the text of a record name. The cause if the
/// language has no path for the item (a gap of the intent language, R87.2-2).
pub(crate) fn item_names(tree: &Workspace, path: &str) -> Result<ItemNames, String> {
    let item = tree.items.get(path).ok_or_else(|| "the tree has no item of the body".to_string())?;
    let Some(names) = tree.names.get() else { return syntax_names(tree, path, item) };
    let def = crate::records::item_def(tree, &names, path).ok_or_else(|| "the name record has no definition at the place of the item (code that a macro makes)".to_string())?;
    let (file, start, end) = crate::records::item_place(tree, item).ok_or_else(|| "the item has no place in its file".to_string())?;
    def_names(tree, &names, &def, file, start, end)
}

/// [R87.2-1] The names of a definition of the name record, with the place of its text.
fn def_names(tree: &Workspace, names: &NameIndex, def: &Def, file: String, start: usize, end: usize) -> Result<ItemNames, String> {
    let text = tree.rust_files.get(&file).and_then(|text| text.get(start..end)).unwrap_or("").to_string();
    let (scope, call, receiver, generic) = match impl_of(names, def) {
        None => {
            if !plain_path(&def.path) {
                return Err(format!("the definition path `{}` has a part that a scope path cannot name (an item inside a function or an unnamed constant)", def.path));
            }
            (def.path.clone(), Ok(def.path.clone()), Err("the item is not in an impl block".to_string()), Vec::new())
        }
        Some(block) => {
            let header = block.trait_ref.clone().ok_or_else(|| "the impl block of the item has no header in the name record".to_string())?;
            let (self_type, trait_path) = impl_parts(&header);
            let adt = self_type.split('<').next().unwrap_or("").to_string();
            if !plain_path(&adt) {
                return Err(format!("the item is in an impl block on the type `{self_type}`, which a scope path cannot name"));
            }
            let generic = impl_generics(tree, block);
            match trait_path {
                Some(trait_path) => (format!("<{self_type} as {trait_path}>::{}", def.name), Err("an item of a trait impl: a call names the trait and the types of an instance".to_string()), Err("an item of a trait impl".to_string()), generic),
                None => {
                    let receiver = if self_type.contains('<') { Err(format!("the type `{self_type}` of `self` has generic arguments")) } else { Ok(format!("before::{adt}")) };
                    (format!("{adt}::{}", def.name), Ok(format!("{adt}::{}", def.name)), receiver, generic)
                }
            }
        }
    };
    library_item(tree, &scope)?;
    Ok(ItemNames { scope, expect: def.path.clone(), call, receiver, generic, file, start, text })
}

/// The names of an item of a tree with no name record (the compiler did not build the tree): the
/// path of the syntax tree. The gate checks the template, as each other template.
fn syntax_names(tree: &Workspace, path: &str, item: &semilla_syntax::Item) -> Result<ItemNames, String> {
    if !plain_path(path) {
        return Err(format!("`{path}` has no plain path for a claim, and the step has no name record of the tree"));
    }
    let (file, start, end) = crate::records::item_place(tree, item).ok_or_else(|| "the item has no place in its file".to_string())?;
    let text = tree.rust_files.get(&file).and_then(|text| text.get(start..end)).unwrap_or("").to_string();
    let owner = path.rsplit_once("::").map(|(owner, _)| owner).unwrap_or(path);
    Ok(ItemNames { scope: path.to_string(), expect: path.to_string(), call: Ok(path.to_string()), receiver: Ok(format!("before::{owner}")), generic: Vec::new(), file, start, text })
}

/// [R87.2-2] The intent crate links the libraries of the workspace only: a scope path must start
/// with the name of a library crate (the type of a qualified path too).
fn library_item(tree: &Workspace, scope: &str) -> Result<(), String> {
    let first = scope.trim_start_matches('<').split("::").next().unwrap_or("");
    // A qualified path: the impl is in the crate of the type or of the trait.
    let trait_first = scope.split(" as ").nth(1).and_then(|rest| rest.split("::").next()).unwrap_or("");
    if tree.crates.iter().any(|info| info.name == first || info.name == trait_first) {
        Ok(())
    } else {
        Err(format!("the item is in the crate `{first}`, which is not a library of the workspace (a test, a bench, an example, or a binary target)"))
    }
}

/// [R87.2-1] The names of a body that the syntax tree has no item for (a body that a derive
/// makes): the one definition of the name record with the plain form of the record name. Its
/// definition path is the qualified path that the compiler prints for an item of a trait impl.
pub(crate) fn made_names(tree: &Workspace, record: &str) -> Result<ItemNames, String> {
    let names = tree.names.get().ok_or_else(|| "the step has no name record of the tree".to_string())?;
    // The record name has the visible paths of other crates, the definition the real paths: the
    // two match by the last name of each path, with the generic arguments.
    let plain = short_form(record);
    let found: Vec<&Def> = names.defs.iter().filter(|def| !def.kind.is_empty() && def.kind != "implementation" && short_form(&def.path) == plain).collect();
    let mut paths: Vec<&str> = found.iter().map(|def| def.path.as_str()).collect();
    paths.sort();
    paths.dedup();
    let def = match paths.as_slice() {
        [_] => found[0],
        [] => return Err(format!("`{record}` is no item of the syntax tree and no definition of the name record (a body that the compiler makes)")),
        _ => {
            // Two impls of one type with the same item name (`fmt` of two traits): the trait of
            // the record name picks one.
            let trait_part = |text: &str| text.split(" as ").nth(1).and_then(|rest| rest.split('>').next()).and_then(|path| path.rsplit("::").next()).map(str::to_string);
            let wanted = trait_part(record);
            let picked: Vec<&&Def> = found.iter().filter(|def| wanted.is_some() && trait_part(&def.path) == wanted).collect();
            match picked.as_slice() {
                [one] => **one,
                _ => return Err(format!("`{record}` is no item of the syntax tree, and the name record has {} definitions with its plain name", paths.len())),
            }
        }
    };
    if !def.path.starts_with('<') {
        // A definition with a place (an item of an impl block): its impl block gives the names.
        if let (Some(file), Some((start, end))) = (def.file.clone(), def.span) {
            return def_names(tree, &names, def, file, start, end);
        }
        return Err(format!("`{record}` is no item of the syntax tree (a body that the compiler makes)"));
    }
    let (self_type, _) = impl_parts(def.path.rsplit_once(">::").map(|(head, _)| head).unwrap_or(&def.path).trim_start_matches('<'));
    if !plain_path(self_type.split('<').next().unwrap_or("")) {
        return Err(format!("the item is in an impl block on the type `{self_type}`, which a scope path cannot name"));
    }
    library_item(tree, &def.path)?;
    Ok(ItemNames { scope: def.path.clone(), expect: def.path.clone(), call: Err("an item of a trait impl: a call names the trait and the types of an instance".into()), receiver: Err("an item of a trait impl".into()), generic: Vec::new(), file: String::new(), start: 0, text: String::new() })
}

/// [R87.2-2] The cause of a pair of two items with two paths (a rename, a twin pair).
pub(crate) fn pair_gap(old: &str, new: &str) -> String {
    format!("the step pairs `{old}` with `{new}` (a rename or a twin pair), and one scope path names one item in the two versions: claim each version with its own claim")
}

/// A name with each path `a::b::C` as its last name `C` (`<NonZero<i8> as Bounded>::max_value`).
fn short_form(name: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let chars: Vec<char> = name.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else if c == ':' && chars.get(at + 1) == Some(&':') && !word.is_empty() && chars.get(at + 2).is_some_and(|next| next.is_alphanumeric() || *next == '_') && !out.ends_with('>') {
            // A part of a path before its last name.
            word.clear();
            at += 2;
            continue;
        } else {
            out.push_str(&word);
            word.clear();
            out.push(c);
        }
        at += 1;
    }
    out.push_str(&word);
    out
}

/// The generic parameters of an impl block, from its source text.
fn impl_generics(tree: &Workspace, block: &Def) -> Vec<String> {
    let (Some(file), Some((start, end))) = (block.file.as_deref(), block.span) else { return Vec::new() };
    tree.rust_files.get(file).and_then(|text| text.get(start..end)).map(semilla_intent::parse::impl_generics).unwrap_or_default()
}

/// [R87.2-1] The names of a signature for the template printer: each type path from the name
/// record at its place. `real`: a type of another crate gets its definition path; else the
/// written path (the second form of the gate, R87.1-1).
pub(crate) fn signature_names<'a>(tree: &'a Workspace, names: &'a ItemNames, real: bool) -> Names<'a> {
    let index = tree.names.get();
    let library: BTreeSet<String> = tree.crates.iter().map(|info| info.name.replace('-', "_")).collect();
    let path = move |written: &str, start: usize, end: usize| -> Named {
        let Some(index) = index.as_ref() else {
            // No name record: a type of the tree by its one name (the syntax tree).
            let found: Vec<&String> = tree.items.iter().filter(|(path, item)| matches!(item.kind, semilla_syntax::ItemKind::Struct | semilla_syntax::ItemKind::Enum | semilla_syntax::ItemKind::Type) && path.rsplit("::").next() == Some(written)).map(|(path, _)| path).collect();
            return if found.len() == 1 && !written.contains("::") { Named::Path(format!("before::{}", found[0])) } else { Named::Written };
        };
        let from = names.start + start;
        let to = names.start + end;
        let target = index.targets_in(&names.file, from, to).filter(|(s, e, _)| *s == from && *e == to).find_map(|(_, _, target)| match target {
            Target::Def(at) => index.defs.get(at),
            Target::Local => None,
        });
        let Some(def) = target else { return Named::Written };
        if !plain_path(&def.path) {
            return Named::Gap(format!("the type `{}` has a part that a path cannot name", def.path));
        }
        let first = def.path.split("::").next().unwrap_or("");
        if def.file.is_some() || library.contains(first) {
            return Named::Path(format!("before::{}", def.path));
        }
        if real { Named::Path(def.path.clone()) } else { Named::Written }
    };
    let call_path = names.call.clone();
    Names {
        path: Box::new(path),
        receiver: names.receiver.clone(),
        call: Box::new(move |version: &str| call_path.clone().map(|path| format!("{version}::{path}"))),
        generic: names.generic.clone(),
    }
}

/// [R87.1-1] One template for the gate: its forms (the gate checks the first; a rejected form
/// gives its place to the next one) and the record names of the item that its scope path must
/// resolve to (`before::NAME`, `after::NAME`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ask {
    pub forms: Vec<String>,
    pub expect: Vec<String>,
}

/// [R87.1-1] The answer of the gate for one template: the index of the form that the check
/// accepts, or the first error of the check in one line.
pub type Verdict = Result<usize, String>;

/// One group of a template line: a label (`the full compare`, `a difference`) and the
/// templates of the group (each one with the alternatives `A` or `B`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Group {
    pub label: String,
    pub templates: Vec<String>,
    /// A text in place of the templates (for example "no property template (…)").
    pub note: String,
}

/// [R87.1-1] The template part of the coverage line of a body before the gate.
#[derive(Clone, Debug, Default)]
pub(crate) struct Offer {
    /// The cause of "MIR changes", or empty.
    pub cause: String,
    /// The part that changes (the signature, a type of the signature, an added body), or empty.
    pub what: String,
    pub groups: Vec<Group>,
    /// The template of a node of the AST map for the item: the index of the entry and the text
    /// of the line ("the claim template of the node nK …").
    pub ast: Option<(usize, String)>,
    /// The cause of a body with no template (a gap of the language, R87.2-2).
    pub none: Option<String>,
}

impl Offer {
    /// The text of the line after the gate.
    pub fn render(&self, verdict: &dyn Fn(&str) -> Option<(String, Option<String>)>, ast_ok: &dyn Fn(usize) -> bool) -> String {
        let tail = if let Some((_, text)) = self.ast.as_ref().filter(|(at, _)| ast_ok(*at)) {
            text.clone()
        } else if let Some(cause) = &self.none {
            format!("no template: {cause}")
        } else {
            let mut first_cause: Option<String> = None;
            let mut parts = Vec::new();
            let mut any = false;
            for group in &self.groups {
                let mut kept = Vec::new();
                for template in &group.templates {
                    match verdict(template) {
                        Some((text, None)) => kept.push(format!("`{text}`")),
                        Some((_, Some(cause))) => {
                            first_cause.get_or_insert(cause);
                        }
                        None => kept.push(format!("`{template}`")),
                    }
                }
                any |= !kept.is_empty();
                let body = if !kept.is_empty() {
                    kept.join(" or ")
                } else if !group.note.is_empty() {
                    // A note of its own (no template of the group): a text of the line.
                    any |= group.templates.is_empty();
                    group.note.clone()
                } else {
                    continue;
                };
                parts.push(if group.label.is_empty() { body } else { format!("{}: {body}", group.label) });
            }
            if !any {
                format!("no template: {}", first_cause.unwrap_or_else(|| "the check of the intent files rejects each template".into()))
            } else {
                parts.join("; ")
            }
        };
        let tail = if self.what.is_empty() { tail } else { format!("{}: {tail}", self.what) };
        if self.cause.is_empty() { tail } else { format!("{}; {tail}", self.cause) }
    }
}

/// The templates of a step that wait for the gate.
#[derive(Clone, Debug, Default)]
pub struct Pending {
    /// The forms and the record names of each template text.
    pub(crate) asks: BTreeMap<String, Ask>,
    /// The offer of each line of the coverage, by the index of the line.
    pub(crate) offers: Vec<(usize, Offer)>,
    /// [R92.1-1] The templates of the notes of the pairs (a tie, a pair that does not pass):
    /// each note has the mark of [`note_template`] until the gate writes its verdict.
    pub(crate) notes: Vec<String>,
}

/// The mark of a template in a note that waits for the gate.
const NOTE_MARK: &str = "\u{2063}template\u{2063}";

/// [R92.1-1] The text of a template in a note before the gate: the gate writes `template: `…``
/// or `no template: CAUSE` in its place.
pub(crate) fn note_template(template: &str) -> String {
    format!("{NOTE_MARK}{template}")
}

impl Pending {
    /// Adds the templates of another printer.
    pub(crate) fn merge(&mut self, other: Pending) {
        for (text, ask) in other.asks {
            let entry = self.asks.entry(text).or_default();
            if entry.forms.is_empty() {
                entry.forms = ask.forms;
            }
            for name in ask.expect {
                if !entry.expect.contains(&name) {
                    entry.expect.push(name);
                }
            }
        }
        self.offers = other.offers;
        for note in other.notes {
            if !self.notes.contains(&note) {
                self.notes.push(note);
            }
        }
    }

    /// Notes a template with its forms (the first form is the text of the line).
    pub(crate) fn note(&mut self, forms: Vec<String>, expect: Vec<String>) -> String {
        let first = forms.first().cloned().unwrap_or_default();
        let entry = self.asks.entry(first.clone()).or_default();
        if entry.forms.is_empty() {
            entry.forms = forms;
        }
        for name in expect {
            if !entry.expect.contains(&name) {
                entry.expect.push(name);
            }
        }
        first
    }
}

/// The name of the function of a template (`fn NAME`).
fn function_name(template: &str) -> Option<(usize, usize)> {
    let at = template.find("] fn ")? + 5;
    let end = template[at..].find(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(template.len(), |len| at + len);
    Some((at, end))
}

/// A template with another function name.
fn renamed(template: &str, name: &str) -> String {
    match function_name(template) {
        Some((at, end)) => format!("{}{name}{}", &template[..at], &template[end..]),
        None => template.to_string(),
    }
}

/// [R87.1-1] [R87.5-1] The gate of the templates of a step: each template of the AST map and of
/// the coverage gets a unique function name (E5), then one check of the intent files for all of
/// them (the oracle); a template that the check rejects is not printed: its place has
/// `no template: CAUSE`. A rejected form gives its place to the next form of the template.
pub(crate) fn gate(input: &StepInput, bodies: &dyn BodyOracle, outcome: &mut StepOutcome) {
    let pending = std::mem::take(&mut outcome.templates);
    // The templates in the order of the output: the AST map, then the coverage.
    let mut order: Vec<String> = Vec::new();
    if let Some(map) = &outcome.report.ast {
        order.extend(map.entries.iter().filter(|entry| entry.template.starts_with("#[")).map(|entry| entry.template.clone()));
    }
    for (_, offer) in &pending.offers {
        if offer.none.is_some() {
            continue;
        }
        for group in &offer.groups {
            order.extend(group.templates.iter().cloned());
        }
    }
    order.extend(pending.notes.iter().cloned());
    if order.is_empty() {
        apply(outcome, &pending, &BTreeMap::new());
        return;
    }
    // [R87.5-1] One function name for each template text: a second template with the name of
    // an earlier one gets the first free name `NAME_2`, `NAME_3`, ….
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut new_name: BTreeMap<String, String> = BTreeMap::new();
    for text in &order {
        if new_name.contains_key(text) {
            continue;
        }
        let Some((at, end)) = function_name(text) else { continue };
        let name = &text[at..end];
        let mut chosen = name.to_string();
        let mut number = 2;
        while taken.contains(&chosen) {
            chosen = format!("{name}_{number}");
            number += 1;
        }
        taken.insert(chosen.clone());
        new_name.insert(text.clone(), chosen);
    }
    let mut unique: Vec<String> = Vec::new();
    let mut asks: Vec<Ask> = Vec::new();
    for text in &order {
        if unique.contains(text) {
            continue;
        }
        let name = new_name.get(text).cloned().unwrap_or_default();
        let base = pending.asks.get(text).cloned().unwrap_or_else(|| Ask { forms: vec![text.clone()], expect: Vec::new() });
        unique.push(text.clone());
        asks.push(Ask { forms: base.forms.iter().map(|form| renamed(form, &name)).collect(), expect: base.expect });
    }
    let verdicts = bodies.check_templates(input.step, &asks).unwrap_or_else(|| vec![Err("the step has no compiled trees for the check of the intent files".into()); asks.len()]);
    let mut result: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    for ((text, ask), verdict) in unique.iter().zip(&asks).zip(verdicts) {
        let entry = match verdict {
            Ok(form) => (ask.forms.get(form).cloned().unwrap_or_default(), None),
            Err(cause) => (String::new(), Some(one_line(&cause))),
        };
        result.insert(text.clone(), entry);
    }
    apply(outcome, &pending, &result);
}

/// The first line of a cause, with no back quote at its ends (the line keeps its back quotes
/// for the templates).
fn one_line(cause: &str) -> String {
    let line = cause.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("the check rejects the template");
    line.chars().take(300).collect()
}

/// Writes the verdicts of the gate into the AST map and the coverage lines.
fn apply(outcome: &mut StepOutcome, pending: &Pending, result: &BTreeMap<String, (String, Option<String>)>) {
    let mut ast_ok: BTreeSet<usize> = BTreeSet::new();
    if let Some(map) = outcome.report.ast.as_mut() {
        for (at, entry) in map.entries.iter_mut().enumerate() {
            if !entry.template.starts_with("#[") {
                continue;
            }
            match result.get(&entry.template) {
                Some((text, None)) => {
                    entry.template = text.clone();
                    ast_ok.insert(at);
                }
                Some((_, Some(cause))) => entry.template = format!("no template: {cause}"),
                None => {
                    ast_ok.insert(at);
                }
            }
        }
    }
    // [R92.1-1] The template of each note of a pair.
    for note in outcome.report.notes.iter_mut() {
        let Some(at) = note.find(NOTE_MARK) else { continue };
        let template = note[at + NOTE_MARK.len()..].to_string();
        let text = match result.get(&template) {
            Some((text, None)) => format!("template: `{text}`"),
            Some((_, Some(cause))) => format!("no template: {cause}"),
            None => "no template: the step has no compiled trees for the check of the intent files".to_string(),
        };
        note.replace_range(at.., &text);
    }
    // [R97.8-1] The template of the closure form of an entry (design 97).
    for fact in outcome.report.facts.iter_mut() {
        let Some(text) = fact.template.as_mut() else { continue };
        let Some(at) = text.find(NOTE_MARK) else { continue };
        let template = text[at + NOTE_MARK.len()..].to_string();
        let verdict = match result.get(&template) {
            Some((form, None)) => format!("template: `{form}`"),
            Some((_, Some(cause))) => format!("no template: {cause}"),
            None => "no template: the step has no compiled trees for the check of the intent files".to_string(),
        };
        text.replace_range(at.., &verdict);
    }
    let verdict = |template: &str| result.get(template).cloned();
    for (at, offer) in &pending.offers {
        if let Some(cover) = outcome.report.coverage.get_mut(*at) {
            cover.detail = offer.render(&verdict, &|node| ast_ok.contains(&node));
        }
    }
}
