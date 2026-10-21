#!/usr/bin/env python3
"""Deterministic final-gate controls; never build binaries or measure in CI."""
import copy
import json
import pathlib
import sqlite3
import tempfile
import unittest
from contextlib import closing
from unittest.mock import MagicMock, patch

import gate


class FixtureDigestTests(unittest.TestCase):
    TABLES = (
        "store_metadata", "projects", "requester_project_counters", "items",
        "item_acceptance_criteria", "item_provenance", "operations", "events",
    )

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = pathlib.Path(self.temporary.name)
        self.baseline = self.directory / "baseline.sqlite"
        self.candidate = self.directory / "candidate.sqlite"
        for database in (self.baseline, self.candidate):
            with closing(sqlite3.connect(database)) as connection, connection:
                for table in self.TABLES:
                    if table == "requester_project_counters":
                        connection.execute(
                            f"CREATE TABLE {table} (requester TEXT, project_id TEXT, next_sequence INTEGER)")
                        connection.execute(f"INSERT INTO {table} VALUES ('GATE', 'gate', 1)")
                    else:
                        connection.execute(f"CREATE TABLE {table} (value TEXT)")
                        connection.executemany(f"INSERT INTO {table} VALUES (?)", [("b",), ("a",)])

    def test_identical_fixtures_have_exactly_eight_required_digest_keys(self):
        baseline = gate.legacy_digest(self.baseline)
        candidate = gate.legacy_digest(self.candidate)
        self.assertEqual(set(baseline), set(self.TABLES))
        self.assertEqual(len(baseline), 8)
        self.assertEqual(baseline, candidate)
        self.assertEqual(baseline, gate.legacy_digest(self.baseline))

    def test_only_counter_next_sequence_change_changes_digest(self):
        before = gate.legacy_digest(self.baseline)
        with closing(sqlite3.connect(self.candidate)) as connection, connection:
            connection.execute("UPDATE requester_project_counters SET next_sequence = 999")
        after = gate.legacy_digest(self.candidate)
        self.assertEqual(
            {table for table in before if before[table] != after[table]},
            {"requester_project_counters"},
        )
        self.assertNotEqual(before, after)

    def test_prepare_rejects_counter_only_fixture_inequality(self):
        def command(argv, *args):
            if argv[0] == "cargo":
                generator = self.directory / "historical-target/release/bif-benchmark-store"
                generator.parent.mkdir(parents=True)
                generator.write_bytes(b"unused generator stub")
            elif "--output" in argv:
                generated = pathlib.Path(argv[argv.index("--output") + 1])
                gate.copy_database(self.baseline, generated)
                pathlib.Path(str(generated) + ".metadata.json").write_text(json.dumps({
                    "logical_digest": "unused", "logical_digest_algorithm": "unused",
                }))
            return {}

        def cli(binary, root, args, env):
            if args == ["init"]:
                (root / "config.toml").write_text("")
            elif args == ["doctor"]:
                with closing(sqlite3.connect(gate.db(root))) as connection, connection:
                    connection.execute("UPDATE requester_project_counters SET next_sequence = 999")
            return {}

        with patch.object(gate, "command", side_effect=command), \
                patch.object(gate, "cli", side_effect=cli), patch.object(gate, "SIZES", [100]):
            with self.assertRaisesRegex(RuntimeError, "fixtures not logically comparable"):
                gate.prepare(self.directory, pathlib.Path("old"), pathlib.Path("candidate"), {})

    def test_missing_each_required_table_errors_even_when_both_fixtures_lack_it(self):
        for table in self.TABLES:
            with self.subTest(table=table):
                for database in (self.baseline, self.candidate):
                    with closing(sqlite3.connect(database)) as connection, connection:
                        connection.execute(f'ALTER TABLE "{table}" RENAME TO missing_table')
                try:
                    for database in (self.baseline, self.candidate):
                        with self.subTest(database=database.name):
                            with self.assertRaisesRegex(RuntimeError, table):
                                gate.legacy_digest(database)
                finally:
                    for database in (self.baseline, self.candidate):
                        with closing(sqlite3.connect(database)) as connection, connection:
                            connection.execute(f'ALTER TABLE missing_table RENAME TO "{table}"')

    def test_empty_existing_required_tables_are_valid(self):
        for table in self.TABLES:
            with self.subTest(table=table):
                for database in (self.baseline, self.candidate):
                    with closing(sqlite3.connect(database)) as connection, connection:
                        connection.execute(f'DELETE FROM "{table}"')
                baseline = gate.legacy_digest(self.baseline)
                self.assertEqual(set(baseline), set(self.TABLES))
                self.assertEqual(baseline[table], gate.digest(b""))
                self.assertEqual(baseline, gate.legacy_digest(self.candidate))

    def test_digest_is_independent_of_insertion_order(self):
        before = gate.legacy_digest(self.candidate)
        with closing(sqlite3.connect(self.candidate)) as connection, connection:
            connection.execute("DELETE FROM items")
            connection.executemany("INSERT INTO items VALUES (?)", [("a",), ("b",)])
        self.assertEqual(before, gate.legacy_digest(self.candidate))

    def test_recorded_digest_keys_must_be_exact(self):
        hashes = {table: gate.digest(b"") for table in self.TABLES}
        gate.validate_fixture_digest({"legacy_table_sha256": hashes})
        for table in self.TABLES:
            with self.subTest(missing=table):
                incomplete = {key: value for key, value in hashes.items() if key != table}
                with self.assertRaisesRegex(RuntimeError, "fixture digest tables"):
                    gate.validate_fixture_digest({"legacy_table_sha256": incomplete})
        for extra in ("counters", "unexpected"):
            with self.subTest(extra=extra):
                with self.assertRaisesRegex(RuntimeError, "fixture digest tables"):
                    gate.validate_fixture_digest({"legacy_table_sha256": {**hashes, extra: gate.digest(b"")}})


