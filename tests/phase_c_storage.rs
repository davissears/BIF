//! Phase C storage regressions: bounded keysets/history and legacy offset reads.
mod support;

use std::{cell::RefCell, sync::mpsc};

use bif::{
    application::{
        HistoryOrdering, HistoryPageRequest, HistoryReadKey, ItemAudit, ItemHistoryPageStore,
        ItemHistoryStore, ItemHistoryStoreError, ItemListFilters, ItemListOrdering, ItemProjection,
        ItemProjectionKind, ItemProjectionPageRequest, ItemProjectionStore, ItemStore,
        ItemTextFilter, NamedViewStore, PageOffset, PageSize, Pagination, ReadPageRequest,
    },
    domain::{AssigneeId, ItemId, NamedView, ProjectId, RequesterId, Revision, Status},
    storage::{
        self, ItemHistoryRepository, ItemRepository, ItemStorageError, ProjectionRepository,
    },
};
use rusqlite::{
    Connection, StatementStatus, params,
    trace::{TraceEvent, TraceEventCodes},
};

thread_local! {
    static SQL: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static ROWS: RefCell<(usize, usize)> = const { RefCell::new((0, 0)) };
    static WORK: RefCell<(i64, i64)> = const { RefCell::new((0, 0)) };
    static WRITER: RefCell<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { RefCell::new(None) };
}

fn trace(event: TraceEvent<'_>) {
    match event {
        TraceEvent::Stmt(statement, _) => {
            SQL.with(|sql| sql.borrow_mut().push(statement.expanded_sql().unwrap()))
        }
        TraceEvent::Row(statement) => ROWS.with(|rows| {
            let mut rows = rows.borrow_mut();
            if statement.sql().contains("FROM items AS i") {
                rows.0 += 1;
            }
            if statement.sql().contains("FROM item_acceptance_criteria") {
                rows.1 += 1;
            }
        }),
        TraceEvent::Profile(statement, _) => {
            WORK.with(|work| {
                let mut work = work.borrow_mut();
                work.0 += i64::from(statement.get_status(StatementStatus::VmStep));
                work.1 += i64::from(statement.get_status(StatementStatus::Sort));
            });
            if statement.sql().contains("FROM items AS i")
                || statement.sql().contains("SELECT EXISTS")
            {
                WRITER.with(|writer| {
                    if let Some((start, done)) = writer.borrow_mut().take() {
                        start.send(()).unwrap();
                        done.recv_timeout(std::time::Duration::from_secs(10))
                            .unwrap();
                    }
                });
            }
        }
        _ => {}
    }
}

fn start_trace(connection: &Connection) {
    SQL.with(|sql| sql.borrow_mut().clear());
    ROWS.with(|rows| *rows.borrow_mut() = (0, 0));
    WORK.with(|work| *work.borrow_mut() = (0, 0));
    connection.trace_v2(TraceEventCodes::all(), Some(trace));
}

fn statements(connection: &Connection) -> Vec<String> {
    connection.trace_v2(TraceEventCodes::empty(), None);
    SQL.with(|sql| std::mem::take(&mut *sql.borrow_mut()))
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
            "INSERT OR IGNORE INTO projects VALUES ('bif', 'same-time')",
            [],
        )
        .unwrap();
    for sequence in sequences {
        let item_id = id(*sequence).to_string();
        connection.execute(
            "INSERT INTO items (item_id, requester, project_id, sequence, title, description,
             status, priority, revision, captured_at, updated_at)
             VALUES (?1, 'ALICE', 'bif', ?2, 'Title', 'Description', 'ready', ?3, 1, ?4, 'updated')",
            params![item_id, i64::try_from(*sequence).unwrap(),
                [Some("P0"), Some("P2"), None][(*sequence % 3) as usize],
                ["z", "a", "z"][(*sequence % 3) as usize]],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO item_provenance (item_id) VALUES (?1)",
                [&item_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO item_acceptance_criteria VALUES (?1, 0, 'criterion')",
                [&item_id],
            )
            .unwrap();
    }
}

