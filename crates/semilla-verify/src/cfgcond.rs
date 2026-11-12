//! The conditions of the build configuration that the checked configurations cover (batch 5s,
//! parts B2 and C; batch 5t, parts A and B).
//!
//! Semilla checks a list of configurations. The compiler gives one record for each compile
//! unit of each checked build: the true `cfg` names and pairs, and the source files that the
//! unit reads. A condition of a `cfg` attribute, a `cfg_attr` attribute, or a `cfg!` call at a
//! place in a file is **covered** if these two rules hold:
//!
//! - each atom is a name (`test`) or a pair (`feature = "F"`);
//! - one checked unit that reads the file makes the condition true, and one makes it false.
//!
//! No atom has a special rule. A file that no unit reads has no covered condition. A tree with
//! a record file that has no unit record has no covered condition. A text that Semilla cannot
//! evaluate (`version(…)`, `accessible(…)`) is not covered.
//!
//! The rule of the build configuration (design 8.2) stays for a condition that is not
//! covered. For a covered condition, the proof in each checked configuration is the check,
//! if the checked units have each valuation of the atoms of the proof (`crate::joint`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use semilla_syntax::Workspace;

use crate::srcconfig::compact;
use crate::{ConfigTruth, Unit};

/// A condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pred {
    /// A name or a pair, in the form of the unit record: `unix`, `feature="std"`.
    Atom(String),
    All(Vec<Pred>),
    Any(Vec<Pred>),
    Not(Box<Pred>),
    /// A text that Semilla cannot evaluate.
    Other,
}

fn is_punct(tree: Option<&TokenTree>, c: char) -> bool {
    matches!(tree, Some(TokenTree::Punct(p)) if p.as_char() == c)
}

/// The parts of a token list between the commas of the top level.
fn parts(stream: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut out: Vec<Vec<TokenTree>> = vec![Vec::new()];
    for tree in stream {
        if is_punct(Some(&tree), ',') {
            out.push(Vec::new());
        } else if let Some(part) = out.last_mut() {
            part.push(tree);
        }
    }
    out.retain(|part| !part.is_empty());
    out
}

pub(crate) fn parse(trees: &[TokenTree]) -> Pred {
    let Some(TokenTree::Ident(name)) = trees.first() else { return Pred::Other };
    let name = name.to_string();
    match (name.as_str(), trees.len()) {
        ("all" | "any" | "not", 2) => {
            let Some(TokenTree::Group(group)) = trees.get(1).filter(|tree| matches!(tree, TokenTree::Group(g) if g.delimiter() == Delimiter::Parenthesis)) else { return Pred::Other };
            let mut inner: Vec<Pred> = parts(group.stream()).iter().map(|part| parse(part)).collect();
            match name.as_str() {
                "all" => Pred::All(inner),
                "any" => Pred::Any(inner),
                _ if inner.len() == 1 => Pred::Not(Box::new(inner.remove(0))),
                _ => Pred::Other,
            }
        }
        (_, 1) => Pred::Atom(name.strip_prefix("r#").unwrap_or(&name).to_string()),
        (_, 3) if is_punct(trees.get(1), '=') => match &trees[2] {
            // The value of the string, not its token text: `"a\u{62}"` is the value `ab`.
            TokenTree::Literal(literal) => match syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                Ok(value) => Pred::Atom(format!("{name}=\"{}\"", value.value())),
                Err(_) => Pred::Other,
            },
            _ => Pred::Other,
        },
        _ => Pred::Other,
    }
}

/// The value of a condition in a unit with the true set `cfg`. `None`: Semilla cannot
/// evaluate the condition.
pub(crate) fn eval(pred: &Pred, cfg: &BTreeSet<String>) -> Option<bool> {
    Some(match pred {
        Pred::Atom(atom) => cfg.contains(atom),
        Pred::All(inner) => {
            let mut all = true;
            for pred in inner {
                all &= eval(pred, cfg)?;
            }
            all
        }
        Pred::Any(inner) => {
            let mut any = false;
            for pred in inner {
                any |= eval(pred, cfg)?;
            }
            any
        }
        Pred::Not(inner) => !eval(inner, cfg)?,
        Pred::Other => return None,
    })
}

