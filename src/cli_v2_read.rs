//! Explicit, JSON-only v2 CLI reads. The v1 CLI and RPC never enter this adapter.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    str::FromStr,
};

use serde_json::{Map, Value, json};

use crate::{
    application::{
        self, Actor, ActorKind, AuthorizationRequest, Command, CursorContext, CursorEncodeError,
        DecodedCursor, Execution, HistoryOrdering, HistoryPageRequest, HistoryReadKey,
        HumanAuthorization, ItemListFilters, ItemListOrdering, ItemProjectionKind,
        ItemProjectionPageRequest, ObservedExecution, ProjectionGetRequest, ReadPageRequest,
    },
    cli_read,
    config::{self, ConfigOverrides, ProjectPathMappings, ProjectRemoteMappings, resolve_project},
    domain::{AssigneeId, NamedView, ProjectId, RequesterId},
    limits::MAXIMUM_REQUEST_BYTES,
    rpc_read::{ClassifyReadStorageError, ReadStorageErrorKind},
    storage::{
        self, ItemHistoryRepository, ProjectRegistrationError, ProjectRepository,
        ProjectionRepository,
    },
    v2_response::{self, CursorCandidate, EncodeError, ReadError, ReadErrorCode, ResponseBudget},
};

const PAGE_OPTIONS: &[&str] = &[
    "project",
    "requester",
    "assignee",
    "status",
    "priority",
    "unassigned",
    "text",
    "projection",
    "limit",
    "cursor",
];

enum ReadCommand {
    Get(ProjectionGetRequest),
    Page {
        view: NamedView,
        ordering: ItemListOrdering,
        projection: ItemProjectionKind,
        filters: ItemListFilters,
        limit: usize,
    },
    History {
        request: HistoryPageRequest,
    },
}

struct Parsed {
    command: ReadCommand,
    cursor: Option<DecodedCursor>,
    overrides: ConfigOverrides,
}

/// Encode into a private buffer so no error can follow a partial success prefix.
pub(crate) fn run(arguments: Vec<OsString>, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let result = arguments
        .into_iter()
        .map(|argument| argument.into_string().map_err(|_| ReadError::InvalidInput))
        .collect::<Result<Vec<_>, _>>()
        .and_then(|arguments| parse(&arguments))
        .and_then(execute);
    let (bytes, exit) = match result {
        Ok(bytes) => (bytes, 0),
        Err(error) => {
            let _ = writeln!(stderr, "error: {error}");
            let mut bytes = Vec::new();
            if let Err(error) =
                v2_response::write_error(&mut bytes, &error, ResponseBudget::default())
            {
                let _ = writeln!(stderr, "error: could not encode response: {error}");
                return 1;
            }
            (bytes, exit_code(error.code()))
        }
    };
    if let Err(error) = stdout.write_all(&bytes) {
        let _ = writeln!(stderr, "error: could not write output: {error}");
        return 1;
    }
    exit
}

