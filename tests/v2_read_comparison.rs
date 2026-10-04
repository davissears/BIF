//! Opt-in Phase C measurements, never a latency-based CI gate or an active ledger reader.

#[allow(dead_code)]
mod support;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bif::{
    application::{
        CursorContext, HistoryOrdering, HistoryPageRequest, HistoryReadKey, ItemHistoryPageStore,
        ItemListFilters, ItemListOrdering, ItemProjectionKind, ItemProjectionPageRequest,
        ItemProjectionStore, ItemReadKey, ItemTextFilter, NamedViewStore, PageOffset, PageSize,
        Pagination, ReadPageRequest,
    },
    benchmark_fixture,
    domain::{ItemId, NamedView, Priority, ProjectId, RequesterId},
    rpc_read,
    storage::{self, ItemHistoryRepository, ItemRepository, ProjectionRepository},
    v2_response::{self, ReadError, ResponseBudget},
};
use rusqlite::{
    Connection, OpenFlags, StatementStatus,
    backup::Backup,
    trace::{TraceEvent, TraceEventCodes},
};
use serde::Serialize;
use serde_json::{Value, json};
use support::OwnedTestDirectory;

const FIXTURES: [(usize, u64); 3] = [
    (100, 0xb819125481255ec7),
    (10_000, 0x957657e192519763),
    (100_000, 0x87a07c0632977092),
];
static TRACE: Mutex<Option<Work>> = Mutex::new(None);

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
struct Work {
    total_statements: usize,
    data_statements: usize,
    primary_row_callbacks: usize,
    criteria_row_callbacks: usize,
    other_row_callbacks: usize,
    vm_steps: i64,
    fullscan_steps: i64,
    sorts: i64,
    #[serde(skip)]
    sql_shapes: Vec<String>,
}

/// Counters include transaction statements. Row callbacks are not scanned rows.
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
                work.sql_shapes.push(statement.sql().into_owned());
            }
        }
        TraceEvent::Row(statement) => {
            let sql = statement.sql();
            // A text-search primary statement also contains a correlated
            // criteria subquery; those callbacks are primary, not hydration.
            if sql.contains("FROM items") || sql.contains("FROM events") {
                work.primary_row_callbacks += 1;
            } else if sql.contains("FROM item_acceptance_criteria") {
                work.criteria_row_callbacks += 1;
            } else {
                work.other_row_callbacks += 1;
            }
        }
        TraceEvent::Profile(statement, _) => {
            work.vm_steps += i64::from(statement.get_status(StatementStatus::VmStep));
            work.fullscan_steps += i64::from(statement.get_status(StatementStatus::FullscanStep));
            work.sorts += i64::from(statement.get_status(StatementStatus::Sort));
        }
        _ => {}
    }
}

#[derive(Serialize)]
struct Distribution {
    p50: u128,
    p95: u128,
    samples: Vec<u128>,
}

fn distribution(samples: Vec<u128>) -> Distribution {
    assert!(!samples.is_empty());
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    let rank = |percent: usize| (sorted.len() * percent).div_ceil(100) - 1;
    Distribution {
        p50: sorted[rank(50)],
        p95: sorted[rank(95)],
        samples,
    }
}

#[derive(Serialize)]
struct Sample {
    read_nanoseconds: u128,
    serialization_nanoseconds: u128,
    response_bytes: usize,
    work: Work,
}

/// The first read is session-first, not filesystem-cold (setup already read it).
fn measure<T>(
    connection: &Connection,
    samples: usize,
    mut read: impl FnMut() -> T,
    mut encode: impl FnMut(&T) -> Vec<u8>,
) -> Value {
    let mut observed = Vec::new();
    for _ in 0..=samples {
        *TRACE.lock().unwrap() = Some(Work::default());
        connection.trace_v2(TraceEventCodes::all(), Some(trace));
        let start = Instant::now();
        let result = read();
        let read_nanoseconds = start.elapsed().as_nanos();
        connection.trace_v2(TraceEventCodes::empty(), None);
        let work = TRACE.lock().unwrap().take().unwrap();
        let start = Instant::now();
        let bytes = encode(&result);
        observed.push(Sample {
            read_nanoseconds,
            serialization_nanoseconds: start.elapsed().as_nanos(),
            response_bytes: bytes.len(),
            work,
        });
    }
    let first = observed.remove(0);
    json!({
        "actual_data_sql_shapes": first.work.sql_shapes,
        "session_first": first,
        "warm_read_nanoseconds": distribution(observed.iter().map(|s| s.read_nanoseconds).collect()),
        "warm_serialization_nanoseconds": distribution(observed.iter().map(|s| s.serialization_nanoseconds).collect()),
        "warm_response_bytes": distribution(observed.iter().map(|s| s.response_bytes as u128).collect()),
        "warm_work": observed.iter().map(|s| &s.work).collect::<Vec<_>>(),
    })
}

