//! The intent language of Semilla (design 67): the syntax tree of the claims, the parser from
//! Rust syntax, and the prelude.
//!
//! An intent file is a Rust source file outside the production code (`.semilla/intent/*.rs`).
//! Each claim is a plain Rust function with an attribute. The body of the function is a Rust
//! expression that must be true for each value of the parameters. In the body, `before::PATH`
//! names an item of the old version, and `after::PATH` names an item of the new version.
//! Semilla never compiles or runs an intent file: it parses it with the syntax tree of
//! rust-analyzer (`ra_ap_syntax`), and it checks each claim with its own model of the two
//! versions.
//!
//! This crate has no compiler data. The references of a claim resolve against the compiler
//! data of each version in `semilla-smt` (`mir::claim`), after the build check of the step.

pub mod heads;
pub mod parse;
pub mod prelude;

pub use parse::parse_file;

use serde::{Deserialize, Serialize};

/// A place in an intent file: the file, the line, and the column (each from 1).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Place {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// The version that a path names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Version {
    /// `before::PATH`: the tree before the step.
    Before,
    /// `after::PATH`: the tree after the step.
    After,
}

impl Version {
    /// The prefix of a path of this version.
    pub fn prefix(self) -> &'static str {
        match self {
            Version::Before => "before",
            Version::After => "after",
        }
    }

    /// The adjective of the version in a message: "old" or "new".
    pub fn word(self) -> &'static str {
        match self {
            Version::Before => "old",
            Version::After => "new",
        }
    }
}

/// A path as the claim writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct IPath {
    /// The version of the prefix `before::` or `after::`. No prefix: `None`.
    pub version: Option<Version>,
    /// The segments after the prefix, without generic arguments.
    pub segments: Vec<String>,
    /// The generic arguments of the last segment (`Vec<u32>`, `Option<u8>`).
    pub generics: Vec<Type>,
    pub place: Place,
}

impl IPath {
    /// The path without the prefix: `shop::Cart`.
    pub fn name(&self) -> String {
        self.segments.join("::")
    }

    /// The path as written, with the prefix.
    pub fn text(&self) -> String {
        match self.version {
            Some(version) => format!("{}::{}", version.prefix(), self.name()),
            None => self.name(),
        }
    }
}

/// A type as the claim writes it.
#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// A path type: `u32`, `Vec<u8>`, `Option<u32>`, `before::shop::Cart`.
    Path(IPath),
    /// `&T` or `&mut T`.
    Ref(bool, Box<Type>),
    /// `()` and `(A, B)`.
    Tuple(Vec<Type>),
    /// `[T]`.
    Slice(Box<Type>),
    /// `[T; N]`.
    Array(Box<Type>, u64),
    /// Another type (a macro type such as `loop_state!`, `impl Trait`): its text.
    Other(String),
}

impl Type {
    /// The text of the type, as Rust writes it.
    pub fn text(&self) -> String {
        match self {
            Type::Path(path) if path.generics.is_empty() => path.text(),
            Type::Path(path) => format!("{}<{}>", path.text(), path.generics.iter().map(Type::text).collect::<Vec<_>>().join(", ")),
            Type::Ref(true, inner) => format!("&mut {}", inner.text()),
            Type::Ref(false, inner) => format!("&{}", inner.text()),
            Type::Tuple(items) if items.len() == 1 => format!("({},)", items[0].text()),
            Type::Tuple(items) => format!("({})", items.iter().map(Type::text).collect::<Vec<_>>().join(", ")),
            Type::Slice(inner) => format!("[{}]", inner.text()),
            Type::Array(inner, n) => format!("[{}; {n}]", inner.text()),
            Type::Other(text) => text.clone(),
        }
    }
}

/// A literal.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    /// An integer: the value and the suffix (`5u8` gives `u8`).
    Int(u128, Option<String>),
    Bool(bool),
    Str(String),
    Char(char),
    /// A literal that the language does not have (a float): its text.
    Other(String),
}

/// A unary operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    Deref,
}

/// A binary operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    /// The operator as Rust writes it.
    pub fn text(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
        }
    }
}

