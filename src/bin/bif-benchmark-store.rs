//! Deterministic developer fixture generator for the v2 read benchmarks.
//!
//! This deliberately writes through a fixture-only relational builder: doing
//! 100,000 separately durable CLI mutations would make regeneration needlessly
//! slow.  Every generated history follows the same lifecycle transitions as the
//! domain, and the finished store is checked through the production loaders.

use std::{
    collections::BTreeMap,
    env, fs,
    fs::{File, OpenOptions},
    io::{Seek, Write},
    path::{Path, PathBuf},
    process,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use bif::{
    application::{ItemHistoryStore, ItemStore},
    benchmark_fixture,
    domain::{
        EventType, EventValue, Item, ItemId, ItemMutation, LifecycleMutation, ProjectId,
        RequesterId, Triage, TriageField,
    },
    storage::{self, ItemHistoryRepository, ItemRepository},
};
use rusqlite::{Connection, params};
use same_file::Handle;
use serde::Serialize;
use tempfile::Builder;

const SIZES: [usize; 3] = [100, 10_000, 100_000];
const PROJECTS: [(&str, usize); 5] = [
    ("core", 68),
    ("agent-tools", 18),
    ("desktop", 9),
    ("docs", 4),
    ("rare-project", 1),
];
const REQUESTERS: [&str; 3] = ["BENCH", "DELTA", "CODEX"];

#[derive(Serialize)]
struct Metadata {
    format: &'static str,
    logical_digest_algorithm: &'static str,
    seed: u64,
    items: usize,
    database: String,
    logical_digest: String,
    elapsed_milliseconds: u128,
    row_counts: BTreeMap<String, u64>,
    distributions: BTreeMap<String, BTreeMap<String, u64>>,
    samples_verified: Vec<String>,
    integrity_check: String,
}

#[derive(Clone)]
struct Event {
    kind: &'static str,
    before: Option<String>,
    after: Option<String>,
    reason: Option<&'static str>,
    note: Option<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("benchmark fixture generation failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let size = args
        .next()
        .ok_or("usage: bif-benchmark-store SIZE [--seed N] [--output PATH]")?
        .parse::<usize>()?;
    if !SIZES.contains(&size) {
        return Err(format!("SIZE must be exactly 100, 10000, or 100000 (got {size})").into());
    }
    let mut seed = 2_003_u64;
    let mut output = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--seed" => seed = args.next().ok_or("--seed requires a value")?.parse()?,
            "--output" => {
                output = Some(PathBuf::from(
                    args.next().ok_or("--output requires a path")?,
                ))
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }

    let (database, mut owned_parent) = match output {
        Some(path) => (path, None),
        None => {
            let path = disposable_path(size, seed)?;
            let parent = path
                .parent()
                .expect("disposable database always has a parent")
                .to_path_buf();
            (path, Some(OwnedDirectory::new(parent)))
        }
    };
    if let Some(parent) = database.parent() {
        fs::create_dir_all(parent)?;
    }
    let metadata_path = PathBuf::from(format!("{}.metadata.json", database.display()));
    // Claim both public names before doing expensive work. SQLite is never
    // given either public name: its database and sidecars live under a private,
    // invocation-owned staging directory.
    let database_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&database)
        .map_err(|error| {
            format!(
                "could not atomically claim output {}: {error}",
                database.display()
            )
        })?;
    let mut owned_database = OwnedOutput::new(database.clone(), database_file);
    let metadata_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&metadata_path)
        .map_err(|error| {
            format!(
                "could not atomically claim metadata sidecar {}: {error}",
                metadata_path.display()
            )
        })?;
    let mut owned_metadata = OwnedOutput::new(metadata_path, metadata_file);

    let parent = database.parent().unwrap_or_else(|| Path::new("."));
    let staging = Builder::new()
        .prefix(".bif-benchmark-stage-")
        .tempdir_in(parent)?;
    let staged_database = staging.path().join("store.sqlite3");
    if env::var_os("BIF_BENCHMARK_STORE_TEST_FAIL_AFTER_CLAIMS").is_some() {
        return Err("injected failure after output claims".into());
    }
    wait_at_test_claim_boundary()?;

    let started = Instant::now();
    let mut connection = storage::open(&staged_database)?;
    generate(&mut connection, size, seed)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let summary = benchmark_fixture::summarize(&connection)?;
    verify_replays(&connection, size.min(100))?;
    let samples_verified = verify_samples(&connection, size)?;
    let integrity_check: String =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity_check != "ok" {
        return Err(format!("SQLite integrity_check returned {integrity_check}").into());
    }
    let foreign_key_violations: i64 =
        connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if foreign_key_violations != 0 {
        return Err(format!("foreign_key_check found {foreign_key_violations} violations").into());
    }
    drop(connection);

    let metadata = Metadata {
        format: benchmark_fixture::FORMAT,
        logical_digest_algorithm: benchmark_fixture::DIGEST_ALGORITHM,
        seed,
        items: size,
        database: database.display().to_string(),
        logical_digest: format!("{:016x}", summary.digest),
        elapsed_milliseconds: started.elapsed().as_millis(),
        row_counts: summary.row_counts,
        distributions: summary.distributions,
        samples_verified,
        integrity_check,
    };
    // Publish through the already atomically claimed file handles. No path is
    // reopened, so a third party replacing a public pathname cannot redirect
    // these writes. The staged connection is closed and checkpointed first.
    publish_file(&staged_database, owned_database.file_mut())?;
    owned_metadata
        .file_mut()
        .write_all(&serde_json::to_vec_pretty(&metadata)?)?;
    owned_metadata.file_mut().sync_all()?;
    owned_database.verify_path_ownership()?;
    owned_metadata.verify_path_ownership()?;
    if let Some(directory) = &mut owned_parent {
        directory.keep();
    }
    println!("{}", serde_json::to_string(&metadata)?);
    Ok(())
}

