use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

const V1_FIXTURES: [(&str, &str); 4] = [
    (
        "bif-v1-authorization.json",
        include_str!("../docs/fixtures/bif-v1-authorization.json"),
    ),
    (
        "bif-v1-canonical.json",
        include_str!("../docs/fixtures/bif-v1-canonical.json"),
    ),
    (
        "bif-v1-lifecycle.json",
        include_str!("../docs/fixtures/bif-v1-lifecycle.json"),
    ),
    (
        "bif-v1-rpc.json",
        include_str!("../docs/fixtures/bif-v1-rpc.json"),
    ),
];

fn fixture() -> Value {
    serde_json::from_str(include_str!("../docs/fixtures/bif-v2-read-contract.json"))
        .expect("v2 contract fixture must be valid JSON")
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|value| value.as_str().expect("string"))
        .collect()
}

fn assert_exact_fields(object: &Map<String, Value>, expected: &[&str], context: &str) {
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "{context} fields");
}

fn assert_normative_history_results(
    value: &Value,
    expected_fields: &[&str],
    context: &str,
) -> usize {
    match value {
        Value::Object(object) => {
            let is_history_result =
                object.contains_key("events") && object.contains_key("next_cursor");
            let mut count = usize::from(is_history_result);

            if is_history_result {
                assert_exact_fields(object, expected_fields, context);
                assert!(
                    value["item_id"]
                        .as_str()
                        .is_some_and(|item_id| !item_id.is_empty()),
                    "{context}.item_id must be a nonempty string"
                );
                assert!(
                    value["events"].is_array(),
                    "{context}.events must be an array, including when empty"
                );
                assert!(
                    value["next_cursor"].is_null() || value["next_cursor"].is_string(),
                    "{context}.next_cursor must be a string or null"
                );
            }

            for (field, child) in object {
                count += assert_normative_history_results(
                    child,
                    expected_fields,
                    &format!("{context}.{field}"),
                );
            }
            count
        }
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, child)| {
                assert_normative_history_results(
                    child,
                    expected_fields,
                    &format!("{context}[{index}]"),
                )
            })
            .sum(),
        _ => 0,
    }
}

fn assert_projection(fixture: &Value, projection: &str, item: &Value, context: &str) {
    let schema = &fixture["projection_schemas"][projection];
    let fields = strings(&schema["fields"]);
    let nullable = strings(&schema["nullable_fields"])
        .into_iter()
        .collect::<BTreeSet<_>>();
    let object = item
        .as_object()
        .unwrap_or_else(|| panic!("{context} must be an object"));
    assert_exact_fields(object, &fields, context);

    for field in fields {
        assert!(
            !item[field].is_null() || nullable.contains(field),
            "{context}.{field} is not nullable"
        );
    }
    assert!(item["revision"].as_u64().is_some_and(|value| value > 0));
    assert!(
        fixture["closed_enums"]["status"]
            .as_array()
            .expect("status enum")
            .contains(&item["status"]),
        "{context}.status must be a closed-enum value"
    );
    if !item["priority"].is_null() {
        assert!(
            fixture["closed_enums"]["priority"]
                .as_array()
                .expect("priority enum")
                .contains(&item["priority"]),
            "{context}.priority must be a closed-enum value"
        );
    }
    if projection != "summary" {
        assert!(item["acceptance_criteria"].as_array().is_some());
    }
    if projection == "audit" {
        assert!(item["sequence"].as_u64().is_some_and(|value| value > 0));
        let provenance = item["provenance"].as_object().expect("provenance object");
        let provenance_fields = strings(&schema["provenance_fields"]);
        assert_exact_fields(provenance, &provenance_fields, context);
        if !item["provenance"]["source_host"].is_null() {
            assert!(
                fixture["closed_enums"]["source_host"]
                    .as_array()
                    .expect("source_host enum")
                    .contains(&item["provenance"]["source_host"])
            );
        }
    }
}

#[test]
fn freezes_v1_fixture_names_without_changing_their_versions() {
    let fixture = fixture();
    let frozen = strings(&fixture["v1_compatibility"]["fixtures_unchanged"]);
    assert_eq!(
        frozen,
        V1_FIXTURES.map(|(name, _)| name).as_slice(),
        "the v2 fixture must enumerate every preserved v1 fixture"
    );

    for (name, source) in V1_FIXTURES {
        let value: Value =
            serde_json::from_str(source).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(value["fixture_version"], 1, "{name}");
    }
    assert_eq!(fixture["v1_compatibility"]["default_cli_version"], 1);
    assert_eq!(fixture["v1_compatibility"]["rpc_protocol_version"], 1);
}

