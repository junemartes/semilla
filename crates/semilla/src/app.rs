//! The operations of semilla. The CLI and the MCP server both call them.
//!
//! The work in progress is the session: `.semilla/session.json`. It holds the change set and the
//! open step. `tag` stores the change set as a git note on the commit, and keeps the step trees
//! reachable with refs under `refs/semilla/`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use semilla_core::view::{FlowGraph, TreeNode, WorkspaceGraph};
use semilla_core::{Anchor, ChangeSet, Class, Claim, Group, GroupKind, Report, Step, Subgroup};
use semilla_syntax::Workspace;
use semilla_verify::{Options, StepOutcome};
use serde::{Deserialize, Serialize};

use crate::git::Repo;
use crate::oracle::{NormalOracle, SmtOracle};

/// A step that the agent started but did not end.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenStep {
    pub intent: String,
    pub class: Option<Class>,
    pub tree_before: String,
    pub claims: Vec<Claim>,
}

/// The work in progress.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub changeset: ChangeSet,
    pub open: Option<OpenStep>,
}

/// The semilla application on one repository.
pub struct App {
    pub repo: Repo,
    workspaces: BTreeMap<String, Workspace>,
    /// For tests: skip the MIR body check.
    pub no_bodies: bool,
}

/// The result of a check.
pub struct CheckResult {
    pub report: Report,
    pub outcomes: Vec<StepOutcome>,
    pub text: String,
    pub tree: TreeNode,
}

impl App {
    pub fn open(dir: &Path) -> Result<App> {
        Ok(App { repo: Repo::discover(dir)?, workspaces: BTreeMap::new(), no_bodies: std::env::var_os("SEMILLA_NO_BODIES").is_some() })
    }

    fn session_file(&self) -> std::path::PathBuf {
        self.repo.state_dir().join("session.json")
    }

    pub fn load_session(&self) -> Result<Session> {
        match std::fs::read_to_string(self.session_file()) {
            Ok(text) => serde_json::from_str(&text).context("read .semilla/session.json"),
            Err(_) => Ok(Session::default()),
        }
    }

    pub fn save_session(&self, session: &Session) -> Result<()> {
        std::fs::create_dir_all(self.repo.state_dir())?;
        std::fs::write(self.session_file(), serde_json::to_string_pretty(session)?)?;
        Ok(())
    }

    /// Loads the workspace of a git tree (cached).
    pub fn workspace(&mut self, tree: &str) -> Result<Workspace> {
        if let Some(ws) = self.workspaces.get(tree) {
            return Ok(ws.clone());
        }
        let dir = self.repo.materialize(tree)?;
        let ws = Workspace::load(&dir).with_context(|| format!("load the workspace of tree {tree}"))?;
        self.workspaces.insert(tree.to_string(), ws.clone());
        Ok(ws)
    }

    // ----- the tag tools -----

    /// Starts a step. The current working tree is the "before" tree.
    pub fn step_begin(&mut self, intent: &str, class: Option<Class>) -> Result<String> {
        let mut session = self.load_session()?;
        if session.open.is_some() {
            bail!("a step is already open; end it with `step_end` first");
        }
        let tree = self.repo.write_tree()?;
        if session.changeset.steps.is_empty() {
            session.changeset.base_tree = tree.clone();
        } else if session.changeset.head_tree != tree {
            bail!("the working tree changed after the last step ended; put all changes inside steps");
        }
        session.open = Some(OpenStep { intent: intent.to_string(), class, tree_before: tree.clone(), claims: Vec::new() });
        self.save_session(&session)?;
        Ok(format!("step {} started at tree {tree}", session.changeset.steps.len()))
    }

