//! A run of the real compiled code confirms each sure difference (design 74, G5 part 4).
//!
//! The model of the equivalence check and of the claims gives an exact input and the outcome of
//! each call of a version. The probe (`semilla/src/probe.rs`) builds a small crate that depends
//! on the tree of the version, runs each call on the input, and gives the outcome of the run.
//! This module has the data of a run and the compare. It does not build and it does not run.
//!
//! # Soundness
//!
//! - A run never makes a result "equivalent" or "proved": it only confirms a difference, or it
//!   shows that the model and the run disagree (a defect of Semilla).
//! - A part of an outcome that the model does not give (a value with no exact model, a type
//!   with its own `Debug` form, a panic with no message in the model) is never a confirmation
//!   and never a disagreement: the compare says that it cannot decide.

use serde::{Deserialize, Serialize};

/// [R74.1-2] The start of the cause of a result whose run gives another outcome than the model
/// (a defect of Semilla): "not decided: the model and the run disagree (the run: …)".
pub const DISAGREE: &str = "the model and the run disagree";

/// The version of a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RunSide {
    Before,
    After,
}

impl RunSide {
    pub fn word(self) -> &'static str {
        match self {
            RunSide::Before => "before",
            RunSide::After => "after",
        }
    }
}

/// How the probe gives an argument to the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgMode {
    /// The value.
    Value,
    /// A shared reference to the value.
    Ref,
    /// A `&mut` reference to the value: the probe shows the value after the call.
    Mut,
}

/// One argument of a call: its name, a Rust expression of its value (of the target for a
/// reference), or the cause why the model gives no such expression.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunArg {
    pub name: String,
    pub expr: Result<String, String>,
    pub mode: ArgMode,
}

/// The exit of a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunExit {
    /// A return: the `Debug` text of the value, or `None` if the model does not give it.
    Returns(Option<String>),
    /// A panic: its message, or `None` if the model does not give it.
    Panics(Option<String>),
    Aborts,
}

/// The outcome of a call that the probe can see: the exit, and the `Debug` text of the value
/// behind each `&mut` argument after the call (`None`: the model does not give it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOut {
    pub exit: RunExit,
    pub muts: Vec<Option<String>>,
}

/// One call of a version on the input of a result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCall {
    pub side: RunSide,
    /// The record name of the body (`depot::add`, `<depot::P as core::ops::Add>::add`).
    pub name: String,
    /// The compiled target of the body (`depot.lib.<hash>`); empty until the oracle sets it.
    pub target: String,
    pub args: Vec<RunArg>,
    /// The outcome of the model.
    pub expect: RunOut,
}

/// The calls of one sure difference: the two versions of a body for an equivalence result,
/// each call of a version on the failed path of a false claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCase {
    pub calls: Vec<RunCall>,
    /// The cause why a run cannot confirm the result, when the model knows it before a run (a
    /// difference in an effect that the probe cannot see).
    pub blind: Option<String>,
}

/// The state of the run of a sure difference.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunNote {
    /// No data of a run (a result of a test with no model).
    #[default]
    None,
    /// The calls that the probe must run.
    Open(Box<RunCase>),
    /// The run gives the outcomes of the model.
    Confirmed,
    /// No run, or a run that cannot decide: the cause.
    NotConfirmed(String),
}

impl RunNote {
    /// The text after the outcomes of a difference: "confirmed by a run", or "not confirmed by
    /// a run: CAUSE". Empty for `None`.
    pub fn text(&self) -> String {
        match self {
            RunNote::None => String::new(),
            RunNote::Open(_) => "not confirmed by a run: the step ran no probe".into(),
            RunNote::Confirmed => "confirmed by a run".into(),
            RunNote::NotConfirmed(cause) => format!("not confirmed by a run: {cause}"),
        }
    }
}

/// The outcome of one run of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    pub exit: RunExit,
    /// The `Debug` text of the value behind each `&mut` argument after the call.
    pub muts: Vec<String>,
}

impl Ran {
    /// The text of the outcome: "returns 5", "panics with the message "too big"", "aborts".
    pub fn text(&self) -> String {
        let mut text = match &self.exit {
            RunExit::Returns(Some(value)) => format!("returns {value}"),
            RunExit::Returns(None) => "returns".into(),
            RunExit::Panics(Some(message)) => format!("panics with the message {message:?}"),
            RunExit::Panics(None) => "panics".into(),
            RunExit::Aborts => "aborts".into(),
        };
        for (index, value) in self.muts.iter().enumerate() {
            text.push_str(&format!(" with `&mut` argument {} = {value}", index + 1));
        }
        text
    }
}

/// The compare of the outcome of the model with the outcome of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// The run gives the outcome of the model.
    Same,
    /// The run gives another outcome.
    Differs,
    /// The model does not give a part that the run shows: the cause.
    Unknown(String),
}

