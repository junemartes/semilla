//! The facts of the expanded code (batch 10c, design 63, part B).
//!
//! Expanded code is code (the decision of the user). The driver gives the expansion of each
//! macro call as text (R63.1-1). For each call site, this pass pairs the expansion before the
//! step with the expansion after it. Two equal texts are `expansion equal`. For two different
//! texts, the pass writes each text as a virtual file of a virtual crate and runs the normal
//! pipeline of Semilla on the two crates: the lossless parse, the tree compare, the fact rules,
//! the categories, and the effect hints (R63.2-1). Each fact of that compare is a line of the
//! call site, with the place of the call and the path in the expansion.
//!
//! The owner of a changed expansion (R63.3-2): each changed macro definition of its chain, and
//! each fact with an edit in the input of the call (the tokens of a function-like call; the
//! item of a derive or of an attribute). The lines go under the facts of the owners
//! (R63.3-3). The step section lists each changed, added, and removed expansion (R63.3-5).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use semilla_core::report::{ExpansionSite, Position};
use semilla_core::{Category, Step};
use semilla_syntax::Workspace;

use crate::{ExprOracle, NoOracle, StepInput, StepOutcome};

/// One macro call and its expanded code, in the form of the driver (R63.1-1).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Expansion {
    /// The place of the call: the file, the byte range, the first and the last line and
    /// column. A call inside an expansion has the place of its tokens, or no place.
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub r: [i64; 2],
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub col: usize,
    #[serde(default)]
    pub end_line: usize,
    #[serde(default)]
    pub end_col: usize,
    /// The index of the expansion that has this call in its code.
    #[serde(default)]
    pub outer: Option<usize>,
    pub name: String,
    /// `bang`, `attr`, or `derive`.
    pub kind: String,
    /// `src/lib.rs:2` for a macro of the workspace, else `crate NAME`.
    pub def: String,
    /// `expr`, `stmts`, `items`, `assoc`, `pat`, or `ty`.
    pub form: String,
    #[serde(default)]
    pub container: String,
    #[serde(default)]
    pub call: String,
    pub text: String,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub uses: Vec<[String; 2]>,
    #[serde(default)]
    pub module: String,
}

/// The expansions of one compiled target in one checked configuration.
#[derive(Clone, Debug, Default)]
pub struct ExpansionUnit {
    /// The name of the configuration (`dev`, `release`).
    pub config: String,
    /// The target of the unit record: `<crate>.<kind>.<hash>`.
    pub target: String,
    /// The package of the target.
    pub package: String,
    pub list: Vec<Expansion>,
}

impl ExpansionUnit {
    /// The crate name of the unit.
    pub fn crate_name(&self) -> &str {
        self.target.split('.').next().unwrap_or("")
    }

    /// True if two units are the same target in the same configuration: the package, the crate
    /// name, and the kind. The hash of the target is not a part: a changed manifest (a new
    /// version of a dependency) changes it.
    pub fn same_target(&self, other: &ExpansionUnit) -> bool {
        let short = |target: &str| target.splitn(3, '.').take(2).collect::<Vec<_>>().join(".");
        self.config == other.config && self.package == other.package && short(&self.target) == short(&other.target)
    }
}

/// The most lines of the diff of an expansion in a stored step and in the views (R63.5-3).
pub const DIFF_LIMIT: usize = 40;

/// The result of one call site over each configuration.
struct Site<'a> {
    old: Option<&'a Expansion>,
    new: Option<&'a Expansion>,
    /// The anchor of the call on each side.
    at: [Option<semilla_core::Anchor>; 2],
    crate_name: String,
    configs: Vec<String>,
}

/// The place of a call site in its tree: the anchor of the smallest node at the start.
fn anchor(ws: &Workspace, expansion: &Expansion) -> Option<semilla_core::Anchor> {
    if expansion.file.is_empty() {
        return None;
    }
    semilla_syntax::anchor_at(ws, &expansion.file, expansion.line, Some(expansion.col)).or_else(|| semilla_syntax::anchor_at(ws, &expansion.file, expansion.line, None))
}

