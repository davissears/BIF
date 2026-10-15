//! Mutation fixtures; reuse capture's complete persisted-state comparison.
#[allow(dead_code)]
#[path = "../capture_journal_support/mod.rs"]
pub mod capture_support;
pub use capture_support::persisted;

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, Clock, Command, Execution, HumanAuthorization,
        MutationIdentity, MutationIdentityGenerator, MutationRequest, MutationResult,
        MutationUseCaseError, ObservedExecution, mutate_item_idempotent,
    },
    domain::{
        AssigneeId, ItemId, ItemMutation, LifecycleMutation, Priority, Revision, Timestamp, Triage,
        TriageField,
    },
    storage::MutationRepository,
};
use rusqlite::{Connection, params};

pub fn seed(connection: &mut Connection) -> ItemId {
    capture_support::capture(connection, "seed", "seed", "bif")
        .unwrap()
        .item
        .id()
        .clone()
}

pub fn compound() -> ItemMutation {
    ItemMutation {
        lifecycle: Some(LifecycleMutation::Approve),
        triage: Some(Triage {
            priority: TriageField::Set(Priority::P1),
            assignee: TriageField::Set(AssigneeId::new("bob").unwrap()),
            note: Some("ready to build".into()),
        }),
    }
}

pub fn request(id: &ItemId, key: &str, revision: u64, mutation: ItemMutation) -> MutationRequest {
    MutationRequest {
        idempotency_key: key.into(),
        item_id: id.clone(),
        expected_revision: Revision::new(revision).unwrap(),
        mutation,
    }
}

struct FixedClock;
impl Clock for FixedClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new("2025-02-04T05:06:07Z")
    }
}

struct FixedIdentities {
    identity: String,
    event_count: usize,
}
impl MutationIdentityGenerator for FixedIdentities {
    fn mutation_identity(&mut self) -> MutationIdentity {
        MutationIdentity {
            operation_id: format!("operation-{}", self.identity),
            event_ids: (0..self.event_count)
                .map(|index| format!("event-{}-{index}", self.identity))
                .collect(),
        }
    }
}

pub fn mutate(
    connection: &mut Connection,
    request: MutationRequest,
    identity: &str,
    event_count: usize,
) -> Result<MutationResult, MutationUseCaseError<rusqlite::Error>> {
    mutate_with_repository(
        &mut MutationRepository::new(connection).unwrap(),
        request,
        identity,
        event_count,
    )
}

/// Keep constructor initialization outside the process-control boundary.
pub fn mutate_with_repository(
    repository: &mut MutationRepository<'_>,
    request: MutationRequest,
    identity: &str,
    event_count: usize,
) -> Result<MutationResult, MutationUseCaseError<rusqlite::Error>> {
    mutate_item_idempotent(
        repository,
        &mut FixedClock,
        &mut FixedIdentities {
            identity: identity.into(),
            event_count,
        },
        &AuthorizationRequest {
            actor: Actor {
                kind: ActorKind::Human,
                id: "alice",
                surface: "cli",
                host: "local",
            },
            execution: Execution::Direct {
                surface: "cli",
                host: "local",
            },
            observed_execution: ObservedExecution::Direct,
            command: Command::Mutation {
                requested_changes: &["lifecycle", "priority", "assignee", "note"],
            },
            human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
        },
        request,
    )
}

/// Assert the complete new journal row, independent of the current item revision.
/// Historical operations must keep their own revisions as later writes occur.
pub fn assert_mutation(
    connection: &Connection,
    request: &MutationRequest,
    identity: &str,
    position: i64,
    event_types: &[&str],
) {
    let operation = format!("operation-{identity}");
    let item = request.item_id.to_string();
    let revision = request.expected_revision.get() as i64 + 1;
    let row: (i64, String, String, String, i64) = connection
        .query_row(
            "SELECT change_seq, operation_id, item_id, project_id, item_revision
             FROM change_journal WHERE operation_id = ?1",
            [&operation],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            position,
            operation.clone(),
            item.clone(),
            "bif".into(),
            revision
        )
    );
    let linked: i64 = connection
        .query_row(
            "SELECT count(*) FROM operations o
             JOIN items i ON i.item_id = o.item_id
             JOIN mutation_receipts r ON r.operation_id = o.operation_id
             WHERE o.operation_id = ?1 AND o.item_id = ?2 AND i.project_id = 'bif'
               AND o.item_revision = ?3 AND o.expected_revision = ?4
               AND o.operation_type = 'triage' AND r.item_id = ?2 AND r.mutation_key = ?5
               AND o.occurred_at = r.created_at",
            params![
                operation,
                item,
                revision,
                revision - 1,
                request.idempotency_key
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        linked, 1,
        "one complete operation/item/project/revision/receipt link"
    );
    let events: Vec<(i64, String, String, i64, String)> = connection
        .prepare(
            "SELECT event_index, event_type, item_id, item_revision, actor_id
             FROM events WHERE operation_id = ?1 ORDER BY event_index",
        )
        .unwrap()
        .query_map([&operation], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        events,
        event_types
            .iter()
            .enumerate()
            .map(|(index, kind)| (
                index as i64,
                (*kind).into(),
                item.clone(),
                revision,
                "alice".into()
            ))
            .collect::<Vec<_>>()
    );
}

/// No hidden extra rows or allocations, even for a four-event compound operation.
pub fn assert_coverage(connection: &Connection, positions: &[i64]) {
    let actual: Vec<i64> = connection
        .prepare("SELECT change_seq FROM change_journal ORDER BY change_seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(actual, positions);
    let high_water = *positions.last().unwrap();
    let metadata: (i64, i64, i64) = connection
        .query_row(
            "SELECT high_water, sync_enabled, bootstrap_required FROM journal_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        metadata,
        (high_water, 0, 1),
        "sync stays inactive/bootstrap-required"
    );
    let allocator: i64 = connection
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'change_journal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(allocator, high_water);
    for table in ["operations", "mutation_receipts"] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, high_water,
            "exactly one {table} row per effective operation"
        );
    }
}
