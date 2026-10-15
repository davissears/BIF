//! Reusable, sequential v2 reads with pinned configuration and store identity.
//!
//! Identity checks and repository selection share a SQLite snapshot. Delivery
//! happens after it ends, followed by a fresh boundary check before returning.
//! No selected project, result, cursor state or read transaction survives a call.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use same_file::Handle;

use crate::{
    application::{
        self, AuthorizationRequest, Command, ConditionalGetRequest, ConditionalReadError,
        ConditionalReadOutcome, CursorContext, CursorEncodeError, DecodedCursor, HistoryOrdering,
        HistoryPageRequest, HistoryReadKey, ItemHistoryEvent, ItemListFilters, ItemListOrdering,
        ItemProjection, ItemProjectionKind, ItemProjectionPageRequest, ProjectedItemRow,
        ProjectionGetRequest, ReadPage, ReadPageRequest, SelectedWorkOutcome,
    },
    config::{self, Config, GitMetadata, ProjectPathMappings, ProjectRemoteMappings},
    domain::{ItemId, NamedView, ProjectId, RequesterId},
    storage::{
        self, ItemHistoryRepository, ProjectRegistrationError, ProjectRepository,
        ProjectionRepository, ReadStoreState,
    },
    v2_response::{self, CursorCandidate, EncodeError, ReadError, ResponseBudget},
};

#[derive(Clone, Debug)]
pub enum ReadRequest {
    Get {
        item_id: ItemId,
        projection: ItemProjectionKind,
        known_version: Option<String>,
        conditional: bool,
    },
    Page {
        view: NamedView,
        ordering: ItemListOrdering,
        projection: ItemProjectionKind,
        filters: ItemListFilters,
        limit: usize,
        cursor: Option<String>,
    },
    History {
        item_id: ItemId,
        limit: usize,
        cursor: Option<String>,
    },
    SelectedWork {
        project: ProjectId,
    },
}

/// One open store, bounded prepared-statement cache, and immutable config.
pub struct ReadSession {
    config: Config,
    connection: Connection,
    database: PathBuf,
    file: Handle,
    state: ReadStoreState,
    restart_reason: Option<String>,
}

