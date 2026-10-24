//! `penumbra-cli mcp`: a Model Context Protocol server on stdin and stdout, for Claude Code,
//! Claude Desktop and other MCP clients.
//!
//! A thin client of the control channel, like `penumbra-cli ctl`: every tool call becomes one or
//! more control channel requests to the running app, so the app's command registry stays the
//! one place where things are done. The tools are generic (list the commands, run one, run a
//! batch, inspect the project, pictures, the `ui.*` commands), so a new command is reachable
//! without touching this file.
//!
//! The tool names, their annotations and the strict arguments follow the MCP servers of the
//! storytold apps (FilmCraft's `command_list`, `command_run`, `command_batch`, `doc_inspect`,
//! `render_preview` and `ui_*`), so one way of working covers both.
//!
//! JSON-RPC 2.0, one message per line. Both lifecycles are served: `initialize` (protocol
//! revisions 2024-11-05 to 2025-11-25) and, for 2026-07-28 clients, `server/discover` with the
//! protocol version in each request's `_meta`; their results carry `resultType`, and list and
//! read results the cache hints `ttlMs` and `cacheScope`. Requests are answered one after
//! another; a long command (a scan, `projector.match`) holds the next request until it is done.
//! Nothing but protocol messages goes to stdout.

use std::io::{self, BufRead, Write};
use std::time::Duration;

use penumbra_engine::control;
use serde_json::{Map, Value, json};

/// Revisions a client may ask for in `initialize`, oldest first.
const WITH_INITIALIZE: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
/// The first revision without `initialize`: each request names its version in `_meta`.
const INLINE: &str = "2026-07-28";
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
/// How long a client may keep the tool and resource lists: they never change while the server
/// runs.
const LIST_TTL_MS: u64 = 600_000;
/// Longest side of a node's picture unless the call says otherwise.
const PREVIEW_SIDE: u64 = 960;
/// Longest side of a window screenshot unless the call says otherwise: large enough that the
/// panel text of a 4K window stays readable.
const SCREENSHOT_SIDE: u64 = 1568;

const DOCUMENT_URI: &str = "penumbra://document";
const COMMANDS_URI: &str = "penumbra://commands";

const INSTRUCTIONS: &str = "penumbra: node-based live visuals for projection mapping (a TouchDesigner replacement). \
Everything the app does is a command of the running app: `command_list` lists them with a one-line summary that names \
their params ({path, kind, params?} and so on), `command_run` runs one, `command_batch` runs several as one history \
entry. `doc_inspect` shows the project and its nodes (paths like /noise1 or /comp/inner). `render_preview` shows a \
node's picture, `ui_screenshot` the window; the other `ui_*` tools click, drag and type as a user would, by the ids \
`ui_elements` lists (node./noise1, button.undo, ...). `node.kinds {kind}` explains a node type and its params. Edits wait until the user has not \
edited for 3 seconds; every edit can be undone with `history.undo`.";

/// Something that answers control channel requests with the reply envelope
/// (`{"ok": true, "result": ...}` or `{"ok": false, "error": ...}`).
pub trait Channel {
    /// `Err` when the request could not be sent or answered at all.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

/// The running app's control channel.
pub struct Port {
    pub port: u16,
    pub timeout: Duration,
}

impl Channel for Port {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        control::call(self.port, method, params, self.timeout)
    }
}

/// Serve MCP on stdin and stdout until stdin closes.
pub fn run(port: u16, timeout: Duration) -> Result<std::process::ExitCode, String> {
    eprintln!("penumbra-cli mcp: serving MCP on stdin/stdout for the app on port {port}");
    serve(io::stdin().lock(), io::stdout().lock(), &mut Port { port, timeout })
        .map_err(|e| format!("stdio: {e}"))?;
    Ok(std::process::ExitCode::SUCCESS)
}