/// Accepted v1 coordinates need not be canonical, even with schema checks on.
fn seed_legacy_coordinates(connection: &Connection, coordinates: &[(&str, &str, u64)]) {
    for &(requester, project, sequence) in coordinates {
        let item_id = ItemId::new(
            RequesterId::new(requester).unwrap(),
            ProjectId::new(project).unwrap(),
            sequence,
        )
        .unwrap()
        .to_string();
        connection
            .execute(
                "INSERT OR IGNORE INTO projects VALUES (?1, 'same-time')",
                [project],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO items (item_id, requester, project_id, sequence, title, description,
                 status, priority, revision, captured_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'Title', 'Description', 'ready', 'P2', 1,
                         'same-time', 'updated')",
                params![
                    item_id,
                    requester,
                    project,
                    i64::try_from(sequence).unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO item_provenance (item_id) VALUES (?1)",
                [&item_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO item_acceptance_criteria VALUES (?1, 0, 'criterion')",
                [&item_id],
            )
            .unwrap();
    }
}

fn request(kind: ItemProjectionKind, ordering: ItemListOrdering) -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view: NamedView::All,
        configured_requester: RequesterId::new("ALICE").unwrap(),
        filters: ItemListFilters::default(),
        projection: kind,
        ordering,
        page: ReadPageRequest::new(2, None).unwrap(),
    }
}

fn history_request(after: Option<HistoryReadKey>, limit: usize) -> HistoryPageRequest {
    HistoryPageRequest {
        item_id: id(1),
        ordering: HistoryOrdering::RevisionThenEventIndex,
        page: ReadPageRequest::new(limit, after).unwrap(),
    }
}

fn event(connection: &Connection, revision: i64, index: i64, value: &str) {
    let operation = format!("operation-{revision}");
    connection
        .execute(
            "INSERT OR IGNORE INTO operations VALUES (?1, ?2, 'triage', NULL, ?3, 'same-time')",
            params![operation, id(1).to_string(), revision],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO events VALUES (?1, ?2, ?3, ?4, ?5, 'approved', 'proposed', ?6,
         'human', 'alice', 'cli', 'local', 'direct', NULL, 'cli', 'local',
         NULL, NULL, 'same-time', 1)",
            params![
                format!("event-{revision}-{index}"),
                operation,
                id(1).to_string(),
                revision,
                index,
                value
            ],
        )
        .unwrap();
}