impl ReadSession {
    pub fn open(config: Config) -> Result<Self, ReadError> {
        let database = config
            .store_paths()
            .map_err(|_| ReadError::Internal)?
            .database;
        // Pin the filesystem object before SQLite opens it and bracket startup.
        let file = Handle::from_path(&database).map_err(|_| ReadError::NotInitialized)?;
        let connection = storage::open(&database).map_err(|error| match error {
            storage::MigrationError::NewerSchema { .. } => ReadError::UnsupportedVersion,
            storage::MigrationError::Sqlite(error) => sqlite_error(&error),
            _ => ReadError::Internal,
        })?;
        connection.set_prepared_statement_cache_capacity(64);
        if Handle::from_path(&database).map_err(|_| restart("database_file_changed"))? != file {
            return Err(restart("database_file_changed"));
        }
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| sqlite_error(&error))?;
        let state =
            storage::read_store_state(&transaction).map_err(|error| sqlite_error(&error))?;
        transaction.commit().map_err(|error| sqlite_error(&error))?;
        Ok(Self {
            config,
            connection,
            database,
            file,
            state,
            restart_reason: None,
        })
    }

    pub fn requester(&self) -> &RequesterId {
        &self.config.requester
    }

    /// Cancellation policy belongs to the transport; only its active call may
    /// be interrupted. The session owns no cancellation flags or request IDs.
    pub fn interrupt_handle(&self) -> rusqlite::InterruptHandle {
        self.connection.get_interrupt_handle()
    }

    #[doc(hidden)]
    pub fn is_autocommit(&self) -> bool {
        self.connection.is_autocommit()
    }

    /// Resolve current project registrations without caching or selecting one.
    pub fn resolve_project(
        &mut self,
        cwd: &Path,
        git: Option<&GitMetadata>,
    ) -> Result<ProjectId, ReadError> {
        let result = self.resolve_project_checked(cwd, git);
        self.invalidate_on_restart(result)
    }

    fn resolve_project_checked(
        &self,
        cwd: &Path,
        git: Option<&GitMetadata>,
    ) -> Result<ProjectId, ReadError> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| sqlite_error(&error))?;
        self.check_boundary(&transaction)?;
        let projects = ProjectRepository::new(&transaction);
        let paths =
            ProjectPathMappings::new(projects.list_paths().map_err(project_registration_error)?)
                .map_err(|_| ReadError::Internal)?;
        let remotes = ProjectRemoteMappings::new(
            projects
                .list_remotes()
                .map_err(project_registration_error)?,
        )
        .map_err(|_| ReadError::Internal)?;
        let project = config::resolve_project(None, cwd, git, &paths, &remotes)
            .map_err(|_| ReadError::Internal)?;
        transaction.commit().map_err(|error| sqlite_error(&error))?;
        self.check_fresh_boundary()?;
        Ok(project)
    }

    pub fn execute(
        &mut self,
        authorization: &AuthorizationRequest<'_>,
        request: ReadRequest,
        budget: ResponseBudget,
    ) -> Result<Vec<u8>, ReadError> {
        let result = self.execute_checked(authorization, request, budget);
        self.invalidate_on_restart(result)
    }

    /// Latch restart failures only after the checked call's transaction ends.
    fn invalidate_on_restart<T>(&mut self, result: Result<T, ReadError>) -> Result<T, ReadError> {
        if let Err(ReadError::RestartRequired { reason }) = &result {
            self.restart_reason = Some(reason.clone());
            self.connection.flush_prepared_statement_cache();
        }
        result
    }

    fn execute_checked(
        &self,
        authorization: &AuthorizationRequest<'_>,
        request: ReadRequest,
        budget: ResponseBudget,
    ) -> Result<Vec<u8>, ReadError> {
        if authorization.command != Command::Read {
            return Err(ReadError::Unauthorized);
        }
        application::authorize(authorization).map_err(|_| ReadError::Unauthorized)?;
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| sqlite_error(&error))?;
        let state = self.check_boundary(&transaction)?;
        let delivery = self.read(&transaction, &state, authorization, request)?;
        transaction.commit().map_err(|error| sqlite_error(&error))?;
        // Encoding (including all cursor byte-budget candidates) holds no lock.
        let mut bytes = Vec::new();
        delivery.write(&mut bytes, budget)?;
        self.check_fresh_boundary()?;
        Ok(bytes)
    }

    fn check_fresh_boundary(&self) -> Result<(), ReadError> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| sqlite_error(&error))?;
        self.check_boundary(&transaction)?;
        transaction.commit().map_err(|error| sqlite_error(&error))
    }

    fn check_boundary(&self, connection: &Connection) -> Result<ReadStoreState, ReadError> {
        if let Some(reason) = &self.restart_reason {
            return Err(restart(reason));
        }
        if Handle::from_path(&self.database).map_err(|_| restart("database_file_changed"))?
            != self.file
        {
            return Err(restart("database_file_changed"));
        }
        let state = storage::read_store_state(connection).map_err(|error| match &error {
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::OperationInterrupted =>
            {
                ReadError::Internal
            }
            _ if sqlite_error(&error) == ReadError::StorageBusy => ReadError::StorageBusy,
            _ => restart("store_or_schema_changed"),
        })?;
        if state != self.state {
            return Err(restart("store_or_schema_changed"));
        }
        Ok(state)
    }

    fn read(
        &self,
        connection: &Connection,
        state: &ReadStoreState,
        authorization: &AuthorizationRequest<'_>,
        request: ReadRequest,
    ) -> Result<Delivery, ReadError> {
        let repository = ProjectionRepository::new(connection);
        match request {
            ReadRequest::Get {
                item_id,
                projection,
                known_version,
                conditional,
            } if conditional || known_version.is_some() => {
                let result = application::read_conditional_item_projection(
                    &repository,
                    authorization,
                    &ConditionalGetRequest {
                        item_id,
                        projection,
                        known_version,
                    },
                )
                .map_err(|error| match error {
                    ConditionalReadError::Unauthorized(_) => ReadError::Unauthorized,
                    ConditionalReadError::NotFound => ReadError::NotFound,
                    ConditionalReadError::InvalidValidator => ReadError::InvalidInput,
                    ConditionalReadError::GenerationMissing => ReadError::GenerationMissing,
                    ConditionalReadError::Storage(error) => storage_error(&error),
                })?;
                Ok(Delivery::Conditional(result))
            }
            ReadRequest::Get {
                item_id,
                projection,
                ..
            } => {
                let item = application::read_item_projection(
                    &repository,
                    authorization,
                    &ProjectionGetRequest {
                        item_id,
                        projection,
                    },
                )
                .map_err(|error| match error {
                    application::ReadItemError::Unauthorized(_) => ReadError::Unauthorized,
                    application::ReadItemError::NotFound => ReadError::NotFound,
                    application::ReadItemError::Storage(error) => storage_error(&error),
                })?;
                Ok(Delivery::Get(item))
            }
            ReadRequest::Page {
                view,
                ordering,
                projection,
                filters,
                limit,
                cursor,
            } => {
                let mut request = ItemProjectionPageRequest {
                    view,
                    ordering,
                    projection,
                    filters,
                    configured_requester: self.config.requester.clone(),
                    page: ReadPageRequest::new(limit, None).map_err(|_| ReadError::InvalidInput)?,
                };
                let context =
                    CursorContext::item_page(&state.store_id, state.generation.as_str(), &request)
                        .map_err(|_| ReadError::InvalidInput)?;
                request.page.after = cursor
                    .as_deref()
                    .map(|token| {
                        let cursor = DecodedCursor::parse(token)?;
                        context.bind_item_key(&cursor)
                    })
                    .transpose()
                    .map_err(cursor_error)?;
                let page =
                    application::read_item_projection_page(&repository, authorization, &request)
                        .map_err(projection_error)?;
                Ok(Delivery::Page(page, context))
            }
            ReadRequest::History {
                item_id,
                limit,
                cursor,
            } => {
                let mut request = HistoryPageRequest {
                    item_id,
                    ordering: HistoryOrdering::RevisionThenEventIndex,
                    page: ReadPageRequest::new(limit, None).map_err(|_| ReadError::InvalidInput)?,
                };
                let context =
                    CursorContext::history(&state.store_id, state.generation.as_str(), &request);
                request.page.after = cursor
                    .as_deref()
                    .map(|token| {
                        let cursor = DecodedCursor::parse(token)?;
                        context.bind_history_key(&cursor)
                    })
                    .transpose()
                    .map_err(cursor_error)?;
                let page = application::read_item_history_page(
                    &ItemHistoryRepository::new(connection),
                    authorization,
                    &request,
                )
                .map_err(|error| match error {
                    application::ItemHistoryError::Unauthorized(_) => ReadError::Unauthorized,
                    application::ItemHistoryError::NotFound => ReadError::NotFound,
                    application::ItemHistoryError::InvalidPersistedData(_) => ReadError::Internal,
                    application::ItemHistoryError::Storage(error) => match error {
                        storage::ItemHistoryStorageError::Sqlite(error) => sqlite_error(&error),
                        _ => ReadError::Internal,
                    },
                })?;
                Ok(Delivery::History(request.item_id, page, context))
            }
            ReadRequest::SelectedWork { project } => {
                let result = application::read_selected_work(
                    &repository,
                    authorization,
                    self.requester(),
                    project,
                )
                .map_err(projection_error)?;
                Ok(Delivery::Selected(result))
            }
        }
    }
}

