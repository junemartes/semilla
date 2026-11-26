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
        target: String::new(),
        externals: Vec::new(),
        indirect: Vec::new(),
        reach_types: Vec::new(),
        drop_types: Vec::new(),
        drop_calls: Vec::new(),
        impl_of: Vec::new(),
        drop_of: None,
        live_callees: None,
        value: false,
        sure_callees: None,
        sure_ends: None,
        param_types: Vec::new(),
        const_types: Vec::new(),
        config: Vec::new(),
        inlined: Vec::new(),
        adts: Vec::new(),
        names: Vec::new(),
        elements: None,
        graph: None,
        q: None,
        qx: None,
        cv: Vec::new(),
        sv: Vec::new(),
        inst: Vec::new(),
        dyn_casts: Vec::new(),
        unit: String::new(),
    }
}

fn in_target(mut record: BodyRecord, target: &str) -> BodyRecord {
    record.target = target.to_string();
    record
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
fn records_round_trip_as_json_lines() {
    let mut first = record("k::f", "n0", "a\nb", &["k::g"]);
    first.span = Some(SpanInfo { file: "src/lib.rs".into(), line_start: 1, line_end: 3, range: None });
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

#[test]
fn targets_keep_lib_and_test_apart() {
    // The lib and the test build of `k::f` swap their bodies: as sets the texts are the same,
    // but each target changed.
    let before = levels(vec![
        in_target(record("k::f", "n0", "a", &[]), "k.lib.1"),
        in_target(record("k::f", "n0", "b", &[]), "k.test.2"),
    ]);
    let after = levels(vec![
        in_target(record("k::f", "n0", "b", &[]), "k.lib.1"),
        in_target(record("k::f", "n0", "a", &[]), "k.test.2"),
    ]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }), "{results:?}");
    // The same bodies in the same targets are equal.
    let results = compare(&before, &before.clone(), &no_renames(), &[]);
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n0".into(), proof: vec![] }
    );
}

#[test]
fn missing_n3_records_use_n2_of_the_same_target() {
    // `k::f` has no inlined call in the lib build (no N3 record), but one in the test build.
    let before = levels(vec![
        in_target(record("k::f", "n0", "a0", &[]), "k.lib.1"),
        in_target(record("k::f", "n0", "t0", &[]), "k.test.2"),
        in_target(record("k::f", "n2", "a2", &[]), "k.lib.1"),
        in_target(record("k::f", "n2", "t2", &[]), "k.test.2"),
        in_target(record("k::f", "n3", "inlined", &[]), "k.test.2"),
    ]);
    // After: the lib build changed (a2 -> x2), the test build inlines to the same text.
    let after = levels(vec![
        in_target(record("k::f", "n0", "x0", &[]), "k.lib.1"),
        in_target(record("k::f", "n0", "u0", &[]), "k.test.2"),
        in_target(record("k::f", "n2", "x2", &[]), "k.lib.1"),
        in_target(record("k::f", "n2", "u2", &[]), "k.test.2"),
        in_target(record("k::f", "n3", "inlined", &[]), "k.test.2"),
    ]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }), "{results:?}");
    // If the lib build is the same, N3 is equal.
    let after = levels(vec![
        in_target(record("k::f", "n0", "x0", &[]), "k.lib.1"),
        in_target(record("k::f", "n0", "u0", &[]), "k.test.2"),
        in_target(record("k::f", "n2", "a2", &[]), "k.lib.1"),
        in_target(record("k::f", "n2", "u2", &[]), "k.test.2"),
        in_target(record("k::f", "n3", "inlined", &[]), "k.test.2"),
    ]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert_eq!(
        status_of(&results, "k::f"),
        &BodyStatus::Equal { level: "n3".into(), proof: vec![] }
    );
}

#[test]
fn a_body_that_moves_to_another_crate_is_equal() {
    // `move-item core::over to=app::over`: the body moves from the lib and test builds of `core`
    // to the bin and test builds of `app`. The targets differ, the text is the same.
    let text = "fn _(_1: &core::Account) -> bool { Gt(_1.1, const 10) }";
    let before = levels(vec![
        in_target(record("core::over", "n0", text, &[]), "core.lib.1"),
        in_target(record("core::over", "n0", text, &[]), "core.test.2"),
    ]);
    let after = |lib: &str, test: &str| {
        levels(vec![
            in_target(record("app::over", "n0", lib, &[]), "app.bin.3"),
            in_target(record("app::over", "n0", test, &[]), "app.test.4"),
        ])
    };
    let renames = BTreeMap::from([("core::over".to_string(), "app::over".to_string())]);
    let results = compare(&before, &after(text, text), &renames, &[]);
    assert_eq!(
        status_of(&results, "app::over"),
        &BodyStatus::Equal { level: "n0".into(), proof: vec![] }
    );
    // A changed body is different, also in one target only.
    let changed = "fn _(_1: &core::Account) -> bool { Ge(_1.1, const 10) }";
    for (lib, test) in [(changed, changed), (text, changed)] {
        let results = compare(&before, &after(lib, test), &renames, &[]);
        assert!(matches!(status_of(&results, "app::over"), BodyStatus::Different { .. }), "{results:?}");
    }
}

