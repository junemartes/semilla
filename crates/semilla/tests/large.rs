//! A step of the size of a pull request (design 37.10).
//!
//! The repository has no source of a crate of a different author. Thus the test makes its
//! fixture: a generator with a fixed seed writes a crate of about 2500 lines in 12 modules.
//! A change script applies a known list of changes. The test knows each change, so it can
//! find the fact of each change in the output.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// The seed of the generator. A different seed gives different numbers in the bodies.
const SEED: u64 = 0x5E41_11A7;

/// The modules of the fixture crate.
const MODULES: [&str; 12] = ["ledger", "stock", "price", "route", "batch", "audit", "quota", "index", "cache", "queue", "shape", "merge"];

/// The files of a tree: the path from the repository root, and the text.
type Tree = BTreeMap<String, String>;

/// A small source of numbers (splitmix64). Each item has its own source, so a change of one
/// item does not change the numbers of a different item.
struct Numbers(u64);

impl Numbers {
    fn of(module: usize, slot: u32) -> Numbers {
        Numbers(SEED ^ ((module as u64 + 1) << 40) ^ ((u64::from(slot) + 1) << 20))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number from `low` to `high - 1`.
    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low)
    }
}

/// The name of the types of a module: `ledger` gives `Ledger`.
fn type_name(module: usize) -> String {
    let name = MODULES[module];
    format!("{}{}", name[..1].to_uppercase(), &name[1..])
}

/// The mark of a function: a literal that only this function has. Each other literal of the
/// generator is less than 1000. A test finds a function by its mark.
fn mark(module: usize, slot: u32) -> u32 {
    1000 + 20 * module as u32 + slot
}

// The slots of the functions of a module.
const STEP0: u32 = 0;
const TOTAL: u32 = 4;
const SCALE: u32 = 5;
const SCORE: u32 = 7;
const GRADE: u32 = 8;
const FOLD: u32 = 9;
const COUNT_ABOVE: u32 = 10;
const BUMP: u32 = 11;
const SUM: u32 = 12;

/// The lines of a body with the result `acc`: a start value from `a` and `b`, the mark line,
/// some blocks, and the result line. `calls` are functions `fn(u32, u32) -> u32` that a block
/// can call.
fn body(module: usize, slot: u32, a: &str, b: &str, blocks: usize, indent: &str, calls: &[String]) -> String {
    let mut numbers = Numbers::of(module, slot);
    let mut lines = vec![format!("let mut acc = {a}.wrapping_mul({}).wrapping_add({b});", numbers.range(3, 900)), format!("acc = acc.wrapping_add({});", mark(module, slot))];
    for (index, call) in calls.iter().enumerate() {
        lines.push(format!("acc = {call}(acc, {});", numbers.range(2, 90) + index as u64));
    }
    for _ in 0..blocks {
        let (c, d, k) = (numbers.range(3, 900), numbers.range(3, 900), numbers.range(2, 9));
        match numbers.range(0, 5) {
            0 => {
                lines.push(format!("for step in 0..{} {{", numbers.range(2, 9)));
                lines.push(format!("    if step % {k} == 0 {{"));
                lines.push(format!("        acc = acc.wrapping_add(step * {c});"));
                lines.push("    } else {".to_string());
                lines.push(format!("        acc ^= {d};"));
                lines.push("    }".to_string());
                lines.push("}".to_string());
            }
            1 => {
                lines.push(format!("match acc % {} {{", k.max(3)));
                lines.push(format!("    0 => acc = acc.wrapping_add({c}),"));
                lines.push(format!("    1 => acc = acc.rotate_left({}),", numbers.range(1, 30)));
                lines.push(format!("    _ => acc ^= {d},"));
                lines.push("}".to_string());
            }
            2 => {
                lines.push(format!("while acc > {} {{", 10_000 + c * 100));
                lines.push(format!("    acc /= {k};"));
                lines.push("}".to_string());
            }
            3 => {
                lines.push(format!("if acc % {} < {} {{", k + 2, numbers.range(1, k + 1)));
                lines.push(format!("    acc = acc.wrapping_mul({c});"));
                lines.push("} else {".to_string());
                lines.push(format!("    acc = acc.wrapping_sub({b});"));
                lines.push("}".to_string());
            }
            _ => lines.push(format!("acc = acc.wrapping_mul({c}).wrapping_add({b} ^ {d});")),
        }
    }
    lines.push("acc".to_string());
    lines.iter().map(|line| format!("{indent}{line}\n")).collect()
}

