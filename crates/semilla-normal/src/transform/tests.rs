//! Unit tests of design 94 (`transform.rs`): the texts and the element ranges are records of the
//! driver for small crates (the case set has the same crates).

use super::*;

fn record(json: &str) -> RecordElements {
    serde_json::from_str(json).unwrap()
}

const OLD_TOTAL: &str = "fn _(_1: u32, _2: u32) -> u32 {\n    let _0: u32;\n    let _3: (u32, bool);\n    let _4: (u32, bool);\n    let _5: (u32, bool);\n    bb0: {\n        _3 = CheckedMul(_1, const u32 = 3);\n        assert(!(_3.1: bool), overflow Mul(_1, const u32 = 3)) -> [success: bb1, unwind continue];\n    }\n    bb1: {\n        _4 = CheckedAdd((_3.0: u32), _2);\n        assert(!(_4.1: bool), overflow Add((_3.0: u32), _2)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n        _5 = CheckedAdd((_4.0: u32), const u32 = 1);\n        assert(!(_5.1: bool), overflow Add((_4.0: u32), const u32 = 1)) -> [success: bb3, unwind continue];\n    }\n    bb3: {\n        _0 = (_5.0: u32);\n        return;\n    }\n}\n";
const OLD_TOTAL_EL: &str = r#"{"n": 8, "f": ["src/lib.rs"], "r": [[0, 2, 0, 50, 55], [2, 2, 0, 50, 59], [4, 3, 0, 65, 70], [7, 1, 0, 72, 72]], "s": [["depot::total", 0, 0, 72]], "y": [[0, 8, 0]], "v": 1, "o": 0, "l": [[0, 0, 32, 35], [1, 0, 13, 14, 0, 16, 19], [1, 0, 21, 22, 0, 24, 27], [3, 0, 50, 55], [3, 0, 50, 59], [3, 0, 65, 70]]}"#;
const NEW_TOTAL: &str = "fn _(_1: u32, _2: u32) -> u32 {\n    let _0: u32;\n    let _3: u32;\n    let _4: (u32, bool);\n    bb0: {\n        _3 = const fn(u32, u32) -> u32 {depot::part}(_1, _2) -> [return: bb1, unwind continue];\n    }\n    bb1: {\n        _4 = CheckedAdd(_3, const u32 = 1);\n        assert(!(_4.1: bool), overflow Add(_3, const u32 = 1)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n        _0 = (_4.0: u32);\n        return;\n    }\n}\n";
const NEW_TOTAL_EL: &str = r#"{"n": 5, "f": ["src/lib.rs"], "r": [[0, 1, 0, 50, 60], [1, 3, 0, 66, 71], [4, 1, 0, 73, 73]], "s": [["depot::total", 0, 0, 73], ["depot::part", 0, 75, 123]], "y": [[0, 1, 0, 1], [1, 4, 0]], "v": 1, "o": 0, "l": [[0, 0, 32, 35], [1, 0, 13, 14, 0, 16, 19], [1, 0, 21, 22, 0, 24, 27], [2, 0, 46, 47], [3, 0, 66, 71]]}"#;
const PART: &str = "fn _(_1: u32, _2: u32) -> u32 {\n    let _0: u32;\n    let _3: (u32, bool);\n    let _4: (u32, bool);\n    bb0: {\n        _3 = CheckedMul(_1, const u32 = 3);\n        assert(!(_3.1: bool), overflow Mul(_1, const u32 = 3)) -> [success: bb1, unwind continue];\n    }\n    bb1: {\n        _4 = CheckedAdd((_3.0: u32), _2);\n        assert(!(_4.1: bool), overflow Add((_3.0: u32), _2)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n        _0 = (_4.0: u32);\n        return;\n    }\n}\n";
const PART_EL: &str = r#"{"n": 6, "f": ["src/lib.rs"], "r": [[0, 2, 0, 112, 117], [2, 3, 0, 112, 121], [5, 1, 0, 123, 123]], "s": [["depot::part", 0, 75, 123]], "y": [[0, 6, 0]], "v": 1, "o": 0, "l": [[0, 0, 102, 105], [1, 0, 83, 84, 0, 86, 89], [1, 0, 91, 92, 0, 94, 97], [3, 0, 112, 117], [3, 0, 112, 121]]}"#;

