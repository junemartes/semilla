//! Design 84: a step starts the solver only for a claim of an intent file. Each changed body with
//! no claim gets only the equal-MIR check: "equal MIR" or "MIR changes" with a claim template. A
//! body with a claim keeps the engine of today (the tries with the solver, design 78).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn command(repo: &Path, env: &[(&str, &str)]) -> Command {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    command.env("SEMILLA_CACHE_DIR", repo.parent().unwrap().join("cache"));
    command.envs(env.iter().copied());
    command
}

fn semilla_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    command(repo, env).args(args).output().unwrap()
}

#[track_caller]
fn run_env(repo: &Path, args: &[&str], env: &[(&str, &str)], expect: i32) -> String {
    let output = semilla_env(repo, args, env);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(expect), "semilla {args:?}\n--- stdout\n{stdout}\n--- stderr\n{stderr}");
    stdout
}

#[track_caller]
fn run(repo: &Path, args: &[&str], expect: i32) -> String {
    run_env(repo, args, &[], expect)
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

fn write(repo: &Path, path: &str, text: &str) {
    let full = repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

/// The old version: each function is the subject of one rule of design 84.
const OLD: &str = "pub fn base() -> u32 { 7 }
pub fn mid() -> u32 { base() + 1 }
pub fn top(n: u32) -> u32 { let m = mid(); n.wrapping_mul(m) }
pub fn calm() -> u32 { 5 }
pub fn quiet(n: u32) -> u32 { let m = calm(); n.wrapping_mul(m) }
pub fn half(x: u32) -> u32 { x / 2 }
pub fn tier(k: u32) -> u32 { if k > 3 { 1 } else { 0 } }
pub fn both(a: u32) -> u32 { let x = a.wrapping_add(1); x.wrapping_mul(2) }
pub fn grow(x: u32) -> u32 { x }
";

/// The new version: `base` changes its value (so `mid` and `top` can change), `top` and `quiet`
/// rename a local (an equal MIR text), `half` is a shift (equivalent, but only the solver shows
/// it), `tier` changes a condition (different for `k = 3`), and `both` has two edits: a rename
/// of a local and a changed literal. `grow` adds a statement: a node that the AST pass of a step
/// with an intent file leaves with its own template.
const NEW: &str = "pub fn base() -> u32 { 6 }
pub fn mid() -> u32 { base() + 1 }
pub fn top(n: u32) -> u32 { let k = mid(); n.wrapping_mul(k) }
pub fn calm() -> u32 { 5 }
pub fn quiet(n: u32) -> u32 { let k = calm(); n.wrapping_mul(k) }
pub fn half(x: u32) -> u32 { x >> 1 }
pub fn tier(k: u32) -> u32 { if k >= 3 { 1 } else { 0 } }
pub fn both(a: u32) -> u32 { let y = a.wrapping_add(1); y.wrapping_mul(3) }
pub fn grow(x: u32) -> u32 { let y = x.wrapping_add(1); y }
";

/// A repository with the old version committed and the new version in the working tree.
fn setup() -> (common::TestDir, PathBuf) {
    let dir = common::TestDir::new();
    let repo = dir.path().join("repo");
    write(&repo, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n");
    write(&repo, ".gitignore", "/target\n/.semilla/\n");
    write(&repo, "src/lib.rs", OLD);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    write(&repo, "src/lib.rs", NEW);
    (dir, repo)
}

/// The line of a body in the section "control flow".
fn body_line<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines().find(|line| line.trim_start().starts_with(&format!("{name} ("))).unwrap_or_else(|| panic!("no line of `{name}`:\n{text}"))
}

/// The coverage line of a body (the section "semantic changes").
fn cover_line<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines().find(|line| line.contains(&format!(": `{name}` ("))).unwrap_or_else(|| panic!("no coverage line of `{name}`:\n{text}"))
}

/// A solver that writes one line to a marker file at each start, then runs the solver of the
/// workspace. With no solver in the workspace it answers only `--version`.
fn marker_solver(dir: &Path) -> (PathBuf, PathBuf) {
    let marker = dir.join("solver-starts");
    let script = dir.join("z3-marker");
    let real = workspace_root().join(".semilla/tools/z3");
    let tail = if real.exists() { format!("exec '{}' \"$@\"", real.display()) } else { "echo 'Z3 version 4.13.0 - 64 bit'".to_string() };
    std::fs::write(&script, format!("#!/bin/sh\necho start >> '{}'\n{tail}\n", marker.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, marker)
}

/// The text of the tool `results` of `semilla mcp` with `verbose`.
fn mcp_results(repo: &Path, env: &[(&str, &str)]) -> String {
    let mut child = command(repo, env).arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |message: serde_json::Value| -> serde_json::Value {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    ask(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
    let answer = ask(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "results", "arguments": {"verbose": true, "wait": 600}}}));
    drop(stdin);
    child.wait().unwrap();
    answer["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

const CLAIMS: &str = "use semilla_intent::prelude::*;

#[same(shop::half)]
fn half_same() {}

#[same(shop::tier)]
fn tier_same() {}
";

// rule+ R84.1-1
// rule- R84.1-1
// rule+ R84.1-2
// rule- R84.1-2
// rule+ R84.1-3
// rule+ R84.2-1
// rule+ R84.2-3
// rule+ R84.4-1
// rule- R84.4-1
// rule+ R84.5-1
// rule- R84.5-1
// rule+ R84.6-1
// rule+ R84.7-1
// rule- R84.3-1
#[test]
fn a_step_with_no_claim_gives_equal_mir_or_mir_changes_and_starts_no_solver() {
    let (dir, repo) = setup();
    let (solver, marker) = marker_solver(dir.path());
    // Each command has the same solver: the key of a stored result names it.
    let env = [("SEMILLA_Z3", solver.to_str().unwrap())];
    let text = run_env(&repo, &["step", "the change"], &env, 0);
    // [R84.7-1] No solver process starts, also not for `--version`.
    assert!(!marker.exists(), "the solver started: {}\n{text}", std::fs::read_to_string(&marker).unwrap_or_default());
    // [R84.1-1] An equal MIR text with no changed callee: "equal MIR". A changed text: "MIR
    // changes", with no result of the solver.
    assert!(body_line(&text, "shop::quiet").ends_with("equivalent (proved by equal MIR)"), "{text}");
    assert!(body_line(&text, "shop::half").ends_with("; MIR changes"), "{text}");
    assert!(body_line(&text, "shop::tier").ends_with("; MIR changes"), "{text}");
    // [R84.1-2] The text of `top` is equal, but its callee `mid` calls the changed `base`: the
    // closure of the changed bodies has `mid`, so the equal text is no proof.
    assert!(body_line(&text, "shop::top").contains("MIR changes (the same MIR text; a changed callee: `shop::mid`)"), "{text}");
    // [R84.2-3] No result of the solver for a body with no claim.
    for words in ["not decided", "; different", "different (", "different:", "suggested test", "proved by SMT"] {
        assert!(!text.contains(words), "{words}: {text}");
    }
    // [R84.1-3] No walk of the callers.
    assert!(!text.contains("reached bodies") && !text.contains("reaches:"), "{text}");
    // [R84.5-1] The variants of the two edits of `both`: the rename alone has an equal MIR text,
    // the literal alone changes the MIR.
    assert!(text.contains("equivalent alone (proved by equal MIR); the body: MIR changes"), "{text}");
    assert!(text.contains("MIR changes alone; the body: MIR changes"), "{text}");
    // [R84.2-1] One line for each body with "MIR changes": the body and its template.
    let line = cover_line(&text, "shop::half");
    assert!(line.contains("MIR changes with no claim: `shop::half`") && line.contains("`#[same(shop::half)] fn half_same() {}`") && line.contains("#[change(shop::half)] fn half_change(x: u32) -> bool { … }") && line.contains("#[different(shop::half)] fn half_different() -> bool { … }"), "{line}");
    // [R84.6-1] The counts of the changed bodies.
    assert!(text.contains("changed bodies: 1 equal MIR, 0 explained by claims, 6 MIR changes with no claim"), "{text}");
    // [R84.4-1] "MIR changes" reads as "not decided" for the list "not stated" (R71.3-1: the form
    // of a line that the check did not decide) and for the coverage; "equal MIR" reads as
    // "equivalent": `quiet` has the state of an equivalent body.
    assert!(text.contains("in: shop::tier\n      lines: src/lib.rs:7, src/lib.rs:-7\n      to state it: the check did not decide if the behavior changes"), "{text}");
    assert!(text.contains("    equal MIR: `shop::quiet`"), "{text}");
    // [R84.6-1] The JSON, `status --verbose`, and the tool `results` of MCP have the same data.
    let status = run_env(&repo, &["status", "--verbose"], &env, 0);
    assert!(status.contains("changed bodies: 1 equal MIR, 0 explained by claims, 6 MIR changes with no claim") && status.contains("MIR changes with no claim: `shop::tier`"), "{status}");
    let json = run_env(&repo, &["verify", "--json"], &env, 0);
    assert!(json.contains("\"state\": \"no_claim\"") && json.contains("\"state\": \"equivalent\""), "{json}");
    let results = mcp_results(&repo, &env);
    assert!(results.contains("changed bodies: 1 equal MIR, 0 explained by claims, 6 MIR changes with no claim"), "{results}");
    assert!(!marker.exists(), "a command after the step started the solver");
}

// rule- R84.2-1
// rule- R84.2-3
// rule+ R84.3-1
// rule- R84.6-1
// rule- R84.7-1
#[test]
fn a_body_with_a_claim_keeps_the_engine_and_starts_the_solver() {
    let (dir, repo) = setup();
    if !workspace_root().join(".semilla/tools/z3").exists() {
        eprintln!("no solver in the workspace: the test needs scripts/fetch-z3.sh");
        return;
    }
    let (solver, marker) = marker_solver(dir.path());
    write(&repo, ".semilla/intent/step.rs", CLAIMS);
    let text = run_env(&repo, &["step", "the change", "--intent", ".semilla/intent/step.rs"], &[("SEMILLA_Z3", solver.to_str().unwrap())], 1);
    // [R84.7-1] The claims start the solver.
    assert!(marker.exists(), "{text}");
    // [R84.3-1] An empty `#[same]` claim is the full compare of today: a proof by the solver, and
    // a sure difference with its input and its suggested test.
    let claim = |id: &str| text.lines().find(|line| line.contains(&format!(" {id} "))).unwrap_or_else(|| panic!("no line of {id}:\n{text}")).to_string();
    assert!(claim("i1").contains("proved") && claim("i1").contains("equivalent by SMT"), "{text}");
    assert!(claim("i2").contains("false: for k = 3: before returns 0, after returns 1") && claim("i2").contains("suggested test `semilla_test_tier_"), "{text}");
    // [R84.2-1] A body with a claim is not "MIR changes with no claim": the proved claim explains
    // `half`, and the false claim leaves `tier` with its claim.
    assert!(cover_line(&text, "shop::half").starts_with("      explained by claim i1"), "{text}");
    assert!(cover_line(&text, "shop::tier").contains("not explained by its claims: `shop::tier`"), "{text}");
    assert!(text.contains("changed bodies: 1 equal MIR, 1 explained by claims, 4 MIR changes with no claim, 1 MIR changes that the claims on the item do not explain"), "{text}");
    // [R84.2-3] The result of the solver is the result of the claim; the body line keeps the
    // equal-MIR check.
    assert!(body_line(&text, "shop::tier").ends_with("; MIR changes"), "{text}");
}

// rule+ R84.2-2
// rule- R84.2-2
#[test]
fn each_template_of_a_body_with_no_claim_passes_the_check_of_the_intent_files() {
    let (_dir, repo) = setup();
    let text = run(&repo, &["step", "the change"], 0);
    let mut templates: Vec<String> = Vec::new();
    for line in text.lines().filter(|line| line.contains("MIR changes with no claim: ")) {
        for piece in line.split('`').filter(|piece| piece.starts_with("#[")) {
            templates.push(piece.replace('…', "true"));
        }
    }
    assert!(templates.len() >= 12, "{templates:?}\n{text}");
    for (index, template) in templates.iter().enumerate() {
        let path = format!(".semilla/intent/t{index}.rs");
        write(&repo, &path, &format!("use semilla_intent::prelude::*;\n\n{template}\n"));
        let output = semilla_env(&repo, &["intent", "check", &path], &[]);
        assert!(output.status.success(), "{template}\n{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }
    // [R84.2-2] An item whose node the AST pass leaves with its own template (a step with an
    // intent file that has no claim on the item): the line names the node and gives no second
    // copy.
    let (_dir, repo) = setup();
    write(&repo, ".semilla/intent/step.rs", "use semilla_intent::prelude::*;\n\n#[same(shop::half)]\nfn half_same() {}\n");
    let text = run(&repo, &["step", "the claim", "--intent", ".semilla/intent/step.rs"], 0);
    let line = cover_line(&text, "shop::grow");
    assert!(line.contains("the claim template of the node n") && !line.contains("#[same(shop::grow)]"), "{line}\n{text}");
    assert!(text.contains("template: #[same(shop::grow)]"), "{text}");
    // The negative case: an item with no such node keeps its own template.
    assert!(cover_line(&text, "shop::tier").contains("`#[same(shop::tier)] fn tier_same() {}`"), "{text}");
}
