//! The tests of batch 10u (design 68).

use crate::facts::const_parts;

fn parts(text: &str) -> Option<(String, String)> {
    const_parts(&Some(text.to_string()))
}

// rule+ R68.2-2
#[test]
fn the_value_of_a_constant_starts_after_the_top_level_equal_sign() {
    let got = parts("#[cfg(target_pointer_width = \"32\")] const K: u32 = 0x9E37_79B9;");
    assert_eq!(got, Some(("#[cfg(target_pointer_width = \"32\")] const K: u32".to_string(), "0x9E37_79B9".to_string())));
    let got = parts("#[doc = \"a ] = b\"] pub const N: [u8; 2] = [1, 2];");
    assert_eq!(got.map(|(_, value)| value), Some("[1, 2]".to_string()));
}

// rule- R68.2-2
#[test]
fn a_constant_with_no_attribute_keeps_its_value_text() {
    assert_eq!(parts("pub const LIMIT: u32 = 4;"), Some(("pub const LIMIT: u32".to_string(), "4".to_string())));
    // An equal sign in the value is a part of the value.
    assert_eq!(parts("const S: &str = \"a = b\";").map(|(_, value)| value), Some("\"a = b\"".to_string()));
    assert_eq!(parts("const T: bool = A == B;").map(|(_, value)| value), Some("A == B".to_string()));
    assert_eq!(parts("const X: u8;"), None);
}

// R71.1-1, the element form (R68.6-3 before batch 11c).
// rule+ R71.1-1
#[test]
fn two_arrays_of_one_length_show_their_changed_elements() {
    use crate::facts::array_change;
    assert_eq!(array_change("[1, 2, 3, 4]", "[1, 2, 9, 4]").as_deref(), Some("[2] 3 → 9 (1 of 4 elements differ)"));
    let old: Vec<String> = (0..20).map(|n| n.to_string()).collect();
    let new: Vec<String> = (0..20).map(|n| if n < 10 { (n + 100).to_string() } else { n.to_string() }).collect();
    let text = array_change(&format!("[{}]", old.join(", ")), &format!("[{}]", new.join(", "))).unwrap();
    assert!(text.starts_with("[0] 0 → 100, [1] 1 → 101,"), "{text}");
    // R71.1-1: each changed element shows (before batch 11c: at most 8, then "and 2 more").
    assert!(text.ends_with("[8] 8 → 108, [9] 9 → 109 (10 of 20 elements differ)"), "{text}");
    assert_eq!(crate::equiv::outcomes("returns [1, 2, 3]", "returns [1, 5, 3]"), "before and after return arrays that differ at [1] 2 → 5 (1 of 3 elements differ)");
}

// rule- R71.1-1
#[test]
fn other_values_keep_their_full_text() {
    use crate::facts::array_change;
    // A scalar, two lengths, each element changed, one element.
    assert_eq!(array_change("7", "6"), None);
    assert_eq!(array_change("[1, 2]", "[1, 2, 3]"), None);
    assert_eq!(array_change("[1, 2]", "[3, 4]"), None);
    assert_eq!(array_change("[1]", "[2]"), None);
    assert_eq!(crate::equiv::outcomes("returns 7", "returns 6"), "before returns 7, after returns 6");
    assert_eq!(crate::equiv::outcomes("returns [1, 2]", "panics (index)"), "before returns [1, 2], after panics (index)");
}

const U4_SOURCE: &str = "mod a {\n    pub(crate) struct Block {\n        pub x: u8,\n    }\n}\nmod b {\n    pub struct Block(pub u16);\n}\nmod c {\n    pub struct Cell(pub u8);\n}\nmod d {\n    pub use super::c::Cell as Other;\n}\npub use b::Block;\npub use std::fmt::Write;\npub use d::Other;\n";

// rule+ R70.2-1
#[test]
fn the_effective_visibility_of_the_compiler_decides_the_public_api() {
    use crate::records::testing::{TestDef, give};
    // The field of the private type: the compiler says that no different crate can use it,
    // though a `pub use` line names a different `Block` (R68.4-1 made this from the text).
    let (_dir, ws) = crate::tests::load(U4_SOURCE);
    give(&ws, &[TestDef { item: "demo::a::Block", ..TestDef::default() }, TestDef { item: "demo::a::Block.x", ..TestDef::default() }, TestDef { item: "demo::b::Block", public: true, ..TestDef::default() }], &[]);
    assert_eq!(crate::api_reach(&ws, "demo::a::Block.x"), crate::Reach::No);
    // The item that a different crate can use.
    assert_eq!(crate::api_reach(&ws, "demo::b::Block"), crate::Reach::Yes);
    // `d::Other` is a name that a `use` line brings: the compiler follows it.
    give(&ws, &[TestDef { item: "demo::c::Cell.0", public: true, ..TestDef::default() }], &[]);
    assert_eq!(crate::api_reach(&ws, "demo::c::Cell.0"), crate::Reach::Yes);
}

// rule- R70.2-1
#[test]
fn a_tree_with_no_record_or_a_removed_name_keeps_the_text_rule() {
    use crate::records::testing::{TestDef, give};
    // R70.1-1: no record: the name rule of the text (the safe side).
    let source = U4_SOURCE.replace("pub use b::Block;", "pub use a::Block;").replace("pub(crate) struct Block", "pub struct Block");
    let (_dir, ws) = crate::tests::load(&source);
    assert!(matches!(crate::api_reach(&ws, "demo::a::Block.x"), crate::Reach::Maybe(cause) if cause.contains("`Block`")));
    let (_dir, ws) = crate::tests::load(U4_SOURCE);
    assert!(matches!(crate::api_reach(&ws, "demo::a::Block.x"), crate::Reach::Maybe(_)));
    // A false `cfg` removes an item with a name of the path: another configuration can export
    // it, so the text rule stays.
    give(&ws, &[TestDef { item: "demo::a::Block", ..TestDef::default() }, TestDef { item: "demo::a::Block.x", ..TestDef::default() }], &["Block"]);
    assert!(matches!(crate::api_reach(&ws, "demo::a::Block.x"), crate::Reach::Maybe(_)));
}