#[test]
fn a_new_hash_in_each_target_name_keeps_equal_bodies_equal() {
    // Design 37.12. A new package version gives each target a new hash. The debug text and the
    // release text of a test body are different, so the body has two texts on each side.
    let (debug, release) = ("fn _() -> u32 { AddWithOverflow(const 1, const 2) }", "fn _() -> u32 { Add(const 1, const 2) }");
    let side = |texts: [&str; 2], hashes: [&str; 2]| {
        levels(vec![
            in_target(record("k::tests::t", "n0", texts[0], &[]), &format!("k.test.{}", hashes[0])),
            in_target(record("k::tests::t", "n0", texts[1], &[]), &format!("k.test.{}", hashes[1])),
        ])
    };
    let before = side([debug, release], ["480693c09eb877f6", "351971ccb5242ec3"]);
    let after = side([debug, release], ["51634368b8893aa1", "d7a1077ee0ccfb6b"]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert_eq!(status_of(&results, "k::tests::t"), &BodyStatus::Equal { level: "n0".into(), proof: vec![] });
    // A changed text in one target is different.
    let changed = "fn _() -> u32 { Add(const 1, const 3) }";
    let after = side([debug, changed], ["51634368b8893aa1", "d7a1077ee0ccfb6b"]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::tests::t"), BodyStatus::Different { .. }), "{results:?}");
    // A target of a different kind does not pair.
    let after = levels(vec![
        in_target(record("k::tests::t", "n0", debug, &[]), "k.test.51634368b8893aa1"),
        in_target(record("k::tests::t", "n0", release, &[]), "k.lib.d7a1077ee0ccfb6b"),
    ]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::tests::t"), BodyStatus::Different { .. }), "{results:?}");
}

#[test]
fn the_source_position_of_a_test_is_not_behavior() {
    // `#[test]` makes a `TestDesc` const with the file, the line, and the column of the test. A
    // move above the test changes the line, not the behavior.
    let desc = |line: u32| test_desc(false, line, "");
    let before = levels(vec![record("k::tests::t", "n0", &desc(59), &[])]);
    let after = levels(vec![record("k::tests::t", "n0", &desc(64), &[])]);
    assert_eq!(status_of(&compare(&before, &after, &no_renames(), &[]), "k::tests::t"), &BodyStatus::Equal { level: "n0".into(), proof: vec![] });
    // Other constants still count.
    let other = |n: u32| format!("_5 = adt k::Desc fields {{k::Desc.start_line: const usize = {n}}};");
    let results = compare(&levels(vec![record("k::f", "n0", &other(1), &[])]), &levels(vec![record("k::f", "n0", &other(2), &[])]), &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }));
}

#[test]
fn the_settings_key_follows_the_outer_cargo_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let build = dir.path().join("repo/.semilla/cache/build");
    std::fs::create_dir_all(&build).unwrap();
    let before = cargo::settings_key(&build, None);
    std::fs::create_dir_all(dir.path().join("repo/.cargo")).unwrap();
    std::fs::write(dir.path().join("repo/.cargo/config.toml"), "[build]\nrustflags = [\"-Coverflow-checks=off\"]\n").unwrap();
    assert_ne!(cargo::settings_key(&build, None), before, "a parent cargo configuration is a build setting");
    // Each configuration file of the key is on the read list of the sandbox, and no other one.
    assert_eq!(cargo::outer_configs(&build).iter().filter(|file| file.starts_with(dir.path())).count(), 1);
}

