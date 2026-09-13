mod support;

use std::{
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Clock, Command, Execution, HumanAuthorization,
        MutationIdentity, MutationIdentityGenerator, MutationRequest, ObservedExecution,
        mutate_item, mutate_item_idempotent,
    },
    domain::{
        AssigneeId, ItemId, ItemMutation, LifecycleMutation, Priority, ProjectId, RequesterId,
        Revision, Timestamp, Triage, TriageField,
    },
    storage::{self, MutationRepository},
};
use support::OwnedTestDirectory as TempDirectory;

struct FixedClock;
impl Clock for FixedClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new("2025-02-04T05:06:07Z")
    }
}

struct FixedIdentities {
    operation_id: String,
    event_ids: Vec<String>,
}
impl MutationIdentityGenerator for FixedIdentities {
    fn mutation_identity(&mut self) -> MutationIdentity {
        MutationIdentity {
            operation_id: self.operation_id.clone(),
            event_ids: self.event_ids.clone(),
        }
    }
}

fn authorization() -> AuthorizationRequest<'static> {
    static CHANGES: &[&str] = &["approve", "priority", "assignee", "note"];
    AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "alice",
            surface: "cli",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "cli",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: Command::Mutation {
            requested_changes: CHANGES,
        },
        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
    }
}

fn seed(connection: &rusqlite::Connection) -> ItemId {
    connection
        .execute(
            "INSERT INTO projects (project_id, created_at) VALUES ('bif', '2025-01-01Z')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO items (item_id, requester, project_id, sequence, title, description,
         status, priority, assignee, status_reason, revision, captured_at, updated_at)
         VALUES ('DAVIS:bif:001', 'DAVIS', 'bif', 1, 'item', NULL, 'proposed',
                 NULL, NULL, NULL, 1, '2025-01-01Z', '2025-01-01Z')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO item_provenance (item_id) VALUES ('DAVIS:bif:001')",
            [],
        )
        .unwrap();
    ItemId::new(
        RequesterId::new("DAVIS").unwrap(),
        ProjectId::new("bif").unwrap(),
        1,
    )
    .unwrap()
}

fn compound() -> ItemMutation {
    ItemMutation {
        lifecycle: Some(LifecycleMutation::Approve),
        triage: Some(Triage {
            priority: TriageField::Set(Priority::P1),
            assignee: TriageField::Set(AssigneeId::new("bob").unwrap()),
            note: Some("ready to build".to_owned()),
        }),
    }
}

