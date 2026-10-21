mod maintenance_support;
#[allow(dead_code, unused_imports)]
mod mutation_journal_support;

use std::fs;

use bif::storage::{self, maintenance};
use maintenance_support::{Fixture, capture, generation, inventory, read, schema};
use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
};

#[test]
fn activation_restore_reactivation_preserves_history_and_never_completes_clients() {
    use bif::storage::journal::{Generation, JournalError, JournalSnapshot};
    for through in [0, 1, 2] {
        let fixture = Fixture::new();
        let old = Generation::parse(&generation(&read(&fixture.store))).unwrap();
        maintenance::prune(&fixture.store, &old, through).unwrap();
        // Ordinary reopen, including repeat migration preflight, cannot enable.
        for _ in 0..2 {
            let connection = storage::open(&fixture.store).unwrap();
            assert!(
                !JournalSnapshot::begin(&connection)
                    .unwrap()
                    .metadata()
                    .sync_enabled
            );
        }
        let before = inventory(&read(&fixture.store));
        let ddl = schema(&read(&fixture.store));
        let mut enabled = before.clone();
        enabled
            .iter_mut()
            .find(|(name, _)| name == "journal_metadata")
            .unwrap()
            .1[0][4] = 1_i64.into();
        maintenance::activate(&fixture.store, &old).unwrap();
        assert_eq!(inventory(&storage::open(&fixture.store).unwrap()), enabled);
        maintenance::activate(&fixture.store, &old).unwrap();
        assert_eq!(inventory(&read(&fixture.store)), enabled);
        assert_eq!(schema(&read(&fixture.store)), ddl);
        maintenance::backup(&fixture.store, &fixture.backup).unwrap();
        maintenance::restore(&fixture.store, &fixture.backup, fixture.path("later")).unwrap();
        let restored = storage::open(&fixture.store).unwrap();
        let fresh = Generation::parse(&generation(&restored)).unwrap();
        assert_ne!(old, fresh);
        let mut expected = before;
        expected
            .iter_mut()
            .find(|(name, _)| name == "journal_metadata")
            .unwrap()
            .1[0][1] = fresh.as_str().to_owned().into();
        assert_eq!(inventory(&restored), expected);
        drop(restored);
        assert!(matches!(
            maintenance::activate(&fixture.store, &old),
            Err(maintenance::MaintenanceError::Journal(
                JournalError::GenerationMismatch
            ))
        ));
        assert_eq!(inventory(&read(&fixture.store)), expected);
        maintenance::activate(&fixture.store, &fresh).unwrap();
        expected
            .iter_mut()
            .find(|(name, _)| name == "journal_metadata")
            .unwrap()
            .1[0][4] = 1_i64.into();
        assert_eq!(inventory(&storage::open(&fixture.store).unwrap()), expected);
    }
    let fixture = Fixture::new();
    let empty = fixture.path("empty");
    let connection = storage::open(&empty).unwrap();
    let generation = bif::storage::journal::Generation::parse(&generation(&connection)).unwrap();
    drop(connection);
    maintenance::activate(&empty, &generation).unwrap();
    let connection = storage::open(&empty).unwrap();
    let snapshot = bif::storage::journal::JournalSnapshot::begin(&connection).unwrap();
    assert!(snapshot.metadata().sync_enabled);
    assert!(snapshot.metadata().bootstrap_required);
    assert_eq!(
        (
            snapshot.metadata().retention_floor,
            snapshot.metadata().high_water
        ),
        (0, 0)
    );
}

