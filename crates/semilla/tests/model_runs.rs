//! [R75.2-1] A run checks each model rule that keeps a name (design 75.2, G5). The model of the
//! check keeps some rules that decide by a name (the table of design 69.2): the intrinsics, the
//! sequence model of `Vec` and of slices, the drop of a `Vec`, a `String`, and a `Box`, the types
//! of the standard library that the model has, and the facts of the configuration check. The
//! compiler data cannot replace them, so this test checks each one by runs: it makes small
//! bodies that use the named part, and for each input of a list (fixed seeds and the edges: an
//! empty sequence, one element, the longest array of the model, `u32::MAX`, a panic) it makes a
//! new version that differs from the old version only at that input. The model must find the
//! difference at that input with the outcome of the old version; the step gives the suggested
//! test (design 75), and this test adds it to the old and to the new tree and runs `cargo
//! test`: the test must pass on the old code and fail on the new code. A test that fails on the
//! old code is a defect of the model.
//!
//! This is a test of the test suite, on generated bodies, never on the code of a user: Semilla
//! itself never runs a test or a probe.

mod common;
mod suggested;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

/// The output of `semilla step --compiled` for an old and a new `src/lib.rs`.
fn step(dir: &Path, old: &str, new: &str, env: &[(&str, &str)]) -> String {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), suggested::MANIFEST).unwrap();
    std::fs::write(repo.join("src/lib.rs"), old).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "old"]);
    std::fs::write(repo.join("src/lib.rs"), new).unwrap();
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(&repo).args(["step", "the change"]);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", dir.join("cache")).env_remove("SEMILLA_COMPILED").env_remove("SEMILLA_TEST_RULE_OFF");
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    command.envs(env.iter().copied());
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.code().is_some_and(|code| code <= 1), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    stdout
}

/// One input of a body: a Rust condition on the parameters, and true if the body panics there
/// (the new version then returns a fixed value at that input; else it panics there with its
/// own message).
#[derive(Clone)]
struct Point {
    when: String,
    panics: bool,
}

/// A body that uses one entry of a list of the model, with its inputs.
struct Shape {
    list: &'static str,
    entry: &'static str,
    params: &'static str,
    ret: &'static str,
    body: &'static str,
    points: Vec<Point>,
}

fn at(when: impl Into<String>) -> Point {
    Point { when: when.into(), panics: false }
}

fn panics(when: impl Into<String>) -> Point {
    Point { when: when.into(), panics: true }
}

/// A fixed sequence of numbers (xorshift64*): the same inputs in each run of the test.
struct Seeds(u64);

