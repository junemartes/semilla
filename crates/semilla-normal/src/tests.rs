//! Unit tests for `compare` and `suggest_renames`.

use super::*;

fn record(name: &str, level: &str, text: &str, callees: &[&str]) -> BodyRecord {
    BodyRecord {
        name: name.to_string(),
        level: level.to_string(),
        text: text.to_string(),
        shape: "0:return->".to_string(),
        callees: callees.iter().map(|callee| callee.to_string()).collect(),
        loop_free: true,
        term: None,
        span: None,
    }
}

fn levels(records: Vec<BodyRecord>) -> Levels {
    group_levels(records)
}

fn status_of<'a>(results: &'a [BodyComparison], name: &str) -> &'a BodyStatus {
    &results
        .iter()
        .find(|result| result.name == name)
        .unwrap_or_else(|| panic!("no result for {name}: {results:?}"))
        .status
}

fn no_renames() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[test]
fn equal_text_is_n0() {
    let before = levels(vec![record("k::f", "n0", "x + 1", &[])]);
    let after = levels(vec![record("k::f", "n0", "x + 1", &[])]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n0".into(), proof: vec![] }
    );
}

#[test]
fn rename_map_gives_n1() {
    let before = levels(vec![
        record("k::helper", "n0", "x + 1", &[]),
        record("k::f", "n0", "call {k::helper}(x)", &["k::helper"]),
    ]);
    let after = levels(vec![
        record("k::add_one", "n0", "x + 1", &[]),
        record("k::f", "n0", "call {k::add_one}(x)", &["k::add_one"]),
    ]);
    let renames = BTreeMap::from([("k::helper".to_string(), "k::add_one".to_string())]);
    let results = compare(&before, &after, &renames, &[]);
    assert_eq!(
        status_of(&results, "k::add_one"),
        &BodyStatus::Equal { level: "n0".into(), proof: vec![] }
    );
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n1".into(), proof: vec!["rename-map".into()] }
    );
    assert_eq!(results.len(), 2, "{results:?}");
}

#[test]
fn content_hash_gives_n1_without_a_claim() {
    let before = levels(vec![
        record("k::helper", "n0", "x + 1", &[]),
        record("k::f", "n0", "call {k::helper}(x)", &["k::helper"]),
    ]);
    let after = levels(vec![
        record("k::add_one", "n0", "x + 1", &[]),
        record("k::f", "n0", "call {k::add_one}(x)", &["k::add_one"]),
    ]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n1".into(), proof: vec!["content-hash".into()] }
    );
    // Without a claim, the renamed function itself is uncovered.
    assert_eq!(status_of(&results, "k::helper"), &BodyStatus::BeforeOnly);
    assert_eq!(status_of(&results, "k::add_one"), &BodyStatus::AfterOnly);
    assert_eq!(
        suggest_renames(&before, &after),
        vec![("k::helper".to_string(), "k::add_one".to_string())]
    );
}

#[test]
fn content_hash_sees_a_changed_callee() {
    let before = levels(vec![
        record("k::helper", "n0", "x + 1", &[]),
        record("k::f", "n0", "call {k::helper}(x)", &["k::helper"]),
    ]);
    let after = levels(vec![
        record("k::add_one", "n0", "x + 2", &[]),
        record("k::f", "n0", "call {k::add_one}(x)", &["k::add_one"]),
    ]);
    let renames = BTreeMap::from([("k::helper".to_string(), "k::add_one".to_string())]);
    let results = compare(&before, &after, &renames, &[]);
    // The caller text is the same after the rename: the call is the same call.
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n1".into(), proof: vec!["rename-map".into()] }
    );
    // The callee changed.
    assert!(matches!(status_of(&results, "k::add_one"), BodyStatus::Different { .. }));
    // Without the claim, the hashes differ, so the caller is not equal.
    let results = compare(&before, &after, &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }));
    assert!(suggest_renames(&before, &after).is_empty());
}

#[test]
fn rename_does_not_touch_longer_names() {
    let before = levels(vec![record("k::f", "n0", "call {k::foo_bar}(x)", &[])]);
    let after = levels(vec![record("k::f", "n0", "call {k::baz_bar}(x)", &[])]);
    let renames = BTreeMap::from([("k::foo".to_string(), "k::baz".to_string())]);
    let results = compare(&before, &after, &renames, &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }));
}

#[test]
fn levels_are_tried_in_order() {
    let before = levels(vec![
        record("k::f", "n0", "a", &[]),
        record("k::f", "n2", "same", &[]),
        record("k::f", "n3", "same", &[]),
        record("k::g", "n0", "a", &[]),
        record("k::g", "n2", "b", &[]),
        record("k::g", "n3", "same", &[]),
    ]);
    let after = levels(vec![
        record("k::f", "n0", "b", &[]),
        record("k::f", "n2", "same", &[]),
        record("k::f", "n3", "same", &[]),
        record("k::g", "n0", "c", &[]),
        record("k::g", "n2", "d", &[]),
        record("k::g", "n3", "same", &[]),
    ]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n2".into(), proof: vec![] }
    );
    assert_eq!(
        status_of(&results, "k::g"),
        &BodyStatus::Equal { level: "n3".into(), proof: vec![] }
    );
}

#[test]
fn different_reports_the_cfg_change() {
    let mut b = record("k::f", "n0", "a", &[]);
    let mut a = record("k::f", "n0", "b", &[]);
    let results = compare(&levels(vec![b.clone()]), &levels(vec![a.clone()]), &no_renames(), &[]);
    assert_eq!(status_of(&results, "k::f"), &BodyStatus::Different { cfg_changed: false });
    b.shape = "0:goto->1;1:return->".into();
    a.shape = "0:switch[0]->1,2;1:return->;2:return->".into();
    let results = compare(&levels(vec![b]), &levels(vec![a]), &no_renames(), &[]);
    assert_eq!(status_of(&results, "k::f"), &BodyStatus::Different { cfg_changed: true });
}

