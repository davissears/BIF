//! Minimal MCP 2025-11-25 JSON-RPC stdio adapter, with no mutation capability.
//!
//! One worker owns the pinned read session. A bounded reader/control path remains
//! responsive during SQLite work; one writer never holds application transactions.
//! Slow output is bounded: exhausting the outgoing queue disconnects the adapter
//! rather than blocking cancellation or accumulating unbounded response memory.

mod tools;
pub use tools::{InvalidToolArguments, decode_tool, tool_catalog};

use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    application::{Actor, ActorKind, AuthorizationRequest, Command, Execution, ObservedExecution},
    config::Config,
    read_session::{ReadRequest, ReadSession},
    v2_response::{self, ReadError, ResponseBudget},
};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
/// Complete output frame, including its one newline.
pub const MAXIMUM_WIRE_BYTES: usize = crate::limits::MAXIMUM_RESPONSE_BYTES;
/// Complete incoming frame, including its newline if present.
pub const MAXIMUM_INPUT_BYTES: usize = crate::limits::MAXIMUM_REQUEST_BYTES;
/// Structured content plus JSON-escaped compatibility text costs at most 7 times
/// the inner JSON bytes. One eighth of the wire limit leaves framing/ID headroom.
pub const TOOL_RESPONSE_BUDGET_BYTES: usize = MAXIMUM_WIRE_BYTES / 8;
const MAXIMUM_ID_BYTES: usize = 1_024;
const QUEUE_CAPACITY: usize = 8;
const MAXIMUM_OUTSTANDING: usize = 32;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Id {
    String(String),
    Integer(serde_json::Number),
}

impl Id {
    fn parse(value: &Value) -> Option<Self> {
        if serde_json::to_vec(value).ok()?.len() > MAXIMUM_ID_BYTES {
            return None;
        }
        match value {
            Value::String(value) => Some(Self::String(value.clone())),
            Value::Number(value) if value.is_i64() || value.is_u64() => {
                Some(Self::Integer(value.clone()))
            }
            _ => None,
        }
    }

    fn value(&self) -> Value {
        match self {
            Self::String(value) => json!(value),
            Self::Integer(value) => json!(value),
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Immediate,
    Queued,
    Active,
    Finished,
}

struct Outstanding {
    stage: Stage,
    cancelled: bool,
}

/// All transitions and interrupts share this lock. An interrupt cannot race a
/// completed call's retirement and accidentally target the next SQLite call.
struct Control {
    calls: HashMap<Id, Outstanding>,
    active: Option<Id>,
    stopped: bool,
    interrupt: rusqlite::InterruptHandle,
    wake_interrupt: SyncSender<()>,
}

type Shared = Arc<Mutex<Control>>;

fn stop(shared: &Shared) {
    let mut state = shared.lock().unwrap();
    state.stopped = true;
    if state.active.is_some() {
        state.interrupt.interrupt();
    }
    let _ = state.wake_interrupt.try_send(());
}

struct Job {
    id: Id,
    request: ReadRequest,
}

struct Output {
    bytes: Vec<u8>,
    /// Some only when retiring the registered request. Duplicate-ID errors must
    /// not retire or replace the original outstanding request.
    retire: Option<Id>,
}

/// Exact frame encoding before any stdout write; no partial success on overflow.
fn frame(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).expect("JSON value serialization");
    if bytes.len() >= MAXIMUM_WIRE_BYTES {
        let id = value.get("id").and_then(Id::parse);
        bytes = serde_json::to_vec(&rpc_error(
            id.as_ref(),
            -32603,
            "Response exceeds the wire budget",
        ))
        .unwrap();
    }
    bytes.push(b'\n');
    bytes
}

/// MCP error IDs are optional string/integer IDs; uncorrelated errors omit them.
fn rpc_error(id: Option<&Id>, code: i32, message: &str) -> Value {
    let mut response = json!({"jsonrpc":"2.0","error":{"code":code,"message":message}});
    if let Some(id) = id {
        response["id"] = id.value();
    }
    response
}

fn rpc_result(id: &Id, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id.value(),"result":result})
}

fn emit(output: &SyncSender<Output>, shared: &Shared, value: Value, retire: Option<Id>) {
    if output
        .try_send(Output {
            bytes: frame(value),
            retire,
        })
        .is_err()
    {
        stop(shared);
    }
}