struct OwnedDirectory {
    path: PathBuf,
    keep: bool,
}

impl OwnedDirectory {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }

    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct OwnedOutput {
    path: PathBuf,
    file: File,
}

impl OwnedOutput {
    fn new(path: PathBuf, file: File) -> Self {
        Self { path, file }
    }

    fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    fn verify_path_ownership(&self) -> Result<(), Box<dyn std::error::Error>> {
        let claimed = Handle::from_file(self.file.try_clone()?)?;
        let current = Handle::from_path(&self.path).map_err(|error| {
            format!(
                "claimed output pathname {} is no longer available: {error}",
                self.path.display()
            )
        })?;
        if claimed != current {
            return Err(format!(
                "claimed output pathname {} was replaced during generation",
                self.path.display()
            )
            .into());
        }
        Ok(())
    }
}

fn wait_at_test_claim_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let Some(marker) = env::var_os("BIF_BENCHMARK_STORE_TEST_CLAIM_MARKER") else {
        return Ok(());
    };
    let marker = PathBuf::from(marker);
    fs::write(&marker, b"claimed")?;
    while marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}

fn publish_file(source: &Path, destination: &mut File) -> Result<(), std::io::Error> {
    let mut source = File::open(source)?;
    destination.rewind()?;
    std::io::copy(&mut source, destination)?;
    destination.sync_all()
}

fn disposable_path(size: usize, seed: u64) -> Result<PathBuf, std::io::Error> {
    let root = env::temp_dir();
    for attempt in 0..1_000 {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = root.join(format!(
            "bif-benchmark-{}-{size}-{seed}-{nonce}-{attempt}",
            process::id()
        ));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory.join("store.sqlite3")),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate disposable benchmark directory",
    ))
}

