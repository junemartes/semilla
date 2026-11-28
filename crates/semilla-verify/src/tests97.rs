//! Unit tests of design 97 (`rewrites.rs`): the texts are records of the driver for the crate of
//! the cases of `map97/` (the old body `shop::Cart::total` and the two closures of the intent
//! crate).

use super::*;

const OLD_TOTAL: &str = "fn _(_1: &shop::Cart) -> usize {\n    let _0: usize;\n    let _2: &std::vec::Vec<u32>;\n    let _3: usize;\n    let _4: (usize, bool);\n    bb0: {\n        _2 = &shared ((*_1).shop::Cart.items: std::vec::Vec<u32>);\n        _3 = const for<'a> fn(&'a std::vec::Vec<u32>) -> usize {std::vec::Vec::<u32>::len}(_2) -> [return: bb1, unwind continue];\n    }\n    bb1: {\n        _4 = CheckedAdd(_3, const usize = 1);\n        assert(!(_4.1: bool), overflow Add(_3, const usize = 1)) -> [success: bb2, unwind continue];\n    }\n    bb2: {\n        _0 = (_4.0: usize);\n        return;\n    }\n}\n";
const NEW_TOTAL: &str = "fn _(_1: &shop::Cart) -> usize {\n    let _0: usize;\n    let _2: usize;\n    let _3: (usize, bool);\n    bb0: {\n        _2 = ((*_1).shop::Cart.count: usize);\n        _3 = CheckedAdd(_2, const usize = 1);\n        assert(!(_3.1: bool), overflow Add(_2, const usize = 1)) -> [success: bb1, unwind continue];\n    }\n    bb1: {\n        _0 = (_3.0: usize);\n        return;\n    }\n}\n";
const OLD_CLOSURE: &str = "fn _(_1: &{closure#0}, _2: &before::shop::Cart) -> usize generics <#0, #1, #2> {\n    let _0: usize;\n    let _3: &std::vec::Vec<u32>;\n    bb0: {\n        _3 = &shared ((*_2).before::shop::Cart.items: std::vec::Vec<u32>);\n        _0 = const for<'a> fn(&'a std::vec::Vec<u32>) -> usize {std::vec::Vec::<u32>::len}(_3) -> [return: bb1, unwind continue];\n    }\n    bb1: {\n        return;\n    }\n}\n";
const NEW_CLOSURE: &str = "fn _(_1: &{closure#0}, _2: &after::shop::Cart) -> usize generics <#0, #1, #2> {\n    let _0: usize;\n    bb0: {\n        _0 = ((*_2).after::shop::Cart.count: usize);\n        return;\n    }\n}\n";

// rule+ R97.1-1
// rule- R97.1-1
#[test]
fn a_closure_of_the_intent_crate_is_a_pattern_and_a_capture_is_an_error() {
    let old = pattern(OLD_CLOSURE).unwrap();
    assert_eq!((old.arity(), old.size, old.ret.as_str()), (1, 2, "usize"), "{old:#?}");
    assert_eq!(old.params.get(&2).map(String::as_str), Some("&shop::Cart"));
    let new = pattern(NEW_CLOSURE).unwrap();
    assert_eq!((new.arity(), new.size), (1, 1));
    // Negative: a closure that reads its environment (a capture), and a closure that calls an
    // item of the intent file.
    let capture = NEW_CLOSURE.replace("_0 = ((*_2).after::shop::Cart.count: usize)", "_0 = ((*_1).0: usize)");
    assert!(pattern(&capture).unwrap_err().contains("a capture"));
    let call = OLD_CLOSURE.replace("{std::vec::Vec::<u32>::len}", "{semilla_claims::step::length}");
    assert!(pattern(&call).unwrap_err().contains("an item of the intent file"));
    // [R97.2-4] A path of the standard library has one name: a crate with only `core` prints
    // `core::ops::Range` for the `std::ops::Range` of the intent crate.
    assert_eq!(std_paths("&std::ops::Range<usize>, alloc::vec::Vec<u8>, my_std::x"), "&core::ops::Range<usize>, core::vec::Vec<u8>, my_std::x");
    // The facade of a version goes; a name that only ends with the word does not change.
    assert_eq!(strip_facade("&before::shop::Cart, x::before::y, unbefore::z"), "&shop::Cart, x::before::y, unbefore::z");
}

