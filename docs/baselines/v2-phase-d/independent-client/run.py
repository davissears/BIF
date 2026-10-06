#!/usr/bin/env python3
"""Focused interoperability test using the official MCP Python SDK, not BIF's client."""

import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import platform
import sqlite3
import subprocess
import sys
import tempfile
import traceback
from datetime import datetime, timedelta, timezone

import anyio
from jsonschema import Draft202012Validator
from mcp import ClientSession, StdioServerParameters, types
from mcp.client.stdio import stdio_client
from mcp.shared.exceptions import McpError


REPO = Path(__file__).resolve().parents[4]
HERE = Path(__file__).resolve().parent
SDK_VERSION = "1.26.0"
PROTOCOL = "2025-11-25"
BINARIES = ("bif", "bif-mcp", "bif-benchmark-store")
LIMITATIONS = [
    "Official SDK stdio client, not native Delta tool registration or an application-host approval.",
    "No actual model tool selection, model/token accounting, or operator-approved live rollout.",
    "Disposable generated fixture only; no real ledger, credentials, or mutation calls over MCP.",
    "SDK validates MCP response models. Server advertises no outputSchema; BIF envelope checks are explicit assertions.",
    "Raw malformed-frame tests are not repeated; no replacement of the SDK transport/parser.",
    "Read-only logical inventory covers schema, every table/row, and user/application versions, not physical SQLite/WAL bytes.",
    "SDK owns process cleanup; distinct OS process IDs/exits are additionally observed through ps.",
]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def command(argv, **kwargs):
    return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=30, **kwargs).stdout


def source_state():
    paths = command(["git", "ls-files", "src", "migrations", "Cargo.toml", "Cargo.lock",
                     "rust-toolchain.toml"], cwd=REPO).splitlines()
    hashes = {p: sha((REPO / p).read_bytes()) for p in paths}
    status = command(["git", "status", "--porcelain"], cwd=REPO)
    return {
        "source_revision": command(["git", "rev-parse", "HEAD"], cwd=REPO).strip(),
        "source_dirty": bool(status),
        "git_status_porcelain": status,
        "source_files_sha256": hashes,
        "source_inputs_sha256": sha(json.dumps(hashes, sort_keys=True).encode()),
        "binary_sha256": {name: sha((REPO / "target/release" / name).read_bytes()) for name in BINARIES},
    }


def inventory(database):
    """Hash all logical rows, including bookkeeping, in one read-only snapshot."""
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
        db.execute("BEGIN")
        schema = db.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name").fetchall()
        tables = {}
        for (name,) in db.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name"):
            quoted = '"' + name.replace('"', '""') + '"'
            rows = db.execute("SELECT * FROM " + quoted).fetchall()
            encoded = sorted(json.dumps(row, separators=(",", ":"), ensure_ascii=False,
                                        default=lambda b: {"bytes_hex": b.hex()}) for row in rows)
            tables[name] = {"rows": len(rows), "sha256": sha("\n".join(encoded).encode())}
        return {
            "sqlite_runtime": sqlite3.sqlite_version,
            "schema_sha256": sha(json.dumps(schema, separators=(",", ":")).encode()),
            "user_version": db.execute("PRAGMA user_version").fetchone()[0],
            "application_id": db.execute("PRAGMA application_id").fetchone()[0],
            "tables": tables,
        }


