use bif::mcp::{MAXIMUM_WIRE_BYTES, TOOL_RESPONSE_BUDGET_BYTES, decode_tool, tool_catalog};
use serde_json::{Value, json};

#[test]
fn discovery_is_small_read_only_and_has_strict_schemas() {
    let catalog = tool_catalog();
    let tools = catalog.as_array().unwrap();
    assert_eq!(tools.len(), 4);
    for tool in tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
        assert_eq!(tool["annotations"]["openWorldHint"], false);
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(
            tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("project"))
        );
        assert!(tool.get("outputSchema").is_none());
    }
    let catalog_bytes = serde_json::to_vec(&catalog).unwrap().len();
    let schema_bytes: usize = tools
        .iter()
        .map(|tool| serde_json::to_vec(&tool["inputSchema"]).unwrap().len())
        .sum();
    println!("tool catalog: {catalog_bytes} bytes; four input schemas: {schema_bytes} bytes");
    assert!(catalog_bytes < 8_000);
    assert!(TOOL_RESPONSE_BUDGET_BYTES * 7 + 16_384 < MAXIMUM_WIRE_BYTES);
}

#[test]
fn tool_arguments_are_typed_scoped_and_cannot_choose_execution() {
    for name in ["bif_list", "bif_get", "bif_history", "bif_selected_work"] {
        assert!(decode_tool(name, json!({})).is_err());
        assert!(
            decode_tool(
                name,
                json!({"project":"widgets","execution":{"kind":"direct"}})
            )
            .is_err()
        );
    }
    for arguments in [
        json!({"project":"widgets","limit":0}),
        json!({"project":"widgets","limit":101}),
        json!({"project":"widgets","limit":1.5}),
        json!({"project":"widgets","limit":"10"}),
        json!({"project":"widgets","view":"sql"}),
        json!({"project":"widgets","projection":"full"}),
        json!({"project":"widgets","unassigned":true,"assignee":"DAVIS"}),
        json!({"project":"widgets","status":"active"}),
        json!({"project":"widgets","priority":"P5"}),
        json!({"project":"widgets","text":""}),
    ] {
        assert!(decode_tool("bif_list", arguments).is_err());
    }
    for name in ["bif_get", "bif_history"] {
        assert!(
            decode_tool(
                name,
                json!({"project":"other","item_id":"DAVIS:widgets:001"})
            )
            .is_err()
        );
        assert!(
            decode_tool(
                name,
                json!({"project":"widgets","item_id":"DAVIS:widgets:001"})
            )
            .is_ok()
        );
    }
    assert!(
        decode_tool(
            "bif_list",
            json!({"project":"widgets","view":"ready","projection":"work",
        "requester":"DAVIS","status":"ready","priority":"P1","text":"fix","limit":7})
        )
        .is_ok()
    );
}

#[test]
fn discovery_schemas_match_accepted_argument_examples() {
    let catalog = tool_catalog();
    let examples: [(&str, Value); 4] = [
        ("bif_list", json!({"project":"widgets"})),
        (
            "bif_get",
            json!({"project":"widgets","item_id":"DAVIS:widgets:001","known_version":"v1.token"}),
        ),
        (
            "bif_history",
            json!({"project":"widgets","item_id":"DAVIS:widgets:001","limit":10}),
        ),
        ("bif_selected_work", json!({"project":"widgets"})),
    ];
    for (name, arguments) in examples {
        let tool = catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        let validator = jsonschema::validator_for(&tool["inputSchema"]).unwrap();
        assert!(validator.is_valid(&arguments));
        assert!(decode_tool(name, arguments).is_ok());
        assert!(!validator.is_valid(&json!({"project":"widgets","limit":101,"actor":"evil"})));
        if name == "bif_list" {
            assert!(
                !validator
                    .is_valid(&json!({"project":"widgets","unassigned":true,"assignee":"DAVIS"}))
            );
        }
        if name == "bif_get" || name == "bif_history" {
            assert!(
                !validator.is_valid(&json!({"project":"widgets","item_id":"DAVIS:widgets:000"}))
            );
        }
    }
}
