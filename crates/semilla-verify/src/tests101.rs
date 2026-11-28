//! Unit tests of design 101: the graph match of a pair of code (`rewrites.rs`) and the case table
//! (`cases.rs`). The texts are records of the driver: the closures of an intent file with type
//! holes, the small callees of the standard library, and a body `shop::build` (old and new).

use super::*;
use crate::cases::{Callees, cases, counts, table};

const OK_OLD: &str = r#"fn _(_1: &{closure#0}, _2: std::result::Result<T, E>) -> T generics <T#0, E#1, #2, #3, #4> {
    let _0: T;
    let _3: isize;
    let _4: isize;
    let _5: !;
    let _6: isize;
    bb0: {
        _3 = discriminant(_2);
        switchInt(_3) -> [std::result::Result::Err: bb5, std::result::Result::Ok: bb1, otherwise: bb8];
    }
    bb1: {
        _0 = ((_2 as std::result::Result::Ok).std::result::Result::Ok.0: T);
        _4 = discriminant(_2);
        switchInt(_4) -> [std::result::Result::Ok: bb2, otherwise: bb3];
    }
    bb2: {
        return;
    }
    bb3: {
        drop(_2) -> [return: bb2, unwind: bb4];
    }
    bb4: {
        resume;
    }
    bb5: {
        _5 = const unsafe fn() -> ! {std::hint::unreachable_unchecked}() -> [no return, unwind: bb6];
    }
    bb6: {
        _6 = discriminant(_2);
        switchInt(_6) -> [std::result::Result::Ok: bb4, otherwise: bb7];
    }
    bb7: {
        drop(_2) -> [return: bb4, unwind terminate];
    }
    bb8: {
        unreachable;
    }
}
"#;
const OK_NEW: &str = r#"fn _(_1: &{closure#0}, _2: std::result::Result<T, E>) -> T generics <T#0, E#1, #2, #3, #4> {
    let _0: T;
    bb0: {
        _0 = const unsafe fn(std::result::Result<T, E>) -> T {std::result::Result::<T, E>::unwrap_unchecked}(_2) -> [return: bb1, unwind: bb2];
    }
    bb1: {
        return;
    }
    bb2: {
        resume;
    }
}
"#;
const ERR_OLD: &str = r#"fn _(_1: &{closure#0}, _2: std::result::Result<T, E>) -> () generics <T#0, E#1, #2, #3, #4> {
    let _0: ();
    let _3: &std::result::Result<T, E>;
    let _4: bool;
    let _5: !;
    bb0: {
        _3 = &shared _2;
        _4 = const for<'a> fn(&'a std::result::Result<T, E>) -> bool {std::result::Result::<T, E>::is_err}(_3) -> [return: bb1, unwind: bb6];
    }
    bb1: {
        switchInt(_4) -> [0: bb2, otherwise: bb5];
    }
    bb2: {
        _0 = const ();
        drop(_2) -> [return: bb3, unwind: bb4];
    }
    bb3: {
        return;
    }
    bb4: {
        resume;
    }
    bb5: {
        _5 = const unsafe fn() -> ! {std::hint::unreachable_unchecked}() -> [no return, unwind: bb6];
    }
    bb6: {
        drop(_2) -> [return: bb4, unwind terminate];
    }
}
"#;
const ERR_NEW: &str = r#"fn _(_1: &{closure#0}, _2: std::result::Result<T, E>) -> () generics <T#0, E#1, #2, #3, #4> {
    let _0: ();
    let _3: T;
    bb0: {
        _3 = const unsafe fn(std::result::Result<T, E>) -> T {std::result::Result::<T, E>::unwrap_unchecked}(_2) -> [return: bb1, unwind: bb3];
    }
    bb1: {
        drop(_3) -> [return: bb2, unwind: bb3];
    }
    bb2: {
        _0 = const ();
        return;
    }
    bb3: {
        resume;
    }
}
"#;
const UNWRAP: &str = r#"fn _(_1: std::result::Result<T, E>) -> T generics <T#0, E#1> {
    let _0: T;
    let _2: isize;
    let _3: E;
    let _4: ();
    bb0: {
        _2 = discriminant(_1);
        switchInt(_2) -> [std::result::Result::Err: bb2, std::result::Result::Ok: bb1, otherwise: bb3];
    }
    bb1: {
        _0 = ((_1 as std::result::Result::Ok).std::result::Result::Ok.0: T);
        return;
    }
    bb2: {
        _3 = ((_1 as std::result::Result::Err).std::result::Result::Err.0: E);
        assume(runtime_checks(UbChecks));
        _4 = const fn() {std::hint::unreachable_unchecked::precondition_check}() -> [return: bb3, unwind unreachable];
    }
    bb3: {
        unreachable;
    }
}
"#;
const UNREACHABLE: &str = r#"fn _() -> ! {
    let _0: !;
    let _1: ();
    bb0: {
        assume(runtime_checks(UbChecks));
        _1 = const fn() {std::hint::unreachable_unchecked::precondition_check}() -> [return: bb1, unwind unreachable];
    }
    bb1: {
        unreachable;
    }
}
"#;
const IS_ERR: &str = r#"fn _(_1: &std::result::Result<T, E>) -> bool generics <T#0, E#1> {
    let _0: bool;
    let _2: isize;
    let _3: bool;
    bb0: {
        _2 = discriminant((*_1)) of std::result::Result values [std::result::Result::Err = 1, std::result::Result::Ok = 0];
        _3 = Eq(_2, const isize = 0);
        _0 = Not(_3);
        return;
    }
}
"#;
const BUILD_OLD: &str = r#"fn _(_1: &A, _2: shop::Layout, _3: usize) -> shop::Table generics <A#0> {
    let _0: shop::Table;
    let _4: &A;
    let _5: shop::Mode;
    let _6: core::result::Result<shop::Table, shop::Error>;
    let _7: isize;
    let _8: !;
    bb0: {
        _4 = &shared (*_1);
        _5 = adt shop::Mode variant shop::Mode::Sure fields {};
        _6 = const for<'a> fn(&'a A, shop::Layout, usize, shop::Mode) -> core::result::Result<shop::Table, shop::Error> {shop::Table::make::<A>}(_4, _2, _3, _5) -> [return: bb1, unwind continue];
    }
    bb1: {
        _7 = discriminant(_6);
        switchInt(_7) -> [core::result::Result::Err: bb3, core::result::Result::Ok: bb2, otherwise: bb4];
    }
    bb2: {
        _0 = ((_6 as core::result::Result::Ok).core::result::Result::Ok.0: shop::Table);
        return;
    }
    bb3: {
        _8 = const unsafe fn() -> ! {core::hint::unreachable_unchecked}() -> [no return, unwind continue];
    }
    bb4: {
        unreachable;
    }
}
"#;
const BUILD_NEW: &str = r#"fn _(_1: &A, _2: shop::Layout, _3: usize) -> shop::Table generics <A#0> {
    let _0: shop::Table;
    let _4: &A;
    let _5: shop::Mode;
    let _6: core::result::Result<shop::Table, shop::Error>;
    bb0: {
        _4 = &shared (*_1);
        _5 = adt shop::Mode variant shop::Mode::Sure fields {};
        _6 = const for<'a> fn(&'a A, shop::Layout, usize, shop::Mode) -> core::result::Result<shop::Table, shop::Error> {shop::Table::make::<A>}(_4, _2, _3, _5) -> [return: bb1, unwind continue];
    }
    bb1: {
        _0 = const unsafe fn(core::result::Result<shop::Table, shop::Error>) -> shop::Table {core::result::Result::<shop::Table, shop::Error>::unwrap_unchecked}(_6) -> [return: bb2, unwind continue];
    }
    bb2: {
        return;
    }
}
"#;

