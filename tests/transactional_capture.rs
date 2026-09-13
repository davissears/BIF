mod support;

use std::{
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureIdentity, CaptureInput, CaptureRequest,
        Clock, Command, Execution, IdentityGenerator, ObservedExecution, capture,
    },
    domain::{
        ItemContent, MessageId, ProjectId, Provenance, RepositoryReference, RequesterId,
        RevisionReference, SourceHost, SourceUrl, ThreadId, Timestamp,
    },
    storage::{self, CaptureRepository},
};
use support::OwnedTestDirectory as TempDirectory;

struct FixedClock(&'static str);

impl Clock for FixedClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new(self.0)
    }
}

struct FixedIdentity {
    operation: String,
    event: String,
}

impl IdentityGenerator for FixedIdentity {
    fn capture_identity(&mut self) -> CaptureIdentity {
        CaptureIdentity {
            operation_id: self.operation.clone(),
            event_id: self.event.clone(),
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

fn input(title: String) -> CaptureInput {
    CaptureInput {
        requester: RequesterId::new("Davis").unwrap(),
        project: ProjectId::new("BIF").unwrap(),
        content: ItemContent::new(
            title,
            Some("description".to_owned()),
            vec!["first".to_owned(), "second".to_owned()],
        )
        .unwrap(),
        provenance: Provenance::new(
            Some(SourceHost::Delta),
            Some(ThreadId::new("thread-1")),
            Some(MessageId::new("message-1")),
            Some(SourceUrl::new("https://example.test/source")),
            Some(RepositoryReference::new("owner/repo")),
            Some(RevisionReference::new("abc123")),
            Some("source context".to_owned()),
        ),
    }
}

fn request(key: impl Into<String>, title: String) -> CaptureRequest {
    CaptureRequest {
        idempotency_key: key.into(),
        input: input(title),
    }
}

#[test]
fn concurrent_captures_have_gap_free_ids_and_complete_history() {
    const CAPTURES: usize = 12;
    let temp = TempDirectory::new();
    let database = temp.path().join("bif.sqlite");
    storage::open(&database).unwrap();
    let barrier = Arc::new(Barrier::new(CAPTURES));

    let handles: Vec<_> = (0..CAPTURES)
        .map(|index| {
            let database = database.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut connection = storage::open(database).unwrap();
                let mut repository = CaptureRepository::new(&mut connection);
                let mut clock = FixedClock("2025-02-03T04:05:06Z");
                let mut identities = FixedIdentity {
                    operation: format!("operation-{index}"),
                    event: format!("event-{index}"),
                };
                barrier.wait();
                capture(
                    &mut repository,
                    &mut clock,
                    &mut identities,
                    &authorization(),
                    request(format!("key-{index}"), format!("capture {index}")),
                )
                .unwrap()
                .item
                .id()
                .sequence()
            })
        })
        .collect();

    let mut sequences: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=CAPTURES as u64).collect::<Vec<_>>());

    let connection = storage::open(&database).unwrap();
    for table in ["items", "item_provenance", "operations", "events"] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, CAPTURES as i64, "{table}");
    }
    let criteria: i64 = connection
        .query_row("SELECT count(*) FROM item_acceptance_criteria", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(criteria, (CAPTURES * 2) as i64);
    let malformed_history: i64 = connection
        .query_row(
            "SELECT count(*) FROM events AS e
             JOIN operations AS o ON o.operation_id = e.operation_id
             WHERE e.event_type != 'captured' OR e.event_index != 0
                OR e.item_revision != 1 OR o.operation_type != 'capture'
                OR o.item_id != e.item_id OR o.item_revision != e.item_revision",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(malformed_history, 0);
}

#[test]
fn failed_capture_rolls_back_allocated_sequence_and_all_rows() {
    let temp = TempDirectory::new();
    let database = temp.path().join("bif.sqlite");
    let mut connection = storage::open(&database).unwrap();
    let mut clock = FixedClock("2025-02-03T04:05:06Z");

    let first = {
        let mut repository = CaptureRepository::new(&mut connection);
        capture(
            &mut repository,
            &mut clock,
            &mut FixedIdentity {
                operation: "duplicate-operation".to_owned(),
                event: "event-1".to_owned(),
            },
            &authorization(),
            request("key-1", "first".to_owned()),
        )
        .unwrap()
        .item
    };
    assert_eq!(first.id().sequence(), 1);

    {
        let mut repository = CaptureRepository::new(&mut connection);
        let error = capture(
            &mut repository,
            &mut clock,
            &mut FixedIdentity {
                operation: "duplicate-operation".to_owned(),
                event: "rolled-back-event".to_owned(),
            },
            &authorization(),
            request("key-2", "must roll back".to_owned()),
        )
        .unwrap_err();
        assert_eq!(error.code(), "storage_error");
    }

    let second = {
        let mut repository = CaptureRepository::new(&mut connection);
        capture(
            &mut repository,
            &mut clock,
            &mut FixedIdentity {
                operation: "operation-2".to_owned(),
                event: "event-2".to_owned(),
            },
            &authorization(),
            request("key-3", "second".to_owned()),
        )
        .unwrap()
        .item
    };
    assert_eq!(second.id().sequence(), 2);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM items WHERE title = 'must roll back'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn capture_persists_canonical_content_provenance_and_attribution() {
    let temp = TempDirectory::new();
    let database = temp.path().join("bif.sqlite");
    let mut connection = storage::open(&database).unwrap();
    let item = {
        let mut repository = CaptureRepository::new(&mut connection);
        capture(
            &mut repository,
            &mut FixedClock("2025-02-03T04:05:06Z"),
            &mut FixedIdentity {
                operation: "operation".to_owned(),
                event: "event".to_owned(),
            },
            &authorization(),
            request("canonical-key", "canonical capture".to_owned()),
        )
        .unwrap()
        .item
    };

    assert_eq!(item.id().to_string(), "DAVIS:bif:001");
    let stored: (String, String, i64, String, Option<String>, String, String) = connection
        .query_row(
            "SELECT i.status, i.captured_at, i.revision, p.source_host,
                    p.context_excerpt, e.actor_id, e.execution_agent_id
             FROM items AS i
             JOIN item_provenance AS p USING (item_id)
             JOIN events AS e USING (item_id)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        stored,
        (
            "proposed".to_owned(),
            "2025-02-03T04:05:06Z".to_owned(),
            1,
            "delta".to_owned(),
            Some("source context".to_owned()),
            "capture-agent".to_owned(),
            "capture-agent".to_owned(),
        )
    );
    let criteria: Vec<String> = connection
        .prepare("SELECT criterion FROM item_acceptance_criteria ORDER BY criterion_index")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(criteria, ["first", "second"]);
}

#[test]
fn writer_lock_maps_capture_timeout_to_storage_busy() {
    let temp = TempDirectory::new();
    let database = temp.path().join("bif.sqlite");
    let writer = storage::open(&database).unwrap();
    let mut contender = storage::open(&database).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = Instant::now();
    let error = capture(
        &mut CaptureRepository::new(&mut contender),
        &mut FixedClock("2025-02-03T04:05:06Z"),
        &mut FixedIdentity {
            operation: "locked-operation".to_owned(),
            event: "locked-event".to_owned(),
        },
        &authorization(),
        request("locked-key", "locked capture".to_owned()),
    )
    .unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(error.code(), "storage_busy");
    assert!(std::error::Error::source(&error).is_some());
    assert!(elapsed >= Duration::from_secs(4), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(8), "{elapsed:?}");
    writer.execute_batch("ROLLBACK").unwrap();
}