/// Answer the messages on `input`, one per line, on `out` until `input` ends.
pub fn serve(mut input: impl BufRead, mut out: impl Write, channel: &mut impl Channel) -> io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if input.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        let reply = match std::str::from_utf8(&buf) {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => handle(line, channel),
            Err(_) => Some(rpc_error(Value::Null, -32700, "parse error: the line is not UTF-8")),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
}

/// A JSON-RPC error: code and message.
type RpcError = (i64, String);

fn invalid(message: impl Into<String>) -> RpcError {
    (-32602, message.into())
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The reply to one line, `None` for notifications and for responses from the client.
fn handle(line: &str, channel: &mut impl Channel) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(e) => return Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let Some(message) = message.as_object() else {
        return Some(rpc_error(
            Value::Null,
            -32600,
            "invalid request: expected one JSON-RPC object per line",
        ));
    };
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response to a request of ours (there are none) or junk without a method.
        return id
            .filter(|_| !message.contains_key("result") && !message.contains_key("error"))
            .map(|id| rpc_error(id, -32600, "invalid request: no method"));
    };
    // Notifications (initialized, cancelled, ...) need no answer and change nothing here.
    let id = id?;
    let empty = Value::Object(Map::new());
    let params = message.get("params").unwrap_or(&empty);
    let Some(params) = params.as_object() else {
        return Some(rpc_error(id, -32602, "params must be an object"));
    };
    let version = params.get("_meta").and_then(|m| m.get(VERSION_KEY)).and_then(Value::as_str);
    if let Some(v) = version
        && !supported(v)
    {
        let all = supported_versions().join(", ");
        return Some(rpc_error(id, -32602, &format!("unsupported protocol version {v}; supported: {all}")));
    }
    let inline = version.is_some_and(|v| v >= INLINE);
    match answer(method, params, inline, channel) {
        Ok(mut result) => {
            if inline && let Some(o) = result.as_object_mut() {
                o.entry("resultType").or_insert(json!("complete"));
            }
            Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        }
        Err((code, text)) => Some(rpc_error(id, code, &text)),
    }
}

fn supported(version: &str) -> bool {
    version == INLINE || WITH_INITIALIZE.contains(&version)
}

fn supported_versions() -> Vec<&'static str> {
    WITH_INITIALIZE.iter().copied().chain([INLINE]).collect()
}

fn server_info() -> Value {
    json!({"name": "penumbra", "title": "penumbra", "version": env!("CARGO_PKG_VERSION")})
}

fn capabilities() -> Value {
    json!({"tools": {"listChanged": false}, "resources": {"listChanged": false, "subscribe": false}})
}

/// Adds the cache hints that 2026-07-28 clients require on list and read results.
fn cached(mut result: Value, inline: bool, ttl_ms: u64) -> Value {
    if inline && let Some(o) = result.as_object_mut() {
        o.insert("ttlMs".into(), json!(ttl_ms));
        o.insert("cacheScope".into(), json!("private"));
    }
    result
}

