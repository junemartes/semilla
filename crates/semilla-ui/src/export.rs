//! The standalone review report (design sections 11 and 19).
//!
//! `semilla review` writes the report for humans when the reviewer does not use the local UI.
//! There are two formats with the same content:
//!
//! - [`render_html`]: one HTML file. All styles and scripts are inline, so the file works offline.
//! - [`render_markdown`]: Markdown for a pull request or a terminal. GitHub draws the flow views
//!   from the ```` ```mermaid ```` blocks.
//!
//! The order is the review order of the UI: the groups and subgroups come from
//! [`summary::changeset`], with the same fold rule and risk.
//!
//! All text in the data comes from source code. The HTML renderer escapes all of it. The
//! Markdown renderer escapes inline text and puts code into code spans and code blocks.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Serialize;

use semilla_core::view::FlowGraph;
use semilla_core::{text, Category, Report, Uncovered};

use crate::ctx::{Ctx, category_label, contains, item_of};
use crate::review::ReviewFile;
use crate::summary::{self, ChangeSetSummary, ClaimView, Counts, GroupSummary, SubgroupSummary};
use crate::{SourcePair, UiData};

/// The vendored Mermaid library. The HTML report inlines it for the flow views.
const MERMAID_JS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/vendor/mermaid/mermaid.min.js"));

/// The script that draws the flow views. It runs after the Mermaid library.
const REPORT_JS: &str = r#"(function () {
  'use strict';
  if (typeof mermaid === 'undefined') return;
  var dark = !!(window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);
  mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default', flowchart: { useMaxWidth: true } });
  var nodes = document.querySelectorAll('pre.mermaid');
  if (nodes.length === 0) return;
  mermaid.run({ nodes: nodes, suppressErrors: true }).catch(function (error) {
    console.error('semilla: Mermaid cannot draw a flow view', error);
  });
})();
"#;

const CSS: &str = r#"
:root {
  color-scheme: light dark;
  --bg: #fbfaf7; --fg: #1f2328; --muted: #646d76; --panel: #ffffff; --panel-2: #f3f1ec;
  --border: #d9d5cc; --accent: #4d7a1f; --code-bg: #f1efe9;
  --added: #1f883d; --added-bg: #dcf5e1; --removed: #c62828; --removed-bg: #fde2e1;
  --changed: #b06d00; --changed-bg: #fff1c9; --uncovered: #d00000; --uncovered-bg: #ffd2d2;
  --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
  --sans: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #14171a; --fg: #e4e6e8; --muted: #9aa3ab; --panel: #1c2024; --panel-2: #23282d;
    --border: #353c43; --accent: #8fc24e; --code-bg: #23282d;
    --added: #56d364; --added-bg: #1d3a24; --removed: #ff7b72; --removed-bg: #47201f;
    --changed: #e3b341; --changed-bg: #3d3115; --uncovered: #ff5c5c; --uncovered-bg: #4d1717;
  }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.5 var(--sans); }
main { max-width: 980px; margin: 0 auto; padding: 16px; }
h1 { font-size: 1.5rem; margin: 0.5rem 0; }
h2 { font-size: 1.25rem; margin: 2rem 0 0.75rem; padding-bottom: 0.25rem; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.05rem; margin: 0; }
a { color: var(--accent); }
code, pre { font-family: var(--mono); font-size: 0.86rem; }
code { background: var(--code-bg); padding: 0 0.25em; border-radius: 4px; overflow-wrap: anywhere; }
pre { background: var(--code-bg); padding: 0.6rem 0.75rem; border-radius: 6px; overflow-x: auto; margin: 0.5rem 0; }
pre code { background: none; padding: 0; }
nav.toc { display: flex; flex-wrap: wrap; gap: 0.25rem 1rem; font-size: 0.9rem; }
.muted { color: var(--muted); }
.chip { display: inline-block; padding: 0 0.5em; border-radius: 999px; border: 1px solid var(--border); font-size: 0.8rem; white-space: nowrap; }
.chip.ok { color: var(--added); border-color: var(--added); }
.chip.bad { color: var(--removed); border-color: var(--removed); }
.chip.warn { color: var(--changed); border-color: var(--changed); }
.verdict { font-size: 1rem; font-weight: 600; }
.notice { background: var(--changed-bg); border-left: 4px solid var(--changed); padding: 0.4rem 0.75rem; border-radius: 4px; margin: 0.5rem 0; }
.notice.bad { background: var(--removed-bg); border-color: var(--removed); }
dl.facts { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 0.5rem; margin: 0.75rem 0; }
dl.facts div { background: var(--panel); border: 1px solid var(--border); border-radius: 6px; padding: 0.4rem 0.6rem; }
dl.facts dt { font-size: 0.75rem; color: var(--muted); }
dl.facts dd { margin: 0; font-weight: 600; overflow-wrap: anywhere; }
section.group { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 0.75rem; margin: 1rem 0; }
.group-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.25rem 0.6rem; }
.kind { font-family: var(--mono); font-size: 0.8rem; color: var(--accent); text-transform: uppercase; }
.risk { color: var(--changed); letter-spacing: 0.1em; }
.group p { margin: 0.35rem 0; }
details.subgroup { border-top: 1px solid var(--border); margin-top: 0.5rem; padding-top: 0.5rem; }
details.subgroup > summary { cursor: pointer; display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.25rem 0.5rem; }
details.subgroup > summary::before { content: "▸"; color: var(--muted); }
details.subgroup[open] > summary::before { content: "▾"; }
details.subgroup > summary::-webkit-details-marker { display: none; }
details.subgroup > summary { list-style: none; }
.role { font-weight: 600; }
.files { font-family: var(--mono); font-size: 0.8rem; color: var(--muted); overflow-wrap: anywhere; }
ul.claims { list-style: none; padding: 0; margin: 0.5rem 0 0; }
li.claim { border-left: 3px solid var(--border); padding: 0.25rem 0 0.25rem 0.6rem; margin: 0.5rem 0; }
li.claim.verified { border-color: var(--added); }
li.claim.pending { border-color: var(--changed); }
li.claim.refuted, li.claim.empty, li.claim.malformed { border-color: var(--removed); }
.claim-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.25rem 0.5rem; }
.claim-meta { font-size: 0.85rem; color: var(--muted); }
.icon { font-weight: 700; }
.verified .icon { color: var(--added); }
.pending .icon { color: var(--changed); }
.refuted .icon, .empty .icon, .malformed .icon { color: var(--removed); }
.hint { font-size: 0.88rem; }
ul.steps { margin: 0.35rem 0; padding-left: 1.2rem; font-size: 0.9rem; }
ul.steps li { margin: 0.15rem 0; }
.new-code > p { margin: 0.35rem 0 0; font-size: 0.85rem; color: var(--muted); }
pre.diff span { display: block; }
pre.diff .add { background: var(--added-bg); }
pre.diff .del { background: var(--removed-bg); }
ul.uncovered li { margin: 0.4rem 0; }
.uncovered-mark { color: var(--uncovered); font-weight: 700; }
.flow { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 0.75rem; margin: 1rem 0; }
.flow pre.mermaid { background: none; text-align: center; }
.flow pre.mermaid svg { max-width: 100%; height: auto; }
.source > summary { cursor: pointer; overflow-wrap: anywhere; }
@media (max-width: 600px) {
  body { font-size: 14px; }
  main { padding: 12px; }
  section.group, .flow { padding: 0.5rem; }
}
"#;

/// Renders the report as one self-contained HTML file. The file loads nothing from the network.
pub fn render_html(data: &UiData) -> String {
    let model = Model::new(data);
    let mut out = String::with_capacity(64 * 1024 + if model.flows.is_empty() { 0 } else { MERMAID_JS.len() });
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    let _ = writeln!(out, "<title>{}</title>", esc(&model.title()));
    let _ = writeln!(out, "<style>{CSS}</style>\n</head>\n<body>\n<main>");
    let _ = writeln!(out, "<h1>{}</h1>", esc(&model.title()));
    let _ = writeln!(out, "<p class=\"muted\">{}</p>", esc(text::AGENT_TEXT_NOTE));
    // An uncovered change is an internal error. The report shows the part only when one exists.
    let uncovered = if model.uncovered().is_empty() { "" } else { "<a href=\"#uncovered\">Uncovered changes</a>" };
    let _ = writeln!(
        out,
        "<nav class=\"toc\"><a href=\"#summary\">Summary</a><a href=\"#groups\">Groups</a>{uncovered}\
         <a href=\"#tree\">Change tree</a><a href=\"#flows\">Flow views</a><a href=\"#sources\">Sources</a></nav>"
    );
    html_summary(&mut out, &model);
    html_groups(&mut out, &model);
    html_uncovered(&mut out, &model);
    out.push_str("<h2 id=\"tree\">Change tree</h2>\n");
    let _ = writeln!(out, "<pre class=\"tree\">{}</pre>", esc_block(model.ctx.data.tree.render().trim_end()));
    html_flows(&mut out, &model);
    html_sources(&mut out, &model);
    out.push_str("</main>\n");
    if !model.flows.is_empty() {
        out.push_str("<script id=\"mermaid-lib\">\n");
        out.push_str(&script_safe(MERMAID_JS));
        out.push_str("\n</script>\n<script id=\"report-js\">\n");
        out.push_str(REPORT_JS);
        out.push_str("</script>\n");
    }
    out.push_str("</body>\n</html>\n");
    out
}

/// Renders the report as Markdown.
///
/// The default report is short (brief 5f, D2): the summary table, then the groups with each fact
/// one time, in the order of design 35.7. No compare result hides a fact or its diff. A body
/// with a fact of the category `semantics` has its flow view, one time.
///
/// The full report (`full`) has the same content as [`render_html`]: it adds the public API
/// changes, the change tree, the other flow views, and all sources.
pub fn render_markdown(data: &UiData, full: bool) -> String {
    let model = Model::new(data);
    let mut out = String::new();
    let _ = writeln!(out, "# {}\n", md(&model.title()));
    let _ = writeln!(out, "{}\n", md(text::AGENT_TEXT_NOTE));
    md_summary(&mut out, &model, full);
    md_groups(&mut out, &model, full);
    md_uncovered(&mut out, &model);
    if full {
        out.push_str("## Change tree\n\n");
        out.push_str(&fence("text", model.ctx.data.tree.render().trim_end(), ""));
        out.push('\n');
    }
    md_flows(&mut out, &model, full);
    md_control_flow(&mut out, &model);
    md_expansions(&mut out, &model);
    md_reached(&mut out, &model);
    if full {
        md_sources(&mut out, &model);
    } else {
        out.push_str("The change tree, the other flow views, and all sources: `semilla review --md FILE --full`.\n");
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The model: the data in the review order.

/// One uncovered change, with the group of its step.
struct UncoveredItem<'a> {
    group: Option<String>,
    uncovered: &'a Uncovered,
}

struct Model<'a> {
    data: &'a UiData,
    ctx: Ctx,
    summary: ChangeSetSummary,
    /// The indexes into `data.flows`, in the review order.
    flows: Vec<usize>,
    /// The anchors in `data.sources` with a changed text, in the review order. A source that
    /// contains a smaller changed part of the same item is not in the list.
    sources: Vec<String>,
}

impl<'a> Model<'a> {
    fn new(data: &'a UiData) -> Model<'a> {
        let ctx = Ctx::new(data.clone());
        let summary = summary::changeset(&ctx, &ReviewFile::default());
        let views: Vec<&ClaimView> =
            summary.groups.iter().flat_map(|group| group.subgroups.iter()).flat_map(|sub| sub.claims.iter()).collect();

        // The flow views: first in the order of the claims that use them, then the others.
        let mut flows: Vec<usize> = Vec::new();
        for view in &views {
            if let Some(index) = view.flow.as_deref().and_then(|function| data.flows.iter().position(|flow| flow.function == function))
                && !flows.contains(&index)
            {
                flows.push(index);
            }
        }
        for index in 0..data.flows.len() {
            if !flows.contains(&index) {
                flows.push(index);
            }
        }

        // The sources: first in the order of the claims, then the others. Only changed texts.
        let mut sources: Vec<String> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let keys = views.iter().filter_map(|view| view.at.as_deref()).filter_map(|at| ctx.source(at).map(|(key, _)| key.to_string()));
        for key in keys.chain(data.sources.keys().cloned()) {
            if seen.insert(key.clone()) && data.sources.get(&key).is_some_and(|pair| pair.before != pair.after) {
                sources.push(key);
            }
        }
        // Only the smallest changed parts: a source goes out when a changed part of the same
        // item is inside it (for example `f/S0/cond` is inside `f`).
        let changed = sources.clone();
        sources.retain(|key| !changed.iter().any(|other| other != key && item_of(other) == item_of(key) && contains(key, other)));
        Model { data, ctx, summary, flows, sources }
    }

    fn report(&self) -> &Report {
        &self.data.report
    }

    fn title(&self) -> String {
        "Semilla review report".to_string()
    }

    /// The uncovered changes in the group order, then the changes of steps without a group.
    fn uncovered(&self) -> Vec<UncoveredItem<'a>> {
        let report = &self.data.report;
        let mut out = Vec::new();
        let mut placed: BTreeSet<usize> = BTreeSet::new();
        for group in &self.summary.groups {
            let Some(steps) = self.ctx.group(&group.id).map(|group| group.steps.clone()) else {
                continue;
            };
            for step in report.steps.iter().filter(|step| steps.contains(&step.step)) {
                if !placed.insert(step.step) {
                    continue;
                }
                out.extend(step.uncovered.iter().map(|uncovered| UncoveredItem { group: Some(group.id.clone()), uncovered }));
            }
        }
        for step in report.steps.iter().filter(|step| !placed.contains(&step.step)) {
            out.extend(step.uncovered.iter().map(|uncovered| UncoveredItem { group: None, uncovered }));
        }
        out
    }

