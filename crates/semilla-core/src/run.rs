//! [R75.1-1] The suggested test of a sure difference (design 75).
//!
//! The decision of the user (2026-11-26): "I think the probe can suggest a test to write to
//! detect the difference. It should not manually do it." Semilla never builds or runs the code
//! of the user in a step. The model of the equivalence check and of the claims gives an exact
//! input and the outcome of each call of a version (`RunCase`); this module makes the text of a
//! Rust test from it: the test calls the function with the input and asserts the outcome of the
//! old version, so it passes on the old code and fails on the new code.
//!
//! # Soundness
//!
//! - A test asserts only a part of the outcome that the model fixes: a part with no exact value
//!   is `_` in a pattern. A test is never a proof, and Semilla does not run it.
//! - A difference that no assertion can see (an effect, a value that the model does not give,
//!   two panics with no message) gets no test, with the cause.
//! - The text is a function of the data only: the same input gives the same test.

use serde::{Deserialize, Serialize};

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

/// How the test gives an argument to the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgMode {
    /// The value.
    Value,
    /// A shared reference to the value.
    Ref,
    /// A `&mut` reference to the value: the test checks the value after the call.
    Mut,
}

/// One argument of a call: its name, a Rust expression of its value (of the target for a
/// reference), or the cause why the model gives no such expression.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunArg {
    pub name: String,
    pub expr: Result<String, String>,
    pub mode: ArgMode,
    /// [R82.3-1] In a sequence of calls: the variable of the test that the argument is (a result
    /// of an earlier call, or a place that the test made for an earlier call). A new variable
    /// gets the value of `expr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub var: Option<String>,
}

/// [R75.1-1] A Rust pattern of a value of the model: each part that the model fixes, and `_`
/// for each other part. A test needs no `PartialEq` and no `Debug` of a type of the user: it
/// matches the value with the pattern.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pat {
    /// A part that the model does not fix.
    Any,
    /// A literal of a scalar: `5`, `-1`, `true`, `'a'`.
    Lit(String),
    Tuple(Vec<Pat>),
    /// The elements of a sequence (a `Vec`, a slice, an array) at the top of a value.
    Slice(Vec<Pat>),
    /// A struct or a variant with named fields.
    Struct { path: String, fields: Vec<(String, Pat)> },
    /// A tuple struct or a variant with fields by position.
    TupleStruct { path: String, items: Vec<Pat> },
    /// A unit struct or a variant with no field.
    Unit(String),
}

impl Pat {
    /// True if the pattern fixes a part of the value.
    pub fn exact(&self) -> bool {
        match self {
            Pat::Any => false,
            Pat::Lit(_) | Pat::Slice(_) | Pat::Struct { .. } | Pat::TupleStruct { .. } | Pat::Unit(_) => true,
            Pat::Tuple(items) => items.iter().any(Pat::exact),
        }
    }

    /// True if a value that matches `other` never matches `self`: the two patterns fix one part
    /// with two values (a literal, a variant, a length).
    pub fn excludes(&self, other: &Pat) -> bool {
        match (self, other) {
            (Pat::Any, _) | (_, Pat::Any) => false,
            (Pat::Lit(a), Pat::Lit(b)) => a != b,
            (Pat::Tuple(a), Pat::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).any(|(x, y)| x.excludes(y)),
            (Pat::Slice(a), Pat::Slice(b)) => a.len() != b.len() || a.iter().zip(b).any(|(x, y)| x.excludes(y)),
            (Pat::Struct { path: p, fields: a }, Pat::Struct { path: q, fields: b }) => p != q || a.iter().zip(b).any(|((_, x), (_, y))| x.excludes(y)),
            (Pat::TupleStruct { path: p, items: a }, Pat::TupleStruct { path: q, items: b }) => p != q || a.iter().zip(b).any(|(x, y)| x.excludes(y)),
            (Pat::Unit(p), Pat::Unit(q)) => p != q,
            (Pat::Unit(_) | Pat::Struct { .. } | Pat::TupleStruct { .. }, Pat::Unit(_) | Pat::Struct { .. } | Pat::TupleStruct { .. }) => true,
            _ => false,
        }
    }

