use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Command, Execution, HumanAuthorization,
        InvalidPagination, ItemListFilters, ObservedExecution, PageOffset, PageSize, Pagination,
        list_item_page, next_item_page,
    },
    domain::{Item, NamedView, RequesterId},
    storage::{self, ItemRepository},
};

fn authorization() -> AuthorizationRequest<'static> {
    AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "alice",
            surface: "test",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "test",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: Command::Read,
        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
    }
}

fn fixture(connection: &rusqlite::Connection) {
    connection
        .execute_batch(
            "INSERT INTO projects (project_id, created_at) VALUES ('bif', '2025-01-01Z');
             INSERT INTO items (
                 item_id, requester, project_id, sequence, title, status, priority,
                 revision, captured_at, updated_at
             ) VALUES
                 ('ALICE:bif:001', 'ALICE', 'bif', 1, 'old p1 b', 'ready', 'P1', 1, '2025-01-01Z', '2025-01-01Z'),
                 ('BOB:bif:001', 'BOB', 'bif', 1, 'old p1 a', 'ready', 'P1', 1, '2025-01-01Z', '2025-01-01Z'),
                 ('ALICE:bif:002', 'ALICE', 'bif', 2, 'new p4', 'ready', 'P4', 1, '2025-01-03Z', '2025-01-03Z'),
                 ('ALICE:bif:003', 'ALICE', 'bif', 3, 'new p0', 'ready', 'P0', 1, '2025-01-03Z', '2025-01-03Z'),
                 ('ALICE:bif:004', 'ALICE', 'bif', 4, 'unprioritized', 'ready', NULL, 1, '2025-01-02Z', '2025-01-02Z'),
                 ('ALICE:bif:005', 'ALICE', 'bif', 5, 'not ready', 'done', 'P0', 1, '2025-01-04Z', '2025-01-04Z');
             INSERT INTO item_provenance (item_id) SELECT item_id FROM items;",
        )
        .unwrap();
}

fn ids(items: &[Item]) -> Vec<String> {
    items.iter().map(|item| item.id().to_string()).collect()
}

fn pagination(limit: usize, offset: usize) -> Pagination {
    Pagination::new(PageSize::new(limit).unwrap(), PageOffset::new(offset))
}

#[test]
fn newest_first_pages_use_id_as_a_total_tie_breaker_without_gaps() {
    let connection = storage::open(":memory:").unwrap();
    fixture(&connection);
    let repository = ItemRepository::new(&connection);
    let requester = RequesterId::new("alice").unwrap();
    let mut offset = PageOffset::default();
    let mut collected = Vec::new();

    loop {
        let page = list_item_page(
            &repository,
            &authorization(),
            NamedView::Ready,
            &requester,
            &ItemListFilters::default(),
            Pagination::new(PageSize::new(2).unwrap(), offset),
        )
        .unwrap();
        collected.extend(ids(&page.items));
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }

    assert_eq!(
        collected,
        [
            "ALICE:bif:002",
            "ALICE:bif:003",
            "ALICE:bif:004",
            "ALICE:bif:001",
            "BOB:bif:001",
        ]
    );
}

#[test]
fn next_orders_ready_items_by_priority_then_age_then_id_and_is_read_only() {
    let connection = storage::open(":memory:").unwrap();
    fixture(&connection);
    let before = connection.total_changes();
    let page = next_item_page(
        &ItemRepository::new(&connection),
        &authorization(),
        &RequesterId::new("alice").unwrap(),
        &ItemListFilters::default(),
        pagination(10, 0),
    )
    .unwrap();

    assert_eq!(
        ids(&page.items),
        [
            "ALICE:bif:003",
            "ALICE:bif:001",
            "BOB:bif:001",
            "ALICE:bif:002",
            "ALICE:bif:004",
        ]
    );
    assert_eq!(page.next_offset, None);
    assert_eq!(connection.total_changes(), before);
}

#[test]
fn exact_and_past_end_boundaries_return_no_spurious_cursor() {
    let connection = storage::open(":memory:").unwrap();
    fixture(&connection);
    let repository = ItemRepository::new(&connection);
    let requester = RequesterId::new("alice").unwrap();

    let exact = list_item_page(
        &repository,
        &authorization(),
        NamedView::Ready,
        &requester,
        &ItemListFilters::default(),
        pagination(5, 0),
    )
    .unwrap();
    assert_eq!(exact.items.len(), 5);
    assert_eq!(exact.next_offset, None);

    let empty = list_item_page(
        &repository,
        &authorization(),
        NamedView::Ready,
        &requester,
        &ItemListFilters::default(),
        pagination(2, 5),
    )
    .unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.next_offset, None);
}

#[test]
fn invalid_page_sizes_have_stable_validation() {
    for value in [0, 101] {
        let error = PageSize::new(value).unwrap_err();
        assert_eq!(error, InvalidPagination);
        assert_eq!(error.code(), "invalid_pagination");
        assert_eq!(error.to_string(), "page size must be between 1 and 100");
    }
}