    /// The spotlight items of all steps, as `(step, text)`.
    fn spotlight(&self) -> Vec<(usize, &str)> {
        self.report().steps.iter().flat_map(|step| step.spotlight.iter().map(move |text| (step.step, text.as_str()))).collect()
    }

    /// The public API changes of all steps, as `(step, text)`.
    fn api_changes(&self) -> Vec<(usize, &str)> {
        self.report().steps.iter().flat_map(|step| step.api_changes.iter().map(move |text| (step.step, text.as_str()))).collect()
    }

    /// A notice for a phase 1 report.
    fn phase_notice(&self) -> Option<&'static str> {
        (self.report().phase == "syntax").then_some("Phase 1 only: each compare result is pending.")
    }

    fn phase_label(&self) -> String {
        match self.report().phase.as_str() {
            "" => "unknown".to_string(),
            "syntax" => "1 (syntax)".to_string(),
            // Design 50: a change set with no compiled step has no phase 2.
            "full" if self.report().steps.iter().any(|step| !step.facts.is_empty()) && self.report().steps.iter().flat_map(|step| &step.facts).all(|fact| fact.compare.is_off()) => "syntax tree (no compiled check)".to_string(),
            "full" => "2 (full)".to_string(),
            other => other.to_string(),
        }
    }

    /// The facts of the summary as `(label, value)`.
    fn facts(&self) -> Vec<(&'static str, String)> {
        let summary = &self.summary;
        let report = self.report();
        let api = if report.api_level.is_some() { summary.api_level.to_string() } else { format!("{} (estimate)", summary.api_level) };
        let mut facts = vec![
            ("Result", (if summary.not_complete { "not complete" } else if summary.accepted { "accepted" } else { "rejected" }).to_string()),
            ("Facts", summary.fact_counts.text.clone()),
            ("Phase", self.phase_label()),
            ("Groups", summary.groups.len().to_string()),
            ("Steps", summary.step_count.to_string()),
            ("Claims", summary.counts.claims.to_string()),
            ("API level", api),
            ("Panic delta", signed(summary.panic_delta)),
        ];
        if !self.uncovered().is_empty() {
            facts.push(("Uncovered changes (an internal error)", self.uncovered().len().to_string()));
        }
        if report.pending > 0 {
            facts.push(("Pending claims", report.pending.to_string()));
        }
        if let Some(lines) = report.lines_text() {
            facts.push(("Changed lines", lines.trim_start_matches("lines: ").to_string()));
        }
        facts.push(("Base tree", short_hash(&summary.base_tree)));
        facts.push(("Head tree", short_hash(&summary.head_tree)));
        facts
    }

    /// Design 54.5: the count of the facts of the agent (if the change set has one), and the
    /// count of the items that are not stated (if the change set has one).
    fn agent_lines(&self) -> Vec<String> {
        let count: usize = self.report().steps.iter().map(|step| step.agent_facts.len()).sum();
        let facts = (count > 0).then(|| format!("facts of the agent: {count}, each one checked against the edit"));
        facts.into_iter().chain(self.report().gap_line()).chain(self.report().comment_count_line()).collect()
    }

    fn flow_index(&self, function: &str) -> Option<usize> {
        self.flows.iter().position(|index| self.data.flows[*index].function == function)
    }

    /// The source of a claim anchor: the same anchor, else the first changed part inside it,
    /// else the source of its item.
    fn source_index(&self, at: &str) -> Option<usize> {
        self.sources.iter().position(|source| source == at).or_else(|| self.sources.iter().position(|source| contains(at, source))).or_else(
            || {
                let (key, _) = self.ctx.source(at)?;
                self.sources.iter().position(|source| source == key)
            },
        )
    }

    /// The steps of a group, with their intents and claims.
    fn steps(&self, group: &GroupSummary) -> Vec<StepLine> {
        group
            .steps
            .iter()
            .map(|index| {
                let step = self.data.changeset.steps.iter().find(|step| step.index == *index);
                let report = self.data.report.steps.iter().find(|report| report.step == *index);
                StepLine {
                    index: *index,
                    intent: step
                        .map(|step| step.intent.clone())
                        .filter(|intent| !intent.is_empty())
                        .or_else(|| report.map(|report| report.intent.clone()))
                        .unwrap_or_default(),
                    claims: step.map(|step| step.claims.iter().map(|claim| claim.id.clone()).collect()).unwrap_or_default(),
                    errors: report.map(|report| report.errors.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// The code that a claim adds: the source of a new item (`add-item`, `extract-function`),
    /// or the code at the anchor after an `inline-function`.
    fn new_code(&self, view: &ClaimView) -> Option<NewCode> {
        let at = view.at.as_deref()?;
        let item = item_of(at);
        let (label, key) = match view.op.as_str() {
            "add-item" => ("new item", item.to_string()),
            "extract-function" => ("new function", full_path(item, view.args.get("new")?.as_str()?)),
            "inline-function" => ("code after the inline", at.to_string()),
            _ => return None,
        };
        // A later `move-item` can move the new item. Then its source is at the new place.
        let moved = self.ctx.all_claims().find_map(|(_, claim)| {
            (claim.op == "move-item" && claim.at.as_ref().is_some_and(|at| at.path.is_empty() && at.item == key))
                .then(|| claim.arg_str("to").map(|to| full_path(&key, to)))
                .flatten()
        });
        let found = |key: &str| match self.data.sources.get_key_value(key) {
            Some((key, pair)) => Some((key.as_str(), pair)),
            None => self.ctx.source(key),
        };
        let (key, pair) = found(&key).or_else(|| moved.as_deref().and_then(found))?;
        let text = pair.after.as_deref().filter(|text| !text.trim().is_empty())?;
        Some(NewCode { label, key: key.to_string(), text: text.to_string() })
    }

    fn flow(&self, position: usize) -> &FlowGraph {
        &self.data.flows[self.flows[position]]
    }

    /// The positions of the flow views of the short report: each body with a fact of the
    /// category `semantics`, one time. The compare result does not
    /// count (design 35.7).
    fn fact_flows(&self) -> Vec<usize> {
        let bodies: BTreeSet<&str> = self
            .summary
            .facts
            .iter()
            .filter(|view| view.fact.category == Category::Semantics)
            .filter_map(|view| view.fact.at.as_ref().map(|at| at.item.as_str()))
            .collect();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        (0..self.flows.len()).filter(|position| {
            let item = item_of(&self.flow(*position).function);
            bodies.contains(item) && seen.insert(item)
        }).collect()
    }

    /// The diff of a fact for the short report, or `None`.
    ///
    /// - No compare result hides a diff (design 35.7).
    /// - A derived rename or move has no diff: its text gives the change in full.
    /// - A new item, a removed item, and a part of a body show the source of the step.
    /// - A change of an item that stays (its docs, its signature) shows the content of the fact:
    ///   the source of the full item has too many lines.
    /// - A change of one line that the description gives in full has no diff.
    fn fact_diff(&self, view: &crate::summary::FactView) -> Option<String> {
        let fact = &view.fact;
        if fact.part == "identity" {
            return None;
        }
        let pair = fact.at.as_ref().and_then(|at| self.data.fact_sources.get(&format!("{}:{at}", view.step))).filter(|pair| pair.before != pair.after);
        let part = fact.at.as_ref().is_some_and(|at| !at.path.is_empty());
        let lines = |text: &Option<String>| text.as_deref().map_or(0, |text| text.lines().count());
        match (pair, &fact.content) {
            (Some(pair), content) if part || pair.before.is_none() || pair.after.is_none() => {
                let short = lines(&pair.before) <= 1 && lines(&pair.after) <= 1 && content.is_none();
                // A diff block is never empty (batch 6a, A2): the content of the fact is
                // the block if the diff of the sources has no line.
                (!short).then(|| diff_text(&dedent(pair.before.as_deref().unwrap_or("")), &dedent(pair.after.as_deref().unwrap_or("")))).filter(|diff| !diff.trim().is_empty()).or_else(|| content.clone())
            }
            (_, Some(content)) => Some(content.clone()),
            _ => None,
        }
    }

    fn source(&self, key: &str) -> &SourcePair {
        &self.data.sources[key]
    }
}

/// The status of a claim as text parts: the name and the detail (the proof or the reason).
/// It reads the status through serde, so a new status also shows as text.
struct StatusText {
    name: String,
    detail: Option<String>,
    /// The mark of changed build code for a verified claim (design 28, I6), or an empty text.
    mark: String,
}

impl StatusText {
    fn of(view: &ClaimView) -> StatusText {
        let Some(result) = &view.result else {
            return StatusText { name: "no result".to_string(), detail: None, mark: String::new() };
        };
        let value = serde_json::to_value(&result.status).unwrap_or_default();
        let name = value.get("status").and_then(|name| name.as_str()).unwrap_or(view.status).to_string();
        let detail = match value.get("proof").and_then(|proof| proof.as_str()) {
            Some(proof) => Some(format!("proof {proof}")),
            None => {
                let parts: Vec<String> = value
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(key, _)| key.as_str() != "status")
                    .filter_map(|(_, value)| value.as_str().map(str::to_string))
                    .collect();
                (!parts.is_empty()).then(|| parts.join("; "))
            }
        };
        StatusText { name, detail, mark: result.mark() }
    }

    /// The CSS class: the status name, or `other` for an unknown name.
    fn css(&self) -> &str {
        match self.name.as_str() {
            "verified" | "refuted" | "empty" | "malformed" | "pending" => &self.name,
            _ => "other",
        }
    }

    fn icon(&self) -> &'static str {
        match self.name.as_str() {
            "verified" => "✓",
            "refuted" => "✗",
            "empty" => "∅",
            "malformed" => "!",
            "pending" => "…",
            "no result" => "·",
            _ => "•",
        }
    }

    /// For example `verified (proof N1)` or `pending: waits for the MIR`.
    fn text(&self) -> String {
        match (&self.detail, self.name.as_str()) {
            (None, _) => format!("{}{}", self.name, self.mark),
            (Some(detail), "verified") => format!("verified ({detail}){}", self.mark),
            (Some(detail), _) => format!("{}: {detail}", self.name),
        }
    }
}

/// One step of a group.
struct StepLine {
    index: usize,
    intent: String,
    claims: Vec<String>,
    errors: Vec<String>,
}

/// The code that a claim adds, with its source key.
struct NewCode {
    label: &'static str,
    key: String,
    text: String,
}

impl NewCode {
    /// False if the claim diff already shows the same text.
    fn adds_to(&self, view: &ClaimView) -> bool {
        view.result.as_ref().and_then(|result| result.after.as_deref()).is_none_or(|after| after.trim() != self.text.trim())
    }
}

/// The most lines of a new-code block.
const NEW_CODE_LINES: usize = 40;

/// The text with at most `NEW_CODE_LINES` lines.
fn short_block(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= NEW_CODE_LINES {
        return text.trim_end().to_string();
    }
    format!("{}\n… {} more lines", lines[..NEW_CODE_LINES].join("\n"), lines.len() - NEW_CODE_LINES)
}

/// The full path of a name next to an item, as the verifier resolves it.
fn full_path(item: &str, name: &str) -> String {
    if name.contains("::") {
        return name.to_string();
    }
    match item.rsplit_once("::") {
        Some((parent, _)) => format!("{parent}::{name}"),
        None => name.to_string(),
    }
}

/// `old → new` for a rename, from the texts of the claim result. Only single-line texts.
fn rename_text(view: &ClaimView) -> Option<(String, String)> {
    if !matches!(view.op.as_str(), "rename" | "rename-local") {
        return None;
    }
    let result = view.result.as_ref()?;
    let (before, after) = (result.before.as_deref()?.trim(), result.after.as_deref()?.trim());
    (!before.contains('\n') && !after.contains('\n')).then(|| (before.to_string(), after.to_string()))
}

/// The steps of a group for its title (C11), for example `steps 4–5` or `step 2`: the reports sort
/// the groups by risk, so the title gives the order of the work.
fn steps_label(steps: &[usize]) -> String {
    let mut sorted = steps.to_vec();
    sorted.sort_unstable();
    match sorted.as_slice() {
        [] => "no steps".to_string(),
        [one] => format!("step {one}"),
        [first, .., last] if last - first + 1 == sorted.len() => format!("steps {first}–{last}"),
        all => format!("steps {}", all.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")),
    }
}

/// The source and the target of a move claim (C12).
fn move_text(view: &ClaimView) -> Option<(String, String)> {
    if view.op != "move-item" {
        return None;
    }
    Some((view.at.clone()?, view.args.get("to")?.as_str()?.to_string()))
}

/// The result word of the change set. A step with no check gives `not complete` (design 28):
/// the first notice gives the steps and the cause.
fn result_word(summary: &crate::summary::ChangeSetSummary) -> &'static str {
    if summary.accepted {
        "accepted"
    } else if summary.not_complete {
        "not complete"
    } else {
        "rejected"
    }
}

/// The result line of the summary (design 35.6), with the same parts as the result line of
/// `semilla tag`: the result word, the steps, the facts by compare result, the claims.
fn result_line(model: &Model) -> String {
    let summary = &model.summary;
    let steps = if summary.step_count == 1 { "1 step".to_string() } else { format!("{} steps", summary.step_count) };
    format!("result: {} · {steps} · {} · {}", result_word(summary), summary.fact_counts.text, counts_text(&summary.counts))
}

/// True if a notice of the report has the line of the scope of the compare results (it comes
/// with the list of the checked configurations).
fn config_scope_shown(report: &Report) -> bool {
    report.notices.iter().any(|notice| notice.contains(semilla_core::report::CONFIG_SCOPE))
}

/// The counts as text, for example `3 claims: 2 verified, 1 refuted`.
fn counts_text(counts: &Counts) -> String {
    // I6: a count of proofs says how many used changed build code.
    let (verified, partial) = (format!("verified{}", counts.marked_text), format!("partial{}", counts.partial_marked_text));
    let parts: Vec<String> = [
        (counts.verified, verified.as_str()),
        (counts.partial, partial.as_str()),
        (counts.refuted, "refuted"),
        (counts.empty, "empty"),
        (counts.malformed, "malformed"),
        (counts.pending, "pending"),
        (counts.not_checked, "not checked"),
        (counts.no_result, "no result"),
    ]
    .iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{count} {label}"))
    .collect();
    let claims = if counts.claims == 1 { "1 claim".to_string() } else { format!("{} claims", counts.claims) };
    if parts.is_empty() { claims } else { format!("{claims}: {}", parts.join(", ")) }
}

fn risk_dots(risk: u8) -> String {
    let risk = usize::from(risk.min(3));
    format!("{}{}", "●".repeat(risk), "○".repeat(3 - risk))
}

fn signed(value: i64) -> String {
    match value {
        0 => "0".to_string(),
        value if value > 0 => format!("+{value}"),
        value => format!("−{}", value.unsigned_abs()),
    }
}

fn short_hash(hash: &str) -> String {
    if hash.is_empty() { "—".to_string() } else { hash.chars().take(12).collect() }
}

/// The serde name of an enum value, for example `ctrl_flow` or `api`.
fn serde_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_default()
}

/// The Mermaid text of a flow view. The class colors are light, so the label text is dark in
/// both themes (as in the UI).
fn mermaid_text(flow: &FlowGraph) -> String {
    let text = flow.to_mermaid();
    // A class that no node uses needs no definition.
    let used = |line: &str| line.trim_start().strip_prefix("classDef ").and_then(|rest| rest.split(' ').next()).is_none_or(|class| text.contains(&format!(":::{class}")));
    text.lines()
        .filter(|line| used(line))
        .map(|line| if line.trim_start().starts_with("classDef ") { format!("{line},color:#1a1a1a") } else { line.to_string() })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text without the indent that all its lines have.
fn dedent(text: &str) -> String {
    let indent = text.lines().filter(|line| !line.trim().is_empty()).map(|line| line.len() - line.trim_start().len()).min().unwrap_or(0);
    text.lines().map(|line| line.get(indent..).unwrap_or(line.trim_start())).collect::<Vec<_>>().join("\n")
}

/// The breadcrumb of a flow view, or `None` if it only repeats the heading: the heading of a
/// body shows one time (trial 4, D9).
fn breadcrumb_text(flow: &FlowGraph) -> Option<String> {
    (!flow.breadcrumb.is_empty() && flow.breadcrumb.join("::") != flow.function).then(|| flow.breadcrumb.join(" › "))
}

/// The parts of a group line: the steps, the counts, the risk reasons, and the dependencies.
fn group_meta(group: &GroupSummary) -> Vec<String> {
    let mut meta = Vec::new();
    if !group.steps.is_empty() {
        let steps: Vec<String> = group.steps.iter().map(usize::to_string).collect();
        meta.push(format!("steps {}", steps.join(", ")));
    }
    meta.push(counts_text(&group.counts));
    if group.counts.uncovered > 0 {
        meta.push(format!("{} uncovered", group.counts.uncovered));
    }
    if group.panic_delta != 0 {
        meta.push(format!("panics {}", signed(group.panic_delta)));
    }
    if !group.depends_on.is_empty() {
        meta.push(format!("uses {}", group.depends_on.join(", ")));
    }
    if group.all_equal {
        meta.push(format!("{}{}", crate::summary::ALL_EQUAL, group.mark));
    }
    meta
}

fn risk_text(group: &GroupSummary) -> String {
    if group.risk_reasons.is_empty() {
        format!("risk {} ({}/3)", risk_dots(group.risk), group.risk)
    } else {
        format!("risk {} ({}/3): {}", risk_dots(group.risk), group.risk, group.risk_reasons.join(", "))
    }
}

fn subgroup_marks(sub: &SubgroupSummary) -> Vec<String> {
    let mut marks = Vec::new();
    if sub.edited {
        marks.push("edited by agent".to_string());
    }
    if !sub.new_claims.is_empty() {
        marks.push(format!("new claims: {}", sub.new_claims.join(", ")));
    }
    marks.extend(sub.badges.iter().cloned());
    marks
}

/// The claim details as `(label, value)`: the category, the step, the effect, the witness, the file.
fn claim_meta(view: &ClaimView) -> Vec<(&'static str, String)> {
    let category = if view.property { "property".to_string() } else { category_label(view.category).to_string() };
    let mut meta = vec![("category", category)];
    if let Some(step) = view.step {
        meta.push(("step", step.to_string()));
    }
    if let Some(result) = &view.result {
        if let Some(effect) = &result.effect {
            meta.push(("effect hint", serde_name(effect)));
        }
        if let Some(witness) = &result.witness {
            meta.push(("witness", witness.clone()));
        }
    }
    if let Some(file) = &view.file {
        meta.push(("file", file.clone()));
    }
    meta
}

// ---------------------------------------------------------------------------------------------
// The line diff.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Same,
    Del,
    Add,
}

/// A simple line diff (longest common subsequence). Large inputs fall back to "all removed,
/// then all added" for the middle part.
fn line_diff<'t>(before: &'t str, after: &'t str) -> Vec<(Line, &'t str)> {
    let a: Vec<&str> = if before.is_empty() { Vec::new() } else { before.lines().collect() };
    let b: Vec<&str> = if after.is_empty() { Vec::new() } else { after.lines().collect() };
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (mid_a, mid_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut out: Vec<(Line, &str)> = a[..prefix].iter().map(|line| (Line::Same, *line)).collect();
    let (n, m) = (mid_a.len(), mid_b.len());
    if n * m > 4_000_000 {
        out.extend(mid_a.iter().map(|line| (Line::Del, *line)));
        out.extend(mid_b.iter().map(|line| (Line::Add, *line)));
    } else {
        // lcs[i][j] = the length of the LCS of mid_a[i..] and mid_b[j..].
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if mid_a[i] == mid_b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && mid_a[i] == mid_b[j] {
                out.push((Line::Same, mid_a[i]));
                i += 1;
                j += 1;
            } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
                // At a tie, the removed line comes before the added line.
                out.push((Line::Del, mid_a[i]));
                i += 1;
            } else {
                out.push((Line::Add, mid_b[j]));
                j += 1;
            }
        }
    }
    out.extend(a[a.len() - suffix..].iter().map(|line| (Line::Same, *line)));
    out
}