/// An expression with its place and its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub place: Place,
    /// The source text of the expression (for a message).
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Lit(Lit),
    Path(IPath),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// A call of a path: a function of a version, a variant, a relation, or a prelude function.
    Call(IPath, Vec<Expr>),
    /// A method call: the receiver, the method name, and the arguments.
    Method(Box<Expr>, String, Vec<Expr>),
    /// A field read: a name or a tuple index (`x.0`).
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    /// A block: its statements and its tail expression.
    Block(Vec<Stmt>, Option<Box<Expr>>),
    /// A closure with no parameter: `|| e`. Only an argument of a prelude function.
    Closure(Box<Expr>),
    /// `&e` (true: `&mut e`).
    Ref(bool, Box<Expr>),
    Tuple(Vec<Expr>),
    Cast(Box<Expr>, Type),
    /// A struct expression: the path and each field.
    Struct(IPath, Vec<(String, Expr)>),
    Array(Vec<Expr>),
    /// A construct outside the language of this batch: its kind (`match`, `loop`, a macro).
    Other(String),
}

/// A statement of a block.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    /// `let NAME[: TYPE] = EXPR;`
    Let(String, Option<Type>, Expr),
    Expr(Expr),
}

/// The kind of a claim: the attribute of the function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// `#[same(ITEM)]`: an empty body is the full compare of the item; else a property.
    Same,
    /// `#[change(ITEM)]`: a property of an intended change.
    Change,
    /// `#[different(ITEM)]`: a sure difference, a property with no parameters.
    Different,
    /// `#[rename(A, B)]`: the form `--same 'rename A B'`.
    Rename,
    /// `#[moved(A, B)]`: the form `--same 'move A B'`.
    Moved,
    /// `#[rewrite(A, B)]`: the form `--same 'rewrite A => B'`.
    Rewrite,
    /// [R94.6-1] `#[extract(before::BODY, into = after::CALLEE)]`: the form `--same 'extract
    /// BODY CALLEE'`.
    Extract,
    /// [R94.6-1] `#[inlined(after::BODY, from = before::CALLEE)]`: the form `--same 'inline
    /// BODY CALLEE'`. The name is not `inline`: that is an attribute of the language.
    Inlined,
    /// A claim of a later batch (`pair_loop`, `lemma`, `contract`, `invariant`, `relation`,
    /// `assume`): Semilla shows it and checks nothing yet.
    Later,
}

impl ClaimKind {
    pub fn name(self) -> &'static str {
        match self {
            ClaimKind::Same => "same",
            ClaimKind::Change => "change",
            ClaimKind::Different => "different",
            ClaimKind::Rename => "rename",
            ClaimKind::Moved => "moved",
            ClaimKind::Rewrite => "rewrite",
            ClaimKind::Extract => "extract",
            ClaimKind::Inlined => "inlined",
            ClaimKind::Later => "later",
        }
    }
}

/// A target of a claim attribute: a path, or a text (a pattern of `rewrite`).
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Path(IPath),
    Text(String, Place),
}

impl Target {
    /// The text of the target.
    pub fn text(&self) -> String {
        match self {
            Target::Path(path) => path.text(),
            Target::Text(text, _) => text.clone(),
        }
    }

    /// The text without a version prefix.
    pub fn name(&self) -> String {
        match self {
            Target::Path(path) => path.name(),
            Target::Text(text, _) => text.clone(),
        }
    }
}

/// The options of a claim attribute.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// `cfg = "…"`: the claim is about the matching configurations only.
    pub cfg: Option<String>,
    /// `reason = "…"`: shown, not checked.
    pub reason: Option<String>,
    /// `except = [NAME, …]`: the observations that the claim ignores.
    pub except: Vec<String>,
}

/// A function of an intent file.
#[derive(Clone, Debug, PartialEq)]
pub struct Func {
    pub name: String,
    /// Each parameter: the name, the type, and the place.
    pub params: Vec<(String, Type, Place)>,
    pub ret: Option<Type>,
    /// The body. An empty block gives `None`.
    pub body: Option<Expr>,
    pub place: Place,
}

/// One claim: one attribute target of one function.
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub kind: ClaimKind,
    /// The attribute name as written (`same`, `pair_loop`).
    pub attribute: String,
    /// The targets: one item for `same`, `change`, and `different`; two for `rename`, `moved`,
    /// and `rewrite`.
    pub targets: Vec<Target>,
    pub options: Options,
    pub func: Func,
    /// The place of the attribute.
    pub place: Place,
}

impl Claim {
    /// The short text of the claim: `#[same(shop::parse)] fn parse(s: String)`.
    pub fn text(&self) -> String {
        let targets = self.targets.iter().map(Target::text).collect::<Vec<_>>().join(", ");
        let params = self.func.params.iter().map(|(name, ty, _)| format!("{name}: {}", ty.text())).collect::<Vec<_>>().join(", ");
        format!("#[{}({targets})] fn {}({params})", self.attribute, self.func.name)
    }

