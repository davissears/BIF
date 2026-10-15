//! Final foundation evidence is deterministic in CI; runtime measurements are manual.
use std::process::Command;

#[test]
fn final_gate_controls_and_recorded_provenance_are_valid() {
    let output = Command::new("python3")
        .arg("docs/baselines/v2-phase-e/final-gate/test_gate.py")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
