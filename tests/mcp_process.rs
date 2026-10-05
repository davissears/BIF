mod support;

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use serde_json::{Value, json};
use support::OwnedTestDirectory;

/// The pinned MCP schema allows an absent ID, but never a null error-response ID.
/// https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2025-11-25/schema.ts
fn assert_rpc_error(response: &Value, id: Option<&Value>, code: i64) {
    assert!(response.is_object(), "{response}");
    assert_eq!(response["jsonrpc"], "2.0", "{response}");
    if let Some(actual) = response.get("id") {
        assert!(actual.is_string() || actual.is_number(), "{response}");
    }
    assert_eq!(response.get("id"), id, "{response}");
    assert!(response["error"].is_object(), "{response}");
    assert_eq!(response["error"]["code"].as_i64(), Some(code), "{response}");
    assert!(response["error"]["message"].is_string(), "{response}");
    assert!(response.get("result").is_none(), "{response}");
}

struct Server {
    child: Child,
    input: Option<ChildStdin>,
    unread_output: Option<ChildStdout>,
    replies: Receiver<Value>,
    root: OwnedTestDirectory,
}

impl Server {
    fn start() -> Self {
        Self::start_fixture(false)
    }

    fn start_fixture(slow_reads: bool) -> Self {
        Self::start_reader(slow_reads, true)
    }

    fn start_reader(slow_reads: bool, read_output: bool) -> Self {
        let root = OwnedTestDirectory::new();
        let config = root.path().join("config.toml");
        let status = Command::new(env!("CARGO_BIN_EXE_bif"))
            .args(["init", "--root"])
            .arg(root.path())
            .args(["--requester", "DAVIS", "--config"])
            .arg(&config)
            .output()
            .unwrap();
        assert!(status.status.success(), "{:?}", status);
        if slow_reads {
            capture(
                &config,
                "Cancel an active SQLite read",
                "cancellation-fixture",
            );
            let paths = bif::config::resolve_store_root(root.path()).unwrap();
            let fixture = rusqlite::Connection::open(paths.database).unwrap();
            // A stable startup schema with switchable expensive SELECTs gives
            // real active-query interruption evidence. WAL locks don't block
            // reads; changing schema after startup rightly requires a restart.
            fixture
                .execute_batch(
                    "CREATE TABLE fixture_delay(enabled INTEGER);
                 INSERT INTO fixture_delay VALUES(1);
                 ALTER TABLE items RENAME TO fixture_items;
                 CREATE VIEW items AS
                 WITH RECURSIVE slow(n) AS (
                     VALUES(1) UNION ALL SELECT n + 1 FROM slow WHERE n < 100000000
                 )
                 SELECT fixture_items.* FROM fixture_items
                 WHERE NOT EXISTS(SELECT 1 FROM fixture_delay WHERE enabled = 1)
                 OR EXISTS (SELECT 1 FROM slow WHERE n = 100000000);",
                )
                .unwrap();
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_bif-mcp"))
            .arg("--config")
            .arg(config)
            .env_remove("BIF_ROOT")
            .env_remove("BIF_REQUESTER")
            .env_remove("BIF_CONFIG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        let (send, replies) = mpsc::channel();
        let unread_output = if read_output {
            std::thread::spawn(move || {
                for line in BufReader::new(output).lines() {
                    let line = line.unwrap();
                    assert!(line.len() < bif::mcp::MAXIMUM_WIRE_BYTES);
                    let value: Value =
                        serde_json::from_str(&line).expect("stdout is compact JSON only");
                    if send.send(value).is_err() {
                        break;
                    }
                }
            });
            None
        } else {
            Some(output)
        };
        Self {
            child,
            input,
            unread_output,
            replies,
            root,
        }
    }

    fn send(&mut self, value: Value) {
        self.raw(&format!("{value}\n"));
    }

    fn raw(&mut self, text: &str) {
        self.input
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }

    fn receive(&self) -> Value {
        self.replies
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP response timeout")
    }

    fn initialize(&mut self) {
        self.send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"bif-test","version":"1"}}}),
        );
        let response = self.receive();
        assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(response["result"]["capabilities"], json!({"tools":{}}));
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    }

    fn capture(&self, title: &str, key: &str) {
        capture(&self.root.path().join("config.toml"), title, key);
    }
}

