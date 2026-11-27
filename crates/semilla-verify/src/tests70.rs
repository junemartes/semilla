//! The tests of batch 11b (design 70): the names and the identity come from the compiler.

use crate::records::testing::{TestDef, give};

fn spotlights(before: &str, after: &str, records: Option<(&[TestDef], &[TestDef])>) -> Vec<String> {
    let (_old_dir, old) = crate::tests::load(before);
    let (_new_dir, new) = crate::tests::load(after);
    if let Some((old_defs, new_defs)) = records {
        give(&old, old_defs, &[]);
        give(&new, new_defs, &[]);
    }
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    crate::spotlight(&edits, &old, &new)
}

const PLAIN: &str = "pub fn inc(n: u32) -> u32 {\n    n + 1\n}\n";
const DOC_TEST: &str = "pub fn inc(n: u32) -> u32 {\n    n + 1\n}\n\n/// It is not unsafe.\n///\n/// ```\n/// # #[cfg(feature = \"x\")]\n/// assert_eq!(demo::inc2(1), 3);\n/// ```\npub fn inc2(n: u32) -> u32 {\n    // unsafe #[cfg(unix)]\n    n + 2\n}\n";
const UNSAFE_BLOCK: &str = "pub fn inc(n: u32) -> u32 {\n    n + 1\n}\n\npub fn inc2(p: *const u32) -> u32 {\n    unsafe { *p }\n}\n";
const CFG_ITEM: &str = "pub fn inc(n: u32) -> u32 {\n    n + 1\n}\n\n#[cfg(unix)]\npub fn inc2(n: u32) -> u32 {\n    n + 2\n}\n";

// rule- R70.2-1
#[test]
fn a_doc_test_and_a_comment_with_the_words_give_no_spotlight_with_the_record() {
    let found = spotlights(PLAIN, DOC_TEST, Some((&[TestDef { item: "demo::inc", ..TestDef::default() }], &[TestDef { item: "demo::inc", ..TestDef::default() }, TestDef { item: "demo::inc2", ..TestDef::default() }])));
    assert!(found.iter().all(|line| !line.starts_with("unsafe code") && !line.starts_with("cfg attribute")), "{found:?}");
}

// rule+ R70.2-1
#[test]
fn the_record_of_an_unsafe_block_gives_the_spotlight() {
    let (_old_dir, old) = crate::tests::load(PLAIN);
    let (_new_dir, new) = crate::tests::load(UNSAFE_BLOCK);
    give(&old, &[TestDef { item: "demo::inc", ..TestDef::default() }], &[]);
    // The record marks the item `unsafe` (here: the item place has the unsafe state).
    give(&new, &[TestDef { item: "demo::inc", ..TestDef::default() }, TestDef { item: "demo::inc2", unsafe_def: true, ..TestDef::default() }], &[]);
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    let found = crate::spotlight(&edits, &old, &new);
    assert!(found.iter().any(|line| line == "unsafe code: demo::inc2"), "{found:?}");
    // The record of the built code has no `unsafe`, and the tokens have it: the line stays
    // with the cause (the safe side).
    give(&new, &[TestDef { item: "demo::inc", ..TestDef::default() }, TestDef { item: "demo::inc2", ..TestDef::default() }], &[]);
    let found = crate::spotlight(&edits, &old, &new);
    assert!(found.iter().any(|line| line.starts_with("unsafe code: demo::inc2 (the code has the keyword `unsafe`; the compiler did not build")), "{found:?}");
}

// rule+ R70.2-1
#[test]
fn a_cfg_attribute_of_the_tokens_that_the_record_does_not_have_keeps_the_line_with_the_cause() {
    let found = spotlights(PLAIN, CFG_ITEM, Some((&[TestDef { item: "demo::inc", ..TestDef::default() }], &[TestDef { item: "demo::inc", ..TestDef::default() }, TestDef { item: "demo::inc2", ..TestDef::default() }])));
    assert!(found.iter().any(|line| line.starts_with("cfg attribute: demo::inc2 (the code has a `cfg` attribute that the compiler did not evaluate")), "{found:?}");
}

// rule+ R70.1-1
#[test]
fn a_tree_with_no_record_uses_the_tokens() {
    // No record: the token rule of R68.1-1 decides (no line for a doc test and a comment, a
    // line for the tokens of a `cfg` attribute).
    assert!(spotlights(PLAIN, DOC_TEST, None).iter().all(|line| !line.starts_with("unsafe code") && !line.starts_with("cfg attribute")));
    assert!(spotlights(PLAIN, CFG_ITEM, None).iter().any(|line| line == "cfg attribute: demo::inc2"));
    assert!(spotlights(PLAIN, UNSAFE_BLOCK, None).iter().any(|line| line == "unsafe code: demo::inc2"));
}

