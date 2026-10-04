mod support;

use std::{cell::RefCell, sync::mpsc};

use bif::{
    application::{
        ItemAudit, ItemListFilters, ItemListOrdering, ItemProjection, ItemProjectionKind,
        ItemProjectionPageRequest, ItemProjectionStore, ItemReadKey, ItemStore, ItemSummary,
        ItemWork, NamedViewStore, PageOffset, PageSize, Pagination, ProjectionGetRequest,
        ReadPageRequest,
    },
    domain::{ItemId, NamedView, ProjectId, RequesterId, Timestamp},
    storage::{self, ItemRepository, ItemStorageError, ProjectionRepository},
};
use rusqlite::{
    Connection, params,
    trace::{TraceEvent, TraceEventCodes},
};

#[derive(Default)]
struct Trace {
    statements: Vec<String>,
    // SQLite ROW callbacks count the sentinel too, not just decoded payloads.
    primary_sql_rows: usize,
    child_rows: usize,
}

thread_local! {
    static TRACE: RefCell<Trace> = RefCell::default();
    static WRITER: RefCell<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { RefCell::new(None) };
}

fn trace(event: TraceEvent<'_>) {
    match event {
        TraceEvent::Stmt(statement, _) => TRACE.with(|trace| {
            trace
                .borrow_mut()
                .statements
                .push(statement.expanded_sql().unwrap());
        }),
        TraceEvent::Row(statement) => TRACE.with(|trace| {
            let mut trace = trace.borrow_mut();
            if statement.sql().contains("FROM items AS i") {
                trace.primary_sql_rows += 1;
            } else if statement.sql().contains("FROM item_acceptance_criteria") {
                trace.child_rows += 1;
            }
        }),
        TraceEvent::Profile(statement, _) if statement.sql().contains("FROM items AS i") => {
            WRITER.with(|writer| {
                if let Some((start, done)) = writer.borrow_mut().take() {
                    start.send(()).unwrap();
                    done.recv().unwrap();
                }
            });
        }
        _ => {}
    }
}

fn start_trace(connection: &Connection) {
    TRACE.with(|trace| *trace.borrow_mut() = Trace::default());
    connection.trace_v2(TraceEventCodes::all(), Some(trace));
}

fn take_trace(connection: &Connection) -> Trace {
    connection.trace_v2(TraceEventCodes::empty(), None);
    TRACE.with(|trace| std::mem::take(&mut *trace.borrow_mut()))
}

fn id(sequence: u64) -> ItemId {
    ItemId::new(
        RequesterId::new("ALICE").unwrap(),
        ProjectId::new("bif").unwrap(),
        sequence,
    )
    .unwrap()
}

fn seed(connection: &Connection, sequences: &[u64]) {
    connection
        .execute(
            "INSERT OR IGNORE INTO projects VALUES ('bif', 'opaque')",
            [],
        )
        .unwrap();
    for (index, sequence) in sequences.iter().enumerate() {
        let item_id = id(*sequence).to_string();
        connection
            .execute(
                "INSERT INTO items (item_id, requester, project_id, sequence, title, description,
                status, priority, assignee, status_reason, revision, captured_at, updated_at)
             VALUES (?1, 'ALICE', 'bif', ?2, 'Title %_ É NUL', 'Description',
                ?3, ?4, ?5, 'Reason', 7, ?6, 'updated')",
                params![
                    item_id,
                    i64::try_from(*sequence).unwrap(),
                    [
                        "proposed",
                        "ready",
                        "in_progress",
                        "blocked",
                        "done",
                        "rejected"
                    ][index % 6],
                    [
                        Some("P0"),
                        Some("P1"),
                        Some("P2"),
                        Some("P3"),
                        Some("P4"),
                        None
                    ][index % 6],
                    (index % 2 == 0).then_some("alice"),
                    ["z", "Z", "a' OR 1=1 --"][index % 3]
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO item_provenance VALUES (?1, 'delta', 'thread', 'message',
                'url', 'repo', 'rev', 'context')",
                [&item_id],
            )
            .unwrap();
        for (index, criterion) in ["second needle", "first"].iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO item_acceptance_criteria VALUES (?1, ?2, ?3)",
                    params![item_id, index as i64, criterion],
                )
                .unwrap();
        }
    }
}

