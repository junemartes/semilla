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
            build_code: None,
            claims: Vec::new(),
            diff: None,
            steps: Vec::new(),
            summary: None,
            children: Vec::new(),
        },
        flows: Vec::new(),
        graph: Default::default(),
        sources: Default::default(),
        fact_sources: Default::default(),
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
    let markdown = render_markdown(&data, true);
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
    for text in ["Rejected", "Facts", "API level", "Panic delta", "Warnings", "Steps", "An internal error: "] {
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
    let markdown = render_markdown(&data, true);
    for step in &data.changeset.steps {
        let ids: Vec<String> = step.claims.iter().map(|claim| format!("`{}`", claim.id)).collect();
        let line = format!("- step {}: {} — {}", step.index, md_text(&format!("\"{}\"", step.intent)), ids.join(", "));
        assert!(markdown.contains(&line), "Markdown: no step line {line}");
        let item = format!("<li data-step=\"{}\">step {}: {}", step.index, step.index, html_text(&format!("\"{}\"", step.intent)));
        assert!(html.contains(&item), "HTML: no step {item}");
        for claim in &step.claims {
            assert!(html.contains(&format!("<a href=\"#claim-{0}\"><code>{0}</code></a>", claim.id)));
        }
    }
    // A step error appears under its step.
    let mut data = demo_data();
    data.report.steps[0].errors.push("the build fails".into());
    assert!(render_markdown(&data, true).contains("  - error: the build fails"));
    assert!(render_html(&data).contains("<div class=\"notice bad\">error: the build fails</div>"));
}

#[test]
fn folded_subgroups_show_renames_and_new_code() {
    let data = demo_data();
    let html = render_html(&data);
    let markdown = render_markdown(&data, true);
    // G1 Mechanical (folded): the renames, with the texts of the claim results.
    assert!(markdown.contains("  - `c1` `rename`: `fn sum(&self)` → `fn total(&self)` · verified (proof "), "{markdown}");
    // C12: a move shows its source and its target.
    assert!(markdown.contains("`c7` `move-item`: `shop_core::cart::line_total` → `shop_core::billing::line_total`") || markdown.contains("`move-item` at `shop_core::cart::line_total` → `shop_core::billing::line_total`"), "{markdown}");
    assert!(html.contains("<code>shop_core::cart::line_total</code> → <code>shop_core::billing::line_total</code>"), "{html}");
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
    let markdown = render_markdown(&data, true);
    assert!(markdown.contains("  - new item: `shop_core::cart::CartError`"), "{markdown}");
    assert!(markdown.contains("    ```rust\n    pub enum CartError {\n"));
}

#[test]
fn the_facts_give_the_risk_and_the_neutral_label() {
    // Trial 3, D9: a group with a `control_flow` step had "risk 0/3 · all neutral".
    let mut data = demo_data();
    for step in data.report.steps.iter_mut().filter(|step| step.step == 6 || step.step == 7) {
        step.uncovered.clear();
        step.api_level = None;
        for claim in &mut step.claims {
            claim.class = Some(semilla_core::Class::Neutral);
        }
    }
    let fact = |class: Option<semilla_core::Class>| semilla_core::Fact { id: "f9".into(), part: "edit".into(), description: "x".into(), status: "derived".into(), class, ..Default::default() };
    let g2 = |data: &semilla_ui::UiData| {
        let ctx = Ctx::new(data.clone());
        changeset(&ctx, &ReviewFile::default()).groups.into_iter().find(|group| group.id == "G2").unwrap()
    };
    let neutral = g2(&data);
    for (class, risk) in [(Some(semilla_core::Class::ControlFlow), 2), (Some(semilla_core::Class::Logic), 1), (None, 1)] {
        let mut data = data.clone();
        data.report.steps.iter_mut().find(|step| step.step == 6).unwrap().facts.push(fact(class));
        let group = g2(&data);
        assert!(!group.all_neutral, "{class:?}");
        assert!(group.risk_score >= neutral.risk_score + risk, "{class:?}: {:?}", group.risk_reasons);
    }
    let mut data = data.clone();
    data.report.steps.iter_mut().find(|step| step.step == 6).unwrap().facts.push(fact(Some(semilla_core::Class::Neutral)));
    assert_eq!(g2(&data).risk_score, neutral.risk_score);
}

