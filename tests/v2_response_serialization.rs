//! Focused adapter contract tests; dynamic JSON is used only as a test oracle.
use std::io::{self, Write};

use bif::{
    application::{
        ActorKind, EventActor, EventExecution, ItemAudit, ItemHistoryEvent, ItemProjection,
        ItemReadKey, ItemSummary, ItemWork, ProjectedItemRow, ReadPage,
    },
    domain::{
        AssigneeId, EventType, EventValue, ItemId, MessageId, Priority, ProjectId, Provenance,
        RepositoryReference, RequesterId, Revision, RevisionReference, SourceHost, SourceUrl,
        Status, ThreadId, Timestamp,
    },
    v2_response::{
        EncodeError, ReadError, RecordKind, ResponseBudget, write_error, write_get,
        write_history_page, write_item_page,
    },
};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../docs/fixtures/bif-v2-read-contract.json")).unwrap()
}

fn id(sequence: u64) -> ItemId {
    ItemId::new(
        RequesterId::new("DAVIS").unwrap(),
        ProjectId::new("bif").unwrap(),
        sequence,
    )
    .unwrap()
}

fn audit(sequence: u64) -> ItemAudit {
    ItemAudit::new(
        id(sequence),
        "Null contract case",
        Status::Proposed,
        None,
        None,
        Revision::new(1).unwrap(),
        None,
        vec![],
        None,
        Timestamp::new("2025-01-02T03:04:05Z"),
        Timestamp::new("2025-01-02T03:04:05Z"),
        Provenance::default(),
    )
}

fn work(sequence: u64) -> ItemWork {
    ItemWork::new(
        id(sequence),
        "Work",
        Status::Ready,
        None,
        None,
        Revision::new(1).unwrap(),
        None,
        vec![],
        None,
    )
}

fn row(sequence: u64) -> ProjectedItemRow {
    ProjectedItemRow {
        item: ItemProjection::Work(work(sequence)),
        key: ItemReadKey {
            id: id(sequence),
            captured_at: Timestamp::new("2025-01-02T03:04:05Z"),
            priority: None,
        },
    }
}

fn event() -> ItemHistoryEvent {
    ItemHistoryEvent {
        operation_id: "op".into(),
        event_id: "event".into(),
        item_revision: Revision::new(u64::MAX).unwrap(),
        event_index: u64::MAX,
        event_type: EventType::AssigneeChanged,
        before: Some(EventValue::Assignee(Some(
            AssigneeId::new("davis").unwrap(),
        ))),
        after: Some(EventValue::Assignee(None)),
        actor: EventActor {
            kind: ActorKind::Human,
            id: "davis".into(),
            surface: "cli".into(),
            host: "local".into(),
        },
        execution: EventExecution::Direct {
            surface: "cli".into(),
            host: "local".into(),
        },
        reason: None,
        note: None,
        occurred_at: Timestamp::new("2025-01-02T03:04:05Z"),
        schema_version: 1,
    }
}

fn budget(bytes: usize) -> ResponseBudget {
    ResponseBudget::new(bytes).unwrap()
}

fn get(item: &ItemProjection) -> Vec<u8> {
    let mut output = vec![];
    write_get(&mut output, item, ResponseBudget::default()).unwrap();
    output
}

fn value(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'));
    serde_json::from_slice(bytes).unwrap()
}

fn assert_order(bytes: &[u8], fields: &Value) {
    let text = std::str::from_utf8(bytes).unwrap();
    let mut remaining = text;
    for field in fields.as_array().unwrap() {
        let key = format!("\"{}\":", field.as_str().unwrap());
        let pos = remaining.find(&key).unwrap();
        remaining = &remaining[pos + key.len()..];
    }
}

