//! Developer-only, machine-readable measurements of the current storage path.
use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Clock, Command, Execution, HumanAuthorization,
        ItemHistoryStore, ItemListFilters, ItemStore, MutationIdentity, MutationIdentityGenerator,
        MutationUseCaseError, ObservedExecution, PageOffset, PageSize, Pagination, list_item_page,
        mutate_item,
    },
    benchmark_fixture,
    domain::{
        ItemId, ItemMutation, NamedView, ProjectId, RequesterId, Revision, Timestamp, Triage,
        TriageField,
    },
    rpc, rpc_read,
    storage::{self, ItemHistoryRepository, ItemRepository, MutationRepository},
};
use rusqlite::{
    Connection, OpenFlags, StatementStatus,
    trace::{TraceEvent, TraceEventCodes},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process,
    sync::{LazyLock, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const MAX_SAMPLES: usize = 1_000;
static TRACE: LazyLock<Mutex<Option<Trace>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Default, Clone, Serialize)]
struct Work {
    fullscan_steps: u64,
    sorts: u64,
    autoindex_rows: u64,
    vm_steps: u64,
    reprepares: u64,
    runs: u64,
    filter_misses: u64,
    filter_hits: u64,
}
#[derive(Default, Clone, Serialize)]
struct Trace {
    lexical_data_statements: u64,
    sqlite_row_callbacks: u64,
    profile_nanoseconds: u64,
    sqlite_work: Work,
}
#[derive(Serialize)]
struct Distribution {
    sample_count: usize,
    p50: u128,
    p95: u128,
    samples: Vec<u128>,
}
#[derive(Serialize)]
struct Availability<T: Serialize> {
    status: &'static str,
    value: Option<T>,
    reason: Option<&'static str>,
}
#[derive(Serialize)]
struct Operation {
    name: &'static str,
    phase: &'static str,
    sql_metrics_scope: &'static str,
    matches_per_sample: Vec<usize>,
    lexical_data_statements_per_sample: Vec<u64>,
    sql_metrics_per_sample: Vec<Trace>,
    sql_totals: Trace,
    wall_nanoseconds: Distribution,
    sqlite_profile_nanoseconds: Distribution,
    non_profiled_wall_residual_nanoseconds: Distribution,
    profile_exceeded_wall_samples: Vec<usize>,
    profile_zero_samples: Vec<usize>,
    serialization_nanoseconds: Distribution,
    response_bytes: Distribution,
    serialization_shape: &'static str,
}
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum FileSize {
    Present { bytes: u64 },
    Missing,
}
#[derive(Serialize)]
struct Sizes {
    database: FileSize,
    wal: FileSize,
}
#[derive(Serialize)]
struct Verification {
    revision: u64,
    expected_revision: u64,
    benchmark_note_events: usize,
    expected_benchmark_note_events: usize,
    integrity_check: String,
    foreign_key_violations: usize,
}
#[derive(Serialize)]
struct Provenance {
    revision: String,
    dirty: bool,
    rustc: String,
    target: String,
    sqlite_version: String,
    profile: &'static str,
    unix_time_seconds: u64,
    host_os: String,
    exact_command: Vec<String>,
    cache_procedure: &'static str,
    fixture_bytes: u64,
    fixture_seed: Option<Value>,
    fixture_digest: Option<Value>,
}
#[derive(Deserialize)]
struct FixtureMetadata {
    format: String,
    logical_digest_algorithm: String,
    seed: u64,
    items: usize,
    database: String,
    logical_digest: String,
    row_counts: BTreeMap<String, u64>,
    distributions: BTreeMap<String, BTreeMap<String, u64>>,
    integrity_check: String,
}
#[derive(Serialize)]
struct Report {
    format: &'static str,
    provenance: Provenance,
    source_database: String,
    measured_store: String,
    measured_store_sizes_before_writes: Sizes,
    measured_store_sizes_after_writes: Sizes,
    durability: BTreeMap<&'static str, String>,
    percentile_definition: &'static str,
    startup_open: Distribution,
    persistence_verification: Verification,
    filesystem_cold: Availability<Distribution>,
    operations: Vec<Operation>,
}

