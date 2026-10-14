//! Effective mutations append once; no-op/replay/error paths preserve all state.
mod mutation_journal_support;
mod support;

use bif::{
    domain::{AssigneeId, ItemMutation, LifecycleMutation, Priority, Triage, TriageField},
    storage::{self, MutationRepository, journal::JournalError},
};
use mutation_journal_support::{
    assert_coverage, assert_mutation, compound, mutate, mutate_with_repository, persisted, request,
    seed,
};
use support::OwnedTestDirectory;

#[test]
fn every_lifecycle_transition_has_one_complete_journal_link() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("lifecycle.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = seed(&mut connection);
    let actions = [
        (LifecycleMutation::Approve, "approved"),
        (LifecycleMutation::Start, "started"),
        (
            LifecycleMutation::Block {
                reason: "waiting".into(),
            },
            "blocked",
        ),
        (LifecycleMutation::Resume, "resumed"),
        (LifecycleMutation::Finish, "finished"),
    ];
    let mut requests = Vec::new();
    for (index, (action, event)) in actions.into_iter().enumerate() {
        let key = format!("lifecycle-{index}");
        let request = request(
            &id,
            &key,
            index as u64 + 1,
            ItemMutation {
                lifecycle: Some(action),
                triage: None,
            },
        );
        let result = mutate(&mut connection, request.clone(), &key, 1).unwrap();
        assert!(!result.replayed);
        assert_eq!(result.item.revision().get(), index as u64 + 2);
        requests.push((request, key, event));
    }
    drop(connection);
    let connection = storage::open(&path).unwrap();
    for (index, (request, key, event)) in requests.iter().enumerate() {
        assert_mutation(&connection, request, key, index as i64 + 2, &[event]);
    }
    assert_coverage(&connection, &[1, 2, 3, 4, 5, 6]);

    // Rejection is the other proposed-item branch, not a transition out of done.
    let path = root.path().join("reject.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = seed(&mut connection);
    let request = request(
        &id,
        "reject",
        1,
        ItemMutation {
            lifecycle: Some(LifecycleMutation::Reject {
                reason: "not needed".into(),
            }),
            triage: None,
        },
    );
    mutate(&mut connection, request.clone(), "reject", 1).unwrap();
    drop(connection);
    let connection = storage::open(&path).unwrap();
    assert_mutation(&connection, &request, "reject", 2, &["rejected"]);
    assert_coverage(&connection, &[1, 2]);
}

#[test]
fn priority_assignee_set_clear_compound_and_note_only_each_append_once() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("triage.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = seed(&mut connection);
    let mutations = [
        (
            compound(),
            vec![
                "approved",
                "priority_changed",
                "assignee_changed",
                "note_added",
            ],
        ),
        (
            ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    priority: TriageField::Clear,
                    ..Default::default()
                }),
            },
            vec!["priority_changed"],
        ),
        (
            ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    priority: TriageField::Set(Priority::P2),
                    ..Default::default()
                }),
            },
            vec!["priority_changed"],
        ),
        (
            ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    assignee: TriageField::Clear,
                    ..Default::default()
                }),
            },
            vec!["assignee_changed"],
        ),
        (
            ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    assignee: TriageField::Set(AssigneeId::new("carol").unwrap()),
                    ..Default::default()
                }),
            },
            vec!["assignee_changed"],
        ),
        (
            ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    note: Some("note only".into()),
                    ..Default::default()
                }),
            },
            vec!["note_added"],
        ),
    ];
    let initial = persisted(&connection);
    let mut requests = Vec::new();
    for (index, (mutation, events)) in mutations.into_iter().enumerate() {
        let key = format!("triage-{index}");
        let request = request(&id, &key, index as u64 + 1, mutation);
        let result = mutate(&mut connection, request.clone(), &key, events.len()).unwrap();
        assert_eq!(result.item.revision().get(), index as u64 + 2);
        assert!(!result.replayed);
        requests.push((request, key, events));
        assert_coverage(&connection, &(1..=index as i64 + 2).collect::<Vec<_>>());
    }
    drop(connection);
    let connection = storage::open(&path).unwrap();
    for (index, (request, key, events)) in requests.iter().enumerate() {
        assert_mutation(&connection, request, key, index as i64 + 2, events);
    }
    assert_coverage(&connection, &[1, 2, 3, 4, 5, 6, 7]);
    let final_state = persisted(&connection);
    // Identity, generation, floor and inactive/bootstrap flags stay untouched.
    for table in ["store_metadata", "journal_metadata"] {
        let mut expected = initial.iter().find(|row| row.0 == table).unwrap().1.clone();
        if table == "journal_metadata" {
            expected[0][3] = rusqlite::types::Value::Integer(7);
        }
        assert_eq!(
            final_state.iter().find(|row| row.0 == table).unwrap().1,
            expected
        );
    }
}

