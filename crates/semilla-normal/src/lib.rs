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
pub mod sandbox;

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use cargo::{Built, Configuration, Features, run_cargo_check, run_cargo_check_delta, run_cargo_check_delta_config, run_cargo_check_delta_features, run_cargo_check_with_trait_table};

/// A call to a function of another crate (design 10.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCall {
    /// The full def path of the callee.
    pub path: String,
    /// `err` (returns `Result` or `Option`), `mut` (takes `&mut`), `unsafe`, `never` (returns `!`).
    pub badges: Vec<String>,
    /// The name of the crate that defines the callee. Empty in an old record.
    #[serde(default)]
    pub krate: String,
    /// The def path of the callee in the crate that defines it (no re-export path).
    #[serde(default)]
    pub def: String,
}

/// A type of another crate, with the crate that defines it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeRef {
    pub path: String,
    pub krate: String,
}

/// The call that gives a type to generic code: the def path of the type, and the printed path
/// of the function.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropCall {
    pub path: String,
    pub call: String,
}

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
    /// The calls to functions of other crates, with badges from their signatures (only at N0).
    #[serde(default)]
    pub externals: Vec<ExternalCall>,
    /// The calls whose target the driver cannot resolve: through a function pointer, a trait
    /// object, or a trait method on a type parameter (only at N0).
    #[serde(default)]
    pub indirect: Vec<String>,
    /// The types and the traits whose trait impls the body can run through the code of another
    /// crate: the types in the generic arguments of its calls and in its unsize casts (only at
    /// N0).
    #[serde(default)]
    pub reach_types: Vec<TypeRef>,
    /// The types with a destructor that the body can drop (only at N0).
    #[serde(default)]
    pub drop_types: Vec<TypeRef>,
    /// For a type of `drop_types` that the body does not drop itself: the first call that has
    /// the type in its generic arguments (only at N0).
    #[serde(default)]
    pub drop_calls: Vec<DropCall>,
    /// For a method of a trait impl: the types and the trait of the impl header (only at N0).
    #[serde(default)]
    pub impl_of: Vec<String>,
    /// For the `drop` method of a `Drop` impl: the type (only at N0).
    #[serde(default)]
    pub drop_of: Option<String>,
    /// The functions that the body can call from a block that its start block reaches after
    /// the constant conditions fold, through its closures: the def paths (only at N0, not for a
    /// closure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_callees: Option<Vec<String>>,
    /// The functions that the body calls on each path from its start to a normal return: the
    /// names, as in `live_callees` (only at N0, not for a closure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sure_callees: Option<Vec<String>>,
    /// The types in the parameters of the body, through the fields of the types that are not
    /// of the standard library (only at N0). `dyn`, `fn`, and `param` are markers.
    #[serde(default)]
    pub param_types: Vec<TypeRef>,
    /// The types in the constants and the statics that the body reads (only at N0).
    #[serde(default)]
    pub const_types: Vec<TypeRef>,
    /// The uses of the build configuration in the body: the text of each macro call that reads
    /// it (`cfg!(debug_assertions)`). A `debug_assert!` of the standard library is not in the
    /// list.
    #[serde(default)]
    pub config: Vec<String>,
    /// The callees that the inline step put into the body, at each depth (only at N3).
    #[serde(default)]
    pub inlined: Vec<String>,
    /// The types with a definition outside the standard library that the body uses (only at
    /// N0). `?` if the driver has no list.
    #[serde(default)]
    pub adts: Vec<String>,
    /// The names of the expanded code of the item: its signature and its body after the macro
    /// expansion (only at N0; a closure has no list of its own).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<NameUse>,
}

/// One name of the expanded code of an item, with its form and its count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameUse {
    pub word: String,
    /// After `.` or `::`.
    #[serde(default)]
    pub member: bool,
    /// The segment before `::` (see the driver, `config::NameUse`).
    #[serde(default)]
    pub qual: Option<String>,
    /// Before `::`.
    #[serde(default)]
    pub prefix: bool,
    /// The outermost macro call that gives the name.
    #[serde(default)]
    pub mac: Option<String>,
    /// The line of the name, or of the outermost macro call.
    #[serde(default)]
    pub line: usize,
    #[serde(default)]
    pub count: usize,
    /// A call or a path with a rival (see the driver, `rival`).
    #[serde(default)]
    pub rival: Option<String>,
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
    /// The uses of the build configuration in the body, on the two sides (see
    /// [`BodyRecord::config`]).
    #[serde(default)]
    pub config: Vec<String>,
}

