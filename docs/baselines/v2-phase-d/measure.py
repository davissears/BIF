#!/usr/bin/env python3
"""Measure generated-store MCP reads; never accepts an existing ledger root."""

import argparse
from contextlib import closing
import hashlib
import json
import os
import pathlib
import platform
import plistlib
import selectors
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import threading
import time


def command(args):
    environment = {key: value for key, value in os.environ.items()
                   if key not in ("BIF_ROOT", "BIF_CONFIG", "BIF_REQUESTER")}
    return subprocess.check_output(args, text=True, stderr=subprocess.PIPE,
                                   env=environment, timeout=30).strip()


def distribution(values, unit="us"):
    ordered = sorted(values)
    return {
        "samples": len(values),
        f"median_{unit}": statistics.median(values),
        f"p95_{unit}": ordered[min(len(ordered) - 1, int(len(ordered) * 0.95))],
        f"minimum_{unit}": ordered[0],
        f"maximum_{unit}": ordered[-1],
    }


def source_snapshot():
    """Identify the actual dirty production inputs, not just the Git base."""
    inputs = [pathlib.Path("Cargo.toml"), pathlib.Path("Cargo.lock"),
              pathlib.Path("rust-toolchain.toml"), *pathlib.Path("src").rglob("*.rs"),
              *pathlib.Path("migrations").glob("*.sql")]
    return {
        "source_revision": command(["git", "rev-parse", "HEAD"]),
        "source_dirty": bool(command(["git", "status", "--porcelain"])),
        "git_status_porcelain": command(["git", "status", "--porcelain"]),
        "source_inputs_sha256": {
            str(path): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(inputs)
        },
    }


def machine_details(root):
    if platform.system() == "Darwin":
        volume = plistlib.loads(command(["diskutil", "info", "-plist", "/"]).encode())
        return {
            "cpu": command(["sysctl", "-n", "machdep.cpu.brand_string"]),
            "memory_bytes": int(command(["sysctl", "-n", "hw.memsize"])),
            "filesystem_type": volume.get("FilesystemType", "unknown"),
        }
    return {
        "cpu": command(["lscpu"]),
        "memory": pathlib.Path("/proc/meminfo").read_text(),
        "filesystem_type": command(["stat", "-f", "-c", "%T", str(root)]),
    }


