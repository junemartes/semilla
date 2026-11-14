//! One command for each step (design 24.3):
//! `semilla step "<intent>" [--group T] [--claim '…']… [--reason 'ANCHOR=TEXT']…`.
//!
//! "Before" is the head tree of the last step (or `HEAD` for the first step). "After" is the
//! working tree. The command derives the facts, checks the explicit claims, and stores the step
//! only when the result is accepted.

use semilla_core::text::put_line;
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use semilla_core::{Anchor, Claim, GroupKind};

use crate::app::{App, OpenStep, Session};

/// The claims that stay (design 24.2, rule 5, and design 35.3). Each other op is retired as a
/// claim.
pub const CLAIMS: &[&str] = &["rewrite-pattern", "no-panic", "no-unsafe", "no-io", "calls-only", "tested-by"];

/// The message for an op that is retired as a claim, or `None`.
pub fn retired_claim(op: &str) -> Option<String> {
    // Design 35.3: Semilla derives each rename and each move. A claim cannot add to the fact.
    if matches!(op, "rename" | "move-item") {
        return Some(format!("the claim `{op}` is gone: Semilla derives each rename and each move as a fact, with the compare result of the compiled code; remove the claim (the claims that stay: {})", CLAIMS.join(", ")));
    }
    (!CLAIMS.contains(&op) && semilla_core::ops::op(op).is_some()).then(|| format!("the claim `{op}` is retired: Semilla derives this change as a fact; remove the claim (the claims that stay: {})", CLAIMS.join(", ")))
}