/// The key of a call site for the pair of the two trees (R63.2-2): the item that holds the call
/// (with the rename map of the step), the macro, the kind, and the number of the call among
/// the calls of the same macro in that item, in source order.
fn keys<'a>(list: &'a [Expansion], ws: &Workspace, map: &dyn Fn(&str) -> String) -> Vec<((String, String, String, usize), &'a Expansion, Option<semilla_core::Anchor>)> {
    let mut found: Vec<(&Expansion, Option<semilla_core::Anchor>)> = list.iter().filter(|expansion| expansion.outer.is_none()).map(|expansion| (expansion, anchor(ws, expansion))).collect();
    found.sort_by(|a, b| (a.0.file.as_str(), a.0.line, a.0.col, a.0.r).cmp(&(b.0.file.as_str(), b.0.line, b.0.col, b.0.r)));
    let mut counts: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    found
        .into_iter()
        .map(|(expansion, at)| {
            let item = at.as_ref().map(|at| map(&at.item)).unwrap_or_else(|| format!("{}:{}", expansion.file, expansion.module));
            let base = (item, expansion.name.clone(), expansion.kind.clone());
            let count = counts.entry(base.clone()).or_default();
            let key = (base.0, base.1, base.2, *count);
            *count += 1;
            (key, expansion, at)
        })
        .collect()
}

/// A folder that the pass removes at its end.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The text of the virtual file of an expansion (R63.2-1): the code in the form that the parser
/// of a source file takes, inside the modules of the call. An expression or statements are the
/// body of the function `expansion`; associated items are in their impl or trait.
pub fn virtual_file(expansion: &Expansion) -> String {
    let body = match expansion.form.as_str() {
        "expr" => format!("fn expansion() {{\n{}\n}}\n", expansion.text),
        // The `;` of a statement call (`guard!(v);`) is the end of the last statement of the
        // expansion: `if … { … };`. The virtual file has no such `;` after a block, so a new
        // statement before it is not a changed statement (the compare of the raw texts keeps
        // it: R63.2-1 changes only the virtual file).
        "stmts" => format!("fn expansion() {{\n{}\n}}\n", expansion.text.strip_suffix("};").map(|text| format!("{text}}}")).unwrap_or_else(|| expansion.text.clone())),
        "assoc" => format!("{} {{\n{}\n}}\n", expansion.container, expansion.text),
        "pat" => format!("fn expansion() {{\nlet {} = ();\n}}\n", expansion.text),
        "ty" => format!("type Expansion = {};\n", expansion.text),
        _ => format!("{}\n", expansion.text),
    };
    let mut text = body;
    for module in expansion.module.split("::").filter(|part| !part.is_empty()).collect::<Vec<_>>().into_iter().rev() {
        text = format!("mod {module} {{\n{text}}}\n");
    }
    text
}

