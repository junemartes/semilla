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
    /// The source text of the call in a file that makes this call, for a call whose tokens
    /// come from a macro with no record (R63.9-3).
    #[serde(default)]
    pub via: String,
    /// The files that the code reads with `include_str!` or `include_bytes!` (R63.9-6).
    #[serde(default)]
    pub reads: Vec<String>,
    /// The text with a mark for each literal from `line!()`, `column!()`, or `file!()`
    /// (R63.9-8). Empty if the code has no such literal.
    #[serde(default)]
    pub placeless: String,
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
    /// The root file of the crate (R63.9-1).
    pub root: String,
    /// Each true `cfg` name and pair of the unit (the features).
    pub cfg: Vec<String>,
    pub list: Vec<Expansion>,
}

impl ExpansionUnit {
    /// The crate name of the unit.
    pub fn crate_name(&self) -> &str {
        self.target.split('.').next().unwrap_or("")
    }

    /// True if two units are of one crate name and kind of one package in one configuration.
    /// The hash of the target is not a part: a changed manifest (a new version of a dependency)
    /// changes it.
    fn same_kind(&self, other: &ExpansionUnit) -> bool {
        let short = |target: &str| target.splitn(3, '.').take(2).collect::<Vec<_>>().join(".");
        self.config == other.config && self.package == other.package && short(&self.target) == short(&other.target)
    }

    /// True if two units are the same target in the same configuration (R63.9-1): the package,
    /// the crate name, the kind, and the root file. A test target and a bench target of one
    /// package can have one name and one kind (`tests/load.rs` and `benches/load.rs`, or the
    /// test builds of `src/lib.rs` and `src/main.rs`): only the root file tells them apart.
    pub fn same_target(&self, other: &ExpansionUnit) -> bool {
        self.same_kind(other) && self.root == other.root
    }
}

/// The pairs of the units of the two trees (R63.9-1): each unit of the new tree gets at most one
/// unit of the old tree, and each unit of the old tree is in at most one pair. A unit pairs with
/// a unit of the same target. With two such units (a crate that the build compiles two times,
/// for two sets of features), the unit with the same features, then the unit with the most
/// calls at the same place with the same text, is the pair. A unit with no such unit pairs with
/// the one unit of the same crate name and kind whose root file is not in the new tree (a moved
/// root file).
fn unit_pairs(old_units: &[ExpansionUnit], new_units: &[ExpansionUnit]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut used = vec![false; old_units.len()];
    let mut pairs = Vec::new();
    let place = |unit: &ExpansionUnit| -> BTreeSet<(String, usize, usize, String, String)> { unit.list.iter().filter(|e| e.outer.is_none()).map(|e| (e.file.clone(), e.line, e.col, e.name.clone(), e.text.clone())).collect() };
    for (index, unit) in new_units.iter().enumerate() {
        let candidates: Vec<usize> = (0..old_units.len()).filter(|at| !used[*at] && old_units[*at].same_target(unit)).collect();
        let found = match candidates.as_slice() {
            [] => {
                let moved: Vec<usize> = (0..old_units.len()).filter(|at| !used[*at] && old_units[*at].same_kind(unit) && !new_units.iter().any(|new| new.same_target(&old_units[*at]))).collect();
                (moved.len() == 1).then(|| moved[0])
            }
            [one] => Some(*one),
            more => {
                let mine = place(unit);
                more.iter().copied().max_by_key(|at| (old_units[*at].cfg == unit.cfg, place(&old_units[*at]).intersection(&mine).count(), std::cmp::Reverse(*at)))
            }
        };
        if let Some(at) = found {
            used[at] = true;
        }
        pairs.push((found, Some(index)));
    }
    pairs.extend((0..old_units.len()).filter(|at| !used[*at]).map(|at| (Some(at), None)));
    pairs
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
fn chain_owners(outcome: &StepOutcome, sides: [Option<&Expansion>; 2]) -> (Vec<usize>, Vec<(String, String)>) {
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
        let cause = (name.to_string(), format!("`macro_rules! {name}` ({def})"));
        if !causes.contains(&cause) {
            causes.push(cause);
        }
    }
    (facts, causes)
}