// rule+ R97.2-2
// rule- R97.2-2
#[test]
fn the_old_closure_fits_the_old_body_with_one_binding_from_the_elements() {
    let (body, old) = (parse_mir(OLD_TOTAL), pattern(OLD_CLOSURE).unwrap());
    let found = matches_at(&body, &old, 0, &Binding::default(), &mut Near::default());
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].elements, vec![0, 1]);
    assert_eq!(found[0].binding.values.get(&2).map(String::as_str), Some("_1"));
    assert_eq!(found[0].binding.values.get(&0).map(String::as_str), Some("_3"));
    assert!(check_found(&body, &old, &found[0]).is_ok());
    // The new closure fits the new body with the binding of the old side (the map of the locals
    // gives `_1` → `_1` and `_3` → `_2`).
    let new_body = parse_mir(NEW_TOTAL);
    let mut bound = Binding::default();
    bound.values.insert(2, "_1".into());
    bound.values.insert(0, "_2".into());
    let found = matches_at(&new_body, &pattern(NEW_CLOSURE).unwrap(), 0, &bound, &mut Near::default());
    assert_eq!(found.len(), 1, "{found:#?}");
    // Negative: the old closure does not fit at an other start, and the near data names the
    // element that differs.
    let mut near = Near::default();
    assert!(matches_at(&body, &old, 2, &Binding::default(), &mut near).is_empty());
    assert_eq!(near.fits, 0);
}

// rule+ R97.2-3
// rule- R97.2-3
#[test]
fn the_binding_of_the_new_side_is_the_old_binding_under_the_map_of_the_locals() {
    let table = MirTable { texts: [OLD_TOTAL.into(), NEW_TOTAL.into()], locals: vec![(0, 0), (1, 1), (3, 2), (4, 3)], ..Default::default() };
    let mut old = Binding::default();
    old.values.insert(2, "_1".into());
    old.values.insert(0, "_3".into());
    let new = mapped_binding(&table, &old).unwrap();
    assert_eq!(new.values.get(&0).map(String::as_str), Some("_2"));
    // Negative: a local with no partner gives no binding of the new side.
    old.values.insert(0, "_9".into());
    assert!(mapped_binding(&table, &old).unwrap_err().contains("no partner"));
}

// rule+ R97.2-5
// rule- R97.2-5
#[test]
fn a_parameter_binds_to_one_operand_and_a_temporary_of_the_site_stays_inside() {
    assert_eq!(operand_end("Add(((*_1).a: u32), const u32 = 2)", 4, Some(", const u32 = 2)")), Some(18));
    // Negative: an operand has no comma at its top level.
    assert_eq!(operand_end("Add(_1, _2)", 4, Some(")")), None);
    // Negative: a temporary of the site that the body reads at an other place.
    let (body, old) = (parse_mir(OLD_TOTAL), pattern(OLD_CLOSURE).unwrap());
    let read = OLD_TOTAL.replace("_4 = CheckedAdd(_3, const usize = 1)", "_4 = CheckedAdd(_3, Len(_2))");
    let found = matches_at(&parse_mir(&read), &old, 0, &Binding::default(), &mut Near::default());
    assert!(check_found(&parse_mir(&read), &old, &found[0]).unwrap_err().contains("at an other place"));
    let found = matches_at(&body, &old, 0, &Binding::default(), &mut Near::default());
    assert!(check_found(&body, &old, &found[0]).is_ok());
}

