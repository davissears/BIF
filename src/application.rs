//! Use cases, authorization, and operation coordination.
//!
//! This layer may depend on [`crate::domain`], but not on delivery or
//! infrastructure modules.

use std::{error::Error, fmt};

use crate::domain::{
    Item, ItemContent, ItemId, ItemMutation, MutationError, ProjectId, Provenance, RequesterId,
    Revision, Timestamp,
};

/// The principal requesting an operation.
///
/// A human remains the actor when an agent executes that human's instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Actor<'a> {
    pub kind: ActorKind,
    pub id: &'a str,
    pub surface: &'a str,
    pub host: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorKind {
    Human,
    Agent,
}

/// Execution attribution declared by the request adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Execution<'a> {
    Direct {
        surface: &'a str,
        host: &'a str,
    },
    Agent {
        agent_id: &'a str,
        surface: &'a str,
        host: &'a str,
    },
}

/// Execution facts authenticated by the host adapter.
///
/// Keeping these facts separate prevents request content from forging or
/// concealing the principal that actually submitted the operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedExecution<'a> {
    Direct,
    Agent { agent_id: &'a str },
}

/// The effective application command, after any transport envelope is removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command<'a> {
    Read,
    Capture,
    Mutation {
        /// Every atomic change the operation will perform.
        requested_changes: &'a [&'a str],
    },
}

/// Human authorization authenticated by the host adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HumanAuthorization<'a> {
    Direct {
        trusted: bool,
    },
    ExplicitInstruction {
        instruction_id: &'a str,
        instruction_text: &'a str,
        /// Machine-interpreted scope supplied as trusted conversation
        /// metadata, never inferred from item or command content.
        authorized_changes: &'a [&'a str],
        trusted: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizationRequest<'a> {
    pub actor: Actor<'a>,
    pub execution: Execution<'a>,
    pub observed_execution: ObservedExecution<'a>,
    pub command: Command<'a>,
    pub human_authorization: Option<HumanAuthorization<'a>>,
}

/// Stable authorization failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unauthorized;

impl Unauthorized {
    pub const fn code(self) -> &'static str {
        "unauthorized"
    }
}

impl fmt::Display for Unauthorized {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("request is not authorized")
    }
}

impl Error for Unauthorized {}

/// Applies the host-neutral BIF authorization policy without side effects.
pub fn authorize(request: &AuthorizationRequest<'_>) -> Result<(), Unauthorized> {
    validate_attribution(request)?;

    match request.command {
        Command::Read | Command::Capture => Ok(()),
        Command::Mutation { requested_changes } => authorize_mutation(request, requested_changes),
    }
}

/// Validated domain data needed to capture one item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureInput {
    pub requester: RequesterId,
    pub project: ProjectId,
    pub content: ItemContent,
    pub provenance: Provenance,
}

/// A caller-generated capture key paired with the exact validated payload.
///
/// Construct this once at the transport boundary and retain the value and
/// payload unchanged for every retry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureRequest {
    pub idempotency_key: String,
    pub input: CaptureInput,
}

/// The durable result of a capture attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureResult {
    pub item: Item,
    pub replayed: bool,
}

/// Server-generated values attached to one capture operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureIdentity {
    pub operation_id: String,
    pub event_id: String,
}

/// Supplies timestamps to application use cases.
pub trait Clock {
    fn now(&mut self) -> Timestamp;
}

/// Supplies opaque operation and event identities.
pub trait IdentityGenerator {
    fn capture_identity(&mut self) -> CaptureIdentity;
}

/// Server-generated identities for one mutation and its ordered events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationIdentity {
    pub operation_id: String,
    pub event_ids: Vec<String>,
}

/// A caller-generated key paired with one complete, validated mutation intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationRequest {
    pub idempotency_key: String,
    pub item_id: ItemId,
    pub expected_revision: Revision,
    pub mutation: ItemMutation,
}

/// The durable result of an item mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationResult {
    pub item: Item,
    pub replayed: bool,
}

/// Supplies opaque identities for mutation persistence.
pub trait MutationIdentityGenerator {
    fn mutation_identity(&mut self) -> MutationIdentity;
}

/// Atomic persistence boundary required by item mutation.
pub trait MutationStore {
    type Error;

