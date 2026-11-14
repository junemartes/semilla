//! The items that a macro call makes (batch 6b, part 2; design 34.2).
//!
//! The syntax tree does not expand a macro call, so the text of a call does not show the items
//! that it makes. The compiler gives each item of the expanded code with the place of the
//! outermost macro call. An item belongs to a call if its place is in the source range of the
//! call. The report then uses the rules of an item with no macro: the fact text lists the items,
//! the public API list has the reachable items, and the spotlight has an attribute that can
//! change the behavior, `unsafe` code, and an impl that the compiler calls with no call in the
//! text.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::{Anchor, Class, Step, StepReport};
use semilla_syntax::{Edit, EditKind, Item, ItemKind, Workspace};

/// One item of the expanded code, from the compiler.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MadeItem {
    /// The file of the place, relative to the workspace root.
    pub file: String,
    /// The line (from 1) and the column (from 0, in characters) of the place: the start of the
    /// outermost macro call, or the start of the item for an item with no macro.
    pub line: usize,
    pub col: usize,
    /// The module path of the item in the crate (empty for the crate root).
    pub module: String,
    /// The source text of the item after the expansion.
    pub text: String,
    /// True if the item is a `#[test]` function.
    pub test: bool,
    /// True if a different crate can reach the item, by the compiler.
    pub public: bool,
}

/// The expanded code of one tree, in each checked configuration.
#[derive(Clone, Debug, Default)]
pub struct Made {
    pub items: Vec<MadeItem>,
    /// The name of a body from a macro → the file, the line, and the column of the outermost
    /// macro call.
    pub sites: BTreeMap<String, (String, usize, usize)>,
}

/// One made item in the form of the report.
#[derive(Clone, Debug)]
struct Shown {
    /// The text for the list: `impl PartialEq for AsKebabCase<&str>`, or the name of a test.
    label: String,
    /// The identity of the item for the compare of the two trees: the module and the text.
    key: (String, String),
    public: bool,
    /// The attributes that can change the behavior (the rule of a new item).
    attributes: Vec<String>,
    /// True if the text has the word `unsafe`.
    has_unsafe: bool,
    /// The trait of an impl that the compiler calls with no call in the text.
    implicit: Option<String>,
}

/// The result of the pass for the later steps of the verifier.
#[derive(Default)]
pub(crate) struct Outcome {
    /// The facts whose text lists the made items already.
    pub listed: BTreeSet<usize>,
    /// The lines for the list of the public API changes.
    pub major: Vec<String>,
    pub minor: Vec<String>,
    /// The lines for the spotlight list of the step.
    pub spotlight: Vec<String>,
}

/// True for the item of a macro call (not a `macro_rules!` definition).
pub(crate) fn is_call(item: &Item) -> bool {
    item.kind == ItemKind::Macro && !crate::owners::is_macro_definition(item)
}

/// True if a place is in the source range of an item.
pub(crate) fn in_range(item: &Item, place: (&str, usize, usize)) -> bool {
    let (file, line, col) = place;
    if item.file != file || line < item.line_start || line > item.line_end {
        return false;
    }
    // A workspace from a stored snapshot has no columns: the lines decide.
    if item.col_start == 0 && item.col_end == 0 {
        return true;
    }
    (line > item.line_start || col >= item.col_start) && (line < item.line_end || col < item.col_end)
}

/// The text of a macro call for a fact: `same!(&str)`.
pub(crate) fn call_text(item: &Item) -> String {
    let text = item.source.lines().filter(|line| !line.trim_start().starts_with("//") && !line.trim_start().starts_with("#[")).collect::<Vec<_>>().join(" ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    semilla_syntax::tree::short(text.trim_end_matches(';').trim_end(), 60)
}

/// The name of a macro definition: `same` for `m::same!` and for `m::same!#1`.
fn definition_name(item: &Item) -> String {
    item.path.rsplit("::").next().unwrap_or("").split('!').next().unwrap_or("").to_string()
}

/// The text of a token stream with no space that a reader does not write.
fn tidy(tokens: proc_macro2::TokenStream) -> String {
    let text = tokens.to_string();
    let word = |part: &str| part.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\'');
    let word_end = |part: &str| part.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut out = String::new();
    let mut last = "";
    for part in text.split_whitespace() {
        let space = match (last, part) {
            ("", _) => false,
            (",", _) | (_, "+" | "=" | "->") | ("+" | "=" | "->" | ":", _) => true,
            // `pub(crate)` has no space.
            ("pub", next) => !next.starts_with('('),
            ("for" | "as" | "dyn" | "mut" | "const" | "impl" | "where" | "in" | "unsafe", next) => next != "<" && next != "(",
            (before, next) => (word_end(before) || before == ">") && word(next),
        };
        if space {
            out.push(' ');
        }
        out.push_str(part);
        last = part;
    }
    out
}