    /// The Rust text of the pattern.
    pub fn text(&self) -> String {
        match self {
            Pat::Any => "_".into(),
            Pat::Lit(text) => text.clone(),
            Pat::Tuple(items) if items.len() == 1 => format!("({},)", items[0].text()),
            Pat::Tuple(items) => format!("({})", items.iter().map(Pat::text).collect::<Vec<_>>().join(", ")),
            Pat::Slice(items) => format!("[{}]", items.iter().map(Pat::text).collect::<Vec<_>>().join(", ")),
            Pat::Struct { path, fields } => format!("{path} {{ {} }}", fields.iter().map(|(name, pat)| format!("{name}: {}", pat.text())).collect::<Vec<_>>().join(", ")),
            Pat::TupleStruct { path, items } => format!("{path}({})", items.iter().map(Pat::text).collect::<Vec<_>>().join(", ")),
            Pat::Unit(path) => path.clone(),
        }
    }
}

/// The exit of a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunExit {
    /// A return: the pattern of the value.
    Returns(Pat),
    /// A panic: its message, or `None` if the model does not give it.
    Panics(Option<String>),
    Aborts,
}

/// The outcome of a call that a test can see: the exit, and the pattern of the value behind
/// each `&mut` argument after the call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOut {
    pub exit: RunExit,
    pub muts: Vec<Pat>,
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
    /// The source file and the line of the body; empty until the oracle sets them.
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: usize,
    /// True for the body of a constant or a static: the test reads its value.
    #[serde(default)]
    pub value: bool,
    /// [R82.3-1] In a sequence of calls: the variable of the result, when a later call uses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
}

/// The calls of one sure difference: the two versions of a body for an equivalence result,
/// each call of a version on the failed path of a false claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCase {
    pub calls: Vec<RunCall>,
    /// The cause why no test can show the result, when the model knows it (a `&mut`
    /// reference inside an input).
    pub blind: Option<String>,
    /// [R82.3-1] True for the calls of a claim in their order (one test makes them in order,
    /// with the values that each call gives the next calls).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sequence: bool,
}

/// [R75.1-1] A suggested test: the name of its module, the place where it goes, the command that
/// runs it, and its text. Two tests are equal when their names, places, and texts are equal: the
/// results of two configurations with one test are one result in a line (the list of the step
/// keeps the command of each configuration).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuggestedTest {
    pub name: String,
    pub place: String,
    pub command: String,
    pub text: String,
    /// The command of each other configuration whose result has the same test.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
}

impl PartialEq for SuggestedTest {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.place == other.place && self.text == other.text
    }
}

impl Eq for SuggestedTest {}

/// The suggested test of a sure difference.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestNote {
    /// No data of the model (a result of a test with no model).
    #[default]
    None,
    /// The calls of the model; the oracle makes the test.
    Open(Box<RunCase>),
    Test(SuggestedTest),
    /// No test can show the result: the cause.
    NoTest(String),
}

impl TestNote {
    /// The text after the outcomes of a difference: "suggested test `NAME`", or "no suggested
    /// test: CAUSE". Empty for `None`.
    pub fn text(&self) -> String {
        match self {
            TestNote::None => String::new(),
            TestNote::Open(_) => "no suggested test: the step made no test".into(),
            TestNote::Test(test) => format!("suggested test `{}`", test.name),
            TestNote::NoTest(cause) => format!("no suggested test: {cause}"),
        }
    }
}

/// The place of a test: the cargo flags of the configuration of the result (for the command; the
/// text of the test does not name the configuration, so the results of two configurations with
/// one outcome share one test), the package of the body, and the texts of the two outcomes for
/// the comment.
pub struct TestPlace<'a> {
    pub flags: &'a str,
    pub package: &'a str,
    pub before: &'a str,
    pub after: &'a str,
}