/// The record of one compile unit, from the compiler (batch 5t, part A). The driver writes it
/// as the first line of each record file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRecord {
    /// The name of the unit: the stem of the record file. Each body record has it as `target`.
    pub target: String,
    /// The name of the package.
    #[serde(default)]
    pub package: String,
    /// The folder of the package, relative to the workspace root.
    #[serde(default)]
    pub folder: String,
    /// Each true `cfg` name (`unix`) and each true pair (`feature="std"`).
    pub cfg: BTreeSet<String>,
    /// Each source file of the crate that the compiler read, relative to the workspace root.
    pub files: BTreeSet<String>,
    /// The root file of the crate, relative to the workspace root.
    #[serde(default)]
    pub root: String,
    /// The target triple.
    #[serde(default)]
    pub triple: String,
}

#[derive(Deserialize)]
struct UnitLine {
    unit: UnitRecord,
}

/// The start of the line of a unit record.
const UNIT_LINE: &str = "{\"unit\":";

/// Parses the JSON lines of the driver. The line of the unit record is not a body record.
pub fn parse_records(text: &str) -> Result<Vec<BodyRecord>> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with(UNIT_LINE))
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

/// Reads the unit record of each `*.jsonl` file in a folder (in name order). `None`: a file has
/// no unit record, so the units of the folder are not known.
pub fn read_units(dir: &Path) -> Option<Vec<UnitRecord>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "jsonl"))
        .collect();
    files.sort();
    let mut units = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).ok()?;
        let line = text.lines().find(|line| line.starts_with(UNIT_LINE))?;
        let mut unit = serde_json::from_str::<UnitLine>(line).ok()?.unit;
        // The name of the file is the name of the unit, also for a file that a caller copied.
        if let Some(stem) = file.file_stem() {
            unit.target = stem.to_string_lossy().into_owned();
        }
        units.push(unit);
    }
    Some(units)
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
    hash::content_hashes(&hash_input(&groups, &BTreeSet::new()))
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
    compare_with(before, after, renames, added, &Marks::default(), &Marks::default())
}

/// The bodies of one version that are opaque inputs of a proof (batch 5l). The names are the
/// record names of the version.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    /// No normal form looks into these bodies: the content hash of N1 uses only the path, and
    /// N3 does not use a body with one of them inlined (`drop_opaque_inlines`).
    pub opaque: BTreeSet<String>,
    /// These bodies get no proof: a level above N0 with equal texts is "different".
    pub stop: BTreeSet<String>,
}

/// Removes each N3 record with an inlined callee that is opaque, or that has no N0 record
/// (the default is opaque). The N2 record of the body stands in (see `Side::new`).
pub fn drop_opaque_inlines(levels: &mut Levels, marks: &Marks) {
    let known: BTreeSet<&str> = levels.get("n0").into_iter().flatten().map(|record| record.name.as_str()).collect();
    let keep: Vec<bool> = levels
        .get("n3")
        .into_iter()
        .flatten()
        .map(|record| record.inlined.iter().all(|callee| known.contains(callee.as_str()) && !marks.opaque.contains(callee)))
        .collect();
    if let Some(records) = levels.get_mut("n3") {
        let mut keep = keep.into_iter();
        records.retain(|_| keep.next().unwrap_or(false));
    }
}