class Evidence:
    """Record SDK-decoded responses and fail-fast, named assertions."""

    def __init__(self, root):
        self.root = root
        self.data = {"kind": "independent_official_sdk_client", "status": "running",
                     "executed_at": datetime.now(timezone.utc).isoformat(),
                     "limitations": LIMITATIONS, "assertions": [], "transcript": []}

    def check(self, name, condition):
        self.data["assertions"].append({"name": name, "passed": bool(condition)})
        if not condition:
            raise AssertionError(name)

    def record(self, name, value):
        self.data["transcript"].append({"step": name, **value})

    async def call(self, session, step, name, args, error=None):
        result = await session.call_tool(name, args)
        raw = result.model_dump(mode="json", by_alias=True, exclude_none=True)
        self.record(step, {"tool": name, "arguments": args, "sdk_response": raw})
        envelope = result.structuredContent
        self.check(step + ": text equals structured BIF v2 envelope",
                   isinstance(envelope, dict) and len(result.content) == 1
                   and result.content[0].type == "text"
                   and json.loads(result.content[0].text) == envelope
                   and envelope.get("api_version") == 2 and envelope.get("schema_version") == 1
                   and envelope.get("ok") is (error is None)
                   and result.isError is (error is not None))
        if error:
            self.check(step + ": application error code", envelope["error"]["code"] == error)
            return envelope
        return envelope["result"]

    async def boundary(self, session, step, name, args):
        try:
            result = await session.call_tool(name, args)
        except McpError as exc:
            self.record(step, {"tool": name, "arguments": args,
                               "sdk_exception": type(exc).__name__,
                               "jsonrpc_error": exc.error.model_dump(mode="json", exclude_none=True)})
            self.check(step + ": SDK raised JSON-RPC invalid params", exc.error.code == -32602)
        else:
            self.record(step, {"tool": name, "arguments": args,
                               "unexpected_sdk_response": result.model_dump(mode="json", exclude_none=True)})
            self.check(step + ": SDK should have raised McpError", False)

    def processes(self):
        # Observe process identity without intercepting or replacing SDK transport.
        rows = command(["ps", "-axo", "pid=,command="]).splitlines()
        expected = str(REPO / "target/release/bif-mcp") + " --config " + str(self.root / "config.toml")
        return [int(row.strip().split(None, 1)[0]) for row in rows if expected in row]

    def write(self, path):
        checks = self.data["assertions"]
        self.data["summary"] = {"passed": sum(c["passed"] for c in checks),
                                "failed": sum(not c["passed"] for c in checks),
                                "sdk_transcript_steps": len(self.data["transcript"])}
        text = json.dumps(self.data, indent=2, ensure_ascii=False)
        # Sanitize only local filesystem roots; preserve all SDK result content/tokens.
        text = text.replace(str(self.root), "<RUN>").replace(str(REPO), "<WORKTREE>")
        # Exclusive creation also protects evidence from concurrent runs.
        with path.open("x", encoding="utf-8") as output:
            output.write(text + "\n")


