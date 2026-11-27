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

use semilla_core::report::ExpansionSite;
use semilla_core::{Category, Step};
use semilla_syntax::Workspace;

use crate::inputs::{self, State};
use semilla_core::matcher::{Key, Ladder, Thing};

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
    /// The text with a mark for each token of the context of the call (R65.1-1): a value that
    /// the compiler or a proc macro makes at the call. Empty if the code has no such token.
    #[serde(default)]
    pub placeless: String,
    /// The source and the value of each token of the context, in the order of the text
    /// (R65.1-2).
    #[serde(default)]
    pub context: Vec<[String; 2]>,
    /// The identity of each definition of the chain that no fact of the step can show: a
    /// macro of a crate outside the workspace and a proc macro, with the hash of its crate
    /// (R65.1-3).
    #[serde(default)]
    pub ids: Vec<String>,
}

/// The expansions of one compiled target in one checked configuration.
#[derive(Clone, Debug, Default, PartialEq)]
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
    /// The name record of the unit (design 70, R70.1-1), or `None`.
    pub names: Option<std::sync::Arc<semilla_core::namerec::Record>>,
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

/// The pairs of the units of the two trees (R63.9-1 before batch 11d, now R72.1-1, part e): each
/// unit of the new tree gets at most one unit of the old tree, and each unit of the old tree is
/// in at most one pair. The one matcher pairs the units of one configuration, package, and kind
/// of target: the identity is the crate name of the target; the similarity gives 0.5 for the same
/// root file, 0.2 for the same features, and 0.3 times the share of the calls with the same place
/// and text. So two units of one target (a test and a bench of one name, a crate that the build
/// compiles two times) pair by their root file and their calls, a moved root file keeps its pair,
/// and a target with a new name and the same root file and calls pairs by similarity.
fn unit_pairs(old_units: &[ExpansionUnit], new_units: &[ExpansionUnit]) -> Vec<(Option<usize>, Option<usize>)> {
    let kind = |unit: &ExpansionUnit| unit.target.split('.').nth(1).unwrap_or("").to_string();
    let thing = |unit: &ExpansionUnit| Thing { group: format!("{}\u{0}{}\u{0}{}", unit.config, unit.package, kind(unit)), identity: Some(unit.crate_name().to_string()), content: None };
    let (old_things, new_things): (Vec<Thing>, Vec<Thing>) = (old_units.iter().map(thing).collect(), new_units.iter().map(thing).collect());
    let place = |unit: &ExpansionUnit| -> BTreeSet<(String, usize, usize, String, String)> { unit.list.iter().filter(|e| e.outer.is_none()).map(|e| (e.file.clone(), e.line, e.col, e.name.clone(), e.text.clone())).collect() };
    let (old_places, new_places): (Vec<_>, Vec<_>) = (old_units.iter().map(place).collect(), new_units.iter().map(place).collect());
    let similarity = |a: usize, b: usize| {
        let (x, y) = (&old_places[a], &new_places[b]);
        let union = x.union(y).count();
        let calls = if union == 0 { 1.0 } else { x.intersection(y).count() as f64 / union as f64 };
        0.5 * f64::from(u8::from(old_units[a].root == new_units[b].root)) + 0.2 * f64::from(u8::from(old_units[a].cfg == new_units[b].cfg)) + 0.3 * calls
    };
    let ladder = Ladder { similarity: &similarity, threshold: UNIT_THRESHOLD, in_order: None };
    let found = semilla_core::matcher::pair(&old_things, &new_things, &ladder);
    let mut partner: Vec<Option<usize>> = vec![None; new_units.len()];
    let mut used = vec![false; old_units.len()];
    for pair in found {
        partner[pair.new] = Some(pair.old);
        used[pair.old] = true;
    }
    let mut pairs: Vec<(Option<usize>, Option<usize>)> = partner.into_iter().enumerate().map(|(index, found)| (found, Some(index))).collect();
    pairs.extend((0..old_units.len()).filter(|at| !used[*at]).map(|at| (Some(at), None)));
    pairs
}

/// The smallest similarity of a pair of two units with two crate names (Decided by Claude, design
/// 72.4): the same root file, or the same features and most calls.
const UNIT_THRESHOLD: f64 = 0.5;

/// The smallest similarity of a pair of two calls by similarity (Decided by Claude, design 72.4).
const CALL_THRESHOLD: f64 = 0.6;

/// R72.1-1 (part e): the identity of the macro of a call, by the compiler: the definition of a
/// macro of another crate or of a proc macro (its crate and the hash of its definition in the
/// record, the same for `format!` and `alloc::format!`); for a macro of the workspace, the file of
/// its definition and its last name (the line moves with an edit above it).
fn macro_identity(expansion: &Expansion) -> String {
    let own = format!("{}: ", expansion.name);
    if let Some(found) = expansion.ids.iter().find_map(|id| id.strip_prefix(own.as_str())) {
        return found.to_string();
    }
    let last = expansion.name.rsplit("::").next().unwrap_or(&expansion.name);
    let file = expansion.def.rsplit_once(':').filter(|(_, line)| !line.is_empty() && line.chars().all(|c| c.is_ascii_digit())).map_or(expansion.def.as_str(), |(file, _)| file);
    format!("{file} {last}")
}

/// The tokens of the text of a call for its similarity: each name or number, and each other
/// character that is not a space.
fn call_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if !c.is_whitespace() {
            out.push(c.to_string());
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
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
    /// The key of the ladder that made the pair (R72.1-3): a weak key shows in the line.
    key: Option<Key>,
}

/// The place of a call site in its tree: the anchor of the smallest node at the start.
fn anchor(index: &semilla_syntax::ItemIndex, expansion: &Expansion) -> Option<semilla_core::Anchor> {
    if expansion.file.is_empty() {
        return None;
    }
    index.anchor_at(&expansion.file, expansion.line, Some(expansion.col)).or_else(|| index.anchor_at(&expansion.file, expansion.line, None))
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
    let step = Step { index: 0, intent: "expansion".into(), claims: Vec::new(), tree_before: "old".into(), tree_after: "new".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent: Default::default(), tags: Vec::new() };
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
    // A proc macro has an identity with the word (R65.1-3).
    let proc = expansion.ids.iter().any(|id| id.starts_with(&format!("{}: proc macro ", expansion.name)));
    let word = match expansion.kind.as_str() {
        "derive" => "derive",
        "attr" => "attribute macro",
        _ if proc => "proc macro",
        _ if expansion.def.starts_with("crate ") => "macro",
        _ => "macro_rules!",
    };
    format!("`{word} {}` ({})", expansion.name, expansion.def)
}

/// The inputs of the chain of a changed call (R73.1-1, R73.2-1): the macro of the call and each
/// macro that its code uses, at each depth (R63.9-4), on each side.
#[derive(Default)]
struct Chain {
    /// The facts of the changed definitions, nearest first.
    owners: Vec<usize>,
    /// The macro and the text of each changed definition with an owner fact.
    causes: Vec<(String, String)>,
    /// The texts of the changed definitions that no fact of the step shows (a definition outside
    /// the tree with a new identity).
    known: Vec<String>,
    /// The texts of the definitions that Semilla does not see (R73.3-1).
    not_seen: Vec<String>,
}