#[test]
fn projections_match_normative_fixtures_and_exact_serializer_order() {
    let contract = fixture();
    let nulls = ItemProjection::Audit(audit(1));
    assert_eq!(
        value(&get(&nulls))["result"]["item"],
        contract["coverage_cases"]["nulls_and_empty_arrays"]["item"]
    );
    let p4 = ItemProjection::Summary(ItemSummary::new(
        id(4),
        "P4 contract case",
        Status::Ready,
        Some(Priority::P4),
        Some(AssigneeId::new("davis").unwrap()),
        Revision::new(2).unwrap(),
    ));
    assert_eq!(
        value(&get(&p4))["result"]["item"],
        contract["coverage_cases"]["p4"]["item"]
    );
    let mut large = audit(9_007_199_254_740_993);
    large.title = "Large numeric ID contract case".into();
    large.status = Status::Ready;
    large.priority = Some(Priority::P0);
    large.assignee = Some(AssigneeId::new("davis").unwrap());
    large.description = Some("Keep the sequence as an exact unsigned integer.".into());
    large.provenance = Provenance::new(Some(SourceHost::Delta), None, None, None, None, None, None);
    assert_eq!(
        value(&get(&ItemProjection::Audit(large)))["result"]["item"],
        contract["coverage_cases"]["large_numeric_id"]["item"]
    );
    for (name, projection) in [
        ("summary", p4),
        ("work", ItemProjection::Work(work(1))),
        ("audit", nulls),
    ] {
        let bytes = get(&projection);
        let expected_fields = &contract["projection_schemas"][name]["fields"];
        assert_order(&bytes, &contract["results"]["success_envelope_fields"]);
        assert_order(&bytes, expected_fields);
        assert_eq!(
            value(&bytes)["result"]["item"].as_object().unwrap().len(),
            expected_fields.as_array().unwrap().len()
        );
        if name == "audit" {
            assert_order(
                &bytes,
                &contract["projection_schemas"]["audit"]["provenance_fields"],
            );
            assert!(value(&bytes)["result"]["item"].get("history").is_none());
        }
    }
}

#[test]
fn audit_provenance_is_complete_and_identity_comes_from_the_current_id() {
    let mut item = audit(1);
    item.id = id(u64::MAX);
    item.description = Some("Complete work".into());
    item.acceptance_criteria = vec!["second".into(), "first".into()];
    item.status_reason = Some("Waiting".into());
    item.provenance = Provenance::new(
        Some(SourceHost::Codex),
        Some(ThreadId::new("thread🦀")),
        Some(MessageId::new("message\"")),
        Some(SourceUrl::new("https://example.test/é")),
        Some(RepositoryReference::new("repo\\ref")),
        Some(RevisionReference::new("revision\n")),
        Some("Excerpt\u{0000}".into()),
    );
    let bytes = get(&ItemProjection::Audit(item));
    let actual = value(&bytes);
    let item = &actual["result"]["item"];
    assert_eq!(item["sequence"].as_u64(), Some(u64::MAX));
    assert_eq!(item["id"], id(u64::MAX).to_string());
    assert_eq!(item["acceptance_criteria"], json!(["second", "first"]));
    assert_eq!(item["status_reason"], "Waiting");
    assert_eq!(
        item["provenance"],
        json!({
            "source_host": "codex", "thread_id": "thread🦀", "message_id": "message\"",
            "url": "https://example.test/é", "repository_reference": "repo\\ref",
            "revision_reference": "revision\n", "context_excerpt": "Excerpt\u{0000}",
        })
    );
}