fn capture(config: &std::path::Path, title: &str, key: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_bif"))
        .args([
            "capture",
            title,
            "--idempotency-key",
            key,
            "--project",
            "widgets",
            "--config",
        ])
        .arg(config)
        .env_remove("BIF_ROOT")
        .env_remove("BIF_REQUESTER")
        .env_remove("BIF_CONFIG")
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
}

impl Drop for Server {
    fn drop(&mut self) {
        self.input.take();
        for _ in 0..100 {
            if let Some(status) = self.child.try_wait().unwrap() {
                let mut stderr = String::new();
                self.child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .unwrap();
                if !std::thread::panicking() {
                    assert!(status.success(), "{status}: {stderr}");
                    assert!(stderr.is_empty(), "{stderr}");
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !std::thread::panicking() {
            panic!("MCP process did not exit after EOF");
        }
    }
}

#[test]
fn lifecycle_discovery_and_empty_read_are_clean_stdio() {
    let mut server = Server::start();
    server.send(json!({"jsonrpc":"2.0","id":"before","method":"tools/list"}));
    assert_eq!(server.receive()["error"]["code"], -32600);
    server.initialize();
    server.send(json!({"jsonrpc":"2.0","id":"tools","method":"tools/list"}));
    let discovery = server.receive();
    assert_eq!(discovery["result"]["tools"].as_array().unwrap().len(), 4);
    server.send(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"bif_selected_work","arguments":{"project":"widgets"}}}),
    );
    let response = server.receive();
    assert_eq!(response["result"]["isError"], false);
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["result"]["outcome"], "empty");
    let text: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&text, structured);
    server.send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}));
    assert_eq!(server.receive()["result"], json!({}));
}

#[test]
fn malformed_frames_bad_ids_batches_and_unknown_methods_are_protocol_errors() {
    let mut server = Server::start();
    for (line, code) in [
        ("{oops}\n", -32700),
        ("[]\n", -32600),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}\n",
            -32600,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":1.5,\"method\":\"ping\"}\n",
            -32600,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":true,\"method\":\"ping\"}\n",
            -32600,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"ping\"}\n",
            -32600,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":[],\"method\":\"ping\"}\n",
            -32600,
        ),
        ("{\"jsonrpc\":\"2.0\"}\n", -32600),
    ] {
        server.raw(line);
        assert_rpc_error(&server.receive(), None, code);
    }
    server.send(json!({"jsonrpc":"2.0","id":"x".repeat(1_025),"method":"ping"}));
    assert_rpc_error(&server.receive(), None, -32600);
    server.initialize();
    server.send(json!({"jsonrpc":"2.0","id":"unknown","method":"mutate"}));
    assert_rpc_error(&server.receive(), Some(&json!("unknown")), -32601);
    server.send(
        json!({"jsonrpc":"2.0","id":"unknown-tool","method":"tools/call",
        "params":{"name":"bif_capture","arguments":{}}}),
    );
    assert_rpc_error(&server.receive(), Some(&json!("unknown-tool")), -32602);
    server.send(json!({"jsonrpc":"2.0","id":"forged","method":"tools/call",
        "params":{"name":"bif_list","arguments":{"project":"widgets","actor":"evil"}}}));
    assert_rpc_error(&server.receive(), Some(&json!("forged")), -32602);
    server.send(json!({"jsonrpc":"2.0","id":"scope","method":"tools/call",
        "params":{"name":"bif_get","arguments":{"project":"other","item_id":"DAVIS:widgets:001"}}}));
    assert_rpc_error(&server.receive(), Some(&json!("scope")), -32602);
}

