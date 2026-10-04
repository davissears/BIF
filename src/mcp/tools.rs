//! The complete read-only tool surface: typed arguments and matching discovery.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    application::{ItemListFilters, ItemListOrdering, ItemProjectionKind, ItemTextFilter},
    domain::{AssigneeId, ItemId, NamedView, Priority, ProjectId, RequesterId, Status},
    read_session::ReadRequest,
};

/// A protocol-level invalid tool name or argument, not an application failure.
#[derive(Clone, Copy, Debug)]
pub struct InvalidToolArguments;

type Result<T> = std::result::Result<T, InvalidToolArguments>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    project: String,
    #[serde(default = "all")]
    view: String,
    #[serde(default = "newest_first")]
    ordering: String,
    #[serde(default = "summary")]
    projection: String,
    requester: Option<String>,
    assignee: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    #[serde(default)]
    unassigned: bool,
    text: Option<String>,
    #[serde(default = "limit")]
    limit: usize,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Get {
    project: String,
    item_id: String,
    #[serde(default = "summary")]
    projection: String,
    known_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    project: String,
    item_id: String,
    #[serde(default = "limit")]
    limit: usize,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedWork {
    project: String,
}

fn all() -> String {
    "all".into()
}
fn newest_first() -> String {
    "newest_first".into()
}
fn summary() -> String {
    "summary".into()
}
fn limit() -> usize {
    20
}

fn valid<T, E>(result: std::result::Result<T, E>) -> Result<T> {
    result.map_err(|_| InvalidToolArguments)
}

fn page_limit(limit: usize) -> Result<usize> {
    if (1..=100).contains(&limit) {
        Ok(limit)
    } else {
        Err(InvalidToolArguments)
    }
}

fn projection(value: &str) -> Result<ItemProjectionKind> {
    match value {
        "summary" => Ok(ItemProjectionKind::Summary),
        "work" => Ok(ItemProjectionKind::Work),
        "audit" => Ok(ItemProjectionKind::Audit),
        _ => Err(InvalidToolArguments),
    }
}

fn status(value: String) -> Result<Status> {
    match value.as_str() {
        "proposed" => Ok(Status::Proposed),
        "ready" => Ok(Status::Ready),
        "in_progress" => Ok(Status::InProgress),
        "blocked" => Ok(Status::Blocked),
        "done" => Ok(Status::Done),
        "rejected" => Ok(Status::Rejected),
        _ => Err(InvalidToolArguments),
    }
}

fn priority(value: String) -> Result<Priority> {
    match value.as_str() {
        "P0" => Ok(Priority::P0),
        "P1" => Ok(Priority::P1),
        "P2" => Ok(Priority::P2),
        "P3" => Ok(Priority::P3),
        "P4" => Ok(Priority::P4),
        _ => Err(InvalidToolArguments),
    }
}

fn scoped_item(project: &str, item: &str) -> Result<ItemId> {
    let mut parts = item.split(':');
    let requester = valid(RequesterId::new(parts.next().unwrap_or_default()))?;
    let item_project = valid(ProjectId::new(parts.next().unwrap_or_default()))?;
    let sequence = parts.next().ok_or(InvalidToolArguments)?;
    if sequence.is_empty() || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(InvalidToolArguments);
    }
    let item = valid(ItemId::new(
        requester,
        item_project,
        valid(sequence.parse::<u64>())?,
    ))?;
    if parts.next().is_some() || item.project() != &valid(ProjectId::new(project))? {
        return Err(InvalidToolArguments);
    }
    Ok(item)
}

