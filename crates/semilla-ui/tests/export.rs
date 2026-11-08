//! Tests of the standalone report: render the demo data to HTML and Markdown and check the
//! contents.

use std::path::PathBuf;
use std::process::Command;

use semilla_core::view::{Status, TreeNode};
use semilla_core::{ChangeSet, ClaimStatus, Report};
use semilla_ui::export::{render_html, render_markdown};
use semilla_ui::fixture::demo_data;
use semilla_ui::review::ReviewFile;
use semilla_ui::summary::changeset;
use semilla_ui::{Ctx, UiData};

/// A new folder for the outputs, in the target folder of the tests.
fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-ui-export");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn empty_data() -> UiData {
    UiData {
        changeset: ChangeSet::default(),
        report: Report::default(),
        tree: TreeNode {
            id: String::new(),
            label: String::new(),
            status: Status::Same,
            class: None,
            claims: Vec::new(),
            diff: None,
            summary: None,
            children: Vec::new(),
        },
        flows: Vec::new(),
        graph: Default::default(),
        sources: Default::default(),
    }
}

/// The text of the element `<script id="{id}">`, if the page has one.
fn script(html: &str, id: &str) -> Option<String> {
    let open = format!("<script id=\"{id}\">");
    let start = html.find(&open)? + open.len();
    let end = start + html[start..].find("</script>")?;
    Some(html[start..end].to_string())
}

/// The page without the vendored Mermaid script.
fn without_vendor(html: &str) -> String {
    let open = "<script id=\"mermaid-lib\">";
    match html.find(open) {
        Some(start) => {
            let end = start + html[start..].find("</script>").expect("the vendor script ends") + "</script>".len();
            format!("{}{}", &html[..start], &html[end..])
        }
        None => html.to_string(),
    }
}

/// The text of the `<details>` element of a subgroup, from its start tag to its end tag.
fn subgroup_element<'a>(html: &'a str, id: &str) -> &'a str {
    let marker = format!("data-subgroup=\"{id}\"");
    let at = html.find(&marker).unwrap_or_else(|| panic!("no subgroup {id} in the HTML"));
    let start = html[..at].rfind("<details").unwrap();
    let end = at + html[at..].find("</details>").unwrap();
    &html[start..end]
}

