//! The equivalence of two versions of each body (design 61, parts C, D, and E): the queries to
//! the oracle and the lines of the output.
//!
//! The oracle checks each body with an edit or a changed graph, in each checked configuration
//! (`semilla-smt`, `mir.rs`). This module asks for the checks, joins the results of the
//! configurations, and writes the lines: the end of the line of each body in the step section,
//! the `equivalence:` line of each fact, and the details of `status --verbose`.
//!
//! A body with more than one edit: each fact gets its own result when Semilla can isolate its
//! edit (part D). The variant of a fact is the old file with only the edits of the fact (an edit
//! of a different fact inside it keeps its old text). The oracle builds the variant and checks
//! the old body against the variant body.

use std::collections::{BTreeMap, BTreeSet};

use semilla_core::Category;
pub use semilla_core::run::RunNote;
use semilla_syntax::Workspace;

/// One check: a body in one configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub config: String,
    /// The record of each side: the name and the target.
    pub old: (String, String),
    pub new: (String, String),
    /// For the check of one fact (part D): the files of the old tree that the variant changes,
    /// with their full text. The oracle checks the old body against the body of the variant.
    pub variant: Option<BTreeMap<String, String>>,
}

/// The result of one check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A proof: the method (`equal MIR`, `SMT`, `SMT after inlining …`).
    Equivalent(String),
    /// An exact input and the two outcomes, and the state of the run of the real code that
    /// confirms it (design 74, R74.1-1).
    Different { input: String, before: String, after: String, run: RunNote },
    /// No proof and no exact input: the cause.
    NotDecided(String),
    /// No check: the cause (the budget of the step, design 61.14, R61.14-6).
    NotChecked(String),
}

/// The result words of a check, one for each kind of verdict (R66.10-6: a cut of a line keeps
/// them).
pub(crate) fn result_words() -> Vec<&'static str> {
    let empty = String::new;
    [Verdict::Equivalent(empty()), Verdict::Different { input: empty(), before: empty(), after: empty(), run: RunNote::None }, Verdict::NotDecided(empty()), Verdict::NotChecked(empty())].iter().map(Verdict::word).collect()
}

/// The cause of a check that the budget of the step stopped (R61.14-6).
pub const BUDGET_CAUSE: &str = "the budget of the step";

/// [R77.2-3] The cause of a check that the safety time limit stopped (the text of
/// `semilla_smt::budget::SAFETY_CAUSE`): a separate cause, never the cause of the budget.
pub const SAFETY_CAUSE: &str = "the safety time limit";

/// The answer of the oracle to one query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub verdict: Verdict,
    /// The callees that the check inlined.
    pub inlined: Vec<String>,
    /// The count of the solver queries.
    pub queries: usize,
    pub millis: u128,
    /// The body-local result when the program result differs from it (design 61.10, defect
    /// 1): `same body code; changed callee: shop::tier`. Only for `status --verbose`, and for
    /// the line of a body with no other result.
    pub local: Option<String>,
}

/// One body that the walk of the reverse call graph reached (design 61.10, part A).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reached {
    /// The name of the body (the new name; the old name for a removed body).
    pub name: String,
    /// The changed callees of the body: the cause of the reach.
    pub through: Vec<String>,
    /// The chain from the body to a changed body of the step (the first path of the walk).
    pub chain: Vec<String>,
    /// The result in each configuration that checked the body.
    pub results: Vec<(String, Verdict)>,
}

/// The walk of the reverse call graph of a step (design 61.10, part A).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReachReport {
    /// The reached bodies, in the order of the walk.
    pub bodies: Vec<Reached>,
    /// The limits that stopped the walk, with the count of the callers that it did not check.
    pub stops: Vec<String>,
    /// For each changed body of the step: the reached bodies that the change can reach (each
    /// reached body that is not equivalent in each configuration, through a chain of such
    /// bodies).
    pub from: BTreeMap<String, Vec<String>>,
}

impl Reached {
    /// The text of the result (design 61.10, A3): one text if each configuration agrees, else
    /// each result with its configurations, the worst first. The second text is the fold key of
    /// the step section: the result with no input and no outcome.
    pub fn text(&self, place: &dyn Fn(&str) -> (String, String)) -> (String, String) {
        let through = self.through.join(", ");
        let mut groups: Vec<(Verdict, Vec<String>)> = Vec::new();
        for (config, verdict) in &self.results {
            match groups.iter_mut().find(|(known, _)| known == verdict) {
                Some((_, configs)) => {
                    if !configs.contains(config) {
                        configs.push(config.clone());
                    }
                }
                None => groups.push((verdict.clone(), vec![config.clone()])),
            }
        }
        groups.sort_by_key(|(verdict, _)| verdict.rank());
        let one = groups.len() == 1;
        let mut texts = Vec::new();
        let mut keys = Vec::new();
        for (verdict, configs) in &groups {
            let configs = if one { String::new() } else { format!(" in {}", configs.join(", ")) };
            let (text, key) = match verdict {
                Verdict::Different { input, before, after, run } => (format!("different through {through}{configs}: {}: {}{}", for_input(input), outcomes(before, after), run_text(run, false)), format!("different through {through}{configs}")),
                // A proof with each changed callee inlined (the check never treats a changed
                // callee as an effect).
                Verdict::Equivalent(_) => (format!("equivalent{configs} (proved after inlining {through})"), format!("equivalent{configs} (proved after inlining {through})")),
                Verdict::NotDecided(cause) => {
                    let (shown, key) = place(cause);
                    (format!("reached through {through}, not decided{configs} ({shown})"), format!("reached through {through}, not decided{configs} ({key})"))
                }
                Verdict::NotChecked(cause) => (format!("reached through {through}, not checked{configs} ({cause})"), format!("reached through {through}, not checked{configs} ({cause})")),
            };
            texts.push(text);
            keys.push(key);
        }
        (texts.join("; "), keys.join("; "))
    }