/// The check of an extract of `depot::part` from `depot::total`, with the callee text `part`.
fn extract(part: &str) -> Vec<Transform> {
    let (old, new, callee) = (record(OLD_TOTAL_EL), record(NEW_TOTAL_EL), record(PART_EL));
    let before = elements::diff((OLD_TOTAL, &old), (NEW_TOTAL, &new)).unwrap();
    let sides = [Side { name: "depot::total", text: OLD_TOTAL, record: &old }, Side { name: "depot::total", text: NEW_TOTAL, record: &new }];
    check([&sides[0], &sides[1]], &Side { name: "depot::part", text: part, record: &callee }, Kind::Extract, &before)
}

// rule+ R94.1-1
// rule- R94.1-1
#[test]
fn the_callee_put_in_at_the_call_aligns_with_the_old_region() {
    let found = extract(PART);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].holds && found[0].equal && found[0].explained >= 3 && found[0].refused.is_none(), "{found:#?}");
    // Negative: a callee with one other operand explains the region only in part.
    let other = PART.replace("const u32 = 3", "const u32 = 4");
    let found = extract(&other);
    assert!(found[0].holds && !found[0].equal && !found[0].left[0].is_empty() && !found[0].left[1].is_empty(), "{found:#?}");
    // Negative: a callee with a different text explains nothing.
    let different = PART.replace("CheckedMul(_1, const u32 = 3)", "CheckedSub(_1, const u32 = 9)").replace("Mul(_1, const u32 = 3)", "Sub(_1, const u32 = 9)").replace("CheckedAdd((_3.0: u32), _2)", "CheckedSub((_3.0: u32), _2)").replace("Add((_3.0: u32), _2)", "Sub((_3.0: u32), _2)");
    let found = extract(&different);
    assert!(!found[0].holds, "{found:#?}");
}

// rule+ R94.5-1
// rule- R94.5-1
#[test]
fn a_call_that_is_not_its_inlined_body_gives_no_check() {
    // A callee whose signature is not the type of the call (design 95: a generic callee with no
    // generic arguments of the call that fit).
    let generic = PART.replace("fn _(_1: u32, _2: u32) -> u32 {", "fn _(_1: T, _2: T) -> T {");
    let found = extract(&generic);
    assert!(!found[0].holds && found[0].refused.as_deref().is_some_and(|cause| cause.contains("not the signature")), "{found:#?}");
    // A recursive callee.
    let recursive = PART.replace("_0 = (_4.0: u32);\n        return;", "_0 = const fn(u32, u32) -> u32 {depot::part}(_1, _2) -> [return: bb3, unwind continue];\n    }\n    bb3: {\n        return;");
    let callee = record(PART_EL);
    let (old, new) = (record(OLD_TOTAL_EL), record(NEW_TOTAL_EL));
    let before = elements::diff((OLD_TOTAL, &old), (NEW_TOTAL, &new)).unwrap();
    let sides = [Side { name: "depot::total", text: OLD_TOTAL, record: &old }, Side { name: "depot::total", text: NEW_TOTAL, record: &new }];
    let found = check([&sides[0], &sides[1]], &Side { name: "depot::part", text: &recursive, record: &callee }, Kind::Extract, &before);
    assert!(found[0].refused.as_deref() == Some("a recursive callee"), "{found:#?}");
    // A cleanup path: a call with an unwind action that the check does not map.
    let unwind = NEW_TOTAL.replace("-> [return: bb1, unwind continue]", "-> [return: bb1, unwind unreachable]");
    let new_record = record(NEW_TOTAL_EL);
    let before = elements::diff((OLD_TOTAL, &old), (&unwind, &new_record)).unwrap();
    let sides = [Side { name: "depot::total", text: OLD_TOTAL, record: &old }, Side { name: "depot::total", text: &unwind, record: &new_record }];
    let found = check([&sides[0], &sides[1]], &Side { name: "depot::part", text: PART, record: &record(PART_EL) }, Kind::Extract, &before);
    assert!(found[0].refused.as_deref().is_some_and(|cause| cause.contains("unwind unreachable")), "{found:#?}");
    // Negative: the plain callee goes in.
    assert!(extract(PART)[0].refused.is_none());
}

