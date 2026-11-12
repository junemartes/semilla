//! SMT checks for semilla (design section 8.8).
//!
//! The crate translates two Rust expressions into SMT-LIB2 and asks a Z3
//! process about their relation:
//!
//! - [`classify_condition`] gives the effect of a condition change: narrow,
//!   widen, negate, equivalent, or unrelated.
//! - [`equivalent`] proves that a neutral expression rewrite keeps the value
//!   and the panic behavior.
//! - [`equivalent_no_panic`] proves that the value is the same when neither side panics.
//! - [`check_rule`] tests a rewrite rule (design B24).
//!
//! # Soundness
//!
//! A proof (`Equivalent`, `Negate`, the implication in `Narrow` and `Widen`,
//! and `Proved`) comes only from an `unsat` answer. A timeout, `unknown`, an
//! unsupported expression, or a float gives `Unrelated` or `NotProved`.
//!
//! The non-implication part of `Narrow`, `Widen`, and `Unrelated`, and the
//! counterexamples, come from a model. A model is a real input only when the
//! field `exact` is true. If `exact` is false, the encoding has atoms, unknown
//! types, or uninterpreted operations, and the model can be impossible in Rust.
//! For this reason [`equivalent`] gives `Refuted` only for an exact model.
//!
//! ## Atoms and calls
//!
//! An expression that the encoding cannot model (a call, a method call,
//! indexing, a macro, a block, a field read, a constant path) is an atom. An
//! atom is an uninterpreted constant. The same text gives the same constant,
//! with two exceptions that keep the encoding sound when calls have side effects:
//!
//! - The k-th call with a given text is a different constant from the other
//!   calls with that text.
//! - A read of a variable or a field after k calls is a different constant
//!   from a read after j calls (a call can change the variable).
//!
//! The two sides must have the same sequence of call texts, and each call
//! must run under an equivalent guard (the `&&` and `||` conditions above
//! it). If not, the result is `Unrelated` or `NotProved`. Then each call runs
//! in the same state on the two sides and gives the same value.
//!
//! ## Soundness of the Int encoding
//!
//! An integer with an unknown type becomes a mathematical `Int`. Each unknown
//! type gets an uninterpreted predicate `in_range_c`, which stands for the
//! value range of the type. The inputs and the literals of the type are in
//! range. An operation that can overflow (`+ - * /`, `%`, unary `-`) does not
//! panic only if its mathematical result is in range.
//!
//! The argument: take any real integer type T for the class. Interpret
//! `in_range_c` as the range of T. If no operation overflows, each Rust
//! operation gives the same value as the mathematical operation, so the
//! Rust values are the values of the encoding. Thus each real execution is
//! a model of the encoding, and a formula that is valid for all models is
//! valid for T. Operations that depend on the width (`!x`, `<<`, `& | ^`)
//! are uninterpreted functions, which also have T as one interpretation.
//!
//! An unknown type is an integer only when an integer literal or a shift
//! fixes it. Without that, the operands can be floats or user types, and the
//! check gives `Unrelated` or `NotProved`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use semilla_core::report::Effect;

pub mod build;
mod encode;
mod lower;
mod solver;
mod types;

pub use solver::{DEFAULT_SEED, DEFAULT_TIMEOUT, SOLVER_ENV, Solver, TOOLS_PATH};
pub use types::{RustType, TypeEnv};

use encode::{Problem, Skip, Top};
use solver::Answer;

/// The result of [`classify_condition`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectResult {
    /// The effect, computed under the assumption that neither side panics.
    pub effect: Effect,
    /// An example input in `name = value` form, for `Narrow`, `Widen`, and `Unrelated`.
    /// For `Narrow`, the input takes the branch before and not after.
    /// For `Widen`, the input takes the branch after and not before.
    pub witness: Option<String>,
    /// The SMT-LIB queries, for the reviewer.
    pub query: String,
    /// True if the panic conditions (overflow, division by zero, shift
    /// amount) of the two sides can be different. Also true when the solver
    /// cannot show that they are the same.
    pub panic_conditions_differ: bool,
    /// An input on which one side panics and the other side does not.
    pub panic_witness: Option<String>,
    /// True if each witness is a real input. See the crate documentation.
    pub exact: bool,
    /// Notes for the reviewer, for example why the result is `Unrelated`.
    pub notes: Vec<String>,
}

