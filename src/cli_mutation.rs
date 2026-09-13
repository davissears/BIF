//! Parsing and execution for lifecycle and triage CLI mutations.
//!
//! Kept separate from the read-oriented CLI adapter so the two command
//! families can evolve independently.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    application::{
        self, Actor, ActorKind, AuthorizationRequest, Clock, Command, Execution,
        HumanAuthorization, ItemStore, MutationIdentity, MutationIdentityGenerator,
        MutationRequest, ObservedExecution,
    },
    config::{self, ConfigOverrides},
    domain::{
        AssigneeId, ItemId, ItemMutation, LifecycleMutation, Priority, ProjectId, RequesterId,
        Revision, Status, Timestamp, Triage, TriageField,
    },
    storage::{self, ItemRepository, MutationRepository},
};

use super::cli::CliError;

#[derive(Debug)]
pub(crate) struct MutationOptions {
    item_id: ItemId,
    expected_revision: Revision,
    idempotency_key: String,
    mutation: ItemMutation,
    requested_changes: Vec<String>,
    config: ConfigOverrides,
}

pub(crate) fn is_command(value: &str) -> bool {
    matches!(
        value,
        "triage"
            | "approve"
            | "reject"
            | "prioritize"
            | "assign"
            | "start"
            | "block"
            | "resume"
            | "finish"
    )
}

pub(crate) fn parse(command: &str, values: &[String]) -> Result<MutationOptions, CliError> {
    let item = values
        .first()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| CliError::Usage(format!("{command} requires ITEM_ID")))?;
    let mut positional_end = 1;
    let positional_value = match command {
        "prioritize" | "assign" | "reject" | "block" => values
            .get(1)
            .filter(|value| !value.starts_with("--"))
            .map(|value| {
                positional_end = 2;
                value.as_str()
            }),
        _ => None,
    };
    let options = Options::parse(&values[positional_end..])?;
    let expected_revision = options
        .required("expected-revision")?
        .parse::<u64>()
        .map_err(|_| CliError::Usage("--expected-revision must be an integer".into()))
        .and_then(|value| Revision::new(value).map_err(CliError::from))?;
    let idempotency_key = nonempty(options.required("idempotency-key")?, "idempotency-key")?;

    let priority_value = exclusive(
        positional_value,
        options.get("priority"),
        command == "prioritize",
    )?;
    let assignee_value = exclusive(
        positional_value,
        options.get("assignee"),
        command == "assign",
    )?;
    let reason_value = exclusive(
        positional_value,
        options.get("reason"),
        matches!(command, "reject" | "block"),
    )?;

    let lifecycle = match command {
        "approve" => Some(LifecycleMutation::Approve),
        "reject" => Some(LifecycleMutation::Reject {
            reason: nonempty(
                required_value(reason_value, "reject requires REASON or --reason")?,
                "reason",
            )?,
        }),
        "start" => Some(LifecycleMutation::Start),
        "block" => Some(LifecycleMutation::Block {
            reason: nonempty(
                required_value(reason_value, "block requires REASON or --reason")?,
                "reason",
            )?,
        }),
        "resume" => Some(LifecycleMutation::Resume),
        "finish" => Some(LifecycleMutation::Finish),
        "triage" => match options.get("action") {
            None => None,
            Some("approve") => Some(LifecycleMutation::Approve),
            Some("reject") => Some(LifecycleMutation::Reject {
                reason: nonempty(options.required("reason")?, "reason")?,
            }),
            Some("start") => Some(LifecycleMutation::Start),
            Some("block") => Some(LifecycleMutation::Block {
                reason: nonempty(options.required("reason")?, "reason")?,
            }),
            Some("resume") => Some(LifecycleMutation::Resume),
            Some("finish") => Some(LifecycleMutation::Finish),
            Some(value) => return Err(CliError::Usage(format!("invalid --action {value:?}"))),
        },
        _ => None,
    };
    let priority = parse_priority(priority_value)?;
    let assignee = parse_assignee(assignee_value)?;
    let note = options.get("note").map(str::to_owned);
    let has_triage = !matches!(priority, TriageField::Omitted)
        || !matches!(assignee, TriageField::Omitted)
        || note.is_some();
    if command == "triage" && lifecycle.is_none() && !has_triage {
        return Err(CliError::Usage(
            "triage requires at least one change".into(),
        ));
    }
    if !matches!(command, "triage" | "prioritize" | "assign")
        && (has_triage || options.get("action").is_some())
    {
        return Err(CliError::Usage(format!(
            "{command} does not accept triage fields"
        )));
    }
    if options.get("reason").is_some() && !matches!(command, "triage" | "reject" | "block") {
        return Err(CliError::Usage(format!(
            "{command} does not accept --reason"
        )));
    }

    let mut requested_changes = Vec::new();
    if let Some(action) = lifecycle_name(lifecycle.as_ref()) {
        requested_changes.push(action.into());
    }
    match &priority {
        TriageField::Set(value) => {
            requested_changes.push(format!("prioritize:{}", priority_name(*value)))
        }
        TriageField::Clear => requested_changes.push("prioritize:clear".into()),
        TriageField::Omitted => {}
    }
    match &assignee {
        TriageField::Set(value) => requested_changes.push(format!("assign:{}", value.as_str())),
        TriageField::Clear => requested_changes.push("assign:clear".into()),
        TriageField::Omitted => {}
    }
    if note.is_some() {
        requested_changes.push("note".into());
    }

    Ok(MutationOptions {
        item_id: parse_item_id(item)?,
        expected_revision,
        idempotency_key,
        mutation: ItemMutation {
            lifecycle,
            triage: has_triage.then_some(Triage {
                priority,
                assignee,
                note,
            }),
        },
        requested_changes,
        config: ConfigOverrides {
            config: options.get("config").map(PathBuf::from),
            root: options.get("root").map(PathBuf::from),
            requester: options.get("requester").map(str::to_owned),
        },
    })
}

