//! Prepared operation journal storage, deliberately not a sync endpoint.
//!
//! Coverage is global within a generation. Callers must not modify the same
//! connection while using a journal read snapshot. Maintenance/restore and
//! activation remain separate work; all supported writers must participate
//! before synchronization can be enabled.

use std::{fmt, num::NonZeroU32};

use rusqlite::{Connection, Transaction, params};

use crate::domain::ProjectId;

use super::ReadSnapshot;

/// Persisted independent 128-bit identity, rendered as lowercase hexadecimal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Generation(String);

impl Generation {
    pub fn parse(value: &str) -> Result<Self, JournalError> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(JournalError::InvalidMetadata);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Durable store-global coverage, independent of retained rows or projects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalMetadata {
    pub generation: Generation,
    pub retention_floor: i64,
    pub high_water: i64,
    pub sync_enabled: bool,
    pub bootstrap_required: bool,
}

/// One effective operation, not one component event or reconstructed summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalChange {
    pub change_seq: i64,
    pub operation_id: String,
    pub item_id: String,
    pub project_id: String,
    pub item_revision: i64,
}

#[derive(Debug)]
pub enum JournalError {
    Sqlite(rusqlite::Error),
    InvalidMetadata,
    GenerationMismatch,
    InvalidCoverage,
    ExpiredCoverage,
    SequenceExhausted,
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "SQLite journal error: {error}"),
            Self::InvalidMetadata => formatter.write_str("missing or malformed journal metadata"),
            Self::GenerationMismatch => formatter.write_str("journal generation changed"),
            Self::InvalidCoverage => formatter.write_str("invalid or future journal coverage"),
            Self::ExpiredCoverage => formatter.write_str("journal coverage expired"),
            Self::SequenceExhausted => {
                formatter.write_str("journal sequence exhausted at i64::MAX")
            }
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for JournalError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Independent draws for migration and every restore. Collision probability for
/// OS-random 128-bit draws is assumed negligible, not mathematically impossible.
/// SQLite randomblob is pseudo-random and is not used for this identity.
pub(super) fn fresh_generation() -> Result<Generation, super::MigrationError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(super::MigrationError::GenerationRandomness)?;
    let mut generation = String::with_capacity(32);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        generation.push(char::from(HEX[usize::from(byte >> 4)]));
        generation.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(Generation(generation))
}

/// Migration-only initialization in the caller's schema transaction.
pub(super) fn initialize(connection: &Connection) -> Result<(), super::MigrationError> {
    let generation = fresh_generation()?;
    connection.execute(
        "INSERT INTO journal_metadata
         (singleton, generation, retention_floor, high_water, sync_enabled, bootstrap_required)
         VALUES (1, ?1, 0, 0, 0, 1)",
        [generation.as_str()],
    )?;
    Ok(())
}

/// Inspect only; never reconstruct H from MAX(change_seq) or repair metadata.
pub(super) fn read_metadata(connection: &Connection) -> Result<JournalMetadata, JournalError> {
    let mut statement = connection.prepare_cached(
        "SELECT singleton, generation, retention_floor, high_water, sync_enabled, bootstrap_required
         FROM journal_metadata LIMIT 2",
    )?;
    let mut rows = statement.query([])?;
    let row = rows.next()?.ok_or(JournalError::InvalidMetadata)?;
    let singleton: i64 = row.get(0)?;
    let generation = Generation::parse(&row.get::<_, String>(1)?)?;
    let retention_floor: i64 = row.get(2)?;
    let high_water: i64 = row.get(3)?;
    let sync_enabled: i64 = row.get(4)?;
    let bootstrap_required: i64 = row.get(5)?;
    if singleton != 1
        || retention_floor < 0
        || high_water < retention_floor
        || !matches!(sync_enabled, 0 | 1)
        || !matches!(bootstrap_required, 0 | 1)
        || rows.next()?.is_some()
    {
        return Err(JournalError::InvalidMetadata);
    }
    Ok(JournalMetadata {
        generation,
        retention_floor,
        high_water,
        sync_enabled: sync_enabled == 1,
        bootstrap_required: bootstrap_required == 1,
    })
}