impl EffectResult {
    fn unrelated(note: String, query: String) -> EffectResult {
        EffectResult {
            effect: Effect::Unrelated,
            witness: None,
            query,
            panic_conditions_differ: false,
            panic_witness: None,
            exact: false,
            notes: vec![note],
        }
    }
}

/// The result of [`equivalent`], [`equivalent_no_panic`], and [`check_rule`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Equivalence {
    /// The solver proved the equivalence. `query` holds the SMT-LIB queries.
    Proved { query: String },
    /// The solver did not prove the equivalence. This is not evidence of a difference.
    NotProved { reason: String },
    /// A real input on which the two sides are different.
    Refuted { counterexample: String },
}

/// The assumption of the effect checks: valid inputs, and no panic on either side.
const NO_PANIC: &str = "(and inputs_valid no_panic_before no_panic_after)";

/// Runs the queries of one check and keeps their text.
struct Run<'a> {
    solver: &'a Solver,
    problem: &'a Problem,
    queries: Vec<String>,
}

impl<'a> Run<'a> {
    fn new(solver: &'a Solver, problem: &'a Problem) -> Self {
        Run {
            solver,
            problem,
            queries: Vec::new(),
        }
    }

    fn check(&mut self, label: &str, assertions: &[&str], model: bool) -> Result<Answer> {
        let symbols = if model {
            self.problem.sym_names()
        } else {
            Vec::new()
        };
        let (query, answer) = self
            .solver
            .check(&self.problem.body(assertions), &symbols)?;
        self.queries.push(format!("; ---- {label}\n{query}"));
        Ok(answer)
    }

    fn query(&self) -> String {
        self.queries.join("\n")
    }

    /// Gives a reason if the calls of the two sides can run differently.
    fn calls_differ(&mut self) -> Result<Option<String>> {
        let (before, after) = (&self.problem.before, &self.problem.after);
        if before.calls != after.calls {
            return Ok(Some(format!(
                "the calls are different or in a different order (before: [{}], after: [{}]); calls can have side effects",
                before.calls.join(", "),
                after.calls.join(", ")
            )));
        }
        if before.calls.is_empty() {
            return Ok(None);
        }
        let differences: Vec<String> = before
            .call_guards
            .iter()
            .zip(&after.call_guards)
            .filter(|(b, a)| b != a)
            .map(|(b, a)| format!("(distinct {b} {a})"))
            .collect();
        if differences.is_empty() {
            return Ok(None);
        }
        let assertion = format!("(or {})", differences.join(" "));
        let answer = self.check(
            "a call runs on one side and not on the other",
            &["inputs_valid", &assertion],
            true,
        )?;
        Ok(match answer {
            Answer::Unsat => None,
            Answer::Sat(model) => Some(format!(
                "a call runs on one side and not on the other (for example, {}); calls can have side effects",
                self.problem.witness(&model)
            )),
            Answer::Unknown(reason) => Some(format!(
                "the solver did not show that the calls run under the same conditions ({reason})"
            )),
        })
    }

    /// Looks for an input on which exactly one side panics.
    fn panic_difference(&mut self) -> Result<Result<(), PanicDifference>> {
        if !self.problem.before.can_panic && !self.problem.after.can_panic {
            return Ok(Ok(()));
        }
        let mut unknown = None;
        for (label, assertion, text) in [
            (
                "before panics and after does not",
                "(and (not no_panic_before) no_panic_after)",
                "before panics, after does not",
            ),
            (
                "after panics and before does not",
                "(and no_panic_before (not no_panic_after))",
                "after panics, before does not",
            ),
        ] {
            match self.check(label, &["inputs_valid", assertion], true)? {
                Answer::Unsat => {}
                Answer::Sat(model) => {
                    return Ok(Err(PanicDifference::Found(format!(
                        "{}; {text}",
                        self.problem.witness(&model)
                    ))));
                }
                Answer::Unknown(reason) => unknown = Some(reason),
            }
        }
        match unknown {
            Some(reason) => Ok(Err(PanicDifference::Unknown(reason))),
            None => Ok(Ok(())),
        }
    }
}

enum PanicDifference {
    Found(String),
    Unknown(String),
}