/// `compare` with the opaque bodies of each version.
pub fn compare_with(
    before: &Levels,
    after: &Levels,
    renames: &BTreeMap<String, String>,
    added: &[String],
    before_marks: &Marks,
    after_marks: &Marks,
) -> Vec<BodyComparison> {
    let rename_pairs = usable_renames(before, renames);
    // The records of the driver in wrapper mode name their target. Old records (and the command
    // mode) do not, and then all records of a name form one set.
    let use_targets = [before, after]
        .iter()
        .all(|levels| levels.values().flatten().all(|record| !record.target.is_empty()));
    let before = Side::new(before, &rename_pairs, use_targets, before_marks);
    let after = Side::new(after, &[], use_targets, after_marks);
    // A body that gets no proof: equal only with the same N0 text, raw or after the rename map.
    let stopped = |name: &str, status: BodyStatus| match &status {
        BodyStatus::Equal { level, proof } if before.stop.contains(name) || after.stop.contains(name) => {
            let same_text = level == "n0" || (level == "n1" && proof.iter().all(|step| step == "rename-map"));
            if same_text { status } else { BodyStatus::Different { cfg_changed: true } }
        }
        _ => status,
    };

    let mut results = Vec::new();
    let mut matched = BTreeSet::new();
    let config = |name: &str| -> Vec<String> {
        let sides = [before.config.get(name), after.config.get(name)];
        sides.into_iter().flatten().flatten().cloned().collect::<BTreeSet<String>>().into_iter().collect()
    };
    for (name, original) in &before.names {
        if !after.names.contains_key(name) {
            results.push(BodyComparison { name: original.clone(), status: BodyStatus::BeforeOnly, config: config(name) });
            continue;
        }
        matched.insert(name.clone());
        results.push(BodyComparison { name: name.clone(), status: stopped(name, ladder(&before, &after, name)), config: config(name) });
    }
    for name in after.names.keys() {
        let declared = added.iter().any(|new| name == new || is_child(name, new));
        if !matched.contains(name) && !declared {
            results.push(BodyComparison { name: name.clone(), status: BodyStatus::AfterOnly, config: config(name) });
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
    let before_hashes = hash::content_hashes(&hash_input(&before_groups, &BTreeSet::new()));
    let after_hashes = hash::content_hashes(&hash_input(&after_groups, &BTreeSet::new()));
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

/// Normalizes the `TestDesc` const of a `#[test]` function `name`:
/// - It removes the source position. The position is for the test report only (like a panic
///   location, design 18), so it is not behavior.
/// - It prints the test name (a string) as the def path of the test, so that the rename map of
///   a `rename` claim applies to it.
fn without_test_positions<'a>(text: &'a str, name: &str) -> std::borrow::Cow<'a, str> {
    if !text.contains("adt test::TestDesc fields {") {
        return std::borrow::Cow::Borrowed(text);
    }
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            if line.contains("adt test::TestName ") {
                return test_name_as_path(line, name);
            }
            if !line.contains("adt test::TestDesc fields {") {
                return line.to_string();
            }
            mask_positions(line)
        })
        .collect();
    let mut joined = lines.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    std::borrow::Cow::Owned(joined)
}

/// Replaces the string of a `TestName` line with the def path of the test, when the string is
/// the path of the test in its crate (`tests::t` for `k::tests::t`). Else the line stays.
fn test_name_as_path(line: &str, name: &str) -> String {
    let decoded = line.split("memory [").nth(1).and_then(|rest| rest.split(']').next()).and_then(|hex| {
        let bytes: Option<Vec<u8>> = (0..hex.len()).step_by(2).map(|at| hex.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok())).collect();
        String::from_utf8(bytes?).ok()
    });
    const OPEN: &str = " fields {";
    let (Some(decoded), Some(open), Some(close)) = (decoded, line.find(OPEN), line.rfind('}')) else { return line.to_string() };
    if !name.ends_with(&format!("::{decoded}")) || close < open {
        return line.to_string();
    }
    format!("{}test name {name}{}", &line[..open + OPEN.len()], &line[close..])
}

/// Replaces the values of the four position fields of one `TestDesc` line (`start_line`,
/// `start_col`, `end_line`, `end_col`) with `_`. The driver prints each field with its name
/// (`test::TestDesc.start_line: const usize = 12`). Each other field (`ignore`, `should_panic`,
/// the name) stays.
fn mask_positions(line: &str) -> String {
    let mut out = line.to_string();
    for field in ["start_line", "start_col", "end_line", "end_col"] {
        let key = format!("test::TestDesc.{field}: const usize = ");
        let Some(found) = out.find(&key) else { continue };
        let value = found + key.len();
        let digits = out[value..].chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 {
            out.replace_range(value..value + digits, "_");
        }
    }
    out
}

/// The marker of the operand list of an ADT aggregate in the driver text (see
/// `semilla-mir/src/print.rs`): `adt k::Pair fields {k::Pair.a: _1, k::Pair.b: _2}`.
const FIELDS_OPEN: &str = " fields {";

