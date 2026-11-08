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

    // The source diffs.
    assert!(markdown.contains("+ pub fn total(&self) -> Result<u32, CartError> {"));
    assert!(markdown.contains("- pub fn sum(&self) -> u32 {"));
    assert!(html.contains("<span class=\"add\">+ pub fn total(&amp;self) -&gt; Result&lt;u32, CartError&gt; {\n</span>"));
    // An unchanged source does not appear in the sources section.
    assert!(!markdown.contains("### `shop_cli::load_cart`"));
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
