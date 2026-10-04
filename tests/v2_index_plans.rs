//! Structural plan guarantees for the measured common paths, not scan-work bounds.
use bif::{
    application::{
        ItemListFilters, ItemListOrdering, ItemProjectionKind, ItemProjectionPageRequest,
        ItemProjectionStore, ReadPageRequest,
    },
    domain::{NamedView, RequesterId},
    storage::{self, ProjectionRepository},
};
use rusqlite::Connection;

const INDEXES: [(&str, &str); 4] = [
    (
        "idx_items_read_list",
        "CREATE INDEX idx_items_read_list ON items
         (captured_at DESC, requester, project_id, sequence)",
    ),
    (
        "idx_items_read_ready",
        "CREATE INDEX idx_items_read_ready ON items
         (CASE priority WHEN 'P0' THEN 0 WHEN 'P1' THEN 1 WHEN 'P2' THEN 2
          WHEN 'P3' THEN 3 WHEN 'P4' THEN 4 ELSE 5 END,
          captured_at, requester, project_id, sequence) WHERE status = 'ready'",
    ),
    (
        "idx_items_read_active",
        "CREATE INDEX idx_items_read_active ON items
         (captured_at DESC, requester, project_id, sequence)
         WHERE status IN ('in_progress', 'blocked')",
    ),
    (
        "idx_items_read_mine",
        "CREATE INDEX idx_items_read_mine ON items
         (assignee, captured_at DESC, requester, project_id, sequence)
         WHERE status IN ('proposed', 'ready', 'in_progress', 'blocked')",
    ),
];

fn normalized(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn fresh_schema_has_exactly_the_four_measured_indexes_with_binary_collation() {
    let connection = storage::open(":memory:").unwrap();
    let mut actual: Vec<(String, String)> = connection
        .prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'index' AND sql IS NOT NULL")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    actual.sort();
    let mut expected: Vec<_> = INDEXES
        .iter()
        .map(|(name, sql)| (name.to_string(), normalized(sql)))
        .collect();
    expected.sort();
    assert_eq!(
        actual
            .iter()
            .map(|(name, sql)| (name.clone(), normalized(sql)))
            .collect::<Vec<_>>(),
        expected
    );
    for (name, _) in INDEXES {
        let keys: Vec<(i64, String, bool)> = connection
            .prepare(&format!("PRAGMA index_xinfo({name})"))
            .unwrap()
            .query_map([], |row| Ok((row.get(1)?, row.get(4)?, row.get(5)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            keys.iter()
                .filter(|(_, _, key)| *key)
                .all(|(_, collation, _)| collation == "BINARY")
        );
        if name == "idx_items_read_ready" {
            assert_eq!(keys[0].0, -2, "priority rank must remain an expression");
        }
    }
}

/// Small deterministic data exercises all statuses, rank tiers, ties, and children.
fn seed(connection: &Connection) {
    connection
        .execute_batch(
            "INSERT INTO projects VALUES ('bif', 'opaque');
         WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 360)
         INSERT INTO items
         (item_id, requester, project_id, sequence, title, description, status,
          priority, assignee, revision, captured_at, updated_at)
         SELECT printf('ALICE:bif:%03d', x), 'ALICE', 'bif', x, 'Title', 'Description',
          CASE x % 6 WHEN 0 THEN 'proposed' WHEN 1 THEN 'ready'
           WHEN 2 THEN 'in_progress' WHEN 3 THEN 'blocked' WHEN 4 THEN 'done' ELSE 'rejected' END,
          CASE (x / 6) % 6 WHEN 0 THEN 'P0' WHEN 1 THEN 'P1' WHEN 2 THEN 'P2'
           WHEN 3 THEN 'P3' WHEN 4 THEN 'P4' ELSE NULL END,
          CASE (x / 6) % 2 WHEN 0 THEN 'alice' ELSE 'bob' END,
          1, CASE (x / 6) % 3 WHEN 0 THEN 'z' WHEN 1 THEN 'Z' ELSE 'a' END, 'updated'
         FROM n;
         INSERT INTO item_provenance
          SELECT item_id, 'delta', 'thread', 'message', 'url', 'repo', 'rev', 'context' FROM items;
         INSERT INTO item_acceptance_criteria SELECT item_id, 0, 'Criterion' FROM items;",
        )
        .unwrap();
}

#[test]
fn production_common_page_plans_use_intended_indexes_without_order_by_sort() {
    let connection = storage::open(":memory:").unwrap();
    seed(&connection);
    let repository = ProjectionRepository::new(&connection);
    // Cover startup planning as well as stores with collected statistics.
    for analyzed in [false, true] {
        if analyzed {
            connection.execute_batch("ANALYZE").unwrap();
        }
        for kind in [
            ItemProjectionKind::Summary,
            ItemProjectionKind::Work,
            ItemProjectionKind::Audit,
        ] {
            for (view, ordering, index) in [
                (
                    NamedView::All,
                    ItemListOrdering::NewestFirst,
                    "idx_items_read_list",
                ),
                (
                    NamedView::Ready,
                    ItemListOrdering::Next,
                    "idx_items_read_ready",
                ),
                (
                    NamedView::Active,
                    ItemListOrdering::NewestFirst,
                    "idx_items_read_active",
                ),
                (
                    NamedView::Mine,
                    ItemListOrdering::NewestFirst,
                    "idx_items_read_mine",
                ),
            ] {
                let request = ItemProjectionPageRequest {
                    view,
                    configured_requester: RequesterId::new("alice").unwrap(),
                    filters: ItemListFilters::default(),
                    projection: kind,
                    ordering,
                    page: ReadPageRequest::new(10, None).unwrap(),
                };
                let plan = repository.explain_projection_page(&request).unwrap();
                assert!(
                    plan.iter()
                        .any(|detail| detail.contains(&format!("USING INDEX {index}"))),
                    "{view:?} {kind:?} analyzed={analyzed}: {plan:?}"
                );
                assert!(
                    plan.iter()
                        .all(|detail| !(detail.contains("TEMP B-TREE")
                            && detail.contains("ORDER BY"))),
                    "{view:?} {kind:?} analyzed={analyzed}: {plan:?}"
                );
                assert!(
                    plan.iter().all(|detail| !detail.contains("events")),
                    "{plan:?}"
                );
                if kind == ItemProjectionKind::Audit {
                    assert!(
                        plan.iter()
                            .any(|detail| detail.contains("LEFT-JOIN")
                                && detail.contains("USING INDEX")),
                        "{plan:?}"
                    );
                }
                // Exercise the production read as well, including work/audit hydration.
                repository.select_projection_page(&request).unwrap();
            }
        }
    }
}