#[test]
fn all_projection_traversals_match_legacy_with_ties_and_numeric_identity() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2, 3, 4, 5, 999, 1000, 1001]);
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let legacy = ItemRepository::new(&connection)
            .select_item_page(
                NamedView::All,
                &RequesterId::new("alice").unwrap(),
                &ItemListFilters::default(),
                ordering,
                Pagination::new(PageSize::new(100).unwrap(), PageOffset::new(0)),
            )
            .unwrap()
            .items;
        for kind in [
            ItemProjectionKind::Summary,
            ItemProjectionKind::Work,
            ItemProjectionKind::Audit,
        ] {
            let mut request = request(kind, ordering);
            let repository = ProjectionRepository::new(&connection);
            let mut keys = Vec::new();
            loop {
                start_trace(&connection);
                let page = repository.select_projection_page(&request).unwrap();
                let sql = statements(&connection);
                assert!(sql.iter().all(|sql| !sql.contains("OFFSET")));
                assert!(page.records.iter().all(|row| {
                    request
                        .page
                        .after
                        .as_ref()
                        .is_none_or(|after| ordering.is_after(&row.key, after))
                }));
                keys.extend(page.records.iter().map(|row| row.key.id.clone()));
                if !page.has_more {
                    break;
                }
                request.page.after = Some(page.records.last().unwrap().key.clone());
            }
            assert_eq!(
                keys,
                legacy
                    .iter()
                    .map(|item| item.id().clone())
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn live_pages_use_stored_boundary_not_current_boundary_item() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2, 3, 4, 5, 6]);
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let repository = ProjectionRepository::new(&connection);
        let mut request = request(ItemProjectionKind::Summary, ordering);
        let first = repository.select_projection_page(&request).unwrap();
        let boundary = first.records.last().unwrap().key.clone();
        // Move the boundary itself, remove a queued member and insert live work.
        connection
            .execute(
                "UPDATE items SET captured_at = 'zz', priority = 'P0' WHERE item_id = ?1",
                [boundary.id.to_string()],
            )
            .unwrap();
        connection
            .execute("DELETE FROM items WHERE item_id = ?1", [id(5).to_string()])
            .unwrap();
        seed(&connection, &[100 + ordering as u64]);
        request.page.after = Some(boundary.clone());
        request.page.limit = PageSize::new(100).unwrap();
        let continued = repository.select_projection_page(&request).unwrap();
        let all = repository
            .select_projection_page(&ItemProjectionPageRequest {
                page: ReadPageRequest::new(100, None).unwrap(),
                ..request.clone()
            })
            .unwrap();
        assert_eq!(
            continued.records,
            all.records
                .into_iter()
                .filter(|row| ordering.is_after(&row.key, &boundary))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn legacy_offset_selection_is_bounded_and_batch_hydrated() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &(1..=50).collect::<Vec<_>>());
    let repository = ItemRepository::new(&connection);
    let complete = repository
        .select_items(
            NamedView::All,
            &RequesterId::new("alice").unwrap(),
            &ItemListFilters::default(),
        )
        .unwrap();
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let mut expected = complete.clone();
        expected.sort_by(|a, b| {
            ordering.compare_keys(
                &bif::application::ItemReadKey {
                    id: a.id().clone(),
                    captured_at: a.captured_at().clone(),
                    priority: a.priority(),
                },
                &bif::application::ItemReadKey {
                    id: b.id().clone(),
                    captured_at: b.captured_at().clone(),
                    priority: b.priority(),
                },
            )
        });
        start_trace(&connection);
        let page = repository
            .select_item_page(
                NamedView::All,
                &RequesterId::new("alice").unwrap(),
                &ItemListFilters::default(),
                ordering,
                Pagination::new(PageSize::new(3).unwrap(), PageOffset::new(30)),
            )
            .unwrap();
        let sql = statements(&connection);
        assert_eq!(page.items, expected[30..33]);
        assert_eq!(page.next_offset, Some(PageOffset::new(33)));
        assert_eq!(
            sql.iter().filter(|sql| sql.starts_with("SELECT")).count(),
            2,
            "{sql:#?}"
        );
        assert!(
            sql.iter().any(|sql| sql.contains("LIMIT 4 OFFSET 30")),
            "{sql:#?}"
        );
        ROWS.with(|rows| assert_eq!(*rows.borrow(), (4, 3)));
        assert!(connection.is_autocommit());
    }
    // An unrepresentable usize offset must stay an empty page, not wrap or error.
    let page = repository
        .select_item_page(
            NamedView::All,
            &RequesterId::new("alice").unwrap(),
            &ItemListFilters::default(),
            ItemListOrdering::Next,
            Pagination::new(PageSize::new(3).unwrap(), PageOffset::new(usize::MAX)),
        )
        .unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.next_offset, None);
}

#[test]
fn legacy_pages_order_decoded_coordinates_before_limit_and_offset() {
    let connection = storage::open(":memory:").unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA ignore_check_constraints", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    seed_legacy_coordinates(
        &connection,
        &[
            ("ALICE", "bif", 2),
            ("ALICE ", "bif", 1),
            ("A-B", "bif", 2),
            ("A!B", "bif", 1),
            ("A☃B", "bif", 3),
            ("A-B ", "bif", 4),
            ("ALICE", "bi-f", 2),
            ("ALICE", "bi!f", 1),
            ("ALICE", "bi☃f", 3),
            ("ALICE", "bi-f ", 4),
            ("ALICE", "bif", 1000),
            ("ALICE", "bif", 999),
            // Whole display-ID ordering puts '-' before ':', unlike ItemId.
            ("ALICE-Z", "bif", 1),
            ("ALICE", "bif-z", 1),
        ],
    );
    let repository = ItemRepository::new(&connection);
    let configured = RequesterId::new("alice").unwrap();
    let mut expected = repository
        .select_items(NamedView::All, &configured, &ItemListFilters::default())
        .unwrap();
    expected.sort_by(|a, b| a.id().cmp(b.id()));
    // Every fixture is accepted by the original decoder; all leading sort keys tie.
    for item in &expected {
        assert_eq!(
            repository.read_item(item.id()).unwrap().as_ref(),
            Some(item)
        );
    }
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        // Cover every identity (including 999/1000), plus non-aligned/end offsets.
        for offset in (0..expected.len())
            .step_by(3)
            .chain([1, expected.len() - 1, expected.len()])
        {
            start_trace(&connection);
            let page = repository
                .select_item_page(
                    NamedView::All,
                    &configured,
                    &ItemListFilters::default(),
                    ordering,
                    Pagination::new(PageSize::new(3).unwrap(), PageOffset::new(offset)),
                )
                .unwrap();
            let sql = statements(&connection);
            let end = (offset + 3).min(expected.len());
            assert_eq!(
                page.items,
                expected[offset..end],
                "{ordering:?}, offset={offset}"
            );
            assert_eq!(
                page.next_offset,
                (end < expected.len()).then(|| PageOffset::new(end))
            );
            let primary_count = (expected.len() - offset).min(4);
            ROWS.with(|rows| assert_eq!(*rows.borrow(), (primary_count, end - offset)));
            assert_eq!(
                sql.iter().filter(|sql| sql.starts_with("SELECT")).count(),
                if page.items.is_empty() { 1 } else { 2 },
                "{sql:#?}"
            );
            assert!(
                sql.iter()
                    .any(|sql| sql.contains(&format!("LIMIT 4 OFFSET {offset}"))),
                "{sql:#?}"
            );
            assert!(connection.is_autocommit());
            // Legacy canonical tie sorting may use a temp sort; v2 no-sort
            // assertions intentionally remain confined to the v2 query tests.
        }
    }
}

