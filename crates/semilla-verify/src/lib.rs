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

pub mod cfgcond;
pub mod checks;
pub mod edits;
pub mod facts;
pub mod files;
pub mod hints;
pub mod identity;
pub mod organize;
pub mod owners;
pub mod scope;
mod joint;
mod opaque;
pub use opaque::{KnownTrait, KNOWN_TRAITS};
mod srcconfig;
pub mod pattern;
pub mod views;

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::report::Effect;
use semilla_core::{Anchor, ChangeSet, Class, ClaimResult, ClaimStatus, Report, Step, StepReport, Uncovered};
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
    /// Returns `Some(true)` if the two expressions are proved equivalent.
    fn equivalent(&self, before: &str, after: &str, types: &BTreeMap<String, String>) -> Option<bool>;
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
    fn equivalent(&self, _: &str, _: &str, _: &BTreeMap<String, String>) -> Option<bool> {
        None
    }
}

/// The inputs of one step check.
pub struct StepInput<'a> {
    pub step: &'a Step,
    pub before: &'a Workspace,
    pub after: &'a Workspace,
}

/// Everything the verifier computed for one step. The views use it too.
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
    /// The feature selections that the check added for a proof (batch 5u, part C).
    pub selections: Vec<Selection>,
}

/// Checks one step with the old protocol: an edit with no claim and no type-level fact is
/// uncovered.
pub fn verify_step(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle) -> StepOutcome {
    verify_step_with(input, bodies, exprs, false)
}

/// Checks one step. With `derive_edits` (design 24.2), each edit with no claim gets a derived fact,
/// so no edit is uncovered, and a step with no identity claim can get a derived identity.
pub fn verify_step_with(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, derive_edits: bool) -> StepOutcome {
    // A reason with no text is not a reason (brief 5f, A4: an empty value proves nothing).
    let mut step = input.step.clone();
    step.reasons.retain(|_, reason| !reason.trim().is_empty());
    let input = &StepInput { step: &step, before: input.before, after: input.after };
    let plain = verify_core(input, bodies, exprs, derive_edits, false);
    if !derive_edits || input.step.claims.iter().any(is_identity) {
        return plain;
    }
    let outcome = identity::derive(input, bodies, exprs, plain);
    // The search for an identity does more checks, and each check sets its own list of added
    // configurations. The oracle must have the list of this result: the output shows it.
    let _ = bodies.set_selections(input.step, &outcome.selections);
    outcome
}