#[test]
fn neutral_rewrites_have_low_risk() {
    // G2 has the stored kind `api`. Make all its claims proved neutral, and remove its uncovered
    // change. The stored kind is a label of the agent: it gives no dot (batch 5k, A3). The API
    // level of the report gives the dot.
    let mut data = demo_data();
    for step in data.report.steps.iter_mut().filter(|step| step.step == 6 || step.step == 7) {
        step.uncovered.clear();
        for claim in &mut step.claims {
            claim.class = Some(semilla_core::Class::Neutral);
        }
    }
    let stored = changeset(&Ctx::new(data.clone()), &ReviewFile::default());
    let g2 = stored.groups.iter().find(|group| group.id == "G2").unwrap();
    assert!(!g2.risk_reasons.contains(&"public API".to_string()), "{:?}", g2.risk_reasons);
    data.report.steps.iter_mut().find(|step| step.step == 6).unwrap().api_level = Some("minor".into());
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
    let markdown = render_markdown(&data, true);
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

    let markdown = render_markdown(&data, true);
    assert!(!markdown.contains("<script>"), "Markdown: raw HTML from the data");
    // The title is the text of the agent: it is in quote marks, and its quote marks have an escape.
    assert!(markdown.contains("\"Title &lt;script&gt;alert('x')&lt;/script&gt; &amp; \\\\\"q\\\\\" \\*b\\* \\`t\\` \\[l\\](u)\""), "{markdown}");
}

#[test]
fn empty_data_renders() {
    let data = empty_data();
    let html = render_html(&data);
    let markdown = render_markdown(&data, true);
    assert!(html.contains("No groups."));
    // The reports have no score and no part for the uncovered changes when there are none.
    for text in [&html, &markdown] {
        assert!(!text.contains("Uncovered") && !text.contains("id=\"uncovered\"") && !text.contains("Score") && !text.contains("has a claim"));
    }
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
    let markdown = render_markdown(&data, true);
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
    let text = semilla_ui::export::render_markdown(&data, true);
    assert!(text.contains("the item `a::b_c` changed"), "the code span stays");
}

#[test]
fn reports_show_the_derived_facts() {
    let mut data = demo_data();
    data.report.steps[0].facts.push(semilla_core::Fact {
        id: "f1".into(),
        at: Some("shop::fee/sig/param[cap]".parse().unwrap()),
        part: "parameter".into(),
        description: "parameter of `shop::fee` added: `cap : Option < u32 >`".into(),
        status: "derived".into(),
        class: Some(semilla_core::Class::Logic),
        affected: vec!["shop::fee".into()],
        sites: vec![semilla_core::SiteGroup { edit: "add the argument `None`".into(), sites: vec!["shop::a/tail".parse().unwrap()] }],
        ..Default::default()
    });
    let markdown = render_markdown(&data, true);
    assert!(markdown.contains("### Derived facts"), "{markdown}");
    assert!(markdown.contains("f1 derived \\[logic\\]") && markdown.contains("1 site(s): add the argument `None`") && markdown.contains("1 affected body"), "{markdown}");
    let html = render_html(&data);
    assert!(html.contains("<h3>Derived facts</h3>") && html.contains("f1 derived [logic]"));
    // The fact is also inside the section of the group that has its step.
    let group = data.changeset.groups.iter().find(|g| g.steps.contains(&data.report.steps[0].step)).unwrap();
    let section = markdown.split(&format!("### `{}`", group.id)).nth(1).unwrap();
    let section = section.split("\n### ").next().unwrap();
    assert!(section.contains("#### Derived facts (1)"), "{section}");
    assert!(html.contains("<h4>Derived facts (1)</h4>"));
    // C11: the group title has its steps, so the reviewer can follow the order of the work.
    assert!(markdown.lines().any(|line| line.starts_with(&format!("### `{}` · step", group.id))), "{markdown}");
}

#[test]
fn reports_show_the_content_of_a_file_fact() {
    // Trial 3, A6e: the report showed `.cargo/config: added` with no content.
    let mut data = demo_data();
    data.report.steps[0].facts.push(semilla_core::Fact {
        id: "f9".into(),
        file: Some(".cargo/config".into()),
        part: ".cargo/config:target.x.runner".into(),
        description: ".cargo/config: target.x.runner added (\"true\")".into(),
        status: "derived".into(),
        class: Some(semilla_core::Class::Other),
        content: Some("+ [target.x]\n+ runner = \"true\"".into()),
        ..Default::default()
    });
    let markdown = render_markdown(&data, true);
    assert!(markdown.contains("  ```diff\n  + [target.x]\n  + runner = \"true\"\n  ```"), "{markdown}");
    let html = render_html(&data);
    assert!(html.contains("<pre class=\"diff\">+ [target.x]\n+ runner = &quot;true&quot;</pre>"), "{html}");
}