/// Decode only the four advertised tools. Project scope is mandatory and cannot
/// be inferred from cwd or bypassed by an item's requester/project components.
pub fn decode_tool(name: &str, arguments: Value) -> Result<ReadRequest> {
    // Optional means omitted, not null: keep serde acceptance aligned to schemas.
    if !arguments.is_object() || arguments.as_object().unwrap().values().any(Value::is_null) {
        return Err(InvalidToolArguments);
    }
    match name {
        "bif_list" => {
            let args: List = valid(serde_json::from_value(arguments))?;
            if args.unassigned && args.assignee.is_some() {
                return Err(InvalidToolArguments);
            }
            Ok(ReadRequest::Page {
                view: valid(args.view.parse::<NamedView>())?,
                ordering: match args.ordering.as_str() {
                    "newest_first" => ItemListOrdering::NewestFirst,
                    "next" => ItemListOrdering::Next,
                    _ => return Err(InvalidToolArguments),
                },
                projection: projection(&args.projection)?,
                filters: ItemListFilters {
                    project: Some(valid(ProjectId::new(args.project))?),
                    requester: args
                        .requester
                        .map(RequesterId::new)
                        .map(valid)
                        .transpose()?,
                    assignee: args.assignee.map(AssigneeId::new).map(valid).transpose()?,
                    status: args.status.map(status).transpose()?,
                    priority: args.priority.map(priority).transpose()?,
                    unassigned: args.unassigned,
                    text: args.text.map(ItemTextFilter::new).map(valid).transpose()?,
                },
                limit: page_limit(args.limit)?,
                cursor: args.cursor,
            })
        }
        "bif_get" => {
            let args: Get = valid(serde_json::from_value(arguments))?;
            Ok(ReadRequest::Get {
                item_id: scoped_item(&args.project, &args.item_id)?,
                projection: projection(&args.projection)?,
                known_version: args.known_version,
                conditional: true,
            })
        }
        "bif_history" => {
            let args: History = valid(serde_json::from_value(arguments))?;
            Ok(ReadRequest::History {
                item_id: scoped_item(&args.project, &args.item_id)?,
                limit: page_limit(args.limit)?,
                cursor: args.cursor,
            })
        }
        "bif_selected_work" => {
            let args: SelectedWork = valid(serde_json::from_value(arguments))?;
            Ok(ReadRequest::SelectedWork {
                project: valid(ProjectId::new(args.project))?,
            })
        }
        _ => Err(InvalidToolArguments),
    }
}

/// Compact discovery. No outputSchema is promised because both v2 success and
/// v2 application-error envelopes are delivered as structuredContent.
pub fn tool_catalog() -> Value {
    let identifier = json!({"type":"string","pattern":"[A-Za-z0-9]"});
    let item_id = json!({"type":"string","pattern":"^[^:]*[A-Za-z0-9][^:]*:[^:]*[A-Za-z0-9][^:]*:[0-9]*[1-9][0-9]*$"});
    let projection = json!({"type":"string","enum":["summary","work","audit"]});
    let limit = json!({"type":"integer","minimum":1,"maximum":100,"default":20});
    let cursor = json!({"type":"string","description":"Opaque continuation from the previous page; repeat all other arguments."});
    let schema = |properties: Value, required: Value| {
        json!({
            "type":"object","properties":properties,"required":required,"additionalProperties":false
        })
    };
    let list_schema = |properties: Value| {
        let mut input = schema(properties, json!(["project"]));
        input["not"] = json!({
            "required":["assignee","unassigned"],"properties":{"unassigned":{"const":true}}
        });
        input
    };
    let tool = |name: &str, description: &str, input: Value| {
        json!({
            "name":name,"description":description,"inputSchema":input,
            "annotations":{"readOnlyHint":true,"destructiveHint":false,
                "idempotentHint":true,"openWorldHint":false}
        })
    };
    json!([
        tool(
            "bif_list",
            "Read a bounded named-view page in an explicit project.",
            list_schema(json!({
                "project":identifier,"view":{"type":"string","enum":["proposed","ready","active","blocked","done","rejected","mine","all"],"default":"all"},
                "ordering":{"type":"string","enum":["newest_first","next"],"default":"newest_first"},
                "projection":with_default(projection.clone(),"summary"),"requester":identifier,
                "assignee":identifier,"status":{"type":"string","enum":["proposed","ready","in_progress","blocked","done","rejected"]},
                "priority":{"type":"string","enum":["P0","P1","P2","P3","P4"]},
                "unassigned":{"type":"boolean","default":false},
                "text":{"type":"string","pattern":"\\S"},"limit":limit,"cursor":cursor
            }))
        ),
        tool(
            "bif_get",
            "Read one complete projection, or not_modified when known_version matches.",
            schema(
                json!({
                    "project":identifier,"item_id":item_id,"projection":with_default(projection,"summary"),
                    "known_version":{"type":"string","description":"Opaque version from a previous get of this projection."}
                }),
                json!(["project", "item_id"])
            )
        ),
        tool(
            "bif_history",
            "Read a bounded audit-event page for an item in an explicit project.",
            schema(
                json!({
                    "project":identifier,"item_id":item_id,"limit":limit,"cursor":cursor
                }),
                json!(["project", "item_id"])
            )
        ),
        tool(
            "bif_selected_work",
            "Read the selected ready work projection or an explicit empty outcome.",
            schema(
                json!({
                    "project":identifier
                }),
                json!(["project"])
            )
        )
    ])
}

fn with_default(mut schema: Value, default: &str) -> Value {
    schema["default"] = json!(default);
    schema
}
