//! N4: equality saturation (egglog) over the body terms of the driver.
//!
//! The driver turns a loop-free body into a term (see `crates/semilla-mir/src/term.rs`). This
//! module puts the "before" term and the "after" term into one e-graph, runs a small list of
//! audited rewrite rules, and checks if the two roots are in the same e-class.
//!
//! # The trusted rules
//!
//! Each rule keeps the behavior the same for all terms that it matches. The term builder makes
//! sure of the preconditions that the rules use:
//!
//! - `Not` is only built for a bool value.
//! - `ICmp` is only built for a comparison of two integers (signed or unsigned, never a float,
//!   a char, a pointer, or a bool).
//! - `Ite` takes a bool condition, and its condition is a pure term (no side effect).
//! - `A "checked <op>"` is only built from a MIR checked binary op, which exists only for
//!   integers. Its value is the pair (wrapped result, overflow flag).
//! - `Assert` and `Load` take the state, so effects keep their order. Pure terms have no state
//!   and no order, so this module never moves a pure term across an effect, and never moves an
//!   effect.
//!
//! See `RULES` for the list. A run stops at the node limit, the iteration limit, or the time
//! limit. A stop gives "not proved", never a proof.

use std::time::{Duration, Instant};

use egglog::{CommandOutput, EGraph};

/// One audited rewrite rule: its name, its egglog text, and why it keeps the behavior.
pub struct Rule {
    pub name: &'static str,
    pub egglog: &'static str,
    pub why: &'static str,
}

/// The audited rules. Keep this list small. Each `why` is the soundness argument.
pub const RULES: &[Rule] = &[
    Rule {
        name: "ite-not",
        egglog: "(rewrite (Ite (Not c) a b) (Ite c b a))",
        why: "`not c` is true exactly when `c` is false, so the other branch runs. `c` is a bool.",
    },
    Rule {
        name: "not-not",
        egglog: "(rewrite (Not (Not c)) c)",
        why: "Double negation of a bool gives the same bool.",
    },
    Rule {
        name: "int-not-lt",
        egglog: "(birewrite (Not (ICmp \"lt\" a b)) (ICmp \"ge\" a b))",
        why: "Integers have a total order: `!(a < b)` is `a >= b`. Not true for floats (NaN).",
    },
    Rule {
        name: "int-not-le",
        egglog: "(birewrite (Not (ICmp \"le\" a b)) (ICmp \"gt\" a b))",
        why: "Integers have a total order: `!(a <= b)` is `a > b`.",
    },
    Rule {
        name: "int-not-gt",
        egglog: "(birewrite (Not (ICmp \"gt\" a b)) (ICmp \"le\" a b))",
        why: "Integers have a total order: `!(a > b)` is `a <= b`.",
    },
    Rule {
        name: "int-not-ge",
        egglog: "(birewrite (Not (ICmp \"ge\" a b)) (ICmp \"lt\" a b))",
        why: "Integers have a total order: `!(a >= b)` is `a < b`.",
    },
    Rule {
        name: "int-not-eq",
        egglog: "(birewrite (Not (ICmp \"eq\" a b)) (ICmp \"ne\" a b))",
        why: "For integers, `!(a == b)` is `a != b`.",
    },
    Rule {
        name: "int-not-ne",
        egglog: "(birewrite (Not (ICmp \"ne\" a b)) (ICmp \"eq\" a b))",
        why: "For integers, `!(a != b)` is `a == b`.",
    },
    Rule {
        name: "int-flip-gt",
        egglog: "(birewrite (ICmp \"gt\" a b) (ICmp \"lt\" b a))",
        why: "`a > b` is `b < a` by definition. Both operands are pure, so their order is free.",
    },
    Rule {
        name: "int-flip-ge",
        egglog: "(birewrite (ICmp \"ge\" a b) (ICmp \"le\" b a))",
        why: "`a >= b` is `b <= a` by definition. Both operands are pure.",
    },
    Rule {
        name: "int-eq-symmetric",
        egglog: "(rewrite (ICmp \"eq\" a b) (ICmp \"eq\" b a))",
        why: "Integer equality is symmetric. Both operands are pure.",
    },
    Rule {
        name: "int-ne-symmetric",
        egglog: "(rewrite (ICmp \"ne\" a b) (ICmp \"ne\" b a))",
        why: "Integer inequality is symmetric. Both operands are pure.",
    },
    Rule {
        name: "checked-add-commute",
        egglog: "(rewrite (A \"checked Add\" (Cons a (Cons b (Nil)))) (A \"checked Add\" (Cons b (Cons a (Nil)))))",
        why: "A checked op exists only for integers. Integer addition is commutative, and the \
              overflow flag is true exactly when the true sum does not fit, which is symmetric.",
    },
    Rule {
        name: "checked-mul-commute",
        egglog: "(rewrite (A \"checked Mul\" (Cons a (Cons b (Nil)))) (A \"checked Mul\" (Cons b (Cons a (Nil)))))",
        why: "A checked op exists only for integers. Integer multiplication is commutative, and \
              the overflow flag is true exactly when the true product does not fit.",
    },
    Rule {
        name: "ite-same",
        egglog: "(rewrite (Ite c a a) a)",
        why: "Both branches give the same result, and the condition is pure (no effect to keep).",
    },
    Rule {
        name: "assert-commute",
        egglog: "(rewrite (Assert c1 m (L \"uw-continue\") (Assert c2 m (L \"uw-continue\") s)) \
                 (Assert c2 m (L \"uw-continue\") (Assert c1 m (L \"uw-continue\") s)))",
        why: "Two consecutive asserts with the same message key and no cleanup: if both pass, \
              nothing happens in either order. If one fails, the body panics with the same \
              message from the same state `s`, in either order. Only the panic location can \
              change, and a panic location is not behavior (design 8.2). The message key holds \
              the operands of the messages that print operands, so the messages are the same.",
    },
    Rule {
        name: "load-past-assert",
        egglog: "(rewrite (Load t a (Assert c m u s)) (Load t a s))",
        why: "An assert does not write memory. If it fails, the read never runs, so the read \
              gives the value of the state before the assert.",
    },
];

