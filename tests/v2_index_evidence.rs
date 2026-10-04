//! Opt-in, disposable-fixture index experiment; never a timing-based CI gate.
#[allow(dead_code)]
mod support;

use std::{fs, path::Path, sync::Mutex, time::Instant};

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Clock, Command, Execution, HumanAuthorization,
        ItemListFilters, ItemListOrdering, ItemProjectionKind, ItemProjectionPageRequest,
        ItemProjectionStore, ItemReadKey, ItemTextFilter, MutationIdentity,
        MutationIdentityGenerator, MutationRequest, ObservedExecution, ReadPageRequest,
        mutate_item_idempotent,
    },
    benchmark_fixture,
    domain::{
        AssigneeId, ItemId, ItemMutation, NamedView, Priority, ProjectId, RequesterId, Timestamp,
        Triage, TriageField,
    },
    storage::{self, ItemRepository, MutationRepository, ProjectionRepository},
};
use rusqlite::{
    Connection, OpenFlags, StatementStatus,
    backup::Backup,
    trace::{TraceEvent, TraceEventCodes},
};
use serde::Serialize;
use support::OwnedTestDirectory;

// Compare subsets of the actual shipped SQL, never a second index definition.
const INDEX_MIGRATION: &str = include_str!("../migrations/0003_projection_read_indexes.sql");

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
struct Work {
    total_statements: usize,
    data_statements: usize,
    row_callbacks: usize,
    vm_steps: i64,
    fullscan_steps: i64,
    sorts: i64,
}

static TRACE: Mutex<Option<Work>> = Mutex::new(None);

fn trace(event: TraceEvent<'_>) {
    let mut guard = TRACE.lock().unwrap();
    let Some(work) = guard.as_mut() else {
        return;
    };
    match event {
        TraceEvent::Stmt(statement, _) => {
            work.total_statements += 1;
            if statement.sql().trim_start().starts_with("SELECT") {
                work.data_statements += 1;
            }
        }
        TraceEvent::Row(_) => work.row_callbacks += 1,
        TraceEvent::Profile(statement, _) => {
            work.vm_steps += i64::from(statement.get_status(StatementStatus::VmStep));
            work.fullscan_steps += i64::from(statement.get_status(StatementStatus::FullscanStep));
            work.sorts += i64::from(statement.get_status(StatementStatus::Sort));
        }
        _ => {}
    }
}

fn request(view: NamedView, ordering: ItemListOrdering) -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view,
        configured_requester: RequesterId::new("benchmark-agent").unwrap(),
        filters: ItemListFilters::default(),
        projection: ItemProjectionKind::Summary,
        ordering,
        page: ReadPageRequest::new(100, None).unwrap(),
    }
}

fn workloads() -> Vec<(&'static str, ItemProjectionPageRequest)> {
    let mut sparse = request(NamedView::All, ItemListOrdering::NewestFirst);
    sparse.filters.text = Some(ItemTextFilter::new("Needle-filter").unwrap());
    let mut single_status = request(NamedView::Active, ItemListOrdering::NewestFirst);
    single_status.filters.status = Some(bif::domain::Status::Blocked);
    let mut nulls = request(NamedView::Ready, ItemListOrdering::Next);
    nulls.page.after = Some(ItemReadKey {
        id: ItemId::new(
            RequesterId::new("BENCH").unwrap(),
            ProjectId::new("core").unwrap(),
            1,
        )
        .unwrap(),
        captured_at: Timestamp::new("0"),
        priority: None,
    });
    vec![
        (
            "all_list",
            request(NamedView::All, ItemListOrdering::NewestFirst),
        ),
        (
            "ready_next",
            request(NamedView::Ready, ItemListOrdering::Next),
        ),
        (
            "active_list",
            request(NamedView::Active, ItemListOrdering::NewestFirst),
        ),
        (
            "mine_list",
            request(NamedView::Mine, ItemListOrdering::NewestFirst),
        ),
        ("blocked_list", single_status),
        ("sparse_text", sparse),
        ("ready_null_boundary", nulls),
    ]
}

/// Copies only into a newly claimed file inside the owned experiment directory.
fn copy(source: &Connection, path: &Path) -> Connection {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    let mut destination = Connection::open(path).unwrap();
    Backup::new(source, &mut destination)
        .unwrap()
        .run_to_completion(128, std::time::Duration::ZERO, None)
        .unwrap();
    drop(destination);
    storage::open(path).unwrap()
}

struct FixedClock;
impl Clock for FixedClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new("2026-10-03T00:00:00Z")
    }
}

struct Identities(usize);
impl MutationIdentityGenerator for Identities {
    fn mutation_identity(&mut self) -> MutationIdentity {
        let operation_id = format!("phase-b-index-write-{}", self.0);
        self.0 += 1;
        MutationIdentity {
            event_ids: (0..3)
                .map(|i| format!("{operation_id}-event-{i}"))
                .collect(),
            operation_id,
        }
    }
}

