use rusqlite::{Connection, params};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "bif-read-cli-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn bif(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bif"))
        .args(arguments)
        .current_dir(directory)
        .env_remove("BIF_ROOT")
        .env_remove("BIF_REQUESTER")
        .env_remove("BIF_CONFIG")
        .env("HOME", directory)
        .output()
        .unwrap()
}

fn json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture() -> (TestDirectory, PathBuf) {
    let directory = TestDirectory::new();
    let root = directory.0.join("root");
    fs::create_dir(&root).unwrap();
    let config = directory.0.join("config.toml");
    let initialized = bif(
        &directory.0,
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
    assert!(initialized.status.success());
    let connection = Connection::open(root.join(".bif/bif.sqlite")).unwrap();
    connection
        .execute(
            "INSERT INTO projects (project_id, created_at) VALUES ('alpha', '2025-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    for (sequence, status, priority, assignee, captured, title) in [
        (1, "proposed", None, None, "2025-01-01T00:00:01Z", "one"),
        (
            2,
            "ready",
            Some("P2"),
            None,
            "2025-01-01T00:00:02Z",
            "two needle",
        ),
        (
            3,
            "ready",
            Some("P0"),
            Some("davis"),
            "2025-01-01T00:00:03Z",
            "three",
        ),
        (
            4,
            "in_progress",
            None,
            Some("davis"),
            "2025-01-01T00:00:04Z",
            "four",
        ),
        (5, "blocked", None, None, "2025-01-01T00:00:05Z", "five"),
        (6, "done", None, None, "2025-01-01T00:00:06Z", "six"),
        (7, "rejected", None, None, "2025-01-01T00:00:07Z", "seven"),
    ] {
        let id = format!("DAVIS:alpha:{sequence:03}");
        connection
            .execute(
                "INSERT INTO items
                 (item_id, requester, project_id, sequence, title, description, status, priority,
                  assignee, status_reason, revision, captured_at, updated_at)
                 VALUES (?1, 'DAVIS', 'alpha', ?2, ?3, 'description', ?4, ?5, ?6, NULL, 1, ?7, ?7)",
                params![id, sequence, title, status, priority, assignee, captured],
            )
            .unwrap();
        connection
            .execute("INSERT INTO item_provenance (item_id) VALUES (?1)", [&id])
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO operations
             (operation_id, item_id, operation_type, expected_revision, item_revision, occurred_at)
             VALUES ('op-1', 'DAVIS:alpha:001', 'capture', NULL, 1, '2025-01-01T00:00:01Z')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO events
             (event_id, operation_id, item_id, item_revision, event_index, event_type,
              actor_kind, actor_id, actor_surface, actor_host, execution_kind,
              execution_surface, execution_host, occurred_at, event_schema_version)
             VALUES ('event-1', 'op-1', 'DAVIS:alpha:001', 1, 0, 'captured',
              'human', 'DAVIS', 'cli', 'local', 'direct', 'cli', 'local',
              '2025-01-01T00:00:01Z', 1)",
            [],
        )
        .unwrap();
    (directory, config)
}

fn list(directory: &Path, config: &Path, extra: &[&str]) -> Value {
    let mut arguments = vec![
        "list",
        "--project",
        "alpha",
        "--config",
        config.to_str().unwrap(),
        "--json",
    ];
    arguments.extend_from_slice(extra);
    json(bif(directory, &arguments))
}

#[test]
fn get_history_and_every_named_view_have_canonical_machine_output() {
    let (directory, config) = fixture();
    let item = json(bif(
        &directory.0,
        &[
            "get",
            "DAVIS:alpha:001",
            "--config",
            config.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(item["id"], "DAVIS:alpha:001");
    assert_eq!(item["title"], "one");

    let history = json(bif(
        &directory.0,
        &[
            "history",
            "DAVIS:alpha:001",
            "--config",
            config.to_str().unwrap(),
            "--format",
            "json",
        ],
    ));
    assert_eq!(history["events"][0]["event_type"], "captured");

    for (view, count) in [
        ("proposed", 1),
        ("ready", 2),
        ("active", 2),
        ("blocked", 1),
        ("done", 1),
        ("rejected", 1),
        ("mine", 2),
        ("all", 7),
    ] {
        assert_eq!(
            list(&directory.0, &config, &["--view", view])["items"]
                .as_array()
                .unwrap()
                .len(),
            count
        );
    }
}

#[test]
fn filters_next_order_and_pagination_compose_end_to_end() {
    let (directory, config) = fixture();
    let filtered = list(
        &directory.0,
        &config,
        &[
            "--view",
            "ready",
            "--requester",
            "DAVIS",
            "--status",
            "ready",
            "--priority",
            "P2",
            "--unassigned",
            "--text",
            "NEEDLE",
        ],
    );
    assert_eq!(filtered["items"][0]["id"], "DAVIS:alpha:002");

    let first = list(&directory.0, &config, &["--view", "all", "--limit", "2"]);
    assert_eq!(first["items"][0]["id"], "DAVIS:alpha:007");
    assert_eq!(first["next_offset"], 2);
    let second = list(
        &directory.0,
        &config,
        &["--view", "all", "--limit", "2", "--offset", "2"],
    );
    assert_eq!(second["items"][0]["id"], "DAVIS:alpha:005");

    let next = json(bif(
        &directory.0,
        &[
            "next",
            "--project",
            "alpha",
            "--config",
            config.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(next["items"][0]["id"], "DAVIS:alpha:003");
    assert_eq!(next["items"][1]["id"], "DAVIS:alpha:002");
}

#[test]
fn usage_not_found_and_not_initialized_have_stable_exit_codes() {
    let (directory, config) = fixture();
    assert_eq!(
        bif(
            &directory.0,
            &[
                "get",
                "DAVIS:alpha:099",
                "--config",
                config.to_str().unwrap()
            ]
        )
        .status
        .code(),
        Some(3)
    );
    assert_eq!(
        bif(&directory.0, &["list", "--limit", "0"]).status.code(),
        Some(2)
    );
    assert_eq!(
        bif(&directory.0, &["get", "DAVIS:alpha:001"]).status.code(),
        Some(9)
    );
}