    fn mutate(
        &mut self,
        request: MutationRequest,
        payload_hash: String,
        actor: Actor<'_>,
        execution: Execution<'_>,
        occurred_at: Timestamp,
        identity: MutationIdentity,
    ) -> Result<MutationResult, MutationStoreError<Self::Error>>;
}

#[derive(Debug)]
pub enum MutationStoreError<StorageError> {
    NotFound,
    VersionConflict,
    IdempotencyConflict,
    Invalid(MutationError),
    Busy(StorageError),
    Storage(StorageError),
}

#[derive(Debug)]
pub enum MutationUseCaseError<StorageError> {
    Unauthorized(Unauthorized),
    NotFound,
    VersionConflict,
    IdempotencyConflict,
    Invalid(MutationError),
    Busy(StorageError),
    Storage(StorageError),
}

impl<StorageError> MutationUseCaseError<StorageError> {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized(_) => "unauthorized",
            Self::NotFound => "not_found",
            Self::VersionConflict => "version_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Invalid(error) => error.code(),
            Self::Busy(_) => "storage_busy",
            Self::Storage(_) => "storage_error",
        }
    }
}

impl<StorageError: fmt::Display> fmt::Display for MutationUseCaseError<StorageError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized(error) => error.fmt(formatter),
            Self::NotFound => formatter.write_str("item was not found"),
            Self::VersionConflict => formatter.write_str("item revision does not match"),
            Self::IdempotencyConflict => {
                formatter.write_str("the idempotency key was already used with different input")
            }
            Self::Invalid(error) => error.fmt(formatter),
            Self::Busy(error) => write!(formatter, "mutation storage is busy: {error}"),
            Self::Storage(error) => write!(formatter, "mutation storage error: {error}"),
        }
    }
}

impl<StorageError: Error + 'static> Error for MutationUseCaseError<StorageError> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unauthorized(error) => Some(error),
            Self::Invalid(error) => Some(error),
            Self::Busy(error) | Self::Storage(error) => Some(error),
            Self::NotFound | Self::VersionConflict | Self::IdempotencyConflict => None,
        }
    }
}

/// Authorizes, validates, and atomically persists one complete intended mutation.
pub fn mutate_item_idempotent<S: MutationStore>(
    store: &mut S,
    clock: &mut impl Clock,
    identities: &mut impl MutationIdentityGenerator,
    authorization: &AuthorizationRequest<'_>,
    request: MutationRequest,
) -> Result<MutationResult, MutationUseCaseError<S::Error>> {
    if !matches!(authorization.command, Command::Mutation { .. }) {
        return Err(MutationUseCaseError::Unauthorized(Unauthorized));
    }
    authorize(authorization).map_err(MutationUseCaseError::Unauthorized)?;
    let payload_hash =
        mutation_payload_hash(&request, authorization.actor, authorization.execution);
    store
        .mutate(
            request,
            payload_hash,
            authorization.actor,
            authorization.execution,
            clock.now(),
            identities.mutation_identity(),
        )
        .map_err(|error| match error {
            MutationStoreError::NotFound => MutationUseCaseError::NotFound,
            MutationStoreError::VersionConflict => MutationUseCaseError::VersionConflict,
            MutationStoreError::IdempotencyConflict => MutationUseCaseError::IdempotencyConflict,
            MutationStoreError::Invalid(error) => MutationUseCaseError::Invalid(error),
            MutationStoreError::Busy(error) => MutationUseCaseError::Busy(error),
            MutationStoreError::Storage(error) => MutationUseCaseError::Storage(error),
        })
}

/// Backwards-compatible mutation entry point for callers that do not retry.
///
/// New adapters should use [`mutate_item_idempotent`] and retain their request
/// key across retries.
pub fn mutate_item<S: MutationStore>(
    store: &mut S,
    clock: &mut impl Clock,
    identities: &mut impl MutationIdentityGenerator,
    authorization: &AuthorizationRequest<'_>,
    item_id: &ItemId,
    expected_revision: Revision,
    mutation: ItemMutation,
) -> Result<Item, MutationUseCaseError<S::Error>> {
    let identity = identities.mutation_identity();
    let key = format!("legacy:{}", identity.operation_id);
    struct OneIdentity(Option<MutationIdentity>);
    impl MutationIdentityGenerator for OneIdentity {
        fn mutation_identity(&mut self) -> MutationIdentity {
            self.0.take().expect("identity is requested exactly once")
        }
    }
    mutate_item_idempotent(
        store,
        clock,
        &mut OneIdentity(Some(identity)),
        authorization,
        MutationRequest {
            idempotency_key: key,
            item_id: item_id.clone(),
            expected_revision,
            mutation,
        },
    )
    .map(|result| result.item)
}

