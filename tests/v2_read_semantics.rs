use std::cmp::Ordering;

use bif::{
    application::{
        ItemListFilters, ItemListOrdering, ItemProjectionKind, ItemProjectionPageRequest,
        ItemReadKey, ItemTextFilter, NamedViewStore, PageOffset, PageSize, Pagination,
        ReadPageRequest,
        read_semantics::{
            AssigneeFilter, EffectiveItemFilters, ItemQueryFingerprintInput, ItemReadOperation,
            ItemSortField, SortDirection, compare_captured_at, compare_identity, priority_rank,
        },
    },
    domain::{
        AssigneeId, Item, ItemId, NamedView, Priority, ProjectId, RequesterId, Status, Timestamp,
    },
    storage::{self, ItemRepository},
};
use rusqlite::{Connection, params, types::Value};

const VIEWS: [NamedView; 8] = [
    NamedView::Proposed,
    NamedView::Ready,
    NamedView::Active,
    NamedView::Blocked,
    NamedView::Done,
    NamedView::Rejected,
    NamedView::Mine,
    NamedView::All,
];
const STATUSES: [Status; 6] = [
    Status::Proposed,
    Status::Ready,
    Status::InProgress,
    Status::Blocked,
    Status::Done,
    Status::Rejected,
];
const PRIORITIES: [Option<Priority>; 6] = [
    Some(Priority::P0),
    Some(Priority::P1),
    Some(Priority::P2),
    Some(Priority::P3),
    Some(Priority::P4),
    None,
];

fn requester() -> RequesterId {
    RequesterId::new("Alice").unwrap()
}

fn fixture() -> Connection {
    let connection = storage::open(":memory:").unwrap();
    connection.execute_batch(
        "INSERT INTO projects (project_id, created_at) VALUES ('bif', 'time'), ('delta', 'time');",
    ).unwrap();
    for (status_index, status) in STATUSES.iter().enumerate() {
        for (priority_index, priority) in PRIORITIES.iter().enumerate() {
            for requester in ["ALICE", "BOB"] {
                let sequence = 1 + (status_index * 6 + priority_index) as u64;
                let project = if priority_index % 2 == 0 {
                    "bif"
                } else {
                    "delta"
                };
                let assignee = [Some("alice"), Some("bob"), None][priority_index % 3];
                insert(
                    &connection,
                    requester,
                    project,
                    sequence,
                    *status,
                    *priority,
                    assignee,
                );
            }
        }
    }
    insert(
        &connection,
        "ALICE-SEARS",
        "bif",
        1,
        Status::Ready,
        Some(Priority::P1),
        Some("alice-sears"),
    );
    for sequence in [
        999,
        1000,
        (1_u64 << 53) + 1,
        (1_u64 << 53) + 2,
        i64::MAX as u64,
    ] {
        insert(
            &connection,
            "ALICE",
            "bif",
            sequence,
            Status::Ready,
            None,
            Some("alice"),
        );
    }
    // This row must not match "%" or "_": those are not LIKE wildcards.
    // Also exercise nullable descriptions and fields/criteria that cannot join.
    connection.execute_batch(
        "UPDATE items SET title = 'plain un', description = NULL WHERE item_id = 'BOB:delta:006';
         UPDATE item_acceptance_criteria SET criterion = 'joined'
             WHERE item_id = 'BOB:delta:006';",
    ).unwrap();
    connection
}