fn request(kind: ItemProjectionKind, limit: usize) -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view: NamedView::All,
        configured_requester: RequesterId::new("alice").unwrap(),
        filters: ItemListFilters::default(),
        projection: kind,
        ordering: ItemListOrdering::NewestFirst,
        page: ReadPageRequest::new(limit, None).unwrap(),
    }
}

const KINDS: [ItemProjectionKind; 3] = [
    ItemProjectionKind::Summary,
    ItemProjectionKind::Work,
    ItemProjectionKind::Audit,
];

/// Tied coordinates isolate identity ordering while retaining valid audit data.
fn seed_identity(connection: &Connection, requester: &str, project: &str) -> ItemId {
    let id = ItemId::new(
        RequesterId::new(requester).unwrap(),
        ProjectId::new(project).unwrap(),
        1,
    )
    .unwrap();
    connection
        .execute(
            "INSERT OR IGNORE INTO projects VALUES (?1, 'tie')",
            [project],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO items (item_id, requester, project_id, sequence, title,
                status, revision, captured_at, updated_at)
             VALUES (?1, ?2, ?3, 1, 'Title', 'proposed', 1, 'tie', 'tie')",
            params![id.to_string(), requester, project],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO item_provenance (item_id) VALUES (?1)",
            [id.to_string()],
        )
        .unwrap();
    id
}

fn assert_invalid_identity(error: ItemStorageError, id: &ItemId) {
    match error {
        ItemStorageError::InvalidPersistedData { item_id, .. } => {
            assert_eq!(item_id, id.to_string());
        }
        other => panic!("expected invalid persisted identity for {id}, got {other:?}"),
    }
}

#[test]
fn noncanonical_identity_components_are_rejected_for_get_and_both_page_orderings() {
    for (column, raw, bypass_checks) in [
        ("requester", "alice-team", true),
        ("requester", "aLiCe-TeAm", true),
        ("requester", " ALICE-TEAM", false),
        ("requester", "ALICE-TEAM ", false),
        ("requester", "ALICE_TEAM", false),
        ("requester", "ALICE--TEAM", false),
        ("project_id", "CORE-API", true),
        ("project_id", "cOrE-aPi", true),
        ("project_id", " core-api", false),
        ("project_id", "core-api ", false),
        ("project_id", "core_api", false),
        ("project_id", "core--api", false),
    ] {
        let connection = storage::open(":memory:").unwrap();
        let id = seed_identity(&connection, "ALICE-TEAM", "core-api");
        // Only impossible casing states bypass CHECKs; foreign keys stay on.
        if bypass_checks {
            connection
                .execute_batch("PRAGMA ignore_check_constraints = ON")
                .unwrap();
        }
        if column == "project_id" {
            assert_eq!(ProjectId::new(raw).unwrap(), *id.project());
            connection
                .execute("INSERT INTO projects VALUES (?1, 'tie')", [raw])
                .unwrap();
        } else {
            assert_eq!(RequesterId::new(raw).unwrap(), *id.requester());
        }
        connection
            .execute(&format!("UPDATE items SET {column} = ?1"), [raw])
            .unwrap();
        assert!(
            connection
                .pragma_query_value(None, "foreign_keys", |row| row.get::<_, bool>(0))
                .unwrap()
        );
        assert_eq!(
            connection
                .pragma_query_value(None, "ignore_check_constraints", |row| row
                    .get::<_, bool>(0))
                .unwrap(),
            bypass_checks
        );
        let repo = ProjectionRepository::new(&connection);
        for kind in KINDS {
            assert_invalid_identity(
                repo.read_projection(&ProjectionGetRequest {
                    item_id: id.clone(),
                    projection: kind,
                })
                .unwrap_err(),
                &id,
            );
            for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
                let mut request = request(kind, 1);
                request.ordering = ordering;
                assert_invalid_identity(repo.select_projection_page(&request).unwrap_err(), &id);
                assert!(connection.is_autocommit());
            }
        }
    }
}