impl Seeds {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// The edges and seeded values of an integer type, as Rust literals.
fn values(ty: &str, seeds: &mut Seeds, count: usize) -> Vec<String> {
    let (bits, signed) = match ty {
        "u8" => (8, false),
        "u16" => (16, false),
        "u32" => (32, false),
        "u64" | "usize" => (64, false),
        "i8" => (8, true),
        "i16" => (16, true),
        "i32" => (32, true),
        "i64" => (64, true),
        "i128" => (128, true),
        other => panic!("no values of {other}"),
    };
    let mut out: Vec<String> = if signed { vec![format!("{ty}::MIN"), "-1".into(), "0".into(), format!("{ty}::MAX")] } else { vec!["0".into(), "1".into(), format!("{ty}::MAX")] };
    for _ in 0..count {
        let raw = seeds.next();
        let text = if signed {
            let value = (raw as i64) >> (64 - bits.min(64));
            value.to_string()
        } else if bits == 64 {
            raw.to_string()
        } else {
            (raw >> (64 - bits)).to_string()
        };
        out.push(text);
    }
    out
}

/// The inputs of a body with integer parameters: each pair of edges and seeded values.
fn int_points(params: &[(&str, &str)], seed: u64) -> Vec<Point> {
    let mut seeds = Seeds(seed);
    let lists: Vec<Vec<String>> = params.iter().map(|(_, ty)| values(ty, &mut seeds, 3)).collect();
    let count = lists.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::new();
    for index in 0..count {
        let parts: Vec<String> = params.iter().zip(&lists).enumerate().map(|(at_param, ((name, _), list))| format!("{name} == {}", list[(index + at_param * 2) % list.len()])).collect();
        out.push(at(parts.join(" && ")));
    }
    out
}

/// A condition that a sequence parameter has exactly these elements.
fn seq(name: &str, items: &[&str]) -> String {
    let mut parts = vec![format!("{name}.len() == {}", items.len())];
    parts.extend(items.iter().enumerate().map(|(index, item)| format!("{name}[{index}] == {item}")));
    parts.join(" && ")
}

fn shapes() -> Vec<Shape> {
    let mut out = Vec::new();
    let mut add = |list: &'static str, entry: &'static str, params: &'static str, ret: &'static str, body: &'static str, points: Vec<Point>| out.push(Shape { list, entry, params, ret, body, points });
    let ints = |spec: &[(&str, &str)], seed: u64| int_points(spec, seed);

    // The intrinsics (`PURE_INTRINSICS` of the effect summary, `intrinsic()` of the check).
    const I: &str = "intrinsics";
    add(I, "wrapping_add", "x: u32, y: u32", "u32", "x.wrapping_add(y)", ints(&[("x", "u32"), ("y", "u32")], 1));
    add(I, "wrapping_sub", "x: i32, y: i32", "i32", "x.wrapping_sub(y)", ints(&[("x", "i32"), ("y", "i32")], 2));
    add(I, "wrapping_mul", "x: u8, y: u8", "u8", "x.wrapping_mul(y)", ints(&[("x", "u8"), ("y", "u8")], 3));
    add(I, "add_with_overflow", "x: u32, y: u32", "(u32, bool)", "x.overflowing_add(y)", ints(&[("x", "u32"), ("y", "u32")], 4));
    add(I, "sub_with_overflow", "x: i16, y: i16", "(i16, bool)", "x.overflowing_sub(y)", ints(&[("x", "i16"), ("y", "i16")], 5));
    add(I, "mul_with_overflow", "x: u64, y: u64", "(u64, bool)", "x.overflowing_mul(y)", ints(&[("x", "u64"), ("y", "u64")], 6));
    add(I, "saturating_add", "x: u32, y: u32", "u32", "x.saturating_add(y)", ints(&[("x", "u32"), ("y", "u32")], 7));
    add(I, "saturating_sub", "x: i32, y: i32", "i32", "x.saturating_sub(y)", ints(&[("x", "i32"), ("y", "i32")], 8));
    add(I, "unchecked_add", "x: u32, y: u32", "u32", "unsafe { (x / 2).unchecked_add(y / 2) }", ints(&[("x", "u32"), ("y", "u32")], 9));
    add(I, "unchecked_sub", "x: u32, y: u32", "u32", "unsafe { x.max(y).unchecked_sub(x.min(y)) }", ints(&[("x", "u32"), ("y", "u32")], 10));
    add(I, "unchecked_mul", "x: u16, y: u16", "u32", "unsafe { (x as u32).unchecked_mul(y as u32) }", ints(&[("x", "u16"), ("y", "u16")], 11));
    let mut rotate = ints(&[("x", "u32")], 12);
    for (index, amount) in ["0", "1", "31", "32", "33", "u32::MAX"].iter().enumerate() {
        let count = rotate.len();
        rotate[index % count].when.push_str(&format!(" && n == {amount}"));
    }
    rotate.retain(|point| point.when.contains("n =="));
    add(I, "rotate_left", "x: u32, n: u32", "u32", "x.rotate_left(n)", rotate.clone());
    add(I, "rotate_right", "x: u32, n: u32", "u32", "x.rotate_right(n)", rotate);
    add(I, "integer_min", "x: i32, y: i32", "i32", "x.min(y)", ints(&[("x", "i32"), ("y", "i32")], 13));
    add(I, "integer_max", "x: u64, y: u64", "u64", "x.max(y)", ints(&[("x", "u64"), ("y", "u64")], 14));
    add(I, "black_box", "x: u32", "u32", "std::hint::black_box(x)", ints(&[("x", "u32")], 15));
    add(I, "likely, unlikely", "x: u32, y: u32", "Option<u32>", "x.checked_add(y)", ints(&[("x", "u32"), ("y", "u32")], 16));
    add(I, "likely, unlikely", "x: u8, y: u8", "Option<u8>", "x.checked_mul(y)", ints(&[("x", "u8"), ("y", "u8")], 17));
    add(I, "three_way_compare", "x: i32, y: i32", "std::cmp::Ordering", "x.cmp(&y)", ints(&[("x", "i32"), ("y", "i32")], 18));
    add(I, "ctpop", "x: u32", "u32", "x.count_ones()", ints(&[("x", "u32")], 19));
    add(I, "ctlz", "x: u64", "u32", "x.leading_zeros()", ints(&[("x", "u64")], 20));
    add(I, "cttz", "x: u16", "u32", "x.trailing_zeros()", ints(&[("x", "u16")], 21));
    add(I, "bswap", "x: u32", "u32", "x.swap_bytes()", ints(&[("x", "u32")], 22));
    add(I, "bitreverse", "x: u8", "u8", "x.reverse_bits()", ints(&[("x", "u8")], 23));
    add(I, "ctlz_nonzero", "x: u32", "u32", "match std::num::NonZeroU32::new(x) { Some(n) => n.leading_zeros(), None => 99 }", ints(&[("x", "u32")], 24));
    add(I, "size_of, align_of", "x: u32", "usize", "std::mem::size_of::<(u8, u32)>() + std::mem::align_of::<u64>() + x as usize", ints(&[("x", "u32")], 25));
    add(I, "needs_drop", "x: u32", "bool", "std::mem::needs_drop::<String>() && x > 3", ints(&[("x", "u32")], 26));
    add(I, "assume", "x: u32", "u32", "unsafe { std::hint::assert_unchecked(x < 1000) }; x + 1", vec![at("x == 0"), at("x == 999"), at("x == 512")]);

    // The drop of a `Vec`, a `String`, and a `Box` of the global allocator (`drop_is_free`).
    const D: &str = "drop_is_free";
    add(D, "Vec<u32>", "x: u32", "u32", "let mut v = Vec::new(); v.push(x); let n = v.len() as u32; drop(v); n.wrapping_add(x)", ints(&[("x", "u32")], 30));
    add(D, "Vec<u8>", "x: u8", "usize", "let mut v: Vec<u8> = Vec::new(); v.extend_from_slice(&[x, x]); let n = v.len(); drop(v); n + x as usize", ints(&[("x", "u8")], 31));
    add(D, "String", "x: u32", "u32", "let s = String::new(); drop(s); x.wrapping_mul(3)", ints(&[("x", "u32")], 32));
    add(D, "Vec<u32> input", "v: Vec<u32>, x: u32", "u32", "drop(v); x ^ 5", vec![at(format!("{} && x == 1", seq("v", &[]))), at(format!("{} && x == u32::MAX", seq("v", &["7", "8"])))]);
    add(D, "Box<u32> input", "b: Box<u32>, x: u32", "u32", "drop(b); x", vec![at("*b == 3 && x == 4")]);

    // The sequence model of `Vec` and of slices (`seq_model`, `scalar_vec`).
    const S: &str = "sequence model";
    let long: Vec<String> = (0..64).map(|index| (index * 3).to_string()).collect();
    let long: Vec<&str> = long.iter().map(String::as_str).collect();
    add(S, "vec_new, vec_push", "x: u32", "Vec<u32>", "let mut v = Vec::new(); v.push(x); v", ints(&[("x", "u32")], 40));
    add(S, "vec_with_capacity", "n: usize", "Vec<u32>", "Vec::with_capacity(n)", vec![at("n == 0"), at("n == 1"), at("n == 64"), at("n == 4294967295"), at("n == 1152921504606846976"), panics("n == 2305843009213693952"), panics("n == usize::MAX")]);
    add(S, "vec_push", "v: &mut Vec<u32>, x: u32", "()", "v.push(x)", vec![at(format!("{} && x == u32::MAX", seq("v", &[]))), at(format!("{} && x == 0", seq("v", &["1"]))), at(format!("{} && x == 1", seq("v", &["u32::MAX", "u32::MAX"]))), at(format!("{} && x == 6", seq("v", &long)))]);
    add(S, "vec_pop", "v: &mut Vec<u32>", "Option<u32>", "v.pop()", vec![at(seq("v", &[])), at(seq("v", &["7"])), at(seq("v", &["1", "2"])), at(seq("v", &["u32::MAX"]))]);
    add(S, "vec_len", "v: &Vec<u8>", "usize", "v.len()", vec![at(seq("v", &[])), at(seq("v", &["0"])), at(seq("v", &["1", "2", "3"])), at(seq("v", &long))]);
    add(S, "vec_is_empty", "v: &Vec<u32>", "bool", "v.is_empty()", vec![at(seq("v", &[])), at(seq("v", &["1"]))]);
    add(S, "vec_clear", "v: &mut Vec<u32>", "()", "v.clear()", vec![at(seq("v", &[])), at(seq("v", &["1", "2"]))]);
    add(S, "vec_truncate", "v: &mut Vec<u32>, k: usize", "()", "v.truncate(k)", vec![at(format!("{} && k == 0", seq("v", &["1", "2", "3"]))), at(format!("{} && k == 2", seq("v", &["1", "2", "3"]))), at(format!("{} && k == 3", seq("v", &["1", "2", "3"]))), at(format!("{} && k == 4", seq("v", &["1", "2", "3"]))), at(format!("{} && k == usize::MAX", seq("v", &["1"])))]);
    add(S, "vec_insert", "v: &mut Vec<u32>, i: usize, x: u32", "()", "v.insert(i, x)", vec![at(format!("{} && i == 0 && x == 5", seq("v", &[]))), at(format!("{} && i == 1 && x == 9", seq("v", &["1", "2"]))), at(format!("{} && i == 2 && x == u32::MAX", seq("v", &["1", "2"]))), panics(format!("{} && i == 3 && x == 9", seq("v", &["1", "2"])))]);
    add(S, "vec_remove", "v: &mut Vec<u32>, i: usize", "u32", "v.remove(i)", vec![at(format!("{} && i == 0", seq("v", &["4"]))), at(format!("{} && i == 2", seq("v", &["1", "2", "3"]))), at(format!("{} && i == 1", seq("v", &["1", "2", "3"]))), panics(format!("{} && i == 2", seq("v", &["1", "2"]))), panics(format!("{} && i == 0", seq("v", &[])))]);
    add(S, "vec_extend_from_slice", "v: &mut Vec<u32>, w: &[u32]", "()", "v.extend_from_slice(w)", vec![at(format!("{} && {}", seq("v", &[]), seq("w", &[]))), at(format!("{} && {}", seq("v", &["1"]), seq("w", &["2", "3"]))), at(format!("{} && {}", seq("v", &["u32::MAX"]), seq("w", &["0"])))]);
    add(S, "vec_deref", "v: &Vec<u32>", "usize", "let s: &[u32] = v; s.len()", vec![at(seq("v", &[])), at(seq("v", &["1", "2"]))]);
    add(S, "vec_index", "v: &Vec<u32>, i: usize", "u32", "v[i]", vec![at(format!("{} && i == 0", seq("v", &["5"]))), at(format!("{} && i == 2", seq("v", &["1", "2", "3"]))), panics(format!("{} && i == 2", seq("v", &["1", "2"]))), panics(format!("{} && i == 0", seq("v", &[]))), panics(format!("{} && i == usize::MAX", seq("v", &["1"])))]);
    add(S, "vec_index (a write)", "v: &mut Vec<u32>, i: usize, x: u32", "()", "v[i] = x", vec![at(format!("{} && i == 0 && x == 1", seq("v", &["5"]))), at(format!("{} && i == 1 && x == u32::MAX", seq("v", &["1", "2"]))), panics(format!("{} && i == 1 && x == 2", seq("v", &["1"])))]);
    add(S, "slice_len", "s: &[u16]", "usize", "s.len()", vec![at(seq("s", &[])), at(seq("s", &["u16::MAX"])), at(seq("s", &["1", "2", "3"]))]);
    add(S, "slice_is_empty", "s: &[u32]", "bool", "s.is_empty()", vec![at(seq("s", &[])), at(seq("s", &["0"]))]);
    add(S, "slice_swap", "v: &mut Vec<u32>, a: usize, b: usize", "()", "v.swap(a, b)", vec![at(format!("{} && a == 0 && b == 1", seq("v", &["1", "2"]))), at(format!("{} && a == 2 && b == 2", seq("v", &["1", "2", "3"]))), panics(format!("{} && a == 0 && b == 1", seq("v", &["1"])))]);
    add(S, "slice_first", "s: &[u32]", "Option<u32>", "s.first().copied()", vec![at(seq("s", &[])), at(seq("s", &["9"])), at(seq("s", &["1", "2"]))]);
    add(S, "slice_last", "s: &[i8]", "Option<i8>", "s.last().copied()", vec![at(seq("s", &[])), at(seq("s", &["i8::MIN"])), at(seq("s", &["1", "-2"]))]);
    add(S, "slice_get", "s: &[u32], i: usize", "Option<u32>", "s.get(i).copied()", vec![at(format!("{} && i == 1", seq("s", &["1", "2"]))), at(format!("{} && i == 2", seq("s", &["1", "2"]))), at(format!("{} && i == 0", seq("s", &[]))), at(format!("{} && i == usize::MAX", seq("s", &["1"])))]);
    add(S, "slice index", "s: &[u32], i: usize", "u32", "s[i]", vec![at(format!("{} && i == 0", seq("s", &["3"]))), panics(format!("{} && i == 1", seq("s", &["3"])))]);
    add(S, "array value", "x: u8, i: usize", "u8", "let a = [x; 64]; a[i]", vec![at("x == 7 && i == 0"), at("x == u8::MAX && i == 63"), panics("x == 7 && i == 64"), panics("x == 1 && i == usize::MAX")]);
    add(S, "array literal", "i: usize", "u32", "[10, 20, 30][i]", vec![at("i == 0"), at("i == 2"), panics("i == 3")]);
    add(S, "constant index", "s: &[u32]", "u32", "if s.len() > 2 { s[2] } else { 0 }", vec![at(seq("s", &["1", "2", "3"])), at(seq("s", &["1"]))]);
    add(S, "vec_push (bool)", "v: &mut Vec<bool>, b: bool", "()", "v.push(b)", vec![at(format!("{} && b", seq("v", &[]))), at(format!("{} && !b", seq("v", &["true"])))]);
    add(S, "slice_first (char)", "v: &Vec<char>", "Option<char>", "v.first().copied()", vec![at(seq("v", &[])), at(seq("v", &["'a'"])), at(seq("v", &["'\\u{10ffff}'"]))]);

    // The types of the standard library that the model has (`modeled`).
    const M: &str = "modeled types";
    add(M, "Option", "x: Option<u32>", "u32", "x.unwrap_or(7)", vec![at("x.is_none()"), at("matches!(x, Some(0))"), at("matches!(x, Some(u32::MAX))")]);
    add(M, "Option", "x: Option<u32>", "Option<u32>", "x.map(|v| v.wrapping_add(1))", vec![at("x.is_none()"), at("matches!(x, Some(u32::MAX))"), at("matches!(x, Some(5))")]);
    add(M, "Option", "x: Option<u8>", "bool", "x.is_some()", vec![at("x.is_none()"), at("matches!(x, Some(u8::MAX))")]);
    add(M, "Option", "x: Option<u32>", "Result<u32, u8>", "x.ok_or(3)", vec![at("x.is_none()"), at("matches!(x, Some(9))")]);
    add(M, "Option", "x: Option<u32>, y: Option<u32>", "Option<u32>", "Some(x?.wrapping_add(y?))", vec![at("x.is_none() && matches!(y, Some(1))"), at("matches!(x, Some(u32::MAX)) && matches!(y, Some(2))"), at("matches!(x, Some(3)) && y.is_none()")]);
    add(M, "Option", "x: &mut Option<u32>", "Option<u32>", "x.take()", vec![at("x.is_none()"), at("matches!(*x, Some(4))")]);
    add(M, "Result", "r: Result<u32, u8>", "Option<u32>", "r.ok()", vec![at("matches!(r, Ok(u32::MAX))"), at("matches!(r, Err(0))")]);
    add(M, "Result", "r: Result<i32, bool>", "i32", "r.unwrap_or(-1)", vec![at("matches!(r, Ok(i32::MIN))"), at("matches!(r, Err(true))")]);
    add(M, "Result", "r: Result<u32, u8>", "Result<u32, u16>", "r.map_err(|e| e as u16 + 1)", vec![at("matches!(r, Ok(6))"), at("matches!(r, Err(u8::MAX))")]);
    add(M, "Result", "r: Result<u32, u8>", "Result<u32, u8>", "Ok(r? / 2)", vec![at("matches!(r, Ok(u32::MAX))"), at("matches!(r, Err(2))")]);
    add(M, "Ordering", "x: u32, y: u32", "std::cmp::Ordering", "x.cmp(&y).reverse()", ints(&[("x", "u32"), ("y", "u32")], 50));
    add(M, "Ordering", "x: i32, y: i32", "bool", "x.cmp(&y).is_lt()", ints(&[("x", "i32"), ("y", "i32")], 51));
    add(M, "Ordering", "a: u8, b: u8, c: u8, d: u8", "std::cmp::Ordering", "a.cmp(&b).then(c.cmp(&d))", vec![at("a == 1 && b == 1 && c == 2 && d == 3"), at("a == 0 && b == u8::MAX && c == 2 && d == 1"), at("a == 5 && b == 5 && c == 5 && d == 5")]);
    add(M, "ControlFlow", "x: u32", "std::ops::ControlFlow<u32>", "if x > 9 { std::ops::ControlFlow::Break(x) } else { std::ops::ControlFlow::Continue(()) }", ints(&[("x", "u32")], 52));
    add(M, "ControlFlow", "x: u32", "Option<u32>", "let c: std::ops::ControlFlow<u32, ()> = if x % 2 == 0 { std::ops::ControlFlow::Break(x) } else { std::ops::ControlFlow::Continue(()) }; c.break_value()", ints(&[("x", "u32")], 53));
    add(M, "Range", "a: u32, b: u32, x: u32", "bool", "(a..b).contains(&x)", vec![at("a == 1 && b == 5 && x == 4"), at("a == 1 && b == 5 && x == 5"), at("a == 0 && b == u32::MAX && x == 0"), at("a == 3 && b == 1 && x == 2")]);
    add(M, "Range", "a: u32, b: u32", "bool", "(a..b).is_empty()", vec![at("a == 1 && b == 1"), at("a == 0 && b == u32::MAX"), at("a == u32::MAX && b == 0")]);
    add(M, "Range", "a: u32, b: u32", "u32", "let r = a..b; r.end.wrapping_sub(r.start)", ints(&[("a", "u32"), ("b", "u32")], 54));
    add(M, "Range", "a: u8", "std::ops::Range<u8>", "a..a.wrapping_add(3)", vec![at("a == 0"), at("a == u8::MAX")]);

    // The facts of the configuration check (`is_fixed_int_const`, `FREE_ALIASES`,
    // `PURE_BUILTIN`, `NOT_CONFIG`): one value in each configuration.
    const C: &str = "configuration facts";
    add(C, "is_fixed_int_const", "x: u32", "u32", "(u8::MAX as u32).wrapping_add(x)", ints(&[("x", "u32")], 60));
    add(C, "is_fixed_int_const", "x: i64", "i64", "i64::MIN.wrapping_add(x)", ints(&[("x", "i64")], 61));
    add(C, "is_fixed_int_const", "x: u32", "u32", "u64::BITS + x % 7", ints(&[("x", "u32")], 62));
    add(C, "is_fixed_int_const", "x: i128", "i128", "i128::MAX ^ x", vec![at("x == 0"), at("x == -1"), at("x == i128::MIN")]);
    add(C, "FREE_ALIASES", "x: u32", "std::fmt::Result", "if x > 3 { Ok(()) } else { Err(std::fmt::Error) }", vec![at("x == 0"), at("x == u32::MAX")]);
    add(C, "NOT_CONFIG", "x: u32", "u32", "line!().wrapping_add(x)", ints(&[("x", "u32")], 63));
    add(C, "NOT_CONFIG", "x: u32", "u32", "column!() + x % 3", ints(&[("x", "u32")], 64));
    add(C, "PURE_BUILTIN", "x: u32", "usize", "stringify!(a + b).len() + x as usize", vec![at("x == 0"), at("x == u32::MAX")]);
    add(C, "PURE_BUILTIN", "x: u32", "usize", "concat!(\"ab\", 1).len() + (x % 2) as usize", vec![at("x == 0"), at("x == 1")]);
    out
}

/// The result of one body.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Got {
    /// The suggested test asserts a value or a message, passes on the old code, and fails on
    /// the new code.
    Confirmed,
    /// The suggested test asserts the exit only (the model does not give the value), passes on
    /// the old code, and fails on the new code.
    Exit,
    /// No suggested test (the cause).
    NoTest(String),
    /// The model gives no sure difference (the cause): no test.
    NoModel(String),
    /// The suggested test fails on the old code: a defect of the model.
    Defect(String),
    /// The model says "equivalent", or the suggested test passes on the new code: a defect.
    Missed(String),
}

