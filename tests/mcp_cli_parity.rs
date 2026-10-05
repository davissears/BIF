mod support;

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use support::OwnedTestDirectory;

const FIRST: &str = "DAVIS:alpha:001";
const URGENT: &str = "DAVIS:alpha:002";
const BLOCKED: &str = "DAVIS:alpha:003";
const OTHER: &str = "BOB:alpha:001";
const BETA: &str = "BOB:beta:001";
const RESPONSE_DEADLINE: Duration = Duration::from_secs(10);

/// One production-initialized store and config shared by both actual adapters.
struct Fixture {
    directory: OwnedTestDirectory,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = OwnedTestDirectory::new();
        let root = directory.path().join("root");
        fs::create_dir(&root).unwrap();
        let fixture = Self {
            config: directory.path().join("config.toml"),
            directory,
        };
        fixture.write(&[
            "init",
            "--root",
            root.to_str().unwrap(),
            "--requester",
            "DAVIS",
        ]);

        // Use production capture/triage/lifecycle commands, not fabricated SQL
        // rows: projections and history must contain real persisted content.
        fixture.capture("alpha", "DAVIS", "Needle older assigned work", "first");
        fixture.ready(FIRST, "P1", Some("Davis"), "first-ready");
        fixture.capture("alpha", "DAVIS", "Needle urgent unassigned work", "urgent");
        fixture.ready(URGENT, "P0", None, "urgent-ready");
        fixture.capture("alpha", "BOB", "Needle older another requester", "other");
        fixture.ready(OTHER, "P2", Some("DAVIS"), "other-ready");
        fixture.capture("alpha", "DAVIS", "Needle older blocked work", "blocked");
        fixture.ready(BLOCKED, "P0", Some("Taylor"), "blocked-ready");
        fixture.write(&[
            "start",
            BLOCKED,
            "--expected-revision",
            "2",
            "--idempotency-key",
            "blocked-start",
        ]);
        fixture.write(&[
            "block",
            BLOCKED,
            "Waiting for upstream",
            "--expected-revision",
            "3",
            "--idempotency-key",
            "blocked-block",
        ]);
        // A real second project with an unassigned proposed item, not another
        // store or an empty sentinel project.
        fixture.capture("beta", "BOB", "Needle outside alpha", "beta");
        fixture.write(&[
            "triage",
            BETA,
            "--priority",
            "P0",
            "--note",
            "Not approved for execution",
            "--expected-revision",
            "1",
            "--idempotency-key",
            "beta-priority",
        ]);
        fixture
    }

    /// Every subprocess is insulated from the invoking user's store/config.
    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(binary);
        command
            .current_dir(self.directory.path())
            .env_remove("BIF_ROOT")
            .env_remove("BIF_REQUESTER")
            .env_remove("BIF_CONFIG")
            .env("HOME", self.directory.path().join("home"))
            .env("XDG_CONFIG_HOME", self.directory.path().join("xdg"))
            .env("APPDATA", self.directory.path().join("appdata"));
        command
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_bif"))
            .args(args)
            .arg("--config")
            .arg(&self.config)
            .output()
            .unwrap()
    }

    fn write(&self, args: &[&str]) {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn capture(&self, project: &str, requester: &str, title: &str, key: &str) {
        self.write(&[
            "capture",
            title,
            "--project",
            project,
            "--requester",
            requester,
            "--idempotency-key",
            key,
            "--description",
            "Complete work content, not just a queue title.",
            "--acceptance",
            "Preserve ordered criteria",
            "--acceptance",
            "Verify adapter parity",
            "--source-host",
            "delta",
            "--thread-id",
            "parity-thread",
            "--message-id",
            key,
            "--url",
            "https://example.test/parity",
            "--repository-reference",
            "repo:BIF",
            "--revision-reference",
            "commit:fixture",
            "--context-excerpt",
            "Read-only adapter parity",
        ]);
    }

    fn ready(&self, id: &str, priority: &str, assignee: Option<&str>, key: &str) {
        let mut args = vec![
            "triage",
            id,
            "--action",
            "approve",
            "--priority",
            priority,
            "--note",
            "Ready for read-only selection",
            "--expected-revision",
            "1",
            "--idempotency-key",
            key,
        ];
        if let Some(assignee) = assignee {
            args.extend(["--assignee", assignee]);
        }
        self.write(&args);
    }

    fn read(&self, args: &[&str], exit: i32) -> Value {
        let mut full = vec!["--api-version", "2"];
        full.extend_from_slice(args);
        full.push("--json");
        let output = self.cli(&full);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{full:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout.last(), Some(&b'\n'));
        assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["api_version"], 2);
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["ok"], exit == 0);
        assert_eq!(envelope.as_object().unwrap().len(), 4);
        envelope
    }

    fn parity(
        &self,
        server: &mut Server,
        tool: &str,
        arguments: Value,
        cli_args: &[&str],
        exit: i32,
    ) -> Value {
        let cli = self.read(cli_args, exit);
        let mcp = server.call(tool, arguments.clone());
        assert_eq!(mcp, cli, "{tool} {arguments} vs CLI {cli_args:?}");
        mcp
    }

    /// Current item state and complete fixture histories detect accidental
    /// assignment, claiming, revision changes, or events during selection.
    fn snapshot(&self) -> Value {
        let history = [FIRST, URGENT, BLOCKED, OTHER, BETA]
            .map(|id| self.read(&["history", id, "--limit", "100"], 0));
        json!({
            "alpha": self.read(&["list", "--project", "alpha", "--projection", "audit"], 0),
            "beta": self.read(&["list", "--project", "beta", "--projection", "audit"], 0),
            "history": history
        })
    }
}