#[test]
fn projection_fields_and_closed_enums_are_exact() {
    let fixture = fixture();
    assert_eq!(
        strings(&fixture["projection_schemas"]["summary"]["fields"]),
        ["id", "title", "status", "priority", "assignee", "revision"]
    );
    assert_eq!(
        strings(&fixture["projection_schemas"]["work"]["fields"]),
        [
            "id",
            "title",
            "status",
            "priority",
            "assignee",
            "revision",
            "description",
            "acceptance_criteria",
            "status_reason"
        ]
    );
    assert_eq!(
        strings(&fixture["closed_enums"]["status"]),
        [
            "proposed",
            "ready",
            "in_progress",
            "blocked",
            "done",
            "rejected"
        ]
    );
    assert_eq!(
        strings(&fixture["closed_enums"]["priority"]),
        ["P0", "P1", "P2", "P3", "P4"]
    );
    assert_eq!(
        strings(&fixture["closed_enums"]["projection"]),
        ["summary", "work", "audit"]
    );

    for schema in ["summary", "work", "audit"] {
        let fields = strings(&fixture["projection_schemas"][schema]["fields"]);
        assert_eq!(
            fields.len(),
            fields.iter().collect::<BTreeSet<_>>().len(),
            "{schema} fields must be unique"
        );
    }
}