#[test]
fn readable_error_ids_are_preserved_for_malformed_envelopes_and_registered_requests() {
    let mut server = Server::start();
    for id in [json!("malformed"), json!(0), json!(-42), json!(u64::MAX)] {
        server.send(json!({"jsonrpc":"invalid","id":id,"method":"ping"}));
        assert_rpc_error(&server.receive(), Some(&id), -32600);
    }
    server.send(json!({"jsonrpc":"2.0","id":"missing-method"}));
    assert_rpc_error(&server.receive(), Some(&json!("missing-method")), -32600);
    server.send(json!({"jsonrpc":"2.0","id":"before-init","method":"tools/list"}));
    assert_rpc_error(&server.receive(), Some(&json!("before-init")), -32600);
    server.initialize();
    server.send(json!({"jsonrpc":"2.0","id":0,"method":"unknown"}));
    assert_rpc_error(&server.receive(), Some(&json!(0)), -32601);
    server.send(json!({"jsonrpc":"2.0","id":-42,"method":"ping","params":[]}));
    assert_rpc_error(&server.receive(), Some(&json!(-42)), -32602);
}

#[test]
fn oversized_input_is_drained_and_next_frame_survives() {
    let mut server = Server::start();
    server.raw(&format!(
        "{}\n",
        "x".repeat(bif::limits::MAXIMUM_REQUEST_BYTES + 1)
    ));
    assert_rpc_error(&server.receive(), None, -32600);
    server.initialize();
    server.send(json!({"jsonrpc":"2.0","id":4,"method":"ping"}));
    assert_eq!(server.receive()["id"], 4);
}

