//! CLI adapter for read-only item operations.
//!
//! Parsing and rendering live here so capture and setup commands can evolve in
//! `cli.rs` without duplicating application or storage behavior.

use std::{env, fmt, path::PathBuf, str::FromStr};

use serde_json::{Value, json};

use crate::{
    application::{
        self, Actor, ActorKind, AuthorizationRequest, Command, Execution, HumanAuthorization,
        ItemListFilters, ObservedExecution, PageOffset, PageSize, Pagination,
    },
    config::{self, ConfigOverrides, ProjectPathMappings, ProjectRemoteMappings, resolve_project},
    domain::{AssigneeId, Item, ItemId, NamedView, Priority, ProjectId, RequesterId, Status},
    rpc::{self, ErrorCode},
    rpc_read::{self, ClassifyReadStorageError, ReadStorageErrorKind},
    storage::{self, ItemHistoryRepository, ItemRepository, ProjectRepository},
};

const LIST_OPTIONS: &[&str] = &[
    "view",
    "project",
    "requester",
    "assignee",
    "status",
    "priority",
    "text",
    "limit",
    "offset",
    "config",
    "root",
];

#[derive(Debug)]
pub(crate) struct ReadError {
    pub message: String,
    pub exit_code: i32,
    pub usage: bool,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl ReadError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: ErrorCode::InvalidInput.exit_code(),
            usage: true,
        }
    }

    fn operation(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: ErrorCode::Internal.exit_code(),
            usage: false,
        }
    }

    fn not_found() -> Self {
        Self {
            message: "item was not found".into(),
            exit_code: ErrorCode::NotFound.exit_code(),
            usage: false,
        }
    }
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Human,
    Json,
}

enum ReadCommand {
    Get(ItemId),
    History(ItemId),
    List { view: NamedView, next: bool },
}

struct Parsed {
    command: ReadCommand,
    filters: ItemListFilters,
    pagination: Pagination,
    overrides: ConfigOverrides,
    explicit_project: bool,
    format: OutputFormat,
}

/// Returns `None` when the arguments do not name a read command.
pub(crate) fn run(arguments: &[String]) -> Option<Result<String, ReadError>> {
    matches!(
        arguments.first().map(String::as_str),
        Some("get" | "list" | "next" | "history")
    )
    .then(|| parse(arguments).and_then(execute))
}

fn parse(arguments: &[String]) -> Result<Parsed, ReadError> {
    let name = arguments[0].as_str();
    let item_command = matches!(name, "get" | "history");
    let mut position = 1;
    let item = if item_command {
        let value = arguments
            .get(position)
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| ReadError::usage(format!("{name} requires ITEM_ID")))?;
        position += 1;
        Some(parse_item_id(value)?)
    } else {
        None
    };

    // `list VIEW` is a convenient spelling of `list --view VIEW`.
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

    let mut values = Vec::<(&str, &str)>::new();
    let mut unassigned = false;
    let mut json = false;
    while position < arguments.len() {
        let option = arguments[position].strip_prefix("--").ok_or_else(|| {
            ReadError::usage(format!("unexpected argument {:?}", arguments[position]))
        })?;
        if option == "unassigned" || option == "json" {
            let flag = if option == "unassigned" {
                &mut unassigned
            } else {
                &mut json
            };
            if *flag {
                return Err(ReadError::usage(format!("duplicate option --{option}")));
            }
            *flag = true;
            position += 1;
            continue;
        }
        let allowed = if item_command {
            &["config", "root", "requester", "format"][..]
        } else {
            LIST_OPTIONS
        };
        if !allowed.contains(&option) && option != "format" {
            return Err(ReadError::usage(format!("unknown option --{option}")));
        }
        if values.iter().any(|(name, _)| *name == option) {
            return Err(ReadError::usage(format!("duplicate option --{option}")));
        }
        let value = arguments
            .get(position + 1)
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| ReadError::usage(format!("--{option} requires a value")))?;
        values.push((option, value));
        position += 2;
    }
    let get = |name| {
        values
            .iter()
            .find_map(|(key, value)| (*key == name).then_some(*value))
    };
    if positional_view.is_some() && get("view").is_some() {
        return Err(ReadError::usage("view may be supplied only once"));
    }
    let format = match (json, get("format")) {
        (true, Some(_)) => return Err(ReadError::usage("output format may be supplied only once")),
        (true, None) | (false, Some("json")) => OutputFormat::Json,
        (false, None) | (false, Some("human")) => OutputFormat::Human,
        (false, Some(value)) => return Err(ReadError::usage(format!("unknown format {value:?}"))),
    };
    let project = get("project")
        .map(ProjectId::new)
        .transpose()
        .map_err(input)?;
    let filters = ItemListFilters {
        project,
        requester: if item_command {
            None
        } else {
            get("requester")
                .map(RequesterId::new)
                .transpose()
                .map_err(input)?
        },
        assignee: get("assignee")
            .map(AssigneeId::new)
            .transpose()
            .map_err(input)?,
        status: get("status").map(parse_status).transpose()?,
        priority: get("priority").map(parse_priority).transpose()?,
        unassigned,
        text: get("text")
            .map(application::ItemTextFilter::new)
            .transpose()
            .map_err(input)?,
    };
    if filters.unassigned && filters.assignee.is_some() {
        return Err(ReadError::usage("--unassigned conflicts with --assignee"));
    }
    let limit = number(get("limit"), 100, "limit")?;
    let offset = number(get("offset"), 0, "offset")?;
    let pagination = Pagination::new(
        PageSize::new(limit).map_err(input)?,
        PageOffset::new(offset),
    );
    let command = match name {
        "get" => ReadCommand::Get(item.expect("parsed item")),
        "history" => ReadCommand::History(item.expect("parsed item")),
        "next" => ReadCommand::List {
            view: NamedView::Ready,
            next: true,
        },
        "list" => ReadCommand::List {
            view: NamedView::from_str(positional_view.or_else(|| get("view")).unwrap_or("all"))
                .map_err(input)?,
            next: false,
        },
        _ => unreachable!(),
    };
    Ok(Parsed {
        command,
        filters,
        pagination,
        overrides: ConfigOverrides {
            config: get("config").map(PathBuf::from),
            root: get("root").map(PathBuf::from),
            requester: if item_command {
                get("requester").map(str::to_owned)
            } else {
                None
            },
        },
        explicit_project: get("project").is_some(),
        format,
    })
}

