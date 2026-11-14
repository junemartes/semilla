//! The operations of semilla. The CLI and the MCP server both call them.
//!
//! The work in progress is the session: `.semilla/session.json`. It holds the change set and the
//! open step. `tag` stores the change set as a git note on the commit, and keeps the step trees
//! reachable with refs under `refs/semilla/`.

use semilla_core::text::put_line;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use semilla_core::view::{FlowGraph, TreeNode, WorkspaceGraph};
use semilla_core::{Anchor, ChangeSet, ClaimStatus, Claim, Compare, Group, GroupKind, Report, Step, Subgroup};
use semilla_syntax::Workspace;
use semilla_verify::{Options, StepOutcome};
use serde::{Deserialize, Serialize};

use crate::git::Repo;
use crate::buildcode::{BuildCode, Mode};
use crate::oracle::{NormalOracle, SmtOracle};

/// A step that the agent started but did not end.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenStep {
    pub intent: String,
    pub tree_before: String,
    pub claims: Vec<Claim>,
    /// The reasons for the spotlight facts, by the anchor of the fact (`semilla reason`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasons: BTreeMap<String, String>,
}

pub use semilla_core::ops::optional_args;

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
    /// The steps that `step --fast` stored with a phase 1 result only. The value is the phase 2
    /// state: `None` when no later command computed it, or the problems of the rejection.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fast_steps: BTreeMap<usize, Option<String>>,
    /// The commit that `tag` stored this change set on. A new replay can then replace the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagged: Option<String>,
    /// The last `semilla step`, if Semilla rejected it and no step came after it (batch 5x,
    /// part C). `tag` and `status` then say the cause, and not "run `semilla step`".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected: Option<RejectedStep>,
}

/// A rejected `semilla step`: nothing is stored for it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RejectedStep {
    pub intent: String,
    /// The cause, in a short form.
    pub cause: String,
    /// The command that runs the step again with the missing parts, if a command can succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Session {
    /// The fix for an edit that has no step. After a rejected step, the fix is not the same
    /// command again: it is the command with the missing parts, or a change of the code.
    pub fn step_fix(&self) -> String {
        match &self.rejected {
            Some(rejected) => {
                let again = match &rejected.command {
                    Some(command) => format!("Run it again with the missing parts: `{command}`"),
                    None => "Correct the cause, then run the step again".to_string(),
                };
                format!("The last `semilla step` ({}) was rejected, and Semilla stored nothing: {}. {again}", semilla_core::text::quoted(&rejected.intent), rejected.cause)
            }
            None => "Run `semilla step \"<intent>\"` for the edit".to_string(),
        }
    }
}

/// The semilla application on one repository.
pub struct App {
    pub repo: Repo,
    workspaces: BTreeMap<String, Workspace>,
    /// For tests: skip the MIR body check.
    pub no_bodies: bool,
    /// What the command does with the build code that the change set changes (design 28): it
    /// runs it in the sandbox, or, with `--no-build-code`, it does not run it.
    pub mode: Mode,
    /// The base trees that a commit gives: the tree of the parent of a commit that this process
    /// checked against its note. The build code of such a tree is trusted.
    trusted_bases: std::cell::RefCell<BTreeSet<String>>,
}

/// Gives no body data for the steps that the command does not check (`--no-build-code`): these
/// steps get the answers of phase 1.
struct Guard<'a> {
    inner: &'a dyn semilla_verify::BodyOracle,
    skip: &'a BTreeSet<usize>,
    code: &'a BuildCode,
}

impl semilla_verify::BodyOracle for Guard<'_> {
    fn compare(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String]) -> Result<Vec<semilla_verify::BodyComparison>> {
        if self.skip.contains(&step.index) { Ok(Vec::new()) } else { self.inner.compare(step, renames, added) }
    }

    fn compare_with(&self, step: &Step, renames: &BTreeMap<String, String>, added: &[String], opaque: &dyn semilla_verify::OpaqueInputs) -> Result<Vec<semilla_verify::BodyComparison>> {
        if self.skip.contains(&step.index) { Ok(Vec::new()) } else { self.inner.compare_with(step, renames, added, opaque) }
    }

    fn facts(&self, step: &Step) -> Result<Option<Vec<semilla_verify::BodyFacts>>> {
        if self.skip.contains(&step.index) { Ok(None) } else { self.inner.facts(step) }
    }

    fn truths(&self, step: &Step) -> Result<[Vec<semilla_verify::ConfigTruth>; 2]> {
        if self.skip.contains(&step.index) { Ok(Default::default()) } else { self.inner.truths(step) }
    }

    fn tests(&self, step: &Step) -> Result<Option<[std::collections::BTreeSet<String>; 2]>> {
        if self.skip.contains(&step.index) { Ok(None) } else { self.inner.tests(step) }
    }

    fn made(&self, step: &Step) -> Result<Option<[semilla_verify::made::Made; 2]>> {
        if self.skip.contains(&step.index) { Ok(None) } else { self.inner.made(step) }
    }

    fn build_code_files(&self, step: &Step) -> Vec<(String, String)> {
        self.code.host_files(step)
    }

    fn set_selections(&self, step: &Step, selections: &[semilla_verify::Selection]) -> Result<bool> {
        if self.skip.contains(&step.index) { Ok(false) } else { self.inner.set_selections(step, selections) }
    }
}

/// Puts the mark on each node of the change tree with the result `code equal` whose step used
/// changed build code (design 28, invariant I6). The count of a parent node says how many of its
/// `code equal` nodes have the mark. Returns that count.
fn mark_tree(node: &mut TreeNode, note: &semilla_core::report::BuildCodeNote, lone: Option<usize>) -> usize {
    let marked: usize = node.children.iter_mut().map(|child| mark_tree(child, note, lone)).sum();
    let mark = if node.steps.is_empty() { lone.and_then(|step| note.mark_of(step)) } else { node.steps.iter().filter_map(|step| note.mark_of(*step)).min() };
    if node.compare == Some(Compare::CodeEqual) {
        node.build_code = mark;
    }
    if node.children.is_empty() {
        // The same rule as the count of the tree: an uncovered node has no compare result.
        let equal = node.compare == Some(Compare::CodeEqual) && node.status != semilla_core::view::Status::Uncovered;
        return usize::from(equal && mark.is_some());
    }
    if let Some(summary) = node.summary.as_mut().filter(|_| marked > 0) {
        let parts: Vec<String> = summary.split(" · ").map(|part| if part.ends_with(" code equal") { format!("{part}{}", note.count_text(marked)) } else { part.to_string() }).collect();
        *summary = parts.join(" · ");
    }
    marked
}

/// Makes the report of a step a report with no check (design 28): each fact has the compare
/// result `not checked`, and no claim is verified. A problem that needs no build stays (a claim with
/// bad syntax, a spotlight with no reason).
fn mark_not_checked(step: &mut semilla_core::report::StepReport, reason: &str) {
    for fact in &mut step.facts {
        fact.compare = Compare::NotChecked;
        fact.affected.clear();
        fact.equal_sites.clear();
    }
    for claim in &mut step.claims {
        if claim.status.is_verified() || claim.status.is_pending() {
            claim.status = ClaimStatus::NotChecked { reason: reason.to_string() };
            claim.category = None;
            claim.effect = None;
            claim.witness = None;
            claim.affected.clear();
            claim.complete_calls.clear();
        }
    }
}

/// True if a step has a problem that is not a missing check: an error, a change with no fact, a
/// spotlight with no reason, or a claim that is wrong.
fn hard_problem(step: &semilla_core::report::StepReport) -> bool {
    !step.errors.is_empty()
        || !step.uncovered.is_empty()
        || step.facts.iter().any(|fact| fact.spotlight && fact.reason.is_none())
        || step.claims.iter().any(|claim| !(claim.status.is_verified() || claim.status.is_pending() || matches!(claim.status, ClaimStatus::NotChecked { .. })))
}

/// The problems of a rejected step, one text for each: a fact with no reason, a claim that is
/// not verified, an error, a change with no fact.
pub(crate) fn step_problems(step: &semilla_core::report::StepReport) -> Vec<String> {
    let mut problems = Vec::new();
    for fact in step.facts.iter().filter(|f| f.spotlight && f.reason.is_none()) {
        problems.push(format!("{} needs a reason ({})", fact.description, fact.spotlight_why.as_deref().unwrap_or("")));
    }
    for claim in step.claims.iter().filter(|c| !c.status.is_verified()) {
        problems.push(format!("claim {} is {}{}", claim.claim, claim.status.text(), claim.hint.as_ref().map(|h| format!("; {h}")).unwrap_or_default()));
    }
    problems.extend(step.errors.iter().cloned());
    problems.extend(step.uncovered.iter().map(|u| format!("uncovered {}", u.description)));
    problems
}

/// The exact commands that store a rejected step again (brief 5h, C3). `semilla undo` removes
/// only the last step, so a step that is not the last step needs one `undo` for each step from
/// the last step back to it. The files stay. Then each removed step is stored again from its tree.
///
/// `reset` is the base commit when `HEAD` is not the base and the route removes each step: a new
/// first step starts at `HEAD`, so `HEAD` must go back to the base first.
pub(crate) fn redo_route(set: &ChangeSet, report: &semilla_core::report::StepReport, reset: Option<&str>, then: &str) -> String {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let Some(first) = set.steps.iter().position(|step| step.index == report.step) else { return String::new() };
    let count = set.steps.len() - first;
    let mut text = match reset {
        Some(base) => format!("first run `git reset --soft {base}`: `HEAD` goes back to the base of the change set, and the files stay.\n"),
        None => String::new(),
    };
    text += &if count == 1 {
        format!("to fix step {}: run `semilla undo` (the files stay), then store the step again:\n", report.step)
    } else {
        format!(
            "to fix step {} (it is not the last step): run `semilla undo` {count} times. This removes the steps {} to {} from the session, and the files stay. Then store each step again, in this order:\n",
            report.step,
            report.step,
            set.steps[set.steps.len() - 1].index
        )
    };
    for step in &set.steps[first..] {
        let mut parts = vec![format!("semilla step {}", quote(&step.intent))];
        if let Some(group) = set.groups.iter().find(|group| group.steps.contains(&step.index)).filter(|group| group.title != step.intent) {
            parts.push(format!("--group {}", quote(&group.title)));
        }
        for claim in &step.claims {
            let mut words = vec![claim.op.clone()];
            words.extend(claim.at.as_ref().map(|at| at.to_string()));
            words.extend(claim.args.iter().map(|(key, value)| format!("{key}={}", value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()))));
            parts.push(format!("--claim {}", quote(&words.join(" "))));
        }
        for (key, reason) in &step.reasons {
            parts.push(format!("--reason {}", quote(&format!("{key}={reason}"))));
        }
        if step.index == report.step {
            for fact in report.facts.iter().filter(|f| f.spotlight && f.reason.is_none()) {
                let place = fact.at.as_ref().map(|at| at.to_string()).unwrap_or_else(|| fact.part.clone());
                parts.push(format!("--reason {}", quote(&format!("{place}=<why>"))));
            }
        }
        put_line(&mut text, &format!("  git restore --source={} --worktree -- . && {}", step.tree_after, parts.join(" ")));
    }
    text.push_str(&format!("`git restore` writes the files of the step. A file that a later step added, and that git does not track, stays: move it away first. {then}"));
    text
}

/// The text after `result: ` of the result line of an output, if the output has one. Each line
/// of an output has the escape of the text of the agent, so that text cannot start a line.
pub(crate) fn result_word(text: &str) -> Option<&str> {
    text.lines().find_map(|line| line.strip_prefix("result: "))
}

/// True if the result line of an output gives the exit code 1 (batch 5y, A3): the result is
/// rejected or not complete. With `full`, a result with no phase 2 fails too (the result of a
/// commit for a reviewer). An output with no result line does not fail.
pub(crate) fn result_fails(text: &str, full: bool) -> bool {
    result_word(text).is_some_and(|word| word.starts_with("rejected") || word.starts_with("not complete") || (full && !word.starts_with("accepted")))
}