#[test]
fn legacy_page_preserves_raw_coordinate_assignee_and_text_filters() {
    let connection = storage::open(":memory:").unwrap();
    seed_legacy_coordinates(&connection, &[("ALICE ", "bif ", 1), ("ALICE", "bif", 2)]);
    connection
        .execute_batch(
            "UPDATE items SET title = 'A_100% Ä', description = ' Needle Description ';
             UPDATE item_acceptance_criteria SET criterion = 'Criterion Needle';
             UPDATE items SET assignee = CASE sequence WHEN 1 THEN 'alice ' ELSE 'alice' END;",
        )
        .unwrap();
    let repository = ItemRepository::new(&connection);
    let configured = RequesterId::new("alice").unwrap();
    let cases = [
        (ItemListFilters::default(), vec![1, 2]),
        (
            ItemListFilters {
                requester: Some(configured.clone()),
                ..Default::default()
            },
            vec![2],
        ),
        (
            ItemListFilters {
                project: Some(ProjectId::new("bif").unwrap()),
                ..Default::default()
            },
            vec![2],
        ),
        (
            ItemListFilters {
                assignee: Some(AssigneeId::new("alice").unwrap()),
                ..Default::default()
            },
            vec![2],
        ),
        (
            ItemListFilters {
                unassigned: true,
                ..Default::default()
            },
            vec![],
        ),
        (
            ItemListFilters {
                assignee: Some(AssigneeId::new("alice").unwrap()),
                unassigned: true,
                ..Default::default()
            },
            vec![],
        ),
    ];
    let text_cases = [
        ("a_100%", vec![1, 2]),
        ("Ä", vec![1, 2]),
        ("ä", vec![]),
        (" NEEDLE DESCRIPTION ", vec![1, 2]),
        ("Criterion Needle", vec![1, 2]),
        (" Needle Description  ", vec![]),
    ];
    for (filters, sequences) in cases.into_iter().chain(text_cases.map(|(text, sequences)| {
        (
            ItemListFilters {
                text: Some(ItemTextFilter::new(text).unwrap()),
                ..Default::default()
            },
            sequences,
        )
    })) {
        let mut original = repository
            .select_items(NamedView::All, &configured, &filters)
            .unwrap();
        original.sort_by(|a, b| a.id().cmp(b.id()));
        assert_eq!(
            original
                .iter()
                .map(|item| item.id().sequence())
                .collect::<Vec<_>>(),
            sequences,
            "{filters:?}"
        );
        for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
            let page = repository
                .select_item_page(
                    NamedView::All,
                    &configured,
                    &filters,
                    ordering,
                    Pagination::new(PageSize::new(10).unwrap(), PageOffset::new(0)),
                )
                .unwrap();
            assert_eq!(page.items, original, "{filters:?}, {ordering:?}");
            assert_eq!(page.next_offset, None);
        }
    }
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let mine = repository
            .select_item_page(
                NamedView::Mine,
                &configured,
                &ItemListFilters::default(),
                ordering,
                Pagination::new(PageSize::new(10).unwrap(), PageOffset::new(0)),
            )
            .unwrap();
        // Raw 'alice ' does not equal Mine's configured 'alice', even though
        // both values decode to the same AssigneeId.
        assert_eq!(
            mine.items
                .iter()
                .map(|item| item.id().clone())
                .collect::<Vec<_>>(),
            [id(2)]
        );
    }
}

