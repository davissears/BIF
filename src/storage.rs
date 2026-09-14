//! Persistence adapters, transactions, and migrations.
//!
//! This outer module may depend on the application and domain layers; those
//! layers do not depend on storage.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::application::{
    Actor, ActorKind, CaptureIdentity, CaptureRequest, CaptureResult, CaptureStore,
    CaptureStoreError, EventActor, EventExecution, Execution, ItemHistoryEvent, ItemHistoryStore,
    ItemHistoryStoreError, ItemListFilters, ItemStore, MutationIdentity, MutationRequest,
    MutationResult, MutationStore, MutationStoreError, NamedViewStore,
};
use crate::config::{NormalizedRemoteIdentity, ProjectPathMapping, ProjectRemoteMapping};
use crate::domain::{
    AssigneeId, DomainEvent, EventType, EventValue, Item, ItemContent, ItemId, ItemMutation,
    MessageId, NamedView, Priority, ProjectId, Provenance, RepositoryReference, RequesterId,
    Revision, RevisionReference, SourceHost, SourceUrl, Status, ThreadId, Timestamp,
};

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
    open_with_flags(path, rusqlite::OpenFlags::default())
}

/// Opens and prepares a SQLite database without following symbolic links in
/// any component of its filename.
///
/// This is used when a database must temporarily be addressed through an
/// ambient path but the caller separately owns the containing directory by
/// capability.
pub fn open_nofollow(path: impl AsRef<Path>) -> Result<Connection, MigrationError> {
    open_with_flags(
        path,
        rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
}

fn open_with_flags(
    path: impl AsRef<Path>,
    flags: rusqlite::OpenFlags,
) -> Result<Connection, MigrationError> {
    let mut connection = Connection::open_with_flags(path, flags)?;
    configure(&connection)?;
    migrate(&mut connection)?;
    Ok(connection)
}

/// SQLite implementation of the read-one-item persistence boundary.
pub struct ItemRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> ItemRepository<'connection> {
    pub fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }
}

/// Failures while loading canonical item state.
#[derive(Debug)]
pub enum ItemStorageError {
    Sqlite(rusqlite::Error),
    InvalidPersistedData { item_id: String, detail: String },
}

impl fmt::Display for ItemStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "SQLite item read error: {error}"),
            Self::InvalidPersistedData { item_id, detail } => {
                write!(
                    formatter,
                    "invalid persisted data for item {item_id}: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for ItemStorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::InvalidPersistedData { .. } => None,
        }
    }
}

impl crate::rpc_read::ClassifyReadStorageError for ItemStorageError {
    fn rpc_read_kind(&self) -> crate::rpc_read::ReadStorageErrorKind {
        match self {
            Self::Sqlite(error) if is_busy(error) => crate::rpc_read::ReadStorageErrorKind::Busy,
            Self::InvalidPersistedData { .. } => {
                crate::rpc_read::ReadStorageErrorKind::InvalidPersistedData
            }
            Self::Sqlite(_) => crate::rpc_read::ReadStorageErrorKind::Other,
        }
    }
}

impl From<rusqlite::Error> for ItemStorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl ItemStore for ItemRepository<'_> {
    type Error = ItemStorageError;

    fn read_item(&self, item_id: &ItemId) -> Result<Option<Item>, Self::Error> {
        load_item(self.connection, &item_id.to_string())
    }
}

