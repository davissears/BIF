//! The process-transport shell for BIF RPC v1.
//!
//! Operation implementations deliberately live behind [`Dispatcher`]. This
//! module owns framing and the protocol envelope, but does not implement any
//! read or mutation operation.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::{self, Read, Write};

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value, json};

use crate::domain::{
    AssigneeId, Item, MessageId, RepositoryReference, RevisionReference, SourceUrl, ThreadId,
};

pub const PROTOCOL_VERSION: u64 = 1;
pub const MAXIMUM_INPUT_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Capture,
    Get,
    List,
    Next,
    History,
    Triage,
    Approve,
    Reject,
    Prioritize,
    Assign,
    Start,
    Block,
    Resume,
    Finish,
}

impl Operation {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "capture" => Self::Capture,
            "get" => Self::Get,
            "list" => Self::List,
            "next" => Self::Next,
            "history" => Self::History,
            "triage" => Self::Triage,
            "approve" => Self::Approve,
            "reject" => Self::Reject,
            "prioritize" => Self::Prioritize,
            "assign" => Self::Assign,
            "start" => Self::Start,
            "block" => Self::Block,
            "resume" => Self::Resume,
            "finish" => Self::Finish,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub struct Request {
    pub request_id: String,
    pub operation: Operation,
    pub params: Map<String, Value>,
}

/// Boundary implemented by BIF-037 and BIF-038 operation adapters.
///
/// Each adapter is responsible for strict, operation-specific validation of
/// `params`; returning [`RpcError::invalid_input`] rejects unknown fields.
pub trait Dispatcher {
    fn dispatch(&mut self, request: Request) -> Result<Value, RpcError>;
}

/// Serializes an item once for every RPC operation that returns canonical state.
pub(crate) fn item_json(item: &Item) -> Value {
    let provenance = item.provenance();
    json!({
        "id": item.id().to_string(),
        "requester": item.id().requester().as_str(),
        "project": item.id().project().as_str(),
        "sequence": item.id().sequence(),
        "title": item.content().title(),
        "description": item.content().description(),
        "acceptance_criteria": item.content().acceptance_criteria(),
        "status": format!("{:?}", item.status()).to_lowercase().replace("inprogress", "in_progress"),
        "priority": item.priority().map(|value| format!("{value:?}")),
        "assignee": item.assignee().map(AssigneeId::as_str),
        "status_reason": item.status_reason(),
        "revision": item.revision().get(),
        "captured_at": item.captured_at().as_str(),
        "updated_at": item.updated_at().as_str(),
        "provenance": {
            "source_host": provenance.source_host().map(|value| format!("{value:?}").to_lowercase()),
            "thread_id": provenance.thread_id().map(ThreadId::as_str),
            "message_id": provenance.message_id().map(MessageId::as_str),
            "url": provenance.url().map(SourceUrl::as_str),
            "repository_reference": provenance.repository_reference().map(RepositoryReference::as_str),
            "revision_reference": provenance.revision_reference().map(RevisionReference::as_str),
            "context_excerpt": provenance.context_excerpt()
        }
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    NotFound,
    Unauthorized,
    InvalidTransition,
    VersionConflict,
    IdempotencyConflict,
    UnsupportedVersion,
    NotInitialized,
    StorageBusy,
    Internal,
}

impl ErrorCode {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidInput => 2,
            Self::NotFound => 3,
            Self::Unauthorized => 4,
            Self::InvalidTransition => 5,
            Self::VersionConflict => 6,
            Self::IdempotencyConflict => 7,
            Self::UnsupportedVersion => 8,
            Self::NotInitialized => 9,
            Self::StorageBusy => 10,
            Self::Internal => 1,
        }
    }
}

#[derive(Debug)]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Map<String, Value>,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>, details: Map<String, Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }

    pub fn invalid_input() -> Self {
        Self::new(
            ErrorCode::InvalidInput,
            "Request input is not valid BIF RPC v1",
            Map::new(),
        )
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: ErrorCode,
    message: String,
    details: Map<String, Value>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Response {
    Success {
        protocol_version: u64,
        request_id: String,
        ok: bool,
        result: Value,
    },
    Error {
        protocol_version: u64,
        request_id: Option<String>,
        ok: bool,
        error: ErrorBody,
    },
}

/// Reads one bounded request and writes one newline-terminated response.
///
/// The returned value is the protocol's exact process exit code. A caller
/// should use it as its process status after the output streams are flushed.
pub fn serve<R, W, E, D>(
    input: R,
    mut output: W,
    mut diagnostics: E,
    dispatcher: &mut D,
) -> io::Result<i32>
where
    R: Read,
    W: Write,
    E: Write,
    D: Dispatcher,
{
    let mut bytes = Vec::new();
    input
        .take((MAXIMUM_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;

    let outcome = if bytes.len() > MAXIMUM_INPUT_BYTES {
        Err((None, RpcError::invalid_input()))
    } else {
        parse_request(&bytes).and_then(|request| {
            let request_id = request.request_id.clone();
            dispatcher
                .dispatch(request)
                .map(|result| (request_id.clone(), result))
                .map_err(|error| (Some(request_id), error))
        })
    };

    let (response, exit_code) = match outcome {
        Ok((request_id, result)) => (
            Response::Success {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                ok: true,
                result,
            },
            0,
        ),
        Err((request_id, error)) => {
            writeln!(diagnostics, "BIF RPC error: {}", error.message)?;
            let exit_code = error.code.exit_code();
            (
                Response::Error {
                    protocol_version: PROTOCOL_VERSION,
                    request_id,
                    ok: false,
                    error: ErrorBody {
                        code: error.code,
                        message: error.message,
                        details: error.details,
                    },
                },
                exit_code,
            )
        }
    };
    serde_json::to_writer(&mut output, &response)?;
    writeln!(output)?;
    Ok(exit_code)
}

fn parse_request(bytes: &[u8]) -> Result<Request, (Option<String>, RpcError)> {
    let text = std::str::from_utf8(bytes).map_err(|_| (None, RpcError::invalid_input()))?;
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let strict = StrictValue::deserialize(&mut deserializer)
        .map_err(|_| (None, RpcError::invalid_input()))?;
    deserializer
        .end()
        .map_err(|_| (None, RpcError::invalid_input()))?;
    let value = strict.into_value();
    let object = value
        .as_object()
        .ok_or_else(|| (None, RpcError::invalid_input()))?;
    let request_id = object
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);

    const FIELDS: [&str; 4] = ["operation", "params", "protocol_version", "request_id"];
    if object.len() != FIELDS.len() || object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
        return Err((request_id, RpcError::invalid_input()));
    }

    let version = object.get("protocol_version").and_then(Value::as_u64);
    if version != Some(PROTOCOL_VERSION) {
        if let Some(requested_version) = version {
            return Err((
                request_id,
                RpcError::new(
                    ErrorCode::UnsupportedVersion,
                    "The requested protocol version is not supported",
                    Map::from_iter([
                        ("requested_version".into(), json!(requested_version)),
                        ("supported_versions".into(), json!([PROTOCOL_VERSION])),
                    ]),
                ),
            ));
        }
        return Err((request_id, RpcError::invalid_input()));
    }

    let request_id = request_id.ok_or_else(|| (None, RpcError::invalid_input()))?;
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .and_then(Operation::parse)
        .ok_or_else(|| (Some(request_id.clone()), RpcError::invalid_input()))?;
    let params = object
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| (Some(request_id.clone()), RpcError::invalid_input()))?;
    Ok(Request {
        request_id,
        operation,
        params,
    })
}

/// A JSON value deserializer that rejects duplicate keys at every depth.
struct StrictValue(Value);

impl StrictValue {
    fn into_value(self) -> Value {
        self.0
    }
}

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.into_value());
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        let mut values = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate object key"));
            }
            values.insert(key, map.next_value::<StrictValue>()?.into_value());
        }
        Ok(StrictValue(Value::Object(values.into_iter().collect())))
    }
}
