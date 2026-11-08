//! The normal form ladder (design section 8.2): read the records of the MIR driver, and compare
//! the bodies of two versions of a workspace.
//!
//! The driver (`crates/semilla-mir`) writes one record for each body and level. This crate:
//!
//! 1. Reads the records (`read_records`), or runs `cargo check` with the driver to make them
//!    (`run_cargo_check`).
//! 2. Compares each "before" body with its "after" body (`compare`). It tries the levels in
//!    order and stops at the first level where the two bodies are equal:
//!    - **N0:** the canonical MIR text at `-Zmir-opt-level=0` is the same.
//!    - **N1:** the N0 text is the same after the rename map of the claims, or after each call of
//!      a local function is replaced by the content hash of that function (see `hash.rs`).
//!    - **N2:** the same compare on the rustc optimized MIR.
//!    - **N3:** the same compare on the N2 MIR after bounded inlining.
//!    - **N4:** equality saturation over the body terms (see `n4.rs`), for loop-free bodies.
//!
//! All levels apply the rename map to the "before" side first (design 8.3), except N0, which
//! compares the raw text.

pub mod cargo;
pub mod hash;
pub mod n4;
pub mod paths;

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use cargo::{run_cargo_check, run_cargo_check_delta};

/// One record of the driver: the normalized MIR of one body at one level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyRecord {
    /// The full def path, with the crate name first (for example `mycrate::cart::Cart::total`).
    pub name: String,
    /// `n0`, `n2`, or `n3`.
    pub level: String,
    /// The canonical MIR text.
    pub text: String,
    /// The shape of the control flow graph: the terminator kinds and the successors, without the
    /// statements.
    #[serde(default)]
    pub shape: String,
    /// The def paths of the local functions that the body calls or refers to.
    #[serde(default)]
    pub callees: Vec<String>,
    /// True if the control flow graph has no cycle.
    #[serde(default)]
    pub loop_free: bool,
    /// The N4 term (only for a loop-free body).
    #[serde(default)]
    pub term: Option<String>,
    /// The source span of the body.
    #[serde(default)]
    pub span: Option<SpanInfo>,
    /// The compiled target, `<crate>.<kind>.<metadata hash>` (the same for both versions of a
    /// workspace). A body name can exist in more than one target (a lib and its test build).
    /// Empty for the command mode of the driver and for old records.
    #[serde(default)]
    pub target: String,
}

/// The source span of a body. The file path is as rustc gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanInfo {
    pub file: String,
    pub line_start: usize,
    pub line_end: usize,
}

/// The records of one version, by level name (`n0`, `n2`, `n3`).
pub type Levels = BTreeMap<String, Vec<BodyRecord>>;

/// The result of the compare of one body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyStatus {
    /// The bodies are equal at `level` (`n0` .. `n4`). For N4, `proof` holds the rule chain. For
    /// N1, it says which substitution made the bodies equal (`rename-map` or `content-hash`).
    Equal { level: String, proof: Vec<String> },
    /// No level makes the bodies equal. `cfg_changed` is true if the N0 control flow shapes are
    /// different (a change of the control flow, not only of the logic).
    Different { cfg_changed: bool },
    /// The body exists only before (after the rename map).
    BeforeOnly,
    /// The body exists only after, and no claim declares it.
    AfterOnly,
}

/// The compare of one body. `name` is the "after" name, or the "before" name for `BeforeOnly`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyComparison {
    pub name: String,
    pub status: BodyStatus,
}

/// Parses the JSON lines of the driver.
pub fn parse_records(text: &str) -> Result<Vec<BodyRecord>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(number, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("line {}: not a body record", number + 1))
        })
        .collect()
}

/// Reads all `*.jsonl` files in a folder (in name order).
pub fn read_records(dir: &Path) -> Result<Vec<BodyRecord>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "jsonl"))
        .collect();
    files.sort();
    let mut records = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        records.extend(parse_records(&text).with_context(|| format!("in {}", file.display()))?);
    }
    Ok(records)
}

/// Groups records by their level.
pub fn group_levels(records: impl IntoIterator<Item = BodyRecord>) -> Levels {
    let mut levels = Levels::new();
    for record in records {
        levels.entry(record.level.clone()).or_default().push(record);
    }
    levels
}

/// Returns the N1 content hash of each body of one level (design 8.4: a body with a new hash
/// and no text change is an indirect change). A name with more than one distinct text (for
/// example a lib target and a test target with different `cfg`) gets no hash.
pub fn body_hashes(records: &[BodyRecord]) -> BTreeMap<String, String> {
    let groups = group_by_name(records.iter(), &[], false);
    hash::content_hashes(&hash_input(&groups))
}

