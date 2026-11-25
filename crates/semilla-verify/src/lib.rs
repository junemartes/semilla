//! The semilla verifier (the trusted core).
//!
//! The verifier checks each step on its own, against the two snapshots around it:
//! 1. It builds the claimed renames, and computes the syntax edits after the renames.
//! 2. It checks each claim against the edits (predict, match the shape, or mask).
//! 3. It checks the bodies with the normal forms (through a [`BodyOracle`]), and the condition
//!    effects with SMT (through an [`ExprOracle`]).
//! 4. It assigns each edit to one claim. An edit without a claim is uncovered. A claim without an
//!    edit is empty.
//! 5. It finds indirect changes: bodies whose MIR changed when their text did not.
//!
//! The change set check adds the group and subgroup rules, and the score.

pub mod agent;
pub mod cfgcond;
mod comments;
pub mod checks;
pub mod edits;
pub mod equiv;
pub mod expand;
pub mod facts;
pub mod files;
pub mod inputs;
pub mod flow;
pub mod hints;
pub mod identity;
pub mod intent;
mod inner;
pub mod organize;
mod overrides;
mod records;
pub mod owners;
mod paths;
pub mod scope;
mod bounds;
mod joint;
pub mod links;
pub mod made;
pub mod mirmap;
mod opaque;
pub mod sites;
pub use opaque::{KnownTrait, KNOWN_TRAITS};
mod srcconfig;
mod uncompiled;
pub use uncompiled::{TargetHidden, print_cfg_atoms, target_hidden};
pub mod pattern;
mod linediff;
mod positions;
mod tags;
pub mod views;

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::Effect;
use semilla_core::{Anchor, Category, ChangeSet, ClaimResult, ClaimStatus, Compare, Report, Step, StepReport, Uncovered};

/// The start of the reason when the derivation of a rename or a move stops for a second change.
pub(crate) const OTHER_CHANGE: &str = "the step has a second change: ";
use semilla_syntax::{Edit, Workspace};

/// The MIR status of one body (design section 8.2).
#[derive(Clone, Debug, PartialEq)]
pub enum BodyStatus {
    /// Equal at a normal form level (`n0` to `n4`). `proof` holds the rule chain for N4.
    Equal { level: String, proof: Vec<String> },
    /// Different at every level.
    Different { cfg_changed: bool },
    BeforeOnly,
    AfterOnly,
}

/// The comparison of one MIR body between the two snapshots of a step.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyComparison {
    /// The MIR def path (after the renames), for example `demo::cart::Cart::total`.
    pub name: String,
    /// The file and the first line of the body on the "after" side (else the "before" side).
    pub span: Option<(String, usize)>,
    pub status: BodyStatus,
    /// The uses of the build configuration in the body, on the two sides: the text of each
    /// macro call that reads it (`cfg!(debug_assertions)`).
    pub config: Vec<String>,
    /// The checked configurations in which the body is not equal, if the body is equal in a
    /// different checked configuration (batch 5s, A5). The list is empty in each other case.
    pub differs_in: Vec<String>,
    /// The checked configurations in which the body is equal, if `differs_in` is not empty
    /// (batch 5u, part A).
    pub equal_in: Vec<String>,
    /// The causes of a proof that Semilla removed (batch 5u): the body is equal in the MIR of
    /// each checked configuration, its text has an edit, and the body depends on a build
    /// condition that Semilla did not check. An entry has the form of an entry of `config`.
    /// The list is the data of a note. It has no part in a decision.
    pub stopped: Vec<String>,
    /// The byte range of the body in the file of `span`, in the file as written (design 40.2).
    /// `None` for a body from a macro expansion and for an old record.
    pub range: Option<(usize, usize)>,
    /// The MIR elements that differ, for a body that is `Different` (design 40.3). `None`: no
    /// element data, and each edit of the body keeps the rule of design 38 (the doubt rule).
    pub elements: Option<MirDiff>,
    /// The control flow graphs of the body on the two sides, for each checked configuration
    /// and each pair of targets (design 60). Empty: no graph data.
    pub graphs: Vec<flow::GraphPair>,
}

/// A source range of a MIR element: the file and the byte range in the file as written.
pub type MirPlace = (String, usize, usize);

/// One MIR element that differs (design 40.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirElement {
    /// The range of the tokens of the element. `None`: no source range.
    pub tokens: Option<MirPlace>,
    /// The range of the outermost macro call, for an element from a macro expansion.
    pub call: Option<MirPlace>,
    /// For a `switchInt` terminator: the count of its targets.
    pub targets: Option<usize>,
    /// True for `return` and for a call that does not return.
    pub exit: bool,
    /// True for `goto`, `return`, `resume`, `unreachable`, and `drop`: the glue of the blocks
    /// (design 40.3).
    pub glue: bool,
    /// True for `goto`, `return`, `resume`, and `unreachable`: a jump with no value.
    pub jump: bool,
    /// The symbols that the element names (design 44).
    pub symbols: Vec<Sym>,
    /// The sources of the difference of the element (design 45): one group for each changed
    /// local and each changed cleanup block that the difference needs.
    pub sources: Vec<MirGroup>,
}

/// A group of sources of a MIR element (design 45): a changed local or a changed cleanup block.
/// The element has a cause only if each group that does not follow it has one; with no such
/// group, one group that follows it (the local that it writes, a temporary that it reads)
/// needs one.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirGroup {
    pub follows: bool,
    pub sources: Vec<MirSource>,
}

/// A source of the difference of a MIR element (design 45). The side is 0 (old) or 1 (new).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MirSource {
    /// A declaration range of a changed local: the edits at this place.
    Declaration(usize, MirPlace),
    /// A changed element of the same body, by its tokens and its macro call: its causes.
    Element(usize, Option<MirPlace>, Option<MirPlace>),
}

/// A symbol that a MIR element or a definition names (design 44): the def path, and the place
/// of the definition in a file of the workspace (the file and the byte range).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sym {
    pub path: String,
    pub place: Option<MirPlace>,
}

/// The definitions of one side of a step (design 44), by the def path of each definition.
pub type DefMap = BTreeMap<String, Def>;

/// A definition of a record (design 44): its place, and the symbols that it names with the
/// flags of the driver ([`links::EVALUATED`], [`links::BODY`], [`links::SELF_TYPE`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Def {
    pub place: Option<MirPlace>,
    pub named: Vec<(Sym, u8)>,
}

/// The MIR elements that differ in a changed body, in each checked configuration together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MirDiff {
    pub old: Vec<MirElement>,
    pub new: Vec<MirElement>,
    /// A part of the MIR text that is not an element differs (the signature line).
    pub other: bool,
    /// The token ranges of the aligned element pairs (old, new), for the MIR check of the
    /// statement map (design 41.8).
    pub pairs: Vec<(MirPlace, MirPlace)>,
    /// True if a record of the body has no symbol data (design 44). Then the body keeps the
    /// rules of the evidence of design 40.3.
    pub no_symbols: bool,
    /// The symbol of the function of the body on each side (0: old, 1: new): the signature line
    /// (`other`) names it (design 44).
    pub own: Vec<(usize, Sym)>,
    /// True for a body that rustc evaluates at compile time: a constant or a static (design 44).
    pub evaluated: bool,
}

/// One compile unit of a checked build, from the record of the compiler (batch 5t, part A).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unit {
    /// The name of the unit. Each body record of the unit has it as its target.
    pub target: String,
    /// The folder of the package (relative to the workspace root).
    pub package: String,
    /// Each true `cfg` name (`unix`) and each true pair (`feature="std"`).
    pub cfg: BTreeSet<String>,
    /// Each source file that the unit reads (relative to the workspace root).
    pub files: BTreeSet<String>,
    /// The root file of the crate of the unit. Two units with one root are one crate in two
    /// builds, for example a library and its test build.
    pub root: String,
}

/// The true settings of one checked build configuration, by the compiler (batch 5s, B2;
/// batch 5t, part A).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigTruth {
    /// The name of the configuration, for example `release, all features`.
    pub name: String,
    /// Each compile unit of the build.
    pub units: Vec<Unit>,
    /// False if a record file of the build has no unit record. Then no condition is covered.
    pub known: bool,
}

/// The MIR facts of one body after a step, for the property claims.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BodyFacts {
    pub name: String,
    pub span: Option<(String, usize)>,
    /// The def paths of the local functions that the body calls.
    pub callees: Vec<String>,
    /// The def paths of the functions of other crates that the body calls.
    pub externals: Vec<String>,
    /// The implicit panic sites of the body: one entry for each MIR `Assert` terminator
    /// (`division by zero`, `bounds check`, `overflow`, …).
    pub panics: Vec<String>,
    /// The calls whose target the MIR does not give: through a function pointer, a trait object,
    /// or a trait method on a type parameter.
    pub indirect: Vec<String>,
    /// The enums whose discriminant value the body reads with a cast (`as`).
    pub casts: Vec<String>,
    /// The local types whose layout or discriminant values the MIR text of the body shows (see
    /// `layout_types`).
    pub layouts: Vec<String>,
    /// For each call to a function of another crate (by its printed path): the crate that
    /// defines the function, and the def path in that crate. A re-export does not change them.
    pub origins: BTreeMap<String, (String, String)>,
    /// The types and the traits whose trait impls the body can run through the code of another
    /// crate (the def path and the crate): the types in the generic arguments of the calls and
    /// in the unsize casts.
    pub reach_types: Vec<(String, String)>,
    /// The types with a destructor that the body can drop (the def path and the crate).
    pub drop_types: Vec<(String, String)>,
    /// For a type of `drop_types` that the body does not drop itself: the first call that has
    /// the type in its generic arguments (the def path of the type and the printed function).
    pub drop_calls: Vec<(String, String)>,
    /// For a method of a trait impl: the types and the trait of the impl header.
    pub impl_of: Vec<String>,
    /// For the `drop` method of a `Drop` impl: the type.
    pub drop_of: Option<String>,
    /// The types in the parameters of a function, through the fields of the types that are not
    /// of the standard library. `dyn` and `fn` are markers (a trait object, a function pointer).
    pub param_types: Vec<(String, String)>,
    /// The types in the constants and the statics that the body reads.
    pub const_types: Vec<(String, String)>,
    /// True if the signature line of the MIR text is different on the two sides of the step
    /// (batch 5n, A1). The MIR signature shows the type behind an `impl Trait` return type, so a
    /// change of that type changes the MIR of each caller.
    pub signature_changed: bool,
    /// The compiled target of the body. A body name can exist in more than one target.
    pub target: String,
    /// The functions that the body can call from a block that its start block reaches after
    /// the constant conditions fold, through its closures (the def paths). `None` for a closure
    /// and when the driver does not give the fact.
    pub live: Option<Vec<String>>,
    /// The functions that the body calls on each path from its start to a normal return (a
    /// path that ends with a panic does not count). `None` as for `live`.
    pub sure: Option<Vec<String>>,
    /// The facts for the ends of a run that are not a return (batch 5y, A4). The keys: `ends`
    /// (the calls of the body that end the process and are not panic code), `calls` (the
    /// functions that the body can call and that can end the process), `before NAME` (the
    /// entries that can run before the sure callee `NAME`; an end has `end ` first), and
    /// `first` (the sure callees with no call and no assert before them).
    pub ends: BTreeMap<String, Vec<String>>,
}