fn generate(connection: &mut Connection, size: usize, seed: u64) -> rusqlite::Result<()> {
    // The schema's random store identity and migration times are physical setup
    // metadata, not fixture contents. Pin store metadata for easier inspection.
    connection.execute(
        "UPDATE store_metadata SET store_id = ?, created_at = ? WHERE singleton = 1",
        params![
            format!("benchmark-{seed:016x}-{size}"),
            "2025-01-01T00:00:00.000Z"
        ],
    )?;
    let transaction = connection.transaction()?;
    for (project, _) in PROJECTS {
        transaction.execute(
            "INSERT INTO projects(project_id, created_at) VALUES (?, ?)",
            params![project, "2025-01-01T00:00:00.000Z"],
        )?;
    }

    let mut sequences = BTreeMap::<(&str, &str), u64>::new();
    for index in 0..size {
        let random = mix(seed ^ index as u64);
        let project = if index < PROJECTS.len() {
            PROJECTS[index].0
        } else {
            weighted_project((random % 100) as usize)
        };
        let requester = REQUESTERS[((random >> 8) % REQUESTERS.len() as u64) as usize];
        let sequence = sequences.entry((requester, project)).or_default();
        *sequence += 1;
        insert_item(
            &transaction,
            size,
            index,
            random,
            requester,
            project,
            *sequence,
        )?;
    }
    for ((requester, project), sequence) in sequences {
        transaction.execute(
            "INSERT INTO requester_project_counters(requester, project_id, next_sequence)
             VALUES (?, ?, ?)",
            params![requester, project, (sequence + 1) as i64],
        )?;
    }
    transaction.commit()
}