/// The check of one step. `derived_identity` is true when the identity claims of the step come
/// from the candidate search (design 24.2, rule 4), not from the agent.
pub(crate) fn verify_core(input: &StepInput, bodies: &dyn BodyOracle, exprs: &dyn ExprOracle, derive_edits: bool, derived_identity: bool) -> StepOutcome {
    let step = input.step;
    let oracle = bodies;
    let mut report = StepReport { step: step.index, intent: step.intent.clone(), declared_class: step.class, ..Default::default() };

    // 1. The renames from the identity claims, then the syntax edits.
    let renames = checks::renames(step, input.before);
    let edits = semilla_syntax::diff(input.before, input.after, &renames);

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
    let settled = loop {
        let truths = bodies.truths(step).unwrap_or_default();
        let cover = cfgcond::Cover::new(&[(input.before, &truths[0]), (input.after, &truths[1])]);
        let atoms = cover.atoms();
        match &scope {
            Some(scope) => scope.set(cover),
            None => scope = Some(cfgcond::enter(cover)),
        }
        // One run of the body check with a part of the rule on again (`cfgcond::Mode`).
        let run = |mode: &cfgcond::Mode| -> anyhow::Result<(Vec<BodyComparison>, joint::Bodies)> {
            cfgcond::with_mode(mode, || {
                let opaque = joint::Recorder::new(opaque::Opaque::new(input.before, input.after));
                let mut bodies = bodies.compare_with(step, &renames.items, &added, &opaque)?;
                // A call `cfg!(P)` with a covered condition: its value is opaque in each proof, and
                // each checked configuration has a proof.
                for body in bodies.iter_mut() {
                    // A `cfg!` call has the value of the unit that expands it: the file of the body.
                    let _at = body.span.as_ref().map(|(file, _)| cfgcond::at(file));
                    body.config.retain(|text| !cfgcond::covered_call(text));
                }
                // The code that a `cfg` attribute selects is not in the MIR (batch 5k, A1).
                srcconfig::apply(&mut bodies, input.before, input.after, &renames.items);
                opaque.opaque.apply(&mut bodies, &renames.items);
                Ok((bodies, opaque.bodies()))
            })
        };
        // The rule goes off for a proof only if the checked units have each valuation of the
        // atoms of the proof (batch 5t, part B).
        let settled = run(&cfgcond::Mode::Plain).and_then(|(plain, seen)| {
            let joint = joint::Joint { sides: [input.before, input.after], truths: &truths, bodies: &seen, renames: &renames.items };
            let property = step.claims.iter().any(|claim| semilla_core::ops::op(&claim.op).is_some_and(|info| info.kind == semilla_core::OpKind::Property));
            joint.settle(plain, &atoms, property, &selections, &|mode| run(mode).map(|(bodies, _)| bodies))
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
    let bodies = match settled {
        Ok(bodies) => bodies,
        Err(error) => {
            report.errors.push(format!("the body check failed: {error:#}"));
            Vec::new()
        }
    };
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

    // A neutral claim must have a body proof for its whole function. Other non-neutral claims in
    // the same function make that impossible.
    let non_neutral_items: BTreeSet<String> = results
        .iter()
        .filter(|(_, outcome)| outcome.class.is_some_and(|class| class != Class::Neutral))
        .filter_map(|(claim, _)| claim.at.as_ref().map(|anchor| anchor.item.clone()))
        .collect();

    for (claim, outcome) in &results {
        let mut status = outcome.status.clone();
        let mut hint = outcome.hint.clone();
        if let Some((identity, identity_hint)) = identity_status.as_ref().filter(|_| is_identity(claim) && outcome.status.is_verified()) {
            status = identity.clone();
            hint = identity_hint.clone();
        }
        if outcome.class == Some(Class::Neutral) && outcome.needs_body_proof {
            if let Some(item) = claim.at.as_ref().map(|anchor| anchor.item.as_str()) {
                if non_neutral_items.contains(item) && status.is_verified() {
                    status = ClaimStatus::Unproved { reason: "other claims change the same function in this step".into() };
                    hint = Some("Put the neutral claims of this function in their own step.".into());
                }
            }
        }
        if status.is_verified() && outcome.explains.is_empty() && !outcome.self_evident {
            status = ClaimStatus::Empty;
        }
        report.claims.push(ClaimResult { build_code: None,
            claim: claim.id.clone(),
            step: step.index,
            status,
            class: outcome.class,
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
            class: Some(Class::Other),
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
                    class: Some(Class::Logic),
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
                    format!("move-item (it looks like a move of `{old_name}` to `{new_name}`: anchor `{old_name}`, to={new_name}, in its own step)")
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
    let owner_input = owners::Input {
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
        callees: call_facts.as_ref(),
    };
    owners::assign(&owner_input, &mut report);
    owners::source_files(&owner_input, &mut report);
    // The class of a fact comes from the bodies that it owns. A fact with no changed body is
    // neutral only when its kind cannot change the behavior outside the MIR (invariant I3).
    if body_index.available {
        for fact in report.facts.iter_mut() {
            if !fact.affected.is_empty() {
                // A fact that changes a body changes the logic (a data file, a manifest key).
                if matches!(fact.class, None | Some(Class::Neutral) | Some(Class::Other)) {
                    // A proved rewrite of an item whose text is an input of other bodies (a
                    // file that `include_bytes!` reads): the text says why the fact is not
                    // neutral (batch 5n, A4).
                    if fact.class == Some(Class::Neutral) && fact.description.contains("neutral rewrite (proved") {
                        fact.description.push_str("; the edit changed other bodies (the affected bodies), so the fact is not neutral");
                    }
                    fact.class = Some(Class::Logic);
                }
            } else if fact.class == Some(Class::Neutral) && !facts::neutral_kind(fact) {
                // A member order changes the drop order, the derive output order, and the
                // discriminant values of an enum: effects outside the MIR records (batch 5e).
                fact.class = Some(if fact.part.starts_with("trait impl") || fact.part == "member order" { Class::Logic } else { Class::Other });
            }
        }
    }

    // Phase 1 has no MIR records, so the class of a file fact is not known (brief 5h, C2): a build
    // script or a macro can read each file, and only the comparison of the bodies shows the
    // effect. The fact shows as `[pending]` until phase 2 gives the class.
    if !body_index.available {
        for fact in report.facts.iter_mut().filter(|fact| fact.at.is_none() && fact.file.is_some() && fact.class == Some(Class::Other)) {
            fact.class = None;
        }
    }

    // A file that a crate reads at build time is a build input, also when its name is on the file
    // allow list (brief 5f, A6). With a changed body, the fact has the class `logic` and its
    // affected bodies. With no changed body, the effect is outside the bodies that Semilla
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
    // fact lists each item that names the changed item, and it is never neutral.
    for (index, edit) in edits.iter().enumerate().filter(|(_, edit)| facts::generic_order_changed(edit)) {
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
                    class: body_index.available.then_some(Class::Logic),
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
        if matches!(fact.class, Some(Class::Neutral) | Some(Class::Other)) {
            fact.class = Some(Class::Logic);
        }
    }

    // A fact whose changed bodies read the build configuration (batch 5k, A1). The check holds
    // for the configurations that Semilla compiles, and not for each build: the fact needs a
    // reason, and it is not neutral (its body changed).
    for fact in report.facts.iter_mut() {
        let own = fact.at.as_ref().map(|at| at.item.clone());
        let Some(text) = facts::config_spotlight(fact.affected.iter().chain(own.iter()), &body_index) else { continue };
        fact.spotlight_why = Some(match fact.spotlight_why.take() {
            Some(why) => format!("{why}; {text}"),
            None => text,
        });
        fact.spotlight = true;
        if matches!(fact.class, Some(Class::Neutral) | Some(Class::Other)) {
            fact.class = Some(Class::Logic);
        }
        if fact.reason.is_none() {
            fact.reason = match &fact.at {
                Some(at) => step.reasons.get(&at.to_string()).or_else(|| step.reasons.get(&at.item)).cloned(),
                None => step.reasons.get(&fact.part).cloned(),
            };
        }
    }

    // A fact whose changed bodies have a proof in only some of the checked configurations
    // (batch 5u, part A). The fact gets a note, and it needs no reason. Soundness: the combined
    // status of such a body is not `Equal`, so no rule makes the fact neutral from the body.
    // The guard below keeps this true for a fact that is neutral from a different cause.
    for fact in report.facts.iter_mut() {
        let own = fact.at.as_ref().map(|at| at.item.clone());
        let Some(note) = facts::config_note(fact.affected.iter().chain(own.iter()), &body_index) else { continue };
        fact.note = Some(note);
        if fact.class == Some(Class::Neutral) {
            fact.class = Some(Class::Logic);
        }
    }

    // A fact with a body whose proof Semilla removed because of a build condition, and with no
    // spotlight for it (batch 5u): the edit does not touch the code that reads the condition.
    // The fact gets a note with the cause. The note changes no class and no decision.
    for fact in report.facts.iter_mut() {
        let own = fact.at.as_ref().map(|at| at.item.clone());
        if facts::config_spotlight(fact.affected.iter().chain(own.iter()), &body_index).is_some() {
            continue;
        }
        let Some(note) = facts::stop_note(fact.affected.iter().chain(own.iter()), &body_index) else { continue };
        fact.note = Some(match fact.note.take() {
            Some(old) => format!("{old}; {note}"),
            None => note,
        });
    }

    // A fact with a name that has characters outside ASCII (batch 5k, D7). The name can look
    // the same as a different name, so the fact shows the escape form and needs a reason.
    for fact in report.facts.iter_mut() {
        let added: String = fact.content.as_deref().unwrap_or_default().lines().filter(|line| line.starts_with('+')).collect::<Vec<_>>().join("\n");
        let mut names: Vec<String> = Vec::new();
        for text in std::iter::once(fact.description.as_str()).chain(fact.sites.iter().map(|group| group.edit.as_str())).chain(std::iter::once(added.as_str())) {
            for (_, name) in facts::non_ascii_names(text) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        if names.is_empty() {
            continue;
        }
        fact.description = facts::with_ascii_forms(&fact.description);
        for group in fact.sites.iter_mut() {
            group.edit = facts::with_ascii_forms(&group.edit);
        }
        let list: Vec<String> = names.iter().map(|name| format!("`{name}` (`{}`)", semilla_core::text::ascii_form(name).unwrap_or_default())).collect();
        let text = format!("{}: {}", facts::NON_ASCII, list.join(", "));
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
    // Each changed body that reads the build configuration (batch 5k, A1).
    let config_items: BTreeSet<String> = body_index.config_names().map(|name| body_index.item_of_name(name).unwrap_or_else(|| name.clone())).collect();
    // The name of a body is not always the key of a reason (batch 5l, B3): the body of an
    // associated constant of a trait impl has the MIR name `<T as Trait>::C`, and the fact is
    // at the impl. The line gives each key that is not the name.
    let config_keys = |item: &String| -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for fact in report.facts.iter().filter(|fact| fact.spotlight_why.as_deref().is_some_and(|why| why.contains(" Uses: "))) {
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
        .map(|item| match config_keys(item) {
            keys if keys.is_empty() || keys == [item.clone()] => format!("build configuration: {item}"),
            keys => format!("build configuration: {item} (the key of the reason: {})", keys.iter().map(|key| format!("`{key}`")).collect::<Vec<_>>().join(", ")),
        })
        .collect();
    report.spotlight.extend(config_lines);
    report.spotlight.sort();
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
                if function && !compiled && !declared {
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

    report.computed_class = report.claims.iter().filter_map(|claim| claim.class).chain(report.facts.iter().filter_map(|fact| fact.class)).max_by_key(|class| class_rank(*class));
    if let (Some(declared), Some(computed)) = (step.class, report.computed_class) {
        if declared != computed {
            // Name the facts and the claims that give the class (M4).
            let mut causes: Vec<String> = report.facts.iter().filter(|f| f.class == Some(computed)).map(|f| format!("the fact \"{}\"", f.description)).collect();
            causes.extend(report.claims.iter().filter(|c| c.class == Some(computed)).map(|c| format!("the claim {}", c.claim)));
            let more = if causes.len() > 3 { format!(", and {} more", causes.len() - 3) } else { String::new() };
            causes.truncate(3);
            report.errors.push(format!("the step declares the class `{}`, but {} give(s) the class `{}`{more}", class_name(declared), causes.join(", "), class_name(computed)));
        }
    }

    // The final assertion (invariant I1): no path removes a changed body from the report.
    for name in owners::check(&owner_input, &report) {
        report.errors.push(format!("internal: a changed body has no owner: `{name}`"));
    }

    if derive_edits {
        edits::fold(&mut report.facts, &mut fact_of);
    }
    // Part B item 4 of batch 5j: a failed check build gives the same long error to `compare` and
    // to `facts`. The report shows each distinct error one time, complete.
    let mut seen = std::collections::BTreeSet::new();
    report.errors.retain(|error| seen.insert(error.clone()));
    StepOutcome { report, edits, owners, fact_of, renames, bodies, identity_reason, selections }
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
        ws.items.get(ty_path).is_some_and(|item| item.attrs.iter().any(|attr| attr.contains("derive") && attr.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == trait_name)))
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
    let split = |what: String| {
        Some((
            ClaimStatus::Unproved { reason: format!("an identity step must be neutral, but {what}: split the step") },
            // The reason ends with "split the step": the hint does not say it again (D10).
            Some("Put each rename and each move in its own step, with no other change. Then make the other changes in the next step.".to_string()),
        ))
    };
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
            if (in_body || initializer) && (equal_body(&item) || !body_index.available) {
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
    // A wider visibility of a moved item.
    for (index, edit) in edits.iter().enumerate() {
        if owners[index].is_some() || edit.kind != EditKind::Visibility {
            continue;
        }
        let moved = results.iter().position(|(claim, _)| claim.op == "move-item" && claim.arg_str("to").is_some_and(|_| edit.after.as_ref().is_some_and(|a| renames.items.get(&claim.at.as_ref().map(|at| at.item.clone()).unwrap_or_default()) == Some(&a.item))));
        if let Some(claim) = moved {
            if visibility_rank(edit.text_after.as_deref().unwrap_or("")) > visibility_rank(edit.text_before.as_deref().unwrap_or("")) {
                owners[index] = Some(claim);
            }
        }
    }
    if !body_index.available {
        return Some((ClaimStatus::Pending { reason: "the MIR proof of the identity step runs in the background check (phase 2)".into() }, None));
    }
    let other_claim = results.iter().find(|(claim, _)| !is_identity(claim) && claim.op != "edit-doc").map(|(claim, _)| format!("the step also has the claim {} (`{}`)", claim.id, claim.op));
    // B5: a doc change and a rename of a parameter do not stop the identity. Each one stays its
    // own fact (a doc fact has the class `other`). A parameter rename is allowed only when its
    // body is equal (N0 or N1): a parameter name is a local in the MIR.
    // A doc test change is not a plain doc change (brief 5h, B1): it stops the identity.
    let allowed = |index: usize, edit: &Edit| {
        edit.kind == EditKind::Comment
            || (edit.kind == EditKind::Docs && facts::doc_test_change(edit).is_none())
            || (facts::is_parameter_rename(edits, index, before, after) && edit.anchor().is_some_and(|a| equal_body(&renames.items.get(&a.item).cloned().unwrap_or_else(|| a.item.clone()))))
    };
    let other_edit = edits.iter().zip(owners.iter()).enumerate().find(|(index, (edit, owner))| owner.is_none() && !allowed(*index, edit)).map(|(_, (edit, _))| format!("the step also has the change {}", edit.anchor().map(|a| format!("at `{a}`")).or_else(|| edit.file.clone()).unwrap_or_default()));
    if let Some(name) = bad.first() {
        // "Split the step" is a route only when the step has a second change.
        if other_claim.is_some() || other_edit.is_some() {
            // A body with code for a different build configuration: its MIR can be equal.
            if !body_index.config(name).is_empty() {
                return split(format!("`{name}` has code that depends on the build configuration"));
            }
            return split(format!("the MIR of `{name}` changed"));
        }
        return Some((
            ClaimStatus::Unproved { reason: format!("the MIR of `{name}` changes with the identity claim, and `{name}` has no text edit and is not a derived body: the step is not neutral for it") },
            Some(format!("Look at `{name}`: if it uses the old name (in a string, `stringify!`, or a macro), the change is real. Then remove the identity claim: the items show as removed and added, with the changes as facts.")),
        ));
    }
    if let Some(what) = other_claim.or(other_edit) {
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
                ClaimStatus::Unproved { reason: format!("the derived output of {list} changes with the rename; give a reason with `--reason '{anchor}=<why>'`") },
                Some("A derived trait other than `Debug` (for example serde) can change a data format. Say why the change is correct.".into()),
            ));
        }
    }
    // The proof: the highest level, with the substitutions that the bodies at that level need.
    let level = bodies.iter().filter_map(|body| match &body.status {
        BodyStatus::Equal { level, .. } => Some(level.clone()),
        _ => None,
    });
    let level = level.max().unwrap_or_else(|| "n0".into());
    let mut rules: Vec<String> = Vec::new();
    for body in bodies {
        if let BodyStatus::Equal { level: found, proof } = &body.status {
            if *found == level {
                rules.extend(proof.iter().filter(|rule| !rules.contains(rule)).cloned().collect::<Vec<_>>());
            }
        }
    }
    let mut proof = level.to_uppercase();
    if !rules.is_empty() {
        proof = format!("{proof}: {}", rules.join(", "));
    }
    if !derived.is_empty() {
        let list = derived.iter().map(|(t, ty)| format!("{t} for {ty}")).collect::<Vec<_>>().join(", ");
        let count = derived.len();
        proof = format!("{proof}; neutral, but {count} derived {} the name ({list})", if count == 1 { "body shows" } else { "bodies show" });
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
            if old.kind != new.kind || old.path == new.path {
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
        warnings.push(semilla_core::PairWarning { old: old.path.clone(), new: new.path.clone(), similarity, claims: vec![removed, added], refusal, reviewer: None, resolves: Vec::new() });
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
/// `unwrap` and `expect` calls, `panic!`, `unreachable!`, `todo!`, `unimplemented!`, and
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
                "macro" => matches!(attr, "panic" | "unreachable" | "todo" | "unimplemented"),
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
        match edit.kind {
            EditKind::ItemRemoved if old.is_some() => major.push(format!("removed `{}`", old.unwrap())),
            EditKind::ItemAdded if new.is_some() => minor.push(format!("added `{}`", new.unwrap())),
            EditKind::Signature if old.is_some() || new.is_some() => major.push(format!("signature of `{}`", old.or(new).unwrap())),
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

/// The edits that always need the reviewer's attention (design 5.5).
pub fn spotlight(edits: &[Edit], before: &Workspace, after: &Workspace) -> Vec<String> {
    let mut found = BTreeSet::new();
    for edit in edits {
        let place = edit.anchor().map(|a| a.to_string()).or_else(|| edit.file.clone()).unwrap_or_default();
        let texts = [edit.text_before.as_deref(), edit.text_after.as_deref()];
        if texts.iter().flatten().any(|text| text.split(' ').any(|token| token == "unsafe")) || in_unsafe(edit, before, after) {
            found.insert(format!("unsafe code: {place}"));
        }
        // `#[cfg(test)]` only selects test code, so it is not in the spotlight.
        // The text of an attribute, not each use of the word `cfg` (a function with the name
        // `cfg` is not an attribute). A use of `cfg!` in a body gets the "build configuration"
        // spotlight from the MIR records.
        let cfg = |text: &str| {
            let text = text.replace(' ', "").replace("#[cfg(test)]", "");
            ["#[cfg(", "#![cfg(", "#[cfg_attr(", "#![cfg_attr("].iter().any(|start| text.contains(start))
        };
        if texts.iter().flatten().any(|text| cfg(text)) && matches!(edit.kind, semilla_syntax::EditKind::Attrs | semilla_syntax::EditKind::ItemAdded | semilla_syntax::EditKind::ItemRemoved) {
            found.insert(format!("cfg attribute: {place}"));
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

/// True if the edit is inside an `unsafe` block of a function body.
fn in_unsafe(edit: &Edit, before: &Workspace, after: &Workspace) -> bool {
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
    check(edit.before.as_ref(), before) || check(edit.after.as_ref(), after)
}

/// The rank of a class: a step has the highest class of its claims.
pub fn class_rank(class: Class) -> u8 {
    match class {
        Class::Neutral => 0,
        Class::Other => 1,
        Class::Logic => 2,
        Class::ControlFlow => 3,
        // No check: the rank of the highest class, so that it never hides below a real class.
        Class::NotChecked => 4,
    }
}

pub fn class_name(class: Class) -> &'static str {
    match class {
        Class::Neutral => "neutral",
        Class::Logic => "logic",
        Class::ControlFlow => "control_flow",
        Class::Other => "other",
        Class::NotChecked => "not checked",
    }
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
                let outcome = verify_step_with(&StepInput { step, before: &before, after: &after }, bodies, exprs, options.derive_edits);
                // Phase 1 has no MIR: the identity proof of a rename or a move waits for phase 2.
                // Phase 2 proves the identity (then the pair is one item), or it gives the pair
                // warning. Phase 1 gives no warning for such a pair.
                if outcome.report.identity_candidates && outcome.bodies.is_empty() && matches!(bodies.facts(step), Ok(None)) {
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
                outcomes.push(StepOutcome { report: step_report, edits: Vec::new(), owners: Vec::new(), fact_of: Vec::new(), renames: Default::default(), bodies: Vec::new(), selections: Vec::new(), identity_reason: None });
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
    (report, outcomes)
}

#[cfg(test)]
mod tests;