/// R73.2-1: the definitions of the chain of a call as inputs. A definition in a file of the tree
/// changes if the content hash of its file changes; its owner facts are the facts of the item at
/// its place, else the facts of its file (a file that Semilla does not parse: a path dependency
/// that is not a member). A proc macro is a function of its crate: with no fact at its place, the
/// facts of each changed file of its package. A definition outside the tree has the identity of
/// its record (R65.1-3): a package of a registry is the rule of R65.10-2; a path package outside
/// the tree or a macro of the tool chain with a new identity is a known change with no fact (the
/// tool chain: the facts of its configuration files); a definition with no identity is not seen.
fn chain_inputs(input: &StepInput, outcome: &StepOutcome, sides: [Option<&Expansion>; 2], locks: &[Option<BTreeMap<String, BTreeSet<String>>>; 2]) -> Chain {
    let trees = [input.before, input.after];
    let facts = &outcome.report.facts;
    let mut chain = Chain::default();
    let identity = |expansion: Option<&Expansion>, name: &str| expansion.and_then(|e| e.ids.iter().find(|id| id.starts_with(&format!("{name}: "))).cloned());
    let mut seen: BTreeSet<(usize, String, String)> = BTreeSet::new();
    for (side, expansion) in sides.iter().enumerate() {
        let Some(expansion) = expansion else { continue };
        let mut all = vec![[expansion.name.clone(), expansion.def.clone()]];
        all.extend(expansion.uses.iter().cloned());
        for [name, def] in all {
            if !seen.insert((side, name.clone(), def.clone())) {
                continue;
            }
            let proc = identity(Some(expansion), &name).is_some_and(|id| id.starts_with(&format!("{name}: proc macro ")));
            if let Some((file, line)) = inputs::place_of(&def) {
                match inputs::file_state(trees, file) {
                    State::Same => continue,
                    State::NotSeen(text) => {
                        chain.not_seen.push(text);
                        continue;
                    }
                    State::Changed => {}
                }
                let mut found = inputs::place_facts(facts, trees[side], side == 0, file, line);
                if found.is_empty() && proc {
                    for changed in inputs::changed_files(trees, &inputs::package_dir(trees, file).unwrap_or_default()) {
                        inputs::extend(&mut found, inputs::file_facts(facts, &changed));
                    }
                }
                if found.is_empty() {
                    continue;
                }
                inputs::extend(&mut chain.owners, found);
                let word = if proc { "proc macro" } else { "macro_rules!" };
                let cause = (name.clone(), format!("`{word} {name}` ({def})"));
                if !chain.causes.contains(&cause) {
                    chain.causes.push(cause);
                }
                continue;
            }
            let Some(krate) = def.strip_prefix("crate ") else {
                chain.not_seen.push(format!("the definition of `{name}` ({def})"));
                continue;
            };
            let kind = inputs::outside_kind(locks, krate);
            if kind == Some(inputs::Outside::Registry) {
                // R65.10-2: the entry of the package in `Cargo.lock` is the identity.
                continue;
            }
            // R73.1-1: an identity is a hash of the definition or of its crate (`macro of crate X
            // (definition H)`, `proc macro of crate X H`). A text with no hash (a macro of this
            // crate in a file that the build makes) is no identity.
            let hashed = |id: Option<String>| id.filter(|id| id.contains(" of crate "));
            let (old, new) = (hashed(identity(sides[0], &name)), hashed(identity(sides[1], &name)));
            match (old, new) {
                (Some(a), Some(b)) if a == b => {}
                (Some(_), Some(_)) => {
                    let tool = if kind == Some(inputs::Outside::ToolChain) { inputs::tool_facts(facts) } else { Vec::new() };
                    if tool.is_empty() {
                        let place = if kind == Some(inputs::Outside::Path) { "a path package outside the tree" } else { "outside the tree" };
                        let text = format!("the definition of `{name}` ({def}, {place}) has a new identity");
                        if !chain.known.contains(&text) {
                            chain.known.push(text);
                        }
                    } else {
                        inputs::extend(&mut chain.owners, tool);
                        let cause = (name.clone(), format!("the tool chain (`{name}`, {def})"));
                        if !chain.causes.contains(&cause) {
                            chain.causes.push(cause);
                        }
                    }
                }
                // A macro of the chain of one side only: the other inputs of the call explain
                // the new chain.
                (Some(_), None) | (None, Some(_)) => {}
                (None, None) => {
                    let text = format!("the definition of `{name}` ({def}): the record has no hash of it");
                    if !chain.not_seen.contains(&text) {
                        chain.not_seen.push(text);
                    }
                }
            }
        }
    }
    chain
}

/// The facts with an edit in the input of a call on either side, in the order of the facts.
fn input_facts(input: &StepInput, outcome: &StepOutcome, site: &Site) -> Vec<usize> {
    let mut found = Vec::new();
    for (side, expansion) in [site.old, site.new].iter().enumerate() {
        let Some(expansion) = expansion else { continue };
        let ws = if side == 0 { input.before } else { input.after };
        let (file, start, end) = input_lines(ws, expansion, site.at[side].as_ref());
        for (index, fact) in outcome.report.facts.iter().enumerate() {
            if inputs::holds(&fact.positions, side == 0, &file, start, end) && !found.contains(&index) {
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

/// One call of a unit for the pair of the two trees: the expansion, the anchor of the call, its
/// owner (the item that holds the call, the old path through the rename map of the step), and
/// true for a call in an expansion.
struct Call<'a> {
    expansion: &'a Expansion,
    at: Option<semilla_core::Anchor>,
    owner: String,
    nested: bool,
}

/// The calls of a unit for the pair (R72.1-1, part e): each outer call, and each call in an
/// expansion whose tokens have a place in a file (an attribute macro can give an item with a
/// derive to the compiler: the derive is then a call in the expansion). In source order.
fn calls<'a>(list: &'a [Expansion], index: &semilla_syntax::ItemIndex, map: &dyn Fn(&str) -> String) -> Vec<Call<'a>> {
    let mut found: Vec<Call<'a>> = list
        .iter()
        .filter(|expansion| expansion.outer.is_none() || !expansion.file.is_empty())
        .map(|expansion| {
            let at = anchor(index, expansion);
            let owner = at.as_ref().map(|at| map(&at.item)).unwrap_or_else(|| format!("{}:{}", expansion.file, expansion.module));
            Call { expansion, at, owner, nested: expansion.outer.is_some() }
        })
        .collect();
    found.sort_by(|a, b| (a.expansion.file.as_str(), a.expansion.line, a.expansion.col, a.expansion.r, a.nested).cmp(&(b.expansion.file.as_str(), b.expansion.line, b.expansion.col, b.expansion.r, b.nested)));
    found
}

/// The pairs of the call sites of the two trees (R72.1-1, part e; before batch 11d: R63.3-1 and
/// R63.9-2), joined over the units: one site for each pair of texts, with the configurations.
/// The second list has the pairs with equal texts (with `keep_equal`, also joined; else one entry
/// for each pair).
///
/// The one matcher pairs the calls of each pair of units. The identity of a call: in a file that
/// the step does not change, its place (the file, the line, the column), its macro, and its kind:
/// only the call and its place make this key, so no edit in a different file can change it; in
/// each other file, the item that holds it, the definition of its macro (not the text of its
/// path: `format!` and `alloc::format!` are one macro), and its kind. Then equal call texts in one
/// item, then the best similarity of the call texts in one item (the threshold
/// [`CALL_THRESHOLD`]), then the source order. A call in an expansion takes part, so that the call
/// pairs with the call of the other tree when it moves out of an expansion or into one; a pair of
/// two such calls and such a call with no pair give no line: the text of the outer call has them.
fn pair_sites<'a>(units: [&'a [ExpansionUnit]; 2], trees: [&Workspace; 2], map: &dyn Fn(&str) -> String, keep_equal: bool) -> (Vec<Site<'a>>, Vec<Site<'a>>) {
    let [old_units, new_units] = units;
    let same = |path: &str| path.to_string();
    let mut sites: Vec<Site> = Vec::new();
    let mut equal: Vec<Site> = Vec::new();
    let push = |list: &mut Vec<Site<'a>>, old: Option<&'a Expansion>, new: Option<&'a Expansion>, at: [Option<semilla_core::Anchor>; 2], unit: &ExpansionUnit, join: bool, key: Option<Key>| {
        let found = if join { list.iter_mut().find(|site| site.old.map(|e| &e.text) == old.map(|e| &e.text) && site.new.map(|e| &e.text) == new.map(|e| &e.text) && site.at == at) } else { None };
        match found {
            Some(site) => {
                if !site.configs.contains(&unit.config) {
                    site.configs.push(unit.config.clone());
                }
            }
            None => list.push(Site { old, new, at, crate_name: unit.crate_name().to_string(), configs: vec![unit.config.clone()], key }),
        }
    };
    // The files with the same text in the two trees.
    let unchanged: BTreeSet<&str> = trees[0].rust_files.iter().filter(|(path, text)| trees[1].rust_files.get(*path) == Some(*text)).map(|(path, _)| path.as_str()).collect();
    // R86.1-5: one index of the items of each tree for each anchor of the calls.
    let index = [semilla_syntax::ItemIndex::new(trees[0]), semilla_syntax::ItemIndex::new(trees[1])];
    for (a, b) in unit_pairs(old_units, new_units) {
        let unit = b.map(|b| &new_units[b]).or(a.map(|a| &old_units[a])).expect("a pair has a unit");
        let old_calls = a.map(|a| calls(&old_units[a].list, &index[0], map)).unwrap_or_default();
        let new_calls = b.map(|b| calls(&new_units[b].list, &index[1], &same)).unwrap_or_default();
        let thing = |call: &Call| {
            let e = call.expansion;
            let identity = if !e.file.is_empty() && unchanged.contains(e.file.as_str()) { format!("place\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}", e.file, e.line, e.col, e.name, e.kind) } else { format!("item\u{0}{}\u{0}{}\u{0}{}", call.owner, macro_identity(e), e.kind) };
            Thing { group: String::new(), identity: Some(identity), content: Some(format!("{}\u{0}{}\u{0}{}", call.owner, e.kind, e.call)) }
        };
        let (old_things, new_things): (Vec<Thing>, Vec<Thing>) = (old_calls.iter().map(thing).collect(), new_calls.iter().map(thing).collect());
        let (old_tokens, new_tokens): (Vec<Vec<String>>, Vec<Vec<String>>) = (old_calls.iter().map(|call| call_tokens(&call.expansion.call)).collect(), new_calls.iter().map(|call| call_tokens(&call.expansion.call)).collect());
        let similarity = |x: usize, y: usize| {
            let (o, n) = (&old_calls[x], &new_calls[y]);
            if o.owner != n.owner || o.expansion.kind != n.expansion.kind || o.nested || n.nested {
                return 0.0;
            }
            semilla_core::matcher::token_similarity(&old_tokens[x], &new_tokens[y])
        };
        let ladder = Ladder { similarity: &similarity, threshold: CALL_THRESHOLD, in_order: None };
        let found = semilla_core::matcher::pair(&old_things, &new_things, &ladder);
        let mut partner: Vec<Option<(usize, Key)>> = vec![None; new_calls.len()];
        let mut used = vec![false; old_calls.len()];
        for pair in &found {
            partner[pair.new] = Some((pair.old, pair.key));
            used[pair.old] = true;
        }
        let mut pairs: Vec<(Option<usize>, Option<usize>, Option<Key>)> = partner.into_iter().enumerate().map(|(index, found)| (found.map(|(at, _)| at), Some(index), found.map(|(_, key)| key))).collect();
        pairs.extend(used.iter().enumerate().filter(|(_, used)| !**used).map(|(at, _)| (Some(at), None, None)));
        for (x, y, key) in pairs {
            let old = x.map(|x| &old_calls[x]);
            let new = y.map(|y| &new_calls[y]);
            // A call in an expansion gives a line only in a pair with an outer call.
            if old.is_none_or(|call| call.nested) && new.is_none_or(|call| call.nested) {
                continue;
            }
            let at = [old.and_then(|call| call.at.clone()), new.and_then(|call| call.at.clone())];
            let (old, new) = (old.map(|call| call.expansion), new.map(|call| call.expansion));
            if let (Some(o), Some(n)) = (old, new)
                && o.text == n.text
            {
                push(&mut equal, old, new, at, unit, keep_equal, key);
                continue;
            }
            push(&mut sites, old, new, at, unit, true, key);
        }
    }
    (sites, equal)
}

