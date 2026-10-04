//! Exact delivery budgets for Phase D's conditional and selected-work envelopes.
use bif::{
    application::{ConditionalReadOutcome, ItemProjection, ItemWork, SelectedWorkOutcome},
    domain::{AssigneeId, ItemId, Priority, ProjectId, RequesterId, Revision, Status},
    v2_response::{
        EncodeError, PayloadTooLarge, ReadError, RecordKind, ResponseBudget, ResponseStats,
        write_conditional_get, write_selected_work,
    },
};
use serde_json::{Value, json};

// These synthetic delivery tokens deliberately exercise escaping and UTF-8;
// validator syntax belongs to the application, not these public serializers.
const VERSION: &str = "version.🦀 café東京\n\"\\\u{0000}";
const ITEM_ID: &str = "DAVIS:bif:041";

fn work() -> ItemProjection {
    ItemProjection::Work(ItemWork::new(
        ItemId::new(
            RequesterId::new("DAVIS").unwrap(),
            ProjectId::new("bif").unwrap(),
            41,
        )
        .unwrap(),
        "Complete 🦀 work\n\"\\",
        Status::Ready,
        Some(Priority::P0),
        Some(AssigneeId::new("davis").unwrap()),
        Revision::new(u64::MAX).unwrap(),
        Some("Preserve 東京\t\r and every field".into()),
        vec![
            "First é criterion".into(),
            "".into(),
            "Last 🦀 criterion".into(),
        ],
        Some("Ready with \"reason\"\n".into()),
    ))
}

fn expected_work() -> Value {
    json!({
        "id": ITEM_ID,
        "title": "Complete 🦀 work\n\"\\",
        "status": "ready",
        "priority": "P0",
        "assignee": "davis",
        "revision": u64::MAX,
        "description": "Preserve 東京\t\r and every field",
        "acceptance_criteria": ["First é criterion", "", "Last 🦀 criterion"],
        "status_reason": "Ready with \"reason\"\n"
    })
}

/// Measure the actual public writer, then require identical, complete delivery
/// at its exact JSON budget and a typed failure before output one byte below it.
fn assert_budget_boundary(
    write: impl Fn(&mut Vec<u8>, ResponseBudget) -> Result<ResponseStats, EncodeError>,
    result: Value,
    record_id: Option<&str>,
) -> Vec<u8> {
    let expected = json!({
        "api_version": 2,
        "schema_version": 1,
        "ok": true,
        "result": result
    });
    let mut ample = vec![];
    let stats = write(&mut ample, ResponseBudget::default()).unwrap();
    assert_eq!(ample.last(), Some(&b'\n'));
    let json_bytes = &ample[..ample.len() - 1];
    let required = json_bytes.len();
    assert_eq!(
        serde_json::from_slice::<Value>(json_bytes).unwrap(),
        expected
    );
    assert_eq!(required, serde_json::to_vec(&expected).unwrap().len());
    assert_eq!(
        stats,
        ResponseStats {
            json_bytes: required,
            emitted_records: usize::from(record_id.is_some()),
        }
    );

    let mut exact = vec![];
    let exact_stats = write(&mut exact, ResponseBudget::new(required).unwrap()).unwrap();
    assert_eq!(
        exact, ample,
        "exact budget must not truncate or alter fields"
    );
    assert_eq!(exact_stats, stats);

    let maximum = required - 1;
    let mut rejected = vec![];
    let error = write(&mut rejected, ResponseBudget::new(maximum).unwrap()).unwrap_err();
    assert!(
        rejected.is_empty(),
        "budget failure must write no partial output"
    );
    match (record_id, error) {
        (Some(record_id), EncodeError::Read(ReadError::PayloadTooLarge(details))) => {
            assert_eq!(
                details,
                PayloadTooLarge {
                    record_kind: RecordKind::Item,
                    record_id: record_id.into(),
                    maximum_response_bytes: maximum,
                    minimum_required_bytes: required,
                }
            );
        }
        (
            None,
            EncodeError::EnvelopeTooLarge {
                maximum_response_bytes,
                minimum_required_bytes,
            },
        ) => {
            assert_eq!(maximum_response_bytes, maximum);
            assert_eq!(minimum_required_bytes, required);
        }
        (_, error) => panic!("unexpected budget classification: {error:?}"),
    }
    ample
}

fn assert_encoded_version(bytes: &[u8]) {
    let text = std::str::from_utf8(bytes).unwrap();
    let encoded_version = serde_json::to_string(VERSION).unwrap();
    assert!(text.contains(&format!("\"version\":{encoded_version}")));
    assert!(
        encoded_version.len() > VERSION.len() + 2,
        "escaping adds bytes"
    );
    assert!(
        VERSION.len() > VERSION.chars().count(),
        "UTF-8 is not a char count"
    );
    assert!(text.len() > text.chars().count());
}

#[test]
fn modified_work_budgets_complete_envelope_and_escaped_version() {
    let outcome = ConditionalReadOutcome::Modified {
        version: VERSION.into(),
        item: work(),
    };
    let bytes = assert_budget_boundary(
        |output, budget| write_conditional_get(output, &outcome, budget),
        json!({
            "outcome": "modified",
            "version": VERSION,
            "item": expected_work()
        }),
        Some(ITEM_ID),
    );
    assert_encoded_version(&bytes);
}

#[test]
fn not_modified_budgets_escaped_version_without_an_item() {
    let outcome = ConditionalReadOutcome::NotModified {
        version: VERSION.into(),
    };
    let bytes = assert_budget_boundary(
        |output, budget| write_conditional_get(output, &outcome, budget),
        json!({
            "outcome": "not_modified",
            "version": VERSION,
            "item": null
        }),
        None,
    );
    assert_encoded_version(&bytes);
}

#[test]
fn selected_work_budgets_complete_work_without_truncation() {
    let outcome = SelectedWorkOutcome::Selected(work());
    assert_budget_boundary(
        |output, budget| write_selected_work(output, &outcome, budget),
        json!({
            "outcome": "selected",
            "item": expected_work()
        }),
        Some(ITEM_ID),
    );
}

#[test]
fn empty_selection_budgets_the_itemless_envelope() {
    assert_budget_boundary(
        |output, budget| write_selected_work(output, &SelectedWorkOutcome::Empty, budget),
        json!({
            "outcome": "empty",
            "item": null
        }),
        None,
    );
}
