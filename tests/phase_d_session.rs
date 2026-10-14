use std::{cell::RefCell, fs};

use bif::{
    application::{
        self, Actor, ActorKind, AuthorizationRequest, Command, ConditionalGetRequest,
        ConditionalReadOutcome, Execution, HumanAuthorization, ItemListFilters, ItemListOrdering,
        ItemProjectionKind, ObservedExecution, ProjectionGetRequest,
    },
    config::Config,
    domain::{ItemId, NamedView, ProjectId, RequesterId},
    read_session::{ReadRequest, ReadSession},
    storage::{self, ProjectionRepository},
    v2_response::{self, ReadError, ResponseBudget},
};
use rusqlite::{
    Connection, params,
    trace::{TraceEvent, TraceEventCodes},
};
use serde_json::Value;

fn authorization() -> AuthorizationRequest<'static> {
    AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "ALICE",
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

struct Fixture {
    directory: tempfile::TempDir,
    config: Config,
    writer: Connection,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            root: directory.path().to_owned(),
            requester: RequesterId::new("ALICE").unwrap(),
            source: None,
        };
        fs::create_dir(directory.path().join(".bif")).unwrap();
        let writer = storage::open(config.store_paths().unwrap().database).unwrap();
        let fixture = Self {
            directory,
            config,
            writer,
        };
        for project in ["alpha", "beta"] {
            fixture
                .writer
                .execute("INSERT INTO projects VALUES (?1, 'now')", [project])
                .unwrap();
            for sequence in 1..=3 {
                let id = format!("ALICE:{project}:{sequence:03}");
                fixture
                    .writer
                    .execute(
                        "INSERT INTO items VALUES (?1, 'ALICE', ?2, ?3, ?4, 'description',
                     'ready', 'P1', NULL, NULL, 1, ?5, 'now')",
                        params![
                            id,
                            project,
                            sequence,
                            format!("{project} {sequence}"),
                            format!("time-{sequence}")
                        ],
                    )
                    .unwrap();
                fixture
                    .writer
                    .execute("INSERT INTO item_provenance (item_id) VALUES (?1)", [&id])
                    .unwrap();
                fixture
                    .writer
                    .execute(
                        "INSERT INTO item_acceptance_criteria VALUES (?1, 0, 'complete')",
                        [&id],
                    )
                    .unwrap();
            }
        }
        fixture
    }

    fn session(&self) -> ReadSession {
        ReadSession::open(self.config.clone()).unwrap()
    }
    fn id(&self) -> ItemId {
        ItemId::new(
            RequesterId::new("ALICE").unwrap(),
            ProjectId::new("alpha").unwrap(),
            1,
        )
        .unwrap()
    }
    fn get(
        &self,
        projection: ItemProjectionKind,
        known_version: Option<String>,
        conditional: bool,
    ) -> ReadRequest {
        ReadRequest::Get {
            item_id: self.id(),
            projection,
            known_version,
            conditional,
        }
    }
}

fn run(session: &mut ReadSession, request: ReadRequest) -> Value {
    let bytes = session
        .execute(&authorization(), request, ResponseBudget::default())
        .unwrap();
    assert!(session.is_autocommit());
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn regular_get_preserves_bytes_and_conditional_observes_external_revisions() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let item = application::read_item_projection(
        &ProjectionRepository::new(&fixture.writer),
        &authorization(),
        &ProjectionGetRequest {
            item_id: fixture.id(),
            projection: ItemProjectionKind::Work,
        },
    )
    .unwrap();
    let mut expected = Vec::new();
    v2_response::write_get(&mut expected, &item, ResponseBudget::default()).unwrap();
    assert_eq!(
        session
            .execute(
                &authorization(),
                fixture.get(ItemProjectionKind::Work, None, false),
                ResponseBudget::default()
            )
            .unwrap(),
        expected
    );
    let first = run(
        &mut session,
        fixture.get(ItemProjectionKind::Work, None, true),
    );
    let version = first["result"]["version"].as_str().unwrap().to_owned();
    assert_eq!(first["result"]["outcome"], "modified");
    let hit = run(
        &mut session,
        fixture.get(ItemProjectionKind::Work, Some(version.clone()), false),
    );
    assert_eq!(hit["result"]["outcome"], "not_modified");
    assert!(hit["result"]["item"].is_null());
    assert_eq!(
        run(
            &mut session,
            fixture.get(ItemProjectionKind::Audit, Some(version.clone()), true)
        )["result"]["outcome"],
        "modified"
    );
    fixture
        .writer
        .execute(
            "UPDATE items SET revision = revision + 1, title = 'changed' WHERE item_id = ?1",
            [fixture.id().to_string()],
        )
        .unwrap();
    let changed = run(
        &mut session,
        fixture.get(ItemProjectionKind::Work, Some(version), true),
    );
    assert_eq!(changed["result"]["outcome"], "modified");
    assert_eq!(changed["result"]["item"]["title"], "changed");
}

