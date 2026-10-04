mod support;

use std::path::PathBuf;

use bif::storage::{MigrationError, migrate, open};
use rusqlite::{Connection, OptionalExtension};
use support::OwnedTestDirectory;

fn temporary_database() -> (OwnedTestDirectory, PathBuf) {
    let directory = OwnedTestDirectory::new();
    let path = directory.path().join("bif.sqlite");
    (directory, path)
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
    let (_directory, path) = temporary_database();
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
}

#[test]
fn repeat_startup_is_a_no_op_and_preserves_store_identity() {
    let (_directory, path) = temporary_database();
    let connection = open(&path).unwrap();
    seed_data(&connection);
    let data = durable_data(&connection);
    let history = rows(
        &connection,
        "SELECT * FROM schema_migrations ORDER BY version",
    );
    let indexes = read_indexes(&connection);
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
    assert_eq!(state, (3, store_id));
    assert_eq!(durable_data(&connection), data);
    assert_eq!(
        rows(
            &connection,
            "SELECT * FROM schema_migrations ORDER BY version"
        ),
        history
    );
    assert_eq!(read_indexes(&connection), indexes);

    drop(connection);
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
             VALUES (4, 'future', 'future-checksum', 'now')",
            [],
        )
        .unwrap();

    let error = migrate(&mut connection).unwrap_err();

    assert!(matches!(
        error,
        MigrationError::NewerSchema {
            found: 4,
            supported: 3
        }
    ));
}

/// Build a real pre-upgrade store from the unchanged v1/v2 scripts.
fn schema_v2(connection: &Connection) {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE schema_migrations (
                 version INTEGER PRIMARY KEY CHECK (version > 0),
                 name TEXT NOT NULL CHECK (length(name) > 0),
                 checksum TEXT NOT NULL CHECK (length(checksum) > 0),
                 applied_at TEXT NOT NULL CHECK (length(applied_at) > 0)
             );",
        )
        .unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../migrations/0002_immutable_events.sql"))
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO schema_migrations VALUES
             (1, 'initial', '6cce50f20fc62f82af5432d4e2be81c29a0f44a73ca5245353f98c6c6030f85e', 'v1-time'),
             (2, 'immutable_events', '1ccf8188fa3ab0c9adbd889c1d80762c9a0d5b3a03189609b204ee05a9c05ced', 'v2-time');",
        )
        .unwrap();
    seed_data(connection);
}

fn seed_data(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO projects VALUES ('bif', 'capture-time');
             INSERT INTO project_path_mappings VALUES ('/original/path', 'bif');
             INSERT INTO project_remote_mappings VALUES ('original-remote', 'bif');
             INSERT INTO requester_project_counters VALUES ('ALICE', 'bif', 2);
             INSERT INTO items VALUES
             ('ALICE:bif:001', 'ALICE', 'bif', 1, 'Original title', 'Description',
              'ready', 'P2', 'alice', 'Reason', 2, 'capture-time', 'approve-time');
             INSERT INTO item_acceptance_criteria VALUES
             ('ALICE:bif:001', 0, 'First criterion'), ('ALICE:bif:001', 1, 'Second criterion');
             INSERT INTO item_provenance VALUES
             ('ALICE:bif:001', 'delta', 'thread', 'message', 'url', 'repository', 'revision', 'context');
             INSERT INTO operations VALUES
             ('capture-op', 'ALICE:bif:001', 'capture', NULL, 1, 'capture-time'),
             ('approve-op', 'ALICE:bif:001', 'approve', 1, 2, 'approve-time');
             INSERT INTO events VALUES
             ('capture-event', 'capture-op', 'ALICE:bif:001', 1, 0, 'captured', NULL, 'proposed',
              'human', 'ALICE', 'cli', 'local', 'direct', NULL, 'cli', 'local',
              NULL, 'Captured note', 'capture-time', 1),
             ('approve-event', 'approve-op', 'ALICE:bif:001', 2, 0, 'approved', 'proposed', 'ready',
              'human', 'ALICE', 'cli', 'local', 'direct', NULL, 'cli', 'local',
              NULL, 'Approved note', 'approve-time', 1);
             INSERT INTO mutation_receipts VALUES
             ('capture-key', 'capture', 'capture-hash', 'capture-op', 'ALICE:bif:001',
              '{\"revision\":1}', 'capture-time'),
             ('approve-key', 'approve', 'approve-hash', 'approve-op', 'ALICE:bif:001',
              '{\"revision\":2}', 'approve-time');",
        )
        .unwrap();
}

/// Compare every persisted value, including opaque timestamps and receipts.
fn rows(connection: &Connection, sql: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = connection.prepare(sql).unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn durable_data(connection: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "store_metadata",
        "projects",
        "project_path_mappings",
        "project_remote_mappings",
        "requester_project_counters",
        "items",
        "item_acceptance_criteria",
        "item_provenance",
        "operations",
        "events",
        "mutation_receipts",
    ]
    .map(|table| rows(connection, &format!("SELECT * FROM {table} ORDER BY rowid")))
    .into()
}