fn insert_item(
    connection: &Connection,
    size: usize,
    index: usize,
    random: u64,
    requester: &str,
    project: &str,
    sequence: u64,
) -> rusqlite::Result<()> {
    let item_id = format!("{requester}:{project}:{sequence:03}");
    let sparse = index % 997 == 0;
    let long = index % 211 == 0;
    let title = if sparse {
        format!("Needle-filter benchmark item {index}")
    } else {
        format!("Improve {project} workflow component {index}")
    };
    let description = if long {
        Some(format!(
            "A realistic investigation of user-visible behavior, compatibility, and recovery. {}",
            "This paragraph captures constraints, alternatives, diagnostics, and validation evidence. ".repeat(24)
        ))
    } else if index % 7 == 0 {
        None
    } else {
        Some(format!(
            "Investigate scenario {} and preserve supported behavior across local developer workflows.",
            random % 10_000
        ))
    };
    let desired = if index < 6 {
        [
            "proposed",
            "ready",
            "in_progress",
            "blocked",
            "done",
            "rejected",
        ][index]
    } else {
        match (random >> 16) % 100 {
            0..=17 => "proposed",
            18..=37 => "ready",
            38..=57 => "in_progress",
            58..=67 => "blocked",
            68..=91 => "done",
            _ => "rejected",
        }
    };
    let priority = if index < 6 {
        [
            Some("P0"),
            Some("P1"),
            Some("P2"),
            Some("P3"),
            Some("P4"),
            None,
        ][index]
    } else {
        match (random >> 24) % 10 {
            0 => Some("P0"),
            1 => Some("P1"),
            2..=3 => Some("P2"),
            4..=5 => Some("P3"),
            6..=7 => Some("P4"),
            _ => None,
        }
    };
    let assignee = if index == 0 {
        None
    } else if index == 1 {
        Some("alice")
    } else if (random >> 32).is_multiple_of(4) {
        None
    } else {
        Some(["alice", "bob", "benchmark-agent"][((random >> 36) % 3) as usize])
    };
    let bucket = index / 8; // Deliberately equal timestamps within each bucket.
    let captured_at = timestamp(bucket);
    let mut events = vec![Event {
        kind: "captured",
        before: None,
        after: None,
        reason: None,
        note: None,
    }];
    if let Some(priority) = priority {
        events.push(Event {
            kind: "priority_changed",
            before: Some("null".to_owned()),
            after: Some(priority.to_owned()),
            reason: None,
            note: None,
        });
    }
    if let Some(assignee) = assignee {
        events.push(Event {
            kind: "assignee_changed",
            before: Some("null".to_owned()),
            after: Some(assignee.to_owned()),
            reason: None,
            note: None,
        });
    }
    events.extend(lifecycle(desired));
    if index % 13 == 0 {
        events.push(Event {
            kind: "note_added",
            before: None,
            after: None,
            reason: None,
            note: Some("Captured benchmark investigation evidence and next steps.".into()),
        });
    }
    // A stable hot set receives substantial valid note-only mutation bursts.
    if index < (size / 1_000).max(1) {
        for burst in 0..24 {
            events.push(Event {
                kind: "note_added",
                before: None,
                after: None,
                reason: None,
                note: Some(format!("Hot-item mutation burst {burst:02}")),
            });
        }
    }
    let revision = events.len() as u64;
    let updated_at = timestamp(bucket + revision as usize / 3);
    let status_reason = match desired {
        "blocked" => Some("Waiting for a reproducible upstream result"),
        "rejected" => Some("Superseded by the supported workflow"),
        _ => None,
    };
    connection.execute(
        "INSERT INTO items(item_id, requester, project_id, sequence, title, description,
             status, priority, assignee, status_reason, revision, captured_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            item_id,
            requester,
            project,
            sequence as i64,
            title,
            description,
            desired,
            priority,
            assignee,
            status_reason,
            revision as i64,
            captured_at,
            updated_at
        ],
    )?;

    let criteria_count = if index % 503 == 0 {
        36
    } else {
        ((random >> 40) % 7) as usize
    };
    for criterion in 0..criteria_count {
        connection.execute(
            "INSERT INTO item_acceptance_criteria(item_id, criterion_index, criterion)
             VALUES (?, ?, ?)",
            params![
                item_id,
                criterion as i64,
                format!(
                    "Criterion {} verifies deterministic behavior for scenario {}{}",
                    criterion + 1,
                    random % 4096,
                    if sparse { " with sparse-marker" } else { "" }
                )
            ],
        )?;
    }
    connection.execute(
        "INSERT INTO item_provenance(item_id, source_host, thread_id, message_id, url,
             repository_reference, revision_reference, context_excerpt)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            item_id,
            ["delta", "codex", "local"][((random >> 48) % 3) as usize],
            format!("benchmark-thread-{}", index / 20),
            format!("message-{index}"),
            if index % 5 == 0 {
                Some(format!("https://example.invalid/benchmark/{index}"))
            } else {
                None
            },
            "bif",
            format!("{:016x}", mix(random)),
            if sparse { Some("sparse-marker") } else { None }
        ],
    )?;
    for (event_index, event) in events.iter().enumerate() {
        let revision = event_index + 1;
        let operation_id = format!("benchmark-op-{index:06}-{revision:03}");
        let event_id = format!("benchmark-event-{index:06}-{revision:03}");
        let operation_type = operation_type(event.kind);
        let occurred_at = timestamp(bucket + revision / 3);
        connection.execute(
            "INSERT INTO operations(operation_id, item_id, operation_type, expected_revision,
                 item_revision, occurred_at) VALUES (?, ?, ?, ?, ?, ?)",
            params![
                operation_id,
                item_id,
                operation_type,
                if revision == 1 {
                    None
                } else {
                    Some((revision - 1) as i64)
                },
                revision as i64,
                occurred_at
            ],
        )?;
        connection.execute(
            "INSERT INTO events(event_id, operation_id, item_id, item_revision, event_index,
                 event_type, before_value, after_value, actor_kind, actor_id, actor_surface,
                 actor_host, execution_kind, execution_agent_id, execution_surface,
                 execution_host, reason, note, occurred_at, event_schema_version)
             VALUES (?, ?, ?, ?, 0, ?, ?, ?, 'human', 'BENCH', 'cli', 'local',
                 'direct', NULL, 'cli', 'local', ?, ?, ?, 1)",
            params![
                event_id,
                operation_id,
                item_id,
                revision as i64,
                event.kind,
                event.before,
                event.after,
                event.reason,
                event.note,
                occurred_at
            ],
        )?;
    }
    Ok(())
}

fn lifecycle(status: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut push = |kind, before: &str, after: &str, reason| {
        events.push(Event {
            kind,
            before: Some(before.into()),
            after: Some(after.into()),
            reason,
            note: None,
        });
    };
    match status {
        "proposed" => {}
        "rejected" => push(
            "rejected",
            "proposed",
            "rejected",
            Some("Superseded by the supported workflow"),
        ),
        _ => {
            push("approved", "proposed", "ready", None);
            match status {
                "ready" => {}
                "in_progress" => push("started", "ready", "in_progress", None),
                "blocked" => {
                    push("started", "ready", "in_progress", None);
                    push(
                        "blocked",
                        "in_progress",
                        "blocked",
                        Some("Waiting for a reproducible upstream result"),
                    );
                    push("resumed", "blocked", "in_progress", None);
                    push(
                        "blocked",
                        "in_progress",
                        "blocked",
                        Some("Waiting for a reproducible upstream result"),
                    );
                }
                "done" => {
                    push("started", "ready", "in_progress", None);
                    push(
                        "blocked",
                        "in_progress",
                        "blocked",
                        Some("Waiting for a reproducible upstream result"),
                    );
                    push("resumed", "blocked", "in_progress", None);
                    push("finished", "in_progress", "done", None);
                }
                _ => unreachable!(),
            }
        }
    }
    events
}

