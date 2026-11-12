//! The replay (design section 20): the agent first makes all its changes with its own tools.
//! Then it rebuilds the same changes as a series of small steps, and semilla checks each step as
//! usual.
//!
//! - `replay begin` saves the work as the target tree, and puts the base tree in the working
//!   tree. With `--commit`, the target is a commit and the base is its parent.
//! - In each step, the agent brings a part of the target back: `replay take` copies line hunks
//!   or whole items from the target, or the agent edits by hand (for example the old version of
//!   code that a later step changes).
//! - `replay status` lists the hunks that remain, with the semantic changes and the suggested ops.
//! - `replay end` requires that the last step ends at the target.
//!
//! The steps are ordinary steps. So the free edits stay at the expression level, and a refactor
//! of a function and a behavior change of the same function go in separate steps.

use semilla_core::text::put_line;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::app::{App, Session};
use crate::git::{Hunk, apply_hunks};

/// A replay in progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Replay {
    /// The tree that the steps must reach.
    pub target: String,
    /// The working tree at `replay begin`. `replay end` and `replay abort` restore it.
    pub original: String,
}

/// The most hunks and lines that `replay status` prints.
const MAX_HUNKS: usize = 40;
const MAX_LINES: usize = 8;

impl App {
    /// Starts a replay of the working tree changes, or of a commit.
    pub fn replay_begin(&mut self, commit: Option<&str>) -> Result<String> {
        let session = self.load_session()?;
        if session.replay.is_some() {
            bail!("a replay is already in progress; `semilla replay status` shows it");
        }
        if session.open.is_some() || (!session.changeset.steps.is_empty() && session.tagged.is_none()) {
            bail!("the session has steps that are not tagged; tag them or finish them before a replay");
        }
        let original = self.repo.write_tree()?;
        let (base, target, commit) = match commit {
            Some(commit) => {
                let commit = self.repo.git(&["rev-parse", commit])?;
                let base = self.repo.commit_tree(&format!("{commit}^")).with_context(|| format!("commit {commit} has no parent"))?;
                (base, self.repo.commit_tree(&commit)?, Some(commit))
            }
            None => (self.repo.commit_tree("HEAD").context("the repository has no commit")?, original.clone(), None),
        };
        if base == target {
            bail!("the target has no changes; make the changes first, then start the replay");
        }
        // Keep the trees reachable: the working tree is about to change.
        self.repo.keep_tree("refs/semilla/replay/target", &target, "semilla replay target")?;
        self.repo.keep_tree("refs/semilla/replay/original", &original, "semilla replay original")?;
        self.repo.switch_worktree(&original, &base)?;
        let mut session = Session { commit, replay: Some(Replay { target: target.clone(), original }), ..Default::default() };
        session.changeset.base_tree = base.clone();
        session.changeset.head_tree = base.clone();
        self.save_session(&session)?;
        let hunks = self.repo.hunks(&base, &target, None)?;
        let files = self.repo.changed_files(&base, &target)?.len();
        Ok(format!(
            "replay started: the working tree is now the base tree {base}; the target tree {target} has {} hunk(s) in {files} file(s).\n\
             Rebuild the target in steps: `semilla replay plan` writes a draft plan, and `semilla replay apply --full` runs the plan.\n\
             `semilla replay status` lists the remaining hunks.",
            hunks.len()
        ))
    }

    fn replay_of(session: &Session) -> Result<&Replay> {
        session.replay.as_ref().context("no replay is in progress; start one with `semilla replay begin`")
    }

    /// A short line with the remaining work of a replay, or `None` outside a replay.
    pub fn replay_remaining(&self, session: &Session) -> Result<Option<String>> {
        let Some(replay) = &session.replay else { return Ok(None) };
        let current = self.repo.write_tree()?;
        if current == replay.target {
            return Ok(Some("the working tree is the target; run `semilla replay end`".into()));
        }
        let hunks = self.repo.hunks(&current, &replay.target, None)?;
        Ok(Some(format!("{} hunk(s) remain to the target (see `semilla replay status`)", hunks.len())))
    }

