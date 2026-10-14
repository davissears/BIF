//! Offline, current-schema SQLite backup, in-place restore and journal pruning.
//!
//! OPERATOR PREREQUISITE for all APIs: stop/reap every reader and writer and
//! disable all automatic restarts/new launches until verification finishes.
//! An acknowledgement, SQLite lock, or generation guard is NOT shutdown proof.
//! Paths/directories must remain trusted and must not be replaced concurrently.
//! These APIs do not migrate, down-migrate, reconcile later writes, or enable sync.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use rusqlite::{
    Connection, OpenFlags, TransactionBehavior,
    backup::{Backup, StepResult},
};

use super::{CREATE_MIGRATION_TABLE, MIGRATIONS, MigrationError, journal};

#[derive(Debug)]
pub enum MaintenanceError {
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Migration(MigrationError),
    Journal(journal::JournalError),
    Invalid(&'static str),
}

impl fmt::Display for MaintenanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "maintenance path error: {error}"),
            Self::Sqlite(error) => write!(f, "SQLite maintenance error: {error}"),
            Self::Migration(error) => error.fmt(f),
            Self::Journal(error) => error.fmt(f),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for MaintenanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Migration(error) => Some(error),
            Self::Journal(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<io::Error> for MaintenanceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for MaintenanceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<MigrationError> for MaintenanceError {
    fn from(error: MigrationError) -> Self {
        Self::Migration(error)
    }
}

impl From<journal::JournalError> for MaintenanceError {
    fn from(error: journal::JournalError) -> Self {
        Self::Journal(error)
    }
}

/// Prunes global journal positions through an explicit inclusive bound.
///
/// Caller must satisfy the module's offline/trusted-path prerequisite. Within
/// one immediate transaction, require the expected generation and F <= through
/// <= H, delete covered rows, and advance only F. Audits, receipts, identity,
/// H/allocator and inactive flags are retained. No automatic policy or VACUUM.
pub fn prune(
    store: impl AsRef<Path>,
    expected_generation: &journal::Generation,
    through: i64,
) -> Result<(), MaintenanceError> {
    let store = existing(store.as_ref())?;
    let mut connection = open_existing(&store, true)?;
    prune_opened(&mut connection, expected_generation, through)
}

fn prune_opened(
    connection: &mut Connection,
    expected_generation: &journal::Generation,
    through: i64,
) -> Result<(), MaintenanceError> {
    // Acquire the writer reservation before validation/binding. A concurrent
    // writer cannot move coverage between these checks and publication.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate(&transaction)?;
    let metadata = journal::read_metadata(&transaction)?;
    if &metadata.generation != expected_generation {
        return Err(journal::JournalError::GenerationMismatch.into());
    }
    if through < metadata.retention_floor || through > metadata.high_water {
        return Err(journal::JournalError::InvalidCoverage.into());
    }
    transaction.execute(
        "DELETE FROM change_journal WHERE change_seq <= ?1",
        [through],
    )?;
    transaction.execute(
        "UPDATE journal_metadata SET retention_floor = ?1 WHERE singleton = 1",
        [through],
    )?;
    validate(&transaction)?;
    transaction.commit()?;
    // Any error above drops the transaction, including a floor-update ABORT
    // (which by itself would roll back only that statement, not the DELETE).
    Ok(())
}

/// Creates and verifies a SQLite-consistent standalone backup at a new path.
///
/// Caller must satisfy the module's offline/trusted-path prerequisite. Source
/// must already have the exact current schema. Existing output or sidecars are
/// refused; an unsuccessful claimed output is retained, never deleted/reused.
pub fn backup(store: impl AsRef<Path>, output: impl AsRef<Path>) -> Result<(), MaintenanceError> {
    let store = existing(store.as_ref())?;
    let output = destination(output.as_ref())?;
    distinct(&[&store, &output])?;
    let source = open_existing(&store, false)?;
    let snapshot = source.unchecked_transaction()?;
    validate(&snapshot)?;
    create_backup(&snapshot, &output)?;
    snapshot.commit()?;
    Ok(())
}

/// Restores through a private validated stage and SQLite's atomic backup write.
///
/// Caller must stop/reap readers AND writers and disable restart/new launches.
/// `preserve_later` must be a new destination: a verified backup of the current
/// target is completed before publication and is never overwritten or deleted,
/// including on failure. Every restore independently draws an OS-random128
/// generation in the staging transaction; only generation and inactive/bootstrap
/// flags change. Target's inode stays in place. SQLite advances its schema cookie.
/// A returned error after publication means keep maintenance in force and inspect
/// the target/preservation; it does not promise the restore never committed.
pub fn restore(
    store: impl AsRef<Path>,
    from: impl AsRef<Path>,
    preserve_later: impl AsRef<Path>,
) -> Result<(), MaintenanceError> {
    // All path rejection precedes even opening the target.
    let store = existing(store.as_ref())?;
    let from = existing(from.as_ref())?;
    let preserve_later = destination(preserve_later.as_ref())?;
    distinct(&[&store, &from, &preserve_later])?;

    let directory = tempfile::Builder::new()
        .prefix(".bif-restore-")
        .tempdir_in(store.parent().expect("canonical path has parent"))?;
    let staged_path = directory.path().join("stage.sqlite");
    let source = open_existing(&from, false)?;
    let mut staged = Connection::open(&staged_path)?;
    transfer(&source, &mut staged)?;
    drop(source);
    validate(&staged)?;

    let transaction = staged.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let generation = journal::fresh_generation()?;
    transaction.execute(
        "UPDATE journal_metadata
         SET generation = ?1, sync_enabled = 0, bootstrap_required = 1
         WHERE singleton = 1",
        [generation.as_str()],
    )?;
    transaction.commit()?;
    validate(&staged)?;

    let current = open_existing(&store, false)?;
    let snapshot = current.unchecked_transaction()?;
    validate(&snapshot)?;
    create_backup(&snapshot, &preserve_later)?;
    snapshot.commit()?;
    drop(current);

    // No transaction may be open on the destination at backup_init. SQLite owns
    // its write transaction; finish before DONE rolls it back, DONE commits it.
    let mut target = open_existing(&store, true)?;
    transfer(&staged, &mut target)?;
    target.close().map_err(|(_, error)| error)?;
    let reopened = open_existing(&store, false)?;
    validate(&reopened)?;
    if journal::read_metadata(&reopened)?.generation != generation {
        return Err(MaintenanceError::Invalid(
            "restored generation verification failed",
        ));
    }
    Ok(())
}

fn open_existing(path: &Path, writable: bool) -> Result<Connection, MaintenanceError> {
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let connection = Connection::open_with_flags(path, flags)?;
    // Never call the normal factory: it migrates and changes journal mode.
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.busy_timeout(std::time::Duration::ZERO)?;
    Ok(connection)
}

fn transfer(source: &Connection, target: &mut Connection) -> Result<(), MaintenanceError> {
    let transfer = Backup::new(source, target)?;
    // One step holds the source snapshot and publishes only on SQLITE_DONE.
    // Fail closed on busy/locked rather than wait for an unproven shutdown.
    match transfer.step(-1)? {
        StepResult::Done => Ok(()),
        _ => Err(MaintenanceError::Invalid(
            "SQLite backup incomplete or busy; keep maintenance in force",
        )),
    }
    // Backup::drop calls sqlite3_backup_finish, including all error paths.
}

fn create_backup(source: &Connection, output: &Path) -> Result<(), MaintenanceError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(output)?;
    let mut target = open_existing(output, true)?;
    transfer(source, &mut target)?;
    // A standalone backup must not require mutable WAL sidecars when reopened.
    // This changes only the new output's SQLite journaling header, not the source.
    target.pragma_update(None, "journal_mode", "DELETE")?;
    validate(&target)?;
    // SQLite closes/checkpoints its own sidecars; never copy/remove WAL/SHM.
    target.close().map_err(|(_, error)| error)?;
    validate(&open_existing(output, false)?)?;
    file.sync_all()?;
    Ok(())
}