/// [R75.1-2] The cause why a test of the old outcome does not fail on the new outcome, or
/// `None`: the exits differ, or a part that the model fixes on the two sides differs.
pub fn blind_cause(before: &RunOut, after: &RunOut) -> Option<String> {
    let fails = match (&before.exit, &after.exit) {
        (RunExit::Returns(a), RunExit::Returns(b)) => a.excludes(b) || before.muts.iter().zip(&after.muts).any(|(a, b)| a.excludes(b)),
        // `#[should_panic(expected = "…")]` takes a part of the message.
        (RunExit::Panics(a), RunExit::Panics(b)) => matches!((a, b), (Some(a), Some(b)) if !b.contains(a.as_str())),
        (RunExit::Aborts, RunExit::Aborts) => false,
        _ => true,
    };
    if fails {
        return None;
    }
    Some(match &before.exit {
        RunExit::Panics(_) => "the two versions panic, and the model does not give two messages that a test can tell apart".into(),
        RunExit::Aborts => "the two versions abort".into(),
        RunExit::Returns(_) => "the difference is in a part of the outcome that a test cannot see (an effect, or a value that the model does not give)".into(),
    })
}

/// [R75.1-1] The suggested test of an equivalence result "different": the call of the old
/// version with the input, and the assertion of its outcome. `Err`: the cause why no test can
/// show the result.
pub fn suggest(case: &RunCase, place: &TestPlace<'_>) -> Result<SuggestedTest, String> {
    if let Some(cause) = &case.blind {
        return Err(format!("{cause}: a test cannot see it"));
    }
    let old = case.calls.iter().find(|call| call.side == RunSide::Before).ok_or("the model gives no call of the old version")?;
    let new = case.calls.iter().find(|call| call.side == RunSide::After).ok_or("the model gives no call of the new version")?;
    if let Some(cause) = blind_cause(&old.expect, &new.expect) {
        return Err(cause);
    }
    let comment = vec![
        "// Semilla (design 75): the two versions differ for this input.".to_string(),
        format!("// The old version {}; the new version {}.", place.before, place.after),
        "// This test passes on the old code and fails on the new code.".to_string(),
    ];
    let body = test_fn("old_outcome", old, &comment)?;
    Ok(finish(old, &[body], place))
}

/// [R75.1-3] The suggested test of a false claim: one test for each call of the old version on
/// the failed path (it asserts the outcome of the model), or, for a claim with no call of the
/// old version, one test for each call of the new version (the counterexample).
pub fn suggest_claim(case: &RunCase, place: &TestPlace<'_>) -> Result<SuggestedTest, String> {
    if case.sequence {
        return suggest_sequence(case, place);
    }
    let old: Vec<&RunCall> = case.calls.iter().filter(|call| call.side == RunSide::Before).collect();
    let (side, calls) = if old.is_empty() { (RunSide::After, case.calls.iter().collect::<Vec<_>>()) } else { (RunSide::Before, old) };
    let first = calls.first().ok_or("the claim is false by its own operations (no call of a version)")?;
    let code = if side == RunSide::Before { "old" } else { "new" };
    let mut bodies = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        let comment = vec![
            format!("// Semilla (design 75): the claim is false for this input: {}.", place.after),
            format!("// This test asserts the outcome of `{}::{}` on the {code} code.", side.word(), call.name),
        ];
        bodies.push(test_fn(&format!("call_{}", index + 1), call, &comment)?);
    }
    Ok(finish(first, &bodies, place))
}