fn execute(mut parsed: Parsed) -> Result<String, ReadError> {
    let config = config::load(parsed.overrides).map_err(|error| {
        if matches!(error, config::ConfigError::NotInitialized) {
            ReadError {
                message: error.to_string(),
                exit_code: ErrorCode::NotInitialized.exit_code(),
                usage: false,
            }
        } else {
            operation(error)
        }
    })?;
    let paths = config.store_paths().map_err(operation)?;
    let mut connection = storage::open(&paths.database).map_err(operation)?;

    if matches!(parsed.command, ReadCommand::List { .. }) && !parsed.explicit_project {
        let projects = ProjectRepository::new(&mut connection);
        let path_mappings = ProjectPathMappings::new(projects.list_paths().map_err(operation)?)
            .map_err(operation)?;
        let remote_mappings =
            ProjectRemoteMappings::new(projects.list_remotes().map_err(operation)?)
                .map_err(operation)?;
        let cwd = env::current_dir().map_err(operation)?;
        parsed.filters.project = Some(
            resolve_project(
                None,
                &cwd,
                super::cli::git_metadata(&cwd).as_ref(),
                &path_mappings,
                &remote_mappings,
            )
            .map_err(operation)?,
        );
    }

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
    let items = ItemRepository::new(&connection);
    match parsed.command {
        ReadCommand::Get(id) => {
            let item =
                application::read_item(&items, &authorization, &id).map_err(
                    |error| match error {
                        application::ReadItemError::NotFound => ReadError::not_found(),
                        application::ReadItemError::Unauthorized(_) => unauthorized(),
                        application::ReadItemError::Storage(error) => storage_error(&error),
                    },
                )?;
            render_item(&item, parsed.format)
        }
        ReadCommand::History(id) => {
            let history = ItemHistoryRepository::new(&connection);
            let events =
                application::read_item_history(&history, &authorization, &id).map_err(|error| {
                    match error {
                        application::ItemHistoryError::NotFound => ReadError::not_found(),
                        application::ItemHistoryError::Unauthorized(_) => unauthorized(),
                        application::ItemHistoryError::InvalidPersistedData(error)
                        | application::ItemHistoryError::Storage(error) => storage_error(&error),
                    }
                })?;
            render_history(&events, parsed.format)
        }
        ReadCommand::List { view, next } => {
            let page = if next {
                application::next_item_page(
                    &items,
                    &authorization,
                    &config.requester,
                    &parsed.filters,
                    parsed.pagination,
                )
            } else {
                application::list_item_page(
                    &items,
                    &authorization,
                    view,
                    &config.requester,
                    &parsed.filters,
                    parsed.pagination,
                )
            }
            .map_err(|error| match error {
                application::SelectNamedViewError::Unauthorized(_) => unauthorized(),
                application::SelectNamedViewError::Storage(error) => storage_error(&error),
            })?;
            render_page(&page.items, page.next_offset, parsed.format)
        }
    }
}