fn answer(
    method: &str,
    params: &Map<String, Value>,
    inline: bool,
    channel: &mut impl Channel,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
            let version = if WITH_INITIALIZE.contains(&asked) { asked } else { "2025-11-25" };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": capabilities(),
                "serverInfo": server_info(),
                "instructions": INSTRUCTIONS,
            }))
        }
        "server/discover" => Ok(json!({
            "supportedVersions": supported_versions(),
            "capabilities": capabilities(),
            "instructions": INSTRUCTIONS,
            "ttlMs": 0,
            "cacheScope": "private",
            "_meta": {"io.modelcontextprotocol/serverInfo": server_info()},
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(cached(json!({"tools": tools()}), inline, LIST_TTL_MS)),
        "tools/call" => call_tool(params, channel),
        "resources/list" => Ok(cached(
            json!({"resources": [
                {"uri": DOCUMENT_URI, "name": "document", "title": "Project",
                 "description": "The project and its nodes (same as doc_inspect).", "mimeType": "application/json"},
                {"uri": COMMANDS_URI, "name": "commands", "title": "Command catalog",
                 "description": "Every command with its summary (same as command_list).", "mimeType": "application/json"},
            ]}),
            inline,
            LIST_TTL_MS,
        )),
        "resources/templates/list" => Ok(cached(json!({"resourceTemplates": []}), inline, LIST_TTL_MS)),
        "resources/read" => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("resources/read needs a uri"))?;
            let content = match uri {
                DOCUMENT_URI => document(channel),
                COMMANDS_URI => ok(channel.call("commands", json!({}))),
                other => {
                    // 2026-07-28 reports a missing resource as invalid params, earlier revisions
                    // with their own code.
                    let code = if inline { -32602 } else { -32002 };
                    return Err((code, format!("resource not found: {other}")));
                }
            }
            .map_err(|e| (-32603, e))?;
            let text = serde_json::to_string_pretty(&content).map_err(|e| (-32603, e.to_string()))?;
            Ok(cached(
                json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": text}]}),
                inline,
                0,
            ))
        }
        "prompts/list" => Ok(cached(json!({"prompts": []}), inline, LIST_TTL_MS)),
        "logging/setLevel" => Ok(json!({})),
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

/// One tool's entry in `tools/list`. Read-only tools are idempotent and never destructive; the
/// others may change the project.
fn tool(
    name: &str,
    title: &str,
    read_only: bool,
    description: &str,
    properties: Value,
    required: &[&str],
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
        "annotations": {
            "title": title,
            "readOnlyHint": read_only,
            "destructiveHint": !read_only,
            "idempotentHint": read_only,
            "openWorldHint": false,
        },
    })
}