    /// True if the claim has a property body (a body that is not empty).
    pub fn has_body(&self) -> bool {
        self.func.body.is_some()
    }
}

/// One item of an intent file.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// `intent!("TEXT")`: shown to the reviewer, not checked.
    Intent(String, Place),
    Claim(Claim),
    /// A function with no attribute: a named relation that the claims can call.
    Relation(Func),
}

/// A parsed intent file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct File {
    pub path: String,
    pub items: Vec<Item>,
}

impl File {
    /// The claims of the file, in order.
    pub fn claims(&self) -> impl Iterator<Item = &Claim> {
        self.items.iter().filter_map(|item| match item {
            Item::Claim(claim) => Some(claim),
            _ => None,
        })
    }

    /// The relation with a name.
    pub fn relation(&self, name: &str) -> Option<&Func> {
        self.items.iter().find_map(|item| match item {
            Item::Relation(func) if func.name == name => Some(func),
            _ => None,
        })
    }
}

/// An error of an intent file: the place and what the parser or the check expected.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Error {
    pub place: Place,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.place, self.message)
    }
}

/// A stored intent file: the path as the agent gave it, and the text (design 67, part D).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub path: String,
    pub text: String,
}

/// The names in a list, each in back quotes: "`a`, `b`".
pub fn quoted_list(names: &[String]) -> String {
    names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
}

/// The candidates for a name that does not resolve: each name of the list that ends with the
/// same last segment, else the names with the smallest edit distance (at most `most`).
pub fn candidates(wanted: &str, names: &[String], most: usize) -> Vec<String> {
    let last = wanted.rsplit("::").next().unwrap_or(wanted);
    let mut found: Vec<String> = names.iter().filter(|name| name.rsplit("::").next() == Some(last) && name.as_str() != wanted).cloned().collect();
    if found.is_empty() {
        let mut scored: Vec<(usize, &String)> = names.iter().map(|name| (distance(name.rsplit("::").next().unwrap_or(name), last), name)).filter(|(score, _)| *score <= last.len().max(3) / 2 + 1).collect();
        scored.sort();
        // The nearest names first: the list keeps the `most` nearest ones.
        scored.dedup_by(|a, b| a.1 == b.1);
        found = scored.into_iter().take(most).map(|(_, name)| name.clone()).collect();
    }
    found.sort();
    found.dedup();
    found.truncate(most);
    found
}

/// The edit distance of two texts (Levenshtein, by characters).
/// The edit distance of two names (for the candidates of an error).
pub fn distance_of(a: &str, b: &str) -> usize {
    distance(a, b)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let next = if x == *y { previous } else { 1 + previous.min(row[j]).min(row[j + 1]) };
            previous = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// The plain form of a body name for a loose match (R67.2-4): no generic arguments, and the
/// type in place of `<Type as Trait>`: `<shop::Cart<T> as Clone>::clone` gives
/// `shop::Cart::clone`, and `shop::Bag::<T>::len` gives `shop::Bag::len`.
pub fn plain_name(name: &str) -> String {
    let name = name.trim();
    // A method of an impl block in a module: `m::<impl shop::Cart>::len` (or `<impl Trait for
    // shop::Cart>`) gives `shop::Cart::len`: the type has its own full path.
    if let Some(at) = name.find("<impl ") {
        let rest = &name[at + 6..];
        let mut depth = 1;
        for (end, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &rest[..end];
                        let ty = top_level_split(inner, " for ").map(|(_, ty)| ty).unwrap_or(inner);
                        return format!("{}{}", strip_generics(ty.trim()), strip_generics(&rest[end + 1..]));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(rest) = name.strip_prefix('<') {
        let mut depth = 1;
        for (at, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &rest[..at];
                        let base = top_level_split(inner, " as ").map(|(base, _)| base).unwrap_or(inner);
                        return format!("{}{}", strip_generics(base), strip_generics(&rest[at + 1..]));
                    }
                }
                _ => {}
            }
        }
    }
    strip_generics(name)
}

fn top_level_split<'a>(text: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 0i32;
    for (at, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth == 0 && text[at..].starts_with(needle) => return Some((&text[..at], &text[at + needle.len()..])),
            _ => {}
        }
    }
    None
}

fn strip_generics(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    while out.contains("::::") {
        out = out.replace("::::", "::");
    }
    out.trim_end_matches("::").to_string()
}