#[test]
fn an_ignored_test_changes_the_test_descriptor() {
    // T5 (trial 2, A5d): `#[ignore]` sets the `ignore` field of the `TestDesc` const. The const
    // and the test function have one name. The N3 record of the function must not hide the N2
    // record of the const.
    let desc = |ignore: bool, line: u32| format!("fn _() -> test::TestDescAndFn {{\n{}\n}}", test_desc(ignore, line, ""));
    let body = "fn _() -> () {\n_0 = k::f() -> bb1;\n}";
    let inlined = "fn _() -> () {\n_0 = const ();\n}";
    let side = |ignore: bool, line: u32| {
        let mut records = Vec::new();
        for level in ["n0", "n2"] {
            records.push(in_target(record("k::tests::t", level, &desc(ignore, line), &[]), "k.test.1"));
            records.push(in_target(record("k::tests::t", level, body, &["k::f"]), "k.test.1"));
        }
        records.push(in_target(record("k::tests::t", "n3", inlined, &[]), "k.test.1"));
        levels(records)
    };
    let results = compare(&side(false, 10), &side(true, 11), &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::tests::t"), BodyStatus::Different { .. }), "{results:?}");
    // The position alone is not a change.
    let results = compare(&side(false, 10), &side(false, 11), &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::tests::t"), BodyStatus::Equal { .. }), "{results:?}");
    // Only the four position fields are masked: another `usize` constant on the line counts.
    let extra = |n: u32| test_desc(false, 1, &format!(", test::TestDesc.z_extra: const usize = {n}"));
    let results = compare(&levels(vec![record("k::tests::u", "n0", &extra(1), &[])]), &levels(vec![record("k::tests::u", "n0", &extra(2), &[])]), &no_renames(), &[]);
    assert!(matches!(status_of(&results, "k::tests::u"), BodyStatus::Different { .. }), "{results:?}");
}

#[test]
fn a_renamed_test_has_the_new_name_in_its_descriptor() {
    // The `TestName` string follows the rename map, so the rename of a test stays neutral after
    // the T5 fix (the descriptor is compared now).
    let hex = |text: &str| text.bytes().map(|b| format!("{b:02x}")).collect::<String>();
    let desc = |name: &str| format!("fn _() -> test::TestDescAndFn {{\n_1 = adt test::TestName variant test::TestName::StaticTestName fields {{test::TestName::StaticTestName.0: const &str = [0000 @0=memory [{}] align 1 Not] align 8 Mut}};\n{}\n}}", hex(name), test_desc(false, 1, ""));
    let before = levels(vec![record("k::tests::t", "n0", &desc("tests::t"), &[])]);
    let after = levels(vec![record("k::tests::u", "n0", &desc("tests::u"), &[])]);
    let renames: BTreeMap<String, String> = [("k::tests::t".to_string(), "k::tests::u".to_string())].into();
    let results = compare(&before, &after, &renames, &[]);
    assert!(matches!(status_of(&results, "k::tests::u"), BodyStatus::Equal { .. }), "{results:?}");
    // Another string in the name is not a path: the line stays.
    let other = "_1 = adt test::TestName variant test::TestName::StaticTestName fields {test::TestName::StaticTestName.0: x};";
    assert_eq!(test_name_as_path(other, "k::t"), other);
}

/// A `TestDesc` aggregate in the form of the driver (batch 5e: the fields by name, sorted).
fn test_desc(ignore: bool, line: u32, extra: &str) -> String {
    format!("_5 = adt test::TestDesc fields {{test::TestDesc.compile_fail: const bool = false, test::TestDesc.end_col: const usize = 29, test::TestDesc.end_line: const usize = {line}, test::TestDesc.ignore: const bool = {ignore}, test::TestDesc.ignore_message: _2, test::TestDesc.name: _1, test::TestDesc.no_run: const bool = false, test::TestDesc.should_panic: _3, test::TestDesc.source_file: const &str = [file], test::TestDesc.start_col: const usize = 8, test::TestDesc.start_line: const usize = {line}, test::TestDesc.test_type: _4{extra}}};")
}

#[test]
fn a_rename_sorts_the_named_operands_again() {
    // Batch 5e: the driver sorts the operands of an ADT aggregate by the field path. A field
    // rename can change the order, so the compare sorts them again after the rename map.
    let text = |a: &str, b: &str, first: &str| {
        let mut parts = [format!("k::Pair.{a}: {}", if first == a { "_1" } else { "_2" }), format!("k::Pair.{b}: {}", if first == a { "_2" } else { "_1" })];
        parts.sort();
        format!("fn _(_1: u64, _2: u64) -> k::Pair {{\n    bb0: {{\n        _0 = adt k::Pair fields {{{}}};\n        return;\n    }}\n}}\n", parts.join(", "))
    };
    // `a` → `z`: `{k::Pair.a: _1, k::Pair.b: _2}` becomes `{k::Pair.b: _2, k::Pair.z: _1}`.
    let before = levels(vec![record("k::make", "n0", &text("a", "b", "a"), &[])]);
    let after = levels(vec![record("k::make", "n0", &text("z", "b", "z"), &[])]);
    let renames: BTreeMap<String, String> = [("k::Pair.a".to_string(), "k::Pair.z".to_string())].into();
    assert!(matches!(status_of(&compare(&before, &after, &renames, &[]), "k::make"), BodyStatus::Equal { .. }));
    // The operands swap: not equal.
    let swapped = levels(vec![record("k::make", "n0", &text("z", "b", "b"), &[])]);
    assert!(matches!(status_of(&compare(&before, &swapped, &renames, &[]), "k::make"), BodyStatus::Different { .. }));
    // A nested list and a constant with brackets stay whole.
    assert_eq!(super::sort_aggregates("x = adt k::P fields {k::P.b: const &[u8] = [01, 02], k::P.a: f({1, 2})};"), "x = adt k::P fields {k::P.a: f({1, 2}), k::P.b: const &[u8] = [01, 02]};");
}

#[test]
fn a_variant_rename_sorts_the_switch_targets_again() {
    // Trial 4, D11: the driver sorts the named targets of a switch by the variant path. The
    // rename of a variant can change the order, so the compare sorts them again after the
    // rename map. The same rule applies to a discriminant table.
    let text = |arms: &str, table: &str| {
        format!("fn _(_1: &k::E) -> u8 {{\n    bb0: {{\n        _2 = discriminant((*_1));\n        _3 = discriminant((*_1)) cast of k::E values [{table}];\n        switchInt(_2) -> [{arms}, otherwise: bb9];\n    }}\n}}\n")
    };
    let before = levels(vec![record("k::f", "n0", &text("k::E::Frozen: bb8, k::E::Low: bb1", "k::E::Frozen = 1, k::E::Low = 0"), &[])]);
    let after = levels(vec![record("k::f", "n0", &text("k::E::Low: bb1, k::E::Stopped: bb8", "k::E::Low = 0, k::E::Stopped = 1"), &[])]);
    let renames: BTreeMap<String, String> = [("k::E::Frozen".to_string(), "k::E::Stopped".to_string())].into();
    assert!(matches!(status_of(&compare(&before, &after, &renames, &[]), "k::f"), BodyStatus::Equal { .. }));
    // The targets swap: not equal.
    let swapped = levels(vec![record("k::f", "n0", &text("k::E::Low: bb8, k::E::Stopped: bb1", "k::E::Low = 0, k::E::Stopped = 1"), &[])]);
    assert!(matches!(status_of(&compare(&before, &swapped, &renames, &[]), "k::f"), BodyStatus::Different { .. }));
    // The values of the table swap: not equal.
    let values = levels(vec![record("k::f", "n0", &text("k::E::Low: bb1, k::E::Stopped: bb8", "k::E::Low = 1, k::E::Stopped = 0"), &[])]);
    assert!(matches!(status_of(&compare(&before, &values, &renames, &[]), "k::f"), BodyStatus::Different { .. }));
    // `otherwise` stays last, and a switch with number targets keeps its order.
    assert_eq!(super::sort_variant_lists("switchInt(_2) -> [k::E::B: bb1, k::E::A: bb2, otherwise: bb3];"), "switchInt(_2) -> [k::E::A: bb2, k::E::B: bb1, otherwise: bb3];");
    assert_eq!(super::sort_variant_lists("switchInt(move _2) -> [10: bb1, 9: bb2, otherwise: bb3];"), "switchInt(move _2) -> [10: bb1, 9: bb2, otherwise: bb3];");
    // Batch 5x, part B: the entry `otherwise` does not make a number switch a named switch.
    let numbers = "switchInt(move _2) -> [1: bb1, 2: bb2, 10: bb3, otherwise: bb4];";
    assert_eq!(super::sort_variant_lists(numbers), numbers);
}

#[test]
fn a_caller_with_a_number_switch_is_equal_after_the_move_of_its_callee() {
    // Batch 5x, part B (trial 7 on humantime): the caller has no text edit. Its MIR differs
    // only by the path of the moved item, and it has a switch on numbers with `otherwise`.
    let text = |callee: &str| {
        format!("fn _(_1: u64) -> bool {{\n    bb0: {{\n        switchInt(_1) -> [1: bb1, 2: bb2, 10: bb3, 11: bb4, otherwise: bb5];\n    }}\n    bb1: {{\n        _0 = {callee}(_1);\n    }}\n}}\n")
    };
    let before = levels(vec![record("k::date::parse", "n0", &text("k::date::is_leap"), &[])]);
    let after = levels(vec![record("k::date::parse", "n0", &text("k::calendar::is_leap"), &[])]);
    let renames: BTreeMap<String, String> = [("k::date::is_leap".to_string(), "k::calendar::is_leap".to_string())].into();
    assert_eq!(status_of(&compare(&before, &after, &renames, &[]), "k::date::parse"), &BodyStatus::Equal { level: "n1".to_string(), proof: vec!["rename-map".to_string()] });
    // A different target of a number is a real change.
    let swapped = levels(vec![record("k::date::parse", "n0", &text("k::calendar::is_leap").replace("10: bb3, 11: bb4", "10: bb4, 11: bb3"), &[])]);
    assert!(matches!(status_of(&compare(&before, &swapped, &renames, &[]), "k::date::parse"), BodyStatus::Different { .. }));
}

#[test]
fn one_unit_gives_one_record_file() {
    // Design 28.8. A unit of a test build has two possible names: the compiler gets `--test`
    // only if the target has a test harness. A build unit of a different kind has one name.
    let stdout = [
        r#"{"reason":"compiler-artifact","target":{"name":"k-core","crate_types":["lib"]},"profile":{"test":false},"filenames":["/t/libk_core-0a.rmeta"]}"#,
        r#"{"reason":"compiler-artifact","target":{"name":"k-core","crate_types":["lib"]},"profile":{"test":true},"filenames":["/t/libk_core-0b.rmeta"]}"#,
        r#"{"reason":"compiler-artifact","target":{"name":"speed","crate_types":["bin"]},"profile":{"test":true},"filenames":["/t/libspeed-0c.rmeta"]}"#,
        r#"{"reason":"compiler-artifact","target":{"name":"build-script-build","kind":["custom-build"],"crate_types":["bin"]},"profile":{"test":false},"filenames":["/t/build/k-core-0d/build-script-build"]}"#,
    ]
    .join("\n");
    let names: Vec<String> = cargo::unit_files(&stdout).into_iter().collect();
    assert_eq!(names, ["k_core.lib.0a.jsonl", "k_core.lib.0b.jsonl", "k_core.test.0b.jsonl", "speed.bin.0c.jsonl", "speed.test.0c.jsonl"]);
    // The driver makes one file for each unit. Two files for one unit: Semilla uses none.
    let files = ["k_core.lib.0a.jsonl", "k_core.test.0b.jsonl", "speed.bin.0c.jsonl", "speed.test.0c.jsonl"].map(str::to_string).to_vec();
    let (single, double) = cargo::one_file_each(files);
    assert_eq!(single, ["k_core.lib.0a.jsonl", "k_core.test.0b.jsonl"]);
    assert_eq!(double, ["speed.bin.0c.jsonl", "speed.test.0c.jsonl"]);
}

#[test]
fn a_fresh_unit_with_no_record_file_needs_a_new_build() {
    // Design 37.11. Cargo did not compile the unit, and an earlier build with other unit names
    // removed its record file. The records of the unit are lost, so the caller builds again.
    let dir = tempfile::tempdir().unwrap();
    let (workspace, out) = (dir.path().join("build"), dir.path().join("records"));
    std::fs::create_dir_all(&out).unwrap();
    let unit = |fresh: bool, manifest: &str, kind: &str, file: &str| {
        format!(r#"{{"reason":"compiler-artifact","manifest_path":"{manifest}","target":{{"name":"k-core","kind":["{kind}"],"crate_types":["lib"]}},"profile":{{"test":false}},"filenames":["{file}"],"fresh":{fresh}}}"#)
    };
    let member = workspace.join("k-core/Cargo.toml").display().to_string();
    let fresh = unit(true, &member, "lib", "/t/libk_core-0a.rmeta");
    assert!(cargo::fresh_without_records(&fresh, &workspace, &out));
    // A unit that cargo compiled, a unit with its record file, a build script, and a crate
    // outside the build folder need no new build.
    assert!(!cargo::fresh_without_records(&unit(false, &member, "lib", "/t/libk_core-0a.rmeta"), &workspace, &out));
    assert!(!cargo::fresh_without_records(&unit(true, &member, "custom-build", "/t/build/k-core-0d/build-script-build"), &workspace, &out));
    assert!(!cargo::fresh_without_records(&unit(true, "/registry/k-core/Cargo.toml", "lib", "/t/libk_core-0a.rmeta"), &workspace, &out));
    std::fs::write(out.join("k_core.lib.0a.jsonl"), "").unwrap();
    assert!(!cargo::fresh_without_records(&fresh, &workspace, &out));
}

#[test]
fn the_cargo_error_text_has_the_first_error_with_its_cause() {
    // Trial 4, D14: with the JSON messages, the standard error of cargo has only the last line.
    // The cause is in a compiler message on the standard output.
    let stdout = [
        r#"{"reason":"compiler-artifact","filenames":["/t/libk-0123.rmeta"]}"#,
        r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused","rendered":"warning: unused variable\n"}}"#,
        r#"{"reason":"compiler-message","message":{"level":"error","message":"invalid character `.` in crate name: `notes.md`","rendered":"error: invalid character `.` in crate name: `notes.md`\n  |\n  = help: you can either pass `--crate-name`\n\n"}}"#,
        r#"{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 1 previous error","rendered":"error: aborting due to 1 previous error\n\n"}}"#,
        r#"{"reason":"build-finished","success":false}"#,
    ]
    .join("\n");
    let stderr = "error: could not compile `ledger_core` (test \"notes.md\") due to 1 previous error\n";
    let text = cargo::error_text(&stdout, stderr);
    assert!(text.starts_with("error: invalid character `.` in crate name: `notes.md`\n"), "{text}");
    assert!(text.contains("= help: you can either pass") && text.contains("could not compile `ledger_core`"), "{text}");
    assert!(!text.contains("unused variable") && !text.contains("aborting"), "{text}");
    // With no JSON message, the text is the `error` lines of the standard error.
    assert_eq!(cargo::error_text("", stderr).trim_end(), stderr.trim_end());
}

#[test]
fn an_opaque_body_is_not_an_input_of_a_proof() {
    // Batch 5l: the build configuration selects `k::add_one`. The content hash does not use its
    // text, so the call of a different path is a different call.
    let before = levels(vec![record("k::helper", "n0", "x + 1", &[]), record("k::f", "n0", "call {k::helper}(x)", &["k::helper"])]);
    let after = levels(vec![record("k::add_one", "n0", "x + 1", &[]), record("k::f", "n0", "call {k::add_one}(x)", &["k::add_one"])]);
    let marks = |opaque: &[&str], stop: &[&str]| Marks { opaque: opaque.iter().map(|name| name.to_string()).collect(), stop: stop.iter().map(|name| name.to_string()).collect() };
    let results = compare_with(&before, &after, &no_renames(), &[], &Marks::default(), &marks(&["k::add_one"], &[]));
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }), "{results:?}");
    let results = compare_with(&before, &after, &no_renames(), &[], &marks(&["k::helper"], &[]), &Marks::default());
    assert!(matches!(status_of(&results, "k::f"), BodyStatus::Different { .. }), "{results:?}");

    // An N3 record with an opaque callee inlined, or with a callee that has no N0 record, is not
    // used. The N3 record of `k::h` stays.
    let mut inlined = record("k::f", "n3", "x + 1", &[]);
    inlined.inlined = vec!["k::add_one".to_string()];
    let mut unknown = record("k::g", "n3", "x + 1", &[]);
    unknown.inlined = vec!["k::no_record".to_string()];
    let mut free = record("k::h", "n3", "x + 1", &[]);
    free.inlined = vec!["k::f".to_string()];
    let mut all = levels(vec![record("k::add_one", "n0", "x + 1", &[]), record("k::f", "n0", "call {k::add_one}(x)", &["k::add_one"]), inlined, unknown, free]);
    drop_opaque_inlines(&mut all, &marks(&["k::add_one"], &[]));
    assert_eq!(all["n3"].iter().map(|record| record.name.as_str()).collect::<Vec<_>>(), vec!["k::h"]);

    // A body that gets no proof: equal only with the same N0 text.
    let before = levels(vec![record("k::f", "n0", "a", &[]), record("k::f", "n2", "c", &[]), record("k::g", "n0", "a", &[])]);
    let after = levels(vec![record("k::f", "n0", "b", &[]), record("k::f", "n2", "c", &[]), record("k::g", "n0", "a", &[])]);
    let results = compare(&before, &after, &no_renames(), &[]);
    assert_eq!(status_of(&results, "k::f"), &BodyStatus::Equal { level: "n2".into(), proof: Vec::new() });
    let results = compare_with(&before, &after, &no_renames(), &[], &Marks::default(), &marks(&[], &["k::f", "k::g"]));
    assert_eq!(status_of(&results, "k::f"), &BodyStatus::Different { cfg_changed: true });
    assert_eq!(status_of(&results, "k::g"), &BodyStatus::Equal { level: "n0".into(), proof: Vec::new() });
}

