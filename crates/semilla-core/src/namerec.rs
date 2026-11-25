//! The name record of the compiler (batch 11b, design 70).
//!
//! The driver of Semilla writes one record for each compiled unit (R70.1-1): each definition of
//! the crate with its place, its effective visibility, its `unsafe` state, and its `cfg` state;
//! each name of the source with the definition that the compiler resolves; each `unsafe` block;
//! and each `cfg` and `cfg_attr` attribute that the compiler evaluated. [`NameIndex`] joins the
//! records of the units of one tree. The text and the syntax tree only find the place of an
//! edit: a byte range of a file joins the place to the data of the compiler.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// The record of one unit, in the form of the driver.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Record {
    #[serde(default)]
    pub f: Vec<String>,
    #[serde(default)]
    pub d: Vec<RecordDef>,
    #[serde(default)]
    pub p: Vec<[i64; 4]>,
    #[serde(default)]
    pub u: Vec<[i64; 3]>,
    #[serde(default)]
    pub c: Vec<RecordCfg>,
}

/// One definition of a record (see the driver, `names.rs`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct RecordDef {
    #[serde(default)]
    pub p: String,
    #[serde(default)]
    pub n: String,
    #[serde(default)]
    pub k: String,
    #[serde(default)]
    pub s: Option<[i64; 3]>,
    #[serde(default)]
    pub i: Option<[i64; 3]>,
    #[serde(default)]
    pub v: String,
    #[serde(default)]
    pub u: bool,
    #[serde(default)]
    pub c: bool,
    #[serde(default)]
    pub t: Option<String>,
    #[serde(default)]
    pub r: Option<usize>,
    #[serde(default)]
    pub o: bool,
    #[serde(default)]
    pub h: Vec<String>,
}

/// One `cfg` or `cfg_attr` attribute of a record.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct RecordCfg {
    pub r: [i64; 3],
    #[serde(default)]
    pub v: bool,
    #[serde(default)]
    pub k: String,
    #[serde(default)]
    pub t: String,
    #[serde(default)]
    pub s: String,
}

/// A definition of the index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Def {
    /// The def path of the compiler, with the crate name first.
    pub path: String,
    pub name: String,
    pub kind: String,
    /// The file and the byte range of the definition, and of its name.
    pub file: Option<String>,
    pub span: Option<(usize, usize)>,
    pub ident: Option<(usize, usize)>,
    /// True if a different crate can use the definition, in a unit of a library (R70.2-1 (b)).
    pub public: bool,
    /// True if a unit of a library has the definition.
    pub in_library: bool,
    pub unsafe_def: bool,
    pub under_cfg: bool,
    /// An impl of a trait: the trait reference with the self type.
    pub trait_ref: Option<String>,
    /// An impl of a trait: the path of the trait. An item of a trait impl: the path of the item
    /// of the trait.
    pub of: Option<String>,
    /// An item of a trait impl: the item of the trait has a default.
    pub of_default: bool,
    /// The last names of the attributes that the compiler keeps on the definition with no
    /// meaning of its own: a derive helper attribute, a tool attribute (R70.2-1 (f)).
    pub inert: BTreeSet<String>,
}

/// What a name of the source means, by the compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A local binding (a `let`, a parameter).
    Local,
    /// A definition: the index in [`NameIndex::defs`].
    Def(usize),
}

/// A `cfg` or a `cfg_attr` attribute that the compiler evaluated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cfg {
    pub start: usize,
    pub end: usize,
    pub value: bool,
    /// `cfg` or `cfg_attr`.
    pub kind: String,
    /// The text of the condition (`feature = "x"`, `test`).
    pub text: String,
    /// A false `cfg`: the name of the item that it removes.
    pub removes: String,
}

/// The records of the units of one tree, joined (design 70).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameIndex {
    pub defs: Vec<Def>,
    by_file: BTreeMap<String, Vec<usize>>,
    paths: BTreeMap<String, Vec<(usize, usize, Target)>>,
    unsafe_blocks: BTreeMap<String, Vec<(usize, usize)>>,
    cfgs: BTreeMap<String, Vec<Cfg>>,
    /// The names of the items that a false `cfg` removes.
    removed: BTreeSet<String>,
}

/// True for the target of a unit of a library: `<crate>.<kind>.<hash>`, a kind that is not
/// `test` and not `bin`.
fn library_unit(target: &str) -> bool {
    let kind = target.split('.').nth(1).unwrap_or("");
    !kind.is_empty() && kind != "test" && kind != "bin"
}

