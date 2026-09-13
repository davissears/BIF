use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Command, Execution, HumanAuthorization,
        ItemListFilters, ItemTextFilter, ObservedExecution, list_items,
    },
    domain::{AssigneeId, Item, NamedView, Priority, ProjectId, RequesterId, Status},
    storage::{self, ItemRepository},
};

fn authorization() -> AuthorizationRequest<'static> {
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
        command: Command::Read,
        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
    }
}

fn ids(items: &[Item]) -> Vec<String> {
    let mut ids: Vec<_> = items.iter().map(|item| item.id().to_string()).collect();
    ids.sort();
    ids
}

fn fixture(connection: &rusqlite::Connection) {
    connection
        .execute_batch(
            "INSERT INTO projects (project_id, created_at) VALUES
                 ('bif', '2025-01-01Z'), ('delta', '2025-01-01Z');
             INSERT INTO items (
                 item_id, requester, project_id, sequence, title, description,
                 status, priority, assignee, revision, captured_at, updated_at
             ) VALUES
                 ('ALICE:bif:001', 'ALICE', 'bif', 1, 'Parser cleanup', 'Rust parser work',
                  'ready', 'P1', 'alice', 1, '2025-01-01Z', '2025-01-01Z'),
                 ('BOB:bif:001', 'BOB', 'bif', 1, 'Parser docs', NULL,
                  'ready', 'P1', NULL, 1, '2025-01-02Z', '2025-01-02Z'),
                 ('ALICE:delta:001', 'ALICE', 'delta', 1, 'Parser cleanup', NULL,
                  'ready', 'P1', 'alice', 1, '2025-01-03Z', '2025-01-03Z'),
                 ('ALICE:bif:002', 'ALICE', 'bif', 2, 'Parser cleanup', NULL,
                  'done', 'P1', 'alice', 1, '2025-01-04Z', '2025-01-04Z'),
                 ('ALICE:bif:003', 'ALICE', 'bif', 3, 'Other work', NULL,
                  'ready', 'P2', 'bob', 1, '2025-01-05Z', '2025-01-05Z');
             INSERT INTO item_acceptance_criteria (item_id, criterion_index, criterion)
             VALUES ('BOB:bif:001', 0, 'Covers RuSt examples');
             INSERT INTO item_provenance (item_id) SELECT item_id FROM items;",
        )
        .unwrap();
}

#[test]
fn filters_intersect_each_other_and_named_view_membership() {
    let connection = storage::open(":memory:").unwrap();
    fixture(&connection);
    let repository = ItemRepository::new(&connection);
    let filters = ItemListFilters {
        project: Some(ProjectId::new("bif").unwrap()),
        requester: Some(RequesterId::new("alice").unwrap()),
        assignee: Some(AssigneeId::new("alice").unwrap()),
        status: Some(Status::Ready),
        priority: Some(Priority::P1),
        text: Some(ItemTextFilter::new("RUST").unwrap()),
        ..ItemListFilters::default()
    };

    let selected = list_items(
        &repository,
        &authorization(),
        NamedView::Ready,
        &RequesterId::new("alice").unwrap(),
        &filters,
    )
    .unwrap();
    assert_eq!(ids(&selected), ["ALICE:bif:001"]);

    // The otherwise matching done item cannot escape the selected ready view.
    assert!(!ids(&selected).contains(&"ALICE:bif:002".to_owned()));
}

#[test]
fn unassigned_and_text_filters_compose_and_text_searches_criteria() {
    let connection = storage::open(":memory:").unwrap();
    fixture(&connection);
    let filters = ItemListFilters {
        unassigned: true,
        text: Some(ItemTextFilter::new("rust").unwrap()),
        ..ItemListFilters::default()
    };

    let selected = list_items(
        &ItemRepository::new(&connection),
        &authorization(),
        NamedView::Ready,
        &RequesterId::new("alice").unwrap(),
        &filters,
    )
    .unwrap();
    assert_eq!(ids(&selected), ["BOB:bif:001"]);
}

#[test]
fn empty_text_filter_has_a_stable_validation_error() {
    let error = ItemTextFilter::new(" \t").unwrap_err();
    assert_eq!(error.code(), "invalid_filter");
    assert_eq!(error.to_string(), "text filter must not be empty");
}