#[test]
fn constraint_enabled_bob_space_cannot_emit_a_repeating_boundary() {
    let connection = storage::open(":memory:").unwrap();
    let bob = seed_identity(&connection, "BOB", "core");
    seed_identity(&connection, "CHARLIE", "core");
    connection
        .execute(
            "UPDATE items SET requester = 'BOB ' WHERE item_id = ?1",
            [bob.to_string()],
        )
        .unwrap();
    let repo = ProjectionRepository::new(&connection);
    for kind in KINDS {
        for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
            let mut request = request(kind, 1);
            request.ordering = ordering;
            assert_invalid_identity(repo.select_projection_page(&request).unwrap_err(), &bob);
            // This canonical key used to select raw BOB-space again forever.
            request.page.after = Some(ItemReadKey {
                id: bob.clone(),
                captured_at: Timestamp::new("tie"),
                priority: None,
            });
            assert_invalid_identity(repo.select_projection_page(&request).unwrap_err(), &bob);
            assert!(connection.is_autocommit());
        }
    }
}

#[test]
fn noncanonical_sentinel_errors_only_when_selected_by_continuation() {
    for (first_requester, first_project, corrupt_requester, corrupt_project, column, raw, bypass) in [
        (
            "BOB",
            "core",
            "CHARLIE",
            "core",
            "requester",
            "CHARLIE ",
            false,
        ),
        ("BOB", "core", "BOB", "zeta", "project_id", "zeta ", false),
        // Raw bOB sorts after CHARLIE, contrary to canonical identity ordering.
        ("CHARLIE", "core", "BOB", "core", "requester", "bOB", true),
    ] {
        let connection = storage::open(":memory:").unwrap();
        let first = seed_identity(&connection, first_requester, first_project);
        let corrupt = seed_identity(&connection, corrupt_requester, corrupt_project);
        if bypass {
            connection
                .execute_batch("PRAGMA ignore_check_constraints = ON")
                .unwrap();
        }
        if column == "project_id" {
            connection
                .execute("INSERT INTO projects VALUES (?1, 'tie')", [raw])
                .unwrap();
        }
        connection
            .execute(
                &format!("UPDATE items SET {column} = ?1 WHERE item_id = ?2"),
                params![raw, corrupt.to_string()],
            )
            .unwrap();
        let repo = ProjectionRepository::new(&connection);
        for kind in KINDS {
            for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
                let mut request = request(kind, 1);
                request.ordering = ordering;
                let page = repo.select_projection_page(&request).unwrap();
                assert_eq!(page.records.len(), 1);
                assert_eq!(page.records[0].key.id, first);
                assert!(page.has_more);
                request.page.after = Some(page.records[0].key.clone());
                assert_invalid_identity(
                    repo.select_projection_page(&request).unwrap_err(),
                    &corrupt,
                );
                assert!(connection.is_autocommit());
            }
        }
    }
}

fn assert_blob_error(error: ItemStorageError, column: &str) {
    assert!(
        matches!(
            error,
            ItemStorageError::Sqlite(rusqlite::Error::InvalidColumnType(
                _, ref name, rusqlite::types::Type::Blob
            )) if name == column
        ),
        "expected BLOB type error for {column}, got {error:?}"
    );
}