impl NamedViewStore for ItemRepository<'_> {
    type Error = ItemStorageError;

    fn select_items(
        &self,
        view: NamedView,
        configured_requester: &RequesterId,
        filters: &ItemListFilters,
    ) -> Result<Vec<Item>, Self::Error> {
        let predicate = match view {
            NamedView::Proposed => "i.status = 'proposed'",
            NamedView::Ready => "i.status = 'ready'",
            NamedView::Active => "i.status IN ('in_progress', 'blocked')",
            NamedView::Blocked => "i.status = 'blocked'",
            NamedView::Done => "i.status = 'done'",
            NamedView::Rejected => "i.status = 'rejected'",
            NamedView::Mine => "i.status NOT IN ('done', 'rejected') AND i.assignee = ?",
            NamedView::All => "1 = 1",
        };
        let mut predicates = vec![predicate];
        let mut parameters = Vec::<String>::new();
        if view == NamedView::Mine {
            parameters.push(configured_requester.as_str().to_ascii_lowercase());
        }
        if let Some(project) = &filters.project {
            predicates.push("i.project_id = ?");
            parameters.push(project.to_string());
        }
        if let Some(requester) = &filters.requester {
            predicates.push("i.requester = ?");
            parameters.push(requester.to_string());
        }
        if let Some(assignee) = &filters.assignee {
            predicates.push("i.assignee = ?");
            parameters.push(assignee.to_string());
        }
        if let Some(status) = filters.status {
            predicates.push("i.status = ?");
            parameters.push(crate::storage::status(status).to_owned());
        }
        if let Some(priority) = filters.priority {
            predicates.push("i.priority = ?");
            parameters.push(crate::storage::priority(priority).to_owned());
        }
        if filters.unassigned {
            predicates.push("i.assignee IS NULL");
        }
        if let Some(text) = &filters.text {
            predicates.push(
                "(instr(lower(i.title), lower(?)) > 0 \
                 OR instr(lower(coalesce(i.description, '')), lower(?)) > 0 \
                 OR EXISTS (SELECT 1 FROM item_acceptance_criteria AS c \
                            WHERE c.item_id = i.item_id \
                            AND instr(lower(c.criterion), lower(?)) > 0))",
            );
            parameters.extend(std::iter::repeat_n(text.as_str().to_owned(), 3));
        }
        let sql = format!(
            "SELECT i.item_id FROM items AS i WHERE {}",
            predicates.join(" AND ")
        );
        let item_ids = {
            let mut statement = self.connection.prepare(&sql)?;
            statement
                .query_map(rusqlite::params_from_iter(parameters), |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        item_ids
            .into_iter()
            .map(|item_id| {
                load_item(self.connection, &item_id)?.ok_or_else(|| {
                    invalid_item(&item_id, "item disappeared during named view selection")
                })
            })
            .collect()
    }
}

/// SQLite implementation of the atomic capture persistence boundary.
pub struct CaptureRepository<'connection> {
    connection: &'connection mut Connection,
}

impl<'connection> CaptureRepository<'connection> {
    pub fn new(connection: &'connection mut Connection) -> Self {
        Self { connection }
    }
}

/// Stable failures returned by capture persistence.
#[derive(Debug)]
pub enum CaptureStorageError {
    SequenceExhausted,
    Sqlite(rusqlite::Error),
}

impl fmt::Display for CaptureStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SequenceExhausted => formatter.write_str("item sequence is exhausted"),
            Self::Sqlite(error) => write!(formatter, "SQLite capture error: {error}"),
        }
    }
}

impl std::error::Error for CaptureStorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SequenceExhausted => None,
            Self::Sqlite(error) => Some(error),
        }
    }
}

impl From<rusqlite::Error> for CaptureStorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<rusqlite::Error> for CaptureStoreError<CaptureStorageError> {
    fn from(error: rusqlite::Error) -> Self {
        let busy = is_busy(&error);
        let error = CaptureStorageError::Sqlite(error);
        if busy {
            Self::Busy(error)
        } else {
            Self::Storage(error)
        }
    }
}

impl CaptureStore for CaptureRepository<'_> {
    type Error = CaptureStorageError;

    fn capture(
        &mut self,
        request: CaptureRequest,
        payload_hash: String,
        actor: Actor<'_>,
        execution: Execution<'_>,
        occurred_at: Timestamp,
        identity: CaptureIdentity,
    ) -> Result<CaptureResult, CaptureStoreError<Self::Error>> {
        self.capture_inner(
            request,
            payload_hash,
            actor,
            execution,
            occurred_at,
            identity,
        )
    }
}

