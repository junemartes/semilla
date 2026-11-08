//! The MCP server: the tag tools for agents, over stdio (JSON-RPC 2.0, one message per line).

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result};
use semilla_core::{Class, GroupKind, Subgroup};
use serde_json::{Value, json};

use crate::app::App;

/// The tools and their input schemas.
fn tools() -> Value {
    let ops: Vec<&str> = semilla_core::ops::OPS.iter().map(|op| op.name).collect();
    let class = json!({"type": "string", "enum": ["neutral", "logic", "control_flow", "other"]});
    json!([
        {"name": "ops", "description": "List all claim operations with their level, class, role, and required arguments.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "step_begin", "description": "Start a step. Make the changes of the step with your own edit tools, then add claims, then call step_end.", "inputSchema": {"type": "object", "properties": {"intent": {"type": "string"}, "class": class}, "required": ["intent"]}},
        {"name": "claim", "description": "Add a claim to the open step. `at` is an anchor from the `anchor` tool. Claims on existing code use the anchors of the tree before the step; `add-*` statement claims use the anchor of the new statement.", "inputSchema": {"type": "object", "properties": {"op": {"type": "string", "enum": ops}, "at": {"type": "string"}, "args": {"type": "object"}}, "required": ["op"]}},
        {"name": "unclaim", "description": "Remove a claim from the open step.", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}},
        {"name": "step_end", "description": "End the open step.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "anchor", "description": "Return the anchor of the lowest node at a file position (the line starts at 1; the column is optional). With `before: true`, the position is in the tree before the open step: use this for claims on existing code.", "inputSchema": {"type": "object", "properties": {"file": {"type": "string"}, "line": {"type": "integer"}, "column": {"type": "integer"}, "before": {"type": "boolean"}}, "required": ["file", "line"]}},
        {"name": "check_step", "description": "Verify one step (default: the last or open step). Returns the report and the change tree, with uncovered changes and hints.", "inputSchema": {"type": "object", "properties": {"step": {"type": "integer"}}}},
        {"name": "suggest_claims", "description": "Suggest claims for the uncovered changes of the last step. The suggestions are not trusted: check and send them with `claim`.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "verify", "description": "Verify the full change set, with the group and subgroup rules.", "inputSchema": {"type": "object", "properties": {"final": {"type": "boolean"}}}},
        {"name": "group_set", "description": "Declare a group: a set of steps with one intent (about one commit).", "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}, "kind": {"type": "string", "enum": ["refactor", "rename", "move", "api", "tests", "deps", "fix", "feature", "behavior"]}, "title": {"type": "string"}, "why": {"type": "string"}, "steps": {"type": "array", "items": {"type": "integer"}}}, "required": ["id", "kind", "title", "steps"]}},
        {"name": "subgroups_suggest", "description": "Return the engine's subgroup suggestion for a group.", "inputSchema": {"type": "object", "properties": {"group": {"type": "string"}}, "required": ["group"]}},
        {"name": "subgroups_set", "description": "Send the final subgroup layout of a group. Returns the rule violations, if any.", "inputSchema": {"type": "object", "properties": {"group": {"type": "string"}, "layout": {"type": "array", "items": {"type": "object"}}}, "required": ["group", "layout"]}},
        {"name": "tag", "description": "Store the change set on a commit (default HEAD) as a git note. The last step must end at the commit tree.", "inputSchema": {"type": "object", "properties": {"commit": {"type": "string"}}}}
    ])
}

/// Runs one tool call.
fn call(app: &mut App, name: &str, args: &Value) -> Result<String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    match name {
        "ops" => Ok(serde_json::to_string_pretty(semilla_core::ops::OPS)?),
        "step_begin" => {
            let class: Option<Class> = args.get("class").map(|c| serde_json::from_value(c.clone())).transpose()?;
            app.step_begin(text("intent").context("`intent` is required")?, class)
        }
        "claim" => {
            let claim_args = args.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
            let id = app.claim(text("op").context("`op` is required")?, text("at"), claim_args)?;
            Ok(format!("claim {id} added"))
        }
        "unclaim" => app.unclaim(text("id").context("`id` is required")?).map(|_| "claim removed".to_string()),
        "step_end" => app.step_end(),
        "anchor" => {
            let line = args.get("line").and_then(Value::as_u64).context("`line` is required")? as usize;
            let column = args.get("column").and_then(Value::as_u64).map(|c| c as usize);
            let before = args.get("before").and_then(Value::as_bool).unwrap_or(false);
            app.anchor(text("file").context("`file` is required")?, line, column, before)
        }
        "check_step" => {
            let session = app.load_session()?;
            let last = session.changeset.steps.len() + usize::from(session.open.is_some());
            let step = args.get("step").and_then(Value::as_u64).map(|s| s as usize).unwrap_or(last.saturating_sub(1));
            let result = app.check(Some(step), false)?;
            Ok(format!("{}\n{}", result.text, result.tree.render()))
        }
        "suggest_claims" => Ok(serde_json::to_string_pretty(&app.suggest_claims()?)?),
        "verify" => {
            let result = app.check(None, args.get("final").and_then(Value::as_bool).unwrap_or(false))?;
            Ok(format!("{}\n{}", result.text, result.tree.render()))
        }
        "group_set" => {
            let kind: GroupKind = serde_json::from_value(args.get("kind").cloned().context("`kind` is required")?)?;
            let steps: Vec<usize> = serde_json::from_value(args.get("steps").cloned().unwrap_or(json!([])))?;
            app.group_set(text("id").context("`id` is required")?, kind, text("title").unwrap_or(""), text("why").unwrap_or(""), steps)?;
            Ok("group saved".into())
        }
        "subgroups_suggest" => Ok(serde_json::to_string_pretty(&app.subgroups_suggest(text("group").context("`group` is required")?)?)?),
        "subgroups_set" => {
            let group = text("group").context("`group` is required")?;
            let mut layout = Vec::new();
            for entry in args.get("layout").and_then(Value::as_array).cloned().unwrap_or_default() {
                let mut entry = entry;
                if let Some(object) = entry.as_object_mut() {
                    object.entry("group").or_insert(json!(group));
                    object.entry("title").or_insert(json!(""));
                }
                let subgroup: Subgroup = serde_json::from_value(entry)?;
                layout.push(subgroup);
            }
            let errors = app.subgroups_set(group, layout)?;
            Ok(if errors.is_empty() { "ok".to_string() } else { format!("rejected:\n{}", errors.join("\n")) })
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
                Err(error) => json!({"content": [{"type": "text", "text": format!("error: {error:#}")}], "isError": true}),
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
