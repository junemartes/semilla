//! Tests of the SMT checks. The tests need a Z3 binary (see scripts/fetch-z3.sh).

use std::time::Duration;

use semilla_smt::{
    Effect, Equivalence, RustType, Solver, TypeEnv, check_rule, classify_condition, equivalent,
    equivalent_no_panic,
};

fn solver() -> Solver {
    Solver::find().expect("a Z3 solver (run scripts/fetch-z3.sh)")
}

fn env(pairs: &[(&str, &str)]) -> TypeEnv {
    let mut env = TypeEnv::new();
    for (name, ty) in pairs {
        env.insert_text(name, ty);
    }
    env
}

/// Reads `name = value` pairs from a witness.
fn values(witness: &str) -> Vec<(String, String)> {
    let pairs = witness.split("; ").next().unwrap_or("");
    pairs
        .split(", ")
        .filter_map(|pair| pair.split_once(" = "))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn value(witness: &str, name: &str) -> String {
    values(witness)
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no value for {name} in {witness}"))
        .1
}

fn effect(before: &str, after: &str, env: &TypeEnv) -> semilla_smt::EffectResult {
    classify_condition(&solver(), before, after, env).unwrap()
}

fn is_proved(result: &Equivalence) -> bool {
    matches!(result, Equivalence::Proved { .. })
}

#[test]
fn widen_with_witness() {
    let env = env(&[("x", "u32"), ("n", "u32")]);
    let result = effect("x < n", "x <= n", &env);
    assert_eq!(result.effect, Effect::Widen, "{result:?}");
    let witness = result.witness.unwrap();
    assert_eq!(value(&witness, "x"), value(&witness, "n"), "{witness}");
    assert!(result.exact);
    assert!(!result.panic_conditions_differ);
    assert!(result.query.contains("bvult"));
}

#[test]
fn narrow_with_witness() {
    let env = env(&[("a", "bool"), ("b", "bool")]);
    let result = effect("a", "a && b", &env);
    assert_eq!(result.effect, Effect::Narrow, "{result:?}");
    let witness = result.witness.unwrap();
    assert_eq!(value(&witness, "a"), "true");
    assert_eq!(value(&witness, "b"), "false");
}

#[test]
fn narrow_with_unknown_bool_types() {
    // `&&` fixes the type of `a` and `b` as `bool`.
    let result = effect("a || b", "a", &TypeEnv::new());
    assert_eq!(result.effect, Effect::Narrow, "{result:?}");
}

#[test]
fn negate() {
    let env = env(&[("x", "i32"), ("n", "i32")]);
    assert_eq!(effect("x < n", "x >= n", &env).effect, Effect::Negate);
    assert_eq!(
        effect("a && b", "!a || !b", &TypeEnv::new()).effect,
        Effect::Negate
    );
}

#[test]
fn equivalent_conditions() {
    let env = env(&[("a", "i64"), ("b", "i64")]);
    let result = effect("!(a < b)", "a >= b", &env);
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
    assert!(result.witness.is_none());
    // A guard change on integers.
    let env = self::env(&[("x", "u8")]);
    assert_eq!(effect("x > 0", "x >= 1", &env).effect, Effect::Equivalent);
    assert_eq!(effect("x > 0", "x != 0", &env).effect, Effect::Equivalent);
}

#[test]
fn equivalent_with_unknown_integer_types() {
    // The literal fixes the type as an integer, so the Int encoding applies.
    let result = effect("x > 0", "x >= 1", &TypeEnv::new());
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
    assert!(!result.exact);
    let result = effect("x + 1 > y", "x >= y", &TypeEnv::new());
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
    // But `x + 1` can overflow and `x` cannot.
    assert!(result.panic_conditions_differ);
}

#[test]
fn unknown_types_without_a_literal_are_unrelated() {
    // The operands can be floats, and then `!(a < b)` is not `a >= b` (NaN).
    let result = effect("!(a < b)", "a >= b", &TypeEnv::new());
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    assert!(
        result.notes[0].contains("unknown type"),
        "{:?}",
        result.notes
    );
}

#[test]
fn unrelated_with_witness() {
    let env = env(&[("x", "u32"), ("y", "u32")]);
    let result = effect("x < 5", "y < 5", &env);
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    let witness = result.witness.unwrap();
    let (x, y): (u32, u32) = (
        value(&witness, "x").parse().unwrap(),
        value(&witness, "y").parse().unwrap(),
    );
    assert_ne!(x < 5, y < 5, "{witness}");
}

#[test]
fn signed_and_unsigned_differ() {
    // On a signed type, `x >= 0` is not always true.
    let unsigned = env(&[("x", "u32")]);
    assert_eq!(
        effect("x >= 0", "true", &unsigned).effect,
        Effect::Equivalent
    );
    let signed = env(&[("x", "i32")]);
    let result = effect("x >= 0", "true", &signed);
    assert_eq!(result.effect, Effect::Widen);
    assert!(value(result.witness.as_deref().unwrap(), "x").starts_with('-'));

    // The sign of a cast changes the comparison.
    let env = env(&[("x", "i8")]);
    let result = effect("x < 0", "(x as u8) > 127", &env);
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
    let result = effect("(x as u8) < 100", "x < 100", &env);
    assert_eq!(result.effect, Effect::Widen, "{result:?}");
}

#[test]
fn widths_are_exact() {
    let env = env(&[("x", "u8")]);
    assert_eq!(effect("x <= 255", "true", &env).effect, Effect::Equivalent);
    assert_eq!(
        effect("x == u8::MAX", "x > 254", &env).effect,
        Effect::Equivalent
    );
    let env = self::env(&[("x", "u16")]);
    assert_eq!(effect("x <= 255", "true", &env).effect, Effect::Widen);
}

#[test]
fn an_unknown_integer_type_gives_no_panic_example() {
    // Trial 3, D7: `cost > account.cents` → `cost > account.cents + 100` with no type for the
    // field gave the example "account . cents = 38; after panics", which is not real for `u64`.
    let result = effect("cost > account . cents", "cost > account . cents + 100", &env(&[]));
    assert!(result.panic_conditions_differ, "{result:?}");
    assert!(result.panic_witness.is_none(), "{result:?}");
    assert!(result.notes.iter().any(|n| n.contains("no example of the panic difference") && n.contains("account . cents")), "{:?}", result.notes);
    // With the type of the field, the example is real: the sum overflows.
    let result = effect("cost > account . cents", "cost > account . cents + 100", &env(&[("cost", "u64"), ("account . cents", "u64")]));
    let witness = result.panic_witness.unwrap();
    let cents: u128 = value(&witness, "account . cents").parse().unwrap();
    assert!(cents + 100 > u64::MAX as u128, "{witness}");
}

#[test]
fn overflow_changes_the_panic_conditions() {
    let env = env(&[("x", "u32"), ("y", "u32")]);
    // Under "no panic", `x + 1 > y` is `x >= y`. But `x + 1` panics for `x = u32::MAX`.
    let result = effect("x + 1 > y", "x >= y", &env);
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
    assert!(result.panic_conditions_differ);
    let witness = result.panic_witness.unwrap();
    assert_eq!(value(&witness, "x"), u32::MAX.to_string(), "{witness}");
    assert!(witness.contains("before panics"), "{witness}");

    // Division by zero: the guard protects the division.
    let result = effect("y != 0 && x / y > 1", "y != 0 && x / y >= 2", &env);
    assert_eq!(result.effect, Effect::Equivalent);
    assert!(!result.panic_conditions_differ, "{result:?}");
    let result = effect("y != 0 && x / y > 1", "x / y > 1 && y != 0", &env);
    assert!(result.panic_conditions_differ);
    assert!(result.panic_witness.unwrap().contains("y = 0"));

    // Signed overflow of `MIN / -1`.
    let env = self::env(&[("a", "i32"), ("b", "i32")]);
    let result = effect(
        "b != 0 && a / b > 0",
        "b != 0 && b != -1 && a / b > 0",
        &env,
    );
    assert!(result.panic_conditions_differ, "{result:?}");
    let witness = result.panic_witness.unwrap();
    assert_eq!(value(&witness, "a"), i32::MIN.to_string(), "{witness}");
    assert_eq!(value(&witness, "b"), "-1", "{witness}");
    assert!(
        witness.ends_with("; before panics, after does not"),
        "{witness}"
    );
}

#[test]
fn atoms_are_uninterpreted() {
    let env = env(&[("v.len()", "usize")]);
    // The encoding does not know that `is_empty` means `len() == 0`.
    let result = effect("v.len() > 0", "!v.is_empty()", &env);
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    // The same atom on the two sides is the same value.
    let result = effect("v.len() > 0", "v.len() >= 1", &env);
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
    assert!(!result.exact);
    // A field read is an atom without side effects.
    let env = self::env(&[("self.count", "u32")]);
    assert_eq!(
        effect("self.count < 10", "self.count <= 9", &env).effect,
        Effect::Equivalent
    );
    // `*x` and `&x` have the value of `x`.
    let env = self::env(&[("x", "&u32")]);
    assert_eq!(effect("*x > 3", "x >= &4", &env).effect, Effect::Equivalent);
}

#[test]
fn call_order_changes_are_unrelated() {
    let result = effect("f() && g()", "g() && f()", &TypeEnv::new());
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    assert!(
        result.notes[0].contains("different order"),
        "{:?}",
        result.notes
    );
    // The same order, but `f()` runs only when `x` is true.
    let result = effect("f() & x", "x && f()", &TypeEnv::new());
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    assert!(
        result.notes[0].contains("runs on one side"),
        "{:?}",
        result.notes
    );
    // A call that is gone.
    let env = env(&[("x", "u32")]);
    let result = effect("x > 0 && check(x)", "x > 0", &env);
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    // The same calls under equivalent guards are fine.
    let result = effect("x > 0 && check(x)", "x >= 1 && check(x)", &env);
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
}

#[test]
fn reads_after_a_call_are_different_values() {
    // `bump()` can change `x`, so `x` before the call is not `x` after the call.
    let env = env(&[("x", "u32"), ("bump()", "bool")]);
    let result = effect("(x > 5) & bump()", "bump() & (x > 5)", &env);
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    assert!(
        result.notes.is_empty() || !result.notes[0].contains("does not support"),
        "{result:?}"
    );
    let result = effect("bump() & (x > 5)", "bump() & (x >= 6)", &env);
    assert_eq!(result.effect, Effect::Equivalent, "{result:?}");
}

#[test]
fn repeated_calls_are_different_values() {
    let env = env(&[("next()", "u32")]);
    // Two calls to `next()` can give different values.
    let result = effect("next() == next() || true", "true", &env);
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    let result = equivalent(&solver(), "next() - next()", "0", &env).unwrap();
    assert!(!is_proved(&result), "{result:?}");
}

#[test]
fn floats_are_unrelated() {
    let env = env(&[("x", "f64")]);
    let result = effect("x < 1.0", "x <= 1.0", &env);
    assert_eq!(result.effect, Effect::Unrelated);
    assert!(result.notes[0].contains("floats"), "{:?}", result.notes);
    let result = effect("y < 1.0", "y < 1.0", &TypeEnv::new());
    assert_eq!(result.effect, Effect::Unrelated);
    let result = equivalent(&solver(), "x + 0.0", "x", &env).unwrap();
    assert!(matches!(result, Equivalence::NotProved { .. }));
}

#[test]
fn chars() {
    let env = env(&[("c", "char")]);
    assert_eq!(
        effect("c >= 'a' && c <= 'z'", "('a'..='z').contains(&c)", &env).effect,
        Effect::Unrelated
    );
    assert_eq!(
        effect("c > 'a'", "c >= 'b'", &env).effect,
        Effect::Equivalent
    );
    // No char lies in the surrogate range.
    assert_eq!(
        effect("c as u32 >= 0xD800 && c as u32 <= 0xDFFF", "false", &env).effect,
        Effect::Equivalent
    );
    assert_eq!(
        effect("c as u32 <= 0x10FFFF", "true", &env).effect,
        Effect::Equivalent
    );
}

#[test]
fn shifts() {
    let env = env(&[("x", "u32"), ("k", "u32")]);
    assert_eq!(
        effect("x >> 1 == 0", "x < 2", &env).effect,
        Effect::Equivalent
    );
    // A shift by `k` panics when `k >= 32`.
    let result = effect("x << k > 0", "x << k != 0", &env);
    assert_eq!(result.effect, Effect::Equivalent);
    assert!(!result.panic_conditions_differ);
    let result = effect("x << k > 0", "x > 0", &env);
    assert!(result.panic_conditions_differ);
    // An arithmetic shift on a signed type.
    let env = self::env(&[("y", "i8")]);
    assert_eq!(
        effect("y >> 7 == -1", "y < 0", &env).effect,
        Effect::Equivalent
    );
}

#[test]
fn equivalence_of_values() {
    let s = solver();
    let env = env(&[("a", "u32"), ("b", "u32"), ("x", "u32")]);
    assert!(is_proved(&equivalent(&s, "a + b", "b + a", &env).unwrap()));
    assert!(is_proved(&equivalent(&s, "a & b | a", "a", &env).unwrap()));
    let result = equivalent(&s, "a - b", "b - a", &env).unwrap();
    assert!(matches!(result, Equivalence::Refuted { .. }), "{result:?}");
    // `x * 2` and `x << 1` are equal when no overflow happens.
    let result = equivalent_no_panic(&s, "x * 2", "x << 1", &env).unwrap();
    assert!(is_proved(&result), "{result:?}");
    // But `x * 2` panics on overflow and `x << 1` does not.
    let result = equivalent(&s, "x * 2", "x << 1", &env).unwrap();
    let Equivalence::Refuted { counterexample } = result else {
        panic!("{result:?}")
    };
    assert!(counterexample.contains("before panics"), "{counterexample}");
    // The condition form of the same check.
    let result = effect("x * 2 == a", "x << 1 == a", &env);
    assert_eq!(result.effect, Effect::Equivalent);
    assert!(result.panic_conditions_differ);
}

#[test]
fn equivalence_with_unknown_types() {
    let s = solver();
    let env = TypeEnv::new();
    // The proof holds for each integer type, with the overflow conditions.
    assert!(is_proved(&equivalent(&s, "a + 1", "1 + a", &env).unwrap()));
    let result = equivalent(&s, "(a + 1) - 1", "a", &env).unwrap();
    assert!(
        matches!(result, Equivalence::NotProved { .. }),
        "{result:?}"
    );
    assert!(is_proved(
        &equivalent_no_panic(&s, "(a + 1) - 1", "a", &env).unwrap()
    ));
    // A model of an unknown type is not a real counterexample.
    let result = equivalent(&s, "a - 1", "1 - a", &env).unwrap();
    assert!(
        matches!(result, Equivalence::NotProved { .. }),
        "{result:?}"
    );
}

#[test]
fn timeouts_are_not_proofs() {
    // A tiny time limit makes a hard nonlinear query time out.
    let s = solver().with_timeout(Duration::from_millis(1));
    let env = env(&[("a", "u64"), ("b", "u64"), ("c", "u64")]);
    let result = equivalent(&s, "(a * b) * c", "a * (b * c)", &env).unwrap();
    let Equivalence::NotProved { reason } = result else {
        panic!("{result:?}")
    };
    assert!(
        reason.contains("no answer") || reason.contains("did not show"),
        "{reason}"
    );
    let env = self::env(&[("a", "u64"), ("b", "u64")]);
    let result = classify_condition(
        &s,
        "a * b == 0x00C4B8E1A42D9A11",
        "a * b == 0x00C4B8E1A42D9A11 && a > 1",
        &env,
    )
    .unwrap();
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
}

#[test]
fn rewrite_rules() {
    let s = solver();
    let ints = env(&[("a", "i32"), ("b", "i32"), ("c", "i32")]);
    let uints = env(&[("a", "u32"), ("b", "u32")]);
    let bools = env(&[("p", "bool"), ("q", "bool")]);
    // Sound rules.
    assert!(is_proved(
        &check_rule(&s, "!(a < b)", "a >= b", &ints).unwrap()
    ));
    assert!(is_proved(
        &check_rule(&s, "!(p && q)", "!p || !q", &bools).unwrap()
    ));
    assert!(is_proved(&check_rule(&s, "a + b", "b + a", &ints).unwrap()));
    assert!(is_proved(&check_rule(&s, "a * 1", "a", &ints).unwrap()));
    assert!(is_proved(&check_rule(&s, "a ^ a", "0", &uints).unwrap()));
    assert!(is_proved(
        &check_rule(&s, "a >> 1", "a / 2", &uints).unwrap()
    ));
    assert!(is_proved(&check_rule(&s, "!!p", "p", &bools).unwrap()));
    // Unsound rules.
    let rule = check_rule(&s, "(a + b) + c", "a + (b + c)", &ints).unwrap();
    // The values agree, but the overflow points differ.
    assert!(matches!(rule, Equivalence::Refuted { .. }), "{rule:?}");
    let rule = check_rule(&s, "a >> 1", "a / 2", &ints).unwrap();
    let Equivalence::Refuted { counterexample } = rule else {
        panic!("{rule:?}")
    };
    assert!(
        value(&counterexample, "a").starts_with('-'),
        "{counterexample}"
    );
    let rule = check_rule(&s, "a * 2", "a << 1", &uints).unwrap();
    assert!(matches!(rule, Equivalence::Refuted { .. }), "{rule:?}");
    let rule = check_rule(&s, "-(a - b)", "b - a", &ints).unwrap();
    assert!(matches!(rule, Equivalence::Refuted { .. }), "{rule:?}");
    let rule = check_rule(&s, "p || q", "p", &bools).unwrap();
    assert!(matches!(rule, Equivalence::Refuted { .. }), "{rule:?}");
}

#[test]
fn unsupported_forms_are_unrelated() {
    let env = env(&[("x", "u32")]);
    // A type mismatch.
    let result = effect("x", "x > 0", &env);
    assert_eq!(result.effect, Effect::Unrelated);
    // An expression that is not valid Rust gives an error.
    assert!(classify_condition(&solver(), "x <", "x", &env).is_err());
}

#[test]
fn the_cache_gives_the_same_answer() {
    let s = solver();
    let env = env(&[("x", "u32"), ("n", "u32")]);
    let first = classify_condition(&s, "x < n", "x <= n", &env).unwrap();
    let second = classify_condition(&s, "x < n", "x <= n", &env).unwrap();
    assert_eq!(first, second);
}

#[test]
fn solver_facts() {
    let s = solver();
    assert!(s.version().contains("Z3"), "{}", s.version());
    assert!(Solver::available());
    assert_eq!(s.timeout(), Duration::from_secs(2));
}

#[test]
fn type_parsing() {
    assert_eq!(
        TypeEnv::parse_type("usize"),
        RustType::Int {
            bits: 64,
            signed: false
        }
    );
    assert_eq!(
        TypeEnv::parse_type("&u32"),
        RustType::Int {
            bits: 32,
            signed: false
        }
    );
}

#[test]
fn a_proof_holds_for_each_build_configuration() {
    // Batch 5k, A1: the check build has a pointer width of 64 bits and overflow checks. A
    // proof for this build only is not a proof.
    // The two sides are equal on each input with no overflow, and they panic on the same
    // inputs. With no overflow checks, the left side is `b > 0` and the right side is not.
    let u8s = env(&[("a", "u8"), ("b", "u8")]);
    let result = effect("(a + b) - a > 0", "a + b > a", &u8s);
    assert_eq!(result.effect, Effect::Unrelated, "{result:?}");
    assert!(result.notes.iter().any(|note| note.contains("no proof for a build with a pointer width of 64 bits, no overflow checks")), "{result:?}");
    assert!(!is_proved(&equivalent(&solver(), "(a + b) - a > 0", "a + b > a", &u8s).unwrap()));
    // The width of `usize`: equal for 64 bits, different for 32 bits.
    let sizes = env(&[("n", "usize")]);
    let result = effect("n.wrapping_add(1 << 31).wrapping_add(1 << 31) == n", "n != n", &sizes);
    assert_ne!(result.effect, Effect::Equivalent, "{result:?}");
    let result = effect("(n as u64) < 0x1_0000_0000", "n == n", &sizes);
    assert_ne!(result.effect, Effect::Equivalent, "{result:?}");
    // The honest cases stay proved: they hold for each width and with wrapped results.
    assert_eq!(effect("n > 0", "n >= 1", &sizes).effect, Effect::Equivalent);
    assert_eq!(effect("a + b > 3", "b + a > 3", &u8s).effect, Effect::Equivalent);
    assert!(is_proved(&equivalent(&solver(), "a + b", "b + a", &u8s).unwrap()));
}