#[test]
fn legacy_mine_keeps_unknown_status_errors_and_explicit_status_intersections() {
    let connection = storage::open(":memory:").unwrap();
    seed_legacy_coordinates(
        &connection,
        &[
            ("ALICE", "bif", 1),
            ("ALICE", "bif", 2),
            ("ALICE", "bif", 3),
            ("ALICE", "bif", 4),
            ("ALICE", "bif", 5),
            ("ALICE", "bif", 6),
            ("ALICE", "bif", 7),
        ],
    );
    connection
        .execute_batch(
            "UPDATE items SET assignee = 'alice';
             UPDATE items SET status = CASE sequence
                 WHEN 3 THEN 'done' WHEN 4 THEN 'proposed' WHEN 5 THEN 'in_progress'
                 WHEN 6 THEN 'blocked' WHEN 7 THEN 'rejected' ELSE 'ready' END;
             PRAGMA ignore_check_constraints = ON;
             UPDATE items SET status = 'unknown' WHERE sequence = 1;
             PRAGMA ignore_check_constraints = OFF;",
        )
        .unwrap();
    let repository = ItemRepository::new(&connection);
    let configured = RequesterId::new("alice").unwrap();
    for view in [NamedView::Mine, NamedView::All] {
        assert!(matches!(
            repository.select_items(view, &configured, &ItemListFilters::default()),
            Err(ItemStorageError::InvalidPersistedData { item_id, .. }) if item_id == id(1).to_string()
        ));
        for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
            assert!(matches!(
                repository.select_item_page(
                    view,
                    &configured,
                    &ItemListFilters::default(),
                    ordering,
                    Pagination::new(PageSize::new(10).unwrap(), PageOffset::new(0)),
                ),
                Err(ItemStorageError::InvalidPersistedData { item_id, .. }) if item_id == id(1).to_string()
            ));
        }
    }
    for view in [
        NamedView::Proposed,
        NamedView::Ready,
        NamedView::Active,
        NamedView::Blocked,
        NamedView::Done,
        NamedView::Rejected,
        NamedView::Mine,
        NamedView::All,
    ] {
        for status in [
            Status::Proposed,
            Status::Ready,
            Status::InProgress,
            Status::Blocked,
            Status::Done,
            Status::Rejected,
        ] {
            let filters = ItemListFilters {
                status: Some(status),
                ..Default::default()
            };
            let original = repository
                .select_items(view, &configured, &filters)
                .unwrap();
            if view == NamedView::Mine {
                assert_eq!(
                    original.len(),
                    usize::from(!matches!(status, Status::Done | Status::Rejected))
                );
            }
            for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
                let page = repository
                    .select_item_page(
                        view,
                        &configured,
                        &filters,
                        ordering,
                        Pagination::new(PageSize::new(10).unwrap(), PageOffset::new(0)),
                    )
                    .unwrap();
                assert_eq!(page.items, original, "{view:?}, {status:?}, {ordering:?}");
                assert_eq!(page.next_offset, None);
            }
        }
    }
}

#[test]
fn legacy_page_matches_single_item_and_ignores_unselected_corruption() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2, 3]);
    let repository = ItemRepository::new(&connection);
    let full = repository.read_item(&id(3)).unwrap().unwrap();
    // Sequence 3 sorts first, sequence 1 is sentinel, and 2 lies beyond it.
    connection
        .execute(
            "DELETE FROM item_provenance WHERE item_id = ?1",
            [id(1).to_string()],
        )
        .unwrap();
    let page = repository
        .select_item_page(
            NamedView::All,
            &RequesterId::new("alice").unwrap(),
            &ItemListFilters::default(),
            ItemListOrdering::Next,
            Pagination::new(PageSize::new(1).unwrap(), PageOffset::new(0)),
        )
        .unwrap();
    assert_eq!(page.items, [full]);
    assert_eq!(page.next_offset, Some(PageOffset::new(1)));
    assert!(
        repository
            .select_item_page(
                NamedView::All,
                &RequesterId::new("alice").unwrap(),
                &ItemListFilters::default(),
                ItemListOrdering::Next,
                Pagination::new(PageSize::new(1).unwrap(), PageOffset::new(1))
            )
            .is_err()
    );
}