fn mutation_payload_hash(
    request: &MutationRequest,
    actor: Actor<'_>,
    execution: Execution<'_>,
) -> String {
    let mut canonical = Vec::new();
    append_field(&mut canonical, Some(&request.item_id.to_string()));
    canonical.extend_from_slice(&request.expected_revision.get().to_be_bytes());
    append_field(
        &mut canonical,
        request
            .mutation
            .lifecycle
            .as_ref()
            .map(|value| match value {
                crate::domain::LifecycleMutation::Approve => "approve",
                crate::domain::LifecycleMutation::Reject { .. } => "reject",
                crate::domain::LifecycleMutation::Start => "start",
                crate::domain::LifecycleMutation::Block { .. } => "block",
                crate::domain::LifecycleMutation::Resume => "resume",
                crate::domain::LifecycleMutation::Finish => "finish",
            }),
    );
    match request.mutation.lifecycle.as_ref() {
        Some(crate::domain::LifecycleMutation::Reject { reason })
        | Some(crate::domain::LifecycleMutation::Block { reason }) => {
            append_field(&mut canonical, Some(reason))
        }
        _ => append_field(&mut canonical, None),
    }
    if let Some(triage) = &request.mutation.triage {
        append_field(&mut canonical, Some("triage"));
        append_field(&mut canonical, priority_field(&triage.priority));
        append_field(&mut canonical, assignee_field(&triage.assignee));
        append_field(&mut canonical, triage.note.as_deref());
    } else {
        append_field(&mut canonical, None);
    }
    append_field(
        &mut canonical,
        Some(match actor.kind {
            ActorKind::Human => "human",
            ActorKind::Agent => "agent",
        }),
    );
    for value in [actor.id, actor.surface, actor.host] {
        append_field(&mut canonical, Some(value));
    }
    match execution {
        Execution::Direct { surface, host } => {
            append_field(&mut canonical, Some("direct"));
            append_field(&mut canonical, None);
            append_field(&mut canonical, Some(surface));
            append_field(&mut canonical, Some(host));
        }
        Execution::Agent {
            agent_id,
            surface,
            host,
        } => {
            append_field(&mut canonical, Some("agent"));
            append_field(&mut canonical, Some(agent_id));
            append_field(&mut canonical, Some(surface));
            append_field(&mut canonical, Some(host));
        }
    }
    sha256(&canonical)
}

fn priority_field(value: &crate::domain::TriageField<crate::domain::Priority>) -> Option<&str> {
    match value {
        crate::domain::TriageField::Omitted => None,
        crate::domain::TriageField::Clear => Some("clear"),
        crate::domain::TriageField::Set(value) => Some(match value {
            crate::domain::Priority::P0 => "set:P0",
            crate::domain::Priority::P1 => "set:P1",
            crate::domain::Priority::P2 => "set:P2",
            crate::domain::Priority::P3 => "set:P3",
            crate::domain::Priority::P4 => "set:P4",
        }),
    }
}

fn assignee_field(value: &crate::domain::TriageField<crate::domain::AssigneeId>) -> Option<&str> {
    match value {
        crate::domain::TriageField::Omitted => None,
        crate::domain::TriageField::Clear => Some("clear"),
        crate::domain::TriageField::Set(value) => Some(value.as_str()),
    }
}

/// Atomic persistence boundary required by capture.
///
/// Implementations must allocate the sequence and write the item, operation,
/// and event in the same transaction.
pub trait CaptureStore {
    type Error;

    fn capture(
        &mut self,
        request: CaptureRequest,
        payload_hash: String,
        actor: Actor<'_>,
        execution: Execution<'_>,
        occurred_at: Timestamp,
        identity: CaptureIdentity,
    ) -> Result<CaptureResult, CaptureStoreError<Self::Error>>;
}

#[derive(Debug)]
pub enum CaptureStoreError<StorageError> {
    IdempotencyConflict,
    Busy(StorageError),
    Storage(StorageError),
}

/// Typed failures from the capture application use case.
#[derive(Debug)]
pub enum CaptureError<StorageError> {
    Unauthorized(Unauthorized),
    IdempotencyConflict,
    Busy(StorageError),
    Storage(StorageError),
}