impl From<rusqlite::Error> for MutationStoreError<rusqlite::Error> {
    fn from(error: rusqlite::Error) -> Self {
        if is_busy(&error) {
            Self::Busy(error)
        } else {
            Self::Storage(error)
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

impl CaptureRepository<'_> {
    fn capture_inner(
        &mut self,
        request: CaptureRequest,
        payload_hash: String,
        actor: Actor<'_>,
        execution: Execution<'_>,
        occurred_at: Timestamp,
        identity: CaptureIdentity,
    ) -> Result<CaptureResult, CaptureStoreError<CaptureStorageError>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(CaptureStoreError::from)?;
        let receipt: Option<(String, String)> = transaction
            .query_row(
                "SELECT payload_hash, item_id FROM mutation_receipts
                 WHERE mutation_key = ?1",
                [&request.idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(CaptureStoreError::from)?;
        if let Some((stored_hash, item_id)) = receipt {
            if stored_hash != payload_hash {
                return Err(CaptureStoreError::IdempotencyConflict);
            }
            let (sequence, captured_at): (i64, String) = transaction
                .query_row(
                    "SELECT sequence, captured_at FROM items WHERE item_id = ?1",
                    [item_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(CaptureStoreError::from)?;
            let input = request.input;
            let item_id = ItemId::new(input.requester, input.project, sequence as u64)
                .expect("a persisted positive sequence is a valid domain sequence");
            let timestamp = Timestamp::new(captured_at);
            return Ok(CaptureResult {
                item: Item::capture(
                    item_id,
                    input.content,
                    input.provenance,
                    timestamp.clone(),
                    timestamp,
                ),
                replayed: true,
            });
        }

        let input = request.input;
        insert_project_at(&transaction, &input.project, occurred_at.as_str())?;

        let current: Option<i64> = transaction
            .query_row(
                "SELECT next_sequence FROM requester_project_counters
                 WHERE requester = ?1 AND project_id = ?2",
                params![input.requester.as_str(), input.project.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let sequence = match current {
            None => {
                transaction.execute(
                    "INSERT INTO requester_project_counters
                     (requester, project_id, next_sequence) VALUES (?1, ?2, 2)",
                    params![input.requester.as_str(), input.project.as_str()],
                )?;
                1_i64
            }
            Some(i64::MAX) => {
                return Err(CaptureStoreError::Storage(
                    CaptureStorageError::SequenceExhausted,
                ));
            }
            Some(sequence) => {
                transaction.execute(
                    "UPDATE requester_project_counters SET next_sequence = ?3
                     WHERE requester = ?1 AND project_id = ?2",
                    params![
                        input.requester.as_str(),
                        input.project.as_str(),
                        sequence + 1
                    ],
                )?;
                sequence
            }
        };
        let item_id = ItemId::new(
            input.requester.clone(),
            input.project.clone(),
            sequence as u64,
        )
        .expect("a positive SQLite sequence is a valid domain sequence");
        let item = Item::capture(
            item_id,
            input.content,
            input.provenance,
            occurred_at.clone(),
            occurred_at,
        );
        insert_capture(&transaction, &item, actor, execution, &identity)?;
        transaction.execute(
            "INSERT INTO mutation_receipts (
                mutation_key, mutation_type, payload_hash, operation_id, item_id,
                response_json, created_at
             ) VALUES (?1, 'capture', ?2, ?3, ?4, ?5, ?6)",
            params![
                request.idempotency_key,
                payload_hash,
                identity.operation_id,
                item.id().to_string(),
                format!("{{\"item_id\":\"{}\"}}", item.id()),
                item.captured_at().as_str(),
            ],
        )?;
        transaction.commit()?;
        Ok(CaptureResult {
            item,
            replayed: false,
        })
    }
}

/// SQLite implementation of immutable item-history reads.
pub struct ItemHistoryRepository<'connection> {
    connection: &'connection Connection,
}

impl<'connection> ItemHistoryRepository<'connection> {
    pub fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }
}

/// A storage or data-integrity failure encountered while reading history.
#[derive(Debug)]
pub enum ItemHistoryStorageError {
    InvalidPersistedData { field: &'static str, value: String },
    Sqlite(rusqlite::Error),
}

impl fmt::Display for ItemHistoryStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPersistedData { field, value } => {
                write!(formatter, "invalid {field}: {value:?}")
            }
            Self::Sqlite(error) => write!(formatter, "SQLite item history error: {error}"),
        }
    }
}

impl std::error::Error for ItemHistoryStorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPersistedData { .. } => None,
            Self::Sqlite(error) => Some(error),
        }
    }
}

impl crate::rpc_read::ClassifyReadStorageError for ItemHistoryStorageError {
    fn rpc_read_kind(&self) -> crate::rpc_read::ReadStorageErrorKind {
        match self {
            Self::Sqlite(error) if is_busy(error) => crate::rpc_read::ReadStorageErrorKind::Busy,
            Self::InvalidPersistedData { .. } => {
                crate::rpc_read::ReadStorageErrorKind::InvalidPersistedData
            }
            Self::Sqlite(_) => crate::rpc_read::ReadStorageErrorKind::Other,
        }
    }
}