#[test]
fn cancellation_duplicate_ids_and_later_calls_do_not_poison_session() {
    let mut server = Server::start_fixture(true);
    server.initialize();
    let paths = bif::config::resolve_store_root(server.root.path()).unwrap();
    let fixture = rusqlite::Connection::open(paths.database).unwrap();
    let call = |id: &str| {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":"bif_list","arguments":{"project":"widgets"}}})
    };
    server.send(call("active"));
    std::thread::sleep(Duration::from_millis(100));
    server.send(call("queued"));
    server.send(call("active"));
    let duplicate = server.receive();
    assert_rpc_error(&duplicate, Some(&json!("active")), -32600);
    // Cancel the queued call first: otherwise interrupting the active query can
    // let the queued call start before its cancellation frame arrives.
    for id in ["queued", "active"] {
        server.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":id,"reason":"test"}}));
    }
    // Ping is handled by the reader, proving cancellation frames reached control
    // handling independently of the worker's SQLite statement.
    server.send(json!({"jsonrpc":"2.0","id":"barrier","method":"ping"}));
    assert_eq!(server.receive()["id"], "barrier");
    fixture
        .execute_batch("UPDATE fixture_delay SET enabled = 0;")
        .unwrap();
    server.send(call("later"));
    let response = server.receive();
    assert_eq!(response["id"], "later");
    assert_eq!(response["result"]["isError"], false);
    assert!(
        server
            .replies
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    for id in ["active", "queued", "later"] {
        server.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":id}}));
    }
    server.send(call("after-late-cancel"));
    let response = server.receive();
    assert_eq!(response["id"], "after-late-cancel");
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn get_versions_history_and_cursors_use_v2_envelopes() {
    let mut server = Server::start();
    server.initialize();
    server.capture("First item", "first");
    server.capture("Second item", "second");
    let tool = |id: &str, name: &str, args: Value| {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":name,"arguments":args}})
    };
    server.send(tool(
        "get",
        "bif_get",
        json!({"project":"widgets","item_id":"DAVIS:widgets:001"}),
    ));
    let response = server.receive();
    let get = &response["result"]["structuredContent"]["result"];
    assert_eq!(get["outcome"], "modified");
    assert_eq!(get["item"]["title"], "First item");
    let version = get["version"].clone();
    server.send(tool(
        "unchanged",
        "bif_get",
        json!({
        "project":"widgets","item_id":"DAVIS:widgets:001","known_version":version}),
    ));
    let unchanged = server.receive();
    assert_eq!(
        unchanged["result"]["structuredContent"]["result"]["outcome"],
        "not_modified"
    );
    assert!(unchanged["result"]["structuredContent"]["result"]["item"].is_null());
    server.send(tool(
        "page",
        "bif_list",
        json!({"project":"widgets","limit":1}),
    ));
    let page = server.receive();
    let result = &page["result"]["structuredContent"]["result"];
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    let cursor = result["next_cursor"].clone();
    assert!(cursor.is_string());
    server.send(tool(
        "next-page",
        "bif_list",
        json!({"project":"widgets","limit":1,"cursor":cursor}),
    ));
    let next = server.receive();
    assert_ne!(
        result["items"][0]["id"],
        next["result"]["structuredContent"]["result"]["items"][0]["id"]
    );
    server.send(tool(
        "history",
        "bif_history",
        json!({
        "project":"widgets","item_id":"DAVIS:widgets:001","limit":1}),
    ));
    let history = server.receive();
    assert_eq!(history["result"]["isError"], false);
    assert_eq!(
        history["result"]["structuredContent"]["result"]["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    server.send(tool(
        "missing",
        "bif_get",
        json!({"project":"widgets","item_id":"DAVIS:widgets:999"}),
    ));
    let missing = server.receive();
    assert_eq!(missing["result"]["isError"], true);
    assert_eq!(
        missing["result"]["structuredContent"]["error"]["code"],
        "not_found"
    );
    server.send(tool(
        "bad-cursor",
        "bif_list",
        json!({"project":"widgets","cursor":"invalid"}),
    ));
    let invalid = server.receive();
    assert_eq!(invalid["result"]["isError"], true);
    assert_eq!(
        invalid["result"]["structuredContent"]["error"]["code"],
        "invalid_cursor"
    );
    let approved = Command::new(env!("CARGO_BIN_EXE_bif"))
        .args([
            "approve",
            "DAVIS:widgets:001",
            "--expected-revision",
            "1",
            "--idempotency-key",
            "ready-fixture",
            "--config",
        ])
        .arg(server.root.path().join("config.toml"))
        .env_remove("BIF_ROOT")
        .env_remove("BIF_REQUESTER")
        .env_remove("BIF_CONFIG")
        .output()
        .unwrap();
    assert!(approved.status.success(), "{approved:?}");
    server.send(tool(
        "selected",
        "bif_selected_work",
        json!({"project":"widgets"}),
    ));
    let selected = server.receive();
    assert_eq!(
        selected["result"]["structuredContent"]["result"]["outcome"],
        "selected"
    );
    assert_eq!(
        selected["result"]["structuredContent"]["result"]["item"]["id"],
        "DAVIS:widgets:001"
    );
}

#[test]
fn process_config_is_pinned_and_large_complete_records_return_application_errors() {
    let mut server = Server::start();
    server.initialize();
    // Capture supports arbitrarily large immutable titles. MCP's inner budget
    // rejects the complete record instead of truncating fields or writing a
    // partial success frame.
    let title = "\"\\\n".repeat(25_000);
    server.capture(&title, "oversized");
    std::fs::write(
        server.root.path().join("config.toml"),
        "requester = \"OTHER\"\nroot = \"/missing\"\n",
    )
    .unwrap();
    server.send(
        json!({"jsonrpc":"2.0","id":"large","method":"tools/call","params":{
        "name":"bif_get","arguments":{"project":"widgets","item_id":"DAVIS:widgets:001"}}}),
    );
    let response = server.receive();
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        "payload_too_large"
    );
    server.send(
        json!({"jsonrpc":"2.0","id":"still-pinned","method":"tools/call","params":{
        "name":"bif_selected_work","arguments":{"project":"widgets"}}}),
    );
    assert_eq!(server.receive()["result"]["isError"], false);
}

#[test]
fn full_read_queue_reports_overload_and_controls_remain_responsive() {
    let mut server = Server::start_fixture(true);
    server.initialize();
    let call = |id: &str| {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"bif_list","arguments":{"project":"widgets"}}})
    };
    server.send(call("active"));
    std::thread::sleep(Duration::from_millis(100));
    for number in 0..8 {
        server.send(call(&format!("queued-{number}")));
    }
    server.send(call("overload"));
    let overload = server.receive();
    assert_rpc_error(&overload, Some(&json!("overload")), -32000);
    server.send(json!({"jsonrpc":"2.0","id":"control","method":"ping"}));
    assert_eq!(server.receive()["id"], "control");
    // Dropping stdin while SQLite is active must also interrupt and exit.
}

