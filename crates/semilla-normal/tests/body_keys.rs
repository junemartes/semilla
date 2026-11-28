//! [R93.3-1] The key of the record of a body (design 93.3): the driver makes it before the
//! print, and a known key gives a pointer line. A changed callee, a changed type, a renamed
//! name, and moved columns give a new key; an equal tree and a moved line give the same keys
//! (R99.8-1: each place is relative to its item).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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

const BASE: &str = "pub struct S {\n    pub a: u32,\n    pub b: u8,\n}\n\npub fn callee(x: u32) -> u32 {\n    x + 1\n}\n\npub fn caller(s: &S) -> u32 {\n    callee(s.a)\n}\n\npub fn other() -> u32 {\n    7\n}\n";

/// The key of each body of a build of `lib`, and the count of the pointer lines, with the keys
/// of `known` in the store.
fn keys(scratch: &Path, lib: &str, known: &[String]) -> (BTreeMap<String, Option<String>>, usize) {
    keys_of(scratch, lib, None, &[], known, ".lib.")
}

/// [`keys`] with a second crate of the workspace (`user`, a user of `keys`), a feature list,
/// and the unit of the record file to read.
fn keys_of(scratch: &Path, lib: &str, user: Option<&str>, features: &[String], known: &[String], unit: &str) -> (BTreeMap<String, Option<String>>, usize) {
    let root = scratch.join("ws");
    let members = if user.is_some() { "members = [\".\", \"user\"]\n" } else { "" };
    write(&root.join("Cargo.toml"), &format!("[package]\nname = \"keys\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[features]\nextra = []\n\n[workspace]\n{members}"));
    write(&root.join("src/lib.rs"), lib);
    // The members change between the builds: the build writes a new lock file.
    let _ = std::fs::remove_file(root.join("Cargo.lock"));
    if let Some(user) = user {
        write(&root.join("user/Cargo.toml"), "[package]\nname = \"user\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nkeys = { path = \"..\" }\n");
        write(&root.join("user/src/lib.rs"), user);
    }
    let out = scratch.join("out");
    let target = scratch.join("target");
    // The build reads only its folders (the sandbox): the file is in the target folder.
    let file = target.join("tmp/known.txt");
    write(&file, &known.iter().map(|key| format!("{key}\n")).collect::<String>());
    semilla_normal::cargo::set_known_file(&target, Some(&file));
    semilla_normal::run_cargo_check_delta_features(&root, "n0", &out, &target, features).unwrap_or_else(|error| panic!("{error:#}"));
    semilla_normal::cargo::set_known_file(&target, None);
    let path = std::fs::read_dir(&out).unwrap().flatten().map(|entry| entry.path()).find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(unit.trim_start_matches('.').split('.').next().filter(|first| *first == "user").map(|_| "user.lib").unwrap_or("keys.lib"))) && path.extension().is_some_and(|extension| extension == "jsonl")).expect("a record file");
    let text = std::fs::read_to_string(path).unwrap();
    let lines: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let list = lines.last().and_then(|last| last.get("semilla-keys")).and_then(|keys| keys.as_array()).expect("a keys line").clone();
    let mut found = BTreeMap::new();
    let mut pointers = 0;
    for (line, key) in lines[1..lines.len() - 1].iter().zip(list) {
        let key = key.as_str().map(str::to_string);
        match line.get("name").and_then(|name| name.as_str()) {
            Some(name) => {
                found.insert(name.to_string(), key);
            }
            None => {
                pointers += 1;
                found.insert(format!("pointer {}", key.clone().unwrap_or_default()), key);
            }
        }
    }
    (found, pointers)
}

/// [R93.3-3] The record of `keys::b` in a crate where an earlier body (`a`) reaches `d4` at a
/// depth past the limit of the effect summary (`deep`), or reaches another function.
fn record_of_b(scratch: &Path, deep: bool) -> String {
    let lib = format!(
        "pub fn a() {{\n    {}();\n}}\n\npub fn e1() {{}}\npub fn d1() {{\n    d2();\n}}\npub fn d2() {{\n    d3();\n}}\npub fn d3() {{\n    d4();\n}}\npub fn d4() {{\n    x();\n}}\npub fn x() {{}}\n\npub fn b() {{\n    d4();\n}}\n",
        if deep { "d1" } else { "e1" }
    );
    keys(scratch, &lib, &[]);
    let out = scratch.join("out");
    let path = std::fs::read_dir(&out).unwrap().flatten().map(|entry| entry.path()).find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("keys.lib"))).expect("a record file");
    let text = std::fs::read_to_string(path).unwrap();
    text.lines().find(|line| line.contains("\"name\":\"keys::b\"")).expect("a record of b").to_string()
}

