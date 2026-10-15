//! Recorded executable evidence is separate from the Phase D approval.
use std::{fs, process::Command};

use serde_json::{Value, json};

fn proof() -> Value {
    serde_json::from_slice(&fs::read("docs/baselines/v2-phase-e/upgrade-proof.json").unwrap())
        .unwrap()
}

fn validate(value: &Value) {
    assert_eq!(value["format"], "bif-phase-e-upgrade-proof-v1");
    assert_eq!(
        value["historical"]["revision"],
        "d4399a93e49ce8f87e39e3129377821858472807"
    );
    assert_eq!(value["historical"]["source_method"], "git archive");
    assert_eq!(value["historical"]["source_modified"], false);
    assert_eq!(value["historical"]["build"]["exit_code"], 0);
    assert_eq!(value["historical"]["build"]["locked"], true);
    assert_eq!(value["historical"]["build"]["separate_target"], true);
    for source in ["historical", "candidate"] {
        for field in [
            "binary_sha256",
            "source_sha256",
            "lock_sha256",
            "toolchain_sha256",
        ] {
            let hash = value[source][field].as_str().unwrap();
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
        assert!(!value[source]["rustc"].as_str().unwrap().is_empty());
        assert!(!value[source]["cargo"].as_str().unwrap().is_empty());
        assert_eq!(
            value[source]["binary_sha256"],
            value[source]["binary_end_sha256"]
        );
        assert_eq!(
            value[source]["source_sha256"],
            value[source]["source_end_sha256"]
        );
    }
    assert_eq!(value["process_mode"]["writer"], "one-shot CLI");
    assert_eq!(
        value["process_mode"]["warm_mcp"],
        "read-only; not exercised"
    );
    assert_eq!(value["process_mode"]["warm_writer_fencing_proven"], false);
    assert_eq!(value["baseline"]["migrations"], json!([1, 2, 3]));
    assert_eq!(value["upgraded"]["migrations"], json!([1, 2, 3, 4]));
    assert_eq!(value["legacy_rows_preserved"], true);
    for (table, rows) in value["baseline"]["tables"].as_object().unwrap() {
        if table == "schema_migrations" {
            let upgraded = value["upgraded"]["tables"][table].as_array().unwrap();
            assert_eq!(&upgraded[..upgraded.len() - 1], rows.as_array().unwrap());
        } else {
            assert_eq!(&value["upgraded"]["tables"][table], rows, "{table}");
        }
    }
    assert_eq!(value["journal"]["sync_enabled"], 0);
    assert_eq!(value["journal"]["bootstrap_required"], 1);
    assert_eq!(value["journal"]["high_water"], 2);
    let events = value["events"].as_array().unwrap();
    let names: Vec<_> = events
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    let position = |name| names.iter().position(|entry| *entry == name).unwrap();
    assert!(position("old_writer_stopped") < position("maintenance_enabled"));
    assert!(position("maintenance_enabled") < position("old_writer_continued"));
    assert!(position("old_writer_reaped") < position("candidate_migration_started"));
    assert!(position("candidate_migration_started") < position("candidate_migration_finished"));
    assert!(position("candidate_migration_finished") < position("maintenance_disabled"));
    assert_eq!(events[position("old_writer_reaped")]["exit_code"], 0);
    assert!(
        events[position("old_writer_stopped")]["pid"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        events[position("old_writer_stopped")]["pid"],
        events[position("old_writer_reaped")]["pid"]
    );
    let blocked = value["blocked_restarts"].as_array().unwrap();
    assert_eq!(blocked.len(), 5);
    for attempt in blocked {
        assert_eq!(attempt["process_spawned"], false);
        assert_eq!(attempt["sentinel_present"], true);
    }
    assert_eq!(value["rejections"].as_array().unwrap().len(), 3);
    for rejection in value["rejections"].as_array().unwrap() {
        assert_ne!(rejection["exit_code"], 0);
        assert!(rejection["stderr"].as_str().unwrap().contains("newer"));
        assert_eq!(rejection["before"], rejection["after"]);
        assert_eq!(
            rejection["before"]["schema_sha256"],
            value["final_state"]["schema_sha256"]
        );
        assert_eq!(
            rejection["before"]["content_sha256"],
            value["final_state"]["content_sha256"]
        );
    }
    assert_eq!(value["real_store_touched"], false);
    assert_eq!(value["operator_inventory_replaced"], false);
    assert_eq!(value["release_approved"], false);
    assert!(value["limitations"].as_array().unwrap().len() >= 4);
}

#[test]
fn authentic_historical_upgrade_proof_is_complete_and_scoped() {
    validate(&proof());
}

#[test]
fn proof_cannot_claim_warm_writer_fencing_or_skip_reap_and_content_checks() {
    let value = proof();
    for (pointer, replacement) in [
        ("/historical/source_modified", json!(true)),
        ("/historical/build/locked", json!(false)),
        ("/process_mode/warm_writer_fencing_proven", json!(true)),
        ("/rejections/0/after/content_sha256", json!("changed")),
        ("/journal/sync_enabled", json!(1)),
        ("/events", json!([])),
    ] {
        let mut altered = value.clone();
        *altered.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            std::panic::catch_unwind(|| validate(&altered)).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn rehearsal_control_tests_run_without_building_or_simulating_an_old_client() {
    let output = Command::new("python3")
        .args(["docs/baselines/v2-phase-e/test_rehearse.py"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