/// The body name of a list that a claim path names (R67.2-4): the name itself; else the one name
/// with the same plain form. `Err` gives the names with that plain form when there are two or
/// more.
pub fn loose_match<'a>(wanted: &str, names: impl IntoIterator<Item = &'a String>) -> Result<Option<String>, Vec<String>> {
    let plain = plain_name(wanted);
    let mut found: Vec<String> = Vec::new();
    for name in names {
        if name == wanted {
            return Ok(Some(name.clone()));
        }
        if plain_name(name) == plain {
            found.push(name.clone());
        }
    }
    found.sort();
    found.dedup();
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err(found),
    }
}

/// True if a `cfg` predicate (`debug_assertions`, `feature = "std"`, `all(…)`, `any(…)`,
/// `not(…)`) holds for a set of true atoms (`debug_assertions`, `feature="std"`). An error
/// names the part that the parser does not read.
pub fn cfg_holds(predicate: &str, atoms: &std::collections::BTreeSet<String>) -> Result<bool, String> {
    let tokens = cfg_tokens(predicate)?;
    let mut at = 0;
    let found = cfg_expr(&tokens, &mut at, atoms)?;
    if at != tokens.len() {
        return Err(format!("the `cfg` text `{predicate}` has more text after its predicate"));
    }
    Ok(found)
}

fn cfg_tokens(text: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {}
            '(' | ')' | ',' | '=' => tokens.push(c.to_string()),
            '"' => {
                let mut value = String::from('"');
                for next in chars.by_ref() {
                    value.push(next);
                    if next == '"' {
                        break;
                    }
                }
                tokens.push(value);
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut word = c.to_string();
                while let Some(next) = chars.peek().copied().filter(|next| next.is_alphanumeric() || *next == '_') {
                    word.push(next);
                    chars.next();
                }
                tokens.push(word);
            }
            other => return Err(format!("the `cfg` text has the character `{other}`")),
        }
    }
    Ok(tokens)
}

fn cfg_expr(tokens: &[String], at: &mut usize, atoms: &std::collections::BTreeSet<String>) -> Result<bool, String> {
    let word = tokens.get(*at).cloned().ok_or("the `cfg` text has no predicate")?;
    *at += 1;
    if matches!(word.as_str(), "all" | "any" | "not") && tokens.get(*at).map(String::as_str) == Some("(") {
        *at += 1;
        let mut values = Vec::new();
        while tokens.get(*at).map(String::as_str) != Some(")") {
            values.push(cfg_expr(tokens, at, atoms)?);
            if tokens.get(*at).map(String::as_str) == Some(",") {
                *at += 1;
            } else if tokens.get(*at).map(String::as_str) != Some(")") {
                return Err("the `cfg` text has no `)`".into());
            }
        }
        *at += 1;
        return match word.as_str() {
            "all" => Ok(values.iter().all(|value| *value)),
            "any" => Ok(values.iter().any(|value| *value)),
            _ if values.len() == 1 => Ok(!values[0]),
            _ => Err("`not(…)` takes one predicate".into()),
        };
    }
    if tokens.get(*at).map(String::as_str) == Some("=") {
        let value = tokens.get(*at + 1).cloned().ok_or("the `cfg` text has no value after `=`")?;
        *at += 2;
        return Ok(atoms.contains(&format!("{word}={value}")));
    }
    Ok(atoms.contains(&word))
}

/// [R77.3-2] The kind of a literal that the language has no value for, from the Rust syntax of
/// its text: "a byte string", "a byte", "a C string", "a raw text", or "a float".
pub fn literal_kind(text: &str) -> &'static str {
    if text.starts_with("br") || text.starts_with("b\"") {
        "a byte string"
    } else if text.starts_with("b'") {
        "a byte"
    } else if text.starts_with("c\"") || text.starts_with("cr") {
        "a C string"
    } else if text.starts_with('r') {
        "a raw text"
    } else if text.starts_with(|c: char| c.is_ascii_digit()) {
        "a float"
    } else {
        "a literal of another kind"
    }
}