/// The old closure `|o: Option<u32>| match o { Some(v) => v, None => panic!() }`; the new closure
/// returns `0` (or `1`) for `None`.
const PANIC_OLD: &str = "fn _(_1: &{closure#0}, _2: std::option::Option<u32>) -> u32 {\n    let _0: u32;\n    let _3: isize;\n    let _4: !;\n    bb0: {\n        _3 = discriminant(_2);\n        switchInt(_3) -> [std::option::Option::None: bb1, std::option::Option::Some: bb2, otherwise: bb3];\n    }\n    bb1: {\n        _4 = const fn(&'static str) -> ! {core::panicking::panic}(const &str = \"no\") -> [no return, unwind: bb4];\n    }\n    bb2: {\n        _0 = ((_2 as std::option::Option::Some).std::option::Option::Some.0: u32);\n        return;\n    }\n    bb3: {\n        unreachable;\n    }\n    bb4: {\n        resume;\n    }\n}\n";

/// The precondition check of `unreachable_unchecked`: it panics.
const PRECONDITION: &str = "fn _() -> () {\n    let _0: ();\n    let _1: !;\n    bb0: {\n        _1 = const fn(&'static str) -> ! {core::panicking::panic_nounwind}(const &str = \"ub\") -> [no return, unwind unreachable];\n    }\n}\n";