// rule- R70.1-1
#[test]
fn the_line_of_the_names_shows_only_for_a_step_with_no_record() {
    let mut report = semilla_core::StepReport::default();
    assert!(report.expansion_section(false).iter().all(|line| !line.starts_with("names from the text")));
    report.names_gap = Some("the records of the compiler have no name record (a record of an earlier driver)".into());
    assert_eq!(report.expansion_section(false), ["names from the text: the records of the compiler have no name record (a record of an earlier driver)"]);
}

const UNSAFE_WORDS: &str = "pub fn inc(n: u32) -> u32 {\n    // This is not unsafe.\n    let _ = \"unsafe\";\n    n + 2\n}\n";

// rule+ R70.2-1
// rule- R70.2-1
#[test]
fn the_unsafe_state_of_a_body_comes_from_the_record() {
    let (_dir, ws) = crate::tests::load(UNSAFE_WORDS);
    give(&ws, &[TestDef { item: "demo::inc", ..TestDef::default() }], &[]);
    assert_eq!(crate::records::item_unsafe(&ws, "demo::inc"), Some(false));
    give(&ws, &[TestDef { item: "demo::inc", unsafe_def: true, ..TestDef::default() }], &[]);
    assert_eq!(crate::records::item_unsafe(&ws, "demo::inc"), Some(true));
    // No record: no answer (the tokens decide).
    let (_dir, ws) = crate::tests::load(UNSAFE_WORDS);
    assert_eq!(crate::records::item_unsafe(&ws, "demo::inc"), None);
}

const LOCAL_NAME: &str = "pub type Block = usize;\n\npub fn width(b: Block) -> Block {\n    let wide: Block = b * 2;\n    wide + 1\n}\n";

// rule+ R70.2-1
#[test]
fn a_use_that_the_compiler_resolves_to_built_code_gives_no_cfg_note() {
    let (_dir, ws) = crate::tests::load(LOCAL_NAME);
    let text = ws.rust_files["src/lib.rs"].clone();
    let at = |word: &str, nth: usize| text.match_indices(word).nth(nth).map(|(at, _)| at).unwrap() as i64;
    let item = &ws.items["demo::width"];
    let (_, start, end) = crate::records::item_place(&ws, item).unwrap();
    let (alias_start, alias_end) = crate::records::item_place(&ws, &ws.items["demo::Block"]).map(|(_, s, e)| (s, e)).unwrap();
    let record: semilla_core::namerec::Record = serde_json::from_value(serde_json::json!({
        "f": ["src/lib.rs"],
        "d": [
            {"p": "demo::width", "n": "width", "k": "function", "s": [0, start, end], "i": [0, start, end]},
            {"p": "demo::Block", "n": "Block", "k": "type alias", "s": [0, alias_start, alias_end], "i": [0, alias_start, alias_end]}
        ],
        "p": [[0, at("wide", 0), at("wide", 0) + 4, -1], [0, at("wide", 1), at("wide", 1) + 4, -1],
              [0, at("Block", 1), at("Block", 1) + 5, 1], [0, at("Block", 2), at("Block", 2) + 5, 1], [0, at("Block", 3), at("Block", 3) + 5, 1]],
        "c": [{"r": [0, 0, 0], "v": false, "k": "cfg", "t": "target_feature = \"avx512f\"", "s": "wide"},
              {"r": [0, 0, 0], "v": false, "k": "cfg", "t": "target_feature = \"avx512f\"", "s": "Block"}]
    }))
    .unwrap();
    ws.names.set(Some(std::sync::Arc::new(semilla_core::namerec::NameIndex::new([("demo.lib.aaaa", &record)]))));
    // `wide` is a local binding; `Block` is the alias that the configuration builds.
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::width", "`wide` (under `#[cfg(target_feature=\"avx512f\")]`)"), None);
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::width", "`Block` (2 definitions, one under `#[cfg(`)"), None);
    // A definition that the driver names by its path: the compiler resolved it in this
    // configuration.
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::width", "`demo::simd::Block` (an item under `cfg`)"), None);
    // A `cfg` attribute or a macro call of the body itself stays.
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::width", "`cfg!(unix)`").as_deref(), Some("`cfg!(unix)`"));
}