    /// Adds a claim to the open step. Returns the claim ID.
    pub fn claim(&mut self, op: &str, at: Option<&str>, args: serde_json::Map<String, serde_json::Value>) -> Result<String> {
        let mut session = self.load_session()?;
        let id = {
            let mut set = session.changeset.clone();
            if let Some(open) = &session.open {
                set.steps.push(Step { index: usize::MAX, intent: String::new(), class: None, claims: open.claims.clone(), tree_before: String::new(), tree_after: String::new() });
            }
            set.next_claim_id()
        };
        let open = session.open.as_mut().context("no step is open; start one with `step_begin`")?;
        if semilla_core::ops::op(op).is_none() {
            bail!("unknown operation `{op}`; see the `ops` tool for the list");
        }
        let at: Option<Anchor> = at.map(|text| text.parse()).transpose().map_err(|e| anyhow::anyhow!("{e}"))?;
        open.claims.push(Claim { id: id.clone(), op: op.to_string(), at, args });
        self.save_session(&session)?;
        Ok(id)
    }

    /// Removes a claim from the open step.
    pub fn unclaim(&mut self, id: &str) -> Result<()> {
        let mut session = self.load_session()?;
        let open = session.open.as_mut().context("no step is open")?;
        let count = open.claims.len();
        open.claims.retain(|claim| claim.id != id);
        if open.claims.len() == count {
            bail!("the open step has no claim {id}");
        }
        self.save_session(&session)
    }

    /// Ends the open step. The current working tree is the "after" tree.
    pub fn step_end(&mut self) -> Result<String> {
        let mut session = self.load_session()?;
        let open = session.open.take().context("no step is open")?;
        let tree = self.repo.write_tree()?;
        let index = session.changeset.steps.len();
        session.changeset.steps.push(Step { index, intent: open.intent, class: open.class, claims: open.claims, tree_before: open.tree_before, tree_after: tree.clone() });
        session.changeset.head_tree = tree.clone();
        self.save_session(&session)?;
        Ok(format!("step {index} ended at tree {tree}"))
    }

    /// The anchor at a file position of the working tree, or of the "before" tree of the open
    /// step when `before` is true. Claims on existing code use the "before" anchors.
    pub fn anchor(&mut self, file: &str, line: usize, column: Option<usize>, before: bool) -> Result<String> {
        let ws = if before {
            let session = self.load_session()?;
            let tree = session.open.map(|open| open.tree_before).or(Some(session.changeset.head_tree)).filter(|t| !t.is_empty()).context("no step yet")?;
            self.workspace(&tree)?
        } else {
            Workspace::load(&self.repo.root)?
        };
        let file = file.trim_start_matches("./");
        semilla_syntax::anchor_at(&ws, file, line, column).map(|anchor| anchor.to_string()).with_context(|| format!("no item at {file}:{line}"))
    }

    // ----- checks -----

    /// The change set with the open step as a last step (its "after" tree is the working tree).
    fn current_changeset(&self) -> Result<ChangeSet> {
        let session = self.load_session()?;
        let mut set = session.changeset;
        if let Some(open) = session.open {
            let tree = self.repo.write_tree()?;
            let index = set.steps.len();
            set.steps.push(Step { index, intent: open.intent, class: open.class, claims: open.claims, tree_before: open.tree_before, tree_after: tree.clone() });
            set.head_tree = tree;
        }
        Ok(set)
    }

    /// Checks the change set. `only_step` limits the report to one step.
    pub fn check(&mut self, only_step: Option<usize>, require_groups: bool) -> Result<CheckResult> {
        let mut set = self.current_changeset()?;
        if let Some(index) = only_step {
            let step = set.steps.get(index).cloned().with_context(|| format!("no step {index}"))?;
            set = ChangeSet { base_tree: step.tree_before.clone(), head_tree: step.tree_after.clone(), steps: vec![step], groups: Vec::new(), subgroups: Vec::new() };
        }
        self.check_set(&set, require_groups)
    }