#[test]
fn covers_required_boundaries_and_rejects_offset_cursor_combinations() {
    let fixture = fixture();
    let cases = &fixture["coverage_cases"];
    for name in [
        "nulls_and_empty_arrays",
        "p4",
        "large_numeric_id",
        "empty_results",
        "unknown_field",
        "oversized_record",
    ] {
        assert!(cases.get(name).is_some(), "missing coverage case {name}");
    }

    for name in ["nulls_and_empty_arrays", "p4", "large_numeric_id"] {
        let case = &cases[name];
        assert_projection(
            &fixture,
            case["projection"].as_str().expect("projection"),
            &case["item"],
            name,
        );
    }
    assert_eq!(cases["p4"]["item"]["priority"], "P4");
    assert_eq!(
        cases["large_numeric_id"]["item"]["sequence"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    assert_eq!(
        cases["empty_results"]["list"]["items"],
        Value::Array(vec![])
    );
    assert_eq!(
        cases["nulls_and_empty_arrays"]["item"]["provenance"]["source_host"],
        Value::Null
    );
    assert_eq!(
        cases["empty_results"]["list"],
        fixture["results"]["empty_page"]
    );
    assert_eq!(cases["empty_results"]["history"]["events"], json!([]));
    assert_eq!(
        cases["empty_results"]["history"]["next_cursor"],
        Value::Null
    );

    assert_eq!(cases["unknown_field"]["operation"], "list");
    let unknown_request = cases["unknown_field"]["request"]
        .as_object()
        .expect("unknown-field request");
    let allowed_list_fields = strings(&fixture["requests"]["list_fields"])
        .into_iter()
        .collect::<BTreeSet<_>>();
    let unknown_fields = unknown_request
        .keys()
        .filter(|field| !allowed_list_fields.contains(field.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(unknown_fields, ["unexpected"]);
    assert_eq!(cases["unknown_field"]["error"], "invalid_input");
    assert_eq!(cases["unknown_field"]["storage_accessed"], false);

    let oversized = &cases["oversized_record"];
    assert_eq!(oversized["operation"], "get");
    assert_exact_fields(
        oversized["request"].as_object().expect("oversized request"),
        &strings(&fixture["requests"]["get_fields"]),
        "oversized request",
    );
    assert_eq!(oversized["request"]["projection"], oversized["projection"]);
    let construction = &oversized["response_construction"];
    assert_eq!(construction["encoding"], "serde_json_compact_utf8");
    assert_eq!(construction["envelope"], "v2_success_get");
    assert_eq!(construction["repeat_field"], "result.item.description");
    assert_eq!(construction["repeat_value"], "x");
    assert_projection(
        &fixture,
        oversized["projection"]
            .as_str()
            .expect("oversized projection"),
        &construction["item"],
        "oversized construction item",
    );
    let repeat_count = construction["repeat_count"].as_u64().expect("repeat count") as usize;
    let mut item = construction["item"].clone();
    item["description"] = Value::String("x".repeat(repeat_count));
    let response = json!({
        "api_version": fixture["results"]["api_version"],
        "schema_version": fixture["results"]["schema_version"],
        "ok": fixture["results"]["success_ok"],
        "result": {"item": item}
    });
    let measured_bytes = serde_json::to_vec(&response)
        .expect("compact JSON serialization")
        .len();
    let maximum = fixture["payload_boundaries"]["maximum_response_bytes"]
        .as_u64()
        .expect("maximum response bytes") as usize;
    assert!(measured_bytes > maximum);

    let mut empty_item = construction["item"].clone();
    empty_item["description"] = Value::String(String::new());
    let fixed_bytes = serde_json::to_vec(&json!({
        "api_version": fixture["results"]["api_version"],
        "schema_version": fixture["results"]["schema_version"],
        "ok": fixture["results"]["success_ok"],
        "result": {"item": empty_item}
    }))
    .expect("compact JSON serialization")
    .len();
    assert!(fixed_bytes < maximum);
    for (description_bytes, expected_size) in [
        (maximum - fixed_bytes, maximum),
        (maximum - fixed_bytes + 1, maximum + 1),
    ] {
        let mut boundary_item = construction["item"].clone();
        boundary_item["description"] = Value::String("x".repeat(description_bytes));
        let encoded = serde_json::to_vec(&json!({
            "api_version": fixture["results"]["api_version"],
            "schema_version": fixture["results"]["schema_version"],
            "ok": fixture["results"]["success_ok"],
            "result": {"item": boundary_item}
        }))
        .expect("compact JSON serialization");
        assert_eq!(encoded.len(), expected_size);
    }

    let expected_error = &oversized["expected_error"];
    assert_eq!(expected_error["code"], "payload_too_large");
    assert!(
        expected_error["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(
        expected_error["details"]["minimum_required_bytes"]
            .as_u64()
            .expect("minimum required bytes") as usize,
        measured_bytes
    );
    let generated_details = json!({
        "record_kind": expected_error["details"]["record_kind"],
        "record_id": expected_error["details"]["record_id"],
        "maximum_response_bytes": maximum,
        "minimum_required_bytes": measured_bytes
    });
    assert_exact_fields(
        generated_details.as_object().expect("generated details"),
        &strings(&fixture["stable_errors"]["details_rules"]["payload_too_large"]),
        "payload-too-large details",
    );
    assert_eq!(generated_details, expected_error["details"]);
    assert_eq!(oversized["cursor_advanced"], false);
    assert_eq!(oversized["partial_success_written"], false);

    let actual_invalid = fixture["cli"]["invalid_combinations"]
        .as_array()
        .expect("invalid combinations")
        .iter()
        .map(|case| {
            assert_eq!(case["error"], "invalid_input");
            (
                case["operation"].as_str().expect("operation").to_owned(),
                strings(&case["options"])
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_invalid = [
        ("get", vec!["--cursor", "opaque"]),
        ("get", vec!["--limit", "1"]),
        ("get", vec!["--offset", "1"]),
        ("list", vec!["--offset", "1"]),
        ("list", vec!["--offset", "1", "--cursor", "opaque"]),
        ("next", vec!["--offset", "1"]),
        ("next", vec!["--offset", "1", "--cursor", "opaque"]),
        ("history", vec!["--offset", "1"]),
        ("history", vec!["--offset", "1", "--cursor", "opaque"]),
        ("history", vec!["--projection", "audit"]),
    ]
    .into_iter()
    .map(|(operation, options)| {
        (
            operation.to_owned(),
            options.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        )
    })
    .collect::<BTreeSet<_>>();
    assert_eq!(actual_invalid, expected_invalid);
}

#[test]
fn every_normative_history_result_matches_the_declared_schema() {
    let fixture = fixture();
    let result_fields = strings(&fixture["history_schema"]["result_fields"]);
    let result_count = assert_normative_history_results(&fixture, &result_fields, "fixture");

    assert!(
        result_count > 0,
        "fixture must contain a normative history result"
    );
    assert_eq!(
        fixture["history_schema"]["existing_item_without_events"]["item_id"],
        fixture["coverage_cases"]["empty_results"]["history"]["item_id"],
        "empty-history examples must use the intended existing item identity"
    );
    assert_eq!(
        fixture["history_schema"]["existing_item_without_events"]["events"],
        json!([]),
        "an existing item without events has a present, empty events array"
    );
    assert_eq!(
        fixture["history_schema"]["existing_item_without_events"]["next_cursor"],
        Value::Null,
        "an empty history page has a null continuation cursor"
    );
}

#[test]
fn freezes_ordering_history_and_payload_rules() {
    let fixture = fixture();
    assert_eq!(
        strings(&fixture["requests"]["get_fields"]),
        ["item_id", "projection"]
    );
    assert_eq!(
        strings(&fixture["results"]["success_envelope_fields"]),
        ["api_version", "schema_version", "ok", "result"]
    );
    assert_eq!(fixture["results"]["api_version"], 2);
    assert_eq!(fixture["results"]["schema_version"], 1);
    assert_eq!(
        fixture["ordering"]["large_id_case"]["ordered_ids"],
        serde_json::json!([
            "DAVIS:bif:999",
            "DAVIS:bif:1000",
            "DAVIS:bif:9007199254740993"
        ])
    );
    assert_eq!(fixture["pagination"]["model"], "live_keyset");
    assert_eq!(
        fixture["history_schema"]["ordering"][0]["field"],
        "item_revision"
    );
    assert_eq!(
        fixture["history_schema"]["ordering"][1]["field"],
        "event_index"
    );
    assert_eq!(
        fixture["payload_boundaries"]["maximum_response_bytes"],
        1_048_576
    );
    assert_eq!(
        fixture["payload_boundaries"]["single_oversized_record"]["cursor_advanced"],
        false
    );
    assert_eq!(
        fixture["strict_input"]["unknown_fields"],
        "reject_with_invalid_input"
    );
    assert_eq!(fixture["stable_errors"]["exit_codes"]["invalid_cursor"], 2);
    assert_eq!(
        fixture["stable_errors"]["exit_codes"]["payload_too_large"],
        11
    );
}