/// Lowers and encodes the two sides.
fn prepare(before: &str, after: &str, env: &TypeEnv, top: Top) -> Result<Result<Problem, String>> {
    let before = lower::parse(before)?;
    let after = lower::parse(after)?;
    let sides = lower::lower(&before).and_then(|b| lower::lower(&after).map(|a| (b, a)));
    let (before, after) = match sides {
        Ok(sides) => sides,
        Err(unsupported) => return Ok(Err(Skip::from(unsupported).reason())),
    };
    Ok(Problem::build(&before, &after, env, top).map_err(|skip| skip.reason()))
}

/// Computes the effect of a condition change from `before` to `after`.
///
/// The effect is computed under the assumption that neither side panics. The
/// field `panic_conditions_differ` tells if the panic conditions are different.
pub fn classify_condition(
    solver: &Solver,
    before: &str,
    after: &str,
    env: &TypeEnv,
) -> Result<EffectResult> {
    let result = classify_one(solver, before, after, env)?;
    if result.effect != Effect::Equivalent || result.panic_conditions_differ {
        return Ok(result);
    }
    // "Equivalent" is a proof, and a proof must hold for each build configuration.
    for other in &build::ALL[1..] {
        let holds = build::with(*other, || classify_one(solver, before, after, env)).is_ok_and(|other| other.effect == Effect::Equivalent && !other.panic_conditions_differ);
        if !holds {
            return Ok(EffectResult::unrelated(format!("the two sides are equivalent in the check build, and there is no proof for a build with {other}"), result.query));
        }
    }
    Ok(result)
}

/// [`classify_condition`] for the build configuration of this thread.
fn classify_one(
    solver: &Solver,
    before: &str,
    after: &str,
    env: &TypeEnv,
) -> Result<EffectResult> {
    let problem = match prepare(before, after, env, Top::Condition)? {
        Ok(problem) => problem,
        Err(reason) => return Ok(EffectResult::unrelated(reason, String::new())),
    };
    let mut run = Run::new(solver, &problem);
    if let Some(reason) = run.calls_differ()? {
        return Ok(EffectResult::unrelated(reason, run.query()));
    }

    let mut notes = Vec::new();
    let (effect, witness) = 'effect: {
        match run.check(
            "an input exists on which neither side panics",
            &[NO_PANIC],
            false,
        )? {
            Answer::Sat(_) => {}
            Answer::Unsat => {
                notes.push("each valid input makes one of the sides panic".into());
                break 'effect (Effect::Unrelated, None);
            }
            Answer::Unknown(reason) => {
                notes.push(format!("the solver gave no answer ({reason})"));
                break 'effect (Effect::Unrelated, None);
            }
        }
        let differ = run.check(
            "before and after differ",
            &[NO_PANIC, "(distinct before after)"],
            true,
        )?;
        if differ == Answer::Unsat {
            break 'effect (Effect::Equivalent, None);
        }
        let negate = run.check(
            "after is not the negation of before",
            &[NO_PANIC, "(= before after)"],
            false,
        )?;
        if negate == Answer::Unsat {
            break 'effect (Effect::Negate, None);
        }
        let gained = run.check(
            "after holds and before does not",
            &[NO_PANIC, "after", "(not before)"],
            true,
        )?;
        let lost = run.check(
            "before holds and after does not",
            &[NO_PANIC, "before", "(not after)"],
            true,
        )?;
        match (&gained, &lost) {
            (Answer::Unsat, Answer::Sat(model)) => (Effect::Narrow, Some(problem.witness(model))),
            (Answer::Sat(model), Answer::Unsat) => (Effect::Widen, Some(problem.witness(model))),
            (Answer::Sat(model), Answer::Sat(_)) => {
                let model = match &differ {
                    Answer::Sat(model) => model,
                    _ => model,
                };
                (Effect::Unrelated, Some(problem.witness(model)))
            }
            _ => {
                for answer in [&differ, &negate, &gained, &lost] {
                    if let Answer::Unknown(reason) = answer {
                        notes.push(format!("the solver gave no answer ({reason})"));
                        break;
                    }
                }
                (Effect::Unrelated, None)
            }
        }
    };

    let (panic_conditions_differ, panic_witness) = match run.panic_difference()? {
        Ok(()) => (false, None),
        Err(PanicDifference::Found(witness)) => {
            // B3: an input of an unknown integer type has no real width in the model, so the
            // model is not an example. The text says that no example exists.
            let unknown = run.problem.unknown_width_inputs();
            if unknown.is_empty() {
                (true, Some(witness))
            } else {
                notes.push(format!("no example of the panic difference: the integer type of {} is not known", unknown.join(", ")));
                (true, None)
            }
        }
        Err(PanicDifference::Unknown(reason)) => {
            notes.push(format!(
                "the solver did not show that the panic conditions are the same ({reason})"
            ));
            (true, None)
        }
    };
    if !problem.exact && witness.is_some() {
        notes.push(
            "the witness can be impossible in Rust: the encoding has atoms or unknown types".into(),
        );
    }
    Ok(EffectResult {
        effect,
        witness,
        query: run.query(),
        panic_conditions_differ,
        panic_witness,
        exact: problem.exact,
        notes,
    })
}