/// What this machine knows about the Semilla check of a commit (batch 5y, A2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitCheck {
    /// No note that belongs to the commit.
    None,
    /// The result that this machine computed for the commit is accepted.
    Accepted,
    /// The stored result of this machine, or phase 1, rejects the change set of the commit.
    Rejected,
    /// The note belongs to the commit and phase 1 accepts it, but this machine has no full result.
    DataOnly,
}

/// Adds a problem of the change set to a result. The result line and the problem agree (B1): no
/// "accepted" line with a rejection below it.
fn add_problem(result: &mut CheckResult, problem: String) {
    result.report.organization.push(problem);
    result.report.accepted = false;
    result.text = format!("{}{}", semilla_verify::views::text_report(&result.report.steps), summary_line(&result.report));
}

/// A rejected result. The CLI prints its text as it is: the first line is "result: rejected",
/// with no "error:" before it (brief 5f, B1).
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

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
        Ok(App { repo, workspaces: BTreeMap::new(), no_bodies: std::env::var_os("SEMILLA_NO_BODIES").is_some(), mode: Mode::default(), trusted_bases: Default::default() })
    }

    /// The review state of the reviewer (the decisions in the UI). It is in the cache folder of
    /// the user: the agent cannot write a decision of the reviewer (I5).
    pub fn review_file(&self) -> std::path::PathBuf {
        self.repo.cache_dir().join("review.json")
    }

    /// The key of the tools that a result depends on: the Semilla binary, the MIR driver, the
    /// solver, the toolchain, and the build settings outside the tree. A stored result is valid
    /// only for the same key.
    pub fn tools_key(&self) -> String {
        let file = |path: Option<std::path::PathBuf>| match path {
            Some(path) => {
                let meta = std::fs::metadata(&path).ok();
                format!("{}:{}:{:?}", std::fs::canonicalize(&path).unwrap_or(path).display(), meta.as_ref().map(|m| m.len()).unwrap_or(0), meta.and_then(|m| m.modified().ok()))
            }
            None => "none".to_string(),
        };
        let driver = semilla_normal::cargo::find_driver().ok();
        let settings = semilla_normal::cargo::settings_key(&self.repo.build_base(), driver.as_deref());
        let solver = SmtOracle::for_repo(&self.repo).path().map(std::path::Path::to_path_buf);
        let text = format!("{}|{}|{}|{settings}", file(std::env::current_exe().ok()), file(driver), file(solver));
        format!("{:016x}", crate::store::stable_hash(text.as_bytes()))
    }

    /// One warning for each tool of the check that is in the repository folder. The agent controls
    /// each file there, so a result of such a tool is not a result for a reviewer (I5). Semilla
    /// uses such a tool only when the path is explicit (a variable, or its own binary).
    fn tool_warnings(&self) -> Vec<String> {
        let tools = [
            ("the Semilla binary", std::env::current_exe().ok()),
            ("the MIR driver", if self.no_bodies { None } else { semilla_normal::cargo::find_driver().ok() }),
            ("the solver", SmtOracle::for_repo(&self.repo).path().map(std::path::Path::to_path_buf)),
        ];
        tools
            .into_iter()
            .filter_map(|(name, path)| path.filter(|path| crate::store::is_inside(path, &self.repo.root)).map(|path| format!("{name} ({}) is in the repository folder: the agent can change it, so this is not a result for a reviewer", path.display())))
            .collect()
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
        // Cargo gives the packages and the targets of the tree, so Semilla and cargo agree on
        // each root file (I4). The name of a file does not decide if it is Rust code.
        let metadata = semilla_normal::cargo::cargo_metadata(&dir).with_context(|| format!("read the cargo targets of tree {tree}"))?;
        crate::timing("cargo metadata", started);
        let mut ws = Workspace::load_with_metadata(&dir, &metadata).with_context(|| format!("load the workspace of tree {tree}"))?;
        // The git tree has each entry, also a submodule and a file in an ignored folder. Its
        // modes and hashes are exact, so the diff compares them instead of the folder.
        ws.files = self.repo.tree_entries(tree)?;
        crate::timing("load the syntax workspace", started);
        self.workspaces.insert(tree.to_string(), ws.clone());
        Ok(ws)
    }

    // ----- the build code (design 28) -----

    /// True if the build code of the base tree of a change set is trusted: the base is the start
    /// point of the reviewer. This is the tree of the parent of the checked commit, the tree of a
    /// commit of the history of `HEAD`, or the empty tree. A session file can name each tree as
    /// its base, so a base that no commit gives is not trusted.
    fn base_is_trusted(&self, base: &str) -> bool {
        self.trusted_bases.borrow().contains(base) || self.base_commit(base).is_some() || self.repo.git(&["hash-object", "-t", "tree", "/dev/null"]).is_ok_and(|empty| empty.trim() == base)
    }

    /// The build code state of a change set: for each tree, if it has the build code of the base
    /// tree (design 28, the test of A1). With a base that is not trusted, each tree counts as a
    /// tree with changed build code.
    pub fn build_code(&mut self, set: &ChangeSet) -> Result<BuildCode> {
        let started = std::time::Instant::now();
        let base = set.base_tree.clone();
        let mut code = BuildCode { base: base.clone(), ..Default::default() };
        let trusted = self.base_is_trusted(&base);
        let key = self.tools_key();
        let mut state: BTreeMap<String, Vec<crate::buildcode::Part>> = BTreeMap::new();
        let trees: Vec<&String> = std::iter::once(&set.base_tree).chain(set.steps.iter().flat_map(|step| [&step.tree_before, &step.tree_after])).chain(std::iter::once(&set.head_tree)).filter(|tree| !tree.is_empty()).collect();
        for tree in trees {
            if state.contains_key(tree) {
                continue;
            }
            let parts = if !trusted {
                vec![crate::buildcode::Part::other("the base tree of the change set", "no commit of this branch has this tree, so its build code is not trusted")]
            } else if *tree == base {
                Vec::new()
            } else {
                match self.tree_build_code(&base, tree, &key) {
                    Ok(parts) => parts,
                    Err(error) => match (error.downcast_ref::<StaleLockTree>(), error.downcast_ref::<crate::buildcode::GraphFailure>()) {
                        (Some(stale), _) => bail!("{}", stale_lock_text(set, &stale.tree)),
                        (_, Some(failure)) => bail!("{}", graph_failure_text(set, if failure.base { &base } else { tree }, &failure.line)),
                        _ => return Err(error),
                    },
                }
            };
            if parts.is_empty() { code.clean.insert(tree.clone()) } else { code.trees.insert(tree.clone()) };
            state.insert(tree.clone(), parts);
        }
        for step in &set.steps {
            code.steps.push(crate::buildcode::StepCode { index: step.index, before: code.trees.contains(&step.tree_before), parts: state.get(&step.tree_after).cloned().unwrap_or_default() });
        }
        // The keys of the folder sets (design 28.9): builds share a folder that a build can
        // write only if they have the same build code.
        if trusted && !code.clean.is_empty() {
            code.shared = self.shared_key(&base, &key);
        }
        for tree in code.trees.clone() {
            let files = self.workspace(&tree).ok().map(|ws| ws.files.clone());
            code.keys.insert(tree.clone(), crate::buildcode::marked_key(&base, &tree, &state[&tree], files.as_ref(), !trusted));
        }
        crate::timing("the build code test", started);
        Ok(code)
    }

    /// The key of the shared folder set of a trusted base tree: the digest of its build code
    /// state. The result is in the cache folder of the user, by the tree and the tools. If
    /// Semilla cannot read the workspace or the dependency graph of the base, the key is the
    /// base tree itself: then only the trees of change sets on this base share the folders.
    fn shared_key(&mut self, base: &str, key: &str) -> String {
        let folder = self.repo.cache_dir().join("host");
        let file = folder.join(format!("{base}-{key}.state"));
        let fallback = format!("tree-{base}");
        if let Some(digest) = std::fs::read_to_string(&file).ok().filter(|text| *text == fallback || (text.len() == 32 && text.chars().all(|digit| digit.is_ascii_hexdigit()))) {
            return digest;
        }
        let digest = (|| -> Result<String> {
            let ws = self.workspace(base)?;
            let dir = self.repo.materialize(base)?;
            let graph = folder.join(format!("{base}-{key}.graph.json"));
            let host = match std::fs::read_to_string(&graph).ok().and_then(|text| serde_json::from_str(&text).ok()) {
                Some(host) => host,
                None => {
                    let metadata = semilla_normal::cargo::cargo_metadata_full(&dir, &self.repo.store.private_path("metadata"))?;
                    let host = crate::buildcode::HostInfo::from_metadata(&metadata, std::fs::read_to_string(dir.join("Cargo.lock")).ok().as_deref())?;
                    let temporary = self.repo.store.private_path("host");
                    if std::fs::create_dir_all(&folder).is_ok() && std::fs::write(&temporary, serde_json::to_string(&host)?).is_ok() {
                        let _ = std::fs::rename(&temporary, &graph);
                    }
                    host
                }
            };
            Ok(crate::buildcode::state_digest(&crate::buildcode::Side { dir: &dir, ws: &ws }, &host))
        })();
        let digest = digest.unwrap_or(fallback);
        let temporary = self.repo.store.private_path("host");
        if std::fs::create_dir_all(&folder).is_ok() && std::fs::write(&temporary, &digest).is_ok() {
            let _ = std::fs::rename(&temporary, &file);
        }
        digest
    }

    /// The changed build code parts of a tree against the base tree. The result is in the cache
    /// folder of the user, by the two trees and the tools.
    fn tree_build_code(&mut self, base: &str, tree: &str, key: &str) -> Result<Vec<crate::buildcode::Part>> {
        let folder = self.repo.cache_dir().join("host");
        let file = folder.join(format!("{base}-{tree}-{key}.json"));
        if let Some(parts) = std::fs::read_to_string(&file).ok().and_then(|text| serde_json::from_str(&text).ok()) {
            return Ok(parts);
        }
        std::fs::create_dir_all(&folder)?;
        let base_dir = self.repo.materialize(base)?;
        let tree_dir = self.repo.materialize(tree)?;
        // A base that is not a cargo workspace (the empty tree) has no code and no build code.
        let base_ws = match self.workspace(base) {
            Ok(ws) => ws,
            Err(_) => Workspace { files: self.repo.tree_entries(base)?, ..Default::default() },
        };
        let tree_ws = self.workspace(tree)?;
        let names: BTreeMap<std::path::PathBuf, &str> = [(base_dir.clone(), base), (tree_dir.clone(), tree)].into_iter().collect();
        let store = &self.repo.store;
        let write = |file: &std::path::Path, text: String| {
            let temporary = store.private_path("host");
            if std::fs::write(&temporary, text).is_ok() {
                let _ = std::fs::rename(&temporary, file);
            }
        };
        // True: the lock file of the base tree is the problem. False: the lock file of the tree.
        let stale: std::cell::RefCell<Option<bool>> = std::cell::RefCell::new(None);
        let mut full = |dir: &std::path::Path| -> Result<crate::buildcode::HostInfo> {
            let cached = names.get(dir).map(|name| folder.join(format!("{name}-{key}.graph.json")));
            if let Some(host) = cached.as_ref().and_then(|file| std::fs::read_to_string(file).ok()).and_then(|text| serde_json::from_str(&text).ok()) {
                return Ok(host);
            }
            let metadata = semilla_normal::cargo::cargo_metadata_full(dir, &store.private_path("metadata")).inspect_err(|error| {
                if error.is::<semilla_normal::cargo::StaleLock>() {
                    stale.borrow_mut().get_or_insert(dir == base_dir);
                }
            })?;
            let lock = std::fs::read_to_string(dir.join("Cargo.lock")).ok();
            let host = crate::buildcode::HostInfo::from_metadata(&metadata, lock.as_deref())?;
            if let Some(file) = &cached {
                write(file, serde_json::to_string(&host)?);
            }
            Ok(host)
        };
        let parts = crate::buildcode::changed_build_code(&crate::buildcode::Side { dir: &base_dir, ws: &base_ws }, &crate::buildcode::Side { dir: &tree_dir, ws: &tree_ws }, &mut full);
        // A lock file that does not agree with the manifests is not a decision about build code.
        // Semilla cannot build such a tree, so the command stops with its own message. The
        // result does not go into the cache.
        if let Some(of_base) = *stale.borrow() {
            return Err(anyhow::Error::new(StaleLockTree { tree: if of_base { base.to_string() } else { tree.to_string() } }));
        }
        // A failed read of the dependency graph is not a decision about build code too (batch
        // 5n, A6). The command stops with its own message, and nothing goes into the cache.
        let parts = parts.map_err(anyhow::Error::new)?;
        write(&file, serde_json::to_string(&parts)?);
        Ok(parts)
    }

    /// The first line of an output of a reviewer command for a change set that changes build
    /// code: the line that names the change and the mark, or the result that is not complete
    /// (`--no-build-code`).
    pub fn build_code_notice(&self, report: &Report) -> Option<String> {
        let note = report.build_code.as_ref().filter(|note| !note.changes.is_empty())?;
        if !note.ran {
            return summary_line(report).lines().next().map(|line| format!("! {line}"));
        }
        // With a missing limit of the sandbox, the line does not say "in the sandbox".
        let missing = semilla_normal::sandbox::missing().map(|missing| missing.state());
        Some(note.ran_line_with(missing.as_deref()))
    }

    // ----- the tag tools -----

    /// Starts a step. The current working tree is the "before" tree.
    pub fn step_begin(&mut self, intent: &str) -> Result<String> {
        if intent.trim().is_empty() {
            bail!("the intent has no text. The reviewer reads the intent: say what the step does");
        }
        let mut session = self.load_session()?;
        if session.open.is_some() {
            bail!("a step is already open; end it with `semilla old-step end` first");
        }
        // A tagged change set is closed (T8): the next step starts a new change set at `HEAD`.
        let mut closed = None;
        if session.tagged.is_some() && session.replay.is_none() {
            closed = session.tagged.take();
            session = Session::default();
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
        session.open = Some(OpenStep { intent: intent.to_string(), tree_before: tree.clone(), claims: Vec::new(), reasons: BTreeMap::new() });
        self.save_session(&session)?;
        let mut text = format!("step {} started at tree {tree}", session.changeset.steps.len());
        if session.changeset.steps.is_empty() {
            if let Some(notice) = self.base_notice(&ChangeSet::default(), None) {
                text = format!("{notice}\n{text}");
            }
        }
        if let Some(commit) = closed {
            text = format!("the change set of {commit} is tagged and closed; a new change set starts at HEAD\n{text}");
        }
        Ok(text)
    }

    /// Adds a claim to the open step. Returns the claim ID.
    pub fn claim(&mut self, op: &str, at: Option<&str>, args: serde_json::Map<String, serde_json::Value>) -> Result<String> {
        let mut session = self.load_session()?;
        let used = session.changeset.steps.iter().flat_map(|s| &s.claims).chain(session.open.iter().flat_map(|o| &o.claims));
        let highest = used.filter_map(|c| c.id.strip_prefix('c').and_then(|n| n.parse::<usize>().ok())).max().unwrap_or(0);
        let number = highest.max(session.last_claim) + 1;
        let id = format!("c{number}");
        let open = session.open.as_mut().context("`semilla claim` is a command of the old protocol (for the replay), and no step of the old protocol is open. In the usual flow, give each claim to the step: `semilla step \"<intent>\" --claim '…'`")?;
        if let Some(message) = semilla_core::ops::retired(op).or_else(|| crate::onestep::retired_claim(op)) {
            bail!("{message}");
        }
        let Some(info) = semilla_core::ops::op(op) else {
            bail!("unknown operation `{op}`; `semilla ops` lists all operations");
        };
        if let Some(message) = args.keys().find_map(|key| semilla_core::ops::retired_arg(key)) {
            bail!("{message}");
        }
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

    /// Stores the reason for a fact in the spotlight with the open step (design 22.5, rule 5).
    pub fn reason(&mut self, at: &str, text: &str) -> Result<String> {
        let mut session = self.load_session()?;
        let open = session.open.as_mut().context("`semilla reason` is a command of the old protocol (for the replay), and no step of the old protocol is open. In the usual flow, give each reason to the step: `semilla step \"<intent>\" --reason 'ANCHOR=TEXT'`")?;
        let anchor: Anchor = at.parse().map_err(|e| anyhow::anyhow!("{e}"))?;
        let key = anchor.to_string();
        let message = if text.trim().is_empty() {
            open.reasons.remove(&key);
            format!("the reason for {key} is removed")
        } else {
            open.reasons.insert(key.clone(), text.trim().to_string());
            format!("the reason for {key} is stored; the verifier does not check it, the reviewer reads it")
        };
        self.save_session(&session)?;
        Ok(message)
    }

    /// Removes a claim from the open step.
    pub fn unclaim(&mut self, id: &str) -> Result<()> {
        let mut session = self.load_session()?;
        let open = session.open.as_mut().context("`semilla unclaim` is a command of the old protocol (for the replay), and no step of the old protocol is open. In the usual flow, `semilla undo` removes the last step; then run `semilla step` again with the correct claims")?;
        let count = open.claims.len();
        open.claims.retain(|claim| claim.id != id);
        if open.claims.len() == count {
            bail!("the open step has no claim {id}");
        }
        self.save_session(&session)
    }

    /// Changes the intent of the open step.
    pub fn step_edit(&mut self, intent: Option<&str>) -> Result<String> {
        let mut session = self.load_session()?;
        let open = session.open.as_mut().context("no step of the old protocol is open; `semilla old-step reopen` opens the last step")?;
        if let Some(intent) = intent {
            open.intent = intent.to_string();
        }
        let text = format!("the open step: intent {}", semilla_core::text::quoted(&open.intent));
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
        session.open = Some(OpenStep { intent: step.intent, tree_before: step.tree_before, claims: step.claims, reasons: step.reasons });
        self.save_session(&session)?;
        Ok(format!("step {} is open again, with {claims} claim(s); edit, claim or unclaim, then `semilla old-step end`", step.index))
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
        session.changeset.steps.push(Step { index, intent: open.intent, claims: open.claims, tree_before: open.tree_before, tree_after: tree.clone(), reasons: open.reasons });
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
        let job = crate::background::Job { key: "session".into(), hash: crate::background::changeset_hash(set), changeset: Some(set.clone()), mode: self.mode };
        crate::background::enqueue(self, &job)?;
        crate::background::spawn_worker(self)
    }

    /// The state of the background check of the session, or of a tagged commit.
    pub fn status(&mut self, commit: Option<&str>, wait: Option<u64>, verbose: bool) -> Result<String> {
        let (key, set) = match commit {
            Some(commit) => {
                let commit = self.repo.git(&["rev-parse", commit])?;
                let note = self.repo.read_note(&commit).with_context(|| format!("commit {commit} has no semilla note; tag it first"))?;
                let set: ChangeSet = serde_json::from_str(&note)?;
                if let Err(error) = self.check_binding(&commit, &set) {
                    let error = error.to_string();
                    // A5: the note problem is the first line.
                    if error.starts_with("the note does not agree") {
                        return Ok(format!("{error}\n{commit}: rejected\nresult: rejected\n"));
                    }
                    return Ok(format!("{commit}: rejected\nresult: rejected · {error}\n"));
                }
                (commit, set)
            }
            None => {
                let session = self.load_session()?;
                // B1: a tagged change set is closed. It is not the session.
                if let (Some(tagged), None, None) = (&session.tagged, &session.open, &session.replay) {
                    // Batch 5y, A1: the session file is not a git object. The commit is "tagged"
                    // only if git has a note that belongs to it.
                    if self.note_set(tagged).is_none() {
                        return Ok(format!(
                            "no open change set; the session names commit {tagged}, but that commit has no Semilla check\n{}The next `semilla step` starts a new change set at HEAD.\n",
                            self.head_check_line(None)
                        ));
                    }
                    return Ok(format!(
                        "no open change set; the last tagged commit is {tagged}\n{}For its result, run `semilla status {}`. The next `semilla step` starts a new change set at HEAD.\n",
                        self.head_check_line(Some(tagged)),
                        &tagged[..tagged.len().min(7)]
                    ));
                }
                if session.changeset.steps.is_empty() && session.open.is_none() && session.replay.is_none() {
                    // No step and no tagged change set (a new clone, or a session with no step).
                    return Ok(format!("no open change set\n{}The next `semilla step` starts a change set at HEAD.\n", self.head_check_line(None)));
                }
                ("session".to_string(), session.changeset)
            }
        };
        let hash = crate::background::changeset_hash(&set);
        let mut state = crate::background::state(self, &key, &hash);
        // A process with no cache that stays (`--fresh`, or no cache folder that Semilla can
        // write) has no worker: it runs the check itself.
        let wait = if self.repo.store.is_temporary() { Some(wait.unwrap_or(600)) } else { wait };
        // No check for this change set exists on this machine (a stale report, a forged or copied
        // report note, a new clone): start the check now.
        if matches!(state, crate::background::State::Stale | crate::background::State::Missing) && !set.steps.is_empty() {
            let job = crate::background::Job { key: key.clone(), hash: hash.clone(), changeset: (key == "session").then(|| set.clone()), mode: self.mode };
            crate::background::enqueue(self, &job)?;
            crate::background::spawn_worker(self)?;
            state = crate::background::state(self, &key, &hash);
        }
        if let Some(seconds) = wait {
            let timeout = std::time::Duration::from_secs(seconds);
            let started = std::time::Instant::now();
            state = crate::background::wait(self, &key, &hash, timeout)?;
            // A lost or failed job: start it again, and run it in this process (M1).
            if state != crate::background::State::Ready && !set.steps.is_empty() && started.elapsed() < timeout {
                let job = crate::background::Job { key: key.clone(), hash: hash.clone(), changeset: (key == "session").then(|| set.clone()), mode: self.mode };
                crate::background::enqueue(self, &job)?;
                state = crate::background::wait(self, &key, &hash, timeout.saturating_sub(started.elapsed()))?;
            }
        }
        // Rule T6: no result comes from a git note or from a stored text. Phase 1 runs again now.
        // Phase 2 comes only from a report that this machine computed for this change set.
        let saved = self.no_bodies;
        self.no_bodies = true;
        let phase1 = self.check_set(&set, commit.is_some());
        self.no_bodies = saved;
        let phase1 = phase1?;
        let stored = crate::background::stored_for(self, &key, &hash);
        // The first line: the state of the check of this key. The jobs of other keys are not
        // the business of this result (trial 5: "queued; 1 job(s)" just after `tag`). The line
        // comes from the result that the command prints: a result that is there is `ready`,
        // also when the worker stored it after the last look at the queue (batch 5m, B2).
        let mut text = match state {
            _ if stored.is_some() => format!("{key}: ready\n"),
            crate::background::State::Queued => format!("{key}: phase 2 runs in the background\n"),
            // The result went away after the last look (a change of the tools): no result.
            crate::background::State::Ready => format!("{key}: missing\n"),
            _ => format!("{key}: {}\n", format!("{state:?}").to_lowercase()),
        };
        if let Some(notice) = self.base_notice(&set, commit.is_some().then_some(key.as_str())).filter(|_| !set.steps.is_empty()) {
            put_line(&mut text, &format!("{notice}"));
        }
        // The build code of the base comes next to the base line (design 28.5).
        if let Some(notice) = self.base_build_code_notice(&set, commit.is_some().then_some(key.as_str())) {
            put_line(&mut text, &notice);
        }
        // Batch 5y, C4.5. The hint of a result for a commit names the commit: `semilla status
        // --verbose` with no commit shows the session, which is a different result.
        let short = |report: &Report| {
            let text = short_text(report);
            match commit {
                Some(name) => text.replace("`semilla status --verbose`", &format!("`semilla status {name} --verbose`")),
                None => text,
            }
        };
        match stored {
            Some(stored) => {
                // Design 28.4: a result that ran the changed build code says so first.
                if let Some(line) = self.build_code_notice(&stored.report).filter(|_| !stored.report.not_complete()) {
                    put_line(&mut text, &format!("{line}"));
                }
                // The agent gets the short line of the conditions (batch 5x, part E). The
                // reviewer (a `status` with a commit) gets the full lines.
                let lines = match (&stored.report.configs, commit) {
                    (Some(note), None) => note.agent_lines(stored.report.steps.len(), verbose),
                    _ => config_lines(&stored.report),
                };
                for line in lines {
                    put_line(&mut text, &line);
                }
                text.push_str("phase 2 (computed on this machine):\n");
                text.push_str(&if verbose { stored.text } else { short(&stored.report) });
            }
            None => {
                text.push_str("phase 1 (computed now):\n");
                text.push_str(&if verbose { phase1.text } else { short(&phase1.report) });
                if wait.is_some() {
                    let log = std::fs::read_to_string(self.repo.cache_dir().join("worker.log")).unwrap_or_default();
                    let last = log.lines().rev().find(|line| line.contains("failed")).unwrap_or("no message");
                    put_line(&mut text, &format!("phase 2: not ready ({}); the last error of the worker: {last}", format!("{state:?}").to_lowercase()));
                } else {
                    put_line(&mut text, &format!("phase 2: not checked on this machine; the check runs now. Wait for it: `semilla status {}`", commit.map(|c| c.to_string()).unwrap_or_else(|| "--wait".into())));
                }
            }
        }
        // Batch 5k, D8: the problems of the present state that `semilla tag` rejects. They are
        // before the result line, and the result word is `rejected`.
        if commit.is_none() {
            let problems = self.tag_problems(&set)?;
            if !problems.is_empty() {
                text = with_tag_problems(&text, &problems);
            }
        }
        // Batch 5y, A3: the first line agrees with the result line. `ready` alone is the first
        // line of an accepted result only.
        let ready = format!("{key}: ready\n");
        if text.starts_with(&ready) {
            if let Some(word) = ["rejected", "not complete"].into_iter().find(|word| result_word(&text).is_some_and(|result| result.starts_with(word))) {
                text = format!("{key}: ready, {word}\n{}", &text[ready.len()..]);
            }
        }
        Ok(text)
    }

    /// The message for a commit that lacks changes of the last step when the working files have
    /// them (batch 5y, B2): the files (a maximum of five) and the commands. `git commit -am`
    /// does not add a new file.
    fn missing_in_commit(&self, commit_tree: &str, step_tree: &str) -> String {
        let files = self.repo.changed_files(commit_tree, step_tree).unwrap_or_default();
        let mut names: Vec<String> = files.iter().take(5).map(|file| format!("`{}`", semilla_core::text::inline(file))).collect();
        if files.len() > 5 {
            names.push(format!("and {} more", files.len() - 5));
        }
        let list = if names.is_empty() { String::new() } else { format!(": {}", names.join(", ")) };
        format!("the commit HEAD does not have the changes of the last step in {} file(s){list}. The working files have them (`git commit -am` does not add a new file). Run `git add -A`, then `git commit --amend --no-edit`, then `semilla tag`.", files.len())
    }

    /// The problems of the present state (`HEAD` and the working tree) that `semilla tag` rejects
    /// (batch 5k, D8): more than one commit after the base of the change set, and an edit with no
    /// step in a commit or in the working tree. An open step and a replay have their own text.
    fn tag_problems(&mut self, set: &ChangeSet) -> Result<Vec<String>> {
        let session = self.load_session()?;
        let mut problems = Vec::new();
        if set.steps.is_empty() || session.replay.is_some() {
            return Ok(problems);
        }
        let Ok(head) = self.repo.commit_tree("HEAD") else { return Ok(problems) };
        // A difference in formatting only is not an edit (the rule of `tag`).
        let same = |app: &mut Self, tree: &str| -> Result<bool> {
            if tree == set.head_tree {
                return Ok(true);
            }
            let (last, other) = (app.workspace(&set.head_tree)?, app.workspace(tree)?);
            Ok(semilla_syntax::diff(&last, &other, &Default::default()).is_empty())
        };
        let work = self.repo.write_tree()?;
        if head == set.base_tree {
            // No commit of the work yet: the working tree must be the tree of the last step.
            if session.open.is_none() && !same(self, &work)? {
                problems.push(format!("the working tree has an edit after the last step. The edit has no step. {}.", session.step_fix()));
            }
            return Ok(problems);
        }
        // `HEAD` is a commit of the work. Its parent must be the base.
        if self.repo.commit_tree("HEAD^").ok().as_deref() != Some(set.base_tree.as_str()) {
            let base = self.base_commit(&set.base_tree);
            let count = base.as_ref().and_then(|base| self.repo.git(&["rev-list", "--count", &format!("{base}..HEAD")]).ok());
            match (base, count) {
                (Some(base), Some(count)) => problems.push(format!("HEAD has {count} commits after the base `{}` of the change set{}", &base[..base.len().min(7)], self.squash_route(&set.base_tree))),
                _ => problems.push(format!("the change set starts at tree {}, but the parent of HEAD has a different tree; the changes between them have no claim", set.base_tree)),
            }
        }
        if !same(self, &head)? {
            if work != head && same(self, &work)? {
                // The steps are complete in the working tree. The commit is older than a step.
                problems.push(self.missing_in_commit(&head, &set.head_tree));
            } else {
                problems.push(format!("the last step ends at tree {}, but commit HEAD has tree {head}: an edit after the last step has no step. {}, then `git commit --amend --no-edit`, then `semilla tag`.", set.head_tree, session.step_fix()));
            }
        }
        Ok(problems)
    }

    /// The change set of a review target: `session`, or a tagged commit.
    pub fn review_target(&self, target: &str) -> Result<(String, ChangeSet)> {
        if target == "session" {
            return Ok(("session".into(), self.load_session()?.changeset));
        }
        self.commit_changeset(target)
    }

    /// Reads the change set of a tagged commit, and checks that it belongs to the commit.
    pub fn commit_changeset(&self, target: &str) -> Result<(String, ChangeSet)> {
        let commit = self.repo.git(&["rev-parse", target])?;
        let note = self.repo.read_note(&commit).with_context(|| format!("commit {commit} has no semilla note; tag it first"))?;
        let set: ChangeSet = serde_json::from_str(&note)?;
        self.check_binding(&commit, &set)?;
        Ok((commit, set))
    }

    /// A change set belongs to a commit when its head tree is the tree of the commit, and its base
    /// tree is the tree of the parent commit. A copied note (`git notes copy`) fails this check.
    pub fn check_binding(&self, commit: &str, set: &ChangeSet) -> Result<()> {
        let tree = self.repo.commit_tree(commit)?;
        let last = set.steps.last().map(|step| step.tree_after.as_str()).unwrap_or(set.base_tree.as_str());
        if set.head_tree != tree {
            bail!("rejected: the change set does not belong to this commit: its head tree is {}, but commit {commit} has tree {tree}", set.head_tree);
        }
        if last != tree && last != set.head_tree {
            bail!("rejected: the change set does not belong to this commit: its last step ends at tree {last}, but commit {commit} has tree {tree}");
        }
        if let Ok(parent) = self.repo.commit_tree(&format!("{commit}^")) {
            if parent != set.base_tree {
                bail!("rejected: the change set does not belong to this commit: its first step starts at tree {}, but the parent of {commit} has tree {parent}", set.base_tree);
            }
            // The parent of the checked commit is the start point of the reviewer: its build
            // code is trusted (design 28).
            self.trusted_bases.borrow_mut().insert(parent);
        }
        if let Some(problem) = self.refs_problem(commit, set) {
            bail!("{problem}");
        }
        Ok(())
    }

    /// `tag` keeps the tree of each step under `refs/semilla/<commit>/`. A note that gives a
    /// different tree for a step is not the note that `tag` wrote (trial 4, D12).
    fn refs_problem(&self, commit: &str, set: &ChangeSet) -> Option<String> {
        let prefix = format!("refs/semilla/{commit}/");
        let refs = self.repo.git(&["for-each-ref", "--format=%(refname) %(tree)", &prefix]).ok()?;
        let start = "the note does not agree with the step refs";
        let mut steps = 0;
        for line in refs.lines() {
            let Some((name, tree)) = line.split_once(' ') else { continue };
            let name = name.strip_prefix(&prefix).unwrap_or(name);
            let (label, expected) = match name.strip_prefix("step-").and_then(|index| index.parse::<usize>().ok()) {
                Some(index) => {
                    steps += 1;
                    match set.steps.iter().find(|step| step.index == index) {
                        Some(step) => (format!("step {index}"), step.tree_after.as_str()),
                        None => return Some(format!("{start}: `{prefix}{name}` exists, but the note has no step {index}")),
                    }
                }
                None if name == "base" => ("the base".to_string(), set.base_tree.as_str()),
                None => continue,
            };
            if tree != expected {
                return Some(format!("{start}: the note gives tree {expected} for {label}, but `{prefix}{name}` has tree {tree}"));
            }
        }
        (steps > 0 && steps != set.steps.len()).then(|| format!("{start}: the note has {} steps, but `{prefix}` has {steps} step refs", set.steps.len()))
    }

    /// The change set of the note of a commit, if the note belongs to the commit: it parses, it
    /// has a step, and it passes [`App::check_binding`].
    fn note_set(&self, commit: &str) -> Option<(String, ChangeSet)> {
        let note = self.repo.read_note(commit).ok()?;
        let set = serde_json::from_str::<ChangeSet>(&note).ok()?;
        (!set.steps.is_empty() && self.check_binding(commit, &set).is_ok()).then_some((note, set))
    }

    /// The state of the Semilla check of a commit (batch 5y, A2; trial 8). A note that belongs to
    /// the commit is data only: an agent can write it for a change set that Semilla rejects. So
    /// the result decides, and the result is one that this machine computes (rule T6: never a
    /// text of the note). The order: a stored result of this machine for the commit; if none,
    /// phase 1 (no build) for the change set of the note. The answer of phase 1 stays in the
    /// machine cache, with a key from the commit, the note content, and the tools.
    fn commit_check(&mut self, commit: &str, noted: &BTreeSet<String>) -> CommitCheck {
        if !noted.contains(commit) {
            return CommitCheck::None;
        }
        let Some((note, set)) = self.note_set(commit) else { return CommitCheck::None };
        if let Some(stored) = crate::background::stored_for(self, commit, &crate::background::changeset_hash(&set)) {
            return if stored.report.accepted { CommitCheck::Accepted } else { CommitCheck::Rejected };
        }
        // A copy of the repository at a different path has a different cache folder (batch 6a,
        // B3; trial 9). The content record says if this machine did compute the full result for
        // this commit, this note, and these tools. No record: the rule of batch 5y stays.
        if let Some(accepted) = crate::background::content_result(self, commit, &crate::background::changeset_hash(&set)) {
            return if accepted { CommitCheck::Accepted } else { CommitCheck::Rejected };
        }
        let key = crate::store::stable_hash(format!("{commit}\0{note}\0{}", self.tools_key()).as_bytes());
        let file = self.repo.cache_dir().join("commit-check").join(format!("{key:016x}"));
        match std::fs::read_to_string(&file).as_deref() {
            Ok("rejected") => return CommitCheck::Rejected,
            Ok("data") => return CommitCheck::DataOnly,
            _ => {}
        }
        let saved = self.no_bodies;
        self.no_bodies = true;
        let phase1 = self.check_set(&set, true);
        self.no_bodies = saved;
        // An error is not a result: the line says that this machine has no full result.
        let Ok(phase1) = phase1 else { return CommitCheck::DataOnly };
        let rejected = !phase1.report.organization.is_empty() || phase1.report.steps.iter().any(hard_problem);
        let _ = std::fs::create_dir_all(file.parent().unwrap_or(&file));
        let _ = std::fs::write(&file, if rejected { "rejected" } else { "data" });
        if rejected { CommitCheck::Rejected } else { CommitCheck::DataOnly }
    }

    /// The base commit of a change set, and if that commit has a Semilla check (design 25.2;
    /// trial 4, cheat C3: a commit with plain `git` before an honest change set). `commit` is
    /// the commit of the change set, or `None` for the session. The answer comes from git at the
    /// time of the call, not from a value in a note. A line that starts with `!` is a warning.
    /// A commit has a check only if the result that this machine computes for it is accepted
    /// (batch 5y, A2).
    pub fn base_notice(&mut self, set: &ChangeSet, commit: Option<&str>) -> Option<String> {
        let base = match commit {
            Some(commit) => self.repo.parent(commit).ok()??,
            None if set.steps.is_empty() => self.repo.git(&["rev-parse", "HEAD"]).ok()?,
            None => self.base_commit(&set.base_tree)?,
        };
        let short = |commit: &str| commit[..7.min(commit.len())].to_string();
        let noted: BTreeSet<String> = self.repo.git(&["notes", "--ref=semilla", "list"]).unwrap_or_default().lines().filter_map(|line| line.split_whitespace().nth(1).map(str::to_string)).collect();
        match self.commit_check(&base, &noted) {
            CommitCheck::Accepted => return Some(format!("base `{}`: it has a Semilla check", short(&base))),
            CommitCheck::Rejected => return Some(format!("! base `{0}`: its Semilla check is rejected. Run `semilla status {0}`", short(&base))),
            CommitCheck::DataOnly => return Some(format!("base `{0}`: it has Semilla data, but this machine did not compute the full result. Run `semilla status {0}`", short(&base))),
            CommitCheck::None => {}
        }
        // The commits from the base back to the last commit with a check.
        let log = self.repo.git(&["log", "--format=%H %s", "-n", "500", &base]).unwrap_or_default();
        let mut open: Vec<String> = Vec::new();
        for line in log.lines() {
            let (hash, subject) = line.split_once(' ').unwrap_or((line, ""));
            let state = self.commit_check(hash, &noted);
            if matches!(state, CommitCheck::Accepted | CommitCheck::DataOnly) {
                let more = if open.len() > 10 { format!(", and {} more", open.len() - 10) } else { String::new() };
                let (count, verb) = if open.len() == 1 { ("1 commit".to_string(), "has") } else { (format!("{} commits", open.len()), "have") };
                let last = match state {
                    CommitCheck::Accepted => format!("the last checked commit is `{}`", short(hash)),
                    _ => format!("the last commit with Semilla data is `{0}`; this machine did not compute its full result: run `semilla status {0}`", short(hash)),
                };
                return Some(format!("! {count} before this change set {verb} no Semilla check: {}{more} ({last})", open.iter().take(10).cloned().collect::<Vec<_>>().join(", ")));
            }
            // The subject is text of the agent: quote marks, and an escape for each hidden character.
            let mark = if state == CommitCheck::Rejected { " (its Semilla check is rejected)" } else { "" };
            open.push(format!("`{}` {}{mark}", short(hash), semilla_core::text::quoted(subject)));
        }
        Some(format!("base `{}`: no Semilla check (no checked commit before it)", short(&base)))
    }

    /// The line about the build code of the base (design 28.5). The base of a change set can
    /// have build code that a commit shortly before it added or changed. Each proof of the
    /// change set ran that code, and no mark shows it. The answer comes from git at the time of
    /// the call: Semilla compares the tree of the base with the tree of the commit
    /// [`crate::buildcode::BASE_WALK`] commits before it (first parents), and git names the
    /// commits that changed the files. No state is stored. The line is information only.
    pub fn base_build_code_notice(&mut self, set: &ChangeSet, commit: Option<&str>) -> Option<String> {
        if set.steps.is_empty() {
            return None;
        }
        let base = match commit {
            Some(commit) => self.repo.parent(commit).ok()??,
            None => self.base_commit(&set.base_tree)?,
        };
        let bound = crate::buildcode::BASE_WALK;
        let list = self.repo.git(&["rev-list", "--first-parent", "-n", &(bound + 1).to_string(), &base]).ok()?;
        let chain: Vec<&str> = list.lines().collect();
        // The first commit of the history is the start of the project, not a change.
        let far = chain.last().copied().filter(|far| *far != base)?.to_string();
        let stopped = chain.len() == bound + 1 && self.repo.parent(&far).ok().flatten().is_some();
        let (far_tree, base_tree) = (self.repo.commit_tree(&far).ok()?, self.repo.commit_tree(&base).ok()?);
        if far_tree == base_tree {
            return crate::buildcode::base_code_line(&[], stopped);
        }
        let key = self.tools_key();
        let parts = match self.tree_build_code(&far_tree, &base_tree, &key) {
            Ok(parts) => parts,
            Err(error) => {
                let cause = error.to_string();
                return Some(format!("base build code: Semilla cannot compare the base with the commit `{}` before it ({})", &far[..7.min(far.len())], cause.lines().next().unwrap_or_default()));
            }
        };
        if parts.is_empty() {
            return crate::buildcode::base_code_line(&[], stopped);
        }
        // Git names the commits that changed these files, from the newest commit back.
        let paths: Vec<&str> = parts.iter().map(|part| part.path.as_str()).collect();
        let range = format!("{far}..{base}");
        let mut args = vec!["log", "--first-parent", "--format=%x00%h", "--name-only", &range, "--"];
        args.extend(paths.iter().copied());
        let log = self.repo.git(&args).unwrap_or_default();
        let mut commits: Vec<(String, Vec<String>)> = Vec::new();
        for record in log.split('\0').filter(|record| !record.trim().is_empty()) {
            let mut lines = record.lines().map(str::trim).filter(|line| !line.is_empty());
            let Some(hash) = lines.next() else { continue };
            let files: Vec<String> = lines.filter(|line| paths.contains(line)).map(str::to_string).collect();
            if !files.is_empty() {
                commits.push((hash.to_string(), files));
            }
        }
        if commits.is_empty() {
            commits.push((format!("{}..{}", &far[..7.min(far.len())], &base[..7.min(base.len())]), paths.iter().map(|path| path.to_string()).collect()));
        }
        crate::buildcode::base_code_line(&commits, stopped)
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
            // The working tree against the tree of `HEAD`: a working tree with changed build code
            // uses the separate folders (design 28).
            let head = self.repo.commit_tree("HEAD").or_else(|_| self.repo.git(&["hash-object", "-t", "tree", "/dev/null"]).map(|empty| empty.trim().to_string()))?;
            let code = self.build_code(&ChangeSet { base_tree: head, head_tree: tree.clone(), ..Default::default() })?;
            let records = NormalOracle { repo: self.repo.clone(), code, mode: self.mode, log: Default::default(), added: Default::default() }.records(&tree, "n0")?;
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
            set.steps.push(Step { index, intent: open.intent, claims: open.claims, tree_before: open.tree_before, tree_after: tree.clone(), reasons: open.reasons });
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
        (expected != set.base_tree).then(|| format!("the change set starts at tree {}, but the commit before the work has tree {expected}; the changes between them have no claim{}", set.base_tree, self.squash_route(&set.base_tree)))
    }

    /// The commit (of the last 200 commits of `HEAD`) whose tree is the base tree of the change
    /// set, or `None`.
    pub fn base_commit(&self, base_tree: &str) -> Option<String> {
        let log = self.repo.git(&["log", "--format=%H %T", "-n", "200", "HEAD"]).ok()?;
        log.lines().filter_map(|line| line.split_once(' ')).find(|(_, tree)| *tree == base_tree).map(|(commit, _)| commit.to_string())
    }

    /// The line of `status` that says if `HEAD` has a Semilla check (brief 5h, C4), or an empty
    /// text when `HEAD` is the tagged commit of the session.
    fn head_check_line(&mut self, tagged: Option<&str>) -> String {
        let Ok(head) = self.repo.git(&["rev-parse", "HEAD"]) else { return String::new() };
        if tagged == Some(head.as_str()) {
            return String::new();
        }
        let short = &head[..head.len().min(7)];
        let noted: BTreeSet<String> = self.repo.git(&["notes", "--ref=semilla", "list"]).unwrap_or_default().lines().filter_map(|line| line.split_whitespace().nth(1).map(str::to_string)).collect();
        match self.commit_check(&head, &noted) {
            CommitCheck::Accepted => format!("HEAD `{short}` has a Semilla check: for its result, run `semilla status {short}`\n"),
            CommitCheck::Rejected => format!("! HEAD `{short}`: its Semilla check is rejected. Run `semilla status {short}`\n"),
            CommitCheck::DataOnly => format!("HEAD `{short}` has Semilla data, but this machine did not compute the full result. Run `semilla status {short}`\n"),
            CommitCheck::None => format!("HEAD `{short}` has no Semilla check\n"),
        }
    }

    /// The exact commands that fix a stored step with a rejected result (brief 5h, C3): see
    /// [`redo_route`]. `tag` is true for the rejection of `semilla tag`.
    pub(crate) fn fix_route(&self, set: &ChangeSet, report: &semilla_core::report::StepReport, tag: bool) -> String {
        // The route removes each step, and `HEAD` is not the base: the next first step would
        // start at `HEAD`.
        let moved = set.steps.first().is_some_and(|step| step.index == report.step) && self.repo.commit_tree("HEAD").is_ok_and(|head| head != set.base_tree);
        let reset = if moved { self.base_commit(&set.base_tree) } else { None };
        let then = match (tag, reset.is_some()) {
            (true, true) => "Then run `git commit`, and then `semilla tag`.",
            (true, false) => "Then run `semilla tag` again. If a file changed, run `git commit --amend --no-edit` first.",
            (false, true) => "Then run `git commit`.",
            (false, false) => "Then continue with the next step.",
        };
        redo_route(set, report, reset.as_deref(), then)
    }

    /// The exact commands that make the work one commit on the base of the change set (B4).
    fn squash_route(&self, base_tree: &str) -> String {
        match self.base_commit(base_tree) {
            Some(base) => format!(". The change set must be one commit on its base. Run: `git reset --soft {base}`, then `git commit -m \"<message>\"`, then `semilla tag`"),
            None => String::new(),
        }
    }

    /// Checks the change set. `only_step` limits the report to one step.
    pub fn check(&mut self, only_step: Option<usize>, require_groups: bool) -> Result<CheckResult> {
        let mut set = self.current_changeset()?;
        let session = self.load_session()?;
        let mut base_problem = if require_groups { self.base_problem(&set, session.commit.as_deref()) } else { None };
        if require_groups && session.replay.is_some() {
            base_problem = Some("a replay is in progress; end it with `semilla replay end` first".into());
        }
        // The build code state comes from the full change set: the base of the change set is the
        // trusted tree, not the tree before one step.
        let code = self.build_code(&set)?;
        if let Some(index) = only_step {
            let step = set.steps.get(index).cloned().with_context(|| format!("no step {index}"))?;
            set = ChangeSet { base_tree: step.tree_before.clone(), head_tree: step.tree_after.clone(), steps: vec![step], pair_refusals: set.pair_refusals.clone(), ..Default::default() };
        }
        let mut result = self.check_set_with(&set, require_groups, &code)?;
        if let Some(problem) = base_problem {
            add_problem(&mut result, problem);
        }
        Ok(result)
    }

    /// The check of `semilla verify` (batch 5y, A1; trial 8). The change set comes from the
    /// session file, and the agent controls that file. So the result is accepted only if the
    /// session agrees with git. A closed session must be the note of `HEAD`: then the check uses
    /// the change set of the note. An open session must start at the tree of a commit on the
    /// first-parent chain of `HEAD`, and its last step must end at the tree of `HEAD` or at the
    /// tree of the working files. The first value is the line that says what the result covers.
    pub fn verify(&mut self, require_groups: bool) -> Result<(String, CheckResult)> {
        let session = self.load_session()?;
        let short = |commit: &str| commit[..commit.len().min(7)].to_string();
        let count = |steps: usize| format!("{steps} step{}", if steps == 1 { "" } else { "s" });
        let head = self.repo.git(&["rev-parse", "HEAD"]).ok();
        if let (Some(tagged), None, None) = (&session.tagged, &session.open, &session.replay) {
            let problem = match self.repo.read_note(tagged).ok().and_then(|note| serde_json::from_str::<ChangeSet>(&note).ok()) {
                _ if head.as_deref() != Some(tagged.as_str()) => format!("the session is the closed change set of commit `{}`, but HEAD is `{}`: this result is not a result of HEAD. Run `semilla status HEAD`", short(tagged), head.as_deref().map(short).unwrap_or_default()),
                None => format!("the session names the tagged commit `{}`, but that commit has no Semilla note: this result is not a result of the commit", short(tagged)),
                Some(set) => match self.check_binding(tagged, &set) {
                    // The change set of the note belongs to `HEAD`: the result is its result.
                    Ok(()) => {
                        let base = self.repo.parent(tagged).ok().flatten().map(|base| format!("the base `{}`", short(&base))).unwrap_or_else(|| "no base commit".to_string());
                        let result = self.check_set(&set, require_groups)?;
                        return Ok((format!("verify: {base}, {}, the tagged commit `{}`", count(set.steps.len()), short(tagged)), result));
                    }
                    Err(error) => format!("the note of the tagged commit `{}` does not belong to it: {error}", short(tagged)),
                },
            };
            let mut result = self.check(None, require_groups)?;
            add_problem(&mut result, problem);
            return Ok((format!("verify: {}, the session of this folder (not a commit)", count(result.report.steps.len())), result));
        }
        let set = self.current_changeset()?;
        let mut result = self.check(None, require_groups)?;
        let chain = self.repo.git(&["log", "--first-parent", "--format=%H %T", "-n", "200", "HEAD"]).unwrap_or_default();
        let base = chain.lines().filter_map(|line| line.split_once(' ')).find(|(_, tree)| *tree == set.base_tree).map(|(commit, _)| commit.to_string());
        if !set.steps.is_empty() {
            let last = set.steps.last().map(|step| step.tree_after.clone()).unwrap_or_default();
            let (head_tree, work_tree) = (self.repo.commit_tree("HEAD").unwrap_or_default(), self.repo.write_tree()?);
            // A difference in formatting only is not an edit (the rule of `tag`).
            let mut same = |tree: &str| -> Result<bool> {
                if tree == last {
                    return Ok(true);
                }
                let (one, other) = (self.workspace(&last)?, self.workspace(tree)?);
                Ok(semilla_syntax::diff(&one, &other, &Default::default()).is_empty())
            };
            if base.is_none() {
                add_problem(&mut result, format!("the session does not agree with git: it starts at tree {}, but no commit on the first-parent chain of HEAD has that tree", short(&set.base_tree)));
            } else if !same(&head_tree)? && !same(&work_tree)? {
                add_problem(&mut result, format!("the session does not agree with git: its last step ends at tree {}, but HEAD has tree {} and the working files are different too. An edit has no step, or the session is not the session of this work", short(&last), short(&head_tree)));
            }
        }
        let base = base.map(|base| format!("the base `{}`", short(&base))).unwrap_or_else(|| "no base commit".to_string());
        Ok((format!("verify: {base}, {}, the session of this folder (not a commit)", count(set.steps.len())), result))
    }

    pub fn check_set(&mut self, set: &ChangeSet, require_groups: bool) -> Result<CheckResult> {
        let code = self.build_code(set)?;
        self.check_set_with(set, require_groups, &code)
    }

    /// Checks a change set, or one step of it, with the build code state of the full change set.
    fn check_set_with(&mut self, set: &ChangeSet, require_groups: bool, code: &BuildCode) -> Result<CheckResult> {
        // A command with `--no-build-code` does not check a step that uses a tree with changed
        // build code. Such a step gets its phase 1 facts, with no MIR data.
        let note = code.note(self.mode.runs_build_code());
        let skip: BTreeSet<usize> = note.as_ref().map(|note| note.not_checked()).unwrap_or_default().into_iter().collect();
        let log = crate::oracle::ConfigLog::default();
        let inner: Box<dyn semilla_verify::BodyOracle> = if self.no_bodies { Box::new(semilla_verify::NoOracle) } else { Box::new(NormalOracle { repo: self.repo.clone(), code: code.clone(), mode: self.mode, log: log.clone(), added: Default::default() }) };
        let bodies = Guard { inner: inner.as_ref(), skip: &skip, code };
        let exprs = SmtOracle::for_repo(&self.repo);
        let trees: BTreeSet<String> = set.steps.iter().flat_map(|s| [s.tree_before.clone(), s.tree_after.clone()]).collect();
        for tree in &trees {
            self.workspace(tree)?;
        }
        let cache = self.workspaces.clone();
        let mut load = |tree: &str| cache.get(tree).cloned().with_context(|| format!("no workspace for tree {tree}"));
        let started = std::time::Instant::now();
        let (mut report, mut outcomes) = semilla_verify::verify_changeset(set, &mut load, &bodies, &exprs, Options { require_groups, derive_edits: true });
        for step in report.steps.iter_mut().chain(outcomes.iter_mut().map(|outcome| &mut outcome.report)).filter(|step| skip.contains(&step.step)) {
            let change = note.as_ref().and_then(|note| note.changes.iter().find(|change| change.steps.contains(&step.step))).map(|change| change.step).unwrap_or(step.step);
            mark_not_checked(step, &format!("build code changed in step {change}"));
        }
        // I6: each proof of a step that used changed build code has the mark. The facts for a
        // build code file of the step go first in each output.
        for step in report.steps.iter_mut().chain(outcomes.iter_mut().map(|outcome| &mut outcome.report)) {
            let mark = note.as_ref().and_then(|note| note.mark_of(step.step));
            let paths = code.change_paths(step.step);
            for fact in &mut step.facts {
                fact.build_code = mark;
                fact.build_code_change = fact.file.as_ref().is_some_and(|file| paths.contains(file));
            }
            for claim in &mut step.claims {
                claim.build_code = mark;
            }
            // A build code change shows its diff. A fact of an item has no content: the first
            // such fact of a file gets the change of the file.
            let Some(trees) = set.steps.iter().find(|change| change.index == step.step).and_then(|change| cache.get(&change.tree_before).zip(cache.get(&change.tree_after))) else { continue };
            let mut seen: BTreeSet<String> = step.facts.iter().filter(|fact| fact.build_code_change && fact.content.is_some()).filter_map(|fact| fact.file.clone()).collect();
            for fact in step.facts.iter_mut().filter(|fact| fact.build_code_change && fact.content.is_none()) {
                let Some(file) = fact.file.clone().filter(|file| seen.insert(file.clone())) else { continue };
                let text = |ws: &Workspace| ws.other_files.get(&file).or_else(|| ws.rust_files.get(&file)).cloned();
                let size = |ws: &Workspace| ws.files.get(&file).map(|entry| entry.size);
                let (old, new) = (text(trees.0), text(trees.1));
                fact.content = Some(semilla_verify::files::content_change(old.as_deref(), new.as_deref(), size(trees.0), size(trees.1)));
            }
        }
        report.build_code = note;
        report.configs = config_note(set, &log, &cache);
        crate::timing("verify the change set (with the body checks)", started);
        // The parts of that time (batch 5x, part F).
        crate::oracle::READ_TIME.print("read the records of a cache folder");
        crate::oracle::MARK_TIME.print("give the records to the build configuration rules");
        crate::oracle::N0_TIME.print("compare the bodies at N0");
        crate::oracle::DEEP_TIME.print("compare the bodies (N2 to N4: not in use)");
        // The test rule of `tests` groups needs the test items of all trees.
        let test_items: BTreeSet<String> = cache.values().flat_map(|ws| ws.items.values().filter(|i| i.test).map(|i| i.path.clone())).collect();
        semilla_verify::organize::check_test_groups(set, &test_items, &mut report);
        // The reviewer's decisions on the pair warnings (from the review UI).
        let review = semilla_ui::review::ReviewFile::load(&self.review_file());
        for pair in &mut report.pairs {
            pair.reviewer = review.pair(&pair.key()).map(str::to_string);
            if pair.reviewer.as_deref() == Some("accepted") {
                // "Own step" is not a route when the step of the pair has no other change.
                report.organization.push(match &pair.stopped {
                    Some(cause) => format!("the reviewer accepted the pair warning `{}`: its step has no other change, and Semilla cannot derive that it is one item ({cause}). Change the code that stops the derivation, or ask the reviewer to dismiss the warning", pair.key()),
                    None => format!("the reviewer accepted the pair warning `{}`: make it one changed item: put the rename or the move in its own step with no other change, then change the body in the next step", pair.key()),
                });
            }
        }
        report.phase = if self.no_bodies { "syntax".into() } else { "full".into() };
        report.finish();
        // A build with a limit of the sandbox missing comes first: code of the change set could
        // change the result. A file of the records folder that is not a record comes next.
        for (index, warning) in crate::oracle::build_warnings().into_iter().enumerate() {
            report.warnings.insert(index, warning);
        }
        report.warnings.extend(self.tool_warnings());
        if !exprs.available() {
            report.warnings.push("Z3 is not available; condition effects are not computed (run scripts/fetch-z3.sh, or set SEMILLA_Z3)".into());
        }
        let text = format!("{}{}", semilla_verify::views::text_report(&report.steps), summary_line(&report));
        let (base, head) = (self.workspace(&set.base_tree).unwrap_or_default(), self.workspace(&set.head_tree).unwrap_or_default());
        let empty = semilla_syntax::Workspace::default();
        let step_trees: Vec<(&semilla_syntax::Workspace, &semilla_syntax::Workspace)> = set.steps.iter().map(|s| (cache.get(&s.tree_before).unwrap_or(&empty), cache.get(&s.tree_after).unwrap_or(&empty))).collect();
        let mut tree = semilla_verify::views::change_tree_with(&outcomes, &head, &base, &step_trees);
        if let Some(note) = &report.build_code {
            // The tree of one step has no step numbers on its nodes.
            let lone = match set.steps.as_slice() {
                [step] => Some(step.index),
                _ => None,
            };
            mark_tree(&mut tree, note, lone);
        }
        Ok(CheckResult { report, outcomes, text, tree })
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
        // After a rejected step, the fix is the step with its missing parts (batch 5x, part C).
        let step_fix = session.step_fix();
        let rejected = session.rejected.is_some();
        let mut set = session.changeset;
        if set.steps.is_empty() {
            if rejected {
                bail!("the change set has no step. {step_fix}, then `git commit --amend --no-edit`, then `semilla tag`.");
            }
            bail!("the change set has no step; edit the code and run `semilla step \"<intent>\"` first");
        }
        let commit = self.repo.git(&["rev-parse", commit])?;
        let tree = self.repo.commit_tree(&commit)?;
        let parent_tree = self.repo.commit_tree(&format!("{commit}^")).ok();
        // B1: the change set of the session is closed (a tag of X), and `commit` is a different
        // commit that the closed set does not describe. The commit has no step. The advice keeps
        // the tagged commit: it never resets to the base of the closed set.
        if let Some(tagged) = session.tagged.as_ref().filter(|tagged| **tagged != commit) {
            let fits = parent_tree.as_ref() == Some(&set.base_tree) && {
                let (last, head) = (self.workspace(&set.head_tree)?, self.workspace(&tree)?);
                set.head_tree == tree || semilla_syntax::diff(&last, &head, &Default::default()).is_empty()
            };
            if !fits {
                let on_tagged = self.repo.git(&["merge-base", "--is-ancestor", tagged, &commit]).is_ok() || parent_tree.as_ref() == Some(&set.base_tree);
                let base = if on_tagged { tagged.clone() } else { self.repo.git(&["rev-parse", &format!("{commit}^")]).unwrap_or_else(|_| format!("{commit}^")) };
                bail!("commit {commit} has no step. Run `git reset --soft {base}`, then `semilla step \"<intent>\"`, then `git commit`, then `semilla tag`.");
            }
        }
        // The change set must start at the parent commit, so that every change of the commit is
        // inside a step.
        if let Some(parent) = parent_tree {
            if parent != set.base_tree {
                // B4: a commit between the steps. The fix is one commit on the base.
                return Err(anyhow::anyhow!("the change set starts at tree {}, but the parent of {commit} has tree {parent}; the changes between them have no claim{}", set.base_tree, self.squash_route(&set.base_tree)));
            }
        }
        if set.head_tree != tree {
            // A difference in formatting only is allowed (for example `cargo fmt` before the
            // commit). A changed comment is not formatting.
            let (last, head) = (self.workspace(&set.head_tree)?, self.workspace(&tree)?);
            if !semilla_syntax::diff(&last, &head, &Default::default()).is_empty() {
                // 5y, B2 (trial 8): `git commit -am` does not add a new file. The working files
                // have the changes of the last step, and the commit does not.
                let work = self.repo.write_tree()?;
                if self.repo.commit_tree("HEAD").ok().as_deref() == Some(tree.as_str()) && work != tree && (work == set.head_tree || semilla_syntax::diff(&last, &self.workspace(&work)?, &Default::default()).is_empty()) {
                    bail!("{}", self.missing_in_commit(&tree, &set.head_tree));
                }
                // B1: the commands for an edit after the last step.
                bail!("the last step ends at tree {}, but commit {commit} has tree {tree}: an edit after the last step has no step. {step_fix}, then `git commit --amend --no-edit`, then `semilla tag`.", set.head_tree);
            }
            set.head_tree = tree.clone();
        }
        if session.replay.is_some() {
            bail!("a replay is in progress; end it with `semilla replay end` first");
        }
        // `tag` stores only a change set that `verify --final` accepts.
        let verdict = self.check(None, true)?;
        let mut line = verdict.text.lines().find(|line| line.starts_with("result:")).unwrap_or("result: rejected").to_string();
        if !verdict.report.accepted {
            // B1: a rejected final check has "result: rejected" as its first line.
            if !line.starts_with("result: rejected") {
                let rest = line.split_once(" · ").map(|(_, rest)| format!(" · {rest}")).unwrap_or_default();
                line = format!("result: rejected{rest}");
            }
            // Each reason with its step (design 24.3), then the exact commands for the first
            // step with a problem (brief 5h, C3).
            let mut reasons = Vec::new();
            for step in &verdict.report.steps {
                reasons.extend(step_problems(step).into_iter().map(|problem| format!("step {}: {problem}", step.step)));
            }
            reasons.extend(verdict.report.organization.iter().cloned());
            let route = verdict.report.steps.iter().find(|step| !step.accepted()).map(|step| format!("\n{}", self.fix_route(&set, step, true))).unwrap_or_default();
            let list = reasons.iter().map(|r| format!("\n  ✗ {r}")).collect::<String>();
            return Err(anyhow::Error::new(Rejected(format!("{line}\n`tag` stores nothing: the final check rejects the change set{list}{route}"))));
        }
        self.repo.write_note(&commit, &serde_json::to_string_pretty(&set)?)?;
        // B1 of batch 5j: the session jobs are done. Their result moves to the tagged commit.
        crate::background::remove_jobs(self, "session");
        let mut session = self.load_session()?;
        session.tagged = Some(commit.clone());
        self.save_session(&session)?;
        // The step refs of an older tag of the same commit do not stay.
        for old in self.repo.git(&["for-each-ref", "--format=%(refname)", &format!("refs/semilla/{commit}/")]).unwrap_or_default().lines() {
            let _ = self.repo.git(&["update-ref", "-d", old]);
        }
        self.repo.keep_tree(&format!("refs/semilla/{commit}/base"), &set.base_tree, "semilla base")?;
        for step in &set.steps {
            self.repo.keep_tree(&format!("refs/semilla/{commit}/step-{}", step.index), &step.tree_after, &format!("semilla step {}", step.index))?;
        }
        // The final check above ran phase 2 already (design 24.3): store it as the report of the
        // commit, so that `status` and `review` need no new run on this machine.
        let hash = crate::background::changeset_hash(&set);
        if verdict.report.phase == "full" {
            let stored = crate::background::StoredReport { key: commit.clone(), tree: tree.clone(), hash, tools: self.tools_key(), report: verdict.report.clone(), text: verdict.text.clone() };
            crate::background::store(self, &stored)?;
        } else {
            let job = crate::background::Job { key: commit.clone(), hash, changeset: None, mode: self.mode };
            crate::background::enqueue(self, &job)?;
            crate::background::spawn_worker(self)?;
        }
        let notice: String = self.base_notice(&set, Some(&commit)).into_iter().chain(self.base_build_code_notice(&set, Some(&commit))).map(|notice| format!("\n{notice}")).collect();
        Ok(format!("{line}\ntagged {commit} with {} steps; the change set is closed{notice}", set.steps.len()))
    }

    /// Loads the change set of a tagged commit into the session (for a review).
    pub fn load(&mut self, commit: &str) -> Result<String> {
        let (commit, set) = self.commit_changeset(commit)?;
        let steps = set.steps.len();
        self.save_session(&Session { changeset: set.clone(), tagged: Some(commit.clone()), ..Default::default() })?;
        // Batch 5y (A3, C4): `load` is a reviewer output. It starts with the build code line
        // (design 28.4), and it prints the result that this machine has for the commit: the
        // stored result, or phase 1 now. It takes no result from the note (rule T6).
        let stored = crate::background::stored_for(self, &commit, &crate::background::changeset_hash(&set));
        let full = stored.is_some();
        let report = match stored {
            Some(stored) => stored.report,
            None => {
                let saved = self.no_bodies;
                self.no_bodies = true;
                let phase1 = self.check_set(&set, true);
                self.no_bodies = saved;
                phase1?.report
            }
        };
        let mut text = String::new();
        if let Some(line) = self.build_code_notice(&report) {
            put_line(&mut text, &line);
        }
        put_line(&mut text, &format!("loaded {steps} steps from {commit}"));
        let result = summary_line(&report);
        if full || result_fails(&result, false) {
            text.push_str(result.lines().next().unwrap_or_default());
        } else {
            text.push_str(&format!("phase 1 ok; this machine has no phase 2 result for the commit. For the result, run `semilla status {}`", &commit[..commit.len().min(7)]));
        }
        Ok(text)
    }

    /// Replaces the guessed external calls of the graph with the calls from the MIR, when the N0
    /// records of the tree are in the cache (design 10.2: full paths and signature badges).
    fn add_external_calls(&self, graph: &mut WorkspaceGraph, workspace: &Workspace, tree: &str, code: &BuildCode) {
        use semilla_core::view::{GraphEdge, GraphEdgeKind, GraphNode, GraphNodeKind, Status};
        // The folder set of the tree. A command with `--no-build-code` reads no record of a
        // tree with changed build code.
        let Ok((root, marked)) = (NormalOracle { repo: self.repo.clone(), code: code.clone(), mode: self.mode, log: Default::default(), added: Default::default() }).root(tree) else { return };
        let settings = semilla_normal::cargo::settings_key(&root.join("build"), semilla_normal::cargo::find_driver().ok().as_deref());
        let cache = root.join("mir").join(tree).join(format!("n0-s{settings}"));
        if !cache.join(".complete").exists() || (!marked && cache.join(crate::oracle::BUILD_CODE_FILE).exists()) {
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
        self.ui_data_for(&set, None)
    }

    /// Builds the data of the review UI for a change set. The body checks come from the cache of
    /// the background worker when it has run.
    pub fn ui_data_for(&mut self, set: &ChangeSet, commit: Option<&str>) -> Result<semilla_ui::UiData> {
        let mut set = set.clone();
        let code = self.build_code(&set)?;
        let mut result = self.check_set_with(&set, false, &code)?;
        // The build code line is the first line of each output of a reviewer (design 28).
        result.report.notices.extend(self.build_code_notice(&result.report));
        result.report.notices.extend(self.base_notice(&set, commit));
        result.report.notices.extend(self.base_build_code_notice(&set, commit));
        // The checked configurations come next to the base line (batch 5s, B1).
        result.report.notices.extend(config_lines(&result.report));
        // Each group shows the engine suggestion of its subgroups. A stored layout has labels of
        // the agent (a role, a title), and no reviewer output shows a stored label (5k, A3).
        let test_items = self.test_items(&set)?;
        set.subgroups.clear();
        for group in set.groups.clone() {
            let suggestion = semilla_verify::organize::suggest(&set, &group.id, &result.report.steps, &test_items);
            set.subgroups.extend(suggestion);
        }
        let mut flows: Vec<FlowGraph> = Vec::new();
        let mut sources = BTreeMap::new();
        // The flow view of a function uses the head tree, so it shows the final names. The statuses
        // come from one diff from the base tree to the head tree (brief 5f, D3): a statement path
        // of an early step can name a different statement in the head tree.
        let head_ws = self.workspace(&set.head_tree).unwrap_or_default();
        let base_ws = self.workspace(&set.base_tree).unwrap_or_default();
        let finals = semilla_verify::views::final_edits(&result.outcomes, &base_ws, &head_ws);
        for (item, (statuses, removed)) in semilla_verify::views::final_flow_inputs(&finals) {
            if let Some(flow) = head_ws.items.get(&item).and_then(|it| semilla_syntax::flow::flow_view(it, &statuses, removed)) {
                flows.push(flow);
            }
        }
        for (key, (old, new)) in semilla_verify::views::final_sources(&finals, &base_ws, &head_ws) {
            sources.insert(key, semilla_ui::SourcePair { before: old, after: new });
        }
        // The source of the anchor of each fact, in the trees of its own step (D2). A statement
        // path such as `S1` can name a different statement in a different step.
        let mut fact_sources = BTreeMap::new();
        for (step, outcome) in set.steps.iter().zip(&result.outcomes) {
            let before = self.workspace(&step.tree_before)?;
            let after = self.workspace(&step.tree_after)?;
            let step_sources = semilla_verify::views::sources(std::slice::from_ref(outcome), &before, &after);
            for fact in result.report.steps.iter().filter(|report| report.step == step.index).flat_map(|report| &report.facts) {
                if let Some((old, new)) = fact.at.as_ref().and_then(|at| step_sources.get(&at.to_string())) {
                    fact_sources.insert(format!("{}:{}", step.index, fact.at.as_ref().unwrap()), semilla_ui::SourcePair { before: old.clone(), after: new.clone() });
                }
            }
            // An anchor that the final diff does not have keeps the source of its step.
            for (key, (old, new)) in step_sources {
                sources.entry(key).or_insert(semilla_ui::SourcePair { before: old, after: new });
            }
        }
        let head = self.workspace(&set.head_tree).unwrap_or_default();
        let mut graph: WorkspaceGraph = semilla_verify::views::graph(&result.outcomes, &head);
        self.add_external_calls(&mut graph, &head, &set.head_tree, &code);
        Ok(semilla_ui::UiData { changeset: set, report: result.report, tree: result.tree, flows, graph, sources, fact_sources })
    }
}

/// The short text of `status` (brief 5f, D2): one line for each step, then the result line.
/// A line that makes the result not accepted stays, because the agent must correct it.
/// `status --verbose` prints the facts.
/// The build configurations of the body checks of a change set (design 8.12; batch 5s, B1),
/// from the log of the oracle. `None` if no step has a body check.
fn config_note(set: &ChangeSet, log: &crate::oracle::ConfigLog, trees: &BTreeMap<String, Workspace>) -> Option<semilla_core::report::ConfigNote> {
    let log = log.lock().ok()?;
    let mut note = semilla_core::report::ConfigNote::default();
    let mut found = false;
    let mut conditions: BTreeSet<String> = BTreeSet::new();
    // A configuration is in the checked list only if each compared step has it.
    let mut order: Vec<String> = Vec::new();
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    let mut compared = 0;
    for step in &set.steps {
        let Some(configs) = log.get(&(step.tree_before.clone(), step.tree_after.clone())) else { continue };
        found = true;
        compared += 1;
        if note.target.is_empty() {
            note.target = configs.target.clone();
        }
        for name in &configs.checked {
            if !order.contains(name) {
                order.push(name.clone());
            }
            *count.entry(name.clone()).or_default() += 1;
            // A configuration that Semilla added for a proof of this step (batch 5u, part C).
            if configs.added.contains(name) {
                match note.added.iter_mut().find(|added| added.name == *name) {
                    Some(added) => added.steps.push(step.index),
                    None => note.added.push(semilla_core::report::ConfigAdded { name: name.clone(), steps: vec![step.index] }),
                }
            }
        }
        for (name, same_as) in &configs.same {
            match note.same.iter_mut().find(|same| same.name == *name && same.same_as == *same_as) {
                Some(same) => same.steps.push(step.index),
                None => note.same.push(semilla_core::report::ConfigSame { name: name.clone(), same_as: same_as.clone(), steps: vec![step.index] }),
            }
        }
        for (name, cause) in &configs.not_checked {
            // One entry for a name and a kind of cause: it keeps the error text of the first step.
            let new = semilla_core::report::ConfigGap { name: name.clone(), cause: cause.clone(), steps: vec![step.index] };
            match note.not_checked.iter_mut().find(|gap| gap.name == new.name && gap.kind() == new.kind()) {
                Some(gap) => gap.steps.push(step.index),
                None => note.not_checked.push(new),
            }
        }
        if let (Some(before), Some(after)) = (trees.get(&step.tree_before), trees.get(&step.tree_after)) {
            let cover = semilla_verify::cfgcond::Cover::new(&[(before, &configs.truth[0]), (after, &configs.truth[1])]);
            conditions.extend(cover.not_covered.iter().map(|text| semilla_verify::cfgcond::show(text)));
        }
    }
    // Batch 5y, C4.4. A configuration of the default list that only some steps have: the
    // output names it with its steps. Before, no output of the reviewer had it.
    for name in order.iter().filter(|name| count.get(*name) != Some(&compared) && !note.added.iter().any(|added| added.name == **name)) {
        let steps = set.steps.iter().filter(|step| log.get(&(step.tree_before.clone(), step.tree_after.clone())).is_some_and(|configs| configs.checked.contains(name))).map(|step| step.index).collect();
        note.in_steps.push(semilla_core::report::ConfigAdded { name: name.clone(), steps });
    }
    note.checked = order.into_iter().filter(|name| count.get(name) == Some(&compared)).collect();
    // An added configuration of each compared step needs no step list.
    for added in note.added.iter_mut().filter(|added| added.steps.len() == compared) {
        added.steps.clear();
    }
    note.not_covered = conditions.into_iter().collect();
    found.then_some(note)
}

/// The lines of the build configurations for an output (batch 5s, B1), if the report has them.
pub(crate) fn config_lines(report: &Report) -> Vec<String> {
    report.configs.as_ref().map(|note| note.lines(report.steps.len())).unwrap_or_default()
}

pub(crate) fn short_text(report: &Report) -> String {
    let mut text = String::new();
    let not_checked = report.build_code.as_ref().map(|note| note.not_checked()).unwrap_or_default();
    for step in &report.steps {
        let facts = step.facts.len();
        let detail = if facts == 0 {
            String::new()
        } else if not_checked.contains(&step.step) {
            // A step with no check has the facts of the syntax only: one fact for each edit.
            // A check can join the edits of a body in one fact, so this count can be larger
            // than the count of the checked step (batch 5w, D4). The text says what it counts.
            format!(" · {facts} fact{} from the syntax only, not checked", if facts == 1 { "" } else { "s" })
        } else {
            // Design 35.6: the count of each compare result. Design 28: the count of `code
            // equal` says how many compares used changed build code.
            let marked = step.facts.iter().filter(|fact| fact.compare == Compare::CodeEqual && fact.build_code.is_some()).count();
            format!(" · {}", semilla_core::report::FactCounts::of(step.facts.iter()).text(&report.marked_text(marked)))
        };
        let claims = match step.claims.len() {
            0 => String::new(),
            1 => " · 1 claim".to_string(),
            count => format!(" · {count} claims"),
        };
        put_line(&mut text, &format!("step {}  {}{detail}{claims}", step.step, semilla_core::text::quoted(&step.intent)));
        for claim in step.claims.iter().filter(|claim| !claim.status.is_verified() && !claim.status.is_pending() && !matches!(claim.status, ClaimStatus::NotChecked { .. })) {
            put_line(&mut text, &format!("  ✗ {} {}", claim.claim, claim.status.text()));
        }
        for fact in step.facts.iter().filter(|fact| fact.spotlight && fact.reason.is_none()) {
            put_line(&mut text, &format!("  ✗ {} has a spotlight with no reason: {}", fact.id, fact.description));
        }
        for uncovered in &step.uncovered {
            put_line(&mut text, &format!("  ✗ uncovered: {}", uncovered.description));
        }
        for error in &step.errors {
            text.push_str(&format!("  ✗ error: {}\n", semilla_core::text::block_indent(error, "      ")));
        }
    }
    text.push_str(&summary_line(report));
    if report.steps.iter().any(|step| !step.facts.is_empty()) {
        text.push_str("For the facts, run `semilla status --verbose`.\n");
    }
    text
}

/// A tree whose lock file does not agree with its manifests (the tree hash).
#[derive(Debug)]
struct StaleLockTree {
    tree: String,
}

impl std::fmt::Display for StaleLockTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`Cargo.lock` does not agree with the manifests in tree {}", self.tree)
    }
}