fn add(target: &mut u64, value: u64, what: &str) {
    *target = target
        .checked_add(value)
        .unwrap_or_else(|| panic!("{what} counter overflow"));
}
fn trace(event: TraceEvent<'_>) {
    let mut guard = TRACE.lock().expect("trace mutex poisoned");
    let Some(metrics) = guard.as_mut() else {
        return;
    };
    match event {
        TraceEvent::Stmt(statement, _) => {
            let first = statement
                .sql()
                .trim_start()
                .split_ascii_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            if matches!(
                first.as_str(),
                "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "WITH" | "REPLACE"
            ) {
                add(&mut metrics.lexical_data_statements, 1, "statement")
            }
        }
        TraceEvent::Row(_) => add(&mut metrics.sqlite_row_callbacks, 1, "row callback"),
        TraceEvent::Profile(statement, elapsed) => {
            add(
                &mut metrics.profile_nanoseconds,
                u64::try_from(elapsed.as_nanos()).expect("PROFILE duration overflow"),
                "PROFILE",
            );
            macro_rules! s {
                ($f:ident,$k:expr) => {
                    add(
                        &mut metrics.sqlite_work.$f,
                        u64::try_from(statement.get_status($k)).expect("negative SQLite status"),
                        stringify!($f),
                    )
                };
            }
            // The measured connection disables the statement cache, avoiding
            // cumulative counters from prepared-statement reuse.
            s!(fullscan_steps, StatementStatus::FullscanStep);
            s!(sorts, StatementStatus::Sort);
            s!(autoindex_rows, StatementStatus::AutoIndex);
            s!(vm_steps, StatementStatus::VmStep);
            s!(reprepares, StatementStatus::RePrepare);
            s!(runs, StatementStatus::Run);
            s!(filter_misses, StatementStatus::FilterMiss);
            s!(filter_hits, StatementStatus::FilterHit);
        }
        _ => {}
    }
}