#[test]
fn history_missing_empty_compound_append_and_final_pages() {
    let connection = storage::open(":memory:").unwrap();
    let repository = ItemHistoryRepository::new(&connection);
    assert!(matches!(
        repository.select_history_page(&history_request(None, 2)),
        Err(ItemHistoryStoreError::NotFound)
    ));
    seed(&connection, &[1]);
    assert!(
        repository
            .select_history_page(&history_request(None, 2))
            .unwrap()
            .records
            .is_empty()
    );
    for (revision, index) in [(2, 2), (1, 0), (2, 0), (2, 1)] {
        event(&connection, revision, index, "ready");
    }
    let first = repository
        .select_history_page(&history_request(None, 2))
        .unwrap();
    assert!(first.has_more);
    assert_eq!(
        first
            .records
            .iter()
            .map(|event| (event.item_revision.get(), event.event_index))
            .collect::<Vec<_>>(),
        [(1, 0), (2, 0)]
    );
    event(&connection, 3, 0, "ready");
    let second = repository
        .select_history_page(&history_request(
            Some(HistoryReadKey::from(first.records.last().unwrap())),
            2,
        ))
        .unwrap();
    assert!(second.has_more);
    let final_page = repository
        .select_history_page(&history_request(
            Some(HistoryReadKey::from(second.records.last().unwrap())),
            2,
        ))
        .unwrap();
    assert!(!final_page.has_more);
    assert_eq!(final_page.records.len(), 1);
    let mut combined = first.records;
    combined.extend(second.records);
    combined.extend(final_page.records);
    assert_eq!(combined, repository.item_history(&id(1)).unwrap());
    for after in [
        HistoryReadKey {
            item_revision: Revision::new(u64::MAX).unwrap(),
            event_index: 0,
        },
        HistoryReadKey {
            item_revision: Revision::new(3).unwrap(),
            event_index: u64::MAX,
        },
    ] {
        let page = repository
            .select_history_page(&history_request(Some(after), 2))
            .unwrap();
        assert!(page.records.is_empty());
        assert!(!page.has_more);
    }
    assert!(connection.is_autocommit());
}

#[test]
fn history_sentinel_is_not_decoded_but_returned_malformed_event_is_error() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1]);
    event(&connection, 1, 0, "ready");
    event(&connection, 2, 0, "not-a-status");
    let repository = ItemHistoryRepository::new(&connection);
    start_trace(&connection);
    let first = repository
        .select_history_page(&history_request(None, 1))
        .unwrap();
    let sql = statements(&connection);
    assert!(first.has_more);
    assert_eq!(first.records.len(), 1);
    assert_eq!(
        sql.iter().filter(|sql| sql.starts_with("SELECT")).count(),
        2
    );
    assert!(sql.iter().any(|sql| sql.contains("LIMIT 2")));
    assert!(sql.iter().all(|sql| !sql.contains("OFFSET")));
    assert!(matches!(
        repository.select_history_page(&history_request(
            Some(HistoryReadKey::from(&first.records[0])),
            1
        )),
        Err(ItemHistoryStoreError::InvalidPersistedData(_))
    ));
    assert!(connection.is_autocommit());
}

