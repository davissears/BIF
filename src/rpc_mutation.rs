//! Mutation operation adapter for BIF RPC v1.
//!
//! Authentication facts are supplied by the embedding host, rather than read
//! from the untrusted RPC payload.

use std::fmt::Display;

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use crate::application::{
    self, AuthorizationRequest, CaptureInput, CaptureRequest, CaptureStore, Clock, Command,
    HumanAuthorization, IdentityGenerator, MutationIdentityGenerator, MutationRequest,
    MutationStore,
};
use crate::domain::{
    AssigneeId, ItemContent, ItemId, ItemMutation, LifecycleMutation, MessageId, Priority,
    ProjectId, Provenance, RepositoryReference, RequesterId, Revision, RevisionReference,
    SourceHost, SourceUrl, ThreadId, Triage, TriageField,
};
use crate::rpc::{Dispatcher, ErrorCode, Operation, Request, RpcError, item_json};

/// Trusted attribution attached to every request handled by an adapter.
#[derive(Clone, Copy)]
pub struct MutationAuthorization<'a> {
    pub actor: application::Actor<'a>,
    pub execution: application::Execution<'a>,
    pub observed_execution: application::ObservedExecution<'a>,
    pub human_authorization: Option<HumanAuthorization<'a>>,
}

/// RPC adapter over the existing capture and mutation application use cases.
pub struct MutationDispatcher<'a, S, C, I> {
    pub store: &'a mut S,
    pub clock: &'a mut C,
    pub identities: &'a mut I,
    pub authorization: MutationAuthorization<'a>,
}

impl<S, C, I> Dispatcher for MutationDispatcher<'_, S, C, I>
where
    S: CaptureStore + MutationStore,
    <S as CaptureStore>::Error: Display,
    <S as MutationStore>::Error: Display,
    C: Clock,
    I: IdentityGenerator + MutationIdentityGenerator,
{
    fn dispatch(&mut self, request: Request) -> Result<Value, RpcError> {
        match request.operation {
            Operation::Capture => self.capture(request.params),
            Operation::Triage
            | Operation::Approve
            | Operation::Reject
            | Operation::Prioritize
            | Operation::Assign
            | Operation::Start
            | Operation::Block
            | Operation::Resume
            | Operation::Finish => self.mutate(request.operation, request.params),
            _ => Err(internal()),
        }
    }
}

impl<S, C, I> MutationDispatcher<'_, S, C, I>
where
    S: CaptureStore + MutationStore,
    <S as CaptureStore>::Error: Display,
    <S as MutationStore>::Error: Display,
    C: Clock,
    I: IdentityGenerator + MutationIdentityGenerator,
{
    fn capture(&mut self, params: Map<String, Value>) -> Result<Value, RpcError> {
        let params: CaptureParams = decode(params)?;
        let provenance = params.provenance.unwrap_or_default().try_into()?;
        let input = CaptureInput {
            requester: RequesterId::new(params.requester).map_err(|_| invalid())?,
            project: ProjectId::new(params.project).map_err(|_| invalid())?,
            content: ItemContent::new(params.title, params.description, params.acceptance_criteria)
                .map_err(|_| invalid())?,
            provenance,
        };
        let authorization = authorization_request(self.authorization, Command::Capture);
        application::capture(
            self.store,
            self.clock,
            self.identities,
            &authorization,
            CaptureRequest {
                idempotency_key: nonempty(params.idempotency_key)?,
                input,
            },
        )
        .map(|result| json!({"item": item_json(&result.item), "replayed": result.replayed}))
        .map_err(map_capture_error)
    }

    fn mutate(
        &mut self,
        operation: Operation,
        params: Map<String, Value>,
    ) -> Result<Value, RpcError> {
        let common: MutationParams = decode(params)?;
        let (mutation, changes) = common.into_mutation(operation)?;
        let change_refs = changes.iter().map(String::as_str).collect::<Vec<_>>();
        let authorization = authorization_request(
            self.authorization,
            Command::Mutation {
                requested_changes: &change_refs,
            },
        );
        application::mutate_item_idempotent(
            self.store,
            self.clock,
            self.identities,
            &authorization,
            MutationRequest {
                idempotency_key: nonempty(common.idempotency_key)?,
                item_id: parse_item_id(&common.item_id)?,
                expected_revision: Revision::new(common.expected_revision)
                    .map_err(|_| invalid())?,
                mutation,
            },
        )
        .map(|result| json!({"item": item_json(&result.item), "replayed": result.replayed}))
        .map_err(map_mutation_error)
    }
}