fn request(kind: ItemProjectionKind, limit: usize) -> ItemProjectionPageRequest {
    ItemProjectionPageRequest {
        view: NamedView::All,
        configured_requester: RequesterId::new("benchmark-agent").unwrap(),
        filters: ItemListFilters::default(),
        projection: kind,
        ordering: ItemListOrdering::NewestFirst,
        page: ReadPageRequest::new(limit, None).unwrap(),
    }
}

fn projection_measurement(
    connection: &Connection,
    identity: &str,
    samples: usize,
    name: &str,
    request: &ItemProjectionPageRequest,
) -> Value {
    let context = CursorContext::item_page(identity, request).unwrap();
    let repository = ProjectionRepository::new(connection);
    let plan = repository.explain_projection_page(request).unwrap();
    let measurement = measure(
        connection,
        samples,
        || repository.select_projection_page(request).unwrap(),
        |page| {
            let mut bytes = Vec::new();
            v2_response::write_item_page(&mut bytes, page, ResponseBudget::default(), |row| {
                context
                    .encode_item_key(&row.key)
                    .map_err(|_| ReadError::Internal)
            })
            .unwrap();
            bytes
        },
    );
    let page = repository.select_projection_page(request).unwrap();
    let expected_statements =
        if request.projection == ItemProjectionKind::Summary || page.records.is_empty() {
            1
        } else {
            2
        };
    for work in std::iter::once(&measurement["session_first"]["work"])
        .chain(measurement["warm_work"].as_array().unwrap())
    {
        assert_eq!(work["data_statements"], expected_statements);
        assert!(work["primary_row_callbacks"].as_u64().unwrap() <= request.page.row_limit() as u64);
    }
    json!({
        "name": name,
        "projection": format!("{:?}", request.projection),
        "view": format!("{:?}", request.view),
        "ordering": format!("{:?}", request.ordering),
        "filters": format!("{:?}", request.filters),
        "limit": request.page.limit.get(),
        "after": request.page.after.as_ref().map(|key| format!("{key:?}")),
        "returned_records": page.records.len(),
        "has_more": page.has_more,
        "query_plan": plan,
        "measurement": measurement,
    })
}

/// Read legacy and keyset pages at exactly the same logical boundary.
fn legacy_measurement(
    connection: &Connection,
    samples: usize,
    name: &str,
    request: &ItemProjectionPageRequest,
    offset: usize,
) -> Value {
    let repository = ItemRepository::new(connection);
    let read = || {
        repository
            .select_item_page(
                request.view,
                &request.configured_requester,
                &request.filters,
                request.ordering,
                Pagination::new(
                    PageSize::new(request.page.limit.get()).unwrap(),
                    PageOffset::new(offset),
                ),
            )
            .unwrap()
    };
    let expected = ProjectionRepository::new(connection)
        .select_projection_page(request)
        .unwrap();
    let page = read();
    assert_eq!(
        page.items.iter().map(|item| item.id()).collect::<Vec<_>>(),
        expected
            .records
            .iter()
            .map(|row| &row.key.id)
            .collect::<Vec<_>>(),
        "{name}: cursor/offset pages differ"
    );
    let measurement = measure(connection, samples, read, |page| {
        serde_json::to_vec(&rpc_read::list_result_json(page)).unwrap()
    });
    for work in std::iter::once(&measurement["session_first"]["work"])
        .chain(measurement["warm_work"].as_array().unwrap())
    {
        assert_eq!(
            work["data_statements"],
            if page.items.is_empty() { 1 } else { 2 }
        );
    }
    json!({
        "name": name, "offset": offset, "limit": request.page.limit.get(),
        "returned_records": page.items.len(), "measurement": measurement,
        "cursor_offset_ids_equal": true,
    })
}

/// Untimed traversal builds an actual last-row boundary; it also warms caches.
fn boundary_at(
    connection: &Connection,
    request: &ItemProjectionPageRequest,
    depth: usize,
) -> Option<ItemReadKey> {
    let mut traversal = request.clone();
    traversal.projection = ItemProjectionKind::Summary;
    traversal.page.after = None;
    let mut remaining = depth;
    while remaining != 0 {
        traversal.page =
            ReadPageRequest::new(remaining.min(100), traversal.page.after.take()).unwrap();
        let page = ProjectionRepository::new(connection)
            .select_projection_page(&traversal)
            .unwrap();
        assert!(!page.records.is_empty(), "requested depth exceeds matches");
        remaining -= page.records.len();
        traversal.page.after = page.records.last().map(|row| row.key.clone());
    }
    traversal.page.after
}

