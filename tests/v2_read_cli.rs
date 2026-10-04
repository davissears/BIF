mod support;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
use support::OwnedTestDirectory;

fn process<S: AsRef<std::ffi::OsStr>>(directory: &Path, args: &[S]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bif"));
    configure_command(&mut command, directory);
    command.args(args).output().unwrap()
}

/// Keeps every platform's config root inside the owned test directory.
fn configure_command(command: &mut Command, directory: &Path) {
    command
        .current_dir(directory)
        .env_remove("BIF_ROOT")
        .env_remove("BIF_REQUESTER")
        .env_remove("BIF_CONFIG")
        .env("HOME", directory.join("home"))
        .env("XDG_CONFIG_HOME", directory.join("config"))
        .env("APPDATA", directory.join("appdata"));
}

fn envelope(output: std::process::Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.last(), Some(&b'\n'));
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["api_version"], 2);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], exit == 0);
    assert_eq!(value.as_object().unwrap().len(), 4);
    if exit != 0 {
        assert_eq!(value["error"].as_object().unwrap().len(), 3);
        assert!(!value["error"]["message"].as_str().unwrap().is_empty());
        assert!(value["error"]["details"].is_object());
    }
    value
}

struct Fixture {
    directory: OwnedTestDirectory,
    config: std::path::PathBuf,
    database: std::path::PathBuf,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let directory = OwnedTestDirectory::new();
        let root = directory.path().join("root");
        fs::create_dir(&root).unwrap();
        let config = directory.path().join("config.toml");
        let output = process(
            directory.path(),
            &[
                "init",
                "--root",
                root.to_str().unwrap(),
                "--requester",
                "DAVIS",
                "--config",
                config.to_str().unwrap(),
            ],
        );
        assert!(output.status.success());
        let database = root.join(".bif/bif.sqlite");
        let connection = Connection::open(&database).unwrap();
        connection.execute(
            "INSERT INTO projects (project_id, created_at) VALUES ('alpha', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();
        for sequence in 1..=count {
            let id = format!("DAVIS:alpha:{sequence:03}");
            connection
                .execute(
                    "INSERT INTO items
                 (item_id, requester, project_id, sequence, title, description, status, priority,
                  assignee, revision, captured_at, updated_at)
                 VALUES (?1, 'DAVIS', 'alpha', ?2, 'title', 'description', 'ready', 'P1',
                         'davis', 1, '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
                    params![id, i64::try_from(sequence).unwrap()],
                )
                .unwrap();
            connection
                .execute("INSERT INTO item_provenance (item_id) VALUES (?1)", [&id])
                .unwrap();
        }
        Self {
            directory,
            config,
            database,
        }
    }

    fn run(&self, args: &[&str], exit: i32) -> Value {
        let mut full = vec!["--api-version", "2"];
        full.extend_from_slice(args);
        full.extend(["--config", self.config.to_str().unwrap()]);
        envelope(process(self.directory.path(), &full), exit)
    }

    fn event(&self, index: u64, note: &str) {
        let mut connection = Connection::open(&self.database).unwrap();
        let transaction = connection.transaction().unwrap();
        let revision = i64::try_from(index.checked_add(2).unwrap()).unwrap();
        let operation = format!("operation-{index}");
        assert_eq!(
            transaction
                .execute(
                    "UPDATE items SET revision = ?1
                     WHERE item_id = 'DAVIS:alpha:001' AND revision = ?2",
                    params![revision, revision - 1],
                )
                .unwrap(),
            1
        );
        transaction
            .execute(
                "INSERT INTO operations
             (operation_id, item_id, operation_type, expected_revision, item_revision, occurred_at)
             VALUES (?1, 'DAVIS:alpha:001', 'triage', ?2, ?3, '2025-01-01T00:00:00Z')",
                params![operation, revision - 1, revision],
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO events
             (event_id, operation_id, item_id, item_revision, event_index, event_type,
              actor_kind, actor_id, actor_surface, actor_host, execution_kind,
              execution_surface, execution_host, note, occurred_at, event_schema_version)
             VALUES (?1, ?2, 'DAVIS:alpha:001', ?3, 0, 'note_added', 'human', 'DAVIS',
                     'cli', 'local', 'direct', 'cli', 'local', ?4, '2025-01-01T00:00:00Z', 1)",
                params![format!("event-{index}"), operation, revision, note],
            )
            .unwrap();
        transaction.commit().unwrap();
    }
}

#[test]
fn subprocesses_isolate_poisoned_config_roots_for_uninitialized_and_explicit_requests() {
    let directory = OwnedTestDirectory::new();
    let poison = OwnedTestDirectory::new();
    let home = poison.path().join("home");
    let xdg = poison.path().join("xdg");
    let appdata = poison.path().join("appdata");
    let invalid = "deliberately invalid configuration\n";
    let configs = [
        // An empty HOME fallback distinguishes it from Linux's poisoned XDG config.
        (home.join(".config/bif/config.toml"), ""),
        (
            home.join("Library/Application Support/BIF/config.toml"),
            invalid,
        ),
        (xdg.join("bif/config.toml"), invalid),
        (appdata.join("BIF/config.toml"), invalid),
    ];
    for (config, contents) in &configs {
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(config, contents).unwrap();
    }

    // Seed Command directly, not the process-global environment: safe in parallel tests.
    let poisoned_command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bif"));
        command
            .current_dir(directory.path())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("APPDATA", &appdata)
            .env("BIF_ROOT", poison.path().join("unused-root"))
            .env("BIF_REQUESTER", "POISON")
            .env("BIF_CONFIG", &configs[2].0);
        command
    };
    let args = ["--api-version", "2", "list", "--project", "alpha", "--json"];