/// [R82.3-1] The suggested test of a false claim from its calls in order (a claim MIR): one
/// `#[test]` that makes the values, makes each call of one version in the order of the claim
/// (a result or a place that a later call takes is a variable of the test), and asserts the
/// outcome of the model after each call: each result and each value behind a `&mut` argument
/// that the model fixes. A call that panics ends the test (`#[should_panic]`).
pub fn suggest_sequence(case: &RunCase, place: &TestPlace<'_>) -> Result<SuggestedTest, String> {
    if let Some(cause) = &case.blind {
        return Err(format!("{cause}: a test cannot see it"));
    }
    let first = case.calls.first().ok_or("the claim is false by its own operations (no call of a version)")?;
    let side = first.side;
    if case.calls.iter().any(|call| call.side != side) {
        return Err("the calls of the test are not of one version".into());
    }
    let code = if side == RunSide::Before { "old" } else { "new" };
    let krate = first.name.trim_start_matches('<').split("::").next().unwrap_or_default().to_string();
    let local = |text: &str| crate_path(text, &krate);
    // The variables that a call takes as `&mut`: `let mut`.
    let mut mutable: Vec<&str> = Vec::new();
    for call in &case.calls {
        for arg in &call.args {
            if let (Some(var), ArgMode::Mut) = (&arg.var, arg.mode) {
                mutable.push(var);
            }
        }
    }
    let binding = |var: &str| if mutable.contains(&var) { format!("let mut {var}") } else { format!("let {var}") };
    let mut lines = vec![
        format!("// Semilla (design 75): the claim is false for this input: {}.", place.after),
        format!("// This test makes the calls of `{}::…` of the claim in their order on the {code} code, and asserts", side.word()),
        "// the outcome of the model after each call.".to_string(),
        "#[test]".to_string(),
    ];
    let mut body: Vec<String> = Vec::new();
    let mut declared: Vec<String> = Vec::new();
    let mut panics = false;
    for (index, call) in case.calls.iter().enumerate() {
        if call.name.contains('{') {
            return Err("the body has no path that a test can call (a closure or a nested body)".into());
        }
        if call.value {
            return Err("a constant or a static in a sequence of calls".into());
        }
        let mut passed = Vec::new();
        let mut muts = Vec::new();
        for (position, arg) in call.args.iter().enumerate() {
            let var = match &arg.var {
                Some(var) if declared.contains(var) => var.clone(),
                Some(var) => {
                    let expr = arg.expr.as_ref().map_err(|cause| format!("the input `{var}` has no Rust form: {cause}"))?;
                    body.push(format!("{} = {};", binding(var), local(expr)));
                    declared.push(var.clone());
                    var.clone()
                }
                None => {
                    let expr = arg.expr.as_ref().map_err(|cause| format!("the input `{}` has no Rust form: {cause}", arg.name))?;
                    let var = format!("arg{}_{}", index + 1, position + 1);
                    let keyword = if arg.mode == ArgMode::Mut { "let mut" } else { "let" };
                    body.push(format!("{keyword} {var} = {};", local(expr)));
                    var
                }
            };
            passed.push(match arg.mode {
                ArgMode::Value => var.clone(),
                ArgMode::Ref => format!("&{var}"),
                ArgMode::Mut => format!("&mut {var}"),
            });
            if arg.mode == ArgMode::Mut {
                muts.push(var);
            }
        }
        let expr = format!("{}({})", local(&call.name), passed.join(", "));
        match &call.expect.exit {
            RunExit::Panics(message) => {
                lines.push(match message {
                    Some(message) => format!("#[should_panic(expected = {message:?})]"),
                    None => "#[should_panic]".to_string(),
                });
                body.push(format!("let _ = {expr};"));
                panics = true;
            }
            RunExit::Aborts => return Err(format!("the {code} version aborts: a test cannot check an abort")),
            RunExit::Returns(pat) => {
                match (&call.bind, pat.exact()) {
                    (Some(var), exact) => {
                        body.push(format!("{} = {expr};", binding(var)));
                        declared.push(var.clone());
                        if exact {
                            body.push(assertion(var, pat, &local));
                        }
                    }
                    (None, true) => {
                        let got = format!("got{}", index + 1);
                        body.push(format!("let {got} = {expr};"));
                        body.push(assertion(&got, pat, &local));
                    }
                    (None, false) => body.push(format!("let _ = {expr};")),
                }
                for (var, pat) in muts.iter().zip(&call.expect.muts) {
                    if pat.exact() {
                        body.push(assertion(var, pat, &local));
                    }
                }
            }
        }
        if panics {
            break;
        }
    }
    lines.push("fn calls_in_order() {".into());
    lines.extend(body.into_iter().map(|line| format!("    {line}")));
    lines.push("}".into());
    Ok(finish(first, &[lines.join("\n")], place))
}