/// The facts of the normal compare of two expansions (R63.2-1): each fact text with its
/// category, in the order of the facts. The text names the function of the virtual file
/// `expansion`, not a path of the virtual crate.
pub fn compare(old: &Expansion, new: &Expansion, crate_name: &str, exprs: &dyn ExprOracle) -> Result<Vec<(String, Category)>, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!("semilla-expansion-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _scratch = Scratch(root.clone());
    let name = if crate_name.is_empty() { "expanded" } else { crate_name };
    let write = |side: &str, expansion: &Expansion| -> std::io::Result<PathBuf> {
        let dir = root.join(side);
        std::fs::create_dir_all(dir.join("src"))?;
        std::fs::write(dir.join("Cargo.toml"), format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n"))?;
        std::fs::write(dir.join("src/lib.rs"), virtual_file(expansion))?;
        Ok(dir)
    };
    let load = |dir: &Path| Workspace::load(dir).map_err(|error| format!("{error:#}"));
    let before = load(&write("old", old).map_err(|error| error.to_string())?)?;
    let after = load(&write("new", new).map_err(|error| error.to_string())?)?;
    let step = Step { index: 0, intent: "expansion".into(), claims: Vec::new(), tree_before: "old".into(), tree_after: "new".into(), reasons: Default::default(), compiled: false, build_check: None, agent: Default::default(), tags: Vec::new() };
    let outcome = crate::verify_step_with(&StepInput { step: &step, before: &before, after: &after }, &NoOracle, exprs, true);
    let module = if new.module.is_empty() { String::new() } else { format!("::{}", new.module) };
    let function = format!("{name}{module}::expansion");
    let mut out = Vec::new();
    // The layout of the expanded code is the layout of the printer of the compiler, not of the
    // author: a layout fact is not a fact of the expansion (R63.3-2).
    for fact in outcome.report.facts.iter().filter(|fact| fact.category != Category::Layout) {
        let mut text = fact.description.clone();
        if matches!(new.form.as_str(), "expr") {
            text = text.replace(&format!("`{function}/tail/"), "`expansion/").replace(&format!("`{function}/tail`"), "`expansion`");
        }
        text = text.replace(&format!("`{function}/"), "`expansion/").replace(&format!("`{function}`"), "`expansion`");
        out.push((one_line(&text), fact.category));
    }
    Ok(out)
}

/// A fact text on one line: the pretty printer of the compiler breaks a long line, and the
/// fact text shows each line end as `⏎`. A line end in the expanded code is a layout of the
/// printer, not of the author.
fn one_line(text: &str) -> String {
    let mut out = text.replace("(⏎", "(").replace("⏎)", ")").replace("`⏎", "`").replace("⏎`", "`").replace("[⏎", "[").replace("⏎]", "]").replace("{⏎", "{ ").replace("⏎}", " }").replace('⏎', " ");
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out
}

/// The text of a definition for a line: `macro_rules! base` (src/lib.rs:1), `derive
/// PartialEq` (crate core).
fn definition_text(expansion: &Expansion) -> String {
    let word = match expansion.kind.as_str() {
        "derive" => "derive",
        "attr" => "attribute macro",
        _ if expansion.def.starts_with("crate ") => "macro",
        _ => "macro_rules!",
    };
    format!("`{word} {}` ({})", expansion.name, expansion.def)
}

/// The changed definitions of the chain of a call (R63.3-2): the facts of a changed macro
/// definition whose name is the macro of the call or a macro that its code uses, at each depth.
fn chain_owners(outcome: &StepOutcome, sides: [Option<&Expansion>; 2]) -> (Vec<usize>, Vec<String>) {
    let mut names: BTreeSet<(String, String)> = BTreeSet::new();
    for expansion in sides.into_iter().flatten() {
        names.insert((expansion.name.clone(), expansion.def.clone()));
        for [name, def] in &expansion.uses {
            names.insert((name.clone(), def.clone()));
        }
    }
    let mut facts = Vec::new();
    let mut causes = Vec::new();
    for (index, fact) in outcome.report.facts.iter().enumerate() {
        let Some(item) = fact.at.as_ref().map(|at| at.item.as_str()) else { continue };
        let short = item.rsplit("::").next().unwrap_or(item);
        let Some(name) = short.strip_suffix('!') else { continue };
        let Some((_, def)) = names.iter().find(|(known, def)| known == name && !def.starts_with("crate ")) else { continue };
        // The fact must be the fact of a definition (a call has `!#N`), in the file of the
        // definition.
        let file = def.rsplit_once(':').map(|(file, _)| file).unwrap_or(def);
        if fact.file.as_deref().is_some_and(|known| known != file) {
            continue;
        }
        facts.push(index);
        let cause = format!("`macro_rules! {name}` ({def})");
        if !causes.contains(&cause) {
            causes.push(cause);
        }
    }
    (facts, causes)
}

/// The lines of the input of a call on one side: the lines of the call for a function-like
/// call, the lines of the item for a derive or an attribute (its input is the item).
fn input_lines(ws: &Workspace, expansion: &Expansion, at: Option<&semilla_core::Anchor>) -> (String, usize, usize) {
    if expansion.kind != "bang"
        && let Some(item) = at.and_then(|at| ws.items.get(&at.item))
    {
        return (item.file.clone(), item.line_start, item.line_end);
    }
    (expansion.file.clone(), expansion.line, expansion.end_line.max(expansion.line))
}

fn holds(positions: &[Position], old: bool, file: &str, start: usize, end: usize) -> bool {
    positions.iter().any(|position| position.file == file && position.old == old && position.start <= end && start <= position.end)
}

/// A small line diff of two texts (R63.5-1): `  ` for an equal line, `- ` for a line of the
/// old text, `+ ` for a line of the new text. A text of more than 2000 lines gives the two
/// texts in full.
pub fn line_diff(old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len() * b.len() > 4_000_000 {
        return a.iter().map(|line| format!("- {line}")).chain(b.iter().map(|line| format!("+ {line}"))).collect();
    }
    let mut table = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push(format!("  {}", a[i]));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || table[i + 1][j] >= table[i][j + 1]) {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    out
}

/// The diff of a call for a view: the changed lines with two lines of context, at most
/// `limit` lines, then a line that points to `semilla expand`.
pub fn cut_diff(diff: &[String], limit: usize, command: &str) -> Vec<String> {
    let near: Vec<bool> = (0..diff.len()).map(|index| (index.saturating_sub(2)..(index + 3).min(diff.len())).any(|other| !diff[other].starts_with("  "))).collect();
    let mut out = Vec::new();
    let mut gap = false;
    for (index, line) in diff.iter().enumerate() {
        if !near[index] {
            if !gap {
                out.push("  …".to_string());
                gap = true;
            }
            continue;
        }
        gap = false;
        out.push(line.clone());
    }
    if out.len() > limit {
        let more = out.len() - limit;
        out.truncate(limit);
        out.push(format!("… {more} more lines: {command}"));
    }
    out
}

/// The pairs of the call sites of the two trees (R63.2-2), joined over the units: one site for
/// each pair of texts, with the configurations. The second list has the pairs with equal
/// texts (with `keep_equal`, also joined; else one entry for each pair).
fn pair_sites<'a>(units: [&'a [ExpansionUnit]; 2], trees: [&Workspace; 2], map: &dyn Fn(&str) -> String, keep_equal: bool) -> (Vec<Site<'a>>, Vec<Site<'a>>) {
    let [old_units, new_units] = units;
    let same = |path: &str| path.to_string();
    let mut sites: Vec<Site> = Vec::new();
    let mut equal: Vec<Site> = Vec::new();
    let push = |list: &mut Vec<Site<'a>>, old: Option<&'a Expansion>, new: Option<&'a Expansion>, at: [Option<semilla_core::Anchor>; 2], unit: &ExpansionUnit, join: bool| {
        let found = if join { list.iter_mut().find(|site| site.old.map(|e| &e.text) == old.map(|e| &e.text) && site.new.map(|e| &e.text) == new.map(|e| &e.text) && site.at == at) } else { None };
        match found {
            Some(site) => {
                if !site.configs.contains(&unit.config) {
                    site.configs.push(unit.config.clone());
                }
            }
            None => list.push(Site { old, new, at, crate_name: unit.crate_name().to_string(), configs: vec![unit.config.clone()] }),
        }
    };
    for unit in new_units {
        let old = old_units.iter().find(|old| old.same_target(unit));
        let old_keys = old.map(|old| keys(&old.list, trees[0], map)).unwrap_or_default();
        let new_keys = keys(&unit.list, trees[1], &same);
        let mut pairs: Vec<(Option<usize>, Option<usize>)> = Vec::new();
        let mut used = vec![false; old_keys.len()];
        for (index, (key, _, _)) in new_keys.iter().enumerate() {
            let found = old_keys.iter().position(|(other, _, _)| other == key).filter(|at| !used[*at]);
            if let Some(at) = found {
                used[at] = true;
            }
            pairs.push((found, Some(index)));
        }
        pairs.extend(used.iter().enumerate().filter(|(_, used)| !**used).map(|(at, _)| (Some(at), None)));
        for (a, b) in pairs {
            let old = a.map(|a| &old_keys[a]);
            let new = b.map(|b| &new_keys[b]);
            let at = [old.and_then(|o| o.2.clone()), new.and_then(|n| n.2.clone())];
            let (old, new) = (old.map(|o| o.1), new.map(|n| n.1));
            if let (Some(o), Some(n)) = (old, new)
                && o.text == n.text
            {
                push(&mut equal, old, new, at, unit, keep_equal);
                continue;
            }
            push(&mut sites, old, new, at, unit, true);
        }
    }
    // A unit of the old tree with no unit in the new tree: each call is removed.
    for unit in old_units.iter().filter(|old| !new_units.iter().any(|new| new.same_target(old))) {
        for (_, expansion, at) in keys(&unit.list, trees[0], map) {
            push(&mut sites, Some(expansion), None, [at, None], unit, true);
        }
    }
    (sites, equal)
}