fn write_samples(connection: &mut Connection) -> Vec<u128> {
    let (requester, project, sequence): (String, String, i64) = connection
        .query_row(
            "SELECT requester, project_id, sequence FROM items
         WHERE status = 'ready' ORDER BY item_id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let id = ItemId::new(
        RequesterId::new(requester).unwrap(),
        ProjectId::new(project).unwrap(),
        u64::try_from(sequence).unwrap(),
    )
    .unwrap();
    let mut revision =
        bif::application::ItemStore::read_item(&ItemRepository::new(connection), &id)
            .unwrap()
            .unwrap()
            .revision();
    let initial_revision = revision;
    let authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "BENCH",
            surface: "cli",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "cli",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: Command::Mutation {
            requested_changes: &["priority", "assignee", "note"],
        },
        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
    };
    let mut identities = Identities(0);
    let samples = (0..30)
        .map(|sample| {
            let mutation = ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    priority: TriageField::Set(if sample % 2 == 0 {
                        Priority::P0
                    } else {
                        Priority::P4
                    }),
                    assignee: TriageField::Set(
                        AssigneeId::new(if sample % 2 == 0 {
                            "benchmark-agent"
                        } else {
                            "bob"
                        })
                        .unwrap(),
                    ),
                    note: Some("disposable index-write evidence".to_owned()),
                }),
            };
            let request = MutationRequest {
                idempotency_key: format!("phase-b-index-key-{sample}"),
                item_id: id.clone(),
                expected_revision: revision,
                mutation,
            };
            let start = Instant::now();
            let result = mutate_item_idempotent(
                &mut MutationRepository::new(connection),
                &mut FixedClock,
                &mut identities,
                &authorization,
                request,
            )
            .unwrap();
            let elapsed = start.elapsed().as_nanos();
            assert!(!result.replayed);
            revision = result.item.revision();
            elapsed
        })
        .collect();
    assert_eq!(revision.get(), initial_revision.get() + 30);
    let notes: i64 = connection
        .query_row(
            "SELECT count(*) FROM events WHERE item_id = ?1
             AND note = 'disposable index-write evidence'",
            [id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(notes, 30);
    samples
}

#[test]
#[ignore = "set BIF_INDEX_FIXTURE to generated 10k/seed-2003 fixture; runs only disposable copies"]
fn compare_candidate_indexes_on_actual_queries() {
    let path = std::env::var_os("BIF_INDEX_FIXTURE").expect("BIF_INDEX_FIXTURE is required");
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let identity: String = source
        .query_row("SELECT store_id FROM store_metadata", [], |r| r.get(0))
        .unwrap();
    assert_eq!(identity, "benchmark-00000000000007d3-10000");
    assert_eq!(
        benchmark_fixture::summarize(&source).unwrap().digest,
        0x957657e192519763
    );
    fs::create_dir_all("target/bif-phase-b-evidence").unwrap();
    let directory = OwnedTestDirectory::in_directory(Path::new("target/bif-phase-b-evidence"));
    let cases = workloads();
    let mut expected = Vec::new();
    println!(
        "sqlite_version={}; artifacts={}",
        rusqlite::version(),
        directory.path().display()
    );
    for label in ["baseline", "two_indexes", "four_indexes"] {
        let path = directory.path().join(format!("{label}.sqlite"));
        let mut connection = copy(&source, &path);
        // This also permits rerunning with a post-migration generated fixture:
        // only disposable copies lose indexes; shipped migration rows stay intact.
        for name in [
            "idx_items_read_list",
            "idx_items_read_ready",
            "idx_items_read_active",
            "idx_items_read_mine",
        ] {
            connection
                .execute_batch(&format!("DROP INDEX IF EXISTS {name};"))
                .unwrap();
        }
        if label != "baseline" {
            connection.execute_batch(INDEX_MIGRATION).unwrap();
            if label == "two_indexes" {
                connection
                    .execute_batch(
                        "DROP INDEX idx_items_read_active; DROP INDEX idx_items_read_mine;",
                    )
                    .unwrap();
            }
        }
        connection
            .execute_batch("ANALYZE; PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        connection.set_prepared_statement_cache_capacity(0);
        let page_size: i64 = connection
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .unwrap();
        let pages: i64 = connection
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        let free: i64 = connection
            .pragma_query_value(None, "freelist_count", |r| r.get(0))
            .unwrap();
        println!(
            "{label} allocated_bytes={} occupied_bytes={}",
            pages * page_size,
            (pages - free) * page_size
        );
        for (index, (name, request)) in cases.iter().enumerate() {
            let repository = ProjectionRepository::new(&connection);
            let plan = repository.explain_projection_page(request).unwrap();
            let mut samples = Vec::new();
            let mut measured = None;
            for _ in 0..7 {
                *TRACE.lock().unwrap() = Some(Work::default());
                connection.trace_v2(TraceEventCodes::all(), Some(trace));
                let start = Instant::now();
                let page = repository.select_projection_page(request).unwrap();
                samples.push(start.elapsed().as_nanos());
                connection.trace_v2(TraceEventCodes::empty(), None);
                let work = TRACE.lock().unwrap().take().unwrap();
                assert_eq!(work.data_statements, 1);
                assert!(work.row_callbacks <= 101);
                if label == "baseline" && measured.is_none() {
                    expected.push(page.clone());
                } else {
                    assert_eq!(page, expected[index], "{label} {name}");
                }
                if let Some(previous) = &measured {
                    assert_eq!(&work, previous);
                }
                measured = Some(work);
            }
            println!(
                "{label} {name} plan={} work={} warm_ns={:?}",
                serde_json::to_string(&plan).unwrap(),
                serde_json::to_string(&measured.unwrap()).unwrap(),
                samples
            );
        }
        let samples = write_samples(&mut connection);
        let integrity: String = connection
            .pragma_query_value(None, "integrity_check", |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert!(
            connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
        println!("{label} durable_priority_assignee_note_write_ns={samples:?}");
    }
    assert_eq!(
        benchmark_fixture::summarize(&source).unwrap().digest,
        0x957657e192519763
    );
}
