#!/usr/bin/env python3
"""Rehearse upgrade and rollback with old/new executables on generated roots."""

import argparse
from contextlib import closing
import hashlib
import json
import os
import pathlib
import sqlite3
import subprocess
import tempfile
import time

from measure import command, machine_details, source_snapshot


def run(binary, root, arguments, success=True):
    environment = {key: value for key, value in os.environ.items()
                   if key not in ("BIF_ROOT", "BIF_CONFIG", "BIF_REQUESTER")}
    result = subprocess.run(
        [str(binary), *arguments, "--config", str(root / "config.toml")],
        env=environment, capture_output=True, text=True, timeout=30,
    )
    if success and result.returncode != 0:
        raise RuntimeError(result.stderr)
    return result


def backup(source, destination):
    destination.parent.mkdir(parents=True)
    with destination.open("xb"):
        pass
    with closing(sqlite3.connect(source)) as original, closing(sqlite3.connect(destination)) as copied:
        original.backup(copied)


def inventory(database):
    with closing(sqlite3.connect(database)) as connection:
        integrity = connection.execute("PRAGMA integrity_check").fetchone()[0]
        foreign_keys = connection.execute("PRAGMA foreign_key_check").fetchall()
        if integrity != "ok" or foreign_keys:
            raise RuntimeError({"integrity": integrity, "foreign_keys": foreign_keys})
        return {
            "integrity": integrity, "foreign_keys": foreign_keys,
            "store_id": connection.execute("SELECT store_id FROM store_metadata").fetchone()[0],
            "migrations": connection.execute(
                "SELECT version,name,checksum FROM schema_migrations ORDER BY version"
            ).fetchall(),
            "counts": {table: connection.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
                       for table in ("items", "item_acceptance_criteria", "item_provenance",
                                     "operations", "events", "mutation_receipts")},
        }