/// Compares two versions. See the crate documentation for the levels.
///
/// - `renames` maps full def paths from "before" to "after" (rename and move claims). A key
///   also renames the child paths (methods of a renamed type, closures of a renamed function).
/// - `added` lists new bodies that the claims declare. Such a body (and its child bodies) can
///   exist only after. It is not in the result, because the claim explains it.
///
/// The work is lazy: a level is only computed for the bodies that are not equal at a lower
/// level, and the content hashes of a level are only computed when a body needs them.
pub fn compare(
    before: &Levels,
    after: &Levels,
    renames: &BTreeMap<String, String>,
    added: &[String],
) -> Vec<BodyComparison> {
    let rename_pairs = usable_renames(before, renames);
    // The records of the driver in wrapper mode name their target. Old records (and the command
    // mode) do not, and then all records of a name form one set.
    let use_targets = [before, after]
        .iter()
        .all(|levels| levels.values().flatten().all(|record| !record.target.is_empty()));
    let before = Side::new(before, &rename_pairs, use_targets);
    let after = Side::new(after, &[], use_targets);

    let mut results = Vec::new();
    let mut matched = BTreeSet::new();
    for (name, original) in &before.names {
        if !after.names.contains_key(name) {
            results.push(BodyComparison { name: original.clone(), status: BodyStatus::BeforeOnly });
            continue;
        }
        matched.insert(name.clone());
        results.push(BodyComparison { name: name.clone(), status: ladder(&before, &after, name) });
    }
    for name in after.names.keys() {
        let declared = added.iter().any(|new| name == new || is_child(name, new));
        if !matched.contains(name) && !declared {
            results.push(BodyComparison { name: name.clone(), status: BodyStatus::AfterOnly });
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    results
}

/// Returns the rename pairs that are safe to apply. A rename whose target path already exists on
/// the "before" side (as a body name or in a body text) would make two different items look the
/// same, so it is not applied. Such a claim is malformed. An exception: the target is itself
/// renamed away (a swap or a chain of renames).
fn usable_renames(before: &Levels, renames: &BTreeMap<String, String>) -> Vec<(String, String)> {
    renames
        .iter()
        .filter(|(from, to)| {
            if from == to || renames.contains_key(*to) {
                return true;
            }
            let marker = [((*to).clone(), "\u{0}".to_string())];
            // `contains` is a fast first test. The token test is exact.
            let mentions =
                |text: &String| text.contains(to.as_str()) && paths::rename(text, &marker) != *text;
            let exists = before
                .values()
                .flatten()
                .any(|record| mentions(&record.name) || mentions(&record.text));
            !exists
        })
        .map(|(from, to)| (from.clone(), to.clone()))
        .collect()
}

/// Returns rename hints: pairs of a "before" body and an "after" body with the same N1 content
/// hash, where the "before" name is missing after and the "after" name is missing before. Use
/// these only as hints; a claim must still declare the rename.
pub fn suggest_renames(before: &Levels, after: &Levels) -> Vec<(String, String)> {
    let level = ["n0", "n2", "n3"]
        .into_iter()
        .find(|level| before.contains_key(*level) && after.contains_key(*level));
    let Some(level) = level else {
        return Vec::new();
    };
    let before_groups = group_by_name(before[level].iter(), &[], false);
    let after_groups = group_by_name(after[level].iter(), &[], false);
    let before_hashes = hash::content_hashes(&hash_input(&before_groups));
    let after_hashes = hash::content_hashes(&hash_input(&after_groups));
    let mut by_hash: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
    for (new, hash) in &after_hashes {
        if !before_groups.contains_key(new) {
            by_hash.entry(hash).or_default().push(new);
        }
    }
    let mut pairs = Vec::new();
    for (old, hash) in &before_hashes {
        if after_groups.contains_key(old) {
            continue;
        }
        for new in by_hash.get(hash).into_iter().flatten() {
            pairs.push((old.clone(), (*new).clone()));
        }
    }
    pairs
}

/// Returns true if `name` is a child path of `parent` (for example a closure or a method).
fn is_child(name: &str, parent: &str) -> bool {
    name.strip_prefix(parent).is_some_and(|rest| rest.starts_with("::"))
}

/// The distinct records of one name at one level. The rename map is applied already. Each map
/// has one entry for each target (or one entry `""` without targets).
#[derive(Clone, Default)]
struct Group {
    /// The raw texts (no rename), for N0. `None` when the rename map is empty: then the raw
    /// texts are `texts`.
    raw: Option<BTreeMap<String, BTreeSet<String>>>,
    /// The texts after the rename map.
    texts: BTreeMap<String, BTreeSet<String>>,
    /// The callees after the rename map.
    callees: BTreeSet<String>,
    shapes: BTreeSet<String>,
    /// The terms after the rename map (`None` for a body without a term).
    terms: BTreeMap<String, BTreeSet<Option<String>>>,
    /// True for an N3 group that has only N2 records (no call was inlined).
    from_n2: bool,
}

impl Group {
    fn add(&mut self, record: &BodyRecord, renames: &[(String, String)], use_targets: bool) {
        let target = if use_targets { record.target.clone() } else { String::new() };
        if !renames.is_empty() {
            self.raw
                .get_or_insert_default()
                .entry(target.clone())
                .or_default()
                .insert(record.text.clone());
        }
        self.texts.entry(target.clone()).or_default().insert(paths::rename(&record.text, renames));
        self.callees.extend(record.callees.iter().map(|callee| paths::rename(callee, renames)));
        self.shapes.insert(record.shape.clone());
        self.terms
            .entry(target)
            .or_default()
            .insert(record.term.as_ref().map(|term| paths::rename(term, renames)));
    }

    /// The raw texts of each target.
    fn raw(&self) -> &BTreeMap<String, BTreeSet<String>> {
        self.raw.as_ref().unwrap_or(&self.texts)
    }

    /// All distinct texts of all targets.
    fn all_texts(&self) -> BTreeSet<&String> {
        self.texts.values().flatten().collect()
    }
}

fn group_by_name<'a>(
    records: impl Iterator<Item = &'a BodyRecord>,
    renames: &[(String, String)],
    use_targets: bool,
) -> BTreeMap<String, Group> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for record in records {
        groups.entry(paths::rename(&record.name, renames)).or_default().add(
            record,
            renames,
            use_targets,
        );
    }
    groups
}

