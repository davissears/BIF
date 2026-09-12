use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use bif::storage::{MigrationError, migrate, open};
use rusqlite::{Connection, OptionalExtension};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

fn temporary_database() -> PathBuf {
    let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "bif-storage-test-{}-{sequence}.sqlite",
        std::process::id()
    ))
}

fn table_exists(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
}

#[test]
fn fresh_install_records_schema_and_store_metadata() {
    let path = temporary_database();
    let connection = open(&path).unwrap();

    assert!(table_exists(&connection, "schema_migrations"));
    assert!(table_exists(&connection, "store_metadata"));
    let migration: (i64, String, String, bool) = connection
        .query_row(
            "SELECT version, name, checksum, length(applied_at) > 0
             FROM schema_migrations
             WHERE version = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(migration.0, 1);
    assert_eq!(migration.1, "initial");
    assert_eq!(migration.2.len(), 64);
    assert!(migration.3);

    let store: (i64, String, bool) = connection
        .query_row(
            "SELECT singleton, store_id, length(created_at) > 0 FROM store_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(store.0, 1);
    assert!(!store.1.is_empty());
    assert!(store.2);

    drop(connection);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn repeat_startup_is_a_no_op_and_preserves_store_identity() {
    let path = temporary_database();
    let connection = open(&path).unwrap();
    let store_id: String = connection
        .query_row("SELECT store_id FROM store_metadata", [], |row| row.get(0))
        .unwrap();
    drop(connection);

    let connection = open(&path).unwrap();

    let state: (i64, String) = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM schema_migrations),
                (SELECT store_id FROM store_metadata)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (2, store_id));

    drop(connection);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn checksum_mismatch_is_rejected_without_changing_history() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    connection
        .execute(
            "UPDATE schema_migrations SET checksum = 'modified' WHERE version = 1",
            [],
        )
        .unwrap();

    let error = migrate(&mut connection).unwrap_err();

    assert!(matches!(
        error,
        MigrationError::ChecksumMismatch {
            version: 1,
            ref found,
            ..
        } if found == "modified"
    ));
    let checksum: String = connection
        .query_row(
            "SELECT checksum FROM schema_migrations WHERE version = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(checksum, "modified");
}

#[test]
fn newer_schema_version_is_rejected() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at)
             VALUES (3, 'future', 'future-checksum', 'now')",
            [],
        )
        .unwrap();

    let error = migrate(&mut connection).unwrap_err();

    assert!(matches!(
        error,
        MigrationError::NewerSchema {
            found: 3,
            supported: 2
        }
    ));
}
