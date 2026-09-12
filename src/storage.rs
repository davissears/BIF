//! Persistence adapters, transactions, and migrations.
//!
//! This outer module may depend on the application and domain layers; those
//! layers do not depend on storage.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::config::{NormalizedRemoteIdentity, ProjectPathMapping, ProjectRemoteMapping};
use crate::domain::ProjectId;

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_initial.sql");
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const IMMUTABLE_EVENTS: &str = include_str!("../migrations/0002_immutable_events.sql");

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: INITIAL_SCHEMA,
        checksum: "6cce50f20fc62f82af5432d4e2be81c29a0f44a73ca5245353f98c6c6030f85e",
    },
    Migration {
        version: 2,
        name: "immutable_events",
        sql: IMMUTABLE_EVENTS,
        checksum: "1ccf8188fa3ab0c9adbd889c1d80762c9a0d5b3a03189609b204ee05a9c05ced",
    },
];

const CREATE_MIGRATION_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS schema_migrations (
        version INTEGER PRIMARY KEY CHECK (version > 0),
        name TEXT NOT NULL CHECK (length(name) > 0),
        checksum TEXT NOT NULL CHECK (length(checksum) > 0),
        applied_at TEXT NOT NULL CHECK (length(applied_at) > 0)
    );
";

#[derive(Clone, Copy)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
    checksum: &'static str,
}

/// An error encountered while checking or advancing the database schema.
#[derive(Debug)]
pub enum MigrationError {
    /// SQLite could not inspect or update the schema.
    Sqlite(rusqlite::Error),
    /// The database was created by a newer version of BIF.
    NewerSchema { found: i64, supported: i64 },
    /// An applied migration no longer matches the embedded migration.
    ChecksumMismatch {
        version: i64,
        expected: String,
        found: String,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "SQLite migration error: {error}"),
            Self::NewerSchema { found, supported } => write!(
                formatter,
                "database schema version {found} is newer than supported version {supported}"
            ),
            Self::ChecksumMismatch {
                version,
                expected,
                found,
            } => write!(
                formatter,
                "migration {version} checksum mismatch: expected {expected}, found {found}"
            ),
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::NewerSchema { .. } | Self::ChecksumMismatch { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for MigrationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Opens and prepares a SQLite database for storage operations.
///
/// This is the single production connection factory. Every connection is
/// configured before migrations run, and is returned only after the embedded
/// schema is current and validated.
pub fn open(path: impl AsRef<Path>) -> Result<Connection, MigrationError> {
    let mut connection = Connection::open(path)?;
    configure(&connection)?;
    migrate(&mut connection)?;
    Ok(connection)
}

/// Focused persistence operations for project identity registrations.
///
/// Mapping construction remains in [`crate::config`], so canonical-path and
/// remote-normalization rules have one source of truth.
pub struct ProjectRepository<'connection> {
    connection: &'connection mut Connection,
}

impl<'connection> ProjectRepository<'connection> {
    pub fn new(connection: &'connection mut Connection) -> Self {
        Self { connection }
    }

    /// Registers a canonical path. Repeating the same mapping is a no-op.
    pub fn register_path(
        &mut self,
        mapping: &ProjectPathMapping,
    ) -> Result<(), ProjectRegistrationError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let path = mapping.path().to_str().ok_or_else(|| {
            ProjectRegistrationError::UnsupportedPathEncoding(mapping.path().to_path_buf())
        })?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT project_id FROM project_path_mappings WHERE canonical_path = ?1",
                [path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing == mapping.project().as_str() {
                return Ok(());
            }
            return Err(ProjectRegistrationError::PathConflict {
                path: mapping.path().to_path_buf(),
                existing: project_id_from_storage(existing)?,
                requested: mapping.project().clone(),
            });
        }
        insert_project(&transaction, mapping.project())?;
        transaction.execute(
            "INSERT INTO project_path_mappings (canonical_path, project_id) VALUES (?1, ?2)",
            params![path, mapping.project().as_str()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Registers a normalized remote. Repeating the same mapping is a no-op.
    pub fn register_remote(
        &mut self,
        mapping: &ProjectRemoteMapping,
    ) -> Result<(), ProjectRegistrationError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let remote = mapping.identity().as_str();
        let existing: Option<String> = transaction
            .query_row(
                "SELECT project_id FROM project_remote_mappings WHERE normalized_remote = ?1",
                [remote],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing == mapping.project().as_str() {
                return Ok(());
            }
            return Err(ProjectRegistrationError::RemoteConflict {
                remote: remote.to_owned(),
                existing: project_id_from_storage(existing)?,
                requested: mapping.project().clone(),
            });
        }
        insert_project(&transaction, mapping.project())?;
        transaction.execute(
            "INSERT INTO project_remote_mappings (normalized_remote, project_id) VALUES (?1, ?2)",
            params![remote, mapping.project().as_str()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_paths(&self) -> Result<Vec<ProjectPathMapping>, ProjectRegistrationError> {
        let mut statement = self.connection.prepare(
            "SELECT project_id, canonical_path
             FROM project_path_mappings ORDER BY canonical_path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
            ))
        })?;
        rows.map(|row| {
            let (project, path) = row?;
            Ok(ProjectPathMapping::from_canonical(
                project_id_from_storage(project)?,
                path,
            ))
        })
        .collect()
    }

    pub fn list_remotes(&self) -> Result<Vec<ProjectRemoteMapping>, ProjectRegistrationError> {
        let mut statement = self.connection.prepare(
            "SELECT project_id, normalized_remote
             FROM project_remote_mappings ORDER BY normalized_remote",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (project, remote) = row?;
            let project = project_id_from_storage(project)?;
            let identity =
                NormalizedRemoteIdentity::new(&format!("https://{remote}")).map_err(|_| {
                    ProjectRegistrationError::InvalidStoredRegistration {
                        field: "normalized_remote",
                        value: remote.clone(),
                    }
                })?;
            if identity.as_str() != remote {
                return Err(ProjectRegistrationError::InvalidStoredRegistration {
                    field: "normalized_remote",
                    value: remote,
                });
            }
            Ok(ProjectRemoteMapping::from_normalized(project, identity))
        })
        .collect()
    }
}

fn insert_project(
    transaction: &rusqlite::Transaction<'_>,
    project: &ProjectId,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO projects (project_id, created_at)
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        [project.as_str()],
    )?;
    Ok(())
}

