//! The tests of design 94: the transformations of the map of the diff (extract, inline,
//! reorder). The checks of the MIR have their tests in `semilla_normal::transform`; the case set
//! has the cases of `map94`.

use semilla_core::{EntryKind, Fact};

use crate::transforms;

// rule+ R94.4-3
// rule- R94.4-3
#[test]
fn two_arm_patterns_are_disjoint_only_for_different_literals_or_different_variants() {
    // Negative of a dependence: patterns that no one value can match.
    assert!(transforms::disjoint("0", "1"));
    assert!(transforms::disjoint("'a' | 'b'", "'c'"));
    assert!(transforms::disjoint("Mode::Idle", "Mode::Busy(x)"));
    assert!(transforms::disjoint("\"on\"", "\"off\""));
    // Positive: a binding, a wildcard, a range, or one equal literal can match one value of both.
    assert!(!transforms::disjoint("0", "_"));
    assert!(!transforms::disjoint("0..=5", "3"));
    assert!(!transforms::disjoint("x", "1"));
    assert!(!transforms::disjoint("1 | 2", "2"));
    assert!(!transforms::disjoint("Mode::Idle", "Other::Busy"));
}

// rule+ R94.1-3
#[test]
fn the_template_of_each_candidate_is_a_statement_with_the_two_paths() {
    assert_eq!(transforms::template(false, "shop::Cart::total", "shop::Cart::subtotal"), "#[extract(before::shop::Cart::total, into = after::shop::Cart::subtotal)] fn total_into_subtotal() {}");
    assert_eq!(transforms::template(true, "shop::add", "shop::check"), "#[inlined(after::shop::add, from = before::shop::check)] fn add_from_check() {}");
}

// rule+ R94.3-2
// rule- R94.3-2
#[test]
fn a_transformation_with_the_mark_of_the_order_is_a_real_edit() {
    let entry = |order: Option<&str>| Fact { id: "f1".into(), kind: Some(EntryKind::Extract), nodes: vec![1], check: "R94.3-1, equal MIR".into(), order: order.map(str::to_string), ..Default::default() };
    // Positive: the mark makes the entry a real edit, and the head shows it.
    let marked = entry(Some("two parts that each can panic"));
    assert!(marked.counts_as_real(), "{marked:#?}");
    assert!(marked.entry_head().unwrap().starts_with("extract, the order can matter (Semilla;"), "{:?}", marked.entry_head());
    // Negative: with no mark, the entry is a transformation.
    let plain = entry(None);
    assert!(!plain.counts_as_real() && plain.section() == Some(3), "{plain:#?}");
}