/// A closure with a join: `|b: bool| { let v = if b { 2 } else { 1 }; v }` with one more element
/// in the join.
const JOIN: &str = "fn _(_1: &{closure#0}, _2: bool) -> u32 {\n    let _0: u32;\n    let _3: u32;\n    bb0: {\n        switchInt(_2) -> [0: bb1, otherwise: bb2];\n    }\n    bb1: {\n        _0 = const u32 = 1;\n        goto -> bb3;\n    }\n    bb2: {\n        _0 = const u32 = 2;\n        goto -> bb3;\n    }\n    bb3: {\n        _3 = const u32 = 5;\n        return;\n    }\n}\n";

fn zero(value: u32) -> String {
    PANIC_OLD.replace("        _4 = const fn(&'static str) -> ! {core::panicking::panic}(const &str = \"no\") -> [no return, unwind: bb4];\n", &format!("        _0 = const u32 = {value};\n        return;\n"))
}

fn callees() -> Callees {
    let texts: BTreeMap<String, String> = [
        ("std::result::Result::<T, E>::unwrap_unchecked".to_string(), UNWRAP.to_string()),
        ("std::hint::unreachable_unchecked".to_string(), UNREACHABLE.to_string()),
        ("std::result::Result::<T, E>::is_err".to_string(), IS_ERR.to_string()),
        ("std::hint::unreachable_unchecked::precondition_check".to_string(), PRECONDITION.to_string()),
    ]
    .into();
    Callees::new(&texts)
}

fn names() -> Vec<String> {
    vec!["r".to_string()]
}

fn lines(old: &str, new: &str, callees: &Callees) -> Vec<String> {
    let rows = table(&cases(old, &names(), callees).unwrap(), &cases(new, &names(), callees).unwrap());
    rows.iter().map(|line| format!("{} | {} | {}", line.key, line.label, line.text)).collect()
}

/// The old site of `shop::build`: the `match` on the result of the call.
fn old_site() -> (Mir, Pattern, Found) {
    let (body, old) = (parse_mir(BUILD_OLD), pattern(OK_OLD).unwrap());
    let found = matches_at(&body, &old, 3, &Binding::default(), &mut Near::default());
    assert_eq!(found.len(), 1, "{found:#?}");
    let found = found[0].clone();
    (body, old, found)
}