/// The input of the content hash: the names with exactly one distinct text.
fn hash_input(groups: &BTreeMap<String, Group>) -> hash::HashInput {
    groups
        .iter()
        .filter_map(|(name, group)| {
            let texts = group.all_texts();
            let [text] = texts.into_iter().collect::<Vec<_>>()[..] else {
                return None;
            };
            Some((name.clone(), (text.clone(), group.callees.iter().cloned().collect())))
        })
        .collect()
}

/// One version, prepared for the compare.
struct Side {
    /// The names (after the rename map), with the original name.
    names: BTreeMap<String, String>,
    /// The groups of each level.
    levels: BTreeMap<String, BTreeMap<String, Group>>,
    /// The content hash replacements of each level, computed when a body first needs them.
    hashes: BTreeMap<String, OnceCell<paths::Replacer>>,
}

impl Side {
    fn new(levels: &Levels, renames: &[(String, String)], use_targets: bool) -> Side {
        let mut names = BTreeMap::new();
        let mut grouped = BTreeMap::new();
        for (level, records) in levels {
            for record in records {
                names.insert(paths::rename(&record.name, renames), record.name.clone());
            }
            grouped.insert(level.clone(), group_by_name(records.iter(), renames, use_targets));
        }
        // The driver writes an N3 record only for a body with an inlined call. For the other
        // bodies, the N3 body is the N2 body of the same target, so the N2 record stands in.
        if let (Some(n2), Some(n3)) = (levels.get("n2"), grouped.get_mut("n3")) {
            let key = |record: &BodyRecord| (record.target.clone(), record.name.clone());
            let present: BTreeSet<(String, String)> = levels["n3"].iter().map(key).collect();
            for record in n2.iter().filter(|record| !present.contains(&key(record))) {
                let name = paths::rename(&record.name, renames);
                let group =
                    n3.entry(name).or_insert_with(|| Group { from_n2: true, ..Group::default() });
                group.add(record, renames, use_targets);
            }
        }
        let hashes = grouped.keys().map(|level| (level.clone(), OnceCell::new())).collect();
        Side { names, levels: grouped, hashes }
    }

    fn group(&self, level: &str, name: &str) -> Option<&Group> {
        self.levels.get(level)?.get(name)
    }

    fn replacer(&self, level: &str) -> Option<&paths::Replacer> {
        let cell = self.hashes.get(level)?;
        Some(cell.get_or_init(|| {
            let hashes = hash::content_hashes(&hash_input(&self.levels[level]));
            paths::Replacer::functions(&hashes.into_iter().collect::<Vec<_>>())
        }))
    }

