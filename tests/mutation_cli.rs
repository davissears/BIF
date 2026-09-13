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
            "bif-mutation-cli-test-{}-{}",
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
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn initialized() -> (TestDirectory, PathBuf) {
    let directory = TestDirectory::new();
    let root = directory.0.join("root");
    fs::create_dir(&root).unwrap();
    let config = directory.0.join("config.toml");
    let output = bif(
        &directory.0,
        &[
            "init",
            "--root",
            root.to_str().unwrap(),
            "--requester",
            "Delta Agent",
            "--config",
            config.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    (directory, config)
}

fn capture(directory: &Path, config: &Path, title: &str, key: &str) -> String {
    let output = bif(
        directory,
        &[
            "capture",
            title,
            "--project",
            "BIF",
            "--idempotency-key",
            key,
            "--config",
            config.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    text(&output.stdout)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Captured ")
        .unwrap()
        .to_owned()
}

fn mutate(directory: &Path, config: &Path, arguments: &[&str]) -> Output {
    let mut complete = arguments.to_vec();
    complete.extend(["--config", config.to_str().unwrap()]);
    bif(directory, &complete)
}

#[test]
fn compound_triage_is_atomic_and_retry_is_reported() {
    let (directory, config) = initialized();
    let id = capture(&directory.0, &config, "Compound", "capture-compound");
    let arguments = [
        "triage",
        &id,
        "--action",
        "approve",
        "--priority",
        "P1",
        "--assignee",
        "Davis",
        "--note",
        "Ready to execute",
        "--expected-revision",
        "1",
        "--idempotency-key",
        "triage-compound",
    ];
    let first = mutate(&directory.0, &config, &arguments);
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert_eq!(
        text(&first.stdout),
        format!(
            "Updated {id}\nstatus: ready\npriority: P1\nassignee: davis\nrevision: 2\nreplayed: false\n"
        )
    );

    let retry = mutate(&directory.0, &config, &arguments);
    assert!(retry.status.success(), "{}", text(&retry.stderr));
    assert!(text(&retry.stdout).ends_with("revision: 2\nreplayed: true\n"));

    let stale = mutate(
        &directory.0,
        &config,
        &[
            "prioritize",
            &id,
            "P2",
            "--expected-revision",
            "1",
            "--idempotency-key",
            "stale",
        ],
    );
    assert_eq!(stale.status.code(), Some(1));
    assert_eq!(text(&stale.stderr), "error: item revision does not match\n");
}

#[test]
fn convenience_commands_cover_lifecycle_and_explicit_clears() {
    let (directory, config) = initialized();
    let id = capture(&directory.0, &config, "Lifecycle", "capture-lifecycle");
    let steps = [
        ("approve", None, "1", "approve", "ready"),
        ("assign", Some("Taylor"), "2", "assign", "ready"),
        ("prioritize", Some("P0"), "3", "priority", "ready"),
        ("start", None, "4", "start", "in_progress"),
        ("block", Some("Waiting"), "5", "block", "blocked"),
        ("resume", None, "6", "resume", "in_progress"),
        ("finish", None, "7", "finish", "done"),
    ];
    for (command, value, revision, key, status) in steps {
        let mut arguments = vec![command, &id];
        if let Some(value) = value {
            arguments.push(value);
        }
        arguments.extend(["--expected-revision", revision, "--idempotency-key", key]);
        let output = mutate(&directory.0, &config, &arguments);
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert!(
            text(&output.stdout).contains(&format!("status: {status}\n")),
            "{}",
            text(&output.stdout)
        );
    }

    let rejected = capture(&directory.0, &config, "Reject", "capture-reject");
    let output = mutate(
        &directory.0,
        &config,
        &[
            "reject",
            &rejected,
            "Not planned",
            "--expected-revision",
            "1",
            "--idempotency-key",
            "reject",
        ],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("status: rejected\n"));
}

#[test]
fn omitted_fields_remain_unchanged_while_clear_is_explicit() {
    let (directory, config) = initialized();
    let id = capture(&directory.0, &config, "Clear", "capture-clear");
    let set = mutate(
        &directory.0,
        &config,
        &[
            "triage",
            &id,
            "--priority",
            "P2",
            "--assignee",
            "Taylor",
            "--expected-revision",
            "1",
            "--idempotency-key",
            "set",
        ],
    );
    assert!(set.status.success(), "{}", text(&set.stderr));

    let clear_priority = mutate(
        &directory.0,
        &config,
        &[
            "prioritize",
            &id,
            "clear",
            "--expected-revision",
            "2",
            "--idempotency-key",
            "clear-priority",
        ],
    );
    assert!(
        clear_priority.status.success(),
        "{}",
        text(&clear_priority.stderr)
    );
    assert!(text(&clear_priority.stdout).contains("priority: unprioritized\nassignee: taylor\n"));

    let clear_assignee = mutate(
        &directory.0,
        &config,
        &[
            "assign",
            &id,
            "clear",
            "--expected-revision",
            "3",
            "--idempotency-key",
            "clear-assignee",
        ],
    );
    assert!(
        clear_assignee.status.success(),
        "{}",
        text(&clear_assignee.stderr)
    );
    assert!(text(&clear_assignee.stdout).contains("assignee: unassigned\n"));
}