/// A bounded stdout reader and RAII child guard, including assertion failures
/// during initialize or a tool call. No protocol test hooks are used.
struct Server {
    child: Child,
    input: Option<ChildStdin>,
    replies: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}

impl Server {
    fn start(fixture: &Fixture) -> Self {
        let mut child = fixture
            .command(env!("CARGO_BIN_EXE_bif-mcp"))
            .arg("--config")
            .arg(&fixture.config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        let (send, replies) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let mut frame = Vec::new();
                let response = match output
                    .by_ref()
                    .take(bif::mcp::MAXIMUM_WIRE_BYTES as u64 + 1)
                    .read_until(b'\n', &mut frame)
                {
                    Ok(0) => break,
                    Ok(_) if frame.len() > bif::mcp::MAXIMUM_WIRE_BYTES => {
                        Err("MCP stdout exceeded the wire limit".into())
                    }
                    Ok(_) if frame.last() != Some(&b'\n') => {
                        Err("MCP stdout ended with an incomplete frame".into())
                    }
                    Ok(_) => serde_json::from_slice(&frame)
                        .map_err(|error| format!("MCP stdout is not JSON: {error}")),
                    Err(error) => Err(format!("MCP stdout read failed: {error}")),
                };
                let failed = response.is_err();
                if send.send(response).is_err() || failed {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            input,
            replies,
            reader: Some(reader),
            next_id: 1,
        };
        server.send(
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},
                "clientInfo":{"name":"mcp-cli-parity","version":"1"}
            }}),
        );
        let initialized = server.receive();
        assert_eq!(initialized["id"], 0);
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        server.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        server
    }

    fn send(&mut self, value: Value) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{value}").unwrap();
        input.flush().unwrap();
    }

    fn receive(&self) -> Value {
        self.replies
            .recv_timeout(RESPONSE_DEADLINE)
            .expect("MCP response missing or deadline exceeded")
            .expect("invalid MCP stdout")
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":tool,"arguments":arguments}}));
        let response = self.receive();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], id);
        assert!(response.get("error").is_none(), "{response}");
        let result = &response["result"];
        let structured = &result["structuredContent"];
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        let text: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(&text, structured, "{tool}: text/structuredContent mismatch");
        assert_eq!(result["isError"], !structured["ok"].as_bool().unwrap());
        structured.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(1);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break None;
                }
            }
        };
        let reader = self.reader.take().unwrap().join();
        if !thread::panicking() {
            assert!(reader.is_ok(), "MCP reader panicked");
            assert!(
                status.is_some_and(|status| status.success()),
                "MCP did not exit successfully after stdin EOF: {status:?}"
            );
        }
    }
}