struct Temp {
    directory: PathBuf,
    database: PathBuf,
}
impl Temp {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        for nonce in 0..100u32 {
            let directory = env::temp_dir().join(format!(
                "bif-measurement-{}-{}-{nonce}",
                process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let database = directory.join("store.sqlite3");
                    return Ok(Self {
                        directory,
                        database,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err("unable to atomically allocate temporary directory".into())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn snapshot(source: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.backup("main", destination, None)?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("benchmark failed: {e}");
        process::exit(1)
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let command = env::args().collect::<Vec<_>>();
    let mut args = command.iter().skip(1);
    let source = PathBuf::from(
        args.next()
            .ok_or("usage: bif-benchmark DATABASE [--samples N] [--output PATH]")?,
    );
    let mut samples = 20usize;
    let mut output = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--samples" => samples = args.next().ok_or("--samples requires N")?.parse()?,
            "--output" => {
                output = Some(PathBuf::from(args.next().ok_or("--output requires PATH")?))
            }
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    if !(1..=MAX_SAMPLES).contains(&samples) {
        return Err(format!("--samples must be between 1 and {MAX_SAMPLES}").into());
    }
    let fixture_bytes = fs::metadata(&source)?.len();
    let metadata = fixture_metadata(&source, fixture_bytes)?;

    let mut startup = Vec::with_capacity(samples);
    for _ in 0..samples {
        let temp = Temp::new()?;
        snapshot(&source, &temp.database)?;
        let now = Instant::now();
        drop(storage::open(&temp.database)?);
        startup.push(now.elapsed().as_nanos());
    }
    let temp = Temp::new()?;
    snapshot(&source, &temp.database)?;
    if let Some(metadata) = metadata.as_ref() {
        let expected = metadata["logical_digest"]
            .as_str()
            .ok_or("fixture metadata digest is not a string")?;
        let snapshot_connection =
            Connection::open_with_flags(&temp.database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let actual = benchmark_fixture::summarize(&snapshot_connection)?.digest;
        if expected != format!("{actual:016x}") {
            return Err("SQLite backup snapshot does not match fixture logical digest".into());
        }
    }
    let mut connection = storage::open(&temp.database)?;
    connection.set_prepared_statement_cache_capacity(0);
    let item_id = first_item(&connection)?;
    let initial_revision = revision(&connection, &item_id)?;
    let initial_notes = note_count(&connection, &item_id)?;
    let requester = RequesterId::new("BENCH")?;
    let list_candidates: usize = connection
        .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))?
        .try_into()?;
    let read_authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "benchmark",
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
    };
    let durability = durability(&connection)?;
    let before = sizes(&temp.database)?;
    connection.trace_v2(
        TraceEventCodes::SQLITE_TRACE_STMT
            | TraceEventCodes::SQLITE_TRACE_PROFILE
            | TraceEventCodes::SQLITE_TRACE_ROW,
        Some(trace),
    );
    let mut operations = Vec::new();
    operations.push(measure(samples, "get", || {
        ItemRepository::new(&connection)
            .read_item(&item_id)
            .map(|item| {
                (
                    usize::from(item.is_some()),
                    item.map_or(Value::Null, |v| rpc_read::get_result_json(&v)),
                )
            })
    })?);
    operations.push(measure(samples, "history", || {
        ItemHistoryRepository::new(&connection)
            .item_history(&item_id)
            .map(|events| (events.len(), rpc_read::history_result_json(&events)))
            .map_err(|e| match e {
                bif::application::ItemHistoryStoreError::Storage(e)
                | bif::application::ItemHistoryStoreError::InvalidPersistedData(e) => e,
                bif::application::ItemHistoryStoreError::NotFound => {
                    panic!("fixture item disappeared")
                }
            })
    })?);
    let list = measure(samples, "list_all", || {
        list_item_page(
            &ItemRepository::new(&connection),
            &read_authorization,
            NamedView::All,
            &requester,
            &ItemListFilters::default(),
            Pagination::new(PageSize::new(100).expect("v1 default"), PageOffset::new(0)),
        )
        .map(|page| (page.items.len(), rpc_read::list_result_json(&page)))
    })?;
    for (&_matches, &actual) in list
        .matches_per_sample
        .iter()
        .zip(&list.lexical_data_statements_per_sample)
    {
        let expected = 1usize
            .checked_add(
                2usize
                    .checked_mul(list_candidates)
                    .ok_or("list count overflow")?,
            )
            .ok_or("list count overflow")?;
        if u64::try_from(expected)? != actual {
            return Err(format!(
                "list sample statement count {actual}, expected 1 + 2M = {expected}"
            )
            .into());
        }
    }
    operations.push(list);
    operations.push(measure_mutation(samples, &mut connection, &item_id)?);
    connection.trace_v2(TraceEventCodes::empty(), None);
    drop(connection);
    let after = sizes(&temp.database)?;
    let connection = storage::open(&temp.database)?;
    let item = ItemRepository::new(&connection)
        .read_item(&item_id)?
        .ok_or("item missing after reopen")?;
    let events = ItemHistoryRepository::new(&connection)
        .item_history(&item_id)
        .map_err(|e| format!("history read after reopen failed: {e:?}"))?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let foreign_keys = connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .mapped(|_| Ok(()))
        .count();
    let verification = Verification {
        revision: item.revision().get(),
        expected_revision: initial_revision
            .checked_add(u64::try_from(samples)?)
            .ok_or("revision overflow")?,
        benchmark_note_events: events
            .iter()
            .filter(|e| e.note.as_deref() == Some("developer benchmark write"))
            .count(),
        expected_benchmark_note_events: initial_notes
            .checked_add(samples)
            .ok_or("note count overflow")?,
        integrity_check: integrity,
        foreign_key_violations: foreign_keys,
    };
    if verification.revision != verification.expected_revision
        || verification.benchmark_note_events != verification.expected_benchmark_note_events
        || verification.integrity_check != "ok"
        || verification.foreign_key_violations != 0
    {
        return Err("persistence verification failed".into());
    }
    let report = Report {
        format: "bif-v2-measurement-v2",
        provenance: provenance(command, fixture_bytes, metadata)?,
        source_database: source.display().to_string(),
        measured_store: temp.database.display().to_string(),
        measured_store_sizes_before_writes: before,
        measured_store_sizes_after_writes: after,
        durability,
        percentile_definition: "nearest rank; overflow-safe quotient/remainder calculation",
        startup_open: distribution(startup)?,
        persistence_verification: verification,
        filesystem_cold: Availability {
            status: "unavailable",
            value: None,
            reason: Some("OS cache not evicted; procedure recorded in provenance"),
        },
        operations,
    };
    let json = serde_json::to_vec_pretty(&report)?;
    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?
        }
        fs::write(path, json)?
    } else {
        println!("{}", String::from_utf8(json)?)
    }
    Ok(())
}

fn first_item(c: &Connection) -> Result<ItemId, Box<dyn std::error::Error>> {
    let value: String = c.query_row(
        "SELECT item_id FROM items ORDER BY item_id LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    let mut p = value.split(':');
    Ok(ItemId::new(
        RequesterId::new(p.next().ok_or("bad item id")?)?,
        ProjectId::new(p.next().ok_or("bad item id")?)?,
        p.next().ok_or("bad item id")?.parse()?,
    )?)
}
fn revision(c: &Connection, id: &ItemId) -> rusqlite::Result<u64> {
    let value: i64 = c.query_row(
        "SELECT revision FROM items WHERE item_id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    u64::try_from(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}
fn note_count(c: &Connection, id: &ItemId) -> rusqlite::Result<usize> {
    let value: i64 = c.query_row(
        "SELECT count(*) FROM events WHERE item_id=?1 AND note=?2",
        (id.to_string(), "developer benchmark write"),
        |r| r.get(0),
    )?;
    usize::try_from(value).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}
fn durability(c: &Connection) -> rusqlite::Result<BTreeMap<&'static str, String>> {
    let mut out = BTreeMap::new();
    let journal: String = c.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    out.insert("journal_mode", journal);
    let foreign_keys: i64 = c.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    out.insert(
        "foreign_keys",
        if foreign_keys == 1 { "ON" } else { "OFF" }.into(),
    );
    let synchronous: i64 = c.pragma_query_value(None, "synchronous", |r| r.get(0))?;
    out.insert(
        "synchronous",
        if synchronous == 2 { "FULL" } else { "OTHER" }.into(),
    );
    Ok(out)
}
fn size(path: &Path) -> Result<FileSize, std::io::Error> {
    match fs::metadata(path) {
        Ok(m) => Ok(FileSize::Present { bytes: m.len() }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileSize::Missing),
        Err(e) => Err(e),
    }
}
fn sizes(path: &Path) -> Result<Sizes, std::io::Error> {
    Ok(Sizes {
        database: size(path)?,
        wal: size(Path::new(&format!("{}-wal", path.display())))?,
    })
}

struct FixedClock;
impl Clock for FixedClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new("2025-02-01T00:00:00.000Z")
    }
}
struct Identities(u64);
impl MutationIdentityGenerator for Identities {
    fn mutation_identity(&mut self) -> MutationIdentity {
        self.0 = self.0.checked_add(1).expect("identity overflow");
        MutationIdentity {
            operation_id: format!("benchmark-operation-{}", self.0),
            event_ids: vec![format!("benchmark-event-{}", self.0)],
        }
    }
}
fn measure_mutation(
    samples: usize,
    c: &mut Connection,
    id: &ItemId,
) -> Result<Operation, Box<dyn std::error::Error>> {
    static CHANGES: &[&str] = &["note"];
    let authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: "benchmark",
            surface: "cli",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "cli",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: Command::Mutation {
            requested_changes: CHANGES,
        },
        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
    };
    let mut rev = revision(c, id)?;
    let mut clock = FixedClock;
    let mut identities = Identities(0);
    measure(samples, "mutation_note", || {
        let item = mutate_item(
            &mut MutationRepository::new(c),
            &mut clock,
            &mut identities,
            &authorization,
            id,
            Revision::new(rev).expect("positive revision"),
            ItemMutation {
                lifecycle: None,
                triage: Some(Triage {
                    priority: TriageField::Omitted,
                    assignee: TriageField::Omitted,
                    note: Some("developer benchmark write".into()),
                }),
            },
        )?;
        rev = item.revision().get();
        Ok::<_, MutationUseCaseError<rusqlite::Error>>((1, rpc::item_json(&item)))
    })
    .map_err(|e| Box::new(e) as _)
}
fn measure<E, F: FnMut() -> Result<(usize, Value), E>>(
    samples: usize,
    name: &'static str,
    mut operation: F,
) -> Result<Operation, E> {
    let (
        mut walls,
        mut profiles,
        mut residuals,
        mut inconsistent,
        mut profile_zero,
        mut serializations,
        mut bytes,
        mut matches,
        mut statements,
        mut sql_samples,
    ) = (
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
    );
    let mut totals = Trace::default();
    for sample in 0..samples {
        *TRACE.lock().unwrap() = Some(Trace::default());
        let now = Instant::now();
        let (count, response) = operation()?;
        let wall = now.elapsed().as_nanos();
        let trace = TRACE.lock().unwrap().take().unwrap();
        let now = Instant::now();
        let encoded = serde_json::to_vec(&response).expect("Value serialization");
        let serialization = now.elapsed().as_nanos();
        let profile = u128::from(trace.profile_nanoseconds);
        if profile > wall {
            inconsistent.push(sample)
        }
        if profile == 0 {
            profile_zero.push(sample)
        }
        matches.push(count);
        statements.push(trace.lexical_data_statements);
        sql_samples.push(trace.clone());
        walls.push(wall);
        profiles.push(profile);
        residuals.push(wall.saturating_sub(profile));
        serializations.push(serialization);
        bytes.push(encoded.len() as u128);
        add_trace(&mut totals, &trace);
    }
    Ok(Operation {
        name,
        phase: "warm_connection_and_os_cache",
        sql_metrics_scope: "totals plus complete per-sample trace and StatementStatus counters",
        matches_per_sample: matches,
        lexical_data_statements_per_sample: statements,
        sql_metrics_per_sample: sql_samples,
        sql_totals: totals,
        wall_nanoseconds: distribution(walls).unwrap(),
        sqlite_profile_nanoseconds: distribution(profiles).unwrap(),
        non_profiled_wall_residual_nanoseconds: distribution(residuals).unwrap(),
        profile_exceeded_wall_samples: inconsistent,
        profile_zero_samples: profile_zero,
        serialization_nanoseconds: distribution(serializations).unwrap(),
        response_bytes: distribution(bytes).unwrap(),
        serialization_shape: "stable current v1 CLI/RPC result JSON (without RPC transport envelope)",
    })
}
fn add_trace(t: &mut Trace, v: &Trace) {
    add(
        &mut t.lexical_data_statements,
        v.lexical_data_statements,
        "statement",
    );
    add(&mut t.sqlite_row_callbacks, v.sqlite_row_callbacks, "row");
    add(&mut t.profile_nanoseconds, v.profile_nanoseconds, "PROFILE");
    macro_rules! a {
        ($f:ident) => {
            add(&mut t.sqlite_work.$f, v.sqlite_work.$f, stringify!($f))
        };
    }
    a!(fullscan_steps);
    a!(sorts);
    a!(autoindex_rows);
    a!(vm_steps);
    a!(reprepares);
    a!(runs);
    a!(filter_misses);
    a!(filter_hits);
}
fn distribution(mut values: Vec<u128>) -> Result<Distribution, &'static str> {
    if values.is_empty() {
        return Err("empty distribution");
    }
    let raw = values.clone();
    values.sort_unstable();
    let at = |p: usize| {
        let rank = (values.len() / 100)
            .checked_mul(p)
            .and_then(|v| v.checked_add(((values.len() % 100) * p).div_ceil(100)))
            .expect("bounded percentile");
        values[rank.saturating_sub(1)]
    };
    Ok(Distribution {
        sample_count: values.len(),
        p50: at(50),
        p95: at(95),
        samples: raw,
    })
}
fn output(
    command: &mut process::Command,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let out = command.output()?;
    if !out.status.success() {
        return Err(format!(
            "{name} failed with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().into())
}
fn provenance(
    command: Vec<String>,
    bytes: u64,
    metadata: Option<Value>,
) -> Result<Provenance, Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let revision = output(
        process::Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "--verify", "HEAD"]),
        "git revision",
    )?;
    let dirty = !output(
        process::Command::new("git").current_dir(root).args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ]),
        "git status",
    )?
    .is_empty();
    let rustc = output(process::Command::new("rustc").arg("--version"), "rustc")?;
    let vv = output(process::Command::new("rustc").arg("-vV"), "rustc target")?;
    let target = vv
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or("missing rustc host")?
        .into();
    let host_os = output(process::Command::new("uname").arg("-a"), "uname")
        .unwrap_or_else(|_| format!("{} {}", env::consts::OS, env::consts::ARCH));
    Ok(Provenance {
        revision,
        dirty,
        rustc,
        target,
        sqlite_version: rusqlite::version().into(),
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        unix_time_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        host_os,
        exact_command: command,
        cache_procedure: "none; no OS cache eviction attempted",
        fixture_bytes: bytes,
        fixture_seed: metadata.as_ref().and_then(|v| v.get("seed")).cloned(),
        fixture_digest: metadata
            .as_ref()
            .and_then(|v| v.get("logical_digest"))
            .cloned(),
    })
}
fn fixture_metadata(
    database: &Path,
    fixture_bytes: u64,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    match fs::read(format!("{}.metadata.json", database.display())) {
        Ok(v) => {
            let value: Value = serde_json::from_slice(&v)?;
            let metadata: FixtureMetadata = serde_json::from_value(value.clone())?;
            if metadata.format != benchmark_fixture::FORMAT
                || metadata.logical_digest_algorithm != benchmark_fixture::DIGEST_ALGORITHM
                || metadata.database != database.display().to_string()
                || metadata.integrity_check != "ok"
            {
                return Err(
                    "fixture metadata has unsupported or mismatched identity fields".into(),
                );
            }
            let source = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let summary = benchmark_fixture::summarize(&source)?;
            let item_count = usize::try_from(*summary.row_counts.get("items").unwrap_or(&0))?;
            if metadata.items != item_count
                || metadata.logical_digest != format!("{:016x}", summary.digest)
                || metadata.row_counts != summary.row_counts
                || metadata.distributions != summary.distributions
                || fs::metadata(database)?.len() != fixture_bytes
            {
                return Err(
                    "fixture metadata is stale or does not describe the source database".into(),
                );
            }
            let _ = metadata.seed;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
