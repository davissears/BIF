use std::{collections::BTreeSet, fs, path::Path};

use serde_json::{Value, json};

fn manifest() -> Value {
    serde_json::from_str(include_str!("../docs/bif-v2-release-evidence.json")).unwrap()
}

fn strings(value: &Value) -> BTreeSet<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect()
}

fn path(value: &Value) {
    let value = value.as_str().unwrap();
    let candidate = Path::new(value);
    assert!(!candidate.is_absolute() && !value.contains(".."), "{value}");
    assert!(candidate.is_file(), "missing evidence/source: {value}");
}

fn read(value: &Value) -> Value {
    path(value);
    serde_json::from_slice(&fs::read(value.as_str().unwrap()).unwrap()).unwrap()
}

/// This validates evidence completeness, not approval or machine timing budgets.
fn validate(value: &Value) {
    assert_eq!(value["format"], "bif-read-release-evidence");
    assert_eq!(value["format_version"], 1);
    assert_eq!(value["candidate"]["release"], "v2.0-read");
    assert_eq!(value["candidate"]["schema"], json!({"from": 2, "to": 3}));
    assert_eq!(value["implementation"]["status"], "implemented");
    assert_eq!(
        value["implementation"]["verification"],
        "required_ci_checks"
    );

    let checks = &value["required_checks"];
    assert_eq!(checks["deterministic"]["command"], "cargo test --locked");
    assert_eq!(
        checks["deterministic"]["coverage"],
        "whole_nonignored_suite"
    );
    assert_eq!(
        strings(&checks["deterministic"]["platforms"]),
        BTreeSet::from(["ubuntu-latest", "macos-latest"])
    );
    assert_eq!(
        checks["manifest"]["command"],
        "cargo test --locked --test read_release_manifest"
    );
    assert_eq!(
        checks["rehearsal"]["command"],
        "cargo test --locked --test read_release_rehearsal"
    );
    assert_eq!(
        checks["instrumentation"]["command"],
        "cargo test --locked --test benchmark_harness report_uses_snapshot_and_contains_review_evidence -- --exact"
    );
    let structural = strings(&checks["structural"]["sources"]);
    assert!(structural.is_superset(&BTreeSet::from([
        "tests/v2_bounded_reads.rs",
        "tests/v2_index_plans.rs",
        "tests/v2_cursor_budget.rs",
        "tests/v2_read_comparison.rs",
    ])));
    assert_eq!(checks["structural"]["covered_by"], "deterministic");
    for check in checks.as_object().unwrap().values() {
        for source in check["sources"].as_array().unwrap() {
            path(source);
        }
    }
    for (check, source) in [
        ("manifest", "tests/read_release_manifest.rs"),
        ("rehearsal", "tests/read_release_rehearsal.rs"),
        ("instrumentation", "tests/benchmark_harness.rs"),
    ] {
        assert!(strings(&checks[check]["sources"]).contains(source));
    }
    path(&value["ci"]["fast"]);
    path(&value["ci"]["comparison"]);
    path(&value["rehearsal"]["runbook"]);
    path(&value["rehearsal"]["source"]);
    assert_eq!(value["ci"]["timing_thresholds"], false);
    assert_eq!(value["ci"]["private_store_required"], false);
    assert_eq!(value["ci"]["comparison_sizes"], json!([100, 10000, 100000]));
    assert_eq!(value["ci"]["comparison_seed"], 2003);
    assert_eq!(value["ci"]["comparison_profile"], "release");

    let mut blockers = BTreeSet::new();
    for phase in ["phase_c", "phase_d"] {
        let evidence = &value["timing_evidence"][phase];
        assert_eq!(evidence["required"], true);
        assert!(!evidence["scope"].as_str().unwrap().is_empty());
        let required = if phase == "phase_c" {
            BTreeSet::from([
                "/source_commit",
                "/git_status_porcelain",
                "/source_sha256",
                "/rustc",
                "/cargo",
                "/sqlite",
                "/uname",
                "/profile",
                "/argv",
                "/unix_time_seconds",
            ])
        } else {
            BTreeSet::from([
                "/source_revision",
                "/source_dirty",
                "/binary_sha256/bif",
                "/binary_sha256/bif-mcp",
                "/binary_sha256/bif-benchmark-store",
                "/environment/rust",
                "/environment/platform",
                "/environment/build_profile",
                "/fixture/seed",
                "/fixture/logical_digest",
                "/protocol_version",
                "/limitations",
            ])
        };
        assert!(strings(&evidence["required_provenance_pointers"]).is_superset(&required));
        match evidence["status"].as_str().unwrap() {
            "recorded" => {
                path(&evidence["report"]);
                let mut sizes = BTreeSet::new();
                for artifact in evidence["artifacts"].as_array().unwrap() {
                    let raw = read(artifact);
                    let fixture = if phase == "phase_c" {
                        assert_eq!(raw["format"], "bif-v2-phase-c-read-comparison-v1");
                        &raw["generator"]
                    } else {
                        assert_eq!(raw["format"], "bif-phase-d-measurement-v1");
                        assert_eq!(raw["environment"]["build_profile"], "release");
                        assert!(raw["source_dirty"].is_boolean());
                        assert_eq!(raw["real_host_smoke"], false);
                        &raw["fixture"]
                    };
                    assert_eq!(fixture["seed"], 2003);
                    assert!(!fixture["logical_digest"].as_str().unwrap().is_empty());
                    assert!(sizes.insert(fixture["items"].as_u64().unwrap()));
                }
                assert_eq!(sizes, BTreeSet::from([100, 10_000, 100_000]));
                for provenance in evidence["provenance"].as_array().unwrap() {
                    let raw = read(provenance);
                    for pointer in strings(&evidence["required_provenance_pointers"]) {
                        let field = raw.pointer(pointer).expect(pointer);
                        assert!(!field.is_null(), "null provenance {pointer}");
                        if let Some(text) = field.as_str() {
                            assert!(
                                !text.is_empty() || pointer == "/git_status_porcelain",
                                "empty provenance {pointer}"
                            );
                        }
                    }
                }
                assert!(!evidence["provenance"].as_array().unwrap().is_empty());
            }
            "pending" => {
                assert!(!evidence["reason"].as_str().unwrap().is_empty());
                assert!(evidence["artifacts"].as_array().unwrap().is_empty());
                assert!(evidence["provenance"].as_array().unwrap().is_empty());
                blockers.insert(if phase == "phase_d" {
                    "phase_d_measurements"
                } else {
                    "phase_c_measurements"
                });
            }
            status => panic!("unsupported evidence status {status}"),
        }
    }

    let host = &value["real_configured_mcp_host"];
    assert_eq!(host["required"], true);
    assert_eq!(host["kind"], "real_configured_mcp_host");
    assert_eq!(
        strings(&host["required_workflows"]),
        BTreeSet::from(["list", "get", "history", "restart"])
    );
    match host["status"].as_str().unwrap() {
        "missing" => {
            assert!(host["evidence"].is_null());
            assert!(!host["reason"].as_str().unwrap().is_empty());
            blockers.insert("real_configured_mcp_host");
        }
        "recorded" => {
            let raw = read(&host["evidence"]);
            assert_eq!(raw["kind"], "real_configured_mcp_host");
            for key in [
                "host_name",
                "host_version",
                "configuration",
                "source_revision",
                "executed_at",
            ] {
                assert!(!raw[key].as_str().unwrap().is_empty(), "{key}");
            }
            for workflow in strings(&host["required_workflows"]) {
                assert_eq!(raw["workflows"][workflow]["status"], "passed");
                path(&raw["workflows"][workflow]["transcript"]);
            }
        }
        status => panic!("unsupported host status {status}"),
    }
    assert_eq!(value["model_tokens"]["required"], false);
    assert_eq!(value["model_tokens"]["status"], "unavailable");
    assert!(
        !value["model_tokens"]["disclosure"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let old_binary = &value["rehearsal"]["old_binary"];
    match old_binary["status"].as_str().unwrap() {
        "not_executed" => assert!(!old_binary["gap"].as_str().unwrap().is_empty()),
        "recorded" => {
            let raw = read(&old_binary["evidence"]);
            assert_eq!(raw["format"], "bif-phase-d-old-binary-rehearsal-v1");
            assert!(!raw["old_revision"].as_str().unwrap().is_empty());
            assert!(!raw["candidate_revision"].as_str().unwrap().is_empty());
            for binary in ["old_bif", "candidate_bif"] {
                assert_eq!(raw["binary_sha256"][binary].as_str().unwrap().len(), 64);
            }
            assert_eq!(raw["baseline"]["migrations"].as_array().unwrap().len(), 2);
            assert_eq!(
                raw["upgraded_before_writes"]["migrations"]
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );
            assert_eq!(
                raw["baseline"]["counts"],
                raw["upgraded_before_writes"]["counts"]
            );
            for outcome in [
                "old_binary_rejects_new_schema",
                "candidate_v1_get_history_equivalent",
                "candidate_replays_old_capture",
                "old_binary_restored_get_history_replay_and_mutation",
                "rollback_discards_candidate_writes",
            ] {
                assert_eq!(raw[outcome], true, "{outcome}");
            }
            assert_eq!(raw["real_store_touched"], false);
        }
        status => panic!("unsupported old-binary rehearsal status {status}"),
    }
    assert_eq!(strings(&value["release_gate"]["blocked_by"]), blockers);
    if !blockers.is_empty() {
        assert_eq!(value["release_gate"]["status"], "blocked");
        assert_eq!(value["release_gate"]["approved"], false);
        assert!(value["release_gate"]["approval"].is_null());
    } else {
        // Evidence can make a candidate reviewable, but never auto-approve it.
        assert_eq!(value["release_gate"]["status"], "awaiting_operator_review");
        assert_eq!(value["release_gate"]["approved"], false);
        assert!(value["release_gate"]["approval"].is_null());
    }
}

#[test]
fn release_manifest_registers_checks_and_truthful_evidence_gate() {
    validate(&manifest());
}

#[test]
fn custom_process_coverage_or_green_ci_cannot_approve_missing_host_evidence() {
    for fake in [
        json!({"kind": "custom_process", "status": "recorded"}),
        json!({"kind": "real_configured_mcp_host", "status": "missing"}),
    ] {
        let mut value = manifest();
        value["real_configured_mcp_host"]["kind"] = fake["kind"].clone();
        value["real_configured_mcp_host"]["status"] = fake["status"].clone();
        value["release_gate"]["status"] = json!("approved");
        value["release_gate"]["approved"] = json!(true);
        assert!(std::panic::catch_unwind(|| validate(&value)).is_err());
    }
}

#[test]
fn required_checks_and_provenance_cannot_be_silently_dropped() {
    for pointer in [
        "/required_checks/deterministic/command",
        "/required_checks/structural/sources",
        "/timing_evidence/phase_c/provenance",
        "/timing_evidence/phase_d/required_provenance_pointers",
    ] {
        let mut value = manifest();
        *value.pointer_mut(pointer).unwrap() = json!([]);
        assert!(
            std::panic::catch_unwind(|| validate(&value)).is_err(),
            "{pointer}"
        );
    }
}
