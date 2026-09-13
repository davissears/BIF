use std::str::FromStr;

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Command, Execution, HumanAuthorization,
        ObservedExecution, SelectNamedViewError, select_named_view,
    },
    domain::{Item, NamedView, RequesterId},
    storage::{self, ItemRepository},
};

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

fn ids(items: &[Item]) -> Vec<String> {
    let mut ids: Vec<_> = items.iter().map(|item| item.id().to_string()).collect();
    ids.sort();
    ids
}

fn insert_fixture(connection: &rusqlite::Connection) {
    connection
        .execute_batch(
            "INSERT INTO projects (project_id, created_at) VALUES ('bif', '2025-01-01Z');
             INSERT INTO items (
                 item_id, requester, project_id, sequence, title, status, assignee,
                 revision, captured_at, updated_at
             ) VALUES
                 ('ALICE:bif:001', 'ALICE', 'bif', 1, 'proposed mine', 'proposed', 'alice', 1, '2025-01-01Z', '2025-01-01Z'),
                 ('ALICE:bif:002', 'ALICE', 'bif', 2, 'ready mine', 'ready', 'alice', 1, '2025-01-02Z', '2025-01-02Z'),
                 ('ALICE:bif:003', 'ALICE', 'bif', 3, 'active other', 'in_progress', 'bob', 1, '2025-01-03Z', '2025-01-03Z'),
                 ('ALICE:bif:004', 'ALICE', 'bif', 4, 'blocked mine', 'blocked', 'alice', 1, '2025-01-04Z', '2025-01-04Z'),
                 ('ALICE:bif:005', 'ALICE', 'bif', 5, 'done mine', 'done', 'alice', 1, '2025-01-05Z', '2025-01-05Z'),
                 ('ALICE:bif:006', 'ALICE', 'bif', 6, 'rejected mine', 'rejected', 'alice', 1, '2025-01-06Z', '2025-01-06Z'),
                 ('BOB:bif:001', 'BOB', 'bif', 1, 'unassigned ready', 'ready', NULL, 1, '2025-01-07Z', '2025-01-07Z');
             INSERT INTO item_provenance (item_id)
             SELECT item_id FROM items;",
        )
        .unwrap();
}

#[test]
fn mixed_status_fixture_selects_exact_ids_for_every_named_view() {
    let connection = storage::open(":memory:").unwrap();
    insert_fixture(&connection);
    let repository = ItemRepository::new(&connection);
    let requester = RequesterId::new("alice").unwrap();
    let cases = [
        (NamedView::Proposed, vec!["ALICE:bif:001"]),
        (NamedView::Ready, vec!["ALICE:bif:002", "BOB:bif:001"]),
        (NamedView::Active, vec!["ALICE:bif:003", "ALICE:bif:004"]),
        (NamedView::Blocked, vec!["ALICE:bif:004"]),
        (NamedView::Done, vec!["ALICE:bif:005"]),
        (NamedView::Rejected, vec!["ALICE:bif:006"]),
        (
            NamedView::Mine,
            vec!["ALICE:bif:001", "ALICE:bif:002", "ALICE:bif:004"],
        ),
        (
            NamedView::All,
            vec![
                "ALICE:bif:001",
                "ALICE:bif:002",
                "ALICE:bif:003",
                "ALICE:bif:004",
                "ALICE:bif:005",
                "ALICE:bif:006",
                "BOB:bif:001",
            ],
        ),
    ];

    for (view, expected) in cases {
        let selected =
            select_named_view(&repository, &authorization(Command::Read), view, &requester)
                .unwrap();
        assert_eq!(ids(&selected), expected, "view {view}");
    }
}

#[test]
fn view_names_are_exact_and_invalid_input_is_typed() {
    for name in [
        "proposed", "ready", "active", "blocked", "done", "rejected", "mine", "all",
    ] {
        assert_eq!(NamedView::from_str(name).unwrap().to_string(), name);
    }

    let error = NamedView::from_str("Ready").unwrap_err();
    assert_eq!(error.code(), "invalid_view");
    assert_eq!(error.input(), "Ready");
}

#[test]
fn named_view_selection_requires_a_read_command() {
    let connection = storage::open(":memory:").unwrap();
    let error = select_named_view(
        &ItemRepository::new(&connection),
        &authorization(Command::Capture),
        NamedView::All,
        &RequesterId::new("alice").unwrap(),
    )
    .unwrap_err();

    assert!(matches!(error, SelectNamedViewError::Unauthorized(_)));
    assert_eq!(error.code(), "unauthorized");
}