    /// True if the body is equivalent in each configuration: the walk stops at it.
    pub fn equivalent(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|(_, verdict)| matches!(verdict, Verdict::Equivalent(_)))
    }
}

impl Verdict {
    fn word(&self) -> &'static str {
        match self {
            Verdict::Equivalent(_) => "equivalent",
            Verdict::Different { .. } => "different",
            Verdict::NotDecided(_) => "not decided",
            Verdict::NotChecked(_) => "not checked",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Verdict::Different { .. } => 0,
            Verdict::NotDecided(_) => 1,
            Verdict::NotChecked(_) => 2,
            Verdict::Equivalent(_) => 3,
        }
    }

    /// The rest of the text after the word, in the form of a fact line (`: for …`) or of the
    /// step line (` (for …)`).
    fn rest(&self, step: bool) -> String {
        match self {
            Verdict::Equivalent(method) => format!(" (proved by {method})"),
            Verdict::Different { input, before, after, run } if step => format!(" ({}: {}{})", for_input(input), outcomes(before, after), run_text(run, true)),
            Verdict::Different { input, before, after, run } => format!(": {}: {}{}", for_input(input), outcomes(before, after), run_text(run, false)),
            Verdict::NotDecided(cause) if step => format!(" ({cause})"),
            Verdict::NotDecided(cause) => format!(": {cause}"),
            // A short text on each line: the step section has the line of the budget.
            Verdict::NotChecked(cause) => format!(" ({cause})"),
        }
    }
}

/// The two outcomes of a different result: "before returns 7, after returns 6". R71.1-1 (the
/// element form; R68.6-3 before batch 11c): two returned arrays of one length give their
/// changed elements, not the full arrays: "before and after return arrays that differ at [2]
/// 3 → 9 (1 of 8 elements differ)".
pub(crate) fn outcomes(before: &str, after: &str) -> String {
    if let (Some(old), Some(new)) = (before.strip_prefix("returns "), after.strip_prefix("returns "))
        && let Some(change) = crate::facts::array_change(old, new)
    {
        return format!("before and after return arrays that differ at {change}");
    }
    format!("before {before}, after {after}")
}

/// [R74.1-1] The text of the run of a difference after its outcomes: "; confirmed by a run" in
/// the step line, " (confirmed by a run)" in a fact line. Empty with no data of a run.
fn run_text(run: &RunNote, step: bool) -> String {
    let text = run.text();
    if text.is_empty() {
        String::new()
    } else if step {
        format!("; {text}")
    } else {
        format!(" ({text})")
    }
}

/// The words of an input: `for x = 4`, or `for each input` for a body with no parameter.
fn for_input(input: &str) -> String {
    if input == "each input" { "for each input".into() } else { format!("for {input}") }
}

/// The text of the results of the configurations of one body: one result if each
/// configuration agrees, else each result with its configurations (the worst first).
pub fn joined(results: &[(String, Verdict)], step: bool, alone: bool) -> String {
    let mut groups: Vec<(Verdict, Vec<String>)> = Vec::new();
    for (config, verdict) in results {
        match groups.iter_mut().find(|(known, _)| known == verdict) {
            Some((_, configs)) => {
                if !configs.contains(config) {
                    configs.push(config.clone());
                }
            }
            None => groups.push((verdict.clone(), vec![config.clone()])),
        }
    }
    groups.sort_by_key(|(verdict, _)| verdict.rank());
    let suffix = if alone { " alone" } else { "" };
    if groups.len() == 1 {
        let (verdict, _) = &groups[0];
        return format!("{}{suffix}{}", verdict.word(), verdict.rest(step));
    }
    groups.iter().map(|(verdict, configs)| format!("{}{suffix} in {}{}", verdict.word(), configs.join(", "), verdict.rest(step))).collect::<Vec<_>>().join("; ")
}

/// One body with a line in the step section, for the check.
pub(crate) struct BodyCheck {
    /// The index of the body in the step section.
    pub index: usize,
    /// The new item and the old item of the body.
    pub item: Option<String>,
    pub old_item: Option<String>,
    /// The MIR name of the body.
    pub name: String,
    /// The name in the output.
    pub shown: String,
    /// Each configuration and pair of targets: the record keys of the two sides.
    pub pairs: Vec<(String, (String, String), (String, String))>,
    /// The configurations with an equal MIR text (level `n0`) for each pair: no solver query.
    pub equal: bool,
}