/// The class of the line of a body before the runs of the tests: the name of its test, or the
/// result.
fn classify(line: &str) -> Result<String, Got> {
    let cause = |marker: &str| line.split(marker).nth(1).map(|rest| rest.chars().take(160).collect::<String>()).unwrap_or_default();
    if line.contains("no suggested test: ") {
        return Err(Got::NoTest(cause("no suggested test: ")));
    }
    if let Some(rest) = line.split("suggested test `").nth(1) {
        return Ok(rest.split('`').next().unwrap_or_default().to_string());
    }
    if line.contains("not decided") || line.contains("not checked") {
        return Err(Got::NoModel(cause("not ")));
    }
    if line.contains("equivalent") {
        return Err(Got::Missed(line.to_string()));
    }
    Err(Got::NoModel(line.chars().take(160).collect()))
}

/// The return of the new version at an input where the body panics: a value that the model
/// has with no call (a call of `Default::default()` would be a value of a call).
fn sentinel(ret: &str) -> &'static str {
    match ret {
        "()" => "return;",
        "bool" => "return false;",
        "Vec<u32>" => "return Vec::new();",
        _ if ret.starts_with("Option<") => "return None;",
        _ if ret.starts_with('u') || ret.starts_with('i') => "return 0;",
        _ => "return Default::default();",
    }
}

