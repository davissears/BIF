#!/usr/bin/env python3
"""Deterministic final-gate controls; never build binaries or measure in CI."""
import copy
import json
import pathlib
import unittest
from unittest.mock import patch

import gate


class GateTests(unittest.TestCase):
    def test_observers_establish_sqlite_access_before_either_workload(self):
        with patch.object(gate.sqlite3, "connect") as connect:
            connection = gate.open_observer(pathlib.Path("/disposable.sqlite"))
            self.assertIs(connection, connect.return_value)
            connection.execute.assert_called_once_with("SELECT count(*) FROM sqlite_schema")
            connection.execute.return_value.fetchone.assert_called_once_with()

    def test_bundled_version_accepts_rust_c_string_bindings(self):
        self.assertEqual(gate.sqlite_version('pub const SQLITE_VERSION: &CStr = c"3.51.1";'), "3.51.1")
        self.assertEqual(gate.sqlite_version('pub const SQLITE_VERSION: &[u8] = b"3.51.1\\0";'), "3.51.1")
        with self.assertRaises(RuntimeError):
            gate.sqlite_version("missing")

    def test_paired_statistics_and_predeclared_disposition(self):
        samples = [{"baseline_ns": 100, "candidate_ns": 120} for _ in range(30)]
        summary = gate.statistics(samples)
        self.assertEqual(summary["paired_median_ratio"], 1.2)
        self.assertEqual(summary["paired_ratio_ci95"], [1.2, 1.2])
        self.assertEqual(summary["disposition"], "explanation_review_required")
        self.assertEqual(gate.statistics(samples), summary)
        for sample in samples:
            sample["candidate_ns"] = 100
        self.assertEqual(gate.statistics(samples)["disposition"], "no_trigger")
        with self.assertRaises(RuntimeError):
            gate.statistics(samples[:29])

    def test_recorded_evidence_is_bound_and_complete(self):
        gate.validate(pathlib.Path(__file__).resolve().parent)

    def test_proof_cannot_omit_cases_or_claim_live_release(self):
        directory = pathlib.Path(__file__).resolve().parent
        proof = json.loads((directory / "proof.json").read_text())
        for key, replacement in [
            ("release_approved", True),
            ("candidate_revision", "wrong"),
            ("current_schema_restore", {}),
            ("diagnostic", {}),
        ]:
            altered = copy.deepcopy(proof)
            altered[key] = replacement
            with self.assertRaises((RuntimeError, KeyError)):
                gate.validate_proof(altered)


if __name__ == "__main__":
    unittest.main()