#[test]
fn continuation_seeks_existing_item_indexes_and_history_unique_key() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1, 2, 3, 4, 5, 6]);
    for (ordering, index) in [
        (ItemListOrdering::NewestFirst, "idx_items_read_list"),
        (ItemListOrdering::Next, "idx_items_read_ready"),
    ] {
        let repository = ProjectionRepository::new(&connection);
        let mut request = request(ItemProjectionKind::Summary, ordering);
        if ordering == ItemListOrdering::Next {
            request.view = NamedView::Ready;
        }
        let first = repository.select_projection_page(&request).unwrap();
        request.page.after = Some(first.records.last().unwrap().key.clone());
        let plans = repository.explain_projection_page(&request).unwrap();
        assert!(
            plans
                .iter()
                .any(|plan| plan.contains("SEARCH") && plan.contains(index)),
            "{plans:?}"
        );
        assert!(
            plans.iter().all(|plan| !plan.contains("TEMP B-TREE")),
            "{plans:?}"
        );
    }
    let plans = ItemHistoryRepository::new(&connection)
        .explain_history_page(&history_request(
            Some(HistoryReadKey {
                item_revision: Revision::new(1).unwrap(),
                event_index: 0,
            }),
            2,
        ))
        .unwrap();
    assert!(
        plans
            .iter()
            .any(|plan| plan.contains("SEARCH") && plan.contains("item_revision")),
        "{plans:?}"
    );
    assert!(
        plans.iter().all(|plan| !plan.contains("TEMP B-TREE")),
        "{plans:?}"
    );
}

#[test]
fn deep_continuation_bounds_materialization_and_records_sql_work() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &(1..=1000).collect::<Vec<_>>());
    // Unique leading times make list's leading range seek depth-independent.
    // Next's leading priority groups can still incur predicate work within a
    // group. Count VM steps explicitly rather than equating emitted rows to work.
    connection
        .execute(
            "UPDATE items SET captured_at = printf('%06d', sequence), priority = 'P2'",
            [],
        )
        .unwrap();
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let mut request = request(ItemProjectionKind::Work, ordering);
        request.view = NamedView::Ready;
        let sequence = if ordering == ItemListOrdering::NewestFirst {
            100
        } else {
            900
        };
        request.page.after = Some(bif::application::ItemReadKey {
            id: id(sequence),
            captured_at: bif::domain::Timestamp::new(format!("{sequence:06}")),
            priority: Some(bif::domain::Priority::P2),
        });
        start_trace(&connection);
        let page = ProjectionRepository::new(&connection)
            .select_projection_page(&request)
            .unwrap();
        let sql = statements(&connection);
        assert_eq!(page.records.len(), 2);
        assert!(page.has_more);
        assert_eq!(
            sql.iter().filter(|sql| sql.starts_with("SELECT")).count(),
            2
        );
        assert!(sql.iter().all(|sql| !sql.contains("OFFSET")));
        ROWS.with(|rows| assert_eq!(*rows.borrow(), (3, 2)));
        WORK.with(|work| {
            let (vm_steps, sorts) = *work.borrow();
            eprintln!("{ordering:?}: emitted_primary=3 hydrated_children=2 vm_steps={vm_steps} sorts={sorts}");
            assert!(vm_steps > 0);
            assert_eq!(sorts, 0);
            if ordering == ItemListOrdering::NewestFirst { assert!(vm_steps < 1000); }
        });
    }
}

#[test]
fn legacy_and_history_assembly_share_short_snapshot_with_concurrent_writer() {
    let directory = support::OwnedTestDirectory::new();
    for history in [false, true] {
        let path = directory.path().join(format!("snapshot-{history}.sqlite"));
        let connection = storage::open(&path).unwrap();
        if history {
            seed(&connection, &[1]);
        } else {
            seed_legacy_coordinates(&connection, &[("ALICE ", "bif ", 1)]);
        }
        event(&connection, 1, 0, "ready");
        let (start, wait) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let connection = storage::open(&path).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            if history {
                event(&connection, 2, 0, "ready");
            } else {
                connection
                    .execute_batch(
                        "BEGIN IMMEDIATE;
                     UPDATE items SET title = 'New', revision = 2;
                     UPDATE item_acceptance_criteria SET criterion = 'New criterion';
                     COMMIT;",
                    )
                    .unwrap();
            }
            done.send(()).unwrap();
        });
        WRITER.with(|writer| *writer.borrow_mut() = Some((start, finished)));
        start_trace(&connection);
        if history {
            let page = ItemHistoryRepository::new(&connection)
                .select_history_page(&history_request(None, 10))
                .unwrap();
            assert_eq!(page.records.len(), 1);
            assert!(!page.has_more);
        } else {
            let page = ItemRepository::new(&connection)
                .select_item_page(
                    NamedView::All,
                    &RequesterId::new("alice").unwrap(),
                    &ItemListFilters::default(),
                    ItemListOrdering::NewestFirst,
                    Pagination::new(PageSize::new(10).unwrap(), PageOffset::new(0)),
                )
                .unwrap();
            assert_eq!(page.items[0].content().title(), "Title");
            assert_eq!(page.items[0].content().acceptance_criteria(), ["criterion"]);
            assert_eq!(page.items[0].revision().get(), 1);
        }
        let sql = statements(&connection);
        if !history {
            assert_eq!(
                sql.iter().filter(|sql| sql.starts_with("SELECT")).count(),
                2,
                "{sql:#?}"
            );
            ROWS.with(|rows| assert_eq!(*rows.borrow(), (1, 1)));
        }
        writer.join().unwrap();
        assert!(connection.is_autocommit());
        if history {
            assert_eq!(
                ItemHistoryRepository::new(&connection)
                    .item_history(&id(1))
                    .unwrap()
                    .len(),
                2
            );
        } else {
            assert_eq!(
                ItemRepository::new(&connection)
                    .read_item(&id(1))
                    .unwrap()
                    .unwrap()
                    .content()
                    .title(),
                "New"
            );
        }
    }
}

