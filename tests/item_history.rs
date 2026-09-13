use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Command, EventExecution, Execution,
        ItemHistoryError, ObservedExecution, read_item_history,
    },
    domain::{AssigneeId, EventType, EventValue, ItemId, Priority, ProjectId, RequesterId, Status},
    storage::{self, ItemHistoryRepository, ItemHistoryStorageError},
};

fn item_id() -> ItemId {
    ItemId::new(
        RequesterId::new("DAVIS").unwrap(),
        ProjectId::new("bif").unwrap(),
        1,
    )
    .unwrap()
}

fn authorization() -> AuthorizationRequest<'static> {
    AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "reader",
            surface: "cli",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "cli",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: Command::Read,
        human_authorization: None,
    }
}

fn seed_item(connection: &rusqlite::Connection) {
    connection
        .execute_batch(
            "
            INSERT INTO projects (project_id, created_at) VALUES ('bif', 'same-time');
            INSERT INTO items (
                item_id, requester, project_id, sequence, title, status, revision,
                captured_at, updated_at
            ) VALUES (
                'DAVIS:bif:001', 'DAVIS', 'bif', 1, 'History', 'ready', 2,
                'same-time', 'same-time'
            );
            INSERT INTO item_provenance (item_id) VALUES ('DAVIS:bif:001');
            INSERT INTO operations (
                operation_id, item_id, operation_type, expected_revision,
                item_revision, occurred_at
            ) VALUES
                ('operation-capture', 'DAVIS:bif:001', 'capture', NULL, 1, 'same-time'),
                ('operation-triage', 'DAVIS:bif:001', 'triage', 1, 2, 'same-time');
            ",
        )
        .unwrap();
}

#[test]
fn reads_complete_typed_events_in_revision_and_index_order_when_timestamps_are_equal() {
    let connection = storage::open(":memory:").unwrap();
    seed_item(&connection);
    connection
        .execute_batch(
            "
            INSERT INTO events VALUES (
                'event-assignee', 'operation-triage', 'DAVIS:bif:001', 2, 2,
                'assignee_changed', 'null', 'builder', 'human', 'alice', 'thread',
                'delta', 'agent', 'worker-7', 'thread', 'delta', NULL, NULL,
                'same-time', 1
            );
            INSERT INTO events VALUES (
                'event-captured', 'operation-capture', 'DAVIS:bif:001', 1, 0,
                'captured', NULL, NULL, 'human', 'alice', 'thread', 'delta',
                'agent', 'worker-7', 'thread', 'delta', NULL, NULL, 'same-time', 1
            );
            INSERT INTO events VALUES (
                'event-note', 'operation-triage', 'DAVIS:bif:001', 2, 3,
                'note_added', NULL, 'ship it', 'human', 'alice', 'thread', 'delta',
                'agent', 'worker-7', 'thread', 'delta', NULL, 'ship it', 'same-time', 1
            );
            INSERT INTO events VALUES (
                'event-approved', 'operation-triage', 'DAVIS:bif:001', 2, 0,
                'approved', 'proposed', 'ready', 'human', 'alice', 'thread', 'delta',
                'agent', 'worker-7', 'thread', 'delta', 'reviewed', NULL,
                'same-time', 1
            );
            INSERT INTO events VALUES (
                'event-priority', 'operation-triage', 'DAVIS:bif:001', 2, 1,
                'priority_changed', 'null', 'P1', 'human', 'alice', 'thread',
                'delta', 'agent', 'worker-7', 'thread', 'delta', NULL, NULL,
                'same-time', 1
            );
            ",
        )
        .unwrap();

    let history = read_item_history(
        &ItemHistoryRepository::new(&connection),
        &authorization(),
        &item_id(),
    )
    .unwrap();

    assert_eq!(
        history
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>(),
        [
            "event-captured",
            "event-approved",
            "event-priority",
            "event-assignee",
            "event-note"
        ]
    );
    assert!(history.iter().all(|event| {
        event.occurred_at.as_str() == "same-time"
            && event.schema_version == 1
            && event.actor.kind == ActorKind::Human
            && event.actor.id == "alice"
            && event.actor.surface == "thread"
            && event.actor.host == "delta"
            && matches!(
                &event.execution,
                EventExecution::Agent {
                    agent_id,
                    surface,
                    host
                } if agent_id == "worker-7" && surface == "thread" && host == "delta"
            )
    }));
    assert_eq!(history[1].operation_id, "operation-triage");
    assert_eq!(history[1].item_revision.get(), 2);
    assert_eq!(history[1].event_index, 0);
    assert_eq!(history[1].event_type, EventType::Approved);
    assert_eq!(
        history[1].before,
        Some(EventValue::Status(Status::Proposed))
    );
    assert_eq!(history[1].after, Some(EventValue::Status(Status::Ready)));
    assert_eq!(history[1].reason.as_deref(), Some("reviewed"));
    assert_eq!(
        history[2].after,
        Some(EventValue::Priority(Some(Priority::P1)))
    );
    assert_eq!(
        history[3].after,
        Some(EventValue::Assignee(Some(
            AssigneeId::new("builder").unwrap()
        )))
    );
    assert_eq!(
        history[4].after,
        Some(EventValue::Note("ship it".to_owned()))
    );
    assert_eq!(history[4].note.as_deref(), Some("ship it"));
}

#[test]
fn reports_typed_not_found_and_invalid_persisted_data() {
    let connection = storage::open(":memory:").unwrap();
    let missing = read_item_history(
        &ItemHistoryRepository::new(&connection),
        &authorization(),
        &item_id(),
    )
    .unwrap_err();
    assert!(matches!(missing, ItemHistoryError::NotFound));
    assert_eq!(missing.code(), "not_found");

    seed_item(&connection);
    connection
        .execute_batch(
            "
            INSERT INTO events VALUES (
                'bad-event', 'operation-capture', 'DAVIS:bif:001', 1, 0,
                'approved', 'proposed', 'not-a-status', 'human', 'alice', 'cli',
                'local', 'direct', NULL, 'cli', 'local', NULL, NULL, 'same-time', 1
            );
            ",
        )
        .unwrap();
    let invalid = read_item_history(
        &ItemHistoryRepository::new(&connection),
        &authorization(),
        &item_id(),
    )
    .unwrap_err();
    assert_eq!(invalid.code(), "invalid_persisted_data");
    assert!(matches!(
        invalid,
        ItemHistoryError::InvalidPersistedData(ItemHistoryStorageError::InvalidPersistedData {
            field: "after_value",
            ..
        })
    ));
}