    pub fn check_set(&mut self, set: &ChangeSet, require_groups: bool) -> Result<CheckResult> {
        let bodies: Box<dyn semilla_verify::BodyOracle> = if self.no_bodies { Box::new(semilla_verify::NoOracle) } else { Box::new(NormalOracle { repo: self.repo.clone() }) };
        let exprs = SmtOracle::new();
        let trees: BTreeSet<String> = set.steps.iter().flat_map(|s| [s.tree_before.clone(), s.tree_after.clone()]).collect();
        for tree in &trees {
            self.workspace(tree)?;
        }
        let cache = self.workspaces.clone();
        let mut load = |tree: &str| cache.get(tree).cloned().with_context(|| format!("no workspace for tree {tree}"));
        let (mut report, outcomes) = semilla_verify::verify_changeset(set, &mut load, bodies.as_ref(), &exprs, Options { require_groups });
        // The test rule of `tests` groups needs the test items of all trees.
        let test_items: BTreeSet<String> = cache.values().flat_map(|ws| ws.items.values().filter(|i| i.test).map(|i| i.path.clone())).collect();
        semilla_verify::organize::check_test_groups(set, &test_items, &mut report);
        report.finish();
        if !exprs.available() {
            report.warnings.push("Z3 is not available; condition effects are not computed (run scripts/fetch-z3.sh)".into());
        }
        let text = format!("{}{}", semilla_verify::views::text_report(&report.steps), summary_line(&report));
        let (base, head) = (self.workspace(&set.base_tree).unwrap_or_default(), self.workspace(&set.head_tree).unwrap_or_default());
        let tree = semilla_verify::views::change_tree(&outcomes, &head, &base);
        Ok(CheckResult { report, outcomes, text, tree })
    }

    /// Claim suggestions for the uncovered edits of the last (or open) step.
    pub fn suggest_claims(&mut self) -> Result<Vec<Claim>> {
        let set = self.current_changeset()?;
        let last = set.steps.len().checked_sub(1).context("no step yet")?;
        let saved = self.no_bodies;
        self.no_bodies = true;
        let result = self.check(Some(last), false);
        self.no_bodies = saved;
        let result = result?;
        let outcome = result.outcomes.first().context("no outcome")?;
        Ok(semilla_verify::hints::suggest_claims(&outcome.edits, &outcome.owners))
    }

    // ----- groups and subgroups -----

    pub fn group_set(&mut self, id: &str, kind: GroupKind, title: &str, why: &str, steps: Vec<usize>) -> Result<()> {
        let mut session = self.load_session()?;
        session.changeset.groups.retain(|group| group.id != id);
        session.changeset.groups.push(Group { id: id.to_string(), title: title.to_string(), kind, why: why.to_string(), steps });
        session.changeset.groups.sort_by(|a, b| a.id.cmp(&b.id));
        let test_items = self.test_items(&session.changeset)?;
        semilla_verify::organize::place_new_claims(&mut session.changeset, id, &test_items);
        self.save_session(&session)
    }

    /// The paths of the test items in all trees of a change set.
    fn test_items(&mut self, set: &ChangeSet) -> Result<BTreeSet<String>> {
        let mut items = BTreeSet::new();
        for step in &set.steps {
            for tree in [&step.tree_before, &step.tree_after] {
                items.extend(self.workspace(tree)?.items.values().filter(|i| i.test).map(|i| i.path.clone()));
            }
        }
        Ok(items)
    }

    pub fn subgroups_suggest(&mut self, group: &str) -> Result<Vec<Subgroup>> {
        let set = self.load_session()?.changeset;
        let result = self.check_set(&set, false)?;
        let test_items: BTreeSet<String> = self.workspaces.values().flat_map(|ws| ws.items.values().filter(|i| i.test).map(|i| i.path.clone())).collect();
        Ok(semilla_verify::organize::suggest(&set, group, &result.report.steps, &test_items))
    }

    /// Sets the subgroups of a group. Returns the subgroup errors (empty when the layout is valid).
    pub fn subgroups_set(&mut self, group: &str, mut layout: Vec<Subgroup>) -> Result<Vec<String>> {
        let mut session = self.load_session()?;
        let suggestion = self.subgroups_suggest(group)?;
        for (index, subgroup) in layout.iter_mut().enumerate() {
            subgroup.group = group.to_string();
            if subgroup.id.is_empty() {
                subgroup.id = format!("{group}:{index}");
            }
            subgroup.new_claims.clear();
            subgroup.edited = !suggestion.iter().any(|s| s.role == subgroup.role && s.claims == subgroup.claims && s.title == subgroup.title);
        }
        session.changeset.subgroups.retain(|s| s.group != group);
        session.changeset.subgroups.extend(layout);
        self.save_session(&session)?;
        let set = session.changeset;
        let result = self.check_set(&set, false)?;
        Ok(result.report.organization.into_iter().filter(|e| e.contains("subgroup")).collect())
    }