/// Owned only for this call, after the snapshot and before bounded encoding.
enum Delivery {
    Get(ItemProjection),
    Conditional(ConditionalReadOutcome),
    Page(ReadPage<ProjectedItemRow>, CursorContext),
    History(ItemId, ReadPage<ItemHistoryEvent>, CursorContext),
    Selected(SelectedWorkOutcome),
}

impl Delivery {
    fn write(self, bytes: &mut Vec<u8>, budget: ResponseBudget) -> Result<(), ReadError> {
        match self {
            Self::Get(item) => v2_response::write_get(bytes, &item, budget),
            Self::Conditional(result) => v2_response::write_conditional_get(bytes, &result, budget),
            Self::Selected(result) => v2_response::write_selected_work(bytes, &result, budget),
            Self::Page(page, context) => {
                v2_response::write_item_page_candidates(bytes, &page, budget, |row| {
                    generated_cursor(context.encode_item_key(&row.key))
                })
            }
            Self::History(item_id, page, context) => v2_response::write_history_page_candidates(
                bytes,
                &item_id,
                &page,
                budget,
                |event| generated_cursor(context.encode_history_key(&HistoryReadKey::from(event))),
            ),
        }
        .map(|_| ())
        .map_err(|error| match error {
            EncodeError::Read(error) => error,
            _ => ReadError::Internal,
        })
    }
}

fn restart(reason: &str) -> ReadError {
    ReadError::RestartRequired {
        reason: reason.into(),
    }
}

fn cursor_error(error: application::InvalidCursor) -> ReadError {
    ReadError::InvalidCursor {
        reason: error.reason().into(),
        restart_required: error.restart_required(),
    }
}

fn generated_cursor(
    result: Result<String, CursorEncodeError>,
) -> Result<CursorCandidate, ReadError> {
    match result {
        Ok(token) => Ok(CursorCandidate::Token(token)),
        Err(CursorEncodeError::TooLarge {
            encoded_token_bytes,
        }) => Ok(CursorCandidate::Oversized {
            encoded_token_bytes,
        }),
        Err(_) => Err(ReadError::Internal),
    }
}

