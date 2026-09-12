use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureError, CaptureIdentity, CaptureInput,
        CaptureRequest, Clock, Command, Execution, IdentityGenerator, ObservedExecution, capture,
    },
    domain::{ItemContent, ProjectId, Provenance, RequesterId, Timestamp},
    storage::{self, CaptureRepository},
};

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "bif-capture-idempotency-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
    let database = temp.0.join("bif.sqlite");
    let mut connection = storage::open(&database).unwrap();

    let first = {
        let mut repository = CaptureRepository::new(&mut connection);
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
        let mut repository = CaptureRepository::new(&mut connection);
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
        let mut repository = CaptureRepository::new(&mut connection);
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

    for table in ["items", "operations", "events", "mutation_receipts"] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
}

#[test]
fn concurrent_identical_keys_allocate_one_item() {
    const ATTEMPTS: usize = 10;
    let temp = TempDirectory::new();
    let database = temp.0.join("bif.sqlite");
    storage::open(&database).unwrap();
    let barrier = Arc::new(Barrier::new(ATTEMPTS));

    let handles: Vec<_> = (0..ATTEMPTS)
        .map(|index| {
            let database = database.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut connection = storage::open(database).unwrap();
                let mut repository = CaptureRepository::new(&mut connection);
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
    for table in ["items", "operations", "events", "mutation_receipts"] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
}
