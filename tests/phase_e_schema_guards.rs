//! Direct SQL is test-only invalidation, not a supported writer or upgrade path.
#[allow(dead_code)]
mod mutation_journal_support;

use bif::{
    application::CaptureStoreError,
    domain::{ItemMutation, Triage},
    storage::{self, CaptureRepository, MutationRepository},
};
use mutation_journal_support::capture_support::{capture, capture_with_repository};
use mutation_journal_support::{compound, mutate, mutate_with_repository, persisted, request};

#[test]
fn schema_change_invalidates_capture_before_replay_or_new_write() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("schema.sqlite");
    let mut connection = storage::open(&path).unwrap();
    capture(&mut connection, "seed", "seed", "bif").unwrap();
    let observer = storage::open(&path).unwrap();
    let mut repository = CaptureRepository::new(&mut connection).unwrap();
    observer
        .execute_batch("CREATE TABLE test_schema_change (value TEXT)")
        .unwrap();
    let before = persisted(&observer);
    for key in ["seed", "new"] {
        assert!(matches!(
            capture_with_repository(&mut repository, key, "unused", "bif"),
            Err(CaptureStoreError::RestartRequired)
        ));
        assert_eq!(persisted(&observer), before);
    }
    drop(repository);
    assert!(connection.is_autocommit());
}

#[test]
fn schema_change_invalidates_mutation_before_replay_noop_or_effective_write() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("schema.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = capture(&mut connection, "seed", "seed", "bif")
        .unwrap()
        .item
        .id()
        .clone();
    let original = request(&id, "first", 1, compound());
    mutate(&mut connection, original.clone(), "first", 4).unwrap();
    let observer = storage::open(&path).unwrap();
    let mut repository = MutationRepository::new(&mut connection).unwrap();
    observer
        .execute_batch("CREATE TABLE test_schema_change (value TEXT)")
        .unwrap();
    let before = persisted(&observer);
    let noop = request(
        &id,
        "noop",
        2,
        ItemMutation {
            lifecycle: None,
            triage: Some(Triage::default()),
        },
    );
    let effective = request(
        &id,
        "effective",
        2,
        ItemMutation {
            lifecycle: None,
            triage: Some(Triage {
                note: Some("new".into()),
                ..Default::default()
            }),
        },
    );
    for request in [original, noop, effective] {
        assert_eq!(
            mutate_with_repository(&mut repository, request, "unused", 0)
                .unwrap_err()
                .code(),
            "restart_required"
        );
        assert_eq!(persisted(&observer), before);
    }
    drop(repository);
    assert!(connection.is_autocommit());
}