fn projection_error(
    error: application::ProjectionPageError<storage::ItemStorageError>,
) -> ReadError {
    match error {
        application::ProjectionPageError::Unauthorized(_) => ReadError::Unauthorized,
        application::ProjectionPageError::InvalidFilters(_) => ReadError::InvalidInput,
        application::ProjectionPageError::Storage(error) => storage_error(&error),
    }
}

fn storage_error(error: &storage::ItemStorageError) -> ReadError {
    match error {
        storage::ItemStorageError::Sqlite(error) => sqlite_error(error),
        _ => ReadError::Internal,
    }
}

fn sqlite_error(error: &rusqlite::Error) -> ReadError {
    match error {
        rusqlite::Error::SqliteFailure(error, _)
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            ReadError::StorageBusy
        }
        _ => ReadError::Internal,
    }
}

fn project_registration_error(error: ProjectRegistrationError) -> ReadError {
    match error {
        ProjectRegistrationError::Sqlite(error) => sqlite_error(&error),
        _ => ReadError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::trace::{TraceEvent, TraceEventCodes};
    use std::cell::RefCell;

    thread_local! {
        static ROTATION: RefCell<Option<(Connection, bool)>> = const { RefCell::new(None) };
    }

    fn rotate_at_payload(event: TraceEvent<'_>) {
        if let TraceEvent::Stmt(statement, _) = event {
            let sql = statement.sql();
            if sql.contains("FROM items AS i") || sql.contains("SELECT EXISTS") {
                ROTATION.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    let (writer, fired) = slot.as_mut().unwrap();
                    if !*fired {
                        *fired = true;
                        writer.execute_batch(
                            "BEGIN IMMEDIATE;
                             UPDATE journal_metadata SET generation = '00000000000000000000000000000000';
                             UPDATE items SET title = 'restored';
                             COMMIT;").unwrap();
                    }
                });
            }
        }
    }

    #[test]
    fn rotation_between_session_binding_and_list_or_history_selection_latches_restart() {
        for history in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join(".bif")).unwrap();
            let config = Config {
                root: temp.path().to_owned(),
                requester: RequesterId::new("ALICE").unwrap(),
                source: None,
            };
            let writer = storage::open(config.store_paths().unwrap().database).unwrap();
            writer
                .execute_batch(
                    "INSERT INTO projects VALUES ('bif', 'now');
                 INSERT INTO items VALUES ('ALICE:bif:001','ALICE','bif',1,'original',NULL,
                  'proposed',NULL,NULL,NULL,1,'now','now');",
                )
                .unwrap();
            let mut session = ReadSession::open(config).unwrap();
            ROTATION.with(|slot| *slot.borrow_mut() = Some((writer, false)));
            session
                .connection
                .trace_v2(TraceEventCodes::all(), Some(rotate_at_payload));
            let id = ItemId::new(
                RequesterId::new("ALICE").unwrap(),
                ProjectId::new("bif").unwrap(),
                1,
            )
            .unwrap();
            let request = if history {
                ReadRequest::History {
                    item_id: id,
                    limit: 1,
                    cursor: None,
                }
            } else {
                ReadRequest::Page {
                    view: NamedView::All,
                    ordering: ItemListOrdering::NewestFirst,
                    projection: ItemProjectionKind::Summary,
                    filters: ItemListFilters::default(),
                    limit: 1,
                    cursor: None,
                }
            };
            let authorization = AuthorizationRequest {
                actor: application::Actor {
                    kind: application::ActorKind::Human,
                    id: "ALICE",
                    surface: "cli",
                    host: "local",
                },
                execution: application::Execution::Direct {
                    surface: "cli",
                    host: "local",
                },
                observed_execution: application::ObservedExecution::Direct,
                command: Command::Read,
                human_authorization: None,
            };
            assert!(matches!(
                session.execute(&authorization, request.clone(), ResponseBudget::default()),
                Err(ReadError::RestartRequired { .. })
            ));
            session.connection.trace_v2(TraceEventCodes::empty(), None);
            assert!(ROTATION.with(|slot| slot.borrow_mut().take().unwrap().1));
            assert!(session.is_autocommit());
            assert!(matches!(
                session.execute(&authorization, request, ResponseBudget::default()),
                Err(ReadError::RestartRequired { .. })
            ));
        }
    }

    #[test]
    fn project_metadata_preserves_busy_and_locked_errors() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            assert_eq!(
                project_registration_error(ProjectRegistrationError::Sqlite(
                    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
                )),
                ReadError::StorageBusy
            );
        }
        assert_eq!(
            project_registration_error(ProjectRegistrationError::Sqlite(
                rusqlite::Error::InvalidQuery
            )),
            ReadError::Internal
        );
    }
}