#[test]
fn activation_rejects_invalid_prerequisites_even_on_enabled_retry_without_repair() {
    use bif::storage::journal::Generation;
    for enabled in [false, true] {
        for damage in [
            "DELETE FROM schema_migrations WHERE version = 4",
            "DELETE FROM schema_migrations WHERE version = 2",
            "UPDATE schema_migrations SET version = 5 WHERE version = 4",
            "UPDATE schema_migrations SET checksum = 'bad' WHERE version = 4",
            "UPDATE schema_migrations SET name = 'bad' WHERE version = 4",
            "UPDATE schema_migrations SET applied_at = '' WHERE version = 4",
            "DROP TRIGGER change_journal_reject_update",
            "CREATE TABLE extra (x)",
            "PRAGMA user_version = 9",
            "PRAGMA application_id = 9",
            "DELETE FROM journal_metadata",
            "UPDATE journal_metadata SET generation = 'bad'",
            "UPDATE journal_metadata SET sync_enabled = 7",
            "UPDATE journal_metadata SET bootstrap_required = 7",
            "DELETE FROM store_metadata",
            "UPDATE store_metadata SET store_id = ''",
            "DELETE FROM change_journal WHERE change_seq = 1",
            "UPDATE journal_metadata SET high_water = 3",
            "UPDATE sqlite_sequence SET seq = 0 WHERE name = 'change_journal'",
            "UPDATE sqlite_sequence SET seq = 3 WHERE name = 'change_journal'",
            "INSERT INTO sqlite_sequence (name, seq) VALUES ('change_journal', 2)",
            "UPDATE items SET project_id = 'missing'",
            "UPDATE change_journal SET item_revision = 99",
            "UPDATE change_journal SET operation_id = 'missing' WHERE change_seq = 1",
            "UPDATE change_journal SET project_id = 'other'",
        ] {
            let fixture = Fixture::new();
            let corrupt = Connection::open(&fixture.store).unwrap();
            let generation = Generation::parse(&generation(&corrupt)).unwrap();
            if enabled {
                maintenance::activate(&fixture.store, &generation).unwrap();
            }
            corrupt
                .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON")
                .unwrap();
            let immutable: String = corrupt
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name = 'change_journal_reject_update'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if damage.starts_with("UPDATE change_journal") {
                corrupt
                    .execute_batch("DROP TRIGGER change_journal_reject_update")
                    .unwrap();
            }
            corrupt.execute_batch(damage).unwrap();
            if damage.starts_with("UPDATE change_journal") {
                corrupt.execute_batch(&immutable).unwrap();
            }
            drop(corrupt);
            let before = inventory(&read(&fixture.store));
            let ddl = schema(&read(&fixture.store));
            assert!(
                maintenance::activate(&fixture.store, &generation).is_err(),
                "{damage}"
            );
            assert_eq!(inventory(&read(&fixture.store)), before, "{damage}");
            assert_eq!(schema(&read(&fixture.store)), ddl, "{damage}");
        }
    }
}

#[test]
fn activation_missing_unsafe_or_busy_store_fails_closed() {
    use bif::storage::journal::Generation;
    let fixture = Fixture::new();
    let generation = Generation::parse(&generation(&read(&fixture.store))).unwrap();
    let missing = fixture.path("missing");
    assert!(maintenance::activate(&missing, &generation).is_err());
    assert!(!missing.exists());
    assert!(maintenance::activate(fixture.directory.path(), &generation).is_err());
    let busy = storage::open(&fixture.store).unwrap();
    let before = inventory(&busy);
    busy.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(maintenance::activate(&fixture.store, &generation).is_err());
    busy.execute_batch("ROLLBACK").unwrap();
    assert_eq!(inventory(&read(&fixture.store)), before);
    drop(busy);
    #[cfg(unix)]
    {
        let innocent = fixture.path("innocent");
        fs::write(&innocent, b"do not mutate").unwrap();
        std::os::unix::fs::symlink(&innocent, format!("{}-wal", fixture.store.display())).unwrap();
        assert!(maintenance::activate(&fixture.store, &generation).is_err());
        assert_eq!(fs::read(&innocent).unwrap(), b"do not mutate");
    }
}

#[test]
fn backup_reads_committed_wal_frames_and_never_overwrites() {
    let fixture = Fixture::new();
    let mut writer = storage::open(&fixture.store).unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    capture(&mut writer, "wal-only", "committed in WAL");
    assert!(
        fs::metadata(format!("{}-wal", fixture.store.display()))
            .unwrap()
            .len()
            > 32
    );
    maintenance::backup(&fixture.store, &fixture.backup).unwrap();
    assert_eq!(inventory(&writer), inventory(&read(&fixture.backup)));
    assert_eq!(
        read(&fixture.backup)
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!std::path::Path::new(&format!("{}{suffix}", fixture.backup.display())).exists());
    }
    let pristine = fs::read(&fixture.backup).unwrap();
    assert!(maintenance::backup(&fixture.store, &fixture.backup).is_err());
    assert_eq!(fs::read(&fixture.backup).unwrap(), pristine);
}

