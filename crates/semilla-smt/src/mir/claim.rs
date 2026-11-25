//! The check of a property claim of an intent file (design 67, part C).
//!
//! A claim is a Rust function whose body must be true for each value of its parameters. The
//! check runs the body on symbolic inputs with the machine of the equivalence check: a call
//! `before::PATH(…)` runs the body of the old version, and `after::PATH(…)` runs the body of
//! the new version, with the inline rules of the equivalence check (design 61). Then Z3 looks
//! for an input where the body is false or panics.
//!
//! # Soundness
//!
//! - "proved" comes only from an `unsat` answer: no input exists for which a path of the body
//!   gives `false`, a panic outside `panics`, or an abort.
//! - "false" comes only from an exact model (design 61.4): the path condition and the failed
//!   condition use only the inputs of modeled types and literals.
//! - Each part that the check does not model gives "claimed" with the cause. A stop of a call
//!   run (a loop, an operation outside the model, a changed callee that the check cannot
//!   inline) on any path gives "claimed": the check never skips a path.
//! - Each call of a claim starts in the world of the input: a call does not see the effects of
//!   another call of the claim. The k-th call with an effect of two calls has the same result
//!   if the callee and the inputs are the same (as in the equivalence check).
//! - `==` compares values only for types whose `PartialEq` compares the values (integers,
//!   `bool`, `char`, tuples, sequences of scalars, `Option` and `Result` of these). For a type
//!   of the workspace, `==` calls `PartialEq::eq` of the version. `==` on another type is
//!   outside the model.

use semilla_intent::{BinOp, Claim, Error, Expr, ExprKind, File, Func, IPath, Lit, Stmt, Type, UnOp, Version};

use super::*;

/// One tree of the step for the check of a claim.
pub struct Tree<'a> {
    /// The form of a body of the workspace by its name.
    pub workspace: &'a dyn Fn(&str) -> Option<Arc<Ir>>,
    /// The forms of the small callees of the standard library and of the instances.
    pub std: &'a BTreeMap<String, Arc<Ir>>,
    /// The map of a callee text of this tree to the text of the new tree (the rename map of the
    /// step for the old tree; the identity for the new tree).
    pub rename: &'a dyn Fn(&str) -> String,
    /// The name of each body of the tree (the record names).
    pub names: &'a [String],
}

/// The result of the check of one claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimVerdict {
    /// The body holds for each input: the method (`SMT`, `SMT after inlining …`).
    Proved { method: String },
    /// An exact input for which the body is false or panics, and the part that fails.
    False { input: String, detail: String },
    /// Semilla cannot check the claim: the cause.
    Claimed { cause: String },
}

/// The check of one claim with its cost.
#[derive(Clone, Debug)]
pub struct ClaimCheck {
    pub verdict: ClaimVerdict,
    /// The reference errors and the type errors.
    pub errors: Vec<Error>,
    /// The bodies that the claim reads: the version and the name.
    pub reads: Vec<(Version, String)>,
    pub queries: usize,
    pub millis: u128,
}

/// The options of one check.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaimOptions {
    /// `except = [panic_message]`: two panics of one kind are the same panic.
    pub except_panic_message: bool,
    /// Resolve the references and check the types only (`semilla intent check`): no run of a
    /// body and no solver.
    pub dry: bool,
}

/// The cause of the result of a dry check.
pub const DRY_CAUSE: &str = "not checked: `semilla intent check` checks the references and the types only";

/// The most paths of one claim.
const MAX_CLAIM_PATHS: usize = 256;
/// The deepest call of a relation.
const MAX_RELATIONS: usize = 8;

/// Checks one claim with a property body against the two trees.
pub fn check_claim(solver: &Solver, file: &File, claim: &Claim, old: &Tree, new: &Tree, step: &Step, options: ClaimOptions, deadline: Option<Instant>) -> ClaimCheck {
    let started = Instant::now();
    let outer = DEADLINE.with(|cell| cell.get());
    let own = match (outer, deadline) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    DEADLINE.with(|cell| cell.set(own));
    let mut queries = 0;
    let mut first = attempt(solver, file, claim, old, new, step, options, false, &mut queries);
    // [R67.3-1] The second try inlines each callee with a form, as the equivalence check does.
    if !options.dry && first.errors.is_empty() && matches!(&first.verdict, ClaimVerdict::Claimed { cause } if cause != DEADLINE_CAUSE) {
        let second = attempt(solver, file, claim, old, new, step, options, true, &mut queries);
        if !matches!(second.verdict, ClaimVerdict::Claimed { .. }) {
            first = second;
        }
    }
    DEADLINE.with(|cell| cell.set(outer));
    ClaimCheck { verdict: first.verdict, errors: first.errors, reads: first.reads.into_iter().collect(), queries, millis: started.elapsed().as_millis() }
}

struct Attempt {
    verdict: ClaimVerdict,
    errors: Vec<Error>,
    reads: BTreeSet<(Version, String)>,
}

#[allow(clippy::too_many_arguments)]
fn attempt(solver: &Solver, file: &File, claim: &Claim, old: &Tree, new: &Tree, step: &Step, options: ClaimOptions, inline_all: bool, queries: &mut usize) -> Attempt {
    let mut ev = Eval::new(file, old, new, step, options, inline_all);
    let verdict = match ev.claim(solver, claim, queries) {
        Ok(verdict) => verdict,
        Err(Fail::Error(error)) => {
            return Attempt { verdict: ClaimVerdict::Claimed { cause: format!("an error of the claim: {}", error.message) }, errors: vec![error], reads: ev.reads };
        }
        Err(Fail::Claimed(cause)) => ClaimVerdict::Claimed { cause },
    };
    Attempt { verdict, errors: Vec::new(), reads: ev.reads }
}

/// A value of the claim: the value, its type, and the version of the names of its type (`None`
/// for a type with no name of the workspace).
#[derive(Clone, Debug)]
struct Val {
    v: V,
    ty: Ty,
    ver: Option<Version>,
}

/// The state of one path of the claim.
#[derive(Clone, Default)]
struct World {
    pc: Vec<T>,
    slots: Vec<V>,
    types: Vec<Option<Ty>>,
    /// The effects of the calls of this path, in order (for `same_outcome`).
    effects: Vec<Effect>,
    /// The local values: the parameters and each `let`.
    env: Vec<(String, Val)>,
    /// Each call of a version on this path and its outcome, for the text of a false claim.
    shown: Vec<(String, Shown)>,
}

#[derive(Clone)]
enum Shown {
    Value(V, Ty),
    Text(String),
}

/// The outcome of an expression on one path.
#[derive(Clone)]
enum Out {
    Val(Val),
    /// A panic: the kind, the arguments, and the text of the expression.
    Panic { kind: String, args: Vec<V>, at: String },
    /// An abort: the text of the expression.
    Abort(String),
}

/// Why an expression has no outcome.
enum Fail {
    /// A reference error or a type error: the claim is wrong.
    Error(Error),
    /// A part outside the model: the claim is "claimed".
    Claimed(String),
}

type Branches = Vec<(World, Out)>;

struct Eval<'a, 'b> {
    file: &'a File,
    old: &'a Tree<'b>,
    new: &'a Tree<'b>,
    step: &'a Step<'b>,
    options: ClaimOptions,
    inline_all: bool,
    decls: Decls,
    /// The types of the claim (the parameters, the literals, the references).
    arena: Vec<IrTy>,
    arena_ir: Arc<Ir>,
    /// The types that the arena has from a form: (the address of the form, the index) → index.
    imported: BTreeMap<(usize, u32), u32>,
    /// The named types of each version (the cache of the search).
    named: BTreeMap<(Version, String), Option<Ty>>,
    reads: BTreeSet<(Version, String)>,
    inlined: BTreeSet<String>,
    fresh: usize,
    relations: usize,
}

fn fail(place: &semilla_intent::Place, message: impl Into<String>) -> Fail {
    Fail::Error(Error { place: place.clone(), message: message.into() })
}

/// The integer form of a type: the width and the sign.
fn int_of(ty: &Ty) -> Option<(u32, bool)> {
    match ty.get()? {
        IrTy::Int(w, s) => Some((*w, *s)),
        IrTy::Char => Some((32, false)),
        _ => None,
    }
}

/// The kind of a type of the standard library that the claim language knows by its variants:
/// `Option` (the variants `None` and `Some`) or `Result` (`Ok` and `Err`).
fn variant_kind(ty: &Ty) -> Option<&'static str> {
    match ty.get()? {
        IrTy::Adt { vn, e: true, .. } if vn.len() == 2 && vn[0] == "None" && vn[1] == "Some" && std_path(ty) => Some("Option"),
        IrTy::Adt { vn, e: true, .. } if vn.len() == 2 && vn[0] == "Ok" && vn[1] == "Err" && std_path(ty) => Some("Result"),
        _ => None,
    }
}

/// True if the name of an ADT type is a path of the standard library (`std::`, `core::`).
fn std_path(ty: &Ty) -> bool {
    match ty.get() {
        Some(IrTy::Adt { n, .. }) => n.starts_with("std::") || n.starts_with("core::"),
        _ => false,
    }
}

/// The text of a type, as the compiler writes it.
fn ty_text(ty: &Ty) -> String {
    ty_text_in(ty, 0)
}

fn ty_text_in(ty: &Ty, depth: usize) -> String {
    if depth > 8 {
        return "…".into();
    }
    match ty.get() {
        Some(IrTy::Bool) => "bool".into(),
        Some(IrTy::Char) => "char".into(),
        Some(IrTy::Unit) => "()".into(),
        Some(IrTy::Never) => "!".into(),
        Some(IrTy::Int(w, s)) => format!("{}{w}", if *s { "i" } else { "u" }),
        Some(IrTy::Tuple(items)) => format!("({})", items.iter().map(|item| ty_text_in(&ty.at(*item), depth + 1)).collect::<Vec<_>>().join(", ")),
        Some(IrTy::Adt { n, .. }) | Some(IrTy::Opaque { n, .. }) | Some(IrTy::Clo { n, .. }) => n.clone(),
        Some(IrTy::Ref(true, inner)) => format!("&mut {}", ty_text_in(&ty.at(*inner), depth + 1)),
        Some(IrTy::Ref(false, inner)) => format!("&{}", ty_text_in(&ty.at(*inner), depth + 1)),
        Some(IrTy::Seq { e, n, k }) => match (k.as_str(), n) {
            ("v", _) => format!("std::vec::Vec<{}>", ty_text_in(&ty.at(*e), depth + 1)),
            ("a", Some(n)) => format!("[{}; {n}]", ty_text_in(&ty.at(*e), depth + 1)),
            _ => format!("[{}]", ty_text_in(&ty.at(*e), depth + 1)),
        },
        None => "?".into(),
    }
}

/// True if a value has a reference to a slot at or after `limit` (a value of a callee frame).
fn refers_beyond(value: &V, limit: usize) -> bool {
    match value {
        V::Ptr(loc) => loc.slot >= limit,
        V::Tup(items) => items.iter().any(|item| refers_beyond(item, limit)),
        V::Enum { f, .. } => f.iter().flatten().any(|item| refers_beyond(item, limit)),
        _ => false,
    }
}

/// A value with no meaning for the fields of a variant that the value does not have.
fn filler(ty: &Ty, depth: usize) -> V {
    if depth > 6 {
        return V::Undef;
    }
    match ty.get() {
        Some(IrTy::Bool) => V::Bool(t("false")),
        Some(IrTy::Int(w, _)) => V::Bv(bv_lit(0, *w), *w),
        Some(IrTy::Char) => V::Bv(bv_lit(0, 32), 32),
        Some(IrTy::Unit) => V::Unit,
        Some(IrTy::Tuple(items)) => V::Tup(items.iter().map(|item| filler(&ty.at(*item), depth + 1)).collect()),
        Some(IrTy::Adt { e: false, v, .. }) => V::Tup(v.first().map(|list| list.iter().map(|item| filler(&ty.at(*item), depth + 1)).collect()).unwrap_or_default()),
        Some(IrTy::Adt { e: true, v, d, dt, .. }) => {
            let (w, ds) = discriminants(&ty.ir, d, *dt);
            V::Enum { d: bv_lit(ds.first().copied().unwrap_or(0), w), w, ds, f: v.iter().map(|list| list.iter().map(|item| filler(&ty.at(*item), depth + 1)).collect()).collect() }
        }
        Some(IrTy::Seq { e, .. }) => match SeqE::of(&ty.at(*e)) {
            Some(e) => V::Seq(Sq::empty(e)),
            None => V::Undef,
        },
        _ => V::Undef,
    }
}

