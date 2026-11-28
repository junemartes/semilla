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
pub mod intent;
pub mod elements;
pub mod graph;
pub mod hash;
pub mod index;
pub mod lazy;
pub mod paths;
pub mod sandbox;
pub mod transform;

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
    /// [R75.1-1] True for the body of a constant or a static: a suggested test reads its value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub value: bool,
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
    /// The facts for the ends of a run that are not a return (batch 5y, A4): the keys `ends`,
    /// `calls`, `first`, and `before NAME` (see `reach::sure_callees` of the driver).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sure_ends: Option<std::collections::BTreeMap<String, Vec<String>>>,
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
    /// The source range of each MIR element (design 40.1; only at N0).
    /// The data is shared: a record in memory is cloned for each read of its cache folder.
    #[serde(rename = "el", default, skip_serializing_if = "Option::is_none", serialize_with = "elements::serialize_shared", deserialize_with = "elements::deserialize_shared")]
    pub elements: Option<std::sync::Arc<elements::RecordElements>>,
    /// The control flow graph of the body (design 60; only at N0). Shared, as `elements`.
    #[serde(rename = "g", default, skip_serializing_if = "Option::is_none", serialize_with = "graph::serialize_shared", deserialize_with = "graph::deserialize_shared")]
    pub graph: Option<std::sync::Arc<graph::RecordGraph>>,
    /// The body in the form of the equivalence check (design 61; only at N0). Shared, as
    /// `elements`. The check of `semilla-smt` reads it.
    /// It loads on its first read (R86.2-1).
    #[serde(default, skip_serializing_if = "lazy::LazyText::is_none")]
    pub q: lazy::LazyText,
    /// The forms of the small callees of the standard library, by the callee text (design 61,
    /// C3; only at N0).
    #[serde(default, skip_serializing_if = "lazy::LazyText::is_none")]
    pub qx: lazy::LazyText,
    /// The values of the constant items that the body reads: the path and the value (design
    /// 61, A2; only at N0).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cv: Vec<(String, String)>,
    /// The initial value of each static with interior mutability that the body reads: the path
    /// and the value (R73.2-1; only at N0).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sv: Vec<(String, String)>,
    /// The instances of the generic bodies that this body makes (design 61, part B; only at N0).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inst: Vec<InstanceUse>,
    /// The conversions to `dyn Trait` in the body: the trait and the source type (design 61,
    /// B3; only at N0).
    #[serde(rename = "dyn", default, skip_serializing_if = "Vec::is_empty")]
    pub dyn_casts: Vec<(String, String)>,
    /// The build settings of the unit of the record with no feature (R61.16-2): the `cfg`
    /// names of its unit record that do not start with `feature=`, joined. Two units of one
    /// target name (a library that cargo builds with `panic="unwind"` and with
    /// `panic="abort"`) have different settings. `read_records` sets it; empty if not known.
    #[serde(skip)]
    pub unit: String,
    /// [R88.3-1] The digests and the paths of the text: from the index of the record file, or
    /// made at the first read. Not a member of the line.
    #[serde(skip)]
    pub known: index::Known,
}

impl BodyRecord {
    /// The digests and the paths of the text of the record (R88.3-1).
    pub fn facts(&self) -> &std::sync::Arc<index::TextFacts> {
        self.known.get(&self.text, &self.shape)
    }
}

/// [R88.3-1] True if two records have the same text: by the digests of the index if the two
/// records have them, else by the texts.
pub fn same_text(a: &BodyRecord, b: &BodyRecord) -> bool {
    match (a.known.from_index(), b.known.from_index()) {
        (Some(x), Some(y)) => x.text == y.text,
        _ => a.text == b.text,
    }
}

/// [R88.3-2] True if the text of `old` after the rename map is the text of `new`. If the two
/// records have the facts of the index and no key of the map can match a path of the old text,
/// the renamed text is the old text: the digests decide, and no text is read.
pub fn same_after_renames(old: &BodyRecord, new: &BodyRecord, renames: &[(String, String)]) -> bool {
    match (old.known.from_index(), new.known.from_index()) {
        (Some(x), Some(y)) if renames.is_empty() || !index::may_rename(&x.paths, renames) => x.text == y.text,
        _ => paths::rename_cow(&old.text, renames) == new.text.as_str(),
    }
}

/// [R88.3-2] The digest of the text of a record after a rename map: the digest of the index if no
/// key of the map can match a path of the text, else the digest of the renamed text.
pub fn renamed_digest(record: &BodyRecord, renames: &[(String, String)]) -> index::Digest {
    match record.known.from_index() {
        Some(facts) if renames.is_empty() || !index::may_rename(&facts.paths, renames) => facts.text,
        _ => index::digest(&paths::rename_cow(&record.text, renames)),
    }
}

/// One instance of a generic body (design 61, part B).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstanceUse {
    /// The name of the generic body.
    pub g: String,
    /// The text of the instance (the path with each generic argument).
    pub t: String,
    /// Each call that the instance resolves: the text in the generic body and the callee.
    #[serde(default)]
    pub c: Vec<(String, String)>,
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
    /// The byte range of the body in the file as written (design 40.2; only at N0, not for a
    /// body from a macro expansion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(usize, usize)>,
}

/// The records of one version, by level name (`n0`, `n2`, `n3`).
pub type Levels = BTreeMap<String, Records>;

/// [R86.3-2] A shared list of records: the records of a cache folder in memory. Each reader gets
/// the same list, with no copy. A change of the list (`make_mut`) copies it only if another
/// reader has it too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Records(pub std::sync::Arc<Vec<BodyRecord>>);

impl Records {
    /// The list for a change: a copy only if another reader shares it.
    pub fn make_mut(&mut self) -> &mut Vec<BodyRecord> {
        std::sync::Arc::make_mut(&mut self.0)
    }

    /// True if the two values are one list in memory (no copy).
    pub fn same_list(&self, other: &Records) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::ops::Deref for Records {
    type Target = Vec<BodyRecord>;
    fn deref(&self) -> &Vec<BodyRecord> {
        &self.0
    }
}

impl<'a> IntoIterator for &'a Records {
    type Item = &'a BodyRecord;
    type IntoIter = std::slice::Iter<'a, BodyRecord>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl From<Vec<BodyRecord>> for Records {
    fn from(records: Vec<BodyRecord>) -> Records {
        Records(std::sync::Arc::new(records))
    }
}

impl FromIterator<BodyRecord> for Records {
    fn from_iter<I: IntoIterator<Item = BodyRecord>>(records: I) -> Records {
        Records::from(records.into_iter().collect::<Vec<_>>())
    }
}

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
    /// The MIR elements that differ (design 40.3), for a body that is `Different` with the
    /// element ranges on the two sides. `None`: no element data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elements: Option<elements::ElementDiff>,
    /// The control flow graphs of the body on the two sides, for each pair of targets (design
    /// 60).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphs: Vec<graph::GraphPair>,
    /// The source file of the body, for a name with more than one source body (design 61.14,
    /// R61.14-1): each source body of the name has its own comparison. `None`: the name is one
    /// source body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
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
    /// The identity of the crate (`StableCrateId`, 16 hex digits; design 78.1).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub identity: String,
    /// The definitions of the crate and the symbols that each one names (design 44). At n0
    /// only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defs: Option<elements::RecordDefs>,
}

#[derive(Deserialize)]
struct UnitLine {
    unit: UnitRecord,
}