/// The text of one module of the base tree.
fn module_text(module: usize) -> String {
    let name = MODULES[module];
    let ty = type_name(module);
    let before = MODULES[(module + MODULES.len() - 1) % MODULES.len()];
    let mut numbers = Numbers::of(module, 99);
    let mut out = String::new();
    out += &format!("//! The {name} part of the depot. The numbers have no meaning.\n\n");
    out += &format!("use crate::{before}::{before}_step0;\nuse std::fmt;\n\n");

    // The record, its impl, and a trait impl of the standard library.
    out += &format!("/// A record of the {name} part.\n#[derive(Debug, Clone, PartialEq)]\npub struct {ty}Record {{\n    pub id: u32,\n    pub weight: u32,\n    pub marks: Vec<u32>,\n}}\n\n");
    out += &format!("impl {ty}Record {{\n    /// Makes a record with no marks.\n    pub fn new(id: u32, weight: u32) -> Self {{\n        Self {{ id, weight, marks: Vec::new() }}\n    }}\n\n");
    out += &format!("    /// Gives the total of the marks that are less than the limit.\n    pub fn total(&self, limit: u32) -> u32 {{\n        let mut sum = {};\n        for value in &self.marks {{\n            if *value < limit {{\n                sum += *value;\n            }}\n        }}\n        sum\n    }}\n\n", mark(module, TOTAL));
    out += &format!("    /// Mixes the weight with a factor, and keeps the result as the new weight.\n    pub fn scale(&mut self, factor: u32) -> u32 {{\n{}        self.weight = acc;\n        self.weight\n    }}\n}}\n\n", body(module, SCALE, "self.weight", "factor", 1, "        ", &[]).replace("        acc\n", ""));
    out += &format!("impl fmt::Display for {ty}Record {{\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {{\n        write!(f, \"{{}}:{{}}\", self.id, self.weight)\n    }}\n}}\n\n");

    // A trait with a default method, and its impl.
    out += &format!("/// The score of a value of the {name} part.\npub trait {ty}Score {{\n    /// Gives the score for a seed.\n    fn score(&self, seed: u32) -> u32;\n\n    /// Gives the grade: a small number from the score.\n    fn grade(&self, seed: u32) -> u32 {{\n        let mut acc = self.score(seed);\n        acc = acc.wrapping_add({});\n        acc % {}\n    }}\n}}\n\n", mark(module, GRADE), numbers.range(5, 40));
    out += &format!("impl {ty}Score for {ty}Record {{\n    fn score(&self, seed: u32) -> u32 {{\n{}    }}\n}}\n\n", body(module, SCORE, "self.weight", "seed", 1, "        ", &[]));

    // A generic type with a generic impl.
    out += &format!("/// Two values of one type.\n#[derive(Debug, Clone, Copy)]\npub struct {ty}Pair<T> {{\n    pub left: T,\n    pub right: T,\n}}\n\n");
    out += &format!("impl<T: Copy + PartialOrd> {ty}Pair<T> {{\n    /// Gives the larger value.\n    pub fn larger(&self) -> T {{\n        if self.left < self.right {{ self.right }} else {{ self.left }}\n    }}\n\n");
    out += &format!("    /// Counts the values that are above the limit, with a fixed start.\n    pub fn count_above(&self, limit: T) -> u32 {{\n        let mut acc = {};\n        if self.left > limit {{\n            acc += 1;\n        }}\n        if self.right > limit {{\n            acc += {};\n        }}\n        acc\n    }}\n}}\n\n", mark(module, COUNT_ABOVE), numbers.range(2, 9));
    out += &format!("/// Uses a pair of two numbers.\npub fn {name}_pair_use(a: u32, b: u32) -> u32 {{\n    let pair = {ty}Pair {{ left: a, right: b }};\n    pair.count_above({}).wrapping_add(pair.larger())\n}}\n\n", numbers.range(2, 90));

    // A type that holds a value.
    out += &format!("/// A value with a count of its reads.\n#[derive(Debug, Clone)]\npub struct {ty}Holder {{\n    pub value: u32,\n    pub count: u32,\n}}\n\n");
    out += &format!("impl {ty}Holder {{\n    /// Makes a holder with no reads.\n    pub fn new(value: u32) -> Self {{\n        Self {{ value, count: 0 }}\n    }}\n\n    /// Reads the value.\n    pub fn get(&mut self) -> u32 {{\n        self.count += 1;\n        self.value\n    }}\n\n");
    out += &format!("    /// Mixes the count with a value.\n    pub fn bump(&mut self, value: u32) -> u32 {{\n{}    }}\n}}\n\n", body(module, BUMP, "self.count", "value", 1, "        ", &[]));
    out += &format!("/// Uses a holder of a number.\npub fn {name}_holder_use(x: u32) -> u32 {{\n    let mut holder = {ty}Holder::new(x);\n    holder.bump({}).wrapping_add(holder.get())\n}}\n\n", numbers.range(2, 90));

    // Generic functions.
    out += &format!("/// Gives the larger of two values.\npub fn {name}_pick<T: Copy + PartialOrd>(a: T, b: T) -> T {{\n    if a < b {{ b }} else {{ a }}\n}}\n\n");
    out += &format!("/// Mixes each item into one number.\npub fn {name}_fold<T: Copy + Into<u64>>(items: &[T]) -> u64 {{\n    let mut acc = {}u64;\n    for item in items {{\n        acc = acc.wrapping_mul({}).wrapping_add((*item).into());\n    }}\n    acc\n}}\n\n", mark(module, FOLD), numbers.range(3, 900));

    // Plain functions. Step 1 calls step 0. Step 2 calls a different module.
    for step in 0..3u32 {
        let mut calls = Vec::new();
        if step == 1 {
            calls.push(format!("{name}_step{}", step - 1));
        }
        if step == 2 {
            calls.push(format!("{before}_step0"));
        }
        out += &format!("/// Step {step} of the {name} part.\npub fn {name}_step{step}(x: u32, y: u32) -> u32 {{\n{}}}\n\n", body(module, STEP0 + step, "x", "y", 1 + (step / 2) as usize, "    ", &calls));
    }

    // A macro and its calls.
    out += &format!("/// Adds each value to a fixed start value.\nmacro_rules! {name}_sum {{\n    ($($value:expr),*) => {{\n        {}u32$(.wrapping_add($value))*\n    }};\n}}\n\n", mark(module, SUM));
    out += &format!("/// Adds two numbers and a fixed number to the start value.\npub fn {name}_sum_of(x: u32, y: u32) -> u32 {{\n    {name}_sum!(x, y, {})\n}}\n\n/// Adds a number two times to the start value.\npub fn {name}_sum_twice(x: u32) -> u32 {{\n    {name}_sum!(x, x)\n}}\n\n", numbers.range(2, 90));

    // The tests of the module.
    out += "#[cfg(test)]\nmod tests {\n    use super::*;\n\n";
    out += &format!("    #[test]\n    fn the_record_keeps_its_fields() {{\n        let record = {ty}Record::new(3, 4);\n        assert_eq!(record.id, 3);\n        assert_eq!(record.weight, 4);\n        assert_eq!(record.total(9), {});\n    }}\n\n", mark(module, TOTAL));
    out += &format!("    #[test]\n    fn each_step_gives_the_same_result_for_the_same_input() {{\n        for x in 0..5 {{\n            assert_eq!({name}_step0(x, 2), {name}_step0(x, 2));\n            assert_eq!({name}_step2(x, 2), {name}_step2(x, 2));\n        }}\n    }}\n\n");
    out += &format!("    #[test]\n    fn the_sum_starts_at_the_fixed_value() {{\n        assert_eq!({name}_sum_twice(0), {});\n    }}\n}}\n", mark(module, SUM));
    out
}

/// The base tree of the fixture: a workspace with the crate `depot`.
fn base_tree() -> Tree {
    let mut tree = Tree::new();
    tree.insert("Cargo.toml".to_string(), "[workspace]\nresolver = \"2\"\nmembers = [\"depot\"]\n".to_string());
    tree.insert(".gitignore".to_string(), "/target\n/.semilla/\n".to_string());
    tree.insert("depot/Cargo.toml".to_string(), "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"A fixture crate\"\n".to_string());
    let mut lib = "//! The depot crate: a fixture that a generator makes.\n\n".to_string();
    for name in MODULES {
        lib += &format!("pub mod {name};\n");
    }
    lib += "\n/// Runs the last step of each part.\npub fn run(seed: u32) -> u32 {\n    let mut acc = seed;\n";
    for name in MODULES {
        lib += &format!("    acc = {name}::{name}_step2(acc, 1);\n");
    }
    lib += "    acc\n}\n";
    tree.insert("depot/src/lib.rs".to_string(), lib);
    for (module, name) in MODULES.iter().enumerate() {
        tree.insert(format!("depot/src/{name}.rs"), module_text(module));
    }
    tree
}

fn file(name: &str) -> String {
    format!("depot/src/{name}.rs")
}