const U9_OLD: &str = "pub struct X;\nimpl std::ops::Add for X {\n    type Output = X;\n    fn add(self, _: X) -> X {\n        X\n    }\n}\n";

// rule+ R70.2-1
#[test]
fn an_impl_is_identified_by_the_trait_reference_of_the_compiler() {
    use crate::records::testing::{TestDef, give};
    let (_old_dir, old) = crate::tests::load(U9_OLD);
    let (_new_dir, new) = crate::tests::load(&U9_OLD.replace("std::ops::Add", "core::ops::Add"));
    let old_impl = old.items.keys().find(|key| key.contains("impl <")).unwrap().clone();
    let new_impl = new.items.keys().find(|key| key.contains("impl <")).unwrap().clone();
    assert_ne!(old_impl, new_impl);
    give(&old, &[TestDef { item: &old_impl, trait_ref: Some("<demo::X as core::ops::arith::Add>"), ..TestDef::default() }], &[]);
    give(&new, &[TestDef { item: &new_impl, trait_ref: Some("<demo::X as core::ops::arith::Add>"), ..TestDef::default() }], &[]);
    let (rekeyed, _) = crate::identity::trait_path_pairs(&old, &new).expect("one pair");
    assert!(rekeyed.items.contains_key(&new_impl));
    // Each item of the impl takes the key too.
    assert!(rekeyed.items.keys().any(|key| key.ends_with("::add") && key.contains("core::ops::Add")), "{:?}", rekeyed.items.keys().collect::<Vec<_>>());
}

// rule- R70.2-1
#[test]
fn an_impl_of_a_different_trait_or_with_no_record_keeps_its_key() {
    use crate::records::testing::{TestDef, give};
    let (_old_dir, old) = crate::tests::load(U9_OLD);
    let (_new_dir, new) = crate::tests::load(&U9_OLD.replace("std::ops::Add", "core::ops::Add"));
    // R70.1-1: no record: no pair (a removed and an added impl, the safe side).
    assert!(crate::identity::trait_path_pairs(&old, &new).is_none());
    let old_impl = old.items.keys().find(|key| key.contains("impl <")).unwrap().clone();
    let new_impl = new.items.keys().find(|key| key.contains("impl <")).unwrap().clone();
    give(&old, &[TestDef { item: &old_impl, trait_ref: Some("<demo::X as core::ops::arith::Add>"), ..TestDef::default() }], &[]);
    give(&new, &[TestDef { item: &new_impl, trait_ref: Some("<demo::X as my::Add>"), ..TestDef::default() }], &[]);
    assert!(crate::identity::trait_path_pairs(&old, &new).is_none());
}

const U12_OLD: &str = "#[cfg(target_feature = \"sse2\")]\npub fn width() -> u32 {\n    16\n}\n\n#[cfg(not(any(target_feature = \"sse2\", target_feature = \"avx2\")))]\npub fn width() -> u32 {\n    8\n}\n\n#[cfg(all(feature = \"simd\", target_os = \"windows\"))]\npub fn wide() -> u32 {\n    1\n}\n\n#[cfg(feature = \"simd\")]\npub fn simd() -> u32 {\n    2\n}\n\n#[cfg(unix)]\npub fn host() -> u32 {\n    3\n}\n\nmacro_rules! pick { ($($t:tt)*) => { $($t)* }; }\npick! {\n    #[cfg(target_os = \"windows\")]\n    pub fn made() -> u32 { 4 }\n}\n";

fn host() -> std::collections::BTreeSet<String> {
    crate::print_cfg_atoms("debug_assertions\npanic=\"unwind\"\ntarget_arch=\"x86_64\"\ntarget_feature=\"sse2\"\ntarget_os=\"linux\"\nunix\n")
}

// rule+ R68.12-1
#[test]
fn changed_code_under_a_false_target_condition_is_not_checked() {
    let new = U12_OLD.replace("    8\n", "    8 + missing\n").replace("    1\n", "    1 + other\n");
    let (_a, old) = crate::tests::load(U12_OLD);
    let (_b, new) = crate::tests::load(&new);
    let found = crate::target_hidden([&old, &new], &host());
    let lines: Vec<(usize, Vec<usize>)> = found.iter().map(|hidden| (hidden.line, hidden.changed.clone())).collect();
    // The default block (sse2 is true), and a feature with a false target condition.
    assert_eq!(lines, [(6, vec![8]), (11, vec![13])], "{found:?}");
    assert!(found[0].text("`x86_64-unknown-linux-gnu`").contains("under `#[cfg(not(any(target_feature = \"sse2\", target_feature = \"avx2\")))]` at src/lib.rs:6 (changed lines: src/lib.rs:8)"), "{}", found[0].text("x"));
}

// rule- R68.12-1
#[test]
fn changed_code_under_a_true_target_condition_or_a_feature_has_no_line() {
    // A true target condition, a feature condition (the check does not decide it), and the
    // input of a macro call.
    let new = U12_OLD.replace("    16\n", "    17\n").replace("    2\n", "    5\n").replace("    3\n", "    6\n").replace("{ 4 }", "{ 7 }");
    let (_a, old) = crate::tests::load(U12_OLD);
    let (_b, new) = crate::tests::load(&new);
    assert_eq!(crate::target_hidden([&old, &new], &host()), Vec::new());
}