#[test]
fn same_backup_restored_twice_preserves_everything_except_generation_and_activation() {
    let fixture = Fixture::new();
    let writer = storage::open(&fixture.store).unwrap();
    writer
        .execute_batch(
            "UPDATE journal_metadata SET sync_enabled = 1, bootstrap_required = 0;
         DELETE FROM change_journal WHERE change_seq = 1;
         UPDATE journal_metadata SET retention_floor = 1;",
        )
        .unwrap();
    maintenance::backup(&fixture.store, &fixture.backup).unwrap();
    let backup_bytes = fs::read(&fixture.backup).unwrap();
    let saved = read(&fixture.backup);
    let old_generation = generation(&saved);
    let saved_schema = schema(&saved);
    let mut expected = inventory(&saved);
    let cookie: i64 = writer
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .unwrap();
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&fixture.store).unwrap().ino()
    };
    let mut generations = vec![old_generation];
    for round in 0..2 {
        let mut current = storage::open(&fixture.store).unwrap();
        capture(&mut current, &format!("later-{round}"), "later write");
        current
            .execute("UPDATE items SET title = ?1", [format!("changed-{round}")])
            .unwrap();
        let before = inventory(&current);
        let preserved = fixture.path(&format!("later-{round}.sqlite"));
        maintenance::restore(&fixture.store, &fixture.backup, &preserved).unwrap();
        assert_eq!(inventory(&read(&preserved)), before);
        let preserved_bytes = fs::read(&preserved).unwrap();
        let restored = read(&fixture.store);
        let fresh = generation(&restored);
        assert!(
            !generations.contains(&fresh),
            "independent fresh draw every restore"
        );
        assert_eq!(fresh.len(), 32);
        generations.push(fresh.clone());
        let metadata = expected
            .iter_mut()
            .find(|(name, _)| name == "journal_metadata")
            .unwrap();
        metadata.1[0][1] = fresh.into();
        metadata.1[0][4] = 0_i64.into();
        metadata.1[0][5] = 1_i64.into();
        assert_eq!(inventory(&restored), expected);
        assert_eq!(schema(&restored), saved_schema);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&fixture.store).unwrap().ino(), inode);
        }
        // SQLite increments the destination schema cookie during online backup.
        // Reset only this disposable control, so the next restore's token checks
        // cannot pass merely because that unrelated cookie changed.
        current
            .pragma_update(None, "schema_version", cookie)
            .unwrap();
        assert!(maintenance::restore(&fixture.store, &fixture.backup, &preserved).is_err());
        assert_eq!(fs::read(&preserved).unwrap(), preserved_bytes);
        assert_eq!(fs::read(&fixture.backup).unwrap(), backup_bytes);
    }
}