#[test]
fn validators_check_authorization_and_existence_before_syntax_and_never_hit_other_stores() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let value = run(
        &mut session,
        fixture.get(ItemProjectionKind::Summary, None, true),
    );
    let version = value["result"]["version"].as_str().unwrap().to_owned();
    let other = Fixture::new();
    assert_eq!(
        run(
            &mut other.session(),
            other.get(ItemProjectionKind::Summary, Some(version), true)
        )["result"]["outcome"],
        "modified"
    );
    let bad = fixture.get(ItemProjectionKind::Summary, Some("bifv99.bad".into()), true);
    assert_eq!(
        session.execute(&authorization(), bad, ResponseBudget::default()),
        Err(ReadError::InvalidInput)
    );
    let mut denied = authorization();
    denied.observed_execution = ObservedExecution::Agent {
        agent_id: "agent:unexpected",
    };
    assert_eq!(
        session.execute(
            &denied,
            fixture.get(ItemProjectionKind::Work, Some("bad".into()), true),
            ResponseBudget::default()
        ),
        Err(ReadError::Unauthorized)
    );
    fixture
        .writer
        .execute(
            "DELETE FROM items WHERE item_id = ?1",
            [fixture.id().to_string()],
        )
        .unwrap();
    assert_eq!(
        session.execute(
            &authorization(),
            fixture.get(ItemProjectionKind::Work, Some("bad".into()), true),
            ResponseBudget::default()
        ),
        Err(ReadError::NotFound)
    );
    assert!(session.is_autocommit());
}

fn page(project: &str, cursor: Option<String>) -> ReadRequest {
    ReadRequest::Page {
        view: NamedView::All,
        ordering: ItemListOrdering::NewestFirst,
        projection: ItemProjectionKind::Summary,
        filters: ItemListFilters {
            project: Some(ProjectId::new(project).unwrap()),
            ..Default::default()
        },
        limit: 1,
        cursor,
    }
}

#[test]
fn explicit_projects_interleave_and_cursor_survives_session_restart() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let first = run(&mut session, page("alpha", None));
    let cursor = first["result"]["next_cursor"].as_str().unwrap().to_owned();
    for project in ["alpha", "beta", "alpha"] {
        let selected = run(
            &mut session,
            ReadRequest::SelectedWork {
                project: ProjectId::new(project).unwrap(),
            },
        );
        assert_eq!(selected["result"]["outcome"], "selected");
        assert_eq!(selected["result"]["item"]["description"], "description");
        assert!(
            selected["result"]["item"]["id"]
                .as_str()
                .unwrap()
                .contains(project)
        );
    }
    drop(session);
    let second = run(&mut fixture.session(), page("alpha", Some(cursor)));
    assert_ne!(
        first["result"]["items"][0]["id"],
        second["result"]["items"][0]["id"]
    );
    let empty = run(
        &mut fixture.session(),
        ReadRequest::SelectedWork {
            project: ProjectId::new("empty").unwrap(),
        },
    );
    assert_eq!(empty["result"]["outcome"], "empty");
    assert!(empty["result"]["item"].is_null());
}