/// Reads one made item from its expanded text. `None`: Semilla cannot read the text.
fn shown(item: &MadeItem) -> Option<Shown> {
    use quote::quote;
    // The compiler prints the crate of a macro as `$crate`.
    let text = item.text.replace("$crate", "crate");
    let parsed = syn::parse_str::<syn::Item>(&text).ok()?;
    let vis = |vis: &syn::Visibility| match vis {
        syn::Visibility::Inherited => String::new(),
        other => format!("{} ", tidy(quote!(#other))),
    };
    let mut implicit = None;
    let mut inner_attrs: Vec<&syn::Attribute> = Vec::new();
    let (attrs, label): (&[syn::Attribute], String) = match &parsed {
        syn::Item::Fn(value) if item.test => (&value.attrs, value.sig.ident.to_string()),
        syn::Item::Fn(value) => (&value.attrs, format!("{}fn {}", vis(&value.vis), value.sig.ident)),
        syn::Item::Impl(value) => {
            if let Some((_, path, _)) = &value.trait_ {
                implicit = path.segments.last().map(|segment| segment.ident.to_string()).filter(|name| matches!(name.as_str(), "Drop" | "Deref" | "DerefMut"));
            }
            inner_attrs.extend(value.items.iter().flat_map(|inner| match inner {
                syn::ImplItem::Fn(inner) => inner.attrs.iter().collect::<Vec<_>>(),
                syn::ImplItem::Const(inner) => inner.attrs.iter().collect(),
                _ => Vec::new(),
            }));
            let mut header = value.clone();
            header.attrs.clear();
            header.items.clear();
            let text = tidy(quote!(#header));
            (&value.attrs, text.trim_end_matches('}').trim_end().trim_end_matches('{').trim_end().to_string())
        }
        syn::Item::Struct(value) => (&value.attrs, format!("{}struct {}", vis(&value.vis), value.ident)),
        syn::Item::Enum(value) => (&value.attrs, format!("{}enum {}", vis(&value.vis), value.ident)),
        syn::Item::Union(value) => (&value.attrs, format!("{}union {}", vis(&value.vis), value.ident)),
        syn::Item::Trait(value) => (&value.attrs, format!("{}trait {}", vis(&value.vis), value.ident)),
        syn::Item::TraitAlias(value) => (&value.attrs, format!("{}trait {}", vis(&value.vis), value.ident)),
        syn::Item::Type(value) => (&value.attrs, format!("{}type {}", vis(&value.vis), value.ident)),
        syn::Item::Const(value) => (&value.attrs, format!("{}const {}", vis(&value.vis), value.ident)),
        syn::Item::Static(value) => (&value.attrs, format!("{}static {}", vis(&value.vis), value.ident)),
        syn::Item::Mod(value) => (&value.attrs, format!("{}mod {}", vis(&value.vis), value.ident)),
        syn::Item::Use(value) => {
            let tree = &value.tree;
            (&value.attrs, format!("{}use {}", vis(&value.vis), tidy(quote!(#tree))))
        }
        syn::Item::ExternCrate(value) => (&value.attrs, format!("extern crate {}", value.ident)),
        syn::Item::ForeignMod(value) => (&value.attrs, format!("extern {} block", value.abi.name.as_ref().map(|name| name.value()).unwrap_or_else(|| "C".into()))),
        syn::Item::Macro(value) => (&value.attrs, format!("macro_rules! {}", value.ident.as_ref()?)),
        _ => return None,
    };
    // The impls of a derive have the attributes of the compiler. A derive is not in the
    // spotlight on an item with no macro, so it is not in the spotlight here.
    let derived = attrs.iter().any(|attr| attr.path().is_ident("automatically_derived"));
    let texts: Vec<String> = if derived { Vec::new() } else { attrs.iter().chain(inner_attrs).map(|attr| quote!(#attr).to_string()).collect() };
    let attributes = if item.test { Vec::new() } else { crate::facts::new_attributes(&texts) };
    let has_unsafe = !derived && text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == "unsafe");
    Some(Shown { label, key: (item.module.clone(), item.text.clone()), public: item.public, attributes, has_unsafe, implicit })
}

/// The items that a macro call makes, in the order of the place and then of the text (the same
/// order in each run). `None`: Semilla cannot list them.
fn items_of(call: &Item, made: Option<&Made>) -> Option<Vec<Shown>> {
    let made = made?;
    let mut found: Vec<&MadeItem> = made.items.iter().filter(|item| in_range(call, (&item.file, item.line, item.col))).collect();
    found.sort();
    let mut out: Vec<Shown> = Vec::new();
    for item in found {
        let shown = shown(item)?;
        // A unit of a different configuration can give the same item again.
        match out.iter_mut().find(|known| known.key == shown.key) {
            Some(known) => known.public |= shown.public,
            None => out.push(shown),
        }
    }
    Some(out)
}

/// The list of the made items for a fact text, with the word `makes` or `made`.
fn list_text(items: &Option<Vec<Shown>>, word: &str) -> String {
    let Some(items) = items else { return format!("{word} items that Semilla cannot list") };
    let mut labels: Vec<&str> = Vec::new();
    for item in items {
        if !labels.contains(&item.label.as_str()) {
            labels.push(&item.label);
        }
    }
    if labels.is_empty() {
        return format!("{word} no item in the checked configurations");
    }
    let mut text = labels.iter().take(5).map(|label| format!("`{label}`")).collect::<Vec<_>>().join(", ");
    if labels.len() > 5 {
        text.push_str(&format!(", and {} more", labels.len() - 5));
    }
    format!("{word}: {text}")
}

/// One change of a macro call: the call before the step and the call after it.
struct Change<'a> {
    old: Option<&'a Item>,
    new: Option<&'a Item>,
    /// The fact of the edit, and true if the fact is the fact of this call (not the fact of a
    /// module around it).
    fact: Option<usize>,
    direct: bool,
}

/// Gives the facts of the macro edits their text, and applies the rules of an item with no
/// macro to the items that a changed macro call makes.
///
/// `made` has the expanded code of the tree before the step and of the tree after it. `None`:
/// the oracle cannot give it. Then a changed call "makes items that Semilla cannot list": the
/// fact is in the spotlight, and the API level of a call in library code is `major`.
pub(crate) fn apply(step: &Step, edits: &[Edit], fact_of: &[Option<usize>], before: &Workspace, after: &Workspace, made: Option<&[Made; 2]>, report: &mut StepReport) -> Outcome {
    let mut outcome = Outcome::default();
    let mut changes: Vec<Change> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let whole = |anchor: &Option<Anchor>| anchor.as_ref().is_some_and(|anchor| anchor.path.is_empty());
        let old = edit.before.as_ref().filter(|_| whole(&edit.before)).and_then(|anchor| before.items.get(&anchor.item));
        let new = edit.after.as_ref().filter(|_| whole(&edit.after)).and_then(|anchor| after.items.get(&anchor.item));
        let fact = fact_of.get(index).copied().flatten();
        // The fact of the item itself: the text names the item.
        let own = |item: &Item, report: &StepReport| fact.and_then(|fact| report.facts.get(fact)).is_some_and(|fact| fact.part == "edit" && fact.at.as_ref().is_some_and(|at| at.item == item.path && at.path.is_empty()));
        let (old, new) = match edit.kind {
            EditKind::ItemAdded => (None, new),
            EditKind::ItemRemoved => (old, None),
            EditKind::Docs | EditKind::Comment | EditKind::Attrs | EditKind::Visibility => continue,
            _ if edit.text_before.is_some() && edit.text_after.is_some() => (old, new),
            _ => continue,
        };
        let Some(item) = new.or(old) else { continue };
        if item.kind != ItemKind::Macro {
            // An added or removed module: each macro call in it follows the rules, and the
            // fact of the module keeps its text.
            if old.is_some() != new.is_some() {
                let ws = if new.is_some() { after } else { before };
                let prefix = format!("{}::", item.path);
                for call in ws.items.values().filter(|call| is_call(call) && call.path.starts_with(&prefix)) {
                    changes.push(Change { old: old.map(|_| call), new: new.map(|_| call), fact, direct: false });
                }
            }
            continue;
        }
        let direct = own(item, report);
        if crate::owners::is_macro_definition(item) {
            if direct && let Some(fact) = fact.and_then(|fact| report.facts.get_mut(fact)) {
                let name = definition_name(item);
                let path = format!("`{}`", item.path);
                if fact.description == format!("added {path}") {
                    fact.description = format!("added macro `{name}`");
                } else if fact.description == format!("removed {path}") {
                    fact.description = format!("removed macro `{name}`");
                } else if let Some(rest) = fact.description.strip_prefix("changed macro or signature ") {
                    fact.description = format!("changed macro `{name}`: {rest}");
                }
            }
            continue;
        }
        if old.is_some_and(|item| !is_call(item)) || new.is_some_and(|item| !is_call(item)) {
            continue;
        }
        changes.push(Change { old, new, fact, direct });
    }

    for change in changes {
        let olds = change.old.map(|call| items_of(call, made.map(|made| &made[0])));
        let news = change.new.map(|call| items_of(call, made.map(|made| &made[1])));
        let call = change.new.or(change.old).expect("a change has a call");
        let name = call_text(call);
        // The text of the fact.
        if change.direct && let Some(index) = change.fact && let Some(fact) = report.facts.get_mut(index) {
            let text = match (&olds, &news) {
                (None, Some(new)) => Some(format!("added macro call `{name}` ({})", list_text(new, "makes"))),
                (Some(old), None) => Some(format!("removed macro call `{name}` ({})", list_text(old, "made"))),
                (Some(old), Some(new)) => {
                    let sides = format!("changed macro call `{}` → `{name}`", change.old.map(call_text).unwrap_or_default());
                    let (made, makes) = (list_text(old, "made"), list_text(new, "makes"));
                    Some(if made.strip_prefix("made") == makes.strip_prefix("makes") { format!("{sides} ({makes})") } else { format!("{sides} ({made}; {makes})") })
                }
                (None, None) => None,
            };
            let known = fact.description.starts_with("added `") || fact.description.starts_with("removed `") || fact.description.starts_with("changed macro or signature ");
            if let Some(text) = text.filter(|_| known) {
                fact.description = text;
                outcome.listed.insert(index);
            }
        }
        // The rules of an item with no macro.
        let mut whys: Vec<String> = Vec::new();
        let sides = [olds.as_ref(), news.as_ref()];
        if sides.iter().flatten().any(|side| side.is_none()) {
            whys.push(format!("the macro call `{name}` makes items that Semilla cannot list"));
            let item_of_api = !call.test && crate::in_library(if change.new.is_some() { after } else { before }, call);
            let line = format!("macro call `{name}`: it makes items that Semilla cannot list (the public API can change)");
            if item_of_api && !outcome.major.contains(&line) {
                outcome.major.push(line);
            }
        }
        let empty = Vec::new();
        let old_items = olds.as_ref().and_then(|side| side.as_ref()).unwrap_or(&empty);
        let new_items = news.as_ref().and_then(|side| side.as_ref()).unwrap_or(&empty);
        let added: Vec<&Shown> = new_items.iter().filter(|item| !old_items.iter().any(|old| old.key == item.key)).collect();
        let removed: Vec<&Shown> = old_items.iter().filter(|item| !new_items.iter().any(|new| new.key == item.key)).collect();
        // A removed item names the call of the tree before the step.
        let old_name = change.old.map(call_text).unwrap_or_else(|| name.clone());
        for item in &removed {
            let line = format!("removed `{}` (the macro call `{old_name}` made it)", item.label);
            if item.public && !outcome.major.contains(&line) {
                outcome.major.push(line);
            }
        }
        for item in &added {
            let line = format!("added `{}` (the macro call `{name}` makes it)", item.label);
            if item.public && !outcome.minor.contains(&line) {
                outcome.minor.push(line);
            }
            if !item.attributes.is_empty() {
                let names = item.attributes.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
                whys.push(format!("a new item with an attribute that can change the behavior: {names} on `{}`, which the macro call `{name}` makes", item.label));
            }
        }
        for (item, word, name) in added.iter().map(|item| (item, "makes", &name)).chain(removed.iter().map(|item| (item, "made", &old_name))) {
            if item.has_unsafe {
                let line = format!("unsafe code: {} (the macro call `{name}` {word} `{}`)", call.path, item.label);
                if !outcome.spotlight.contains(&line) {
                    outcome.spotlight.push(line);
                }
            }
            if let Some(trait_name) = &item.implicit {
                whys.push(format!("an implicit call: the macro call `{name}` {word} `{}`. The compiler calls `{trait_name}` with no call in the text, so the MIR of the callers does not show the change", item.label));
                let line = format!("implicit call: {} (the macro call `{name}` {word} `{}`; the MIR of the callers does not show it)", call.path, item.label);
                if !outcome.spotlight.contains(&line) {
                    outcome.spotlight.push(line);
                }
            }
        }
        if whys.is_empty() {
            continue;
        }
        let why = whys.join("; ");
        match change.fact.and_then(|index| report.facts.get_mut(index)) {
            Some(fact) => {
                let known = fact.spotlight_why.take();
                fact.spotlight_why = Some(match known {
                    Some(known) if known.contains(&why) => known,
                    Some(known) => format!("{known}; {why}"),
                    None => why,
                });
                if !fact.spotlight {
                    fact.spotlight = true;
                }
                if fact.reason.is_none() {
                    fact.reason = step.reasons.get(&fact.reason_place()).cloned().or_else(|| step.reasons.get(&call.path).cloned());
                }
            }
            // A claim owns the edit: the made items get a fact of their own.
            None => {
                let id = format!("f{}", report.facts.len() + 1);
                report.facts.push(semilla_core::Fact {
                    id,
                    at: Some(Anchor::item(&call.path)),
                    file: Some(call.file.clone()),
                    part: "macro call".into(),
                    description: format!("items of the macro call `{name}`"),
                    status: "derived".into(),
                    class: Some(Class::Logic),
                    spotlight: true,
                    spotlight_why: Some(why),
                    reason: step.reasons.get(&call.path).cloned(),
                    ..Default::default()
                });
            }
        }
    }
    outcome
}