#[test]
fn both_formats_show_all_parts() {
    let data = demo_data();
    let html = render_html(&data);
    let markdown = render_markdown(&data);
    let dir = out_dir();
    std::fs::write(dir.join("demo.html"), &html).unwrap();
    std::fs::write(dir.join("demo.md"), &markdown).unwrap();

    let ctx = Ctx::new(data.clone());
    let summary = changeset(&ctx, &ReviewFile::default());
    assert!(!summary.groups.is_empty());
    for group in &summary.groups {
        for text in [&group.id, &group.title] {
            assert!(html.contains(&html_text(text)), "HTML: no group text {text}");
            assert!(markdown.contains(&md_text(text)), "Markdown: no group text {text}");
        }
        for sub in &group.subgroups {
            assert!(html.contains(&format!("data-subgroup=\"{}\"", sub.id)), "HTML: no subgroup {}", sub.id);
            assert!(html.contains(&html_text(&sub.title)), "HTML: no subgroup title {}", sub.title);
            assert!(markdown.contains(&md_text(&sub.title)), "Markdown: no subgroup title {}", sub.title);
            for claim in &sub.claims {
                assert!(html.contains(&format!("data-claim=\"{}\"", claim.id)), "HTML: no claim {}", claim.id);
                assert!(
                    markdown.contains(&format!("**{}**", claim.id)) || markdown.contains(&format!("`{}`", claim.id)),
                    "Markdown: no claim {}",
                    claim.id
                );
            }
        }
    }
    // Each claim of the change set appears.
    for step in &data.changeset.steps {
        for claim in &step.claims {
            assert!(html.contains(&format!("data-claim=\"{}\"", claim.id)), "HTML: no claim {}", claim.id);
        }
    }

    // The uncovered changes, with their hints.
    let uncovered: Vec<_> = data.report.steps.iter().flat_map(|step| step.uncovered.iter()).collect();
    assert!(!uncovered.is_empty());
    for item in &uncovered {
        assert!(html.contains(&html_text(&item.description)), "HTML: no uncovered change {}", item.description);
        assert!(markdown.contains(&item.description), "Markdown: no uncovered change {}", item.description);
        let hint = item.hint.as_deref().unwrap();
        assert!(html.contains(&html_text(hint)));
        assert!(markdown.contains(&md_text(hint)), "Markdown: no hint {hint}");
    }
    assert!(html.contains("id=\"uncovered\""));
    assert!(markdown.contains("## Uncovered changes"));

    // The flow views: in HTML for Mermaid with the inlined library, in Markdown as Mermaid blocks.
    assert!(!data.flows.is_empty());
    for flow in &data.flows {
        assert!(html.contains(&format!("data-flow=\"{}\"", flow.function)), "HTML: no flow view {}", flow.function);
        assert!(markdown.contains(&format!("### `{}`", flow.function)), "Markdown: no flow view {}", flow.function);
    }
    assert_eq!(html.matches("<pre class=\"mermaid\">").count(), data.flows.len());
    assert_eq!(markdown.matches("```mermaid\n").count(), data.flows.len());
    assert!(script(&html, "mermaid-lib").is_some_and(|text| text.len() > 1_000_000), "the Mermaid library is inline");

    // The change tree and the summary facts.
    let tree_line = data.tree.render().lines().nth(1).unwrap().to_string();
    assert!(markdown.contains(&tree_line));
    assert!(html.contains(&html_text(&tree_line)));
    for text in ["Rejected", "Score", "API level", "Panic delta", "Warnings", "Steps"] {
        assert!(html.contains(text), "HTML: no {text}");
        assert!(markdown.contains(text), "Markdown: no {text}");
    }
    assert!(html.contains(&html_text(&data.report.warnings[0])));

    // The source diffs: only the smallest changed parts, and no unchanged source.
    let md_sources = &markdown[markdown.find("## Sources").unwrap()..];
    let html_sources = &html[html.find("<h2 id=\"sources\">").unwrap()..];
    assert!(md_sources.contains("### `shop_core::cart::Cart::total/S2/body/S0/cond`"));
    assert!(md_sources.contains("- item.qty > MAX_QTY\n+ item.qty >= MAX_QTY"));
    assert!(html_sources.contains("<span class=\"add\">+ item.qty &gt;= MAX_QTY\n</span>"));
    // `Cart::total` contains the changed part `Cart::total/S2/body/S0/cond`, so it does not appear.
    assert!(!md_sources.contains("### `shop_core::cart::Cart::total`\n"));
    assert!(!html_sources.contains("data-source=\"shop_core::cart::Cart::total\""));
    assert!(md_sources.contains("+     let total = cart.total().unwrap_or(0);"));
    assert!(!md_sources.contains("### `shop_cli::load_cart`"));
    assert!(!html_sources.contains("data-source=\"shop_cli::load_cart\""));
}

#[test]
fn steps_show_their_intents_and_claims() {
    let data = demo_data();
    let html = render_html(&data);
    let markdown = render_markdown(&data);
    for step in &data.changeset.steps {
        let ids: Vec<String> = step.claims.iter().map(|claim| format!("`{}`", claim.id)).collect();
        let line = format!("- step {}: {} — {}", step.index, md_text(&step.intent), ids.join(", "));
        assert!(markdown.contains(&line), "Markdown: no step line {line}");
        let item = format!("<li data-step=\"{}\">step {}: {}", step.index, step.index, html_text(&step.intent));
        assert!(html.contains(&item), "HTML: no step {item}");
        for claim in &step.claims {
            assert!(html.contains(&format!("<a href=\"#claim-{0}\"><code>{0}</code></a>", claim.id)));
        }
    }
    // A step error appears under its step.
    let mut data = demo_data();
    data.report.steps[0].errors.push("the build fails".into());
    assert!(render_markdown(&data).contains("  - error: the build fails"));
    assert!(render_html(&data).contains("<div class=\"notice bad\">error: the build fails</div>"));
}