#[test]
fn history_keeps_complete_large_events_and_uses_existing_order_index() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection, &[1]);
    let large = "x".repeat(1_048_576);
    connection
        .execute(
            "INSERT INTO operations VALUES ('large-op', ?1, 'triage', NULL, 1, 'same-time')",
            [id(1).to_string()],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO events VALUES ('large-event', 'large-op', ?1, 1, 0, 'note_added', NULL, ?2,
         'human', 'alice', 'cli', 'local', 'direct', NULL, 'cli', 'local', NULL, ?2, 'same-time', 1)",
        params![id(1).to_string(), large]).unwrap();
    let repository = ItemHistoryRepository::new(&connection);
    let page = repository
        .select_history_page(&history_request(None, 1))
        .unwrap();
    assert_eq!(page.records[0].note.as_deref(), Some(large.as_str()));
    let plans: Vec<String> = connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT event_id FROM events WHERE item_id = ?1
         AND (item_revision, event_index) > (?2, ?3) ORDER BY item_revision, event_index LIMIT 2",
        )
        .unwrap()
        .query_map(params![id(1).to_string(), 1, 0], |row| row.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plans
            .iter()
            .any(|plan| plan.contains("INDEX") && plan.contains("item_revision")),
        "{plans:?}"
    );
    assert!(
        plans.iter().all(|plan| !plan.contains("TEMP B-TREE")),
        "{plans:?}"
    );
    // Legacy remains a complete current-state item, with no history fields.
    let audit = ItemAudit::from(
        &ItemRepository::new(&connection)
            .read_item(&id(1))
            .unwrap()
            .unwrap(),
    );
    assert!(
        matches!(ProjectionRepository::new(&connection).read_projection(
        &bif::application::ProjectionGetRequest { item_id: id(1), projection: ItemProjectionKind::Audit }
    ).unwrap(), Some(ItemProjection::Audit(actual)) if actual == audit)
    );
}

#[test]
fn store_identity_requires_a_nonempty_text_singleton() {
    let connection = storage::open(":memory:").unwrap();
    let identity = storage::read_store_identity(&connection).unwrap();
    assert!(!identity.is_empty());
    assert_eq!(storage::read_store_identity(&connection).unwrap(), identity);
    connection
        .execute("DELETE FROM store_metadata", [])
        .unwrap();
    assert!(matches!(
        storage::read_store_identity(&connection),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    for value in [
        rusqlite::types::Value::Text(String::new()),
        rusqlite::types::Value::Blob(vec![1]),
    ] {
        connection
            .execute(
                "INSERT OR REPLACE INTO store_metadata VALUES (1, ?1, 'same-time')",
                [value],
            )
            .unwrap();
        assert!(storage::read_store_identity(&connection).is_err());
    }
    connection
        .execute("DELETE FROM store_metadata", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO store_metadata VALUES (2, 'other-singleton', 'same-time')",
            [],
        )
        .unwrap();
    assert!(matches!(
        storage::read_store_identity(&connection),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
}
