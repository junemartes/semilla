//! A tag after the commit (design 62): `semilla tag <rev>` and `semilla tag A..B` on commits
//! with no change set of the agent.
//!
//! Semilla makes one step for each commit: the tree before is the tree of the first parent (the
//! empty tree for a root commit), and the tree after is the tree of the commit. The check of the
//! step is the check of `semilla step` ([`App::check_step_of`]), and the build check is the
//! build check of `semilla step`. The trees come from the git objects: the command changes no
//! file of the working tree, not the index, not `HEAD`, not the branch, and not the session of
//! the agent. The only writes to the repository are the git notes.

use anyhow::{Context, Result, bail};
use semilla_core::text::put_line;
use semilla_core::{AfterCommit, ChangeSet, FailedConfig, Group, GroupKind, Step};

use crate::app::{App, OpenItems};

/// The options of a tag after the commit.
#[derive(Clone, Debug, Default)]
pub struct TagOptions {
    /// Also run the compiled check (as `semilla step --compiled`).
    pub compiled: bool,
    /// Do not run the build check (as `semilla step --no-check`).
    pub no_check: bool,
    /// Make the note of a commit again (rule R62.7-2).
    pub again: bool,
}

/// The tag of one commit after the commit.
struct CommitTag {
    /// The short hash of the commit, for the commands.
    short: String,
    /// The text of the commit: its head line, then the lines of the step.
    text: String,
    /// The count of the steps and of the facts that the command stored (0 for a commit that
    /// keeps its note).
    steps: usize,
    facts: usize,
    /// The facts part of the result line of one commit: `14 facts`, or the counts of the
    /// compare results with the compiled check.
    facts_line: String,
    open: OpenItems,
    /// The reason entries (`KEY=<why>`) for each open spotlight.
    reasons: Vec<String>,
}

impl App {
    /// `semilla tag [REV | A..B]`. Returns the text and true if no commit has an open item.
    ///
    /// The change set of the session is used when it belongs to the commit (rule R62.8-1): the
    /// session has stored steps that are not tagged and no commit is given, or the change set
    /// starts at the tree of the first parent of the commit, or the session is the tagged change
    /// set of the commit. An open step or a replay with no commit given keeps the behavior of
    /// today. Each other commit gets a tag after the commit.
    pub fn tag_command(&mut self, spec: Option<&str>, options: &TagOptions) -> Result<(String, bool)> {
        if let Some((from, to)) = spec.and_then(|spec| spec.split_once("..")) {
            return self.tag_range(from, to, options);
        }
        let rev = spec.unwrap_or("HEAD");
        if self.session_owns(spec)? {
            if options.compiled || options.no_check || options.again {
                bail!("the change set of the session has the steps of the agent for this commit; `--compiled`, `--no-check`, and `--again` are for a tag after the commit (each step keeps its own mode)");
            }
            return Ok((self.tag(rev)?, true));
        }
        let tag = self.tag_after(rev, options)?;
        let mut text = String::new();
        let open = tag.open.texts();
        if tag.steps == 0 {
            // The commit keeps its note, or it has no change.
            text.push_str(&tag.text);
            return Ok((text, true));
        }
        put_line(&mut text, &result_line(tag.steps, None, &tag.facts_line, &open));
        text.push_str(&tag.text);
        text.push_str(&reason_commands(std::slice::from_ref(&tag)));
        Ok((text, open.is_empty()))
    }

    /// True if the change set of the session belongs to the commit (rule R62.8-1).
    fn session_owns(&mut self, spec: Option<&str>) -> Result<bool> {
        let session = self.load_session()?;
        let open_set = !session.changeset.steps.is_empty() && session.tagged.is_none();
        if spec.is_none() && (open_set || session.open.is_some() || session.replay.is_some()) {
            return Ok(true);
        }
        let Ok(commit) = self.repo.git(&["rev-parse", "--verify", &format!("{}^{{commit}}", spec.unwrap_or("HEAD"))]) else { return Ok(false) };
        if session.tagged.as_deref() == Some(commit.as_str()) && !session.changeset.steps.is_empty() {
            return Ok(true);
        }
        let parent = self.repo.commit_tree(&format!("{commit}^")).ok();
        Ok(open_set && parent.as_deref() == Some(session.changeset.base_tree.as_str()))
    }

