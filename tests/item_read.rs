use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureIdentity, CaptureInput, CaptureRequest,
        Clock, Command, Execution, HumanAuthorization, IdentityGenerator, MutationIdentity,
        MutationIdentityGenerator, ObservedExecution, ReadItemError, capture, mutate_item,
        read_item,
    },
    domain::{
        AssigneeId, ItemContent, ItemId, ItemMutation, LifecycleMutation, MessageId, Priority,
        ProjectId, Provenance, RepositoryReference, RequesterId, RevisionReference, SourceHost,
        SourceUrl, ThreadId, Timestamp, Triage, TriageField,
    },
    storage::{self, CaptureRepository, ItemRepository, MutationRepository},
};

struct FixedClock(&'static str);

impl Clock for FixedClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new(self.0)
    }
}

struct CaptureIds;

impl IdentityGenerator for CaptureIds {
    fn capture_identity(&mut self) -> CaptureIdentity {
        CaptureIdentity {
            operation_id: "capture-operation".to_owned(),
            event_id: "capture-event".to_owned(),
        }
    }
}

struct MutationIds;

impl MutationIdentityGenerator for MutationIds {
    fn mutation_identity(&mut self) -> MutationIdentity {
        MutationIdentity {
            operation_id: "triage-operation".to_owned(),
            event_ids: (0..3)
                .map(|index| format!("triage-event-{index}"))
                .collect(),
        }
    }
}

fn authorization(command: Command<'static>) -> AuthorizationRequest<'static> {
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
        command,
        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
    }
}

fn missing_id() -> ItemId {
    ItemId::new(
        RequesterId::new("ALICE").unwrap(),
        ProjectId::new("bif").unwrap(),
        99,
    )
    .unwrap()
}

#[test]
fn captured_and_triaged_item_round_trips_every_canonical_field() {
    static CHANGES: &[&str] = &["approve", "priority", "assignee"];
    let mut connection = storage::open(":memory:").unwrap();
    let captured = capture(
        &mut CaptureRepository::new(&mut connection),
        &mut FixedClock("2025-03-01T01:02:03Z"),
        &mut CaptureIds,
        &authorization(Command::Capture),
        CaptureRequest {
            idempotency_key: "capture-key".to_owned(),
            input: CaptureInput {
                requester: RequesterId::new("Alice").unwrap(),
                project: ProjectId::new("BIF").unwrap(),
                content: ItemContent::new(
                    "Complete read".to_owned(),
                    Some("Every canonical field must survive".to_owned()),
                    vec!["content".to_owned(), "triage".to_owned()],
                )
                .unwrap(),
                provenance: Provenance::new(
                    Some(SourceHost::Delta),
                    Some(ThreadId::new("thread-31")),
                    Some(MessageId::new("message-31")),
                    Some(SourceUrl::new("https://example.test/items/31")),
                    Some(RepositoryReference::new("example/bif")),
                    Some(RevisionReference::new("deadbeef")),
                    Some("BIF-031 context".to_owned()),
                ),
            },
        },
    )
    .unwrap()
    .item;

    let expected = mutate_item(
        &mut MutationRepository::new(&mut connection),
        &mut FixedClock("2025-03-02T02:03:04Z"),
        &mut MutationIds,
        &authorization(Command::Mutation {
            requested_changes: CHANGES,
        }),
        captured.id(),
        captured.revision(),
        ItemMutation {
            lifecycle: Some(LifecycleMutation::Approve),
            triage: Some(Triage {
                priority: TriageField::Set(Priority::P1),
                assignee: TriageField::Set(AssigneeId::new("bob").unwrap()),
                note: None,
            }),
        },
    )
    .unwrap();

    let loaded = read_item(
        &ItemRepository::new(&connection),
        &authorization(Command::Read),
        expected.id(),
    )
    .unwrap();

    assert_eq!(loaded, expected);
}

#[test]
fn missing_item_is_a_typed_not_found_error() {
    let connection = storage::open(":memory:").unwrap();
    let error = read_item(
        &ItemRepository::new(&connection),
        &authorization(Command::Read),
        &missing_id(),
    )
    .unwrap_err();

    assert!(matches!(error, ReadItemError::NotFound));
    assert_eq!(error.code(), "not_found");
}

#[test]
fn invalid_persisted_data_identifies_the_item_and_field() {
    let connection = storage::open(":memory:").unwrap();
    connection
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO projects (project_id, created_at) VALUES ('bif', '2025-01-01Z');
             INSERT INTO items (
                 item_id, requester, project_id, sequence, title, status, revision,
                 captured_at, updated_at
             ) VALUES (
                 'ALICE:bif:099', 'ALICE', 'bif', 99, 'broken', 'unknown', 1,
                 '2025-01-01Z', '2025-01-01Z'
             );
             INSERT INTO item_provenance (item_id) VALUES ('ALICE:bif:099');",
        )
        .unwrap();

    let error = read_item(
        &ItemRepository::new(&connection),
        &authorization(Command::Read),
        &missing_id(),
    )
    .unwrap_err();
    let message = error.to_string();

    assert_eq!(error.code(), "storage_error");
    assert!(message.contains("ALICE:bif:099"), "{message}");
    assert!(message.contains("unknown status value"), "{message}");
}