    // ----- storage -----

    /// Stores the change set on a commit. The last step must end at the commit tree.
    pub fn tag(&mut self, commit: &str) -> Result<String> {
        let session = self.load_session()?;
        if session.open.is_some() {
            bail!("a step is still open; end it first");
        }
        let set = session.changeset;
        let commit = self.repo.git(&["rev-parse", commit])?;
        let tree = self.repo.commit_tree(&commit)?;
        if set.head_tree != tree {
            bail!("the last step ends at tree {}, but commit {commit} has tree {tree}", set.head_tree);
        }
        self.repo.write_note(&commit, &serde_json::to_string_pretty(&set)?)?;
        self.repo.keep_tree(&format!("refs/semilla/{commit}/base"), &set.base_tree, "semilla base")?;
        for step in &set.steps {
            self.repo.keep_tree(&format!("refs/semilla/{commit}/step-{}", step.index), &step.tree_after, &format!("semilla step {}", step.index))?;
        }
        Ok(format!("tagged {commit} with {} steps", set.steps.len()))
    }

    /// Loads the change set of a tagged commit into the session (for a review).
    pub fn load(&mut self, commit: &str) -> Result<String> {
        let commit = self.repo.git(&["rev-parse", commit])?;
        let note = self.repo.read_note(&commit).with_context(|| format!("commit {commit} has no semilla note"))?;
        let set: ChangeSet = serde_json::from_str(&note)?;
        let steps = set.steps.len();
        self.save_session(&Session { changeset: set, open: None })?;
        Ok(format!("loaded {steps} steps from {commit}"))
    }

    /// Builds the data of the review UI.
    pub fn ui_data(&mut self) -> Result<semilla_ui::UiData> {
        let set = self.current_changeset()?;
        let result = self.check_set(&set, false)?;
        let mut flows: Vec<FlowGraph> = Vec::new();
        let mut sources = BTreeMap::new();
        for (step, outcome) in set.steps.iter().zip(&result.outcomes) {
            let before = self.workspace(&step.tree_before)?;
            let after = self.workspace(&step.tree_after)?;
            for flow in semilla_verify::views::flows(std::slice::from_ref(outcome), &after) {
                flows.retain(|f| f.function != flow.function);
                flows.push(flow);
            }
            for (key, (old, new)) in semilla_verify::views::sources(std::slice::from_ref(outcome), &before, &after) {
                let entry: &mut semilla_ui::SourcePair = sources.entry(key).or_insert(semilla_ui::SourcePair { before: None, after: None });
                if entry.before.is_none() {
                    entry.before = old;
                }
                if new.is_some() {
                    entry.after = new;
                }
            }
        }
        let head = self.workspace(&set.head_tree).unwrap_or_default();
        let graph: WorkspaceGraph = semilla_verify::views::graph(&result.outcomes, &head);
        Ok(semilla_ui::UiData { changeset: set, report: result.report, tree: result.tree, flows, graph, sources })
    }
}

fn summary_line(report: &Report) -> String {
    let claims: usize = report.steps.iter().map(|s| s.claims.len()).sum();
    let uncovered: usize = report.steps.iter().map(|s| s.uncovered.len()).sum();
    let mut text = format!(
        "result: {} · {} steps · {claims} claims · {uncovered} uncovered · score {:.2}\n",
        if report.accepted { "accepted" } else { "rejected" },
        report.steps.len(),
        report.score
    );
    for error in &report.organization {
        text.push_str(&format!("  ✗ {error}\n"));
    }
    for warning in &report.warnings {
        text.push_str(&format!("  ! {warning}\n"));
    }
    text
}