impl ItemHistoryStore for ItemHistoryRepository<'_> {
    type Error = ItemHistoryStorageError;

    fn item_history(
        &self,
        item_id: &ItemId,
    ) -> Result<Vec<ItemHistoryEvent>, ItemHistoryStoreError<Self::Error>> {
        let item_id = item_id.to_string();
        let exists = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM items WHERE item_id = ?1)",
                [&item_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(history_sqlite)?;
        if !exists {
            return Err(ItemHistoryStoreError::NotFound);
        }

        let mut statement = self
            .connection
            .prepare(
                "SELECT operation_id, event_id, item_revision, event_index, event_type,
                        before_value, after_value, actor_kind, actor_id, actor_surface,
                        actor_host, execution_kind, execution_agent_id, execution_surface,
                        execution_host, reason, note, occurred_at, event_schema_version
                 FROM events
                 WHERE item_id = ?1
                 ORDER BY item_revision ASC, event_index ASC",
            )
            .map_err(history_sqlite)?;
        type HistoryRow = (
            String,
            String,
            i64,
            i64,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            i64,
        );
        let rows = statement
            .query_map([item_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    row.get(18)?,
                ))
            })
            .map_err(history_sqlite)?;

        rows.map(|row| {
            row.map_err(history_sqlite)
                .and_then(|row: HistoryRow| history_event(row))
        })
        .collect()
    }
}

fn history_sqlite(error: rusqlite::Error) -> ItemHistoryStoreError<ItemHistoryStorageError> {
    ItemHistoryStoreError::Storage(ItemHistoryStorageError::Sqlite(error))
}

fn invalid_history(
    field: &'static str,
    value: impl Into<String>,
) -> ItemHistoryStoreError<ItemHistoryStorageError> {
    ItemHistoryStoreError::InvalidPersistedData(ItemHistoryStorageError::InvalidPersistedData {
        field,
        value: value.into(),
    })
}

fn history_event(
    row: (
        String,
        String,
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        i64,
    ),
) -> Result<ItemHistoryEvent, ItemHistoryStoreError<ItemHistoryStorageError>> {
    let revision = Revision::new(
        u64::try_from(row.2).map_err(|_| invalid_history("item_revision", row.2.to_string()))?,
    )
    .map_err(|_| invalid_history("item_revision", row.2.to_string()))?;
    let event_index =
        u64::try_from(row.3).map_err(|_| invalid_history("event_index", row.3.to_string()))?;
    let event_type = parse_history_event_type(&row.4)?;
    let before = parse_history_value(event_type, "before_value", row.5)?;
    let after = parse_history_value(event_type, "after_value", row.6)?;
    let actor_kind = match row.7.as_str() {
        "human" => ActorKind::Human,
        "agent" => ActorKind::Agent,
        _ => return Err(invalid_history("actor_kind", row.7)),
    };
    let execution = match row.11.as_str() {
        "direct" if row.12.is_none() => EventExecution::Direct {
            surface: row.13,
            host: row.14,
        },
        "agent" => EventExecution::Agent {
            agent_id: row
                .12
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid_history("execution_agent_id", "NULL"))?,
            surface: row.13,
            host: row.14,
        },
        _ => return Err(invalid_history("execution_kind", row.11)),
    };
    let schema_version = u64::try_from(row.18)
        .map_err(|_| invalid_history("event_schema_version", row.18.to_string()))?;
    if schema_version != 1 {
        return Err(invalid_history(
            "event_schema_version",
            schema_version.to_string(),
        ));
    }
    Ok(ItemHistoryEvent {
        operation_id: row.0,
        event_id: row.1,
        item_revision: revision,
        event_index,
        event_type,
        before,
        after,
        actor: EventActor {
            kind: actor_kind,
            id: row.8,
            surface: row.9,
            host: row.10,
        },
        execution,
        reason: row.15,
        note: row.16,
        occurred_at: Timestamp::new(row.17),
        schema_version,
    })
}

fn parse_history_event_type(
    value: &str,
) -> Result<EventType, ItemHistoryStoreError<ItemHistoryStorageError>> {
    match value {
        "captured" => Ok(EventType::Captured),
        "approved" => Ok(EventType::Approved),
        "rejected" => Ok(EventType::Rejected),
        "started" => Ok(EventType::Started),
        "blocked" => Ok(EventType::Blocked),
        "resumed" => Ok(EventType::Resumed),
        "finished" => Ok(EventType::Finished),
        "priority_changed" => Ok(EventType::PriorityChanged),
        "assignee_changed" => Ok(EventType::AssigneeChanged),
        "note_added" => Ok(EventType::NoteAdded),
        _ => Err(invalid_history("event_type", value)),
    }
}