/// The diff as text lines with ` `, `-`, and `+` marks.
fn diff_text(before: &str, after: &str) -> String {
    let mut out = String::new();
    for (kind, line) in line_diff(before, after) {
        out.push(match kind {
            Line::Same => ' ',
            Line::Del => '-',
            Line::Add => '+',
        });
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// The source pair as a diff, with headers as in a unified diff.
fn source_diff(key: &str, pair: &SourcePair) -> String {
    let before = if pair.before.is_some() { format!("--- before: {key}") } else { "--- (does not exist before)".to_string() };
    let after = if pair.after.is_some() { format!("+++ after: {key}") } else { "+++ (does not exist after)".to_string() };
    let body = diff_text(pair.before.as_deref().unwrap_or(""), pair.after.as_deref().unwrap_or(""));
    format!("{before}\n{after}\n{body}")
}

// ---------------------------------------------------------------------------------------------
// HTML.

/// Escapes inline text for HTML element content and attribute values. Each hidden character
/// (a control character, a bidirectional control, a line break) shows as an escape (5k, A2).
fn esc(text: &str) -> String {
    html_escape(&text::inline(text))
}

/// Escapes the text of a block (a `pre` element): the line breaks stay.
fn esc_block(text: &str) -> String {
    html_escape(&text::block(text))
}

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Makes a script safe to inline: a `</script` in the text would end the element early.
fn script_safe(script: &str) -> std::borrow::Cow<'_, str> {
    let lower = script.to_ascii_lowercase();
    if !lower.contains("</script") {
        return std::borrow::Cow::Borrowed(script);
    }
    let mut out = String::with_capacity(script.len() + 16);
    let mut last = 0;
    for (index, _) in lower.match_indices("</script") {
        out.push_str(&script[last..index]);
        out.push_str("<\\/");
        last = index + 2;
    }
    out.push_str(&script[last..]);
    std::borrow::Cow::Owned(out)
}

/// The line under the title "Spotlight" (batch 5w, D4). The section and the summary count
/// different things, so the line gives the two counts and says what each one is.
fn spotlight_counts(lines: usize, facts: usize) -> String {
    let (line_noun, fact_noun) = (if lines == 1 { "line names" } else { "lines name" }, if facts == 1 { "fact" } else { "facts" });
    format!("{lines} {line_noun} sensitive code that a step changed. The summary counts a different thing: {facts} {fact_noun} with a spotlight. Each such fact shows its spotlight and its reason in its group.")
}

fn html_list(out: &mut String, class: &str, items: &[String]) {
    let _ = writeln!(out, "<ul class=\"{class}\">");
    for item in items {
        let _ = writeln!(out, "<li>{}</li>", esc(item));
    }
    out.push_str("</ul>\n");
}

fn html_diff(out: &mut String, before: &str, after: &str, header: Option<(&str, &str)>) {
    out.push_str("<pre class=\"diff\">");
    if let Some((old, new)) = header {
        let _ = write!(out, "<span class=\"muted\">{}\n</span><span class=\"muted\">{}\n</span>", esc(old), esc(new));
    }
    for (kind, line) in line_diff(before, after) {
        let (class, mark) = match kind {
            Line::Same => ("ctx", ' '),
            Line::Del => ("del", '-'),
            Line::Add => ("add", '+'),
        };
        // Each line ends with a line break, so a copy of the text keeps the lines.
        let _ = write!(out, "<span class=\"{class}\">{mark} {}\n</span>", esc(line));
    }
    out.push_str("</pre>\n");
}

fn html_summary(out: &mut String, model: &Model) {
    let summary = &model.summary;
    let report = model.report();
    for notice in &report.notices {
        let _ = writeln!(out, "<p class=\"notice\">{}</p>", esc(notice));
    }
    out.push_str("<h2 id=\"summary\">Summary</h2>\n");
    let verdict = result_word(summary);
    let class = match verdict {
        "accepted" => "ok",
        "not complete" => "warn",
        _ => "bad",
    };
    let line = result_line(model);
    let _ = writeln!(out, "<p class=\"verdict\">result: <span class=\"chip {class}\">{verdict}</span>{}</p>", esc(line.trim_start_matches(&format!("result: {verdict}"))));
    for line in model.agent_lines() {
        let _ = writeln!(out, "<p class=\"note\">{}</p>", esc(&line));
    }
    if let Some(notice) = model.phase_notice() {
        let _ = writeln!(out, "<p class=\"notice\">{}</p>", esc(notice));
    }
    if !config_scope_shown(report) {
        let _ = writeln!(out, "<p class=\"note\">{}</p>", esc(semilla_core::report::CONFIG_SCOPE));
    }
    if !summary.fact_counts.category.is_empty() {
        out.push_str("<table class=\"categories\">\n<tr><th>Category</th><th>Facts</th></tr>\n");
        for row in &summary.fact_counts.category {
            let _ = writeln!(out, "<tr><td>{}</td><td>{}</td></tr>", esc(row.name), row.count);
        }
        out.push_str("</table>\n");
    }
    out.push_str("<dl class=\"facts\">\n");
    for (label, value) in model.facts() {
        let _ = writeln!(out, "<div><dt>{}</dt><dd>{}</dd></div>", esc(label), esc(&value));
    }
    out.push_str("</dl>\n");
    if !report.organization.is_empty() {
        out.push_str("<h3>Organization errors</h3>\n");
        html_list(out, "organization", &report.organization);
    }
    if !report.warnings.is_empty() {
        out.push_str("<h3>Warnings</h3>\n");
        html_list(out, "warnings", &report.warnings);
    }
    let facts = fact_items_of(model, None);
    if !facts.is_empty() {
        out.push_str("<h3>Derived facts</h3>\n");
        html_fact_list(out, &facts);
    }
    if !report.pairs.is_empty() {
        out.push_str("<h3>Pair warnings</h3>\n");
        let items: Vec<String> = report.pairs.iter().map(|pair| pair.text()).collect();
        html_list(out, "pairs", &items);
    }
    let spotlight = model.spotlight();
    if !spotlight.is_empty() {
        out.push_str("<h3>Spotlight</h3>\n");
        let _ = writeln!(out, "<p class=\"note\">{}</p>", esc(&spotlight_counts(spotlight.len(), model.summary.fact_counts.spotlight)));
        let items: Vec<String> = spotlight.iter().map(|(step, text)| format!("step {step}: {text}")).collect();
        html_list(out, "spotlight", &items);
    }
    let api_changes = model.api_changes();
    if !api_changes.is_empty() {
        out.push_str("<h3>Public API changes</h3>\n");
        let items: Vec<String> = api_changes.iter().map(|(step, text)| format!("step {step}: {text}")).collect();
        html_list(out, "api-changes", &items);
    }
}

/// The title or the `why` of a group: the words of the agent, in quote marks (batch 5k, A3).
/// A group that the report makes (the steps with no group) has text of Semilla.
fn agent_text(group: &GroupSummary, words: &str) -> String {
    if group.synthetic { words.to_string() } else { text::quoted(words) }
}

fn html_groups(out: &mut String, model: &Model) {
    out.push_str("<h2 id=\"groups\">Groups</h2>\n");
    if model.summary.groups.is_empty() {
        out.push_str("<p class=\"muted\">No groups.</p>\n");
    }
    for group in &model.summary.groups {
        let _ = writeln!(out, "<section class=\"group\" id=\"group-{}\" data-group=\"{}\">", esc(&group.id), esc(&group.id));
        let _ = writeln!(
            out,
            "<div class=\"group-head\"><span class=\"kind\">{}</span><code>{}</code> <span class=\"muted\">{}</span><h3>{}</h3><span class=\"risk\" title=\"{}\">{}</span></div>",
            esc(&format!("{}{}", serde_name(&group.kind), group.kind_mark)),
            esc(&group.id),
            esc(&steps_label(&group.steps)),
            esc(&agent_text(group, &group.title)),
            esc(&risk_text(group)),
            risk_dots(group.risk),
        );
        if !group.why.is_empty() {
            let _ = writeln!(out, "<p>{}</p>", esc(&agent_text(group, &group.why)));
        }
        let _ = writeln!(out, "<p class=\"muted\">{} · {}</p>", esc(&risk_text(group)), esc(&group_meta(group).join(" · ")));
        // Design 28.3: the build code changes of the group come first, each with its diff.
        let code = fact_items(model, Some(&group.steps), true, Some(true));
        if !code.is_empty() {
            let _ = writeln!(out, "<h4 class=\"build-code\">{}</h4>", esc(&build_code_title(code.len())));
            html_fact_list(out, &code);
        }
        html_steps(out, model, group);
        html_facts(out, model, &group.steps);
        // Design 54.7: the note of the child mark, one time in the report.
        if out.contains(text::CHILD_MARK) && !out.contains(text::CHILD_NOTE) {
            let _ = writeln!(out, "<p class=\"muted\">Note: {}</p>", esc(text::CHILD_NOTE));
        }
        if let Some(hint) = &group.split_hint {
            let _ = writeln!(out, "<p class=\"notice\">{}</p>", esc(hint));
        }
        if group.counts.uncovered > 0 {
            let _ = writeln!(
                out,
                "<p class=\"notice bad\"><a href=\"#uncovered\">{} uncovered change(s)</a> in this group.</p>",
                group.counts.uncovered
            );
        }
        for sub in &group.subgroups {
            html_subgroup(out, model, sub);
        }
        out.push_str("</section>\n");
    }
}

fn html_steps(out: &mut String, model: &Model, group: &GroupSummary) {
    let steps = model.steps(group);
    if steps.is_empty() {
        return;
    }
    out.push_str("<ul class=\"steps\">\n");
    for step in &steps {
        let _ = write!(out, "<li data-step=\"{}\">step {}: {}", step.index, step.index, esc(&text::quoted(&step.intent)));
        if !step.claims.is_empty() {
            let links: Vec<String> = step.claims.iter().map(|id| format!("<a href=\"#claim-{0}\"><code>{0}</code></a>", esc(id))).collect();
            let _ = write!(out, " — {}", links.join(", "));
        }
        for error in &step.errors {
            let _ = write!(out, "<div class=\"notice bad\">error: {}</div>", esc(error));
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n");
}

fn html_subgroup(out: &mut String, model: &Model, sub: &SubgroupSummary) {
    let open = if sub.folded { "" } else { " open" };
    let folded = if sub.folded { " folded" } else { "" };
    let _ = writeln!(out, "<details class=\"subgroup{folded}\" data-subgroup=\"{}\"{open}>", esc(&sub.id));
    out.push_str("<summary>");
    let _ = write!(out, "<span class=\"role\">{}</span> <span>{}</span>", esc(sub.role_label), esc(&sub.title));
    for mark in subgroup_marks(sub) {
        let class = if mark == "edited by agent" || mark.starts_with("new claims") { "chip warn" } else { "chip" };
        let _ = write!(out, " <span class=\"{class}\">{}</span>", esc(&mark));
    }
    let _ = write!(out, " <span class=\"muted\">{}</span>", esc(&counts_text(&sub.counts)));
    if sub.folded {
        let _ = write!(out, " <span class=\"chip ok\">folded: {}</span>", esc(&sub.fold_reason));
    }
    out.push_str("</summary>\n");
    if !sub.locations.is_empty() {
        let _ = writeln!(out, "<p class=\"files\">{}</p>", esc(&sub.locations.join(" · ")));
    }
    if !sub.why.is_empty() {
        let _ = writeln!(out, "<p>{}</p>", esc(&sub.why));
    }
    if let Some(mismatch) = &sub.label_mismatch {
        let _ = writeln!(out, "<p class=\"notice\">{}</p>", esc(mismatch));
    }
    out.push_str("<ul class=\"claims\">\n");
    for view in &sub.claims {
        html_claim(out, model, view);
    }
    out.push_str("</ul>\n</details>\n");
}

fn html_claim(out: &mut String, model: &Model, view: &ClaimView) {
    let status = StatusText::of(view);
    let _ = writeln!(out, "<li class=\"claim {}\" id=\"claim-{}\" data-claim=\"{}\">", esc(status.css()), esc(&view.id), esc(&view.id));
    let _ = write!(
        out,
        "<div class=\"claim-head\"><span class=\"icon\" title=\"{}\">{}</span><code>{}</code><code>{}</code>",
        esc(&status.name),
        status.icon(),
        esc(&view.id),
        esc(&view.op)
    );
    if let Some(at) = &view.at {
        let _ = write!(out, "<code>{}</code>", esc(at));
    }
    if let Some((_, to)) = move_text(view) {
        let _ = write!(out, " → <code>{}</code>", esc(&to));
    }
    if view.new_in_subgroup {
        out.push_str("<span class=\"chip warn\">new</span>");
    }
    out.push_str("</div>\n");
    if !view.summary.is_empty() {
        let _ = writeln!(out, "<div>{}</div>", esc(&view.summary));
    }
    let mut meta: Vec<String> = vec![esc(&status.text())];
    meta.extend(claim_meta(view).iter().map(|(label, value)| format!("{label}: {}", esc(value))));
    if let Some(index) = view.flow.as_deref().and_then(|function| model.flow_index(function)) {
        meta.push(format!("<a href=\"#flow-{index}\">flow view</a>"));
    }
    if let Some(index) = view.at.as_deref().and_then(|at| model.source_index(at)) {
        meta.push(format!("<a href=\"#source-{index}\">source</a>"));
    }
    let _ = writeln!(out, "<div class=\"claim-meta\">{}</div>", meta.join(" · "));
    if let Some(hint) = view.result.as_ref().and_then(|result| result.hint.as_ref()) {
        let _ = writeln!(out, "<p class=\"hint\">Hint: {}</p>", esc(hint));
    }
    if let Some(checked) = view.result.as_ref().and_then(|result| result.after.as_deref()).filter(|_| view.property) {
        let _ = writeln!(out, "<p class=\"checked\">Checked: {}</p>", esc(checked));
    } else if let Some((before, after)) = rename_text(view) {
        let _ = writeln!(out, "<div class=\"rename\"><code>{}</code> → <code>{}</code></div>", esc(&before), esc(&after));
    } else if let Some(result) = &view.result
        && (result.before.is_some() || result.after.is_some())
    {
        html_diff(out, result.before.as_deref().unwrap_or(""), result.after.as_deref().unwrap_or(""), None);
    }
    if let Some(new) = model.new_code(view).filter(|new| new.adds_to(view)) {
        let _ = writeln!(
            out,
            "<div class=\"new-code\"><p>{} <code>{}</code></p><pre><code>{}</code></pre></div>",
            esc(new.label),
            esc(&new.key),
            esc_block(&short_block(&new.text))
        );
    }
    out.push_str("</li>\n");
}

fn html_uncovered(out: &mut String, model: &Model) {
    let items = model.uncovered();
    if items.is_empty() {
        return;
    }
    out.push_str("<h2 id=\"uncovered\">Uncovered changes</h2>\n");
    let _ = writeln!(out, "<p class=\"notice bad\">An internal error: {} change(s) have no fact. Semilla rejects the change set.</p>", items.len());
    out.push_str("<ul class=\"uncovered\">\n");
    for item in &items {
        let uncovered = item.uncovered;
        out.push_str("<li class=\"uncovered-change\"><span class=\"uncovered-mark\">✗</span> ");
        let _ = write!(out, "{}", esc(&uncovered.description));
        let mut meta = vec![format!("step {}", uncovered.step), format!("view {}", serde_name(&uncovered.view))];
        if let Some(group) = &item.group {
            meta.push(format!("group {group}"));
        }
        let _ = write!(out, "<div class=\"claim-meta\">{}", esc(&meta.join(" · ")));
        if let Some(at) = &uncovered.at {
            let _ = write!(out, " · at <code>{}</code>", esc(&at.to_string()));
        }
        if let Some(file) = &uncovered.file {
            let _ = write!(out, " · file <code>{}</code>", esc(file));
        }
        out.push_str("</div>");
        if let Some(hint) = &uncovered.hint {
            let _ = write!(out, "<p class=\"hint\">Hint: {}</p>", esc(hint));
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n");
}

fn html_flows(out: &mut String, model: &Model) {
    out.push_str("<h2 id=\"flows\">Flow views</h2>\n");
    if model.flows.is_empty() {
        out.push_str("<p class=\"muted\">No flow views.</p>\n");
    }
    for position in 0..model.flows.len() {
        let flow = model.flow(position);
        let _ = writeln!(out, "<div class=\"flow\" id=\"flow-{position}\" data-flow=\"{}\">", esc(&flow.function));
        let _ = writeln!(out, "<h3><code>{}</code></h3>", esc(&flow.function));
        if let Some(breadcrumb) = breadcrumb_text(flow) {
            let _ = writeln!(out, "<p class=\"muted\">{}</p>", esc(&breadcrumb));
        }
        let text = mermaid_text(flow);
        // Mermaid reads the text of the element and decodes the HTML entities.
        let _ = writeln!(out, "<pre class=\"mermaid\">{}</pre>", esc_block(&text));
        let _ = writeln!(out, "<details><summary class=\"muted\">Mermaid text</summary><pre>{}</pre></details>", esc_block(&text));
        out.push_str("</div>\n");
    }
}

fn html_sources(out: &mut String, model: &Model) {
    out.push_str("<h2 id=\"sources\">Sources</h2>\n");
    if model.sources.is_empty() {
        out.push_str("<p class=\"muted\">No changed sources.</p>\n");
    }
    for (index, key) in model.sources.iter().enumerate() {
        let pair = model.source(key);
        let _ = writeln!(out, "<details class=\"source\" id=\"source-{index}\" data-source=\"{}\" open>", esc(key));
        let _ = writeln!(out, "<summary><code>{}</code></summary>", esc(key));
        let old = if pair.before.is_some() { format!("--- before: {key}") } else { "--- (does not exist before)".to_string() };
        let new = if pair.after.is_some() { format!("+++ after: {key}") } else { "+++ (does not exist after)".to_string() };
        html_diff(out, pair.before.as_deref().unwrap_or(""), pair.after.as_deref().unwrap_or(""), Some((&old, &new)));
        out.push_str("</details>\n");
    }
}

// ---------------------------------------------------------------------------------------------
// Markdown.

/// Like [`md`], but text between backticks becomes a code span. The verifier messages use
/// backticks for code.
fn md_with_code(text: &str) -> String {
    let parts: Vec<&str> = text.split('`').collect();
    if parts.len() % 2 == 0 {
        // The backticks are not in pairs: escape all of them.
        return md(text);
    }
    // A code part has no backtick and no line break: it cannot close its span.
    let last = parts.len() - 1;
    parts.iter().enumerate().map(|(index, part)| if index % 2 == 1 { format!("`{}`", text::inline(part)) } else { md_part(part, index == 0, index == last) }).collect()
}

/// Escapes inline Markdown text (5k, A2). Each hidden character shows as an escape, also a
/// line break. The text cannot start or close a block: a heading, a quote, a fence, a table,
/// a list, or HTML.
fn md(text: &str) -> String {
    md_part(text, true, true)
}

/// A part of an inline text. `first`: the part can be at the start of a line, so a mark that
/// starts a list, a heading line, or a code block there gets an escape.
fn md_part(text: &str, first: bool, last: bool) -> String {
    let text = text::inline(text);
    // White space at the start of a line can start a code block.
    let text = if first { text.trim_start_matches(' ') } else { text.as_str() };
    let mut out = String::with_capacity(text.len());
    // The digits at the start: `1.` and `1)` start a list.
    let digits = if first { text.chars().take_while(char::is_ascii_digit).count() } else { 0 };
    for (index, c) in text.chars().enumerate() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '#' | '|' | '~' | '!' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '-' | '+' | '=' if first && index == 0 => {
                out.push('\\');
                out.push(c);
            }
            '.' | ')' if digits > 0 && index == digits => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    // Two spaces at the end of a line are a line break.
    if last {
        out.truncate(out.trim_end_matches(' ').len());
    }
    out
}

/// An inline code span. It uses a backtick run longer than any run in the text.
fn code(text: &str) -> String {
    let text = text::inline(text);
    let ticks = "`".repeat(longest_run(&text, '`') + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') || text.starts_with(' ') { " " } else { "" };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

/// A fenced code block. Each line has the indent, so the block can be part of a list item.
fn fence(lang: &str, body: &str, indent: &str) -> String {
    let body = &text::block(body);
    let ticks = "`".repeat(longest_run(body, '`').max(2) + 1);
    let mut out = format!("{indent}{ticks}{lang}\n");
    for line in body.lines() {
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
    let _ = writeln!(out, "{indent}{ticks}");
    out
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        run = if ch == c { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

/// One line for each derived fact (design 22.5, 35): the description, the category and the
/// compare result, the forced edits,
/// the affected bodies, the spotlight with its reason, and the content change of a file fact.
fn fact_items_of(model: &Model, steps: Option<&[usize]>) -> Vec<(String, Option<String>)> {
    fact_items(model, steps, true, None)
}

/// The title of the list of the build code changes of a group (design 28.3). The list comes
/// before the other facts of the group, and each item has its diff.
fn build_code_title(count: usize) -> String {
    format!("Build code changes ({count}): read these changes first")
}

/// The fact items of the full report (the content of each fact), or of the short report (the
/// diff of [`Model::fact_diff`]). The order is the order of design 35.7: the spotlight facts,
/// the facts with the mismatch mark, then the compare results. `code` selects the build code changes
/// (`Some(true)`), the other facts (`Some(false)`), or each fact (`None`). A build code change
/// has its diff in each report (design 28.3).
fn fact_items(model: &Model, steps: Option<&[usize]>, full: bool, code: Option<bool>) -> Vec<(String, Option<String>)> {
    fact_items_where(model, steps, full, code, &|_| true)
}

/// The place of a fact in the order of design 54.5: below the fact of the agent that states
/// it (`Ok`: the index in the step), or in a group of the facts that Semilla filled in (`Err`).
fn fact_place(model: &Model, view: &crate::summary::FactView) -> Result<usize, usize> {
    // [R92.8-1] An entry of the map of the diff is in its section of the one list (design 90,
    // R90.5-1): the fact of the agent names it.
    if let Some(section) = view.fact.section() {
        return Err(section);
    }
    let agent = model.report().steps.iter().find(|step| step.step == view.step).and_then(|step| step.agent_facts.iter().position(|agent| agent.facts.contains(&view.fact.id)));
    agent.ok_or(view.fact.filled_group())
}

/// [R92.8-1] True if the steps have entries of the map of the diff (design 90): the review
/// shows the sections of the one list, as the step output does.
fn sectioned(reports: &[&semilla_core::report::StepReport]) -> bool {
    reports.iter().any(|step| step.facts.iter().any(|fact| fact.section().is_some()))
}

/// [R92.8-1] The groups of the list of the facts that Semilla gives: the sections of the one list
/// (design 90, 92), or the groups of a record of an earlier version.
fn fact_groups(reports: &[&semilla_core::report::StepReport]) -> (&'static [&'static str], &'static str) {
    if sectioned(reports) { (semilla_core::report::ENTRY_SECTIONS, "Entries of the map of the diff") } else { (semilla_core::report::FILLED_GROUPS, "Derived facts") }
}

/// True if the step of the fact has a tag (design 55.3): the fact shows in the group of its tag.
fn in_tagged_step(model: &Model, view: &crate::summary::FactView) -> bool {
    model.report().steps.iter().any(|step| step.step == view.step && !step.tags.is_empty())
}

/// The largest count of positions that a gap line of the Markdown review shows. A gap with
/// more positions has them in `semilla status --verbose`.
const GAP_POSITIONS_MAX: usize = 6;

/// The lines of the cover check of the steps that need the reader (design 55.4): the changed
/// lines with no fact.
fn no_fact_lines(reports: &[&semilla_core::report::StepReport]) -> Vec<String> {
    reports.iter().flat_map(|step| step.no_fact_lines().into_iter().map(move |line| format!("{} (step {})", line, step.step))).collect()
}

/// The fact items of [`fact_items`] that `keep` selects.
fn fact_items_where(model: &Model, steps: Option<&[usize]>, full: bool, code: Option<bool>, keep: &dyn Fn(&crate::summary::FactView) -> bool) -> Vec<(String, Option<String>)> {
    let mut views: Vec<&crate::summary::FactView> = model
        .summary
        .facts
        .iter()
        .filter(|view| steps.is_none_or(|steps| steps.contains(&view.step)))
        .filter(|view| code.is_none_or(|code| code == view.fact.build_code_change))
        .filter(|view| keep(view))
        .collect();
    // Design 38.9: the order of the text output (a stable sort: the order of the numbers stays
    // inside one place).
    views.sort_by_key(|view| view.fact.text_rank());
    let items: Vec<(usize, &str, String, Option<String>)> = views
        .into_iter()
        .map(|view| {
            let fact = &view.fact;
            // The origin of the fact (design 54.5): `derived` or `stated`.
            let origin = if fact.status.is_empty() { "derived" } else { fact.status.as_str() };
            // [R90.5-1] The head of an entry of the map of the diff (design 90).
            let origin = fact.entry_head().unwrap_or_else(|| origin.to_string());
            // [R92.5-1] The line "not stated" of an entry, with the statement that states it.
            let open = model.report().steps.iter().find(|step| step.step == view.step).and_then(|step| step.not_stated_line(fact)).map(|line| format!(" · {line}")).unwrap_or_default();
            // Design 54.7: a long text and a text with more than one line are in a block
            // below the fact line.
            // R71.1-1: a text over the budget shows the changed part, the sizes, and the command.
            let command = semilla_core::budget::full_text_command(&fact.id, view.step);
            let budget = text::Budget { command: Some(&command), keep: fact.category == semilla_core::Category::Docs };
            let (line, blocks) = text::fact_layout_with(&fact.description, Some(budget));
            let mut text = format!("step {}: {} {origin}: {line} [{}]{}", view.step, fact.id, fact.label(), fact.mark());
            for group in fact.sites.iter().filter(|group| !fact.names_sites(group)) {
                let anchors = group.sites.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
                // A folded fact has the count and the text in its description: only the anchors
                // follow (batch 5x, part E).
                if fact.folds(group) {
                    text.push_str(&format!(" (at {anchors})"));
                } else {
                    text.push_str(&format!("; {} site(s): {} ({anchors})", group.sites.len(), text::flat_text(&group.edit)));
                }
            }
            // The short report shows the count and the scope of a long list (batch 5x, part E).
            // The full report has each body (brief 5h, C6).
            let list = |bodies: &[String]| {
                if full || bodies.len() <= semilla_core::report::LIST_LIMIT {
                    format!(": {}", bodies.iter().map(|b| format!("`{b}`")).collect::<Vec<_>>().join(", "))
                } else {
                    format!(" of {} (the names: `--full`)", semilla_core::report::list_scope(bodies))
                }
            };
            // Design 35.2: the mismatch mark. The text of the change gives no cause for
            // different compiled code.
            if fact.mismatch() {
                text.push_str(&format!("; {}{}", semilla_core::report::MISMATCH.trim_end_matches(':'), if fact.affected.is_empty() { String::new() } else { list(&fact.affected) }));
            } else if !fact.affected.is_empty() {
                text.push_str(&format!("; {} affected bod{}{}", fact.affected.len(), if fact.affected.len() == 1 { "y" } else { "ies" }, list(&fact.affected)));
            }
            // Two or more edits in one body (design 36.5).
            text.push_str(&fact.other_edits_text());
            if !fact.equal_sites.is_empty() {
                let count = if full || fact.equal_sites.len() <= semilla_core::report::LIST_LIMIT { String::new() } else { format!("{} ", fact.equal_sites.len()) };
                text.push_str(&format!("; {count}{}{}{}", semilla_core::report::EQUAL_SITES, fact.part_mark(), list(&fact.equal_sites)));
            }
            // The branch mark (design 40.4). With the graph data of the step (design 60, C1),
            // the lines `control flow:` of the fact replace it, at the end of the item.
            let flow = model.report().steps.iter().find(|step| step.step == view.step).and_then(|step| step.flow.as_ref());
            if flow.is_none()
                && let Some(branches) = &fact.branches
            {
                text.push_str(&format!("; branches: {branches}"));
            }
            // A public API change (design 36.3). No line for a fact with no compare (design 50).
            if fact.api_change() && !fact.compare.is_off() {
                text.push_str(&format!("; {}", semilla_core::report::API_LINE));
            }
            // A changed body with `unsafe` code (design 36.5).
            if fact.unsafe_code {
                text.push_str(&format!("; {}", semilla_core::report::UNSAFE_LINE));
            }
            if let Some(note) = &fact.note {
                text.push_str(&format!("; note: {note}{}", fact.part_mark()));
            }
            // A stored reason of a fact with no spotlight is a note (batch 5x, part E).
            if let Some(reason) = fact.reason.as_deref().filter(|_| fact.spotlight_why.is_none()) {
                text.push_str(&format!("; note: no spotlight; the agent gave a reason: {}", text::quoted(reason)));
            }
            if let Some(why) = &fact.spotlight_why {
                text.push_str(&format!("; spotlight: {why}; {}: {}", fact.reason_label(), fact.reason.as_deref().map(text::quoted).unwrap_or_else(|| "MISSING".into())));
            }
            // [R98.8-1] The graph part of the entry (design 98), then the lines of its changes.
            if let Some(graph) = flow.and_then(|flow| flow.entries.get(&fact.id)) {
                text.push_str(&format!("; {}", graph.line()));
            }
            for line in flow.and_then(|flow| flow.facts.get(&fact.id)).into_iter().flatten() {
                text.push_str(&format!("; control flow: {line}"));
            }
            for line in flow.and_then(|flow| flow.equivalence.get(&fact.id)).into_iter().flatten() {
                text.push_str(&format!("; equivalence: {line}"));
            }
            // The changed expansions that the fact owns (design 63); the section "Macro
            // expansions" has the diff of each one.
            for line in flow.and_then(|flow| flow.expansion_lines.get(&fact.id)).into_iter().flatten() {
                text.push_str(&format!("; expansion: {line}"));
            }
            // The callers that the change reaches (design 61.10, A5); the section "Reached
            // callers" has the links.
            if let Some((line, _)) = flow.and_then(|flow| flow.reaches.get(&fact.id)) {
                text.push_str(&format!("; reaches: {line}"));
            }
            // [R92.5-1] The line "not stated" is the last part of the item.
            text.push_str(&open.replacen(" · ", "; ", 1));
            let diff = if full { fact.content.clone() } else { model.fact_diff(view).or_else(|| fact.content.clone().filter(|_| fact.build_code_change)) };
            // The blocks of the fact text have the full text of each side: no second diff.
            (view.step, fact.id.as_str(), text, if blocks.is_empty() { diff } else { Some(blocks.join("\n")) })
        })
        .collect();
    without_equal_diffs(items)
}

/// Batch 5y, C2. In one step, the second fact with an equal diff block does not show the
/// block again: its text names the first fact. The Markdown report and the HTML report use
/// this list.
fn without_equal_diffs(items: Vec<(usize, &str, String, Option<String>)>) -> Vec<(String, Option<String>)> {
    // The step, the id, the diff block, the text with no id, and the position in the result.
    let mut shown: Vec<(usize, &str, Option<String>, String, usize)> = Vec::new();
    let mut result: Vec<(String, Option<String>)> = Vec::new();
    for (step, id, text, content) in items {
        // The text of a fact with no id: two facts with the same text say the same thing.
        let plain = text.replacen(&format!(" {id} "), " ", 1);
        // Batch 7b, part 5.8, and design 38.9 (the merge of the text output): the same text
        // with the same diff block (or with no block) shows one time. The first fact names the
        // second fact.
        if let Some((_, _, _, _, position)) = shown.iter().find(|(known, _, block, text, _)| *known == step && *block == content && *text == plain) {
            result[*position].0.push_str(&format!("; also {id}"));
            continue;
        }
        let first = content.as_ref().and_then(|content| shown.iter().find(|(known, _, block, _, _)| *known == step && block.as_ref() == Some(content)));
        match first {
            Some((_, first, _, _, _)) => result.push((format!("{text}; the same diff as {first}"), None)),
            None => {
                shown.push((step, id, content.clone(), plain, result.len()));
                result.push((text, content));
            }
        }
    }
    result
}

/// The Markdown list of fact items: each content change is a fenced block in its list item.
fn md_fact_list(out: &mut String, items: &[(String, Option<String>)]) {
    md_fact_items(out, items, "");
    out.push('\n');
}

/// [R92.8-1] A name of a section with a capital first letter.
fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The list items of [`md_fact_list`] at an indent.
fn md_fact_items(out: &mut String, items: &[(String, Option<String>)], indent: &str) {
    for (text, content) in items {
        let _ = writeln!(out, "{indent}- {}", md_with_code(text));
        if let Some(content) = content {
            out.push('\n');
            out.push_str(&fence("diff", content, &format!("{indent}  ")));
        }
    }
}

/// The facts of the steps in the order of design 54.5 (48.3): the facts of the agent with
/// the check results, the places, and the facts that each one states; the items that are not
/// stated; then the facts that Semilla filled in, in groups. The text is Markdown.
fn md_facts(out: &mut String, model: &Model, steps: &[usize], full: bool) {
    let reports: Vec<&semilla_core::report::StepReport> = model.report().steps.iter().filter(|step| steps.contains(&step.step)).collect();
    let count: usize = reports.iter().map(|step| step.agent_facts.len()).sum();
    if count > 0 {
        let _ = writeln!(out, "#### Facts of the agent ({count})\n");
        let _ = writeln!(out, "{}.\n", md(semilla_core::report::AGENT_FACTS_NOTE));
        for step in &reports {
            for (index, agent) in step.agent_facts.iter().enumerate() {
                let _ = writeln!(out, "- step {}: {} {}", step.step, if agent.holds { "✓" } else { "✗" }, md_with_code(&agent.line()));
                for line in agent.lines() {
                    let _ = writeln!(out, "  - {}", md_with_code(&line));
                }
                // Design 55.3: in a step with tags, each fact shows in the group of its tag.
                if !step.tags.is_empty() {
                    if !agent.facts.is_empty() {
                        let _ = writeln!(out, "  - facts, in the groups of the tags below: {}", agent.facts.join(", "));
                    }
                    continue;
                }
                // [R92.8-1] The entries are in the sections of the one list: the fact names them.
                if sectioned(&[*step]) {
                    if !agent.facts.is_empty() {
                        let _ = writeln!(out, "  - entries, in the list below: {}", agent.facts.join(", "));
                    }
                    continue;
                }
                let below = fact_items_where(model, Some(&[step.step]), full, Some(false), &|view| fact_place(model, view) == Ok(index));
                md_fact_items(out, &below, "  ");
            }
        }
        out.push('\n');
    }
    // [R92.5-1] The entries have the line "not stated": the separate list is only for a record
    // of an earlier version.
    let gaps: usize = reports.iter().filter(|step| !sectioned(&[**step])).map(|step| step.gaps.len()).sum();
    if gaps > 0 {
        let items: usize = reports.iter().filter(|step| !sectioned(&[**step])).map(|step| step.gap_items().len()).sum();
        let places: usize = reports.iter().filter(|step| !sectioned(&[**step])).flat_map(|step| &step.gaps).map(|gap| gap.places.len()).sum();
        let _ = writeln!(out, "#### Not stated ({items})\n");
        let _ = writeln!(out, "{}\n", md(&semilla_core::report::gap_count_line(items, places).unwrap_or_default()));
        for step in reports.iter().filter(|step| !sectioned(&[**step])) {
            for gap in &step.gaps {
                let tags = step.tags_of(&gap.facts).map(|tags| format!("; {}", md_with_code(&tags))).unwrap_or_default();
                // The lines of the edits (design 55.2), if the list is short.
                let positions = step.positions_of(&gap.facts);
                let lines = if positions.is_empty() || positions.len() > GAP_POSITIONS_MAX { String::new() } else { format!("; lines: {}", md(&semilla_core::Position::list(&positions))) };
                let _ = writeln!(out, "- step {}: {}; in: {}{lines}{tags}", step.step, md_with_code(&gap.line()), gap.items.iter().map(|item| code(item)).collect::<Vec<_>>().join(", "));
            }
        }
        out.push('\n');
    } else if let Some(line) = reports.iter().any(|step| sectioned(&[*step]) && !step.gaps.is_empty()).then(|| semilla_core::report::gap_count_line(reports.iter().map(|step| step.gap_items().len()).sum(), reports.iter().flat_map(|step| &step.gaps).map(|gap| gap.places.len()).sum())).flatten() {
        // [R92.5-1] The count line stays; each line of the list is below its entries.
        let _ = writeln!(out, "{}\n", md(&line));
    } else if count > 0 {
        // Design 54.14: a change set with a fact of the agent always has the count line.
        let _ = writeln!(out, "{}\n", md(semilla_core::report::NO_GAP_LINE));
    }
    // Design 55.3: the facts of a step with tags, in the groups of the tags, in the order of
    // the agent. Then the facts of the step that are in no tag. A tag is not a fact.
    let tags: usize = reports.iter().map(|step| step.tags.len()).sum();
    if tags > 0 {
        let _ = writeln!(out, "#### Tags of the agent ({tags})\n");
        let _ = writeln!(out, "A tag is a name of the agent for a part of a step. It is not a fact, and Semilla does not check the name.\n");
        for step in reports.iter().filter(|step| !step.tags.is_empty()) {
            for (index, tag) in step.tags.iter().enumerate() {
                let _ = writeln!(out, "Step {}, {}:\n", step.step, md_with_code(&step.tag_head(tag)));
                let group = semilla_core::report::TagGroup::Tag(index);
                let facts = fact_items_where(model, Some(&[step.step]), full, Some(false), &|view| step.tag_group(&view.fact) == group);
                md_fact_items(out, &facts, "");
                for (ids, place) in step.shown_in_other_tag(index).iter().zip(["above", "below"]).filter(|(ids, _)| !ids.is_empty()) {
                    let _ = writeln!(out, "- also in this tag, shown in a tag {place}: {}", ids.join(", "));
                }
                // Design 55.3, rule 5: a fact that is in the tag in part, with its other lines.
                for note in &tag.notes {
                    let _ = writeln!(out, "- {}", md_with_code(note));
                }
                out.push('\n');
            }
            let outside = step.not_in_a_tag();
            if !outside.is_empty() {
                let _ = writeln!(out, "Step {}, not in a tag: {}; {} changed line(s):\n", step.step, step.count_text(&outside), step.line_count(&outside));
                let facts = fact_items_where(model, Some(&[step.step]), full, Some(false), &|view| outside.contains(&view.fact.id));
                md_fact_list(out, &facts);
            }
            // Design 55.3, rule 6: a fact with no position is not a part of the count above.
            let unplaced = step.no_position();
            if !unplaced.is_empty() {
                let _ = writeln!(out, "Step {}, facts with no position (a tag selects changed lines; these facts have none): {}:\n", step.step, step.count_text(&unplaced));
                let facts = fact_items_where(model, Some(&[step.step]), full, Some(false), &|view| unplaced.contains(&view.fact.id));
                md_fact_list(out, &facts);
            }
        }
    }
    // Design 55.4: a changed line with no fact is a defect of Semilla. The review shows it.
    for line in no_fact_lines(&reports) {
        let _ = writeln!(out, "{}\n", md_with_code(&line));
    }
    let total = fact_items_where(model, Some(steps), full, Some(false), &|view| fact_place(model, view).is_err() && !in_tagged_step(model, view)).len();
    let (groups, title) = fact_groups(&reports);
    if total > 0 {
        let _ = writeln!(out, "#### {title} ({total})\n");
        // [R92.8-1] The coverage line of the map of each step (R90.5-3).
        for step in reports.iter().filter(|step| sectioned(&[**step]) && step.tags.is_empty()) {
            if let Some(map) = &step.ast {
                let _ = writeln!(out, "Step {}: {}\n", step.step, md(&semilla_core::report::map_coverage_line(map, &step.facts, step.flow.as_ref())));
            }
        }
    }
    // [R98.4-1] The graph changes of a body with no entry are lines of the section "compiled
    // code that changes with no edit" (design 98).
    let no_entry: Vec<(usize, &semilla_core::report::NoEntryGraph)> = reports.iter().filter(|step| step.tags.is_empty()).flat_map(|step| step.flow.iter().flat_map(|flow| flow.no_entry.iter()).map(|body| (step.step, body))).collect();
    for (group, name) in groups.iter().enumerate() {
        let facts = fact_items_where(model, Some(steps), full, Some(false), &|view| fact_place(model, view) == Err(group) && !in_tagged_step(model, view));
        let alone = sectioned(&reports) && *name == semilla_core::report::NO_EDIT_SECTION && !no_entry.is_empty();
        if facts.is_empty() && !alone {
            continue;
        }
        if sectioned(&reports) {
            let _ = writeln!(out, "{} ({}):\n", capitalized(name), facts.len());
        } else {
            let _ = writeln!(out, "Filled in by Semilla, {name} ({}):\n", facts.len());
        }
        md_fact_list(out, &facts);
        if alone {
            for (step, body) in &no_entry {
                let _ = writeln!(out, "- step {step}: {}", md_with_code(&body.line()));
            }
            out.push('\n');
        }
    }
    // Design 58.9: the comments of the changed items, each one with its first fact.
    let comments: Vec<(usize, &semilla_core::Fact, &semilla_core::RelatedComment)> = reports.iter().flat_map(|step| step.comment_list().into_iter().map(|(fact, comment)| (step.step, fact, comment))).collect();
    if !comments.is_empty() {
        let _ = writeln!(out, "#### Comments of the changed items ({})\n", comments.len());
        if let Some(line) = semilla_core::report::comment_count_line(reports.iter().map(|step| step.comments_no_edit()).sum()) {
            let _ = writeln!(out, "{}. {}\n", md(&line), md(semilla_core::report::COMMENT_NOTE));
        }
        for (step, fact, comment) in comments {
            // R71.1-1: a comment text over the budget shows its first lines and the command.
            let (head, body) = comment.head_and_text(Some(&semilla_core::budget::full_text_command(&fact.id, step)));
            let _ = writeln!(out, "- step {step}, with {}: {}", fact.id, md_with_code(&head));
            if comment.shows_text() && !body.is_empty() {
                out.push('\n');
                out.push_str(&fence("rust", &body, "  "));
            }
        }
        out.push('\n');
    }
    // Design 54.7: the note of the child mark, one time in the report.
    if out.contains(text::CHILD_MARK) && !out.contains(text::CHILD_NOTE) {
        let _ = writeln!(out, "Note: {}\n", text::CHILD_NOTE);
    }
}

/// The facts of the steps in the order of design 54.5, as HTML (see [`md_facts`]).
fn html_facts(out: &mut String, model: &Model, steps: &[usize]) {
    let reports: Vec<&semilla_core::report::StepReport> = model.report().steps.iter().filter(|step| steps.contains(&step.step)).collect();
    let count: usize = reports.iter().map(|step| step.agent_facts.len()).sum();
    if count > 0 {
        let _ = writeln!(out, "<h4>Facts of the agent ({count})</h4>\n<p class=\"muted\">{}.</p>\n<ul class=\"agent-facts\">", esc(semilla_core::report::AGENT_FACTS_NOTE));
        for step in &reports {
            for (index, agent) in step.agent_facts.iter().enumerate() {
                let _ = write!(out, "<li>step {}: {} {}", step.step, if agent.holds { "✓" } else { "✗" }, esc(&agent.line()));
                for line in agent.lines() {
                    let _ = write!(out, "<br>{}", esc(&line));
                }
                // [R92.8-1] The entries are in the sections of the one list: the fact names them.
                if sectioned(&[*step]) {
                    if !agent.facts.is_empty() {
                        let _ = write!(out, "<br>entries, in the list below: {}", esc(&agent.facts.join(", ")));
                    }
                    out.push_str("</li>\n");
                    continue;
                }
                let below = fact_items_where(model, Some(&[step.step]), true, Some(false), &|view| fact_place(model, view) == Ok(index));
                if !below.is_empty() {
                    html_fact_list(out, &below);
                }
                out.push_str("</li>\n");
            }
        }
        out.push_str("</ul>\n");
    }
    let gaps: usize = reports.iter().filter(|step| !sectioned(&[**step])).map(|step| step.gaps.len()).sum();
    if gaps > 0 {
        let items: usize = reports.iter().filter(|step| !sectioned(&[**step])).map(|step| step.gap_items().len()).sum();
        let places: usize = reports.iter().filter(|step| !sectioned(&[**step])).flat_map(|step| &step.gaps).map(|gap| gap.places.len()).sum();
        let _ = writeln!(out, "<h4>Not stated ({items})</h4>\n<p class=\"muted\">{}</p>\n<ul class=\"gaps\">", esc(&semilla_core::report::gap_count_line(items, places).unwrap_or_default()));
        for step in reports.iter().filter(|step| !sectioned(&[**step])) {
            for gap in &step.gaps {
                let _ = writeln!(out, "<li>step {}: {}; in: {}</li>", step.step, esc(&gap.line()), esc(&gap.items.join(", ")));
            }
        }
        out.push_str("</ul>\n");
    } else if let Some(line) = reports.iter().any(|step| sectioned(&[*step]) && !step.gaps.is_empty()).then(|| semilla_core::report::gap_count_line(reports.iter().map(|step| step.gap_items().len()).sum(), reports.iter().flat_map(|step| &step.gaps).map(|gap| gap.places.len()).sum())).flatten() {
        // [R92.5-1] The count line stays; each line of the list is below its entries.
        let _ = writeln!(out, "<p class=\"muted\">{}</p>", esc(&line));
    } else if count > 0 {
        let _ = writeln!(out, "<p class=\"muted\">{}</p>", esc(semilla_core::report::NO_GAP_LINE));
    }
    // Design 55.3, the smallest change of the local UI: one line for each tag, with its
    // counts and the numbers of its facts. The lists of the facts stay as they are.
    let tags: usize = reports.iter().map(|step| step.tags.len()).sum();
    if tags > 0 {
        let _ = writeln!(out, "<h4>Tags of the agent ({tags})</h4>\n<p class=\"muted\">A tag is a name of the agent for a part of a step. It is not a fact.</p>\n<ul class=\"tags\">");
        for step in &reports {
            for tag in &step.tags {
                let _ = writeln!(out, "<li>step {}: {}; facts: {}</li>", step.step, esc(&step.tag_head(tag)), esc(&tag.facts.iter().chain(&tag.partial).cloned().collect::<Vec<_>>().join(", ")));
            }
        }
        out.push_str("</ul>\n");
    }
    // Design 55.4: a changed line with no fact is a defect of Semilla.
    for line in no_fact_lines(&reports) {
        let _ = writeln!(out, "<p class=\"warn\">{}</p>", esc(&line));
    }
    let total = fact_items_where(model, Some(steps), true, Some(false), &|view| fact_place(model, view).is_err()).len();
    let (groups, title) = fact_groups(&reports);
    if total > 0 {
        let _ = writeln!(out, "<h4>{title} ({total})</h4>");
    }
    for (group, name) in groups.iter().enumerate() {
        let facts = fact_items_where(model, Some(steps), true, Some(false), &|view| fact_place(model, view) == Err(group));
        if facts.is_empty() {
            continue;
        }
        if sectioned(&reports) {
            let _ = writeln!(out, "<p class=\"muted\">{} ({}):</p>", esc(&capitalized(name)), facts.len());
        } else {
            let _ = writeln!(out, "<p class=\"muted\">Filled in by Semilla, {name} ({}):</p>", facts.len());
        }
        html_fact_list(out, &facts);
    }
    // Design 58.9: the comments of the changed items, each one with its first fact.
    let comments: Vec<(usize, &semilla_core::Fact, &semilla_core::RelatedComment)> = reports.iter().flat_map(|step| step.comment_list().into_iter().map(|(fact, comment)| (step.step, fact, comment))).collect();
    if comments.is_empty() {
        return;
    }
    let _ = writeln!(out, "<h4>Comments of the changed items ({})</h4>", comments.len());
    if let Some(line) = semilla_core::report::comment_count_line(reports.iter().map(|step| step.comments_no_edit()).sum()) {
        let _ = writeln!(out, "<p class=\"muted\">{}. {}</p>", esc(&line), esc(semilla_core::report::COMMENT_NOTE));
    }
    out.push_str("<ul class=\"derived-facts\">\n");
    for (step, fact, comment) in comments {
        let (head, body) = comment.head_and_text(Some(&semilla_core::budget::full_text_command(&fact.id, step)));
        let _ = write!(out, "<li>step {step}, with {}: {}", fact.id, esc(&head));
        if comment.shows_text() && !body.is_empty() {
            let _ = write!(out, "<pre class=\"diff\">{}</pre>", esc_block(&body));
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n");
}

/// The HTML list of fact items: each content change is a `pre` block in its list item.
fn html_fact_list(out: &mut String, items: &[(String, Option<String>)]) {
    out.push_str("<ul class=\"derived-facts\">\n");
    for (text, content) in items {
        let _ = write!(out, "<li>{}", esc(text));
        if let Some(content) = content {
            let _ = write!(out, "<pre class=\"diff\">{}</pre>", esc_block(content));
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ul>\n");
}

fn md_summary(out: &mut String, model: &Model, full: bool) {
    let summary = &model.summary;
    let report = model.report();
    // The base commit and its Semilla check come first (design 25.2).
    for notice in &report.notices {
        let _ = writeln!(out, "> {}\n", md_with_code(notice));
    }
    out.push_str("## Summary\n\n");
    let verdict = result_word(summary);
    let _ = writeln!(out, "{}\n", md(&result_line(model)).replacen(&format!("result: {verdict}"), &format!("result: **{verdict}**"), 1));
    for line in model.agent_lines() {
        let _ = writeln!(out, "{}\n", md(&line));
    }
    if let Some(notice) = model.phase_notice() {
        let _ = writeln!(out, "> **{}**\n", md(notice));
    }
    if !config_scope_shown(report) {
        let _ = writeln!(out, "{}\n", md(semilla_core::report::CONFIG_SCOPE));
    }
    if !summary.fact_counts.category.is_empty() {
        out.push_str("| Category | Facts |\n|---|---|\n");
        for row in &summary.fact_counts.category {
            let _ = writeln!(out, "| {} | {} |", md(row.name), row.count);
        }
        out.push('\n');
    }
    out.push_str("| Fact | Value |\n|---|---|\n");
    for (label, value) in model.facts() {
        let _ = writeln!(out, "| {} | {} |", md(label), md(&value));
    }
    out.push('\n');
    // Each fact shows one time, in its group (D2). The public API changes repeat facts: they
    // are in the full report only.
    let lists: [(&str, Vec<String>); 5] = [
        ("Organization errors", report.organization.clone()),
        ("Warnings", report.warnings.clone()),
        ("Pair warnings", report.pairs.iter().map(|pair| pair.text()).collect()),
        ("Spotlight", model.spotlight().iter().map(|(step, text)| format!("step {step}: {text}")).collect()),
        ("Public API changes", model.api_changes().iter().filter(|_| full).map(|(step, text)| format!("step {step}: {text}")).collect()),
    ];
    for (title, items) in lists {
        if items.is_empty() {
            continue;
        }
        let _ = writeln!(out, "### {title}\n");
        if title == "Spotlight" {
            let _ = writeln!(out, "{}\n", md(&spotlight_counts(items.len(), model.summary.fact_counts.spotlight)));
        }
        for item in items {
            let _ = writeln!(out, "- {}", md_with_code(&item));
        }
        out.push('\n');
    }
}

fn md_groups(out: &mut String, model: &Model, full: bool) {
    out.push_str("## Groups\n\n");
    if model.summary.groups.is_empty() {
        out.push_str("No groups.\n\n");
    }
    for group in &model.summary.groups {
        let _ = writeln!(out, "### {} · {} · {} · {}\n", code(&group.id), md(&steps_label(&group.steps)), md(&format!("{}{}", serde_name(&group.kind), group.kind_mark)), md(&agent_text(group, &group.title)));
        if !group.why.is_empty() {
            let _ = writeln!(out, "{}\n", md(&agent_text(group, &group.why)));
        }
        let _ = writeln!(out, "{} · {}\n", md(&risk_text(group)), md(&group_meta(group).join(" · ")));
        // The short report does not repeat the title of the group as the text of its one step.
        let steps = model.steps(group);
        let repeat = steps.len() == 1 && steps[0].intent == group.title && steps[0].errors.is_empty();
        // Design 28.3: the build code changes of the group come first, each with its diff.
        let code = fact_items(model, Some(&group.steps), full, Some(true));
        if !code.is_empty() {
            let _ = writeln!(out, "#### {}\n", build_code_title(code.len()));
            md_fact_list(out, &code);
        }
        if full || !repeat {
            md_steps(out, model, group);
        }
        // The facts of the steps of the group, as visible as its claims (design 54.5).
        md_facts(out, model, &group.steps, full);
        if let Some(hint) = &group.split_hint {
            let _ = writeln!(out, "> {}\n", md(hint));
        }
        if group.counts.uncovered > 0 {
            let _ = writeln!(
                out,
                "> **{} uncovered change(s)** in this group. See [Uncovered changes](#uncovered-changes).\n",
                group.counts.uncovered
            );
        }
        for sub in &group.subgroups {
            md_subgroup(out, model, sub);
        }
    }
    // A step with no group: its facts show here, so that each fact shows one time.
    let grouped: BTreeSet<usize> = model.summary.groups.iter().flat_map(|group| group.steps.iter().copied()).collect();
    let free: Vec<usize> = model.report().steps.iter().map(|step| step.step).filter(|step| !grouped.contains(step)).collect();
    let (code, facts) = (fact_items(model, Some(&free), full, Some(true)), fact_items(model, Some(&free), full, Some(false)));
    if !code.is_empty() || !facts.is_empty() || model.report().steps.iter().any(|step| free.contains(&step.step) && !step.agent_facts.is_empty()) {
        out.push_str("### Steps with no group\n\n");
    }
    if !code.is_empty() {
        let _ = writeln!(out, "#### {}\n", build_code_title(code.len()));
        md_fact_list(out, &code);
    }
    md_facts(out, model, &free, full);
}

fn md_steps(out: &mut String, model: &Model, group: &GroupSummary) {
    let steps = model.steps(group);
    if steps.is_empty() {
        return;
    }
    out.push_str("Steps:\n\n");
    for step in &steps {
        let mut line = format!("- step {}: {}", step.index, md(&text::quoted(&step.intent)));
        if !step.claims.is_empty() {
            let ids: Vec<String> = step.claims.iter().map(|id| code(id)).collect();
            let _ = write!(line, " — {}", ids.join(", "));
        }
        let _ = writeln!(out, "{line}");
        for error in &step.errors {
            let _ = writeln!(out, "  - error: {}", md_with_code(error));
        }
    }
    out.push('\n');
}

/// The code that a claim adds, as a list line and an indented code block.
fn md_new_code(out: &mut String, new: &NewCode, indent: &str) {
    let _ = writeln!(out, "{indent}- {}: {}\n", md(new.label), code(&new.key));
    out.push_str(&fence("rust", &short_block(&new.text), &format!("{indent}  ")));
}

fn md_subgroup(out: &mut String, model: &Model, sub: &SubgroupSummary) {
    let marks = subgroup_marks(sub);
    let marks = if marks.is_empty() { String::new() } else { format!(" · _{}_", md(&marks.join(" · "))) };
    if sub.folded {
        // A folded subgroup is one line with the counts. It has no claim (design 35.7).
        let ids: Vec<String> = sub.claims.iter().map(|view| code(&view.id)).collect();
        let ids = if ids.is_empty() { String::new() } else { format!(" — {}", ids.join(", ")) };
        let _ = writeln!(
            out,
            "- ▸ **{}** · {}{marks} · folded: {} ({}){ids}",
            md(sub.role_label),
            md(&sub.title),
            md(&counts_text(&sub.counts)),
            md(&sub.fold_reason)
        );
        out.push('\n');
        return;
    }
    let _ = writeln!(out, "#### ▾ {} · {}{marks}\n", md(sub.role_label), md(&sub.title));
    let mut lines = Vec::new();
    // A location is a file, or a module or a crate when the item has no file (a removed item).
    let is_file = |location: &&String| location.contains('/') || location.contains('.');
    let files: Vec<String> = sub.locations.iter().filter(is_file).map(|location| code(location)).collect();
    let modules: Vec<String> = sub.locations.iter().filter(|location| !is_file(location)).map(|location| code(location)).collect();
    if !files.is_empty() {
        lines.push(format!("Files: {}", files.join(" · ")));
    }
    if !modules.is_empty() {
        lines.push(format!("Modules: {}", modules.join(" · ")));
    }
    lines.push(md(&counts_text(&sub.counts)));
    if !sub.why.is_empty() {
        lines.push(md(&sub.why));
    }
    let _ = writeln!(out, "{}\n", lines.join("  \n"));
    if let Some(mismatch) = &sub.label_mismatch {
        let _ = writeln!(out, "> {}\n", md(mismatch));
    }
    for view in &sub.claims {
        md_claim(out, model, view);
    }
    out.push('\n');
}

fn md_claim(out: &mut String, model: &Model, view: &ClaimView) {
    let status = StatusText::of(view);
    let mut head = format!("- {} **{}** {}", status.icon(), md(&view.id), code(&view.op));
    if let Some(at) = &view.at {
        let _ = write!(head, " at {}", code(at));
    }
    if let Some((_, to)) = move_text(view) {
        let _ = write!(head, " → {}", code(&to));
    }
    if view.new_in_subgroup {
        head.push_str(" · _new_");
    }
    if !view.summary.is_empty() {
        let _ = write!(head, " — {}", code(&view.summary));
    }
    let _ = writeln!(out, "{head}");
    let mut meta = vec![md(&status.text())];
    for (label, value) in claim_meta(view) {
        meta.push(if label == "witness" || label == "file" {
            format!("{label}: {}", code(&value))
        } else {
            format!("{label}: {}", md(&value))
        });
    }
    if let Some(flow) = &view.flow {
        meta.push(format!("flow view: {}", code(flow)));
    }
    let _ = writeln!(out, "  - {}", meta.join(" · "));
    if let Some(hint) = view.result.as_ref().and_then(|result| result.hint.as_ref()) {
        let _ = writeln!(out, "  - Hint: {}", md(hint));
    }
    if let Some(checked) = view.result.as_ref().and_then(|result| result.after.as_deref()).filter(|_| view.property) {
        let _ = writeln!(out, "  - checked: {}", md(checked));
    } else if let Some((before, after)) = rename_text(view) {
        let _ = writeln!(out, "  - rename: {} → {}", code(&before), code(&after));
    } else if let Some(result) = &view.result
        && (result.before.is_some() || result.after.is_some())
    {
        let diff = diff_text(result.before.as_deref().unwrap_or(""), result.after.as_deref().unwrap_or(""));
        out.push('\n');
        out.push_str(&fence("diff", &diff, "  "));
    }
    if let Some(new) = model.new_code(view).filter(|new| new.adds_to(view)) {
        out.push('\n');
        md_new_code(out, &new, "  ");
    }
}

fn md_uncovered(out: &mut String, model: &Model) {
    let items = model.uncovered();
    if items.is_empty() {
        return;
    }
    out.push_str("## Uncovered changes\n\n");
    let _ = writeln!(out, "**An internal error: {} change(s) have no fact.** Semilla rejects the change set.\n", items.len());
    for item in &items {
        let uncovered = item.uncovered;
        let mut meta = vec![format!("step {}", uncovered.step), format!("view {}", md(&serde_name(&uncovered.view)))];
        if let Some(group) = &item.group {
            meta.push(format!("group {}", code(group)));
        }
        if let Some(at) = &uncovered.at {
            meta.push(format!("at {}", code(&at.to_string())));
        }
        if let Some(file) = &uncovered.file {
            meta.push(format!("file {}", code(file)));
        }
        let _ = writeln!(out, "- ✗ {}", code(&uncovered.description));
        let _ = writeln!(out, "  - {}", meta.join(" · "));
        if let Some(hint) = &uncovered.hint {
            let _ = writeln!(out, "  - Hint: {}", md(hint));
        }
    }
    out.push('\n');
}

fn md_flows(out: &mut String, model: &Model, full: bool) {
    let positions: Vec<usize> = if full { (0..model.flows.len()).collect() } else { model.fact_flows() };
    if positions.is_empty() && !full {
        return;
    }
    out.push_str("## Flow views\n\n");
    if positions.is_empty() {
        out.push_str("No flow views.\n\n");
    }
    for position in positions {
        let flow = model.flow(position);
        let _ = writeln!(out, "### {}\n", code(&flow.function));
        if let Some(breadcrumb) = breadcrumb_text(flow) {
            let _ = writeln!(out, "{}\n", md(&breadcrumb));
        }
        out.push_str(&fence("mermaid", &mermaid_text(flow), ""));
        out.push('\n');
    }
}

/// The control flow changes of the compiled check (design 60, C4): a section for each body
/// whose graph changes, with the diff list and a flowchart of the changed part. A link goes to
/// the line in the repository (relative to its root). A changed part with more than
/// [`semilla_core::report::FLOW_CHART_POINTS`] points has only the list.
fn md_control_flow(out: &mut String, model: &Model) {
    fn changed(line: &str) -> bool {
        line.starts_with("graph changes") || line.starts_with("graph added") || line.starts_with("graph removed")
    }
    let mut bodies: Vec<(usize, &semilla_core::report::FlowBody)> = Vec::new();
    for step in &model.report().steps {
        for body in step.flow.iter().flat_map(|flow| flow.bodies.iter()).filter(|body| changed(&body.line)) {
            bodies.push((step.step, body));
        }
    }
    if bodies.is_empty() {
        return;
    }
    out.push_str("## Control flow changes\n\n");
    for (step, body) in bodies {
        let _ = writeln!(out, "### {} (step {step})\n", code(&body.name));
        let _ = writeln!(out, "{}\n", md(&format!("{} ({})", body.line, body.place)));
        for (place, text) in &body.list {
            let _ = writeln!(out, "- {}: {}", line_link(place), md_with_code(text));
        }
        out.push('\n');
        match &body.chart {
            Some(chart) => {
                out.push_str(&fence("mermaid", &flow_chart(chart), ""));
                out.push('\n');
            }
            None if !body.list.is_empty() => {
                let _ = writeln!(out, "{}\n", md(&format!("The changed part has more than {} points: the list only.", semilla_core::report::FLOW_CHART_POINTS)));
            }
            None => {}
        }
    }
}

/// The most calls with a diff in the section "Macro expansions" (design 63, R63.5-3). The
/// other calls have one line each.
const EXPANSION_PARTS: usize = 20;

/// The section "Macro expansions" (design 63, R63.5-3): a part for each changed expansion with
/// the call (a link), the definition, and the diff of the expanded code. Calls with an equal
/// change fold into one part with the list of the calls. A long diff has a cut and points to
/// `semilla expand`.
fn md_expansions(out: &mut String, model: &Model) {
    let mut parts: Vec<(usize, &str, Vec<&semilla_core::report::ExpansionSite>)> = Vec::new();
    for step in &model.report().steps {
        for site in step.flow.iter().flat_map(|flow| flow.expansions.iter()) {
            match parts.iter_mut().find(|(known, key, list)| *known == step.step && *key == site.key && list[0].diff == site.diff) {
                Some((_, _, list)) => list.push(site),
                None => parts.push((step.step, site.key.as_str(), vec![site])),
            }
        }
    }
    if parts.is_empty() {
        return;
    }
    out.push_str("## Macro expansions\n\n");
    for (index, (step, _, list)) in parts.iter().enumerate() {
        let first = list[0];
        let calls: Vec<String> = list.iter().map(|site| format!("{} at {} ({})", md_with_code(&site.call), code(&site.at), line_link(&site.place))).collect();
        if index >= EXPANSION_PARTS {
            // R76.1-1: a part with no diff names the command that prints it.
            let _ = writeln!(out, "- step {step}: {}: {} ({})", calls.join(", "), md(&first.result), code(&semilla_core::budget::expansion_command(&first.place, *step)));
            continue;
        }
        let title = if list.len() == 1 { calls[0].clone() } else { format!("{} calls of {}", list.len(), md_with_code(&first.definition)) };
        let _ = writeln!(out, "### {title} (step {step})\n");
        if list.len() > 1 {
            for call in &calls {
                let _ = writeln!(out, "- {call}");
            }
            out.push('\n');
        }
        let _ = writeln!(out, "{}\n", md_with_code(&format!("{}; definition: {}", first.result, first.definition)));
        for fact in &first.facts {
            let _ = writeln!(out, "- {}", md_with_code(fact));
        }
        if !first.cause.is_empty() {
            let _ = writeln!(out, "- cause: {}", md_with_code(&first.cause));
        }
        out.push('\n');
        if !first.diff.is_empty() {
            out.push_str(&fence("diff", &first.diff.join("\n"), ""));
            out.push('\n');
        }
    }
    if parts.len() > EXPANSION_PARTS {
        let _ = writeln!(out, "{}\n", md(&format!("The parts after the first {EXPANSION_PARTS} have no diff: the command of each part prints it.")));
    }
}

/// The reached public functions of each fact (design 61.10, A5): a section with a list for
/// each fact whose body is in the start set of the walk, with a link to each function.
fn md_reached(out: &mut String, model: &Model) {
    let mut items: Vec<(usize, &String, &Vec<(String, String)>)> = Vec::new();
    for step in &model.report().steps {
        for (id, (_, links)) in step.flow.iter().flat_map(|flow| flow.reaches.iter()) {
            if !links.is_empty() {
                items.push((step.step, id, links));
            }
        }
    }
    if items.is_empty() {
        return;
    }
    out.push_str("## Reached callers

");
    for (step, id, links) in items {
        let list: Vec<String> = links.iter().map(|(name, place)| format!("{} ({})", code(name), line_link(place))).collect();
        let _ = writeln!(out, "- step {step}, {id}: {}", list.join(", "));
    }
    out.push('\n');
}

/// A link to a line of the repository: `src/lib.rs:12` → `[src/lib.rs:12](src/lib.rs#L12)`. A
/// line of the old tree (`src/lib.rs:-12`) has no link: the repository has the new tree.
fn line_link(place: &str) -> String {
    match place.rsplit_once(':') {
        Some((file, line)) if !line.starts_with('-') && line.bytes().all(|b| b.is_ascii_digit()) && !line.is_empty() => format!("[{}]({file}#L{line})", md(place)),
        _ => code(place),
    }
}

/// The Mermaid text of a flowchart (design 60, C4). An added element has the mark `+` and a
/// green class; a removed element has the mark `−`, a red class, and a dashed line.
fn flow_chart(chart: &semilla_core::report::FlowChart) -> String {
    let escape = |text: &str| text.replace('"', "#quot;").replace('<', "#lt;").replace('>', "#gt;").replace('|', "#124;").replace('`', "'");
    let mut lines = vec!["flowchart TD".to_string()];
    for node in &chart.nodes {
        let sign = match node.mark.as_str() {
            "added" => "+ ",
            "removed" => "− ",
            "changed" => "~ ",
            _ => "",
        };
        lines.push(format!("  {}[\"{sign}{}<br/>{}\"]:::{}", node.id, escape(&node.label), escape(&node.place), node.mark));
    }
    for edge in &chart.edges {
        let (arrow, sign) = match edge.mark.as_str() {
            "added" => ("==>", "+ "),
            "removed" => ("-.->", "− "),
            _ => ("-->", ""),
        };
        let label = format!("{sign}{}", edge.label);
        if label.is_empty() {
            lines.push(format!("  {} {arrow} {}", edge.from, edge.to));
        } else {
            lines.push(format!("  {} {arrow}|\"{}\"| {}", edge.from, escape(&label), edge.to));
        }
    }
    lines.push("  classDef added fill:#d8f5d8,stroke:#2e7d32,color:#1a1a1a".to_string());
    lines.push("  classDef removed fill:#f9dada,stroke:#c62828,stroke-dasharray: 5 5,color:#1a1a1a".to_string());
    lines.push("  classDef changed fill:#fff3c4,stroke:#b28704,color:#1a1a1a".to_string());
    lines.push("  classDef kept fill:#eeeeee,stroke:#757575,color:#1a1a1a".to_string());
    lines.join("\n")
}

fn md_sources(out: &mut String, model: &Model) {
    out.push_str("## Sources\n\n");
    if model.sources.is_empty() {
        out.push_str("No changed sources.\n");
    }
    for key in &model.sources {
        let _ = writeln!(out, "### {}\n", code(key));
        out.push_str(&fence("diff", &source_diff(key, model.source(key)), ""));
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_lines() {
        let diff = diff_text("a\nb\nc", "a\nx\nc\nd");
        assert_eq!(diff, "  a\n- b\n+ x\n  c\n+ d");
        assert_eq!(diff_text("", "new"), "+ new");
        assert_eq!(diff_text("old", ""), "- old");
    }

    #[test]
    fn markdown_escapes() {
        assert_eq!(md("a <b> *c*"), "a &lt;b&gt; \\*c\\*");
        assert_eq!(code("x`y"), "``x`y``");
        assert_eq!(code("`x"), "`` `x ``");
        assert!(fence("text", "```\nin", "").starts_with("````text\n"));
    }

    #[test]
    fn the_text_of_the_agent_cannot_start_a_block() {
        // Batch 5k, A2: inline text stays on one line and in its block.
        assert_eq!(md("a\nresult: accepted\u{1b}[8m"), "a\\\\nresult: accepted\\\\u{1b}\\[8m");
        for start in ["# title", "> quote", "- item", "+ item", "* item", "1. item", "2) item", "```", "~~~", "| a | b |", "<details>", "=== ", "    code", "\tcode", "---"] {
            let text = md(start);
            let first = text.chars().next().unwrap();
            assert!(first == '\\' || first == '&' || first.is_ascii_alphanumeric(), "{start:?} gives {text:?}");
            assert!(!text.contains('\n') && !text.contains('|') || text.contains("\\|"), "{start:?} gives {text:?}");
        }
        assert_eq!(md("1. item"), "1\\. item");
        assert_eq!(md("a | b"), "a \\| b");
        assert_eq!(md("\u{202e}x\u{200b}"), "\\\\u{202e}x\\\\u{200b}");
        // A code span and a code fence are longer than each run of backticks in the content. A
        // line break cannot end a code span. A fence keeps its line breaks.
        assert_eq!(code("a\n```\nb"), "````a\\n```\\nb````");
        let block = fence("diff", "+ a\u{1b}[8m\r\n````\n+ b\u{202e}", "");
        assert_eq!(block, "`````diff\n+ a\\u{1b}[8m\\r\n````\n+ b\\u{202e}\n`````\n");
        // The HTML report: the same escapes, then the HTML escape.
        assert_eq!(esc("a\n<b>\u{1b}"), "a\\n&lt;b&gt;\\u{1b}");
        assert_eq!(esc_block("a\n<b>\u{1b}"), "a\n&lt;b&gt;\\u{1b}");
    }

    // rule+ R92.8-1
    // rule- R92.8-1
    #[test]
    fn the_review_shows_the_sections_of_the_one_list() {
        use semilla_core::report::{EntryKind, Fact, StepReport};
        let entry = |id: &str, kind: Option<EntryKind>| Fact { id: id.into(), kind, nodes: vec![1], ..Default::default() };
        let step = StepReport { facts: vec![entry("f1", Some(EntryKind::Replace))], ..Default::default() };
        assert!(sectioned(&[&step]));
        assert_eq!(fact_groups(&[&step]), (semilla_core::report::ENTRY_SECTIONS, "Entries of the map of the diff"));
        assert_eq!(capitalized(semilla_core::report::NO_EDIT_SECTION), "Compiled code that changes with no edit");
        // Negative: a record of an earlier version (no kind) keeps the groups of before.
        let old = StepReport { facts: vec![entry("f1", None)], ..Default::default() };
        assert!(!sectioned(&[&old]));
        assert_eq!(fact_groups(&[&old]), (semilla_core::report::FILLED_GROUPS, "Derived facts"));
    }

    #[test]
    fn a_crate_is_not_a_file() {
        let is_file = |location: &str| location.contains('/') || location.contains('.');
        assert!(is_file("crates/cli/src/main.rs") && is_file("Cargo.toml") && !is_file("ledger_cli") && !is_file("ledger_cli::tests"));
    }

    #[test]
    fn the_second_equal_diff_of_a_step_names_the_first_fact() {
        // Batch 5y, C2: two facts of one step with the same diff block. An equal block in a
        // different step stays.
        let block = Some("- a\n+ b".to_string());
        let items = vec![(0, "f1", "one".to_string(), block.clone()), (0, "f2", "two".to_string(), block.clone()), (0, "f3", "three".to_string(), None), (1, "f1", "four".to_string(), block.clone())];
        let list = without_equal_diffs(items);
        assert_eq!(list, [("one".to_string(), block.clone()), ("two; the same diff as f1".to_string(), None), ("three".to_string(), None), ("four".to_string(), block.clone())]);
        // Batch 7b, part 5.8: two facts of one step with the same text and the same diff block
        // show the change one time.
        let items = vec![(0, "f1", "step 0: f1 derived: one".to_string(), block.clone()), (0, "f2", "step 0: f2 derived: one".to_string(), block.clone()), (1, "f1", "step 1: f1 derived: one".to_string(), block.clone())];
        assert_eq!(without_equal_diffs(items), [("step 0: f1 derived: one; also f2".to_string(), block.clone()), ("step 1: f1 derived: one".to_string(), block.clone())]);
        // Design 38.9: the merge of the text output. The same text with no block shows one
        // time. The same text with a different block shows two times.
        let other = Some("- a\n+ c".to_string());
        let items = vec![(0, "f1", "step 0: f1 derived: two".to_string(), None), (0, "f2", "step 0: f2 derived: two".to_string(), None), (0, "f3", "step 0: f3 derived: two".to_string(), block.clone()), (0, "f4", "step 0: f4 derived: two".to_string(), other.clone())];
        assert_eq!(without_equal_diffs(items), [("step 0: f1 derived: two; also f2".to_string(), None), ("step 0: f3 derived: two".to_string(), block), ("step 0: f4 derived: two".to_string(), other)]);
    }

    #[test]
    fn script_end_is_broken() {
        assert_eq!(script_safe("a</script>b</SCRIPT>"), "a<\\/script>b<\\/SCRIPT>");
        assert_eq!(script_safe("plain"), "plain");
    }
}