fn key_of(keys: &BTreeMap<String, Option<String>>, name: &str) -> String {
    keys.get(name).cloned().flatten().unwrap_or_else(|| panic!("no key of {name}: {keys:?}"))
}

// rule+ R93.3-1
// rule- R93.3-1
// rule+ R93.3-3
#[test]
fn a_change_of_an_input_gives_a_new_key_and_an_equal_tree_gives_pointers() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("semilla-normal-body-keys");
    let _ = std::fs::remove_dir_all(&scratch);
    let driver = ensure_driver(&scratch);
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe { std::env::set_var("SEMILLA_MIR_DRIVER", &driver) };
    // [R93.3-3] The record of a body does not depend on the bodies before it in the compile:
    // a body that reaches a function past the depth limit of the effect summary does not change
    // the record of another body that calls the function at a small depth.
    assert_eq!(record_of_b(&scratch, true), record_of_b(&scratch, false));
    let (base, _) = keys(&scratch, BASE, &[]);
    let (caller, callee, other) = (key_of(&base, "keys::caller"), key_of(&base, "keys::callee"), key_of(&base, "keys::other"));
    // Positive: the same tree gives the same keys, and with the keys in the store each body
    // is a pointer line.
    let known: Vec<String> = base.values().flatten().cloned().collect();
    let (again, pointers) = keys(&scratch, BASE, &known);
    assert_eq!(pointers, known.len(), "{again:?}");
    // Negative: a changed callee gives the caller a new key; the other bodies keep theirs.
    let (changed, _) = keys(&scratch, &BASE.replace("x + 1", "x + 2"), &[]);
    assert_ne!(key_of(&changed, "keys::caller"), caller);
    assert_ne!(key_of(&changed, "keys::callee"), callee);
    assert_eq!(key_of(&changed, "keys::other"), other);
    // A changed type gives a new key (a field type of the same length: no place changes).
    let (typed, _) = keys(&scratch, &BASE.replace("pub b: u8,", "pub b: i8,"), &[]);
    assert_ne!(key_of(&typed, "keys::caller"), caller);
    // A changed value of a constant that a body reads through a promoted constant gives a
    // new key.
    let constant = format!("{BASE}\npub const LIMIT: u32 = 3;\n\npub fn reads() -> &'static u32 {{\n    &LIMIT\n}}\n");
    let (first, _) = keys(&scratch, &constant, &[]);
    let (second, _) = keys(&scratch, &constant.replace("= 3;", "= 4;"), &[]);
    assert_ne!(key_of(&first, "keys::reads"), key_of(&second, "keys::reads"));
    // A constant of a trait with no default: the value of the local impl is a part of the key
    // of its reader (and the key asks the compiler for no body of the trait constant).
    let traited = format!("{BASE}\npub trait K {{\n    const Q: bool;\n}}\n\nimpl K for u64 {{\n    const Q: bool = true;\n}}\n\npub fn gate() -> u32 {{\n    if <u64 as K>::Q {{ 1 }} else {{ 0 }}\n}}\n");
    let (first, _) = keys(&scratch, &traited, &[]);
    let (second, _) = keys(&scratch, &traited.replace("= true;", "= !!!true;"), &[]);
    assert_ne!(key_of(&first, "keys::gate"), key_of(&second, "keys::gate"));
    // A renamed name gives a new key.
    let (renamed, _) = keys(&scratch, &BASE.replace("callee", "helper"), &[]);
    assert_ne!(key_of(&renamed, "keys::caller"), caller);
    // [R99.8-1] A moved line (a line above the body) keeps each key: each place of a key is
    // relative to its item (design 99.8). A line below each body keeps each key too.
    let (moved, _) = keys(&scratch, &BASE.replace("pub fn caller", "// a line\npub fn caller"), &[]);
    assert_eq!(key_of(&moved, "keys::caller"), caller);
    assert_eq!(key_of(&moved, "keys::other"), other);
    assert_eq!(key_of(&moved, "keys::callee"), callee);
    let (below, _) = keys(&scratch, &format!("{BASE}// a last line\n"), &[]);
    assert_eq!(key_of(&below, "keys::caller"), caller);
    // [R99.8-1] A text before the body on its first line moves its columns: a new key. A body
    // below it moves by bytes only, and keeps its key.
    let (shifted, _) = keys(&scratch, &BASE.replace("pub fn caller", "/* a */ pub fn caller"), &[]);
    assert_ne!(key_of(&shifted, "keys::caller"), caller);
    assert_eq!(key_of(&shifted, "keys::other"), other);
    // [G3] A changed comment in a body moves the lines of the body: the body gets a new key,
    // and a body above it keeps its key.
    let (commented, _) = keys(&scratch, &BASE.replace("    callee(s.a)\n", "    // a comment\n    callee(s.a)\n"), &[]);
    assert_ne!(key_of(&commented, "keys::caller"), caller);
    assert_eq!(key_of(&commented, "keys::callee"), callee);
    // [F4, R99.8-1] The places of the body itself and of the definitions that it names: a line
    // between a body and a type that it names moves the type and not the body. Each place is
    // relative to its item, so the key stays (Semilla moves the place of the type in the
    // stored record); a new field of the type gives a new key.
    let named = format!("{BASE}\npub fn field(t: &T) -> u16 {{\n    t.c\n}}\n\npub struct T {{\n    pub c: u16,\n}}\n");
    let (first, _) = keys(&scratch, &named, &[]);
    let (second, _) = keys(&scratch, &named.replace("pub struct T", "// a line\npub struct T"), &[]);
    assert_eq!(key_of(&first, "keys::field"), key_of(&second, "keys::field"));
    let (third, _) = keys(&scratch, &named.replace("    pub c: u16,\n", "    pub c: u16,\n    pub d: u8,\n"), &[]);
    assert_ne!(key_of(&first, "keys::field"), key_of(&third, "keys::field"));
    // A line between a callee and its caller moves the caller only: a non-generic callee keeps
    // its key.
    // An impl that resolves a call: a new body of the method gives the caller a new key.
    let resolved = format!("{BASE}\npub struct W;\n\nimpl Clone for W {{\n    fn clone(&self) -> W {{\n        W\n    }}\n}}\n\npub fn copy(w: &W) -> W {{\n    w.clone()\n}}\n");
    let (first, _) = keys(&scratch, &resolved, &[]);
    let (second, _) = keys(&scratch, &resolved.replace("        W\n", "        let w = W;\n        w\n"), &[]);
    assert_ne!(key_of(&first, "keys::copy"), key_of(&second, "keys::copy"));
    // A feature (`cfg`) of the compile gives each body a new key.
    let (featured, _) = keys_of(&scratch, BASE, None, &["extra".to_string()], &[], ".lib.");
    assert_ne!(key_of(&featured, "keys::other"), other);
    // A crate of the workspace that uses `keys` (no `crate_hash` of `keys` in its keys): a new
    // body of a function of `keys` keeps the keys of `user`: the metadata of a check build has
    // no MIR of a function (generic or not), so the record reads no body of it (the walk of a
    // key goes into a body of the metadata if the metadata has one). A new type of `keys`, and
    // a new value of a constant, give new keys.
    let lib = format!("{BASE}\npub fn plain(x: u32) -> u32 {{\n    x * 3\n}}\n\npub fn generic<T: Copy>(x: T) -> T {{\n    x\n}}\n");
    let user = "pub fn uses(s: &keys::S) -> u32 {\n    keys::plain(s.a) + keys::generic(1u32)\n}\n\npub fn alone() -> u32 {\n    4\n}\n";
    let (first, _) = keys_of(&scratch, &lib, Some(user), &[], &[], "user");
    let (plain, _) = keys_of(&scratch, &lib.replace("x * 3", "x * 4"), Some(user), &[], &[], "user");
    assert_eq!(key_of(&first, "user::uses"), key_of(&plain, "user::uses"));
    assert_eq!(key_of(&first, "user::alone"), key_of(&plain, "user::alone"));
    let (generic, _) = keys_of(&scratch, &lib.replace("    x\n}", "    let y = x;\n    y\n}"), Some(user), &[], &[], "user");
    assert_eq!(key_of(&first, "user::uses"), key_of(&generic, "user::uses"));
    let (typed, _) = keys_of(&scratch, &lib.replace("pub b: u8,", "pub b: i8,"), Some(user), &[], &[], "user");
    assert_ne!(key_of(&first, "user::alone"), key_of(&typed, "user::alone"));
    let valued = format!("{lib}\npub const K: u32 = 5;\n");
    let user_k = format!("{user}\npub fn reads_k() -> u32 {{\n    keys::K\n}}\n");
    let (first, _) = keys_of(&scratch, &valued, Some(&user_k), &[], &[], "user");
    let (second, _) = keys_of(&scratch, &valued.replace("= 5;", "= 6;"), Some(&user_k), &[], &[], "user");
    assert_ne!(key_of(&first, "user::reads_k"), key_of(&second, "user::reads_k"));
}