impl<StorageError> CaptureError<StorageError> {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized(_) => "unauthorized",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Busy(_) => "storage_busy",
            Self::Storage(_) => "storage_error",
        }
    }
}

impl<StorageError: fmt::Display> fmt::Display for CaptureError<StorageError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized(error) => error.fmt(formatter),
            Self::IdempotencyConflict => {
                formatter.write_str("the idempotency key was already used with different input")
            }
            Self::Busy(error) => write!(formatter, "capture storage is busy: {error}"),
            Self::Storage(error) => write!(formatter, "capture storage error: {error}"),
        }
    }
}

impl<StorageError: Error + 'static> Error for CaptureError<StorageError> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unauthorized(error) => Some(error),
            Self::IdempotencyConflict => None,
            Self::Busy(error) => Some(error),
            Self::Storage(error) => Some(error),
        }
    }
}

/// Authorizes and atomically captures one canonical item.
pub fn capture<S: CaptureStore>(
    store: &mut S,
    clock: &mut impl Clock,
    identities: &mut impl IdentityGenerator,
    authorization: &AuthorizationRequest<'_>,
    request: CaptureRequest,
) -> Result<CaptureResult, CaptureError<S::Error>> {
    if authorization.command != Command::Capture {
        return Err(CaptureError::Unauthorized(Unauthorized));
    }
    authorize(authorization).map_err(CaptureError::Unauthorized)?;

    let payload_hash = capture_payload_hash(&request.input);
    store
        .capture(
            request,
            payload_hash,
            authorization.actor,
            authorization.execution,
            clock.now(),
            identities.capture_identity(),
        )
        .map_err(|error| match error {
            CaptureStoreError::IdempotencyConflict => CaptureError::IdempotencyConflict,
            CaptureStoreError::Busy(error) => CaptureError::Busy(error),
            CaptureStoreError::Storage(error) => CaptureError::Storage(error),
        })
}

fn capture_payload_hash(input: &CaptureInput) -> String {
    let mut canonical = Vec::new();
    append_field(&mut canonical, Some(input.requester.as_str()));
    append_field(&mut canonical, Some(input.project.as_str()));
    append_field(&mut canonical, Some(input.content.title()));
    append_field(&mut canonical, input.content.description());
    canonical.extend_from_slice(&(input.content.acceptance_criteria().len() as u64).to_be_bytes());
    for criterion in input.content.acceptance_criteria() {
        append_field(&mut canonical, Some(criterion));
    }
    let provenance = &input.provenance;
    append_field(
        &mut canonical,
        provenance.source_host().map(|host| match host {
            crate::domain::SourceHost::Delta => "delta",
            crate::domain::SourceHost::Codex => "codex",
            crate::domain::SourceHost::Local => "local",
        }),
    );
    append_field(
        &mut canonical,
        provenance.thread_id().map(|value| value.as_str()),
    );
    append_field(
        &mut canonical,
        provenance.message_id().map(|value| value.as_str()),
    );
    append_field(&mut canonical, provenance.url().map(|value| value.as_str()));
    append_field(
        &mut canonical,
        provenance
            .repository_reference()
            .map(|value| value.as_str()),
    );
    append_field(
        &mut canonical,
        provenance.revision_reference().map(|value| value.as_str()),
    );
    append_field(&mut canonical, provenance.context_excerpt());
    sha256(&canonical)
}

fn append_field(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => output.push(0),
        Some(value) => {
            output.push(1);
            output.extend_from_slice(&(value.len() as u64).to_be_bytes());
            output.extend_from_slice(value.as_bytes());
        }
    }
}