/// The module text of a test, its name, its place, and its command.
fn finish(call: &RunCall, bodies: &[String], place: &TestPlace<'_>) -> SuggestedTest {
    let inner = bodies.join("\n");
    let last: String = call.name.rsplit("::").next().unwrap_or("body").chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    let name = format!("semilla_test_{}_{:06x}", last.trim_matches('_'), fnv(&inner) & 0xff_ffff);
    let mut text = format!("#[cfg(test)]\nmod {name} {{\n");
    for line in inner.lines() {
        text.push_str(&format!("    {line}\n"));
    }
    text.push_str("}\n");
    let flags = if place.flags.is_empty() { String::new() } else { format!(" {}", place.flags) };
    let file = if call.file.is_empty() { "the file of the function".to_string() } else { format!("`{}`", call.file) };
    SuggestedTest { place: format!("{file}, in the module of `{}` (line {})", call.name, call.line), command: format!("cargo test -p {}{flags} {name}", place.package), name, text, also: Vec::new() }
}

/// The text of one `#[test]` function: the arguments, the call, and the assertions of the
/// outcome of the model.
fn test_fn(name: &str, call: &RunCall, comment: &[String]) -> Result<String, String> {
    if call.name.contains('{') {
        return Err("the body has no path that a test can call (a closure or a nested body)".into());
    }
    let krate = call.name.trim_start_matches('<').split("::").next().unwrap_or_default().to_string();
    let local = |text: &str| crate_path(text, &krate);
    let mut lines: Vec<String> = comment.to_vec();
    if call.name.contains("::<") {
        lines.push("// The path of the function has generic arguments: write the types of the instance.".into());
    }
    lines.push("#[test]".into());
    match &call.expect.exit {
        RunExit::Returns(_) => {}
        RunExit::Panics(Some(message)) => lines.push(format!("#[should_panic(expected = {message:?})]")),
        RunExit::Panics(None) => lines.push("#[should_panic]".into()),
        RunExit::Aborts => return Err("the old version aborts: a test cannot check an abort".into()),
    }
    lines.push(format!("fn {name}() {{"));
    let mut passed = Vec::new();
    let mut checks = Vec::new();
    let mut muts = call.expect.muts.iter();
    for (index, arg) in call.args.iter().enumerate() {
        let expr = arg.expr.as_ref().map_err(|cause| format!("the input `{}` has no Rust form: {cause}", arg.name))?;
        let var = variable(&arg.name, index);
        match arg.mode {
            ArgMode::Value => {
                lines.push(format!("    let {var} = {};", local(expr)));
                passed.push(var);
            }
            ArgMode::Ref => {
                lines.push(format!("    let {var} = {};", local(expr)));
                passed.push(format!("&{var}"));
            }
            ArgMode::Mut => {
                lines.push(format!("    let mut {var} = {};", local(expr)));
                passed.push(format!("&mut {var}"));
                if let Some(pat) = muts.next()
                    && pat.exact()
                {
                    checks.push(assertion(&var, pat, &local));
                }
            }
        }
    }
    let callee = local(&call.name);
    // The value of a constant or a static is a place: the test reads it through a reference.
    let (expr, got) = if call.value { (format!("&{callee}"), "*got") } else { (format!("{callee}({})", passed.join(", ")), "got") };
    match &call.expect.exit {
        RunExit::Returns(pat) if pat.exact() => {
            lines.push(format!("    let got = {expr};"));
            lines.push(format!("    {}", assertion(got, pat, &local)));
        }
        _ => lines.push(format!("    let _ = {expr};")),
    }
    if matches!(call.expect.exit, RunExit::Returns(_)) {
        lines.extend(checks.into_iter().map(|check| format!("    {check}")));
    }
    lines.push("}".into());
    Ok(lines.join("\n"))
}