/// The local types whose member order or layout a printed body depends on, with no name of the
/// type in the source text: the type of a constant in memory (`layout-of k::Pair: …`), and an
/// enum whose discriminant values the body reads (`discriminant(_1) of k::Kind values [...]`,
/// `reads the values of k::Kind [...]`). A change of such a type changes the text of the body.
pub fn layout_types(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let path_end = |rest: &str| rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')).unwrap_or(rest.len());
    for marker in ["layout-of ", ") of ", ") cast of ", "reads the values of "] {
        for rest in text.split(marker).skip(1) {
            let path = &rest[..path_end(rest)];
            let path = path.trim_end_matches(':');
            if !path.is_empty() {
                found.push(path.to_string());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The enums whose discriminant a printed body reads with a cast (`as`). The driver prints such a
/// read as `_3 = discriminant(_2) cast of k::Kind values [...]`.
pub fn discriminant_casts(text: &str) -> Vec<String> {
    let mut found: Vec<String> = text
        .split(") cast of ")
        .skip(1)
        .filter_map(|rest| rest.split_once(" values [").map(|(path, _)| path.to_string()))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The kinds of the MIR `Assert` terminators in a printed body (one entry for each terminator).
pub fn assert_sites(text: &str) -> Vec<String> {
    let kinds = [
        ("bounds_check(", "bounds check"),
        ("overflow_neg(", "overflow"),
        ("overflow ", "overflow"),
        ("division_by_zero(", "division by zero"),
        ("remainder_by_zero(", "remainder by zero"),
        ("misaligned_pointer_dereference(", "pointer alignment"),
        ("null_pointer_dereference", "null pointer"),
    ];
    // B3: an assert whose condition compares two different constants never fails, for example
    // the zero check of `x / 100`: `_7 = Eq(const u64 = 100, const u64 = 0); assert(!_7, …)`.
    // The overflow check of a signed division by -1 compares a variable, so it stays.
    let assignments = |local: &str| text.lines().map(str::trim).filter(|line| line.starts_with(&format!("{local} = "))).collect::<Vec<_>>();
    let never_fails = |line: &str| {
        let Some(local) = line.trim_start().strip_prefix("assert(!").and_then(|rest| rest.split([',', ')']).next()) else { return false };
        if !local.starts_with('_') || !local[1..].chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let found = assignments(local);
        let [definition] = found.as_slice() else { return false };
        let Some(args) = definition.strip_prefix(&format!("{local} = Eq(")).and_then(|rest| rest.strip_suffix(");")) else { return false };
        let value = |part: &str| part.trim().strip_prefix("const ").and_then(|c| c.split_once(" = ")).and_then(|(_, v)| v.trim().parse::<i128>().ok());
        match args.split_once(", ") {
            Some((a, b)) => matches!((value(a), value(b)), (Some(a), Some(b)) if a != b),
            None => false,
        }
    };
    text.lines()
        .filter(|line| line.trim_start().starts_with("assert(") && !never_fails(line))
        .map(|line| kinds.iter().find(|(key, _)| line.contains(key)).map(|(_, name)| name.to_string()).unwrap_or_else(|| "assert".into()))
        .collect()
}

/// The MIR facts of one item (all its bodies, with its closures).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemFacts {
    /// The items that the bodies call.
    pub callees: Vec<String>,
    pub externals: Vec<String>,
    /// The implicit panic sites (MIR `Assert` terminators) of the bodies.
    pub panics: Vec<String>,
    /// The calls whose target the MIR does not give (A3).
    pub indirect: Vec<String>,
    /// The enums whose discriminant value the bodies read with a cast (`as`).
    pub casts: Vec<String>,
    /// The local types whose layout or discriminant values the MIR texts show.
    pub layouts: Vec<String>,
    /// See [`BodyFacts`] for the fields below (batch 5f, A1).
    pub origins: BTreeMap<String, (String, String)>,
    pub reach_types: BTreeSet<(String, String)>,
    pub drop_types: BTreeSet<(String, String)>,
    /// The call for each type that no body of the item drops itself.
    pub drop_calls: BTreeMap<String, String>,
    /// The types that a body of the item drops itself.
    pub drop_real: BTreeSet<String>,
    pub impl_of: BTreeSet<String>,
    pub drop_of: BTreeSet<String>,
    pub param_types: BTreeSet<(String, String)>,
    pub const_types: BTreeSet<(String, String)>,
    /// True if the MIR signature of the item (not of a closure in it) changed in the step.
    pub signature_changed: bool,
    /// The bodies of the item that have a list of live callees: the body name, the target, and
    /// the list (see [`BodyFacts::live`]).
    pub live: Vec<(String, String, Vec<String>)>,
    /// The bodies of the item that have a list of sure callees, as `live` (see
    /// [`BodyFacts::sure`]).
    pub sure: Vec<(String, String, Vec<String>)>,
    /// The end facts of the bodies of `sure`, in the same order (see [`BodyFacts::ends`]).
    pub ends: Vec<BTreeMap<String, Vec<String>>>,
}

/// The access to the normal forms. The real one runs the MIR driver on both snapshots.
pub trait BodyOracle {
    /// Compares all bodies of the two snapshots of a step.
    ///
    /// `renames` maps item paths (before to after) from the identity claims. `added` lists the
    /// new bodies that claims declare, for example the new function of `extract-function`.
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> anyhow::Result<Vec<BodyComparison>>;

    /// `compare` with the opaque inputs of the source text (batch 5l). An oracle with normal
    /// forms that look into a different body must ask `opaque` first.
    fn compare_with(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String], _opaque: &dyn OpaqueInputs) -> anyhow::Result<Vec<BodyComparison>> {
        self.compare(step, renames, added)
    }

    /// The changed files of the step that are build code by the dependency graph (design 28): a
    /// file of a host package (a proc macro crate, or a crate that a proc macro crate or a build
    /// script uses). Each entry is the path and the cause. The syntax does not show this role.
    fn build_code_files(&self, _step: &Step) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The checked configurations of the tree before the step and of the tree after it (batch
    /// 5s), with the features and the profile of each package. Empty: no body check.
    fn truths(&self, _step: &Step) -> anyhow::Result<[Vec<ConfigTruth>; 2]> {
        Ok(Default::default())
    }

    /// The def path of each `#[test]` function in the tree before the step and in the tree
    /// after it (batch 6a, B2). `None`: the oracle does not know the tests.
    fn tests(&self, _step: &Step) -> anyhow::Result<Option<[BTreeSet<String>; 2]>> {
        Ok(None)
    }

    /// The expanded code of the tree before the step and of the tree after it (batch 6b, part
    /// 2): each item after the macro expansion, and the macro call site of each body from a
    /// macro. `None`: the oracle cannot list the items.
    fn made(&self, _step: &Step) -> anyhow::Result<Option<[made::Made; 2]>> {
        Ok(None)
    }

    /// The definitions of the workspace before the step and after it (design 44): each
    /// definition with the symbols that it names. Empty: no data.
    fn defs(&self, _step: &Step) -> anyhow::Result<[DefMap; 2]> {
        Ok(Default::default())
    }

    /// The MIR facts of the bodies after a step. `None` when the MIR is not available (phase 1).
    fn facts(&self, _step: &Step) -> anyhow::Result<Option<Vec<BodyFacts>>> {
        Ok(None)
    }

    /// Sets the feature selections that the oracle adds to the checked configurations of a
    /// step (batch 5u, part C). The list replaces the list of an earlier call. Each later
    /// call for the step uses the configurations of the list also. False: the oracle adds no
    /// configuration.
    fn set_selections(&self, _step: &Step, _selections: &[Selection]) -> anyhow::Result<bool> {
        Ok(false)
    }

    /// The equivalence check of two versions of each body (design 61, part C): one answer for
    /// each query, `None` if the oracle cannot check it. `renames` is the rename map of the step
    /// (old path to new path).
    /// The check of the claims of the intent files of a step (design 67): one answer for each
    /// query. `None`: the oracle has no compiler data (the default mode, or phase 1).
    fn intent(&self, _step: &Step, _renames: &BTreeMap<String, String>, _queries: &[intent::Query]) -> Option<Vec<intent::Answer>> {
        None
    }

    fn equivalence(&self, _step: &Step, _renames: &BTreeMap<String, String>, queries: &[equiv::Query]) -> Vec<Option<equiv::Answer>> {
        vec![None; queries.len()]
    }

    /// The time budget of the equivalence check of a step in seconds (design 61.14, R61.14-6),
    /// for the line of a check that the budget stopped. `None`: no budget.
    fn equivalence_budget(&self, _step: &Step) -> Option<u64> {
        None
    }

    /// The changed instances of the generic bodies and the changed candidates of the `dyn`
    /// calls (design 61, part B). Empty: no data.
    fn instances(&self, _step: &Step, _renames: &BTreeMap<String, String>) -> Vec<equiv::InstanceLine> {
        Vec::new()
    }

    /// The walk of the reverse call graph from the changed bodies of the step (design 61.10,
    /// part A): each reached caller with its result in each configuration. `None`: no data.
    fn reach(&self, _step: &Step, _renames: &BTreeMap<String, String>) -> Option<equiv::ReachReport> {
        None
    }

    /// The expansion of each macro call in each compiled target of each checked configuration
    /// of the tree before the step and of the tree after it (design 63). `None`: no data. A
    /// step of the default mode has the records of its build check (R65.8-1).
    fn expansions(&self, _step: &Step) -> Option<[Vec<expand::ExpansionUnit>; 2]> {
        None
    }

    /// Why a step of the default mode has no expansion records (R65.8-2): the build check did
    /// not run, or it could not write the records. `None`: the step has the records, or it is a
    /// step of the compiled mode.
    fn expansion_gap(&self, _step: &Step) -> Option<String> {
        None
    }

    /// The name records of the two trees of the step (design 70, R70.1-1): the records of the
    /// units of the expansion records. A step with no record gets the cause (R70.1-1); an oracle
    /// with no data (a virtual crate of an expansion compare) gets neither.
    fn names(&self, step: &Step) -> NameState {
        let Some([old, new]) = self.expansions(step) else {
            return match self.expansion_gap(step) {
                Some(cause) => NameState::Gap(cause),
                None => NameState::None,
            };
        };
        if !old.iter().chain(&new).any(|unit| unit.names.is_some()) {
            return NameState::Gap("the records of the compiler have no name record (a record of an earlier driver)".to_string());
        }
        let index = |units: &[expand::ExpansionUnit]| std::sync::Arc::new(semilla_core::namerec::NameIndex::new(units.iter().filter_map(|unit| unit.names.as_deref().map(|record| (unit.target.as_str(), record)))));
        NameState::Records([index(&old), index(&new)])
    }
}

/// The name records of a step (design 70).
pub enum NameState {
    /// The index of the records of each tree: before the step, after the step.
    Records([std::sync::Arc<semilla_core::namerec::NameIndex>; 2]),
    /// The step has no record: the cause. The facts take the names from the text (R70.1-1).
    Gap(String),
    /// No data and no cause (an oracle of a test, a virtual crate).
    None,
}

/// A feature selection that Semilla adds for a proof (batch 5u, part C): a build with these
/// features is a checked configuration of the step, in the two profiles.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Selection {
    /// True: the default features are off (`--no-default-features`).
    pub no_default: bool,
    /// The features of the list (`--features a,b`), in order.
    pub features: Vec<String>,
}

/// The highest number of feature selections that Semilla adds for the proofs of one step.
pub const MOST_SELECTIONS: usize = 4;

/// One name of the expanded code of a body, by the compiler (batch 5p): an identifier after the
/// macro expansion, with its form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpandedName {
    pub word: String,
    /// After `.` or `::`.
    pub member: bool,
    /// The segment before `::`. `$`: a module of a crate outside the workspace. `>`: a type
    /// that is not a path. `.`: a field of a struct expression or a struct pattern.
    pub qual: Option<String>,
    /// Before `::`.
    pub prefix: bool,
    /// The name of the outermost macro call that gives the name.
    pub mac: Option<String>,
    /// The line of the name, or of the outermost macro call.
    pub line: usize,
    pub count: usize,
    /// A method call or a path `Type::name` with a rival (batch 5q): a different item with
    /// the name wins if a trait fact changes. The kinds of the facts, then `|`, then the
    /// message.
    pub rival: Option<String>,
}

/// The inputs of a proof that the build configuration can select, by the syntax tree of the
/// two snapshots of a step (batch 5l). The default answer is "opaque".
pub trait OpaqueInputs {
    /// Keeps the inputs of one body (the `types` list of the driver). The caller gives each
    /// body of the two trees before the first call of `callee` (batch 5o).
    fn note(&self, _after: bool, _name: &str, _span: Option<&(String, usize)>, _types: &[String]) {}

    /// Keeps the names of the expanded code of one body (the `names` list of the driver). The
    /// caller gives each body of the two trees before the first call of `callee` (batch 5p).
    fn names(&self, _after: bool, _name: &str, _span: Option<&(String, usize)>, _names: &[ExpandedName]) {}

    /// True if no normal form can look into the body `name` of the tree before or after the
    /// step. `span` is the file and the line of the body. `types` is the list of the types
    /// and the resolved items that the body uses.
    fn callee(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) -> bool;

    /// True if an entry of the list stops the proof of the body `name`: a type with a layout,
    /// a field set, or an `impl` that the build configuration selects, or an item under
    /// `cfg`.
    fn layout(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) -> bool;

    /// Keeps the unit of one body record (batch 5t, part B): the checked configuration `config`
    /// (its index in the list of `BodyOracle::truths`) has a record of the body `name` in the
    /// unit `target`. The caller gives each N0 record of the two trees.
    fn body(&self, _after: bool, _name: &str, _span: Option<&(String, usize)>, _config: usize, _target: &str) {}
}

/// The result of an SMT check on a condition.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectCheck {
    pub effect: Effect,
    pub witness: Option<String>,
}

/// The access to the SMT checks.
pub trait ExprOracle {
    /// Classifies the change of a condition. `types` maps variable names to Rust type text.
    fn classify(&self, before: &str, after: &str, types: &BTreeMap<String, String>) -> Option<EffectCheck>;
}

/// An oracle that proves nothing. Every check gives "not proved".
pub struct NoOracle;

impl BodyOracle for NoOracle {
    fn compare(&self, _: &Step, _: &BTreeMap<String, String>, _: &[String]) -> anyhow::Result<Vec<BodyComparison>> {
        Ok(Vec::new())
    }
}

impl ExprOracle for NoOracle {
    fn classify(&self, _: &str, _: &str, _: &BTreeMap<String, String>) -> Option<EffectCheck> {
        None
    }
}

/// The inputs of one step check. The two workspaces can live longer than the step: a check of
/// a changed step (the rename search) uses the workspaces of the check (R61.15-2).
pub struct StepInput<'s, 'w> {
    pub step: &'s Step,
    pub before: &'w Workspace,
    pub after: &'w Workspace,
}

/// Everything the verifier computed for one step. The views use it too.
#[derive(Clone)]
pub struct StepOutcome {
    pub report: StepReport,
    pub edits: Vec<Edit>,
    /// The index of the claim that explains each edit, or `None`.
    pub owners: Vec<Option<usize>>,
    /// The index of the fact (in `report.facts`) that owns each edit, or `None`. An edit with no
    /// claim and no fact is uncovered.
    pub fact_of: Vec<Option<usize>>,
    pub renames: semilla_syntax::Renames,
    pub bodies: Vec<BodyComparison>,
    /// For a derived identity: the derived bodies of other traits than `Debug` (serde and others).
    /// They put the identity fact in the spotlight.
    pub identity_reason: Option<String>,
    /// The cause for each stated rename or move that is no fact (design 54.11), by the index
    /// of its `same` option.
    pub stated_causes: BTreeMap<usize, String>,
    /// The feature selections that the check added for a proof (batch 5u, part C).
    pub selections: Vec<Selection>,
    /// The node pairs of the tree compare of each body, by the path of the old item (design
    /// 59, B-d).
    pub node_pairs: BTreeMap<String, Vec<semilla_syntax::NodePair>>,
}

/// Checks one step with the old protocol: an edit with no claim and no type-level fact is
/// uncovered.
pub fn verify_step(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle) -> StepOutcome {
    verify_step_with(input, bodies, exprs, false)
}

/// Checks one step. With `derive_edits` (design 24.2), each edit with no claim gets a derived fact,
/// so no edit is uncovered, and a step with no identity claim can get a derived identity.
pub fn verify_step_with(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, derive_edits: bool) -> StepOutcome {
    // Design 70: the name records of the two trees. Each rule that needs a name, an identity,
    // a visibility, a `cfg` state, or an `unsafe` state reads them from the workspace.
    let names_gap = match bodies.names(input.step) {
        NameState::Records([old, new]) => {
            input.before.names.set(Some(old));
            input.after.names.set(Some(new));
            None
        }
        NameState::Gap(cause) => {
            input.before.names.set(None);
            input.after.names.set(None);
            Some(cause)
        }
        // No data and no cause: the workspaces keep their state (a virtual crate has none).
        NameState::None => None,
    };
    let mut outcome = verify_step_records(input, bodies, exprs, derive_edits);
    outcome.report.names_gap = names_gap;
    outcome
}

/// [`verify_step_with`] after the name records are in the two workspaces.
fn verify_step_records(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, derive_edits: bool) -> StepOutcome {
    // Design 38.3: an item that gets a twin keeps its key for the twin with its text.
    let twins = identity::twin_keys(input.before, input.after);
    let input = match &twins {
        Some((before, after)) => &StepInput { step: input.step, before, after },
        None => input,
    };
    // Design 59, part C: the twins of one path pair by their tokens, then in source order.
    let twin_pairs = identity::twin_pairs(input.before, input.after);
    let (input, twin_moves) = match &twin_pairs {
        Some((before, after, moved)) => (&StepInput { step: input.step, before, after }, moved.as_slice()),
        None => (input, &[][..]),
    };
    // R70.2-1 (d): an impl keeps its key when the text of its trait path changes and the compiler
    // gives the same trait and self type.
    let trait_pairs = identity::trait_path_pairs(input.before, input.after);
    let input = match &trait_pairs {
        Some((before, after)) => &StepInput { step: input.step, before, after },
        None => input,
    };
    // A reason with no text is not a reason (brief 5f, A4: an empty value proves nothing).
    let mut step = input.step.clone();
    step.reasons.retain(|_, reason| !reason.trim().is_empty());
    // Design 59: the name of a macro call with a path has no space (`depot::crate::table!#0`).
    // A record of an earlier version has the key with spaces (`depot::crate :: table!#0`).
    step.reasons = std::mem::take(&mut step.reasons).into_iter().map(|(key, reason)| (macro_call_key(&key), reason)).collect();
    for change in &mut step.agent.change {
        if let Some((items, text)) = change.split_once('=').filter(|(items, _)| items.contains(" :: ")) {
            *change = format!("{}={text}", items.split(',').map(macro_call_key).collect::<Vec<_>>().join(","));
        }
    }
    // Design 67: a claim of the forms of `--same` in an intent file is a `same` fact.
    for text in intent::same_facts(&step.agent.intent) {
        if !step.agent.same.contains(&text) {
            step.agent.same.push(text);
        }
    }
    for text in intent::change_facts(&step.agent.intent) {
        if !step.agent.change.contains(&text) {
            step.agent.change.push(text);
        }
    }
    impl_names(&mut step, input.before, input.after);
    let input = &StepInput { step: &step, before: input.before, after: input.after };
    // The opaque inputs of each check of this step, also of the rename search (R61.15-2).
    let bases = joint::OpaqueBases::new(input.before, input.after);
    let mut plain = verify_core(input, bodies, exprs, derive_edits, false, &bases);
    if !derive_edits || input.step.claims.iter().any(is_identity) {
        if derive_edits {
            identity_api_facts(input, &mut plain);
        }
        item_reasons(input, &mut plain.report);
        kind_reasons(input, &mut plain.report);
        no_compare_result(input.step, &mut plain.report);
        positions::assign(input, &mut plain);
        flow::apply(input, bodies, &mut plain);
        expand::apply(input, bodies, exprs, &mut plain);
        macro_module_spotlight(input, &mut plain.report);
        twin_notes(twin_moves, &mut plain.report);
        agent::apply(input, &mut plain);
        intent::apply(input, bodies, &mut plain);
        tags::apply(input, &mut plain);
        comments::apply(input, &mut plain);
        return plain;
    }
    let mut outcome = identity::derive(input, bodies, exprs, plain, &bases);
    // The search for an identity does more checks, and each check sets its own list of added
    // configurations. The oracle must have the list of this result: the output shows it.
    let _ = bodies.set_selections(input.step, &outcome.selections);
    identity_api_facts(input, &mut outcome);
    item_reasons(input, &mut outcome.report);
    kind_reasons(input, &mut outcome.report);
    no_compare_result(input.step, &mut outcome.report);
    positions::assign(input, &mut outcome);
    flow::apply(input, bodies, &mut outcome);
    expand::apply(input, bodies, exprs, &mut outcome);
    macro_module_spotlight(input, &mut outcome.report);
    twin_notes(twin_moves, &mut outcome.report);
    agent::apply(input, &mut outcome);
    intent::apply(input, bodies, &mut outcome);
    tags::apply(input, &mut outcome);
    comments::apply(input, &mut outcome);
    outcome
}

/// The key of an earlier version for a macro call with a path (`depot::crate :: table!#0`), in
/// the form of today (`depot::crate::table!#0`). Each other key stays as it is.
pub(crate) fn macro_call_key(key: &str) -> String {
    if key.contains(" :: ") && key.contains('!') {
        key.replace(" :: ", "::")
    } else {
        key.to_string()
    }
}

/// Design 59, A-d: a module whose `mod` line is in the input of a macro call. Semilla does
/// not expand the macro, so it does not know if the build has the module. Each such module
/// with a fact (an item of the module, or a line of its file) gets one spotlight line. The
/// line is not a reject: it needs no reason.
fn macro_module_spotlight(input: &StepInput, report: &mut StepReport) {
    let mut modules: BTreeMap<&String, &semilla_syntax::MacroModule> = BTreeMap::new();
    for workspace in [input.before, input.after] {
        for (path, module) in &workspace.macro_modules {
            modules.entry(path).or_insert(module);
        }
    }
    for (path, module) in modules {
        let inside = format!("{path}::");
        let has_fact = report.facts.iter().any(|fact| {
            fact.at.as_ref().is_some_and(|at| at.item == *path || at.item.starts_with(&inside)) || fact.positions.iter().any(|position| position.file == module.file) || fact.file.as_ref() == Some(&module.file)
        });
        if !has_fact {
            continue;
        }
        let line = format!(
            "module of a macro input: `{path}` (`{}`) has its line `{}` in the input of the macro call `{}` (`{}:{}`); Semilla does not expand the macro, so it does not know if the build has this module",
            module.file, module.text, module.call, module.at.0, module.at.1
        );
        if !report.spotlight.contains(&line) {
            report.spotlight.push(line);
        }
    }
}

/// Design 59, C-e: a pair of twins with two different numbers has one key in the facts (the
/// number of the tree with more twins of the path). Each such pair with a fact gets one note
/// with the two paths, so the reader finds each twin in its own tree.
fn twin_notes(moves: &[(String, String, String)], report: &mut StepReport) {
    for (old, new, key) in moves {
        let inside = |path: &str| path == key || path.strip_prefix(key.as_str()).is_some_and(|rest| rest.starts_with("::") || rest.starts_with('.'));
        if !report.facts.iter().any(|fact| fact.at.as_ref().is_some_and(|at| inside(&at.item))) {
            continue;
        }
        report.notes.push(format!("twins: `{old}` of the old tree and `{new}` of the new tree are one pair (equal tokens first, then the `cfg` condition, then the source order); the facts name the pair `{key}`"));
    }
}

/// A step with no compiled check (design 50): no fact has a compare result. This is the last
/// pass, so a fact that a later rule made (a derived rename, a folded fact) has none too.
fn no_compare_result(step: &Step, report: &mut StepReport) {
    if !step.compiled {
        for fact in &mut report.facts {
            fact.compare = Compare::Off;
            // No note gives a cause for a compare that Semilla did not make.
            if fact.note.as_deref().is_some_and(|note| note.starts_with(uncompiled::NO_COMPARE)) {
                fact.note = None;
            }
        }
    }
}

/// The parts of the name of an item of a trait impl (`m::<Type as Trait<A>>::method#0`,
/// `m::impl <Type as Trait>`): the text before `<`, the type, the trait path, the generic
/// arguments of the trait, and the text after `>`.
fn impl_name_parts(name: &str) -> Option<(&str, &str, &str, Option<&str>, &str)> {
    for (start, _) in name.match_indices('<') {
        if !(start == 0 || name[..start].ends_with("::") || name[..start].ends_with("impl ")) {
            continue;
        }
        let (mut depth, mut end, mut split) = (0usize, None, None);
        for (index, c) in name[start..].char_indices() {
            match c {
                '<' => depth += 1,
                '>' if name[..start + index].ends_with('-') => {}
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(start + index);
                        break;
                    }
                }
                ' ' if depth == 1 && split.is_none() && name[start + index..].starts_with(" as ") => split = Some(start + index),
                _ => {}
            }
        }
        let (Some(end), Some(split)) = (end, split) else { continue };
        let trait_text = &name[split + 4..end];
        let (path, arguments) = match trait_text.find('<') {
            Some(at) if trait_text.ends_with('>') => (&trait_text[..at], Some(&trait_text[at + 1..trait_text.len() - 1])),
            _ => (trait_text, None),
        };
        return Some((&name[..start], &name[start + 1..split], path, arguments, &name[end + 1..]));
    }
    None
}

/// The generic arguments of the trait in the header of a trait impl (`DecodeError` for
/// `impl From<DecodeError> for std::io::Error`), with no space.
pub(crate) fn trait_arguments(item: &semilla_syntax::Item) -> Option<String> {
    let (_, _, path, _, _) = impl_name_parts(&item.path)?;
    let last = path.rsplit("::").next()?;
    let header = item.signature.split(" for ").next()?.trim_end();
    // The last full word `last` of the header that has `<` after it.
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let start = header.rmatch_indices(last).map(|(at, _)| at).find(|at| !header[..*at].ends_with(word) && header[at + last.len()..].trim_start().starts_with('<'))?;
    let arguments = header[start + last.len()..].trim_start().strip_prefix('<')?.strip_suffix('>')?;
    Some(arguments.chars().filter(|c| !c.is_whitespace()).collect())
}

/// The self type in the header of an impl as the source has it (batch 6a, A5):
/// `AsKebabCase<&str>` for `impl Render for AsKebabCase<&str>`. `None` if the header does not
/// parse.
pub(crate) fn self_type_text(item: &semilla_syntax::Item) -> Option<String> {
    let Some(semilla_syntax::rs::ast::Item::Impl(parsed)) = semilla_syntax::rs::parse_item(&item.signature) else { return None };
    let mut text = semilla_syntax::rs::compare(&parsed.self_ty()?);
    for (from, to) in [(" :: ", "::"), (":: ", "::"), (" < ", "<"), ("< ", "<"), (" >", ">"), ("& ", "&"), (" ,", ","), ("( ", "("), (" )", ")"), ("[ ", "["), (" ]", "]"), (" ;", ";")] {
        text = text.replace(from, to);
    }
    Some(text)
}