// rule+ R101.1-1
// rule- R101.1-1
#[test]
fn a_branch_of_the_pattern_fits_a_branch_of_the_body_on_the_same_value() {
    let (_, _, found) = old_site();
    // The discriminant, the switch, the `Ok` arm, and the two ends of the `Err` arm and of the
    // arm with no variant; the exit is the `return` of the body.
    assert_eq!(found.elements, vec![3, 4, 5, 7, 8]);
    assert_eq!(found.exit, Some(6));
    assert_eq!(found.binding.values.get(&2).map(String::as_str), Some("_6"));
    // Negative: the body switches on another value than the discriminant of the bound value.
    let other = BUILD_OLD.replace("switchInt(_7)", "switchInt(_5)");
    assert!(matches_at(&parse_mir(&other), &pattern(OK_OLD).unwrap(), 3, &Binding::default(), &mut Near::default()).is_empty());
    // Negative: one arm of the body has one more element (the `Err` arm writes a value before
    // its end).
    let more = BUILD_OLD.replace("    bb3: {\n        _8 =", "    bb3: {\n        _5 = adt shop::Mode variant shop::Mode::Sure fields {};\n        _8 =");
    let mut near = Near::default();
    assert!(matches_at(&parse_mir(&more), &pattern(OK_OLD).unwrap(), 3, &Binding::default(), &mut near).is_empty());
    assert!(near.body.starts_with("_5 = adt"), "{near:?}");
    // Each block of the closure has one place in the body: the join of the closure is one join
    // of the body.
    let one = "fn _(_1: bool) -> u32 {\n    let _0: u32;\n    let _7: u32;\n    bb0: {\n        switchInt(_1) -> [0: bb1, otherwise: bb2];\n    }\n    bb1: {\n        _0 = const u32 = 1;\n        goto -> bb3;\n    }\n    bb2: {\n        _0 = const u32 = 2;\n        goto -> bb3;\n    }\n    bb3: {\n        _7 = const u32 = 5;\n        goto -> bb5;\n    }\n    bb4: {\n        _7 = const u32 = 5;\n        goto -> bb5;\n    }\n    bb5: {\n        return;\n    }\n}\n";
    assert_eq!(matches_at(&parse_mir(one), &pattern(JOIN).unwrap(), 0, &Binding::default(), &mut Near::default()).len(), 1);
    // Negative: the two arms of the body go to two places.
    let two = one.replace("_0 = const u32 = 2;\n        goto -> bb3;", "_0 = const u32 = 2;\n        goto -> bb4;");
    assert!(matches_at(&parse_mir(&two), &pattern(JOIN).unwrap(), 0, &Binding::default(), &mut Near::default()).is_empty());
}

// rule+ R101.1-2
// rule- R101.1-2
#[test]
fn a_site_has_one_entry() {
    let (body, old, found) = old_site();
    assert!(check_found(&body, &old, &found).is_ok());
    // Negative: the block of the `Ok` arm has an edge from outside the site (an unwind edge of
    // the call before the site).
    let entry = BUILD_OLD.replacen("-> [return: bb1, unwind continue]", "-> [return: bb1, unwind: bb2]", 1);
    let body = parse_mir(&entry);
    let found = matches_at(&body, &old, 3, &Binding::default(), &mut Near::default());
    assert!(check_found(&body, &old, &found[0]).unwrap_err().contains("one entry"));
}

// rule+ R101.2-1
// rule- R101.2-1
#[test]
fn a_type_hole_binds_to_one_type_on_the_two_sides() {
    let (body, old, found) = old_site();
    let bound = check_found(&body, &old, &found).unwrap();
    assert_eq!(bound.types.get(&0).map(String::as_str), Some("shop::Table"));
    assert_eq!(bound.types.get(&1).map(String::as_str), Some("shop::Error"));
    // The new closure fits the new body with the same types (the map of the locals is the
    // identity here).
    let table = MirTable { texts: [BUILD_OLD.into(), BUILD_NEW.into()], locals: (0..9).map(|local| (local, local)).collect(), ..Default::default() };
    let new_binding = mapped_binding(&table, &bound).unwrap();
    let found = matches_at(&parse_mir(BUILD_NEW), &pattern(OK_NEW).unwrap(), 3, &new_binding, &mut Near::default());
    assert_eq!(found.len(), 1, "{found:#?}");
    // Negative: a type hole that is bound takes no other type.
    let mut binding = Binding::default();
    binding.types.insert(0, "u32".into());
    let segs = segments("_0 = ((_2 as core::result::Result::Ok).core::result::Result::Ok.0: T)", 2, &["T".to_string()]);
    assert!(unify(&segs, "_0 = ((_6 as core::result::Result::Ok).core::result::Result::Ok.0: u8)", &binding).is_none());
    assert!(unify(&segs, "_0 = ((_6 as core::result::Result::Ok).core::result::Result::Ok.0: u32)", &binding).is_some());
}