fn render_item(item: &Item, format: OutputFormat) -> Result<String, ReadError> {
    match format {
        OutputFormat::Json => json_line(rpc::item_json(item)),
        OutputFormat::Human => Ok(format!(
            "{}\t{}\t{}\t{}\t{}\n{}\n",
            item.id(),
            status(item.status()),
            item.priority().map_or("-", priority),
            item.assignee().map_or("-", AssigneeId::as_str),
            item.content().title(),
            item.content().description().unwrap_or("-")
        )),
    }
}

fn render_page(
    items: &[Item],
    next_offset: Option<PageOffset>,
    format: OutputFormat,
) -> Result<String, ReadError> {
    match format {
        OutputFormat::Json => json_line(json!({
            "items": items.iter().map(rpc::item_json).collect::<Vec<_>>(),
            "next_offset": next_offset.map(PageOffset::get)
        })),
        OutputFormat::Human => {
            let mut output = String::new();
            for item in items {
                output.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    item.id(),
                    status(item.status()),
                    item.priority().map_or("-", priority),
                    item.assignee().map_or("-", AssigneeId::as_str),
                    item.content().title()
                ));
            }
            if items.is_empty() {
                output.push_str("No items.\n");
            }
            if let Some(offset) = next_offset {
                output.push_str(&format!("Next offset: {}\n", offset.get()));
            }
            Ok(output)
        }
    }
}

fn render_history(
    events: &[application::ItemHistoryEvent],
    format: OutputFormat,
) -> Result<String, ReadError> {
    match format {
        OutputFormat::Json => json_line(json!({
            "events": events.iter().map(rpc_read::history_json).collect::<Vec<_>>()
        })),
        OutputFormat::Human => {
            let mut output = String::new();
            for event in events {
                output.push_str(&format!(
                    "r{}.{}\t{}\t{}\t{}\n",
                    event.item_revision.get(),
                    event.event_index,
                    rpc_read::event_type(event.event_type),
                    event.occurred_at.as_str(),
                    event.actor.id
                ));
            }
            Ok(output)
        }
    }
}

fn json_line(value: Value) -> Result<String, ReadError> {
    serde_json::to_string(&value)
        .map(|mut output| {
            output.push('\n');
            output
        })
        .map_err(operation)
}

fn parse_item_id(value: &str) -> Result<ItemId, ReadError> {
    let mut parts = value.split(':');
    let item = ItemId::new(
        RequesterId::new(parts.next().unwrap_or_default()).map_err(input)?,
        ProjectId::new(parts.next().unwrap_or_default()).map_err(input)?,
        parts
            .next()
            .ok_or_else(|| ReadError::usage("invalid item ID"))?
            .parse()
            .map_err(|_| ReadError::usage("invalid item ID"))?,
    )
    .map_err(input)?;
    if parts.next().is_some() || item.to_string() != value {
        Err(ReadError::usage("invalid item ID"))
    } else {
        Ok(item)
    }
}

fn parse_status(value: &str) -> Result<Status, ReadError> {
    match value {
        "proposed" => Ok(Status::Proposed),
        "ready" => Ok(Status::Ready),
        "in_progress" => Ok(Status::InProgress),
        "blocked" => Ok(Status::Blocked),
        "done" => Ok(Status::Done),
        "rejected" => Ok(Status::Rejected),
        _ => Err(ReadError::usage(format!("unknown status {value:?}"))),
    }
}

fn parse_priority(value: &str) -> Result<Priority, ReadError> {
    match value {
        "P0" => Ok(Priority::P0),
        "P1" => Ok(Priority::P1),
        "P2" => Ok(Priority::P2),
        "P3" => Ok(Priority::P3),
        "P4" => Ok(Priority::P4),
        _ => Err(ReadError::usage(format!("unknown priority {value:?}"))),
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

fn number(value: Option<&str>, default: usize, name: &str) -> Result<usize, ReadError> {
    value
        .map(str::parse)
        .transpose()
        .map_err(|_| ReadError::usage(format!("--{name} requires a non-negative integer")))
        .map(|value| value.unwrap_or(default))
}

fn input(error: impl fmt::Display) -> ReadError {
    ReadError::usage(error.to_string())
}

fn operation(error: impl fmt::Display) -> ReadError {
    ReadError::operation(error.to_string())
}

fn unauthorized() -> ReadError {
    ReadError {
        message: "the requested operation is not authorized".into(),
        exit_code: ErrorCode::Unauthorized.exit_code(),
        usage: false,
    }
}

fn storage_error(error: &impl ClassifyReadStorageError) -> ReadError {
    match error.rpc_read_kind() {
        ReadStorageErrorKind::Busy => ReadError {
            message: "the BIF store is busy".into(),
            exit_code: ErrorCode::StorageBusy.exit_code(),
            usage: false,
        },
        ReadStorageErrorKind::InvalidPersistedData | ReadStorageErrorKind::Other => {
            ReadError::operation("an internal error occurred")
        }
    }
}