#[test]
fn identity_schema_and_migration_changes_require_restart_and_poison_session() {
    for sql in [
        "UPDATE store_metadata SET store_id = 'changed'",
        "CREATE TABLE external_schema_change (value TEXT)",
        "UPDATE schema_migrations SET checksum = 'changed' WHERE version = 3",
        "DELETE FROM schema_migrations WHERE version = 3",
        "UPDATE schema_migrations SET name = 'changed' WHERE version = 3",
    ] {
        let fixture = Fixture::new();
        let mut session = fixture.session();
        fixture.writer.execute_batch(sql).unwrap();
        for _ in 0..2 {
            assert!(
                matches!(
                    session.execute(
                        &authorization(),
                        page("alpha", None),
                        ResponseBudget::default()
                    ),
                    Err(ReadError::RestartRequired { .. })
                ),
                "{sql}"
            );
            assert!(session.is_autocommit());
        }
    }
}

#[test]
fn resolver_restart_remains_required_after_identity_or_schema_is_restored() {
    for change in ["identity", "schema"] {
        let fixture = Fixture::new();
        let cwd = fixture.directory.path().canonicalize().unwrap();
        fixture
            .writer
            .execute(
                "INSERT INTO project_path_mappings VALUES (?1, 'alpha')",
                [cwd.to_str().unwrap()],
            )
            .unwrap();
        let store_id: String = fixture
            .writer
            .query_row("SELECT store_id FROM store_metadata", [], |row| row.get(0))
            .unwrap();
        let schema_version: i64 = fixture
            .writer
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap();
        let mut session = fixture.session();
        assert_eq!(
            session.resolve_project(&cwd, None).unwrap(),
            ProjectId::new("alpha").unwrap()
        );
        run(&mut session, page("alpha", None));

        match change {
            "identity" => {
                fixture
                    .writer
                    .execute("UPDATE store_metadata SET store_id = 'changed'", [])
                    .unwrap();
            }
            "schema" => fixture
                .writer
                .pragma_update(None, "schema_version", schema_version + 1)
                .unwrap(),
            _ => unreachable!(),
        }
        let expected = Err(ReadError::RestartRequired {
            reason: "store_or_schema_changed".into(),
        });
        assert_eq!(session.resolve_project(&cwd, None), expected, "{change}");
        assert!(session.is_autocommit());

        // The separate writer restores every boundary value; only the observed
        // restart requirement should keep this session from accepting requests.
        fixture
            .writer
            .execute("UPDATE store_metadata SET store_id = ?1", [store_id])
            .unwrap();
        fixture
            .writer
            .pragma_update(None, "schema_version", schema_version)
            .unwrap();
        let mut fresh = fixture.session();
        assert_eq!(
            fresh.resolve_project(&cwd, None).unwrap(),
            ProjectId::new("alpha").unwrap()
        );
        run(&mut fresh, page("alpha", None));

        assert_eq!(
            session.execute(
                &authorization(),
                page("alpha", None),
                ResponseBudget::default()
            ),
            Err(ReadError::RestartRequired {
                reason: "store_or_schema_changed".into(),
            }),
            "{change}"
        );
        assert!(session.is_autocommit());
        assert_eq!(session.resolve_project(&cwd, None), expected, "{change}");
        assert!(session.is_autocommit());
    }
}

#[cfg(unix)]
#[test]
fn replacing_database_file_is_detected_even_when_store_identity_is_copied() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let database = fixture.config.store_paths().unwrap().database;
    fixture
        .writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    fs::copy(&database, fixture.directory.path().join("replacement")).unwrap();
    fs::rename(fixture.directory.path().join("replacement"), &database).unwrap();
    assert!(matches!(
        session.execute(
            &authorization(),
            page("alpha", None),
            ResponseBudget::default()
        ),
        Err(ReadError::RestartRequired { .. })
    ));
    assert!(session.is_autocommit());
}

thread_local! { static QUERIES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }
fn trace(event: TraceEvent<'_>) {
    if let TraceEvent::Stmt(statement, _) = event {
        QUERIES.with(|queries| queries.borrow_mut().push(statement.sql().into_owned()));
    }
}