/// One item of the expanded code, from the compiler (batch 6b, part 2). The driver writes the
/// list in the unit record at N0.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MadeItem {
    /// The file of the place, relative to the workspace root.
    pub file: String,
    /// The line (from 1) and the column (from 0, in characters) of the place: the start of the
    /// outermost macro call, or the start of the item for an item with no macro.
    pub line: usize,
    pub col: usize,
    /// The module path of the item in the crate (empty for the crate root).
    #[serde(default)]
    pub module: String,
    /// The source text of the item after the expansion. A module has no content.
    pub text: String,
    /// True if the span of the item comes from a macro expansion.
    #[serde(default)]
    pub expanded: bool,
    /// True if the item is a `#[test]` function.
    #[serde(default)]
    pub test: bool,
    /// True if a different crate can reach the item, by the compiler.
    #[serde(default)]
    pub public: bool,
    /// For an associated item that a macro call in an impl or a trait makes: the self type or
    /// the trait (batch 10c).
    #[serde(default)]
    pub owner: String,
}

/// The expanded code of the units of one tree (batch 6b, part 2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Made {
    /// Each item after the macro expansion.
    pub items: BTreeSet<MadeItem>,
    /// The name of a body from a macro → the file, the line, and the column of the outermost
    /// macro call.
    pub sites: BTreeMap<String, (String, usize, usize)>,
}

#[derive(Deserialize)]
struct MadeUnit {
    made: Option<Vec<MadeItem>>,
}

#[derive(Deserialize)]
struct MadeLine {
    unit: MadeUnit,
}

#[derive(Deserialize)]
struct SiteLine {
    name: String,
    #[serde(default)]
    site: Option<(String, usize, usize)>,
}

/// Reads the expanded items and the macro call sites from each `*.jsonl` file of an N0 folder
/// into `made`. False: a file has no list of the items, so the items are not known.
pub fn read_made(dir: &Path, made: &mut Made) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    let files: Vec<_> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).filter(|path| path.extension().is_some_and(|extension| extension == "jsonl")).collect();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { return false };
        let Some(line) = text.lines().find(|line| line.starts_with(UNIT_LINE)) else { return false };
        let Some(items) = serde_json::from_str::<MadeLine>(line).ok().and_then(|line| line.unit.made) else { return false };
        made.items.extend(items);
        // The text search is a fast filter: most bodies have no site.
        for line in text.lines().filter(|line| !line.starts_with(UNIT_LINE) && line.contains("\"site\":")) {
            if let Ok(SiteLine { name, site: Some(site) }) = serde_json::from_str::<SiteLine>(line) {
                made.sites.insert(name, site);
            }
        }
    }
    true
}

/// The expansions of one compiled target (batch 10c, design 63): the target name of the unit
/// record, and each expansion as a JSON value in the form of the driver.
#[derive(Clone, Debug, Default)]
pub struct UnitExpansions {
    pub target: String,
    /// The package of the target (two packages can have a test target with one name).
    pub package: String,
    /// The root file of the crate (batch 10c2, R63.9-1): a test target and a bench target of
    /// one package can have one name and one kind.
    pub root: String,
    /// Each true `cfg` name and pair of the unit (the features).
    pub cfg: Vec<String>,
    pub list: Vec<serde_json::Value>,
    /// The name record of the unit (design 70, R70.1-1), or `None` (a record of an earlier
    /// driver).
    pub names: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ExpansionUnit {
    #[serde(default)]
    target: String,
    #[serde(default)]
    package: String,
    #[serde(default)]
    root: String,
    #[serde(default)]
    cfg: Vec<String>,
    expansions: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    names: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ExpansionLine {
    unit: ExpansionUnit,
}

/// Reads the expansion of each macro call from the unit record of each `*.jsonl` file of an N0
/// folder (batch 10c). The unit record is the first line, so the read stops there. `None`: a
/// file has no expansion list (a record of an earlier driver), so the expansions are not known.
pub fn read_expansions(dir: &Path) -> Option<Vec<UnitExpansions>> {
    use std::io::BufRead as _;
    let entries = std::fs::read_dir(dir).ok()?;
    let files = unit_order(entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).filter(|path| path.extension().is_some_and(|extension| extension == "jsonl")).collect());
    let mut out = Vec::new();
    for file in files {
        let reader = std::io::BufReader::new(std::fs::File::open(&file).ok()?);
        let line = reader.lines().next()?.ok()?;
        if !line.starts_with(UNIT_LINE) {
            return None;
        }
        let unit = serde_json::from_str::<ExpansionLine>(&line).ok()?.unit;
        out.push(UnitExpansions { target: unit.target, package: unit.package, root: unit.root, cfg: unit.cfg, list: unit.expansions?, names: unit.names });
    }
    Some(out)
}

/// The start of the line of a unit record.
pub const UNIT_LINE: &str = "{\"unit\":";

/// The order of the record files of a folder (R66.10-3): by the data of the unit record of each
/// file (the package, the target name with no metadata hash, the configuration, the root file),
/// not by the file name. The file name `<crate>.<kind>.<metadata>.jsonl` has the metadata hash
/// of cargo, which changes with the driver, so two targets with one crate name and one kind (two
/// packages with a test target `integration`) had an order that changed with the driver. The
/// file name only breaks a tie.
pub fn unit_order(files: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    use std::io::BufRead as _;
    #[derive(Deserialize)]
    struct KeyUnit {
        #[serde(default)]
        target: String,
        #[serde(default)]
        package: String,
        #[serde(default)]
        root: String,
        #[serde(default)]
        cfg: Vec<String>,
    }
    #[derive(Deserialize)]
    struct KeyLine {
        unit: KeyUnit,
    }
    let key = |file: &std::path::PathBuf| -> (String, String, Vec<String>, String) {
        let line = std::fs::File::open(file).ok().and_then(|handle| std::io::BufReader::new(handle).lines().next()).and_then(Result::ok).unwrap_or_default();
        let unit = line.starts_with(UNIT_LINE).then(|| serde_json::from_str::<KeyLine>(&line).ok()).flatten().map(|line| line.unit);
        let Some(unit) = unit else { return Default::default() };
        // The target is the stem of the file: `<crate>.<kind>.<metadata>`.
        let name = unit.target.rsplit_once('.').map_or(unit.target.as_str(), |(name, _)| name).to_string();
        (unit.package, name, unit.cfg, unit.root)
    };
    let mut keyed: Vec<((String, String, Vec<String>, String), std::path::PathBuf)> = files.into_iter().map(|file| (key(&file), file)).collect();
    keyed.sort();
    keyed.into_iter().map(|(_, file)| file).collect()
}

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

/// Reads all `*.jsonl` files in a folder (in the order of their units, `unit_order`).
pub fn read_records(dir: &Path) -> Result<Vec<BodyRecord>> {
    let files = unit_order(
        std::fs::read_dir(dir)
            .with_context(|| format!("cannot read {}", dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|extension| extension == "jsonl"))
            .collect(),
    );
    let mut records = Vec::new();
    for file in files {
        let mut found = match read_lazy(&file) {
            Some(found) => found,
            None => {
                let text = std::fs::read_to_string(&file)
                    .with_context(|| format!("cannot read {}", file.display()))?;
                let unit = unit_settings(&text);
                let mut found = parse_records(&text).with_context(|| format!("in {}", file.display()))?;
                for record in found.iter_mut() {
                    record.unit.clone_from(&unit);
                }
                found
            }
        };
        records.append(&mut found);
    }
    Ok(records)
}

/// The member of a record that loads on its first read (R86.2-1): the two forms of the
/// equivalence check, the members `q` and `qx` of [`BodyRecord`]. This is the format of the
/// records of Semilla, not a name of the code under review.
pub(crate) fn lazy_member(key: &str) -> Option<bool> {
    match key {
        "q" => Some(false),
        "qx" => Some(true),
        _ => None,
    }
}

/// [R86.2-1] The records of one file, with each member of `LAZY_MEMBERS` at its place in the
/// file: the parse skips it. `None` if a line does not have the plain form (then
/// [`parse_records`] reads the file, with its errors).
fn read_lazy(file: &Path) -> Option<Vec<BodyRecord>> {
    let bytes = std::fs::read(file).ok()?;
    let handle = std::sync::Arc::new(std::fs::File::open(file).ok()?);
    let mut records = Vec::new();
    let mut unit = None;
    let mut offset = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        let start = offset;
        offset += line.len() + 1;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(UNIT_LINE.as_bytes()) {
            if unit.is_none() {
                unit = Some(unit_settings(std::str::from_utf8(line).ok()?));
            }
            continue;
        }
        if line.trim_ascii().is_empty() {
            continue;
        }
        let members = lazy::members(line)?;
        let lazy: Vec<&(&str, std::ops::Range<usize>, std::ops::Range<usize>)> = members.iter().filter(|(key, _, _)| lazy_member(key).is_some()).collect();
        let mut record: BodyRecord = if lazy.is_empty() {
            serde_json::from_slice(line).ok()?
        } else {
            // Two members with one key: the full parse gives its error.
            if [false, true].iter().any(|kind| lazy.iter().filter(|(key, _, _)| lazy_member(key) == Some(*kind)).count() > 1) {
                return None;
            }
            let mut rest = Vec::with_capacity(line.len());
            rest.push(b'{');
            for (key, member, _) in &members {
                if lazy_member(key).is_none() {
                    if rest.len() > 1 {
                        rest.push(b',');
                    }
                    rest.extend_from_slice(&line[member.clone()]);
                }
            }
            rest.push(b'}');
            serde_json::from_slice(&rest).ok()?
        };
        for (key, _, value) in lazy {
            let text = if &line[value.clone()] == b"null" {
                lazy::LazyText::known(None)
            } else {
                // The value must be a text: else the full parse gives its error.
                if line[value.start] != b'"' {
                    return None;
                }
                lazy::LazyText::at(lazy::Place { file: handle.clone(), offset: (start + value.start) as u64, len: value.len() })
            };
            if lazy_member(key) == Some(false) {
                record.q = text;
            } else {
                record.qx = text;
            }
        }
        records.push(record);
    }
    // The settings of the first unit line, as `unit_settings` reads them.
    let unit = unit.unwrap_or_default();
    for record in records.iter_mut() {
        record.unit.clone_from(&unit);
    }
    Some(records)
}

