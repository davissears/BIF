mod support;

use std::{collections::BTreeSet, fs, path::Path, process::Command};

use serde_json::Value;
use support::OwnedTestDirectory;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../docs/fixtures/bif-v2-workflow-evaluation.json"
    ))
    .expect("workflow fixture must be valid JSON")
}

fn contract() -> Value {
    serde_json::from_str(include_str!("../docs/fixtures/bif-v2-read-contract.json"))
        .expect("V2-001 contract fixture must be valid JSON")
}

fn frozen_request_fields(contract: &Value, operation: &str) -> Option<BTreeSet<String>> {
    contract["requests"]
        .get(format!("{operation}_fields"))?
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .map(|field| field.as_str().unwrap().to_owned())
                .collect()
        })
}

fn validate_frozen_request(
    contract: &Value,
    operation: &str,
    request: &Value,
) -> Result<(), String> {
    let legal = frozen_request_fields(contract, operation)
        .ok_or_else(|| format!("{operation} is not frozen by V2-001"))?;
    let actual = request
        .as_object()
        .ok_or_else(|| format!("{operation} request must be an object"))?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let illegal = actual.difference(&legal).cloned().collect::<Vec<_>>();
    if illegal.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{operation} request has unfrozen fields: {illegal:?}"
        ))
    }
}