#[test]
fn conditional_hit_queries_only_revision_and_binding_not_children_or_provenance() {
    let fixture = Fixture::new();
    let repository = ProjectionRepository::new(&fixture.writer);
    let mut request = ConditionalGetRequest {
        item_id: fixture.id(),
        projection: ItemProjectionKind::Audit,
        known_version: None,
    };
    let first =
        application::read_conditional_item_projection(&repository, &authorization(), &request)
            .unwrap();
    let ConditionalReadOutcome::Modified { version, .. } = first else {
        panic!("modified")
    };
    request.known_version = Some(version);
    QUERIES.with(|queries| queries.borrow_mut().clear());
    fixture.writer.trace_v2(TraceEventCodes::all(), Some(trace));
    assert!(matches!(
        application::read_conditional_item_projection(&repository, &authorization(), &request)
            .unwrap(),
        ConditionalReadOutcome::NotModified { .. }
    ));
    fixture.writer.trace_v2(TraceEventCodes::empty(), None);
    let queries = QUERIES.with(|queries| queries.borrow().clone());
    assert_eq!(
        queries
            .iter()
            .filter(|sql| sql.contains("FROM items"))
            .count(),
        1
    );
    assert!(
        !queries
            .iter()
            .any(|sql| sql.contains("item_acceptance_criteria") || sql.contains("item_provenance"))
    );
    assert!(fixture.writer.is_autocommit());
}

#[test]
fn errors_and_small_budgets_do_not_leak_transactions() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    assert!(
        session
            .execute(
                &authorization(),
                page("alpha", Some("bad".into())),
                ResponseBudget::default()
            )
            .is_err()
    );
    assert!(session.is_autocommit());
    assert!(
        session
            .execute(
                &authorization(),
                fixture.get(ItemProjectionKind::Audit, None, true),
                ResponseBudget::new(1).unwrap()
            )
            .is_err()
    );
    assert!(session.is_autocommit());
    assert_eq!(
        run(
            &mut session,
            ReadRequest::History {
                item_id: fixture.id(),
                limit: 1,
                cursor: None
            }
        )["result"]["events"],
        serde_json::json!([])
    );
}

fn token_json(token: &str) -> Value {
    let bytes = token
        .strip_prefix("bifc1.")
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    serde_json::from_slice(&bytes).unwrap()
}

fn json_token(value: &Value) -> String {
    format!(
        "bifc1.{}",
        serde_json::to_vec(value)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[test]
fn generation_less_cursors_and_validators_require_explicit_restart_or_refetch() {
    let fixture = Fixture::new();
    let first = run(&mut fixture.session(), page("alpha", None));
    let token = first["result"]["next_cursor"].as_str().unwrap();
    for omitted in [false, true] {
        let mut envelope = token_json(token);
        if omitted {
            envelope.as_object_mut().unwrap().remove("generation");
        } else {
            envelope["generation"] = Value::Null;
        }
        let error = fixture
            .session()
            .execute(
                &authorization(),
                page("alpha", Some(json_token(&envelope))),
                ResponseBudget::default(),
            )
            .unwrap_err();
        assert_eq!(
            serde_json::to_value(error).unwrap()["details"],
            serde_json::json!({"reason":"generation_missing","restart_required":true})
        );
    }
    let legacy = format!(
        "bifv1.{}.{}.work.{}.1",
        storage::read_store_identity(&fixture.writer)
            .unwrap()
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        fixture
            .id()
            .to_string()
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        fixture
            .writer
            .query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
            .unwrap()
    );
    for token in [legacy.clone(), format!("{legacy}.null")] {
        let error = fixture
            .session()
            .execute(
                &authorization(),
                fixture.get(ItemProjectionKind::Work, Some(token), true),
                ResponseBudget::default(),
            )
            .unwrap_err();
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], "invalid_input");
        assert_eq!(
            value["details"],
            serde_json::json!({"reason":"generation_missing","refetch_required":true})
        );
    }
}

