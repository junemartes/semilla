//! One command for each step (design 24.3):
//! `semilla step "<intent>" [--group T] [--claim '…']… [--reason 'ANCHOR=TEXT']…`.
//!
//! "Before" is the head tree of the last step (or `HEAD` for the first step). "After" is the
//! working tree. The command derives the facts, checks the explicit claims, and stores the step
//! only when the result is accepted.

use semilla_core::text::put_line;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use semilla_core::{Anchor, Claim, GroupKind};

use crate::app::{App, OpenStep, Session};

/// The claims that stay (design 24.2, rule 5, and design 35.3). Each other op is retired as a
/// claim.
pub(crate) const CLAIMS: &[&str] = &["rewrite-pattern", "no-panic", "no-unsafe", "no-io", "calls-only", "tested-by"];

/// The message for an op that is retired as a claim, or `None`.
pub(crate) fn retired_claim(op: &str) -> Option<String> {
    // Design 35.3: Semilla derives each rename and each move. A claim cannot add to the fact.
    if matches!(op, "rename" | "move-item") {
        return Some(format!(
            "the claim `{op}` is gone: Semilla derives each rename and each move as a fact, with the compare result of the compiled code; remove the claim (the claims that stay: {})",
            CLAIMS.join(", ")
        ));
    }
    (!CLAIMS.contains(&op) && semilla_core::ops::op(op).is_some()).then(|| {
        format!(
            "the claim `{op}` is retired: Semilla derives this change as a fact; remove the claim (the claims that stay: {})",
            CLAIMS.join(", ")
        )
    })
}

/// The input of one `step` command.
#[derive(Clone, Debug, Default)]
pub(crate) struct StepRequest {
    pub intent: String,
    pub group: Option<String>,
    /// Each claim as `OP ANCHOR key=value …`.
    pub claims: Vec<String>,
    /// Each reason as `ANCHOR=TEXT`.
    pub reasons: Vec<String>,
    pub tree: bool,
    /// Phase 1 only; phase 2 runs in the background (B1).
    pub fast: bool,
}

/// One more place where a `--reason` text can split into a key and a reason (design 37.6).
struct ReasonSplit {
    /// The index of the text in the request.
    text: usize,
    key: String,
    reason: String,
}

/// The keys of a fact that a reason can have.
fn fact_keys(fact: &semilla_core::report::Fact) -> Vec<String> {
    let mut keys = vec![fact.reason_place(), fact.part.clone()];
    keys.extend(fact.at.as_ref().map(|at| at.item.clone()));
    keys.extend(fact.reason_key.clone());
    keys.extend(fact.reason_item.clone());
    keys
}

/// The split of each `--reason` text with two `=` or more (design 37.6). The rule: the key of
/// a text is the longest key `K` that is open in the step, where the text starts with `K=`. If
/// no such key is open, the key is the text before the first `=`.
///
/// A key is open if a fact with this key got the reason of the split, or if a claim has the key
/// as its anchor. The check ran with each possible key in the reason map.
///
/// Returns the split of each text whose key is not the text before the first `=`, and true if
/// the check must run again: a split that is not the split of its text gave a reason to a fact.
fn reason_splits(
    report: &semilla_core::report::StepReport,
    claim_anchors: &[String],
    first: &[(String, String, String)],
    others: &[ReasonSplit],
) -> (BTreeMap<usize, ReasonSplit>, bool) {
    let open = |key: &str, reason: &str| {
        claim_anchors.iter().any(|anchor| anchor == key)
            || report
                .facts
                .iter()
                .any(|fact| fact.reason.as_deref() == Some(reason) && fact_keys(fact).iter().any(|found| found == key))
    };
    let mut chosen: BTreeMap<usize, ReasonSplit> = BTreeMap::new();
    let mut again = false;
    for (index, (_, first_key, first_reason)) in first.iter().enumerate() {
        // The splits of this text, the longest key first.
        let mut splits: Vec<&ReasonSplit> = others.iter().filter(|split| split.text == index).collect();
        splits.sort_by_key(|split| std::cmp::Reverse(split.key.len()));
        let fits: Vec<&ReasonSplit> = splits.into_iter().filter(|split| open(&split.key, &split.reason)).collect();
        let Some(longest) = fits.first() else { continue };
        chosen.insert(
            index,
            ReasonSplit {
                text: index,
                key: longest.key.clone(),
                reason: longest.reason.clone(),
            },
        );
        // The first split and each shorter split are not the split of the text.
        again |= fits.len() > 1 || open(first_key, first_reason);
    }
    (chosen, again)
}

impl StepRequest {
    /// The command line that runs this request again, with extra reasons.
    pub(crate) fn command(&self, extra: &[String]) -> String {
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
        let mut parts = vec!["semilla step".to_string(), quote(&self.intent)];
        if let Some(group) = &self.group {
            parts.push(format!("--group {}", quote(group)));
        }
        for claim in &self.claims {
            parts.push(format!("--claim {}", quote(claim)));
        }
        for reason in self.reasons.iter().chain(extra) {
            parts.push(format!("--reason {}", quote(reason)));
        }
        if self.fast {
            parts.push("--fast".into());
        }
        parts.join(" ")
    }
}

/// The phase 2 state of each step of `--fast` that has no final result, one line for each
/// (brief 5h, C2). With the report of a rejected step, the exact commands follow.
fn fast_step_lines(session: &Session, route: Option<String>) -> String {
    let mut text = String::new();
    for (step, state) in &session.fast_steps {
        match state {
            Some(problems) => put_line(&mut text, &format!("step {step}: phase 2 rejected: {problems}")),
            None => put_line(&mut text, &format!("step {step}: phase 2 runs (the result: `semilla status --wait`)")),
        }
    }
    match route {
        Some(route) => {
            let _ = writeln!(text, "{route}");
        }
        None => {
            if let Some(first) = session.fast_steps.iter().find(|(_, state)| state.is_some()).map(|(step, _)| *step) {
                let count = session.changeset.steps.iter().filter(|step| step.index >= first).count();
                put_line(
                    &mut text,
                    &format!("to fix step {first}: run `semilla undo` {count} time(s) (the files stay), then store each removed step again"),
                );
            }
        }
    }
    text
}