fn assert_projection(contract: &Value, projection: &str, item: &Value) {
    let schema = &contract["projection_schemas"][projection];
    let fields = schema["fields"].as_array().unwrap();
    let expected = fields
        .iter()
        .map(|field| field.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let nullable = schema["nullable_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let object = item.as_object().expect("projection item must be an object");
    assert_eq!(
        object.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        expected
    );
    for (field, value) in object {
        assert!(
            !value.is_null() || nullable.contains(field.as_str()),
            "{projection}.{field} is not nullable"
        );
    }
    assert!(
        item["revision"]
            .as_u64()
            .is_some_and(|revision| revision > 0)
    );
    if projection == "work" {
        assert!(item["acceptance_criteria"].is_array());
    }
}

fn state(value: &Value, context: &str) -> String {
    value["state"]
        .as_str()
        .unwrap_or_else(|| panic!("{context} must have a labeled state"))
        .to_owned()
}

fn generate_store(root: &Path) {
    fs::create_dir(root.join(".bif")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bif-benchmark-store"))
        .args(["100", "--seed", "2003", "--output"])
        .arg(root.join(".bif/bif.sqlite"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(metadata["logical_digest"], "73701e6b7b09a5e1");
}

fn execute_argv(root: &Path, request: &str) -> String {
    let request: Value = serde_json::from_str(request).unwrap();
    let argv = request["argv"].as_array().expect("request argv");
    let output = Command::new(env!("CARGO_BIN_EXE_bif"))
        .args(argv.iter().map(|arg| arg.as_str().expect("string argv")))
        .args(["--root"])
        .arg(root)
        .args(["--requester", "BENCH"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "argv={argv:?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn dry_run_accounts_for_every_call_envelope_and_total() {
    let fixture = fixture();
    let components = [
        "tool_schema",
        "request_envelope",
        "response_envelope",
        "response_content",
        "follow_up",
    ];

    for workflow in fixture["workflows"].as_array().expect("workflows") {
        for variant in workflow["variants"].as_array().expect("variants") {
            let calls = variant["calls"].as_array().expect("calls");
            let mut variant_total = 0_u64;
            for (index, call) in calls.iter().enumerate() {
                assert_eq!(call["sequence"].as_u64(), Some(index as u64 + 1));
                let mut call_total = 0_u64;
                for component in components {
                    let content = call[component]
                        .as_str()
                        .unwrap_or_else(|| panic!("missing {component}"));
                    assert!(!content.is_empty(), "{component} may not be omitted");
                    let measured = call["proxy_utf8_bytes"][component]
                        .as_u64()
                        .unwrap_or_else(|| panic!("missing byte count for {component}"));
                    assert_eq!(
                        measured,
                        content.len() as u64,
                        "{} / {} / {component}",
                        workflow["id"],
                        variant["id"]
                    );
                    call_total += measured;
                }
                assert_eq!(call["total_proxy_utf8_bytes"].as_u64(), Some(call_total));
                variant_total += call_total;

                for token_kind in ["prompt", "completion", "cached", "uncached"] {
                    assert_eq!(
                        state(&call["tokens"][token_kind], token_kind),
                        "unknown",
                        "proxy bytes must never be presented as tokens"
                    );
                }
                assert_eq!(
                    state(&call["completion_time_ms"], "completion_time_ms"),
                    "not_measured"
                );
            }
            assert_eq!(
                variant["totals"]["proxy_utf8_bytes"].as_u64(),
                Some(variant_total)
            );
        }
    }
}

#[test]
fn workflows_have_distinct_baselines_and_valid_outcomes() {
    let fixture = fixture();
    let expected = [
        "planning",
        "select_and_execute",
        "triage",
        "refresh",
        "recovery",
        "agent_handoff",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let workflows = fixture["workflows"].as_array().expect("workflows");
    let actual = workflows
        .iter()
        .map(|workflow| workflow["id"].as_str().expect("workflow id"))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);

    for workflow in workflows {
        assert!(workflow["initial_state"].is_object());
        assert_eq!(
            workflow["reset_before_variant"]["method"],
            "regenerate_benchmark_store"
        );
        assert_eq!(
            workflow["reset_before_variant"]["logical_digest"],
            fixture["benchmark_store"]["logical_digest"]
        );
        assert!(
            workflow["stopping_condition"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        for assertion in workflow["success_assertions"]
            .as_array()
            .expect("success assertions")
        {
            assert!(
                assertion["path"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
            );
            assert!(
                assertion.get("equals").is_some(),
                "success outcomes must be independently checkable"
            );
        }
        assert!(
            !workflow["error_retry_specifications"]
                .as_array()
                .expect("error/retry specifications")
                .is_empty()
        );
        let variants = workflow["variants"].as_array().expect("variants");
        let ids = variants
            .iter()
            .map(|variant| variant["id"].as_str().expect("variant id"))
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("v1_full_json"));
        assert!(ids.contains("v1_compact_human"));
        assert_ne!(
            variants
                .iter()
                .find(|variant| variant["id"] == "v1_full_json")
                .unwrap()["calls"][0]["response_content"],
            variants
                .iter()
                .find(|variant| variant["id"] == "v1_compact_human")
                .unwrap()["calls"][0]["response_content"]
        );

        let baseline_operations = |id: &str| {
            variants.iter().find(|variant| variant["id"] == id).unwrap()["calls"]
                .as_array()
                .unwrap()
                .iter()
                .map(|call| call["operation"].as_str().unwrap())
                .collect::<Vec<_>>()
        };
        match workflow["id"].as_str().unwrap() {
            "select_and_execute" => {
                assert_eq!(
                    baseline_operations("v1_full_json"),
                    ["next_json", "start", "finish"]
                );
                assert_eq!(
                    baseline_operations("v1_compact_human"),
                    ["next_human", "start", "finish"]
                );
            }
            "triage" => {
                assert_eq!(baseline_operations("v1_full_json"), ["get_json", "triage"]);
                assert_eq!(
                    baseline_operations("v1_compact_human"),
                    ["get_human", "triage"]
                );
            }
            "recovery" => {
                assert_eq!(baseline_operations("v1_full_json").len(), 2);
                assert_eq!(baseline_operations("v1_compact_human").len(), 2);
            }
            "agent_handoff" => {
                assert_eq!(baseline_operations("v1_full_json"), ["get_json"]);
                assert_eq!(
                    baseline_operations("v1_compact_human"),
                    ["get_human", "get_json_for_handoff_fields"]
                );
            }
            _ => assert_eq!(baseline_operations("v1_full_json").len(), 1),
        }
    }
}

#[test]
fn error_retry_plans_are_complete_and_contain_no_fabricated_evidence() {
    let fixture = fixture();
    let contract = contract();
    let frozen_codes = contract["closed_enums"]["read_error_code"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    for workflow in fixture["workflows"].as_array().unwrap() {
        for specification in workflow["error_retry_specifications"].as_array().unwrap() {
            for field in [
                "trigger_precondition",
                "operation",
                "request_shape",
                "expected_error",
                "retryability",
                "retry_or_termination_policy",
                "expected_subsequent_request_behavior",
            ] {
                assert!(
                    !specification[field].is_null(),
                    "{} lacks {field}",
                    workflow["id"]
                );
            }
            assert!(
                frozen_codes.contains(specification["expected_error"]["code"].as_str().unwrap())
            );
            validate_frozen_request(
                &contract,
                specification["operation"].as_str().unwrap(),
                &specification["request_shape"],
            )
            .unwrap();
            assert!(
                !specification["expected_error"]["assertions"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(specification["execution_state"], "not_executed");
            assert_eq!(specification["attempts"], Value::Array(vec![]));
            assert_eq!(specification["evidence"], Value::Array(vec![]));
        }
    }
}

#[test]
fn v2_requests_preserve_declared_scope_filters_and_bounds() {
    let fixture = fixture();
    let contract = contract();
    let expected = [
        (
            "planning",
            serde_json::json!({
                "view": "ready",
                "project": "core",
                "projection": "summary",
                "limit": 2
            }),
        ),
        (
            "select_and_execute",
            serde_json::json!({
                "project": "agent-tools",
                "projection": "work",
                "limit": 1
            }),
        ),
        (
            "refresh",
            serde_json::json!({
                "view": "active",
                "project": "core",
                "projection": "work",
                "limit": 1,
                "cursor": "cursor-page-1"
            }),
        ),
        (
            "agent_handoff",
            serde_json::json!({
                "item_id": "CODEX:agent-tools:003",
                "projection": "work"
            }),
        ),
    ];

    for (workflow_id, expected_request) in expected {
        let workflow = fixture["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|workflow| workflow["id"] == workflow_id)
            .unwrap();
        let call = &workflow["variants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|variant| variant["id"] == "v2_contract")
            .unwrap()["calls"][0];
        let operation = match call["operation"].as_str().unwrap() {
            "list_summary" | "list_active_cursor" => "list",
            "next_work" => "next",
            "get_work" => "get",
            operation => panic!("unfrozen workflow operation {operation}"),
        };
        let request: Value =
            serde_json::from_str(call["request_envelope"].as_str().unwrap()).unwrap();
        validate_frozen_request(&contract, operation, &request).unwrap();
        assert_eq!(request, expected_request, "{workflow_id}");

        for key in ["project", "view", "limit"] {
            if let Some(declared) = workflow["initial_state"].get(key) {
                assert_eq!(request.get(key), Some(declared), "{workflow_id}.{key}");
            }
        }
    }

    let refresh = fixture["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workflow| workflow["id"] == "refresh")
        .unwrap();
    let continuation: Value = serde_json::from_str(
        refresh["variants"][2]["calls"][0]["request_envelope"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let invalid_cursor = &refresh["error_retry_specifications"][0]["request_shape"];
    for key in ["view", "project", "projection", "limit"] {
        assert_eq!(
            continuation.get(key),
            invalid_cursor.get(key),
            "cursor continuation changed {key}"
        );
    }
}

#[test]
fn semantic_validator_rejects_unfrozen_operations_and_fields() {
    let contract = contract();
    assert!(
        validate_frozen_request(
            &contract,
            "start",
            &serde_json::json!({"item_id": "BENCH:agent-tools:001"})
        )
        .unwrap_err()
        .contains("not frozen")
    );
    assert!(
        validate_frozen_request(
            &contract,
            "list",
            &serde_json::json!({"view": "ready", "unexpected": true})
        )
        .unwrap_err()
        .contains("unexpected")
    );
    assert!(
        validate_frozen_request(
            &contract,
            "next",
            &serde_json::json!({"project": "agent-tools", "idempotency_key": "not-legal"})
        )
        .unwrap_err()
        .contains("idempotency_key")
    );
}

#[test]
fn handoff_fields_are_derived_from_each_variants_reads() {
    let workflow = fixture()["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workflow| workflow["id"] == "agent_handoff")
        .unwrap()
        .clone();
    for variant in workflow["variants"].as_array().unwrap() {
        let calls = variant["calls"].as_array().unwrap();
        let source = calls
            .iter()
            .rev()
            .find_map(|call| {
                serde_json::from_str::<Value>(call["response_content"].as_str().unwrap())
                    .ok()
                    .and_then(|response| response.get("item").cloned().or(Some(response)))
                    .filter(|item| {
                        item.get("revision").is_some() && item.get("acceptance_criteria").is_some()
                    })
            })
            .expect("handoff variant must read revision and acceptance criteria");
        let handoff: Value =
            serde_json::from_str(calls.last().unwrap()["follow_up"].as_str().unwrap()).unwrap();
        assert_eq!(handoff["handoff"]["item_id"], source["id"]);
        assert_eq!(handoff["handoff"]["revision"], source["revision"]);
        let next_action = source["acceptance_criteria"][0]
            .as_str()
            .unwrap()
            .to_lowercase()
            .replacen("criterion 1 verifies", "verify", 1);
        assert_eq!(handoff["handoff"]["next_action"], next_action);
    }
}

#[test]
fn stopping_conditions_match_asserted_terminal_revisions() {
    let fixture = fixture();
    for (id, revision) in [("select_and_execute", 6), ("triage", 4)] {
        let workflow = fixture["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|workflow| workflow["id"] == id)
            .unwrap();
        assert!(
            workflow["stopping_condition"]
                .as_str()
                .unwrap()
                .contains(&format!("revision {revision}"))
        );
        assert!(
            workflow["success_assertions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|assertion| assertion["path"] == "item.revision"
                    && assertion["equals"] == revision)
        );
    }
}

#[test]
fn guarded_mutations_derive_revisions_and_use_fresh_variant_keys() {
    let fixture = fixture();
    let mut keys = BTreeSet::new();
    for workflow in fixture["workflows"].as_array().unwrap() {
        for variant in workflow["variants"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|variant| variant["baseline"] == true)
        {
            let variant_id = variant["id"].as_str().unwrap();
            let mut known_revision = workflow["initial_state"]["revision"].as_u64();
            for call in variant["calls"].as_array().unwrap() {
                let request: Value =
                    serde_json::from_str(call["request_envelope"].as_str().unwrap()).unwrap();
                let argv = request["argv"].as_array().unwrap();
                if let Some(position) = argv.iter().position(|arg| arg == "--expected-revision") {
                    assert_eq!(
                        argv[position + 1].as_str().unwrap().parse::<u64>().unwrap(),
                        known_revision.unwrap()
                    );
                    let key_position = argv
                        .iter()
                        .position(|arg| arg == "--idempotency-key")
                        .unwrap();
                    let key = argv[key_position + 1].as_str().unwrap();
                    assert!(key.contains(variant_id), "key must identify its variant");
                    assert!(
                        keys.insert(key.to_owned()),
                        "idempotency keys must be fresh"
                    );
                }
                if let Some(revision) = call["response_content"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .find_map(|line| line.strip_prefix("revision: "))
                    .and_then(|revision| revision.parse().ok())
                {
                    known_revision = Some(revision);
                }
            }
        }
    }
}

#[test]
fn future_variants_do_not_overstate_the_frozen_contract_or_measurements() {
    let fixture = fixture();
    let contract = contract();
    for key in [
        "host",
        "model",
        "tokenizer",
        "tokenizer_version_config",
        "cache_accounting_semantics",
    ] {
        assert_eq!(state(&fixture["environment"][key], key), "unknown");
    }
    assert_eq!(
        fixture["environment"]["measurement_provenance"]["value"],
        "local_fixture_validation"
    );

    for workflow in fixture["workflows"].as_array().unwrap() {
        for variant in workflow["variants"].as_array().unwrap() {
            if variant["id"] == "v2_contract" {
                assert!(
                    variant["implementation"]
                        .as_str()
                        .is_some_and(|value| value.contains("not_implemented"))
                );
                for call in variant["calls"].as_array().unwrap() {
                    assert!(matches!(
                        call["operation"].as_str().unwrap(),
                        "list_summary" | "next_work" | "list_active_cursor" | "get_work"
                    ));
                    let projection = if call["operation"] == "list_summary" {
                        "summary"
                    } else {
                        "work"
                    };
                    let response: Value =
                        serde_json::from_str(call["response_content"].as_str().unwrap()).unwrap();
                    if let Some(items) = response["items"].as_array() {
                        for item in items {
                            assert_projection(&contract, projection, item);
                        }
                    } else {
                        assert_projection(&contract, projection, &response["item"]);
                    }
                }
            }
            assert!(matches!(
                state(&variant["outcome"], "outcome").as_str(),
                "unknown" | "not_measured" | "measured"
            ));
        }
    }
}

#[test]
fn full_json_read_responses_are_complete_existing_v1_shapes() {
    let fixture = fixture();
    assert_eq!(fixture["benchmark_store"]["size"], 100);
    assert_eq!(fixture["benchmark_store"]["seed"], 2003);
    assert_eq!(
        fixture["benchmark_store"]["logical_digest"],
        "73701e6b7b09a5e1"
    );
    for workflow in fixture["workflows"].as_array().unwrap() {
        let variant = workflow["variants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|variant| variant["id"] == "v1_full_json")
            .unwrap();
        for call in variant["calls"].as_array().unwrap() {
            match call["operation"].as_str().unwrap() {
                "get_json" => {
                    let parsed: Value =
                        serde_json::from_str(call["response_content"].as_str().unwrap()).unwrap();
                    for key in [
                        "id",
                        "requester",
                        "project",
                        "sequence",
                        "title",
                        "status",
                        "revision",
                        "acceptance_criteria",
                        "provenance",
                    ] {
                        assert!(parsed.get(key).is_some(), "get response missing {key}");
                    }
                }
                operation if operation.contains("list") || operation == "next_json" => {
                    let parsed: Value =
                        serde_json::from_str(call["response_content"].as_str().unwrap()).unwrap();
                    assert!(parsed["items"].is_array());
                    assert!(parsed.get("next_offset").is_some());
                }
                _ => {}
            }
        }
    }
}

#[test]
fn all_v1_calls_replay_and_follow_ups_match_the_next_call() {
    let fixture = fixture();
    for workflow in fixture["workflows"].as_array().unwrap() {
        for variant in workflow["variants"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|variant| variant["baseline"] == true)
        {
            let directory = OwnedTestDirectory::new();
            generate_store(directory.path());
            let calls = variant["calls"].as_array().unwrap();
            for (index, call) in calls.iter().enumerate() {
                let request = call["request_envelope"].as_str().unwrap();
                let request_value: Value = serde_json::from_str(request).unwrap();
                let argv = request_value["argv"].as_array().unwrap();
                let is_mutation = argv.iter().any(|arg| {
                    matches!(
                        arg.as_str(),
                        Some(
                            "approve"
                                | "reject"
                                | "prioritize"
                                | "assign"
                                | "start"
                                | "block"
                                | "resume"
                                | "finish"
                                | "triage"
                        )
                    )
                });
                if is_mutation {
                    assert!(request.contains("--expected-revision"));
                    assert!(request.contains("--idempotency-key"));
                }
                assert_eq!(
                    execute_argv(directory.path(), request),
                    call["response_content"],
                    "{} / {} / {}",
                    workflow["id"],
                    variant["id"],
                    call["operation"]
                );

                if let Some(next) = calls.get(index + 1) {
                    let next_request: Value =
                        serde_json::from_str(next["request_envelope"].as_str().unwrap()).unwrap();
                    let expected = format!(
                        "bif {}",
                        next_request["argv"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|arg| arg.as_str().unwrap())
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    assert_eq!(call["follow_up"], expected);
                }
            }
        }
    }
}

#[test]
fn schema_closes_and_constrains_the_normative_fixture_sections() {
    let schema: Value = serde_json::from_str(include_str!(
        "../docs/fixtures/bif-v2-workflow-evaluation.schema.json"
    ))
    .unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["benchmark_store"]["additionalProperties"],
        false
    );
    for definition in ["state", "bytes", "tokens", "attempt", "workflow"] {
        assert_eq!(
            schema["$defs"][definition]["additionalProperties"], false,
            "{definition} must reject fixture drift"
        );
    }
    assert_eq!(
        schema["$defs"]["attempt"]["properties"]["total_proxy_utf8_bytes"]["type"],
        "integer"
    );
    let compiled = jsonschema::validator_for(&schema).expect("workflow JSON Schema must compile");
    let fixture = fixture();
    if let Err(error) = compiled.validate(&fixture) {
        panic!("workflow fixture violates its JSON Schema: {error}");
    }
}