impl<'a, 'b> Eval<'a, 'b> {
    fn new(file: &'a File, old: &'a Tree<'b>, new: &'a Tree<'b>, step: &'a Step<'b>, options: ClaimOptions, inline_all: bool) -> Self {
        Eval {
            file,
            old,
            new,
            step,
            options,
            inline_all,
            decls: Decls::default(),
            arena: Vec::new(),
            arena_ir: Arc::new(Ir::default()),
            imported: BTreeMap::new(),
            named: BTreeMap::new(),
            reads: BTreeSet::new(),
            inlined: BTreeSet::new(),
            fresh: 0,
            relations: 0,
        }
    }

    fn tree(&self, version: Version) -> &'a Tree<'b> {
        match version {
            Version::Before => self.old,
            Version::After => self.new,
        }
    }

    // -----------------------------------------------------------------------------------------
    // The types.

    /// Adds a type to the arena.
    fn add(&mut self, ty: IrTy) -> Ty {
        self.arena.push(ty);
        self.arena_ir = Arc::new(Ir { t: self.arena.clone(), ..Ir::default() });
        Ty { ir: self.arena_ir.clone(), id: (self.arena.len() - 1) as u32 }
    }

    /// The index of a type of a form in the arena (a copy of the type and of its parts).
    fn import(&mut self, ty: &Ty) -> u32 {
        // A type of the arena (a snapshot of it is a prefix of the arena).
        if self.arena_prefix(&ty.ir) {
            return ty.id;
        }
        let key = (Arc::as_ptr(&ty.ir) as usize, ty.id);
        if let Some(found) = self.imported.get(&key) {
            return *found;
        }
        let id = self.arena.len() as u32;
        self.arena.push(IrTy::Unit);
        self.imported.insert(key, id);
        let copied = match ty.get().cloned() {
            Some(IrTy::Tuple(items)) => IrTy::Tuple(items.iter().map(|item| self.import(&ty.at(*item))).collect()),
            Some(IrTy::Adt { n, v, vn, d, dt, e, fs }) => {
                let v = v.iter().map(|list| list.iter().map(|item| self.import(&ty.at(*item))).collect()).collect();
                let dt = dt.map(|dt| self.import(&ty.at(dt)));
                IrTy::Adt { n, v, vn, d, dt, e, fs }
            }
            Some(IrTy::Ref(m, inner)) => IrTy::Ref(m, self.import(&ty.at(inner))),
            Some(IrTy::Seq { e, n, k }) => IrTy::Seq { e: self.import(&ty.at(e)), n, k },
            Some(IrTy::Clo { n, v, g, d, s, q }) => IrTy::Clo { n, v: v.iter().map(|item| self.import(&ty.at(*item))).collect(), g, d, s, q },
            Some(other) => other,
            None => IrTy::Opaque { n: "?".into(), d: false },
        };
        self.arena[id as usize] = copied;
        self.arena_ir = Arc::new(Ir { t: self.arena.clone(), ..Ir::default() });
        id
    }

    /// True if a form is a snapshot of the arena (its types are a prefix of the arena).
    fn arena_prefix(&self, ir: &Ir) -> bool {
        ir.b.is_empty() && ir.l.is_empty() && ir.t.len() <= self.arena.len() && ir.t[..] == self.arena[..ir.t.len()]
    }

    /// The type `&T` (shared).
    fn ref_to(&mut self, ty: &Ty) -> Ty {
        let inner = self.import(ty);
        self.add(IrTy::Ref(false, inner))
    }

    fn bool_ty(&mut self) -> Ty {
        self.add(IrTy::Bool)
    }

    fn usize_ty(&mut self) -> Ty {
        self.add(IrTy::Int(64, false))
    }

