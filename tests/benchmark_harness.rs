mod support;
use bif::benchmark_fixture;
use rusqlite::Connection;
use serde_json::Value;
use std::{fs, path::Path, process::Command};
use support::OwnedTestDirectory;

fn state(path: &Path) -> Option<(Vec<u8>, u64, Option<std::time::SystemTime>)> {
    fs::read(path).ok().map(|bytes| {
        let m = fs::metadata(path).unwrap();
        (bytes, m.len(), m.modified().ok())
    })
}

fn refresh_metadata(database: &Path) {
    let connection = Connection::open(database).unwrap();
    let summary = benchmark_fixture::summarize(&connection).unwrap();
    let path = format!("{}.metadata.json", database.display());
    let mut metadata: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    metadata["logical_digest"] = format!("{:016x}", summary.digest).into();
    metadata["row_counts"] = serde_json::to_value(summary.row_counts).unwrap();
    metadata["distributions"] = serde_json::to_value(summary.distributions).unwrap();
    fs::write(path, serde_json::to_vec_pretty(&metadata).unwrap()).unwrap();
}

#[test]
fn report_uses_snapshot_and_contains_review_evidence() {
    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("100.sqlite3");
    let report = directory.path().join("measurement.json");
    let generated = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--seed", "2003", "--output"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let wal = database.with_file_name("100.sqlite3-wal");
    let shm = database.with_file_name("100.sqlite3-shm");
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let database_before_wal_commit = state(&database);
    writer
        .execute(
            "UPDATE items SET title = 'committed only in active WAL' \
             WHERE item_id = (SELECT item_id FROM items ORDER BY item_id LIMIT 1)",
            [],
        )
        .unwrap();
    assert_eq!(
        database_before_wal_commit,
        state(&database),
        "the committed fixture change must still reside outside the database file"
    );
    assert!(wal.metadata().unwrap().len() > 0);
    assert!(shm.exists());
    refresh_metadata(&database);
    let metadata = database.with_file_name("100.sqlite3.metadata.json");
    let logical_before = {
        let reader =
            Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let summary = benchmark_fixture::summarize(&reader).unwrap();
        (summary.digest, summary.row_counts, summary.distributions)
    };
    let durable_before = [state(&database), state(&wal), state(&metadata)];
    let shm_before = state(&shm);
    let measured = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
        .arg(&database)
        .args(["--samples", "2", "--output"])
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        measured.status.success(),
        "{}",
        String::from_utf8_lossy(&measured.stderr)
    );
    assert_eq!(
        durable_before,
        [state(&database), state(&wal), state(&metadata)],
        "measurement must not change database, WAL, or fixture sidecar bytes or metadata"
    );
    assert_eq!(
        shm_before.is_some(),
        state(&shm).is_some(),
        "measurement must neither create nor remove an active WAL's SHM path"
    );
    // A read-only SQLite online backup may update transient read marks and
    // coordination bytes in an active WAL's SHM file. Its bytes and metadata
    // are intentionally not asserted; DB, WAL, sidecar, and logical state are.
    let _shm_coordination_changed = shm_before != state(&shm);
    let logical_after = {
        let reader =
            Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let summary = benchmark_fixture::summarize(&reader).unwrap();
        (summary.digest, summary.row_counts, summary.distributions)
    };
    assert_eq!(
        logical_before, logical_after,
        "measurement must not change the source fixture's logical content"
    );
    let value: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(value["format"], "bif-v2-measurement-v2");
    assert_ne!(value["source_database"], value["measured_store"]);
    assert_eq!(value["durability"]["journal_mode"], "wal");
    assert_eq!(value["durability"]["foreign_keys"], "ON");
    assert_eq!(value["durability"]["synchronous"], "FULL");
    assert!(
        value["measured_store_sizes_before_writes"]["database"]["bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(value["measured_store_sizes_before_writes"]["wal"]["status"].is_string());
    assert!(value["measured_store_sizes_after_writes"]["wal"]["status"].is_string());
    assert_eq!(value["persistence_verification"]["integrity_check"], "ok");
    assert_eq!(
        value["persistence_verification"]["foreign_key_violations"],
        0
    );
    assert_eq!(
        value["persistence_verification"]["revision"],
        value["persistence_verification"]["expected_revision"]
    );
    assert_eq!(value["provenance"]["fixture_seed"], 2003);
    assert!(value["provenance"]["fixture_digest"].is_string());
    assert!(value["provenance"]["sqlite_version"].is_string());
    assert!(value["provenance"]["exact_command"].is_array());
    let operation = |name| {
        value["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == name)
            .unwrap()
    };
    let get = operation("get");
    assert!(
        get["serialization_shape"]
            .as_str()
            .unwrap()
            .contains("v1 CLI/RPC")
    );
    assert_eq!(get["sqlite_profile_nanoseconds"]["sample_count"], 2);
    assert_eq!(
        get["sqlite_profile_nanoseconds"]["samples"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(get["profile_exceeded_wall_samples"].is_array());
    assert!(
        operation("history")["response_bytes"]["samples"][0]
            .as_u64()
            .unwrap()
            > 2
    );
    let list = operation("list_all");
    assert_eq!(list["matches_per_sample"], serde_json::json!([100, 100]));
    assert_eq!(
        list["lexical_data_statements_per_sample"],
        serde_json::json!([201, 201])
    );
    assert_eq!(list["sql_metrics_per_sample"].as_array().unwrap().len(), 2);
    assert!(
        list["sql_metrics_per_sample"][0]["sqlite_row_callbacks"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn stale_fixture_sidecar_is_rejected() {
    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("100.sqlite3");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .args(["100", "--output"])
            .arg(&database)
            .status()
            .unwrap()
            .success()
    );
    let metadata_path = format!("{}.metadata.json", database.display());
    let mut metadata: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["logical_digest"] = "0000000000000000".into();
    fs::write(metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
        .arg(&database)
        .args(["--samples", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("stale"));
}

#[test]
fn report_output_cannot_overwrite_fixture_paths_or_existing_files() {
    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("100.sqlite3");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .args(["100", "--output"])
            .arg(&database)
            .status()
            .unwrap()
            .success()
    );
    let existing = directory.path().join("existing.json");
    fs::write(&existing, b"keep me").unwrap();
    let protected = [
        database.clone(),
        database.with_file_name("100.sqlite3-wal"),
        database.with_file_name("100.sqlite3-shm"),
        database.with_file_name("100.sqlite3.metadata.json"),
        existing.clone(),
    ];
    for path in protected {
        let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
            .arg(&database)
            .args(["--samples", "1", "--output"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "protected output unexpectedly succeeded: {}",
            path.display()
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("refusing"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(fs::read(existing).unwrap(), b"keep me");
    assert!(database.exists());
    assert!(
        database
            .with_file_name("100.sqlite3.metadata.json")
            .exists()
    );
}

#[test]
fn sample_count_is_bounded() {
    for invalid in ["0", "1001", "18446744073709551615"] {
        let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
            .args(["missing.sqlite3", "--samples", invalid])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--samples"));
    }
}
