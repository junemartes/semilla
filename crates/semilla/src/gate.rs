//! [R87.1-1] The gate of the claim templates of a step (design 87): the check of `semilla intent
//! check` (rustc compiles the intent crate against the two trees, then Semilla checks the meaning
//! of each attribute) on the templates of the step, in one intent file.

use std::collections::BTreeMap;

use semilla_core::Step;
use semilla_normal::intent::{Source, fnv};
use semilla_verify::template::{Ask, Verdict};

use crate::oracle::NormalOracle;

/// The path of the intent file of the gate in the messages of rustc.
const FILE: &str = ".semilla/intent/templates-of-the-step.rs";

/// The most compile rounds of the gate for one step.
const ROUNDS: usize = 12;

/// The intent file of a list of templates: one template on each line, from line 3.
fn file_text(texts: &[&str]) -> String {
    let mut text = String::from("use semilla_intent::prelude::*;\n");
    for template in texts {
        text.push('\n');
        text.push_str(&template.replace('…', "true"));
    }
    text.push('\n');
    text
}

/// The line of the template with the index `at` in the file of [`file_text`].
fn line_of(at: usize) -> usize {
    at + 3
}

use crate::intentcrate::is_panic;

impl NormalOracle {
    /// [R87.1-1] The verdict of the gate for each template of a step (see
    /// [`semilla_verify::BodyOracle::check_templates`]). The result goes to the cache, so a warm
    /// step starts no check again.
    pub(crate) fn gate_templates(&self, step: &Step, asks: &[Ask]) -> Option<Vec<Verdict>> {
        let driver = semilla_normal::cargo::find_driver().ok()?;
        let settings = semilla_normal::cargo::settings_key(&self.repo.cache_dir(), Some(&driver));
        let stamp = std::fs::metadata(&driver).and_then(|meta| meta.modified()).map(|time| format!("{time:?}")).unwrap_or_default();
        let mut keyed = format!("{}\n{}\n{settings}\n{stamp}\n", step.tree_before, step.tree_after);
        for ask in asks {
            keyed.push_str(&format!("{}\n{}\n", ask.forms.join("\n"), ask.expect.join("\n")));
        }
        let folder = self.repo.cache_dir().join("intent");
        let stored = folder.join(format!("gate-{:016x}.json", fnv(keyed.as_bytes())));
        if let Some(found) = std::fs::read_to_string(&stored).ok().and_then(|text| serde_json::from_str::<Vec<Verdict>>(&text).ok()).filter(|found| found.len() == asks.len()) {
            crate::timing("the gate of the claim templates (from the cache)", std::time::Instant::now());
            return Some(found);
        }
        let started = std::time::Instant::now();
        let found = self.gate_rounds(step, asks)?;
        crate::timing(&format!("the gate of the claim templates ({} templates)", asks.len()), started);
        let _ = std::fs::create_dir_all(&folder);
        if let Ok(text) = serde_json::to_string(&found) {
            let _ = std::fs::write(&stored, text);
        }
        Some(found)
    }

    /// The rounds of the gate: compile the live templates; a template with an error goes to its
    /// next form, or out with the error; repeat until one compile has no error. Then the scope
    /// path of each template must name its body (R87.2-1).
    fn gate_rounds(&self, step: &Step, asks: &[Ask]) -> Option<Vec<Verdict>> {
        let mut choice = vec![0usize; asks.len()];
        let mut verdict: Vec<Option<Verdict>> = asks.iter().map(|ask| ask.forms.is_empty().then(|| Err("the template has no text".to_string()))).collect();
        for _ in 0..ROUNDS {
            let live: Vec<usize> = (0..asks.len()).filter(|at| verdict[*at].is_none()).collect();
            if live.is_empty() {
                break;
            }
            let (errors, check) = self.gate_errors(step, asks, &choice, &live)?;
            if errors.is_empty() {
                let Some(check) = check else { break };
                for (index, at) in live.iter().enumerate() {
                    verdict[*at] = Some(match names_its_body(&check, &asks[*at], line_of(index)) {
                        Ok(()) => Ok(choice[*at]),
                        Err(cause) => Err(cause),
                    });
                }
                break;
            }
            for (at, cause) in errors {
                if choice[at] + 1 < asks[at].forms.len() {
                    choice[at] += 1;
                } else {
                    verdict[at] = Some(Err(cause));
                }
            }
        }
        Some(verdict.into_iter().map(|found| found.unwrap_or_else(|| Err("the gate stopped after its rounds of compiles".into()))).collect())
    }