fn parse_history_value(
    event_type: EventType,
    field: &'static str,
    value: Option<String>,
) -> Result<Option<EventValue>, ItemHistoryStoreError<ItemHistoryStorageError>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match event_type {
        EventType::Captured => return Err(invalid_history(field, value)),
        EventType::Approved
        | EventType::Rejected
        | EventType::Started
        | EventType::Blocked
        | EventType::Resumed
        | EventType::Finished => EventValue::Status(
            parse_history_status(&value).ok_or_else(|| invalid_history(field, &value))?,
        ),
        EventType::PriorityChanged => EventValue::Priority(if value == "null" {
            None
        } else {
            Some(parse_history_priority(&value).ok_or_else(|| invalid_history(field, &value))?)
        }),
        EventType::AssigneeChanged => EventValue::Assignee(if value == "null" {
            None
        } else {
            Some(AssigneeId::new(&value).map_err(|_| invalid_history(field, &value))?)
        }),
        EventType::NoteAdded => EventValue::Note(value),
    };
    Ok(Some(parsed))
}

fn parse_history_status(value: &str) -> Option<Status> {
    match value {
        "proposed" => Some(Status::Proposed),
        "ready" => Some(Status::Ready),
        "in_progress" => Some(Status::InProgress),
        "blocked" => Some(Status::Blocked),
        "done" => Some(Status::Done),
        "rejected" => Some(Status::Rejected),
        _ => None,
    }
}

fn parse_history_priority(value: &str) -> Option<Priority> {
    match value {
        "P0" => Some(Priority::P0),
        "P1" => Some(Priority::P1),
        "P2" => Some(Priority::P2),
        "P3" => Some(Priority::P3),
        "P4" => Some(Priority::P4),
        _ => None,
    }
}

/// SQLite implementation of atomic item mutation persistence.
pub struct MutationRepository<'connection> {
    connection: &'connection mut Connection,
}

impl<'connection> MutationRepository<'connection> {
    pub fn new(connection: &'connection mut Connection) -> Self {
        Self { connection }
    }
}

impl MutationStore for MutationRepository<'_> {
    type Error = rusqlite::Error;

    fn mutate(
        &mut self,
        request: MutationRequest,
        payload_hash: String,
        actor: Actor<'_>,
        execution: Execution<'_>,
        occurred_at: Timestamp,
        identity: MutationIdentity,
    ) -> Result<MutationResult, MutationStoreError<Self::Error>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MutationStoreError::from)?;
        let receipt: Option<(String, String)> = transaction
            .query_row(
                "SELECT payload_hash, item_id FROM mutation_receipts WHERE mutation_key = ?1",
                [&request.idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(MutationStoreError::from)?;
        if let Some((stored_hash, stored_item_id)) = receipt {
            if stored_hash != payload_hash {
                return Err(MutationStoreError::IdempotencyConflict);
            }
            let item = load_item(&transaction, &stored_item_id)
                .map_err(mutation_load_error)?
                .ok_or(MutationStoreError::NotFound)?;
            return Ok(MutationResult {
                item,
                replayed: true,
            });
        }
        let item_id = &request.item_id;
        let expected_revision = request.expected_revision;
        let mutation = request.mutation;
        let mut item = load_item(&transaction, &item_id.to_string())
            .map_err(mutation_load_error)?
            .ok_or(MutationStoreError::NotFound)?;
        if item.revision() != expected_revision {
            return Err(MutationStoreError::VersionConflict);
        }
        let reason = mutation_reason(&mutation).map(str::to_owned);
        let events = item
            .apply_mutation(mutation)
            .map_err(MutationStoreError::Invalid)?;
        if events.is_empty() {
            return Ok(MutationResult {
                item,
                replayed: false,
            });
        }
        if identity.event_ids.len() != events.len() {
            return Err(MutationStoreError::Storage(
                rusqlite::Error::InvalidParameterCount(identity.event_ids.len(), events.len()),
            ));
        }
        item.set_updated_at(occurred_at.clone());
        transaction
            .execute(
                "UPDATE items SET status = ?2, priority = ?3, assignee = ?4,
                    status_reason = ?5, revision = ?6, updated_at = ?7
                 WHERE item_id = ?1",
                params![
                    item.id().to_string(),
                    status(item.status()),
                    item.priority().map(priority),
                    item.assignee().map(|value| value.as_str()),
                    item.status_reason(),
                    item.revision().get() as i64,
                    occurred_at.as_str(),
                ],
            )
            .map_err(MutationStoreError::from)?;
        transaction
            .execute(
                "INSERT INTO operations (
                    operation_id, item_id, operation_type, expected_revision,
                    item_revision, occurred_at
                 ) VALUES (?1, ?2, 'triage', ?3, ?4, ?5)",
                params![
                    identity.operation_id,
                    item.id().to_string(),
                    expected_revision.get() as i64,
                    item.revision().get() as i64,
                    occurred_at.as_str()
                ],
            )
            .map_err(MutationStoreError::from)?;
        for (index, (event, event_id)) in events.iter().zip(identity.event_ids.iter()).enumerate() {
            insert_mutation_event(
                &transaction,
                &identity.operation_id,
                event_id,
                item.id(),
                index,
                event,
                actor,
                execution,
                reason.as_deref(),
                &occurred_at,
            )
            .map_err(MutationStoreError::from)?;
        }
        transaction
            .execute(
                "INSERT INTO mutation_receipts (
                    mutation_key, mutation_type, payload_hash, operation_id, item_id,
                    response_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    request.idempotency_key,
                    mutation_type(&events),
                    payload_hash,
                    identity.operation_id,
                    item.id().to_string(),
                    format!(
                        "{{\"item_id\":\"{}\",\"revision\":{}}}",
                        item.id(),
                        item.revision().get()
                    ),
                    occurred_at.as_str(),
                ],
            )
            .map_err(MutationStoreError::from)?;
        transaction.commit().map_err(MutationStoreError::from)?;
        Ok(MutationResult {
            item,
            replayed: false,
        })
    }
}

