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

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| name.to_string()).collect()
}

// rule+ R68.4-1
#[test]
fn a_pub_use_line_with_the_path_of_a_different_item_does_not_name_the_item() {
    let (_dir, ws) = crate::tests::load(U4_SOURCE);
    use crate::{UsePath, resolve_use};
    assert_eq!(resolve_use(&ws, "demo", &names(&["b", "Block"])), UsePath::Item("demo::b::Block".into()));
    assert_eq!(resolve_use(&ws, "demo::d", &names(&["super", "c", "Cell"])), UsePath::Item("demo::c::Cell".into()));
    assert_eq!(resolve_use(&ws, "demo", &names(&["crate", "a", "Block"])), UsePath::Item("demo::a::Block".into()));
    assert_eq!(resolve_use(&ws, "demo", &names(&["std", "fmt", "Write"])), UsePath::Foreign);
    // The field of the private type: the `pub use` lines name `b::Block` and a type of `std`.
    assert_eq!(crate::api_reach(&ws, "demo::a::Block.x"), crate::Reach::No);
}

// rule- R68.4-1
#[test]
fn a_pub_use_line_of_the_item_or_with_a_path_that_semilla_cannot_resolve_keeps_the_line() {
    let source = U4_SOURCE.replace("pub use b::Block;", "pub use a::Block;").replace("pub(crate) struct Block", "pub struct Block");
    let (_dir, ws) = crate::tests::load(&source);
    assert!(matches!(crate::api_reach(&ws, "demo::a::Block.x"), crate::Reach::Maybe(cause) if cause.contains("`Block`")));
    // `d::Other` is a name that a `use` line brings: Semilla does not follow it, so the name
    // rule stays (the `as` name of the path).
    let (_dir, ws) = crate::tests::load(U4_SOURCE);
    assert_eq!(crate::resolve_use(&ws, "demo", &names(&["d", "Other"])), crate::UsePath::Unknown);
    assert!(matches!(crate::api_reach(&ws, "demo::c::Cell.0"), crate::Reach::Maybe(_)));
}

// rule+ R68.9-1
#[test]
fn a_path_of_core_or_alloc_has_the_form_of_std() {
    use semilla_core::names::std_facade_form;
    assert_eq!(std_facade_form("shop::impl <X as core::ops::Add>"), "shop::impl <X as std::ops::Add>");
    assert_eq!(std_facade_form("shop::<X as ::alloc::borrow::ToOwned>::to_owned"), "shop::<X as std::borrow::ToOwned>::to_owned");
    assert_eq!(std_facade_form("core :: ops :: Add"), "std::ops :: Add");
    assert_eq!(std_facade_form("shop::impl <core::num::Wrapping as Tr>"), "shop::impl <std::num::Wrapping as Tr>");
}

// rule- R68.9-1
#[test]
fn a_path_of_a_module_with_the_name_core_keeps_its_text() {
    use semilla_core::names::std_facade_form;
    for text in ["shop::core::f", "shop::impl <X as my::core::Tr>", "shop::impl <X as corex::Tr>", "shop::impl <X as std::ops::Add>", "core"] {
        assert_eq!(std_facade_form(text), text);
    }
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