#[test]
fn invalid_backups_fail_before_preservation_and_leave_target_logically_unchanged() {
    for damage in [
        "DELETE FROM schema_migrations WHERE version = 4",
        "DELETE FROM schema_migrations WHERE version = 2",
        "UPDATE schema_migrations SET version = 5 WHERE version = 4",
        "UPDATE schema_migrations SET checksum = 'bad' WHERE version = 4",
        "UPDATE schema_migrations SET name = 'bad' WHERE version = 4",
        "UPDATE schema_migrations SET applied_at = '' WHERE version = 4",
        "DROP TRIGGER change_journal_reject_update",
        "CREATE TABLE extra (x)",
        "DELETE FROM journal_metadata",
        "UPDATE journal_metadata SET generation = 'bad'",
        "DELETE FROM store_metadata",
        "UPDATE store_metadata SET store_id = ''",
        "DELETE FROM change_journal WHERE change_seq = 1",
        "UPDATE journal_metadata SET high_water = 3",
        "UPDATE sqlite_sequence SET seq = 0 WHERE name = 'change_journal'",
        "UPDATE sqlite_sequence SET seq = 3 WHERE name = 'change_journal'",
        "INSERT INTO sqlite_sequence (name, seq) VALUES ('change_journal', 2)",
        "UPDATE items SET project_id = 'missing'",
        "DROP TRIGGER change_journal_reject_update; UPDATE change_journal SET item_revision = 99",
    ] {
        let fixture = Fixture::new();
        maintenance::backup(&fixture.store, &fixture.backup).unwrap();
        let corrupt = Connection::open(&fixture.backup).unwrap();
        corrupt
            .execute_batch("PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON")
            .unwrap();
        let immutable: String = corrupt
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name = 'change_journal_reject_update'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        corrupt.execute_batch(damage).unwrap();
        if damage.contains("UPDATE change_journal") {
            corrupt.execute_batch(&immutable).unwrap();
        }
        drop(corrupt);
        let before = inventory(&read(&fixture.store));
        let preserve = fixture.path("later.sqlite");
        assert!(
            maintenance::restore(&fixture.store, &fixture.backup, &preserve).is_err(),
            "{damage}"
        );
        assert_eq!(inventory(&read(&fixture.store)), before, "{damage}");
        assert!(!preserve.exists(), "{damage}");
    }
    let fixture = Fixture::new();
    fs::write(&fixture.backup, b"not sqlite").unwrap();
    let before = inventory(&read(&fixture.store));
    assert!(maintenance::restore(&fixture.store, &fixture.backup, fixture.path("later")).is_err());
    assert_eq!(inventory(&read(&fixture.store)), before);
}

#[test]
fn aliases_sidecar_collisions_and_existing_preservation_are_rejected_without_mutation() {
    let fixture = Fixture::new();
    maintenance::backup(&fixture.store, &fixture.backup).unwrap();
    let before = inventory(&read(&fixture.store));
    let backup_bytes = fs::read(&fixture.backup).unwrap();
    let alias = fixture.path("alias");
    fs::hard_link(&fixture.store, &alias).unwrap();
    for input in [&fixture.store, &alias] {
        assert!(maintenance::restore(&fixture.store, input, fixture.path("later")).is_err());
    }
    #[cfg(unix)]
    {
        let symbolic = fixture.path("symbolic");
        std::os::unix::fs::symlink(&fixture.store, &symbolic).unwrap();
        assert!(maintenance::restore(&fixture.store, &symbolic, fixture.path("later")).is_err());
    }
    for preserve in [
        fixture.store.clone(),
        alias,
        fixture.backup.clone(),
        std::path::PathBuf::from(format!("{}-wal", fixture.store.display())),
        std::path::PathBuf::from(format!("{}-shm", fixture.backup.display())),
    ] {
        assert!(maintenance::restore(&fixture.store, &fixture.backup, preserve).is_err());
    }
    let output = fixture.path("unsafe");
    fs::write(format!("{}-wal", output.display()), b"reserved sidecar").unwrap();
    assert!(maintenance::backup(&fixture.store, &output).is_err());
    assert!(!output.exists());
    assert_eq!(inventory(&read(&fixture.store)), before);
    assert_eq!(fs::read(&fixture.backup).unwrap(), backup_bytes);
}

#[test]
fn blocked_publish_retains_verified_preservation_and_original_target() {
    let fixture = Fixture::new();
    maintenance::backup(&fixture.store, &fixture.backup).unwrap();
    let mut current = storage::open(&fixture.store).unwrap();
    capture(&mut current, "later", "preserve me");
    let before = inventory(&current);
    current.execute_batch("BEGIN IMMEDIATE").unwrap();
    let preservation = fixture.path("later.sqlite");
    assert!(maintenance::restore(&fixture.store, &fixture.backup, &preservation).is_err());
    current.execute_batch("ROLLBACK").unwrap();
    assert_eq!(inventory(&read(&fixture.store)), before);
    assert_eq!(inventory(&read(&preservation)), before);
}

#[test]
fn incompatible_wal_page_size_fails_transfer_without_target_changes() {
    let fixture = Fixture::new();
    let larger = fixture.path("larger.sqlite");
    let connection = Connection::open(&larger).unwrap();
    connection.pragma_update(None, "page_size", 8192).unwrap();
    connection.execute_batch("VACUUM").unwrap();
    drop(connection);
    let mut connection = storage::open(&larger).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))
            .unwrap(),
        8192
    );
    capture(&mut connection, "large", "8192 byte pages");
    maintenance::backup(&larger, &fixture.backup).unwrap();
    let before = inventory(&read(&fixture.store));
    let preservation = fixture.path("later.sqlite");
    assert!(maintenance::restore(&fixture.store, &fixture.backup, &preservation).is_err());
    assert_eq!(inventory(&read(&fixture.store)), before);
    assert_eq!(inventory(&read(&preservation)), before);
}