fn error(output: &SyncSender<Output>, shared: &Shared, id: &Id, code: i32, message: &str) {
    emit(
        output,
        shared,
        rpc_error(Some(id), code, message),
        Some(id.clone()),
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Initialize {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: Map<String, Value>,
    #[serde(rename = "clientInfo")]
    client_info: ClientInfo,
    #[serde(rename = "_meta")]
    _meta: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
struct ClientInfo {
    name: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCall {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
    #[serde(rename = "_meta")]
    _meta: Option<Map<String, Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolList {
    cursor: Option<String>,
    #[serde(rename = "_meta")]
    _meta: Option<Map<String, Value>>,
}

fn empty_object() -> Value {
    json!({})
}

enum Lifecycle {
    New,
    AwaitingInitialized,
    Ready,
}

struct Envelope {
    id: Option<Id>,
    method: String,
    params: Option<Value>,
}

fn envelope(value: Value) -> Result<Envelope, Option<Id>> {
    let Some(object) = value.as_object() else {
        return Err(None);
    };
    let id = match object.get("id") {
        Some(value) => Some(Id::parse(value).ok_or(None)?),
        None => None,
    };
    if object.get("jsonrpc") != Some(&json!("2.0"))
        || object
            .keys()
            .any(|key| !["jsonrpc", "id", "method", "params"].contains(&key.as_str()))
    {
        return Err(id);
    }
    let Some(Value::String(method)) = object.get("method") else {
        return Err(id);
    };
    Ok(Envelope {
        id,
        method: method.clone(),
        params: object.get("params").cloned(),
    })
}

fn empty_params(params: Option<&Value>) -> bool {
    params.is_none_or(|value| {
        value.as_object().is_some_and(|map| {
            map.is_empty() || (map.len() == 1 && map.get("_meta").is_some_and(Value::is_object))
        })
    })
}

/// Bounded newline scanning also drains an oversized line without retaining it,
/// so the next frame can be parsed. EOF's final unterminated frame is accepted.
fn read_frame(input: &mut impl BufRead) -> io::Result<Option<Result<Vec<u8>, ()>>> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    let mut seen = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(seen.then_some(if oversized { Err(()) } else { Ok(bytes) }));
        }
        seen = true;
        let end = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1);
        let count = end.unwrap_or(chunk.len());
        if !oversized {
            if bytes.len() + count > MAXIMUM_INPUT_BYTES {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&chunk[..count]);
            }
        }
        input.consume(count);
        if end.is_some() {
            return Ok(Some(if oversized { Err(()) } else { Ok(bytes) }));
        }
    }
}

fn cancel(shared: &Shared, params: Option<Value>) {
    let Some(params) = params.and_then(|value| value.as_object().cloned()) else {
        return;
    };
    if params
        .keys()
        .any(|key| !["requestId", "reason", "_meta"].contains(&key.as_str()))
        || params.get("reason").is_some_and(|value| !value.is_string())
        || params.get("_meta").is_some_and(|value| !value.is_object())
    {
        return;
    }
    let Some(id) = params.get("requestId").and_then(Id::parse) else {
        return;
    };
    let mut state = shared.lock().unwrap();
    let Some(call) = state.calls.get_mut(&id) else {
        return;
    };
    if matches!(call.stage, Stage::Queued | Stage::Active) {
        call.cancelled = true;
        if state.active.as_ref() == Some(&id) {
            state.interrupt.interrupt();
            let _ = state.wake_interrupt.try_send(());
        }
    }
}