fn operation_type(event: &str) -> &'static str {
    match event {
        "captured" => "capture",
        "approved" => "approve",
        "rejected" => "reject",
        "started" => "start",
        "blocked" => "block",
        "resumed" => "resume",
        "finished" => "finish",
        "priority_changed" => "prioritize",
        "assignee_changed" => "assign",
        "note_added" => "triage",
        _ => unreachable!(),
    }
}

fn weighted_project(value: usize) -> &'static str {
    let mut boundary = 0;
    for (project, weight) in PROJECTS {
        boundary += weight;
        if value < boundary {
            return project;
        }
    }
    unreachable!()
}

fn timestamp(bucket: usize) -> String {
    let seconds = bucket % 86_400;
    format!(
        "2025-01-{:02}T{:02}:{:02}:{:02}.000Z",
        1 + (bucket / 86_400) % 28,
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn verify_samples(
    connection: &Connection,
    size: usize,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let indexes = [0, size / 2, size - 1];
    let mut samples = Vec::new();
    for index in indexes {
        let item_id: String = connection.query_row(
            "SELECT item_id FROM items ORDER BY rowid LIMIT 1 OFFSET ?",
            [index as i64],
            |row| row.get(0),
        )?;
        let parsed = parse_item_id(&item_id)?;
        let item = ItemRepository::new(connection)
            .read_item(&parsed)?
            .ok_or("generated sample was not canonically loadable")?;
        let history = ItemHistoryRepository::new(connection)
            .item_history(&parsed)
            .map_err(|error| format!("canonical history load failed: {error:?}"))?;
        if item.revision().get() as usize != history.len() {
            return Err(format!("sample {item_id} revision/history mismatch").into());
        }
        samples.push(item_id);
    }
    Ok(samples)
}

fn verify_replays(connection: &Connection, count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut item_ids = connection.prepare("SELECT item_id FROM items ORDER BY rowid LIMIT ?")?;
    let item_ids = item_ids
        .query_map([count as i64], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    for item_id in item_ids {
        let parsed = parse_item_id(&item_id)?;
        let stored = ItemRepository::new(connection)
            .read_item(&parsed)?
            .ok_or("generated replay item was not canonically loadable")?;
        let history = ItemHistoryRepository::new(connection)
            .item_history(&parsed)
            .map_err(|error| format!("canonical history load failed: {error:?}"))?;
        let captured = history.first().ok_or("generated history was empty")?;
        if captured.event_type != EventType::Captured
            || captured.item_revision.get() != 1
            || captured.before.is_some()
            || captured.after.is_some()
        {
            return Err(format!("item {item_id} has an invalid capture event").into());
        }

        let mut replayed = Item::capture(
            stored.id().clone(),
            stored.content().clone(),
            stored.provenance().clone(),
            stored.captured_at().clone(),
            stored.updated_at().clone(),
        );
        for event in history.iter().skip(1) {
            let mutation = mutation_for(event)?;
            let produced = replayed
                .apply_mutation(mutation)
                .map_err(|error| format!("item {item_id} replay rejected: {error}"))?;
            if produced.len() != 1 {
                return Err(format!("item {item_id} replay produced compound events").into());
            }
            let produced = &produced[0];
            if produced.event_type != event.event_type
                || produced.item_revision != event.item_revision
                || produced.before != event.before
                || produced.after != replay_after(event)
            {
                return Err(format!(
                    "item {item_id} replay diverged at revision {}: produced {produced:?}, stored {event:?}",
                    event.item_revision.get(),
                )
                .into());
            }
        }
        if replayed != stored {
            return Err(format!("item {item_id} replayed state differs from stored item").into());
        }
    }
    Ok(())
}

fn mutation_for(
    event: &bif::application::ItemHistoryEvent,
) -> Result<ItemMutation, Box<dyn std::error::Error>> {
    let lifecycle = match event.event_type {
        EventType::Approved => Some(LifecycleMutation::Approve),
        EventType::Rejected => Some(LifecycleMutation::Reject {
            reason: event.reason.clone().ok_or("rejection reason missing")?,
        }),
        EventType::Started => Some(LifecycleMutation::Start),
        EventType::Blocked => Some(LifecycleMutation::Block {
            reason: event.reason.clone().ok_or("blocking reason missing")?,
        }),
        EventType::Resumed => Some(LifecycleMutation::Resume),
        EventType::Finished => Some(LifecycleMutation::Finish),
        _ => None,
    };
    let triage = match (&event.event_type, &event.after) {
        (EventType::PriorityChanged, Some(EventValue::Priority(priority))) => Some(Triage {
            priority: priority.map_or(TriageField::Clear, TriageField::Set),
            ..Triage::default()
        }),
        (EventType::AssigneeChanged, Some(EventValue::Assignee(assignee))) => Some(Triage {
            assignee: assignee
                .clone()
                .map_or(TriageField::Clear, TriageField::Set),
            ..Triage::default()
        }),
        (EventType::NoteAdded, _) => Some(Triage {
            note: Some(event.note.clone().ok_or("note text missing")?),
            ..Triage::default()
        }),
        _ => None,
    };
    if lifecycle.is_none() && triage.is_none() {
        return Err(format!("cannot replay event {:?}", event.event_type).into());
    }
    Ok(ItemMutation { lifecycle, triage })
}

fn replay_after(event: &bif::application::ItemHistoryEvent) -> Option<EventValue> {
    match event.event_type {
        EventType::NoteAdded => event.note.clone().map(EventValue::Note),
        _ => event.after.clone(),
    }
}

fn parse_item_id(value: &str) -> Result<ItemId, Box<dyn std::error::Error>> {
    let mut parts = value.split(':');
    let requester = RequesterId::new(parts.next().ok_or("missing requester")?)?;
    let project = ProjectId::new(parts.next().ok_or("missing project")?)?;
    let sequence = parts.next().ok_or("missing sequence")?.parse()?;
    if parts.next().is_some() {
        return Err("too many item ID components".into());
    }
    Ok(ItemId::new(requester, project, sequence)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_covers_store_identity_and_requester_counters() {
        let mut connection = Connection::open_in_memory().unwrap();
        storage::migrate(&mut connection).unwrap();
        generate(&mut connection, 100, 2003).unwrap();
        let original = benchmark_fixture::summarize(&connection).unwrap().digest;
        connection
            .execute("UPDATE store_metadata SET store_id = 'changed'", [])
            .unwrap();
        assert_ne!(
            original,
            benchmark_fixture::summarize(&connection).unwrap().digest
        );

        let metadata_changed = benchmark_fixture::summarize(&connection).unwrap().digest;
        connection
            .execute(
                "UPDATE requester_project_counters SET next_sequence = next_sequence + 1
                 WHERE rowid = (SELECT min(rowid) FROM requester_project_counters)",
                [],
            )
            .unwrap();
        assert_ne!(
            metadata_changed,
            benchmark_fixture::summarize(&connection).unwrap().digest
        );
    }

    #[test]
    fn all_small_fixture_histories_replay_through_domain_rules() {
        let mut connection = Connection::open_in_memory().unwrap();
        storage::migrate(&mut connection).unwrap();
        generate(&mut connection, 100, 2003).unwrap();

        verify_replays(&connection, 100).unwrap();

        let mut kinds = connection
            .prepare("SELECT DISTINCT event_type FROM events")
            .unwrap();
        let kinds = kinds
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for expected in [
            "captured",
            "approved",
            "rejected",
            "started",
            "blocked",
            "resumed",
            "finished",
            "priority_changed",
            "assignee_changed",
            "note_added",
        ] {
            assert!(kinds.iter().any(|kind| kind == expected), "{expected}");
        }

        for terminal in ["done", "rejected"] {
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM items WHERE status = ?",
                    [terminal],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(count > 0, "{terminal}");
        }
    }
}