// rule+ R94.1-3
// rule- R94.1-3
#[test]
fn two_candidates_of_one_region_are_a_tie() {
    let one = extract(PART).remove(0);
    let mut two = Transform { callee: "depot::piece".into(), ..one.clone() };
    two.call = None;
    let mut list = vec![one.clone(), two];
    mark_ties(&mut list);
    assert!(list.iter().all(|found| found.tie), "{list:#?}");
    // Negative: one candidate is no tie.
    let mut list = vec![one];
    mark_ties(&mut list);
    assert!(!list[0].tie);
}

const SWAP_OLD: &str = "fn _(_1: u32, _2: u32) -> u32 {\n    let _0: u32;\n    let _3: (u32, bool);\n    let _4: (u32, bool);\n    let _5: (u32, bool);\n    bb0: {\n        _3 = CheckedMul(_1, const u32 = 2);\n        assert(!(_3.1: bool), overflow Mul(_1, const u32 = 2)) -> [success: bb1, unwind continue];\n    }\n    bb1: {\n        _4 = CheckedMul(_2, const u32 = 3);\n        assert(!(_4.1: bool), overflow Mul(_2, const u32 = 3)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n        _5 = CheckedAdd((_3.0: u32), (_4.0: u32));\n        assert(!(_5.1: bool), overflow Add((_3.0: u32), (_4.0: u32))) -> [success: bb3, unwind continue];\n    }\n    bb3: {\n        _0 = (_5.0: u32);\n        return;\n    }\n}\n";
const SWAP_EL: &str = r#"{"n": 8, "f": ["src/lib.rs"], "r": [[0, 2, 0, 50, 55], [2, 2, 0, 69, 74], [4, 3, 0, 80, 85], [7, 1, 0, 87, 87]], "s": [["depot::total", 0, 0, 87]], "y": [[0, 8, 0]], "v": 1, "o": 0, "l": [[0, 0, 32, 35], [1, 0, 13, 14, 0, 16, 19], [1, 0, 21, 22, 0, 24, 27], [3, 0, 50, 55], [3, 0, 69, 74], [3, 0, 80, 85]]}"#;

fn swapped(old: &str) -> String {
    old.replace("_3 = CheckedMul(_1, const u32 = 2)", "#A").replace("_4 = CheckedMul(_2, const u32 = 3)", "#B").replace("assert(!(_3.1: bool), overflow Mul(_1, const u32 = 2))", "#C").replace("assert(!(_4.1: bool), overflow Mul(_2, const u32 = 3))", "#D")
        .replace("#A", "_3 = CheckedMul(_2, const u32 = 3)").replace("#B", "_4 = CheckedMul(_1, const u32 = 2)").replace("#C", "assert(!(_3.1: bool), overflow Mul(_2, const u32 = 3))").replace("#D", "assert(!(_4.1: bool), overflow Mul(_1, const u32 = 2))")
}

// rule+ R94.4-3
// rule- R94.4-2
#[test]
fn two_statements_that_each_can_panic_depend_on_each_other() {
    let el = record(SWAP_EL);
    let found = permutation((SWAP_OLD, &el), (&swapped(SWAP_OLD), &el)).expect("a new order of the same parts");
    assert!(found.flips > 0 && found.dependent.iter().any(|(_, _, cause)| cause.contains("each can panic")), "{found:#?}");
}