/// The build settings of the unit record of a record file with no feature (see
/// [`BodyRecord::unit`]). Empty if the file has no unit record.
pub fn unit_settings(text: &str) -> String {
    let Some(line) = text.lines().find(|line| line.starts_with(UNIT_LINE)) else { return String::new() };
    let Ok(unit) = serde_json::from_str::<UnitLine>(line) else { return String::new() };
    unit.unit.cfg.iter().filter(|name| !name.starts_with("feature=")).cloned().collect::<Vec<_>>().join(",")
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
        // R86.3-3: the read stops at the unit record (the first line of a file of the driver).
        use std::io::BufRead as _;
        let reader = std::io::BufReader::new(std::fs::File::open(&file).ok()?);
        let mut found = None;
        for line in reader.lines() {
            let line = line.ok()?;
            if line.starts_with(UNIT_LINE) {
                found = Some(line);
                break;
            }
        }
        let line = found?;
        let mut unit = serde_json::from_str::<UnitLine>(&line).ok()?.unit;
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
    let mut levels: BTreeMap<String, Vec<BodyRecord>> = BTreeMap::new();
    for record in records {
        levels.entry(record.level.clone()).or_default().push(record);
    }
    levels.into_iter().map(|(level, records)| (level, Records::from(records))).collect()
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
    if let Some(records) = levels.get_mut("n3").filter(|_| keep.iter().any(|kept| !kept)) {
        let mut keep = keep.into_iter();
        records.make_mut().retain(|_| keep.next().unwrap_or(false));
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
    // The names of the impl items that lost or got a lifetime (design 37.2).
    let renamed = lifetime_names(before, after, before_marks);
    let (before, before_marks) = renamed.as_ref().map_or((before, before_marks), |(levels, marks)| (levels, marks));
    let mut rename_pairs = usable_renames(before, renames);
    // The generic parameters of a renamed impl item that got new names (design 38.10).
    let params = param_names(before, after, &rename_pairs, before_marks);
    let (before, before_marks) = params.as_ref().map_or((before, before_marks), |(levels, marks)| (levels, marks));
    let generic = generic_renames(before, after, &rename_pairs, renames);
    rename_pairs.extend(generic);
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
    // A name with more than one source body: one comparison for each source body (R61.14-1).
    let split = |name: &String, original: &String, results: &mut Vec<BodyComparison>| -> bool {
        let bodies = if use_targets { source_bodies(&before, &after, name) } else { Vec::new() };
        let declared = added.iter().any(|new| name == new || is_child(name, new));
        for (file, targets) in &bodies {
            let (old, new) = (Only::new(&before, name, targets), Only::new(&after, name, targets));
            let place = Some(file.clone());
            let (old_group, new_group) = (old.groups.get("n0"), new.groups.get("n0"));
            match (old_group.is_some(), new_group.is_some()) {
                (true, true) => {
                    let status = stopped(name, ladder(&old, &new, name));
                    let elements = if matches!(status, BodyStatus::Different { .. }) { element_diff(&old, &new, name) } else { None };
                    let graphs = graph_pairs(old_group, new_group, [original, name]);
                    results.push(BodyComparison { name: name.clone(), status, config: config(name), elements, graphs, place });
                }
                (true, false) => {
                    let graphs = graph_pairs(old_group, None, [original, name]);
                    results.push(BodyComparison { name: original.clone(), status: BodyStatus::BeforeOnly, config: config(name), elements: None, graphs, place });
                }
                (false, true) if !declared => {
                    let graphs = graph_pairs(None, new_group, [name, name]);
                    results.push(BodyComparison { name: name.clone(), status: BodyStatus::AfterOnly, config: config(name), elements: None, graphs, place });
                }
                _ => {}
            }
        }
        !bodies.is_empty()
    };
    for (name, original) in &before.names {
        if after.names.contains_key(name) {
            matched.insert(name.clone());
        }
        if split(name, original, &mut results) {
            continue;
        }
        if !after.names.contains_key(name) {
            let graphs = graph_pairs(before.group("n0", name), None, [original, name]);
            results.push(BodyComparison { name: original.clone(), status: BodyStatus::BeforeOnly, config: config(name), elements: None, graphs, place: None });
            continue;
        }
        let status = stopped(name, ladder(&before, &after, name));
        let elements = if matches!(status, BodyStatus::Different { .. }) { element_diff(&before, &after, name) } else { None };
        let graphs = graph_pairs(before.group("n0", name), after.group("n0", name), [original, name]);
        results.push(BodyComparison { name: name.clone(), status, config: config(name), elements, graphs, place: None });
    }
    for name in after.names.keys() {
        let declared = added.iter().any(|new| name == new || is_child(name, new));
        if !matched.contains(name) && !declared {
            if split(name, name, &mut results) {
                continue;
            }
            let graphs = graph_pairs(None, after.group("n0", name), [name, name]);
            results.push(BodyComparison { name: name.clone(), status: BodyStatus::AfterOnly, config: config(name), elements: None, graphs, place: None });
        }
    }
    // [R94.1-1] [R94.2-1] [R94.4-2] The checks of design 94 for each changed body pair.
    let started = std::time::Instant::now();
    transforms(&before, &after, &mut results);
    TRANSFORM_NANOS.fetch_add(started.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    results.sort_by(|a, b| (&a.name, &a.place).cmp(&(&b.name, &b.place)));
    results
}

/// [R94.1-1] The time of the checks of design 94 in this process (the phase of the
/// transformations).
pub static TRANSFORM_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The one N0 text of a name on a side, with its element ranges: `None` if the targets of the
/// name have more than one text or no element data.
fn one_text<'g>(side: &'g Side, name: &str) -> Option<(&'g str, &'g elements::RecordElements)> {
    one_body(side, name).map(|(text, record, _)| (text, record))
}

/// [`one_text`] with the form of the equivalence check of the same record (R95.2-1).
fn one_body<'g>(side: &'g Side, name: &str) -> Option<(&'g str, &'g elements::RecordElements, Option<&'g lazy::LazyText>)> {
    let group = side.group("n0", name)?;
    let texts: BTreeSet<&String> = group.texts.values().flatten().collect();
    let [text] = texts.into_iter().collect::<Vec<_>>()[..] else { return None };
    let target = group.texts.iter().find(|(_, list)| list.contains(text)).map(|(target, _)| target)?;
    let record = group.elements.get(target)?.first()?.as_deref()?;
    let form = group.forms.get(target).and_then(|list| list.first());
    Some((text.as_str(), record, form))
}

