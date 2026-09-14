mod support;

use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use support::OwnedTestDirectory as TestDirectory;

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

fn initialized() -> (TestDirectory, PathBuf, PathBuf) {
    let directory = TestDirectory::new();
    let root = directory.path().join("root");
    fs::create_dir(&root).unwrap();
    let config = directory.path().join("config.toml");
    let initialized = bif(
        directory.path(),
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
    assert!(
        initialized.status.success(),
        "{}",
        text(&initialized.stderr)
    );
    (directory, root, config)
}

#[test]
fn capture_persists_content_provenance_and_ordered_acceptance() {
    let (directory, root, config) = initialized();
    let captured = bif(
        directory.path(),
        &[
            "capture",
            "Ship capture CLI",
            "--description",
            "Use the application service",
            "--acceptance",
            "Persists content",
            "--acceptance",
            "Reports the ID",
            "--idempotency-key",
            "capture-cli-1",
            "--project",
            "BIF",
            "--config",
            config.to_str().unwrap(),
            "--source-host",
            "delta",
            "--thread-id",
            "thread-41",
            "--message-id",
            "message-41",
            "--url",
            "https://delta.example/thread-41",
            "--repository-reference",
            "repo:BIF",
            "--revision-reference",
            "commit:abc",
            "--context-excerpt",
            "Implement BIF-041 only",
        ],
    );
    assert!(captured.status.success(), "{}", text(&captured.stderr));
    assert_eq!(
        text(&captured.stdout),
        "Captured DELTA-AGENT:bif:001\ntitle: Ship capture CLI\nstatus: proposed\nreplayed: false\nsource: delta\n"
    );

    let connection = Connection::open(root.join(".bif/bif.sqlite")).unwrap();
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT i.requester, i.project_id, i.description, p.source_host, p.thread_id,
                    p.message_id, p.repository_reference, p.context_excerpt
             FROM items i JOIN item_provenance p USING (item_id)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "DELTA-AGENT".into(),
            "bif".into(),
            "Use the application service".into(),
            "delta".into(),
            "thread-41".into(),
            "message-41".into(),
            "repo:BIF".into(),
            "Implement BIF-041 only".into(),
        )
    );
    let criteria = connection
        .prepare("SELECT criterion FROM item_acceptance_criteria ORDER BY criterion_index")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(criteria, ["Persists content", "Reports the ID"]);
}

#[test]
fn retry_returns_the_original_id_and_conflicts_are_operational_failures() {
    let (directory, _, config) = initialized();
    let arguments = [
        "capture",
        "Retry safely",
        "--idempotency-key",
        "same-key",
        "--project",
        "BIF",
        "--config",
        config.to_str().unwrap(),
    ];
    let first = bif(directory.path(), &arguments);
    let retry = bif(directory.path(), &arguments);
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert!(retry.status.success(), "{}", text(&retry.stderr));
    assert_eq!(
        text(&retry.stdout),
        "Captured DELTA-AGENT:bif:001\ntitle: Retry safely\nstatus: proposed\nreplayed: true\nsource: local\n"
    );

    let conflict = bif(
        directory.path(),
        &[
            "capture",
            "Changed payload",
            "--idempotency-key",
            "same-key",
            "--project",
            "BIF",
            "--config",
            config.to_str().unwrap(),
        ],
    );
    assert_eq!(conflict.status.code(), Some(1));
    assert_eq!(
        text(&conflict.stderr),
        "error: the idempotency key was already used with different input\n"
    );
}

#[test]
fn capture_uses_configured_requester_and_resolves_registered_project() {
    let (directory, _, config) = initialized();
    let checkout = directory.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    let registered = bif(
        directory.path(),
        &[
            "project",
            "register",
            "Registered Project",
            "--path",
            checkout.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
        ],
    );
    assert!(registered.status.success(), "{}", text(&registered.stderr));
    let captured = bif(
        &checkout,
        &[
            "capture",
            "Resolved identities",
            "--idempotency-key",
            "resolved",
            "--config",
            config.to_str().unwrap(),
            "--requester",
            "Override User",
        ],
    );
    assert!(captured.status.success(), "{}", text(&captured.stderr));
    assert!(text(&captured.stdout).starts_with("Captured OVERRIDE-USER:registered-project:001\n"));
}