fn authorization_request<'a>(
    facts: MutationAuthorization<'a>,
    command: Command<'a>,
) -> AuthorizationRequest<'a> {
    AuthorizationRequest {
        actor: facts.actor,
        execution: facts.execution,
        observed_execution: facts.observed_execution,
        command,
        human_authorization: facts.human_authorization,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureParams {
    idempotency_key: String,
    requester: String,
    project: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    provenance: Option<ProvenanceParams>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvenanceParams {
    source_host: Option<String>,
    thread_id: Option<String>,
    message_id: Option<String>,
    url: Option<String>,
    repository_reference: Option<String>,
    revision_reference: Option<String>,
    context_excerpt: Option<String>,
}

impl TryFrom<ProvenanceParams> for Provenance {
    type Error = RpcError;
    fn try_from(value: ProvenanceParams) -> Result<Self, Self::Error> {
        let host = match value.source_host.as_deref() {
            None => None,
            Some("delta") => Some(SourceHost::Delta),
            Some("codex") => Some(SourceHost::Codex),
            Some("local") => Some(SourceHost::Local),
            Some(_) => return Err(invalid()),
        };
        Ok(Provenance::new(
            host,
            value.thread_id.map(ThreadId::new),
            value.message_id.map(MessageId::new),
            value.url.map(SourceUrl::new),
            value.repository_reference.map(RepositoryReference::new),
            value.revision_reference.map(RevisionReference::new),
            value.context_excerpt,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationParams {
    idempotency_key: String,
    item_id: String,
    expected_revision: u64,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    priority: NullableField<String>,
    #[serde(default)]
    assignee: NullableField<String>,
    #[serde(default)]
    note: Option<String>,
}

impl MutationParams {
    fn into_mutation(&self, operation: Operation) -> Result<(ItemMutation, Vec<String>), RpcError> {
        let lifecycle = match operation {
            Operation::Approve => exact_empty(self, LifecycleMutation::Approve)?,
            Operation::Reject => reason_only(self, true)?,
            Operation::Start => exact_empty(self, LifecycleMutation::Start)?,
            Operation::Block => reason_only(self, false)?,
            Operation::Resume => exact_empty(self, LifecycleMutation::Resume)?,
            Operation::Finish => exact_empty(self, LifecycleMutation::Finish)?,
            Operation::Triage => match self.action.as_deref() {
                None => None,
                Some("approve") => Some(LifecycleMutation::Approve),
                Some("reject") => Some(LifecycleMutation::Reject {
                    reason: nonempty(self.reason.clone().ok_or_else(invalid)?)?,
                }),
                Some("start") => Some(LifecycleMutation::Start),
                Some("block") => Some(LifecycleMutation::Block {
                    reason: nonempty(self.reason.clone().ok_or_else(invalid)?)?,
                }),
                Some("resume") => Some(LifecycleMutation::Resume),
                Some("finish") => Some(LifecycleMutation::Finish),
                Some(_) => return Err(invalid()),
            },
            Operation::Prioritize | Operation::Assign => None,
            _ => return Err(invalid()),
        };
        let mut triage = Triage::default();
        triage.priority = field_priority(&self.priority)?;
        triage.assignee = field_assignee(&self.assignee)?;
        triage.note = self.note.clone();
        match operation {
            Operation::Prioritize if self.priority.is_omitted() => return Err(invalid()),
            Operation::Assign if self.assignee.is_omitted() => return Err(invalid()),
            Operation::Approve
            | Operation::Reject
            | Operation::Start
            | Operation::Block
            | Operation::Resume
            | Operation::Finish
                if !self.priority.is_omitted()
                    || !self.assignee.is_omitted()
                    || self.note.is_some() =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        let has_triage =
            !self.priority.is_omitted() || !self.assignee.is_omitted() || self.note.is_some();
        let triage = has_triage.then_some(triage);
        if lifecycle.is_none() && triage.is_none() {
            return Err(invalid());
        }
        let mut changes = Vec::new();
        if let Some(action) = self.action.as_deref().or_else(|| operation_name(operation)) {
            changes.push(action.to_owned());
        }
        if let NullableField::Value(value) = &self.priority {
            changes.push(format!(
                "prioritize:{}",
                value.as_deref().unwrap_or("clear")
            ));
        }
        if let NullableField::Value(value) = &self.assignee {
            changes.push(format!("assign:{}", value.as_deref().unwrap_or("clear")));
        }
        if self.note.is_some() {
            changes.push("note".into());
        }
        Ok((ItemMutation { lifecycle, triage }, changes))
    }
}

#[derive(Default)]
enum NullableField<T> {
    #[default]
    Omitted,
    Value(Option<T>),
}

impl<T> NullableField<T> {
    fn is_omitted(&self) -> bool {
        matches!(self, Self::Omitted)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for NullableField<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(Self::Value)
    }
}

fn exact_empty(
    params: &MutationParams,
    value: LifecycleMutation,
) -> Result<Option<LifecycleMutation>, RpcError> {
    if params.action.is_some() || params.reason.is_some() {
        Err(invalid())
    } else {
        Ok(Some(value))
    }
}
fn reason_only(
    params: &MutationParams,
    reject: bool,
) -> Result<Option<LifecycleMutation>, RpcError> {
    if params.action.is_some() {
        return Err(invalid());
    }
    let reason = nonempty(params.reason.clone().ok_or_else(invalid)?)?;
    Ok(Some(if reject {
        LifecycleMutation::Reject { reason }
    } else {
        LifecycleMutation::Block { reason }
    }))
}
fn operation_name(operation: Operation) -> Option<&'static str> {
    Some(match operation {
        Operation::Approve => "approve",
        Operation::Reject => "reject",
        Operation::Start => "start",
        Operation::Block => "block",
        Operation::Resume => "resume",
        Operation::Finish => "finish",
        _ => return None,
    })
}
fn field_priority(value: &NullableField<String>) -> Result<TriageField<Priority>, RpcError> {
    match value {
        NullableField::Omitted => Ok(TriageField::Omitted),
        NullableField::Value(None) => Ok(TriageField::Clear),
        NullableField::Value(Some(v)) => Ok(TriageField::Set(match v.as_str() {
            "P0" => Priority::P0,
            "P1" => Priority::P1,
            "P2" => Priority::P2,
            "P3" => Priority::P3,
            "P4" => Priority::P4,
            _ => return Err(invalid()),
        })),
    }
}
fn field_assignee(value: &NullableField<String>) -> Result<TriageField<AssigneeId>, RpcError> {
    match value {
        NullableField::Omitted => Ok(TriageField::Omitted),
        NullableField::Value(None) => Ok(TriageField::Clear),
        NullableField::Value(Some(v)) => {
            Ok(TriageField::Set(AssigneeId::new(v).map_err(|_| invalid())?))
        }
    }
}
fn decode<T: for<'de> Deserialize<'de>>(params: Map<String, Value>) -> Result<T, RpcError> {
    serde_json::from_value(Value::Object(params)).map_err(|_| invalid())
}
fn nonempty(value: String) -> Result<String, RpcError> {
    if value.trim().is_empty() {
        Err(invalid())
    } else {
        Ok(value)
    }
}
fn parse_item_id(value: &str) -> Result<ItemId, RpcError> {
    let mut parts = value.split(':');
    let (requester, project, sequence) = (parts.next(), parts.next(), parts.next());
    if parts.next().is_some() {
        return Err(invalid());
    }
    let item = ItemId::new(
        RequesterId::new(requester.ok_or_else(invalid)?).map_err(|_| invalid())?,
        ProjectId::new(project.ok_or_else(invalid)?).map_err(|_| invalid())?,
        sequence
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?,
    )
    .map_err(|_| invalid())?;
    if item.to_string() != value {
        return Err(invalid());
    }
    Ok(item)
}

fn invalid() -> RpcError {
    RpcError::invalid_input()
}
fn internal() -> RpcError {
    RpcError::new(
        ErrorCode::Internal,
        "An internal error occurred",
        Map::new(),
    )
}
fn stable(code: ErrorCode, message: &'static str) -> RpcError {
    RpcError::new(code, message, Map::new())
}
fn map_capture_error<E: Display>(error: application::CaptureError<E>) -> RpcError {
    match error {
        application::CaptureError::Unauthorized(_) => stable(
            ErrorCode::Unauthorized,
            "The requested operation is not authorized",
        ),
        application::CaptureError::IdempotencyConflict => stable(
            ErrorCode::IdempotencyConflict,
            "The idempotency key was already used with different input",
        ),
        application::CaptureError::Busy(_) => {
            stable(ErrorCode::StorageBusy, "The BIF store is busy")
        }
        application::CaptureError::Storage(_) => internal(),
    }
}
fn map_mutation_error<E: Display>(error: application::MutationUseCaseError<E>) -> RpcError {
    match error {
        application::MutationUseCaseError::Unauthorized(_) => stable(
            ErrorCode::Unauthorized,
            "The requested operation is not authorized",
        ),
        application::MutationUseCaseError::NotFound => {
            stable(ErrorCode::NotFound, "Item was not found")
        }
        application::MutationUseCaseError::VersionConflict => stable(
            ErrorCode::VersionConflict,
            "The item revision does not match",
        ),
        application::MutationUseCaseError::IdempotencyConflict => stable(
            ErrorCode::IdempotencyConflict,
            "The idempotency key was already used with different input",
        ),
        application::MutationUseCaseError::Invalid(e) if e.code() == "invalid_transition" => {
            stable(
                ErrorCode::InvalidTransition,
                "The requested status transition is not allowed",
            )
        }
        application::MutationUseCaseError::Invalid(_) => invalid(),
        application::MutationUseCaseError::Busy(_) => {
            stable(ErrorCode::StorageBusy, "The BIF store is busy")
        }
        application::MutationUseCaseError::Storage(_) => internal(),
    }
}