#[test]
fn folded_subgroups_show_renames_and_new_code() {
    let data = demo_data();
    let html = render_html(&data);
    let markdown = render_markdown(&data);
    // G1 Mechanical (folded): the renames, with the texts of the claim results.
    assert!(markdown.contains("  - `c1` `rename`: `fn sum(&self)` → `fn total(&self)`"), "{markdown}");
    let renames = subgroup_element(&html, "G1:mechanical");
    assert!(!renames[..renames.find('>').unwrap()].contains(" open"));
    assert!(renames.contains("<code>fn sum(&amp;self)</code> → <code>fn total(&amp;self)</code>"));
    // G3 Mechanical (folded): the extracted function. A later move-item moved it to billing.
    let line_total = data.sources["shop_core::billing::line_total"].after.clone().unwrap();
    let first = line_total.lines().next().unwrap();
    assert!(markdown.contains("  - `c6` `extract-function`\n    - new function: `shop_core::billing::line_total`\n"), "{markdown}");
    assert!(markdown.contains(&format!("      ```rust\n      {first}\n")));
    let extract = subgroup_element(&html, "G3:mechanical");
    assert!(!extract[..extract.find('>').unwrap()].contains(" open"));
    assert!(extract.contains("new function <code>shop_core::billing::line_total</code>"));
    assert!(extract.contains(&html_text(first)));
    // An added item in an open subgroup shows its source too, unless the claim diff shows it.
    let mut data = demo_data();
    data.sources.get_mut("shop_core::cart::CartError").unwrap().after =
        Some("pub enum CartError {\n    TooMany(u32),\n    Empty,\n}".into());
    let markdown = render_markdown(&data);
    assert!(markdown.contains("  - new item: `shop_core::cart::CartError`"), "{markdown}");
    assert!(markdown.contains("    ```rust\n    pub enum CartError {\n"));
}

#[test]
fn neutral_rewrites_have_low_risk() {
    // G2 is an api group. Make all its claims proved neutral, and remove its uncovered change.
    let mut data = demo_data();
    for step in data.report.steps.iter_mut().filter(|step| step.step == 6 || step.step == 7) {
        step.uncovered.clear();
        for claim in &mut step.claims {
            claim.class = Some(semilla_core::Class::Neutral);
        }
    }
    let ctx = Ctx::new(data.clone());
    let summary = changeset(&ctx, &ReviewFile::default());
    let g2 = summary.groups.iter().find(|group| group.id == "G2").unwrap();
    assert_eq!(g2.risk, 1, "the public API alone gives one dot: {:?}", g2.risk_reasons);
    assert_eq!(g2.risk_reasons, ["public API"]);
    let g1 = summary.groups.iter().find(|group| group.id == "G1").unwrap();
    assert!(g1.risk > g2.risk, "a real control flow change ranks above neutral rewrites");
    assert_eq!(summary.groups[0].id, "G1");

    // Pending claims give a reason, but no points.
    for claim in data.report.steps.iter_mut().filter(|step| step.step == 6).flat_map(|step| step.claims.iter_mut()) {
        claim.status = ClaimStatus::Pending { reason: "phase 2".into() };
    }
    let ctx = Ctx::new(data);
    let summary = changeset(&ctx, &ReviewFile::default());
    let g2 = summary.groups.iter().find(|group| group.id == "G2").unwrap();
    assert_eq!(g2.risk, 1, "{:?}", g2.risk_reasons);
    assert!(g2.risk_reasons.contains(&"7 pending".to_string()), "{:?}", g2.risk_reasons);
}

#[test]
fn folded_subgroups_are_collapsed() {
    let data = demo_data();
    let html = render_html(&data);
    let markdown = render_markdown(&data);
    let ctx = Ctx::new(data);
    let summary = changeset(&ctx, &ReviewFile::default());
    let subgroups: Vec<_> = summary.groups.iter().flat_map(|group| group.subgroups.iter()).collect();
    assert!(subgroups.iter().any(|sub| sub.folded), "the demo has a folded subgroup");
    assert!(subgroups.iter().any(|sub| !sub.folded), "the demo has an open subgroup");
    for sub in subgroups {
        let element = subgroup_element(&html, &sub.id);
        let start_tag = &element[..element.find('>').unwrap()];
        if sub.folded {
            assert!(!start_tag.contains(" open"), "HTML: {} must be collapsed: {start_tag}", sub.id);
            assert!(start_tag.contains("folded"));
            // Markdown: one line with the counts and the claim IDs.
            let line = markdown
                .lines()
                .find(|line| line.starts_with("- ▸ ") && line.contains(&md_text(&sub.title)))
                .unwrap_or_else(|| panic!("Markdown: no folded line for {}", sub.id));
            assert!(line.contains("folded:"), "{line}");
            assert!(line.contains(&format!("{} claim", sub.counts.claims)), "{line}");
            for claim in &sub.claims {
                assert!(line.contains(&format!("`{}`", claim.id)), "{line}");
            }
        } else {
            assert!(start_tag.ends_with(" open"), "HTML: {} must be open: {start_tag}", sub.id);
            assert!(markdown.contains(&format!("#### ▾ {} · {}", sub.role_label, md_text(&sub.title))), "Markdown: no open {}", sub.id);
        }
    }
}