/// Batch 5t, part A: the first line of a record file is the record of the compile unit.
#[test]
fn a_record_file_starts_with_the_unit_record() {
    let dir = tempfile::tempdir().unwrap();
    let body = serde_json::to_string(&record("demo::f", "n0", "bb0: return", &[])).unwrap();
    let unit = r#"{"unit":{"target":"old name","package":"demo","folder":"","cfg":["debug_assertions","feature=\"std\""],"files":["src/lib.rs"],"root":"src/lib.rs","triple":"x86_64-unknown-linux-gnu"}}"#;
    let text = format!("{unit}\n{body}\n");
    // The unit line is not a body record.
    let records = parse_records(&text).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].name, "demo::f");
    std::fs::write(dir.path().join("demo.lib.1.jsonl"), &text).unwrap();
    let units = read_units(dir.path()).unwrap();
    assert_eq!(units.len(), 1);
    // The name of the file is the name of the unit.
    assert_eq!(units[0].target, "demo.lib.1");
    assert!(units[0].cfg.contains("feature=\"std\"") && units[0].files.contains("src/lib.rs"));
    assert_eq!(units[0].root, "src/lib.rs");
    // A record file of an older driver has no unit record: the caller gets no list. Then no
    // condition is covered.
    std::fs::write(dir.path().join("demo.test.2.jsonl"), format!("{body}\n")).unwrap();
    assert_eq!(read_units(dir.path()), None);
    assert_eq!(read_units(&dir.path().join("none")), None);
}

