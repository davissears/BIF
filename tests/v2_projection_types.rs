use std::collections::BTreeSet;

use bif::{
    application::{ItemAudit, ItemSummary, ItemWork},
    domain::{
        AssigneeId, Item, ItemContent, ItemId, MessageId, Priority, ProjectId, Provenance,
        RepositoryReference, RequesterId, Revision, RevisionReference, SourceHost, SourceUrl,
        Status, ThreadId, Timestamp,
    },
};
use serde_json::{Value, json};

const CONTRACT: &str = include_str!("../docs/fixtures/bif-v2-read-contract.json");

fn provenance() -> Provenance {
    Provenance::new(
        Some(SourceHost::Delta),
        Some(ThreadId::new("thread-42")),
        Some(MessageId::new("message-7")),
        Some(SourceUrl::new("https://example.test/thread-42")),
        Some(RepositoryReference::new("BIF")),
        Some(RevisionReference::new("abc123")),
        Some("Only load requested content".to_owned()),
    )
}

fn item() -> Item {
    Item::new(
        ItemId::new(
            RequesterId::new("DAVIS").unwrap(),
            ProjectId::new("bif").unwrap(),
            42,
        )
        .unwrap(),
        ItemContent::new(
            "Bound reads",
            Some("Only load requested content".to_owned()),
            vec!["Keep field order".to_owned(), "Preserve nulls".to_owned()],
        )
        .unwrap(),
        Status::InProgress,
        Some(Priority::P1),
        Some(AssigneeId::new("delta").unwrap()),
        Some("Implementing bounded reads".to_owned()),
        Revision::new(7).unwrap(),
        Timestamp::new("2026-09-14T10:00:00Z"),
        Timestamp::new("2026-09-14T11:00:00Z"),
        provenance(),
    )
}

// Handcrafted fixture shape only; production serialization belongs to V2-013.
fn summary_fixture_json(item: &ItemSummary) -> Value {
    json!({
        "id": item.id.to_string(),
        "title": item.title,
        "status": "in_progress",
        "priority": "P1",
        "assignee": item.assignee.as_ref().map(AssigneeId::as_str),
        "revision": item.revision.get(),
    })
}