/// The def paths of the direct calls of a canonical text (`transform::called_paths`).
fn called_paths(text: &str) -> BTreeSet<&str> {
    text.lines().flat_map(transform::called_paths).collect()
}

/// [R95.3-1] [R95.4-1] The body of a call text on a side, with the generic arguments of the
/// call: the body with the name of the call text, else the one body whose name has the path of
/// the call with the generic parameters in place of the arguments (`depot::part` for
/// `depot::part::<u32>`, `<depot::Wrap<T> as depot::Tax>::rate` for `<depot::Wrap<u32> as
/// depot::Tax>::rate`). `None`: no body, or two bodies fit.
fn resolve_target(side: &Side, index: &std::cell::OnceCell<BTreeMap<String, Vec<String>>>, path: &str) -> Option<transform::Target> {
    let bind = |name: &str| -> Option<transform::Target> {
        let (text, _) = one_text(side, name)?;
        let params = transform::generic_names(text);
        let args = transform::bind_args(path, name, &params)?;
        Some(transform::Target { path: path.to_string(), name: name.to_string(), args: if params.is_empty() { Vec::new() } else { args }, closure: false })
    };
    if side.names.contains_key(path) {
        return bind(path);
    }
    let index = index.get_or_init(|| {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in side.names.keys() {
            out.entry(transform::skeleton(name)).or_default().push(name.clone());
        }
        out
    });
    let found: Vec<transform::Target> = index.get(&transform::skeleton(path))?.iter().filter_map(|name| bind(name)).collect();
    match <[transform::Target; 1]>::try_from(found) {
        Ok([one]) => Some(one),
        Err(_) => None,
    }
}

/// [R94.1-1] [R94.2-1] [R94.4-2] The checks of design 94 of each changed body pair with element
/// data (the source bodies of one name, R61.14-1, get no check): an extract (a call of the new
/// body to a callee that the step adds or that does not change), an inline (the reverse), and a
/// new order of the same parts. [R95.3-1] [R95.4-1] A call of a generic callee or of a trait
/// method that rustc resolves to an impl method of the workspace is a candidate too.
fn transforms(before: &Side, after: &Side, results: &mut [BodyComparison]) {
    let equal: BTreeSet<String> = results.iter().filter(|result| result.place.is_none() && matches!(result.status, BodyStatus::Equal { .. })).map(|result| result.name.clone()).collect();
    let indexes = [std::cell::OnceCell::new(), std::cell::OnceCell::new()];
    for result in results.iter_mut() {
        if result.place.is_some() || !matches!(result.status, BodyStatus::Different { .. }) {
            continue;
        }
        let Some(diff) = result.elements.as_mut() else { continue };
        if diff.old.is_empty() && diff.new.is_empty() {
            continue;
        }
        let name = result.name.clone();
        let (Some(old), Some(new)) = (one_body(before, &name), one_body(after, &name)) else { continue };
        // [R95.2-1] The effect summary of each call of the two versions.
        let facts = [old.2.and_then(|form| form.as_deref()).and_then(transform::call_facts), new.2.and_then(|form| form.as_deref()).and_then(transform::call_facts)];
        let (old, new) = ((old.0, old.1), (new.0, new.1));
        let sides = [transform::Side { name: &name, text: old.0, record: old.1 }, transform::Side { name: &name, text: new.0, record: new.1 }];
        let mut found: Vec<transform::Transform> = Vec::new();
        for (text, kind) in [(new.0, transform::Kind::Extract), (old.0, transform::Kind::Inline)] {
            let (at, other) = if kind == transform::Kind::Extract { (1, 0) } else { (0, 1) };
            let trees = [before, after];
            for path in called_paths(text) {
                let Some(target) = resolve_target(trees[at], &indexes[at], path) else { continue };
                // An extract: a callee that the step adds, or that does not change; an inline: a
                // callee that the step removes, or that does not change.
                let one_side = !trees[other].names.contains_key(&target.name);
                if target.name == name || !(one_side || equal.contains(&target.name)) {
                    continue;
                }
                if let Some((text, record)) = one_text(trees[at], &target.name) {
                    found.extend(transform::check_target([&sides[0], &sides[1]], &transform::Side { name: &target.name, text, record }, &target, kind, diff, facts[at].as_ref()));
                }
            }
            // [R95.6-1] A closure that the body makes and calls: an extract into a closure that
            // the step adds, or an inline of a closure that the step removes.
            for closure in transform::closure_bodies(text) {
                if trees[other].names.contains_key(&closure) && !equal.contains(&closure) {
                    continue;
                }
                if let Some((text, record)) = one_text(trees[at], &closure) {
                    let target = transform::Target { path: closure.clone(), name: closure.clone(), args: Vec::new(), closure: true };
                    found.extend(transform::check_target([&sides[0], &sides[1]], &transform::Side { name: &closure, text, record }, &target, kind, diff, facts[at].as_ref()));
                }
            }
        }
        transform::mark_ties(&mut found);
        found.truncate(transform::MOST_CANDIDATES);
        // [R100.3-1] The elements that the check of a transformation explains leave the rest of
        // their region (design 100): the alignment pairs the rest of the region again without
        // them, so the code that the transformation leaves pairs only with code that no check
        // explains. The other side of a check has the region, the side of the call has the call.
        let mut explained: [BTreeSet<elements::Place>; 2] = [BTreeSet::new(), BTreeSet::new()];
        // Only a check that holds: a candidate that does not hold can be a call of an other
        // callee that the step keeps.
        for candidate in found.iter().filter(|candidate| candidate.holds && !candidate.tie) {
            let (at, other) = if candidate.kind == transform::Kind::Extract { (1, 0) } else { (0, 1) };
            explained[other].extend(candidate.region.iter().cloned());
            explained[at].extend(candidate.call.iter().cloned());
        }
        let one_table = diff.graph_tables.len() <= 1 && diff.table.as_ref().is_some_and(|table| table.texts[0] == old.0 && table.texts[1] == new.0);
        if one_table && explained.iter().any(|set| !set.is_empty())
            && let Some(again) = elements::diff_explained(old, new, [&explained[0], &explained[1]])
        {
            diff.changed = again.changed;
            diff.table = again.table;
            diff.graph_tables = again.graph_tables;
        }
        diff.transforms = found;
        diff.reorder = transform::permutation_with(old, new, [facts[0].as_ref(), facts[1].as_ref()], Some(diff));
    }
}