fn reader(shared: Shared, jobs: SyncSender<Job>, output: SyncSender<Output>) {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut lifecycle = Lifecycle::New;
    loop {
        if shared.lock().unwrap().stopped {
            break;
        }
        let bytes = match read_frame(&mut input) {
            Ok(Some(Ok(bytes))) => bytes,
            Ok(Some(Err(()))) => {
                emit(
                    &output,
                    &shared,
                    rpc_error(None, -32600, "Input exceeds the frame budget"),
                    None,
                );
                continue;
            }
            Ok(None) => break,
            Err(error) => {
                eprintln!("bif-mcp: stdin: {error}");
                break;
            }
        };
        let message = match crate::strict_json::parse(&bytes) {
            Ok(value) => match envelope(value) {
                Ok(message) => message,
                Err(id) => {
                    emit(
                        &output,
                        &shared,
                        rpc_error(id.as_ref(), -32600, "Invalid request"),
                        None,
                    );
                    continue;
                }
            },
            Err(error) => {
                let (code, message) = if error.is_data() {
                    (-32600, "Invalid request")
                } else {
                    (-32700, "Parse error")
                };
                emit(&output, &shared, rpc_error(None, code, message), None);
                continue;
            }
        };
        let Some(id) = message.id else {
            match message.method.as_str() {
                "notifications/initialized"
                    if matches!(lifecycle, Lifecycle::AwaitingInitialized)
                        && empty_params(message.params.as_ref()) =>
                {
                    lifecycle = Lifecycle::Ready
                }
                "notifications/cancelled" => cancel(&shared, message.params),
                _ => {}
            }
            continue;
        };
        {
            let mut state = shared.lock().unwrap();
            if state.calls.contains_key(&id) {
                drop(state);
                emit(
                    &output,
                    &shared,
                    rpc_error(Some(&id), -32600, "Duplicate outstanding request ID"),
                    None,
                );
                continue;
            }
            if state.calls.len() >= MAXIMUM_OUTSTANDING {
                drop(state);
                emit(
                    &output,
                    &shared,
                    rpc_error(Some(&id), -32000, "Too many outstanding requests"),
                    None,
                );
                continue;
            }
            state.calls.insert(
                id.clone(),
                Outstanding {
                    stage: Stage::Immediate,
                    cancelled: false,
                },
            );
        }
        match message.method.as_str() {
            "initialize" if matches!(lifecycle, Lifecycle::New) => {
                match serde_json::from_value::<Initialize>(message.params.unwrap_or(Value::Null)) {
                    Ok(params)
                        if !params.protocol_version.is_empty()
                            && !params.client_info.name.is_empty()
                            && !params.client_info.version.is_empty() =>
                    {
                        // The server supports one frozen version. Per MCP negotiation,
                        // return this version for other requested versions as well.
                        let _ = params.capabilities;
                        lifecycle = Lifecycle::AwaitingInitialized;
                        emit(
                            &output,
                            &shared,
                            rpc_result(
                                &id,
                                json!({
                                    "protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},
                                    "serverInfo":{"name":"bif-mcp","version":env!("CARGO_PKG_VERSION")}
                                }),
                            ),
                            Some(id),
                        );
                    }
                    _ => error(
                        &output,
                        &shared,
                        &id,
                        -32602,
                        "Invalid initialize parameters",
                    ),
                }
            }
            "initialize" => error(&output, &shared, &id, -32600, "Already initialized"),
            "ping" if empty_params(message.params.as_ref()) => {
                emit(&output, &shared, rpc_result(&id, json!({})), Some(id))
            }
            "ping" => error(&output, &shared, &id, -32602, "Invalid ping parameters"),
            "tools/list" | "tools/call" if !matches!(lifecycle, Lifecycle::Ready) => error(
                &output,
                &shared,
                &id,
                -32600,
                "Initialization is not complete",
            ),
            "tools/list" => {
                match serde_json::from_value::<ToolList>(
                    message.params.unwrap_or_else(empty_object),
                ) {
                    Ok(params) if params.cursor.is_none() => emit(
                        &output,
                        &shared,
                        rpc_result(&id, json!({"tools":tool_catalog()})),
                        Some(id),
                    ),
                    _ => error(
                        &output,
                        &shared,
                        &id,
                        -32602,
                        "Invalid tools/list parameters",
                    ),
                }
            }
            "tools/call" => {
                let request =
                    serde_json::from_value::<ToolCall>(message.params.unwrap_or(Value::Null))
                        .ok()
                        .and_then(|params| decode_tool(&params.name, params.arguments).ok());
                if let Some(request) = request {
                    shared.lock().unwrap().calls.get_mut(&id).unwrap().stage = Stage::Queued;
                    if jobs
                        .try_send(Job {
                            id: id.clone(),
                            request,
                        })
                        .is_err()
                    {
                        shared.lock().unwrap().calls.get_mut(&id).unwrap().stage = Stage::Finished;
                        error(&output, &shared, &id, -32000, "Read queue is full");
                    }
                } else {
                    error(
                        &output,
                        &shared,
                        &id,
                        -32602,
                        "Unknown tool or invalid tool arguments",
                    );
                }
            }
            _ => error(&output, &shared, &id, -32601, "Method not found"),
        }
    }
    stop(&shared);
}