/// Reject options, types and the encoded request budget before loading config.
fn parse(arguments: &[String]) -> Result<Parsed, ReadError> {
    match arguments.get(1).map(String::as_str) {
        Some("2") => {}
        None => return Err(ReadError::InvalidInput),
        Some(_) => return Err(ReadError::UnsupportedVersion),
    }
    let name = arguments.get(2).ok_or(ReadError::InvalidInput)?.as_str();
    if !matches!(name, "get" | "list" | "next" | "history") {
        return Err(ReadError::InvalidInput);
    }
    let mut position = 3;
    let item_id = if matches!(name, "get" | "history") {
        let value = arguments.get(position).ok_or(ReadError::InvalidInput)?;
        position += 1;
        Some(cli_read::parse_item_id(value).map_err(invalid)?)
    } else {
        None
    };
    let positional_view = if name == "list" {
        arguments
            .get(position)
            .filter(|value| !value.starts_with("--"))
            .map(|value| {
                position += 1;
                value.as_str()
            })
    } else {
        None
    };
    let mut options = BTreeMap::<&str, Option<&str>>::new();
    while position < arguments.len() {
        let option = arguments[position]
            .strip_prefix("--")
            .ok_or(ReadError::InvalidInput)?;
        let allowed = matches!(option, "config" | "root" | "json")
            || match name {
                "get" => option == "projection",
                "history" => matches!(option, "limit" | "cursor"),
                "list" => option == "view" || PAGE_OPTIONS.contains(&option),
                "next" => PAGE_OPTIONS.contains(&option),
                _ => false,
            };
        if !allowed || options.contains_key(option) {
            return Err(ReadError::InvalidInput);
        }
        position += 1;
        let value = if matches!(option, "json" | "unassigned") {
            None
        } else {
            let value = arguments
                .get(position)
                .filter(|value| !value.starts_with("--"))
                .ok_or(ReadError::InvalidInput)?;
            position += 1;
            Some(value.as_str())
        };
        options.insert(option, value);
    }
    if !options.contains_key("json") || (positional_view.is_some() && options.contains_key("view"))
    {
        return Err(ReadError::InvalidInput);
    }
    let get = |name: &str| options.get(name).copied().flatten();
    let projection_name = get("projection").unwrap_or("summary");
    let projection = match projection_name {
        "summary" => ItemProjectionKind::Summary,
        "work" => ItemProjectionKind::Work,
        "audit" => ItemProjectionKind::Audit,
        _ => return Err(ReadError::InvalidInput),
    };
    let limit = match get("limit") {
        Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            value.parse::<usize>().map_err(invalid)?
        }
        Some(_) => return Err(ReadError::InvalidInput),
        None => 20,
    };
    application::PageSize::new(limit).map_err(invalid)?;
    let view_name = positional_view.or_else(|| get("view")).unwrap_or("all");
    let mut wire = Map::new();
    let command = match name {
        "get" => {
            let item_id = item_id.expect("validated item command");
            wire.insert("item_id".into(), json!(item_id.to_string()));
            wire.insert("projection".into(), json!(projection_name));
            ReadCommand::Get(ProjectionGetRequest {
                item_id,
                projection,
            })
        }
        "history" => {
            let item_id = item_id.expect("validated item command");
            wire.insert("item_id".into(), json!(item_id.to_string()));
            wire.insert("limit".into(), json!(limit));
            if let Some(cursor) = get("cursor") {
                wire.insert("cursor".into(), json!(cursor));
            }
            ReadCommand::History {
                request: HistoryPageRequest {
                    item_id,
                    ordering: HistoryOrdering::RevisionThenEventIndex,
                    page: ReadPageRequest::new(limit, None).map_err(invalid)?,
                },
            }
        }
        "list" | "next" => {
            let filters = ItemListFilters {
                project: get("project")
                    .map(ProjectId::new)
                    .transpose()
                    .map_err(invalid)?,
                requester: get("requester")
                    .map(RequesterId::new)
                    .transpose()
                    .map_err(invalid)?,
                assignee: get("assignee")
                    .map(AssigneeId::new)
                    .transpose()
                    .map_err(invalid)?,
                status: get("status")
                    .map(cli_read::parse_status)
                    .transpose()
                    .map_err(invalid)?,
                priority: get("priority")
                    .map(cli_read::parse_priority)
                    .transpose()
                    .map_err(invalid)?,
                unassigned: options.contains_key("unassigned"),
                text: get("text")
                    .map(application::ItemTextFilter::new)
                    .transpose()
                    .map_err(invalid)?,
            };
            if filters.assignee.is_some() && filters.unassigned {
                return Err(ReadError::InvalidInput);
            }
            let view = if name == "next" {
                NamedView::Ready
            } else {
                wire.insert("view".into(), json!(view_name));
                NamedView::from_str(view_name).map_err(invalid)?
            };
            for key in [
                "project",
                "requester",
                "assignee",
                "status",
                "priority",
                "text",
                "cursor",
            ] {
                if let Some(value) = get(key) {
                    wire.insert(key.into(), json!(value));
                }
            }
            wire.insert("unassigned".into(), json!(filters.unassigned));
            wire.insert("projection".into(), json!(projection_name));
            wire.insert("limit".into(), json!(limit));
            ReadCommand::Page {
                view,
                ordering: if name == "next" {
                    ItemListOrdering::Next
                } else {
                    ItemListOrdering::NewestFirst
                },
                projection,
                filters,
                limit,
            }
        }
        _ => unreachable!(),
    };
    serde_json::to_writer(&mut RequestByteCounter(0), &Value::Object(wire)).map_err(invalid)?;
    // Preserve request-budget priority, then parse once before config/storage.
    // Binding to the authorized current store/query happens only in execute.
    let cursor = get("cursor")
        .map(|token| {
            let cursor = DecodedCursor::parse(token)?;
            match &command {
                ReadCommand::Page { ordering, .. } => cursor.require_item_kind(*ordering)?,
                ReadCommand::History { .. } => cursor.require_history_kind()?,
                ReadCommand::Get(_) => unreachable!("get rejects the cursor option"),
            }
            Ok(cursor)
        })
        .transpose()
        .map_err(cursor_error)?;
    Ok(Parsed {
        command,
        cursor,
        overrides: ConfigOverrides {
            config: get("config").map(PathBuf::from),
            root: get("root").map(PathBuf::from),
            // --requester is a query filter, never an actor or `mine` override.
            requester: None,
        },
    })
}