fn ids(envelope: &Value) -> Vec<&str> {
    envelope["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect()
}

#[test]
fn filtered_projections_mine_and_both_queue_orders_match_cli() {
    let fixture = Fixture::new();
    let mut server = Server::start(&fixture);
    let summary = fixture.parity(
        &mut server,
        "bif_list",
        // Each predicate excludes a different alpha item: requester excludes
        // OTHER, status excludes BLOCKED, and text excludes URGENT.
        json!({"project":"alpha","ordering":"newest_first","requester":"DAVIS",
            "status":"ready","text":"older"}),
        &[
            "list",
            "--project",
            "alpha",
            "--requester",
            "DAVIS",
            "--status",
            "ready",
            "--text",
            "older",
        ],
        0,
    );
    assert_eq!(ids(&summary), [FIRST]);
    assert_eq!(summary["result"]["items"][0].as_object().unwrap().len(), 6);

    let work = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","projection":"work","unassigned":true,
            "priority":"P0","text":"Needle"}),
        &[
            "list",
            "--project",
            "alpha",
            "--projection",
            "work",
            "--unassigned",
            "--priority",
            "P0",
            "--text",
            "Needle",
        ],
        0,
    );
    assert_eq!(ids(&work), [URGENT]);
    assert_eq!(
        work["result"]["items"][0]["acceptance_criteria"],
        json!(["Preserve ordered criteria", "Verify adapter parity"])
    );
    assert_eq!(
        work["result"]["items"][0]["description"],
        "Complete work content, not just a queue title."
    );

    let audit = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","projection":"audit","assignee":"Taylor"}),
        &[
            "list",
            "--project",
            "alpha",
            "--projection",
            "audit",
            "--assignee",
            "Taylor",
        ],
        0,
    );
    assert_eq!(ids(&audit), [BLOCKED]);
    let item = &audit["result"]["items"][0];
    assert_eq!(item["status_reason"], "Waiting for upstream");
    assert_eq!(item["provenance"]["source_host"], "delta");
    assert_eq!(item["provenance"]["message_id"], "blocked");
    assert_eq!(
        item["provenance"]["context_excerpt"],
        "Read-only adapter parity"
    );
    assert!(item.get("history").is_none());

    let mine = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","view":"mine"}),
        &["list", "mine", "--project", "alpha"],
        0,
    );
    // Mine means assigned to the configured identity, not captured by it.
    let mut mine_ids = ids(&mine);
    mine_ids.sort_unstable();
    assert_eq!(mine_ids, [OTHER, FIRST]);
    // Requester BOB narrows mine while ownership remains configured DAVIS;
    // the BOB-captured, davis-assigned item must still match.
    let narrowed = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","view":"mine","requester":"BOB"}),
        &["list", "mine", "--project", "alpha", "--requester", "BOB"],
        0,
    );
    assert_eq!(ids(&narrowed), [OTHER]);

    let newest = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","view":"ready","ordering":"newest_first","projection":"audit"}),
        &[
            "list",
            "ready",
            "--project",
            "alpha",
            "--projection",
            "audit",
        ],
        0,
    );
    // BOB was captured after both ready DAVIS items, and also precedes DAVIS
    // under the identity tie-break when production timestamps share a second.
    assert_eq!(ids(&newest)[0], OTHER);
    assert_eq!(ids(&newest).len(), 3);
    let rows = newest["result"]["items"].as_array().unwrap();
    for pair in rows.windows(2) {
        let key = |row: &Value| {
            (
                row["captured_at"].as_str().unwrap().to_owned(),
                row["requester"].as_str().unwrap().to_owned(),
                row["project"].as_str().unwrap().to_owned(),
                row["sequence"].as_u64().unwrap(),
            )
        };
        let left = key(&pair[0]);
        let right = key(&pair[1]);
        assert!(
            left.0 > right.0
                || (left.0 == right.0 && (left.1, left.2, left.3) < (right.1, right.2, right.3))
        );
    }
    let next = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","view":"ready","ordering":"next","projection":"work"}),
        &["next", "--project", "alpha", "--projection", "work"],
        0,
    );
    assert_eq!(ids(&next), [URGENT, FIRST, OTHER]);
    assert_ne!(ids(&newest), ids(&next));
}