// Batch 7c, part 2 (design 37.4): a rename of a method of a generic impl.

#[test]
fn a_rename_key_matches_a_body_name_with_generic_arguments() {
    let body = "fn allocate(_1: &mut Map<V>) -> () { return; }";
    let caller = |callee: &str, other: &str| format!("_2 = {{k::Map::<V>::{callee}}}(move _1); _3 = {{k::Map::<u32>::{callee}}}(move _4); _5 = {{k::Map::<V>::{other}}}(move _1);");
    let before = levels(vec![
        record("k::Map::<V>::allocate", "n0", body, &[]),
        record("k::Map::<V>::insert", "n0", &caller("allocate", "len"), &["k::Map::<V>::allocate"]),
        record("k::Map::<V>::len", "n0", "return;", &[]),
    ]);
    let after = levels(vec![
        record("k::Map::<V>::allocate_x", "n0", body, &[]),
        record("k::Map::<V>::insert", "n0", &caller("allocate_x", "len"), &["k::Map::<V>::allocate_x"]),
        record("k::Map::<V>::len", "n0", "return;", &[]),
    ]);
    let renames = BTreeMap::from([("k::Map::allocate".to_string(), "k::Map::allocate_x".to_string())]);
    let results = compare(&before, &after, &renames, &[]);
    let equal = BodyStatus::Equal { level: "n1".into(), proof: vec!["rename-map".into()] };
    assert_eq!(status_of(&results, "k::Map::<V>::allocate_x"), &BodyStatus::Equal { level: "n0".into(), proof: vec![] }, "{results:?}");
    // The call with other generic arguments (`Map::<u32>`) is a call of the same function.
    assert_eq!(status_of(&results, "k::Map::<V>::insert"), &equal, "{results:?}");
    assert!(results.iter().all(|result| matches!(result.status, BodyStatus::Equal { .. })), "{results:?}");

    // A rename with a real change of the body keeps the result "different".
    let changed = levels(vec![
        record("k::Map::<V>::allocate_x", "n0", "fn allocate(_1: &mut Map<V>) -> () { _2 = const 1_u8; return; }", &[]),
        record("k::Map::<V>::insert", "n0", &caller("allocate_x", "len"), &["k::Map::<V>::allocate_x"]),
        record("k::Map::<V>::len", "n0", "return;", &[]),
    ]);
    let results = compare(&before, &changed, &renames, &[]);
    assert!(matches!(status_of(&results, "k::Map::<V>::allocate_x"), BodyStatus::Different { .. }), "{results:?}");

    // Two bodies fit one key: the match is not unique, and Semilla does not use the key.
    let twins = |name: &str| {
        levels(vec![
            record(&format!("k::Map::<u8>::{name}"), "n0", body, &[]),
            record("k::Map::<u16>::allocate", "n0", body, &[]),
        ])
    };
    let results = compare(&twins("allocate"), &twins("allocate_x"), &renames, &[]);
    assert_eq!(status_of(&results, "k::Map::<u8>::allocate"), &BodyStatus::BeforeOnly, "{results:?}");
    assert_eq!(status_of(&results, "k::Map::<u8>::allocate_x"), &BodyStatus::AfterOnly, "{results:?}");

    // The new name with generic arguments is there before the step: the rename is not usable.
    let mut taken = before.clone();
    taken.get_mut("n0").unwrap().push(record("k::Map::<V>::allocate_x", "n0", "return;", &[]));
    let results = compare(&taken, &after, &renames, &[]);
    assert_eq!(status_of(&results, "k::Map::<V>::allocate"), &BodyStatus::BeforeOnly, "{results:?}");

    // A move to a different module keeps the list at the type: the same distance from the end.
    let moved = levels(vec![
        record("k::m::Map::<V>::allocate_x", "n0", body, &[]),
    ]);
    let single = levels(vec![record("k::Map::<V>::allocate", "n0", body, &[])]);
    let renames = BTreeMap::from([("k::Map::allocate".to_string(), "k::m::Map::allocate_x".to_string())]);
    let results = compare(&single, &moved, &renames, &[]);
    assert_eq!(status_of(&results, "k::m::Map::<V>::allocate_x"), &BodyStatus::Equal { level: "n0".into(), proof: vec![] }, "{results:?}");
}