/// Count escaped UTF-8 JSON, without constructing an unbounded encoded body.
struct RequestByteCounter(usize);

impl Write for RequestByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.checked_add(bytes.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "request exceeds byte limit")
        })?;
        if self.0 > MAXIMUM_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "request exceeds byte limit",
            ));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn execute(mut parsed: Parsed) -> Result<Vec<u8>, ReadError> {
    let config = config::load(parsed.overrides).map_err(|error| match error {
        config::ConfigError::NotInitialized => ReadError::NotInitialized,
        _ => ReadError::Internal,
    })?;
    let authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: config.requester.as_str(),
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
    // Cursors supply only a boundary; authorization is reevaluated independently.
    application::authorize(&authorization).map_err(|_| ReadError::Unauthorized)?;
    let paths = config.store_paths().map_err(|_| ReadError::Internal)?;
    let mut connection = storage::open(&paths.database).map_err(|error| match error {
        storage::MigrationError::NewerSchema { .. } => ReadError::UnsupportedVersion,
        storage::MigrationError::Sqlite(error) => sqlite_error(&error),
        _ => ReadError::Internal,
    })?;
    if let ReadCommand::Page { filters, .. } = &mut parsed.command {
        if filters.project.is_none() {
            let projects = ProjectRepository::new(&mut connection);
            let path_mappings = ProjectPathMappings::new(
                projects.list_paths().map_err(project_registration_error)?,
            )
            .map_err(|_| ReadError::Internal)?;
            let remote_mappings = ProjectRemoteMappings::new(
                projects
                    .list_remotes()
                    .map_err(project_registration_error)?,
            )
            .map_err(|_| ReadError::Internal)?;
            let cwd = env::current_dir().map_err(|_| ReadError::Internal)?;
            filters.project = Some(
                resolve_project(
                    None,
                    &cwd,
                    super::cli::git_metadata(&cwd).as_ref(),
                    &path_mappings,
                    &remote_mappings,
                )
                .map_err(|_| ReadError::Internal)?,
            );
        }
    }
    let mut output = Vec::new();
    let budget = ResponseBudget::default();
    match parsed.command {
        ReadCommand::Get(request) => {
            let item = application::read_item_projection(
                &ProjectionRepository::new(&connection),
                &authorization,
                &request,
            )
            .map_err(|error| match error {
                application::ReadItemError::NotFound => ReadError::NotFound,
                application::ReadItemError::Unauthorized(_) => ReadError::Unauthorized,
                application::ReadItemError::Storage(error) => storage_error(&error),
            })?;
            v2_response::write_get(&mut output, &item, budget).map_err(encode_error)?;
        }
        ReadCommand::Page {
            view,
            ordering,
            projection,
            filters,
            limit,
        } => {
            let store_id =
                storage::read_store_identity(&connection).map_err(|error| sqlite_error(&error))?;
            let mut request = ItemProjectionPageRequest {
                view,
                configured_requester: config.requester.clone(),
                filters,
                projection,
                ordering,
                page: ReadPageRequest::new(limit, None).map_err(invalid)?,
            };
            let context = CursorContext::item_page(store_id, &request).map_err(invalid)?;
            request.page.after = parsed
                .cursor
                .as_ref()
                .map(|cursor| context.bind_item_key(cursor))
                .transpose()
                .map_err(cursor_error)?;
            let page = application::read_item_projection_page(
                &ProjectionRepository::new(&connection),
                &authorization,
                &request,
            )
            .map_err(|error| match error {
                application::ProjectionPageError::InvalidFilters(_) => ReadError::InvalidInput,
                application::ProjectionPageError::Unauthorized(_) => ReadError::Unauthorized,
                application::ProjectionPageError::Storage(error) => storage_error(&error),
            })?;
            v2_response::write_item_page_candidates(&mut output, &page, budget, |row| {
                generated_cursor(context.encode_item_key(&row.key))
            })
            .map_err(encode_error)?;
        }
        ReadCommand::History { mut request } => {
            let store_id =
                storage::read_store_identity(&connection).map_err(|error| sqlite_error(&error))?;
            let context = CursorContext::history(store_id, &request);
            request.page.after = parsed
                .cursor
                .as_ref()
                .map(|cursor| context.bind_history_key(cursor))
                .transpose()
                .map_err(cursor_error)?;
            let page = application::read_item_history_page(
                &ItemHistoryRepository::new(&connection),
                &authorization,
                &request,
            )
            .map_err(|error| match error {
                application::ItemHistoryError::NotFound => ReadError::NotFound,
                application::ItemHistoryError::Unauthorized(_) => ReadError::Unauthorized,
                application::ItemHistoryError::InvalidPersistedData(_) => ReadError::Internal,
                application::ItemHistoryError::Storage(error) => storage_error(&error),
            })?;
            v2_response::write_history_page_candidates(
                &mut output,
                &request.item_id,
                &page,
                budget,
                |event| generated_cursor(context.encode_history_key(&HistoryReadKey::from(event))),
            )
            .map_err(encode_error)?;
        }
    }
    Ok(output)
}