// rule- R70.2-1
#[test]
fn a_use_that_the_record_does_not_resolve_or_with_no_record_keeps_the_note() {
    let (_dir, ws) = crate::tests::load(LOCAL_NAME);
    let entry = "`wide` (under `#[cfg(x)]`)";
    // No record: the text rule (R70.1-1).
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::width", entry).as_deref(), Some(entry));
    // A record with no use of the name: the text has more uses than the record, and the note
    // says so.
    give(&ws, &[TestDef { item: "demo::width", ..TestDef::default() }], &["wide"]);
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::width", entry).as_deref(), Some("`wide` (under `#[cfg(x)]`; the record of the compiler does not resolve each use of the name)"));
}

const HELPER_OLD: &str = "#[derive(Debug)]\n#[named(x)]\n#[derive(Clone)]\npub struct Item {\n    pub a: u8,\n}\n";
const HELPER_NEW: &str = "#[derive(Debug)]\n#[derive(Clone)]\n#[named(x)]\npub struct Item {\n    pub a: u8,\n}\n";

// rule+ R70.2-1
// rule- R70.2-1
#[test]
fn an_attribute_that_the_compiler_keeps_on_the_item_is_no_macro() {
    let old = "#[derive(Debug)] #[named(x)] #[derive(Clone)]";
    let new = "#[derive(Debug)] #[derive(Clone)] #[named(x)]";
    // With no record, the name list cannot tell a derive helper from an attribute macro.
    assert!(crate::facts::attribute_macro_moves(old, new, None).is_some());
    // The compiler keeps `named` on the item: a derive helper, no macro call.
    let kept: std::collections::BTreeSet<String> = ["named".to_string()].into();
    assert!(crate::facts::attribute_macro_moves(old, new, Some(&kept)).is_none());
    // A record with no such attribute: the attribute was a macro call (the compiler took it
    // from the item).
    assert!(crate::facts::attribute_macro_moves(old, new, Some(&Default::default())).is_some());
    // The record of the item gives the set.
    let (_old_dir, old_ws) = crate::tests::load(HELPER_OLD);
    let (_new_dir, new_ws) = crate::tests::load(HELPER_NEW);
    give(&old_ws, &[TestDef { item: "demo::Item", inert: &["named"], ..TestDef::default() }], &[]);
    give(&new_ws, &[TestDef { item: "demo::Item", inert: &["named"], ..TestDef::default() }], &[]);
    let anchor = semilla_core::Anchor::item("demo::Item");
    assert_eq!(crate::records::inert_attributes([&old_ws, &new_ws], [Some(&anchor), Some(&anchor)]), Some(kept));
}

const TRAIT_IMPL_OLD: &str = "pub struct Count(pub u32);\n\nimpl Iterator for Count {\n    type Item = u32;\n    fn next(&mut self) -> Option<u32> {\n        None\n    }\n}\n";
const TRAIT_IMPL_NEW: &str = "pub struct Count(pub u32);\n\nimpl Iterator for Count {\n    type Item = u32;\n    fn next(&mut self) -> Option<u32> {\n        None\n    }\n\n    fn size_hint(&self) -> (usize, Option<usize>) {\n        (0, None)\n    }\n}\n";

// rule+ R70.2-1
// rule- R70.2-1
#[test]
fn an_override_of_a_default_method_of_a_trait_of_another_crate_is_a_change_of_the_impl() {
    let (_old_dir, old) = crate::tests::load(TRAIT_IMPL_OLD);
    let (_new_dir, new) = crate::tests::load(TRAIT_IMPL_NEW);
    let method = new.items.keys().find(|key| key.ends_with("::size_hint")).unwrap().clone();
    // No record: no decision (an added item, R70.1-1).
    assert!(crate::overrides::override_text(&old, &new, &method, true).is_none());
    give(&new, &[TestDef { item: &method, of: Some(("core::iter::traits::iterator::Iterator::size_hint", true)), ..TestDef::default() }], &[]);
    let (text, default) = crate::overrides::override_text(&old, &new, &method, true).expect("an override");
    assert_eq!(default, "core::iter::traits::iterator::Iterator::size_hint");
    assert!(text.contains("now overrides the default body of `core::iter::traits::iterator::Iterator::size_hint` (a trait of another crate"), "{text}");
    // An item of the trait with no default: a required method, not an override.
    give(&new, &[TestDef { item: &method, of: Some(("core::iter::traits::iterator::Iterator::size_hint", false)), ..TestDef::default() }], &[]);
    assert!(crate::overrides::override_text(&old, &new, &method, true).is_none());
}

