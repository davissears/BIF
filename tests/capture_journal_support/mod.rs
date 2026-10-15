//! Shared persisted-state assertions for capture journal and process-reopen tests.
use bif::{
    application::{
        Actor, ActorKind, CaptureIdentity, CaptureInput, CaptureRequest, CaptureResult,
        CaptureStore, CaptureStoreError, Execution,
    },
    domain::{
        ItemContent, MessageId, ProjectId, Provenance, RepositoryReference, RequesterId,
        RevisionReference, SourceHost, SourceUrl, ThreadId, Timestamp,
    },
    storage::{CaptureRepository, CaptureStorageError},
};
use rusqlite::{Connection, types::Value};

/// Full values, not just counts: includes both allocators and both metadata tables.
pub fn persisted(connection: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
    [
        "projects",
        "requester_project_counters",
        "items",
        "item_provenance",
        "item_acceptance_criteria",
        "operations",
        "events",
        "mutation_receipts",
        "change_journal",
        "journal_metadata",
        "store_metadata",
        "sqlite_sequence",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        (table.to_owned(), rows)
    })
    .collect()
}

pub fn capture(
    connection: &mut Connection,
    key: &str,
    identity: &str,
    project: &str,
) -> Result<CaptureResult, CaptureStoreError<CaptureStorageError>> {
    capture_with_repository(
        &mut CaptureRepository::new(connection).unwrap(),
        key,
        identity,
        project,
    )
}

/// Allow the process harness to arm tracing after handle initialization.
pub fn capture_with_repository(
    repository: &mut CaptureRepository<'_>,
    key: &str,
    identity: &str,
    project: &str,
) -> Result<CaptureResult, CaptureStoreError<CaptureStorageError>> {
    repository.capture(
        CaptureRequest {
            idempotency_key: key.into(),
            input: CaptureInput {
                requester: RequesterId::new("Davis").unwrap(),
                project: ProjectId::new(project).unwrap(),
                content: ItemContent::new(
                    "journal capture",
                    Some("complete content".into()),
                    vec!["first".into(), "second".into()],
                )
                .unwrap(),
                provenance: Provenance::new(
                    Some(SourceHost::Delta),
                    Some(ThreadId::new("thread")),
                    Some(MessageId::new("message")),
                    Some(SourceUrl::new("https://example.test/source")),
                    Some(RepositoryReference::new("owner/repo")),
                    Some(RevisionReference::new("revision")),
                    Some("context".into()),
                ),
            },
        },
        format!("payload-{project}"),
        Actor {
            kind: ActorKind::Agent,
            id: "capture-agent",
            surface: "thread",
            host: "delta",
        },
        Execution::Agent {
            agent_id: "capture-agent",
            surface: "thread",
            host: "delta",
        },
        Timestamp::new("2025-02-03T04:05:06Z"),
        CaptureIdentity {
            operation_id: format!("operation-{identity}"),
            event_id: format!("event-{identity}"),
        },
    )
}

/// Check every journal field against its operation, item, event and receipt.
pub fn assert_complete_journal(connection: &Connection, expected_positions: &[i64]) {
    let rows: Vec<(i64, String, String, String, i64)> = connection
        .prepare(
            "SELECT change_seq, operation_id, item_id, project_id, item_revision
             FROM change_journal ORDER BY change_seq",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.0).collect::<Vec<_>>(),
        expected_positions,
        "exactly one journal position per new capture"
    );
    for (_, operation, item, project, revision) in rows {
        let linked: i64 = connection
            .query_row(
                "SELECT count(*) FROM operations o
                 JOIN items i ON i.item_id = o.item_id
                 JOIN events e ON e.operation_id = o.operation_id
                 JOIN mutation_receipts r ON r.operation_id = o.operation_id
                 WHERE o.operation_id = ?1 AND o.item_id = ?2 AND i.project_id = ?3
                   AND o.item_revision = ?4 AND i.revision = ?4 AND e.item_revision = ?4
                   AND e.item_id = ?2 AND r.item_id = ?2
                   AND o.operation_type = 'capture' AND o.expected_revision IS NULL
                   AND e.event_type = 'captured' AND e.event_index = 0
                   AND r.mutation_type = 'capture'",
                rusqlite::params![operation, item, project, revision],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 1);
        assert_eq!(linked, 1, "complete operation/event/receipt-linked row");
    }
    let (high_water, enabled, bootstrap): (i64, i64, i64) = connection
        .query_row(
            "SELECT high_water, sync_enabled, bootstrap_required FROM journal_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(high_water, *expected_positions.last().unwrap());
    assert_eq!(
        (enabled, bootstrap),
        (0, 1),
        "capture must not activate sync"
    );
    let allocator: i64 = connection
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'change_journal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(allocator, high_water);
}