#[test]
fn history_is_complete_ordered_and_preserves_null_clearing_and_u64() {
    let contract = fixture();
    let mut agent = event();
    agent.actor.kind = ActorKind::Agent;
    agent.execution = EventExecution::Agent {
        agent_id: "worker".into(),
        surface: "delta".into(),
        host: "host".into(),
    };
    let mut cleared = event();
    cleared.event_type = EventType::PriorityChanged;
    cleared.before = Some(EventValue::Priority(Some(Priority::P4)));
    cleared.after = Some(EventValue::Priority(None));
    let events = ReadPage {
        records: vec![event(), agent, cleared],
        has_more: false,
    };
    let mut bytes = vec![];
    write_history_page(
        &mut bytes,
        &id(1),
        &events,
        ResponseBudget::default(),
        |_| panic!("terminal history must not request a cursor"),
    )
    .unwrap();
    let result = value(&bytes);
    assert_order(&bytes, &contract["history_schema"]["result_fields"]);
    assert_order(&bytes, &contract["history_schema"]["event_fields"]);
    assert_order(&bytes, &contract["history_schema"]["actor_fields"]);
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains(r#""execution":{"kind":"direct","surface":"cli","host":"local"}"#));
    assert!(text.contains(
        r#""execution":{"kind":"agent","agent_id":"worker","surface":"delta","host":"host"}"#
    ));
    for (index, event) in events.records.iter().enumerate() {
        let actual = &result["result"]["events"][index];
        assert_eq!(actual, &bif::rpc_read::history_json(event));
        assert_eq!(actual["item_revision"].as_u64(), Some(u64::MAX));
        assert_eq!(actual["event_index"].as_u64(), Some(u64::MAX));
        assert!(actual["after"].is_null());
        assert_eq!(actual.as_object().unwrap().len(), 13);
    }
    assert_eq!(
        result["result"]["events"][1]["execution"]
            .as_object()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        result["result"]["events"][0]["execution"]
            .as_object()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn unicode_escaping_and_u64_are_counted_as_encoded_utf8_not_characters() {
    let mut item = work(u64::MAX);
    item.title = "🦀 café\n\"\\\u{0000}".into();
    item.description = Some("東京\t\r".into());
    item.acceptance_criteria = vec!["é".into(), "🦀".into()];
    item.revision = Revision::new(u64::MAX).unwrap();
    let item = ItemProjection::Work(item);
    let bytes = get(&item);
    let exact = bytes.len() - 1;
    let mut output = vec![];
    write_get(&mut output, &item, budget(exact)).unwrap();
    assert_eq!(output, bytes);
    assert_eq!(
        value(&bytes)["result"]["item"]["revision"].as_u64(),
        Some(u64::MAX)
    );
    let mut output = vec![];
    let error = write_get(&mut output, &item, budget(exact - 1)).unwrap_err();
    assert_payload(
        error,
        RecordKind::Item,
        &id(u64::MAX).to_string(),
        exact,
        exact - 1,
    );
    assert!(output.is_empty());
}

fn assert_payload(error: EncodeError, kind: RecordKind, record_id: &str, min: usize, max: usize) {
    let EncodeError::Read(ReadError::PayloadTooLarge(details)) = error else {
        panic!("unexpected error: {error:?}");
    };
    assert_eq!(details.record_kind, kind);
    assert_eq!(details.record_id, record_id);
    assert_eq!(details.minimum_required_bytes, min);
    assert_eq!(details.maximum_response_bytes, max);
}

#[test]
fn empty_pages_match_fixture_and_do_not_generate_cursors() {
    let contract = fixture();
    let mut bytes = vec![];
    write_item_page(
        &mut bytes,
        &ReadPage {
            records: vec![],
            has_more: false,
        },
        ResponseBudget::default(),
        |_| panic!("empty cursor"),
    )
    .unwrap();
    assert_eq!(
        value(&bytes)["result"],
        contract["coverage_cases"]["empty_results"]["list"]
    );
    let exact = bytes.len() - 1;
    bytes.clear();
    write_item_page(
        &mut bytes,
        &ReadPage {
            records: vec![],
            has_more: false,
        },
        budget(exact),
        |_| panic!("empty cursor"),
    )
    .unwrap();
    assert_eq!(bytes.len(), exact + 1);
    bytes.clear();
    write_history_page(
        &mut bytes,
        &id(1),
        &ReadPage {
            records: vec![],
            has_more: false,
        },
        ResponseBudget::default(),
        |_| panic!("empty cursor"),
    )
    .unwrap();
    assert_eq!(
        value(&bytes)["result"],
        contract["coverage_cases"]["empty_results"]["history"]
    );
}

#[test]
fn page_budget_reserves_envelope_and_escaped_cursor_from_last_emitted_row() {
    let cursor = "cursor🦀\"\\\n";
    let one = ReadPage {
        records: vec![row(999)],
        has_more: true,
    };
    let mut expected = vec![];
    write_item_page(&mut expected, &one, ResponseBudget::default(), |last| {
        assert_eq!(last.key.id.sequence(), 999);
        Ok(cursor.into())
    })
    .unwrap();
    let exact = expected.len() - 1;
    let page = ReadPage {
        records: vec![row(999), row(1000)],
        has_more: false,
    };
    let mut bytes = vec![];
    let mut called = vec![];
    write_item_page(&mut bytes, &page, budget(exact), |last| {
        called.push(last.key.id.sequence());
        Ok(cursor.into())
    })
    .unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(called, [999]);
    assert_eq!(value(&bytes)["result"]["next_cursor"], cursor);
    bytes.clear();
    let error =
        write_item_page(&mut bytes, &page, budget(exact - 1), |_| Ok(cursor.into())).unwrap_err();
    assert_payload(error, RecordKind::Item, "DAVIS:bif:999", exact, exact - 1);
    assert!(bytes.is_empty());
}

#[test]
fn required_cursor_can_make_an_otherwise_fitting_first_record_oversized() {
    let mut page = ReadPage {
        records: vec![row(1)],
        has_more: false,
    };
    let mut terminal = vec![];
    write_item_page(
        &mut terminal,
        &page,
        ResponseBudget::default(),
        |_| panic!(),
    )
    .unwrap();
    page.has_more = true;
    let cursor = "continuation".repeat(40);
    let mut continued = vec![];
    write_item_page(&mut continued, &page, ResponseBudget::default(), |_| {
        Ok(cursor.clone())
    })
    .unwrap();
    let mut bytes = vec![];
    let error = write_item_page(&mut bytes, &page, budget(terminal.len() - 1), |_| {
        Ok(cursor.clone())
    })
    .unwrap_err();
    assert_payload(
        error,
        RecordKind::Item,
        "DAVIS:bif:001",
        continued.len() - 1,
        terminal.len() - 1,
    );
    assert!(bytes.is_empty());
}

#[test]
fn largest_fitting_prefix_handles_variable_cursor_size_and_terminal_null() {
    let terminal = ReadPage {
        records: vec![row(1), row(2)],
        has_more: false,
    };
    let mut expected = vec![];
    write_item_page(
        &mut expected,
        &terminal,
        ResponseBudget::default(),
        |_| panic!(),
    )
    .unwrap();
    let mut bytes = vec![];
    // A huge cursor for an earlier prefix must not reject a fitting terminal
    // page. No cursor work is necessary for a page that fits in its entirety.
    write_item_page(
        &mut bytes,
        &terminal,
        budget(expected.len() - 1),
        |_| panic!(),
    )
    .unwrap();
    assert_eq!(bytes, expected);

    // The longest possible prefix's cursor need not fit. Fall back to the last
    // row actually returned, not the largest counted prefix or storage sentinel.
    let page = ReadPage {
        records: vec![row(1), row(2)],
        has_more: true,
    };
    let mut short = vec![];
    write_item_page(
        &mut short,
        &ReadPage {
            records: vec![row(1)],
            has_more: true,
        },
        ResponseBudget::default(),
        |_| Ok("short".into()),
    )
    .unwrap();
    bytes.clear();
    let mut called = vec![];
    write_item_page(&mut bytes, &page, budget(expected.len() - 1), |last| {
        called.push(last.key.id.sequence());
        Ok(if last.key.id.sequence() == 2 {
            "x".repeat(2000)
        } else {
            "short".into()
        })
    })
    .unwrap();
    assert_eq!(bytes, short);
    assert_eq!(called, [2, 1]);
}

#[test]
fn short_cursor_can_fit_when_the_null_cursor_lower_bound_would_not() {
    let mut page = ReadPage {
        records: vec![row(1), row(2)],
        has_more: false,
    };
    let mut terminal = vec![];
    write_item_page(
        &mut terminal,
        &page,
        ResponseBudget::default(),
        |_| panic!(),
    )
    .unwrap();
    page.has_more = true;
    let mut expected = vec![];
    write_item_page(&mut expected, &page, ResponseBudget::default(), |_| {
        Ok(String::new())
    })
    .unwrap();
    assert_eq!(expected.len() + 2, terminal.len());
    let mut bytes = vec![];
    write_item_page(&mut bytes, &page, budget(expected.len() - 1), |_| {
        Ok(String::new())
    })
    .unwrap();
    assert_eq!(bytes, expected);
}

#[test]
fn page_order_is_not_resorted_and_count_schema_bounds_are_enforced() {
    let page = ReadPage {
        records: vec![row(1000), row(999)],
        has_more: false,
    };
    let mut bytes = vec![];
    let stats =
        write_item_page(&mut bytes, &page, ResponseBudget::default(), |_| panic!()).unwrap();
    assert_eq!(stats.emitted_records, 2);
    assert_eq!(stats.json_bytes, bytes.len() - 1);
    assert_eq!(value(&bytes)["result"]["items"][0]["id"], "DAVIS:bif:1000");
    assert_eq!(value(&bytes)["result"]["items"][1]["id"], "DAVIS:bif:999");
    assert_eq!(value(&bytes)["result"].as_object().unwrap().len(), 2);
    let mut mixed = row(2);
    mixed.item = ItemProjection::Audit(audit(2));
    for invalid in [
        ReadPage {
            records: (1..=101).map(row).collect(),
            has_more: false,
        },
        ReadPage {
            records: vec![],
            has_more: true,
        },
        ReadPage {
            records: vec![row(1), mixed],
            has_more: false,
        },
    ] {
        bytes.clear();
        assert!(matches!(
            write_item_page(
                &mut bytes,
                &invalid,
                ResponseBudget::default(),
                |_| panic!()
            ),
            Err(EncodeError::Read(ReadError::Internal))
        ));
        assert!(bytes.is_empty());
    }
    assert_eq!(ResponseBudget::new(1_048_577), Err(ReadError::InvalidInput));
    bytes.clear();
    let maximum = ReadPage {
        records: (1..=100).map(row).collect(),
        has_more: false,
    };
    assert_eq!(
        write_item_page(
            &mut bytes,
            &maximum,
            ResponseBudget::default(),
            |_| panic!()
        )
        .unwrap()
        .emitted_records,
        100
    );
}

#[test]
fn empty_and_error_envelopes_are_budgeted_before_output() {
    let page = ReadPage {
        records: vec![],
        has_more: false,
    };
    let mut bytes = vec![];
    write_item_page(&mut bytes, &page, ResponseBudget::default(), |_| panic!()).unwrap();
    let exact = bytes.len() - 1;
    bytes.clear();
    assert!(
        matches!(write_item_page(&mut bytes, &page, budget(exact - 1), |_| panic!()),
        Err(EncodeError::EnvelopeTooLarge { minimum_required_bytes, .. }) if minimum_required_bytes == exact)
    );
    assert!(bytes.is_empty());
    write_error(&mut bytes, &ReadError::NotFound, ResponseBudget::default()).unwrap();
    let exact = bytes.len() - 1;
    bytes.clear();
    write_error(&mut bytes, &ReadError::NotFound, budget(exact)).unwrap();
    bytes.clear();
    assert!(
        matches!(write_error(&mut bytes, &ReadError::NotFound, budget(exact - 1)),
        Err(EncodeError::EnvelopeTooLarge { minimum_required_bytes, .. }) if minimum_required_bytes == exact)
    );
    assert!(bytes.is_empty());
}

#[test]
fn history_budget_stops_at_complete_events_and_cursor_uses_last_emitted_event() {
    let mut second = event();
    second.event_id = "second".into();
    let one = ReadPage {
        records: vec![event()],
        has_more: true,
    };
    let mut expected = vec![];
    write_history_page(
        &mut expected,
        &id(1),
        &one,
        ResponseBudget::default(),
        |last| Ok(last.event_id.clone()),
    )
    .unwrap();
    let page = ReadPage {
        records: vec![event(), second],
        has_more: false,
    };
    let mut bytes = vec![];
    write_history_page(
        &mut bytes,
        &id(1),
        &page,
        budget(expected.len() - 1),
        |last| Ok(last.event_id.clone()),
    )
    .unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(
        value(&bytes)["result"]["events"].as_array().unwrap().len(),
        1
    );
    bytes.clear();
    let error = write_history_page(
        &mut bytes,
        &id(1),
        &one,
        budget(expected.len() - 2),
        |last| Ok(last.event_id.clone()),
    )
    .unwrap_err();
    assert_payload(
        error,
        RecordKind::Event,
        "event",
        expected.len() - 1,
        expected.len() - 2,
    );
    assert!(bytes.is_empty());
}

#[test]
fn first_oversized_record_matches_recipe_without_a_success_prefix() {
    let contract = fixture();
    let recipe = &contract["coverage_cases"]["oversized_record"];
    let mut item = work(7);
    item.title = "Oversized record contract case".into();
    item.description = Some("x".repeat(1_048_576));
    let mut bytes = vec![];
    let error = write_get(
        &mut bytes,
        &ItemProjection::Work(item),
        ResponseBudget::default(),
    )
    .unwrap_err();
    assert!(bytes.is_empty());
    assert_payload(
        error,
        RecordKind::Item,
        "DAVIS:bif:007",
        1_048_829,
        1_048_576,
    );
    let details = bif::v2_response::PayloadTooLarge {
        record_kind: RecordKind::Item,
        record_id: "DAVIS:bif:007".into(),
        maximum_response_bytes: 1_048_576,
        minimum_required_bytes: 1_048_829,
    };
    write_error(
        &mut bytes,
        &ReadError::PayloadTooLarge(details),
        ResponseBudget::default(),
    )
    .unwrap();
    assert_eq!(value(&bytes)["error"], recipe["expected_error"]);
}

#[test]
fn all_closed_enums_and_error_envelopes_use_canonical_spellings() {
    let contract = fixture();
    for (status, spelling) in [
        Status::Proposed,
        Status::Ready,
        Status::InProgress,
        Status::Blocked,
        Status::Done,
        Status::Rejected,
    ]
    .into_iter()
    .zip(contract["closed_enums"]["status"].as_array().unwrap())
    {
        let mut item = work(1);
        item.status = status;
        assert_eq!(
            &value(&get(&ItemProjection::Work(item)))["result"]["item"]["status"],
            spelling
        );
    }
    for (priority, spelling) in [
        Priority::P0,
        Priority::P1,
        Priority::P2,
        Priority::P3,
        Priority::P4,
    ]
    .into_iter()
    .zip(contract["closed_enums"]["priority"].as_array().unwrap())
    {
        let mut item = work(1);
        item.priority = Some(priority);
        assert_eq!(
            &value(&get(&ItemProjection::Work(item)))["result"]["item"]["priority"],
            spelling
        );
    }
    for (source, spelling) in [SourceHost::Delta, SourceHost::Codex, SourceHost::Local]
        .into_iter()
        .zip(contract["closed_enums"]["source_host"].as_array().unwrap())
    {
        let mut item = audit(1);
        item.provenance = Provenance::new(Some(source), None, None, None, None, None, None);
        assert_eq!(
            &value(&get(&ItemProjection::Audit(item)))["result"]["item"]["provenance"]["source_host"],
            spelling
        );
    }
    for (kind, spelling) in [
        EventType::Captured,
        EventType::Approved,
        EventType::Rejected,
        EventType::Started,
        EventType::Blocked,
        EventType::Resumed,
        EventType::Finished,
        EventType::PriorityChanged,
        EventType::AssigneeChanged,
        EventType::NoteAdded,
    ]
    .into_iter()
    .zip(contract["closed_enums"]["event_type"].as_array().unwrap())
    {
        let mut event = event();
        event.event_type = kind;
        event.before = Some(EventValue::Status(Status::InProgress));
        event.after = Some(EventValue::Note("note".into()));
        let page = ReadPage {
            records: vec![event],
            has_more: false,
        };
        let mut bytes = vec![];
        write_history_page(
            &mut bytes,
            &id(1),
            &page,
            ResponseBudget::default(),
            |_| panic!(),
        )
        .unwrap();
        assert_eq!(
            &value(&bytes)["result"]["events"][0]["event_type"],
            spelling
        );
        assert_eq!(
            value(&bytes)["result"]["events"][0]["before"],
            "in_progress"
        );
        assert_eq!(value(&bytes)["result"]["events"][0]["after"], "note");
    }
    for error in [
        ReadError::InvalidInput,
        ReadError::NotFound,
        ReadError::Unauthorized,
        ReadError::UnsupportedVersion,
        ReadError::NotInitialized,
        ReadError::StorageBusy,
        ReadError::InvalidCursor {
            reason: "binding_mismatch".into(),
            restart_required: true,
        },
        ReadError::Internal,
    ] {
        let mut bytes = vec![];
        write_error(&mut bytes, &error, ResponseBudget::default()).unwrap();
        let actual = value(&bytes);
        assert_eq!(actual["ok"], false);
        assert_eq!(actual.as_object().unwrap().len(), 4);
        assert_eq!(actual["error"].as_object().unwrap().len(), 3);
        assert!(actual["error"]["message"].as_str().unwrap().len() > 0);
        assert!(
            contract["closed_enums"]["read_error_code"]
                .as_array()
                .unwrap()
                .contains(&actual["error"]["code"])
        );
        assert_order(&bytes, &contract["results"]["error_envelope_fields"]);
        if matches!(error, ReadError::InvalidCursor { .. }) {
            assert_eq!(
                actual["error"]["details"],
                json!({"reason":"binding_mismatch","restart_required":true})
            );
        } else {
            assert_eq!(actual["error"]["details"], json!({}));
        }
    }
}

#[test]
fn cursor_failure_and_output_write_failure_propagate() {
    let mut bytes = vec![];
    let error = write_item_page(
        &mut bytes,
        &ReadPage {
            records: vec![row(1)],
            has_more: true,
        },
        ResponseBudget::default(),
        |_| Err(ReadError::Internal),
    )
    .unwrap_err();
    assert!(matches!(error, EncodeError::Read(ReadError::Internal)));
    assert!(bytes.is_empty());
    struct FailingWriter;
    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let error = write_get(
        &mut FailingWriter,
        &ItemProjection::Work(work(1)),
        ResponseBudget::default(),
    )
    .unwrap_err();
    assert!(
        matches!(error, EncodeError::Write(error) if error.kind() == io::ErrorKind::BrokenPipe)
    );
    struct PartialWriter(Vec<u8>);
    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.0.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "closed mid-write",
                ));
            }
            self.0.extend_from_slice(&bytes[..32]);
            Ok(32)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut partial = PartialWriter(vec![]);
    let error = write_get(
        &mut partial,
        &ItemProjection::Work(work(1)),
        ResponseBudget::default(),
    )
    .unwrap_err();
    assert!(
        matches!(error, EncodeError::Write(error) if error.kind() == io::ErrorKind::BrokenPipe)
    );
    assert!(serde_json::from_slice::<Value>(&partial.0).is_err());
}
