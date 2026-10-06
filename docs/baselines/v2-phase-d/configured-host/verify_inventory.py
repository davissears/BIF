"""Reproduce the frozen inventory comparison using repository files only."""

import copy
import hashlib
import json
import sys
from datetime import datetime
from pathlib import Path


BASE = Path(__file__).resolve().parent
STAGES = ("before", "after_reads", "after_restart", "after_fixture_write")
# Positional rows follow the CREATE TABLE column order retained in schema.
ITEM_COLUMNS = (
    "item_id", "requester", "project_id", "sequence", "title", "description",
    "status", "priority", "assignee", "status_reason", "revision",
    "captured_at", "updated_at",
)


def check(condition, message):
    """Keep verification active even when Python runs with optimization."""
    if not condition:
        raise ValueError(message)


def canonical_sha256(value):
    """Match preparation.json's compact, sorted Python JSON serialization."""
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def item_summary(row):
    return dict(zip(
        ("id", "title", "status", "priority", "assignee", "revision"),
        (row[0], row[4], row[6], row[7], row[8], row[10]),
    ))


def added_row(before, after, table):
    """Require exactly one appended row and preserve every historical row."""
    old, new = before[table], after[table]
    added = [row for row in new if row not in old]
    check(len(added) == 1, f"{table}: expected one added row")
    check(new == old + added, f"{table}: historical rows or row order changed")
    return added[0]


def verify_snapshots(snapshots, report, transcript):
    """Compare all metadata/rows, then allow only the captured fixture change."""
    before = snapshots["before"]
    for stage in ("after_reads", "after_restart"):
        check(snapshots[stage] == before, f"{stage}: full inventory differs from before")
    after = snapshots["after_fixture_write"]
    check(
        {k: v for k, v in after.items() if k != "tables"}
        == {k: v for k, v in before.items() if k != "tables"},
        "after_fixture_write: schema or metadata changed",
    )
    old, new = before["tables"], after["tables"]
    check(old.keys() == new.keys() and len(old) == 12, "expected the same 12 tables")
    changed = sorted(name for name in old if old[name] != new[name])
    check(
        changed == ["events", "items", "mutation_receipts", "operations"],
        "after_fixture_write: unintended changed tables",
    )

    capture = transcript["fixture_write"]
    check(
        (capture["before"]["id"], capture["before"]["priority"],
         capture["before"]["revision"], capture["after"]["priority"],
         capture["after"]["revision"]) == ("BENCH:core:026", "P1", 4, "P4", 5),
        "unexpected recorded fixture mutation",
    )
    matches = [row for row in old["items"] if row[0] == capture["before"]["id"]]
    check(len(matches) == 1, "expected one fixture item")
    item = matches[0]
    check(item_summary(item) == capture["before"], "item disagrees with pre-write capture")
    timestamp = (
        datetime.fromisoformat(capture["executed_at"])
        .replace(microsecond=0).isoformat().replace("+00:00", "Z")
    )
    expected_items = copy.deepcopy(old["items"])
    updated = expected_items[old["items"].index(item)]
    updated[7], updated[10], updated[12] = "P4", 5, timestamp
    check(new["items"] == expected_items, "items: unintended row or field change")
    check(item_summary(updated) == capture["after"], "item disagrees with post-write capture")
    observed = transcript["after_corrected_fixture_write"]["observations"][0]
    check(
        observed["response"]["structuredContent"]["result"]["item"] == capture["after"],
        "item disagrees with corrected host get",
    )
    for command, summary in zip(
        (capture["commands"][0], capture["commands"][2]),
        (capture["before"], capture["after"]),
    ):
        check(
            command["exit_code"] == 0
            and json.loads(command["stdout"])["result"]["item"] == summary,
            "item disagrees with captured CLI get",
        )

    event = added_row(old, new, "events")
    operation = added_row(old, new, "operations")
    receipt = added_row(old, new, "mutation_receipts")
    operation_id = operation[0]
    item_id = item[0]
    check(
        operation == [operation_id, item_id, "triage", 4, 5, timestamp],
        "operations: expected the captured revision-checked triage",
    )
    check(
        event == [
            operation_id.replace("cli-mutation-", "cli-mutation-event-", 1) + "-0",
            operation_id, item_id, 5, 0,
            "priority_changed", "P1", "P4", "human", "BENCH", "cli", "local",
            "direct", None, "cli", "local", None, None, timestamp, 1,
        ],
        "events: expected one linked priority_changed event",
    )
    # Original CLI payload digest; the capture does not expose its preimage.
    check(
        receipt == [
            capture["idempotency_key"], "prioritize",
            "da4ad76bbc5c1a6329f4235d3690f04a5090d7bc1b08232e2076f0e0aaa14874",
            operation_id, item_id,
            json.dumps({"item_id": item_id, "revision": 5}, separators=(",", ":")),
            timestamp,
        ],
        "mutation_receipts: expected the captured key, operation, and response",
    )
    result = {
        "read_inventory_unchanged": True,
        "tables_compared": len(old),
        "changed_tables": changed,
        "changed_item_fields": sorted(
            name for name, a, b in zip(ITEM_COLUMNS, item, updated) if a != b
        ),
        "counts_before": {name: len(rows) for name, rows in old.items()},
        "counts_after": {name: len(rows) for name, rows in new.items()},
    }
    for field, value in result.items():
        check(report[field] == value, f"inventory-verification.json disagrees: {field}")
    return result


def verify_inventory(base=BASE):
    """Verify byte/canonical hashes and reproduce the original frozen report."""
    def load(name):
        return json.loads((base / name).read_bytes())

    manifest = load("inventory-retention.json")
    check(tuple(manifest["snapshots"]) == STAGES, "expected four retained snapshot stages")
    snapshots = {}
    for stage, entry in manifest["snapshots"].items():
        data = (base / entry["path"]).read_bytes()
        check(
            hashlib.sha256(data).hexdigest() == entry["file_sha256"],
            f"{stage}: file-byte SHA-256 mismatch",
        )
        snapshots[stage] = json.loads(data)
        check(
            canonical_sha256(snapshots[stage]) == entry["canonical_sha256"],
            f"{stage}: canonical SHA-256 mismatch",
        )
    preparation = load("preparation.json")
    check(
        canonical_sha256(snapshots["before"]) == preparation["logical_inventory_sha256"],
        "preparation.json canonical hash disagrees",
    )
    check(
        {name: len(rows) for name, rows in snapshots["before"]["tables"].items()}
        == preparation["logical_table_counts"],
        "preparation.json table counts disagree",
    )
    return verify_snapshots(snapshots, load("inventory-verification.json"), load("transcript.json"))


if __name__ == "__main__":
    try:
        print(json.dumps(verify_inventory(), indent=2))
    except (ValueError, KeyError, IndexError, TypeError, OSError) as error:
        print(f"Inventory verification failed: {error}", file=sys.stderr)
        sys.exit(1)
