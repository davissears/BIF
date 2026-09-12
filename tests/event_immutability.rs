use bif::storage::migrate;
use rusqlite::Connection;

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_initial.sql");
const INITIAL_CHECKSUM: &str = "6cce50f20fc62f82af5432d4e2be81c29a0f44a73ca5245353f98c6c6030f85e";

fn insert_event_fixture(connection: &Connection) {
    connection
        .execute_batch(
            "
            INSERT INTO projects (project_id, created_at) VALUES ('bif', 'now');
            INSERT INTO items (
                item_id, requester, project_id, sequence, title, status, revision,
                captured_at, updated_at
            ) VALUES (
                'DAVIS:bif:001', 'DAVIS', 'bif', 1, 'Immutable event',
                'proposed', 1, 'now', 'now'
            );
            INSERT INTO operations (
                operation_id, item_id, operation_type, item_revision, occurred_at
            ) VALUES ('op-1', 'DAVIS:bif:001', 'capture', 1, 'now');
            INSERT INTO events (
                event_id, operation_id, item_id, item_revision, event_index,
                event_type, actor_kind, actor_id, actor_surface, actor_host,
                execution_kind, execution_agent_id, execution_surface,
                execution_host, occurred_at, event_schema_version
            ) VALUES (
                'event-1', 'op-1', 'DAVIS:bif:001', 1, 0, 'captured',
                'agent', 'agent-1', 'thread', 'delta', 'agent', 'agent-1',
                'thread', 'delta', 'now', 1
            );
            ",
        )
        .unwrap();
}

#[test]
fn events_can_be_inserted_but_not_updated_or_deleted() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();

    insert_event_fixture(&connection);

    let update_error = connection
        .execute(
            "UPDATE events SET note = 'amended' WHERE event_id = 'event-1'",
            [],
        )
        .unwrap_err();
    assert!(update_error.to_string().contains("events are immutable"));

    let delete_error = connection
        .execute("DELETE FROM events WHERE event_id = 'event-1'", [])
        .unwrap_err();
    assert!(delete_error.to_string().contains("events are immutable"));

    let event: (String, Option<String>) = connection
        .query_row(
            "SELECT event_id, note FROM events WHERE event_id = 'event-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(event, ("event-1".to_owned(), None));
}

#[test]
fn existing_v1_database_upgrades_and_repeat_startup_is_valid() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(INITIAL_SCHEMA).unwrap();
    connection
        .execute_batch(
            "
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY CHECK (version > 0),
                name TEXT NOT NULL CHECK (length(name) > 0),
                checksum TEXT NOT NULL CHECK (length(checksum) > 0),
                applied_at TEXT NOT NULL CHECK (length(applied_at) > 0)
            );
            ",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at)
             VALUES (1, 'initial', ?1, 'now')",
            [INITIAL_CHECKSUM],
        )
        .unwrap();

    migrate(&mut connection).unwrap();
    migrate(&mut connection).unwrap();

    let migrations: i64 = connection
        .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migrations, 2);

    insert_event_fixture(&connection);
    assert!(
        connection
            .execute("DELETE FROM events WHERE event_id = 'event-1'", [])
            .is_err()
    );
}
