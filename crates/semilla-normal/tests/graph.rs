//! The control flow graph of each body in the record of the driver (design 60, part A).
//!
//! One crate has a function for each kind of construct. The test reads the graph of each body
//! and compares its text: each point with its kind, its text, its mark, and its out arrows.

use std::path::{Path, PathBuf};
use std::process::Command;

use semilla_normal::{read_records, run_cargo_check};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Makes sure that a driver exists, and returns its path.
fn ensure_driver(scratch: &Path) -> PathBuf {
    if let Ok(path) = semilla_normal::cargo::find_driver() {
        return path;
    }
    let target = scratch.join("driver-target");
    let status = Command::new("cargo").args(["build", "-p", "semilla-mir", "--target-dir"]).arg(&target).current_dir(semilla_normal::cargo::semilla_workspace()).status().expect("cargo runs");
    assert!(status.success(), "cannot build the driver");
    target.join("debug/semilla-mir-driver")
}

/// The text of a graph: each point with its out arrows.
fn graph_text(graph: &semilla_normal::graph::RecordGraph) -> String {
    let mut parts = Vec::new();
    for (index, point) in graph.p.iter().enumerate() {
        let mut label = point.k.clone();
        if !point.t.is_empty() {
            label.push_str(&format!(" `{}`", point.t));
        }
        if let Some(mark) = &point.m {
            label.push_str(&format!(" [{mark}]"));
        }
        if !point.x.is_empty() {
            label.push_str(&format!(" {{{}}}", point.x.join(", ")));
        }
        let arrows: Vec<String> = graph.a.iter().filter(|(from, _, _)| *from as usize == index).map(|(_, to, name)| format!("{name}→{to}")).collect();
        parts.push(if arrows.is_empty() { format!("{index} {label}") } else { format!("{index} {label}: {}", arrows.join(", ")) });
    }
    parts.join("; ")
}

const SOURCE: &str = r##"pub enum Shape { Dot, Line(u32), Box { w: u32, h: u32 } }

pub fn price(q: u32) -> u32 { q * 3 }

pub fn f_if(x: u32) -> u32 {
    if x > 3 { price(x) } else { 0 }
}

pub fn f_match(s: &Shape) -> u32 {
    match s {
        Shape::Dot => 0,
        Shape::Line(n) => *n,
        Shape::Box { w, h } => w + h,
    }
}

pub fn f_while(mut n: u32) -> u32 {
    let mut s = 0;
    while n > 0 { s += n; n -= 1; }
    s
}

pub fn f_loop(v: &[u32]) -> u32 {
    let mut i = 0;
    let mut s = 0;
    loop {
        if i >= v.len() { break; }
        if v[i] == 7 { i += 1; continue; }
        s += v[i];
        i += 1;
    }
    s
}

pub fn f_try(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    let x = a?;
    let y = b?;
    Some(x + y)
}

pub fn f_for(v: &[u32]) -> u32 {
    let mut s = 0;
    for x in v.iter() { s += price(*x); }
    s
}

pub fn f_return(q: u32) -> Result<u32, String> {
    if q == 0 { return Err(String::new()); }
    Ok(price(q))
}

pub fn f_panic(q: u32) -> u32 {
    if q == 0 { panic!("zero"); }
    q
}

pub fn f_assert(q: u32) -> u32 {
    assert!(q > 1, "small");
    q
}

pub fn f_overflow(a: u32, b: u32) -> u32 {
    a + b
}

pub fn f_closure(v: u32) -> u32 {
    let add = |x: u32| x + v;
    add(2)
}

pub trait Speak { fn speak(&self) -> u32; }

pub fn f_dyn(s: &dyn Speak) -> u32 {
    s.speak()
}

pub fn f_ptr(f: fn(u32) -> u32) -> u32 { f(1) }

pub fn f_generic() -> Vec<u8> { let mut v = Vec::<u8>::new(); v.push(1); std::mem::take::<Vec<u8>>(&mut v) }

pub async fn f_async(x: u32) -> u32 { std::future::ready(x).await + 1 }

pub fn f_and(a: bool, b: bool) -> u32 { if a && b { 1 } else { 2 } }

pub fn f_iflet(o: Option<u32>) -> u32 { if let Some(x) = o { x } else { price(1) } }

pub fn f_whilelet(mut v: Vec<u32>) -> u32 { let mut s = 0; while let Some(x) = v.pop() { s ^= x; } s }