    /// A named type of a version: an ADT or an opaque type of a form of the tree with this text.
    fn named_type(&mut self, version: Version, text: &str) -> Option<Ty> {
        let key = (version, text.to_string());
        if let Some(found) = self.named.get(&key) {
            return found.clone();
        }
        let tree = self.tree(version);
        let mut found = None;
        // The forms of the bodies that the claim names first, then each form.
        let mut names: Vec<String> = self.reads.iter().filter(|(ver, _)| *ver == version).map(|(_, name)| name.clone()).collect();
        names.extend(tree.names.iter().cloned());
        'search: for name in names {
            let Some(form) = (tree.workspace)(&name) else { continue };
            for (index, item) in form.t.iter().enumerate() {
                let n = match item {
                    IrTy::Adt { n, .. } | IrTy::Opaque { n, .. } => n,
                    _ => continue,
                };
                if n == text {
                    found = Some(Ty { ir: form.clone(), id: index as u32 });
                    break 'search;
                }
            }
        }
        self.named.insert(key, found.clone());
        found
    }

    /// The names of the ADT types of a version that are near a wanted name (for an error).
    fn type_candidates(&self, version: Version, wanted: &str) -> Vec<String> {
        let tree = self.tree(version);
        let mut names: BTreeSet<String> = BTreeSet::new();
        for name in tree.names.iter() {
            let Some(form) = (tree.workspace)(name) else { continue };
            for item in &form.t {
                if let IrTy::Adt { n, .. } = item
                    && !n.starts_with("std::")
                    && !n.starts_with("core::")
                {
                    names.insert(n.clone());
                }
            }
            if names.len() > 4000 {
                break;
            }
        }
        semilla_intent::candidates(wanted, &names.into_iter().collect::<Vec<_>>(), 6)
    }

    /// The crates of the workspace of a tree: the first segment of each body name.
    fn is_workspace_crate(&self, segment: &str) -> bool {
        [self.old, self.new].iter().any(|tree| tree.names.iter().any(|name| name.split("::").next() == Some(segment)))
    }

    /// The type of a type of the claim, and the version of its names.
    fn resolve_type(&mut self, ty: &Type) -> Result<(Ty, Option<Version>), Fail> {
        match ty {
            Type::Ref(true, _) => Err(Fail::Claimed(format!("a `&mut` type (`{}`): the claims of this batch read values only", ty.text()))),
            Type::Ref(false, inner) => {
                let (inner, ver) = self.resolve_type(inner)?;
                Ok((self.ref_to(&inner), ver))
            }
            Type::Tuple(items) => {
                if items.is_empty() {
                    return Ok((self.add(IrTy::Unit), None));
                }
                let mut ids = Vec::new();
                let mut ver = None;
                for item in items {
                    let (found, v) = self.resolve_type(item)?;
                    ver = ver.or(v);
                    ids.push(self.import(&found));
                }
                Ok((self.add(IrTy::Tuple(ids)), ver))
            }
            Type::Slice(inner) | Type::Array(inner, _) => {
                let (found, ver) = self.resolve_type(inner)?;
                if SeqE::of(&found).is_none() {
                    return Err(Fail::Claimed(format!("a sequence of `{}`: the model has sequences of integers, `char`, and `bool` only", ty_text(&found))));
                }
                let e = self.import(&found);
                let (n, k) = match ty {
                    Type::Array(_, n) => (Some(*n), "a"),
                    _ => (None, "s"),
                };
                Ok((self.add(IrTy::Seq { e, n, k: k.into() }), ver))
            }
            Type::Other(text) => Err(Fail::Claimed(format!("the type `{text}` is outside the language of this batch"))),
            Type::Path(path) => self.path_type(path),
        }
    }

    fn path_type(&mut self, path: &IPath) -> Result<(Ty, Option<Version>), Fail> {
        let name = path.name();
        if path.version.is_none() && path.segments.len() == 1 {
            let scalar = match name.as_str() {
                "bool" => Some(IrTy::Bool),
                "char" => Some(IrTy::Char),
                "usize" => Some(IrTy::Int(64, false)),
                "isize" => Some(IrTy::Int(64, true)),
                other => other.strip_prefix('u').or_else(|| other.strip_prefix('i')).and_then(|bits| bits.parse::<u32>().ok()).filter(|bits| [8, 16, 32, 64, 128].contains(bits)).map(|bits| IrTy::Int(bits, other.starts_with('i'))),
            };
            if let Some(scalar) = scalar {
                return Ok((self.add(scalar), None));
            }
            let mut args = Vec::new();
            let mut ver = None;
            for generic in &path.generics {
                let (found, v) = self.resolve_type(generic)?;
                ver = ver.or(v);
                args.push(found);
            }
            match (name.as_str(), args.as_slice()) {
                ("Vec", [item]) => {
                    if SeqE::of(item).is_none() {
                        return Err(Fail::Claimed(format!("a `Vec<{}>`: the model has sequences of integers, `char`, and `bool` only", ty_text(item))));
                    }
                    let e = self.import(item);
                    return Ok((self.add(IrTy::Seq { e, n: None, k: "v".into() }), ver));
                }
                ("Option", [item]) => {
                    let text = format!("std::option::Option<{}>", ty_text(item));
                    let found = match ver.and_then(|ver| self.named_type(ver, &text)).or_else(|| self.named_type(Version::After, &text)).or_else(|| self.named_type(Version::Before, &text)) {
                        Some(found) => found,
                        None => {
                            let item = self.import(item);
                            let dt = self.add(IrTy::Int(64, true)).id;
                            self.add(IrTy::Adt { n: text, v: vec![Vec::new(), vec![item]], vn: vec!["None".into(), "Some".into()], d: vec!["0".into(), "1".into()], dt: Some(dt), e: true, fs: vec![Vec::new(), vec!["0".into()]] })
                        }
                    };
                    return Ok((found, ver));
                }
                ("Result", [ok, err]) => {
                    let text = format!("std::result::Result<{}, {}>", ty_text(ok), ty_text(err));
                    let found = match ver.and_then(|ver| self.named_type(ver, &text)).or_else(|| self.named_type(Version::After, &text)).or_else(|| self.named_type(Version::Before, &text)) {
                        Some(found) => found,
                        None => {
                            let (ok, err) = (self.import(ok), self.import(err));
                            let dt = self.add(IrTy::Int(64, true)).id;
                            self.add(IrTy::Adt { n: text, v: vec![vec![ok], vec![err]], vn: vec!["Ok".into(), "Err".into()], d: vec!["0".into(), "1".into()], dt: Some(dt), e: true, fs: vec![vec!["0".into()], vec!["0".into()]] })
                        }
                    };
                    return Ok((found, ver));
                }
                ("String", []) => {
                    let text = "std::string::String".to_string();
                    let found = self.named_type(Version::After, &text).or_else(|| self.named_type(Version::Before, &text)).unwrap_or_else(|| self.add(IrTy::Opaque { n: text, d: true }));
                    return Ok((found, None));
                }
                _ => {}
            }
        }
        let text = if path.generics.is_empty() { name.clone() } else { format!("{name}<{}>", path.generics.iter().map(Type::text).collect::<Vec<_>>().join(", ")) };
        match path.version {
            Some(version) => match self.named_type(version, &text) {
                Some(found) => Ok((found, Some(version))),
                None => {
                    let candidates = self.type_candidates(version, &name);
                    Err(fail(&path.place, format!("`{}`: the {} version has no type `{text}` in the compiler data of its bodies{}", path.text(), version.word(), candidate_text(&candidates))))
                }
            },
            None => {
                let (a, b) = (self.named_type(Version::Before, &text), self.named_type(Version::After, &text));
                match (a, b) {
                    (Some(a), Some(b)) if same_type(&a, &b, 0, self.old.rename) => Ok((b, Some(Version::After))),
                    (Some(_), Some(_)) => Err(fail(&path.place, format!("`{text}` is a different type in the two versions: write `before::{text}` or `after::{text}`"))),
                    (Some(_), None) => Err(fail(&path.place, format!("only the old version has the type `{text}`: write `before::{text}`"))),
                    (None, Some(_)) => Err(fail(&path.place, format!("only the new version has the type `{text}`: write `after::{text}`"))),
                    (None, None) if self.is_workspace_crate(&path.segments[0]) => {
                        let candidates = self.type_candidates(Version::After, &name);
                        Err(fail(&path.place, format!("no version has the type `{text}`{}", candidate_text(&candidates))))
                    }
                    (None, None) => Err(Fail::Claimed(format!("the type `{text}` is outside the model of this batch"))),
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // The claim.

    fn claim(&mut self, solver: &Solver, claim: &Claim, queries: &mut usize) -> Result<ClaimVerdict, Fail> {
        let Some(body) = &claim.func.body else { return Err(Fail::Claimed("the claim has no body".into())) };
        // The bodies that the claim calls first: their forms have the types of the claim.
        let mut calls = Vec::new();
        semilla_intent::calls_of(body, &mut calls);
        for (version, name) in calls {
            if (self.tree(version).workspace)(&name).is_some() {
                self.reads.insert((version, name));
            }
        }
        let mut world = World::default();
        let mut muts = Vec::new();
        for (name, ty, place) in &claim.func.params {
            let (ty, ver) = self.resolve_type(ty).map_err(|error| match error {
                Fail::Error(mut error) => {
                    if error.place.line == 0 {
                        error.place = place.clone();
                    }
                    Fail::Error(error)
                }
                other => other,
            })?;
            let value = input(&mut self.decls, &ty, name, &mut world.slots, &mut world.types, &mut muts, 0).map_err(|Stop(cause)| Fail::Claimed(format!("the parameter `{name}`: {cause}")))?;
            self.decls.inputs.push((name.clone(), value.clone(), ty.clone()));
            world.env.push((name.clone(), Val { v: value, ty, ver }));
        }
        let want = self.bool_ty();
        let branches = self.eval(body, world, Some(&want))?;
        let mut fails: Vec<(T, World, Out)> = Vec::new();
        for (world, out) in branches {
            match &out {
                Out::Val(value) => {
                    let V::Bool(term) = &value.v else {
                        return Err(fail(&body.place, format!("the body of a claim must be a `bool` expression; it has the type `{}`", ty_text(&value.ty))));
                    };
                    let failed = and(&[and(&world.pc), not(term)]);
                    if lit_bool(&failed) != Some(false) {
                        fails.push((failed, world, out));
                    }
                }
                Out::Panic { .. } | Out::Abort(_) => {
                    let failed = and(&world.pc);
                    if lit_bool(&failed) != Some(false) {
                        fails.push((failed, world, out));
                    }
                }
            }
        }
        if self.options.dry {
            return Ok(ClaimVerdict::Claimed { cause: DRY_CAUSE.into() });
        }
        if past_deadline() {
            return Err(Fail::Claimed(DEADLINE_CAUSE.into()));
        }
        let mut text = String::new();
        for line in &self.decls.lines {
            text.push_str(line);
            text.push('\n');
        }
        for (index, (failed, _, _)) in fails.iter().enumerate() {
            text.push_str(&format!("(define-fun |f{index}| () Bool {})\n", failed.s));
        }
        for line in &self.decls.assume {
            text.push_str(&format!("(assert {line})\n"));
        }
        if self.decls.consts.len() > 1 {
            text.push_str(&format!("(assert (distinct {}))\n", self.decls.consts.join(" ")));
        }
        text.push_str(&format!("(assert (or {} false))\n", (0..fails.len()).map(|index| format!("|f{index}|")).collect::<Vec<_>>().join(" ")));
        let mut symbols: Vec<String> = Vec::new();
        for (_, value, _) in &self.decls.inputs {
            terms_of(value, &mut symbols);
        }
        for slot in self.decls.inputs.iter().filter_map(|(_, value, _)| if let V::Ptr(loc) = value { Some(loc.slot) } else { None }) {
            if let Some(found) = fails.first().and_then(|(_, world, _)| world.slots.get(slot)) {
                terms_of(found, &mut symbols);
            }
        }
        symbols.extend((0..fails.len()).map(|index| format!("|f{index}|")));
        *queries += 1;
        if std::env::var_os("SEMILLA_CLAIM_DEBUG").is_some() {
            eprintln!("[claim] {}\n{text}", claim.func.name);
        }
        let answer = match solver.check(&text, &symbols) {
            Ok((_, answer)) => answer,
            Err(error) => return Err(Fail::Claimed(format!("the solver failed: {error}"))),
        };
        let model = match answer {
            Answer::Unsat => {
                let method = method(&self.inlined);
                return Ok(ClaimVerdict::Proved { method });
            }
            Answer::Unknown(reason) => return Err(Fail::Claimed(format!("the solver gave no answer ({reason})"))),
            Answer::Sat(model) => model,
        };
        let mut model: BTreeMap<String, String> = model.into_iter().collect();
        let pick = |model: &BTreeMap<String, String>| (0..fails.len()).find(|index| model.get(&format!("|f{index}|")).map(String::as_str) == Some("true"));
        let Some(mut index) = pick(&model) else { return Err(Fail::Claimed("the model of the solver has no failed path".into())) };
        // [R67.3-2] A false claim needs an exact model (design 61.4).
        if !fails[index].0.x {
            // An exact failed path can exist next to an inexact one.
            let exact: Vec<usize> = (0..fails.len()).filter(|index| fails[*index].0.x).collect();
            let mut found = None;
            if !exact.is_empty() {
                let mut only = text.clone();
                only.push_str(&format!("(assert (or {} false))\n", exact.iter().map(|index| format!("|f{index}|")).collect::<Vec<_>>().join(" ")));
                *queries += 1;
                if let Ok((_, Answer::Sat(other))) = solver.check(&only, &symbols) {
                    let other: BTreeMap<String, String> = other.into_iter().collect();
                    if let Some(at) = pick(&other).filter(|at| fails[*at].0.x) {
                        found = Some((at, other));
                    }
                }
            }
            match found {
                Some((at, other)) => {
                    index = at;
                    model = other;
                }
                None => {
                    return Err(Fail::Claimed("the claim can fail for an input that depends on the result of a call or on a value that the check does not model (no exact input)".into()));
                }
            }
        }
        // A small input if one exists.
        let numbers: Vec<(String, u32)> = self
            .decls
            .inputs
            .iter()
            .flat_map(|(_, value, _)| flatten(value))
            .filter_map(|(sort, term)| {
                let w: u32 = sort.strip_prefix("(_ BitVec ")?.trim_end_matches(')').parse().ok()?;
                (w > 8 && !term.s.ends_with(".d|")).then_some((term.s, w))
            })
            .collect();
        let short = |most: usize, bound: u128| -> Vec<String> {
            let mut list = Vec::new();
            for (symbol, e) in &self.decls.seqs {
                list.push(format!("(<= (seq.len {symbol}) {most})"));
                if let SeqE::Bv(w) = e {
                    for at in 0..most {
                        list.push(format!("(=> (< {at} (seq.len {symbol})) (bvule (seq.nth {symbol} {at}) {}))", bv_lit(bound, *w).s));
                    }
                }
            }
            list
        };
        let selected = format!("(assert |f{index}|)\n");
        for (bound, most) in [(1u128, 0usize), (1, 2), (10, 3), (1000, 4)] {
            if numbers.is_empty() && self.decls.seqs.is_empty() {
                break;
            }
            let mut list: Vec<String> = numbers.iter().map(|(term, w)| format!("(bvule {term} {})", bv_lit(bound, *w).s)).collect();
            list.extend(short(most, bound));
            let narrow = format!("{text}{selected}(assert (and {} true))\n", list.join(" "));
            *queries += 1;
            if let Ok((_, Answer::Sat(found))) = solver.check(&narrow, &symbols) {
                model = found.into_iter().collect();
                break;
            }
        }
        let (_, world, out) = &fails[index];
        // The values of the calls on the failed path, with the inputs of the model.
        let mut wanted: Vec<(String, String)> = Vec::new();
        for (_, shown) in &world.shown {
            if let Shown::Value(value, _) = shown {
                for (sort, term) in flatten(value) {
                    if lit_bv(&term).is_none() && lit_bool(&term).is_none() && !wanted.iter().any(|(_, known)| *known == term.s) {
                        wanted.push((sort, term.s));
                    }
                }
            }
        }
        let mut values = model.clone();
        if !wanted.is_empty() {
            let mut fixed = String::new();
            for line in &self.decls.lines {
                fixed.push_str(line);
                fixed.push('\n');
            }
            for line in &self.decls.assume {
                fixed.push_str(&format!("(assert {line})\n"));
            }
            for symbol in &symbols {
                if let Some(value) = model.get(symbol)
                    && symbol.starts_with("|in ")
                    && !value.contains("!val!")
                {
                    fixed.push_str(&format!("(assert (= {symbol} {value}))\n"));
                }
            }
            let mut request = Vec::new();
            for (at, (sort, term)) in wanted.iter().enumerate() {
                fixed.push_str(&format!("(define-fun |w{at}| () {sort} {term})\n"));
                request.push(format!("|w{at}|"));
            }
            *queries += 1;
            if let Ok((_, Answer::Sat(found))) = solver.check(&fixed, &request) {
                let found: BTreeMap<String, String> = found.into_iter().collect();
                for (at, (_, term)) in wanted.iter().enumerate() {
                    if let Some(value) = found.get(&format!("|w{at}|")) {
                        values.insert(term.clone(), value.clone());
                    }
                }
            }
        }
        let shown_input = |value: &V, ty: &Ty| -> String {
            match (value, ty.get()) {
                (V::Ptr(loc), Some(IrTy::Ref(_, inner))) if loc.path.is_empty() => format!("&{}", world.slots.get(loc.slot).map(|target| value_text(target, Some(&ty.at(*inner)), &values)).unwrap_or_else(|| "?".into())),
                _ => value_text(value, Some(ty), &values),
            }
        };
        let input = self.decls.inputs.iter().map(|(name, value, ty)| format!("{name} = {}", shown_input(value, ty))).collect::<Vec<_>>().join(", ");
        let input = if input.is_empty() { "the input of the claim".to_string() } else { input };
        let mut parts: Vec<String> = world
            .shown
            .iter()
            .map(|(call, shown)| match shown {
                Shown::Value(value, ty) => format!("`{call}` returns {}", value_text(value, Some(ty), &values)),
                Shown::Text(text) => format!("`{call}` {text}"),
            })
            .collect();
        parts.dedup();
        let head = match out {
            Out::Val(_) => "the body is false".to_string(),
            Out::Panic { kind, at, .. } => format!("`{at}` panics ({}) outside `panics`", kind.trim_start_matches("call ")),
            Out::Abort(at) => format!("`{at}` aborts"),
        };
        let detail = if parts.is_empty() { head } else { format!("{head}: {}", parts.join("; ")) };
        Ok(ClaimVerdict::False { input, detail })
    }

    // -----------------------------------------------------------------------------------------
    // The expressions.

    /// Applies `then` to each outcome with a value. A panic and an abort stay as they are.
    fn bind(&mut self, branches: Branches, mut then: impl FnMut(&mut Self, World, Val) -> Result<Branches, Fail>) -> Result<Branches, Fail> {
        let mut out = Vec::new();
        for (world, outcome) in branches {
            match outcome {
                Out::Val(value) => out.extend(then(self, world, value)?),
                other => out.push((world, other)),
            }
            if out.len() > MAX_CLAIM_PATHS {
                return Err(Fail::Claimed(format!("more than {MAX_CLAIM_PATHS} paths")));
            }
        }
        Ok(out)
    }

    /// A copy of the world with one more condition, or `None` if the condition is false.
    fn with(world: &World, condition: &T) -> Option<World> {
        match lit_bool(condition) {
            Some(false) => None,
            Some(true) => Some(world.clone()),
            None => {
                let mut next = world.clone();
                next.pc.push(condition.clone());
                if lit_bool(&and(&next.pc)) == Some(false) {
                    return None;
                }
                Some(next)
            }
        }
    }

    fn val(v: V, ty: Ty, ver: Option<Version>) -> Out {
        Out::Val(Val { v, ty, ver })
    }

    fn eval(&mut self, e: &Expr, world: World, want: Option<&Ty>) -> Result<Branches, Fail> {
        if past_deadline() {
            return Err(Fail::Claimed(DEADLINE_CAUSE.into()));
        }
        match &e.kind {
            ExprKind::Lit(lit) => {
                let value = self.literal(e, lit, want, false)?;
                Ok(vec![(world, Out::Val(value))])
            }
            ExprKind::Path(path) => self.path_value(e, path, world, want),
            ExprKind::Unary(op, inner) => self.unary(e, *op, inner, world, want),
            ExprKind::Binary(op, left, right) => self.binary(e, *op, left, right, world, want),
            ExprKind::Call(path, args) => self.call(e, path, args, world, want),
            ExprKind::Method(receiver, name, args) => {
                let branches = self.eval(receiver, world, None)?;
                self.bind(branches, |ev, world, value| ev.method(e, value, name, args, world))
            }
            ExprKind::Field(inner, name) => {
                let branches = self.eval(inner, world, None)?;
                self.bind(branches, |ev, world, value| {
                    let (world, value) = ev.deref_all(world, value);
                    let found = ev.field(e, &value, name)?;
                    Ok(vec![(world, Out::Val(found))])
                })
            }
            ExprKind::Index(base, index) => {
                let branches = self.eval(base, world, None)?;
                self.bind(branches, |ev, world, base_value| {
                    let usize_ty = ev.usize_ty();
                    let branches = ev.eval(index, world, Some(&usize_ty))?;
                    let base_value = base_value.clone();
                    ev.bind(branches, |ev, world, at| {
                        let (world, seq) = ev.deref_all(world, base_value.clone());
                        let (V::Seq(sq), Some(IrTy::Seq { e: elem, .. })) = (&seq.v, seq.ty.get().cloned()) else {
                            return Err(fail(&e.place, format!("an index needs a sequence (a `Vec`, a slice, or an array of scalars), not `{}`", ty_text(&seq.ty))));
                        };
                        let V::Bv(i, 64) = &at.v else { return Err(fail(&e.place, format!("an index must be a `usize`, not `{}`", ty_text(&at.ty)))) };
                        let inside = sq.has(i);
                        let mut out = Vec::new();
                        if let Some(next) = Self::with(&world, &not(&inside)) {
                            out.push((next, Out::Panic { kind: "index out of bounds".into(), args: Vec::new(), at: e.text.clone() }));
                        }
                        if let Some(next) = Self::with(&world, &inside) {
                            out.push((next, Self::val(sq.at(i), seq.ty.at(elem), seq.ver)));
                        }
                        Ok(out)
                    })
                })
            }
            ExprKind::If(condition, then, other) => {
                let bool_ty = self.bool_ty();
                let branches = self.eval(condition, world, Some(&bool_ty))?;
                self.bind(branches, |ev, world, value| {
                    let V::Bool(c) = &value.v else { return Err(fail(&condition.place, format!("the condition of `if` must be a `bool`, not `{}`", ty_text(&value.ty)))) };
                    let mut out = Vec::new();
                    if let Some(next) = Self::with(&world, c) {
                        out.extend(ev.eval(then, next, want)?);
                    }
                    if let Some(next) = Self::with(&world, &not(c)) {
                        match other {
                            Some(other) => out.extend(ev.eval(other, next, want)?),
                            None => {
                                let unit = ev.add(IrTy::Unit);
                                out.push((next, Self::val(V::Unit, unit, None)));
                            }
                        }
                    }
                    Ok(out)
                })
            }
            ExprKind::Block(stmts, tail) => self.block(stmts, tail.as_deref(), world, want, 0),
            ExprKind::Closure(_) => Err(Fail::Claimed(format!("a closure outside the argument of `panics`, `panics_with`, `returns`, or `same_outcome` (`{}`)", e.text))),
            ExprKind::Ref(true, _) => Err(Fail::Claimed(format!("a `&mut` reference (`{}`): the claims of this batch read values only", e.text))),
            ExprKind::Ref(false, inner) => {
                let inner_want = want.and_then(|want| match want.get() {
                    Some(IrTy::Ref(_, id)) => Some(want.at(*id)),
                    _ => None,
                });
                let branches = self.eval(inner, world, inner_want.as_ref())?;
                self.bind(branches, |ev, mut world, value| {
                    world.slots.push(value.v.clone());
                    world.types.push(Some(value.ty.clone()));
                    let ty = match (&inner_want, want) {
                        (Some(inner), Some(want)) if same_type(inner, &value.ty, 0, &|text: &str| text.to_string()) => want.clone(),
                        _ => ev.ref_to(&value.ty),
                    };
                    let target = world.slots.len() - 1;
                    Ok(vec![(world, Self::val(V::Ptr(Loc { slot: target, path: Vec::new() }), ty, value.ver))])
                })
            }
            ExprKind::Tuple(items) => {
                let wants: Vec<Option<Ty>> = match want.and_then(|want| want.get().cloned()) {
                    Some(IrTy::Tuple(ids)) if ids.len() == items.len() => ids.iter().map(|id| want.map(|want| want.at(*id))).collect(),
                    _ => vec![None; items.len()],
                };
                if items.is_empty() {
                    let unit = self.add(IrTy::Unit);
                    return Ok(vec![(world, Self::val(V::Unit, unit, None))]);
                }
                self.sequence(items, &wants, world, &mut |ev, world, values| {
                    let ids: Vec<u32> = values.iter().map(|value| ev.import(&value.ty)).collect();
                    let ver = values.iter().find_map(|value| value.ver);
                    let ty = ev.add(IrTy::Tuple(ids));
                    Ok(vec![(world, Self::val(V::Tup(values.into_iter().map(|value| value.v).collect()), ty, ver))])
                })
            }
            ExprKind::Array(items) => {
                let elem_want = want.and_then(|want| match want.get() {
                    Some(IrTy::Seq { e, .. }) => Some(want.at(*e)),
                    _ => None,
                });
                let wants = vec![elem_want.clone(); items.len()];
                let want = want.cloned();
                self.sequence(items, &wants, world, &mut |ev, world, values| {
                    let elem = match values.first() {
                        Some(first) => first.ty.clone(),
                        None => elem_want.clone().ok_or_else(|| Fail::Claimed(format!("an empty array with no known type (`{}`)", e.text)))?,
                    };
                    let Some(kind) = SeqE::of(&elem) else { return Err(Fail::Claimed(format!("an array of `{}`: the model has sequences of integers, `char`, and `bool` only", ty_text(&elem)))) };
                    let mut seq = Sq::empty(kind);
                    for value in &values {
                        let term = kind.term(&value.v).map_err(|Stop(cause)| Fail::Claimed(cause))?;
                        seq = seq.then(&Sq::unit(kind, &term));
                    }
                    seq.n = bv_lit(values.len() as u128, 64);
                    // The type: the wanted sequence type (a slice or a `Vec`), else an array.
                    let ty = match &want {
                        Some(want) if matches!(want.get(), Some(IrTy::Seq { .. })) => want.clone(),
                        _ => {
                            let e_id = ev.import(&elem);
                            ev.add(IrTy::Seq { e: e_id, n: Some(values.len() as u64), k: "a".into() })
                        }
                    };
                    Ok(vec![(world, Self::val(V::Seq(seq), ty, None))])
                })
            }
            ExprKind::Cast(inner, ty) => {
                let (target, _) = self.resolve_type(ty)?;
                let branches = self.eval(inner, world, None)?;
                self.bind(branches, |_, world, value| {
                    let from_signed = int_of(&value.ty).is_some_and(|(_, s)| s);
                    let found = cast(value.v.clone(), target.get(), from_signed).map_err(|Stop(cause)| Fail::Claimed(format!("{cause} (`{}`)", e.text)))?;
                    Ok(vec![(world, Self::val(found, target.clone(), None))])
                })
            }
            ExprKind::Struct(path, fields) => self.struct_value(e, path, fields, world),
            ExprKind::Other(kind) => Err(Fail::Claimed(format!("{kind} is outside the language of this batch (`{}` at {})", short(&e.text), e.place))),
        }
    }

    /// Evaluates a list of expressions in order, then calls `then` with the values.
    fn sequence(&mut self, items: &[Expr], wants: &[Option<Ty>], world: World, then: &mut dyn FnMut(&mut Self, World, Vec<Val>) -> Result<Branches, Fail>) -> Result<Branches, Fail> {
        let mut partial: Vec<(World, Vec<Val>)> = vec![(world, Vec::new())];
        let mut done: Branches = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let mut next = Vec::new();
            for (world, values) in partial {
                for (world, out) in self.eval(item, world, wants.get(index).and_then(|want| want.as_ref()))? {
                    match out {
                        Out::Val(value) => {
                            let mut values = values.clone();
                            values.push(value);
                            next.push((world, values));
                        }
                        other => done.push((world, other)),
                    }
                }
            }
            if next.len() + done.len() > MAX_CLAIM_PATHS {
                return Err(Fail::Claimed(format!("more than {MAX_CLAIM_PATHS} paths")));
            }
            partial = next;
        }
        for (world, values) in partial {
            done.extend(then(self, world, values)?);
        }
        Ok(done)
    }

    fn block(&mut self, stmts: &[Stmt], tail: Option<&Expr>, world: World, want: Option<&Ty>, at: usize) -> Result<Branches, Fail> {
        let depth = world.env.len();
        let restore = |mut branches: Branches| {
            for (world, _) in &mut branches {
                world.env.truncate(depth);
            }
            branches
        };
        let Some(stmt) = stmts.get(at) else {
            return Ok(restore(match tail {
                Some(tail) => self.eval(tail, world, want)?,
                None => {
                    let unit = self.add(IrTy::Unit);
                    vec![(world, Self::val(V::Unit, unit, None))]
                }
            }));
        };
        let branches = match stmt {
            Stmt::Let(name, ty, value) => {
                let declared = match ty {
                    Some(ty) => Some(self.resolve_type(ty)?.0),
                    None => None,
                };
                let found = self.eval(value, world, declared.as_ref())?;
                let name = name.clone();
                self.bind(found, |ev, mut world, value| {
                    world.env.push((name.clone(), value));
                    ev.block(stmts, tail, world, want, at + 1)
                })?
            }
            Stmt::Expr(value) => {
                let found = self.eval(value, world, None)?;
                self.bind(found, |ev, world, _| ev.block(stmts, tail, world, want, at + 1))?
            }
        };
        Ok(restore(branches))
    }

    fn literal(&mut self, e: &Expr, lit: &Lit, want: Option<&Ty>, negative: bool) -> Result<Val, Fail> {
        match lit {
            Lit::Bool(value) => {
                let ty = self.bool_ty();
                Ok(Val { v: V::Bool(t(value.to_string())), ty, ver: None })
            }
            Lit::Char(value) => {
                let ty = self.add(IrTy::Char);
                Ok(Val { v: V::Bv(bv_lit(*value as u128, 32), 32), ty, ver: None })
            }
            Lit::Int(value, suffix) => {
                let ty = match suffix {
                    Some(suffix) => self.path_type(&IPath { version: None, segments: vec![suffix.clone()], generics: Vec::new(), place: e.place.clone() })?.0,
                    None => match want.filter(|want| int_of(want).is_some() && !matches!(want.get(), Some(IrTy::Char))) {
                        Some(want) => want.clone(),
                        None => self.add(IrTy::Int(32, true)),
                    },
                };
                let Some((w, signed)) = int_of(&ty).filter(|_| !matches!(ty.get(), Some(IrTy::Char))) else {
                    return Err(fail(&e.place, format!("the integer `{}` cannot have the type `{}`", e.text, ty_text(&ty))));
                };
                let limit: u128 = match (signed, negative) {
                    (false, false) => if w >= 128 { u128::MAX } else { (1u128 << w) - 1 },
                    (false, true) => 0,
                    (true, false) => (1u128 << (w - 1)) - 1,
                    (true, true) => 1u128 << (w - 1),
                };
                if *value > limit {
                    return Err(fail(&e.place, format!("the integer `{}{}` does not fit in the type `{}`", if negative { "-" } else { "" }, value, ty_text(&ty))));
                }
                let mask = if w >= 128 { u128::MAX } else { (1u128 << w) - 1 };
                let bits = if negative { value.wrapping_neg() & mask } else { *value };
                Ok(Val { v: V::Bv(bv_lit(bits, w), w), ty, ver: None })
            }
            Lit::Str(_) => Err(Fail::Claimed(format!("a text value (`{}`) outside `panics_with`", e.text))),
            Lit::Other(text) => Err(Fail::Claimed(format!("the literal `{text}` is outside the model (a float)"))),
        }
    }

    fn path_value(&mut self, e: &Expr, path: &IPath, world: World, want: Option<&Ty>) -> Result<Branches, Fail> {
        if path.version.is_none() && path.segments.len() == 1 {
            let name = &path.segments[0];
            if let Some((_, value)) = world.env.iter().rev().find(|(known, _)| known == name) {
                let value = value.clone();
                return Ok(vec![(world, Out::Val(value))]);
            }
            if name == "None" {
                let ty = want.filter(|want| variant_kind(want) == Some("Option")).cloned().ok_or_else(|| Fail::Claimed(format!("`None` with no known type (`{}`): compare it with a value of a known type", e.text)))?;
                let value = self.variant_value(&ty, 0, Vec::new())?;
                return Ok(vec![(world, Self::val(value, ty, None))]);
            }
            if self.file.relation(name).is_some() {
                return Err(fail(&path.place, format!("the relation `{name}` is a function: call it, `{name}(…)`")));
            }
            return Err(fail(&path.place, format!("no local value `{name}`; the values of a claim are its parameters and its `let` values")));
        }
        // A unit variant of an enum of a version: `after::shop::Kind::A`.
        if let Some((ty, index, ver)) = self.variant_of(path)? {
            let value = self.variant_value(&ty, index, Vec::new())?;
            return Ok(vec![(world, Self::val(value, ty, ver))]);
        }
        Err(Fail::Claimed(format!("the path `{}` is not a value of this batch (a constant or a static is outside the language of this batch)", path.text())))
    }

    /// The enum type and the variant index of a path `PATH::Variant` of a version.
    fn variant_of(&mut self, path: &IPath) -> Result<Option<(Ty, usize, Option<Version>)>, Fail> {
        if path.segments.len() < 2 {
            return Ok(None);
        }
        let type_path = IPath { segments: path.segments[..path.segments.len() - 1].to_vec(), generics: Vec::new(), ..path.clone() };
        let versions: Vec<Version> = match path.version {
            Some(version) => vec![version],
            None => vec![Version::After, Version::Before],
        };
        let variant = path.segments.last().cloned().unwrap_or_default();
        for version in versions {
            if let Some(ty) = self.named_type(version, &type_path.name())
                && let Some(IrTy::Adt { vn, e: true, .. }) = ty.get()
            {
                return match vn.iter().position(|name| *name == variant) {
                    Some(index) => Ok(Some((ty.clone(), index, Some(version)))),
                    None => Err(fail(&path.place, format!("`{}`: the {} `{}` has no variant `{variant}`; its variants: {}", path.text(), version.word(), type_path.name(), semilla_intent::quoted_list(vn)))),
                };
            }
        }
        Ok(None)
    }

    /// The value of a variant of an enum type with its fields (the other variants get a value
    /// with no meaning).
    fn variant_value(&mut self, ty: &Ty, index: usize, fields: Vec<V>) -> Result<V, Fail> {
        let Some(IrTy::Adt { e: true, v, d, dt, .. }) = ty.get() else { return Err(Fail::Claimed("a variant of a type that is not an enum".into())) };
        let (w, ds) = discriminants(&ty.ir, d, *dt);
        let mut values: Vec<Vec<V>> = v.iter().map(|list| list.iter().map(|item| filler(&ty.at(*item), 0)).collect()).collect();
        if let Some(slot) = values.get_mut(index) {
            if slot.len() != fields.len() {
                return Err(Fail::Claimed(format!("a variant with {} fields gets {} values", slot.len(), fields.len())));
            }
            *slot = fields;
        }
        Ok(V::Enum { d: bv_lit(ds.get(index).copied().unwrap_or(index as u128), w), w, ds, f: values })
    }

    /// Reads through each reference of a value (a field read and a method call auto-deref).
    fn deref_all(&self, world: World, mut value: Val) -> (World, Val) {
        for _ in 0..8 {
            match (&value.v, value.ty.get()) {
                (V::Ptr(loc), Some(IrTy::Ref(_, inner))) => {
                    let inner = value.ty.at(*inner);
                    let Some(target) = read_loc(&world, loc) else { break };
                    value = Val { v: target, ty: inner, ver: value.ver };
                }
                _ => break,
            }
        }
        (world, value)
    }

    fn field(&mut self, e: &Expr, value: &Val, name: &str) -> Result<Val, Fail> {
        let index_of = |names: &[String]| names.iter().position(|known| known == name);
        let (index, field_ty) = match value.ty.get() {
            Some(IrTy::Tuple(items)) => {
                let index = name.parse::<usize>().ok().filter(|index| *index < items.len()).ok_or_else(|| fail(&e.place, format!("the tuple `{}` has no field `{name}`", ty_text(&value.ty))))?;
                (index, value.ty.at(items[index]))
            }
            Some(IrTy::Adt { n, e: false, v, fs, .. }) => {
                let names = fs.first().cloned().unwrap_or_default();
                let fields = v.first().cloned().unwrap_or_default();
                if names.len() != fields.len() {
                    return Err(Fail::Claimed(format!("the records of `{n}` have no field names (a record of an earlier driver)")));
                }
                match index_of(&names) {
                    Some(index) => (index, value.ty.at(fields[index])),
                    None => {
                        let which = value.ver.map(|ver| format!("the {} ", ver.word())).unwrap_or_else(|| "the type ".into());
                        let path = value.ver.map(|ver| format!("{}::{n}.{name}", ver.prefix())).unwrap_or_else(|| format!("{n}.{name}"));
                        return Err(fail(&e.place, format!("`{path}`: {which}`{n}` has no field `{name}`; its fields: {}", if names.is_empty() { "none".to_string() } else { semilla_intent::quoted_list(&names) })));
                    }
                }
            }
            Some(IrTy::Adt { n, e: true, .. }) => return Err(Fail::Claimed(format!("a field of the enum `{n}` (`{}`): the language of this batch has no `match`", e.text))),
            Some(IrTy::Opaque { n, .. }) => return Err(Fail::Claimed(format!("a field of `{n}`, a type that the model does not encode (`{}`)", e.text))),
            _ => return Err(fail(&e.place, format!("the type `{}` has no field `{name}`", ty_text(&value.ty)))),
        };
        let V::Tup(items) = &value.v else { return Err(Fail::Claimed(format!("a field of a value that the model does not encode (`{}`)", e.text))) };
        let found = items.get(index).cloned().ok_or_else(|| Fail::Claimed(format!("a field with no value (`{}`)", e.text)))?;
        Ok(Val { v: found, ty: field_ty, ver: value.ver })
    }

    fn unary(&mut self, e: &Expr, op: UnOp, inner: &Expr, world: World, want: Option<&Ty>) -> Result<Branches, Fail> {
        if let (UnOp::Neg, ExprKind::Lit(lit)) = (op, &inner.kind) {
            let value = self.literal(inner, lit, want, true)?;
            return Ok(vec![(world, Out::Val(value))]);
        }
        let inner_want = if op == UnOp::Deref { None } else { want };
        let branches = self.eval(inner, world, inner_want)?;
        self.bind(branches, |ev, world, value| match op {
            UnOp::Deref => match (&value.v, value.ty.get()) {
                (V::Ptr(loc), Some(IrTy::Ref(_, id))) => {
                    let found = read_loc(&world, loc).ok_or_else(|| Fail::Claimed(format!("a reference to a value that the claim does not have (`{}`)", e.text)))?;
                    let ty = value.ty.at(*id);
                    Ok(vec![(world, Self::val(found, ty, value.ver))])
                }
                _ => Err(fail(&e.place, format!("`*` needs a reference, not `{}`", ty_text(&value.ty)))),
            },
            UnOp::Not => match &value.v {
                V::Bool(term) => Ok(vec![(world, Self::val(V::Bool(not(term)), value.ty.clone(), None))]),
                V::Bv(term, w) => Ok(vec![(world, Self::val(V::Bv(app("bvnot", &[term]), *w), value.ty.clone(), None))]),
                _ => Err(fail(&e.place, format!("`!` needs a `bool` or an integer, not `{}`", ty_text(&value.ty)))),
            },
            UnOp::Neg => {
                let (V::Bv(term, w), Some((_, true))) = (&value.v, int_of(&value.ty)) else {
                    return Err(fail(&e.place, format!("`-` needs a signed integer, not `{}`", ty_text(&value.ty))));
                };
                let min = bv_lit(1u128 << (w - 1), *w);
                let overflow = eq_t(term, &min);
                let mut out = Vec::new();
                if let Some(next) = Self::with(&world, &overflow) {
                    out.push((next, Out::Panic { kind: "overflow neg".into(), args: Vec::new(), at: e.text.clone() }));
                }
                if let Some(next) = Self::with(&world, &not(&overflow)) {
                    out.push((next, Self::val(V::Bv(app("bvneg", &[term]), *w), value.ty.clone(), None)));
                }
                let _ = ev;
                Ok(out)
            }
        })
    }

    fn binary(&mut self, e: &Expr, op: BinOp, left: &Expr, right: &Expr, world: World, want: Option<&Ty>) -> Result<Branches, Fail> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let bool_ty = self.bool_ty();
            let branches = self.eval(left, world, Some(&bool_ty))?;
            return self.bind(branches, |ev, world, value| {
                let V::Bool(c) = &value.v else { return Err(fail(&left.place, format!("`{}` needs `bool` values, not `{}`", op.text(), ty_text(&value.ty)))) };
                let mut out = Vec::new();
                // The right side runs only if the left side does not decide (Rust short-circuits).
                let (stop, go) = if op == BinOp::And { (not(c), c.clone()) } else { (c.clone(), not(c)) };
                if let Some(next) = Self::with(&world, &stop) {
                    out.push((next, Self::val(V::Bool(t((op == BinOp::Or).to_string())), value.ty.clone(), None)));
                }
                if let Some(next) = Self::with(&world, &go) {
                    let bool_ty = ev.bool_ty();
                    for (world, outcome) in ev.eval(right, next, Some(&bool_ty))? {
                        match outcome {
                            Out::Val(found) if !matches!(found.v, V::Bool(_)) => return Err(fail(&right.place, format!("`{}` needs `bool` values, not `{}`", op.text(), ty_text(&found.ty)))),
                            other => out.push((world, other)),
                        }
                    }
                }
                Ok(out)
            });
        }
        let compare = matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge);
        // A literal takes the type of the other side: the other side first.
        let literal_left = matches!(left.kind, ExprKind::Lit(_)) || matches!(&left.kind, ExprKind::Unary(UnOp::Neg, inner) if matches!(inner.kind, ExprKind::Lit(_)));
        let none_left = matches!(&left.kind, ExprKind::Path(path) if path.version.is_none() && path.segments == ["None"]);
        let (first, second, swapped) = if literal_left || none_left { (right, left, true) } else { (left, right, false) };
        let first_want = if compare { None } else { want };
        let branches = self.eval(first, world, first_want)?;
        self.bind(branches, |ev, world, a| {
            let want_b = if matches!(op, BinOp::Shl | BinOp::Shr) { None } else { Some(a.ty.clone()) };
            let branches = ev.eval(second, world, want_b.as_ref())?;
            let a = a.clone();
            ev.bind(branches, |ev, world, b| {
                let (x, y) = if swapped { (b, a.clone()) } else { (a.clone(), b) };
                ev.operate(e, op, x, y, world)
            })
        })
    }

    fn operate(&mut self, e: &Expr, op: BinOp, a: Val, b: Val, world: World) -> Result<Branches, Fail> {
        match op {
            BinOp::Eq | BinOp::Ne => {
                let branches = self.equal(e, a, b, world)?;
                if op == BinOp::Eq {
                    return Ok(branches);
                }
                self.bind(branches, |_, world, value| match &value.v {
                    V::Bool(term) => Ok(vec![(world, Self::val(V::Bool(not(term)), value.ty.clone(), None))]),
                    _ => Ok(vec![(world, Out::Val(value))]),
                })
            }
            _ => {
                let (Some((w, signed)), Some((wb, _))) = (int_of(&a.ty), int_of(&b.ty)) else {
                    if let (V::Bool(_), V::Bool(_), BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) = (&a.v, &b.v, op) {
                        let name = match op {
                            BinOp::BitAnd => "and",
                            BinOp::BitOr => "or",
                            _ => "xor",
                        };
                        let found = binary(name, &a.v, &b.v, false, None).map_err(|Stop(cause)| Fail::Claimed(cause))?;
                        return Ok(vec![(world, Self::val(found, a.ty.clone(), None))]);
                    }
                    return Err(fail(&e.place, format!("`{}` needs integers, not `{}` and `{}`", op.text(), ty_text(&a.ty), ty_text(&b.ty))));
                };
                let shift = matches!(op, BinOp::Shl | BinOp::Shr);
                if !shift && (w != wb || int_of(&a.ty) != int_of(&b.ty)) {
                    return Err(fail(&e.place, format!("the two sides of `{}` have the types `{}` and `{}`", op.text(), ty_text(&a.ty), ty_text(&b.ty))));
                }
                let ty = a.ty.clone();
                let name = match op {
                    BinOp::Add => "add",
                    BinOp::Sub => "sub",
                    BinOp::Mul => "mul",
                    BinOp::Div => "div",
                    BinOp::Rem => "rem",
                    BinOp::BitAnd => "and",
                    BinOp::BitOr => "or",
                    BinOp::BitXor => "xor",
                    BinOp::Shl => "shl",
                    BinOp::Shr => "shr",
                    BinOp::Lt => "lt",
                    BinOp::Le => "le",
                    BinOp::Gt => "gt",
                    BinOp::Ge => "ge",
                    BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or => unreachable!(),
                };
                if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                    let found = binary(name, &a.v, &b.v, signed, None).map_err(|Stop(cause)| Fail::Claimed(cause))?;
                    let bool_ty = self.bool_ty();
                    return Ok(vec![(world, Self::val(found, bool_ty, None))]);
                }
                if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
                    let found = binary(name, &a.v, &b.v, signed, None).map_err(|Stop(cause)| Fail::Claimed(cause))?;
                    return Ok(vec![(world, Self::val(found, ty, None))]);
                }
                // [R67.3-3] An operation of the claim has the checks of a debug build: an
                // overflow, a division by zero, and a shift past the width panic.
                let (V::Bv(x, _), V::Bv(y, _)) = (&a.v, &b.v) else { unreachable!() };
                let (value, fails, kind) = match op {
                    BinOp::Div | BinOp::Rem => {
                        let zero = eq_t(y, &bv_lit(0, w));
                        let overflow = if signed { and(&[eq_t(x, &bv_lit(1u128 << (w - 1), w)), eq_t(y, &bv_lit(if w >= 128 { u128::MAX } else { (1u128 << w) - 1 }, w))]) } else { t("false") };
                        let found = binary(name, &a.v, &b.v, signed, None).map_err(|Stop(cause)| Fail::Claimed(cause))?;
                        let mut out = Vec::new();
                        if let Some(next) = Self::with(&world, &zero) {
                            out.push((next, Out::Panic { kind: if op == BinOp::Div { "division by zero".into() } else { "remainder by zero".into() }, args: Vec::new(), at: e.text.clone() }));
                        }
                        let rest = not(&zero);
                        if let Some(next) = Self::with(&world, &and(&[rest.clone(), overflow.clone()])) {
                            out.push((next, Out::Panic { kind: format!("overflow {name}"), args: Vec::new(), at: e.text.clone() }));
                        }
                        if let Some(next) = Self::with(&world, &and(&[rest, not(&overflow)])) {
                            out.push((next, Self::val(found, ty, None)));
                        }
                        return Ok(out);
                    }
                    _ => {
                        let checked_value = checked(name, &a.v, &b.v, signed).map_err(|Stop(cause)| Fail::Claimed(cause))?;
                        let V::Tup(parts) = checked_value else { unreachable!() };
                        let V::Bool(overflow) = &parts[1] else { unreachable!() };
                        (parts[0].clone(), overflow.clone(), format!("overflow {name}"))
                    }
                };
                let mut out = Vec::new();
                if let Some(next) = Self::with(&world, &fails) {
                    out.push((next, Out::Panic { kind, args: Vec::new(), at: e.text.clone() }));
                }
                if let Some(next) = Self::with(&world, &not(&fails)) {
                    out.push((next, Self::val(value, ty, None)));
                }
                Ok(out)
            }
        }
    }

    /// `a == b` (design 67, R67.3-4).
    fn equal(&mut self, e: &Expr, a: Val, b: Val, world: World) -> Result<Branches, Fail> {
        // `&a == &b` compares the values behind the references.
        let (world, a) = self.deref_all(world, a);
        let (world, b) = self.deref_all(world, b);
        let bool_ty = self.bool_ty();
        let rename_old = self.old.rename;
        let same = |x: &Val, y: &Val| match (x.ver, y.ver) {
            (Some(Version::Before), Some(Version::After)) | (Some(Version::Before), None) => same_type(&x.ty, &y.ty, 0, rename_old),
            (Some(Version::After), Some(Version::Before)) | (None, Some(Version::Before)) => same_type(&y.ty, &x.ty, 0, rename_old),
            _ => same_type(&x.ty, &y.ty, 0, &|text: &str| text.to_string()),
        };
        if !same(&a, &b) {
            return Err(fail(&e.place, format!("the two sides of `==` have the types `{}` and `{}`", ty_text(&a.ty), ty_text(&b.ty))));
        }
        match self.value_compare(&a.ty, 0) {
            Compare::Values => {
                let d = differ(&a.v, &b.v);
                Ok(vec![(world, Self::val(V::Bool(not(&d)), bool_ty, None))])
            }
            Compare::Workspace(name) => {
                // Two values of one version: the `PartialEq::eq` of that version. Values of the
                // two versions have two different Rust types, so no `PartialEq` exists: the
                // claim compares the fields (Decided by Claude).
                if a.ver.is_some() && b.ver.is_some() && a.ver != b.ver {
                    let d = differ(&a.v, &b.v);
                    return Ok(vec![(world, Self::val(V::Bool(not(&d)), bool_ty, None))]);
                }
                let version = a.ver.or(b.ver).unwrap_or(Version::After);
                let tree = self.tree(version);
                let base = name.split('<').next().unwrap_or(&name).to_string();
                let method = tree.names.iter().find(|known| known.starts_with(&format!("<{base} as ")) && known.contains("PartialEq") && known.ends_with(">::eq")).cloned();
                let Some(method) = method else {
                    return Err(Fail::Claimed(format!("`==` on `{name}`: the {} version has no body of its `PartialEq::eq`", version.word())));
                };
                let Some(form) = (tree.workspace)(&method) else { return Err(Fail::Claimed(format!("`==` on `{name}`: no form of `{method}`"))) };
                let mut world = world;
                let mut args = Vec::new();
                for value in [a, b] {
                    world.slots.push(value.v.clone());
                    world.types.push(Some(value.ty.clone()));
                    let ty = self.ref_to(&value.ty);
                    args.push(Val { v: V::Ptr(Loc { slot: world.slots.len() - 1, path: Vec::new() }), ty, ver: value.ver });
                }
                self.run_call(world, version, &method, form, args, &e.text)
            }
            Compare::Outside(cause) => Err(Fail::Claimed(format!("`==` on `{}`: {cause}", ty_text(&a.ty)))),
        }
    }

    /// How `==` compares two values of a type.
    fn value_compare(&self, ty: &Ty, depth: usize) -> Compare {
        if depth > 6 {
            return Compare::Outside("a type that is too deep".into());
        }
        match ty.get() {
            Some(IrTy::Bool | IrTy::Char | IrTy::Int(..) | IrTy::Unit | IrTy::Seq { .. }) => Compare::Values,
            Some(IrTy::Tuple(items)) => {
                for item in items {
                    match self.value_compare(&ty.at(*item), depth + 1) {
                        Compare::Values => {}
                        Compare::Workspace(name) => return Compare::Outside(format!("a tuple with the type `{name}` of the workspace in it")),
                        other => return other,
                    }
                }
                Compare::Values
            }
            Some(IrTy::Adt { n, v, .. }) if variant_kind(ty).is_some() => {
                for item in v.iter().flatten() {
                    match self.value_compare(&ty.at(*item), depth + 1) {
                        Compare::Values => {}
                        Compare::Workspace(name) => return Compare::Outside(format!("`{n}` with the type `{name}` of the workspace in it")),
                        other => return other,
                    }
                }
                Compare::Values
            }
            Some(IrTy::Adt { n, .. }) if !std_path(ty) => Compare::Workspace(n.clone()),
            Some(IrTy::Ref(..)) => Compare::Outside("a reference inside a value".into()),
            _ => Compare::Outside("a type whose `PartialEq` the model does not encode (a float, a type of the standard library)".into()),
        }
    }

    fn method(&mut self, e: &Expr, receiver: Val, name: &str, args: &[Expr], world: World) -> Result<Branches, Fail> {
        let (world, value) = self.deref_all(world, receiver.clone());
        // The methods of the modeled types of the prelude.
        let modeled = semilla_intent::prelude::METHODS.iter().any(|(known, _)| *known == name);
        if modeled && args.is_empty() {
            if let (V::Seq(sq), "len" | "is_empty") = (&value.v, name) {
                return Ok(vec![if name == "len" {
                    let ty = self.usize_ty();
                    (world, Self::val(V::Bv(sq.n.clone(), 64), ty, None))
                } else {
                    let ty = self.bool_ty();
                    (world, Self::val(V::Bool(sq.is_empty()), ty, None))
                }]);
            }
            if let (Some(kind), V::Enum { d, w, ds, f }) = (variant_kind(&value.ty), &value.v) {
                let is = |index: usize| eq_t(d, &bv_lit(ds.get(index).copied().unwrap_or(index as u128), *w));
                let good = if kind == "Option" { 1 } else { 0 };
                let bool_ty = self.bool_ty();
                match name {
                    "is_some" | "is_ok" if (name == "is_some") == (kind == "Option") => return Ok(vec![(world, Self::val(V::Bool(is(good)), bool_ty, None))]),
                    "is_none" | "is_err" if (name == "is_none") == (kind == "Option") => return Ok(vec![(world, Self::val(V::Bool(not(&is(good))), bool_ty, None))]),
                    "unwrap" => {
                        let Some(IrTy::Adt { v, .. }) = value.ty.get() else { unreachable!() };
                        let field_ty = value.ty.at(v[good][0]);
                        let mut out = Vec::new();
                        if let Some(next) = Self::with(&world, &not(&is(good))) {
                            out.push((next, Out::Panic { kind: format!("`unwrap` on {}", if kind == "Option" { "`None`" } else { "`Err`" }), args: Vec::new(), at: e.text.clone() }));
                        }
                        if let Some(next) = Self::with(&world, &is(good)) {
                            out.push((next, Self::val(f[good][0].clone(), field_ty, value.ver)));
                        }
                        return Ok(out);
                    }
                    _ => {}
                }
            }
        }
        // A method of a type of a version: `TYPE::name` of that version.
        if let Some(IrTy::Adt { n, .. } | IrTy::Opaque { n, .. }) = value.ty.get().cloned()
            && !std_path(&value.ty)
        {
            let version = value.ver.unwrap_or(Version::After);
            let tree = self.tree(version);
            let base = n.split('<').next().unwrap_or(&n).to_string();
            let inherent = format!("{base}::{name}");
            // The record name of the method, also a loose one (R67.2-4): `Type::<T>::name`,
            // `m::<impl Type>::name`, or `<Type as Trait>::name`.
            let found = match semilla_intent::loose_match(&inherent, tree.names.iter()) {
                Ok(found) => found,
                Err(list) => return Err(fail(&e.place, format!("`{}::{inherent}` names two or more methods; call one of them with its path: {}", version.prefix(), semilla_intent::quoted_list(&list)))),
            };
            let Some(method) = found else {
                let methods: Vec<String> = tree.names.iter().filter(|known| known.starts_with(&format!("{base}::")) || known.starts_with(&format!("<{base} as "))).cloned().collect();
                return Err(fail(&e.place, format!("`{}::{base}::{name}`: the {} `{base}` has no method `{name}`{}", version.prefix(), version.word(), candidate_text(&semilla_intent::candidates(&inherent, &methods, 6)))));
            };
            let Some(form) = (tree.workspace)(&method) else { return Err(Fail::Claimed(format!("the method `{method}` has no form"))) };
            let first = form.l.get(1).map(|id| Ty { ir: form.clone(), id: *id });
            let mut world = world;
            let receiver_value = match first.as_ref().and_then(|ty| ty.get()) {
                Some(IrTy::Ref(true, _)) => return Err(Fail::Claimed(format!("the method `{method}` takes `&mut self`: the claims of this batch read values only"))),
                Some(IrTy::Ref(false, _)) => {
                    world.slots.push(value.v.clone());
                    world.types.push(Some(value.ty.clone()));
                    Val { v: V::Ptr(Loc { slot: world.slots.len() - 1, path: Vec::new() }), ty: first.clone().unwrap(), ver: value.ver }
                }
                _ => value.clone(),
            };
            return self.call_form(e, world, version, &method, form, vec![receiver_value], args);
        }
        if modeled {
            return Err(fail(&e.place, format!("the method `{name}` needs a receiver of the types {}, not `{}`", semilla_intent::prelude::METHODS.iter().find(|(known, _)| *known == name).map(|(_, types)| *types).unwrap_or(""), ty_text(&value.ty))));
        }
        Err(Fail::Claimed(format!("the method `{name}` of `{}` is outside the model of this batch", ty_text(&value.ty))))
    }

    fn call(&mut self, e: &Expr, path: &IPath, args: &[Expr], world: World, want: Option<&Ty>) -> Result<Branches, Fail> {
        let name = path.name();
        if path.version.is_none() && path.segments.len() == 1 {
            // A prelude function.
            if let Some((_, count, batch)) = semilla_intent::prelude::FUNCTIONS.iter().find(|(known, ..)| *known == name) {
                if *batch != "10r1" {
                    return Err(Fail::Claimed(format!("`{name}` is a part of batch {batch}")));
                }
                if args.len() != *count {
                    return Err(fail(&e.place, format!("`{name}` takes {count} argument(s); the claim gives {}", args.len())));
                }
                return self.prelude(e, &name, args, world);
            }
            // `Some(x)`, `Ok(x)`, `Err(x)`.
            if let Some((_, kind)) = semilla_intent::prelude::VARIANTS.iter().find(|(known, _)| *known == name) {
                return self.std_variant(e, &name, kind, args, world, want);
            }
            if let Some(func) = self.file.relation(&name) {
                return self.relation(e, func, args, world);
            }
        }
        match path.version {
            Some(version) => {
                let tree = self.tree(version);
                if let Some((found, form)) = self.function(version, &name, &path.place)? {
                    return self.call_form(e, world, version, &found, form, Vec::new(), args);
                }
                if let Some((ty, index, ver)) = self.variant_of(path)? {
                    return self.variant_call(e, ty, index, ver, args, world);
                }
                let candidates = semilla_intent::candidates(&name, tree.names, 6);
                Err(fail(&path.place, format!("`{}`: the {} version has no function `{name}`{}", path.text(), version.word(), candidate_text(&candidates))))
            }
            None => {
                let (a, b) = (self.function(Version::Before, &name, &path.place)?, self.function(Version::After, &name, &path.place)?);
                match (a, b) {
                    (Some((old_name, _)), Some((found, form))) if !(self.step.changed)(&found) && !(self.step.changed)(&old_name) => self.call_form(e, world, Version::After, &found, form, Vec::new(), args),
                    (Some(_), Some(_)) => Err(fail(&path.place, format!("`{name}` has a different body in the two versions: write `before::{name}` or `after::{name}`"))),
                    (Some(_), None) => Err(fail(&path.place, format!("only the old version has the function `{name}`: write `before::{name}`"))),
                    (None, Some(_)) => Err(fail(&path.place, format!("only the new version has the function `{name}`: write `after::{name}`"))),
                    (None, None) => {
                        if let Some((ty, index, ver)) = self.variant_of(path)? {
                            return self.variant_call(e, ty, index, ver, args, world);
                        }
                        if self.is_workspace_crate(&path.segments[0]) || path.segments.len() == 1 {
                            let mut names: Vec<String> = self.new.names.to_vec();
                            names.extend(self.file.items.iter().filter_map(|item| match item {
                                semilla_intent::Item::Relation(func) => Some(func.name.clone()),
                                _ => None,
                            }));
                            let candidates = semilla_intent::candidates(&name, &names, 6);
                            return Err(fail(&path.place, format!("no function `{name}` in the two versions, in the relations of the file, or in the prelude{}", candidate_text(&candidates))));
                        }
                        Err(Fail::Claimed(format!("the call of `{name}` (a function outside the workspace) is outside the model of this batch")))
                    }
                }
            }
        }
    }

    /// The body of a version that a path names: the name itself, else the one body with the
    /// same plain form (R67.2-4).
    fn function(&self, version: Version, name: &str, place: &semilla_intent::Place) -> Result<Option<(String, Arc<Ir>)>, Fail> {
        let tree = self.tree(version);
        if let Some(form) = (tree.workspace)(name) {
            return Ok(Some((name.to_string(), form)));
        }
        match semilla_intent::loose_match(name, tree.names.iter()) {
            Ok(Some(found)) => Ok((tree.workspace)(&found).map(|form| (found, form))),
            Ok(None) => Ok(None),
            Err(list) => Err(fail(place, format!("`{}::{name}` names two or more bodies of the {} version; write one of them: {}", version.prefix(), version.word(), semilla_intent::quoted_list(&list)))),
        }
    }

    /// Evaluates the arguments with the parameter types of a form, then runs the form.
    #[allow(clippy::too_many_arguments)]
    fn call_form(&mut self, e: &Expr, world: World, version: Version, name: &str, form: Arc<Ir>, first: Vec<Val>, args: &[Expr]) -> Result<Branches, Fail> {
        if let Some(cause) = &form.u {
            return Err(Fail::Claimed(format!("`{name}` has no form ({cause})")));
        }
        let count = first.len() + args.len();
        if form.a != count {
            let path = format!("{}::{name}", version.prefix());
            return Err(fail(&e.place, format!("`{path}` takes {} argument(s); the claim gives {count}", form.a)));
        }
        let wants: Vec<Option<Ty>> = (first.len()..form.a).map(|index| form.l.get(index + 1).map(|id| Ty { ir: form.clone(), id: *id })).collect();
        let place = e.place.clone();
        let text = e.text.clone();
        self.sequence(args, &wants, world, &mut |ev, world, values| {
            let mut all = first.clone();
            all.extend(values);
            // The type of each argument must be the type of the parameter.
            for (index, value) in all.iter().enumerate() {
                let param = Ty { ir: form.clone(), id: form.l[index + 1] };
                let rename_old = ev.old.rename;
                let fits = match (value.ver, version) {
                    (Some(Version::Before), Version::After) => same_type(&value.ty, &param, 0, rename_old),
                    (Some(Version::After), Version::Before) => same_type(&param, &value.ty, 0, rename_old),
                    _ => same_type(&value.ty, &param, 0, &|text: &str| text.to_string()),
                };
                if !fits {
                    return Err(Fail::Error(Error { place: place.clone(), message: format!("the argument {} of `{}::{name}` has the type `{}`; the parameter has the type `{}`", index + 1, version.prefix(), ty_text(&value.ty), ty_text(&param)) }));
                }
                if let Some(IrTy::Ref(true, _)) = param.get() {
                    return Err(Fail::Claimed(format!("the parameter {} of `{name}` is `&mut`: the claims of this batch read values only", index + 1)));
                }
            }
            ev.run_call(world, version, name, form.clone(), all, &text)
        })
    }

    /// Runs the body of a version with the arguments (R67.3-5).
    fn run_call(&mut self, world: World, version: Version, name: &str, form: Arc<Ir>, args: Vec<Val>, text: &str) -> Result<Branches, Fail> {
        self.reads.insert((version, name.to_string()));
        let ret = Ty { ir: form.clone(), id: form.l[0] };
        if self.options.dry {
            let mut world = world;
            let symbol = format!("dry{}", self.fresh);
            self.fresh += 1;
            let mut muts = Vec::new();
            let value = input(&mut self.decls, &ret, &symbol, &mut world.slots, &mut world.types, &mut muts, 0).unwrap_or(V::Undef);
            return Ok(vec![(world, Self::val(value, ret, Some(version)))]);
        }
        let tree = self.tree(version);
        let side = Side { ir: form.clone(), workspace: tree.workspace, std: tree.std, rename: tree.rename };
        let inputs: Vec<V> = args.iter().map(|value| value.v.clone()).collect();
        let mut state = start_state(&side, &inputs, &world.slots, &world.types);
        state.pc = world.pc.clone();
        state.frames[0].name = name.to_string();
        let paths = run_state(&side, self.step, self.inline_all, &mut self.decls, state, &[], version == Version::Before, LoopMode::Plain);
        self.inlined.extend(paths.inlined.iter().cloned());
        if let Some((_, cause)) = paths.stops.first() {
            if cause == DEADLINE_CAUSE {
                return Err(Fail::Claimed(DEADLINE_CAUSE.into()));
            }
            return Err(Fail::Claimed(format!("`{text}`: {cause}")));
        }
        let mut out = Vec::new();
        for path in paths.outs {
            let mut next = world.clone();
            next.pc = vec![path.pc.clone()];
            next.effects.extend(path.effects.iter().cloned());
            match path.exit {
                Exit::Return(value) => {
                    if refers_beyond(&value, world.slots.len()) {
                        return Err(Fail::Claimed(format!("`{text}` returns a reference to a value of the callee: the model of this batch does not keep the memory of a call")));
                    }
                    next.shown.push((text.to_string(), Shown::Value(value.clone(), ret.clone())));
                    out.push((next, Self::val(value, ret.clone(), Some(version))));
                }
                Exit::Panic { kind, args } => {
                    next.shown.push((text.to_string(), Shown::Text(format!("panics ({})", kind.trim_start_matches("call ")))));
                    out.push((next, Out::Panic { kind, args, at: text.to_string() }));
                }
                Exit::Abort => out.push((next, Out::Abort(text.to_string()))),
                Exit::Cut => return Err(Fail::Claimed("a loop head".into())),
            }
        }
        Ok(out)
    }

    fn variant_call(&mut self, e: &Expr, ty: Ty, index: usize, ver: Option<Version>, args: &[Expr], world: World) -> Result<Branches, Fail> {
        let Some(IrTy::Adt { v, .. }) = ty.get().cloned() else { unreachable!() };
        let fields = v.get(index).cloned().unwrap_or_default();
        if fields.len() != args.len() {
            return Err(fail(&e.place, format!("the variant takes {} value(s); the claim gives {}", fields.len(), args.len())));
        }
        let wants: Vec<Option<Ty>> = fields.iter().map(|id| Some(ty.at(*id))).collect();
        self.sequence(args, &wants, world, &mut |ev, world, values| {
            for (value, want) in values.iter().zip(&wants) {
                if let Some(want) = want
                    && !same_type(&value.ty, want, 0, &|text: &str| text.to_string())
                {
                    return Err(fail(&e.place, format!("a value of the type `{}` where the variant has the type `{}`", ty_text(&value.ty), ty_text(want))));
                }
            }
            let value = ev.variant_value(&ty, index, values.into_iter().map(|value| value.v).collect())?;
            Ok(vec![(world, Self::val(value, ty.clone(), ver))])
        })
    }

    fn std_variant(&mut self, e: &Expr, name: &str, kind: &str, args: &[Expr], world: World, want: Option<&Ty>) -> Result<Branches, Fail> {
        if args.len() != 1 {
            return Err(fail(&e.place, format!("`{name}` takes one value")));
        }
        let index = match name {
            "Some" | "Err" => 1,
            _ => 0,
        };
        let known = want.filter(|want| variant_kind(want) == Some(kind)).cloned();
        let field_want = known.as_ref().and_then(|ty| match ty.get() {
            Some(IrTy::Adt { v, .. }) => v.get(index).and_then(|list| list.first()).map(|id| ty.at(*id)),
            _ => None,
        });
        let branches = self.eval(&args[0], world, field_want.as_ref())?;
        self.bind(branches, |ev, world, value| {
            let ty = match &known {
                Some(ty) => ty.clone(),
                None if kind == "Option" => {
                    let text = format!("std::option::Option<{}>", ty_text(&value.ty));
                    let item = ev.import(&value.ty);
                    let dt = ev.add(IrTy::Int(64, true)).id;
                    ev.add(IrTy::Adt { n: text, v: vec![Vec::new(), vec![item]], vn: vec!["None".into(), "Some".into()], d: vec!["0".into(), "1".into()], dt: Some(dt), e: true, fs: vec![Vec::new(), vec!["0".into()]] })
                }
                None => return Err(Fail::Claimed(format!("`{name}(…)` with no known type (`{}`): compare it with a value of a known type", e.text))),
            };
            let v = ev.variant_value(&ty, index, vec![value.v.clone()])?;
            Ok(vec![(world, Self::val(v, ty, value.ver))])
        })
    }

    fn struct_value(&mut self, e: &Expr, path: &IPath, fields: &[(String, Expr)], world: World) -> Result<Branches, Fail> {
        let (ty, ver) = self.path_type(path)?;
        let Some(IrTy::Adt { n, e: false, v, fs, .. }) = ty.get().cloned() else { return Err(fail(&e.place, format!("`{}` is not a struct", path.text()))) };
        let names = fs.first().cloned().unwrap_or_default();
        let types = v.first().cloned().unwrap_or_default();
        if names.len() != types.len() {
            return Err(Fail::Claimed(format!("the records of `{n}` have no field names")));
        }
        let mut ordered: Vec<&Expr> = Vec::new();
        for name in &names {
            match fields.iter().find(|(known, _)| known == name) {
                Some((_, value)) => ordered.push(value),
                None => return Err(fail(&e.place, format!("the struct expression has no value for the field `{name}` of `{n}`"))),
            }
        }
        if let Some((extra, _)) = fields.iter().find(|(known, _)| !names.contains(known)) {
            return Err(fail(&e.place, format!("`{n}` has no field `{extra}`; its fields: {}", semilla_intent::quoted_list(&names))));
        }
        let items: Vec<Expr> = ordered.into_iter().cloned().collect();
        let wants: Vec<Option<Ty>> = types.iter().map(|id| Some(ty.at(*id))).collect();
        self.sequence(&items, &wants, world, &mut |_, world, values| Ok(vec![(world, Self::val(V::Tup(values.into_iter().map(|value| value.v).collect()), ty.clone(), ver))]))
    }

    fn relation(&mut self, e: &Expr, func: &Func, args: &[Expr], world: World) -> Result<Branches, Fail> {
        if self.relations >= MAX_RELATIONS {
            return Err(Fail::Claimed(format!("a relation calls relations deeper than {MAX_RELATIONS} levels (`{}`)", func.name)));
        }
        if func.params.len() != args.len() {
            return Err(fail(&e.place, format!("the relation `{}` takes {} argument(s); the claim gives {}", func.name, func.params.len(), args.len())));
        }
        let Some(body) = func.body.clone() else { return Err(fail(&e.place, format!("the relation `{}` has no body", func.name))) };
        let mut wants = Vec::new();
        for (_, ty, _) in &func.params {
            wants.push(Some(self.resolve_type(ty)?.0));
        }
        let ret = match &func.ret {
            Some(ty) => Some(self.resolve_type(ty)?.0),
            None => None,
        };
        let names: Vec<String> = func.params.iter().map(|(name, ..)| name.clone()).collect();
        let outer_env = world.env.clone();
        self.sequence(args, &wants, world, &mut |ev, mut world, values| {
            for (value, want) in values.iter().zip(&wants) {
                if let Some(want) = want
                    && !same_type(&value.ty, want, 0, &|text: &str| text.to_string())
                    && !same_type(&value.ty, want, 0, ev.old.rename)
                {
                    return Err(fail(&e.place, format!("an argument of the type `{}` where the relation `{}` takes `{}`", ty_text(&value.ty), func.name, ty_text(want))));
                }
            }
            world.env = names.iter().cloned().zip(values).collect();
            ev.relations += 1;
            let found = ev.eval(&body, world, ret.as_ref());
            ev.relations -= 1;
            let mut found = found?;
            for (world, _) in &mut found {
                world.env = outer_env.clone();
            }
            Ok(found)
        })
    }

    fn prelude(&mut self, e: &Expr, name: &str, args: &[Expr], world: World) -> Result<Branches, Fail> {
        let closure = |arg: &Expr| -> Result<Expr, Fail> {
            match &arg.kind {
                ExprKind::Closure(body) => Ok((**body).clone()),
                _ => Err(fail(&arg.place, format!("`{name}` takes a closure with no parameter: `{name}(|| …)`"))),
            }
        };
        let bool_ty = self.bool_ty();
        match name {
            "panics" | "returns" | "panics_with" => {
                let body = closure(&args[0])?;
                if name == "panics_with" && !matches!(&args[1].kind, ExprKind::Lit(Lit::Str(_))) {
                    return Err(fail(&args[1].place, "`panics_with` takes a text literal as its second argument"));
                }
                let branches = self.eval(&body, world.clone(), None)?;
                let mut out = Vec::new();
                for (mut next, outcome) in branches {
                    // The values of the closure stay in the closure.
                    next.env = world.env.clone();
                    let value = match (&outcome, name) {
                        (Out::Abort(_), _) => {
                            out.push((next, outcome));
                            continue;
                        }
                        (Out::Val(_), "returns") | (Out::Panic { .. }, "panics") => t("true"),
                        (Out::Val(_), _) | (Out::Panic { .. }, "returns") => t("false"),
                        // [R67.3-6] The text of a panic message is outside the model: an unknown
                        // value, so the claim is never proved on a path that panics and never
                        // false there.
                        (Out::Panic { .. }, _) => {
                            let symbol = format!("|pm{}|", self.fresh);
                            self.fresh += 1;
                            self.decls.declare(&symbol, format!("(declare-const {symbol} Bool)"));
                            tx(symbol, false)
                        }
                    };
                    out.push((next, Self::val(V::Bool(value), bool_ty.clone(), None)));
                }
                Ok(out)
            }
            "same_outcome" => {
                let (a, b) = (closure(&args[0])?, closure(&args[1])?);
                let start = world.effects.len();
                let first = self.eval(&a, world.clone(), None)?;
                let mut out = Vec::new();
                for (left_world, left) in first {
                    let mut from = world.clone();
                    from.pc = left_world.pc.clone();
                    from.slots = left_world.slots.clone();
                    from.types = left_world.types.clone();
                    from.shown = left_world.shown.clone();
                    let want = match &left {
                        Out::Val(value) => Some(value.ty.clone()),
                        _ => None,
                    };
                    for (right_world, right) in self.eval(&b, from, want.as_ref())? {
                        let as_path = |outcome: &Out, effects: &[Effect]| -> PathOut {
                            let exit = match outcome {
                                Out::Val(value) => Exit::Return(value.v.clone()),
                                Out::Panic { kind, args, .. } => Exit::Panic { kind: kind.clone(), args: if self.options.except_panic_message { Vec::new() } else { args.clone() } },
                                Out::Abort(_) => Exit::Abort,
                            };
                            PathOut { pc: t("true"), effects: effects.to_vec(), calls: Vec::new(), exit, after: Vec::new(), cut: None }
                        };
                        if let (Out::Val(x), Out::Val(y)) = (&left, &right)
                            && !same_type(&x.ty, &y.ty, 0, &|text: &str| text.to_string())
                            && !same_type(&x.ty, &y.ty, 0, self.old.rename)
                        {
                            return Err(fail(&e.place, format!("the two closures of `same_outcome` have the types `{}` and `{}`", ty_text(&x.ty), ty_text(&y.ty))));
                        }
                        let d = outcome_differs(&as_path(&left, &left_world.effects[start.min(left_world.effects.len())..]), &as_path(&right, &right_world.effects[start.min(right_world.effects.len())..]));
                        let mut next = right_world.clone();
                        next.env = world.env.clone();
                        next.effects = world.effects.clone();
                        out.push((next, Self::val(V::Bool(not(&d)), bool_ty.clone(), None)));
                    }
                    if out.len() > MAX_CLAIM_PATHS {
                        return Err(Fail::Claimed(format!("more than {MAX_CLAIM_PATHS} paths")));
                    }
                }
                Ok(out)
            }
            _ => Err(Fail::Claimed(format!("`{name}` is outside the language of this batch"))),
        }
    }
}