/// The assertion of a value with its pattern: `assert_eq!` for a literal, else `matches!`.
fn assertion(var: &str, pat: &Pat, local: &dyn Fn(&str) -> String) -> String {
    match pat {
        Pat::Lit(text) => format!("assert_eq!({var}, {text});"),
        Pat::Slice(_) => format!("assert!(matches!({}[..], {}));", var.trim_start_matches('*'), local(&pat.text())),
        _ => format!("assert!(matches!({var}, {}));", local(&pat.text())),
    }
}

/// The name of a variable of an argument: the name of the parameter, or `argN`.
fn variable(name: &str, index: usize) -> String {
    let plain = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.starts_with(|c: char| c.is_ascii_digit()) && name.chars().any(|c| c != '_');
    if plain && name != "self" && name != "got" { name.to_string() } else { format!("arg{}", index + 1) }
}

/// A path of the crate of the body from the root of the crate: `depot::Pt` → `crate::Pt`. A
/// test in the crate names the crate as `crate`.
fn crate_path(text: &str, krate: &str) -> String {
    if krate.is_empty() {
        return text.to_string();
    }
    let head = format!("{krate}::");
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(&head) {
        let before = rest[..at].chars().last();
        out.push_str(&rest[..at]);
        if before.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == ':')) {
            out.push_str("crate::");
        } else {
            out.push_str(&head);
        }
        rest = &rest[at + head.len()..];
    }
    out.push_str(rest);
    out
}