/// The record of `ws` with a definition for each item of `items` and one `cfg` attribute at the
/// start of the place of the item `cfg_item`, with the condition `condition`.
fn give_cfg(ws: &semilla_syntax::Workspace, items: &[&str], cfg_item: &str, condition: &str) {
    let place = |path: &str| crate::records::item_place(ws, &ws.items[path]).unwrap();
    let defs: Vec<serde_json::Value> = items.iter().map(|path| {
        let (_, start, end) = place(path);
        serde_json::json!({"p": path, "n": path.rsplit("::").next().unwrap(), "k": "function", "s": [0, start, end], "i": [0, start, end]})
    }).collect();
    let (_, start, _) = place(cfg_item);
    let record: semilla_core::namerec::Record = serde_json::from_value(serde_json::json!({"f": ["src/lib.rs"], "d": defs, "c": [{"r": [0, start, start + 8], "v": true, "k": "cfg", "t": condition}]})).unwrap();
    ws.names.set(Some(std::sync::Arc::new(semilla_core::namerec::NameIndex::new([("demo.lib.aaaa", &record)]))));
}

// rule+ R70.2-1
// rule- R70.2-1
#[test]
fn a_cfg_of_the_record_in_the_place_of_the_edit_gives_the_spotlight() {
    let (_old_dir, old) = crate::tests::load(PLAIN);
    let (_new_dir, new) = crate::tests::load(CFG_ITEM);
    give(&old, &[TestDef { item: "demo::inc", ..TestDef::default() }], &[]);
    give_cfg(&new, &["demo::inc", "demo::inc2"], "demo::inc2", "unix");
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    let found = crate::spotlight(&edits, &old, &new);
    assert!(found.iter().any(|line| line == "cfg attribute: demo::inc2"), "{found:?}");
    // A `cfg(test)` of the record only selects test code: no line.
    let (_new_dir, new) = crate::tests::load(&CFG_ITEM.replace("#[cfg(unix)]", "#[cfg(test)]"));
    give_cfg(&new, &["demo::inc", "demo::inc2"], "demo::inc2", "test");
    let edits = semilla_syntax::diff(&old, &new, &Default::default());
    let found = crate::spotlight(&edits, &old, &new);
    assert!(found.iter().all(|line| !line.starts_with("cfg attribute")), "{found:?}");
}

const SPEAK_OLD: &str = "pub trait Speak {\n    fn name(&self) -> u32 {\n        1\n    }\n}\n\npub struct Dog;\n\nimpl Speak for Dog {}\n";
const SPEAK_NEW: &str = "pub trait Speak {\n    fn name(&self) -> u32 {\n        1\n    }\n}\n\npub struct Dog;\n\nimpl Speak for Dog {\n    fn name(&self) -> u32 {\n        2\n    }\n}\n";

// rule+ R70.1-1
// rule- R70.2-1
#[test]
fn an_override_with_no_record_uses_the_text_rule_and_the_record_decides_where_it_has_the_method() {
    let (_old_dir, old) = crate::tests::load(SPEAK_OLD);
    let (_new_dir, new) = crate::tests::load(SPEAK_NEW);
    let method = "demo::<Dog as Speak>::name";
    // No record (code that the build does not select): the trait of the crate with the last
    // name has a default body (the text rule of R68.5-1).
    let (text, default) = crate::overrides::override_text(&old, &new, method, true).expect("an override by the text rule");
    assert_eq!(default, "demo::Speak::name");
    assert!(text.contains("now overrides the default body of `demo::Speak::name`: `fn name(&self) -> u32 { 1 }` → `fn name(&self) -> u32 { 2 }`"), "{text}");
    // The record says that the item of the trait has no default: no override.
    give(&new, &[TestDef { item: method, of: Some(("demo::Speak::name", false)), ..TestDef::default() }], &[]);
    assert!(crate::overrides::override_text(&old, &new, method, true).is_none());
}

const SPEAK_REQUIRED: &str = "pub trait Speak {\n    fn name(&self) -> u32;\n}\n\npub struct Dog;\n\nimpl Speak for Dog {\n    fn name(&self) -> u32 {\n        2\n    }\n}\n";