impl NameIndex {
    /// Joins the records of the units of one tree. `units` has the target of each unit
    /// (`<crate>.<kind>.<hash>`) and its record.
    pub fn new<'a>(units: impl IntoIterator<Item = (&'a str, &'a Record)>) -> NameIndex {
        let mut index = NameIndex::default();
        let mut known: BTreeMap<(String, String, Option<String>, Option<(usize, usize)>), usize> = BTreeMap::new();
        for (target, record) in units {
            let library = library_unit(target);
            let file = |index: i64| record.f.get(usize::try_from(index).ok()?).cloned();
            let place = |range: &Option<[i64; 3]>| range.and_then(|[f, start, end]| Some((file(f)?, (start.max(0) as usize, end.max(0) as usize))));
            let mut local: Vec<usize> = Vec::with_capacity(record.d.len());
            for def in &record.d {
                let at = place(&def.s);
                let key = (def.p.clone(), def.k.clone(), at.as_ref().map(|(f, _)| f.clone()), at.as_ref().map(|(_, r)| *r));
                let found = *known.entry(key).or_insert_with(|| {
                    index.defs.push(Def {
                        path: def.p.clone(),
                        name: def.n.clone(),
                        kind: def.k.clone(),
                        file: at.as_ref().map(|(f, _)| f.clone()),
                        span: at.as_ref().map(|(_, r)| *r),
                        ident: place(&def.i).map(|(_, r)| r),
                        trait_ref: def.t.clone(),
                        ..Def::default()
                    });
                    index.defs.len() - 1
                });
                let entry = &mut index.defs[found];
                entry.unsafe_def |= def.u;
                entry.under_cfg |= def.c;
                entry.of_default |= def.o;
                entry.inert.extend(def.h.iter().cloned());
                if library && def.s.is_some() {
                    entry.in_library = true;
                    entry.public |= !def.v.is_empty();
                }
                local.push(found);
            }
            for (at, def) in record.d.iter().enumerate() {
                if let Some(of) = def.r.and_then(|r| record.d.get(r)) {
                    let found = local[at];
                    index.defs[found].of.get_or_insert_with(|| of.p.clone());
                }
            }
            for [f, start, end, target] in &record.p {
                let Some(name) = file(*f) else { continue };
                let target = if *target < 0 { Target::Local } else { match local.get(*target as usize) { Some(found) => Target::Def(*found), None => continue } };
                index.paths.entry(name).or_default().push((*start as usize, *end as usize, target));
            }
            for [f, start, end] in &record.u {
                if let Some(name) = file(*f) {
                    index.unsafe_blocks.entry(name).or_default().push((*start as usize, *end as usize));
                }
            }
            for cfg in &record.c {
                let Some(name) = file(cfg.r[0]) else { continue };
                if !cfg.s.is_empty() {
                    index.removed.insert(cfg.s.clone());
                }
                index.cfgs.entry(name).or_default().push(Cfg { start: cfg.r[1] as usize, end: cfg.r[2] as usize, value: cfg.v, kind: cfg.k.clone(), text: cfg.t.clone(), removes: cfg.s.clone() });
            }
        }
        for (at, def) in index.defs.iter().enumerate() {
            if let Some(file) = &def.file {
                index.by_file.entry(file.clone()).or_default().push(at);
            }
        }
        for list in index.paths.values_mut() {
            list.sort_by_key(|(start, end, target)| (*start, *end, matches!(target, Target::Local)));
            list.dedup();
        }
        for list in index.unsafe_blocks.values_mut() {
            list.sort();
            list.dedup();
        }
        for list in index.cfgs.values_mut() {
            list.sort_by(|a, b| (a.start, a.end, &a.text, &a.removes).cmp(&(b.start, b.end, &b.text, &b.removes)));
            list.dedup();
        }
        index
    }

    /// The definitions with a place in a file.
    pub fn defs_in(&self, file: &str) -> impl Iterator<Item = &Def> {
        self.by_file.get(file).into_iter().flatten().map(|at| &self.defs[*at])
    }

    /// True if the compiler built code at the range: a definition that is not a module has a
    /// place that contains it. Code under a `cfg` that the configuration turns off, a macro
    /// definition, and a file that no unit compiles are not built.
    pub fn built(&self, file: &str, start: usize, end: usize) -> bool {
        self.defs_in(file).any(|def| def.kind != "module" && def.kind != "macro" && !def.kind.ends_with("macro") && def.span.is_some_and(|(s, e)| s <= start && end <= e))
    }

    /// The definition of an item of the syntax tree: the definition with the name whose name
    /// is in the range (the outermost one). An impl has no name: the outermost impl whose
    /// place starts in the range.
    pub fn def_of(&self, file: &str, start: usize, end: usize, name: &str, impl_item: bool) -> Option<&Def> {
        self.defs_in(file)
            .filter(|def| {
                if impl_item {
                    def.kind == "implementation" && def.span.is_some_and(|(s, _)| start <= s && s < end)
                } else {
                    def.name == name && def.ident.is_some_and(|(s, e)| start <= s && e <= end)
                }
            })
            .max_by_key(|def| def.span.map(|(s, e)| e - s).unwrap_or(0))
    }

    /// What the compiler resolves each name of the range to.
    pub fn targets_in(&self, file: &str, start: usize, end: usize) -> impl Iterator<Item = (usize, usize, Target)> + '_ {
        self.paths.get(file).into_iter().flatten().filter(move |(s, e, _)| start <= *s && *e <= end).copied()
    }

    /// True if an `unsafe` block that the author wrote meets the range, or if the range has
    /// the name of an `unsafe fn`, an `unsafe impl`, or an `unsafe trait` (R70.2-1 (a)).
    pub fn unsafe_in(&self, file: &str, start: usize, end: usize) -> bool {
        let meets = |(s, e): (usize, usize)| s < end.max(start + 1) && start < e;
        self.unsafe_blocks.get(file).into_iter().flatten().any(|block| meets(*block))
            || self.defs_in(file).any(|def| def.unsafe_def && def.ident.or(def.span.map(|(s, _)| (s, s + 1))).is_some_and(|(s, e)| start <= s && e <= end.max(s + 1) && s < end))
    }

    /// Each `cfg` and `cfg_attr` attribute that the compiler evaluated in the range.
    pub fn cfgs_in(&self, file: &str, start: usize, end: usize) -> impl Iterator<Item = &Cfg> {
        self.cfgs.get(file).into_iter().flatten().filter(move |cfg| start <= cfg.start && cfg.end <= end)
    }

    /// True if a false `cfg` removes an item with this name in this configuration.
    pub fn removes(&self, name: &str) -> bool {
        self.removed.contains(name)
    }
}