/// The text of a `calls-only` claim with the complete list of `calls`, and the entries that the
/// list adds to the written claim.
fn with_complete_calls(written: &str, complete: &[String]) -> (String, Vec<String>) {
    let mut old: Vec<String> = Vec::new();
    let mut words = split_words(written);
    words.retain(|word| match word.strip_prefix("calls=") {
        Some(value) => {
            match serde_json::from_str::<Vec<String>>(value) {
                Ok(entries) => old.extend(entries),
                Err(_) => old.extend(value.split(',').map(|part| part.trim().to_string())),
            }
            false
        }
        None => true,
    });
    let old: Vec<&str> = old.iter().map(|entry| entry.strip_suffix("::").unwrap_or(entry)).collect();
    let added = complete.iter().filter(|entry| !old.contains(&entry.as_str())).cloned().collect();
    let list = serde_json::to_string(complete).unwrap_or_default();
    let words: Vec<String> = words
        .into_iter()
        .map(|word| {
            if word.contains(char::is_whitespace) {
                format!("\"{word}\"")
            } else {
                word
            }
        })
        .collect();
    (format!("{} calls={list}", words.join(" ")), added)
}

/// Splits a claim text into words. A word in double quotes can hold spaces. Inside a JSON list
/// or object (`calls=["core::","std::"]`), the quotes stay, so that the value parses as JSON.
///
/// The anchor of an impl item has spaces between angle brackets (`a::<T as fmt::Display>::fmt`).
/// Semilla prints this anchor with no quotes, so a claim accepts it with no quotes (batch 5w,
/// D2): a space between `<` and its `>` does not end the word. The `<` must be at the start of
/// a word, or after `::`, `=`, or a different `<`. If a `<` has no `>`, this rule does not apply.
fn split_words(text: &str) -> Vec<String> {
    split_words_with(text, true).unwrap_or_else(|| split_words_with(text, false).unwrap_or_default())
}

