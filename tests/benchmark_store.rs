#![cfg(unix)]

mod support;

use std::{
    fs,
    process::{Command, Stdio},
};

use bif::benchmark_fixture;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use support::OwnedTestDirectory;

fn wait_for_marker_path(
    marker: &std::path::Path,
    ready: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    for _ in 0..1_000 {
        if let Ok(bytes) = fs::read(marker)
            && let Ok(text) = String::from_utf8(bytes)
        {
            let path = std::path::PathBuf::from(text);
            if ready(&path) {
                return path;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!(
        "generator did not publish a ready marker path at {}",
        marker.display()
    );
}

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
    assert!(!std::path::Path::new(&format!("{}.metadata.json", database.display())).exists());

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
    assert_eq!(
        fs::metadata(&database).unwrap().len(),
        0,
        "the invocation-owned database claim may remain after failure"
    );
    assert_eq!(fs::read(sidecar).unwrap(), b"existing metadata");
    assert_eq!(fs::read(wal).unwrap(), b"other process wal");
    assert_eq!(fs::read(shm).unwrap(), b"other process shm");
}

#[test]
fn replacement_after_claim_is_preserved_and_prevents_success() {
    for replace_metadata in [false, true] {
        let directory = OwnedTestDirectory::new();
        let database = directory.path().join(if replace_metadata {
            "metadata.sqlite3"
        } else {
            "database.sqlite3"
        });
        let metadata = format!("{}.metadata.json", database.display());
        let marker = directory.path().join("claimed.marker");
        let mut child = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .args(["100", "--output"])
            .arg(&database)
            .env("BIF_BENCHMARK_STORE_TEST_CLAIM_MARKER", &marker)
            .spawn()
            .unwrap();
        for _ in 0..1_000 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(marker.exists(), "generator reached the post-claim boundary");

        let replaced = if replace_metadata {
            std::path::Path::new(&metadata)
        } else {
            database.as_path()
        };
        fs::remove_file(replaced).unwrap();
        fs::write(replaced, b"unowned replacement").unwrap();
        fs::remove_file(&marker).unwrap();

        assert!(!child.wait().unwrap().success());
        assert_eq!(fs::read(replaced).unwrap(), b"unowned replacement");
    }
}

#[test]
fn replaced_staging_path_cannot_redirect_sqlite_or_cleanup() {
    use std::os::unix::fs::symlink;

    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("store.sqlite3");
    let metadata = format!("{}.metadata.json", database.display());
    let wal = format!("{}-wal", database.display());
    let shm = format!("{}-shm", database.display());
    let marker = directory.path().join("staging-ready.marker");
    fs::write(&wal, b"final wal sentinel").unwrap();
    fs::write(&shm, b"final shm sentinel").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--output"])
        .arg(&database)
        .env("BIF_BENCHMARK_STORE_TEST_CLAIM_MARKER", &marker)
        .spawn()
        .unwrap();
    let staging = wait_for_marker_path(&marker, std::path::Path::is_dir);
    let renamed_staging = directory.path().join("renamed-owned-staging");
    fs::rename(&staging, &renamed_staging).unwrap();
    symlink(directory.path(), &staging).unwrap();
    assert!(
        staging.is_dir(),
        "the staging pathname now resolves through the replacement symlink"
    );
    fs::remove_file(&marker).unwrap();

    let status = child.wait().unwrap();
    assert!(
        !status.success(),
        "staging replacement cannot report success"
    );
    assert_eq!(fs::read(&wal).unwrap(), b"final wal sentinel");
    assert_eq!(fs::read(&shm).unwrap(), b"final shm sentinel");
    assert!(
        fs::symlink_metadata(&staging)
            .unwrap()
            .file_type()
            .is_symlink(),
        "capability cleanup must preserve the unowned pathname replacement"
    );
    assert!(
        renamed_staging.is_dir(),
        "the renamed owned directory is orphaned rather than recursively deleted"
    );
    assert_eq!(
        fs::metadata(&database).unwrap().len(),
        0,
        "the already claimed final database is never opened by SQLite"
    );
    assert_eq!(
        fs::metadata(&metadata).unwrap().len(),
        0,
        "the already claimed metadata file remains unpublished"
    );
}

#[test]
fn replaced_staged_file_is_rejected_before_sqlite_mutates_substitute() {
    let directory = OwnedTestDirectory::new();
    let donor = directory.path().join("existing.sqlite3");
    let donor_generated = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--seed", "2003", "--output"])
        .arg(&donor)
        .output()
        .unwrap();
    assert!(
        donor_generated.status.success(),
        "{}",
        String::from_utf8_lossy(&donor_generated.stderr)
    );
    let donor_bytes = fs::read(&donor).unwrap();
    let donor_digest = {
        let connection =
            Connection::open_with_flags(&donor, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        benchmark_fixture::summarize(&connection).unwrap().digest
    };

    let database = directory.path().join("store.sqlite3");
    let metadata = format!("{}.metadata.json", database.display());
    let final_wal = format!("{}-wal", database.display());
    let marker = directory.path().join("claimed.marker");
    fs::write(&final_wal, b"final wal sentinel").unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--output"])
        .arg(&database)
        .env("BIF_BENCHMARK_STORE_TEST_CLAIM_MARKER", &marker)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let staging = wait_for_marker_path(&marker, std::path::Path::is_dir);
    let staged = fs::read_dir(&staging)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_file())
        .expect("generator claimed its staged database");
    let retained_staged = staged.with_extension("retained-owned-database");
    fs::rename(&staged, &retained_staged).unwrap();
    fs::hard_link(&donor, &staged).unwrap();
    fs::remove_file(&marker).unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("was replaced during generation"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&donor).unwrap(),
        donor_bytes,
        "SQLite must not mutate the hard-linked substitute"
    );
    let unchanged_digest = {
        let connection =
            Connection::open_with_flags(&donor, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        benchmark_fixture::summarize(&connection).unwrap().digest
    };
    assert_eq!(unchanged_digest, donor_digest);
    assert!(
        !staged
            .with_file_name(format!(
                "{}-wal",
                staged.file_name().unwrap().to_string_lossy()
            ))
            .exists()
    );
    assert!(
        !staged
            .with_file_name(format!(
                "{}-shm",
                staged.file_name().unwrap().to_string_lossy()
            ))
            .exists()
    );
    assert_eq!(fs::read(&final_wal).unwrap(), b"final wal sentinel");
    assert_eq!(fs::metadata(&database).unwrap().len(), 0);
    assert_eq!(fs::metadata(&metadata).unwrap().len(), 0);
}

#[test]
fn replaced_staged_file_cannot_substitute_publication_input() {
    use std::os::unix::fs::symlink;

    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("store.sqlite3");
    let metadata = format!("{}.metadata.json", database.display());
    let marker = directory.path().join("staged-database-ready.marker");
    let substitute = directory.path().join("unowned-substitute");
    fs::write(&substitute, b"not a SQLite database").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--output"])
        .arg(&database)
        .env("BIF_BENCHMARK_STORE_TEST_STAGED_MARKER", &marker)
        .spawn()
        .unwrap();
    let staged = wait_for_marker_path(&marker, std::path::Path::is_file);
    assert!(
        fs::metadata(&staged).unwrap().len() > 0,
        "SQLite completed a nonempty staged database"
    );
    let retained_staged = staged.with_extension("retained-owned-database");
    fs::rename(&staged, &retained_staged).unwrap();
    symlink(&substitute, &staged).unwrap();
    fs::remove_file(&marker).unwrap();

    assert!(
        !child.wait().unwrap().success(),
        "staged-file replacement cannot report publication success"
    );
    assert_eq!(
        fs::metadata(&database).unwrap().len(),
        0,
        "the substitute was not copied into the claimed final database"
    );
    assert_eq!(fs::metadata(&metadata).unwrap().len(), 0);
    assert_eq!(fs::read(&substitute).unwrap(), b"not a SQLite database");
    assert!(
        fs::symlink_metadata(&staged)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the replacement staged entry is preserved"
    );
    assert!(
        fs::metadata(&retained_staged).unwrap().len() > 0,
        "the retained owned staged file is preserved on failure"
    );
}

#[test]
fn explicit_output_never_opens_final_sqlite_basename() {
    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("store.sqlite3");
    let wal = format!("{}-wal", database.display());
    let shm = format!("{}-shm", database.display());
    fs::write(&wal, b"unowned wal sentinel").unwrap();
    fs::write(&shm, b"unowned shm sentinel").unwrap();

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
    // Success proves the test reached generation and SQLite work rather than
    // taking an early metadata/output collision path.
    assert_eq!(fs::read(&wal).unwrap(), b"unowned wal sentinel");
    assert_eq!(fs::read(&shm).unwrap(), b"unowned shm sentinel");
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn successful_generation_leaves_only_a_truncated_staging_orphan() {
    let directory = OwnedTestDirectory::new();
    let database = directory.path().join("store.sqlite3");
    let generated = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--output"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let staging = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".bif-benchmark-stage-")
        })
        .expect("successful generation preserves its staging directory");
    let staged_entries = fs::read_dir(&staging)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        staged_entries.len(),
        1,
        "checkpoint and close leave only the staged main file"
    );
    assert_eq!(
        staged_entries[0].metadata().unwrap().len(),
        0,
        "successful publication truncates only through the retained staged handle"
    );
}