fn project_id_from_storage(value: String) -> Result<ProjectId, ProjectRegistrationError> {
    let project = ProjectId::new(&value).map_err(|_| {
        ProjectRegistrationError::InvalidStoredRegistration {
            field: "project_id",
            value: value.clone(),
        }
    })?;
    if project.as_str() != value {
        return Err(ProjectRegistrationError::InvalidStoredRegistration {
            field: "project_id",
            value,
        });
    }
    Ok(project)
}

/// Stable failures returned by project registration storage operations.
#[derive(Debug)]
pub enum ProjectRegistrationError {
    PathConflict {
        path: PathBuf,
        existing: ProjectId,
        requested: ProjectId,
    },
    RemoteConflict {
        remote: String,
        existing: ProjectId,
        requested: ProjectId,
    },
    InvalidStoredRegistration {
        field: &'static str,
        value: String,
    },
    UnsupportedPathEncoding(PathBuf),
    Sqlite(rusqlite::Error),
}

impl fmt::Display for ProjectRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathConflict { path, .. } => {
                write!(
                    formatter,
                    "canonical path already registered: {}",
                    path.display()
                )
            }
            Self::RemoteConflict { remote, .. } => {
                write!(formatter, "normalized remote already registered: {remote}")
            }
            Self::InvalidStoredRegistration { field, value } => {
                write!(formatter, "invalid stored {field}: {value}")
            }
            Self::UnsupportedPathEncoding(path) => write!(
                formatter,
                "canonical path is not valid UTF-8: {}",
                path.display()
            ),
            Self::Sqlite(error) => write!(formatter, "SQLite project registration error: {error}"),
        }
    }
}

impl std::error::Error for ProjectRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for ProjectRegistrationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

fn configure(connection: &Connection) -> rusqlite::Result<()> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    Ok(())
}

/// Checks the migration history and applies all pending embedded migrations.
///
/// Validation, schema changes, and migration bookkeeping share one transaction,
/// so an unsuccessful startup leaves the database at its previous version.
pub fn migrate(connection: &mut Connection) -> Result<(), MigrationError> {
    migrate_all(connection, MIGRATIONS)
}

fn migrate_all(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<(), MigrationError> {
    let supported = migrations.last().map_or(0, |migration| migration.version);
    let transaction = connection.transaction()?;
    transaction.execute_batch(CREATE_MIGRATION_TABLE)?;

    let found = transaction.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if found > supported {
        return Err(MigrationError::NewerSchema { found, supported });
    }

    for migration in migrations {
        let expected = migration.checksum;
        let applied: Option<String> = transaction
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(found) = applied {
            if found != expected {
                return Err(MigrationError::ChecksumMismatch {
                    version: migration.version,
                    expected: expected.to_owned(),
                    found,
                });
            }
            continue;
        }

        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at)
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            params![migration.version, migration.name, expected],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUCCESSFUL: Migration = Migration {
        version: 1,
        name: "successful",
        sql: "CREATE TABLE retained (value TEXT NOT NULL);",
        checksum: "successful-checksum",
    };
    const FAILING: Migration = Migration {
        version: 2,
        name: "failing",
        sql: "
            CREATE TABLE must_roll_back (value TEXT NOT NULL);
            INSERT INTO table_that_does_not_exist VALUES (1);
        ",
        checksum: "failing-checksum",
    };

    #[test]
    fn failed_migration_rolls_back_all_changes_from_startup() {
        let mut connection = Connection::open_in_memory().unwrap();

        assert!(migrate_all(&mut connection, &[SUCCESSFUL, FAILING]).is_err());

        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema
                 WHERE type = 'table'
                   AND name IN ('schema_migrations', 'retained', 'must_roll_back')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }
}
