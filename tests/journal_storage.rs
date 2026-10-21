mod support;

use std::num::NonZeroU32;

use bif::{
    domain::ProjectId,
    storage::{
        self,
        journal::{Generation, JournalError, JournalSnapshot, append_change},
    },
};
use rusqlite::{Connection, TransactionBehavior};
use support::OwnedTestDirectory;

fn metadata(connection: &Connection) -> (String, i64, i64) {
    connection
        .query_row(
            "SELECT generation, retention_floor, high_water FROM journal_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn seed(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO projects VALUES ('bif', 'time'), ('other', 'time');
         INSERT INTO items (item_id, requester, project_id, sequence, title, status,
          revision, captured_at, updated_at) VALUES
          ('ALICE:bif:001', 'ALICE', 'bif', 1, 'Title', 'proposed', 3, 'time', 'time'),
          ('ALICE:other:001', 'ALICE', 'other', 1, 'Title', 'proposed', 1, 'time', 'time');
         INSERT INTO operations VALUES
          ('op-1', 'ALICE:bif:001', 'capture', NULL, 1, 'time'),
          ('op-2', 'ALICE:other:001', 'capture', NULL, 1, 'time'),
          ('op-3', 'ALICE:bif:001', 'triage', 1, 2, 'time'),
          ('op-4', 'ALICE:bif:001', 'triage', 2, 3, 'time');",
        )
        .unwrap();
}

fn append(connection: &mut Connection, operation: &str) -> i64 {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let sequence = append_change(&transaction, operation).unwrap();
    transaction.commit().unwrap();
    sequence
}

fn selected(
    snapshot: &JournalSnapshot<'_>,
    generation: &Generation,
    c: i64,
    h: i64,
) -> Result<Vec<i64>, JournalError> {
    Ok(snapshot
        .select_range(
            generation,
            &ProjectId::new("bif").unwrap(),
            c,
            h,
            NonZeroU32::new(100).unwrap(),
        )?
        .into_iter()
        .map(|change| change.change_seq)
        .collect())
}

#[test]
fn append_is_operation_derived_unique_and_never_commits_the_caller() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("journal.sqlite");
    let mut writer = storage::open(&path).unwrap();
    let reader = storage::open(&path).unwrap();
    seed(&writer);
    let transaction = writer.transaction().unwrap();
    assert_eq!(append_change(&transaction, "op-1").unwrap(), 1);
    assert_eq!(metadata(&transaction).2, 1);
    assert_eq!(metadata(&reader).2, 0, "helper must not commit");
    assert!(append_change(&transaction, "op-1").is_err());
    assert!(append_change(&transaction, "missing-operation").is_err());
    assert_eq!(metadata(&transaction).2, 1);
    transaction.commit().unwrap();
    assert_eq!(metadata(&reader).2, 1);
    let row: (String, String, String, i64) = reader
        .query_row(
            "SELECT operation_id, item_id, project_id, item_revision FROM change_journal",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        ("op-1".into(), "ALICE:bif:001".into(), "bif".into(), 1)
    );
    let transaction = writer.transaction().unwrap();
    assert_eq!(append_change(&transaction, "op-2").unwrap(), 2);
    transaction.rollback().unwrap();
    assert_eq!(metadata(&reader).2, 1);
    assert_eq!(
        append(&mut writer, "op-2"),
        2,
        "rolled-back allocations may be reused"
    );
}

#[test]
fn failed_high_water_update_aborts_the_row_even_if_caller_commits() {
    let mut connection = storage::open(":memory:").unwrap();
    seed(&connection);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_high_water BEFORE UPDATE ON journal_metadata
         BEGIN SELECT RAISE(ABORT, 'injected high-water failure'); END;",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    assert!(append_change(&transaction, "op-1").is_err());
    transaction.commit().unwrap();
    assert_eq!(metadata(&connection).2, 0);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM change_journal", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn operation_audit_receipt_and_journal_rollback_together() {
    let mut connection = storage::open(":memory:").unwrap();
    seed(&connection);
    let transaction = connection.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO operations VALUES ('new-op', 'ALICE:bif:001', 'triage', 3, 4, 'time')",
            [],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO events VALUES
         ('new-event', 'new-op', 'ALICE:bif:001', 4, 0, 'note_added', NULL, NULL,
          'human', 'ALICE', 'cli', 'local', 'direct', NULL, 'cli', 'local',
          NULL, 'note', 'time', 1)",
            [],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO mutation_receipts VALUES
         ('new-key', 'triage', 'hash', 'new-op', 'ALICE:bif:001', '{}', 'time')",
            [],
        )
        .unwrap();
    append_change(&transaction, "new-op").unwrap();
    transaction.rollback().unwrap();
    for table in [
        "operations",
        "events",
        "mutation_receipts",
        "change_journal",
    ] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE operation_id = 'new-op'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    assert_eq!(metadata(&connection).2, 0);
}