#[test]
fn a_generic_key_matches_only_a_whole_path_with_a_list() {
    let pairs = [(format!("{}a::T::m", paths::GENERIC_KEY), "a::T::n".to_string())];
    let rename = |text: &str| paths::rename(text, &pairs);
    assert_eq!(rename("{a::T::<A, B>::m}(x)"), "{a::T::<A, B>::n}(x)");
    assert_eq!(rename("a::T::<fn(u8) -> Vec<u8>>::m::{closure#0}"), "a::T::<fn(u8) -> Vec<u8>>::n::{closure#0}");
    // No list: the plain key is the key for this text.
    assert_eq!(rename("a::T::m"), "a::T::m");
    // A longer name, a longer path before, and a different segment do not match.
    assert_eq!(rename("a::T::<A>::m2"), "a::T::<A>::m2");
    assert_eq!(rename("b::a::T::<A>::m"), "b::a::T::<A>::m");
    assert_eq!(rename("a::T::<A>::x::m"), "a::T::<A>::x::m");
    assert_eq!(paths::generic_prefix("a::T::<A>::m::{closure#0}", "a::T::m"), Some("a::T::<A>::m"));
    assert_eq!(paths::generic_prefix("a::T::<A>::mm", "a::T::m"), None);
}

#[test]
fn two_body_names_that_differ_only_in_a_lifetime_are_one_body() {
    // Design 37.2: the header of the impl lost its lifetime.
    let old = "<&'a mut a::Node as a::Parse<a::Attrs>>::parse";
    let new = "<&mut a::Node as a::Parse<a::Attrs>>::parse";
    assert_eq!(paths::no_lifetimes(old), new);
    assert_eq!(paths::no_lifetimes("<(dyn a::Shape + 'static)>::area"), "<(dyn a::Shape)>::area");
    assert_eq!(paths::no_lifetimes("a::T::<'a, B>::m"), "a::T::<B>::m");
    assert_eq!(paths::no_lifetimes("a::T::<'x'>::m"), "a::T::<'x'>::m");
    assert_eq!(paths::no_lifetimes("a::Walk::<'a>::len"), "a::Walk::len");
    assert_eq!(paths::no_lifetimes("<a::Walk<'a> as a::P<B, 'b>>::f"), "<a::Walk as a::P<B>>::f");
    let before = levels(vec![record(old, "n0", "body", &[]), record("<(dyn a::Shape + 'a)>::area", "n0", "one", &[])]);
    let after = levels(vec![record(new, "n0", "body", &[]), record("<(dyn a::Shape + 'static)>::area", "n0", "two", &[])]);
    let results = compare_with(&before, &after, &BTreeMap::new(), &[], &Marks::default(), &Marks::default());
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(status_of(&results, new), &BodyStatus::Equal { level: "n0".into(), proof: vec![] });
    // A body with a change stays different.
    assert!(matches!(status_of(&results, "<(dyn a::Shape + 'static)>::area"), BodyStatus::Different { .. }));
    // Two names of one tree fit: no pair.
    let before = levels(vec![record("<&'a a::Node as a::P>::f", "n0", "body", &[]), record("<&'b a::Node as a::P>::f", "n0", "body", &[])]);
    let after = levels(vec![record("<&a::Node as a::P>::f", "n0", "body", &[])]);
    let results = compare_with(&before, &after, &BTreeMap::new(), &[], &Marks::default(), &Marks::default());
    assert_eq!(status_of(&results, "<&'a a::Node as a::P>::f"), &BodyStatus::BeforeOnly);
    assert_eq!(status_of(&results, "<&a::Node as a::P>::f"), &BodyStatus::AfterOnly);
}