/// A type text with no lifetime (design 37.2): `&'a mut Node` gives `&mut Node`, and
/// `dyn Parse + 'a` gives `dyn Parse`. The owner of an impl in an item path has no lifetime.
fn no_lifetimes(text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\'') {
        let name = rest[at + 1..].find(|c: char| !word(c)).unwrap_or(rest.len() - at - 1);
        let after = &rest[at + 1 + name..];
        if name == 0 || after.starts_with('\'') {
            // A character literal, not a lifetime.
            out.push_str(&rest[..at + 1]);
            rest = &rest[at + 1..];
            continue;
        }
        // A bound (`+ 'a`) goes away with its `+`.
        let before = rest[..at].trim_end();
        out.push_str(before.strip_suffix('+').map_or(&rest[..at], str::trim_end));
        rest = after.strip_prefix(' ').unwrap_or(after);
        // An argument of a list goes away with its comma. An empty list goes away.
        if let Some(next) = rest.strip_prefix(',') {
            rest = next.trim_start();
        } else if rest.starts_with('>') {
            let kept = out.trim_end().strip_suffix(',').map(str::len);
            out.truncate(kept.unwrap_or(out.len()));
            if let Some(kept) = out.strip_suffix("::<").or_else(|| out.strip_suffix('<')).map(str::len) {
                out.truncate(kept);
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Batch 5y, B1 (trial 8): the items of a trait impl that the name `name` can mean. An agent
/// writes the name from the source: a full trait path (`core::convert::From`) and the generic
/// arguments of the trait (`From<DecodeError>`). The item name has the trait path of the source
/// and no arguments, and a twin has `#0`, `#1`. The arguments of `name` select between twins.
pub(crate) fn impl_name_matches(name: &str, workspace: &Workspace) -> Vec<String> {
    let plain = |text: &str| -> String {
        let mut depth = 0usize;
        text.chars().filter(|c| match c {
            '<' => { depth += 1; false }
            '>' => { depth = depth.saturating_sub(1); false }
            c => depth == 0 && !c.is_whitespace(),
        }).collect()
    };
    let key = |name: &str, short: bool| -> Option<String> {
        let (before, self_type, path, _, after) = impl_name_parts(name)?;
        let after = after.rsplit_once('#').filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).map_or(after, |(start, _)| start);
        let self_type = plain(&no_lifetimes(self_type));
        let self_type = if short { self_type.rsplit("::").next().unwrap_or_default().to_string() } else { self_type };
        // A name with no `impl ` word and no text after `>` means the impl block: the text of
        // the fact shows the block as `<Type as Trait<A>>`.
        let before = before.strip_suffix("impl ").unwrap_or(before);
        Some(format!("{before}<{self_type} as {}>{after}", path.rsplit("::").next().unwrap_or(path)))
    };
    let Some((_, _, _, arguments, _)) = impl_name_parts(name) else { return Vec::new() };
    let mut found: Vec<&semilla_syntax::Item> = Vec::new();
    for short in [false, true] {
        let wanted = key(name, short);
        found = workspace.items.values().filter(|item| item.path.contains(" as ") && key(&item.path, short) == wanted).collect();
        if !found.is_empty() {
            break;
        }
    }
    if let (true, Some(arguments)) = (found.len() > 1, arguments) {
        let arguments: String = arguments.chars().filter(|c| !c.is_whitespace()).collect();
        // The impl block of an item: the nearest impl that has the lines of the item.
        let block = |item: &semilla_syntax::Item| -> Option<String> {
            if item.kind == semilla_syntax::ItemKind::Impl {
                return trait_arguments(item);
            }
            workspace.items.values().filter(|other| other.kind == semilla_syntax::ItemKind::Impl && other.file == item.file && other.line_start <= item.line_start && item.line_end <= other.line_end).max_by_key(|other| other.line_start).and_then(trait_arguments)
        };
        let selected: Vec<&semilla_syntax::Item> = found.iter().copied().filter(|item| block(item).as_deref() == Some(arguments.as_str())).collect();
        if !selected.is_empty() {
            found = selected;
        }
    }
    found.into_iter().map(|item| item.path.clone()).collect()
}

/// A list of names for a message: each name in backticks.
fn ticks(names: &[String]) -> String {
    names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
}

/// The text after "no item `NAME` …": the names that `name` can mean (batch 5y, B1).
pub(crate) fn name_hint(name: &str, workspace: &Workspace) -> String {
    let matches = impl_name_matches(name, workspace);
    if matches.len() > 1 {
        return format!(". More than one item matches: {}. Use one of these names, or give the generic arguments of the trait (`<Type as From<Source>>`)", ticks(&matches));
    }
    let last = |path: &str| path.rsplit("::").next().unwrap_or(path).split('#').next().unwrap_or_default().to_string();
    let wanted = last(name);
    let near: Vec<String> = workspace.items.keys().filter(|path| !wanted.is_empty() && last(path) == wanted).take(5).cloned().collect();
    if near.is_empty() {
        return String::new();
    }
    format!(". The nearest names: {}", ticks(&near))
}

/// Batch 5y, B1: puts the item name of the workspace in each claim anchor and each reason key
/// that names an item of a trait impl in a different form. A name with more than one match
/// stays for a claim (the message of the claim lists the names). A reason goes to each match:
/// a reason is text for the reviewer, and it proves nothing.
fn impl_names(step: &mut Step, before: &Workspace, after: &Workspace) {
    let known = |name: &str| after.items.contains_key(name) || before.items.contains_key(name);
    let matches = |name: &str| -> Vec<String> {
        if !name.contains(" as ") || known(name) {
            return Vec::new();
        }
        let found = impl_name_matches(name, after);
        if found.is_empty() { impl_name_matches(name, before) } else { found }
    };
    for claim in &mut step.claims {
        let Some(at) = claim.at.as_mut() else { continue };
        if let [one] = matches(&at.item).as_slice() {
            at.item = one.clone();
        }
    }
    let keys: Vec<String> = step.reasons.keys().filter(|key| key.contains(" as ")).cloned().collect();
    for key in keys {
        // A key can have a part after the item (`ITEM/…`).
        let cut = key.rfind('>').and_then(|at| key[at..].find('/').map(|slash| at + slash)).unwrap_or(key.len());
        let (item, rest) = key.split_at(cut);
        let found = matches(item);
        if found.is_empty() {
            continue;
        }
        let reason = step.reasons.remove(&key).unwrap_or_default();
        for name in found {
            step.reasons.entry(format!("{name}{rest}")).or_insert_with(|| reason.clone());
        }
    }
}

/// True if `key` is the path of an item that a reason can name for each spotlight in it: a
/// function, a method, a constant, a static, a type, or a trait (batch 5x, part D). A module, a
/// crate, an impl block, a field, and a variant are not such a key.
pub fn is_item_reason_key(key: &str, before: &Workspace, after: &Workspace) -> bool {
    use semilla_syntax::ItemKind::{Const, Enum, Fn, Static, Struct, Trait, Type};
    [after, before].iter().any(|ws| ws.items.get(key).is_some_and(|item| matches!(item.kind, Fn | Struct | Enum | Trait | Const | Static | Type)))
}

/// Batch 5x, part D (trial 7 on humantime): a reason whose key is the path of an item covers
/// each spotlight of the step whose key is that path, or starts with that path and `/`. The
/// agent can then give the reason in the first call: it knows the item, not the exact key.
/// The exact key of a fact goes first. A reason proves nothing: it is text for the reviewer.
fn item_reasons(input: &StepInput, report: &mut StepReport) {
    // Batch 5x, part E: a fact with no spotlight gets the reason that has its exact key. The
    // outputs show the reason as a note. A reason proves nothing.
    for fact in report.facts.iter_mut().filter(|fact| !fact.spotlight && fact.spotlight_why.is_none() && fact.reason.is_none()) {
        fact.reason = input.step.reasons.get(&fact.reason_place()).cloned();
    }
    // The key for the command to run again (batch 7b, part 5.5): the path of the item, for a
    // spotlight in a part of an item that a reason can name.
    for fact in report.facts.iter_mut().filter(|fact| fact.spotlight) {
        fact.reason_item = fact.at.as_ref().filter(|at| !at.path.is_empty() && fact.reason_place().starts_with(&format!("{}/", at.item)) && is_item_reason_key(&at.item, input.before, input.after)).map(|at| at.item.clone());
    }
    let keys: Vec<(&String, &String)> = input.step.reasons.iter().filter(|(key, _)| is_item_reason_key(key, input.before, input.after)).collect();
    if keys.is_empty() {
        return;
    }
    for fact in report.facts.iter_mut().filter(|fact| fact.spotlight) {
        let place = fact.reason_place();
        if input.step.reasons.contains_key(&place) {
            continue;
        }
        // The longest path that covers the fact: the nearest item.
        let covers = |key: &str| place.strip_prefix(key).is_some_and(|rest| rest.starts_with('/'));
        let Some((key, reason)) = keys.iter().filter(|(key, _)| covers(key)).max_by_key(|(key, _)| key.len()) else { continue };
        match &fact.reason {
            None => fact.reason = Some((*reason).clone()),
            Some(text) if text == *reason => {}
            Some(_) => continue,
        }
        fact.reason_key = Some((*key).clone());
    }
}

/// A kind reason (design 37.19, Decided by Claude): a reason with the key `kind:NAME` covers
/// each open spotlight of the kind `NAME` in this step. The exact key of a fact and the path
/// of its item go first. A kind reason does not cover a changed file of build code.
/// Each covered fact has the count of the spotlights that the reason covers.
fn kind_reasons(input: &StepInput, report: &mut StepReport) {
    use semilla_core::report::{BUILD_CODE_KIND, KIND_KEY};
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for fact in report.facts.iter_mut().filter(|fact| fact.spotlight && fact.reason.is_none()) {
        let Some(kind) = fact.kind().filter(|kind| *kind != BUILD_CODE_KIND) else { continue };
        let key = format!("{KIND_KEY}{kind}");
        let Some(reason) = input.step.reasons.get(&key) else { continue };
        fact.reason = Some(reason.clone());
        *counts.entry(key.clone()).or_default() += 1;
        fact.reason_key = Some(key);
    }
    for fact in &mut report.facts {
        if let Some(count) = fact.reason_key.as_ref().and_then(|key| counts.get(key)) {
            fact.reason_covers = *count;
        }
    }
}

/// The check of one step. `derived_identity` is true when the identity claims of the step come
/// from the candidate search (design 24.2, rule 4), not from the agent.
pub(crate) fn verify_core<'a>(input: &StepInput<'_, 'a>, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, derive_edits: bool, derived_identity: bool, shared: &joint::OpaqueBases<'a>) -> StepOutcome {
    let step = input.step;
    let oracle = bodies;
    let mut report = StepReport { step: step.index, intent: step.intent.clone(), ..Default::default() };

    // 1. The renames from the identity claims, then the syntax edits.
    let mut renames = checks::renames(step, input.before);
    checks::moved_impls(step, input.before, input.after, &mut renames);
    // The impl blocks of a renamed type or trait (design 37.2).
    report.notes.extend(checks::renamed_impls(step, input.before, input.after, &mut renames));
    // The map of a rename is a map of the references to the item (design section 51).
    sites::resolve(&mut renames, input.before, input.after);
    let renames = renames;
    let (edits, body_maps) = semilla_syntax::diff_maps(input.before, input.after, &renames);

    // 2. The MIR bodies.
    let added: Vec<String> = step.claims.iter().flat_map(checks::declared_new_items).collect();
    // The MIR names the members of a type by their paths (`k::Pair.a`, `k::Kind::Small`), so the
    // rename map of the items also maps the renamed fields and variants (batch 5e).
    // The inputs of a proof that the build configuration selects (batch 5l).
    // The conditions that the checked configurations cover (batch 5s, part C). The rule of
    // the build configuration does not apply to them: the proof in each configuration is the
    // check. An error here comes again from the compare.
    //
    // Batch 5u, part C. A proof can need a feature state that no checked unit has. Then the
    // verifier asks the oracle for a configuration with this state, and it does each check
    // again. Soundness: an added configuration is a checked configuration as each other one.
    // Each proof must hold in it, and the joint check reads the true values of its units
    // from the compiler. The selection is only the way to get the units: a selection that
    // gives a different state adds a check, and the gap stays. The list starts empty in each
    // check, so the result is a function of the two trees and the claims only.
    let mut selections: Vec<Selection> = Vec::new();
    let can_add = bodies.set_selections(step, &[]).unwrap_or(false);
    let mut scope: Option<cfgcond::Scope> = None;
    // The compare result of each body with no rule of the build configuration (design 35): the
    // MIR level N0 after the rename and move map (N1), in the checked configurations.
    let raw: std::cell::RefCell<Vec<(String, BodyStatus)>> = std::cell::RefCell::new(Vec::new());
    // The name of a compiled body has no twin number (design 37.4).
    // Design 38.10: a twin in a file that no checked unit reads gives no compiled body.
    let read_files: Vec<BTreeSet<String>> = bodies.truths(step).unwrap_or_default().iter().map(|side| side.iter().filter(|config| config.known).flat_map(|config| config.units.iter().flat_map(|unit| unit.files.iter().cloned())).collect()).collect();
    let read = |side: usize| read_files.get(side).filter(|files| !files.is_empty());
    let body_renames = checks::body_renames_read(&renames.items, input.before, input.after, [read(0), read(1)]);
    // The opaque inputs of the runs of this check (R61.15-2): those of the step, if they are
    // for the two trees of this input.
    let own;
    let bases = if shared.fits(input.before, input.after) {
        shared
    } else {
        own = joint::OpaqueBases::new(input.before, input.after);
        &own
    };
    let settled = loop {
        let truths = bodies.truths(step).unwrap_or_default();
        let cover = cfgcond::Cover::cached(&[(input.before, &truths[0]), (input.after, &truths[1])]);
        let atoms = cover.atoms();
        match &scope {
            Some(scope) => scope.set(cover),
            None => scope = Some(cfgcond::enter(cover)),
        }
        // One run of the body check with a part of the rule on again (`cfgcond::Mode`).
        let run = |mode: &cfgcond::Mode| -> anyhow::Result<(Vec<BodyComparison>, joint::Bodies)> {
            cfgcond::with_mode(mode, || {
                let opaque = joint::Recorder::new(&bases);
                let mut bodies = bodies.compare_with(step, &body_renames, &added, &opaque)?;
                if matches!(mode, cfgcond::Mode::Plain) {
                    *raw.borrow_mut() = bodies.iter().map(|body| (body.name.clone(), body.status.clone())).collect();
                }
                // A call `cfg!(P)` with a covered condition: its value is opaque in each proof, and
                // each checked configuration has a proof.
                for body in bodies.iter_mut() {
                    // A `cfg!` call has the value of the unit that expands it: the file of the body.
                    let _at = body.span.as_ref().map(|(file, _)| cfgcond::at(file));
                    body.config.retain(|text| !cfgcond::covered_call(text));
                }
                // The code that a `cfg` attribute selects is not in the MIR (batch 5k, A1).
                srcconfig::apply(&mut bodies, input.before, input.after, &renames.items);
                // With no body, the opaque inputs have no use (batch 9b3, G6).
                if !bodies.is_empty() {
                    opaque.opaque().apply(&mut bodies, &renames.items);
                }
                Ok((bodies, opaque.bodies()))
            })
        };
        // The rule goes off for a proof only if the checked units have each valuation of the
        // atoms of the proof (batch 5t, part B).
        let settled = run(&cfgcond::Mode::Plain).and_then(|(plain, seen)| {
            let joint = joint::Joint { sides: [input.before, input.after], truths: &truths, bodies: &seen, renames: &renames.items };
            let property = step.claims.iter().any(|claim| semilla_core::ops::op(&claim.op).is_some_and(|info| info.kind == semilla_core::OpKind::Property));
            // The joint check stays for each step (design 35.5): an equal compare that uses
            // two or more conditions gets the configurations of the joint states (batch 5u).
            // The added configurations are checked configurations. A state that Semilla
            // cannot add gives a note under the fact.
            let kept = raw.borrow().clone();
            let result = joint.settle(plain, &atoms, property, &selections, &|mode| run(mode).map(|(bodies, _)| bodies));
            *raw.borrow_mut() = kept;
            result
        });
        let (bodies_now, wanted) = match settled {
            Ok(result) => result,
            Err(error) => break Err(error),
        };
        // The new selections, in the order of the bodies and of their states, to the limit.
        let mut next = selections.clone();
        next.extend(wanted.into_iter().take(MOST_SELECTIONS.saturating_sub(selections.len())));
        if !can_add || next.len() == selections.len() {
            break Ok(bodies_now);
        }
        match bodies.set_selections(step, &next) {
            Ok(true) => selections = next,
            Ok(false) => break Ok(bodies_now),
            Err(error) => break Err(error),
        }
    };
    let mut build_failed = false;
    let mut bodies = match settled {
        Ok(bodies) => bodies,
        Err(error) => {
            report.errors.push(format!("the body check failed: {error:#}"));
            build_failed = true;
            Vec::new()
        }
    };
    // The rules of the build configuration change no compare result (design 35). Each body gets
    // its raw status again. The data of the rules (`config`, `stopped`) stays for the notes.
    // An equal result above N1 is not a result of this compare.
    {
        let raw = raw.borrow();
        let same = raw.len() == bodies.len() && raw.iter().zip(&bodies).all(|((name, _), body)| *name == body.name);
        for (index, body) in bodies.iter_mut().enumerate() {
            let status = if same { Some(&raw[index].1) } else { raw.iter().find(|(name, _)| *name == body.name).map(|(_, status)| status) };
            if let Some(status) = status {
                body.status = status.clone();
            }
            if matches!(&body.status, BodyStatus::Equal { level, .. } if level != "n0" && level != "n1") {
                body.status = BodyStatus::Different { cfg_changed: false };
            }
        }
    }
    let body_index = checks::BodyIndex::new(&bodies, input.after, input.before);

    // The data of the property claims: the local calls (syntax), and the MIR facts (phase 2).
    let properties: Vec<&semilla_core::Claim> = step.claims.iter().filter(|c| semilla_core::ops::op(&c.op).is_some_and(|i| i.kind == semilla_core::OpKind::Property)).collect();
    let mut calls: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if !properties.is_empty() {
        let graph = semilla_syntax::graph::workspace_graph(input.after, &BTreeMap::new(), &BTreeMap::new());
        for edge in graph.edges.iter().filter(|e| e.kind == semilla_core::view::GraphEdgeKind::Call) {
            calls.entry(edge.from.clone()).or_default().insert(edge.to.clone());
        }
    }
    let mut facts: Option<BTreeMap<String, ItemFacts>> = None;
    if properties.iter().any(|c| matches!(c.op.as_str(), "no-io" | "calls-only" | "no-panic" | "no-unsafe" | "tested-by")) {
        match oracle.facts(step) {
            Ok(Some(found)) => facts = Some(item_facts(&found, input.after)),
            Ok(None) => {}
            // The same build serves `compare` and `facts`. A failed build gives both the same
            // error; the report shows it one time (Part B item 4 of batch 5j).
            Err(error) => {
                let text = format!("the MIR facts failed: {error:#}");
                let build_failed = |e: &String| e.contains("cargo check failed") || e.contains("cargo cannot get a dependency") || e.contains("the body check failed");
                if !(build_failed(&text) && report.errors.iter().any(build_failed)) {
                    report.errors.push(text);
                }
            }
        }
    }

    // 3. Each claim.
    let context = checks::Context { step, before: input.before, after: input.after, edits: &edits, bodies: &body_index, exprs, renames: &renames, calls: &calls, facts: facts.as_ref() };
    let mut owners: Vec<Option<usize>> = vec![None; edits.len()];
    let mut results = Vec::new();
    for (index, claim) in step.claims.iter().enumerate() {
        let outcome = checks::check(&context, claim);
        for &edit in &outcome.explains {
            if owners[edit].is_none() {
                owners[edit] = Some(index);
            }
        }
        results.push((claim, outcome));
    }

    // An identity step (design 22.4): a step with a `rename` or a `move-item` claim must be neutral.
    let mut identity_derived: BTreeSet<String> = BTreeSet::new();
    let mut identity_reason = None;
    let identity_status = identity_step(step, &results, &edits, &mut owners, &bodies, &body_index, &renames, input.before, input.after, &mut identity_derived, derived_identity.then_some(&mut identity_reason));

    for (claim, outcome) in &results {
        let mut status = outcome.status.clone();
        let mut hint = outcome.hint.clone();
        if let Some((identity, identity_hint)) = identity_status.as_ref().filter(|_| is_identity(claim) && outcome.status.is_verified()) {
            status = identity.clone();
            hint = identity_hint.clone();
        }
        if status.is_verified() && outcome.explains.is_empty() && !outcome.self_evident {
            status = ClaimStatus::Empty;
        }
        report.claims.push(ClaimResult { build_code: None,
            claim: claim.id.clone(),
            step: step.index,
            status,
            category: outcome.category,
            effect: outcome.effect,
            witness: outcome.witness.clone(),
            hint,
            before: outcome.before.clone(),
            after: outcome.after.clone(),
            affected: Vec::new(),
            complete_calls: outcome.complete_calls.clone(),
        });
    }

    // An empty claim near an uncovered change: tell the agent where the change is.
    for result in report.claims.iter_mut().filter(|r| r.status == ClaimStatus::Empty) {
        let Some(at) = step.claims.iter().find(|c| c.id == result.claim).and_then(|c| c.at.clone()) else { continue };
        let nearest = edits
            .iter()
            .zip(&owners)
            .filter(|(_, owner)| owner.is_none())
            .filter_map(|(edit, _)| edit.before.clone().or(edit.after.clone()))
            .filter(|anchor| anchor.item == at.item)
            .min_by_key(|anchor| {
                let common = anchor.path.iter().zip(&at.path).take_while(|(a, b)| a == b).count();
                (anchor.path.len() + at.path.len()) - 2 * common
            });
        if let Some(anchor) = nearest {
            // The change is at the anchor of the claim, but it has another shape (D11).
            result.hint = Some(if anchor == at {
                format!("The change at `{anchor}` is not a change of this claim. Remove the claim: Semilla derives a fact for the change.")
            } else {
                format!("The change is at `{anchor}`: move the claim there.")
            });
        }
    }

    // The facts: the type-level changes that no claim explains (design 22.5).
    // The MIR callee lists are the evidence for the call sites and the affected bodies (F1, F5).
    let mut call_facts = facts.clone();
    if call_facts.is_none() && body_index.available {
        match oracle.facts(step) {
            Ok(Some(found)) => call_facts = Some(item_facts(&found, input.after)),
            Ok(None) => {}
            Err(error) => report.errors.push(format!("the MIR facts failed: {error:#}")),
        }
    }
    let derived = facts::derive(step, &edits, &owners, input.before, input.after, &bodies, &body_index, call_facts.as_ref(), &renames, derive_edits);
    report.facts = derived.facts;
    let mut fact_of = derived.fact_of;
    if derive_edits {
        edits::derive(&context, &owners, &mut fact_of, &mut report.facts);
    }

    // A changed file of a host package (design 28, A4.5). The compiler runs this code during
    // each build, as it runs a build script. The file gets a fact with the spotlight "build
    // code", so it needs a reason. A file fact with a spotlight has the role already.
    for (file, why) in oracle.build_code_files(step) {
        let place = format!("{file}:");
        if report.facts.iter().any(|fact| fact.spotlight && fact.part.starts_with(&place)) {
            continue;
        }
        let text = |ws: &Workspace| ws.other_files.get(&file).or_else(|| ws.rust_files.get(&file)).cloned();
        let (old, new) = (text(input.before), text(input.after));
        // The oracle gives the files against the base tree. Only a file that this step changes
        // gets the fact.
        let entry = |ws: &Workspace| ws.files.get(&file).map(|entry| entry.hash.clone());
        if entry(input.before) == entry(input.after) && old == new {
            continue;
        }
        let what = match (&old, &new) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            _ => "changed",
        };
        report.facts.push(semilla_core::report::Fact {
            id: format!("f{}", report.facts.len() + 1),
            file: Some(file.clone()),
            part: place.clone(),
            description: format!("{file}: {what}"),
            status: "derived".into(),
            category: Category::BuildCode,
            spotlight: true,
            spotlight_why: Some(format!("build code: this file is {why}. The compiler runs this code during each build, and it can change the code and the result of the build")),
            reason: step.reasons.get(&place).cloned(),
            content: Some(files::content_change(old.as_deref(), new.as_deref(), None, None)),
            ..Default::default()
        });
    }

    // The attributes of a new item (brief 5h, B2). A new item follows the attribute allow list of
    // a changed item, and `#[test]` and `#[cfg(test)]` are also allowed. Each other attribute
    // puts the fact of the new item in the spotlight, so it needs a reason. A new item that a
    // claim owns, or that has no fact, gets a fact of its own.
    let mut attribute_facts: BTreeMap<Option<usize>, (String, Option<String>, Vec<String>)> = BTreeMap::new();
    for (index, edit) in edits.iter().enumerate().filter(|(_, edit)| edit.kind == semilla_syntax::EditKind::ItemAdded) {
        let Some(item) = edit.after.as_ref().and_then(|anchor| input.after.items.get(&anchor.item)) else { continue };
        let names = facts::new_item_attributes(item);
        if names.is_empty() {
            continue;
        }
        let text = format!("{} on `{}`", names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", "), item.path);
        let entry = attribute_facts.entry(fact_of[index]).or_insert_with(|| (item.path.clone(), edit.file.clone(), Vec::new()));
        entry.2.push(text);
    }
    for (fact, (item, file, texts)) in attribute_facts {
        let why = format!("a new item with an attribute that can change the behavior: {}", texts.join("; "));
        match fact.and_then(|index| report.facts.get_mut(index)) {
            Some(fact) => {
                fact.spotlight = true;
                fact.spotlight_why = Some(match fact.spotlight_why.take() {
                    Some(old) => format!("{old}; {why}"),
                    None => why,
                });
                if fact.reason.is_none() {
                    fact.reason = fact.at.as_ref().and_then(|at| step.reasons.get(&at.to_string()).cloned()).or_else(|| step.reasons.get(&item).cloned());
                }
            }
            None => {
                let id = format!("f{}", report.facts.len() + 1);
                report.facts.push(semilla_core::Fact {
                    id,
                    at: Some(Anchor::item(&item)),
                    file,
                    part: "attribute".into(),
                    description: format!("attributes of the new item `{item}`"),
                    status: "derived".into(),
                    category: Category::Attribute,
                    spotlight: true,
                    spotlight_why: Some(why),
                    reason: step.reasons.get(&item).cloned(),
                    ..Default::default()
                });
            }
        }
    }

    // 4. Coverage of the syntax edits.
    for ((edit, owner), fact) in edits.iter().zip(&owners).zip(&fact_of) {
        if owner.is_none() && fact.is_none() {
            if derive_edits {
                report.errors.push(format!("internal: an edit has no fact: {}", hints::describe(edit)));
            }
            report.uncovered.push(Uncovered {
                step: step.index,
                view: edit.view,
                // Claims on existing code use the "before" numbering; new statements use "after".
                at: if edit.kind == semilla_syntax::EditKind::Inserted { edit.after.clone() } else { edit.before.clone().or(edit.after.clone()) },
                file: edit.file.clone(),
                description: hints::describe(edit),
                hint: Some(hints::suggest(edit).to_string()),
            });
        }
    }

    // An unclaimed rename shows as a removed item and an added item of the same kind, with the
    // same text after the name change. The hint says so (the claim is still necessary).
    for (old_name, new_name) in hints::likely_renames(&edits, input.before, input.after) {
        for uncovered in report.uncovered.iter_mut() {
            let item = uncovered.at.as_ref().map(|a| a.item.as_str());
            if item == Some(old_name.as_str()) || item == Some(new_name.as_str()) {
                // `rename` keeps the module; another module or crate is a move.
                let parent = |path: &str| path.rsplit_once("::").map(|(p, _)| p.to_string()).unwrap_or_default();
                uncovered.hint = Some(if parent(&old_name) == parent(&new_name) {
                    format!("rename (it looks like a rename of `{old_name}` to `{new_name}`)")
                } else {
                    format!("move-item (it looks like a move of `{old_name}` to `{new_name}`: anchor `{old_name}`, to={new_name}; with no identity claim in the step, Semilla derives the move)")
                });
            }
        }
    }

    for (item, old, new) in hints::likely_local_renames(&edits, input.before, input.after) {
        for uncovered in report.uncovered.iter_mut().filter(|u| u.at.as_ref().is_some_and(|a| a.item == item)) {
            uncovered.hint = Some(format!("rename-local (it looks like a rename of `{old}` to `{new}`: anchor `{item}`, from={old}, to={new})"));
        }
    }

    // 5. The owner of each changed body (invariants I1 and I2). An edit of the item gives the
    // owner of that edit. Else the evidence gives a fact or a claim. Else a new fact with the
    // spotlight owns the body: "indirect change, cause not known".
    let identity_verified = identity_status.as_ref().is_some_and(|(status, _)| status.is_verified());
    let identity_owned: BTreeSet<String> = if identity_verified { identity_derived.clone() } else { BTreeSet::new() };
    let rename_targets: BTreeSet<String> = renames.items.values().cloned().collect();
    let identity_claim = step.claims.iter().position(is_identity);
    // Each entry of the rename map with its identity claim (design 36.1). An `impl` block and
    // its methods follow the move of their type (`checks::moved_impls`).
    let identity_pairs: Vec<(usize, String, String)> = renames
        .items
        .iter()
        .filter_map(|(old, new)| {
            let owns = |claim: &semilla_core::Claim| {
                let Some(at) = claim.at.as_ref().filter(|_| is_identity(claim)) else { return false };
                let (module, name) = at.item.rsplit_once("::").unwrap_or(("", at.item.as_str()));
                let with_type = old.strip_prefix(module).and_then(|rest| rest.strip_prefix("::")).is_some_and(|rest| (rest.starts_with("impl ") || rest.starts_with('<')) && hints::words(rest).contains(&name));
                at.item == *old || checks::is_child(old, &at.item) || with_type
            };
            Some((step.claims.iter().position(owns).or(identity_claim)?, old.clone(), new.clone()))
        })
        .collect();
    // The tests of the two trees (batch 6a, B2): only for a step with an edit of a macro
    // definition. A failure of the oracle gives no data, and the text then names no test.
    let macro_edit = edits.iter().any(|edit| {
        let item = |anchor: &Option<Anchor>, ws: &Workspace| anchor.as_ref().and_then(|a| ws.items.get(&a.item)).is_some_and(owners::is_macro_definition);
        item(&edit.before, input.before) || item(&edit.after, input.after)
    });
    let tests = if macro_edit && body_index.available { oracle.tests(step).ok().flatten() } else { None };
    // The items that a changed macro call makes (batch 6b, part 2): only for a step with an
    // edit of a macro item or of a module. A failure of the oracle gives no data, and the text
    // then says that Semilla cannot list the items.
    let made_edit = edits.iter().any(|edit| {
        let item = |anchor: &Option<Anchor>, ws: &Workspace| anchor.as_ref().and_then(|a| ws.items.get(&a.item)).is_some_and(|item| matches!(item.kind, semilla_syntax::ItemKind::Macro | semilla_syntax::ItemKind::Mod));
        item(&edit.before, input.before) || item(&edit.after, input.after)
    });
    let made = if made_edit && body_index.available { oracle.made(step).ok().flatten() } else { None };
    let made_outcome = made::apply(step, &edits, &fact_of, input.before, input.after, made.as_ref(), &mut report);
    // The definitions of the two sides, for the symbol links (design 44).
    let defs = if body_index.available { oracle.defs(step).unwrap_or_default() } else { Default::default() };
    // The changed expansions of the compiled mode (R63.9-5).
    let expansion_links = expand::links(step, [input.before, input.after], oracle, &renames.items);
    let owner_input = owners::Input {
        expansions: &expansion_links,
        defs: &defs,
        renames: &renames.items,
        made: made.as_ref(),
        made_listed: &made_outcome.listed,
        tests: tests.as_ref(),
        step,
        edits: &edits,
        owners: &owners,
        fact_of: &fact_of,
        bodies: &bodies,
        body_index: &body_index,
        before: input.before,
        after: input.after,
        identity_owned: &identity_owned,
        identity_derived: &identity_derived,
        rename_targets: &rename_targets,
        identity_claim,
        identity_pairs: &identity_pairs,
        callees: call_facts.as_ref(),
        maps: &body_maps,
    };
    owners::assign(&owner_input, &mut report);
    owners::source_files(&owner_input, &mut report);
    // A file that a crate reads at build time is a build input, also when its name is on the file
    // allow list (brief 5f, A6). With a changed body, the fact has its affected bodies. With no changed body, the effect is outside the bodies that Semilla
    // compares (docs, doc tests, attributes, code that no checked configuration compiles), so the
    // fact needs a reason.
    if body_index.available {
        // The items with a changed body that a fact or a claim owns: the report shows them.
        let shown: BTreeSet<String> = report.facts.iter().flat_map(|fact| fact.affected.iter().cloned()).chain(report.claims.iter().flat_map(|claim| claim.affected.iter().cloned())).collect();
        for fact in report.facts.iter_mut() {
            let Some(file) = fact.file.clone().filter(|file| fact.at.is_none() && !fact.spotlight && fact.affected.is_empty() && !(input.before.is_rust_file(file) || input.after.is_rust_file(file)) && !files::is_manifest(file)) else { continue };
            if let Some((reader, _)) = owners::build_reader(input.before, input.after, &file).filter(|(_, items)| !items.iter().any(|item| shown.contains(item))) {
                fact.spotlight = true;
                fact.spotlight_why = Some(format!("`{reader}` reads this file at build time (`include_str!`, `include_bytes!`, or `include!`), and no body changed. The effect is outside the bodies that Semilla compares: docs and doc tests, attributes, or code that no checked configuration compiles"));
                fact.reason = step.reasons.get(&fact.part).cloned();
            }
        }
    }

    // The order of generic parameters (brief 5h, B4). Generic arguments go by position, so a site
    // with explicit arguments keeps its text and its MIR text, and it changes its meaning. The
    // fact lists each item that names the changed item.
    // R70.2-1 (d): an impl header whose trait and self type are the same by the compiler keeps the
    // meaning of each position.
    let same_impl = |edit: &&Edit| match (edit.before.as_ref(), edit.after.as_ref()) {
        (Some(old), Some(new)) => records::impl_trait_ref(input.before, &old.item).is_some_and(|key| records::impl_trait_ref(input.after, &new.item) == Some(key)),
        _ => false,
    };
    for (index, edit) in edits.iter().enumerate().filter(|(_, edit)| facts::generic_order_changed(edit) && !same_impl(edit)) {
        let Some(anchor) = edit.anchor() else { continue };
        let sites = facts::positional_sites(&anchor.item, input.after, call_facts.as_ref());
        // D10 of batch 5k: the count of the items is the count of the list below (the changed
        // item itself is also in that list when its body changes).
        let text = format!("generic arguments go by position: {} site(s) in {} other item(s)", sites.iter().map(|(_, count)| count).sum::<usize>(), sites.len());
        let fact = match fact_of[index].filter(|fact| *fact < report.facts.len()) {
            Some(fact) => fact,
            None => {
                report.facts.push(semilla_core::Fact {
                    id: format!("f{}", report.facts.len() + 1),
                    at: Some(Anchor::item(&anchor.item)),
                    file: edit.file.clone(),
                    part: "generics".into(),
                    description: format!("order of the generic parameters of `{}`", anchor.item),
                    status: "derived".into(),
                    category: Category::Declaration,
                    ..Default::default()
                });
                report.facts.len() - 1
            }
        };
        let fact = &mut report.facts[fact];
        if !fact.description.contains("generic arguments go by position") {
            fact.description.push_str(&format!("; {text}"));
        }
        for (item, _) in sites {
            if !fact.affected.contains(&item) {
                fact.affected.push(item);
            }
        }
        fact.affected.sort();
    }

    // A file that `include!` reads is Rust code that Semilla does not parse, whatever its name
    // is (batch 5w, D3). Its fact has a spotlight (the file rule). The fact lists the items that
    // read the file and that a fact or a claim of this step shows: the file is code of them.
    let mut carried: BTreeSet<String> = BTreeSet::new();
    {
        let shown: BTreeSet<String> = report
            .facts
            .iter()
            .flat_map(|fact| fact.affected.iter().cloned().chain(fact.at.as_ref().map(|at| at.item.clone())))
            .chain(report.claims.iter().flat_map(|claim| claim.affected.iter().cloned()))
            .collect();
        for fact in report.facts.iter_mut().filter(|fact| fact.at.is_none() && fact.spotlight) {
            let Some(readers) = fact.file.as_deref().and_then(|file| owners::code_readers(input.before, input.after, file)) else { continue };
            for reader in readers.iter().filter(|reader| shown.contains(*reader)) {
                if !fact.affected.contains(reader) {
                    fact.affected.push(reader.clone());
                }
            }
            fact.affected.sort();
            carried.extend(readers);
        }
    }

    // A fact whose bodies read a file at build time (batch 6a, B4): the fact needs a reason.
    // A use of the build configuration gives no spotlight (design 35): see the note below.
    for fact in report.facts.iter_mut() {
        let own = fact.at.as_ref().map(|at| at.item.clone());
        let Some(text) = facts::file_spotlight(fact.affected.iter().chain(own.iter()).filter(|item| !carried.contains(*item)), &body_index) else { continue };
        fact.spotlight_why = Some(match fact.spotlight_why.take() {
            Some(why) => format!("{why}; {text}"),
            None => text,
        });
        fact.spotlight = true;
        if fact.reason.is_none() {
            fact.reason = match &fact.at {
                Some(at) => step.reasons.get(&at.to_string()).or_else(|| step.reasons.get(&at.item)).cloned(),
                None => step.reasons.get(&fact.part).cloned(),
            };
        }
    }

    // A fact whose bodies have equal compiled code in only some of the checked configurations
    // (batch 5u, part A). The fact gets a note, and it needs no reason.
    for fact in report.facts.iter_mut() {
        let own = fact.at.as_ref().map(|at| at.item.clone());
        let Some(note) = facts::config_note(fact.affected.iter().chain(own.iter()), &body_index) else { continue };
        fact.note = Some(note);
    }

    // A fact with a body that has code under a build condition that Semilla did not check
    // (design 35): `cfg!`, a `cfg` attribute, `option_env!`, or a macro that Semilla cannot
    // see into. The fact gets a note. The note asks for no reason, and it changes no result.
    for fact in report.facts.iter_mut() {
        let own = fact.at.as_ref().map(|at| at.item.clone());
        let Some(note) = facts::stop_note(fact.affected.iter().chain(own.iter()), &body_index, &carried) else { continue };
        fact.note = Some(match fact.note.take() {
            Some(old) => format!("{old}; {note}"),
            None => note,
        });
    }

    // A fact with a name that has characters outside ASCII (batch 5k, D7). The name can look
    // the same as a different name, so the fact shows the escape form and needs a reason.
    //
    // A literal with a character outside ASCII (batch 5w, part B): each fact text shows the
    // escape form after the literal. A changed literal with a new character outside ASCII
    // needs a reason: the new character can look the same as the old one (`µ` and `μ`). A new
    // literal in new code gets the escape form only.
    let mut literals: Vec<Vec<facts::NewLiteral>> = report.facts.iter().map(|_| Vec::new()).collect();
    let mut add_literals = |fact: usize, old: &str, new: &str| {
        for literal in facts::new_literals(old, new) {
            match literals[fact].iter_mut().find(|other| other.text == literal.text) {
                Some(other) => other.chars.extend(literal.chars.into_iter().filter(|c| !other.chars.contains(c)).collect::<Vec<_>>()),
                None => literals[fact].push(literal),
            }
        }
    };
    // The full text of each edit: a fact text can be cut before the changed character.
    for (edit, fact) in edits.iter().zip(&fact_of) {
        if let (Some(fact), Some(new)) = (fact.filter(|fact| *fact < report.facts.len()), edit.text_after.as_deref()) {
            add_literals(fact, edit.text_before.as_deref().unwrap_or_default(), new);
        }
    }
    for (index, fact) in report.facts.iter().enumerate() {
        for text in std::iter::once(fact.description.as_str()).chain(fact.sites.iter().map(|group| group.edit.as_str())) {
            if let Some((old, new)) = facts::change_sides(text) {
                add_literals(index, old, new);
            }
        }
        if let Some(content) = &fact.content {
            let side = |mark: char| content.lines().filter(|line| line.starts_with(mark) && !line.starts_with("+++") && !line.starts_with("---")).map(|line| &line[1..]).collect::<Vec<_>>().join("\n");
            add_literals(index, &side('-'), &side('+'));
        }
    }
    for (fact, literals) in report.facts.iter_mut().zip(literals) {
        let added: String = fact.content.as_deref().unwrap_or_default().lines().filter(|line| line.starts_with('+')).collect::<Vec<_>>().join("\n");
        let mut names: Vec<String> = Vec::new();
        for text in std::iter::once(fact.description.as_str()).chain(fact.sites.iter().map(|group| group.edit.as_str())).chain(std::iter::once(added.as_str())) {
            for (_, name) in facts::non_ascii_names(text) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        // The new literals that the fact text does not show get a note with the escape form.
        let shown = |fact: &semilla_core::Fact, literal: &str| std::iter::once(fact.description.as_str()).chain(fact.sites.iter().map(|group| group.edit.as_str())).any(|text| facts::marks(text).iter().any(|mark| mark.literal && mark.text == literal));
        // Each output shows a hidden character as its escape, so such a character gets no second form.
        let plain = |text: &str| text.chars().all(|c| c.is_ascii() || semilla_core::text::is_hidden(c));
        let form = |text: &str| match plain(text) {
            true => format!("`{}`", semilla_syntax::tree::one_line(text)),
            false => format!("`{}` (`{}`)", semilla_syntax::tree::one_line(text), semilla_syntax::tree::one_line(&semilla_core::text::ascii_form(text).unwrap_or_default())),
        };
        let quiet: Vec<String> = literals.iter().filter(|literal| literal.chars.is_empty() && !shown(fact, &literal.text)).map(|literal| form(&literal.text)).collect();
        if !quiet.is_empty() {
            let more = if quiet.len() > 5 { format!(", and {} more", quiet.len() - 5) } else { String::new() };
            let note = format!("{} {}{more}", facts::NON_ASCII_NOTE, quiet.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
            fact.note = Some(match fact.note.take() {
                Some(old) => format!("{old}; {note}"),
                None => note,
            });
        }
        fact.description = facts::with_ascii_forms(&fact.description);
        for group in fact.sites.iter_mut() {
            group.edit = facts::with_ascii_forms(&group.edit);
        }
        let mut causes = Vec::new();
        if !names.is_empty() {
            let list: Vec<String> = names.iter().map(|name| format!("`{name}` (`{}`)", semilla_core::text::ascii_form(name).unwrap_or_default())).collect();
            causes.push(format!("{}: {}", facts::NON_ASCII, list.join(", ")));
        }
        let changed: Vec<String> = literals
            .iter()
            .filter(|literal| !literal.chars.is_empty())
            .map(|literal| format!("{} in {}", literal.chars.iter().map(|c| form(&c.to_string())).collect::<Vec<_>>().join(", "), form(&literal.text)))
            .collect();
        if !changed.is_empty() {
            causes.push(format!("{}: {}", facts::NON_ASCII_LITERAL, changed.join("; ")));
        }
        if causes.is_empty() {
            continue;
        }
        let text = causes.join("; ");
        fact.spotlight_why = Some(match fact.spotlight_why.take() {
            Some(why) => format!("{why}; {text}"),
            None => text,
        });
        fact.spotlight = true;
        if fact.reason.is_none() {
            fact.reason = match &fact.at {
                Some(at) => step.reasons.get(&at.to_string()).or_else(|| step.reasons.get(&at.item)).cloned(),
                None => step.reasons.get(&fact.part).cloned(),
            };
        }
    }

    report.panic_delta = panic_sites(input.after) - panic_sites(input.before);
    report.spotlight = spotlight(&edits, input.before, input.after);
    report.spotlight.extend(made_outcome.spotlight.iter().filter(|line| !report.spotlight.contains(line)).cloned().collect::<Vec<_>>());
    // Each body that reads a file at build time (batch 6a, B4). A use of the build
    // configuration gives no line (design 35).
    let config_items: BTreeSet<String> = body_index.config_names().map(|name| body_index.item_of_name(name).unwrap_or_else(|| name.clone())).collect();
    // The name of a body is not always the key of a reason (batch 5l, B3): the body of an
    // associated constant of a trait impl has the MIR name `<T as Trait>::C`, and the fact is
    // at the impl. The line gives each key that is not the name.
    let config_keys = |item: &String| -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for fact in report.facts.iter().filter(|fact| fact.spotlight_why.as_deref().is_some_and(|why| why.contains(facts::INCLUDE_SPOTLIGHT))) {
            let own = fact.at.as_ref().map(|at| &at.item);
            let names = |name: &String| name == item || body_index.item_of_name(name).as_ref() == Some(item);
            if own == Some(item) || fact.affected.iter().any(names) {
                let key = fact.at.as_ref().map(|at| at.to_string()).unwrap_or_else(|| fact.part.clone());
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        keys
    };
    let config_lines: Vec<String> = config_items
        .iter()
        .filter(|item| !carried.contains(*item) && facts::reads_files(&body_index, item))
        .map(|item| match config_keys(item) {
            keys if keys.is_empty() || keys == [item.clone()] => format!("a file that the code reads at build time: {item}"),
            keys => format!("a file that the code reads at build time: {item} (the key of the reason: {})", keys.iter().map(|key| format!("`{key}`")).collect::<Vec<_>>().join(", ")),
        })
        .collect();
    report.spotlight.extend(config_lines);
    // Batch 5y, B3: the line of a features change prints the name of the manifest. The name is
    // not the key of a reason, so the line gives the keys of the facts of the features table.
    for line in report.spotlight.iter_mut() {
        let Some(file) = line.strip_prefix("features: ") else { continue };
        let prefix = format!("{file}:features.");
        let keys: Vec<String> = report.facts.iter().filter(|fact| fact.spotlight && fact.at.is_none() && fact.part.starts_with(&prefix)).map(|fact| format!("`{}`", fact.part)).collect();
        if !keys.is_empty() {
            *line = format!("{line} (the key of the reason: {})", keys.join(", "));
        }
    }
    report.spotlight.sort();
    let truths = if body_index.available { oracle.truths(step).unwrap_or_default() } else { Default::default() };
    // An edit inside a function that no checked configuration compiles (design B11). Only the
    // syntax checks this edit, so the reviewer must read it.
    if body_index.available {
        let mut inactive = BTreeSet::new();
        for edit in &edits {
            for (anchor, ws, side_before) in [(&edit.before, input.before, true), (&edit.after, input.after, false)] {
                let Some(anchor) = anchor else { continue };
                let function = ws.items.get(&anchor.item).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Fn && item.body.is_some());
                // The bodies have the "after" names: a renamed item has its new name there.
                let renamed = side_before.then(|| renames.items.get(&anchor.item)).flatten();
                let compiled = body_index.has_body(&anchor.item) || renamed.is_some_and(|new| body_index.has_body(new));
                // A body that a claim declares as new has no compare result.
                let declared = added.iter().any(|new| anchor.item == *new || checks::is_child(&anchor.item, new));
                if function && !compiled && !declared && !twin_compiled(input.before, input.after, &truths, &anchor.item) {
                    inactive.insert(format!("inactive code: {} (no checked configuration compiles `{}`)", edit.anchor().unwrap_or(anchor), anchor.item));
                }
            }
        }
        report.spotlight.extend(inactive);
    }
    for claim in step.claims.iter().filter(|claim| claim.op == "replace-macro-input") {
        if let Some(at) = &claim.at {
            report.spotlight.push(format!("macro input: {at}"));
        }
    }
    // `tested-by` proves that the test calls the function, not that it checks a result. A test
    // body with no assertion (no `assert!`, `unwrap`, `expect`, `panic!`, or indexing) is in the
    // spotlight.
    for claim in step.claims.iter().filter(|claim| claim.op == "tested-by") {
        let Some(test) = claim.arg_str("test") else { continue };
        let found = input.after.items.get(test).or_else(|| input.after.items.values().find(|item| item.test && item.path.rsplit("::").next() == Some(test)));
        if let Some(item) = found.filter(|item| checks::item_panic_sites(item).is_empty()) {
            report.spotlight.push(format!("test without an assertion: `{}` (claim {}) calls the function but checks nothing in its body", item.path, claim.id));
        }
    }
    for claim in step.claims.iter().filter(|claim| claim.op == "rewrite-item") {
        if let Some(at) = &claim.at {
            report.spotlight.push(format!("rewritten item: {at} ({})", claim.arg_str("note").unwrap_or("")));
        }
    }
    report.lines = line_counts(step, &edits, &owners);
    let (mut level, mut api_changes) = api_level(&edits, input.before, input.after);
    // The items that a changed macro call makes follow the rule of an item with no macro
    // (batch 6b, part 2).
    if !made_outcome.major.is_empty() {
        level = "major";
    } else if !made_outcome.minor.is_empty() && level == "patch" {
        level = "minor";
    }
    api_changes.extend(made_outcome.major.iter().chain(&made_outcome.minor).filter(|line| !api_changes.contains(line)).cloned().collect::<Vec<_>>());
    // A rename or a move of a public item changes the public API too: the diff applies the name
    // map first, so it gives no edit for it.
    for claim in step.claims.iter().filter(|c| is_identity(c)) {
        let Some(at) = &claim.at else { continue };
        if is_public(input.before, &at.item) {
            let new = renames.items.get(&at.item).cloned().unwrap_or_default();
            api_changes.insert(0, format!("{} `{}` → `{new}`", if claim.op == "rename" { "renamed" } else { "moved" }, at.item));
            level = "major";
        }
    }
    report.api_level = Some(level.to_string());
    report.api_changes = api_changes;

    // The compare result of each fact (design 35).
    set_compare(&mut report, &body_index, input.before, input.after, &renames, build_failed, &truths, step.compiled);

    // An edit in code that no checked configuration compiles has no compare (batch 5w, part A).
    if body_index.available {
        let dead = uncompiled::dead_edits([input.before, input.after], &truths, &body_index);
        uncompiled::apply(&dead, [input.before, input.after], step, &edits, &fact_of, &renames, &mut report);
    }

    // The final assertion (invariant I1): no path removes a changed body from the report.
    for name in owners::check(&owner_input, &report) {
        report.errors.push(format!("internal: a changed body has no owner: `{name}`"));
    }

    // A nested function is its own body (batch 5x, part E). The lists get the names for the
    // reader after the last pass that reads them.
    if body_index.available {
        let lists = report.facts.iter_mut().map(|fact| &mut fact.affected).chain(report.claims.iter_mut().map(|claim| &mut claim.affected));
        for list in lists {
            let mut shown: Vec<String> = list.iter().flat_map(|item| body_index.shown_bodies(item)).collect();
            shown.sort();
            shown.dedup();
            *list = shown;
        }
    }

    if derive_edits {
        edits::fold_macro_renames(&mut report.facts, &mut fact_of, input.before, input.after);
        edits::fold(&mut report.facts, &mut fact_of);
    }
    // Part B item 4 of batch 5j: a failed check build gives the same long error to `compare` and
    // to `facts`. The report shows each distinct error one time, complete.
    let mut seen = std::collections::BTreeSet::new();
    report.errors.retain(|error| seen.insert(error.clone()));
    StepOutcome { report, edits, owners, fact_of, renames, bodies, identity_reason, stated_causes: BTreeMap::new(), selections, node_pairs: body_maps.pairs }
}

/// Sets the compare result of each fact (design 35, "Decided by Claude").
///
/// 0. A step with no compiled check (design 50): no fact has a compare result.
/// 1. A fact with the result `not checked` keeps it.
/// 2. A doc test: `no compare`. Semilla does not compile a doc test.
/// 3. A failed build: `no compare`, with the cause.
/// 4. Phase 1 has no MIR: `pending`. A doc text, a comment, and a format change of a manifest
///    have no code: `no code`.
/// 5. A fact with a changed body (`affected`): `code different`.
/// 6. A fact in a function that no checked configuration compiles: `no compare`.
/// 7. A doc text, a comment, a manifest key, and a file with no changed body: `no code`.
/// 8. Each other fact: `code equal`.
#[allow(clippy::too_many_arguments)]
fn set_compare(report: &mut StepReport, body_index: &checks::BodyIndex, before: &Workspace, after: &Workspace, renames: &semilla_syntax::Renames, build_failed: bool, truths: &[Vec<ConfigTruth>; 2], compiled: bool) {
    let twin_compiled = |function: &str| twin_compiled(before, after, truths, function);
    let note = |fact: &mut semilla_core::Fact, text: String| {
        fact.note = Some(match fact.note.take() {
            Some(old) if old.contains(&text) => old,
            Some(old) => format!("{old}; {text}"),
            None => text,
        });
    };
    // A changed body with `unsafe` code (design 36.5): the body of the edit, and each body
    // with changed compiled code. An edit of the signature only is not an edit of the body.
    // R70.2-1 (a): the record of the compiler decides; a tree with no record of the item reads the
    // tokens of the item (a comment, a doc comment, and a string are no tokens).
    let has_unsafe = |item: &str| {
        [after, before].iter().any(|ws| {
            ws.items.get(item).is_some_and(|found| {
                found.kind == semilla_syntax::ItemKind::Fn && found.body.is_some() && records::item_unsafe(ws, item).unwrap_or_else(|| cfgcond::has_keyword(&found.text, "unsafe").unwrap_or_else(|| hints::words(&found.text).contains(&"unsafe")))
            })
        })
    };
    for fact in report.facts.iter_mut() {
        let in_body = fact.at.as_ref().filter(|at| at.path.first().is_some_and(|first| first.to_string() != "sig")).is_some_and(|at| has_unsafe(&at.item));
        fact.unsafe_code = in_body || fact.affected.iter().any(|item| has_unsafe(item));
    }
    // Two or more edits in one body with changed compiled code (design 36.5): the compare is
    // a result of the edits together, and each fact says so.
    let body_of = |fact: &semilla_core::Fact| fact.at.as_ref().filter(|at| !at.path.is_empty() && fact.affected.contains(&at.item)).map(|at| at.item.clone());
    let edited: Vec<Option<String>> = report.facts.iter().map(body_of).collect();
    for (index, fact) in report.facts.iter_mut().enumerate() {
        fact.other_edits = match &edited[index] {
            Some(body) => edited.iter().enumerate().filter(|(other, item)| *other != index && item.as_ref() == Some(body)).count(),
            None => 0,
        };
    }
    for fact in report.facts.iter_mut() {
        // A step with no compiled check (design 50): no fact has a compare result, and no note
        // gives a cause for a compare that Semilla did not make.
        if !compiled {
            fact.compare = Compare::Off;
            continue;
        }
        if fact.compare == Compare::NotChecked {
            continue;
        }
        let no_code = match fact.category {
            Category::Docs => fact.part != "doc test",
            Category::Manifest | Category::File | Category::BuildCode => true,
            _ => false,
        };
        // A doc test, and a `doc(test(…))` attribute that changes each doc test of the crate.
        let doc_test = fact.part == "doc test" || fact.spotlight_why.as_deref().is_some_and(|why| why.starts_with(facts::DOC_TEST_SPOTLIGHT));
        if doc_test {
            fact.compare = Compare::NoCompare;
            note(fact, format!("{} Semilla does not compile a doc test", uncompiled::NO_COMPARE));
            continue;
        }
        if !body_index.available {
            if build_failed {
                fact.compare = Compare::NoCompare;
                note(fact, format!("{} the build failed", uncompiled::NO_COMPARE));
            } else {
                // A manifest key and a file can change a body: only phase 2 knows.
                let format_only = fact.category == Category::Manifest && fact.description.ends_with(": format only");
                fact.compare = if fact.category == Category::Docs || format_only { Compare::NoCode } else { Compare::Pending };
            }
            continue;
        }
        if !fact.affected.is_empty() {
            fact.compare = Compare::CodeDifferent;
            continue;
        }
        // A member order has effects that no MIR body shows (batch 5e): the drop order, the
        // output order of a derive, the discriminant values. Equal bodies are not a result.
        if fact.part == "member order" {
            fact.compare = Compare::NoCompare;
            note(fact, format!("{} the MIR bodies do not show each effect of a member order", uncompiled::NO_COMPARE));
            continue;
        }
        // A fact in a function that no checked configuration compiles. The functions of a fact:
        // the item of the fact, and each function in the lines of the item (an impl, a module).
        let functions = |ws: &Workspace, item: &str| -> Vec<String> {
            let Some(outer) = ws.items.get(item) else { return Vec::new() };
            let inside = |inner: &&semilla_syntax::Item| inner.kind == semilla_syntax::ItemKind::Fn && inner.body.is_some() && inner.file == outer.file && inner.line_start >= outer.line_start && inner.line_end <= outer.line_end;
            ws.items.values().filter(inside).map(|inner| inner.path.clone()).collect()
        };
        let inactive = fact.at.as_ref().filter(|_| !no_code).and_then(|at| {
            let mut found = functions(after, &at.item);
            // A function with a new name or a new module has its body at the new path.
            found.extend(functions(before, &at.item).into_iter().map(|function| renames.items.get(&function).cloned().unwrap_or(function)));
            found.into_iter().find(|function| !body_index.has_body(function))
        });
        if let Some(item) = inactive {
            if matches!(fact.category, Category::Added | Category::Removed) && twin_compiled(&item) {
                fact.compare = Compare::CodeDifferent;
                fact.affected = vec![item];
                continue;
            }
            fact.compare = Compare::NoCompare;
            note(fact, format!("{} no checked configuration compiles `{item}`", uncompiled::NO_COMPARE));
            continue;
        }
        // An added or a removed item with no body (a type, a trait) has no compiled code. An
        // item with a body that is here has a compiled body with an equal result (batch 7b,
        // part 5.1): a different definition of the name gives the same code.
        // A new version of a dependency (design 38.6): Semilla does not compare its code.
        if fact.category == Category::Manifest && fact.at.is_none() && files::is_dependency_version(&fact.part) {
            fact.compare = Compare::NoCompare;
            note(fact, format!("{} Semilla does not compare the code of a dependency", uncompiled::NO_COMPARE));
            continue;
        }
        let bodies = fact.at.as_ref().is_some_and(|at| !functions(after, &at.item).is_empty() || !functions(before, &at.item).is_empty());
        let no_body = matches!(fact.category, Category::Added | Category::Removed) && !bodies;
        fact.compare = if no_code || no_body { Compare::NoCode } else { Compare::CodeEqual };
    }
}

/// True if `function` is a twin that a step adds or removes (design 38.3), and a checked
/// configuration compiles it. The compiled body has the name of the plain path, so the body
/// index has no body for the twin key. The twin is compiled if a checked unit that reads its
/// file makes each own `cfg` condition of the twin true.
fn twin_compiled(before: &Workspace, after: &Workspace, truths: &[Vec<ConfigTruth>; 2], function: &str) -> bool {
    let Some(base) = identity::twin_base(function) else { return false };
    [(before, &truths[0]), (after, &truths[1])].iter().any(|(ws, side)| {
        let Some(item) = ws.items.get(function).filter(|_| ws.items.contains_key(base)) else { return false };
        let preds: Vec<cfgcond::Pred> = item.attrs.iter().flat_map(|attr| cfgcond::own_conditions(attr)).collect();
        !preds.is_empty() && side.iter().any(|config| config.known && config.units.iter().any(|unit| unit.files.contains(&item.file) && preds.iter().all(|pred| cfgcond::eval(pred, &unit.cfg) == Some(true))))
    })
}

/// A derived body of a claimed type: the MIR name is `<TYPE as TRAIT>::method`, the type derives
/// the trait, and no hand-written impl of the trait for the type exists. Returns (trait, type).
fn derived_body(name: &str, types: &[String], before: &Workspace, after: &Workspace) -> Option<(String, String)> {
    let inner = name.strip_prefix('<')?.split_once(">::")?.0;
    let (ty, trait_path) = inner.split_once(" as ")?;
    let ty = ty.split('<').next()?;
    let trait_name = trait_path.rsplit("::").next()?.split('<').next()?.to_string();
    let ty_name = ty.rsplit("::").next()?;
    let ty_path = types.iter().find(|path| path.as_str() == ty || path.rsplit("::").next() == Some(ty_name))?;
    let derives = |ws: &Workspace| {
        ws.items.get(ty_path).is_some_and(|item| facts::derives_trait(&item.attrs, &trait_name))
    };
    let hand_written = |ws: &Workspace| ws.items.values().any(|item| item.kind == semilla_syntax::ItemKind::Impl && item.path.contains(&format!("<{ty_name} as ")) && item.path.split(" as ").nth(1).is_some_and(|tr| tr.trim_end_matches('>').rsplit("::").next() == Some(trait_name.as_str())));
    (derives(before) && derives(after) && !hand_written(before) && !hand_written(after)).then(|| (trait_name, ty_path.clone()))
}

/// True for an identity claim: `rename` or `move-item`.
fn is_identity(claim: &semilla_core::Claim) -> bool {
    matches!(claim.op.as_str(), "rename" | "move-item")
}

/// The rank of a visibility, from private to `pub`.
fn visibility_rank(vis: &str) -> u8 {
    let vis = vis.replace(' ', "");
    match vis.as_str() {
        "" | "pub(self)" => 0,
        "pub" => 3,
        "pub(crate)" => 2,
        _ => 1,
    }
}

/// True if a token text has one of the names in a string literal or in the tokens of a macro
/// call (batch 5x, part C). A text that does not parse gives true.
fn names_in_text(text: &str, names: &BTreeSet<String>) -> bool {
    use semilla_syntax::tt::{TokenStream, TokenTree};
    fn walk(stream: TokenStream, names: &BTreeSet<String>, in_macro: bool) -> bool {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        trees.iter().enumerate().any(|(index, tree)| match tree {
            TokenTree::Literal(literal) => {
                let text = literal.to_string();
                text.contains('"') && names.iter().any(|name| text.contains(name.as_str()))
            }
            TokenTree::Ident(ident) => in_macro && names.contains(&ident.to_string()),
            TokenTree::Group(group) => {
                let call = index > 0 && matches!(&trees[index - 1], TokenTree::Punct(punct) if punct.as_char() == '!');
                walk(group.stream(), names, in_macro || call)
            }
            TokenTree::Punct(_) => false,
        })
    }
    if names.is_empty() {
        return false;
    }
    text.parse::<TokenStream>().map_or(true, |stream| walk(stream, names, false))
}

/// The `cfg` conditions on the path of an item: the `cfg` and `cfg_attr` attributes of each
/// module above the item, and the inner ones of each module file (batch 5x, part B).
fn path_conditions(workspace: &Workspace, path: &str) -> BTreeSet<String> {
    let condition = |attr: &String| -> Option<String> {
        let text: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
        let inner = text.trim_start_matches('#').trim_start_matches('!').trim_start_matches('[');
        (inner.starts_with("cfg(") || inner.starts_with("cfg_attr(")).then(|| format!("#[{inner}"))
    };
    let mut found = BTreeSet::new();
    let mut current = path;
    while let Some((parent, _)) = current.rsplit_once("::") {
        current = parent;
        found.extend(workspace.items.get(current).into_iter().flat_map(|item| item.attrs.iter()).filter_map(condition));
        found.extend(workspace.inner_attrs.get(current).into_iter().flat_map(|(_, attrs)| attrs.iter()).filter_map(condition));
    }
    found
}

/// True for a new module that has only the items of a move (batch 5x, part B): the `mod` line
/// and the file of the module are parts of the move.
///
/// The conditions keep each change of behavior out of the rule:
/// - the `mod` item has no attribute (`cfg`, `path`, `macro_use`), and it is not `pub`;
/// - the file of the module has no inner attribute other than a plain doc comment (`//!`,
///   `/*! */`, `#![doc = "text"]`). The doc text stays its own fact, and a code block in it
///   is a doc test that stops the identity step (batch 5x, round 2);
/// - the `mod` item has no doc comment: such a doc text has no fact of its own;
/// - the module is new: no item had this path, or a path in it, before the step;
/// - each item in the module is an item of the rename map (a moved item or a member of one),
///   or a private `use` line with no attribute. The MIR compare with the rename map is the
///   check of each moved body, so a `use` line that changes a meaning stops the proof.
fn new_module_of_move(module: &semilla_syntax::Item, renames: &semilla_syntax::Renames, before: &Workspace, after: &Workspace) -> bool {
    use semilla_syntax::ItemKind;
    if module.kind != ItemKind::Mod || !module.attrs.is_empty() || visibility_rank(&module.vis) > 2 {
        return false;
    }
    // The loader stores a doc comment with a literal text as `//!` lines. Each other inner
    // attribute keeps its token form (`# ! [...]`), also a doc attribute with a computed text.
    let plain_doc = |attr: &String| attr.lines().all(|line| line.starts_with("//!"));
    if after.inner_attrs.get(&module.path).is_some_and(|(_, attrs)| !attrs.iter().all(plain_doc)) {
        return false;
    }
    // A doc line or a doc attribute on the `mod` item is a part of the added item.
    if module.docs.iter().any(|line| semilla_syntax::diff::is_doc_line(line) || line.starts_with('#')) {
        return false;
    }
    let inside = format!("{}::", module.path);
    if before.items.contains_key(&module.path) || before.items.keys().any(|path| path.starts_with(&inside)) {
        return false;
    }
    let moved: BTreeSet<&String> = renames.items.iter().filter(|(old, new)| old != new).map(|(_, new)| new).collect();
    let mut count = 0;
    after.items.values().filter(|item| item.path.starts_with(&inside)).all(|item| {
        if moved.contains(&item.path) {
            count += 1;
            return true;
        }
        // A new member of a moved item has its own fact (design 36.1).
        if moved.iter().any(|new| checks::is_child(&item.path, new)) {
            return true;
        }
        item.kind == ItemKind::Use && item.vis.is_empty() && item.attrs.is_empty() && !item.path[inside.len()..].contains("::")
    }) && count > 0
}

/// The check of an identity step (design 22.4). Returns the status (and the hint) of its identity
/// claims, or `None` when the step has no verified identity claim.
///
/// 1. The identity claims make one name map (the renames).
/// 2. Each MIR body must be equal at N0 or N1.
/// 3. The MIR proof covers each edit inside a body with a MIR record (also a const initializer).
///    So a second type with the same member name gives no false report.
/// 4. An edit in a body without a MIR record stays uncovered (the name map did not remove it).
/// 5. Outside the bodies, only the forced edits of the claims are allowed: the private `use`
///    lines and the paths (from the claim checks), and a wider visibility of a moved item.
/// 6. Any other claim (except `edit-doc`) or edit gives "split the step".
///
/// For a derived identity, `reason_out` is set: a derived body of another trait than `Debug` does
/// not make the identity unproved. It gives the text for the spotlight of the identity fact, so the
/// fact needs a reason, as a claimed identity does.
#[allow(clippy::too_many_arguments)]
fn identity_step(
    step: &Step,
    results: &[(&semilla_core::Claim, checks::Outcome)],
    edits: &[Edit],
    owners: &mut [Option<usize>],
    bodies: &[BodyComparison],
    body_index: &checks::BodyIndex,
    renames: &semilla_syntax::Renames,
    before: &Workspace,
    after: &Workspace,
    derived_out: &mut BTreeSet<String>,
    reason_out: Option<&mut Option<String>>,
) -> Option<(ClaimStatus, Option<String>)> {
    use semilla_syntax::EditKind;
    let identity: Vec<usize> = results.iter().enumerate().filter(|(_, (claim, outcome))| is_identity(claim) && outcome.status.is_verified()).map(|(index, _)| index).collect();
    let &first = identity.first()?;
    // Design 36.1: a derived rename or move does not stop for a second change. Each other
    // change of the step stays an edit with its own fact, and each changed body gets its owner
    // from the usual rules (`owners::assign`). Only an explicit claim keeps the rules of 22.4.
    let with_changes = reason_out.is_some();
    let split = |what: String| {
        Some((
            ClaimStatus::Refuted { reason: format!("{OTHER_CHANGE}{what}") },
            None,
        ))
    };
    // A move keeps the `cfg` conditions on the path of the item (batch 5x, part B): a module
    // with a different condition puts the item in a different set of builds.
    for (claim, _) in results.iter().filter(|(claim, outcome)| claim.op == "move-item" && outcome.status.is_verified()) {
        let Some((old, new)) = claim.at.as_ref().and_then(|at| renames.items.get(&at.item).map(|new| (&at.item, new))) else { continue };
        let (old_conditions, new_conditions) = (path_conditions(before, old), path_conditions(after, new));
        if old_conditions != new_conditions {
            let list = |conditions: &BTreeSet<String>| if conditions.is_empty() { "none".to_string() } else { conditions.iter().map(|text| format!("`{text}`")).collect::<Vec<_>>().join(", ") };
            return Some((
                ClaimStatus::Refuted { reason: format!("the move of `{old}` changes the `cfg` conditions on its path (before: {}; after: {}): the item is not in the same builds", list(&old_conditions), list(&new_conditions)) },
                Some("Move the item to a module with the same `cfg` conditions. Or remove the identity claim: the items show as removed and added, with the changes as facts.".to_string()),
            ));
        }
    }
    // The rules of design 22.4 for each body:
    // 1. a body with no text edit can be equal at each level (N0 to N4);
    // 2. a body with a text edit must be equal at N0 or N1;
    // 3. a derived body of a type that a claim names (or owns a renamed member of) can differ;
    // 4. every other body that differs rejects the step.
    let edited: BTreeSet<String> = edits.iter().filter_map(|edit| edit.after.as_ref().map(|a| a.item.clone()).or_else(|| edit.before.as_ref().map(|b| renames.items.get(&b.item).cloned().unwrap_or_else(|| b.item.clone())))).collect();
    let claimed_types: Vec<String> = results
        .iter()
        .filter(|(claim, _)| is_identity(claim))
        .filter_map(|(claim, _)| claim.at.as_ref().map(|at| at.item.clone()))
        .flat_map(|item| {
            let owner = item.rsplit_once('.').map(|(owner, _)| owner.to_string()).unwrap_or_else(|| item.clone());
            // The type of a variant, or of a field of a variant, is the enum (trial 4, D11: the
            // derived `Debug` impl of the enum was "not a derived body" for a variant rename).
            let is_variant = before.items.get(&owner).is_some_and(|found| found.kind == semilla_syntax::ItemKind::Variant);
            let owner = if is_variant { owner.rsplit_once("::").map(|(parent, _)| parent.to_string()).unwrap_or(owner) } else { owner };
            let new = renames.items.get(&owner).cloned().unwrap_or_else(|| owner.clone());
            [owner, new]
        })
        .collect();
    let mut derived: Vec<(String, String)> = Vec::new();
    let bad: Vec<&str> = bodies
        .iter()
        .filter(|body| {
            let item = body_index.item_of(body).unwrap_or_else(|| body.name.clone());
            match &body.status {
                BodyStatus::Equal { level, .. } if level == "n0" || level == "n1" => false,
                BodyStatus::Equal { .. } => edited.contains(&item),
                _ => match derived_body(&body.name, &claimed_types, before, after) {
                    Some((trait_name, ty)) => {
                        derived_out.insert(body.name.clone());
                        derived.push((trait_name, ty));
                        false
                    }
                    None => true,
                },
            }
        })
        .map(|body| body.name.as_str())
        .collect();
    // The MIR proof of a body covers the edits inside that body: the body must be equal at N0 or
    // N1 (or the proof waits for phase 2). A changed body keeps its edits uncovered.
    let equal_body = |item: &str| matches!(body_index.status(item), Some(BodyStatus::Equal { level, .. }) if level == "n0" || level == "n1");
    {
        for (index, edit) in edits.iter().enumerate() {
            if owners[index].is_some() {
                continue;
            }
            let item = edit.after.as_ref().map(|a| a.item.clone()).or_else(|| edit.before.as_ref().map(|b| renames.items.get(&b.item).cloned().unwrap_or_else(|| b.item.clone())));
            let Some(item) = item else { continue };
            let in_body = matches!(edit.kind, EditKind::Inserted | EditKind::Deleted | EditKind::Moved | EditKind::Modified | EditKind::Replaced);
            // A const or a static has its initializer in its signature text (design 22.3, A6).
            let initializer = edit.kind == EditKind::Signature && matches!(edit.attr_before.as_deref(), Some("const" | "static")) && edit.anchor().is_some_and(|a| a.path.is_empty());
            // An equal compare never hides the text of an edit (design 35.1): with a derived
            // identity, the edit keeps its own fact.
            if !with_changes && (in_body || initializer) && (equal_body(&item) || !body_index.available) {
                owners[index] = Some(first);
            }
        }
    }
    // A new path dependency on a crate of the workspace, when a move goes to another crate
    // (design 23, rule 8).
    let cross_crate = results.iter().any(|(claim, _)| claim.op == "move-item" && claim.at.as_ref().zip(claim.arg_str("to")).is_some_and(|(at, to)| at.item.split("::").next() != to.split("::").next()));
    if cross_crate {
        for (index, edit) in edits.iter().enumerate() {
            let Some(file) = edit.file.as_deref().filter(|f| f.ends_with("Cargo.toml")) else { continue };
            if owners[index].is_some() || edit.kind != EditKind::FileChanged {
                continue;
            }
            let changes = files::key_changes(file, before.other_files.get(file).map(String::as_str), after.other_files.get(file).map(String::as_str));
            if changes.is_some_and(|changes| files::is_workspace_path_dependency(&changes, before)) {
                owners[index] = results.iter().position(|(claim, _)| claim.op == "move-item");
            }
        }
    }
    // A wider visibility of a moved item (`pub(crate)`, `pub(super)`, `pub`) does not stop the
    // move. It stays its own fact to read, with no reason (batch 5x, part B): see `allowed`.
    let moved_item = |edit: &Edit| {
        edit.after.as_ref().is_some_and(|a| a.path.is_empty() && results.iter().any(|(claim, _)| claim.op == "move-item" && claim.at.as_ref().is_some_and(|at| renames.items.get(&at.item) == Some(&a.item))))
    };
    let wider = |edit: &Edit| edit.kind == EditKind::Visibility && moved_item(edit) && visibility_rank(edit.text_after.as_deref().unwrap_or("")) > visibility_rank(edit.text_before.as_deref().unwrap_or(""));
    // A new module that has only the items of a move is a part of the move (batch 5x, part B).
    for (index, edit) in edits.iter().enumerate() {
        if owners[index].is_some() || edit.kind != EditKind::ItemAdded {
            continue;
        }
        let Some(module) = edit.after.as_ref().filter(|a| a.path.is_empty()).and_then(|a| after.items.get(&a.item)) else { continue };
        if !new_module_of_move(module, renames, before, after) {
            continue;
        }
        owners[index] = results.iter().position(|(claim, outcome)| claim.op == "move-item" && outcome.status.is_verified() && claim.arg_str("to").is_some_and(|to| to.strip_prefix(module.path.as_str()).is_some_and(|rest| rest.starts_with("::") && !rest[2..].contains("::"))));
    }
    // A step with no compiled check (design 50): the syntax tree is the proof of a derived
    // rename or move. The check of the claim made the name map, and each edit that the map does
    // not give keeps its own fact (`with_changes`).
    if !body_index.available && with_changes && !step.compiled {
        return Some((ClaimStatus::Verified { proof: "syntax tree".into() }, None));
    }
    if !body_index.available {
        return Some((ClaimStatus::Pending { reason: "the compare of the rename or the move runs in the background check (phase 2)".into() }, None));
    }
    let other_claim = results.iter().find(|(claim, _)| !is_identity(claim) && claim.op != "edit-doc").map(|(claim, _)| format!("the step also has the claim {} (`{}`)", claim.id, claim.op));
    // B5: a doc change and a rename of a parameter do not stop the identity. Each one stays its
    // own fact (a doc fact has the category `docs`). A parameter rename is allowed only when its
    // body is equal (N0 or N1): a parameter name is a local in the MIR.
    // A doc test change is not a plain doc change (brief 5h, B1): it stops the identity.
    // An attribute of the `doc` family with no literal text (`#[doc(hidden)]`) is an attribute
    // edit of its item (batch 6b, part 1). It is not a plain doc comment: it stops the identity
    // (batch 5x, round 2). The line filter stays for an attribute line in a comment edit.
    let attribute_lines = |text: &Option<String>| -> Vec<String> { text.as_deref().unwrap_or("").lines().filter(|line| line.starts_with('#')).map(str::to_string).collect() };
    let allowed = |index: usize, edit: &Edit| {
        (edit.kind == EditKind::Comment && attribute_lines(&edit.text_before) == attribute_lines(&edit.text_after))
            || wider(edit)
            || (edit.kind == EditKind::Docs && facts::doc_test_change(edit).is_none())
            || (facts::is_parameter_rename(edits, index, before, after, renames) && edit.anchor().is_some_and(|a| equal_body(&renames.items.get(&a.item).cloned().unwrap_or_else(|| a.item.clone()))))
    };
    let other_edit = edits.iter().zip(owners.iter()).enumerate().find(|(index, (edit, owner))| owner.is_none() && !allowed(*index, edit)).map(|(_, (edit, _))| format!("the step also has the change {}", edit.anchor().map(|a| format!("at `{a}`")).or_else(|| edit.file.clone()).unwrap_or_default()));
    if let Some(name) = bad.first().filter(|_| !with_changes) {
        // "Split the step" is a route only when the step has a second change.
        if other_claim.is_some() || other_edit.is_some() {
            return split(format!("the MIR of `{name}` changed"));
        }
        let remove = "Remove the identity claim: the items show as removed and added, with the changes as facts.";
        // The old name in a string or in a macro call: only then the hint can be true.
        let olds: BTreeSet<String> = renames.items.iter().filter_map(|(old, new)| (checks::last_segment(old) != checks::last_segment(new)).then(|| checks::last_segment(old).to_string())).collect();
        let item = body_index.item_of_name(name).unwrap_or_else(|| name.to_string());
        let old_item = renames.items.iter().find(|(_, new)| **new == item).map(|(old, _)| old.clone()).unwrap_or_else(|| item.clone());
        let look = before.items.get(&old_item).is_some_and(|found| names_in_text(&found.text, &olds));
        let hint = if look {
            format!("Look at `{name}`: it has the old name in a string or in a macro call. If the output of this code changes, the change is real. Then remove the identity claim: the items show as removed and added, with the changes as facts.")
        } else {
            remove.to_string()
        };
        return Some((
            ClaimStatus::Refuted { reason: format!("the MIR of `{name}` changes with the rename or the move, and `{name}` has no text edit and is not a derived body") },
            Some(hint),
        ));
    }
    // Batch 6a, B4 (trial 9): the only other change is a new `pub` module that has only the
    // items of the move. The advice "split the step" is wrong: a new step gives the same result.
    // The real cause is the new public module.
    let others: Vec<&Edit> = edits.iter().zip(owners.iter()).enumerate().filter(|(index, (edit, owner))| owner.is_none() && !allowed(*index, edit)).map(|(_, (edit, _))| edit).collect();
    let public_module = match others.as_slice() {
        [edit] if edit.kind == EditKind::ItemAdded && other_claim.is_none() => edit.after.as_ref().filter(|a| a.path.is_empty()).and_then(|a| after.items.get(&a.item)).filter(|module| {
            let mut private = (*module).clone();
            private.vis = String::new();
            visibility_rank(&module.vis) > 2 && new_module_of_move(&private, renames, before, after)
        }),
        _ => None,
    };
    if let Some(module) = public_module.filter(|_| !with_changes) {
        return Some((ClaimStatus::Refuted { reason: format!("the new module `{}` is `pub`, so the move changes the public API. Semilla does not derive a move into a new public module: this is a known limit", module.path) }, None));
    }
    if let Some(what) = other_claim.or(other_edit).filter(|_| !with_changes) {
        return split(what);
    }
    // Rule 3: the derived bodies. `Debug` shows the name, which is expected. Any other trait (serde
    // and others) can change an output format, so the claim needs a reason.
    derived.sort();
    derived.dedup();
    let others: Vec<&(String, String)> = derived.iter().filter(|(trait_name, _)| trait_name != "Debug").collect();
    if !others.is_empty() {
        let anchor = results[first].0.at.as_ref().map(|a| a.to_string()).unwrap_or_default();
        let list = others.iter().map(|(t, ty)| format!("{t} for {ty}")).collect::<Vec<_>>().join(", ");
        if let Some(out) = reason_out {
            *out = Some(format!("the derived output of {list} changes with the rename"));
        } else if !step.reasons.contains_key(&anchor) {
            return Some((
                ClaimStatus::Refuted { reason: format!("the derived output of {list} changes with the rename; give a reason with `--reason '{anchor}=<why>'`") },
                Some("A derived trait other than `Debug` (for example serde) can change a data format. Say why the change is correct.".into()),
            ));
        }
    }
    // The derivation holds. The text names the derived bodies that show the name.
    let mut proof = "compare".to_string();
    if !derived.is_empty() {
        let list = derived.iter().map(|(t, ty)| format!("{t} for {ty}")).collect::<Vec<_>>().join(", ");
        let count = derived.len();
        proof = format!("{proof}; {count} derived {} the name ({list})", if count == 1 { "body shows" } else { "bodies show" });
    }
    let _ = step;
    Some((ClaimStatus::Verified { proof }, None))
}

/// The items of any kind whose lines contain a body span (an impl, a module, a struct with a
/// derive).
pub(crate) fn containers(workspace: &Workspace, span: Option<&(String, usize)>) -> Vec<String> {
    let Some((file, line)) = span else { return Vec::new() };
    workspace
        .items
        .values()
        .filter(|item| (file.ends_with(&item.file) || item.file.ends_with(file.as_str())) && item.line_start <= *line && *line <= item.line_end)
        .map(|item| item.path.clone())
        .collect()
}

/// Maps the MIR facts of the bodies to the items (a closure belongs to its function).
fn item_facts(found: &[BodyFacts], after: &Workspace) -> BTreeMap<String, ItemFacts> {
    let names: BTreeMap<&str, String> = found.iter().filter_map(|f| Some((f.name.as_str(), checks::find_item_by(&f.name, f.span.as_ref(), after)?))).collect();
    let mut facts: BTreeMap<String, ItemFacts> = BTreeMap::new();
    let bodies: BTreeSet<&str> = found.iter().map(|f| f.name.as_str()).collect();
    for body in found {
        // A body with no syntax item (a derived impl, generated code) keeps its own name, so a
        // property claim still examines it (A3).
        let item = names.get(body.name.as_str()).cloned().unwrap_or_else(|| body.name.clone());
        let item = &item;
        let entry = facts.entry(item.clone()).or_default();
        entry.externals.extend(body.externals.iter().cloned());
        entry.panics.extend(body.panics.iter().cloned());
        entry.origins.extend(body.origins.iter().map(|(path, origin)| (path.clone(), origin.clone())));
        entry.reach_types.extend(body.reach_types.iter().cloned());
        entry.drop_types.extend(body.drop_types.iter().cloned());
        for (path, _) in &body.drop_types {
            match body.drop_calls.iter().find(|(ty, _)| ty == path) {
                Some((_, call)) => drop(entry.drop_calls.entry(path.clone()).or_insert_with(|| call.clone())),
                None => drop(entry.drop_real.insert(path.clone())),
            }
        }
        entry.impl_of.extend(body.impl_of.iter().cloned());
        entry.drop_of.extend(body.drop_of.iter().cloned());
        entry.param_types.extend(body.param_types.iter().cloned());
        entry.const_types.extend(body.const_types.iter().cloned());
        // The signature of a closure is not a signature of the item: no caller names it.
        entry.signature_changed |= body.signature_changed && !body.name.contains("::{");
        if let Some(live) = &body.live {
            entry.live.push((body.name.clone(), body.target.clone(), live.clone()));
        }
        if let Some(sure) = &body.sure {
            entry.sure.push((body.name.clone(), body.target.clone(), sure.clone()));
            entry.ends.push(body.ends.clone());
        }
        entry.casts.extend(body.casts.iter().filter(|c| !entry.casts.contains(c)).cloned().collect::<Vec<_>>());
        entry.layouts.extend(body.layouts.iter().filter(|c| !entry.layouts.contains(c)).cloned().collect::<Vec<_>>());
        // The same body in two targets (the lib and its test build) counts once.
        for (index, call) in body.indirect.iter().enumerate() {
            let key = format!("{call} (in `{}`, #{index})", body.name);
            if !entry.indirect.contains(&key) {
                entry.indirect.push(key);
            }
        }
        for callee in &body.callees {
            let target = names.get(callee.as_str()).cloned().or_else(|| (after.items.contains_key(callee.as_str()) || bodies.contains(callee.as_str())).then(|| callee.clone()));
            if let Some(target) = target.filter(|t| t != item) {
                entry.callees.push(target);
            }
        }
    }
    facts
}

/// The changed lines of a step, by the kind of claim that explains them.
pub fn line_counts(step: &Step, edits: &[Edit], owners: &[Option<usize>]) -> semilla_core::LineCounts {
    let mut counts = semilla_core::LineCounts::default();
    for (edit, owner) in edits.iter().zip(owners) {
        let Some(claim) = owner.and_then(|index| step.claims.get(index)) else { continue };
        // The member list of an impl repeats a new or removed method: count the method only.
        if edit.kind == semilla_syntax::EditKind::Members && matches!(claim.op.as_str(), "add-item" | "remove-item" | "rewrite-item") {
            continue;
        }
        let lines = |text: &Option<String>| text.as_deref().map(|t| t.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0);
        let count = lines(&edit.source_before).max(lines(&edit.source_after)).max(1);
        match claim.op.as_str() {
            "add-item" => counts.new += count,
            "remove-item" => counts.removed += count,
            "rewrite-item" => counts.rewritten += count,
            _ => counts.fine += count,
        }
    }
    counts
}


/// The pair threshold: a removed item and an added item with this similarity or more look like
/// one changed item.
pub const PAIR_THRESHOLD: f64 = 0.7;

/// Finds the pair warnings of a change set: a verified `remove-item` and a verified `add-item` of
/// the same kind, with similar text. `items` gives the removed item (in the tree before its step)
/// and the added item (in the tree after its step) of each claim.
pub fn pair_warnings(set: &ChangeSet, report: &Report, items: &BTreeMap<String, semilla_syntax::Item>) -> Vec<semilla_core::PairWarning> {
    pair_warnings_with(set, report, items, &[])
}

/// The pair warnings from the claims and from the derived facts of removed and added items
/// (design 24.2). `facts` holds (an ID, true for an added item, the item).
pub fn pair_warnings_with(set: &ChangeSet, report: &Report, items: &BTreeMap<String, semilla_syntax::Item>, facts: &[(String, bool, semilla_syntax::Item)]) -> Vec<semilla_core::PairWarning> {
    let verified = |id: &str| report.steps.iter().flat_map(|s| &s.claims).any(|c| c.claim == id && c.status.is_verified());
    let claims: Vec<&semilla_core::Claim> = set.steps.iter().flat_map(|s| &s.claims).filter(|c| verified(&c.id)).collect();
    let mut removed_items: Vec<(String, &semilla_syntax::Item)> = claims.iter().filter(|c| c.op == "remove-item").filter_map(|c| Some((c.id.clone(), items.get(&c.id)?))).collect();
    let mut added_items: Vec<(String, &semilla_syntax::Item)> = claims.iter().filter(|c| c.op == "add-item").filter_map(|c| Some((c.id.clone(), items.get(&c.id)?))).collect();
    for (id, added, item) in facts {
        if *added { added_items.push((id.clone(), item)) } else { removed_items.push((id.clone(), item)) }
    }
    // All candidates, then a greedy match: each item is in at most one pair, the most similar first.
    let mut candidates = Vec::new();
    for (removed_id, old) in &removed_items {
        let old = *old;
        for (added_id, new) in &added_items {
            let new = *new;
            // Two `use` lines are no pair (batch 6a, A4): a `use` line has no name of its own.
            if old.kind != new.kind || old.path == new.path || old.kind == semilla_syntax::ItemKind::Use {
                continue;
            }
            let similarity = hints::similarity(old, new);
            if similarity >= PAIR_THRESHOLD {
                candidates.push((similarity, old, new, removed_id.clone(), added_id.clone()));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut used = BTreeSet::new();
    let mut warnings = Vec::new();
    for (similarity, old, new, removed, added) in candidates {
        if used.contains(&old.path) || used.contains(&new.path) {
            continue;
        }
        used.insert(old.path.clone());
        used.insert(new.path.clone());
        let refusal = set.pair_refusals.iter().find(|r| r.old == old.path && r.new == new.path).map(|r| r.reason.clone());
        warnings.push(semilla_core::PairWarning { old: old.path.clone(), new: new.path.clone(), similarity, claims: vec![removed, added], refusal, reviewer: None, resolves: Vec::new(), stopped: None });
    }
    warnings.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
    warnings
}

/// The last segment of the record name of a test harness `main`. The MIR driver has the same
/// text (`semilla-mir/src/harness.rs`).
pub const TEST_HARNESS_MAIN: &str = "{test-harness-main}";

/// True for the record of the `main` function that rustc generates for a test build. The MIR
/// driver finds the function by its origin in the compiler, and gives the record a name that no
/// item of the user can have: `<crate>::{test-harness-main}`.
///
/// This function does not use the name `main`. A `main` function of the user (also one from a
/// macro) has the full check.
pub fn is_test_harness_main(name: &str) -> bool {
    name.strip_suffix(TEST_HARNESS_MAIN).is_some_and(|start| start.ends_with("::"))
}

/// Counts the explicit panic sites in the non-test functions of a workspace (design 5.5):
/// `unwrap` and `expect` calls, each panic macro of the standard prelude (the shared list,
/// R65.4-1: `panic!`, `unreachable!`, `todo!`, `unimplemented!`, the `assert!` macros), and
/// indexing. The implicit checks (arithmetic overflow) do not count.
pub fn panic_sites(workspace: &Workspace) -> i64 {
    let mut count = 0;
    for item in workspace.items.values().filter(|item| !item.test) {
        let Some(body) = &item.body else { continue };
        let mut path = Vec::new();
        body.walk(&mut path, &mut |_, node| {
            let attr = node.attr.as_deref().unwrap_or("");
            let site = match node.kind {
                "method_call" => matches!(attr, "unwrap" | "expect"),
                "macro" => semilla_core::names::panics(attr),
                "index" => true,
                _ => false,
            };
            if site {
                count += 1;
            }
        });
    }
    count
}

/// True if an item and all its parent modules are `pub` (the item is in the public API).
fn is_public(workspace: &Workspace, path: &str) -> bool {
    let Some(item) = workspace.items.get(path) else { return false };
    // A field is public if it is `pub` and its owner is public.
    // A field of a variant has no visibility of its own: it is public with its variant (D7).
    if let Some((owner, _)) = path.rsplit_once('.').filter(|_| item.kind == semilla_syntax::ItemKind::Field) {
        let of_variant = workspace.items.get(owner).is_some_and(|o| o.kind == semilla_syntax::ItemKind::Variant);
        return (item.vis == "pub" || (of_variant && item.vis.is_empty())) && is_public(workspace, owner);
    }
    // A variant and a trait method have no visibility of their own: they are public with their owner.
    let owned = matches!(item.kind, semilla_syntax::ItemKind::Variant) || path.rsplit_once("::").and_then(|(owner, _)| workspace.items.get(owner)).is_some_and(|owner| owner.kind == semilla_syntax::ItemKind::Trait);
    if owned && item.vis.is_empty() {
        return path.rsplit_once("::").is_some_and(|(owner, _)| is_public(workspace, owner));
    }
    if item.vis != "pub" || item.test {
        return false;
    }
    // A `pub use` can re-export an item (or the owner type of a method) from a private module.
    let reexported = |name: &str| {
        workspace.items.values().any(|use_item| {
            use_item.kind == semilla_syntax::ItemKind::Use && use_item.vis == "pub" && use_item.signature.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == name)
        })
    };
    let segments: Vec<&str> = path.split("::").collect();
    if segments.len() >= 2 && (reexported(segments[segments.len() - 1]) || reexported(segments[segments.len() - 2])) {
        return true;
    }
    // Methods: the owner type is public. Items: each parent module is public.
    let mut current = path;
    while let Some((parent, _)) = current.rsplit_once("::") {
        if let Some(owner) = workspace.items.get(parent) {
            if owner.vis != "pub" && owner.kind != semilla_syntax::ItemKind::Impl {
                return false;
            }
        }
        current = parent;
    }
    true
}

/// The answer to "can a different crate name this item?" (batch 5x, round 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// No path from a different crate goes to the item.
    No,
    /// The item and each owner on its path are `pub`.
    Yes,
    /// Semilla cannot decide. The text is the cause.
    Maybe(String),
}

/// True if the file of an item can be a part of a library: a different crate can use a library
/// only. A binary, a test, an example, and a bench have no API. A file with no known target
/// counts as a library file.
pub(crate) fn in_library(workspace: &Workspace, item: &semilla_syntax::Item) -> bool {
    let mut roots = BTreeMap::new();
    srcconfig::file_roots(workspace, &mut roots);
    let Some(roots) = roots.get(&item.file) else { return true };
    workspace.targets.iter().filter(|target| roots.contains(&target.file)).any(|target| !matches!(target.kind.as_str(), "bin" | "test" | "example" | "bench" | "custom-build"))
}

/// The leaves of a `use` tree: the path of each leaf, the name that it gives, and the glob mark.
pub(crate) fn use_leaves(tree: &semilla_syntax::rs::UseTree, prefix: &[String], out: &mut Vec<(Vec<String>, Option<String>, bool)>) {
    use semilla_syntax::rs::UseTree;
    let with = |ident: &str| -> Vec<String> { if ident == "self" && !prefix.is_empty() { prefix.to_vec() } else { prefix.iter().cloned().chain([ident.to_string()]).collect() } };
    match tree {
        UseTree::Path(ident, tree) => use_leaves(tree, &with(ident), out),
        UseTree::Name(ident) => out.push((with(ident), None, false)),
        UseTree::Rename(ident, rename) => out.push((with(ident), Some(rename.clone()), false)),
        UseTree::Group(items) => items.iter().for_each(|tree| use_leaves(tree, prefix, out)),
        UseTree::Glob => out.push((prefix.to_vec(), None, true)),
    }
}

/// Decides if a different crate can name an item (batch 5x, round 2). The rule is conservative:
/// each case that the text does not decide gives `Maybe`.
///
/// 1. `No`: the item is not `pub`, it is test code, or its file is not a part of a library.
/// 2. `Yes`: each owner on the path of the item (a module or a type) is `pub`.
/// 3. `Maybe`: a `pub use` line of the crate brings the item or an owner on its path (by the
///    name, also through an `as` name of a `use` line), or it is a glob on a relative path.
/// 4. `Maybe`: a macro of the crate has a `pub use` line, or a macro call names the item or
///    an owner. A module of the crate has a `#[path]` attribute.
/// 5. `Maybe`: the item is a member of a `pub` type. A public signature or a type alias can
///    give the type to a different crate with no `pub use` line.
/// 6. `No` in each other case.
pub(crate) fn api_reach(workspace: &Workspace, path: &str) -> Reach {
    use semilla_syntax::ItemKind;
    let Some(item) = workspace.items.get(path) else { return Reach::No };
    let compact = |text: &str| text.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    if item.test || compact(&item.vis) != "pub" || !in_library(workspace, item) {
        return Reach::No;
    }
    // R70.2-1 (b): the effective visibility of the compiler decides. A false `cfg` that removes an
    // item with a name of the path keeps the text rule below (the safe side): another
    // configuration can have a `pub use` line or an owner with that name.
    if let Some(public) = records::item_public(workspace, path) {
        if public {
            return Reach::Yes;
        }
        let path_names: Vec<String> = path.split([':', '.']).filter(|part| !part.is_empty()).map(|part| part.split('#').next().unwrap_or(part).to_string()).collect();
        if records::removed_name(workspace, &path_names).is_none() {
            return Reach::No;
        }
    }
    // The owners on the path, from the nearest one: `c::m::T.f` has `c::m::T` and `c::m`.
    let mut owners: Vec<&semilla_syntax::Item> = Vec::new();
    let mut current = path;
    while let Some(at) = current.rfind("::").into_iter().chain(current.rfind('.')).max() {
        current = &current[..at];
        owners.extend(workspace.items.get(current));
    }
    if owners.iter().all(|owner| compact(&owner.vis) == "pub" || owner.kind == ItemKind::Impl) {
        return Reach::Yes;
    }
    // The names on the path, with no twin suffix (`f#1`). The crate name is not one of them.
    let own_name = |path: &str| path.rsplit(['.', ':']).next().unwrap_or(path).split('#').next().unwrap_or("").to_string();
    let mut names: BTreeSet<String> = owners.iter().map(|owner| own_name(&owner.path)).chain([own_name(path)]).filter(|name| !name.is_empty()).collect();
    // A `use` line has no name of its own: its names are the names that it brings.
    if let Some(parsed) = semilla_syntax::rs::parse_use(&item.signature).filter(|_| item.kind == ItemKind::Use) {
        let mut leaves = Vec::new();
        use_leaves(&parsed.tree, &[], &mut leaves);
        names.extend(leaves.into_iter().filter_map(|(target, alias, _)| alias.or_else(|| target.last().cloned())));
    }
    let same_crate = |other: &&semilla_syntax::Item| other.crate_name == item.crate_name;
    let uses: Vec<(bool, Vec<(Vec<String>, Option<String>, bool)>)> = workspace
        .items
        .values()
        .filter(same_crate)
        .filter(|other| other.kind == ItemKind::Use)
        .filter_map(|other| {
            let parsed = semilla_syntax::rs::parse_use(&other.signature)?;
            let mut leaves = Vec::new();
            use_leaves(&parsed.tree, &[], &mut leaves);
            Some((compact(&other.vis) == "pub", leaves))
        })
        .collect();
    // An `as` name of a `use` line for a name of the path is a name of the path too.
    for _ in 0..4 {
        for (_, leaves) in &uses {
            for (target, alias, glob) in leaves {
                if let Some(alias) = alias.as_ref().filter(|_| !glob && target.last().is_some_and(|last| names.contains(last))) {
                    names.insert(alias.clone());
                }
            }
        }
    }
    let relative = |segment: &String| matches!(segment.as_str(), "crate" | "self" | "super");
    for (_, leaves) in uses.iter().filter(|(public, _)| *public) {
        for (target, _, glob) in leaves {
            if *glob && target.iter().all(relative) {
                return Reach::Maybe("a glob `pub use` line with a relative path".into());
            }
            let found = if *glob { target.iter().find(|segment| names.contains(*segment)) } else { target.last().filter(|last| names.contains(*last)) };
            if let Some(name) = found {
                return Reach::Maybe(format!("a {}`pub use` line names `{name}`", if *glob { "glob " } else { "" }));
            }
        }
    }
    for other in workspace.items.values().filter(same_crate) {
        if other.kind == ItemKind::Macro {
            if other.text.contains("pub use") {
                return Reach::Maybe(format!("the macro `{}` has a `pub use` line", own_name(&other.path)));
            }
            let definition = other.text.starts_with("macro_rules");
            if let Some(name) = names.iter().find(|name| !definition && other.text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == name.as_str())) {
                return Reach::Maybe(format!("a macro call names `{name}`"));
            }
        }
        if other.kind == ItemKind::Mod && other.attrs.iter().any(|attr| compact(attr).contains("path=")) {
            return Reach::Maybe(format!("the module `{}` has a `#[path]` attribute", other.path));
        }
    }
    if let Some(owner) = owners.iter().find(|owner| matches!(owner.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Trait | ItemKind::Type | ItemKind::Other)) {
        if compact(&owner.vis) == "pub" {
            return Reach::Maybe(format!("the type `{}` is `pub`, and a public signature can give it to a different crate", own_name(&owner.path)));
        }
    }
    Reach::No
}

/// The effect of a visibility edit on the public API (batch 5x, round 2): the words for the
/// fact, or `None` if no crate other than the crate of the item sees a difference.
pub(crate) fn visibility_api_change(edit: &Edit, before: &Workspace, after: &Workspace) -> Option<String> {
    let old = edit.before.as_ref().map_or(Reach::No, |anchor| api_reach(before, &anchor.item));
    let new = edit.after.as_ref().map_or(Reach::No, |anchor| api_reach(after, &anchor.item));
    match (old, new) {
        (Reach::No, Reach::No) => None,
        (Reach::Yes, Reach::Yes) => None,
        (_, Reach::Yes) => Some("the public API changes: a different crate can now use the item".into()),
        (Reach::Yes, Reach::No) => Some("the public API changes: a different crate can no longer use the item".into()),
        (Reach::Maybe(cause), _) | (_, Reach::Maybe(cause)) => Some(format!("the public API can change: {cause}")),
    }
}

/// The answer of `api_reach` for an item of an identity pair (batch 5x, round 3). A variant, a
/// field of a variant, and a method of a trait have no visibility of their own: the owner
/// gives the answer.
fn member_reach(workspace: &Workspace, path: &str) -> Reach {
    use semilla_syntax::ItemKind;
    let Some(item) = workspace.items.get(path) else { return Reach::No };
    let owner = path.rfind("::").into_iter().chain(path.rfind('.')).max().and_then(|at| workspace.items.get(&path[..at]));
    match owner {
        Some(owner) if item.vis.is_empty() && (matches!(owner.kind, ItemKind::Trait | ItemKind::Variant) || item.kind == ItemKind::Variant) => member_reach(workspace, &owner.path),
        _ => api_reach(workspace, path),
    }
}

/// The answer to "can a different crate use this declaration?" (batch 6a, A1). It is
/// `member_reach` for an item with a visibility. An impl block and a method of a trait impl
/// have no visibility: the trait and the self type of the header give the answer.
pub(crate) fn declaration_reach(workspace: &Workspace, path: &str) -> Reach {
    use semilla_syntax::ItemKind;
    if let Some(item) = workspace.items.get(path).filter(|item| item.kind == ItemKind::Impl) {
        return impl_reach(workspace, item);
    }
    // A method of a trait impl: `m::<T as Trait>::f` belongs to `m::impl <T as Trait>`.
    if let Some((module, rest)) = path.split_once("::<") {
        if let Some(at) = rest.rfind(">::") {
            return match workspace.items.get(&format!("{module}::impl <{}>", &rest[..at])) {
                Some(owner) => impl_reach(workspace, owner),
                None if workspace.items.get(path).is_some_and(|item| item.test) => Reach::No,
                None => Reach::Maybe("Semilla did not find the impl block of the method".into()),
            };
        }
    }
    member_reach(workspace, path)
}

/// Decides if a different crate can use an impl block (batch 6a, A1): it can, if it can name
/// the trait or the self type. A name that is not a type or a trait of the crate (a trait of a
/// different crate, a generic parameter, `&str`) gives "possibly": Semilla cannot decide.
fn impl_reach(workspace: &Workspace, item: &semilla_syntax::Item) -> Reach {
    use semilla_syntax::ItemKind;
    if item.test || !in_library(workspace, item) {
        return Reach::No;
    }
    use semilla_syntax::rs::ast;
    let Some(ast::Item::Impl(parsed)) = semilla_syntax::rs::parse_item(&item.signature) else {
        return Reach::Maybe("Semilla cannot read the header of the impl block".into());
    };
    let text = |ty: &ast::Type| semilla_syntax::rs::compare(ty).split_whitespace().collect::<String>();
    // The last name of a plain path type (no `<T as Trait>::` start).
    let last = |ty: &ast::Type| match ty {
        ast::Type::PathType(path) => path.path().filter(|path| path.first_segment().is_some_and(|first| first.type_anchor().is_none())).and_then(|path| path.segment()).and_then(|segment| segment.name_ref()).map(|name| name.text().to_string()),
        _ => None,
    };
    let mut names: Vec<(Option<String>, String)> = Vec::new();
    if let Some(path) = parsed.trait_() {
        names.push((last(&path), text(&path)));
    }
    if let Some(self_ty) = parsed.self_ty() {
        names.push((last(&self_ty), text(&self_ty)));
    }
    let (mut maybe, mut unknown): (Option<String>, Option<String>) = (None, None);
    for (name, shown) in names {
        let found: Vec<&semilla_syntax::Item> = match &name {
            Some(name) => workspace
                .items
                .values()
                .filter(|other| other.crate_name == item.crate_name && matches!(other.kind, ItemKind::Struct | ItemKind::Enum | ItemKind::Trait | ItemKind::Type | ItemKind::Other))
                .filter(|other| other.path.rsplit("::").next().is_some_and(|own| own.split('#').next() == Some(name.as_str())))
                .collect(),
            None => Vec::new(),
        };
        if found.is_empty() {
            unknown.get_or_insert(format!("`{shown}` is not an item of the crate, and a different crate can name it"));
            continue;
        }
        for other in found {
            match api_reach(workspace, &other.path) {
                Reach::Yes => return Reach::Yes,
                Reach::Maybe(cause) => {
                    maybe.get_or_insert(cause);
                }
                Reach::No => {}
            }
        }
    }
    // The cause of an item of the crate is the better text.
    maybe.or(unknown).map_or(Reach::No, Reach::Maybe)
}

/// The answer to "can a different crate use this associated constant or type?" (design
/// 36.2). A member of a trait impl has no visibility: the impl gives the answer. A member of
/// an inherent impl must be `pub`. A member that Semilla cannot find gets the answer of the
/// impl (the higher level).
fn assoc_reach(workspace: &Workspace, impl_path: &str, name: &str) -> Reach {
    let Some(item) = workspace.items.get(impl_path) else { return Reach::No };
    use semilla_syntax::rs::ast;
    use ast::{HasName, HasVisibility};
    let public = match semilla_syntax::rs::parse_item(&item.source) {
        Some(ast::Item::Impl(parsed)) if parsed.trait_().is_none() => {
            let named = |node: Option<ast::Name>| node.is_some_and(|node| node.text() == name);
            let vis = parsed.assoc_item_list().into_iter().flat_map(|list| list.assoc_items()).find_map(|member| match member {
                ast::AssocItem::Const(constant) if named(constant.name()) => Some(constant.visibility()),
                ast::AssocItem::TypeAlias(ty) if named(ty.name()) => Some(ty.visibility()),
                _ => None,
            });
            // Only `pub` is public: `pub(crate)` is not.
            vis.is_none_or(|vis| vis.is_some_and(|vis| vis.visibility_inner().is_none()))
        }
        _ => true,
    };
    if public { impl_reach(workspace, item) } else { Reach::No }
}

/// The text of a possible public API change, from the answer for the two sides.
fn reach_note(old: Reach, new: Reach) -> Option<String> {
    match (old, new) {
        (Reach::No, Reach::No) => None,
        (Reach::Yes, _) | (_, Reach::Yes) => Some("the public API can change: a different crate can use the item".to_string()),
        (Reach::Maybe(cause), _) | (_, Reach::Maybe(cause)) => Some(format!("the public API can change: {cause}")),
    }
}

/// The public API note of a fact about an associated constant or type (design 36.2).
pub(crate) fn assoc_api_note(impl_path: &str, name: &str, before: &Workspace, after: &Workspace) -> Option<String> {
    reach_note(assoc_reach(before, impl_path, name), assoc_reach(after, impl_path, name))
}

/// True for an impl of `Send` or `Sync`: the auto trait of each type with a field of the self
/// type can change too.
fn is_auto_trait_impl(item: &semilla_syntax::Item) -> bool {
    let name = facts::trait_impl_name(item);
    name.split_once(" as ").is_some_and(|(_, tr)| matches!(tr.trim_end_matches('>').rsplit("::").next().unwrap_or(""), "Send" | "Sync"))
}

/// The public API note of a fact about a new or removed trait impl (design 36.2).
pub(crate) fn impl_api_note(impl_path: &str, before: &Workspace, after: &Workspace) -> Option<String> {
    let side = |ws: &Workspace| ws.items.get(impl_path).map_or(Reach::No, |item| impl_reach(ws, item));
    let note = reach_note(side(before), side(after))?;
    let auto = before.items.get(impl_path).or_else(|| after.items.get(impl_path)).is_some_and(is_auto_trait_impl);
    Some(if auto { format!("{note}; {AUTO_TRAIT_NOTE}") } else { note })
}

/// The text for a changed `Send` or `Sync` impl.
const AUTO_TRAIT_NOTE: &str = "each type with a field of this type can change too";

/// The public API note of a fact about a changed `derive` list of a type (design 36.2).
pub(crate) fn derive_api_note(type_path: &str, before: &Workspace, after: &Workspace) -> Option<String> {
    reach_note(declaration_reach(before, type_path), declaration_reach(after, type_path))
}

/// The public API note of the fact of a new or removed item (design 36.2). An inherent impl
/// has the note if a different crate can use one of its constants or types. `None` for a
/// `use` line (its fact has the public names) and for an item that no different crate can use.
pub(crate) fn item_api_note(workspace: &Workspace, path: &str, added: bool) -> Option<String> {
    use semilla_syntax::ItemKind;
    let item = workspace.items.get(path)?;
    let reach = match item.kind {
        ItemKind::Use => return None,
        ItemKind::Impl if !item.path.contains(" as ") => {
            let names = item.members.iter().filter(|member| member.starts_with("const ") || member.starts_with("type ")).filter_map(|member| member.split_whitespace().nth(1));
            names.map(|name| assoc_reach(workspace, path, name)).find(|reach| *reach != Reach::No).unwrap_or(Reach::No)
        }
        _ => declaration_reach(workspace, path),
    };
    match reach {
        Reach::No => None,
        Reach::Yes if added => Some("the public API changes: a different crate can now use the item".into()),
        Reach::Yes => Some("the public API changes: a different crate can no longer use the item".into()),
        Reach::Maybe(cause) => Some(format!("the public API can change: {cause}")),
    }
}

/// The members of an impl block for the text of its fact: `` with: `const ALL`, `fn new` ``.
/// The empty text for an impl with no member.
pub(crate) fn impl_members_text(item: &semilla_syntax::Item) -> String {
    let names: Vec<String> = item.members.iter().map(|member| format!("`{}`", member.split_whitespace().take(2).collect::<Vec<_>>().join(" "))).collect();
    if names.is_empty() { String::new() } else { format!(" with: {}", names.join(", ")) }
}

/// The fact kinds of a declaration for which an equal MIR of the workspace proves nothing
/// about a different crate (batch 6a, A1; the audit table of design 33.1).
fn declaration_part(part: &str) -> bool {
    matches!(part, "generics" | "where clause" | "parameter" | "parameter order" | "return type" | "qualifiers" | "source text" | "type" | "value" | "type and value" | "discriminant" | "member added" | "member removed" | "tuple fields")
}

/// The effect of a declaration change outside the bodies of the workspace (batch 6a, A1).
///
/// - `read`: the fact is a fact to read, also with no changed body. This is true for each
///   change of the generic parameters, the bounds, the where clause, and the supertrait list,
///   and for each declaration part of an item that a different crate can use.
/// - The text is the note for the fact, or `None` if no different crate can use the item.
///
/// Semilla does not decide if a bound is weaker or stronger, so the text says "can change".
pub(crate) fn declaration_api_change(part: &str, item: &str, before: &Workspace, after: &Workspace) -> (bool, Option<String>) {
    if !declaration_part(part) {
        return (false, None);
    }
    let bounds = matches!(part, "generics" | "where clause");
    let note = match (declaration_reach(before, item), declaration_reach(after, item)) {
        (Reach::No, Reach::No) => None,
        (Reach::Yes, _) | (_, Reach::Yes) => Some("the public API can change: a different crate can use the item".to_string()),
        (Reach::Maybe(cause), _) | (_, Reach::Maybe(cause)) => Some(format!("the public API can change: {cause}")),
    };
    (bounds || note.is_some(), note)
}

/// One change of a public name from a `pub use` line (batch 6a, A4).
pub(crate) struct NameChange {
    pub text: String,
    /// True if code of a different crate can stop to compile or get a different item.
    pub breaking: bool,
}

/// The public names that the `pub use` lines of a module give: the name and the target. The
/// key of a glob line is `*` and the target.
fn public_names(workspace: &Workspace, module: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for item in workspace.items.values().filter(|item| item.kind == semilla_syntax::ItemKind::Use && item.vis.replace(' ', "") == "pub" && item.path.rsplit_once("::").is_some_and(|(owner, _)| owner == module)) {
        for (name, target, glob) in checks::use_line_names(workspace, item).unwrap_or_default() {
            out.insert(if glob { format!("*{target}") } else { name }, target);
        }
    }
    out
}

/// The changes of the public names that a removed line (`old`) and an added line (`new`) of a
/// module make (batch 6a, A4). `None` if no line is a `pub use` line that a different crate
/// can use.
///
/// - The line must be `pub`. A `pub(crate) use` line gives no name to a different crate.
/// - The module must be the crate root, or a different crate can possibly name it (the rule
///   of batch 5x, `api_reach`).
/// - The names come from each `pub use` line of the module. So a name that goes from one
///   line to a different line with the same target is no change.
pub(crate) fn pub_use_changes(before: &Workspace, after: &Workspace, old: Option<&str>, new: Option<&str>) -> Option<Vec<NameChange>> {
    let public = |workspace: &Workspace, path: &str| -> Option<Vec<(String, String, bool)>> {
        let item = workspace.items.get(path)?;
        let module = path.rsplit_once("::").map(|(owner, _)| owner)?;
        let reach = !item.test && in_library(workspace, item) && (!module.contains("::") || api_reach(workspace, module) != Reach::No);
        (item.kind == semilla_syntax::ItemKind::Use && item.vis.replace(' ', "") == "pub" && reach).then(|| checks::use_line_names(workspace, item).unwrap_or_default())
    };
    let (old_names, new_names) = (old.and_then(|path| public(before, path)), new.and_then(|path| public(after, path)));
    if old_names.is_none() && new_names.is_none() {
        return None;
    }
    let module = old.or(new)?.rsplit_once("::").map(|(owner, _)| owner)?;
    let (names_before, names_after) = (public_names(before, module), public_names(after, module));
    let mut changes = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, target, glob) in old_names.into_iter().flatten().chain(new_names.into_iter().flatten()) {
        let key = if glob { format!("*{target}") } else { name.clone() };
        if !seen.insert(key.clone()) {
            continue;
        }
        let change = match (names_before.get(&key), names_after.get(&key)) {
            (None, Some(_)) if glob => Some(NameChange { text: format!("added a glob: each public name of `{target}` now is a public name of `{module}`"), breaking: false }),
            (Some(_), None) if glob => Some(NameChange { text: format!("removed a glob: the public names of `{target}` are no longer public names of `{module}`"), breaking: true }),
            (None, Some(new)) => Some(NameChange { text: format!("added the public name `{module}::{name}` (`{new}`)"), breaking: false }),
            (Some(old), None) => Some(NameChange { text: format!("removed the public name `{module}::{name}` (`{old}`)"), breaking: true }),
            (Some(old), Some(new)) if old != new => Some(NameChange { text: format!("the public name `{module}::{name}` now is `{new}` (before: `{old}`)"), breaking: true }),
            _ => None,
        };
        changes.extend(change);
    }
    Some(changes)
}

