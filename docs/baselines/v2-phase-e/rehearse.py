#!/usr/bin/env python3
"""Build exact historical source and rehearse stop/drain/upgrade on disposable roots.

No patched old source, warm writer, live launcher, restore, or sync activation.
Failure produces no successful proof. Missing tools/dependencies are blockers.
"""
import argparse
from contextlib import closing
import hashlib
import json
import os
import pathlib
import platform
import signal
import sqlite3
import subprocess
import tempfile
import time

OLD_REVISION = "d4399a93e49ce8f87e39e3129377821858472807"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def source_inputs(root, historical=False):
    paths = sorted(path for path in root.rglob("*") if path.is_file()) if historical else sorted(
        [*root.joinpath("src").rglob("*.rs"), *root.joinpath("migrations").glob("*.sql"),
         root / "Cargo.toml", root / "Cargo.lock", root / "rust-toolchain.toml"]
    )
    return {str(path.relative_to(root)): digest(path.read_bytes()) for path in paths}


def command(argv, cwd=None, env=None):
    result = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=600)
    record = {"argv": [str(arg) for arg in argv], "cwd": str(cwd) if cwd else None,
              "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
    require(result.returncode == 0, record)
    return record


def build(root, target, env, historical=False):
    before = source_inputs(root, historical)
    rustc = command(["rustc", "--version", "--verbose"], root, env)["stdout"].strip()
    cargo = command(["cargo", "--version"], root, env)["stdout"].strip()
    require("rustc 1.94.0 " in rustc and "cargo 1.94.0 " in cargo, "exact toolchain unavailable")
    toolchain = command(["rustup", "show", "active-toolchain"], root, env)["stdout"].strip()
    executables = {}
    for name in ["rustc", "cargo"]:
        path = command(["rustup", "which", name], root, env)["stdout"].strip()
        executables[name] = {"path": path, "sha256": digest(pathlib.Path(path).read_bytes())}
    result = command(["cargo", "build", "--locked", "--release", "--bin", "bif",
                      "--target-dir", str(target)], root, env)
    after = source_inputs(root, historical)
    require(before == after, "source changed during build")
    binary = target / "release/bif"
    return binary, {
        "binary_sha256": digest(binary.read_bytes()),
        "source_sha256": digest(canonical(before)), "source_inputs_sha256": before,
        "lock_sha256": before["Cargo.lock"],
        "toolchain_sha256": before["rust-toolchain.toml"],
        "rustc": rustc, "cargo": cargo, "active_toolchain": toolchain,
        "toolchain_executables": executables,
        "build": {**result, "locked": True, "separate_target": True},
    }


def encoded(value):
    return {"blob_hex": value.hex()} if isinstance(value, bytes) else value


def snapshot(database):
    """Full table values, catalog and persistent pragmas; not a physical-byte backup."""
    with closing(sqlite3.connect(database.as_uri() + "?mode=rw", uri=True)) as connection:
        schema = {
            "catalog": connection.execute(
                "SELECT type,name,tbl_name,rootpage,sql FROM sqlite_schema ORDER BY type,name"
            ).fetchall(),
            "persistent_pragmas": {
                name: connection.execute("PRAGMA " + name).fetchone()[0]
                for name in ["schema_version", "user_version", "application_id",
                             "page_size", "auto_vacuum", "encoding", "journal_mode"]
            },
        }
        tables = {}
        for name, in connection.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name"):
            quoted = '"' + name.replace('"', '""') + '"'
            rows = [[encoded(cell) for cell in row] for row in connection.execute("SELECT * FROM " + quoted)]
            tables[name] = sorted(rows, key=lambda row: canonical(row))
        integrity = connection.execute("PRAGMA integrity_check").fetchall()
        foreign_keys = connection.execute("PRAGMA foreign_key_check").fetchall()
        require(integrity == [("ok",)] and not foreign_keys, "database integrity check failed")
        return {"schema": schema, "tables": tables,
                "schema_sha256": digest(canonical(schema)),
                "content_sha256": digest(canonical(tables)),
                "migrations": [row[0] for row in tables["schema_migrations"]] if "schema_migrations" in tables else [],
                "integrity": "ok", "foreign_keys": foreign_keys}


def summary(state):
    return {key: state[key] for key in
            ["schema_sha256", "content_sha256", "migrations", "integrity", "foreign_keys"]}


class MaintenanceBlocked(RuntimeError):
    pass


class Launcher:
    """Disposable launcher only; its maintenance check is not a live service fence.

    The harness owns all launches serially. This gate has no atomicity guarantee
    against outside launchers or direct executable/SQL access.
    """
    def __init__(self, sentinel):
        self.sentinel = sentinel

    def start(self, argv, **kwargs):
        if self.sentinel.exists():
            raise MaintenanceBlocked("disposable launcher maintenance window")
        return subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **kwargs)