/// `None` if `angles` is set and an angle bracket stays open at the end of the text.
fn split_words_with(text: &str, angles: bool) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut depth = 0usize;
    let mut angle = 0usize;
    for c in text.chars() {
        match c {
            '[' | '{' if !quoted => {
                depth += 1;
                current.push(c);
            }
            ']' | '}' if !quoted => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            '"' => {
                quoted = !quoted;
                if depth > 0 {
                    current.push(c);
                }
            }
            '<' if angles && !quoted && (angle > 0 || current.is_empty() || current.ends_with("::") || current.ends_with('=')) => {
                angle += 1;
                current.push(c);
            }
            '>' if angle > 0 && !quoted && !current.ends_with('-') => {
                angle -= 1;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted && angle == 0 => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if angle > 0 {
        return None;
    }
    if !current.is_empty() {
        words.push(current);
    }
    Some(words)
}

/// A position `FILE.rs:LINE[:COLUMN]`, or `None`.
fn position(text: &str) -> Option<(String, usize, Option<usize>)> {
    let (head, last) = text.rsplit_once(':')?;
    let last: usize = last.parse().ok()?;
    if semilla_core::text::has_extension(head, "rs") {
        return Some((head.to_string(), last, None));
    }
    let (file, line) = head.rsplit_once(':')?;
    let line: usize = line.parse().ok()?;
    semilla_core::text::has_extension(file, "rs").then(|| (file.to_string(), line, Some(last)))
}

/// The state at the start of a `step` command.
struct StepStart {
    session: Session,
    /// The result of the background check for the stored steps, if it is complete.
    background: Option<crate::background::StoredReport>,
    /// The tagged commit of a closed change set.
    closed: Option<String>,
    /// The session before the command.
    previous: Session,
    /// The tree of the working files.
    tree: String,
    /// The tree before the step.
    before: String,
}

/// The `--reason` texts of a step.
struct StepReasons {
    /// The reason of each key.
    reasons: BTreeMap<String, String>,
    /// Each text with its key and its reason.
    keys: Vec<(String, String, String)>,
    /// The other places where a text can split into a key and a reason.
    other_splits: Vec<ReasonSplit>,
}

/// The state of one `step` command after the open step is saved.
struct StepRun<'r> {
    request: &'r StepRequest,
    index: usize,
    /// True when phase 2 runs in the command.
    full: bool,
    /// The highest id of a stored claim.
    highest: usize,
    claim_count: usize,
    /// The id and the text of each claim.
    claim_texts: Vec<(String, String)>,
    claim_anchors: Vec<String>,
    reason_keys: Vec<(String, String, String)>,
    other_splits: Vec<ReasonSplit>,
    closed: Option<String>,
    previous: Session,
    /// The session to restore when the step is not stored.
    saved: Session,
    background: Option<crate::background::StoredReport>,
    tree: String,
}

/// The parts of the text of a step result.
struct StepText {
    report: semilla_core::report::StepReport,
    /// The text report of the step, with no first line.
    body: String,
    /// The text so far.
    text: String,
    /// The lines of [`App::step_notes`].
    notes: String,
    unused: UnusedReasons,
}

/// The `--reason` texts that no fact and no claim uses.
struct UnusedReasons {
    /// Each text with its key and its reason.
    list: Vec<(String, String, String)>,
    /// The unused keys that are a module path or a crate name.
    wide_keys: BTreeMap<String, &'static str>,
}

impl UnusedReasons {
    /// Puts one line for each unused reason.
    fn put_lines(&self, text: &mut String, report: &semilla_core::report::StepReport) {
        for (_, key, _) in &self.list {
            put_line(text, &self.line(report, key));
        }
    }

    /// The line of an unused reason. A key that is the name of a body, not the anchor of its
    /// fact (batch 5l, B3): the message gives the key of the fact.
    fn line(&self, report: &semilla_core::report::StepReport, key: &String) -> String {
        let keys: Vec<String> = report
            .facts
            .iter()
            .filter(|fact| fact.spotlight_why.is_some() && fact.affected.contains(key))
            .map(|fact| {
                format!(
                    "`{}`",
                    fact.at.as_ref().map_or_else(|| fact.part.clone(), std::string::ToString::to_string)
                )
            })
            .collect();
        // Batch 5y, B3: a key that is the file of a fact, or the text of a fact. A message
        // prints these texts, so the message gives the keys that the next call accepts.
        let file_keys: Vec<String> = report
            .facts
            .iter()
            .filter(|fact| fact.spotlight && fact.reason.is_none())
            .filter(|fact| {
                fact.description == *key
                    || (fact.at.is_none()
                        && fact
                            .part
                            .strip_prefix(key.as_str())
                            .is_some_and(|rest| rest.starts_with(':') && rest.len() > 1))
            })
            .map(|fact| format!("`{}`", fact.reason_place()))
            .collect();
        let hint = if !keys.is_empty() {
            format!(
                ". `{key}` is a body of a fact with a spotlight: the key of the reason is {}",
                keys.join(", ")
            )
        } else if !file_keys.is_empty() {
            format!(
                ". `{key}` is not the key of a reason. Each fact has its own key: {}",
                file_keys.join(", ")
            )
        } else if let Some(kind) = key.strip_prefix(semilla_core::report::KIND_KEY) {
            // Design 37.19: a kind reason for a kind with no open spotlight in this step.
            if kind == semilla_core::report::BUILD_CODE_KIND {
                ". A kind reason does not cover build code: each changed file of build code needs its own reason".to_string()
            } else if semilla_core::report::SPOTLIGHT_KINDS.iter().any(|(name, _)| *name == kind) {
                format!(". No open spotlight of this step has the kind `{kind}`")
            } else {
                format!(". `{kind}` is not the name of a kind. The kinds: {}", semilla_core::report::kind_names())
            }
        } else if let Some(kind) = self.wide_keys.get(key) {
            // Batch 5x, part D: a module path and a crate name are not a key of a reason.
            format!(
                ". `{key}` is a {kind}, and a {kind} is not a valid key of a reason. A key is the path of an item (a function, a method, a constant, a static, a type, a trait), the exact key of a spotlight, or a file key (`path/to/file:`)"
            )
        } else {
            String::new()
        };
        format!("  ! the reason for `{key}` belongs to no fact of this step; Semilla does not use it{hint}")
    }
}

/// The `key=<why>` entries of the open spotlights of a rejected step, for the command to run
/// again.
fn missing_reasons(report: &semilla_core::report::StepReport) -> Vec<String> {
    let mut missing = Vec::new();
    // Design 37.19: a kind with four open spotlights or more gets one kind reason. A
    // kind with fewer gets the keys of its facts. Build code keeps one key for each file.
    let kind_of = |fact: &semilla_core::report::Fact| fact.kind().filter(|kind| *kind != semilla_core::report::BUILD_CODE_KIND);
    let open_kinds: Vec<&str> = report
        .facts
        .iter()
        .filter(|f| f.spotlight && f.reason.is_none())
        .filter_map(kind_of)
        .collect();
    for fact in report.facts.iter().filter(|f| f.spotlight && f.reason.is_none()) {
        if let Some(kind) =
            kind_of(fact).filter(|kind| open_kinds.iter().filter(|open| *open == kind).count() >= semilla_core::report::KIND_REASON_MIN)
        {
            let entry = format!("{}{kind}=<why>", semilla_core::report::KIND_KEY);
            if !missing.contains(&entry) {
                missing.push(entry);
            }
            continue;
        }
        let place = fact.at.as_ref().map_or_else(|| fact.part.clone(), std::string::ToString::to_string);
        // Part B of batch 5j: the body below shows the fact with "it needs a reason" and
        // the exact anchor. The top summary does not repeat the reason line of the fact.
        // Batch 5y, B3: two facts of one item have one key. The command gives it one time.
        // Batch 7b, part 5.5: the path of the item is the key. A reason with this key
        // covers each spotlight in the item, so the command gives it one time.
        let place = fact.reason_item.clone().unwrap_or(place);
        let entry = format!("{place}=<why>");
        if !missing.contains(&entry) {
            missing.push(entry);
        }
    }
    missing
}

/// Gives each earlier step of `--fast` its phase 2 state from the background check.
fn refresh_fast_steps(restore: &mut Session, background: Option<&crate::background::StoredReport>) {
    for earlier in restore.fast_steps.keys().copied().collect::<Vec<_>>() {
        match background.and_then(|stored| stored.report.steps.iter().find(|step| step.step == earlier)) {
            Some(step) if !step.accepted() => {
                restore.fast_steps.insert(earlier, Some(crate::app::step_problems(step).join("; ")));
            }
            Some(_) => {
                restore.fast_steps.remove(&earlier);
            }
            None => {}
        }
    }
}

/// The command to run a rejected step again, and the causes that block it.
struct Retry {
    /// The request with the missing reasons and without the false claims.
    corrected: StepRequest,
    /// The `key=<why>` entries of the open spotlights.
    missing: Vec<String>,
    /// The causes that only a code change can fix.
    blocked: Vec<String>,
    /// The `calls-only` claims that the command changes: the claim, and the new entries.
    widened: Vec<(String, Vec<String>)>,
}

impl Retry {
    /// Puts a line for each claim that is not verified, and corrects the request.
    fn add_claims(&mut self, text: &mut String, report: &semilla_core::report::StepReport, claim_texts: &[(String, String)]) {
        for claim in report.claims.iter().filter(|c| !c.status.is_verified()) {
            let written = claim_texts.iter().find(|(id, _)| *id == claim.claim).map(|(_, text)| text.clone());
            let label = written.as_ref().map_or_default(|w| format!(" (`{w}`)"));
            put_line(
                text,
                &format!(
                    "  ✗ claim {}{label} is {}{}",
                    claim.claim,
                    claim.status.text(),
                    claim.hint.as_ref().map_or_default(|h| format!("; {h}"))
                ),
            );
            match written {
                // A `calls-only` claim with missing entries: the command has the claim with
                // the complete list (design 26.3). This changes the claim, and the text says so.
                Some(written) if !claim.complete_calls.is_empty() => {
                    let (text, added) = with_complete_calls(&written, &claim.complete_calls);
                    for slot in self.corrected.claims.iter_mut().filter(|c| **c == written) {
                        slot.clone_from(&text);
                    }
                    self.widened.push((claim.claim.clone(), added));
                }
                Some(written) => self.corrected.claims.retain(|c| *c != written),
                None => self.blocked.push(format!("claim {}", claim.claim)),
            }
        }
    }

    /// Adds the errors and the uncovered changes of the step to the blocked causes.
    fn add_errors(&mut self, text: &mut String, report: &semilla_core::report::StepReport) {
        for error in &report.errors {
            // The body below shows each error one time. The top summary does not repeat it
            // (Part B item 4 of batch 5j).
            self.blocked.push(error.clone());
        }
        for uncovered in &report.uncovered {
            put_line(text, &format!("  ✗ internal: no fact for {}", uncovered.description));
            self.blocked.push(uncovered.description.clone());
        }
    }

    /// Puts the command to run again, or the line that says why no command can succeed. Returns
    /// the command.
    fn again(&self, text: &mut String, request: &StepRequest) -> Option<String> {
        if self.blocked.is_empty() {
            let what = match (self.corrected.claims.len() < request.claims.len(), !self.widened.is_empty()) {
                (true, true) => " (without the claim that failed, and with the complete list of `calls`)",
                (true, false) => " (without the claim that failed)",
                (false, true) => " (with the complete list of `calls`)",
                (false, false) => "",
            };
            for (claim, added) in &self.widened {
                let entries: Vec<String> = added.iter().map(|entry| format!("`{entry}`")).collect();
                let _ = writeln!(
                    text,
                    "the command below changes the claim {claim}: it adds {} to `calls`. Use it only if the function must make these calls. If not, change the code.",
                    entries.join(", ")
                );
            }
            put_line(text, &format!("run again{what}: {}", self.corrected.command(&self.missing)));
            return Some(self.corrected.command(&self.missing));
        }
        if self.blocked.iter().any(|error| error.contains(semilla_normal::cargo::MISSING_DEPENDENCY)) {
            text.push_str("a code change is not necessary: run `cargo fetch` in the repository, then run the step again\n");
        } else {
            text.push_str("no command can succeed without a code change: fix the cause, then run the step again\n");
        }
        None
    }

    /// The short cause of the rejection. `tag` and `status` say this cause, and not "run
    /// `semilla step`" (batch 5x, part C).
    fn cause(&self, report: &semilla_core::report::StepReport) -> String {
        let failed = report.claims.iter().filter(|c| !c.status.is_verified()).count();
        let mut causes = Vec::new();
        if !self.missing.is_empty() {
            causes.push(format!(
                "{} spotlight{} with no reason",
                self.missing.len(),
                if self.missing.len() == 1 { "" } else { "s" }
            ));
        }
        if failed > 0 {
            causes.push(format!("{failed} claim{} not verified", if failed == 1 { "" } else { "s" }));
        }
        if let Some(error) = self.blocked.iter().find(|error| !error.starts_with("claim ")) {
            causes.push(error.lines().next().unwrap_or("").chars().take(200).collect());
        }
        if causes.is_empty() {
            causes.push("the result of the step says the cause".to_string());
        }
        causes.join("; ")
    }
}

/// The phase 2 states of the earlier steps of `--fast`.
struct FastStates {
    session: Session,
    /// The first earlier step that phase 2 rejects.
    first_rejected: Option<semilla_core::report::StepReport>,
    /// The lines of the steps that phase 2 accepts now.
    final_lines: String,
}

impl App {
    /// Resolves an anchor: an item path, or a position `FILE:LINE[:COLUMN]` in the working tree.
    /// A position of a claim on existing code (a rename, a move) is in the "before" tree.
    fn resolve_anchor(&mut self, text: &str, before: bool) -> Result<String> {
        match position(text) {
            Some((file, line, column)) => self.anchor_up(&file, line, column, before, 0),
            None => Ok(text.to_string()),
        }
    }

    /// Runs one `step` command. Returns the text and true if the step is stored.
    pub(crate) fn step_once(&mut self, request: &StepRequest) -> Result<(String, bool)> {
        let StepStart {
            mut session,
            background,
            closed,
            previous,
            tree,
            before,
        } = self.step_start(request)?;

        // The claims and the reasons.
        // The claim ids count the stored claims of the change set (trial 4, D14): a rejected step
        // stores no claim, so its ids are free again. The first stored claim is `c1`.
        let highest = session
            .changeset
            .steps
            .iter()
            .flat_map(|s| &s.claims)
            .filter_map(|c| c.id.strip_prefix('c').and_then(|n| n.parse::<usize>().ok()))
            .max()
            .unwrap_or(0);
        let claims = request
            .claims
            .iter()
            .enumerate()
            .map(|(offset, text)| self.parse_claim(text, format!("c{}", highest + offset + 1)))
            .collect::<Result<Vec<Claim>>>()?;
        let claim_texts: Vec<(String, String)> = claims
            .iter()
            .zip(&request.claims)
            .map(|(claim, text)| (claim.id.clone(), text.clone()))
            .collect();
        let StepReasons { reasons, keys, other_splits } = self.step_reasons(request)?;

        // B1: phase 2 runs in the command, so the result is final: phase 2 can reject a step (an
        // indirect change with no known cause). With `--fast`, phase 2 runs now only when a
        // property claim or an identity candidate needs it.
        let full = !request.fast
            || claims
                .iter()
                .any(|c| semilla_core::ops::op(&c.op).is_some_and(|i| i.kind == semilla_core::OpKind::Property));
        let index = session.changeset.steps.len();
        let claim_count = claims.len();
        let claim_anchors: Vec<String> = claims
            .iter()
            .filter_map(|claim| claim.at.as_ref().map(std::string::ToString::to_string))
            .collect();
        session.open = Some(OpenStep {
            intent: request.intent.clone(),
            tree_before: before.clone(),
            claims,
            reasons,
        });
        // The session to restore when the step is not stored: the counter of the stored claims.
        let mut saved = session.clone();
        saved.last_claim = highest;
        session.last_claim = highest + claim_count;
        self.save_session(&session)?;
        let mut run = StepRun {
            request,
            index,
            full,
            highest,
            claim_count,
            claim_texts,
            claim_anchors,
            reason_keys: keys,
            other_splits,
            closed,
            previous,
            saved,
            background,
            tree,
        };
        let result = self.step_check(&mut run)?;
        let report = result.report.steps.first().cloned().unwrap_or_default();
        let accepted = report.accepted();
        // A rejected step prints the full text of each failed claim in its first lines. The claim
        // list then has only the status and a short cause (brief 5g, item 4).
        let body = semilla_verify::views::text_report_with(std::slice::from_ref(&report), !accepted);
        let body = body.lines().skip(1).collect::<Vec<_>>().join("\n");
        let mut text = String::new();
        if let Some(commit) = &run.closed {
            put_line(
                &mut text,
                &format!("the change set of {commit} is tagged and closed; a new change set starts at HEAD"),
            );
        }
        let unused = self.unused_reasons(&run, &report);
        let notes = self.step_notes(run.index, &result);
        let out = StepText {
            report,
            body,
            text,
            notes,
            unused,
        };
        if !accepted {
            return self.step_rejected(run, out);
        }
        self.step_accepted(&run, &result, out)
    }

    /// Checks that a `step` command can run, and finds the trees of the step.
    fn step_start(&mut self, request: &StepRequest) -> Result<StepStart> {
        if request.intent.trim().is_empty() {
            bail!("the intent has no text. The reviewer reads the intent: say what the step does, for example `semilla step \"rename the field\"`");
        }
        let mut session = self.load_session()?;
        // The record of a rejected step is for the last `semilla step` only.
        session.rejected = None;
        if session.open.is_some() {
            bail!(
                "a step of the old protocol is open (the hidden command `old-step`, for the replay); end it with `semilla old-step end`, or remove it with `semilla old-step drop`"
            );
        }
        if session.replay.is_some() {
            bail!(
                "a replay is in progress, and `semilla step` does not work in a replay; `semilla replay apply` runs the plan, and `semilla replay abort` stops the replay"
            );
        }
        // The result of the background check for the stored steps, if it is complete (D7).
        let background = crate::background::stored_for(self, "session", &crate::background::changeset_hash(&session.changeset));
        let mut closed = None;
        let previous = session.clone();
        if session.tagged.is_some() {
            closed = session.tagged.take();
            session = Session::default();
        }
        let tree = self.repo.write_tree()?;
        let before = if session.changeset.steps.is_empty() {
            let head = self
                .repo
                .commit_tree("HEAD")
                .context("the repository has no commit; commit the base first")?;
            session.changeset.base_tree.clone_from(&head);
            head
        } else {
            session.changeset.head_tree.clone()
        };
        if before == tree {
            bail!("the working tree has no change since the last step (tree {tree}); edit the code first");
        }
        Ok(StepStart {
            session,
            background,
            closed,
            previous,
            tree,
            before,
        })
    }

    /// Reads one `--claim` text into a claim with the id `id`.
    fn parse_claim(&mut self, text: &str, id: String) -> Result<Claim> {
        let words = split_words(text);
        let Some(op) = words.first() else { bail!("an empty `--claim`") };
        if let Some(message) = retired_claim(op).or_else(|| semilla_core::ops::retired(op)) {
            bail!("{message}");
        }
        let info = semilla_core::ops::op(op).with_context(|| format!("unknown claim `{op}`; the claims are: {}", CLAIMS.join(", ")))?;
        let mut at = None;
        let mut args = serde_json::Map::new();
        for word in &words[1..] {
            match word.split_once('=') {
                Some((key, value)) if !key.contains("::") && !key.contains(':') => {
                    if let Some(message) = semilla_core::ops::retired_arg(key) {
                        bail!("{message}");
                    }
                    let value = serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
                    args.insert(key.to_string(), value);
                }
                _ if at.is_none() => {
                    let identity = matches!(op.as_str(), "rename" | "move-item");
                    let resolved = self.resolve_anchor(word, identity)?;
                    at = Some(resolved.parse::<Anchor>().map_err(|e| anyhow::anyhow!("{e}"))?);
                }
                _ => bail!("`--claim {text}`: a claim has one anchor and `key=value` arguments"),
            }
        }
        if let Some(missing) = info.args.iter().find(|arg| !args.contains_key(**arg)) {
            bail!("`{op}` needs the argument `{missing}`; `semilla ops {op}` shows an example");
        }
        Ok(Claim {
            id,
            op: op.clone(),
            at,
            args,
        })
    }

    /// Reads the `--reason` texts of a step.
    fn step_reasons(&mut self, request: &StepRequest) -> Result<StepReasons> {
        let mut reasons = BTreeMap::new();
        // Each `--reason` text with its key, to find a reason that no fact uses.
        let mut reason_keys: Vec<(String, String, String)> = Vec::new();
        for text in &request.reasons {
            let (anchor, reason) = text.split_once('=').with_context(|| format!("`--reason {text}` is not ANCHOR=TEXT"))?;
            let anchor = self.resolve_anchor(anchor.trim(), false)?;
            let key = anchor.parse::<Anchor>().map(|a| a.to_string()).unwrap_or(anchor);
            if reason.trim().is_empty() {
                bail!("`--reason {text}`: the reason has no text. The reviewer reads the reason: say why the change is correct");
            }
            reason_keys.push((text.clone(), key.clone(), reason.trim().to_string()));
            reasons.insert(key, reason.trim().to_string());
        }
        // A key can have a `=` (a manifest key with a `cfg(…)` condition), and a reason can
        // have one too. So a text with two `=` or more has more than one possible key (design
        // 37.6). The check runs with each possible key. Then the facts tell which key is open,
        // and the longest open key is the key of the text (`reason_splits`).
        let first_keys: BTreeSet<String> = reasons.keys().cloned().collect();
        let mut other_splits: Vec<ReasonSplit> = Vec::new();
        for (index, text) in request.reasons.iter().enumerate() {
            for (at, _) in text.match_indices('=').skip(1) {
                let (key, reason) = (text[..at].trim(), text[at + 1..].trim());
                if reason.is_empty() || first_keys.contains(key) {
                    continue;
                }
                other_splits.push(ReasonSplit {
                    text: index,
                    key: key.to_string(),
                    reason: reason.to_string(),
                });
            }
        }
        for split in &other_splits {
            reasons.entry(split.key.clone()).or_insert_with(|| split.reason.clone());
        }
        Ok(StepReasons {
            reasons,
            keys: reason_keys,
            other_splits,
        })
    }

    /// Checks the open step. A check that stops with an error restores the session.
    fn step_check(&mut self, run: &mut StepRun) -> Result<crate::app::CheckResult> {
        let index = run.index;
        let no_bodies = self.no_bodies;
        self.no_bodies = no_bodies || !run.full;
        let mut result = self.check(Some(index), false);
        // A candidate pair for a derived identity: only phase 2 can derive it (design 24.2, rule 4).
        if !run.full
            && !no_bodies
            && result
                .as_ref()
                .is_ok_and(|r| r.report.steps.first().is_some_and(|s| s.identity_candidates))
        {
            run.full = true;
            self.no_bodies = false;
            result = self.check(Some(index), false);
        }
        // The key of each `--reason` text with two `=` or more (design 37.6).
        if !run.other_splits.is_empty()
            && let Ok(found) = &result
        {
            let report = found.report.steps.first().cloned().unwrap_or_default();
            let (chosen, again) = reason_splits(&report, &run.claim_anchors, &run.reason_keys, &run.other_splits);
            let mut reasons = BTreeMap::new();
            for (index, (_, key, reason)) in run.reason_keys.iter_mut().enumerate() {
                if let Some(split) = chosen.get(&index) {
                    (*key, *reason) = (split.key.clone(), split.reason.clone());
                }
                reasons.insert(key.clone(), reason.clone());
            }
            // The stored step has only the key of each text.
            let mut session = self.load_session()?;
            if let Some(open) = session.open.as_mut() {
                open.reasons = reasons;
            }
            self.save_session(&session)?;
            if again {
                // A key that is not the key of its text gave a reason to a fact: the result
                // is not the result of the final keys. The check runs again.
                self.no_bodies = no_bodies || !run.full;
                result = self.check(Some(index), false);
            }
        }
        self.no_bodies = no_bodies;
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                // A step that stops with an error starts nothing too (batch 5n, A6): the tagged
                // change set stays as it was.
                let mut restore = if run.closed.is_some() { run.previous.clone() } else { run.saved.clone() };
                restore.open = None;
                self.save_session(&restore)?;
                Err(error)
            }
        }
    }

    /// The `--reason` texts that no fact and no claim uses.
    fn unused_reasons(&mut self, run: &StepRun, report: &semilla_core::report::StepReport) -> UnusedReasons {
        // A reason that no fact and no claim uses (brief 5f, A4): a partial or wrong anchor. The
        // step tells it, and the command to run again omits it.
        let list: Vec<(String, String, String)> = run
            .reason_keys
            .iter()
            // Design 37.19: a kind reason is in use only if it covers a spotlight.
            .filter(|(_, key, reason)| {
                if key.starts_with(semilla_core::report::KIND_KEY) {
                    !report.facts.iter().any(|fact| fact.reason_key.as_ref() == Some(key))
                } else {
                    !report.facts.iter().any(|fact| fact.reason.as_ref() == Some(reason)) && !run.claim_anchors.contains(key)
                }
            })
            .cloned()
            .collect();
        // The unused keys that are a module path or a crate name.
        let mut wide_keys: BTreeMap<String, &'static str> = BTreeMap::new();
        if !list.is_empty()
            && let Ok(after_ws) = self.workspace(&run.tree)
        {
            for (_, key, _) in &list {
                if after_ws.crates.iter().any(|found| found.name == *key) {
                    wide_keys.insert(key.clone(), "crate");
                } else if after_ws.items.get(key).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Mod) {
                    wide_keys.insert(key.clone(), "module");
                }
            }
        }
        UnusedReasons { list, wide_keys }
    }

    /// The lines of a step result after its first line: the base commit, the sandbox, the build
    /// code, and the configurations.
    fn step_notes(&mut self, index: usize, result: &crate::app::CheckResult) -> String {
        // The first step of a change set states its base commit after the result line (design
        // 25.2). A change set with no step: its base commit is HEAD.
        let base_line = if index == 0 {
            self.base_notice(&semilla_core::ChangeSet::default(), None)
                .map_or_default(|notice| format!("{notice}\n"))
        } else {
            String::new()
        };
        // A build with a limit of the sandbox missing (design 27), and a file of the records
        // folder that is not a record (design 28.8): each result says so.
        let sandbox_line = crate::app::surround_each(crate::oracle::build_warnings(), "  ! ", "\n");
        // Build code that the change set changes (design 28): the step that changes it has the
        // full line, and each later step has the short form.
        let sandbox_line = format!(
            "{sandbox_line}{}",
            result
                .report
                .build_code
                .as_ref()
                .and_then(|note| note.step_line_with(index, semilla_normal::sandbox::missing().map(|missing| missing.state()).as_deref()))
                .map_or_default(|line| format!("{}\n", semilla_verify::views::fit_line(&format!("  {line}"))))
        );
        // A configuration of the default list with no check in this step (batch 5s, B1): one
        // line. A step with a check in each configuration has no line.
        let sandbox_line = format!(
            "{sandbox_line}{}",
            crate::app::surround_each(
                result
                    .report
                    .configs
                    .iter()
                    .flat_map(|note| note.gap_lines(1))
                    .map(|line| semilla_verify::views::fit_line(&format!("  {line}"))),
                "",
                "\n"
            )
        );
        // A configuration that Semilla added for a joint state of this step (batch 5u, part C): one line.
        let sandbox_line = format!(
            "{sandbox_line}{}",
            crate::app::surround_each(
                result
                    .report
                    .configs
                    .iter()
                    .filter_map(semilla_core::report::ConfigNote::added_line)
                    .map(|line| semilla_verify::views::fit_line(&format!("  {line}"))),
                "",
                "\n"
            )
        );
        base_line + &sandbox_line
    }

    /// Ends a rejected step: nothing is stored. The text has the reasons first, then the command
    /// to run again.
    fn step_rejected(&mut self, run: StepRun, out: StepText) -> Result<(String, bool)> {
        let StepText {
            report,
            body,
            mut text,
            notes,
            unused,
        } = out;
        let request = run.request;
        // Nothing is stored. The reasons first, then the command to run again.
        let mut restore = run.saved;
        restore.open = None;
        if run.closed.is_some() {
            // The tagged change set stays as it was: a rejected step starts nothing.
            restore = run.previous;
            restore.open = None;
        }
        self.save_session(&restore)?;
        put_line(
            &mut text,
            &format!(
                "rejected: step {} {} is not stored",
                run.index,
                semilla_core::text::quoted(&request.intent)
            ),
        );
        text.push_str(&notes);
        if run.closed.is_none() {
            refresh_fast_steps(&mut restore, run.background.as_ref());
            self.save_session(&restore)?;
            text.push_str(&fast_step_lines(&restore, None));
        }
        unused.put_lines(&mut text, &report);
        // The command to run again (M3): with the missing reasons, and without the false
        // claims. A cause that only a code change can fix omits it.
        let mut corrected = request.clone();
        corrected.reasons.retain(|reason| !unused.list.iter().any(|(text, _, _)| text == reason));
        let mut retry = Retry {
            corrected,
            missing: missing_reasons(&report),
            blocked: Vec::new(),
            widened: Vec::new(),
        };
        retry.add_claims(&mut text, &report, &run.claim_texts);
        retry.add_errors(&mut text, &report);
        // C3: the facts first, and the command to run again on the last line.
        text.push('\n');
        text.push_str(&body);
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let again = retry.again(&mut text, request);
        restore.rejected = Some(crate::app::RejectedStep {
            intent: request.intent.clone(),
            cause: retry.cause(&report),
            command: again,
        });
        self.save_session(&restore)?;
        Ok((text, false))
    }

    /// Ends an accepted step: stores the step and its group.
    fn step_accepted(&mut self, run: &StepRun, result: &crate::app::CheckResult, out: StepText) -> Result<(String, bool)> {
        let StepText {
            report,
            body,
            mut text,
            notes,
            unused,
        } = out;
        let (request, index, full) = (run.request, run.index, run.full);
        // Accepted: store the step and its group.
        self.step_end_with(false)?;
        let (id, title, base_tree) = self.store_step_group(run, &report)?;
        // B4: a commit between the steps makes `tag` fail. Say it now, with the fix.
        if index > 0
            && let Some(line) = self.head_moved_line(&base_tree)
        {
            put_line(&mut text, &line);
        }
        let fast_lines = self.fast_step_states(run)?;
        let result_line = if full {
            "accepted"
        } else {
            "accepted (phase 1; phase 2 runs in the background: `semilla status`)"
        };
        put_line(
            &mut text,
            &format!(
                "{result_line}: step {index} {} stored in group {id} {}",
                semilla_core::text::quoted(&request.intent),
                semilla_core::text::quoted(&title)
            ),
        );
        text.push_str(&notes);
        text.push_str(&fast_lines);
        unused.put_lines(&mut text, &report);
        text.push_str(&body);
        text.push('\n');
        if full {
            // B1: the final result has the pair warnings of the step, so that `tag` brings no
            // new information.
            for pair in &result.report.pairs {
                put_line(&mut text, &format!("  ! pair: {}", pair.text()));
            }
        }
        if request.tree {
            text.push_str(&result.tree.render());
        }
        Ok((text, true))
    }

    /// Puts the stored step into its group, and queues the background check. Returns the id and
    /// the title of the group, and the base tree of the change set.
    fn store_step_group(&mut self, run: &StepRun, report: &semilla_core::report::StepReport) -> Result<(String, String, String)> {
        let title = run.request.group.clone().unwrap_or_else(|| run.request.intent.clone());
        let mut session = self.load_session()?;
        // The kind comes from the facts and the compare results (phase 2). In phase 1 a compare
        // result is pending, so the kind is `change`, and the review computes it again from the
        // full report.
        let step_kind = semilla_verify::organize::derived_kind(&[report]).unwrap_or(GroupKind::Change);
        let found = session.changeset.groups.iter().find(|g| g.title == title).cloned();
        let (id, kind, why, mut steps) = match found {
            Some(group) => (
                group.id,
                semilla_verify::organize::merge_kinds(group.kind, step_kind),
                group.why,
                group.steps,
            ),
            None => (format!("G{}", session.changeset.groups.len() + 1), step_kind, String::new(), Vec::new()),
        };
        steps.push(run.index);
        session.last_claim = run.highest + run.claim_count;
        self.save_session(&session)?;
        self.group_set(&id, kind, &title, &why, steps)?;
        // The background check gets the change set with the group of the step, so that the stored
        // result agrees with the session of the next command.
        let stored_set = self.load_session()?.changeset;
        // Design 37.21: a change set of one step has its full result in this process. The step
        // stores it, so the background check and `semilla review` do not check the step again.
        // A failure here is not a failure of the step: the background check does the work.
        if run.full && !self.no_bodies && stored_set.steps.len() == 1 {
            let _ = self.store_session_result(&stored_set);
        }
        self.queue_session(&stored_set)?;
        Ok((id, title, session.changeset.base_tree))
    }

    /// The note for a HEAD that moved since the change set began, with the fix.
    fn head_moved_line(&self, base_tree: &str) -> Option<String> {
        let head = self.repo.commit_tree("HEAD").unwrap_or_default();
        if head == base_tree {
            return None;
        }
        let route = match self.base_commit(base_tree) {
            Some(base) => format!("; before `semilla tag`, run `git reset --soft {base}` and make one commit"),
            None => String::new(),
        };
        Some(format!(
            "note: HEAD moved since the change set began (do not commit between steps){route}"
        ))
    }

    /// D7 (brief 5h, C2): a step of `--fast` has no final result. Each later step prints the
    /// phase 2 state of each such step. A step that runs phase 2 computes the state now: the
    /// records of the trees are in the cache after the background check, so this costs little.
    fn fast_step_states(&mut self, run: &StepRun) -> Result<String> {
        let mut fast = FastStates {
            session: self.load_session()?,
            first_rejected: None,
            final_lines: String::new(),
        };
        for earlier in fast.session.fast_steps.keys().copied().collect::<Vec<_>>() {
            self.fast_step_state(&mut fast, earlier, run);
        }
        let FastStates {
            mut session,
            first_rejected,
            final_lines,
        } = fast;
        let route = first_rejected.as_ref().map(|step| self.fix_route(&session.changeset, step, false));
        let fast_lines = final_lines + &fast_step_lines(&session, route);
        if !run.full {
            session.fast_steps.insert(run.index, None);
        }
        self.save_session(&session)?;
        Ok(fast_lines)
    }

    /// Finds the phase 2 state of the earlier step `earlier` of `--fast`.
    fn fast_step_state(&mut self, fast: &mut FastStates, earlier: usize, run: &StepRun) {
        let report = if run.full {
            match self.check(Some(earlier), false) {
                Ok(result) => result.report.steps.first().cloned(),
                Err(error) => {
                    fast.session
                        .fast_steps
                        .insert(earlier, Some(format!("{error:#}").lines().next().unwrap_or_default().to_string()));
                    return;
                }
            }
        } else {
            // A step of `--fast` runs no phase 2. The background check has the result when
            // it is complete for the change set before this step.
            run.background
                .as_ref()
                .and_then(|stored| stored.report.steps.iter().find(|step| step.step == earlier).cloned())
        };
        match report {
            Some(step) if !step.accepted() => {
                fast.session.fast_steps.insert(earlier, Some(crate::app::step_problems(&step).join("; ")));
                fast.first_rejected.get_or_insert(step);
            }
            Some(_) => {
                // Phase 2 accepts the step: the result is final.
                fast.session.fast_steps.remove(&earlier);
                put_line(&mut fast.final_lines, &format!("step {earlier}: phase 2 accepted"));
            }
            None => {}
        }
    }

    /// Removes the last stored step (`semilla undo`), so that the agent can do it again.
    pub(crate) fn undo(&mut self) -> Result<String> {
        let mut session = self.load_session()?;
        if session.open.is_some() {
            bail!("a step of the old protocol is open; remove it with `semilla old-step drop`");
        }
        if session.tagged.is_some() {
            bail!("the change set is tagged; a tagged step cannot be removed");
        }
        let step = session.changeset.steps.pop().context("no step to remove")?;
        session.rejected = None;
        session.fast_steps.retain(|index, _| *index < step.index);
        session.changeset.head_tree.clone_from(&step.tree_before);
        if session.changeset.steps.is_empty() {
            session.changeset.head_tree.clear();
        }
        for group in &mut session.changeset.groups {
            group.steps.retain(|i| *i != step.index);
        }
        session.changeset.groups.retain(|g| !g.steps.is_empty());
        self.save_session(&session)?;
        // C9: the exact command that also restores the files, and the trap of `git checkout .`.
        Ok(format!(
            "step {} {} is removed; the working tree keeps its changes\nto also restore the files of the step before: `git restore --source={} --worktree -- .` (a file that the step added stays: remove it by hand)\ndo not use `git checkout .`: it goes back to HEAD, not to the last step",
            step.index,
            semilla_core::text::quoted(&step.intent),
            step.tree_before
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::split_words;

    #[test]
    fn a_claim_accepts_each_anchor_that_semilla_prints() {
        // Batch 5w, D2: the anchor of an impl item has spaces between angle brackets.
        let words = split_words("tested-by humantime::duration::<FormattedDuration as fmt::Display>::fmt test=t_fmt");
        assert_eq!(
            words,
            ["tested-by", "humantime::duration::<FormattedDuration as fmt::Display>::fmt", "test=t_fmt"]
        );
        assert_eq!(
            split_words("no-panic <Unit as FromStr>::from_str/S0/arm[1].pat"),
            ["no-panic", "<Unit as FromStr>::from_str/S0/arm[1].pat"]
        );
        assert_eq!(
            split_words("calls-only a::<W<T> as From<Vec<T>>>::from calls=[\"core::\",\"std::\"]"),
            ["calls-only", "a::<W<T> as From<Vec<T>>>::from", "calls=[\"core::\",\"std::\"]"]
        );
        assert_eq!(
            split_words("tested-by a::f test=<T as Checks>::t_f"),
            ["tested-by", "a::f", "test=<T as Checks>::t_f"]
        );
        // The quotes work as before.
        assert_eq!(split_words("no-panic \"a::<T as B>::f\""), ["no-panic", "a::<T as B>::f"]);
        // A `<` with no `>` does not join the words.
        assert_eq!(split_words("pure a::<T as B key=1"), ["pure", "a::<T", "as", "B", "key=1"]);
        // A `<` in the middle of a word is not the start of an impl anchor.
        assert_eq!(split_words("pure a::f note=x<y z"), ["pure", "a::f", "note=x<y", "z"]);
    }
}

#[cfg(test)]
mod reason_tests {
    use super::*;
    use semilla_core::report::{Fact, StepReport};

    fn fact(part: &str, reason: Option<&str>) -> Fact {
        Fact {
            id: "f1".into(),
            part: part.into(),
            spotlight: true,
            reason: reason.map(str::to_string),
            ..Fact::default()
        }
    }

    #[test]
    fn the_longest_open_key_is_the_key_of_a_reason_text() {
        // Design 37.6. The text `a=b=c` can have the key `a` or the key `a=b`.
        let first = vec![("a=b=c".to_string(), "a".to_string(), "b=c".to_string())];
        let others = vec![ReasonSplit {
            text: 0,
            key: "a=b".into(),
            reason: "c".into(),
        }];
        // Only `a` is open: the first `=` splits the text, and no check runs again.
        let report = StepReport {
            facts: vec![fact("a", Some("b=c")), fact("z", None)],
            ..StepReport::default()
        };
        let (chosen, again) = reason_splits(&report, &[], &first, &others);
        assert!(chosen.is_empty() && !again);
        // Only `a=b` is open: it is the key, and the result is already the result of this key.
        let report = StepReport {
            facts: vec![fact("a=b", Some("c"))],
            ..StepReport::default()
        };
        let (chosen, again) = reason_splits(&report, &[], &first, &others);
        assert!(chosen[&0].key == "a=b" && chosen[&0].reason == "c" && !again);
        // The two keys are open: the longest key is the key. The fact `a` got a reason that is
        // not its reason, so the check runs again.
        let report = StepReport {
            facts: vec![fact("a", Some("b=c")), fact("a=b", Some("c"))],
            ..StepReport::default()
        };
        let (chosen, again) = reason_splits(&report, &[], &first, &others);
        assert!(chosen[&0].key == "a=b" && again);
        // No key is open: the first `=` splits the text.
        let report = StepReport {
            facts: vec![fact("z", None)],
            ..StepReport::default()
        };
        let (chosen, again) = reason_splits(&report, &[], &first, &others);
        assert!(chosen.is_empty() && !again);
        // A fact with the same reason text and a different key does not make a key open.
        let report = StepReport {
            facts: vec![fact("z", Some("c"))],
            ..StepReport::default()
        };
        let (chosen, again) = reason_splits(&report, &[], &first, &others);
        assert!(chosen.is_empty() && !again);
        // The anchor of a claim is a key too.
        let (chosen, _) = reason_splits(&report, &["a=b".to_string()], &first, &others);
        assert_eq!(chosen[&0].key, "a=b");
    }
}