impl std::error::Error for StaleLockTree {}

/// The message for a tree whose lock file does not agree with its manifests. Semilla gives no
/// decision about the build code of such a tree, and it does not build the tree.
fn stale_lock_text(set: &ChangeSet, tree: &str) -> String {
    let place = tree_place(set, tree);
    format!(
        "`Cargo.lock` does not agree with the manifests in {place}. Cargo must change the lock file before it can build this tree, and Semilla does not write a file of a tree. Update the lock file: run `cargo update --workspace --offline`. Then run the Semilla command again; the lock file is a part of the step. Semilla made no check and no decision about build code."
    )
}

/// The name of a tree of a change set in a message.
fn tree_place(set: &ChangeSet, tree: &str) -> String {
    if tree == set.base_tree {
        return "the base tree of the change set".to_string();
    }
    match set.steps.iter().find(|step| step.tree_after == tree) {
        Some(step) => format!("the tree after step {}", step.index),
        None => format!("tree {}", &tree[..tree.len().min(7)]),
    }
}

/// The message for a tree whose dependency graph cargo cannot read (batch 5n, A6). This is not a
/// result of a check, and Semilla does not store it. The usual cause is a package that is not in
/// the cargo cache, so the message gives the command that gets it.
fn graph_failure_text(set: &ChangeSet, tree: &str, line: &str) -> String {
    format!(
        "Semilla has no result: cargo cannot read the dependency graph of {} ({line}). Semilla runs cargo with `--offline`. If a package is not in the cargo cache, run `cargo fetch` in the repository. If a manifest is wrong, correct it. Then run the command again.",
        tree_place(set, tree)
    )
}