#[test]
fn closed_stdout_ends_process_even_when_stdin_is_still_open() {
    let mut server = Server::start();
    server.initialize();
    let (_, dummy) = mpsc::channel();
    drop(std::mem::replace(&mut server.replies, dummy));
    server.send(json!({"jsonrpc":"2.0","id":"close-reader","method":"ping"}));
    std::thread::sleep(Duration::from_millis(50));
    server.send(json!({"jsonrpc":"2.0","id":"broken-pipe","method":"ping"}));
    for _ in 0..100 {
        if let Some(status) = server.child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("closed stdout left the process waiting on stdin");
}

#[test]
fn duplicate_json_keys_are_rejected_at_every_depth() {
    let mut server = Server::start();
    server.initialize();
    for raw in [
        r#"{"jsonrpc":"2.0","id":"a","id":"b","method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":"a","method":"tools/call","params":{},"params":{}}"#,
        r#"{"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"bif_list","arguments":{"project":"widgets","project":"other"}}}"#,
        r#"{"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"bif_list","arguments":{"project":"widgets"},"_meta":{"host":"local","host":"evil"}}}"#,
    ] {
        server.raw(&format!("{raw}\n"));
        let response = server.receive();
        assert_rpc_error(&response, None, -32600);
    }
}

#[test]
fn version_negotiation_and_ignored_notifications_match_pinned_protocol() {
    let mut server = Server::start();
    server.send(
        json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{
        "protocolVersion":"2099-01-01","capabilities":{},
        "clientInfo":{"name":"test","version":"1"}}}),
    );
    server.send(
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"init"}}),
    );
    assert_eq!(server.receive()["result"]["protocolVersion"], "2025-11-25");
    server.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    for params in [
        json!({"requestId":"unknown"}),
        json!({"requestId":null}),
        json!([]),
        json!({"requestId":1,"reason":42}),
    ] {
        server.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":params}));
    }
    server.send(json!({"jsonrpc":"2.0","id":"ping","method":"ping"}));
    assert_eq!(server.receive()["id"], "ping");
    server.send(
        json!({"jsonrpc":"2.0","id":"task","method":"tools/call","params":{
        "name":"bif_list","arguments":{"project":"widgets"},"task":{"ttl":60000}}}),
    );
    assert_eq!(server.receive()["error"]["code"], -32602);
}

#[test]
fn unread_stdout_overload_exits_without_waiting_for_the_writer_lock() {
    let mut server = Server::start_reader(false, false);
    let mut output = BufReader::new(server.unread_output.take().unwrap());
    server.send(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},
        "clientInfo":{"name":"slow-test","version":"1"}}}),
    );
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 1);
    server.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    server.capture(&"x".repeat(30_000), "slow-output");
    // Keep the stdout pipe open but stop consuming it. Large responses block
    // the writer; neither the reader nor the single database worker may wait
    // for that writer or retain unbounded queued frames.
    // Pace requests so the DB queue is not the bottleneck; generate enough
    // complete responses to fill even platforms with large dynamic pipe buffers.
    for number in 0..100 {
        let call = format!(
            "{}\n",
            json!({
                "jsonrpc":"2.0","id":format!("slow-{number}"),"method":"tools/call","params":{
                "name":"bif_get","arguments":{"project":"widgets","item_id":"DAVIS:widgets:001"}}
            })
        );
        if server
            .input
            .as_mut()
            .unwrap()
            .write_all(call.as_bytes())
            .is_err()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        if server.child.try_wait().unwrap().is_some() {
            break;
        }
    }
    for _ in 0..100 {
        if let Some(status) = server.child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("slow stdout prevented overload shutdown");
}