fn sha256(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut padded = input.to_vec();
    let bit_len = (padded.len() as u64).wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in words[..16].iter_mut().enumerate() {
            *word = u32::from_be_bytes(chunk[index * 4..index * 4 + 4].try_into().unwrap());
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let temp2 = sum0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (value, addition) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *value = value.wrapping_add(addition);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

fn validate_attribution(request: &AuthorizationRequest<'_>) -> Result<(), Unauthorized> {
    if request.actor.id.trim().is_empty()
        || request.actor.surface.trim().is_empty()
        || request.actor.host.trim().is_empty()
    {
        return Err(Unauthorized);
    }

    match (request.execution, request.observed_execution) {
        (Execution::Direct { surface, host }, ObservedExecution::Direct)
            if request.actor.kind == ActorKind::Human
                && surface == request.actor.surface
                && host == request.actor.host
                && !surface.trim().is_empty()
                && !host.trim().is_empty() =>
        {
            Ok(())
        }
        (
            Execution::Agent {
                agent_id,
                surface,
                host,
            },
            ObservedExecution::Agent {
                agent_id: observed_id,
            },
        ) if !agent_id.trim().is_empty()
            && agent_id == observed_id
            && surface == request.actor.surface
            && host == request.actor.host
            && !surface.trim().is_empty()
            && !host.trim().is_empty()
            && (request.actor.kind == ActorKind::Human || request.actor.id == agent_id) =>
        {
            Ok(())
        }
        _ => Err(Unauthorized),
    }
}

fn authorize_mutation(
    request: &AuthorizationRequest<'_>,
    requested_changes: &[&str],
) -> Result<(), Unauthorized> {
    if request.actor.kind != ActorKind::Human || requested_changes.is_empty() {
        return Err(Unauthorized);
    }

    match (request.execution, request.human_authorization) {
        (Execution::Direct { .. }, Some(HumanAuthorization::Direct { trusted: true })) => Ok(()),
        (
            Execution::Agent { .. },
            Some(HumanAuthorization::ExplicitInstruction {
                instruction_id,
                instruction_text,
                authorized_changes,
                trusted: true,
            }),
        ) if !instruction_id.trim().is_empty()
            && !instruction_text.trim().is_empty()
            && requested_changes
                .iter()
                .all(|change| authorized_changes.contains(change)) =>
        {
            Ok(())
        }
        _ => Err(Unauthorized),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Actor, ActorKind, AuthorizationRequest, Command, Execution, HumanAuthorization,
        ObservedExecution, authorize,
    };

    const FIXTURE: &str = include_str!("../docs/fixtures/bif-v1-authorization.json");
    const APPROVE_P1_ASSIGN: &[&str] = &["approve", "prioritize:P1", "assign:davis"];
    const P1_ASSIGN: &[&str] = &["prioritize:P1", "assign:davis"];
    const P1: &[&str] = &["prioritize:P1"];
    const START: &[&str] = &["start"];
    const FINISH: &[&str] = &["finish"];
    const APPROVE: &[&str] = &["approve"];

    struct Case<'a> {
        id: &'static str,
        request: AuthorizationRequest<'a>,
        allowed: bool,
    }

    fn actor(
        kind: ActorKind,
        id: &'static str,
        surface: &'static str,
        host: &'static str,
    ) -> Actor<'static> {
        Actor {
            kind,
            id,
            surface,
            host,
        }
    }

    fn cases() -> Vec<Case<'static>> {
        vec![
            Case {
                id: "read-agent-allowed",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Agent, "delta-agent:reader", "thread", "delta"),
                    execution: Execution::Agent {
                        agent_id: "delta-agent:reader",
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:reader",
                    },
                    command: Command::Read,
                    human_authorization: None,
                },
                allowed: true,
            },
            Case {
                id: "read-forged-human-attribution-denied",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Human, "DAVIS", "thread", "delta"),
                    execution: Execution::Direct {
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:reader",
                    },
                    command: Command::Read,
                    human_authorization: None,
                },
                allowed: false,
            },
            Case {
                id: "capture-autonomous-agent-allowed",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Agent, "delta-agent:capture", "thread", "delta"),
                    execution: Execution::Agent {
                        agent_id: "delta-agent:capture",
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:capture",
                    },
                    command: Command::Capture,
                    human_authorization: None,
                },
                allowed: true,
            },
            Case {
                id: "capture-missing-executing-agent-denied",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Agent, "delta-agent:capture", "thread", "delta"),
                    execution: Execution::Agent {
                        agent_id: "",
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:capture",
                    },
                    command: Command::Capture,
                    human_authorization: None,
                },
                allowed: false,
            },
            Case {
                id: "mutation-direct-human-allowed",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Human, "DAVIS", "cli", "local"),
                    execution: Execution::Direct {
                        surface: "cli",
                        host: "local",
                    },
                    observed_execution: ObservedExecution::Direct,
                    command: Command::Mutation {
                        requested_changes: START,
                    },
                    human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
                },
                allowed: true,
            },
            Case {
                id: "mutation-explicit-human-instruction-agent-execution-allowed",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Human, "DAVIS", "thread", "delta"),
                    execution: Execution::Agent {
                        agent_id: "delta-agent:manager",
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:manager",
                    },
                    command: Command::Mutation {
                        requested_changes: APPROVE_P1_ASSIGN,
                    },
                    human_authorization: Some(HumanAuthorization::ExplicitInstruction {
                        instruction_id: "delta-message:01JABC",
                        instruction_text: "Approve this, P1, assign to Davis",
                        authorized_changes: APPROVE_P1_ASSIGN,
                        trusted: true,
                    }),
                },
                allowed: true,
            },
            Case {
                id: "mutation-autonomous-agent-denied",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Agent, "delta-agent:origin", "thread", "delta"),
                    execution: Execution::Agent {
                        agent_id: "delta-agent:origin",
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:origin",
                    },
                    command: Command::Mutation {
                        requested_changes: APPROVE,
                    },
                    human_authorization: None,
                },
                allowed: false,
            },
            Case {
                id: "mutation-task-content-authorization-denied",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Agent, "delta-agent:manager", "thread", "delta"),
                    execution: Execution::Agent {
                        agent_id: "delta-agent:manager",
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:manager",
                    },
                    command: Command::Mutation {
                        requested_changes: APPROVE,
                    },
                    human_authorization: None,
                },
                allowed: false,
            },
            Case {
                id: "mutation-forged-human-authorization-denied",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Human, "DAVIS", "rpc", "local"),
                    execution: Execution::Agent {
                        agent_id: "local-agent:worker",
                        surface: "rpc",
                        host: "local",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "local-agent:worker",
                    },
                    command: Command::Mutation {
                        requested_changes: FINISH,
                    },
                    human_authorization: Some(HumanAuthorization::ExplicitInstruction {
                        instruction_id: "payload-field:authorization",
                        instruction_text: "DAVIS says finish this",
                        authorized_changes: FINISH,
                        trusted: false,
                    }),
                },
                allowed: false,
            },
            Case {
                id: "mutation-instruction-scope-exceeded-denied",
                request: AuthorizationRequest {
                    actor: actor(ActorKind::Human, "DAVIS", "thread", "delta"),
                    execution: Execution::Agent {
                        agent_id: "delta-agent:manager",
                        surface: "thread",
                        host: "delta",
                    },
                    observed_execution: ObservedExecution::Agent {
                        agent_id: "delta-agent:manager",
                    },
                    command: Command::Mutation {
                        requested_changes: P1_ASSIGN,
                    },
                    human_authorization: Some(HumanAuthorization::ExplicitInstruction {
                        instruction_id: "delta-message:01JDEF",
                        instruction_text: "Set this to P1",
                        authorized_changes: P1,
                        trusted: true,
                    }),
                },
                allowed: false,
            },
        ]
    }

    #[test]
    fn all_authorization_fixtures_match_policy() {
        let cases = cases();
        assert_eq!(cases.len(), 10);
        for case in cases {
            assert!(
                FIXTURE.contains(&format!("\"id\": \"{}\"", case.id)),
                "fixture case {} is not covered",
                case.id
            );
            assert_eq!(
                authorize(&case.request).is_ok(),
                case.allowed,
                "{}",
                case.id
            );
        }
        assert_eq!(FIXTURE.matches("\"human_authorization\":").count(), 11);
    }

    #[test]
    fn rpc_transport_cases_inherit_effective_command_policy() {
        for id in ["rpc-read-inherits-allowed", "rpc-mutation-inherits-denied"] {
            assert!(FIXTURE.contains(&format!("\"id\": \"{id}\"")));
        }
        assert_eq!(FIXTURE.matches("\"transport\": \"rpc\"").count(), 2);
    }

    #[test]
    fn rejects_mismatched_execution_attribution() {
        let request = AuthorizationRequest {
            actor: actor(ActorKind::Agent, "agent:one", "thread", "delta"),
            execution: Execution::Agent {
                agent_id: "agent:one",
                surface: "thread",
                host: "delta",
            },
            observed_execution: ObservedExecution::Agent {
                agent_id: "agent:two",
            },
            command: Command::Read,
            human_authorization: None,
        };
        assert_eq!(authorize(&request).unwrap_err().code(), "unauthorized");
    }
}
