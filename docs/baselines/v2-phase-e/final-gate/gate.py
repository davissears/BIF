#!/usr/bin/env python3
"""Disposable Phase E foundation gate. Runtime evidence is manual, validation is CI."""
import argparse
from contextlib import closing
import hashlib
import json
import os
import pathlib
import platform
import random
import re
import sqlite3
import statistics as stats
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[3]
sys.path.insert(0, str(HERE.parent))
import rehearse as archived

CANDIDATE = "f0cd787f64a6bd3d301f11e616bb587e112a14fa"
OLD = archived.OLD_REVISION
# Accepted Phase E artifact bytes, independently pinned from validation-results.json.
# A mutable manifest beside the evidence cannot authorize replacement artifacts.
HISTORICAL_ARTIFACT_SHA256 = {
    "proof.json": "70433c66156438d1eed31dab2120fc679e1207ea8cb759c68cc690845312a3a9",
    "protocol.json": "dff0bd5e5413a0a4dfcb2eda0b23494fd1d5189047ecf572f2546cb13ea0be5e",
    "measurements.json": "9053432b4053cb9a25a0df726b7b8c24bf4f276556edc62aed918473d3a21d21",
    "upgrade-proof.json": "911f74b83de70469fe3f8857cda102c75346794fb876b7e76b2965394f197f0b",
}
KINDS = ["capture", "effective", "note_only", "compound", "replay", "genuine_noop"]
SIZES = [100, 10000, 100000]
LEGACY_TABLES = (
    "store_metadata", "projects", "requester_project_counters", "items",
    "item_acceptance_criteria", "item_provenance", "operations", "events",
)
require = archived.require
digest = archived.digest
canonical = archived.canonical
command = archived.command
snapshot = archived.snapshot

# Frozen verbatim in the protocol before any measured command. Templates and
# effects are shared by generation and validation; samples never supply state.
WORKLOAD_SPEC = {
    "version": 1, "requester": "GATE", "target": "GATE:gate:001",
    "first_capture_sequence": 2,
    "initial_state": {"revision": 2, "priority": "P2", "assignee": "unassigned"},
    "setup": [
        {"args": ["capture", "Measurement target", "--project", "gate",
                  "--idempotency-key", "measure-target"],
         "output": {"Captured": "GATE:gate:001", "title": "Measurement target",
                    "status": "proposed", "replayed": "false", "source": "local"}},
        {"args": ["triage", "GATE:gate:001", "--expected-revision", "1",
                  "--idempotency-key", "measure-initial", "--priority", "P2"],
         "output": {"Updated": "GATE:gate:001", "status": "proposed", "priority": "P2",
                    "assignee": "unassigned", "revision": "2", "replayed": "false"}},
    ],
    "workloads": {
        "capture": {"args": ["capture", "Measured {index}", "--project", "gate",
                             "--idempotency-key", "measure-capture-{index}"],
                    "revision_delta": 0, "journal_delta": 1},
        "effective": {"args": ["--priority", "{toggle}"], "revision_delta": 1, "journal_delta": 1},
        "note_only": {"args": ["--note", "Measured note {index}"], "revision_delta": 1, "journal_delta": 1},
        "compound": {"args": ["--priority", "{toggle}", "--assignee", "OWNER{index}",
                              "--note", "Compound note {index}"], "revision_delta": 1, "journal_delta": 1},
        "replay": {"args": ["capture", "Measurement target", "--project", "gate",
                            "--idempotency-key", "measure-target"],
                   "revision_delta": 0, "journal_delta": 0},
        "genuine_noop": {"args": ["--priority", "{priority}"], "revision_delta": 0, "journal_delta": 0},
    },
}


def workload_step(kind, index, state):
    """Derive one command and result from ordered expected successes, not rows."""
    rule = WORKLOAD_SPEC["workloads"][kind]
    values = {**state, "index": index, "toggle": "P1" if state["priority"] != "P1" else "P2"}
    args = [arg.format(**values) for arg in rule["args"]]
    after = {**state, "revision": state["revision"] + rule["revision_delta"]}
    if kind in ("capture", "replay"):
        replay = kind == "replay"
        item = (WORKLOAD_SPEC["target"] if replay else
                f'GATE:gate:{WORKLOAD_SPEC["first_capture_sequence"] + index:03d}')
        output = {"Captured": item, "title": args[1], "status": "proposed",
                  "replayed": str(replay).lower(), "source": "local"}
    else:
        args = ["triage", WORKLOAD_SPEC["target"], "--expected-revision", str(state["revision"]),
                "--idempotency-key", f"measure-{kind}-{index}", *args]
        if "--priority" in args:
            after["priority"] = args[args.index("--priority") + 1]
        if "--assignee" in args:
            after["assignee"] = args[args.index("--assignee") + 1].lower()
        output = {"Updated": WORKLOAD_SPEC["target"], "status": "proposed",
                  "priority": after["priority"], "assignee": after["assignee"],
                  "revision": str(after["revision"]), "replayed": "false"}
    return args, output, after


