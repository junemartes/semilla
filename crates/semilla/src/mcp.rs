//! The MCP server: the tools of the agent protocol (design 24.3), over stdio (JSON-RPC 2.0, one
//! message per line). The tools of the old protocol are not served: the replay uses a plan.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::app::App;

/// The tools and their input schemas.
fn tools() -> Value {
    json!([
        {"name": "step", "description": "Check and store one step (design 24.3). \"Before\" is the end of the last step (or HEAD for the first step); \"after\" is the working tree. Semilla derives a fact for each change from the syntax tree, with one category. Each step builds the two trees and has their MIR: each fact gets one compare result of the compiled code (`code equal`, `code different`, `no compare`, `no code`). The build is also the build check: a step that does not build is rejected with the errors of the compiler. It gives no verdict for a change. A rejected step stores nothing; the text gives the cause and the call to make again. `same` and `change` are the facts of the agent (very simple, and optional): Semilla checks each fact against the edit, and a false fact rejects the step. A `rewrite` fact is a check of the form of the edit, not of the behavior. The result is one list of entries of the map of the diff (design 90), the real edits first and grouped by item: each entry has its kind (`same code`, `rename`, `move`, `replace`, `add`, `delete`, `type change`, `not mapped`; `syntax only` for a node with no MIR), its source (Semilla, or the fact of the agent `aN`), its check, its category, and its compare result. The sections are `real edits`, `compiled code that changes with no edit` (an entry with no node and no changed line of its own: no real edit), `not mapped`, `transformations`, and `syntax only` (design 92). A real edit that no fact states has a line below it: `not stated (LINES); to state it: STATEMENT` (the line of the gap list of the entry); the step passes. The coverage line is `changed nodes: N in M entries; K real edits (S stated by the agent); J not mapped`, then the request of each node that is not mapped, then the one command that states the first lines of the gap list. A tie of two candidates of a rename or a move gives no pair (design 92). Each `same` and `change` text is a short form of an intent statement (`rename A B` is `#[rename(before::A, after::B)]`, `move A B` is `#[moved(…)]`, `rewrite FROM => TO` is `#[rewrite(\"FROM\", \"TO\")]`, `ITEM=TEXT` is `#[change(ITEM, reason = \"TEXT\")]`), and gives the same entries. `claims` are optional claims (`OP ANCHOR key=value …`); `reasons` maps an anchor to the reason for a fact in the spotlight.", "inputSchema": {"type": "object", "properties": {"release": {"type": "boolean", "description": "Check the `release` profile (design 85). With no argument of the list, a step checks `dev` with the default features, or the list of the setting `configurations` of the manifest."}, "features": {"type": "array", "items": {"type": "string"}, "description": "Check these features, with the default features (as `--features`)."}, "all_features": {"type": "boolean"}, "no_default_features": {"type": "boolean"}, "configurations": {"type": "array", "items": {"type": "string"}, "description": "The printed names of the configurations to check (`dev`, `release, all features`), or `full` for the full list."}, "intent": {"type": "string"}, "group": {"type": "string"}, "claims": {"type": "array", "items": {"type": "string"}}, "same": {"type": "array", "items": {"type": "string"}, "description": "Each entry is an exact change that keeps the form: `rename OLD NEW`, `move FROM TO`, or `rewrite FROM => TO` (a pattern of the syntax tree; `$name` is a variable for one expression)."}, "change": {"type": "array", "items": {"type": "string"}, "description": "Each entry is `ITEM[,ITEM]=TEXT`: the items whose behavior changes on purpose (a loose path of an item, an `impl`, a module, or a file), and one sentence."}, "intent": {"type": "array", "items": {"type": "string"}, "description": "Each entry is the path of an intent file (design 67): a Rust file of claims outside the production code. A false claim rejects the step."}, "tags": {"type": "array", "items": {"type": "string"}, "description": "Each entry is `PLACE[,PLACE]=NAME`: a tag, a name for a part of the step. A place is `FILE:LINE` or `FILE:START-END` (lines of the new tree), `FILE:-START-END` (lines of the old tree, for a deleted text), or `FILE`. A tag has each fact whose edit is fully in its lines. A tag is not a fact: it changes no fact, no gap, and no result."}, "reasons": {"type": "object", "additionalProperties": {"type": "string"}}, "tree": {"type": "boolean"}, "fast": {"type": "boolean", "description": "Run phase 1 now and the compare of the compiled code (phase 2) in the background (for a large project)."}}, "required": ["intent"]}},
        {"name": "fact", "description": "Add facts of the agent to a stored step (the last stored step, or `step`). No build check runs again. A false fact stores nothing. The result has the check of each fact, the entries that each fact states (design 90), and the gap list. Each `same` and `change` text is a short form of an intent statement. With `commit`, the step is a step of the note of a tagged commit, and `reasons` maps the key of a fact in the spotlight to its reason (a reason that belongs to no fact stores nothing).", "inputSchema": {"type": "object", "properties": {"step": {"type": "integer"}, "commit": {"type": "string"}, "reasons": {"type": "object", "additionalProperties": {"type": "string"}}, "same": {"type": "array", "items": {"type": "string"}}, "change": {"type": "array", "items": {"type": "string"}}, "tags": {"type": "array", "items": {"type": "string"}, "description": "Each entry is `PLACE[,PLACE]=NAME`, as for the tool `step`."}}}},
        {"name": "undo", "description": "Remove the last stored step. The working tree keeps its changes.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "tag", "description": "After `git commit`: the final check, the result, and the git note on the commit (default HEAD). It stores nothing when the final check rejects the change set, and it gives each reason with its step. A commit with no change set of the agent gets a tag after the commit (design 62): one step from the first parent to the commit, with the facts of `step`; `commit` can be a range `A..B` (each commit of the first-parent chain). It changes no file and not the session; it writes only the git note. An open item (a spotlight with no reason, a build that fails) does not stop it: the tool `fact` with `commit` and `reasons` adds the reasons later. `again` makes the note of a tag after the commit again.", "inputSchema": {"type": "object", "properties": {"release": {"type": "boolean", "description": "Check the `release` profile (design 85). With no argument of the list, a step checks `dev` with the default features, or the list of the setting `configurations` of the manifest."}, "features": {"type": "array", "items": {"type": "string"}, "description": "Check these features, with the default features (as `--features`)."}, "all_features": {"type": "boolean"}, "no_default_features": {"type": "boolean"}, "configurations": {"type": "array", "items": {"type": "string"}, "description": "The printed names of the configurations to check (`dev`, `release, all features`), or `full` for the full list."}, "commit": {"type": "string", "description": "A commit (default HEAD), or a range `A..B`."}, "again": {"type": "boolean"}}}},
        {"name": "results", "description": "The result of the session, or of a tagged commit: phase 1 runs again now; phase 2 comes only from a check on this machine (else it starts now). The text has one line for each step and the result line. `wait` waits at most this many seconds for phase 2. `verbose` adds the facts.", "inputSchema": {"type": "object", "properties": {"release": {"type": "boolean", "description": "Check the `release` profile (design 85). With no argument of the list, a step checks `dev` with the default features, or the list of the setting `configurations` of the manifest."}, "features": {"type": "array", "items": {"type": "string"}, "description": "Check these features, with the default features (as `--features`)."}, "all_features": {"type": "boolean"}, "no_default_features": {"type": "boolean"}, "configurations": {"type": "array", "items": {"type": "string"}, "description": "The printed names of the configurations to check (`dev`, `release, all features`), or `full` for the full list."}, "commit": {"type": "string"}, "wait": {"type": "integer"}, "verbose": {"type": "boolean"}}}},
        {"name": "ops", "description": "List the optional claims with their arguments and an example.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "verify", "description": "Check the full change set (`final: true`: the final check of `tag`). It stores nothing.", "inputSchema": {"type": "object", "properties": {"release": {"type": "boolean", "description": "Check the `release` profile (design 85). With no argument of the list, a step checks `dev` with the default features, or the list of the setting `configurations` of the manifest."}, "features": {"type": "array", "items": {"type": "string"}, "description": "Check these features, with the default features (as `--features`)."}, "all_features": {"type": "boolean"}, "no_default_features": {"type": "boolean"}, "configurations": {"type": "array", "items": {"type": "string"}, "description": "The printed names of the configurations to check (`dev`, `release, all features`), or `full` for the full list."}, "final": {"type": "boolean"}}}},
        {"name": "pair_refuse", "description": "Refuse a pair warning: say why a removed item and an added item are really two items. The reviewer sees the reason and can still accept the warning. Without `reason`, remove the refusal.", "inputSchema": {"type": "object", "properties": {"old": {"type": "string"}, "new": {"type": "string"}, "reason": {"type": "string"}}, "required": ["old", "new"]}},
        {"name": "replay_begin", "description": "Start a replay: semilla saves your finished work (or `commit`) as the target and puts the base tree in the working tree.", "inputSchema": {"type": "object", "properties": {"commit": {"type": "string"}}}},
        {"name": "replay_status", "description": "List the hunks that remain between the working tree and the replay target.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "replay_plan", "description": "Return a draft plan (JSON) of all replay steps: the intents, the takes, and the optional claims of each step. Check and edit it, then send it with replay_apply.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "replay_apply", "description": "Apply a whole replay plan: start again from the base, run each step, and check the change set. It reports all problems at once. `full: true` also runs phase 2 (the MIR checks).", "inputSchema": {"type": "object", "properties": {"release": {"type": "boolean", "description": "Check the `release` profile (design 85). With no argument of the list, a step checks `dev` with the default features, or the list of the setting `configurations` of the manifest."}, "features": {"type": "array", "items": {"type": "string"}, "description": "Check these features, with the default features (as `--features`)."}, "all_features": {"type": "boolean"}, "no_default_features": {"type": "boolean"}, "configurations": {"type": "array", "items": {"type": "string"}, "description": "The printed names of the configurations to check (`dev`, `release, all features`), or `full` for the full list."}, "plan": {"type": "object"}, "full": {"type": "boolean"}}, "required": ["plan"]}},
        {"name": "replay_end", "description": "End the replay. The working tree must be the target.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "replay_abort", "description": "Stop the replay, remove its steps, and restore the working tree.", "inputSchema": {"type": "object", "properties": {}}}
    ])
}

/// [R83.1-1] The error line of a removed argument of the mode (design 83), or `None`. No tool has
/// an argument with one of these names now (the tools `step`, `tag`, and `replay_apply` had them).
fn removed_argument(args: &Value) -> Option<String> {
    let given = |key: &str| args.get(key).is_some();
    if given("compiled") {
        Some("the argument `compiled` is gone (design 83): Semilla has one mode, and each step builds the two trees and has their MIR. Remove it.".to_string())
    } else if given("no_check") {
        Some("the argument `no_check` is gone (design 83): each step runs the build check, and a step that does not build is not stored. Remove it.".to_string())
    } else {
        None
    }
}

/// Runs one tool call.
fn call(app: &mut App, name: &str, args: &Value) -> Result<String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    // [R83.1-1] A removed argument of the mode is one error line (design 83).
    if let Some(line) = removed_argument(args) {
        anyhow::bail!("{line}");
    }
    // Each tool runs the build code that the change set changes, in the sandbox (design 28).
    // Each proof that depends on it has the mark (I6). The result of a tagged commit and
    // `verify` are reviewer commands: with a limit of the sandbox missing, they do not run it.
    let (mode, guard) = match name {
        "verify" => crate::buildcode::reviewer_mode(crate::buildcode::Mode::Run, false),
        "results" if text("commit").is_some() => crate::buildcode::reviewer_mode(crate::buildcode::Mode::Run, false),
        _ => (crate::buildcode::Mode::Run, None),
    };
    app.mode = mode;
    // [R85.2-4] The list of the build configurations of the arguments (design 85), as for the
    // options of the command line.
    let words = |key: &str| match args.get(key) {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(text)) => vec![text.clone()],
        _ => Vec::new(),
    };
    let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
    let list = crate::configs::Options { release: flag("release"), features: words("features"), all_features: flag("all_features"), no_default_features: flag("no_default_features"), configurations: words("configurations") }.list()?;
    app.new_list = Vec::new();
    crate::configs::set_override(None);
    match (name, list) {
        (_, None) => {}
        ("step" | "tag" | "replay_apply", Some(list)) => app.new_list = list,
        ("results" | "verify", Some(list)) => crate::configs::set_override(Some(list)),
        (_, Some(_)) => anyhow::bail!("{}", crate::configs::form_error(&format!("the tool `{name}` takes no argument of the build configurations"))),
    }
    let guard = guard.map(|line| format!("{line}\n")).unwrap_or_default();
    match name {
        "ops" => {
            let ops: Vec<&semilla_core::ops::OpInfo> = semilla_core::ops::OPS.iter().filter(|op| crate::onestep::CLAIMS.contains(&op.name)).collect();
            Ok(serde_json::to_string_pretty(&ops)?)
        }
        "step" => {
            let strings = |key: &str| args.get(key).and_then(Value::as_array).map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
            let reasons = match args.get("reasons") {
                Some(Value::Object(map)) => map.iter().map(|(anchor, text)| format!("{anchor}={}", text.as_str().unwrap_or(""))).collect(),
                _ => strings("reasons"),
            };
            let request = crate::onestep::StepRequest { intent: text("intent").context("`intent` is required")?.to_string(), group: text("group").map(str::to_string), claims: strings("claims"), same: strings("same"), change: strings("change"), intent_files: app.read_intent_files(&strings("intent").iter().map(std::path::PathBuf::from).collect::<Vec<_>>())?, tags: strings("tags"), reasons, tree: args.get("tree").and_then(Value::as_bool).unwrap_or(false), fast: args.get("fast").and_then(Value::as_bool).unwrap_or(false) , configurations: app.new_list.clone() };
            let (text, stored) = app.step_once(&request)?;
            if !stored {
                anyhow::bail!("{text}");
            }
            Ok(text)
        }
        "fact" => {
            let strings = |key: &str| args.get(key).and_then(Value::as_array).map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
            let step = args.get("step").and_then(Value::as_u64).map(|step| step as usize);
            // Design 62: `reasons` maps a key to its text, as for the tool `step`.
            let reasons: Vec<String> = args.get("reasons").and_then(Value::as_object).map(|map| map.iter().map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or_default())).collect()).unwrap_or_default();
            let (text, stored) = match text("commit") {
                Some(commit) => app.add_to_commit(commit, step, &strings("same"), &strings("change"), &strings("tags"), &reasons)?,
                // R68.11-1: a reason for a stored step of the session too.
                None => app.add_facts(step, &strings("same"), &strings("change"), &strings("tags"), &reasons, &[], false)?,
            };
            if !stored {
                anyhow::bail!("{text}");
            }
            Ok(text)
        }
        "undo" => app.undo(),
        "pair_refuse" => app.pair_refuse(text("old").context("`old` is required")?, text("new").context("`new` is required")?, text("reason")),
        "replay_begin" => app.replay_begin(text("commit")),
        "replay_status" => app.replay_status(),
        "replay_plan" => Ok(serde_json::to_string_pretty(&app.replay_plan()?)?),
        "replay_apply" => {
            let plan: crate::plan::Plan = serde_json::from_value(args.get("plan").cloned().context("`plan` is required")?)?;
            let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
            let applied = app.replay_apply(&plan, flag("full"))?;
            Ok(applied.text)
        }
        "replay_end" => app.replay_end(),
        "replay_abort" => app.replay_abort(),
        "results" => Ok(format!("{guard}{}", app.status(text("commit"), args.get("wait").and_then(Value::as_u64), args.get("verbose").and_then(Value::as_bool).unwrap_or(false))?)),
        "verify" => {
            let (cover, result) = app.verify(args.get("final").and_then(Value::as_bool).unwrap_or(false))?;
            // The line that says what the result covers (batch 5y, A1), then the build code line
            // of the result (design 28.4), then the configurations.
            let head: String = app.build_code_notice(&result.report).into_iter().chain(crate::app::config_lines(&result.report)).map(|line| format!("{line}\n")).collect();
            Ok(format!("{guard}{cover}\n{head}{}\n{}", result.text, result.tree.render()))
        }
        "tag" => {
            let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
            let options = crate::aftertag::TagOptions { again: flag("again") };
            // An open item is not an error of the tool: the text names each one.
            Ok(app.tag_command(text("commit"), &options)?.0)
        }
        other => anyhow::bail!("unknown tool `{other}`"),
    }
}

/// Serves MCP over stdin and stdout until stdin closes.
pub fn serve(dir: &Path) -> Result<()> {
    let mut app = App::open(dir)?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            write_message(&mut stdout, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}))?;
            continue;
        };
        if let Some(response) = handle(&mut app, &message) {
            write_message(&mut stdout, &response)?;
        }
    }
    Ok(())
}

/// Handles one JSON-RPC message. Notifications get no response.
pub fn handle(app: &mut App, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => {
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            Ok(json!({"protocolVersion": version, "capabilities": {"tools": {}}, "serverInfo": {"name": "semilla", "version": env!("CARGO_PKG_VERSION")}}))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(match call(app, name, &args) {
                Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                Err(error) => {
                    let text = match error.downcast_ref::<crate::app::Rejected>() {
                        Some(rejected) => rejected.to_string(),
                        None => format!("error: {error:#}"),
                    };
                    json!({"content": [{"type": "text", "text": text}], "isError": true})
                }
            })
        }
        _ if id.is_none() => return None,
        other => Err(json!({"code": -32601, "message": format!("unknown method `{other}`")})),
    };
    let id = id?;
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

fn write_message(out: &mut impl Write, message: &Value) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string(message)?)?;
    out.flush()?;
    Ok(())
}