fn fixture_fields(projection: &str) -> BTreeSet<String> {
    serde_json::from_str::<Value>(CONTRACT).unwrap()["projection_schemas"][projection]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn projections_preserve_every_populated_field() {
    let item = item();
    let summary = ItemSummary::from(&item);
    let work = ItemWork::from(&item);
    let audit = ItemAudit::from(&item);

    assert_eq!(summary.id, audit.id);
    assert_eq!(summary.title, "Bound reads");
    assert_eq!(summary.status, Status::InProgress);
    assert_eq!(summary.priority, Some(Priority::P1));
    assert_eq!(summary.assignee.as_ref().unwrap().as_str(), "delta");
    assert_eq!(summary.revision.get(), 7);

    assert_eq!(work.id, summary.id);
    assert_eq!(work.title, summary.title);
    assert_eq!(work.status, summary.status);
    assert_eq!(work.priority, summary.priority);
    assert_eq!(work.assignee, summary.assignee);
    assert_eq!(work.revision, summary.revision);
    assert_eq!(
        work.description.as_deref(),
        Some("Only load requested content")
    );
    assert_eq!(
        work.acceptance_criteria,
        ["Keep field order", "Preserve nulls"]
    );
    assert_eq!(
        work.status_reason.as_deref(),
        Some("Implementing bounded reads")
    );

    assert_eq!(audit.id, work.id);
    assert_eq!(audit.title, work.title);
    assert_eq!(audit.status, work.status);
    assert_eq!(audit.priority, work.priority);
    assert_eq!(audit.assignee, work.assignee);
    assert_eq!(audit.revision, work.revision);
    assert_eq!(audit.description, work.description);
    assert_eq!(audit.acceptance_criteria, work.acceptance_criteria);
    assert_eq!(audit.status_reason, work.status_reason);
    assert_eq!(audit.requester().as_str(), "DAVIS");
    assert_eq!(audit.project().as_str(), "bif");
    assert_eq!(audit.sequence(), 42);
    assert_eq!(audit.captured_at.as_str(), "2026-09-14T10:00:00Z");
    assert_eq!(audit.updated_at.as_str(), "2026-09-14T11:00:00Z");
    assert_eq!(audit.provenance, provenance());
}

#[test]
fn handcrafted_summary_json_matches_fixture_shape() {
    let encoded = summary_fixture_json(&ItemSummary::from(&item()));
    let actual = encoded
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();

    assert_eq!(actual, fixture_fields("summary"));
    assert!(actual.is_disjoint(&BTreeSet::from([
        "description".to_owned(),
        "acceptance_criteria".to_owned(),
        "status_reason".to_owned(),
        "provenance".to_owned(),
    ])));
}

#[test]
fn converted_projections_replace_populated_fields_with_nulls_and_empty_criteria() {
    let populated = item();
    let cleared = Item::new(
        populated.id().clone(),
        ItemContent::new("Cleared content", None, vec![]).unwrap(),
        Status::Ready,
        None,
        None,
        None,
        Revision::new(8).unwrap(),
        populated.captured_at().clone(),
        Timestamp::new("2026-09-14T12:00:00Z"),
        Provenance::default(),
    );
    let mut summary = ItemSummary::from(&populated);
    let mut work = ItemWork::from(&populated);
    let mut audit = ItemAudit::from(&populated);
    assert!(summary.priority.is_some());
    assert!(summary.assignee.is_some());
    assert!(work.description.is_some());
    assert!(!work.acceptance_criteria.is_empty());
    assert!(work.status_reason.is_some());
    assert_eq!(audit.provenance, provenance());

    summary = ItemSummary::from(&cleared);
    work = ItemWork::from(&cleared);
    audit = ItemAudit::from(&cleared);

    assert_eq!(summary.id, *cleared.id());
    assert_eq!(summary.title, "Cleared content");
    assert_eq!(summary.status, Status::Ready);
    assert_eq!(summary.revision.get(), 8);
    assert_eq!(summary.priority, None);
    assert_eq!(summary.assignee, None);
    assert_eq!(work.id, summary.id);
    assert_eq!(work.title, summary.title);
    assert_eq!(work.status, summary.status);
    assert_eq!(work.revision, summary.revision);
    assert_eq!(work.priority, None);
    assert_eq!(work.assignee, None);
    assert_eq!(work.description, None);
    assert!(work.acceptance_criteria.is_empty());
    assert_eq!(work.status_reason, None);
    assert_eq!(audit.id, work.id);
    assert_eq!(audit.title, work.title);
    assert_eq!(audit.status, work.status);
    assert_eq!(audit.revision, work.revision);
    assert_eq!(audit.priority, None);
    assert_eq!(audit.assignee, None);
    assert_eq!(audit.description, None);
    assert!(audit.acceptance_criteria.is_empty());
    assert_eq!(audit.status_reason, None);
    assert_eq!(audit.provenance, Provenance::default());
    assert_eq!(audit.requester(), cleared.id().requester());
    assert_eq!(audit.project(), cleared.id().project());
    assert_eq!(audit.sequence(), cleared.id().sequence());
    assert_eq!(audit.captured_at, *cleared.captured_at());
    assert_eq!(audit.updated_at, *cleared.updated_at());
}

#[test]
fn populated_constructors_preserve_fields_and_identity_tracks_replaced_id() {
    let item = item();
    let summary = ItemSummary::new(
        item.id().clone(),
        item.content().title(),
        item.status(),
        item.priority(),
        item.assignee().cloned(),
        item.revision(),
    );
    let work = ItemWork::new(
        item.id().clone(),
        item.content().title(),
        item.status(),
        item.priority(),
        item.assignee().cloned(),
        item.revision(),
        item.content().description().map(str::to_owned),
        item.content().acceptance_criteria().to_vec(),
        item.status_reason().map(str::to_owned),
    );
    let audit = ItemAudit::new(
        item.id().clone(),
        item.content().title(),
        item.status(),
        item.priority(),
        item.assignee().cloned(),
        item.revision(),
        item.content().description().map(str::to_owned),
        item.content().acceptance_criteria().to_vec(),
        item.status_reason().map(str::to_owned),
        item.captured_at().clone(),
        item.updated_at().clone(),
        item.provenance().clone(),
    );
    assert_eq!(summary, ItemSummary::from(&item));
    assert_eq!(work, ItemWork::from(&item));
    assert_eq!(audit, ItemAudit::from(&item));

    let replacement_id = ItemId::new(
        RequesterId::new("OTHER").unwrap(),
        ProjectId::new("other-project").unwrap(),
        u64::MAX,
    )
    .unwrap();
    for mut audit in [audit, ItemAudit::from(&item)] {
        audit.id = replacement_id.clone();
        assert_eq!(audit.requester(), replacement_id.requester());
        assert_eq!(audit.project(), replacement_id.project());
        assert_eq!(audit.sequence(), u64::MAX);
    }
}

#[test]
fn projections_can_be_assembled_directly_without_a_complete_item() {
    let id = ItemId::new(
        RequesterId::new("DAVIS").unwrap(),
        ProjectId::new("bif").unwrap(),
        9_007_199_254_740_993,
    )
    .unwrap();
    let revision = Revision::new(u64::MAX).unwrap();
    let summary = ItemSummary::new(
        id.clone(),
        "Direct row",
        Status::Ready,
        None,
        None,
        revision,
    );
    let work = ItemWork::new(
        id.clone(),
        "Direct row",
        Status::Ready,
        None,
        None,
        revision,
        None,
        vec![],
        None,
    );
    let audit = ItemAudit::new(
        id.clone(),
        "Direct row",
        Status::Ready,
        None,
        None,
        revision,
        None,
        vec![],
        None,
        Timestamp::new("2025-01-02T03:04:05Z"),
        Timestamp::new("2025-01-02T03:04:05Z"),
        Provenance::default(),
    );
    assert_eq!(summary.id, id);
    assert_eq!(work.revision, revision);
    assert_eq!(audit.requester(), id.requester());
    assert_eq!(audit.project(), id.project());
    assert_eq!(audit.sequence(), 9_007_199_254_740_993);
    assert_eq!(audit.revision.get(), u64::MAX);
    assert_eq!(summary.priority, None);
    assert_eq!(summary.assignee, None);
    assert_eq!(work.priority, None);
    assert_eq!(work.assignee, None);
    assert_eq!(work.description, None);
    assert!(work.acceptance_criteria.is_empty());
    assert_eq!(work.status_reason, None);
    assert_eq!(audit.priority, None);
    assert_eq!(audit.assignee, None);
    assert_eq!(audit.description, None);
    assert!(audit.acceptance_criteria.is_empty());
    assert_eq!(audit.status_reason, None);
    assert_eq!(audit.provenance, Provenance::default());
}