/// Replaces a text that a file has exactly one time.
#[track_caller]
fn replace_one(tree: &mut Tree, path: &str, from: &str, to: &str) {
    let text = tree.get_mut(path).unwrap_or_else(|| panic!("no file {path}"));
    assert_eq!(text.matches(from).count(), 1, "`{from}` in {path}");
    *text = text.replace(from, to);
}

/// Replaces a name in each file. Gives the count of the places.
#[track_caller]
fn replace_name(tree: &mut Tree, from: &str, to: &str) -> usize {
    let mut count = 0;
    for text in tree.values_mut() {
        count += text.matches(from).count();
        *text = text.replace(from, to);
    }
    assert!(count > 0, "no `{from}`");
    count
}

/// The byte range of an item: from its first doc line or attribute line to its last line,
/// with the empty line after it. `head` is the start of the first line of the declaration.
#[track_caller]
fn item_range(text: &str, head: &str) -> (usize, usize) {
    let at = text.find(head).unwrap_or_else(|| panic!("no item `{head}`"));
    assert_eq!(text.matches(head).count(), 1, "`{head}` is not one item");
    let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    let indent = &text[line_start..at];
    assert!(indent.trim().is_empty(), "`{head}` is not at the start of a line");
    // Go up through the doc lines and the attribute lines.
    let mut start = line_start;
    while start > 0 {
        let previous = text[..start - 1].rfind('\n').map_or(0, |index| index + 1);
        let line = text[previous..start - 1].trim_start();
        if line.starts_with("///") || line.starts_with("#[") {
            start = previous;
        } else {
            break;
        }
    }
    // The item ends at its first line, or at the first `}` line with the same indent.
    let first_end = at + text[at..].find('\n').unwrap();
    let first_line = &text[at..first_end];
    let mut end = first_end + 1;
    if !first_line.ends_with(';') && !(first_line.ends_with('}') && !first_line.ends_with("{}") && first_line.matches('{').count() == first_line.matches('}').count()) {
        let close = format!("\n{indent}}}\n");
        end = first_end + text[first_end..].find(&close).unwrap_or_else(|| panic!("no end of `{head}`")) + close.len();
    }
    if text[end..].starts_with('\n') {
        end += 1;
    }
    (start, end)
}

/// Removes an item from a file and gives its text.
#[track_caller]
fn take_item(tree: &mut Tree, path: &str, head: &str) -> String {
    let text = tree.get_mut(path).unwrap();
    let (start, end) = item_range(text, head);
    let item = text[start..end].to_string();
    text.replace_range(start..end, "");
    item
}

/// Puts a text before an item of a file.
#[track_caller]
fn put_before(tree: &mut Tree, path: &str, head: &str, item: &str) {
    let text = tree.get_mut(path).unwrap();
    let (start, _) = item_range(text, head);
    text.insert_str(start, item);
}

/// Replaces a text in one item of a file. The item must have the text exactly one time.
#[track_caller]
fn replace_in_item(tree: &mut Tree, path: &str, head: &str, from: &str, to: &str) {
    let text = tree.get_mut(path).unwrap();
    let (start, end) = item_range(text, head);
    let item = &text[start..end];
    assert_eq!(item.matches(from).count(), 1, "`{from}` in the item `{head}`");
    let item = item.replace(from, to);
    text.replace_range(start..end, &item);
}

/// A fact that the output must have: one fact line with the category, the compare result, and
/// each text of `has`. An empty compare result permits each result.
struct Expect {
    category: &'static str,
    compare: &'static str,
    has: Vec<String>,
}

/// One change of the list, with the facts that show it.
struct Change {
    name: &'static str,
    facts: Vec<Expect>,
}

fn expect(category: &'static str, compare: &'static str, has: &[&str]) -> Expect {
    Expect { category, compare, has: has.iter().map(|text| text.to_string()).collect() }
}

/// A function with a generated body, for an added item.
fn generated_fn(doc: &str, name: &str, module: usize, slot: u32, blocks: usize) -> String {
    format!("/// {doc}\npub fn {name}(x: u32, y: u32) -> u32 {{\n{}}}\n\n", body(module, slot, "x", "y", blocks, "    ", &[]))
}