#[test]
fn same_file_restore_rotates_tokens_and_latches_warm_session_restart() {
    let fixture = Fixture::new();
    let mut warm = fixture.session();
    let first = run(&mut warm, page("alpha", None));
    let cursor = first["result"]["next_cursor"].as_str().unwrap().to_owned();
    let first = run(&mut warm, fixture.get(ItemProjectionKind::Work, None, true));
    let version = first["result"]["version"].as_str().unwrap().to_owned();
    fixture.writer.execute_batch(
        "INSERT INTO operations VALUES ('history-op', 'ALICE:alpha:001', 'triage', 1, 2, 'now');
         INSERT INTO events (event_id, operation_id, item_id, item_revision, event_index,
          event_type, before_value, after_value, actor_kind, actor_id, actor_surface, actor_host,
          execution_kind, execution_surface, execution_host, occurred_at, event_schema_version)
         VALUES ('h0', 'history-op', 'ALICE:alpha:001', 2, 0, 'note_added', NULL, 'one',
          'human', 'ALICE', 'cli', 'local', 'direct', 'cli', 'local', 'now', 1),
         ('h1', 'history-op', 'ALICE:alpha:001', 2, 1, 'note_added', NULL, 'two',
          'human', 'ALICE', 'cli', 'local', 'direct', 'cli', 'local', 'now', 1);").unwrap();
    let history = ReadRequest::History {
        item_id: fixture.id(),
        limit: 1,
        cursor: None,
    };
    let first_history = run(&mut warm, history.clone());
    let history_cursor = first_history["result"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let continuation = ReadRequest::History {
        item_id: fixture.id(),
        limit: 1,
        cursor: Some(history_cursor),
    };
    assert_eq!(
        run(&mut fixture.session(), continuation.clone())["result"]["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        run(
            &mut fixture.session(),
            fixture.get(ItemProjectionKind::Work, Some(version.clone()), true)
        )["result"]["outcome"],
        "not_modified"
    );
    let database = fixture.config.store_paths().unwrap().database;
    let file = same_file::Handle::from_path(&database).unwrap();
    let store = storage::read_store_identity(&fixture.writer).unwrap();
    let old_generation: String = fixture
        .writer
        .query_row("SELECT generation FROM journal_metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    let schema: i64 = fixture
        .writer
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    fixture
        .writer
        .execute_batch(
            "BEGIN IMMEDIATE;
         UPDATE journal_metadata SET generation = '00000000000000000000000000000000';
         UPDATE items SET title = 'restored content' WHERE item_id = 'ALICE:alpha:001';
         COMMIT;",
        )
        .unwrap();
    assert_eq!(file, same_file::Handle::from_path(&database).unwrap());
    assert_eq!(
        store,
        storage::read_store_identity(&fixture.writer).unwrap()
    );
    assert_eq!(
        schema,
        fixture
            .writer
            .query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
            .unwrap()
    );
    assert_eq!(
        fixture
            .writer
            .query_row(
                "SELECT revision FROM items WHERE item_id = 'ALICE:alpha:001'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        fixture.session().execute(
            &authorization(),
            page("alpha", Some(cursor)),
            ResponseBudget::default()
        ),
        Err(ReadError::InvalidCursor { .. })
    ));
    assert!(matches!(
        fixture
            .session()
            .execute(&authorization(), continuation, ResponseBudget::default()),
        Err(ReadError::InvalidCursor { .. })
    ));
    let changed = run(
        &mut fixture.session(),
        fixture.get(ItemProjectionKind::Work, Some(version), true),
    );
    assert_eq!(changed["result"]["outcome"], "modified");
    assert_eq!(changed["result"]["item"]["title"], "restored content");
    for _ in 0..2 {
        assert!(matches!(
            warm.execute(
                &authorization(),
                page("alpha", None),
                ResponseBudget::default()
            ),
            Err(ReadError::RestartRequired { .. })
        ));
        assert!(warm.is_autocommit());
        fixture
            .writer
            .execute(
                "UPDATE journal_metadata SET generation = ?1",
                [&old_generation],
            )
            .unwrap();
        run(&mut fixture.session(), page("alpha", None));
    }
}