    // Prove the disposable poison is selected by the actual platform resolver.
    // On Linux, XDG must win over HOME's .config location.
    let mut control = poisoned_command();
    for key in ["BIF_ROOT", "BIF_REQUESTER", "BIF_CONFIG"] {
        control.env_remove(key);
    }
    let error = envelope(control.args(args).output().unwrap(), 1);
    assert_eq!(error["error"]["code"], "internal");
    if !cfg!(any(target_os = "windows", target_os = "macos")) {
        control.env_remove("XDG_CONFIG_HOME");
        let fallback = envelope(control.output().unwrap(), 9);
        assert_eq!(fallback["error"]["code"], "not_initialized");
    }

    let sandboxed_command = |root: &Path| {
        let mut command = poisoned_command();
        configure_command(&mut command, root);
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        // Check all platform roots even when only one resolver runs on this OS.
        // Assert before executing so a broken helper never reaches host config.
        for (key, relative) in [
            ("XDG_CONFIG_HOME", "config"),
            ("APPDATA", "appdata"),
            ("HOME", "home"),
        ] {
            assert_eq!(
                environment[std::ffi::OsStr::new(key)],
                Some(root.join(relative).as_os_str()),
                "{key}"
            );
        }
        for key in ["BIF_ROOT", "BIF_REQUESTER", "BIF_CONFIG"] {
            assert_eq!(environment[std::ffi::OsStr::new(key)], None, "{key}");
        }
        command
    };
    let uninitialized = envelope(
        sandboxed_command(directory.path())
            .args(args)
            .output()
            .unwrap(),
        9,
    );
    assert_eq!(uninitialized["error"]["code"], "not_initialized");