/// The tree after the step, and the list of the changes.
fn changed_tree() -> (Tree, Vec<Change>) {
    let mut tree = base_tree();
    let t = &mut tree;
    let mut changes = Vec::new();
    let [ledger, stock, price, route, batch, audit, quota, index, cache, queue, shape, merge] = MODULES.map(file);

    // Three renames: a function with callers in two modules, a type with three impls, and a
    // method of a generic impl.
    replace_name(t, "ledger_step0", "ledger_first_step");
    changes.push(Change { name: "rename a function", facts: vec![expect("rename", "code equal", &["renamed `depot::ledger::ledger_step0` → `depot::ledger::ledger_first_step`"])] });
    replace_name(t, "StockRecord", "StockEntry");
    changes.push(Change { name: "rename a type with impls", facts: vec![expect("rename", "code different", &["renamed `depot::stock::StockRecord` → `depot::stock::StockEntry`"])] });
    for (from, to) in [("pub fn count_above(&self, limit: T)", "pub fn count_over(&self, limit: T)"), ("pair.count_above(", "pair.count_over(")] {
        replace_one(t, &price, from, to);
    }
    changes.push(Change { name: "rename a method of a generic impl", facts: vec![expect("rename", "code equal", &["renamed `depot::price::PricePair::count_above` → `depot::price::PricePair::count_over`"])] });

    // Two moves of a function to a different module. The second function needs an import.
    let item = take_item(t, &route, "pub fn route_step1(");
    put_before(t, &batch, "pub fn batch_step0(", &item);
    changes.push(Change { name: "move a function", facts: vec![expect("move", "code equal", &["moved `depot::route::route_step1` → `depot::batch::route_step1`"])] });
    let item = take_item(t, &audit, "pub fn audit_holder_use(");
    put_before(t, &quota, "pub fn quota_holder_use(", &item);
    replace_one(t, &quota, "use std::fmt;\n", "use crate::audit::AuditHolder;\nuse std::fmt;\n");
    changes.push(Change { name: "move a function that needs an import", facts: vec![expect("move", "code equal", &["moved `depot::audit::audit_holder_use` → `depot::quota::audit_holder_use`", "with 1 `use` line(s)"])] });

    // A type gets a generic parameter.
    replace_one(t, &index, "pub struct IndexHolder {\n    pub value: u32,", "pub struct IndexHolder<T> {\n    pub value: T,");
    replace_one(t, &index, "impl IndexHolder {", "impl<T: Copy> IndexHolder<T> {");
    replace_one(t, &index, "pub fn new(value: u32) -> Self {", "pub fn new(value: T) -> Self {");
    replace_one(t, &index, "pub fn get(&mut self) -> u32 {", "pub fn get(&mut self) -> T {");
    changes.push(Change { name: "a type gets a generic parameter", facts: vec![expect("declaration", "code different", &["generics of `depot::index::IndexHolder`: `struct IndexHolder { }` → `struct IndexHolder<T> { }`"]), expect("declaration", "code different", &["type `u32` → `T` in 3 places: `depot::index::IndexHolder.value`, return type of `depot::index::IndexHolder::get`, parameter `value` of `depot::index::IndexHolder::new`"]), expect("declaration", "code different", &["generics of `depot::index::impl IndexHolder`: `impl IndexHolder { }` → `impl<T: Copy> IndexHolder<T> { }`"])] });

    // Six body edits: two of a value, two of the logic, two of the control flow. The category
    // of each one is `semantics` (design 42). A statement that moves into a new `if`
    // keeps its compiled code: the `if` owns the change (design 40.3).
    replace_one(t, &cache, &format!("acc = acc.wrapping_add({});", mark(8, STEP0)), &format!("acc = acc.wrapping_add({});", mark(8, STEP0) + 1000));
    changes.push(Change { name: "body edit: a value in a function", facts: vec![expect("semantics", "code different", &["changed literal `1160` → `2160` at `depot::cache::cache_step0/S1/rhs/args[0]`"])] });
    replace_one(t, &shape, &format!("acc = acc.wrapping_add({});", mark(10, GRADE)), &format!("acc = acc.wrapping_add({});", mark(10, GRADE) + 1000));
    changes.push(Change { name: "body edit: a value in a default method of a trait", facts: vec![expect("semantics", "code different", &["changed literal `1208` → `2208` at `depot::shape::ShapeScore::grade/S1/rhs/args[0]`"])] });
    replace_one(t, &queue, &format!("acc = acc.wrapping_add({});", mark(9, STEP0 + 1)), &format!("acc = acc.wrapping_sub({});", mark(9, STEP0 + 1)));
    changes.push(Change { name: "body edit: the logic of a function", facts: vec![expect("semantics", "code different", &["changed expression `acc.wrapping_add(1181)` → `acc.wrapping_sub(1181)` at `depot::queue::queue_step1/S1/rhs`"])] });
    let line = format!("        acc = acc.wrapping_add({});\n", mark(11, SCALE));
    replace_one(t, &merge, &line, &format!("{line}        acc = acc.rotate_left(3);\n"));
    changes.push(Change { name: "body edit: a new statement in a method", facts: vec![expect("semantics", "code different", &["added statement `acc = acc.rotate_left(3)` at `depot::merge::MergeRecord::scale/S2`"])] });
    replace_one(t, &quota, &format!("acc = acc.wrapping_add({});", mark(6, SCALE)), &format!("acc = acc.wrapping_add({}) ^ 5;", mark(6, SCALE)));
    changes.push(Change { name: "body edit: the logic of a method", facts: vec![expect("semantics", "code different", &["changed expression `acc.wrapping_add(1125)` → `acc.wrapping_add(1125) ^ 5` at `depot::quota::QuotaRecord::scale/S1/rhs`"])] });
    let line = format!("    acc = acc.wrapping_add({});\n", mark(10, STEP0 + 2));
    replace_one(t, &shape, &line, &format!("{line}    if acc > 4000 {{\n        return acc;\n    }}\n"));
    changes.push(Change { name: "body edit: the control flow of a function", facts: vec![expect("semantics", "code different", &["added early exit `if acc > 4000 { return acc; }` at `depot::shape::shape_step2/S2`"])] });
    let line = format!("        acc = acc.wrapping_add({});\n", mark(8, SCORE));
    replace_one(t, &cache, &line, &format!("        if seed > 3 {{\n    {line}        }}\n"));
    // Design 41: the statement moves into the new `if`. The move and the new `if` are two facts.
    changes.push(Change { name: "body edit: the control flow of a trait method", facts: vec![expect("semantics", "code equal", &["moved statement `acc = acc.wrapping_add(1167)` from `depot::cache::<CacheRecord as CacheScore>::score/S1` to `depot::cache::<CacheRecord as CacheScore>::score/S1/then/S0`"]), expect("semantics", "code different", &["added `if` `if seed > 3 { acc = acc.wrapping_add(1167); }` (each inner statement is a moved statement of the body) at `depot::cache::<CacheRecord as CacheScore>::score/S1`"])] });

    // Added items: functions, a type with an impl, a method, a constant, and a trait impl.
    let blend = generated_fn("Blends two numbers. The larger number goes first.", "cache_blend", 8, 13, 9).replace("-> u32 {\n", "-> u32 {\n    let (x, y) = match x.cmp(&y) {\n        Ordering::Less => (y, x),\n        Ordering::Equal | Ordering::Greater => (x, y),\n    };\n");
    put_before(t, &cache, "macro_rules! cache_sum {", &blend);
    changes.push(Change { name: "add a function", facts: vec![expect("added", "code different", &["added `depot::cache::cache_blend`"])] });
    let window = format!(
        "/// A window of the last values.\n#[derive(Debug, Clone, Default)]\npub struct QueueWindow {{\n    pub values: Vec<u32>,\n    pub size: usize,\n}}\n\nimpl QueueWindow {{\n    /// Adds a value. Removes the oldest value first if the window is full.\n    pub fn push(&mut self, value: u32) -> u32 {{\n        if self.values.len() == self.size && !self.values.is_empty() {{\n            self.values.remove(0);\n        }}\n        self.values.push(value);\n{}    }}\n\n    /// Mixes each value of the window.\n    pub fn mix(&self, seed: u32) -> u32 {{\n        let mut acc = seed;\n        for value in &self.values {{\n            acc = acc.wrapping_mul(31).wrapping_add(*value);\n        }}\n        acc\n    }}\n}}\n\n",
        body(9, 14, "value", "self.size as u32", 6, "        ", &[])
    );
    put_before(t, &queue, "pub fn queue_pick<", &window);
    changes.push(Change { name: "add a type with an impl", facts: vec![expect("added", "code different", &["added `depot::queue::QueueWindow`"]), expect("added", "code different", &["added `depot::queue::impl QueueWindow` with: `fn push`, `fn mix`"])] });
    put_before(t, &shape, "    pub fn total(", "    /// Removes each mark. Gives the count of the removed marks.\n    pub fn reset(&mut self) -> usize {\n        let count = self.marks.len();\n        self.marks.clear();\n        count\n    }\n\n");
    changes.push(Change { name: "add a method", facts: vec![expect("added", "code different", &["added `depot::shape::ShapeRecord::reset`"])] });
    put_before(t, &merge, "pub fn merge_step0(", "/// The largest input that a caller can give.\npub const MERGE_LIMIT: u32 = 77;\n\n");
    changes.push(Change { name: "add a constant", facts: vec![expect("added", "no code", &["added `depot::merge::MERGE_LIMIT`"])] });
    put_before(t, &merge, "pub fn merge_holder_use(", "impl Default for MergeHolder {\n    fn default() -> Self {\n        Self::new(0)\n    }\n}\n\n");
    changes.push(Change { name: "add a trait impl", facts: vec![expect("added", "code different", &["trait impl `<MergeHolder as Default>` added with: `fn default`"])] });
    put_before(t, &route, "macro_rules! route_sum {", &generated_fn("Mixes two numbers in many steps.", "route_long_mix", 3, 13, 22));
    changes.push(Change { name: "add a second function", facts: vec![expect("added", "code different", &["added `depot::route::route_long_mix`"])] });
    put_before(t, &audit, "macro_rules! audit_sum {", &generated_fn("Mixes two numbers in many steps.", "audit_long_mix", 5, 13, 22));
    changes.push(Change { name: "add a third function", facts: vec![expect("added", "code different", &["added `depot::audit::audit_long_mix`"])] });
    put_before(t, &stock, "macro_rules! stock_sum {", &generated_fn("Mixes two numbers in many steps.", "stock_long_mix", 1, 13, 22));
    changes.push(Change { name: "add a fourth function", facts: vec![expect("added", "code different", &["added `depot::stock::stock_long_mix`"])] });

    for (path, name, call) in [(&index, "the_long_way_gives_the_same_result", "index_step1"), (&queue, "the_window_keeps_the_last_values", "queue_step2")] {
        let test = format!(
            "\n    #[test]\n    fn {name}() {{\n        let mut seen = Vec::new();\n        for x in 0..7 {{\n            let value = {call}(x, 3);\n            assert_eq!(value, {call}(x, 3));\n            seen.push(value);\n        }}\n        assert_eq!(seen.len(), 7);\n        seen.dedup();\n        assert!(!seen.is_empty());\n    }}\n}}\n"
        );
        let text = t.get_mut(path).unwrap();
        assert!(text.ends_with("    }\n}\n"), "{path}");
        text.truncate(text.len() - 2);
        text.push_str(&test);
    }
    changes.push(Change { name: "add a test", facts: vec![expect("added", "code different", &["added `depot::index::tests::the_long_way_gives_the_same_result`"])] });
    changes.push(Change { name: "add a second test", facts: vec![expect("added", "code different", &["added `depot::queue::tests::the_window_keeps_the_last_values`"])] });

    // Removed items: a function, a generic function, and a trait impl with its import.
    take_item(t, &audit, "pub fn audit_pair_use(");
    changes.push(Change { name: "remove a function", facts: vec![expect("removed", "code different", &["removed `depot::audit::audit_pair_use`"])] });
    take_item(t, &quota, "pub fn quota_pick<");
    changes.push(Change { name: "remove a generic function", facts: vec![expect("removed", "code different", &["removed `depot::quota::quota_pick`"])] });
    take_item(t, &batch, "impl fmt::Display for BatchRecord {");
    changes.push(Change { name: "remove a trait impl", facts: vec![expect("removed", "code different", &["trait impl `<BatchRecord as fmt::Display>` removed with: `fn fmt`"])] });
    replace_one(t, &batch, "use std::fmt;\n", "");
    changes.push(Change { name: "import: remove an import", facts: vec![expect("import", "code equal", &["removed `use` line: `use std::fmt;`"])] });

    // Declaration changes.
    replace_one(t, &ledger, "pub fn ledger_sum_of(x: u32, y: u32) -> u32 {", "pub fn ledger_sum_of(x: u32, y: u32, z: u32) -> u32 {");
    let text = t.get(&ledger).unwrap().clone();
    let (start, end) = item_range(&text, "pub fn ledger_sum_of(");
    let call = text[start..end].lines().find(|line| line.contains("ledger_sum!(")).unwrap().to_string();
    replace_one(t, &ledger, &call, "    ledger_sum!(x, y, z)");
    changes.push(Change { name: "declaration: a function gets a parameter", facts: vec![expect("declaration", "code different", &["parameter of `depot::ledger::ledger_sum_of` added: `z: u32`"]), expect("macro", "code different", &[&format!("replace macro input `{}` → `ledger_sum!(x, y, z)` at `depot::ledger::ledger_sum_of/tail`", call.trim())])] });
    replace_one(t, &stock, "pub fn stock_fold<T: Copy + Into<u64>>(", "pub fn stock_fold<T: Copy + Into<u64> + PartialOrd>(");
    changes.push(Change { name: "declaration: a generic function gets a bound", facts: vec![expect("declaration", "code equal", &["generics of `depot::stock::stock_fold` `<T: Copy + Into<u64>>` → `<T: Copy + Into<u64> + PartialOrd>`"])] });
    replace_one(t, &route, "pub fn route_pick<T: Copy + PartialOrd>(a: T, b: T) -> T {", "pub fn route_pick<T>(a: T, b: T) -> T\nwhere\n    T: Copy + PartialOrd,\n{");
    changes.push(Change { name: "declaration: the bounds go to a `where` clause", facts: vec![expect("declaration", "code equal", &["bounds of `depot::route::route_pick`: moved to the `where` clause"])] });
    replace_one(t, &cache, "pub struct CacheHolder {\n    pub value: u32,\n    pub count: u32,\n", "pub struct CacheHolder {\n    pub value: u32,\n    pub count: u32,\n    pub limit: u32,\n");
    replace_one(t, &cache, "Self { value, count: 0 }", "Self { value, count: 0, limit: 9 }");
    changes.push(Change { name: "declaration: a type gets a field", facts: vec![expect("added", "code different", &["field `depot::cache::CacheHolder.limit` added: `u32`"])] });

    // Doc changes.
    replace_one(t, &route, "/// Step 0 of the route part.", "/// Step 0 of the route part: the first mix.");
    changes.push(Change { name: "docs: a function", facts: vec![expect("docs", "no code", &["changed docs `/// Step 0 of the route part.` → `/// Step 0 of the route part: the first mix.`"])] });
    replace_one(t, &audit, "//! The audit part of the depot. The numbers have no meaning.", "//! The audit part of the depot. The numbers have no meaning at all.");
    changes.push(Change { name: "docs: a module", facts: vec![expect("docs", "no code", &["changed docs `…numbers have no meaning.` → `…numbers have no meaning at all.`"])] });
    replace_one(t, &quota, "/// A record of the quota part.", "/// One record of the quota part.");
    changes.push(Change { name: "docs: a type", facts: vec![expect("docs", "no code", &["changed docs `/// A record of the quota part.` → `/// One record of the quota part.`"])] });
    replace_one(t, &merge, "    /// Makes a record with no marks.", "    /// Makes a new record with no marks.");
    changes.push(Change { name: "docs: a method", facts: vec![expect("docs", "no code", &["changed docs `/// Makes a record with no marks.` → `/// Makes a new record with no marks.`"])] });
    replace_one(t, &price, "/// The score of a value of the price part.", "/// The score of one value of the price part.");
    changes.push(Change { name: "docs: a trait", facts: vec![expect("docs", "no code", &["changed docs `/// The score of a value of the price part.` → `/// The score of one value of the price part.`"])] });

    // Import changes. The removed import is above, with the removed impl.
    replace_one(t, &cache, "use std::fmt;\n", "use std::cmp::Ordering;\nuse std::fmt;\n");
    changes.push(Change { name: "import: add an import", facts: vec![expect("import", "code equal", &["added `use` line: `use std::cmp::Ordering;`"])] });
    replace_one(t, &stock, "use std::fmt;\n", "use std::fmt::{self, Formatter};\n");
    replace_one(t, &stock, "f: &mut fmt::Formatter<'_>", "f: &mut Formatter<'_>");
    changes.push(Change { name: "import: use a name through a new import", facts: vec![expect("import", "code equal", &["added `use` line: `use std::fmt::{self, Formatter};`"]), expect("declaration", "code equal", &["parameter of `depot::stock::<StockEntry as fmt::Display>::fmt` `f: &mut fmt::Formatter<'_>` → `f: &mut Formatter<'_>`"])] });

    // A manifest change.
    replace_one(t, "depot/Cargo.toml", "version = \"0.1.0\"", "version = \"0.2.0\"");
    changes.push(Change { name: "manifest: the version", facts: vec![expect("manifest", "no code", &["depot/Cargo.toml: package.version \"0.1.0\" → \"0.2.0\""])] });
    (tree, changes)
}