#[test]
fn prune_all_reopen_preserves_high_water_and_committed_positions_are_not_reused() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("journal.sqlite");
    let mut connection = storage::open(&path).unwrap();
    seed(&connection);
    assert_eq!(append(&mut connection, "op-1"), 1);
    assert_eq!(append(&mut connection, "op-2"), 2);
    let generation = metadata(&connection).0;
    storage::maintenance::prune(&path, &Generation::parse(&generation).unwrap(), 2).unwrap();
    drop(connection);
    let mut connection = storage::open(&path).unwrap();
    assert_eq!(metadata(&connection), (generation.clone(), 2, 2));
    assert_eq!(append(&mut connection, "op-3"), 3);
    assert_eq!(metadata(&connection), (generation, 2, 3));
    let snapshot = JournalSnapshot::begin(&connection).unwrap();
    let generation = snapshot.metadata().generation.clone();
    assert_eq!(selected(&snapshot, &generation, 2, 3).unwrap(), vec![3]);
    assert!(matches!(
        selected(&snapshot, &generation, 1, 3),
        Err(JournalError::ExpiredCoverage)
    ));
}

#[test]
fn signed_allocator_limit_fails_closed_before_insert_without_random_rowid_fallback() {
    let mut connection = storage::open(":memory:").unwrap();
    seed(&connection);
    append(&mut connection, "op-1");
    connection
        .execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'change_journal'",
            [i64::MAX - 1],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE journal_metadata SET high_water = ?1",
            [i64::MAX - 1],
        )
        .unwrap();
    assert_eq!(append(&mut connection, "op-2"), i64::MAX);
    connection
        .execute_batch(
            "DELETE FROM change_journal; UPDATE journal_metadata SET retention_floor = high_water;",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    assert!(matches!(
        append_change(&transaction, "op-3"),
        Err(JournalError::SequenceExhausted)
    ));
    transaction.commit().unwrap();
    assert_eq!(metadata(&connection).2, i64::MAX);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM change_journal", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn malformed_metadata_fails_closed_on_snapshot_append_and_reopen_without_repair() {
    for damage in [
        "DELETE FROM journal_metadata",
        "UPDATE journal_metadata SET generation = 'bad'",
        "UPDATE journal_metadata SET retention_floor = 1",
        "UPDATE journal_metadata SET high_water = -1, generation = '33333333333333333333333333333333'",
        "UPDATE journal_metadata SET bootstrap_required = 7",
        "UPDATE journal_metadata SET singleton = 2",
    ] {
        let root = OwnedTestDirectory::new();
        let path = root.path().join("journal.sqlite");
        let mut connection = storage::open(&path).unwrap();
        seed(&connection);
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        connection.execute_batch(damage).unwrap();
        assert!(JournalSnapshot::begin(&connection).is_err(), "{damage}");
        let transaction = connection.transaction().unwrap();
        assert!(append_change(&transaction, "op-1").is_err(), "{damage}");
        transaction.rollback().unwrap();
        drop(connection);
        assert!(storage::open(&path).is_err(), "{damage}");
    }
}

#[test]
fn metadata_constraints_enforce_global_floor_and_high_water_monotonicity() {
    let mut connection = storage::open(":memory:").unwrap();
    seed(&connection);
    append(&mut connection, "op-1");
    append(&mut connection, "op-2");
    connection
        .execute("UPDATE journal_metadata SET retention_floor = 1", [])
        .unwrap();
    for invalid in [
        "UPDATE journal_metadata SET retention_floor = 0",
        "UPDATE journal_metadata SET high_water = 1",
        "UPDATE journal_metadata SET retention_floor = 3",
        "UPDATE journal_metadata SET generation = upper(generation)",
        "UPDATE journal_metadata SET generation = 'zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz'",
    ] {
        assert!(connection.execute(invalid, []).is_err(), "{invalid}");
    }
    assert_eq!((metadata(&connection).1, metadata(&connection).2), (1, 2));
}