    /// `semilla tag A..B`: a tag after the commit for each commit of the first-parent chain from
    /// `A` (not included) to `B`, in order (rule R62.2-1).
    fn tag_range(&mut self, from: &str, to: &str, options: &TagOptions) -> Result<(String, bool)> {
        let (from, to) = (if from.is_empty() { "HEAD" } else { from }, if to.is_empty() { "HEAD" } else { to });
        let start = self.repo.git(&["rev-parse", "--verify", &format!("{from}^{{commit}}")]).with_context(|| format!("`{from}` is not a commit"))?;
        let end = self.repo.git(&["rev-parse", "--verify", &format!("{to}^{{commit}}")]).with_context(|| format!("`{to}` is not a commit"))?;
        // The first-parent chain from the end back to the start. The start must be on it.
        let mut chain = Vec::new();
        let mut at = end.clone();
        while at != start {
            chain.push(at.clone());
            match self.repo.parents(&at)?.into_iter().next() {
                Some(parent) => at = parent,
                None => bail!("`{from}` is not on the first-parent chain of `{to}`; a range `A..B` needs a commit `A` that is a first-parent ancestor of `B`"),
            }
        }
        chain.reverse();
        if chain.is_empty() {
            bail!("the range `{from}..{to}` has no commit");
        }
        let started = std::time::Instant::now();
        let mut tags = Vec::new();
        for commit in &chain {
            // R68.10-1: a tool error stops the tag of one commit, not the range. The commit gets
            // a note with the error, and the range continues with the next commit.
            match self.tag_after(commit, options) {
                Ok(tag) => tags.push(tag),
                Err(error) => tags.push(self.error_tag(commit, &format!("{error:#}"))),
            }
        }
        crate::timing("tag a range after the commit", started);
        let mut open = OpenItems::default();
        let (mut steps, mut facts) = (0, 0);
        for tag in &tags {
            open.add(&tag.open);
            steps += tag.steps;
            facts += tag.facts;
        }
        let texts = open.texts();
        let mut text = String::new();
        put_line(&mut text, &result_line(steps, Some(chain.len()), &facts_text(facts), &texts));
        for tag in &tags {
            text.push_str(&tag.text);
        }
        text.push_str(&reason_commands(&tags));
        Ok((text, texts.is_empty()))
    }

    /// R68.10-1: the tag of a commit of a range whose tag stops with a tool error. A commit with
    /// no note gets a note with the error and no step (`semilla tag REV --again` makes it
    /// again). A commit with a note keeps it: the error is only a line of the output. The error
    /// is an open item, so the exit code is 1.
    fn error_tag(&mut self, commit: &str, error: &str) -> CommitTag {
        let short = self.repo.git(&["rev-parse", "--short", commit]).unwrap_or_else(|_| commit[..commit.len().min(12)].to_string());
        let error = semilla_core::text::inline(error);
        let stored = if self.repo.read_note(commit).is_ok() {
            "the note of the commit stays".to_string()
        } else {
            let before = self.repo.parents(commit).ok().and_then(|parents| parents.first().cloned()).and_then(|parent| self.repo.commit_tree(&parent).ok()).or_else(|| self.repo.empty_tree().ok()).unwrap_or_default();
            let after = self.repo.commit_tree(commit).unwrap_or_default();
            let set = ChangeSet { base_tree: before, head_tree: after, after_commit: Some(AfterCommit { tool_error: Some(error.clone()), ..Default::default() }), ..Default::default() };
            match serde_json::to_string_pretty(&set).map_err(anyhow::Error::from).and_then(|text| self.repo.write_note(commit, &text)) {
                Ok(()) => format!("the note has the error; `semilla tag {short} --again` makes it again"),
                Err(note_error) => format!("Semilla cannot store the note: {}", semilla_core::text::inline(&format!("{note_error:#}"))),
            }
        };
        let text = format!("commit {short}: a tool error stops the tag of this commit; the range continues ({stored}): {error}\n");
        CommitTag { short, text, steps: 0, facts: 0, facts_line: String::new(), open: OpenItems { errors: 1, ..Default::default() }, reasons: Vec::new() }
    }

