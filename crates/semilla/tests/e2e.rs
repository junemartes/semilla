//! End-to-end test: an agent tags its work with the real `semilla` binary, and the verifier, the
//! storage, the MCP server, and the UI work on a real git repository with a two-crate workspace.
//!
//! The test builds the MIR driver first. It uses Z3 from `.semilla/tools/z3` of this workspace
//! when it exists. Each test repository has its own cache folder (`SEMILLA_CACHE_DIR`).

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
    command.envs(env.iter().copied());
    let z3 = root.join(".semilla/tools/z3");
    if z3.exists() {
        command.env("SEMILLA_Z3", z3);
    }
    command.output().unwrap()
}

/// Runs semilla and returns stdout. Panics with the output if the exit code is not `expect`.
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

fn setup() -> tempfile::TempDir {
    // The MIR driver is a separate binary of the workspace.
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");

    let dir = tempfile::tempdir().unwrap();
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

    // Step 0 (design 24.3): a neutral rename of a local variable, with one command. The declared
    // class runs phase 2 in the command, and the MIR proves it.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let anchor = run(repo, &["anchor", "crates/shop-core/src/cart.rs:7"], 0).trim().to_string();
    assert_eq!(anchor, "shop_core::cart::Cart::total/S0");
    let retired = semilla(repo, &["step", "rename s to sum", "--claim", "rename-local shop_core::cart::Cart::total/S0 to=sum"]);
    assert!(String::from_utf8_lossy(&retired.stderr).contains("is retired"), "{}", String::from_utf8_lossy(&retired.stderr));
    let stored = run(repo, &["step", "rename s to sum", "--class", "neutral"], 0);
    assert!(stored.starts_with("accepted") && stored.contains("renamed local `s` → `sum`"), "{stored}");
    // An empty diff is an error.
    let empty = semilla(repo, &["step", "nothing"]);
    assert!(String::from_utf8_lossy(&empty.stderr).contains("no change since the last step"));

    // Step 1: an early exit and a changed operator. No claim: both are derived facts.
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    let stored = run(repo, &["step", "return 0 for an empty cart, include the max"], 0);
    assert!(stored.contains("added early exit `if self.items.is_empty()") && stored.contains("changed operator"), "{stored}");
    let check = run(repo, &["check", "--full"], 0);
    assert!(check.contains("[control_flow]"), "{check}");
    if workspace_root().join(".semilla/tools/z3").exists() {
        assert!(check.contains("effect hint: widen"), "{check}");
    }

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
    let retired = ask(serde_json::json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "step", "arguments": {"intent": "x", "claims": ["change-literal shop_app::main"]}}}));
    assert_eq!(retired["result"]["isError"], true, "{retired}");
    assert!(retired["result"]["content"][0]["text"].as_str().unwrap().contains("is retired"), "{retired}");
    drop(stdin);
    child.wait().unwrap();
}

/// Calls tools of `semilla mcp` in one session. Gives the text and the error mark of each call.
fn mcp_calls(repo: &Path, calls: &[(&str, serde_json::Value)]) -> Vec<(String, bool)> {
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
    run(repo, &["old-step", "begin", "rename s to sum", "--class", "neutral"], 0);
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
    let dir = tempfile::tempdir().unwrap();
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
    assert!(applied.contains("renamed `demo::scale` → `demo::multiply` (proved N1: rename-map)"), "{applied}");
    assert!(applied.contains("verified (mir)"), "no-io uses the MIR: {applied}");
    assert!(applied.contains("the test `demo::tests::t_wide` has a reachable call to `demo::sum_wide`"), "{applied}");
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
/// change, and a refusal from `pair refuse` must stay across applies.
#[test]
fn plan_draft_covers_every_change() {
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");
    let dir = tempfile::tempdir().unwrap();
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

    // A plan without the second rename gives a pair warning. A refusal stays across applies.
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
    // Phase 1 gives no warning for a pair that waits for its identity proof (brief 5f, B1).
    let out = run(repo, &["replay", "apply"], 0);
    assert!(!out.contains("! pair: ") && out.contains("a rename or a move is possible (`ledger::money::fmt_cents` → `ledger::money::format_cents`)"), "{out}");
    let out = run(repo, &["replay", "apply", "--full"], 0);
    assert!(out.contains("! pair: `ledger::money::fmt_cents` (removed)") && out.contains("add it to `refusals`"), "{out}");
    run(repo, &["pair", "refuse", "ledger::money::fmt_cents", "ledger::money::format_cents", "--reason", "a new output contract"], 0);
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
    run(repo, &["claim", "move-item", "--at", "shop_core::over", "to=shop_app::over"], 0);
    let check = run(repo, &["check", "--full"], 0);
    assert!(check.contains("c1 verified (N1"), "{check}");
    assert!(!check.contains("uncovered"), "{check}");

    // A changed body is not a move.
    write(repo, "crates/shop-app/src/main.rs", &app_after(">="));
    let check = run(repo, &["check", "--full"], 1);
    assert!(check.contains("c1 unproved"), "{check}");
    assert!(check.contains("changed operator") && check.contains("shop_core::over/tail"), "{check}");
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
    // A false claim: the claim explains no change.
    run(repo, &["claim", "rename", "--at", "shop_app::main", "to=start"], 0);
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
    let status = run(repo, &["status", "HEAD"], 0);
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
    commit("tidy more");
    let more = short("HEAD");
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V1.replace("20", "60"));
    let line = format!("! 2 commits before this change set have no Semilla check: `{more}` tidy more, `{tidy}` tidy (the last checked commit is `{checked}`)");
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
    assert!(text.lines().take(6).any(|l| l.contains(&line)), "the line is at the top of the report: {text}");
    let html = repo.join(".semilla/review.html");
    run(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains(&format!("(the last checked commit is `{checked}`)")), "the HTML report has the line");
    // The answer comes from git, not from the note: without the note of the checked commit, no
    // commit before the change set has a check.
    git(repo, &["notes", "--ref=semilla", "remove", &checked]);
    let status = run(repo, &["status", "HEAD"], 0);
    assert!(status.contains("! 3 commits before this change set have no Semilla check") && status.contains(&format!("`{checked}` raise the max again")), "{status}");
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
    let status = run(repo, &["status", "HEAD"], 0);
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

    // A text file, a binary file, an `include!` file, and a computed path: a fact of the class
    // `logic` with the body that reads the file. No reason is necessary.
    for (file, body) in [("crates/shop-core/src/notes.md", "shop_core::notes"), ("crates/shop-core/src/rate.txt", "shop_core::rate"), ("crates/shop-core/terms.txt", "shop_core::terms")] {
        let old = std::fs::read_to_string(repo.join(file)).unwrap();
        write(repo, file, &old.replace("Notes", "Notes 2").replace("1", "5").replace("terms", "terms 2"));
        let out = run(repo, &["step", &format!("change {file}")], 0);
        assert!(out.contains(&format!("{file}: changed [logic]")) && out.contains(&format!("affected bodies: {body}")), "{out}");
    }
    std::fs::write(repo.join("crates/shop-core/src/logo.png"), [0x89, b'P', b'N', b'G', 0, 1, 2, 0xfe, 7]).unwrap();
    let out = run(repo, &["step", "change the logo"], 0);
    assert!(out.contains("crates/shop-core/src/logo.png: changed (binary file changed) [logic]") && out.contains("affected bodies: shop_core::logo"), "{out}");
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
        assert!(out.contains(&format!("{file}: changed [other]")) && out.contains("needs a reason") && out.contains(reader), "{out}");
        let out = run(repo, &["step", &format!("change {file}"), "--reason", &format!("{file}:=the text is clearer")], 0);
        assert!(out.contains("accepted"), "{out}");
    }
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
    assert_eq!(line, "result: accepted · 1 step · 1 fact: 0 proved neutral, 1 to read (0 with a spotlight) · 0 claims", "{tagged}");
    assert!(!tagged.contains("score") && !tagged.contains("uncovered"), "{tagged}");

    // After `tag`, no session is open.
    let closed = format!("no open change set; the last tagged commit is {first}");
    let status = run(repo, &["status"], 0);
    assert!(status.starts_with(&closed), "{status}");

    // A new commit with no step on the tagged commit. The advice keeps the tagged commit.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let second = commit(&["-am", "rename the sum"]);
    let out = semilla(repo, &["tag"]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{text}");
    assert!(text.contains(&format!("commit {second} has no step. Run `git reset --soft {first}`, then `semilla step \"<intent>\"`, then `git commit`, then `semilla tag`.")), "{text}");
    assert!(!text.contains(&base) && !text.contains("result:"), "{text}");
    assert!(run(repo, &["status"], 0).starts_with(&closed));
    // The commands of the advice give an accepted change set on the tagged commit.
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
    // Only the body with the addition changes. Before batch 5g, `shop_app::main` was in the list
    // with no change of its MIR: the test build had two bodies with this name (the `main` of the
    // user and the `main` of the test harness), and the profile changes the key of each target.
    assert!(check.contains("affected bodies: shop_core::cart::add\n"), "{check}");
    assert!(!check.contains("shop_app::main") && !check.contains("test-harness-main"), "{check}");
}

#[test]
fn status_checks_again_and_tag_closes_the_change_set() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    run(repo, &["old-step", "begin", "raise the max"], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    // T2 and T3: the `affects` argument is retired.
    let retired = semilla(repo, &["claim", "no-panic", "--at", "shop_app::main", "affects=shop_app::main"]);
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
    assert!(!session.contains("no overflow checks"), "a rejected step is not stored: {session}");
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
    let retired = semilla(repo, &["step", "raise the max", "--claim", "change-literal shop_app::main/S1/args[1]/args[0]"]);
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
    assert!(status.contains("run `semilla status --verbose`"), "{status}");
    let verbose = run(repo, &["status", "HEAD", "--verbose"], 0);
    assert!(verbose.contains("f1 derived") && verbose.contains("\nresult: accepted · 2 steps"), "{verbose}");
    // The default report is short. `--full` adds the change tree and the sources.
    let out = tempfile::tempdir().unwrap();
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
    for (args, flag) in [(&["claim", "no-panic", "--at", "shop_core::cart::Cart::total"][..], "--claim"), (&["reason", "--at", "shop_app::main", "why"], "--reason"), (&["unclaim", "c1"], "semilla undo")] {
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
    assert!(removed.contains("removed `shop_app::tests`") && removed.contains("shop_app::tests::one"), "the test facts own the change: {removed}");
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
    assert!(text.lines().any(|line| line.contains("affected bodies:") && line.contains("shop_app::main")), "the changed `main` of the user shows: {text}");
    // The same check with the last test removed, and with an edit of the text of `main`.
    if output.status.code() != Some(0) {
        run(repo, &["step", "the first test again", "--reason", "shop_app::main=the new trait method wins"], 0);
    }
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V1}{hidden}"));
    let output = semilla(repo, &["step", "remove the test and raise the max"]);
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(text.contains("shop_app::main"), "the edit of `main` shows: {text}");
    assert!(text.contains("removed `shop_app::tests`"), "{text}");
}