fn insert(
    connection: &Connection,
    requester: &str,
    project: &str,
    sequence: u64,
    status: Status,
    priority: Option<Priority>,
    assignee: Option<&str>,
) {
    let id = ItemId::new(
        RequesterId::new(requester).unwrap(),
        ProjectId::new(project).unwrap(),
        sequence,
    )
    .unwrap()
    .to_string();
    let status = match status {
        Status::Proposed => "proposed",
        Status::Ready => "ready",
        Status::InProgress => "in_progress",
        Status::Blocked => "blocked",
        Status::Done => "done",
        Status::Rejected => "rejected",
    };
    let priority = priority.map(|p| format!("{p:?}"));
    // Opaque timestamps intentionally include non-dates and equivalent instants.
    // Large sequences must tie on all earlier coordinates to catch lexical/REAL order.
    let captured = if sequence >= 999 {
        "opaque"
    } else {
        [
            "opaque",
            "2025-01-01T01:00:00+01:00",
            "2025-01-01T00:00:00Z",
        ][(sequence % 3) as usize]
    };
    connection
        .execute(
            "INSERT INTO items (item_id, requester, project_id, sequence, title, description,
             status, priority, assignee, revision, captured_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?10)",
            params![
                id,
                requester,
                project,
                i64::try_from(sequence).unwrap(),
                format!("Title {status} 100%_literal Äpfel"),
                " Rust café and STRASSE ",
                status,
                priority,
                assignee,
                captured
            ],
        )
        .unwrap();
    connection
        .execute("INSERT INTO item_provenance (item_id) VALUES (?1)", [&id])
        .unwrap();
    connection
        .execute(
            "INSERT INTO item_acceptance_criteria (item_id, criterion_index, criterion)
         VALUES (?1, 0, 'Criteria Ωmega with [brackets]')",
            [&id],
        )
        .unwrap();
}

fn ids(items: &[Item]) -> Vec<String> {
    items.iter().map(|item| item.id().to_string()).collect()
}

fn filter_cases() -> Vec<ItemListFilters> {
    let mut cases = vec![ItemListFilters::default()];
    cases.extend(STATUSES.map(|status| ItemListFilters {
        status: Some(status),
        ..Default::default()
    }));
    cases.extend(
        PRIORITIES
            .into_iter()
            .flatten()
            .map(|priority| ItemListFilters {
                priority: Some(priority),
                ..Default::default()
            }),
    );
    for project in ["BIF", "delta", "absent"] {
        cases.push(ItemListFilters {
            project: Some(ProjectId::new(project).unwrap()),
            ..Default::default()
        });
    }
    for name in ["alice", "BOB", "absent"] {
        cases.push(ItemListFilters {
            requester: Some(RequesterId::new(name).unwrap()),
            ..Default::default()
        });
        cases.push(ItemListFilters {
            assignee: Some(AssigneeId::new(name).unwrap()),
            ..Default::default()
        });
    }
    cases.push(ItemListFilters {
        unassigned: true,
        ..Default::default()
    });
    for text in [
        "TITLE",
        "rUsT",
        "Criteria",
        "Äpfel",
        "äpfel",
        "café",
        "CAFÉ",
        "Ωmega",
        "ωmega",
        "straße",
        "%_",
        "_",
        "[brackets]",
        " Rust ",
        " Rust  ",
        "unjoined",
        "absent",
    ] {
        cases.push(ItemListFilters {
            text: Some(ItemTextFilter::new(text).unwrap()),
            ..Default::default()
        });
    }
    cases.push(ItemListFilters {
        project: Some(ProjectId::new("bif").unwrap()),
        requester: Some(RequesterId::new("alice").unwrap()),
        assignee: Some(AssigneeId::new("alice").unwrap()),
        status: Some(Status::Ready),
        priority: Some(Priority::P0),
        text: Some(ItemTextFilter::new("RUST").unwrap()),
        unassigned: false,
    });
    cases.push(ItemListFilters {
        status: Some(Status::Blocked),
        unassigned: true,
        text: Some(ItemTextFilter::new("%").unwrap()),
        ..Default::default()
    });
    cases
}