    /// The errors of one compile of the templates `live` (each with its chosen form), by the
    /// index of the template. An error with no line of a template (a panic of the compiler, an
    /// error of the facade) splits the list in two halves until one template has it. With no
    /// error: the check of the compile.
    fn gate_errors(&self, step: &Step, asks: &[Ask], choice: &[usize], live: &[usize]) -> Option<(BTreeMap<usize, String>, Option<crate::intentcrate::Check>)> {
        let texts: Vec<&str> = live.iter().map(|at| asks[*at].forms[choice[*at]].as_str()).collect();
        let sources = vec![Source { path: FILE.into(), text: file_text(&texts) }];
        let check = match crate::intentcrate::check(self, &step.tree_before, &step.tree_after, &sources) {
            Ok(check) => check,
            Err(error) => {
                // [R87.1-1] The check stops before the templates (for example the facade of a tree
                // fails): the first line of the cause, and the first error of rustc in it.
                let text = format!("{error:#}");
                let first = text.lines().next().unwrap_or("").trim_end_matches(':');
                let rustc = text.lines().map(str::trim).find(|line| line.starts_with("error")).map(|line| format!(" ({line})")).unwrap_or_default();
                let cause = format!("the check of the intent files stops before the templates: {first}{rustc}");
                return Some((live.iter().map(|at| (*at, cause.clone())).collect(), None));
            }
        };
        let mut placed: BTreeMap<usize, String> = BTreeMap::new();
        let mut loose: Vec<String> = Vec::new();
        let owner = |file: Option<&str>, line: usize| -> Option<usize> { (file == Some(FILE) && line >= 3).then(|| line - 3).filter(|index| *index < live.len()) };
        for diagnostic in check.compiled.diagnostics.iter().filter(|d| d.level == "error") {
            let text = format!("error{}: {}", diagnostic.code.as_ref().map(|code| format!("[{code}]")).unwrap_or_default(), diagnostic.message);
            let text = if is_panic(&diagnostic.message) || is_panic(&diagnostic.rendered) { "the compiler stopped with an internal error on this template".to_string() } else { text };
            match owner(diagnostic.file.as_deref(), diagnostic.line) {
                Some(index) => {
                    placed.entry(live[index]).or_insert(text);
                }
                None => loose.push(text),
            }
        }
        for finding in check.findings.iter().filter(|finding| finding.error) {
            let text = format!("error[{}]: {}", finding.code, finding.message);
            match owner(Some(finding.file.as_str()), finding.line) {
                Some(index) => {
                    placed.entry(live[index]).or_insert(text);
                }
                None => loose.push(text),
            }
        }
        if placed.is_empty() && loose.is_empty() {
            return Some((placed, Some(check)));
        }
        if !loose.is_empty() {
            if live.len() == 1 {
                placed.entry(live[0]).or_insert_with(|| loose[0].clone());
                return Some((placed, None));
            }
            // An error with no place: each half alone.
            let (left, right) = live.split_at(live.len() / 2);
            let mut all = BTreeMap::new();
            for half in [left, right] {
                let (errors, _) = self.gate_errors(step, asks, choice, half)?;
                all.extend(errors);
            }
            if all.is_empty() {
                // The two halves compile alone: the error comes from the two together.
                all.insert(live[live.len() - 1], loose[0].clone());
            }
            return Some((all, None));
        }
        Some((placed, None))
    }
}

/// [R87.2-1] The property of a template: each scope path names the body of the template (the
/// definition that the driver gives for the path, or one of the items of that name of the
/// type for a qualified path, is a record name of the item).
fn names_its_body(check: &crate::intentcrate::Check, ask: &Ask, line: usize) -> Result<(), String> {
    if ask.expect.is_empty() {
        return Ok(());
    }
    for (_, heads) in &check.heads {
        for head in heads.iter().filter(|head| head.line == line) {
            for (index, scope) in head.scope.iter().enumerate() {
                for link in semilla_intent::heads::head_links(head, index, scope) {
                    let version = link.split("::").next().unwrap_or("");
                    let wanted: Vec<&String> = ask.expect.iter().filter(|name| name.split("::").next() == Some(version)).collect();
                    if wanted.is_empty() {
                        continue;
                    }
                    let Some(entry) = check.compiled.links.iter().find(|entry| entry.path == link) else { continue };
                    let mut named: Vec<&String> = entry.resolved.iter().collect();
                    named.extend(entry.also.iter());
                    let same = |a: &str, b: &str| a == b || semilla_intent::plain_name(a) == semilla_intent::plain_name(b);
                    if !named.iter().any(|name| wanted.iter().any(|want| same(name, want))) {
                        let got = entry.resolved.clone().unwrap_or_else(|| "no item".into());
                        return Err(format!("the scope path `{link}` names `{got}`, not the body `{}` of the template", wanted[0]));
                    }
                }
            }
        }
    }
    Ok(())
}