/// A changed expansion for the owner pass (R63.9-5): the items that hold the call on each side,
/// the macros of its chain, and the files that its code reads.
#[derive(Clone, Debug, Default)]
pub struct Link {
    /// The item of the anchor of the call on each side and the items that its code makes (the
    /// old ones with the rename map).
    pub items: BTreeSet<String>,
    /// The definition places of the macros of the chain in the tree, on each side (the macro of
    /// the call and each macro that its code uses): the side (0 for the old tree), the file, the
    /// line, and true for a proc macro (R73.2-1).
    pub defs: BTreeSet<(usize, String, usize, bool)>,
    /// The files that the code of the call reads on the two sides.
    pub reads: BTreeSet<String>,
    /// The lines of the item that holds the call on each side: the side, the file, the first
    /// and the last line (R73.2-1: a body inside them has the expansion as an input).
    pub ranges: BTreeSet<(usize, String, usize, usize)>,
}

/// The changed expansions of a step (R63.9-5), for the owner pass: only a pair of two calls
/// with two different texts. An added or a removed call is an edit of the call or of its item.
pub fn links(step: &Step, trees: [&Workspace; 2], bodies: &dyn crate::BodyOracle, renames: &BTreeMap<String, String>) -> Vec<Link> {
    let Some(units) = bodies.expansions(step) else { return Vec::new() };
    let [old_units, new_units] = &*units;
    let map = |path: &str| renames.get(path).cloned().unwrap_or_else(|| path.to_string());
    let (sites, _) = pair_sites([&old_units, &new_units], trees, &map, false);
    let mut out = Vec::new();
    for site in &sites {
        let (Some(old), Some(new)) = (site.old, site.new) else { continue };
        let mut link = Link::default();
        for (side, at) in site.at.iter().enumerate() {
            if let Some(at) = at {
                link.items.insert(if side == 0 { map(&at.item) } else { at.item.clone() });
                if let Some(item) = trees[side].items.get(&at.item) {
                    link.ranges.insert((side, item.file.clone(), item.line_start, item.line_end));
                }
            }
        }
        for (side, expansion) in [old, new].into_iter().enumerate() {
            // R73.2-1: the items that the code of the call makes are on the edge of the call too
            // (a body that a call makes, also when the step has no list of the made sites).
            link.items.extend(expansion.items.iter().map(|item| if side == 0 { map(item) } else { item.clone() }));
            let mut all = vec![[expansion.name.clone(), expansion.def.clone()]];
            all.extend(expansion.uses.iter().cloned());
            for [name, def] in all {
                if let Some((file, line)) = inputs::place_of(&def) {
                    let proc = expansion.ids.iter().any(|id| id.starts_with(&format!("{name}: proc macro ")));
                    link.defs.insert((side, file.to_string(), line, proc));
                }
            }
            link.reads.extend(expansion.reads.iter().cloned());
        }
        out.push(link);
    }
    out
}

