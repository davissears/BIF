mod support;

use std::{fs, process::Command};

use serde_json::Value;
use support::OwnedTestDirectory;

#[test]
fn small_benchmark_store_is_deterministic_and_verified() {
    let directory = OwnedTestDirectory::new();
    let first = directory.path().join("first.sqlite3");
    let second = directory.path().join("second.sqlite3");
    let generate = |path: &std::path::Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .args(["100", "--seed", "8675309", "--output"])
            .arg(path)
            .output()
            .expect("run fixture generator");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).expect("machine-readable metadata")
    };

    let first_metadata = generate(&first);
    let second_metadata = generate(&second);
    assert_eq!(first_metadata["items"], 100);
    assert_eq!(
        first_metadata["logical_digest"],
        second_metadata["logical_digest"]
    );
    assert_eq!(first_metadata["row_counts"], second_metadata["row_counts"]);
    assert_eq!(
        first_metadata["distributions"],
        second_metadata["distributions"]
    );
    assert_eq!(first_metadata["integrity_check"], "ok");
    assert_eq!(
        first_metadata["samples_verified"].as_array().unwrap().len(),
        3
    );
    assert!(fs::metadata(format!("{}.metadata.json", first.display())).is_ok());
    for (distribution, buckets) in [
        (
            "projects",
            &["core", "agent-tools", "desktop", "docs", "rare-project"][..],
        ),
        (
            "statuses",
            &[
                "proposed",
                "ready",
                "in_progress",
                "blocked",
                "done",
                "rejected",
            ][..],
        ),
        ("priorities", &["P0", "P1", "P2", "P3", "P4", "null"][..]),
    ] {
        for bucket in buckets {
            assert!(
                first_metadata["distributions"][distribution][bucket]
                    .as_u64()
                    .is_some_and(|count| count > 0),
                "{distribution}/{bucket} must be guaranteed"
            );
        }
    }
    assert!(
        first_metadata["distributions"]["assignees"]["null"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        first_metadata["distributions"]["assignees"]
            .as_object()
            .unwrap()
            .iter()
            .any(|(name, count)| name != "null" && count.as_u64().unwrap() > 0)
    );
    for bucket in ["present", "absent"] {
        assert!(
            first_metadata["distributions"]["sparse_markers"][bucket]
                .as_u64()
                .unwrap()
                > 0
        );
    }
    assert_eq!(
        first_metadata["logical_digest_algorithm"],
        "fnv1a64-framed-canonical-tables-v2"
    );
    assert_eq!(first_metadata["row_counts"]["store_metadata"], 1);
    assert!(
        first_metadata["row_counts"]["requester_project_counters"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn explicit_output_failure_never_removes_unowned_database_or_sqlite_sidecars() {
    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("store.sqlite3");
    fs::write(&database, b"existing database").unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--output"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(fs::read(&database).unwrap(), b"existing database");

    fs::remove_file(&database).unwrap();
    let sidecar = format!("{}.metadata.json", database.display());
    let wal = format!("{}-wal", database.display());
    let shm = format!("{}-shm", database.display());
    fs::write(&sidecar, b"existing metadata").unwrap();
    // These paths model another process winning the sidecar creation race.
    // An explicit output only atomically claims the main database pathname.
    fs::write(&wal, b"other process wal").unwrap();
    fs::write(&shm, b"other process shm").unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--output"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        !database.exists(),
        "only the invocation-owned database is removed"
    );
    assert_eq!(fs::read(sidecar).unwrap(), b"existing metadata");
    assert_eq!(fs::read(wal).unwrap(), b"other process wal");
    assert_eq!(fs::read(shm).unwrap(), b"other process shm");
}
