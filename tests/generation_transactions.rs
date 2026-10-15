//! Deterministic WAL interleavings at actual repository statement boundaries.
use std::{cell::RefCell, time::Duration};

use bif::{
    application::{
        Actor, ActorKind, CaptureIdentity, CaptureInput, CaptureRequest, CaptureStore,
        CaptureStoreError, ConditionalGetRequest, ConditionalProjectionStore,
        ConditionalReadOutcome, Execution, ItemProjection, ItemProjectionKind,
    },
    domain::{ItemContent, ProjectId, Provenance, RequesterId, Timestamp},
    storage::{self, CaptureRepository, MutationRepository, ProjectionRepository},
};
use rusqlite::{
    Connection,
    trace::{TraceEvent, TraceEventCodes},
};

const ROTATE: &str = "UPDATE journal_metadata SET generation = '00000000000000000000000000000000'";
struct Control {
    writer: Connection,
    boundary: &'static str,
    expect_busy: bool,
    fired: bool,
    rotation_result: Option<Result<(), rusqlite::Error>>,
    statements: Vec<String>,
}
thread_local! {
    static CONTROL: RefCell<Option<Control>> = const { RefCell::new(None) };
}

fn trace(event: TraceEvent<'_>) {
    if let TraceEvent::Stmt(statement, _) = event {
        CONTROL.with(|slot| {
            let mut slot = slot.borrow_mut();
            let control = slot.as_mut().unwrap();
            let sql = statement.sql();
            control.statements.push(sql.to_string());
            if !control.fired && sql.contains(control.boundary) {
                control.fired = true;
                let result = control.writer.execute_batch(&format!(
                    "BEGIN IMMEDIATE; {ROTATE}; UPDATE items SET title = 'restored'; COMMIT;"
                ));
                // Assert outside the FFI callback: callback panics may be caught
                // by the trace adapter rather than failing the surrounding test.
                control.rotation_result = Some(result);
            }
        });
    }
}

fn actor() -> Actor<'static> {
    Actor {
        kind: ActorKind::Human,
        id: "ALICE",
        surface: "cli",
        host: "local",
    }
}
fn execution() -> Execution<'static> {
    Execution::Direct {
        surface: "cli",
        host: "local",
    }
}
fn capture_request(key: &str) -> CaptureRequest {
    CaptureRequest {
        idempotency_key: key.into(),
        input: CaptureInput {
            requester: RequesterId::new("ALICE").unwrap(),
            project: ProjectId::new("bif").unwrap(),
            content: ItemContent::new("original", None, vec![]).unwrap(),
            provenance: Provenance::default(),
        },
    }
}
fn capture(
    repository: &mut CaptureRepository<'_>,
    key: &str,
) -> Result<bif::application::CaptureResult, CaptureStoreError<bif::storage::CaptureStorageError>> {
    repository.capture(
        capture_request(key),
        key.into(),
        actor(),
        execution(),
        Timestamp::new("now"),
        CaptureIdentity {
            operation_id: key.into(),
            event_id: key.into(),
        },
    )
}

fn connections() -> (tempfile::TempDir, Connection, Connection) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bif.sqlite");
    let reader = storage::open(&path).unwrap();
    let writer = storage::open(&path).unwrap();
    assert_eq!(
        reader
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    writer.busy_timeout(Duration::ZERO).unwrap();
    (temp, reader, writer)
}