// rule+ R101.2-2
// rule- R101.2-2
#[test]
fn a_call_through_a_bound_of_a_type_hole_fits_the_call_of_the_bound_type() {
    let segs = segments("_0 = const for<'a> fn(&'a T) -> T {<T as core::clone::Clone>::clone}(_2) -> [return: bb?, unwind ?]", 2, &["T".to_string()]);
    let found = unify(&segs, "_0 = const for<'a> fn(&'a shop::Item) -> shop::Item {<shop::Item as core::clone::Clone>::clone}(_1) -> [return: bb?, unwind ?]", &Binding::default()).unwrap();
    assert_eq!(found.types.get(&0).map(String::as_str), Some("shop::Item"));
    // Negative: the call of another type.
    let mut binding = Binding::default();
    binding.types.insert(0, "shop::Item".into());
    assert!(unify(&segs, "_0 = const for<'a> fn(&'a shop::Other) -> shop::Other {<shop::Other as core::clone::Clone>::clone}(_1) -> [return: bb?, unwind ?]", &binding).is_none());
}

// rule+ R101.3-2
// rule- R101.3-2
#[test]
fn a_drop_of_a_value_with_a_type_hole_can_have_no_element() {
    // The drop of the rest of the result after the move out of `Ok` (the body has no such drop:
    // its types need none): the match of `old_site` skips it.
    let old = pattern(OK_OLD).unwrap();
    assert_eq!(old.optional.get(&(1, 1)), Some(&(2, 2)));
    old_site();
    // Negative: a drop of a value with no type hole is no optional drop.
    let plain = "fn _(_1: &{closure#0}, _2: std::string::String) -> () {\n    let _0: ();\n    bb0: {\n        _0 = const ();\n        drop(_2) -> [return: bb1, unwind: bb2];\n    }\n    bb1: {\n        return;\n    }\n    bb2: {\n        resume;\n    }\n}\n";
    assert!(pattern(plain).unwrap().optional.is_empty());
}

// rule+ R101.3-3
// rule- R101.3-3
#[test]
fn a_result_kept_in_its_place_binds_to_the_place() {
    // The body reads the value of the `Ok` arm where it uses it: no element writes a local.
    let kept = BUILD_OLD.replace(
        "        _0 = ((_6 as core::result::Result::Ok).core::result::Result::Ok.0: shop::Table);\n        return;\n",
        "        _0 = const fn(shop::Table) -> shop::Table {shop::keep}(((_6 as core::result::Result::Ok).core::result::Result::Ok.0: shop::Table)) -> [return: bb5, unwind continue];\n    }\n    bb5: {\n        return;\n",
    );
    let found = matches_at(&parse_mir(&kept), &pattern(OK_OLD).unwrap(), 3, &Binding::default(), &mut Near::default());
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].binding.values.get(&0).map(String::as_str), Some("((_6 as core::result::Result::Ok).core::result::Result::Ok.0: shop::Table)"));
    // Negative: no element reads the place: no site.
    let unread = kept.replace("(((_6 as core::result::Result::Ok).core::result::Result::Ok.0: shop::Table))", "(const shop::Table = 0)");
    assert!(matches_at(&parse_mir(&unread), &pattern(OK_OLD).unwrap(), 3, &Binding::default(), &mut Near::default()).is_empty());
    // Also when the type of the result is known.
    let mut typed = Binding::default();
    typed.types.insert(0, "shop::Table".into());
    assert!(matches_at(&parse_mir(&unread), &pattern(OK_OLD).unwrap(), 3, &typed, &mut Near::default()).is_empty());
}

// rule+ R101.4-1
// rule- R101.4-1
#[test]
fn a_small_callee_goes_in_for_the_cases_only() {
    // With the callees, the call `unwrap_unchecked` has the cases of its `match`.
    let with = lines(OK_OLD, OK_NEW, &callees());
    assert_eq!(with.len(), 2, "{with:#?}");
    // Negative: with no callee text, the call is one path, and the table has no pair.
    let none = Callees::new(&BTreeMap::new());
    let without = lines(OK_OLD, OK_NEW, &none);
    assert!(without.iter().any(|line| line.contains("| new |")) && without.iter().any(|line| line.contains("| removed |")), "{without:#?}");
    // The fit reads the closure as it is: the callees change no site.
    old_site();
}