#[test]
fn range_validation_uses_global_coverage_even_for_empty_project_and_allows_floor_equality() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("coverage.sqlite");
    let mut connection = storage::open(&path).unwrap();
    seed(&connection);
    append(&mut connection, "op-1");
    append(&mut connection, "op-2");
    append(&mut connection, "op-3");
    let generation = Generation::parse(&metadata(&connection).0).unwrap();
    storage::maintenance::prune(&path, &generation, 1).unwrap();
    let snapshot = JournalSnapshot::begin(&connection).unwrap();
    let generation = snapshot.metadata().generation.clone();
    assert_eq!(selected(&snapshot, &generation, 1, 3).unwrap(), vec![3]);
    let row = snapshot
        .select_range(
            &generation,
            &ProjectId::new("other").unwrap(),
            1,
            3,
            NonZeroU32::new(1).unwrap(),
        )
        .unwrap();
    assert_eq!(row[0].change_seq, 2);
    assert_eq!(row[0].item_revision, 1);
    assert!(
        snapshot
            .select_range(
                &generation,
                &ProjectId::new("empty").unwrap(),
                1,
                3,
                NonZeroU32::new(1).unwrap()
            )
            .unwrap()
            .is_empty()
    );
    for (c, h) in [(-1, 3), (3, 2), (1, 4)] {
        assert!(matches!(
            selected(&snapshot, &generation, c, h),
            Err(JournalError::InvalidCoverage)
        ));
    }
    assert!(matches!(
        selected(&snapshot, &generation, 0, 3),
        Err(JournalError::ExpiredCoverage)
    ));
    let stale = Generation::parse("11111111111111111111111111111111").unwrap();
    assert!(matches!(
        selected(&snapshot, &stale, 1, 3),
        Err(JournalError::GenerationMismatch)
    ));
    snapshot.finish().unwrap();
    assert!(connection.is_autocommit());
}

#[test]
fn wal_snapshot_keeps_complete_old_set_across_pruning_or_rotation_in_both_orders() {
    for rotation in [false, true] {
        for maintenance_first in [false, true] {
            let root = OwnedTestDirectory::new();
            let path = root.path().join("journal.sqlite");
            let reader = storage::open(&path).unwrap();
            let mut writer = storage::open(&path).unwrap();
            for connection in [&reader, &writer] {
                assert_eq!(
                    connection
                        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                        .unwrap(),
                    "wal"
                );
                assert_eq!(
                    connection
                        .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
                        .unwrap(),
                    2
                );
            }
            seed(&writer);
            for operation in ["op-1", "op-2", "op-3", "op-4"] {
                append(&mut writer, operation);
            }
            let generation = Generation::parse(&metadata(&writer).0).unwrap();
            let maintain = |writer: &mut Connection| {
                if !rotation {
                    storage::maintenance::prune(&path, &generation, 4).unwrap();
                    return;
                }
                let transaction = writer
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                transaction
                    .execute("DELETE FROM change_journal", [])
                    .unwrap();
                transaction.execute(
                    "UPDATE journal_metadata SET generation = ?1, retention_floor = 0, high_water = 0",
                    ["22222222222222222222222222222222"],
                ).unwrap();
                transaction.commit().unwrap();
            };
            if maintenance_first {
                maintain(&mut writer);
            }
            let snapshot = JournalSnapshot::begin(&reader).unwrap(); // metadata SELECT establishes it
            if !maintenance_first {
                assert_eq!(snapshot.metadata().high_water, 4);
                maintain(&mut writer);
                assert_eq!(
                    selected(&snapshot, &generation, 0, 4).unwrap(),
                    vec![1, 3, 4]
                );
            } else {
                let result = selected(&snapshot, &generation, 0, 4);
                assert!(if rotation {
                    matches!(result, Err(JournalError::GenerationMismatch))
                } else {
                    matches!(result, Err(JournalError::ExpiredCoverage))
                });
            }
            snapshot.finish().unwrap();
            let fresh = JournalSnapshot::begin(&reader).unwrap();
            let result = selected(&fresh, &generation, 0, 4);
            assert!(if rotation {
                matches!(result, Err(JournalError::GenerationMismatch))
            } else {
                matches!(result, Err(JournalError::ExpiredCoverage))
            });
        }
    }
}

#[test]
fn snapshot_reuses_outer_transaction_and_does_not_commit_it() {
    let mut connection = storage::open(":memory:").unwrap();
    seed(&connection);
    let transaction = connection.transaction().unwrap();
    append_change(&transaction, "op-1").unwrap();
    let snapshot = JournalSnapshot::begin(&transaction).unwrap();
    let generation = snapshot.metadata().generation.clone();
    assert_eq!(selected(&snapshot, &generation, 0, 1).unwrap(), vec![1]);
    snapshot.finish().unwrap();
    assert!(!transaction.is_autocommit());
    transaction.rollback().unwrap();
    assert_eq!(metadata(&connection).2, 0);
}