/// Full checks against embedded schema, not just a trusted migration version.
fn validate(connection: &Connection) -> Result<(), MaintenanceError> {
    let integrity = connection
        .prepare("PRAGMA integrity_check")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if integrity != ["ok"] {
        return Err(MaintenanceError::Invalid("database integrity_check failed"));
    }
    if connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(MaintenanceError::Invalid(
            "database foreign_key_check failed",
        ));
    }
    if super::migration_prefix(connection, MIGRATIONS)? != MIGRATIONS.len() {
        return Err(MaintenanceError::Invalid(
            "maintenance requires exact current migration history",
        ));
    }
    super::read_store_state(connection)?;
    let identity_count: i64 = connection.query_row(
        "SELECT count(*) FROM store_metadata WHERE singleton = 1 AND length(store_id) > 0",
        [],
        |row| row.get(0),
    )?;
    let count: i64 =
        connection.query_row("SELECT count(*) FROM store_metadata", [], |row| row.get(0))?;
    if identity_count != 1 || count != 1 {
        return Err(MaintenanceError::Invalid("invalid store identity"));
    }
    let reference = Connection::open_in_memory()?;
    reference.execute_batch(CREATE_MIGRATION_TABLE)?;
    for migration in MIGRATIONS {
        reference.execute_batch(migration.sql)?;
    }
    if schema(connection)? != schema(&reference)? {
        return Err(MaintenanceError::Invalid(
            "maintenance requires exact current SQLite schema",
        ));
    }
    for pragma in ["user_version", "application_id"] {
        let actual: i64 = connection.pragma_query_value(None, pragma, |row| row.get(0))?;
        let expected: i64 = reference.pragma_query_value(None, pragma, |row| row.get(0))?;
        if actual != expected {
            return Err(MaintenanceError::Invalid(
                "unsupported database header version",
            ));
        }
    }
    validate_journal(connection)?;
    Ok(())
}