// rule+ R101.5-1
// rule- R101.5-1
#[test]
fn the_cases_pair_by_their_decisions_on_the_holes() {
    let rows = lines(OK_OLD, OK_NEW, &callees());
    assert!(rows[0].starts_with("`r` is `Err` |"), "{rows:#?}");
    assert!(rows[1].starts_with("`r` is `Ok` |"), "{rows:#?}");
    // `is_err` goes in: its `Eq` on the discriminant with the table of the values gives the
    // variant, so the keys of the `if` are the keys of the `match`.
    let rows = lines(ERR_OLD, ERR_NEW, &callees());
    assert!(rows[0].starts_with("`r` is `Err` |") && rows[1].starts_with("`r` is `Ok` |"), "{rows:#?}");
    // Negative: a case with no partner is "removed" or "new".
    let rows = lines(PANIC_OLD, OK_NEW, &callees());
    assert!(rows.iter().all(|line| line.contains("| removed |") || line.contains("| new |")), "{rows:#?}");
    // Each old case and each new case has its line: 2 old cases and 2 new cases.
    assert_eq!(rows.len(), 4, "{rows:#?}");
}

// rule+ R101.5-2
// rule- R101.5-2
#[test]
fn the_check_of_undefined_behavior_ends_a_path_and_the_arm_of_no_variant_is_no_case() {
    let found = cases(OK_NEW, &names(), &callees()).unwrap();
    // Two cases: the arm `otherwise` of the full `match` (a block with only `unreachable`) is no
    // case, and the `Err` arm ends at `assume(UbChecks)`.
    assert_eq!(found.len(), 2);
    assert!(found.iter().any(|(_, end)| *end == crate::cases::End::Unreachable));
    // Negative: an arm `otherwise` with code is a case.
    let open = PANIC_OLD.replace("    bb3: {\n        unreachable;\n", "    bb3: {\n        _0 = const u32 = 7;\n        return;\n");
    assert_eq!(cases(&open, &names(), &callees()).unwrap().len(), 3);
}

// rule+ R101.6-1
// rule- R101.6-1
#[test]
fn the_label_of_a_case_comes_from_the_code() {
    let rows = lines(OK_OLD, OK_NEW, &callees());
    assert!(rows[0].contains("| undefined on the two sides |"), "{rows:#?}");
    assert!(rows[1].contains("| equal code |"), "{rows:#?}");
    let rows = lines(PANIC_OLD, &zero(0), &callees());
    assert!(rows[0].contains("`o` is `None`") || rows[0].contains("is `None`"), "{rows:#?}");
    assert!(rows[0].contains("| accepts more |"), "{rows:#?}");
    let rows = lines(&zero(0), PANIC_OLD, &callees());
    assert!(rows[0].contains("| accepts less |"), "{rows:#?}");
    // Negative: only one path ends in `unreachable`: no "undefined on the two sides".
    let undefined = PANIC_OLD.replace("        _4 = const fn(&'static str) -> ! {core::panicking::panic}(const &str = \"no\") -> [no return, unwind: bb4];\n", "        unreachable;\n");
    let rows = lines(&undefined, &zero(0), &callees());
    assert!(rows[0].contains("| accepts more |"), "{rows:#?}");
    let rows = lines(&zero(0), &zero(1), &callees());
    assert!(rows[0].contains("| changed | the result: `0` → `1`") || rows[0].contains("| changed |"), "{rows:#?}");
    assert!(rows[1].contains("| equal code |"), "{rows:#?}");
    assert_eq!(counts(&table(&cases(&zero(0), &names(), &callees()).unwrap(), &cases(&zero(1), &names(), &callees()).unwrap())), "1 changed, 1 equal code");
    // Negative: the drops differ, so the case is not "equal code" (the old code drops the whole
    // value, the new code drops the value of the `Ok` arm).
    let rows = lines(ERR_OLD, ERR_NEW, &callees());
    assert!(rows[1].contains("| changed | the drops:"), "{rows:#?}");
}

// rule+ R101.8-2
// rule- R101.8-2
#[test]
fn the_text_form_splits_at_the_arrow_outside_the_brackets() {
    let text = "match $a { Some(lco) => lco, None => unsafe { hint::unreachable_unchecked() }, } => { let option = $a; unsafe { option.unwrap_unchecked() } }";
    let (from, to) = crate::agent::split_arrow(text).unwrap();
    assert!(from.ends_with("}, }") && to.starts_with("{ let option"), "{from} | {to}");
    // Negative: an arrow only inside brackets is no split.
    assert!(crate::agent::split_arrow("match $a { Some(v) => v }").is_none());
}
