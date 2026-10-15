#!/usr/bin/env python3
"""Focused control tests; no historical client emulation or executable build."""
import pathlib
import json
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from rehearse import Launcher, MaintenanceBlocked, canonical, digest, snapshot, validate_order


class ControlTests(unittest.TestCase):
    def test_recorded_proof_hashes_bind_full_snapshots_sources_and_controls(self):
        directory = pathlib.Path(__file__).resolve().parent
        proof = json.loads((directory / "upgrade-proof.json").read_text())
        self.assertEqual(proof["harness_sha256"], digest((directory / "rehearse.py").read_bytes()))
        self.assertEqual(proof["control_tests_sha256"], digest(pathlib.Path(__file__).read_bytes()))
        for name in ["baseline", "upgraded", "final_state"]:
            state = proof[name]
            self.assertEqual(state["content_sha256"], digest(canonical(state["tables"])))
            self.assertEqual(state["schema_sha256"], digest(canonical(state["schema"])))
        for name in ["historical", "candidate"]:
            source = proof[name]
            self.assertEqual(source["source_sha256"], digest(canonical(source["source_inputs_sha256"])))
            self.assertEqual(source["lock_sha256"], source["source_inputs_sha256"]["Cargo.lock"])
            self.assertEqual(source["toolchain_sha256"], source["source_inputs_sha256"]["rust-toolchain.toml"])

    def test_gate_refuses_spawn_while_maintenance_exists(self):
        with tempfile.TemporaryDirectory() as directory:
            sentinel = pathlib.Path(directory) / "maintenance"
            sentinel.write_text("stopped")
            launcher = Launcher(sentinel)
            with patch("rehearse.subprocess.Popen") as spawn:
                with self.assertRaises(MaintenanceBlocked):
                    launcher.start(["not-an-old-client"])
                spawn.assert_not_called()
            sentinel.unlink()
            with patch("rehearse.subprocess.Popen") as spawn:
                launcher.start(["not-an-old-client"])
                spawn.assert_called_once_with(["not-an-old-client"], stdout=-1, stderr=-1, text=True)

    def test_snapshot_covers_full_rows_schema_allocator_and_blob_types(self):
        with tempfile.TemporaryDirectory() as directory:
            database = pathlib.Path(directory) / "fixture.sqlite"
            connection = sqlite3.connect(database)
            connection.executescript(
                "CREATE TABLE example (id INTEGER PRIMARY KEY AUTOINCREMENT, value BLOB);"
                "INSERT INTO example(value) VALUES (x'00ff');"
                "CREATE INDEX example_value ON example(value);"
            )
            connection.commit()
            before = snapshot(database)
            self.assertEqual(before["tables"]["example"], [[1, {"blob_hex": "00ff"}]])
            self.assertEqual(before["tables"]["sqlite_sequence"], [["example", 1]])
            connection.execute("UPDATE example SET value = x'01'")
            connection.commit()
            changed = snapshot(database)
            self.assertNotEqual(before["content_sha256"], changed["content_sha256"])
            self.assertEqual(before["schema_sha256"], changed["schema_sha256"])
            connection.execute("DROP INDEX example_value")
            connection.commit()
            self.assertNotEqual(changed["schema_sha256"], snapshot(database)["schema_sha256"])
            before_metadata = snapshot(database)
            connection.execute("PRAGMA user_version = 23")
            self.assertNotEqual(before_metadata["schema_sha256"], snapshot(database)["schema_sha256"])
            connection.close()

    def test_migration_cannot_precede_drain_or_outlast_restart_prevention(self):
        events = [{"event": name} for name in [
            "old_writer_stopped", "maintenance_enabled", "old_writer_continued",
            "old_writer_reaped", "candidate_migration_started",
            "candidate_migration_finished", "maintenance_disabled",
        ]]
        validate_order(events)
        for left, right in [(3, 4), (5, 6), (1, 2)]:
            invalid = list(events)
            invalid[left], invalid[right] = invalid[right], invalid[left]
            with self.assertRaises(RuntimeError):
                validate_order(invalid)


if __name__ == "__main__":
    unittest.main()