/// Adds the facts of the expanded code to a step (R63.2-1 to R63.3-5).
pub(crate) fn apply(input: &StepInput, bodies: &dyn crate::BodyOracle, exprs: &dyn ExprOracle, outcome: &mut StepOutcome) {
    let Some(units) = bodies.expansions(input.step) else {
        return;
    };
    let [old_units, new_units] = &*units;
    let map = |path: &str| outcome.renames.items.get(path).cloned().unwrap_or_else(|| path.to_string());
    let (sites, equal) = pair_sites([&old_units, &new_units], [input.before, input.after], &map, false);
    order_spotlights(input, outcome, &sites, &equal);
    let all_configs: BTreeSet<&str> = new_units.iter().chain(old_units).map(|unit| unit.config.as_str()).collect();
    let mut flow = outcome.report.flow.take().unwrap_or_default();
    // R65.10-1: the identity of each package of a registry or of git in `Cargo.lock`.
    let locks = [lock_identities(input.before), lock_identities(input.after)];
    // R73.1-1: each entry of `Cargo.lock`, also a path package, for the kind of a crate outside
    // the tree.
    let entries = [inputs::lock_entries(input.before), inputs::lock_entries(input.after)];
    let dependency = |expansion: &Expansion| dependency_change(expansion, &locks);
    let lock_facts = |facts: &[semilla_core::report::Fact], name: &str| -> Vec<usize> { facts.iter().enumerate().filter(|(_, fact)| lock_fact_names(fact, name)).map(|(index, _)| index).collect() };
    // R65.10-2: the equal calls of each macro of a package whose identity changes: one count line
    // under the fact of the package.
    let mut stay: BTreeMap<(String, String), BTreeSet<(String, usize, usize)>> = BTreeMap::new();
    for site in &equal {
        if let Some(new) = site.new
            && let Some((name, Some(_))) = dependency(new)
        {
            stay.entry((name, macro_text(new))).or_default().insert((new.file.clone(), new.line, new.col));
        }
    }
    // A call of a library and of its test build, in each configuration, is one call.
    let equal: BTreeSet<(String, usize, usize)> = equal.iter().filter_map(|site| site.new).map(|e| (e.file.clone(), e.line, e.col)).collect();
    flow.expansions_equal = equal.len();
    let mut cache: BTreeMap<(String, String), Result<Vec<(String, Category)>, String>> = BTreeMap::new();
    let mut by_fact: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    for site in &sites {
        let any = site.new.or(site.old).expect("a site has a side");
        // R65.1-4: a call whose expansion changes only by its context, on the evidence of the
        // compiler: the texts with the marks of the context are equal, the call text is equal,
        // the identities of the definitions are equal, no definition of the workspace in the
        // chain changes, and no fact has an edit in the input of the call.
        //
        // R71.1-3: an edit in the input of the call does not stop the rule when each changed
        // value of the context moves with the call (a line or a column of the call, or the name
        // of its item after a rename): the edit has its own fact, and the expansion changes only
        // by the place. Such a line goes under no fact: the step section has one line for the
        // calls of the macro.
        let names = [0, 1].map(|side| site.at[side].as_ref().map(|at| at.item.rsplit("::").next().unwrap_or(&at.item).to_string()).unwrap_or_default());
        // R73.2-1: the definitions of the chain as inputs, for a changed call.
        let chain = if site.old.is_some() && site.new.is_some() { chain_inputs(input, outcome, [site.old, site.new], &entries) } else { Chain::default() };
        let moved = match (site.old, site.new) {
            (Some(old), Some(new)) if chain.owners.is_empty() && chain.known.is_empty() && (input_facts(input, outcome, site).is_empty() || moves_with_call(old, new, &names)) => context_only(old, new),
            _ => None,
        };
        let (result, facts) = match (site.old, site.new) {
            // R66.11-1: a text with a part that the driver cannot print is never equal to another
            // text. The line names the cause, with no compare and no proof.
            (Some(old), Some(new)) if !not_known(old).is_empty() || !not_known(new).is_empty() => {
                let mut causes = not_known(old);
                for cause in not_known(new) {
                    if !causes.contains(&cause) {
                        causes.push(cause);
                    }
                }
                (NOT_KNOWN_RESULT, causes.iter().map(|cause| format!("not known: {cause}")).collect())
            }
            (Some(old), Some(new)) => {
                if let Some(moved) = moved.clone() {
                    ("expansion changed", vec![moved])
                } else if old.text.len().max(new.text.len()) > LARGE_EXPANSION {
                    // A large text gets no compare: the line gives the changed lines, the sizes,
                    // and the command that prints the full text (R71.1-1).
                    ("expansion changed", vec![large_text(old, new, outcome.report.step)])
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
        // R73.2-1: the owner of the line is the set of the changed inputs that reach it, with the
        // nearest facts of each one: the definitions of the chain (a changed definition owns a
        // changed expansion; an added or a removed call belongs to the edit of the call), the
        // input of the call, and the files that its code reads.
        let Chain { mut owners, causes, known, mut not_seen } = chain;
        // R65.10-2: a changed expansion of a macro of a package whose identity changes in
        // `Cargo.lock` (a new version, source, or checksum): the fact of the package owns it, as a
        // changed definition of the chain.
        let mut causes = causes;
        if let (Some(_), Some(new)) = (site.old, site.new)
            && let Some((name, Some(change))) = dependency(new)
        {
            let found: Vec<usize> = lock_facts(&outcome.report.facts, &name).into_iter().filter(|index| !owners.contains(index)).collect();
            if !found.is_empty() {
                owners.splice(0..0, found);
                causes.insert(0, (name.clone(), format!("the package `{name}` ({change})")));
            }
        }
        // R63.9-4: a changed definition of a call in the input of this call: the cause names
        // that call (`mkfn!(made, add!(4; 5))` changes through `add!(4; 5)`).
        let mut causes: Vec<String> = causes.into_iter().map(|(name, cause)| match inner_call(&sites, site, &name) {
            Some(call) if name != any.name => format!("{cause} through the inner call `{call}`"),
            _ => cause,
        }).collect();
        causes.extend(known);
        // The facts with an edit in the input of the call. A call that only moves (R71.1-3) has
        // no owner of its input. A call with no place has an input that Semilla does not see.
        let input_owners: Vec<usize> = if moved.is_some() { Vec::new() } else { input_facts(input, outcome, site).into_iter().filter(|index| !owners.contains(index)).collect() };
        if any.file.is_empty() {
            not_seen.push(format!("the input of the call (its tokens come from a macro with no record: `{}`)", any.via));
        }
        let mut cause = causes.iter().map(|cause| if cause.starts_with("the definition of ") { cause.clone() } else { format!("from {cause}") }).collect::<Vec<_>>().join(", ");
        if !input_owners.is_empty() {
            let word = match result {
                "expansion added" => "the step adds the call or its input",
                "expansion removed" => "the step removes the call or its input",
                _ => "the input of the call changes",
            };
            cause = if cause.is_empty() { word.to_string() } else { format!("{cause}; {word}") };
        }
        owners.extend(input_owners);
        // R63.9-6, R73.1-1: a file that the code of the call reads (`include_str!`,
        // `include_bytes!`, `include!`) is an input: a changed file owns the expansion with the
        // facts of the file. A file outside the tree is an input that Semilla does not see.
        {
            let reads: BTreeSet<&String> = [site.old, site.new].iter().flatten().flat_map(|e| e.reads.iter()).collect();
            for file in reads {
                match inputs::file_state([input.before, input.after], file) {
                    State::Changed => {
                        let found: Vec<usize> = inputs::file_facts(&outcome.report.facts, file).into_iter().filter(|index| !owners.contains(index)).collect();
                        if !found.is_empty() {
                            inputs::extend(&mut owners, found);
                            let text = format!("the code of the call reads `{file}`, which the step changes");
                            cause = if cause.is_empty() { text } else { format!("{cause}; {text}") };
                        }
                    }
                    State::NotSeen(text) => not_seen.push(text),
                    State::Same => {}
                }
            }
        }
        // No owner fact (a definition outside the tree with a new identity, a call that moves,
        // or no changed input): only an indirect fact of the body that holds the call owns the
        // line (R63.9-7). Each other fact of that body has a different cause. R73.3-1: the
        // negative sentence shows only with full data; else the cause is not known.
        if owners.is_empty() {
            let holder = site.at[1].as_ref().or(site.at[0].as_ref()).map(|at| at.item.clone()).unwrap_or_default();
            owners.extend(outcome.report.facts.iter().enumerate().filter(|(_, fact)| !holder.is_empty() && fact.part == "indirect change" && fact.affected.contains(&holder)).map(|(index, _)| index));
            if moved.is_some() {
                cause = "the context of the call changes".to_string();
            } else if cause.is_empty() {
                // R73.3-1: the text of the call is an input too. A call of one side only, or two
                // call texts that differ, is a changed input with no fact at its place (a pair of
                // two different calls): never the negative sentence.
                cause = match (site.old, site.new) {
                    (None, Some(_)) => "the call is only in the tree after the step, and no fact has its place".to_string(),
                    (Some(_), None) => "the call is only in the tree before the step, and no fact has its place".to_string(),
                    (Some(old), Some(new)) if old.call != new.call => format!("the text of the call changes ({}), and no fact has its place", changed_text(&old.call, &new.call)),
                    _ => inputs::no_owner_cause(&not_seen, NO_CHANGED_INPUT),
                };
            }
        }
        // R72.1-3: a pair of two calls by a weak key says so.
        if let Some(key) = site.key.filter(Key::weak).filter(|_| site.old.is_some() && site.new.is_some()) {
            let words = format!("the two calls are {}", key.words());
            cause = if cause.is_empty() { words } else { format!("{cause}; {words}") };
        }
        let configs = if site.configs.len() == all_configs.len() { Vec::new() } else { site.configs.clone() };
        let shown_call = shown(any);
        let summary = facts.join("; ");
        let key = format!("{result}: {}: {summary}{}", definition_text(any), if cause.is_empty() { String::new() } else { format!("; {cause}") });
        let shown_text = |e: Option<&Expansion>| e.map(|e| semilla_core::text::without_unique(&e.text)).unwrap_or_default();
        let diff = cut_diff(&line_diff(&shown_text(site.old), &shown_text(site.new)), DIFF_LIMIT, &semilla_core::budget::expansion_command(&place, outcome.report.step));
        let owner_ids: Vec<String> = owners.iter().map(|index| outcome.report.facts[*index].id.clone()).collect();
        // R63.9-7: the line goes under one fact, the first owner (a changed definition of the
        // chain, else an edit of the input, else a changed file that the code reads). The line
        // names the other owners; their own lines do not repeat it.
        // R65.2-2: an added or a removed expansion that makes no item is code of the edit of
        // the call, which the fact of the call shows: the line is only in the step section. An
        // added or a removed expansion that makes items, and each changed expansion, go under
        // the fact.
        let under_fact = result == "expansion changed" || result == NOT_KNOWN_RESULT || !any.items.is_empty();
        if let Some((first, others)) = owner_ids.split_first().filter(|_| under_fact) {
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
        if result == "expansion changed" || result == NOT_KNOWN_RESULT {
            let mark = format!("; the expansion of {shown_call} changes ({})", semilla_core::budget::expansion_command(&place, outcome.report.step));
            for body in flow.bodies.iter_mut().filter(|body| at_call(body) && body.line.contains("no edit in the body") && !body.line.contains("; the expansion of ")) {
                body.line.push_str(&mark);
            }
        }
        flow.expansions.push(ExpansionSite { call: shown_call, at, place, definition: definition_text(any), result: result.to_string(), configs, facts, cause, owners: owner_ids, diff, key, bodies });
    }
    // The lines of each fact (R63.3-3): equal changes of one macro fold into one line with the
    // count (R63.3-4). R76.1-1: a count names the command of the step: the expansion section of
    // its full text has each call.
    let full_step = semilla_core::budget::step_text_command(outcome.report.step);
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
                format!("{} calls of `{name}`: {result}: {summary}: {}{more} ({full_step})", list.len(), places.join(", "))
            })
            .collect();
        let more = out.len().saturating_sub(crate::flow::FACT_LINES);
        out.truncate(crate::flow::FACT_LINES);
        if more > 0 {
            out.push(format!("… {more} more ({full_step})"));
        }
        flow.expansion_lines.insert(id, out);
    }
    // R65.10-2: the count of the equal calls of each macro of a package whose identity changes,
    // under the fact of the package.
    for ((name, shown), calls) in &stay {
        let Some(&first) = lock_facts(&outcome.report.facts, name).first() else { continue };
        let id = outcome.report.facts[first].id.clone();
        let line = format!("{} expansion{} of {shown} stay{} the same", calls.len(), if calls.len() == 1 { "" } else { "s" }, if calls.len() == 1 { "s" } else { "" });
        flow.expansion_lines.entry(id).or_default().push(line);
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

/// R65.9-1: the spotlight of a member order change of a type with a derive or an attribute
/// macro (R65.3-3) goes away when the expansion records show that the order changes no code that
/// a macro makes: each derive and each attribute macro of the type (on the two sides) has a
/// record at the type with the same text in the two trees, no call at the type has a changed,
/// an added, a removed, or a not known expansion, the type has no `#[repr]` and no `cfg_attr`,
/// no attribute macro moves across a derive (R65.9-2), and no implicit discriminant value of an
/// enum changes (a derive reads it with no text that names it). The fact keeps its text and gets
/// a note. Each other spotlight stays.
fn order_spotlights(input: &StepInput, outcome: &mut StepOutcome, sites: &[Site], equal: &[Site]) {
    let renames = outcome.renames.items.clone();
    let map = |path: &str| renames.get(path).cloned().unwrap_or_else(|| path.to_string());
    for fact in outcome.report.facts.iter_mut().filter(|fact| fact.part == "member order" && fact.spotlight) {
        let Some(path) = fact.at.as_ref().map(|at| at.item.clone()) else { continue };
        let old_path = renames.iter().find(|(_, to)| **to == path).map(|(from, _)| from.clone()).unwrap_or_else(|| path.clone());
        let (Some(old_item), Some(new_item)) = (input.before.items.get(&old_path), input.after.items.get(&path)) else { continue };
        let (old_attrs, new_attrs) = (old_item.attrs.join(" "), new_item.attrs.join(" "));
        // Only the spotlight of R65.3-3 alone: a spotlight with an other cause stays.
        let reader = crate::facts::order_reader(&new_attrs).or_else(|| crate::facts::order_reader(&old_attrs));
        if reader.is_none() || fact.spotlight_why != reader {
            continue;
        }
        if fact.description.contains("the discriminant values of the variants with no explicit value change") {
            continue;
        }
        let (old_inputs, new_inputs) = (crate::facts::order_inputs(&old_attrs), crate::facts::order_inputs(&new_attrs));
        if old_inputs.fixed || new_inputs.fixed || crate::facts::attribute_macro_moves(&old_attrs, &new_attrs, crate::records::inert_attributes([input.before, input.after], [Some(&semilla_core::Anchor::item(&old_path)), Some(&semilla_core::Anchor::item(&path))]).as_ref()).is_some() {
            continue;
        }
        // The calls at the type: the item of the anchor of the call, the old one with the rename
        // map, is the type or an item inside it.
        let at_type = |site: &Site| {
            let kind = site.new.or(site.old).map(|e| e.kind.as_str()).unwrap_or("");
            let inside = |item: &str| item == path || item.starts_with(&format!("{path}::"));
            matches!(kind, "derive" | "attr") && (site.at[1].as_ref().is_some_and(|at| inside(&at.item)) || site.at[0].as_ref().is_some_and(|at| inside(&map(&at.item))))
        };
        if sites.iter().any(at_type) {
            continue;
        }
        let made: BTreeSet<String> = equal.iter().filter(|site| at_type(site)).filter_map(|site| site.new.map(|e| e.name.rsplit("::").next().unwrap_or(&e.name).to_string())).collect();
        let makers: BTreeSet<&String> = old_inputs.makers.iter().chain(&new_inputs.makers).collect();
        if makers.is_empty() || !makers.iter().all(|name| made.contains(*name)) {
            continue;
        }
        let list = makers.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
        let note = format!("the member order needs no reason: each macro of the type ({list}) makes the same code in the two versions (the expansion records, R65.9-1)");
        fact.spotlight = false;
        fact.spotlight_why = None;
        fact.note = Some(match fact.note.take() {
            Some(old) => format!("{old}; {note}"),
            None => note,
        });
    }
}

/// The size in bytes above which the text of an expansion gets no compare (the user number,
/// 2026-11-25: 50 kB; the unit, 1000 bytes of the text with the marks of hygiene, is Decided by
/// Claude). The line of such an expansion has the form of each text over the budget (R71.1-1).
pub const LARGE_EXPANSION: usize = 50_000;

/// The line of a large changed expansion (R71.1-1; before batch 11c, R65.10-3 gave only the
/// sizes and the count of the lines that differ): the fact rules do not run on the text, and
/// the line gives each changed line with its line number in the text (a long run shows its
/// changed tokens), the sizes, and the command that prints the full text. The words are not a
/// compare result (the line is in the syntax-only check too, R83.1-3).
fn large_text(old: &Expansion, new: &Expansion, step: usize) -> String {
    let place = if new.file.is_empty() { NO_PLACE.to_string() } else { format!("{}:{}", new.file, new.line) };
    // R76.1-1: the command names the step, so that it works in each state after the step.
    format!("the text is too large for the fact rules (over {} kB): {}", LARGE_EXPANSION / 1000, semilla_core::budget::inline_pair(&old.text, &new.text, Some(&semilla_core::budget::expansion_command(&place, step)), false))
}

/// R65.10-1: the identity of each package of `Cargo.lock` with a source (a registry or a
/// repository), by its crate name (`-` gives `_`): the set of its versions with the source and
/// the checksum. `None`: the tree has no readable `Cargo.lock`. A package with no source is a
/// package of the workspace or a path: its code is in the tree, so its facts show its changes.
fn lock_identities(ws: &Workspace) -> Option<BTreeMap<String, BTreeSet<String>>> {
    let mut out = inputs::lock_entries(ws)?;
    for entries in out.values_mut() {
        entries.retain(|entry| !entry.starts_with("path "));
    }
    out.retain(|_, entries| !entries.is_empty());
    Some(out)
}

/// R65.10-1: the package of the definition of a macro of a dependency, with the text of its
/// change if its identity changes (`None`: the same identity). `None` for a macro of the
/// workspace, of the standard library, or of a path package, and for a tree with no
/// `Cargo.lock`: the rules of design 63 and 65 hold for them.
fn dependency_change(expansion: &Expansion, locks: &[Option<BTreeMap<String, BTreeSet<String>>>; 2]) -> Option<(String, Option<String>)> {
    let name = expansion.def.strip_prefix("crate ")?;
    let [Some(old), Some(new)] = locks else { return None };
    let (a, b) = (old.get(name), new.get(name));
    if a.is_none() && b.is_none() {
        return None;
    }
    if a == b {
        return Some((name.to_string(), None));
    }
    let versions = |set: Option<&BTreeSet<String>>| set.map(|set| set.iter().map(|entry| entry.split(' ').next().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).filter(|text| !text.is_empty()).unwrap_or_else(|| "none".to_string());
    let change = if versions(a) == versions(b) { format!("{}: a new source or checksum in `Cargo.lock`", versions(b)) } else { format!("{} → {} in `Cargo.lock`", versions(a), versions(b)) };
    Some((name.to_string(), Some(change)))
}

/// True for a fact of `Cargo.lock` about the package with this crate name (R65.10-2).
fn lock_fact_names(fact: &semilla_core::report::Fact, name: &str) -> bool {
    fact.file.as_deref() == Some("Cargo.lock") && fact.part.strip_prefix("Cargo.lock:package ").is_some_and(|rest| rest.split(' ').next().is_some_and(|package| package.replace('-', "_") == name))
}

/// The text of a macro for a count line (R65.10-2): `` `derive depot::Rows` ``, ``
/// `#[depot::tag]` ``, or `` `depot::here!` ``.
fn macro_text(expansion: &Expansion) -> String {
    let package = expansion.def.strip_prefix("crate ").unwrap_or("");
    let path = if package.is_empty() || expansion.name.contains("::") { expansion.name.clone() } else { format!("{package}::{}", expansion.name) };
    match expansion.kind.as_str() {
        "derive" => format!("`derive {path}`"),
        "attr" => format!("`#[{path}]`"),
        _ => format!("`{path}!`"),
    }
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
pub fn view(place: &str, units: [&[ExpansionUnit]; 2], trees: [&Workspace; 2], full: bool, sides: Sides, again: &str) -> Result<String, String> {
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
            (Some(old), Some(new)) if !not_known(old).is_empty() || !not_known(new).is_empty() => NOT_KNOWN_RESULT,
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
        // R66.11-1: a part that the driver cannot print shows its cause, with no unique word.
        let text_of = |expansion: Option<&Expansion>| expansion.map(|e| semilla_core::text::without_unique(&e.text)).unwrap_or_default();
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
            // R76.1-1: the command of this output with `--full` (its step and its commit).
            out.push_str(&format!("  … {} more lines: {again} --full\n", total - EXPAND_LIMIT));
        }
    }
    Ok(out)
}

/// The result of a call whose expansion has a part that the driver cannot print (R66.11-1).
pub const NOT_KNOWN_RESULT: &str = "expansion not known";

/// The causes of the parts of the text of an expansion that the driver cannot print (R66.11-1).
/// A part of the text with the marks of the context (`placeless`) only stops R65.1-4: the text
/// itself is known, so its compare stays.
fn not_known(expansion: &Expansion) -> Vec<String> {
    semilla_core::text::unknown_causes(&expansion.text)
}

/// The negative sentence of a changed expansion with full data and no changed input (R63.9-7,
/// R73.3-1).
pub const NO_CHANGED_INPUT: &str = "no changed definition, no changed input of the call, and no changed file that its code reads";

/// The place text of a call with no place in a file of the workspace (R63.9-3).
pub const NO_PLACE: &str = "no place: the tokens of the call are not in a file of the workspace";

/// A call inside the input of a different call (R63.9-4): a changed site of the macro `name`
/// whose place is inside the place of the call of `site`.
fn inner_call(sites: &[Site], site: &Site, name: &str) -> Option<String> {
    let outer = site.new.or(site.old)?;
    sites.iter().filter(|other| !std::ptr::eq(*other, site) && other.old.is_some() && other.new.is_some()).filter_map(|other| other.new).find(|inner| inner.name == name && !outer.file.is_empty() && inner.file == outer.file && outer.r[0] <= inner.r[0] && inner.r[1] <= outer.r[1] && inner.r != outer.r).map(|inner| inner.call.clone())
}

/// The start of the text of a call whose expansion changes only by its context (R65.1-4).
pub const CONTEXT: &str = "only the context of the call changes";

/// R65.1-4 (R63.9-8 is one case of it): the text of a changed call whose definitions and input
/// are equal. Each token of an expansion comes from a definition of the chain, from the input
/// of the call, or from the context of the call (R65.1-1). The driver gives each expansion a
/// second text (`placeless`) with a mark for each token of the context, and the source and the
/// value of each mark (`context`). With equal identities of the definitions that no fact can
/// show (`ids`: a macro of another crate, a proc macro), an equal call text, and equal texts
/// with the marks, each difference is in a token of the context: the text names the source of
/// each mark whose value changes ("the line of the call", "the environment variable `X`",
/// "the file `data.txt`", "the output of the proc macro `Rows`"). The caller checks the
/// definitions of the workspace (no changed `macro_rules!` of the chain) and the input (no
/// fact with an edit in it). A number or a text that only looks like the line is not
/// evidence: a record with no mark never gives this text. `None` for each other pair.
pub fn context_only(old: &Expansion, new: &Expansion) -> Option<String> {
    if old.text == new.text || old.call != new.call || old.ids != new.ids {
        return None;
    }
    if old.placeless.is_empty() || old.placeless != new.placeless || old.context.len() != new.context.len() {
        return None;
    }
    // R66.11-1: a part that the driver cannot print is never evidence.
    if [old, new].iter().any(|e| !not_known(e).is_empty() || !semilla_core::text::unknown_causes(&e.placeless).is_empty()) {
        return None;
    }
    // The marks of the text are stable (the maker and the text of its call); the source of a
    // mark comes from a compare of its value with the place, so it is the source of the new side.
    let mut sources: Vec<&str> = Vec::new();
    for ([_, before], [source, after]) in old.context.iter().zip(&new.context) {
        if before != after && !sources.contains(&source.as_str()) {
            sources.push(source);
        }
    }
    // The numbers are in the diff of the call: the text is the same for each such call, so the
    // calls of one macro fold into one line.
    if sources.is_empty() {
        return None;
    }
    // A source of data (a read file, an environment variable, the module path, the output of a
    // proc macro) gives a new value to the code: the text shows it. A source of the place (the
    // line, the column) only moves the call, and its value is in the diff.
    let data = sources.iter().any(|source| ["the file `", "the environment variable `", "the module path", "the output of the proc macro"].iter().any(|kind| source.starts_with(kind)));
    let values = if data { format!("; {}", changed_text(&old.text, &new.text)) } else { String::new() };
    Some(format!("{CONTEXT}: {}{values}", sources.join(", ")))
}

/// R71.1-3: true if the two texts of a call differ only in values that move with the call: each
/// pair of different tokens is an integer (with or without a type suffix) that changes by the
/// line shift or by the column shift of the call, or a name (a word or a text) that changes only
/// as the name of the item of the call changes (`names`: the last name of the item on each
/// side). The texts must have the same tokens in each other place. Each other change is not a
/// move: a value that the input gives can change with an edit of the input.
fn moves_with_call(old: &Expansion, new: &Expansion, names: &[String; 2]) -> bool {
    let (a, b) = (semilla_core::budget::token_texts(&old.text), semilla_core::budget::token_texts(&new.text));
    if a.len() != b.len() {
        return false;
    }
    let number = |text: &str| -> Option<i64> { text.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '_').parse().ok() };
    let shifts = [new.line as i64 - old.line as i64, new.col as i64 - old.col as i64];
    let mut any = false;
    for (before, after) in a.iter().zip(&b) {
        if before == after {
            continue;
        }
        any = true;
        let moved = match (number(before), number(after)) {
            (Some(x), Some(y)) => y != x && shifts.contains(&(y - x)),
            _ => !names[0].is_empty() && names[0] != names[1] && before.replace(names[0].as_str(), &names[1]) == *after,
        };
        if !moved {
            return false;
        }
    }
    any
}

/// R63.9-8: the changed part of two texts on one line: the text between the equal start and
/// the equal end: "changed code `39u32` → `41u32`". A part over the budget shows its changed
/// tokens (R71.1-1).
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
    // R71.1-1: the changed part shows in full under the budget (before batch 11c: a cut at 80
    // characters with no command); over the budget, its changed tokens.
    let (old_part, new_part) = (a[from..to_a].iter().collect::<String>().trim().to_string(), b[from..to_b].iter().collect::<String>().trim().to_string());
    match semilla_core::budget::pair(&old_part, &new_part, None, false) {
        Some(semilla_core::budget::Form::Line(text)) => format!("changed code {text}"),
        _ => format!("changed code `{old_part}` → `{new_part}`"),
    }
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

    // R71.1-1, the part of the lines with the same origin: a call that moves one line down with
    // an edit in its input (a test with a body edit) changes only by values that move with the
    // call, so its line goes to the one line of the macro in the step section.
    // rule+ R71.1-1
    #[test]
    fn a_value_that_moves_with_the_call_is_a_move_and_another_value_is_not() {
        let at = |line: usize, text: &str| Expansion { line, col: 4, text: text.to_string(), ..expansion("items", "") };
        let names = ["two".to_string(), "two".to_string()];
        let old = at(12, "start_line: 13usize, start_col: 8usize, end_line: 13usize, name: \"tests::two\", ignore: false");
        let new = at(13, "start_line: 14usize, start_col: 8usize, end_line: 14usize, name: \"tests::two\", ignore: false");
        assert!(moves_with_call(&old, &new, &names));
        // A rename of the item: the name in a text follows the rename.
        let renamed = at(13, "start_line: 14usize, start_col: 8usize, end_line: 14usize, name: \"tests::three\", ignore: false");
        assert!(moves_with_call(&old, &renamed, &["two".to_string(), "three".to_string()]));
    }

    // rule- R71.1-1
    #[test]
    fn a_value_that_the_input_gives_is_not_a_move() {
        let at = |line: usize, text: &str| Expansion { line, col: 4, text: text.to_string(), ..expansion("items", "") };
        let names = ["two".to_string(), "two".to_string()];
        let old = at(12, "start_line: 13usize, ignore: false");
        // `#[ignore]`: a value that does not move with the call.
        assert!(!moves_with_call(&old, &at(12, "start_line: 13usize, ignore: true"), &names));
        // A number that changes by a different count than the line of the call.
        assert!(!moves_with_call(&old, &at(13, "start_line: 20usize, ignore: false"), &names));
        // The same place and the same text: no change, so no move.
        assert!(!moves_with_call(&old, &at(12, "start_line: 13usize, ignore: false"), &names));
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
        let step = Step { index: 0, intent: "code".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent: Default::default(), tags: Vec::new() };
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
        ExpansionUnit { config: "dev".into(), target: target.into(), package: "shop".into(), root: root.into(), cfg: Vec::new(), list, names: None }
    }

    // rule+ R72.1-1
    // rule- R72.1-1
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

    // rule+ R72.1-1
    // rule- R72.1-1
    #[test]
    fn a_target_with_a_new_name_pairs_by_its_root_file_and_its_calls() {
        // A binary target gets a new name: the same root file and the same calls.
        let old = vec![unit("tool.bin.aaaa", "src/main.rs", &[("src/main.rs", 2, "a"), ("src/main.rs", 3, "b")])];
        let new = vec![unit("toolbox.bin.bbbb", "src/main.rs", &[("src/main.rs", 2, "a"), ("src/main.rs", 3, "b")])];
        assert_eq!(unit_pairs(&old, &new), vec![(Some(0), Some(0))]);
        // A removed target and a new target with a different root file and other calls: no pair.
        let other = vec![unit("other.bin.cccc", "src/bin/other.rs", &[("src/bin/other.rs", 9, "z")])];
        assert_eq!(unit_pairs(&old, &other), vec![(None, Some(0)), (Some(0), None)]);
    }

    // rule+ R72.1-1
    // rule- R72.1-1
    #[test]
    fn a_call_is_identified_by_the_definition_of_its_macro() {
        let call = |name: &str, def: &str, ids: &[&str]| Expansion { name: name.into(), def: def.into(), ids: ids.iter().map(|id| id.to_string()).collect(), ..Default::default() };
        // `format!` and `alloc::format!`: one definition of the compiler.
        let a = call("format", "crate alloc", &["format: macro of crate alloc (definition 749bf28f6f9814cd)"]);
        let b = call("alloc::format", "crate alloc", &["alloc::format: macro of crate alloc (definition 749bf28f6f9814cd)"]);
        assert_eq!(macro_identity(&a), macro_identity(&b));
        // A different macro of the same crate is a different identity.
        let c = call("vec", "crate alloc", &["vec: macro of crate alloc (definition 13b1c1d7b0b0b0b0)"]);
        assert_ne!(macro_identity(&a), macro_identity(&c));
        // A macro of the workspace: the file of the definition and the last name, not the line.
        assert_eq!(macro_identity(&call("table", "src/lib.rs:1", &[])), macro_identity(&call("crate::table", "src/lib.rs:7", &[])));
        assert_ne!(macro_identity(&call("table", "src/lib.rs:1", &[])), macro_identity(&call("table", "src/other.rs:1", &[])));
    }

    // rule+ R63.9-8
    // rule- R63.9-8
    // rule+ R65.1-4
    // rule- R65.1-4
    #[test]
    fn a_call_that_only_changes_by_its_context_has_one_text_and_a_file_fact_gives_the_changed_text() {
        let at = |line: usize, text: &str, placeless: &str, value: &str| Expansion {
            file: "src/lib.rs".into(),
            line,
            col: 4,
            call: "m!()".into(),
            placeless: placeless.into(),
            context: if placeless.is_empty() { Vec::new() } else { vec![["the line of the call".into(), value.into()]] },
            ..expansion("stmts", text)
        };
        // The driver marks the literal of `line!()` with its source and its value.
        let mark = "(1, \"\u{0}the line of the call\u{0}\")";
        assert_eq!(context_only(&at(16, "(1, 16u32)", mark, "16u32"), &at(18, "(1, 18u32)", mark, "18u32")).as_deref(), Some("only the context of the call changes: the line of the call"));
        // A value of `env!` and of `module_path!()`: the source names the variable.
        let env = |value: &str| Expansion {
            call: "m!()".into(),
            placeless: "\"\u{0}the environment variable `CARGO_PKG_DESCRIPTION` (`env!(\"CARGO_PKG_DESCRIPTION\")`)\u{0}\"".into(),
            context: vec![["the environment variable `CARGO_PKG_DESCRIPTION` (`env!(\"CARGO_PKG_DESCRIPTION\")`)".into(), value.into()]],
            ..expansion("expr", value)
        };
        assert_eq!(context_only(&env("\"a\""), &env("\"b\"")).as_deref(), Some("only the context of the call changes: the environment variable `CARGO_PKG_DESCRIPTION` (`env!(\"CARGO_PKG_DESCRIPTION\")`); changed code `a` → `b`"));
        // The negative cases: a different change with the move, a changed literal that only
        // looks like the line (`5u32` → `6u32`, `"src/lib.rs:5"` → `"src/lib.rs:6"`, no mark of
        // the driver), a different call text, a call that does not move, a changed identity of
        // a definition of another crate, and a mark with a different source.
        assert_eq!(context_only(&at(16, "(1, 16u32)", mark, "16u32"), &at(18, "(2, 18u32)", "(2, \"\u{0}the line of the call\u{0}\")", "18u32")), None);
        assert_eq!(context_only(&at(5, "5u32", "", ""), &at(6, "6u32", "", "")), None);
        assert_eq!(context_only(&at(5, "\"src/lib.rs:5\"", "", ""), &at(6, "\"src/lib.rs:6\"", "", "")), None);
        let mut other = at(18, "(1, 18u32)", mark, "18u32");
        other.call = "m!(x)".into();
        assert_eq!(context_only(&at(16, "(1, 16u32)", mark, "16u32"), &other), None);
        assert_eq!(context_only(&at(16, "(1, 16u32)", mark, "16u32"), &at(16, "(1, 16u32)", mark, "16u32")), None);
        let (mut a, mut b) = (at(16, "(1, 16u32)", mark, "16u32"), at(18, "(1, 18u32)", mark, "18u32"));
        a.ids = vec!["here: macro of crate depot 1".into()];
        b.ids = vec!["here: macro of crate depot 2".into()];
        assert_eq!(context_only(&a, &b), None);
        // A mark of another maker (the text of a changed call) is a different text with marks.
        let c = at(18, "(1, 18u32)", "(1, \"\u{0}made by `other!()`\u{0}\")", "18u32");
        assert_eq!(context_only(&at(16, "(1, 16u32)", mark, "16u32"), &c), None);
        // The changed text of two texts, at word bounds.
        assert_eq!(changed_text("let a = f(39u32, x);", "let a = f(41u32, x);"), "changed code `39u32` → `41u32`");
        assert_eq!(changed_text("value_one + 1", "value_two + 1"), "changed code `value_one` → `value_two`");
    }

    // R66.11-1: a text with a part that the driver cannot print gives its cause, and it is never
    // evidence of a change of the context only (R65.1-4). The negative case: a known text with
    // known marks gives the text of R65.1-4 as before.
    // rule+ R66.11-1
    // rule- R66.11-1
    #[test]
    fn a_text_that_the_driver_cannot_print_is_never_evidence() {
        let at = |line: usize, text: &str, placeless: &str| Expansion {
            file: "src/lib.rs".into(),
            line,
            call: "m!()".into(),
            placeless: placeless.into(),
            context: vec![["the line of the call".into(), format!("{line}u32")]],
            ..expansion("stmts", text)
        };
        let mark = "(1, \"\u{0}the line of the call\u{0}\")";
        let unknown = |n: u32| format!("not known: the printer of the compiler cannot print an `else` that is not a block (unknown_print#9.{n}.1)");
        assert_eq!(context_only(&at(16, "(1, 16u32)", mark), &at(18, "(1, 18u32)", mark)).as_deref(), Some("only the context of the call changes: the line of the call"));
        assert_eq!(context_only(&at(16, "(1, 16u32)", &unknown(1)), &at(18, "(1, 18u32)", &unknown(2))), None);
        assert_eq!(context_only(&at(16, &unknown(3), mark), &at(18, &unknown(4), mark)), None);
        assert_eq!(not_known(&at(16, &format!("impl A {{ {} }}", unknown(5)), mark)), vec!["the printer of the compiler cannot print an `else` that is not a block".to_string()]);
        assert!(not_known(&at(16, "(1, 16u32)", &unknown(6))).is_empty());
    }

    #[test]
    fn a_virtual_file_has_the_modules_of_the_call() {
        let mut e = expansion("expr", "a + 1");
        e.module = "a::b".into();
        assert_eq!(virtual_file(&e), "mod a {\nmod b {\nfn expansion() {\na + 1\n}\n}\n}\n");
    }

    /// The oracle of a test: the expansion records of the two trees (batch 10n2).
    struct Records([Vec<ExpansionUnit>; 2]);

    impl crate::BodyOracle for Records {
        fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<crate::BodyComparison>> {
            Ok(Vec::new())
        }

        fn expansions(&self, _: &Step) -> Option<std::sync::Arc<[Vec<ExpansionUnit>; 2]>> {
            Some(std::sync::Arc::new(self.0.clone()))
        }
    }

    /// The lock file of a test with the package `depot` of a registry at `version`.
    fn lock(version: &str) -> String {
        format!("version = 4\n\n[[package]]\nname = \"depot\"\nversion = \"{version}\"\nsource = \"registry+https://example.org/index\"\nchecksum = \"{}\"\n\n[[package]]\nname = \"shop\"\nversion = \"0.1.0\"\ndependencies = [\"depot\"]\n", if version == "1.0.0" { "aa" } else { "bb" })
    }

    /// A tree of the crate `shop` with two calls of macros of the package `depot`: `m!` in
    /// `shop::f` (line 2) and `n!` in `shop::g` (line 5).
    fn dependency_tree(lock_text: &str, input: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndepot = \"1\"\n").unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), lock_text).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), format!("pub fn f() -> u32 {{\n    depot::m!({input})\n}}\npub fn g() -> u32 {{\n    depot::n!(2)\n}}\n")).unwrap();
        let ws = Workspace::load(dir.path()).unwrap();
        (dir, ws)
    }

    /// The records of one tree: the call of `m!` with `text`, and the call of `n!` with `2`.
    fn dependency_units(m_call: &str, text: &str) -> Vec<ExpansionUnit> {
        let call = |line: usize, name: &str, call: &str, text: &str| Expansion { file: "src/lib.rs".into(), line, col: 5, end_line: line, name: name.into(), def: "crate depot".into(), call: call.into(), ..expansion("expr", text) };
        vec![ExpansionUnit { config: "dev".into(), target: "shop.lib.aaaa".into(), package: "shop".into(), root: "src/lib.rs".into(), cfg: Vec::new(), list: vec![call(2, "m", m_call, text), call(5, "n", "depot::n!(2)", "2")] , names: None }]
    }

    /// The step outcome of a test of the dependency rules: each fact with its expansion lines.
    fn dependency_outcome(old: (&str, &str, &str, &str), new: (&str, &str, &str, &str)) -> (Vec<(String, Vec<String>)>, Vec<ExpansionSite>) {
        let (old_lock, old_input, old_call, old_text) = old;
        let (new_lock, new_input, new_call, new_text) = new;
        let ((_a, before), (_b, after)) = (dependency_tree(&lock(old_lock), old_input), dependency_tree(&lock(new_lock), new_input));
        let step = Step { index: 0, intent: "code".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent: Default::default(), tags: Vec::new() };
        let oracle = Records([dependency_units(old_call, old_text), dependency_units(new_call, new_text)]);
        let outcome = crate::verify_step_with(&StepInput { step: &step, before: &before, after: &after }, &oracle, &NoOracle, true);
        let flow = outcome.report.flow.clone().unwrap_or_default();
        let facts = outcome.report.facts.iter().map(|fact| (fact.part.clone(), flow.expansion_lines.get(&fact.id).cloned().unwrap_or_default())).collect();
        (facts, flow.expansions)
    }

    // rule+ R65.10-1
    // rule- R65.10-1
    #[test]
    fn the_identity_of_a_dependency_is_its_entry_in_the_lock_file() {
        let tree = |text: &str| {
            let (dir, ws) = dependency_tree(text, "1");
            drop(dir);
            lock_identities(&ws)
        };
        let (one, two) = (tree(&lock("1.0.0")), tree(&lock("1.0.1")));
        let call = Expansion { def: "crate depot".into(), ..expansion("expr", "1") };
        // A new version is a new identity, with the two versions in the text.
        assert_eq!(dependency_change(&call, &[one.clone(), two.clone()]), Some(("depot".to_string(), Some("1.0.0 → 1.0.1 in `Cargo.lock`".to_string()))));
        // The same version with a new checksum is a new identity too.
        let other = tree(&lock("1.0.0").replace("checksum = \"aa\"", "checksum = \"cc\""));
        assert_eq!(dependency_change(&call, &[one.clone(), other]).and_then(|(_, change)| change), Some("1.0.0: a new source or checksum in `Cargo.lock`".to_string()));
        // The negative cases: the same entry is the same identity; a package with no source (the
        // workspace, a path), a crate of the standard library, and a tree with no lock file are
        // not a dependency of this rule.
        assert_eq!(dependency_change(&call, &[one.clone(), one.clone()]), Some(("depot".to_string(), None)));
        assert_eq!(dependency_change(&Expansion { def: "crate shop".into(), ..call.clone() }, &[one.clone(), two.clone()]), None);
        assert_eq!(dependency_change(&Expansion { def: "crate core".into(), ..call.clone() }, &[one.clone(), two.clone()]), None);
        assert_eq!(dependency_change(&Expansion { def: "src/lib.rs:1".into(), ..call.clone() }, &[one.clone(), two.clone()]), None);
        assert_eq!(dependency_change(&call, &[None, two]), None);
    }

    // rule+ R65.10-2
    // rule- R65.10-2
    #[test]
    fn a_dependency_expansion_is_a_full_fact_only_when_its_identity_or_its_input_changes() {
        // A new version of the package: the fact of `Cargo.lock` owns the changed expansion of
        // `m!`, and the equal call of `n!` is one count line under the same fact.
        let (facts, sites) = dependency_outcome(("1.0.0", "1", "depot::m!(1)", "1 + 100"), ("1.0.1", "1", "depot::m!(1)", "1 + 101"));
        let lock_lines = facts.iter().find(|(part, _)| part.starts_with("Cargo.lock:package depot")).map(|(_, lines)| lines.clone()).expect("a fact of the lock file");
        assert!(lock_lines.iter().any(|line| line.starts_with("`depot::m!(1)` at `shop::f") && line.contains("expansion changed: changed literal `100` → `101`")), "{lock_lines:?}");
        assert!(lock_lines.contains(&"1 expansion of `depot::n!` stays the same".to_string()), "{lock_lines:?}");
        assert!(sites.iter().any(|site| site.cause.starts_with("from the package `depot` (1.0.0 → 1.0.1 in `Cargo.lock`)")), "{sites:?}");
        // The negative case: the same identity and the same input. The changed text is no fact of
        // the lock file (it has no fact) and no count line; the step section keeps the call with
        // the cause "no changed definition".
        let (facts, sites) = dependency_outcome(("1.0.0", "1", "depot::m!(1)", "1 + 100"), ("1.0.0", "1", "depot::m!(1)", "1 + 101"));
        assert!(facts.iter().all(|(_, lines)| lines.is_empty()), "{facts:?}");
        assert!(sites.iter().any(|site| site.result == "expansion changed" && site.cause.starts_with("no changed definition")), "{sites:?}");
        // A changed input with the same identity: the fact of the edit of the call owns the full
        // line (a count line would hide the change).
        let (facts, _) = dependency_outcome(("1.0.0", "1", "depot::m!(1)", "1 + 100"), ("1.0.0", "3", "depot::m!(3)", "3 + 100"));
        let owner = facts.iter().find(|(_, lines)| !lines.is_empty()).expect("an owner of the changed expansion");
        assert!(!owner.0.starts_with("Cargo.lock"), "{facts:?}");
        assert!(owner.1.iter().any(|line| line.contains("`depot::m!(3)`") && line.contains("expansion changed: changed literal `1` → `3`")), "{facts:?}");
        assert!(facts.iter().flat_map(|(_, lines)| lines).all(|line| !line.contains("stay")), "{facts:?}");
    }

    /// The step section of a changed call of `m!` whose definition is `def`, with the identity
    /// `ids` on each side (R73.1-1, R73.3-1). The lock file and the input do not change.
    fn identity_sites(def: &str, ids: [&[&str]; 2]) -> Vec<ExpansionSite> {
        let ((_a, before), (_b, after)) = (dependency_tree(&lock("1.0.0"), "1"), dependency_tree(&lock("1.0.0"), "1"));
        let step = Step { index: 0, intent: "code".into(), claims: Vec::new(), tree_before: "a".into(), tree_after: "b".into(), reasons: Default::default(), syntax_only: true, build_check: None, agent: Default::default(), tags: Vec::new() };
        let units = |text: &str, ids: &[&str]| {
            let mut units = dependency_units("depot::m!(1)", text);
            units[0].list[0].def = def.to_string();
            units[0].list[0].ids = ids.iter().map(|id| id.to_string()).collect();
            units
        };
        let oracle = Records([units("1 + 100", ids[0]), units("1 + 101", ids[1])]);
        let outcome = crate::verify_step_with(&StepInput { step: &step, before: &before, after: &after }, &oracle, &NoOracle, true);
        outcome.report.flow.unwrap_or_default().expansions
    }

    // R73.3-1, R73.1-1: the negative sentence shows only when Semilla has the hash of each input
    // of the call. A path package outside the tree (`shop` has no source in `Cargo.lock`) with an
    // equal hashed identity is seen; with no identity, or with an identity that is not a hash, the
    // cause is not known; a new hash is a known change with no fact.
    // rule- R73.3-1
    // rule- R73.1-1
    #[test]
    fn a_negative_sentence_needs_the_hash_of_each_input() {
        let cause = |def: &str, ids: [&[&str]; 2]| identity_sites(def, ids).into_iter().find(|site| site.result == "expansion changed").map(|site| site.cause).unwrap_or_default();
        let hash = "m: macro of crate shop (definition 00000000000000ab)";
        assert_eq!(cause("crate shop", [&[hash], &[hash]]), NO_CHANGED_INPUT);
        // A registry package with the same entry in `Cargo.lock`: the entry is the identity.
        assert_eq!(cause("crate depot", [&[], &[]]), NO_CHANGED_INPUT);
        let not_known = cause("crate shop", [&[], &[]]);
        assert!(not_known.starts_with("the cause is not known: Semilla does not see the definition of `m` (crate shop)"), "{not_known}");
        let no_hash = "m: a macro of this crate that is not `macro_rules!`";
        assert!(cause("crate shop", [&[no_hash], &[no_hash]]).starts_with(inputs::NOT_KNOWN));
        let new = cause("crate shop", [&[hash], &["m: macro of crate shop (definition 00000000000000cd)"]]);
        assert!(new.contains("the definition of `m` (crate shop, a path package outside the tree) has a new identity"), "{new}");
        // The text of the call is an input too: two call texts that differ with no fact at the
        // place of the call (a pair of two different calls) are a changed input.
        let (_, sites) = dependency_outcome(("1.0.0", "1", "depot::m!(1)", "1 + 100"), ("1.0.0", "1", "depot::m!(2)", "2 + 100"));
        let changed = sites.iter().find(|site| site.result == "expansion changed").map(|site| site.cause.clone()).unwrap_or_default();
        assert!(changed.starts_with("the text of the call changes (") && changed.ends_with("), and no fact has its place"), "{changed}");
        // A definition in a file that no tree of the step has is not seen either.
        assert!(cause("gen/mac.rs:1", [&[], &[]]).starts_with("the cause is not known: Semilla does not see the file `gen/mac.rs`, which no tree of the step has"));
    }

    // rule+ R71.1-1
    // rule- R71.1-1
    #[test]
    fn a_large_expansion_gives_its_size_and_the_command_and_no_compare() {
        let big = |value: u32| (0..3000).map(|line| format!("let v{line} = {};", if line == 7 { value } else { line })).collect::<Vec<_>>().join("\n");
        let (old, new) = (big(1), big(2));
        assert!(old.len() > LARGE_EXPANSION, "{}", old.len());
        let (_, sites) = dependency_outcome(("1.0.0", "1", "depot::m!(1)", &old), ("1.0.0", "1", "depot::m!(1)", &new));
        let site = sites.iter().find(|site| site.result == "expansion changed").expect("a changed expansion");
        // R71.1-1 (before batch 11c, R65.10-3 gave only the count of the lines that differ):
        // the changed line, its line number, the sizes, and the command.
        assert_eq!(site.facts, vec![format!("the text is too large for the fact rules (over 50 kB): `let v7 = 1;` → `let v7 = 2;` (line 8) (a large text: 3000 → 3000 lines, {:.1} kB → {:.1} kB; 1 changed part; the full text: semilla expand src/lib.rs:2 --step 0)", old.len() as f64 / 1000.0, new.len() as f64 / 1000.0)]);
        // The negative case: a small text has the facts of the compare.
        let (_, sites) = dependency_outcome(("1.0.0", "1", "depot::m!(1)", "let v7 = 1;"), ("1.0.0", "1", "depot::m!(1)", "let v7 = 2;"));
        let site = sites.iter().find(|site| site.result == "expansion changed").expect("a changed expansion");
        assert!(site.facts.iter().any(|fact| fact.contains("changed literal `1` → `2`")), "{:?}", site.facts);
    }
}
