mod support;

use std::path::Path;

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureIdentity, CaptureInput, CaptureRequest,
        Clock, Command, Execution, IdentityGenerator, ObservedExecution, capture,
    },
    domain::{ItemContent, ProjectId, Provenance, RequesterId, Timestamp},
    storage::{self, CaptureRepository},
};
use rusqlite::{Connection, OpenFlags, backup::Backup, types::Value};
use support::OwnedTestDirectory;

struct Inputs<'a>(&'a str);

impl Clock for Inputs<'_> {
    fn now(&mut self) -> Timestamp {
        Timestamp::new("2026-01-01T00:00:00Z")
    }
}

impl IdentityGenerator for Inputs<'_> {
    fn capture_identity(&mut self) -> CaptureIdentity {
        CaptureIdentity {
            operation_id: format!("rehearsal-operation-{}", self.0),
            event_id: format!("rehearsal-event-{}", self.0),
        }
    }
}

/// Production capture creates durable audit history and an idempotency receipt.
fn write(connection: &mut Connection, key: &str) {
    capture(
        &mut CaptureRepository::new(connection),
        &mut Inputs("clock"),
        &mut Inputs(key),
        &AuthorizationRequest {
            actor: Actor {
                kind: ActorKind::Human,
                id: "rehearsal",
                surface: "cli",
                host: "local",
            },
            execution: Execution::Direct {
                surface: "cli",
                host: "local",
            },
            observed_execution: ObservedExecution::Direct,
            command: Command::Capture,
            human_authorization: Some(bif::application::HumanAuthorization::Direct {
                trusted: true,
            }),
        },
        CaptureRequest {
            idempotency_key: key.into(),
            input: CaptureInput {
                requester: RequesterId::new("REHEARSAL").unwrap(),
                project: ProjectId::new("bif").unwrap(),
                content: ItemContent::new("Before upgrade", None, vec!["Preserve receipt".into()])
                    .unwrap(),
                provenance: Provenance::default(),
            },
        },
    )
    .unwrap();
}

/// Restore always targets a new file in an independently owned root.
fn online_backup(source: &Connection, destination: &Path) {
    assert!(!destination.exists());
    let mut target = Connection::open(destination).unwrap();
    Backup::new(source, &mut target)
        .unwrap()
        .run_to_completion(8, std::time::Duration::from_millis(1), None)
        .unwrap();
}

fn rows(connection: &Connection, sql: &str) -> Vec<Vec<Value>> {
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

/// Snapshot every application table, including registrations and opaque receipts.
fn durable_state(connection: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
    let tables: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table'
             AND name NOT LIKE 'sqlite_%' AND name != 'schema_migrations' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let data = rows(
                connection,
                &format!(
                    "SELECT * FROM \"{}\" ORDER BY rowid",
                    table.replace('"', "\"\"")
                ),
            );
            (table, data)
        })
        .collect()
}

fn verify(connection: &Connection, version: i64) {
    assert_eq!(
        rows(connection, "PRAGMA integrity_check"),
        vec![vec![Value::Text("ok".into())]]
    );
    assert!(rows(connection, "PRAGMA foreign_key_check").is_empty());
    assert_eq!(
        rows(connection, "SELECT max(version) FROM schema_migrations"),
        vec![vec![Value::Integer(version)]]
    );
}