/// The limits of one run.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_iterations: usize,
    pub max_nodes: usize,
    pub max_time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_iterations: 30, max_nodes: 100_000, max_time: Duration::from_secs(3) }
    }
}

/// The result of a proof attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The terms are equal. The list holds the rule names of the proof, in proof order.
    Proved(Vec<String>),
    /// The rules could not connect the terms, or a limit stopped the run.
    NotProved,
}

const DATATYPE: &str = "(datatype T
  (L String)
  (A String T)
  (Nil)
  (Cons T T)
  (Ite T T T)
  (Not T)
  (ICmp String T T)
  (Assert T T T T)
  (Load T T T))
(ruleset audited)
";

/// Tries to prove that two terms are equal. A term text is the output of the driver (one node
/// for each line, see `term.rs`). Lines that start with `;` are comments.
pub fn prove(before: &str, after: &str, limits: &Limits) -> Outcome {
    let (Some((before_lets, before_root)), Some((after_lets, after_root))) =
        (to_egglog(before, "b"), to_egglog(after, "a"))
    else {
        return Outcome::NotProved;
    };
    let mut program = String::from(DATATYPE);
    for rule in RULES {
        // Put the options before the last closing parenthesis of the rule.
        let text = rule.egglog.strip_suffix(')').expect("a rule ends with a parenthesis");
        program.push_str(&format!("{text} :ruleset audited :name \"{}\")\n", rule.name));
    }
    program.push_str(&before_lets);
    program.push_str(&after_lets);
    let goal = format!("(= {before_root} {after_root})");

    // First try with proofs, to get the rule chain. If the proof mode fails, use the plain
    // mode and report the rules that fired.
    match run(&program, &goal, limits, true) {
        Some(outcome) => outcome,
        None => run(&program, &goal, limits, false).unwrap_or(Outcome::NotProved),
    }
}