/// True if a `pub use` line of the crate of `path` can give the last name of `path`: a line
/// with that name, or a glob line.
fn pub_use_gives(workspace: &Workspace, path: &str) -> bool {
    use semilla_syntax::ItemKind;
    let name = path.rsplit(['.', ':']).next().unwrap_or(path);
    let krate = path.split("::").next().unwrap_or(path);
    workspace.items.values().filter(|item| item.kind == ItemKind::Use && item.crate_name == krate && item.vis.replace(' ', "") == "pub").any(|item| {
        let Some(parsed) = semilla_syntax::rs::parse_use(&item.signature) else { return true };
        let mut leaves = Vec::new();
        use_leaves(&parsed.tree, &[], &mut leaves);
        leaves.iter().any(|(target, alias, glob)| *glob || alias.as_deref().or(target.last().map(String::as_str)) == Some(name))
    })
}

/// The effect of a proved rename or move on the public API (batch 5x, round 3): the words for
/// one fact, or `None` if no different crate can use the old path or the new path.
pub(crate) fn identity_api_change(old: &str, new: &str, before: &Workspace, after: &Workspace) -> Option<String> {
    match (member_reach(before, old), member_reach(after, new)) {
        (Reach::No, Reach::No) => None,
        (Reach::Yes, Reach::Yes) if pub_use_gives(after, old) => Some(format!("public API: `{old}` is now `{new}` (a `pub use` line can keep the old path)")),
        (Reach::Yes, Reach::Yes) => Some(format!("public API: `{old}` is now `{new}` (a different crate can no longer use the old path)")),
        (_, Reach::Yes) => Some(format!("public API: a different crate can now use `{new}`")),
        (Reach::Yes, Reach::No) => Some(format!("public API: a different crate can no longer use `{old}`")),
        (Reach::Maybe(cause), _) | (_, Reach::Maybe(cause)) => Some(format!("public API can change: {cause} (`{old}` is now `{new}`)")),
    }
}