#[test]
fn sync_guard_checks_supplied_identity_before_availability_and_initial_scan_fails_closed() {
    let connection = storage::open(":memory:").unwrap();
    let id = storage::read_store_identity(&connection).unwrap();
    let snapshot = JournalSnapshot::begin(&connection).unwrap();
    let generation = snapshot.metadata().generation.clone();
    let wrong = Generation::parse("11111111111111111111111111111111").unwrap();
    assert!(matches!(
        snapshot.require_sync(None),
        Err(JournalError::SyncDisabled)
    ));
    assert!(matches!(
        snapshot.require_sync(Some((&id, &generation))),
        Err(JournalError::SyncDisabled)
    ));
    assert!(matches!(
        snapshot.require_sync(Some(("wrong", &wrong))),
        Err(JournalError::StoreMismatch)
    ));
    assert!(matches!(
        snapshot.require_sync(Some((&id, &wrong))),
        Err(JournalError::GenerationMismatch)
    ));
    snapshot.finish().unwrap();
    connection
        .execute("UPDATE journal_metadata SET sync_enabled = 1", [])
        .unwrap();
    let snapshot = JournalSnapshot::begin(&connection).unwrap();
    snapshot.require_sync(None).unwrap();
    snapshot.require_sync(Some((&id, &generation))).unwrap();
    assert!(matches!(
        snapshot.require_sync(Some(("wrong", &generation))),
        Err(JournalError::StoreMismatch)
    ));
    assert!(matches!(
        snapshot.require_sync(Some((&id, &wrong))),
        Err(JournalError::GenerationMismatch)
    ));
}

#[test]
fn sync_guard_identity_availability_coverage_and_rows_share_the_callers_snapshot() {
    for outer in [false, true] {
        for enabled_first in [false, true] {
            let root = OwnedTestDirectory::new();
            let path = root.path().join("guard.sqlite");
            let reader = storage::open(&path).unwrap();
            let mut writer = storage::open(&path).unwrap();
            seed(&writer);
            append(&mut writer, "op-1");
            let id = storage::read_store_identity(&reader).unwrap();
            let generation = Generation::parse(&metadata(&reader).0).unwrap();
            if enabled_first {
                storage::maintenance::activate(&path, &generation).unwrap();
            }
            if outer {
                reader.execute_batch("BEGIN").unwrap();
                // Establish the caller's snapshot before availability changes.
                assert_eq!(storage::read_store_identity(&reader).unwrap(), id);
            }
            let snapshot = JournalSnapshot::begin(&reader).unwrap();
            let transaction = writer
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            // Simulate an atomic identity/availability/coverage transition, to
            // deterministically interleave between begin and guard/row selection.
            transaction
                .execute_batch(
                    "UPDATE store_metadata SET store_id = 'replacement-store';
                 DELETE FROM change_journal;
                 UPDATE journal_metadata SET generation = '22222222222222222222222222222222',
                   retention_floor = 1, sync_enabled = 1 - sync_enabled;",
                )
                .unwrap();
            transaction.commit().unwrap();
            let result = snapshot.require_sync(Some((&id, &generation)));
            if enabled_first {
                result.unwrap();
                snapshot.require_sync(None).unwrap();
            } else {
                assert!(matches!(result, Err(JournalError::SyncDisabled)));
                assert!(matches!(
                    snapshot.require_sync(None),
                    Err(JournalError::SyncDisabled)
                ));
            }
            assert_eq!(snapshot.store_id(), id);
            assert_eq!(snapshot.metadata().retention_floor, 0);
            assert_eq!(selected(&snapshot, &generation, 0, 1).unwrap(), vec![1]);
            snapshot.finish().unwrap();
            if outer {
                assert!(!reader.is_autocommit());
                reader.execute_batch("ROLLBACK").unwrap();
            }
            let fresh = JournalSnapshot::begin(&reader).unwrap();
            assert!(matches!(
                fresh.require_sync(Some((&id, &generation))),
                Err(JournalError::StoreMismatch)
            ));
            assert!(matches!(
                fresh.require_sync(Some(("replacement-store", &generation))),
                Err(JournalError::GenerationMismatch)
            ));
            assert_eq!(fresh.store_id(), "replacement-store");
            assert_eq!(fresh.metadata().retention_floor, 1);
            if enabled_first {
                assert!(matches!(
                    fresh.require_sync(None),
                    Err(JournalError::SyncDisabled)
                ));
            } else {
                fresh.require_sync(None).unwrap();
            }
        }
    }
}