/// The tree with 300 changed lines in one file (design 37.10): the file of the `ledger` part.
fn one_file_tree() -> (Tree, Vec<Change>) {
    let mut tree = base_tree();
    let t = &mut tree;
    let mut changes = Vec::new();
    let ledger = file("ledger");
    for (name, slot) in [("ledger_wide_mix", 13), ("ledger_deep_mix", 14)] {
        put_before(t, &ledger, "macro_rules! ledger_sum {", &generated_fn("Mixes two numbers in many steps.", name, 0, slot, 28));
        changes.push(Change { name: "add a long function", facts: vec![Expect { category: "added", compare: "code different", has: vec![format!("added `depot::ledger::{name}`")] }] });
    }
    take_item(t, &ledger, "pub fn ledger_pair_use(");
    changes.push(Change { name: "remove a function", facts: vec![expect("removed", "code different", &["removed `depot::ledger::ledger_pair_use`"])] });
    take_item(t, &ledger, "impl fmt::Display for LedgerRecord {");
    replace_one(t, &ledger, "use std::fmt;\n", "");
    changes.push(Change { name: "remove a trait impl and its import", facts: vec![expect("removed", "code different", &["trait impl `<LedgerRecord as fmt::Display>` removed with: `fn fmt`"]), expect("import", "code equal", &["removed `use` line: `use std::fmt;`"])] });
    replace_in_item(t, &ledger, "pub fn ledger_step1(", &format!("acc = acc.wrapping_add({});", mark(0, STEP0 + 1)), &format!("acc = acc.wrapping_add({});", mark(0, STEP0 + 1) + 1000));
    changes.push(Change { name: "body edit: a value", facts: vec![expect("semantics", "code different", &["changed literal `1001` → `2001` at `depot::ledger::ledger_step1/S1/rhs/args[0]`"])] });
    replace_in_item(t, &ledger, "    pub fn scale(", &format!("acc = acc.wrapping_add({});", mark(0, SCALE)), &format!("acc = acc.wrapping_sub({});", mark(0, SCALE)));
    changes.push(Change { name: "body edit: the logic", facts: vec![expect("semantics", "code different", &["changed expression `acc.wrapping_add(1005)` → `acc.wrapping_sub(1005)` at `depot::ledger::LedgerRecord::scale/S1/rhs`"])] });
    let line = format!("    acc = acc.wrapping_add({});\n", mark(0, STEP0 + 2));
    replace_one(t, &ledger, &line, &format!("{line}    if acc > 4000 {{\n        return acc;\n    }}\n"));
    changes.push(Change { name: "body edit: the control flow", facts: vec![expect("semantics", "code different", &["added early exit `if acc > 4000 { return acc; }` at `depot::ledger::ledger_step2/S2`"])] });
    replace_name(t, "ledger_sum_twice", "ledger_sum_double");
    changes.push(Change { name: "rename a function", facts: vec![expect("rename", "", &["renamed `depot::ledger::ledger_sum_twice` → `depot::ledger::ledger_sum_double`"])] });
    replace_one(t, &ledger, "/// A record of the ledger part.", "/// One record of the ledger part.");
    changes.push(Change { name: "docs: a type", facts: vec![expect("docs", "no code", &["changed docs `/// A record of the ledger part.` → `/// One record of the ledger part.`"])] });
    (tree, changes)
}