    /// The remaining hunks, and the semantic changes between the working tree and the target.
    pub fn replay_status(&mut self) -> Result<String> {
        let session = self.load_session()?;
        let replay = Self::replay_of(&session)?.clone();
        let current = self.repo.write_tree()?;
        let steps = session.changeset.steps.len() + usize::from(session.open.is_some());
        if current == replay.target {
            return Ok(format!("replay: {steps} step(s); the working tree is the target. End the open step, then run `semilla replay end`."));
        }
        let hunks = self.repo.hunks(&current, &replay.target, None)?;
        let mut text = format!("replay: {steps} step(s); {} hunk(s) remain to the target\n", hunks.len());
        let mut file = "";
        for hunk in hunks.iter().take(MAX_HUNKS) {
            if hunk.file != file {
                file = &hunk.file;
                put_line(&mut text, &format!("{file}"));
            }
            let (lo, hi) = hunk.new_range();
            let place = if hunk.new_count == 0 { format!("delete after target line {lo}") } else { format!("target lines {lo}-{hi}") };
            put_line(&mut text, &format!("  take {file}:{lo}-{hi}   ({place}; working tree line {})", hunk.old_start));
            for line in hunk.lines.iter().take(MAX_LINES) {
                put_line(&mut text, &format!("      {}", semilla_syntax::tree::short(line, 100)));
            }
            if hunk.lines.len() > MAX_LINES {
                put_line(&mut text, &format!("      … {} more line(s)", hunk.lines.len() - MAX_LINES));
            }
        }
        if hunks.len() > MAX_HUNKS {
            put_line(&mut text, &format!("  … {} more hunk(s)", hunks.len() - MAX_HUNKS));
        }
        // The semantic view of the same difference: what each change is, and the op for it.
        let (now, target) = (self.workspace(&current)?, self.workspace(&replay.target)?);
        let edits = semilla_syntax::diff(&now, &target, &Default::default());
        if !edits.is_empty() {
            text.push_str("the remaining changes (the suggested op; the anchor in the working tree):\n");
            for edit in edits.iter().take(MAX_HUNKS) {
                let anchor = edit.before.as_ref().map(|a| a.to_string()).unwrap_or_else(|| "new code: anchor it after you take it".into());
                put_line(&mut text, &format!("  {:<28} {}  [{}]", semilla_verify::hints::suggest(edit), semilla_verify::hints::describe(edit), anchor));
            }
        }
        Ok(text)
    }