#[test]
fn a_rename_and_a_move_need_no_claim() {
    // Design 24.2, rule 4: Semilla derives the identity when the step has no other change.
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
    assert!(renamed.starts_with("accepted: step 0") && renamed.contains("renamed `shop_core::Account.balance` → `shop_core::Account.cents` (proved N1: rename-map"), "{renamed}");
    assert_eq!(renamed.matches('◆').count(), 1, "the forced edits fold under the identity fact: {renamed}");

    // A move with a hidden change: no proof, the change shows, and a note.
    let core_after = core.replace("\npub fn over(account: &Account) -> bool {\n    account.cents > LIMIT\n}\n", "");
    let app_after = |op: &str| app.replace("use shop_core::{Account, over};", "use shop_core::{Account, LIMIT};") + &format!("\npub fn over(account: &Account) -> bool {{\n    account.cents {op} LIMIT\n}}\n");
    write(repo, "crates/shop-core/src/lib.rs", &core_after);
    write(repo, "crates/shop-app/src/main.rs", &app_after(">="));
    let hidden = run(repo, &["step", "move over to the app"], 0);
    assert!(!hidden.contains(" moved `"), "{hidden}");
    assert!(hidden.contains("removed `shop_core::over`") && hidden.contains("the pair has a difference: changed operator `account.cents > LIMIT` → `account.cents >= LIMIT`") && hidden.contains("A move with no other change in its step gets a proof"), "{hidden}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-app/src/main.rs", &app_after(">"));
    let moved = run(repo, &["step", "move over to the app"], 0);
    assert!(moved.contains("moved `shop_core::over` → `shop_app::over` (proved"), "{moved}");
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
    let out = run(repo, &["step", "cap each total", "--claim", r#"rewrite-pattern shop_core::cart::show from="$c.total($m)" to="$c.total($m).min(9)""#], 0);
    assert!(out.contains("✓ c") && out.contains("verified"), "{out}");
    // A missed site refutes the claim.
    run(repo, &["undo"], 0);
    let missed = show.replace("cart.total(10) + cart.total(20)", "cart.total(10).min(9) + cart.total(20)");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{missed}{wide}"));
    let out = semilla(repo, &["step", "cap each total", "--claim", r#"rewrite-pattern shop_core::cart::show from="$c.total($m)" to="$c.total($m).min(9)""#]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stdout));
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
    // `rename` as an explicit claim: an assertion, proved like a derived rename.
    let renamed = capped.replace("pub fn show(", "pub fn display(");
    write(repo, "crates/shop-core/src/cart.rs", &format!("{CART_V0}{renamed}{wide}{share}{test}"));
    let out = run(repo, &["step", "rename show to display", "--claim", "rename shop_core::cart::show to=display"], 0);
    // No other body uses the name, so the bodies are equal at N0.
    assert!(out.contains("✓ c4 verified (N0) [neutral]"), "{out}");
    // `move-item` as an explicit claim. A body of the app uses the name, so the proof needs the
    // name map (N1).
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n\npub fn over(total: u32) -> bool {\n    total > 9\n}\n");
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("println!(\"{}\", cart.total(10));", "println!(\"{} {}\", cart.total(10), shop_core::over(3));"));
    run(repo, &["step", "add over"], 0);
    write(repo, "crates/shop-core/src/lib.rs", "pub mod cart;\n");
    write(repo, "crates/shop-app/src/main.rs", &format!("{}\npub fn over(total: u32) -> bool {{\n    total > 9\n}}\n", MAIN_V0.replace("println!(\"{}\", cart.total(10));", "println!(\"{} {}\", cart.total(10), over(3));")));
    let out = run(repo, &["step", "move over to the app", "--claim", "move-item shop_core::over to=shop_app::over"], 0);
    assert!(out.contains("✓ c5 verified (N1: rename-map) [neutral]"), "{out}");
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
    for (index, function) in [(6, "six"), (7, "seven")] {
        assert!(result(index).contains("verified (mir)") && result(index).contains(&format!("has a reachable call to `shop_core::cart::{function}`. Semilla does not run the test and does not check what the test asserts.")), "c{index}: {text}");
    }
    // Trial 4, D10: a test name with no path resolves in the workspace. The test is in another crate.
    assert!(result(8).contains("verified (mir)") && result(8).contains("the test `shop_app::tests::the_app_uses_one` has a reachable call to `shop_core::cart::one`"), "{text}");
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
    let out = run(repo, &["step", "reorder the fields of Account"], 0);
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
    let dir = setup();
    let out = semilla(&dir.path().join("repo"), &["tag"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("the change set has no step"), "{}", String::from_utf8_lossy(&out.stderr));
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
    assert!(out.contains("[logic]") && out.contains("shop_core::cart::add"), "{out}");
    assert!(!out.contains("exists only"), "{out}");
}

#[test]
fn the_messages_name_the_cause_and_a_command_that_can_succeed() {
    let dir = setup();
    let repo = &dir.path().join("repo");
    // M6: the help lists the new commands first, and hides the old ones.
    let help = run(repo, &["--help"], 0);
    let commands: Vec<&str> = help.lines().skip_while(|l| !l.starts_with("Commands:")).skip(1).take(6).map(|l| l.split_whitespace().next().unwrap_or("")).collect();
    assert_eq!(commands, ["step", "undo", "tag", "status", "review", "ops"], "{help}");
    assert!(!help.contains("\n  claim ") && !help.contains("\n  anchor "), "{help}");
    // M4 and M3: a wrong class names the facts, and the command to run again has the computed class.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let out = semilla(repo, &["step", "raise the max", "--class", "neutral"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("the fact \"changed literal `10` → `20`") && text.contains("give(s) the class `logic`"), "{text}");
    assert!(text.contains("run again (with the computed class): semilla step 'raise the max' --class logic"), "{text}");
    run(repo, &["step", "raise the max", "--class", "logic"], 0);
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
    assert!(edit.ends_with("[logic]"), "{out}");
    assert!(fact_line(&out, "member order of `shop_core::members::Pair`").ends_with("[logic]"), "{out}");
    assert!(!out.contains("neutral rewrite") && !out.contains("neutral (proved)"), "{out}");

    // Case 2: a new field `c` between `a` and `b`, and the reader changes from `t.b` to `t.c`.
    // The reader reads index 1 before and after.
    let code = code.replace("pub struct Trio { pub a: u64, pub b: u64 }", "pub struct Trio { pub a: u64, pub c: u64, pub b: u64 }").replace("{ t.b }", "{ t.c }").replace("Trio { a: x, b: y }", "Trio { a: x, c: x + y, b: y }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "add the field c and read it"], 0);
    assert!(fact_line(&out, "`t.b` → `t.c`").ends_with("[logic]"), "{out}");
    assert!(!out.contains("neutral rewrite") && !out.contains("neutral (proved)"), "{out}");

    // Case 3: the variant order `Large, Small`, and the two arm patterns swap. `small()` gave 1
    // and gives 9.
    let code = code.replace("pub enum Kind { Small, Large }", "pub enum Kind { Large, Small }").replace("match k { Kind::Small => 1, Kind::Large => 9 }", "match k { Kind::Large => 1, Kind::Small => 9 }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "reorder Kind and swap the patterns"], 0);
    let order = fact_line(&out, "member order of `shop_core::members::Kind`");
    assert!(order.contains("the discriminant values") && order.ends_with("[logic]"), "{out}");
    assert!(!out.contains("neutral rewrite") && !out.contains("neutral (proved)"), "{out}");
    assert!(out.contains("affected bodies: shop_core::members::weight") && !out.contains("shop_core::members::small"), "the caller with no edit is equal by name: {out}");
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
    let out = run(repo, &["step", "reorder Pair"], 0);
    let order = fact_line(&out, "member order of `shop_core::members::Pair`");
    assert!(order.ends_with("[logic]"), "{out}");
    assert!(out.contains("<shop_core::members::Pair as std::fmt::Debug>::fmt"), "{out}");
    assert!(!out.contains("indirect change") && !out.contains("members::first") && !out.contains("members::second") && !out.contains("members::make"), "{out}");
    assert_eq!(out.matches('◆').count(), 1, "{out}");

    // A new field in the middle, with no edit of the readers.
    let code = code.replace("pub struct Pair { pub b: u64, pub a: u64 }", "pub struct Pair { pub b: u64, pub c: u64, pub a: u64 }").replace("Pair { a: x, b: y }", "Pair { a: x, c: 0, b: y }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "add the field c"], 0);
    assert!(!out.contains("indirect change") && !out.contains("members::first") && !out.contains("members::second"), "{out}");

    // Case 4: a variant order change of a fieldless enum that a cast reads. No body has a text
    // edit, but `code` gives another number. The fact is `logic` and in the spotlight.
    let code = code.replace("pub enum Kind { Small, Large }", "pub enum Kind { Large, Small }");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "reorder Kind"], 1);
    let order = fact_line(&out, "member order of `shop_core::members::Kind`");
    assert!(order.ends_with("[logic]"), "{out}");
    assert!(out.contains("a cast (`as`) reads (in `shop_core::members::code`)") && out.contains("affected bodies: shop_core::members::code"), "{out}");
    assert!(!out.contains("members::weight"), "a match by name is equal: {out}");
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
    assert!(!out.contains("neutral (proved)") && out.contains("shop_core::members::first"), "{out}");

    // The generic parameters `<B, A>`: `use_size()` gave 1 and gives 8.
    let code = code.replace("pub fn size<A, B>()", "pub fn size<B, A>()");
    write(repo, "crates/shop-core/src/members.rs", &code);
    let out = run(repo, &["step", "swap the generic parameters"], 0);
    assert!(fact_line(&out, "generics of `shop_core::members::size`").ends_with("[logic]"), "{out}");
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
    assert!(out.contains("renamed `shop_core::members::Pair.a` → `shop_core::members::Pair.left` (proved N1: rename-map) [neutral (proved)]"), "{out}");
    assert!(!out.contains("affected bodies"), "{out}");

    // The other direction: a rename with a new member order and the same bytes of the constant
    // (`first` gave 1 and gives 2). The note has the members in the order of the layout, so the
    // rename is not proved, and the bodies with the constant show.
    let swapped = renamed.replace("pub struct Pair { pub left: u64, pub b: u64 }", "pub struct Pair { pub b: u64, pub start: u64 }").replace("Pair { left: 1, b: 2 }", "Pair { b: 1, start: 2 }").replace("origin().left", "origin().start");
    write(repo, "crates/shop-core/src/members.rs", &swapped);
    let out = run(repo, &["step", "rename left to start and reorder"], 0);
    assert!(!out.contains("proved N1") && !out.contains("neutral (proved)"), "{out}");
    assert!(out.contains("shop_core::members::origin"), "{out}");
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
    assert!(honest.contains("<=") && honest.contains("effect hint: "), "{honest}");

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
    let forged = serde_json::json!({ "key": commit, "tree": tree, "hash": "x", "tools": "x", "text": "FORGED: neutral rewrite (proved N2)\n", "report": {} }).to_string();
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
    assert!(!fresh.contains("FORGED") && fresh.contains("effect hint: "), "{fresh}");
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
    // The step changes build code. A reviewer command does not check it with no permission, but
    // the report shows the change of the build script (design 28).
    run(repo, &["review", "--md", md.to_str().unwrap()], 1);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("Not complete") && text.contains("build script") && text.contains("PWNED"), "{text}");
    run(repo, &["review", "--trust-build-code", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("build script") && text.contains("PWNED"), "{text}");

    // The binary in `LICENSE.txt`: Semilla parses it, so a changed condition is a fact of the
    // code, not a file fact of a text file.
    let license = std::fs::read_to_string(repo.join("crates/shop-app/LICENSE.txt")).unwrap();
    write(repo, "crates/shop-app/LICENSE.txt", &license.replace("x < 10", "x <= 10"));
    let out = run(repo, &["step", "reword the license"], 0);
    assert!(out.contains("shop_app::license::limit") && out.contains("[control_flow]"), "{out}");
    assert!(!out.contains("LICENSE.txt: changed"), "{out}");
}

#[test]
fn a_doc_test_is_code() {
    // Brief 5h, B1 (attempt 21). A doc comment with a code block is a test that `cargo test`
    // runs. A change of the block is a fact of the class `logic`, and it stops an identity.
    let dir = setup();
    let repo = &dir.path().join("repo");
    write(repo, "crates/shop-core/src/lib.rs", "//! The shop rules.\n\npub mod cart;\n");
    write(repo, "crates/shop-core/src/cart.rs", &CART_V0.replace("    pub fn total", "    /// The total of the items below `max`.\n    pub fn total"));
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "docs"]);

    // A doc change with no code block stays `other`. The text is the source text of the doc.
    write(repo, "crates/shop-core/src/lib.rs", "//! The rules of the shop.\n\npub mod cart;\n");
    let out = run(repo, &["step", "reword the crate doc"], 0);
    assert!(out.contains("changed docs `//! The shop rules.` → `//! The rules of the shop.` [other]"), "{out}");
    assert!(!out.contains("[doc ="), "{out}");

    // A rename and a new doc test in one step: the identity claim fails, and the doc test is a
    // fact of the class `logic`.
    let renamed = CART_V0.replace("total", "sum_below").replace("    pub fn sum_below", "    /// The total of the items below `max`.\n    ///\n    /// ```no_run\n    /// std::fs::write(\"/tmp/pwned-by-doctest\", \"x\").unwrap();\n    /// ```\n    pub fn sum_below");
    write(repo, "crates/shop-core/src/cart.rs", &renamed);
    write(repo, "crates/shop-app/src/main.rs", &MAIN_V0.replace("total", "sum_below"));
    let out = run(repo, &["step", "rename total", "--claim", "rename shop_core::cart::Cart::total to=sum_below"], 1);
    assert!(out.contains("c1 unproved") && out.contains("doc test of `shop_core::cart::Cart::sum_below` added (block attributes: `no_run`) [logic]"), "{out}");
    // With no claim, Semilla does not derive the identity: the doc test stops it.
    let out = run(repo, &["step", "rename total"], 0);
    assert!(out.contains("doc test of `shop_core::cart::Cart::") && out.contains(" added (block attributes: `no_run`) [logic]") && !out.contains("renamed `shop_core::cart::Cart::total`"), "{out}");
    // The report shows the full lines of the doc.
    let md = dir.path().join("doc.md");
    run(repo, &["status", "--wait", "600"], 0);
    run(repo, &["review", "--full", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("+ /// std::fs::write(\"/tmp/pwned-by-doctest\", \"x\").unwrap();"), "{text}");

    // A changed block attribute is a change of the doc test: now `cargo test` runs the block.
    write(repo, "crates/shop-core/src/cart.rs", &renamed.replace("```no_run", "```"));
    let out = run(repo, &["step", "run the example"], 0);
    assert!(out.contains("doc test of `shop_core::cart::Cart::sum_below` changed [logic]"), "{out}");
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
    assert!(fact_line(&out, "generics of `shop_core::cart::tier`").contains("generic arguments go by position: 1 site(s) in 1 other item(s) [logic]"), "{out}");
    assert!(out.contains("affected bodies: shop_core::cart::gold, shop_core::cart::tier"), "{out}");
    again(repo);

    // Batch 5k, D10: the name in a comment or in a literal is not a site.
    let words = base.replace("    tier::<10, 2>(amount)", "    // tier is the name of a function\n    let _ = \"tier\";\n    tier::<10, 2>(amount)");
    assert_ne!(words, base);
    write(repo, "crates/shop-core/src/cart.rs", &words.replace("tier<const F: u64, const P: u64>", "tier<const P: u64, const F: u64>"));
    let out = run(repo, &["step", "reorder tier"], 0);
    assert!(fact_line(&out, "generics of `shop_core::cart::tier`").contains("generic arguments go by position: 1 site(s) in 1 other item(s) [logic]"), "{out}");
    again(repo);

    // A type: no MIR text changes, and the layout of `Pair<u8, u16>` changes.
    write(repo, "crates/shop-core/src/cart.rs", &base.replace("pub struct Pair<A, B>", "pub struct Pair<B, A>"));
    let out = run(repo, &["step", "reorder Pair"], 0);
    assert!(fact_line(&out, "generics of `shop_core::cart::Pair`").contains("generic arguments go by position: 4 site(s) in 3 other item(s) [logic]"), "{out}");
    assert!(out.contains("affected bodies: shop_core::cart::first_width, shop_core::cart::impl Pair, shop_core::cart::pass") && !out.contains("neutral (proved)"), "{out}");
    again(repo);

    // An impl: the header gives the position of each parameter in the arguments of the type.
    write(repo, "crates/shop-core/src/cart.rs", &base.replace("impl<A, B> Pair<A, B>", "impl<B, A> Pair<B, A>"));
    let out = run(repo, &["step", "reorder the impl"], 0);
    assert!(fact_line(&out, "generics of `shop_core::cart::impl Pair`").contains("generic arguments go by position: 3 site(s) in 2 other item(s) [logic]"), "{out}");
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
    let line = "the build ran with no sandbox: a build script or a proc macro of this change set can change the result";
    let out = run(repo, &["step", "add a build script", "--reason", reason], 0);
    if let Some(cause) = semilla_normal::sandbox::check() {
        // A machine with no Landlock: the check works, and the result has the warning.
        assert!(out.contains(line) && out.contains(&cause), "{out}");
        return;
    }
    assert!(!out.contains("no sandbox"), "{out}");
    assert!(!cache.join("planted.txt").exists(), "the build script wrote the cache folder of the user");
    assert!(!repo.join("planted.txt").exists(), "the build script wrote the repository");
    let planted: Vec<PathBuf> = ["n0", "n2", "n3"].iter().map(|level| repo_cache(repo).join("build").join(level).join("crates/shop-core/src/planted.rs")).filter(|path| path.exists()).collect();
    assert!(planted.is_empty(), "the build script wrote the source files of the build: {planted:?}");
    let status = run(repo, &["status", "--wait"], 0);
    assert!(!status.contains("no sandbox"), "{status}");

    // With no sandbox, the check still works. The step, the status, and the report have the line.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let no_sandbox = |args: &[&str]| {
        let output = semilla_env(repo, args, &[("SEMILLA_NO_SANDBOX", "1")]);
        assert!(output.status.success(), "semilla {args:?}\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let out = no_sandbox(&["step", "rename a local"]);
    assert!(out.contains(&format!("! {line} (the variable SEMILLA_NO_SANDBOX is set)")), "{out}");
    assert!(cache.join("planted.txt").exists(), "with no sandbox, the build script writes the cache folder");
    // The records of this build keep the state: a later result with the sandbox has the line too.
    let status = run(repo, &["status", "--wait"], 0);
    assert!(status.contains(line), "{status}");
    let (md, html) = (dir.path().join("report.md"), dir.path().join("report.html"));
    // The change set changes build code: the reviewer gives the permission to run it.
    run(repo, &["review", "--trust-build-code", "--md", md.to_str().unwrap()], 0);
    run(repo, &["review", "--trust-build-code", "--html", html.to_str().unwrap()], 0);
    let (md, html) = (std::fs::read_to_string(md).unwrap(), std::fs::read_to_string(html).unwrap());
    // The line is at the top of each report: it is the first warning.
    let first_fact = |text: &str| text.find("spotlight (step 0)").unwrap_or(text.len());
    assert!(md.find(line).is_some_and(|at| at < first_fact(&md)), "{md}");
    assert!(html.find(line).is_some_and(|at| at < first_fact(&html)), "the HTML report has the line at the top");
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

    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    run(repo, &["step", "a larger limit"], 0);
    // The data file: phase 1 does not know its effect, so the class is pending, not `other`.
    write(repo, "crates/shop-core/bonus.txt", "900\n");
    let fast = run(repo, &["step", "--fast", "update the bonus data"], 0);
    assert!(fast.contains("accepted (phase 1;") && fast.contains("crates/shop-core/bonus.txt: changed [pending]"), "{fast}");
    assert!(!fast.contains("step 1: phase 2"), "the step itself has no phase 2 line: {fast}");
    // A later step with phase 2 computes the state of the fast step, and gives the commands.
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let later = run(repo, &["step", "rename a local"], 0);
    assert!(later.contains("accepted: step 2"), "{later}");
    assert!(later.contains("step 1: phase 2 rejected: indirect change, cause not known: `shop_core::bonus`"), "{later}");
    assert!(later.contains("to fix step 1 (it is not the last step): run `semilla undo` 2 times.") && later.contains("the files stay"), "{later}");
    let commands: Vec<&str> = later.lines().filter(|line| line.trim_start().starts_with("git restore --source=")).collect();
    assert_eq!(commands.len(), 2, "{later}");
    assert!(commands[0].ends_with("&& semilla step 'update the bonus data' --reason 'shop_core::bonus=<why>'"), "{later}");
    assert!(commands[1].ends_with("&& semilla step 'rename a local'"), "{later}");
    // A later step of `--fast` prints the stored state.
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    let fast = run(repo, &["step", "--fast", "an early return"], 0);
    assert!(fast.contains("step 1: phase 2 rejected: indirect change") && fast.contains("to fix step 1: run `semilla undo` 3 time(s)"), "{fast}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);

    // `tag` rejects the change set with the same commands, and the route works as written.
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-a", "-m", "work"]);
    let refused = semilla(repo, &["tag"]);
    assert!(!refused.status.success());
    let tag = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(tag.starts_with("result: rejected"), "{tag}");
    assert!(tag.contains("✗ step 1: indirect change, cause not known"), "{tag}");
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

/// A build script that writes a marker file. In the sandbox, the write has no effect.
fn marker_build_script(marker: &Path) -> String {
    format!("fn main() {{\n    println!(\"cargo:rerun-if-changed=build.rs\");\n    let _ = std::fs::write({marker:?}, \"the build script ran\");\n}}\n")
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
    let mut text = None;
    for _ in 0..600 {
        let get = || -> Option<String> {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
            write!(stream, "GET /api/changeset HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").ok()?;
            let mut text = String::new();
            stream.read_to_string(&mut text).ok()?;
            Some(text)
        };
        text = get();
        if text.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    text.expect("the UI starts")
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

/// A session of three steps: step 0 is clean, step 1 adds a build script, step 2 changes code.
fn build_code_session(repo: &Path, marker: &Path) {
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let out = run(repo, &["step", "rename a local"], 0);
    assert!(!out.contains("build code"), "a clean step has no line about build code\n{out}");
    write(repo, "crates/shop-core/build.rs", &marker_build_script(marker));
    let out = run(repo, &["step", "add a build script", "--reason", BUILD_REASON], 0);
    let full = "! build code changed in step 1: crates/shop-core/build.rs. It runs during the check and can change each result from step 1 on. A reviewer command checks these steps only with --trust-build-code.";
    assert!(out.contains(full), "{out}");
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let out = run(repo, &["step", "change the limit of the cart"], 0);
    let short = "! build code changed in step 1 (crates/shop-core/build.rs): a reviewer command checks this step only with --trust-build-code.";
    assert!(out.contains(short) && !out.contains(full), "{out}");
}

const NOT_CHECKED_1_2: &str = "steps 1–2 not checked: build code changed in step 1 (crates/shop-core/build.rs). Read that change. Then run the command again with --trust-build-code.";
const TRUSTED_1: &str = "! this result ran the build code that the change set changes (--trust-build-code): crates/shop-core/build.rs (step 1). That code can change each result from step 1 on.";

#[test]
fn a_reviewer_command_runs_no_build_code_that_the_change_set_changes() {
    // Design 28, I6. The agent commands run the changed build code in the sandbox. A reviewer
    // command with no permission does not build a tree with changed build code: the steps from
    // the change on are `not checked`, and the result is `not complete` with the exit code 1.
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

    // The reviewer commands on the session: the Markdown report, the HTML report, and the UI.
    run(repo, &["status", "--wait", "600"], 0);
    let (md, html) = (dir.path().join("r.md"), dir.path().join("r.html"));
    let out = run(repo, &["review", "--md", md.to_str().unwrap()], 1);
    assert!(out.contains("result: not complete") && out.contains(NOT_CHECKED_1_2), "{out}");
    run(repo, &["review", "--html", html.to_str().unwrap()], 1);
    let (md_text, html_text) = (std::fs::read_to_string(&md).unwrap(), std::fs::read_to_string(&html).unwrap());
    let top = |text: &str| text.find(NOT_CHECKED_1_2).is_some_and(|at| at < text.find("step 0").unwrap_or(text.len()) + 200);
    assert!(top(&md_text) && md_text.contains("**Not complete**") && md_text.contains("| Verdict | not complete |"), "{md_text}");
    assert!(top(&html_text) && html_text.contains("Not complete"), "the HTML report has the text at the top");
    // The facts of the build code change show with the spotlight, the reason, and the diff.
    assert!(md_text.contains("derived \\[not checked\\]: crates/shop-core/build.rs: added; spotlight: build script") && md_text.contains("reason: \"the build needs a script\"") && md_text.contains("the build script ran"), "{md_text}");
    let ui = ui_changeset(repo, &[]);
    assert!(ui.contains(NOT_CHECKED_1_2) && ui.contains("\"not_complete\":true"), "{ui}");
    // With the permission: the complete result, and the line at the top of each output.
    let out = run(repo, &["review", "--trust-build-code", "--md", md.to_str().unwrap()], 0);
    assert!(out.contains(TRUSTED_1), "{out}");
    run(repo, &["review", "--trust-build-code", "--html", html.to_str().unwrap()], 0);
    let (md_text, html_text) = (std::fs::read_to_string(&md).unwrap(), std::fs::read_to_string(&html).unwrap());
    assert!(md_text.contains(TRUSTED_1) && md_text.contains("**Accepted**") && !md_text.contains("not checked"), "{md_text}");
    assert!(html_text.contains(TRUSTED_1), "the HTML report has the line");
    let ui = ui_changeset(repo, &["--trust-build-code"]);
    assert!(ui.contains(TRUSTED_1) && ui.contains("\"not_complete\":false"), "{ui}");

    commit_all(repo, "a build script");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted · 3 steps") && out.contains(" · build code changed from step 1\n"), "{out}");

    // A reviewer with an empty cache folder and no sandbox: a build of the changed build code
    // writes the marker file. The reviewer command with no permission does not write it.
    let fresh = dir.path().join("reviewer-cache");
    let env = [("SEMILLA_CACHE_DIR", fresh.to_str().unwrap()), ("SEMILLA_NO_SANDBOX", "1")];
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600", "--verbose"], &env);
    assert_eq!(code, 1, "{out}");
    assert!(!marker.exists(), "a reviewer command with no permission ran the changed build code\n{out}");
    assert!(out.contains("result: not complete · step 0: ") && out.contains(NOT_CHECKED_1_2), "{out}");
    // Step 0 has the full result. Each later step has its facts with the class `not checked`:
    // no proof, no affected bodies, never `neutral`.
    let (before, after) = out.split_once("step 1  ").expect("the text of step 1");
    assert!(before.contains("[neutral (proved)]"), "{out}");
    assert!(after.contains("crates/shop-core/build.rs: added [not checked]") && after.contains("spotlight: build script") && after.contains("reason: \"the build needs a script\""), "{out}");
    let facts = after.split("result: ").next().unwrap();
    assert!(!facts.contains("neutral") && !facts.contains("[logic]") && !facts.contains("affected bodies") && !facts.contains("proved"), "{out}");
    let (code, out) = run_env(repo, &["verify", "--help"], &env);
    assert!(code == 0 && out.contains("--trust-build-code"), "{out}");

    // With the permission, the reviewer command runs the build code and gives the full result.
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600", "--trust-build-code"], &env);
    assert_eq!(code, 0, "{out}");
    assert!(marker.exists(), "with the permission and no sandbox, the build script runs");
    assert!(out.contains(TRUSTED_1) && out.contains("result: accepted · 3 steps") && !out.contains("not checked"), "{out}");

    // The result with the permission has a mark. A command with no permission reads no marked
    // result and no marked record: it gives `not complete` again.
    let (code, out) = run_env(repo, &["status", "HEAD", "--wait", "600"], &env);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("result: not complete · step 0: ") && out.contains(NOT_CHECKED_1_2), "{out}");
}

#[test]
fn build_code_that_goes_back_to_the_base_ends_the_steps_with_no_check() {
    // Design 28, case 7. Step 1 adds a build script and step 3 removes it. The old side of
    // step 3 has changed build code, so a reviewer command with no permission does not check
    // steps 1, 2, and 3. Step 4 has two trees with the build code of the base: the full result.
    if semilla_normal::sandbox::check().is_some() {
        return;
    }
    let dir = setup();
    let repo = &dir.path().join("repo");
    let marker = dir.path().join("marker.txt");
    build_code_session(repo, &marker);
    std::fs::remove_file(repo.join("crates/shop-core/build.rs")).unwrap();
    let out = run(repo, &["step", "remove the build script", "--reason", "crates/shop-core/build.rs:=the build needs no script"], 0);
    assert!(out.contains("! build code changed in step 1 (crates/shop-core/build.rs): a reviewer command checks this step only with --trust-build-code."), "{out}");
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    let out = run(repo, &["step", "an empty cart has the total 0, and the limit is inclusive"], 0);
    assert!(!out.contains("build code"), "step 4 has the build code of the base on the two sides\n{out}");
    commit_all(repo, "a build script comes and goes");
    let out = run(repo, &["tag"], 0);
    assert!(out.contains("result: accepted · 5 steps") && out.contains(" · build code changed in steps 1–3\n"), "{out}");
    let out = run(repo, &["status", "HEAD", "--wait", "600", "--verbose"], 1);
    assert!(out.contains("result: not complete · steps 0, 4: ") && out.contains("steps 1–3 not checked: build code changed in step 1 (crates/shop-core/build.rs). Read that change. Then run the command again with --trust-build-code."), "{out}");
    let step_4 = out.split_once("step 4  ").expect("the text of step 4").1;
    assert!(step_4.contains("[control_flow]") && step_4.contains("affected bodies") && !step_4.split("result: ").next().unwrap().contains("not checked"), "{out}");
    let out = run(repo, &["status", "HEAD", "--wait", "600", "--trust-build-code"], 0);
    assert!(out.contains("(--trust-build-code): crates/shop-core/build.rs (step 1). That code can change each result in steps 1–3.") && out.contains("result: accepted · 5 steps"), "{out}");
}

#[test]
fn a_file_of_a_host_package_is_build_code() {
    // Design 28, A4.5. The compiler runs the code of a proc macro crate during each build. A
    // change of a file of this crate gets the spotlight "build code" and needs a reason. The
    // step also has the line of an agent command, and a reviewer command does not check it.
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
    assert!(out.contains(&format!("! build code changed in step 1: {file}. It runs during the check and can change each result from step 1 on.")), "{out}");
    assert!(out.contains("spotlight: build code: this file is a file of a host package") && out.contains("reason: \"the limit is 20 now\""), "{out}");

    // The report of a reviewer command shows the fact with the diff, and the step has no check.
    run(repo, &["status", "--wait", "600"], 0);
    let md = dir.path().join("host.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 1);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains(&format!("step 1 not checked: build code changed in step 1 ({file})")), "{text}");
    assert!(text.contains("spotlight: build code: this file is a file of a host package") && text.contains("\"20\".parse()"), "{text}");
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
    assert!(out.contains("added comment `// the basket` [other]"), "{out}");
    assert!(!out.contains("changed docs") && !out.contains("needs a reason"), "{out}");
}

const FEES: &str = "pub mod cart;\n\npub const BASE_FEE: u64 = 25;\n\npub fn charge(amount: u64, rate: u64) -> u64 {\n    BASE_FEE + percent(amount, rate)\n}\n\npub fn percent(amount: u64, rate: u64) -> u64 {\n    amount * rate / 100\n}\n\npub fn scaled(amount: u64, rate: u64) -> u64 {\n    debug_assert!(rate <= 100);\n    amount * rate / 100\n}\n\npub fn withdraw(cents: u64, amount: u64) -> u64 {\n    let fee = charge(amount, 1);\n    let cost = amount + fee;\n    cents - cost\n}\n";

/// The spotlight text of a body that reads the build configuration (batch 5k, A1).
const CONFIG_SPOTLIGHT: &str = "build configuration: this code depends on a build condition that Semilla did not check. A proof holds only in the checked configurations.";

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

    // The table of the brief. A use of `cfg!` gets the spotlight, so the step needs a reason.
    let out = probe("if !cfg!(unix) { return 0; }", "", 1);
    assert!(out.contains("[control_flow]") && out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `cfg!(unix)` in `shop_core::percent`") && !out.contains("neutral"), "{out}");
    for statement in ["if usize::BITS != 64 { return 0; }", "if usize::MAX as u64 != u64::MAX { return 0; }", "if core::mem::size_of::<usize>() != 8 { return 0; }", "if core::mem::align_of::<u64>() != 8 { return 0; }", "if (1u64 << 40) as usize == 0 { return 0; }", "if (u64::MAX as usize as u64) != u64::MAX { return 0; }"] {
        let out = probe(statement, "", 0);
        assert!(out.contains("added early exit") && out.contains("[control_flow]") && !out.contains("neutral"), "{statement}: {out}");
    }
    let out = probe("if !CHECKED { return 0; }", "\nconst CHECKED: bool = cfg!(unix);\n", 1);
    assert!(out.contains("added early exit `if !CHECKED { return 0; }` at `shop_core::percent/S0` [control_flow]") && !out.contains("neutral"), "{out}");
    assert!(out.contains("Uses: `cfg!(unix)` in `shop_core::CHECKED`") && out.contains("! spotlight: build configuration: shop_core::CHECKED") && !out.contains("cfg attribute"), "{out}");
    let out = probe("if option_env!(\"LEDGER_FREE\").is_some() { return 0; }", "", 1);
    assert!(out.contains("[control_flow]") && out.contains("Uses: `option_env!(\"LEDGER_FREE\")` in `shop_core::percent`"), "{out}");
    // A plain constant of the workspace is equal in each build: the fold is correct.
    let out = probe("if OFF { return 0; }", "\nconst OFF: bool = false;\n", 0);
    assert!(out.contains("`shop_core::percent`: neutral rewrite (proved"), "{out}");
    let out = probe("if u64::MAX == 0 { return 0; }", "", 0);
    assert!(out.contains("`shop_core::percent`: neutral rewrite (proved"), "{out}");

    // `cfg!` in an `if` expression (trial 6, attempt 10), with a feature, and in a macro of
    // the workspace.
    let out = step(&FEES.replace("let cost = amount + fee;", "let cost = if cfg!(target_os = \"windows\") { amount } else { amount + fee };"), &["step", "probe"], 1);
    assert!(out.contains("[control_flow]") && out.contains("Uses: `cfg!(target_os = \"windows\")` in `shop_core::withdraw`") && !out.contains("neutral"), "{out}");
    let out = probe("if cfg!(feature = \"x\") { return 0; }", "", 1);
    assert!(out.contains("[control_flow]") && out.contains("Uses: `cfg!(feature = \"x\")` in `shop_core::percent`") && !out.contains("neutral"), "{out}");
    let out = step(&FEES.replace(percent, &format!("macro_rules! free {{ () => {{ !cfg!(unix) }}; }}\n\n{percent}    if free!() {{ return 0; }}\n")), &["step", "probe"], 1);
    assert!(out.contains("added early exit `if free!() { return 0; }` at `shop_core::percent/S0` [control_flow]") && out.contains("Uses: `cfg!(unix)` in `shop_core::percent`"), "{out}");

    // `cfg!` hidden in a rename step (attempt 21), with no claim and with a `rename` claim. D5:
    // no line says "neutral rewrite" when a note says that the MIR changed.
    let hidden = FEES.replace("BASE_FEE", "FIXED_FEE").replace("    FIXED_FEE + percent", "    if !cfg!(unix) { return 0; }\n    FIXED_FEE + percent");
    let out = step(&hidden, &["step", "rename the fee"], 1);
    assert!(out.contains("added early exit `if !cfg!(unix) { return 0; }` at `shop_core::charge/S0` [control_flow]") && out.contains(CONFIG_SPOTLIGHT), "{out}");
    assert!(!out.contains("neutral rewrite"), "{out}");
    let out = step(&hidden, &["step", "rename the fee", "--claim", "rename shop_core::BASE_FEE to=FIXED_FEE"], 1);
    assert!(out.contains("is unproved: an identity step must be neutral, but `shop_core::charge` has code that depends on the build configuration") && out.contains(CONFIG_SPOTLIGHT) && !out.contains("neutral rewrite"), "{out}");

    // The honest cases stay proved: a named constant for a literal, also in a body with
    // `debug_assert!`. A removed and an added `debug_assert!` show.
    let out = step(&format!("{}\nconst HUNDRED: u64 = 100;\n", FEES.replace("amount * rate / 100", "amount * rate / HUNDRED")), &["step", "name the literal"], 0);
    assert!(out.contains("2 bodies: neutral rewrite (proved") && out.contains("in: shop_core::percent, shop_core::scaled") && !out.contains("build configuration"), "{out}");
    let out = step(&FEES.replace("    debug_assert!(rate <= 100);\n", ""), &["step", "no assert"], 0);
    assert!(out.contains("debug_assert!(rate <= 100)") && out.contains("affected bodies: shop_core::scaled") && !out.contains("neutral") && !out.contains("build configuration"), "{out}");
    let out = step(&FEES.replace("    debug_assert!(rate <= 100);\n", "    debug_assert!(rate <= 100);\n    debug_assert!(amount > 0);\n"), &["step", "one more assert"], 0);
    assert!(out.contains("debug_assert!(amount > 0)") && out.contains("affected bodies: shop_core::scaled") && !out.contains("neutral"), "{out}");

    // A statement with a `cfg` attribute, in the text and in a macro of the workspace: the MIR of
    // the check configuration does not have this code.
    let out = probe("#[cfg(not(unix))]\n    return 0;", "", 1);
    assert!(out.contains("[control_flow]") && out.contains("Uses: `#[cfg(not(unix))]` in `shop_core::percent`") && !out.contains("neutral"), "{out}");
    let out = step(&FEES.replace(percent, &format!("macro_rules! rel {{ ($e:expr) => {{ #[cfg(not(unix))] {{ $e }} }}; }}\n\n{percent}    rel!(return 0);\n")), &["step", "probe"], 1);
    assert!(out.contains("added macro statement `rel!(return 0);`") && out.contains("Uses: `rel! (#[cfg(not(unix))])` in `shop_core::percent`") && !out.contains("neutral"), "{out}");

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
    assert!(out.contains("`shop_core::withdraw`: neutral rewrite (proved") && !out.contains("build configuration"), "{out}");
    let out = step(&with_if.replace(old_if, "if cost > cents {\n        return !(u64::MAX);\n    }\n    cents - cost"), &["step", "probe"], 0);
    assert!(out.contains("`shop_core::withdraw`: neutral rewrite (proved") && !out.contains("build configuration"), "{out}");
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
    // Batch 5l: a proof of a changed body uses facts of other items. An item that the build
    // configuration selects or fills is opaque: no normal form looks through it.
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
    let uses = |out: &str, text: &str| out.contains("[control_flow]") && out.contains(CONFIG_SPOTLIGHT) && out.contains(&format!("Uses: {text}")) && !out.contains("neutral");

    // Group 1: the item is in the base and has no change.
    let out = probe("if !strict() { return 0; }", "", 1);
    assert!(uses(&out, "`strict` (2 definitions, one under `#[cfg("), "{out}");
    let out = probe("if !wide() { return 0; }", "", 1);
    assert!(uses(&out, "`wide` (`#[cfg(target_pointer_width=\"64\")]`)"), "{out}");
    let out = probe("if LEVEL != 64 { return 0; }", "", 1);
    assert!(uses(&out, "static `LEVEL` (`usize::BITS`)"), "{out}");

    // Group 2: the step adds the item and the statement.
    let out = probe("if !QUICK { return 0; }", "\nstatic QUICK: bool = cfg!(unix);\n", 1);
    assert!(uses(&out, "static `QUICK` (`cfg!(unix)`)"), "{out}");
    let out = probe("if WIDE != 64 { return 0; }", "\nstatic WIDE: u32 = usize::BITS;\n", 1);
    assert!(uses(&out, "static `WIDE` (`usize::BITS`)"), "{out}");
    let out = probe("if !is_quick() { return 0; }", "\nstatic QK: bool = cfg!(unix);\nfn is_quick() -> bool {\n    QK\n}\n", 1);
    assert!(out.contains("added early exit `if !is_quick() { return 0; }`") && out.contains("static `QK` (`cfg!(unix)`) in `shop_core::is_quick`") && !out.contains("neutral"), "{out}");
    let out = probe("if !exact() { return 0; }", "\n#[cfg(unix)]\nfn exact() -> bool {\n    true\n}\n#[cfg(not(unix))]\nfn exact() -> bool {\n    false\n}\n", 1);
    assert!(uses(&out, "`exact` (2 definitions, one under `#[cfg("), "{out}");
    let out = probe("if W::MAX as u64 != u64::MAX { return 0; }", "\n#[cfg(unix)]\ntype W = u64;\n#[cfg(not(unix))]\ntype W = u32;\n", 1);
    assert!(uses(&out, "`W` (2 definitions, one under `#[cfg("), "{out}");
    let out = probe("if !m::q() { return 0; }", "\n#[cfg(unix)]\nmod m {\n    pub fn q() -> bool {\n        true\n    }\n}\n#[cfg(not(unix))]\nmod m {\n    pub fn q() -> bool {\n        false\n    }\n}\n", 1);
    assert!(uses(&out, "`m` (2 definitions, one under `#[cfg("), "{out}");

    // More forms: a method of an `impl` under `cfg`, and an item that a macro with `cfg` makes.
    let out = probe("if !amount.lim() { return 0; }", "\ntrait Lim {\n    fn lim(&self) -> bool;\n}\n#[cfg(unix)]\nimpl Lim for u64 {\n    fn lim(&self) -> bool {\n        true\n    }\n}\n#[cfg(not(unix))]\nimpl Lim for u64 {\n    fn lim(&self) -> bool {\n        false\n    }\n}\n", 1);
    assert!(uses(&out, "`lim` (an `impl` under `cfg`)"), "{out}");
    let out = probe("if !made() { return 0; }", "\nmacro_rules! twice {\n    () => {\n        #[cfg(unix)]\n        fn made() -> bool {\n            true\n        }\n        #[cfg(not(unix))]\n        fn made() -> bool {\n            false\n        }\n    };\n}\ntwice!();\n", 1);
    assert!(uses(&out, "`made` (a macro with `cfg` makes it)"), "{out}");

    // A type with a drop that the configuration selects: the same MIR in the check build is not
    // a proof for an edit that moves the drop.
    let out = step(&base.replace("let _h = hold();", "let _ = hold();"), 0);
    assert!(out.contains("changed pattern `_h` → `_`") && out.contains("[control_flow]") && !out.contains("neutral"), "{out}");

    // The decision: a use in code with no change gets no spotlight, but the body gets no proof.
    let out = step(&base.replace("    helper();\n    1\n", "    helper();\n    if u64::MAX == 0 {\n        return 0;\n    }\n    1\n"), 0);
    assert!(out.contains("at `shop_core::keep/") && out.contains("[control_flow]") && !out.contains("neutral") && !out.contains("build configuration"), "{out}");
    // Batch 5o (A3): a call of a function under `cfg` stays a node if each definition has the
    // same header. An edit in a different part of the body keeps its proof.
    let out = step(&base.replace("    if !strict() {\n        return 0;\n    }\n    amount\n", "    if !strict() {\n        return 0;\n    }\n    if u64::MAX == 0 {\n        return 0;\n    }\n    amount\n"), 0);
    assert!(out.contains("`shop_core::guarded`: neutral rewrite") && !out.contains("build configuration"), "{out}");
    // The edit changes the place of the call: the term is different, so no proof.
    let out = step(&base.replace("    if !strict() {\n        return 0;\n    }\n    amount\n", "    if amount == 0 {\n        return 0;\n    }\n    if !strict() {\n        return 0;\n    }\n    amount\n"), 0);
    assert!(out.contains("shop_core::guarded") && !out.contains("neutral"), "{out}");

    // A function of a different crate with `cfg!`: no form looks into it.
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}\npub fn gate() -> u64 {{\n    if !shop_core::fast() {{\n        return 0;\n    }}\n    1\n}}\n"));
    let out = run(repo, &["step", "probe"], 0);
    run(repo, &["undo"], 0);
    assert!(out.contains("added early exit") && out.contains("[control_flow]") && !out.contains("neutral"), "{out}");

    // A type alias of a crate outside the workspace: the configuration can select its target, and
    // the MIR shows only the target. An alias of the allow list is free.
    write(repo, "crates/shop-app/src/main.rs", &format!("{MAIN_V0}\npub fn gate() -> u64 {{\n    1\n}}\n"));
    let out = probe("if std::ffi::c_long::MAX as i64 != i64::MAX { return 0; }", "", 1);
    assert!(uses(&out, "`core::ffi::primitives::c_long` (a type alias of a different crate)"), "{out}");
    let out = probe("if big() { return 0; }", "\nfn big() -> bool {\n    std::ffi::c_long::MAX as i64 != i64::MAX\n}\n", 0);
    assert!(out.contains("added early exit `if big() { return 0; }`") && !out.contains("neutral"), "{out}");
    let out = probe("if KL != i64::MAX { return 0; }", "\nconst KL: i64 = std::ffi::c_long::MAX;\n", 0);
    assert!(out.contains("added early exit `if KL != i64::MAX { return 0; }`") && !out.contains("neutral"), "{out}");
    let with_alias = base.replace(percent, &format!("{percent}    let seen: std::io::Result<u64> = Ok(amount);\n    if seen.is_err() {{\n        return 1;\n    }}\n"));
    write(repo, "crates/shop-core/src/lib.rs", &with_alias);
    commit_all(repo, "an alias of the allow list");
    let out = step(&with_alias.replace(percent, &format!("{percent}    if u64::MAX == 0 {{ return 0; }}\n")), 0);
    assert!(out.contains("`shop_core::percent`: neutral rewrite (proved") && !out.contains("build configuration"), "{out}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the base again");

    // The default of a `const` parameter is a constant that the body does not show.
    let with_default = format!("{}\npub struct Bits<const N: usize = {{ usize::BITS as usize }}>;\nimpl<const N: usize> Bits<N> {{\n    pub fn get() -> u64 {{\n        N as u64\n    }}\n}}\n", base.replace(percent, &format!("{percent}    if <Bits<64>>::get() != 64 {{ return 0; }}\n")));
    write(repo, "crates/shop-core/src/lib.rs", &with_default);
    commit_all(repo, "a default of a const parameter");
    let out = step(&with_default.replace("<Bits<64>>::get()", "<Bits>::get()"), 0);
    assert!(out.contains("`<Bits<64>>::get` → `<Bits>::get`") && !out.contains("neutral"), "{out}");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the base again");

    // The honest cases stay proved: a static with a literal, and a call of a function with no
    // configuration. The module `tests` under `#[cfg(test)]` does not make `helper` opaque.
    let out = probe("if OFF { return 0; }", "\nstatic OFF: bool = false;\n", 0);
    assert!(out.contains("`shop_core::percent`: neutral rewrite (proved") && !out.contains("build configuration"), "{out}");
    let out = probe("if !helper() { return 0; }", "", 0);
    assert!(out.contains("`shop_core::percent`: neutral rewrite (proved N4: constant-switch)") && !out.contains("build configuration"), "{out}");

    // Batch 5l, P10: a variant under `cfg` changes the set of the values of the type. The check
    // of the variant is not dead code, with `unix` and with a feature.
    for attr in ["#[cfg(not(unix))]", "#[cfg(feature = \"check\")]"] {
        let check = "    assert!(matches!(mode, Mode::Plain), \"no strict mode here\");\n";
        let with_mode = format!("{base}\npub enum Mode {{\n    Plain,\n    {attr}\n    Strict,\n}}\npub fn fee_for(mode: Mode, amount: u64) -> u64 {{\n{check}    percent(amount, 1)\n}}\n");
        write(repo, "crates/shop-core/src/lib.rs", &with_mode);
        commit_all(repo, "a variant under cfg");
        let out = step(&with_mode.replace(check, ""), 0);
        assert!(out.contains("deleted statement `assert!(matches!(mode, Mode::Plain)") && out.contains("at `shop_core::fee_for/S0` [control_flow]") && !out.contains("neutral"), "{attr}: {out}");
    }
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit_all(repo, "the base again");

    // Batch 5l, B3: the body of an associated constant of a trait impl has a MIR name that is
    // not the key of the reason. Each message gives the key.
    let with_const = format!("{}\npub trait K {{\n    const Q: bool;\n}}\nimpl K for u64 {{\n    const Q: bool = cfg!(unix);\n}}\n", base.replace(percent, &format!("{percent}    if !<u64 as K>::Q {{ return 0; }}\n")));
    write(repo, "crates/shop-core/src/lib.rs", &with_const);
    let (body, key) = ("<u64 as shop_core::K>::Q", "shop_core::impl <u64 as K>");
    let out = run(repo, &["step", "probe", "--reason", &format!("{body}=the debug build has more checks")], 1);
    assert!(out.contains(&format!("the reason for `{body}` belongs to no fact of this step; Semilla does not use it. `{body}` is a body of a fact with a spotlight: the key of the reason is `{key}`")), "{out}");
    assert!(out.contains(&format!("Uses: `cfg!(unix)` in `{body}`; it needs a reason: --reason '{key}=…'")), "{out}");
    assert!(out.contains(&format!("! spotlight: build configuration: {body} (the key of the reason: `{key}`)")), "{out}");
    assert!(out.contains(&format!("run again: semilla step 'probe' --reason '{key}=<why>'")), "{out}");
    let out = run(repo, &["step", "probe", "--reason", &format!("{key}=the debug build has more checks")], 0);
    assert!(out.contains("reason: \"the debug build has more checks\"") && !out.contains("belongs to no fact"), "{out}");
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
    // release build calls a different function.
    let out = step("    glob::lim()\n", "    call!()\n", 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `lim` (under `#[cfg(not(unix))]`; the macro `call!` puts the name into the code) in `shop_core::by_call`") && !out.contains("neutral"), "{out}");
    // The edit removes a macro call that gives a constant under `cfg`.
    let out = step("    if x!() != 1 {\n        return 0;\n    }\n", "", 0);
    assert!(out.contains("deleted statement `if x!() != 1 { return 0; }`") && !out.contains("neutral"), "{out}");
    // The value of a constant uses a name under `cfg`. No macro.
    let out = step("    if BY_VALUE != 1 {\n        return 0;\n    }\n", "", 0);
    assert!(out.contains("deleted statement `if BY_VALUE != 1 { return 0; }`") && !out.contains("neutral"), "{out}");
    // `write!` gives a method call `write_fmt`. An inherent method under `cfg` gets the call
    // in the release build.
    let out = step("    std::fmt::Write::write_fmt(w, format_args!(\"x\"))\n", "    write!(w, \"x\")\n", 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `write_fmt` (an `impl` under `cfg`; the macro `write!` puts the name into the code) in `shop_core::by_write`") && !out.contains("neutral"), "{out}");
    // The control: an honest rewrite in a body with a macro that gives no such name.
    let out = step("    let v = two!() + a;\n    v\n", "    two!() + a\n", 0);
    assert!(out.contains("neutral rewrite"), "{out}");
}

#[test]
fn a_trait_fact_of_a_different_configuration_can_change_a_call() {
    // Batch 5q: a generic `impl` with a bound gives its methods to a type when the bound
    // starts to hold. The driver gives the rival of each call. The verifier makes the call an
    // input if a trait fact of the rival can change.
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
    let out = step("    tr::Other::dig(&t)\n", "    t.dig()\n", 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `dig` (the call means `Other::dig` now; `Sup::dig` wins if `Coin: Sup` holds in a different configuration) in `shop_core::by_rival`") && !out.contains("neutral"), "{out}");
    // The debug build has `Bill: Flag`: the call means `Low::low`. The release build does not
    // have it: the call means `Base::low`. The winner uses a fact that can change.
    let out = step("    let _ = &t;\n    1\n", "    t.low()\n", 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `low` (the call means `Low::low` now because of `impl Low for Bill`; a different configuration can change that fact) in `shop_core::by_fact`") && !out.contains("neutral"), "{out}");
    // The control: the calls of an honest rewrite have no rival with such a fact.
    let out = step("    let n = v.iter().map(|y| y + 1).sum::<u64>();\n    n + a\n", "    v.iter().map(|y| y + 1).sum::<u64>() + a\n", 0);
    assert!(out.contains("neutral rewrite"), "{out}");
    // A path in the signature of a nested item (`I::Item`) has no type check result in the
    // body around it. The driver must not ask for one.
    let out = step("    let v = first([a].into_iter()).unwrap_or(0);\n    v\n", "    first([a].into_iter()).unwrap_or(0)\n", 0);
    assert!(out.contains("neutral rewrite") && !out.contains("compiler error"), "{out}");
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
    let out = unstable("#![cfg_attr(unix, feature(exact_size_is_empty))]\n", "ExactSizeIterator::is_empty(&it)", "ExactSizeIterator::is_empty(&it)", "it.is_empty()", 1);
    assert!(
        out.contains("Uses: `is_empty` (the call means `ExactSizeIterator::is_empty` now by the feature `exact_size_is_empty`; a configuration with no such feature can select a different item) in `shop_core::pick`")
            && !out.contains("neutral"),
        "{out}"
    );
    // The unstable candidate holds now: a build with the feature gives the call to it.
    let out = unstable("#![cfg_attr(not(unix), feature(exact_size_is_empty))]\n", "Emp::is_empty(&mut it)", "Emp::is_empty(&mut it)", "it.is_empty()", 1);
    assert!(
        out.contains("Uses: `is_empty` (the call means `Emp::is_empty` now; `ExactSizeIterator::is_empty` wins if the crate gets its unstable feature in a different configuration) in `shop_core::pick`")
            && !out.contains("neutral"),
        "{out}"
    );
    // The control: the unstable candidate does not hold for an array, and no `impl` of the
    // workspace can make it hold. The call is not an input. The crate has no attribute
    // under `cfg` here: such an attribute at the root makes each `impl` of the root a fact
    // that can change.
    let out = unstable("", "Emp::is_empty(&mut it)", "    let e = [a].is_empty();\n    e\n", "    [a].is_empty()\n", 0);
    assert!(out.contains("neutral rewrite"), "{out}");
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
    assert!(out.contains("`shop_core::plain`: neutral rewrite"), "{out}");
    let out = orphan("    Ord::cmp(&a, &b)\n", "    a.cmp(&b)\n", 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `cmp` (the call means `Ord::cmp` now; `Iterator::cmp` wins if `Tok: Iterator` holds in a different configuration) in `shop_core::local`") && !out.contains("neutral"), "{out}");
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
    assert!(out.contains("deleted statement `assert!(matches!(mode, shop_dep::Mode::Plain)") && !out.contains("neutral"), "{out}");
    // The value has no drop code in this build. The feature `check` adds a `Drop` impl.
    let out = step(&base.replace(hold, "    let _ = shop_dep::loud();\n"));
    assert!(out.contains("changed pattern `_h` → `_` at `shop_core::hold/S0/pat` [control_flow]") && !out.contains("neutral") && !out.contains("renamed local"), "{out}");
    // A body with a call of that crate and with no type of it keeps its proof.
    let out = step(&base.replace(far, "    let v = shop_dep::next(n);\n    v + 1\n"));
    assert!(out.contains("`shop_core::far`: neutral rewrite (proved N0)"), "{out}");
}

#[test]
fn a_trait_impl_of_a_crate_outside_the_workspace_can_change_a_call() {
    // Batch 5q: Semilla does not read the source of a crate outside the workspace. A feature
    // of that crate can add or remove an `impl`. A call whose meaning uses such an `impl`, or
    // whose rival needs one, is an input.
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
    let out = step("    shop_dep::Loud::size(&n)\n", "    n.size()\n", 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `size` (the call means `Loud::size` now because of `impl Loud for u64`; a different configuration can change that fact) in `shop_core::by_fact`") && !out.contains("neutral"), "{out}");
    // The call means `Quiet::deep`. The feature `check` adds `impl Soft for u32`: then the
    // call means `Soft::deep`.
    let out = step("    Quiet::deep(&mut n)\n", "    n.deep()\n", 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `deep` (the call means `Quiet::deep` now; `Soft::deep` wins if `u32: Soft` holds in a different configuration) in `shop_core::by_rival`") && !out.contains("neutral"), "{out}");
    // The control: no trait of the other crate has the name `calm`.
    let out = step("    let v = n.calm();\n    v\n", "    n.calm()\n", 0);
    assert!(out.contains("`shop_core::honest`: neutral rewrite"), "{out}");
}

/// The intent of cheat B (trial 6): a line break, a false result line, and the ESC sequence that
/// hides the text after it.
const CHEAT_B: &str = "set the fee rate at build time\nresult: accepted · 1 step · 1 fact: 1 proved neutral, 0 to read (0 with a spotlight) · 0 claims\u{1b}[8m";
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
        assert!(!line.contains("1 proved neutral, 0 to read"), "{name}: the result line of the agent\n{text}");
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
    // The reviewer commands, with and with no permission for the changed build code.
    let (_, out) = run_env(repo, &["status", "HEAD"], &[]);
    no_forged_line("status HEAD", &out, 1);
    let (_, out) = run_env(repo, &["status", "HEAD", "--trust-build-code", "--verbose"], &[]);
    no_forged_line("status HEAD --trust-build-code", &out, 1);
    assert!(out.contains("\"set the fee rate at build time\\nresult: accepted"), "{out}");
    assert!(out.contains("reason: \"set the fee rate at build time\\nresult: accepted"), "{out}");
    let md = dir.path().join("review.md");
    let (_, out) = run_env(repo, &["review", "HEAD", "--trust-build-code", "--full", "--md", md.to_str().unwrap()], &[]);
    no_forged_line("review --md", &out, 0);
    let text = std::fs::read_to_string(&md).unwrap();
    no_forged_line("the Markdown report", &text, 0);
    assert!(text.contains(CHEAT_B_START) && text.contains("Text in quote marks is the text of the agent. Semilla does not check it."), "{text}");
    // No line of the Markdown report starts with the words of the agent after the line break.
    assert!(!text.lines().any(|line| line.trim_start().starts_with("result")), "{text}");
    let html = dir.path().join("review.html");
    let (_, out) = run_env(repo, &["review", "HEAD", "--trust-build-code", "--html", html.to_str().unwrap()], &[]);
    no_forged_line("review --html", &out, 0);
    let text = std::fs::read_to_string(&html).unwrap();
    no_forged_line("the HTML report", &text, 0);
    assert!(text.contains("&quot;set the fee rate at build time\\nresult: accepted") && text.contains("Text in quote marks is the text of the agent. Semilla does not check it."), "the HTML report shows the escape\n{text}");
    // The UI data: JSON. Each hidden character is a JSON escape, and the UI shows it as an escape.
    let ui = ui_changeset(repo, &["--trust-build-code"]);
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
    let out = run(repo, &["step", "odd names"], 0);
    no_forged_line("step", &out, 0);
    assert!(fact_line(&out, "docs/a").contains("docs/a\\nresult: accepted\\u{1b}[8m.md: added [other]"), "{out}");
    assert!(fact_line(&out, "changed literal").contains("`\"{}\"` → `\"\\u{202e}{}\"`"), "{out}");
    let (_, out) = run_env(repo, &["status"], &[]);
    no_forged_line("status", &out, 1);
    commit_all(repo, "odd names");
    run(repo, &["tag"], 0);
    let md = dir.path().join("review.md");
    let html = dir.path().join("review.html");
    let (_, out) = run_env(repo, &["review", "HEAD", "--full", "--md", md.to_str().unwrap()], &[]);
    no_forged_line("review --md", &out, 0);
    let (_, out) = run_env(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], &[]);
    no_forged_line("review --html", &out, 0);
    for file in [&md, &html] {
        let text = std::fs::read_to_string(file).unwrap();
        no_forged_line(&file.display().to_string(), &text, 0);
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
        assert!(text.contains("· behavior · \"tidy the main function\"") && text.contains("\"only docs\"") && !text.contains("refactor"), "{name}: {text}");
        assert!(text.lines().take(6).any(|line| line == "Text in quote marks is the text of the agent. Semilla does not check it."), "{name}: {text}");
        run(repo, &["review", "HEAD", "--html", html.to_str().unwrap()], 0);
        let text = std::fs::read_to_string(&html).unwrap();
        assert!(text.contains("<span class=\"kind\">behavior</span>") && text.contains("&quot;tidy the main function&quot;") && text.contains("&quot;only docs&quot;"), "{name}: {text}");
        assert!(!text.contains("<span class=\"kind\">refactor</span>") && text.contains("Text in quote marks is the text of the agent. Semilla does not check it."), "{name}: {text}");
        let ui = ui_changeset(repo, &[]);
        let body = ui.split("\r\n\r\n").nth(1).unwrap_or_default();
        assert!(body.contains("\"kind\":\"behavior\"") && !body.contains("\"kind\":\"refactor\""), "{name}: {body}");
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
    assert!(text.contains("· behavior · \"tidy the main function\"") && !text.contains("· rename ·"), "{text}");
    let ui = ui_changeset(repo, &[]);
    let body = ui.split("\r\n\r\n").nth(1).unwrap_or_default();
    assert!(body.contains("\"kind\":\"behavior\"") && !body.contains("\"kind\":\"rename\""), "{body}");
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
    // configurations. The step needs no reason. The fact is not neutral in each output.
    let note = "note: proved neutral in `dev`, `dev, all features`; not proved in `release`, `release, all features`";
    let not_neutral = |out: &str| out.contains(note) && !out.contains("neutral rewrite") && !out.contains("neutral (proved)") && !out.contains("spotlight:") && !out.contains("MISSING") && !out.contains("Uses:");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("let _h = hold();", "let _ = hold();"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(not_neutral(&out) && out.contains("accepted"), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(not_neutral(&out), "{out}");
    let md = repo.join(".semilla/note.md");
    let html = repo.join(".semilla/note.html");
    let out = run(repo, &["review", "--md", md.to_str().unwrap(), "--html", html.to_str().unwrap()], 0);
    assert!(!out.contains("neutral (proved)"), "{out}");
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(not_neutral(&text), "{text}");
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains("proved neutral in") && text.contains("not proved in") && !text.contains("neutral (proved)"), "{text}");
    let out = run(repo, &["verify"], 0);
    assert!(not_neutral(&out), "{out}");
    let answers = mcp_calls(repo, &[("results", serde_json::json!({"verbose": true, "wait": 600})), ("undo", serde_json::json!({})), ("step", serde_json::json!({"intent": "probe"}))]);
    assert!(not_neutral(&answers[0].0) && !answers[0].1, "{answers:?}");
    assert!(not_neutral(&answers[2].0) && !answers[2].1 && answers[2].0.contains("accepted"), "{answers:?}");
    run(repo, &["undo"], 0);

    // An honest rewrite of a body that only the feature `fast` compiles: the configuration with
    // all features has the proof. The step output has no line for the configurations.
    let out = step(&base.replace("    a * 2\n", "    let b = a * 2;\n    b\n"), 0);
    assert!(out.contains("`shop_core::only_fast`: neutral rewrite") && !out.contains("configuration"), "{out}");

    // Part C: the list covers `debug_assertions`, so a call of a function with a definition for
    // each value does not stop the proof. The proof holds in each checked configuration.
    let out = step(&base.replace("    chk(a) + 1\n", "    let b = chk(a);\n    b + 1\n"), 0);
    assert!(out.contains("`shop_core::via`: neutral rewrite") && !out.contains("build configuration"), "{out}");

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
    // the rule of the build configuration stays.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace(percent, &format!("{percent}    if cfg!(feature = \"fast\") {{\n        return 7;\n    }}\n")));
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains(gap) && out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `cfg!(feature = \"fast\")` in `shop_core::percent`"), "{out}");

    // A step that breaks the release profile only: the message names the configuration and
    // shows the error of the compiler.
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}\n#[cfg(not(debug_assertions))]\npub fn broken() -> u64 {{\n    \"text\"\n}}\n"));
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains("the code after the step does not compile in the configuration `release` (it compiles before the step)") && out.contains("mismatched types"), "{out}");

    // An honest step: one line for the configurations with no check, and the proof of the
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
    assert!(out.contains("; not proved in `release`") && !out.contains(CONFIG_SPOTLIGHT) && !out.contains("neutral rewrite"), "{out}");
    run(repo, &["undo"], 0);

    // A file under `tests/`: `test` is true in each unit that reads the file. The item under
    // `not(test)` is in no build, and the condition is not covered.
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "crates/shop-core/tests/it.rs", &test_base.replace("{ a / m::lim() }", "{ a / lim() }"));
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("under `#[cfg(not(test))]`) in `shop_core::it::via`") && !out.contains("neutral rewrite"), "{out}");
    write(repo, "crates/shop-core/tests/it.rs", &test_base);

    // The flags of the compiler set the debug assertions on in each build. No unit has the
    // condition false: the proof of the step holds in no build with the assertions off.
    write(repo, ".cargo/config.toml", "[build]\nrustflags = [\"-C\", \"debug-assertions=on\"]\n");
    commit("the flags");
    write(repo, "crates/shop-core/src/lib.rs", &step);
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("under `#[cfg(not(debug_assertions))]`) in `shop_core::via`") && !out.contains("neutral rewrite"), "{out}");

    // The header names the condition: the list does not cover it.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("pub fn percent(amount: u64, rate: u64) -> u64 {\n    amount * rate / 100\n", "pub fn percent(amount: u64, rate: u64) -> u64 {\n    let part = amount * rate;\n    part / 100\n"));
    run(repo, &["step", "an honest rewrite"], 0);
    let out = run(repo, &["status", "--wait", "600"], 0);
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
    assert!(out.contains(&format!("; not proved in `{narrow}`")) && !out.contains(CONFIG_SPOTLIGHT) && !out.contains("neutral rewrite"), "{out}");
    run(repo, &["undo"], 0);

    // A step that breaks the smaller build is a compile error.
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}\n#[cfg(not(feature = \"std\"))]\npub fn broken() -> u64 {{\n    \"text\"\n}}\n"));
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains(&format!("the code after the step does not compile in the configuration `{narrow}` (it compiles before the step)")) && out.contains("mismatched types"), "{out}");

    // An honest step: the header names the smaller builds, and no configuration has a gap.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("{ a / m::lim() }", "{\n    let l = m::lim();\n    a / l\n}"));
    let out = run(repo, &["step", "an honest rewrite"], 0);
    assert!(out.contains("`shop_core::via`: neutral rewrite") && !out.contains("not checked"), "{out}");
    let out = run(repo, &["status", "--wait", "600"], 0);
    let line = out.lines().find(|line| line.contains("checked configurations (target `")).unwrap_or_default();
    assert!(line.contains("`dev, no default features` (libraries and binaries only: the other targets do not build), `release, no default features` (libraries and binaries only: the other targets do not build)"), "{out}");
    assert!(line.contains("`dev`, `release`") && !out.contains("are not checked") && !out.contains("`feature = \"std\"`"), "{out}");
}

#[test]
fn a_fact_with_no_proof_needs_no_reason_for_a_covered_condition() {
    // Batch 5u, part B (probes k4, k5, k9, b3): the step adds an early exit that reads
    // `debug_assertions` through a constant. The two profiles cover the condition. The body
    // is not equal, so the fact has no proof and its class is not `neutral`: the reviewer
    // reads it. The step needs no reason.
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
        assert!(out.contains(&format!("added early exit `{statement}` at `shop_core::percent/S0` [control_flow]")), "{statement}: {out}");
        assert!(!out.contains("neutral") && !out.contains("spotlight:") && !out.contains("not checked together"), "{statement}: {out}");
    }
    // The same step with a condition that no checked configuration makes false keeps the
    // spotlight: the step needs a reason.
    let out = probe("if !QC { return 0; }", "const QC: bool = cfg!(unix);", 1);
    assert!(out.contains("[control_flow]") && out.contains(CONFIG_SPOTLIGHT) && !out.contains("neutral"), "{out}");
}

#[test]
fn a_removed_proof_with_no_spotlight_has_a_note_with_the_cause() {
    // Batch 5u (an addition to part A): the body of `use_h` calls `l0`, and a `cfg` attribute
    // selects the definition of `l0`. No checked configuration has `unix` off. An honest
    // rewrite of a different part of the body has no proof and no spotlight. The fact gets a
    // note with the cause. The decision does not change.
    let dir = setup();
    let repo = &dir.path().join("repo");
    let base = format!("{FEES}\nmod m0 {{\n    pub fn l0() -> u64 {{\n        1\n    }}\n}}\nuse m0::*;\n#[cfg(unix)]\nfn l0() -> u64 {{\n    2\n}}\npub fn use_h(a: u64, b: u64) -> u64 {{\n    if a > b {{ l0() + a }} else {{ 0 }}\n}}\npub fn gate(a: u64, b: u64) -> u64 {{\n    #[cfg(not(unix))]\n    return 0;\n    if a > b {{ a }} else {{ 0 }}\n}}\n");
    write(repo, "crates/shop-core/src/lib.rs", &base);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
    git(repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a name under cfg"]);
    let note = "note: no proof: `shop_core::use_h` uses `l0` (under `#[cfg(unix)]`), and Semilla did not check that condition";
    let shown = |out: &str| out.contains(note) && out.contains("[control_flow]") && !out.contains("spotlight:") && !out.contains("neutral rewrite") && !out.contains("neutral (proved)");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("if a > b { l0() + a }", "if b < a { l0() + a }"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(shown(&out) && out.contains("accepted"), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(shown(&out), "{out}");
    let md = repo.join(".semilla/review.md");
    run(repo, &["review", "--md", md.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains("no proof: `shop_core::use_h` uses `l0` (under `#[cfg(unix)]`), and Semilla did not check that condition"), "{text}");
    run(repo, &["undo"], 0);

    // The same for a statement with a `cfg` attribute that the edit does not touch.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("    if a > b { a } else { 0 }", "    if b < a { a } else { 0 }"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("note: no proof: `shop_core::gate` uses `#[cfg(not(unix))]`, and Semilla did not check that condition") && !out.contains("spotlight:") && !out.contains("neutral rewrite"), "{out}");
    run(repo, &["undo"], 0);

    // Negative: an edit of the code that reads the condition keeps the spotlight. The step
    // needs a reason, and the fact has no note.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("{ l0() + a }", "{ a + l0() }"));
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains("Uses: `l0` (under `#[cfg(unix)]`) in `shop_core::use_h`") && !out.contains("no proof:"), "{out}");
    // Negative: a body with no name under `cfg` keeps its proof, with no note.
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("    amount * rate / 100\n}\n\npub fn scaled", "    rate * amount / 100\n}\n\npub fn scaled"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("`shop_core::percent`: neutral rewrite (proved") && !out.contains("no proof:"), "{out}");
}

#[test]
fn an_added_configuration_is_a_check_and_not_a_proof() {
    // Batch 5u, part C: the negative cases. No case gives a neutral fact.
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
    let out = run(repo, &["step", "probe"], 1);
    assert!(out.contains("Semilla added the configurations `dev, features fast`, `release, features fast`"), "{out}");
    assert!(out.contains("; not proved in `dev, features fast`, `release, features fast`") && !out.contains("neutral rewrite"), "{out}");

    // 2. The state does not build before the step (the build with only `fast` has no
    // function `only_slow`): the added configuration is not checked, and the gap stays. The
    // text gives the cause.
    let base = format!("{FEES}{}{}#[cfg(feature = \"fast\")]\npub fn need() -> u64 {{ only_slow() }}\n#[cfg(feature = \"slow\")]\npub fn only_slow() -> u64 {{ 1 }}\npub fn cut(a: u64) -> u64 {{ a as TA as TB as u64 }}\n", alias("TA", "fast"), alias("TB", "slow"));
    write(repo, "crates/shop-core/src/lib.rs", &base);
    commit("a state with no build");
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("{ a as TA as TB as u64 }", "{ let b = a as TA; b as TB as u64 }"));
    let out = run(repo, &["step", "probe"], 1);
    let gap = "Not checked together: `feature = \"fast\"` on with `feature = \"slow\"` off. Semilla added the configuration `dev, features fast` for this state. The configuration does not build before the step, or it does not give this state";
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains(gap) && !out.contains("neutral rewrite"), "{out}");
    assert!(out.contains("configurations `dev, features fast`, `release, features fast` are not checked: it does not build before the step"), "{out}");

    // 3. Three features: the proof needs six states that the default list does not have.
    // The limit is four added selections. The gap stays, with the present message.
    let three = format!("{FEES}{}{}{}pub fn cut(a: u64) -> u64 {{ a as TA as TB as TC as u64 }}\n", alias("TA", "fa"), alias("TB", "fb"), alias("TC", "fc"));
    write(repo, "crates/shop-core/src/lib.rs", &three);
    write(repo, "crates/shop-core/Cargo.toml", &manifest("fa = []\nfb = []\nfc = []\n"));
    commit("three features");
    let honest = three.replace("{ a as TA as TB as TC as u64 }", "{ let b = a as TA; b as TB as TC as u64 }");
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 1);
    let gap = "Not checked together: `feature = \"fa\"` on with `feature = \"fb\"` off with `feature = \"fc\"` on. Add `feature-sets = [[\"fa\", \"fc\"]]` to `[workspace.metadata.semilla]`";
    assert!(out.contains(CONFIG_SPOTLIGHT) && out.contains(gap) && !out.contains("neutral rewrite"), "{out}");
    // The user lists the two states that are left: the four added selections give the others.
    write(repo, "crates/shop-core/src/lib.rs", &three);
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n[workspace.metadata.semilla]\nfeature-sets = [[\"fa\", \"fc\"], [\"fb\", \"fc\"]]\n");
    commit("two feature sets");
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("`shop_core::cut`: neutral rewrite") && out.contains("`dev, features fa`, `release, features fa`, `dev, features fb`, `release, features fb`, `dev, features fa,fb`, `release, features fa,fb`, `dev, features fc`, `release, features fc` for a proof"), "{out}");
    run(repo, &["undo"], 0);
    // The false step of this form is not equal in a listed configuration. It has no proof,
    // and Semilla adds no configuration for it.
    write(repo, "crates/shop-core/src/lib.rs", &three.replace("a as TA as TB as TC as u64", "a as TB as TC as u64"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("; not proved in `dev, features fa,fc`") && !out.contains("neutral rewrite") && !out.contains("Semilla added"), "{out}");
}

#[test]
fn a_proof_with_two_conditions_needs_each_joint_state() {
    // Batch 5t, part B. Each of the two features is covered. The proof uses the two
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
    let added = "Semilla added the configurations `dev, features fast`, `release, features fast`, `dev, features slow`, `release, features slow` for a proof";
    let mark = "`dev, features fast` (added for a proof), `release, features fast` (added for a proof), `dev, features slow` (added for a proof), `release, features slow` (added for a proof)";

    // The bodies are equal in each configuration of the default list (the two aliases are
    // `u64`, or the two are `u8`). The proof does not hold for the build with only `fast`:
    // the added configuration shows it. The fact is not neutral in each output.
    let note = "note: proved neutral in `dev`, `dev, all features`, `dev, features slow`, `release`, `release, all features`, `release, features slow`; not proved in `dev, features fast`, `release, features fast`";
    let not_neutral = |out: &str| out.contains(note) && !out.contains("neutral rewrite") && !out.contains("neutral (proved)") && !out.contains("spotlight:");
    write(repo, "crates/shop-core/src/lib.rs", &drop);
    let out = run(repo, &["step", "probe"], 0);
    assert!(not_neutral(&out) && out.contains(added) && !out.contains("feature-sets"), "{out}");
    let out = run(repo, &["verify"], 0);
    assert!(not_neutral(&out) && out.contains(mark), "{out}");
    run(repo, &["undo"], 0);

    // The honest rewrite has a proof in each configuration, also in the added ones. Each
    // output for the reviewer lists the added configurations with a mark.
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("`shop_core::cut`: neutral rewrite") && out.contains(added) && !out.contains("build configuration"), "{out}");
    let out = run(repo, &["status", "--verbose", "--wait", "600"], 0);
    assert!(out.contains(mark) && out.contains("neutral rewrite"), "{out}");
    let md = repo.join(".semilla/note.md");
    let html = repo.join(".semilla/note.html");
    run(repo, &["review", "--md", md.to_str().unwrap(), "--html", html.to_str().unwrap()], 0);
    let text = std::fs::read_to_string(&md).unwrap();
    assert!(text.contains(mark), "{text}");
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains("(added for a proof)"), "{text}");
    let answers = mcp_calls(repo, &[("results", serde_json::json!({"verbose": true, "wait": 600}))]);
    assert!(answers[0].0.contains(mark) && !answers[0].1, "{answers:?}");
    // The added list is a function of the two trees only: a check with no cache gives the
    // same list and the same classes.
    let lines = |out: &str| -> Vec<String> { out.lines().filter(|line| line.contains("checked configurations") || line.contains("neutral") || line.contains('[')).map(str::to_string).collect() };
    let first = run(repo, &["verify"], 0);
    assert!(first.contains(mark) && first.contains("neutral rewrite"), "{first}");
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
    write(repo, "crates/shop-core/src/lib.rs", &base.replace("amount * rate / 100\n}\n\npub fn scaled", "let all = amount * rate;\n    all / 100\n}\n\npub fn scaled"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("neutral rewrite") && !out.contains("added"), "{out}");
    let out = run(repo, &["verify"], 0);
    assert!(!out.contains("added for a proof") && !out.contains("features fast"), "{out}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/lib.rs", &base);
    // A body that uses one of the conditions keeps the rule of batch 5s.
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}pub fn one(a: u64) -> u64 {{\n    let b = a as TA;\n    b as u64\n}}\n"));
    commit("one alias");
    write(repo, "crates/shop-core/src/lib.rs", &format!("{base}pub fn one(a: u64) -> u64 {{\n    a as TA as u64\n}}\n"));
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("`shop_core::one`: neutral rewrite") && !out.contains("build configuration"), "{out}");
    run(repo, &["undo"], 0);

    // The list has each joint state after the two entries. The honest rewrite has a proof,
    // and the false step is not equal in the build with only `fast`.
    write(repo, "crates/shop-core/src/lib.rs", &base);
    write(repo, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n[workspace.metadata.semilla]\nfeature-sets = [[\"fast\"], [\"slow\"]]\n");
    commit("the feature sets");
    write(repo, "crates/shop-core/src/lib.rs", &honest);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("`shop_core::cut`: neutral rewrite") && !out.contains("build configuration") && !out.contains("added"), "{out}");
    run(repo, &["undo"], 0);
    write(repo, "crates/shop-core/src/lib.rs", &drop);
    let out = run(repo, &["step", "probe"], 0);
    assert!(out.contains("; not proved in `dev, features fast`, `release, features fast`") && !out.contains("neutral rewrite") && !out.contains("added"), "{out}");
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

fn rules_5u_setup() -> tempfile::TempDir {
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
    // same value before and after a call. The rewrite has a proof. The functions have the
    // form of `bs58-c-0001` and `bs58-c-0019`: the optimizations of the compiler do not remove
    // the copy of the loop variable.
    let dir = rules_5u_setup();
    let repo = &dir.path().join("repo");
    let out = rules_5u_probe(repo, "i < encode.len()", "encode.len() > i");
    assert!(out.contains("`shop_core::scan`: neutral rewrite (proved"), "{out}");
    let out = rules_5u_probe(repo, "index += e.valid_up_to();", "index = index + (e.valid_up_to());");
    assert!(out.contains("`shop_core::grow`: neutral rewrite (proved"), "{out}");
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
        assert!(!out.contains("neutral"), "{old}: {out}");
        assert!(out.contains("[control_flow]") || out.contains("[logic]"), "{old}: {out}");
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
        assert!(out.contains(&format!("`shop_core::{name}`: neutral rewrite (proved")), "{out}");
        // Negative test: the opposite condition is a real change.
        let new = format!("pub fn {name}(a: {ty}, b: {ty}) -> u64 {{\n    if a == b {{ 1 }} else {{ 2 }}");
        let out = rules_5u_probe(repo, &old, &new);
        assert!(!out.contains("neutral"), "{out}");
    }
    // Negative test: a type with a `PartialEq` impl. The `ne` function of the impl is not the
    // opposite of its `eq` function. The step changes the result.
    let old = "pub fn differ_odd(a: &Odd, b: &Odd) -> u64 {\n    if a != b { 1 } else { 2 }";
    let new = "pub fn differ_odd(a: &Odd, b: &Odd) -> u64 {\n    if !(a == b) { 1 } else { 2 }";
    let out = rules_5u_probe(repo, old, new);
    assert!(!out.contains("neutral"), "{out}");
}