#[test]
fn online_backup_upgrade_and_separate_root_rollback_preserve_schema_two() {
    let original_root = OwnedTestDirectory::new();
    let backup_root = OwnedTestDirectory::new();
    let candidate_root = OwnedTestDirectory::new();
    let rollback_root = OwnedTestDirectory::new();
    let original_path = original_root.path().join("bif.sqlite3");
    let backup_path = backup_root.path().join("bif.sqlite3");
    let candidate_path = candidate_root.path().join("bif.sqlite3");
    let rollback_path = rollback_root.path().join("bif.sqlite3");

    // No current migration followed by DELETE/downmigration: use untouched scripts
    // and the historical checksums that production migration 3 must accept.
    let mut original = Connection::open(&original_path).unwrap();
    original
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE schema_migrations (
                 version INTEGER PRIMARY KEY CHECK (version > 0),
                 name TEXT NOT NULL CHECK (length(name) > 0),
                 checksum TEXT NOT NULL CHECK (length(checksum) > 0),
                 applied_at TEXT NOT NULL CHECK (length(applied_at) > 0)
             );",
        )
        .unwrap();
    original
        .execute_batch(include_str!("../migrations/0001_initial.sql"))
        .unwrap();
    original
        .execute_batch(include_str!("../migrations/0002_immutable_events.sql"))
        .unwrap();
    original.execute_batch(
        "INSERT INTO schema_migrations VALUES
         (1, 'initial', '6cce50f20fc62f82af5432d4e2be81c29a0f44a73ca5245353f98c6c6030f85e', 'v1-time'),
         (2, 'immutable_events', '1ccf8188fa3ab0c9adbd889c1d80762c9a0d5b3a03189609b204ee05a9c05ced', 'v2-time');",
    ).unwrap();
    write(&mut original, "pre-upgrade-key");
    write(&mut original, "pre-upgrade-key"); // A retry must not duplicate history.
    verify(&original, 2);
    assert_eq!(
        rows(&original, "SELECT count(*) FROM events"),
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        rows(&original, "SELECT count(*) FROM mutation_receipts"),
        vec![vec![Value::Integer(1)]]
    );
    let before = durable_state(&original);
    let schema = rows(
        &original,
        "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name",
    );
    let migrations = rows(
        &original,
        "SELECT * FROM schema_migrations ORDER BY version",
    );
    let identity = storage::read_store_identity(&original).unwrap();

    // Source remains open with WAL commits: a plain file copy is not the recipe.
    assert!(
        original_path
            .with_file_name("bif.sqlite3-wal")
            .metadata()
            .unwrap()
            .len()
            > 0
    );
    online_backup(&original, &backup_path);
    let pristine =
        Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    verify(&pristine, 2);
    assert_eq!(durable_state(&pristine), before);
    online_backup(&pristine, &candidate_path);

    let mut candidate = storage::open(&candidate_path).unwrap();
    verify(&candidate, 3);
    assert_eq!(storage::read_store_identity(&candidate).unwrap(), identity);
    assert_eq!(durable_state(&candidate), before);
    assert_eq!(
        rows(
            &candidate,
            "SELECT * FROM schema_migrations WHERE version < 3 ORDER BY version"
        ),
        migrations
    );
    assert_eq!(
        rows(
            &candidate,
            "SELECT type, name, tbl_name, sql FROM sqlite_schema WHERE name NOT LIKE 'idx_items_read_%' ORDER BY name"
        ),
        schema
    );
    write(&mut candidate, "candidate-only-key");
    verify(&candidate, 3);
    assert_ne!(durable_state(&candidate), before);
    drop(candidate);

    // Rollback deliberately loses the candidate-only write. It neither replaces
    // an active store nor runs a downmigration; current BIF must not reopen it.
    assert_eq!(durable_state(&pristine), before);
    online_backup(&pristine, &rollback_path);
    let rollback =
        Connection::open_with_flags(&rollback_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    verify(&rollback, 2);
    assert_eq!(storage::read_store_identity(&rollback).unwrap(), identity);
    assert_eq!(durable_state(&rollback), before);
    assert_eq!(
        rows(
            &rollback,
            "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name"
        ),
        schema
    );
    assert_eq!(
        rows(
            &rollback,
            "SELECT * FROM schema_migrations ORDER BY version"
        ),
        migrations
    );
    assert_eq!(durable_state(&original), before);
    verify(&original, 2);
    // Gap: this verifies raw SQLite reopening, not a historical BIF executable's
    // schema-2 startup/read/write workflow. That remains an operator rehearsal.
}
