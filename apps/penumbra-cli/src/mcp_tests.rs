use std::sync::Arc;

use super::*;

/// Answers like the app would, and records what it was asked.
#[derive(Default)]
struct Fake {
    calls: Vec<(String, Value)>,
    /// `edit.status`'s mode.
    claude_holds: bool,
    /// `edit.begin` fails as when the user is editing.
    user_editing: bool,
    /// The file `node.snapshot` and `ui.screenshot` answer with.
    picture: Option<std::path::PathBuf>,
}

impl Channel for Fake {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.calls.push((method.to_string(), params.clone()));
        let fail = |e: &str| Ok(json!({"ok": false, "error": e}));
        let result = match method {
            "commands" => json!({"commands": [
                {"id": "node.create", "summary": "Create a node; {path, kind, params?}"},
                {"id": "node.set", "summary": "Set params"},
                {"id": "ping", "summary": "Version"},
            ]}),
            "edit.status" => json!({"mode": if self.claude_holds { "claude" } else { "user" }}),
            "edit.begin" if self.user_editing => {
                return fail("the user is editing (last change 1.0 s ago); retry in 2.0 s");
            }
            "node.snapshot" | "ui.screenshot" => match &self.picture {
                Some(f) => json!({"file": f.display().to_string(), "width": 8, "height": 4}),
                None => return fail("no GPU yet"),
            },
            "bad" => return fail("nope"),
            _ => json!({"echo": params}),
        };
        Ok(json!({"ok": true, "result": result}))
    }
}

fn ask(fake: &mut Fake, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}).to_string();
    let reply = handle(&line, fake).unwrap();
    assert_eq!(reply["id"], 7, "{reply}");
    reply
}

fn call(fake: &mut Fake, name: &str, arguments: Value) -> Value {
    ask(fake, "tools/call", json!({"name": name, "arguments": arguments}))
}

/// The text of a tool result's first text item, parsed when it is JSON.
fn text(reply: &Value) -> Value {
    let content = reply["result"]["content"].as_array().unwrap();
    let t = content.iter().find(|c| c["type"] == "text").unwrap()["text"].as_str().unwrap();
    serde_json::from_str(t).unwrap_or_else(|_| json!(t))
}

fn methods(fake: &Fake) -> Vec<&str> {
    fake.calls.iter().map(|(m, _)| m.as_str()).collect()
}

#[test]
fn initialize_echoes_a_known_version_and_offers_the_newest_otherwise() {
    let mut f = Fake::default();
    for (asked, given) in
        [("2025-06-18", "2025-06-18"), ("2024-11-05", "2024-11-05"), ("1999-01-01", "2025-11-25")]
    {
        let r = ask(
            &mut f,
            "initialize",
            json!({"protocolVersion": asked, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        );
        assert_eq!(r["result"]["protocolVersion"], given);
        assert_eq!(r["result"]["serverInfo"]["name"], "penumbra");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert!(r["result"].get("resultType").is_none());
    }
    assert!(f.calls.is_empty(), "the handshake needs no app");
}

#[test]
fn every_tool_has_a_title_and_the_four_hints() {
    let mut f = Fake::default();
    let r = ask(&mut f, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    assert!(r["result"].get("ttlMs").is_none(), "no cache hints for older revisions");
    for t in tools {
        assert!(t["title"].is_string(), "{t}");
        for hint in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
            assert!(t["annotations"][hint].is_boolean(), "{} lacks {hint}", t["name"]);
        }
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
    }
    let read_only: Vec<&str> = tools
        .iter()
        .filter(|t| t["annotations"]["readOnlyHint"] == true)
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        read_only,
        ["command_list", "doc_inspect", "render_preview", "ui_inspect", "ui_elements", "ui_screenshot"]
    );
}

#[test]
fn arguments_are_strict() {
    let mut f = Fake::default();
    let r = call(&mut f, "command_list", json!({"filtr": "node"}));
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["error"]["message"], "unknown argument \"filtr\" for command_list; expected: filter");
    let r = call(&mut f, "doc_inspect", json!({"x": 1}));
    assert!(r["error"]["message"].as_str().unwrap().ends_with("expected: no arguments"), "{r}");
    let r = call(&mut f, "command_run", json!({"params": {}}));
    assert_eq!(r["error"]["message"], "missing argument \"id\" for command_run");
    let r = call(&mut f, "command_run", json!({"id": 3}));
    assert_eq!(r["error"]["message"], "argument \"id\" for command_run must be string");
    let r = call(&mut f, "render_preview", json!({"path": "/a", "output": 1.5}));
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let r = call(&mut f, "nope", json!({}));
    assert_eq!(r["error"]["code"], -32602);
    assert!(f.calls.is_empty(), "nothing reached the app");
}

