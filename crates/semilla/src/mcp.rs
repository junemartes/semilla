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
        {"name": "step", "description": "Check and store one step (design 24.3). \"Before\" is the end of the last step (or HEAD for the first step); \"after\" is the working tree. Semilla derives a fact for each change, with one category and one compare result of the compiled code (`code equal`, `code different`, `no compare`, `no code`). It does not decide if a change is neutral. A rejected step stores nothing; the text gives the cause and the call to make again. `claims` are optional claims (`OP ANCHOR key=value …`); `reasons` maps an anchor to the reason for a fact in the spotlight.", "inputSchema": {"type": "object", "properties": {"intent": {"type": "string"}, "group": {"type": "string"}, "claims": {"type": "array", "items": {"type": "string"}}, "reasons": {"type": "object", "additionalProperties": {"type": "string"}}, "tree": {"type": "boolean"}, "fast": {"type": "boolean", "description": "Run only phase 1 now and phase 2 in the background (for a large project)."}}, "required": ["intent"]}},
        {"name": "undo", "description": "Remove the last stored step. The working tree keeps its changes.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "tag", "description": "After `git commit`: the final check with phase 2, the result, and the git note on the commit (default HEAD). It stores nothing when the final check rejects the change set, and it gives each reason with its step.", "inputSchema": {"type": "object", "properties": {"commit": {"type": "string"}}}},
        {"name": "results", "description": "The result of the session, or of a tagged commit: phase 1 runs again now; phase 2 comes only from a check on this machine (else it starts now). The text has one line for each step and the result line. `wait` waits at most this many seconds for phase 2. `verbose` adds the facts.", "inputSchema": {"type": "object", "properties": {"commit": {"type": "string"}, "wait": {"type": "integer"}, "verbose": {"type": "boolean"}}}},
        {"name": "ops", "description": "List the optional claims with their arguments and an example.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "verify", "description": "Check the full change set (`final: true`: the final check of `tag`). It stores nothing.", "inputSchema": {"type": "object", "properties": {"final": {"type": "boolean"}}}},
        {"name": "pair_refuse", "description": "Refuse a pair warning: say why a removed item and an added item are really two items. The reviewer sees the reason and can still accept the warning. Without `reason`, remove the refusal.", "inputSchema": {"type": "object", "properties": {"old": {"type": "string"}, "new": {"type": "string"}, "reason": {"type": "string"}}, "required": ["old", "new"]}},
        {"name": "replay_begin", "description": "Start a replay: semilla saves your finished work (or `commit`) as the target and puts the base tree in the working tree.", "inputSchema": {"type": "object", "properties": {"commit": {"type": "string"}}}},
        {"name": "replay_status", "description": "List the hunks that remain between the working tree and the replay target.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "replay_plan", "description": "Return a draft plan (JSON) of all replay steps: the intents, the takes, and the optional claims of each step. Check and edit it, then send it with replay_apply.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "replay_apply", "description": "Apply a whole replay plan: start again from the base, run each step, and check the change set. It reports all problems at once. `full: true` also runs phase 2 (the MIR checks).", "inputSchema": {"type": "object", "properties": {"plan": {"type": "object"}, "full": {"type": "boolean"}}, "required": ["plan"]}},
        {"name": "replay_end", "description": "End the replay. The working tree must be the target.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "replay_abort", "description": "Stop the replay, remove its steps, and restore the working tree.", "inputSchema": {"type": "object", "properties": {}}}
    ])
}

/// Runs one tool call.
fn call(app: &mut App, name: &str, args: &Value) -> Result<String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    // Each tool runs the build code that the change set changes, in the sandbox (design 28).
    // Each proof that depends on it has the mark (I6). The result of a tagged commit and
    // `verify` are reviewer commands: with a limit of the sandbox missing, they do not run it.
    let (mode, guard) = match name {
        "verify" => crate::buildcode::reviewer_mode(crate::buildcode::Mode::Run, false),
        "results" if text("commit").is_some() => crate::buildcode::reviewer_mode(crate::buildcode::Mode::Run, false),
        _ => (crate::buildcode::Mode::Run, None),
    };
    app.mode = mode;
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
            let request = crate::onestep::StepRequest { intent: text("intent").context("`intent` is required")?.to_string(), group: text("group").map(str::to_string), claims: strings("claims"), reasons, tree: args.get("tree").and_then(Value::as_bool).unwrap_or(false), fast: args.get("fast").and_then(Value::as_bool).unwrap_or(false) };
            let (text, stored) = app.step_once(&request)?;
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
            let applied = app.replay_apply(&plan, args.get("full").and_then(Value::as_bool).unwrap_or(false))?;
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
        "tag" => app.tag(text("commit").unwrap_or("HEAD")),
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