def cli_argv(binary, root, args):
    return [str(binary), *args, "--config", str(root / "config.toml"),
            "--root", str(root), "--requester", WORKLOAD_SPEC["requester"]]


def validate_command(result, context, args, expected, label):
    """Parse the existing human CLI output; no production evidence mode."""
    require(result["argv"] == cli_argv(context["executable"], pathlib.PurePosixPath(context["root"]), args),
            "semantic argv: " + label)
    require(result["cwd"] == context["cwd"], "semantic cwd: " + label)
    require(result["exit_code"] == 0 and result["stderr"] == "", "semantic success: " + label)
    lines = result["stdout"].splitlines()
    require(bool(lines), "semantic output: " + label)
    header, separator, item = lines[0].partition(" ")
    require(separator and header in ("Captured", "Updated"), "semantic output: " + label)
    fields = {header: item}
    for line in lines[1:]:
        key, separator, value = line.partition(": ")
        require(separator and key not in fields, "semantic output: " + label)
        fields[key] = value
    require(fields == expected, "semantic output: " + label)


def freeze_execution(fixture, old, candidate, upgrade):
    """Read disposable configs once before protocol creation, never during CI."""
    execution = {}
    for role, binary, source in [("baseline", old, "historical"), ("candidate", candidate, "candidate")]:
        root = pathlib.Path(fixture["roots"][role])
        config = root / "config.toml"
        contents = config.read_bytes().decode("utf-8")
        require(digest(binary.read_bytes()) == upgrade[source]["binary_sha256"], "binary changed before freeze")
        execution[role] = {
            "executable": str(binary), "binary_sha256": upgrade[source]["binary_sha256"],
            "root": str(root), "config_path": str(config), "config_contents": contents,
            "config_sha256": digest(contents.encode()), "requester": WORKLOAD_SPEC["requester"],
            "cwd": str(ROOT),
        }
    return execution


def validate_execution(protocol, fixture):
    """Bind roles to recorded build targets and fixture/config identities only."""
    require(set(fixture["roots"]) == set(fixture["execution"]) == {"baseline", "candidate"},
            "semantic context roles")
    targets = {}
    for role, target_name in [("baseline", "historical-target"), ("candidate", "candidate-target")]:
        build = protocol[role + "_build"]
        argv = build["build"]["argv"]
        require(len(argv) == 8 and argv[:7] ==
                ["cargo", "build", "--locked", "--release", "--bin", "bif", "--target-dir"],
                "semantic build command")
        target = pathlib.PurePosixPath(argv[-1])
        require(target.is_absolute() and ".." not in target.parts and target.name == target_name
                and str(target) == argv[-1] and build["build"]["exit_code"] == 0, "semantic build target")
        targets[role] = target
    require(targets["baseline"].parent == targets["candidate"].parent, "semantic build isolation")
    directory = targets["baseline"].parent
    cwd = protocol["candidate_build"]["build"]["cwd"]
    require(pathlib.PurePosixPath(cwd).is_absolute(), "semantic build cwd")
    size = fixture["items"]
    # init's config is copied unchanged into the two isolated working roots.
    config = f'root = "{directory}/benchmark-{size}-baseline"\nrequester = "GATE"\n'
    for role, root_name in [("baseline", f"working-{size}-baseline"),
                            ("candidate", f"benchmark-{size}-candidate")]:
        root = directory / root_name
        expected = {
            "executable": str(targets[role] / "release/bif"),
            "binary_sha256": protocol[role + "_build"]["binary_sha256"],
            "root": str(root), "config_path": str(root / "config.toml"),
            "config_contents": config, "config_sha256": digest(config.encode()),
            "requester": WORKLOAD_SPEC["requester"], "cwd": cwd,
        }
        require(fixture["execution"][role] == expected and fixture["roots"][role] == str(root),
                "semantic context: " + role)