def validate_order(events):
    names = [event["event"] for event in events]
    expected = ["old_writer_stopped", "maintenance_enabled", "old_writer_continued",
                "old_writer_reaped", "candidate_migration_started",
                "candidate_migration_finished", "maintenance_disabled"]
    positions = [names.index(name) for name in expected]
    require(positions == sorted(positions), "migration must follow drain inside restart-prevention window")


def rehearse(checkout, directory):
    env = {key: value for key, value in os.environ.items()
           if key not in ("BIF_ROOT", "BIF_CONFIG", "BIF_REQUESTER", "CARGO_TARGET_DIR")}
    historical_root = directory / "historical-source"
    historical_root.mkdir()
    archive = directory / "historical.tar"
    archive_command = command(
        ["git", "archive", "--format=tar", "--output", str(archive), OLD_REVISION], checkout, env)
    extraction_command = command(["tar", "-xf", str(archive), "-C", str(historical_root)], checkout, env)
    old, historical = build(historical_root, directory / "historical-target", env, historical=True)
    original_inputs = source_inputs(historical_root, historical=True)
    historical.update({"revision": OLD_REVISION, "source_method": "git archive",
                       "source_modified": False, "archive_sha256": digest(archive.read_bytes()),
                       "archive_command": archive_command, "extraction_command": extraction_command})
    candidate, candidate_provenance = build(checkout, directory / "candidate-target", env)
    candidate_inputs = source_inputs(checkout)
    candidate_provenance.update({
        "revision": command(["git", "rev-parse", "HEAD"], checkout, env)["stdout"].strip(),
        "git_status_porcelain": command(["git", "status", "--porcelain"], checkout, env)["stdout"],
    })
    root = directory / "disposable-store"
    root.mkdir()
    config = root / "config.toml"
    database = root / ".bif/bif.sqlite"
    sentinel = directory / "launcher-maintenance"
    launcher = Launcher(sentinel)
    commands, events, blocked = [], [], []

    def argv(binary, arguments):
        return [str(binary), *arguments, "--config", str(config)]

    def invoke(binary, arguments):
        record = command(argv(binary, arguments), checkout, env)
        commands.append(record)
        return record

    def event(name, **fields):
        events.append({"sequence": len(events), "event": name, **fields})

    def blocked_restart(stage):
        try:
            process = launcher.start(argv(old, capture), env=env)
        except MaintenanceBlocked:
            blocked.append({"stage": stage, "sentinel_present": sentinel.exists(), "process_spawned": False})
            event("restart_blocked", stage=stage)
        else:
            process.kill()
            process.communicate()
            raise RuntimeError("launcher restarted old writer during maintenance")

    invoke(old, ["init", "--root", str(root), "--requester", "PROOF"])
    require(snapshot(database)["migrations"] == [1, 2, 3], "historical init did not create schema 3")
    capture = ["capture", "Authentic historical writer drain", "--project", "phase-e",
               "--description", "Disposable schema-3 one-shot CLI process",
               "--acceptance", "complete persisted content survives upgrade",
               "--idempotency-key", "phase-e-historical-capture"]
    # A real SQLite lock prevents completion before the OS stop is acknowledged.
    # SIGSTOP + waitpid(WUNTRACED) is a deterministic process control, not a sleep
    # or an assertion that the old process has reached a particular SQL statement.
    with closing(sqlite3.connect(database)) as lock:
        lock.execute("BEGIN IMMEDIATE")
        writer = launcher.start(argv(old, capture), env=env)
        try:
            os.kill(writer.pid, signal.SIGSTOP)
            pid, status = os.waitpid(writer.pid, os.WUNTRACED)
            require(pid == writer.pid and os.WIFSTOPPED(status), "old writer did not stop")
            event("old_writer_stopped", pid=pid, stop_signal=os.WSTOPSIG(status),
                  boundary="exec completed; SQLite writer lock held; no claimed SQL readiness")
            sentinel.write_text("Phase E disposable maintenance only\n")
            event("maintenance_enabled")
            blocked_restart("before_drain")
            lock.rollback()
            os.kill(writer.pid, signal.SIGCONT)
            event("old_writer_continued", pid=pid)
            stdout, stderr = writer.communicate(timeout=30)
            require(writer.returncode == 0, {"stdout": stdout, "stderr": stderr})
            # communicate waits/reaps the actual historical child.
            event("old_writer_reaped", pid=pid, exit_code=writer.returncode)
            commands.append({"argv": argv(old, capture), "exit_code": writer.returncode,
                             "stdout": stdout, "stderr": stderr, "mode": "controlled one-shot CLI"})
        finally:
            if writer.returncode is None:
                writer.kill()
                writer.communicate(timeout=30)
            lock.rollback()
    blocked_restart("after_drain")
    baseline = snapshot(database)
    require(len(baseline["tables"]["items"]) == 1, "real old writer did not persist item")
    item_id = "PROOF:phase-e:001"
    blocked_restart("before_migration")
    event("candidate_migration_started")
    invoke(candidate, ["doctor"])
    upgraded = snapshot(database)
    event("candidate_migration_finished")
    require(upgraded["migrations"] == [1, 2, 3, 4], "candidate failed schema-4 migration")
    for table, rows in baseline["tables"].items():
        if table == "schema_migrations":
            require(upgraded["tables"][table][:-1] == rows, "migration changed historical migration records")
        else:
            require(upgraded["tables"][table] == rows, "migration changed legacy table " + table)
    candidate_get = json.loads(invoke(candidate, ["get", item_id, "--json"])["stdout"])
    candidate_history = json.loads(invoke(candidate, ["history", item_id, "--json"])["stdout"])
    require(candidate_get["revision"] == 1, "candidate could not read historical item")
    require("replayed: true" in invoke(candidate, capture)["stdout"], "old capture receipt failed replay")
    blocked_restart("after_migration")
    invoke(candidate, ["capture", "Candidate journal capture", "--project", "phase-e",
                       "--idempotency-key", "phase-e-candidate-capture"])
    invoke(candidate, ["approve", item_id, "--expected-revision", "1",
                       "--idempotency-key", "phase-e-candidate-approve"])
    final = snapshot(database)
    metadata = final["tables"]["journal_metadata"][0]
    journal = dict(zip(["singleton", "generation", "retention_floor", "high_water",
                        "sync_enabled", "bootstrap_required"], metadata))
    require((journal["high_water"], journal["sync_enabled"], journal["bootstrap_required"]) == (2, 0, 1),
            "unexpected journal coverage or activation")
    require(len(final["tables"]["change_journal"]) == 2, "missing effective-write journal links")
    blocked_restart("after_candidate_writes")
    sentinel.unlink()
    event("maintenance_disabled")
    rejections = []
    for arguments in [["doctor"], ["get", item_id, "--json"],
                      ["capture", "Must reject", "--project", "phase-e",
                       "--idempotency-key", "phase-e-rejected-old-write"]]:
        before = snapshot(database)
        process = launcher.start(argv(old, arguments), env=env)
        try:
            stdout, stderr = process.communicate(timeout=30)
        finally:
            if process.returncode is None:
                process.kill()
                process.communicate(timeout=30)
        require(process.returncode != 0 and "newer" in stderr, "fresh old open did not reject newer schema")
        after = snapshot(database)
        require(before == after == final, "old rejection altered full persisted content or schema")
        rejections.append({"argv": argv(old, arguments), "pid": process.pid,
                           "exit_code": process.returncode, "stdout": stdout, "stderr": stderr,
                           "before": summary(before), "after": summary(after)})
    validate_order(events)
    require(source_inputs(historical_root, historical=True) == original_inputs, "historical source modified")
    require(source_inputs(checkout) == candidate_inputs, "candidate inputs changed during rehearsal")
    for binary, provenance in [(old, historical), (candidate, candidate_provenance)]:
        provenance["binary_end_sha256"] = digest(binary.read_bytes())
        require(provenance["binary_end_sha256"] == provenance["binary_sha256"], "executable changed during rehearsal")
    historical["source_end_sha256"] = digest(canonical(source_inputs(historical_root, historical=True)))
    candidate_provenance["source_end_sha256"] = digest(canonical(source_inputs(checkout)))
    return {
        "format": "bif-phase-e-upgrade-proof-v1",
        "executed_at_unix_seconds": int(time.time()),
        "harness_sha256": digest(pathlib.Path(__file__).read_bytes()),
        "control_tests_sha256": digest(pathlib.Path(__file__).with_name("test_rehearse.py").read_bytes()),
        "historical": historical, "candidate": candidate_provenance,
        "environment": {"platform": platform.platform(), "uname": list(platform.uname()),
                        "python": platform.python_version(), "sqlite": sqlite3.sqlite_version},
        "process_mode": {"writer": "one-shot CLI", "rpc": "historical one-shot; not exercised",
                         "warm_mcp": "read-only; not exercised", "warm_writer_fencing_proven": False},
        "events": events, "blocked_restarts": blocked, "commands": commands,
        "baseline": baseline, "upgraded": upgraded, "final_state": final,
        "journal": journal, "legacy_rows_preserved": True,
        "candidate_get": candidate_get, "candidate_history": candidate_history,
        "rejections": rejections,
        "real_store_touched": False, "operator_inventory_replaced": False, "release_approved": False,
        "limitations": [
            "Only disposable serial launcher gate; no live service/launcher inventory or enforcement.",
            "Fresh old opens reject; no retroactive guard or hypothetical warm old writer fencing.",
            "Historical writes are one-shot CLI/RPC; MCP warm process is read-only.",
            "Current macOS host only; no configured MCP host or other-platform validation for Phase E.",
            "No host crash, power-loss, fsync/durable-backup or disaster-recovery proof.",
            "Direct SQL and out-of-band writers are unsupported and not fenced by this launcher.",
            "No sync activation, restore, prune, generation rotation, Phase F or live-store operation.",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    require(os.name == "posix" and hasattr(os, "WUNTRACED"), "POSIX process controls required")
    checkout = pathlib.Path(__file__).resolve().parents[3]
    scratch = os.environ.get("DELTA_SCRATCH_DIR")
    with tempfile.TemporaryDirectory(prefix="bif-phase-e-", dir=scratch) as temporary:
        proof = rehearse(checkout, pathlib.Path(temporary).resolve())
    args.output.write_text(json.dumps(proof, indent=2) + "\n")
    print(json.dumps({"proof": str(args.output), "sha256": digest(args.output.read_bytes()),
                      "historical_binary_sha256": proof["historical"]["binary_sha256"],
                      "candidate_binary_sha256": proof["candidate"]["binary_sha256"],
                      "result": "passed; operational release gates remain pending"}, indent=2))


if __name__ == "__main__":
    main()