#[test]
fn normalized_membership_and_order_match_v1_for_every_view_and_filter() {
    let connection = fixture();
    let repository = ItemRepository::new(&connection);
    let all = repository
        .select_items(NamedView::All, &requester(), &Default::default())
        .unwrap();
    for name in ["Alice", "BOB", "Alice Sears"] {
        let configured_requester = RequesterId::new(name).unwrap();
        for view in VIEWS {
            for filters in filter_cases() {
                let effective =
                    EffectiveItemFilters::new(view, &configured_requester, &filters).unwrap();
                let selected: Vec<_> = all
                    .iter()
                    .filter(|item| effective.matches(item))
                    .cloned()
                    .collect();
                let mut expected = ids(&repository
                    .select_items(view, &configured_requester, &filters)
                    .unwrap());
                expected.sort();
                let mut actual = ids(&selected);
                actual.sort();
                assert_eq!(actual, expected, "{name} {view:?} {filters:?}");
                for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
                    let mut ordered = selected.clone();
                    ordered.sort_by(|a, b| {
                        ordering.compare_keys(&ItemReadKey::from(a), &ItemReadKey::from(b))
                    });
                    let v1 = repository
                        .select_item_page(
                            view,
                            &configured_requester,
                            &filters,
                            ordering,
                            Pagination::new(PageSize::new(100).unwrap(), PageOffset::default()),
                        )
                        .unwrap();
                    assert_eq!(
                        ids(&ordered),
                        ids(&v1.items),
                        "{name} {view:?} {filters:?} {ordering:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn substring_search_never_joins_adjacent_acceptance_criteria() {
    let connection = fixture();
    connection
        .execute_batch(
            "UPDATE item_acceptance_criteria SET criterion = 'criterion-left'
                WHERE item_id = 'BOB:delta:006';
             INSERT INTO item_acceptance_criteria (item_id, criterion_index, criterion)
                VALUES ('BOB:delta:006', 1, 'criterion-right');",
        )
        .unwrap();
    let repository = ItemRepository::new(&connection);
    let filters = ItemListFilters {
        text: Some(ItemTextFilter::new("criterion-leftcriterion-right").unwrap()),
        ..Default::default()
    };
    let effective = EffectiveItemFilters::new(NamedView::All, &requester(), &filters).unwrap();
    let all = repository
        .select_items(NamedView::All, &requester(), &Default::default())
        .unwrap();
    assert!(all.iter().all(|item| !effective.matches(item)));
    assert!(
        repository
            .select_items(NamedView::All, &requester(), &filters)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mine_intersects_explicit_ownership_and_status_instead_of_overriding_them() {
    let mine =
        EffectiveItemFilters::new(NamedView::Mine, &requester(), &Default::default()).unwrap();
    assert_eq!(mine.statuses(), &STATUSES[..4]);
    assert_eq!(
        mine.assignee(),
        &AssigneeFilter::Assigned(AssigneeId::new("alice").unwrap())
    );
    for filters in [
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
            status: Some(Status::Rejected),
            ..Default::default()
        },
    ] {
        assert!(
            EffectiveItemFilters::new(NamedView::Mine, &requester(), &filters)
                .unwrap()
                .is_empty()
        );
    }
    let invalid = ItemListFilters {
        assignee: Some(AssigneeId::new("bob").unwrap()),
        unassigned: true,
        ..Default::default()
    };
    assert!(EffectiveItemFilters::new(NamedView::Mine, &requester(), &invalid).is_err());
}

fn key(
    requester: &str,
    project: &str,
    sequence: u64,
    time: &str,
    priority: Option<Priority>,
) -> ItemReadKey {
    ItemReadKey {
        id: ItemId::new(
            RequesterId::new(requester).unwrap(),
            ProjectId::new(project).unwrap(),
            sequence,
        )
        .unwrap(),
        captured_at: Timestamp::new(time),
        priority,
    }
}

#[test]
fn comparisons_keep_full_u64_numeric_identity_and_opaque_timestamps() {
    assert_eq!(PRIORITIES.map(priority_rank), [0, 1, 2, 3, 4, 5]);
    let keys = [
        key("alice", "bif", 999, "time", None),
        key("alice", "bif", 1000, "time", None),
        key("alice", "bif", (1 << 53) + 1, "time", None),
        key("alice", "bif", (1 << 53) + 2, "time", None),
        key("alice", "bif", u64::MAX, "time", None),
        key("alice", "delta", 1, "time", None),
        key("bob", "bif", 1, "time", None),
    ];
    for pair in keys.windows(2) {
        assert_eq!(compare_identity(&pair[0].id, &pair[1].id), Ordering::Less);
        for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
            assert_eq!(ordering.compare_keys(&pair[0], &pair[1]), Ordering::Less);
            assert!(ordering.is_after(&pair[1], &pair[0]));
            assert!(!ordering.is_after(&pair[0], &pair[1]));
            assert!(!ordering.is_after(&pair[0], &pair[0]));
        }
    }
    let left = key("alice", "bif", 1, "2025-01-01T00:00:00Z", None);
    let right = key(
        "alice",
        "bif",
        1,
        "2025-01-01T01:00:00+01:00",
        Some(Priority::P4),
    );
    assert_eq!(
        compare_captured_at(&left.captured_at, &right.captured_at),
        Ordering::Less
    );
    assert!(ItemListOrdering::NewestFirst.is_after(&left, &right));
    assert!(ItemListOrdering::Next.is_after(&left, &right)); // Null priority is last.
}

fn sql_expression(field: ItemSortField) -> &'static str {
    match field {
        ItemSortField::PriorityRank => {
            "CASE i.priority WHEN 'P0' THEN 0 WHEN 'P1' THEN 1 WHEN 'P2' THEN 2 WHEN 'P3' THEN 3 WHEN 'P4' THEN 4 ELSE 5 END"
        }
        ItemSortField::CapturedAt => "i.captured_at COLLATE BINARY",
        ItemSortField::Requester => "i.requester COLLATE BINARY",
        ItemSortField::Project => "i.project_id COLLATE BINARY",
        ItemSortField::Sequence => "i.sequence",
    }
}

fn sql_value(key: &ItemReadKey, field: ItemSortField) -> Value {
    match field {
        ItemSortField::PriorityRank => Value::Integer(i64::from(priority_rank(key.priority))),
        ItemSortField::CapturedAt => Value::Text(key.captured_at.as_str().to_owned()),
        ItemSortField::Requester => Value::Text(key.id.requester().to_string()),
        ItemSortField::Project => Value::Text(key.id.project().to_string()),
        ItemSortField::Sequence => Value::Integer(i64::try_from(key.id.sequence()).unwrap()),
    }
}

/// Candidate adapter predicate, including an overflow-safe unsigned boundary.
fn sql_boundary(ordering: ItemListOrdering, boundary: &ItemReadKey) -> (String, Vec<Value>) {
    let spec = ordering.sort_spec();
    assert_eq!(spec.last().unwrap().field, ItemSortField::Sequence);
    let overflowing_sequence = boundary.id.sequence() > i64::MAX as u64;
    let terms: Vec<_> = spec
        .iter()
        .enumerate()
        .map(|(index, term)| {
            let mut equal: Vec<_> = spec[..index]
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{} = ?{}", sql_expression(t.field), i + 1))
                .collect();
            if term.field == ItemSortField::Sequence && overflowing_sequence {
                equal.push(match term.direction {
                    SortDirection::Ascending => "0".to_owned(),
                    SortDirection::Descending => "1".to_owned(),
                });
            } else {
                equal.push(format!(
                    "{} {} ?{}",
                    sql_expression(term.field),
                    match term.direction {
                        SortDirection::Ascending => ">",
                        SortDirection::Descending => "<",
                    },
                    index + 1
                ));
            }
            format!("({})", equal.join(" AND "))
        })
        .collect();
    let values = spec
        .iter()
        .filter(|term| term.field != ItemSortField::Sequence || !overflowing_sequence)
        .map(|term| sql_value(boundary, term.field))
        .collect();
    (terms.join(" OR "), values)
}

#[test]
fn candidate_sql_order_and_exclusive_boundaries_match_v1_rust() {
    let connection = fixture();
    let repository = ItemRepository::new(&connection);
    for ordering in [ItemListOrdering::NewestFirst, ItemListOrdering::Next] {
        let v1 = repository
            .select_item_page(
                NamedView::All,
                &requester(),
                &Default::default(),
                ordering,
                Pagination::new(PageSize::new(100).unwrap(), PageOffset::default()),
            )
            .unwrap();
        let expected = ids(&v1.items);
        let spec = ordering.sort_spec();
        let order_by = spec
            .iter()
            .map(|term| {
                format!(
                    "{} {}",
                    sql_expression(term.field),
                    match term.direction {
                        SortDirection::Ascending => "ASC",
                        SortDirection::Descending => "DESC",
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let query = |predicate: &str, values: Vec<Value>| {
            connection
                .prepare(&format!(
                    "SELECT i.item_id FROM items i WHERE {predicate} ORDER BY {order_by}"
                ))
                .unwrap()
                .query_map(rusqlite::params_from_iter(values), |row| {
                    row.get::<_, String>(0)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(query("1", vec![]), expected);
        for (index, item) in v1.items.iter().enumerate() {
            let boundary = ItemReadKey::from(item);
            let (predicate, values) = sql_boundary(ordering, &boundary);
            assert_eq!(query(&predicate, values), expected[index + 1..]);
            for (candidate_index, candidate) in v1.items.iter().enumerate() {
                assert_eq!(
                    ordering.is_after(&ItemReadKey::from(candidate), &boundary),
                    candidate_index > index
                );
            }
        }
        for sequence in [i64::MAX as u64 + 1, u64::MAX] {
            let boundary = key("alice", "bif", sequence, "opaque", None);
            let expected_after: Vec<_> = v1
                .items
                .iter()
                .filter(|item| ordering.is_after(&ItemReadKey::from(*item), &boundary))
                .map(|item| item.id().to_string())
                .collect();
            let (predicate, values) = sql_boundary(ordering, &boundary);
            assert_eq!(query(&predicate, values), expected_after);
        }
    }
}

fn request(view: NamedView, filters: ItemListFilters) -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view,
        configured_requester: requester(),
        filters,
        projection: ItemProjectionKind::Summary,
        ordering: ItemListOrdering::NewestFirst,
        page: ReadPageRequest::new(20, None).unwrap(),
    }
}

fn fingerprint(request: &ItemProjectionPageRequest) -> ItemQueryFingerprintInput {
    ItemQueryFingerprintInput::new(ItemReadOperation::List, "store-A", 1, request).unwrap()
}

#[test]
fn fingerprints_are_deterministic_effective_query_input_not_cursor_encoding() {
    let ready = request(
        NamedView::Ready,
        ItemListFilters {
            text: Some(ItemTextFilter::new("RuST Ä").unwrap()),
            ..Default::default()
        },
    );
    let mut equivalent = request(
        NamedView::All,
        ItemListFilters {
            status: Some(Status::Ready),
            text: Some(ItemTextFilter::new("rust Ä").unwrap()),
            ..Default::default()
        },
    );
    equivalent.configured_requester = RequesterId::new("bob").unwrap();
    equivalent.page =
        ReadPageRequest::new(1, Some(key("bob", "delta", u64::MAX, "opaque", None))).unwrap();
    assert_eq!(fingerprint(&ready), fingerprint(&equivalent));
    let bytes = fingerprint(&ready).canonical_bytes();
    assert_eq!(bytes, fingerprint(&equivalent).canonical_bytes());
    assert_eq!(bytes, fingerprint(&ready).canonical_bytes());
    for mut different in [
        request(NamedView::Blocked, ready.filters.clone()),
        request(
            NamedView::Ready,
            ItemListFilters {
                text: Some(ItemTextFilter::new("rust ä").unwrap()),
                ..Default::default()
            },
        ),
        request(
            NamedView::Ready,
            ItemListFilters {
                text: Some(ItemTextFilter::new(" rust Ä").unwrap()),
                ..Default::default()
            },
        ),
    ] {
        assert_ne!(bytes, fingerprint(&different).canonical_bytes());
        different.filters = ready.filters.clone();
        different.ordering = ItemListOrdering::Next;
        assert_ne!(bytes, fingerprint(&different).canonical_bytes());
    }
    for projection in [ItemProjectionKind::Work, ItemProjectionKind::Audit] {
        let mut different = ready.clone();
        different.projection = projection;
        assert_ne!(bytes, fingerprint(&different).canonical_bytes());
    }
    for input in [
        ItemQueryFingerprintInput::new(ItemReadOperation::Next, "store-A", 1, &ready).unwrap(),
        ItemQueryFingerprintInput::new(ItemReadOperation::List, "store-B", 1, &ready).unwrap(),
        ItemQueryFingerprintInput::new(ItemReadOperation::List, "store-A", 2, &ready).unwrap(),
    ] {
        assert_ne!(bytes, input.canonical_bytes());
    }
    // Contradictions have one canonical empty predicate, not competing encodings.
    assert_eq!(
        fingerprint(&request(
            NamedView::Ready,
            ItemListFilters {
                status: Some(Status::Done),
                ..Default::default()
            }
        )),
        fingerprint(&request(
            NamedView::Mine,
            ItemListFilters {
                unassigned: true,
                ..Default::default()
            }
        )),
    );
}

#[test]
fn fingerprint_binds_each_effective_filter_and_resolved_mine_requester() {
    let baseline = fingerprint(&request(NamedView::All, Default::default())).canonical_bytes();
    for filters in [
        ItemListFilters {
            project: Some(ProjectId::new("bif").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            requester: Some(requester()),
            ..Default::default()
        },
        ItemListFilters {
            assignee: Some(AssigneeId::new("alice").unwrap()),
            ..Default::default()
        },
        ItemListFilters {
            unassigned: true,
            ..Default::default()
        },
        ItemListFilters {
            priority: Some(Priority::P0),
            ..Default::default()
        },
        ItemListFilters {
            status: Some(Status::Ready),
            ..Default::default()
        },
        ItemListFilters {
            text: Some(ItemTextFilter::new("text").unwrap()),
            ..Default::default()
        },
    ] {
        assert_ne!(
            baseline,
            fingerprint(&request(NamedView::All, filters)).canonical_bytes()
        );
    }
    let mine = request(
        NamedView::Mine,
        ItemListFilters {
            status: Some(Status::Ready),
            ..Default::default()
        },
    );
    let ready = request(
        NamedView::Ready,
        ItemListFilters {
            assignee: Some(AssigneeId::new("ALICE").unwrap()),
            ..Default::default()
        },
    );
    assert_eq!(fingerprint(&mine), fingerprint(&ready));
    let mut other_mine = mine.clone();
    other_mine.configured_requester = RequesterId::new("bob").unwrap();
    assert_ne!(
        fingerprint(&mine).canonical_bytes(),
        fingerprint(&other_mine).canonical_bytes()
    );
}

#[test]
fn fingerprint_format_has_a_stable_versioned_golden() {
    let bytes = fingerprint(&request(NamedView::Ready, Default::default())).canonical_bytes();
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        hex,
        concat!(
            "4249463a6974656d2d71756572793a3100", // BIF:item-query:1\0
            "00",                                 // list
            "0000000000000007",
            "73746f72652d41", // store-A
            "00000001",       // schema 1
            "00",
            "00", // summary, newest-first
            "01",
            "01", // one status: ready
            "00",
            "00",
            "00",
            "00",
            "00", // no project/requester/ownership/priority/text
        )
    );
}
