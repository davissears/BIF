use rusqlite::{Connection, Result, params};

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_initial.sql");

const REQUIRED_TABLES: &[&str] = &[
    "store_metadata",
    "projects",
    "project_path_mappings",
    "project_remote_mappings",
    "requester_project_counters",
    "items",
    "item_acceptance_criteria",
    "item_provenance",
    "operations",
    "events",
    "mutation_receipts",
];

#[test]
fn fresh_database_has_complete_constrained_schema_and_persistent_store_id() -> Result<()> {
    let temporary = std::env::temp_dir().join(format!(
        "bif-schema-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock must follow Unix epoch")
            .as_nanos()
    ));

    let store_id: String;
    {
        let connection = Connection::open(&temporary)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        connection.execute_batch(INITIAL_SCHEMA)?;

        let mut table_query = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?1")?;
        for table in REQUIRED_TABLES {
            assert_eq!(
                table_query.query_row([table], |row| row.get::<_, String>(0))?,
                *table
            );
        }

        store_id = connection.query_row(
            "SELECT store_id FROM store_metadata WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        assert!(!store_id.is_empty());

        connection.execute(
            "INSERT INTO projects (project_id, created_at) VALUES ('bif', 'now')",
            [],
        )?;
        connection.execute(
            "INSERT INTO requester_project_counters
                (requester, project_id, next_sequence) VALUES ('DAVIS', 'bif', 1)",
            [],
        )?;
        connection.execute(
            "INSERT INTO items (
                item_id, requester, project_id, sequence, title, status, revision,
                captured_at, updated_at
             ) VALUES ('DAVIS:bif:001', 'DAVIS', 'bif', 1, 'Schema', 'proposed', 1, 'now', 'now')",
            [],
        )?;
        connection.execute(
            "INSERT INTO item_acceptance_criteria (item_id, criterion_index, criterion)
             VALUES ('DAVIS:bif:001', 0, 'first'), ('DAVIS:bif:001', 1, 'second')",
            [],
        )?;
        connection.execute(
            "INSERT INTO item_provenance (item_id, source_host, thread_id)
             VALUES ('DAVIS:bif:001', 'delta', 'thread-1')",
            [],
        )?;
        connection.execute(
            "INSERT INTO operations (
                operation_id, item_id, operation_type, item_revision, occurred_at
             ) VALUES ('op-1', 'DAVIS:bif:001', 'capture', 1, 'now')",
            [],
        )?;
        connection.execute(
            "INSERT INTO events (
                event_id, operation_id, item_id, item_revision, event_index, event_type,
                actor_kind, actor_id, actor_surface, actor_host, execution_kind,
                execution_agent_id, execution_surface, execution_host, occurred_at,
                event_schema_version
             ) VALUES (
                'event-1', 'op-1', 'DAVIS:bif:001', 1, 0, 'captured',
                'agent', 'agent-1', 'thread', 'delta', 'agent',
                'agent-1', 'thread', 'delta', 'now', 1
             )",
            [],
        )?;
        connection.execute(
            "INSERT INTO mutation_receipts (
                mutation_key, mutation_type, payload_hash, operation_id, item_id,
                response_json, created_at
             ) VALUES ('key-1', 'capture', 'hash-1', 'op-1', 'DAVIS:bif:001', '{}', 'now')",
            [],
        )?;

        assert!(
            connection
                .execute(
                    "INSERT INTO items (
                item_id, requester, project_id, sequence, title, status, revision,
                captured_at, updated_at
             ) VALUES ('bad', 'DAVIS', 'bif', 0, 'bad', 'unknown', 0, 'now', 'now')",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO project_path_mappings (canonical_path, project_id)
             VALUES ('/missing', 'missing')",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO item_acceptance_criteria (item_id, criterion_index, criterion)
             VALUES ('DAVIS:bif:001', 0, 'duplicate position')",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO events (
                event_id, operation_id, item_id, item_revision, event_index, event_type,
                actor_kind, actor_id, actor_surface, actor_host, execution_kind,
                execution_agent_id, execution_surface, execution_host, occurred_at,
                event_schema_version
             ) VALUES (
                'event-2', 'op-1', 'DAVIS:bif:001', 1, 1, 'captured',
                'human', 'DAVIS', 'cli', 'local', 'direct',
                'agent-should-be-null', 'cli', 'local', 'now', 1
             )",
                    [],
                )
                .is_err()
        );

        let criteria: Vec<String> = connection
            .prepare(
                "SELECT criterion FROM item_acceptance_criteria
                 WHERE item_id = ?1 ORDER BY criterion_index",
            )?
            .query_map(params!["DAVIS:bif:001"], |row| row.get(0))?
            .collect::<Result<_>>()?;
        assert_eq!(criteria, ["first", "second"]);
    }

    let reopened = Connection::open(&temporary)?;
    let persisted_id: String =
        reopened.query_row("SELECT store_id FROM store_metadata", [], |row| row.get(0))?;
    assert_eq!(persisted_id, store_id);
    drop(reopened);
    std::fs::remove_file(temporary).expect("temporary schema database should be removable");
    Ok(())
}