def validate_semantics(protocol, measurements):
    """Pure offline semantic control, also called by the complete gate."""
    require(protocol.get("workload_spec") == WORKLOAD_SPEC and protocol["workload_order"] == KINDS
            and protocol["warmups"] == 5 and protocol["paired_samples"] == 30, "semantic protocol")
    require([row["items"] for row in protocol["fixtures"]] ==
            [row["items"] for row in measurements["fixtures"]] == SIZES, "semantic fixture order")
    for fixture, measured in zip(protocol["fixtures"], measurements["fixtures"]):
        validate_execution(protocol, fixture)
        contexts = fixture["execution"]
        require(set(measured["setup"]) == {"baseline", "candidate"}, "semantic setup roles")
        for role in contexts:
            require(len(measured["setup"][role]) == len(WORKLOAD_SPEC["setup"]), "semantic setup count")
            for result, step in zip(measured["setup"][role], WORKLOAD_SPEC["setup"]):
                validate_command(result, contexts[role], step["args"], step["output"], role + " setup")
        require(set(measured["workloads"]) == set(KINDS), "semantic workload coverage")
        state = dict(WORKLOAD_SPEC["initial_state"])
        for kind in KINDS:
            workload = measured["workloads"][kind]
            require(len(workload["warmups"]) == 5 and len(workload["raw_samples"]) == 30,
                    "semantic sample count: " + kind)
            for index, row in enumerate(workload["warmups"] + workload["raw_samples"]):
                args, output, after = workload_step(kind, index, state)
                label = f'{fixture["items"]} {kind} {index}'
                require(row["pair"] == index and row["order"] ==
                        (["baseline", "candidate"] if index % 2 == 0 else ["candidate", "baseline"]),
                        "semantic pair order: " + label)
                require(row["candidate_journal_delta"] == WORKLOAD_SPEC["workloads"][kind]["journal_delta"],
                        "semantic journal: " + label)
                require(set(row["commands"]) == {"baseline", "candidate"}, "semantic command roles")
                for role in contexts:
                    validate_command(row["commands"][role], contexts[role], args, output, role + " " + label)
                state = after


def write(path, value):
    """Evidence files are append-only per run: never replace an archived result."""
    with path.open("x") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


def statistics(samples):
    """Seeded paired percentile bootstrap of median candidate/baseline ratios."""
    require(len(samples) == 30, "exactly 30 paired measured samples required")
    ratios = [row["candidate_ns"] / row["baseline_ns"] for row in samples]
    rng = random.Random(2003)
    draws = sorted(stats.median(rng.choices(ratios, k=30)) for _ in range(10000))
    interval = [draws[249], draws[9749]]
    median = stats.median(ratios)
    disposition = ("explanation_review_required" if median >= 1.10 and interval[0] > 1
                   else "inconclusive_repeat" if median >= 1.10 else "no_trigger")
    return {
        "baseline_median_ns": stats.median(row["baseline_ns"] for row in samples),
        "candidate_median_ns": stats.median(row["candidate_ns"] for row in samples),
        "baseline_p95_ns": sorted(row["baseline_ns"] for row in samples)[28],
        "candidate_p95_ns": sorted(row["candidate_ns"] for row in samples)[28],
        "paired_median_ratio": median, "paired_ratio_ci95": interval,
        "disposition": disposition,
    }


def harness_inputs():
    paths = [HERE / "gate.py", HERE / "test_gate.py", HERE.parent / "rehearse.py",
             HERE.parent / "test_rehearse.py", ROOT / "tests/phase_e_final_gate.rs",
             ROOT / "tests/read_release_manifest.rs",
             ROOT / ".github/workflows/ci.yml", ROOT / ".github/workflows/phase-e-evidence.yml"]
    paths += sorted(ROOT.joinpath("tests").rglob("*.rs"))
    return {str(path.relative_to(ROOT)): digest(path.read_bytes()) for path in sorted(set(paths))}


