//! Prune only advances global coverage; SQLite owns publication and snapshots.
#[allow(dead_code)]
mod maintenance_support;

use std::num::NonZeroU32;

use bif::{
    domain::ProjectId,
    storage::{
        self,
        journal::{Generation, JournalError, JournalSnapshot},
        maintenance,
    },
};
use maintenance_support::{Fixture, capture, generation, inventory, read};
use rusqlite::types::Value;

fn binding(connection: &rusqlite::Connection) -> Generation {
    Generation::parse(&generation(connection)).unwrap()
}

fn positions(
    snapshot: &JournalSnapshot<'_>,
    generation: &Generation,
    project: &str,
    c: i64,
) -> Result<Vec<i64>, JournalError> {
    Ok(snapshot
        .select_range(
            generation,
            &ProjectId::new(project).unwrap(),
            c,
            snapshot.metadata().high_water,
            NonZeroU32::new(100).unwrap(),
        )?
        .into_iter()
        .map(|row| row.change_seq)
        .collect())
}

#[test]
fn none_some_all_preserve_every_other_value_and_reopen_append_above_committed_h() {
    let fixture = Fixture::new();
    let generation = binding(&read(&fixture.store));
    for through in [0, 1, 1, 2] {
        let connection = read(&fixture.store);
        let mut expected = inventory(&connection);
        let rows = &mut expected
            .iter_mut()
            .find(|table| table.0 == "change_journal")
            .unwrap()
            .1;
        rows.retain(|row| matches!(row[0], Value::Integer(sequence) if sequence > through));
        expected
            .iter_mut()
            .find(|table| table.0 == "journal_metadata")
            .unwrap()
            .1[0][2] = Value::Integer(through);
        drop(connection);
        maintenance::prune(&fixture.store, &generation, through).unwrap();
        assert_eq!(inventory(&read(&fixture.store)), expected);
        let connection = storage::open(&fixture.store).unwrap();
        let snapshot = JournalSnapshot::begin(&connection).unwrap();
        assert_eq!(snapshot.metadata().generation, generation);
        assert_eq!(snapshot.metadata().retention_floor, through);
        assert_eq!(snapshot.metadata().high_water, 2);
        assert_eq!(
            positions(&snapshot, &generation, "bif", through).unwrap(),
            ((through + 1)..=2).collect::<Vec<_>>()
        );
    }
    let mut connection = storage::open(&fixture.store).unwrap();
    capture(&mut connection, "three", "after prune");
    let snapshot = JournalSnapshot::begin(&connection).unwrap();
    assert_eq!(snapshot.metadata().high_water, 3);
    assert_eq!(snapshot.metadata().retention_floor, 2);
    assert_eq!(positions(&snapshot, &generation, "bif", 2).unwrap(), [3]);
    snapshot.finish().unwrap();
    // The intentionally pruned operations still exist; retained-only validation
    // must not demand links for those old audit records.
    maintenance::prune(&fixture.store, &generation, 2).unwrap();
}

#[test]
fn wrong_generation_regressive_negative_and_future_bounds_preserve_persisted_state() {
    let fixture = Fixture::new();
    let generation = binding(&read(&fixture.store));
    maintenance::prune(&fixture.store, &generation, 1).unwrap();
    let before = inventory(&read(&fixture.store));
    let stale = Generation::parse(
        if generation.as_str() == "00000000000000000000000000000000" {
            "11111111111111111111111111111111"
        } else {
            "00000000000000000000000000000000"
        },
    )
    .unwrap();
    for (expected, through) in [
        (&stale, 1),
        (&generation, 0),
        (&generation, -1),
        (&generation, 3),
        (&generation, i64::MAX),
    ] {
        assert!(maintenance::prune(&fixture.store, expected, through).is_err());
        assert_eq!(inventory(&read(&fixture.store)), before);
    }
}

#[test]
fn wal_old_snapshot_keeps_identity_floor_high_water_and_rows_while_fresh_readers_expire() {
    for prune_first in [false, true] {
        let fixture = Fixture::new();
        let reader = storage::open(&fixture.store).unwrap();
        assert_eq!(
            reader
                .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        let generation = binding(&reader);
        if prune_first {
            maintenance::prune(&fixture.store, &generation, 1).unwrap();
        }
        let old = JournalSnapshot::begin(&reader).unwrap();
        if !prune_first {
            maintenance::prune(&fixture.store, &generation, 1).unwrap();
            assert_eq!(old.metadata().generation, generation);
            assert_eq!(old.metadata().retention_floor, 0);
            assert_eq!(old.metadata().high_water, 2);
            assert_eq!(positions(&old, &generation, "bif", 0).unwrap(), [1, 2]);
        } else {
            assert!(matches!(
                positions(&old, &generation, "bif", 0),
                Err(JournalError::ExpiredCoverage)
            ));
        }
        old.finish().unwrap();
        let fresh = JournalSnapshot::begin(&reader).unwrap();
        assert_eq!(fresh.metadata().generation, generation);
        assert_eq!(fresh.metadata().retention_floor, 1);
        assert_eq!(fresh.metadata().high_water, 2);
        for project in ["bif", "other", "empty"] {
            assert!(matches!(
                positions(&fresh, &generation, project, 0),
                Err(JournalError::ExpiredCoverage)
            ));
            let expected = if project == "bif" { vec![2] } else { vec![] };
            assert_eq!(
                positions(&fresh, &generation, project, 1).unwrap(),
                expected
            );
        }
    }
}

#[test]
fn invalid_retained_links_gaps_allocator_schema_and_metadata_are_not_repaired() {
    for damage in [
        "DELETE FROM change_journal WHERE change_seq = 2",
        "UPDATE operations SET item_revision = 9 WHERE operation_id = 'two'",
        "UPDATE sqlite_sequence SET seq = 3 WHERE name = 'change_journal'",
        "UPDATE journal_metadata SET generation = 'bad'",
        "CREATE TABLE unsupported (value TEXT)",
        "DELETE FROM schema_migrations WHERE version = 4",
    ] {
        let fixture = Fixture::new();
        let connection = storage::open(&fixture.store).unwrap();
        let generation = binding(&connection);
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;")
            .unwrap();
        connection.execute_batch(damage).unwrap();
        drop(connection);
        let before = inventory(&read(&fixture.store));
        assert!(
            maintenance::prune(&fixture.store, &generation, 2).is_err(),
            "{damage}"
        );
        assert_eq!(inventory(&read(&fixture.store)), before, "{damage}");
    }
}

#[test]
fn empty_store_equal_floor_missing_path_and_busy_writer_fail_closed() {
    let fixture = Fixture::new();
    let path = fixture.path("empty.sqlite");
    let connection = storage::open(&path).unwrap();
    let generation = binding(&connection);
    let before = inventory(&connection);
    maintenance::prune(&path, &generation, 0).unwrap();
    assert_eq!(inventory(&read(&path)), before);
    let missing = fixture.path("missing.sqlite");
    assert!(maintenance::prune(&missing, &generation, 0).is_err());
    assert!(!missing.exists());
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let error = maintenance::prune(&path, &generation, 0).unwrap_err();
    assert!(matches!(error, maintenance::MaintenanceError::Sqlite(error)
        if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)));
    assert!(
        !connection.is_autocommit(),
        "prune cannot own another connection's transaction"
    );
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(inventory(&read(&path)), before);
}