/// The FNV-1a hash of a text (a fixed function: the name of a test does not change between
/// versions of Rust).
fn fnv(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(side: RunSide, exit: RunExit, muts: Vec<Pat>) -> RunCall {
        RunCall {
            side,
            name: "depot::shift".into(),
            target: "depot.lib.1".into(),
            args: vec![RunArg { name: "p".into(), expr: Ok("depot::Pt { x: 1, y: 2 }".into()), mode: ArgMode::Value, var: None }, RunArg { name: "v".into(), expr: Ok("vec![1]".into()), mode: ArgMode::Mut, var: None }],
            expect: RunOut { exit, muts },
            file: "src/lib.rs".into(),
            line: 4,
            value: false,
            bind: None,
        }
    }

    fn place() -> TestPlace<'static> {
        TestPlace { flags: "", package: "depot", before: "returns 3", after: "returns 4" }
    }

    // rule+ R75.1-1
    #[test]
    fn a_difference_gives_a_test_that_asserts_the_old_outcome_with_paths_of_the_crate() {
        let case = RunCase { calls: vec![call(RunSide::Before, RunExit::Returns(Pat::Lit("3".into())), vec![Pat::Slice(vec![Pat::Lit("1".into()), Pat::Lit("5".into())])]), call(RunSide::After, RunExit::Returns(Pat::Lit("4".into())), vec![Pat::Slice(vec![Pat::Lit("1".into())])])], blind: None, sequence: false };
        let test = suggest(&case, &place()).unwrap();
        assert!(test.text.contains("let p = crate::Pt { x: 1, y: 2 };\n        let mut v = vec![1];\n        let got = crate::shift(p, &mut v);\n        assert_eq!(got, 3);\n        assert!(matches!(v[..], [1, 5]));"), "{}", test.text);
        assert!(test.text.starts_with("#[cfg(test)]\nmod semilla_test_shift_"), "{}", test.text);
        assert_eq!(test.command, format!("cargo test -p depot {}", test.name));
        // The same data gives the same test.
        assert_eq!(suggest(&case, &place()).unwrap(), test);
    }

    // rule- R75.1-1
    #[test]
    fn an_old_panic_gives_a_test_that_expects_the_panic_and_an_abort_gives_no_test() {
        let case = RunCase { calls: vec![call(RunSide::Before, RunExit::Panics(Some("too big".into())), vec![Pat::Any]), call(RunSide::After, RunExit::Returns(Pat::Lit("4".into())), vec![Pat::Any])], blind: None, sequence: false };
        let text = suggest(&case, &place()).unwrap().text;
        assert!(text.contains("#[should_panic(expected = \"too big\")]\n    fn old_outcome() {") && text.contains("let _ = crate::shift(p, &mut v);\n    }"), "{text}");
        let abort = RunCase { calls: vec![call(RunSide::Before, RunExit::Aborts, vec![Pat::Any]), call(RunSide::After, RunExit::Returns(Pat::Lit("4".into())), vec![Pat::Any])], blind: None, sequence: false };
        assert!(suggest(&abort, &place()).unwrap_err().contains("aborts"));
    }

    // rule+ R75.1-2
    #[test]
    fn a_difference_that_no_assertion_sees_gives_no_test() {
        let same = |a: RunExit, b: RunExit| RunCase { calls: vec![call(RunSide::Before, a, vec![Pat::Any]), call(RunSide::After, b, vec![Pat::Any])], blind: None, sequence: false };
        assert!(suggest(&same(RunExit::Returns(Pat::Any), RunExit::Returns(Pat::Lit("2".into()))), &place()).unwrap_err().contains("a test cannot see"));
        assert!(suggest(&same(RunExit::Panics(Some("too big".into())), RunExit::Panics(Some("x is too big".into()))), &place()).is_err());
    }

    // rule- R75.1-2
    #[test]
    fn a_difference_of_a_variant_or_a_message_gives_a_test() {
        let some = Pat::TupleStruct { path: "core::option::Option::Some".into(), items: vec![Pat::Any] };
        let none = Pat::Unit("core::option::Option::None".into());
        assert!(some.excludes(&none) && !some.excludes(&Pat::TupleStruct { path: "core::option::Option::Some".into(), items: vec![Pat::Lit("2".into())] }));
        let case = RunCase { calls: vec![call(RunSide::Before, RunExit::Returns(some.clone()), vec![Pat::Any]), call(RunSide::After, RunExit::Returns(none), vec![Pat::Any])], blind: None, sequence: false };
        assert!(suggest(&case, &place()).unwrap().text.contains("assert!(matches!(got, core::option::Option::Some(_)));"));
        assert_eq!(blind_cause(&RunOut { exit: RunExit::Panics(Some("a".into())), muts: Vec::new() }, &RunOut { exit: RunExit::Panics(Some("b".into())), muts: Vec::new() }), None);
    }

    fn seq_call(name: &str, args: Vec<RunArg>, exit: RunExit, muts: Vec<Pat>, bind: Option<&str>) -> RunCall {
        RunCall { side: RunSide::Before, name: name.into(), target: "depot.lib.1".into(), args, expect: RunOut { exit, muts }, file: "src/lib.rs".into(), line: 4, value: false, bind: bind.map(str::to_string) }
    }

    fn arg(expr: Result<&str, &str>, mode: ArgMode, var: Option<&str>) -> RunArg {
        RunArg { name: "arg1".into(), expr: expr.map(str::to_string).map_err(str::to_string), mode, var: var.map(str::to_string) }
    }

    // rule+ R82.3-1
    #[test]
    fn a_sequence_of_calls_is_one_test_with_the_values_that_each_call_gives_the_next() {
        let counter = |n: &str| Pat::Struct { path: "depot::Counter".into(), fields: vec![("n".into(), Pat::Lit(n.into()))] };
        let calls = vec![
            seq_call("depot::Counter::new", Vec::new(), RunExit::Returns(counter("0")), Vec::new(), Some("value1")),
            seq_call("depot::Counter::set", vec![arg(Ok(""), ArgMode::Mut, Some("value1")), arg(Ok("101"), ArgMode::Value, None)], RunExit::Returns(Pat::Any), vec![counter("101")], None),
            seq_call("depot::Counter::get", vec![arg(Ok(""), ArgMode::Ref, Some("value1"))], RunExit::Returns(Pat::Lit("101".into())), Vec::new(), None),
        ];
        let case = RunCase { calls, blind: None, sequence: true };
        let test = suggest_claim(&case, &place()).unwrap();
        assert!(test.text.contains("fn calls_in_order() {\n        let mut value1 = crate::Counter::new();\n        assert!(matches!(value1, crate::Counter { n: 0 }));\n        let arg2_2 = 101;\n        let _ = crate::Counter::set(&mut value1, arg2_2);\n        assert!(matches!(value1, crate::Counter { n: 101 }));\n        let got3 = crate::Counter::get(&value1);\n        assert_eq!(got3, 101);\n    }"), "{}", test.text);
        // A place of the test with a value of the model: a variable that the next calls take.
        let calls = vec![
            seq_call("depot::add_to", vec![arg(Ok("5"), ArgMode::Mut, Some("place1_1")), arg(Ok("1"), ArgMode::Value, None)], RunExit::Returns(Pat::Any), vec![Pat::Lit("6".into())], None),
            seq_call("depot::add_to", vec![arg(Ok(""), ArgMode::Mut, Some("place1_1")), arg(Ok("2"), ArgMode::Value, None)], RunExit::Returns(Pat::Any), vec![Pat::Lit("8".into())], None),
        ];
        let text = suggest_claim(&RunCase { calls, blind: None, sequence: true }, &place()).unwrap().text;
        assert!(text.contains("let mut place1_1 = 5;") && text.contains("assert_eq!(place1_1, 6);") && text.contains("let _ = crate::add_to(&mut place1_1, arg2_2);\n        assert_eq!(place1_1, 8);"), "{text}");
    }

    // rule- R82.3-1
    #[test]
    fn a_call_that_panics_ends_the_sequence_and_an_input_with_no_form_gives_no_test() {
        let calls = vec![
            seq_call("depot::Counter::new", Vec::new(), RunExit::Returns(Pat::Any), Vec::new(), Some("value1")),
            seq_call("depot::Counter::check", vec![arg(Ok(""), ArgMode::Ref, Some("value1"))], RunExit::Panics(None), Vec::new(), None),
            seq_call("depot::Counter::get", vec![arg(Ok(""), ArgMode::Ref, Some("value1"))], RunExit::Returns(Pat::Lit("1".into())), Vec::new(), None),
        ];
        let text = suggest_claim(&RunCase { calls, blind: None, sequence: true }, &place()).unwrap().text;
        assert!(text.contains("#[should_panic]") && text.contains("let _ = crate::Counter::check(&value1);") && !text.contains("Counter::get"), "{text}");
        let calls = vec![seq_call("depot::add_to", vec![arg(Err("the model gives no value of a part of the input"), ArgMode::Mut, Some("place1_1"))], RunExit::Returns(Pat::Any), vec![Pat::Any], None)];
        let error = suggest_claim(&RunCase { calls, blind: None, sequence: true }, &place()).unwrap_err();
        assert!(error.contains("has no Rust form"), "{error}");
    }

    #[test]
    fn a_path_of_the_crate_starts_at_crate_and_a_name_of_another_crate_stays() {
        assert_eq!(crate_path("<depot::P as core::ops::Add>::add", "depot"), "<crate::P as core::ops::Add>::add");
        assert_eq!(crate_path("depot::a::B { c: depot::C(1) }", "depot"), "crate::a::B { c: crate::C(1) }");
        assert_eq!(crate_path("my_depot::x", "depot"), "my_depot::x");
        assert_eq!(variable("self", 0), "arg1");
        assert_eq!(variable("argument 1 of `before::depot::half`", 0), "arg1");
    }
}