def copy_database(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("xb"):
        pass
    with closing(sqlite3.connect(source.as_uri() + "?mode=rw", uri=True)) as src:
        with closing(sqlite3.connect(destination)) as dst:
            src.backup(dst)


def db(root):
    return root / ".bif/bif.sqlite"


def cli(binary, root, args, env):
    return command(cli_argv(binary, root, args), ROOT, env)


def generation(state):
    return state["tables"]["journal_metadata"][0][1]


def restored_tables(state):
    tables = json.loads(json.dumps(state["tables"]))
    tables["journal_metadata"][0][1] = "<fresh-generation>"
    return tables


def correctness(directory, old, candidate, env):
    """Real CLI operations only; SQL reads inspect evidence, never perform writes."""
    commands = []
    fresh = directory / "fresh"
    fresh.mkdir()
    commands.append(cli(candidate, fresh, ["init"], env))
    fresh_state = snapshot(db(fresh))
    require(fresh_state["migrations"] == [1, 2, 3, 4], "fresh migrations")
    root = directory / "restore-source"
    root.mkdir()
    commands.append(cli(old, root, ["init"], env))
    capture = ["capture", "Historical rollback item", "--project", "gate",
               "--idempotency-key", "gate-historical"]
    commands.append(cli(old, root, capture, env))
    legacy = snapshot(db(root))
    pristine = directory / "schema3-pristine.sqlite"
    copy_database(db(root), pristine)
    commands.append(cli(candidate, root, ["doctor"], env))
    upgraded = snapshot(db(root))
    require(upgraded["migrations"] == [1, 2, 3, 4], "upgrade migrations")
    commands.append(cli(candidate, root, ["capture", "Current backup journal row", "--project", "gate",
                                         "--idempotency-key", "gate-before-backup"], env))
    backup = directory / "schema4-backup.sqlite"
    commands.append(command([str(candidate), "maintenance", "backup", "--store", str(db(root)),
                             "--output", str(backup), "--offline"], ROOT, env))
    backup_state = snapshot(backup)
    backup_hash = digest(backup.read_bytes())
    commands.append(cli(candidate, root, ["capture", "Preserved later write", "--project", "gate",
                                         "--idempotency-key", "gate-later"], env))
    later = snapshot(db(root))
    preserved = []
    generations = [generation(backup_state)]
    for index in range(2):
        before = snapshot(db(root))
        output = directory / f"preserved-later-{index}.sqlite"
        commands.append(command([str(candidate), "maintenance", "restore", "--store", str(db(root)),
                                 "--from", str(backup), "--preserve-later", str(output),
                                 "--offline"], ROOT, env))
        saved = snapshot(output)
        after = snapshot(db(root))
        require(saved["tables"] == before["tables"], "later store not preserved")
        require(restored_tables(after) == restored_tables(backup_state), "restore changed backup content")
        require(generation(after) not in generations, "restore reused generation")
        generations.append(generation(after))
        preserved.append({"path": str(output), "sha256": digest(output.read_bytes()),
                          "state": archived.summary(saved), "tables_equal_before": True})
    require(digest(backup.read_bytes()) == backup_hash, "pristine input was modified")
    require(later["tables"] == snapshot(directory / "preserved-later-0.sqlite")["tables"],
            "later writes lost from preservation")
    # Make two operation-linked rows after reset, then retain only the second.
    for index in range(2):
        commands.append(cli(candidate, root, ["capture", f"Retained {index}", "--project", "gate",
                                             "--idempotency-key", f"gate-retain-{index}"], env))
    before_prune = snapshot(db(root))
    metadata = before_prune["tables"]["journal_metadata"][0]
    through = metadata[3] - 1
    diagnostic_argv = ["sqlite3", "-readonly", str(db(root)),
                      "SELECT generation, retention_floor AS F, high_water AS H FROM journal_metadata WHERE singleton = 1;"]
    result = subprocess.run(diagnostic_argv, capture_output=True, text=True, timeout=30)
    diagnostic = {"argv": diagnostic_argv, "exit_code": result.returncode, "stdout": result.stdout,
                  "stderr": result.stderr, "filesystem": command(["df", "-h", str(root)])["stdout"],
                  "sqlite_cli": command(["sqlite3", "--version"])["stdout"],
                  "disposition": "passed" if result.returncode == 0 else "blocked_on_this_filesystem",
                  "live_configuration_workaround": False}
    # The caller stops before benchmarks if this exact documented diagnostic is unusable.
    if result.returncode != 0:
        write(directory / "diagnostic-blocker.json", diagnostic)
        raise RuntimeError({"material_blocker": diagnostic,
                            "message": "Exact read-only diagnostic failed; report before continuing."})
    commands.append(command([str(candidate), "maintenance", "prune", "--store", str(db(root)),
                             "--expected-generation", generation(before_prune),
                             "--through", str(through), "--offline"], ROOT, env))
    after_prune = snapshot(db(root))
    for table in before_prune["tables"]:
        if table not in ("change_journal", "journal_metadata"):
            require(before_prune["tables"][table] == after_prune["tables"][table],
                    "prune altered " + table)
    journal = after_prune["tables"]["change_journal"]
    require(len(journal) == 1 and journal[0][0] == metadata[3], "retained coverage")
    require(after_prune["tables"]["journal_metadata"][0] ==
            [metadata[0], metadata[1], through, metadata[3], 0, 1], "prune metadata")
    rollback = directory / "schema3-rollback"
    copy_database(pristine, db(rollback))
    (rollback / "config.toml").write_bytes((root / "config.toml").read_bytes())
    rolled_back = snapshot(db(rollback))
    require(rolled_back["tables"] == legacy["tables"], "historical rollback content")
    for args in [["doctor"], ["get", "GATE:gate:001", "--json"],
                 ["history", "GATE:gate:001", "--json"], capture]:
        commands.append(cli(old, rollback, args, env))
    require(snapshot(db(rollback))["tables"] == legacy["tables"], "old rollback replay changed content")
    require(len(later["tables"]["items"]) > len(rolled_back["tables"]["items"]), "rollback loss not shown")
    return {
        "fresh": fresh_state, "upgrade": upgraded,
        "historical_rollback": {"separate_root": True, "migrations": rolled_back["migrations"],
                                "full_legacy_tables_equal": True, "later_write_absent": True,
                                "later_write_preserved_separately": True},
        "current_schema_restore": {"generations": generations, "repeated_identical_backup": True,
                                   "input_sha256_unchanged": backup_hash,
                                   "restored_tables_equal_except_generation": True,
                                   "preserved_later": preserved},
        "prune": {"before": before_prune, "after": after_prune,
                  "audit_receipts_and_allocator_unchanged": True, "retained_coverage_verified": True},
        "diagnostic": diagnostic, "commands": commands,
    }


def legacy_digest(database):
    """Require every canonical fixture table and stream its deterministic logical rows."""
    hashes = {}
    with closing(sqlite3.connect(database)) as connection:
        for table in LEGACY_TABLES:
            exists = connection.execute(
                "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?", [table]).fetchone()[0]
            require(exists, "missing required fixture table: " + table)
            count = connection.execute(f'PRAGMA table_info("{table}")').fetchall()
            order = ",".join(str(index + 1) for index in range(len(count)))
            hasher = hashlib.sha256()
            for row in connection.execute(f'SELECT * FROM "{table}" ORDER BY {order}'):
                hasher.update(canonical([archived.encoded(cell) for cell in row]) + b"\n")
            hashes[table] = hasher.hexdigest()
    return hashes


def validate_fixture_digest(fixture):
    """Recorded equality cannot stand in for complete required-table coverage."""
    require(set(fixture["legacy_table_sha256"]) == set(LEGACY_TABLES), "fixture digest tables")


def sizes(database):
    return {"main_db_bytes": database.stat().st_size,
            "wal_bytes": pathlib.Path(str(database) + "-wal").stat().st_size
            if pathlib.Path(str(database) + "-wal").exists() else 0}

def sqlite_version(bindings):
    version = re.search(r'pub const SQLITE_VERSION:.*?[bc]"([^"\\]+)', bindings)
    require(version is not None, "SQLite version missing from generated bindings")
    return version.group(1)


def sqlite_build_provenance(directory):
    """Record the bundled dependency actually compiled, not the system CLI version."""
    provenance = {}
    for name, target in [("baseline", "historical-target"), ("candidate", "candidate-target")]:
        bindings = list((directory / target / "release/build").glob("libsqlite3-sys-*/out/bindgen.rs"))
        require(len(bindings) == 1, "expected one bundled SQLite bindings output")
        data = bindings[0].read_text()
        provenance[name] = {"version": sqlite_version(data), "bindings_path": str(bindings[0]),
                            "bindings_sha256": digest(bindings[0].read_bytes()),
                            "dependency": "libsqlite3-sys 0.36.0 bundled; rusqlite 0.38.0"}
    return provenance


def prepare(directory, old, candidate, env):
    generator = directory / "historical-target/release/bif-benchmark-store"
    generator_build = command(["cargo", "build", "--locked", "--release", "--bin",
                               "bif-benchmark-store", "--target-dir",
                               str(directory / "historical-target")],
                              directory / "historical-source", env)
    fixtures = []
    for size in SIZES:
        base = directory / f"benchmark-{size}-baseline"
        target = directory / f"benchmark-{size}-candidate"
        base.mkdir()
        cli(old, base, ["init"], env)
        generated = directory / f"generated-{size}.sqlite"
        generation_command = command([str(generator), str(size), "--seed", "2003",
                                      "--output", str(generated)], ROOT, env)
        # init only supplies isolated config; copy into another new root, not over its DB.
        working_base = directory / f"working-{size}-baseline"
        working_base.mkdir()
        (working_base / "config.toml").write_bytes((base / "config.toml").read_bytes())
        copy_database(generated, db(working_base))
        target.mkdir()
        (target / "config.toml").write_bytes((base / "config.toml").read_bytes())
        copy_database(generated, db(target))
        cli(candidate, target, ["doctor"], env)
        baseline_hashes, candidate_hashes = legacy_digest(db(working_base)), legacy_digest(db(target))
        require(baseline_hashes == candidate_hashes, "fixtures not logically comparable")
        metadata = json.loads(pathlib.Path(str(generated) + ".metadata.json").read_text())
        fixtures.append({"items": size, "seed": 2003, "logical_digest": metadata["logical_digest"],
                         "logical_digest_algorithm": metadata["logical_digest_algorithm"],
                         "legacy_table_sha256": baseline_hashes, "legacy_tables_equal": True,
                         "generator_command": generation_command,
                         "roots": {"baseline": str(working_base), "candidate": str(target)}})
    return fixtures, {"build": generator_build, "sha256": digest(generator.read_bytes())}


def open_observer(database):
    """Establish SQLite/WAL access symmetrically, without retaining a read transaction."""
    connection = sqlite3.connect(database)
    connection.execute("SELECT count(*) FROM sqlite_schema").fetchone()
    return connection


def measure(fixture, old, candidate, env):
    roots = {name: pathlib.Path(path) for name, path in fixture["roots"].items()}
    binaries = {"baseline": old, "candidate": candidate}
    keepers = {name: open_observer(db(root)) for name, root in roots.items()}
    results, setup = {}, {}
    state = dict(WORKLOAD_SPEC["initial_state"])
    try:
        for name, root in roots.items():
            setup[name] = []
            for step in WORKLOAD_SPEC["setup"]:
                result = cli(binaries[name], root, step["args"], env)
                validate_command(result, fixture["execution"][name], step["args"], step["output"], name + " setup")
                setup[name].append(result)
            require(keepers[name].execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0] == 0,
                    "initial checkpoint busy")
        for kind in KINDS:
            observations = {name: {"before_workload": sizes(db(root))}
                            for name, root in roots.items()}
            warmups, samples = [], []
            for index in range(35):
                order = ["baseline", "candidate"] if index % 2 == 0 else ["candidate", "baseline"]
                args, output, after = workload_step(kind, index, state)
                for name, root in roots.items():
                    # Retain the original untimed get/preconditioning, but do not
                    # let a sample's reported state choose the workload.
                    item = json.loads(cli(binaries[name], root, ["get", "GATE:gate:001", "--json"], env)["stdout"])
                    require(item["revision"] == state["revision"] and item["priority"] == state["priority"],
                            "measurement state differs from frozen setup/sequence")
                row = {"pair": index, "order": order, "commands": {}}
                connection = keepers["candidate"]
                before_h = connection.execute("SELECT high_water FROM journal_metadata").fetchone()[0]
                for name in order:
                    started = time.perf_counter_ns()
                    result = cli(binaries[name], roots[name], args, env)
                    row[name + "_ns"] = time.perf_counter_ns() - started
                    row["commands"][name] = result
                after_h = connection.execute("SELECT high_water FROM journal_metadata").fetchone()[0]
                expected = WORKLOAD_SPEC["workloads"][kind]["journal_delta"]
                require(after_h - before_h == expected, "measurement journal effect differs from protocol")
                row["candidate_journal_delta"] = after_h - before_h
                for name in order:
                    validate_command(row["commands"][name], fixture["execution"][name], args, output,
                                     f"{name} {kind} {index}")
                state = after
                (warmups if index < 5 else samples).append(row)
            for name, root in roots.items():
                observations[name]["after_workload_before_checkpoint"] = sizes(db(root))
                checkpoint = keepers[name].execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchall()
                require(checkpoint[0][0] == 0, "checkpoint busy")
                observations[name]["checkpoint_result"] = checkpoint
                observations[name]["after_explicit_wal_checkpoint_TRUNCATE"] = sizes(db(root))
            results[kind] = {"warmups": warmups, "raw_samples": samples,
                             "statistics": statistics(samples), "storage": observations}
    finally:
        for connection in keepers.values():
            connection.close()
    return {"items": fixture["items"], "setup": setup, "workloads": results}


