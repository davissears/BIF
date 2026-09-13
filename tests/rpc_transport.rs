use std::collections::BTreeMap;

use bif::rpc::{Dispatcher, ErrorCode, MAXIMUM_INPUT_BYTES, Request, RpcError, serve};
use serde_json::{Value, json};

#[derive(Default)]
struct FixtureDispatcher;

impl Dispatcher for FixtureDispatcher {
    fn dispatch(&mut self, request: Request) -> Result<Value, RpcError> {
        if request.operation == bif::rpc::Operation::Get {
            let fields = request
                .params
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            if fields != ["item_id"] {
                return Err(RpcError::invalid_input());
            }
            return Ok(json!({"item": {"id": request.params["item_id"]}}));
        }
        Err(RpcError::new(
            ErrorCode::Internal,
            "Operation adapter is not installed",
            serde_json::Map::new(),
        ))
    }
}

fn invoke(input: &[u8]) -> (i32, Value, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = serve(input, &mut stdout, &mut stderr, &mut FixtureDispatcher).unwrap();
    assert_eq!(stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
    (
        code,
        serde_json::from_slice(&stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn consumes_rpc_fixture_and_matches_strict_examples() {
    let fixture: Value =
        serde_json::from_str(include_str!("../docs/fixtures/bif-v1-rpc.json")).unwrap();
    assert_eq!(
        fixture["protocol"]["transport"]["maximum_input_bytes"],
        MAXIMUM_INPUT_BYTES
    );

    for name in [
        "strict_unexpected_request_field",
        "strict_unexpected_params_field",
    ] {
        let example = &fixture["examples"][name];
        let input = serde_json::to_vec(&example["request"]).unwrap();
        let (code, response, _) = invoke(&input);
        assert_eq!(code, example["exit_code"]);
        assert_eq!(response["error"]["code"], example["error_code"]);
        assert_eq!(
            response["request_id"], example["request"]["request_id"],
            "{name}"
        );
    }

    let exit_codes = fixture["exit_codes"]["errors"].as_object().unwrap();
    let actual = BTreeMap::from([
        ("invalid_input", ErrorCode::InvalidInput.exit_code()),
        ("not_found", ErrorCode::NotFound.exit_code()),
        ("unauthorized", ErrorCode::Unauthorized.exit_code()),
        (
            "invalid_transition",
            ErrorCode::InvalidTransition.exit_code(),
        ),
        ("version_conflict", ErrorCode::VersionConflict.exit_code()),
        (
            "idempotency_conflict",
            ErrorCode::IdempotencyConflict.exit_code(),
        ),
        (
            "unsupported_version",
            ErrorCode::UnsupportedVersion.exit_code(),
        ),
        ("not_initialized", ErrorCode::NotInitialized.exit_code()),
        ("storage_busy", ErrorCode::StorageBusy.exit_code()),
        ("internal", ErrorCode::Internal.exit_code()),
    ]);
    for (name, code) in actual {
        assert_eq!(json!(code), exit_codes[name]);
    }
}

#[test]
fn accepts_one_value_with_json_whitespace() {
    let (code, response, stderr) = invoke(
        br#" 
 {"protocol_version":1,"request_id":"req-001","operation":"get","params":{"item_id":"DAVIS:delta-db:001"}}
 "#,
    );
    assert_eq!(code, 0);
    assert_eq!(response["request_id"], "req-001");
    assert_eq!(response["ok"], true);
    assert!(stderr.is_empty());
}

#[test]
fn malformed_framing_utf8_and_duplicates_have_null_request_id() {
    for input in [
        br#"{"protocol_version":1"#.as_slice(),
        br#"{"protocol_version":1,"request_id":"x","operation":"get","params":{}} trailing"#,
        br#"{"protocol_version":1,"request_id":"x","operation":"get","params":{"a":1,"a":2}}"#,
        &[0xff, 0xfe],
    ] {
        let (code, response, _) = invoke(input);
        assert_eq!(code, 2);
        assert_eq!(response["request_id"], Value::Null);
        assert_eq!(response["error"]["code"], "invalid_input");
    }
}

#[test]
fn envelope_errors_recover_only_a_valid_request_id() {
    let cases = [
        (
            br#"{"protocol_version":1,"request_id":"recover-me","operation":"wat","params":{}}"#
                .as_slice(),
            json!("recover-me"),
            2,
        ),
        (
            br#"{"protocol_version":1,"request_id":"","operation":"get","params":{}}"#,
            Value::Null,
            2,
        ),
        (
            br#"{"protocol_version":2,"request_id":"version","operation":"get","params":{}}"#,
            json!("version"),
            8,
        ),
    ];
    for (input, request_id, exit_code) in cases {
        let (code, response, _) = invoke(input);
        assert_eq!(code, exit_code);
        assert_eq!(response["request_id"], request_id);
    }
}

#[test]
fn rejects_oversized_input_without_parsing_request_id() {
    let input = vec![b' '; MAXIMUM_INPUT_BYTES + 1];
    let (code, response, _) = invoke(&input);
    assert_eq!(code, 2);
    assert_eq!(response["request_id"], Value::Null);
}