fn tools() -> Vec<Value> {
    let max_side = |default: u64| {
        json!({"type": "integer", "minimum": 0,
               "description": format!("longest side of the picture in pixels, 0 for full size (default {default})")})
    };
    let modifiers = json!({
        "ctrl": {"type": "boolean"}, "shift": {"type": "boolean"}, "alt": {"type": "boolean"},
    });
    let with = |mut base: Value, extra: &Value| {
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        base
    };
    let point = json!({"type": "object", "description": "{id} of an element or {x, y} in points",
        "properties": {"id": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}}});
    vec![
        tool(
            "command_list",
            "List commands",
            true,
            "Every command of the running app: id and a one-line summary naming its params. `filter` keeps the ones whose id or summary contains it (case-insensitive).",
            json!({"filter": {"type": "string"}}),
            &[],
        ),
        tool(
            "command_run",
            "Run a command",
            false,
            "Run one command by id with its params, e.g. {\"id\": \"node.create\", \"params\": {\"path\": \"/noise1\", \"kind\": \"tex.noise\"}}. Returns the command's result; a failing command is an error result with the app's message. Edits are undoable (history.undo).",
            json!({"id": {"type": "string"}, "params": {"type": "object"}}),
            &["id"],
        ),
        tool(
            "command_batch",
            "Run several commands",
            false,
            "Run commands in order: {steps: [{id, params?}], stop_on_error?: true, label?}. The batch claims editing (edit.begin) and gives it back (edit.end), so its edits form one history entry named `label`; inside a claim already held it joins that claim. Not all or nothing: steps before a failure stay applied (history.undo removes the whole batch; node.batch is all or nothing). Returns {completed, failed, claim: own | joined, results: [{id, ok, result | error}]}.",
            json!({
                "steps": {"type": "array", "items": {"type": "object",
                    "properties": {"id": {"type": "string"}, "params": {"type": "object"}}, "required": ["id"]}},
                "stop_on_error": {"type": "boolean"},
                "label": {"type": "string"},
            }),
            &["steps"],
        ),
        tool(
            "doc_inspect",
            "Inspect the project",
            true,
            "The open project (directory, unsaved changes, broken nodes, who is editing; as project.info) and every node with its kind (node.list, recursive). node.info {path} gives one node's params and connections.",
            json!({}),
            &[],
        ),
        tool(
            "render_preview",
            "Show a node's picture",
            true,
            "Cook a tex node (or a geo node's picture) and return its output as PNG (node.snapshot). Also returns its size, cook errors and pixel stats.",
            json!({
                "path": {"type": "string", "description": "node path, e.g. /out1"},
                "output": {"type": ["string", "integer"], "description": "output name or index (default: the first)"},
                "max_side": max_side(PREVIEW_SIDE),
            }),
            &["path"],
        ),
        tool(
            "ui_inspect",
            "Inspect the UI",
            true,
            "Selection, viewer, pan and zoom, editing mode, the status bar's message (ui.state).",
            json!({"positions": {"type": "boolean", "description": "also the nodes' positions in the editor"}}),
            &[],
        ),
        tool(
            "ui_elements",
            "List UI elements",
            true,
            "Automation ids of what is on screen, with their rects in points (ui.elements); `prefix` filters by id.",
            json!({"prefix": {"type": "string"}}),
            &[],
        ),
        tool(
            "ui_click",
            "Click",
            false,
            "Click an element by id or a point in points (ui.click).",
            with(
                json!({"id": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"},
                       "button": {"type": "string", "enum": ["left", "right", "middle"]}, "double": {"type": "boolean"}}),
                &modifiers,
            ),
            &[],
        ),
        tool(
            "ui_drag",
            "Drag",
            false,
            "Press, move and release between two elements or points (ui.drag).",
            json!({"from": point, "to": point, "button": {"type": "string", "enum": ["left", "right", "middle"]},
                   "steps": {"type": "integer", "minimum": 1}}),
            &["from", "to"],
        ),
        tool(
            "ui_key",
            "Press a key",
            false,
            "Press a key by its egui name (Delete, Tab, F, Z, Enter, ...), with modifiers (ui.key).",
            with(json!({"key": {"type": "string"}}), &modifiers),
            &["key"],
        ),
        tool(
            "ui_type",
            "Type text",
            false,
            "Type text into the focused field (ui.type).",
            json!({"text": {"type": "string"}}),
            &["text"],
        ),
        tool(
            "ui_screenshot",
            "Screenshot the window",
            true,
            "A PNG of the app's window (ui.screenshot).",
            json!({"max_side": max_side(SCREENSHOT_SIDE)}),
            &[],
        ),
        tool(
            "ui_control",
            "Call a ui command",
            false,
            "Call any ui.* command directly with its params (ui.view, ui.enter, ui.scroll, ui.camera, ui.options, ...).",
            json!({"method": {"type": "string"}, "params": {"type": "object"}}),
            &["method"],
        ),
    ]
}

/// Checks `args` against the tool's schema: no unknown names, the required ones present, each of
/// the right JSON type. A mistake is a JSON-RPC error naming the argument.
fn check_args(tool: &Value, name: &str, args: &Map<String, Value>) -> Result<(), RpcError> {
    let schema = tool.get("inputSchema");
    let empty = Map::new();
    let props = schema.and_then(|s| s.get("properties")).and_then(Value::as_object).unwrap_or(&empty);
    if let Some(bad) = args.keys().find(|k| !props.contains_key(*k)) {
        let accepted: Vec<&str> = props.keys().map(String::as_str).collect();
        let accepted = if accepted.is_empty() { "no arguments".to_string() } else { accepted.join(", ") };
        return Err(invalid(format!("unknown argument \"{bad}\" for {name}; expected: {accepted}")));
    }
    let required = schema.and_then(|s| s.get("required")).and_then(Value::as_array);
    for required in required.into_iter().flatten().filter_map(Value::as_str) {
        if !args.contains_key(required) {
            return Err(invalid(format!("missing argument \"{required}\" for {name}")));
        }
    }
    for (key, value) in args {
        let types: Vec<&str> = match props.get(key).and_then(|p| p.get("type")) {
            Some(Value::String(t)) => vec![t.as_str()],
            Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        let fits = types.iter().any(|t| match *t {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            _ => true,
        });
        if !fits {
            return Err(invalid(format!("argument \"{key}\" for {name} must be {}", types.join(" or "))));
        }
    }
    Ok(())
}

/// The result of a reply envelope: `Err` with the app's message when it says `ok: false`.
fn ok(reply: Result<Value, String>) -> Result<Value, String> {
    let reply = reply?;
    if reply.get("ok") == Some(&Value::Bool(true)) {
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the app answered without a result")
            .to_string())
    }
}

