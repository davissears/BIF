//! Use cases, authorization, and operation coordination.
//!
//! This layer may depend on [`crate::domain`], but not on delivery or
//! infrastructure modules.

use std::{error::Error, fmt};

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