    /// Copies a part of the target into the working tree: a file (`FILE`), the hunks at target
    /// lines (`FILE:LINE` or `FILE:START-END`), or the hunks of an item (`crate::module::item`).
    pub fn replay_take(&mut self, spec: &str) -> Result<String> {
        let session = self.load_session()?;
        let replay = Self::replay_of(&session)?.clone();
        if session.open.is_none() {
            bail!("no step is open. `semilla replay take` is for a replay by hand, with the hidden command `old-step`. Use the plan: `semilla replay plan`, then `semilla replay apply --full`");
        }
        let current = self.repo.write_tree()?;
        let target_dir = self.repo.materialize(&replay.target)?;
        let root = self.repo.root.clone();
        let take_file = |file: &str| -> Result<String> {
            let source = target_dir.join(file);
            let dest = root.join(file);
            if source.exists() {
                std::fs::create_dir_all(dest.parent().unwrap())?;
                std::fs::copy(&source, &dest)?;
                Ok(format!("took the file {file} from the target"))
            } else if dest.exists() {
                std::fs::remove_file(&dest)?;
                Ok(format!("removed the file {file}: the target does not have it"))
            } else {
                bail!("no file {file} in the working tree or in the target")
            }
        };
        let is_item = spec.contains("::") && !spec.contains('/');
        let (file, select): (String, Box<dyn Fn(&Hunk) -> bool>) = if is_item {
            let target = self.workspace(&replay.target)?;
            let now = self.workspace(&current)?;
            match (target.items.get(spec), now.items.get(spec)) {
                (Some(item), _) => {
                    let (start, end) = (item.line_start, item.line_end);
                    (item.file.clone(), Box::new(move |h: &Hunk| overlaps(h.new_range(), (start, end))))
                }
                (None, Some(item)) => {
                    // The target does not have the item: take the hunks that remove it.
                    let (start, end) = (item.line_start, item.line_end);
                    (item.file.clone(), Box::new(move |h: &Hunk| overlaps(h.old_range(), (start, end))))
                }
                (None, None) => bail!("no item {spec} in the working tree or in the target"),
            }
        } else {
            match spec.rsplit_once(':') {
                Some((file, lines)) if lines.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
                    let (start, end) = match lines.split_once('-') {
                        Some((a, b)) => (a.parse::<usize>()?, b.parse::<usize>()?),
                        None => (lines.parse::<usize>()?, lines.parse::<usize>()?),
                    };
                    (file.to_string(), Box::new(move |h: &Hunk| overlaps(h.new_range(), (start, end))))
                }
                _ => return take_file(spec.trim_start_matches("./")),
            }
        };
        let file = file.trim_start_matches("./").to_string();
        let hunks = self.repo.hunks(&current, &replay.target, Some(&file))?;
        let chosen: Vec<&Hunk> = hunks.iter().filter(|h| select(h)).collect();
        if chosen.is_empty() {
            bail!("no remaining hunk of {file} matches {spec}; `semilla replay status` lists the hunks");
        }
        if chosen.len() == hunks.len() && (!root.join(&file).exists() || !target_dir.join(&file).exists()) {
            return take_file(&file);
        }
        let old = std::fs::read_to_string(root.join(&file)).unwrap_or_default();
        let new = std::fs::read_to_string(target_dir.join(&file)).unwrap_or_default();
        std::fs::write(root.join(&file), apply_hunks(&old, &new, &chosen))?;
        let places: Vec<String> = chosen.iter().map(|h| format!("{}-{}", h.new_range().0, h.new_range().1)).collect();
        Ok(format!("took {} hunk(s) of {file} from the target (target lines {}); claim the change, then check the step", chosen.len(), places.join(", ")))
    }

    /// Ends the replay. The working tree must be the target (a difference in formatting only is
    /// allowed), and no step may be open.
    pub fn replay_end(&mut self) -> Result<String> {
        let mut session = self.load_session()?;
        let replay = Self::replay_of(&session)?.clone();
        if session.open.is_some() {
            bail!("a step is still open; end it first");
        }
        let current = self.repo.write_tree()?;
        if current != replay.target {
            let (now, target) = (self.workspace(&current)?, self.workspace(&replay.target)?);
            if !semilla_syntax::diff(&now, &target, &Default::default()).is_empty() {
                let remaining = self.repo.hunks(&current, &replay.target, None)?.len();
                bail!("the working tree is not the target yet: {remaining} hunk(s) remain; `semilla replay status` lists them");
            }
            // Formatting only: the last step ends at the target tree.
            if let Some(last) = session.changeset.steps.last_mut() {
                last.tree_after = replay.target.clone();
            }
            session.changeset.head_tree = replay.target.clone();
            self.repo.switch_worktree(&current, &replay.target)?;
        }
        self.repo.switch_worktree(&replay.target, &replay.original)?;
        session.replay = None;
        let steps = session.changeset.steps.len();
        let groups = if session.changeset.groups.is_empty() { "declare the groups, " } else { "" };
        let next = match &session.commit {
            Some(commit) => format!("{groups}run `semilla verify --final`, then `semilla tag {commit}`"),
            None => format!("{groups}run `semilla verify --final`, commit, then `semilla tag`"),
        };
        self.save_session(&session)?;
        let _ = self.repo.git(&["update-ref", "-d", "refs/semilla/replay/target"]);
        let _ = self.repo.git(&["update-ref", "-d", "refs/semilla/replay/original"]);
        Ok(format!("replay ended with {steps} step(s); {next}"))
    }

    /// Stops a replay, removes its steps, and restores the working tree of `replay begin`.
    pub fn replay_abort(&mut self) -> Result<String> {
        let session = self.load_session()?;
        let replay = Self::replay_of(&session)?.clone();
        let current = self.repo.write_tree()?;
        self.repo.switch_worktree(&current, &replay.original)?;
        self.save_session(&Session::default())?;
        Ok(format!("replay stopped; the working tree is tree {} again, and the session is empty", replay.original))
    }
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}