/// Runs the rules one iteration at a time. Returns `None` if egglog fails.
fn run(program: &str, goal: &str, limits: &Limits, proofs: bool) -> Option<Outcome> {
    let start = Instant::now();
    let mut egraph = if proofs { EGraph::new_with_proofs() } else { EGraph::default() };
    egraph.parse_and_run_program(None, program).ok()?;
    let check = format!("(check {goal})");
    let mut fired: Vec<String> = Vec::new();
    let mut iteration = 0;
    loop {
        if egraph.parse_and_run_program(None, &check).is_ok() {
            break;
        }
        if iteration >= limits.max_iterations
            || egraph.num_tuples() > limits.max_nodes
            || start.elapsed() > limits.max_time
        {
            return Some(Outcome::NotProved);
        }
        iteration += 1;
        let outputs = egraph.parse_and_run_program(None, "(run audited 1)").ok()?;
        let mut updated = false;
        for output in outputs {
            if let CommandOutput::RunSchedule(report) = output {
                updated |= report.updated;
                for (rule, count) in &report.num_matches_per_rule {
                    let name = base_rule_name(rule);
                    if *count > 0 && !fired.contains(&name) {
                        fired.push(name);
                    }
                }
            }
        }
        if !updated {
            // Saturation: no rule adds anything, so the terms stay apart.
            return Some(Outcome::NotProved);
        }
    }
    if !proofs {
        return Some(Outcome::Proved(fired));
    }
    let outputs = egraph.parse_and_run_program(None, &format!("(prove {goal})")).ok()?;
    for output in outputs {
        if let CommandOutput::ProveExists { proof_store, proof_id } = output {
            let mut names = Vec::new();
            collect_rule_names(&proof_store, proof_id, &mut names);
            return Some(Outcome::Proved(names));
        }
    }
    None
}

/// Removes the direction mark of a `birewrite` rule name (`name=>` or `name<=`).
fn base_rule_name(name: &str) -> String {
    name.trim_end_matches("=>").trim_end_matches("<=").to_string()
}

/// Collects the rule names of a proof, in proof order, each name once.
fn collect_rule_names(
    store: &egglog::proof::ProofStore,
    id: egglog::proof::ProofId,
    names: &mut Vec<String>,
) {
    use egglog::proof::Justification;
    let mut stack = vec![id];
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(format!("{id:?}")) {
            continue;
        }
        match store.get(id).justification() {
            Justification::Rule { name, premise_proofs, .. } => {
                order.push(base_rule_name(name));
                stack.extend(premise_proofs.iter().rev().copied());
            }
            Justification::Trans(first, second) => {
                stack.push(*second);
                stack.push(*first);
            }
            Justification::Sym(inner) | Justification::ContainerNormalize { proof: inner } => {
                stack.push(*inner)
            }
            Justification::Congr { proof, child_proof, .. } => {
                stack.push(*child_proof);
                stack.push(*proof);
            }
            Justification::MergeFn { old_proof, new_proof, .. } => {
                stack.push(*new_proof);
                stack.push(*old_proof);
            }
            Justification::Fiat | Justification::Eval => {}
        }
    }
    for name in order {
        // Only the audited rules count. egglog can add internal rules.
        if RULES.iter().any(|rule| rule.name == name) && !names.contains(&name) {
            names.push(name);
        }
    }
}

/// Converts a driver term into egglog `let` commands. Node `#n` becomes the global
/// `$<prefix>_n`. Returns the commands and the name of the root.
fn to_egglog(term: &str, prefix: &str) -> Option<(String, String)> {
    let mut out = String::new();
    let mut root = None;
    for line in term.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let (id, body) = line.split_once(' ')?;
        let id = id.strip_prefix('#')?;
        let name = format!("${prefix}_{id}");
        out.push_str(&format!("(let {name} {})\n", rename_references(body, prefix)?));
        root = Some(name);
    }
    Some((out, root?))
}