// rule+ R70.2-1
#[test]
fn a_removed_method_whose_trait_gets_a_default_in_the_step_is_an_override_that_goes() {
    // The old trait has no default; the step adds the default and removes the method of the
    // impl (`uuid` #932): the impl now uses the new default body.
    let (_old_dir, old) = crate::tests::load(SPEAK_REQUIRED);
    let (_new_dir, new) = crate::tests::load(SPEAK_OLD);
    let method = "demo::<Dog as Speak>::name";
    give(&old, &[TestDef { item: method, of: Some(("demo::Speak::name", false)), ..TestDef::default() }], &[]);
    give(&new, &[TestDef { item: "demo::Speak::name", of: None, ..TestDef::default() }], &[]);
    // The record of the new tree must say that the item of the trait has a default.
    assert!(crate::overrides::override_text(&old, &new, method, false).is_none());
    let record: semilla_core::namerec::Record = serde_json::from_value(serde_json::json!({
        "f": ["src/lib.rs"],
        "d": [{"p": "demo::Speak::name", "n": "name", "k": "associated function", "s": [0, 20, 60], "i": [0, 22, 26], "o": true}]
    })).unwrap();
    new.names.set(Some(std::sync::Arc::new(semilla_core::namerec::NameIndex::new([("demo.lib.aaaa", &record)]))));
    let (text, _) = crate::overrides::override_text(&old, &new, method, false).expect("a removed override");
    assert!(text.contains("is removed: the impl now uses the default body of `demo::Speak::name`"), "{text}");
}

const GUARD: &str = "pub struct Guard;\n\n#[cfg(not(unix))]\nimpl Drop for Guard {\n    fn drop(&mut self) {}\n}\n\npub fn keep() -> u32 {\n    let _h = Guard;\n    1\n}\n";

// rule+ R70.2-1
// rule- R70.2-1
#[test]
fn a_type_with_a_part_that_the_configuration_does_not_build_keeps_its_cfg_note() {
    let (_dir, ws) = crate::tests::load(GUARD);
    let entry = "`demo::Guard` (a type with an `impl` under `#[cfg(not(unix))]`)";
    // The record has the type and no definition of the `impl` under the false `cfg`.
    give(&ws, &[TestDef { item: "demo::Guard", ..TestDef::default() }, TestDef { item: "demo::keep", ..TestDef::default() }], &[]);
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::keep", entry).as_deref(), Some("`demo::Guard` (a type with an `impl` under `#[cfg(not(unix))]`; the configuration does not build a part of it)"));
    // The configuration builds the `impl`: the record has it, and the use gives no note.
    let impl_path = ws.items.keys().find(|key| key.contains("impl <Guard as Drop>")).unwrap().clone();
    give(&ws, &[TestDef { item: "demo::Guard", ..TestDef::default() }, TestDef { item: "demo::keep", ..TestDef::default() }, TestDef { item: &impl_path, ..TestDef::default() }], &[]);
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::keep", entry), None);
}

const DOCUMENTED: &str = "pub type Block = usize;\n\n/// Uses `Block`.\n#[doc(alias = \"w\")]\npub fn width(b: Block) -> Block {\n    b\n}\n";

// rule+ R70.2-1
#[test]
fn an_item_with_doc_comments_has_the_record_of_its_definition() {
    // The place of the item starts at its doc comment; the definition of the record starts at
    // `pub fn`. The uses of `Block` resolve.
    let (_dir, ws) = crate::tests::load(DOCUMENTED);
    let text = ws.rust_files["src/lib.rs"].clone();
    let def_start = text.find("pub fn").unwrap() as i64;
    let at = |nth: usize| text.match_indices("Block").nth(nth).map(|(at, _)| at).unwrap() as i64;
    let record: semilla_core::namerec::Record = serde_json::from_value(serde_json::json!({
        "f": ["src/lib.rs"],
        "d": [{"p": "demo::width", "n": "width", "k": "function", "s": [0, def_start, text.len() as i64 - 1], "i": [0, def_start + 7, def_start + 12]},
              {"p": "demo::Block", "n": "Block", "k": "type alias", "s": [0, 0, 23], "i": [0, 9, 14]}],
        "p": [[0, at(2), at(2) + 5, 1], [0, at(3), at(3) + 5, 1]]
    })).unwrap();
    ws.names.set(Some(std::sync::Arc::new(semilla_core::namerec::NameIndex::new([("demo.lib.aaaa", &record)]))));
    assert_eq!(crate::records::cfg_use([&ws, &ws], "demo::width", "`Block` (under `cfg`)"), None);
}