/// One small change that a reviewer must see: the place, and the text that a fact must show.
struct Needle {
    name: &'static str,
    module: &'static str,
    from: String,
    to: String,
    /// The text of the change that a fact line must have.
    shows: &'static str,
    /// Each fact line that differs from the run with no needle has this text.
    item: &'static str,
}

/// The needles: each is one edit of the tree after the step.
fn needles() -> Vec<Needle> {
    let add = |module: usize, slot: u32| format!("acc = acc.wrapping_add({});", mark(module, slot));
    let value = |name, module: usize, slot: u32, shows, item| Needle { name, module: MODULES[module], from: add(module, slot), to: add(module, slot).replace('(', "(9"), shows, item };
    let new_statement = |name, module: usize, text: String, indent: &str, statement: &str, shows, item| Needle { name, module: MODULES[module], to: format!("{text}\n{indent}{statement}"), from: text, shows, item };
    vec![
        value("a function that the list does not change", 11, STEP0, "91220", "merge_step0"),
        Needle { name: "a second function that the list does not change", module: "price", from: add(2, STEP0 + 2), to: add(2, STEP0 + 2).replace("wrapping_add", "wrapping_sub"), shows: "wrapping_sub(1042)", item: "price_step2" },
        new_statement("a method that the list does not change", 5, add(5, SCALE), "        ", "acc ^= 90003;", "90003", "AuditRecord::scale"),
        new_statement("a function that the list changes", 8, add(8, STEP0).replace("1160", "2160"), "    ", "acc ^= 90004;", "90004", "cache_step0"),
        value("a second function that the list changes", 10, STEP0 + 2, "91202", "shape_step2"),
        value("a renamed function", 0, STEP0, "91000", "ledger_first_step"),
        Needle { name: "a renamed method", module: "price", from: format!("let mut acc = {};", mark(2, COUNT_ABOVE)), to: format!("let mut acc = 9{};", mark(2, COUNT_ABOVE)), shows: "91050", item: "count_over" },
        value("a method of a renamed type", 1, SCALE, "91025", "StockEntry"),
        Needle { name: "a moved function", module: "batch", from: add(3, STEP0 + 1), to: add(3, STEP0 + 1).replace('(', "(9"), shows: "91061", item: "route_step1" },
        Needle { name: "a moved function with an import", module: "quota", from: "let mut holder = AuditHolder::new(x);".to_string(), to: "let mut holder = AuditHolder::new(x ^ 90010);".to_string(), shows: "90010", item: "audit_holder_use" },
        value("a member of the impl with the new generic parameter", 7, BUMP, "91151", "IndexHolder"),
        Needle { name: "a second member of the impl with the new generic parameter", module: "index", from: "self.count += 1;".to_string(), to: "self.count += 90012;".to_string(), shows: "90012", item: "IndexHolder" },
        Needle { name: "a macro body", module: "shape", from: format!("{}u32$(", mark(10, SUM)), to: format!("9{}u32$(", mark(10, SUM)), shows: "91212", item: "shape_sum" },
        value("a method of a trait impl", 9, SCORE, "91187", "QueueRecord"),
    ]
}