/// Each atom of a condition.
pub(crate) fn atoms(pred: &Pred, out: &mut BTreeSet<String>) {
    match pred {
        Pred::Atom(atom) => {
            out.insert(atom.clone());
        }
        Pred::All(inner) | Pred::Any(inner) => inner.iter().for_each(|pred| atoms(pred, out)),
        Pred::Not(inner) => atoms(inner, out),
        Pred::Other => {}
    }
}

/// True if the units that read `file` make the condition true and false.
fn covered_at(pred: &Pred, file: &str, side: &Side) -> bool {
    let (mut yes, mut no) = (false, false);
    if !side.known {
        return false;
    }
    for unit in side.units.iter().filter(|unit| unit.files.contains(file)) {
        match eval(pred, &unit.cfg) {
            Some(true) => yes = true,
            Some(false) => no = true,
            None => return false,
        }
    }
    yes && no
}

/// The key of a condition: its token text.
fn key(stream: TokenStream) -> String {
    compact(&stream.to_string())
}

/// The text of a condition for a message: `feature = "x"`, `all(unix, test)`.
pub fn show(key: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    for c in key.chars() {
        match c {
            '"' => quoted = !quoted,
            '=' if !quoted => {
                out.push_str(" = ");
                continue;
            }
            ',' if !quoted => {
                out.push_str(", ");
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    // A comma before a closing bracket has no meaning.
    out.replace(", )", ")")
}

/// The conditions of an attribute, from the tokens between its brackets.
fn attribute(content: Vec<TokenTree>, out: &mut Vec<TokenStream>) {
    let Some(TokenTree::Ident(name)) = content.first() else { return };
    let Some(TokenTree::Group(group)) = content.get(1).filter(|_| content.len() == 2) else { return };
    if group.delimiter() != Delimiter::Parenthesis {
        return;
    }
    match name.to_string().as_str() {
        "cfg" => out.push(group.stream()),
        "cfg_attr" => {
            let mut all = parts(group.stream()).into_iter();
            if let Some(first) = all.next() {
                out.push(first.into_iter().collect());
            }
            // The content can be a `cfg` or a `cfg_attr`.
            for part in all {
                attribute(part, out);
            }
        }
        _ => {}
    }
}

/// Each condition in a token text: `#[cfg(P)]`, `#[cfg_attr(P, …)]`, and `cfg!(P)`.
fn scan(stream: TokenStream, out: &mut Vec<TokenStream>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        let before = |back: usize| index.checked_sub(back).and_then(|at| trees.get(at));
        match tree {
            TokenTree::Group(group) => {
                let attr = group.delimiter() == Delimiter::Bracket && (is_punct(before(1), '#') || (is_punct(before(1), '!') && is_punct(before(2), '#')));
                if attr {
                    attribute(group.stream().into_iter().collect(), out);
                }
                scan(group.stream(), out);
            }
            TokenTree::Ident(ident) if ident == "cfg" && is_punct(trees.get(index + 1), '!') => {
                if let Some(TokenTree::Group(group)) = trees.get(index + 2) {
                    out.push(group.stream());
                }
            }
            _ => {}
        }
    }
}

/// The units of one tree.
#[derive(Debug, Default, Clone)]
struct Side {
    /// Each unit of each checked configuration.
    units: Vec<Unit>,
    /// False if a record file of a checked configuration has no unit record.
    known: bool,
    /// The Rust files of the tree.
    files: BTreeSet<String>,
}

/// One part of the rule that a caller can switch on again (batch 5t, part B).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mode {
    /// Each covered condition is covered.
    Plain,
    /// A condition with this atom is not covered.
    Without(String),
    /// A condition stays covered only if each atom is in the first set, and the file of the
    /// place is in the second set.
    Only(BTreeSet<String>, BTreeSet<String>),
    /// No condition is covered: the rule before batch 5s.
    Strict,
}

/// The conditions of the source text of the trees, by their state.
#[derive(Debug, Default, Clone)]
pub struct Cover {
    sides: Vec<Side>,
    /// The state of each place: a file and the key of a condition.
    places: BTreeMap<(String, String), bool>,
    /// The parsed condition of each key.
    preds: BTreeMap<String, Pred>,
    /// The state of each key: covered at each place.
    covered: BTreeSet<String>,
    /// The conditions that the checked configurations do not cover at one place or more.
    pub not_covered: BTreeSet<String>,
}

impl Cover {
    /// Reads each Rust file of each tree. `sides`: a tree and the checked configurations of
    /// this tree. A condition at a place is covered only if each tree with the place covers it.
    pub fn new(sides: &[(&Workspace, &[ConfigTruth])]) -> Cover {
        let mut cover = Cover::default();
        for (workspace, truth) in sides {
            let side = Side {
                units: truth.iter().flat_map(|config| config.units.iter().cloned()).collect(),
                known: !truth.is_empty() && truth.iter().all(|config| config.known),
                files: workspace.rust_files.keys().cloned().collect(),
            };
            for (file, text) in &workspace.rust_files {
                // The fast path: most files have no condition.
                if !text.contains("cfg") {
                    continue;
                }
                let Ok(stream) = text.parse::<TokenStream>() else { continue };
                let mut found = Vec::new();
                scan(stream, &mut found);
                for condition in found {
                    let pred = parse(&condition.clone().into_iter().collect::<Vec<_>>());
                    let covered = covered_at(&pred, file, &side);
                    let key = key(condition);
                    *cover.places.entry((file.clone(), key.clone())).or_insert(true) &= covered;
                    cover.preds.insert(key, pred);
                }
            }
            cover.sides.push(side);
        }
        let mut states: BTreeMap<&str, bool> = BTreeMap::new();
        for ((_, key), covered) in &cover.places {
            *states.entry(key).or_insert(true) &= covered;
        }
        for (text, covered) in states {
            if covered {
                cover.covered.insert(text.to_string());
            } else {
                cover.not_covered.insert(text.to_string());
            }
        }
        cover
    }

    /// The state of a condition at a place. `file`: the file of the place, if the caller
    /// knows it. With no file, the condition must be covered at each place of the trees.
    fn covers(&self, condition: TokenStream, file: Option<&str>, mode: &Mode) -> bool {
        let trees: Vec<TokenTree> = condition.clone().into_iter().collect();
        let key = key(condition);
        let covered = match file {
            Some(file) => match self.places.get(&(file.to_string(), key.clone())) {
                Some(covered) => *covered,
                // A place that the text scan did not find: each tree with the file must cover it.
                None => {
                    let pred = parse(&trees);
                    let with: Vec<&Side> = self.sides.iter().filter(|side| side.files.contains(file)).collect();
                    !with.is_empty() && with.iter().all(|side| covered_at(&pred, file, side))
                }
            },
            None => self.covered.contains(&key),
        };
        if !covered || *mode == Mode::Plain {
            return covered;
        }
        let mut found = BTreeSet::new();
        atoms(&parse(&trees), &mut found);
        match mode {
            Mode::Plain => true,
            Mode::Strict => false,
            Mode::Without(atom) => !found.contains(atom),
            Mode::Only(names, files) => file.is_some_and(|file| files.contains(file)) && found.iter().all(|atom| names.contains(atom)),
        }
    }

    /// Each atom of each covered place.
    pub(crate) fn atoms(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for ((_, key), covered) in &self.places {
            if let Some(pred) = self.preds.get(key).filter(|_| *covered) {
                atoms(pred, &mut out);
            }
        }
        out
    }
}

/// Each condition `P` of an attribute text `#[cfg(P)]`. The item of the attribute exists only
/// if `P` is true.
pub(crate) fn own_conditions(attr: &str) -> Vec<Pred> {
    let Ok(stream) = attr.parse::<TokenStream>() else { return Vec::new() };
    let mut out = Vec::new();
    for tree in stream {
        let TokenTree::Group(group) = tree else { continue };
        if group.delimiter() != Delimiter::Bracket {
            continue;
        }
        let content: Vec<TokenTree> = group.stream().into_iter().collect();
        if let (Some(TokenTree::Ident(name)), Some(TokenTree::Group(inner)), 2) = (content.first(), content.get(1), content.len())
            && name == "cfg"
            && inner.delimiter() == Delimiter::Parenthesis
        {
            out.push(parse(&inner.stream().into_iter().collect::<Vec<_>>()));
        }
    }
    out
}

thread_local! {
    /// The conditions of the step that the verifier checks now.
    static COVER: RefCell<Option<Rc<Cover>>> = const { RefCell::new(None) };
    /// The file of the attribute that the verifier reads now.
    static FILE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The part of the rule that is on again.
    static MODE: RefCell<Mode> = const { RefCell::new(Mode::Plain) };
}

/// The time in which `covered` uses a `Cover`. The end of the scope removes it.
pub(crate) struct Scope;

impl Scope {
    /// Makes a different `Cover` the answer of `covered` for the rest of the scope.
    pub(crate) fn set(&self, cover: Cover) {
        COVER.with(|slot| *slot.borrow_mut() = Some(Rc::new(cover)));
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        COVER.with(|cover| *cover.borrow_mut() = None);
    }
}

/// Makes `cover` the answer of `covered` on this thread.
pub(crate) fn enter(cover: Cover) -> Scope {
    COVER.with(|slot| *slot.borrow_mut() = Some(Rc::new(cover)));
    Scope
}

/// The `Cover` of this thread.
pub(crate) fn current() -> Option<Rc<Cover>> {
    COVER.with(|cover| cover.borrow().clone())
}

/// The time in which `covered` knows the file of its condition. The end gives the file back
/// that was there before.
pub(crate) struct At(Option<String>);

impl Drop for At {
    fn drop(&mut self) {
        FILE.with(|file| *file.borrow_mut() = self.0.take());
    }
}

/// Makes `file` the place of each condition that `covered` gets on this thread.
pub(crate) fn at(file: &str) -> At {
    At(FILE.with(|slot| slot.borrow_mut().replace(file.to_string())))
}

/// Runs `run` with a part of the rule on again.
pub(crate) fn with_mode<R>(mode: &Mode, run: impl FnOnce() -> R) -> R {
    let old = MODE.with(|slot| slot.replace(mode.clone()));
    let result = run();
    MODE.with(|slot| *slot.borrow_mut() = old);
    result
}

/// True if the checked configurations cover the condition (the tokens between the
/// parentheses of `cfg(…)`). With no `Scope`, no condition is covered.
pub(crate) fn covered(condition: TokenStream) -> bool {
    let Some(cover) = current() else { return false };
    let file = FILE.with(|file| file.borrow().clone());
    MODE.with(|mode| cover.covers(condition, file.as_deref(), &mode.borrow()))
}

/// True for the text of a call `cfg!(P)` with a covered condition.
pub(crate) fn covered_call(text: &str) -> bool {
    let Some(inner) = text.trim().strip_prefix("cfg!").map(str::trim).and_then(|rest| rest.strip_prefix('(')).and_then(|rest| rest.strip_suffix(')')) else { return false };
    inner.parse::<TokenStream>().is_ok_and(covered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = "crates/a/src/lib.rs";

    fn unit(target: &str, cfg: &[&str], files: &[&str]) -> Unit {
        Unit { target: target.to_string(), package: "crates/a".to_string(), cfg: cfg.iter().map(|f| f.to_string()).collect(), files: files.iter().map(|f| f.to_string()).collect(), root: files.first().map(|f| f.to_string()).unwrap_or_default() }
    }

    /// A build with a library unit and a test unit of the library.
    fn truth(name: &str, cfg: &[&str]) -> ConfigTruth {
        let with_test: Vec<&str> = cfg.iter().copied().chain(["test"]).collect();
        ConfigTruth { name: name.to_string(), units: vec![unit("a.lib", cfg, &[LIB]), unit("a.test", &with_test, &[LIB])], known: true }
    }

    fn workspace(files: &[(&str, &str)]) -> Workspace {
        let mut workspace = Workspace::default();
        for (file, text) in files {
            workspace.rust_files.insert(file.to_string(), text.to_string());
        }
        workspace
    }

    #[test]
    fn a_condition_is_covered_only_with_a_true_unit_and_a_false_unit() {
        let text = r#"
            #[cfg(debug_assertions)] fn a() {}
            #[cfg(not(feature = "x"))] fn b() {}
            #[cfg(all(feature = "x", feature = "y"))] fn c() {}
            #[cfg(feature = "none")] fn d() {}
            #[cfg(target_os = "windows")] fn e() {}
            #[cfg(any(test, unix))] fn f() {}
            #[cfg_attr(feature = "y", cfg(miri))] fn g() {}
            fn h() -> bool { cfg!(test) && cfg!(panic = "abort") }
            #[cfg(any(debug_assertions, version("1.80")))] fn i() {}
            #[cfg(secret)] fn j() {}
        "#;
        let ws = workspace(&[(LIB, text)]);
        let both = [truth("dev", &["unix", "debug_assertions"]), truth("release", &["unix"]), truth("dev, all features", &["unix", "debug_assertions", "feature=\"x\"", "feature=\"y\""])];
        let cover = Cover::new(&[(&ws, &both)]);
        let closed: Vec<&str> = cover.covered.iter().map(String::as_str).collect();
        assert_eq!(closed, ["all(feature=\"x\",feature=\"y\")", "debug_assertions", "feature=\"y\"", "not(feature=\"x\")", "test"]);
        let open: Vec<&str> = cover.not_covered.iter().map(String::as_str).collect();
        assert_eq!(open, ["any(debug_assertions,version(\"1.80\"))", "any(test,unix)", "feature=\"none\"", "miri", "panic=\"abort\"", "secret", "target_os=\"windows\""]);
        assert_eq!(show("all(feature=\"a,b\",not(unix))"), "all(feature = \"a,b\", not(unix))");
        assert_eq!(show("all(unix,not(test),)"), "all(unix, not(test))");
        // One profile: `debug_assertions` has one value.
        let cover = Cover::new(&[(&ws, &both[..1])]);
        assert!(cover.not_covered.contains("debug_assertions") && cover.covered.contains("test"), "{cover:?}");
        // The two trees must cover a condition.
        let cover = Cover::new(&[(&ws, &both), (&ws, &both[..2])]);
        assert!(cover.not_covered.contains("not(feature=\"x\")") && cover.covered.contains("debug_assertions"), "{cover:?}");
        // No scope: no condition is covered.
        let condition = || "debug_assertions".parse::<TokenStream>().unwrap();
        assert!(!covered(condition()));
        {
            let _scope = enter(Cover::new(&[(&ws, &both)]));
            assert!(covered(condition()) && covered_call("cfg!(debug_assertions)") && !covered_call("cfg!(unix)") && !covered_call("env!(\"A\")"));
            // A part of the rule is on again: for one atom, or for each atom and file out of a set.
            let without = Mode::Without("debug_assertions".to_string());
            assert!(!with_mode(&without, || covered(condition())) && with_mode(&without, || covered_call("cfg!(test)")));
            assert!(!with_mode(&without, || covered("all(test, debug_assertions)".parse().unwrap())));
            let set = |names: &[&str]| names.iter().map(|name| name.to_string()).collect::<BTreeSet<String>>();
            let at_lib = |mode: &Mode, text: &str| {
                with_mode(mode, || {
                    let _at = at(LIB);
                    covered(text.parse().unwrap())
                })
            };
            let only = Mode::Only(set(&["debug_assertions"]), set(&[LIB]));
            assert!(at_lib(&only, "debug_assertions") && !at_lib(&only, "test") && !at_lib(&only, "not(feature = \"x\")"));
            // A place with no file, and a place in a file out of the set, are not covered.
            assert!(!with_mode(&only, || covered(condition())));
            assert!(!at_lib(&Mode::Only(set(&["debug_assertions"]), set(&["crates/a/src/b.rs"])), "debug_assertions"));
            assert!(!with_mode(&Mode::Strict, || covered(condition())) && covered(condition()));
        }
        assert!(!covered(condition()));
    }

    /// Batch 5t, part A: the values come from the units, with no rule for a name.
    #[test]
    fn the_units_that_read_the_file_decide() {
        let test_file = "crates/a/tests/it.rs";
        let ws = workspace(&[(LIB, "#[cfg(test)] mod t {}\n#[cfg(debug_assertions)] fn a() {}\n#[cfg(feature = \"x\")] fn b() {}"), (test_file, "#[cfg(not(test))] fn a() {}\n#[cfg(test)] fn b() {}")]);
        let build = |name: &str, cfg: &[&str]| {
            let mut config = truth(name, cfg);
            let with_test: Vec<&str> = cfg.iter().copied().chain(["test"]).collect();
            config.units.push(unit("it.test", &with_test, &[test_file]));
            config
        };
        let list = [build("dev", &["debug_assertions"]), build("release", &[]), build("dev, all features", &["debug_assertions", "feature=\"x\""])];
        let _scope = enter(Cover::new(&[(&ws, &list)]));
        let covered_at = |file: &str, text: &str| {
            let _at = at(file);
            covered(text.parse().unwrap())
        };
        // `test` is true in each unit that reads the file of the test target.
        assert!(covered_at(LIB, "test") && !covered_at(test_file, "test") && !covered_at(test_file, "not(test)"));
        // With no file, one place that is not covered decides.
        assert!(!covered("test".parse().unwrap()) && covered("debug_assertions".parse().unwrap()));
        assert!(covered_at(test_file, "debug_assertions") && covered_at(LIB, "feature = \"x\"") && covered_at(test_file, "feature = \"x\""));
        assert!(!covered_at("crates/a/src/none.rs", "debug_assertions"));
        drop(_scope);
        // A flag of the compiler makes `debug_assertions` true in each unit, and `feature = "x"` also.
        let flags = [build("dev", &["debug_assertions", "feature=\"x\""]), build("release", &["debug_assertions", "feature=\"x\""])];
        let cover = Cover::new(&[(&ws, &flags)]);
        assert!(cover.covered.is_empty() && cover.not_covered.contains("debug_assertions") && cover.not_covered.contains("feature=\"x\""), "{cover:?}");
        // A record file with no unit record: no condition is covered.
        let mut old = list.clone();
        old[1].known = false;
        assert!(Cover::new(&[(&ws, &old)]).covered.is_empty());
        assert!(Cover::new(&[(&ws, &[])]).covered.is_empty());
        // The value of a string, not its text.
        assert_eq!(parse(&"feature = \"a\\u{62}\"".parse::<TokenStream>().unwrap().into_iter().collect::<Vec<_>>()), Pred::Atom("feature=\"ab\"".to_string()));
        assert_eq!(parse(&"version(\"1.0\")".parse::<TokenStream>().unwrap().into_iter().collect::<Vec<_>>()), Pred::Other);
    }
}