#[test]
fn genuine_same_value_noops_replays_conflicts_and_stale_requests_write_nothing() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("replay.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = seed(&mut connection);
    // Clearing already-absent values is a genuine no-op, not omitted input.
    let empty = request(
        &id,
        "noop-clear",
        1,
        ItemMutation {
            lifecycle: None,
            triage: Some(Triage {
                priority: TriageField::Clear,
                assignee: TriageField::Clear,
                note: None,
            }),
        },
    );
    let before = persisted(&connection);
    let noop = mutate(&mut connection, empty, "unused", 0).unwrap();
    assert!(!noop.replayed);
    assert_eq!(noop.item.revision().get(), 1);
    assert_eq!(persisted(&connection), before);

    let original = request(
        &id,
        "committed",
        1,
        ItemMutation {
            lifecycle: None,
            triage: Some(Triage {
                priority: TriageField::Set(Priority::P1),
                assignee: TriageField::Set(AssigneeId::new("bob").unwrap()),
                note: None,
            }),
        },
    );
    let first = mutate(&mut connection, original.clone(), "committed", 2).unwrap();
    assert_mutation(
        &connection,
        &original,
        "committed",
        2,
        &["priority_changed", "assignee_changed"],
    );
    assert_coverage(&connection, &[1, 2]);
    let same = request(
        &id,
        "noop-set",
        2,
        ItemMutation {
            lifecycle: None,
            triage: Some(Triage {
                priority: TriageField::Set(Priority::P1),
                assignee: TriageField::Set(AssigneeId::new("bob").unwrap()),
                note: None,
            }),
        },
    );
    let before = persisted(&connection);
    let noop = mutate(&mut connection, same, "unused-noop", 0).unwrap();
    assert!(!noop.replayed);
    assert_eq!(noop.item, first.item);
    assert_eq!(persisted(&connection), before);
    drop(connection);
    let mut connection = storage::open(&path).unwrap();
    let before = persisted(&connection);
    // The expected revision is stale and applying these values again would be
    // a no-op, but the receipt must win over both checks and report a replay.
    let replay = mutate(&mut connection, original.clone(), "unused-replay", 0).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.item, first.item);
    assert_eq!(persisted(&connection), before);
    let mut changed = original.clone();
    changed.mutation.triage.as_mut().unwrap().note = Some("changed payload".into());
    assert_eq!(
        mutate(&mut connection, changed, "unused-conflict", 0)
            .unwrap_err()
            .code(),
        "idempotency_conflict"
    );
    assert_eq!(persisted(&connection), before);
    let mut stale = original.clone();
    stale.idempotency_key = "new-stale-key".into();
    assert_eq!(
        mutate(&mut connection, stale, "unused-stale", 0)
            .unwrap_err()
            .code(),
        "version_conflict"
    );
    assert_eq!(persisted(&connection), before);

    // Test-only pruning: receipts/audits/H/allocator are retained, never backfilled.
    connection
        .execute_batch(
            "BEGIN IMMEDIATE; DELETE FROM change_journal;
         UPDATE journal_metadata SET retention_floor = high_water; COMMIT;",
        )
        .unwrap();
    drop(connection);
    let mut connection = storage::open(&path).unwrap();
    let before = persisted(&connection);
    assert!(
        mutate(&mut connection, original, "unused-pruned", 0)
            .unwrap()
            .replayed
    );
    assert_eq!(persisted(&connection), before);
    let next = request(
        &id,
        "next",
        2,
        ItemMutation {
            lifecycle: None,
            triage: Some(Triage {
                note: Some("after pruning".into()),
                ..Default::default()
            }),
        },
    );
    mutate(&mut connection, next.clone(), "next", 1).unwrap();
    assert_mutation(&connection, &next, "next", 3, &["note_added"]);
    assert_coverage(&connection, &[3]);
}

