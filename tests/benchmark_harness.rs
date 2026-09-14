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

#[cfg(unix)]
fn copied_bundle_summary(database: &Path) -> benchmark_fixture::Summary {
    let copy_directory = OwnedTestDirectory::new();
    let copy = copy_directory.path().join("readable.sqlite3");
    fs::copy(database, &copy).unwrap();
    fs::copy(
        database.with_file_name("100.sqlite3-wal"),
        copy.with_file_name("readable.sqlite3-wal"),
    )
    .unwrap();
    // SQLite may create SHM beside this private byte-for-byte copy. It never
    // opens the public source path, so it cannot make the source assertion
    // vacuous.
    let connection =
        Connection::open_with_flags(copy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    benchmark_fixture::summarize(&connection).unwrap()
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
    let measured_store = Path::new(value["measured_store"].as_str().unwrap());
    assert_eq!(
        fs::metadata(measured_store).unwrap().len(),
        0,
        "completed harness cleanup truncates the exact retained snapshot file"
    );
    assert!(
        measured_store.parent().unwrap().is_dir(),
        "the owned temporary directory remains for external cleanup"
    );
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

#[cfg(unix)]
#[test]
fn wal_without_shm_fails_closed_without_changing_the_source() {
    use std::os::unix::fs::symlink;

    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("100.sqlite3");
    let alias = directory.path().join("fixture-alias.sqlite3");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .args(["100", "--seed", "2003", "--output"])
            .arg(&database)
            .status()
            .unwrap()
            .success()
    );
    symlink(&database, &alias).unwrap();

    let wal = database.with_file_name("100.sqlite3-wal");
    let shm = database.with_file_name("100.sqlite3-shm");
    let metadata = database.with_file_name("100.sqlite3.metadata.json");
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let database_before_wal_commit = fs::read(&database).unwrap();
    writer
        .execute(
            "UPDATE items SET title = 'committed crash-residue WAL content' \
             WHERE item_id = (SELECT item_id FROM items ORDER BY item_id LIMIT 1)",
            [],
        )
        .unwrap();
    assert_eq!(
        database_before_wal_commit,
        fs::read(&database).unwrap(),
        "the committed change must reside only in the WAL"
    );
    assert!(wal.metadata().unwrap().len() > 0);
    assert!(shm.exists());
    refresh_metadata(&database);

    // Preserve the committed database/WAL pair as though the writer crashed,
    // then restore those exact bytes after normal close cleanup. SHM is a
    // reconstructible index and is intentionally omitted from this residue.
    let database_residue = fs::read(&database).unwrap();
    let wal_residue = fs::read(&wal).unwrap();
    let metadata_residue = fs::read(&metadata).unwrap();
    drop(writer);
    fs::write(&database, database_residue).unwrap();
    fs::write(&wal, wal_residue).unwrap();
    fs::write(&metadata, metadata_residue).unwrap();
    if shm.exists() {
        fs::remove_file(&shm).unwrap();
    }

    let expected_metadata: Value = serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    let readable = copied_bundle_summary(&database);
    assert_eq!(
        format!("{:016x}", readable.digest),
        expected_metadata["logical_digest"].as_str().unwrap(),
        "a private copy must be readable and observe the committed WAL content"
    );
    assert!(
        !shm.exists(),
        "the independent copied-bundle readability check must not create source SHM"
    );

    let lexical_shm = alias.with_file_name("fixture-alias.sqlite3-shm");
    fs::write(
        &lexical_shm,
        b"lexical alias companion is not canonical SHM",
    )
    .unwrap();
    let lexical_shm_before = state(&lexical_shm);
    let source_before = [state(&database), state(&wal), state(&metadata)];
    let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
        .arg(&alias)
        .args(["--samples", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unsupported source state"), "{stderr}");
    assert!(stderr.contains("WAL"), "{stderr}");
    assert!(stderr.contains("SHM"), "{stderr}");
    assert!(
        !shm.exists(),
        "the rejected measurement must leave the source SHM absent"
    );
    assert_eq!(
        source_before,
        [state(&database), state(&wal), state(&metadata)],
        "the rejected measurement must preserve database, WAL, and metadata bytes"
    );
    assert_eq!(
        lexical_shm_before,
        state(&lexical_shm),
        "a lexical-alias SHM must not substitute for or be changed as canonical SHM"
    );

    let readable_after = copied_bundle_summary(&database);
    assert_eq!(
        readable.digest, readable_after.digest,
        "the rejected source must remain readable with the same logical content"
    );
    assert!(
        !shm.exists(),
        "post-failure copied-bundle reading must not create source SHM"
    );
}

#[cfg(unix)]
#[test]
fn symlink_alias_uses_canonical_provenance_and_protects_both_companion_sets() {
    use std::os::unix::fs::symlink;

    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("100.sqlite3");
    let alias = directory.path().join("fixture-alias.sqlite3");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .args(["100", "--seed", "2003", "--output"])
            .arg(&database)
            .status()
            .unwrap()
            .success()
    );
    symlink(&database, &alias).unwrap();

    let report = directory.path().join("alias-measurement.json");
    let measured = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
        .arg(&alias)
        .args(["--samples", "1", "--output"])
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        measured.status.success(),
        "{}",
        String::from_utf8_lossy(&measured.stderr)
    );
    let value: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(value["provenance"]["fixture_seed"], 2003);
    assert!(value["provenance"]["fixture_digest"].is_string());
    assert_eq!(
        value["source_database"],
        fs::canonicalize(&database).unwrap().display().to_string()
    );

    for protected in [
        format!("{}-wal", alias.display()),
        format!("{}-shm", alias.display()),
        format!("{}.metadata.json", alias.display()),
        format!("{}-wal", database.display()),
        format!("{}-shm", database.display()),
        format!("{}.metadata.json", database.display()),
    ] {
        let protected = Path::new(&protected);
        let existed = protected.exists();
        let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
            .arg(&alias)
            .args(["--samples", "1", "--output"])
            .arg(protected)
            .output()
            .unwrap();
        assert!(
            !output.status.success()
                && String::from_utf8_lossy(&output.stderr).contains("refusing"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            protected.exists(),
            existed,
            "must not create {}",
            protected.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_alias_sidecar_is_rejected_as_ambiguous() {
    use std::os::unix::fs::symlink;

    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("100.sqlite3");
    let alias = directory.path().join("fixture-alias.sqlite3");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .args(["100", "--output"])
            .arg(&database)
            .status()
            .unwrap()
            .success()
    );
    symlink(&database, &alias).unwrap();
    let canonical_sidecar = format!("{}.metadata.json", database.display());
    let alias_sidecar = format!("{}.metadata.json", alias.display());
    let mut conflicting = fs::read(&canonical_sidecar).unwrap();
    conflicting.extend_from_slice(b"\n");
    fs::write(&alias_sidecar, conflicting).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
        .arg(&alias)
        .args(["--samples", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ambiguous fixture metadata"), "{stderr}");
    assert!(stderr.contains(&alias_sidecar), "{stderr}");
    assert!(stderr.contains(&canonical_sidecar), "{stderr}");
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
    let wal = database.with_file_name("100.sqlite3-wal");
    let shm = database.with_file_name("100.sqlite3-shm");
    let metadata = database.with_file_name("100.sqlite3.metadata.json");
    let logical_before = {
        let reader =
            Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let summary = benchmark_fixture::summarize(&reader).unwrap();
        (summary.digest, summary.row_counts, summary.distributions)
    };
    let durable_before = [state(&database), state(&wal), state(&metadata)];
    let shm_existed_before = shm.exists();
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

    for sidecar in [
        "100.sqlite3",
        "100.sqlite3-wal",
        "100.sqlite3-shm",
        "100.sqlite3.metadata.json",
    ] {
        let child = directory.path().join(sidecar).join("child.json");
        let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
            .arg(&database)
            .args(["--samples", "1", "--output"])
            .arg(&child)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!child.parent().unwrap().is_dir());
    }

    let missing_parent = directory.path().join("missing").join("report.json");
    let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
        .arg(&database)
        .args(["--samples", "1", "--output"])
        .arg(&missing_parent)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("output parent directory must already exist")
    );
    assert!(!missing_parent.parent().unwrap().exists());

    assert_eq!(
        durable_before,
        [state(&database), state(&wal), state(&metadata)],
        "rejected outputs must not change source database, WAL, or metadata"
    );
    assert_eq!(
        shm_existed_before,
        shm.exists(),
        "rejected outputs must neither create nor remove the source SHM path"
    );
    let reader =
        Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let summary = benchmark_fixture::summarize(&reader).unwrap();
    assert_eq!(
        logical_before,
        (summary.digest, summary.row_counts, summary.distributions),
        "rejected outputs must leave the source readable and logically unchanged"
    );
}

#[test]
fn report_accepts_bare_and_dot_relative_filenames_without_overwriting() {
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

    for report in ["report.json", "./dot-report.json"] {
        let measured = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
            .current_dir(directory.path())
            .arg(&database)
            .args(["--samples", "1", "--output", report])
            .output()
            .unwrap();
        assert!(
            measured.status.success(),
            "{}",
            String::from_utf8_lossy(&measured.stderr)
        );
        let report_path = directory.path().join(report);
        let value: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
        assert_eq!(value["format"], "bif-v2-measurement-v2");

        let second = Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
            .current_dir(directory.path())
            .arg(&database)
            .args(["--samples", "1", "--output", report])
            .output()
            .unwrap();
        assert!(!second.status.success());
        assert!(String::from_utf8_lossy(&second.stderr).contains("refusing to overwrite"));
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(report_path).unwrap()).unwrap(),
            value
        );
    }
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