/// Corrupt child payloads make accidental sentinel hydration fail as well.
fn corrupt_sentinel(connection: &Connection, table: &str, column: &str) {
    connection
        .execute(
            &format!("UPDATE {table} SET {column} = x'FF' WHERE item_id = ?1"),
            [id(2).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE item_acceptance_criteria SET criterion = x'FF' WHERE item_id = ?1",
            [id(2).to_string()],
        )
        .unwrap();
}

fn assert_content_sentinel(kind: ItemProjectionKind, table: &str, column: &str) {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2]); // 'z' sorts before 'Z' in NewestFirst.
    let repo = ProjectionRepository::new(&connection);
    let expected = repo
        .read_projection(&ProjectionGetRequest {
            item_id: id(1),
            projection: kind,
        })
        .unwrap()
        .unwrap();
    let summaries = repo
        .select_projection_page(&request(ItemProjectionKind::Summary, 2))
        .unwrap();
    corrupt_sentinel(&connection, table, column);

    start_trace(&connection);
    let mut page_request = request(kind, 1);
    let page = repo.select_projection_page(&page_request).unwrap();
    let trace = take_trace(&connection);
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].item, expected);
    assert!(page.has_more);
    assert_eq!(trace.primary_sql_rows, 2);
    assert_eq!(trace.child_rows, 2); // Only the returned item's children.
    assert_eq!(
        trace
            .statements
            .iter()
            .filter(|sql| sql.starts_with("SELECT"))
            .count(),
        2
    );
    assert!(connection.is_autocommit());

    // Summary ignores content, provenance and criteria even when selected.
    assert_eq!(
        repo.select_projection_page(&request(ItemProjectionKind::Summary, 2))
            .unwrap(),
        summaries
    );
    page_request.page.after = Some(page.records[0].key.clone());
    assert_blob_error(
        repo.select_projection_page(&page_request).unwrap_err(),
        column,
    );
    assert_blob_error(
        repo.read_projection(&ProjectionGetRequest {
            item_id: id(2),
            projection: kind,
        })
        .unwrap_err(),
        column,
    );
    assert!(connection.is_autocommit());
}

#[test]
fn work_sql_type_corrupt_content_sentinel_is_only_read_when_selected() {
    assert_content_sentinel(ItemProjectionKind::Work, "items", "description");
}

#[test]
fn audit_sql_type_corrupt_content_sentinel_is_only_read_when_selected() {
    assert_content_sentinel(ItemProjectionKind::Audit, "items", "description");
}

#[test]
fn audit_sql_type_corrupt_provenance_sentinel_is_only_read_when_selected() {
    assert_content_sentinel(
        ItemProjectionKind::Audit,
        "item_provenance",
        "context_excerpt",
    );
}

#[test]
fn sql_type_corrupt_summary_sentinel_is_only_read_when_selected() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2]);
    // A nonempty BLOB satisfies the schema's title check without bypassing it
    // and does not participate in this page's membership or ordering.
    corrupt_sentinel(&connection, "items", "title");
    let repo = ProjectionRepository::new(&connection);
    let mut request = request(ItemProjectionKind::Summary, 1);
    start_trace(&connection);
    let page = repo.select_projection_page(&request).unwrap();
    let trace = take_trace(&connection);
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].key.id, id(1));
    assert!(page.has_more);
    assert_eq!(trace.primary_sql_rows, 2);
    assert_eq!(trace.child_rows, 0);
    assert_eq!(
        trace
            .statements
            .iter()
            .filter(|sql| sql.starts_with("SELECT"))
            .count(),
        1
    );
    request.page.after = Some(page.records[0].key.clone());
    assert_blob_error(repo.select_projection_page(&request).unwrap_err(), "title");
    assert!(connection.is_autocommit());
}

fn assert_legacy_sentinel(table: &str, column: &str) {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2]);
    let repo = ItemRepository::new(&connection);
    let expected = repo.read_item(&id(1)).unwrap().unwrap();
    corrupt_sentinel(&connection, table, column);
    let select = |offset| {
        repo.select_item_page(
            NamedView::All,
            &RequesterId::new("alice").unwrap(),
            &ItemListFilters::default(),
            ItemListOrdering::NewestFirst,
            Pagination::new(PageSize::new(1).unwrap(), PageOffset::new(offset)),
        )
    };
    start_trace(&connection);
    let page = select(0).unwrap();
    let trace = take_trace(&connection);
    assert_eq!(page.items, [expected]); // Complete legacy Item, not a projection.
    assert_eq!(page.next_offset, Some(PageOffset::new(1)));
    assert_eq!(trace.primary_sql_rows, 2);
    assert_eq!(trace.child_rows, 2);
    assert_eq!(
        trace
            .statements
            .iter()
            .filter(|sql| sql.starts_with("SELECT"))
            .count(),
        2
    );
    assert_blob_error(select(1).unwrap_err(), column);
    assert!(connection.is_autocommit());
}