/// Oversized generated boundaries are page candidates, not invalid input.
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

fn invalid(_: impl std::fmt::Display) -> ReadError {
    ReadError::InvalidInput
}

fn cursor_error(error: application::InvalidCursor) -> ReadError {
    ReadError::InvalidCursor {
        reason: error.reason().into(),
        restart_required: error.restart_required(),
    }
}

fn storage_error(error: &impl ClassifyReadStorageError) -> ReadError {
    match error.rpc_read_kind() {
        ReadStorageErrorKind::Busy => ReadError::StorageBusy,
        ReadStorageErrorKind::InvalidPersistedData | ReadStorageErrorKind::Other => {
            ReadError::Internal
        }
    }
}

/// Preserve native SQLite contention when resolving implicit project metadata.
fn project_registration_error(error: ProjectRegistrationError) -> ReadError {
    match error {
        ProjectRegistrationError::Sqlite(error) => sqlite_error(&error),
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

fn encode_error(error: EncodeError) -> ReadError {
    match error {
        EncodeError::Read(error) => error,
        _ => ReadError::Internal,
    }
}

fn exit_code(code: ReadErrorCode) -> i32 {
    match code {
        ReadErrorCode::InvalidInput | ReadErrorCode::InvalidCursor => 2,
        ReadErrorCode::NotFound => 3,
        ReadErrorCode::Unauthorized => 4,
        ReadErrorCode::UnsupportedVersion => 8,
        ReadErrorCode::NotInitialized => 9,
        ReadErrorCode::StorageBusy => 10,
        ReadErrorCode::PayloadTooLarge => 11,
        ReadErrorCode::Internal => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_metadata_preserves_busy_and_locked_sqlite_errors() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = ProjectRegistrationError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ));
            let classified = project_registration_error(error);
            assert_eq!(classified.code(), ReadErrorCode::StorageBusy);
            assert_eq!(exit_code(classified.code()), 10);
        }
    }

    #[test]
    fn other_project_metadata_errors_remain_internal() {
        let existing = ProjectId::new("alpha").unwrap();
        let requested = ProjectId::new("beta").unwrap();
        for error in [
            ProjectRegistrationError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                None,
            )),
            ProjectRegistrationError::Sqlite(rusqlite::Error::InvalidQuery),
            ProjectRegistrationError::InvalidStoredRegistration {
                field: "project_id",
                value: "invalid project".into(),
            },
            ProjectRegistrationError::PathConflict {
                path: PathBuf::from("/project"),
                existing: existing.clone(),
                requested: requested.clone(),
            },
            ProjectRegistrationError::RemoteConflict {
                remote: "example.com/project".into(),
                existing,
                requested,
            },
            ProjectRegistrationError::UnsupportedPathEncoding(PathBuf::from("/project")),
        ] {
            let classified = project_registration_error(error);
            assert_eq!(classified.code(), ReadErrorCode::Internal);
            assert_eq!(exit_code(classified.code()), 1);
        }
    }
}
