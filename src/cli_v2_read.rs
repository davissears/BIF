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
        self, Actor, ActorKind, AuthorizationRequest, Command, DecodedCursor, Execution,
        HumanAuthorization, ItemListFilters, ItemListOrdering, ItemProjectionKind,
        ObservedExecution,
    },
    cli_read,
    config::{self, ConfigOverrides},
    domain::{AssigneeId, NamedView, ProjectId, RequesterId},
    limits::MAXIMUM_REQUEST_BYTES,
    read_session::{ReadRequest, ReadSession},
    v2_response::{self, ReadError, ReadErrorCode, ResponseBudget},
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

struct Parsed {
    command: ReadRequest,
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
    if !matches!(name, "get" | "list" | "next" | "history" | "selected-work") {
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
                "get" => matches!(option, "projection" | "conditional" | "known-version"),
                "history" => matches!(option, "limit" | "cursor"),
                "list" => option == "view" || PAGE_OPTIONS.contains(&option),
                "next" => PAGE_OPTIONS.contains(&option),
                "selected-work" => option == "project",
                _ => false,
            };
        if !allowed || options.contains_key(option) {
            return Err(ReadError::InvalidInput);
        }
        position += 1;
        let value = if matches!(option, "json" | "unassigned" | "conditional") {
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
            if let Some(version) = get("known-version") {
                wire.insert("known_version".into(), json!(version));
            }
            let conditional = options.contains_key("conditional") || get("known-version").is_some();
            if conditional {
                wire.insert("conditional".into(), json!(true));
            }
            ReadRequest::Get {
                item_id,
                projection,
                conditional,
                known_version: get("known-version").map(str::to_owned),
            }
        }
        "history" => {
            let item_id = item_id.expect("validated item command");
            wire.insert("item_id".into(), json!(item_id.to_string()));
            wire.insert("limit".into(), json!(limit));
            if let Some(cursor) = get("cursor") {
                wire.insert("cursor".into(), json!(cursor));
            }
            ReadRequest::History {
                item_id,
                limit,
                cursor: get("cursor").map(str::to_owned),
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
            ReadRequest::Page {
                view,
                ordering: if name == "next" {
                    ItemListOrdering::Next
                } else {
                    ItemListOrdering::NewestFirst
                },
                projection,
                filters,
                limit,
                cursor: get("cursor").map(str::to_owned),
            }
        }
        "selected-work" => {
            let project =
                ProjectId::new(get("project").ok_or(ReadError::InvalidInput)?).map_err(invalid)?;
            wire.insert("project".into(), json!(project.as_str()));
            ReadRequest::SelectedWork { project }
        }
        _ => unreachable!(),
    };
    serde_json::to_writer(&mut RequestByteCounter(0), &Value::Object(wire)).map_err(invalid)?;
    // Preserve request-budget priority, then parse once before config/storage.
    // Binding to the authorized current store/query happens only in execute.
    get("cursor")
        .map(|token| {
            let cursor = DecodedCursor::parse(token)?;
            match &command {
                ReadRequest::Page { ordering, .. } => cursor.require_item_kind(*ordering)?,
                ReadRequest::History { .. } => cursor.require_history_kind()?,
                _ => unreachable!("these commands reject the cursor option"),
            }
            Ok(cursor)
        })
        .transpose()
        .map_err(cursor_error)?;
    Ok(Parsed {
        command,
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
    let requester = config.requester.clone();
    let authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: requester.as_str(),
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
    let mut session = ReadSession::open(config)?;
    if let ReadRequest::Page { filters, .. } = &mut parsed.command
        && filters.project.is_none()
    {
        let cwd = env::current_dir().map_err(|_| ReadError::Internal)?;
        filters.project =
            Some(session.resolve_project(&cwd, super::cli::git_metadata(&cwd).as_ref())?);
    }
    session.execute(&authorization, parsed.command, ResponseBudget::default())
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

fn exit_code(code: ReadErrorCode) -> i32 {
    match code {
        ReadErrorCode::InvalidInput | ReadErrorCode::InvalidCursor => 2,
        ReadErrorCode::NotFound => 3,
        ReadErrorCode::Unauthorized => 4,
        ReadErrorCode::UnsupportedVersion => 8,
        ReadErrorCode::NotInitialized => 9,
        ReadErrorCode::StorageBusy => 10,
        ReadErrorCode::PayloadTooLarge => 11,
        ReadErrorCode::RestartRequired => 12,
        ReadErrorCode::Internal => 1,
    }
}
