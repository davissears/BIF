use serde_json::Value;
use std::fs;

const SIZES: [(u64, usize, &str); 3] = [
    (100, 20, "b819125481255ec7"),
    (10_000, 3, "957657e192519763"),
    (100_000, 1, "87a07c0632977092"),
];

fn read(path: &str) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn nearest_rank(values: &[u64], numerator: usize, denominator: usize) -> u64 {
    let mut values = values.to_vec();
    values.sort_unstable();
    let rank = (numerator * values.len()).div_ceil(denominator);
    values[rank - 1]
}

#[test]
fn checked_in_baselines_have_bound_provenance_and_correct_arithmetic() {
    for (items, samples, digest) in SIZES {
        let generator = read(&format!(
            "docs/baselines/v2-pre-phase-b/generator-{items}.json"
        ));
        let measurement = read(&format!(
            "docs/baselines/v2-pre-phase-b/measurement-{items}.json"
        ));
        assert_eq!(generator["format"], "bif-v2-benchmark-store-v1");
        assert_eq!(generator["items"], items);
        assert_eq!(generator["seed"], 2003);
        assert_eq!(generator["logical_digest"], digest);
        assert_eq!(generator["integrity_check"], "ok");

        assert_eq!(measurement["format"], "bif-v2-measurement-v2");
        assert_eq!(measurement["provenance"]["fixture_seed"], 2003);
        assert_eq!(measurement["provenance"]["fixture_digest"], digest);
        assert_eq!(measurement["provenance"]["dirty"], true);
        assert_eq!(
            measurement["persistence_verification"]["integrity_check"],
            "ok"
        );
        assert_eq!(
            measurement["persistence_verification"]["foreign_key_violations"],
            0
        );
        assert_eq!(
            measurement["persistence_verification"]["revision"],
            measurement["persistence_verification"]["expected_revision"]
        );

        for operation in measurement["operations"].as_array().unwrap() {
            for metric in ["wall_nanoseconds", "response_bytes"] {
                let distribution = &operation[metric];
                let raw: Vec<u64> = distribution["samples"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_u64().unwrap())
                    .collect();
                assert_eq!(raw.len(), samples);
                assert_eq!(distribution["sample_count"], samples);
                assert_eq!(distribution["p50"], nearest_rank(&raw, 50, 100));
                assert_eq!(distribution["p95"], nearest_rank(&raw, 95, 100));
            }
        }

        let list = measurement["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|operation| operation["name"] == "list_all")
            .unwrap();
        for statements in list["lexical_data_statements_per_sample"]
            .as_array()
            .unwrap()
        {
            assert_eq!(statements.as_u64().unwrap(), 1 + 2 * items);
        }
    }
}

#[test]
fn baseline_report_is_cross_linked_from_phase_a_docs() {
    let expected = "baselines/v2-pre-phase-b/README.md";
    for path in [
        "README.md",
        "docs/bif-v2-benchmark-stores.md",
        "docs/bif-v2-migration-plan.md",
        "docs/bif-v2-workflow-evaluation.md",
    ] {
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains(expected), "{path} does not link the baseline");
    }
}