#[test]
fn list_and_history_cursors_continue_in_both_adapters_and_errors_match() {
    let fixture = Fixture::new();
    let mut server = Server::start(&fixture);
    for (tool, arguments, base, collection) in [
        (
            "bif_list",
            json!({"project":"alpha","view":"ready","projection":"work"}),
            vec![
                "list",
                "ready",
                "--project",
                "alpha",
                "--projection",
                "work",
            ],
            "items",
        ),
        (
            "bif_history",
            json!({"project":"alpha","item_id":FIRST}),
            vec!["history", FIRST],
            "events",
        ),
    ] {
        let mut full_args = base.clone();
        full_args.extend(["--limit", "100"]);
        let full = fixture.read(&full_args, 0);
        let expected = full["result"][collection].as_array().unwrap();
        assert!(expected.len() >= 3, "fixture must exercise both handoffs");

        let mut first_args = base.clone();
        first_args.extend(["--limit", "1"]);
        let first_cli = fixture.read(&first_args, 0);
        let mut first_arguments = arguments.clone();
        first_arguments["limit"] = json!(1);
        let first_mcp = server.call(tool, first_arguments);
        assert_eq!(first_mcp, first_cli);
        let mut collected = first_cli["result"][collection].as_array().unwrap().clone();
        let mut cursor = first_cli["result"]["next_cursor"].clone();
        let mut page = 1;
        while let Some(token) = cursor.as_str() {
            assert!(page <= expected.len(), "cursor did not advance");
            // CLI token -> MCP, then MCP token -> CLI. Limits can change
            // without changing the effective cursor query.
            let limit = if page == 1 { "1" } else { "2" };
            let mut cli_args = base.clone();
            cli_args.extend(["--limit", limit, "--cursor", token]);
            let mut mcp_args = arguments.clone();
            mcp_args["limit"] = json!(limit.parse::<u64>().unwrap());
            mcp_args["cursor"] = json!(token);
            let cli_continued = fixture.read(&cli_args, 0);
            let continued = server.call(tool, mcp_args);
            assert_eq!(continued, cli_continued, "{tool}: cursor continuation");
            let rows = continued["result"][collection].as_array().unwrap();
            assert!(!rows.is_empty());
            collected.extend(rows.iter().cloned());
            cursor = if page % 2 == 1 {
                continued["result"]["next_cursor"].clone()
            } else {
                cli_continued["result"]["next_cursor"].clone()
            };
            page += 1;
        }
        assert!(page >= 3, "both token directions must be exercised");
        assert_eq!(
            &collected, expected,
            "{tool}: gaps, repeats, or changed records"
        );
    }

    let invalid = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","cursor":"invalid"}),
        &["list", "--project", "alpha", "--cursor", "invalid"],
        2,
    );
    assert_eq!(invalid["error"]["code"], "invalid_cursor");
    let first = fixture.read(&["list", "--project", "alpha", "--limit", "1"], 0);
    let token = first["result"]["next_cursor"].as_str().unwrap();
    let rebound = fixture.parity(
        &mut server,
        "bif_list",
        json!({"project":"alpha","priority":"P0","cursor":token}),
        &[
            "list",
            "--project",
            "alpha",
            "--priority",
            "P0",
            "--cursor",
            token,
        ],
        2,
    );
    assert_eq!(rebound["error"]["code"], "invalid_cursor");
    for (tool, command) in [("bif_get", "get"), ("bif_history", "history")] {
        let mut args = vec![command, "DAVIS:alpha:999"];
        if command == "get" {
            args.push("--conditional");
        }
        let missing = fixture.parity(
            &mut server,
            tool,
            json!({"project":"alpha","item_id":"DAVIS:alpha:999"}),
            &args,
            3,
        );
        assert_eq!(missing["error"]["code"], "not_found");
    }
}