/// Gives a body of the old tree the name of the body of the new tree when the two names are
/// different only in a lifetime (design 37.2): `<&'a mut a::Node as a::Parse>::parse` and
/// `<&mut a::Node as a::Parse>::parse`. The header of the impl changed, and its bodies did
/// not move. The two names make a pair only if no other body name of the two trees has the
/// same text without lifetimes. `None` if there is no such pair.
fn lifetime_names(before: &Levels, after: &Levels, marks: &Marks) -> Option<(Levels, Marks)> {
    let names = |levels: &Levels| -> BTreeSet<String> { levels.values().flatten().map(|record| record.name.clone()).collect() };
    let (old, new) = (names(before), names(after));
    if !old.iter().chain(&new).any(|name| name.contains('\'')) {
        return None;
    }
    let keys = |names: &BTreeSet<String>| -> BTreeMap<String, Vec<String>> {
        let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in names {
            keys.entry(paths::no_lifetimes(name)).or_default().push(name.clone());
        }
        keys
    };
    let (old_keys, new_keys) = (keys(&old), keys(&new));
    let mut pairs = BTreeMap::new();
    for (key, list) in &old_keys {
        if let ([from], Some([to])) = (list.as_slice(), new_keys.get(key).map(Vec::as_slice)) {
            if from != to && !new.contains(from) && !old.contains(to) {
                pairs.insert(from.clone(), to.clone());
            }
        }
    }
    if pairs.is_empty() {
        return None;
    }
    let mut levels = before.clone();
    for record in levels.values_mut().flat_map(|records| records.make_mut().iter_mut()) {
        if let Some(to) = pairs.get(&record.name) {
            record.name = to.clone();
        }
    }
    let rename = |names: &BTreeSet<String>| names.iter().map(|name| pairs.get(name).unwrap_or(name).clone()).collect();
    Some((levels, Marks { opaque: rename(&marks.opaque), stop: rename(&marks.stop) }))
}

/// The start of each body name of `levels` that fits `path` with generic arguments
/// (`a::Type::<T>::method` for `a::Type::method`).
fn generic_starts(levels: &Levels, path: &str) -> BTreeSet<String> {
    let last = path.rsplit("::").next().unwrap_or(path);
    levels.values().flatten().filter(|record| record.name.contains(last)).filter_map(|record| paths::generic_prefix(&record.name, path).map(str::to_string)).collect()
}

/// The lists of generic arguments of a body name start (`a::W::<T, N>::m` gives `[[T, N]]`).
/// `None` if an argument is not a plain name.
fn generic_lists(start: &str) -> Option<Vec<Vec<String>>> {
    let mut lists = Vec::new();
    let mut rest = start;
    while let Some(at) = rest.find("::<") {
        let inner = &rest[at + 3..];
        let end = inner.find('>')?;
        let list: Vec<String> = inner[..end].split(", ").map(str::to_string).collect();
        if list.iter().any(|name| name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_')) {
            return None;
        }
        lists.push(list);
        rest = &inner[end..];
    }
    Some(lists)
}

/// Replaces each word of `map` in a text (a word is a run of name characters).
fn replace_words(text: &str, map: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        out.push_str(map.get(word.as_str()).unwrap_or(word));
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// The generic parameters of a renamed impl item with new names (design 38.10): the body name
/// `a::W::<T>::get` becomes `a::W::<U>::value`, and the body text has `U` for `T`. The old
/// records of such a body get the new names of the parameters. A pair applies only if exactly
/// one body name of each tree fits the paths, each argument is a plain name, and no new name is
/// a word of an old body text (and no old name a word of a new body text). `None` if no pair
/// applies.
fn param_names(before: &Levels, after: &Levels, usable: &[(String, String)], marks: &Marks) -> Option<(Levels, Marks)> {
    let mut changes: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for (from, to) in usable.iter().filter(|(from, to)| from != to && from.matches("::").count() >= 2) {
        let (old, new) = (generic_starts(before, from), generic_starts(after, to));
        let (Some(old), Some(new), 1, 1) = (old.first(), new.first(), old.len(), new.len()) else { continue };
        let (Some(old_lists), Some(new_lists)) = (generic_lists(old), generic_lists(new)) else { continue };
        if old_lists.len() != new_lists.len() || old_lists.iter().zip(&new_lists).any(|(a, b)| a.len() != b.len()) {
            continue;
        }
        let map: BTreeMap<String, String> = old_lists.iter().flatten().zip(new_lists.iter().flatten()).filter(|(a, b)| a != b).map(|(a, b)| (a.clone(), b.clone())).collect();
        // A parameter with the name of a path segment (`impl<W> W<W>`) is not changed.
        if map.is_empty() || map.keys().chain(map.values()).any(|name| from.split("::").chain(to.split("::")).any(|segment| segment == name)) {
            continue;
        }
        let of = |levels: &Levels, start: &str| -> Vec<String> { levels.values().flatten().filter(|record| record.name == start || record.name.strip_prefix(start).is_some_and(|rest| rest.starts_with("::"))).map(|record| record.text.clone()).collect() };
        let words = |texts: &[String]| -> BTreeSet<String> { texts.iter().flat_map(|text| text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|word| !word.is_empty()).map(str::to_string).collect::<Vec<_>>()).collect() };
        let (old_words, new_words) = (words(&of(before, old)), words(&of(after, new)));
        if map.values().any(|name| old_words.contains(name)) || map.keys().any(|name| new_words.contains(name)) {
            continue;
        }
        changes.push((old.clone(), map));
    }
    if changes.is_empty() {
        return None;
    }
    let change = |name: &str| changes.iter().find(|(start, _)| name == start || name.strip_prefix(start.as_str()).is_some_and(|rest| rest.starts_with("::"))).map(|(_, map)| map);
    let mut levels = before.clone();
    // A different body names the old start in its text and in its callees: it gets the new
    // start (`genren::W::<T>::get` becomes `genren::W::<U>::get`).
    let starts: Vec<(String, String)> = changes.iter().map(|(start, map)| (start.clone(), replace_words(start, map))).collect();
    let other = |text: &str| -> String {
        let mut text = text.to_string();
        for (old, new) in &starts {
            let mut out = String::with_capacity(text.len());
            let mut rest = text.as_str();
            while let Some(at) = rest.find(old.as_str()) {
                let after = &rest[at + old.len()..];
                let start_ok = rest[..at].chars().last().is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == ':'));
                let end_ok = after.chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                out.push_str(&rest[..at]);
                out.push_str(if start_ok && end_ok { new } else { old });
                rest = after;
            }
            out.push_str(rest);
            text = out;
        }
        text
    };
    for record in levels.values_mut().flat_map(|records| records.make_mut().iter_mut()) {
        if let Some(map) = change(&record.name) {
            record.name = replace_words(&record.name, map);
            record.text = replace_words(&record.text, map);
        } else {
            record.text = other(&record.text);
        }
        for callee in record.callees.iter_mut() {
            *callee = other(callee);
        }
    }
    let rename = |names: &BTreeSet<String>| names.iter().map(|name| change(name).map(|map| replace_words(name, map)).unwrap_or_else(|| name.clone())).collect();
    Some((levels, Marks { opaque: rename(&marks.opaque), stop: rename(&marks.stop) }))
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

