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

/// The optional arguments of an op, in addition to its required arguments. `affects` and `note`
/// apply to every op.
pub fn optional_args(op: &str) -> Vec<&'static str> {
    let mut args = vec!["affects", "note"];
    args.extend(match op {
        "extract-function" | "inline-function" => &["count", "removes"][..],
        "add-if" | "add-early-exit" => &["cond"][..],
        "rewrite-pattern" => &["sites"][..],
        "rewrite-item" => &["new"][..],
        "rename-local" => &["from"][..],
        "extract-variable" | "inline-variable" | "change-mutability" | "change-type-annotation" | "invert-if" | "swap-branches" | "if-to-match" | "match-to-if"
        | "guard-clause" | "reorder-statements" | "question-mark" => &["count"][..],
        _ => &[][..],
    });
    args
}

/// The work in progress.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub changeset: ChangeSet,
    pub open: Option<OpenStep>,
    /// The highest claim number so far, so that a removed claim ID is not used again.
    #[serde(default)]
    pub last_claim: usize,
    /// The replay in progress (`replay begin`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<crate::replay::Replay>,
    /// The commit that the change set describes, when it is not the next commit
    /// (`replay begin --commit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The commit that `tag` stored this change set on. A new replay can then replace the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagged: Option<String>,
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
        let repo = Repo::discover(dir)?;
        // The state folder never goes into a tree, also when the repository does not ignore it.
        let ignore = repo.state_dir().join(".gitignore");
        if !ignore.exists() {
            std::fs::create_dir_all(repo.state_dir())?;
            std::fs::write(&ignore, "*\n")?;
        }
        Ok(App { repo, workspaces: BTreeMap::new(), no_bodies: std::env::var_os("SEMILLA_NO_BODIES").is_some() })
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
        let started = std::time::Instant::now();
        let dir = self.repo.materialize(tree)?;
        crate::timing("materialize a tree", started);
        let started = std::time::Instant::now();
        let ws = Workspace::load(&dir).with_context(|| format!("load the workspace of tree {tree}"))?;
        crate::timing("load the syntax workspace", started);
        self.workspaces.insert(tree.to_string(), ws.clone());
        Ok(ws)
    }

    // ----- the tag tools -----

    /// Starts a step. The current working tree is the "before" tree.
    pub fn step_begin(&mut self, intent: &str, class: Option<Class>) -> Result<String> {
        let mut session = self.load_session()?;
        if session.open.is_some() {
            bail!("a step is already open; end it with `semilla step end` (MCP: `step_end`) first");
        }
        let tree = self.repo.write_tree()?;
        if session.changeset.steps.is_empty() && session.replay.is_some() {
            // In a replay, the first step starts at the base of the replay.
            if tree != session.changeset.base_tree {
                bail!("the working tree differs from the base of the replay; only `replay take` and your edits inside steps may change it");
            }
        } else if session.changeset.steps.is_empty() {
            // The base must be the last commit. A change in the working tree before the first step
            // would be in the base, and no claim would cover it.
            if let Ok(head) = self.repo.commit_tree("HEAD") {
                if head != tree {
                    bail!("the working tree differs from HEAD; commit or stash these changes before the first step, or they would be in the base without a claim");
                }
            }
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
        let used = session.changeset.steps.iter().flat_map(|s| &s.claims).chain(session.open.iter().flat_map(|o| &o.claims));
        let highest = used.filter_map(|c| c.id.strip_prefix('c').and_then(|n| n.parse::<usize>().ok())).max().unwrap_or(0);
        let number = highest.max(session.last_claim) + 1;
        let id = format!("c{number}");
        let open = session.open.as_mut().context("no step is open; start one with `semilla step begin` (MCP: `step_begin`), or reopen the last step with `semilla step reopen`")?;
        let Some(info) = semilla_core::ops::op(op) else {
            bail!("unknown operation `{op}`; `semilla ops` lists all operations");
        };
        // Reject an unknown argument: it is usually a typo, and the verifier would ignore it.
        let mut known: Vec<&str> = info.args.to_vec();
        known.extend(optional_args(op).into_iter().filter(|arg| !info.args.contains(arg)));
        if let Some(unknown) = args.keys().find(|key| !known.contains(&key.as_str())) {
            bail!("`{op}` has no argument `{unknown}`; its arguments are: {}", known.join(", "));
        }
        if let Some(missing) = info.args.iter().find(|arg| !args.contains_key(**arg)) {
            bail!("`{op}` needs the argument `{missing}`; `semilla ops {op}` shows an example");
        }
        let at: Option<Anchor> = at.map(|text| text.parse()).transpose().map_err(|e| anyhow::anyhow!("{e}"))?;
        open.claims.push(Claim { id: id.clone(), op: op.to_string(), at, args });
        session.last_claim = number;
        self.save_session(&session)?;
        Ok(id)
    }

    /// Removes a claim from the open step.
    pub fn unclaim(&mut self, id: &str) -> Result<()> {
        let mut session = self.load_session()?;
        let open = session.open.as_mut().context("no step is open; reopen the last step with `semilla step reopen`")?;
        let count = open.claims.len();
        open.claims.retain(|claim| claim.id != id);
        if open.claims.len() == count {
            bail!("the open step has no claim {id}");
        }
        self.save_session(&session)
    }

    /// Changes the intent or the declared class of the open step. `class` is `Some(None)` to
    /// remove the declared class.
    pub fn step_edit(&mut self, intent: Option<&str>, class: Option<Option<Class>>) -> Result<String> {
        let mut session = self.load_session()?;
        let open = session.open.as_mut().context("no step is open; `semilla step reopen` opens the last step")?;
        if let Some(intent) = intent {
            open.intent = intent.to_string();
        }
        if let Some(class) = class {
            open.class = class;
        }
        let text = format!("the open step: intent \"{}\", class {}", open.intent, open.class.map(semilla_verify::class_name).unwrap_or("not declared"));
        self.save_session(&session)?;
        Ok(text)
    }

    /// Removes the open step when it has no change, or the last ended step when it has no change
    /// and no claim.
    pub fn step_drop(&mut self) -> Result<String> {
        let mut session = self.load_session()?;
        let current = self.repo.write_tree()?;
        if let Some(open) = &session.open {
            if current != open.tree_before {
                bail!("the open step has changes; undo them first, or end the step");
            }
            session.open = None;
            self.save_session(&session)?;
            return Ok("the open step is removed".into());
        }
        let step = session.changeset.steps.last().context("no step to remove")?;
        if step.tree_before != step.tree_after || !step.claims.is_empty() {
            bail!("step {} has changes or claims; only an empty step can be removed", step.index);
        }
        let index = step.index;
        let before = step.tree_before.clone();
        session.changeset.steps.pop();
        session.changeset.head_tree = before;
        for group in &mut session.changeset.groups {
            group.steps.retain(|i| *i != index);
        }
        self.save_session(&session)?;
        Ok(format!("step {index} is removed"))
    }

    /// The saved subgroup layout of a group, if the agent set one.
    pub fn subgroups_saved(&self, group: &str) -> Result<Vec<Subgroup>> {
        Ok(self.load_session()?.changeset.subgroups.into_iter().filter(|s| s.group == group).collect())
    }

    /// Opens the last step again, so that the agent can fix its claims. The working tree must still
    /// be the tree at the end of that step.
    pub fn step_reopen(&mut self) -> Result<String> {
        let mut session = self.load_session()?;
        if session.open.is_some() {
            bail!("a step is already open");
        }
        let step = session.changeset.steps.pop().context("no step to reopen")?;
        if self.repo.write_tree()? != step.tree_after {
            bail!("the working tree changed after step {} ended; reopen works only on an unchanged tree", step.index);
        }
        if session.changeset.steps.is_empty() {
            session.changeset.base_tree = step.tree_before.clone();
        }
        session.changeset.head_tree = step.tree_before.clone();
        for group in &mut session.changeset.groups {
            group.steps.retain(|index| *index != step.index);
        }
        let claims = step.claims.len();
        session.open = Some(OpenStep { intent: step.intent, class: step.class, tree_before: step.tree_before, claims: step.claims });
        self.save_session(&session)?;
        Ok(format!("step {} is open again, with {claims} claim(s); edit, claim or unclaim, then `semilla step end`", step.index))
    }

    /// Ends the open step. The current working tree is the "after" tree.
    pub fn step_end(&mut self) -> Result<String> {
        self.step_end_with(true)
    }

    /// Ends the open step. `queue` starts the background check (a plan queues it once, at the end).
    pub(crate) fn step_end_with(&mut self, queue: bool) -> Result<String> {
        let mut session = self.load_session()?;
        let open = session.open.take().context("no step is open")?;
        let tree = self.repo.write_tree()?;
        let index = session.changeset.steps.len();
        session.changeset.steps.push(Step { index, intent: open.intent, class: open.class, claims: open.claims, tree_before: open.tree_before, tree_after: tree.clone() });
        session.changeset.head_tree = tree.clone();
        self.save_session(&session)?;
        if queue {
            self.queue_session(&session.changeset)?;
        }
        let mut text = format!("step {index} ended at tree {tree}; the background check runs (see `status`)");
        if let Some(remaining) = self.replay_remaining(&session)? {
            text.push_str(&format!("\nreplay: {remaining}"));
        }
        Ok(text)
    }

    /// Adds a background job for the session, and starts the worker.
    pub(crate) fn queue_session(&self, set: &ChangeSet) -> Result<()> {
        let job = crate::background::Job { key: "session".into(), hash: crate::background::changeset_hash(set), changeset: Some(set.clone()) };
        crate::background::enqueue(self, &job)?;
        crate::background::spawn_worker(self)
    }

    /// The state of the background check of the session, or of a tagged commit.
    pub fn status(&mut self, commit: Option<&str>) -> Result<String> {
        let (key, set) = match commit {
            Some(commit) => {
                let commit = self.repo.git(&["rev-parse", commit])?;
                let note = self.repo.read_note(&commit).with_context(|| format!("commit {commit} has no semilla note"))?;
                let set: ChangeSet = serde_json::from_str(&note)?;
                (commit, set)
            }
            None => ("session".to_string(), self.load_session()?.changeset),
        };
        let hash = crate::background::changeset_hash(&set);
        let state = crate::background::state(self, &key, &hash);
        let jobs = crate::background::jobs(self).len();
        let mut text = format!("{key}: {}; {jobs} job(s) in the queue\n", format!("{state:?}").to_lowercase());
        if let Some(stored) = crate::background::stored(self, &key) {
            if stored.hash != hash {
                text.push_str("the report below is for an older change set; a new check is necessary or runs now:\n");
            }
            text.push_str(&stored.text);
        }
        Ok(text)
    }

    /// The change set of a review target: `session`, or a tagged commit.
    pub fn review_target(&self, target: &str) -> Result<(String, ChangeSet)> {
        if target == "session" {
            return Ok(("session".into(), self.load_session()?.changeset));
        }
        let commit = self.repo.git(&["rev-parse", target])?;
        let note = self.repo.read_note(&commit).with_context(|| format!("commit {commit} has no semilla note; tag it first"))?;
        Ok((commit, serde_json::from_str(&note)?))
    }

    /// The anchor at a file position of the working tree, or of the "before" tree of the open
    /// step when `before` is true. Claims on existing code use the "before" anchors.
    /// Then it goes `up` levels up (for example from `cond/lhs` to `cond`).
    /// The column starts at 1, as in editors.
    pub fn anchor_up(&mut self, file: &str, line: usize, column: Option<usize>, before: bool, up: usize) -> Result<String> {
        let column = column.map(|c| c.saturating_sub(1));
        let ws = if before {
            let session = self.load_session()?;
            let tree = session.open.map(|open| open.tree_before).or(Some(session.changeset.head_tree)).filter(|t| !t.is_empty()).context("no step yet")?;
            self.workspace(&tree)?
        } else {
            Workspace::load(&self.repo.root)?
        };
        let file = file.trim_start_matches("./");
        let mut anchor = semilla_syntax::anchor_at(&ws, file, line, column).with_context(|| format!("no item at {file}:{line}"))?;
        for _ in 0..up {
            anchor = anchor.parent().context("the anchor has no parent inside its item")?;
        }
        Ok(anchor.to_string())
    }

    /// Builds the index of the working tree: the workspace graph, the items, and the MIR content
    /// hash of each body. Writes `.semilla/index.json` and returns a summary.
    pub fn index(&mut self) -> Result<String> {
        let tree = self.repo.write_tree()?;
        let ws = self.workspace(&tree)?;
        let graph = semilla_syntax::graph::workspace_graph(&ws, &BTreeMap::new(), &BTreeMap::new());
        let hashes = if self.no_bodies {
            BTreeMap::new()
        } else {
            let records = NormalOracle { repo: self.repo.clone() }.records(&tree, "n0")?;
            semilla_normal::body_hashes(&records)
        };
        let items: Vec<serde_json::Value> = ws
            .items
            .values()
            .map(|item| serde_json::json!({"path": item.path, "kind": semilla_syntax::diff::kind_name(item.kind), "file": item.file, "lines": [item.line_start, item.line_end], "test": item.test}))
            .collect();
        let index = serde_json::json!({"tree": tree, "crates": ws.crates, "items": items, "graph": graph, "body_hashes": hashes});
        std::fs::write(self.repo.state_dir().join("index.json"), serde_json::to_string_pretty(&index)?)?;
        Ok(format!("indexed tree {tree}: {} crates, {} items, {} bodies", ws.crates.len(), ws.items.len(), hashes.len()))
    }

    // ----- checks -----

    /// The change set with the open step as a last step (its "after" tree is the working tree).
    pub(crate) fn current_changeset(&self) -> Result<ChangeSet> {
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

    /// The base check of the final verification: the change set starts at the last commit (before
    /// the commit of the work), or at the parent of `HEAD` (after it).
    fn base_problem(&self, set: &ChangeSet, commit: Option<&str>) -> Option<String> {
        let head = self.repo.commit_tree("HEAD").ok()?;
        let expected = match commit {
            Some(commit) => self.repo.commit_tree(&format!("{commit}^")).ok()?,
            None if head == set.head_tree && !set.steps.is_empty() => self.repo.commit_tree("HEAD^").ok()?,
            None => head,
        };
        (expected != set.base_tree).then(|| format!("the change set starts at tree {}, but the commit before the work has tree {expected}; the changes between them have no claim", set.base_tree))
    }

    /// Checks the change set. `only_step` limits the report to one step.
    pub fn check(&mut self, only_step: Option<usize>, require_groups: bool) -> Result<CheckResult> {
        let mut set = self.current_changeset()?;
        let session = self.load_session()?;
        let mut base_problem = if require_groups { self.base_problem(&set, session.commit.as_deref()) } else { None };
        if require_groups && session.replay.is_some() {
            base_problem = Some("a replay is in progress; end it with `semilla replay end` first".into());
        }
        if let Some(index) = only_step {
            let step = set.steps.get(index).cloned().with_context(|| format!("no step {index}"))?;
            set = ChangeSet { base_tree: step.tree_before.clone(), head_tree: step.tree_after.clone(), steps: vec![step], pair_refusals: set.pair_refusals.clone(), ..Default::default() };
        }
        let mut result = self.check_set(&set, require_groups)?;
        if let Some(problem) = base_problem {
            result.report.organization.push(problem.clone());
            result.report.accepted = false;
            result.text.push_str(&format!("  ✗ {problem}\n"));
        }
        Ok(result)
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
        let started = std::time::Instant::now();
        let (mut report, outcomes) = semilla_verify::verify_changeset(set, &mut load, bodies.as_ref(), &exprs, Options { require_groups });
        crate::timing("verify the change set (with the body checks)", started);
        // The test rule of `tests` groups needs the test items of all trees.
        let test_items: BTreeSet<String> = cache.values().flat_map(|ws| ws.items.values().filter(|i| i.test).map(|i| i.path.clone())).collect();
        semilla_verify::organize::check_test_groups(set, &test_items, &mut report);
        // The reviewer's decisions on the pair warnings (from the review UI).
        let review = semilla_ui::review::ReviewFile::load(&self.repo.state_dir().join("review.json"));
        for pair in &mut report.pairs {
            pair.reviewer = review.pair(&pair.key()).map(str::to_string);
            if pair.reviewer.as_deref() == Some("accepted") {
                report.organization.push(format!(
                    "the reviewer accepted the pair warning `{}`: claim it as one changed item (`rename` or `move-item`, then the body changes; or `rewrite-item`), not as `remove-item` and `add-item`",
                    pair.key()
                ));
            }
        }
        report.phase = if self.no_bodies { "syntax".into() } else { "full".into() };
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
        let step = set.steps[last].clone();
        let (before, after) = (self.workspace(&step.tree_before)?, self.workspace(&step.tree_after)?);
        let renames = semilla_verify::hints::likely_renames(&outcome.edits, &before, &after);
        // A local rename explains all uncovered edits of its function.
        let uncovered: Vec<semilla_syntax::Edit> = outcome.edits.iter().zip(&outcome.owners).filter(|(_, owner)| owner.is_none()).map(|(edit, _)| edit.clone()).collect();
        let local = semilla_verify::hints::likely_local_renames(&uncovered, &before, &after);
        let (edits, owners): (Vec<semilla_syntax::Edit>, Vec<Option<usize>>) = outcome
            .edits
            .iter()
            .zip(&outcome.owners)
            .map(|(edit, owner)| {
                let renamed = edit.anchor().is_some_and(|a| local.iter().any(|(item, _, _)| *item == a.item));
                (edit.clone(), if renamed { Some(usize::MAX) } else { *owner })
            })
            .unzip();
        let mut claims = semilla_verify::hints::local_rename_claims(&local);
        claims.extend(semilla_verify::hints::suggest_claims_with_renames(&edits, &owners, &renames));
        Ok(claims)
    }

    // ----- pair warnings -----

    /// Refuses a pair warning: the removed item and the added item are really two items. The
    /// reviewer sees the reason. `reason` `None` removes the refusal.
    pub fn pair_refuse(&mut self, old: &str, new: &str, reason: Option<&str>) -> Result<String> {
        let mut session = self.load_session()?;
        session.changeset.pair_refusals.retain(|r| !(r.old == old && r.new == new));
        let text = match reason {
            Some(reason) if reason.trim().len() < 10 => bail!("give a reason of at least 10 characters: the reviewer reads it"),
            Some(reason) => {
                session.changeset.pair_refusals.push(semilla_core::PairRefusal { old: old.to_string(), new: new.to_string(), reason: reason.trim().to_string() });
                format!("refused the pair warning `{old} -> {new}`; the reviewer sees the reason and can still accept the warning")
            }
            None => format!("removed the refusal of `{old} -> {new}`"),
        };
        self.save_session(&session)?;
        Ok(text)
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
        let mut candidate = session.changeset.clone();
        candidate.subgroups.retain(|s| s.group != group);
        candidate.subgroups.extend(layout);
        let saved = self.no_bodies;
        self.no_bodies = true;
        let result = self.check_set(&candidate, false);
        self.no_bodies = saved;
        let errors: Vec<String> = result?.report.organization.into_iter().filter(|e| e.contains("subgroup") || e.contains(&format!("group {group}"))).collect();
        // A rejected layout is not saved.
        if errors.is_empty() {
            session.changeset = candidate;
            self.save_session(&session)?;
        }
        Ok(errors)
    }

    // ----- storage -----

    /// Stores the change set on a commit. The last step must end at the commit tree.
    pub fn tag(&mut self, commit: &str) -> Result<String> {
        let session = self.load_session()?;
        if session.open.is_some() {
            bail!("a step is still open; end it first");
        }
        let mut set = session.changeset;
        let commit = self.repo.git(&["rev-parse", commit])?;
        let tree = self.repo.commit_tree(&commit)?;
        // The change set must start at the parent commit, so that every change of the commit is
        // inside a step.
        if let Ok(parent) = self.repo.commit_tree(&format!("{commit}^")) {
            if parent != set.base_tree {
                bail!("the change set starts at tree {}, but the parent of {commit} has tree {parent}; the changes between them have no claim", set.base_tree);
            }
        }
        if set.head_tree != tree {
            // A difference in formatting only is allowed (for example `cargo fmt` before the
            // commit). A changed comment is not formatting.
            let (last, head) = (self.workspace(&set.head_tree)?, self.workspace(&tree)?);
            if !semilla_syntax::diff(&last, &head, &Default::default()).is_empty() {
                bail!("the last step ends at tree {}, but commit {commit} has tree {tree}; put all changes inside steps", set.head_tree);
            }
            set.head_tree = tree.clone();
        }
        if session.replay.is_some() {
            bail!("a replay is in progress; end it with `semilla replay end` first");
        }
        self.repo.write_note(&commit, &serde_json::to_string_pretty(&set)?)?;
        let mut session = self.load_session()?;
        session.tagged = Some(commit.clone());
        self.save_session(&session)?;
        self.repo.keep_tree(&format!("refs/semilla/{commit}/base"), &set.base_tree, "semilla base")?;
        for step in &set.steps {
            self.repo.keep_tree(&format!("refs/semilla/{commit}/step-{}", step.index), &step.tree_after, &format!("semilla step {}", step.index))?;
        }
        let job = crate::background::Job { key: commit.clone(), hash: crate::background::changeset_hash(&set), changeset: None };
        crate::background::enqueue(self, &job)?;
        crate::background::spawn_worker(self)?;
        Ok(format!("tagged {commit} with {} steps; the background check runs (see `status {commit}`)", set.steps.len()))
    }

    /// Loads the change set of a tagged commit into the session (for a review).
    pub fn load(&mut self, commit: &str) -> Result<String> {
        let commit = self.repo.git(&["rev-parse", commit])?;
        let note = self.repo.read_note(&commit).with_context(|| format!("commit {commit} has no semilla note"))?;
        let set: ChangeSet = serde_json::from_str(&note)?;
        let steps = set.steps.len();
        self.save_session(&Session { changeset: set, tagged: Some(commit.clone()), ..Default::default() })?;
        Ok(format!("loaded {steps} steps from {commit}"))
    }

    /// Replaces the guessed external calls of the graph with the calls from the MIR, when the N0
    /// records of the tree are in the cache (design 10.2: full paths and signature badges).
    fn add_external_calls(&self, graph: &mut WorkspaceGraph, workspace: &Workspace, tree: &str) {
        use semilla_core::view::{GraphEdge, GraphEdgeKind, GraphNode, GraphNodeKind, Status};
        let cache = self.repo.state_dir().join("cache/mir").join(tree).join("n0");
        if !cache.join(".complete").exists() {
            return;
        }
        let Ok(records) = semilla_normal::read_records(&cache) else { return };
        let mut calls: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        for record in &records {
            let Some(span) = &record.span else { continue };
            let item = workspace
                .items
                .values()
                .filter(|item| item.kind == semilla_syntax::ItemKind::Fn && span.file.ends_with(&item.file) && item.line_start <= span.line_start && span.line_start <= item.line_end)
                .min_by_key(|item| item.line_end - item.line_start);
            let Some(item) = item else { continue };
            for external in &record.externals {
                calls.entry(item.path.clone()).or_default().entry(external.path.clone()).or_default().extend(external.badges.iter().cloned());
            }
        }
        if calls.is_empty() {
            return;
        }
        // The MIR knows the real calls of these functions: drop their guessed external edges.
        graph.edges.retain(|edge| !(edge.kind == GraphEdgeKind::Call && edge.to.starts_with("ext::") && calls.contains_key(&edge.from)));
        for (from, targets) in calls {
            for (path, badges) in targets {
                let id = format!("ext::{path}");
                match graph.nodes.iter_mut().find(|node| node.id == id) {
                    Some(node) => node.badges = node.badges.iter().cloned().chain(badges.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect(),
                    None => graph.nodes.push(GraphNode {
                        id: id.clone(),
                        label: path.clone(),
                        kind: GraphNodeKind::External,
                        parent: None,
                        status: Status::Same,
                        changes: 0,
                        badges: badges.into_iter().collect(),
                        entry: false,
                        test: false,
                        file: None,
                    }),
                }
                graph.edges.push(GraphEdge { from: from.clone(), to: id, kind: GraphEdgeKind::Call, status: Status::Same });
            }
        }
        // Remove the external nodes without an edge.
        let used: BTreeSet<String> = graph.edges.iter().map(|edge| edge.to.clone()).collect();
        graph.nodes.retain(|node| node.kind != GraphNodeKind::External || used.contains(&node.id));
    }

    /// Builds the data of the review UI for the work in progress.
    pub fn ui_data(&mut self) -> Result<semilla_ui::UiData> {
        let set = self.current_changeset()?;
        self.ui_data_for(&set)
    }

    /// Builds the data of the review UI for a change set. The body checks come from the cache of
    /// the background worker when it has run.
    pub fn ui_data_for(&mut self, set: &ChangeSet) -> Result<semilla_ui::UiData> {
        let mut set = set.clone();
        let result = self.check_set(&set, false)?;
        // A group without subgroups of the agent shows the engine suggestion.
        let test_items = self.test_items(&set)?;
        for group in set.groups.clone() {
            if !set.subgroups.iter().any(|s| s.group == group.id) {
                let suggestion = semilla_verify::organize::suggest(&set, &group.id, &result.report.steps, &test_items);
                set.subgroups.extend(suggestion);
            }
        }
        let mut flows: Vec<FlowGraph> = Vec::new();
        let mut sources = BTreeMap::new();
        // The flow view of a function uses the head tree, so it shows the final names. The statuses
        // come from the last step that changed the function: no later step changed its structure.
        let head_ws = self.workspace(&set.head_tree).unwrap_or_default();
        let mut last_inputs: BTreeMap<String, (String, BTreeMap<String, semilla_core::view::Status>, usize)> = BTreeMap::new();
        for (step, outcome) in set.steps.iter().zip(&result.outcomes) {
            for (item, (statuses, removed)) in semilla_verify::views::flow_inputs(std::slice::from_ref(outcome)) {
                last_inputs.insert(item, (step.tree_after.clone(), statuses, removed));
            }
        }
        let later_renames: Vec<BTreeMap<String, String>> = result.outcomes.iter().map(|o| o.renames.items.clone()).collect();
        for (item, (tree, statuses, removed)) in &last_inputs {
            // A later rename of the function itself gives its head path.
            let head_path = later_renames.iter().fold(item.clone(), |path, map| map.get(&path).cloned().unwrap_or(path));
            let step_ws = self.workspace(tree)?;
            let source = head_ws.items.get(&head_path).or_else(|| step_ws.items.get(item));
            if let Some(flow) = source.and_then(|it| semilla_syntax::flow::flow_view(it, statuses, *removed)) {
                flows.push(flow);
            }
        }
        for (step, outcome) in set.steps.iter().zip(&result.outcomes) {
            let before = self.workspace(&step.tree_before)?;
            let after = self.workspace(&step.tree_after)?;
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
        let mut graph: WorkspaceGraph = semilla_verify::views::graph(&result.outcomes, &head);
        self.add_external_calls(&mut graph, &head, &set.head_tree);
        Ok(semilla_ui::UiData { changeset: set, report: result.report, tree: result.tree, flows, graph, sources })
    }
}

fn summary_line(report: &Report) -> String {
    let claims: usize = report.steps.iter().map(|s| s.claims.len()).sum();
    let uncovered: usize = report.steps.iter().map(|s| s.uncovered.len()).sum();
    let phase1_ok = report.phase == "syntax"
        && report.organization.is_empty()
        && report.steps.iter().all(|s| s.errors.is_empty() && s.uncovered.is_empty() && s.claims.iter().all(|c| c.status.is_verified() || c.status.is_pending()));
    let verdict = if report.accepted {
        "accepted".to_string()
    } else if phase1_ok && report.pending > 0 {
        format!("phase 1 ok, {} body proof(s) pending", report.pending)
    } else if phase1_ok {
        "phase 1 ok".to_string()
    } else {
        "rejected".to_string()
    };
    let mut text = format!(
        "result: {verdict} · {} steps · {claims} claims · {uncovered} uncovered · score {:.2}\n",
        report.steps.len(),
        report.score
    );
    for error in &report.organization {
        text.push_str(&format!("  ✗ {error}\n"));
    }
    for warning in &report.warnings {
        text.push_str(&format!("  ! {warning}\n"));
    }
    for pair in &report.pairs {
        text.push_str(&format!("  ! pair: {}\n", pair.text()));
        if pair.refusal.is_none() && pair.reviewer.is_none() {
            text.push_str(&format!("      if the two items are really different, run `semilla pair refuse '{}' '{}' --reason \"…\"` (in a replay plan: add it to `refusals`)\n", pair.old, pair.new));
        }
    }
    if report.lines.total() > 0 {
        text.push_str(&format!("  {}\n", report.lines.summary()));
    }
    text
}
