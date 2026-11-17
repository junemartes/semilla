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
use std::fmt::Write as _;

use semilla_core::{Anchor, Category, Step, StepReport};
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
    let text = item
        .source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//") && !line.trim_start().starts_with("#["))
        .collect::<Vec<_>>()
        .join(" ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    semilla_syntax::tree::short(text.trim_end_matches(';').trim_end(), 60)
}

/// The name of a macro definition: `same` for `m::same!` and for `m::same!#1`.
/// The macros of the standard prelude (design 38.7). A local `macro_rules!` with one of these
/// names replaces the standard macro for each call in its scope.
const STD_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "matches",
    "vec",
    "format",
    "format_args",
    "write",
    "writeln",
    "print",
    "println",
    "eprint",
    "eprintln",
    "panic",
    "unreachable",
    "unimplemented",
    "todo",
    "concat",
    "stringify",
    "line",
    "column",
    "file",
    "module_path",
    "cfg",
    "env",
    "option_env",
    "include",
    "include_str",
    "include_bytes",
    "compile_error",
    "dbg",
];

/// The most call sites that the text of a standard macro name names one by one.
const STD_CALL_NAMES: usize = 6;

/// The text for a local macro with the name of a standard macro (design 38.7): " (it replaces
/// the standard macro `matches!` for each call in its scope: `a::f`, `a::g`)". The scope is
/// the module of the definition and its inner modules, after the definition in its own file.
/// A call with a path (`std::matches!`) is not in the list. An empty text for each other name.
fn std_macro_text(name: &str, definition: &Item, ws: &Workspace, added: bool) -> String {
    if !STD_MACROS.contains(&name) {
        return String::new();
    }
    let module = definition.path.rsplit_once("::").map_or("", |(module, _)| module);
    let calls = |source: &str| -> bool {
        let pattern = format!("{name}!");
        source.match_indices(&pattern).any(|(at, _)| {
            let before = source[..at].chars().last();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
        })
    };
    let mut sites: Vec<&str> = ws
        .items
        .values()
        .filter(|item| {
            item.crate_name == definition.crate_name && item.kind != ItemKind::Macro && item.kind != ItemKind::Mod && item.kind != ItemKind::Impl
        })
        .filter(|item| item.path.starts_with(&format!("{module}::")) && (item.file != definition.file || item.line_start > definition.line_end))
        .filter(|item| calls(&item.source))
        .map(|item| item.path.as_str())
        .collect();
    sites.sort_unstable();
    let count = sites.len();
    let mut names: Vec<String> = sites.iter().take(STD_CALL_NAMES).map(|site| format!("`{site}`")).collect();
    if count > STD_CALL_NAMES {
        names.push(format!("and {} more", count - STD_CALL_NAMES));
    }
    let list = if count == 0 {
        "no call in its scope".to_string()
    } else {
        format!("each call in its scope: {}", names.join(", "))
    };
    if added {
        format!(" (it replaces the standard macro `{name}!`; {list})")
    } else {
        format!(" (the standard macro `{name}!` is used again; {list})")
    }
}

fn definition_name(item: &Item) -> String {
    item.path.rsplit("::").next().unwrap_or("").split('!').next().unwrap_or("").to_string()
}