/// The source of one step: the old and the new text, and the shape and the point of each body.
fn sources(cases: &[(usize, &Shape, &Point)]) -> (String, String) {
    let mut old = String::from("#![allow(unused, unreachable_code)]\n");
    let mut new = old.clone();
    for (index, shape, point) in cases {
        let head = format!("pub fn c{index}({}) -> {} {{\n", shape.params, shape.ret);
        old.push_str(&format!("{head}    {}\n}}\n\n", shape.body));
        let forced = if point.panics { sentinel(shape.ret).to_string() } else { "panic!(\"semilla point\")".to_string() };
        new.push_str(&format!("{head}    if {} {{ {forced} }}\n    {}\n}}\n\n", point.when, shape.body));
    }
    (old, new)
}

/// Runs each body of the shapes: steps of a few bodies, some steps at one time.
fn run_shapes(shapes: &[Shape], env: &[(&str, &str)]) -> Vec<(String, String, Got)> {
    let mut all: Vec<(usize, &Shape, &Point)> = Vec::new();
    for shape in shapes {
        for point in &shape.points {
            all.push((all.len(), shape, point));
        }
    }
    let chunks: Vec<&[(usize, &Shape, &Point)]> = all.chunks(16).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                loop {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(chunk) = chunks.get(at) else { break };
                    let (old, new) = sources(chunk);
                    let dir = common::TestDir::new();
                    let output = step(dir.path(), &old, &new, env);
                    let tests = suggested::suggested(&output);
                    let passed_old = suggested::passing(dir.path(), "old", &old, &tests);
                    let passed_new = suggested::passing(dir.path(), "new", &new, &tests);
                    let mut mine = Vec::new();
                    for (index, shape, point) in chunk.iter() {
                        let name = format!("depot::c{index} (");
                        let got = match output.lines().find(|line| line.trim_start().starts_with(&name)).map(classify) {
                            Some(Ok(test)) => {
                                let text = tests.iter().find(|found| found.name == test).map(|found| found.text.clone()).unwrap_or_default();
                                if !passed_old.contains(&test) {
                                    Got::Defect(format!("the test fails on the old code:\n{text}"))
                                } else if passed_new.contains(&test) {
                                    Got::Missed(format!("the test passes on the new code:\n{text}"))
                                } else if text.contains("assert") || text.contains("expected = ") {
                                    Got::Confirmed
                                } else {
                                    Got::Exit
                                }
                            }
                            Some(Err(got)) => got,
                            None => Got::NoModel("no line in the step section".into()),
                        };
                        mine.push((format!("{} / {}", shape.list, shape.entry), format!("{} at {}", shape.body, point.when), got));
                    }
                    found.lock().unwrap().extend(mine);
                }
            });
        }
    });
    let mut found = found.into_inner().unwrap();
    found.sort();
    found
}