#[test]
fn commands_run_with_their_params_unchanged_and_fail_with_the_apps_message() {
    let mut f = Fake::default();
    let params = json!({"path": "/n", "params": {"a": [1, 2]}, "extra": null});
    let r = call(&mut f, "command_run", json!({"id": "node.set", "params": params}));
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(text(&r), json!({"echo": params}));
    let r = call(&mut f, "command_run", json!({"id": "ping"}));
    assert_eq!(text(&r), json!({"echo": {}}), "params default to an empty object");
    let r = call(&mut f, "command_run", json!({"id": "bad"}));
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(text(&r), json!("nope"));
}

#[test]
fn the_command_list_filters_by_id_and_summary() {
    let mut f = Fake::default();
    let ids = |r: &Value| -> Vec<String> {
        text(r)["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids(&call(&mut f, "command_list", json!({}))).len(), 3);
    assert_eq!(ids(&call(&mut f, "command_list", json!({"filter": "NODE."}))), ["node.create", "node.set"]);
    assert_eq!(ids(&call(&mut f, "command_list", json!({"filter": "version"}))), ["ping"]);
}

#[test]
fn a_batch_is_one_claim() {
    let mut f = Fake::default();
    let steps = json!([{"id": "node.create", "params": {"path": "/a"}}, {"id": "node.create", "params": {"path": "/b"}}]);
    let r = call(&mut f, "command_batch", json!({"steps": steps, "label": "two nodes"}));
    assert_eq!(r["result"]["isError"], false);
    let report = text(&r);
    assert_eq!((report["completed"].as_u64(), report["failed"].as_u64()), (Some(2), Some(0)));
    assert_eq!(report["claim"], "own");
    assert_eq!(methods(&f), ["edit.status", "edit.begin", "node.create", "node.create", "edit.end"]);
    assert_eq!(f.calls[1].1, json!({"label": "two nodes"}));

    // A claim the caller holds is joined, not ended.
    let mut f = Fake { claude_holds: true, ..Fake::default() };
    let r = call(&mut f, "command_batch", json!({"steps": [{"id": "ping"}]}));
    assert_eq!(text(&r)["claim"], "joined");
    assert_eq!(methods(&f), ["edit.status", "ping"]);
}

#[test]
fn a_batch_stops_at_a_failure_unless_told_otherwise_and_still_ends_its_claim() {
    let steps = json!([{"id": "ping"}, {"id": "bad"}, {"id": "ping"}]);
    let mut f = Fake::default();
    let r = call(&mut f, "command_batch", json!({"steps": steps}));
    assert_eq!(r["result"]["isError"], true);
    let report = text(&r);
    assert_eq!((report["completed"].as_u64(), report["failed"].as_u64()), (Some(1), Some(1)));
    assert_eq!(report["results"][1], json!({"id": "bad", "ok": false, "error": "nope"}));
    assert_eq!(methods(&f), ["edit.status", "edit.begin", "ping", "bad", "edit.end"]);
    assert_eq!(f.calls[1].1, json!({"label": "batch of 3 commands"}));

    let mut f = Fake::default();
    let r = call(&mut f, "command_batch", json!({"steps": steps, "stop_on_error": false}));
    assert_eq!(text(&r)["completed"], 2);
    assert_eq!(methods(&f).last(), Some(&"edit.end"));
}

#[test]
fn a_batch_runs_nothing_when_it_cannot_claim_or_a_step_would_claim() {
    let mut f = Fake { user_editing: true, ..Fake::default() };
    let r = call(&mut f, "command_batch", json!({"steps": [{"id": "node.create"}]}));
    assert_eq!(r["result"]["isError"], true);
    assert!(text(&r).as_str().unwrap().contains("the user is editing"), "{r}");
    assert_eq!(methods(&f), ["edit.status", "edit.begin"]);

    for step in ["edit.end", "edit.begin", ""] {
        let mut f = Fake::default();
        let r = call(&mut f, "command_batch", json!({"steps": [{"id": "ping"}, {"id": step}]}));
        assert_eq!(r["result"]["isError"], true, "{step}");
        assert!(f.calls.is_empty(), "{step}");
    }
}

#[test]
fn pictures_come_back_inline_and_shrunk() {
    let dir = penumbra_core::scratch::ScratchDir::new("mcp-picture").unwrap();
    let file = dir.path().join("p.png");
    std::fs::write(&file, penumbra_gpu::encode_png_rgba8(8, 4, vec![200; 8 * 4 * 4]).unwrap()).unwrap();
    let mut f = Fake { picture: Some(file), ..Fake::default() };
    let r = call(&mut f, "render_preview", json!({"path": "/out1", "output": 0, "max_side": 4}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let image = &r["result"]["content"][0];
    assert_eq!((&image["type"], &image["mimeType"]), (&json!("image"), &json!("image/png")));
    assert_eq!(text(&r)["width"], 8, "the command's reply comes along");
    assert_eq!(f.calls[0], ("node.snapshot".to_string(), json!({"path": "/out1", "output": 0})));
    // The PNG header holds the size: 4 x 2 after shrinking.
    let png = decode64(image["data"].as_str().unwrap());
    assert_eq!(&png[16..24], &[0, 0, 0, 4, 0, 0, 0, 2]);

    let r = call(&mut f, "ui_screenshot", json!({"max_side": 0}));
    let png = decode64(r["result"]["content"][0]["data"].as_str().unwrap());
    assert_eq!(&png[16..24], &[0, 0, 0, 8, 0, 0, 0, 4], "0 keeps the full size");

    let mut f = Fake::default();
    let r = call(&mut f, "render_preview", json!({"path": "/out1"}));
    assert_eq!((r["result"]["isError"].clone(), text(&r)), (json!(true), json!("no GPU yet")));
}

fn decode64(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let digits: Vec<u8> = text.bytes().filter(|&c| c != b'=').map(value).collect();
    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &d)| n | (u32::from(d) << (18 - 6 * i)));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}

#[test]
fn base64_matches_the_rfc_vectors() {
    for (plain, coded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(plain.as_bytes()), coded);
        assert_eq!(decode64(coded), plain.as_bytes());
    }
}

#[test]
fn ui_tools_pass_their_arguments_and_ui_control_stays_in_ui() {
    let mut f = Fake::default();
    call(&mut f, "ui_click", json!({"id": "editor.node./a", "double": true}));
    call(&mut f, "ui_control", json!({"method": "ui.view", "params": {"path": "/a"}}));
    assert_eq!(f.calls[0], ("ui.click".to_string(), json!({"id": "editor.node./a", "double": true})));
    assert_eq!(f.calls[1], ("ui.view".to_string(), json!({"path": "/a"})));
    let r = call(&mut f, "ui_control", json!({"method": "node.delete", "params": {"path": "/a"}}));
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(f.calls.len(), 2);
}

#[test]
fn junk_lines_are_answered_and_the_server_keeps_serving() {
    let input = "not json\n\n[1, 2]\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
        {\"jsonrpc\":\"2.0\",\"id\":\"a\",\"method\":\"nope\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
    let mut input = input.as_bytes().to_vec();
    input.extend_from_slice(b"\xff\xfe\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}");
    let mut out = Vec::new();
    serve(input.as_slice(), &mut out, &mut Fake::default()).unwrap();
    let replies: Vec<Value> =
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let codes: Vec<Value> = replies.iter().map(|r| r["error"]["code"].clone()).collect();
    assert_eq!(codes, [json!(-32700), json!(-32600), json!(-32601), Value::Null, json!(-32700), Value::Null]);
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(replies[2]["id"], "a");
    assert_eq!((&replies[3]["id"], &replies[3]["result"]), (&json!(1), &json!({})));
    assert_eq!(replies[5]["id"], 2, "a last line without a newline is answered");
}

fn inline_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

#[test]
fn inline_clients_get_result_type_and_cache_hints() {
    let mut f = Fake::default();
    let r = ask(&mut f, "server/discover", json!({"_meta": inline_meta()}));
    assert!(r["result"]["supportedVersions"].as_array().unwrap().contains(&json!("2026-07-28")), "{r}");
    assert_eq!(r["result"]["resultType"], "complete");
    for (method, params) in [
        ("tools/list", json!({"_meta": inline_meta()})),
        ("resources/list", json!({"_meta": inline_meta()})),
        ("resources/read", json!({"_meta": inline_meta(), "uri": COMMANDS_URI})),
    ] {
        let r = ask(&mut f, method, params);
        assert_eq!(r["result"]["resultType"], "complete", "{method}: {r}");
        assert!(r["result"]["ttlMs"].is_u64(), "{method}: {r}");
        assert_eq!(r["result"]["cacheScope"], "private", "{method}: {r}");
    }
    let r = call_inline(&mut f, "command_run", json!({"id": "ping"}));
    assert_eq!(r["result"]["resultType"], "complete");
    // Older clients get the old shape.
    for method in ["tools/list", "resources/list"] {
        let r = ask(&mut f, method, json!({}));
        assert!(
            r["result"].get("resultType").is_none() && r["result"].get("ttlMs").is_none(),
            "{method}: {r}"
        );
    }
    let mut meta = inline_meta();
    meta[VERSION_KEY] = json!("2031-01-01");
    let r = ask(&mut f, "tools/list", json!({"_meta": meta}));
    assert_eq!(r["error"]["code"], -32602);
    // A missing resource: invalid params from 2026-07-28 on, its own code before.
    let r = ask(&mut f, "resources/read", json!({"_meta": inline_meta(), "uri": "penumbra://nope"}));
    assert_eq!(r["error"]["code"], -32602);
    let r = ask(&mut f, "resources/read", json!({"uri": "penumbra://nope"}));
    assert_eq!(r["error"]["code"], -32002);
}

fn call_inline(fake: &mut Fake, name: &str, arguments: Value) -> Value {
    ask(fake, "tools/call", json!({"_meta": inline_meta(), "name": name, "arguments": arguments}))
}

#[test]
fn the_document_resource_is_doc_inspect() {
    let mut f = Fake::default();
    let r = ask(&mut f, "resources/read", json!({"uri": DOCUMENT_URI}));
    let contents = &r["result"]["contents"][0];
    assert_eq!(contents["mimeType"], "application/json");
    let doc: Value = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    assert_eq!(doc["nodes"], json!({"echo": {"recursive": true}}));
    assert_eq!(methods(&f), ["project.info", "node.list"]);
}

#[test]
fn an_app_that_is_not_running_is_an_error_result() {
    // A port nothing listens on: bind one, then let it go.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut channel = Port { port, timeout: Duration::from_secs(2) };
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "command_run", "arguments": {"id": "ping"}}});
    let r = handle(&line.to_string(), &mut channel).unwrap();
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("is penumbra running?"), "{r}");
}

