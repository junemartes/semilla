//! The tests of batch 11c (design 71).

use semilla_core::report::{Fact, StepReport};

fn fact(id: &str, description: &str, note: &str) -> Fact {
    Fact { id: id.to_string(), part: "edit".to_string(), description: description.to_string(), note: Some(note.to_string()), ..Default::default() }
}

fn report(facts: Vec<Fact>) -> String {
    let step = StepReport { step: 0, intent: "x".to_string(), facts, ..Default::default() };
    crate::views::text_report_with(std::slice::from_ref(&step), false)
}

// R71.1-1, the part of a line that repeats: a line below a fact of 120 characters or more that
// two facts have shows one time, with the numbers of the other facts; each other fact names
// the first fact. A full text (`semilla expand fN`) keeps each line.
// rule+ R71.1-1
#[test]
fn a_long_line_of_many_facts_shows_one_time() {
    let long = format!("this body uses {}, which Semilla did not check", (0..12).map(|n| format!("`shop::helper_{n}`")).collect::<Vec<_>>().join(", "));
    let text = report(vec![fact("f1", "changed literal `1` → `2` at `shop::a/tail`", &long), fact("f2", "changed literal `3` → `4` at `shop::b/tail`", &long), fact("f3", "changed literal `5` → `6` at `shop::c/tail`", &long)]);
    assert_eq!(text.matches(&long).count(), 1, "{text}");
    assert!(text.contains(&format!("      note: {long} (the same line for f2, f3)")), "{text}");
    assert_eq!(text.matches("      note: the same line as f1\n").count(), 2, "{text}");
    let step = StepReport { step: 0, intent: "x".to_string(), facts: vec![fact("f1", "changed literal `1` → `2` at `shop::a/tail`", &long), fact("f2", "changed literal `3` → `4` at `shop::b/tail`", &long)], ..Default::default() };
    let full = crate::views::fact_full_text(&step, "f2").expect("the fact");
    assert!(full.contains(&long) && !full.contains("the same line"), "{full}");
}

// rule- R71.1-1
#[test]
fn a_short_line_of_many_facts_shows_at_each_fact() {
    let short = "this body uses `shop::helper`, which Semilla did not check";
    let text = report(vec![fact("f1", "changed literal `1` → `2` at `shop::a/tail`", short), fact("f2", "changed literal `3` → `4` at `shop::b/tail`", short)]);
    assert_eq!(text.matches(short).count(), 2, "{text}");
    assert!(!text.contains("the same line"), "{text}");
}

// R71.1-1, the fact line: a pair of block texts over the budget shows the changed lines with
// their context; a short pair keeps the line diff of design 54.15.
// rule+ R71.1-1
#[test]
fn a_long_block_pair_shows_its_changed_lines_and_the_command() {
    let block = |change: &str| format!("⏎{}⏎", (0..60).map(|n| if n == 30 { change.to_string() } else { format!("    let v{n} = {n};") }).collect::<Vec<_>>().join("⏎"));
    let description = format!("changed block: `{}` → `{}` at `shop::f/S0`", block("    let v30 = 30;"), block("    let v30 = 31;"));
    let text = report(vec![Fact { id: "f1".into(), part: "edit".into(), description: description.clone(), ..Default::default() }]);
    assert!(text.contains("(line diff: `-` old, `+` new) (a large text: 60 → 60 lines, "), "{text}");
    assert!(text.contains("2 changed lines, with 2 lines of context; the full text: semilla expand f1 --step 0)"), "{text}");
    assert!(text.contains("- " ) && text.contains("let v30 = 30;") && text.contains("let v30 = 31;"), "{text}");
    assert!(!text.contains("let v5 = 5;"), "{text}");
    // Negative: a short pair keeps each line.
    let short = |change: &str| format!("⏎{}⏎", (0..5).map(|n| if n == 2 { change.to_string() } else { format!("let v{n} = {n};") }).collect::<Vec<_>>().join("⏎"));
    let text = report(vec![Fact { id: "f1".into(), part: "edit".into(), description: format!("changed block: `{}` → `{}` at `shop::f/S0`", short("let v2 = 2;"), short("let v2 = 3;")), ..Default::default() }]);
    assert!(text.contains("let v0 = 0;") && text.contains("let v4 = 4;") && !text.contains("a large text"), "{text}");
}