/// The generic keys of the rename map (design 37.4). The key of a rename is a path of the source
/// (`a::Type::method`). The name of a body of a generic impl has the generic arguments
/// (`a::Type::<T>::method`), and the plain key does not match it. A rename gets a generic key
/// (`paths::GENERIC_KEY`) if these conditions are true:
///
/// - Exactly one body name of the old tree fits the old path, and no body has the plain path.
/// - At most one body name of the new tree fits the new path.
/// - No body of the old tree has the new path with generic arguments, in its name or in its text.
///
/// If two bodies fit one key (`Type::<u8>::get` and `Type::<u16>::get`), the key is not
/// unique: the rename gets no generic key, and the compare result stays `code different`.
fn generic_renames(before: &Levels, after: &Levels, usable: &[(String, String)], renames: &BTreeMap<String, String>) -> Vec<(String, String)> {
    // Only a path with two segments or more after the crate name can have a list in it.
    let candidates: Vec<&(String, String)> = usable.iter().filter(|(from, to)| from != to && from.matches("::").count() >= 2).collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let names = |levels: &Levels| -> BTreeSet<String> { levels.values().flatten().map(|record| record.name.clone()).collect() };
    let (old_names, new_names) = (names(before), names(after));
    // The distinct starts of the body names that fit a path, and true if a body has the plain path.
    let fits = |names: &BTreeSet<String>, path: &str| -> (BTreeSet<String>, bool) {
        let last = path.rsplit("::").next().unwrap_or(path);
        let mut starts = BTreeSet::new();
        let mut plain = false;
        for name in names.iter().filter(|name| name.contains(last)) {
            if let Some(start) = paths::generic_prefix(name, path) {
                starts.insert(start.to_string());
            }
            plain |= name == path || name.strip_prefix(path).is_some_and(|rest| rest.starts_with("::"));
        }
        (starts, plain)
    };
    let mut out = Vec::new();
    for (from, to) in candidates {
        let (old, plain) = fits(&old_names, from);
        if old.len() != 1 || plain {
            continue;
        }
        let (new, new_plain) = fits(&new_names, to);
        if new.len() > 1 || new_plain {
            continue;
        }
        // The new path must not be there before the step, as `usable_renames` requires.
        if !renames.contains_key(to) {
            // The marker has the segments of the path, so that each list has a place.
            let marker = [(format!("{}{to}", paths::GENERIC_KEY), format!("{to}\u{0}"))];
            let last = to.rsplit("::").next().unwrap_or(to);
            let mentions = |text: &String| text.contains(last) && paths::rename(text, &marker) != *text;
            if before.values().flatten().any(|record| mentions(&record.name) || mentions(&record.text)) {
                continue;
            }
        }
        out.push((format!("{}{from}", paths::GENERIC_KEY), to.clone()));
    }
    out
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
    /// The element ranges of each record, by target (at N0). The text of a target is the one
    /// text of `texts`.
    elements: BTreeMap<String, Vec<Option<std::sync::Arc<elements::RecordElements>>>>,
    /// The control flow graph of each record, by target (at N0), with the rename map on the
    /// call texts (design 60).
    graphs: BTreeMap<String, Vec<Option<std::sync::Arc<graph::RecordGraph>>>>,
    /// [R95.2-1] The form of the equivalence check of each record, by target (at N0): the
    /// effect summary of each call (R64.3-1) and the body that rustc resolves it to.
    forms: BTreeMap<String, Vec<lazy::LazyText>>,
    /// The source file of the N0 record of each target (design 61.14, R61.14-1).
    files: BTreeMap<String, String>,
    /// The build settings of the unit of each target, with no feature (R61.16-2).
    units: BTreeMap<String, String>,
    /// [R88.3-1] The digests of the raw texts of each target, from the index of the records.
    /// `None` if a record of the group has no digest from the index, or a text that the
    /// normal form of the test positions changes: then the compare reads the texts.
    digests: Option<BTreeMap<String, BTreeSet<index::Digest>>>,
    /// True if a record of the group has no digest of its raw text.
    no_digest: bool,
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
    // A switch with number targets only keeps the order of the driver. The entry `otherwise`
    // is not a named target (batch 5x, part B): with it in the test, the rename of a callee
    // sorted the numbers as text (`10, 11, 1, 2`), and the caller was not equal.
    sort_list(&text, SWITCH_OPEN, " -> [", |parts| {
        parts.iter().any(|part| !part.starts_with(|c: char| c.is_ascii_digit() || c == '-') && !part.starts_with("otherwise: "))
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
        // R86.3-1: the normal text is a borrow of the record when the record has no test position.
        let text = without_test_positions(&record.text, &record.name);
        let target = if use_targets { record.target.clone() } else { String::new() };
        match (record.known.from_index(), &text) {
            (Some(facts), std::borrow::Cow::Borrowed(_)) if !self.no_digest => {
                self.digests.get_or_insert_default().entry(target.clone()).or_default().insert(facts.text);
            }
            _ => {
                self.no_digest = true;
                self.digests = None;
            }
        }
        if !renames.is_empty() {
            self.raw
                .get_or_insert_default()
                .entry(target.clone())
                .or_default()
                .insert(text.to_string());
        }
        // A rename can change the order of the named operands of an aggregate, of the named
        // targets of a switch, and of the entries of a discriminant table.
        let renamed = match paths::rename_cow(&text, renames) {
            std::borrow::Cow::Owned(renamed) if renamed != *text => sort_variant_lists(&sort_aggregates(&renamed)),
            _ => text.into_owned(),
        };
        if record.level == "n0" {
            if let Some(span) = &record.span {
                self.files.entry(target.clone()).or_insert_with(|| span.file.clone());
            }
            self.units.entry(target.clone()).or_insert_with(|| record.unit.clone());
            self.elements.entry(target.clone()).or_default().push(record.elements.clone());
            self.graphs.entry(target.clone()).or_default().push(record.graph.as_ref().map(|found| graph::renamed(found, renames)));
            self.forms.entry(target.clone()).or_default().push(record.q.clone());
        }
        self.texts.entry(target.clone()).or_default().insert(renamed);
        self.callees.extend(record.callees.iter().map(|callee| paths::rename_cow(callee, renames).into_owned()));
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

    /// The group with only the records of some targets (design 61.14, R61.14-1). The callees
    /// and the shapes stay the sets of each target.
    fn only(&self, targets: &BTreeSet<String>) -> Group {
        fn keep<T: Clone>(map: &BTreeMap<String, T>, targets: &BTreeSet<String>) -> BTreeMap<String, T> {
            map.iter().filter(|(target, _)| targets.contains(*target)).map(|(target, value)| (target.clone(), value.clone())).collect()
        }
        Group {
            raw: self.raw.as_ref().map(|raw| keep(raw, targets)),
            texts: keep(&self.texts, targets),
            callees: self.callees.clone(),
            shapes: self.shapes.clone(),
            terms: keep(&self.terms, targets),
            from_n2: self.from_n2,
            elements: keep(&self.elements, targets),
            graphs: keep(&self.graphs, targets),
            forms: keep(&self.forms, targets),
            files: keep(&self.files, targets),
            units: keep(&self.units, targets),
            digests: self.digests.as_ref().map(|digests| keep(digests, targets)),
            no_digest: self.no_digest,
        }
    }

    /// True if the group has no record.
    fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }
}

/// The source bodies of a name (design 61.14, R61.14-1): the targets of the name by the source
/// file of their N0 records, on the two sides. Two packages can each have a target with one
/// kind and one name (a test target `integration` in each package), so one body name can be
/// two source bodies. A lib and its test build have one file, so they stay one body. Empty if
/// no tree has the name in two files.
fn source_bodies(before: &Side, after: &Side, name: &str) -> Vec<(String, BTreeSet<String>)> {
    let mut files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut seen: BTreeSet<&String> = BTreeSet::new();
    for side in [after, before] {
        for (target, file) in side.group("n0", name).map(|group| &group.files).into_iter().flatten() {
            if seen.insert(target) {
                files.entry(file.clone()).or_default().insert(target.clone());
            }
        }
    }
    // Only a name with two source bodies in one tree: a body that moves to a different file
    // (a move to another crate) is one body with a new file.
    let one_tree = [before, after].iter().any(|side| side.group("n0", name).is_some_and(|group| group.files.values().collect::<BTreeSet<_>>().len() > 1));
    if files.len() < 2 || !one_tree {
        return Vec::new();
    }
    // A target with no N0 file (an N2 or N3 record only) has no source body: no split.
    let all: BTreeSet<&String> = [before, after].iter().flat_map(|side| ["n0", "n2", "n3"].map(|level| side.group(level, name))).flatten().flat_map(|group| group.texts.keys()).collect();
    if all.iter().any(|target| !seen.contains(*target)) {
        return Vec::new();
    }
    files.into_iter().collect()
}

/// A view of one side with the records of a name restricted to some targets.
struct Only<'a> {
    side: &'a Side,
    name: &'a str,
    /// The groups of the name at each level, restricted to the targets.
    groups: BTreeMap<String, Group>,
}