#[test]
fn html_has_no_remote_urls() {
    let html = render_html(&demo_data());
    let page = without_vendor(&html);
    assert!(!page.contains("http://"), "the page has an http URL outside the vendor script");
    assert!(!page.contains("https://"), "the page has an https URL outside the vendor script");
    for pattern in [" src=", "<link", "@import", "url("] {
        assert!(!page.contains(pattern), "the page loads a resource: {pattern}");
    }
    // The vendor script must not end the element early.
    let vendor = script(&html, "mermaid-lib").unwrap();
    assert!(!vendor.to_ascii_lowercase().contains("</script"));
}

#[test]
fn text_from_the_data_is_escaped() {
    let mut data = demo_data();
    let evil = "<script>alert('x')</script> & \"q\" *b* `t` [l](u)";
    let claim = &mut data.changeset.steps[0].claims[0];
    claim.args.insert("note".into(), serde_json::Value::String(evil.into()));
    data.changeset.groups[0].title = format!("Title {evil}");
    data.report.steps[0].claims[0].hint = Some(format!("Hint {evil}"));

    let html = render_html(&data);
    assert!(!html.contains("<script>alert"), "a script tag from the data is not escaped");
    assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;q&quot;"));
    // The demo claims have Rust generics and references.
    assert!(html.contains("Result&lt;u32, CartError&gt;"));
    assert!(!without_vendor(&html).contains("Result<u32, CartError>"));
    // Only the report scripts are elements of the page.
    assert_eq!(without_vendor(&html).matches("<script").count(), 1);

    let markdown = render_markdown(&data);
    assert!(!markdown.contains("<script>"), "Markdown: raw HTML from the data");
    assert!(markdown.contains("Title &lt;script&gt;alert('x')&lt;/script&gt; &amp; \"q\" \\*b\\* \\`t\\` \\[l\\](u)"));
}

#[test]
fn empty_data_renders() {
    let data = empty_data();
    let html = render_html(&data);
    let markdown = render_markdown(&data);
    assert!(html.contains("No groups."));
    assert!(html.contains("No uncovered changes"));
    assert!(!html.contains("<script"), "no flow views: no scripts");
    assert!(markdown.contains("No groups."));
    assert!(markdown.contains("No flow views."));
}

#[test]
fn pending_and_phase_appear() {
    let mut data = demo_data();
    data.report.phase = "syntax".into();
    data.report.pending = 1;
    data.report.steps[0].claims[0].status = ClaimStatus::Pending { reason: "the MIR check waits".into() };
    data.report.steps[0].spotlight.push("unsafe block in shop_core::cart".into());
    let html = render_html(&data);
    let markdown = render_markdown(&data);
    for text in
        ["Phase 1 only: the body proofs are pending.", "pending: the MIR check waits", "Pending claims", "unsafe block in shop_core::cart"]
    {
        assert!(html.contains(text), "HTML: no {text}");
        assert!(markdown.contains(&md_text(text)), "Markdown: no {text}");
    }
    // A pending claim keeps its subgroup open.
    let ctx = Ctx::new(data.clone());
    let summary = changeset(&ctx, &ReviewFile::default());
    let sub =
        summary.groups.iter().flat_map(|group| group.subgroups.iter()).find(|sub| sub.claims.iter().any(|claim| claim.id == "c1")).unwrap();
    assert!(!sub.folded);
    assert_eq!(sub.counts.pending, 1);
    assert!(subgroup_element(&html, &sub.id)[..].split('>').next().unwrap().ends_with(" open"));
}

#[test]
fn inline_report_script_has_valid_syntax() {
    let html = render_html(&demo_data());
    let own = script(&html, "report-js").expect("the page has the report script");
    let dir = out_dir();
    let path = dir.join("report-js.js");
    std::fs::write(&path, &own).unwrap();
    match Command::new("node").arg("--check").arg(&path).output() {
        Ok(output) => assert!(output.status.success(), "node --check fails: {}", String::from_utf8_lossy(&output.stderr)),
        Err(_) => eprintln!("node is not installed: no syntax check"),
    }
}

fn html_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn md_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '#' | '|' | '~' | '!' | '$' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

#[test]
fn markdown_lists_keep_code_spans() {
    let mut data = semilla_ui::fixture::demo_data();
    data.report.warnings.push("the item `a::b_c` changed".into());
    let text = semilla_ui::export::render_markdown(&data);
    assert!(text.contains("the item `a::b_c` changed"), "the code span stays");
}