// rule+ R97.2-6
// rule- R97.2-6
#[test]
fn the_type_of_a_binding_is_the_type_of_the_parameter() {
    let (body, old) = (parse_mir(OLD_TOTAL), pattern(OLD_CLOSURE).unwrap());
    assert_eq!(operand_type(&body, "((*_1).shop::Cart.items: std::vec::Vec<u32>)").as_deref(), Some("std::vec::Vec<u32>"));
    assert_eq!(operand_type(&body, "const usize = 1").as_deref(), Some("usize"));
    // Negative: a closure that takes the cart by value does not bind to a reference.
    let by_value = OLD_CLOSURE.replace("_2: &before::shop::Cart", "_2: before::shop::Cart").replace("(*_2)", "_2");
    let by_value = pattern(&by_value).unwrap();
    let text = OLD_TOTAL.replace("((*_1).shop::Cart.items", "(_1.shop::Cart.items");
    let found = matches_at(&parse_mir(&text), &by_value, 0, &Binding::default(), &mut Near::default());
    assert!(check_found(&parse_mir(&text), &by_value, &found[0]).unwrap_err().contains("has the type `&shop::Cart`"), "{found:#?}");
    let found = matches_at(&body, &old, 0, &Binding::default(), &mut Near::default());
    assert!(check_found(&body, &old, &found[0]).is_ok());
}

// rule+ R97.6-1
// rule- R97.6-1
#[test]
fn a_symmetric_operation_with_a_local_gives_its_two_orders() {
    let (list, count) = forms("_4 = BitXor(_2, _3)");
    assert_eq!((list.len(), count), (2, 1), "{list:?}");
    assert!(list.contains(&"_4 = BitXor(_3, _2)".to_string()));
    // Negative: an operation that is not symmetric keeps its one order.
    assert_eq!(forms("_4 = Sub(_2, _3)").0.len(), 1);
}

// rule+ R97.7-1
// rule- R97.7-1
#[test]
fn the_key_of_a_fragment_renames_the_locals_and_keeps_the_callees() {
    let table = MirTable { texts: [OLD_TOTAL.into(), NEW_TOTAL.into()], locals: vec![(0, 0), (1, 1), (3, 2), (4, 3)], ..Default::default() };
    let mirs = [parse_mir(OLD_TOTAL), parse_mir(NEW_TOTAL)];
    let key = fragment_key(&mirs, &table, &[0, 1], &[0]);
    // Other numbers of the locals give the same key.
    let renumbered = [OLD_TOTAL.replace("_2", "_7").replace("_3", "_8"), NEW_TOTAL.replace("_2", "_9")];
    let other = MirTable { texts: renumbered.clone(), locals: vec![(0, 0), (1, 1), (8, 9)], ..Default::default() };
    let other_mirs = [parse_mir(&renumbered[0]), parse_mir(&renumbered[1])];
    assert_eq!(fragment_key(&other_mirs, &other, &[0, 1], &[0]), key);
    // Negative: an other callee gives an other key.
    let callee = [OLD_TOTAL.replace("::len}", "::capacity}"), NEW_TOTAL.to_string()];
    let callee_mirs = [parse_mir(&callee[0]), parse_mir(&callee[1])];
    assert_ne!(fragment_key(&callee_mirs, &table, &[0, 1], &[0]), key);
}

// rule+ R97.8-1
// rule- R97.8-1
#[test]
fn the_type_of_a_template_parameter_has_the_version_of_each_path_of_the_workspace() {
    let crates: BTreeSet<String> = ["shop".to_string()].into();
    assert_eq!(intent_type("&shop::Cart", 0, &crates).unwrap(), "&before::shop::Cart");
    assert_eq!(intent_type("std::vec::Vec<shop::Item>", 1, &crates).unwrap(), "std::vec::Vec<after::shop::Item>");
    // A lifetime is the elided lifetime: the generic arguments keep their commas.
    assert_eq!(intent_type("&mut shop::Entry<'a, u32>", 0, &crates).unwrap(), "&mut before::shop::Entry<'_, u32>");
    // Negative: a closure type and an opaque type have no path.
    assert!(intent_type("{closure@src/lib.rs:3:9}", 0, &crates).unwrap_err().contains("closure"));
    assert!(intent_type("impl std::fmt::Debug", 0, &crates).unwrap_err().contains("opaque"));
}