/// A changed instance of a generic body, or a changed candidate set of a `dyn` call (part B).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstanceLine {
    /// The generic body, or `dyn TRAIT`.
    pub body: String,
    /// The text of the line.
    pub text: String,
    /// The names that the old side and the new side have and the other side has not (the
    /// last segment of each type): a fact that names one of each is the cause.
    pub names: [Vec<String>; 2],
}

/// The names of the type arguments of an instance text (`shop::charge::<shop::Fuel>` gives
/// `Fuel`), or of a type text: the last segment of each path.
pub fn type_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut last = String::new();
    let inner = text.find("::<").map_or(text, |at| &text[at..]);
    for c in inner.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            if !word.is_empty() {
                last = std::mem::take(&mut word);
            }
            if c != ':' && !last.is_empty() {
                if last.chars().next().is_some_and(char::is_uppercase) && !out.contains(&last) {
                    out.push(last.clone());
                }
                last.clear();
            }
        }
    }
    out
}

/// Adds the lines of the changed instances (part B): a line under each fact that names an old
/// type and a new type of the line (the cause), and a line in the step section.
pub(crate) fn instance_lines(outcome: &mut crate::StepOutcome, lines: Vec<InstanceLine>) {
    if lines.is_empty() {
        return;
    }
    let mut flow = outcome.report.flow.take().unwrap_or_default();
    let word = |text: &str, name: &str| text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|part| part == name);
    for line in &lines {
        flow.bodies.push(semilla_core::report::FlowBody { name: line.body.clone(), place: "no place".into(), line: line.text.clone(), details: Vec::new(), chart: None, list: Vec::new() });
        for fact in &outcome.report.facts {
            let text = &fact.description;
            if line.names[0].iter().any(|name| word(text, name)) && line.names[1].iter().any(|name| word(text, name)) {
                let found = flow.facts.entry(fact.id.clone()).or_default();
                if !found.contains(&line.text) {
                    found.push(line.text.clone());
                }
            }
        }
    }
    outcome.report.flow = Some(flow);
}

/// The most characters of the source text of a cause.
const CAUSE_TEXT: usize = 48;

/// A cause with the source text and the line of its place (design 61.10, C2):
/// `a closure value: |x| *x > 9 (src/lib.rs:2)`; a place of the old tree has the line `-N`. The
/// second text is the cause with no place (the fold key). A cause with no mark stays.
pub fn cause_text(cause: &str, before: &Workspace, after: &Workspace) -> (String, String) {
    let Some((part, mark)) = cause.split_once(&format!(" {}", semilla_smt_mark())) else { return (cause.to_string(), cause.to_string()) };
    let fields: Vec<&str> = mark.splitn(4, '|').collect();
    let [side, path, start, end] = fields[..] else { return (part.to_string(), part.to_string()) };
    let workspace = if side == "o" { before } else { after };
    let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) else { return (part.to_string(), part.to_string()) };
    let Some(text) = workspace.rust_files.get(path) else { return (part.to_string(), part.to_string()) };
    let Some(piece) = text.get(start..end.max(start)) else { return (part.to_string(), part.to_string()) };
    let line = text[..start].matches('\n').count() + 1;
    let mut snippet: String = piece.split_whitespace().collect::<Vec<_>>().join(" ");
    if snippet.chars().count() > CAUSE_TEXT {
        snippet = format!("{}…", snippet.chars().take(CAUSE_TEXT).collect::<String>());
    }
    let sign = if side == "o" { "-" } else { "" };
    let shown = if snippet.is_empty() { format!("{part} ({path}:{sign}{line})") } else { format!("{part}: {snippet} ({path}:{sign}{line})") };
    (shown, part.to_string())
}

/// The mark of a place in a cause of the check (`semilla-smt`, `mir.rs`).
fn semilla_smt_mark() -> &'static str {
    "@@"
}

/// The most variant builds of one step (part D).
pub const MAX_VARIANTS: usize = 8;

/// True if a fact can change the code of a body: each category but the docs and the layout.
fn code_fact(fact: &semilla_core::Fact) -> bool {
    !matches!(fact.category, Category::Docs | Category::Layout)
}

/// True if a position of a fact is in the lines of an item on one side.
fn in_item(fact: &semilla_core::Fact, side: usize, workspace: &Workspace, item: &str) -> bool {
    let Some(found) = workspace.items.get(item) else { return false };
    fact.positions.iter().any(|position| position.old == (side == 0) && position.file == found.file && position.start <= found.line_end && found.line_start <= position.end)
}