/// How `==` compares two values of a type.
enum Compare {
    /// The values (the `PartialEq` of the type compares the values).
    Values,
    /// A type of the workspace: its `PartialEq::eq`.
    Workspace(String),
    /// Outside the model: the cause.
    Outside(String),
}

/// The value at a place of the memory of a world.
fn read_loc(world: &World, loc: &Loc) -> Option<V> {
    let mut value = world.slots.get(loc.slot)?.clone();
    let mut variant = None;
    for step in &loc.path {
        value = match (step, value) {
            (Step2::Field(index), V::Tup(items)) => items.get(*index)?.clone(),
            (Step2::Field(index), V::Enum { f, .. }) => f.get(variant.unwrap_or(0))?.get(*index)?.clone(),
            (Step2::Variant(index), found) => {
                variant = Some(*index);
                found
            }
            (Step2::Elem(at), V::Seq(sq)) => sq.at(at),
            _ => return None,
        };
    }
    Some(value)
}

/// The text of a list of candidates, after a message.
fn candidate_text(candidates: &[String]) -> String {
    if candidates.is_empty() { String::new() } else { format!("; candidates: {}", semilla_intent::quoted_list(candidates)) }
}

/// The first 60 characters of a text, on one line.
fn short(text: &str) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 60 { format!("{}…", line.chars().take(60).collect::<String>()) } else { line }
}

thread_local! {
    /// True while the equivalence check of an empty `#[same]` claim with `except =
    /// [panic_message]` runs: two panics of one kind are the same panic (R67.3-7).
    pub(super) static IGNORE_PANIC_ARGS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with the rule "two panics of one kind are the same panic" (`except =
/// [panic_message]`).
pub fn with_panic_messages_ignored<R>(f: impl FnOnce() -> R) -> R {
    let outer = IGNORE_PANIC_ARGS.with(|cell| cell.replace(true));
    let found = f();
    IGNORE_PANIC_ARGS.with(|cell| cell.set(outer));
    found
}