#[test]
fn legacy_offset_sql_type_corrupt_content_sentinel_is_only_read_when_selected() {
    assert_legacy_sentinel("items", "description");
}

#[test]
fn legacy_offset_sql_type_corrupt_provenance_sentinel_is_only_read_when_selected() {
    assert_legacy_sentinel("item_provenance", "context_excerpt");
}

#[test]
fn bounded_statement_and_row_counts_are_constant_and_sentinel_is_not_hydrated() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &(1..=150).collect::<Vec<_>>());
    for kind in KINDS {
        for limit in [1, 10, 100] {
            start_trace(&connection);
            let page = ProjectionRepository::new(&connection)
                .select_projection_page(&request(kind, limit))
                .unwrap();
            let trace = take_trace(&connection);
            assert_eq!(page.records.len(), limit);
            assert!(page.has_more);
            assert_eq!(trace.primary_sql_rows, limit + 1);
            assert_eq!(
                trace.child_rows,
                if kind == ItemProjectionKind::Summary {
                    0
                } else {
                    limit * 2
                }
            );
            let data: Vec<_> = trace
                .statements
                .iter()
                .filter(|sql| sql.starts_with("SELECT"))
                .collect();
            assert_eq!(
                data.len(),
                if kind == ItemProjectionKind::Summary {
                    1
                } else {
                    2
                },
                "{data:?}"
            );
            assert!(!data.iter().any(|sql| sql.contains("events")));
            if kind == ItemProjectionKind::Summary {
                assert!(!data[0].contains("description"));
                assert!(!data[0].contains("provenance"));
                assert!(!data[0].contains("criteria"));
            }
            assert_eq!(
                trace
                    .statements
                    .iter()
                    .filter(|sql| sql.starts_with("BEGIN"))
                    .count(),
                1
            );
            assert_eq!(
                trace
                    .statements
                    .iter()
                    .filter(|sql| sql.starts_with("COMMIT"))
                    .count(),
                1
            );
            assert!(connection.is_autocommit());
        }
    }
}