    let fixture = Fixture::new(1);
    let explicit = envelope(
        sandboxed_command(fixture.directory.path())
            .args(args)
            .arg("--config")
            .arg(&fixture.config)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(
        explicit,
        fixture.run(&["list", "--project", "alpha", "--json"], 0)
    );
    assert_eq!(explicit["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(explicit["result"]["items"][0]["id"], "DAVIS:alpha:001");
    for (config, contents) in &configs {
        assert_eq!(fs::read_to_string(config).unwrap(), *contents);
    }
    assert!(!poison.path().join("unused-root").exists());
}

#[test]
fn every_projection_is_a_complete_replacement_and_defaults_are_bounded() {
    let fixture = Fixture::new(21);
    let contract: Value =
        serde_json::from_str(include_str!("../docs/fixtures/bif-v2-read-contract.json")).unwrap();
    for projection in ["summary", "work", "audit"] {
        let result = fixture.run(
            &[
                "get",
                "DAVIS:alpha:001",
                "--projection",
                projection,
                "--json",
            ],
            0,
        );
        let object = result["result"]["item"].as_object().unwrap();
        let expected = contract["projection_schemas"][projection]["fields"]
            .as_array()
            .unwrap();
        assert_eq!(object.len(), expected.len());
        for field in expected {
            assert!(object.contains_key(field.as_str().unwrap()));
        }
        assert_eq!(object["revision"], 1);
        assert!(!object.contains_key("history"));
    }
    let default = fixture.run(&["get", "DAVIS:alpha:001", "--json"], 0);
    assert_eq!(default["result"]["item"].as_object().unwrap().len(), 6);
    for command in ["list", "next"] {
        let default = fixture.run(&[command, "--project", "alpha", "--json"], 0);
        assert_eq!(default["result"]["items"].as_array().unwrap().len(), 20);
        assert!(default["result"]["next_cursor"].is_string());
    }
}

#[test]
fn list_and_next_resume_every_projection_without_gaps_and_allow_new_limits() {
    let fixture = Fixture::new(5);
    for command in ["list", "next"] {
        for projection in ["summary", "work", "audit"] {
            let first = fixture.run(
                &[
                    command,
                    "--project",
                    "alpha",
                    "--projection",
                    projection,
                    "--limit",
                    "2",
                    "--json",
                ],
                0,
            );
            let cursor = first["result"]["next_cursor"].as_str().unwrap();
            let second = fixture.run(
                &[
                    command,
                    "--project",
                    "alpha",
                    "--projection",
                    projection,
                    "--limit",
                    "3",
                    "--cursor",
                    cursor,
                    "--json",
                ],
                0,
            );
            let ids: Vec<_> = first["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .chain(second["result"]["items"].as_array().unwrap())
                .map(|item| item["id"].clone())
                .collect();
            assert_eq!(
                ids,
                (1..=5)
                    .map(|n| json!(format!("DAVIS:alpha:{n:03}")))
                    .collect::<Vec<_>>()
            );
            assert!(second["result"]["next_cursor"].is_null());
        }
    }
}

#[test]
fn history_pages_are_complete_live_events_and_distinguish_missing_and_empty() {
    let fixture = Fixture::new(2);
    fixture.event(0, "zero");
    fixture.event(1, "one");
    let first = fixture.run(&["history", "DAVIS:alpha:001", "--limit", "1", "--json"], 0);
    assert_eq!(first["result"]["events"][0]["event_index"], 0);
    assert_eq!(first["result"]["events"][0].as_object().unwrap().len(), 13);
    let cursor = first["result"]["next_cursor"].as_str().unwrap();
    fixture.event(2, "appended after first page");
    let second = fixture.run(
        &["history", "DAVIS:alpha:001", "--cursor", cursor, "--json"],
        0,
    );
    assert_eq!(second["result"]["events"].as_array().unwrap().len(), 2);
    assert_eq!(
        second["result"]["events"][1]["note"],
        "appended after first page"
    );
    assert!(second["result"]["next_cursor"].is_null());
    let empty = fixture.run(&["history", "DAVIS:alpha:002", "--json"], 0);
    assert_eq!(
        empty["result"],
        json!({"item_id":"DAVIS:alpha:002","events":[],"next_cursor":null})
    );
    for command in ["get", "history"] {
        assert_eq!(
            fixture.run(&[command, "DAVIS:alpha:999", "--json"], 3)["error"]["code"],
            "not_found"
        );
    }
}

#[test]
fn strict_options_fail_before_config_or_storage_and_frame_errors() {
    let directory = OwnedTestDirectory::new();
    let cases: &[&[&str]] = &[
        &["get", "DAVIS:alpha:001"],
        &["get", "DAVIS:alpha:001", "--limit", "1", "--json"],
        &["get", "DAVIS:alpha:001", "--cursor", "x", "--json"],
        &["get", "DAVIS:alpha:001", "--requester", "OTHER", "--json"],
        &[
            "history",
            "DAVIS:alpha:001",
            "--projection",
            "audit",
            "--json",
        ],
        &["list", "--offset", "0", "--json"],
        &["next", "--offset", "0", "--cursor", "x", "--json"],
        &["history", "DAVIS:alpha:001", "--offset", "0", "--json"],
        &["list", "--limit", "0", "--json"],
        &["list", "--limit", "101", "--json"],
        &["list", "--limit", "-1", "--json"],
        &["list", "--limit", "1", "--limit", "2", "--json"],
        &["list", "--projection", "Work", "--json"],
        &["list", "--priority", "p1", "--json"],
        &["list", "--status", "READY", "--json"],
        &["list", "--unassigned", "--assignee", "davis", "--json"],
        &["get", "DAVIS:alpha:001", "--unassigned", "--json"],
        &["list", "ready", "--view", "ready", "--json"],
        &["next", "--view", "ready", "--json"],
        &["list", "--json", "--json"],
        &["list", "--unknown", "x", "--json"],
        &["list", "--format", "json"],
        &["list", "--json", "--api-version", "2"],
        &["capture", "title", "--json"],
    ];
    for case in cases {
        let mut args = vec!["--api-version", "2"];
        args.extend_from_slice(case);
        assert_eq!(
            envelope(process(directory.path(), &args), 2)["error"]["code"],
            "invalid_input",
            "{case:?}"
        );
    }
    let unsupported = envelope(
        process(directory.path(), &["--api-version", "3", "list", "--json"]),
        8,
    );
    assert_eq!(unsupported["error"]["code"], "unsupported_version");
    let uninitialized = envelope(
        process(
            directory.path(),
            &["--api-version", "2", "list", "--project", "alpha", "--json"],
        ),
        9,
    );
    assert_eq!(uninitialized["error"]["code"], "not_initialized");
}

#[cfg(unix)]
#[test]
fn initial_v2_selector_frames_invalid_utf8_as_invalid_input() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let directory = OwnedTestDirectory::new();
    let args = [
        OsString::from("--api-version"),
        OsString::from("2"),
        OsString::from("list"),
        OsString::from("--text"),
        OsString::from_vec(vec![0xff]),
        OsString::from("--json"),
    ];
    let error = envelope(process(directory.path(), &args), 2);
    assert_eq!(error["error"]["code"], "invalid_input");
}

#[test]
fn misplaced_api_version_without_initial_selector_remains_a_v1_usage_error() {
    let directory = OwnedTestDirectory::new();
    let output = process(directory.path(), &["list", "--api-version", "2", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option --api-version"));
}

#[test]
fn oversized_cursor_is_rejected_before_config_access() {
    let directory = OwnedTestDirectory::new();
    let cursor = "x".repeat(bif::application::MAX_CURSOR_BYTES + 1);
    let missing_config = directory.path().join("missing-config.toml");
    for command in [
        vec!["list", "--project", "alpha"],
        vec!["history", "DAVIS:alpha:001"],
    ] {
        let mut args = vec!["--api-version", "2"];
        args.extend(command);
        args.extend([
            "--cursor",
            &cursor,
            "--json",
            "--config",
            missing_config.to_str().unwrap(),
        ]);
        let error = envelope(process(directory.path(), &args), 2);
        assert_eq!(error["error"]["code"], "invalid_cursor");
        assert_eq!(
            error["error"]["details"],
            json!({"reason":"oversized","restart_required":true}),
        );
    }
    assert!(!missing_config.exists());
}

/// Snapshot bytes and modification metadata without opening SQLite.
fn filesystem_snapshot(
    directory: &Path,
) -> std::collections::BTreeMap<std::path::PathBuf, (bool, u64, std::time::SystemTime, Vec<u8>)> {
    fn visit(
        root: &Path,
        path: &Path,
        snapshot: &mut std::collections::BTreeMap<
            std::path::PathBuf,
            (bool, u64, std::time::SystemTime, Vec<u8>),
        >,
    ) {
        let metadata = fs::metadata(path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(path).unwrap()
        } else {
            Vec::new()
        };
        snapshot.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            (
                metadata.is_dir(),
                metadata.len(),
                metadata.modified().unwrap(),
                bytes,
            ),
        );
        if metadata.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                visit(root, &child.unwrap().path(), snapshot);
            }
        }
    }
    let mut snapshot = std::collections::BTreeMap::new();
    visit(directory, directory, &mut snapshot);
    snapshot
}

fn cursor_token(json: &str) -> String {
    let mut token = String::from("bifc1.");
    for byte in json.bytes() {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

#[test]
fn cursor_structure_and_kind_fail_before_config_or_any_storage_side_effects() {
    let item = json!({
        "cursor_version": 1, "kind": "list", "store_id": "unbound-store",
        "generation": null, "query_fingerprint": "unbound-query",
        "order_version": 1, "projection_schema_version": 1,
        "boundary": {"item": {
            "requester": "DAVIS", "project": "alpha", "sequence": u64::MAX,
            "captured_at": "opaque time", "priority": "P1"
        }}
    });
    let mut history = item.clone();
    history["kind"] = json!("history");
    history["boundary"] = json!({"history": {"item_revision": u64::MAX, "event_index": u64::MAX}});
    let mut cases = vec![("broken".to_owned(), "malformed")];
    for (pointer, value, reason) in [
        ("/cursor_version", json!(2), "unsupported_version"),
        ("/order_version", json!(2), "unsupported_version"),
        (
            "/projection_schema_version",
            json!(2),
            "unsupported_version",
        ),
        ("/generation", json!("reserved"), "unsupported_version"),
        ("/kind", json!({"list": null}), "malformed"),
        ("/store_id", json!(1), "malformed"),
        ("/generation", json!(1), "malformed"),
        ("/boundary/item/requester", json!("davis"), "malformed"),
        ("/boundary/item/project", json!("ALPHA"), "malformed"),
        (
            "/boundary/item/sequence",
            json!("18446744073709551615"),
            "malformed",
        ),
        ("/boundary/item/sequence", json!(0), "malformed"),
        ("/boundary/item/priority", json!({"P1": null}), "malformed"),
        ("/boundary/item/captured_at", json!(3), "malformed"),
        (
            "/boundary/item",
            json!(["DAVIS", "alpha", u64::MAX, "time", "P1"]),
            "malformed",
        ),
    ] {
        let mut value_copy = item.clone();
        *value_copy.pointer_mut(pointer).unwrap() = value;
        cases.push((cursor_token(&value_copy.to_string()), reason));
    }
    cases.push((
        cursor_token(
            &item
                .to_string()
                .replace("18446744073709551615", "18446744073709551616"),
        ),
        "malformed",
    ));
    for (pointer, value, reason) in [
        ("/boundary/history/item_revision", json!(0), "malformed"),
        (
            "/boundary/history/event_index",
            json!("18446744073709551615"),
            "malformed",
        ),
        (
            "/boundary/history",
            json!([u64::MAX, u64::MAX]),
            "malformed",
        ),
    ] {
        let mut value_copy = history.clone();
        *value_copy.pointer_mut(pointer).unwrap() = value;
        cases.push((cursor_token(&value_copy.to_string()), reason));
    }

    // Cover absent default/explicit config, malformed config, and configured
    // empty stores (with and without an existing zero-byte database file).
    for config_state in [
        "configured",
        "zero-byte-db",
        "default",
        "missing",
        "malformed",
    ] {
        let directory = OwnedTestDirectory::new();
        let root = directory.path().join("root");
        let bif = root.join(".bif");
        fs::create_dir_all(&bif).unwrap();
        let config = directory.path().join("config.toml");
        if matches!(config_state, "configured" | "zero-byte-db") {
            fs::write(
                &config,
                format!(
                    "root = {}\nrequester = \"DAVIS\"\n",
                    json!(root.to_str().unwrap())
                ),
            )
            .unwrap();
        } else if config_state == "malformed" {
            fs::write(&config, "not valid config\n").unwrap();
        }
        if config_state == "zero-byte-db" {
            fs::write(bif.join("bif.sqlite"), []).unwrap();
        }
        let before = filesystem_snapshot(directory.path());
        for command in [
            vec!["list", "--project", "alpha"],
            vec!["next", "--project", "alpha"],
            vec!["history", "DAVIS:alpha:001"],
        ] {
            let mut command_cases = cases.clone();
            for valid in [&item, &history] {
                if valid["kind"] != command[0] {
                    command_cases.push((cursor_token(&valid.to_string()), "wrong_kind"));
                }
            }
            for (token, reason) in command_cases {
                let mut args = vec!["--api-version", "2"];
                args.extend_from_slice(&command);
                args.extend(["--cursor", &token, "--json"]);
                if config_state != "default" {
                    args.extend(["--config", config.to_str().unwrap()]);
                }
                let output = process(directory.path(), &args);
                assert!(
                    filesystem_snapshot(directory.path()) == before,
                    "cursor {reason} changed config/storage for {config_state} {command:?}"
                );
                let error = envelope(output, 2);
                assert_eq!(error["error"]["code"], "invalid_cursor");
                assert_eq!(
                    error["error"]["details"],
                    json!({"reason":reason,"restart_required":true}),
                    "{config_state} {command:?}"
                );
            }
        }
    }
}

#[test]
fn full_width_structurally_valid_cursors_need_context_for_store_and_query_binding() {
    use bif::{
        application::{
            CursorContext, HistoryOrdering, HistoryPageRequest, HistoryReadKey, ItemListFilters,
            ItemListOrdering, ItemProjectionKind, ItemProjectionPageRequest, ItemReadKey,
            ReadPageRequest,
        },
        domain::{ItemId, NamedView, RequesterId, Revision, Timestamp},
    };
    let fixture = Fixture::new(1);
    let request = ItemProjectionPageRequest {
        view: NamedView::All,
        configured_requester: RequesterId::new("DAVIS").unwrap(),
        filters: ItemListFilters::default(),
        projection: ItemProjectionKind::Summary,
        ordering: ItemListOrdering::NewestFirst,
        page: ReadPageRequest::new(20, None).unwrap(),
    };
    let item_key = ItemReadKey {
        id: ItemId::new(
            RequesterId::new("DAVIS").unwrap(),
            bif::domain::ProjectId::new("alpha").unwrap(),
            u64::MAX,
        )
        .unwrap(),
        captured_at: Timestamp::new("opaque time"),
        priority: None,
    };
    let history_request = HistoryPageRequest {
        item_id: item_key.id.clone(),
        ordering: HistoryOrdering::RevisionThenEventIndex,
        page: ReadPageRequest::new(20, None).unwrap(),
    };
    let store_id =
        bif::storage::read_store_identity(&Connection::open(&fixture.database).unwrap()).unwrap();
    let tokens = [
        (
            CursorContext::item_page("different-store", &request)
                .unwrap()
                .encode_item_key(&item_key)
                .unwrap(),
            vec!["list", "--project", "alpha"],
            "wrong_store",
        ),
        (
            CursorContext::history("different-store", &history_request)
                .encode_history_key(&HistoryReadKey {
                    item_revision: Revision::new(u64::MAX).unwrap(),
                    event_index: u64::MAX,
                })
                .unwrap(),
            vec!["history", "DAVIS:alpha:001"],
            "wrong_store",
        ),
        (
            CursorContext::item_page(&store_id, &request)
                .unwrap()
                .encode_item_key(&item_key)
                .unwrap(),
            vec!["list", "--project", "alpha"],
            "wrong_query",
        ),
        (
            CursorContext::history(&store_id, &history_request)
                .encode_history_key(&HistoryReadKey {
                    item_revision: Revision::new(u64::MAX).unwrap(),
                    event_index: u64::MAX,
                })
                .unwrap(),
            vec!["history", "DAVIS:alpha:001"],
            "wrong_query",
        ),
    ];
    let missing = OwnedTestDirectory::new();
    let malformed_config = missing.path().join("malformed.toml");
    fs::write(&malformed_config, "invalid configuration\n").unwrap();
    let before = filesystem_snapshot(missing.path());
    for (token, command, reason) in tokens {
        let mut args = vec!["--api-version", "2"];
        args.extend_from_slice(&command);
        args.extend(["--cursor", &token, "--json"]);
        let error = envelope(process(missing.path(), &args), 9);
        assert_eq!(error["error"]["code"], "not_initialized");
        args.extend(["--config", malformed_config.to_str().unwrap()]);
        let error = envelope(process(missing.path(), &args), 1);
        assert_eq!(error["error"]["code"], "internal");
        assert!(filesystem_snapshot(missing.path()) == before);
        let mut args = command;
        args.extend(["--cursor", &token, "--json"]);
        let error = fixture.run(&args, 2);
        assert_eq!(error["error"]["code"], "invalid_cursor");
        assert_eq!(
            error["error"]["details"],
            json!({
                "reason": reason, "restart_required": true
            })
        );
    }
}

#[test]
fn explicit_requester_filter_does_not_override_configured_mine_identity() {
    let fixture = Fixture::new(2);
    let connection = Connection::open(&fixture.database).unwrap();
    for (sequence, assignee) in [(3, "davis"), (4, "other")] {
        let id = format!("OTHER:alpha:{sequence:03}");
        connection
            .execute(
                "INSERT INTO items
             (item_id, requester, project_id, sequence, title, description, status, priority,
              assignee, revision, captured_at, updated_at)
             VALUES (?1, 'OTHER', 'alpha', ?2, 'title', 'description', 'ready', 'P1',
                     ?3, 1, '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
                params![id, sequence, assignee],
            )
            .unwrap();
        connection
            .execute("INSERT INTO item_provenance (item_id) VALUES (?1)", [&id])
            .unwrap();
    }
    let all = fixture.run(
        &[
            "list",
            "--project",
            "alpha",
            "--requester",
            "OTHER",
            "--json",
        ],
        0,
    );
    assert_eq!(all["result"]["items"].as_array().unwrap().len(), 2);
    let mine = fixture.run(
        &[
            "list",
            "mine",
            "--project",
            "alpha",
            "--requester",
            "OTHER",
            "--json",
        ],
        0,
    );
    let ids: Vec<_> = mine["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["OTHER:alpha:003"]);
}

#[test]
fn cursor_binding_reports_stable_restart_guidance_and_uses_configured_mine_identity() {
    let fixture = Fixture::new(3);
    let first = fixture.run(
        &[
            "list",
            "mine",
            "--project",
            "alpha",
            "--limit",
            "1",
            "--json",
        ],
        0,
    );
    let cursor = first["result"]["next_cursor"].as_str().unwrap();
    for (args, reason) in [
        (
            vec![
                "list",
                "mine",
                "--project",
                "alpha",
                "--cursor",
                "broken",
                "--json",
            ],
            "malformed",
        ),
        (
            vec!["next", "--project", "alpha", "--cursor", cursor, "--json"],
            "wrong_kind",
        ),
        (
            vec![
                "list",
                "mine",
                "--project",
                "alpha",
                "--projection",
                "work",
                "--cursor",
                cursor,
                "--json",
            ],
            "wrong_query",
        ),
    ] {
        let error = fixture.run(&args, 2);
        assert_eq!(error["error"]["code"], "invalid_cursor");
        assert_eq!(
            error["error"]["details"],
            json!({"reason":reason,"restart_required":true})
        );
    }
    let config = fs::read_to_string(&fixture.config).unwrap();
    fs::write(&fixture.config, config.replace("DAVIS", "OTHER")).unwrap();
    let error = fixture.run(
        &[
            "list",
            "mine",
            "--project",
            "alpha",
            "--cursor",
            cursor,
            "--json",
        ],
        2,
    );
    assert_eq!(error["error"]["details"]["reason"], "wrong_query");
}

#[test]
fn encoded_request_limit_counts_escaping_before_storage() {
    // The raw argument fits, but its escaped JSON representation exceeds 1 MiB.
    let text = "x\n".repeat(349_525);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = bif::cli::run(
        [
            "--api-version",
            "2",
            "list",
            "--text",
            &text,
            "--cursor",
            "broken",
            "--json",
        ],
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(exit, 2);
    assert_eq!(stdout.last(), Some(&b'\n'));
    let value: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(value["error"]["code"], "invalid_input");
}

#[test]
fn output_writer_failure_returns_internal_exit_using_existing_writer_seam() {
    struct ClosedStdout;

    impl std::io::Write for ClosedStdout {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "stdout closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut stderr = Vec::new();
    // An invalid request avoids config access; its error envelope still needs stdout.
    let exit = bif::cli::run(
        ["--api-version", "2", "list", "--limit", "0", "--json"],
        &mut ClosedStdout,
        &mut stderr,
    );
    assert_eq!(exit, 1);
    assert!(String::from_utf8_lossy(&stderr).contains("could not write output: stdout closed"));
}

#[test]
fn byte_stops_resume_from_last_emitted_item_and_oversized_records_fail_atomically() {
    let fixture = Fixture::new(3);
    let connection = Connection::open(&fixture.database).unwrap();
    connection
        .execute("UPDATE items SET description = ?1", ["x".repeat(600_000)])
        .unwrap();
    for command in ["list", "next"] {
        let first = fixture.run(
            &[
                command,
                "--project",
                "alpha",
                "--projection",
                "work",
                "--json",
            ],
            0,
        );
        assert_eq!(first["result"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(first["result"]["items"][0]["id"], "DAVIS:alpha:001");
        let second = fixture.run(
            &[
                command,
                "--project",
                "alpha",
                "--projection",
                "work",
                "--cursor",
                first["result"]["next_cursor"].as_str().unwrap(),
                "--json",
            ],
            0,
        );
        assert_eq!(second["result"]["items"][0]["id"], "DAVIS:alpha:002");
        assert!(second["result"]["next_cursor"].is_string());
    }
    connection
        .execute(
            "UPDATE items SET description = ?1 WHERE item_id = 'DAVIS:alpha:001'",
            ["x".repeat(1_048_576)],
        )
        .unwrap();
    let error = fixture.run(
        &["get", "DAVIS:alpha:001", "--projection", "work", "--json"],
        11,
    );
    let expected_item = json!({"id":"DAVIS:alpha:001","title":"title","status":"ready","priority":"P1","assignee":"davis","revision":1,"description":"x".repeat(1_048_576),"acceptance_criteria":[],"status_reason":null});
    let minimum = serde_json::to_vec(
        &json!({"api_version":2,"schema_version":1,"ok":true,"result":{"item":expected_item}}),
    )
    .unwrap()
    .len();
    assert_eq!(
        error["error"]["details"],
        json!({"record_kind":"item","record_id":"DAVIS:alpha:001","maximum_response_bytes":1_048_576,"minimum_required_bytes":minimum})
    );
    assert_eq!(
        fixture.run(
            &[
                "list",
                "--project",
                "alpha",
                "--projection",
                "work",
                "--json"
            ],
            11
        )["error"]["code"],
        "payload_too_large"
    );
}

#[test]
fn history_byte_stops_resume_and_oversized_events_do_not_advance_cursor() {
    let fixture = Fixture::new(1);
    fixture.event(0, &"x".repeat(600_000));
    fixture.event(1, &"y".repeat(600_000));
    fixture.event(2, &"z".repeat(1_048_576));
    let first = fixture.run(&["history", "DAVIS:alpha:001", "--json"], 0);
    assert_eq!(first["result"]["events"].as_array().unwrap().len(), 1);
    let second = fixture.run(
        &[
            "history",
            "DAVIS:alpha:001",
            "--cursor",
            first["result"]["next_cursor"].as_str().unwrap(),
            "--json",
        ],
        0,
    );
    assert_eq!(second["result"]["events"][0]["item_revision"], 3);
    assert_eq!(second["result"]["events"][0]["event_index"], 0);
    let cursor = second["result"]["next_cursor"].as_str().unwrap();
    for _ in 0..2 {
        let error = fixture.run(
            &["history", "DAVIS:alpha:001", "--cursor", cursor, "--json"],
            11,
        );
        assert_eq!(error["error"]["code"], "payload_too_large");
        assert_eq!(error["error"]["details"]["record_kind"], "event");
        assert_eq!(error["error"]["details"]["record_id"], "event-2");
    }
}