type SchemaObject = (String, String, String, Option<String>);
fn schema(connection: &Connection) -> rusqlite::Result<Vec<SchemaObject>> {
    connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect()
}

fn validate_journal(connection: &Connection) -> Result<(), MaintenanceError> {
    let metadata = journal::read_metadata(connection)?;
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM change_journal j
           LEFT JOIN operations o ON o.operation_id = j.operation_id
           LEFT JOIN items i ON i.item_id = j.item_id
           WHERE j.change_seq > ?1 OR o.operation_id IS NULL OR i.item_id IS NULL
              OR j.item_id != o.item_id OR j.item_revision != o.item_revision
              OR j.project_id != i.project_id OR j.item_revision > i.revision)",
        [metadata.high_water],
        |row| row.get(0),
    )?;
    let retained: i64 = connection.query_row(
        "SELECT count(*) FROM change_journal WHERE change_seq > ?1 AND change_seq <= ?2",
        [metadata.retention_floor, metadata.high_water],
        |row| row.get(0),
    )?;
    let allocator = connection
        .prepare("SELECT seq FROM sqlite_sequence WHERE name = 'change_journal'")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if invalid
        || retained != metadata.high_water - metadata.retention_floor
        || !(allocator == [metadata.high_water]
            || (allocator.is_empty() && metadata.high_water == 0))
    {
        return Err(MaintenanceError::Invalid(
            "invalid retained journal history or allocator",
        ));
    }
    Ok(())
}