/// Batch 7d, part 5.2 (design 38.10): an impl with new names of its generic parameters and a
/// renamed method. The rename map has the generic keys of the new impl.
#[test]
fn a_rename_in_an_impl_with_new_parameter_names_is_equal() {
    let get = "fn _(_1: &k::W<T>) -> T generics <T#0> { _0 = ((*_1).k::W.0: T); }";
    let call = |name: &str| format!("fn _(_1: &k::W<u32>) -> u32 {{ _3 = const fn(&k::W<u32>) -> u32 {{k::W::<u32>::{name}}}(_1); }}");
    let before = levels(vec![record("k::W::<T>::get", "n0", get, &[]), record("k::twice", "n0", &call("get"), &["k::W::<T>::get"])]);
    let value = get.replace('T', "U");
    let after = |value: &str| levels(vec![record("k::W::<U>::value", "n0", value, &[]), record("k::twice", "n0", &call("value"), &["k::W::<U>::value"])]);
    let renames: BTreeMap<String, String> = [("k::W::get".to_string(), "k::W::value".to_string())].into();
    let results = compare(&before, &after(&value), &renames, &[]);
    assert!(matches!(status_of(&results, "k::W::<U>::value"), BodyStatus::Equal { .. }), "{results:?}");
    assert!(matches!(status_of(&results, "k::twice"), BodyStatus::Equal { .. }), "{results:?}");
    // A needle in the renamed body: different.
    let results = compare(&before, &after(&value.replace(".0: U", ".1: U")), &renames, &[]);
    assert!(matches!(status_of(&results, "k::W::<U>::value"), BodyStatus::Different { .. }), "{results:?}");
    // A new name that the old body has as a word: no new names, so the compare stays different.
    let both = "fn _(_1: &k::W<T>, _2: U) -> T generics <T#0> { _0 = ((*_1).k::W.0: T); }";
    let before = levels(vec![record("k::W::<T>::get", "n0", both, &[]), record("k::twice", "n0", &call("get"), &["k::W::<T>::get"])]);
    let results = compare(&before, &after(&both.replace('T', "U")), &renames, &[]);
    assert!(matches!(status_of(&results, "k::W::<U>::value"), BodyStatus::Different { .. } | BodyStatus::BeforeOnly | BodyStatus::AfterOnly), "{results:?}");
}

fn at_file(mut record: BodyRecord, file: &str) -> BodyRecord {
    record.span = Some(SpanInfo { file: file.to_string(), line_start: 1, line_end: 3, range: None });
    record
}