/// Asks the oracle for the checks of the bodies with a line, and adds the lines of the output.
pub(crate) fn apply(input: &crate::StepInput, oracle: &dyn crate::BodyOracle, outcome: &mut crate::StepOutcome, checks: &[BodyCheck]) {
    let Some(mut flow) = outcome.report.flow.take() else { return };
    let (before, after) = (input.before, input.after);
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    // The facts of each body: a fact with a position in the lines of the item and an edit in
    // the item.
    let has_edit = |fact: usize, check: &BodyCheck| {
        outcome.edits.iter().enumerate().any(|(index, edit)| {
            outcome.fact_of.get(index).copied().flatten() == Some(fact) && [(&edit.before, &check.old_item), (&edit.after, &check.item)].iter().any(|(anchor, item)| anchor.as_ref().zip(item.as_ref()).is_some_and(|(anchor, item)| &anchor.item == item))
        })
    };
    let facts_of: Vec<Vec<usize>> = checks
        .iter()
        .map(|check| {
            (0..outcome.report.facts.len())
                .filter(|index| {
                    let fact = &outcome.report.facts[*index];
                    code_fact(fact) && (check.item.as_ref().is_some_and(|item| in_item(fact, 1, after, item)) || check.old_item.as_ref().is_some_and(|item| in_item(fact, 0, before, item))) && has_edit(*index, check)
                })
                .collect()
        })
        .collect();
    // The edits of each body: a fact, or a group of facts that only add or remove nodes with one
    // parent node (a deleted statement and an added statement at its place: one replacement).
    let groups_of: Vec<Vec<Vec<usize>>> = checks.iter().zip(&facts_of).map(|(check, facts)| groups(outcome, check, facts)).collect();
    // The queries: each pair of each body, then the variants of the facts of a body with more
    // than one edit.
    let mut queries: Vec<Query> = Vec::new();
    let mut owner: Vec<(usize, Option<usize>)> = Vec::new();
    for (index, check) in checks.iter().enumerate() {
        for (config, old, new) in &check.pairs {
            queries.push(Query { config: config.clone(), old: old.clone(), new: new.clone(), variant: None });
            owner.push((index, None));
        }
    }
    let mut variants = 0;
    let mut variant_cause: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for (index, check) in checks.iter().enumerate() {
        if groups_of[index].len() < 2 || check.equal {
            continue;
        }
        for group in &groups_of[index] {
            if group.len() > 1 {
                for fact in group {
                    variant_cause.insert((index, *fact), "the edit adds or removes a node".into());
                }
                continue;
            }
            let fact = &group[0];
            match variant(input, outcome, *fact, check) {
                Ok(files) if variants < MAX_VARIANTS => {
                    variants += 1;
                    // The variant in each configuration of the body.
                    let mut seen = BTreeSet::new();
                    for (config, old, _) in &check.pairs {
                        if !seen.insert(config.clone()) {
                            continue;
                        }
                        queries.push(Query { config: config.clone(), old: old.clone(), new: old.clone(), variant: Some(files.clone()) });
                        owner.push((index, Some(*fact)));
                    }
                }
                Ok(_) => {
                    variant_cause.insert((index, *fact), format!("the step has more than {MAX_VARIANTS} variants"));
                }
                Err(cause) => {
                    variant_cause.insert((index, *fact), cause);
                }
            }
        }
    }
    if queries.is_empty() {
        outcome.report.flow = Some(flow);
        reach_lines(input, oracle, outcome, checks, &facts_of);
        return;
    }
    let mut answers = oracle.equivalence(input.step, &renames, &queries);
    // The source text and the line of each cause (design 61.10, C2).
    for answer in answers.iter_mut().flatten() {
        if let Verdict::NotDecided(cause) = &answer.verdict {
            answer.verdict = Verdict::NotDecided(cause_text(cause, before, after).0);
        }
    }
    // The results of each body and of each fact alone.
    let mut body_results: Vec<Vec<(String, Verdict, Option<Answer>)>> = vec![Vec::new(); checks.len()];
    let mut fact_results: BTreeMap<(usize, usize), Vec<(String, Verdict)>> = BTreeMap::new();
    for ((query, (index, fact)), answer) in queries.iter().zip(&owner).zip(answers) {
        let Some(answer) = answer else { continue };
        match fact {
            None => body_results[*index].push((query.config.clone(), answer.verdict.clone(), Some(answer))),
            Some(fact) => fact_results.entry((*index, *fact)).or_default().push((query.config.clone(), answer.verdict.clone())),
        }
    }
    let mut fact_lines: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (index, check) in checks.iter().enumerate() {
        let results = &body_results[index];
        if results.is_empty() {
            continue;
        }
        let plain: Vec<(String, Verdict)> = results.iter().map(|(config, verdict, _)| (config.clone(), verdict.clone())).collect();
        // [R77.1-2] The results of the body for the coverage of the claims.
        outcome.verdicts.entry(flow.bodies[check.index].name.clone()).or_default().extend(plain.iter().cloned());
        let body = &mut flow.bodies[check.index];
        body.line.push_str(&format!("; {}", joined(&plain, true, false)));
        // `status --verbose`: the method, the inlined callees, the time, and the query count.
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        for (config, verdict, answer) in results {
            let Some(answer) = answer else { continue };
            let text = format!("{}{}", verdict.word(), verdict.rest(false));
            if !seen.insert((config.clone(), text.clone())) {
                continue;
            }
            let inlined = if answer.inlined.is_empty() { "no callee inlined".to_string() } else { format!("inlined: {}", answer.inlined.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")) };
            // The body-local result (design 61.10, defect 1): only here.
            let local = answer.local.as_ref().map(|local| format!("; body-local: {local}")).unwrap_or_default();
            body.details.push(format!("equivalence (in {config}): {text}{local}; {inlined}; {} solver quer{}; {} ms", answer.queries, if answer.queries == 1 { "y" } else { "ies" }, answer.millis));
        }
        let edits = groups_of[index].len();
        for fact in &facts_of[index] {
            let id = outcome.report.facts[*fact].id.clone();
            let text = if edits <= 1 {
                joined(&plain, false, false)
            } else {
                let alone = match fact_results.get(&(index, *fact)) {
                    Some(found) => joined(found, false, true),
                    None => format!("not decided alone (the body has {edits} edits{})", variant_cause.get(&(index, *fact)).map(|cause| format!("; {cause}")).unwrap_or_default()),
                };
                format!("{alone}; the body: {}", joined(&plain, false, false))
            };
            let lines = fact_lines.entry(id).or_default();
            let line = if checks.iter().enumerate().filter(|(other, _)| facts_of[*other].contains(fact) && !body_results[*other].is_empty()).count() > 1 { format!("`{}`: {text}", check.shown) } else { text };
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    flow.equivalence = fact_lines;
    // R71.2-1: the result of an item adds up over the bodies of the item. The line of the body
    // of the item names a worse result of a nested body (a closure, a constant block): "with its
    // nested bodies: different in `shop::f::{closure#0}`". Each body keeps its own result.
    let worst = |results: &[(String, Verdict, Option<Answer>)]| results.iter().map(|(_, verdict, _)| verdict.rank()).min();
    for (index, check) in checks.iter().enumerate() {
        let Some(own) = worst(&body_results[index]) else { continue };
        let prefix = format!("{}::{{", check.name);
        let nested: Vec<(u8, &str, &'static str)> = checks
            .iter()
            .enumerate()
            .filter(|(other, found)| *other != index && found.item.is_some() && found.item == check.item && found.name.starts_with(&prefix))
            .filter_map(|(other, found)| {
                let rank = worst(&body_results[other])?;
                let word = body_results[other].iter().find(|(_, verdict, _)| verdict.rank() == rank).map(|(_, verdict, _)| verdict.word())?;
                Some((rank, found.name.as_str(), word))
            })
            .collect();
        let Some(best) = nested.iter().map(|(rank, _, _)| *rank).min().filter(|rank| *rank < own) else { continue };
        let names: Vec<String> = nested.iter().filter(|(rank, _, _)| *rank == best).map(|(_, name, _)| format!("`{name}`")).collect();
        let word = nested.iter().find(|(rank, _, _)| *rank == best).map(|(_, _, word)| *word).unwrap_or_default();
        flow.bodies[check.index].line.push_str(&format!("; with its nested bodies: {word} in {}", names.join(", ")));
    }
    // One line for the step when the budget stopped a check (R61.14-6). [R77.2-3] The safety time
    // limit has its own line.
    let safety = |verdict: &Verdict| matches!(verdict, Verdict::NotChecked(cause) if cause == SAFETY_CAUSE);
    let by_safety = body_results.iter().filter(|results| results.iter().any(|(_, verdict, _)| safety(verdict))).count() + fact_results.values().filter(|results| results.iter().any(|(_, verdict)| safety(verdict))).count();
    let stopped = |verdict: &Verdict| matches!(verdict, Verdict::NotChecked(cause) if cause == BUDGET_CAUSE);
    let bodies = body_results.iter().filter(|results| results.iter().any(|(_, verdict, _)| stopped(verdict))).count();
    let alone = fact_results.values().filter(|results| results.iter().any(|(_, verdict)| stopped(verdict))).count();
    if bodies + alone > 0 {
        let seconds = oracle.equivalence_budget(input.step).map(|parts| format!(" at its budget ({parts})")).unwrap_or_default();
        let mut parts = Vec::new();
        if bodies > 0 {
            parts.push(format!("{bodies} bod{} not checked", if bodies == 1 { "y" } else { "ies" }));
        }
        if alone > 0 {
            parts.push(format!("{alone} fact{} not checked alone", if alone == 1 { "" } else { "s" }));
        }
        flow.equivalence_stop = Some(format!("the equivalence check stopped{seconds}: {}; {}", parts.join(", "), semilla_core::budget::step_text_command(outcome.report.step)));
    }
    if by_safety > 0 {
        let line = format!("the safety time limit stopped the equivalence check: {by_safety} result(s) not checked (the safety time limit); semilla status --verbose");
        flow.equivalence_stop = Some(match flow.equivalence_stop.take() {
            Some(found) => format!("{found}; {line}"),
            None => line,
        });
    }
    // [R74.1-2] One line for the step when a run of the real code disagrees with the model.
    let defect = |verdict: &Verdict| matches!(verdict, Verdict::NotDecided(cause) if cause.starts_with(semilla_core::run::DISAGREE));
    flow.run_defects += body_results.iter().filter(|results| results.iter().any(|(_, verdict, _)| defect(verdict))).count() + fact_results.values().filter(|results| results.iter().any(|(_, verdict)| defect(verdict))).count();
    outcome.report.flow = Some(flow);
    reach_lines(input, oracle, outcome, checks, &facts_of);
}

/// The most names of a `reaches:` line of a fact.
const REACH_NAMES: usize = 3;

/// Adds the lines of the walk of the reverse call graph (design 61.10, A5): the part `reached
/// bodies:` of the step section, the `reaches:` line of each fact whose body is in the start
/// set, and the reached public functions of each fact for the Markdown review.
fn reach_lines(input: &crate::StepInput, oracle: &dyn crate::BodyOracle, outcome: &mut crate::StepOutcome, checks: &[BodyCheck], facts_of: &[Vec<usize>]) {
    let renames: BTreeMap<String, String> = outcome.renames.items.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    let Some(report) = oracle.reach(input.step, &renames) else { return };
    if report.bodies.is_empty() && report.stops.is_empty() {
        return;
    }
    let Some(mut flow) = outcome.report.flow.take() else { return };
    let (before, after) = (input.before, input.after);
    let index = crate::checks::BodyIndex::new(&outcome.bodies, after, before);
    // The item of a MIR name: its place and its visibility.
    let item_of = |name: &str| -> Option<(&semilla_syntax::Item, bool)> {
        let path = index.item_of_name(name).unwrap_or_else(|| name.to_string());
        after.items.get(&path).map(|item| (item, true)).or_else(|| before.items.get(&path).map(|item| (item, false)))
    };
    let place_of = |name: &str| item_of(name).map(|(item, new)| format!("{}:{}{}", item.file, if new { "" } else { "-" }, item.line_start)).unwrap_or_else(|| "no place".to_string());
    let public = |name: &str| item_of(name).is_some_and(|(item, _)| item.vis == "pub" && matches!(item.kind, semilla_syntax::ItemKind::Fn));
    let place = |cause: &str| cause_text(cause, before, after);
    for body in &report.bodies {
        // [R74.1-2] A reached body whose run disagrees with the model.
        flow.run_defects += usize::from(body.results.iter().any(|(_, verdict)| matches!(verdict, Verdict::NotDecided(cause) if cause.starts_with(semilla_core::run::DISAGREE))));
        let (text, key) = body.text(&place);
        flow.reached.push(semilla_core::report::ReachedBody { name: body.name.clone(), place: place_of(&body.name), text, chain: body.chain.join(" → "), key });
    }
    flow.reach_stops = report.stops.clone();
    // R76.1-1: the part `reached bodies` of the full text of the step has each body.
    let full_step = semilla_core::budget::step_text_command(outcome.report.step);
    for (check, facts) in checks.iter().zip(facts_of) {
        let Some(list) = report.from.get(&check.name) else { continue };
        let shown: Vec<&String> = list.iter().filter(|name| public(name)).collect();
        let line = if shown.is_empty() {
            let names: Vec<&str> = list.iter().take(REACH_NAMES).map(String::as_str).collect();
            let more = if list.len() > REACH_NAMES { format!(", and {} more", list.len() - REACH_NAMES) } else { String::new() };
            format!("{} bod{}, no public function: {}{more} ({full_step})", list.len(), if list.len() == 1 { "y" } else { "ies" }, names.join(", "))
        } else {
            let names: Vec<&str> = shown.iter().take(REACH_NAMES).map(|name| name.as_str()).collect();
            let more = if shown.len() > REACH_NAMES { format!(", and {} more", shown.len() - REACH_NAMES) } else { String::new() };
            format!("{} public function{}: {}{more} ({full_step})", shown.len(), if shown.len() == 1 { "" } else { "s" }, names.join(", "))
        };
        let links: Vec<(String, String)> = shown.iter().map(|name| ((*name).clone(), place_of(name))).collect();
        for fact in facts {
            let id = outcome.report.facts[*fact].id.clone();
            let entry = flow.reaches.entry(id).or_insert_with(|| (String::new(), Vec::new()));
            if entry.0.is_empty() {
                *entry = (line.clone(), links.clone());
            } else if !entry.0.contains(&line) {
                entry.0.push_str(&format!("; `{}`: {line}", check.shown));
                entry.1.extend(links.iter().cloned());
            }
        }
    }
    outcome.report.flow = Some(flow);
}

/// The edits of a body: each fact is one edit, but the facts that only add or only remove nodes
/// with one parent node are one edit together (Decided by Claude: a replacement can give a
/// deleted statement and an added statement).
fn groups(outcome: &crate::StepOutcome, check: &BodyCheck, facts: &[usize]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut by_parent: BTreeMap<String, usize> = BTreeMap::new();
    for fact in facts {
        let mut parents: BTreeSet<String> = BTreeSet::new();
        let mut plain = true;
        for (index, edit) in outcome.edits.iter().enumerate() {
            if outcome.fact_of.get(index).copied().flatten() != Some(*fact) {
                continue;
            }
            let in_body = [(&edit.before, &check.old_item), (&edit.after, &check.item)].iter().any(|(anchor, item)| anchor.as_ref().zip(item.as_ref()).is_some_and(|(anchor, item)| &anchor.item == item));
            if !in_body {
                continue;
            }
            match (&edit.before, &edit.after) {
                (Some(anchor), None) | (None, Some(anchor)) => {
                    let text = anchor.to_string();
                    parents.insert(text.rsplit_once('/').map_or(text.clone(), |(parent, _)| parent.to_string()));
                }
                _ => plain = false,
            }
        }
        let parent = (!plain || parents.len() != 1).then_some(()).map_or_else(|| parents.into_iter().next(), |_| None);
        match parent.and_then(|parent| by_parent.get(&parent).copied().map(|at| (parent.clone(), at)).or(Some((parent, usize::MAX)))) {
            Some((_, at)) if at != usize::MAX => out[at].push(*fact),
            Some((parent, _)) => {
                by_parent.insert(parent, out.len());
                out.push(vec![*fact]);
            }
            None => out.push(vec![*fact]),
        }
    }
    out
}

/// True for the text of one identifier.
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_') && text != "_"
}

/// Replaces each whole identifier `from` in a text with `to`.
fn replace_word(text: &str, from: &str, to: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = text[at..].find(from) {
        let start = at + found;
        let end = start + from.len();
        let before = text[..start].chars().next_back().is_some_and(word);
        let after = text[end..].chars().next().is_some_and(word);
        out.push_str(&text[at..start]);
        out.push_str(if before || after { from } else { to });
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// The files of the variant of one fact: the old file with only the edits of the fact. An edit
/// of a different fact inside an edit of the fact keeps its old text. `Err`: the cause when
/// Semilla cannot isolate the edit.
///
/// The sites of one rename go together (Decided by Claude): an edit whose old node and new node
/// are one identifier each is a rename site. The variant of a fact with only rename sites
/// renames each whole identifier of the old body (the full rename). The variant of a different
/// fact keeps the old name of each rename of the body in its new text.
fn variant(input: &crate::StepInput, outcome: &crate::StepOutcome, fact: usize, check: &BodyCheck) -> Result<BTreeMap<String, String>, String> {
    let (before, after) = (input.before, input.after);
    // The old range, the new range, and the file of an edit.
    let range = |edit: &semilla_syntax::Edit| -> Option<(String, (usize, usize), (usize, usize))> {
        let old = edit.before.as_ref()?;
        let new = edit.after.as_ref()?;
        let (Some(semilla_syntax::Resolved::Node(old_item, old_node)), Some(semilla_syntax::Resolved::Node(new_item, new_node))) = (semilla_syntax::resolve(before, old), semilla_syntax::resolve(after, new)) else { return None };
        if old_item.file != new_item.file {
            return None;
        }
        Some((old_item.file.clone(), old_node.span, new_node.span))
    };
    // The rename sites of the body: an old identifier and a new identifier.
    let rename_of = |edit: &semilla_syntax::Edit| -> Option<(String, String)> {
        let (Some(semilla_syntax::Resolved::Node(_, old)), Some(semilla_syntax::Resolved::Node(_, new))) = (semilla_syntax::resolve(before, edit.before.as_ref()?), semilla_syntax::resolve(after, edit.after.as_ref()?)) else { return None };
        (is_identifier(&old.text) && is_identifier(&new.text) && old.text != new.text).then(|| (old.text.clone(), new.text.clone()))
    };
    let mut own = Vec::new();
    let mut others = Vec::new();
    let mut own_renames: BTreeSet<(String, String)> = BTreeSet::new();
    let mut own_other = false;
    let mut renames: BTreeSet<(String, String)> = BTreeSet::new();
    for (index, edit) in outcome.edits.iter().enumerate() {
        let in_body = [(&edit.before, &check.old_item), (&edit.after, &check.item)].iter().any(|(anchor, item)| anchor.as_ref().zip(item.as_ref()).is_some_and(|(anchor, item)| &anchor.item == item));
        if !in_body {
            continue;
        }
        let rename = rename_of(edit);
        if let Some(pair) = &rename {
            renames.insert(pair.clone());
        }
        if outcome.fact_of.get(index).copied().flatten() == Some(fact) {
            match &rename {
                Some(pair) => {
                    own_renames.insert(pair.clone());
                }
                None => own_other = true,
            }
            own.push(range(edit).ok_or_else(|| "an edit of the fact adds or removes a node".to_string())?);
        } else if let Some(found) = range(edit) {
            others.push(found);
        }
    }
    if own.is_empty() {
        return Err("the fact has no edit in the body".into());
    }
    // A fact with only rename sites: the full rename in the old body.
    if !own_other {
        let item = check.old_item.as_ref().and_then(|item| before.items.get(item)).ok_or_else(|| "the old item has no body".to_string())?;
        let body = item.body.as_ref().ok_or_else(|| "the old item has no body".to_string())?;
        let text = before.rust_files.get(&item.file).ok_or_else(|| "the old file has no text".to_string())?;
        let (start, end) = body.span;
        let mut piece = text.get(start..end).ok_or_else(|| "a range outside the old file".to_string())?.to_string();
        for (from, to) in &own_renames {
            piece = replace_word(&piece, from, to);
        }
        let mut full = text.clone();
        full.replace_range(start..end, &piece);
        return Ok(BTreeMap::from([(item.file.clone(), full)]));
    }
    // The outermost edits of the fact.
    own.sort_by_key(|(file, old, _)| (file.clone(), old.0, std::cmp::Reverse(old.1)));
    let mut kept: Vec<(String, (usize, usize), (usize, usize))> = Vec::new();
    for edit in own {
        if kept.last().is_some_and(|last| last.0 == edit.0 && last.1.0 <= edit.1.0 && edit.1.1 <= last.1.1) {
            continue;
        }
        kept.push(edit);
    }
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for (file, old, new) in kept.iter().rev() {
        let old_text = files.get(file).cloned().or_else(|| before.rust_files.get(file).cloned()).ok_or_else(|| "the old file has no text".to_string())?;
        let new_text = after.rust_files.get(file).ok_or_else(|| "the new file has no text".to_string())?;
        // The new text of the edit, with each edit of a different fact inside it back to its
        // old text.
        let mut inner: Vec<&(String, (usize, usize), (usize, usize))> = others.iter().filter(|other| other.0 == *file && new.0 <= other.2.0 && other.2.1 <= new.1 && (other.2.0, other.2.1) != (new.0, new.1)).collect();
        inner.sort_by_key(|other| std::cmp::Reverse(other.2.0));
        let mut piece = new_text.get(new.0..new.1).ok_or_else(|| "a range outside the new file".to_string())?.to_string();
        let mut last_start = usize::MAX;
        for other in inner {
            if other.2.1 > last_start {
                continue;
            }
            let back = before.rust_files.get(file).and_then(|text| text.get(other.1.0..other.1.1)).ok_or_else(|| "a range outside the old file".to_string())?;
            piece.replace_range(other.2.0 - new.0..other.2.1 - new.0, back);
            last_start = other.2.0;
        }
        // Each rename of a different fact keeps its old name in the text of this fact.
        for (from, to) in renames.difference(&own_renames) {
            piece = replace_word(&piece, to, from);
        }
        let mut text = old_text;
        if old.1 > text.len() {
            return Err("a range outside the old file".into());
        }
        text.replace_range(old.0..old.1, &piece);
        files.insert(file.clone(), text);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    // rule+ R61.12-2
    // rule- R61.12-2
    #[test]
    fn a_cause_names_the_source_text_and_the_line_of_its_place() {
        let mut before = Workspace::default();
        let mut after = Workspace::default();
        let text = "pub fn big(v: &[u32]) -> bool {\n    v.iter().any(|x| *x > 9)\n}\n";
        before.rust_files.insert("src/lib.rs".into(), text.into());
        after.rust_files.insert("src/lib.rs".into(), text.into());
        let start = text.find("|x|").unwrap();
        let end = text.find(")\n}").unwrap();
        let cause = format!("a closure value @@o|src/lib.rs|{start}|{end}");
        assert_eq!(cause_text(&cause, &before, &after), ("a closure value: |x| *x > 9 (src/lib.rs:-2)".to_string(), "a closure value".to_string()));
        let cause = format!("a closure value @@n|src/lib.rs|{start}|{end}");
        assert_eq!(cause_text(&cause, &before, &after).0, "a closure value: |x| *x > 9 (src/lib.rs:2)");
        // A cause with no place stays.
        assert_eq!(cause_text("a loop", &before, &after), ("a loop".to_string(), "a loop".to_string()));
    }

    #[test]
    fn a_rename_replaces_only_whole_identifiers() {
        assert_eq!(replace_word("if total >= 100 { total - 5 } else { totals }", "total", "t"), "if t >= 100 { t - 5 } else { totals }");
        assert!(is_identifier("t") && !is_identifier("t > 1") && !is_identifier("_"));
    }

    // rule+ R61.4-5
    // rule- R61.4-5
    #[test]
    fn the_results_of_the_configurations_join_into_one_text_when_they_agree() {
        let equal = Verdict::Equivalent("SMT".into());
        let different = Verdict::Different { input: "x = 4294967295".into(), before: "returns 0".into(), after: "panics (overflow add)".into(), run: RunNote::None };
        assert_eq!(joined(&[("dev".into(), equal.clone()), ("release".into(), equal.clone())], false, false), "equivalent (proved by SMT)");
        assert_eq!(
            joined(&[("dev".into(), different.clone()), ("release".into(), equal.clone())], false, false),
            "different in dev: for x = 4294967295: before returns 0, after panics (overflow add); equivalent in release (proved by SMT)"
        );
        assert_eq!(joined(&[("dev".into(), different)], true, false), "different (for x = 4294967295: before returns 0, after panics (overflow add))");
        assert_eq!(joined(&[("dev".into(), equal)], false, true), "equivalent alone (proved by SMT)");
    }
}