    /// The texts of a group with each local call replaced by its content hash.
    fn hashed_texts(&self, level: &str, group: &Group) -> BTreeMap<String, BTreeSet<String>> {
        let Some(replacer) = self.replacer(level) else {
            return group.texts.clone();
        };
        group
            .texts
            .iter()
            .map(|(target, texts)| {
                (target.clone(), texts.iter().map(|text| replacer.apply(text)).collect())
            })
            .collect()
    }

    fn hashed_term(&self, level: &str, term: &str) -> String {
        match self.replacer(level) {
            Some(replacer) => replacer.apply(term),
            None => term.to_string(),
        }
    }
}

/// Tries the levels in order for one body, and stops at the first equal level.
fn ladder(before: &Side, after: &Side, name: &str) -> BodyStatus {
    let equal =
        |level: &str, proof: Vec<String>| BodyStatus::Equal { level: level.to_string(), proof };

    if let (Some(b), Some(a)) = (before.group("n0", name), after.group("n0", name)) {
        if b.raw() == a.raw() {
            return equal("n0", Vec::new());
        }
        if b.texts == a.texts {
            return equal("n1", vec!["rename-map".to_string()]);
        }
        if before.hashed_texts("n0", b) == after.hashed_texts("n0", a) {
            return equal("n1", vec!["content-hash".to_string()]);
        }
    }
    for level in ["n2", "n3"] {
        if let (Some(b), Some(a)) = (before.group(level, name), after.group(level, name)) {
            // An N3 pair of N2 records is the N2 compare again.
            if level == "n3" && b.from_n2 && a.from_n2 {
                continue;
            }
            if b.texts == a.texts || before.hashed_texts(level, b) == after.hashed_texts(level, a) {
                return equal(level, Vec::new());
            }
        }
    }
    for level in ["n2", "n3"] {
        if let Some(proof) = n4_equal(before, after, level, name) {
            return equal("n4", proof);
        }
    }

    let shapes = |side: &Side| {
        ["n0", "n2", "n3"]
            .iter()
            .find_map(|level| side.group(level, name))
            .map(|group| group.shapes.clone())
    };
    BodyStatus::Different { cfg_changed: shapes(before) != shapes(after) }
}

/// N4 on the terms of one level. Returns the proof (the build rules and the egglog rules).
fn n4_equal(before: &Side, after: &Side, level: &str, name: &str) -> Option<Vec<String>> {
    let (b, a) = (before.group(level, name)?, after.group(level, name)?);
    if level == "n3" && b.from_n2 && a.from_n2 {
        return None;
    }
    // Both sides must have the same targets, and one distinct term for all of them.
    if !b.terms.keys().eq(a.terms.keys()) {
        return None;
    }
    let single = |group: &Group| {
        let terms: BTreeSet<&Option<String>> = group.terms.values().flatten().collect();
        match terms.into_iter().collect::<Vec<_>>()[..] {
            [Some(term)] => Some(term.clone()),
            _ => None,
        }
    };
    let (b_term, a_term) = (single(b)?, single(a)?);
    let (b_rules, b_body) = split_term(&b_term);
    let (a_rules, a_body) = split_term(&a_term);
    let mut proof: Vec<String> = Vec::new();
    for rule in b_rules.into_iter().chain(a_rules) {
        if !proof.contains(&rule) {
            proof.push(rule);
        }
    }
    let plain = (b_body.to_string(), a_body.to_string());
    let hashed = (before.hashed_term(level, b_body), after.hashed_term(level, a_body));
    let attempts = if hashed == plain { vec![plain] } else { vec![plain, hashed] };
    for (b_text, a_text) in &attempts {
        if b_text == a_text {
            return Some(proof);
        }
        if let n4::Outcome::Proved(rules) = n4::prove(b_text, a_text, &n4::Limits::default()) {
            for rule in rules {
                if !proof.contains(&rule) {
                    proof.push(rule);
                }
            }
            return Some(proof);
        }
    }
    None
}

/// Splits a term text into the build rules (the `;rules` line) and the node lines.
fn split_term(term: &str) -> (Vec<String>, &str) {
    match term.split_once('\n') {
        Some((first, rest)) if first.starts_with(";rules") => {
            let rules =
                first.trim_start_matches(";rules").split_whitespace().map(str::to_string).collect();
            (rules, rest)
        }
        _ => (Vec::new(), term),
    }
}

#[cfg(test)]
mod tests;