def rehearse(args):
    old = args.old_binaries.resolve(strict=True)
    new = args.binaries.resolve(strict=True)
    source_inputs = source_snapshot()
    parent = pathlib.Path("target/phase-d-rehearsals")
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="run-", dir=parent) as temporary:
        directory = pathlib.Path(temporary).resolve()
        source, pristine, candidate, restored = [directory / name for name in
                                                  ("source", "pristine", "candidate", "restored")]
        database = source / ".bif/bif.sqlite"
        database.parent.mkdir(parents=True)
        subprocess.run([str(old / "bif-benchmark-store"), "100", "--seed", "2003",
                        "--output", str(database)], check=True, stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, timeout=60)
        run(old / "bif", source, ["init", "--root", str(source), "--requester", "BENCH"])
        capture = ["capture", "Phase D rollback probe", "--project", "phase-d",
                   "--description", "Generated disposable rehearsal only",
                   "--idempotency-key", "phase-d-old-capture"]
        run(old / "bif", source, capture)
        item_id = "BENCH:phase-d:001"
        baseline_get = json.loads(run(old / "bif", source, ["get", item_id, "--json"]).stdout)
        baseline_history = json.loads(run(old / "bif", source, ["history", item_id, "--json"]).stdout)
        baseline = inventory(database)
        if [row[0] for row in baseline["migrations"]] != [1, 2]:
            raise RuntimeError("old executable did not generate schema 2")
        backup(database, pristine / ".bif/bif.sqlite")
        backup(pristine / ".bif/bif.sqlite", candidate / ".bif/bif.sqlite")
        run(new / "bif", candidate, ["init", "--root", str(candidate), "--requester", "BENCH"])
        upgraded = inventory(candidate / ".bif/bif.sqlite")
        if [row[0] for row in upgraded["migrations"]] != [1, 2, 3]:
            raise RuntimeError("candidate did not upgrade schema 3")
        for field in ("store_id", "counts", "integrity", "foreign_keys"):
            if baseline[field] != upgraded[field]:
                raise RuntimeError(f"upgrade changed {field}")
        if json.loads(run(new / "bif", candidate, ["get", item_id, "--json"]).stdout) != baseline_get:
            raise RuntimeError("candidate v1 get differs")
        if json.loads(run(new / "bif", candidate, ["history", item_id, "--json"]).stdout) != baseline_history:
            raise RuntimeError("candidate v1 history differs")
        run(new / "bif", candidate, ["approve", item_id, "--expected-revision", "1",
                                   "--idempotency-key", "phase-d-candidate-approve"])
        candidate_get = json.loads(run(new / "bif", candidate, ["get", item_id, "--json"]).stdout)
        candidate_history = json.loads(run(new / "bif", candidate, ["history", item_id, "--json"]).stdout)
        if candidate_get["revision"] != 2 or candidate_get["status"] != "ready":
            raise RuntimeError("candidate mutation did not survive reopen")
        replay = run(new / "bif", candidate, capture).stdout
        if "replayed: true" not in replay:
            raise RuntimeError("candidate failed old capture replay")
        rejection = run(old / "bif", candidate, ["doctor"], success=False)
        if rejection.returncode == 0 or "newer" not in rejection.stderr:
            raise RuntimeError("old binary unexpectedly accepted upgraded store")
        post_write = inventory(candidate / ".bif/bif.sqlite")
        if inventory(pristine / ".bif/bif.sqlite") != baseline:
            raise RuntimeError("pristine rollback backup was changed")
        backup(pristine / ".bif/bif.sqlite", restored / ".bif/bif.sqlite")
        run(old / "bif", restored, ["init", "--root", str(restored), "--requester", "BENCH"])
        if inventory(restored / ".bif/bif.sqlite") != baseline:
            raise RuntimeError("rollback inventory differs")
        if json.loads(run(old / "bif", restored, ["get", item_id, "--json"]).stdout) != baseline_get:
            raise RuntimeError("old binary restored get differs")
        if json.loads(run(old / "bif", restored, ["history", item_id, "--json"]).stdout) != baseline_history:
            raise RuntimeError("old binary restored history differs")
        if "replayed: true" not in run(old / "bif", restored, capture).stdout:
            raise RuntimeError("old binary restored replay differs")
        run(old / "bif", restored, ["approve", item_id, "--expected-revision", "1",
                                  "--idempotency-key", "phase-d-restored-approve"])
        restored_get = json.loads(run(old / "bif", restored, ["get", item_id, "--json"]).stdout)
        restored_history = json.loads(run(old / "bif", restored, ["history", item_id, "--json"]).stdout)
        if restored_get["revision"] != 2 or restored_get["status"] != "ready":
            raise RuntimeError("restored old-binary mutation did not survive reopen")
        end = source_snapshot()
        if source_inputs["source_inputs_sha256"] != end["source_inputs_sha256"]:
            raise RuntimeError("candidate inputs changed during rehearsal")
        return {
            "format": "bif-phase-d-old-binary-rehearsal-v1",
            "old_revision": args.old_revision,
            "executed_at_unix_seconds": int(time.time()),
            "harness_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
            "candidate_source": source_inputs,
            "candidate_source_end": end,
            "environment": {
                "rust": command(["rustc", "--version"]),
                "cargo": command(["cargo", "--version"]),
                "platform": command(["uname", "-a"]),
                "build_profile": "release",
                **machine_details(directory),
            },
            "candidate_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
            "candidate_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], text=True).strip()),
            "binary_sha256": {
                "old_bif": hashlib.sha256((old / "bif").read_bytes()).hexdigest(),
                "candidate_bif": hashlib.sha256((new / "bif").read_bytes()).hexdigest(),
            },
            "sqlite_backup_version": sqlite3.sqlite_version,
            "fixture_seed": 2003, "fixture_items_before_probe": 100,
            "baseline": baseline, "upgraded_before_writes": upgraded,
            "candidate_after_writes": post_write,
            "restored_after_writes": inventory(restored / ".bif/bif.sqlite"),
            "read_evidence": {
                "baseline_get": baseline_get, "baseline_history": baseline_history,
                "candidate_after_write_get": candidate_get,
                "candidate_after_write_history": candidate_history,
                "restored_after_write_get": restored_get,
                "restored_after_write_history": restored_history,
                "old_binary_new_schema_exit": rejection.returncode,
                "old_binary_new_schema_stderr": rejection.stderr,
            },
            "old_binary_rejects_new_schema": True,
            "candidate_v1_get_history_equivalent": True,
            "candidate_replays_old_capture": True,
            "old_binary_restored_get_history_replay_and_mutation": True,
            "rollback_discards_candidate_writes": True,
            "real_store_touched": False,
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-binaries", type=pathlib.Path, required=True)
    parser.add_argument("--old-revision", required=True)
    parser.add_argument("--binaries", type=pathlib.Path, default=pathlib.Path("target/release"))
    print(json.dumps(rehearse(parser.parse_args()), indent=2))