/// The counts of each list: the bodies, and the results of each class.
fn report(found: &[(String, String, Got)]) -> String {
    let mut lists: BTreeMap<String, [usize; 6]> = BTreeMap::new();
    for (entry, _, got) in found {
        let list = entry.split(" / ").next().unwrap_or_default().to_string();
        let slot = match got {
            Got::Confirmed => 0,
            Got::Exit => 1,
            Got::NoTest(_) => 2,
            Got::NoModel(_) => 3,
            Got::Defect(_) => 4,
            Got::Missed(_) => 5,
        };
        lists.entry(list).or_default()[slot] += 1;
    }
    let mut text = String::from("list: cases, confirmed, exit only, no test, no model result, defects, missed\n");
    for (list, counts) in &lists {
        text.push_str(&format!("{list}: {}, {}, {}, {}, {}, {}, {}\n", counts.iter().sum::<usize>(), counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]));
    }
    for (entry, case, got) in found {
        if !matches!(got, Got::Confirmed) {
            text.push_str(&format!("  {entry}: {case}: {got:?}\n"));
        }
    }
    text
}

// rule+ R75.2-1
#[test]
fn each_model_rule_that_keeps_a_name_agrees_with_a_run_on_its_inputs() {
    let shapes = shapes();
    let found = run_shapes(&shapes, &[]);
    let text = report(&found);
    println!("{text}");
    if let Ok(path) = std::env::var("SEMILLA_MODEL_RUNS_REPORT") {
        let _ = std::fs::write(path, &text);
    }
    let defects: Vec<&(String, String, Got)> = found.iter().filter(|(_, _, got)| matches!(got, Got::Defect(_) | Got::Missed(_))).collect();
    assert!(defects.is_empty(), "a run shows a defect of the model:\n{defects:#?}\n{text}");
    // Each list has bodies that a run confirms: the check reaches each list.
    for list in ["intrinsics", "drop_is_free", "sequence model", "modeled types", "configuration facts"] {
        let confirmed = found.iter().filter(|(entry, _, got)| entry.starts_with(list) && *got == Got::Confirmed).count();
        assert!(confirmed >= 2, "the list {list} has {confirmed} confirmed case(s):\n{text}");
    }
}

