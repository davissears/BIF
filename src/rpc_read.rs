//! Strict BIF RPC v1 adapters for authorized read operations.

use std::{fmt::Display, str::FromStr};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::application::{
    self, AuthorizationRequest, Command, ItemHistoryStore, ItemListFilters, ItemStore,
    NamedViewStore, PageOffset, PageSize, Pagination,
};
use crate::domain::{
    AssigneeId, EventValue, ItemId, NamedView, Priority, ProjectId, RequesterId, Status,
};
use crate::rpc::{Dispatcher, ErrorCode, Operation, Request, RpcError, item_json};

/// Trusted read identity and the requester used by the `mine` view.
#[derive(Clone, Copy)]
pub struct ReadAuthorization<'a> {
    pub request: AuthorizationRequest<'a>,
    pub configured_requester: &'a RequesterId,
}

/// Classification needed to keep infrastructure details out of RPC responses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadStorageErrorKind {
    Busy,
    InvalidPersistedData,
    Other,
}

pub trait ClassifyReadStorageError {
    fn rpc_read_kind(&self) -> ReadStorageErrorKind;
}

/// Dispatcher over the existing application read use cases.
pub struct ReadDispatcher<'a, S, H> {
    pub store: &'a S,
    pub history_store: &'a H,
    pub authorization: ReadAuthorization<'a>,
}