// rule+ R94.4-2
// rule- R94.4-3
#[test]
fn two_statements_with_no_effect_and_no_panic_are_independent() {
    let pure = SWAP_OLD.replace("CheckedMul(_1, const u32 = 2)", "BitXor(_1, const u32 = 2)").replace("CheckedMul(_2, const u32 = 3)", "BitOr(_2, const u32 = 3)");
    let pure = pure.replace("        assert(!(_3.1: bool), overflow Mul(_1, const u32 = 2)) -> [success: bb1, unwind continue];\n    }\n    bb1: {\n", "").replace("        assert(!(_4.1: bool), overflow Mul(_2, const u32 = 3)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n", "");
    let pure = pure.replace("let _3: (u32, bool);", "let _3: u32;").replace("let _4: (u32, bool);", "let _4: u32;").replace("(_3.0: u32)", "_3").replace("(_4.0: u32)", "_4");
    let el = record(r#"{"n": 6, "f": ["src/lib.rs"], "r": [[0, 1, 0, 50, 55], [1, 1, 0, 69, 74], [2, 3, 0, 80, 85], [5, 1, 0, 87, 87]], "v": 1}"#);
    let new = pure.replace("_3 = BitXor(_1, const u32 = 2);\n        _4 = BitOr(_2, const u32 = 3);", "_3 = BitOr(_2, const u32 = 3);\n        _4 = BitXor(_1, const u32 = 2);");
    assert_ne!(new, pure);
    let found = permutation((&pure, &el), (&new, &el)).expect("a new order of the same parts");
    assert!(found.flips > 0 && found.dependent.is_empty(), "{found:#?}");
    // Negative: a change of one value is not a new order.
    assert!(permutation((&pure, &el), (&new.replace("const u32 = 3", "const u32 = 4"), &el)).is_none());
}

// rule+ R94.4-1
#[test]
fn a_switch_on_values_in_another_order_is_a_new_order() {
    assert_eq!(sorted_switch("switchInt(_1) -> [1: bb3, 0: bb2, otherwise: bb1]"), "switchInt(_1) -> [0: bb2, 1: bb3, otherwise: bb1]");
    assert_eq!(sorted_switch("_3 = CheckedAdd(_1, _2)"), "_3 = CheckedAdd(_1, _2)");
}


// rule+ R94.2-1
// rule- R94.2-1
#[test]
fn the_old_callee_put_in_at_the_old_call_aligns_with_the_new_code() {
    let (old, new, callee) = (record(NEW_TOTAL_EL), record(OLD_TOTAL_EL), record(PART_EL));
    let before = elements::diff((NEW_TOTAL, &old), (OLD_TOTAL, &new)).unwrap();
    let sides = [Side { name: "depot::total", text: NEW_TOTAL, record: &old }, Side { name: "depot::total", text: OLD_TOTAL, record: &new }];
    let found = check([&sides[0], &sides[1]], &Side { name: "depot::part", text: PART, record: &callee }, Kind::Inline, &before);
    assert!(found.len() == 1 && found[0].kind == Kind::Inline && found[0].holds && found[0].equal, "{found:#?}");
    // Negative: an old callee with one other operand does not give equal code.
    let other = PART.replace("const u32 = 3", "const u32 = 5");
    let found = check([&sides[0], &sides[1]], &Side { name: "depot::part", text: &other, record: &callee }, Kind::Inline, &before);
    assert!(!found[0].equal, "{found:#?}");
}

// rule- R94.1-1
#[test]
fn a_callee_with_new_code_is_no_extract() {
    // The getter `self.total ^ 7` in place of a field read: the old code has no element of the
    // form of the `^` (new code), so the check does not hold.
    let callee_text = "fn _(_1: &depot::Cart) -> u32 {\n    let _0: u32;\n    let _2: u32;\n    bb0: {\n        _2 = ((*_1).depot::Cart.total: u32);\n        _0 = BitXor(_2, const u32 = 7);\n        return;\n    }\n}\n";
    let callee_el = record(r#"{"n": 3, "f": ["src/lib.rs"], "r": [[0, 1, 0, 143, 153], [1, 1, 0, 143, 157], [2, 1, 0, 159, 159]], "v": 1}"#);
    let new_text = "fn _(_1: &depot::Cart) -> u32 {\n    let _0: u32;\n    let _2: &depot::Cart;\n    let _3: u32;\n    let _4: (u32, bool);\n    bb0: {\n        _2 = &shared (*_1);\n        _3 = const for<'a> fn(&'a depot::Cart) -> u32 {depot::Cart::total}(_2) -> [return: bb1, unwind continue];\n    }\n    bb1: {\n        _4 = CheckedAdd(_3, const u32 = 1);\n        assert(!(_4.1: bool), overflow Add(_3, const u32 = 1)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n        _0 = (_4.0: u32);\n        return;\n    }\n}\n";
    let new_el = record(r#"{"n": 6, "f": ["src/lib.rs"], "r": [[0, 1, 0, 244, 245], [1, 1, 0, 244, 253], [2, 3, 0, 244, 257], [5, 1, 0, 259, 259]], "v": 1}"#);
    let old_text = "fn _(_1: &depot::Cart) -> u32 {\n    let _0: u32;\n    let _2: u32;\n    let _3: (u32, bool);\n    bb0: {\n        _2 = ((*_1).depot::Cart.total: u32);\n        _3 = CheckedAdd(_2, const u32 = 1);\n        assert(!(_3.1: bool), overflow Add(_2, const u32 = 1)) -> [success: bb1, unwind continue];\n    }\n    bb1: {\n        _0 = (_3.0: u32);\n        return;\n    }\n}\n";
    let old_el = record(r#"{"n": 5, "f": ["src/lib.rs"], "r": [[0, 1, 0, 244, 251], [1, 3, 0, 244, 255], [4, 1, 0, 257, 257]], "v": 1}"#);
    let before = elements::diff((old_text, &old_el), (new_text, &new_el)).unwrap();
    let sides = [Side { name: "depot::a", text: old_text, record: &old_el }, Side { name: "depot::a", text: new_text, record: &new_el }];
    let found = check([&sides[0], &sides[1]], &Side { name: "depot::Cart::total", text: callee_text, record: &callee_el }, Kind::Extract, &before);
    assert!(found.iter().all(|found| !found.holds), "{found:#?}");
    // A callee with the old code and one more operation (`(a * 3 + b) ^ 7`): half of its places
    // are equal, and the `^` has no element of its form in the old code.
    let more = PART.replace("_0 = (_4.0: u32);", "_0 = BitXor((_4.0: u32), const u32 = 7);");
    let found = extract(&more);
    assert!(found.iter().all(|found| !found.holds), "{found:#?}");
    // The plain getter (a field read) is an extract.
    let getter = callee_text.replace("        _0 = BitXor(_2, const u32 = 7);\n", "").replace("_2 = ((*_1)", "_0 = ((*_1)").replace("    let _2: u32;\n", "");
    let getter_el = record(r#"{"n": 2, "f": ["src/lib.rs"], "r": [[0, 1, 0, 143, 153], [1, 1, 0, 159, 159]], "v": 1}"#);
    let found = check([&sides[0], &sides[1]], &Side { name: "depot::Cart::total", text: &getter, record: &getter_el }, Kind::Extract, &before);
    assert!(found.len() == 1 && found[0].holds && found[0].equal, "{found:#?}");
}

// rule+ R94.1-1
#[test]
fn a_place_is_a_local_a_field_or_a_downcast_of_a_place() {
    assert_eq!(place_local("_3"), Some(3));
    assert_eq!(place_local("(_3.0: u32)"), Some(3));
    assert_eq!(place_local("((_4 as depot::E::A).depot::E::A.0: u32)"), Some(4));
    assert_eq!(place_local("(_7.depot::Pair.a: u32)"), Some(7));
    // An rvalue, a deref, a constant, and an index are no place of the forward.
    assert_eq!(place_local("BitXor(_2, const u32 = 7)"), None);
    assert_eq!(place_local("((*_1).depot::Cart.total: u32)"), None);
    assert_eq!(place_local("(*_1)"), None);
    assert_eq!(place_local("const u32 = 1"), None);
    assert_eq!(place_local("_3[_4]"), None);
}

// ---------------------------------------------------------------------------------------------
// Design 95.

// rule+ R95.3-1
// rule- R95.3-1
#[test]
fn the_generic_arguments_of_a_call_bind_the_parameters_of_the_callee() {
    let params = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
    assert_eq!(bind_args("depot::part::<u32>", "depot::part", &params(&["T"])), Some(vec!["u32".to_string()]));
    assert_eq!(bind_args("depot::Wrap::<u32>::get", "depot::Wrap::<T>::get", &params(&["T"])), Some(vec!["u32".to_string()]));
    assert_eq!(bind_args("<depot::Wrap<(u8, u16)> as depot::Tax>::rate", "<depot::Wrap<T> as depot::Tax>::rate", &params(&["T"])), Some(vec!["(u8, u16)".to_string()]));
    assert_eq!(bind_args("depot::Wrap::<u32>::map::<f64>", "depot::Wrap::<T>::map", &params(&["T", "U"])), Some(vec!["u32".to_string(), "f64".to_string()]));
    assert_eq!(bind_args("depot::scaled::<4>", "depot::scaled", &params(&["N"])), Some(vec!["4".to_string()]));
    assert_eq!(bind_args("depot::pair::<u8, u16>", "depot::pair", &params(&["A", "B"])), Some(vec!["u8".to_string(), "u16".to_string()]));
    assert_eq!(skeleton("<depot::Wrap<u32> as depot::Tax>::rate"), "<depot::Wrap as depot::Tax>::rate");
    assert_eq!(skeleton("depot::Wrap::<u32>::get"), "depot::Wrap::get");
    // Negative: an other path, an other count of arguments, and two values of one parameter.
    assert_eq!(bind_args("depot::other::<u32>", "depot::part", &params(&["T"])), None);
    assert_eq!(bind_args("depot::part::<u32, u8>", "depot::part", &params(&["T"])), None);
    assert_eq!(bind_args("<depot::Pair<u8, u16> as depot::Tax>::rate", "<depot::Pair<T, T> as depot::Tax>::rate", &params(&["T"])), None);
}

const GENERIC: &str = "fn _(_1: T, _2: T) -> T generics <T#0> {\n    let _0: T;\n    bb0: {\n        _0 = const fn(T, T) -> T {<T as std::cmp::Ord>::max}(_1, _2) -> [return: bb1, unwind: bb2];\n    }\n    bb1: {\n        return;\n    }\n    bb2: {\n        resume;\n    }\n}\n";
const GENERIC_EL: &str = r#"{"n": 3, "f": ["src/lib.rs"], "r": [[0, 1, 0, 90, 98], [1, 1, 0, 100, 100], [2, 1, 0, 30, 100]], "v": 1}"#;

// rule+ R95.3-1
// rule- R95.3-1
#[test]
fn a_generic_callee_takes_the_arguments_of_the_call() {
    let record = record(GENERIC_EL);
    let text = parse_text(GENERIC, &record).expect("a text");
    assert_eq!(text.generics, vec![("T".to_string(), 0)]);
    let instance = substitute(&text, &["u32".to_string()]).expect("the instance");
    let printed = instance.print();
    // The trait method on the parameter type has the self type of the call; the cleanup block
    // with only `resume` is `unwind continue`, as in the MIR of an instance.
    assert!(printed.contains("fn _(_1: u32, _2: u32) -> u32 {") && printed.contains("{<u32 as std::cmp::Ord>::max}(_1, _2) -> [return: bb1, unwind continue]"), "{printed}");
    // A const parameter takes the value of the call; a path, a field, and a string keep the name.
    assert_eq!(replace_param("_2 = CheckedMul(_1, const usize = param N#0)", "N", 0, "4"), "_2 = CheckedMul(_1, const usize = 4)");
    assert_eq!(replace_param("_3 = depot::T::new(const \"T\", (_1.T: u8), T)", "T", 0, "u32"), "_3 = depot::T::new(const \"T\", (_1.T: u8), u32)");
    // Negative: a parameter with the name of a primitive type, and a count that does not fit.
    let shadow = parse_text(&GENERIC.replace('T', "u8").replace("u8#0", "u8#0"), &record).expect("a text");
    assert!(substitute(&shadow, &["u32".to_string()]).is_err());
    assert!(substitute(&text, &["u32".to_string(), "u8".to_string()]).is_err());
    // Negative: a drop of a value with a destructor stays; a drop of a scalar goes.
    assert!(no_drop_glue("u32") && no_drop_glue("&std::string::String") && no_drop_glue("(u8, [bool; 4])") && no_drop_glue("*mut u8"));
    assert!(!no_drop_glue("std::string::String") && !no_drop_glue("(u8, std::vec::Vec<u8>)") && !no_drop_glue("T"));
}

// rule+ R95.2-1
// rule- R95.2-1
#[test]
fn a_call_whose_summary_has_no_effect_is_no_effect_part() {
    let form = r#"{"a": 1, "l": [], "t": [], "f": ["src/lib.rs"], "b": [{"s": [], "t": {"k": {"Call": [{"p": "depot::f", "w": "depot::f", "e": {}}, [], {"l": 2}, 1, "C"]}, "r": [0, 10, 14]}}, {"s": [], "t": {"k": {"Call": [{"p": "depot::g", "w": "depot::g", "e": {"p": true}}, [], {"l": 3}, 2, "C"]}, "r": [0, 20, 24]}}, {"s": [], "t": {"k": {"Call": [{"p": "depot::h", "e": {"w": "a static that can change", "p": true}}, [], {"l": 4}, 3, "C"]}, "r": [0, 30, 34]}}, {"s": [], "t": {"k": {"Call": [{"p": "depot::k"}, [], {"l": 5}, 4, "C"]}, "r": [0, 40, 44]}}]}"#;
    let facts = call_facts(form).expect("the facts");
    let at = |start: usize, end: usize| Place { file: "src/lib.rs".into(), start, end };
    assert_eq!(summary_at(Some(&facts), Some(&at(10, 14))), Some((true, false)));
    assert_eq!(summary_at(Some(&facts), Some(&at(20, 24))), Some((true, true)));
    assert_eq!(facts[&at(10, 14)][0].resolved.as_deref(), Some("depot::f"));
    // Negative: an effect on the world, no summary, and no form at the place: an effect.
    assert_eq!(summary_at(Some(&facts), Some(&at(30, 34))), Some((false, true)));
    assert_eq!(summary_at(Some(&facts), Some(&at(40, 44))), None);
    assert_eq!(summary_at(Some(&facts), Some(&at(50, 54))), None);
    assert!(call_facts(r#"{"u": "no form: too large"}"#).is_none());
    // A call with no effect and a `&mut` argument writes its target: an effect part.
    let text = parse_text("fn _(_1: &mut u32) -> u32 {\n    let _0: u32;\n    bb0: {\n        _0 = const fn(&mut u32) -> u32 {depot::f}(_1) -> [return: bb1, unwind continue];\n    }\n    bb1: {\n        return;\n    }\n}\n", &record(r#"{"n": 2, "f": ["src/lib.rs"], "r": [[0, 1, 0, 10, 14], [1, 1, 0, 15, 15]], "v": 1}"#)).expect("a text");
    let element = &text.blocks[0].1[0];
    assert_eq!(effect_facts(&text, &element.text, Some(&facts), element.tokens.as_ref()), (true, true, false, false));
}

// rule- R95.4-1
#[test]
fn a_trait_call_that_the_records_do_not_resolve_gives_no_check() {
    let (old, new) = (record(OLD_TOTAL_EL), record(NEW_TOTAL_EL));
    let trait_call = NEW_TOTAL.replace("{depot::part}", "{<depot::Food as depot::Tax>::part}");
    let before = elements::diff((OLD_TOTAL, &old), (&trait_call, &new)).unwrap();
    let sides = [Side { name: "depot::total", text: OLD_TOTAL, record: &old }, Side { name: "depot::total", text: &trait_call, record: &new }];
    let target = Target { path: "<depot::Food as depot::Tax>::part".into(), name: "<depot::Food as depot::Tax>::part".into(), args: Vec::new(), closure: false };
    let callee = record(PART_EL);
    let found = check_target([&sides[0], &sides[1]], &Side { name: &target.name, text: PART, record: &callee }, &target, Kind::Extract, &before, None);
    assert!(found.iter().all(|found| !found.holds && found.refused.as_deref().is_some_and(|cause| cause.contains("do not resolve"))), "{found:#?}");
    // A form that resolves the call to an other body: no check either.
    let mut facts = CallFacts::new();
    facts.insert(Place { file: "src/lib.rs".into(), start: 50, end: 60 }, vec![CallFact { summary: true, resolved: Some("<depot::Fuel as depot::Tax>::part".into()), ..Default::default() }]);
    let found = check_target([&sides[0], &sides[1]], &Side { name: &target.name, text: PART, record: &callee }, &target, Kind::Extract, &before, Some(&facts));
    assert!(found.iter().all(|found| !found.holds), "{found:#?}");
}

// rule+ R95.5-3
// rule- R95.5-3
#[test]
fn a_value_that_goes_to_a_place_through_a_local_is_one_write() {
    let one = "fn _(_1: &mut u32, _2: u32) -> () {\n    let _0: ();\n    let _3: u32;\n    bb0: {\n        _3 = BitXor(_2, const u32 = 7);\n        (*_1) = _3;\n        _0 = const ();\n        return;\n    }\n}\n";
    let el = record(r#"{"n": 4, "f": ["src/lib.rs"], "r": [[0, 1, 0, 40, 46], [1, 1, 0, 30, 46], [2, 2, 0, 48, 48]], "v": 1}"#);
    let mut text = parse_text(one, &el).expect("a text");
    assert!(fold_into_place(&mut text, 3));
    assert_eq!(text.blocks[0].1[0].text, "(*_1) = BitXor(_2, const u32 = 7)");
    assert_eq!(text.blocks[0].1[0].tokens.as_ref().map(|place| place.start), Some(40));
    // Negative: a second use of the local, and an element between the two.
    let twice = one.replace("        _0 = const ();\n", "        _0 = const ();\n        (*_1) = _3;\n");
    let el_twice = record(r#"{"n": 5, "f": ["src/lib.rs"], "r": [[0, 1, 0, 40, 46], [1, 1, 0, 30, 46], [2, 3, 0, 48, 48]], "v": 1}"#);
    let mut text = parse_text(&twice, &el_twice).expect("a text");
    assert!(!fold_into_place(&mut text, 3));
    let between = one.replace("        (*_1) = _3;\n        _0 = const ();\n", "        _0 = const ();\n        (*_1) = _3;\n");
    let mut text = parse_text(&between, &el).expect("a text");
    assert!(!fold_into_place(&mut text, 3));
}

// rule+ R95.1-1
// rule- R95.1-1
#[test]
fn a_residue_element_must_differ_in_the_map_of_the_body() {
    let el = record(SWAP_EL);
    let changed = swapped(SWAP_OLD).replace("_5 = CheckedAdd((_3.0: u32), (_4.0: u32))", "_5 = CheckedSub((_3.0: u32), (_4.0: u32))").replace("overflow Add((_3.0: u32), (_4.0: u32))", "overflow Sub((_3.0: u32), (_4.0: u32))");
    let diff = elements::diff((SWAP_OLD, &el), (&changed, &el)).expect("a diff");
    let found = permutation_with((SWAP_OLD, &el), (&changed, &el), [None, None], Some(&diff)).expect("a new order of the region");
    assert!(found.flips > 0 && found.residue.iter().all(|list| !list.is_empty()), "{found:#?}");
    // Negative: with no element that differs in the map, the residue does not fit: no new order.
    assert!(permutation_with((SWAP_OLD, &el), (&changed, &el), [None, None], Some(&ElementDiff::default())).is_none());
    // Negative: with no diff, the whole body must be a new order.
    assert!(permutation((SWAP_OLD, &el), (&changed, &el)).is_none());
}