class WorkloadSemanticTests(unittest.TestCase):
    """Synthetic pre-rerun inputs: no archived evidence is rewritten or trusted."""

    @classmethod
    def setUpClass(cls):
        cls.protocol, cls.measurements, cls.proof, cls.upgrade = cls.synthetic_evidence()

    @staticmethod
    def synthetic_evidence():
        cwd, directory = "/recorded/checkout", "/recorded/disposable"
        production = gate.archived.source_inputs(gate.ROOT)
        builds = {}
        for role, target, revision in [
            ("baseline", "historical-target", gate.OLD),
            ("candidate", "candidate-target", gate.CANDIDATE),
        ]:
            source = production if role == "candidate" else {"old.rs": "old-source"}
            builds[role] = {
                "revision": revision, "source_modified": False,
                "source_inputs_sha256": source,
                "source_sha256": gate.digest(gate.canonical(source)),
                "source_end_sha256": gate.digest(gate.canonical(source)),
                "binary_sha256": gate.digest(role.encode()),
                "binary_end_sha256": gate.digest(role.encode()),
                "build": {
                    "argv": ["cargo", "build", "--locked", "--release", "--bin", "bif",
                             "--target-dir", f"{directory}/{target}"],
                    "cwd": cwd if role == "candidate" else f"{directory}/historical-source",
                    "exit_code": 0, "locked": True, "separate_target": True,
                },
            }
        upgrade = {
            "historical": builds["baseline"], "candidate": builds["candidate"],
            "events": [{"event": name} for name in [
                "old_writer_stopped", "maintenance_enabled", "old_writer_continued",
                "old_writer_reaped", "candidate_migration_started",
                "candidate_migration_finished", "maintenance_disabled",
            ]],
            "blocked_restarts": [{"process_spawned": False}],
            "rejections": [{"before": {}, "after": {}}],
        }
        proof = {
            "candidate_revision": gate.CANDIDATE, "release_approved": False,
            "sync_enabled": 0, "bootstrap_required": 1, "fresh": {"migrations": [1, 2, 3, 4]},
            "historical_rollback": {"separate_root": True},
            "current_schema_restore": {
                "generations": ["a", "b", "c"], "repeated_identical_backup": True,
                "restored_tables_equal_except_generation": True,
                "preserved_later": [{"tables_equal_before": True}] * 2,
            },
            "prune": {"audit_receipts_and_allocator_unchanged": True,
                      "retained_coverage_verified": True},
            "diagnostic": {"exit_code": 0},
        }
        protocol = {
            "candidate_revision": gate.CANDIDATE, "baseline_revision": gate.OLD,
            "baseline_build": builds["baseline"], "candidate_build": builds["candidate"],
            "production_inputs_sha256": production, "harness_inputs_sha256": gate.harness_inputs(),
            "frozen_before_timing": True, "frozen_at_unix_ns": 1,
            "warmups": 5, "paired_samples": 30, "sizes": [100, 10000, 100000], "seed": 2003,
            "workload_order": ["capture", "effective", "note_only", "compound", "replay", "genuine_noop"],
            "fixtures": [],
        }
        # Sample expectations below deliberately do not use the shared generator.
        protocol["workload_spec"] = copy.deepcopy(gate.WORKLOAD_SPEC)
        measurements = {"started_at_unix_ns": 2, "fixtures": []}
        summary = gate.statistics([{"baseline_ns": 100, "candidate_ns": 101}] * 30)
        for size in protocol["sizes"]:
            fixture = {
                "items": size, "legacy_tables_equal": True,
                "legacy_table_sha256": {table: gate.digest(b"") for table in FixtureDigestTests.TABLES},
                "roots": {}, "execution": {},
            }
            config = f'root = "{directory}/benchmark-{size}-baseline"\nrequester = "GATE"\n'
            for role, target, root_name in [
                ("baseline", "historical-target", f"working-{size}-baseline"),
                ("candidate", "candidate-target", f"benchmark-{size}-candidate"),
            ]:
                root = f"{directory}/{root_name}"
                fixture["roots"][role] = root
                fixture["execution"][role] = {
                    "executable": f"{directory}/{target}/release/bif",
                    "binary_sha256": builds[role]["binary_sha256"],
                    "root": root, "config_path": f"{root}/config.toml",
                    "config_contents": config, "config_sha256": gate.digest(config.encode()),
                    "requester": "GATE", "cwd": cwd,
                }

            def record(role, args, stdout):
                context = fixture["execution"][role]
                return {
                    "argv": [context["executable"], *args, "--config", context["config_path"],
                             "--root", context["root"], "--requester", "GATE"],
                    "cwd": cwd, "exit_code": 0, "stdout": stdout, "stderr": "",
                }

            def capture(item, title, replay=False):
                return (f"Captured {item}\ntitle: {title}\nstatus: proposed\n"
                        f"replayed: {str(replay).lower()}\nsource: local\n")

            def triage(revision, priority, assignee):
                return (f"Updated GATE:gate:001\nstatus: proposed\npriority: {priority}\n"
                        f"assignee: {assignee}\nrevision: {revision}\nreplayed: false\n")

            measured = {"items": size, "setup": {}, "workloads": {}}
            for role in ("baseline", "candidate"):
                measured["setup"][role] = [
                    record(role, ["capture", "Measurement target", "--project", "gate",
                                  "--idempotency-key", "measure-target"],
                           capture("GATE:gate:001", "Measurement target")),
                    record(role, ["triage", "GATE:gate:001", "--expected-revision", "1",
                                  "--idempotency-key", "measure-initial", "--priority", "P2"],
                           triage(2, "P2", "unassigned")),
                ]
            for kind in protocol["workload_order"]:
                rows = []
                for index in range(35):
                    revision = {"effective": 2 + index, "note_only": 37 + index,
                                "compound": 72 + index, "genuine_noop": 107}.get(kind, 2)
                    args = ["triage", "GATE:gate:001", "--expected-revision", str(revision),
                            "--idempotency-key", f"measure-{kind}-{index}"]
                    if kind == "capture":
                        args = ["capture", f"Measured {index}", "--project", "gate",
                                "--idempotency-key", f"measure-capture-{index}"]
                        stdout = capture(f"GATE:gate:{index + 2:03}", f"Measured {index}")
                    elif kind == "replay":
                        args = ["capture", "Measurement target", "--project", "gate",
                                "--idempotency-key", "measure-target"]
                        stdout = capture("GATE:gate:001", "Measurement target", True)
                    elif kind == "effective":
                        priority = "P1" if index % 2 == 0 else "P2"
                        args += ["--priority", priority]
                        stdout = triage(revision + 1, priority, "unassigned")
                    elif kind == "note_only":
                        args += ["--note", f"Measured note {index}"]
                        stdout = triage(revision + 1, "P1", "unassigned")
                    elif kind == "compound":
                        priority = "P2" if index % 2 == 0 else "P1"
                        args += ["--priority", priority, "--assignee", f"OWNER{index}",
                                 "--note", f"Compound note {index}"]
                        stdout = triage(revision + 1, priority, f"owner{index}")
                    else:
                        args += ["--priority", "P2"]
                        stdout = triage(107, "P2", "owner34")
                    rows.append({
                        "pair": index,
                        "order": ["baseline", "candidate"] if index % 2 == 0 else ["candidate", "baseline"],
                        "baseline_ns": 100, "candidate_ns": 101,
                        "candidate_journal_delta": 0 if kind in ("replay", "genuine_noop") else 1,
                        "commands": {role: record(role, args, stdout) for role in ("baseline", "candidate")},
                    })
                storage = {phase: {"main_db_bytes": 1, "wal_bytes": 0} for phase in [
                    "before_workload", "after_workload_before_checkpoint",
                    "after_explicit_wal_checkpoint_TRUNCATE",
                ]}
                measured["workloads"][kind] = {
                    "warmups": rows[:5], "raw_samples": rows[5:], "statistics": summary,
                    "storage": {role: {**storage, "checkpoint_result": [[0, 0, 0]]}
                                for role in ("baseline", "candidate")},
                }
            protocol["fixtures"].append(fixture)
            measurements["fixtures"].append(measured)
        return protocol, measurements, proof, upgrade

    def validate_full(self, measurements):
        # Brand-new synthetic envelope with real harness/production bindings.
        # Attack mutations affect only measurements, never frozen protocol bytes.
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            protocol, proof = copy.deepcopy(self.protocol), copy.deepcopy(self.proof)
            gate.write(directory / "upgrade-proof.json", self.upgrade)
            protocol["upgrade_proof_sha256"] = gate.digest((directory / "upgrade-proof.json").read_bytes())
            payload = {key: value for key, value in proof.items() if key not in
                       ("candidate_revision", "release_approved", "sync_enabled", "bootstrap_required")}
            protocol["correctness_payload_sha256"] = gate.digest(gate.canonical(payload))
            gate.write(directory / "protocol.json", protocol)
            protocol_hash = gate.digest((directory / "protocol.json").read_bytes())
            proof["protocol_sha256"] = protocol_hash
            gate.write(directory / "proof.json", proof)
            gate.write(directory / "measurements.json", {**measurements, "protocol_sha256": protocol_hash})
            gate.validate(directory)
            return protocol_hash

    def test_complete_six_workload_sequence_passes_full_validation_offline(self):
        self.validate_full(self.measurements)
        with patch.object(pathlib.Path, "open", side_effect=AssertionError("live filesystem read")), \
                patch.object(gate, "command", side_effect=AssertionError("live command")):
            gate.validate_semantics(self.protocol, self.measurements)

    def test_strict_validation_rejects_changed_harness_and_production_separately(self):
        for owner, name, message in [
            (gate, "harness_inputs", "harness/tests changed"),
            (gate.archived, "source_inputs", "production changed"),
        ]:
            with self.subTest(inputs=name), patch.object(owner, name, return_value={"changed": "sha256"}):
                with self.assertRaisesRegex(RuntimeError, message):
                    self.validate_full(self.measurements)

    def test_all_candidate_argv_replaced_by_baseline_is_rejected_semantically(self):
        altered = copy.deepcopy(self.measurements)
        for fixture in altered["fixtures"]:
            for workload in fixture["workloads"].values():
                for row in workload["warmups"] + workload["raw_samples"]:
                    row["commands"]["candidate"]["argv"] = row["commands"]["baseline"]["argv"][:]
        with self.assertRaisesRegex(RuntimeError, "semantic argv"):
            self.validate_full(altered)

    def test_whole_effective_and_note_workloads_swapped_is_rejected_semantically(self):
        altered = copy.deepcopy(self.measurements)
        for fixture in altered["fixtures"]:
            workloads = fixture["workloads"]
            workloads["effective"], workloads["note_only"] = workloads["note_only"], workloads["effective"]
        with self.assertRaisesRegex(RuntimeError, "semantic argv"):
            self.validate_full(altered)

    def test_exact_argv_for_both_roles_and_warmup_and_measured_boundaries(self):
        changes = [
            ("capture", 0, "/wrong/bif"), ("capture", 2, "Repeated title"),
            ("capture", "--idempotency-key", "measure-target"),
            ("effective", "--expected-revision", "999"),
            ("effective", "--priority", "P2"), ("note_only", "--note", "wrong note"),
            ("compound", "--assignee", "OWNER999"), ("compound", "--note", "wrong note"),
            ("replay", "--idempotency-key", "fresh-key"),
            ("genuine_noop", "--idempotency-key", "measure-genuine_noop-0"),
            ("genuine_noop", "--priority", "P1"),
            ("capture", "--root", "/wrong/root"), ("capture", "--config", "/wrong/config"),
            ("capture", "--requester", "OTHER"),
        ]
        for role in ("baseline", "candidate"):
            for phase, index in (("warmups", 1), ("raw_samples", 0), ("raw_samples", 29)):
                for kind, selector, value in changes:
                    with self.subTest(role=role, phase=phase, index=index, kind=kind, selector=selector):
                        altered = copy.deepcopy(self.measurements)
                        argv = altered["fixtures"][0]["workloads"][kind][phase][index]["commands"][role]["argv"]
                        position = selector if isinstance(selector, int) else argv.index(selector) + 1
                        # Ensure even the alternating priority cases actually change.
                        argv[position] = value if argv[position] != value else "definitely-wrong"
                        with self.assertRaisesRegex(RuntimeError, "semantic argv"):
                            gate.validate_semantics(self.protocol, altered)
        for kind in self.protocol["workload_order"]:
            for extra in (["--json"], ["--priority", "P1"], ["--assignee", "OWNER0"],
                          ["--idempotency-key", "duplicate"], ["--requester", "GATE"]):
                with self.subTest(kind=kind, extra=extra):
                    altered = copy.deepcopy(self.measurements)
                    argv = altered["fixtures"][0]["workloads"][kind]["warmups"][0]["commands"]["candidate"]["argv"]
                    argv.extend(extra)
                    with self.assertRaisesRegex(RuntimeError, "semantic argv"):
                        gate.validate_semantics(self.protocol, altered)
        altered = copy.deepcopy(self.measurements)
        argv = altered["fixtures"][0]["workloads"]["compound"]["raw_samples"][0]["commands"]["candidate"]["argv"]
        del argv[argv.index("--note"):argv.index("--note") + 2]
        with self.assertRaisesRegex(RuntimeError, "semantic argv"):
            gate.validate_semantics(self.protocol, altered)

    def test_output_semantics_are_derived_from_setup_not_sample_claims(self):
        changes = [
            ("capture", "GATE:gate:002", "GATE:gate:001"),
            ("capture", "Measured 0", "Measured 1"),
            ("capture", "replayed: false", "replayed: true"),
            ("effective", "GATE:gate:001", "OTHER:gate:001"),
            ("effective", "revision: 3", "revision: 2"),
            ("effective", "priority: P1", "priority: P2"),
            ("note_only", "priority: P1", "priority: P2"),
            ("note_only", "assignee: unassigned", "assignee: owner0"),
            ("note_only", "revision: 38", "revision: 37"),
            ("compound", "assignee: owner0", "assignee: OWNER0"),
            ("compound", "revision: 73", "revision: 72"),
            ("replay", "GATE:gate:001", "GATE:gate:002"),
            ("replay", "replayed: true", "replayed: false"),
            ("genuine_noop", "revision: 107", "revision: 108"),
            ("genuine_noop", "replayed: false", "replayed: true"),
            ("genuine_noop", "assignee: owner34", "assignee: unassigned"),
        ]
        for role in ("baseline", "candidate"):
            for kind, before, after in changes:
                with self.subTest(role=role, kind=kind, before=before):
                    altered = copy.deepcopy(self.measurements)
                    result = altered["fixtures"][0]["workloads"][kind]["warmups"][0]["commands"][role]
                    self.assertIn(before, result["stdout"])
                    result["stdout"] = result["stdout"].replace(before, after)
                    with self.assertRaisesRegex(RuntimeError, "semantic output"):
                        gate.validate_semantics(self.protocol, altered)
        for field, value in [
            ("exit_code", 1), ("stderr", "unexpected warning"),
            ("cwd", "/current/checkout"),
            ("stdout", "Captured GATE:gate:002\nreplayed: false\nreplayed: true\n"),
        ]:
            with self.subTest(field=field):
                altered = copy.deepcopy(self.measurements)
                altered["fixtures"][0]["workloads"]["capture"]["raw_samples"][0]["commands"]["candidate"][field] = value
                with self.assertRaisesRegex(RuntimeError, "semantic (output|cwd|success)"):
                    gate.validate_semantics(self.protocol, altered)

    def test_execution_context_is_bound_to_recorded_build_and_config_identity(self):
        for field, value in [
            ("executable", "/recorded/disposable/historical-target/release/bif"),
            ("binary_sha256", "wrong"), ("root", "/another/root"),
            ("config_path", "/another/config.toml"), ("config_contents", 'requester = "OTHER"\n'),
            ("config_sha256", "wrong"), ("requester", "OTHER"), ("cwd", "/another/checkout"),
        ]:
            with self.subTest(field=field):
                protocol = copy.deepcopy(self.protocol)
                context = protocol["fixtures"][0]["execution"]["candidate"]
                context[field] = value
                if field == "config_contents":
                    context["config_sha256"] = gate.digest(value.encode())
                with self.assertRaisesRegex(RuntimeError, "semantic context"):
                    gate.validate_semantics(protocol, self.measurements)
        protocol = copy.deepcopy(self.protocol)
        protocol["candidate_build"]["build"]["argv"][-1] = "/recorded/disposable/historical-target"
        with self.assertRaisesRegex(RuntimeError, "semantic build"):
            gate.validate_semantics(protocol, self.measurements)

    def test_freeze_records_exact_config_bytes_paths_and_binary_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            fixture, upgrade, binaries = {"roots": {}}, {}, {}
            for role, source in [("baseline", "historical"), ("candidate", "candidate")]:
                root = directory / role
                root.mkdir()
                config = b'root = "/recorded/root"\r\nrequester = "GATE"\r\n'
                (root / "config.toml").write_bytes(config)
                binary = directory / (role + "-bif")
                binary.write_bytes(role.encode())
                fixture["roots"][role] = str(root)
                binaries[role] = binary
                upgrade[source] = {"binary_sha256": gate.digest(role.encode())}
            execution = gate.freeze_execution(fixture, binaries["baseline"], binaries["candidate"], upgrade)
            for role, context in execution.items():
                self.assertEqual(context["root"], fixture["roots"][role])
                self.assertEqual(context["executable"], str(binaries[role]))
                self.assertEqual(context["config_path"], fixture["roots"][role] + "/config.toml")
                self.assertEqual(context["config_contents"].encode(), config)
                self.assertEqual(context["config_sha256"], gate.digest(config))
                self.assertEqual(context["binary_sha256"], gate.digest(role.encode()))
                self.assertEqual(context["requester"], "GATE")
                self.assertEqual(context["cwd"], str(gate.ROOT))
            binaries["candidate"].write_bytes(b"changed")
            with self.assertRaisesRegex(RuntimeError, "binary changed before freeze"):
                gate.freeze_execution(fixture, binaries["baseline"], binaries["candidate"], upgrade)

    def test_frozen_spec_order_setup_and_effects_are_enforced(self):
        for field, value in [("workload_order", list(reversed(self.protocol["workload_order"]))),
                             ("workload_spec", {"version": "wrong"})]:
            with self.subTest(field=field):
                protocol = copy.deepcopy(self.protocol)
                protocol[field] = value
                with self.assertRaisesRegex(RuntimeError, "semantic protocol"):
                    gate.validate_semantics(protocol, self.measurements)
        for role in ("baseline", "candidate"):
            altered = copy.deepcopy(self.measurements)
            altered["fixtures"][0]["setup"][role][1]["stdout"] = "Updated GATE:gate:001\nrevision: 999\n"
            with self.assertRaisesRegex(RuntimeError, "semantic output"):
                gate.validate_semantics(self.protocol, altered)
        for kind in self.protocol["workload_order"]:
            altered = copy.deepcopy(self.measurements)
            row = altered["fixtures"][0]["workloads"][kind]["raw_samples"][0]
            row["candidate_journal_delta"] = 1 - row["candidate_journal_delta"]
            with self.assertRaisesRegex(RuntimeError, "semantic journal"):
                gate.validate_semantics(self.protocol, altered)

    def test_setup_commands_counts_pair_order_and_roles_are_exact(self):
        for role in ("baseline", "candidate"):
            altered = copy.deepcopy(self.measurements)
            altered["fixtures"][0]["setup"][role][0]["argv"][2] = "Wrong setup target"
            with self.assertRaisesRegex(RuntimeError, "semantic argv"):
                gate.validate_semantics(self.protocol, altered)
            altered = copy.deepcopy(self.measurements)
            altered["fixtures"][0]["setup"][role].pop()
            with self.assertRaisesRegex(RuntimeError, "semantic setup count"):
                gate.validate_semantics(self.protocol, altered)
        for field, value in [("pair", 99), ("order", ["candidate", "baseline"]),
                             ("commands", {"candidate": {}})]:
            altered = copy.deepcopy(self.measurements)
            altered["fixtures"][0]["workloads"]["capture"]["warmups"][0][field] = value
            with self.assertRaisesRegex(RuntimeError, "semantic (pair order|command roles)"):
                gate.validate_semantics(self.protocol, altered)
        for phase in ("warmups", "raw_samples"):
            altered = copy.deepcopy(self.measurements)
            altered["fixtures"][0]["workloads"]["capture"][phase].pop()
            with self.assertRaisesRegex(RuntimeError, "semantic sample count"):
                gate.validate_semantics(self.protocol, altered)
        for field in ("workload_spec", "fixtures"):
            protocol = copy.deepcopy(self.protocol)
            if field == "workload_spec":
                protocol[field]["workloads"]["note_only"]["revision_delta"] = 0
            else:
                protocol[field].reverse()
            with self.assertRaisesRegex(RuntimeError, "semantic (protocol|fixture order)"):
                gate.validate_semantics(protocol, self.measurements)

    def test_measure_generates_the_independently_expected_complete_sequence(self):
        fixture, expected = self.protocol["fixtures"][0], self.measurements["fixtures"][0]
        queues = {role: [(None, result) for result in expected["setup"][role]]
                  for role in ("baseline", "candidate")}
        for kind in self.protocol["workload_order"]:
            workload = expected["workloads"][kind]
            for row in workload["warmups"] + workload["raw_samples"]:
                index = row["pair"]
                revision = {"capture": 2, "effective": 2 + index, "note_only": 37 + index,
                            "compound": 72 + index, "replay": 107, "genuine_noop": 107}[kind]
                priority = "P2"
                if kind == "effective":
                    priority = "P2" if index % 2 == 0 else "P1"
                elif kind == "note_only":
                    priority = "P1"
                elif kind == "compound":
                    priority = "P1" if index % 2 == 0 else "P2"
                for role in queues:
                    queues[role].append(({"revision": revision, "priority": priority}, row["commands"][role]))
        calls = {role: iter(queue) for role, queue in queues.items()}
        pending = {}

        def cli(binary, root, args, env):
            role = "baseline" if "historical-target" in str(binary) else "candidate"
            if args[0] == "get":
                state, result = next(calls[role])
                pending[role] = result
                return {"stdout": json.dumps(state)}
            result = pending.pop(role) if role in pending else next(calls[role])[1]
            self.assertEqual(gate.cli_argv(binary, root, args), result["argv"])
            return copy.deepcopy(result)

        # Deterministic stand-ins only: no subprocess, database, or real timing.
        observers = {role: MagicMock() for role in ("baseline", "candidate")}
        deltas = [row["candidate_journal_delta"]
                  for kind in self.protocol["workload_order"]
                  for row in expected["workloads"][kind]["warmups"] + expected["workloads"][kind]["raw_samples"]]
        watermarks, high = [], 0
        for delta in deltas:
            watermarks.extend([(high,), (high + delta,)])
            high += delta
        for observer in observers.values():
            observer.execute.return_value.fetchone.return_value = (0, 0, 0)
            observer.execute.return_value.fetchall.return_value = [[0, 0, 0]]
        observers["candidate"].execute.return_value.fetchone.side_effect = [(0, 0, 0), *watermarks]
        with patch.object(gate, "cli", side_effect=cli), \
                patch.object(gate, "open_observer", side_effect=observers.values()), \
                patch.object(gate, "sizes", return_value={"main_db_bytes": 1, "wal_bytes": 0}), \
                patch.object(gate.time, "perf_counter_ns", side_effect=range(840)):
            measured = gate.measure(
                fixture, pathlib.Path(fixture["execution"]["baseline"]["executable"]),
                pathlib.Path(fixture["execution"]["candidate"]["executable"]), {})
        for kind in self.protocol["workload_order"]:
            for phase in ("warmups", "raw_samples"):
                for actual, wanted in zip(measured["workloads"][kind][phase], expected["workloads"][kind][phase]):
                    self.assertEqual(actual["commands"], wanted["commands"])
                    self.assertEqual(actual["candidate_journal_delta"], wanted["candidate_journal_delta"])
        for role in calls:
            self.assertEqual(list(calls[role]), [])
            observers[role].close.assert_called_once_with()


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
        with patch.object(gate, "harness_inputs", return_value={"changed": "harness"}), \
                patch.object(gate.archived, "source_inputs", return_value={"changed": "production"}):
            gate.validate_historical(pathlib.Path(__file__).resolve().parent)

    def test_historical_validation_runs_provenance_semantics_and_statistics(self):
        with patch.object(gate, "validate_proof", wraps=gate.validate_proof) as proof, \
                patch.object(gate.archived, "validate_order", wraps=gate.archived.validate_order) as order, \
                patch.object(gate, "validate_semantics", wraps=gate.validate_semantics) as semantics, \
                patch.object(gate, "validate_fixture_digest", wraps=gate.validate_fixture_digest) as fixtures, \
                patch.object(gate, "statistics", wraps=gate.statistics) as statistics:
            gate.validate_historical(pathlib.Path(__file__).resolve().parent)
        proof.assert_called_once()
        order.assert_called_once()
        semantics.assert_called_once()
        self.assertEqual(fixtures.call_count, 3)
        self.assertEqual(statistics.call_count, 18)

    def test_historical_validation_rejects_each_changed_artifact_before_semantic_checks(self):
        here = pathlib.Path(__file__).resolve().parent
        names = ("proof.json", "protocol.json", "measurements.json", "upgrade-proof.json")
        for changed in names:
            with self.subTest(artifact=changed), tempfile.TemporaryDirectory() as temporary:
                directory = pathlib.Path(temporary)
                for name in names:
                    (directory / name).write_bytes((here / name).read_bytes())
                artifact = directory / changed
                artifact.write_bytes(artifact.read_bytes() + b"\n")
                # An adjacent mutable manifest cannot authorize different bytes.
                gate.write(directory / "validation-results.json", {
                    "generated_artifacts_sha256": {
                        name: gate.digest((directory / name).read_bytes()) for name in names}})
                with patch.object(gate, "validate_proof") as proof:
                    with self.assertRaisesRegex(RuntimeError, "historical artifact changed: " + changed):
                        gate.validate_historical(directory)
                proof.assert_not_called()

    def test_historical_validation_rejects_coordinated_internal_rebinding(self):
        here = pathlib.Path(__file__).resolve().parent
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            records = {name: json.loads((here / name).read_text()) for name in
                       ("proof.json", "protocol.json", "measurements.json", "upgrade-proof.json")}
            protocol, upgrade = records["protocol.json"], records["upgrade-proof.json"]
            replacement = {"replacement.rs": gate.digest(b"replacement source")}
            candidate = upgrade["candidate"]
            candidate["source_inputs_sha256"] = replacement
            candidate["source_sha256"] = candidate["source_end_sha256"] = gate.digest(gate.canonical(replacement))
            protocol["candidate_build"] = candidate
            protocol["production_inputs_sha256"] = replacement
            gate.write(directory / "upgrade-proof.json", upgrade)
            protocol["upgrade_proof_sha256"] = gate.digest((directory / "upgrade-proof.json").read_bytes())
            gate.write(directory / "protocol.json", protocol)
            protocol_hash = gate.digest((directory / "protocol.json").read_bytes())
            for name in ("proof.json", "measurements.json"):
                records[name]["protocol_sha256"] = protocol_hash
                gate.write(directory / name, records[name])
            gate.write(directory / "validation-results.json", {
                "generated_artifacts_sha256": {
                    name: gate.digest((directory / name).read_bytes()) for name in records}})
            # All existing internal bindings pass with matching current inputs.
            with patch.object(gate, "harness_inputs", return_value=protocol["harness_inputs_sha256"]), \
                    patch.object(gate.archived, "source_inputs", return_value=replacement):
                gate.validate(directory)
            with self.assertRaisesRegex(RuntimeError, "historical artifact changed"):
                gate.validate_historical(directory)

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
