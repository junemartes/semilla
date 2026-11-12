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
use semilla_core::{text, Class, Report, Uncovered};

use crate::ctx::{Ctx, class_label, contains, item_of};
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
li.claim.unproved, li.claim.pending { border-color: var(--changed); }
li.claim.refuted, li.claim.empty, li.claim.malformed { border-color: var(--removed); }
.claim-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.25rem 0.5rem; }
.claim-meta { font-size: 0.85rem; color: var(--muted); }
.icon { font-weight: 700; }
.verified .icon { color: var(--added); }
.unproved .icon, .pending .icon { color: var(--changed); }
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
/// one time. A fact that is not "neutral (proved)" has its source diff. A body with a fact of
/// the class `control_flow` or `logic` has its flow view, one time.
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
        (self.report().phase == "syntax").then_some("Phase 1 only: the body proofs are pending.")
    }

    fn phase_label(&self) -> String {
        match self.report().phase.as_str() {
            "" => "unknown".to_string(),
            "syntax" => "1 (syntax)".to_string(),
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
            ("Verdict", (if summary.not_complete { "not complete" } else if summary.accepted { "accepted" } else { "rejected" }).to_string()),
            ("Facts", {
                let facts = &summary.fact_counts;
                if report.phase == "syntax" {
                    format!("{}: {} with a spotlight; phase 2 gives the classes", facts.total, facts.spotlight)
                } else {
                    let not_checked = if facts.not_checked > 0 { format!(", {} not checked", facts.not_checked) } else { String::new() };
                    format!("{}: {} proved neutral, {} to read ({} with a spotlight){not_checked}", facts.total, facts.neutral, facts.read, facts.spotlight)
                }
            }),
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
                    declared: step.and_then(|step| step.class).or_else(|| report.and_then(|report| report.declared_class)),
                    computed: report.and_then(|report| report.computed_class),
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

    /// The positions of the flow views of the short report: each body with a fact of the class
    /// `control_flow` or `logic`, one time.
    fn fact_flows(&self) -> Vec<usize> {
        let bodies: BTreeSet<&str> = self
            .summary
            .facts
            .iter()
            .filter(|view| matches!(view.fact.class, Some(semilla_core::Class::ControlFlow | semilla_core::Class::Logic)))
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
    /// - A fact that is "neutral (proved)" has no diff.
    /// - A new item, a removed item, and a part of a body show the source of the step.
    /// - A change of an item that stays (its docs, its signature) shows the content of the fact:
    ///   the source of the full item has too many lines.
    /// - A change of one line that the description gives in full has no diff.
    fn fact_diff(&self, view: &crate::summary::FactView) -> Option<String> {
        let fact = &view.fact;
        if fact.class == Some(semilla_core::Class::Neutral) {
            return None;
        }
        let pair = fact.at.as_ref().and_then(|at| self.data.fact_sources.get(&format!("{}:{at}", view.step))).filter(|pair| pair.before != pair.after);
        let part = fact.at.as_ref().is_some_and(|at| !at.path.is_empty());
        let lines = |text: &Option<String>| text.as_deref().map_or(0, |text| text.lines().count());
        match (pair, &fact.content) {
            (Some(pair), content) if part || pair.before.is_none() || pair.after.is_none() => {
                let short = lines(&pair.before) <= 1 && lines(&pair.after) <= 1 && content.is_none() && !fact.description.contains('…');
                (!short).then(|| diff_text(&dedent(pair.before.as_deref().unwrap_or("")), &dedent(pair.after.as_deref().unwrap_or(""))))
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
}

impl StatusText {
    fn of(view: &ClaimView) -> StatusText {
        let Some(result) = &view.result else {
            return StatusText { name: "no result".to_string(), detail: None };
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
        StatusText { name, detail }
    }

    /// The CSS class: the status name, or `other` for an unknown name.
    fn css(&self) -> &str {
        match self.name.as_str() {
            "verified" | "unproved" | "refuted" | "empty" | "malformed" | "pending" => &self.name,
            _ => "other",
        }
    }

    fn icon(&self) -> &'static str {
        match self.name.as_str() {
            "verified" => "✓",
            "unproved" => "?",
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
            (None, _) => self.name.clone(),
            (Some(detail), "verified") => format!("verified ({detail})"),
            (Some(detail), _) => format!("{}: {detail}", self.name),
        }
    }
}

/// One step of a group.
struct StepLine {
    index: usize,
    intent: String,
    declared: Option<Class>,
    computed: Option<Class>,
    claims: Vec<String>,
    errors: Vec<String>,
}

impl StepLine {
    /// The classes, for example `declared logic · computed ctrl flow (mismatch)`.
    fn classes(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(declared) = self.declared {
            parts.push(format!("declared {}", class_label(Some(declared))));
        }
        if let Some(computed) = self.computed {
            let mismatch = if self.declared.is_some_and(|declared| declared != computed) { " (mismatch)" } else { "" };
            parts.push(format!("computed {}{mismatch}", class_label(Some(computed))));
        }
        parts
    }
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

/// The counts as text, for example `3 claims: 2 verified, 1 unproved`.
fn counts_text(counts: &Counts) -> String {
    let parts: Vec<String> = [
        (counts.verified, "verified"),
        (counts.partial, "partial"),
        (counts.unproved, "unproved"),
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
    if group.all_neutral {
        meta.push("all neutral".to_string());
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

/// The claim details as `(label, value)`: the class, the step, the effect, the witness, the file.
fn claim_meta(view: &ClaimView) -> Vec<(&'static str, String)> {
    let class = if view.property { "property".to_string() } else { class_label(view.class).to_string() };
    let mut meta = vec![("class", class)];
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
    let (class, verdict) = if summary.accepted {
        ("ok", "Accepted")
    } else if summary.not_complete {
        // A step has no check (design 28). The first notice gives the steps and the cause.
        ("warn", "Not complete")
    } else {
        ("bad", "Rejected")
    };
    let _ = writeln!(out, "<p class=\"verdict\"><span class=\"chip {class}\">{verdict}</span> {}</p>", esc(&counts_text(&summary.counts)));
    if let Some(notice) = model.phase_notice() {
        let _ = writeln!(out, "<p class=\"notice\">{}</p>", esc(notice));
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
            esc(&serde_name(&group.kind)),
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
        html_steps(out, model, group);
        let facts = fact_items_of(model, Some(&group.steps));
        if !facts.is_empty() {
            let _ = writeln!(out, "<h4>Derived facts ({})</h4>", facts.len());
            html_fact_list(out, &facts);
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
        let classes = step.classes();
        if !classes.is_empty() {
            let _ = write!(out, " <span class=\"muted\">· {}</span>", esc(&classes.join(" · ")));
        }
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

/// One line for each derived fact (design 22.5): the class, the description, the forced edits,
/// the affected bodies, the spotlight with its reason, and the content change of a file fact.
fn fact_items_of(model: &Model, steps: Option<&[usize]>) -> Vec<(String, Option<String>)> {
    fact_items(model, steps, true)
}

/// The fact items of the full report (the content of each fact), or of the short report (a diff
/// only for a fact that is not "neutral (proved)").
fn fact_items(model: &Model, steps: Option<&[usize]>, full: bool) -> Vec<(String, Option<String>)> {
    model
        .summary
        .facts
        .iter()
        .filter(|view| steps.is_none_or(|steps| steps.contains(&view.step)))
        .map(|view| {
            let fact = &view.fact;
            let class = fact
                .class
                .map(|c| match c {
                    semilla_core::Class::Neutral => "neutral (proved)".to_string(),
                    semilla_core::Class::NotChecked => "not checked".to_string(),
                    other => serde_name(&other),
                })
                .unwrap_or_else(|| "class after phase 2".into());
            let mut text = format!("step {}: {} derived [{class}]: {}", view.step, fact.id, fact.description);
            for group in fact.sites.iter().filter(|group| !fact.names_sites(group)) {
                text.push_str(&format!("; {} site(s): {} ({})", group.sites.len(), group.edit, group.sites.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")));
            }
            // The short report shows the first bodies of a long list. The full report has each
            // body (brief 5h, C6).
            let list = |bodies: &[String]| {
                let limit = if full { bodies.len() } else { semilla_core::report::LIST_LIMIT };
                let more = if bodies.len() > limit { format!(", and {} more (the full list: `--full`)", bodies.len() - limit) } else { String::new() };
                format!("{}{more}", bodies.iter().take(limit).map(|b| format!("`{b}`")).collect::<Vec<_>>().join(", "))
            };
            if !fact.affected.is_empty() {
                text.push_str(&format!("; {} affected bod{}: {}", fact.affected.len(), if fact.affected.len() == 1 { "y" } else { "ies" }, list(&fact.affected)));
            }
            if !fact.equal_sites.is_empty() {
                text.push_str(&format!("; {}: {}", semilla_core::report::EQUAL_SITES, list(&fact.equal_sites)));
            }
            if let Some(why) = &fact.spotlight_why {
                text.push_str(&format!("; spotlight: {why}; reason: {}", fact.reason.as_deref().map(text::quoted).unwrap_or_else(|| "MISSING".into())));
            }
            (text, if full { fact.content.clone() } else { model.fact_diff(view) })
        })
        .collect()
}

/// The Markdown list of fact items: each content change is a fenced block in its list item.
fn md_fact_list(out: &mut String, items: &[(String, Option<String>)]) {
    for (text, content) in items {
        let _ = writeln!(out, "- {}", md_with_code(text));
        if let Some(content) = content {
            out.push('\n');
            out.push_str(&fence("diff", content, "  "));
        }
    }
    out.push('\n');
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
    let verdict = if summary.accepted {
        "**Accepted**"
    } else if summary.not_complete {
        // A step has no check (design 28). The first notice gives the steps and the cause.
        "**Not complete**"
    } else {
        "**Rejected**"
    };
    let _ = writeln!(out, "{verdict} · {}\n", md(&counts_text(&summary.counts)));
    if let Some(notice) = model.phase_notice() {
        let _ = writeln!(out, "> **{}**\n", md(notice));
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
        let _ = writeln!(out, "### {} · {} · {} · {}\n", code(&group.id), md(&steps_label(&group.steps)), md(&serde_name(&group.kind)), md(&agent_text(group, &group.title)));
        if !group.why.is_empty() {
            let _ = writeln!(out, "{}\n", md(&agent_text(group, &group.why)));
        }
        let _ = writeln!(out, "{} · {}\n", md(&risk_text(group)), md(&group_meta(group).join(" · ")));
        // The short report does not repeat the title of the group as the text of its one step.
        let steps = model.steps(group);
        let repeat = steps.len() == 1 && steps[0].intent == group.title && steps[0].errors.is_empty();
        if full || !repeat {
            md_steps(out, model, group);
        }
        // The facts of the steps of the group, as visible as its claims.
        let facts = fact_items(model, Some(&group.steps), full);
        if !facts.is_empty() {
            let _ = writeln!(out, "#### Derived facts ({})\n", facts.len());
            md_fact_list(out, &facts);
        }
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
    let facts = fact_items(model, Some(&free), full);
    if !facts.is_empty() {
        let _ = writeln!(out, "### Steps with no group\n\n#### Derived facts ({})\n", facts.len());
        md_fact_list(out, &facts);
    }
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
        let classes = step.classes();
        if !classes.is_empty() {
            let _ = write!(line, " · {}", md(&classes.join(" · ")));
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
        // A folded subgroup is one line with the counts and the claim IDs.
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
        // The renames, the moves, and the new code stay visible in a folded subgroup, with the
        // status and its proof text (C6: the same notes as the HTML report; C12: a move shows its
        // source and its target).
        for view in &sub.claims {
            let status = StatusText::of(view).text();
            if let Some((before, after)) = rename_text(view) {
                let _ = writeln!(out, "  - {} {}: {} → {} · {}", code(&view.id), code(&view.op), code(&before), code(&after), md(&status));
            } else if let Some((from, to)) = move_text(view) {
                let _ = writeln!(out, "  - {} {}: {} → {} · {}", code(&view.id), code(&view.op), code(&from), code(&to), md(&status));
            } else if model.new_code(view).is_none() {
                let _ = writeln!(out, "  - {} {}{} · {}", code(&view.id), code(&view.op), view.at.as_deref().map(|at| format!(" {}", code(at))).unwrap_or_default(), md(&status));
            }
            if let Some(new) = model.new_code(view) {
                let _ = writeln!(out, "  - {} {}", code(&view.id), code(&view.op));
                md_new_code(out, &new, "    ");
            }
        }
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

    #[test]
    fn a_crate_is_not_a_file() {
        let is_file = |location: &str| location.contains('/') || location.contains('.');
        assert!(is_file("crates/cli/src/main.rs") && is_file("Cargo.toml") && !is_file("ledger_cli") && !is_file("ledger_cli::tests"));
    }

    #[test]
    fn script_end_is_broken() {
        assert_eq!(script_safe("a</script>b</SCRIPT>"), "a<\\/script>b<\\/SCRIPT>");
        assert_eq!(script_safe("plain"), "plain");
    }
}