#[cfg(unix)]
#[test]
fn input_sidecar_symlinks_are_unsafe_even_when_main_paths_are_distinct() {
    let fixture = Fixture::new();
    maintenance::backup(&fixture.store, &fixture.backup).unwrap();
    let innocent = fixture.path("innocent");
    fs::write(&innocent, b"do not mutate").unwrap();
    let sidecar = format!("{}-shm", fixture.backup.display());
    std::os::unix::fs::symlink(&innocent, &sidecar).unwrap();
    let before = inventory(&read(&fixture.store));
    assert!(maintenance::restore(&fixture.store, &fixture.backup, fixture.path("later")).is_err());
    assert_eq!(inventory(&read(&fixture.store)), before);
    assert_eq!(fs::read(&innocent).unwrap(), b"do not mutate");
}

#[test]
fn pruned_all_and_empty_histories_keep_high_water_and_allocators() {
    for populated in [false, true] {
        let fixture = Fixture::new();
        let store = if populated {
            fixture.store.clone()
        } else {
            let empty = fixture.path("empty.sqlite");
            storage::open(&empty).unwrap();
            empty
        };
        let writer = storage::open(&store).unwrap();
        writer
            .execute_batch(
                "DELETE FROM change_journal;
             UPDATE journal_metadata SET retention_floor = high_water",
            )
            .unwrap();
        maintenance::backup(&store, &fixture.backup).unwrap();
        let before = inventory(&writer);
        maintenance::restore(&store, &fixture.backup, fixture.path("later")).unwrap();
        let mut after = inventory(&read(&store));
        let original = before
            .iter()
            .find(|(name, _)| name == "journal_metadata")
            .unwrap()
            .clone();
        *after
            .iter_mut()
            .find(|(name, _)| name == "journal_metadata")
            .unwrap() = original;
        assert_eq!(after, before);
    }
}

#[test]
fn abandoning_incomplete_online_backup_rolls_back_the_destination() {
    let fixture = Fixture::new();
    let source = read(&fixture.store);
    let target_path = fixture.path("target.sqlite");
    let mut target = storage::open(&target_path).unwrap();
    capture(&mut target, "target", "must remain");
    let before = inventory(&target);
    {
        let transfer = Backup::new(&source, &mut target).unwrap();
        assert_eq!(transfer.step(1).unwrap(), StepResult::More);
        assert!(transfer.progress().remaining > 0);
        // Drop calls sqlite3_backup_finish; incomplete destination transaction rolls back.
    }
    drop(target);
    assert_eq!(inventory(&read(&target_path)), before);
}