/// The whole path: MCP lines in, the control channel over TCP, a real engine with its registry.
#[test]
fn a_batch_on_a_real_engine_is_one_history_entry() {
    let dir = penumbra_core::scratch::ScratchDir::new("mcp-engine").unwrap();
    let types = Arc::new(penumbra_ops::builtin_types().unwrap());
    let session = Arc::new(
        penumbra_project::Session::open(dir.path(), types, &penumbra_project::Content::default()).unwrap(),
    );
    let engine = penumbra_engine::Engine::start(session).unwrap();
    let registry = penumbra_engine::engine_registry(&engine);
    let addr = control::serve(0, Arc::new(move |m: &str, p: &Value| registry.dispatch(m, p))).unwrap();
    let mut channel = Port { port: addr.port(), timeout: Duration::from_secs(30) };
    let mut tool = |name: &str, arguments: Value| {
        let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}});
        handle(&line.to_string(), &mut channel).unwrap()
    };
    let steps = json!([
        {"id": "node.create", "params": {"path": "/a", "kind": "tex.noise"}},
        {"id": "node.create", "params": {"path": "/b", "kind": "tex.level"}},
        {"id": "node.connect", "params": {"from": "/a", "to": "/b"}},
    ]);
    let r = tool("command_batch", json!({"steps": steps, "label": "noise and level"}));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = tool("command_run", json!({"id": "history.list"}));
    let history = text(&r);
    let entries = history["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{history}");
    assert_eq!(
        (&entries[0]["label"], &entries[0]["nodes"]),
        (&json!("noise and level"), &json!(2)),
        "{history}"
    );
    let r = tool("command_run", json!({"id": "edit.status"}));
    assert_eq!(text(&r)["mode"], "user", "the claim was given back");
    let doc = text(&tool("doc_inspect", json!({})));
    assert!(doc.to_string().contains("/b"), "{doc}");
    let r = tool("command_run", json!({"id": "node.create", "params": {"path": "/a", "kind": "tex.noise"}}));
    assert_eq!(r["result"]["isError"], true, "a second /a is refused: {r}");
    engine.shutdown();
}
