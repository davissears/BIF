mod support;

use std::{
    sync::{Arc, Barrier},
    thread,
};

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureError, CaptureIdentity, CaptureInput,
        CaptureRequest, Clock, Command, Execution, IdentityGenerator, ObservedExecution, capture,
    },
    domain::{ItemContent, ProjectId, Provenance, RequesterId, Timestamp},
    storage::{self, CaptureRepository},
};
use support::OwnedTestDirectory as TempDirectory;

struct FixedClock(&'static str);

impl Clock for FixedClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new(self.0)
    }
}

struct FixedIdentity(String);

impl IdentityGenerator for FixedIdentity {
    fn capture_identity(&mut self) -> CaptureIdentity {
        CaptureIdentity {
            operation_id: format!("operation-{}", self.0),
            event_id: format!("event-{}", self.0),
        }
    }
}

fn authorization() -> AuthorizationRequest<'static> {
    AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Agent,
            id: "capture-agent",
            surface: "thread",
            host: "delta",
        },
        execution: Execution::Agent {
            agent_id: "capture-agent",
            surface: "thread",
            host: "delta",
        },
        observed_execution: ObservedExecution::Agent {
            agent_id: "capture-agent",
        },
        command: Command::Capture,
        human_authorization: None,
    }
}

fn request(key: &str, criteria: &[&str]) -> CaptureRequest {
    CaptureRequest {
        idempotency_key: key.to_owned(),
        input: CaptureInput {
            requester: RequesterId::new("Davis").unwrap(),
            project: ProjectId::new("BIF").unwrap(),
            content: ItemContent::new(
                "idempotent capture",
                Some("complete payload".to_owned()),
                criteria.iter().map(|value| (*value).to_owned()).collect(),
            )
            .unwrap(),
            provenance: Provenance::default(),
        },
    }
}

#[test]
fn retry_ignores_new_server_values_and_conflicting_order_writes_nothing() {
    let temp = TempDirectory::new();
    let database = temp.path().join("bif.sqlite");
    let mut connection = storage::open(&database).unwrap();

    let first = {
        let mut repository = CaptureRepository::new(&mut connection).unwrap();
        capture(
            &mut repository,
            &mut FixedClock("2025-01-01T00:00:00Z"),
            &mut FixedIdentity("first".to_owned()),
            &authorization(),
            request("stable-key", &["one", "two"]),
        )
        .unwrap()
    };
    let replay = {
        let mut repository = CaptureRepository::new(&mut connection).unwrap();
        capture(
            &mut repository,
            &mut FixedClock("2099-01-01T00:00:00Z"),
            &mut FixedIdentity("retry".to_owned()),
            &authorization(),
            request("stable-key", &["one", "two"]),
        )
        .unwrap()
    };

    assert!(!first.replayed);
    assert!(replay.replayed);
    assert_eq!(replay.item.id(), first.item.id());
    assert_eq!(replay.item.captured_at(), first.item.captured_at());

    let conflict = {
        let mut repository = CaptureRepository::new(&mut connection).unwrap();
        capture(
            &mut repository,
            &mut FixedClock("2100-01-01T00:00:00Z"),
            &mut FixedIdentity("conflict".to_owned()),
            &authorization(),
            request("stable-key", &["two", "one"]),
        )
        .unwrap_err()
    };
    assert!(matches!(conflict, CaptureError::IdempotencyConflict));
    assert_eq!(conflict.code(), "idempotency_conflict");

    for table in [
        "items",
        "operations",
        "events",
        "mutation_receipts",
        "change_journal",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
}

#[test]
fn stale_capture_handle_rejects_new_and_replayed_receipts_after_generation_rotation() {
    let temp = TempDirectory::new();
    let database = temp.path().join("generation.sqlite");
    let mut connection = storage::open(&database).unwrap();
    let writer = storage::open(&database).unwrap();
    let mut repository = CaptureRepository::new(&mut connection).unwrap();
    capture(
        &mut repository,
        &mut FixedClock("now"),
        &mut FixedIdentity("first".into()),
        &authorization(),
        request("first", &[]),
    )
    .unwrap();
    writer
        .execute(
            "UPDATE journal_metadata SET generation = '00000000000000000000000000000000'",
            [],
        )
        .unwrap();
    for key in ["new", "first"] {
        let error = capture(
            &mut repository,
            &mut FixedClock("now"),
            &mut FixedIdentity(key.into()),
            &authorization(),
            request(key, &[]),
        )
        .unwrap_err();
        assert_eq!(error.code(), "restart_required");
    }
    assert_eq!(
        writer
            .query_row("SELECT count(*) FROM mutation_receipts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(repository);
    assert!(connection.is_autocommit());
    let replay = capture(
        &mut CaptureRepository::new(&mut connection).unwrap(),
        &mut FixedClock("now"),
        &mut FixedIdentity("unused".into()),
        &authorization(),
        request("first", &[]),
    )
    .unwrap();
    assert!(replay.replayed);
    let fresh = capture(
        &mut CaptureRepository::new(&mut connection).unwrap(),
        &mut FixedClock("now"),
        &mut FixedIdentity("fresh".into()),
        &authorization(),
        request("fresh", &[]),
    )
    .unwrap();
    assert!(!fresh.replayed);
}

#[test]
fn concurrent_identical_keys_allocate_one_item() {
    const ATTEMPTS: usize = 10;
    let temp = TempDirectory::new();
    let database = temp.path().join("bif.sqlite");
    storage::open(&database).unwrap();
    let barrier = Arc::new(Barrier::new(ATTEMPTS));

    let handles: Vec<_> = (0..ATTEMPTS)
        .map(|index| {
            let database = database.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut connection = storage::open(database).unwrap();
                let mut repository = CaptureRepository::new(&mut connection).unwrap();
                barrier.wait();
                capture(
                    &mut repository,
                    &mut FixedClock("2025-01-01T00:00:00Z"),
                    &mut FixedIdentity(index.to_string()),
                    &authorization(),
                    request("shared-key", &["one", "two"]),
                )
                .unwrap()
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| !result.replayed).count(), 1);
    assert!(
        results
            .windows(2)
            .all(|pair| pair[0].item.id() == pair[1].item.id())
    );

    let connection = storage::open(database).unwrap();
    for table in [
        "items",
        "operations",
        "events",
        "mutation_receipts",
        "change_journal",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
    let complete: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT j.change_seq, m.high_water, c.next_sequence,
                    (SELECT seq FROM sqlite_sequence WHERE name = 'change_journal')
             FROM change_journal j
             JOIN operations o ON o.operation_id = j.operation_id
             JOIN items i ON i.item_id = j.item_id
             JOIN mutation_receipts r ON r.operation_id = o.operation_id
             JOIN events e ON e.operation_id = o.operation_id
             JOIN requester_project_counters c
               ON c.project_id = i.project_id AND c.requester = i.requester
             CROSS JOIN journal_metadata m
             WHERE j.item_id = o.item_id AND j.item_id = r.item_id
               AND j.item_id = e.item_id AND j.project_id = i.project_id
               AND j.item_revision = 1 AND o.item_revision = 1
               AND i.revision = 1 AND e.item_revision = 1
               AND o.operation_type = 'capture' AND r.mutation_type = 'capture'
               AND e.event_type = 'captured' AND e.event_index = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(complete, (1, 1, 2, 1));
}