#[test]
fn repeated_restore_invalidates_tokens_and_warm_handles_even_with_same_schema_cookie() {
    use bif::{
        application::{
            Actor, ActorKind, AuthorizationRequest, CaptureStoreError, Command, Execution,
            ItemListFilters, ItemListOrdering, ItemProjectionKind, MutationUseCaseError,
            ObservedExecution,
        },
        config::Config,
        domain::{ItemId, NamedView, ProjectId, RequesterId},
        read_session::{ReadRequest, ReadSession},
        storage::{CaptureRepository, MutationRepository},
        v2_response::{ReadError, ResponseBudget},
    };
    use mutation_journal_support::{compound, mutate, mutate_with_repository, request};
    let fixture = Fixture::new();
    let mut writer = storage::open(&fixture.store).unwrap();
    let id = ItemId::new(
        RequesterId::new("ALICE").unwrap(),
        ProjectId::new("bif").unwrap(),
        1,
    )
    .unwrap();
    let mutation = request(&id, "compound", 1, compound());
    mutate(&mut writer, mutation.clone(), "compound", 4).unwrap();
    maintenance::backup(&fixture.store, &fixture.backup).unwrap();
    let config = Config {
        root: fixture.directory.path().to_owned(),
        requester: RequesterId::new("ALICE").unwrap(),
        source: None,
    };
    let authorization = AuthorizationRequest {
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
        human_authorization: None,
    };
    let get = |version| ReadRequest::Get {
        item_id: id.clone(),
        projection: ItemProjectionKind::Work,
        conditional: true,
        known_version: version,
    };
    let page = |cursor| ReadRequest::Page {
        view: NamedView::All,
        ordering: ItemListOrdering::NewestFirst,
        projection: ItemProjectionKind::Summary,
        filters: ItemListFilters {
            project: Some(ProjectId::new("bif").unwrap()),
            ..Default::default()
        },
        limit: 1,
        cursor,
    };
    let history = |cursor| ReadRequest::History {
        item_id: id.clone(),
        limit: 1,
        cursor,
    };
    let run = |session: &mut ReadSession, read| -> serde_json::Value {
        serde_json::from_slice(
            &session
                .execute(&authorization, read, ResponseBudget::default())
                .unwrap(),
        )
        .unwrap()
    };
    let mut all_generations = vec![generation(&writer)];
    for round in 0..2 {
        let mut warm = ReadSession::open(config.clone()).unwrap();
        let version = run(&mut warm, get(None))["result"]["version"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            run(&mut warm, get(Some(version.clone())))["result"]["outcome"],
            "not_modified"
        );
        let page_cursor = run(&mut warm, page(None))["result"]["next_cursor"]
            .as_str()
            .unwrap()
            .to_owned();
        let history_cursor = run(&mut warm, history(None))["result"]["next_cursor"]
            .as_str()
            .unwrap()
            .to_owned();
        let cookie: i64 = writer
            .pragma_query_value(None, "schema_version", |row| row.get(0))
            .unwrap();
        let mut capture_connection = storage::open(&fixture.store).unwrap();
        let mut mutation_connection = storage::open(&fixture.store).unwrap();
        let mut warm_capture = CaptureRepository::new(&mut capture_connection).unwrap();
        let mut warm_mutation = MutationRepository::new(&mut mutation_connection).unwrap();
        capture(&mut writer, &format!("between-{round}"), "later capture");
        writer
            .execute(
                "UPDATE items SET title = ?1",
                [format!("later content {round}")],
            )
            .unwrap();
        let later_mutation = request(
            &id,
            &format!("later-mutation-{round}"),
            2,
            bif::domain::ItemMutation {
                lifecycle: Some(bif::domain::LifecycleMutation::Start),
                triage: None,
            },
        );
        mutate(
            &mut writer,
            later_mutation,
            &format!("later-mutation-{round}"),
            1,
        )
        .unwrap();
        maintenance::restore(
            &fixture.store,
            &fixture.backup,
            fixture.path(&format!("preserve-{round}")),
        )
        .unwrap();
        writer
            .pragma_update(None, "schema_version", cookie)
            .unwrap();
        assert_eq!(
            writer
                .pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            cookie
        );
        let fresh = generation(&writer);
        assert!(!all_generations.contains(&fresh));
        all_generations.push(fresh);
        assert!(matches!(
            warm.execute(&authorization, get(None), ResponseBudget::default()),
            Err(ReadError::RestartRequired { .. })
        ));
        assert!(matches!(
            mutation_journal_support::capture_support::capture_with_repository(
                &mut warm_capture,
                "one",
                "no-write",
                "bif"
            ),
            Err(CaptureStoreError::RestartRequired)
        ));
        assert!(matches!(
            mutate_with_repository(&mut warm_mutation, mutation.clone(), "no-write", 4),
            Err(MutationUseCaseError::RestartRequired)
        ));
        let mut fresh = ReadSession::open(config.clone()).unwrap();
        assert_eq!(
            run(&mut fresh, get(Some(version)))["result"]["outcome"],
            "modified"
        );
        for stale in [page(Some(page_cursor)), history(Some(history_cursor))] {
            assert!(matches!(
                fresh.execute(&authorization, stale, ResponseBudget::default()),
                Err(ReadError::InvalidCursor { .. })
            ));
        }
        assert_eq!(run(&mut fresh, get(None))["result"]["item"]["revision"], 2);
    }
}
