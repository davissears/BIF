//! One-shot CLI entry point for BIF RPC v1.

use std::{
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureIdentity, CaptureRequest, CaptureResult,
        CaptureStore, CaptureStoreError, Clock, Command, Execution, HumanAuthorization,
        IdentityGenerator, MutationIdentity, MutationIdentityGenerator, MutationRequest,
        MutationResult, MutationStore, MutationStoreError, ObservedExecution,
    },
    config::{self, ConfigOverrides},
    domain::Timestamp,
    rpc::{Dispatcher, ErrorCode, Operation, Request, RpcError},
    rpc_mutation::{MutationAuthorization, MutationDispatcher},
    rpc_read::{ReadAuthorization, ReadDispatcher},
    storage::{
        self, CaptureRepository, CaptureStorageError, ItemHistoryRepository, ItemRepository,
        MutationRepository,
    },
};

/// Runs exactly one RPC exchange, returning its stable process exit code.
pub fn run<I, S>(
    arguments: I,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let overrides = match parse(arguments) {
        Ok(value) => value,
        Err(message) => {
            // Argument errors happen outside the RPC exchange and must not put
            // non-protocol text on stdout.
            let _ = writeln!(stderr, "error: {message}");
            return 2;
        }
    };
    let mut dispatcher = ProcessDispatcher { overrides };
    match crate::rpc::serve(stdin, stdout, &mut *stderr, &mut dispatcher) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(stderr, "BIF RPC transport error: {error}");
            1
        }
    }
}

fn parse<I, S>(arguments: I) -> Result<ConfigOverrides, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let values = arguments
        .into_iter()
        .map(|value| {
            value
                .into()
                .into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut overrides = ConfigOverrides::default();
    let mut index = 0;
    while index < values.len() {
        let name = values[index]
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected RPC argument {:?}", values[index]))?;
        let value = values
            .get(index + 1)
            .ok_or_else(|| format!("--{name} requires a value"))?;
        match name {
            "config" => overrides.config = Some(PathBuf::from(value)),
            "root" => overrides.root = Some(PathBuf::from(value)),
            "requester" => overrides.requester = Some(value.clone()),
            _ => return Err(format!("unknown RPC option --{name}")),
        }
        index += 2;
    }
    Ok(overrides)
}

struct ProcessDispatcher {
    overrides: ConfigOverrides,
}

impl Dispatcher for ProcessDispatcher {
    fn dispatch(&mut self, request: Request) -> Result<Value, RpcError> {
        let config = config::load(self.overrides.clone()).map_err(config_error)?;
        let paths = config.store_paths().map_err(config_error)?;
        if !paths.database.is_file() {
            return Err(not_initialized());
        }
        let connection = storage::open(&paths.database).map_err(storage_error)?;
        let actor_id = config.requester.to_string();
        let actor = Actor {
            kind: ActorKind::Human,
            id: &actor_id,
            surface: "rpc",
            host: "local",
        };
        let execution = Execution::Direct {
            surface: "rpc",
            host: "local",
        };

        match request.operation {
            Operation::Get | Operation::List | Operation::Next | Operation::History => {
                let items = ItemRepository::new(&connection);
                let history = ItemHistoryRepository::new(&connection);
                ReadDispatcher {
                    store: &items,
                    history_store: &history,
                    authorization: ReadAuthorization {
                        request: AuthorizationRequest {
                            actor,
                            execution,
                            observed_execution: ObservedExecution::Direct,
                            command: Command::Read,
                            human_authorization: None,
                        },
                        configured_requester: &config.requester,
                    },
                }
                .dispatch(request)
            }
            _ => {
                let mut store = WriteStore { connection };
                MutationDispatcher {
                    store: &mut store,
                    clock: &mut SystemClock,
                    identities: &mut SystemIdentities,
                    authorization: MutationAuthorization {
                        actor,
                        execution,
                        observed_execution: ObservedExecution::Direct,
                        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
                    },
                }
                .dispatch(request)
            }
        }
    }
}

fn config_error(error: config::ConfigError) -> RpcError {
    if matches!(error, config::ConfigError::NotInitialized) {
        not_initialized()
    } else {
        RpcError::new(
            ErrorCode::Internal,
            "BIF configuration could not be resolved",
            Map::new(),
        )
    }
}

fn not_initialized() -> RpcError {
    RpcError::new(
        ErrorCode::NotInitialized,
        "BIF is not initialized",
        Map::new(),
    )
}

fn storage_error(error: storage::MigrationError) -> RpcError {
    let busy = matches!(
        &error,
        storage::MigrationError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    );
    RpcError::new(
        if busy {
            ErrorCode::StorageBusy
        } else {
            ErrorCode::Internal
        },
        if busy {
            "BIF storage is busy"
        } else {
            "BIF storage could not be opened"
        },
        Map::new(),
    )
}

struct WriteStore {
    connection: Connection,
}

impl CaptureStore for WriteStore {
    type Error = CaptureStorageError;

    fn capture(
        &mut self,
        request: CaptureRequest,
        payload_hash: String,
        actor: Actor<'_>,
        execution: Execution<'_>,
        occurred_at: Timestamp,
        identity: CaptureIdentity,
    ) -> Result<CaptureResult, CaptureStoreError<Self::Error>> {
        CaptureRepository::new(&mut self.connection).capture(
            request,
            payload_hash,
            actor,
            execution,
            occurred_at,
            identity,
        )
    }
}

impl MutationStore for WriteStore {
    type Error = rusqlite::Error;

    fn mutate(
        &mut self,
        request: MutationRequest,
        payload_hash: String,
        actor: Actor<'_>,
        execution: Execution<'_>,
        occurred_at: Timestamp,
        identity: MutationIdentity,
    ) -> Result<MutationResult, MutationStoreError<Self::Error>> {
        MutationRepository::new(&mut self.connection).mutate(
            request,
            payload_hash,
            actor,
            execution,
            occurred_at,
            identity,
        )
    }
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new(rfc3339_now())
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct SystemIdentities;

impl SystemIdentities {
    fn prefix() -> String {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let serial = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        format!("rpc-{nonce:032x}-{serial:016x}")
    }
}

impl IdentityGenerator for SystemIdentities {
    fn capture_identity(&mut self) -> CaptureIdentity {
        let prefix = Self::prefix();
        CaptureIdentity {
            operation_id: prefix.clone(),
            event_id: format!("{prefix}-event-0"),
        }
    }
}

impl MutationIdentityGenerator for SystemIdentities {
    fn mutation_identity(&mut self) -> MutationIdentity {
        let prefix = Self::prefix();
        MutationIdentity {
            operation_id: prefix.clone(),
            event_ids: (0..8)
                .map(|index| format!("{prefix}-event-{index}"))
                .collect(),
        }
    }
}

fn rfc3339_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3_600,
        day_seconds % 3_600 / 60,
        day_seconds % 60
    )
}