/// Adds the facts of the expanded code to a step of the compiled mode (R63.2-1 to R63.3-5).
pub(crate) fn apply(input: &StepInput, bodies: &dyn crate::BodyOracle, exprs: &dyn ExprOracle, outcome: &mut StepOutcome) {
    if !input.step.compiled {
        return;
    }
    let Some([old_units, new_units]) = bodies.expansions(input.step) else { return };
    let map = |path: &str| outcome.renames.items.get(path).cloned().unwrap_or_else(|| path.to_string());
    let (sites, equal) = pair_sites([&old_units, &new_units], [input.before, input.after], &map, false);
    let all_configs: BTreeSet<&str> = new_units.iter().chain(&old_units).map(|unit| unit.config.as_str()).collect();
    let mut flow = outcome.report.flow.take().unwrap_or_default();
    // A call of a library and of its test build, in each configuration, is one call.
    let equal: BTreeSet<(String, usize, usize)> = equal.iter().filter_map(|site| site.new).map(|e| (e.file.clone(), e.line, e.col)).collect();
    flow.expansions_equal = equal.len();
    let mut cache: BTreeMap<(String, String), Result<Vec<(String, Category)>, String>> = BTreeMap::new();
    let mut by_fact: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    for site in &sites {
        let any = site.new.or(site.old).expect("a site has a side");
        let (result, facts) = match (site.old, site.new) {
            (Some(old), Some(new)) => {
                let found = cache.entry((old.text.clone(), new.text.clone())).or_insert_with(|| compare(old, new, &site.crate_name, exprs));
                let facts: Vec<String> = match found {
                    Ok(list) if list.is_empty() => vec!["no fact: the two texts differ only in the layout".to_string()],
                    Ok(list) => list.iter().map(|(text, category)| format!("{text} [{}]", category.name())).collect(),
                    Err(error) => vec![format!("not compared: {error}")],
                };
                ("expansion changed", facts)
            }
            (None, Some(new)) => ("expansion added", vec![made_text("makes", new)]),
            (Some(old), None) => ("expansion removed", vec![made_text("made", old)]),
            (None, None) => continue,
        };
        let side = if site.new.is_some() { 1 } else { 0 };
        let at = site.at[side].as_ref().map(|at| at.to_string()).unwrap_or_else(|| format!("{} (no item)", any.module));
        let place = format!("{}:{}{}", any.file, if side == 0 { "-" } else { "" }, any.line);
        // A changed definition owns a changed expansion; an added or a removed call belongs to
        // the edit of the call.
        let (mut owners, causes) = if site.old.is_some() && site.new.is_some() { chain_owners(outcome, [site.old, site.new]) } else { (Vec::new(), Vec::new()) };
        // The facts with an edit in the input of the call.
        let mut input_owner = false;
        for (side, expansion) in [site.old, site.new].iter().enumerate() {
            let Some(expansion) = expansion else { continue };
            let ws = if side == 0 { input.before } else { input.after };
            let (file, start, end) = input_lines(ws, expansion, site.at[side].as_ref());
            for (index, fact) in outcome.report.facts.iter().enumerate() {
                if holds(&fact.positions, side == 0, &file, start, end) && !owners.contains(&index) {
                    owners.push(index);
                    input_owner = true;
                }
            }
        }
        owners.sort();
        let mut cause = causes.iter().map(|cause| format!("from {cause}")).collect::<Vec<_>>().join(", ");
        if input_owner {
            let word = match result {
                "expansion added" => "the step adds the call or its input",
                "expansion removed" => "the step removes the call or its input",
                _ => "the input of the call changes",
            };
            cause = if cause.is_empty() { word.to_string() } else { format!("{cause}; {word}") };
        }
        // No changed definition and no changed input (a file that `include_bytes!` reads, a
        // change of a dependency): the facts of the body that holds the call own it (R63.3-2).
        if owners.is_empty() {
            let holder = site.at[1].as_ref().or(site.at[0].as_ref()).map(|at| at.item.clone()).unwrap_or_default();
            owners.extend(outcome.report.facts.iter().enumerate().filter(|(_, fact)| !holder.is_empty() && fact.affected.contains(&holder)).map(|(index, _)| index));
            cause = "no changed definition and no changed input of the call".to_string();
        }
        let configs = if site.configs.len() == all_configs.len() { Vec::new() } else { site.configs.clone() };
        let shown_call = if any.kind == "derive" { format!("derive `{}`", any.name) } else { format!("`{}`", any.call) };
        let summary = facts.join("; ");
        let key = format!("{result}: {}: {summary}{}", definition_text(any), if cause.is_empty() { String::new() } else { format!("; {cause}") });
        let diff = cut_diff(&line_diff(site.old.map(|e| e.text.as_str()).unwrap_or(""), site.new.map(|e| e.text.as_str()).unwrap_or("")), DIFF_LIMIT, &format!("semilla expand {place}"));
        let owner_ids: Vec<String> = owners.iter().map(|index| outcome.report.facts[*index].id.clone()).collect();
        for id in &owner_ids {
            let prefix = if any.kind == "derive" { format!("derived `{}` of `{at}`", any.name) } else { format!("{shown_call} at `{at}` ({place})") };
            let line = format!("{prefix}: {result}: {summary}");
            let fold = format!("{}|{result}|{summary}", any.name);
            by_fact.entry(id.clone()).or_default().push((fold, line, at.clone()));
        }
        // The compiled check of the expanded code (R63.3-9): the line of each body that holds
        // the call or that the expansion makes, from the control flow section (60, 61).
        let holder = site.at[side].as_ref().map(|at| at.item.clone()).unwrap_or_default();
        // A body of a derive is a method of the impl of its trait (`<shop::Point as
        // std::fmt::Debug>::fmt`); two derives of one type have one place.
        let at_call = |body: &semilla_core::report::FlowBody| if any.kind == "derive" { body.place == place && body.name.contains(&format!("::{}>", any.name)) } else { body.name == holder || body.place == place };
        let bodies: Vec<String> = flow.bodies.iter().filter(|body| at_call(body)).map(|body| format!("{} ({}): {}", body.name, body.place, body.line)).collect();
        // A body line with no edit names the changed expansion at its place (R63.3-9).
        if result == "expansion changed" {
            let mark = format!("; the expansion of {shown_call} changes (semilla expand {place})");
            for body in flow.bodies.iter_mut().filter(|body| at_call(body) && body.line.contains("no edit in the body") && !body.line.contains("; the expansion of ")) {
                body.line.push_str(&mark);
            }
        }
        flow.expansions.push(ExpansionSite { call: shown_call, at, place, definition: definition_text(any), result: result.to_string(), configs, facts, cause, owners: owner_ids, diff, key, bodies });
    }
    // The lines of each fact (R63.3-3): equal changes of one macro fold into one line with the
    // count (R63.3-4).
    for (id, lines) in by_fact {
        let mut folded: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for (fold, line, at) in lines {
            match folded.iter_mut().find(|(key, _)| *key == fold) {
                Some((_, list)) => list.push((line, at)),
                None => folded.push((fold, vec![(line, at)])),
            }
        }
        let mut out: Vec<String> = folded
            .into_iter()
            .map(|(fold, list)| {
                if list.len() == 1 {
                    return list[0].0.clone();
                }
                let mut parts = fold.splitn(3, '|');
                let (name, result, summary) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let places: Vec<String> = list.iter().take(semilla_core::report::REACH_FOLD_NAMES).map(|(_, at)| format!("`{at}`")).collect();
                let more = if list.len() > semilla_core::report::REACH_FOLD_NAMES { format!(", and {} more", list.len() - semilla_core::report::REACH_FOLD_NAMES) } else { String::new() };
                format!("{} calls of `{name}`: {result}: {summary}: {}{more} (semilla status --verbose)", list.len(), places.join(", "))
            })
            .collect();
        let more = out.len().saturating_sub(crate::flow::FACT_LINES);
        out.truncate(crate::flow::FACT_LINES);
        if more > 0 {
            out.push(format!("… {more} more (semilla status --verbose)"));
        }
        flow.expansion_lines.insert(id, out);
    }
    // A changed definition whose calls all have an equal expansion (R63.3-6): the proof line.
    for fact in &outcome.report.facts {
        if flow.expansion_lines.contains_key(&fact.id) {
            continue;
        }
        let Some(item) = fact.at.as_ref().map(|at| at.item.as_str()) else { continue };
        let Some(name) = item.rsplit("::").next().and_then(|short| short.strip_suffix('!')) else { continue };
        // The definition of the fact: a macro of the workspace in the file of the fact.
        let ours = |def: &str| !def.starts_with("crate ") && fact.file.as_deref().is_none_or(|file| def.starts_with(&format!("{file}:")));
        let users: Vec<&Expansion> = new_units.iter().flat_map(|unit| unit.list.iter()).filter(|e| e.outer.is_none() && ((e.name == name && ours(&e.def)) || e.uses.iter().any(|[used, def]| used == name && ours(def)))).collect();
        if users.is_empty() {
            continue;
        }
        let tops: BTreeSet<(&str, usize, usize)> = users.iter().map(|e| (e.file.as_str(), e.line, e.col)).collect();
        if tops.iter().all(|(file, line, col)| equal.contains(&(file.to_string(), *line, *col))) && !tops.is_empty() {
            let text = if tops.len() == 1 { format!("the call that uses `{name}!` has an equal expansion") } else { format!("each of the {} calls that use `{name}!` has an equal expansion", tops.len()) };
            flow.expansion_lines.insert(fact.id.clone(), vec![text]);
        }
    }
    outcome.report.flow = Some(flow);
}