fn existing(path: &Path) -> Result<PathBuf, MaintenanceError> {
    let path = fs::canonicalize(path)?;
    if !fs::metadata(&path)?.is_file() {
        return Err(MaintenanceError::Invalid(
            "database input must be a regular file",
        ));
    }
    let family = family(&path);
    for (index, member) in family.iter().enumerate() {
        match fs::symlink_metadata(member) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(MaintenanceError::Invalid(
                    "input SQLite sidecar is not a regular file",
                ));
            }
            Ok(_) => {
                for other in &family[index + 1..] {
                    if other.exists() && same_file::is_same_file(member, other)? {
                        return Err(MaintenanceError::Invalid(
                            "input database aliases its SQLite sidecar",
                        ));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

fn destination(path: &Path) -> Result<PathBuf, MaintenanceError> {
    let filename = path
        .file_name()
        .ok_or(MaintenanceError::Invalid("output requires a filename"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let path = fs::canonicalize(parent)?.join(filename);
    for candidate in family(&path) {
        match fs::symlink_metadata(candidate) {
            Ok(_) => {
                return Err(MaintenanceError::Invalid(
                    "output or SQLite sidecar already exists; choose a new destination",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

/// Database names reserve their SQLite sidecars, even when those do not exist.
fn family(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_owned()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        paths.push(name.into());
    }
    paths
}

fn distinct(paths: &[&Path]) -> Result<(), MaintenanceError> {
    for (index, path) in paths.iter().enumerate() {
        for other in &paths[index + 1..] {
            for left in family(path) {
                for right in family(other) {
                    if left == right
                        || (left.exists()
                            && right.exists()
                            && same_file::is_same_file(&left, &right)?)
                    {
                        return Err(MaintenanceError::Invalid(
                            "database paths alias or collide with SQLite sidecars",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use rusqlite::types::Value;

    /// All persisted table values, including audit, receipt, identity and allocator.
    fn inventory(connection: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
        let names = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        names
            .into_iter()
            .map(|name| {
                let mut statement = connection
                    .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))
                    .unwrap();
                let count = statement.column_count();
                let values = statement
                    .query_map([], |row| {
                        (0..count)
                            .map(|index| row.get(index))
                            .collect::<rusqlite::Result<Vec<Value>>>()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                (name, values)
            })
            .collect()
    }

    fn assert_prune_rollback(trigger: &str, expected_error: &str) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prune.sqlite");
        let mut connection = super::super::open(&path).unwrap();
        connection
            .execute_batch(
                "INSERT INTO projects VALUES ('bif', 'time');
             INSERT INTO items (item_id, requester, project_id, sequence, title, status,
               revision, captured_at, updated_at) VALUES
               ('ALICE:bif:001', 'ALICE', 'bif', 1, 'Title', 'proposed', 1, 'time', 'time');
             INSERT INTO operations VALUES ('op', 'ALICE:bif:001', 'capture', NULL, 1, 'time');
             INSERT INTO events VALUES
               ('event', 'op', 'ALICE:bif:001', 1, 0, 'captured', NULL, NULL,
                'human', 'ALICE', 'cli', 'local', 'direct', NULL, 'cli', 'local',
                NULL, NULL, 'time', 1);
             INSERT INTO mutation_receipts VALUES
               ('key', 'capture', 'hash', 'op', 'ALICE:bif:001', '{}', 'time');",
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        journal::append_change(&transaction, "op").unwrap();
        transaction.commit().unwrap();
        drop(connection);
        let mut connection = open_existing(&path, true).unwrap();
        let generation = journal::read_metadata(&connection).unwrap().generation;
        let before = inventory(&connection);
        // TEMP leaves exact main-schema validation intact. This is the same core
        // called by public prune; the trigger proves deletion preceded the failure.
        connection.execute_batch(trigger).unwrap();
        let error = prune_opened(&mut connection, &generation, 1).unwrap_err();
        assert!(error.to_string().contains(expected_error), "{error}");
        assert!(connection.is_autocommit());
        drop(connection);
        let connection = super::super::open(&path).unwrap();
        assert_eq!(inventory(&connection), before);
    }

    #[test]
    fn floor_update_abort_after_deletion_rolls_back_every_value_on_fresh_reopen() {
        assert_prune_rollback(
            "CREATE TEMP TRIGGER fail_floor BEFORE UPDATE OF retention_floor ON main.journal_metadata
             BEGIN
               SELECT CASE WHEN (SELECT count(*) FROM change_journal) = 0
                 THEN RAISE(ABORT, 'injected floor failure after deletion')
                 ELSE RAISE(ABORT, 'deletion was not reached') END;
             END;",
            "injected floor failure after deletion",
        );
    }

    #[test]
    fn post_delete_revalidation_failure_rolls_back_rows_floor_and_allocator() {
        assert_prune_rollback(
            "CREATE TEMP TRIGGER damage_allocator AFTER UPDATE OF retention_floor ON main.journal_metadata
             BEGIN
               UPDATE sqlite_sequence SET seq = seq + 1 WHERE name = 'change_journal';
             END;",
            "invalid retained journal history or allocator",
        );
    }
}