def validate_proof(proof):
    require(proof["candidate_revision"] == CANDIDATE, "wrong candidate")
    require(proof["release_approved"] is False, "no live release approval")
    require(proof["sync_enabled"] == 0 and proof["bootstrap_required"] == 1, "activation forbidden")
    require(proof["fresh"]["migrations"] == [1, 2, 3, 4], "fresh proof absent")
    require(proof["historical_rollback"]["separate_root"] is True, "rollback distinct")
    restore = proof["current_schema_restore"]
    require(len(set(restore["generations"])) == 3 and restore["repeated_identical_backup"], "generations")
    require(restore["restored_tables_equal_except_generation"], "restored tables")
    require(len(restore["preserved_later"]) == 2, "preservation")
    require(all(row["tables_equal_before"] for row in restore["preserved_later"]), "later content")
    require(proof["prune"]["audit_receipts_and_allocator_unchanged"], "prune preservation")
    require(proof["prune"]["retained_coverage_verified"], "retention")
    require(proof["diagnostic"]["exit_code"] == 0, "diagnostic not verified")


def validate(directory):
    """Validate evidence against current inputs; generation and --validate stay strict."""
    _validate(directory, check_current_inputs=True)


def validate_historical(directory):
    """Check only accepted historical evidence, without asserting current-source coverage."""
    for name, expected in HISTORICAL_ARTIFACT_SHA256.items():
        require(digest((directory / name).read_bytes()) == expected, "historical artifact changed: " + name)
    _validate(directory, check_current_inputs=False)