impl<S, H> Dispatcher for ReadDispatcher<'_, S, H>
where
    S: ItemStore + NamedViewStore<Error = <S as ItemStore>::Error>,
    H: ItemHistoryStore,
    <S as ItemStore>::Error: Display + ClassifyReadStorageError,
    H::Error: Display + ClassifyReadStorageError,
{
    fn dispatch(&mut self, request: Request) -> Result<Value, RpcError> {
        match request.operation {
            Operation::Get => self.get(request.params),
            Operation::List => self.list(request.params, false),
            Operation::Next => self.list(request.params, true),
            Operation::History => self.history(request.params),
            _ => Err(internal()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;
    use crate::application::{
        Actor, ActorKind, Execution, HumanAuthorization, ItemHistoryStoreError, ObservedExecution,
    };
    use crate::domain::Item;
    use crate::rpc::serve;

    #[derive(Debug)]
    struct TestError;
    impl fmt::Display for TestError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("test error")
        }
    }
    impl ClassifyReadStorageError for TestError {
        fn rpc_read_kind(&self) -> ReadStorageErrorKind {
            ReadStorageErrorKind::Other
        }
    }

    struct EmptyStore;
    impl ItemStore for EmptyStore {
        type Error = TestError;
        fn read_item(&self, _: &ItemId) -> Result<Option<Item>, Self::Error> {
            Ok(None)
        }
    }
    impl NamedViewStore for EmptyStore {
        type Error = TestError;
        fn select_items(
            &self,
            _: NamedView,
            _: &RequesterId,
            _: &ItemListFilters,
        ) -> Result<Vec<Item>, Self::Error> {
            Ok(Vec::new())
        }
    }
    impl ItemHistoryStore for EmptyStore {
        type Error = TestError;
        fn item_history(
            &self,
            _: &ItemId,
        ) -> Result<Vec<application::ItemHistoryEvent>, ItemHistoryStoreError<Self::Error>>
        {
            Err(ItemHistoryStoreError::NotFound)
        }
    }

    fn call(operation: &str, params: Value) -> (i32, Value) {
        let requester = RequesterId::new("DAVIS").unwrap();
        let store = EmptyStore;
        let request = AuthorizationRequest {
            actor: Actor {
                kind: ActorKind::Human,
                id: "DAVIS",
                surface: "rpc",
                host: "local",
            },
            execution: Execution::Direct {
                surface: "rpc",
                host: "local",
            },
            observed_execution: ObservedExecution::Direct,
            command: Command::Read,
            human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
        };
        let mut dispatcher = ReadDispatcher {
            store: &store,
            history_store: &store,
            authorization: ReadAuthorization {
                request,
                configured_requester: &requester,
            },
        };
        let input = json!({
            "protocol_version": 1, "request_id": "read-test",
            "operation": operation, "params": params
        })
        .to_string();
        let mut output = Vec::new();
        let code = serve(
            input.as_bytes(),
            &mut output,
            &mut Vec::new(),
            &mut dispatcher,
        )
        .unwrap();
        (code, serde_json::from_slice(&output).unwrap())
    }

    #[test]
    fn get_and_history_map_missing_items_to_the_stable_envelope() {
        for operation in ["get", "history"] {
            let (exit, response) = call(operation, json!({"item_id": "DAVIS:delta-db:001"}));
            assert_eq!(exit, 3);
            assert_eq!(response["error"]["code"], "not_found");
            assert_eq!(response["error"]["message"], "Item was not found");
        }
    }

    #[test]
    fn read_params_reject_unknown_fields_invalid_views_filters_and_pagination() {
        let cases = [
            (
                "get",
                json!({"item_id": "DAVIS:delta-db:001", "extra": true}),
            ),
            ("list", json!({"view": "Ready"})),
            ("list", json!({"text": "   "})),
            ("list", json!({"limit": 0})),
            ("next", json!({"view": "ready"})),
        ];
        for (operation, params) in cases {
            let (exit, response) = call(operation, params);
            assert_eq!(exit, 2);
            assert_eq!(response["error"]["code"], "invalid_input");
        }
    }

    #[test]
    fn list_accepts_every_filter_and_pagination_field() {
        let (exit, response) = call(
            "list",
            json!({
                "view": "all", "project": "delta-db", "requester": "DAVIS",
                "assignee": "worker", "status": "in_progress", "priority": "P1",
                "unassigned": false, "text": "rpc", "limit": 25, "offset": 10
            }),
        );
        assert_eq!(exit, 0);
        assert_eq!(
            response["result"],
            json!({"items": [], "next_offset": null})
        );
    }
}

impl<S, H> ReadDispatcher<'_, S, H>
where
    S: ItemStore + NamedViewStore<Error = <S as ItemStore>::Error>,
    H: ItemHistoryStore,
    <S as ItemStore>::Error: Display + ClassifyReadStorageError,
    H::Error: Display + ClassifyReadStorageError,
{
    fn authorization(&self) -> AuthorizationRequest<'_> {
        AuthorizationRequest {
            command: Command::Read,
            ..self.authorization.request
        }
    }

    fn get(&self, params: Map<String, Value>) -> Result<Value, RpcError> {
        let params: ItemParams = decode(params)?;
        application::read_item(
            self.store,
            &self.authorization(),
            &parse_item_id(&params.item_id)?,
        )
        .map(|item| json!({ "item": item_json(&item) }))
        .map_err(map_read_error)
    }

    fn history(&self, params: Map<String, Value>) -> Result<Value, RpcError> {
        let params: ItemParams = decode(params)?;
        application::read_item_history(
            self.history_store,
            &self.authorization(),
            &parse_item_id(&params.item_id)?,
        )
        .map(|events| json!({ "events": events.iter().map(history_json).collect::<Vec<_>>() }))
        .map_err(map_history_error)
    }

    fn list(&self, params: Map<String, Value>, next: bool) -> Result<Value, RpcError> {
        let params: ListParams = decode(params)?;
        if next && params.view.is_some() {
            return Err(invalid());
        }
        let filters = params.filters()?;
        let pagination = Pagination::new(
            PageSize::new(params.limit.unwrap_or(100)).map_err(|_| invalid())?,
            PageOffset::new(params.offset.unwrap_or(0)),
        );
        let result = if next {
            application::next_item_page(
                self.store,
                &self.authorization(),
                self.authorization.configured_requester,
                &filters,
                pagination,
            )
        } else {
            let view = NamedView::from_str(params.view.as_deref().unwrap_or("all"))
                .map_err(|_| invalid())?;
            application::list_item_page(
                self.store,
                &self.authorization(),
                view,
                self.authorization.configured_requester,
                &filters,
                pagination,
            )
        };
        result
            .map(|page| {
                json!({
                    "items": page.items.iter().map(item_json).collect::<Vec<_>>(),
                    "next_offset": page.next_offset.map(PageOffset::get)
                })
            })
            .map_err(map_view_error)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemParams {
    item_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    #[serde(default)]
    view: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    requester: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    unassigned: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

impl ListParams {
    fn filters(&self) -> Result<ItemListFilters, RpcError> {
        if self.unassigned && self.assignee.is_some() {
            return Err(invalid());
        }
        Ok(ItemListFilters {
            project: self
                .project
                .as_deref()
                .map(ProjectId::new)
                .transpose()
                .map_err(|_| invalid())?,
            requester: self
                .requester
                .as_deref()
                .map(RequesterId::new)
                .transpose()
                .map_err(|_| invalid())?,
            assignee: self
                .assignee
                .as_deref()
                .map(AssigneeId::new)
                .transpose()
                .map_err(|_| invalid())?,
            status: self.status.as_deref().map(parse_status).transpose()?,
            priority: self.priority.as_deref().map(parse_priority).transpose()?,
            unassigned: self.unassigned,
            text: self
                .text
                .clone()
                .map(application::ItemTextFilter::new)
                .transpose()
                .map_err(|_| invalid())?,
        })
    }
}

fn parse_status(value: &str) -> Result<Status, RpcError> {
    match value {
        "proposed" => Ok(Status::Proposed),
        "ready" => Ok(Status::Ready),
        "in_progress" => Ok(Status::InProgress),
        "blocked" => Ok(Status::Blocked),
        "done" => Ok(Status::Done),
        "rejected" => Ok(Status::Rejected),
        _ => Err(invalid()),
    }
}
fn parse_priority(value: &str) -> Result<Priority, RpcError> {
    match value {
        "P0" => Ok(Priority::P0),
        "P1" => Ok(Priority::P1),
        "P2" => Ok(Priority::P2),
        "P3" => Ok(Priority::P3),
        "P4" => Ok(Priority::P4),
        _ => Err(invalid()),
    }
}
fn parse_item_id(value: &str) -> Result<ItemId, RpcError> {
    let mut parts = value.split(':');
    let item = ItemId::new(
        RequesterId::new(parts.next().ok_or_else(invalid)?).map_err(|_| invalid())?,
        ProjectId::new(parts.next().ok_or_else(invalid)?).map_err(|_| invalid())?,
        parts
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?,
    )
    .map_err(|_| invalid())?;
    if parts.next().is_some() || item.to_string() != value {
        Err(invalid())
    } else {
        Ok(item)
    }
}
fn decode<T: for<'de> Deserialize<'de>>(params: Map<String, Value>) -> Result<T, RpcError> {
    serde_json::from_value(Value::Object(params)).map_err(|_| invalid())
}

pub(crate) fn history_json(event: &application::ItemHistoryEvent) -> Value {
    json!({
        "operation_id": event.operation_id, "event_id": event.event_id,
        "item_revision": event.item_revision.get(), "event_index": event.event_index,
        "event_type": event_type(event.event_type), "before": event.before.as_ref().map(event_value),
        "after": event.after.as_ref().map(event_value),
        "actor": {"kind": format!("{:?}", event.actor.kind).to_lowercase(), "id": event.actor.id,
            "surface": event.actor.surface, "host": event.actor.host},
        "execution": execution_json(&event.execution), "reason": event.reason, "note": event.note,
        "occurred_at": event.occurred_at.as_str(), "schema_version": event.schema_version
    })
}
pub(crate) fn event_type(value: crate::domain::EventType) -> String {
    let text = format!("{value:?}");
    text.chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if c.is_uppercase() && i > 0 {
                vec!['_', c.to_ascii_lowercase()]
            } else {
                vec![c.to_ascii_lowercase()]
            }
        })
        .collect()
}
fn event_value(value: &EventValue) -> Value {
    match value {
        EventValue::Status(v) => json!(
            format!("{v:?}")
                .to_lowercase()
                .replace("inprogress", "in_progress")
        ),
        EventValue::Priority(v) => json!(v.map(|p| format!("{p:?}"))),
        EventValue::Assignee(v) => json!(v.as_ref().map(AssigneeId::as_str)),
        EventValue::Note(v) => json!(v),
    }
}
fn execution_json(value: &application::EventExecution) -> Value {
    match value {
        application::EventExecution::Direct { surface, host } => {
            json!({"kind": "direct", "surface": surface, "host": host})
        }
        application::EventExecution::Agent {
            agent_id,
            surface,
            host,
        } => json!({"kind": "agent", "agent_id": agent_id, "surface": surface, "host": host}),
    }
}

fn invalid() -> RpcError {
    RpcError::invalid_input()
}
fn stable(code: ErrorCode, message: &'static str) -> RpcError {
    RpcError::new(code, message, Map::new())
}
fn internal() -> RpcError {
    stable(ErrorCode::Internal, "An internal error occurred")
}
fn storage<E: ClassifyReadStorageError>(error: &E) -> RpcError {
    match error.rpc_read_kind() {
        ReadStorageErrorKind::Busy => stable(ErrorCode::StorageBusy, "The BIF store is busy"),
        ReadStorageErrorKind::InvalidPersistedData | ReadStorageErrorKind::Other => internal(),
    }
}
fn map_read_error<E: ClassifyReadStorageError>(error: application::ReadItemError<E>) -> RpcError {
    match error {
        application::ReadItemError::Unauthorized(_) => stable(
            ErrorCode::Unauthorized,
            "The requested operation is not authorized",
        ),
        application::ReadItemError::NotFound => stable(ErrorCode::NotFound, "Item was not found"),
        application::ReadItemError::Storage(e) => storage(&e),
    }
}
fn map_view_error<E: ClassifyReadStorageError>(
    error: application::SelectNamedViewError<E>,
) -> RpcError {
    match error {
        application::SelectNamedViewError::Unauthorized(_) => stable(
            ErrorCode::Unauthorized,
            "The requested operation is not authorized",
        ),
        application::SelectNamedViewError::Storage(e) => storage(&e),
    }
}
fn map_history_error<E: ClassifyReadStorageError>(
    error: application::ItemHistoryError<E>,
) -> RpcError {
    match error {
        application::ItemHistoryError::Unauthorized(_) => stable(
            ErrorCode::Unauthorized,
            "The requested operation is not authorized",
        ),
        application::ItemHistoryError::NotFound => {
            stable(ErrorCode::NotFound, "Item was not found")
        }
        application::ItemHistoryError::InvalidPersistedData(e)
        | application::ItemHistoryError::Storage(e) => storage(&e),
    }
}