/// The input of one `step` command.
#[derive(Clone, Debug, Default)]
pub struct StepRequest {
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

impl StepRequest {
    /// The command line that runs this request again, with extra reasons.
    pub fn command(&self, extra: &[String]) -> String {
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
        Some(route) => text.push_str(&format!("{route}\n")),
        None => {
            if let Some(first) = session.fast_steps.iter().find(|(_, state)| state.is_some()).map(|(step, _)| *step) {
                let count = session.changeset.steps.iter().filter(|step| step.index >= first).count();
                put_line(&mut text, &format!("to fix step {first}: run `semilla undo` {count} time(s) (the files stay), then store each removed step again"));
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
    let words: Vec<String> = words.into_iter().map(|word| if word.contains(char::is_whitespace) { format!("\"{word}\"") } else { word }).collect();
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
    if head.ends_with(".rs") {
        return Some((head.to_string(), last, None));
    }
    let (file, line) = head.rsplit_once(':')?;
    let line: usize = line.parse().ok()?;
    file.ends_with(".rs").then(|| (file.to_string(), line, Some(last)))
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
    pub fn step_once(&mut self, request: &StepRequest) -> Result<(String, bool)> {
        if request.intent.trim().is_empty() {
            bail!("the intent has no text. The reviewer reads the intent: say what the step does, for example `semilla step \"rename the field\"`");
        }
        let mut session = self.load_session()?;
        // The record of a rejected step is for the last `semilla step` only.
        session.rejected = None;
        if session.open.is_some() {
            bail!("a step of the old protocol is open (the hidden command `old-step`, for the replay); end it with `semilla old-step end`, or remove it with `semilla old-step drop`");
        }
        if session.replay.is_some() {
            bail!("a replay is in progress, and `semilla step` does not work in a replay; `semilla replay apply` runs the plan, and `semilla replay abort` stops the replay");
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
            let head = self.repo.commit_tree("HEAD").context("the repository has no commit; commit the base first")?;
            session.changeset.base_tree = head.clone();
            head
        } else {
            session.changeset.head_tree.clone()
        };
        if before == tree {
            bail!("the working tree has no change since the last step (tree {tree}); edit the code first");
        }

        // The claims and the reasons.
        let mut claims = Vec::new();
        let mut claim_texts: Vec<(String, String)> = Vec::new();
        // The claim ids count the stored claims of the change set (trial 4, D14): a rejected step
        // stores no claim, so its ids are free again. The first stored claim is `c1`.
        let highest = session.changeset.steps.iter().flat_map(|s| &s.claims).filter_map(|c| c.id.strip_prefix('c').and_then(|n| n.parse::<usize>().ok())).max().unwrap_or(0);
        for (offset, text) in request.claims.iter().enumerate() {
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
            claim_texts.push((format!("c{}", highest + offset + 1), text.clone()));
            claims.push(Claim { id: format!("c{}", highest + offset + 1), op: op.to_string(), at, args });
        }
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

        // B1: phase 2 runs in the command, so the result is final: phase 2 can reject a step (an
        // indirect change with no known cause). With `--fast`, phase 2 runs now only when a
        // property claim or an identity candidate needs it.
        let mut full = !request.fast || claims.iter().any(|c| semilla_core::ops::op(&c.op).is_some_and(|i| i.kind == semilla_core::OpKind::Property));
        let index = session.changeset.steps.len();
        let claim_count = claims.len();
        let claim_anchors: Vec<String> = claims.iter().filter_map(|claim| claim.at.as_ref().map(|at| at.to_string())).collect();
        session.open = Some(OpenStep { intent: request.intent.clone(), tree_before: before.clone(), claims, reasons });
        // The session to restore when the step is not stored: the counter of the stored claims.
        let mut saved = session.clone();
        saved.last_claim = highest;
        session.last_claim = highest + claim_count;
        self.save_session(&session)?;
        let no_bodies = self.no_bodies;
        self.no_bodies = no_bodies || !full;
        let mut result = self.check(Some(index), false);
        // A candidate pair for a derived identity: only phase 2 can derive it (design 24.2, rule 4).
        if !full && !no_bodies && result.as_ref().is_ok_and(|r| r.report.steps.first().is_some_and(|s| s.identity_candidates)) {
            full = true;
            self.no_bodies = false;
            result = self.check(Some(index), false);
        }
        self.no_bodies = no_bodies;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // A step that stops with an error starts nothing too (batch 5n, A6): the tagged
                // change set stays as it was.
                let mut restore = if closed.is_some() { previous.clone() } else { saved.clone() };
                restore.open = None;
                self.save_session(&restore)?;
                return Err(error);
            }
        };
        let report = result.report.steps.first().cloned().unwrap_or_default();
        let accepted = report.accepted();
        // A rejected step prints the full text of each failed claim in its first lines. The claim
        // list then has only the status and a short cause (brief 5g, item 4).
        let body = semilla_verify::views::text_report_with(std::slice::from_ref(&report), !accepted);
        let body = body.lines().skip(1).collect::<Vec<_>>().join("\n");
        let mut text = String::new();
        if let Some(commit) = &closed {
            put_line(&mut text, &format!("the change set of {commit} is tagged and closed; a new change set starts at HEAD"));
        }
        // A reason that no fact and no claim uses (brief 5f, A4): a partial or wrong anchor. The
        // step tells it, and the command to run again omits it.
        let unused: Vec<&(String, String, String)> = reason_keys
            .iter()
            .filter(|(_, key, reason)| !report.facts.iter().any(|fact| fact.reason.as_ref() == Some(reason)) && !claim_anchors.contains(key))
            .collect();
        // The unused keys that are a module path or a crate name.
        let mut wide_keys: BTreeMap<String, &str> = BTreeMap::new();
        if !unused.is_empty() {
            if let Ok(after_ws) = self.workspace(&tree) {
                for (_, key, _) in &unused {
                    if after_ws.crates.iter().any(|found| found.name == *key) {
                        wide_keys.insert(key.clone(), "crate");
                    } else if after_ws.items.get(key).is_some_and(|item| item.kind == semilla_syntax::ItemKind::Mod) {
                        wide_keys.insert(key.clone(), "module");
                    }
                }
            }
        }
        // A key that is the name of a body, not the anchor of its fact (batch 5l, B3): the
        // message gives the key of the fact.
        let unused_line = |key: &String| -> String {
            let keys: Vec<String> = report
                .facts
                .iter()
                .filter(|fact| fact.spotlight_why.is_some() && fact.affected.contains(key))
                .map(|fact| format!("`{}`", fact.at.as_ref().map(|at| at.to_string()).unwrap_or_else(|| fact.part.clone())))
                .collect();
            // Batch 5y, B3: a key that is the file of a fact, or the text of a fact. A message
            // prints these texts, so the message gives the keys that the next call accepts.
            let file_keys: Vec<String> = report
                .facts
                .iter()
                .filter(|fact| fact.spotlight && fact.reason.is_none())
                .filter(|fact| fact.description == *key || (fact.at.is_none() && fact.part.strip_prefix(key.as_str()).is_some_and(|rest| rest.starts_with(':') && rest.len() > 1)))
                .map(|fact| format!("`{}`", fact.reason_place()))
                .collect();
            let hint = if !keys.is_empty() {
                format!(". `{key}` is a body of a fact with a spotlight: the key of the reason is {}", keys.join(", "))
            } else if !file_keys.is_empty() {
                format!(". `{key}` is not the key of a reason. Each fact has its own key: {}", file_keys.join(", "))
            } else if let Some(kind) = wide_keys.get(key) {
                // Batch 5x, part D: a module path and a crate name are not a key of a reason.
                format!(". `{key}` is a {kind}, and a {kind} is not a valid key of a reason. A key is the path of an item (a function, a method, a constant, a static, a type, a trait), the exact key of a spotlight, or a file key (`path/to/file:`)")
            } else {
                String::new()
            };
            format!("  ! the reason for `{key}` belongs to no fact of this step; Semilla does not use it{hint}")
        };
        // The first step of a change set states its base commit after the result line (design
        // 25.2). A change set with no step: its base commit is HEAD.
        let base_line = if index == 0 { self.base_notice(&semilla_core::ChangeSet::default(), None).map(|notice| format!("{notice}\n")).unwrap_or_default() } else { String::new() };
        // A build with a limit of the sandbox missing (design 27), and a file of the records
        // folder that is not a record (design 28.8): each result says so.
        let sandbox_line: String = crate::oracle::build_warnings().iter().map(|warning| format!("  ! {warning}\n")).collect();
        // Build code that the change set changes (design 28): the step that changes it has the
        // full line, and each later step has the short form.
        let sandbox_line = format!("{sandbox_line}{}", result.report.build_code.as_ref().and_then(|note| note.step_line_with(index, semilla_normal::sandbox::missing().map(|missing| missing.state()).as_deref())).map(|line| format!("  {line}\n")).unwrap_or_default());
        // A configuration of the default list with no check in this step (batch 5s, B1): one
        // line. A step with a check in each configuration has no line.
        let sandbox_line = format!("{sandbox_line}{}", result.report.configs.iter().flat_map(|note| note.gap_lines(1)).map(|line| format!("  {line}\n")).collect::<String>());
        // A configuration that Semilla added for a joint state of this step (batch 5u, part C): one line.
        let sandbox_line = format!("{sandbox_line}{}", result.report.configs.iter().flat_map(|note| note.added_line()).map(|line| format!("  {line}\n")).collect::<String>());
        if !accepted {
            // Nothing is stored. The reasons first, then the command to run again.
            let mut restore = saved;
            restore.open = None;
            if closed.is_some() {
                // The tagged change set stays as it was: a rejected step starts nothing.
                restore = previous;
                restore.open = None;
            }
            self.save_session(&restore)?;
            put_line(&mut text, &format!("rejected: step {index} {} is not stored", semilla_core::text::quoted(&request.intent)));
            text.push_str(&base_line);
            text.push_str(&sandbox_line);
            if closed.is_none() {
                for earlier in restore.fast_steps.keys().copied().collect::<Vec<_>>() {
                    match background.as_ref().and_then(|stored| stored.report.steps.iter().find(|step| step.step == earlier)) {
                        Some(step) if !step.accepted() => {
                            restore.fast_steps.insert(earlier, Some(crate::app::step_problems(step).join("; ")));
                        }
                        Some(_) => {
                            restore.fast_steps.remove(&earlier);
                        }
                        None => {}
                    }
                }
                self.save_session(&restore)?;
                text.push_str(&fast_step_lines(&restore, None));
            }
            for (_, key, _) in &unused {
                put_line(&mut text, &unused_line(key));
            }
            // The command to run again (M3): with the missing reasons, and without the false
            // claims. A cause that only a code change can fix omits it.
            let mut corrected = request.clone();
            corrected.reasons.retain(|reason| !unused.iter().any(|(text, _, _)| text == reason));
            let mut missing = Vec::new();
            let mut blocked = Vec::new();
            // The `calls-only` claims that the command changes: the claim, and the new entries.
            let mut widened: Vec<(String, Vec<String>)> = Vec::new();
            for fact in report.facts.iter().filter(|f| f.spotlight && f.reason.is_none()) {
                let place = fact.at.as_ref().map(|a| a.to_string()).unwrap_or_else(|| fact.part.clone());
                // Part B of batch 5j: the body below shows the fact with "it needs a reason" and
                // the exact anchor. The top summary does not repeat the reason line of the fact.
                // Batch 5y, B3: two facts of one item have one key. The command gives it one time.
                let entry = format!("{place}=<why>");
                if !missing.contains(&entry) {
                    missing.push(entry);
                }
            }
            for claim in report.claims.iter().filter(|c| !c.status.is_verified()) {
                let written = claim_texts.iter().find(|(id, _)| *id == claim.claim).map(|(_, text)| text.clone());
                let label = written.as_ref().map(|w| format!(" (`{w}`)")).unwrap_or_default();
                put_line(&mut text, &format!("  ✗ claim {}{label} is {}{}", claim.claim, claim.status.text(), claim.hint.as_ref().map(|h| format!("; {h}")).unwrap_or_default()));
                match written {
                    // A `calls-only` claim with missing entries: the command has the claim with
                    // the complete list (design 26.3). This changes the claim, and the text says so.
                    Some(written) if !claim.complete_calls.is_empty() => {
                        let (text, added) = with_complete_calls(&written, &claim.complete_calls);
                        for slot in corrected.claims.iter_mut().filter(|c| **c == written) {
                            *slot = text.clone();
                        }
                        widened.push((claim.claim.clone(), added));
                    }
                    Some(written) => corrected.claims.retain(|c| *c != written),
                    None => blocked.push(format!("claim {}", claim.claim)),
                }
            }
            for error in &report.errors {
                // The body below shows each error one time. The top summary does not repeat it
                // (Part B item 4 of batch 5j).
                blocked.push(error.clone());
            }
            for uncovered in &report.uncovered {
                put_line(&mut text, &format!("  ✗ internal: no fact for {}", uncovered.description));
                blocked.push(uncovered.description.clone());
            }
            // C3: the facts first, and the command to run again on the last line.
            text.push('\n');
            text.push_str(&body);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            let mut again = None;
            if blocked.is_empty() {
                let what = match (corrected.claims.len() < request.claims.len(), !widened.is_empty()) {
                    (true, true) => " (without the claim that failed, and with the complete list of `calls`)",
                    (true, false) => " (without the claim that failed)",
                    (false, true) => " (with the complete list of `calls`)",
                    (false, false) => "",
                };
                for (claim, added) in &widened {
                    let entries: Vec<String> = added.iter().map(|entry| format!("`{entry}`")).collect();
                    text.push_str(&format!(
                        "the command below changes the claim {claim}: it adds {} to `calls`. Use it only if the function must make these calls. If not, change the code.\n",
                        entries.join(", ")
                    ));
                }
                put_line(&mut text, &format!("run again{what}: {}", corrected.command(&missing)));
                again = Some(corrected.command(&missing));
            } else if blocked.iter().any(|error| error.contains(semilla_normal::cargo::MISSING_DEPENDENCY)) {
                text.push_str("a code change is not necessary: run `cargo fetch` in the repository, then run the step again\n");
            } else {
                text.push_str("no command can succeed without a code change: fix the cause, then run the step again\n");
            }
            // `tag` and `status` say this cause, and not "run `semilla step`" (batch 5x, part C).
            let failed = report.claims.iter().filter(|c| !c.status.is_verified()).count();
            let mut causes = Vec::new();
            if !missing.is_empty() {
                causes.push(format!("{} spotlight{} with no reason", missing.len(), if missing.len() == 1 { "" } else { "s" }));
            }
            if failed > 0 {
                causes.push(format!("{failed} claim{} not verified", if failed == 1 { "" } else { "s" }));
            }
            if let Some(error) = blocked.iter().find(|error| !error.starts_with("claim ")) {
                causes.push(error.lines().next().unwrap_or("").chars().take(200).collect());
            }
            if causes.is_empty() {
                causes.push("the result of the step says the cause".to_string());
            }
            restore.rejected = Some(crate::app::RejectedStep { intent: request.intent.clone(), cause: causes.join("; "), command: again });
            self.save_session(&restore)?;
            return Ok((text, false));
        }
        // Accepted: store the step and its group.
        let stored = self.step_end_with(false)?;
        let title = request.group.clone().unwrap_or_else(|| request.intent.clone());
        let mut session = self.load_session()?;
        // The kind comes from the facts and the compare results (phase 2). In phase 1 a compare
        // result is pending, so the kind is `behavior`, and the review computes it again from the
        // full report.
        let step_kind = semilla_verify::organize::derived_kind(&[&report]).unwrap_or(GroupKind::Behavior);
        let found = session.changeset.groups.iter().find(|g| g.title == title).cloned();
        let (id, kind, why, mut steps) = match found {
            Some(group) => (group.id, semilla_verify::organize::merge_kinds(group.kind, step_kind), group.why, group.steps),
            None => (format!("G{}", session.changeset.groups.len() + 1), step_kind, String::new(), Vec::new()),
        };
        steps.push(index);
        session.last_claim = highest + claim_count;
        self.save_session(&session)?;
        self.group_set(&id, kind, &title, &why, steps)?;
        // The background check gets the change set with the group of the step, so that the stored
        // result agrees with the session of the next command.
        self.queue_session(&self.load_session()?.changeset)?;
        // B4: a commit between the steps makes `tag` fail. Say it now, with the fix.
        if index > 0 {
            let head = self.repo.commit_tree("HEAD").unwrap_or_default();
            if head != session.changeset.base_tree {
                let route = match self.base_commit(&session.changeset.base_tree) {
                    Some(base) => format!("; before `semilla tag`, run `git reset --soft {base}` and make one commit"),
                    None => String::new(),
                };
                put_line(&mut text, &format!("note: HEAD moved since the change set began (do not commit between steps){route}"));
            }
        }
        // D7 (brief 5h, C2): a step of `--fast` has no final result. Each later step prints the
        // phase 2 state of each such step. A step that runs phase 2 computes the state now: the
        // records of the trees are in the cache after the background check, so this costs little.
        let mut session = self.load_session()?;
        let mut first_rejected = None;
        let mut final_lines = String::new();
        for earlier in session.fast_steps.keys().copied().collect::<Vec<_>>() {
            let report = if full {
                match self.check(Some(earlier), false) {
                    Ok(result) => result.report.steps.first().cloned(),
                    Err(error) => {
                        session.fast_steps.insert(earlier, Some(format!("{error:#}").lines().next().unwrap_or_default().to_string()));
                        continue;
                    }
                }
            } else {
                // A step of `--fast` runs no phase 2. The background check has the result when
                // it is complete for the change set before this step.
                background.as_ref().and_then(|stored| stored.report.steps.iter().find(|step| step.step == earlier).cloned())
            };
            match report {
                Some(step) if !step.accepted() => {
                    session.fast_steps.insert(earlier, Some(crate::app::step_problems(&step).join("; ")));
                    first_rejected.get_or_insert(step);
                }
                Some(_) => {
                    // Phase 2 accepts the step: the result is final.
                    session.fast_steps.remove(&earlier);
                    put_line(&mut final_lines, &format!("step {earlier}: phase 2 accepted"));
                }
                None => {}
            }
        }
        let route = first_rejected.as_ref().map(|step| self.fix_route(&session.changeset, step, false));
        let fast_lines = final_lines + &fast_step_lines(&session, route);
        if !full {
            session.fast_steps.insert(index, None);
        }
        self.save_session(&session)?;
        let result_line = if full { "accepted" } else { "accepted (phase 1; phase 2 runs in the background: `semilla status`)" };
        put_line(&mut text, &format!("{result_line}: step {index} {} stored in group {id} {}", semilla_core::text::quoted(&request.intent), semilla_core::text::quoted(&title)));
        text.push_str(&base_line);
        text.push_str(&sandbox_line);
        text.push_str(&fast_lines);
        for (_, key, _) in &unused {
            put_line(&mut text, &unused_line(key));
        }
        text.push_str(&body);
        text.push('\n');
        if full {
            // B1: the final result has the pair warnings of the step, so that `tag` brings no
            // new information.
            for pair in &result.report.pairs {
                put_line(&mut text, &format!("  ! pair: {}", pair.text()));
            }
        }
        let _ = stored;
        if request.tree {
            text.push_str(&result.tree.render());
        }
        Ok((text, true))
    }

    /// Removes the last stored step (`semilla undo`), so that the agent can do it again.
    pub fn undo(&mut self) -> Result<String> {
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
        session.changeset.head_tree = step.tree_before.clone();
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
            step.index, semilla_core::text::quoted(&step.intent), step.tree_before
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
        assert_eq!(words, ["tested-by", "humantime::duration::<FormattedDuration as fmt::Display>::fmt", "test=t_fmt"]);
        assert_eq!(split_words("no-panic <Unit as FromStr>::from_str/S0/arm[1].pat"), ["no-panic", "<Unit as FromStr>::from_str/S0/arm[1].pat"]);
        assert_eq!(split_words("calls-only a::<W<T> as From<Vec<T>>>::from calls=[\"core::\",\"std::\"]"), ["calls-only", "a::<W<T> as From<Vec<T>>>::from", "calls=[\"core::\",\"std::\"]"]);
        assert_eq!(split_words("tested-by a::f test=<T as Checks>::t_f"), ["tested-by", "a::f", "test=<T as Checks>::t_f"]);
        // The quotes work as before.
        assert_eq!(split_words("no-panic \"a::<T as B>::f\""), ["no-panic", "a::<T as B>::f"]);
        // A `<` with no `>` does not join the words.
        assert_eq!(split_words("pure a::<T as B key=1"), ["pure", "a::<T", "as", "B", "key=1"]);
        // A `<` in the middle of a word is not the start of an impl anchor.
        assert_eq!(split_words("pure a::f note=x<y z"), ["pure", "a::f", "note=x<y", "z"]);
    }
}