fn text_result(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

fn tool_result(r: Result<Value, String>) -> Value {
    match r {
        Ok(v) => text_result(&v, false),
        Err(e) => text_result(&Value::String(e), true),
    }
}

fn call_tool(params: &Map<String, Value>, channel: &mut impl Channel) -> Result<Value, RpcError> {
    let name =
        params.get("name").and_then(Value::as_str).ok_or_else(|| invalid("tools/call needs a name"))?;
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(a)) => a,
        Some(_) => return Err(invalid("arguments must be an object")),
    };
    let list = tools();
    let tool = list
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| invalid(format!("unknown tool \"{name}\"")))?;
    check_args(tool, name, args)?;
    let params_of = |key: &str| args.get(key).cloned().unwrap_or_else(|| json!({}));
    // Arguments that go to the command unchanged, without the ones this server reads itself.
    let passed = |skip: &[&str]| {
        Value::Object(
            args.iter()
                .filter(|(k, _)| !skip.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    };
    let side = |default: u64| args.get("max_side").and_then(Value::as_u64).unwrap_or(default);
    Ok(match name {
        "command_list" => tool_result(command_list(channel, args.get("filter").and_then(Value::as_str))),
        "command_run" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            tool_result(ok(channel.call(id, params_of("params"))))
        }
        "command_batch" => command_batch(channel, args),
        "doc_inspect" => tool_result(document(channel)),
        "render_preview" => picture(channel, "node.snapshot", passed(&["max_side"]), side(PREVIEW_SIDE)),
        "ui_inspect" => tool_result(ok(channel.call("ui.state", passed(&[])))),
        "ui_elements" => tool_result(ok(channel.call("ui.elements", passed(&[])))),
        "ui_click" => tool_result(ok(channel.call("ui.click", passed(&[])))),
        "ui_drag" => tool_result(ok(channel.call("ui.drag", passed(&[])))),
        "ui_key" => tool_result(ok(channel.call("ui.key", passed(&[])))),
        "ui_type" => tool_result(ok(channel.call("ui.type", passed(&[])))),
        "ui_screenshot" => picture(channel, "ui.screenshot", json!({}), side(SCREENSHOT_SIDE)),
        "ui_control" => {
            let method = args.get("method").and_then(Value::as_str).unwrap_or_default();
            if !method.starts_with("ui.") {
                return Err(invalid(format!(
                    "ui_control calls ui.* commands only, not \"{method}\"; use command_run"
                )));
            }
            tool_result(ok(channel.call(method, params_of("params"))))
        }
        // Every listed tool has an arm above.
        other => return Err(invalid(format!("unknown tool \"{other}\""))),
    })
}

fn command_list(channel: &mut impl Channel, filter: Option<&str>) -> Result<Value, String> {
    let all = ok(channel.call("commands", json!({})))?;
    let filter = filter.unwrap_or_default().to_lowercase();
    let commands: Vec<Value> = all
        .get("commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| {
            filter.is_empty()
                || ["id", "summary"].iter().any(|k| {
                    c.get(*k).and_then(Value::as_str).is_some_and(|s| s.to_lowercase().contains(&filter))
                })
        })
        .cloned()
        .collect();
    Ok(json!({"commands": commands}))
}