pub(crate) fn execute(options: MutationOptions) -> Result<String, CliError> {
    let config = config::load(options.config)?;
    let paths = config.store_paths()?;
    let mut connection = storage::open(&paths.database)?;
    let event_count = ItemRepository::new(&connection)
        .read_item(&options.item_id)?
        .map(|mut item| {
            if item.revision() == options.expected_revision {
                item.apply_mutation(options.mutation.clone())
                    .map(|events| events.len())
            } else {
                Ok(0)
            }
        })
        .transpose()?
        .unwrap_or(0);
    let actor_id = config.requester.to_string();
    let changes = options
        .requested_changes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: &actor_id,
            surface: "cli",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "cli",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: Command::Mutation {
            requested_changes: &changes,
        },
        human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
    };
    let result = application::mutate_item_idempotent(
        &mut MutationRepository::new(&mut connection),
        &mut SystemClock,
        &mut SystemIdentities { event_count },
        &authorization,
        MutationRequest {
            idempotency_key: options.idempotency_key,
            item_id: options.item_id,
            expected_revision: options.expected_revision,
            mutation: options.mutation,
        },
    )?;
    let item = result.item;
    Ok(format!(
        "Updated {}\nstatus: {}\npriority: {}\nassignee: {}\nrevision: {}\nreplayed: {}\n",
        item.id(),
        status_name(item.status()),
        item.priority()
            .map(priority_name)
            .unwrap_or("unprioritized"),
        item.assignee()
            .map(AssigneeId::as_str)
            .unwrap_or("unassigned"),
        item.revision().get(),
        result.replayed
    ))
}

struct Options<'a>(Vec<(&'a str, &'a str)>);

impl<'a> Options<'a> {
    fn parse(values: &'a [String]) -> Result<Self, CliError> {
        const ALLOWED: &[&str] = &[
            "expected-revision",
            "idempotency-key",
            "action",
            "reason",
            "priority",
            "assignee",
            "note",
            "config",
            "root",
            "requester",
        ];
        let mut parsed = Vec::new();
        let mut index = 0;
        while index < values.len() {
            let name = values[index].strip_prefix("--").ok_or_else(|| {
                CliError::Usage(format!("unexpected argument {:?}", values[index]))
            })?;
            if !ALLOWED.contains(&name) {
                return Err(CliError::Usage(format!("unknown option --{name}")));
            }
            if parsed.iter().any(|(candidate, _)| *candidate == name) {
                return Err(CliError::Usage(format!("duplicate option --{name}")));
            }
            let value = values
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| CliError::Usage(format!("--{name} requires a value")))?;
            parsed.push((name, value.as_str()));
            index += 2;
        }
        Ok(Self(parsed))
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.0
            .iter()
            .find_map(|(candidate, value)| (*candidate == name).then_some(*value))
    }

    fn required(&self, name: &str) -> Result<&'a str, CliError> {
        self.get(name)
            .ok_or_else(|| CliError::Usage(format!("missing required option --{name}")))
    }
}