/// Append in the caller's existing operation/audit/receipt transaction.
///
/// Identity and revision come from the operation, never the current item revision.
/// The insert trigger atomically advances H; this helper never commits, rolls back,
/// or activates the caller's transaction. Duplicate operations are rejected rather
/// than treated as idempotent replay (future writer adapters own replay/no-op rules).
/// Allocation stops at i64::MAX; u64 wire representability is not a SQLite limit.
pub fn append_change(
    transaction: &Transaction<'_>,
    operation_id: &str,
) -> Result<i64, JournalError> {
    if read_metadata(transaction)?.high_water == i64::MAX {
        return Err(JournalError::SequenceExhausted);
    }
    Ok(transaction
        .prepare_cached(
            "INSERT INTO change_journal (operation_id, item_id, project_id, item_revision)
         SELECT o.operation_id, o.item_id, i.project_id, o.item_revision
         FROM operations o JOIN items i ON i.item_id = o.item_id
         WHERE o.operation_id = ?1 RETURNING change_seq",
        )?
        .query_row([operation_id], |row| row.get(0))?)
}

/// Exact storage selection query, exposed only for structural/VM-step evidence.
#[doc(hidden)]
pub const JOURNAL_RANGE_SQL: &str =
    "SELECT change_seq, operation_id, item_id, project_id, item_revision
     FROM change_journal WHERE project_id = ?1 AND change_seq > ?2 AND change_seq <= ?3
     ORDER BY change_seq LIMIT ?4";

/// Owns a read transaction, or reuses the caller's outer ReadSnapshot transaction.
/// The metadata SELECT in begin establishes the snapshot before any row selection.
pub struct JournalSnapshot<'connection> {
    snapshot: ReadSnapshot<'connection>,
    metadata: JournalMetadata,
}

impl<'connection> JournalSnapshot<'connection> {
    pub fn begin(connection: &'connection Connection) -> Result<Self, JournalError> {
        let snapshot = ReadSnapshot::begin(connection)?;
        let metadata = read_metadata(&snapshot)?;
        Ok(Self { snapshot, metadata })
    }

    pub fn metadata(&self) -> &JournalMetadata {
        &self.metadata
    }

    /// Validate generation and F <= C <= H <= durable H before selecting (C,H].
    /// F==C is valid; the floor remains global even for an empty project.
    /// This returns raw bounded rows only: no coalescing, hydration, wire or cursors.
    pub fn select_range(
        &self,
        generation: &Generation,
        project: &ProjectId,
        covered_through: i64,
        high_water: i64,
        limit: NonZeroU32,
    ) -> Result<Vec<JournalChange>, JournalError> {
        if generation != &self.metadata.generation {
            return Err(JournalError::GenerationMismatch);
        }
        if covered_through < 0
            || high_water < covered_through
            || high_water > self.metadata.high_water
        {
            return Err(JournalError::InvalidCoverage);
        }
        if covered_through < self.metadata.retention_floor {
            return Err(JournalError::ExpiredCoverage);
        }
        Ok(self
            .snapshot
            .prepare_cached(JOURNAL_RANGE_SQL)?
            .query_map(
                params![
                    project.as_str(),
                    covered_through,
                    high_water,
                    i64::from(limit.get())
                ],
                |row| {
                    Ok(JournalChange {
                        change_seq: row.get(0)?,
                        operation_id: row.get(1)?,
                        item_id: row.get(2)?,
                        project_id: row.get(3)?,
                        item_revision: row.get(4)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Ends an owned snapshot; an outer caller transaction is never committed.
    pub fn finish(self) -> Result<(), JournalError> {
        Ok(self.snapshot.commit()?)
    }
}