def _validate(directory, *, check_current_inputs):
    """Recompute shared provenance, semantics and summaries; timings are not CI thresholds."""
    proof = json.loads((directory / "proof.json").read_text())
    validate_proof(proof)
    protocol = json.loads((directory / "protocol.json").read_text())
    measurements = json.loads((directory / "measurements.json").read_text())
    require(proof["protocol_sha256"] == measurements["protocol_sha256"] ==
            digest((directory / "protocol.json").read_bytes()), "protocol binding")
    require(protocol["frozen_before_timing"] is True, "protocol must precede timing")
    require(protocol["warmups"] == 5 and protocol["paired_samples"] == 30, "sample protocol")
    require(protocol["sizes"] == SIZES and protocol["seed"] == 2003, "fixture protocol")
    if check_current_inputs:
        require(protocol["harness_inputs_sha256"] == harness_inputs(), "harness/tests changed; remeasure explicitly")
        require(protocol["production_inputs_sha256"] == archived.source_inputs(ROOT), "production changed")
    require(measurements["started_at_unix_ns"] > protocol["frozen_at_unix_ns"], "timing preceded freeze")
    upgrade = json.loads((directory / "upgrade-proof.json").read_text())
    require(protocol["upgrade_proof_sha256"] ==
            digest((directory / "upgrade-proof.json").read_bytes()), "upgrade proof binding")
    payload = {key: value for key, value in proof.items() if key not in
               ("format", "candidate_revision", "protocol_sha256", "release_approved",
                "sync_enabled", "bootstrap_required")}
    require(protocol["correctness_payload_sha256"] == digest(canonical(payload)), "correctness proof binding")
    require(upgrade["candidate"]["source_inputs_sha256"] == protocol["production_inputs_sha256"], "candidate binding")
    require(upgrade["historical"]["source_modified"] is False, "modified old binary")
    archived.validate_order(upgrade["events"])
    require(all(not row["process_spawned"] for row in upgrade["blocked_restarts"]), "restart exclusion")
    require(all(row["before"] == row["after"] for row in upgrade["rejections"]), "old refusal content")
    require(upgrade["historical"]["revision"] == OLD, "baseline binding")
    for name, source in [("baseline", "historical"), ("candidate", "candidate")]:
        require(protocol[name + "_build"] == upgrade[source], "build provenance binding")
        build = upgrade[source]
        require(build["binary_sha256"] == build["binary_end_sha256"], "binary end hash")
        require(build["source_sha256"] == build["source_end_sha256"] ==
                digest(canonical(build["source_inputs_sha256"])), "source end hash")
        require(build["build"]["locked"] and build["build"]["separate_target"], "locked builds")
    require(protocol["candidate_revision"] == CANDIDATE and protocol["baseline_revision"] == OLD, "revisions")
    validate_semantics(protocol, measurements)
    require(len(measurements["fixtures"]) == 3, "measurement sizes")
    for fixture, measured in zip(protocol["fixtures"], measurements["fixtures"]):
        validate_fixture_digest(fixture)
        require(fixture["legacy_tables_equal"], "fixtures incomparable")
        require(fixture["items"] == measured["items"], "fixture identity")
        require(set(measured["workloads"]) == set(KINDS), "workload coverage")
        for kind, workload in measured["workloads"].items():
            require(len(workload["warmups"]) == 5, "warmups")
            require(workload["statistics"] == statistics(workload["raw_samples"]), "statistics")
            for row in workload["warmups"] + workload["raw_samples"]:
                for name in ("baseline", "candidate"):
                    require(row[name + "_ns"] > 0, "raw sample")
            for storage in workload["storage"].values():
                require(storage["checkpoint_result"][0][0] == 0, "checkpoint")
                for phase in ("before_workload", "after_workload_before_checkpoint",
                              "after_explicit_wal_checkpoint_TRUNCATE"):
                    require(set(storage[phase]) == {"main_db_bytes", "wal_bytes"}, "separate byte observations")


