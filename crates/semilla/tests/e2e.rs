//! End-to-end test: an agent tags its work with the real `semilla` binary, and the verifier, the
//! storage, the MCP server, and the UI work on a real git repository with a two-crate workspace.
//!
//! The test builds the MIR driver first. It uses Z3 from `.semilla/tools/z3` of this workspace
//! when it exists. Each test repository has its own cache folder (`SEMILLA_CACHE_DIR`).

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The cache folder of the tests of one repository: `cache` next to the repository folder.
fn cache_dir(repo: &Path) -> PathBuf {
    repo.parent().unwrap().join("cache")
}

/// The folder of the repository in the cache folder (the only folder there, except `tools`).
fn repo_cache(repo: &Path) -> PathBuf {
    std::fs::read_dir(cache_dir(repo)).unwrap().flatten().map(|entry| entry.path()).find(|path| path.join("repository").is_file()).expect("a cache folder of the repository")
}

fn semilla(repo: &Path, args: &[&str]) -> Output {
    semilla_env(repo, args, &[])
}

/// Runs semilla with more environment variables.
fn semilla_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
    // The cache folder of the user: a folder next to the test repository, not the home folder.
    // A test can give a different folder in `env`.
    command.env("SEMILLA_CACHE_DIR", cache_dir(repo));
    // Design 50: the default of `semilla step` has no compiled check. The tests of the compare
    // results get it from this variable. A test of the default mode gives the value `0`.
    // Design 53: the build check of a step is not the subject of most tests, and some test
    // crates do not build on purpose. A test of the build check gives the value `0`.
    command.envs(env.iter().copied());
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    command.output().unwrap()
}

/// Runs semilla and returns stdout. Panics with the output if the exit code is not `expect`.
#[track_caller]
fn run(repo: &Path, args: &[&str], expect: i32) -> String {
    let output = semilla(repo, args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(expect), "semilla {args:?}\n--- stdout\n{stdout}\n--- stderr\n{stderr}");
    stdout
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").current_dir(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write(repo: &Path, path: &str, text: &str) {
    let full = repo.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

const CART_V0: &str = "pub struct Cart {\n    pub items: Vec<u32>,\n}\n\nimpl Cart {\n    pub fn total(&self, max: u32) -> u32 {\n        let mut s = 0;\n        for x in &self.items {\n            if *x < max {\n                s += x;\n            }\n        }\n        s\n    }\n}\n";
const CART_V1: &str = "pub struct Cart {\n    pub items: Vec<u32>,\n}\n\nimpl Cart {\n    pub fn total(&self, max: u32) -> u32 {\n        let mut sum = 0;\n        for x in &self.items {\n            if *x < max {\n                sum += x;\n            }\n        }\n        sum\n    }\n}\n";
const CART_V2: &str = "pub struct Cart {\n    pub items: Vec<u32>,\n}\n\nimpl Cart {\n    pub fn total(&self, max: u32) -> u32 {\n        if self.items.is_empty() {\n            return 0;\n        }\n        let mut sum = 0;\n        for x in &self.items {\n            if *x <= max {\n                sum += x;\n            }\n        }\n        sum\n    }\n}\n";
const MAIN_V0: &str = "fn main() {\n    let cart = shop_core::cart::Cart { items: vec![1, 2, 3] };\n    println!(\"{}\", cart.total(10));\n}\n";
const MAIN_V1: &str = "fn main() {\n    let cart = shop_core::cart::Cart { items: vec![1, 2, 3] };\n    println!(\"{}\", cart.total(20));\n}\n";

fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(repo).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn setup() -> common::TestDir {
    // The MIR driver is a separate binary of the workspace.
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");

    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n");
    write(repo, ".gitignore", "/target\n/.semilla/\n");
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n");
    write(repo, "crates/shop-core/src/cart.rs", CART_V0);
    write(repo, "crates/shop-app/Cargo.toml", "[package]\nname = \"shop-app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nshop-core = { path = \"../shop-core\" }\n");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V0);
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    dir
}

#[test]
fn agent_flow_end_to_end() {
    let dir = setup();
    let repo = &dir.path().join("repo");

    // The index of the base tree: the graph and the MIR content hash of each body.
    let indexed = run(repo, &["index"], 0);
    assert!(indexed.contains("2 crates"), "{indexed}");
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".semilla/index.json")).unwrap()).unwrap();
    assert!(index["body_hashes"].as_object().unwrap().keys().any(|k| k.contains("Cart::total")), "{}", index["body_hashes"]);

    // A change before the first step would be in the base without a claim, so the start of a step refuses it.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let refused = semilla(repo, &["old-step", "begin", "too late"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("differs from HEAD"));
    write(repo, "crates/shop-app/src/main.rs", MAIN_V0);

    // Step 0 (design 24.3): a rename of a local variable, with one command. Design 35: the
    // fact has the category and the compare result.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let anchor = run(repo, &["anchor", "crates/shop-core/src/cart.rs:7"], 0).trim().to_string();
    assert_eq!(anchor, "shop_core::cart::Cart::total/S0");
    let retired = semilla(repo, &["step", "rename s to sum", "--claim", "rename-local shop_core::cart::Cart::total/S0 to=sum"]);
    assert!(String::from_utf8_lossy(&retired.stderr).contains("is retired"), "{}", String::from_utf8_lossy(&retired.stderr));
    // Design 35.3: no step declares a class.
    assert_eq!(semilla(repo, &["step", "rename s to sum", "--class", "neutral"]).status.code(), Some(2));
    let stored = run(repo, &["step", "rename s to sum"], 0);
    assert!(stored.starts_with("accepted") && stored.contains("renamed local `s` → `sum`") && stored.contains("[rename · code equal]"), "{stored}");
    // An empty diff is an error.
    let empty = semilla(repo, &["step", "nothing"]);
    assert!(String::from_utf8_lossy(&empty.stderr).contains("no change since the last step"));

    // Step 1: an early exit and a changed operator. No claim: both are derived facts.
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    let stored = run(repo, &["step", "return 0 for an empty cart, include the max"], 0);
    assert!(stored.contains("added early exit `if self.items.is_empty()") && stored.contains("changed operator"), "{stored}");
    let check = run(repo, &["check", "--full"], 0);
    assert!(check.contains("[semantics · code different]"), "{check}");
    // [R84.7-1] No solver for the effect of a changed condition: only a claim of an intent file
    // starts the solver (design 84).
    assert!(!check.contains("effect hint: widen"), "{check}");

    // Step 2: a change without a claim is rejected, with a hint.
    run(repo, &["old-step", "begin", "raise the max in the app"], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    // Design 24: a change without a claim gets a derived fact.
    let check = run(repo, &["check"], 0);
    assert!(check.contains("changed literal `10` → `20`") && !check.contains("uncovered"), "{check}");
    run(repo, &["old-step", "end"], 0);

    // Groups, then the final check.
    // Step 2 came from the old protocol (`old-step begin`), so it has no group yet.
    let rejected = run(repo, &["verify", "--final"], 1);
    assert!(rejected.contains("step 2 is in 0 groups"), "{rejected}");
    run(repo, &["group", "G1", "--kind", "refactor", "--title", "Rename the sum", "--steps", "0"], 0);
    run(repo, &["group", "G2", "--kind", "fix", "--title", "Fix the cart total", "--steps", "1,2"], 0);
    // The subgroups hold claims; with facts only, the suggestion is empty.
    run(repo, &["subgroups", "G2"], 0);
    let verified = run(repo, &["verify", "--final"], 0);
    assert!(verified.contains("result: accepted"), "{verified}");

    // Commit and tag. The note and the refs stay local.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "fix the cart total"]);
    let tagged = run(repo, &["tag"], 0);
    assert!(tagged.contains("3 steps"), "{tagged}");
    // The background check of the commit, and the standalone report.
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains("queued") || status.contains("ready"), "{status}");
    let report_file = repo.join("review.md");
    let review = run(repo, &["review", "HEAD", "--md", report_file.to_str().unwrap(), "--wait", "600"], 0);
    assert!(review.contains("wrote"), "{review}");
    let markdown = std::fs::read_to_string(&report_file).unwrap();
    assert!(markdown.contains("Fix the cart total") && markdown.contains("Rename the sum"), "{markdown}");
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains("ready"), "{status}");
    assert!(git(repo, &["notes", "--ref=semilla-report", "show", "HEAD"]).contains("\"accepted\": true"));

    let note = git(repo, &["notes", "--ref=semilla", "show", "HEAD"]);
    assert!(note.contains("return 0 for an empty cart"));
    assert!(git(repo, &["for-each-ref", "refs/semilla/"]).lines().count() >= 4);

    // A reviewer loads the commit and verifies it again.
    std::fs::remove_file(repo.join(".semilla/session.json")).unwrap();
    run(repo, &["load", "HEAD"], 0);
    let verified = run(repo, &["verify", "--final", "--json"], 0);
    let report: serde_json::Value = serde_json::from_str(&verified).unwrap();
    assert_eq!(report["accepted"], true);

    // The change tree.
    let tree = run(repo, &["show"], 0);
    assert!(tree.contains("crate shop_core"), "{tree}");
    assert!(tree.contains("⟦<⟧→⟦<=⟧"), "{tree}");

    ui_flow(repo);
    // The MCP flow stores a step and removes it again; the change set of the session is new then.
    mcp_flow(repo);
}

/// Talks to `semilla mcp` with JSON-RPC over stdio.
fn mcp_flow(repo: &Path) {
    let root = workspace_root();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
        .env("SEMILLA_CACHE_DIR", cache_dir(repo))
        .current_dir(repo)
        .arg("mcp")
        .env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |message: serde_json::Value| -> serde_json::Value {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = ask(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "semilla");
    let tools = ask(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for name in ["step", "undo", "tag", "results", "ops", "verify", "replay_plan", "replay_apply"] {
        assert!(names.contains(&name), "missing tool {name}");
    }
    // The tools of the old protocol are not served (design 24.3).
    for name in ["claim", "anchor", "check_step", "suggest_claims", "group_set", "subgroups_set", "step_begin", "step_end"] {
        assert!(!names.contains(&name), "a retired tool is served: {name}");
    }
    let ops = ask(serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "ops", "arguments": {}}}));
    let ops = ops["result"]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(ops.contains("tested-by") && !ops.contains("\"change-literal\""), "{ops}");
    // The `step` tool: the review file of the test is a new file in the working tree.
    let stored = ask(serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "step", "arguments": {"intent": "add the review file"}}}));
    let text = stored["result"]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(stored["result"]["isError"].is_null() && text.contains("accepted") && text.contains("review.md: added"), "{stored}");
    let undone = ask(serde_json::json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "undo", "arguments": {}}}));
    assert!(undone["result"]["content"][0]["text"].as_str().unwrap().contains("is removed"), "{undone}");
    // A rejected step is a tool error with the cause.
    let retired = ask(serde_json::json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "step", "arguments": {"intent": "x", "claims": ["change-literal shop_app (bin \"shop-app\")::main"]}}}));
    assert_eq!(retired["result"]["isError"], true, "{retired}");
    assert!(retired["result"]["content"][0]["text"].as_str().unwrap().contains("is retired"), "{retired}");
    drop(stdin);
    child.wait().unwrap();
}

/// Calls tools of `semilla mcp` in one session. Gives the text and the error mark of each call.
fn mcp_calls(repo: &Path, calls: &[(&str, serde_json::Value)]) -> Vec<(String, bool)> {
    mcp_calls_env(repo, calls, &[])
}

/// As [`mcp_calls`], with more environment variables. The build check of a step is off
/// (design 53): a test of the build check gives `SEMILLA_NO_CHECK` with the value `0`.
fn mcp_calls_env(repo: &Path, calls: &[(&str, serde_json::Value)], env: &[(&str, &str)]) -> Vec<(String, bool)> {
    let root = workspace_root();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
        .env("SEMILLA_CACHE_DIR", cache_dir(repo))
        .current_dir(repo)
        .arg("mcp")
        .env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"))
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
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
    let mut out = Vec::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        let answer = ask(serde_json::json!({"jsonrpc": "2.0", "id": index + 2, "method": "tools/call", "params": {"name": name, "arguments": arguments}}));
        out.push((answer["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string(), answer["result"]["isError"] == true));
    }
    drop(stdin);
    child.wait().unwrap();
    out
}

/// Starts `semilla ui` and fetches the page and the API.
fn ui_flow(repo: &Path) {
    let root = workspace_root();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
        .env("SEMILLA_CACHE_DIR", cache_dir(repo))
        .current_dir(repo)
        .args(["ui", "--port", &port.to_string()])
        .env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let get = |path: &str| -> Option<String> {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").ok()?;
        let mut text = String::new();
        stream.read_to_string(&mut text).ok()?;
        Some(text)
    };
    let mut page = None;
    for _ in 0..600 {
        if let Some(text) = get("/") {
            page = Some(text);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let page = page.expect("the UI starts");
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    let changeset = get("/api/changeset").unwrap();
    assert!(changeset.contains("Fix the cart total"), "{changeset}");
    let flow = get("/api/flow/shop_core%3A%3Acart%3A%3ACart%3A%3Atotal").unwrap();
    assert!(flow.contains("flowchart"), "{flow}");
    let search = get("/api/search?q=total").unwrap();
    assert!(search.contains("Cart::total"), "{search}");
    // The worker built the N0 records, so the external calls come from the MIR, with full paths.
    let graph = get("/api/graph").unwrap();
    assert!(graph.contains("\"external\"") && graph.contains("into_iter"), "{graph}");
    child.kill().unwrap();
    child.wait().unwrap();
}

/// The replay: the agent makes all changes first, then rebuilds them as checked steps.
#[test]
fn replay_flow() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let cart = "crates/shop-core/src/cart.rs";
    let main = "crates/shop-app/src/main.rs";

    // The finished work, without steps: a rename, an early exit, a changed operator, a literal.
    write(repo, cart, CART_V2);
    write(repo, main, MAIN_V1);
    let begun = run(repo, &["replay", "begin"], 0);
    assert!(begun.contains("hunk(s) in 2 file(s)"), "{begun}");
    assert_eq!(std::fs::read_to_string(repo.join(cart)).unwrap(), CART_V0, "the working tree is the base");
    let status = run(repo, &["replay", "status"], 0);
    assert!(status.contains("take crates/shop-app/src/main.rs:3-3"), "{status}");
    assert!(status.contains("change-literal") || status.contains("replace-macro-input"), "{status}");

    // A take needs an open step.
    assert_eq!(semilla(repo, &["replay", "take", main]).status.code(), Some(2));

    // Step 0: the rename. Its line is in the same hunk as the early exit, so the agent edits by hand.
    run(repo, &["old-step", "begin", "rename s to sum"], 0);
    write(repo, cart, CART_V1);
    let ended = run(repo, &["old-step", "end"], 0);
    assert!(ended.contains("hunk(s) remain to the target"), "{ended}");

    // Step 1: take the rest of the function from the target by its item path.
    run(repo, &["old-step", "begin", "return 0 for an empty cart, include the max"], 0);
    let took = run(repo, &["replay", "take", "shop_core::cart::Cart::total"], 0);
    assert!(took.contains("took 2 hunk(s)"), "{took}");
    assert_eq!(std::fs::read_to_string(repo.join(cart)).unwrap(), CART_V2);
    run(repo, &["check"], 0);
    run(repo, &["old-step", "end"], 0);

    // The end is refused while hunks remain.
    assert_eq!(semilla(repo, &["replay", "end"]).status.code(), Some(2));

    // Step 2: one hunk by its target line.
    run(repo, &["old-step", "begin", "a larger max in the app"], 0);
    // Design 55.3: the parser of the position is the parser of a place of a tag. Lines of
    // the old tree and a line 0 are errors.
    let old_tree = semilla(repo, &["replay", "take", &format!("{main}:-3")]);
    assert!(old_tree.status.code() == Some(2) && String::from_utf8_lossy(&old_tree.stderr).contains("gives lines of the old tree"), "{}", String::from_utf8_lossy(&old_tree.stderr));
    assert_eq!(semilla(repo, &["replay", "take", &format!("{main}:0")]).status.code(), Some(2));
    run(repo, &["replay", "take", &format!("{main}:3")], 0);
    run(repo, &["check"], 0);
    let ended = run(repo, &["old-step", "end"], 0);
    assert!(ended.contains("the working tree is the target"), "{ended}");
    let done = run(repo, &["replay", "end"], 0);
    assert!(done.contains("3 step(s)"), "{done}");
    assert_eq!(std::fs::read_to_string(repo.join(main)).unwrap(), MAIN_V1);

    run(repo, &["group", "A", "--kind", "refactor", "--title", "Rename the sum", "--steps", "0"], 0);
    run(repo, &["group", "B", "--kind", "behavior", "--title", "New total rules", "--steps", "1,2"], 0);
    let verified = run(repo, &["verify", "--final"], 0);
    assert!(verified.contains("accepted"), "{verified}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "work"]);
    run(repo, &["tag"], 0);

    // A commit made without semilla: replay it after the fact. The working tree comes back at the end.
    let main_v2 = MAIN_V1.replace("20", "30");
    write(repo, main, &main_v2);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "untagged work"]);
    write(repo, "notes.txt", "not part of the commit\n");
    run(repo, &["replay", "begin", "--commit", "HEAD"], 0);
    assert_eq!(std::fs::read_to_string(repo.join(main)).unwrap(), MAIN_V1, "the working tree is the parent commit");
    assert!(!repo.join("notes.txt").exists());
    run(repo, &["old-step", "begin", "an even larger max"], 0);
    run(repo, &["replay", "take", main], 0);
    run(repo, &["old-step", "end"], 0);
    run(repo, &["replay", "end"], 0);
    assert_eq!(std::fs::read_to_string(repo.join("notes.txt")).unwrap(), "not part of the commit\n", "the working tree is restored");
    run(repo, &["group", "C", "--kind", "behavior", "--title", "Larger max", "--steps", "0"], 0);
    let verified = run(repo, &["verify", "--final"], 0);
    assert!(verified.contains("accepted"), "{verified}");
    let tagged = run(repo, &["tag"], 0);
    assert!(tagged.contains("1 steps"), "{tagged}");
}

const PLAN_BASE: &str = "pub fn total(v: &[u32]) -> u32 {\n    let mut s = 0;\n    for x in v {\n        s += x;\n    }\n    s\n}\n\npub fn scale(v: &[u32], k: u32) -> Vec<u32> {\n    let out: Vec<u32> = v.iter().map(|x| x * k).collect();\n    out\n}\n\npub fn first(v: &[u32]) -> u32 {\n    v[0]\n}\n\npub fn twice(v: &[u32]) -> Vec<u32> {\n    scale(v, 2)\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t_total() {\n        assert_eq!(total(&[1, 2]), 3);\n    }\n}\n";
const PLAN_TARGET: &str = "pub fn total(v: &[u32]) -> u32 {\n    if v.is_empty() {\n        return 0;\n    }\n    let mut s = 0;\n    for x in v {\n        s += x;\n    }\n    s\n}\n\npub fn multiply(v: &[u32], k: u32) -> Vec<u32> {\n    let out: Vec<u32> = v.iter().map(|x| x * k + 1).collect();\n    out\n}\n\npub fn sum_wide(v: &[u32]) -> u64 {\n    v.iter().map(|x| *x as u64).sum()\n}\n\npub fn twice(v: &[u32]) -> Vec<u32> {\n    multiply(v, 2)\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t_total() {\n        assert_eq!(total(&[1, 2]), 3);\n    }\n    #[test]\n    fn t_wide() {\n        assert_eq!(sum_wide(&[1, 2]), 3);\n    }\n}\n";

/// The plan: the agent drafts all steps at once, edits the draft, and applies the whole plan.
#[test]
fn plan_flow() {
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");
    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    // No `.semilla/` line in `.gitignore`: semilla keeps its state out of the trees by itself.
    write(repo, "Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    write(repo, ".gitignore", "/target\n");
    write(repo, "src/lib.rs", PLAN_BASE);
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    write(repo, "src/lib.rs", PLAN_TARGET);
    run(repo, &["replay", "begin"], 0);

    // The draft: the rename is its own step (a whole-word edit), and its body change comes later.
    run(repo, &["replay", "plan"], 0);
    let plan_file = repo.join(".semilla/plan.json");
    let mut plan: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&plan_file).unwrap()).unwrap();
    let steps = plan["steps"].as_array().unwrap().clone();
    let intents: Vec<&str> = steps.iter().map(|s| s["intent"].as_str().unwrap()).collect();
    assert!(intents[0].starts_with("rename `scale` to `multiply`"), "{intents:?}");
    // Design 24.2: Semilla derives the rename, so the draft has no claim for it.
    assert!(steps[0]["claims"].as_array().unwrap().is_empty(), "{}", steps[0]);
    assert!(steps[0]["edits"][0]["word"].as_bool().unwrap());
    assert!(intents.iter().any(|i| i.contains("`sum_wide`") && i.contains("`t_wide`")), "the test goes with the item that it uses: {intents:?}");
    let add_step = intents.iter().position(|i| i.contains("`sum_wide`")).unwrap();

    // The agent adds the property claims of the new function, and the groups.
    for claim in [
        serde_json::json!({"op": "no-panic", "at": "demo::sum_wide"}),
        serde_json::json!({"op": "no-io", "at": "demo::sum_wide"}),
        serde_json::json!({"op": "tested-by", "at": "demo::sum_wide", "args": {"test": "t_wide"}}),
    ] {
        plan["steps"][add_step]["claims"].as_array_mut().unwrap().push(claim);
    }
    let all: Vec<usize> = (1..steps.len()).collect();
    plan["groups"] = serde_json::json!([
        {"id": "G1", "kind": "rename", "title": "Rename scale", "steps": [0]},
        {"id": "G2", "kind": "behavior", "title": "The other changes", "steps": all},
    ]);
    std::fs::write(&plan_file, serde_json::to_string_pretty(&plan).unwrap()).unwrap();
    let applied = run(repo, &["replay", "apply", "--full"], 0);
    assert!(applied.contains("renamed `demo::scale` → `demo::multiply` [rename · code equal]"), "{applied}");
    assert!(applied.contains("verified (mir)"), "no-io uses the MIR: {applied}");
    assert!(applied.contains("the test `demo::tests::t_wide` calls `demo::sum_wide` on each path from its start to its normal return"), "{applied}");
    assert!(applied.contains("the plan is accepted"), "{applied}");
    assert!(!applied.contains("pair:"), "an honest plan gives no pair warning: {applied}");

    // A second apply starts again from the base: the same plan gives the same result.
    let again = run(repo, &["replay", "apply", "--full"], 0);
    assert!(again.contains("the plan is accepted"), "{again}");

    // A file outside the target stops the apply, so that the reset does not delete it.
    write(repo, "notes.txt", "mine\n");
    let refused = semilla(repo, &["replay", "apply"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("notes.txt"));
    std::fs::remove_file(repo.join("notes.txt")).unwrap();

    run(repo, &["replay", "end"], 0);
    let verified = run(repo, &["verify", "--final"], 0);
    assert!(verified.contains("accepted"), "{verified}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "work"]);
    run(repo, &["tag"], 0);
    // The state folder is not in the commit.
    assert!(!git(repo, &["ls-tree", "-r", "--name-only", "HEAD"]).contains(".semilla"));
}

const LEDGER_BASE: &str = "pub mod money;\n\npub struct Entry {\n    pub amount: u64,\n    pub fee_free: bool,\n}\n\npub fn total(entries: &[Entry]) -> u64 {\n    let mut s = 0;\n    for e in entries {\n        s += e.amount;\n    }\n    s\n}\n\npub fn charge(entries: &[Entry]) -> u64 {\n    let mut s = 0;\n    for e in entries {\n        if e.fee_free {\n            s += e.amount;\n        } else {\n            s += e.amount + e.amount / 100 + 1;\n        }\n    }\n    s\n}\n\npub fn report(entries: &[Entry]) -> String {\n    let t = total(entries);\n    money::fmt_cents(t)\n}\n\npub fn is_large(entries: &[Entry]) -> bool {\n    total(entries) > 10_000\n}\n\npub fn legacy_round(x: u64) -> u64 {\n    (x + 50) / 100 * 100\n}\n";
const LEDGER_TARGET: &str = "pub mod money;\n\npub struct Entry {\n    pub amount: u64,\n    pub fee_free: bool,\n}\n\npub fn sum_amounts(entries: &[Entry]) -> u64 {\n    let mut s = 0;\n    for e in entries {\n        s += e.amount;\n    }\n    s\n}\n\nfn fee(amount: u64) -> u64 {\n    amount / 100 + 1\n}\n\npub fn charge(entries: &[Entry]) -> u64 {\n    let mut s = 0;\n    for e in entries {\n        if e.fee_free {\n            s += e.amount;\n        } else {\n            s += e.amount + fee(e.amount);\n        }\n    }\n    s\n}\n\npub fn report(entries: &[Entry]) -> String {\n    let t = sum_amounts(entries);\n    money::format_cents(t)\n}\n\npub fn is_large(entries: &[Entry]) -> bool {\n    sum_amounts(entries) > 50_000\n}\n\npub fn average(entries: &[Entry]) -> u64 {\n    if entries.is_empty() {\n        return 0;\n    }\n    sum_amounts(entries) / entries.len() as u64\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn average_of_two() {\n        let v = vec![Entry { amount: 2, fee_free: true }, Entry { amount: 4, fee_free: true }];\n        assert_eq!(average(&v), 3);\n    }\n}\n";
const MONEY_BASE: &str = "pub fn fmt_cents(c: u64) -> String {\n    let whole = c / 100;\n    let part = c % 100;\n    if part < 10 {\n        format!(\"{}.0{}\", whole, part)\n    } else {\n        format!(\"{}.{}\", whole, part)\n    }\n}\n";
const MONEY_TARGET: &str = "pub fn format_cents(cents: u64) -> String {\n    let whole = cents / 100;\n    let part = cents % 100;\n    if part < 10 {\n        format!(\"{}.0{}\", whole, part)\n    } else {\n        format!(\"{}.{}\", whole, part)\n    }\n}\n";

/// A commit with two renames that share a call site, a parameter rename, an extraction, a
/// literal change, a new function with a test, and a removed function. The draft must keep every
/// change.
#[test]
fn plan_draft_covers_every_change() {
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");
    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    write(repo, "Cargo.toml", "[package]\nname = \"ledger\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    write(repo, ".gitignore", "/target\n");
    write(repo, "src/lib.rs", LEDGER_BASE);
    write(repo, "src/money.rs", MONEY_BASE);
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    write(repo, "src/lib.rs", LEDGER_TARGET);
    write(repo, "src/money.rs", MONEY_TARGET);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "rework"]);
    run(repo, &["replay", "begin", "--commit", "HEAD"], 0);

    run(repo, &["replay", "plan"], 0);
    let plan_file = repo.join(".semilla/plan.json");
    let plan: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&plan_file).unwrap()).unwrap();
    let steps = plan["steps"].as_array().unwrap();
    let intents: Vec<&str> = steps.iter().map(|s| s["intent"].as_str().unwrap()).collect();
    let claims: Vec<String> = steps.iter().flat_map(|s| s["claims"].as_array().unwrap().iter().map(|c| format!("{} {} {}", c["op"].as_str().unwrap(), c["at"].as_str().unwrap_or(""), c["args"]))).collect();
    let has = |op: &str, at: &str| claims.iter().any(|c| c.starts_with(&format!("{op} {at}")));
    // Both renames are their own steps, and no change is lost.
    assert!(intents.contains(&"rename `total` to `sum_amounts`") && intents.contains(&"rename `fmt_cents` to `format_cents`"), "{intents:?}");
    // Design 24.2: the draft has no claim. Each change is a derived fact, and Semilla derives the
    // renames.
    assert!(claims.is_empty() && !has("rename", "ledger::total"), "{claims:#?}");
    assert!(intents.iter().any(|i| i.contains("`average`") && i.contains("`legacy_round`")), "the intent names the new function: {intents:?}");
    // The raw draft is accepted.
    let out = run(repo, &["replay", "apply"], 0);
    assert!(!out.contains("uncovered") && out.contains("changed literal"), "{out}");

    // An unknown field is an error, not a silent loss.
    let mut bad = plan.clone();
    bad["steps"][0]["note"] = serde_json::json!("x");
    std::fs::write(&plan_file, serde_json::to_string_pretty(&bad).unwrap()).unwrap();
    let output = semilla(repo, &["replay", "apply"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field `note`"));

    // A plan with the second rename and the parameter rename in one step (batch 7b, part 1):
    // Semilla derives the rename and gives one fact for each other change. No pair warning.
    let mut pair = plan.clone();
    let index = intents.iter().position(|i| *i == "rename `fmt_cents` to `format_cents`").unwrap();
    pair["steps"][index] = serde_json::json!({
        "intent": "replace `fmt_cents` with `format_cents`",
        "take": ["src/money.rs", "src/lib.rs:34"],
        "claims": []
    });
    let rename_local = intents.iter().position(|i| i.contains("`format_cents`") && !i.starts_with("rename")).unwrap();
    pair["steps"].as_array_mut().unwrap().remove(rename_local);
    std::fs::write(&plan_file, serde_json::to_string_pretty(&pair).unwrap()).unwrap();
    let out = run(repo, &["replay", "apply", "--full"], 0);
    assert!(!out.contains("! pair: "), "{out}");
    assert!(fact_line(&out, "renamed `ledger::money::fmt_cents` → `ledger::money::format_cents`").ends_with("[rename · code equal]"), "{out}");
    assert!(fact_line(&out, "parameter `c` of `ledger::money::format_cents` renamed to `cents`: `u64`").ends_with("[rename · code equal]"), "{out}");
    assert!(fact_line(&out, "`ledger::money::format_cents`: renamed local `c` → `cents`, 2 edit(s)").ends_with("[rename · code equal]"), "{out}");
    run(repo, &["replay", "abort"], 0);
}

/// A pair that Semilla does not derive (a new name in a new module) gives a pair warning, and a
/// refusal from `pair refuse` stays across applies.
#[test]
fn a_pair_refusal_stays_across_applies() {
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");
    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    write(repo, "Cargo.toml", "[package]\nname = \"ledger\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    write(repo, ".gitignore", "/target\n");
    write(repo, "src/lib.rs", "pub mod money;\npub mod text;\n");
    write(repo, "src/money.rs", &format!("pub fn whole(c: u64) -> u64 {{\n    c / 100\n}}\n\n{MONEY_BASE}"));
    write(repo, "src/text.rs", "pub fn dot() -> char {\n    '.'\n}\n");
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]);
    write(repo, "src/money.rs", "pub fn whole(c: u64) -> u64 {\n    c / 100\n}\n");
    write(repo, "src/text.rs", &format!("pub fn dot() -> char {{\n    '.'\n}}\n\n{}", MONEY_BASE.replace("fmt_cents", "format_cents")));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "rework"]);
    run(repo, &["replay", "begin", "--commit", "HEAD"], 0);
    run(repo, &["replay", "plan"], 0);
    // Semilla does not derive this pair, so phase 1 gives the warning with the cause.
    let out = run(repo, &["replay", "apply"], 0);
    assert!(out.contains("! pair: `ledger::money::fmt_cents` (removed) and `ledger::text::format_cents` (added) are 100% similar"), "{out}");
    let out = run(repo, &["replay", "apply", "--full"], 0);
    assert!(out.contains("! pair: `ledger::money::fmt_cents` (removed)") && out.contains("add it to `refusals`"), "{out}");
    assert!(out.contains("the name and the module are new: Semilla derives a rename in the same module, or a move with the same name"), "{out}");
    run(repo, &["pair", "refuse", "ledger::money::fmt_cents", "ledger::text::format_cents", "--reason", "a new output contract"], 0);
    let out = run(repo, &["replay", "apply", "--full"], 0);
    assert!(out.contains("the agent refused: \"a new output contract\""), "{out}");
    run(repo, &["replay", "abort"], 0);
}

#[test]
fn each_changed_path_needs_a_claim() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    std::fs::write(repo.join("logo.bin"), [0u8, 1, 2, 0xff, 0xfe]).unwrap();
    write(repo, "run.sh", "#!/bin/sh\necho hi\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "files"]);

    // A binary file and a file mode change. The old verifier accepted this step with 0 claims.
    run(repo, &["old-step", "begin", "change the logo and the script mode"], 0);
    std::fs::write(repo.join("logo.bin"), [0u8, 1, 2, 0xff, 0xfe, 7]).unwrap();
    git(repo, &["update-index", "--chmod=+x", "run.sh"]);
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(repo.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Each changed path is a file fact (design 23). A mode change is in the spotlight, so the step
    // needs a reason. `edit-config` is retired.
    let check = run(repo, &["check"], 1);
    assert!(check.contains("logo.bin: changed (binary file changed)"), "{check}");
    assert!(check.contains("run.sh: changed (mode 100644 → 100755)"), "{check}");
    assert!(check.contains("it needs a reason"), "{check}");
    let retired = semilla(repo, &["claim", "edit-config", "file=logo.bin"]);
    assert!(String::from_utf8_lossy(&retired.stderr).contains("retired"));
    run(repo, &["reason", "--at", "run.sh:", "the script must run in CI"], 0);
    // A1: a `.bin` file is not on the allow list of files that no tool reads.
    run(repo, &["reason", "--at", "logo.bin:", "the new logo"], 0);
    let check = run(repo, &["check"], 0);
    assert!(!check.contains("uncovered") && check.contains("reason: \"the script must run in CI\""), "{check}");
    run(repo, &["old-step", "end"], 0);
}

#[test]
fn a_file_mode_change_has_the_text_for_a_file_mode() {
    // Brief 5g, item 4: a mode change of a parsed `.rs` file had the spotlight text "Rust code
    // that Semilla does not parse".
    use std::os::unix::fs::PermissionsExt;
    let dir = setup();
    let repo = &dir.path().join("repo");
    let executable = |file: &str| {
        git(repo, &["update-index", "--chmod=+x", file]);
        std::fs::set_permissions(repo.join(file), std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    executable("crates/shop-core/src/cart.rs");
    let out = semilla(repo, &["step", "make cart.rs executable"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("spotlight: a file mode (a tool can run or skip a file by its mode); it needs a reason: --reason 'crates/shop-core/src/cart.rs:=…'"), "{text}");
    assert!(text.contains("crates/shop-core/src/cart.rs: changed (mode 100644 → 100755)") && !text.contains("does not parse"), "{text}");
    run(repo, &["step", "make cart.rs executable", "--reason", "crates/shop-core/src/cart.rs:=a test of the mode"], 0);
    // A manifest with a mode change and a key change: the key fact, and the mode fact. Only
    // the mode needs a reason.
    let manifest = "crates/shop-core/Cargo.toml";
    write(repo, manifest, &std::fs::read_to_string(repo.join(manifest)).unwrap().replace("0.1.0", "0.1.1"));
    executable(manifest);
    let out = semilla(repo, &["step", "version 0.1.1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("package.version") && text.contains("0.1.1") && text.contains(&format!("{manifest}: changed (mode 100644 → 100755)")), "{text}");
    assert_eq!(text.lines().filter(|line| line.contains("it needs a reason:")).count(), 1, "{text}");
    assert!(text.contains("spotlight: a file mode (") && !text.contains("cannot compare"), "{text}");
}

#[test]
fn a_function_moves_to_another_crate() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let core = "pub const LIMIT: u32 = 10;\n\npub mod cart;\n\npub struct Account {\n    pub balance: u32,\n}\n\npub fn over(account: &Account) -> bool {\n    account.balance > LIMIT\n}\n";
    let app = "use shop_core::{Account, over};\n\nfn main() {\n    let account = Account { balance: 7 };\n    println!(\"{}\", over(&account));\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", core);
    write(repo, "crates/shop-app/src/main.rs", app);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "over"]);

    let core_after = core.replace("\npub fn over(account: &Account) -> bool {\n    account.balance > LIMIT\n}\n", "");
    let app_after = |op: &str| app.replace("use shop_core::{Account, over};", "use shop_core::{Account, LIMIT};") + &format!("\npub fn over(account: &Account) -> bool {{\n    account.balance {op} LIMIT\n}}\n");
    run(repo, &["old-step", "begin", "move `over` to the app"], 0);
    write(repo, "crates/shop-core/src/lib.rs", &core_after);
    write(repo, "crates/shop-app/src/main.rs", &app_after(">"));
    // Design 35.3: Semilla derives each move. The claim `move-item` is gone.
    let gone = semilla(repo, &["claim", "move-item", "--at", "shop_core::over", "to=shop_app (bin \"shop-app\")::over"]);
    assert_eq!(gone.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&gone.stderr).contains("the claim `move-item` is gone: Semilla derives each rename and each move"), "{}", String::from_utf8_lossy(&gone.stderr));
    let check = run(repo, &["check", "--full"], 0);
    // Design 50: the `use` line gets the new name `LIMIT` because the moved item uses it, and
    // the name gave the same item in the old module. The move fact covers the line.
    assert!(check.contains("moved `shop_core::over` → `shop_app (bin \"shop-app\")::over`, with 1 `use` line(s) [move · code equal]"), "{check}");
    assert!(!check.contains("uncovered"), "{check}");

    // A move with a changed body (batch 7b, part 1): the move fact, and the edit fact has the
    // body.
    write(repo, "crates/shop-app/src/main.rs", &app_after(">="));
    let check = run(repo, &["check", "--full"], 0);
    assert!(fact_line(&check, "moved `shop_core::over` → `shop_app (bin \"shop-app\")::over`").ends_with("[move · code equal]"), "{check}");
    assert!(fact_line(&check, "changed operator `account.balance > LIMIT` → `account.balance >= LIMIT` at `shop_app (bin \"shop-app\")::over/tail`").ends_with("[semantics · code different]"), "{check}");
    assert!(check.contains("affected bodies: shop_app (bin \"shop-app\")::over") && !check.contains("in a step with no other change") && !check.contains("looks like a move"), "{check}");
    assert!(!check.contains("inactive code"), "{check}");
}

#[test]
fn a_note_belongs_to_its_commit_and_tag_needs_an_accepted_set() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let commit = |message: &str| git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", message]);
    // A rejected change set: `tag` refuses it.
    run(repo, &["old-step", "begin", "raise the max"], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    // A false claim: `main` prints a line.
    run(repo, &["claim", "no-io", "--at", "shop_app (bin \"shop-app\")::main"], 0);
    run(repo, &["old-step", "end"], 0);
    run(repo, &["group", "G1", "--kind", "fix", "--title", "Raise the max", "--steps", "0"], 0);
    commit("raise the max");
    let refused = semilla(repo, &["tag"]);
    assert_ne!(refused.status.code(), Some(0));
    let text = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(text.contains("`tag` stores nothing"), "{text}");
    // Brief 5f, B1: a rejected final check has "result: rejected" as its first line.
    assert!(text.starts_with("result: rejected · 1 step · ") && !text.contains("error:") && !text.contains("result: accepted"), "{text}");
    // Without the false claim it is accepted, and `tag` stores it.
    run(repo, &["old-step", "reopen"], 0);
    run(repo, &["unclaim", "c1"], 0);
    run(repo, &["old-step", "end"], 0);
    run(repo, &["group", "G1", "--kind", "fix", "--title", "Raise the max", "--steps", "0"], 0);
    // A changed session starts its check: `status` is not `stale` with no job.
    let status = run(repo, &["status"], 0);
    assert!(!status.starts_with("session: stale; 0 job(s)"), "{status}");
    run(repo, &["tag"], 0);
    let honest = git(repo, &["rev-parse", "HEAD"]);
    // A copied note does not make another commit accepted.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "99"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--amend", "-am", "raise the max"]);
    let other = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["notes", "--ref=semilla", "copy", &honest, &other]);
    let status = run(repo, &["status", "HEAD"], 1);
    assert!(status.contains("rejected") && status.contains("does not belong to this commit"), "{status}");
    let review = semilla(repo, &["review", "HEAD", "--md", "/dev/null"]);
    assert!(String::from_utf8_lossy(&review.stderr).contains("does not belong to this commit"));
    let load = semilla(repo, &["load", "HEAD"]);
    assert!(String::from_utf8_lossy(&load.stderr).contains("does not belong to this commit"));
}

#[test]
fn each_result_states_the_base_commit_and_its_check() {
    // Design 25.2 (trial 4, cheat C3): a commit with plain `git` before an honest change set.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let commit = |message: &str| git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", message]);
    let short = |rev: &str| git(repo, &["rev-parse", "--short=7", rev]).trim().to_string();
    // No commit has a check: the first step tells that.
    let first = short("HEAD");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let step = run(repo, &["step", "raise the max"], 0);
    assert!(step.contains(&format!("base `{first}`: no Semilla check (no checked commit before it)")), "{step}");
    commit("raise the max");
    let tag = run(repo, &["tag"], 0);
    assert!(tag.contains(&format!("base `{first}`: no Semilla check")), "{tag}");
    let checked = short("HEAD");
    // A change set directly on the checked commit.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "30"));
    let step = run(repo, &["step", "raise the max again"], 0);
    assert!(step.contains(&format!("base `{checked}`: it has a Semilla check")), "{step}");
    commit("raise the max again");
    run(repo, &["tag"], 0);
    let checked = short("HEAD");
    // Two commits with no step, then an honest change set.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "40"));
    commit("tidy");
    let tidy = short("HEAD");
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "50"));
    // Batch 5w, D1: a commit subject is text of the agent. It shows in quote marks, and each
    // hidden character shows as an escape.
    commit("tidy \"more\"\u{1b}[8m");
    let more = short("HEAD");
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "60"));
    let line = format!("! 2 commits before this change set have no Semilla check: `{more}` \"tidy \\\"more\\\"\\u{{1b}}[8m\", `{tidy}` \"tidy\" (the last checked commit is `{checked}`)");
    let step = run(repo, &["step", "the honest step"], 0);
    assert!(step.contains(&line), "{step}");
    // The session status has the line before the commit also.
    let status = run(repo, &["status"], 0);
    assert!(status.contains(&line), "{status}");
    commit("the honest step");
    let tag = run(repo, &["tag"], 0);
    assert!(tag.contains(&line) && tag.contains("result: accepted"), "the line is a warning, not a rejection: {tag}");
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains(&line), "{status}");
    let md = repo.join(".semilla/review.md");
    run(repo, &["review", "HEAD", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    // The Markdown report adds a backslash before each character that Markdown reads.
    let md_line = format!("`{more}` \"tidy \\\\\"more\\\\\"\\\\u{{1b}}\\[8m\", `{tidy}` \"tidy\" (the last checked commit is `{checked}`)");
    assert!(text.lines().take(6).any(|l| l.contains("2 commits before this change set have no Semilla check") && l.contains(&md_line)), "the line is at the top of the report: {text}");
    let html = repo.join(".semilla/review.html");
    run(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains(&format!("(the last checked commit is `{checked}`)")), "the HTML report has the line");
    // The answer comes from git, not from the note: without the note of the checked commit, no
    // commit before the change set has a check.
    git(repo, &["notes", "--ref=semilla", "remove", &checked]);
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains("! 3 commits before this change set have no Semilla check") && status.contains(&format!("`{checked}` \"raise the max again\"")), "{status}");
}

#[test]
fn a_note_that_does_not_agree_with_the_step_refs_is_not_accepted() {
    // Trial 4, D12: `tag` keeps the tree of each step under `refs/semilla/<commit>/`. A note with
    // other step trees is not the note of `tag`.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "raise the max"], 0);
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "30"));
    run(repo, &["step", "raise the max again"], 0);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "raise the max"]);
    run(repo, &["tag"], 0);
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(!status.contains("does not agree"), "{status}");
    // A forged note: one step from the base to the head. The trees of the commit agree with it.
    let note = git(repo, &["notes", "--ref=semilla", "show", "HEAD"]);
    let mut forged: serde_json::Value = serde_json::from_str(&note).unwrap();
    let last = forged["steps"][1]["tree_after"].clone();
    forged["steps"][0]["tree_after"] = last;
    forged["steps"].as_array_mut().unwrap().pop();
    let file = repo.join(".semilla/forged.json");
    std::fs::write(&file, forged.to_string()).unwrap();
    git(repo, &["notes", "--ref=semilla", "add", "-f", "-F", file.to_str().unwrap(), "HEAD"]);
    let status = run(repo, &["status", "HEAD"], 1);
    assert!(status.starts_with("the note does not agree with the step refs"), "{status}");
    assert!(status.contains("result: rejected") && !status.contains("accepted"), "{status}");
    let review = semilla(repo, &["review", "HEAD", "--md", "/dev/null"]);
    assert_ne!(review.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&review.stderr).contains("the note does not agree with the step refs"), "{}", String::from_utf8_lossy(&review.stderr));
    let load = semilla(repo, &["load", "HEAD"]);
    assert_ne!(load.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&load.stderr).contains("the note does not agree with the step refs"), "{}", String::from_utf8_lossy(&load.stderr));
}

#[test]
fn an_empty_reason_and_an_empty_intent_are_rejected() {
    // Brief 5f, A4: the same class of defect as an empty `calls` entry.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "ci.sh", "echo hi\n");
    let empty = semilla(repo, &["step", "add a script", "--reason", "ci.sh:="]);
    assert_ne!(empty.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&empty.stderr).contains("the reason has no text"), "{}", String::from_utf8_lossy(&empty.stderr));
    let intent = semilla(repo, &["step", " ", "--reason", "ci.sh:=the CI runs it"]);
    assert_ne!(intent.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&intent.stderr).contains("the intent has no text"), "{}", String::from_utf8_lossy(&intent.stderr));
    // A reason with a partial anchor is not in use: the step tells it, and the next command omits it.
    let partial = run(repo, &["step", "add a script", "--reason", "ci=the CI runs it"], 1);
    assert!(partial.contains("the reason for `ci` belongs to no fact of this step"), "{partial}");
    assert!(partial.contains("run again: semilla step 'add a script' --reason 'ci.sh:=<why>'"), "{partial}");
    let stored = run(repo, &["step", "add a script", "--reason", "ci.sh:=the CI runs it"], 0);
    assert!(stored.contains("accepted") && !stored.contains("belongs to no fact"), "{stored}");
}

const READERS: &str = r#"pub mod cart;

/// The notes.
pub fn notes() -> &'static str {
    include_str!("notes.md")
}

/// The logo.
pub fn logo() -> &'static [u8] {
    include_bytes!("logo.png")
}

/// The rate.
pub fn rate() -> u64 {
    include!("rate.txt")
}

/// A path that the compiler computes.
pub fn terms() -> &'static str {
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/terms.txt"))
}

#[doc =
    include_str!("../MANUAL.md")]
pub struct Manual;

/// No checked configuration compiles this function.
#[cfg(target_os = "none")]
pub fn hidden() -> &'static str {
    include_str!("hidden.txt")
}
"#;

#[test]
fn a_file_that_a_crate_reads_at_build_time_is_a_build_input() {
    // Brief 5f, A6: each file name is on the file allow list (`*.md`, `*.txt`, an image). A crate
    // reads the file at build time, so the change is a fact with its affected bodies, or it
    // needs a reason when no body changes.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", READERS);
    write(repo, "crates/shop-core/src/notes.md", "# Notes\n");
    std::fs::write(repo.join("crates/shop-core/src/logo.png"), [0x89, b'P', b'N', b'G', 0, 1, 2, 0xff]).unwrap();
    write(repo, "crates/shop-core/src/rate.txt", "1\n");
    write(repo, "crates/shop-core/terms.txt", "terms\n");
    write(repo, "crates/shop-core/MANUAL.md", "# Manual\n");
    write(repo, "crates/shop-core/src/hidden.txt", "hidden\n");
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nnever = []\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "the readers"]);

    // A text file, a binary file, and a computed path: a fact of the category `semantics` with the
    // body that reads the file. No reason is necessary.
    for (file, body) in [("crates/shop-core/src/notes.md", "shop_core::notes"), ("crates/shop-core/terms.txt", "shop_core::terms")] {
        let old = std::fs::read_to_string(repo.join(file)).unwrap();
        write(repo, file, &old.replace("Notes", "Notes 2").replace("terms", "terms 2"));
        let out = run(repo, &["step", &format!("change {file}")], 0);
        assert!(out.contains(&format!("{file}: changed [file · code different]")) && out.contains(&format!("affected bodies: {body}")), "{out}");
    }
    // Batch 5w, D3: a file that `include!` reads is Rust code that Semilla does not parse,
    // whatever its name is. The fact has the body that reads the file, and it needs a reason.
    {
        let file = "crates/shop-core/src/rate.txt";
        let old = std::fs::read_to_string(repo.join(file)).unwrap();
        write(repo, file, &old.replace("1", "5"));
        let out = run(repo, &["step", "change the rate"], 1);
        assert!(out.contains(&format!("{file}: changed [file · code different]")) && out.contains("affected bodies: shop_core::rate"), "{out}");
        assert!(out.contains(&format!("✗ spotlight: Rust code that Semilla does not parse (a `.rs` file that no target of a member package names, or a file that `include!` reads); it needs a reason: --reason '{file}:=…'")), "{out}");
        let out = run(repo, &["step", "change the rate", "--reason", &format!("{file}:=the new rate")], 0);
        assert!(out.contains("accepted"), "{out}");
    }
    std::fs::write(repo.join("crates/shop-core/src/logo.png"), [0x89, b'P', b'N', b'G', 0, 1, 2, 0xfe, 7]).unwrap();
    let out = run(repo, &["step", "change the logo"], 0);
    assert!(out.contains("crates/shop-core/src/logo.png: changed (binary file changed) [file · code different]") && out.contains("affected bodies: shop_core::logo"), "{out}");
    // The report shows the content: a diff, or the size of a binary file.
    let md = repo.join(".semilla/review.md");
    run(repo, &["status", "--wait", "600"], 0);
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("binary, 8 bytes → binary, 9 bytes") || text.contains("binary, 9 bytes"), "{text}");
    assert!(text.contains("+# Notes 2") || text.contains("+ # Notes 2"), "{text}");

    // A doc attribute on two lines, and code that no checked configuration compiles: no body
    // changes, so the fact has the class `other` and needs a reason.
    for (file, reader) in [("crates/shop-core/MANUAL.md", "docs and doc tests"), ("crates/shop-core/src/hidden.txt", "reads this file at build time")] {
        write(repo, file, "new text\n");
        let out = run(repo, &["step", &format!("change {file}")], 1);
        assert!(out.contains(&format!("{file}: changed [file · no code]")) && out.contains("needs a reason") && out.contains(reader), "{out}");
        let out = run(repo, &["step", &format!("change {file}"), "--reason", &format!("{file}:=the text is clearer")], 0);
        assert!(out.contains("accepted"), "{out}");
    }
}

#[test]
fn an_include_file_is_code_with_each_name_and_each_content() {
    // Batch 5w, D3 (trial 7 on humantime). A step moves an expression to a new file with a name
    // of the allow list, and reads it with `include!`. The file fact was `added [other]` with no
    // body. A file with the same content needed a reason (`Uses: include!`), and a file with a
    // different content needed no reason. Now the two cases agree: the file fact has the body
    // that reads the file, and it needs one reason.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n\npub fn rate() -> u64 {\n    7\n}\n\npub fn fee() -> u64 {\n    3\n}\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "two functions"]);
    let lib = std::fs::read_to_string(repo.join("crates/shop-core/src/lib.rs")).unwrap();
    for (name, function, old, content, equal) in [("NOTICE.txt", "rate", "    7\n", "7\n", true), ("FEE.txt", "fee", "    3\n", "4\n", false)] {
        let file = format!("crates/shop-core/src/{name}");
        write(repo, &file, content);
        let lib = std::fs::read_to_string(repo.join("crates/shop-core/src/lib.rs")).unwrap().replace(old, &format!("    include!(\"{name}\")\n"));
        write(repo, "crates/shop-core/src/lib.rs", &lib);
        let out = run(repo, &["step", &format!("move the value of {function}")], 1);
        assert!(out.contains(&format!("{file}: added [file · code different]")) && out.contains(&format!("affected bodies: shop_core::{function}")), "{out}");
        assert!(out.contains(&format!("✗ spotlight: Rust code that Semilla does not parse (a `.rs` file that no target of a member package names, or a file that `include!` reads); it needs a reason: --reason '{file}:=…'")), "{out}");
        // One reason is sufficient in the two cases: the `include!` call is not a second cause.
        assert!(!out.contains(CONFIG_NOTE) && out.matches("spotlight:").count() == 1, "{out}");
        assert_eq!(out.matches("--reason '").count(), 2, "one spotlight line and one `run again` line: {out}");
        // The compare result of the edit facts shows if the content is the same.
        assert_eq!(out.contains("[semantics · code equal]"), equal, "{out}");
        let out = run(repo, &["step", &format!("move the value of {function}"), "--reason", &format!("{file}:=the value is in a file now")], 0);
        assert!(out.starts_with("accepted"), "{out}");
    }
    assert!(lib.contains("    7\n"));
}

#[test]
fn a_tagged_change_set_is_closed() {
    // Brief 5f, B1 and D1.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let commit = |args: &[&str]| {
        git(repo, &[&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q"], args].concat());
        git_out(repo, &["rev-parse", "HEAD"])
    };
    let base = git_out(repo, &["rev-parse", "HEAD"]);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "raise the max"], 0);
    let first = commit(&["-am", "raise the max"]);
    let tagged = run(repo, &["tag"], 0);
    // D1: the result line has the counts that the reviewer reads, and no score.
    let line = tagged.lines().next().unwrap();
    assert_eq!(line, "result: accepted · 1 step · 1 fact: 1 code different · 0 claims", "{tagged}");
    assert!(!tagged.contains("score") && !tagged.contains("uncovered"), "{tagged}");

    // After `tag`, no session is open.
    let closed = format!("no open change set; the last tagged commit is {first}");
    let status = run(repo, &["status"], 0);
    assert!(status.starts_with(&closed), "{status}");

    // A new commit with no step on the tagged commit. Design 62: it gets a tag after the
    // commit, and the session stays the closed change set of the first commit.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let second = commit(&["-am", "rename the sum"]);
    let out = semilla_env(repo, &["tag"], &[]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.starts_with("result: tagged after the commit · 1 step · ") && text.contains(&format!("tagged {second} after the commit")), "{text}");
    assert!(!text.contains(&base), "{text}");
    assert!(run(repo, &["status"], 0).starts_with(&closed));
    // The steps of the agent for the same work: an accepted change set on the tagged commit.
    git(repo, &["reset", "-q", "--soft", &first]);
    let out = run(repo, &["step", "rename the sum"], 0);
    assert!(out.contains("tagged and closed") && out.contains("accepted"), "{out}");
    // The session is open now: `status` shows it, with no hidden command.
    let status = run(repo, &["status"], 0);
    assert!(status.starts_with("session: ") && !status.contains("semilla check"), "{status}");
    let second = commit(&["-m", "rename the sum"]);
    assert_eq!(git_out(repo, &["rev-parse", "HEAD^"]), first, "the tagged commit stays");
    let out = run(repo, &["tag"], 0);
    assert!(out.starts_with("result: accepted · 1 step · "), "{out}");
    assert!(run(repo, &["status", &first], 0).contains("result: accepted"), "the first change set stays accepted");

    // A rejected first step of a new change set opens no session.
    let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
    write(repo, "Cargo.toml", &format!("{manifest}\n[profile.dev]\noverflow-checks = false\n"));
    let out = semilla(repo, &["step", "no overflow checks"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stdout));
    let status = run(repo, &["status"], 0);
    assert!(status.starts_with(&format!("no open change set; the last tagged commit is {second}")), "{status}");
    write(repo, "Cargo.toml", &manifest);

    // An edit after the last step: the `tag` error gives the commands, and they work.
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    run(repo, &["step", "an empty cart has the total 0"], 0);
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "30"));
    commit(&["-am", "an empty cart"]);
    let out = semilla(repo, &["tag"]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success() && text.contains("Run `semilla step \"<intent>\"` for the edit, then `git commit --amend --no-edit`, then `semilla tag`."), "{text}");
    run(repo, &["step", "raise the max again"], 0);
    commit(&["--amend", "--no-edit"]);
    let out = run(repo, &["tag"], 0);
    assert!(out.starts_with("result: accepted · 2 steps · "), "{out}");

    // Brief 5h, C4 (D8): `status` with no open change set says if `HEAD` has a check. `HEAD` is
    // the tagged commit: no line. After `git commit --amend`, `HEAD` is a commit with no check.
    let status = run(repo, &["status"], 0);
    assert!(status.starts_with("no open change set; the last tagged commit is ") && !status.contains("HEAD `"), "{status}");
    commit(&["--amend", "-m", "an empty cart, with a new message"]);
    let head = git(repo, &["rev-parse", "--short=7", "HEAD"]);
    let status = run(repo, &["status"], 0);
    assert!(status.starts_with("no open change set; the last tagged commit is ") && status.contains(&format!("HEAD `{head}` has no Semilla check\n")), "{status}");

    // A final check that rejects the base: the first line is the rejection, with no "error:"
    // and no "accepted" line above the problem.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "40"));
    run(repo, &["step", "raise the max to 40"], 0);
    commit(&["-am", "40"]);
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "50"));
    commit(&["-am", "50, with no step"]);
    let out = semilla(repo, &["tag", "HEAD~1"]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success() && text.starts_with("result: rejected · 1 step · "), "{text}");
    assert!(!text.contains("result: accepted") && !text.contains("error:") && text.contains("`tag` stores nothing"), "{text}");
}

#[test]
fn a_profile_change_shows_its_affected_bodies() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // `[profile.dev] overflow-checks = false` changes the MIR of each addition. N2 and N3 must use
    // the profile of the tree, so the change is not equal at a higher level.
    //
    // The base has a function with an addition of two `u32` values. The loop of `Cart::total` has
    // no checked addition: `s += x` with `x: &u32` is a call to the `AddAssign` impl of `core`.
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}\npub fn add(a: u32, b: u32) -> u32 {{\n    a + b\n}}\n"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "add"]);
    run(repo, &["old-step", "begin", "no overflow checks"], 0);
    let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
    write(repo, "Cargo.toml", &format!("{manifest}\n[profile.dev]\noverflow-checks = false\n"));
    let check = run(repo, &["check", "--full"], 1);
    assert!(check.contains("profile.dev.overflow-checks added (false)"), "{check}");
    // Only the body with the addition changes. Before batch 5g, `shop_app (bin "shop-app")::main` was in the list
    // with no change of its MIR: the test build had two bodies with this name (the `main` of the
    // user and the `main` of the test harness), and the profile changes the key of each target.
    assert!(check.contains("affected bodies: shop_core::cart::add\n"), "{check}");
    assert!(!check.contains("shop_app (bin \"shop-app\")::main") && !check.contains("test-harness-main"), "{check}");
}

#[test]
fn status_checks_again_and_tag_closes_the_change_set() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    run(repo, &["old-step", "begin", "raise the max"], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    // T2 and T3: the `affects` argument is retired.
    let retired = semilla(repo, &["claim", "no-panic", "--at", "shop_app (bin \"shop-app\")::main", "affects=shop_app (bin \"shop-app\")::main"]);
    assert!(String::from_utf8_lossy(&retired.stderr).contains("`affects` is retired"), "{}", String::from_utf8_lossy(&retired.stderr));
    run(repo, &["old-step", "end"], 0);
    run(repo, &["group", "G1", "--kind", "fix", "--title", "Raise the max", "--steps", "0"], 0);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "raise the max"]);
    run(repo, &["tag"], 0);
    let status = run(repo, &["status", "HEAD", "--wait", "600"], 0);
    assert!(status.contains("ready") && status.contains("computed on this machine"), "{status}");

    // T6 (trial 2, A7d): a forged report note and no local report. `status` does not print the
    // note; it computes phase 1 again and starts phase 2.
    let note = git(repo, &["notes", "--ref=semilla-report", "show", "HEAD"]);
    let mut forged: serde_json::Value = serde_json::from_str(&note).unwrap();
    forged["text"] = serde_json::json!("FORGED: tidy the limit check\n");
    let file = repo.join(".semilla/forged.json");
    std::fs::write(&file, forged.to_string()).unwrap();
    git(repo, &["notes", "--ref=semilla-report", "add", "-f", "-F", file.to_str().unwrap(), "HEAD"]);
    std::fs::remove_dir_all(repo_cache(repo).join("reports")).unwrap();
    let output = semilla(repo, &["status", "HEAD", "--no-wait"]);
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(!status.contains("FORGED"), "{status}");
    assert!(status.contains("phase 1 (computed now)") && status.contains("result: phase 1 ok"), "{status}");
    assert!(status.contains("not checked on this machine"), "{status}");
    let status = run(repo, &["status", "HEAD", "--wait", "600"], 0);
    assert!(!status.contains("FORGED") && status.contains("ready"), "{status}");

    // T8: after `tag`, the next step starts a new change set at HEAD.
    let begin = run(repo, &["old-step", "begin", "the next change"], 0);
    assert!(begin.contains("tagged and closed") && begin.contains("step 0 started"), "{begin}");
}

/// Design 58.8: the step output, `status`, and `tag` have the count line of the comments of
/// the changed items that have no edit.
// rule+ R58.8-1
#[test]
fn the_step_status_and_tag_show_the_count_of_the_comments_with_no_edit() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let docs = CART_V0.replace("    pub fn total(", "    /// Gives the sum of the items.\n    pub fn total(");
    assert_ne!(docs, CART_V0);
    write(repo, "crates/shop-core/src/cart.rs", &docs);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "docs"]);
    const COUNT: &str = "comments: 1 comment(s) of the changed items have no edit in this step";
    let default_mode = [];
    let text = |output: Output| -> String {
        assert_eq!(output.status.code(), Some(0), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    // No step: no count line.
    let status = text(semilla_env(repo, &["status"], &default_mode));
    assert!(!status.contains("comments: "), "{status}");
    write(repo, "crates/shop-core/src/cart.rs", &docs.replace("let mut s = 0;", "let mut s = 1;"));
    let step = text(semilla_env(repo, &["step", "start the sum at 1"], &default_mode));
    assert!(step.contains("docs of `shop_core::cart::Cart::total`, no edit in this step") && step.contains(&format!("  {COUNT}\n")), "{step}");
    let status = text(semilla_env(repo, &["status"], &default_mode));
    assert!(status.contains(COUNT), "{status}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "start the sum at 1"]);
    let tagged = text(semilla_env(repo, &["tag"], &default_mode));
    assert!(tagged.contains(COUNT), "{tagged}");
}

#[test]
fn one_command_for_each_step() {
    // Design 24.3: `semilla step "<intent>"` checks and stores a step; a rejected step stores
    // nothing and prints the command to run again.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
    write(repo, "Cargo.toml", &format!("{manifest}\n[profile.dev]\noverflow-checks = false\n"));
    let output = semilla(repo, &["step", "no overflow checks"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.starts_with("rejected: step 0") && text.contains("run again: semilla step 'no overflow checks' --reason 'Cargo.toml:profile.dev.overflow-checks=<why>'"), "{text}");
    let session = std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap_or_default();
    // The session has the record of the rejection for `tag` (batch 5x, part C), and no step.
    let stored: serde_json::Value = serde_json::from_str(&session).unwrap_or_default();
    assert!(stored["changeset"]["steps"].as_array().is_none_or(|steps| steps.is_empty()) && stored["open"].is_null(), "a rejected step is not stored: {session}");
    assert_eq!(stored["rejected"]["intent"], "no overflow checks", "{session}");
    let stored = run(repo, &["step", "no overflow checks", "--reason", "Cargo.toml:profile.dev.overflow-checks=the benchmark needs it"], 0);
    assert!(stored.starts_with("accepted"), "{stored}");
    // `undo` removes the last step; the working tree keeps the change.
    let undone = run(repo, &["undo"], 0);
    assert!(undone.contains("step 0"), "{undone}");
    // C9: the exact command that restores the files, and the warning about `git checkout .`.
    assert!(undone.contains("git restore --source=") && undone.contains("do not use `git checkout .`"), "{undone}");
    let tree = undone.split("--source=").nth(1).and_then(|rest| rest.split(' ').next()).unwrap().to_string();
    let copy = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
    git(repo, &["restore", &format!("--source={tree}"), "--worktree", "--", "."]);
    assert!(!std::fs::read_to_string(repo.join("Cargo.toml")).unwrap().contains("overflow-checks"), "the restore goes back to the step before");
    std::fs::write(repo.join("Cargo.toml"), copy).unwrap();
    run(repo, &["step", "no overflow checks", "--group", "Build", "--reason", "Cargo.toml:profile.dev.overflow-checks=the benchmark needs it"], 0);
    // A rename with a position anchor runs phase 2 in the command; the old claims are retired.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let retired = semilla(repo, &["step", "raise the max", "--claim", "change-literal shop_app (bin \"shop-app\")::main/S1/args[1]/args[0]"]);
    assert!(String::from_utf8_lossy(&retired.stderr).contains("is retired"));
    run(repo, &["step", "raise the max", "--group", "Build"], 0);
    // No `group` command: `verify --final` and `tag` work with the derived groups.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "work"]);
    let tagged = run(repo, &["tag"], 0);
    assert!(tagged.starts_with("result: accepted · 2 steps"), "{tagged}");
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains("ready"), "tag stores phase 2: {status}");
    // Brief 5f, D2: `status` prints one line for each step and the result line. `--verbose`
    // prints the facts.
    let step_lines: Vec<&str> = status.lines().filter(|line| line.starts_with("step ")).collect();
    assert_eq!(step_lines.len(), 2, "{status}");
    assert!(step_lines[0].starts_with("step 0  \"no overflow checks\" · 1 fact: "), "{status}");
    assert!(step_lines[1].starts_with("step 1  \"raise the max\" · "), "{status}");
    assert!(status.contains("\nresult: accepted · 2 steps") && !status.contains("f1 derived"), "{status}");
    // Batch 5y, C4.5: the hint for a commit names the commit.
    assert!(status.contains("run `semilla status HEAD --verbose`"), "{status}");
    let verbose = run(repo, &["status", "HEAD", "--verbose"], 0);
    assert!(verbose.contains("f1 derived") && verbose.contains("\nresult: accepted · 2 steps"), "{verbose}");
    // The default report is short. `--full` adds the change tree and the sources.
    let out = common::TestDir::new();
    let (short, full) = (out.path().join("short.md"), out.path().join("full.md"));
    run(repo, &["review", "HEAD", "--md", short.to_str().unwrap()], 0);
    run(repo, &["review", "HEAD", "--md", full.to_str().unwrap(), "--full"], 0);
    let (short, full) = (std::fs::read_to_string(short).unwrap(), std::fs::read_to_string(full).unwrap());
    assert!(!short.contains("## Change tree") && !short.contains("## Sources") && short.contains("--full"), "{short}");
    assert!(full.contains("## Change tree") && full.contains("## Sources"), "{full}");
    // The old protocol is in the hidden command `old-step`.
    run(repo, &["old-step", "begin", "back"], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V0);
    run(repo, &["old-step", "end"], 0);
}

#[test]
fn the_first_argument_of_step_is_always_the_intent() {
    // Batch 5g, item 2: `semilla step "edit"` ran the old subcommand `step edit`, and
    // `semilla step "reopen"` opened the last step again.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    for word in ["begin", "end", "reopen", "edit", "drop", "help"] {
        let stored = run(repo, &["step", word], 0);
        assert!(stored.starts_with(&format!("accepted: step 0 \"{word}\"")), "{stored}");
        run(repo, &["undo"], 0);
    }
    // No help text and no message of the usual flow names an old subcommand of `step`.
    let old_names = |text: &str| ["begin", "end", "reopen", "edit", "drop"].iter().any(|name| text.contains(&format!("semilla step {name}")) || text.contains(&format!("`step {name}")) || text.contains(&format!("step_{name}")));
    for args in [&["--help"][..], &["step", "--help"], &["help", "step"]] {
        let help = run(repo, args, 0);
        assert!(!old_names(&help) && !help.contains("begin"), "{help}");
    }
    // Batch 5l, B1: the help names no hidden command. The help of a hidden command still works.
    let help = run(repo, &["--help"], 0);
    assert!(help.trim_end().ends_with("The commands of the old protocol are hidden. Only a replay by hand uses them (see `README.md`).") && !help.contains("old-step"), "{help}");
    for hidden in ["old-step", "claim", "unclaim", "reason", "anchor", "check", "suggest-claims", "group", "subgroups", "show"] {
        // A word such as "check" is also a usual word: look for the command forms.
        assert!(!help.contains(&format!("semilla {hidden}")) && !help.contains(&format!("{hidden},")) && !help.lines().any(|line| line.trim_start().starts_with(&format!("{hidden} "))), "{hidden}: {help}");
        assert!(run(repo, &[hidden, "--help"], 0).contains("old protocol"), "{hidden}");
    }
    // A command of the old protocol with no open step names the usual flow.
    for (args, flag) in [(&["claim", "no-panic", "--at", "shop_core::cart::Cart::total"][..], "--claim"), (&["reason", "--at", "shop_app (bin \"shop-app\")::main", "why"], "--reason"), (&["unclaim", "c1"], "semilla undo")] {
        let output = semilla(repo, args);
        let error = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(output.status.code(), Some(2), "{error}");
        assert!(error.contains("old protocol") && error.contains(flag) && !old_names(&error), "{error}");
    }
    // The old protocol is in a hidden command with another name. `semilla step` refuses to run
    // while such a step is open, and it names the way out.
    git(repo, &["checkout", "-q", "."]);
    run(repo, &["old-step", "begin", "by hand"], 0);
    let output = semilla(repo, &["step", "edit"]);
    let error = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(error.contains("`semilla old-step end`") && error.contains("`semilla old-step drop`") && !old_names(&error), "{error}");
    run(repo, &["old-step", "drop"], 0);
    // The message of a replay names the plan.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let begun = run(repo, &["replay", "begin"], 0);
    assert!(begun.contains("`semilla replay plan`") && !old_names(&begun), "{begun}");
    let output = semilla(repo, &["step", "end"]);
    let error = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(error.contains("a replay is in progress") && error.contains("`semilla replay apply`") && !old_names(&error), "{error}");
}

#[test]
fn the_test_harness_main_is_not_a_body_of_the_user() {
    // Batch 5g, item 1. In the test build of a binary crate, rustc generates a second function
    // with the def path `<crate>::main`. Its body holds the list of the tests. The change of this
    // body was a fact "indirect change, cause not known" on the `main` function of the user.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let clean = |out: &str| assert!(out.starts_with("accepted") && !out.contains("indirect change") && !out.contains("does not see") && !out.contains("test-harness") && !out.contains("needs a reason"), "{out}");
    let one = "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {\n        assert_eq!(1 + 1, 2);\n    }\n}\n";
    let two = "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {\n        assert_eq!(1 + 1, 2);\n    }\n\n    #[test]\n    fn two() {\n        assert_eq!(2 + 2, 4);\n    }\n}\n";
    // A binary crate: the first test, a second test, and then the last test goes away.
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}{one}"));
    clean(&run(repo, &["step", "the first test of the binary crate"], 0));
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}{two}"));
    clean(&run(repo, &["step", "a second test of the binary crate"], 0));
    write(repo, "crates/shop-app/src/main.rs", MAIN_V0);
    let removed = run(repo, &["step", "remove the tests of the binary crate"], 0);
    clean(&removed);
    assert!(removed.contains("removed `shop_app (bin \"shop-app\")::tests`") && removed.contains("shop_app (bin \"shop-app\")::tests::one"), "the test facts own the change: {removed}");
    // A library crate: the first test, and then the last test goes away.
    write(repo, "crates/shop-core/src/lib.rs", &format!("pub mod cart;\n{one}"));
    clean(&run(repo, &["step", "the first test of the library crate"], 0));
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n");
    clean(&run(repo, &["step", "remove the test of the library crate"], 0));
    // The `main` function of the user keeps the full check. This step adds the first test, and
    // it changes the function that `main` calls with no edit of the text of `main`: a trait
    // method with `self` by value wins over the inherent method with `&self`.
    let hidden = "\ntrait Hidden {\n    fn total(self, max: u32) -> u32;\n}\n\nimpl Hidden for shop_core::cart::Cart {\n    fn total(self, _max: u32) -> u32 {\n        0\n    }\n}\n";
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}{hidden}{one}"));
    let output = semilla(repo, &["step", "the first test again"]);
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(text.lines().any(|line| line.contains("affected bodies:") && line.contains("shop_app (bin \"shop-app\")::main")), "the changed `main` of the user shows: {text}");
    // The same check with the last test removed, and with an edit of the text of `main`.
    if output.status.code() != Some(0) {
        run(repo, &["step", "the first test again", "--reason", "shop_app (bin \"shop-app\")::main=the new trait method wins"], 0);
    }
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V1}{hidden}"));
    let output = semilla(repo, &["step", "remove the test and raise the max"]);
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(text.contains("shop_app (bin \"shop-app\")::main"), "the edit of `main` shows: {text}");
    assert!(text.contains("removed `shop_app (bin \"shop-app\")::tests`"), "{text}");
}

#[test]
fn a_rename_and_a_move_need_no_claim() {
    // Design 24.2, rule 4, and design 36.1: Semilla derives the identity, also with a second
    // change in the step.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let core = "pub const LIMIT: u32 = 10;\n\npub mod cart;\n\n#[derive(Debug)]\npub struct Account {\n    pub balance: u32,\n}\n\npub fn over(account: &Account) -> bool {\n    account.balance > LIMIT\n}\n";
    let app = "use shop_core::{Account, over};\n\nfn main() {\n    let account = Account { balance: 7 };\n    println!(\"{} {account:?}\", over(&account));\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", core);
    write(repo, "crates/shop-app/src/main.rs", app);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "over"]);

    let core = core.replace("balance", "cents");
    let app = app.replace("balance", "cents");
    write(repo, "crates/shop-core/src/lib.rs", &core);
    write(repo, "crates/shop-app/src/main.rs", &app);
    let renamed = run(repo, &["step", "rename balance to cents"], 0);
    assert!(renamed.starts_with("accepted: step 0") && renamed.contains("renamed `shop_core::Account.balance` → `shop_core::Account.cents` [rename · code different]\n      ! the compiled code changes: <shop_core::Account as std::fmt::Debug>::fmt"), "{renamed}");
    // The field is a part of the public API: the rename gets one more fact (batch 5x,
    // round 3).
    assert!(renamed.contains("public API: `shop_core::Account.balance` is now `shop_core::Account.cents` (a different crate can no longer use the old path) [rename · no code]"), "{renamed}");
    assert_eq!(renamed.matches('◆').count(), 2, "the forced edits fold under the identity fact: {renamed}");

    // A move with a second change (batch 7b, part 1): the move fact and the edit fact.
    let core_after = core.replace("\npub fn over(account: &Account) -> bool {\n    account.cents > LIMIT\n}\n", "");
    let app_after = |op: &str| app.replace("use shop_core::{Account, over};", "use shop_core::{Account, LIMIT};") + &format!("\npub fn over(account: &Account) -> bool {{\n    account.cents {op} LIMIT\n}}\n");
    write(repo, "crates/shop-core/src/lib.rs", &core_after);
    write(repo, "crates/shop-app/src/main.rs", &app_after(">="));
    let hidden = run(repo, &["step", "move over to the app"], 0);
    assert!(fact_line(&hidden, "moved `shop_core::over` → `shop_app (bin \"shop-app\")::over`").ends_with("[move · code equal]"), "{hidden}");
    assert!(fact_line(&hidden, "changed operator `account.cents > LIMIT` → `account.cents >= LIMIT` at `shop_app (bin \"shop-app\")::over/tail`").ends_with("[semantics · code different]"), "{hidden}");
    assert!(!hidden.contains("removed `shop_core::over`") && !hidden.contains("in a step with no other change") && !hidden.contains("looks like a move"), "{hidden}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-app/src/main.rs", &app_after(">"));
    let moved = run(repo, &["step", "move over to the app"], 0);
    assert!(moved.contains("moved `shop_core::over` → `shop_app (bin \"shop-app\")::over`") && moved.contains("[move · code equal]"), "{moved}");
    assert!(moved.contains("public API: a different crate can no longer use `shop_core::over` [move · no code]"), "{moved}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "work"]);
    let tagged = run(repo, &["tag"], 0);
    assert!(tagged.starts_with("result: accepted · 2 steps · ") && tagged.lines().next().unwrap().ends_with(" · 0 claims"), "{tagged}");
}

#[test]
fn the_guide_examples_of_calls_only_and_rewrite_pattern() {
    // The examples of `docs/agent-guide.md`, run through the CLI.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let show = "\npub fn show(cart: &Cart) -> u32 {\n    cart.total(10) + cart.total(20)\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{show}"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "show"]);

    let wide = "\npub fn sum_wide(v: &[u32]) -> u64 {\n    v.iter().map(|x| *x as u64).sum()\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{show}{wide}"));
    // Each call of `sum_wide` goes to `core`: the empty list is a true claim (brief 5g, item 3).
    let out = run(repo, &["step", "add sum_wide", "--claim", "calls-only shop_core::cart::sum_wide calls=[]"], 0);
    assert!(out.contains("verified (mir)") && out.contains("added `shop_core::cart::sum_wide`"), "{out}");
    assert!(out.contains("scope: each call to another crate is below an allowed path; calls to `core` and `alloc` are always allowed; examined:"), "{out}");
    run(repo, &["undo"], 0);
    // A function that uses a type with derives, and that writes a file. The derived impls call
    // only `core`, so `std::fs` is the one entry that the claim needs.
    let save = "\n#[derive(Debug, Clone, PartialEq)]\npub struct Receipt {\n    pub total: u64,\n}\n\npub fn save(receipt: &Receipt) -> u64 {\n    let _ = std::fs::write(\"/tmp/semilla-receipt.txt\", format!(\"{receipt:?}\"));\n    receipt.clone().total\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{show}{wide}{save}"));
    // A call outside the list refutes the claim, and the step is not stored. The command to run
    // again has the claim with the complete list, and the line before it says so.
    let out = semilla(repo, &["step", "add sum_wide and save", "--claim", "calls-only shop_core::cart::save calls=[]", "--claim", "no-io shop_core::cart::save"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("is refuted: external calls outside the list of `calls`: `std::fs::write` in `shop_core::cart::save` (calls to `core` and `alloc` are always allowed)"), "{text}");
    let lines: Vec<&str> = text.trim_end().lines().collect();
    assert_eq!(lines[lines.len() - 2], "the command below changes the claim c1: it adds `std::fs` to `calls`. Use it only if the function must make these calls. If not, change the code.", "{text}");
    // C3: the command to run again is the last line.
    assert_eq!(
        lines[lines.len() - 1],
        r#"run again (without the claim that failed, and with the complete list of `calls`): semilla step 'add sum_wide and save' --claim 'calls-only shop_core::cart::save calls=["std::fs"]'"#,
        "{text}"
    );
    // Brief 5g, item 4: the full text of a failed claim is in the first lines only. The claim
    // list has the status and a short cause.
    assert_eq!(text.matches("`std::fs::write` in `shop_core::cart::save`").count(), 2, "one time for each claim: {text}");
    assert!(text.contains("\n  ✗ c1 refuted: external calls outside the list of `calls`\n  ✗ c2 refuted: external calls\n"), "{text}");
    // The command of the last line is accepted.
    let out = run(repo, &["step", "add sum_wide and save", "--claim", r#"calls-only shop_core::cart::save calls=["std::fs"]"#], 0);
    assert!(out.contains("✓ c1 verified (mir)"), "{out}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{show}{wide}"));
    // A claim with no complete list (`no-io`) goes out of the command.
    let out = semilla(repo, &["step", "add sum_wide", "--claim", "no-panic shop_core::cart::show"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("is refuted") && text.trim_end().ends_with("run again (without the claim that failed): semilla step 'add sum_wide'"), "{text}");
    run(repo, &["step", "add sum_wide"], 0);

    let capped = show.replace("cart.total(10) + cart.total(20)", "cart.total(10).min(9) + cart.total(20).min(9)");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{capped}{wide}"));
    // Design 54.3: the pattern is a `same` fact of the agent. The claim `rewrite-pattern` is not
    // a claim of a new step.
    let out = semilla(repo, &["step", "cap each total", "--claim", r#"rewrite-pattern shop_core::cart::show from="$c.total($m)" to="$c.total($m).min(9)""#]);
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(text.contains("the claim `rewrite-pattern` is gone: state the pattern as a fact of the step, `--same 'rewrite FROM => TO'`"), "{text}");
    let out = run(repo, &["step", "cap each total", "--same", "rewrite $c.total($m) => $c.total($m).min(9)"], 0);
    assert!(out.contains("  ✓ a1 same: rewrite `$c.total($m)` => `$c.total($m).min(9)` (the form holds: 2 site(s) have exactly this edit. Semilla does not check that the behavior is the same)\n      places: shop_core::cart::show/tail/lhs, shop_core::cart::show/tail/rhs\n    ◆ f1 stated: changed expression `cart.total(10)` → `cart.total(10).min(9)` at "), "{out}");
    assert!(out.contains(" stated: ") && out.contains("  not stated: 0 item(s)\n"), "{out}");
    // The claim does not say that each place of the old form changed: it holds for one site.
    run(repo, &["undo"], 0);
    let missed = show.replace("cart.total(10) + cart.total(20)", "cart.total(10).min(9) + cart.total(20)");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{missed}{wide}"));
    let out = run(repo, &["step", "cap each total", "--same", "rewrite $c.total($m) => $c.total($m).min(9)"], 0);
    assert!(out.contains("(the form holds: 1 site(s) have exactly this edit. Semilla does not check that the behavior is the same)"), "{out}");
    run(repo, &["undo"], 0);
    // A pattern with no site is false: the step is rejected, and the message shows the nearest edit.
    let out = semilla(repo, &["step", "cap each total", "--same", "rewrite $c.total($m) => $c.total($m).min(8)"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("  ✗ fact a1 of the agent is false: `--same 'rewrite $c.total($m) => $c.total($m).min(8)'`") && text.contains("cart.total(10).min(9)"), "{text}");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{capped}{wide}"));
    run(repo, &["step", "cap each total"], 0);

    // `no-panic` on a division: refuted, with the panic site.
    let share = "\npub fn share(total: u32, n: u32) -> u32 {\n    total / n\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{capped}{wide}{share}"));
    let out = semilla(repo, &["step", "add share", "--claim", "no-panic shop_core::cart::share"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.code() == Some(1) && text.contains("is refuted: implicit panic sites: division by zero in `shop_core::cart::share`"), "{text}");
    // `no-unsafe`, `no-io`, and `tested-by` on a new function with its test.
    let test = "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn wide() {\n        assert_eq!(super::sum_wide(&[1, 2]), 3);\n    }\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{capped}{wide}{share}{test}"));
    let out = run(repo, &["step", "add share and a test", "--claim", "no-unsafe shop_core::cart::sum_wide", "--claim", "no-io shop_core::cart::sum_wide", "--claim", "tested-by shop_core::cart::sum_wide test=wide"], 0);
    assert_eq!(out.matches("verified (").count(), 3, "{out}");
    // The claim `rename` is gone: Semilla derives the rename.
    let renamed = capped.replace("pub fn show(", "pub fn display(");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{renamed}{wide}{share}{test}"));
    run(repo, &["step", "rename show to display", "--claim", "rename shop_core::cart::show to=display"], 2);
    let out = run(repo, &["step", "rename show to display"], 0);
    assert!(out.contains("renamed `shop_core::cart::show` → `shop_core::cart::display` [rename · code equal]"), "{out}");
    // The claim `move-item` is gone: Semilla derives the move. A body of the app uses the name,
    // so the compare needs the name map (N1).
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n\npub fn over(total: u32) -> bool {\n    total > 9\n}\n");
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("println!(\"{}\", cart.total(10));", "println!(\"{} {}\", cart.total(10), shop_core::over(3));"));
    run(repo, &["step", "add over"], 0);
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n");
    write(repo, "crates/shop-app/src/main.rs", &format!("{}\npub fn over(total: u32) -> bool {{\n    total > 9\n}}\n", MAIN_V0.replace("println!(\"{}\", cart.total(10));", "println!(\"{} {}\", cart.total(10), over(3));")));
    run(repo, &["step", "move over to the app", "--claim", "move-item shop_core::over to=shop_app (bin \"shop-app\")::over"], 2);
    let out = run(repo, &["step", "move over to the app"], 0);
    assert!(out.contains("moved `shop_core::over` → `shop_app (bin \"shop-app\")::over`") && out.contains("[move · code equal]"), "{out}");
}

/// Functions and tests for `tested-by` (batch 5f, A2).
const TESTED_CODE: &str = r#"
pub fn one() -> u32 { 1 }
pub fn two() -> u32 { 2 }
pub fn three() -> u32 { 3 }
pub fn four() -> u32 { 4 }
pub fn five() -> u32 { 5 }
pub fn six() -> u32 { 6 }
pub fn seven() -> u32 { 7 }

#[cfg(test)]
mod reach {
    use super::*;
    const NEVER: bool = false;

    #[test]
    fn dead_literal() {
        if false {
            one();
        }
    }

    #[test]
    fn dead_const() {
        if NEVER {
            two();
        }
    }

    #[test]
    fn dead_compare() {
        let off = 1 > 2;
        if off {
            three();
        }
    }

    #[test]
    fn dead_closure() {
        let call = || four();
        if false {
            call();
        }
    }

    #[test]
    fn after_a_return() {
        if true {
            return;
        }
        five();
    }

    #[test]
    fn live_closure() {
        let call = || six();
        assert_eq!(call(), 6);
    }

    #[test]
    fn live_helper() {
        assert_eq!([1u32].iter().map(|x| x + helper()).sum::<u32>(), 8);
    }

    fn helper() -> u32 {
        seven()
    }
}
"#;

const SURE_CODE: &str = r#"
pub fn one() -> u32 { 1 }

#[cfg(test)]
mod sure {
    use super::*;

    #[test]
    fn guard() { if std::hint::black_box(false) { assert_eq!(one(), 9); } }

    #[test]
    fn literal() {
        if false {
            assert_eq!(one(), 9);
        }
    }

    #[test]
    fn early_return() {
        if std::env::var("SHOP_SKIP").is_ok() {
            return;
        }
        assert_eq!(one(), 1);
    }

    #[test]
    fn in_a_loop() {
        for n in 0..3u32 {
            assert_eq!(one() + n, 1 + n);
        }
    }

    #[test]
    fn question_mark() -> Result<(), std::num::ParseIntError> {
        let n: u32 = "1".parse()?;
        assert_eq!(one(), n);
        Ok(())
    }

    #[test]
    fn two_lines() {
        assert_eq!(one(), 1);
        assert_eq!(one() + 1, 2);
    }

    #[test]
    fn through_a_helper() {
        assert_eq!(helper(), 1);
    }

    fn helper() -> u32 {
        one()
    }

    #[test]
    fn closure_with_no_call() {
        let call = || one();
        let _ = &call;
        assert_eq!(1, 1);
    }

    #[test]
    fn after_an_unwrap() {
        let n: u32 = "1".parse().unwrap();
        assert_eq!(one(), n);
    }

    #[test]
    fn each_arm() {
        let n = match std::hint::black_box(1u32) {
            1 => one(),
            _ => one() + 1,
        };
        assert_eq!(n, 1);
    }

    #[test]
    #[should_panic]
    fn then_a_panic() {
        let n = one();
        panic!("the value is {n}");
    }

    #[test]
    fn helper_with_a_condition() {
        assert_eq!(maybe(std::hint::black_box(true)), 1);
    }

    fn maybe(run: bool) -> u32 {
        if run { one() } else { 0 }
    }
}
"#;

#[test]
fn tested_by_is_verified_only_if_each_run_of_the_test_calls_the_function() {
    // Batch 5w, part C (trial 7 on humantime): a test with the call in
    // `if std::hint::black_box(false) { … }` got `verified`. The call is reachable for the
    // compiler, but no run of the test has it.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{SURE_CODE}"));
    let tests = ["guard", "literal", "early_return", "in_a_loop", "question_mark", "two_lines", "through_a_helper", "closure_with_no_call", "after_an_unwrap", "each_arm", "then_a_panic", "helper_with_a_condition"];
    let step = |tests: &[&str], expect: i32| {
        let mut args = vec!["step".to_string(), "add the tests".to_string(), "--reason".to_string(), "shop_core::cart::sure=one test must end with a panic".to_string()];
        for test in tests {
            args.extend(["--claim".to_string(), format!("tested-by shop_core::cart::one test={test}")]);
        }
        run(repo, &args.iter().map(String::as_str).collect::<Vec<_>>(), expect)
    };
    let text = step(&tests, 1);
    let result = |text: &str, index: usize| {
        if let Some(line) = text.lines().find(|line| line.trim_start().starts_with(&format!("✗ claim c{index} ("))) {
            return line.to_string();
        }
        let start = format!("c{index} ");
        let lines: Vec<&str> = text.lines().skip_while(|line| !line.trim_start().trim_start_matches(['✓', '✗', '…', '◐', ' ']).starts_with(&start) || line.contains("claim c")).take(2).collect();
        lines.join(" ")
    };
    let partial = "but not on each path from its start to its normal return (the call is under a condition, in a loop, after an early return, or in a closure that a different function calls). Semilla did not prove that the test runs the call.";
    for (index, test) in tests.iter().enumerate() {
        let line = result(&text, index + 1);
        match *test {
            "literal" => assert!(line.contains("refuted") && line.contains("only in code that cannot run"), "{test}: {text}"),
            "closure_with_no_call" => assert!(line.contains("refuted"), "{test}: {text}"),
            "guard" | "early_return" | "in_a_loop" | "question_mark" | "helper_with_a_condition" => assert!(line.contains("partial") && !line.contains("verified (mir)") && line.contains(partial), "{test}: {text}"),
            _ => assert!(line.contains("verified (mir)") && line.contains(&format!("the test `shop_core::cart::sure::{test}` calls `shop_core::cart::one` on each path from its start to its normal return.")), "{test}: {text}"),
        }
    }
    // A `partial` claim does not reject the step, and the summary counts it as partial.
    let text = step(&["guard", "two_lines"], 0);
    assert!(text.contains("✓ c1 verified (partial)") && text.contains("✓ c2 verified (mir)"), "{text}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-a", "-m", "add the tests"]);
    let text = run(repo, &["tag"], 0);
    assert!(text.contains("2 claims: 1 verified, 1 partial"), "{text}");
}

/// Batch 5y, A4: tests that can end the process, or end with a panic, before the call.
const ENDS_CODE: &str = r#"
pub fn twice(n: u32) -> u32 { n * 2 }
pub fn unit() -> u32 { 1 }
pub fn done() { std::process::exit(0) }

#[cfg(test)]
mod ends {
    use super::*;

    fn stop() { std::process::exit(0) }
    fn outer() { stop() }

    #[test]
    fn exit_in_a_helper() { stop(); assert_eq!(twice(2), 5); }

    #[test]
    fn exit_through_two_helpers() { outer(); assert_eq!(twice(2), 5); }

    #[test]
    fn exit_under_a_condition() { if std::env::args().count() > 0 { std::process::exit(0); } assert_eq!(twice(2), 5); }

    #[test]
    fn exit_in_a_closure() { Some(1).into_iter().for_each(|_| stop()); assert_eq!(twice(2), 5); }

    #[test]
    fn abort_under_a_condition() { if std::env::args().count() > 0 { std::process::abort(); } assert_eq!(twice(2), 5); }

    #[test]
    #[should_panic]
    fn panic_before_the_call() { let n: u32 = "x".parse().unwrap(); assert_eq!(twice(n), 4); }

    #[test]
    fn normal() { assert_eq!(twice(2), 4); }

    #[test]
    fn after_an_unwrap() { let n: u32 = "2".parse().unwrap(); assert_eq!(twice(n), 4); }

    #[test]
    fn after_an_assert() { let n = std::hint::black_box(2); assert!(n > 1); assert_eq!(twice(n), 4); }

    #[test]
    fn exit_after_the_call() { assert_eq!(twice(2), 4); stop(); }

    #[test]
    #[should_panic]
    fn panic_after_the_call() { let n = twice(2); assert_eq!(n, 5); }
}
"#;

#[test]
fn tested_by_counts_an_end_of_the_process_as_an_end_of_the_test() {
    // Batch 5y, A4 (trial 8): a test with `std::process::exit(0)` before the call got
    // `verified (mir)`. Under `cargo test`, no one calls the function, and the run has success.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{ENDS_CODE}"));
    // A test of a different crate: the function that ends the process is in the library.
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn exit_in_the_library() {{\n        shop_core::cart::done();\n        assert_eq!(shop_core::cart::twice(2), 5);\n    }}\n\n    #[test]\n    fn normal_in_the_app() {{\n        let n = shop_core::cart::unit();\n        assert_eq!(shop_core::cart::twice(n), 2);\n    }}\n}}\n"));
    let end = |call: &str, function: &str| format!("on each path from its start to its normal return, but the test can end the process before the call: `{call}` in `{function}`. Semilla did not prove that the test runs the call.");
    let tests = [
        ("exit_in_a_helper", Some(end("std::process::exit", "shop_core::cart::ends::stop"))),
        ("exit_through_two_helpers", Some(end("std::process::exit", "shop_core::cart::ends::stop"))),
        ("exit_under_a_condition", Some(end("std::process::exit", "shop_core::cart::ends::exit_under_a_condition"))),
        ("exit_in_a_closure", Some(end("std::process::exit", "shop_core::cart::ends::stop"))),
        ("abort_under_a_condition", Some(end("std::process::abort", "shop_core::cart::ends::abort_under_a_condition"))),
        ("panic_before_the_call", Some("but the test has `#[should_panic]`, and a panic can end the test before the call (`shop_core::cart::ends::panic_before_the_call` has a call or a check before its call of `shop_core::cart::twice`). Semilla did not prove that the test runs the call.".to_string())),
        ("exit_in_the_library", Some(end("std::process::exit", "shop_core::cart::done"))),
        ("normal", None),
        ("after_an_unwrap", None),
        ("after_an_assert", None),
        ("exit_after_the_call", None),
        ("panic_after_the_call", None),
        ("normal_in_the_app", None),
    ];
    let mut args = vec!["step".to_string(), "add the tests".to_string(), "--reason".to_string(), "shop_core::cart::ends=two tests must end with a panic".to_string()];
    for (test, _) in &tests {
        args.extend(["--claim".to_string(), format!("tested-by shop_core::cart::twice test={test}")]);
    }
    let (_, text) = run_env(repo, &args.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
    for (index, (test, partial)) in tests.iter().enumerate() {
        let start = format!("c{} ", index + 1);
        let lines: Vec<&str> = text.lines().skip_while(|line| !line.trim_start().trim_start_matches(['✓', '✗', '…', '◐', ' ']).starts_with(&start) || line.contains("claim c")).take(2).collect();
        let line = lines.join(" ");
        match partial {
            Some(cause) => assert!(line.contains("verified (partial)") && line.contains(cause.as_str()), "{test}: {line}\n{text}"),
            None => assert!(line.contains("verified (mir)") && line.contains("calls `shop_core::cart::twice` on each path from its start to its normal return. Semilla does not run the test"), "{test}: {line}\n{text}"),
        }
    }
}

#[test]
fn tested_by_needs_a_call_in_code_that_can_run() {
    // Trial 4, cheat C2: the test has the call in `if false { … }`.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{TESTED_CODE}"));
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn the_app_uses_one() {{\n        assert_eq!(shop_core::cart::one(), 1);\n    }}\n}}\n"));
    let claims = [("one", "dead_literal"), ("two", "dead_const"), ("three", "dead_compare"), ("four", "dead_closure"), ("five", "after_a_return"), ("six", "live_closure"), ("seven", "live_helper"), ("one", "the_app_uses_one"), ("two", "live_helper")];
    let mut args = vec!["step".to_string(), "add the tests".to_string()];
    for (function, test) in claims {
        args.extend(["--claim".to_string(), format!("tested-by shop_core::cart::{function} test={test}")]);
    }
    let out = semilla(repo, &args.iter().map(String::as_str).collect::<Vec<_>>());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{text}");
    let result = |index: usize| {
        // A rejected step prints the full text of a failed claim one time, in the first lines.
        if let Some(line) = text.lines().find(|line| line.trim_start().starts_with(&format!("✗ claim c{index} ("))) {
            return line.to_string();
        }
        let start = format!("c{index} ");
        let lines: Vec<&str> = text.lines().skip_while(|line| !line.trim_start().trim_start_matches(['✓', '✗', '…', ' ']).starts_with(&start) || line.contains("claim c")).take(2).collect();
        lines.join(" ")
    };
    for (index, function) in [(1, "one"), (2, "two"), (3, "three"), (4, "four"), (5, "five")] {
        assert!(result(index).contains("refuted") && result(index).contains(&format!("has a call to `shop_core::cart::{function}` only in code that cannot run")), "c{index}: {text}");
    }
    // A direct call of a closure is a call of the test (batch 5w, part C).
    assert!(result(6).contains("verified (mir)") && result(6).contains("calls `shop_core::cart::six` on each path from its start to its normal return. Semilla does not run the test and does not check what the test asserts."), "c6: {text}");
    // The closure runs in `Iterator::sum`: Semilla does not prove that the call runs.
    assert!(result(7).contains("partial") && result(7).contains("has a reachable call to `shop_core::cart::seven`, but not on each path"), "c7: {text}");
    // Trial 4, D10: a test name with no path resolves in the workspace. The test is in another crate.
    assert!(result(8).contains("verified (mir)") && result(8).contains("the test `shop_app (bin \"shop-app\")::tests::the_app_uses_one` calls `shop_core::cart::one` on each path"), "{text}");
    assert!(result(9).contains("refuted") && result(9).contains("does not call `shop_core::cart::two`"), "{text}");
}

#[test]
fn a_property_claim_examines_the_functions_that_a_body_uses_as_values() {
    // Trial 3, cheat A12: a panic in a function that the body calls through a `fn` pointer.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let boom = |body: &str| format!("\nfn boom(n: u32) -> u32 {{\n    if n > 5 {{\n        {body}\n    }}\n    n\n}}\n\npub fn checked(n: u32) -> u32 {{\n    let f: fn(u32) -> u32 = boom;\n    f(n)\n}}\n");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{}", boom("panic!(\"boom\")")));
    let out = semilla(repo, &["step", "add checked", "--claim", "no-panic shop_core::cart::checked"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.code() == Some(1) && text.contains("refuted") && text.contains("shop_core::cart::boom"), "{text}");
    // No panic in `boom`: partial, with the count of the calls through a pointer.
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{}", boom("return 0;")));
    let out = run(repo, &["step", "add checked", "--claim", "no-panic shop_core::cart::checked"], 0);
    assert!(out.contains("verified (partial)") && out.contains("not examined: a call through a function pointer"), "{out}");
    // A closure is a callee, and a call on a trait object is not resolved.
    let more = "\npub fn each(v: &[u32]) -> Vec<u32> {\n    v.iter().map(|x| x / 2).collect()\n}\n\npub fn show(d: &dyn std::fmt::Display) -> String {\n    d.to_string()\n}\n\npub fn split(v: &[u32]) -> Vec<u32> {\n    v.iter().map(|x| 100 / *x).collect()\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{}{more}", boom("return 0;")));
    let out = semilla(repo, &["step", "add each", "--claim", "no-panic shop_core::cart::split"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.code() == Some(1) && text.contains("division by zero"), "{text}");
    let out = run(repo, &["step", "add each", "--claim", "no-io shop_core::cart::show"], 0);
    assert!(out.contains("verified (partial)") && out.contains("not examined: "), "{out}");
}

/// The code of batch 5f, A1: bodies that run with no direct call from the claimed function.
const REACH_CODE: &str = r#"
pub struct Label(pub u64);
impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = std::fs::write("/tmp/semilla-test-leak.txt", "x");
        write!(f, "{}", self.0)
    }
}
pub fn label(n: u64) -> String {
    Label(n).to_string()
}
pub fn label_fmt(n: u64) -> String {
    format!("{}", Label(n))
}
#[derive(Debug)]
pub struct Honest(pub u64);
pub fn honest(n: u64) -> String {
    format!("{:?}", Honest(n))
}
pub struct Loud(pub u64);
impl std::fmt::Debug for Loud {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        println!("loud");
        Ok(())
    }
}
pub fn louds(n: u64) -> String {
    format!("{:?}", vec![Loud(n)])
}
pub struct Seven(pub u64);
impl std::fmt::Display for Seven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 == 7 {
            panic!("seven");
        }
        write!(f, "{}", self.0)
    }
}
pub fn seven(n: u64) -> String {
    Seven(n).to_string()
}
pub struct Audit(pub u64);
impl Drop for Audit {
    fn drop(&mut self) {
        let _ = std::fs::write("/tmp/semilla-test-audit.txt", "x");
    }
}
pub fn audit(n: u64) -> u64 {
    let audit = Audit(n);
    audit.0
}
pub async fn later(n: u64) -> u64 {
    let _ = std::fs::write("/tmp/semilla-test-later.txt", "x");
    n
}
unsafe extern "C" {
    fn getpid() -> i32;
}
pub fn pid() -> i32 {
    unsafe { getpid() }
}
pub fn args_in(a: std::fmt::Arguments<'_>) -> String {
    std::fmt::format(a)
}
"#;

#[test]
fn a_property_claim_examines_each_body_that_can_run() {
    // Trial 4, cheat C1 and attempt 13: the I/O is in a trait impl or in a `Drop` impl, and the
    // code of another crate calls it. The claimed function has no call to it.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{REACH_CODE}"));
    let claims = ["no-io label", "no-io label_fmt", "no-io louds", "no-panic seven", "no-io audit", "no-io later", "no-io honest", "no-unsafe honest", "no-io pid", "no-io args_in"];
    let mut args = vec!["step".to_string(), "add the reach cases".to_string(), "--reason".to_string(), "shop_core::cart::impl <Audit as Drop>=a test".to_string()];
    for claim in claims {
        let (op, name) = claim.split_once(' ').unwrap();
        args.extend(["--claim".to_string(), format!("{op} shop_core::cart::{name}")]);
    }
    let out = semilla(repo, &args.iter().map(String::as_str).collect::<Vec<_>>());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{text}");
    let result = |index: usize| {
        // A rejected step prints the full text of a failed claim one time, in the first lines.
        if let Some(line) = text.lines().find(|line| line.trim_start().starts_with(&format!("✗ claim c{index} ("))) {
            return line.to_string();
        }
        let start = format!("c{index} ");
        let lines: Vec<&str> = text.lines().skip_while(|line| !line.trim_start().trim_start_matches(['✓', '✗', '…', ' ']).starts_with(&start) || line.contains("claim c")).take(2).collect();
        lines.join(" ")
    };
    // C1: `to_string` and `format!` run the local `Display::fmt`.
    for index in [1, 2] {
        assert!(result(index).contains("refuted") && result(index).contains("`std::fs::write` in `shop_core::cart::<Label as std::fmt::Display>::fmt` (a trait impl of `shop_core::cart::Label`"), "{text}");
    }
    // A nested type: `Vec<Loud>` runs `Debug::fmt` of `Loud`.
    assert!(result(3).contains("refuted") && result(3).contains("`std::io::_print` in `shop_core::cart::<Loud as std::fmt::Debug>::fmt`"), "{text}");
    // A panic in a local `Display` impl refutes `no-panic`.
    assert!(result(4).contains("refuted") && result(4).contains("<Seven as std::fmt::Display>::fmt"), "{text}");
    // Attempt 13: the drop glue runs `Drop::drop`.
    assert!(result(5).contains("refuted") && result(5).contains("`std::fs::write` in `shop_core::cart::<Audit as Drop>::drop` (the code can drop a value of `shop_core::cart::Audit`)"), "{text}");
    // The body of an `async fn` is a coroutine.
    assert!(result(6).contains("refuted") && result(6).contains("std::fs::write"), "{text}");
    // An honest function with a derived `Debug` impl: verified in full, with the counts.
    assert!(result(7).contains("verified (mir)") && result(7).contains("examined: 2 local bodies (1 through trait impls, 0 drop bodies)"), "{text}");
    assert!(result(8).contains("verified (mir)"), "{text}");
    // A foreign function has no body: partial.
    assert!(result(9).contains("verified (partial)") && result(9).contains("not examined: a call of the foreign function `shop_core::cart::getpid`"), "{text}");
    // A value from the caller that can hold code: partial.
    assert!(result(10).contains("verified (partial)") && result(10).contains("a parameter of `shop_core::cart::args_in` holds a value of `core::fmt::Arguments`"), "{text}");

    // Rule 5: a global allocator in the workspace makes each property claim partial.
    let allocator = "\n#[global_allocator]\nstatic ALLOCATOR: std::alloc::System = std::alloc::System;\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{REACH_CODE}{allocator}"));
    let out = semilla(repo, &["step", "add the reach cases", "--claim", "no-io shop_core::cart::honest", "--reason", "shop_core::cart::impl <Audit as Drop>=a test", "--reason", "shop_core::cart::ALLOCATOR=a test"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("verified (partial)") && text.contains("the workspace has a `#[global_allocator]` (`shop_core::cart::ALLOCATOR`): each allocation runs its code"), "{text}");
}

#[test]
fn a_field_reorder_owns_the_derived_bodies_and_keeps_the_readers_neutral() {
    // Trial 3, A8: a reorder of the fields of a `Debug` type gave "indirect change, cause not
    // known" for each body that reads a field.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let account = |fields: &str| format!("#[derive(Debug, Clone)]\npub struct Account {{\n{fields}}}\n\npub fn total(v: &[Account]) -> u64 {{\n    let mut sum = 0;\n    for a in v {{\n        sum += a.cents + a.id as u64;\n    }}\n    sum\n}}\n\npub fn open(id: u32) -> Account {{\n    Account {{ id, owner: String::new(), cents: 0 }}\n}}\n");
    write(repo, "crates/shop-core/src/lib.rs", &format!("pub mod cart;\n{}", account("    pub id: u32,\n    pub owner: String,\n    pub cents: u64,\n")));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "account"]);
    write(repo, "crates/shop-core/src/lib.rs", &format!("pub mod cart;\n{}", account("    pub id: u32,\n    pub cents: u64,\n    pub owner: String,\n")));
    // R65.3-3: `Debug` prints the fields in order, so the order change needs a reason.
    let out = run(repo, &["step", "reorder the fields of Account"], 1);
    assert!(out.contains("an order change in a type with `#[repr]`, a derive, or an attribute macro (`Debug`, `Clone`)"), "{out}");
    let out = run(repo, &["step", "reorder the fields of Account", "--reason", "shop_core::Account=the debug text has the new order"], 0);
    assert!(out.contains("member order of `shop_core::Account`") && out.contains("<shop_core::Account as std::fmt::Debug>::fmt"), "{out}");
    assert!(!out.contains("indirect change") && !out.contains("shop_core::total") && !out.contains("shop_core::open"), "{out}");
}

#[test]
fn a_commit_between_steps_gets_a_note_and_an_exact_route() {
    // Trial 3, P2: `step` after a commit gave no warning, and `tag` failed with no route.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = git_out(repo, &["rev-parse", "HEAD"]);
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    run(repo, &["step", "rename the sum"], 0);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "one"]);
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    let out = run(repo, &["step", "an empty cart has the total 0"], 0);
    assert!(out.contains("note: HEAD moved since the change set began") && out.contains(&format!("git reset --soft {base}")), "{out}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "two"]);
    let out = semilla(repo, &["tag"]);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success() && text.contains(&format!("git reset --soft {base}")), "{text}");
    git(repo, &["reset", "-q", "--soft", &base]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "the work"]);
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted"), "{out}");
    // A rejected step after `tag` keeps the tagged change set, and `tag` with no step says so.
    write(repo, "Cargo.lock", "x = [\n");
    let out = semilla(repo, &["step", "a broken lock file"]);
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    // Batch 5n, A6: a failed read of the dependency graph is not "build code changed". The
    // message says what the user can do, and the step starts nothing.
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("Semilla has no result: cargo cannot read the dependency graph of the tree after step 0") && text.contains("`cargo fetch`") && !text.contains("build code changed"), "{text}");
    let session = std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap();
    assert!(session.contains("an empty cart has the total 0"), "{session}");
    std::fs::remove_file(repo.join("Cargo.lock")).unwrap();
    // Design 62: `tag` with no change set tags the commit after the commit (here the root
    // commit, compared with the empty tree).
    let dir = setup();
    let out = semilla_env(&dir.path().join("repo"), &["tag"], &[]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.starts_with("result: tagged after the commit · 1 step · ") && text.contains("from the empty tree (a root commit)"), "{text}{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_new_cargo_config_changes_the_bodies_of_the_after_tree_only() {
    // C5 and trial 3, A6e: the build of each tree ran inside the repository, so cargo read the
    // `.cargo/config` of the working tree for both trees, and the change was not visible.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let add = "\npub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{add}"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "add"]);
    write(repo, ".cargo/config", "[profile.dev]\noverflow-checks = false\n");
    let out = semilla(repo, &["step", "turn off the overflow checks"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success() && text.contains(".cargo/config: profile.dev.overflow-checks added (false)") && text.contains("needs a reason"), "{text}");
    let out = run(repo, &["step", "turn off the overflow checks", "--reason", ".cargo/config:profile.dev.overflow-checks=the benchmark needs it"], 0);
    assert!(out.contains("[manifest · code different]") && out.contains("affected bodies: shop_core::cart::add"), "{out}");
    assert!(!out.contains("exists only"), "{out}");
}

#[test]
fn the_messages_name_the_cause_and_a_command_that_can_succeed() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // M6: the help lists the new commands first, and hides the old ones.
    let help = run(repo, &["--help"], 0);
    let commands: Vec<&str> = help.lines().skip_while(|l| !l.starts_with("Commands:")).skip(1).take(7).map(|l| l.split_whitespace().next().unwrap_or("")).collect();
    assert_eq!(commands, ["step", "fact", "undo", "tag", "status", "review", "ops"], "{help}");
    assert!(!help.contains("\n  claim ") && !help.contains("\n  anchor "), "{help}");
    // M4 and M3: a claim that is gone names the cause and the claims that stay.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let out = semilla(repo, &["step", "raise the max", "--claim", "rename shop_app (bin \"shop-app\")::MAX to=LIMIT"]);
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(text.contains("the claim `rename` is gone: Semilla derives each rename and each move as a fact") && text.contains("or state the fact with `--same 'rename OLD NEW'`") && text.contains("the claims that stay: no-panic, no-unsafe, no-io, calls-only, tested-by"), "{text}");
    run(repo, &["step", "raise the max"], 0);
    // M5: phase 1 prints no score. M1: `status --wait` waits for phase 2 of a stale session.
    let phase1 = run(repo, &["verify"], 0);
    assert!(phase1.contains("result:"), "{phase1}");
    std::fs::create_dir_all(repo_cache(repo).join("reports")).unwrap();
    let report = repo_cache(repo).join("reports/session.json");
    if let Ok(text) = std::fs::read_to_string(&report) {
        let mut stored: serde_json::Value = serde_json::from_str(&text).unwrap();
        stored["hash"] = serde_json::json!("stale");
        std::fs::write(&report, stored.to_string()).unwrap();
    }
    let status = run(repo, &["status", "--wait"], 0);
    assert!(status.starts_with("session: ready") && status.contains("phase 2 (computed on this machine)"), "{status}");
    let no_bodies = Command::new(env!("CARGO_BIN_EXE_semilla")).current_dir(repo).env("SEMILLA_CACHE_DIR", cache_dir(repo)).env("SEMILLA_NO_BODIES", "1").args(["verify"]).output().unwrap();
    let text = String::from_utf8_lossy(&no_bodies.stdout);
    assert!(text.contains("result: ") && !text.contains("score"), "{text}");
}

/// The fact line (`◆ …`) of a step output that contains `text`, or an empty string.
fn fact_line<'a>(out: &'a str, text: &str) -> &'a str {
    out.lines().find(|line| line.trim_start().starts_with('◆') && line.contains(text)).unwrap_or("")
}

/// Commits a module `members.rs` in `shop-core` with `code` as the base of a test.
fn members_base(repo: &Path, code: &str) {
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod members;\n");
    write(repo, "crates/shop-core/src/members.rs", code);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "members"]);
}

#[test]
fn a_member_edit_that_changes_the_indexes_is_not_a_neutral_rewrite() {
    // Batch 5e: the MIR named a field and a variant by its index. A step that changed the map
    // from names to indexes made a real change look like a "neutral rewrite (proved)".
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub struct Pair { pub a: u64, pub b: u64 }\npub fn first(p: &Pair) -> u64 { p.a }\npub fn make(x: u64, y: u64) -> u64 { first(&Pair { a: x, b: y }) }\n\npub struct Trio { pub a: u64, pub b: u64 }\npub fn second(t: &Trio) -> u64 { t.b }\npub fn make_trio(x: u64, y: u64) -> u64 { second(&Trio { a: x, b: y }) }\n\n#[derive(Clone, Copy)]\npub enum Kind { Small, Large }\npub fn weight(k: Kind) -> u32 { match k { Kind::Small => 1, Kind::Large => 9 } }\npub fn small() -> u32 { weight(Kind::Small) }\n";
    members_base(repo, base);

    // Case 1: the member order `b, a` and `first` reads `p.b`. `make(x, y)` gave `x` and gives `y`.
    let code = base.replace("pub struct Pair { pub a: u64, pub b: u64 }", "pub struct Pair { pub b: u64, pub a: u64 }").replace("{ p.a }", "{ p.b }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "reorder Pair and read b"], 0);
    let edit = fact_line(&out, "`p.a` → `p.b`");
    assert!(edit.ends_with("code different]"), "{out}");
    // Design 44: no element of a changed body names the order (the reader has its own edit),
    // so the order has no compare result of its own: the MIR does not show each effect of an
    // order. Before 44, the type fact owned each changed body that named the type.
    assert!(fact_line(&out, "member order of `shop_core::members::Pair`").ends_with("no compare]"), "{out}");
    assert!(!out.contains("code equal"), "{out}");

    // Case 2: a new field `c` between `a` and `b`, and the reader changes from `t.b` to `t.c`.
    // The reader reads index 1 before and after.
    let code = code.replace("pub struct Trio { pub a: u64, pub b: u64 }", "pub struct Trio { pub a: u64, pub c: u64, pub b: u64 }").replace("{ t.b }", "{ t.c }").replace("Trio { a: x, b: y }", "Trio { a: x, c: x + y, b: y }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "add the field c and read it"], 0);
    assert!(fact_line(&out, "`t.b` → `t.c`").ends_with("code different]"), "{out}");
    assert!(!out.contains("code equal"), "{out}");

    // Case 3: the variant order `Large, Small`, and the two arm patterns swap. `small()` gave 1
    // and gives 9.
    let code = code.replace("pub enum Kind { Small, Large }", "pub enum Kind { Large, Small }").replace("match k { Kind::Small => 1, Kind::Large => 9 }", "match k { Kind::Large => 1, Kind::Small => 9 }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    // R65.3-3: `Kind` has derives, so the order change needs a reason.
    let out = run(repo, &["step", "reorder Kind and swap the patterns", "--reason", "shop_core::members::Kind=the patterns swap with the order"], 0);
    let order = fact_line(&out, "member order of `shop_core::members::Kind`");
    // Design 44 (fix round 1): the pattern edits own the elements of `weight` by their place,
    // and a link adds the order as a second cause (the signature of `weight` names `Kind`).
    assert!(order.contains("the discriminant values") && order.ends_with("code different]"), "{out}");
    assert!(!out.contains("code equal"), "{out}");
    // The caller with no edit is equal by name: it is not an affected body (design 61.10: it
    // can be a reached body of the walk).
    assert!(out.contains("affected bodies: shop_core::members::weight") && !out.lines().any(|line| line.contains("affected bodies:") && line.contains("shop_core::members::small")), "the caller with no edit is equal by name: {out}");
}

#[test]
fn a_member_order_change_alone_makes_no_noise() {
    // Batch 5e: with names in the MIR, the readers and the constructors stay equal after a
    // reorder or a new field in the middle. The derived bodies go under the member order fact.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "#[derive(Debug, Clone, PartialEq)]\npub struct Pair { pub a: u64, pub b: u64 }\npub fn first(p: &Pair) -> u64 { p.a }\npub fn second(p: &Pair) -> u64 { p.b }\npub fn make(x: u64, y: u64) -> Pair { Pair { a: x, b: y } }\n\n#[derive(Clone, Copy)]\npub enum Kind { Small, Large }\npub fn code(k: Kind) -> u8 { k as u8 }\npub fn weight(k: Kind) -> u32 { match k { Kind::Small => 1, Kind::Large => 9 } }\n";
    members_base(repo, base);

    let code = base.replace("pub struct Pair { pub a: u64, pub b: u64 }", "pub struct Pair { pub b: u64, pub a: u64 }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    // R65.3-3: the derives read the fields in order, so the order change needs a reason.
    let out = run(repo, &["step", "reorder Pair", "--reason", "shop_core::members::Pair=the debug text has the new order"], 0);
    let order = fact_line(&out, "member order of `shop_core::members::Pair`");
    assert!(order.ends_with("code different]"), "{out}");
    assert!(out.contains("<shop_core::members::Pair as std::fmt::Debug>::fmt"), "{out}");
    assert!(!out.contains("indirect change") && !out.contains("members::first") && !out.contains("members::second") && !out.contains("members::make"), "{out}");
    assert_eq!(out.matches('◆').count(), 1, "{out}");

    // A new field in the middle, with no edit of the readers.
    let code = code.replace("pub struct Pair { pub b: u64, pub a: u64 }", "pub struct Pair { pub b: u64, pub c: u64, pub a: u64 }").replace("Pair { a: x, b: y }", "Pair { a: x, c: 0, b: y }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "add the field c"], 0);
    assert!(!out.contains("indirect change") && !out.contains("members::first") && !out.contains("members::second"), "{out}");

    // Case 4: a variant order change of a fieldless enum that a cast reads. No body has a text
    // edit, but `code` gives another number. The fact is `code different` and in the spotlight.
    let code = code.replace("pub enum Kind { Small, Large }", "pub enum Kind { Large, Small }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "reorder Kind"], 1);
    let order = fact_line(&out, "member order of `shop_core::members::Kind`");
    assert!(order.ends_with("code different]"), "{out}");
    assert!(out.contains("a cast (`as`) reads (in `shop_core::members::code`)") && out.contains("affected bodies: shop_core::members::code"), "{out}");
    // The compare is at the level N0: a match by name reads the new discriminant values, so
    // its compiled code is different (design 35.4).
    assert!(out.contains("affected bodies: shop_core::members::code, shop_core::members::weight"), "{out}");
    let out = run(repo, &["step", "reorder Kind", "--reason", "shop_core::members::Kind=the codes are internal"], 0);
    assert!(out.starts_with("accepted"), "{out}");
}

#[test]
fn constants_and_generic_parameters_follow_the_names() {
    // Batch 5e: a constant in memory has its bytes in the order of the layout, and a caller gives
    // the generic arguments by position. Both hid a real change.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub struct Pair { pub a: u64, pub b: u64 }\npub const P: Pair = Pair { a: 1, b: 2 };\npub fn first() -> u64 { P.a }\n\npub fn size<A, B>() -> usize { std::mem::size_of::<A>() }\npub fn use_size() -> usize { size::<u8, u64>() }\n";
    members_base(repo, base);

    // The member order `b, a` and the value `Pair { b: 1, a: 2 }`: the same bytes, and `first`
    // gave 1 and gives 2.
    let code = base.replace("pub struct Pair { pub a: u64, pub b: u64 }", "pub struct Pair { pub b: u64, pub a: u64 }").replace("Pair { a: 1, b: 2 }", "Pair { b: 1, a: 2 }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "reorder Pair and change P"], 0);
    assert!(!out.contains("code equal") && out.contains("shop_core::members::first"), "{out}");

    // The generic parameters `<B, A>`: `use_size()` gave 1 and gives 8.
    let code = code.replace("pub fn size<A, B>()", "pub fn size<B, A>()");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "swap the generic parameters"], 0);
    assert!(fact_line(&out, "generics of `shop_core::members::size`").ends_with("code different]"), "{out}");
}

#[test]
fn a_field_rename_of_a_type_with_a_constant_in_memory_is_proved() {
    // Brief 5g, item 4: the layout note of a constant in memory had bare field names, so the
    // name map of a rename did not apply, and Semilla did not prove the rename.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub struct Pair { pub a: u64, pub b: u64 }\npub const P: Pair = Pair { a: 1, b: 2 };\npub fn origin() -> &'static Pair { &P }\npub fn first() -> u64 { origin().a }\n";
    members_base(repo, base);
    let renamed = base.replace("pub a: u64", "pub left: u64").replace("Pair { a: 1", "Pair { left: 1").replace("origin().a", "origin().left");
    write(repo, "crates/shop-core/src/members.rs", &renamed);
    let out = run(repo, &["step", "rename a to left"], 0);
    assert!(out.contains("renamed `shop_core::members::Pair.a` → `shop_core::members::Pair.left` [rename · code equal]"), "{out}");
    assert!(!out.contains("affected bodies"), "{out}");

    // The other direction: a rename with a new member order and the same bytes of the constant
    // (`first` gave 1 and gives 2). Batch 7b, part 1: the rename is a fact, and the member order
    // and the value are facts with `code different` and the bodies with the constant.
    let swapped = renamed.replace("pub struct Pair { pub left: u64, pub b: u64 }", "pub struct Pair { pub b: u64, pub start: u64 }").replace("Pair { left: 1, b: 2 }", "Pair { b: 1, start: 2 }").replace("origin().left", "origin().start");
    write(repo, "crates/shop-core/src/members.rs", &swapped);
    let out = run(repo, &["step", "rename left to start and reorder"], 0);
    assert!(fact_line(&out, "renamed `shop_core::members::Pair.left` → `shop_core::members::Pair.start`").ends_with("[rename · code equal]"), "{out}");
    assert!(fact_line(&out, "member order of `shop_core::members::Pair`: `left, b` → `b, start`").ends_with("[declaration · code different]"), "{out}");
    assert!(fact_line(&out, "value of `shop_core::members::P` changed").ends_with("[semantics · code different]"), "{out}");
    assert!(out.contains("affected bodies: shop_core::members::P, shop_core::members::origin") && !out.contains("changed code"), "{out}");
}

#[test]
fn claim_ids_count_the_stored_claims() {
    // Brief 5f, C6 (trial 4, D14): a rejected step used a claim id, so the first stored claim
    // of the trial was `c6`. The ids count the stored claims of the change set.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let false_claim = "tested-by shop_core::cart::Cart::total test=shop_core::cart::tests::missing";
    let true_claim = "no-unsafe shop_core::cart::Cart::total";

    // A rejected step at the start of the change set stores nothing.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let rejected = String::from_utf8_lossy(&semilla(repo, &["step", "raise the max", "--claim", false_claim]).stdout).into_owned();
    assert!(rejected.starts_with("rejected: step 0") && rejected.contains("✗ c1 malformed"), "{rejected}");
    let accepted = run(repo, &["step", "raise the max", "--claim", true_claim], 0);
    assert!(accepted.starts_with("accepted: step 0") && accepted.contains("✓ c1 verified"), "the first stored claim is c1: {accepted}");

    // A rejected step after a stored step: the next stored claim is `c2`.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "30"));
    let rejected = String::from_utf8_lossy(&semilla(repo, &["step", "raise the max again", "--claim", false_claim]).stdout).into_owned();
    assert!(rejected.starts_with("rejected: step 1") && rejected.contains("✗ c2 malformed"), "{rejected}");
    let rejected = String::from_utf8_lossy(&semilla(repo, &["step", "raise the max again", "--claim", false_claim, "--claim", true_claim]).stdout).into_owned();
    assert!(rejected.starts_with("rejected: step 1"), "{rejected}");
    let accepted = run(repo, &["step", "raise the max again", "--claim", true_claim], 0);
    assert!(accepted.starts_with("accepted: step 1") && accepted.contains("✓ c2 verified"), "the second stored claim is c2: {accepted}");

    // After `undo`, the id of the removed claim is free again.
    run(repo, &["undo"], 0);
    let accepted = run(repo, &["step", "raise the max again", "--claim", true_claim], 0);
    assert!(accepted.contains("✓ c2 verified"), "{accepted}");
}

/// Writes an executable script in the repository folder. Each run adds a line to `marker`.
#[cfg(unix)]
fn marker_script(repo: &Path, name: &str, marker: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = repo.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n{body}\n", marker.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A change set with one step (a changed condition), a commit, and `tag`.
fn tagged_condition_change(repo: &Path) {
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("*x < max", "*x <= max"));
    run(repo, &["step", "include the max"], 0);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "include the max"]);
    let tag = run(repo, &["tag"], 0);
    assert!(tag.contains("result: accepted"), "{tag}");
}

#[cfg(unix)]
#[test]
fn no_reviewer_result_depends_on_a_file_of_the_repository_that_is_not_a_git_object() {
    // Brief 5h, A1 to A3 (invariant I5). The agent controls each file of the repository folder:
    // `.semilla`, the git configuration, the attributes, the hooks, the replace refs, and the
    // shallow file. No reviewer command reads a result from these files or runs a program that
    // they name.
    let dir = setup();
    let repo = &dir.path().join("repo");
    tagged_condition_change(repo);
    let commit = git_out(repo, &["rev-parse", "HEAD"]);
    let tree = git_out(repo, &["rev-parse", "HEAD^{tree}"]);

    // A2: `status` after `tag` on the same machine has the result at once, with no new build.
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains(": ready") && status.contains("phase 2 (computed on this machine)"), "{status}");
    assert!(!status.contains("queued") && !status.contains("not checked on this machine"), "{status}");
    let report = |name: &str, extra: &[&str]| {
        let file = dir.path().join(name);
        let mut args = vec!["review", "HEAD", "--full", "--md", file.to_str().unwrap()];
        args.extend(extra);
        run(repo, &args, 0);
        std::fs::read_to_string(&file).unwrap()
    };
    let honest = report("honest.md", &[]);
    assert!(honest.contains("<="), "{honest}");

    // The attack. Each script records its run in the marker file.
    let marker = dir.path().join("marker");
    let filter = marker_script(repo, ".semilla/evil/filter", &marker, "sed 's/<= max/< max/'");
    let program = marker_script(repo, ".semilla/evil/program", &marker, "exit 0");
    marker_script(repo, ".semilla/evil/hooks/post-checkout", &marker, "exit 0");
    marker_script(repo, ".semilla/evil/hooks/reference-transaction", &marker, "exit 0");
    marker_script(repo, ".semilla/tools/z3", &marker, "echo 'Z3 version 4.15.4 - 64 bit'; echo unsat");
    for (key, value) in [
        ("filter.evil.smudge", filter.to_str().unwrap()),
        ("filter.evil.clean", filter.to_str().unwrap()),
        ("filter.evil.required", "true"),
        ("diff.evil.textconv", filter.to_str().unwrap()),
        ("diff.evil.command", program.to_str().unwrap()),
        ("diff.external", program.to_str().unwrap()),
        ("core.fsmonitor", program.to_str().unwrap()),
        ("core.hooksPath", repo.join(".semilla/evil/hooks").to_str().unwrap()),
        ("core.sshCommand", program.to_str().unwrap()),
        ("core.pager", program.to_str().unwrap()),
        ("core.autocrlf", "true"),
    ] {
        git(repo, &["config", key, value]);
    }
    write(repo, ".git/info/attributes", "* filter=evil diff=evil text eol=crlf\n");
    // A replace ref gives the old file for the new file. The shallow file hides the parent.
    let old_blob = git_out(repo, &["rev-parse", "HEAD^:crates/shop-core/src/cart.rs"]);
    let new_blob = git_out(repo, &["rev-parse", "HEAD:crates/shop-core/src/cart.rs"]);
    git(repo, &["replace", &new_blob, &old_blob]);
    write(repo, ".git/shallow", &format!("{commit}\n"));
    // Forged results and records in `.semilla` (the trial 5 cheats C2 and C3).
    let forged = serde_json::json!({ "key": commit, "tree": tree, "hash": "x", "tools": "x", "text": "FORGED: [semantics · code equal]\n", "report": {} }).to_string();
    write(repo, &format!(".semilla/reports/{commit}.json"), &forged);
    write(repo, ".semilla/reports/session.json", &forged);
    write(repo, &format!(".semilla/cache/mir/{tree}/n0/.complete"), "");
    write(repo, &format!(".semilla/cache/mir/{tree}/n2/.complete"), "");
    write(repo, ".semilla/review.json", "{\"FORGED\": true}");
    // A cargo configuration of the working tree that is not in a tree.
    write(repo, ".cargo/config.toml", &format!("[build]\nrustc-wrapper = \"{}\"\n", program.display()));

    // The git commands of this test ran a hook. From here, only Semilla runs.
    let _ = std::fs::remove_file(&marker);

    // The stored result, and a result that Semilla computes again from nothing.
    let stored = run(repo, &["status", "HEAD"], 0);
    assert_eq!(stored, status, "the stored result does not change");
    assert_eq!(report("stored.md", &[]), honest, "the report from the cache does not change");
    assert_eq!(report("fresh.md", &["--fresh"]), honest, "the report that `--fresh` computes is the same report");
    let fresh = run(repo, &["status", "HEAD", "--fresh", "--verbose"], 0);
    assert!(!fresh.contains("FORGED") && fresh.contains("<="), "{fresh}");
    let ran = std::fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(ran, "", "no program of the repository folder ran");
}

#[test]
fn a_cache_folder_that_semilla_cannot_use_gives_one_note_and_a_result() {
    // Brief 5h, A2: the cache folder is not writable, or it is in the repository folder. The
    // command works with no cache.
    let dir = setup();
    let repo = &dir.path().join("repo");
    tagged_condition_change(repo);
    let blocked = dir.path().join("a-file");
    std::fs::write(&blocked, "").unwrap();
    for (cache, cause) in [(blocked.join("cache"), "Semilla cannot write the cache folder"), (repo.join(".semilla/cache"), "is in the repository folder")] {
        let out = Command::new(env!("CARGO_BIN_EXE_semilla"))
            .current_dir(repo)
            .env("SEMILLA_MIR_DRIVER", workspace_root().join("target/debug/semilla-mir-driver"))
            .env("SEMILLA_CACHE_DIR", &cache)
            .args(["status", "HEAD"])
            .output()
            .unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "{stdout}\n{stderr}");
        assert_eq!(stderr.matches("note: ").count(), 1, "{stderr}");
        assert!(stderr.contains(cause) && stderr.contains("Semilla works with no cache and computes each record again"), "{stderr}");
        assert!(stdout.contains(": ready") && stdout.contains("result: accepted"), "{stdout}");
        assert!(!cache.exists(), "Semilla wrote {}", cache.display());
    }
}

#[test]
fn a_stored_result_of_other_tools_is_not_valid() {
    // Brief 5h, A2: the agent can run Semilla with a false solver or driver (a variable). A
    // stored result holds the key of its tools. Other tools compute the result again.
    let z3 = workspace_root().join(".semilla/tools/z3");
    if !z3.exists() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    tagged_condition_change(repo);
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains(": ready"), "{status}");
    let other = dir.path().join("other-z3");
    std::fs::copy(&z3, &other).unwrap();
    let with_other = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_semilla"))
            .current_dir(repo)
            .env("SEMILLA_MIR_DRIVER", workspace_root().join("target/debug/semilla-mir-driver"))
            .env("SEMILLA_CACHE_DIR", cache_dir(repo))
            .env("SEMILLA_Z3", &other)
            .env("SEMILLA_NO_WORKER", "1")
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // Batch 5k, B7: `--no-wait` gives the present state. With no flag, `status <commit>` waits
    // for the full check.
    let first = with_other(&["status", "HEAD", "--no-wait"]);
    assert!(!first.contains(": ready") && first.contains("not checked on this machine"), "{first}");
    assert!(first.contains("Wait for it: `semilla status HEAD`"), "{first}");
    let second = with_other(&["status", "HEAD"]);
    assert!(second.contains(": ready") && second.contains("phase 2 (computed on this machine)") && !second.contains("[pending]"), "{second}");
}

#[cfg(unix)]
#[test]
fn a_clean_filter_gives_the_same_tree_to_step_and_to_tag() {
    // Brief 5h, A3: `step` stores the tree as `git commit` makes it, so a clean filter applies
    // to the two in the same way. Semilla reads the tree, not the file of the working tree.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let marker = dir.path().join("marker");
    let filter = marker_script(repo, ".semilla/filter", &marker, "sed 's/total(20)/total(30)/'");
    git(repo, &["config", "filter.grow.clean", filter.to_str().unwrap()]);
    write(repo, ".git/info/attributes", "*.rs filter=grow\n");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let step = run(repo, &["step", "raise the max"], 0);
    assert!(step.contains("changed literal `10` → `30`"), "the fact comes from the tree: {step}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "raise the max"]);
    assert!(git_out(repo, &["show", "HEAD:crates/shop-app/src/main.rs"]).contains("total(30)"));
    let tag = run(repo, &["tag"], 0);
    assert!(tag.contains("result: accepted"), "the tree of the step is the tree of the commit: {tag}");
    let status = run(repo, &["status", "HEAD", "--verbose", "--fresh"], 0);
    assert!(status.contains("changed literal `10` → `30`"), "{status}");
}

#[test]
fn a_target_file_is_rust_code_whatever_its_name() {
    // Brief 5h, A4 (cheat C1). Cargo names the root files. A build script with the name
    // `NOTES.md` and a binary with the name `LICENSE.txt` are Rust code: the file allow list does
    // not apply to them.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let commit = |message: &str| {
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message]);
    };
    let core = "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
    let app = "[package]\nname = \"shop-app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nshop-core = { path = \"../shop-core\" }\n";
    write(repo, "crates/shop-app/Cargo.toml", &format!("{app}\n[[bin]]\nname = \"license\"\npath = \"LICENSE.txt\"\n"));
    write(repo, "crates/shop-app/LICENSE.txt", "fn main() {\n    println!(\"{}\", limit(3));\n}\n\nfn limit(x: u32) -> u32 {\n    if x < 10 { x } else { 10 }\n}\n");
    commit("a binary with the name of a license");

    // The build script comes in: the manifest key and the new file are in the spotlight.
    write(repo, "crates/shop-core/Cargo.toml", &format!("{core}build = \"NOTES.md\"\n"));
    write(repo, "crates/shop-core/NOTES.md", "fn main() {\n    println!(\"cargo:rerun-if-changed=NOTES.md\");\n}\n");
    let out = run(repo, &["step", "add a build script"], 1);
    assert!(out.contains("crates/shop-core/NOTES.md: added") && out.contains("build script: this file is code of the build script of `shop_core`"), "{out}");
    let out = run(repo, &["step", "add a build script", "--reason", "crates/shop-core/Cargo.toml:package.build=the build needs a script", "--reason", "crates/shop-core/NOTES.md:=the build script"], 0);
    assert!(out.contains("accepted"), "{out}");
    commit("build script");
    assert!(run(repo, &["tag"], 0).contains("result: accepted"));

    // C1: a change of the build script is not a doc change. It needs a reason, and the report
    // shows the spotlight "build script" with the diff.
    write(repo, "crates/shop-core/NOTES.md", "fn main() {\n    println!(\"cargo:rerun-if-changed=NOTES.md\");\n    let _ = std::fs::write(\"/nonexistent/PWNED\", \"the build script ran\");\n}\n");
    let out = run(repo, &["step", "reword the notes"], 1);
    assert!(out.contains("crates/shop-core/NOTES.md: changed") && out.contains("build script") && out.contains("needs a reason"), "{out}");
    let out = run(repo, &["step", "reword the notes", "--reason", "crates/shop-core/NOTES.md:=the script writes a file"], 0);
    assert!(out.contains("spotlight: build script"), "{out}");
    let md = dir.path().join("c1.md");
    run(repo, &["status", "--wait", "600"], 0);
    // The step changes build code. A reviewer command runs it in the sandbox, and the report
    // shows the change of the build script first (design 28).
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("Build code changes (1): read these changes first") && text.contains("build script") && text.contains("PWNED"), "{text}");
    // With the careful option, the command does not run the build code.
    run(repo, &["review", "--no-build-code", "--md", md.to_str().unwrap()], 1);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("result: **not complete**") && text.contains("build script") && text.contains("PWNED"), "{text}");

    // The binary in `LICENSE.txt`: Semilla parses it, so a changed condition is a fact of the
    // code, not a file fact of a text file.
    let license = std::fs::read_to_string(repo.join("crates/shop-app/LICENSE.txt")).unwrap();
    write(repo, "crates/shop-app/LICENSE.txt", &license.replace("x < 10", "x <= 10"));
    // The driver records no unit for a root file with no `.rs` suffix. So no checked unit
    // reads the file, and the edit has no body evidence: it needs a reason (batch 5w, part A).
    let out = run(repo, &["step", "reword the license"], 1);
    assert!(out.contains("no checked unit reads the file `crates/shop-app/LICENSE.txt`") && out.contains("--reason 'crates/shop-app/LICENSE.txt=…'"), "{out}");
    let out = run(repo, &["step", "reword the license", "--reason", "crates/shop-app/LICENSE.txt=the limit is in the range"], 0);
    assert!(out.contains("shop_app (bin \"license\")::limit") && out.contains("[semantics · no compare]") && out.contains("note: no compare: no checked configuration compiles `shop_app (bin \"license\")::limit`"), "{out}");
    assert!(!out.contains("LICENSE.txt: changed"), "{out}");
}

#[test]
fn a_doc_test_is_code() {
    // Brief 5h, B1 (attempt 21). A doc comment with a code block is a test that `cargo test`
    // runs. A change of the block is a fact of the category `semantics`, and it stops an identity.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", "//! The shop rules.\n\npub mod cart;\n");
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("    pub fn total", "    /// The total of the items below `max`.\n    pub fn total"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "docs"]);

    // A doc change with no code block stays `other`. The text is the source text of the doc.
    write(repo, "crates/shop-core/src/lib.rs", "//! The rules of the shop.\n\npub mod cart;\n");
    let out = run(repo, &["step", "reword the crate doc"], 0);
    assert!(out.contains("changed docs `//! The shop rules.` → `//! The rules of the shop.` [docs · no code]"), "{out}");
    assert!(!out.contains("[doc ="), "{out}");

    // A rename and a new doc test in one step: the claim `rename` is gone.
    let renamed = CART_V0.replace("total", "sum_below").replace("    pub fn sum_below", "    /// The total of the items below `max`.\n    ///\n    /// ```no_run\n    /// std::fs::write(\"/tmp/pwned-by-doctest\", \"x\").unwrap();\n    /// ```\n    pub fn sum_below");
    write(repo, "crates/shop-core/src/cart.rs", &renamed);
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("total", "sum_below"));
    run(repo, &["step", "rename total", "--claim", "rename shop_core::cart::Cart::total to=sum_below"], 2);
    // Batch 7b, part 1: Semilla derives the rename, and the doc test is a fact of its own.
    // Semilla does not compile a doc test, so that fact has no compare.
    let out = run(repo, &["step", "rename total"], 0);
    assert!(out.contains("doc test of `shop_core::cart::Cart::sum_below` added (block attributes: `no_run`) [docs · no compare]") && out.contains("no compare: Semilla does not compile a doc test"), "{out}");
    assert!(fact_line(&out, "renamed `shop_core::cart::Cart::total` → `shop_core::cart::Cart::sum_below`").ends_with("[rename · code equal]"), "{out}");
    // The report shows the full lines of the doc.
    let md = dir.path().join("doc.md");
    run(repo, &["status", "--wait", "600"], 0);
    run(repo, &["review", "--full", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("+ /// std::fs::write(\"/tmp/pwned-by-doctest\", \"x\").unwrap();"), "{text}");

    // A changed block attribute is a change of the doc test: now `cargo test` runs the block.
    write(repo, "crates/shop-core/src/cart.rs", &renamed.replace("```no_run", "```"));
    let out = run(repo, &["step", "run the example"], 0);
    assert!(out.contains("doc test of `shop_core::cart::Cart::sum_below` changed [docs · no compare]"), "{out}");
}

#[test]
fn an_attribute_of_a_new_item_follows_the_allow_list() {
    // Brief 5h, B2 (D5). A new item follows the attribute allow list of a changed item. `#[test]`
    // and `#[cfg(test)]` are also allowed. Each other attribute needs a reason.
    let dir = setup();
    let repo = &dir.path().join("repo");

    // A new module with `#[path]`: the code comes from a file with another name.
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n\n#[path = \"extra.txt\"]\nmod extra;\n");
    write(repo, "crates/shop-core/src/extra.txt", "pub fn extra() -> u64 {\n    7\n}\n");
    let out = run(repo, &["step", "add the module extra"], 1);
    assert!(out.contains("spotlight: a new item with an attribute that can change the behavior: `path` on `shop_core::extra`; it needs a reason"), "{out}");
    assert!(out.contains("--reason 'shop_core::extra=<why>'"), "{out}");
    let out = run(repo, &["step", "add the module extra", "--reason", "shop_core::extra=the file has a text name"], 0);
    assert!(out.contains("reason: \"the file has a text name\""), "{out}");
    run(repo, &["undo"], 0);
    git(repo, &["checkout", "-q", "."]);
    std::fs::remove_file(repo.join("crates/shop-core/src/extra.txt")).unwrap();

    // New tests that do not run, or that pass when they fail.
    let tests = "\n#[cfg(test)]\nmod more {\n    #[test]\n    #[ignore]\n    fn slow() {\n        assert_eq!(1 + 1, 2);\n    }\n\n    #[test]\n    #[should_panic]\n    fn bad() {\n        assert_eq!(1 + 1, 3);\n    }\n\n    #[test]\n    fn good() {\n        assert_eq!(1 + 1, 2);\n    }\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{tests}"));
    let out = run(repo, &["step", "add tests"], 1);
    assert!(out.contains("`should_panic` on `shop_core::cart::more::bad`") && out.contains("`ignore` on `shop_core::cart::more::slow`"), "{out}");
    assert!(!out.contains("`test` on") && !out.contains("`cfg` on"), "{out}");

    // `#[test]`, `#[cfg(test)]`, and each attribute of the allow list need no reason.
    let plain = tests.replace("    #[ignore]\n", "").replace("    #[should_panic]\n", "").replace("1 + 1, 3", "1 + 2, 3");
    let more = "\n#[derive(Debug, Clone)]\n#[allow(dead_code)]\npub struct Note {\n    pub text: String,\n}\n\n#[inline]\n#[must_use]\npub fn note() -> u32 {\n    1\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{more}{plain}"));
    let out = run(repo, &["step", "add tests and a note"], 0);
    assert!(!out.contains("spotlight") && !out.contains("needs a reason"), "{out}");

    // Each other attribute needs a reason: a `cfg` other than `cfg(test)`, a link attribute, and
    // an attribute that Semilla does not know.
    let other = "\n#[cfg(not(test))]\npub fn only_release() -> u32 {\n    2\n}\n\n#[unsafe(no_mangle)]\npub extern \"C\" fn shop_abi() -> u32 {\n    3\n}\n\n#[track_caller]\npub fn tracked() -> u32 {\n    4\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{more}{plain}{other}"));
    let out = run(repo, &["step", "add three functions"], 1);
    assert!(out.contains("`cfg` on `shop_core::cart::only_release`"), "{out}");
    assert!(out.contains("`unsafe` on `shop_core::cart::shop_abi`") || out.contains("`no_mangle` on `shop_core::cart::shop_abi`"), "{out}");
    assert!(out.contains("`track_caller` on `shop_core::cart::tracked`"), "{out}");
}

#[test]
fn a_changed_order_of_generic_parameters_lists_each_site() {
    // Brief 5h, B4 (D9). Generic arguments go by position. A site with explicit arguments keeps
    // its text and its MIR text, and its result changes. The fact lists each item that names the
    // changed item, and it is never "neutral (proved)".
    let dir = setup();
    let repo = &dir.path().join("repo");
    let code = "\npub fn tier<const F: u64, const P: u64>(amount: u64) -> u64 {\n    F + amount * P / 100\n}\n\npub fn gold(amount: u64) -> u64 {\n    tier::<10, 2>(amount)\n}\n\n#[repr(C)]\npub struct Pair<A, B> {\n    pub a: A,\n    pub b: B,\n}\n\nimpl<A, B> Pair<A, B> {\n    pub fn width() -> usize {\n        std::mem::size_of::<A>()\n    }\n}\n\npub fn first_width() -> usize {\n    Pair::<u8, u16>::width()\n}\n\npub fn pass(pair: Pair<u8, u16>) -> Pair<u8, u16> {\n    pair\n}\n";
    let base = format!("{CART_V0}{code}");
    write(repo, "crates/shop-core/src/cart.rs", &base);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "generic items"]);
    let again = |repo: &Path| {
        run(repo, &["undo"], 0);
        git(repo, &["checkout", "-q", "."]);
    };

    // A function: the caller `gold` has an equal MIR text, and its result changes.
    write(repo, "crates/shop-core/src/cart.rs", &base.replace("tier<const F: u64, const P: u64>", "tier<const P: u64, const F: u64>"));
    let out = run(repo, &["step", "reorder tier"], 0);
    assert!(fact_line(&out, "generics of `shop_core::cart::tier`").contains("generic arguments go by position: 1 site(s) in 1 other item(s) [declaration · code different]"), "{out}");
    assert!(out.contains("affected bodies: shop_core::cart::gold, shop_core::cart::tier"), "{out}");
    // Design 50.4: the name "items with a use:" is the name of the default mode only.
    assert!(!out.contains("items with a use:"), "{out}");
    again(repo);

    // Batch 5k, D10: the name in a comment or in a literal is not a site.
    let words = base.replace("    tier::<10, 2>(amount)", "    // tier is the name of a function\n    let _ = \"tier\";\n    tier::<10, 2>(amount)");
    assert_ne!(words, base);
    write(repo, "crates/shop-core/src/cart.rs", &words.replace("tier<const F: u64, const P: u64>", "tier<const P: u64, const F: u64>"));
    let out = run(repo, &["step", "reorder tier"], 0);
    assert!(fact_line(&out, "generics of `shop_core::cart::tier`").contains("generic arguments go by position: 1 site(s) in 1 other item(s) [declaration · code different]"), "{out}");
    again(repo);

    // A type: no MIR text changes, and the layout of `Pair<u8, u16>` changes.
    write(repo, "crates/shop-core/src/cart.rs", &base.replace("pub struct Pair<A, B>", "pub struct Pair<B, A>"));
    let out = run(repo, &["step", "reorder Pair"], 0);
    assert!(fact_line(&out, "generics of `shop_core::cart::Pair`").contains("generic arguments go by position: 4 site(s) in 3 other item(s) [declaration · code different]"), "{out}");
    assert!(out.contains("affected bodies: shop_core::cart::first_width, shop_core::cart::impl Pair, shop_core::cart::pass") && !out.contains("code equal"), "{out}");
    again(repo);

    // An impl: the header gives the position of each parameter in the arguments of the type.
    write(repo, "crates/shop-core/src/cart.rs", &base.replace("impl<A, B> Pair<A, B>", "impl<B, A> Pair<B, A>"));
    let out = run(repo, &["step", "reorder the impl"], 0);
    assert!(fact_line(&out, "generics of `shop_core::cart::impl Pair`").contains("generic arguments go by position: 3 site(s) in 2 other item(s) [declaration · code different]"), "{out}");
    assert!(out.contains("affected bodies: shop_core::cart::Pair::width, shop_core::cart::first_width, shop_core::cart::pass"), "{out}");
    again(repo);

    // A new bound keeps the order: the fact has no site list.
    write(repo, "crates/shop-core/src/cart.rs", &base.replace("impl<A, B> Pair<A, B>", "impl<A: Copy, B> Pair<A, B>"));
    let out = run(repo, &["step", "add a bound"], 0);
    assert!(!out.contains("generic arguments go by position"), "{out}");
}

#[test]
fn a_build_script_cannot_write_outside_its_build_folder() {
    // Brief 5h, A5. The check build runs code of the change set. In the sandbox, this code writes
    // only the build folder: a write to the cache folder of the user, to the source files of the
    // build, or to the repository has no effect. With no sandbox, each result says so.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let cache = cache_dir(repo);
    let script = format!(
        "fn main() {{\n    let _ = std::fs::write({:?}, \"x\");\n    let _ = std::fs::write({:?}, \"x\");\n    let _ = std::fs::write(\"src/planted.rs\", \"x\");\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(std::path::Path::new(&out).join(\"made.txt\"), \"x\").unwrap();\n}}\n",
        cache.join("planted.txt"),
        repo.join("planted.txt")
    );
    write(repo, "crates/shop-core/build.rs", &script);
    let reason = "crates/shop-core/build.rs:=the build needs a script";
    let line = "the build ran with no sandbox: the write limit, the read limit, and the network limit are missing (the variable SEMILLA_NO_SANDBOX is set). A build script or a proc macro of this change set can change the result";
    let out = run(repo, &["step", "add a build script", "--reason", reason], 0);
    if let Some(missing) = semilla_normal::sandbox::missing() {
        // A machine with a missing limit: the check works, and the result names each missing limit.
        assert!(out.contains(&missing.line()), "{out}");
        return;
    }
    assert!(!out.contains("sandbox:"), "{out}");
    assert!(!cache.join("planted.txt").exists(), "the build script wrote the cache folder of the user");
    assert!(!repo.join("planted.txt").exists(), "the build script wrote the repository");
    let planted = find_named(&repo_cache(repo), "planted.rs");
    assert!(planted.is_empty(), "the build script wrote the source files of the build: {planted:?}");
    let status = run(repo, &["status", "--wait"], 0);
    assert!(!status.contains("sandbox:"), "{status}");

    // With no sandbox, the check of an agent command still works. The step has the line, and
    // the line names each missing limit (batch 5v, part D.2).
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let no_sandbox = |args: &[&str]| {
        let output = semilla_env(repo, args, &[("SEMILLA_NO_SANDBOX", "1")]);
        assert!(output.status.success(), "semilla {args:?}\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let out = no_sandbox(&["step", "rename a local"]);
    assert!(out.contains(&format!("! {line}")), "{out}");
    assert!(cache.join("planted.txt").exists(), "with no sandbox, the build script writes the cache folder");
    let status = no_sandbox(&["status", "--wait"]);
    assert!(status.contains(line), "{status}");
    // The state of the sandbox is a part of the key of each record (part D.4). A command with
    // the sandbox reads no record of that build: it builds again, and its result has no line.
    let status = run(repo, &["status", "--wait"], 0);
    assert!(!status.contains("sandbox:") && status.contains("1 code equal"), "{status}");
    let (md, html) = (dir.path().join("report.md"), dir.path().join("report.html"));
    let (_, out) = run_env(repo, &["review", "--md", md.to_str().unwrap()], &[]);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(!text.contains("sandbox:") && !text.contains("not checked"), "{out}\n{text}");
    // The policy (part D.3). The change set changes build code. A reviewer command with a
    // missing limit does not run it: each step is `not checked`, and the output says why.
    let guard = "The build sandbox is not complete: the write limit, the read limit, and the network limit are missing (the variable SEMILLA_NO_SANDBOX is set). The command does not run changed build code. To run it with this sandbox, use the option --build-code-no-sandbox.";
    let (_, out) = run_env(repo, &["review", "--md", md.to_str().unwrap()], &[("SEMILLA_NO_SANDBOX", "1")]);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(out.contains(guard), "{out}");
    assert!(text.contains("not checked") && !text.contains(line), "{text}");
    assert!(!text.contains("code equal"), "{text}");
    // The explicit option runs the build code with the limits that the machine has. The report
    // has the line.
    let env = [("SEMILLA_NO_SANDBOX", "1")];
    let (code, out) = run_env(repo, &["review", "--md", md.to_str().unwrap(), "--build-code-no-sandbox"], &env);
    assert!(code == 0 && !out.contains(guard), "{out}");
    let (code, out) = run_env(repo, &["review", "--html", html.to_str().unwrap(), "--build-code-no-sandbox"], &env);
    assert_eq!(code, 0, "{out}");
    let (md, html) = (std::fs::read_to_string(md).unwrap(), std::fs::read_to_string(html).unwrap());
    // The line is at the top of each report: it is the first warning.
    let first_fact = |text: &str| text.find("spotlight (step 0)").unwrap_or(text.len());
    assert!(md.find(&line.replace('_', "\\_")).is_some_and(|at| at < first_fact(&md)), "{md}");
    assert!(html.find(line).is_some_and(|at| at < first_fact(&html)), "the HTML report has the line at the top");

    // The stored report of a commit has the state of the sandbox in its key (part D.4). The
    // report of an agent command with no sandbox is not the report of a reviewer command with
    // the sandbox: the reviewer command computes its result again.
    commit_all(repo, "a build script");
    no_sandbox(&["tag"]);
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "0", "--build-code-no-sandbox"], &env);
    assert!(code == 0 && out.contains(": ready\n") && out.contains(line), "the stored report has the line: {out}");
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600"], &[]);
    assert!(code == 0 && out.contains(": ready\n") && out.contains("1 code equal") && !out.contains("sandbox:"), "{out}");
}

#[test]
fn a_later_step_prints_the_phase_2_state_of_a_fast_step() {
    // Brief 5h, C2 and C3 (D7, doc problem 5). A step of `--fast` has only a phase 1 result. Each
    // later step prints its phase 2 state. A rejected step that is not the last step gets the
    // exact commands: how many times to run `semilla undo`, and each step command.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/build.rs", "fn main() {\n    println!(\"cargo:rerun-if-changed=bonus.txt\");\n    let bonus = std::fs::read_to_string(\"bonus.txt\").unwrap();\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(format!(\"{out}/bonus.rs\"), format!(\"pub const BONUS: u32 = {};\", bonus.trim())).unwrap();\n}\n");
    write(repo, "crates/shop-core/bonus.txt", "100\n");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n\ninclude!(concat!(env!(\"OUT_DIR\"), \"/bonus.rs\"));\n\npub fn bonus() -> u32 {\n    BONUS\n}\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "the bonus"]);

    // These steps start no background worker. A `--fast` step prints the long route when the
    // background result of the change set is complete at its start, and the short line when
    // it is not. With a worker, the text of the last `--fast` step depends on the time.
    let no_worker = |repo: &Path, args: &[&str], expect: i32| -> String {
        let output = semilla_env(repo, args, &[("SEMILLA_NO_WORKER", "1")]);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert_eq!(output.status.code(), Some(expect), "semilla {args:?}\n--- stdout\n{stdout}\n--- stderr\n{}", String::from_utf8_lossy(&output.stderr));
        stdout
    };
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    no_worker(repo, &["step", "a larger limit"], 0);
    // The data file: phase 1 does not know its effect, so the compare result is pending.
    write(repo, "crates/shop-core/bonus.txt", "900\n");
    let fast = no_worker(repo, &["step", "--fast", "update the bonus data"], 0);
    assert!(fast.contains("accepted (phase 1;") && fast.contains("crates/shop-core/bonus.txt: changed [file · pending]"), "{fast}");
    assert!(!fast.contains("step 1: phase 2"), "the step itself has no phase 2 line: {fast}");
    // A later step with phase 2 computes the state of the fast step, and gives the commands.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let later = no_worker(repo, &["step", "rename a local"], 0);
    assert!(later.contains("accepted: step 2"), "{later}");
    // Design 40.3: the fact names the node of the changed MIR element.
    assert!(later.contains("step 1: phase 2 rejected: the compiled code of `shop_core::bonus` changes with no edit at its place: `BONUS`"), "{later}");
    assert!(later.contains("to fix step 1 (it is not the last step): run `semilla undo` 2 times.") && later.contains("the files stay"), "{later}");
    let commands: Vec<&str> = later.lines().filter(|line| line.trim_start().starts_with("git restore --source=")).collect();
    assert_eq!(commands.len(), 2, "{later}");
    assert!(commands[0].ends_with("&& semilla step 'update the bonus data' --reason 'shop_core::bonus=<why>'"), "{later}");
    assert!(commands[1].ends_with("&& semilla step 'rename a local'"), "{later}");
    // A later step of `--fast` prints the stored state.
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    let fast = no_worker(repo, &["step", "--fast", "an early return"], 0);
    assert!(fast.contains("step 1: phase 2 rejected: the compiled code of `shop_core::bonus` changes") && fast.contains("to fix step 1: run `semilla undo` 3 time(s)"), "{fast}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);

    // `tag` rejects the change set with the same commands, and the route works as written.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-a", "-m", "work"]);
    let refused = semilla(repo, &["tag"]);
    assert!(!refused.status.success());
    let tag = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(tag.starts_with("result: rejected"), "{tag}");
    assert!(tag.contains("✗ step 1: the compiled code of `shop_core::bonus` changes with no edit at its place: `BONUS`"), "{tag}");
    assert!(tag.contains("to fix step 1 (it is not the last step): run `semilla undo` 2 times.") && tag.contains("Then run `semilla tag` again."), "{tag}");
    assert!(!tag.contains("for the last step"), "{tag}");
    let commands: Vec<String> = tag.lines().filter(|line| line.trim_start().starts_with("git restore --source=")).map(|line| line.trim().replace("<why>", "the build script reads the file")).collect();
    assert_eq!(commands.len(), 2, "{tag}");
    run(repo, &["undo"], 0);
    run(repo, &["undo"], 0);
    for command in &commands {
        // Each line is `git restore … && semilla step …`: run the two parts.
        let (restore, step) = command.split_once(" && semilla ").unwrap();
        let status = Command::new("sh").current_dir(repo).args(["-c", restore]).status().unwrap();
        assert!(status.success(), "{restore}");
        let words: Vec<String> = step.split('\'').map(str::trim).filter(|word| !word.is_empty()).map(str::to_string).collect();
        let words: Vec<&str> = words.iter().flat_map(|word| if word == "step" || word.starts_with("--") { word.split(' ').collect::<Vec<_>>() } else { vec![word.as_str()] }).collect();
        let out = run(repo, &words, 0);
        assert!(out.contains("accepted: step"), "{out}");
    }
    let tag = run(repo, &["tag"], 0);
    assert!(tag.contains("result: accepted · 3 steps"), "{tag}");
}

// ---- The build code rule (design 28, invariant I6) ----

const BUILD_REASON: &str = "crates/shop-core/build.rs:=the build needs a script";

/// A build script that writes a marker file, and one more file for each profile. In the
/// sandbox, the write has no effect.
fn marker_build_script(marker: &Path) -> String {
    let profile = format!("{}.", marker.display());
    format!("fn main() {{\n    println!(\"cargo:rerun-if-changed=build.rs\");\n    let _ = std::fs::write({marker:?}, \"the build script ran\");\n    let _ = std::fs::write(format!(\"{{}}{{}}\", {profile:?}, std::env::var(\"PROFILE\").unwrap()), \"\");\n}}\n")
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message]);
}

/// Runs semilla with more environment variables. Returns the exit code and stdout.
fn run_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    let output = semilla_env(repo, args, env);
    (output.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

/// Starts the UI with `args`, reads `/api/changeset`, and stops the UI.
fn ui_changeset(repo: &Path, args: &[&str]) -> String {
    ui_get(repo, args, &["/api/changeset"]).remove(0)
}

/// Starts the UI with `args`, reads each path of `paths`, and stops the UI.
fn ui_get(repo: &Path, args: &[&str], paths: &[&str]) -> Vec<String> {
    let root = workspace_root();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
        .env("SEMILLA_CACHE_DIR", cache_dir(repo))
        .current_dir(repo)
        .args(["ui", "--port", &port.to_string()])
        .args(args)
        .env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let get = |path: &str| -> Option<String> {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").ok()?;
        let mut text = String::new();
        stream.read_to_string(&mut text).ok()?;
        Some(text)
    };
    let mut texts = Vec::new();
    for _ in 0..600 {
        if let Some(text) = get(paths[0]) {
            texts.push(text);
            texts.extend(paths[1..].iter().map(|path| get(path).expect("the UI answers")));
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!texts.is_empty(), "the UI starts");
    texts
}

/// Each file with the name `name` below `dir`.
fn find_named(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            found.extend(find_named(&path, name));
        } else if path.file_name().is_some_and(|file| file == name) {
            found.push(path);
        }
    }
    found
}

const CLAIM: &str = "no-io shop_core::cart::Cart::total";
const MARK_1: &str = "(with the build code of step 1)";
/// The line `RAN_1` of a run with the variable SEMILLA_NO_SANDBOX and the option
/// --build-code-no-sandbox (batch 5w, D5).
fn ran_1_no_sandbox() -> String {
    RAN_1.replace("in the sandbox.", "with a sandbox that is not complete: the write limit, the read limit, and the network limit are missing (the variable SEMILLA_NO_SANDBOX is set).")
}
const RAN_1: &str = "! build code changed in step 1 (crates/shop-core/build.rs). The check ran it in the sandbox. It can change each result from step 1 on. Read that change first. Each result `code equal` and each verified claim that depends on it has the mark (with the build code of step 1).";
const SECTION: &str = "! build code changed in this step; read this change first";
const RESULT_4: &str = "result: accepted · 4 steps · 4 facts: 1 code different, 2 code equal (1 with the build code of step 1), 1 no code (1 with a spotlight) · 2 claims: 2 verified (1 with the build code of step 1) · build code changed from step 1\n";

/// A session of four steps. Step 0 is clean: it has a proof and a claim. Step 1 adds a build
/// script. Step 2 changes logic. Step 3 has a proof and a claim.
fn build_code_session(repo: &Path, marker: &Path) {
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let out = run(repo, &["step", "rename a local", "--claim", CLAIM], 0);
    assert!(!out.contains("build code"), "a clean step has no line about build code\n{out}");
    write(repo, "crates/shop-core/build.rs", &marker_build_script(marker));
    let out = run(repo, &["step", "add a build script", "--reason", BUILD_REASON], 0);
    let full = "! build code changed in step 1: crates/shop-core/build.rs. It runs in the sandbox during each check, and it can change each result from step 1 on. Each result `code equal` and each verified claim that depends on it has the mark (with the build code of step 1).";
    assert!(out.contains(full), "{out}");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let out = run(repo, &["step", "change the limit of the cart"], 0);
    let short = "! build code changed in step 1 (crates/shop-core/build.rs): each result `code equal` and each verified claim of this step has the mark (with the build code of step 1).";
    assert!(out.contains(short) && !out.contains(full), "{out}");
    write(repo, "crates/shop-core/src/cart.rs", &CART_V1.replace("*x", "*item").replace("for x in", "for item in").replace("+= x", "+= item"));
    let out = run(repo, &["step", "rename the loop variable", "--claim", CLAIM], 0);
    assert!(out.contains(short), "{out}");
}

/// The steps that a line names: `step 3`, `step: 3`, `steps 0, 3`, `steps 1–3`.
fn named_steps(line: &str) -> Vec<usize> {
    let mut steps = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("step") {
        rest = &rest[at + 4..];
        let list = rest.strip_prefix('s').unwrap_or(rest);
        let list = list.strip_prefix(':').unwrap_or(list);
        let Some(mut list) = list.strip_prefix(' ') else { continue };
        loop {
            let digits: String = list.chars().take_while(char::is_ascii_digit).collect();
            let Ok(step) = digits.parse::<usize>() else { break };
            steps.push(step);
            list = &list[digits.len()..];
            if let Some(end) = list.strip_prefix('–') {
                let digits: String = end.chars().take_while(char::is_ascii_digit).collect();
                steps.extend(step + 1..=digits.parse::<usize>().unwrap_or(step));
                list = &end[digits.len()..];
            }
            match list.strip_prefix(", ") {
                Some(next) => list = next,
                None => break,
            }
        }
    }
    steps
}

/// The text of an HTML line with no tags.
fn strip_tags(line: &str) -> String {
    let mut text = String::new();
    let mut tag = false;
    for c in line.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            _ if !tag => text.push(c),
            _ => {}
        }
    }
    text
}

/// Invariant I6 for a text output (terminal, MCP, Markdown, HTML). A line shows a proof if it
/// has the text `code equal` or `verified`. Such a line must have the mark of
/// `first`, the first step with changed build code. The exceptions are a line that names only
/// steps before `first`, and a line with no step in the block of a step before `first`.
fn no_proof_without_mark(name: &str, text: &str, first: usize) {
    let mark = format!("with the build code of step {first}");
    let mut block: Option<usize> = None;
    let mut proofs = 0;
    for raw in text.lines() {
        let plain = strip_tags(raw);
        if raw.starts_with("step ") || raw.starts_with("### ") || raw.contains("class=\"group-head\"") {
            block = named_steps(&plain.replace(&mark, "")).first().copied();
        } else if raw.starts_with("result:") || raw.starts_with("## ") || raw.starts_with("<h2") {
            block = None;
        }
        let line = plain.replace("0 verified", "").replace("not verified", "");
        if !["code equal", "verified"].iter().any(|word| line.contains(word)) {
            continue;
        }
        proofs += 1;
        if line.contains(&mark) {
            continue;
        }
        let named = named_steps(&line);
        let clean = if named.is_empty() { block.is_some_and(|step| step < first) } else { named.iter().all(|step| *step < first) };
        assert!(clean, "{name}: this line shows a proof that used changed build code, and it has no mark (I6)\n{raw}\n\n{text}");
    }
    assert!(proofs > 0, "{name}: the output shows no proof\n{text}");
}

/// Invariant I6 for the data of the UI. An object that shows a proof (the compare result
/// `code_equal` or the status `verified`) of a step from `first` on must have a mark.
fn json_proofs_have_the_mark(name: &str, value: &serde_json::Value, first: u64, proofs: &mut usize) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let is = |key: &str, text: &str| map.get(key).and_then(Value::as_str) == Some(text);
            let mut steps: Vec<u64> = map.get("step").and_then(Value::as_u64).into_iter().collect();
            steps.extend(map.get("steps").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64));
            if (is("compare", "code_equal") || is("status", "verified")) && steps.iter().any(|step| *step >= first) {
                *proofs += 1;
                let mark = map.get("mark").and_then(Value::as_str).is_some_and(|mark| mark.contains("with the build code of step")) || map.get("build_code").is_some_and(Value::is_u64);
                assert!(mark, "{name}: this object shows a proof that used changed build code, and it has no mark (I6)\n{value}");
            }
            for child in map.values() {
                json_proofs_have_the_mark(name, child, first, proofs);
            }
        }
        Value::Array(list) => list.iter().for_each(|child| json_proofs_have_the_mark(name, child, first, proofs)),
        _ => {}
    }
}

/// The JSON body of an HTTP answer.
fn json_body(answer: &str) -> serde_json::Value {
    let body = answer.split("\r\n\r\n").nth(1).unwrap_or_default();
    // The body can have the chunked form: take the text from the first brace to the last one.
    let (start, end) = (body.find(['{', '[']).unwrap_or(0), body.rfind(['}', ']']).map_or(body.len(), |at| at + 1));
    serde_json::from_str(&body[start..end]).unwrap_or_else(|error| panic!("{error}\n{answer}"))
}

#[test]
fn each_command_runs_changed_build_code_and_marks_each_proof() {
    // Design 28, rules 1, 3, 4, 5, and invariant I6. Each command runs the changed build code
    // in the sandbox. Each output form shows the build code change first, and each proof from
    // that step on has the mark.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let marker = dir.path().join("marker.txt");
    build_code_session(repo, &marker);
    assert!(!marker.exists(), "the sandbox stops the write of the build script");

    // The records of a tree with changed build code have a mark, and they are only in the
    // separate folder set. The stored result of the agent command has the mark too.
    let cache = repo_cache(repo);
    let marks = find_named(&cache, ".build-code");
    assert!(!marks.is_empty() && marks.iter().all(|path| path.starts_with(cache.join("build-code"))), "{marks:?}");
    assert!(cache.join("reports/session.build-code.json").is_file(), "the stored result of the agent command has the mark");

    // The terminal: `status`, `verify`, and the MCP tool `results`. No option is necessary.
    let short = run(repo, &["status", "--wait", "600"], 0);
    assert!(short.contains(RAN_1) && short.contains(RESULT_4), "{short}");
    no_proof_without_mark("status", &short, 1);
    let out = run(repo, &["status", "--verbose"], 0);
    assert!(out.starts_with("session: ready\n") && out.contains(RAN_1) && out.contains(RESULT_4), "{out}");
    no_proof_without_mark("status --verbose", &out, 1);
    // Step 0 has no mark. Step 3 has the mark on the claim and on the proved fact.
    let (step_0, later) = out.split_once("step 1  ").expect("the text of step 1");
    assert!(step_0.contains("✓ c1 verified (mir)\n") && step_0.contains("code equal]\n") && !step_0.split("step 0  ").nth(1).unwrap().contains(MARK_1), "{out}");
    let step_3 = later.split_once("step 3  ").expect("the text of step 3").1;
    assert!(step_3.contains(&format!("✓ c2 verified (mir) {MARK_1}\n")) && step_3.contains(&format!("code equal] {MARK_1}\n")), "{out}");
    // The build code change is the first section of step 1, with its spotlight and its diff.
    assert!(later.starts_with(&format!("\"add a build script\"\n  {SECTION}\n  ◆ f1 derived: crates/shop-core/build.rs: added [build code · no code]\n      spotlight: build script")), "{out}");
    assert!(later.contains("reason: \"the build needs a script\"") && later.contains("the build script ran"), "{out}");
    let out = run(repo, &["verify"], 0);
    assert!(out.contains(RAN_1) && out.contains(&format!("code equal] {MARK_1}")), "{out}");
    no_proof_without_mark("verify", &out, 1);
    let answers = mcp_calls(repo, &[("results", serde_json::json!({"verbose": true, "wait": 600})), ("verify", serde_json::json!({}))]);
    for (tool, (text, error)) in ["results", "verify"].iter().zip(&answers) {
        assert!(!error && text.contains(RAN_1) && text.contains(SECTION) && text.contains(MARK_1), "{text}");
        no_proof_without_mark(&format!("the MCP tool `{tool}`"), text, 1);
    }

    // The Markdown report (short and full) and the HTML report.
    let (md, full, html) = (dir.path().join("r.md"), dir.path().join("full.md"), dir.path().join("r.html"));
    let out = run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    assert!(out.contains(RAN_1), "{out}");
    run(repo, &["review", "--full", "--md", full.to_str().unwrap()], 0);
    run(repo, &["review", "--html", html.to_str().unwrap()], 0);
    let texts = [("the Markdown report", &md), ("the full Markdown report", &full), ("the HTML report", &html)].map(|(name, path)| (name, std::fs::read_to_string(path).unwrap()));
    for (name, text) in &texts {
        // The scripts and the styles of the HTML report are not text of the report.
        let text = text.split("<script").next().unwrap().rsplit("</style>").next().unwrap();
        no_proof_without_mark(name, text, 1);
        let first_group = text.find("step 0 ·").or(text.find(">step 0<")).expect("the group of step 0");
        let section = text.find("Build code changes (1): read these changes first").unwrap_or(usize::MAX);
        assert!(section < first_group && text.contains("the build script ran"), "{name}: the build code change comes first, with its diff\n{text}");
        assert!(text.contains("2 code equal (1 with the build code of step 1)") && text.contains("2 verified (1 with the build code of step 1)"), "{name}: the counts\n{text}");
    }
    let md_text = &texts[0].1;
    assert!(md_text.contains("\\! build code changed in step 1 (crates/shop-core/build.rs). The check ran it in the sandbox.") && md_text.contains("result: **accepted**"), "{md_text}");
    // R61.6-1: the Markdown review has the equivalence of each fact at the end of its item.
    assert!(md_text.contains(&format!("3 edit(s) \\[rename · code equal\\] {MARK_1}; equivalence: equivalent (proved by equal MIR)\n")) && md_text.contains(&format!("- verified (proof mir) {MARK_1} · ")), "{md_text}");
    assert!(md_text.contains("3 edit(s) \\[rename · code equal\\]; equivalence: equivalent (proved by equal MIR)\n") && md_text.contains("- verified (proof mir) · "), "{md_text}");
    assert!(texts[1].1.contains(&format!("(1 code equal (1 {})   step 3", &MARK_1[1..])) && texts[1].1.contains(&format!("[rename · code equal] {MARK_1}   step 3")), "{}", texts[1].1);

    // The data of the UI.
    let answers = ui_get(repo, &[], &["/api/changeset", "/api/group/G4", "/api/tree", "/api/search?q=total"]);
    let mut proofs = 0;
    for answer in &answers {
        json_proofs_have_the_mark("the UI data", &json_body(answer), 1, &mut proofs);
    }
    assert!(proofs >= 6, "the UI data shows the proofs of step 3: {proofs}");
    let set = json_body(&answers[0]);
    assert!(answers[0].contains(RAN_1) && answers[0].contains("\"not_complete\":false"), "{}", answers[0]);
    assert_eq!(set["fact_counts"]["marked"], 1, "{set}");
    assert_eq!(set["fact_counts"]["marked_text"], " (1 with the build code of step 1)", "{set}");
    assert_eq!(set["counts"]["marked_text"], " (1 with the build code of step 1)", "{set}");
    // The group of the build code change is the first group, and its fact has the flag.
    assert_eq!(set["groups"][0]["steps"], serde_json::json!([1]), "{set}");
    assert!(set["facts"].as_array().unwrap().iter().any(|fact| fact["step"] == 1 && fact["build_code_change"] == true), "{set}");

    // Part B.1: `tag` stores its result, so `status <commit>` gives the result at once. It
    // prints no line about a check in the background.
    commit_all(repo, "a build script");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains(RESULT_4), "{out}");
    let out = run(repo, &["status", "HEAD", "--wait", "0"], 0);
    assert!(out.contains(": ready\n") && out.contains(RAN_1) && out.contains(RESULT_4), "{out}");
    assert!(!out.contains("runs in the background") && !out.contains("the check runs now") && !out.contains("not checked"), "{out}");

    // A reviewer with an empty cache folder and the sandbox: the command runs the changed build
    // code with no option. The result is the same, and the sandbox stops the write of the script.
    let fresh = dir.path().join("reviewer-cache-sandbox");
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600"], &[("SEMILLA_CACHE_DIR", fresh.to_str().unwrap())]);
    assert!(code == 0 && out.contains(RESULT_4) && !out.contains("not checked") && !out.contains("sandbox:"), "{out}");
    assert!(!marker.exists(), "the sandbox stops the write of the build script");

    // A reviewer with an empty cache folder and no sandbox (batch 5v, part D.3): the command
    // does not run the changed build code. The result is that of `--no-build-code`, and one
    // line says why. The variable does not permit the run: the agent can set it.
    let fresh = dir.path().join("reviewer-cache");
    let env = [("SEMILLA_CACHE_DIR", fresh.to_str().unwrap()), ("SEMILLA_NO_SANDBOX", "1")];
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600", "--verbose"], &env);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("result: not complete · step 0: ") && out.contains(NOT_CHECKED_1_3) && !out.contains(MARK_1), "{out}");
    assert!(out.contains("The build sandbox is not complete: the write limit, the read limit, and the network limit are missing (the variable SEMILLA_NO_SANDBOX is set). The command does not run changed build code. To run it with this sandbox, use the option --build-code-no-sandbox.\n"), "{out}");
    assert!(!marker.exists(), "a reviewer command with no sandbox ran the changed build code");
    // The explicit option: the command runs the changed build code for each build
    // configuration. The result is the same, and it has the line about the sandbox.
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600", "--verbose", "--build-code-no-sandbox"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("! the build ran with no sandbox: the write limit, the read limit, and the network limit are missing (the variable SEMILLA_NO_SANDBOX is set)"), "{out}");
    // Batch 5w, D5: a limit of the sandbox is missing, so the line does not say "in the sandbox".
    assert!(out.contains(&ran_1_no_sandbox()) && !out.contains(RAN_1) && out.contains(RESULT_4) && !out.contains("not checked"), "{out}");
    // Part B.2: a final result has the first line `ready`, and no line about the background.
    assert!(out.contains(": ready\n") && !out.contains("runs in the background") && !out.contains("the check runs now"), "{out}");
    no_proof_without_mark("status HEAD", &out, 1);
    assert!(marker.exists(), "with no sandbox, the build script runs");
    for profile in ["debug", "release"] {
        assert!(dir.path().join(format!("marker.txt.{profile}")).exists(), "the build script runs for the profile `{profile}`\n{out}");
    }
}

const NOT_CHECKED_1_3: &str = "steps 1–3 not checked: build code changed in step 1 (crates/shop-core/build.rs). The command does not run it (the option --no-build-code, or a build sandbox that is not complete).";

#[test]
fn the_careful_option_runs_no_changed_build_code() {
    // Design 28, rule 6. With `--no-build-code`, a command does not build a tree with changed
    // build code: the steps from the change on are `not checked`, and the result is
    // `not complete` with the exit code 1. The old option `--trust-build-code` is gone.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let marker = dir.path().join("marker.txt");
    build_code_session(repo, &marker);
    run(repo, &["status", "--wait", "600"], 0);

    // The reports and the UI on the session.
    let (md, html) = (dir.path().join("r.md"), dir.path().join("r.html"));
    let out = run(repo, &["review", "--no-build-code", "--md", md.to_str().unwrap()], 1);
    assert!(out.contains("result: not complete") && out.contains(NOT_CHECKED_1_3), "{out}");
    run(repo, &["review", "--no-build-code", "--html", html.to_str().unwrap()], 1);
    let (md_text, html_text) = (std::fs::read_to_string(&md).unwrap(), std::fs::read_to_string(&html).unwrap());
    let top = |text: &str| text.find(NOT_CHECKED_1_3).is_some_and(|at| at < text.find("step 0").unwrap_or(text.len()) + 200);
    assert!(top(&md_text) && md_text.contains("result: **not complete**") && md_text.contains("| Result | not complete |") && !md_text.contains(MARK_1), "{md_text}");
    assert!(top(&html_text) && html_text.contains("not complete"), "the HTML report has the text at the top");
    // The facts of the build code change show with the spotlight, the reason, and the diff.
    assert!(md_text.contains("derived: crates/shop-core/build.rs: added \\[build code · not checked\\]; spotlight: build script") && md_text.contains("reason: \"the build needs a script\"") && md_text.contains("the build script ran"), "{md_text}");
    // Batch 5w, D4: the section "Spotlight" says what it counts and what the summary counts.
    let counts = "1 line names sensitive code that a step changed. The summary counts a different thing: 1 fact with a spotlight. Each such fact shows its spotlight and its reason in its group.";
    assert!(md_text.contains(&format!("### Spotlight\n\n{counts}\n\n- step 1: build script: ")), "{md_text}");
    assert!(html_text.contains(&format!("<h3>Spotlight</h3>\n<p class=\"note\">{counts}</p>")), "the HTML report has the two counts");
    // The count of a step that has no check is the count of the syntax facts, and the line says so.
    let out = run(repo, &["status", "--no-build-code"], 1);
    assert!(out.contains("step 2  \"change the limit of the cart\" · 1 fact from the syntax only, not checked\n") && out.contains("step 3  \"rename the loop variable\" · 1 fact from the syntax only, not checked · 1 claim\n"), "{out}");
    let ui = ui_changeset(repo, &["--no-build-code"]);
    assert!(ui.contains(NOT_CHECKED_1_3) && ui.contains("\"not_complete\":true"), "{ui}");

    commit_all(repo, "a build script");
    run(repo, &["tag"], 0);

    // A reviewer with an empty cache folder and no sandbox: a build of the changed build code
    // writes the marker file. The command with the careful option does not write it.
    let fresh = dir.path().join("reviewer-cache");
    let env = [("SEMILLA_CACHE_DIR", fresh.to_str().unwrap()), ("SEMILLA_NO_SANDBOX", "1")];
    let careful = |name: &str| {
        let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600", "--verbose", "--no-build-code"], &env);
        assert_eq!(code, 1, "{name}\n{out}");
        assert!(out.contains("result: not complete · step 0: ") && out.contains(NOT_CHECKED_1_3) && !out.contains(MARK_1), "{name}\n{out}");
        // Step 0 has the full result. Each later step has its facts with the class
        // `not checked`: no proof, no affected bodies, never `neutral`.
        let (before, after) = out.split_once("step 1  ").expect("the text of step 1");
        assert!(before.contains("code equal]") && before.contains("✓ c1 verified (mir)"), "{name}\n{out}");
        assert!(after.contains("crates/shop-core/build.rs: added [build code · not checked]") && after.contains("spotlight: build script") && after.contains("reason: \"the build needs a script\""), "{name}\n{out}");
        let facts = after.split("result: ").next().unwrap();
        assert!(!facts.contains("code equal") && !facts.contains("code different") && !facts.contains("affected bodies") && !facts.contains("verified"), "{name}\n{out}");
    };
    careful("the first run");
    assert!(!marker.exists(), "a command with --no-build-code ran the changed build code");

    // The old option: one line says that the option is gone. The command gives the default result.
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600", "--trust-build-code", "--build-code-no-sandbox"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("The option --trust-build-code is gone: the command runs changed build code in the sandbox by default. Remove the option.\n"), "{out}");
    // Batch 5w, D5: a limit of the sandbox is missing, so the line does not say "in the sandbox".
    assert!(marker.exists() && out.contains(&ran_1_no_sandbox()) && !out.contains(RAN_1) && out.contains(RESULT_4), "{out}");
    let (code, out) = run_env(repo, &["verify", "--help"], &env);
    assert!(code == 0 && out.contains("--no-build-code") && !out.contains("--trust-build-code"), "{out}");
    let (code, out) = run_env(repo, &["status", "HEAD", "--no-build-code", "--trust-build-code"], &env);
    assert!(code == 2 && out.contains("cannot be used with"), "{out}");

    // The default result has a mark. A command with the careful option reads no marked result
    // and no marked record: it gives `not complete` again.
    careful("after the default run");
    // The agent commands have no such option: an agent command always runs the build code.
    let (code, out) = run_env(repo, &["step", "x", "--no-build-code"], &env);
    assert!(code == 2 && out.contains("unexpected argument"), "{out}");
}

#[test]
fn build_code_that_goes_back_to_the_base_ends_the_mark() {
    // Design 28, case 7. Step 1 adds a build script and step 4 removes it. The old side of
    // step 4 has changed build code, so steps 1, 2, 3, and 4 have the mark. Step 5 has two
    // trees with the build code of the base: its proof has no mark.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let marker = dir.path().join("marker.txt");
    build_code_session(repo, &marker);
    std::fs::remove_file(repo.join("crates/shop-core/build.rs")).unwrap();
    let out = run(repo, &["step", "remove the build script", "--reason", "crates/shop-core/build.rs:=the build needs no script"], 0);
    assert!(out.contains("! build code changed in step 1 (crates/shop-core/build.rs): each result `code equal` and each verified claim of this step has the mark (with the build code of step 1)."), "{out}");
    let cart = std::fs::read_to_string(repo.join("crates/shop-core/src/cart.rs")).unwrap();
    write(repo, "crates/shop-core/src/cart.rs", &cart.replace("sum", "acc"));
    let out = run(repo, &["step", "rename the sum"], 0);
    assert!(!out.contains("build code"), "step 5 has the build code of the base on the two sides\n{out}");
    commit_all(repo, "a build script comes and goes");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted · 6 steps") && out.contains(" · build code changed in steps 1–4\n"), "{out}");
    let out = run(repo, &["status", "HEAD", "--verbose"], 0);
    assert!(out.contains("! build code changed in step 1 (crates/shop-core/build.rs). The check ran it in the sandbox. It can change each result in steps 1–4."), "{out}");
    assert!(out.contains("3 code equal (1 with the build code of step 1)"), "{out}");
    let step_3 = out.split_once("step 3  ").expect("the text of step 3").1.split("step 4  ").next().unwrap();
    assert!(step_3.contains(&format!("code equal] {MARK_1}\n")), "{out}");
    let step_5 = out.split_once("step 5  ").expect("the text of step 5").1.split("result: ").next().unwrap();
    assert!(step_5.contains("code equal]\n") && !step_5.contains("with the build code"), "{out}");
    // With the careful option, steps 1–4 have no check. Step 5 has the full result.
    let out = run(repo, &["status", "HEAD", "--wait", "600", "--verbose", "--no-build-code"], 1);
    assert!(out.contains("result: not complete · steps 0, 5: ") && out.contains("steps 1–4 not checked: build code changed in step 1 (crates/shop-core/build.rs). The command does not run it (the option --no-build-code, or a build sandbox that is not complete)."), "{out}");
    let step_5 = out.split_once("step 5  ").expect("the text of step 5").1.split("result: ").next().unwrap();
    assert!(step_5.contains("code equal]\n") && !step_5.contains("not checked"), "{out}");
}

#[test]
fn a_step_can_mix_build_code_and_other_code() {
    // Design 28, rule 2. `step` accepts a step that changes build code and other code. The
    // build code change is the first section of the step, and the proof of the step has the
    // mark of that step.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let marker = dir.path().join("marker.txt");
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    write(repo, "crates/shop-core/build.rs", &marker_build_script(&marker));
    let out = run(repo, &["step", "rename a local, change the limit, add a build script", "--reason", BUILD_REASON], 0);
    assert!(out.contains("! build code changed in step 0: crates/shop-core/build.rs. It runs in the sandbox during each check"), "{out}");
    let out = run(repo, &["status", "--wait", "600", "--verbose"], 0);
    // The section comes before each other fact of the step.
    let section = out.find(SECTION).expect("the section");
    let build = out.find("crates/shop-core/build.rs: added [build code · no code]").expect("the build code fact");
    let (proof, logic) = (out.find("code equal] (with the build code of step 0)\n").expect("the proof with the mark"), out.find("code different]").expect("the logic fact"));
    assert!(section < build && build < proof.min(logic), "{out}");
    assert!(out.contains("1 code equal (1 with the build code of step 0)") && out.contains(" · build code changed from step 0\n"), "{out}");
    no_proof_without_mark("status --verbose", &out, 0);
    let md = dir.path().join("mix.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    let section = text.find("#### Build code changes (1): read these changes first").expect("the section");
    assert!(section < text.find("#### Derived facts").expect("the other facts") && text.contains("code equal\\] (with the build code of step 0)"), "{text}");
    no_proof_without_mark("the Markdown report", &text, 0);
}

#[test]
fn a_file_that_build_code_can_read_costs_only_the_mark() {
    // Design 28, rule (d). The tree has a build script. A changed file outside the neutral
    // allow list counts as changed build code, because the build script can read it. The cost
    // is the mark: the result is complete, with the exit code 0.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/build.rs", "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n}\n");
    write(repo, "crates/shop-core/rates.json", "{\"rate\": 1}\n");
    commit_all(repo, "a build script and a data file");
    write(repo, "crates/shop-core/rates.json", "{\"rate\": 2}\n");
    let out = run(repo, &["step", "change the rate", "--reason", "crates/shop-core/rates.json:=the rate is 2 now"], 0);
    assert!(out.contains("! build code changed in step 0: crates/shop-core/rates.json (the tree has a build script, and build code can read or run this file)."), "{out}");
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    run(repo, &["step", "rename a local"], 0);
    commit_all(repo, "the rate and a rename");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted · 2 steps") && out.contains("1 code equal (1 with the build code of step 0)") && out.contains(" · build code changed from step 0\n"), "{out}");
    let out = run(repo, &["status", "HEAD", "--verbose"], 0);
    assert!(out.contains("code equal] (with the build code of step 0)\n") && !out.contains("not checked"), "{out}");
    no_proof_without_mark("status HEAD", &out, 0);
    // The careful option gives the result of batch 5j: no check from step 0 on.
    let out = run(repo, &["status", "HEAD", "--wait", "600", "--no-build-code"], 1);
    assert!(out.contains("steps 0–1 not checked: build code changed in step 0 (crates/shop-core/rates.json"), "{out}");
}

#[test]
fn the_base_line_names_build_code_that_a_recent_commit_changed() {
    // Design 28.5 (rule 9). The base of a change set can have build code that a commit shortly
    // before it changed. The proofs of the change set ran that code, and they have no mark. The
    // header of each reviewer output names the commit and the files. No state is stored.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let head = |repo: &Path| git(repo, &["rev-parse", "--short=7", "HEAD"]).trim().to_string();
    let found = "! the base has build code that a recent commit changed: ";
    let end = "Each check of this change set ran this code. No mark shows this, because the change is before the base.";

    // The base is the first commit of the history: no line.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    run(repo, &["step", "rename a local"], 0);
    commit_all(repo, "rename a local");
    let out = run(repo, &["tag"], 0);
    assert!(!out.contains("base build code") && !out.contains(found), "{out}");

    // Commit A has no Semilla check: it adds a build script. Change set B changes the script.
    write(repo, "crates/shop-core/build.rs", "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n}\n");
    commit_all(repo, "add a build script with plain git");
    let a = head(repo);
    write(repo, "crates/shop-core/build.rs", "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n    println!(\"cargo:rustc-env=SHOP=1\");\n}\n");
    run(repo, &["step", "set a variable in the build script", "--reason", BUILD_REASON], 0);
    commit_all(repo, "set a variable");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains(&format!("{found}`{a}` (crates/shop-core/build.rs). {end}\n")) || out.ends_with(&format!("{found}`{a}` (crates/shop-core/build.rs). {end}")), "{out}");
    let b = head(repo);

    // Change set C changes other code. Its result is complete and has no mark, but each proof
    // ran the build script of A and B. Each reviewer output has the line near the base line.
    write(repo, "crates/shop-core/src/cart.rs", &CART_V1.replace("*x", "*item").replace("for x in", "for item in").replace("+= x", "+= item"));
    run(repo, &["step", "rename the loop variable"], 0);
    commit_all(repo, "rename the loop variable");
    let line = format!("{found}`{b}` (crates/shop-core/build.rs); `{a}` (crates/shop-core/build.rs). {end}");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains(&line), "{out}");
    let out = run(repo, &["status", "HEAD", "--wait", "0"], 0);
    assert!(out.contains(&format!("`: it has a Semilla check\n{line}\n")) && out.contains("1 fact: 1 code equal") && !out.contains("with the build code"), "{out}");
    let (md, html) = (dir.path().join("c.md"), dir.path().join("c.html"));
    run(repo, &["review", "HEAD", "--md", md.to_str().unwrap()], 0);
    run(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], 0);
    let (md_text, html_text) = (std::fs::read_to_string(&md).unwrap(), std::fs::read_to_string(&html).unwrap());
    let top = |text: &str, line: &str, summary: &str| text.find(line).is_some_and(|at| at < text.find(summary).unwrap_or(0));
    assert!(top(&md_text, &format!("> \\{line}"), "## Summary"), "{md_text}");
    assert!(top(&html_text, &format!("<p class=\"notice\">{line}</p>"), "<h2 id=\"summary\">"), "the HTML report has the line in its header");
    let ui = json_body(&ui_changeset(repo, &[]));
    assert!(ui["notices"].as_array().is_some_and(|notices| notices.iter().any(|notice| notice.as_str() == Some(line.as_str()))), "{ui}");
    let answers = mcp_calls(repo, &[("results", serde_json::json!({"commit": "HEAD"}))]);
    assert!(answers[0].0.contains(&line), "{}", answers[0].0);

    // The bound. Semilla examines 20 commits before the change set. With 18 commits after C,
    // the commit of B is in the bound and the commit of A is not: the line says so.
    for n in 0..18 {
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", &format!("empty {n}")]);
    }
    let cart = std::fs::read_to_string(repo.join("crates/shop-core/src/cart.rs")).unwrap();
    write(repo, "crates/shop-core/src/cart.rs", &cart.replace("sum", "acc"));
    run(repo, &["step", "rename the sum"], 0);
    commit_all(repo, "rename the sum");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains(&format!("{found}`{b}` (crates/shop-core/build.rs). {end} Semilla examined only the 20 commits before this change set.")), "{out}");
    // One more change set: the commit of B is not in the bound. The output says where the
    // examination stops.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "change the limit of the cart"], 0);
    commit_all(repo, "change the limit");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("base build code: no change in the 20 commits before this change set (Semilla examines no more commits)") && !out.contains(found), "{out}");
}

#[test]
fn a_file_of_a_host_package_is_build_code() {
    // Design 28, A4.5. The compiler runs the code of a proc macro crate during each build. A
    // change of a file of this crate gets the spotlight "build code" and needs a reason. The
    // step also has the line of the build code, and a reviewer command shows the change first.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let macros = |value: u32| format!("use proc_macro::TokenStream;\n\n#[proc_macro]\npub fn limit(_input: TokenStream) -> TokenStream {{\n    \"{value}\".parse().unwrap()\n}}\n");
    write(repo, "crates/shop-macros/Cargo.toml", "[package]\nname = \"shop-macros\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\nproc-macro = true\n");
    write(repo, "crates/shop-macros/src/lib.rs", &macros(10));
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nshop-macros = { path = \"../shop-macros\" }\n");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n\npub fn limit() -> u32 {\n    shop_macros::limit!()\n}\n");
    commit_all(repo, "a proc macro crate");

    // A change of plain code with a proc macro crate in the tree: no line about build code.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let out = run(repo, &["step", "rename a local"], 0);
    assert!(!out.contains("build code"), "{out}");

    // The proc macro gives another value. The text of `shop_core::limit` does not change.
    let file = "crates/shop-macros/src/lib.rs";
    write(repo, file, &macros(20));
    let out = run(repo, &["step", "change the limit"], 1);
    assert!(out.contains(&format!("{file}: changed")) && out.contains("build code: this file is a file of a host package") && out.contains("needs a reason"), "{out}");
    let reason = format!("{file}:=the limit is 20 now");
    let out = run(repo, &["step", "change the limit", "--reason", &reason], 0);
    assert!(out.contains(&format!("! build code changed in step 1: {file}. It runs in the sandbox during each check, and it can change each result from step 1 on.")), "{out}");
    assert!(out.contains("spotlight: build code: this file is a file of a host package") && out.contains("reason: \"the limit is 20 now\""), "{out}");

    // The report of a reviewer command shows the fact first, with the diff. The result is complete.
    run(repo, &["status", "--wait", "600"], 0);
    let md = dir.path().join("host.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains(&format!("\\! build code changed in step 1 ({file}). The check ran it in the sandbox.")), "{text}");
    // The section has the file fact and the code fact of the changed file.
    let section = text.find("#### Build code changes (2): read these changes first").unwrap_or_else(|| panic!("the section\n{text}"));
    assert!(section < text.find("step 0 ·").expect("the group of step 0"), "{text}");
    assert!(text.contains("spotlight: build code: this file is a file of a host package") && text.contains("\"20\".parse()"), "{text}");
    // With the careful option, the step has no check.
    run(repo, &["review", "--no-build-code", "--md", md.to_str().unwrap()], 1);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains(&format!("step 1 not checked: build code changed in step 1 ({file})")), "{text}");
}

#[test]
fn a_trailing_comment_is_a_comment_fact_not_a_doc_fact() {
    // Part B of batch 5j. A plain `//` comment at the end of an item line is not a doc. The fact
    // says "changed comment", class `other`. It needs no reason.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &format!("/// The cart.\n{CART_V0}"));
    commit_all(repo, "a doc comment");
    // Add a trailing comment on the struct line. The doc comment above does not change.
    let text = std::fs::read_to_string(repo.join("crates/shop-core/src/cart.rs")).unwrap();
    write(repo, "crates/shop-core/src/cart.rs", &text.replace("pub struct Cart {", "pub struct Cart { // the basket"));
    let out = run(repo, &["step", "note the cart"], 0);
    assert!(out.contains("added comment `// the basket` [docs · no code]"), "{out}");
    assert!(!out.contains("changed docs") && !out.contains("added docs") && !out.contains("removed docs") && !out.contains("needs a reason"), "{out}");
}

/// A build script for the folder tests of design 28.9. It puts the marker `partf-<name>` into
/// each folder above its output folder that it can write. If it finds the marker of a
/// different script there, it writes `partf-seen.txt` into its output folder.
fn folder_probe_script(name: &str) -> String {
    r#"fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut seen = Vec::new();
    for dir in out.ancestors().skip(1) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with("partf-") && file != "partf-NAME" {
                seen.push(file);
            }
        }
        let _ = std::fs::write(dir.join("partf-NAME"), "x");
    }
    if !seen.is_empty() {
        std::fs::write(out.join("partf-seen.txt"), seen.join(",")).unwrap();
    }
}
"#
    .replace("NAME", name)
}

/// The folder set (`shared/<key>` or `build-code/<key>`) of a path of the cache folder.
fn folder_set_of(cache: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(cache).unwrap().components().take(2).collect()
}

#[test]
fn the_build_code_cache_keeps_the_entries_of_the_stored_steps_and_the_four_newest() {
    // Batch 7c, part 6.3 (design 37.22). An entry of the `build-code` cache can have 500 MB.
    // After a step, an entry that no stored step uses stays only if it is one of the four
    // newest. `semilla cache --clean` removes each entry that no stored step uses.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    run(repo, &["step", "rename a local"], 0);
    let cache = repo_cache(repo);
    let entries = || {
        let mut names: Vec<String> = std::fs::read_dir(cache.join("build-code")).map(|list| list.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        names
    };
    // Six entries of old steps, each with 1000 bytes. The entry `old-6` is the newest.
    let start = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for number in 1..=6u64 {
        let file = cache.join(format!("build-code/old-{number}/target/data"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, vec![b'x'; 1000]).unwrap();
        let time = start + std::time::Duration::from_secs(number * 60);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(time).unwrap();
        for folder in [file.parent().unwrap(), file.parent().unwrap().parent().unwrap()] {
            std::fs::File::open(folder).unwrap().set_modified(time).unwrap();
        }
    }
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "change the limit of the cart"], 0);
    assert_eq!(entries(), ["old-3", "old-4", "old-5", "old-6"], "a step keeps the four newest entries that no stored step uses");
    let out = run(repo, &["cache", "--clean"], 0);
    assert_eq!(out, "removed 4 entries of the build code cache that no stored step uses: 4 kB freed\n");
    assert_eq!(entries(), Vec::<String>::new());
    assert_eq!(run(repo, &["cache", "--clean"], 0), "removed 0 entries of the build code cache that no stored step uses: 0 B freed\n");
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    // The entry of a stored step with changed build code stays, also with `--clean`.
    write(repo, "crates/shop-core/build.rs", &folder_probe_script("a"));
    run(repo, &["step", "add a build script", "--reason", BUILD_REASON], 0);
    let used = entries();
    assert_eq!(used.len(), 1, "{used:?}");
    std::fs::create_dir_all(cache.join("build-code/old-7")).unwrap();
    assert_eq!(run(repo, &["cache", "--clean"], 0), "removed 1 entry of the build code cache that no stored step uses: 0 B freed\n");
    assert_eq!(entries(), used);
    // The step is not stored after `undo`: its entry is not in use.
    run(repo, &["undo"], 0);
    let out = run(repo, &["cache", "--clean"], 0);
    assert!(out.starts_with("removed 1 entry of the build code cache that no stored step uses: ") && out.ends_with(" freed\n") && !out.contains(": 0 B"), "{out}");
    assert_eq!(entries(), Vec::<String>::new());
    // The removed entry is only a cache: the same step builds it again.
    run(repo, &["step", "add a build script", "--reason", BUILD_REASON], 0);
    assert_eq!(entries(), used);
}

#[test]
fn builds_share_a_folder_only_with_the_same_build_code() {
    // Design 28.9, rule 1. A build can write its target folder, and a file stays there. Two
    // builds share a folder set only if they run the same build code. The marker of one build
    // script is thus never in a folder that a different build script can read.
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    run(repo, &["step", "rename a local"], 0);
    let cache = repo_cache(repo);
    let sets = |kind: &str| std::fs::read_dir(cache.join(kind)).map(|list| list.flatten().count()).unwrap_or(0);
    let sets_of = |marker: &str| find_named(&cache, marker).iter().map(|path| folder_set_of(&cache, path)).collect::<std::collections::BTreeSet<_>>();
    let no_script_saw_a_marker = || assert_eq!(find_named(&cache, "partf-seen.txt"), Vec::<PathBuf>::new(), "a build script found the marker of a different build");

    // The marked folder sets. Step 1 adds script A. Step 2 changes other code: it has the
    // build code of step 1, and it uses the same folder set.
    assert_eq!((sets("shared"), sets("build-code")), (1, 0));
    write(repo, "crates/shop-core/build.rs", &folder_probe_script("a"));
    run(repo, &["step", "add a build script", "--reason", BUILD_REASON], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "change the limit of the cart"], 0);
    assert_eq!((sets("shared"), sets("build-code")), (1, 1), "the same build code uses one folder set");
    let set_a = sets_of("partf-a");
    assert_eq!(set_a.len(), 1, "{set_a:?}");
    // The agent removes the two steps and adds a different script B on the same base. The
    // build of B has its own folder set: it does not find a file of the build of A.
    run(repo, &["undo"], 0);
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/build.rs", &folder_probe_script("b"));
    run(repo, &["step", "add a different build script", "--reason", BUILD_REASON], 0);
    assert_eq!((sets("shared"), sets("build-code")), (1, 2), "different build code has a different folder set");
    let set_b = sets_of("partf-b");
    assert!(set_b.len() == 1 && set_b.is_disjoint(&set_a), "{set_a:?} {set_b:?}");
    no_script_saw_a_marker();

    // The shared folder sets. A base with script B, and then a base with script C: each base
    // has a different shared folder set, and it is not the marked folder set of B.
    commit_all(repo, "add a build script");
    run(repo, &["tag"], 0);
    write(repo, "crates/shop-core/src/cart.rs", &CART_V1.replace("*x", "*item").replace("for x in", "for item in").replace("+= x", "+= item"));
    run(repo, &["step", "rename the loop variable"], 0);
    assert_eq!((sets("shared"), sets("build-code")), (2, 2), "a base with different build code has a different shared folder set");
    let set_b = sets_of("partf-b");
    assert!(set_b.len() == 2 && set_b.is_disjoint(&set_a), "{set_b:?}");
    commit_all(repo, "rename the loop variable");
    run(repo, &["tag"], 0);
    write(repo, "crates/shop-core/build.rs", &folder_probe_script("c"));
    commit_all(repo, "change the build script with plain git");
    let cart = std::fs::read_to_string(repo.join("crates/shop-core/src/cart.rs")).unwrap();
    write(repo, "crates/shop-core/src/cart.rs", &cart.replace("sum", "acc"));
    run(repo, &["step", "rename the sum"], 0);
    assert_eq!((sets("shared"), sets("build-code")), (3, 2));
    let set_c = sets_of("partf-c");
    assert!(set_c.len() == 1 && set_c.is_disjoint(&set_b) && set_c.is_disjoint(&set_a), "{set_c:?} {set_b:?}");
    no_script_saw_a_marker();
    // A base that changes only the Rust code of a target has the same build code: the change
    // set on it uses the shared folder set of the base before it.
    commit_all(repo, "rename the sum");
    run(repo, &["tag"], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V0);
    run(repo, &["step", "change the limit of the cart back"], 0);
    assert_eq!((sets("shared"), sets("build-code")), (3, 2), "a base with the same build code uses the same shared folder set");
    no_script_saw_a_marker();
}

#[test]
fn the_records_cache_gets_only_the_records_of_the_units_of_the_build() {
    // Design 28.8, rule 2. A build can write its records folder. A build script puts two files
    // there: one with a plain name, and one with the name form of a record. Semilla copies no
    // one of them to the records cache, and the output names each one.
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let fake = "shop_core.lib.0123456789abcdef.jsonl";
    let script = "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n    let records = std::path::PathBuf::from(std::env::var(\"SEMILLA_MIR_OUT\").unwrap());\n    std::fs::write(records.join(\"planted.txt\"), \"x\").unwrap();\n    std::fs::write(records.join(\"FAKE\"), \"{}\\n\").unwrap();\n}\n".replace("FAKE", fake);
    write(repo, "crates/shop-core/build.rs", &script);
    let out = run(repo, &["step", "add a build script", "--reason", BUILD_REASON], 0);
    let line = format!("  ! the build left files in the records folder that are not a record of a build unit: `planted.txt`, `{fake}`. Semilla does not use them\n");
    assert!(out.contains(&line), "{out}");
    let cache = repo_cache(repo);
    assert_eq!(find_named(&cache, "planted.txt"), Vec::<PathBuf>::new());
    assert_eq!(find_named(&cache, fake), Vec::<PathBuf>::new());
    // The result of a later command uses the same records: it has the same line.
    let out = run(repo, &["status", "--wait", "600"], 0);
    assert!(out.contains(line.trim()), "{out}");
}

#[test]
fn a_target_with_no_test_harness_has_a_record() {
    // Design 28.8. A bench target with `harness = false` builds with no `--test`, so the driver
    // gives the kind `bin` to its record file. The file is a record of a unit of the build:
    // Semilla uses it, and the output has no line about a file that is not a record.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let manifest = std::fs::read_to_string(repo.join("crates/shop-core/Cargo.toml")).unwrap();
    write(repo, "crates/shop-core/Cargo.toml", &format!("{manifest}\n[[bench]]\nname = \"speed\"\nharness = false\n"));
    let bench = |local: &str| format!("fn sum(items: &[u32]) -> u32 {{\n    let {local} = items.iter().sum();\n    {local}\n}}\n\nfn main() {{\n    println!(\"{{}}\", sum(&[1, 2, 3]));\n}}\n");
    write(repo, "crates/shop-core/benches/speed.rs", &bench("t"));
    commit_all(repo, "add a bench target");
    write(repo, "crates/shop-core/benches/speed.rs", &bench("total"));
    let out = run(repo, &["step", "rename a local"], 0);
    assert!(out.contains("`shop_core (bench \"speed\")::sum`: renamed local `t` → `total`, 2 edit(s) [rename · code equal]"), "{out}");
    assert!(!out.contains("the build left"), "{out}");
}

#[test]
fn no_process_of_a_build_continues_after_the_build() {
    // Design 28.10, rule 3. A build script starts two programs and does not wait for them. One
    // of them tries to leave the process group. Semilla stops each process of the build at the
    // end of the build.
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let script = r#"use std::process::{Command, Stdio};
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = std::env::var("OUT_DIR").unwrap();
    let quiet = |command: &mut Command| command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok();
    let plain = quiet(Command::new("sleep").arg("86399"));
    let session = quiet(Command::new("setsid").args(["sleep", "86398"]));
    std::fs::write(format!("{out}/partf-started.txt"), format!("{plain} {session}")).unwrap();
}
"#;
    write(repo, "crates/shop-core/build.rs", script);
    run(repo, &["step", "add a build script", "--reason", BUILD_REASON], 0);
    run(repo, &["status", "--wait", "600"], 0);
    let started = find_named(&repo_cache(repo), "partf-started.txt");
    assert!(!started.is_empty(), "the build script ran");
    assert!(started.iter().all(|path| std::fs::read_to_string(path).unwrap().starts_with("true ")), "the build script started a program");
    let alive = || {
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let line = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
            let line = String::from_utf8_lossy(&line).replace('\0', " ");
            if line == "sleep 86399 " || line == "sleep 86398 " {
                found.push(line);
            }
        }
        found
    };
    let mut left = alive();
    for _ in 0..50 {
        if left.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        left = alive();
    }
    assert!(left.is_empty(), "a process of the build continues: {left:?}");
}

#[test]
fn a_build_gets_no_variable_no_program_and_no_file_of_the_agent() {
    // Design 27.11 and 27.12 (batch 5v, parts A and C). The build script of the base records what
    // it can get: a variable of the command, a program of a repository folder in `PATH`, and
    // an open of a path outside the allow list. It records only `true` or `false`.
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let script = r#"fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PROBE_ENV");
    let out = std::env::var("OUT_DIR").unwrap();
    let open = |path: &str| std::fs::File::open(path).is_ok() || std::fs::read_dir(path).is_ok();
    let lines = [
        format!("variable {}", std::env::var("PROBE_ENV").is_ok()),
        format!("program {}", std::process::Command::new("probe-tool").status().is_ok()),
        format!("path {}", std::env::var("PATH").unwrap_or_default().contains("REPO")),
        format!("ignored file {}", open("REPO/target/notes.txt")),
        format!("working tree {}", open("REPO/Cargo.toml")),
        format!("git folder {}", open("REPO/.git/config")),
        format!("home folder {}", open("HOME")),
        format!("cache folder {}", open("CACHE")),
        format!("temporary folder {}", open("/tmp")),
        format!("process folder {}", open("/proc/self/environ")),
    ];
    std::fs::write(format!("{out}/probe-ac.txt"), lines.join("\n")).unwrap();
}
"#
    .replace("REPO", repo.to_str().unwrap())
    .replace("HOME", &home)
    .replace("CACHE", cache_dir(repo).to_str().unwrap());
    write(repo, "crates/shop-core/build.rs", &script);
    commit_all(repo, "add a build script with plain git");
    write(repo, "target/notes.txt", "a file that git ignores\n");
    write(repo, "target/bin/probe-tool", "#!/bin/sh\nexit 0\n");
    std::fs::set_permissions(repo.join("target/bin/probe-tool"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", repo.join("target/bin").display(), std::env::var("PATH").unwrap());
    assert!(Command::new("probe-tool").env("PATH", &path).status().is_ok_and(|status| status.success()), "the program runs with this PATH outside the build");

    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let (code, out) = run_env(repo, &["step", "rename a local"], &[("PROBE_ENV", "1"), ("PATH", &path)]);
    assert!(code == 0 && out.contains("code equal]") && !out.contains("sandbox:"), "{out}");
    let probes = find_named(&repo_cache(repo), "probe-ac.txt");
    assert!(!probes.is_empty(), "the build script ran");
    for probe in probes {
        let text = std::fs::read_to_string(&probe).unwrap();
        assert_eq!(text.lines().count(), 10, "{text}");
        for line in text.lines() {
            assert!(line.ends_with(" false"), "the build got: {line}\n{text}");
        }
    }
}

#[test]
fn a_build_does_not_read_the_credentials_file_of_cargo() {
    // Design 27.12 (batch 5v, part C). The cargo home of this test is a new folder with a
    // credentials file. A build reads the registry folder of the cargo home. It does not read
    // the cargo home folder or the credentials file. The script records only `true` or `false`.
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let home = dir.path().join("cargo-home");
    std::fs::create_dir_all(home.join("registry")).unwrap();
    std::fs::write(home.join("credentials.toml"), "# a test file\n").unwrap();
    let script = r#"fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = std::env::var("OUT_DIR").unwrap();
    let open = |path: &str| std::fs::File::open(path).is_ok() || std::fs::read_dir(path).is_ok();
    let lines = [
        format!("registry {}", open("HOME/registry")),
        format!("credentials {}", open("HOME/credentials.toml")),
        format!("cargo home {}", std::fs::read_dir("HOME").is_ok()),
    ];
    std::fs::write(format!("{out}/probe-cred.txt"), lines.join("\n")).unwrap();
}
"#
    .replace("HOME", home.to_str().unwrap());
    write(repo, "crates/shop-core/build.rs", &script);
    commit_all(repo, "add a build script with plain git");
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let (code, out) = run_env(repo, &["step", "rename a local"], &[("CARGO_HOME", home.to_str().unwrap())]);
    assert!(code == 0 && out.contains("code equal]") && !out.contains("sandbox:"), "{out}");
    let probes = find_named(&repo_cache(repo), "probe-cred.txt");
    assert!(!probes.is_empty(), "the build script ran");
    for probe in probes {
        assert_eq!(std::fs::read_to_string(&probe).unwrap(), "registry true\ncredentials false\ncargo home false");
    }
}

#[test]
fn a_source_file_that_names_a_file_outside_the_build_folder_gets_a_clear_error() {
    // Design 27.12 (batch 5v, part C). The code includes a file by an absolute path. The build
    // cannot read the file: the error says that the file is outside the build folder.
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "target/notes.txt", "a file that git ignores\n");
    let notes = repo.join("target/notes.txt");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V1}\npub const NOTES: &str = include_str!({:?});\n", notes.to_str().unwrap()));
    let (code, out) = run_env(repo, &["step", "include the notes"], &[]);
    let line = format!("The file `{}` is outside the build folder. A check build reads only the files of the tree, the toolchain, and the cargo registry.", notes.display());
    assert!(out.contains(&line), "{code}\n{out}");
    assert!(!out.contains("a file that git ignores"), "{out}");
}

#[test]
fn a_registry_crate_with_a_build_script_and_a_proc_macro_builds_in_the_sandbox() {
    // Design 27.12 (batch 5v, part C). `serde` and `proc-macro2` have a build script, and
    // `serde_derive` is a proc macro. The build reads them from the registry of the cargo home.
    // The test needs these crates in the cargo cache of the machine: it uses no network.
    if semilla_normal::sandbox::missing().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let manifest = std::fs::read_to_string(repo.join("crates/shop-core/Cargo.toml")).unwrap();
    write(repo, "crates/shop-core/Cargo.toml", &format!("{manifest}\n[dependencies]\nserde = {{ version = \"1\", features = [\"derive\"] }}\n"));
    let receipt = |local: &str| format!("{CART_V0}\n#[derive(serde::Serialize)]\npub struct Receipt {{\n    pub total: u32,\n}}\n\npub fn receipt(cart: &Cart) -> Receipt {{\n    let {local} = cart.total(10);\n    Receipt {{ total: {local} }}\n}}\n");
    write(repo, "crates/shop-core/src/cart.rs", &receipt("t"));
    let lock = Command::new("cargo").current_dir(repo).args(["generate-lockfile", "--offline"]).output().unwrap();
    if !lock.status.success() {
        eprintln!("no test: the cargo cache of this machine does not have serde\n{}", String::from_utf8_lossy(&lock.stderr));
        return;
    }
    let text = std::fs::read_to_string(repo.join("Cargo.lock")).unwrap();
    assert!(text.contains("name = \"serde_derive\"") && text.contains("name = \"proc-macro2\""), "{text}");
    commit_all(repo, "use serde");
    write(repo, "crates/shop-core/src/cart.rs", &receipt("sum"));
    let out = run(repo, &["step", "rename a local"], 0);
    assert!(out.contains("`shop_core::cart::receipt`: renamed local `t` → `sum`, 2 edit(s) [rename · code equal]"), "{out}");
    assert!(!out.contains("sandbox:") && !out.contains("build failed"), "{out}");
    // The build made the proc macro: the target folder has its library.
    fn has_macro(dir: &Path) -> bool {
        std::fs::read_dir(dir).into_iter().flatten().flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() && !path.is_symlink() { has_macro(&path) } else { entry.file_name().to_string_lossy().starts_with("libserde_derive-") }
        })
    }
    assert!(has_macro(&repo_cache(repo)), "the build made the proc macro");
}

const FEES: &str = "pub mod cart;\n\npub const BASE_FEE: u64 = 25;\n\npub fn charge(amount: u64, rate: u64) -> u64 {\n    BASE_FEE + percent(amount, rate)\n}\n\npub fn percent(amount: u64, rate: u64) -> u64 {\n    amount * rate / 100\n}\n\npub fn scaled(amount: u64, rate: u64) -> u64 {\n    debug_assert!(rate <= 100);\n    amount * rate / 100\n}\n\npub fn withdraw(cents: u64, amount: u64) -> u64 {\n    let fee = charge(amount, 1);\n    let cost = amount + fee;\n    cents - cost\n}\n";

/// The functions of the test of the opaque rule (batch 5v, part B). One kind of value: a name,
/// the type, the macro call, the tests of the value that are true in the check build, and the
/// arms of a `match` (the first arm is the arm of the check build).
const MACRO_VALUES: [(&str, &str, &str, &[&str], &str); 9] = [
    ("oe", "Option<&'static str>", "option_env!(\"SEMILLA_T_X\")", &["V.is_none()", "!V.is_some()", "V.unwrap_or(\"x\").len() == 1", "if let Some(_) = V { false } else { true }"], "None => a + 1, Some(_) => a"),
    ("env", "&'static str", "env!(\"CARGO_MANIFEST_DIR\")", &["V.len() > 0"], "\"\" => a, _ => a + 1"),
    ("cfg", "bool", "cfg!(semilla_t_none)", &["!V"], "false => a + 1, true => a"),
    ("file", "&'static str", "file!()", &["V.len() > 0"], "\"\" => a, _ => a + 1"),
    ("line", "u32", "line!()", &["V > 0"], "0 => a, _ => a + 1"),
    ("column", "u32", "column!()", &["V > 0"], "0 => a, _ => a + 1"),
    ("module", "&'static str", "module_path!()", &["V.len() > 0"], "\"\" => a, _ => a + 1"),
    ("text", "&'static str", "include_str!(\"note.txt\")", &["V.len() == 4"], "\"\" => a, _ => a + 1"),
    ("bytes", "&'static [u8]", "include_bytes!(\"note.txt\")", &["V.len() == 4"], "0 => a, _ => a + 1"),
];

/// The text of the functions of [`MACRO_VALUES`], and their names. With `folded`, each body is
/// the value that the check build gives: `a + 1`.
fn macro_value_functions(folded: bool) -> (String, Vec<String>) {
    let mut text = String::from("#![allow(dead_code, unexpected_cfgs)]\n");
    let mut names = Vec::new();
    for (kind, ty, call, tests, arms) in MACRO_VALUES {
        // The value comes from the macro call, a constant, a static, or a function of the crate.
        for carrier in ["direct", "const", "static", "function"] {
            let mut forms: Vec<String> = tests.iter().map(|test| format!("if {test} {{ a + 1 }} else {{ a }}")).collect();
            // The type of the bytes is an array with a length: the `match` reads the length.
            forms.push(format!("match {} {{ {arms} }}", if kind == "bytes" { "V.len()" } else { "V" }));
            for (index, form) in forms.iter().enumerate() {
                let name = format!("{kind}_{carrier}_{index}");
                let value = match carrier {
                    "direct" => call.to_string(),
                    "const" => {
                        text.push_str(&format!("const C_{}: {ty} = {call};\n", name.to_uppercase()));
                        format!("C_{}", name.to_uppercase())
                    }
                    "static" => {
                        text.push_str(&format!("static S_{}: {ty} = {call};\n", name.to_uppercase()));
                        format!("S_{}", name.to_uppercase())
                    }
                    _ => {
                        text.push_str(&format!("fn v_{name}() -> {ty} {{ {call} }}\n"));
                        format!("v_{name}()")
                    }
                };
                let body = if folded { "a + 1".to_string() } else { form.replace('V', &value) };
                text.push_str(&format!("pub fn {name}(a: u64) -> u64 {{ {body} }}\n"));
                names.push(name);
            }
        }
    }
    // The control: a constant of the tree. The proof of this fold is correct.
    text.push_str("const OFF: bool = false;\n");
    text.push_str(&format!("pub fn control(a: u64) -> u64 {{ {} }}\n", if folded { "a + 1" } else { "if OFF { a } else { a + 1 }" }));
    (text, names)
}

#[test]
fn no_proof_uses_the_value_of_a_macro_that_the_tree_does_not_decide() {
    // Batch 5v, part B (defect 1). The check build gives one value for `option_env!`, `env!`,
    // `cfg!`, a source position, and the content of a file. A different build can give a
    // different value. Each function of the list becomes the value of the check build in one
    // step. The compare is at level N0 (design 35.4): Semilla folds no condition, so no fact is
    // `code equal`. A note names each value that Semilla did not check (design 35.5).
    let dir = setup();
    let repo = &dir.path().join("repo");
    let (before, names) = macro_value_functions(false);
    let (after, _) = macro_value_functions(true);
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod values;\n");
    write(repo, "crates/shop-core/src/note.txt", "note");
    write(repo, "crates/shop-core/src/values.rs", &before);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "values"]);
    write(repo, "crates/shop-core/src/values.rs", &after);
    // The variable of the command does not reach the build (part A): the result is the same.
    let output = semilla_env(repo, &["step", "fold"], &[("SEMILLA_T_X", "1")]);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    // Design 40.3: the deleted condition owns the changed elements, so it is never `code equal`.
    // A kept statement (`a + 1`) can have the same compiled code before and after.
    let equal_deletions = out.lines().filter(|line| line.contains("deleted statement") && line.contains("code equal]")).count();
    assert!(output.status.success() && equal_deletions == 0 && out.contains("[semantics · code different]"), "{out}\n{}", String::from_utf8_lossy(&output.stderr));
    for name in &names {
        assert!(out.contains(&format!("shop_core::values::{name}")), "the step shows `{name}`: {out}");
    }
    // The note asks for no reason. No spotlight shows.
    assert!(out.contains(CONFIG_NOTE), "{out}");
    assert!(out.contains("note: this body uses `option_env!(\"SEMILLA_T_X\")`, which Semilla did not check (`shop_core::values::oe_direct_4`)") && !out.contains("spotlight"), "{out}");
}

/// The note of a fact in a body that reads the build configuration (design 35.5). It asks for no
/// reason.
const CONFIG_NOTE: &str = "which Semilla did not check";

// rule+ R65.3-2
// rule- R65.3-2
#[test]
fn a_proof_holds_for_each_build_configuration() {
    // Batch 5k, A1 (trial 6, D1): a condition whose value is a constant of the check build is
    // not a constant of each build. The normal forms do not fold it.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", FEES);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "fees"]);
    let percent = "pub fn percent(amount: u64, rate: u64) -> u64 {\n";
    // One probe: the statement goes first in `percent`, the item goes to the end of the file.
    let probe = |statement: &str, item: &str, expect: i32| -> String {
        write(repo, "crates/shop-core/src/lib.rs", &format!("{}{item}", FEES.replace(percent, &format!("{percent}    {statement}\n"))));
        let out = run(repo, &["step", "probe"], expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };
    let step = |text: &str, args: &[&str], expect: i32| -> String {
        write(repo, "crates/shop-core/src/lib.rs", text);
        let out = run(repo, args, expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };

    // The table of the brief. A use of `cfg!` gets a note under the fact (design 35.5). The note
    // asks for no reason.
    let out = probe("if !cfg!(unix) { return 0; }", "", 0);
    assert!(out.contains("[semantics · code different]") && out.contains("note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::percent`)") && !out.contains("spotlight"), "{out}");
    for statement in ["if usize::BITS != 64 { return 0; }", "if usize::MAX as u64 != u64::MAX { return 0; }", "if core::mem::size_of::<usize>() != 8 { return 0; }", "if core::mem::align_of::<u64>() != 8 { return 0; }", "if (1u64 << 40) as usize == 0 { return 0; }", "if (u64::MAX as usize as u64) != u64::MAX { return 0; }"] {
        let out = probe(statement, "", 0);
        assert!(out.contains("added early exit") && out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{statement}: {out}");
    }
    let out = probe("if !CHECKED { return 0; }", "\nconst CHECKED: bool = cfg!(unix);\n", 0);
    assert!(out.contains("added early exit `if !CHECKED { return 0; }` at `shop_core::percent/S0` [semantics · code different]"), "{out}");
    assert!(out.contains("note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::CHECKED`)") && !out.contains("spotlight") && !out.contains("cfg attribute"), "{out}");
    let out = probe("if option_env!(\"LEDGER_FREE\").is_some() { return 0; }", "", 0);
    assert!(out.contains("[semantics · code different]") && out.contains("note: this body uses `option_env!(\"LEDGER_FREE\")`, which Semilla did not check (`shop_core::percent`)"), "{out}");
    // A plain constant of the workspace is equal in each build. The compare at level N0 folds no
    // condition: the fact is `code different`, with no note.
    let out = probe("if OFF { return 0; }", "\nconst OFF: bool = false;\n", 0);
    assert!(out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{out}");
    let out = probe("if u64::MAX == 0 { return 0; }", "", 0);
    assert!(out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{out}");

    // `cfg!` in an `if` expression (trial 6, attempt 10), with a feature, and in a macro of
    // the workspace.
    let out = step(&FEES.replace("let cost = amount + fee;", "let cost = if cfg!(target_os = \"windows\") { amount } else { amount + fee };"), &["step", "probe"], 0);
    assert!(out.contains("[semantics · code different]") && out.contains("note: this body uses `cfg!(target_os = \"windows\")`, which Semilla did not check (`shop_core::withdraw`)"), "{out}");
    let out = probe("if cfg!(feature = \"x\") { return 0; }", "", 0);
    assert!(out.contains("[semantics · code different]") && out.contains("note: this body uses `cfg!(feature = \"x\")`, which Semilla did not check (`shop_core::percent`)"), "{out}");
    let out = step(&FEES.replace(percent, &format!("macro_rules! free {{ () => {{ !cfg!(unix) }}; }}\n\n{percent}    if free!() {{ return 0; }}\n")), &["step", "probe"], 1);
    // The new macro has a `cfg` condition in its input: the spotlight "inactive code" asks for a reason.
    assert!(out.contains("added early exit `if free!() { return 0; }` at `shop_core::percent/S0` [semantics · code different]") && out.contains("note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::percent`)") && out.contains("spotlight: inactive code"), "{out}");

    // `cfg!` hidden in a rename step (attempt 21): the edit shows as `code different` with the
    // note. The claim `rename` is gone (design 35.3): the command stops with exit code 2.
    let hidden = FEES.replace("BASE_FEE", "FIXED_FEE").replace("    FIXED_FEE + percent", "    if !cfg!(unix) { return 0; }\n    FIXED_FEE + percent");
    let out = step(&hidden, &["step", "rename the fee"], 0);
    assert!(out.contains("added early exit `if !cfg!(unix) { return 0; }` at `shop_core::charge/S0` [semantics · code different]") && out.contains(CONFIG_NOTE), "{out}");
    // Batch 7b, part 1: the rename is a fact of its own. The edit fact has the changed body.
    assert!(fact_line(&out, "renamed `shop_core::BASE_FEE` → `shop_core::FIXED_FEE`").ends_with("[rename · code equal]") && out.contains("affected bodies: shop_core::charge"), "{out}");
    let out = step(&hidden, &["step", "rename the fee", "--claim", "rename shop_core::BASE_FEE to=FIXED_FEE"], 2);
    assert!(out.is_empty(), "{out}");

    // The honest cases stay `code equal`: a named constant for a literal, also in a body with
    // `debug_assert!`. A removed and an added `debug_assert!` show.
    let out = step(&format!("{}\nconst HUNDRED: u64 = 100;\n", FEES.replace("amount * rate / 100", "amount * rate / HUNDRED")), &["step", "name the literal"], 0);
    assert!(out.contains("code equal]") && !out.contains("code different]") && out.contains("in: shop_core::percent, shop_core::scaled") && !out.contains(CONFIG_NOTE), "{out}");
    let out = step(&FEES.replace("    debug_assert!(rate <= 100);\n", ""), &["step", "no assert"], 0);
    assert!(out.contains("debug_assert!(rate <= 100)") && out.contains("affected bodies: shop_core::scaled") && !out.contains("code equal") && !out.contains(CONFIG_NOTE), "{out}");
    let out = step(&FEES.replace("    debug_assert!(rate <= 100);\n", "    debug_assert!(rate <= 100);\n    debug_assert!(amount > 0);\n"), &["step", "one more assert"], 0);
    assert!(out.contains("debug_assert!(amount > 0)") && out.contains("affected bodies: shop_core::scaled") && !out.contains("code equal"), "{out}");

    // A statement with a `cfg` attribute, in the text and in a macro of the workspace: the MIR of
    // the check configuration does not have this code. The note names the attribute.
    let out = probe("#[cfg(not(unix))]\n    return 0;", "", 1);
    assert!(out.contains("[semantics · no compare]") && out.contains("note: this body uses `#[cfg(not(unix))]`, which Semilla did not check (`shop_core::percent`); no compare: no checked configuration compiles `shop_core::percent`"), "{out}");
    let out = step(&FEES.replace(percent, &format!("macro_rules! rel {{ ($e:expr) => {{ #[cfg(not(unix))] {{ $e }} }}; }}\n\n{percent}    rel!(return 0);\n")), &["step", "probe"], 1);
    assert!(out.contains("added macro statement `rel!(return 0);` at `shop_core::percent/S0` [semantics · code equal]") && out.contains("note: this body uses `rel! (#[cfg(not(unix))])`, which Semilla did not check (`shop_core::percent`)"), "{out}");

    // D4: a function with the name `cfg` has no `cfg` attribute.
    let out = step(&format!("{FEES}\npub fn cfg() -> u64 {{\n    7\n}}\n"), &["step", "add cfg"], 0);
    assert!(out.contains("added `shop_core::cfg`") && !out.contains("spotlight"), "{out}");
    let out = step(&format!("{FEES}\n#[cfg(unix)]\npub fn only_unix() -> u64 {{\n    7\n}}\n"), &["step", "add a function for unix"], 1);
    assert!(out.contains("spotlight: cfg attribute: shop_core::only_unix"), "{out}");

    // Batch 5l, B4: `if !(a)` and `return !(a)` are not calls of a macro `if!` or `return!`.
    let old_if = "if cost > cents { 0 } else { cents - cost }";
    let with_if = FEES.replace("    cents - cost\n", &format!("    {old_if}\n"));
    write(repo, "crates/shop-core/src/lib.rs", &with_if);
    commit_all(repo, "a condition");
    let out = step(&with_if.replace(old_if, "if !(cost > cents) { cents - cost } else { 0 }"), &["step", "probe"], 0);
    assert!(out.contains("code equal]") && !out.contains("code different]") && !out.contains(CONFIG_NOTE), "{out}");
    let out = step(&with_if.replace(old_if, "if cost > cents {\n        return !(u64::MAX);\n    }\n    cents - cost"), &["step", "probe"], 0);
    // The compare at level N0 does not fold `!(u64::MAX)`.
    assert!(out.contains("code different]") && !out.contains(CONFIG_NOTE), "{out}");
}

/// The items of the build configuration that the base of the test has (batch 5l, group 1).
const CONFIG_ITEMS: &str = "
#[cfg(unix)]
pub fn strict() -> bool {
    true
}
#[cfg(not(unix))]
pub fn strict() -> bool {
    false
}

pub fn wide() -> bool {
    #[cfg(target_pointer_width = \"64\")]
    {
        return true;
    }
    #[allow(unreachable_code)]
    false
}

pub static LEVEL: u32 = usize::BITS;

#[inline]
pub fn fast() -> bool {
    cfg!(unix)
}

pub fn helper() -> bool {
    true
}

pub fn guarded(amount: u64) -> u64 {
    if !strict() {
        return 0;
    }
    amount
}

pub struct Guard;
#[cfg(not(unix))]
impl Drop for Guard {
    fn drop(&mut self) {
        std::process::exit(1)
    }
}
pub struct Hold(pub Guard);
pub fn hold() -> Hold {
    Hold(Guard)
}
pub fn keep() -> u64 {
    let _h = hold();
    helper();
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn helper_is_true() {
        assert!(super::helper());
    }
}
";

#[test]
fn a_proof_does_not_look_through_an_item_of_the_build_configuration() {
    // Batch 5l, with the rule of design 35.5: a changed body that uses an item that the build
    // configuration selects or fills gets no proof (the compare is at level N0, design 35.4:
    // Semilla folds no condition). Batch 11b (R70.2-1, part c): the compiler resolves each such
    // use to the item that this configuration builds, so the check of this configuration has
    // it, and the fact gets no note "this body uses …, which Semilla did not check".
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = format!("{FEES}{CONFIG_ITEMS}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}\npub fn gate() -> u64 {{\n    1\n}}\n"));
    commit_all(repo, "the items of the build configuration");
    let percent = "pub fn percent(amount: u64, rate: u64) -> u64 {\n";
    let step = |text: &str, expect: i32| -> String {
        write(repo, "crates/shop-core/src/lib.rs", text);
        let out = run(repo, &["step", "probe"], expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };
    // One probe: the statement goes first in `percent`, the item goes to the end of the file.
    let probe = |statement: &str, item: &str, expect: i32| -> String { step(&format!("{}{item}", base.replace(percent, &format!("{percent}    {statement}\n"))), expect) };
    let uses = |out: &str, text: &str| out.contains("[semantics · code different]") && !out.contains(&format!("note: this body uses {text}")) && !out.contains(&format!(", {text}"));

    // Group 1: the item is in the base and has no change.
    let out = probe("if !strict() { return 0; }", "", 0);
    assert!(uses(&out, "`strict` (2 definitions, one under `#[cfg("), "{out}");
    let out = probe("if !wide() { return 0; }", "", 0);
    assert!(uses(&out, "`wide` (`#[cfg(target_pointer_width=\"64\")]`)"), "{out}");
    let out = probe("if LEVEL != 64 { return 0; }", "", 0);
    assert!(uses(&out, "static `LEVEL` (`usize::BITS`)"), "{out}");

    // Group 2: the step adds the item and the statement.
    let out = probe("if !QUICK { return 0; }", "\nstatic QUICK: bool = cfg!(unix);\n", 0);
    assert!(uses(&out, "static `QUICK` (`cfg!(unix)`)"), "{out}");
    let out = probe("if WIDE != 64 { return 0; }", "\nstatic WIDE: u32 = usize::BITS;\n", 0);
    assert!(uses(&out, "static `WIDE` (`usize::BITS`)"), "{out}");
    let out = probe("if !is_quick() { return 0; }", "\nstatic QK: bool = cfg!(unix);\nfn is_quick() -> bool {\n    QK\n}\n", 0);
    // The `cfg!` of the new static stays a use of its own body; the read of `QK` in
    // `is_quick` resolves to the static that this configuration builds (R70.2-1, part c).
    assert!(out.contains("added early exit `if !is_quick() { return 0; }`") && out.contains("note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::QK`)") && !out.contains("static `QK`"), "{out}");
    let out = probe("if !exact() { return 0; }", "\n#[cfg(unix)]\nfn exact() -> bool {\n    true\n}\n#[cfg(not(unix))]\nfn exact() -> bool {\n    false\n}\n", 1);
    assert!(uses(&out, "`exact` (2 definitions, one under `#[cfg("), "{out}");
    let out = probe("if W::MAX as u64 != u64::MAX { return 0; }", "\n#[cfg(unix)]\ntype W = u64;\n#[cfg(not(unix))]\ntype W = u32;\n", 1);
    assert!(uses(&out, "`W` (2 definitions, one under `#[cfg("), "{out}");
    let out = probe("if !m::q() { return 0; }", "\n#[cfg(unix)]\nmod m {\n    pub fn q() -> bool {\n        true\n    }\n}\n#[cfg(not(unix))]\nmod m {\n    pub fn q() -> bool {\n        false\n    }\n}\n", 1);
    assert!(uses(&out, "`m` (2 definitions, one under `#[cfg("), "{out}");

    // More forms: a method of an `impl` under `cfg`, and an item that a macro with `cfg` makes.
    let out = probe("if !amount.lim() { return 0; }", "\ntrait Lim {\n    fn lim(&self) -> bool;\n}\n#[cfg(unix)]\nimpl Lim for u64 {\n    fn lim(&self) -> bool {\n        true\n    }\n}\n#[cfg(not(unix))]\nimpl Lim for u64 {\n    fn lim(&self) -> bool {\n        false\n    }\n}\n", 1);
    assert!(uses(&out, "`lim` (an `impl` under `#[cfg(unix)]`)"), "{out}");
    let out = probe("if !made() { return 0; }", "\nmacro_rules! twice {\n    () => {\n        #[cfg(unix)]\n        fn made() -> bool {\n            true\n        }\n        #[cfg(not(unix))]\n        fn made() -> bool {\n            false\n        }\n    };\n}\ntwice!();\n", 1);
    assert!(uses(&out, "`made` (a macro with `cfg` makes it)"), "{out}");

    // A type with a drop that the configuration selects: the MIR is the same in each checked
    // configuration, so the result is `code equal`. The note names each type and its condition.
    let out = step(&base.replace("let _h = hold();", "let _ = hold();"), 0);
    // The pattern `_` is not a local: the fact is not a rename.
    // R70.2-1 (c): the `impl` of `Guard` under `#[cfg(not(unix))]` is code that this
    // configuration does not build, so the use of `Guard` stays and says so; `Hold` has no such
    // part of its own.
    assert!(out.contains("changed pattern `_h` → `_` at `shop_core::keep/S0/pat` [semantics · code equal]") && out.contains("note: this body uses `shop_core::Guard` (a type with an `impl` under `#[cfg(not(unix))]`; the configuration does not build a part of it), which Semilla did not check (`shop_core::keep`)") && !out.contains("renamed local"), "{out}");

    // A use in code with no change gets no note.
    let out = step(&base.replace("    helper();\n    1\n", "    helper();\n    if u64::MAX == 0 {\n        return 0;\n    }\n    1\n"), 0);
    assert!(out.contains("at `shop_core::keep/") && out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{out}");
    // Batch 5o (A3): a call of a function under `cfg` with no change gives no note for an edit in
    // a different part of the body.
    let out = step(&base.replace("    if !strict() {\n        return 0;\n    }\n    amount\n", "    if !strict() {\n        return 0;\n    }\n    if u64::MAX == 0 {\n        return 0;\n    }\n    amount\n"), 0);
    assert!(out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{out}");
    // The edit changes the place of the call.
    let out = step(&base.replace("    if !strict() {\n        return 0;\n    }\n    amount\n", "    if amount == 0 {\n        return 0;\n    }\n    if !strict() {\n        return 0;\n    }\n    amount\n"), 0);
    assert!(out.contains("shop_core::guarded"), "{out}");

    // A function of a different crate with `cfg!`: no form looks into it.
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}\npub fn gate() -> u64 {{\n    if !shop_core::fast() {{\n        return 0;\n    }}\n    1\n}}\n"));
    let out = run(repo, &["step", "probe"], 0);
    run(repo, &["undo"], 0);
    assert!(out.contains("added early exit") && out.contains("[semantics · code different]"), "{out}");

    // A type alias of a crate outside the workspace: the configuration can select its target, and
    // the MIR shows only the target. An alias of the allow list is free.
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}\npub fn gate() -> u64 {{\n    1\n}}\n"));
    let out = probe("if std::ffi::c_long::MAX as i64 != i64::MAX { return 0; }", "", 0);
    assert!(uses(&out, "`core::ffi::primitives::c_long` (a type alias of a different crate)"), "{out}");
    let out = probe("if big() { return 0; }", "\nfn big() -> bool {\n    std::ffi::c_long::MAX as i64 != i64::MAX\n}\n", 0);
    assert!(out.contains("added early exit `if big() { return 0; }`"), "{out}");
    let out = probe("if KL != i64::MAX { return 0; }", "\nconst KL: i64 = std::ffi::c_long::MAX;\n", 0);
    assert!(out.contains("added early exit `if KL != i64::MAX { return 0; }`"), "{out}");
    let with_alias = base.replace(percent, &format!("{percent}    let seen: std::io::Result<u64> = Ok(amount);\n    if seen.is_err() {{\n        return 1;\n    }}\n"));
    write(repo, "crates/shop-core/src/lib.rs", &with_alias);
    commit_all(repo, "an alias of the allow list");
    let out = step(&with_alias.replace(percent, &format!("{percent}    if u64::MAX == 0 {{ return 0; }}\n")), 0);
    assert!(out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{out}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the base again");

    // The default of a `const` parameter is a constant that the body does not show.
    let with_default = format!("{}\npub struct Bits<const N: usize = {{ usize::BITS as usize }}>;\nimpl<const N: usize> Bits<N> {{\n    pub fn get() -> u64 {{\n        N as u64\n    }}\n}}\n", base.replace(percent, &format!("{percent}    if <Bits<64>>::get() != 64 {{ return 0; }}\n")));
    write(repo, "crates/shop-core/src/lib.rs", &with_default);
    commit_all(repo, "a default of a const parameter");
    let out = step(&with_default.replace("<Bits<64>>::get()", "<Bits>::get()"), 0);
    assert!(out.contains("`<Bits<64>>::get` → `<Bits>::get`"), "{out}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the base again");

    // The cases with no note: a static with a literal, and a call of a function with no
    // configuration. The module `tests` under `#[cfg(test)]` does not make `helper` opaque.
    let out = probe("if OFF { return 0; }", "\nstatic OFF: bool = false;\n", 0);
    assert!(out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{out}");
    let out = probe("if !helper() { return 0; }", "", 0);
    assert!(out.contains("[semantics · code different]") && !out.contains(CONFIG_NOTE), "{out}");

    // Batch 5l, P10: a variant under `cfg` changes the set of the values of the type. The check
    // of the variant is not dead code, with `unix` and with a feature.
    for attr in ["#[cfg(not(unix))]", "#[cfg(feature = \"check\")]"] {
        let check = "    assert!(matches!(mode, Mode::Plain), \"no strict mode here\");\n";
        let with_mode = format!("{base}\npub enum Mode {{\n    Plain,\n    {attr}\n    Strict,\n}}\npub fn fee_for(mode: Mode, amount: u64) -> u64 {{\n{check}    percent(amount, 1)\n}}\n");
        write(repo, "crates/shop-core/src/lib.rs", &with_mode);
        commit_all(repo, "a variant under cfg");
        let out = step(&with_mode.replace(check, ""), 0);
        assert!(out.contains("deleted statement `assert!(matches!(mode, Mode::Plain)") && out.contains("at `shop_core::fee_for/S0` [semantics · code different]"), "{attr}: {out}");
    }
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the base again");

    // Batch 5l, B3: the body of an associated constant of a trait impl has a MIR name. The note
    // names this body. No message asks for a reason. The constant is an item (design 56.2),
    // thus the note has the path of the item.
    let with_const = format!("{}\npub trait K {{\n    const Q: bool;\n}}\nimpl K for u64 {{\n    const Q: bool = cfg!(unix);\n}}\n", base.replace(percent, &format!("{percent}    if !<u64 as K>::Q {{ return 0; }}\n")));
    write(repo, "crates/shop-core/src/lib.rs", &with_const);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::<u64 as K>::Q`)"), "{out}");
    assert!(!out.contains("spotlight") && !out.contains("run again"), "{out}");
}

#[test]
fn a_name_that_a_macro_puts_into_a_body_is_a_use_of_the_build_configuration() {
    // Batch 5p: the name rule reads the names of the expanded code. The text of a body with
    // a macro call does not have the names that the macro gives.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let off = "#[cfg(not(unix))]";
    let items = format!(
        "\nmod glob {{\n    pub fn lim() -> u64 {{\n        1\n    }}\n    pub const X: u64 = 1;\n}}\nuse glob::*;\n{off}\nfn lim() -> u64 {{\n    2\n}}\n{off}\nconst X: u64 = 2;\n\
         macro_rules! call {{\n    () => {{\n        lim()\n    }};\n}}\nmacro_rules! x {{\n    () => {{\n        X\n    }};\n}}\nmacro_rules! two {{\n    () => {{\n        2u64\n    }};\n}}\n\
         const BY_VALUE: u64 = X;\n\
         use std::fmt::Write as _;\npub struct Sink(pub String);\nimpl std::fmt::Write for Sink {{\n    fn write_str(&mut self, s: &str) -> std::fmt::Result {{\n        self.0.push_str(s);\n        Ok(())\n    }}\n}}\n\
         {off}\nimpl Sink {{\n    pub fn write_fmt(&mut self, _a: std::fmt::Arguments<'_>) -> std::fmt::Result {{\n        Ok(())\n    }}\n}}\n"
    );
    let bodies = [
        "pub fn by_call() -> u64 {\n    glob::lim()\n}\n",
        "pub fn by_const(a: u64) -> u64 {\n    if x!() != 1 {\n        return 0;\n    }\n    a\n}\n",
        "pub fn by_value(a: u64) -> u64 {\n    if BY_VALUE != 1 {\n        return 0;\n    }\n    a\n}\n",
        "pub fn by_write(w: &mut Sink) -> std::fmt::Result {\n    std::fmt::Write::write_fmt(w, format_args!(\"x\"))\n}\n",
        "pub fn honest(a: u64) -> u64 {\n    let v = two!() + a;\n    v\n}\n",
    ];
    let base = format!("{FEES}{items}{}", bodies.concat());
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the macros and the items under cfg");
    let step = |old: &str, new: &str, expect: i32| -> String {
        assert!(base.contains(old), "{old}");
        write(repo, "crates/shop-core/src/lib.rs", &base.replace(old, new));
        let out = run(repo, &["step", "probe"], expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };
    // A new macro call that gives a function with a second definition under `cfg`. The
    // release build calls a different function. The note names it (design 35.5).
    let out = step("    glob::lim()\n", "    call!()\n", 0);
    // R70.2-1 (c): the driver gives the name that the macro makes, and the record has the
    // `lim` that this configuration builds (the one under `not(unix)` is in no build here): no
    // note.
    assert!(out.contains("[semantics · code equal]") && !out.contains("this body uses `lim`"), "{out}");
    // The edit removes a macro call that gives a constant under `cfg`.
    let out = step("    if x!() != 1 {\n        return 0;\n    }\n", "", 0);
    assert!(out.contains("deleted statement `if x!() != 1 { return 0; }`"), "{out}");
    // The value of a constant uses a name under `cfg`. No macro.
    let out = step("    if BY_VALUE != 1 {\n        return 0;\n    }\n", "", 0);
    assert!(out.contains("deleted statement `if BY_VALUE != 1 { return 0; }`"), "{out}");
    // `write!` gives a method call `write_fmt`. An inherent method under `cfg` gets the call
    // in the release build.
    let out = step("    std::fmt::Write::write_fmt(w, format_args!(\"x\"))\n", "    write!(w, \"x\")\n", 0);
    // R70.2-1 (c): the compiler resolves `write_fmt` to the method that this configuration
    // builds (the inherent method under `not(unix)` is in no build here): no note.
    assert!(out.contains("[semantics · code equal]") && !out.contains("this body uses `write_fmt`"), "{out}");
    // The control: a rewrite in a body with a macro that gives no such name has no note. The
    // compare at level N0 sees the removed local.
    let out = step("    let v = two!() + a;\n    v\n", "    two!() + a\n", 0);
    assert!(out.contains("[move · code different]") && !out.contains(CONFIG_NOTE), "{out}");
}

#[test]
fn a_trait_fact_of_a_different_configuration_can_change_a_call() {
    // Batch 5q, with the rule of design 35.5: the rule of the rival impl gives no spotlight, no
    // note, and no change of the compare result. Each case shows its compare result in the
    // checked configurations.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let items = "\nmod tr {\n    pub trait Sup {\n        fn dig(self) -> u64;\n    }\n    pub trait Mark {}\n    impl<X: Mark> Sup for X {\n        fn dig(self) -> u64 {\n            2\n        }\n    }\n    pub trait Other {\n        fn dig(&self) -> u64;\n    }\n\
         \x20   pub trait Low {\n        fn low(self) -> u64;\n    }\n    pub trait Flag {}\n    impl<X: Flag> Low for X {\n        fn low(self) -> u64 {\n            1\n        }\n    }\n    pub trait Base {\n        fn low(&self) -> u64;\n    }\n}\n\
         use tr::{Base as _, Low as _, Other as _, Sup as _};\n\
         pub struct Coin(pub u64);\nimpl tr::Other for Coin {\n    fn dig(&self) -> u64 {\n        1\n    }\n}\n#[cfg(not(unix))]\nimpl tr::Mark for Coin {}\n\
         pub struct Bill(pub u64);\nimpl tr::Base for Bill {\n    fn low(&self) -> u64 {\n        2\n    }\n}\n#[cfg(unix)]\nimpl tr::Flag for Bill {}\n";
    let bodies = [
        "pub fn by_rival(t: Coin) -> u64 {\n    tr::Other::dig(&t)\n}\n",
        "pub fn by_fact(t: Bill) -> u64 {\n    let _ = &t;\n    1\n}\n",
        "pub fn honest(v: &[u64], a: u64) -> u64 {\n    let n = v.iter().map(|y| y + 1).sum::<u64>();\n    n + a\n}\n",
        "pub fn nested(a: u64) -> u64 {\n    fn first<I: Iterator>(mut i: I) -> Option<I::Item> {\n        i.next()\n    }\n    let v = first([a].into_iter()).unwrap_or(0);\n    v\n}\n",
    ];
    let base = format!("{FEES}{items}{}", bodies.concat());
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the traits and the facts under cfg");
    let step = |old: &str, new: &str, expect: i32| -> String {
        assert!(base.contains(old), "{old}");
        write(repo, "crates/shop-core/src/lib.rs", &base.replace(old, new));
        let out = run(repo, &["step", "probe"], expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };
    // The debug build has no `Coin: Mark`: the call means `Other::dig`. The release build
    // has it: the method with `self` by value wins.
    let out = step("    tr::Other::dig(&t)\n", "    t.dig()\n", 0);
    assert!(out.contains("[semantics · code equal]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // The debug build has `Bill: Flag`: the call means `Low::low`. The release build does not
    // have it: the call means `Base::low`. The winner uses a fact that can change.
    let out = step("    let _ = &t;\n    1\n", "    t.low()\n", 0);
    // Design 40.5: the category comes from the syntax (a call), not from the compiled code.
    assert!(out.contains("added statement `t.low()` at `shop_core::by_fact/tail` [semantics · code different]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // The control: the calls of an honest rewrite have no rival with such a fact. The step
    // inlines a variable (design 41.5): one `move` fact. Since fix round 1 of design 46, rule 3c
    // forwards the copy of `n` into both reads of the overflow check: equal code.
    let out = step("    let n = v.iter().map(|y| y + 1).sum::<u64>();\n    n + a\n", "    v.iter().map(|y| y + 1).sum::<u64>() + a\n", 0);
    assert!(out.contains("[move · code equal]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // A path in the signature of a nested item (`I::Item`) has no type check result in the
    // body around it. The driver must not ask for one.
    let out = step("    let v = first([a].into_iter()).unwrap_or(0);\n    v\n", "    first([a].into_iter()).unwrap_or(0)\n", 0);
    assert!(out.contains("[move · code different]") && !out.contains("compiler error"), "{out}");
    // The unstable rule. The compiler puts an unstable candidate after each stable one, if
    // the crate does not have the feature. A `cfg_attr` at the crate root can select it.
    let emp = "pub trait Emp {\n    fn is_empty(&mut self) -> bool;\n}\nimpl Emp for std::slice::Iter<'_, u64> {\n    fn is_empty(&mut self) -> bool {\n        false\n    }\n}\n";
    let unstable = |attr: &str, first: &str, old: &str, new: &str, expect: i32| -> String {
        let dir = setup();
        let repo = &dir.path().join("repo");
        let base = format!(
            "{attr}{FEES}{emp}#[allow(unused_mut)]\npub fn pick(mut it: std::slice::Iter<'_, u64>) -> bool {{\n    {first}\n}}\npub fn array(a: u64) -> bool {{\n    let e = [a].is_empty();\n    e\n}}\n"
        );
        assert!(base.contains(old), "{old}");
        write(repo, "crates/shop-core/src/lib.rs", &base);
        commit_all(repo, "an unstable method with the name of a stable one");
        write(repo, "crates/shop-core/src/lib.rs", &base.replace(old, new));
        run(repo, &["step", "probe"], expect)
    };
    // The winner is unstable: a build with no feature gives the call to `Emp::is_empty`.
    let out = unstable("#![cfg_attr(unix, feature(exact_size_is_empty))]\n", "ExactSizeIterator::is_empty(&it)", "ExactSizeIterator::is_empty(&it)", "it.is_empty()", 0);
    assert!(out.contains("[semantics · code equal]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // The unstable candidate holds now: a build with the feature gives the call to it.
    let out = unstable("#![cfg_attr(not(unix), feature(exact_size_is_empty))]\n", "Emp::is_empty(&mut it)", "Emp::is_empty(&mut it)", "it.is_empty()", 0);
    assert!(out.contains("[semantics · code equal]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // The control: the unstable candidate does not hold for an array, and no `impl` of the
    // workspace can make it hold. The call is not an input. The crate has no attribute
    // under `cfg` here: such an attribute at the root makes each `impl` of the root a fact
    // that can change.
    let out = unstable("", "Emp::is_empty(&mut it)", "    let e = [a].is_empty();\n    e\n", "    [a].is_empty()\n", 0);
    assert!(out.contains("[move · code different]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // The orphan rule. A glob `use` under `cfg` is a coarse case: each trait fact of the
    // workspace can change. Only the standard library can have `impl Iterator for usize`.
    // Thus the rival `Iterator::cmp` cannot win for `usize`. It can win for a local type.
    // The glob is in a different module: a glob in the module of the body stops the proof.
    let orphan = |old: &str, new: &str, expect: i32| -> String {
        let dir = setup();
        let repo = &dir.path().join("repo");
        let base = format!(
            "{FEES}pub mod pick {{\n    pub mod inner {{\n        pub fn one() -> u64 {{\n            1\n        }}\n    }}\n    #[cfg(unix)]\n    pub use inner::*;\n}}\n\
             #[derive(PartialEq, Eq, PartialOrd, Ord)]\npub struct Tok(pub u64);\n\
             pub fn plain(a: usize, b: usize) -> std::cmp::Ordering {{\n    let o = a.cmp(&b);\n    o\n}}\n\
             pub fn local(a: Tok, b: Tok) -> std::cmp::Ordering {{\n    Ord::cmp(&a, &b)\n}}\n"
        );
        assert!(base.contains(old), "{old}");
        write(repo, "crates/shop-core/src/lib.rs", &base);
        commit_all(repo, "a glob use under cfg");
        write(repo, "crates/shop-core/src/lib.rs", &base.replace(old, new));
        run(repo, &["step", "probe"], expect)
    };
    let out = orphan("    let o = a.cmp(&b);\n    o\n", "    a.cmp(&b)\n", 0);
    assert!(out.contains("[move · code different]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    let out = orphan("    Ord::cmp(&a, &b)\n", "    a.cmp(&b)\n", 0);
    assert!(out.contains("[semantics · code equal]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
}

#[test]
fn a_type_of_a_crate_outside_the_workspace_is_opaque() {
    // Batch 5l, P10: Semilla does not read the source of a crate outside the workspace. A feature
    // of that crate can add a variant or a `Drop` impl. A body that uses its type gets no proof.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\nexclude = [\"vendor\"]\n");
    write(repo, "vendor/shop-dep/Cargo.toml", "[package]\nname = \"shop-dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ncheck = []\n");
    let dep = "pub enum Mode {\n    Plain,\n    #[cfg(feature = \"check\")]\n    Strict,\n}\npub struct Loud;\n#[cfg(feature = \"check\")]\nimpl Drop for Loud {\n    fn drop(&mut self) {\n        std::process::exit(1)\n    }\n}\npub fn loud() -> Loud {\n    Loud\n}\npub fn next(n: u64) -> u64 {\n    n + 1\n}\n";
    write(repo, "vendor/shop-dep/src/lib.rs", dep);
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nshop-dep = { path = \"../../vendor/shop-dep\" }\n");
    let check = "    assert!(matches!(mode, shop_dep::Mode::Plain), \"no strict mode here\");\n";
    let hold = "    let _h = shop_dep::loud();\n";
    let far = "    let v = shop_dep::next(n);\n    return v + 1;\n";
    let base = format!("{FEES}\npub fn fee_for(mode: shop_dep::Mode, amount: u64) -> u64 {{\n{check}    percent(amount, 1)\n}}\npub fn hold(amount: u64) -> u64 {{\n{hold}    percent(amount, 1)\n}}\npub fn far(n: u64) -> u64 {{\n{far}}}\n");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "a crate outside the workspace");
    let step = |text: &str| -> String {
        write(repo, "crates/shop-core/src/lib.rs", text);
        let out = run(repo, &["step", "probe"], 0);
        run(repo, &["undo"], 0);
        out
    };
    // The check of the variant is not dead code: the feature `check` adds the variant.
    let out = step(&base.replace(check, ""));
    assert!(out.contains("deleted statement `assert!(matches!(mode, shop_dep::Mode::Plain)"), "{out}");
    // The value has no drop code in this build. The feature `check` adds a `Drop` impl. The
    // result is `code equal` for the checked configurations. Batch 11b (R70.2-1, part c): the
    // compiler resolves `Loud` to the type that this build has, so the fact has no note.
    let out = step(&base.replace(hold, "    let _ = shop_dep::loud();\n"));
    assert!(out.contains("changed pattern `_h` → `_` at `shop_core::hold/S0/pat` [semantics · code equal]") && !out.contains("this body uses `shop_dep::Loud`") && !out.contains("renamed local"), "{out}");
    // A body with a call of that crate and with no type of it keeps its proof.
    let out = step(&base.replace(far, "    let v = shop_dep::next(n);\n    v + 1\n"));
    assert!(out.contains("code equal]") && !out.contains("code different]"), "{out}");
}

#[test]
fn a_trait_impl_of_a_crate_outside_the_workspace_can_change_a_call() {
    // Batch 5q: Semilla does not read the source of a crate outside the workspace. A feature
    // of that crate can add or remove an `impl`. The rule of the rival impl gives no output
    // (design 35.5): each case shows its compare result in the checked configurations.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\nexclude = [\"vendor\"]\n");
    write(repo, "vendor/shop-dep/Cargo.toml", "[package]\nname = \"shop-dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ncheck = []\n");
    let dep = "pub trait Loud {\n    fn size(&self) -> u64;\n}\n#[cfg(not(feature = \"check\"))]\nimpl Loud for u64 {\n    fn size(&self) -> u64 {\n        8\n    }\n}\npub trait Soft {\n    fn deep(&self) -> u64;\n}\n#[cfg(feature = \"check\")]\nimpl Soft for u32 {\n    fn deep(&self) -> u64 {\n        4\n    }\n}\n";
    write(repo, "vendor/shop-dep/src/lib.rs", dep);
    write(
        repo,
        "crates/shop-core/Cargo.toml",
        "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nshop-dep = { path = \"../../vendor/shop-dep\" }\n\n[features]\ncheck = [\"shop-dep/check\"]\n",
    );
    let items = "use shop_dep::{Loud as _, Soft as _};\npub trait Quiet {\n    fn size(&mut self) -> u64;\n    fn deep(&mut self) -> u64;\n    fn calm(&mut self) -> u64;\n}\n\
         impl Quiet for u64 {\n    fn size(&mut self) -> u64 {\n        1\n    }\n    fn deep(&mut self) -> u64 {\n        1\n    }\n    fn calm(&mut self) -> u64 {\n        1\n    }\n}\n\
         impl Quiet for u32 {\n    fn size(&mut self) -> u64 {\n        1\n    }\n    fn deep(&mut self) -> u64 {\n        1\n    }\n    fn calm(&mut self) -> u64 {\n        1\n    }\n}\n";
    let bodies = "#[allow(unused_mut)]\npub fn by_fact(mut n: u64) -> u64 {\n    shop_dep::Loud::size(&n)\n}\npub fn by_rival(mut n: u32) -> u64 {\n    Quiet::deep(&mut n)\n}\npub fn honest(mut n: u64) -> u64 {\n    let v = n.calm();\n    v\n}\n";
    let base = format!("{FEES}{items}{bodies}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "a crate outside the workspace with traits");
    let step = |old: &str, new: &str, expect: i32| -> String {
        assert!(base.contains(old), "{old}");
        write(repo, "crates/shop-core/src/lib.rs", &base.replace(old, new));
        let out = run(repo, &["step", "probe"], expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };
    // The call means `Loud::size` by an `impl` of the other crate. The feature `check`
    // removes that `impl`: then the call means `Quiet::size`.
    let out = step("    shop_dep::Loud::size(&n)\n", "    n.size()\n", 0);
    assert!(out.contains("[semantics · code equal]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // The call means `Quiet::deep`. The feature `check` adds `impl Soft for u32`: then the
    // call means `Soft::deep`.
    let out = step("    Quiet::deep(&mut n)\n", "    n.deep()\n", 0);
    assert!(out.contains("[semantics · code equal]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
    // The control: no trait of the other crate has the name `calm`. The step inlines a variable
    // (design 41.5).
    let out = step("    let v = n.calm();\n    v\n", "    n.calm()\n", 0);
    assert!(out.contains("[move · code different]") && !out.contains(CONFIG_NOTE) && !out.contains("spotlight") && !out.contains("the call means"), "{out}");
}

/// The intent of cheat B (trial 6): a line break, a false result line, and the ESC sequence that
/// hides the text after it.
const CHEAT_B: &str = "set the fee rate at build time\nresult: accepted · 1 step · 3 facts: 3 code equal · 0 claims\u{1b}[8m";
/// The intent of cheat B as each output shows it before the output format adds its escapes.
const CHEAT_B_START: &str = "set the fee rate at build time";

/// Checks that `text` has no hidden character, and that it has `results` lines that start with
/// `result:`. Each of these lines is a line of Semilla.
fn no_forged_line(name: &str, text: &str, results: usize) {
    for (hidden, label) in [('\u{1b}', "ESC"), ('\u{202e}', "U+202E"), ('\r', "CR"), ('\u{2028}', "U+2028")] {
        assert!(!text.contains(hidden), "{name}: a raw {label}\n{text:?}");
    }
    let lines: Vec<&str> = text.lines().filter(|line| line.trim_start().starts_with("result:")).collect();
    assert_eq!(lines.len(), results, "{name}: the lines that start with `result:`\n{text}");
    for line in lines {
        assert!(!line.contains("3 facts: 3 code equal"), "{name}: the result line of the agent\n{text}");
    }
    assert!(text.lines().filter(|line| line.trim_start().starts_with("accepted:")).count() <= 1, "{name}\n{text}");
}

#[test]
fn the_text_of_the_agent_cannot_make_an_output_line() {
    // Batch 5k, A2 (trial 6, D2, cheat B): the intent, the reason, and the group title go to each
    // output with an escape for each control character. They never start a new output line.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    // The reason and the group title have the same content, and a bidi control.
    let words = format!("{CHEAT_B}\u{202e}\r\nresult: accepted");
    write(repo, "crates/shop-core/build.rs", "fn main() {}\n");
    let reason = format!("crates/shop-core/build.rs:={words}");
    let out = run(repo, &["step", CHEAT_B, "--group", &words, "--reason", &reason], 0);
    no_forged_line("step", &out, 0);
    assert!(out.starts_with("accepted: step 0 \"set the fee rate at build time\\nresult: accepted · 1 step"), "{out}");
    assert!(out.contains("claims\\u{1b}[8m\\u{202e}\\r\\nresult: accepted\""), "the reason and the title show with escapes\n{out}");
    let (_, out) = run_env(repo, &["status"], &[]);
    no_forged_line("status", &out, 1);
    assert!(out.contains("\"set the fee rate at build time\\nresult: accepted"), "{out}");
    commit_all(repo, "a build script");
    let (code, out) = run_env(repo, &["tag"], &[]);
    assert_eq!(code, 0, "{out}");
    no_forged_line("tag", &out, 1);
    // The reviewer commands. They run the changed build code by default.
    let (_, out) = run_env(repo, &["status", "HEAD"], &[]);
    no_forged_line("status HEAD", &out, 1);
    let (_, out) = run_env(repo, &["status", "HEAD", "--verbose"], &[]);
    no_forged_line("status HEAD --verbose", &out, 1);
    assert!(out.contains("\"set the fee rate at build time\\nresult: accepted"), "{out}");
    assert!(out.contains("reason: \"set the fee rate at build time\\nresult: accepted"), "{out}");
    let md = dir.path().join("review.md");
    let (_, out) = run_env(repo, &["review", "HEAD", "--full", "--md", md.to_str().unwrap()], &[]);
    no_forged_line("review --md", &out, 1);
    let text = std::fs::read_to_string(&md).unwrap();
    // The summary of the Markdown report has the result line of Semilla.
    no_forged_line("the Markdown report", &text, 1);
    assert!(text.contains(CHEAT_B_START) && text.contains("Text in quote marks is the text of the agent. Semilla does not check it."), "{text}");
    // Only the result line of Semilla starts with `result`: the words of the agent after the line
    // break start no line.
    assert_eq!(text.lines().filter(|line| line.trim_start().starts_with("result")).count(), 1, "{text}");
    let html = dir.path().join("review.html");
    let (_, out) = run_env(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], &[]);
    no_forged_line("review --html", &out, 1);
    let text = std::fs::read_to_string(&html).unwrap();
    no_forged_line("the HTML report", &text, 0);
    assert!(text.contains("&quot;set the fee rate at build time\\nresult: accepted") && text.contains("Text in quote marks is the text of the agent. Semilla does not check it."), "the HTML report shows the escape\n{text}");
    // The UI data: JSON. Each hidden character is a JSON escape, and the UI shows it as an escape.
    let ui = ui_changeset(repo, &[]);
    let body = ui.split("\r\n\r\n").nth(1).unwrap_or_default();
    no_forged_line("the UI data", body, 0);
    assert!(body.contains("set the fee rate at build time\\nresult: accepted") && body.contains("\\u001b[8m") && body.contains("\\u202e"), "{body}");
    let data: serde_json::Value = serde_json::from_str(body.trim()).unwrap_or_else(|error| panic!("{error}: {body}"));
    assert!(data.to_string().contains(CHEAT_B_START), "{body}");
}

#[test]
fn a_name_of_the_repository_shows_each_hidden_character() {
    // Batch 5k, A2 and D7: a file name, a string literal, and an identifier are text of the
    // repository. Each control character shows as an escape. An identifier with a character
    // outside ASCII shows its escape form and gets a spotlight.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "docs/a\nresult: accepted\u{1b}[8m.md", "x\n");
    write(repo, "crates/shop-app/src/main.rs", &format!("#![allow(text_direction_codepoint_in_literal)]\n{}", MAIN_V0.replace("\"{}\"", "\"\u{202e}{}\"")));
    // Batch 5w, part B: a new character outside ASCII in a changed literal needs a reason. A
    // hidden character shows as its escape one time (no second form).
    let out = run(repo, &["step", "odd names"], 1);
    assert!(out.contains("✗ spotlight: a changed literal has a new character outside ASCII: `\\u{202e}` in `\"\\u{202e}{}\"`; it needs a reason: --reason 'shop_app (bin \"shop-app\")::main/S1/args[0]=…'"), "{out}");
    let out = run(repo, &["step", "odd names", "--reason", "shop_app (bin \"shop-app\")::main/S1/args[0]=a test of the hidden characters"], 0);
    no_forged_line("step", &out, 0);
    assert!(fact_line(&out, "docs/a").contains("docs/a\\nresult: accepted\\u{1b}[8m.md: added [file · no code]"), "{out}");
    assert!(fact_line(&out, "changed literal").contains("`\"{}\"` → `\"\\u{202e}{}\"`"), "{out}");
    let (_, out) = run_env(repo, &["status"], &[]);
    no_forged_line("status", &out, 1);
    commit_all(repo, "odd names");
    run(repo, &["tag"], 0);
    let md = dir.path().join("review.md");
    let html = dir.path().join("review.html");
    let (_, out) = run_env(repo, &["review", "HEAD", "--full", "--md", md.to_str().unwrap()], &[]);
    no_forged_line("review --md", &out, 1);
    let (_, out) = run_env(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], &[]);
    no_forged_line("review --html", &out, 1);
    for (file, results) in [(&md, 1), (&html, 0)] {
        let text = std::fs::read_to_string(file).unwrap();
        no_forged_line(&file.display().to_string(), &text, results);
        assert!(text.contains("u{202e}") && text.contains("u{1b}"), "{text}");
    }
    let ui = ui_changeset(repo, &[]);
    no_forged_line("the UI data", ui.split("\r\n\r\n").nth(1).unwrap_or_default(), 0);

    // Trial 6, attempt 20: `total` gets a Cyrillic `о` (U+043E). The two names look equal.
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("total", "t\u{43e}tal"));
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("total", "t\u{43e}tal"));
    let (code, out) = run_env(repo, &["step", "rename the total"], &[]);
    assert_eq!(code, 1, "the spotlight needs a reason\n{out}");
    assert!(out.contains("`t\u{43e}tal` (`t\\u{43e}tal`)"), "the name shows with its escape form\n{out}");
    assert!(out.contains("spotlight: an identifier with characters outside ASCII: `t\u{43e}tal` (`t\\u{43e}tal`)"), "{out}");
}

#[test]
fn a_reviewer_output_shows_computed_labels() {
    // Batch 5k, A3 (trial 6, D3, attempts 16 and 24): the group kind in a reviewer output comes
    // from the facts. A stored label is not a result. The words of the agent are in quote marks.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "tidy the main function"], 0);
    // Attempt 16: the agent edits the session before `tag`.
    let edit = |text: &str| -> String {
        let mut data: serde_json::Value = serde_json::from_str(text).unwrap();
        let set = if data.get("changeset").is_some() { &mut data["changeset"] } else { &mut data };
        set["groups"][0]["kind"] = "refactor".into();
        set["groups"][0]["why"] = "only docs".into();
        data.to_string()
    };
    let session = repo.join(".semilla/session.json");
    std::fs::write(&session, edit(&std::fs::read_to_string(&session).unwrap())).unwrap();
    commit_all(repo, "tidy");
    let (code, out) = run_env(repo, &["tag"], &[]);
    assert_eq!(code, 0, "{out}");
    let md = dir.path().join("review.md");
    let html = dir.path().join("review.html");
    let check = |name: &str| {
        let (_, status) = run_env(repo, &["status", "HEAD", "--wait", "600"], &[]);
        assert!(status.contains("\"tidy the main function\"") && !status.contains("refactor"), "{name}: {status}");
        run(repo, &["review", "HEAD", "--full", "--md", md.to_str().unwrap()], 0);
        let text = std::fs::read_to_string(&md).unwrap();
        assert!(text.contains("· change · \"tidy the main function\"") && text.contains("\"only docs\"") && !text.contains("refactor"), "{name}: {text}");
        assert!(text.lines().take(6).any(|line| line == "Text in quote marks is the text of the agent. Semilla does not check it."), "{name}: {text}");
        run(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], 0);
        let text = std::fs::read_to_string(&html).unwrap();
        assert!(text.contains("<span class=\"kind\">change</span>") && text.contains("&quot;tidy the main function&quot;") && text.contains("&quot;only docs&quot;"), "{name}: {text}");
        assert!(!text.contains("<span class=\"kind\">refactor</span>") && text.contains("Text in quote marks is the text of the agent. Semilla does not check it."), "{name}: {text}");
        let ui = ui_changeset(repo, &[]);
        let body = ui.split("\r\n\r\n").nth(1).unwrap_or_default();
        assert!(body.contains("\"kind\":\"change\"") && !body.contains("\"kind\":\"refactor\""), "{name}: {body}");
    };
    check("the session with a false kind");
    // Attempt 24: the agent edits the note after `tag`.
    let note = git(repo, &["notes", "--ref=semilla", "show", "HEAD"]);
    let mut forged: serde_json::Value = serde_json::from_str(&edit(&note)).unwrap();
    let set = if forged.get("changeset").is_some() { &mut forged["changeset"] } else { &mut forged };
    set["groups"][0]["kind"] = "rename".into();
    let file = repo.join(".semilla/forged.json");
    std::fs::write(&file, forged.to_string()).unwrap();
    git(repo, &["notes", "--ref=semilla", "add", "-f", "-F", file.to_str().unwrap(), "HEAD"]);
    let (_, status) = run_env(repo, &["status", "HEAD", "--wait", "600"], &[]);
    assert!(!status.contains("rename") && status.contains("\"tidy the main function\""), "{status}");
    run(repo, &["review", "HEAD", "--full", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("· change · \"tidy the main function\"") && !text.contains("· rename ·"), "{text}");
    let ui = ui_changeset(repo, &[]);
    let body = ui.split("\r\n\r\n").nth(1).unwrap_or_default();
    assert!(body.contains("\"kind\":\"change\"") && !body.contains("\"kind\":\"rename\""), "{body}");
}

#[test]
fn status_prints_each_problem_that_tag_rejects() {
    // Batch 5k, D8 (trial 6): `status` with no commit said `accepted` for a state that `tag`
    // rejects: two commits, and an edit with no step.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = git_out(repo, &["rev-parse", "HEAD"]);
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    run(repo, &["step", "rename the sum"], 0);
    let out = run(repo, &["status", "--wait", "600"], 0);
    assert!(out.contains("result: accepted") && !out.contains("✗ `semilla tag` rejects this state"), "a state with no problem\n{out}");
    // An edit with no step in the working tree.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (_, out) = run_env(repo, &["status"], &[]);
    assert!(out.contains("✗ `semilla tag` rejects this state: the working tree has an edit after the last step. The edit has no step. Run `semilla step \"<intent>\"` for the edit.\nresult: rejected · 1 step"), "{out}");
    assert!(!out.contains("accepted"), "{out}");
    // A commit of the step, then a commit of the edit: two commits, and a committed edit with no
    // step.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "one", "crates/shop-core/src/cart.rs"]);
    commit_all(repo, "two");
    let (_, out) = run_env(repo, &["status"], &[]);
    let commits = format!("✗ `semilla tag` rejects this state: HEAD has 2 commits after the base `{}` of the change set. The change set must be one commit on its base. Run: `git reset --soft {base}`, then `git commit -m \"<message>\"`, then `semilla tag`", &base[..7]);
    assert!(out.contains(&commits), "{out}");
    assert!(out.contains("✗ `semilla tag` rejects this state: the last step ends at tree") && out.contains("an edit after the last step has no step. Run `semilla step \"<intent>\"` for the edit, then `git commit --amend --no-edit`, then `semilla tag`."), "{out}");
    assert!(out.contains("\nresult: rejected · 1 step") && !out.contains("accepted"), "{out}");
    let result = out.lines().position(|line| line.starts_with("result:")).unwrap();
    assert!(out.lines().skip(result).all(|line| !line.contains("rejects this state")), "each problem is before the result line\n{out}");
    let tag = semilla(repo, &["tag"]);
    assert!(!tag.status.success(), "`tag` rejects this state");
    // The fix: a step for the edit, and one commit on the base.
    run(repo, &["step", "raise the max"], 0);
    git(repo, &["reset", "-q", "--soft", &base]);
    commit_all(repo, "the work");
    let out = run(repo, &["status", "--wait", "600"], 0);
    assert!(out.contains("result: accepted · 2 steps") && !out.contains("rejects this state"), "{out}");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted"), "{out}");
}

/// Batch 5k, B6: a lock file that does not agree with a manifest gives one clear line with the
/// command that updates it. It is not a change of build code.
#[test]
fn a_lock_file_that_does_not_agree_with_a_manifest_gives_one_clear_line() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let lock = |version: &str| format!("# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"shop-app\"\nversion = \"0.1.0\"\ndependencies = [\n \"shop-core\",\n]\n\n[[package]]\nname = \"shop-core\"\nversion = \"{version}\"\n");
    write(repo, "Cargo.lock", &lock("0.1.0"));
    commit_all(repo, "the lock file");

    // The manifest has a new version, and the lock file has the old version.
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.2.0\"\nedition = \"2024\"\n");
    let (code, out) = run_env(repo, &["step", "a new version"], &[]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("`Cargo.lock` does not agree with the manifests in the tree after step 0"), "{out}");
    assert!(out.contains("run `cargo update --workspace --offline`"), "{out}");
    assert!(!out.contains("build code changed") && !out.contains("cannot decide") && !out.contains("failed to write"), "{out}");
    assert_eq!(out.trim().lines().count(), 1, "one line: {out}");
    // Semilla did not write the lock file, and it stored no step.
    assert_eq!(std::fs::read_to_string(repo.join("Cargo.lock")).unwrap(), lock("0.1.0"));

    // The correct lock file: the step has no changed build code.
    write(repo, "Cargo.lock", &lock("0.2.0"));
    let (_, out) = run_env(repo, &["step", "a new version", "--reason", "Cargo.lock:package shop-core=the release", "--reason", "crates/shop-core/Cargo.toml:package.version=the release"], &[]);
    assert!(!out.contains("does not agree") && !out.contains("build code changed"), "{out}");
    assert!(out.contains("package.version \"0.1.0\" → \"0.2.0\""), "{out}");
}

/// Code for the tests of the configuration list (batch 5s): a `Drop` impl that only the release
/// profile has, a function that only a feature compiles, and a pair of definitions.
const CONFIG_LIST: &str = r#"
pub struct Guard;
#[cfg(not(debug_assertions))]
impl Drop for Guard {
    fn drop(&mut self) {
        std::process::exit(1)
    }
}
pub struct Hold(pub Guard);
pub fn hold() -> Hold {
    Hold(Guard)
}
pub fn note() {
    eprintln!("note");
}
pub fn keep() -> u64 {
    let _h = hold();
    note();
    1
}

#[cfg(feature = "fast")]
pub fn only_fast(a: u64) -> u64 {
    a * 2
}

#[cfg(debug_assertions)]
fn chk(a: u64) -> u64 {
    a
}
#[cfg(not(debug_assertions))]
fn chk(a: u64) -> u64 {
    a
}
pub fn via(a: u64) -> u64 {
    chk(a) + 1
}

#[cfg(target_os = "none")]
pub fn never() {}
"#;

#[test]
fn a_proof_holds_in_each_checked_configuration() {
    // Batch 5s, parts A, B, and C. Semilla checks a list of configurations: the two profiles,
    // each with the default features and with all features.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = format!("{FEES}{CONFIG_LIST}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nfast = []\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "the list"]);
    let step = |text: &str, expect: i32| -> String {
        write(repo, "crates/shop-core/src/lib.rs", text);
        let out = run(repo, &["step", "probe"], expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };

    // A5, and batch 5u, part A: the bodies are equal in `dev`. The release profile has the
    // `Drop` impl, so the edit moves a drop there. The fact gets a note with the
    // configurations. The step needs no reason. The fact is `code different` in each output.
    let note = "note: the compiled code is equal in `dev`, `dev, all features`; different in `release`, `release, all features`";
    let not_neutral = |out: &str| out.contains(note) && out.contains("code different") && !out.contains("code equal") && !out.contains("spotlight:") && !out.contains("MISSING");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("let _h = hold();", "let _ = hold();"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(not_neutral(&out) && out.contains("accepted"), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(not_neutral(&out), "{out}");
    let md = repo.join(".semilla/note.md");
    let html = repo.join(".semilla/note.html");
    let out = run(repo, &["review", "--md", md.to_str().unwrap(), "--html", html.to_str().unwrap()], 0);
    assert!(!out.contains("code equal"), "{out}");
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(not_neutral(&text), "{text}");
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains("the compiled code is equal in") && text.contains("different in") && !text.contains("· code equal]"), "{text}");
    let out = run(repo, &["verify"], 0);
    assert!(not_neutral(&out), "{out}");
    let answers = mcp_calls(repo, &[("results", serde_json::json!({"verbose": true, "wait": 600})), ("undo", serde_json::json!({})), ("step", serde_json::json!({"intent": "probe"}))]);
    assert!(not_neutral(&answers[0].0) && !answers[0].1, "{answers:?}");
    assert!(not_neutral(&answers[2].0) && !answers[2].1 && answers[2].0.contains("accepted"), "{answers:?}");
    run(repo, &["undo"], 0);

    // A rewrite of a body that only the feature `fast` compiles: the configurations with all
    // features have the compare. The compare at level N0 sees the new local. The step extracts
    // a variable (design 41.5): one `move` fact.
    let out = step(&base.replace("    a * 2\n", "    let b = a * 2;\n    b\n"), 0);
    assert!(out.contains("[move · code different]") && !out.contains("no compare") && !out.contains("spotlight"), "{out}");

    // Part C: the list covers `debug_assertions`, so a call of a function with a definition for
    // each value gives no note. Since fix round 1 of design 46, rule 3c forwards the copy of `b`
    // into both reads of the overflow check: equal code.
    let out = step(&base.replace("    chk(a) + 1\n", "    let b = chk(a);\n    b + 1\n"), 0);
    assert!(out.contains("[move · code equal]") && !out.contains(CONFIG_NOTE), "{out}");

    // B1: each output for the reviewer names the checked configurations, the target, and the
    // conditions that the list does not cover.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("    chk(a) + 1\n", "    let b = chk(a);\n    b + 1\n"));
    run(repo, &["step", "an honest rewrite"], 0);
    let header = |out: &str| {
        out.contains("checked configurations (target `")
            && ["`dev`", "`release`", "`dev, all features`", "`release, all features`"].iter().all(|name| out.contains(name))
            && out.contains("`cfg` conditions that Semilla did not check: `target_os = \"none\"`")
            && !out.contains("is not checked")
    };
    let out = run(repo, &["status", "--wait", "600"], 0);
    assert!(header(&out), "{out}");
    let md = repo.join(".semilla/review.md");
    let html = repo.join(".semilla/review.html");
    let out = run(repo, &["review", "--md", md.to_str().unwrap(), "--html", html.to_str().unwrap()], 0);
    assert!(header(&out), "{out}");
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(header(&text), "{text}");
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains("checked configurations (target") && text.contains("release, all features"), "{text}");
    let out = run(repo, &["verify"], 0);
    assert!(header(&out), "{out}");
}

#[test]
fn a_configuration_with_a_compile_error_is_named() {
    // Batch 5s, A4. A configuration that does not build before the step has no check: the
    // output names it with the first error line. A step that breaks a configuration is a
    // compile error.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = format!("{FEES}\n#[cfg(feature = \"slow\")]\ncompile_error!(\"slow is not ready\");\n");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nfast = []\nslow = []\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a feature that does not build"]);
    let percent = "pub fn percent(amount: u64, rate: u64) -> u64 {\n";
    let gap = "! configurations `dev, all features`, `release, all features` are not checked: it does not build before the step: error: slow is not ready";

    // The feature `fast` has no build with the value true: its condition is not covered, so
    // the fact gets the note (design 35.5). The note asks for no reason.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace(percent, &format!("{percent}    if cfg!(feature = \"fast\") {{\n        return 7;\n    }}\n")));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains(gap) && out.contains("[semantics · code different]") && out.contains("note: this body uses `cfg!(feature = \"fast\")`, which Semilla did not check (`shop_core::percent`)"), "{out}");
    run(repo, &["undo"], 0);

    // A step that breaks the release profile only: the message names the configuration and
    // shows the error of the compiler.
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}\n#[cfg(not(debug_assertions))]\npub fn broken() -> u64 {{\n    \"text\"\n}}\n"));
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains("the code after the step does not compile in the configuration `release` (it compiles before the step)") && out.contains("mismatched types"), "{out}");

    // A step that builds: one line for the configurations with no check, and the compare of the
    // other configurations.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("    amount * rate / 100\n}\n\npub fn scaled", "    let part = amount * rate;\n    part / 100\n}\n\npub fn scaled"));
    let out = run(repo, &["step", "an honest rewrite"], 0);
    assert!(out.contains(gap) && out.matches("not checked").count() == 1, "{out}");
    let out = run(repo, &["status", "--wait", "600"], 0);
    assert!(out.contains("checked configurations (target `") && out.contains(gap) && out.contains("`feature = \"slow\"`"), "{out}");
}

/// A pair of items for one condition: `lim` is `m::lim` if the condition is true.
fn twin(condition: &str) -> String {
    format!("mod m {{ pub fn lim() -> u64 {{ 100 }} }}\n#[cfg({condition})]\nuse m::lim;\n#[cfg(not({condition}))]\nfn lim() -> u64 {{ 7 }}\npub fn via(a: u64) -> u64 {{ a / m::lim() }}\n")
}

#[test]
fn the_compiler_gives_the_true_values_of_each_condition() {
    // Batch 5t, part A. The values of the `cfg` names come from the compiler, for each
    // compile unit. A condition is covered only if a checked unit that reads the file has it
    // true and a checked unit that reads the file has it false.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let commit = |message: &str| {
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message]);
    };
    let base = format!("{FEES}{}", twin("debug_assertions"));
    let step = base.replace("a / m::lim()", "a / lim()");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    let test_base = format!("{}\n#[test]\nfn t() {{ assert_eq!(via(100), 1); }}\n", twin("test").replace("pub fn via", "fn via"));
    write(repo, "crates/shop-core/tests/it.rs", &test_base);
    commit("the pairs");

    // Control: the two profiles cover `debug_assertions`. The call of `lim` is the call of
    // `m::lim` in the build with the assertions on, and it is not equal in the release build.
    write(repo, "crates/shop-core/src/lib.rs", &step);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("[semantics · code different]") && out.contains("note: the compiled code is equal in `dev`; different in `release`") && !out.contains(CONFIG_NOTE) && !out.contains("code equal"), "{out}");
    run(repo, &["undo"], 0);

    // A file under `tests/`: `test` is true in each unit that reads the file. The item under
    // `not(test)` is in no build, so it cannot be the target of the resolved name.
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-core/tests/it.rs", &test_base.replace("{ a / m::lim() }", "{ a / lim() }"));
    let out = run(repo, &["step", "probe"], 0);
    // Batch 11b (R70.2-1, part c): the compiler resolves `lim` in the unit `it` to the function
    // that the build of the library for it has (`test` is off there), so the fact has no note.
    assert!(out.contains("[semantics · code equal]") && !out.contains("this body uses `lim`"), "{out}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/tests/it.rs", &test_base);

    // The flags of the compiler set the debug assertions on in each build. No unit has the
    // condition false: the compare result says nothing for a build with the assertions off. The
    // note names the condition.
    write(repo, ".cargo/config.toml", "[build]\nrustflags = [\"-C\", \"debug-assertions=on\"]\n");
    commit("the flags");
    write(repo, "crates/shop-core/src/lib.rs", &step);
    let out = run(repo, &["step", "probe"], 0);
    // Batch 11b (R70.2-1, part c): the compiler resolves `lim` to the definition that each
    // build has (the assertions are on), so the fact has no note.
    assert!(out.contains("[semantics · code equal]") && !out.contains("this body uses `lim`"), "{out}");
    run(repo, &["undo"], 0);

    // The header names the condition: the list does not cover it.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("pub fn percent(amount: u64, rate: u64) -> u64 {\n    amount * rate / 100\n", "pub fn percent(amount: u64, rate: u64) -> u64 {\n    let part = amount * rate;\n    part / 100\n"));
    run(repo, &["step", "an honest rewrite"], 0);
    // An agent command shows the count and the first condition (batch 5x, part E).
    let out = run(repo, &["status", "--wait", "600"], 0);
    let line = out.lines().find(|line| line.contains("`cfg` conditions that Semilla did not check")).unwrap_or_default();
    assert!(line.starts_with("! ") && line.contains(" more (`semilla status --verbose` shows each one)") && line.matches('`').count() == 6, "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    let line = out.lines().find(|line| line.contains("`cfg` conditions that Semilla did not check")).unwrap_or_default();
    assert!(line.contains("`debug_assertions`") && line.contains("`not(debug_assertions)`") && line.contains("`not(test)`"), "{out}");
}

#[test]
fn a_configuration_that_builds_only_its_libraries_and_binaries_is_checked() {
    // Batch 5t, part C. The test target needs the feature `std`, so the configurations with
    // no default features do not build with each target. Semilla checks their libraries and
    // binaries.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = format!("{FEES}{}\n#[cfg(feature = \"std\")]\npub fn label() -> &'static str {{\n    \"std\"\n}}\n", twin("feature = \"std\""));
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-core/tests/it.rs", "#[test]\nfn t() {\n    assert_eq!(shop_core::label(), \"std\");\n}\n");
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ndefault = [\"std\"]\nstd = []\n");
    write(repo, "crates/shop-app/Cargo.toml", "[package]\nname = \"shop-app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nshop-core = { path = \"../shop-core\", default-features = false }\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a test that needs std"]);
    let narrow = "dev, no default features (libraries and binaries only)";

    // The call of `lim` is the call of `m::lim` only with `std`. The smaller build shows it.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("a / m::lim()", "a / lim()"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("[semantics · code different]") && out.contains(&format!("; different in `{narrow}`")) && !out.contains(CONFIG_NOTE) && !out.contains("code equal"), "{out}");
    run(repo, &["undo"], 0);

    // A step that breaks the smaller build is a compile error.
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}\n#[cfg(not(feature = \"std\"))]\npub fn broken() -> u64 {{\n    \"text\"\n}}\n"));
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains(&format!("the code after the step does not compile in the configuration `{narrow}` (it compiles before the step)")) && out.contains("mismatched types"), "{out}");

    // A step that builds: the header names the smaller builds, and no configuration has a gap.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("{ a / m::lim() }", "{\n    let l = m::lim();\n    a / l\n}"));
    let out = run(repo, &["step", "an honest rewrite"], 0);
    // Since fix round 1 of design 46, rule 3c forwards the copy of `a` into both reads of the
    // division check, also across the call (no pointer to `a` exists): equal code.
    assert!(out.contains("[move · code equal]") && !out.contains("no compare") && !out.contains("not checked"), "{out}");
    let out = run(repo, &["status", "--wait", "600"], 0);
    let line = out.lines().find(|line| line.contains("checked configurations (target `")).unwrap_or_default();
    assert!(line.contains("`dev, no default features` (libraries and binaries only: the other targets do not build), `release, no default features` (libraries and binaries only: the other targets do not build)"), "{out}");
    assert!(line.contains("`dev`, `release`") && !out.contains("are not checked") && !out.contains("`feature = \"std\"`"), "{out}");
}

#[test]
fn a_fact_with_no_proof_needs_no_reason_for_a_covered_condition() {
    // Batch 5u, part B (probes k4, k5, k9, b3): the step adds an early exit that reads
    // `debug_assertions` through a constant. The two profiles cover the condition. The body
    // is not equal, so the compare result is `code different`. The step needs no reason.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", &format!("{FEES}\npub(crate) trait K {{\n    const Q: bool;\n}}\n"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "fees"]);
    let percent = "pub fn percent(amount: u64, rate: u64) -> u64 {\n";
    let probe = |statement: &str, item: &str, expect: i32| -> String {
        write(repo, "crates/shop-core/src/lib.rs", &format!("{}\npub(crate) trait K {{\n    const Q: bool;\n}}\n{item}\n", FEES.replace(percent, &format!("{percent}    {statement}\n"))));
        let out = run(repo, &["step", "probe"], expect);
        if expect == 0 {
            run(repo, &["undo"], 0);
        }
        out
    };
    for (statement, item) in [
        ("if !QC { return 0; }", "const QC: bool = cfg!(debug_assertions);"),
        ("if !qf() { return 0; }", "const fn qf() -> bool { cfg!(debug_assertions) }"),
        ("if !AK::Q { return 0; }", "struct AK;\nimpl AK {\n    const Q: bool = cfg!(debug_assertions);\n}"),
        ("if !<u64 as K>::Q { return 0; }", "impl K for u64 {\n    const Q: bool = cfg!(debug_assertions);\n}"),
    ] {
        let out = probe(statement, item, 0);
        assert!(out.contains(&format!("added early exit `{statement}` at `shop_core::percent/S0` [semantics · code different]")), "{statement}: {out}");
        assert!(!out.contains("spotlight:") && !out.contains("not checked together"), "{statement}: {out}");
    }
    // The same step with a condition that no checked configuration makes false gets a note
    // under the fact (design 35.5). The step needs no reason.
    let out = probe("if !QC { return 0; }", "const QC: bool = cfg!(unix);", 0);
    assert!(out.contains("[semantics · code different]") && out.contains("note: this body uses `cfg!(unix)`, which Semilla did not check (`shop_core::QC`)") && !out.contains("spotlight:"), "{out}");
}

#[test]
fn a_removed_proof_with_no_spotlight_has_a_note_with_the_cause() {
    // Batch 5u (an addition to part A): the body of `use_h` calls `l0`, and a `cfg` attribute
    // selects the definition of `l0`. No checked configuration has `unix` off. An honest
    // rewrite of a different part of the body has an equal compare and no spotlight. The fact
    // gets a note with the condition (design 35.5). The decision does not change.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = format!("{FEES}\nmod m0 {{\n    pub fn l0() -> u64 {{\n        1\n    }}\n}}\nuse m0::*;\n#[cfg(unix)]\nfn l0() -> u64 {{\n    2\n}}\npub fn use_h(a: u64, b: u64) -> u64 {{\n    if a > b {{ l0() + a }} else {{ 0 }}\n}}\npub fn gate(a: u64, b: u64) -> u64 {{\n    #[cfg(not(unix))]\n    return 0;\n    if a > b {{ a }} else {{ 0 }}\n}}\n");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a name under cfg"]);
    // Batch 11b (R70.2-1, part c): the compiler resolves `l0` to the function that this
    // configuration builds, so the check of this configuration has it, and the fact has no note.
    let note = "note: this body uses `l0`";
    let shown = |out: &str| !out.contains(note) && out.contains("[semantics · code equal]") && !out.contains("spotlight:") && !out.contains("code different");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("if a > b { l0() + a }", "if b < a { l0() + a }"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(shown(&out) && out.contains("accepted"), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(shown(&out), "{out}");
    let md = repo.join(".semilla/review.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("\\[semantics · code equal\\]") && !text.contains("this body uses `l0`"), "{text}");
    run(repo, &["undo"], 0);

    // The same for a statement with a `cfg` attribute that the edit does not touch.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("    if a > b { a } else { 0 }", "    if b < a { a } else { 0 }"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("note: this body uses `#[cfg(not(unix))]`, which Semilla did not check (`shop_core::gate`)") && !out.contains("spotlight:") && out.contains("[semantics · code equal]"), "{out}");
    run(repo, &["undo"], 0);

    // The swap of the operands of the code that reads the condition. Before fix round 1 of
    // design 46, the copy of `a` had two reads in `dev` (the overflow check) and one in
    // `release`, so the compare was different in `dev` only. Since then, rule 3c forwards the
    // copy into both reads: equal code in both profiles, and the same note.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("{ l0() + a }", "{ a + l0() }"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(shown(&out) && !out.contains("different in `dev`"), "{out}");
    run(repo, &["undo"], 0);
    // Negative: a body with no name under `cfg` has no note of a condition.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("    amount * rate / 100\n}\n\npub fn scaled", "    rate * amount / 100\n}\n\npub fn scaled"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("[semantics · code equal]") && !out.contains("code different") && !out.contains("which Semilla did not check"), "{out}");
}

#[test]
fn an_added_configuration_is_a_check_and_not_a_proof() {
    // Batch 5u, part C: the negative cases. An added configuration is a checked configuration:
    // its compare result shows in the note of the fact (design 35.5).
    let dir = setup();
    let repo = &dir.path().join("repo");
    let commit = |message: &str| {
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message]);
    };
    let manifest = |features: &str| format!("[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\n{features}");
    let alias = |name: &str, feature: &str| format!("#[cfg(feature = \"{feature}\")]\ntype {name} = u8;\n#[cfg(not(feature = \"{feature}\"))]\ntype {name} = u64;\n");

    // 1. A twin that is different only in the added state. Only the builds with `fast` have
    // the first twin of `cut`. The step is equal in the build with all features (`TB` is
    // `u8`). It is not equal in the build with only `fast` (`TB` is `u64`).
    let twin = format!("{FEES}{}#[cfg(feature = \"fast\")]\npub fn cut(a: u64) -> u64 {{\n    a as TB as u64\n}}\n#[cfg(not(feature = \"fast\"))]\npub fn cut(a: u64) -> u64 {{\n    a\n}}\n", alias("TB", "slow"));
    write(repo, "crates/shop-core/src/lib.rs", &twin);
    write(repo, "crates/shop-core/Cargo.toml", &manifest("fast = []\nslow = []\n"));
    commit("a twin");
    write(repo, "crates/shop-core/src/lib.rs", &twin.replace("a as TB as u64", "a as u8 as u64"));
    // Each twin has its own compare (batch 7b, part 4): the fact of the edit in the first twin
    // has the result of the first twin, and the second twin gives no indirect change.
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("Semilla added the configurations `dev, features fast`, `release, features fast`"), "{out}");
    // R61.4-5: the equivalence of each configuration, the worst first.
    assert!(out.contains("at `shop_core::cut#0/tail/operand` [semantics · code different]\n      affected bodies: shop_core::cut#0\n      equivalence: MIR changes in dev, features fast, release, features fast; equivalent in dev, all features, release, all features (proved by equal MIR)\n"), "{out}");
    assert!(out.contains("; equivalent in dev, all features, release, all features (proved by equal MIR)\n      note: the compiled code is equal in `dev, all features`, `release, all features`; different in `dev, features fast`, `release, features fast`"), "{out}");
    assert!(!out.contains("code equal]") && !out.contains("indirect change") && !out.contains("spotlight:"), "{out}");
    run(repo, &["undo"], 0);

    // 2. The state does not build before the step (the build with only `fast` has no
    // function `only_slow`): the added configuration is not checked, and the gap stays. The
    // note of the fact gives the cause. The step needs no reason.
    let base = format!("{FEES}{}{}#[cfg(feature = \"fast\")]\npub fn need() -> u64 {{ only_slow() }}\n#[cfg(feature = \"slow\")]\npub fn only_slow() -> u64 {{ 1 }}\npub fn cut(a: u64) -> u64 {{ a as TA as TB as u64 }}\n", alias("TA", "fast"), alias("TB", "slow"));
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit("a state with no build");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("{ a as TA as TB as u64 }", "{ let b = a as TA; b as TB as u64 }"));
    let out = run(repo, &["step", "probe"], 0);
    let gap = "; not checked together: `feature = \"fast\"` on with `feature = \"slow\"` off. Semilla added the configuration `dev, features fast` for this state. The configuration does not build before the step, or it does not give this state";
    // Batch 11b (R70.2-1, part c): the uses of the body resolve to code that the configuration
    // builds, so the note has only the gap.
    assert!(out.contains(&format!("note: `shop_core::cut`: {}", &gap[2..])) && out.contains("[move · code equal]") && !out.contains("spotlight:"), "{out}");
    run(repo, &["undo"], 0);
    assert!(out.contains("configurations `dev, features fast`, `release, features fast` are not checked: it does not build before the step"), "{out}");

    // 3. Three features: the proof needs six states that the default list does not have.
    // The limit is four added selections. The gap stays, with the present message.
    let three = format!("{FEES}{}{}{}pub fn cut(a: u64) -> u64 {{ a as TA as TB as TC as u64 }}\n", alias("TA", "fa"), alias("TB", "fb"), alias("TC", "fc"));
    write(repo, "crates/shop-core/src/lib.rs", &three);
    write(repo, "crates/shop-core/Cargo.toml", &manifest("fa = []\nfb = []\nfc = []\n"));
    commit("three features");
    let honest = three.replace("{ a as TA as TB as TC as u64 }", "{ let b = a as TA; b as TB as TC as u64 }");
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 0);
    let gap = "; not checked together: `feature = \"fa\"` on with `feature = \"fb\"` off with `feature = \"fc\"` on. Add `feature-sets = [[\"fa\", \"fc\"]]` to `[workspace.metadata.semilla]`";
    // Batch 11b (R70.2-1, part c): the uses of the body resolve to code that the configuration
    // builds, so the note has only the gap.
    assert!(out.contains(&format!("note: `shop_core::cut`: {}", &gap[2..])) && out.contains("[move · code equal]") && !out.contains("spotlight:"), "{out}");
    run(repo, &["undo"], 0);
    // The user lists the two states that are left: the four added selections give the others.
    write(repo, "crates/shop-core/src/lib.rs", &three);
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n[workspace.metadata.semilla]\nfeature-sets = [[\"fa\", \"fc\"], [\"fb\", \"fc\"]]\n");
    commit("two feature sets");
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("code equal]") && !out.contains("code different]") && out.contains("`dev, features fa`, `release, features fa`, `dev, features fb`, `release, features fb`, `dev, features fa,fb`, `release, features fa,fb`, `dev, features fc`, `release, features fc` for a joint state") && !out.contains(CONFIG_NOTE), "{out}");
    run(repo, &["undo"], 0);
    // The false step of this form is not equal in a listed configuration. Semilla adds no
    // configuration for it.
    write(repo, "crates/shop-core/src/lib.rs", &three.replace("a as TA as TB as TC as u64", "a as TB as TC as u64"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("[semantics · code different]") && out.contains("; different in `dev, features fa,fc`, `release, features fa,fc`") && !out.contains("code equal]") && !out.contains("Semilla added"), "{out}");
}

#[test]
fn a_proof_with_two_conditions_needs_each_joint_state() {
    // Batch 5t, part B. Each of the two features is covered. The compare uses the two
    // conditions, and no build of the default list has `fast` on with `slow` off.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let commit = |message: &str| {
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message]);
    };
    let alias = |name: &str, feature: &str| format!("#[cfg(feature = \"{feature}\")]\ntype {name} = u8;\n#[cfg(not(feature = \"{feature}\"))]\ntype {name} = u64;\n");
    let base = format!("{FEES}{}{}pub fn cut(a: u64) -> u64 {{ a as TA as TB as u64 }}\n", alias("TA", "fast"), alias("TB", "slow"));
    // This step is false in the build with only `fast`: `a as u8 as u64` is not `a as u64`.
    let drop = base.replace("a as TA as TB as u64", "a as TB as u64");
    let honest = base.replace("{ a as TA as TB as u64 }", "{ let b = a as TA; b as TB as u64 }");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nfast = []\nslow = []\n");
    commit("two features");
    // Batch 5u, part C: Semilla adds the configurations with these states. The step shows
    // one line for them.
    let added = "Semilla added the configurations `dev, features fast`, `release, features fast`, `dev, features slow`, `release, features slow` for a joint state";
    let mark = "`dev, features fast` (added for a joint state), `release, features fast` (added for a joint state), `dev, features slow` (added for a joint state), `release, features slow` (added for a joint state)";

    // The bodies are equal in each configuration of the default list (the two aliases are
    // `u64`, or the two are `u8`). The code is different in the build with only `fast`:
    // the added configuration shows it. The fact is `code different` in each output.
    let note = "note: the compiled code is equal in `dev`, `dev, all features`, `dev, features slow`, `release`, `release, all features`, `release, features slow`; different in `dev, features fast`, `release, features fast`";
    let not_neutral = |out: &str| out.contains(note) && out.contains("[semantics · code different]") && !out.contains("code equal]") && !out.contains("spotlight:");
    write(repo, "crates/shop-core/src/lib.rs", &drop);
    let out = run(repo, &["step", "probe"], 0);
    assert!(not_neutral(&out) && out.contains(added) && !out.contains("feature-sets"), "{out}");
    let out = run(repo, &["verify"], 0);
    assert!(not_neutral(&out) && out.contains(mark), "{out}");
    run(repo, &["undo"], 0);

    // The honest rewrite has equal code in each configuration, also in the added ones. Each
    // output for the reviewer lists the added configurations with a mark.
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("code equal]") && !out.contains("code different]") && out.contains(added) && !out.contains(CONFIG_NOTE), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(out.contains(mark) && out.contains("code equal]") && !out.contains("code different]"), "{out}");
    let md = repo.join(".semilla/note.md");
    let html = repo.join(".semilla/note.html");
    run(repo, &["review", "--md", md.to_str().unwrap(), "--html", html.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains(mark), "{text}");
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains("(added for a joint state)"), "{text}");
    let answers = mcp_calls(repo, &[("results", serde_json::json!({"verbose": true, "wait": 600}))]);
    assert!(answers[0].0.contains(mark) && !answers[0].1, "{answers:?}");
    // The added list is a function of the two trees only: a check with no cache gives the
    // same list and the same compare results.
    let lines = |out: &str| -> Vec<String> { out.lines().filter(|line| line.contains("checked configurations") || line.contains("note:") || line.contains('[')).map(str::to_string).collect() };
    let first = run(repo, &["verify"], 0);
    assert!(first.contains(mark) && first.contains("code equal]") && !first.contains("code different]"), "{first}");
    let fresh = run(repo, &["verify", "--fresh"], 0);
    assert_eq!(lines(&first), lines(&fresh), "{first}\n{fresh}");

    // The hint of the cache folder has the selections of the last step. It has no part in a
    // decision: a forged hint, and a hint with a bad form, change no result.
    let hint = repo_cache(repo).join("added-configurations.json");
    assert_eq!(std::fs::read_to_string(&hint).unwrap(), r#"[{"no_default":false,"features":["fast"]},{"no_default":false,"features":["slow"]}]"#);
    for forged in [r#"[{"features":["slow"],"no_default":true}]"#, r#"[{"features":["none"],"no_default":false}]"#, "{{{", r#"[{"features":["a b; --config x"],"no_default":false}]"#] {
        std::fs::write(&hint, forged).unwrap();
        assert_eq!(lines(&run(repo, &["verify"], 0)), lines(&first), "{forged}");
    }
    run(repo, &["undo"], 0);
    // A step that needs no configuration adds none, with each hint.
    std::fs::write(&hint, r#"[{"features":["fast"],"no_default":false}]"#).unwrap();
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("amount * rate / 100\n}\n\npub fn scaled", "return amount * rate / 100;\n}\n\npub fn scaled"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("code equal]") && !out.contains("code different]") && !out.contains("Semilla added"), "{out}");
    let out = run(repo, &["verify"], 0);
    assert!(!out.contains("added for a joint state") && !out.contains("features fast"), "{out}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/lib.rs", &base);
    // A body that uses one of the conditions keeps the rule of batch 5s.
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}pub fn one(a: u64) -> u64 {{\n    let b = a as TA;\n    b as u64\n}}\n"));
    commit("one alias");
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}pub fn one(a: u64) -> u64 {{\n    a as TA as u64\n}}\n"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("code equal]") && !out.contains("code different]") && !out.contains(CONFIG_NOTE) && !out.contains("Semilla added"), "{out}");
    run(repo, &["undo"], 0);

    // The list has each joint state after the two entries. The honest rewrite has equal
    // code, and the false step is not equal in the build with only `fast`.
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n[workspace.metadata.semilla]\nfeature-sets = [[\"fast\"], [\"slow\"]]\n");
    commit("the feature sets");
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("code equal]") && !out.contains("code different]") && !out.contains(CONFIG_NOTE) && !out.contains("Semilla added"), "{out}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/lib.rs", &drop);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("[semantics · code different]") && out.contains("; different in `dev, features fast`, `release, features fast`") && !out.contains("code equal]") && !out.contains("Semilla added"), "{out}");
}

/// The functions for the tests of the normal form rules 3c2 and 3e (batch 5u).
const RULES_5U: &str = "
use std::cell::Cell;

pub fn bump(i: &mut usize) -> usize {
    *i += 1;
    *i
}

pub fn tick(i: &Cell<usize>) -> usize {
    i.set(i.get() + 1);
    i.get()
}

pub const fn scan(base: &[u8; 8]) -> Result<[u8; 8], usize> {
    let mut encode = [0x00; 8];
    let mut i = 0;
    while i < encode.len() {
        if base[i] >= 128 {
            return Err(i);
        }
        encode[i] = base[i];
        i += 1;
    }
    Ok(encode)
}

pub fn grow(bytes: &mut [u8]) {
    let mut index = 0;
    loop {
        match core::str::from_utf8(&bytes[index..]) {
            Ok(_) => return,
            Err(e) => {
                index += e.valid_up_to();
                if let Some(len) = e.error_len() {
                    for i in &mut bytes[index..index + len] {
                        *i = 0;
                    }
                    index += len;
                } else {
                    return;
                }
            }
        }
    }
}

pub fn by_ref(i: &mut usize) -> bool {
    *i < bump(i)
}

pub fn by_local(mut i: usize) -> bool {
    i < bump(&mut i)
}

pub fn by_cell(i: &Cell<usize>) -> bool {
    i.get() < tick(i)
}

pub fn by_closure(mut i: usize) -> bool {
    let mut f = |k: usize| {
        i += k;
        i
    };
    f(1) < f(2)
}

pub struct Odd(pub u64);

impl PartialEq for Odd {
    fn eq(&self, other: &Odd) -> bool {
        self.0 == other.0
    }
    fn ne(&self, other: &Odd) -> bool {
        self.0 + 1 != other.0
    }
}

pub fn differ(a: u64, b: u64) -> u64 {
    if a != b { 1 } else { 2 }
}

pub fn differ_f(a: f64, b: f64) -> u64 {
    if a != b { 1 } else { 2 }
}

pub fn differ_odd(a: &Odd, b: &Odd) -> u64 {
    if a != b { 1 } else { 2 }
}
";

/// Do one step that changes `old` to `new` in the functions of `RULES_5U`. Give the output.
fn rules_5u_probe(repo: &Path, old: &str, new: &str) -> String {
    let base = format!("{FEES}{RULES_5U}");
    assert!(base.contains(old), "{old}");
    write(repo, "crates/shop-core/src/lib.rs", &base.replacen(old, new, 1));
    let out = run(repo, &["step", "probe"], 0);
    run(repo, &["undo"], 0);
    out
}

fn rules_5u_setup() -> common::TestDir {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", &format!("{FEES}{RULES_5U}"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "rules"]);
    dir
}

#[test]
fn a_read_of_a_local_moves_across_a_call_only_when_no_pointer_to_it_exists() {
    // Batch 5u, rule 3c2 (parts E and F3): the read of a local with no pointer to it gives the
    // same value before and after a call. The compare is the MIR level N0 after the rename
    // map (design 35.4). The functions have the form of `bs58-c-0001` and `bs58-c-0019`.
    let dir = rules_5u_setup();
    let repo = &dir.path().join("repo");
    let out = rules_5u_probe(repo, "i < encode.len()", "encode.len() > i");
    // Design 40.5, 42: a changed condition is `semantics`. Design 46: rule 3c forwards the read
    // of `i` along each path, also across the two calls of the `const fn` (the `opaque` call of
    // the driver and `len`). Before 46, rule 3c2 took one call only, and the rewrite was
    // `code different`.
    assert!(out.contains("[semantics · code equal]") && !out.contains("code different]"), "{out}");
    // Since fix round 1 of design 46, rule 3c forwards the copy of `index` into both reads of
    // the overflow check, also across the call of `valid_up_to` (no pointer to `index` exists):
    // equal code. Before, the copy had two reads, and the rewrite was `code different`.
    let out = rules_5u_probe(repo, "index += e.valid_up_to();", "index = index + (e.valid_up_to());");
    assert!(out.contains("[semantics · code equal]") && !out.contains("code different]"), "{out}");
    // Negative tests: the callee can write the place. Each of these steps changes the result
    // of the function. No fact is `neutral`.
    for (old, new) in [
        // The place is behind a `&mut` reference.
        ("*i < bump(i)", "bump(i) > *i"),
        // The body takes the address of the local.
        ("i < bump(&mut i)", "bump(&mut i) > i"),
        // The place is in a `Cell`.
        ("i.get() < tick(i)", "tick(i) > i.get()"),
        // A closure has the local by mutable reference.
        ("f(1) < f(2)", "f(2) > f(1)"),
    ] {
        let out = rules_5u_probe(repo, old, new);
        assert!(!out.contains("code equal"), "{old}: {out}");
        assert!(out.contains("[semantics · code different]"), "{old}: {out}");
    }
}

#[test]
fn a_branch_on_not_equal_is_a_branch_on_equal_only_for_a_primitive_type() {
    // Batch 5u, rule 3e (part F1): `a != b` and `!(a == b)` are the same branch for a
    // primitive type (also for a float with NaN).
    let dir = rules_5u_setup();
    let repo = &dir.path().join("repo");
    for (name, ty) in [("differ", "u64"), ("differ_f", "f64")] {
        let old = format!("pub fn {name}(a: {ty}, b: {ty}) -> u64 {{\n    if a != b {{ 1 }} else {{ 2 }}");
        let new = format!("pub fn {name}(a: {ty}, b: {ty}) -> u64 {{\n    if !(a == b) {{ 1 }} else {{ 2 }}");
        let out = rules_5u_probe(repo, &old, &new);
        assert!(out.contains("code equal]") && !out.contains("code different]"), "{out}");
        // Negative test: the opposite condition is a real change.
        let new = format!("pub fn {name}(a: {ty}, b: {ty}) -> u64 {{\n    if a == b {{ 1 }} else {{ 2 }}");
        let out = rules_5u_probe(repo, &old, &new);
        assert!(!out.contains("code equal"), "{out}");
    }
    // Negative test: a type with a `PartialEq` impl. The `ne` function of the impl is not the
    // opposite of its `eq` function. The step changes the result.
    let old = "pub fn differ_odd(a: &Odd, b: &Odd) -> u64 {\n    if a != b { 1 } else { 2 }";
    let new = "pub fn differ_odd(a: &Odd, b: &Odd) -> u64 {\n    if !(a == b) { 1 } else { 2 }";
    let out = rules_5u_probe(repo, old, new);
    assert!(!out.contains("code equal"), "{out}");
}

/// The functions for the tests of rule 3c along each path (design 46). The functions `n_*` read
/// the copy `t` of `s` after a write of `s`.
const RULES_46: &str = "
pub fn h46(x: u32) -> u32 {
    x ^ 5
}

pub fn bump46(x: &mut u32) {
    *x = x.wrapping_add(1);
}

pub fn look46(x: &u32) -> u32 {
    *x + 1
}

pub fn pair46(x: u32, y: u32) -> u32 {
    x ^ y
}

pub fn ends(a: u32, b: u32) -> u32 {
    let s = a + b;
    let t = s * 2;
    t - a
}

pub fn ends_q(a: u32, b: u32, o: Option<u32>) -> Option<u32> {
    let s = a + b;
    let t = s * 2;
    Some(t - a)
}

pub fn late_borrow(a: u32, b: u32) -> u32 {
    let x = a ^ b;
    let y = pair46(x, a + b);
    y
}

pub async fn later(a: u32, b: u32) -> u32 {
    let s = a + b;
    let t = s * 2;
    t - b
}

pub fn n_write(a: u32, b: u32) -> u32 {
    let mut s = a + b;
    let t = s;
    s = 9;
    t + 1
}

pub fn n_call(a: u32, b: u32) -> u32 {
    let mut s = a + b;
    let t = s;
    s = h46(b);
    t + 2
}

pub fn n_arm(a: u32, b: u32, c: bool) -> u32 {
    let mut s = a + b;
    let t = s;
    if c {
        s = 0;
    }
    t + 3
}

pub fn n_loop(a: u32, b: u32) -> u32 {
    let mut s = a + b;
    let t = s;
    for i in 0..b {
        s = s.wrapping_add(i);
    }
    t + 4
}

pub fn n_borrow(a: u32, b: u32) -> u32 {
    let mut s = a + b;
    let t = s;
    bump46(&mut s);
    t + 5
}

pub fn n_cycle(a: u32, b: u32) -> u32 {
    let mut s = a + b;
    let t = s;
    let mut out = 0;
    for _ in 0..b {
        out ^= t;
        s ^= 1;
    }
    out ^ s
}
";

#[test]
fn a_copy_moves_to_its_use_along_each_path_only_when_no_path_changes_its_value() {
    // Design 46: rule 3c forwards a copy along each path from the copy to the use, not only in
    // one block. Then an added statement with a block end (an overflow check, a `?`) between a
    // binding and its use does not change the canonical text of the binding and the use.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", &format!("{FEES}{RULES_46}"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "rules"]);
    let probe = |old: &str, new: &str| -> String {
        let base = format!("{FEES}{RULES_46}");
        assert!(base.contains(old), "{old}");
        write(repo, "crates/shop-core/src/lib.rs", &base.replacen(old, new, 1));
        let out = run(repo, &["step", "probe"], 0);
        run(repo, &["undo"], 0);
        out
    };
    // An added `let` with an overflow check, an added `?`, and a borrow after the last use.
    for (old, new) in [
        ("    let s = a + b;\n    let t = s * 2;\n    t - a\n", "    let s = a + b;\n    let k = b + 7;\n    let t = s * 2;\n    t - k\n"),
        ("    let s = a + b;\n    let t = s * 2;\n    Some(t - a)\n", "    let s = a + b;\n    let _k = o?;\n    let t = s * 2;\n    Some(t - a)\n"),
        ("    let y = pair46(x, a + b);\n    y\n", "    let y = pair46(x, a + b);\n    y + look46(&x)\n"),
    ] {
        let out = probe(old, new);
        assert!(out.contains("[semantics · code different]") && !out.contains("[indirect") && !out.contains("code equal"), "{new}: {out}");
    }
    // The needles: the old code reads the copy `t` of `s` after a write of `s`, and the new
    // code inlines `t` and reads `s`. A write in a statement, by a call, in one arm, and in a
    // loop (condition 2); a borrow (condition 3); in `n_cycle`, a write after the use in the
    // block of the use, in a loop that does not run the copy (a cycle). A forward past the
    // write makes the two sides equal, and the inline gets `code equal`.
    for (old, new) in [
        ("    let t = s;\n    s = 9;\n    t + 1\n", "    s = 9;\n    s + 1\n"),
        ("    let t = s;\n    s = h46(b);\n    t + 2\n", "    s = h46(b);\n    s + 2\n"),
        ("    let t = s;\n    if c {\n        s = 0;\n    }\n    t + 3\n", "    if c {\n        s = 0;\n    }\n    s + 3\n"),
        ("    let t = s;\n    for i in 0..b {\n        s = s.wrapping_add(i);\n    }\n    t + 4\n", "    for i in 0..b {\n        s = s.wrapping_add(i);\n    }\n    s + 4\n"),
        ("    let t = s;\n    bump46(&mut s);\n    t + 5\n", "    bump46(&mut s);\n    s + 5\n"),
        ("    let t = s;\n    let mut out = 0;\n    for _ in 0..b {\n        out ^= t;\n", "    let mut out = 0;\n    for _ in 0..b {\n        out ^= s;\n"),
    ] {
        let out = probe(old, new);
        assert!(out.contains("inlined variable `t` = `s`") && out.contains("code different]") && !out.contains("code equal"), "{new}: {out}");
    }
    // Condition 5: inline assembly on a path and a coroutine body stop the rule. The binding
    // stays on one side, so the unchanged statements keep an `indirect` fact.
    let out = probe("    let s = a + b;\n    let t = s * 2;\n    t - a\n", "    let s = a + b;\n    unsafe { core::arch::asm!(\"nop\") };\n    let t = s * 2;\n    t - a\n");
    assert!(out.contains("[indirect · code different]") && !out.contains("code equal"), "{out}");
    let out = probe("    let s = a + b;\n    let t = s * 2;\n    t - b\n", "    let s = a + b;\n    let k = b + 7;\n    let t = s * 2;\n    t - k\n");
    assert!(out.contains("[indirect · code different]") && !out.contains("code equal"), "{out}");
}

const RULES_46F: &str = "
pub fn note46(x: u32) {
    std::hint::black_box(x);
}

pub fn weight46(x: u32, y: u32) -> u32 {
    x ^ y
}

pub fn scale(a: u32, b: u32) -> u32 {
    let s = a + b;
    note46(s);
    s * 2
}

pub fn count(a: usize, v: &[u32]) -> usize {
    let n = a / 2;
    let mut i = 0;
    let mut t = 0;
    while i < n {
        t += v[i] as usize;
        i += 1;
    }
    t
}

pub fn best(a: u32, b: u32, c: u32) -> u32 {
    let t = a + b + 1;
    let u = weight46(c, a);
    core::cmp::min(t, u)
}

pub fn twice(a: u32, b: u32) -> u32 {
    let mut t = (a, b);
    let n = t.0;
    note46(n);
    t.1 = 3;
    n + 1 + t.1
}

pub fn alias(a: u32) -> u32 {
    let s = a / 2;
    s ^ 1
}
";

#[test]
fn a_binding_that_rule_3c_forwards_on_one_side_keeps_the_cause_of_its_edit() {
    // Fix round 1 of design 46. Form A: an added or deleted read of a binding changed the count
    // of its uses, and rule 3c forwarded the kept use on one side only. Form B: a changed init
    // makes the binding a copy of a place on one side only; test 2b of the dependence gives
    // the use the causes of the changed locals.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = format!("{FEES}{RULES_46F}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "rules");
    let probe = |old: &str, new: &str| -> String {
        assert!(base.contains(old), "{old}");
        write(repo, "crates/shop-core/src/lib.rs", &base.replacen(old, new, 1));
        let out = run(repo, &["step", "probe"], 0);
        run(repo, &["undo"], 0);
        out
    };
    for (old, new) in [
        // Form A: a deleted and a changed read (a call argument).
        ("    note46(s);\n    s * 2", "    s * 2"),
        ("    note46(s);\n    s * 2", "    note46(a);\n    s * 2"),
        // Form B: a division and an operator with an overflow check; an operator and a call.
        ("let n = a / 2;", "let n = a * 2;"),
        ("let t = a + b + 1;", "let t = weight46(a, b);"),
    ] {
        let out = probe(old, new);
        assert!(out.contains("[semantics · code different]") && !out.contains("[indirect") && !out.contains("code equal"), "{new}: {out}");
    }
    // The needles: a write of the source place `t.0` between two uses of `n`. Form A forwards
    // `n` into the first use only. The inline form reads `t.0` after the write: a forward past
    // the write would make the two sides equal.
    let out = probe("    note46(n);\n    t.1 = 3;", "    note46(n);\n    t.0 = 9;\n    t.1 = 3;");
    assert!(out.contains("added statement `t.0 = 9`") && out.contains("[semantics · code different]") && !out.contains("code equal"), "{out}");
    let out = probe("    let n = t.0;\n    note46(n);\n    t.1 = 3;\n    n + 1", "    note46(t.0);\n    t.0 = 9;\n    t.1 = 3;\n    t.0 + 1");
    assert!(out.contains("code different]") && !out.contains("code equal"), "{out}");
    // Safety: an added alias `let s = t;` shadows `s`. Rule 3c forwards it into the use, so it
    // has no element, and the use has the cause of a different edit (here rule 1 of the edit
    // `1` → `2`). The alias keeps the doubt rule: not `code equal`.
    let out = probe("    let s = a / 2;\n    s ^ 1", "    let s = a / 2;\n    let t = a ^ 5;\n    let s = t;\n    s ^ 2");
    assert!(out.contains("added `let` `let s = t;` at `shop_core::alias/S2` [semantics · code different]") && !out.contains("code equal"), "{out}");
    let out = probe("    let s = a / 2;\n    s ^ 1", "    let s = a / 3;\n    let t = a ^ 5;\n    let s = t;\n    s ^ 1");
    assert!(out.contains("added `let` `let s = t;` at `shop_core::alias/S2` [semantics · code different]") && !out.contains("code equal"), "{out}");
}

#[test]
fn an_edit_in_code_that_no_configuration_compiles_needs_a_reason() {
    // Batch 5w, part A (trial 7 on humantime). A constant in a module with a `cfg` condition
    // that is false in each checked configuration got `neutral (proved)`. "No body changed"
    // is evidence only for code that a checked configuration compiles.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let lib = "pub mod cart;\n\n#[cfg(target_os = \"none\")]\nmod win;\n\n#[cfg(not(target_os = \"none\"))]\nmod nix;\n\n#[cfg(target_pointer_width = \"16\")]\nmod small {\n    pub const LIMIT: u64 = 7;\n    pub static NAME: &str = \"small\";\n    pub type Word = u16;\n    pub fn twice(a: u64) -> u64 {\n        a * 2\n    }\n    pub fn half(a: u64) -> u64 {\n        a / 2\n    }\n    pub struct Pair {\n        pub a: u32,\n        pub b: u32,\n    }\n    pub enum Kind {\n        One,\n        Two,\n    }\n    impl Pair {\n        pub fn sum(&self) -> u32 {\n            self.a + self.b\n        }\n    }\n    use std::collections::BTreeMap;\n    macro_rules! double {\n        ($e:expr) => {\n            $e * 2\n        };\n    }\n    pub mod inner {\n        pub fn deep() -> u32 {\n            3\n        }\n    }\n    pub fn map() -> BTreeMap<u32, u32> {\n        let mut m = BTreeMap::new();\n        m.insert(1, double!(2));\n        m\n    }\n}\n";
    let file = "pub const SEP: char = '/';\n\npub fn join(a: &str, b: &str) -> String {\n    format!(\"{a}{SEP}{b}\")\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", lib);
    write(repo, "crates/shop-core/src/win.rs", file);
    write(repo, "crates/shop-core/src/nix.rs", file);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "code under conditions"]);

    // Each edit of the list in one step: each fact has the spotlight, and no fact is neutral.
    let edits: &[(&str, &str, &str)] = &[
        ("LIMIT: u64 = 7", "LIMIT: u64 = 8", "shop_core::small::LIMIT"),
        ("= \"small\"", "= \"tiny\"", "shop_core::small::NAME"),
        ("type Word = u16", "type Word = u8", "shop_core::small::Word"),
        ("a * 2", "a * 3", "shop_core::small::twice/tail/rhs"),
        ("half(a: u64) -> u64", "half(a: u64) -> u32", "shop_core::small::half/sig/ret"),
        ("pub a: u32", "pub a: u64", "shop_core::small::Pair.a"),
        ("        Two,\n", "        Two,\n        Three,\n", "shop_core::small::Kind::Three"),
        ("self.a + self.b", "self.a - self.b", "shop_core::small::Pair::sum/tail"),
        ("use std::collections::BTreeMap;", "use std::collections::{BTreeMap, BTreeSet};", "shop_core::small::use#0"),
        ("$e * 2", "$e * 3", "shop_core::small::double!"),
        ("            3\n", "            4\n", "shop_core::small::inner::deep/tail"),
    ];
    let mut text = lib.to_string();
    for (old, new, _) in edits {
        assert!(text.contains(old), "{old}");
        text = text.replacen(old, new, 1);
    }
    write(repo, "crates/shop-core/src/lib.rs", &text);
    write(repo, "crates/shop-core/src/win.rs", &file.replace("'/'", "'\\\\'"));
    let out = run(repo, &["step", "tidy the small module"], 1);
    // Design 37.13: the text is equal for each fact. The first fact shows the text and the
    // fact numbers. Each other fact names the first fact. Each fact keeps its reason key.
    let cause = "inactive code: no checked configuration compiles the changed code (`#[cfg(target_pointer_width = \"16\")]` at crates/shop-core/src/lib.rs:9), so Semilla did not compare the edit (the same text for f1 to f6, f8 to f12); it needs a reason: --reason";
    assert_eq!(out.matches(cause).count(), 1, "{out}");
    for (_, _, key) in edits {
        assert!(out.contains(&format!("{cause} '{key}=…'")) || out.contains(&format!("      ✗ spotlight: the same text as f1; it needs a reason: --reason '{key}=…'")), "{key}\n{out}");
    }
    // The file module: the attribute is in a different file.
    // The file has one spotlight, and the key of the reason is the file (design 37.3).
    assert!(out.contains("(no checked unit reads the file `crates/shop-core/src/win.rs`: `#[cfg(target_os = \"none\")]` at `shop_core::win`), so Semilla did not compare the edit; it needs a reason: --reason 'crates/shop-core/src/win.rs=…'"), "{out}");
    assert!(out.contains("value of `shop_core::win::SEP` changed: ") && out.contains("[semantics · no compare]") && !out.contains("--reason 'shop_core::win::SEP"), "{out}");
    assert!(!out.contains("code equal"), "{out}");

    // Design 37.19: eleven open spotlights have the kind `inactive-code`. The command to run
    // again has one kind reason for them, and no key of an item of the kind.
    let again = out.lines().find(|line| line.starts_with("run again: ")).unwrap_or_else(|| panic!("{out}"));
    assert_eq!(again, "run again: semilla step 'tidy the small module' --reason 'kind:inactive-code=<why>'", "{out}");
    // A kind with no open spotlight, a name that is no kind, and the build code kind: each
    // reason gets the usual warning, and the command to run again omits it.
    let out = run(repo, &["step", "tidy the small module", "--reason", "kind:cfg-attribute=a", "--reason", "kind:other=b", "--reason", "kind:build-code=c"], 1);
    assert!(out.contains("  ! the reason for `kind:cfg-attribute` belongs to no fact of this step; Semilla does not use it. No open spotlight of this step has the kind `cfg-attribute`\n"), "{out}");
    assert!(out.contains("  ! the reason for `kind:other` belongs to no fact of this step; Semilla does not use it. `other` is not the name of a kind. The kinds: `cfg-attribute`, "), "{out}");
    assert!(out.contains("  ! the reason for `kind:build-code` belongs to no fact of this step; Semilla does not use it. A kind reason does not cover build code: each changed file of build code needs its own reason\n"), "{out}");
    assert!(out.ends_with("run again: semilla step 'tidy the small module' --reason 'kind:inactive-code=<why>'\n"), "{out}");
    // The kind reason covers each spotlight of the kind. The exact key of a fact goes first.
    // Each covered spotlight says how many spotlights the reason covers.
    let out = run(repo, &["step", "tidy the small module", "--reason", "kind:inactive-code=the 16 bit target is not in the checks", "--reason", "shop_core::small::LIMIT=the limit of the small target is 8"], 0);
    assert!(out.starts_with("accepted: "), "{out}");
    // Design 38.4: a run of facts with an equal spotlight line shows the line one time.
    // Design 54.6: the groups of the facts make five runs (before the groups: four).
    assert_eq!(out.matches("; reason (key `kind:inactive-code`; one reason covers 11 spotlights of the kind inactive-code): \"the 16 bit target is not in the checks\"\n").count(), 5, "{out}");
    assert!(out.contains("; reason: \"the limit of the small target is 8\"\n"), "{out}");
    let md = repo.join(".semilla/review.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let report = std::fs::read_to_string(&md).unwrap();
    assert!(report.matches("one reason covers 11 spotlights of the kind inactive-code").count() >= 11, "{report}");
    run(repo, &["undo"], 0);

    // One edit with one reason: the step is accepted, and the fact is to read.
    write(repo, "crates/shop-core/src/lib.rs", &lib.replace("LIMIT: u64 = 7", "LIMIT: u64 = 8"));
    write(repo, "crates/shop-core/src/win.rs", file);
    let out = run(repo, &["step", "a new limit", "--reason", "shop_core::small::LIMIT=the limit of the small target is 8"], 0);
    assert!(out.contains("value of `shop_core::small::LIMIT` changed: 7 → 8 [semantics · no compare]") && out.contains("reason: \"the limit of the small target is 8\""), "{out}");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-a", "-m", "a new limit"]);
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("1 fact: 1 no compare (1 with a spotlight)"), "{out}");

    // Negative: a comment in the dead module, and an edit in the file module that a checked
    // configuration compiles. The step needs no reason.
    let now = std::fs::read_to_string(repo.join("crates/shop-core/src/lib.rs")).unwrap();
    write(repo, "crates/shop-core/src/lib.rs", &now.replace("    pub const LIMIT", "    // The limit of the small target.\n    pub const LIMIT"));
    write(repo, "crates/shop-core/src/nix.rs", &file.replace("{a}{SEP}{b}", "{b}{SEP}{a}"));
    let out = run(repo, &["step", "a comment and a live edit"], 0);
    assert!(!out.contains("spotlight") && out.contains("affected bodies: shop_core::nix::join"), "{out}");
}

#[test]
fn tag_after_a_rejected_step_says_the_cause() {
    // Batch 5x, part C (trial 7): after a rejected step and a commit, `tag` said "run
    // `semilla step`": the same step that failed. Now it says the cause and the command.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", FEES);
    commit_all(repo, "fees");
    // The first step of the change set is rejected: a spotlight has no reason.
    let unix = FEES.replace("    BASE_FEE + percent", "    #[cfg(not(unix))]\n    return 0;\n    BASE_FEE + percent");
    write(repo, "crates/shop-core/src/lib.rs", &unix);
    let out = run(repo, &["step", "only unix"], 1);
    assert!(out.contains("run again: semilla step 'only unix' --reason 'shop_core::charge=<why>'"), "{out}");
    commit_all(repo, "x");
    // Design 62: the session has no stored step, so the commit gets a tag after the commit.
    // The spotlight is open, and the last line gives the command that adds the reason.
    let tag = semilla(repo, &["tag"]);
    let text = String::from_utf8_lossy(&tag.stdout).to_string();
    assert_eq!(tag.status.code(), Some(1), "{text}");
    assert!(text.starts_with("result: tagged after the commit · 1 step · ") && text.contains("1 open spotlight"), "{text}");
    assert!(text.trim_end().ends_with("--reason 'shop_core::charge=<why>'"), "{text}");
    let fix = "The last `semilla step` (\"only unix\") was rejected, and Semilla stored nothing: 1 spotlight with no reason. Run it again with the missing parts: `semilla step 'only unix' --reason 'shop_core::charge=<why>'`, then `git commit --amend --no-edit`, then `semilla tag`.";
    // The same after a stored step: `tag` and `status` say it.
    git(repo, &["reset", "-q", "--soft", "HEAD^"]);
    write(repo, "crates/shop-core/src/lib.rs", &FEES.replace("amount * rate / 100\n}\n\npub fn scaled", "amount * rate / 10\n}\n\npub fn scaled"));
    run(repo, &["step", "a new rate"], 0);
    write(repo, "crates/shop-core/src/lib.rs", &unix);
    run(repo, &["step", "only unix"], 1);
    commit_all(repo, "x");
    let tag = semilla(repo, &["tag"]);
    let text = String::from_utf8_lossy(&tag.stderr).to_string();
    assert!(!tag.status.success() && text.contains("an edit after the last step has no step. ") && text.contains(fix), "{text}");
    let (_, out) = run_env(repo, &["status"], &[]);
    assert!(out.contains("✗ `semilla tag` rejects this state: the last step ends at tree") && out.contains(fix), "{out}");
    // The step with the reason, then the commit: `tag` accepts, and the record is gone.
    run(repo, &["step", "only unix", "--reason", "shop_core::charge/S0=the shop has only unix hosts"], 0);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--amend", "--no-edit", "-a"]);
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted") && !out.contains("was rejected"), "{out}");
    // An edit with no step and no rejected step keeps the old text.
    let now = unix.replace("/ 100\n}\n\npub fn scaled", "/ 10\n}\n\npub fn scaled");
    write(repo, "crates/shop-core/src/lib.rs", &now.replace("charge(amount, 1)", "charge(amount, 2)"));
    run(repo, &["step", "a new rate of the fee"], 0);
    write(repo, "crates/shop-core/src/lib.rs", &now.replace("charge(amount, 1)", "charge(amount, 3)"));
    let (_, out) = run_env(repo, &["status"], &[]);
    assert!(out.contains("The edit has no step. Run `semilla step \"<intent>\"` for the edit.") && !out.contains("was rejected"), "{out}");
}

#[test]
fn a_module_file_that_git_ignores_has_its_cause_in_the_build_error() {
    // Batch 5x, part C (trial 7): the build said only `E0583`. The file is in the folder, and
    // git ignores it: Semilla builds a tree of git.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", FEES);
    commit_all(repo, "fees");
    let note = "`crates/shop-core/src/extra.rs` is in the folder, but git ignores it (`git check-ignore -v crates/shop-core/src/extra.rs` shows the rule). Semilla builds only files that git tracks or that are not ignored.";
    for rules in [".git/info/exclude", ".gitignore"] {
        let old = std::fs::read_to_string(repo.join(rules)).unwrap_or_default();
        write(repo, rules, &format!("{old}extra.rs\n"));
        if rules == ".gitignore" {
            commit_all(repo, "ignore");
        }
        write(repo, "crates/shop-core/src/lib.rs", &format!("pub mod extra;\n{FEES}"));
        write(repo, "crates/shop-core/src/extra.rs", "pub fn extra() -> u32 {\n    1\n}\n");
        let out = run(repo, &["step", "a new module"], 1);
        assert!(out.contains("E0583") && out.contains(note), "{rules}: {out}");
        // With no rule, the step has the file.
        std::fs::remove_file(repo.join("crates/shop-core/src/extra.rs")).unwrap();
        write(repo, "crates/shop-core/src/lib.rs", FEES);
        write(repo, rules, &old);
        if rules == ".gitignore" {
            commit_all(repo, "no ignore");
        }
        write(repo, "crates/shop-core/src/lib.rs", &format!("pub mod extra;\n{FEES}"));
        write(repo, "crates/shop-core/src/extra.rs", "pub fn extra() -> u32 {\n    1\n}\n");
        let out = run(repo, &["step", "a new module"], 0);
        assert!(out.contains("added `shop_core::extra`") && !out.contains("git ignores it"), "{rules}: {out}");
        run(repo, &["undo"], 0);
        std::fs::remove_file(repo.join("crates/shop-core/src/extra.rs")).unwrap();
        write(repo, "crates/shop-core/src/lib.rs", FEES);
    }
    // A module with no file in the folder: the build error has no note.
    write(repo, "crates/shop-core/src/lib.rs", &format!("pub mod extra;\n{FEES}"));
    let out = run(repo, &["step", "a new module"], 1);
    assert!(out.contains("E0583") && !out.contains("is in the folder"), "{out}");
}

#[test]
fn a_reason_with_an_item_path_is_enough_in_the_first_call() {
    // Batch 5x, part D (trial 7): the key of a reason was only in the rejection.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", FEES);
    commit_all(repo, "fees");
    // Two spotlights in one function.
    let unix = FEES.replace("    BASE_FEE + percent", "    #[cfg(not(unix))]\n    return 0;\n    #[cfg(windows)]\n    return 1;\n    BASE_FEE + percent");
    write(repo, "crates/shop-core/src/lib.rs", &unix);
    // A crate name and a module path are not a key: the step says so, and it has no reason.
    for (key, kind) in [("shop_core", "crate"), ("shop_core::cart", "module")] {
        let out = run(repo, &["step", "only unix", "--reason", &format!("{key}=the shop has only unix hosts")], 1);
        assert!(out.contains(&format!("the reason for `{key}` belongs to no fact of this step; Semilla does not use it. `{key}` is a {kind}, and a {kind} is not a valid key of a reason. A key is the path of an item")), "{out}");
        // Each fact shows its exact key. The command to run again has one reason with the path
        // of the item, one time (batch 7b, part 5.5).
        assert!(out.contains("it needs a reason: --reason 'shop_core::charge/S0=…'") && out.contains("it needs a reason: --reason 'shop_core::charge/S1=…'"), "{out}");
        assert!(out.trim_end().ends_with(&format!("--reason '{key}=the shop has only unix hosts' --reason 'shop_core::charge=<why>'")) || out.contains("run again: semilla step 'only unix' --reason 'shop_core::charge=<why>'\n"), "{out}");
        assert_eq!(out.matches("=<why>").count(), 1, "{out}");
    }
    // An item with no spotlight: the result of an unused reason, as before this batch.
    let out = run(repo, &["step", "only unix", "--reason", "shop_core::percent=the shop has only unix hosts"], 1);
    assert!(out.contains("the reason for `shop_core::percent` belongs to no fact of this step; Semilla does not use it\n"), "{out}");
    // The path of the function covers the two spotlights. Each output shows the key.
    let out = run(repo, &["step", "only unix", "--reason", "shop_core::charge=the shop has only unix hosts"], 0);
    assert_eq!(out.matches("; reason (key `shop_core::charge`): \"the shop has only unix hosts\"").count(), 2, "{out}");
    assert!(!out.contains("belongs to no fact"), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert_eq!(out.matches("; reason (key `shop_core::charge`): \"the shop has only unix hosts\"").count(), 2, "{out}");
    let file = dir.path().join("report.md");
    run(repo, &["review", "--md", file.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches("; reason (key `shop_core::charge`): \"the shop has only unix hosts\"").count(), 2, "{text}");
}

/// Changes the session file of the repository by hand (the agent controls this file).
fn edit_session(repo: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let file = repo.join(".semilla/session.json");
    let mut session: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    change(&mut session);
    std::fs::write(&file, session.to_string()).unwrap();
}

/// Batch 5y, A1 (trial 8): `verify` and `status` with no commit read the session file. The
/// result is accepted only if the session agrees with git.
#[test]
fn verify_accepts_only_a_session_that_agrees_with_git() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let short = |rev: &str| git(repo, &["rev-parse", "--short=7", rev]).trim().to_string();
    let base = short("HEAD");
    // The normal flow: after a step, after the commit, and after `tag`.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "raise the max"], 0);
    let out = run(repo, &["verify"], 0);
    assert!(out.starts_with(&format!("verify: the base `{base}`, 1 step, the session of this folder (not a commit)\n")) && out.contains("result: accepted"), "{out}");
    commit_all(repo, "raise the max");
    let out = run(repo, &["verify"], 0);
    assert!(out.starts_with(&format!("verify: the base `{base}`, 1 step, the session of this folder (not a commit)\n")) && out.contains("result: accepted"), "{out}");
    run(repo, &["tag"], 0);
    let tagged = short("HEAD");
    let out = run(repo, &["verify"], 0);
    assert!(out.starts_with(&format!("verify: the base `{base}`, 1 step, the tagged commit `{tagged}`\n")) && out.contains("result: accepted"), "{out}");
    // `verify` takes no commit: the error gives the command for a commit.
    let output = semilla(repo, &["verify", "HEAD"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("use `semilla status HEAD`"), "{}", String::from_utf8_lossy(&output.stderr));
    // A different change in a new commit with plain `git`. The session is the old change set.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "99"));
    commit_all(repo, "a change with no step");
    let other = short("HEAD");
    let out = run(repo, &["verify"], 1);
    assert!(out.contains(&format!("the session is the closed change set of commit `{tagged}`, but HEAD is `{other}`")) && out.contains("result: rejected") && !out.contains("result: accepted"), "{out}");
    // A session file that names `HEAD` as the tagged commit. `HEAD` has no note.
    let head = git(repo, &["rev-parse", "HEAD"]);
    edit_session(repo, |session| session["tagged"] = serde_json::json!(head));
    let out = run(repo, &["verify"], 1);
    assert!(out.contains("that commit has no Semilla note") && out.contains("result: rejected") && !out.contains("result: accepted"), "{out}");
    let out = run(repo, &["status"], 0);
    assert!(out.contains(&format!("the session names commit {head}, but that commit has no Semilla check")) && !out.contains("the last tagged commit"), "{out}");
    // An open session that does not end at `HEAD` or at the working files.
    edit_session(repo, |session| {
        session.as_object_mut().unwrap().remove("tagged");
    });
    let out = run(repo, &["verify"], 1);
    assert!(out.contains("the session does not agree with git: its last step ends at tree") && out.contains("result: rejected") && !out.contains("result: accepted"), "{out}");
    // An open session with a base that no commit of `HEAD` has.
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    edit_session(repo, |session| {
        let old = session["changeset"]["steps"][0]["tree_after"].clone();
        session["changeset"]["base_tree"] = old.clone();
        session["changeset"]["steps"][0]["tree_before"] = old;
        session["changeset"]["steps"][0]["tree_after"] = serde_json::json!(tree);
        session["changeset"]["head_tree"] = serde_json::json!(tree);
    });
    let out = run(repo, &["verify"], 0);
    assert!(out.contains(&format!("verify: the base `{tagged}`, 1 step, the session of this folder (not a commit)")), "a session from a commit of the chain to HEAD agrees with git\n{out}");
    git(repo, &["reset", "-q", "--hard", &base]);
    let out = run(repo, &["verify"], 1);
    assert!(out.contains("the session does not agree with git: it starts at tree") && out.contains("no commit on the first-parent chain of HEAD has that tree") && !out.contains("result: accepted"), "{out}");
}

/// A change set with a spotlight (a new script) and its reason, in a tagged commit. Then the note
/// loses the reason: the note belongs to the commit (the trees and the step refs agree), but the
/// result that Semilla computes for it is rejected. Returns the short name of the commit.
fn commit_with_a_rejected_note(repo: &Path) -> String {
    write(repo, "ci.sh", "cargo test\n");
    run(repo, &["step", "add the CI script", "--reason", "ci.sh:=the CI runs it"], 0);
    git(repo, &["add", "-A"]);
    commit_all(repo, "add the CI script");
    run(repo, &["tag"], 0);
    let note = git(repo, &["notes", "--ref=semilla", "show", "HEAD"]);
    let mut forged: serde_json::Value = serde_json::from_str(&note).unwrap();
    assert!(forged["steps"][0]["reasons"].as_object().is_some_and(|reasons| !reasons.is_empty()), "the note has the reason: {note}");
    forged["steps"][0].as_object_mut().unwrap().remove("reasons");
    let file = repo.join(".semilla/forged.json");
    std::fs::write(&file, forged.to_string()).unwrap();
    git(repo, &["notes", "--ref=semilla", "add", "-f", "-F", file.to_str().unwrap(), "HEAD"]);
    git(repo, &["rev-parse", "--short=7", "HEAD"]).trim().to_string()
}

/// Batch 5y, A2 (trial 8): a commit has a Semilla check only if the result that this machine
/// computes for it is accepted. A note that belongs to the commit is not sufficient.
#[test]
fn a_base_with_a_rejected_check_is_not_a_checked_base() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let short = |rev: &str| git(repo, &["rev-parse", "--short=7", rev]).trim().to_string();
    // An accepted change set.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "raise the max"], 0);
    commit_all(repo, "raise the max");
    run(repo, &["tag"], 0);
    let good = short("HEAD");
    let other_cache = dir.path().join("other-cache");
    // A change set whose note Semilla rejects.
    let bad = commit_with_a_rejected_note(repo);
    // The next change set on it: the base line is a warning, and it gives the command.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "30"));
    let step = run(repo, &["step", "raise the max again"], 0);
    let line = format!("! base `{bad}`: its Semilla check is rejected. Run `semilla status {bad}`");
    assert!(step.contains(&line) && !step.contains("it has a Semilla check"), "{step}");
    // The answer is in the machine cache, and no file of the repository folder holds it.
    assert!(std::fs::read_dir(repo_cache(repo).join("commit-check")).unwrap().count() >= 1, "the cache has the answer");
    let status = run(repo, &["status", "--wait", "600"], 0);
    assert!(status.contains(&line), "{status}");
    commit_all(repo, "raise the max again");
    let tag = run(repo, &["tag"], 0);
    assert!(tag.contains(&line), "{tag}");
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains(&line) && status.contains("result: accepted"), "the line is a warning for the reviewer\n{status}");
    // On a machine with no stored result, the base of the change set on the good commit has
    // data only. After `status` of that commit, it has a check.
    let env = [("SEMILLA_CACHE_DIR", other_cache.to_str().unwrap())];
    let (code, out) = run_env(repo, &["status", &bad, "--no-wait"], &env);
    assert!(code == 1 && out.contains(&format!("base `{good}`: it has Semilla data, but this machine did not compute the full result. Run `semilla status {good}`")), "{code}\n{out}");
    let (code, out) = run_env(repo, &["status", &good], &env);
    assert!(code == 0 && out.contains("result: accepted"), "{code}\n{out}");
    let (_, out) = run_env(repo, &["status", &bad, "--no-wait"], &env);
    assert!(out.contains(&format!("base `{good}`: it has a Semilla check")), "{out}");
    // The walk over older commits uses the same rule: a commit with a rejected check is a commit
    // with no check, and the walk stops at the last commit with an accepted result.
    git(repo, &["reset", "-q", "--hard", &bad]);
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "40"));
    commit_all(repo, "a commit with no step");
    let plain = short("HEAD");
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "50"));
    let step = run(repo, &["step", "the next step"], 0);
    let line = format!("! 2 commits before this change set have no Semilla check: `{plain}` \"a commit with no step\", `{bad}` \"add the CI script\" (its Semilla check is rejected) (the last checked commit is `{good}`)");
    assert!(step.contains(&line), "{step}");
}

/// Batch 5y, A3 (trial 8): a result that is rejected or not complete has the exit code 1, and
/// the first line says `rejected`.
#[test]
fn a_rejected_result_has_the_exit_code_1() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // An accepted commit: the exit code 0, and `ready` alone in the first line.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "raise the max"], 0);
    commit_all(repo, "raise the max");
    run(repo, &["tag"], 0);
    let good = git(repo, &["rev-parse", "HEAD"]);
    let out = run(repo, &["status", "HEAD"], 0);
    assert!(out.starts_with(&format!("{good}: ready\n")) && out.contains("result: accepted"), "{out}");
    let md = dir.path().join("good.md");
    let out = run(repo, &["review", "HEAD", "--md", md.to_str().unwrap()], 0);
    assert!(out.contains("result: accepted") && md.is_file(), "{out}");
    let out = run(repo, &["load", "HEAD"], 0);
    assert!(out.contains("loaded 1 steps") && out.contains("result: accepted"), "{out}");
    // With no stored result, `status <commit> --no-wait` has no full result: the exit code 1.
    let other_cache = dir.path().join("other-cache");
    let (code, out) = run_env(repo, &["status", "HEAD", "--no-wait"], &[("SEMILLA_CACHE_DIR", other_cache.to_str().unwrap())]);
    assert!(code == 1 && out.contains("result: phase 1 ok"), "{code}\n{out}");
    // A commit with a rejected result.
    commit_with_a_rejected_note(repo);
    let bad = git(repo, &["rev-parse", "HEAD"]);
    let out = run(repo, &["status", "HEAD"], 1);
    assert!(out.starts_with(&format!("{bad}: ready, rejected\n")), "{out}");
    assert!(out.lines().any(|line| line.starts_with("result: rejected")) && !out.contains("result: accepted"), "{out}");
    let md = dir.path().join("bad.md");
    let out = run(repo, &["review", "HEAD", "--md", md.to_str().unwrap()], 1);
    assert!(out.contains("result: rejected") && md.is_file(), "the report file is there\n{out}");
    let out = run(repo, &["load", "HEAD"], 1);
    assert!(out.contains("loaded 1 steps") && out.contains("result: rejected"), "{out}");
    // The session: an edit with no step is a rejected result. A result that is not there yet
    // (phase 2 runs) keeps the exit code 0.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "30"));
    run(repo, &["step", "raise the max again", "--fast"], 0);
    let out = run(repo, &["status"], 0);
    assert!(!out.contains("result: rejected"), "{out}");
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "31"));
    let out = run(repo, &["status", "--wait", "600"], 1);
    assert!(out.starts_with("session: ready, rejected\n") && out.contains("result: rejected"), "{out}");
}

#[test]
fn an_anchor_of_a_trait_impl_accepts_the_forms_of_the_source() {
    // Batch 5y, B1 (trial 8): the agent wrote `<Type as core::convert::From<Source>>::from`.
    // Semilla said "no item … after the step", and no doc showed the name that Semilla uses.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let code = "\npub struct Grams(pub u32);\npub struct Kilos(pub u32);\npub struct Weight(pub u32);\n\nimpl From<Grams> for Weight {\n    fn from(grams: Grams) -> Weight {\n        Weight(grams.0)\n    }\n}\n\nimpl From<Kilos> for Weight {\n    fn from(kilos: Kilos) -> Weight {\n        Weight(kilos.0.wrapping_mul(1000))\n    }\n}\n";
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V1}{code}"));
    // The key has no generic arguments, and two impls match: the message lists the names.
    let (code_1, out) = run_env(repo, &["step", "add the weight types", "--claim", "no-unsafe shop_core::cart::<Weight as core::convert::From>::from"], &[]);
    assert_eq!(code_1, 1, "{out}");
    assert!(out.contains("no item `shop_core::cart::<Weight as core::convert::From>::from` after the step. More than one item matches: `shop_core::cart::<Weight as From>::from#0`, `shop_core::cart::<Weight as From>::from#1`. Use one of these names, or give the generic arguments of the trait"), "{out}");
    // A name with no match: the nearest names.
    let (_, out) = run_env(repo, &["step", "add the weight types", "--claim", "no-unsafe shop_core::cart::Weight::from"], &[]);
    assert!(out.contains("no item `shop_core::cart::Weight::from` after the step. The nearest names: `shop_core::cart::<Weight as From>::from#0`, `shop_core::cart::<Weight as From>::from#1`"), "{out}");
    // The full trait path and the generic arguments select one impl, in a claim and in a reason key.
    let (code_2, out) = run_env(repo, &["step", "add the weight types", "--claim", "no-unsafe shop_core::cart::<Weight as core::convert::From<Kilos>>::from", "--reason", "shop_core::cart::impl <Weight as From<Grams>>=the scale gives grams"], &[]);
    assert_eq!(code_2, 0, "{out}");
    assert!(out.contains("c1 verified"), "{out}");
    let out = run(repo, &["status", "--verbose"], 0);
    assert!(out.contains("trait impl `<Weight as From<Grams>>#0` added") && out.contains("trait impl `<Weight as From<Kilos>>#1` added"), "{out}");
    // Design 60: the fact has a line `control flow:` before its note.
    let grams = out.lines().skip_while(|line| !line.contains("trait impl `<Weight as From<Grams>>#0` added")).take(5).collect::<Vec<_>>().join("\n");
    assert!(grams.contains("the scale gives grams"), "{out}");
    assert_eq!(out.matches("the scale gives grams").count(), 1, "{out}");
}

#[test]
fn tag_names_the_new_files_that_the_commit_lacks() {
    // Batch 5y, B2 (trial 8): `git commit -am` does not add a new file. Then `tag` said "an edit
    // after the last step has no step", and `step` said "no change since the last step".
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod unit;\n");
    write(repo, "crates/shop-core/src/unit.rs", "pub fn unit() -> u32 {\n    1\n}\n");
    run(repo, &["step", "add the module unit"], 0);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-a", "-m", "add the module unit"]);
    let (code, out) = run_env(repo, &["tag"], &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("the commit HEAD does not have the changes of the last step in 1 file(s): `crates/shop-core/src/unit.rs`. The working files have them (`git commit -am` does not add a new file). Run `git add -A`, then `git commit --amend --no-edit`, then `semilla tag`."), "{out}");
    assert!(!out.contains("an edit after the last step has no step"), "{out}");
    // `status` gives the same text.
    let (_, out) = run_env(repo, &["status"], &[]);
    assert!(out.contains("does not have the changes of the last step in 1 file(s): `crates/shop-core/src/unit.rs`"), "{out}");
    // A real edit after the last step: the old message stays.
    write(repo, "crates/shop-core/src/unit.rs", "pub fn unit() -> u32 {\n    2\n}\n");
    let (code, out) = run_env(repo, &["tag"], &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("an edit after the last step has no step. Run `semilla step \"<intent>\"` for the edit"), "{out}");
    // The commands of the message give an accepted tag.
    write(repo, "crates/shop-core/src/unit.rs", "pub fn unit() -> u32 {\n    1\n}\n");
    git(repo, &["add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--amend", "--no-edit"]);
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted"), "{out}");
}

#[test]
fn a_manifest_step_gets_each_reason_key_one_time_in_one_round() {
    // Batch 5y, B3 (trial 8): the first rejected result gives each key that the step needs, each
    // printed key is a key that the next call accepts, and the command gives each key one time.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let member = "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nstd = []\n";
    write(repo, "crates/shop-core/Cargo.toml", &format!("{member}default = [\"std\"]\n"));
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod unit;\n");
    write(repo, "crates/shop-core/src/unit.rs", "pub fn mode(x: u32) -> u32 {\n    if cfg!(debug_assertions) { x } else { x + 1 }\n}\n");
    commit_all(repo, "a default feature");
    let root = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
    write(repo, "crates/shop-core/Cargo.toml", &format!("{member}default = []\n"));
    // Batch 7c (design 37.19): three open spotlights of one kind get their item keys. A fourth
    // spotlight of the kind gives one kind key (the last part of this test).
    write(repo, "Cargo.toml", &format!("{root}\n[profile.release]\ndebug-assertions = true\n"));
    write(repo, "crates/shop-core/src/unit.rs", "pub fn mode(x: u32) -> u64 {\n    if cfg!(debug_assertions) { x as u64 } else { x as u64 + 2 }\n}\n");
    // A file name and the text of a fact are not keys. The message gives the keys.
    let (code, out) = run_env(repo, &["step", "change the build", "--reason", "crates/shop-core/Cargo.toml=the file", "--reason", "crates/shop-core/Cargo.toml: features.default: \"std\" removed=the text"], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("`crates/shop-core/Cargo.toml` is not the key of a reason. Each fact has its own key: `crates/shop-core/Cargo.toml:features.default`, `crates/shop-core/Cargo.toml:features.default: \"std\"`"), "{out}");
    assert!(out.contains("`crates/shop-core/Cargo.toml: features.default: \"std\" removed` is not the key of a reason. Each fact has its own key: `crates/shop-core/Cargo.toml:features.default: \"std\"`"), "{out}");
    assert!(out.contains("! spotlight: features: crates/shop-core/Cargo.toml (the key of the reason: `crates/shop-core/Cargo.toml:features.default`, `crates/shop-core/Cargo.toml:features.default: \"std\"`)"), "{out}");
    // The command of the last line has each key one time.
    let again = out.lines().find(|line| line.starts_with("run again")).unwrap_or_else(|| panic!("{out}"));
    let keys: Vec<&str> = again.split("--reason '").skip(1).filter_map(|part| part.split("=<why>'").next()).collect();
    let mut unique = keys.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(keys.len(), unique.len(), "{again}");
    for key in ["crates/shop-core/Cargo.toml:features.default", "crates/shop-core/Cargo.toml:features.default: \"std\"", "Cargo.toml:profile.release.debug-assertions"] {
        assert!(keys.contains(&key), "{key}: {again}");
    }
    // The second call with these keys is accepted: the first result had each key.
    let reasons: Vec<String> = keys.iter().map(|key| format!("{key}=the build needs it")).collect();
    let mut args = vec!["step", "change the build"];
    for reason in &reasons {
        args.extend(["--reason", reason.as_str()]);
    }
    let (code, out) = run_env(repo, &args, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("accepted: step 0"), "{out}");
    // A fourth build setting in the step: the command gives one kind key, and the key is accepted.
    run(repo, &["undo"], 0);
    write(repo, "Cargo.toml", &format!("{root}\n[profile.release]\ndebug-assertions = true\noverflow-checks = false\n"));
    let (code, out) = run_env(repo, &["step", "change the build"], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("run again: semilla step 'change the build' --reason 'kind:build-setting=<why>'\n"), "{out}");
    let (code, out) = run_env(repo, &["step", "change the build", "--reason", "kind:build-setting=the build needs it"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("accepted: step 0"), "{out}");
    // Design 38.4: the equal spotlight line of the run of 4 manifest facts shows one time.
    assert_eq!(out.matches("one reason covers 4 spotlights of the kind build-setting").count(), 1, "{out}");
    assert!(out.contains("      spotlight of f"), "{out}");
}

#[test]
fn a_review_after_a_step_uses_the_stored_result_of_the_step() {
    // Batch 7c, part 6.2 (design 37.21): `semilla review --md` after an accepted step does not
    // check the step again. The report is the report of a new check.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod unit;\n");
    write(repo, "crates/shop-core/src/unit.rs", "pub fn mode(x: u32) -> u32 {\n    x + 1\n}\n");
    commit_all(repo, "a unit module");
    write(repo, "crates/shop-core/src/unit.rs", "pub fn mode(x: u32) -> u32 {\n    x + 2\n}\n\npub fn two() -> u32 {\n    2\n}\n");
    let (code, out) = run_env(repo, &["step", "add a function"], &[]);
    assert_eq!(code, 0, "{out}");
    let review = |name: &str| {
        let file = dir.path().join(name);
        let (code, out) = run_env(repo, &["review", "--md", file.to_str().unwrap(), "--wait", "600"], &[("SEMILLA_TIMING", "1")]);
        assert_eq!(code, 0, "{out}");
        (out, std::fs::read_to_string(&file).unwrap().replace(name, "FILE"))
    };
    let (out, stored) = review("review.md");
    assert!(!out.contains("verify the change set"), "the review must not check the step again: {out}");
    assert!(stored.contains("x + 2"), "{stored}");
    assert!(out.contains("result: accepted · 1 step"), "{out}");
    // With no stored data, the review checks the step. The report is the same.
    std::fs::remove_dir_all(repo_cache(repo).join("review-data")).unwrap();
    let (out, fresh) = review("fresh.md");
    assert!(out.contains("verify the change set"), "{out}");
    assert_eq!(stored, fresh);
    // That review stored its data: the next review does no check.
    let (out, again) = review("again.md");
    assert!(!out.contains("verify the change set"), "{out}");
    assert_eq!(stored, again);
    // A second step: the stored data of one step is not the data of two steps. The background
    // check stores the new data, and the review shows the two steps.
    write(repo, "crates/shop-core/src/unit.rs", "pub fn mode(x: u32) -> u32 {\n    x + 3\n}\n\npub fn two() -> u32 {\n    2\n}\n");
    let (code, out) = run_env(repo, &["step", "a new value"], &[]);
    assert_eq!(code, 0, "{out}");
    let (out, two) = review("two.md");
    assert!(out.contains("result: accepted · 2 steps"), "{out}");
    assert!(two.contains("x + 3") && two.contains("a new value"), "{two}");
    let (out, last) = review("last.md");
    assert!(!out.contains("verify the change set"), "{out}");
    assert_eq!(two, last);
}

#[test]
fn a_reviewer_output_names_a_configuration_of_only_some_steps() {
    // Batch 5y, C4.4 and C4.5 (trial 8). Step 1 changes the default features: only this step
    // has the configuration `all features`. `status HEAD` had no line for it.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let member = "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nstd = []\n";
    write(repo, "crates/shop-core/Cargo.toml", &format!("{member}default = [\"std\"]\n"));
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod unit;\n");
    write(repo, "crates/shop-core/src/unit.rs", "pub fn mode(x: u32) -> u32 {\n    x + 1\n}\n");
    commit_all(repo, "a default feature");
    write(repo, "crates/shop-core/src/unit.rs", "pub fn mode(x: u32) -> u32 {\n    x + 1\n}\n\npub fn two() -> u32 {\n    2\n}\n");
    let (code, out) = run_env(repo, &["step", "add a function"], &[]);
    assert_eq!(code, 0, "{out}");
    write(repo, "crates/shop-core/Cargo.toml", &format!("{member}default = []\n"));
    let key = "crates/shop-core/Cargo.toml:features.default";
    let (code, out) = run_env(repo, &["step", "no default feature", "--reason", &format!("{key}=the library needs no std"), "--reason", &format!("{key}: \"std\"=the library needs no std")], &[]);
    assert_eq!(code, 0, "{out}");
    commit_all(repo, "add a function, no default feature");
    run(repo, &["tag"], 0);
    let (_, out) = run_env(repo, &["status", "HEAD"], &[]);
    let checked = out.lines().find(|line| line.starts_with("checked configurations")).unwrap_or_else(|| panic!("{out}"));
    assert!(checked.contains("`dev, all features` (only in step 1)") && checked.contains("`release, all features` (only in step 1)"), "{out}");
    // One line says why step 0 does not have the configuration.
    assert!(out.contains("not in the list, because the features are the same (also for `release`): `dev, all features` = `dev` in step 0\n"), "{out}");
    // C4.5: the hint for a commit names the commit.
    assert!(out.contains("run `semilla status HEAD --verbose`"), "{out}");
    assert!(!out.contains("`semilla status --verbose`"), "{out}");
}

#[test]
fn a_call_through_an_extern_crate_item_has_the_name_of_its_crate() {
    // Batch 5y, C4 (trial 8): in a `no_std` crate with `extern crate std;`, the external call
    // had the name `data_encoding::std::fs::metadata`: the local crate before a different crate.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/bare/Cargo.toml", "[package]\nname = \"bare\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    write(repo, "crates/bare/src/lib.rs", "#![no_std]\nextern crate std;\n\npub fn size(path: &str) -> u64 {\n    std::fs::metadata(path).map(|data| data.len()).unwrap_or(0)\n}\n");
    let (code, out) = run_env(repo, &["step", "add the crate bare", "--claim", "no-io bare::size"], &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("external calls: `std::fs::Metadata::len` in `bare::size`, `std::fs::metadata` in `bare::size`"), "{out}");
    assert!(!out.contains("bare::std::"), "{out}");
}

#[test]
fn a_doc_test_fence_change_gives_no_indirect_fact() {
    // Batch 5y, C4 (trial 8): the item has a macro call that depends on the build. A change of
    // the fence of its doc test gave a second fact: "indirect change, cause not known: … (the
    // MIR changed, but the text did not)". The MIR did not change, and the code text did not.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let unit = "use core::debug_assert as safety_assert;\n\n/// Returns one.\n///\n/// ```rust\n/// assert_eq!(shop_core::unit::one(), 1);\n/// ```\npub fn one() -> u32 {\n    safety_assert!(1 == 1);\n    1\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod unit;\n");
    write(repo, "crates/shop-core/src/unit.rs", unit);
    commit_all(repo, "a function with a doc test");
    write(repo, "crates/shop-core/src/unit.rs", &unit.replace("```rust", "```text"));
    let (code, out) = run_env(repo, &["step", "make the example a text block"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("doc test of `shop_core::unit::one` changed"), "{out}");
    assert!(!out.contains("indirect change") && !out.contains("needs a reason"), "{out}");
    // An edit of the code of the same item gives a fact with the compare result.
    write(repo, "crates/shop-core/src/unit.rs", &unit.replace("```rust", "```text").replace("1 == 1", "2 == 2"));
    let (code, out) = run_env(repo, &["step", "a different check"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[macro · code different]") && out.contains("affected bodies: shop_core::unit::one") && !out.contains("needs a reason"), "{out}");
}

/// Batch 6a, B3 (trial 9): the cache folder of a repository has the path of the repository in
/// its name. A copy of a tagged repository at a different path, with the same cache folder of
/// the user, finds the result of its base by content: the commit, the note, and the tools. A
/// base that this machine did not check keeps the line of batch 5y.
#[test]
fn a_copy_at_a_different_path_finds_the_result_of_its_base() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "raise the max"], 0);
    commit_all(repo, "raise the max");
    run(repo, &["tag"], 0);
    let good = git(repo, &["rev-parse", "--short=7", "HEAD"]);
    // The copy has the same parent folder, so it has the same cache folder of the user.
    let copy = &dir.path().join("copy");
    assert!(Command::new("cp").arg("-r").arg(repo).arg(copy).status().unwrap().success());
    write(copy, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "30"));
    let checked = format!("base `{good}`: it has a Semilla check");
    let data_only = format!("base `{good}`: it has Semilla data, but this machine did not compute the full result. Run `semilla status {good}`");
    let step = run(copy, &["step", "raise the max again"], 0);
    assert!(step.contains(&checked) && !step.contains("did not compute"), "{step}");
    // The copy has its own cache folder: two folders with a `repository` file.
    let folders = std::fs::read_dir(cache_dir(repo)).unwrap().flatten().filter(|entry| entry.path().join("repository").is_file()).count();
    assert_eq!(folders, 2, "the copy has a different cache folder");
    // A machine with a different cache did not compute the result: the line of batch 5y stays.
    let other_cache = dir.path().join("other-cache");
    let (_, out) = run_env(copy, &["status", "--no-wait"], &[("SEMILLA_CACHE_DIR", other_cache.to_str().unwrap())]);
    assert!(out.contains(&data_only) && !out.contains(&checked), "{out}");
    // `--fresh` reads no cache.
    let (_, out) = run_env(copy, &["status", "--no-wait", "--fresh"], &[]);
    assert!(out.contains(&data_only) && !out.contains(&checked), "{out}");
    // A note with a different content is not the note that this machine did check.
    let note = git(copy, &["notes", "--ref=semilla", "show", "HEAD"]);
    let mut changed: serde_json::Value = serde_json::from_str(&note).unwrap();
    changed["steps"][0]["intent"] = serde_json::Value::String("a different text".to_string());
    let file = dir.path().join("changed-note.json");
    std::fs::write(&file, changed.to_string()).unwrap();
    git(copy, &["notes", "--ref=semilla", "add", "-f", "-F", file.to_str().unwrap(), "HEAD"]);
    let (_, out) = run_env(copy, &["status", "--no-wait"], &[]);
    assert!(!out.contains(&checked), "a changed note has no result of this machine\n{out}");
    assert!(out.contains(&data_only), "{out}");
}

/// Batch 6a, B4 (trial 9): a file mode changes no body. The fact of a mode change of a Rust
/// file showed the bodies of a changed macro of the file as its affected bodies, and the reason
/// for the mode was the only reason that the step asked for.
#[test]
fn a_file_mode_change_has_no_affected_bodies() {
    use std::os::unix::fs::PermissionsExt;
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/made.rs";
    let made = "macro_rules! make {\n    ($name:ident, $n:expr) => {\n        pub fn $name(x: u32) -> u32 {\n            x + $n\n        }\n    };\n}\nmake!(one, 1);\nmake!(two, 2);\n";
    write(repo, file, made);
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod made;\n");
    commit_all(repo, "a macro");
    // `status` of a commit with no note gives the hint of `review` (batch 6a, B4).
    let (code, out) = run_env(repo, &["status", "HEAD"], &[]);
    // The message has no raw text of the `git` command (batch 7b, part 5.4).
    assert!(code != 0 && out.contains("has no semilla note; tag it first") && !out.contains("git notes") && !out.contains("no note found"), "{code}\n{out}");
    git(repo, &["update-index", "--chmod=+x", file]);
    std::fs::set_permissions(repo.join(file), std::fs::Permissions::from_mode(0o755)).unwrap();
    write(repo, file, &made.replace("x + $n", "x * $n"));
    let (code, out) = run_env(repo, &["step", "multiply"], &[]);
    assert_eq!(code, 1, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let mode = lines.iter().position(|line| line.contains(&format!("{file}: changed (mode 100644 → 100755) [file · no code]"))).unwrap_or_else(|| panic!("the mode fact has the category `file`\n{out}"));
    assert!(!lines[mode + 1].contains("affected bodies"), "a mode changes no body\n{out}");
    // R63.9-5 (batch 10c2): the changed expansions of the two calls show the change of the
    // bodies that the macro makes, so the fact of the macro owns them with no spotlight. Only
    // the mode needs a reason.
    assert!(out.contains("affected bodies: shop_core::made::one, shop_core::made::two") && out.contains("expansion: `make!(one, 1)`") && !out.contains("a changed macro: the MIR of 2 bodies"), "{out}");
    assert!(out.contains(&format!("--reason '{file}:=<why>'")) && !out.contains("--reason 'shop_core::made::make!=<why>'"), "{out}");
}

/// Batch 6b, part 2 (trial 10): the fact of a macro call lists the items that the call makes,
/// from the data of the compiler. A made item follows the rules of the same item with no macro.
#[test]
fn a_macro_call_fact_lists_the_items_that_the_call_makes() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/cart.rs";
    let same = "\nmacro_rules! same {\n    ($t:ty) => {\n        impl PartialEq for $t {\n            fn eq(&self, other: &Self) -> bool {\n                self.items == other.items\n            }\n        }\n    };\n}\n";
    write(repo, file, &format!("{CART_V0}{same}"));
    let out = run(repo, &["step", "add a macro"], 0);
    assert!(out.contains("derived: added macro `same` [added · no code]"), "{out}");
    // A call that makes a trait impl of a public type.
    let with_call = format!("{CART_V0}{same}same!(Cart);\n");
    write(repo, file, &with_call);
    let out = run(repo, &["step", "call the macro"], 0);
    assert!(out.contains("derived: added macro call `same!(Cart)` (makes: `impl PartialEq for Cart`) [added · code different]"), "{out}");
    let bodies = out.lines().find(|line| line.contains("affected bodies:")).unwrap_or_default();
    assert!(bodies.contains("PartialEq>::eq") && !bodies.contains("total"), "only the bodies that the step changes\n{out}");
    assert!(!out.contains("needs a reason") && !out.contains("indirect change") && !out.contains("does not see"), "{out}");
    // A call that makes a test function, and a call that makes a public function.
    let tests = "\nmacro_rules! getter {\n    ($name:ident, $n:expr) => {\n        pub fn $name() -> u32 {\n            $n\n        }\n    };\n}\ngetter!(made_one, 1);\n\n#[cfg(test)]\nmod tests {\n    macro_rules! t {\n        ($name:ident) => {\n            #[test]\n            fn $name() {\n                assert_eq!(super::made_one(), 1);\n            }\n        };\n    }\n    t!(test26);\n}\n";
    let with_tests = format!("{with_call}{tests}");
    write(repo, file, &with_tests.replace("    t!(test26);\n", ""));
    let out = run(repo, &["step", "a function from a macro"], 0);
    assert!(out.contains("added macro call `getter!(made_one, 1)` (makes: `pub fn made_one`) [added · code different]"), "{out}");
    write(repo, file, &with_tests);
    let out = run(repo, &["step", "a test from a macro"], 0);
    assert!(out.contains("derived: added macro call `t!(test26)` (makes: `test26`) [added · code different]"), "{out}");
    assert!(!out.contains("needs a reason") && !out.contains("indirect change") && !out.contains("does not see"), "{out}");
    // A call that makes an impl of `Drop`: the rule of the direct impl. The fact needs a reason.
    let on_drop = "\nmacro_rules! on_drop {\n    ($t:ty) => {\n        impl Drop for $t {\n            fn drop(&mut self) {}\n        }\n    };\n}\non_drop!(Cart);\n";
    write(repo, file, &format!("{with_tests}{on_drop}"));
    let out = run(repo, &["step", "a drop impl from a macro"], 1);
    assert!(out.contains("added macro call `on_drop!(Cart)` (makes: `impl Drop for Cart`)"), "{out}");
    assert!(out.contains("✗ spotlight: an implicit call: the macro call `on_drop!(Cart)` makes `impl Drop for Cart`") && out.contains("--reason 'shop_core::cart::on_drop!#0=…'"), "{out}");
    write(repo, file, &with_tests);
    // The report: the made items are in the list of the public API changes.
    let md = repo.join(".semilla/review.md");
    run(repo, &["status", "--wait", "600"], 0);
    run(repo, &["review", "--full", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("added `impl PartialEq for Cart` (the macro call `same!(Cart)` makes it)"), "{text}");
    assert!(text.contains("added `pub fn made_one` (the macro call `getter!(made_one, 1)` makes it)"), "{text}");
    assert!(text.contains("| API level | minor |"), "{text}");
}

/// Batch 6b, part 1, case 4: a new test function above other test functions in the root of a
/// test file. The line of each test below it changes. The descriptor of a test has its source
/// position, and the position is not behavior: the step has only the fact of the new test.
#[test]
fn a_new_test_above_other_tests_in_a_test_file_gives_one_fact() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/tests/req.rs";
    let helper = "#[track_caller]\nfn check(n: u32) {\n    assert!(n > 0);\n}\n";
    let old_tests = "\n#[test]\nfn test_one() {\n    check(1);\n}\n\n#[test]\nfn test_two() {\n    check(2);\n}\n";
    write(repo, file, &format!("{helper}{old_tests}"));
    run(repo, &["step", "a test file", "--reason", "shop_core (test \"req\")::check=the panic location of the caller"], 0);
    let new_test = "\n#[test]\nfn test_new() {\n    check(3);\n}\n";
    write(repo, file, &format!("{helper}{new_test}{old_tests}"));
    let out = run(repo, &["step", "a new test above"], 0);
    assert!(out.contains("derived: added `shop_core (test \"req\")::test_new` [added · code different]"), "{out}");
    assert_eq!(out.matches("derived:").count(), 1, "one fact\n{out}");
    assert!(!out.contains("indirect change") && !out.contains("needs a reason"), "{out}");
    // A real change of a test below the new lines stays a fact of that test.
    write(repo, file, &format!("{helper}{new_test}{}", old_tests.replace("check(2)", "check(4)")));
    let out = run(repo, &["step", "a changed test"], 0);
    assert!(out.contains("shop_core (test \"req\")::test_two") && out.matches("derived:").count() == 1, "{out}");
    // A changed attribute of a test is a part of the descriptor: the fact stays.
    write(repo, file, &format!("{helper}{new_test}{}", old_tests.replace("check(2)", "check(4)").replace("#[test]\nfn test_one", "#[test]\n#[should_panic]\nfn test_one")));
    let out = run(repo, &["step", "a test that must panic"], 1);
    assert!(out.contains("a changed attribute that can change the behavior: `should_panic`") && out.contains("[attribute · code different]") && out.contains("! the compiled code changes: shop_core (test \"req\")::test_one"), "{out}");
}

/// Runs a command line of Semilla through the shell, as an agent does.
fn shell(repo: &Path, line: &str) -> (i32, String) {
    let root = workspace_root();
    let binary = Path::new(env!("CARGO_BIN_EXE_semilla"));
    let path = format!("{}:{}", binary.parent().unwrap().display(), std::env::var("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .current_dir(repo)
        .args(["-c", line])
        .env("PATH", path)
        .env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"))
        .env("SEMILLA_CACHE_DIR", cache_dir(repo))
        .output()
        .unwrap();
    (output.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

#[test]
fn the_run_again_line_passes_with_a_key_that_has_an_equal_sign() {
    // Batch 7c, part 3.1 (design 37.6). A manifest key with a `cfg(…)` condition has `=` and
    // `"`. The text before the first `=` was the key, so the reason belonged to no fact, and
    // the step could never pass.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-util/Cargo.toml", "[package]\nname = \"shop-util\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
    write(repo, "crates/shop-util/src/lib.rs", "pub fn util() -> u32 {\n    1\n}\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a util crate"]);
    let manifest = std::fs::read_to_string(repo.join("crates/shop-core/Cargo.toml")).unwrap();
    write(repo, "crates/shop-core/Cargo.toml", &format!("{manifest}\n[package.metadata.\"it's\"]\nlevel = \"a=b\"\n\n[target.'cfg(all(target_arch = \"aarch64\", target_os = \"android\"))'.dependencies]\nshop-util = {{ path = \"../shop-util\" }}\n"));
    let out = run(repo, &["step", "it's a step for the \"android\" target"], 1);
    let again = out.lines().find_map(|line| line.strip_prefix("run again: ")).unwrap_or_else(|| panic!("{out}"));
    assert!(again.contains("target.cfg(all(target_arch = \"aarch64\", target_os = \"android\")).dependencies.shop-util"), "{again}");
    // The line passes with no change but the reason texts. One reason has a `=` and quotes.
    let line = again.replacen("<why>", "the value is a = b, it's \"right\"", 1).replace("<why>", "the target needs it");
    let (code, out) = shell(repo, &line.replace("it's \"right\"", "it'\\''s \"right\""));
    assert_eq!(code, 0, "{line}\n{out}");
    assert!(out.starts_with("accepted: step 0 \"it's a step for the \\\"android\\\" target\""), "{out}");
    assert!(!out.contains("belongs to no fact") && !out.contains("needs a reason"), "{out}");
    assert!(out.contains("package.metadata.it's.level added (\"a=b\") [manifest · no code]\n      spotlight: a manifest key that is not in the allow list; reason: \"the value is a = b, it's \\\"right\\\"\""), "{out}");
    assert!(out.contains("dependencies.shop-util.path added (\"../shop-util\") [manifest · no code]\n      spotlight: a new dependency; reason: \"the target needs it\""), "{out}");
    run(repo, &["undo"], 0);

    // The key and the reason have a `=`: the longest open key is the key.
    let out = run(repo, &["step", "the target again"], 1);
    let keys: Vec<String> = out.lines().find_map(|line| line.strip_prefix("run again: ")).unwrap().split("--reason '").skip(1).map(|part| part.split("=<why>'").next().unwrap().replace("'\\''", "'")).collect();
    let long = keys.iter().find(|key| key.contains("target_arch = ")).unwrap_or_else(|| panic!("{keys:?}"));
    let mut args = vec!["step".to_string(), "the target again".to_string()];
    for key in &keys {
        args.extend(["--reason".to_string(), format!("{key}=why = this")]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(repo, &args, 0);
    assert!(out.contains("reason: \"why = this\""), "{out}");
    assert!(!out.contains("belongs to no fact"), "{long}\n{out}");
}

// Batch 7c, part 3.2 (design 37.7).

#[test]
fn a_feature_that_only_one_side_has_does_not_stop_the_step() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let manifest = "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nold-name = []\n";
    let code = "pub mod cart;\n\n#[cfg(feature = \"old-name\")]\npub fn extra() -> u32 {\n    1\n}\n\npub fn plain(a: u32) -> u32 {\n    a + 1\n}\n";
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n[workspace.metadata.semilla]\nfeature-sets = [[\"old-name\"]]\n");
    write(repo, "crates/shop-core/Cargo.toml", manifest);
    write(repo, "crates/shop-core/src/lib.rs", code);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a feature"]);

    // The step gives the feature a new name, and changes a body.
    write(repo, "crates/shop-core/Cargo.toml", &manifest.replace("old-name", "new-name"));
    write(repo, "crates/shop-core/src/lib.rs", &code.replace("old-name", "new-name").replace("a + 1", "a + 2"));
    let (_, out) = shell(repo, "semilla step 'a new name for the feature'");
    // The build of the configuration with the old feature does not stop the step.
    assert!(!out.contains("the body check failed") && !out.contains("does not contain this feature") && !out.contains("the build failed"), "{out}");
    let gap = "! configurations `dev, features old-name`, `release, features old-name` are not checked: only the code before the step has the feature `old-name`. The code after the step has no such configuration: no compare";
    assert!(out.contains(gap), "{out}");
    // The other configurations compare as usual.
    assert!(out.contains("changed literal `1` → `2` at `shop_core::plain/tail/rhs` [semantics · code different]"), "{out}");
    // The manifest facts of the feature stay.
    assert!(out.contains("crates/shop-core/Cargo.toml: features.old-name removed ([]) [manifest · no code]"), "{out}");
    assert!(out.contains("crates/shop-core/Cargo.toml: features.new-name added ([]) [manifest · no code]"), "{out}");
    // The step passes with the reasons of the `run again:` line.
    let again = out.lines().find_map(|line| line.strip_prefix("run again: ")).unwrap_or_else(|| panic!("{out}")).replace("<why>", "the feature has a new name");
    let (code, out) = shell(repo, &again);
    assert!(code == 0 && out.starts_with("accepted: step 0") && out.contains(gap), "{out}");
}

/// Batch 7d, part 1.1 (design 38.1): two sites with the same head text and a different inner
/// line do not fold. A change of the inner line in one site changes the text output and the
/// Markdown report.
#[test]
fn a_fold_never_hides_an_inner_line() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub fn first(v: &[u32]) -> u32 {\n    let mut sum = 0;\n    sum\n}\n\npub fn second(v: &[u32]) -> u32 {\n    let mut sum = 0;\n    sum\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod loops;\n");
    write(repo, "crates/shop-core/src/loops.rs", base);
    git(repo, &["add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "loops"]);
    let looped = |inner: &str| format!("    for (index, value) in v.iter().enumerate() {{\n        if index > 0 {{\n            {inner}\n        }}\n    }}\n    sum\n");
    let outputs = |second: &str| -> (String, String) {
        let function = |name: &str, inner: &str| format!("pub fn {name}(v: &[u32]) -> u32 {{\n    let mut sum = 0;\n{}}}\n", looped(inner));
        let text = format!("{}\n{}", function("first", "sum += value;"), function("second", second));
        write(repo, "crates/shop-core/src/loops.rs", &text);
        let step = run(repo, &["step", "add the loops"], 0);
        let md = repo.join(".semilla/review.md");
        run(repo, &["review", "--md", md.to_str().unwrap()], 0);
        let report = std::fs::read_to_string(&md).unwrap();
        run(repo, &["undo"], 0);
        (step, report)
    };
    let (honest, honest_md) = outputs("sum += value;");
    // Design 54.7: the text of the loop is full. Before: a cut mark was after `if index > 0 {`.
    assert!(honest.contains("◆ f1 derived: 2 site(s): added loop `for (index, value) in v.iter().enumerate() { if index > 0 { sum += value; } }` [semantics"), "{honest}");
    let (needle, needle_md) = outputs("sum -= value;");
    assert!(!needle.contains("site(s): added loop"), "{needle}");
    assert_eq!(needle.matches(" derived: added loop `for (index, value) in v.iter().enumerate() { if index > 0 { sum ").count(), 2, "{needle}");
    assert!(needle.contains(" derived: added loop `for (index, value) in v.iter().enumerate() { if index > 0 { sum -= value; } }` at `shop_core::loops::second/S1` [semantics"), "{needle}");
    // The Markdown report shows the source of each site.
    assert!(needle_md.contains("sum -= value;") && needle_md.contains("sum += value;"), "{needle_md}");
    assert!(!honest_md.contains("sum -= value;"), "{honest_md}");
}

#[test]
fn each_edit_of_a_body_gets_its_own_compare_result_and_a_category_from_its_syntax() {
    // Design 40.3 to 40.5. A body with two edits: a call in a new form with equal compiled code,
    // and a changed literal. Each edit owns its own MIR elements, so the first edit is
    // `code equal` and the second is `code different`. A second step adds a branch to the same
    // body: the literal keeps its category, and the new branch has the branch mark.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub mod cart;\n\npub fn count(v: &Vec<u32>) -> usize {\n    let n = v.len();\n    n + 1\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", base);
    commit_all(repo, "count");
    let edited = base.replace("v.len()", "Vec::len(v)").replace("n + 1", "n + 2");
    write(repo, "crates/shop-core/src/lib.rs", &edited);
    let call = "changed expression `v.len()` → `Vec::len(v)` at `shop_core::count/S0/init` [semantics · code equal]";
    let literal = "changed literal `1` → `2` at `shop_core::count/tail/rhs` [semantics · code different]";
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains(call) && out.contains(literal), "{out}");
    // The list of the affected bodies has only the edit that owns a changed element.
    assert_eq!(out.matches("affected bodies: shop_core::count").count(), 1, "{out}");
    run(repo, &["undo"], 0);
    let branched = edited.replace("    n + 2\n", "    if n > 9 {\n        return 0;\n    }\n    n + 2\n");
    write(repo, "crates/shop-core/src/lib.rs", &branched);
    let out = run(repo, &["step", "probe"], 0);
    // The same edits keep their categories and their results. The new branch has the mark.
    assert!(out.contains(call) && out.contains(literal), "{out}");
    assert!(out.contains("added early exit `if n > 9 { return 0; }` at `shop_core::count/S1` [semantics · code different]"), "{out}");
    // Design 60: the line `control flow:` replaces the branch mark of design 40.4.
    assert!(out.contains("control flow: + branch `n > 9` (crates/shop-core/src/lib.rs:5): false → check `overflow: n + 2`, true → return\n"), "{out}");
    assert!(!out.contains("branches: "), "{out}");
    assert!(!out.contains("[indirect"), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(out.contains("step 0: MIR map: 0 map error(s); 2 edit(s) own a changed MIR element; 1 edit(s) with equal code in a changed body; 0 edit(s) with the doubt rule"), "{out}");
}

/// The line of a claim of the intent files in a step output: the line with ` ID `.
fn claim_line<'a>(text: &'a str, id: &str) -> &'a str {
    text.lines().find(|line| line.contains(&format!(" {id} "))).unwrap_or_else(|| panic!("no line of {id}:\n{text}"))
}

/// An intent file with one empty `#[same]` claim for each item: the full compare of the item
/// (design 78; design 84: only a claim starts the solver).
fn same_claims(repo: &Path, items: &[&str]) -> Vec<&'static str> {
    let mut text = String::from("use semilla_intent::prelude::*;\n");
    for (index, item) in items.iter().enumerate() {
        text.push_str(&format!("\n#[same({item})]\nfn same_{index}() {{}}\n"));
    }
    write(repo, ".semilla/intent/step.rs", &text);
    vec!["--intent", ".semilla/intent/step.rs"]
}

/// Design 61.10 and design 84: the walk of the callers is a part of the proof of a claim (the
/// bodies that the check inlines), and the step has no lines of the walk. An empty `#[same]`
/// claim on a caller of a changed callee inlines the callee: the claim is false with the input of
/// the caller, also through a chain of two callers.
// rule+ R61.10-3 R61.10-5
// rule- R61.10-3 R61.10-5
#[test]
fn a_claim_on_a_caller_inlines_the_changed_callee_and_the_step_has_no_lines_of_the_walk() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub mod cart;\n\npub fn tier(k: u8) -> u32 {\n    match k {\n        0 => 2,\n        1 => 4,\n        _ => 8,\n    }\n}\n\npub fn cost(k: u8, n: u32) -> u32 {\n    n.wrapping_mul(tier(k))\n}\n\npub fn quote(k: u8, n: u32) -> u32 {\n    cost(k, n).wrapping_add(1)\n}\n\npub fn calm(n: u32) -> u32 {\n    n.wrapping_add(3)\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", base);
    commit_all(repo, "tier");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("1 => 4", "1 => 5"));
    // [R84.1-3] With no claim: no walk, so no line `reaches:` and no part `reached bodies:`.
    let out = run(repo, &["step", "probe"], 0);
    assert!(!out.contains("reaches:") && !out.contains("reached bodies"), "{out}");
    run(repo, &["undo"], 0);
    let mut args = vec!["step", "probe"];
    args.extend(same_claims(repo, &["shop_core::cost", "shop_core::quote", "shop_core::calm"]));
    let out = run(repo, &args, 1);
    // [R61.10-3] The claim on `cost` inlines the changed `tier`; [R61.10-5] the claim on `quote`
    // inlines `cost` (a caller that the walk does not prove equivalent).
    assert!(claim_line(&out, "i1").contains("false: for k = 1, n = 1: before returns 4, after returns 5") && claim_line(&out, "i1").contains("suggested test `semilla_test_cost_"), "{out}");
    assert!(claim_line(&out, "i2").contains("false: for k = 1, n = 1: before returns 5, after returns 6"), "{out}");
    // The negative case: `calm` calls no changed body.
    assert!(claim_line(&out, "i3").contains("proved"), "{out}");
    assert!(!out.contains("reached bodies"), "{out}");
}

/// Design 61.14 and design 84: the count of the variant builds of a step (R77.2-2) stops the
/// builds with one line for the step (R61.14-6). With no count, each variant builds.
// rule+ R61.14-6
// rule- R61.14-6
// rule+ R77.2-2
// rule- R77.2-2
#[test]
fn the_count_of_the_variant_builds_stops_each_variant_with_one_line_for_the_step() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub mod cart;\n\npub fn total(n: u32) -> u32 {\n    let sum = n.wrapping_mul(3);\n    sum.wrapping_add(1)\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", base);
    commit_all(repo, "total");
    // One body with two edits: a rename of a local and a changed literal.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("let sum", "let all").replace("sum.wrapping", "all.wrapping").replace("wrapping_add(1)", "wrapping_add(2)"));
    let (code, out) = run_env(repo, &["step", "probe"], &[("SEMILLA_VARIANT_BUILDS", "0")]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("      equivalence: not checked alone (the budget of the step); the body: MIR changes\n"), "{out}");
    assert!(out.contains("  the equivalence check stopped at its budget (0 variant builds): 2 facts not checked alone; semilla expand --step 0\n"), "{out}");
    run(repo, &["undo"], 0);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("      equivalence: equivalent alone (proved by equal MIR); the body: MIR changes\n") && out.contains("      equivalence: MIR changes alone; the body: MIR changes\n"), "{out}");
    assert!(!out.contains("not checked") && !out.contains("the equivalence check stopped"), "{out}");
    // [R77.2-2] The budget of a claim: an empty `#[same]` claim of the body gets the result of
    // the solver in its budget.
    run(repo, &["undo"], 0);
    let mut args = vec!["step", "probe"];
    args.extend(same_claims(repo, &["shop_core::total"]));
    let out = run(repo, &args, 1);
    assert!(claim_line(&out, "i1").contains("false: for n = "), "{out}");
}

/// A proc macro of the crate `depot` (E1 of batch 10nq-fix2) whose output has an `if` with an
/// `else` block of made tokens: a mark of the context (R65.1-1) in place of that block made the
/// printer of the compiler panic, and the drop of its boxes aborted the driver. The build check
/// failed for code that compiles, or the compiled mode lost the test and example targets.
const LEVEL_DEPOT: &str = "use proc_macro::{Delimiter, Group, TokenStream, TokenTree};\n\n#[proc_macro_derive(Level, attributes(level))]\npub fn level(input: TokenStream) -> TokenStream {\n    let mut name = String::new();\n    let mut limit = None;\n    let mut next = false;\n    for token in input {\n        match token {\n            TokenTree::Group(group) if group.delimiter() == Delimiter::Bracket => {\n                for inner in group.stream() {\n                    if let TokenTree::Group(args) = inner {\n                        limit = args.stream().into_iter().next();\n                    }\n                }\n            }\n            TokenTree::Ident(ident) if next => {\n                name = ident.to_string();\n                break;\n            }\n            TokenTree::Ident(ident) => next = ident.to_string() == \"struct\",\n            _ => {}\n        }\n    }\n    let mut body: TokenStream = \"if x >\".parse().unwrap();\n    body.extend(limit);\n    body.extend(\"{ x } else { 0 }\".parse::<TokenStream>().unwrap());\n    let mut inner: TokenStream = \"pub fn level(&self, x: u32) -> u32\".parse().unwrap();\n    inner.extend([TokenTree::Group(Group::new(Delimiter::Brace, body))]);\n    let mut out: TokenStream = format!(\"impl {name}\").parse().unwrap();\n    out.extend([TokenTree::Group(Group::new(Delimiter::Brace, inner))]);\n    out\n}\n";

// rule+ R66.11-1 R66.11-2
// rule- R66.11-2
#[test]
fn a_derive_whose_else_block_has_only_made_tokens_passes_the_build_check_in_each_target() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/depot/Cargo.toml", "[package]\nname = \"depot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\nproc-macro = true\n");
    write(repo, "crates/depot/src/lib.rs", LEVEL_DEPOT);
    write(repo, "crates/shop-core/Cargo.toml", "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\ndepot = { path = \"../depot\" }\n");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n\n#[derive(depot::Level)]\n#[level(3)]\npub struct Shelf;\n\n#[derive(Debug, PartialEq)]\npub struct Plain(pub u32);\n");
    write(repo, "crates/shop-core/examples/demo.rs", "#[derive(depot::Level)]\n#[level(5)]\nstruct Bin;\n\nfn main() {\n    println!(\"{}\", Bin.level(6) + shop_core::Shelf.level(2));\n}\n");
    write(repo, "crates/shop-core/tests/levels.rs", "#[derive(depot::Level)]\n#[level(7)]\nstruct Crate;\n\n#[test]\nfn level() {\n    assert_eq!(Crate.level(8), 8);\n}\n");
    commit_all(repo, "levels");
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("*x < max", "*x <= max"));
    let (code, out) = run_env(repo, &["step", "include the limit"], &[]);
    assert!(out.contains("\nbuild check: passes (the build of the compiled check)\n"), "{code}: {out}");
    assert!(!out.contains("libraries and binaries only") && !out.contains("does not compile") && !out.contains("the body check failed"), "{out}");
    // Each target has its records: the library, the test target, and the example.
    fn files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() { files(&path, out) } else { out.push(path) }
        }
    }
    let mut found = Vec::new();
    files(&repo_cache(repo), &mut found);
    let records: Vec<&PathBuf> = found.iter().filter(|path| path.extension().is_some_and(|extension| extension == "jsonl")).collect();
    for target in ["shop_core.lib.", "levels.test.", "demo.bin."] {
        let record = records.iter().find(|path| path.file_name().unwrap().to_string_lossy().starts_with(target)).unwrap_or_else(|| panic!("{target}: {records:?}"));
        // R66.11-2: the mark of the made `else` block keeps a block. The negative case: the
        // derives of the standard library keep their text.
        let text = std::fs::read_to_string(record).unwrap();
        assert!(text.contains("} else { \\\"\\u0000made by the proc macro `depot::Level`\\u0000\\\" }"), "{target}: {text}");
        assert!(!text.contains("unknown_print"), "{target}: {text}");
        if target.starts_with("shop_core.lib") {
            assert!(text.contains("impl ::core::fmt::Debug for Plain {"), "{text}");
        }
    }
}

/// Design 60: the control flow graph of each body with `--compiled`. One step has a body of
/// each result: `graph changes` (an early exit), `same structure` (a changed condition), `same
/// graph` (a changed literal with no check), and a body with no edit whose graph changes (a
/// changed macro).
const FLOW_BASE: &str = "pub mod cart;\n\npub enum Error {\n    Empty,\n}\n\nmacro_rules! guard {\n    ($v:expr) => {\n        if $v > 9 {\n            return 0;\n        }\n    };\n}\n\npub fn price(qty: u32) -> u32 {\n    qty * 3\n}\n\npub fn order(qty: u32) -> Result<u32, Error> {\n    let total = price(qty);\n    Ok(total)\n}\n\npub fn tax(v: u32) -> u32 {\n    if v > 3 { v / 2 } else { v }\n}\n\npub fn mask(v: u32) -> u32 {\n    v ^ 1\n}\n\npub fn limit(v: u32) -> u32 {\n    guard!(v);\n    v\n}\n";

// rule+ R60.3-5 R60.3-6 R60.4-1 R60.4-2 R60.4-3 R60.4-4 R61.6-3
// rule- R60.3-5 R60.3-6 R60.4-1 R60.4-2 R60.4-4 R61.6-3
#[test]
fn the_compiled_check_gives_the_control_flow_of_each_body_with_an_edit_or_a_changed_graph() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", FLOW_BASE);
    commit_all(repo, "flow");
    let edited = FLOW_BASE
        .replace("    let total = price(qty);\n", "    if qty == 0 {\n        return Err(Error::Empty);\n    }\n    let total = price(qty);\n")
        .replace("if v > 3", "if v >= 3")
        .replace("v ^ 1", "v ^ 2")
        .replace("            return 0;\n        }\n", "            return 0;\n        }\n        if $v == 5 {\n            return 5;\n        }\n")
        .replace("pub fn limit", "pub fn twice(v: u32) -> u32 {\n    v ^ 3\n}\n\npub fn limit");
    write(repo, "crates/shop-core/src/lib.rs", &edited);
    // Design 84: the results of the solver come from the claims of the intent file.
    let mut args = vec!["step", "probe"];
    args.extend(same_claims(repo, &["shop_core::limit", "shop_core::mask", "shop_core::order", "shop_core::tax"]));
    let out = run(repo, &args, 1);
    // R60.4-1: the line of the fact, with the changed point and its arrows. It replaces the
    // line `branches:` of design 40.4. R60.4-7: a callee has its short form in a fact line
    // (`price`), and its full form in `status --verbose` (`shop_core::price`, below).
    assert!(out.contains("added early exit `if qty == 0 { return Err(Error::Empty); }` at `shop_core::order/S0` [semantics · code different]\n      affected bodies: shop_core::order\n      control flow: + branch `qty == 0` (crates/shop-core/src/lib.rs:23): false → call `price`, true → return\n"), "{out}");
    assert!(!out.contains("branches: "), "{out}");
    // R60.4-1, the negative case: a fact with no changed point and no changed arrow at its
    // lines has no line.
    let mask = out.lines().skip_while(|line| !line.contains("changed literal `1` → `2` at `shop_core::mask")).skip(1).take_while(|line| !line.contains('◆')).collect::<Vec<_>>().join("\n");
    assert!(!mask.contains("control flow:"), "{mask}");
    // R60.4-2 and R60.3-6: one line for each body with a result. A body with no edit and a
    // changed graph has its line; a body with no edit and the same graph has none.
    assert!(out.contains("  control flow: 5 bodies\n"), "{out}");
    assert!(out.contains("    shop_core::limit (crates/shop-core/src/lib.rs:"), "{out}");
    // R61.6-2: the line of each body ends with its equal-MIR check (design 84), and the claims
    // give the results of the solver.
    assert!(out.contains("graph changes: +1 branch, +3 arrows, −1 arrow; no edit in the body; MIR changes"), "{out}");
    assert!(out.contains("    shop_core::mask (crates/shop-core/src/lib.rs:34): same graph (the edit keeps the control flow); MIR changes\n"), "{out}");
    assert!(claim_line(&out, "i1").contains("proved: equivalent by SMT"), "{out}");
    assert!(claim_line(&out, "i2").contains("false: for v = 0: before returns 1, after returns 2") && claim_line(&out, "i2").contains("suggested test `semilla_test_mask_"), "{out}");
    assert!(claim_line(&out, "i3").contains("false: for qty = 0: before returns Ok(a value), after returns Err(Empty)"), "{out}");
    assert!(claim_line(&out, "i4").contains("false: for v = 3: before returns 3, after returns 1"), "{out}");
    // R60.3-5: a new body has its full graph, not a diff.
    assert!(out.contains("    shop_core::twice (crates/shop-core/src/lib.rs:"), "{out}");
    assert!(out.contains("graph added: 2 points, 1 arrow"), "{out}");
    assert!(!out.contains("shop_core::price ("), "{out}");
    // R60.4-3: `status --verbose` has each changed point and each arrow that is not shared (a
    // step with no false claim is stored).
    let mut args = vec!["step", "probe"];
    args.extend(same_claims(repo, &["shop_core::limit"]));
    run(repo, &args, 0);
    let verbose = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(verbose.contains("          − arrow entry (crates/shop-core/src/lib.rs:-19) → call `shop_core::price` (crates/shop-core/src/lib.rs:-20)\n"), "{verbose}");
    // A branch from the body of a macro definition has the text of the macro call first, then
    // the expanded text (R63.2-5: no metavariable).
    assert!(verbose.contains("          + branch `guard!(v): v == 5` (crates/shop-core/src/lib.rs:43)\n"), "{verbose}");
    assert!(verbose.contains("          ~ branch `v > 3` → `v >= 3` (crates/shop-core/src/lib.rs:-25, crates/shop-core/src/lib.rs:31)\n"), "{verbose}");
    // R61.6-3: `status --verbose` has the method, the inlined callees, the query count, and
    // the time of each check (the equal-MIR check: no query).
    assert!(verbose.contains("      equivalence (in dev): MIR changes; no callee inlined; 0 solver queries; "), "{verbose}");
    // R60.4-4: the Markdown review has a section for each body whose graph changes, with the
    // list and a flowchart. The negative case: a body with the same structure has no section.
    let md = dir.path().join("review.md");
    run(repo, &["review", "--md", md.to_str().unwrap(), "--wait", "600"], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("## Control flow changes\n\n### `shop_core::limit` (step 0)\n") && text.contains("### `shop_core::order` (step 0)\n"), "{text}");
    assert!(text.contains("- [crates/shop-core/src/lib.rs:23](crates/shop-core/src/lib.rs#L23): \\+ branch `qty == 0`"), "{text}");
    assert!(text.contains("```mermaid\nflowchart TD\n") && text.contains("-.->"), "{text}");
    assert!(!text.contains("### `shop_core::tax` (step 0)"), "{text}");
}

/// The six forms of fix round 1, D1 (design 40.3): one small edit, and unchanged code around
/// it. The alignment pairs each unchanged element, so no fact says "no edit at its place".
const D1_FORMS: &str = r##"pub mod jaro {
use std::cmp::{max, min};
pub fn jaro(a: &str, b: &str) -> f64 {
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    if a_len == 0 && b_len == 0 {
        return 1.0;
    } else if a_len == 0 || b_len == 0 {
        return 0.0;
    }
    let mut search_range = max(a_len, b_len) / 2;
    search_range = search_range.saturating_sub(1);
    let mut flags_memory = vec![false; a_len + b_len];
    let (a_flags, b_flags) = flags_memory.split_at_mut(a_len);
    let mut matches = 0_usize;
    for (i, a_elem) in a.chars().enumerate() {
        let min_bound = if i > search_range { i - search_range } else { 0 };
        let max_bound = min(b_len, i + search_range + 1);
        for (j, b_elem) in b.chars().enumerate().take(max_bound) {
            if min_bound <= j && a_elem == b_elem && !b_flags[j] {
                a_flags[i] = true;
                b_flags[j] = true;
                matches += 1;
                break;
            }
        }
    }
    let mut transpositions = 0_usize;
    if matches != 0 {
        let mut b_iter = b_flags.iter().zip(b.chars());
        for (a_flag, ch1) in a_flags.iter().zip(a.chars()) {
            if *a_flag {
                loop {
                    if let Some((b_flag, ch2)) = b_iter.next() {
                        if !*b_flag {
                            continue;
                        }
                        if ch1 != ch2 {
                            transpositions += 1;
                        }
                        break;
                    }
                }
            }
        }
    }
    transpositions /= 2;
    if matches == 0 {
        0.0
    } else {
        ((matches as f64 / a_len as f64) + (matches as f64 / b_len as f64) + ((matches - transpositions) as f64 / matches as f64)) / 3.0
    }
}
}
pub mod bs {
pub struct Alphabet { encode: [u8; 58], decode: [u8; 128] }
pub enum Error { Dup(char, usize), NonAscii(char, usize) }
impl Alphabet {
    pub fn new(base: &[u8; 58]) -> Result<Self, Error> {
        let mut encode = [0x00; 58];
        let mut decode = [0xFF; 128];
        let mut i = 0;
        while i < encode.len() {
            if base[i] >= 128 {
                return Err(Error::NonAscii(base[i] as char, i));
            }
            if decode[base[i] as usize] != 0xFF {
                return Err(Error::Dup(base[i] as char, i));
            }
            encode[i] = base[i];
            decode[base[i] as usize] = i as u8;
            i += 1;
        }
        Ok(Self { encode, decode })
    }
}
}
pub mod map {
pub struct Map { fill: usize, mask: usize, keys: Vec<u64> }
impl Map {
    pub fn get_mut(&mut self, key: u64) -> Option<&mut u64> {
        let at = (key as usize) & self.mask;
        if self.keys[at] == key {
            return self.keys.get_mut(at);
        }
        if self.keys[at] == 0 {
            self.keys[at] = key;
            self.fill += 1;
            if self.fill * 3 >= (self.mask + 1) * 2 {
                self.mask = self.mask * 2 + 1;
            }
        }
        None
    }
}
}
pub mod zero {
pub struct Secret(pub Vec<u8>);
pub trait Zeroize { fn zeroize(&mut self); }
impl Zeroize for Vec<u8> { fn zeroize(&mut self) { for b in self.iter_mut() { *b = 0; } } }
impl Clone for Secret {
    fn clone(&self) -> Self { Secret(self.0.clone()) }
    fn clone_from(&mut self, source: &Self) {
        self.0.zeroize();
        self.0.clone_from(&source.0);
    }
}
}
pub mod xo {
pub struct Xo { s: [u32; 4] }
fn read_u32_into(src: &[u8], dst: &mut [u32]) { for (i, d) in dst.iter_mut().enumerate() { *d = src[i] as u32; } }
impl Xo {
    pub fn from_seed(seed: [u8; 16]) -> Xo {
        let mut state = [0; 4];
        read_u32_into(&seed, &mut state);
        Xo { s: state }
    }
}
}
pub mod dec {
pub fn decode(input: &[u8]) -> usize {
    let mut val = 0usize;
    let mut i = 0;
    while i < input.len() {
        let byte = input[i];
        val += (byte as usize) * 58;
        i += 1;
    }
    val
}
}
pub mod either {
    pub enum Either<L, R> { Left(L), Right(R) }
    impl<L, R> Either<L, R> {
        pub fn left(self) -> Option<L> {
            match self {
                Either::Left(l) => Some(l),
                Either::Right(_) => None,
            }
        }
    }
}
pub mod packing {
    pub const fn pack(input: &[u8], output: &mut [u8; 8]) -> usize {
        let mut i = 0;
        let mut index = 0;
        while i < input.len() {
            let mut val = input[i] as usize;
            let mut j = 0;
            while j < index {
                let byte = output[j];
                val += (byte as usize) * 58;
                output[j] = (val & 0xFF) as u8;
                val >>= 8;
                j += 1;
            }
            while val > 0 && index < 8 {
                output[index] = (val & 0xFF) as u8;
                index += 1;
                val >>= 8;
            }
            i += 1;
        }
        index
    }
}
pub mod checks {
    pub enum Error { NonAscii { index: usize }, Duplicate { index: usize } }
    pub fn table(base: &[u8; 8]) -> Result<[u8; 128], Error> {
        let mut decode = [0xFF; 128];
        let mut i = 0;
        while i < base.len() {
            if base[i] >= 128 {
                return Err(Error::NonAscii { index: i });
            }
            if decode[base[i] as usize] != 0xFF {
                return Err(Error::Duplicate { index: i });
            }
            decode[base[i] as usize] = i as u8;
            i += 1;
        }
        Ok(decode)
    }
    pub fn ratio(a_len: usize, b_len: usize) -> f64 {
        if a_len == 0 && b_len == 0 {
            return 1.0;
        } else if a_len == 0 || b_len == 0 {
            return 0.0;
        }
        (a_len as f64) / (b_len as f64)
    }
}
"##;

#[test]
fn a_small_edit_gives_no_indirect_fact_for_the_code_around_it() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod d1;\n");
    write(repo, "crates/shop-core/src/d1.rs", D1_FORMS);
    commit_all(repo, "the forms");
    // Since fix round 1 of design 46, rule 3c forwards the copy of `transpositions` into both
    // reads of the division: the compound assignment and its long form have equal code.
    write(repo, "crates/shop-core/src/d1.rs", &D1_FORMS.replacen("    transpositions /= 2;", "    transpositions = transpositions / (2);", 1));
    let out = run(repo, &["step", "probe"], 0);
    assert!(!out.contains("[indirect") && out.contains("[semantics · code equal]") && !out.contains("code different]"), "{out}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/d1.rs", D1_FORMS);
    for (from, to) in [
        ("        while i < encode.len() {", "        while !(i < encode.len()) {"),
        ("        if self.keys[at] == 0 {", "        if !(self.keys[at] == 0) {"),
        ("        self.0.zeroize();\n", ""),
        ("        read_u32_into(&seed, &mut state);\n", ""),
        ("    while i < input.len() {\n        let byte = input[i];", "    loop {\n        if !(i < input.len()) {\n            break;\n        }\n        let byte = input[i];"),
        // Fix round 2: the forms of the mutants of `bs58` and `strsim` (a value that the driver
        // wraps as opaque before its use, two returns of one value, a negated condition).
        ("                val >>= 8;\n                j += 1;", "                j += 1;"),
        ("        while i < input.len() {\n            let mut val", "        while !(i < input.len()) {\n            let mut val"),
        ("            if decode[base[i] as usize] != 0xFF {\n                return Err(Error::Duplicate { index: i });\n            }\n", ""),
        ("        if a_len == 0 && b_len == 0 {", "        if !(a_len == 0 && b_len == 0) {"),
        // Fix round 1, D2: the tail `match` is replaced, and the element of the block belongs
        // to the edit.
        ("            match self {\n                Either::Left(l) => Some(l),\n                Either::Right(_) => None,\n            }\n", "            Default::default()\n"),
    ] {
        write(repo, "crates/shop-core/src/d1.rs", &D1_FORMS.replacen(from, to, 1));
        let out = run(repo, &["step", "probe"], 0);
        assert!(!out.contains("[indirect") && !out.contains("with no edit at its place"), "{from} → {to}\n{out}");
        assert!(out.contains("code different]"), "{from} → {to}\n{out}");
        run(repo, &["undo"], 0);
        write(repo, "crates/shop-core/src/d1.rs", D1_FORMS);
    }
}

#[test]
fn an_edit_whose_only_effect_is_the_place_of_a_drop_is_code_different() {
    // Fix round 1, D4: `let _ = guard();` drops the guard at once; `let _g = guard();` drops it
    // at the end of the scope. The drop at the end of the scope is a glue element with no
    // owner; the drop at the statement belongs to the edit. The edit is `code different`. The
    // overflow check of `a + 1` now unwinds through a drop of the guard: a real change with no
    // edit at its place. Since design 44, the fact names no possible cause: a type or a path edit
    // is a cause (a link), or it is not.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub mod cart;\n\npub struct Guard;\n\nimpl Drop for Guard {\n    fn drop(&mut self) {\n        println!(\"done\");\n    }\n}\n\npub fn guard() -> Guard {\n    Guard\n}\n\npub fn work(a: u32) -> u32 {\n    let _ = guard();\n    a + 1\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", base);
    commit_all(repo, "a guard");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("let _ = guard();", "let _g = guard();"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("changed pattern `_` → `_g` at `shop_core::work/S0/pat` [semantics · code different]") && !out.contains("code equal]"), "{out}");
    assert!(out.contains("the compiled code of `shop_core::work` changes with no edit at its place: `a + 1` [indirect · code different]") && !out.contains("a type or a path changes"), "{out}");
    assert_eq!(out.matches("◆").count(), 2, "{out}");
}

#[test]
fn the_statement_mapper_gives_one_fact_for_each_refactoring_and_keeps_each_change() {
    // Design 41: a statement that moves into a block, an extracted variable, a swap, and a
    // swap with a changed operator (a needle). Each refactoring is one fact; the change of
    // behavior keeps its own fact with `code different`. A moved statement has the category of
    // its statement form, with no mismatch mark (fix round 2); a variable is `move`.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub mod cart;\n\npub fn mix(a: u32, b: u32) -> u32 {\n    let mut s = 0;\n    s += a * 2;\n    if b > 3 {\n        s += b;\n    }\n    s\n}\n\npub fn area(a: u32, b: u32) -> u32 {\n    if a * b + 1 > 10 {\n        return a * b + 1;\n    }\n    0\n}\n\npub fn pick(a: u32, b: u32) -> u32 {\n    let x = a + 1;\n    let y = b * 2;\n    if x > y {\n        return x - y;\n    }\n    x + y\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", base);
    commit_all(repo, "refactor");
    let step = |text: String| -> String {
        write(repo, "crates/shop-core/src/lib.rs", &text);
        let out = run(repo, &["step", "probe"], 0);
        run(repo, &["undo"], 0);
        out
    };
    let out = step(base.replace("    s += a * 2;\n    if b > 3 {\n", "    if b > 3 {\n        s += a * 2;\n"));
    assert!(out.contains("moved statement `s += a * 2` from `shop_core::mix/S1` to `shop_core::mix/S1/then/S0` [semantics · code different]") && !out.contains("! the compiled code changes"), "{out}");
    assert!(!out.contains("[indirect") && out.matches(" derived: ").count() == 1, "{out}");
    let out = step(base.replace("    if a * b + 1 > 10 {\n        return a * b + 1;\n", "    let area = a * b + 1;\n    if area > 10 {\n        return area;\n"));
    assert!(out.contains("extracted variable `area` = `a * b + 1` (2 uses) at `shop_core::area/S0` [move · code"), "{out}");
    assert!(!out.contains("[indirect") && out.matches(" derived: ").count() == 1, "{out}");
    let swapped = base.replace("    let x = a + 1;\n    let y = b * 2;\n", "    let y = b * 2;\n    let x = a + 1;\n");
    let out = step(swapped.clone());
    assert!(out.contains("moved statement `let x = a + 1;` from `shop_core::pick/S0` to `shop_core::pick/S1` [semantics · code") && !out.contains("! the compiled code changes"), "{out}");
    assert!(!out.contains("[indirect") && out.matches(" derived: ").count() == 1, "{out}");
    // The needle: the move does not absorb the changed operator.
    let out = step(swapped.replace("if x > y", "if x >= y"));
    assert!(out.contains("moved statement `let x = a + 1;` from `shop_core::pick/S0` to `shop_core::pick/S1` [semantics · code"), "{out}");
    assert!(out.contains("`x > y` → `x >= y` at `shop_core::pick/S2/cond` [semantics · code different]"), "{out}");
    write(repo, "crates/shop-core/src/lib.rs", &swapped);
    run(repo, &["step", "probe"], 0);
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(out.contains("; statement map: "), "{out}");
}

/// Design 44 (batch 8f): the symbol links. A changed constant, alias, field type, `Drop` impl,
/// and `use` line change the compiled code of the bodies that name them, with no edit at their
/// place: each body goes to the edit of its symbol, also in a different crate. A change of
/// behavior next to a changed symbol keeps its own fact (the needle).
#[test]
fn the_symbol_links_give_each_changed_body_to_the_edit_of_its_symbol() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "pub const LIMIT: u32 = 10;\npub type Num = u32;\n\npub struct P {\n    pub x: u32,\n}\n\npub struct G {\n    pub v: u32,\n}\n\npub mod a {\n    pub fn f() -> u32 {\n        1\n    }\n}\n\npub mod b {\n    pub fn f() -> u32 {\n        2\n    }\n}\n\npub fn over(x: u32) -> bool {\n    x > LIMIT\n}\n\npub fn inc(a: Num) -> Num {\n    a + 1\n}\n\npub fn big(p: &P) -> bool {\n    p.x > 10\n}\n\npub fn use_g() -> u32 {\n    let g = G { v: 1 };\n    g.v + 1\n}\n\npub fn both(x: u32, y: u32) -> u32 {\n    let s = x + LIMIT;\n    s * y\n}\n";
    let pick = "use crate::links::a::f;\n\npub fn g() -> u32 {\n    f() + 1\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod links;\npub mod pick;\n");
    write(repo, "crates/shop-core/src/links.rs", base);
    write(repo, "crates/shop-core/src/pick.rs", pick);
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("fn main() {\n", "fn main() {\n    println!(\"{}\", shop_core::links::LIMIT);\n"));
    commit_all(repo, "links");
    let after = base
        .replace("LIMIT: u32 = 10", "LIMIT: u32 = 12")
        .replace("type Num = u32", "type Num = u64")
        .replace("pub x: u32", "pub x: u64")
        .replace("pub mod a {", "impl Drop for G {\n    fn drop(&mut self) {\n        self.v = 0;\n    }\n}\n\npub mod a {")
        .replace("s * y", "s + y");
    write(repo, "crates/shop-core/src/links.rs", &after);
    write(repo, "crates/shop-core/src/pick.rs", &pick.replace("a::f", "b::f"));
    let (code, out) = run_env(repo, &["step", "links"], &[]);
    assert_eq!(code, 1, "the new `Drop` impl needs a reason\n{out}");
    let out = run(repo, &["step", "links", "--reason", "shop_core::links::impl <G as Drop>=the guard clears the value"], 0);
    assert!(!out.contains("[indirect") && !out.contains("cause not known"), "each changed body has a link\n{out}");
    let block = |text: &str| -> String {
        let lines: Vec<&str> = out.lines().collect();
        let start = lines.iter().position(|line| line.contains('◆') && line.contains(text)).unwrap_or_else(|| panic!("no fact `{text}`\n{out}"));
        let end = lines[start + 1..].iter().position(|line| line.contains('◆')).map_or(lines.len(), |at| start + 1 + at);
        lines[start..end].join("\n")
    };
    let value = block("value of `shop_core::links::LIMIT` changed");
    assert!(value.contains("code different]") && value.contains("shop_core::links::over") && value.contains("shop_core::links::both") && value.contains("shop_app (bin \"shop-app\")::main"), "{value}\n{out}");
    assert!(value.contains("through: `shop_core::links::LIMIT`"), "{value}");
    assert!(block("type of `shop_core::links::Num`").contains("shop_core::links::inc"), "{out}");
    assert!(block("type of `shop_core::links::P.x`").contains("shop_core::links::big"), "{out}");
    assert!(block("trait impl `<G as Drop>` added").contains("shop_core::links::use_g"), "{out}");
    assert!(block("added `use` line: `use crate::links::b::f;`").contains("shop_core::pick::g"), "{out}");
    // The needle: the change of behavior keeps its own fact and its own result.
    let needle = block("`s * y` → `s + y`");
    assert!(needle.contains("code different]") && needle.contains("affected bodies: shop_core::links::both"), "{needle}\n{out}");
}

/// Runs a step with new texts of files, returns the output, and goes back to the base: `undo` and
/// the base texts of the files (fix round 1 of batch 8f).
fn step_and_undo(repo: &Path, base: &[(&str, &str)], new: &[(&str, &str)], intent: &str) -> String {
    for (file, text) in new {
        write(repo, file, text);
    }
    let out = run(repo, &["step", intent], 0);
    run(repo, &["undo"], 0);
    for (file, text) in base {
        write(repo, file, text);
    }
    for (file, _) in new.iter().filter(|(file, _)| !base.iter().any(|(known, _)| known == file)) {
        std::fs::remove_file(repo.join(file)).unwrap();
    }
    out
}

/// Design 44, fix round 1 (D2, D4): a link adds a cause; it never takes the elements of a moved
/// statement. A changed impl set of a type (a `derive` list, a new impl) is no cause of a field
/// read: only a call that resolves to an impl, a drop, or a projection follows it.
#[test]
fn a_link_never_takes_the_elements_of_a_moved_statement_and_an_impl_set_is_no_cause_of_a_field_read() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = "#[derive(Clone)]\npub struct Counter {\n    pub n: u32,\n    pub m: u32,\n}\n\nimpl Counter {\n    pub fn step(&mut self) -> u32 {\n        let v = self.n + self.m;\n        self.m = self.n.wrapping_add(7);\n        v\n    }\n}\n\n#[derive(PartialEq)]\npub struct Name(pub u32);\n\npub fn same(a: &Name, b: &Name) -> bool {\n    a == b\n}\n";
    let file = "crates/shop-core/src/counter.rs";
    let lib = ("crates/shop-core/src/lib.rs", "pub mod cart;\npub mod counter;\n");
    write(repo, lib.0, lib.1);
    write(repo, file, base);
    commit_all(repo, "counter");
    let swapped = base.replace("        let v = self.n + self.m;\n        self.m = self.n.wrapping_add(7);\n", "        self.m = self.n.wrapping_add(7);\n        let v = self.n + self.m;\n");
    let derive = swapped.replace("#[derive(Clone)]", "#[derive(Clone, Debug)]");
    let with_impl = format!("{swapped}\nimpl Default for Counter {{\n    fn default() -> Self {{\n        Counter {{ n: 1, m: 2 }}\n    }}\n}}\n");
    for (text, form) in [(derive.as_str(), "a derive"), (with_impl.as_str(), "an impl")] {
        let out = step_and_undo(repo, &[(file, base)], &[(file, text)], "swap");
        assert!(fact_line(&out, "moved statement `let v = self.n + self.m;`").ends_with("[semantics · code different]"), "{form}\n{out}");
        assert!(!out.contains("through: `shop_core::counter::Counter`"), "a field read does not resolve through {form}\n{out}");
    }
    // D4: a marker impl changes no compiled code.
    let out = step_and_undo(repo, &[(file, base)], &[(file, &format!("{base}\nimpl Eq for Name {{}}\n"))], "a marker");
    let marker = fact_line(&out, "trait impl `<Name as Eq>` added");
    assert!(!marker.is_empty() && !marker.ends_with("code different]"), "{out}");
}

/// Design 44, fix round 1 (D3): a new order or a new path is not a change of a definition. A
/// `use` line is a cause only when the target of a path through it differs.
#[test]
fn an_order_or_a_path_is_no_change_of_a_definition() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let lib = "pub mod cart;\npub mod forms;\npub mod user;\n";
    let forms = "pub mod a {\n    pub fn f() -> u32 {\n        1\n    }\n\n    pub const LIMIT: u32 = 10;\n}\n\npub mod b {\n    pub fn f() -> u32 {\n        2\n    }\n}\n\npub mod c {\n    pub fn f() -> u32 {\n        3\n    }\n}\n\npub struct Wrap<T> {\n    pub v: T,\n}\n\nimpl<T: Clone + Default> Wrap<T> {\n    pub fn get(&self) -> T {\n        self.v.clone()\n    }\n}\n\npub struct P {\n    pub x: u32,\n}\n\nimpl P {\n    pub fn new() -> P {\n        P { x: P::X }\n    }\n\n    pub const X: u32 = 1;\n}\n";
    let user = "use crate::forms::{a, b, c};\nuse crate::forms::a::LIMIT;\n\npub fn g(w: &crate::forms::Wrap<u32>) -> u32 {\n    a::f() + b::f() + c::f() + w.get() + crate::forms::P::new().x + LIMIT\n}\n";
    let (lib_file, forms_file, user_file) = ("crates/shop-core/src/lib.rs", "crates/shop-core/src/forms.rs", "crates/shop-core/src/user.rs");
    write(repo, lib_file, lib);
    write(repo, forms_file, forms);
    write(repo, user_file, user);
    commit_all(repo, "forms");
    let base = [(forms_file, forms), (user_file, user)];
    // A new order of a `use` list.
    let out = step_and_undo(repo, &base, &[(user_file, &user.replace("{a, b, c}", "{c, a, b}"))], "use order");
    assert!(fact_line(&out, "changed `use` line").ends_with("[import · code equal]"), "{out}");
    // A new order of the bounds of an impl.
    let out = step_and_undo(repo, &base, &[(forms_file, &forms.replace("T: Clone + Default", "T: Default + Clone"))], "bounds order");
    assert!(fact_line(&out, "order only").ends_with("code equal]"), "{out}");
    // A new order of the members of an impl.
    let reordered = forms.replace("    pub fn new() -> P {\n        P { x: P::X }\n    }\n\n    pub const X: u32 = 1;\n", "    pub const X: u32 = 1;\n\n    pub fn new() -> P {\n        P { x: P::X }\n    }\n");
    let out = step_and_undo(repo, &base, &[(forms_file, &reordered)], "member order");
    assert!(!out.contains("code different]"), "{out}");
    // A moved constant with a new value: the value fact owns the change; the move has one key.
    let moved = forms.replace("\n    pub const LIMIT: u32 = 10;\n", "").replace("pub mod b {\n", "pub mod b {\n    pub const LIMIT: u32 = 12;\n\n");
    let out = step_and_undo(repo, &base, &[(forms_file, &moved), (user_file, &user.replace("forms::a::LIMIT", "forms::b::LIMIT"))], "move the limit");
    assert!(fact_line(&out, "moved `shop_core::forms::a::LIMIT` → `shop_core::forms::b::LIMIT`").contains("code equal]") && !out.contains("! the compiled code changes"), "{out}");
    assert!(fact_line(&out, "value of `shop_core::forms::b::LIMIT` changed").ends_with("code different]") && !out.contains("through: `shop_core::forms::a::LIMIT`"), "{out}");
    // A `use` line that routes a call to a different function is a cause.
    let routed = "use crate::forms::{b as a, a as b, c};\nuse crate::forms::a::LIMIT;\n\npub fn g(w: &crate::forms::Wrap<u32>) -> u32 {\n    a::f() + b::f() + c::f() + w.get() + crate::forms::P::new().x + LIMIT\n}\n";
    let out = step_and_undo(repo, &base, &[(user_file, routed)], "route");
    // The mismatch mark of an import names the body (design 35.2).
    assert!(fact_line(&out, "`use` line").ends_with("[import · code different]") && out.contains("! the compiled code changes: shop_core::user::g"), "{out}");
}

/// Design 44, fix round 1 (D5): a link explains a changed element that the two sides have at
/// one place. New code at the place of an edit gets its cause from that edit only.
#[test]
fn new_code_gets_its_cause_from_its_edit_and_a_changed_constructor_from_the_new_field() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/shapes.rs";
    let base = "pub struct P {\n    pub x: u32,\n}\n\npub fn origin() -> P {\n    P { x: 0 }\n}\n\npub fn f(x: u32) -> u32 {\n    x + 1\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod shapes;\n");
    write(repo, file, base);
    commit_all(repo, "shapes");
    // An added struct that only new code uses: no compiled code of its own.
    let added = base.replace("pub fn f(x: u32) -> u32 {\n    x + 1\n}", "pub struct S {\n    pub v: u32,\n}\n\npub fn f(x: u32) -> u32 {\n    let s = S { v: x };\n    s.v + 1\n}");
    let out = step_and_undo(repo, &[(file, base)], &[(file, &added)], "a new struct");
    assert!(fact_line(&out, "added `shop_core::shapes::S`").ends_with("[added · no code]"), "{out}");
    // An added field: the constructor of a user changes.
    let field = base.replace("    pub x: u32,\n}", "    pub x: u32,\n    pub y: u32,\n}").replace("P { x: 0 }", "P { x: 0, y: 0 }");
    let out = step_and_undo(repo, &[(file, base)], &[(file, &field)], "a new field");
    assert!(fact_line(&out, "field `shop_core::shapes::P.y` added").ends_with("[added · code different]"), "{out}");
}

/// Design 44, fix round 1 (D1): a constant follows the chain of `const fn` calls of its value to
/// the edit of a body. The step is accepted with no `indirect` fact.
#[test]
fn a_constant_follows_a_chain_of_const_fn_to_a_changed_body() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/table.rs";
    let base = "pub const fn build(x: u32) -> u32 {\n    x + 1\n}\n\npub const fn make(x: u32) -> u32 {\n    build(x) * 2\n}\n\npub const A: u32 = make(3);\n\npub fn f(y: u32) -> u32 {\n    y + A\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod table;\n");
    write(repo, file, base);
    commit_all(repo, "table");
    write(repo, file, &base.replace("x + 1", "x + 2"));
    let out = run(repo, &["step", "a new table"], 0);
    assert!(!out.contains("[indirect") && fact_line(&out, "changed literal `1` → `2`").ends_with("code different]") && out.contains("shop_core::table::f"), "{out}");
}

/// Design 44, fix round 2: an associated constant follows the `const fn` of its value (R1); an
/// added `use` line for a moved item is no cause (R2); the receiver of a method call follows
/// the call (R3); the code of a derive belongs to the edit of its list (R4).
#[test]
fn associated_constants_new_use_lines_receivers_and_derive_lists_have_their_causes() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/round.rs";
    let lib = ("crates/shop-core/src/lib.rs", "pub mod cart;\npub mod round;\n");
    let table = "pub struct Table {\n    pub enc: [u8; 4],\n}\n\nimpl Table {\n    pub const FIRST: &'static Self = &Self::make_unwrap(b\"abcd\");\n    pub const SECOND: &'static Self = Self::FIRST;\n\n    pub const fn make(base: &[u8; 4]) -> Result<Self, ()> {\n        let mut enc = [0; 4];\n        let mut i = 0;\n        while i < 4 {\n            enc[i] = base[i];\n            i += 1;\n        }\n        Ok(Table { enc })\n    }\n\n    pub const fn make_unwrap(base: &[u8; 4]) -> Self {\n        match Self::make(base) {\n            Ok(table) => table,\n            Err(()) => panic!(\"bad table\"),\n        }\n    }\n}\n\npub fn first() -> u8 {\n    Table::SECOND.enc[0]\n}\n";
    let show = "use std::fmt;\n\npub struct Wrapper<T>(pub T);\n\nimpl fmt::Display for Wrapper<&str> {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        write!(f, \"[{}]\", self.0)\n    }\n}\n\npub fn show(text: &str) -> String {\n    Wrapper(text).to_string()\n}\n\n#[derive(Copy, Clone, Debug)]\npub struct Pair {\n    pub x: u32,\n}\n\npub mod a {\n    const LIMIT: u64 = 400;\n\n    pub fn cycle() -> u64 {\n        LIMIT\n    }\n}\n";
    let base = format!("{table}\n{show}");
    write(repo, lib.0, lib.1);
    write(repo, file, &base);
    commit_all(repo, "round");
    let accepted = |out: &str| out.starts_with("accepted") && !out.contains("[indirect");
    // R1: the edit of the body of `make`, with the three bodies in its affected bodies.
    let out = step_and_undo(repo, &[(file, &base)], &[(file, &base.replace("enc[i] = base[i];", "enc[i] = base[i] + 1;"))], "R1");
    assert!(accepted(&out) && out.contains("shop_core::round::Table::FIRST, shop_core::round::Table::SECOND") && out.contains("shop_core::round::first"), "{out}");
    // R2: a moved constant with a new value and a new `use` line.
    let moved = base.replace("pub mod a {\n    const LIMIT: u64 = 400;\n\n", "pub mod b {\n    pub(crate) const LIMIT: u64 = 401;\n}\n\npub mod a {\n    use super::b::LIMIT;\n\n");
    let out = step_and_undo(repo, &[(file, &base)], &[(file, &moved)], "R2");
    assert!(fact_line(&out, "moved `shop_core::round::a::LIMIT` → `shop_core::round::b::LIMIT`").ends_with("[move · code equal]") && !out.contains("! the compiled code changes"), "{out}");
    assert!(fact_line(&out, "value of `shop_core::round::b::LIMIT` changed").ends_with("code different]"), "{out}");
    // R3: a new trait method that the call of `show` resolves to.
    let trait_added = format!("{base}\npub trait Show {{\n    fn to_string(self) -> String;\n}}\n\nimpl Show for Wrapper<&str> {{\n    fn to_string(self) -> String {{\n        String::new()\n    }}\n}}\n");
    let out = step_and_undo(repo, &[(file, &base)], &[(file, &trait_added)], "R3");
    assert!(accepted(&out) && fact_line(&out, "trait impl `<Wrapper<&str> as Show>` added").ends_with("code different]"), "{out}");
    // R4: a derive list without `Copy`: the derived `clone` belongs to the edit of the list.
    let out = step_and_undo(repo, &[(file, &base)], &[(file, &base.replace("#[derive(Copy, Clone, Debug)]", "#[derive(Clone, Debug)]"))], "R4");
    assert!(accepted(&out) && fact_line(&out, "derive removed: Copy").ends_with("code different]"), "{out}");
}

/// Design 45 (batch 8g): the dependence in a body. A changed type of a `let`, a literal suffix,
/// and an auto-ref of a receiver and of the operands of `==`: each changed element goes to the
/// edit of the local that it depends on, with no `indirect` fact and no doubt. The needle keeps
/// its own fact. The safety forms (a swap of two statements that write one local, a rename that
/// also changes a type) stay `code different`. A type that inference gives to a different local
/// follows its reader (fix round 1); a cast that the new type makes the identity keeps an
/// `indirect` fact.
#[test]
fn the_dependence_gives_each_changed_element_to_the_edit_of_its_local() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/dep.rs";
    let base = "use std::num::Wrapping;\n\npub fn f(a: u8) -> u64 {\n    let x: u32 = a.into();\n    let p = x as u64 + 1;\n    let q = (x as u64) * 2;\n    p + q\n}\n\npub fn s(a: u64) -> u64 {\n    let x = 5u32;\n    let y = x.wrapping_mul(3);\n    y as u64 + a\n}\n\npub fn r(a: u8) -> u32 {\n    let x: Wrapping<u32> = Wrapping(a.into());\n    let s = x.to_string();\n    s.len() as u32\n}\n\npub fn e(a: u8, b: u8) -> bool {\n    let x: Wrapping<u32> = Wrapping(a.into());\n    let y: Wrapping<u32> = Wrapping(b.into());\n    x == y\n}\n\npub fn w(a: u32, b: u32) -> u32 {\n    let mut s = 0;\n    s = a;\n    s = b;\n    s + 1\n}\n\npub fn i(a: u8, b: u8) -> u64 {\n    let x: Wrapping<u32> = Wrapping(a.into());\n    let y = Wrapping(b.into());\n    let s = x + y;\n    s.0 as u64\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod dep;\n");
    write(repo, file, base);
    commit_all(repo, "dep");
    let clean = |out: &str| out.starts_with("accepted") && !out.contains("[indirect") && !out.contains("code equal]");
    let step = |text: String, intent: &str| step_and_undo(repo, &[(file, base)], &[(file, &text)], intent);
    // The `let` of 44.5: before 45, a rename `u32` → `u16` with `code equal` or the doubt rule.
    let typed = base.replace("    let x: u32 = a.into();", "    let x: u16 = a.into();");
    let out = step(typed.clone(), "type");
    assert!(clean(&out) && fact_line(&out, "changed pattern `x: u32` → `x: u16` at `shop_core::dep::f/S0/pat`").ends_with("[semantics · code different]") && !out.contains("renamed local"), "{out}");
    let out = step(base.replace("5u32", "5u16"), "suffix");
    assert!(clean(&out) && fact_line(&out, "changed literal `5u32` → `5u16`").ends_with("[semantics · code different]"), "{out}");
    let receiver = base.replace("    let x: Wrapping<u32> = Wrapping(a.into());\n    let s = x.to_string();", "    let x: Wrapping<u64> = Wrapping(a.into());\n    let s = x.to_string();");
    let out = step(receiver, "receiver");
    assert!(clean(&out) && out.matches(" derived: ").count() == 1, "{out}");
    let operands = base.replace("    let x: Wrapping<u32> = Wrapping(a.into());\n    let y: Wrapping<u32> = Wrapping(b.into());\n    x == y", "    let x: Wrapping<u64> = Wrapping(a.into());\n    let y: Wrapping<u64> = Wrapping(b.into());\n    x == y");
    let out = step(operands, "operands");
    assert!(clean(&out) && out.matches(" derived: ").count() == 2, "{out}");
    // The needle: a change of behavior in the same body keeps its own fact.
    let out = step(typed.replace("    p + q\n", "    p - q\n"), "needle");
    assert!(clean(&out) && fact_line(&out, "changed operator `p + q` → `p - q`").ends_with("[semantics · code different]") && fact_line(&out, "changed pattern `x: u32`").ends_with("code different]"), "{out}");
    // The safety forms.
    let out = step(base.replace("    s = a;\n    s = b;\n", "    s = b;\n    s = a;\n"), "swap");
    assert!(clean(&out) && fact_line(&out, "moved statement `s = a`").ends_with("[semantics · code different]"), "{out}");
    let renamed = base.replace("    let x: u32 = a.into();\n    let p = x as u64 + 1;\n    let q = (x as u64) * 2;", "    let y: u16 = a.into();\n    let p = y as u64 + 1;\n    let q = (y as u64) * 2;");
    let out = step(renamed, "rename and type");
    assert!(clean(&out) && fact_line(&out, "changed pattern `x: u32` → `y: u16`").ends_with("[semantics · code different]"), "{out}");
    // Inference gives `y` the new type: since fix round 1, `y` takes the causes of its reader
    // `x + y`. The cast `s.0 as u64` that the new type makes the identity keeps its fact.
    let out = step(base.replace("    let x: Wrapping<u32> = Wrapping(a.into());\n    let y = Wrapping(b.into());", "    let x: Wrapping<u64> = Wrapping(a.into());\n    let y = Wrapping(b.into());"), "inference");
    let rest = fact_line(&out, "the compiled code of `shop_core::dep::i` changes with no edit at its place");
    assert!(rest.contains("s.0") && !rest.contains("Wrapping(b.into())") && !rest.contains("x + y"), "{out}");
    // The counts of `semilla status --verbose`.
    write(repo, file, &typed);
    run(repo, &["step", "type"], 0);
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(out.contains("; changed elements by cause: ") && out.contains("dependence "), "{out}");
}

/// Design 45, fix round 1 (G1, core rule 6): a `let` with no type annotation gets its type by
/// inference from the elements that read it. The changed callee `take` gives `n` a new type; the
/// call has a link, and `n` takes the causes of its reader. The step needs no reason. A `Vec`
/// that a changed literal of `push` gives a new type is the same form.
#[test]
fn a_local_with_no_type_annotation_takes_the_causes_of_its_readers() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/infer.rs";
    let base = "pub fn take(v: u32) -> u64 {\n    v as u64 * 3\n}\n\npub fn caller(k: bool) -> u64 {\n    let n = 5;\n    if k { take(n) } else { 0 }\n}\n\npub fn count(k: u32) -> usize {\n    let mut v = Vec::new();\n    for _ in 0..k {\n        v.push(7u8);\n    }\n    v.len()\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod infer;\n");
    write(repo, file, base);
    commit_all(repo, "infer");
    let out = step_and_undo(repo, &[(file, base)], &[(file, &base.replace("pub fn take(v: u32) -> u64 {\n    v as u64 * 3", "pub fn take(v: u64) -> u64 {\n    v * 3"))], "take");
    assert!(out.starts_with("accepted") && !out.contains("[indirect") && !out.contains("needs a reason"), "{out}");
    let out = step_and_undo(repo, &[(file, base)], &[(file, &base.replace("7u8", "7u16"))], "push");
    assert!(out.starts_with("accepted") && !out.contains("[indirect") && fact_line(&out, "changed literal `7u8` → `7u16`").ends_with("[semantics · code different]"), "{out}");
}

/// Design 45, fix round 1 (G2, core rule 4): the region of a move is a local cause. A link of a
/// changed bound (or a dependence) adds a cause to an element of the region; it never takes the
/// element from the move. The swap stays `code different`, also with a changed `derive` list.
#[test]
fn a_link_never_takes_the_region_of_a_move() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/swap.rs";
    let swap_a = "        self.core.generate(&mut self.results);\n        self.index = index;\n";
    let swap_b = "        self.index = index;\n        self.core.generate(&mut self.results);\n";
    let base = format!("pub trait Source {{\n    fn generate(&mut self, out: &mut Vec<u32>);\n}}\n\n#[derive(Clone)]\npub struct Rng<R: Source + ?Sized> {{\n    pub results: Vec<u32>,\n    pub index: usize,\n    pub core: R,\n}}\n\nimpl<R: Source> Rng<R> {{\n    pub fn generate_and_set(&mut self, index: usize) {{\n        assert!(index < self.results.len());\n{swap_a}    }}\n}}\n");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod swap;\n");
    write(repo, file, &base);
    commit_all(repo, "swap");
    let moved = "moved statement `self.core.generate(&mut self.results)`";
    for (form, text) in [("bound", base.replace("R: Source + ?Sized", "R: Source")), ("derive", base.replace("#[derive(Clone)]", "#[derive(Clone, Debug)]"))] {
        let out = step_and_undo(repo, &[(file, &base)], &[(file, &text.replace(swap_a, swap_b))], form);
        assert!(fact_line(&out, moved).ends_with("[semantics · code different]"), "{form}\n{out}");
    }
}

/// Design 47 (G1): no order in the canonical text reads the text of a local number. The new
/// `let` gives the temporaries of `(b + 1) * (c + 1)` numbers with two digits, and the order of
/// the two operands stays. The step has no `indirect` fact. Negative: a swap of the operands of
/// a subtraction stays `code different`.
#[test]
fn a_new_local_number_does_not_change_the_order_of_two_operands() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/order.rs";
    let base = "pub fn f(a: u32, b: u32, c: u32, d: u32) -> u32 {\n    let p0 = a / 2;\n    let p1 = d / 3;\n    let m = (b + 1) * (c + 1);\n    m ^ p0 ^ p1\n}\n\npub fn g(a: u32, b: u32) -> u32 {\n    a - b\n}\n";
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod order;\n");
    write(repo, file, base);
    commit_all(repo, "order");
    let added = base.replace("    let m = ", "    let e = a / 99;\n    let m = ").replace("m ^ p0 ^ p1", "m ^ p0 ^ p1 ^ e");
    let out = step_and_undo(repo, &[(file, base)], &[(file, &added)], "add");
    assert!(out.starts_with("accepted") && !out.contains("[indirect") && out.contains("code different]"), "{out}");
    let out = step_and_undo(repo, &[(file, base)], &[(file, &base.replace("a - b", "b - a"))], "swap");
    assert!(out.starts_with("accepted") && !out.contains("code equal]") && out.contains("code different]"), "{out}");
}

// ---- Batch 9a4 (design 53): the build check of a step ----

/// Runs semilla with the build check of `cargo check` (design 53). Gives the exit code and the
/// text of stdout and stderr. Design 83: the build of the compiled check is the build check of a
/// step, so a `step` command here has `--fast`: its compiled check runs later, and the command
/// runs `cargo check`. A tag, a replay, and `old-step end` run `cargo check` too.
fn semilla_checked(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut args: Vec<&str> = args.to_vec();
    if args.first() == Some(&"step") && !args.contains(&"--fast") {
        args.push("--fast");
    }
    let args = &args[..];
    let mut all = vec![("SEMILLA_NO_WORKER", "1")];
    all.extend(env.iter().copied());
    let output = semilla_env(repo, args, &all);
    (output.status.code(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

/// The stored steps of the session file.
fn session_steps(repo: &Path) -> Vec<serde_json::Value> {
    let session: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap()).unwrap();
    session["changeset"]["steps"].as_array().cloned().unwrap_or_default()
}

/// The folder of the build check in the cache of the repository.
fn check_folder(repo: &Path) -> PathBuf {
    repo_cache(repo).join("check")
}

/// A cart with a type error in `total`.
fn cart_type_error() -> String {
    CART_V0.replace("        s\n    }", "        \"sum\"\n    }")
}

// rule+ R53.1-2
// rule- R53.1-3
// rule- R53.3-2 R53.3-5 R53.3-9 R53.6-4
#[test]
fn a_step_that_builds_has_the_result_line_and_a_review_reads_the_record() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[]);
    assert_eq!(code, Some(0), "{step}");
    let lines: Vec<&str> = step.lines().collect();
    assert!(lines[0].starts_with("accepted (phase 1; phase 2 runs in the background: `semilla status`): step 0"), "{step}");
    assert!(lines[1].starts_with("build check: passes (") && lines[1].ends_with(" s)"), "{step}");
    // The check adds no fact and no compare result.
    assert!(fact_line(&step, "changed literal `10` → `20`").ends_with("[semantics · pending]"), "{step}");
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "passes"}));
    // A check that passes has no line of a different result, no way out, and no record of a reject.
    assert!(!step.contains("does not build") && !step.contains("--no-check") && !step.contains("not run") && !step.contains("! build check"), "{step}");
    let session: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap()).unwrap();
    assert!(session["rejected"].is_null(), "{session}");
    // The check has its own folder in the cache. The repository has no `target` folder.
    assert!(check_folder(repo).join("target").is_dir() && !repo.join("target").exists());

    // A review reads the record. It does not run the check: no result is new in the cache.
    let results = || std::fs::read_dir(check_folder(repo).join("results")).unwrap().count();
    let before = results();
    let (code, status) = semilla_checked(repo, &["status"], &[]);
    assert_eq!(code, Some(0), "{status}");
    assert!(status.contains("\nbuild check of step 0: passes\n"), "{status}");
    let report = dir.path().join("review.md");
    let (code, out) = semilla_checked(repo, &["review", "--md", report.to_str().unwrap()], &[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(std::fs::read_to_string(&report).unwrap().contains("build check of step 0: passes"));
    assert_eq!(results(), before);

    // The same tree again: the stored result of the tree, and no new build.
    run(repo, &["undo"], 0);
    let (code, again) = semilla_checked(repo, &["step", "raise the limit"], &[]);
    assert_eq!(code, Some(0), "{again}");
    assert!(again.contains("\nbuild check: passes (a stored result of this tree)\n"), "{again}");
}

// rule+ R53.1-3
// rule- R53.1-2 R53.1-7
// rule+ R53.3-2 R53.3-4 R53.3-5 R53.3-12
// rule- R53.3-12 R53.4-7
#[test]
fn a_step_with_a_type_error_is_rejected_with_the_error_of_the_compiler() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &cart_type_error());
    let (code, step) = semilla_checked(repo, &["step", "give the sum as a text"], &[]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("rejected: step 0 \"give the sum as a text\" is not stored\n  ✗ build check: fails ("), "{step}");
    assert!(step.contains("after the step, the targets `shop-core` (lib test), `shop-core` (lib) do not build"), "{step}");
    // The error of the compiler has the file and the line. It shows one time.
    assert!(step.contains("\n\n    error[E0308]: mismatched types\n      --> crates/shop-core/src/cart.rs:13:9\n"), "{step}");
    assert_eq!(step.matches("error[E0308]").count(), 1, "{step}");
    // The facts of the syntax phase come after the errors. Then the two ways out.
    let fact = step.find("◆").expect("a fact line");
    assert!(step.find("error[E0308]").unwrap() < fact, "{step}");
    let end = &step[fact..];
    assert!(end.contains("the code after the step does not build: correct the code, then run: semilla step 'give the sum as a text' --fast\n"), "{step}");
    assert!(session_steps(repo).is_empty());
    // The record of the reject has the cause. Each failed target is a target of the step.
    let session: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".semilla/session.json")).unwrap()).unwrap();
    assert_eq!(session["rejected"]["cause"], "the build check fails: the code after the step does not build", "{session}");
    assert!(!step.contains("not a cause of this result"), "{step}");

    // `SEMILLA_NO_CHECK` with the value `0` or with an empty value does not turn the check off.
    for value in ["0", ""] {
        let (code, step) = semilla_checked(repo, &["step", "give the sum as a text"], &[("SEMILLA_NO_CHECK", value)]);
        assert_eq!(code, Some(1), "{step}");
        assert!(step.contains("✗ build check: fails (") && step.contains("error[E0308]"), "`{value}`: {step}");
    }
    assert!(session_steps(repo).is_empty());

    // The route of the old protocol has the same check.
    write(repo, "crates/shop-core/src/cart.rs", CART_V0);
    run(repo, &["old-step", "begin", "give the sum as a text"], 0);
    write(repo, "crates/shop-core/src/cart.rs", &cart_type_error());
    let (code, ended) = semilla_checked(repo, &["old-step", "end"], &[]);
    assert_ne!(code, Some(0), "{ended}");
    assert!(ended.contains("rejected: the step is not stored, and it stays open") && ended.contains("error[E0308]") && ended.contains("correct the code, then run `semilla old-step end` again"), "{ended}");
    // The step stays open: the corrected code ends it.
    write(repo, "crates/shop-core/src/cart.rs", CART_V0);
    let (code, ended) = semilla_checked(repo, &["old-step", "end"], &[]);
    assert_eq!(code, Some(0), "{ended}");
    assert!(ended.contains("build check: passes") && !ended.contains("rejected"), "{ended}");
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "passes"}));
}

// rule+ R53.1-3
#[test]
fn a_rename_that_misses_one_call_is_rejected() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // The method gets a new name. The call in the binary keeps the old name.
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("pub fn total(", "pub fn sum("));
    let (code, step) = semilla_checked(repo, &["step", "rename `total` to `sum`"], &[]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("rejected: step 0"), "{step}");
    assert!(step.contains("after the step, the targets `shop-app` (bin \"shop-app\" test), `shop-app` (bin \"shop-app\") do not build"), "{step}");
    assert!(step.contains("error[E0599]: no method named `total` found for struct `Cart`") && step.contains(" --> crates/shop-app/src/main.rs:3:25\n"), "{step}");
    assert!(session_steps(repo).is_empty());
    // With the call, the step builds.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("cart.total(", "cart.sum("));
    let (code, step) = semilla_checked(repo, &["step", "rename `total` to `sum`"], &[]);
    assert_eq!(code, Some(0), "{step}");
    assert!(step.contains("\nbuild check: passes ("), "{step}");
}


// rule+ R53.1-8
// rule+ R53.3-9 R53.4-5 R53.4-7 R53.6-2
// rule- R53.4-6 R53.4-8
#[test]
fn a_target_that_does_not_build_in_the_base_does_not_reject_the_step() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // The base has a test target that does not build.
    write(repo, "crates/shop-core/tests/old.rs", "#[test]\nfn old() {\n    let count: u32 = \"three\";\n    assert_eq!(count, 3);\n}\n");
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a test that does not build"]);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[]);
    assert_eq!(code, Some(0), "{step}");
    // The result word is not `passes`: a target of the tree does not build. The line below
    // says what Semilla did not check.
    let limit = "the target `shop-core` (test \"old\") does not build before the step and after it, so Semilla did not check a new error of the step in this target, or in a target that needs it";
    let lines: Vec<&str> = step.lines().collect();
    assert!(lines[1].starts_with("build check: no new failure found (") && lines[1].ends_with(" s)"), "{step}");
    assert_eq!(lines[2], format!("  ! build check: {limit}"), "{step}");
    assert!(!step.contains("three") && !step.contains("passes"), "{step}");
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "base_fails", "targets": ["`shop-core` (test \"old\")"]}));
    let status = run(repo, &["status"], 0);
    assert!(status.contains(&format!("! build check of step 0: no new failure found: {limit}\n")) && !status.contains("passes"), "{status}");
    let report = dir.path().join("review.md");
    run(repo, &["review", "--md", report.to_str().unwrap()], 0);
    let review = std::fs::read_to_string(&report).unwrap();
    assert!(review.contains("build check of step 0: no new failure found: the target") && !review.contains("build check of step 0: passes"), "{review}");

    // A second error, in a different target: the step is rejected. The message shows the
    // error of the step, not the error of the base.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("cart.total(20)", "cart.total(\"all\")"));
    let (code, step) = semilla_checked(repo, &["step", "give a text as the limit"], &[]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("rejected: step 1"), "{step}");
    assert!(step.contains("`shop-app` (bin \"shop-app\")") && step.contains("(not a cause of this result: the target `shop-core` (test \"old\") does not build before the step too)"), "{step}");
    assert!(step.contains(" --> crates/shop-app/src/main.rs:3:31\n") && !step.contains("three"), "{step}");
    assert_eq!(session_steps(repo).len(), 1);
}

// rule+ R53.1-7
// rule+ R53.3-7
// rule- R53.3-8
#[test]
fn a_build_check_that_cannot_run_does_not_store_the_step() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // A rustup home with the tool chain of the driver only (design 83: the compiled build).
    let home = rustup_home_with_proj(dir.path(), "proj-1.0");
    let env = [("RUSTUP_HOME", home.to_str().unwrap()), ("RUSTUP_TOOLCHAIN", "stable")];
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &env);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("not stored: step 0 \"raise the limit\": the build check cannot run on this machine\n  ! build check: cannot run: the toolchain `stable` (the variable RUSTUP_TOOLCHAIN) is not on this machine: "), "{step}");
    assert!(step.contains("The build check does not download a toolchain. Run `rustup toolchain install stable`\n"), "{step}");
    assert!(!step.contains("rejected") && !step.contains("fails"), "{step}");
    assert!(step.contains("correct the environment (the cause is above), then run: semilla step 'raise the limit'"), "{step}");
    // The facts of the syntax phase show.
    assert!(fact_line(&step, "changed literal `10` → `20`").ends_with("[semantics · pending]"), "{step}");
    assert!(session_steps(repo).is_empty());
    // The way out: an environment with the tool chain.
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[]);
    assert_eq!(code, Some(0), "{step}");
    assert!(step.contains("\nbuild check: passes ("), "{step}");
}

// rule+ R53.5-1 R53.5-2
// rule- R53.5-2 R53.5-3
#[test]
fn the_compiled_check_is_the_build_check_and_no_second_build_runs() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // A tree that does not build: the reject of the compiled check, as before design 53.
    write(repo, "crates/shop-core/src/cart.rs", &cart_type_error());
    let output = semilla_env(repo, &["step", "give the sum as a text"], &[]);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("mismatched types") && !text.contains("build check"), "{text}");
    assert!(session_steps(repo).is_empty() && !check_folder(repo).exists());
    // A tree that builds: the build of the compiled check gives the result.
    write(repo, "crates/shop-core/src/cart.rs", CART_V0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let output = semilla_env(repo, &["step", "raise the limit"], &[("SEMILLA_TIMING", "1")]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("\nbuild check: passes (the build of the compiled check)\n") && text.contains("code different]"), "{text}");
    // The record of the build check does not make the compiled check of the stored step run a
    // second time: the step result of this process serves the stored step.
    let timing = String::from_utf8_lossy(&output.stderr);
    assert_eq!(timing.matches("total of compare the bodies").count(), 1, "{timing}");
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "passes"}));
    assert!(!check_folder(repo).exists());
}

#[test]
fn the_mcp_tool_has_the_build_check() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &cart_type_error());
    let calls = mcp_calls_env(repo, &[("step", serde_json::json!({"intent": "give the sum as a text"})), ("step", serde_json::json!({"intent": "give the sum as a text", "fast": true}))], &[("SEMILLA_NO_WORKER", "1")]);
    // The build of the compiled check rejects the step with the error of the compiler.
    assert!(calls[0].1 && calls[0].0.contains("rejected: step 0") && calls[0].0.contains("error[E0308]: mismatched types"), "{}", calls[0].0);
    // With `fast`, `cargo check` is the build check.
    assert!(calls[1].1 && calls[1].0.contains("✗ build check: fails") && calls[1].0.contains("error[E0308]: mismatched types"), "{}", calls[1].0);
}

// rule+ R53.3-10 R53.3-11
// rule- R53.3-10 R53.3-11
#[test]
fn the_plan_route_has_the_build_check() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "raise the limit"]);
    run(repo, &["replay", "begin", "--commit", "HEAD"], 0);
    let plan = |replace: &str| {
        let plan = serde_json::json!({"steps": [{"intent": "raise the limit", "edits": [{"file": "crates/shop-app/src/main.rs", "find": "cart.total(10)", "replace": replace}]}]});
        std::fs::write(repo.join(".semilla/plan.json"), serde_json::to_string_pretty(&plan).unwrap()).unwrap();
    };
    // A step of the plan that does not build is a problem of the plan.
    plan("cart.total(\"all\")");
    let (_, out) = semilla_checked(repo, &["replay", "apply"], &[]);
    assert!(out.contains("✗ plan step 0") && out.contains("    ✗ the code after the step does not build\n      ✗ build check: fails ("), "{out}");
    assert!(out.contains("error[E0308]: mismatched types") && out.contains("a step that does not build needs a change of the plan"), "{out}");
    assert!(!out.contains("✓ plan step") && !out.contains("build check: passes"), "{out}");
    // A plan that builds.
    plan("cart.total(20)");
    let (code, out) = semilla_checked(repo, &["replay", "apply"], &[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("✓ plan step 0") && out.contains("    build check: passes ("), "{out}");
    assert!(!out.contains("does not build") && !out.contains("needs a change of the plan"), "{out}");
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "passes"}));
}

#[test]
fn a_dependency_that_is_not_in_the_cargo_cache_is_an_error_of_cargo_and_no_step() {
    // Cargo runs with `--offline`: this test opens no network connection. Design 83: the
    // compiled check reads the dependency graph first, so the error of cargo stops the command
    // (before, the build check of the default mode said "cannot run").
    let dir = setup();
    let repo = &dir.path().join("repo");
    let manifest = std::fs::read_to_string(repo.join("crates/shop-core/Cargo.toml")).unwrap();
    write(repo, "crates/shop-core/Cargo.toml", &format!("{manifest}\n[dependencies]\nzz-not-in-the-cargo-cache = \"1\"\n"));
    let reason = "crates/shop-core/Cargo.toml:dependencies.zz-not-in-the-cargo-cache=a test";
    let (code, step) = run_env(repo, &["step", "add a dependency", "--reason", reason], &[]);
    assert_eq!(code, 2, "{step}");
    assert!(step.starts_with("error: Semilla has no result: cargo cannot read the dependency graph of the tree after step 0"), "{step}");
    assert!(step.contains("no matching package named `zz-not-in-the-cargo-cache` found") && step.contains("run `cargo fetch`") && !step.contains("rejected"), "{step}");
    assert!(session_steps(repo).is_empty());
}

// ---- Batch 9a4b (design 53): the fix round of the build check ----

/// The example of design 53.4: the library of `shop` has a type error, and its integration
/// test has a different type error. Cargo does not check the test in this tree. The package
/// `tool` does not use `shop`, so it builds.
fn shop_with_two_errors() -> common::TestDir {
    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"shop\", \"tool\"]\n");
    write(repo, ".gitignore", "/target\n/.semilla/\n");
    write(repo, "shop/Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(repo, "shop/src/lib.rs", "pub fn price() -> u32 {\n    \"ten\"\n}\n");
    write(repo, "shop/tests/api.rs", "#[test]\nfn api() {\n    let count: u32 = \"three\";\n    assert_eq!(shop::price(), count);\n}\n");
    write(repo, "tool/Cargo.toml", "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(repo, "tool/src/main.rs", "fn main() {\n    let count: u32 = 3;\n    println!(\"{count}\");\n}\n");
    git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "base");
    dir
}

const SHOP_LIB_CORRECT: &str = "pub fn price() -> u32 {\n    10\n}\n";
const API_NOT_CHECKED: &str = "the target `shop` (test \"api\") does not build after the step, and cargo did not check it before the step";

// rule+ R53.4-6 R53.4-8
// rule- R53.4-5 R53.6-2
#[test]
fn a_target_that_cargo_did_not_check_in_the_base_does_not_reject_the_step() {
    let dir = shop_with_two_errors();
    let repo = &dir.path().join("repo");
    // The step corrects the library. Cargo now checks the test, and the old error of the test
    // shows. The step did not make it.
    write(repo, "shop/src/lib.rs", SHOP_LIB_CORRECT);
    let (code, step) = semilla_checked(repo, &["step", "correct the price"], &[]);
    assert_eq!(code, Some(0), "{step}");
    let lines: Vec<&str> = step.lines().collect();
    assert!(lines[0].starts_with("accepted (phase 1; phase 2 runs in the background: `semilla status`): step 0"), "{step}");
    assert!(lines[1].starts_with("build check: no new failure found ("), "{step}");
    assert_eq!(lines[2], format!("  ! build check: {API_NOT_CHECKED} (the targets `shop` (lib test), `shop` (lib) did not build then), so Semilla cannot tell if the step makes this error"), "{step}");
    assert!(!step.contains("rejected") && !step.contains("passes") && !step.contains("three"), "{step}");
    // The record keeps the two lists. The library builds after the step: the first list is empty.
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "base_fails", "targets": [], "not_checked": ["`shop` (test \"api\")"]}));
    let status = run(repo, &["status"], 0);
    assert!(status.contains(&format!("! build check of step 0: no new failure found: {API_NOT_CHECKED} (a different target did not build then), so Semilla cannot tell if the step makes this error\n")), "{status}");
}

// rule- R53.1-8
// rule+ R53.4-7
#[test]
fn an_error_in_a_target_that_builds_in_the_base_rejects_the_step_of_the_same_example() {
    let dir = shop_with_two_errors();
    let repo = &dir.path().join("repo");
    // The step corrects the library, and it adds an error to a target that builds in the base.
    write(repo, "shop/src/lib.rs", SHOP_LIB_CORRECT);
    write(repo, "tool/src/main.rs", "fn main() {\n    let count: u32 = \"3\";\n    println!(\"{count}\");\n}\n");
    let (code, step) = semilla_checked(repo, &["step", "correct the price"], &[]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("rejected: step 0"), "{step}");
    assert!(step.contains("after the step, the targets `tool` (bin \"tool\" test), `tool` (bin \"tool\") do not build\n"), "{step}");
    assert!(step.contains(&format!("    (not a cause of this result: {API_NOT_CHECKED})\n")), "{step}");
    // The message shows the error of the step, not the old error of the test.
    assert!(step.contains(" --> tool/src/main.rs:2:22\n") && !step.contains("three"), "{step}");
    assert!(session_steps(repo).is_empty());

    // A new target with an error: the base tree does not have it, so the step makes its error.
    write(repo, "tool/src/main.rs", "fn main() {\n    let count: u32 = 3;\n    println!(\"{count}\");\n}\n");
    write(repo, "shop/tests/more.rs", "#[test]\nfn more() {\n    let count: u32 = \"four\";\n    assert_eq!(count, 4);\n}\n");
    let (code, step) = semilla_checked(repo, &["step", "correct the price"], &[]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.contains("after the step, the target `shop` (test \"more\") does not build\n") && step.contains(" --> shop/tests/more.rs:3:22\n"), "{step}");
    assert!(session_steps(repo).is_empty());
}

// rule+ R53.6-5
#[test]
fn tag_prints_each_marked_line_of_the_build_check() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // The base has a test target that does not build: step 0 gets `no new failure found`.
    write(repo, "crates/shop-core/tests/old.rs", "#[test]\nfn old() {\n    let count: u32 = \"three\";\n    assert_eq!(count, 3);\n}\n");
    commit_all(repo, "a test that does not build");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[]);
    assert_eq!(code, Some(0), "{step}");
    // Step 1 has the same result: one line for the two steps.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("cart.total(20)", "cart.total(30)"));
    let (code, step) = semilla_checked(repo, &["step", "raise the limit again"], &[]);
    assert_eq!(code, Some(0), "{step}");
    let status = run(repo, &["status"], 0);
    let marked: Vec<&str> = status.lines().filter(|line| line.starts_with("! build check of ")).collect();
    assert_eq!(marked.len(), 1, "{status}");
    assert!(marked[0].starts_with("! build check of each step: no new failure found: "), "{status}");
    commit_all(repo, "raise the limit");
    // `tag` does not refuse the change set. It prints each marked line.
    let (code, tagged) = semilla_checked(repo, &["tag"], &[]);
    assert_eq!(code, Some(0), "{tagged}");
    assert!(tagged.contains("tagged ") && marked.iter().all(|line| tagged.contains(&format!("\n{line}\n"))), "{tagged}");
    assert_eq!(tagged.matches("build check").count(), 1, "{tagged}");
}

// rule- R53.6-5
#[test]
fn tag_prints_no_line_of_a_build_check_that_passes() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[]);
    assert_eq!(code, Some(0), "{step}");
    assert!(step.contains("\nbuild check: passes ("), "{step}");
    commit_all(repo, "raise the limit");
    let (code, tagged) = semilla_checked(repo, &["tag"], &[]);
    assert_eq!(code, Some(0), "{tagged}");
    assert!(tagged.contains("tagged ") && !tagged.contains("build check"), "{tagged}");
}

/// A rustup home with one tool chain, `proj-1.0`: a link to the tool chain of this test run.
/// The file `settings.toml` gives `default` as the default tool chain.
fn rustup_home_with_proj(dir: &Path, default: &str) -> PathBuf {
    let output = Command::new("rustc").current_dir(workspace_root()).args(["--print", "sysroot"]).output().unwrap();
    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    assert!(root.join("bin/cargo").is_file(), "the tool chain of the test run has no cargo: {}", root.display());
    let home = dir.join("rustup-home");
    if !home.exists() {
        std::fs::create_dir_all(home.join("toolchains")).unwrap();
        std::os::unix::fs::symlink(&root, home.join(format!("toolchains/proj-1.0-{}-unknown-test", std::env::consts::ARCH))).unwrap();
        // Design 83: each step has the compiled build, with the tool chain of the driver.
        std::os::unix::fs::symlink(&root, home.join("toolchains").join(root.file_name().unwrap())).unwrap();
    }
    std::fs::write(home.join("settings.toml"), format!("version = \"12\"\ndefault_toolchain = \"{default}\"\n")).unwrap();
    home
}

// rule+ R53.2-1
// rule+ R53.3-7
#[test]
fn the_build_check_uses_the_tool_chain_file_of_the_tree() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let home = rustup_home_with_proj(dir.path(), "proj-1.0");
    // The tree names a tool chain that is not on the machine. The variable and the default of
    // rustup name a tool chain that is there, but the file of the tree comes first. Semilla
    // does not download the tool chain: the check cannot run.
    write(repo, "rust-toolchain.toml", "[toolchain]\nchannel = \"gone-2.0\"\n");
    commit_all(repo, "pin a tool chain");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[("RUSTUP_HOME", home.to_str().unwrap()), ("RUSTUP_TOOLCHAIN", "proj-1.0")]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("not stored: step 0 \"raise the limit\": the build check cannot run on this machine\n  ! build check: cannot run: the toolchain `gone-2.0` (the file `rust-toolchain.toml` of the tree) is not on this machine: "), "{step}");
    assert!(step.contains("The build check does not download a toolchain. Run `rustup toolchain install gone-2.0`\n"), "{step}");
    assert!(session_steps(repo).is_empty() && !home.join("toolchains/gone-2.0").exists());

    // The tree names a tool chain of the machine. The variable names one that is not there.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V0);
    write(repo, "rust-toolchain.toml", "[toolchain]\nchannel = \"proj-1.0\"\n");
    commit_all(repo, "pin a tool chain of the machine");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[("RUSTUP_HOME", home.to_str().unwrap()), ("RUSTUP_TOOLCHAIN", "gone-2.0")]);
    assert_eq!(code, Some(0), "{step}");
    assert!(step.lines().nth(1).is_some_and(|line| line.starts_with("build check: passes (") && line.ends_with(" s)")), "{step}");
}

// rule- R53.2-1
// rule- R53.3-7
#[test]
fn a_tree_with_no_tool_chain_file_uses_the_default_of_the_machine() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    // The default of rustup is not on the machine, and no variable names a tool chain.
    let home = rustup_home_with_proj(dir.path(), "gone-2.0");
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[("RUSTUP_HOME", home.to_str().unwrap()), ("RUSTUP_TOOLCHAIN", "")]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.contains("  ! build check: cannot run: the toolchain `gone-2.0` (the default of rustup) is not on this machine: ") && step.contains("Run `rustup toolchain install gone-2.0`\n"), "{step}");
    assert!(session_steps(repo).is_empty());
    // The variable comes before the default of rustup.
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[("RUSTUP_HOME", home.to_str().unwrap()), ("RUSTUP_TOOLCHAIN", "proj-1.0")]);
    assert_eq!(code, Some(0), "{step}");
    assert!(step.contains("\nbuild check: passes ("), "{step}");
    // The default of rustup, with no variable. The tool chain is the same, so the stored
    // result of the tree applies.
    run(repo, &["undo"], 0);
    rustup_home_with_proj(dir.path(), "proj-1.0");
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[("RUSTUP_HOME", home.to_str().unwrap()), ("RUSTUP_TOOLCHAIN", "")]);
    assert_eq!(code, Some(0), "{step}");
    assert!(step.contains("\nbuild check: passes (a stored result of this tree)\n"), "{step}");
}

#[test]
fn cache_clean_removes_the_folder_of_the_build_check() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let build_code = "removed 0 entries of the build code cache that no stored step uses: 0 B freed\n";
    // No build check ran: there is no folder, and the output has no line for it.
    assert_eq!(run(repo, &["cache", "--clean"], 0), build_code);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let (code, step) = semilla_checked(repo, &["step", "raise the limit"], &[]);
    assert_eq!(code, Some(0), "{step}");
    assert!(check_folder(repo).join("target").is_dir());
    let cleaned = run(repo, &["cache", "--clean"], 0);
    let lines: Vec<&str> = cleaned.lines().collect();
    assert_eq!(lines.len(), 2, "{cleaned}");
    assert_eq!(format!("{}\n", lines[0]), build_code);
    assert!(lines[1].starts_with("removed the folder of the build check: ") && lines[1].ends_with(" freed (the next build check builds each dependency again)") && !lines[1].contains(": 0 B freed"), "{cleaned}");
    assert!(!check_folder(repo).exists());
    // The record of the step stays, and the second run finds no folder.
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "passes"}));
    assert_eq!(run(repo, &["cache", "--clean"], 0), build_code);
    // The next check is cold: it runs again and does not read a stored result.
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("cart.total(20)", "cart.total(30)"));
    let (code, step) = semilla_checked(repo, &["step", "raise the limit again"], &[]);
    assert_eq!(code, Some(0), "{step}");
    assert!(step.lines().nth(1).is_some_and(|line| line.starts_with("build check: passes (") && line.ends_with(" s)")), "{step}");
}

// Batch 9b (design 54): the facts of the agent, the gap list, and the full text.

const FACTS_BASE: &str = "pub struct Cart {\n    pub total: u32,\n    pub count: u32,\n}\nimpl Cart {\n    pub fn total(&self) -> u32 {\n        self.total\n    }\n}\npub fn a(c: &Cart) -> u32 {\n    c.total + 1\n}\npub fn b(c: &Cart) -> u32 {\n    c.total + 2\n}\npub fn d(c: &Cart, e: &Cart) -> u32 {\n    b(c) + e.total\n}\npub fn idle(x: u32) -> u32 {\n    x\n}\npub fn tail(x: u32) {\n    drop(x)\n}\npub mod inner {\n    pub fn a(x: u32) -> u32 {\n        x + 1\n    }\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert_eq!(super::inner::a(1), 2);\n    }\n}\n";

const LONG_LET: &str = "let scaled_total_with_a_long_name = x.wrapping_mul(31).wrapping_add(17).wrapping_mul(5).rotate_left(3) ^ 0x5bd1e995;";

/// The target of the step: a field read becomes a call at three sites, a literal changes in a
/// function and in its test, a function gets a long statement and a loop, and a tail gets a `;`.
fn facts_target() -> String {
    facts_edits().iter().fold(FACTS_BASE.to_string(), |text, (find, replace)| text.replace(find, replace))
}

/// The edits of [`facts_target`]: each one is a text to find and its new text.
fn facts_edits() -> Vec<(String, String)> {
    let long = format!("pub fn idle(x: u32) -> u32 {{\n    {LONG_LET}\n    for step in 0..scaled_total_with_a_long_name.min(4) {{\n        if step > 2 {{\n            return step + x.wrapping_mul(scaled_total_with_a_long_name);\n        }}\n    }}\n    x\n}}");
    let pairs = [
        ("c.total + 1", "c.total() + 1"),
        ("c.total + 2", "c.total() + 2"),
        ("e.total", "e.total()"),
        ("        x + 1\n", "        x + 5\n"),
        ("inner::a(1), 2", "inner::a(1), 6"),
        ("pub fn idle(x: u32) -> u32 {\n    x\n}", long.as_str()),
        ("    drop(x)\n", "    drop(x);\n"),
    ];
    pairs.iter().map(|(find, replace)| (find.to_string(), replace.to_string())).collect()
}

/// A repository with the module `shop_core::facts` in the base commit, and the target in the
/// working tree.
fn facts_setup() -> common::TestDir {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\npub mod facts;\n");
    write(repo, "crates/shop-core/src/facts.rs", FACTS_BASE);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "the facts module"]);
    write(repo, "crates/shop-core/src/facts.rs", &facts_target());
    dir
}

/// Runs Semilla in the default mode (the syntax tree model). Gives the exit code and the output.
fn tree_mode(repo: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = semilla_env(repo, args, &[]);
    (output.status.code(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

/// True if a fact line of `text` has the origin `stated`.
fn has_stated(text: &str) -> bool {
    text.lines().any(|line| line.trim_start().starts_with('◆') && line.contains(" stated: "))
}

const GAP_HEAD: &str = "that can change the behavior of existing code and that no fact of the agent states";

// rule+ R48.2-1
#[test]
fn a_step_with_no_agent_fact_is_valid_and_prints_the_gap_list_with_one_command() {
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    let (code, out) = tree_mode(repo, &["step", "use the method"]);
    assert_eq!(code, Some(0), "{out}");
    // The gap list: the largest group first, with the option that covers the group. The test
    // function is not in the list.
    let list = format!(
        "  not stated: 6 item(s) with 7 place(s) {GAP_HEAD}\n  ? 3 places: `$a.total` => `$a.total()` (f1, f2)\n      in: shop_core::facts::a, shop_core::facts::b, shop_core::facts::d\n      lines: crates/shop-core/src/facts.rs:11, crates/shop-core/src/facts.rs:14, crates/shop-core/src/facts.rs:17, crates/shop-core/src/facts.rs:-11, crates/shop-core/src/facts.rs:-14, crates/shop-core/src/facts.rs:-17\n      to state it: the check did not decide if the behavior changes: if it changes, --change 'shop_core::facts::a,shop_core::facts::b,shop_core::facts::d=TEXT'; if it does not change, --same 'rewrite $a.total => $a.total()'\n  ? 2 places: 2 different edits (f3, f4)\n      in: shop_core::facts::idle\n      lines: crates/shop-core/src/facts.rs:20-25\n      to state it: the check did not decide if the behavior changes: if it changes, --change 'shop_core::facts::idle=TEXT'; if it does not change, a `--same` fact (rename, move, or rewrite) that explains the edit\n  ? 1 place: changed literal `1` → `5` (f5)\n"
    );
    assert!(out.contains(&list), "{out}");
    assert!(!out.lines().any(|line| line.starts_with("      in: ") && line.contains("tests::t")), "{out}");
    // A line whose check did not decide has the two forms, and the command does not have it
    // (R71.3-1). [R84.4-1] "MIR changes" reads as "not decided": each line of this step has the
    // two forms, so the step has no command.
    assert!(out.contains("  ? 1 place: changed literal `1` → `5` (f5)\n      in: shop_core::facts::inner::a\n      lines: crates/shop-core/src/facts.rs:33, crates/shop-core/src/facts.rs:-27\n      to state it: the check did not decide if the behavior changes: if it changes, --change 'shop_core::facts::inner::a=TEXT'; if it does not change, --same 'rewrite 1 => 5'\n"), "{out}");
    assert!(!out.contains("  to state them, add facts to the stored step"), "{out}");
    // Each fact has the origin of Semilla, in the groups of design 48.3.
    assert!(out.contains("  tests and benches, filled in by Semilla:\n  ◆ f7 derived: changed literal `2` → `6` at `shop_core::facts::tests::t/S0/args[1]` [semantics · code different]\n"), "{out}");
    assert!(out.contains("  other facts, filled in by Semilla:\n  ◆ f1 derived: 2 site(s): changed expression `c.total` → `c.total()` [semantics · code different]\n"), "{out}");
    assert!(!has_stated(&out) && !out.contains("facts of the agent"), "{out}");

    // Part D (design 54.7). A long statement: the full text is in a block below the fact line.
    assert!(out.contains(&format!("  ◆ f3 derived: added `let` [1] at `shop_core::facts::idle/S0` [semantics · code different]\n        [1] {LONG_LET}\n")), "{out}");
    // A text with more than one line keeps its lines.
    assert!(
        out.contains("  ◆ f4 derived: added loop [1] at `shop_core::facts::idle/S1` [semantics · code different]\n        [1] for step in 0..scaled_total_with_a_long_name.min(4) {\n                if step > 2 {\n                    return step + x.wrapping_mul(scaled_total_with_a_long_name);\n                }\n            }\n"),
        "{out}"
    );
    // A tail expression that gets a `;`: the text shows the `;`.
    assert!(out.contains("  ◆ f6 derived: moved call: `drop(x)` → `drop(x);` from `tail` to `S0` in `shop_core::facts::tail` [semantics · code different]\n"), "{out}");
    // Negative: a short text stays in the fact line, and no fact text has a cut mark.
    assert!(out.contains("  ◆ f5 derived: changed literal `1` → `5` at `shop_core::facts::inner::a/tail/rhs` [semantics · code different]\n"), "{out}");
    assert!(!out.lines().any(|line| line.trim_start().starts_with('◆') && line.contains('…')), "{out}");

    // The status and the review have one line with the count.
    let (_, status) = tree_mode(repo, &["status"]);
    assert!(status.contains(&format!("  not stated: 6 item(s) with 7 place(s) {GAP_HEAD}\n")), "{status}");
    let md = repo.join(".semilla/review.md");
    tree_mode(repo, &["review", "--md", md.to_str().unwrap()]);
    let report = std::fs::read_to_string(&md).unwrap();
    assert!(report.contains(&format!("not stated: 6 item(s) with 7 place(s) {GAP_HEAD}")), "{report}");
    assert!(report.contains(&format!("  [1] {LONG_LET}\n")) && report.contains("          if step > 2 {\n              return step + x.wrapping_mul(scaled_total_with_a_long_name);\n") && report.contains("`drop(x)` → `drop(x);`"), "{report}");
    assert!(report.contains("#### Not stated (6)") && report.contains("Filled in by Semilla, tests and benches (1):") && !report.contains("Facts of the agent"), "{report}");
    // `tag` has the count line too.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "use the method"]);
    let (code, tag) = tree_mode(repo, &["tag"]);
    assert!(code == Some(0) && tag.contains(&format!("0 fact(s) of the agent, each one checked\nnot stated: 6 item(s) with 7 place(s) {GAP_HEAD}")), "{tag}");
}

// rule+ R54.12-2
// rule- R54.12-2
// rule+ R54.4-5 R54.4-6 R54.5-3
// rule- R54.4-5 R54.4-6
#[test]
fn the_fact_command_adds_facts_to_the_stored_step_with_no_second_build_check() {
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    // The step runs the build check one time.
    let checked = [];
    let output = semilla_env(repo, &["step", "use the method", "--same", "rewrite $a.total => $a.total()"], &checked);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success() && out.contains("build check: passes ("), "{out}{}", String::from_utf8_lossy(&output.stderr));
    assert!(out.contains("  ✓ a1 same: rewrite `$a.total` => `$a.total()` (the form holds: 3 site(s) have exactly this edit. Semilla does not check that the behavior is the same)\n      places: shop_core::facts::a/tail/lhs, shop_core::facts::b/tail/lhs, shop_core::facts::d/tail/rhs\n    ◆ f1 stated: 2 site(s): changed expression `c.total` → `c.total()` [semantics · code different]\n"), "{out}");
    // A partial fact set: the other items are in the gap list.
    assert!(out.contains(&format!("  not stated: 3 item(s) with 4 place(s) {GAP_HEAD}\n")), "{out}");
    let build = session_steps(repo)[0]["build_check"].clone();
    // The command `fact` adds facts. The build check does not run again: with a broken tool
    // chain variable the command still passes, and the stored result of the check stays.
    let output = semilla_env(repo, &["fact", "--change", "shop_core::facts::idle=idle scales the value", "--change", "inner=the fee is 5"], &[("CARGO", "/nonexistent/cargo")]);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{out}{}", String::from_utf8_lossy(&output.stderr));
    assert!(out.starts_with("stored: 2 new fact(s) of the agent in step 0 \"use the method\"; no build check runs again (the trees of the step are the same)\n") && !out.contains("build check:"), "{out}");
    assert!(out.contains("  ✓ a2 change: `shop_core::facts::idle` \"idle scales the value\" (holds: the behavior of existing code can change in each item; covers 2 fact(s) in 1 item(s) with 6 changed line(s))\n      places: shop_core::facts::idle/S0, shop_core::facts::idle/S1\n"), "{out}");
    assert!(out.contains("  ✓ a3 change: `inner` \"the fee is 5\" (holds: ") && out.contains(&format!("  not stated: 1 item(s) with 1 place(s) {GAP_HEAD}\n")), "{out}");
    // R71.3-1: the one gap line has the two forms, so the output has no one command.
    assert!(out.contains("      to state it: the check did not decide if the behavior changes: if it changes, --change 'shop_core::facts::tail=TEXT'; if it does not change, a `--same` fact (rename, move, or rewrite) that explains the edit\n") && !out.contains("to state them"), "{out}");
    let step = session_steps(repo)[0].clone();
    assert_eq!(step["build_check"], build);
    assert_eq!(step["agent"], serde_json::json!({"same": ["rewrite $a.total => $a.total()"], "change": ["shop_core::facts::idle=idle scales the value", "inner=the fee is 5"]}));
    // Negative: a false fact stores nothing, with the exit code 1 and the cause.
    let (code, out) = tree_mode(repo, &["fact", "--change", "shop_core::facts::Cart=the cart changes", "--change", "shop_core::facts::tail=the tail is a statement"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.starts_with("rejected: no fact is added to step 0 \"use the method\"\n  ✗ fact a4 of the agent is false: `--change 'shop_core::facts::Cart=the cart changes'`\n"), "{out}");
    assert!(out.contains("      Semilla sees no edit in `shop_core::facts::Cart`\n"), "{out}");
    assert_eq!(session_steps(repo)[0], step);
    // A form that is not valid, a step that does not exist, and a command with no fact: exit code 2.
    for (args, cause) in [(&["fact", "--same", "swap a b"][..], "the forms are `rename OLD NEW`, `move FROM TO`, and `rewrite FROM => TO`"), (&["fact", "--step", "4", "--change", "inner=x"][..], "step 4"), (&["fact"][..], "--same")] {
        let (code, out) = tree_mode(repo, args);
        assert!(code == Some(2) && out.contains(cause), "{args:?}: {code:?} {out}");
    }
    assert_eq!(session_steps(repo)[0], step);
    // The last gap: the list is empty.
    let (code, out) = tree_mode(repo, &["fact", "--change", "shop_core::facts::tail=the tail is a statement"]);
    assert!(code == Some(0) && out.contains("  not stated: 0 item(s)\n"), "{out}");

    // The record. `status` has the agent facts with their check results and the count line
    // of the gaps, with the count 0 (design 54.14).
    let (_, status) = tree_mode(repo, &["status"]);
    assert!(status.contains("  ✓ a1 same: rewrite `$a.total` => `$a.total()` (the form holds: 3 site(s) have exactly this edit. Semilla does not check that the behavior is the same)\n  ✓ a2 change: `shop_core::facts::idle` \"idle scales the value\" (holds: ") && status.contains("\n  not stated: 0 item(s)\n") && !status.contains("  ? "), "{status}");
    // `status --verbose` has the order of design 48.3: the agent facts with the facts below
    // them (origin `stated`), then the facts that Semilla filled in (origin `derived`).
    let (_, verbose) = tree_mode(repo, &["status", "--verbose"]);
    let at = |text: &str| verbose.find(text).unwrap_or_else(|| panic!("no `{text}` in:\n{verbose}"));
    assert!(at("  facts of the agent (Semilla checks each fact against the edit. It does not check the sentence of a `change`. It does not check the behavior after a `rewrite`):") < at("  ✓ a1 same: ") && at("  ✓ a1 same: ") < at("    ◆ f1 stated: ") && at("    ◆ f1 stated: ") < at("  ✓ a2 change: "), "{verbose}");
    assert!(at("    ◆ f6 stated: moved call: `drop(x)` → `drop(x);`") < at("  tests and benches, filled in by Semilla:\n  ◆ f7 derived: "), "{verbose}");
    assert!(verbose.contains(&format!("    ◆ f3 stated: added `let` [1] at `shop_core::facts::idle/S0` [semantics · code different]\n          [1] {LONG_LET}\n")), "{verbose}");
    // The review has the same order, with the origin of each fact.
    let md = repo.join(".semilla/review.md");
    tree_mode(repo, &["review", "--md", md.to_str().unwrap()]);
    let report = std::fs::read_to_string(&md).unwrap();
    let at = |text: &str| report.find(text).unwrap_or_else(|| panic!("no `{text}` in:\n{report}"));
    assert!(report.contains("facts of the agent: 4, each one checked against the edit") && report.contains("\nnot stated: 0 item(s)\n") && !report.contains("to state it"), "{report}");
    assert!(at("#### Facts of the agent (4)") < at("✓ a1 same: rewrite `$a.total` =&gt; `$a.total()` (the form holds: 3 site(s) have exactly this edit. Semilla does not check that the behavior is the same)") && at("f1 stated: ") < at("f7 derived: "), "{report}");
    // Design 54.12: the HTML export has the same words for the `rewrite` fact, and a `change`
    // fact keeps the word "holds".
    let html = repo.join(".semilla/review.html");
    tree_mode(repo, &["review", "--html", html.to_str().unwrap()]);
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("(the form holds: 3 site(s) have exactly this edit. Semilla does not check that the behavior is the same)") && page.contains("(holds: "), "{page}");
    // `tag` has the agent facts.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "use the method"]);
    let (code, tag) = tree_mode(repo, &["tag"]);
    assert_eq!(code, Some(0), "{tag}");
    assert!(tag.contains("  ✓ a1 same: rewrite `$a.total` => `$a.total()` (the form holds: 3 site(s) have exactly this edit. Semilla does not check that the behavior is the same)\n") && tag.contains("  ✓ a4 change: `shop_core::facts::tail` \"the tail is a statement\" (holds: ") && tag.contains("\nnot stated: 0 item(s)\n") && !tag.contains("to state it"), "{tag}");
    // The result of the tagged commit comes from the stored record.
    let (_, status) = tree_mode(repo, &["status", "HEAD"]);
    assert!(status.contains("  ✓ a2 change: `shop_core::facts::idle` \"idle scales the value\" (holds: "), "{status}");
}

// rule+ R54.5-2
#[test]
fn a_false_fact_of_the_agent_rejects_the_step_and_hides_no_edit() {
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    // The item `Cart` has no edit, and Semilla derives no rename of `a`.
    let (code, out) = tree_mode(repo, &["step", "use the method", "--same", "rewrite $a.total => $a.total()", "--same", "rename shop_core::facts::a shop_core::facts::first", "--change", "facts::Cart=the cart changes"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.starts_with("rejected: step 0 \"use the method\" is not stored\n"), "{out}");
    assert!(out.contains("  ✗ fact a2 of the agent is false: `--same 'rename shop_core::facts::a shop_core::facts::first'`\n") && out.contains("  ✗ fact a3 of the agent is false: `--change 'facts::Cart=the cart changes'`\n"), "{out}");
    assert!(out.contains("(false: Semilla derives no rename `shop_core::facts::a` → `shop_core::facts::first` from the syntax tree)\n") && out.contains("      Semilla sees no edit in `shop_core::facts::Cart`\n"), "{out}");
    // No edit is hidden: each fact of the step is in the output.
    for id in 1..=7 {
        assert_eq!(out.matches(&format!("◆ f{id} ")).count(), 1, "f{id}: {out}");
    }
    // The command to run again has the fact that holds, and not the false facts.
    assert!(out.trim_end().ends_with("semilla step 'use the method' --same 'rewrite $a.total => $a.total()'"), "{out}");
    assert!(!repo.join(".semilla/session.json").exists() || session_steps(repo).is_empty(), "a rejected step stores nothing");
    // A path with two matches: the candidates.
    let (code, out) = tree_mode(repo, &["step", "use the method", "--change", "a=the fee"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("      `a` names 2 items: give more of the path\n      candidates: `shop_core::facts::a`, `shop_core::facts::inner::a`\n"), "{out}");
    // The kinds of a later batch and a bad form stop before the check, with the exit code 2.
    for (option, cause) in [("extract shop_core::facts::a shop_core::facts::helper", "the kind `extract` has no check yet (a later batch adds it)"), ("inline shop_core::facts::a", "the kind `inline` has no check yet (a later batch adds it)"), ("rewrite $a.total", "FROM => TO")] {
        let (code, out) = tree_mode(repo, &["step", "use the method", "--same", option]);
        assert!(code == Some(2) && out.contains(cause), "{option}: {code:?} {out}");
    }
    let (code, out) = tree_mode(repo, &["step", "use the method", "--change", "shop_core::facts::a"]);
    assert!(code == Some(2) && out.contains("ITEM[,ITEM]=TEXT"), "{code:?} {out}");
    // Negative: the command of the last line is accepted, and a new item gets a note.
    let fresh = format!("{}pub fn fresh() -> u32 {{\n    7\n}}\n", facts_target());
    write(repo, "crates/shop-core/src/facts.rs", &fresh);
    let (code, out) = tree_mode(repo, &["step", "use the method", "--same", "rewrite $a.total => $a.total()", "--change", "fresh=a new function"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("  ✓ a2 change: `fresh` \"a new function\" (holds: no reject)\n      note: Semilla shows `shop_core::facts::fresh` as added: the fact covers no existing code\n"), "{out}");
    assert!(out.contains("  new and removed items, filled in by Semilla:\n  ◆ f8 derived: added `shop_core::facts::fresh`"), "{out}");
}

// rule- R54.3-4
#[test]
fn an_old_session_file_and_a_stored_rewrite_pattern_claim_still_read() {
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    let (code, out) = tree_mode(repo, &["step", "use the method", "--same", "rewrite $a.total => $a.total()"]);
    assert_eq!(code, Some(0), "{out}");
    let file = repo.join(".semilla/session.json");
    let mut session: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    // The form of a session from before this batch: no key `agent`, and the old claim.
    let step = session["changeset"]["steps"][0].as_object_mut().unwrap();
    assert!(step.remove("agent").is_some());
    step.insert("claims".into(), serde_json::json!([{"id": "c1", "op": "rewrite-pattern", "at": "shop_core::facts::a", "args": {"from": "$c.total", "to": "$c.total()"}}]));
    std::fs::write(&file, serde_json::to_string_pretty(&session).unwrap()).unwrap();
    let (code, status) = tree_mode(repo, &["status", "--verbose"]);
    assert_eq!(code, Some(0), "{status}");
    // The old claim keeps its check. The step has no agent fact, so each fact is `derived`.
    assert!(!status.contains("facts of the agent") && !has_stated(&status), "{status}");
    // The place that the old claim covers is not a gap.
    assert!(status.contains("  ✓ c1 verified (pattern (1 sites)) [semantics]\n") && status.contains(&format!("  not stated: 5 item(s) with 6 place(s) {GAP_HEAD}\n")), "{status}");
    // The agent adds a fact to the old record.
    let (code, out) = tree_mode(repo, &["fact", "--same", "rewrite $a.total => $a.total()"]);
    assert!(code == Some(0) && out.contains("(the form holds: 3 site(s) have exactly this edit. Semilla does not check that the behavior is the same)\n") && out.contains("1 of the site(s) are below a claim of the step: the claim shows them\n"), "{out}");
    assert!(out.contains("      places: shop_core::facts::b/tail/lhs, shop_core::facts::d/tail/rhs\n"), "{out}");
}

// rule+ R48.5-2 R54.13-1 R54.13-2 R54.13-3
// rule- R54.13-1 R54.13-2 R54.13-3
#[test]
fn the_mcp_tools_and_a_plan_step_take_the_facts_of_the_agent() {
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    let env = [];
    let calls = mcp_calls_env(
        repo,
        &[
            ("step", serde_json::json!({"intent": "use the method", "same": ["rewrite $a.total => $a.total()"], "change": ["facts::Cart=the cart changes"]})),
            ("step", serde_json::json!({"intent": "use the method", "same": ["rewrite $a.total => $a.total()"]})),
            ("fact", serde_json::json!({"change": ["shop_core::facts::idle=idle scales the value"]})),
            ("fact", serde_json::json!({"step": 0, "change": ["shop_core::facts::Cart=the cart changes"]})),
            ("results", serde_json::json!({})),
        ],
        &env,
    );
    assert!(calls[0].1 && calls[0].0.contains("rejected: step 0") && calls[0].0.contains("fact a2 of the agent is false"), "{}", calls[0].0);
    assert!(!calls[1].1 && calls[1].0.contains("accepted: step 0") && calls[1].0.contains("✓ a1 same: rewrite `$a.total` => `$a.total()` (the form holds: 3 site(s) have exactly this edit. Semilla does not check that the behavior is the same)"), "{}", calls[1].0);
    assert!(!calls[2].1 && calls[2].0.contains("stored: 1 new fact(s) of the agent in step 0") && calls[2].0.contains("✓ a2 change: "), "{}", calls[2].0);
    assert!(calls[3].1 && calls[3].0.contains("rejected: no fact is added to step 0"), "{}", calls[3].0);
    assert!(calls[4].0.contains("✓ a2 change: `shop_core::facts::idle`") && calls[4].0.contains(&format!("not stated: 2 item(s) with 2 place(s) {GAP_HEAD}")), "{}", calls[4].0);

    // A step of a replay plan has the same two keys.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "use the method"]);
    run(repo, &["tag"], 0);
    run(repo, &["replay", "begin", "--commit", "HEAD"], 0);
    let plan = |same: &str| {
        let edits: Vec<serde_json::Value> = facts_edits().iter().map(|(find, replace)| serde_json::json!({"file": "crates/shop-core/src/facts.rs", "find": find, "replace": replace})).collect();
        let plan = serde_json::json!({"steps": [{"intent": "use the method", "same": [same], "change": ["shop_core::facts::idle=idle scales the value"], "edits": edits}]});
        std::fs::write(repo.join(".semilla/plan.json"), serde_json::to_string_pretty(&plan).unwrap()).unwrap();
    };
    plan("rewrite $a.count => $a.count()");
    // The check of the change set checks each fact: a false fact stops the plan.
    let (code, out) = tree_mode(repo, &["replay", "apply"]);
    assert!(out.contains("  ✗ a1 same: rewrite `$a.count` => `$a.count()` (false: ") && out.contains("result: rejected") && !out.contains("the plan is accepted"), "{code:?} {out}");
    // Design 54.13: the mark of the plan line agrees with the result of the step, with and
    // without `--full`. The exit code is 1.
    let false_fact = "✗ plan step 0: \"use the method\"\n    ✗ fact a1 of the agent is false: `--same 'rewrite $a.count => $a.count()'`\n";
    assert!(code == Some(1) && out.starts_with(false_fact) && !out.contains("✓ plan step") && !out.contains("phase 1 passed"), "{code:?} {out}");
    let (code, out) = tree_mode(repo, &["replay", "apply", "--full"]);
    assert!(code == Some(1) && out.starts_with(false_fact) && out.contains("result: rejected") && !out.contains("✓ plan step"), "{code:?} {out}");
    // A refuted claim of a plan step has the same mark and the same exit code.
    let with_claim = |claims: serde_json::Value| {
        let mut plan: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".semilla/plan.json")).unwrap()).unwrap();
        plan["steps"][0]["same"] = serde_json::json!(["rewrite $a.total => $a.total()"]);
        plan["steps"][0]["claims"] = claims;
        std::fs::write(repo.join(".semilla/plan.json"), serde_json::to_string_pretty(&plan).unwrap()).unwrap();
    };
    with_claim(serde_json::json!([{"op": "no-panic", "at": "shop_core::facts::idle"}]));
    let output = semilla(repo, &["replay", "apply", "--full"]);
    let out = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.code() == Some(1) && out.starts_with("✗ plan step 0: \"use the method\"\n    ✗ claim c1 is refuted: ") && out.contains("  ✓ a1 same: ") && out.contains("\n  ✗ c1 refuted: ") && out.contains("result: rejected"), "{out}");
    // Negative: a plan with true facts and no claim has the mark `✓` and the exit code 0.
    plan("rewrite $a.total => $a.total()");
    let (code, out) = tree_mode(repo, &["replay", "apply"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("✓ plan step 0: \"use the method\"\n") && !out.contains("✗ plan step") && !out.contains("is false"), "{out}");
    assert_eq!(session_steps(repo)[0]["agent"], serde_json::json!({"same": ["rewrite $a.total => $a.total()"], "change": ["shop_core::facts::idle=idle scales the value"]}));
}

/// Design 55.3 (batch 9b4, T2 and T3): the option `--tag` of `semilla step` and of
/// `semilla fact`. A tag groups the facts in the outputs. It changes no fact and no gap.
// rule+ R55-1
// rule+ R55.2-10 R55.3-9 R55.3-10 R55.3-11 R55.3-12 R55.3-13 R55.3-14 R55.3-15
// rule- R55.2-10 R55.3-9 R55.3-10 R55.3-12 R55.3-13 R55.3-14 R55.3-15
#[test]
fn a_tag_groups_the_facts_of_a_step_and_changes_no_fact() {
    let file = "crates/shop-core/src/facts.rs";
    let fact_lines = |text: &str| -> Vec<String> { text.lines().filter(|line| line.starts_with("  ◆ ") || line.starts_with("  ? ") || line.starts_with("  not stated: ")).map(str::to_string).collect() };
    // The same step with no tag.
    let plain_dir = facts_setup();
    let (code, plain) = tree_mode(&plain_dir.path().join("repo"), &["step", "use the method"]);
    assert_eq!(code, Some(0), "{plain}");
    assert!(!plain.contains("tag") && plain.contains("\n  cover: each of the 18 changed line(s) is in a fact\n"), "{plain}");
    let plain_steps = session_steps(&plain_dir.path().join("repo"));

    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    // The form of a tag: exit code 2, and no step is open after it.
    for (tag, cause) in [
        (format!("{file}:11"), "is not a tag: write PLACE[,PLACE]=NAME".to_string()),
        (format!("{file}:9999=x"), format!("the position `{file}:9999` of the tag `x` is after the last line: `{file}` has 42 line(s) in the new tree")),
        (format!("{file}:-9999=x"), format!("`{file}` has 36 line(s) in the old tree")),
    ] {
        let (code, out) = tree_mode(repo, &["step", "use the method", "--tag", &tag]);
        assert!(code == Some(2) && out.contains(&cause), "{tag}: {code:?} {out}");
    }
    // A position with no edit: the step is not stored (exit code 1). The output has the
    // nearest positions with an edit, and the command to run again has no tag.
    let (code, out) = tree_mode(repo, &["step", "use the method", "--tag", &format!("{file}:1-3=x")]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.starts_with("rejected: step 0 \"use the method\" is not stored\n") && out.contains(&format!("  ✗ tag `x` has a position with no edit: `--tag '{file}:1-3=x'`\n")), "{out}");
    assert!(out.contains(&format!("  ✗ tag: the tag `x` has a position with no edit: the step has no edit in `{file}:1-3`; the nearest positions with an edit: {file}:11\n")), "{out}");
    assert!(out.ends_with("run again: semilla step 'use the method'\n"), "{out}");
    // A file that the step does not change.
    let (code, out) = tree_mode(repo, &["step", "use the method", "--tag", "crates/shop-core/src/lib.rs=x"]);
    assert!(code == Some(1) && out.contains(&format!("the step does not change the file `crates/shop-core/src/lib.rs`; the changed files: {file}\n")), "{code:?} {out}");
    let (_, status) = tree_mode(repo, &["status"]);
    assert!(status.contains("no open change set"), "{status}");

    // Two tags: lines of the new tree, and a line of the old tree for the old place of a
    // moved call (the hunk header `@@ -23 +29 @@`).
    let (code, out) = tree_mode(repo, &["step", "use the method", "--tag", &format!("{file}:11-17=method calls"), "--tag", &format!("{file}:20-25,{file}:-23=idle and tail")]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(&format!("  tag `method calls` ({file}:11-17): 2 fact(s): 2 semantics; 6 changed line(s)\n  ◆ f1 derived: 2 site(s): changed expression `c.total` → `c.total()` [semantics · code different]\n")), "{out}");
    assert!(out.contains(&format!("  tag `idle and tail` ({file}:20-25, {file}:-23): 3 fact(s): 3 semantics; 8 changed line(s)\n  ◆ f3 derived: added `let` [1]")), "{out}");
    assert!(out.contains("  not in a tag: 2 fact(s): 2 semantics; 4 changed line(s)\n  ◆ f7 derived: "), "{out}");
    // Negative test: no fact is in the two tags, thus no tag names a fact of a tag above.
    assert!(!out.contains("also in this tag"), "{out}");
    // Design 55.2: the default output has positions only in the gap list. No fact line has a
    // line `lines:` below it.
    assert!(out.contains("\n      lines: "), "{out}");
    assert!(!out.lines().zip(out.lines().skip(1)).any(|(fact, next)| fact.starts_with("  ◆ ") && next.trim_start().starts_with("lines: ")), "{out}");
    assert!(out.contains("      tag: `method calls`\n      to state it: the check did not decide if the behavior changes: if it changes, --change 'shop_core::facts::a,shop_core::facts::b,shop_core::facts::d=TEXT'; if it does not change, --same 'rewrite $a.total => $a.total()'\n") && out.contains("      not in a tag\n      to state it: the check did not decide if the behavior changes: if it changes, --change 'shop_core::facts::inner::a=TEXT'; if it does not change, --same 'rewrite 1 => 5'\n"), "{out}");
    assert!(out.contains("\n  cover: each of the 18 changed line(s) is in a fact\n"), "{out}");
    // A tag does not replace a fact: each fact line and each gap line of the step with no
    // tag is in the output one time, and the stored facts are equal.
    for line in fact_lines(&plain) {
        assert_eq!(out.lines().filter(|other| *other == line).count(), 1, "{line}\n{out}");
    }
    assert_eq!(fact_lines(&plain).len(), fact_lines(&out).len(), "{out}");
    let steps = session_steps(repo);
    assert_eq!(steps[0]["tags"], serde_json::json!([format!("{file}:11-17=method calls"), format!("{file}:20-25,{file}:-23=idle and tail")]));
    assert!(plain_steps[0].get("tags").is_none(), "{}", plain_steps[0]);
    assert_eq!(steps[0]["agent"], plain_steps[0]["agent"]);

    // `semilla fact --tag` adds a tag to the stored step. A fact of two tags shows in the
    // first tag; the second tag names it.
    let (code, out) = tree_mode(repo, &["fact", "--tag", &format!("{file}:+29,1=tail"), "--change", "shop_core::facts::idle=idle scales the value"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("stored: 1 new fact(s) of the agent and 1 new tag(s) in step 0 \"use the method\"; no build check runs again"), "{out}");
    assert!(out.contains("(holds: the behavior of existing code can change in each item; covers 2 fact(s) in 1 item(s) with 6 changed line(s))\n"), "{out}");
    assert!(out.contains(&format!("  tag `tail` ({file}:29): 1 fact(s): 1 semantics; 2 changed line(s)\n      also in this tag, shown in a tag above: f6\n")), "{out}");
    assert!(out.contains("      tags: `idle and tail`, `tail`\n"), "{out}");
    // A tag with no edit adds nothing (exit code 1); a line after the last line: exit code 2.
    let stored = session_steps(repo)[0].clone();
    let (code, out) = tree_mode(repo, &["fact", "--tag", &format!("{file}:1=none")]);
    assert!(code == Some(1) && out.starts_with("rejected: no fact and no tag is added to step 0") && out.contains("  ✗ tag: the tag `none` has a position with no edit: "), "{code:?} {out}");
    let (code, out) = tree_mode(repo, &["fact", "--tag", &format!("{file}:999=none")]);
    assert!(code == Some(2) && out.contains("is after the last line"), "{code:?} {out}");
    assert_eq!(session_steps(repo)[0], stored);

    // `status --verbose` has the groups of the tags and the lines of each fact. The short
    // `status` has no line of a tag.
    let (_, verbose) = tree_mode(repo, &["status", "--verbose"]);
    assert!(verbose.contains("      facts, in the groups of the tags below: f3, f4\n"), "{verbose}");
    // The lines of the compiled check come between the fact line and its line `lines:`.
    let f2 = verbose.find("  ◆ f2 derived: changed expression `e.total` → `e.total()` at `shop_core::facts::d/tail/rhs` [semantics · code different]\n").expect("the line of f2");
    let next = verbose[f2 + 1..].find("  ◆ ").map_or(verbose.len(), |at| f2 + 1 + at);
    assert!(verbose[f2..next].contains(&format!("\n      lines: {file}:17, {file}:-17\n")), "{verbose}");
    assert!(verbose.contains(&format!("  tag `tail` ({file}:29): 1 fact(s): 1 semantics; 2 changed line(s)\n")), "{verbose}");
    let (_, status) = tree_mode(repo, &["status"]);
    assert!(!status.contains("tag `") && !status.contains("changed lines with no fact"), "{status}");
    // The Markdown review has the groups of the tags, and the tag of each gap.
    let md = repo.join(".semilla/review.md");
    tree_mode(repo, &["review", "--md", md.to_str().unwrap()]);
    let report = std::fs::read_to_string(&md).unwrap();
    assert!(report.contains("#### Tags of the agent (3)\n") && report.contains(&format!("Step 0, tag `method calls` ({file}:11-17): 2 fact(s): 2 semantics; 6 changed line(s):\n\n- step 0: f1 derived: ")), "{report}");
    assert!(report.contains("- also in this tag, shown in a tag above: f6\n") && report.contains("Step 0, not in a tag: 2 fact(s): 2 semantics; 4 changed line(s):\n"), "{report}");
    // The gap line has the lines of its edits (a short list), then the tag.
    assert!(report.contains(&format!("in: `shop_core::facts::inner::a`; lines: {file}:33, {file}:-27; not in a tag\n")) && report.contains(&format!("{file}:-17; tag: `method calls`\n")), "{report}");
    assert!(!report.contains("#### Derived facts") && report.contains("  - facts, in the groups of the tags below: f3, f4\n"), "{report}");
    for id in ["f1", "f2", "f3", "f4", "f5", "f6", "f7"] {
        assert_eq!(report.matches(&format!("- step 0: {id} ")).count(), 1, "{id}\n{report}");
    }
    // Negative test: the review of the step with no tag has no tag section.
    let plain_repo = plain_dir.path().join("repo");
    let md = plain_repo.join(".semilla/review.md");
    tree_mode(&plain_repo, &["review", "--md", md.to_str().unwrap()]);
    let report = std::fs::read_to_string(&md).unwrap();
    assert!(!report.contains("Tags of the agent") && !report.contains("not in a tag") && report.contains("#### Derived facts (7)"), "{report}");
}

/// Design 55.3: the MCP tools `step` and `fact`, and a step of a replay plan, take tags.
// rule+ R55.3-14 R55.3-18
#[test]
fn the_mcp_tools_and_a_plan_step_take_tags() {
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    let file = "crates/shop-core/src/facts.rs";
    let env = [];
    let calls = mcp_calls_env(
        repo,
        &[
            ("step", serde_json::json!({"intent": "use the method", "tags": [format!("{file}:1-3=none")]})),
            ("step", serde_json::json!({"intent": "use the method", "tags": [format!("{file}:11-17=method calls")]})),
            ("fact", serde_json::json!({"tags": [format!("{file}:20-25=idle")]})),
            ("fact", serde_json::json!({"tags": [format!("{file}:2=none")]})),
            ("results", serde_json::json!({})),
        ],
        &env,
    );
    assert!(calls[0].1 && calls[0].0.contains("rejected: step 0") && calls[0].0.contains("has a position with no edit"), "{}", calls[0].0);
    assert!(!calls[1].1 && calls[1].0.contains("accepted: step 0") && calls[1].0.contains(&format!("  tag `method calls` ({file}:11-17): 2 fact(s): 2 semantics; 6 changed line(s)\n")), "{}", calls[1].0);
    assert!(!calls[2].1 && calls[2].0.contains("stored: 0 new fact(s) of the agent and 1 new tag(s) in step 0") && calls[2].0.contains(&format!("  tag `idle` ({file}:20-25): 2 fact(s): 2 semantics; 6 changed line(s)\n")), "{}", calls[2].0);
    assert!(calls[3].1 && calls[3].0.contains("rejected: no fact and no tag is added to step 0"), "{}", calls[3].0);
    // A tag changes no result: the short result has the gap count of the step with no tag.
    assert!(calls[4].0.contains(&format!("  not stated: 6 item(s) with 7 place(s) {GAP_HEAD}\n")) && calls[4].0.contains("result: accepted"), "{}", calls[4].0);
    assert_eq!(session_steps(repo)[0]["tags"], serde_json::json!([format!("{file}:11-17=method calls"), format!("{file}:20-25=idle")]));

    // A step of a replay plan has the key `tag`.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "use the method"]);
    run(repo, &["tag"], 0);
    run(repo, &["replay", "begin", "--commit", "HEAD"], 0);
    let plan = |tag: &str| {
        let edits: Vec<serde_json::Value> = facts_edits().iter().map(|(find, replace)| serde_json::json!({"file": file, "find": find, "replace": replace})).collect();
        let plan = serde_json::json!({"steps": [{"intent": "use the method", "tag": [tag], "edits": edits}]});
        std::fs::write(repo.join(".semilla/plan.json"), serde_json::to_string_pretty(&plan).unwrap()).unwrap();
    };
    // A tag with a position with no edit stops the plan.
    plan(&format!("{file}:1-3=none"));
    let (code, out) = tree_mode(repo, &["replay", "apply"]);
    assert!(code == Some(1) && out.contains("has a position with no edit") && !out.contains("✓ plan step"), "{code:?} {out}");
    // A tag with a wrong form is a problem of the plan.
    plan(&format!("{file}:11-17"));
    let (code, out) = tree_mode(repo, &["replay", "apply"]);
    assert!(code != Some(0) && out.contains("is not a tag: write PLACE[,PLACE]=NAME"), "{code:?} {out}");
    plan(&format!("{file}:11-17=method calls"));
    let (code, out) = tree_mode(repo, &["replay", "apply"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("✓ plan step 0: \"use the method\"\n"), "{out}");
    assert_eq!(session_steps(repo)[0]["tags"], serde_json::json!([format!("{file}:11-17=method calls")]));
    let (_, verbose) = tree_mode(repo, &["status", "--verbose"]);
    assert!(verbose.contains(&format!("  tag `method calls` ({file}:11-17): 2 fact(s): 2 semantics; 6 changed line(s)\n")), "{verbose}");
}

/// Design 55.1 and 55.3 (batch 9b4b, P1 and P2.1): the step output names the line diff command
/// with the two trees of the step. The hunks of the command are the hunks of Semilla: one tag
/// with each hunk header has each changed line and each fact, and no fact is "not in a tag".
// rule+ R55.8-1
// rule+ R55.8-3
// rule- R55.8-3
#[test]
fn the_step_output_names_the_command_that_gives_the_hunks_of_a_tag() {
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    let (code, out) = tree_mode(repo, &["step", "use the method"]);
    assert_eq!(code, Some(0), "{out}");
    let line = out.lines().find(|line| line.starts_with("  hunks: `git diff -U0 ")).unwrap_or_else(|| panic!("{out}"));
    assert!(line.starts_with("  hunks: `git diff -U0 --inter-hunk-context=0 --diff-algorithm=myers --indent-heuristic --no-renames --no-ext-diff --no-textconv --no-relative --text --no-color "), "{line}");
    let command: Vec<&str> = line.trim_start_matches("  hunks: `").trim_end_matches('`').split(' ').collect();
    assert_eq!(command.len(), 14, "{command:?}");
    // The agent runs the command, and makes one place from each hunk header.
    let diff = git(repo, &command[1..]);
    let mut places = Vec::new();
    let mut lines = 0;
    let mut file = String::new();
    for line in diff.lines() {
        if let Some(name) = line.strip_prefix("+++ b/") {
            file = name.to_string();
        } else if let Some(header) = line.strip_prefix("@@ -") {
            let (old, rest) = header.split_once(" +").unwrap();
            let new = rest.split_once(" @@").unwrap().0;
            let count = |side: &str| side.split_once(',').map_or(1, |(_, count)| count.parse::<usize>().unwrap());
            lines += count(old) + count(new);
            let side = |side: &str| if side.contains(',') { side.to_string() } else { format!("{side},1") };
            places.push(if count(new) == 0 { format!("{file}:-{}", side(old)) } else { format!("{file}:+{}", side(new)) });
        }
    }
    assert!(places.len() >= 5 && lines == 18, "{places:?} {lines}\n{diff}");
    assert!(!out.contains("git gave no line diff"), "{out}");
    assert!(out.contains("\n  cover: each of the 18 changed line(s) is in a fact\n"), "{out}");
    let (code, out) = tree_mode(repo, &["fact", "--tag", &format!("{}=each hunk", places.join(","))]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("): 7 fact(s): 7 semantics; 18 changed line(s)\n"), "{out}");
    assert!(!out.contains("not in a tag") && !out.contains("in part") && !out.contains("facts with no position"), "{out}");
    // The help of the option names the command.
    let (_, help) = tree_mode(repo, &["step", "--help"]);
    assert!(help.contains("git diff -U0 --inter-hunk-context=0 --diff-algorithm=myers --indent-heuristic --no-renames --no-ext-diff --no-textconv --no-relative --text --no-color"), "{help}");

    // Negative test: with no line diff of git, the output does not name the git command. It
    // names the command that prints the hunks of Semilla, and that command prints them.
    let dir = facts_setup();
    let repo = &dir.path().join("repo");
    let own = |args: &[&str]| {
        let output = semilla_env(repo, args, &[("SEMILLA_OWN_LINE_DIFF", "1")]);
        format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
    };
    let out = own(&["step", "use the method"]);
    assert!(!out.contains("git diff -U0") && out.contains("\n  hunks: git gave no line diff of this step; the "), "{out}");
    assert!(out.contains(" hunk(s) are the hunks of the line diff of Semilla (`semilla expand --step "), "{out}");
    assert!(out.contains("\n  cover: each of the 18 changed line(s) is in a fact\n"), "{out}");
    let verbose = own(&["status", "--verbose"]);
    assert!(verbose.contains(" hunk(s) are the hunks of the line diff of Semilla:\n    crates/shop-core/src/facts.rs @@ -"), "{verbose}");
}

// ---- Batch 9t2 (design 57.6): the tests of the rules that had no test ----

/// Design 53.3: the build check runs also for a step that the syntax phase rejects, so one run
/// shows each cause. Negative test: with no check, the output has no result of a check.
// rule+ R53.3-1 R53.3-8
// rule- R53.3-1
#[test]
fn a_step_that_the_syntax_phase_rejects_has_the_result_of_the_build_check() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let args = ["step", "raise the limit", "--same", "rename shop_core::cart::gold shop_core::cart::silver"];
    // The tree builds, and the fact of the agent is false.
    let (code, step) = semilla_checked(repo, &args, &[]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("rejected: step 0") && step.contains("fact a1 of the agent is false"), "{step}");
    assert!(step.contains("\nbuild check: passes ("), "{step}");
    assert!(session_steps(repo).is_empty());
    // With no `--fast`, the compiled check of the step does not accept it, so no build check
    // runs (R53.5-2): the output has no result line of a check that passes.
    let (code, step) = run_env(repo, &args, &[]);
    assert_eq!(code, 1, "{step}");
    assert!(step.starts_with("rejected: step 0") && !step.contains("build check: passes"), "{step}");
    // A check that cannot run: the first line says "rejected", and the line of the check shows.
    // The rustup home has the tool chain of the driver (the compiled build), and no `stable`.
    let home = rustup_home_with_proj(dir.path(), "proj-1.0");
    let (code, step) = semilla_checked(repo, &args, &[("RUSTUP_HOME", home.to_str().unwrap()), ("RUSTUP_TOOLCHAIN", "stable")]);
    assert_eq!(code, Some(1), "{step}");
    assert!(step.starts_with("rejected: step 0") && step.contains("\n  ! build check: cannot run: the toolchain `stable`"), "{step}");
    assert!(session_steps(repo).is_empty());
}

/// Design 53.5: with `--fast`, the compiled check runs later, so the build check runs cargo
/// itself. The negative test is `the_compiled_check_is_the_build_check_and_no_second_build_runs`.
// rule+ R53.5-3
// rule- R53.5-1
#[test]
fn a_fast_step_of_the_compiled_check_runs_the_build_check_itself() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let output = semilla_env(repo, &["step", "raise the limit", "--fast"], &[("SEMILLA_NO_WORKER", "1")]);
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{text}");
    let line = text.lines().find(|line| line.starts_with("build check: ")).expect("the result line of the build check");
    assert!(line.starts_with("build check: passes (") && line.ends_with(" s)") && !text.contains("the build of the compiled check"), "{text}");
    assert!(check_folder(repo).join("target").is_dir());
    assert_eq!(session_steps(repo)[0]["build_check"], serde_json::json!({"result": "passes"}));
}

/// Design 51.4, decision 5: the variable `SEMILLA_SITE_COUNTS` adds the line with the three
/// counts to each rename fact. Negative test: with no variable, no fact has the line.
// rule+ R51.4-5
// rule- R51.4-5
#[test]
fn the_line_of_the_site_counts_shows_only_with_its_variable() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("pub fn total(", "pub fn sum("));
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("cart.total(", "cart.sum("));
    let step = |env: &[(&str, &str)]| {
        let output = semilla_env(repo, &["step", "rename `total` to `sum`"], &[&[], env].concat());
        assert!(output.status.success());
        format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
    };
    let plain = step(&[]);
    assert!(plain.contains("renamed `shop_core::cart::Cart::total` → `shop_core::cart::Cart::sum`") && !plain.contains("sites: "), "{plain}");
    run(repo, &["undo"], 0);
    let counts = step(&[("SEMILLA_SITE_COUNTS", "1")]);
    let line = counts.lines().find(|line| line.contains("sites: ")).unwrap_or_else(|| panic!("no line of the counts:\n{counts}"));
    assert!(line.contains(" sure, ") && line.contains(" by name, ") && line.trim_end().ends_with(" with their own fact"), "{counts}");
}

/// Design 48.5, negative test: an MCP step and a plan step with no key `same` or `change` have
/// no fact of the agent, in the output and in the record.
// rule- R48.5-2
// rule- R55.3-18
// rule- R54.5-3
#[test]
fn an_mcp_step_and_a_plan_step_with_no_key_have_no_fact_of_the_agent() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let calls = mcp_calls_env(repo, &[("step", serde_json::json!({"intent": "raise the limit"}))], &[]);
    assert!(!calls[0].1 && calls[0].0.contains("accepted: step 0"), "{}", calls[0].0);
    assert!(!calls[0].0.contains("facts of the agent") && !calls[0].0.contains(" a1 "), "{}", calls[0].0);
    assert!(session_steps(repo)[0].get("agent").is_none(), "{:?}", session_steps(repo));
    // Design 55.3: with no argument `tags`, the step has no tag.
    assert!(!calls[0].0.contains("  tag `") && !calls[0].0.contains("not in a tag") && session_steps(repo)[0].get("tags").is_none(), "{}", calls[0].0);
    // The same edit as a step of a plan.
    run(repo, &["undo"], 0);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "raise the limit"]);
    run(repo, &["replay", "begin", "--commit", "HEAD"], 0);
    let plan = serde_json::json!({"steps": [{"intent": "raise the limit", "edits": [{"file": "crates/shop-app/src/main.rs", "find": "cart.total(10)", "replace": "cart.total(20)"}]}]});
    std::fs::write(repo.join(".semilla/plan.json"), serde_json::to_string_pretty(&plan).unwrap()).unwrap();
    let (code, out) = tree_mode(repo, &["replay", "apply"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("✓ plan step 0") && !out.contains("fact a1") && !out.contains("facts of the agent"), "{out}");
    assert!(session_steps(repo)[0].get("agent").is_none(), "{:?}", session_steps(repo));
    assert!(session_steps(repo)[0].get("tags").is_none(), "{:?}", session_steps(repo));
}

/// Design 61.16 (R61.16-1): a bench and a test with one crate name (`probe`), each with its own
/// `fn hash`. The bench needs the feature `fast`, so the configurations with no feature compile
/// only the test. A step that changes only the library opens no body of the joint check of the
/// build configuration: the check runs the body compare two times (the plain run and the strict
/// run), with no run for each atom. Before, the records of the test in the configurations with
/// no bench went to the body of the bench, the two runs gave different graphs, and the check
/// ran once more for each atom.
// rule+ R61.16-1
#[test]
fn a_body_name_of_two_targets_opens_no_body_of_the_joint_check() {
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");
    let dir = common::TestDir::new();
    let repo = &dir.path().join("repo");
    write(repo, ".gitignore", "/target\n/.semilla/\n");
    write(repo, "Cargo.toml", "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nfast = []\nsmall = []\n\n[[bench]]\nname = \"probe\"\nrequired-features = [\"fast\"]\n");
    let lib = "#[cfg(feature = \"fast\")]\npub fn speed() -> u32 {\n    2\n}\n\n#[cfg(not(feature = \"fast\"))]\npub fn speed() -> u32 {\n    1\n}\n\n#[cfg(feature = \"small\")]\npub fn size() -> u32 {\n    1\n}\n\npub fn price(x: u32) -> u32 {\n    x + 1\n}\n";
    write(repo, "src/lib.rs", lib);
    write(repo, "benches/probe.rs", "pub fn hash(x: u64) -> u64 {\n    x.wrapping_mul(31) ^ 7\n}\n\n#[test]\nfn bench_hash() {\n    assert!(hash(1) > 0);\n}\n");
    write(repo, "tests/probe.rs", "pub fn hash(x: u64) -> u64 {\n    if x > 10 {\n        x + 1\n    } else {\n        x * 3\n    }\n}\n\n#[test]\nfn test_hash() {\n    assert!(hash(2) > 0);\n}\n");
    git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "base");
    write(repo, "src/lib.rs", &lib.replace("x + 1", "x + 2"));
    let output = semilla_env(repo, &["step", "probe"], &[("SEMILLA_TIMING", "1")]);
    let (out, err) = (String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.status.code(), Some(0), "{out}\n{err}");
    assert!(out.contains("    shop::price (src/lib.rs:16): ") && !out.contains("probe::hash"), "{out}");
    // The builds of the first step: one for each configuration and tree.
    let configs = err.lines().filter(|line| line.starts_with("[timing] cargo check n0")).count() / 2;
    assert!(configs >= 2, "{err}");
    let runs = err.lines().find_map(|line| line.strip_prefix("[timing] total of give the records to the build configuration rules (")).and_then(|rest| rest.split(' ').next()).and_then(|count| count.parse::<usize>().ok());
    assert_eq!(runs, Some(2 * configs), "two runs of the body compare (plain and strict) in each of {configs} configurations\n{err}");
}

/// The crate of the expansion test (design 63): a nested macro, a derive, and a macro that the
/// step does not change.
const EXPAND_BASE: &str = "pub mod cart;

macro_rules! base { ($v:expr) => { $v + 10 } }
macro_rules! fee { ($v:expr) => { base!($v) * 2 } }

pub fn price(v: u32) -> u32 { fee!(v) }

#[derive(PartialEq)]
pub struct Point { pub x: i32 }
";

// rule+ R63.4-1 R63.4-2 R63.3-7 R63.3-9
// rule- R63.4-1 R63.4-2
#[test]
fn a_changed_expansion_has_its_facts_a_command_to_expand_it_and_a_section_of_the_review() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", EXPAND_BASE);
    commit_all(repo, "expand");
    // R63.4-2, the negative case: a step with no changed expansion has no section.
    write(repo, "crates/shop-core/src/lib.rs", &EXPAND_BASE.replace("pub fn price", "/// The price.\npub fn price"));
    run(repo, &["step", "docs"], 0);
    let md = dir.path().join("review-docs.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    assert!(!std::fs::read_to_string(&md).unwrap().contains("## Macro expansions"));
    // The command reads the working tree against `HEAD` before a step: it builds the records.
    write(repo, "crates/shop-core/src/lib.rs", &EXPAND_BASE.replace("/// The price.\n", "").replace("+ 10", "+ 12").replace("pub x: i32 }", "pub x: i32, pub y: i32 }"));
    let expanded = run(repo, &["expand", "base!"], 0);
    assert!(expanded.contains("`fee!(v)` at `shop_core::price/tail` (crates/shop-core/src/lib.rs:6): expansion changed\n  definition: `macro_rules! fee` (crates/shop-core/src/lib.rs:4); uses: `base!` (crates/shop-core/src/lib.rs:3)\n  --- before\n  +++ after\n  - (v + 10) * 2\n  + (v + 12) * 2\n"), "{expanded}");
    run(repo, &["step", "probe"], 0);
    // The step owns the two changes: the line of the definition, and the line of the field.
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(out.contains("expansion: `fee!(v)` at `shop_core::price/tail` (crates/shop-core/src/lib.rs:6): expansion changed: changed literal `10` → `12` at `expansion/lhs/rhs` [semantics]"), "{out}");
    assert!(out.contains("expansion: derived `PartialEq` of `shop_core::Point`: expansion changed: changed expression `self.x == other.x` → `self.x == other.x && self.y == other.y`"), "{out}");
    // R63.3-7: `status --verbose` lists each call with its result, its facts, and its cause.
    // R65.2-2: the equal expansions of the crate (also of the standard library) are a count.
    assert!(out.contains("  expansions: 2 changed, 2 equal\n"), "{out}");
    assert!(out.contains("      cause: from `macro_rules! base` (crates/shop-core/src/lib.rs:3)\n"), "{out}");
    // R63.3-9: the line of the body that holds the call, with its graph and its equivalence.
    assert!(out.contains("      body: shop_core::price (crates/shop-core/src/lib.rs:6): same structure: 2 changed checks"), "{out}");
    // R63.4-1: a stored step, one side, a place by line, and a place by item.
    let after = run(repo, &["expand", "crates/shop-core/src/lib.rs:6", "--step", "1", "--after"], 0);
    assert!(after.contains("  (v + 12) * 2\n") && !after.contains("(v + 10)"), "{after}");
    let before = run(repo, &["expand", "shop_core::Point", "--step", "1", "--before"], 0);
    assert!(before.contains("derive `PartialEq` at `shop_core::Point`") && before.contains("self.x == other.x }\n") && !before.contains("other.y"), "{before}");
    // R63.4-1, the negative case: a place with no macro call is an error.
    let output = semilla(repo, &["expand", "nothing!", "--step", "1"]);
    assert!(!output.status.success() && String::from_utf8_lossy(&output.stderr).contains("no macro call with an expansion matches `nothing!`"));
    // R63.4-2: the review has the section with the call, a link, the definition, and the diff.
    let md = dir.path().join("review.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("## Macro expansions\n\n### `fee!(v)` at `shop_core::price/tail` ([crates/shop-core/src/lib.rs:6](crates/shop-core/src/lib.rs#L6)) (step 1)\n"), "{text}");
    assert!(text.contains("```diff\n- (v + 10) * 2\n+ (v + 12) * 2\n```"), "{text}");
    assert!(text.contains("; expansion: `fee!(v)` at"), "{text}");
}

/// R71.1-1: a text over the budget names the command that prints the full text. `semilla expand
/// fN` prints each text of the fact in full: for the step that `semilla step` printed and did not
/// store (the working tree against the last stored step), and with `--step N` for a stored step.
/// The negative case: a fact number that the step does not have is an error with the list of
/// the facts.
// rule+ R71.1-1
// rule- R71.1-1
#[test]
fn a_text_over_the_budget_names_a_command_that_prints_the_full_text() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let table = |changed: &str| format!("pub mod cart;\nmacro_rules! table {{\n    ($($x:expr),* $(,)?) => {{\n        pub const TABLE: &[u32] = &[$($x),*];\n    }};\n}}\ntable!(\n{}\n);\n", (0..800).map(|n| if n == 400 { changed.to_string() } else { (n * 3).to_string() }).collect::<Vec<_>>().join(", "));
    write(repo, "crates/shop-core/src/lib.rs", &table("1200"));
    commit_all(repo, "table");
    write(repo, "crates/shop-core/src/lib.rs", &table("77777"));
    let out = run(repo, &["step", "a new value"], 0);
    assert!(out.contains("1194, 1197, 1200, 1203, 1206 …` → `table!( … 1194, 1197, 77777, 1203, 1206 …` (a large text: "), "{out}");
    assert!(out.contains("; 1 changed part; the full text: semilla expand f1 --step 0)"), "{out}");
    assert!(!out.contains("2397"), "{out}");
    // The stored step: with `--step`, and with no step when the working tree is the end of
    // the last step.
    for args in [&["expand", "f1", "--step", "0"][..], &["expand", "f1"][..]] {
        let full = run(repo, args, 0);
        assert!(full.contains("2394, 2397") && full.contains("77777") && !full.contains("a large text"), "{full}");
    }
    // A step that is not stored: the working tree against the end of the last step.
    write(repo, "crates/shop-core/src/lib.rs", &table("88888"));
    let next = run(repo, &["expand", "f1"], 0);
    assert!(next.contains("77777") && next.contains("88888") && next.contains("2394, 2397"), "{next}");
    let (code, missing) = run_env(repo, &["expand", "f9", "--step", "0"], &[]);
    assert_ne!(code, 0, "{missing}");
    assert!(missing.contains("step 0 has no fact f9; its facts: f1"), "{missing}");
}

/// The full-text commands of an output (R76.1-1): each `semilla expand …`, to the end of the
/// command.
fn full_text_commands(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("semilla expand ") {
            let tail = &rest[at..];
            let end = tail.find(['`', ')', ';']).unwrap_or(tail.len());
            let command = tail[..end].trim().to_string();
            if !found.contains(&command) {
                found.push(command);
            }
            rest = &tail[end..];
        }
    }
    found
}

/// The source of a crate with 40 calls of one macro: a change of the macro changes each call.
fn forty_calls(plus: u32) -> String {
    format!("pub mod cart;\nmacro_rules! make {{\n    ($n:ident, $v:expr) => {{ pub fn $n() -> u32 {{ $v + {plus} }} }};\n}}\n{}", (0..40).map(|i| format!("make!(item_with_a_long_name_{i}, {i});\n")).collect::<String>())
}

/// R76.1-1: each line that cuts a text names one command that prints the full text, and the
/// command works in the state after the output: after a rejected step, after a stored step,
/// after `semilla tag`, after a tag after the commit (the command names the commit), and after
/// a step of `--fast`. Before, the lines of a rejected step named `semilla status --verbose`,
/// which says "no open change set" after a rejected step.
// rule+ R76.1-1
#[test]
fn each_full_text_command_works_in_each_state_after_its_output() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let lib = "crates/shop-core/src/lib.rs";
    write(repo, lib, &forty_calls(1));
    commit_all(repo, "calls");
    write(repo, lib, &forty_calls(2));
    // One mode (design 83): the compiled build writes the expansion records.
    let default = [];
    let works = |output: &str, state: &str, env: &[(&str, &str)], want: &str| {
        let commands = full_text_commands(output);
        assert!(!commands.is_empty(), "{state}: the output names no full-text command:\n{output}");
        for command in commands {
            let args: Vec<&str> = command.split_whitespace().skip(1).collect();
            let (code, text) = run_env(repo, &args, env);
            assert_eq!(code, 0, "{state}: `{command}`:\n{text}");
            assert!(!text.contains("no open change set") && !text.trim().is_empty(), "{state}: `{command}`:\n{text}");
            // The full text has each hidden line: the last call of the macro, or the far value.
            // Design 83: the expansion lines are in the step section, so the full text of the
            // step has the last call; the full text of a fact has the far value.
            if command.split_whitespace().nth(2).is_some_and(|place| place == "--step" || (place.starts_with('f') && !want.starts_with("make!"))) {
                assert!(text.contains(want), "{state}: `{command}` has no `{want}`:\n{text}");
            }
        }
    };
    let last_call = "make!(item_with_a_long_name_39, 39)";
    // A rejected step (a false fact of the agent).
    let (code, rejected) = run_env(repo, &["step", "plus two", "--same", "rename shop_core::item_with_a_long_name_0 => shop_core::zzz"], &default);
    assert_eq!(code, 1, "{rejected}");
    assert!(rejected.contains("      expansion: … 37 more (semilla expand --step 0)"), "{rejected}");
    works(&rejected, "after a rejected step", &default, last_call);
    let (_, status) = run_env(repo, &["status", "--verbose"], &default);
    assert!(status.contains("no open change set"), "{status}");
    // A stored step.
    let (code, stored) = run_env(repo, &["step", "plus two"], &default);
    assert_eq!(code, 0, "{stored}");
    works(&stored, "after a stored step", &default, last_call);
    // `semilla tag`: the commands of the stored step and of the tag.
    commit_all(repo, "plus two");
    let (code, tag) = run_env(repo, &["tag"], &default);
    assert_eq!(code, 0, "{tag}");
    works(&format!("{stored}\n{tag}"), "after `semilla tag`", &default, last_call);
    // A tag after the commit: each command names the commit.
    write(repo, lib, &forty_calls(3));
    commit_all(repo, "plus three");
    let (code, after) = run_env(repo, &["tag"], &default);
    assert_eq!(code, 0, "{after}");
    assert!(after.contains("(semilla expand --step 0 --commit "), "{after}");
    works(&after, "after a tag after the commit", &default, last_call);
    // A step of `--fast` (the compiled mode, phase 1): a fact text over the budget.
    let table = |changed: &str| format!("pub mod cart;\nmacro_rules! table {{\n    ($($x:expr),* $(,)?) => {{\n        pub const TABLE: &[u32] = &[$($x),*];\n    }};\n}}\ntable!(\n{}\n);\n", (0..800).map(|n| if n == 400 { changed.to_string() } else { (n * 3).to_string() }).collect::<Vec<_>>().join(", "));
    write(repo, lib, &table("1200"));
    commit_all(repo, "table");
    write(repo, lib, &table("77777"));
    let compiled = [];
    let (code, fast) = run_env(repo, &["step", "a new value", "--fast", "--reason", "shop_core::table!#0=a table of the test"], &compiled);
    assert_eq!(code, 0, "{fast}");
    assert!(fast.contains("accepted (phase 1;") && fast.contains("the full text: semilla expand f1 --step 0)") && !fast.contains("2394, 2397"), "{fast}");
    works(&fast, "after a step of `--fast`", &compiled, "2394, 2397");
    // A rejected step of the compiled mode: the full text of the step has the lines of the
    // compiled check that the step printed (the record of the printed text).
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("if *x < max", "if *x <= max"));
    let (code, rejected) = run_env(repo, &["step", "an edge", "--same", "rename shop_core::cart::Cart::total => shop_core::cart::Cart::sum"], &compiled);
    assert_eq!(code, 1, "{rejected}");
    assert!(rejected.contains("      equivalence: "), "{rejected}");
    let (code, full) = run_env(repo, &["expand", "--step", "1"], &compiled);
    assert_eq!(code, 0, "{full}");
    assert!(full.contains("      equivalence: ") && full.contains("✗ a1 same: rename") && !full.contains("has no record"), "{full}");
}

/// R76.1-1, the negative case: a full-text command names only a step that the state has. A
/// step number that the session does not have is an error, not the text of a different step.
/// After the next `semilla step`, the record of a rejected step is gone, and the number names
/// the step of the new state. An output with no cut names no command.
// rule- R76.1-1
#[test]
fn a_full_text_command_names_only_a_step_of_the_state() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    let lib = "crates/shop-core/src/lib.rs";
    write(repo, lib, "pub mod cart;\npub fn one() -> u32 {\n    1\n}\n");
    commit_all(repo, "one");
    write(repo, lib, "pub mod cart;\npub fn one() -> u32 {\n    2\n}\n");
    let env = [];
    let (code, out) = run_env(repo, &["step", "two"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(full_text_commands(&out).is_empty(), "a short output names no command:\n{out}");
    let (code, missing) = run_env(repo, &["expand", "--step", "3"], &env);
    assert_ne!(code, 0, "{missing}");
    assert!(missing.contains("the session has no step 3"), "{missing}");
    // The full text of the stored step.
    let (code, full) = run_env(repo, &["expand", "--step", "0"], &env);
    assert_eq!(code, 0, "{full}");
    assert!(full.starts_with("step 0  \"two\"\n") && full.contains("changed literal `1` → `2`"), "{full}");
    // A rejected step keeps its record until the next step.
    write(repo, lib, "pub mod cart;\npub fn one() -> u32 {\n    3\n}\n");
    let (code, rejected) = run_env(repo, &["step", "three", "--same", "rename shop_core::one => shop_core::zzz"], &env);
    assert_eq!(code, 1, "{rejected}");
    let (_, text) = run_env(repo, &["expand", "--step", "1"], &env);
    assert!(text.contains("✗ a1 same: rename shop_core::one => shop_core::zzz"), "{text}");
    let (code, stored) = run_env(repo, &["step", "three"], &env);
    assert_eq!(code, 0, "{stored}");
    let (_, text) = run_env(repo, &["expand", "--step", "1"], &env);
    assert!(text.starts_with("step 1  \"three\"\n") && !text.contains("✗ a1"), "{text}");
}