fn mutation_type(events: &[DomainEvent]) -> &'static str {
    if events.len() > 1 {
        return "triage";
    }
    match events.first().map(|event| event.event_type) {
        Some(EventType::Approved) => "approve",
        Some(EventType::Rejected) => "reject",
        Some(EventType::PriorityChanged) => "prioritize",
        Some(EventType::AssigneeChanged) => "assign",
        Some(EventType::Started) => "start",
        Some(EventType::Blocked) => "block",
        Some(EventType::Resumed) => "resume",
        Some(EventType::Finished) => "finish",
        Some(EventType::NoteAdded) | Some(EventType::Captured) | None => "triage",
    }
}

fn load_item(connection: &Connection, item_id: &str) -> Result<Option<Item>, ItemStorageError> {
    type ItemRow = (
        String,
        String,
        i64,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<ItemRow> = connection
        .query_row(
            "SELECT i.requester, i.project_id, i.sequence, i.title, i.description,
                    i.status, i.priority, i.assignee, i.status_reason, i.revision,
                    i.captured_at, i.updated_at, p.source_host, p.thread_id, p.message_id,
                    p.url, p.repository_reference, p.revision_reference, p.context_excerpt
             FROM items AS i JOIN item_provenance AS p USING (item_id)
             WHERE i.item_id = ?1",
            [item_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    row.get(18)?,
                ))
            },
        )
        .optional()
        .map_err(ItemStorageError::from)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let criteria = connection
        .prepare(
            "SELECT criterion FROM item_acceptance_criteria
             WHERE item_id = ?1 ORDER BY criterion_index",
        )
        .and_then(|mut statement| {
            statement
                .query_map([item_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(ItemStorageError::from)?;
    let invalid = |detail: &str| invalid_item(item_id, detail);
    let requester =
        RequesterId::new(&row.0).map_err(|_| invalid("requester is not a valid RequesterId"))?;
    let project =
        ProjectId::new(&row.1).map_err(|_| invalid("project_id is not a valid ProjectId"))?;
    let sequence =
        u64::try_from(row.2).map_err(|_| invalid("sequence is not a positive integer"))?;
    let id = ItemId::new(requester, project, sequence)
        .map_err(|_| invalid("identity components do not form a valid ItemId"))?;
    if id.to_string() != item_id {
        return Err(invalid("item_id does not match its identity components"));
    }
    let content = ItemContent::new(row.3, row.4, criteria)
        .map_err(|_| invalid("content violates canonical item constraints"))?;
    let provenance = Provenance::new(
        row.12
            .as_deref()
            .map(|value| parse_source_host(value, item_id))
            .transpose()?,
        row.13.map(ThreadId::new),
        row.14.map(MessageId::new),
        row.15.map(SourceUrl::new),
        row.16.map(RepositoryReference::new),
        row.17.map(RevisionReference::new),
        row.18,
    );
    Ok(Some(Item::new(
        id,
        content,
        parse_status(&row.5, item_id)?,
        row.6
            .as_deref()
            .map(|value| parse_priority(value, item_id))
            .transpose()?,
        row.7
            .map(AssigneeId::new)
            .transpose()
            .map_err(|_| invalid("assignee is not a valid AssigneeId"))?,
        row.8,
        Revision::new(
            u64::try_from(row.9).map_err(|_| invalid("revision is not a positive integer"))?,
        )
        .map_err(|_| invalid("revision is not a positive integer"))?,
        Timestamp::new(row.10),
        Timestamp::new(row.11),
        provenance,
    )))
}

fn invalid_item(item_id: &str, detail: &str) -> ItemStorageError {
    ItemStorageError::InvalidPersistedData {
        item_id: item_id.to_owned(),
        detail: detail.to_owned(),
    }
}

fn mutation_load_error(error: ItemStorageError) -> MutationStoreError<rusqlite::Error> {
    match error {
        ItemStorageError::Sqlite(error) => MutationStoreError::from(error),
        error @ ItemStorageError::InvalidPersistedData { .. } => {
            MutationStoreError::Storage(rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
        }
    }
}

fn mutation_reason(mutation: &ItemMutation) -> Option<&str> {
    match mutation.lifecycle.as_ref() {
        Some(crate::domain::LifecycleMutation::Reject { reason })
        | Some(crate::domain::LifecycleMutation::Block { reason }) => Some(reason),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_mutation_event(
    transaction: &rusqlite::Transaction<'_>,
    operation_id: &str,
    event_id: &str,
    item_id: &ItemId,
    index: usize,
    event: &DomainEvent,
    actor: Actor<'_>,
    execution: Execution<'_>,
    reason: Option<&str>,
    occurred_at: &Timestamp,
) -> rusqlite::Result<()> {
    let (execution_kind, execution_agent_id, execution_surface, execution_host) = match execution {
        Execution::Direct { surface, host } => ("direct", None, surface, host),
        Execution::Agent {
            agent_id,
            surface,
            host,
        } => ("agent", Some(agent_id), surface, host),
    };
    let note = match &event.after {
        Some(EventValue::Note(note)) => Some(note.as_str()),
        _ => None,
    };
    transaction.execute(
        "INSERT INTO events (
            event_id, operation_id, item_id, item_revision, event_index, event_type,
            before_value, after_value, actor_kind, actor_id, actor_surface, actor_host,
            execution_kind, execution_agent_id, execution_surface, execution_host,
            reason, note, occurred_at, event_schema_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, ?17, ?18, ?19, 1)",
        params![
            event_id,
            operation_id,
            item_id.to_string(),
            event.item_revision.get() as i64,
            index as i64,
            event_type(event.event_type),
            event.before.as_ref().map(event_value),
            event.after.as_ref().map(event_value),
            match actor.kind {
                ActorKind::Human => "human",
                ActorKind::Agent => "agent",
            },
            actor.id,
            actor.surface,
            actor.host,
            execution_kind,
            execution_agent_id,
            execution_surface,
            execution_host,
            reason.filter(|_| matches!(event.event_type, EventType::Rejected | EventType::Blocked)),
            note,
            occurred_at.as_str(),
        ],
    )?;
    Ok(())
}

fn event_value(value: &EventValue) -> String {
    match value {
        EventValue::Status(value) => status(*value).to_owned(),
        EventValue::Priority(value) => value.map(priority).unwrap_or("null").to_owned(),
        EventValue::Assignee(value) => value
            .as_ref()
            .map(AssigneeId::as_str)
            .unwrap_or("null")
            .to_owned(),
        EventValue::Note(value) => value.clone(),
    }
}

fn event_type(value: EventType) -> &'static str {
    match value {
        EventType::Captured => "captured",
        EventType::Approved => "approved",
        EventType::Rejected => "rejected",
        EventType::Started => "started",
        EventType::Blocked => "blocked",
        EventType::Resumed => "resumed",
        EventType::Finished => "finished",
        EventType::PriorityChanged => "priority_changed",
        EventType::AssigneeChanged => "assignee_changed",
        EventType::NoteAdded => "note_added",
    }
}

fn status(value: Status) -> &'static str {
    match value {
        Status::Proposed => "proposed",
        Status::Ready => "ready",
        Status::InProgress => "in_progress",
        Status::Blocked => "blocked",
        Status::Done => "done",
        Status::Rejected => "rejected",
    }
}

fn priority(value: Priority) -> &'static str {
    match value {
        Priority::P0 => "P0",
        Priority::P1 => "P1",
        Priority::P2 => "P2",
        Priority::P3 => "P3",
        Priority::P4 => "P4",
    }
}

fn parse_status(value: &str, item_id: &str) -> Result<Status, ItemStorageError> {
    match value {
        "proposed" => Ok(Status::Proposed),
        "ready" => Ok(Status::Ready),
        "in_progress" => Ok(Status::InProgress),
        "blocked" => Ok(Status::Blocked),
        "done" => Ok(Status::Done),
        "rejected" => Ok(Status::Rejected),
        _ => Err(invalid_item(
            item_id,
            &format!("unknown status value {value:?}"),
        )),
    }
}

fn parse_priority(value: &str, item_id: &str) -> Result<Priority, ItemStorageError> {
    match value {
        "P0" => Ok(Priority::P0),
        "P1" => Ok(Priority::P1),
        "P2" => Ok(Priority::P2),
        "P3" => Ok(Priority::P3),
        "P4" => Ok(Priority::P4),
        _ => Err(invalid_item(
            item_id,
            &format!("unknown priority value {value:?}"),
        )),
    }
}

fn parse_source_host(value: &str, item_id: &str) -> Result<SourceHost, ItemStorageError> {
    match value {
        "delta" => Ok(SourceHost::Delta),
        "codex" => Ok(SourceHost::Codex),
        "local" => Ok(SourceHost::Local),
        _ => Err(invalid_item(
            item_id,
            &format!("unknown source_host value {value:?}"),
        )),
    }
}

fn insert_capture(
    transaction: &rusqlite::Transaction<'_>,
    item: &Item,
    actor: Actor<'_>,
    execution: Execution<'_>,
    identity: &CaptureIdentity,
) -> rusqlite::Result<()> {
    let id = item.id().to_string();
    transaction.execute(
        "INSERT INTO items (
            item_id, requester, project_id, sequence, title, description, status,
            priority, assignee, status_reason, revision, captured_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'proposed', NULL, NULL, NULL, 1, ?7, ?7)",
        params![
            id,
            item.id().requester().as_str(),
            item.id().project().as_str(),
            item.id().sequence() as i64,
            item.content().title(),
            item.content().description(),
            item.captured_at().as_str(),
        ],
    )?;
    for (index, criterion) in item.content().acceptance_criteria().iter().enumerate() {
        transaction.execute(
            "INSERT INTO item_acceptance_criteria (item_id, criterion_index, criterion)
             VALUES (?1, ?2, ?3)",
            params![id, index as i64, criterion],
        )?;
    }
    let provenance = item.provenance();
    transaction.execute(
        "INSERT INTO item_provenance (
            item_id, source_host, thread_id, message_id, url,
            repository_reference, revision_reference, context_excerpt
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            provenance.source_host().map(source_host),
            provenance.thread_id().map(|value| value.as_str()),
            provenance.message_id().map(|value| value.as_str()),
            provenance.url().map(|value| value.as_str()),
            provenance
                .repository_reference()
                .map(|value| value.as_str()),
            provenance.revision_reference().map(|value| value.as_str()),
            provenance.context_excerpt(),
        ],
    )?;
    transaction.execute(
        "INSERT INTO operations (
            operation_id, item_id, operation_type, expected_revision, item_revision, occurred_at
         ) VALUES (?1, ?2, 'capture', NULL, 1, ?3)",
        params![identity.operation_id, id, item.captured_at().as_str()],
    )?;
    let (execution_kind, execution_agent_id, execution_surface, execution_host) = match execution {
        Execution::Direct { surface, host } => ("direct", None, surface, host),
        Execution::Agent {
            agent_id,
            surface,
            host,
        } => ("agent", Some(agent_id), surface, host),
    };
    transaction.execute(
        "INSERT INTO events (
            event_id, operation_id, item_id, item_revision, event_index, event_type,
            before_value, after_value, actor_kind, actor_id, actor_surface, actor_host,
            execution_kind, execution_agent_id, execution_surface, execution_host,
            reason, note, occurred_at, event_schema_version
         ) VALUES (
            ?1, ?2, ?3, 1, 0, 'captured', NULL, NULL, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10, ?11, NULL, NULL, ?12, 1
         )",
        params![
            identity.event_id,
            identity.operation_id,
            id,
            match actor.kind {
                ActorKind::Human => "human",
                ActorKind::Agent => "agent",
            },
            actor.id,
            actor.surface,
            actor.host,
            execution_kind,
            execution_agent_id,
            execution_surface,
            execution_host,
            item.captured_at().as_str(),
        ],
    )?;
    Ok(())
}

fn source_host(host: SourceHost) -> &'static str {
    match host {
        SourceHost::Delta => "delta",
        SourceHost::Codex => "codex",
        SourceHost::Local => "local",
    }
}

fn insert_project_at(
    transaction: &rusqlite::Transaction<'_>,
    project: &ProjectId,
    created_at: &str,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO projects (project_id, created_at) VALUES (?1, ?2)",
        params![project.as_str(), created_at],
    )?;
    Ok(())
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