fn assert_injected_rollback(trigger: &str, existing_mutation: bool) {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("rollback.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = seed(&mut connection);
    let original = request(&id, "failed", 1, compound());
    let (revision, mutation, count) = if existing_mutation {
        mutate(&mut connection, original.clone(), "previous", 4).unwrap();
        (
            2,
            ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    priority: TriageField::Set(Priority::P2),
                    assignee: TriageField::Clear,
                    note: Some("new".into()),
                }),
            },
            3,
        )
    } else {
        (1, compound(), 4)
    };
    let failed = request(&id, "failed-new-key", revision, mutation);
    let before = persisted(&connection);
    connection.execute_batch(trigger).unwrap();
    let error = mutate(&mut connection, failed.clone(), "failed", count).unwrap_err();
    let bif::application::MutationUseCaseError::Storage(sqlite) = error else {
        panic!("injected ABORT must remain a mutation storage error");
    };
    assert_eq!(
        sqlite.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    );
    assert!(sqlite.to_string().contains("injected mutation"));
    assert!(connection.is_autocommit());
    drop(connection);
    let mut connection = storage::open(&path).unwrap();
    assert_eq!(
        persisted(&connection),
        before,
        "fresh reopen: content/audits/receipts/metadata/allocators all roll back"
    );
    connection
        .execute_batch("DROP TRIGGER reject_mutation")
        .unwrap();
    let retry = mutate(&mut connection, failed.clone(), "failed", count).unwrap();
    assert!(!retry.replayed);
    assert_eq!(retry.item.revision().get(), revision + 1);
    let events = if existing_mutation {
        &["priority_changed", "assignee_changed", "note_added"][..]
    } else {
        &[
            "approved",
            "priority_changed",
            "assignee_changed",
            "note_added",
        ][..]
    };
    assert_mutation(
        &connection,
        &failed,
        "failed",
        if existing_mutation { 3 } else { 2 },
        events,
    );
    assert_coverage(
        &connection,
        if existing_mutation {
            &[1, 2, 3]
        } else {
            &[1, 2]
        },
    );
}

#[test]
fn journal_insert_abort_rolls_back_all_mutation_state_on_fresh_reopen() {
    for existing in [false, true] {
        assert_injected_rollback(
            "CREATE TRIGGER reject_mutation BEFORE INSERT ON change_journal
             BEGIN SELECT RAISE(ABORT, 'injected mutation journal insertion failure'); END;",
            existing,
        );
    }
}

#[test]
fn high_water_abort_rolls_back_all_mutation_state_on_fresh_reopen() {
    for existing in [false, true] {
        assert_injected_rollback(
            "CREATE TRIGGER reject_mutation BEFORE UPDATE OF high_water ON journal_metadata
             BEGIN SELECT RAISE(ABORT, 'injected mutation H update failure'); END;",
            existing,
        );
    }
}

#[test]
fn exhausted_journal_is_existing_storage_error_and_rolls_back_every_value() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("exhausted.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = seed(&mut connection);
    connection
        .execute("UPDATE journal_metadata SET high_water = ?1", [i64::MAX])
        .unwrap();
    let before = persisted(&connection);
    let error = mutate(
        &mut connection,
        request(&id, "exhausted", 1, compound()),
        "exhausted",
        4,
    )
    .unwrap_err();
    assert_eq!(error.code(), "storage_error");
    let bif::application::MutationUseCaseError::Storage(rusqlite::Error::ToSqlConversionFailure(
        source,
    )) = error
    else {
        panic!("non-SQL journal errors must use the existing storage error type");
    };
    assert!(matches!(
        source.downcast_ref::<JournalError>(),
        Some(JournalError::SequenceExhausted)
    ));
    assert!(connection.is_autocommit());
    drop(connection);
    assert_eq!(persisted(&storage::open(&path).unwrap()), before);
}

#[test]
fn invalidated_handle_checks_guard_before_replay_noop_or_effective_mutation() {
    let root = OwnedTestDirectory::new();
    let path = root.path().join("generation.sqlite");
    let mut connection = storage::open(&path).unwrap();
    let id = seed(&mut connection);
    let original = request(&id, "first", 1, compound());
    let first = mutate(&mut connection, original.clone(), "first", 4).unwrap();
    let observer = storage::open(&path).unwrap();
    let mut repository = MutationRepository::new(&mut connection).unwrap();
    observer
        .execute(
            "UPDATE journal_metadata SET generation = '00000000000000000000000000000000'",
            [],
        )
        .unwrap();
    let before = persisted(&observer);
    let noop = request(
        &id,
        "noop",
        2,
        ItemMutation {
            lifecycle: None,
            triage: Some(Triage {
                priority: TriageField::Set(Priority::P1),
                ..Default::default()
            }),
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
    for request in [original.clone(), noop, effective] {
        assert_eq!(
            mutate_with_repository(&mut repository, request, "unused", 0)
                .unwrap_err()
                .code(),
            "restart_required"
        );
        assert_eq!(persisted(&observer), before);
    }
    drop(repository);
    let replay = mutate(&mut connection, original, "unused", 0).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.item, first.item);
    assert_eq!(persisted(&observer), before);
}