/// The byte offset of a line (from 1) and a column (a count of characters from 0) in a text.
/// `None` for a place outside the text.
pub fn offset(text: &str, line: usize, column: usize) -> Option<usize> {
    let mut start = 0;
    for _ in 1..line {
        start += text[start..].find('\n')? + 1;
    }
    let rest = &text[start..];
    let line_text = rest.split('\n').next().unwrap_or(rest);
    match line_text.char_indices().nth(column) {
        Some((at, _)) => Some(start + at),
        None if line_text.chars().count() == column => Some(start + line_text.len()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        serde_json::from_str(
            r#"{"f":["src/lib.rs"],
            "d":[{"p":"shop::f","n":"f","k":"function","s":[0,0,40],"i":[0,7,8],"v":"e"},
                 {"p":"shop::g","n":"g","k":"function","s":[0,41,80],"i":[0,51,52],"u":true},
                 {"p":"shop::m","n":"m","k":"module","s":[0,0,200]}],
            "p":[[0,20,21,-1],[0,25,26,0]],
            "u":[[0,10,30]],
            "c":[{"r":[0,90,100],"v":false,"k":"cfg","t":"unix","s":"h"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn the_index_joins_the_places_of_a_record() {
        let record = record();
        let index = NameIndex::new([("shop.lib.aaaa", &record)]);
        assert!(index.built("src/lib.rs", 10, 12));
        // A module does not make its range built code.
        assert!(!index.built("src/lib.rs", 85, 95));
        assert!(index.unsafe_in("src/lib.rs", 12, 14));
        assert!(!index.unsafe_in("src/lib.rs", 31, 40));
        // The name of an `unsafe fn` in the range.
        assert!(index.unsafe_in("src/lib.rs", 41, 60));
        assert_eq!(index.cfgs_in("src/lib.rs", 80, 120).count(), 1);
        assert!(index.removes("h"));
        assert_eq!(index.def_of("src/lib.rs", 0, 40, "f", false).map(|def| def.path.as_str()), Some("shop::f"));
        assert!(index.def_of("src/lib.rs", 0, 40, "f", false).is_some_and(|def| def.public && def.in_library));
        let targets: Vec<Target> = index.targets_in("src/lib.rs", 0, 40).map(|(_, _, target)| target).collect();
        assert_eq!(targets, [Target::Local, Target::Def(0)]);
    }

    #[test]
    fn a_test_unit_gives_no_visibility() {
        let record = record();
        let index = NameIndex::new([("shop.test.aaaa", &record)]);
        let def = index.def_of("src/lib.rs", 0, 40, "f", false).unwrap();
        assert!(!def.public && !def.in_library);
    }

    #[test]
    fn an_offset_counts_characters() {
        let text = "ab\nçd\n";
        assert_eq!(offset(text, 2, 1), Some(5));
        assert_eq!(offset(text, 2, 2), Some(6));
        assert_eq!(offset(text, 1, 0), Some(0));
        assert_eq!(offset(text, 4, 0), None);
    }
}
