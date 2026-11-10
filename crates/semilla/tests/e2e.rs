//! End-to-end test: an agent tags its work with the real `semilla` binary, and the verifier, the
//! storage, the MCP server, and the UI work on a real git repository with a two-crate workspace.
//!
//! The test builds the MIR driver first. It uses Z3 from `.semilla/tools/z3` when it exists.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn semilla(repo: &Path, args: &[&str]) -> Output {
    let root = workspace_root();
    let mut command = Command::new(env!("CARGO_BIN_EXE_semilla"));
    command.current_dir(repo).args(args);
    command.env("SEMILLA_MIR_DRIVER", root.join("target/debug/semilla-mir-driver"));
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

fn setup() -> tempfile::TempDir {
    // The MIR driver is a separate binary of the workspace.
    let status = Command::new("cargo").current_dir(workspace_root()).args(["build", "-q", "-p", "semilla-mir"]).status().unwrap();
    assert!(status.success(), "build the MIR driver");

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
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
    let repo = dir.path();

    // The index of the base tree: the graph and the MIR content hash of each body.
    let indexed = run(repo, &["index"], 0);
    assert!(indexed.contains("2 crates"), "{indexed}");
    let index: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".semilla/index.json")).unwrap()).unwrap();
    assert!(index["body_hashes"].as_object().unwrap().keys().any(|k| k.contains("Cart::total")), "{}", index["body_hashes"]);

    // A change before the first step would be in the base without a claim, so step begin refuses it.
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let refused = semilla(repo, &["step", "begin", "too late"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("differs from HEAD"));
    write(repo, "crates/shop-app/src/main.rs", MAIN_V0);

    // Step 0: a neutral rename of a local variable. The MIR proves it.
    run(repo, &["step", "begin", "rename s to sum", "--class", "neutral"], 0);
    write(repo, "crates/shop-core/src/cart.rs", CART_V1);
    let anchor = run(repo, &["anchor", "crates/shop-core/src/cart.rs:7"], 0).trim().to_string();
    assert_eq!(anchor, "shop_core::cart::Cart::total/S0");
    run(repo, &["claim", "rename-local", "--at", &anchor, "to=sum"], 0);
    run(repo, &["step", "end"], 0);
    // Phase 1 is fast: the neutral claim is pending. `--full` also runs the body proofs.
    let check = run(repo, &["check"], 0);
    assert!(check.contains("pending"), "{check}");
    let check = run(repo, &["check", "--full"], 0);
    assert!(check.contains("verified (N0"), "{check}");

    // Step 1: an early exit and a changed operator.
    run(repo, &["step", "begin", "return 0 for an empty cart, include the max"], 0);
    write(repo, "crates/shop-core/src/cart.rs", CART_V2);
    run(repo, &["claim", "add-early-exit", "--at", "shop_core::cart::Cart::total/S0", "cond=self.items.is_empty()"], 0);
    // The operator is on line 9 of the "before" file. The current file has it on line 12.
    // Columns start at 1: column 19 is the `<` of `if *x < max`.
    let cond = run(repo, &["anchor", "--before", "crates/shop-core/src/cart.rs:9:19"], 0).trim().to_string();
    assert_eq!(cond, "shop_core::cart::Cart::total/S1/body/S0/cond");
    run(repo, &["claim", "change-operator", "--at", &cond], 0);
    run(repo, &["step", "end"], 0);
    let check = run(repo, &["check"], 0);
    assert!(check.contains("[control_flow]"), "{check}");
    if workspace_root().join(".semilla/tools/z3").exists() {
        assert!(check.contains("effect: widen"), "{check}");
    }

    // Step 2: a change without a claim is rejected, with a hint.
    run(repo, &["step", "begin", "raise the max in the app"], 0);
    write(repo, "crates/shop-app/src/main.rs", MAIN_V1);
    let check = run(repo, &["check"], 1);
    assert!(check.contains("uncovered"), "{check}");
    assert!(check.contains("hint: add change-literal"), "{check}");
    let suggestions = run(repo, &["suggest-claims"], 0);
    assert!(suggestions.contains("change-literal"), "{suggestions}");
    run(repo, &["claim", "change-literal", "--at", "shop_app::main/S1/args[1]/args[0]"], 0);
    let check = run(repo, &["check"], 0);
    assert!(check.contains("✓ c4"), "{check}");
    run(repo, &["step", "end"], 0);

    // Groups, then the final check.
    let rejected = run(repo, &["verify", "--final"], 1);
    assert!(rejected.contains("no groups"), "{rejected}");
    run(repo, &["group", "G1", "--kind", "refactor", "--title", "Rename the sum", "--steps", "0"], 0);
    run(repo, &["group", "G2", "--kind", "fix", "--title", "Fix the cart total", "--steps", "1,2"], 0);
    let suggestion = run(repo, &["subgroups", "G2"], 0);
    assert!(suggestion.contains("\"core\""), "{suggestion}");
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
    assert!(note.contains("add-early-exit"));
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

    mcp_flow(repo);
    ui_flow(repo);
}