fn fact(id: &str, at: &str, class: semilla_core::Class, description: &str) -> semilla_core::Fact {
    semilla_core::Fact { id: id.into(), at: Some(at.parse().unwrap()), part: "edit".into(), description: description.into(), status: "derived".into(), class: Some(class), ..Default::default() }
}

#[test]
fn the_short_report_shows_each_fact_one_time() {
    // Brief 5f, D2: the report of trial 4 had 1105 lines for six small changes.
    let mut data = demo_data();
    let step = data.report.steps[0].step;
    let function = data.flows[0].function.clone();
    assert!(data.flows.len() >= 2, "the demo data has two flow views");
    data.report.steps[0].facts.push(fact("f1", &format!("{function}/S0"), semilla_core::Class::Logic, "changed statement `let total = …`"));
    data.report.steps[0].facts.push(fact("f2", "shop_core::helper/S0", semilla_core::Class::Neutral, "changed statement `let name = …`"));
    let pair = |old: &str, new: &str| semilla_ui::SourcePair { before: Some(old.into()), after: Some(new.into()) };
    data.fact_sources.insert(format!("{step}:{function}/S0"), pair("let total =\n    old_sum(items);", "let total =\n    new_sum(items);"));
    data.fact_sources.insert(format!("{step}:shop_core::helper/S0"), pair("let name =\n    NEUTRAL_OLD;", "let name =\n    NEUTRAL_NEW;"));
    let short = render_markdown(&data, false);
    let full = render_markdown(&data, true);
    // Each fact shows one time: in its group, not in the summary.
    assert_eq!(short.matches("f1 derived").count(), 1, "{short}");
    assert_eq!(short.matches("f2 derived").count(), 1, "{short}");
    assert!(!short.contains("\n### Derived facts") && !full.contains("\n### Derived facts"), "{short}");
    assert_eq!(full.matches("f1 derived").count(), 1, "{full}");
    // A source diff only for a fact that is not "neutral (proved)".
    assert!(short.contains("new_sum(items)") && !short.contains("NEUTRAL_NEW"), "{short}");
    // A flow view only for the body of the logic fact, one time.
    assert_eq!(short.matches("```mermaid").count(), 1, "{short}");
    assert_eq!(short.matches(&format!("\n### `{function}`\n")).count(), 1, "{short}");
    assert_eq!(full.matches("```mermaid").count(), data.flows.len(), "{full}");
    // The change tree and the sources come only with `--full`.
    assert!(!short.contains("## Change tree") && !short.contains("## Sources"), "{short}");
    assert!(short.contains("`semilla review --md FILE --full`"), "{short}");
    assert!(full.contains("## Change tree") && full.contains("## Sources"), "{full}");
    assert!(short.lines().count() < full.lines().count());
    // No fact of the class `control_flow` or `logic`: no flow view.
    data.report.steps[0].facts.remove(0);
    assert!(!render_markdown(&data, false).contains("```mermaid"));
}

#[test]
fn the_heading_of_a_flow_view_shows_one_time() {
    // Trial 4, D9: the breadcrumb `ledger_core › withdraw` repeated the heading.
    let mut data = demo_data();
    let function = data.flows[1].function.clone();
    data.flows[1].breadcrumb = function.split("::").map(str::to_string).collect();
    let (markdown, html) = (render_markdown(&data, true), render_html(&data));
    let crumb = data.flows[1].breadcrumb.join(" › ");
    assert!(!markdown.contains(&md_text(&crumb)) && !html.contains(&html_text(&crumb)), "{markdown}");
    let flows = markdown.split("\n## Flow views").nth(1).unwrap().split("\n## ").next().unwrap();
    assert_eq!(flows.matches(&format!("### `{function}`\n")).count(), 1, "{flows}");
    // A frame in the function is new information, so the breadcrumb shows.
    data.flows[1].breadcrumb.push("for line in lines".into());
    assert!(render_markdown(&data, true).contains(&md_text(&format!("{crumb} › for line in lines"))));
    assert!(render_html(&data).contains(&html_text(&format!("{crumb} › for line in lines"))));
}