// rule- R75.2-1
#[test]
fn a_model_defect_shows_as_a_disagreement_in_the_run_check() {
    // The switch of the driver turns R69.1-1 off (each optional overflow check of an inlined
    // callee of `core` stays in `release`, as before batch 11a). The model then says that
    // `i32::abs(i32::MIN)` panics in `release`; the run returns `i32::MIN`. The check of this
    // test reads it as a defect of the model.
    let shape = Shape { list: "test switch", entry: "abs", params: "x: i32", ret: "i32", body: "x.abs()", points: vec![at("x == i32::MIN"), at("x == -5")] };
    let found = run_shapes(std::slice::from_ref(&shape), &[("SEMILLA_TEST_RULE_OFF", "R69.1-1")]);
    assert!(matches!(&found.iter().find(|(_, case, _)| case.contains("i32::MIN")).unwrap().2, Got::Defect(_)), "{found:#?}");
    assert_eq!(found.iter().find(|(_, case, _)| case.contains("-5")).unwrap().2, Got::Confirmed, "{found:#?}");
}

/// The bodies of the rules R75.2-2 to R75.2-4 in one step: the name of each body, and its line.
fn rule_lines() -> &'static BTreeMap<&'static str, (String, String)> {
    static FOUND: std::sync::OnceLock<BTreeMap<&'static str, (String, String)>> = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        let shapes = [
            ("count_ones", Shape { list: "", entry: "", params: "x: u32", ret: "u32", body: "x.count_ones()", points: vec![at("x == 7")] }),
            ("wrapping_add", Shape { list: "", entry: "", params: "x: u32, y: u32", ret: "u32", body: "x.wrapping_add(y)", points: vec![at("x == u32::MAX && y == 2")] }),
            ("capacity_large", Shape { list: "", entry: "", params: "n: usize", ret: "Vec<u32>", body: "Vec::with_capacity(n)", points: vec![at("n == 1152921504606846976")] }),
            ("capacity_small", Shape { list: "", entry: "", params: "n: usize", ret: "Vec<u32>", body: "Vec::with_capacity(n)", points: vec![at("n == 64")] }),
            ("first", Shape { list: "", entry: "", params: "s: &[u32]", ret: "Option<u32>", body: "s.first().copied()", points: vec![at(seq("s", &["4", "5"]))] }),
            ("last", Shape { list: "", entry: "", params: "s: &[u32]", ret: "Option<u32>", body: "s.last().copied()", points: vec![at(seq("s", &["4", "5"]))] }),
            ("is_empty", Shape { list: "", entry: "", params: "s: &[u8]", ret: "bool", body: "s.is_empty()", points: vec![at(seq("s", &[]))] }),
            ("first_pair", Shape { list: "", entry: "", params: "s: &[(u32, u32)]", ret: "Option<(u32, u32)>", body: "s.first().copied()", points: vec![at("s.len() == 1 && s[0].0 == 3")] }),
        ];
        let cases: Vec<(usize, &Shape, &Point)> = shapes.iter().enumerate().map(|(index, (_, shape))| (index, shape, &shape.points[0])).collect();
        let (old, new) = sources(&cases);
        let dir = common::TestDir::new();
        let output = step(dir.path(), &old, &new, &[]);
        let tests = suggested::suggested(&output);
        shapes.iter().enumerate().map(|(index, (name, _))| {
            let head = format!("depot::c{index} (");
            let line = output.lines().find(|line| line.trim_start().starts_with(&head)).unwrap_or_else(|| panic!("no line of {name}:\n{output}")).to_string();
            let test = line.split("suggested test `").nth(1).and_then(|rest| rest.split('`').next()).and_then(|test| tests.iter().find(|found| found.name == test)).map(|found| found.text.clone()).unwrap_or_default();
            (*name, (line, test))
        }).collect()
    })
}