async def exercise(e, env, database):
    params = StdioServerParameters(command=str(REPO / "target/release/bif-mcp"),
                                   args=["--config", str(e.root / "config.toml")],
                                   env=env, cwd=str(e.root))
    e.data["launch"] = params.model_dump(mode="json", exclude_none=True)
    e.data["configuration"] = (e.root / "config.toml").read_text()
    e.data["inventory_before"] = inventory(database)
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
        other = db.execute("SELECT project_id FROM items WHERE project_id!='core' ORDER BY project_id LIMIT 1").fetchone()[0]
        history_id = db.execute(
            "SELECT i.item_id FROM items i JOIN events e ON i.item_id=e.item_id "
            "WHERE i.project_id='core' GROUP BY i.item_id ORDER BY count(*) DESC,i.item_id LIMIT 1"
        ).fetchone()[0]
    e.data["fixture_choices"] = {"primary_project": "core", "other_project": other, "history_item": history_id}
    list_args = {"project": "core", "limit": 3}
    history_args = {"project": "core", "item_id": history_id, "limit": 2}
    old_pid = None
    # Exiting both contexts closes the session and the old process BEFORE restart.
    for index in (1, 2):
        with (e.root / f"stderr-{index}.log").open("w+") as stderr:
            async with stdio_client(params, errlog=stderr) as (read, write):
                async with ClientSession(read, write, read_timeout_seconds=timedelta(seconds=10),
                                         client_info=types.Implementation(
                                             name="bif-independent-official-python-sdk", version=SDK_VERSION)) as session:
                    init = await session.initialize()
                    e.record(f"initialize-{index}", {"sdk_response": init.model_dump(mode="json", exclude_none=True)})
                    e.check(f"initialize-{index}: negotiated pinned protocol", init.protocolVersion == PROTOCOL)
                    pids = e.processes()
                    e.check(f"initialize-{index}: exactly one actual server process", len(pids) == 1)
                    e.record(f"process-{index}", {"pid": pids[0]})
                    if index == 2:
                        e.check("restart: new OS process", pids[0] != old_pid)
                        again = await e.call(session, "restart-list", "bif_list", list_continuation)
                        e.check("restart: list continuation equal on unchanged store", again == list_second)
                        again = await e.call(session, "restart-history", "bif_history", history_continuation)
                        e.check("restart: history continuation equal on unchanged store", again == history_second)
                        again = await e.call(session, "restart-validator", "bif_get", known_args)
                        e.check("restart: old validator still hits", again == hit)
                    else:
                        old_pid = pids[0]
                        catalog = await session.list_tools()
                        e.record("tools/list", {"sdk_response": catalog.model_dump(mode="json", exclude_none=True)})
                        tools = {tool.name: tool for tool in catalog.tools}
                        e.check("discovery: exactly four read tools", set(tools) == {
                            "bif_list", "bif_get", "bif_history", "bif_selected_work"})
                        for name, tool in tools.items():
                            Draft202012Validator.check_schema(tool.inputSchema)
                            e.check(name + ": strict project schema and read-only annotation",
                                    tool.inputSchema["additionalProperties"] is False
                                    and "project" in tool.inputSchema["required"]
                                    and tool.annotations.readOnlyHint is True
                                    and tool.annotations.destructiveHint is False)
                        for name in ("bif_list", "bif_history"):
                            e.check(name + ": live-pagination description",
                                    "live pagination, not a snapshot" in tools[name].description)
                        first = await e.call(session, "list-first", "bif_list", list_args)
                        e.check("list: bounded summary page", len(first["items"]) == 3
                                and all(set(x) == {"id", "title", "status", "priority", "assignee", "revision"}
                                        and x["id"].split(":")[1] == "core"
                                        for x in first["items"]))
                        e.check("list: actual continuation token", bool(first["next_cursor"]))
                        item_id = first["items"][0]["id"]
                        get_args = {"project": "core", "item_id": item_id}
                        summary = await e.call(session, "get-summary", "bif_get", get_args)
                        e.check("get: complete summary modified", summary["outcome"] == "modified"
                                and summary["item"] == first["items"][0])
                        known_args = {**get_args, "known_version": summary["version"]}
                        hit = await e.call(session, "known-version-hit", "bif_get", known_args)
                        e.check("conditional: matching projection hit", hit == {
                            "outcome": "not_modified", "version": summary["version"], "item": None})
                        work = await e.call(session, "projection-mismatch", "bif_get",
                                            {**known_args, "projection": "work"})
                        e.check("conditional: summary token cannot suppress work",
                                work["outcome"] == "modified"
                                and set(work["item"]) == set(summary["item"]) |
                                {"acceptance_criteria", "description", "status_reason"})
                        work_again = await e.call(session, "get-work", "bif_get",
                                                  {**get_args, "projection": "work"})
                        e.check("get: work equality", work_again == work)
                        selected = await e.call(session, "selected-work", "bif_selected_work", {"project": "core"})
                        next_page = await e.call(session, "next-ready", "bif_list",
                                                {"project": "core", "view": "ready", "ordering": "next",
                                                 "projection": "work", "limit": 1})
                        e.check("selected-work: actual selected complete work, same next policy",
                                selected["outcome"] == "selected" and selected["item"] == next_page["items"][0])
                        other_page = await e.call(session, "other-project", "bif_list", {"project": other, "limit": 3})
                        e.check("scope: different project's records", bool(other_page["items"])
                                and all(x["id"].split(":")[1] == other for x in other_page["items"]))
                        again = await e.call(session, "core-after-other", "bif_list", list_args)
                        e.check("scope: core unchanged after other project", again == first)
                        list_continuation = {**list_args, "cursor": first["next_cursor"]}
                        list_second = await e.call(session, "list-continuation", "bif_list", list_continuation)
                        e.check("list: actual next bounded, disjoint page", len(list_second["items"]) == 3
                                and not ({x["id"] for x in first["items"]} &
                                         {x["id"] for x in list_second["items"]}))
                        history = await e.call(session, "history-first", "bif_history", history_args)
                        e.check("history: actual bounded continuation", len(history["events"]) == 2
                                and bool(history["next_cursor"]))
                        history_continuation = {**history_args, "cursor": history["next_cursor"]}
                        history_second = await e.call(session, "history-continuation", "bif_history", history_continuation)
                        e.check("history: actual different next events", bool(history_second["events"])
                                and not any(x in history["events"] for x in history_second["events"]))
                        await e.call(session, "missing-item", "bif_get",
                                     {"project": "core", "item_id": "BENCH:core:999999"}, "not_found")
                        await e.call(session, "invalid-cursor", "bif_list",
                                     {**list_args, "cursor": "invalid"}, "invalid_cursor")
                        await e.call(session, "invalid-known-version", "bif_get",
                                     {**get_args, "known_version": "invalid"}, "invalid_input")
                        await e.boundary(session, "missing-project", "bif_list", {})
                        await e.boundary(session, "invalid-limit", "bif_list", {"project": "core", "limit": 0})
                        await e.boundary(session, "unknown-tool", "bif_mutate", {"project": "core"})
                        await e.boundary(session, "scope-mismatch", "bif_get", {**get_args, "project": other})
                        for key, value in {
                            "actor": {"kind": "human", "id": "BENCH"},
                            "execution": {"kind": "direct"}, "authorization": {"approved": True},
                            "root": str(e.root), "config": str(e.root / "config.toml"),
                        }.items():
                            await e.boundary(session, "reject-" + key, "bif_list", {**list_args, key: value})
                        again = await e.call(session, "success-after-errors", "bif_get", get_args)
                        e.check("SDK and server recover after both error layers", again == summary)
            e.check(f"session-{index}: server process stopped", e.processes() == [])
            stderr.seek(0)
            e.record(f"stderr-{index}", {"text": stderr.read()})
    e.data["inventory_after"] = inventory(database)
    e.check("read-only: entire SQLite logical inventory unchanged",
            e.data["inventory_before"] == e.data["inventory_after"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", type=Path, default=HERE / "results.json",
                        help="New JSON evidence path inside this directory (never overwrite).")
    args = parser.parse_args()
    output = args.results.resolve()
    if output.parent != HERE or output.exists():
        parser.error("--results must be a NEW file directly inside independent-client/")
    workspace = REPO / "target/independent-client"
    workspace.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="run-", dir=workspace)).resolve()
    e = Evidence(root)
    try:
        e.check("official SDK exact pin", importlib.metadata.version("mcp") == SDK_VERSION)
        e.check("official SDK latest protocol matches pinned server", types.LATEST_PROTOCOL_VERSION == PROTOCOL)
        e.data["sdk"] = {"name": "mcp (official modelcontextprotocol/python-sdk)",
                         "version": SDK_VERSION, "python": sys.version, "executable": sys.executable,
                         "platform": platform.platform(), "distributions": {
                             d.metadata["Name"]: d.version for d in importlib.metadata.distributions()}}
        e.data["source_start"] = source_state()
        e.data["driver_sha256"] = sha(Path(__file__).read_bytes())
        e.data["build"] = {"command": "cargo build --locked --release --bin bif-mcp --bin bif-benchmark-store --bin bif",
                           "profile": "release", "rust": command(["rustc", "--version"]).strip()}
        isolated = {"HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "xdg"),
                    "APPDATA": str(root / "appdata"), "TMPDIR": str(root / "tmp")}
        for path in isolated.values():
            Path(path).mkdir()
        env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", **isolated}
        e.data["environment_policy"] = "CLI env replaced; SDK safe inherited env excludes BIF_*; HOME/XDG/APPDATA/TMPDIR overridden."
        database = root / ".bif/bif.sqlite"
        database.parent.mkdir()
        generator = [str(REPO / "target/release/bif-benchmark-store"), "100", "--seed", "2003",
                     "--output", str(database)]
        fixture = json.loads(command(generator, env=env, cwd=root))
        e.record("production-fixture-generator", {"argv": generator, "stdout_json": fixture})
        init = [str(REPO / "target/release/bif"), "init", "--root", str(root),
                "--requester", "BENCH", "--config", str(root / "config.toml")]
        e.record("production-init", {"argv": init, "stdout": command(init, env=env, cwd=root)})

        async def bounded():
            with anyio.fail_after(120):
                await exercise(e, env, database)

        anyio.run(bounded)
        e.data["source_end"] = source_state()
        e.check("source and binaries unchanged during SDK workflow",
                all(e.data["source_start"][key] == e.data["source_end"][key]
                    for key in ("source_revision", "source_inputs_sha256", "binary_sha256")))
        e.data["status"] = "passed"
    except BaseException:
        e.data["status"] = "failed"
        e.data["failure"] = traceback.format_exc()
    finally:
        e.write(output)
    print(json.dumps({"status": e.data["status"], **e.data["summary"], "results": str(output)}))
    return 0 if e.data["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