#[test]
fn get_and_page_match_every_v1_projection_field_and_missing_get_is_none() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2, 3, 4, 5, 6]);
    // Raw persisted assignees use the same canonical normalization in v1 and v2.
    connection
        .execute(
            "UPDATE items SET assignee = 'invalid space' WHERE item_id = ?1",
            [id(1).to_string()],
        )
        .unwrap();
    for kind in KINDS {
        for sequence in 1..=6 {
            let expected = ItemRepository::new(&connection)
                .read_item(&id(sequence))
                .unwrap()
                .unwrap();
            if sequence == 1 {
                assert_eq!(expected.assignee().unwrap().as_str(), "invalid-space");
            }
            let expected = match kind {
                ItemProjectionKind::Summary => {
                    ItemProjection::Summary(ItemSummary::from(&expected))
                }
                ItemProjectionKind::Work => ItemProjection::Work(ItemWork::from(&expected)),
                ItemProjectionKind::Audit => ItemProjection::Audit(ItemAudit::from(&expected)),
            };
            let actual = ProjectionRepository::new(&connection)
                .read_projection(&ProjectionGetRequest {
                    item_id: id(sequence),
                    projection: kind,
                })
                .unwrap()
                .unwrap();
            assert_eq!(actual, expected);
            let page = ProjectionRepository::new(&connection)
                .select_projection_page(&request(kind, 100))
                .unwrap();
            assert_eq!(
                page.records
                    .iter()
                    .find(|row| row.key.id == id(sequence))
                    .unwrap()
                    .item,
                expected
            );
            assert!(!page.has_more);
        }
        assert!(
            ProjectionRepository::new(&connection)
                .read_projection(&ProjectionGetRequest {
                    item_id: id(u64::MAX),
                    projection: kind,
                })
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn empty_pages_skip_child_queries_and_release_snapshot() {
    let connection = storage::open(":memory:").unwrap();
    for kind in KINDS {
        start_trace(&connection);
        let page = ProjectionRepository::new(&connection)
            .select_projection_page(&request(kind, 10))
            .unwrap();
        let trace = take_trace(&connection);
        assert!(page.records.is_empty());
        assert!(!page.has_more);
        assert_eq!(
            trace
                .statements
                .iter()
                .filter(|sql| sql.starts_with("SELECT"))
                .count(),
            1
        );
        assert!(connection.is_autocommit());
    }
}

#[test]
fn selection_order_and_boundaries_match_v1_across_filters_and_full_width_sequences() {
    let connection = storage::open(":memory:").unwrap();
    seed(
        &connection,
        &[1, 2, 3, 4, 5, 6, 999, 1000, (1 << 53) + 1, i64::MAX as u64],
    );
    let repo = ProjectionRepository::new(&connection);
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        for view in [
            NamedView::All,
            NamedView::Ready,
            NamedView::Active,
            NamedView::Mine,
            NamedView::Done,
            NamedView::Rejected,
            NamedView::Blocked,
            NamedView::Proposed,
        ] {
            for text in [
                None,
                Some("NEEDLE"),
                Some("%_"),
                Some("É"),
                Some("é"),
                Some(" NEEDLE "),
                Some("' OR 1=1 --"),
                Some("absent"),
            ] {
                let mut request = request(ItemProjectionKind::Summary, 1);
                request.ordering = ordering;
                request.view = view;
                request.filters.text =
                    text.map(|value| bif::application::ItemTextFilter::new(value).unwrap());
                let mut expected = ItemRepository::new(&connection)
                    .select_items(view, &request.configured_requester, &request.filters)
                    .unwrap();
                expected.sort_by(|left, right| {
                    ordering.compare_keys(
                        &ItemReadKey {
                            id: left.id().clone(),
                            captured_at: left.captured_at().clone(),
                            priority: left.priority(),
                        },
                        &ItemReadKey {
                            id: right.id().clone(),
                            captured_at: right.captured_at().clone(),
                            priority: right.priority(),
                        },
                    )
                });
                let mut actual = Vec::new();
                loop {
                    let page = repo.select_projection_page(&request).unwrap();
                    actual.extend(page.records.iter().map(|row| row.key.id.clone()));
                    if !page.has_more {
                        break;
                    }
                    request.page.after = Some(page.records.last().unwrap().key.clone());
                }
                assert_eq!(
                    actual,
                    expected
                        .iter()
                        .map(|item| item.id().clone())
                        .collect::<Vec<_>>()
                );
                for sequence in [i64::MAX as u64 + 1, u64::MAX] {
                    let boundary = ItemReadKey {
                        id: id(sequence),
                        captured_at: Timestamp::new("z"),
                        priority: None,
                    };
                    request.page = ReadPageRequest::new(100, Some(boundary.clone())).unwrap();
                    let actual = repo.select_projection_page(&request).unwrap();
                    let expected: Vec<_> = expected
                        .iter()
                        .filter(|item| {
                            ordering.is_after(
                                &ItemReadKey {
                                    id: item.id().clone(),
                                    captured_at: item.captured_at().clone(),
                                    priority: item.priority(),
                                },
                                &boundary,
                            )
                        })
                        .map(|item| item.id().clone())
                        .collect();
                    assert_eq!(
                        actual
                            .records
                            .iter()
                            .map(|row| row.key.id.clone())
                            .collect::<Vec<_>>(),
                        expected
                    );
                }
            }
        }
    }
}

#[test]
fn tied_numeric_identity_order_and_sentinel_corruption_are_bounded() {
    let connection = storage::open(":memory:").unwrap();
    let sequences = [999, 1000, (1 << 53) + 1, (1 << 53) + 2, i64::MAX as u64];
    seed(&connection, &sequences);
    connection
        .execute("UPDATE items SET captured_at = 'tie', priority = NULL", [])
        .unwrap();
    for kind in KINDS {
        for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
            let mut request = request(kind, 1);
            request.ordering = ordering;
            let mut actual = Vec::new();
            loop {
                let page = ProjectionRepository::new(&connection)
                    .select_projection_page(&request)
                    .unwrap();
                actual.extend(page.records.iter().map(|row| row.key.id.sequence()));
                if !page.has_more {
                    break;
                }
                request.page.after = Some(page.records.last().unwrap().key.clone());
            }
            assert_eq!(actual, sequences);
        }
    }
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    connection
        .execute(
            "UPDATE item_provenance SET source_host = 'invalid' WHERE item_id = ?1",
            [id(1000).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE item_acceptance_criteria SET criterion = x'FF' WHERE item_id = ?1",
            [id(1000).to_string()],
        )
        .unwrap();
    for kind in [ItemProjectionKind::Work, ItemProjectionKind::Audit] {
        let page = ProjectionRepository::new(&connection)
            .select_projection_page(&request(kind, 1))
            .unwrap();
        assert_eq!(page.records.len(), 1);
        assert!(page.has_more);
        assert_eq!(page.records[0].key.id, id(999));
    }
}

#[test]
fn explicit_filter_intersections_match_v1_and_invalid_filters_do_not_open_a_transaction() {
    use bif::domain::{AssigneeId, Priority, Status};
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &(1..=12).collect::<Vec<_>>());
    let filters = [
        ItemListFilters {
            project: Some(ProjectId::new("bif").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            project: Some(ProjectId::new("other").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            requester: Some(RequesterId::new("alice").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            requester: Some(RequesterId::new("bob").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            assignee: Some(AssigneeId::new("ALICE").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            assignee: Some(AssigneeId::new("bob").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            unassigned: true,
            ..Default::default()
        },
        ItemListFilters {
            status: Some(Status::Done),
            ..Default::default()
        },
        ItemListFilters {
            priority: Some(Priority::P2),
            ..Default::default()
        },
        ItemListFilters {
            status: Some(Status::Ready),
            priority: Some(Priority::P0),
            ..Default::default()
        },
    ];
    for view in [NamedView::All, NamedView::Active, NamedView::Mine] {
        for filters in &filters {
            let mut request = request(ItemProjectionKind::Summary, 100);
            request.view = view;
            request.filters = filters.clone();
            let expected = ItemRepository::new(&connection)
                .select_items(view, &request.configured_requester, filters)
                .unwrap();
            let actual = ProjectionRepository::new(&connection)
                .select_projection_page(&request)
                .unwrap();
            let mut expected: Vec<_> = expected.into_iter().map(|item| item.id().clone()).collect();
            let mut actual: Vec<_> = actual.records.into_iter().map(|row| row.key.id).collect();
            expected.sort();
            actual.sort();
            assert_eq!(actual, expected);
        }
    }
    let mut invalid = request(ItemProjectionKind::Audit, 10);
    invalid.filters.assignee = Some(AssigneeId::new("alice").unwrap());
    invalid.filters.unassigned = true;
    start_trace(&connection);
    assert!(
        ProjectionRepository::new(&connection)
            .select_projection_page(&invalid)
            .is_err()
    );
    assert!(take_trace(&connection).statements.is_empty());
    assert!(connection.is_autocommit());
}

#[test]
fn explain_uses_actual_bound_query_and_existing_provenance_key_without_events() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1]);
    let mut request = request(ItemProjectionKind::Audit, 10);
    request.view = NamedView::Active;
    request.page.after = Some(ItemReadKey {
        id: id(u64::MAX),
        captured_at: Timestamp::new("' OR 1=1 --"),
        priority: None,
    });
    start_trace(&connection);
    let plan = ProjectionRepository::new(&connection)
        .explain_projection_page(&request)
        .unwrap();
    let trace = take_trace(&connection);
    assert!(
        plan.iter().any(|detail| detail.contains("LEFT-JOIN")),
        "{plan:?}"
    );
    assert!(trace.statements.iter().all(|sql| !sql.contains("events")));
    assert!(plan.iter().all(|detail| !detail.contains("events")));
}

#[test]
fn corrupt_requested_fields_are_errors_but_unrequested_children_are_not_read() {
    for sql in [
        "UPDATE items SET title = ''",
        "UPDATE items SET status = 'unknown'",
        "UPDATE items SET priority = 'p1'",
        "UPDATE items SET assignee = '!!!'",
        "UPDATE items SET revision = -1",
        "UPDATE items SET requester = 'invalid space'",
        "UPDATE items SET sequence = -1",
    ] {
        let connection = storage::open(":memory:").unwrap();
        seed(&connection, &[1]);
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        connection.execute(sql, []).unwrap();
        for kind in KINDS {
            assert!(
                ProjectionRepository::new(&connection)
                    .read_projection(&ProjectionGetRequest {
                        item_id: id(1),
                        projection: kind,
                    })
                    .is_err(),
                "{sql}"
            );
        }
        assert!(connection.is_autocommit());
    }
    for sql in [
        "DELETE FROM item_provenance",
        "UPDATE item_provenance SET source_host = 'invalid'",
        "UPDATE item_acceptance_criteria SET criterion = x'FF' WHERE criterion_index = 0",
    ] {
        let connection = storage::open(":memory:").unwrap();
        seed(&connection, &[1]);
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        connection.execute(sql, []).unwrap();
        let repo = ProjectionRepository::new(&connection);
        assert!(
            repo.read_projection(&ProjectionGetRequest {
                item_id: id(1),
                projection: ItemProjectionKind::Summary
            })
            .unwrap()
            .is_some()
        );
        let result = repo.read_projection(&ProjectionGetRequest {
            item_id: id(1),
            projection: ItemProjectionKind::Audit,
        });
        assert!(result.is_err(), "{sql}");
        if sql.contains("provenance") {
            assert!(matches!(
                result,
                Err(ItemStorageError::InvalidPersistedData { .. })
            ));
            assert!(
                repo.read_projection(&ProjectionGetRequest {
                    item_id: id(1),
                    projection: ItemProjectionKind::Work
                })
                .unwrap()
                .is_some()
            );
        }
    }
}

#[test]
fn concurrent_writer_commits_between_primary_and_children_but_assembly_uses_one_snapshot() {
    let directory = support::OwnedTestDirectory::new();
    let path = directory.path().join("snapshot.sqlite");
    let connection = storage::open(&path).unwrap();
    seed(&connection, &[1]);
    let (start, wait) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let connection = storage::open(&path).unwrap();
        wait.recv().unwrap();
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
            UPDATE items SET title = 'New', revision = 8;
            UPDATE item_acceptance_criteria SET criterion = 'New criterion';
            COMMIT;",
            )
            .unwrap();
        done.send(()).unwrap();
    });
    WRITER.with(|writer| *writer.borrow_mut() = Some((start, finished)));
    start_trace(&connection);
    let result = ProjectionRepository::new(&connection)
        .read_projection(&ProjectionGetRequest {
            item_id: id(1),
            projection: ItemProjectionKind::Audit,
        })
        .unwrap()
        .unwrap();
    take_trace(&connection);
    writer.join().unwrap();
    let ItemProjection::Audit(item) = result else {
        panic!("audit required")
    };
    assert_eq!(item.title, "Title %_ É NUL");
    assert_eq!(item.revision.get(), 7);
    assert_eq!(item.acceptance_criteria, ["second needle", "first"]);
    assert!(connection.is_autocommit());
    assert_eq!(
        connection
            .query_row("SELECT title FROM items", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "New"
    );
}