// rule+ R75.2-2
#[test]
fn a_value_that_the_model_does_not_fix_has_no_number_and_no_assertion() {
    // `count_ones` is a pure intrinsic: a value of the call (an uninterpreted function). The
    // solver gives it any number, so the line says "a value", and the test does not assert it.
    let (line, test) = &rule_lines()["count_ones"];
    assert!(line.contains("before returns a value, after panics") && line.contains("suggested test `"), "{line}");
    assert!(test.contains("crate::c0(x);") && !test.contains("assert"), "{test}");
}

// rule- R75.2-2
#[test]
fn a_value_that_the_model_fixes_has_its_number_and_its_assertion() {
    let (line, test) = &rule_lines()["wrapping_add"];
    assert!(line.contains("before returns 1, after panics"), "{line}");
    assert!(test.contains("assert_eq!(got, 1);"), "{test}");
}

// rule+ R75.2-3
#[test]
fn a_capacity_that_a_machine_can_lack_has_no_sure_outcome() {
    // 2^60 elements of 4 bytes: below the capacity overflow, but no machine has the memory. A
    // run aborts (`handle_alloc_error`); the model has an abort path that is never exact.
    let (line, _) = &rule_lines()["capacity_large"];
    assert!(line.contains("not decided") && !line.contains("before returns []") && !line.contains("suggested test"), "{line}");
}

// rule- R75.2-3
#[test]
fn a_small_capacity_returns_an_empty_vector_and_the_test_asserts_it() {
    let (line, test) = &rule_lines()["capacity_small"];
    assert!(line.contains("before returns [], after panics"), "{line}");
    assert!(test.contains("assert!(matches!(got[..], []));"), "{test}");
}

// rule+ R75.2-4
#[test]
fn the_mir_of_first_last_and_is_empty_of_a_slice_replaces_the_list_entries() {
    for (name, outcome, assertion) in [("first", "before returns Some(4)", "Option::Some(4)"), ("last", "before returns Some(5)", "Option::Some(5)"), ("is_empty", "before returns true", "assert_eq!(got, true);")] {
        let (line, test) = &rule_lines()[name];
        assert!(line.contains(outcome), "{line}");
        assert!(test.contains(assertion), "{test}");
    }
}

// rule- R75.2-4
#[test]
fn the_mir_of_first_does_not_model_a_slice_of_elements_that_the_sequence_model_does_not_have() {
    let (line, _) = &rule_lines()["first_pair"];
    assert!(line.contains("not decided"), "{line}");
}