impl<'a> Only<'a> {
    fn new(side: &'a Side, name: &'a str, targets: &BTreeSet<String>) -> Only<'a> {
        let groups = ["n0", "n2", "n3"].iter().filter_map(|level| Some((level.to_string(), side.group(level, name)?.only(targets)))).filter(|(_, group)| !group.is_empty()).collect();
        Only { side, name, groups }
    }
}

/// The groups of one side, for the compare of one body.
trait Groups {
    fn group(&self, level: &str, name: &str) -> Option<&Group>;
    fn hashed_texts(&self, level: &str, group: &Group) -> BTreeMap<String, BTreeSet<String>>;
}

impl Groups for Side {
    fn group(&self, level: &str, name: &str) -> Option<&Group> {
        Side::group(self, level, name)
    }
    fn hashed_texts(&self, level: &str, group: &Group) -> BTreeMap<String, BTreeSet<String>> {
        Side::hashed_texts(self, level, group)
    }
}

impl Groups for Only<'_> {
    fn group(&self, level: &str, name: &str) -> Option<&Group> {
        if name == self.name { self.groups.get(level) } else { self.side.group(level, name) }
    }
    fn hashed_texts(&self, level: &str, group: &Group) -> BTreeMap<String, BTreeSet<String>> {
        self.side.hashed_texts(level, group)
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
}

