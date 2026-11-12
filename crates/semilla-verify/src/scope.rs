//! The scope of a property claim (design 25): each body that can run when the function runs.
//!
//! A property claim (`no-io`, `no-panic`, `no-unsafe`, `calls-only`) is "verified" in full only
//! when Semilla examined each body that can run. The scope has:
//!
//! - the function and its local callees (the syntax calls and the MIR callees);
//! - the trait impls of each workspace type that goes to the code of another crate (a generic
//!   argument of a call, an unsize cast to a trait object);
//! - the `Drop` impls of each workspace type that the code can drop.
//!
//! Each part that Semilla cannot examine is a **gap**. A gap makes the result
//! "verified (partial)". The lists in this file are allow lists: an unknown item is a gap or a
//! refutation, never a pass.

use std::collections::{BTreeMap, BTreeSet};

use semilla_syntax::Workspace;

use crate::ItemFacts;

/// The scope of a property claim.
#[derive(Debug, Default)]
pub struct Scope {
    /// The local bodies in the scope, by item path (or by body name for generated code).
    pub items: BTreeSet<String>,
    /// The items that are in the scope only as a trait impl of a type.
    pub through_impls: BTreeSet<String>,
    /// The items that are in the scope only as a `Drop` impl.
    pub drop_bodies: BTreeSet<String>,
    /// Why an item of `through_impls` or `drop_bodies` is in the scope: the type.
    pub why: BTreeMap<String, String>,
    /// The calls to functions outside the workspace: the printed path, with the crate, the def
    /// path, and the first item that has the call.
    pub externals: BTreeMap<String, (String, String, String)>,
    /// The dropped types of the standard library (`std` only): the def path, with the first
    /// item that has the drop.
    pub std_drops: BTreeMap<String, String>,
    /// For a type of `std_drops` that no body of the scope drops itself (batch 5k, D9): the first
    /// call that has the type in its generic arguments, and the item of the call.
    pub std_drop_calls: BTreeMap<String, (String, String)>,
    /// The types of `std_drops` that a body of the scope drops itself.
    pub std_drop_real: BTreeSet<String>,
    /// The parts that Semilla did not examine.
    pub gaps: Vec<String>,
}

/// The crates of the standard library.
fn is_std(krate: &str) -> bool {
    matches!(krate, "core" | "alloc" | "std" | "proc_macro" | "test")
}

/// The modules of the library types that are plain data: a value holds no code that the type
/// arguments do not show. A type that is not in the list can hold a trait object or a function
/// pointer (`core::fmt::Arguments`, `std::io::Error`, `core::task::Waker`).
const PLAIN_TYPES: &[&str] = &[
    "alloc::string",
    "alloc::vec",
    "alloc::boxed",
    "alloc::rc",
    "alloc::sync",
    "alloc::borrow",
    "alloc::collections",
    "alloc::alloc",
    "alloc::ffi",
    "core::option",
    "core::result",
    "core::cell",
    "core::ops",
    "core::cmp",
    "core::num",
    "core::time",
    "core::marker",
    "core::mem",
    "core::pin",
    "core::ptr",
    "core::iter",
    "core::slice",
    "core::str",
    "core::char",
    "core::array",
    "core::convert",
    "core::ffi",
    "core::sync::atomic",
    "core::fmt::Error",
    "core::fmt::Alignment",
    "core::net",
    "core::alloc",
    "core::hash",
    "core::range",
    "std::collections",
    "std::hash",
    "std::ffi",
    "std::path",
    "std::sync",
    "std::time",
    "std::num",
    "std::alloc",
];

/// The modules of `std` whose functions do no input or output. The list is an allow list: a
/// function of `std` outside the list counts as I/O.
pub const STD_NO_IO: &[&str] = &[
    "std::collections",
    "std::hash",
    "std::error",
    "std::f16",
    "std::f32",
    "std::f64",
    "std::f128",
    "std::num",
    "std::ascii",
    "std::ffi",
    "std::sync",
    "std::alloc",
    "std::panic",
    "std::panicking",
    "std::rt",
    "std::thread::local",
    "std::sys::thread_local",
];

/// True when `path` starts with `prefix` at a segment border: `std::fs` matches `std::fs::read`
/// and `std::fs`, and it does not match `std::fsx::read`.
pub fn has_prefix(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// The path tokens of a printed function name: `<std::fs::File as std::io::Read>::read` gives
/// `std::fs::File`, `std::io::Read`, and `read`.
pub fn path_tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')).map(|token| token.trim_matches(':')).filter(|token| !token.is_empty()).collect()
}

/// The decision of `no-io` for one function of another crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Io {
    /// The function does no I/O by the rule.
    No,
    /// The function counts as I/O.
    Yes,
    /// Semilla has no rule for the crate.
    NotExamined,
}