#[test]
fn omitted_output_allocates_distinct_private_destinations() {
    let generate = || {
        let generated = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
            .arg("100")
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        serde_json::from_slice::<Value>(&generated.stdout).unwrap()
    };

    let first = generate();
    let second = generate();
    let first_database = first["database"].as_str().unwrap();
    let second_database = second["database"].as_str().unwrap();
    assert_ne!(first_database, second_database);
    assert!(std::path::Path::new(first_database).is_file());
    assert!(std::path::Path::new(second_database).is_file());
}

#[test]
fn omitted_output_failure_leaves_only_owned_orphans() {
    let temporary_root = OwnedTestDirectory::new();
    let failed = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .arg("100")
        .env("TMPDIR", temporary_root.path())
        .env("BIF_BENCHMARK_STORE_TEST_FAIL_AFTER_CLAIMS", "1")
        .output()
        .unwrap();

    assert!(!failed.status.success());
    let entries = fs::read_dir(temporary_root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "one invocation-owned outer orphan remains"
    );
    let orphan = entries[0].path();
    assert!(
        orphan
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("bif-benchmark-"),
        "the orphan is clearly identified for manual removal"
    );
    assert!(orphan.join("store.sqlite3").is_file());
    assert!(orphan.join("store.sqlite3.metadata.json").is_file());
    assert!(
        fs::read_dir(&orphan).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".bif-benchmark-stage-")),
        "the owned staging orphan remains inside the owned outer orphan"
    );
}