    /// The tag of one commit after the commit (rules R62.1-1 to R62.7-2).
    fn tag_after(&mut self, rev: &str, options: &TagOptions) -> Result<CommitTag> {
        let commit = self.repo.git(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")]).with_context(|| format!("`{rev}` is not a commit"))?;
        let short = self.repo.git(&["rev-parse", "--short", &commit]).unwrap_or_else(|_| commit[..commit.len().min(12)].to_string());
        let mut kept: Option<Step> = None;
        // Rule R62.7-1: a commit with a note keeps the note. `--again` makes it again.
        if let Ok(note) = self.repo.read_note(&commit) {
            let set = serde_json::from_str::<ChangeSet>(&note).ok();
            // R68.10-1: a kept note with a tool error stays an open item.
            let tool_error = set.as_ref().and_then(|set| set.after_commit.as_ref()).and_then(|after| after.tool_error.clone());
            let what = match &set {
                Some(_) if tool_error.is_some() => format!("a note with a tool error ({})", tool_error.as_deref().unwrap_or_default()),
                Some(set) if set.after_commit.is_some() => "a note of a tag after the commit".to_string(),
                Some(set) => format!("a note with {} step(s) of the agent", set.steps.len()),
                None => "a note that Semilla cannot read".to_string(),
            };
            match &set {
                _ if !options.again => {
                    let text = format!("commit {short} has {what}; the note stays. `semilla tag {short} --again` makes it again; `semilla status {short}` shows its result\n");
                    let open = OpenItems { errors: usize::from(tool_error.is_some()), ..Default::default() };
                    return Ok(CommitTag { short, text, steps: 0, facts: 0, facts_line: String::new(), open, reasons: Vec::new() });
                }
                Some(set) if set.after_commit.is_none() => {
                    bail!("commit {short} has {what}; `--again` makes only a note of a tag after the commit again, so the steps of the agent stay");
                }
                Some(set) => kept = set.steps.first().cloned(),
                None => {}
            }
        }
        let parents = self.repo.parents(&commit)?;
        let after = self.repo.commit_tree(&commit)?;
        let before = match parents.first() {
            Some(parent) => self.repo.commit_tree(parent)?,
            None => self.repo.empty_tree()?,
        };
        if before == after {
            let text = format!("commit {short} changes no file (its tree is the tree of its first parent): no note\n");
            return Ok(CommitTag { short, text, steps: 0, facts: 0, facts_line: String::new(), open: OpenItems::default(), reasons: Vec::new() });
        }
        // Design 28: the first parent of the commit is the start point of the reviewer.
        self.trusted_bases.borrow_mut().insert(before.clone());
        let subject = self.repo.subject(&commit).unwrap_or_default();
        let intent = if subject.trim().is_empty() { format!("commit {short}") } else { subject.trim().to_string() };
        let compiled = options.compiled || crate::app::compiled_env();
        // Rule R62.7-2: `--again` keeps the facts, the tags, and the reasons of the agent. The
        // check below shows each one that does not hold.
        let (agent, tags, reasons) = kept.map(|step| (step.agent, step.tags, step.reasons)).unwrap_or_default();
        let step = Step { index: 0, intent: intent.clone(), claims: Vec::new(), tree_before: before.clone(), tree_after: after.clone(), reasons, compiled, build_check: None, agent, tags };
        let after_commit = AfterCommit { merge: parents.len() > 1, ..Default::default() };
        let mut set = ChangeSet { base_tree: before.clone(), head_tree: after.clone(), steps: vec![step], after_commit: Some(after_commit), ..Default::default() };
        // The check of `semilla step` for the same two trees (rule R62.3-1).
        // R65.8-1: a step of the default mode runs the build check first, so the syntax phase
        // reads the expansion records of the two trees.
        let early_build = if compiled { None } else { Some(self.build_check(crate::buildcheck::skip_cause(options.no_check), &before, &after, true)) };
        if let Some(crate::buildcheck::Outcome::Stored { record, .. }) = &early_build {
            set.steps[0].build_check = Some(record.clone());
        }
        let saved = self.no_bodies;
        self.no_bodies = saved || !compiled;
        let mut result = self.check_step_of(&set, 0);
        self.no_bodies = saved;
        // Rule R62.5-3: an error that stops the compiled check does not stop the tag. The step
        // gets the facts of the syntax tree, and the error is an open item of the note.
        if let (true, Err(error)) = (compiled, &result) {
            let error = format!("{error:#}");
            set.steps[0].compiled = false;
            set.after_commit.as_mut().unwrap().compiled_error = Some(error);
            result = self.check_step_of(&set, 0);
        }
        let result = result?;
        let compiled = set.steps[0].compiled;
        let mut report = result.report;
        // The build check of `semilla step` (design 53). A failure is an open item (R62.5-2).
        let build = match early_build {
            Some(outcome) => outcome,
            None => self.build_check(crate::buildcheck::skip_cause(options.no_check), &before, &after, false),
        };
        let (record, build_text) = match build {
            crate::buildcheck::Outcome::Stored { record, lines } => (record, lines.iter().map(|line| format!("{line}\n")).collect::<String>()),
            crate::buildcheck::Outcome::Fails { text, record } => (record, text),
            crate::buildcheck::Outcome::CannotRun { text } => {
                let cause = text.lines().next().unwrap_or_default().trim().trim_start_matches("! build check: cannot run: ").to_string();
                (semilla_core::BuildCheck::CannotRun { cause }, text)
            }
        };
        set.steps[0].build_check = Some(record);
        // Rule R62.5-3: each configuration of the compiled check that does not build.
        if let Some(note) = &report.configs {
            let sides = [crate::oracle::BEFORE_SIDE, crate::oracle::AFTER_SIDE, crate::oracle::BOTH_SIDES];
            set.after_commit.as_mut().unwrap().failed_configs = note.not_checked.iter().filter(|gap| sides.iter().any(|side| gap.cause.starts_with(side))).map(|gap| FailedConfig { name: gap.name.clone(), cause: gap.cause.clone() }).collect();
        }
        let step_report = report.steps.first().cloned().unwrap_or_default();
        let kind = semilla_verify::organize::derived_kind(&[&step_report]).unwrap_or(GroupKind::Change);
        set.groups = vec![Group { id: "G1".into(), title: intent.clone(), kind, why: String::new(), steps: vec![0] }];
        crate::app::mark_after_commit(&mut report, &set);
        let open = OpenItems::of(&report, &set);
        // Rule R62.5-1: the commit is already there, so the note is always stored.
        self.repo.write_note(&commit, &serde_json::to_string_pretty(&set)?)?;
        // The compiled check ran here: store it as the report of the commit, as `tag` does, so
        // that `status` and `review` need no new run on this machine.
        if compiled && report.phase == "full" {
            let hash = crate::background::changeset_hash(&set);
            let full_text = format!("{}{}", semilla_verify::views::text_report(&report.steps), crate::app::summary_line(&report));
            let stored = crate::background::StoredReport { key: commit.clone(), tree: after.clone(), hash, tools: self.tools_key(), report: report.clone(), text: full_text };
            let _ = crate::background::store(self, &stored);
        }
        // The lines of the commit.
        let mut text = String::new();
        let from = match parents.first() {
            Some(parent) => format!("the first parent {}", self.repo.git(&["rev-parse", "--short", parent]).unwrap_or_else(|_| parent.clone())),
            None => "the empty tree (a root commit)".to_string(),
        };
        put_line(&mut text, &format!("tagged {commit} after the commit: one step from {from} to the commit {} · {}", semilla_core::text::quoted(&intent), line_counts(&report.facts_text(), &open.texts())));
        for line in crate::app::after_commit_lines(&set).into_iter().skip(1) {
            put_line(&mut text, &line);
        }
        text.push_str(&build_text);
        // A build with a limit of the sandbox missing, the build code, and the configurations:
        // the lines of `semilla step`.
        for warning in crate::oracle::build_warnings() {
            put_line(&mut text, &format!("  ! {warning}"));
        }
        if let Some(line) = report.build_code.as_ref().and_then(|note| note.step_line_with(0, semilla_normal::sandbox::missing().map(|missing| missing.state()).as_deref())) {
            put_line(&mut text, &semilla_verify::views::fit_line(&format!("  {line}")));
        }
        for line in crate::app::config_lines(&report) {
            put_line(&mut text, &semilla_verify::views::fit_line(&format!("  {line}")));
        }
        text.push_str(&with_commit_command(&step_body(&step_report), &short, 0));
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let reasons = crate::onestep::missing_reasons(&step_report);
        let facts = step_report.facts.len();
        Ok(CommitTag { short, text, steps: 1, facts, facts_line: report.facts_text(), open, reasons })
    }
}

/// The facts of a step in the text of `semilla step` (rule R62.3-1): the text report of the
/// step without its first line (the step line).
pub(crate) fn step_body(report: &semilla_core::report::StepReport) -> String {
    let body = semilla_verify::views::text_report_with(std::slice::from_ref(report), !report.accepted());
    body.lines().skip(1).map(|line| format!("{line}\n")).collect()
}

/// The gap list names the command of a stored step of the session. In the output of a commit,
/// the command names the commit: `semilla fact --commit SHORT`. One step of a note is the
/// last step, so the command needs no `--step`; a different step keeps `--step N`.
fn with_commit_command(text: &str, short: &str, index: usize) -> String {
    let step_command = format!("{}{index} ", semilla_core::report::GAP_COMMAND);
    let step_option = if index == 0 { String::new() } else { format!("--step {index} ") };
    let commit_command = semilla_core::report::GAP_COMMAND.replace("semilla fact --step ", &format!("semilla fact --commit {short} {step_option}"));
    text.replace(&step_command, &commit_command)
}

/// `14 facts`.
fn facts_text(count: usize) -> String {
    format!("{count} fact{}", if count == 1 { "" } else { "s" })
}

/// The counts and the open items: `1 step · 14 facts · 2 open spotlights`.
fn line_counts(facts: &str, open: &[String]) -> String {
    std::iter::once(facts.to_string()).chain(open.iter().cloned()).collect::<Vec<_>>().join(" · ")
}

/// The result line of a tag after the commit (rule R62.5-4).
fn result_line(steps: usize, commits: Option<usize>, facts: &str, open: &[String]) -> String {
    let commits = commits.map(|count| format!("{count} commit{} · ", if count == 1 { "" } else { "s" })).unwrap_or_default();
    format!("result: {} · {commits}{steps} step{} · {}", semilla_core::report::AFTER_COMMIT, if steps == 1 { "" } else { "s" }, line_counts(facts, open))
}

/// The last lines: the exact command that adds the reasons of each commit (rule R62.5-5).
fn reason_commands(tags: &[CommitTag]) -> String {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let mut text = String::new();
    let open: Vec<&CommitTag> = tags.iter().filter(|tag| !tag.reasons.is_empty()).collect();
    if open.is_empty() {
        return text;
    }
    put_line(&mut text, "to give the reason for each open spotlight, run:");
    for tag in open {
        let reasons: Vec<String> = tag.reasons.iter().map(|reason| format!("--reason {}", quote(reason))).collect();
        put_line(&mut text, &format!("  semilla fact --commit {} {}", tag.short, reasons.join(" ")));
    }
    text
}

/// Each possible split of each reason text (design 37.6): the check runs with each key, and the
/// longest key that a fact uses is the key of the text. Used by `semilla fact --commit` and by
/// `semilla fact --step` (R68.11-1).
pub(crate) fn reason_splits(reasons: &[String]) -> Result<Vec<Vec<(String, String)>>> {
    let mut splits: Vec<Vec<(String, String)>> = Vec::new();
    for text in reasons {
        let options: Vec<(String, String)> = text.match_indices('=').map(|(at, _)| (text[..at].trim().to_string(), text[at + 1..].trim().to_string())).filter(|(key, reason)| !key.is_empty() && !reason.is_empty()).collect();
        if options.is_empty() {
            bail!("`--reason {text}` is not KEY=TEXT with a text. The reviewer reads the reason: say why the change is correct");
        }
        splits.push(options);
    }
    Ok(splits)
}

/// The key of each reason text: the longest key of its splits that a fact of the step uses with
/// the text. Returns the new reasons of the step (from `base`) with the count of new or
/// changed reasons, or the texts that belong to no fact.
pub(crate) fn chosen_reasons(report: &semilla_core::report::StepReport, base: &std::collections::BTreeMap<String, String>, reasons: &[String], splits: &[Vec<(String, String)>]) -> std::result::Result<(std::collections::BTreeMap<String, String>, usize), Vec<String>> {
    let used = |key: &str, reason: &str| report.facts.iter().any(|fact| fact.reason.as_deref() == Some(reason) && crate::onestep::fact_keys(fact).iter().any(|found| found == key));
    let (mut chosen, mut count, mut unused) = (base.clone(), 0, Vec::new());
    for (text, options) in reasons.iter().zip(splits) {
        let mut fits: Vec<&(String, String)> = options.iter().filter(|(key, reason)| used(key, reason)).collect();
        fits.sort_by_key(|(key, _)| std::cmp::Reverse(key.len()));
        match fits.first() {
            Some((key, reason)) => count += usize::from(chosen.insert(key.clone(), reason.clone()).as_ref() != Some(reason)),
            None => unused.push(text.clone()),
        }
    }
    if unused.is_empty() { Ok((chosen, count)) } else { Err(unused) }
}

impl App {
    /// `semilla fact --commit REV`: adds facts, tags, and reasons of the agent to a step of the
    /// note of a commit (rule R62.6-1). The checks are the checks of `semilla fact`, and a reason
    /// must belong to a spotlight of the step. A false fact, a tag with no edit, or a reason
    /// that belongs to no fact stores nothing. No build check runs again: the trees of the step
    /// do not change. Returns the text and true if the note is stored.
    pub fn add_to_commit(&mut self, rev: &str, step: Option<usize>, same: &[String], change: &[String], tags: &[String], reasons: &[String]) -> Result<(String, bool)> {
        let (commit, mut set) = self.commit_changeset(rev)?;
        let short = self.repo.git(&["rev-parse", "--short", &commit]).unwrap_or_else(|_| commit.clone());
        let last = set.steps.len().checked_sub(1).with_context(|| format!("the note of commit {short} has no step"))?;
        let index = step.unwrap_or(last);
        if index > last {
            bail!("no step {index} in the note of commit {short}; the last step is step {last}");
        }
        let added = semilla_core::AgentFacts { same: same.to_vec(), change: change.to_vec() };
        if added.is_empty() && tags.is_empty() && reasons.is_empty() {
            bail!("give a reason, a fact, or a tag: `semilla fact --commit {short} --reason 'KEY=TEXT'`, `--same '…'`, `--change 'ITEM=TEXT'`, or `--tag 'PLACE=NAME'`");
        }
        if let Err(message) = semilla_verify::agent::check_form(&added) {
            bail!("{message}");
        }
        for tag in tags {
            if let Err(message) = semilla_core::report::parse_tag(tag) {
                bail!("`--tag '{tag}'`: {message}");
            }
        }
        let splits = reason_splits(reasons)?;
        let stored = &mut set.steps[index];
        let mut counts = [0usize; 3];
        for (list, texts) in [(&mut stored.agent.same, same), (&mut stored.agent.change, change)] {
            for text in texts {
                if !list.contains(text) {
                    list.push(text.clone());
                    counts[0] += 1;
                }
            }
        }
        for tag in tags {
            if !stored.tags.contains(tag) {
                stored.tags.push(tag.clone());
                counts[1] += 1;
            }
        }
        let base_reasons = stored.reasons.clone();
        for options in &splits {
            for (key, reason) in options {
                stored.reasons.entry(key.clone()).or_insert_with(|| reason.clone());
            }
        }
        let (intent, compiled) = (stored.intent.clone(), stored.compiled);
        let saved = self.no_bodies;
        self.no_bodies = saved || !compiled;
        let mut result = self.check_step_of(&set, index);
        if let Ok(found) = &result {
            // The key of each reason text: the longest key that a fact of the step uses.
            let report = found.report.steps.first().cloned().unwrap_or_default();
            let (chosen, unused) = match chosen_reasons(&report, &base_reasons, reasons, &splits) {
                Ok((chosen, count)) => {
                    counts[2] += count;
                    (chosen, Vec::new())
                }
                Err(unused) => (base_reasons.clone(), unused),
            };
            if !unused.is_empty() {
                self.no_bodies = saved;
                let mut text = String::new();
                put_line(&mut text, &format!("rejected: nothing is added to the note of commit {short}"));
                for reason in &unused {
                    put_line(&mut text, &format!("  ✗ the reason `--reason '{reason}'` belongs to no fact of step {index}: its key is not the key of a spotlight"));
                }
                let open: Vec<String> = crate::onestep::missing_reasons(&report);
                if !open.is_empty() {
                    put_line(&mut text, &format!("the keys of the open spotlights: {}", open.iter().map(|entry| format!("`{}`", entry.trim_end_matches("=<why>"))).collect::<Vec<_>>().join(", ")));
                }
                return Ok((text, false));
            }
            // The step keeps only the key of each text. The check runs again with these keys.
            if chosen != set.steps[index].reasons {
                set.steps[index].reasons = chosen;
                result = self.check_step_of(&set, index);
            }
        }
        self.no_bodies = saved;
        let result = result?;
        let report = result.report.steps.first().cloned().unwrap_or_default();
        let mut lines = semilla_verify::views::agent_lines(&report, Some(semilla_core::report::GAP_LIMIT), &|_| Vec::new());
        lines.extend(semilla_verify::views::tag_lines(&report, &|_| Vec::new()));
        if let Some(error) = report.tags.iter().filter(|tag| tag.form).find_map(|tag| tag.error.clone()) {
            bail!("{error}");
        }
        let mut text = String::new();
        let false_facts: Vec<&semilla_core::AgentFact> = report.agent_facts.iter().filter(|fact| !fact.holds).collect();
        if !false_facts.is_empty() || report.tags.iter().any(|tag| tag.error.is_some()) {
            put_line(&mut text, &format!("rejected: nothing is added to the note of commit {short}"));
            for fact in false_facts {
                put_line(&mut text, &format!("  ✗ fact {} of the agent is false: `--{} '{}'`", fact.id, fact.kind, fact.options.join("', '")));
            }
            for line in &lines {
                put_line(&mut text, line);
            }
            put_line(&mut text, "correct each false fact and each tag, then run the command again. A missing fact is not a reject: the gap list names the items.");
            return Ok((text, false));
        }
        self.repo.write_note(&commit, &serde_json::to_string_pretty(&set)?)?;
        // The report of the compiled check is for the old note: store the new one.
        let mut full = result.report.clone();
        if set.after_commit.is_some() {
            crate::app::mark_after_commit(&mut full, &set);
        }
        if compiled && full.phase == "full" {
            let hash = crate::background::changeset_hash(&set);
            let full_text = format!("{}{}", semilla_verify::views::text_report(&full.steps), crate::app::summary_line(&full));
            let stored = crate::background::StoredReport { key: commit.clone(), tree: set.head_tree.clone(), hash, tools: self.tools_key(), report: full.clone(), text: full_text };
            let _ = crate::background::store(self, &stored);
        }
        put_line(&mut text, &format!("stored: {} new fact(s) of the agent, {} new tag(s), and {} new reason(s) in step {index} {} of the note of commit {short}; no build check runs again (the trees of the step are the same)", counts[0], counts[1], counts[2], semilla_core::text::quoted(&intent)));
        for line in &lines {
            put_line(&mut text, &with_commit_command(line, &short, index));
        }
        if set.after_commit.is_some() {
            let open = OpenItems::of(&full, &set).texts();
            put_line(&mut text, &format!("{}: {}", semilla_core::report::AFTER_COMMIT, if open.is_empty() { "no open item".to_string() } else { open.join(" · ") }));
            let missing = crate::onestep::missing_reasons(&report);
            if !missing.is_empty() {
                let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
                put_line(&mut text, &format!("to give the other reasons, run: semilla fact --commit {short} {}", missing.iter().map(|reason| format!("--reason {}", quote(reason))).collect::<Vec<_>>().join(" ")));
            }
        }
        put_line(&mut text, &format!("For each fact of the step, run `semilla status {short} --verbose`."));
        Ok((text, true))
    }
}