/// Sorts the operands of each ADT aggregate of a text. The driver sorts them by the field path,
/// but the rename map can change a path and so the order. The order of the operands has no
/// effect: the statements before the aggregate evaluate them. An operand list that does not
/// parse stays as it is.
fn sort_aggregates(text: &str) -> String {
    if !text.contains(FIELDS_OPEN) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find(FIELDS_OPEN) {
        let start = found + FIELDS_OPEN.len();
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        // The end of the list: the `}` at depth zero.
        let mut depth = 0i32;
        let mut end = None;
        for (index, c) in rest.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' => depth -= 1,
                '}' if depth == 0 => {
                    end = Some(index);
                    break;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        let Some(end) = end else { break };
        // A nested aggregate (in a promoted constant) gets its own sort.
        let inner = sort_aggregates(&rest[..end]);
        let mut parts = split_top(&inner);
        parts.sort();
        out.push_str(&parts.join(", "));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The marker of the target list of a switch in the driver text:
/// `switchInt(_2) -> [k::Kind::Large: bb1, k::Kind::Small: bb2, otherwise: bb3]`.
const SWITCH_OPEN: &str = "switchInt(";

/// The marker of a discriminant table in the driver text:
/// `of k::Kind values [k::Kind::Large = 1, k::Kind::Small = 0]`.
const TABLE_OPEN: &str = " values [";

/// Sorts the named targets of each switch and the entries of each discriminant table of a
/// text. The driver sorts them by the variant path, but the rename map can change a path and
/// so the order (trial 4, D11: the rename of a variant). The order has no effect: the values
/// of a switch are different, and a table is a set of pairs. A list that does not parse stays
/// as it is.
fn sort_variant_lists(text: &str) -> String {
    let text = sort_list(text, TABLE_OPEN, "", |_| true);
    // A switch with number targets only keeps the order of the driver.
    sort_list(&text, SWITCH_OPEN, " -> [", |parts| {
        parts.iter().any(|part| !part.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// Sorts each list that starts after `marker` and then `open`, and ends at the `]` at depth
/// zero. The last entry `otherwise: …` of a switch keeps its position. `wanted` selects the
/// lists.
fn sort_list(text: &str, marker: &str, open: &str, wanted: impl Fn(&[&str]) -> bool) -> String {
    if !text.contains(marker) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find(marker) {
        let after_marker = found + marker.len();
        // The list opens on the same line as the marker.
        let line_end = rest[after_marker..].find('\n').map_or(rest.len(), |index| after_marker + index);
        let Some(opened) = rest[after_marker..line_end].find(open) else {
            out.push_str(&rest[..after_marker]);
            rest = &rest[after_marker..];
            continue;
        };
        let start = after_marker + opened + open.len();
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let mut depth = 0i32;
        let mut end = None;
        for (index, c) in rest.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ']' if depth == 0 => {
                    end = Some(index);
                    break;
                }
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let mut parts = split_top(&rest[..end]);
        if wanted(&parts) {
            let last = parts.last().filter(|part| part.starts_with("otherwise: ")).copied();
            if last.is_some() {
                parts.pop();
            }
            parts.sort();
            parts.extend(last);
        }
        out.push_str(&parts.join(", "));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Splits a list at the commas outside brackets (`()`, `[]`, `{}`).
fn split_top(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (index, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 && text[index..].starts_with(", ") => {
                parts.push(&text[start..index]);
                start = index + 2;
            }
            _ => {}
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

impl Group {
    fn add(&mut self, record: &BodyRecord, renames: &[(String, String)], use_targets: bool) {
        let normal = BodyRecord { text: without_test_positions(&record.text, &record.name).into_owned(), ..record.clone() };
        let record = &normal;
        let target = if use_targets { record.target.clone() } else { String::new() };
        if !renames.is_empty() {
            self.raw
                .get_or_insert_default()
                .entry(target.clone())
                .or_default()
                .insert(record.text.clone());
        }
        let renamed = paths::rename(&record.text, renames);
        // A rename can change the order of the named operands of an aggregate, of the named
        // targets of a switch, and of the entries of a discriminant table.
        let renamed = if renamed != record.text { sort_variant_lists(&sort_aggregates(&renamed)) } else { renamed };
        self.texts.entry(target.clone()).or_default().insert(renamed);
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
fn hash_input(groups: &BTreeMap<String, Group>, opaque: &BTreeSet<String>) -> hash::HashInput {
    groups
        .iter()
        // An opaque body has no content hash: a call of it keeps its path.
        .filter(|(name, _)| !opaque.contains(*name))
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
    /// The uses of the build configuration of each name (after the rename map).
    config: BTreeMap<String, BTreeSet<String>>,
    /// The opaque bodies and the bodies with no proof (`Marks`), after the rename map.
    opaque: BTreeSet<String>,
    stop: BTreeSet<String>,
}

impl Side {
    fn new(levels: &Levels, renames: &[(String, String)], use_targets: bool, marks: &Marks) -> Side {
        let mut names = BTreeMap::new();
        let mut grouped = BTreeMap::new();
        let mut config: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (level, records) in levels {
            for record in records {
                let name = paths::rename(&record.name, renames);
                // Only the N0 record: the N3 record also has the uses of the inlined callees.
                if level == "n0" && !record.config.is_empty() {
                    config.entry(name.clone()).or_default().extend(record.config.iter().cloned());
                }
                names.insert(name, record.name.clone());
            }
            grouped.insert(level.clone(), group_by_name(records.iter(), renames, use_targets));
        }
        // The driver writes an N3 record only for a body with an inlined call. For the other
        // bodies, the N3 body is the N2 body of the same target, so the N2 record stands in.
        // Two bodies can have one name: a `#[test]` function and its `TestDesc` const. The first
        // line of the text (the signature) tells them apart, so that the N3 record of the
        // function does not hide the N2 record of the const (T5).
        if let (Some(n2), Some(n3)) = (levels.get("n2"), grouped.get_mut("n3")) {
            let signature = |text: &str| text.lines().next().filter(|line| line.starts_with("fn ")).unwrap_or("").to_string();
            let key = |record: &BodyRecord| (record.target.clone(), record.name.clone(), signature(&record.text));
            let present: BTreeSet<(String, String, String)> = levels["n3"].iter().map(key).collect();
            for record in n2.iter().filter(|record| !present.contains(&key(record))) {
                let name = paths::rename(&record.name, renames);
                let group =
                    n3.entry(name).or_insert_with(|| Group { from_n2: true, ..Group::default() });
                group.add(record, renames, use_targets);
            }
        }
        let hashes = grouped.keys().map(|level| (level.clone(), OnceCell::new())).collect();
        let renamed = |names: &BTreeSet<String>| names.iter().map(|name| paths::rename(name, renames)).collect();
        Side { names, levels: grouped, hashes, config, opaque: renamed(&marks.opaque), stop: renamed(&marks.stop) }
    }

    fn group(&self, level: &str, name: &str) -> Option<&Group> {
        self.levels.get(level)?.get(name)
    }

    fn replacer(&self, level: &str) -> Option<&paths::Replacer> {
        let cell = self.hashes.get(level)?;
        Some(cell.get_or_init(|| {
            let hashes = hash::content_hashes(&hash_input(&self.levels[level], &self.opaque));
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
        if same_bodies(b.raw(), a.raw()) {
            return equal("n0", Vec::new());
        }
        if same_bodies(&b.texts, &a.texts) {
            return equal("n1", vec!["rename-map".to_string()]);
        }
        if same_bodies(&before.hashed_texts("n0", b), &after.hashed_texts("n0", a)) {
            return equal("n1", vec!["content-hash".to_string()]);
        }
    }
    for level in ["n2", "n3"] {
        if let (Some(b), Some(a)) = (before.group(level, name), after.group(level, name)) {
            // An N3 pair of N2 records is the N2 compare again.
            if level == "n3" && b.from_n2 && a.from_n2 {
                continue;
            }
            if same_bodies(&b.texts, &a.texts)
                || same_bodies(&before.hashed_texts(level, b), &after.hashed_texts(level, a))
            {
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

/// True if the texts of one body are the same on both sides, by target.
///
/// The targets of a body differ when the body moves to another crate (`move-item`), or when the
/// package of a crate changes. No target map exists then, so the bodies are equal only when each
/// side has exactly one distinct text for all its targets, and the two texts are the same.
fn same_bodies(
    before: &BTreeMap<String, BTreeSet<String>>,
    after: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    if before.keys().eq(after.keys()) {
        return before == after;
    }
    fn single(texts: &BTreeMap<String, BTreeSet<String>>) -> Option<&String> {
        let all: BTreeSet<&String> = texts.values().flatten().collect();
        match all.into_iter().collect::<Vec<_>>()[..] {
            [text] => Some(text),
            _ => None,
        }
    }
    matches!((single(before), single(after)), (Some(b), Some(a)) if b == a)
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