#[test]
fn conditional_versions_and_selected_work_match_without_claiming_or_mutating() {
    let fixture = Fixture::new();
    let mut server = Server::start(&fixture);
    let initial = fixture.parity(
        &mut server,
        "bif_get",
        json!({"project":"alpha","item_id":FIRST}),
        &["get", FIRST, "--conditional"],
        0,
    );
    assert_eq!(initial["result"]["outcome"], "modified");
    assert_eq!(initial["result"]["item"]["id"], FIRST);
    // The initial CLI token is accepted unchanged by MCP.
    let cli_initial = fixture.read(&["get", FIRST, "--conditional"], 0);
    let summary_version = cli_initial["result"]["version"].as_str().unwrap();
    let hit = fixture.parity(
        &mut server,
        "bif_get",
        json!({"project":"alpha","item_id":FIRST,"known_version":summary_version}),
        &["get", FIRST, "--known-version", summary_version],
        0,
    );
    assert_eq!(hit["result"]["outcome"], "not_modified");
    assert!(hit["result"]["item"].is_null());
    assert_eq!(hit["result"]["version"], summary_version);

    let work = fixture.parity(
        &mut server,
        "bif_get",
        json!({"project":"alpha","item_id":FIRST,"projection":"work",
            "known_version":summary_version}),
        &[
            "get",
            FIRST,
            "--projection",
            "work",
            "--known-version",
            summary_version,
        ],
        0,
    );
    assert_eq!(work["result"]["outcome"], "modified");
    assert_eq!(
        work["result"]["item"]["acceptance_criteria"],
        json!(["Preserve ordered criteria", "Verify adapter parity"])
    );
    assert_ne!(work["result"]["version"], summary_version);
    // The work token obtained from MCP is accepted unchanged by CLI.
    let work_version = work["result"]["version"].as_str().unwrap();
    let work_hit = fixture.parity(
        &mut server,
        "bif_get",
        json!({"project":"alpha","item_id":FIRST,"projection":"work","known_version":work_version}),
        &[
            "get",
            FIRST,
            "--projection",
            "work",
            "--known-version",
            work_version,
        ],
        0,
    );
    assert_eq!(work_hit["result"]["outcome"], "not_modified");
    assert!(work_hit["result"]["item"].is_null());

    fixture.write(&[
        "prioritize",
        FIRST,
        "P3",
        "--expected-revision",
        "2",
        "--idempotency-key",
        "external-priority",
    ]);
    let changed = fixture.parity(
        &mut server,
        "bif_get",
        json!({"project":"alpha","item_id":FIRST,"known_version":summary_version}),
        &["get", FIRST, "--known-version", summary_version],
        0,
    );
    assert_eq!(changed["result"]["outcome"], "modified");
    assert_eq!(changed["result"]["item"]["priority"], "P3");
    assert_ne!(changed["result"]["version"], summary_version);

    let before = fixture.snapshot();
    let next = fixture.read(
        &[
            "next",
            "--project",
            "alpha",
            "--projection",
            "work",
            "--limit",
            "1",
        ],
        0,
    );
    assert_eq!(ids(&next), [URGENT]);
    for _ in 0..2 {
        let selected = fixture.parity(
            &mut server,
            "bif_selected_work",
            json!({"project":"alpha"}),
            &["selected-work", "--project", "alpha"],
            0,
        );
        assert_eq!(selected["result"]["outcome"], "selected");
        assert_eq!(selected["result"]["item"], next["result"]["items"][0]);
        assert_eq!(selected["result"]["item"]["status"], "ready");
        assert!(selected["result"]["item"]["assignee"].is_null());
        let empty = fixture.parity(
            &mut server,
            "bif_selected_work",
            json!({"project":"beta"}),
            &["selected-work", "--project", "beta"],
            0,
        );
        assert_eq!(empty["result"], json!({"outcome":"empty","item":null}));
    }
    assert_eq!(
        fixture.snapshot(),
        before,
        "read adapters mutated state/history"
    );
}