/// Adds one fact for each proved rename or move that changes the public API (batch 5x, round
/// 3; design 31.8). The pair is the top item of a derived identity fact or of a verified
/// identity claim. The fact has the category of its identity fact and the result `no code`.
fn identity_api_facts(input: &StepInput, outcome: &mut StepOutcome) {
    let report = &mut outcome.report;
    let mut olds: Vec<String> = report.facts.iter().filter(|fact| fact.part == "identity").filter_map(|fact| fact.at.as_ref()).map(|at| at.item.clone()).collect();
    for claim in input.step.claims.iter().filter(|claim| is_identity(claim)) {
        let verified = report.claims.iter().any(|result| result.claim == claim.id && result.status.is_verified());
        olds.extend(claim.at.iter().filter(|_| verified).map(|at| at.item.clone()));
    }
    for old in olds {
        let Some(new) = outcome.renames.items.get(&old) else { continue };
        let Some(description) = identity_api_change(&old, new, input.before, input.after) else { continue };
        // A visibility fact with an API text for the same item says the same thing.
        let said = |fact: &semilla_core::Fact| fact.part == "visibility" && fact.description.contains("public API") && fact.at.as_ref().is_some_and(|at| at.item == *new || at.item == old);
        if report.facts.iter().any(said) {
            continue;
        }
        report.facts.push(semilla_core::Fact {
            id: format!("f{}", report.facts.len() + 1),
            at: Some(Anchor::item(new)),
            file: input.after.items.get(new).map(|item| item.file.clone()),
            part: "public api".into(),
            description,
            status: "derived".into(),
            // The category of the identity fact of the item, if one exists (batch 9b2, F2: a
            // renamed variant of a renamed enum has a new parent, and it is a rename).
            category: match report.facts.iter().find(|fact| fact.part == "identity" && fact.at.as_ref().is_some_and(|at| at.item == old)) {
                Some(fact) => fact.category,
                None if old.rsplit_once("::").map(|(parent, _)| parent) == new.rsplit_once("::").map(|(parent, _)| parent) => Category::Rename,
                None => Category::Move,
            },
            compare: Compare::NoCode,
            ..Default::default()
        });
    }
}