pub fn f_letelse(o: Option<u32>) -> u32 { let Some(x) = o else { return price(0) }; x }

pub fn f_label(n: u32) -> u32 { let mut c = 0; 'outer: for i in 0..n { for j in 0..n { if i == j { continue 'outer; } if j > 5 { break 'outer; } c ^= j; } } c }

pub fn f_guard(x: i32) -> i32 { match x { -1 => 0, v if v > 10 => price(1) as i32, _ => 2 } }

pub fn f_matches(c: char) -> bool { matches!(c, 'a' | 'b') }

pub fn f_assert_eq(a: u32, b: u32) { assert_eq!(a, b); }

pub fn f_div(a: u32, b: u32) -> u32 { a / b }

pub fn f_inf() -> ! { loop {} }

pub trait Tax { fn rate(&self) -> u32; fn base(&self) -> u32 { 1 } }
pub struct Food;
impl Tax for Food { fn rate(&self) -> u32 { 5 } }
pub fn f_impl() -> (u32, u32) { (Food.rate(), Food.base()) }
pub fn f_tgen<T: Tax>(t: &T) -> u32 { t.rate() }
pub fn f_parse(s: &str) -> Option<u64> { s.parse::<u64>().ok() }

pub struct Count(pub u32);
impl Iterator for Count { type Item = u32; fn next(&mut self) -> Option<u32> { None } }
pub fn f_count(n: u32) -> u32 { let mut s = 0; for x in Count(n) { s ^= x; } s }

pub fn f_todo() -> u32 { todo!() }
pub fn f_unreachable(x: u32) -> u32 { if x > 1 { unreachable!() } x }
pub fn f_expect(o: Option<u32>) -> u32 { o.expect("none") }
pub fn f_format(x: u32) -> String { format!("{x}") }
pub fn f_sum(v: &[u32]) -> u32 { v.iter().map(|x| x ^ 1).sum() }
"##;

/// The expected graph of each body.
const EXPECTED: &[(&str, &str)] = &[
        ("probe::f_if", r#"0 entry: →1; 1 branch `x > 3`: false→2, true→3; 2 return; 3 call `probe::price`: return→2"#),
        ("probe::f_match", r#"0 entry: →1; 1 branch `s`: probe::Shape::Dot→2, probe::Shape::Line→2, probe::Shape::Box→3; 2 return; 3 call `<&u32 as std::ops::Add>::add`: return→2"#),
        ("probe::f_while", r#"0 entry: →1; 1 loop `while`: →2; 2 branch `n > 0`: false→3, true→4; 3 return; 4 check `overflow: s += n`: ok→5, fail→6; 5 check `overflow: n -= 1`: ok→1, fail→7; 6 panic `overflow`; 7 panic `overflow`"#),
        ("probe::f_loop", r#"0 entry: →1; 1 loop `loop`: →2; 2 call `core::slice::<impl [u32]>::len`: return→3; 3 branch `i >= v.len()`: false→4, true→5; 4 check `bounds: v[i]`: ok→6, fail→7; 5 return; 6 branch `v[i] == 7`: false→8, true→9; 7 panic `bounds`; 8 check `bounds: v[i]`: ok→10, fail→11; 9 check `overflow: i += 1`: ok→1, fail→12; 10 check `overflow: s += v[i]`: ok→13, fail→14; 11 panic `bounds`; 12 panic `overflow`; 13 check `overflow: i += 1`: ok→1, fail→15; 14 panic `overflow`; 15 panic `overflow`"#),
        ("probe::f_try", r#"0 entry: →1; 1 try `a?` {<std::option::Option<u32> as std::ops::FromResidual<std::option::Option<!>>>::from_residual, <std::option::Option<u32> as std::ops::Try>::branch}: continue→2, exit→3; 2 try `b?` {<std::option::Option<u32> as std::ops::FromResidual<std::option::Option<!>>>::from_residual, <std::option::Option<u32> as std::ops::Try>::branch}: continue→4, exit→3; 3 return; 4 check `overflow: x + y`: ok→3, fail→5; 5 panic `overflow`"#),
        ("probe::f_for", r#"0 entry: →1; 1 call `core::slice::<impl [u32]>::iter`: return→2; 2 for `v.iter()` {<std::slice::Iter<'_, u32> as std::iter::IntoIterator>::into_iter, <std::slice::Iter<'_, u32> as std::iter::Iterator>::next}: done→3, next→4; 3 return; 4 call `probe::price`: return→5; 5 check `overflow: s += price(*x)`: ok→2, fail→6; 6 panic `overflow`"#),
        ("probe::f_return", r#"0 entry: →1; 1 branch `q == 0`: false→2, true→3; 2 call `probe::price`: return→4; 3 call `std::string::String::new`: return→4; 4 return"#),
        ("probe::f_panic", r#"0 entry: →1; 1 branch `q == 0`: false→2, true→3; 2 return; 3 panic `panic!("zero")` {std::fmt::Arguments::<'_>::from_str, std::rt::panic_fmt}"#),
        ("probe::f_assert", r#"0 entry: →1; 1 branch `q > 1`: false→2, true→3; 2 panic `assert!(q > 1, "small")` {std::fmt::Arguments::<'_>::from_str, std::rt::panic_fmt}; 3 return"#),
        ("probe::f_overflow", r#"0 entry: →1; 1 check `overflow: a + b`: ok→2, fail→3; 2 return; 3 panic `overflow`"#),
        ("probe::f_closure", r#"0 entry: →1; 1 call `add` [closure]: return→2; 2 return"#),
        ("probe::f_dyn", r#"0 entry: →1; 1 call `s.speak` [dyn]: return→2; 2 return"#),
        ("probe::f_ptr", r#"0 entry: →1; 1 call `f` [pointer]: return→2; 2 return"#),
        ("probe::f_iflet", r#"0 entry: →1; 1 branch `let Some(x) = o`: std::option::Option::Some→2, std::option::Option::None→3; 2 return; 3 call `probe::price`: return→2"#),
        ("probe::f_letelse", r#"0 entry: →1; 1 branch `let Some(x) = o`: std::option::Option::Some→2, std::option::Option::None→3; 2 return; 3 call `probe::price`: return→2"#),
        ("probe::f_label", r#"0 entry: →1; 1 for `0..n` {<std::ops::Range<u32> as std::iter::IntoIterator>::into_iter, std::iter::range::<impl std::iter::Iterator for std::ops::Range<u32>>::next}: done→2, next→3; 2 return; 3 for `0..n` {<std::ops::Range<u32> as std::iter::IntoIterator>::into_iter, std::iter::range::<impl std::iter::Iterator for std::ops::Range<u32>>::next}: done→1, next→4; 4 branch `i == j`: false→5, true→1; 5 branch `j > 5`: false→3, true→2"#),
        ("probe::f_guard", r#"0 entry: →1; 1 branch `x`: -1→2, otherwise→3; 2 return; 3 branch `v > 10`: false→2, true→4; 4 call `probe::price`: return→2"#),
        ("probe::f_inf", r#"0 entry: →1; 1 loop `loop`: →1"#),
        ("probe::f_async", r#"0 entry: →1; 1 return"#),
        ("probe::f_assert_eq", r#"0 entry: →1; 1 branch `assert_eq!(a, b)`: false→2, true→3; 2 panic `assert_eq!(a, b)` {core::panicking::assert_failed::<u32, u32>}; 3 return"#),
        ("probe::f_impl", r#"0 entry: →1; 1 call `<probe::Food as probe::Tax>::rate`: return→2; 2 call `<probe::Food as probe::Tax>::base (trait default)`: return→3; 3 return"#),
        ("probe::f_tgen", r#"0 entry: →1; 1 call `<T as probe::Tax>::rate`: return→2; 2 return"#),
        ("probe::f_parse", r#"0 entry: →1; 1 call `core::str::<impl str>::parse::<u64>`: return→2; 2 call `std::result::Result::<u64, std::num::ParseIntError>::ok`: return→3; 3 return"#),
        ("probe::f_count", r#"0 entry: →1; 1 for `Count(n)` {<probe::Count as std::iter::IntoIterator>::into_iter, <probe::Count as std::iter::Iterator>::next}: done→2, next→2; 2 return"#),
        ("probe::f_todo", r#"0 entry: →1; 1 panic `todo!()` {core::panicking::todo_default_msg}"#),
        ("probe::f_unreachable", r#"0 entry: →1; 1 branch `x > 1`: false→2, true→3; 2 return; 3 panic `unreachable!()` {core::panicking::unreachable_default_msg}"#),
        ("probe::f_expect", r#"0 entry: →1; 1 call `std::option::Option::<u32>::expect`: return→2; 2 return"#),
        ("probe::f_format", r#"0 entry: →1; 1 construct `{x}` {core::fmt::rt::Argument::<'_>::new_display::<u32>, std::fmt::Arguments::<'_>::new::<2, 1>}: return→2; 2 call `std::fmt::format`: return→3; 3 call `std::hint::must_use::<std::string::String>`: return→4; 4 return"#),
        ("probe::f_sum", r#"0 entry: →1; 1 call `core::slice::<impl [u32]>::iter`: return→2; 2 call `<std::slice::Iter<'_, u32> as std::iter::Iterator>::map::<u32, probe::f_sum::{closure#0}> (trait default)`: return→3; 3 call `<std::iter::Map<std::slice::Iter<'_, u32>, probe::f_sum::{closure#0}> as std::iter::Iterator>::sum::<u32> (trait default)`: return→4; 4 return"#),
];

/// The graph texts of the records of a folder, by the body name.
fn graph_lines(out: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(out).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            if let Some(graph) = value.get("g") {
                found.push((value["name"].to_string(), graph.to_string()));
            }
        }
    }
    found.sort();
    found
}

// rule+ R60.2-1 R60.2-2 R60.2-3 R60.2-4 R60.2-6 R60.2-7 R60.2-8 R60.2-9 R60.2-10 R60.2-11 R60.2-12 R76.2-1 R76.2-2
// rule- R60.2-1 R60.2-2 R60.2-3 R60.2-4 R60.2-7 R60.2-8 R60.2-9 R60.2-10 R60.2-11 R60.2-12 R76.2-1
#[test]
fn each_construct_gives_its_points_and_its_arrows_and_two_runs_give_the_same_graph() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-normal-graph");
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    let root = scratch.join("ws");
    let _ = std::fs::remove_dir_all(&root);
    write(&root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&root.join("src/lib.rs"), SOURCE);
    let mut texts = Vec::new();
    for run in ["one", "two"] {
        let out = scratch.join(format!("records-{run}"));
        let _ = std::fs::remove_dir_all(&out);
        run_cargo_check(&root, "n0", &out, &scratch.join(format!("target-{run}"))).unwrap_or_else(|error| panic!("{error:#}"));
        let records = read_records(&out).unwrap();
        let graph = |name: &str| records.iter().find(|record| record.name == name && record.level == "n0").and_then(|record| record.graph.clone()).unwrap_or_else(|| panic!("no graph of {name}"));
        // R60.2-1 to R60.2-3 and R60.2-7 to R60.2-9: the points, the texts, the marks, and
        // the labels. The negative cases are in the list: the `match` keyword is not in the
        // text of a scrutinee (`s`); a `while` loop is a loop point and a branch, not a
        // construct; a body with no back edge has no loop point (`f_if`); an `assert!` is a
        // branch and a panic, not a check; a call of a known function has no mark
        // (`probe::price`).
        // R60.2-11: the text of a call is the callee that the MIR calls, with each generic
        // argument: the impl item (`<probe::Food as probe::Tax>::rate`), a provided method of
        // the trait (`(trait default)`), the arguments that the source does not show
        // (`Vec::<u8>::push`), a closure type by the name of its body (`f_sum`), the callees of
        // a construct (`f_try`, `f_for`, `f_count`). The negative case: a generic body keeps the
        // parameter (`<T as probe::Tax>::rate`).
        // R60.2-12: the points of the expansion of `panic!`, `assert!`, `assert_eq!`,
        // `unreachable!`, and `todo!` that lead only to a panic are one `panic` point with the
        // text of the macro call. The negative cases: the branch `q > 1` of `assert!` stays a
        // point; `expect` is a call; `format!` keeps its points (a construct for `{x}` and the
        // calls `format` and `must_use`), because they do not lead to a panic; `matches!` keeps
        // its branch.
        let wrong: Vec<String> = EXPECTED.iter().filter(|(name, expected)| graph_text(&graph(name)) != *expected).map(|(name, expected)| format!("{name}\n  have: {}\n  want: {expected}", graph_text(&graph(name)))).collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        assert!(graph_text(&graph("probe::f_generic")).contains("call `std::vec::Vec::<u8>::new`: return→2; 2 call `std::vec::Vec::<u8>::push`: return→3; 3 call `std::mem::take::<std::vec::Vec<u8>>`"));
        // R60.2-2, the negative case: the call `v.iter()` before a `for` is its own point. The
        // `for` point has the text of the head of the loop.
        assert!(graph_text(&graph("probe::f_for")).starts_with("0 entry: →1; 1 call `core::slice::<impl [u32]>::iter`: return→2; 2 for `v.iter()`"));
        // R60.2-4: an unwind arrow and a cleanup block are not points; the record counts them.
        // R100.6-1 (design 100): a drop is a point now, and the record still counts it. The
        // negative case: a body with no drop has no count.
        let generic = graph("probe::f_generic");
        assert_eq!((generic.n.drops, generic.n.unwind, generic.n.cleanup), (1, 3, 2), "{generic:?}");
        assert_eq!(generic.p.iter().filter(|point| point.k == "drop").count(), 1, "{generic:?}");
        assert_eq!(graph("probe::f_if").n, semilla_normal::graph::GraphCounts::default());
        // R76.2-1 (before batch 12b, R60.2-5: only the entry): the body of a coroutine has the
        // graph of its MIR before the coroutine transform. R76.2-2: the `.await` is one point
        // `await` (its two expansions, the expression and the keyword, are one construct), with
        // the text of the expression, the callees of its parts, and one arrow `done` (the poll
        // loop is inside the point). The negative case: the `async fn` itself (the function
        // that makes the future) keeps its own graph, and no record has the mark `co`.
        let body = graph("probe::f_async::{closure#0}");
        assert_eq!(graph_text(&body), "0 entry: →1; 1 call `std::future::ready::<u32>`: return→2; 2 await `std::future::ready(x).await` {<std::future::Ready<u32> as std::future::Future>::poll, <std::future::Ready<u32> as std::future::IntoFuture>::into_future, std::future::get_context::<'_, '_>, std::pin::Pin::<&mut std::future::Ready<u32>>::new_unchecked}: done→3; 3 check `overflow: std::future::ready(x).await + 1`: ok→4, fail→5; 4 return; 5 panic `overflow`");
        assert!(!body.co && body.p[0].r.is_some(), "{body:?}");
        assert!(!graph("probe::f_async").co);
        // R60.2-10: a point from a macro has the range of the macro call; a point of a
        // desugaring has no macro call.
        let text = |place: Option<semilla_normal::elements::Place>| place.map(|place| SOURCE[place.start..place.end].to_string());
        let panic = graph("probe::f_panic");
        let call = panic.p.iter().find(|point| point.k == "panic").unwrap();
        assert_eq!(text(panic.place(&call.c)).as_deref(), Some("panic!(\"zero\")"));
        let tried = graph("probe::f_try");
        let first = tried.p.iter().find(|point| point.k == "try").unwrap();
        assert_eq!((text(tried.place(&first.r)).as_deref(), first.c), (Some("a?"), None));
        // R60.2-6: no block number and no local number in a text.
        for record in records.iter().filter(|record| record.level == "n0") {
            for point in record.graph.iter().flat_map(|graph| graph.p.iter()) {
                let local = point.t.split(|c: char| !c.is_alphanumeric() && c != '_').any(|word| word.starts_with('_') && word[1..].parse::<u32>().is_ok());
                assert!(!point.t.contains("bb") && !local, "{point:?}");
            }
        }
        texts.push(graph_lines(&out));
    }
    // R60.2-6: two runs give the same graph text.
    assert!(!texts[0].is_empty());
    assert_eq!(texts[0], texts[1]);
    // R76.2-1: a build that takes the unchanged coroutine body from its incremental data does
    // not run the query override; the driver copies the body of `mir_promoted` at the read, so
    // the body keeps its graph. The third build uses the target folder of the second one, with
    // a new function in the crate.
    write(&root.join("src/lib.rs"), &format!("{SOURCE}\npub fn f_added(x: u32) -> u32 {{ x + 7 }}\n"));
    let out = scratch.join("records-three");
    let _ = std::fs::remove_dir_all(&out);
    run_cargo_check(&root, "n0", &out, &scratch.join("target-two")).unwrap_or_else(|error| panic!("{error:#}"));
    let after: Vec<(String, String)> = graph_lines(&out).into_iter().filter(|(name, _)| !name.contains("f_added")).collect();
    let coroutine = |list: &[(String, String)]| list.iter().find(|(name, _)| name.contains("f_async::{closure#0}")).cloned();
    assert!(coroutine(&after).is_some_and(|(_, graph)| graph.contains("\"await\"") && !graph.contains("\"co\"")), "{:?}", coroutine(&after));
    assert_eq!(coroutine(&after), coroutine(&texts[1]));
    let _ = std::fs::remove_dir_all(&scratch);
}