fn read_comparison(
    connection: &Connection,
    identity: &str,
    items: usize,
    samples: usize,
) -> Vec<Value> {
    let mut cases = Vec::new();
    for kind in [
        ItemProjectionKind::Summary,
        ItemProjectionKind::Work,
        ItemProjectionKind::Audit,
    ] {
        for limit in [1, 10, 100] {
            let request = request(kind, limit);
            cases.push(projection_measurement(
                connection,
                identity,
                samples,
                "first_list",
                &request,
            ));
        }
    }
    for (name, view, ordering) in [
        ("ready_next", NamedView::Ready, ItemListOrdering::Next),
        (
            "active_list",
            NamedView::Active,
            ItemListOrdering::NewestFirst,
        ),
        ("mine_list", NamedView::Mine, ItemListOrdering::NewestFirst),
    ] {
        let mut request = request(ItemProjectionKind::Summary, 100);
        request.view = view;
        request.ordering = ordering;
        cases.push(projection_measurement(
            connection, identity, samples, name, &request,
        ));
    }
    for text in [false, true] {
        let mut request = request(ItemProjectionKind::Summary, 100);
        let name = if text {
            request.filters.text = Some(ItemTextFilter::new("Needle-filter").unwrap());
            "sparse_literal_text"
        } else {
            request.filters.project = Some(ProjectId::new("rare-project").unwrap());
            "sparse_project"
        };
        cases.push(projection_measurement(
            connection, identity, samples, name, &request,
        ));
        cases.push(legacy_measurement(
            connection,
            samples,
            &format!("legacy_{name}"),
            &request,
            0,
        ));
    }
    let mut first = request(ItemProjectionKind::Summary, 100);
    cases.push(legacy_measurement(
        connection,
        samples,
        "legacy_first_list",
        &first,
        0,
    ));
    let depth = items * 9 / 10;
    first.page.after = boundary_at(connection, &first, depth);
    first.page = ReadPageRequest::new(10, first.page.after).unwrap();
    for kind in [
        ItemProjectionKind::Summary,
        ItemProjectionKind::Work,
        ItemProjectionKind::Audit,
    ] {
        first.projection = kind;
        cases.push(projection_measurement(
            connection,
            identity,
            samples,
            "deep_list",
            &first,
        ));
    }
    cases.push(legacy_measurement(
        connection,
        samples,
        "legacy_deep_list",
        &first,
        depth,
    ));

    // Isolate same-priority next depth: a keyset can still scan a priority partition.
    let mut next = request(ItemProjectionKind::Work, 10);
    next.view = NamedView::Ready;
    next.ordering = ItemListOrdering::Next;
    next.filters.priority = Some(Priority::P4);
    let matches: i64 = connection
        .query_row(
            "SELECT count(*) FROM items WHERE status='ready' AND priority='P4'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let matches = usize::try_from(matches).expect("SQLite match count must fit usize");
    cases.push(projection_measurement(
        connection,
        identity,
        samples,
        "same_priority_next_first",
        &next,
    ));
    let depth = matches * 9 / 10;
    next.page.after = boundary_at(connection, &next, depth);
    cases.push(projection_measurement(
        connection,
        identity,
        samples,
        "same_priority_next_deep",
        &next,
    ));
    cases.push(legacy_measurement(
        connection,
        samples,
        "legacy_same_priority_next_deep",
        &next,
        depth,
    ));
    cases.extend(history_comparison(connection, identity, samples));
    cases
}

fn history_comparison(connection: &Connection, identity: &str, samples: usize) -> Vec<Value> {
    let (requester, project, sequence): (String, String, i64) = connection
        .query_row(
            "SELECT requester, project_id, sequence FROM items WHERE item_id =
             (SELECT item_id FROM events GROUP BY item_id ORDER BY count(*) DESC, item_id LIMIT 1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let item_id = ItemId::new(
        RequesterId::new(requester).unwrap(),
        ProjectId::new(project).unwrap(),
        u64::try_from(sequence).expect("SQLite item sequence must fit u64"),
    )
    .unwrap();
    let id = item_id.to_string();
    let repository = ItemHistoryRepository::new(connection);
    let mut request = HistoryPageRequest {
        item_id,
        ordering: HistoryOrdering::RevisionThenEventIndex,
        page: ReadPageRequest::new(10, None).unwrap(),
    };
    let mut cases = Vec::new();
    for name in ["history_first", "history_after_10"] {
        let context = CursorContext::history(identity, &request);
        let measurement = measure(
            connection,
            samples,
            || repository.select_history_page(&request).unwrap(),
            |page| {
                let mut bytes = Vec::new();
                v2_response::write_history_page(
                    &mut bytes,
                    &request.item_id,
                    page,
                    ResponseBudget::default(),
                    |event| {
                        context
                            .encode_history_key(&HistoryReadKey::from(event))
                            .map_err(|_| ReadError::Internal)
                    },
                )
                .unwrap();
                bytes
            },
        );
        let page = repository.select_history_page(&request).unwrap();
        cases.push(json!({
            "name": name, "item_id": id, "limit": 10,
            "after": request.page.after.map(|key| format!("{key:?}")),
            "returned_records": page.records.len(), "has_more": page.has_more,
            "measurement": measurement,
        }));
        request.page.after = page.records.last().map(HistoryReadKey::from);
    }
    cases
}

fn checked_output(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command {command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn source_hashes() -> String {
    // Hash the complete implementation, migration, dependency, and harness inputs.
    // Repository paths are passed as argv, never interpolated into a shell command.
    let listed = checked_output(Command::new("git").args([
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "src",
        "migrations",
        "Cargo.lock",
        "Cargo.toml",
        "tests/support",
    ]));
    let mut paths: Vec<_> = listed.lines().collect();
    paths.push("tests/v2_read_comparison.rs");
    checked_output(Command::new("shasum").args(["-a", "256"]).args(paths))
}

fn provenance() -> Value {
    json!({
        "source_commit": checked_output(Command::new("git").args(["rev-parse", "HEAD"])),
        "git_status_porcelain": checked_output(Command::new("git").args(["status", "--porcelain", "--untracked-files=normal"])),
        "source_sha256": source_hashes(),
        "rustc": checked_output(Command::new("rustc").arg("-vV")),
        "cargo": checked_output(Command::new("cargo").arg("--version")),
        "uname": checked_output(Command::new("uname").arg("-a")),
        "sqlite": rusqlite::version(),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "unix_time_seconds": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        "argv": std::env::args().collect::<Vec<_>>(),
        "requested_sizes": std::env::var("BIF_READ_COMPARISON_SIZES").ok(),
        "requested_warm_samples": std::env::var("BIF_READ_COMPARISON_SAMPLES").ok(),
    })
}

/// Same exclusive-claim/online-backup pattern as the existing index experiment.
fn snapshot(source: &Connection, path: &Path) -> Connection {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    let mut copy = Connection::open(path).unwrap();
    Backup::new(source, &mut copy)
        .unwrap()
        .run_to_completion(128, Duration::ZERO, None)
        .unwrap();
    drop(copy);
    storage::open(path).unwrap()
}

fn publish(path: &Path, value: &Value) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    serde_json::to_writer(&mut file, value).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

/// No caller-supplied store path: every input is generated inside a newly owned directory.
#[test]
#[ignore = "release measurement: generates disposable 100/10k/100k seed-2003 fixtures; prints artifact directory"]
fn publish_phase_c_read_comparison() {
    let samples: usize = std::env::var("BIF_READ_COMPARISON_SAMPLES")
        .unwrap_or_else(|_| "20".into())
        .parse()
        .unwrap();
    assert!((1..=100).contains(&samples));
    let sizes: Vec<usize> = std::env::var("BIF_READ_COMPARISON_SIZES")
        .unwrap_or_else(|_| "100,10000,100000".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    assert!(
        !sizes.is_empty()
            && sizes
                .iter()
                .all(|size| FIXTURES.iter().any(|(n, _)| n == size))
    );
    let parent = PathBuf::from("target/bif-phase-c-measurements");
    fs::create_dir_all(&parent).unwrap();
    let directory = OwnedTestDirectory::in_directory(&parent);
    println!("Phase C artifacts: {}", directory.path().display());
    let before = provenance();
    publish(&directory.path().join("provenance-start.json"), &before);
    for items in sizes {
        let path = directory.path().join(format!("{items}.sqlite3"));
        let generator_stdout = checked_output(
            Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
                .arg(items.to_string())
                .args(["--seed", "2003", "--output"])
                .arg(&path),
        );
        let metadata_bytes = fs::read(format!("{}.metadata.json", path.display())).unwrap();
        let metadata: Value = serde_json::from_slice(&metadata_bytes).unwrap();
        let digest = FIXTURES.iter().find(|(n, _)| *n == items).unwrap().1;
        assert_eq!(metadata["logical_digest"], format!("{digest:016x}"));
        for suffix in ["-wal", "-shm"] {
            assert!(
                !PathBuf::from(format!("{}{suffix}", path.display())).exists(),
                "generator must finish with a DB-only source"
            );
        }
        let source = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            benchmark_fixture::summarize(&source).unwrap().digest,
            digest
        );
        let baseline_path = directory.path().join(format!("measurement-{items}.json"));
        let baseline_stdout = checked_output(
            Command::new(env!("CARGO_BIN_EXE_bif-benchmark"))
                .arg(&path)
                .args(["--samples", &samples.to_string(), "--output"])
                .arg(&baseline_path),
        );
        let snapshot_path = directory.path().join(format!("{items}-reads.sqlite3"));
        let connection = snapshot(&source, &snapshot_path);
        assert_eq!(
            benchmark_fixture::summarize(&connection).unwrap().digest,
            digest
        );
        connection.set_prepared_statement_cache_capacity(0);
        let identity: String = connection
            .query_row("SELECT store_id FROM store_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(identity, format!("benchmark-00000000000007d3-{items}"));
        let page_size: i64 = connection
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .unwrap();
        let page_count: i64 = connection
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        let freelist_count: i64 = connection
            .pragma_query_value(None, "freelist_count", |r| r.get(0))
            .unwrap();
        let page_size = u64::try_from(page_size).expect("SQLite page size must fit u64");
        let page_count = u64::try_from(page_count).expect("SQLite page count must fit u64");
        let freelist_count =
            u64::try_from(freelist_count).expect("SQLite freelist count must fit u64");
        let cases = read_comparison(&connection, &identity, items, samples);
        assert_eq!(
            benchmark_fixture::summarize(&connection).unwrap().digest,
            digest
        );
        assert_eq!(
            benchmark_fixture::summarize(&source).unwrap().digest,
            digest
        );
        assert_eq!(
            fs::read(format!("{}.metadata.json", path.display())).unwrap(),
            metadata_bytes
        );
        assert_eq!(
            source_hashes(),
            before["source_sha256"].as_str().unwrap(),
            "source changed during measurement; rerun"
        );
        let report = json!({
            "format": "bif-v2-phase-c-read-comparison-v1",
            "provenance": before,
            "generator": metadata,
            "generator_stdout": generator_stdout,
            "baseline_stdout": baseline_stdout,
            "baseline_report": baseline_path,
            "pre_b_logical_digest_equal": true,
            "source_and_snapshot_digest_verified_after": true,
            "read_snapshot_storage": {
                "page_size": page_size, "page_count": page_count, "freelist_count": freelist_count,
                "allocated_page_bytes": page_size * page_count,
                "occupied_page_bytes": page_size * (page_count - freelist_count),
            },
            "procedure": {
                "warm_samples": samples,
                "percentiles": "nearest rank; raw samples retain execution order",
                "cache": "no OS eviction; first read per case plus consecutive warm reads; traversal/equivalence checks warm caches",
                "prepared_statement_cache": "disabled",
                "read_scope": "production storage adapter, assembly and trace callbacks; excludes startup, backup, authorization, cursor decode and serialization",
                "serialization_scope": "v2 typed full success envelope/newline and real cursor encoding; v1 dynamic result JSON without transport envelope",
                "sqlite_work_scope": "all traced statements, including transaction statements; callbacks are returned rows, not scanned rows",
                "filesystem_cold": "unavailable: not performed",
                "whole_read_allocations": "unavailable: no allocation instrumentation in this test",
                "analyze": "not run: use generator's normal production migration/index state",
            },
            "cases": cases,
        });
        let report_path = directory
            .path()
            .join(format!("read-comparison-{items}.json"));
        publish(&report_path, &report);
        println!("Published {}", report_path.display());
    }
    assert_eq!(source_hashes(), before["source_sha256"].as_str().unwrap());
    publish(&directory.path().join("provenance-end.json"), &provenance());
}

#[test]
fn percentiles_use_nearest_rank_without_reordering_raw_samples() {
    let observed = distribution(vec![9, 1, 7, 3, 5]);
    assert_eq!(observed.p50, 5);
    assert_eq!(observed.p95, 9);
    assert_eq!(observed.samples, [9, 1, 7, 3, 5]);
}

#[test]
fn original_fixture_digests_are_pinned_to_published_pre_b_artifacts() {
    for (items, digest) in FIXTURES {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "docs/baselines/v2-pre-phase-b/generator-{items}.json"
        ));
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(metadata["seed"], 2003);
        assert_eq!(metadata["items"], items);
        assert_eq!(metadata["logical_digest"], format!("{digest:016x}"));
    }
}