/// The text of a token stream with no space that a reader does not write.
fn tidy(tokens: &semilla_syntax::tt::TokenStream) -> String {
    let text = tokens.to_string();
    let word = |part: &str| part.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\'');
    let word_end = |part: &str| part.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut out = String::new();
    let mut last = "";
    for part in text.split_whitespace() {
        let space = match (last, part) {
            ("", _) => false,
            ("," | "+" | "=" | "->" | ":", _) | (_, "+" | "=" | "->") => true,
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

/// The name of a named item, or an empty text.
fn name_text(node: &impl semilla_syntax::rs::ast::HasName) -> String {
    node.name().map_or_default(|name| name.text().to_string())
}

/// The label of a made impl: the header with no attributes and no items. `implicit` gets the
/// name of a `Drop`, `Deref`, or `DerefMut` trait. `inner_attrs` gets the attributes of the
/// functions and the constants of the impl. `None`: Semilla cannot read the header.
fn impl_label(value: &semilla_syntax::rs::ast::Impl, implicit: &mut Option<String>, inner_attrs: &mut Vec<(String, String)>) -> Option<String> {
    use semilla_syntax::rs::{AstNode, ast};
    if let Some(path) = value.trait_() {
        let last = path
            .syntax()
            .descendants()
            .filter_map(ast::NameRef::cast)
            .filter(|name| {
                name.syntax()
                    .ancestors()
                    .all(|ancestor| ancestor.kind() != semilla_syntax::rs::SyntaxKind::GENERIC_ARG_LIST)
            })
            .last();
        *implicit = last
            .map(|name| name.text().to_string())
            .filter(|name| matches!(name.as_str(), "Drop" | "Deref" | "DerefMut"));
    }
    inner_attrs.extend(
        value
            .assoc_item_list()
            .into_iter()
            .flat_map(|list| list.assoc_items())
            .flat_map(|inner| match inner {
                ast::AssocItem::Fn(inner) => semilla_syntax::load::attribute_texts(inner.syntax()),
                ast::AssocItem::Const(inner) => semilla_syntax::load::attribute_texts(inner.syntax()),
                _ => Vec::new(),
            }),
    );
    let mut skip: Vec<semilla_syntax::rs::TextRange> = value
        .syntax()
        .children()
        .filter(|child| {
            matches!(
                child.kind(),
                semilla_syntax::rs::SyntaxKind::ATTR | semilla_syntax::rs::SyntaxKind::DOC_COMMENT
            )
        })
        .map(|child| child.text_range())
        .collect();
    skip.extend(value.assoc_item_list().map(|list| list.syntax().text_range()));
    let header = semilla_syntax::tt::text_without(value.syntax(), &skip);
    Some(tidy(&header.parse().ok()?))
}

/// Reads one made item from its expanded text. `None`: Semilla cannot read the text.
fn shown(item: &MadeItem) -> Option<Shown> {
    use ast::{HasName, HasVisibility};
    use semilla_syntax::rs::{AstNode, SyntaxNode, ast};
    use semilla_syntax::tt::TokenStream;
    // The compiler prints the crate of a macro as `$crate`.
    let text = item.text.replace("$crate", "crate");
    let parsed = semilla_syntax::rs::parse_item(&text)?;
    let vis = |vis: Option<ast::Visibility>| match vis {
        None => String::new(),
        Some(other) => format!("{} ", tidy(&TokenStream::from_node(other.syntax()))),
    };
    let attrs_of = |node: &SyntaxNode| semilla_syntax::load::attribute_texts(node);
    let mut implicit = None;
    let mut inner_attrs: Vec<(String, String)> = Vec::new();
    let label = match &parsed {
        ast::Item::Fn(value) if item.test => name_text(value),
        ast::Item::Fn(value) => format!("{}fn {}", vis(value.visibility()), name_text(value)),
        ast::Item::Impl(value) => impl_label(value, &mut implicit, &mut inner_attrs)?,
        ast::Item::Struct(value) => format!("{}struct {}", vis(value.visibility()), name_text(value)),
        ast::Item::Enum(value) => format!("{}enum {}", vis(value.visibility()), name_text(value)),
        ast::Item::Union(value) => format!("{}union {}", vis(value.visibility()), name_text(value)),
        ast::Item::Trait(value) => format!("{}trait {}", vis(value.visibility()), name_text(value)),
        ast::Item::TypeAlias(value) => format!("{}type {}", vis(value.visibility()), name_text(value)),
        ast::Item::Const(value) => format!(
            "{}const {}",
            vis(value.visibility()),
            value.name().map_or_else(|| "_".into(), |name| name.text().to_string())
        ),
        ast::Item::Static(value) => format!("{}static {}", vis(value.visibility()), name_text(value)),
        ast::Item::Module(value) => format!("{}mod {}", vis(value.visibility()), name_text(value)),
        ast::Item::Use(value) => format!(
            "{}use {}",
            vis(value.visibility()),
            tidy(&TokenStream::from_node(value.use_tree()?.syntax()))
        ),
        ast::Item::ExternCrate(value) => format!("extern crate {}", value.name_ref()?.text()),
        ast::Item::ExternBlock(value) => {
            let abi = value
                .abi()
                .and_then(|abi| abi.abi_string())
                .and_then(|text| text.value().ok().map(std::borrow::Cow::into_owned));
            format!("extern {} block", abi.unwrap_or_else(|| "C".into()))
        }
        ast::Item::MacroRules(value) => format!("macro_rules! {}", name_text(value)),
        _ => return None,
    };
    let attrs = attrs_of(parsed.syntax());
    // The impls of a derive have the attributes of the compiler. A derive is not in the
    // spotlight on an item with no macro, so it is not in the spotlight here.
    let derived = attrs.iter().any(|(path, _)| path == "automatically_derived");
    let texts: Vec<String> = if derived {
        Vec::new()
    } else {
        attrs.into_iter().chain(inner_attrs).map(|(_, text)| text).collect()
    };
    let attributes = if item.test { Vec::new() } else { crate::facts::new_attributes(&texts) };
    let has_unsafe = !derived && text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == "unsafe");
    Some(Shown {
        label,
        key: (item.module.clone(), item.text.clone()),
        public: item.public,
        attributes,
        has_unsafe,
        implicit,
    })
}

/// The items that a macro call makes, in the order of the place and then of the text (the same
/// order in each run). `None`: Semilla cannot list them.
fn items_of(call: &Item, made: Option<&Made>) -> Option<Vec<Shown>> {
    let made = made?;
    let mut found: Vec<&MadeItem> = made
        .items
        .iter()
        .filter(|item| in_range(call, (&item.file, item.line, item.col)))
        .collect();
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
fn list_text(items: Option<&[Shown]>, word: &str) -> String {
    let Some(items) = items else {
        return format!("{word} items that Semilla cannot list");
    };
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
        let _ = write!(text, ", and {} more", labels.len() - 5);
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

/// The made items of one side of a change: `None` for no call on the side, `Some(None)` if
/// Semilla cannot list the items.
type SideItems = Option<Option<Vec<Shown>>>;

/// The items that one changed macro call adds and removes.
struct CallItems<'a, 's> {
    call: &'a Item,
    /// The call text after the step, or before it for a removed call.
    name: String,
    /// The call text before the step.
    old_name: String,
    added: Vec<&'s Shown>,
    removed: Vec<&'s Shown>,
}

/// The state of [`apply`]: the trees, the expanded code, the report, and the outcome.
struct Apply<'a, 'r> {
    step: &'a Step,
    before: &'a Workspace,
    after: &'a Workspace,
    made: Option<&'a [Made; 2]>,
    report: &'r mut StepReport,
    outcome: Outcome,
}

/// Gives the facts of the macro edits their text, and applies the rules of an item with no
/// macro to the items that a changed macro call makes.
///
/// `made` has the expanded code of the tree before the step and of the tree after it. `None`:
/// the oracle cannot give it. Then a changed call "makes items that Semilla cannot list": the
/// fact is in the spotlight, and the API level of a call in library code is `major`.
pub(crate) fn apply(
    step: &Step,
    edits: &[Edit],
    fact_of: &[Option<usize>],
    before: &Workspace,
    after: &Workspace,
    made: Option<&[Made; 2]>,
    report: &mut StepReport,
) -> Outcome {
    let mut state = Apply {
        step,
        before,
        after,
        made,
        report,
        outcome: Outcome::default(),
    };
    let mut changes: Vec<Change> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        state.edit_changes(edit, fact_of.get(index).copied().flatten(), &mut changes);
    }
    for change in changes {
        state.apply_change(&change);
    }
    state.outcome
}

impl<'a> Apply<'a, '_> {
    /// Adds the changes of macro calls of one edit to `changes`. `fact`: the fact of the edit.
    fn edit_changes(&mut self, edit: &Edit, fact: Option<usize>, changes: &mut Vec<Change<'a>>) {
        let (before, after) = (self.before, self.after);
        let whole = |anchor: &Option<Anchor>| anchor.as_ref().is_some_and(|anchor| anchor.path.is_empty());
        let old = edit
            .before
            .as_ref()
            .filter(|_| whole(&edit.before))
            .and_then(|anchor| before.items.get(&anchor.item));
        let new = edit
            .after
            .as_ref()
            .filter(|_| whole(&edit.after))
            .and_then(|anchor| after.items.get(&anchor.item));
        // The fact of the item itself: the text names the item.
        let own = |item: &Item, report: &StepReport| {
            fact.and_then(|fact| report.facts.get(fact))
                .is_some_and(|fact| fact.part == "edit" && fact.at.as_ref().is_some_and(|at| at.item == item.path && at.path.is_empty()))
        };
        let (old, new) = match edit.kind {
            EditKind::ItemAdded => (None, new),
            EditKind::ItemRemoved => (old, None),
            EditKind::Docs | EditKind::Comment | EditKind::Attrs | EditKind::Visibility => return,
            _ if edit.text_before.is_some() && edit.text_after.is_some() => (old, new),
            _ => return,
        };
        let Some(item) = new.or(old) else { return };
        if item.kind != ItemKind::Macro {
            // An added or removed module: each macro call in it follows the rules, and the
            // fact of the module keeps its text.
            if old.is_some() != new.is_some() {
                let ws = if new.is_some() { after } else { before };
                let prefix = format!("{}::", item.path);
                changes.extend(
                    ws.items
                        .values()
                        .filter(|call| is_call(call) && call.path.starts_with(&prefix))
                        .map(|call| Change {
                            old: old.map(|_| call),
                            new: new.map(|_| call),
                            fact,
                            direct: false,
                        }),
                );
            }
            return;
        }
        let direct = own(item, self.report);
        if crate::owners::is_macro_definition(item) {
            if direct && let Some(fact) = fact {
                self.definition_text(item, fact);
            }
            return;
        }
        if old.is_some_and(|item| !is_call(item)) || new.is_some_and(|item| !is_call(item)) {
            return;
        }
        changes.push(Change { old, new, fact, direct });
    }

    /// Gives the fact of an added, removed, or changed macro definition its text.
    fn definition_text(&mut self, item: &Item, fact: usize) {
        let Some(fact) = self.report.facts.get_mut(fact) else {
            return;
        };
        let name = definition_name(item);
        let path = format!("`{}`", item.path);
        // The text with no public API note at its end.
        let plain = fact.description.split(" (the public API ").next().unwrap_or_default().to_string();
        if plain == format!("added {path}") {
            fact.description = format!("added macro `{name}`{}", std_macro_text(&name, item, self.after, true));
        } else if plain == format!("removed {path}") {
            fact.description = format!("removed macro `{name}`{}", std_macro_text(&name, item, self.before, false));
        } else if let Some(rest) = fact.description.strip_prefix("changed macro or signature ") {
            fact.description = format!("changed macro `{name}`: {rest}");
        }
    }

    /// Applies the rules of an item with no macro to the items of one changed call.
    fn apply_change(&mut self, change: &Change<'a>) {
        let olds: SideItems = change.old.map(|call| items_of(call, self.made.map(|made| &made[0])));
        let news: SideItems = change.new.map(|call| items_of(call, self.made.map(|made| &made[1])));
        let call = change.new.or(change.old).expect("a change has a call");
        let name = call_text(call);
        // The text of the fact.
        self.call_fact_text(change, olds.as_ref(), news.as_ref(), &name);
        // The rules of an item with no macro.
        let mut whys: Vec<String> = Vec::new();
        let sides = [olds.as_ref(), news.as_ref()];
        if sides.iter().flatten().any(|side| side.is_none()) {
            whys.push(format!("the macro call `{name}` makes items that Semilla cannot list"));
            let item_of_api = !call.test && crate::in_library(if change.new.is_some() { self.after } else { self.before }, call);
            let line = format!("macro call `{name}`: it makes items that Semilla cannot list (the public API can change)");
            if item_of_api && !self.outcome.major.contains(&line) {
                self.outcome.major.push(line);
            }
        }
        let empty = Vec::new();
        let old_items = olds.as_ref().and_then(|side| side.as_ref()).unwrap_or(&empty);
        let new_items = news.as_ref().and_then(|side| side.as_ref()).unwrap_or(&empty);
        let added: Vec<&Shown> = new_items.iter().filter(|item| !old_items.iter().any(|old| old.key == item.key)).collect();
        let removed: Vec<&Shown> = old_items.iter().filter(|item| !new_items.iter().any(|new| new.key == item.key)).collect();
        // A removed item names the call of the tree before the step.
        let old_name = change.old.map_or_else(|| name.clone(), call_text);
        let items = CallItems {
            call,
            name,
            old_name,
            added,
            removed,
        };
        for item in &items.removed {
            let line = format!("removed `{}` (the macro call `{}` made it)", item.label, items.old_name);
            if item.public && !self.outcome.major.contains(&line) {
                self.outcome.major.push(line);
            }
        }
        self.wrapped_facts(&items);
        self.added_rules(&items, &mut whys);
        self.hidden_rules(&items, &mut whys);
        if whys.is_empty() {
            return;
        }
        self.spotlight(change, &items, whys.join("; "));
    }

    /// Gives the fact of a macro call that the step adds, removes, or changes the text that
    /// lists the made items.
    fn call_fact_text(&mut self, change: &Change, olds: Option<&Option<Vec<Shown>>>, news: Option<&Option<Vec<Shown>>>, name: &str) {
        if change.direct
            && let Some(index) = change.fact
            && let Some(fact) = self.report.facts.get_mut(index)
        {
            let text = match (olds, news) {
                (None, Some(new)) => Some(format!("added macro call `{name}` ({})", list_text(new.as_deref(), "makes"))),
                (Some(old), None) => Some(format!("removed macro call `{name}` ({})", list_text(old.as_deref(), "made"))),
                (Some(old), Some(new)) => {
                    let sides = format!("changed macro call `{}` → `{name}`", change.old.map_or_default(call_text));
                    let (made, makes) = (list_text(old.as_deref(), "made"), list_text(new.as_deref(), "makes"));
                    Some(if made.strip_prefix("made") == makes.strip_prefix("makes") {
                        format!("{sides} ({makes})")
                    } else {
                        format!("{sides} ({made}; {makes})")
                    })
                }
                (None, None) => None,
            };
            let known = fact.description.starts_with("added `")
                || fact.description.starts_with("removed `")
                || fact.description.starts_with("changed macro or signature ");
            if let Some(text) = text.filter(|_| known) {
                fact.description = text;
                self.outcome.listed.insert(index);
            }
        }
    }

    /// An item of the source that a macro call makes now, or the reverse. The item is not
    /// removed and not added: its fact names the macro call. The compare of its body, if it
    /// has one, is the compare of the fact.
    fn wrapped_facts(&mut self, items: &CallItems) {
        let call = items.call;
        let wrapped = |item: &Shown, word: &str| -> Option<String> {
            let mut words = item.label.split_whitespace().rev();
            let (ident, kind) = (words.next()?, words.next()?);
            // The made item is in the module of the macro call.
            let module = call.path.rsplit_once("::").map(|(module, _)| module)?;
            ["fn", "struct", "enum", "union", "trait", "type", "const", "static"]
                .contains(&kind)
                .then(|| format!("{word} `{module}::{ident}`"))
        };
        let macro_name = |text: &str| format!("{}!", text.split('!').next().unwrap_or(text).trim());
        for (list, word, call_name) in [(&items.added, "removed", &items.name), (&items.removed, "added", &items.old_name)] {
            for head in list.iter().filter_map(|item| wrapped(item, word)) {
                let whole = |fact: &semilla_core::Fact| fact.description == head || fact.description.starts_with(&format!("{head} ("));
                let Some(fact) = self
                    .report
                    .facts
                    .iter_mut()
                    .find(|fact| matches!(fact.category, Category::Added | Category::Removed) && whole(fact))
                else {
                    continue;
                };
                let path = head.split('`').nth(1).unwrap_or_default().to_string();
                fact.description = if word == "removed" {
                    format!("`{path}` comes from the macro call `{}` now", macro_name(call_name))
                } else {
                    format!(
                        "`{path}` came from the macro call `{}` before, and it is an item of the source now",
                        macro_name(call_name)
                    )
                };
                fact.category = Category::Macro;
            }
        }
    }

    /// The rules of a new item: the public API list, and an attribute that can change the
    /// behavior.
    fn added_rules(&mut self, items: &CallItems, whys: &mut Vec<String>) {
        let name = &items.name;
        for item in &items.added {
            let line = format!("added `{}` (the macro call `{name}` makes it)", item.label);
            if item.public && !self.outcome.minor.contains(&line) {
                self.outcome.minor.push(line);
            }
            if !item.attributes.is_empty() {
                let names = item.attributes.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
                whys.push(format!(
                    "a new item with an attribute that can change the behavior: {names} on `{}`, which the macro call `{name}` makes",
                    item.label
                ));
            }
        }
    }

    /// The rules of `unsafe` code and of an impl that the compiler calls with no call in the
    /// text, for each added and each removed item.
    fn hidden_rules(&mut self, items: &CallItems, whys: &mut Vec<String>) {
        let call = items.call;
        let added = items.added.iter().map(|item| (item, "makes", &items.name));
        for (item, word, name) in added.chain(items.removed.iter().map(|item| (item, "made", &items.old_name))) {
            if item.has_unsafe {
                let line = format!("unsafe code: {} (the macro call `{name}` {word} `{}`)", call.path, item.label);
                if !self.outcome.spotlight.contains(&line) {
                    self.outcome.spotlight.push(line);
                }
            }
            if let Some(trait_name) = &item.implicit {
                whys.push(format!(
                    "an implicit call: the macro call `{name}` {word} `{}`. The compiler calls `{trait_name}` with no call in the text, so the MIR of the callers does not show the change",
                    item.label
                ));
                let line = format!(
                    "implicit call: {} (the macro call `{name}` {word} `{}`; the MIR of the callers does not show it)",
                    call.path, item.label
                );
                if !self.outcome.spotlight.contains(&line) {
                    self.outcome.spotlight.push(line);
                }
            }
        }
    }

    /// Puts the fact of the change in the spotlight with the reason `why`, or adds a new fact
    /// for the call if the change has no fact.
    fn spotlight(&mut self, change: &Change, items: &CallItems, why: String) {
        let (step, call, name) = (self.step, items.call, &items.name);
        if let Some(fact) = change.fact.and_then(|index| self.report.facts.get_mut(index)) {
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
                fact.reason = step
                    .reasons
                    .get(&fact.reason_place())
                    .cloned()
                    .or_else(|| step.reasons.get(&call.path).cloned());
            }
        } else {
            let id = format!("f{}", self.report.facts.len() + 1);
            self.report.facts.push(semilla_core::Fact {
                id,
                at: Some(Anchor::item(&call.path)),
                file: Some(call.file.clone()),
                part: "macro call".into(),
                description: format!("items of the macro call `{name}`"),
                status: "derived".into(),
                category: Category::Macro,
                spotlight: true,
                spotlight_why: Some(why),
                reason: step.reasons.get(&call.path).cloned(),
                ..Default::default()
            });
        }
    }
}