/// Proves that `after` has the same value as `before` for each input, and
/// that the two sides panic on the same inputs.
pub fn equivalent(
    solver: &Solver,
    before: &str,
    after: &str,
    env: &TypeEnv,
) -> Result<Equivalence> {
    compare(solver, before, after, env, true)
}

/// Proves that `after` has the same value as `before` for each input on which
/// neither side panics. The panic conditions can be different.
pub fn equivalent_no_panic(
    solver: &Solver,
    before: &str,
    after: &str,
    env: &TypeEnv,
) -> Result<Equivalence> {
    compare(solver, before, after, env, false)
}

/// Tests a rewrite rule `lhs => rhs` (design B24). The pattern variables are
/// the free names of the rule, with their types in `env`. The rule is sound
/// only if the two sides have the same value and the same panic conditions.
pub fn check_rule(solver: &Solver, lhs: &str, rhs: &str, env: &TypeEnv) -> Result<Equivalence> {
    compare(solver, lhs, rhs, env, true)
}

fn compare(
    solver: &Solver,
    before: &str,
    after: &str,
    env: &TypeEnv,
    strict: bool,
) -> Result<Equivalence> {
    let result = compare_one(solver, before, after, env, strict)?;
    // The check with `strict == false` is about the inputs with no panic in the check build. It
    // is not a proof of equal behavior, and no caller uses it as one.
    if !strict || !matches!(result, Equivalence::Proved { .. }) {
        return Ok(result);
    }
    // A proof must hold for each build configuration.
    for other in &build::ALL[1..] {
        let holds = build::with(*other, || compare_one(solver, before, after, env, strict)).is_ok_and(|other| matches!(other, Equivalence::Proved { .. }));
        if !holds {
            return Ok(Equivalence::NotProved { reason: format!("the two sides are equal in the check build, and there is no proof for a build with {other}") });
        }
    }
    Ok(result)
}

/// [`compare`] for the build configuration of this thread.
fn compare_one(
    solver: &Solver,
    before: &str,
    after: &str,
    env: &TypeEnv,
    strict: bool,
) -> Result<Equivalence> {
    let problem = match prepare(before, after, env, Top::SameType)? {
        Ok(problem) => problem,
        Err(reason) => return Ok(Equivalence::NotProved { reason }),
    };
    let mut run = Run::new(solver, &problem);
    if let Some(reason) = run.calls_differ()? {
        return Ok(Equivalence::NotProved { reason });
    }
    let refute = |witness: String, what: &str| {
        if problem.exact {
            Equivalence::Refuted {
                counterexample: witness,
            }
        } else {
            Equivalence::NotProved {
                reason: format!(
                    "{what} for a model that can be impossible in Rust (the encoding has atoms or unknown types): {witness}"
                ),
            }
        }
    };
    match run.check(
        "the values differ",
        &[NO_PANIC, "(distinct before after)"],
        true,
    )? {
        Answer::Unsat => {}
        Answer::Sat(model) => return Ok(refute(problem.witness(&model), "the values differ")),
        Answer::Unknown(reason) => {
            return Ok(Equivalence::NotProved {
                reason: format!("the solver gave no answer ({reason})"),
            });
        }
    }
    if strict {
        match run.panic_difference()? {
            Ok(()) => {}
            Err(PanicDifference::Found(witness)) => {
                return Ok(refute(witness, "the panic conditions differ"));
            }
            Err(PanicDifference::Unknown(reason)) => {
                return Ok(Equivalence::NotProved {
                    reason: format!(
                        "the solver did not show that the panic conditions are the same ({reason})"
                    ),
                });
            }
        }
    }
    Ok(Equivalence::Proved { query: run.query() })
}