#[test]
fn added_bodies_are_not_reported() {
    let before = levels(vec![record("k::f", "n0", "a", &[])]);
    let after = levels(vec![
        record("k::f", "n0", "a", &[]),
        record("k::new", "n0", "x", &[]),
        record("k::new::{closure#0}", "n0", "y", &[]),
        record("k::other", "n0", "z", &[]),
    ]);
    let results = compare(&before, &after, &no_renames(), &["k::new".to_string()]);
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(status_of(&results, "k::other"), &BodyStatus::AfterOnly);
}

#[test]
fn duplicate_names_must_match_as_sets() {
    // A lib target and a test target with the same body name and different code.
    let before = levels(vec![record("k::f", "n0", "a", &[]), record("k::f", "n0", "b", &[])]);
    let same = levels(vec![record("k::f", "n0", "b", &[]), record("k::f", "n0", "a", &[])]);
    let other = levels(vec![record("k::f", "n0", "a", &[]), record("k::f", "n0", "c", &[])]);
    let results = compare(&before, &same, &no_renames(), &[]);
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n0".into(), proof: vec![] }
    );
    let results = compare(&before, &other, &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }));
}

#[test]
fn n4_proves_an_inverted_guard() {
    let term_before = ";rules \n#0 (L \"arg 1\")\n#1 (L \"arg 2\")\n#2 (ICmp \"lt\" #0 #1)\n#3 (L \"x\")\n#4 (L \"y\")\n#5 (Ite #2 #3 #4)";
    let term_after = ";rules path-fact\n#0 (L \"arg 1\")\n#1 (L \"arg 2\")\n#2 (ICmp \"ge\" #0 #1)\n#3 (L \"y\")\n#4 (L \"x\")\n#5 (Ite #2 #3 #4)";
    let mut b = record("k::f", "n2", "before", &[]);
    b.term = Some(term_before.into());
    let mut a = record("k::f", "n2", "after", &[]);
    a.term = Some(term_after.into());
    let results = compare(&levels(vec![b]), &levels(vec![a]), &no_renames(), &[]);
    match status_of(&results, "k::f") {
        BodyStatus::Equal { level, proof } => {
            assert_eq!(level, "n4");
            assert_eq!(proof[0], "path-fact");
            assert!(proof.contains(&"ite-not".to_string()), "{proof:?}");
        }
        other => panic!("not equal: {other:?}"),
    }
}

#[test]
fn n4_keeps_a_changed_constant() {
    let term = |constant: &str| {
        format!(
            ";rules \n#0 (L \"c\")\n#1 (L \"const u32 = 1\")\n#2 (L \"{constant}\")\n#3 (Ite #0 #1 #2)"
        )
    };
    let mut b = record("k::f", "n2", "before", &[]);
    b.term = Some(term("const u32 = 2"));
    let mut a = record("k::f", "n2", "after", &[]);
    a.term = Some(term("const u32 = 3"));
    let results = compare(&levels(vec![b]), &levels(vec![a]), &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }));
}

#[test]
fn records_round_trip_as_json_lines() {
    let mut first = record("k::f", "n0", "a\nb", &["k::g"]);
    first.span = Some(SpanInfo { file: "src/lib.rs".into(), line_start: 1, line_end: 3 });
    let text = format!(
        "{}\n\n{}\n",
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&first).unwrap()
    );
    let parsed = parse_records(&text).unwrap();
    assert_eq!(parsed, vec![first.clone(), first]);
    // The old driver format (name, level, text only) still parses.
    let old = parse_records("{\"name\":\"k::f\",\"level\":\"n0\",\"text\":\"a\"}").unwrap();
    assert_eq!(old[0].term, None);
}

#[test]
fn rename_to_an_existing_name_is_not_applied() {
    // The claim renames `k::a` to `k::b`, but a different `k::b` exists before.
    let before = levels(vec![
        record("k::a", "n0", "x + 1", &[]),
        record("k::b", "n0", "x + 2", &[]),
        record("k::f", "n0", "call {k::a}(x)", &["k::a"]),
    ]);
    let after = levels(vec![
        record("k::b", "n0", "x + 2", &[]),
        record("k::f", "n0", "call {k::b}(x)", &["k::b"]),
    ]);
    let renames = BTreeMap::from([("k::a".to_string(), "k::b".to_string())]);
    let results = compare(&before, &after, &renames, &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }), "{results:?}");
    assert_eq!(status_of(&results, "k::a"), &BodyStatus::BeforeOnly);
}

#[test]
fn swapped_names_are_applied() {
    let before = levels(vec![
        record("k::a", "n0", "x + 1", &[]),
        record("k::b", "n0", "x + 2", &[]),
        record("k::f", "n0", "call {k::a}(x) {k::b}", &["k::a", "k::b"]),
    ]);
    let after = levels(vec![
        record("k::b", "n0", "x + 1", &[]),
        record("k::a", "n0", "x + 2", &[]),
        record("k::f", "n0", "call {k::b}(x) {k::a}", &["k::a", "k::b"]),
    ]);
    let renames = BTreeMap::from([
        ("k::a".to_string(), "k::b".to_string()),
        ("k::b".to_string(), "k::a".to_string()),
    ]);
    let results = compare(&before, &after, &renames, &[]);
    assert!(
        results.iter().all(|result| matches!(result.status, BodyStatus::Equal { .. })),
        "{results:?}"
    );
}