fn document(channel: &mut impl Channel) -> Result<Value, String> {
    let project = ok(channel.call("project.info", json!({})))?;
    let nodes = ok(channel.call("node.list", json!({"recursive": true})))?;
    Ok(json!({"project": project, "nodes": nodes}))
}

fn command_batch(channel: &mut impl Channel, args: &Map<String, Value>) -> Value {
    let steps: Vec<(String, Value)> = args
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|s| {
            let id = s.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            (id, s.get("params").cloned().unwrap_or_else(|| json!({})))
        })
        .collect();
    if let Some((id, _)) =
        steps.iter().find(|(id, _)| id.is_empty() || id == "edit.begin" || id == "edit.end")
    {
        let what = if id.is_empty() {
            "every step needs an id".to_string()
        } else {
            format!("{id} cannot be a step")
        };
        return text_result(
            &json!(format!("{what}; the batch claims and releases editing itself. Nothing ran.")),
            true,
        );
    }
    let stop = args.get("stop_on_error").and_then(Value::as_bool).unwrap_or(true);
    // Inside a claim the caller already holds, the steps join it and the claim stays theirs.
    let held = match ok(channel.call("edit.status", json!({}))) {
        Ok(status) => status.get("mode").and_then(Value::as_str) == Some("claude"),
        Err(e) => return text_result(&json!(format!("edit.status: {e}. Nothing ran.")), true),
    };
    if !held {
        let label = match args.get("label").and_then(Value::as_str) {
            Some(l) if !l.is_empty() => l.to_string(),
            _ => format!("batch of {} commands", steps.len()),
        };
        if let Err(e) = ok(channel.call("edit.begin", json!({"label": label}))) {
            return text_result(&json!(format!("could not claim editing: {e}. Nothing ran.")), true);
        }
    }
    let (mut completed, mut failed, mut results) = (0u32, 0u32, Vec::new());
    for (id, params) in steps {
        match ok(channel.call(&id, params)) {
            Ok(result) => {
                completed += 1;
                results.push(json!({"id": id, "ok": true, "result": result}));
            }
            Err(error) => {
                failed += 1;
                results.push(json!({"id": id, "ok": false, "error": error}));
                if stop {
                    break;
                }
            }
        }
    }
    let claim = if held { "joined" } else { "own" };
    let mut report = Map::new();
    report.insert("completed".into(), json!(completed));
    report.insert("failed".into(), json!(failed));
    report.insert("claim".into(), json!(claim));
    report.insert("results".into(), Value::Array(results));
    if !held && let Err(e) = ok(channel.call("edit.end", json!({}))) {
        report.insert("edit_end_error".into(), json!(e));
    }
    text_result(&Value::Object(report), failed > 0)
}

/// A command that writes a PNG and answers with its `file`, as an image result (shrunk to
/// `max_side`) plus the command's reply as text.
fn picture(channel: &mut impl Channel, method: &str, params: Value, max_side: u64) -> Value {
    let reply = match ok(channel.call(method, params)) {
        Ok(r) => r,
        Err(e) => return text_result(&json!(e), true),
    };
    let Some(file) = reply.get("file").and_then(Value::as_str) else {
        return text_result(&json!(format!("{method} answered without a file: {reply}")), true);
    };
    let png = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) => return text_result(&json!(format!("{file}: {e}")), true),
    };
    let side = u32::try_from(max_side).unwrap_or(u32::MAX);
    let png = match penumbra_gpu::image::shrink_png(&png, side) {
        Ok(p) => p,
        Err(e) => return text_result(&json!(format!("{file}: {e}")), true),
    };
    json!({
        "content": [
            {"type": "image", "data": base64(&png), "mimeType": "image/png"},
            {"type": "text", "text": reply.to_string()},
        ],
        "isError": false,
    })
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [0, 1, 2].map(|i| chunk.get(i).copied().unwrap_or(0));
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let six = (n >> (18 - 6 * i)) & 63;
                out.push(ALPHABET.get(six as usize).map_or('A', |&c| char::from(c)));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