/// Replaces the references `#n` outside of string literals with `$<prefix>_n`.
fn rename_references(text: &str, prefix: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                loop {
                    let c = chars.next()?;
                    out.push(c);
                    if c == '\\' {
                        out.push(chars.next()?);
                    } else if c == '"' {
                        break;
                    }
                }
            }
            '#' => {
                out.push_str(&format!("${prefix}_"));
                while let Some(&digit) = chars.peek() {
                    if !digit.is_ascii_digit() {
                        break;
                    }
                    out.push(digit);
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(lines: &[&str]) -> String {
        lines.join("\n")
    }

    #[test]
    fn proves_guard_clause_shape() {
        // ite(lt(i, n), x, y) against ite(ge(i, n), y, x).
        let before = term(&[
            "#0 (L \"arg 1\")",
            "#1 (L \"arg 2\")",
            "#2 (ICmp \"lt\" #0 #1)",
            "#3 (L \"x\")",
            "#4 (L \"y\")",
            "#5 (Ite #2 #3 #4)",
        ]);
        let after = term(&[
            "#0 (L \"arg 1\")",
            "#1 (L \"arg 2\")",
            "#2 (ICmp \"ge\" #0 #1)",
            "#3 (L \"y\")",
            "#4 (L \"x\")",
            "#5 (Ite #2 #3 #4)",
        ]);
        match prove(&before, &after, &Limits::default()) {
            Outcome::Proved(rules) => {
                assert!(rules.contains(&"ite-not".to_string()), "{rules:?}");
                assert!(
                    rules.contains(&"int-not-lt".to_string())
                        || rules.contains(&"int-not-ge".to_string()),
                    "{rules:?}"
                );
            }
            Outcome::NotProved => panic!("not proved"),
        }
    }

    #[test]
    fn does_not_swap_branches_without_negation() {
        let before = term(&["#0 (L \"c\")", "#1 (L \"x\")", "#2 (L \"y\")", "#3 (Ite #0 #1 #2)"]);
        let after = term(&["#0 (L \"c\")", "#1 (L \"y\")", "#2 (L \"x\")", "#3 (Ite #0 #1 #2)"]);
        assert_eq!(prove(&before, &after, &Limits::default()), Outcome::NotProved);
    }

    #[test]
    fn asserts_commute_only_with_the_same_message() {
        let asserts = |first: &str, second: &str, m1: &str, m2: &str| {
            term(&[
                "#0 (L \"state start\")",
                "#1 (L \"uw-continue\")",
                &format!("#2 (L \"{first}\")"),
                &format!("#3 (L \"{second}\")"),
                &format!("#4 (L \"{m1}\")"),
                &format!("#5 (L \"{m2}\")"),
                "#6 (Assert #2 #4 #1 #0)",
                "#7 (Assert #3 #5 #1 #6)",
            ])
        };
        let same = prove(
            &asserts("c1", "c2", "panic overflow Add", "panic overflow Add"),
            &asserts("c2", "c1", "panic overflow Add", "panic overflow Add"),
            &Limits::default(),
        );
        assert_eq!(same, Outcome::Proved(vec!["assert-commute".to_string()]));
        let different = prove(
            &asserts("c1", "c2", "panic overflow Add", "panic overflow Mul"),
            &asserts("c2", "c1", "panic overflow Mul", "panic overflow Add"),
            &Limits::default(),
        );
        assert_eq!(different, Outcome::NotProved);
    }

    #[test]
    fn float_style_comparison_is_not_negated() {
        // A generic comparison (not `ICmp`) gets no negation rule.
        let before = term(&[
            "#0 (L \"a\")",
            "#1 (L \"b\")",
            "#2 (A \"binary Lt\" (Cons #0 (Cons #1 (Nil))))",
            "#3 (Not #2)",
            "#4 (L \"x\")",
            "#5 (L \"y\")",
            "#6 (Ite #3 #4 #5)",
        ]);
        let after = term(&[
            "#0 (L \"a\")",
            "#1 (L \"b\")",
            "#2 (A \"binary Ge\" (Cons #0 (Cons #1 (Nil))))",
            "#3 (L \"x\")",
            "#4 (L \"y\")",
            "#5 (Ite #2 #3 #4)",
        ]);
        assert_eq!(prove(&before, &after, &Limits::default()), Outcome::NotProved);
    }

    #[test]
    fn references_inside_strings_stay() {
        assert_eq!(
            rename_references("(A \"variant#1 {closure#0}\" (Cons #12 (Nil)))", "b").unwrap(),
            "(A \"variant#1 {closure#0}\" (Cons $b_12 (Nil)))"
        );
    }

    #[test]
    fn limits_give_not_proved() {
        let before = term(&["#0 (L \"a\")", "#1 (L \"b\")", "#2 (ICmp \"lt\" #0 #1)"]);
        let after = term(&["#0 (L \"a\")", "#1 (L \"b\")", "#2 (ICmp \"gt\" #1 #0)"]);
        let none = Limits { max_iterations: 0, ..Limits::default() };
        assert_eq!(prove(&before, &after, &none), Outcome::NotProved);
        assert!(matches!(prove(&before, &after, &Limits::default()), Outcome::Proved(_)));
    }
}