/// The calls of the versions in an expression: the version and the name.
pub fn calls_of(e: &Expr, out: &mut Vec<(Version, String)>) {
    match &e.kind {
        ExprKind::Call(path, args) => {
            if let Some(version) = path.version {
                out.push((version, path.name()));
            }
            for arg in args {
                calls_of(arg, out);
            }
        }
        ExprKind::Unary(_, inner) | ExprKind::Field(inner, _) | ExprKind::Closure(inner) | ExprKind::Ref(_, inner) | ExprKind::Cast(inner, _) => calls_of(inner, out),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
            calls_of(a, out);
            calls_of(b, out);
        }
        ExprKind::Method(receiver, _, args) => {
            calls_of(receiver, out);
            for arg in args {
                calls_of(arg, out);
            }
        }
        ExprKind::If(c, a, b) => {
            calls_of(c, out);
            calls_of(a, out);
            if let Some(b) = b {
                calls_of(b, out);
            }
        }
        ExprKind::Block(stmts, tail) => {
            for stmt in stmts {
                match stmt {
                    Stmt::Let(_, _, value) | Stmt::Expr(value) => calls_of(value, out),
                }
            }
            if let Some(tail) = tail {
                calls_of(tail, out);
            }
        }
        ExprKind::Tuple(items) | ExprKind::Array(items) => {
            for item in items {
                calls_of(item, out);
            }
        }
        ExprKind::Struct(_, fields) => {
            for (_, value) in fields {
                calls_of(value, out);
            }
        }
        ExprKind::Lit(_) | ExprKind::Path(_) | ExprKind::Other(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // rule+ R77.3-2
    // rule- R77.3-2
    #[test]
    fn the_kind_of_a_literal_comes_from_its_syntax() {
        assert_eq!(literal_kind("b\"ab\""), "a byte string");
        assert_eq!(literal_kind("br#\"ab\"#"), "a byte string");
        assert_eq!(literal_kind("b'a'"), "a byte");
        assert_eq!(literal_kind("c\"ab\""), "a C string");
        // The negative case: a float stays a float.
        assert_eq!(literal_kind("1.5"), "a float");
        assert_eq!(literal_kind("2e3"), "a float");
    }

    #[test]
    fn a_cfg_predicate_reads_the_atoms_of_a_configuration() {
        let atoms: std::collections::BTreeSet<String> = ["debug_assertions".to_string(), "feature=\"std\"".to_string()].into_iter().collect();
        assert_eq!(cfg_holds("debug_assertions", &atoms), Ok(true));
        assert_eq!(cfg_holds("not(debug_assertions)", &atoms), Ok(false));
        assert_eq!(cfg_holds("all(feature = \"std\", not(test))", &atoms), Ok(true));
        assert_eq!(cfg_holds("any(feature = \"alloc\", test)", &atoms), Ok(false));
        assert!(cfg_holds("all(", &atoms).is_err());
    }

    // rule+ R67.2-4
    #[test]
    fn a_loose_path_names_the_one_body_with_its_plain_form() {
        assert_eq!(plain_name("<shop::Cart<T> as core::clone::Clone>::clone"), "shop::Cart::clone");
        assert_eq!(plain_name("hashbrown::raw::RawTable::<T, A>::reserve"), "hashbrown::raw::RawTable::reserve");
        assert_eq!(plain_name("uuid::parser::<impl uuid::Uuid>::parse_str"), "uuid::Uuid::parse_str");
        assert_eq!(plain_name("m::<impl serde::Deserialize<'de> for shop::Cart<T>>::deserialize"), "shop::Cart::deserialize");
        let names = vec!["shop::Bag::<T>::len".to_string(), "<shop::Bag<T> as core::clone::Clone>::clone".to_string()];
        assert_eq!(loose_match("shop::Bag::len", &names), Ok(Some("shop::Bag::<T>::len".to_string())));
        assert_eq!(loose_match("shop::Bag::clone", &names), Ok(Some("<shop::Bag<T> as core::clone::Clone>::clone".to_string())));
    }

    // rule- R67.2-4
    #[test]
    fn two_bodies_with_one_plain_form_are_an_error() {
        let names = vec!["<shop::Bag as core::clone::Clone>::clone".to_string(), "<shop::Bag as other::Clone>::clone".to_string(), "shop::Bag::len".to_string()];
        assert_eq!(loose_match("shop::Bag::clone", &names).unwrap_err().len(), 2);
        assert_eq!(loose_match("shop::Bag::size", &names), Ok(None));
    }

    #[test]
    fn a_candidate_has_the_same_last_segment_or_a_near_name() {
        let names = vec!["shop::Cart::total".to_string(), "shop::sum".to_string(), "depot::total".to_string()];
        assert_eq!(candidates("shop::total", &names, 5), vec!["depot::total".to_string(), "shop::Cart::total".to_string()]);
        assert_eq!(candidates("shop::summ", &names, 5), vec!["shop::sum".to_string()]);
        assert!(candidates("shop::xyzzyq", &names, 5).is_empty());
    }
}
