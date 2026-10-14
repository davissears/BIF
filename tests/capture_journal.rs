//! Capture's operation, receipt, journal and H are one caller-owned transaction.
mod capture_journal_support;
mod support;

use bif::storage;
use capture_journal_support::{assert_complete_journal, capture, persisted};
use support::OwnedTestDirectory;

#[test]
fn success_reopen_replay_and_retained_receipt_after_pruning() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("capture.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let original_metadata = persisted(&connection);
    let first = capture(&mut connection, "key", "first", "bif").unwrap();
    assert!(!first.replayed);
    drop(connection);
    let mut connection = storage::open(&path).unwrap();
    assert_complete_journal(&connection, &[1]);
    // Only H and the journal allocator advance; identity, F and inactive/bootstrap
    // metadata remain exactly as initialized.
    let metadata = persisted(&connection);
    for table in ["store_metadata", "journal_metadata"] {
        let before = &original_metadata
            .iter()
            .find(|row| row.0 == table)
            .unwrap()
            .1;
        let mut expected = before.clone();
        if table == "journal_metadata" {
            expected[0][3] = rusqlite::types::Value::Integer(1);
        }
        assert_eq!(
            metadata.iter().find(|row| row.0 == table).unwrap().1,
            expected
        );
    }
    let before = persisted(&connection);
    let replay = capture(&mut connection, "key", "unused", "bif").unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.item.id(), first.item.id());
    assert_eq!(persisted(&connection), before);

    // Real pruning retains receipt/operation and H without refilling old links.
    let generation = storage::journal::JournalSnapshot::begin(&connection)
        .unwrap()
        .metadata()
        .generation
        .clone();
    storage::maintenance::prune(&path, &generation, 1).unwrap();
    drop(connection);
    let mut connection = storage::open(&path).unwrap();
    let before = persisted(&connection);
    assert!(
        capture(&mut connection, "key", "also-unused", "bif")
            .unwrap()
            .replayed
    );
    assert_eq!(
        persisted(&connection),
        before,
        "replay must not refill pruning"
    );
    let next = capture(&mut connection, "next", "next", "bif").unwrap();
    assert_eq!(next.item.id().sequence(), 2);
    assert_complete_journal(&connection, &[2]);
}

fn assert_injected_rollback(trigger: &str, existing_counter: bool) {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("rollback.sqlite");
    let mut connection = storage::open(&path).unwrap();
    if existing_counter {
        capture(&mut connection, "seed", "seed", "bif").unwrap();
        assert_complete_journal(&connection, &[1]);
    }
    let before = persisted(&connection);
    // ABORT only undoes the failing statement. The repository must return an
    // error and drop its outer transaction to undo project/counter/content too.
    connection.execute_batch(trigger).unwrap();
    let error = capture(&mut connection, "failed", "failed", "bif").unwrap_err();
    let bif::application::CaptureStoreError::Storage(storage::CaptureStorageError::Sqlite(sqlite)) =
        error
    else {
        panic!("injected ABORT must be a capture storage error");
    };
    assert_eq!(
        sqlite.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    assert!(sqlite.to_string().contains("injected capture"));
    assert!(
        connection.is_autocommit(),
        "outer transaction must be dropped"
    );
    drop(connection);
    let mut connection = storage::open(&path).unwrap();
    assert_eq!(
        persisted(&connection),
        before,
        "fresh reopen must preserve every value and allocator"
    );
    connection
        .execute_batch("DROP TRIGGER reject_capture")
        .unwrap();
    // Retrying the same failed key must create a new capture, not find a receipt.
    let retry = capture(&mut connection, "failed", "retry", "bif").unwrap();
    assert!(!retry.replayed);
    let sequence = if existing_counter { 2 } else { 1 };
    assert_eq!(retry.item.id().sequence(), sequence);
    let positions = if existing_counter {
        vec![1, 2]
    } else {
        vec![1]
    };
    assert_complete_journal(&connection, &positions);
}

#[test]
fn journal_insert_abort_rolls_back_fresh_project_and_counter_on_reopen() {
    assert_injected_rollback(
        "CREATE TRIGGER reject_capture BEFORE INSERT ON change_journal
         BEGIN SELECT RAISE(ABORT, 'injected capture journal insertion failure'); END;",
        false,
    );
}

#[test]
fn journal_insert_abort_preserves_existing_counter_and_all_values_on_reopen() {
    assert_injected_rollback(
        "CREATE TRIGGER reject_capture BEFORE INSERT ON change_journal
         BEGIN SELECT RAISE(ABORT, 'injected capture journal insertion failure'); END;",
        true,
    );
}

#[test]
fn high_water_abort_rolls_back_fresh_project_and_counter_on_reopen() {
    assert_injected_rollback(
        "CREATE TRIGGER reject_capture BEFORE UPDATE OF high_water ON journal_metadata
         BEGIN SELECT RAISE(ABORT, 'injected capture H update failure'); END;",
        false,
    );
}

#[test]
fn high_water_abort_preserves_existing_counter_and_all_values_on_reopen() {
    assert_injected_rollback(
        "CREATE TRIGGER reject_capture BEFORE UPDATE OF high_water ON journal_metadata
         BEGIN SELECT RAISE(ABORT, 'injected capture H update failure'); END;",
        true,
    );
}