#[test]
fn a_new_node_of_a_flow_view_has_the_added_class() {
    // Trial 4, D9. The graph defines only the classes that it uses.
    let mut data = demo_data();
    data.flows.truncate(1);
    for node in &mut data.flows[0].nodes {
        node.status = Status::Same;
    }
    let last = data.flows[0].nodes.len() - 1;
    data.flows[0].nodes[last].status = Status::Added;
    let markdown = render_markdown(&data, true);
    let id = &data.flows[0].nodes[last].id;
    assert!(markdown.lines().any(|line| line.trim_start().starts_with(id.as_str()) && line.ends_with(":::added")), "{markdown}");
    assert!(markdown.contains("classDef added") && !markdown.contains("classDef removed") && !markdown.contains("classDef changed"), "{markdown}");
}

#[test]
fn the_line_count_shows_only_for_a_change_set_with_no_fact() {
    // Trial 4, D9: "Changed lines: 7 fine-grained (100%)" did not agree with a change set of 30
    // facts. The count has only the lines that a claim explains.
    let mut data = demo_data();
    data.report.lines.fine = 7;
    assert!(render_markdown(&data, true).contains("Changed lines") && render_html(&data).contains("Changed lines"));
    assert_eq!(changeset(&Ctx::new(data.clone()), &ReviewFile::default()).lines_text, "lines: 7 fine-grained (100%), 0 new, 0 removed, 0 rewritten");
    data.report.steps[0].facts.push(fact("f1", "shop_core::helper/S0", semilla_core::Class::Logic, "changed statement"));
    assert!(!render_markdown(&data, true).contains("Changed lines") && !render_html(&data).contains("Changed lines"));
    assert_eq!(changeset(&Ctx::new(data.clone()), &ReviewFile::default()).lines_text, "");
}

#[test]
fn a_partial_claim_is_not_counted_as_verified() {
    // Brief 5h, C1 (defect D4). The result line of `tag` says "N partial" for a property claim
    // with a partial proof. The summary and the group line of a report use the same counts.
    let mut data = demo_data();
    let before = semilla_ui::export::render_markdown(&data, false);
    assert!(!before.contains(" partial"), "the demo data has no partial claim");
    let result = data.report.steps.iter_mut().flat_map(|step| step.claims.iter_mut()).find(|result| matches!(result.status, semilla_core::report::ClaimStatus::Verified { .. })).expect("a verified claim");
    result.status = semilla_core::report::ClaimStatus::Verified { proof: "partial".into() };
    let markdown = semilla_ui::export::render_markdown(&data, false);
    let count = |text: &str, word: &str| -> usize {
        // The number before the word in the summary line (`… claims: 5 verified, 1 partial`).
        let line = text.lines().find(|line| line.starts_with("**")).unwrap_or_default();
        line.split(&format!(" {word}")).next().and_then(|head| head.rsplit([' ', ':']).next()).and_then(|number| number.parse().ok()).unwrap_or(0)
    };
    assert_eq!(count(&markdown, "partial"), 1, "{markdown}");
    assert_eq!(count(&markdown, "verified") + 1, count(&before, "verified"), "{markdown}");
    assert!(markdown.lines().filter(|line| line.contains("1 partial")).count() >= 2, "the summary and the group line: {markdown}");
    let html = semilla_ui::export::render_html(&data);
    assert!(html.contains("1 partial"), "the HTML report has the count");
}

#[test]
fn a_long_list_of_bodies_is_short_in_the_short_report() {
    // Brief 5h, C6. The text output and the short report show 6 bodies of a fact, then
    // "and N more". The report with `--full` has each body.
    let mut data = demo_data();
    let bodies: Vec<String> = (0..21).map(|index| format!("demo::body_{index:02}")).collect();
    let fact = semilla_core::Fact { id: "f9".into(), part: "type".into(), description: "type of `demo::Cart::total` changed: u32 → u64".into(), status: "derived".into(), class: Some(semilla_core::Class::Logic), affected: bodies.clone(), equal_sites: bodies[..8].to_vec(), ..Default::default() };
    data.report.steps[0].facts.push(fact);
    let line = |text: &str| text.lines().find(|line| line.contains("f9 derived")).unwrap_or_default().to_string();
    let short = line(&semilla_ui::export::render_markdown(&data, false));
    assert!(short.contains("21 affected bodies: `demo::body_00`") && short.contains("`demo::body_05`, and 15 more (the full list: `--full`)") && !short.contains("demo::body_06"), "{short}");
    assert!(short.contains("bodies with a changed call site and no change of behavior (proved): `demo::body_00`") && short.contains("and 2 more"), "{short}");
    let full = line(&semilla_ui::export::render_markdown(&data, true));
    assert!(bodies.iter().all(|body| full.contains(&format!("`{body}`"))) && !full.contains(" more"), "{full}");
}