/// The sides that `semilla expand` prints (R63.5-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sides {
    Both,
    Before,
    After,
}

/// The most lines of the text of one call in `semilla expand` with no `--full`.
pub const EXPAND_LIMIT: usize = 200;

/// The text of `semilla expand <place>` (R63.5-1): each call that the place selects, with the
/// expansion before the step and after it and a line diff. The place is `FILE:LINE` (a line
/// `-N` is in the old tree), an item path, or a macro name with `!` (each call of the macro, also
/// each call whose expansion uses it).
pub fn view(place: &str, units: [&[ExpansionUnit]; 2], trees: [&Workspace; 2], full: bool, sides: Sides) -> Result<String, String> {
    let same = |path: &str| path.to_string();
    let (changed, equal) = pair_sites(units, trees, &same, true);
    let line_place = place.rsplit_once(':').and_then(|(file, line)| {
        let (old, line) = match line.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        line.parse::<usize>().ok().map(|line| (file.to_string(), old, line))
    });
    let selects = |site: &Site| -> bool {
        [site.old, site.new].iter().enumerate().any(|(side, expansion)| {
            let Some(expansion) = expansion else { return false };
            if let Some(name) = place.strip_suffix('!') {
                return expansion.name == name || expansion.uses.iter().any(|[used, _]| used == name);
            }
            if let Some((file, old, line)) = &line_place {
                if *old != (side == 0) || *file != expansion.file {
                    return false;
                }
                let (_, start, end) = input_lines(trees[side], expansion, site.at[side].as_ref());
                return (expansion.line <= *line && *line <= expansion.end_line.max(expansion.line)) || (expansion.kind != "bang" && start <= *line && *line <= end);
            }
            site.at[side].as_ref().is_some_and(|at| at.item == place || at.item.starts_with(&format!("{place}::")) || at.to_string() == place)
        })
    };
    let mut found: Vec<&Site> = changed.iter().chain(equal.iter()).filter(|site| selects(site)).collect();
    if found.is_empty() {
        return Err(format!("no macro call with an expansion matches `{place}` (a place is FILE:LINE, an item path, or a macro name with `!`)"));
    }
    found.sort_by_key(|site| {
        let e = site.new.or(site.old).expect("a site has a side");
        (e.file.clone(), e.line, e.col)
    });
    let all_configs: BTreeSet<&str> = units.iter().flat_map(|list| list.iter()).map(|unit| unit.config.as_str()).collect();
    let mut out = String::new();
    for site in found {
        let any = site.new.or(site.old).expect("a site has a side");
        let side = if site.new.is_some() { 1 } else { 0 };
        let at = site.at[side].as_ref().map(|at| at.to_string()).unwrap_or_else(|| "no item".to_string());
        let result = match (site.old, site.new) {
            (Some(old), Some(new)) if old.text == new.text => "expansion equal",
            (Some(_), Some(_)) => "expansion changed",
            (None, _) => "expansion added",
            _ => "expansion removed",
        };
        let configs = if site.configs.len() == all_configs.len() { String::new() } else { format!(" (in {})", site.configs.join(", ")) };
        let call = if any.kind == "derive" { format!("derive `{}`", any.name) } else { format!("`{}`", any.call) };
        out.push_str(&format!("{call} at `{at}` ({}:{}{}): {result}{configs}\n", any.file, if side == 0 { "-" } else { "" }, any.line));
        let uses: Vec<String> = any.uses.iter().map(|[name, def]| format!("`{name}!` ({def})")).collect();
        out.push_str(&format!("  definition: {}{}\n", definition_text(any), if uses.is_empty() { String::new() } else { format!("; uses: {}", uses.join(", ")) }));
        let text_of = |expansion: Option<&Expansion>| expansion.map(|e| e.text.clone()).unwrap_or_default();
        let lines: Vec<String> = match sides {
            Sides::Before => text_of(site.old).lines().map(|line| format!("  {line}")).collect(),
            Sides::After => text_of(site.new).lines().map(|line| format!("  {line}")).collect(),
            Sides::Both if result == "expansion equal" => text_of(site.new).lines().map(|line| format!("  {line}")).collect(),
            Sides::Both => {
                let mut lines = vec!["--- before".to_string(), "+++ after".to_string()];
                lines.extend(line_diff(&text_of(site.old), &text_of(site.new)));
                lines
            }
        };
        let total = lines.len();
        for line in lines.iter().take(if full { usize::MAX } else { EXPAND_LIMIT }) {
            out.push_str(&format!("  {line}\n"));
        }
        if !full && total > EXPAND_LIMIT {
            out.push_str(&format!("  … {} more lines: semilla expand {place} --full\n", total - EXPAND_LIMIT));
        }
    }
    Ok(out)
}