/// Talks to `semilla mcp` with JSON-RPC over stdio.
fn mcp_flow(repo: &Path) {
    let root = workspace_root();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
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
    for name in ["step_begin", "claim", "step_end", "anchor", "check_step", "suggest_claims", "group_set", "subgroups_suggest", "subgroups_set", "tag"] {
        assert!(names.contains(&name), "missing tool {name}");
    }
    let anchor = ask(serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "anchor", "arguments": {"file": "crates/shop-core/src/cart.rs", "line": 7}}}));
    assert_eq!(anchor["result"]["content"][0]["text"], "shop_core::cart::Cart::total/S0");
    let bad = ask(serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "claim", "arguments": {"op": "edit-fn-body"}}}));
    assert_eq!(bad["result"]["isError"], true);
    drop(stdin);
    child.wait().unwrap();
}

/// Starts `semilla ui` and fetches the page and the API.
fn ui_flow(repo: &Path) {
    let root = workspace_root();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semilla"))
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
    let repo = dir.path();
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
    run(repo, &["step", "begin", "rename s to sum", "--class", "neutral"], 0);
    write(repo, cart, CART_V1);
    run(repo, &["claim", "rename-local", "--at", "shop_core::cart::Cart::total/S0", "to=sum"], 0);
    let ended = run(repo, &["step", "end"], 0);
    assert!(ended.contains("hunk(s) remain to the target"), "{ended}");

    // Step 1: take the rest of the function from the target by its item path.
    run(repo, &["step", "begin", "return 0 for an empty cart, include the max"], 0);
    let took = run(repo, &["replay", "take", "shop_core::cart::Cart::total"], 0);
    assert!(took.contains("took 2 hunk(s)"), "{took}");
    assert_eq!(std::fs::read_to_string(repo.join(cart)).unwrap(), CART_V2);
    run(repo, &["claim", "add-early-exit", "--at", "shop_core::cart::Cart::total/S0", "cond=self.items.is_empty()"], 0);
    let cond = run(repo, &["anchor", "--before", "crates/shop-core/src/cart.rs:9:19"], 0).trim().to_string();
    run(repo, &["claim", "change-operator", "--at", &cond], 0);
    run(repo, &["check"], 0);
    run(repo, &["step", "end"], 0);

    // The end is refused while hunks remain.
    assert_eq!(semilla(repo, &["replay", "end"]).status.code(), Some(2));

    // Step 2: one hunk by its target line.
    run(repo, &["step", "begin", "a larger max in the app"], 0);
    run(repo, &["replay", "take", &format!("{main}:3")], 0);
    let literal = run(repo, &["anchor", "--before", &format!("{main}:3:31")], 0).trim().to_string();
    run(repo, &["claim", "change-literal", "--at", &literal], 0);
    run(repo, &["check"], 0);
    let ended = run(repo, &["step", "end"], 0);
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
    run(repo, &["step", "begin", "an even larger max"], 0);
    run(repo, &["replay", "take", main], 0);
    let literal = run(repo, &["anchor", "--before", &format!("{main}:3:31")], 0).trim().to_string();
    run(repo, &["claim", "change-literal", "--at", &literal], 0);
    run(repo, &["step", "end"], 0);
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
    let repo = dir.path();
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
    assert_eq!(steps[0]["claims"][0]["op"], "rename", "{}", steps[0]);
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
    assert!(applied.contains("verified (N1: rename-map)"), "{applied}");
    assert!(applied.contains("verified (mir)"), "no-io uses the MIR: {applied}");
    assert!(applied.contains("verified (graph)"), "{applied}");
    assert!(applied.contains("the plan is accepted"), "{applied}");
    assert!(applied.contains("lines: "), "{applied}");
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
    let repo = dir.path();
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
    assert!(has("change-literal", "ledger::is_large/tail/rhs"), "{claims:#?}");
    assert!(has("remove-item", "ledger::legacy_round") && has("add-item", "ledger::average"), "{claims:#?}");
    assert!(intents.iter().any(|i| i.contains("`average`") && i.contains("`legacy_round`")), "the intent names the new function: {intents:?}");
    // The parameter rename is one neutral claim.
    assert!(claims.iter().any(|c| c.starts_with("rename-local ledger::money::format_cents") && c.contains("\"from\":\"c\"")), "{claims:#?}");
    assert!(!has("change-signature", "ledger::money::format_cents"), "{claims:#?}");
    // The raw draft is accepted.
    let out = run(repo, &["replay", "apply"], 0);
    assert!(out.contains("0 uncovered"), "{out}");

    // An unknown field is an error, not a silent loss.
    let mut bad = plan.clone();
    bad["steps"][0]["claims"][0]["note"] = serde_json::json!("x");
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
        "claims": [
            {"op": "remove-item", "at": "ledger::money::fmt_cents"},
            {"op": "add-item", "at": "ledger::money::format_cents"},
            {"op": "change-path", "at": "ledger::report/tail/callee"}
        ]
    });
    let rename_local = intents.iter().position(|i| i.contains("`format_cents`") && !i.starts_with("rename")).unwrap();
    pair["steps"].as_array_mut().unwrap().remove(rename_local);
    std::fs::write(&plan_file, serde_json::to_string_pretty(&pair).unwrap()).unwrap();
    let out = run(repo, &["replay", "apply"], 0);
    assert!(out.contains("! pair: `ledger::money::fmt_cents` (removed)") && out.contains("add it to `refusals`"), "{out}");
    run(repo, &["pair", "refuse", "ledger::money::fmt_cents", "ledger::money::format_cents", "--reason", "a new output contract"], 0);
    let out = run(repo, &["replay", "apply"], 0);
    assert!(out.contains("the agent refused: a new output contract"), "{out}");
    run(repo, &["replay", "abort"], 0);
}