#[test]
fn compound_mutation_updates_once_and_persists_ordered_complete_history() {
    let temp = TempDirectory::new();
    let mut connection = storage::open(temp.path().join("bif.sqlite")).unwrap();
    let id = seed(&connection);
    let item = mutate_item(
        &mut MutationRepository::new(&mut connection),
        &mut FixedClock,
        &mut FixedIdentities {
            operation_id: "mutation-operation".to_owned(),
            event_ids: (0..4).map(|index| format!("event-{index}")).collect(),
        },
        &authorization(),
        &id,
        Revision::new(1).unwrap(),
        compound(),
    )
    .unwrap();
    assert_eq!(item.revision().get(), 2);
    let stored: (String, String, String, i64, String) = connection
        .query_row(
            "SELECT status, priority, assignee, revision, updated_at FROM items",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        stored,
        (
            "ready".into(),
            "P1".into(),
            "bob".into(),
            2,
            "2025-02-04T05:06:07Z".into()
        )
    );
    let events: Vec<(i64, String, Option<String>, Option<String>, String, String)> = connection
        .prepare("SELECT event_index, event_type, before_value, after_value, actor_id, execution_kind FROM events ORDER BY event_index")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
        .unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.1.as_str())
            .collect::<Vec<_>>(),
        [
            "approved",
            "priority_changed",
            "assignee_changed",
            "note_added"
        ]
    );
    assert!(
        events
            .iter()
            .all(|event| event.4 == "alice" && event.5 == "direct")
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM operations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn invalid_component_and_event_insert_failure_roll_back_item_and_history() {
    let temp = TempDirectory::new();
    let mut connection = storage::open(temp.path().join("bif.sqlite")).unwrap();
    let id = seed(&connection);
    let invalid = ItemMutation {
        lifecycle: Some(LifecycleMutation::Approve),
        triage: Some(Triage {
            priority: TriageField::Omitted,
            assignee: TriageField::Omitted,
            note: None,
        }),
    };
    assert!(
        mutate_item(
            &mut MutationRepository::new(&mut connection),
            &mut FixedClock,
            &mut FixedIdentities {
                operation_id: "invalid-operation".to_owned(),
                event_ids: vec![],
            },
            &authorization(),
            &id,
            Revision::new(1).unwrap(),
            invalid,
        )
        .is_err()
    );
    let error = mutate_item(
        &mut MutationRepository::new(&mut connection),
        &mut FixedClock,
        &mut FixedIdentities {
            operation_id: "failed-operation".to_owned(),
            event_ids: vec!["same".into(); 4],
        },
        &authorization(),
        &id,
        Revision::new(1).unwrap(),
        compound(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "storage_error");
    let state: (String, i64, String) = connection
        .query_row(
            "SELECT status, revision, updated_at FROM items",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, ("proposed".into(), 1, "2025-01-01Z".into()));
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM operations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn concurrent_expected_revision_allows_one_winner_and_stale_requests_write_nothing() {
    let temp = TempDirectory::new();
    let database_path = temp.path().join("bif.sqlite");
    let connection = storage::open(&database_path).unwrap();
    let id = seed(&connection);
    drop(connection);

    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|request| {
            let database_path = database_path.clone();
            let barrier = Arc::clone(&barrier);
            let id = id.clone();
            thread::spawn(move || {
                let mut connection = storage::open(database_path).unwrap();
                let mut identities = FixedIdentities {
                    operation_id: format!("concurrent-operation-{request}"),
                    event_ids: (0..4)
                        .map(|index| format!("concurrent-event-{request}-{index}"))
                        .collect(),
                };
                barrier.wait();
                mutate_item(
                    &mut MutationRepository::new(&mut connection),
                    &mut FixedClock,
                    &mut identities,
                    &authorization(),
                    &id,
                    Revision::new(1).unwrap(),
                    compound(),
                )
                .map(|item| item.revision().get())
                .map_err(|error| error.code())
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| **result == Ok(2)).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err("version_conflict"))
            .count(),
        1
    );

    let mut connection = storage::open(database_path).unwrap();
    let snapshot = mutation_state(&connection);
    let stale = mutate_item(
        &mut MutationRepository::new(&mut connection),
        &mut FixedClock,
        &mut FixedIdentities {
            operation_id: "stale-operation".to_owned(),
            event_ids: (0..4).map(|index| format!("stale-event-{index}")).collect(),
        },
        &authorization(),
        &id,
        Revision::new(1).unwrap(),
        compound(),
    )
    .unwrap_err();
    assert_eq!(stale.code(), "version_conflict");
    assert_eq!(mutation_state(&connection), snapshot);
}

#[test]
fn committed_mutation_replays_before_stale_revision_and_changed_payload_conflicts() {
    let temp = TempDirectory::new();
    let mut connection = storage::open(temp.path().join("bif.sqlite")).unwrap();
    let id = seed(&connection);
    let request = MutationRequest {
        idempotency_key: "caller-stable-mutation-key".to_owned(),
        item_id: id,
        expected_revision: Revision::new(1).unwrap(),
        mutation: compound(),
    };

    let first = mutate_item_idempotent(
        &mut MutationRepository::new(&mut connection),
        &mut FixedClock,
        &mut FixedIdentities {
            operation_id: "lost-response-operation".to_owned(),
            event_ids: (0..4).map(|index| format!("lost-event-{index}")).collect(),
        },
        &authorization(),
        request.clone(),
    )
    .unwrap();
    assert!(!first.replayed);

    let replay = mutate_item_idempotent(
        &mut MutationRepository::new(&mut connection),
        &mut FixedClock,
        &mut FixedIdentities {
            operation_id: "unused-retry-operation".to_owned(),
            event_ids: (0..4)
                .map(|index| format!("unused-event-{index}"))
                .collect(),
        },
        &authorization(),
        request.clone(),
    )
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.item, first.item);

    let mut changed = request;
    changed.mutation.triage.as_mut().unwrap().note = Some("changed note".to_owned());
    let conflict = mutate_item_idempotent(
        &mut MutationRepository::new(&mut connection),
        &mut FixedClock,
        &mut FixedIdentities {
            operation_id: "unused-conflict-operation".to_owned(),
            event_ids: vec![],
        },
        &authorization(),
        changed,
    )
    .unwrap_err();
    assert_eq!(conflict.code(), "idempotency_conflict");
    assert_eq!(mutation_state(&connection).3, 1);
    assert_eq!(mutation_state(&connection).4, 4);
    assert_eq!(mutation_state(&connection).5, 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM events WHERE event_type = 'note_added'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn writer_lock_maps_mutation_timeout_to_storage_busy() {
    let temp = TempDirectory::new();
    let database = temp.path().join("bif.sqlite");
    let writer = storage::open(&database).unwrap();
    let id = seed(&writer);
    let mut contender = storage::open(&database).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = Instant::now();
    let error = mutate_item(
        &mut MutationRepository::new(&mut contender),
        &mut FixedClock,
        &mut FixedIdentities {
            operation_id: "locked-operation".to_owned(),
            event_ids: (0..4)
                .map(|index| format!("locked-event-{index}"))
                .collect(),
        },
        &authorization(),
        &id,
        Revision::new(1).unwrap(),
        compound(),
    )
    .unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(error.code(), "storage_busy");
    assert!(std::error::Error::source(&error).is_some());
    assert!(elapsed >= Duration::from_secs(4), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(8), "{elapsed:?}");
    writer.execute_batch("ROLLBACK").unwrap();
}

fn mutation_state(connection: &rusqlite::Connection) -> (String, i64, String, i64, i64, i64) {
    let (status, revision, updated_at) = connection
        .query_row(
            "SELECT status, revision, updated_at FROM items",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let count = |table| {
        connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    (
        status,
        revision,
        updated_at,
        count("operations"),
        count("events"),
        count("mutation_receipts"),
    )
}