fn exclusive<'a>(
    positional: Option<&'a str>,
    option: Option<&'a str>,
    applies: bool,
) -> Result<Option<&'a str>, CliError> {
    if !applies {
        return Ok(option);
    }
    match (positional, option) {
        (Some(_), Some(_)) => Err(CliError::Usage(
            "value supplied both positionally and as an option".into(),
        )),
        (value, None) | (None, value) => Ok(value),
    }
}

fn required_value<'a>(value: Option<&'a str>, message: &str) -> Result<&'a str, CliError> {
    value.ok_or_else(|| CliError::Usage(message.into()))
}

fn nonempty(value: &str, name: &str) -> Result<String, CliError> {
    if value.trim().is_empty() {
        Err(CliError::Usage(format!("--{name} must not be empty")))
    } else {
        Ok(value.to_owned())
    }
}

fn parse_priority(value: Option<&str>) -> Result<TriageField<Priority>, CliError> {
    Ok(match value {
        None => TriageField::Omitted,
        Some("clear" | "none") => TriageField::Clear,
        Some("P0" | "p0") => TriageField::Set(Priority::P0),
        Some("P1" | "p1") => TriageField::Set(Priority::P1),
        Some("P2" | "p2") => TriageField::Set(Priority::P2),
        Some("P3" | "p3") => TriageField::Set(Priority::P3),
        Some("P4" | "p4") => TriageField::Set(Priority::P4),
        Some(value) => return Err(CliError::Usage(format!("invalid priority {value:?}"))),
    })
}

fn parse_assignee(value: Option<&str>) -> Result<TriageField<AssigneeId>, CliError> {
    match value {
        None => Ok(TriageField::Omitted),
        Some("clear" | "none") => Ok(TriageField::Clear),
        Some(value) => Ok(TriageField::Set(AssigneeId::new(value)?)),
    }
}

fn parse_item_id(value: &str) -> Result<ItemId, CliError> {
    let mut parts = value.split(':');
    let item = ItemId::new(
        RequesterId::new(parts.next().unwrap_or_default())?,
        ProjectId::new(parts.next().unwrap_or_default())?,
        parts
            .next()
            .ok_or_else(|| CliError::Usage("invalid item ID".into()))?
            .parse()
            .map_err(|_| CliError::Usage("invalid item ID".into()))?,
    )?;
    if parts.next().is_some() || item.to_string() != value {
        return Err(CliError::Usage("invalid item ID".into()));
    }
    Ok(item)
}

fn lifecycle_name(value: Option<&LifecycleMutation>) -> Option<&'static str> {
    Some(match value? {
        LifecycleMutation::Approve => "approve",
        LifecycleMutation::Reject { .. } => "reject",
        LifecycleMutation::Start => "start",
        LifecycleMutation::Block { .. } => "block",
        LifecycleMutation::Resume => "resume",
        LifecycleMutation::Finish => "finish",
    })
}

fn priority_name(value: Priority) -> &'static str {
    match value {
        Priority::P0 => "P0",
        Priority::P1 => "P1",
        Priority::P2 => "P2",
        Priority::P3 => "P3",
        Priority::P4 => "P4",
    }
}

fn status_name(value: Status) -> &'static str {
    match value {
        Status::Proposed => "proposed",
        Status::Ready => "ready",
        Status::InProgress => "in_progress",
        Status::Blocked => "blocked",
        Status::Done => "done",
        Status::Rejected => "rejected",
    }
}

struct SystemClock;
impl Clock for SystemClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new(crate::cli::rfc3339_now())
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
struct SystemIdentities {
    event_count: usize,
}
impl MutationIdentityGenerator for SystemIdentities {
    fn mutation_identity(&mut self) -> MutationIdentity {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let serial = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        MutationIdentity {
            operation_id: format!("cli-mutation-{nonce:032x}-{serial:016x}"),
            event_ids: (0..self.event_count)
                .map(|event| format!("cli-mutation-event-{nonce:032x}-{serial:016x}-{event}"))
                .collect(),
        }
    }
}