/// The text of `status` with the problems that `semilla tag` rejects (batch 5k, D8): each
/// problem is a line before the result line, and the result word is `rejected`.
fn with_tag_problems(text: &str, problems: &[String]) -> String {
    let lines: String = problems.iter().map(|problem| format!("✗ `semilla tag` rejects this state: {problem}\n")).collect();
    let mut out = String::new();
    let mut done = false;
    for line in text.split_inclusive('\n') {
        if !done && line.starts_with("result:") {
            let rest = line.split_once(" · ").map(|(_, rest)| format!(" · {rest}")).unwrap_or_else(|| "\n".to_string());
            out.push_str(&lines);
            out.push_str(&format!("result: rejected{rest}"));
            done = true;
        } else {
            out.push_str(line);
        }
    }
    if !done {
        out.push_str(&lines);
        out.push_str("result: rejected\n");
    }
    out
}

/// The result line (brief 5f, D1): the verdict, the steps, the facts that the reviewer must read,
/// and the claims by result. It has no score. Uncovered changes show only if they exist.
pub(crate) fn summary_line(report: &Report) -> String {
    // The steps with no check (design 28). The counts of the line are the counts of the other
    // steps: a step with no check gives no result.
    let note = report.build_code.as_ref().filter(|note| !note.changes.is_empty());
    let skipped: BTreeSet<usize> = note.map(|note| note.not_checked()).unwrap_or_default().into_iter().collect();
    let checked: Vec<&semilla_core::report::StepReport> = report.steps.iter().filter(|step| !skipped.contains(&step.step)).collect();
    let uncovered: usize = report.steps.iter().map(|s| s.uncovered.len()).sum();
    let phase1_ok = report.phase == "syntax" && report.organization.is_empty() && !report.steps.iter().any(hard_problem);
    // A result that is not complete, with no other problem. A real problem gives `rejected`.
    let not_complete = report.not_complete() && report.organization.is_empty() && !report.steps.iter().any(|step| if skipped.contains(&step.step) || report.phase == "syntax" { hard_problem(step) } else { !step.accepted() });
    let verdict = if not_complete {
        "not complete".to_string()
    } else if report.accepted && report.phase == "syntax" {
        // Phase 2 (the MIR checks) did not run: phase 1 is not the final result.
        "phase 1 ok; phase 2 (the MIR checks) is necessary: run `semilla status --wait`".to_string()
    } else if report.accepted {
        "accepted".to_string()
    } else if phase1_ok && report.pending > 0 && !report.not_complete() {
        format!("phase 1 ok, {} check(s) pending", report.pending)
    } else if phase1_ok && !report.not_complete() {
        "phase 1 ok".to_string()
    } else {
        "rejected".to_string()
    };
    // Design 35.6: the count of each compare result. A step with no check has the result
    // `not checked` for each fact.
    let facts = report.facts_text();
    // The claims by result. A property claim that Semilla proves only in part is "partial".
    let mut results: Vec<(&str, usize)> = Vec::new();
    // I6: the count of the proofs says how many used changed build code.
    let mut marked: BTreeMap<&str, (usize, Vec<usize>)> = BTreeMap::new();
    for claim in checked.iter().flat_map(|s| &s.claims) {
        let name = match &claim.status {
            ClaimStatus::Verified { proof } if proof == "partial" => "partial",
            ClaimStatus::Verified { .. } => "verified",
            ClaimStatus::Refuted { .. } => "refuted",
            ClaimStatus::Empty => "empty",
            ClaimStatus::Malformed { .. } => "malformed",
            ClaimStatus::Pending { .. } => "pending",
            ClaimStatus::NotChecked { .. } => "not checked",
        };
        match results.iter_mut().find(|(known, _)| *known == name) {
            Some((_, count)) => *count += 1,
            None => results.push((name, 1)),
        }
        if let Some(step) = claim.build_code.filter(|_| claim.status.is_verified()) {
            let entry = marked.entry(name).or_default();
            entry.0 += 1;
            if !entry.1.contains(&step) {
                entry.1.push(step);
            }
        }
    }
    let order = ["verified", "partial", "pending", "refuted", "empty", "malformed", "not checked"];
    results.sort_by_key(|(name, _)| order.iter().position(|known| known == name));
    let total: usize = results.iter().map(|(_, count)| count).sum();
    let claims = match total {
        0 => "0 claims".to_string(),
        _ => format!("{total} claim{}: {}", if total == 1 { "" } else { "s" }, results.iter().map(|(name, count)| format!("{count} {name}{}", marked.get(name).map(|(count, steps)| semilla_core::report::count_mark(*count, steps)).unwrap_or_default())).collect::<Vec<_>>().join(", ")),
    };
    let steps = report.steps.len();
    let mut text = match note {
        // A4.3: the result of the checked steps, then the steps with no check and the cause.
        Some(note) if !note.ran => {
            let numbers: Vec<usize> = checked.iter().map(|step| step.step).collect();
            // The counts of the checked steps only. The line names the other steps.
            let facts = semilla_core::report::FactCounts::of(checked.iter().flat_map(|step| &step.facts)).text("");
            let done = if numbers.is_empty() { String::new() } else { format!(" · {}: {facts} · {claims}", semilla_core::report::step_ranges(&numbers)) };
            format!("result: {verdict}{done} · {}", note.not_checked_text())
        }
        // A4.2: the result ran the changed build code.
        Some(note) => format!("result: {verdict} · {steps} step{} · {facts} · {claims} · {}", if steps == 1 { "" } else { "s" }, note.tag_text()),
        None => format!("result: {verdict} · {steps} step{} · {facts} · {claims}", if steps == 1 { "" } else { "s" }),
    };
    if uncovered > 0 {
        text.push_str(&format!(" · {uncovered} uncovered (an internal error: a change with no fact)"));
    }
    text.push('\n');
    for error in &report.organization {
        put_line(&mut text, &format!("  ✗ {error}"));
    }
    for warning in &report.warnings {
        put_line(&mut text, &format!("  ! {warning}"));
    }
    for pair in &report.pairs {
        put_line(&mut text, &format!("  ! pair: {}", pair.text()));
        if pair.refusal.is_none() && pair.reviewer.is_none() {
            put_line(&mut text, &format!("      if the two items are really different, run `semilla pair refuse '{}' '{}' --reason \"…\"` (in a replay plan: add it to `refusals`)", pair.old, pair.new));
        }
    }
    if let Some(lines) = report.lines_text() {
        put_line(&mut text, &format!("  {lines}"));
    }
    text
}