fn read_indexes(connection: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    rows(
        connection,
        "SELECT name, sql FROM sqlite_schema
         WHERE type = 'index' AND name LIKE 'idx_items_read_%' ORDER BY name",
    )
}

#[test]
fn schema_v2_upgrade_preserves_all_data_and_repeat_startup_is_unchanged() {
    let (_directory, path) = temporary_database();
    let connection = Connection::open(&path).unwrap();
    schema_v2(&connection);
    let before = durable_data(&connection);
    let history = rows(
        &connection,
        "SELECT * FROM schema_migrations ORDER BY version",
    );
    let schema = rows(
        &connection,
        "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name",
    );
    drop(connection);

    let connection = open(&path).unwrap();
    assert_eq!(durable_data(&connection), before);
    assert_eq!(
        rows(
            &connection,
            "SELECT * FROM schema_migrations WHERE version < 3 ORDER BY version"
        ),
        history
    );
    assert_eq!(
        rows(
            &connection,
            "SELECT type, name, tbl_name, sql FROM sqlite_schema
             WHERE name NOT LIKE 'idx_items_read_%' ORDER BY name",
        ),
        schema
    );
    assert_eq!(read_indexes(&connection).len(), 4);
    let upgraded_history = rows(
        &connection,
        "SELECT * FROM schema_migrations ORDER BY version",
    );
    let upgraded_indexes = read_indexes(&connection);
    drop(connection);

    let connection = open(&path).unwrap();
    assert_eq!(durable_data(&connection), before);
    assert_eq!(
        rows(
            &connection,
            "SELECT * FROM schema_migrations ORDER BY version"
        ),
        upgraded_history
    );
    assert_eq!(read_indexes(&connection), upgraded_indexes);
    assert!(
        connection
            .execute("UPDATE events SET note = 'changed'", [])
            .is_err()
    );
    assert!(connection.execute("DELETE FROM events", []).is_err());
    assert_eq!(durable_data(&connection), before);
}

#[test]
fn every_registered_checksum_is_pinned_and_v3_tampering_is_rejected() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let history: Vec<(i64, String, String)> = connection
        .prepare("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(
        history[0],
        (
            1,
            "initial".into(),
            "6cce50f20fc62f82af5432d4e2be81c29a0f44a73ca5245353f98c6c6030f85e".into()
        )
    );
    assert_eq!(
        history[1],
        (
            2,
            "immutable_events".into(),
            "1ccf8188fa3ab0c9adbd889c1d80762c9a0d5b3a03189609b204ee05a9c05ced".into()
        )
    );
    assert_eq!(history[2].0, 3);
    assert_eq!(history[2].1, "projection_read_indexes");
    // Independently computed SHA256 of the exact shipped migration bytes.
    assert_eq!(
        history[2].2,
        "18e0648e7f4beed109743b2de3a0dfb6bd733ca1fbce593b8a8da0eb9032d3d5"
    );
    connection
        .execute(
            "UPDATE schema_migrations SET checksum = 'tampered' WHERE version = 3",
            [],
        )
        .unwrap();
    let indexes = read_indexes(&connection);
    assert!(matches!(
        migrate(&mut connection),
        Err(MigrationError::ChecksumMismatch { version: 3, ref found, .. }) if found == "tampered"
    ));
    assert_eq!(read_indexes(&connection), indexes);
}

#[test]
fn conflicting_index_and_injected_bookkeeping_failure_roll_back_upgrade() {
    for injected_failure in [false, true] {
        let mut connection = Connection::open_in_memory().unwrap();
        schema_v2(&connection);
        if injected_failure {
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_v3 BEFORE INSERT ON schema_migrations
                 WHEN NEW.version = 3 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
                )
                .unwrap();
        } else {
            // Conflict on the last index proves earlier CREATE INDEXs roll back.
            connection
                .execute_batch("CREATE INDEX idx_items_read_mine ON items(title);")
                .unwrap();
        }
        let data = durable_data(&connection);
        let history = rows(
            &connection,
            "SELECT * FROM schema_migrations ORDER BY version",
        );
        let schema = rows(
            &connection,
            "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name",
        );
        assert!(matches!(
            migrate(&mut connection),
            Err(MigrationError::Sqlite(_))
        ));
        assert_eq!(durable_data(&connection), data);
        assert_eq!(
            rows(
                &connection,
                "SELECT * FROM schema_migrations ORDER BY version"
            ),
            history
        );
        assert_eq!(
            rows(
                &connection,
                "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name"
            ),
            schema
        );
    }
}