/// Design 61.14 (R61.14-1): two packages each have a test target `integration`, so one body
/// name is two source bodies. Each one has its own comparison with its file: a change of one
/// never gives the other a status. A lib and its test build (one file) stay one body.
// rule+ R61.14-1
// rule- R61.14-1
#[test]
fn a_body_name_of_two_source_files_gives_one_comparison_for_each() {
    let side = |a: &str, b: &str| {
        levels(vec![
            at_file(in_target(record("integration::suite", "n0", a, &[]), "integration.test.1111111111111111"), "tests/lib.rs"),
            at_file(in_target(record("integration::suite", "n0", b, &[]), "integration.test.2222222222222222"), "core/tests/lib.rs"),
        ])
    };
    let results = compare_with(&side("a", "b"), &side("a", "c"), &no_renames(), &[], &Marks::default(), &Marks::default());
    let found: Vec<(Option<&str>, bool)> = results.iter().map(|result| (result.place.as_deref(), matches!(result.status, BodyStatus::Equal { .. }))).collect();
    assert_eq!(found, [(Some("core/tests/lib.rs"), false), (Some("tests/lib.rs"), true)], "{results:?}");
    // Each source body has the graphs of its own target only.
    assert!(results.iter().all(|result| result.graphs.len() == 1), "{results:?}");
    // The negative case: a lib and its test build have one file, so they are one body.
    let one = |a: &str, b: &str| {
        levels(vec![
            at_file(in_target(record("k::f", "n0", a, &[]), "k.lib.1111111111111111"), "src/lib.rs"),
            at_file(in_target(record("k::f", "n0", b, &[]), "k.test.2222222222222222"), "src/lib.rs"),
        ])
    };
    let results = compare_with(&one("a", "a"), &one("a", "b"), &no_renames(), &[], &Marks::default(), &Marks::default());
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(results[0].place.is_none() && matches!(results[0].status, BodyStatus::Different { .. }), "{results:?}");
}

/// Design 61.16 (R61.16-2): the graph pairs of a body do not depend on the hash in the target
/// names (it depends on the folders of the build). Two targets of one name without the hash
/// pair in the order of their texts. Two such targets with different texts on one side, and
/// new hashes on the other side, give one pair that is not compared: Semilla does not choose
/// one of them. The negative case: equal texts on each side give a pair for each target.
// rule+ R61.16-2
// rule- R61.16-2
#[test]
fn the_graph_pairs_of_a_body_do_not_depend_on_the_hash_of_a_target() {
    let side = |targets: [(&str, &str); 2]| levels(targets.iter().map(|(target, text)| at_file(in_target(record("k::f", "n0", text, &[]), target), "src/lib.rs")).collect());
    let texts = |results: &[BodyComparison], one: [(&str, &str); 2]| -> Vec<String> {
        let map: BTreeMap<&str, &str> = one.into_iter().collect();
        results[0].graphs.iter().map(|pair| pair.old_key.as_ref().map(|(_, target)| map[target.as_str()].to_string()).unwrap_or_default()).collect()
    };
    let (h1, h2) = ("k.lib.1111111111111111", "k.lib.2222222222222222");
    for one in [[(h1, "a"), (h2, "b")], [(h1, "b"), (h2, "a")]] {
        let results = compare_with(&side(one), &side(one), &no_renames(), &[], &Marks::default(), &Marks::default());
        assert_eq!(texts(&results, one), ["a", "b"], "{results:?}");
    }
    let (h3, h4) = ("k.lib.3333333333333333", "k.lib.4444444444444444");
    let results = compare_with(&side([(h1, "a"), (h2, "b")]), &side([(h3, "a"), (h4, "b")]), &no_renames(), &[], &Marks::default(), &Marks::default());
    assert_eq!(results[0].graphs.len(), 1, "{results:?}");
    assert!(results[0].graphs[0].not_compared && results[0].graphs[0].old_key.is_none(), "{results:?}");
    // The negative cases: one text on each side; the build settings of the units (a library
    // with `panic="unwind"` and with `panic="abort"`) pair the targets; a body of one side only
    // has a pair with one side for each target.
    let results = compare_with(&side([(h1, "a"), (h2, "a")]), &side([(h3, "a"), (h4, "a")]), &no_renames(), &[], &Marks::default(), &Marks::default());
    assert_eq!(results[0].graphs.len(), 2, "{results:?}");
    assert!(results[0].graphs.iter().all(|pair| !pair.not_compared && pair.old_key.is_some() && pair.new_key.is_some()), "{results:?}");
    let units = |targets: [(&str, &str, &str); 2]| levels(targets.iter().map(|(target, text, unit)| BodyRecord { unit: unit.to_string(), ..at_file(in_target(record("k::f", "n0", text, &[]), target), "src/lib.rs") }).collect());
    let results = compare_with(&units([(h1, "a", "panic=\"abort\""), (h2, "b", "panic=\"unwind\"")]), &units([(h3, "b2", "panic=\"unwind\""), (h4, "a2", "panic=\"abort\"")]), &no_renames(), &[], &Marks::default(), &Marks::default());
    let keys: Vec<(String, String)> = results[0].graphs.iter().map(|pair| (pair.old_key.clone().unwrap_or_default().1, pair.new_key.clone().unwrap_or_default().1)).collect();
    assert_eq!(keys, [(h1.to_string(), h4.to_string()), (h2.to_string(), h3.to_string())], "{results:?}");
    let results = compare_with(&side([(h1, "a"), (h2, "b")]), &levels(Vec::new()), &no_renames(), &[], &Marks::default(), &Marks::default());
    assert_eq!(results[0].graphs.len(), 2, "{results:?}");
    assert!(results[0].graphs.iter().all(|pair| !pair.not_compared && pair.old_key.is_some() && !pair.new_body), "{results:?}");
}