/// The rule of `no-io` for a function of another crate. `krate` is the crate that defines the
/// function, and `def` is its def path in that crate. A re-export does not change them.
///
/// - `core` and `alloc` have no operating system below them: no I/O.
/// - `std`: each `std::…` path in the def path must be in [`STD_NO_IO`]. Else the function
///   counts as I/O.
/// - Each other crate: not examined.
pub fn io_class(krate: &str, def: &str) -> Io {
    match krate {
        "core" | "alloc" => Io::No,
        "std" => {
            let tokens: Vec<&str> = path_tokens(def).into_iter().filter(|token| has_prefix(token, "std")).collect();
            if !tokens.is_empty() && tokens.iter().all(|token| STD_NO_IO.iter().any(|module| has_prefix(token, module))) { Io::No } else { Io::Yes }
        }
        _ => Io::NotExamined,
    }
}

/// The crate and the def path of an external call. An old record has no origin: the printed
/// path decides. That is safe: a printed `core::…` or `alloc::…` path is never a function of
/// `std`, and the rule of `std` is the strict one.
pub fn origin(printed: &str, origins: &BTreeMap<String, (String, String)>) -> (String, String) {
    if let Some(found) = origins.get(printed) {
        return found.clone();
    }
    let first = path_tokens(printed).into_iter().find(|token| token.contains("::")).unwrap_or("");
    (first.split("::").next().unwrap_or("").to_string(), printed.to_string())
}

/// True for a library type that is plain data.
fn is_plain(path: &str) -> bool {
    PLAIN_TYPES.iter().any(|module| has_prefix(path, module))
}

/// The crates of the workspace.
fn local_crates(after: &Workspace) -> BTreeSet<String> {
    after.crates.iter().map(|c| c.name.clone()).chain(after.items.values().map(|item| item.path.split("::").next().unwrap_or("").to_string())).collect()
}