fn tool_result(bytes: Vec<u8>, is_error: bool) -> Result<Value, ReadError> {
    let text = String::from_utf8(bytes).map_err(|_| ReadError::Internal)?;
    let structured: Value = serde_json::from_str(&text).map_err(|_| ReadError::Internal)?;
    Ok(json!({"content":[{"type":"text","text":text}],
        "structuredContent":structured,"isError":is_error}))
}

/// Run a configured adapter until EOF/output disconnect. Stdin is on a detached
/// control thread so a closed stdout also ends the process while stdin is open.
pub fn run_stdio(config: Config) -> Result<(), ReadError> {
    let mut session = ReadSession::open(config)?;
    let (wake_interrupt, cancellations) = mpsc::sync_channel(1);
    let shared = Arc::new(Mutex::new(Control {
        calls: HashMap::new(),
        active: None,
        stopped: false,
        interrupt: session.interrupt_handle(),
        wake_interrupt,
    }));
    let (job_send, jobs) = mpsc::sync_channel::<Job>(QUEUE_CAPACITY);
    let (output, replies) = mpsc::sync_channel::<Output>(QUEUE_CAPACITY);
    let writer_state = Arc::clone(&shared);
    thread::spawn(move || {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        while let Ok(reply) = replies.recv() {
            let state = writer_state.lock().unwrap();
            if state.stopped {
                break;
            }
            let cancelled = reply
                .retire
                .as_ref()
                .and_then(|id| state.calls.get(id))
                .is_some_and(|call| call.cancelled);
            drop(state);
            if !cancelled && (stdout.write_all(&reply.bytes).is_err() || stdout.flush().is_err()) {
                stop(&writer_state);
                break;
            }
            if let Some(id) = reply.retire {
                writer_state.lock().unwrap().calls.remove(&id);
            }
        }
        stop(&writer_state);
    });
    // A cancellation can land just before SQLite starts its first statement.
    // Repeat interrupts for the same canceled active call, under the transition
    // lock; never retry an interrupt once a subsequent call becomes active.
    // The helper sleeps on a bounded wake channel during normal/idle operation.
    let interrupt_state = Arc::clone(&shared);
    thread::spawn(move || {
        while cancellations.recv().is_ok() {
            loop {
                {
                    let state = interrupt_state.lock().unwrap();
                    if state.stopped && state.active.is_none() {
                        return;
                    }
                    let needs_interrupt = state.active.is_some()
                        && (state.stopped
                            || state
                                .active
                                .as_ref()
                                .and_then(|id| state.calls.get(id))
                                .is_some_and(|call| call.cancelled));
                    if !needs_interrupt {
                        break;
                    }
                    state.interrupt.interrupt();
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
    });
    let reader_state = Arc::clone(&shared);
    let reader_output = output.clone();
    thread::spawn(move || reader(reader_state, job_send, reader_output));
    let budget = ResponseBudget::new(TOOL_RESPONSE_BUDGET_BYTES)?;
    let requester = session.requester().clone();
    let authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: requester.as_str(),
            surface: "mcp",
            host: "local",
        },
        execution: Execution::Agent {
            agent_id: "bif-mcp",
            surface: "mcp",
            host: "local",
        },
        observed_execution: ObservedExecution::Agent {
            agent_id: "bif-mcp",
        },
        command: Command::Read,
        human_authorization: None,
    };
    loop {
        if shared.lock().unwrap().stopped {
            break;
        }
        let job = match jobs.recv_timeout(Duration::from_millis(20)) {
            Ok(job) => job,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        {
            let mut state = shared.lock().unwrap();
            if state.stopped {
                break;
            }
            let call = state.calls.get_mut(&job.id).unwrap();
            if call.cancelled {
                state.calls.remove(&job.id);
                continue;
            }
            call.stage = Stage::Active;
            state.active = Some(job.id.clone());
        }
        let result = session.execute(&authorization, job.request, budget);
        let cancelled = {
            let mut state = shared.lock().unwrap();
            state.active = None;
            let call = state.calls.get_mut(&job.id).unwrap();
            call.stage = Stage::Finished;
            if call.cancelled || state.stopped {
                state.calls.remove(&job.id);
                true
            } else {
                false
            }
        };
        if cancelled {
            continue;
        }
        let result = match result {
            Ok(bytes) => tool_result(bytes, false),
            Err(error) => {
                let mut bytes = Vec::new();
                v2_response::write_error(&mut bytes, &error, budget)
                    .map_err(|_| ReadError::Internal)
                    .and_then(|_| tool_result(bytes, true))
            }
        };
        match result {
            Ok(result) => emit(&output, &shared, rpc_result(&job.id, result), Some(job.id)),
            Err(_) => error(
                &output,
                &shared,
                &job.id,
                -32603,
                "Could not encode tool response",
            ),
        }
    }
    stop(&shared);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_budget_keeps_both_tool_representations_inside_wire_limit() {
        let id = Id::String("\\".repeat(500));
        assert!(serde_json::to_vec(&id.value()).unwrap().len() <= MAXIMUM_ID_BYTES);
        for text in ["x", "\\", "\"", "\u{0001}", "\n", "é"] {
            let payload = text.repeat((TOOL_RESPONSE_BUDGET_BYTES - 1_024) / (text.len() * 6));
            let bytes = serde_json::to_vec(&json!({
                "api_version":2,"schema_version":1,"ok":true,"result":{"item":{"title":payload}}
            }))
            .unwrap();
            assert!(bytes.len() <= TOOL_RESPONSE_BUDGET_BYTES);
            let result = tool_result(bytes, false).unwrap();
            let encoded = frame(rpc_result(&id, result));
            assert!(encoded.len() <= MAXIMUM_WIRE_BYTES);
            let response: Value = serde_json::from_slice(&encoded).unwrap();
            assert!(response.get("result").is_some());
            let compatibility: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(compatibility, response["result"]["structuredContent"]);
        }
    }

    #[test]
    fn exact_wire_overflow_replaces_success_before_any_output() {
        for id in [Some(json!(1)), Some(json!("known")), None] {
            let mut value = json!({"jsonrpc":"2.0","result":{
                "large":"x".repeat(MAXIMUM_WIRE_BYTES)}});
            if let Some(id) = &id {
                value["id"] = id.clone();
            }
            let encoded = frame(value);
            assert!(encoded.len() <= MAXIMUM_WIRE_BYTES);
            assert_eq!(encoded.last(), Some(&b'\n'));
            let response: Value = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(response.get("id"), id.as_ref());
            assert_eq!(response["error"]["code"], -32603);
            assert!(response.get("result").is_none());
        }
    }

    #[test]
    fn newline_budget_is_exact_and_eof_does_not_require_a_newline() {
        let maximum = vec![b'x'; MAXIMUM_INPUT_BYTES - 1];
        let mut bytes = maximum.clone();
        bytes.push(b'\n');
        assert!(read_frame(&mut bytes.as_slice()).unwrap().unwrap().is_ok());
        bytes.insert(0, b'x');
        bytes.extend_from_slice(b"{}\n");
        let mut input = bytes.as_slice();
        assert!(read_frame(&mut input).unwrap().unwrap().is_err());
        assert_eq!(read_frame(&mut input).unwrap().unwrap().unwrap(), b"{}\n");
        assert_eq!(
            read_frame(&mut b"{}".as_slice()).unwrap().unwrap().unwrap(),
            b"{}"
        );
        assert!(read_frame(&mut b"".as_slice()).unwrap().is_none());
    }

    #[test]
    fn full_output_queue_disconnects_without_blocking_control_handling() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let (wake_interrupt, _) = mpsc::sync_channel(1);
        let shared = Arc::new(Mutex::new(Control {
            calls: HashMap::new(),
            active: None,
            stopped: false,
            interrupt: connection.get_interrupt_handle(),
            wake_interrupt,
        }));
        let (output, _receiver) = mpsc::sync_channel(1);
        emit(&output, &shared, json!({}), None);
        assert!(!shared.lock().unwrap().stopped);
        emit(&output, &shared, json!({}), None);
        assert!(shared.lock().unwrap().stopped);
    }
}
