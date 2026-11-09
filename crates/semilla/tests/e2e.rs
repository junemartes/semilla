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