/// Computes the scope of a property claim on `root`. `reached` is the function and its callees
/// by the syntax.
pub fn scope(root: &str, reached: &BTreeSet<String>, facts: &BTreeMap<String, ItemFacts>, after: &Workspace) -> Scope {
    let mut scope = Scope { items: reached.clone(), ..Scope::default() };
    let local = local_crates(after);
    let mut types: BTreeSet<String> = BTreeSet::new();
    let mut drops: BTreeSet<String> = BTreeSet::new();
    let mut gaps: BTreeSet<String> = BTreeSet::new();
    let mut indirect: BTreeMap<String, usize> = BTreeMap::new();
    let mut todo: Vec<String> = reached.iter().cloned().collect();
    let mut done: BTreeSet<String> = BTreeSet::new();
    loop {
        while let Some(current) = todo.pop() {
            if !done.insert(current.clone()) {
                continue;
            }
            let Some(found) = facts.get(&current) else {
                if after.items.get(&current).is_some_and(|item| item.body.is_some()) {
                    gaps.insert(format!("`{current}` has no MIR"));
                }
                continue;
            };
            for callee in &found.callees {
                if scope.items.insert(callee.clone()) {
                    todo.push(callee.clone());
                }
            }
            for call in &found.externals {
                let (krate, def) = origin(call, &found.origins);
                if after.items.contains_key(call.as_str()) || facts.contains_key(call.as_str()) {
                    // A function of another crate of the workspace.
                    if scope.items.insert(call.clone()) {
                        todo.push(call.clone());
                    }
                } else if local.contains(&krate) && !is_std(&krate) {
                    gaps.insert(format!("`{call}` (a function of the workspace) has no MIR"));
                } else {
                    scope.externals.entry(call.clone()).or_insert((krate, def, current.clone()));
                }
            }
            for (path, krate) in &found.reach_types {
                if local.contains(krate) && !is_std(krate) {
                    types.insert(path.clone());
                } else if is_std(krate) {
                    if !is_plain(path) {
                        gaps.insert(format!("a value of `{path}` goes to the code of another crate, and it can hold code that Semilla does not see"));
                    }
                } else {
                    gaps.insert(format!("the trait impls of `{path}` (the crate `{krate}`)"));
                }
            }
            for (path, krate) in &found.drop_types {
                if local.contains(krate) && !is_std(krate) {
                    drops.insert(path.clone());
                } else if krate == "std" {
                    scope.std_drops.entry(path.clone()).or_insert(current.clone());
                    match found.drop_calls.get(path).filter(|_| !found.drop_real.contains(path)) {
                        Some(call) => drop(scope.std_drop_calls.entry(path.clone()).or_insert((call.clone(), current.clone()))),
                        None => drop(scope.std_drop_real.insert(path.clone())),
                    }
                } else {
                    gaps.insert(format!("the destructor of `{path}` (the crate `{krate}`)"));
                }
            }
            for (path, krate) in &found.const_types {
                match (path.as_str(), is_std(krate)) {
                    ("dyn", _) if krate.is_empty() => drop(gaps.insert(format!("a static or a constant in `{current}` holds a trait object"))),
                    ("fn", _) if krate.is_empty() => drop(gaps.insert(format!("a static or a constant in `{current}` holds a function pointer"))),
                    (_, true) if !is_plain(path) => drop(gaps.insert(format!("a static or a constant in `{current}` holds a value of `{path}`, and it can hold code that Semilla does not see"))),
                    _ => {}
                }
            }
            for call in &found.indirect {
                let kind = call.split(" (in `").next().unwrap_or(call).to_string();
                *indirect.entry(kind).or_default() += 1;
            }
        }
        // The trait impls of the reached types, and the `Drop` impls of the dropped types.
        for (name, found) in facts {
            if scope.items.contains(name) {
                continue;
            }
            if let Some(ty) = found.drop_of.iter().find(|ty| drops.contains(*ty)) {
                scope.items.insert(name.clone());
                scope.drop_bodies.insert(name.clone());
                scope.why.insert(name.clone(), format!("the code can drop a value of `{ty}`"));
                todo.push(name.clone());
            } else if let Some(ty) = found.impl_of.iter().find(|ty| types.contains(*ty)) {
                scope.items.insert(name.clone());
                scope.through_impls.insert(name.clone());
                scope.why.insert(name.clone(), format!("a trait impl of `{ty}`, which goes to the code of another crate"));
                todo.push(name.clone());
            }
        }
        if todo.is_empty() {
            break;
        }
    }
    // A dropped workspace type must have its `Drop` body in the facts.
    for ty in &drops {
        if !facts.values().any(|found| found.drop_of.contains(ty)) {
            gaps.insert(format!("the `Drop` impl of `{ty}` has no MIR"));
        }
    }
    // The values that come from the outside: the parameters of the function.
    if let Some(found) = facts.get(root) {
        for (path, krate) in &found.param_types {
            match (path.as_str(), is_std(krate)) {
                ("dyn", _) if krate.is_empty() => drop(gaps.insert(format!("a parameter of `{root}` is or holds a trait object: the caller gives its code"))),
                ("fn", _) if krate.is_empty() => drop(gaps.insert(format!("a parameter of `{root}` is or holds a function pointer: the caller gives its code"))),
                (_, true) if !is_plain(path) => drop(gaps.insert(format!("a parameter of `{root}` holds a value of `{path}`, and it can hold code that Semilla does not see"))),
                _ => {}
            }
        }
    }
    // A global allocator or a panic handler in the workspace runs with no call in the MIR.
    for item in after.items.values() {
        for (attr, what) in [("global_allocator", "each allocation runs its code"), ("panic_handler", "each panic runs its code")] {
            // `#[global_allocator]`, and the same attribute in `#[cfg_attr(…)]` or `#[unsafe(…)]`.
            if item.attrs.iter().any(|a| a.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| word == attr)) {
                gaps.insert(format!("the workspace has a `#[{attr}]` (`{}`): {what}, and Semilla does not follow it", item.path));
            }
        }
    }
    scope.gaps = indirect.into_iter().map(|(kind, count)| if count == 1 { kind } else { format!("{kind} ({count} places)") }).chain(gaps).collect();
    scope
}

impl Scope {
    /// The text "in `item` (why the item is in the scope)".
    pub fn place(&self, item: &str) -> String {
        match self.why.get(item) {
            Some(why) => format!("in `{item}` ({why})"),
            None => format!("in `{item}`"),
        }
    }

    /// The number of local bodies that Semilla examined.
    pub fn examined(&self, facts: &BTreeMap<String, ItemFacts>) -> usize {
        self.items.iter().filter(|item| facts.contains_key(*item)).count()
    }

    /// The text "examined: N local bodies (M through trait impls, K drop bodies); not examined: …".
    /// `more` has the gaps of the one property (for example the calls to other crates).
    pub fn text(&self, facts: &BTreeMap<String, ItemFacts>, more: &[String]) -> String {
        let gaps: Vec<&String> = self.gaps.iter().chain(more).collect();
        let mut text = format!("examined: {} local bodies ({} through trait impls, {} drop bodies)", self.examined(facts), self.through_impls.len(), self.drop_bodies.len());
        if !gaps.is_empty() {
            let shown: Vec<String> = gaps.iter().take(6).map(|gap| gap.to_string()).collect();
            let rest = if gaps.len() > 6 { format!("; and {} more", gaps.len() - 6) } else { String::new() };
            text.push_str(&format!("; not examined: {}{rest}", shown.join("; ")));
        }
        text
    }
}