// The run of Semilla on a repository of the test.

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Sets the tools and the cache folder of a test repository: `cache` next to the repository.
fn with_env(command: &mut Command, repo: &Path) {
    let root = workspace_root();
    command.current_dir(repo);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", repo.parent().unwrap().join("cache"));
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
}

/// Runs Semilla. Gives the exit code and the text of stdout.
fn semilla(repo: &Path, args: &[&str]) -> (i32, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    with_env(&mut command, repo);
    let output = command.args(args).output().unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs a command line of Semilla through the shell, as an agent does.
fn shell(repo: &Path, line: &str) -> (i32, String) {
    let binary = Path::new(env!("CARGO_BIN_EXE_semilla"));
    let mut command = Command::new("sh");
    with_env(&mut command, repo);
    command.env("PATH", format!("{}:{}", binary.parent().unwrap().display(), std::env::var("PATH").unwrap_or_default()));
    let output = command.args(["-c", line]).output().unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").current_dir(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write_tree(folder: &Path, tree: &Tree) {
    for (path, text) in tree {
        let full = folder.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
}

/// Makes a repository with the base tree in one commit. The folder `repo` has the repository.
fn setup() -> common::TestDir {
    // The MIR driver is a separate binary of the workspace.
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");
    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    write_tree(repo, &base_tree());
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    dir
}

/// The count of the changed lines of the work tree: the added lines and the removed lines.
fn changed_lines(repo: &Path) -> usize {
    git(repo, &["diff", "--numstat"]).lines().flat_map(|line| line.split_whitespace().take(2).map(|count| count.parse::<usize>().unwrap()).collect::<Vec<_>>()).sum()
}

/// One fact line of the output: `◆ f1 derived: TEXT [category · compare]`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Fact {
    text: String,
    category: String,
    compare: String,
}

/// The facts of an output, without their numbers. A needle can change the numbers.
fn facts(output: &str) -> Vec<Fact> {
    let mut facts = Vec::new();
    for line in output.lines() {
        let Some(rest) = line.trim_start().strip_prefix("◆ f") else { continue };
        let (_, rest) = rest.split_once(": ").expect("a fact line has its source and its text");
        let (text, class) = rest.rsplit_once(" [").expect("a fact line has its category");
        let (category, compare) = class.trim_end_matches(']').split_once(" · ").expect("a fact line has its compare result");
        facts.push(Fact { text: text.to_string(), category: category.to_string(), compare: compare.to_string() });
    }
    facts
}

impl Expect {
    fn fits(&self, fact: &Fact) -> bool {
        fact.category == self.category && (self.compare.is_empty() || fact.compare == self.compare) && self.has.iter().all(|text| fact.text.contains(text.as_str()))
    }
}

/// Runs the step until Semilla accepts it. A second run has the line that Semilla prints after
/// `run again:`, with a text for each reason. Gives the count of the runs and the last output.
fn accept(repo: &Path, intent: &str) -> (usize, String) {
    let (code, output) = semilla(repo, &["step", intent]);
    if code == 0 {
        return (1, output);
    }
    assert_eq!(code, 1, "{output}");
    let line = output.lines().find_map(|line| line.strip_prefix("run again: ")).unwrap_or_else(|| panic!("a rejected step prints the next command:\n{output}"));
    let (code, output) = shell(repo, &line.replace("<why>", "the test gives this reason"));
    assert_eq!(code, 0, "the second run must pass:\n{output}");
    (2, output)
}

/// The asserts of a step of PR size: Semilla accepts the step in at most two runs, each change
/// has its facts, and the output stays small.
fn check_step(repo: &Path, changes: &[Change]) -> (usize, Vec<Fact>) {
    let (runs, output) = accept(repo, "a step of the size of a pull request");
    assert!(output.starts_with("accepted: "), "{output}");
    let facts = facts(&output);
    for change in changes {
        for expect in &change.facts {
            let count = facts.iter().filter(|fact| expect.fits(fact)).count();
            assert_eq!(count, 1, "the change \"{}\" needs one fact [{} · {}] with {:?}:\n{output}", change.name, expect.category, expect.compare, expect.has);
        }
    }
    // The output is small: at most two facts for each change, and no very long line.
    assert!(facts.len() <= 2 * changes.len(), "{} facts for {} changes:\n{output}", facts.len(), changes.len());
    let longest = output.lines().map(|line| line.chars().count()).max().unwrap_or(0);
    // Design 38.5: no line of the text output has more than 500 characters.
    assert!(longest <= 500, "a line has {longest} characters:\n{output}");
    (runs, facts)
}

#[test]
fn the_generator_gives_the_same_tree_for_the_same_seed() {
    let (base, (changed, changes)) = (base_tree(), changed_tree());
    assert_eq!(base, base_tree());
    assert_eq!(changed, changed_tree().0);
    assert_eq!(base.len(), 4 + MODULES.len());
    let lines = |tree: &Tree| tree.values().map(|text| text.lines().count()).sum::<usize>();
    assert!((2500..=3100).contains(&lines(&base)), "{}", lines(&base));
    // The list has each kind of change that the test needs.
    let count = |start: &str| changes.iter().filter(|change| change.name.starts_with(start)).count();
    assert!(count("rename") >= 3 && count("move") >= 2 && count("body edit") >= 6 && count("add") >= 5 && count("remove") >= 3, "{:?}", changes.iter().map(|change| change.name).collect::<Vec<_>>());
    assert!(count("declaration") >= 4 && count("docs") >= 5 && count("import") >= 3 && count("manifest") >= 1 && count("a type gets a generic parameter") == 1);
    // A different seed gives a different tree: the seed is in use.
    assert!(Numbers::of(0, 0).next() != Numbers(SEED ^ 1).next());
    // A developer can look at the trees: `SEMILLA_LARGE_DUMP=<folder>` writes them.
    if let Ok(folder) = std::env::var("SEMILLA_LARGE_DUMP") {
        write_tree(&Path::new(&folder).join("base"), &base);
        write_tree(&Path::new(&folder).join("changed"), &changed);
        write_tree(&Path::new(&folder).join("one-file"), &one_file_tree().0);
    }
}

#[test]
fn a_step_of_pr_size_is_accepted_and_each_change_has_its_fact() {
    let start = Instant::now();
    let (tree, changes) = changed_tree();
    let dir = setup();
    let repo = &dir.path().join("repo");
    write_tree(repo, &tree);
    let lines = changed_lines(repo);
    assert!((500..=800).contains(&lines), "{lines} changed lines");

    let (runs, facts) = check_step(repo, &changes);
    assert!(runs <= 2);
    // A rename or a move is one identity fact. It does not show as a removed and an added item.
    for name in ["ledger_step0", "ledger_first_step", "StockRecord", "StockEntry", "count_above", "count_over", "route_step1", "audit_holder_use"] {
        let wrong: Vec<&Fact> = facts.iter().filter(|fact| ["added", "removed"].contains(&fact.category.as_str()) && fact.text.contains(name)).collect();
        assert!(wrong.is_empty(), "{wrong:?}");
    }
    // A method with the name of the renamed method in a different type has no fact (design
    // 37.10): the 11 other `count_above` methods and their calls did not change.
    assert_eq!(facts.iter().filter(|fact| fact.text.contains("count_above")).count(), 2, "{facts:#?}");
    // The time of the step with a cold cache, on a machine with other work.
    let time = start.elapsed().as_secs();
    assert!(time < 120, "the step needs {time} s");
    println!("{lines} changed lines, {} changes, {} facts, {runs} run(s), {time} s", changes.len(), facts.len());
}

#[test]
fn a_derived_body_of_a_renamed_type_goes_to_the_rename() {
    // Design 37.12. `derive(Debug)` puts the name of the type in the code. The rename of the
    // type is the cause, and its fact shows the body. No reason is necessary.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let mut tree = base_tree();
    replace_name(&mut tree, "StockRecord", "StockEntry");
    write_tree(repo, &tree);
    let (code, output) = semilla(repo, &["step", "rename a type"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("renamed `depot::stock::StockRecord` → `depot::stock::StockEntry` [rename · code different]\n      ! the compiled code changes: <depot::stock::StockEntry as std::fmt::Debug>::fmt\n"), "{output}");
    assert!(!output.contains("indirect"), "{output}");

    // A new package version changes the hash in each target name. The bodies stay equal.
    semilla(repo, &["undo"]);
    let mut tree = base_tree();
    replace_one(&mut tree, "depot/Cargo.toml", "version = \"0.1.0\"", "version = \"0.2.0\"");
    write_tree(repo, &tree);
    let (code, output) = semilla(repo, &["step", "a new version"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("depot/Cargo.toml: package.version \"0.1.0\" → \"0.2.0\" [manifest · no code]") && !output.contains("spotlight"), "{output}");
}

#[test]
fn a_step_of_300_lines_in_one_file_is_accepted_and_each_change_has_its_fact() {
    let (tree, changes) = one_file_tree();
    let dir = setup();
    let repo = &dir.path().join("repo");
    write_tree(repo, &tree);
    assert_eq!(git(repo, &["diff", "--name-only"]), "depot/src/ledger.rs");
    let lines = changed_lines(repo);
    assert!((280..=340).contains(&lines), "{lines} changed lines");
    let (runs, facts) = check_step(repo, &changes);
    assert!(runs <= 2);
    println!("{lines} changed lines, {} changes, {} facts, {runs} run(s)", changes.len(), facts.len());
}

/// One small edit in a large step must not hide. The function runs the step one time with no
/// needle and one time with each needle of a part. Only a fact of the item of the needle can
/// differ, and a fact shows the text of the needle. The needles are in three parts, so that
/// three tests run them at the same time: one test with each needle needs more than 60 s.
fn check_needles(part: usize) {
    let start = Instant::now();
    let (tree, _) = changed_tree();
    let dir = setup();
    let repo = &dir.path().join("repo");
    write_tree(repo, &tree);
    let step = |repo: &Path| -> BTreeSet<Fact> {
        let (code, output) = semilla(repo, &["step", "a large step"]);
        assert!(code == 0 || code == 1, "{output}");
        // An accepted step goes away again, so each run has the same base.
        if code == 0 {
            assert_eq!(semilla(repo, &["undo"]).0, 0);
        }
        facts(&output).into_iter().collect()
    };
    let plain = step(repo);
    let needles = needles();
    assert!(needles.len() >= 12);
    for needle in needles.iter().enumerate().filter(|(index, _)| index % 3 == part).map(|(_, needle)| needle) {
        let path = file(needle.module);
        let mut with = tree.clone();
        replace_one(&mut with, &path, &needle.from, &needle.to);
        std::fs::write(repo.join(&path), &with[&path]).unwrap();
        let found = step(repo);
        std::fs::write(repo.join(&path), &tree[&path]).unwrap();

        let new: Vec<&Fact> = found.difference(&plain).collect();
        let gone: Vec<&Fact> = plain.difference(&found).collect();
        assert!(new.iter().any(|fact| fact.text.contains(needle.shows) && fact.compare == "code different"), "the needle in {} needs a fact with `{}`:\nnew: {new:#?}\ngone: {gone:#?}", needle.name, needle.shows);
        for fact in new.iter().chain(&gone) {
            assert!(fact.text.contains(needle.item), "the needle in {} changes a fact of a different item:\nnew: {new:#?}\ngone: {gone:#?}", needle.name);
        }
        println!("the needle in {}: {}", needle.name, new.iter().map(|fact| format!("{} [{} · {}]", fact.text, fact.category, fact.compare)).collect::<Vec<_>>().join(" | "));
    }
    let time = start.elapsed().as_secs();
    assert!(time < 240, "the needles of part {part} need {time} s");
}

#[test]
fn a_needle_in_a_step_of_pr_size_shows_in_a_fact_and_changes_no_other_fact_part_1() {
    check_needles(0);
}

#[test]
fn a_needle_in_a_step_of_pr_size_shows_in_a_fact_and_changes_no_other_fact_part_2() {
    check_needles(1);
}

#[test]
fn a_needle_in_a_step_of_pr_size_shows_in_a_fact_and_changes_no_other_fact_part_3() {
    check_needles(2);
}