/// Estimates the semver level of the public API change of a step, from the syntax edits.
///
/// - `major`: a public item is removed, or its signature or visibility changes.
/// - `minor`: a public item is added, or a public struct or enum gets a new member.
/// - `patch`: no change to the public API.
pub fn api_level(edits: &[Edit], before: &Workspace, after: &Workspace) -> (&'static str, Vec<String>) {
    use semilla_syntax::EditKind;
    let mut major = Vec::new();
    let mut minor = Vec::new();
    for edit in edits {
        let old = edit.before.as_ref().map(|a| a.item.as_str()).filter(|item| is_public(before, item));
        let new = edit.after.as_ref().map(|a| a.item.as_str()).filter(|item| is_public(after, item));
        // An item edit, or an edit of a signature part (`sig/param[a]`).
        let path_only = edit.anchor().is_some_and(|a| a.path.is_empty() || a.path.first() == Some(&semilla_core::Segment::Role("sig".into())));
        if !path_only {
            continue;
        }
        // A `use` line (batch 6a, A4): the public names, not the key of the line.
        if matches!(edit.kind, EditKind::ItemRemoved | EditKind::ItemAdded) && (edit.attr_before.as_deref() == Some("use") || edit.attr_after.as_deref() == Some("use")) {
            let changes = pub_use_changes(before, after, edit.before.as_ref().map(|a| a.item.as_str()), edit.after.as_ref().map(|a| a.item.as_str()));
            for change in changes.into_iter().flatten() {
                let list = if change.breaking { &mut major } else { &mut minor };
                if !list.contains(&change.text) {
                    list.push(change.text);
                }
            }
            continue;
        }
        // A trait impl has no visibility (batch 6b, part 2): a different crate can use it if it
        // can name the trait or the self type. A new impl is a new part of the public API, and
        // a removed impl breaks the code that uses it.
        if matches!(edit.kind, EditKind::ItemRemoved | EditKind::ItemAdded) {
            let (ws, anchor) = if edit.kind == EditKind::ItemAdded { (after, &edit.after) } else { (before, &edit.before) };
            let item = anchor.as_ref().and_then(|a| ws.items.get(&a.item)).filter(|item| item.kind == semilla_syntax::ItemKind::Impl && item.path.contains(" as "));
            if let Some(item) = item.filter(|item| impl_reach(ws, item) != Reach::No) {
                // The form of a `derive` line: "`Version` now has `From<u64>`" (design 36.2).
                let name = facts::trait_impl_name(item);
                let inner = name.trim_start_matches('<').strip_suffix('>').unwrap_or(&name);
                let (ty, tr) = inner.split_once(" as ").unwrap_or((inner, ""));
                let ty = if ty.contains('<') { ty } else { ty.rsplit("::").next().unwrap_or(ty) };
                let auto = if is_auto_trait_impl(item) { format!(": {AUTO_TRAIT_NOTE}") } else { String::new() };
                match edit.kind {
                    EditKind::ItemAdded => minor.push(format!("`{ty}` now has `{tr}`{auto}")),
                    _ => major.push(format!("`{ty}` no longer has `{tr}` (the public API can change){auto}")),
                }
                continue;
            }
            // A new or removed inherent impl: each member is an item with its own edit
            // (design 56.2). The impl has no line of its own.
            if anchor.as_ref().and_then(|a| ws.items.get(&a.item)).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Impl && !item.path.contains(" as ")) {
                continue;
            }
        }
        // The bounds of a declaration with an equal set of predicates (batch 6b, part 3): the
        // order of the bounds and the place of a bound are not a part of the public API.
        if facts::bounds_form(edit, before, after).is_some() {
            continue;
        }
        // A changed `derive` list of a type that a different crate can use (batch 6b, part 4):
        // each new name is a new trait impl, and a removed name removes an impl.
        if edit.kind == EditKind::Attrs {
            let side = |ws: &Workspace, anchor: &Option<Anchor>| -> Option<(String, Vec<String>)> {
                let item = ws.items.get(&anchor.as_ref()?.item)?;
                let is_type = matches!(item.kind, semilla_syntax::ItemKind::Struct | semilla_syntax::ItemKind::Enum);
                (is_type && declaration_reach(ws, &item.path) != Reach::No).then(|| (item.path.rsplit("::").next().unwrap_or("").to_string(), derive_names(&item.attrs)))
            };
            if let (Some((_, was)), Some((name, now))) = (side(before, &edit.before), side(after, &edit.after)) {
                let list = |names: Vec<&String>| names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ");
                let (added, removed): (Vec<&String>, Vec<&String>) = (now.iter().filter(|n| !was.contains(n)).collect(), was.iter().filter(|n| !now.contains(n)).collect());
                let lines = [(!added.is_empty()).then(|| (false, format!("`{name}` now has {}", list(added)))), (!removed.is_empty()).then(|| (true, format!("`{name}` no longer has {} (the public API can change)", list(removed))))];
                for (breaking, line) in lines.into_iter().flatten() {
                    let target = if breaking { &mut major } else { &mut minor };
                    if !target.contains(&line) {
                        target.push(line);
                    }
                }
            }
            continue;
        }
        match edit.kind {
            EditKind::ItemRemoved if old.is_some() => major.push(format!("removed `{}`", old.unwrap())),
            EditKind::ItemAdded if new.is_some() => minor.push(format!("added `{}`", new.unwrap())),
            EditKind::Signature if old.is_some() || new.is_some() => major.push(format!("signature of `{}`", old.or(new).unwrap())),
            // An impl block and a method of a trait impl have no visibility (batch 6a, A1): a
            // changed header or a changed bound can change the public API.
            EditKind::Signature if facts::signature_part(edit).is_some() && (edit.before.as_ref().is_some_and(|a| declaration_reach(before, &a.item) != Reach::No) || edit.after.as_ref().is_some_and(|a| declaration_reach(after, &a.item) != Reach::No)) => {
                major.push(format!("signature of `{}` (the public API can change)", edit.anchor().map(|a| a.item.clone()).unwrap_or_default()))
            }
            EditKind::Visibility if old.is_some() => major.push(format!("visibility of `{}`", old.unwrap())),
            EditKind::Visibility if new.is_some() => minor.push(format!("`{}` is now public", new.unwrap())),
            EditKind::Members if old.is_some() => {
                let old_members: Vec<&str> = edit.text_before.as_deref().unwrap_or("").lines().collect();
                let new_members: Vec<&str> = edit.text_after.as_deref().unwrap_or("").lines().collect();
                if old_members.iter().all(|m| new_members.contains(m)) {
                    minor.push(format!("new members in `{}`", old.unwrap()));
                } else {
                    major.push(format!("changed members of `{}`", old.unwrap()));
                }
            }
            _ => {}
        }
    }
    let level = if !major.is_empty() { "major" } else if !minor.is_empty() { "minor" } else { "patch" };
    major.extend(minor);
    (level, major)
}