#[test]
fn conditional_generation_revision_and_payload_share_the_same_wal_read_snapshot() {
    let (_temp, mut connection, writer) = connections();
    let id = capture(
        &mut CaptureRepository::new(&mut connection).unwrap(),
        "seed",
    )
    .unwrap()
    .item
    .id()
    .clone();
    let request = ConditionalGetRequest {
        item_id: id,
        projection: ItemProjectionKind::Work,
        known_version: None,
    };
    let repository = ProjectionRepository::new(&connection);
    let ConditionalReadOutcome::Modified { version, .. } =
        repository.read_conditional_projection(&request).unwrap()
    else {
        panic!("miss")
    };
    CONTROL.with(|slot| {
        *slot.borrow_mut() = Some(Control {
            writer,
            boundary: "SELECT generation FROM journal_metadata",
            expect_busy: false,
            fired: false,
            rotation_result: None,
            statements: vec![],
        })
    });
    connection.trace_v2(TraceEventCodes::all(), Some(trace));
    let during = repository.read_conditional_projection(&request).unwrap();
    connection.trace_v2(TraceEventCodes::empty(), None);
    let control = CONTROL.with(|slot| slot.borrow_mut().take().unwrap());
    assert!(control.fired);
    assert!(!control.expect_busy);
    control.rotation_result.unwrap().unwrap();
    let ConditionalReadOutcome::Modified {
        version: during_version,
        item,
    } = during
    else {
        panic!("miss")
    };
    assert_eq!(during_version, version);
    let ItemProjection::Work(item) = item else {
        panic!("work")
    };
    assert_eq!(item.title, "original");
    let after = repository
        .read_conditional_projection(&ConditionalGetRequest {
            known_version: Some(version),
            ..request
        })
        .unwrap();
    let ConditionalReadOutcome::Modified { item, .. } = after else {
        panic!("must refetch, never not_modified")
    };
    let ItemProjection::Work(item) = item else {
        panic!("work")
    };
    assert_eq!(item.title, "restored");
    assert!(connection.is_autocommit());
}

#[test]
fn immediate_writer_guard_precedes_receipt_and_serializes_rotation_until_commit() {
    let (_temp, mut connection, writer) = connections();
    CONTROL.with(|slot| {
        *slot.borrow_mut() = Some(Control {
            writer,
            boundary: "SELECT payload_hash, item_id FROM mutation_receipts",
            expect_busy: true,
            fired: false,
            rotation_result: None,
            statements: vec![],
        })
    });
    connection.trace_v2(TraceEventCodes::all(), Some(trace));
    let mut repository = CaptureRepository::new(&mut connection).unwrap();
    capture(&mut repository, "first").unwrap();
    let mut control = CONTROL.with(|slot| slot.borrow_mut().take().unwrap());
    assert!(control.fired);
    assert!(control.expect_busy);
    assert_eq!(
        control
            .rotation_result
            .take()
            .unwrap()
            .unwrap_err()
            .sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    assert!(control.writer.is_autocommit());
    let begin = control
        .statements
        .iter()
        .position(|s| s == "BEGIN IMMEDIATE")
        .unwrap();
    let guard = control
        .statements
        .iter()
        .enumerate()
        .skip(begin)
        .find(|(_, s)| s.contains("SELECT generation FROM journal_metadata"))
        .unwrap()
        .0;
    let receipt = control
        .statements
        .iter()
        .position(|s| s.contains("SELECT payload_hash, item_id"))
        .unwrap();
    let commit = control
        .statements
        .iter()
        .enumerate()
        .skip(receipt)
        .find(|(_, s)| *s == "COMMIT")
        .unwrap()
        .0;
    assert!(begin < guard && guard < receipt && receipt < commit);
    // The competing rotation can commit only once the guarded write has ended.
    control.writer.execute_batch(ROTATE).unwrap();
    CONTROL.with(|slot| *slot.borrow_mut() = Some(control));
    for key in ["first", "new"] {
        assert!(matches!(
            capture(&mut repository, key),
            Err(CaptureStoreError::RestartRequired)
        ));
    }
    drop(repository);
    connection.trace_v2(TraceEventCodes::empty(), None);
    let control = CONTROL.with(|slot| slot.borrow_mut().take().unwrap());
    assert_eq!(
        control
            .writer
            .query_row("SELECT count(*) FROM mutation_receipts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(connection.is_autocommit());
}

#[test]
fn constructors_validate_generation_without_repair_and_release_their_snapshot() {
    let (_temp, mut connection, writer) = connections();
    let _ = CaptureRepository::new(&mut connection).unwrap();
    assert!(connection.is_autocommit());
    let _ = MutationRepository::new(&mut connection).unwrap();
    assert!(connection.is_autocommit());
    writer
        .execute_batch(
            "PRAGMA ignore_check_constraints = ON; UPDATE journal_metadata SET generation = 'bad'",
        )
        .unwrap();
    assert!(CaptureRepository::new(&mut connection).is_err());
    assert!(MutationRepository::new(&mut connection).is_err());
    assert!(connection.is_autocommit());
    assert_eq!(
        writer
            .query_row("SELECT generation FROM journal_metadata", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "bad"
    );
    writer.execute("DELETE FROM journal_metadata", []).unwrap();
    assert!(CaptureRepository::new(&mut connection).is_err());
    assert!(MutationRepository::new(&mut connection).is_err());
}