/// The count of the bodies that the N0 compare found equal by the digests of the index
/// (R88.3-1; a timing line reads it).
pub static FROM_INDEX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Tries the levels in order for one body, and stops at the first equal level.
fn ladder(before: &impl Groups, after: &impl Groups, name: &str) -> BodyStatus {
    let equal =
        |level: &str, proof: Vec<String>| BodyStatus::Equal { level: level.to_string(), proof };

    if let (Some(b), Some(a)) = (before.group("n0", name), after.group("n0", name)) {
        // [R88.3-1] The digests of the index first: equal digests are equal texts, and other
        // digests are other texts. Only a group with no digests reads the texts.
        match (&b.digests, &a.digests) {
            (Some(old), Some(new)) => {
                if same_bodies(old, new) {
                    FROM_INDEX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return equal("n0", Vec::new());
                }
            }
            _ => {
                if same_bodies(b.raw(), a.raw()) {
                    return equal("n0", Vec::new());
                }
            }
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

    let shapes = |side: &dyn Groups| {
        ["n0", "n2", "n3"]
            .iter()
            .find_map(|level| side.group(level, name))
            .map(|group| group.shapes.clone())
    };
    BodyStatus::Different { cfg_changed: shapes(before) != shapes(after) }
}

/// The elements that differ in a changed body (design 40.3), over each target. The targets
/// pair by their name, or by their name without the hash, or as the one target of each side.
/// `None` if a target has no partner, a target has two texts, or a record has no element data.
fn element_diff(before: &impl Groups, after: &impl Groups, name: &str) -> Option<elements::ElementDiff> {
    let (old, new) = (before.group("n0", name)?, after.group("n0", name)?);
    let pairs: Vec<(&String, &String)> = if old.elements.keys().eq(new.elements.keys()) {
        old.elements.keys().zip(new.elements.keys()).collect()
    } else {
        fn stems(group: &Group) -> BTreeMap<&str, &String> {
            group.elements.keys().map(|target| (target_stem(target), target)).collect()
        }
        let (a, b) = (stems(old), stems(new));
        if a.len() == old.elements.len() && b.len() == new.elements.len() && a.keys().eq(b.keys()) {
            a.values().copied().zip(b.values().copied()).collect()
        } else if old.elements.len() == 1 && new.elements.len() == 1 {
            old.elements.keys().zip(new.elements.keys()).collect()
        } else {
            return None;
        }
    };
    // A target with one text: the text and the ranges of its first record.
    let one = |group: &Group, target: &String| -> Option<(String, std::sync::Arc<elements::RecordElements>)> {
        let [text] = group.texts.get(target)?.iter().collect::<Vec<_>>()[..] else { return None };
        Some((text.clone(), group.elements.get(target)?.first()?.clone()?))
    };
    let mut out = elements::ElementDiff::default();
    for (a, b) in pairs {
        let (old_text, old_ranges) = one(old, a)?;
        let (new_text, new_ranges) = one(new, b)?;
        if old_text != new_text {
            out.merge(elements::diff_cached((&old_text, &old_ranges), (&new_text, &new_ranges))?);
        }
    }
    Some(out)
}

/// The graphs of a body on the two sides (design 60), for each pair of targets. The targets
/// pair as in [`element_diff`]. A target with no partner gives a pair with one side. A target
/// with more than one record takes the graph of its first record.
///
/// R61.16-2: no pair depends on the order of the targets (the hash in a target name depends
/// on the folders of the build). The pairs are in the order of the target names without the
/// hash, then of the texts. Two targets of one name without the hash on one side pair by the
/// build settings of their units, else only if the texts of each side are equal; else the name
/// gives one pair that is not compared.
fn graph_pairs(old: Option<&Group>, new: Option<&Group>, names: [&str; 2]) -> Vec<graph::GraphPair> {
    let first = |group: Option<&Group>, target: &String| -> Option<std::sync::Arc<graph::RecordGraph>> { group?.graphs.get(target)?.first()?.clone() };
    let empty = BTreeMap::new();
    let (a, b) = (old.map_or(&empty, |group| &group.graphs), new.map_or(&empty, |group| &group.graphs));
    fn texts<'g>(group: Option<&'g Group>, target: Option<&String>, none: &'g BTreeSet<String>) -> &'g BTreeSet<String> {
        target.and_then(|target| group?.texts.get(target)).unwrap_or(none)
    }
    let none = BTreeSet::new();
    // `None` in a pair of targets: a pair that is not compared.
    let mut pairs: Vec<Option<(Option<&String>, Option<&String>)>> = Vec::new();
    let mut open: Vec<(bool, bool)> = Vec::new();
    if a.keys().eq(b.keys()) {
        pairs.extend(a.keys().zip(b.keys()).map(|(x, y)| Some((Some(x), Some(y)))));
    } else if a.len() == 1 && b.len() == 1 {
        pairs.push(Some((a.keys().next(), b.keys().next())));
    } else {
        let stems = |map: &'_ BTreeMap<String, Vec<Option<std::sync::Arc<graph::RecordGraph>>>>| -> BTreeMap<String, Vec<String>> {
            let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for target in map.keys() {
                found.entry(target_stem(target).to_string()).or_default().push(target.clone());
            }
            found
        };
        let (x, y) = (stems(a), stems(b));
        let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
        for key in keys {
            let left: Vec<&String> = x.get(key).into_iter().flatten().filter_map(|target| a.get_key_value(target).map(|(target, _)| target)).collect();
            let right: Vec<&String> = y.get(key).into_iter().flatten().filter_map(|target| b.get_key_value(target).map(|(target, _)| target)).collect();
            if left.len() <= 1 && right.len() <= 1 {
                pairs.push(Some((left.first().copied(), right.first().copied())));
                continue;
            }
            // Two targets of one name without the hash. A side with no target: no choice, a
            // pair with one side for each target. Else the targets pair by the build settings
            // of their units (a library with `panic="unwind"` and with `panic="abort"`), if
            // each side has the same distinct settings; else only if each target of a side has
            // the same texts, so that each pair has the same content.
            if left.is_empty() || right.is_empty() {
                pairs.extend(left.iter().map(|x| Some((Some(*x), None))).chain(right.iter().map(|y| Some((None, Some(*y))))));
                continue;
            }
            let settings = |group: Option<&Group>, list: &[&String]| -> Option<BTreeMap<String, String>> {
                let map: BTreeMap<String, String> = list.iter().filter_map(|target| Some((group?.units.get(*target)?.clone(), (*target).clone()))).collect();
                (map.len() == list.len() && !map.contains_key("")).then_some(map)
            };
            let one_text = |group: Option<&Group>, list: &[&String]| list.windows(2).all(|two| texts(group, Some(two[0]), &none) == texts(group, Some(two[1]), &none));
            if let (Some(x), Some(y)) = (settings(old, &left), settings(new, &right))
                && x.keys().eq(y.keys())
            {
                for (unit, target) in &x {
                    let left = left.iter().copied().find(|found| *found == target);
                    let right = y.get(unit).and_then(|target| right.iter().copied().find(|found| *found == target));
                    pairs.push(Some((left, right)));
                }
            } else if left.len() == right.len() && one_text(old, &left) && one_text(new, &right) {
                pairs.extend(left.iter().zip(&right).map(|(x, y)| Some((Some(*x), Some(*y)))));
            } else {
                pairs.push(None);
                open.push((!left.is_empty(), !right.is_empty()));
            }
        }
    }
    let mut found: Vec<(Option<&String>, Option<&String>)> = pairs.iter().flatten().copied().collect();
    found.sort_by(|(a, b), (c, d)| {
        let stems = |x: &Option<&String>, y: &Option<&String>| (x.map(|target| target_stem(target).to_string()), y.map(|target| target_stem(target).to_string()));
        stems(a, b).cmp(&stems(c, d)).then_with(|| (texts(old, *a, &none), texts(new, *b, &none)).cmp(&(texts(old, *c, &none), texts(new, *d, &none))))
    });
    let mut out: Vec<graph::GraphPair> = found
        .into_iter()
        .map(|(left, right)| graph::GraphPair {
            old_body: left.is_some(),
            new_body: right.is_some(),
            old: left.and_then(|target| first(old, target)),
            new: right.and_then(|target| first(new, target)),
            old_key: left.map(|target| (names[0].to_string(), target.clone())),
            new_key: right.map(|target| (names[1].to_string(), target.clone())),
            not_compared: false,
        })
        .collect();
    out.extend(open.into_iter().map(|(old_body, new_body)| graph::GraphPair { old_body, new_body, not_compared: true, ..graph::GraphPair::default() }));
    out
}

/// True if the texts of one body are the same on both sides, by target.
///
/// The targets of a body differ when the body moves to another crate (`move-item`), or when the
/// package of a crate changes. No target map exists then, so the bodies are equal only when each
/// side has exactly one distinct text for all its targets, and the two texts are the same.
///
/// [R88.3-1] The values can be the texts or their digests: the result is the same, because a
/// digest names one text.
fn same_bodies<T: Ord>(
    before: &BTreeMap<String, BTreeSet<T>>,
    after: &BTreeMap<String, BTreeSet<T>>,
) -> bool {
    if before.keys().eq(after.keys()) {
        return before == after;
    }
    // A change of the package version or of a dependency changes the hash in each target name
    // (design 37.12). The targets then pair by their name without the hash: for each such name,
    // the two sides have the same number of targets with the same texts.
    fn by_stem<T: Ord>(texts: &BTreeMap<String, BTreeSet<T>>) -> BTreeMap<&str, Vec<&BTreeSet<T>>> {
        let mut stems: BTreeMap<&str, Vec<&BTreeSet<T>>> = BTreeMap::new();
        for (target, texts) in texts {
            stems.entry(target_stem(target)).or_default().push(texts);
        }
        stems.values_mut().for_each(|texts| texts.sort());
        stems
    }
    if by_stem(before) == by_stem(after) {
        return true;
    }
    fn single<T: Ord>(texts: &BTreeMap<String, BTreeSet<T>>) -> Option<&T> {
        let all: BTreeSet<&T> = texts.values().flatten().collect();
        match all.into_iter().collect::<Vec<_>>()[..] {
            [text] => Some(text),
            _ => None,
        }
    }
    matches!((single(before), single(after)), (Some(b), Some(a)) if b == a)
}

/// The name of a target without its hash: `depot.test` for `depot.test.480693c09eb877f6`. A name
/// with no hash at its end stays as it is.
pub fn target_stem(target: &str) -> &str {
    match target.rsplit_once('.') {
        Some((stem, hash)) if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) => stem,
        _ => target,
    }
}

#[cfg(test)]
mod tests;