def run(output, directory):
    output.mkdir(parents=True, exist_ok=True)
    require(not any((output / name).exists() for name in
                    ["upgrade-proof.json", "proof.json", "protocol.json", "measurements.json"]),
            "use a new output directory; never overwrite recorded evidence")
    env = {key: value for key, value in os.environ.items()
           if key not in ("BIF_ROOT", "BIF_CONFIG", "BIF_REQUESTER", "CARGO_TARGET_DIR")}
    production = archived.source_inputs(ROOT)
    for path, sha in production.items():
        original = subprocess.run(["git", "show", f"{CANDIDATE}:{path}"], cwd=ROOT,
                                  capture_output=True, check=True).stdout
        require(digest(original) == sha, "production differs from pinned candidate: " + path)
    print("Build authentic old/candidate and rerun drain/upgrade/refusal proof", flush=True)
    upgrade = archived.rehearse(ROOT, directory)
    write(output / "upgrade-proof.json", upgrade)
    old = directory / "historical-target/release/bif"
    candidate = directory / "candidate-target/release/bif"
    correctness_proof = correctness(directory, old, candidate, env)
    print("Generate comparable fixtures before protocol freeze", flush=True)
    fixtures, generator = prepare(directory, old, candidate, env)
    for fixture in fixtures:
        fixture["execution"] = freeze_execution(fixture, old, candidate, upgrade)
    protocol = {
        "format": "bif-phase-e-final-gate-protocol-v1",
        "baseline_revision": OLD, "candidate_revision": CANDIDATE,
        "production_inputs_sha256": production, "harness_inputs_sha256": harness_inputs(),
        "upgrade_proof_sha256": digest((output / "upgrade-proof.json").read_bytes()),
        "correctness_payload_sha256": digest(canonical(correctness_proof)),
        "baseline_build": upgrade["historical"], "candidate_build": upgrade["candidate"],
        "generator": generator, "fixtures": fixtures, "sizes": SIZES, "seed": 2003,
        "warmups": 5, "paired_samples": 30, "workload_order": KINDS, "workload_spec": WORKLOAD_SPEC,
        "pair_order": "alternate baseline/candidate on even indices, candidate/baseline on odd; indices 0..34",
        "measurement": "end-to-end supported one-shot CLI including spawn, startup, preflight, durable write, response and reap; get/SQL observations outside timing",
        "statistics": "median; nearest-rank p95; paired candidate/baseline ratios; median ratio; percentile bootstrap 10000 resamples of 30 pairs, Python random.Random(2003), sorted indices 249 and 9749",
        "disposition": "Key effective writes capture/effective/note_only/compound: >=10% paired median slowdown AND CI95 wholly above parity requires explicit explanation/disposition review; >=10% with overlapping CI is inconclusive, repeat whole protocol without cherry-picking; no absolute CI time gate",
        "storage": "main DB and WAL separately before/after each 35-pair workload and after explicit PRAGMA wal_checkpoint(TRUNCATE); both idle keeper connections first query sqlite_schema before setup, then initial TRUNCATE checkpoint after setup; no active read transaction or external readers; production WAL/FULL and default wal_autocheckpoint=1000 unchanged",
        "environment": {"platform": platform.platform(), "uname": list(platform.uname()),
                        "python": platform.python_version(), "python_sqlite": sqlite3.sqlite_version,
                        "sqlite_cli": command(["sqlite3", "--version"])["stdout"],
                        "production_sqlite": sqlite_build_provenance(directory),
                        "filesystem": command(["df", "-h", str(directory)])["stdout"],
                        "profile": "release"},
        "limits": ["Process crash/reopen tests are not power-loss/torn-write evidence.",
                   "Maintenance process tests exercise primitive boundaries, not instrumented CLI interiors.",
                   "Logical retention is not physical shrinking; explicit checkpoint observations are separate.",
                   "Synthetic generated workloads and one host are not a live platform/configuration approval."],
        "frozen_before_timing": True, "frozen_at_unix_ns": time.time_ns(),
    }
    for fixture in fixtures:
        validate_execution(protocol, fixture)
    write(output / "protocol.json", protocol)
    protocol_hash = digest((output / "protocol.json").read_bytes())
    print("Protocol frozen: " + protocol_hash, flush=True)
    measurements = {"protocol_sha256": protocol_hash, "started_at_unix_ns": time.time_ns(), "fixtures": []}
    for fixture in fixtures:
        print(f"Measure {fixture['items']} items: 5 warmups + 30 paired samples x six workloads", flush=True)
        measurements["fixtures"].append(measure(fixture, old, candidate, env))
    measurements["finished_at_unix_ns"] = time.time_ns()
    require(production == archived.source_inputs(ROOT), "production changed during execution")
    require(protocol["harness_inputs_sha256"] == harness_inputs(), "harness changed during execution")
    for name, binary in [("baseline", old), ("candidate", candidate)]:
        require(digest(binary.read_bytes()) == protocol[name + "_build"]["binary_sha256"], "binary changed")
    write(output / "measurements.json", measurements)
    proof = {"format": "bif-phase-e-final-gate-proof-v1", "candidate_revision": CANDIDATE,
             "protocol_sha256": protocol_hash, "release_approved": False,
             "sync_enabled": 0, "bootstrap_required": 1, **correctness_proof}
    write(output / "proof.json", proof)
    validate(output)
    print("Recorded evidence passed deterministic validation; independent disposition/release review still required.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--work-dir", type=pathlib.Path)
    parser.add_argument("--validate", action="store_true")
    args = parser.parse_args()
    if args.validate:
        validate(args.output or HERE)
    else:
        require(args.output and args.work_dir, "--output and new --work-dir required")
        args.work_dir.mkdir(parents=True, exist_ok=False)
        run(args.output.resolve(), args.work_dir.resolve())


if __name__ == "__main__":
    main()
