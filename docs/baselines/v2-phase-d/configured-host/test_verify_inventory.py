"""Focused checks of the retained snapshots, without a database or host."""

import copy
import hashlib
import json
import unittest
from pathlib import Path

from verify_inventory import canonical_sha256, verify_inventory, verify_snapshots


BASE = Path(__file__).resolve().parent
STAGES = ("before", "after_reads", "after_restart", "after_fixture_write")


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.snapshots = {
            stage: json.loads(
                (BASE / "inventory" / f"inventory-{stage.replace('_', '-')}.json")
                .read_text(encoding="utf-8")
            )
            for stage in STAGES
        }
        self.report = json.loads((BASE / "inventory-verification.json").read_text())
        self.transcript = json.loads((BASE / "transcript.json").read_text())

    def verify(self, snapshots=None):
        return verify_snapshots(
            snapshots if snapshots is not None else self.snapshots,
            self.report,
            self.transcript,
        )

    def test_retained_evidence(self):
        self.assertEqual(verify_inventory(BASE), self.verify())

    def test_same_row_count_corruption_during_reads_or_restart(self):
        for stage in ("after_reads", "after_restart"):
            with self.subTest(stage=stage):
                snapshots = copy.deepcopy(self.snapshots)
                snapshots[stage]["tables"]["items"][0][4] = "Corrupted title"
                with self.assertRaisesRegex(ValueError, stage):
                    self.verify(snapshots)

    def test_schema_and_metadata_corruption(self):
        for stage in STAGES[1:]:
            for field in ("schema", "user_version", "application_id"):
                with self.subTest(stage=stage, field=field):
                    snapshots = copy.deepcopy(self.snapshots)
                    if field == "schema":
                        snapshots[stage][field][0][3] += " -- changed"
                    else:
                        snapshots[stage][field] += 1
                    with self.assertRaises(ValueError):
                        self.verify(snapshots)

    def test_unintended_after_write_mutation(self):
        for table, column in (
            ("projects", 1),
            ("items", 4),
            ("events", 6),
            ("operations", 2),
        ):
            with self.subTest(table=table):
                snapshots = copy.deepcopy(self.snapshots)
                snapshots["after_fixture_write"]["tables"][table][0][column] = "bad"
                with self.assertRaises(ValueError):
                    self.verify(snapshots)
        snapshots = copy.deepcopy(self.snapshots)
        item = next(
            row for row in snapshots["after_fixture_write"]["tables"]["items"]
            if row[0] == "BENCH:core:026"
        )
        item[4] = "Unintended change on the deliberately changed item"
        with self.assertRaises(ValueError):
            self.verify(snapshots)

    def test_added_mutation_rows_must_match_capture_and_each_other(self):
        for table, column in (
            ("events", 1),              # Operation link
            ("events", 8),              # Actor
            ("operations", 3),          # Expected revision
            ("mutation_receipts", 0),   # Recorded idempotency key
            ("mutation_receipts", 3),   # Operation link
            ("mutation_receipts", 5),   # Response JSON
        ):
            with self.subTest(table=table, column=column):
                snapshots = copy.deepcopy(self.snapshots)
                snapshots["after_fixture_write"]["tables"][table][-1][column] = "bad"
                with self.assertRaises(ValueError):
                    self.verify(snapshots)

    def test_canonical_hash_is_not_pretty_file_hash(self):
        value = {"z": "é", "a": [1, None]}
        canonical = b'{"a":[1,null],"z":"\\u00e9"}'
        pretty = json.dumps(value, indent=2, ensure_ascii=False).encode("utf-8")
        self.assertEqual(canonical_sha256(value), hashlib.sha256(canonical).hexdigest())
        self.assertNotEqual(canonical_sha256(value), hashlib.sha256(pretty).hexdigest())
        self.assertEqual(canonical_sha256(json.loads(pretty)), canonical_sha256(value))


if __name__ == "__main__":
    unittest.main()