/// The text of the items of an added or a removed expansion.
fn made_text(word: &str, expansion: &Expansion) -> String {
    let lines = expansion.text.lines().count();
    if expansion.items.is_empty() {
        return format!("{lines} line(s) of code");
    }
    let mut names: Vec<String> = expansion.items.iter().take(5).map(|item| format!("`{item}`")).collect();
    if expansion.items.len() > 5 {
        names.push(format!("and {} more", expansion.items.len() - 5));
    }
    format!("{word} {}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expansion(form: &str, text: &str) -> Expansion {
        Expansion { name: "m".into(), kind: "bang".into(), def: "src/lib.rs:1".into(), form: form.into(), text: text.into(), ..Default::default() }
    }

    #[test]
    fn the_line_diff_has_each_line_of_the_two_texts() {
        let diff = line_diff("a\nb\nc", "a\nx\nc");
        assert_eq!(diff, vec!["  a", "- b", "+ x", "  c"]);
    }

    /// The facts of the normal check of a crate with one function `f` with the body `text`.
    fn code_facts(old: &str, new: &str) -> Vec<String> {
        let write = |text: &str| {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("src")).unwrap();
            std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"shop\"\nversion = \"0.0.0\"\nedition = \"2021\"\n").unwrap();
            std::fs::write(dir.path().join("src/lib.rs"), format!("fn f() {{\n{text}\n}}\n")).unwrap();
            let ws = Workspace::load(dir.path()).unwrap();
            (dir, ws)
        };
        let ((_a, before), (_b, after)) = (write(old), write(new));
        let step = Step { index: 0, intent: "code".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), compiled: false, build_check: None, agent: Default::default(), tags: Vec::new() };
        let outcome = crate::verify_step_with(&StepInput { step: &step, before: &before, after: &after }, &NoOracle, &NoOracle, true);
        outcome.report.facts.iter().map(|fact| format!("{} [{}]", one_line(&fact.description).replace("`shop::f/", "`expansion/").replace("`shop::f`", "`expansion`"), fact.category.name())).collect()
    }

    // rule+ R63.3-2
    // rule- R63.3-2
    #[test]
    fn a_rule_of_the_normal_check_gives_the_same_fact_in_expanded_code() {
        // Each pair is the code of a macro arm before and after a change, as the driver prints
        // the expansion: a changed operator, an extracted variable, a renamed local, a moved
        // statement, and a changed condition.
        let pairs = [
            ("stmts", "let total = n + 1;\ntotal", "let total = n - 1;\ntotal", "changed operator `n + 1` → `n - 1`"),
            ("stmts", "let total = n * 3 + 1;\ntotal", "let base = n * 3;\nlet total = base + 1;\ntotal", "extracted variable `base` = `n * 3`"),
            ("stmts", "let value = n;\nvalue + value", "let amount = n;\namount + amount", "renamed local `value` → `amount`"),
            ("stmts", "a.push(1);\nb.push(2);\na.push(3);", "b.push(2);\na.push(1);\na.push(3);", "moved statement `a.push(1)`"),
            ("stmts", "if v > 10 { 10 } else { v }", "if v >= 10 { 10 } else { v }", "changed operator `v > 10` → `v >= 10`"),
        ];
        for (form, old, new, want) in pairs {
            let got: Vec<String> = compare(&expansion(form, old), &expansion(form, new), "shop", &NoOracle).unwrap().into_iter().map(|(text, category)| format!("{text} [{}]", category.name())).collect();
            assert!(got.iter().any(|fact| fact.contains(want)), "{want}: {got:?}");
            // The same rule gives the same fact text for the code of a function.
            assert_eq!(got, code_facts(old, new), "{want}");
        }
        // The negative case: two texts that differ only in the layout of the printer give no
        // fact.
        let layout = compare(&expansion("expr", "f(a,\n    b)"), &expansion("expr", "f(a, b)"), "shop", &NoOracle).unwrap();
        assert!(layout.is_empty(), "{layout:?}");
    }

    #[test]
    fn a_virtual_file_has_the_modules_of_the_call() {
        let mut e = expansion("expr", "a + 1");
        e.module = "a::b".into();
        assert_eq!(virtual_file(&e), "mod a {\nmod b {\nfn expansion() {\na + 1\n}\n}\n}\n");
    }
}