class Client:
    """One in-flight request, deadline-bound reads, and explicit process cleanup."""

    def __init__(self, executable, root):
        environment = {key: value for key, value in os.environ.items()
                       if key not in ("BIF_ROOT", "BIF_CONFIG", "BIF_REQUESTER")}
        self.process = subprocess.Popen(
            [str(executable), "--config", str(root / "config.toml")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            bufsize=0, env=environment,
        )
        self.sequence = 0
        self.pending = bytearray()
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)

    def send(self, value):
        self.process.stdin.write(json.dumps(value, separators=(",", ":")).encode() + b"\n")
        self.process.stdin.flush()

    def request(self, method, params=None):
        self.sequence += 1
        start = time.perf_counter_ns()
        self.send({"jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params or {}})
        deadline = time.monotonic() + 30
        while b"\n" not in self.pending:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.selector.select(remaining):
                raise RuntimeError(f"timeout waiting for {method}")
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError(f"server disconnected during {method}")
            self.pending.extend(chunk)
            if len(self.pending) > 4 * 1024 * 1024:
                raise RuntimeError("unbounded response")
        line, _, rest = self.pending.partition(b"\n")
        self.pending = bytearray(rest)
        elapsed = (time.perf_counter_ns() - start) / 1000
        response = json.loads(line)
        if response.get("id") != self.sequence or "error" in response:
            raise RuntimeError(response)
        return response["result"], elapsed, len(line) + 1

    def initialize(self):
        result, _, _ = self.request("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "bif-phase-d-evidence", "version": "1"},
        })
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return result["protocolVersion"]

    def call(self, name, arguments):
        result, elapsed, size = self.request("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise RuntimeError(result)
        return result["structuredContent"], elapsed, size

    def rss(self):
        # ps reports KiB on the Unix platforms supported by the fixture builder.
        return int(command(["ps", "-o", "rss=", "-p", str(self.process.pid)]))

    def close(self):
        self.process.stdin.close()
        try:
            status = self.process.wait(timeout=10)
            diagnostics = self.process.stderr.read().decode()
            if status != 0:
                raise RuntimeError(f"server exit {status}: {diagnostics}")
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            self.selector.close()
            self.process.stdout.close()
            self.process.stderr.close()


def measure(args):
    binaries = args.binaries.resolve(strict=True)
    bif, mcp, generator = [binaries / name for name in ("bif", "bif-mcp", "bif-benchmark-store")]
    source = source_snapshot()
    # All writes are confined to a newly allocated directory under the worktree.
    destination = pathlib.Path("target/phase-d-measurements")
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="run-", dir=destination) as temporary:
        root = pathlib.Path(temporary).resolve()
        database = root / ".bif/bif.sqlite"
        database.parent.mkdir()
        fixture = json.loads(command([
            str(generator), str(args.items), "--seed", str(args.seed), "--output", str(database),
        ]))
        config = root / "config.toml"
        command([str(bif), "init", "--root", str(root), "--requester", "BENCH",
                 "--config", str(config)])
        startup, cli_samples = [], []
        for _ in range(args.startups):
            started = time.perf_counter_ns()
            client = Client(mcp, root)
            try:
                protocol = client.initialize()
                client.call("bif_list", {"project": "core", "limit": 20})
                startup.append((time.perf_counter_ns() - started) / 1000)
            finally:
                client.close()
            started = time.perf_counter_ns()
            command([str(bif), "--api-version", "2", "list", "all", "--project", "core",
                     "--limit", "20", "--json", "--config", str(config)])
            cli_samples.append((time.perf_counter_ns() - started) / 1000)

        client = Client(mcp, root)
        writer_errors = []
        writer_samples = []
        writer = None
        try:
            client.initialize()
            tools, _, schema_bytes = client.request("tools/list")
            input_schema_bytes = sum(len(json.dumps(tool["inputSchema"], separators=(",", ":"),
                                                    ensure_ascii=False).encode())
                                     for tool in tools["tools"])
            page, _, _ = client.call("bif_list", {"project": "core", "limit": 1})
            cursor = page["result"]["next_cursor"]
            item_id = page["result"]["items"][0]["id"]
            get_args = {"project": "core", "item_id": item_id, "projection": "work"}
            initial, _, _ = client.call("bif_get", get_args)
            get_args["known_version"] = initial["result"]["version"]
            rss = [client.rss()]
            wal_path = database.with_name(database.name + "-wal")
            wal_bytes = [wal_path.stat().st_size if wal_path.exists() else 0]
            loops = {name: [] for name in (
                "bif_list", "bif_get", "bif_history", "bif_selected_work",
            )}
            wire_bytes = {name: [] for name in loops}
            statements = [
                ("bif_list", {"project": "core", "limit": 20}),
                ("bif_get", get_args),
                ("bif_history", {"project": "core", "item_id": item_id, "limit": 20}),
                ("bif_selected_work", {"project": "core"}),
            ]

            def write():
                try:
                    for index in range(20):
                        started = time.perf_counter_ns()
                        command([str(bif), "capture", f"phase-d-concurrent-{index}",
                                 "--project", "core", "--root", str(root),
                                 "--requester", "BENCH", "--config", str(config),
                                 "--idempotency-key", f"phase-d-{index}"])
                        writer_samples.append((time.perf_counter_ns() - started) / 1000)
                except Exception as error:
                    writer_errors.append(str(error))

            writer = threading.Thread(target=write)
            writer.start()
            for index in range(args.requests):
                name, arguments = statements[index % len(statements)]
                result, elapsed, size = client.call(name, arguments)
                if name == "bif_get" and result["result"]["outcome"] != "not_modified":
                    raise RuntimeError("unchanged conditional read was not a hit")
                loops[name].append(elapsed)
                wire_bytes[name].append(size)
                if (index + 1) % 100 == 0:
                    rss.append(client.rss())
                    wal_bytes.append(wal_path.stat().st_size if wal_path.exists() else 0)
            writer.join(timeout=30)
            if writer.is_alive() or writer_errors:
                raise RuntimeError(f"concurrent writer failed: {writer_errors}")
            latest, _, _ = client.call("bif_list", {"project": "core", "limit": 20})
            if not any(item["title"].startswith("phase-d-concurrent-")
                       for item in latest["result"]["items"]):
                raise RuntimeError("warm process did not observe concurrent CLI writes")
            with closing(sqlite3.connect(database)) as connection:
                writes_observed = connection.execute(
                    "SELECT count(*) FROM items WHERE title LIKE 'phase-d-concurrent-%'"
                ).fetchone()[0]
                if writes_observed != 20:
                    raise RuntimeError(f"expected 20 durable CLI writes, got {writes_observed}")
                wal_bytes_before_checkpoint = wal_path.stat().st_size if wal_path.exists() else 0
                checkpoint = list(connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone())
                if checkpoint != [0, 0, 0]:
                    raise RuntimeError(f"unreleased WAL snapshot: {checkpoint}")
                integrity = connection.execute("PRAGMA integrity_check").fetchone()[0]
                if integrity != "ok":
                    raise RuntimeError(integrity)
                wal_bytes_after_checkpoint = wal_path.stat().st_size if wal_path.exists() else 0
            before, _, _ = client.call("bif_list", {"project": "core", "limit": 1, "cursor": cursor})
        finally:
            if writer is not None:
                writer.join(timeout=30)
            client.close()
        client = Client(mcp, root)
        try:
            client.initialize()
            after, _, _ = client.call("bif_list", {"project": "core", "limit": 1, "cursor": cursor})
            if before != after:
                raise RuntimeError("cursor meaning changed after process restart")
            hit, _, _ = client.call("bif_get", get_args)
            if hit["result"]["outcome"] != "not_modified":
                raise RuntimeError("validator meaning changed after process restart")
        finally:
            client.close()

        end = source_snapshot()
        if (source["source_inputs_sha256"] != end["source_inputs_sha256"]
                or source["source_revision"] != end["source_revision"]):
            raise RuntimeError("production source changed during measurements")
        return {
            "format": "bif-phase-d-measurement-v1",
            "measured_at_unix_seconds": int(time.time()),
            "harness_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
            **source,
            "source_end": end,
            "argv": sys.argv,
            "binary_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                              for path in (bif, mcp, generator)},
            "environment": {
                "platform": platform.platform(), "machine": platform.machine(),
                "cpu_count": os.cpu_count(), "python": platform.python_version(),
                "rust": command(["rustc", "--version"]),
                "filesystem": command(["df", str(root)]),
                "build_profile": args.profile, "python_sqlite": sqlite3.sqlite_version,
                "production_sqlite": args.sqlite_version,
                **machine_details(root),
            },
            "fixture": {"items": args.items, "seed": args.seed,
                        "logical_digest": fixture["logical_digest"], "row_counts": fixture["row_counts"]},
            "protocol_version": protocol,
            "tools_list_wire_bytes": schema_bytes,
            "input_schema_bytes": input_schema_bytes,
            "tool_count": len(tools["tools"]),
            "startup_through_first_list": distribution(startup),
            "one_shot_cli_list": distribution(cli_samples),
            "warm_calls": {name: distribution(values) for name, values in loops.items()},
            "wire_bytes": {name: distribution(values, "bytes") for name, values in wire_bytes.items()},
            "raw_samples": {
                "startup_us": startup, "one_shot_cli_us": cli_samples,
                "warm_calls_us": loops, "wire_bytes": wire_bytes,
                "cli_capture_us": writer_samples,
            },
            "server_rss_kib_samples": rss,
            "wal_bytes_samples": wal_bytes,
            "wal_bytes_before_checkpoint": wal_bytes_before_checkpoint,
            "wal_bytes_after_checkpoint": wal_bytes_after_checkpoint,
            "integrity": integrity,
            "concurrent_cli_captures": 20,
            "durable_cli_captures_observed": writes_observed,
            "writer_schedule": "20 separate sequential CLI processes on one thread overlapping the read loop",
            "wal_checkpoint": checkpoint,
            "cursor_restart_equivalent": True,
            "validator_restart_hit": True,
            "real_host_smoke": False,
            "limitations": [
                "Custom protocol harness, not a real configured MCP host.",
                "Warm OS caches; no cold-filesystem or model-token measurement.",
                "Sampled RSS, not an allocator/peak-memory proof.",
                "One sequential CLI writer thread; no sustained-contention capacity claim.",
            ],
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=pathlib.Path, default=pathlib.Path("target/release"))
    parser.add_argument("--items", type=int, choices=[100, 10000, 100000], default=10000)
    parser.add_argument("--seed", type=int, default=2003)
    parser.add_argument("--requests", type=int, default=1000)
    parser.add_argument("--startups", type=int, default=20)
    parser.add_argument("--profile", default="release")
    parser.add_argument("--sqlite-version", required=True, help="bundled SQLite version from Rust evidence")
    options = parser.parse_args()
    if options.requests < 4 or options.startups < 1:
        parser.error("at least four requests and one startup are required")
    print(json.dumps(measure(options), indent=2))