/// The names in the `derive` attributes of an item, in source order: the last segment of each
/// path. A `derive` in a `cfg_attr` is not in the list.
fn derive_names(attrs: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for attr in attrs {
        let text: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
        let Some(inner) = text.strip_prefix("#[derive(").and_then(|rest| rest.strip_suffix(")]")) else { continue };
        for name in inner.split(',').filter_map(|path| path.rsplit("::").next()).filter(|name| !name.is_empty()) {
            if !names.iter().any(|known| known == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// The edits that always need the reviewer's attention (design 5.5).
pub fn spotlight(edits: &[Edit], before: &Workspace, after: &Workspace) -> Vec<String> {
    let mut found = BTreeSet::new();
    for edit in edits {
        let place = edit.anchor().map(|a| a.to_string()).or_else(|| edit.file.clone()).unwrap_or_default();
        let texts = [edit.text_before.as_deref(), edit.text_after.as_deref()];
        // The text rule of R70.1-1 (batch 10u): the tokens of the code decide, never the text of
        // a comment or of a string. A text that does not parse as tokens uses the text match (the
        // old rule): it can only add a line.
        let unsafe_word = |text: &str| cfgcond::has_keyword(text, "unsafe").unwrap_or_else(|| text.split(' ').any(|token| token == "unsafe"));
        // R70.2-1 (a): the record of the compiler decides at the place of the edit; the tokens decide
        // only where the tree has no record. A token `unsafe` that the record of the built code
        // does not have keeps the line (the safe side), with the cause.
        let in_block = in_unsafe_sides(edit, before, after);
        let text = [texts[0].is_some_and(unsafe_word) || in_block[0], texts[1].is_some_and(unsafe_word) || in_block[1]];
        match records::spotlight(edit, [before, after], text, &records::unsafe_at) {
            records::Decision::Yes => {
                found.insert(format!("unsafe code: {place}"));
            }
            records::Decision::Text => {
                found.insert(format!("unsafe code: {place} (the code has the keyword `unsafe`; the compiler did not build an `unsafe` block, `unsafe fn`, or `unsafe impl` there in the checked configuration)"));
            }
            records::Decision::No => {}
        }
        // `#[cfg(test)]` only selects test code, so it is not in the spotlight.
        // The text of an attribute, not each use of the word `cfg` (a function with the name
        // `cfg` is not an attribute). A use of `cfg!` in a body gets the "build configuration"
        // spotlight from the MIR records.
        let cfg = |text: &str| {
            cfgcond::has_cfg_attribute(text).unwrap_or_else(|| {
                let text = text.replace(' ', "").replace("#[cfg(test)]", "");
                ["#[cfg(", "#![cfg(", "#[cfg_attr(", "#![cfg_attr("].iter().any(|start| text.contains(start))
            })
        };
        // R70.2-1 (a): the record of the compiler decides at the place of the edit, as for `unsafe`.
        if matches!(edit.kind, semilla_syntax::EditKind::Attrs | semilla_syntax::EditKind::ItemAdded | semilla_syntax::EditKind::ItemRemoved) {
            match records::spotlight(edit, [before, after], [texts[0].is_some_and(cfg), texts[1].is_some_and(cfg)], &records::cfg_at) {
                records::Decision::Yes => {
                    found.insert(format!("cfg attribute: {place}"));
                }
                records::Decision::Text => {
                    found.insert(format!("cfg attribute: {place} (the code has a `cfg` attribute that the compiler did not evaluate in the checked configuration)"));
                }
                records::Decision::No => {}
            }
        }
        // A new or removed impl of a trait that the compiler calls with no call in the text.
        if matches!(edit.kind, semilla_syntax::EditKind::ItemAdded | semilla_syntax::EditKind::ItemRemoved) {
            if let Some(item) = edit.item().filter(|item| item.contains("::impl <")) {
                if [" as Drop>", " as Deref>", " as DerefMut>"].iter().any(|t| item.ends_with(t)) {
                    found.insert(format!("implicit call: {item} (the MIR of the callers does not show it)"));
                }
            }
        }
        if let Some(file) = &edit.file {
            if file.ends_with("build.rs") || files::code_file(before, after, file).is_some_and(|code| code.is_build_script()) {
                found.insert(format!("build script: {file}"));
            }
            if file.ends_with("Cargo.toml") {
                let old = before.other_files.get(file).map(String::as_str).unwrap_or("");
                let new = after.other_files.get(file).map(String::as_str).unwrap_or("");
                let features = |text: &str| text.split("[features]").nth(1).map(|rest| rest.split("\n[").next().unwrap_or("").to_string());
                if features(old) != features(new) {
                    found.insert(format!("features: {file}"));
                }
            }
        }
    }
    found.into_iter().collect()
}

/// For each tree (before, after): true if the edit is inside an `unsafe` block of a function
/// body, by the syntax tree.
fn in_unsafe_sides(edit: &Edit, before: &Workspace, after: &Workspace) -> [bool; 2] {
    let check = |anchor: Option<&Anchor>, workspace: &Workspace| {
        let Some(anchor) = anchor else { return false };
        let mut current = anchor.clone();
        while let Some(parent) = current.parent() {
            if semilla_syntax::resolve_node(workspace, &parent).is_some_and(|node| node.kind == "unsafe") {
                return true;
            }
            current = parent;
        }
        false
    };
    [check(edit.before.as_ref(), before), check(edit.after.as_ref(), after)]
}



/// The options of a change set check.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Require groups (the final check before a review). Without it, missing groups are a warning.
    pub require_groups: bool,
    /// Derive a fact for each edit with no claim (design 24.2).
    pub derive_edits: bool,
}

/// Checks a full change set. `load` gives the workspace of a git tree.
pub fn verify_changeset(
    set: &ChangeSet,
    load: &mut dyn FnMut(&str) -> anyhow::Result<Workspace>,
    bodies: &dyn BodyOracle,
    exprs: &dyn ExprOracle,
    options: Options,
) -> (Report, Vec<StepOutcome>) {
    verify_changeset_cached(set, load, bodies, exprs, options, &mut NoStepCache)
}

/// The results of the step checks that a process did before (design 37.21). The caller makes
/// the key: it must have each input of the check of a step.
pub trait StepCache {
    /// The result of an equal check of this step, if the process has it.
    fn get(&mut self, step: &Step) -> Option<StepOutcome>;
    /// Keeps the result of the check of a step.
    fn put(&mut self, step: &Step, outcome: &StepOutcome);
}

/// A cache that keeps no result.
pub struct NoStepCache;

impl StepCache for NoStepCache {
    fn get(&mut self, _: &Step) -> Option<StepOutcome> {
        None
    }

    fn put(&mut self, _: &Step, _: &StepOutcome) {}
}

/// [`verify_changeset`] with the step results of this process. A step with a result in the
/// cache does not run its check again. The checks of the full change set run each time.
pub fn verify_changeset_cached(
    set: &ChangeSet,
    load: &mut dyn FnMut(&str) -> anyhow::Result<Workspace>,
    bodies: &dyn BodyOracle,
    exprs: &dyn ExprOracle,
    options: Options,
    cache: &mut dyn StepCache,
) -> (Report, Vec<StepOutcome>) {
    let mut report = Report::default();
    let mut outcomes = Vec::new();
    let mut pair_items: BTreeMap<String, semilla_syntax::Item> = BTreeMap::new();
    let mut fact_pairs: Vec<(String, bool, semilla_syntax::Item)> = Vec::new();
    // B5 of batch 5k: for each removed item and each added item (the flag is true for an added
    // item), its text and the items that its names point to.
    let mut name_targets: BTreeMap<(bool, String), (String, BTreeMap<String, String>)> = BTreeMap::new();
    // The pairs whose identity proof waits for phase 2 (brief 5f, B1).
    let mut pending_pairs: BTreeSet<(String, String)> = BTreeSet::new();
    let mut previous_tree = set.base_tree.clone();
    for step in &set.steps {
        if step.tree_before != previous_tree {
            report.organization.push(format!("step {} does not start at the tree where the previous step ends", step.index));
        }
        previous_tree = step.tree_after.clone();
        let loaded = load(&step.tree_before).and_then(|before| Ok((before, load(&step.tree_after)?)));
        match loaded {
            Ok((before, after)) => {
                for claim in &step.claims {
                    let found = match (claim.op.as_str(), &claim.at) {
                        ("remove-item", Some(at)) => before.items.get(&at.item),
                        ("add-item", Some(at)) => after.items.get(&at.item),
                        _ => None,
                    };
                    if let Some(item) = found {
                        pair_items.insert(claim.id.clone(), item.clone());
                        let added = claim.op == "add-item";
                        name_targets.insert((added, item.path.clone()), (item.text.clone(), identity::name_targets(item, if added { &after } else { &before })));
                    }
                }
                let outcome = match cache.get(step) {
                    Some(outcome) => outcome,
                    None => {
                        let outcome = verify_step_with(&StepInput { step, before: &before, after: &after }, bodies, exprs, options.derive_edits);
                        cache.put(step, &outcome);
                        outcome
                    }
                };
                // Phase 1 has no MIR: the identity proof of a rename or a move waits for phase 2.
                // Phase 2 proves the identity (then the pair is one item), or it gives the pair
                // warning. Phase 1 gives no warning for such a pair.
                // A step with no compiled check has its final result now (design 50): its pair
                // warning shows.
                if step.compiled && outcome.report.identity_candidates && outcome.bodies.is_empty() && matches!(bodies.facts(step), Ok(None)) {
                    pending_pairs.extend(identity::candidates(&before, &after).0.into_iter().map(|pair| (pair.old, pair.new)));
                }
                // The removed and added items of the derived facts are pair candidates too.
                for (edit, fact) in outcome.edits.iter().zip(&outcome.fact_of) {
                    let Some(fact) = fact.and_then(|f| outcome.report.facts.get(f)) else { continue };
                    if fact.part != "edit" {
                        continue;
                    }
                    let id = format!("step {} {}", step.index, fact.id);
                    match edit.kind {
                        semilla_syntax::EditKind::ItemRemoved => if let Some(item) = edit.before.as_ref().and_then(|a| before.items.get(&a.item)) {
                            name_targets.insert((false, item.path.clone()), (item.text.clone(), identity::name_targets(item, &before)));
                            fact_pairs.push((id, false, item.clone()))
                        },
                        semilla_syntax::EditKind::ItemAdded => if let Some(item) = edit.after.as_ref().and_then(|a| after.items.get(&a.item)) {
                            name_targets.insert((true, item.path.clone()), (item.text.clone(), identity::name_targets(item, &after)));
                            fact_pairs.push((id, true, item.clone()))
                        },
                        _ => {}
                    }
                }
                outcomes.push(outcome);
            }
            Err(error) => {
                let step_report = StepReport { step: step.index, intent: step.intent.clone(), errors: vec![format!("{error:#}")], ..Default::default() };
                outcomes.push(StepOutcome { report: step_report, edits: Vec::new(), owners: Vec::new(), fact_of: Vec::new(), renames: Default::default(), bodies: Vec::new(), selections: Vec::new(), identity_reason: None, stated_causes: BTreeMap::new(), node_pairs: BTreeMap::new() });
            }
        }
    }
    if !set.steps.is_empty() && previous_tree != set.head_tree && !set.head_tree.is_empty() {
        // A difference in formatting only is allowed: the syntax diff ignores whitespace, but not
        // comments.
        let same = load(&previous_tree).and_then(|last| Ok((last, load(&set.head_tree)?))).is_ok_and(|(last, head)| {
            semilla_syntax::diff(&last, &head, &Default::default()).is_empty()
        });
        if same {
            report.warnings.push("the head tree differs from the last step only in formatting".to_string());
        } else {
            report.organization.push("the last step does not end at the head tree".to_string());
        }
    }
    report.steps = outcomes.iter().map(|outcome| outcome.report.clone()).collect();
    for step in &report.steps {
        for item in &step.spotlight {
            report.warnings.push(format!("spotlight (step {}): {item}", step.step));
        }
    }
    organize::check(set, &report.steps.clone(), &mut report, options);
    report.finish();
    report.pairs = pair_warnings_with(set, &report, &pair_items, &fact_pairs);
    report.pairs.retain(|pair| !pending_pairs.contains(&(pair.old.clone(), pair.new.clone())));
    // B5 of batch 5k: a pair with equal texts in which a name points to a different item. A
    // target that is itself a pair (it moves with the item) is not a different item.
    let moved: BTreeSet<(String, String)> = report.pairs.iter().map(|pair| (pair.old.clone(), pair.new.clone())).collect();
    for pair in report.pairs.iter_mut() {
        let (Some((old_text, old)), Some((new_text, new))) = (name_targets.get(&(false, pair.old.clone())), name_targets.get(&(true, pair.new.clone()))) else { continue };
        if old_text != new_text {
            continue;
        }
        for (name, target) in old {
            if let Some(other) = new.get(name).filter(|other| *other != target && !moved.contains(&(target.clone(), (*other).clone()))) {
                pair.resolves.push(semilla_core::report::PairName { name: name.clone(), old: target.clone(), new: other.clone() });
            }
        }
    }
    // Design 36.1: a pair warning is only for a pair that Semilla does not derive, and it gives
    // the cause. The note of the step has the cause when the check of the name map stopped
    // (`identity::derive`). Each other cause comes from the pair.
    let step_of = |id: &str| -> Option<usize> {
        let from_fact = id.strip_prefix("step ").and_then(|rest| rest.split(' ').next()).and_then(|index| index.parse().ok());
        from_fact.or_else(|| set.steps.iter().find(|step| step.claims.iter().any(|claim| claim.id == id)).map(|step| step.index))
    };
    let above_limit = report.steps.iter().flat_map(|step| &step.notes).any(|note| note.contains(": Semilla derives no rename or move above "));
    // With no derived facts (the claim mode of the tests), Semilla tries no derivation.
    for pair in report.pairs.iter_mut().filter(|_| options.derive_edits) {
        let from_note = report.steps.iter().flat_map(|step| &step.notes).find_map(|note| stopped_pair(note, &pair.old, &pair.new));
        let parent = |path: &str| path.rsplit_once("::").map(|(parent, _)| parent.to_string()).unwrap_or_default();
        let last = |path: &str| path.rsplit("::").next().unwrap_or(path).to_string();
        let steps = pair.claims.first().and_then(|id| step_of(id)).zip(pair.claims.get(1).and_then(|id| step_of(id)));
        // The step has the pair as a candidate, but the compare gave no body (a build that fails).
        let waits = report.steps.iter().flat_map(|step| &step.notes).any(|note| note.contains("phase 2 tries the derivation") && note.contains(&format!("`{}` → `{}`", pair.old, pair.new)));
        pair.stopped = from_note.or_else(|| match steps {
            _ if waits => Some("Semilla has no compare result for the step".to_string()),
            Some((removed, added)) if removed != added => Some(format!("step {removed} removes the first item and step {added} adds the second item, and Semilla derives a rename or a move in one step")),
            _ if parent(&pair.old) != parent(&pair.new) && last(&pair.old) != last(&pair.new) => Some("the name and the module are new: Semilla derives a rename in the same module, or a move with the same name".to_string()),
            _ if above_limit => Some(format!("the step has more than {} removed or added items of one kind in one module", identity::MAX_GROUP)),
            _ => Some("no name map of the step has this pair".to_string()),
        });
    }
    (report, outcomes)
}

/// The cause in the note of a derived rename or move that has no proof, when its step has no
/// other change (`identity::derive`). `None` for each other note.
fn stopped_pair(note: &str, old: &str, new: &str) -> Option<String> {
    let (pairs, rest) = note.split_once(" show(s) as removed and added: Semilla does not derive the ")?;
    let cause = rest.split_once(" (")?.1.strip_suffix(')')?;
    pairs.contains(&format!("`{old}` → `{new}`")).then(|| cause.to_string())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests68;
#[cfg(test)]
mod tests70;
#[cfg(test)]
mod tests71;