/// The facts with an edit in the input of a call on either side, in the order of the facts.
fn input_facts(input: &StepInput, outcome: &StepOutcome, site: &Site) -> Vec<usize> {
    let mut found = Vec::new();
    for (side, expansion) in [site.old, site.new].iter().enumerate() {
        let Some(expansion) = expansion else { continue };
        let ws = if side == 0 { input.before } else { input.after };
        let (file, start, end) = input_lines(ws, expansion, site.at[side].as_ref());
        for (index, fact) in outcome.report.facts.iter().enumerate() {
            if holds(&fact.positions, side == 0, &file, start, end) && !found.contains(&index) {
                found.push(index);
            }
        }
    }
    found.sort();
    found
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
    // The files with the same text in the two trees (R63.9-2).
    let unchanged: BTreeSet<&str> = trees[0].rust_files.iter().filter(|(path, text)| trees[1].rust_files.get(*path) == Some(*text)).map(|(path, _)| path.as_str()).collect();
    for (a, b) in unit_pairs(old_units, new_units) {
        let unit = b.map(|b| &new_units[b]).or(a.map(|a| &old_units[a])).expect("a pair has a unit");
        let old_keys = a.map(|a| keys(&old_units[a].list, trees[0], map)).unwrap_or_default();
        let new_keys = b.map(|b| keys(&new_units[b].list, trees[1], &same)).unwrap_or_default();
        let mut found: Vec<Option<usize>> = vec![None; new_keys.len()];
        let mut used = vec![false; old_keys.len()];
        // R63.9-2: in a file that the step does not change, a call pairs with the call at the
        // same place (the file, the line, and the column) of the same macro and kind. Only the
        // call and its place make this key, so no edit in a different file can change it.
        for (index, (_, new, _)) in new_keys.iter().enumerate() {
            if new.file.is_empty() || !unchanged.contains(new.file.as_str()) {
                continue;
            }
            let at = old_keys.iter().enumerate().position(|(at, (_, old, _))| !used[at] && old.file == new.file && old.line == new.line && old.col == new.col && old.name == new.name && old.kind == new.kind);
            if let Some(at) = at {
                used[at] = true;
                found[index] = Some(at);
            }
        }
        // Else the key of R63.3-1: the item, the macro, the kind, and the number in the item.
        for (index, (key, _, _)) in new_keys.iter().enumerate() {
            if found[index].is_some() {
                continue;
            }
            let at = old_keys.iter().enumerate().position(|(at, (other, _, _))| !used[at] && other == key);
            if let Some(at) = at {
                used[at] = true;
                found[index] = Some(at);
            }
        }
        let mut pairs: Vec<(Option<usize>, Option<usize>)> = found.into_iter().enumerate().map(|(index, at)| (at, Some(index))).collect();
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
    (sites, equal)
}

/// A changed expansion for the owner pass (R63.9-5): the items that hold the call on each side,
/// the macros of its chain, and the files that its code reads.
#[derive(Clone, Debug, Default)]
pub struct Link {
    /// The item of the anchor of the call on each side (the old one with the rename map).
    pub items: BTreeSet<String>,
    /// The names of the macros of the chain on the two sides (the macro of the call and each
    /// macro that its code uses).
    pub chain: BTreeSet<String>,
    /// The files that the code of the call reads on the two sides.
    pub reads: BTreeSet<String>,
}

/// The changed expansions of a step (R63.9-5), for the owner pass: only a pair of two calls
/// with two different texts. An added or a removed call is an edit of the call or of its item.
pub fn links(step: &Step, trees: [&Workspace; 2], bodies: &dyn crate::BodyOracle, renames: &BTreeMap<String, String>) -> Vec<Link> {
    if !step.compiled {
        return Vec::new();
    }
    let Some([old_units, new_units]) = bodies.expansions(step) else { return Vec::new() };
    let map = |path: &str| renames.get(path).cloned().unwrap_or_else(|| path.to_string());
    let (sites, _) = pair_sites([&old_units, &new_units], trees, &map, false);
    let mut out = Vec::new();
    for site in &sites {
        let (Some(old), Some(new)) = (site.old, site.new) else { continue };
        let mut link = Link::default();
        for (side, at) in site.at.iter().enumerate() {
            if let Some(at) = at {
                link.items.insert(if side == 0 { map(&at.item) } else { at.item.clone() });
            }
        }
        for expansion in [old, new] {
            link.chain.insert(expansion.name.clone());
            link.chain.extend(expansion.uses.iter().map(|[name, _]| name.clone()));
            link.reads.extend(expansion.reads.iter().cloned());
        }
        out.push(link);
    }
    out
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
        // R63.9-8: a call that only moves, on the evidence of the compiler: the texts with no
        // place literal are equal, the call text is equal, no definition of the chain changes,
        // and no fact has an edit in the input of the call.
        let moved = match (site.old, site.new) {
            (Some(old), Some(new)) if chain_owners(outcome, [site.old, site.new]).0.is_empty() && input_facts(input, outcome, site).is_empty() => moved_place(old, new),
            _ => None,
        };
        let (result, facts) = match (site.old, site.new) {
            (Some(old), Some(new)) => {
                if let Some(moved) = moved.clone() {
                    ("expansion changed", vec![moved])
                } else {
                    let found = cache.entry((old.text.clone(), new.text.clone())).or_insert_with(|| compare(old, new, &site.crate_name, exprs));
                    let facts: Vec<String> = match found {
                        Ok(list) if list.is_empty() => vec!["no fact: the two texts differ only in the layout".to_string()],
                        // R63.9-8: a fact of the whole virtual file (`src/lib.rs: changed`) names
                        // a file that the author does not have: the line gives the changed text.
                        Ok(list) if list.iter().all(|(text, _)| text.starts_with("src/lib.rs:")) => vec![changed_text(&old.text, &new.text)],
                        Ok(list) => list.iter().map(|(text, category)| format!("{text} [{}]", category.name())).collect(),
                        Err(error) => vec![format!("not compared: {error}"), changed_text(&old.text, &new.text)],
                    };
                    ("expansion changed", facts)
                }
            }
            (None, Some(new)) => ("expansion added", vec![made_text("makes", new)]),
            (Some(old), None) => ("expansion removed", vec![made_text("made", old)]),
            (None, None) => continue,
        };
        let side = if site.new.is_some() { 1 } else { 0 };
        let at = site.at[side].as_ref().map(|at| at.to_string()).unwrap_or_else(|| format!("{} (no item)", any.module));
        // R63.9-3: a call with no place in a file of the workspace says so.
        let place = if any.file.is_empty() { NO_PLACE.to_string() } else { format!("{}:{}{}", any.file, if side == 0 { "-" } else { "" }, any.line) };
        // A changed definition owns a changed expansion; an added or a removed call belongs to
        // the edit of the call.
        let (mut owners, causes) = if site.old.is_some() && site.new.is_some() { chain_owners(outcome, [site.old, site.new]) } else { (Vec::new(), Vec::new()) };
        owners.sort();
        // R63.9-4: a changed definition of a call in the input of this call: the cause names
        // that call (`mkfn!(made, add!(4; 5))` changes through `add!(4; 5)`).
        let causes: Vec<String> = causes.into_iter().map(|(name, cause)| match inner_call(&sites, site, &name) {
            Some(call) if name != any.name => format!("{cause} through the inner call `{call}`"),
            _ => cause,
        }).collect();
        // The facts with an edit in the input of the call.
        let input_owners: Vec<usize> = input_facts(input, outcome, site).into_iter().filter(|index| !owners.contains(index)).collect();
        let mut cause = causes.iter().map(|cause| format!("from {cause}")).collect::<Vec<_>>().join(", ");
        if !input_owners.is_empty() {
            let word = match result {
                "expansion added" => "the step adds the call or its input",
                "expansion removed" => "the step removes the call or its input",
                _ => "the input of the call changes",
            };
            cause = if cause.is_empty() { word.to_string() } else { format!("{cause}; {word}") };
        }
        owners.extend(input_owners);
        // R63.9-6: a changed file that the code of the call reads (`include_bytes!`) is a cause:
        // the fact of the file owns the expansion.
        if owners.is_empty() {
            let reads: BTreeSet<&String> = [site.old, site.new].iter().flatten().flat_map(|e| e.reads.iter()).collect();
            for (index, fact) in outcome.report.facts.iter().enumerate() {
                if fact.at.is_none() && fact.part != "indirect change" && fact.file.as_ref().is_some_and(|file| reads.contains(file)) {
                    owners.push(index);
                    let text = format!("the code of the call reads `{}`, which the step changes", fact.file.as_deref().unwrap_or(""));
                    cause = if cause.is_empty() { text } else { format!("{cause}; {text}") };
                }
            }
        }
        // No changed definition, no changed input, and no changed file that the code reads (a
        // change of a dependency, a call that moves): only an indirect fact of the body that
        // holds the call owns it (R63.9-7). Each other fact of that body has a different cause.
        if owners.is_empty() {
            let holder = site.at[1].as_ref().or(site.at[0].as_ref()).map(|at| at.item.clone()).unwrap_or_default();
            owners.extend(outcome.report.facts.iter().enumerate().filter(|(_, fact)| !holder.is_empty() && fact.part == "indirect change" && fact.affected.contains(&holder)).map(|(index, _)| index));
            cause = if moved.is_some() {
                "the call moves".to_string()
            } else {
                "no changed definition, no changed input of the call, and no changed file that its code reads".to_string()
            };
        }
        let configs = if site.configs.len() == all_configs.len() { Vec::new() } else { site.configs.clone() };
        let shown_call = shown(any);
        let summary = facts.join("; ");
        let key = format!("{result}: {}: {summary}{}", definition_text(any), if cause.is_empty() { String::new() } else { format!("; {cause}") });
        let diff = cut_diff(&line_diff(site.old.map(|e| e.text.as_str()).unwrap_or(""), site.new.map(|e| e.text.as_str()).unwrap_or("")), DIFF_LIMIT, &format!("semilla expand {place}"));
        let owner_ids: Vec<String> = owners.iter().map(|index| outcome.report.facts[*index].id.clone()).collect();
        // R63.9-7: the line goes under one fact, the first owner (a changed definition of the
        // chain, else an edit of the input, else a changed file that the code reads). The line
        // names the other owners; their own lines do not repeat it.
        if let Some((first, others)) = owner_ids.split_first() {
            let prefix = if any.kind == "derive" { format!("derived `{}` of `{at}`", any.name) } else if any.file.is_empty() { format!("{shown_call} ({place})") } else { format!("{shown_call} at `{at}` ({place})") };
            let also = if others.is_empty() {
                String::new()
            } else {
                let limit = semilla_core::report::REACH_FOLD_NAMES;
                let more = if others.len() > limit { format!(", and {} more", others.len() - limit) } else { String::new() };
                format!(" (also the change of {}{more})", others.iter().take(limit).cloned().collect::<Vec<_>>().join(", "))
            };
            let line = format!("{prefix}: {result}: {summary}{also}");
            let fold = format!("{}|{result}|{summary}{also}", any.name);
            by_fact.entry(first.clone()).or_default().push((fold, line, at.clone()));
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
        let call = shown(any);
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

/// The place text of a call with no place in a file of the workspace (R63.9-3).
pub const NO_PLACE: &str = "no place: the tokens of the call are not in a file of the workspace";

/// A call inside the input of a different call (R63.9-4): a changed site of the macro `name`
/// whose place is inside the place of the call of `site`.
fn inner_call(sites: &[Site], site: &Site, name: &str) -> Option<String> {
    let outer = site.new.or(site.old)?;
    sites.iter().filter(|other| !std::ptr::eq(*other, site) && other.old.is_some() && other.new.is_some()).filter_map(|other| other.new).find(|inner| inner.name == name && !outer.file.is_empty() && inner.file == outer.file && outer.r[0] <= inner.r[0] && inner.r[1] <= outer.r[1] && inner.r != outer.r).map(|inner| inner.call.clone())
}

/// The text of a call that only moves (R63.9-8).
pub const MOVED: &str = "only the place of the call changes: its code has the line, the column, or the file of the call (`line!()`, `column!()`, `file!()`)";

/// R63.9-8: the text of a call that only moves, on the evidence of the compiler. The driver
/// gives each expansion a second text (`placeless`) in which each literal from an expansion of
/// `line!()`, `column!()`, or `file!()` (also through `concat!`) is a mark. Two different texts
/// with equal placeless texts differ only in such literals: the place of the call. The call
/// text must be equal too. A number or a text that only looks like a line is not evidence: a
/// record with no such literal has no placeless text, so it never gives this text. `None`
/// for each other pair. The caller checks the definitions of the chain and the input.
pub fn moved_place(old: &Expansion, new: &Expansion) -> Option<String> {
    if old.text == new.text || old.call != new.call || (old.placeless.is_empty() && new.placeless.is_empty()) {
        return None;
    }
    let placeless = |e: &Expansion| if e.placeless.is_empty() { e.text.clone() } else { e.placeless.clone() };
    // The numbers are in the diff of the call: the text is the same for each moved call, so
    // the moved calls of one macro fold into one line.
    (placeless(old) == placeless(new)).then(|| MOVED.to_string())
}

/// R63.9-8: the changed part of two texts on one line: the text between the equal start and
/// the equal end, at most 80 characters on each side: "changed code `39u32` → `41u32`".
pub fn changed_text(old: &str, new: &str) -> String {
    let a: Vec<char> = one_line(old).chars().collect();
    let b: Vec<char> = one_line(new).chars().collect();
    let start = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let end = a[start..].iter().rev().zip(b[start..].iter().rev()).take_while(|(x, y)| x == y).count();
    // Start and end at a word boundary, so a token is not cut.
    let word = |c: &char| c.is_alphanumeric() || *c == '_';
    let mut from = start;
    while from > 0 && (word(&a[from - 1]) || word(&b[from - 1])) {
        from -= 1;
    }
    let (mut to_a, mut to_b) = (a.len() - end, b.len() - end);
    while to_a < a.len() && to_b < b.len() && (word(&a[to_a]) || word(&b[to_b])) && a[to_a] == b[to_b] {
        to_a += 1;
        to_b += 1;
    }
    let cut = |chars: &[char]| -> String {
        let text: String = chars.iter().collect::<String>().trim().to_string();
        if text.chars().count() > 80 { text.chars().take(80).collect::<String>() + "…" } else { text }
    };
    format!("changed code `{}` → `{}`", cut(&a[from..to_a]), cut(&b[from..to_b]))
}

/// The text of a call for a line: `fee!(v)`, derive `PartialEq`, or the call and the call in a
/// file that makes it (R63.9-3): `make!{@inner …}` from `sum!(1, 2)`.
fn shown(expansion: &Expansion) -> String {
    if expansion.kind == "derive" {
        return format!("derive `{}`", expansion.name);
    }
    if expansion.via.is_empty() {
        return format!("`{}`", expansion.call);
    }
    format!("`{}` from `{}`", expansion.call, expansion.via)
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

    fn unit(target: &str, root: &str, calls: &[(&str, usize, &str)]) -> ExpansionUnit {
        let list = calls.iter().map(|(file, line, text)| Expansion { file: file.to_string(), line: *line, ..expansion("expr", text) }).collect();
        ExpansionUnit { config: "dev".into(), target: target.into(), package: "shop".into(), root: root.into(), cfg: Vec::new(), list }
    }

    // rule+ R63.9-1
    // rule- R63.9-1
    #[test]
    fn a_unit_pairs_with_the_unit_of_its_root_file() {
        // A test and a bench with one name and one kind: the order of the files changes, and a
        // new bench comes. Each unit pairs with the unit of its root file.
        let old = vec![unit("load.test.aaaa", "benches/load.rs", &[("benches/load.rs", 5, "1")]), unit("load.test.bbbb", "tests/load.rs", &[("tests/load.rs", 5, "2")])];
        let new = vec![unit("load.test.cccc", "tests/load.rs", &[("tests/load.rs", 5, "2")]), unit("extra.test.dddd", "benches/extra.rs", &[]), unit("load.test.eeee", "benches/load.rs", &[("benches/load.rs", 5, "1")])];
        assert_eq!(unit_pairs(&old, &new), vec![(Some(1), Some(0)), (None, Some(1)), (Some(0), Some(2))]);
        // The negative case: a unit with a different root file is not the same target, and an
        // old unit with no pair is in the list alone (its calls are removed).
        assert!(!old[0].same_target(&new[0]));
        assert_eq!(unit_pairs(&old, &new[..1]), vec![(Some(1), Some(0)), (Some(0), None)]);
        // A moved root file: the one unit of the same crate name and kind whose root file is not
        // in the new tree.
        let moved = vec![unit("load.test.ffff", "tests/load/main.rs", &[])];
        assert_eq!(unit_pairs(&old[1..], &moved), vec![(Some(0), Some(0))]);
        // Two units of one target (a crate that the build compiles two times): the unit with the
        // most calls with the same place and text.
        let twice = vec![unit("load.lib.1", "src/lib.rs", &[("src/lib.rs", 1, "a")]), unit("load.lib.2", "src/lib.rs", &[("src/lib.rs", 1, "b")])];
        let again = vec![unit("load.lib.3", "src/lib.rs", &[("src/lib.rs", 1, "b")]), unit("load.lib.4", "src/lib.rs", &[("src/lib.rs", 1, "a")])];
        assert_eq!(unit_pairs(&twice, &again), vec![(Some(1), Some(0)), (Some(0), Some(1))]);
    }

    // rule+ R63.9-8
    // rule- R63.9-8
    #[test]
    fn a_call_that_only_moves_has_one_text_and_a_file_fact_gives_the_changed_text() {
        let at = |line: usize, text: &str, placeless: &str| Expansion { file: "src/lib.rs".into(), line, col: 4, call: "m!()".into(), placeless: placeless.into(), ..expansion("stmts", text) };
        // The driver marks the literals of `line!()` and of `concat!(file!(), ":", line!())`.
        let mark = "(1, \"\u{0}line\u{0}\")";
        assert_eq!(moved_place(&at(16, "(1, 16u32)", mark), &at(18, "(1, 18u32)", mark)).as_deref(), Some(MOVED));
        let concat = "panic!(\"\u{0}concat!(\"at \", file!(), \":\", line!())\u{0}\")";
        assert_eq!(moved_place(&at(16, "panic!(\"at src/lib.rs:16\")", concat), &at(18, "panic!(\"at src/lib.rs:18\")", concat)).as_deref(), Some(MOVED));
        // The negative cases: a different change with the move, a changed literal that only
        // looks like the line (`5u32` → `6u32`, `"src/lib.rs:5"` → `"src/lib.rs:6"`, no mark of
        // the driver), a different call text, and a call that does not move.
        assert_eq!(moved_place(&at(16, "(1, 16u32)", mark), &at(18, "(2, 18u32)", "(2, \"\u{0}line\u{0}\")")), None);
        assert_eq!(moved_place(&at(5, "5u32", ""), &at(6, "6u32", "")), None);
        assert_eq!(moved_place(&at(5, "\"src/lib.rs:5\"", ""), &at(6, "\"src/lib.rs:6\"", "")), None);
        let mut other = at(18, "(1, 18u32)", mark);
        other.call = "m!(x)".into();
        assert_eq!(moved_place(&at(16, "(1, 16u32)", mark), &other), None);
        assert_eq!(moved_place(&at(16, "(1, 16u32)", mark), &at(16, "(1, 16u32)", mark)), None);
        // The changed text of two texts, at word bounds.
        assert_eq!(changed_text("let a = f(39u32, x);", "let a = f(41u32, x);"), "changed code `39u32` → `41u32`");
        assert_eq!(changed_text("value_one + 1", "value_two + 1"), "changed code `value_one` → `value_two`");
    }

    #[test]
    fn a_virtual_file_has_the_modules_of_the_call() {
        let mut e = expansion("expr", "a + 1");
        e.module = "a::b".into();
        assert_eq!(virtual_file(&e), "mod a {\nmod b {\nfn expansion() {\na + 1\n}\n}\n}\n");
    }
}