/// [R74.1-2] Compares the outcome of the model with the outcome of a run: the exit kind, the
/// `Debug` text of a returned value, the message of a panic, and the value behind each `&mut`
/// argument. A part that the model does not give is never a difference.
pub fn compare(expect: &RunOut, ran: &Ran) -> Check {
    let mut unknown: Option<String> = None;
    match (&expect.exit, &ran.exit) {
        (RunExit::Returns(Some(want)), RunExit::Returns(Some(got))) => {
            if want != got {
                return Check::Differs;
            }
        }
        (RunExit::Returns(None), RunExit::Returns(_)) => unknown = Some("the model does not give the `Debug` text of the returned value".into()),
        (RunExit::Returns(Some(_)), RunExit::Returns(None)) => unknown = Some("the run does not give the returned value".into()),
        (RunExit::Panics(Some(want)), RunExit::Panics(Some(got))) => {
            if want != got {
                return Check::Differs;
            }
        }
        (RunExit::Panics(_), RunExit::Panics(_)) => {}
        (RunExit::Aborts, RunExit::Aborts) => {}
        _ => return Check::Differs,
    }
    for (want, got) in expect.muts.iter().zip(&ran.muts) {
        match want {
            Some(want) if want != got => return Check::Differs,
            Some(_) => {}
            None => {
                unknown.get_or_insert_with(|| "the model does not give the value behind a `&mut` argument".into());
            }
        }
    }
    match unknown {
        Some(cause) => Check::Unknown(cause),
        None => Check::Same,
    }
}

/// [R74.1-2] The cause why a run cannot confirm a difference of two outcomes of the model, or
/// `None`: a run confirms a difference only if the two outcomes differ in a part that the probe
/// sees (the exit kind, a value or a message that the model gives, a value behind a `&mut`
/// argument).
pub fn blind_cause(before: &RunOut, after: &RunOut) -> Option<String> {
    let seen = |exit: &RunExit| match exit {
        RunExit::Returns(_) => 0,
        RunExit::Panics(_) => 1,
        RunExit::Aborts => 2,
    };
    if seen(&before.exit) != seen(&after.exit) {
        return None;
    }
    let differ = |a: &Option<String>, b: &Option<String>| matches!((a, b), (Some(a), Some(b)) if a != b);
    let exits = match (&before.exit, &after.exit) {
        (RunExit::Returns(a), RunExit::Returns(b)) | (RunExit::Panics(a), RunExit::Panics(b)) => differ(a, b),
        _ => false,
    };
    if exits || before.muts.iter().zip(&after.muts).any(|(a, b)| differ(a, b)) {
        return None;
    }
    Some(match &before.exit {
        RunExit::Panics(_) => "the two versions panic, and the model does not give the two messages".into(),
        _ => "the difference is in a part of the outcome that the probe does not see (an effect, or a value that the model does not give)".into(),
    })
}

/// The text of a Rust string literal that `{:?}` of a `String` writes: the text itself, or
/// `None` for a text with a bad form.
pub fn unquote(literal: &str) -> Option<String> {
    let inner = literal.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let mut hex = String::new();
                loop {
                    let next = chars.next()?;
                    if next == '}' {
                        break;
                    }
                    hex.push(next);
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(exit: RunExit) -> RunOut {
        RunOut { exit, muts: Vec::new() }
    }

    // rule+ R74.1-2
    // rule- R74.1-2
    #[test]
    fn a_run_with_the_outcome_of_the_model_is_the_same_and_another_one_differs() {
        let model = out(RunExit::Returns(Some("5".into())));
        assert_eq!(compare(&model, &Ran { exit: RunExit::Returns(Some("5".into())), muts: Vec::new() }), Check::Same);
        assert_eq!(compare(&model, &Ran { exit: RunExit::Returns(Some("6".into())), muts: Vec::new() }), Check::Differs);
        assert_eq!(compare(&model, &Ran { exit: RunExit::Panics(Some("attempt to add with overflow".into())), muts: Vec::new() }), Check::Differs);
        let panic = out(RunExit::Panics(Some("too big".into())));
        assert_eq!(compare(&panic, &Ran { exit: RunExit::Panics(Some("too big".into())), muts: Vec::new() }), Check::Same);
        assert_eq!(compare(&panic, &Ran { exit: RunExit::Panics(Some("x is too big".into())), muts: Vec::new() }), Check::Differs);
    }

    #[test]
    fn a_part_that_the_model_does_not_give_is_never_a_difference() {
        let model = out(RunExit::Returns(None));
        assert!(matches!(compare(&model, &Ran { exit: RunExit::Returns(Some("X { a: 1 }".into())), muts: Vec::new() }), Check::Unknown(_)));
        let panic = out(RunExit::Panics(None));
        assert_eq!(compare(&panic, &Ran { exit: RunExit::Panics(Some("called `Option::unwrap()` on a `None` value".into())), muts: Vec::new() }), Check::Same);
        let write = RunOut { exit: RunExit::Returns(Some("()".into())), muts: vec![Some("2".into())] };
        assert_eq!(compare(&write, &Ran { exit: RunExit::Returns(Some("()".into())), muts: vec!["3".into()] }), Check::Differs);
    }

    #[test]
    fn two_outcomes_that_differ_only_in_a_part_that_the_probe_does_not_see_are_blind() {
        assert_eq!(blind_cause(&out(RunExit::Returns(Some("1".into()))), &out(RunExit::Returns(Some("2".into())))), None);
        assert_eq!(blind_cause(&out(RunExit::Returns(Some("1".into()))), &out(RunExit::Panics(None))), None);
        assert!(blind_cause(&out(RunExit::Returns(Some("1".into()))), &out(RunExit::Returns(Some("1".into())))).is_some());
        assert!(blind_cause(&out(RunExit::Panics(None)), &out(RunExit::Panics(Some("a".into())))).is_some());
    }

    #[test]
    fn a_debug_text_of_a_string_comes_back() {
        for text in ["plain", "a \"quoted\" word", "two\nlines", "tab\tand \\ and \u{301}"] {
            assert_eq!(unquote(&format!("{text:?}")).as_deref(), Some(text));
        }
        assert_eq!(unquote("no quotes"), None);
    }
}
