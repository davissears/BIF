# Independent official-SDK interoperability

**Passed:** [`results-rerun.json`](results-rerun.json) records **65 passing
assertions, zero failures, and 37 transcript steps**. This is an independent
implementation test of a real `bif-mcp` stdio process launched from Delta's
terminal, not Delta native tool registration or release approval.

## Client, server, and provenance

- Official [MCP Python SDK](https://github.com/modelcontextprotocol/python-sdk),
  PyPI distribution **`mcp==1.26.0`**, using its unmodified `stdio_client` and
  `ClientSession`. The SDK, not a handwritten parser, performs initialization,
  JSON-RPC framing, response-model validation, tool discovery, calls, and cleanup.
- Runtime: **CPython 3.12.15**, macOS arm64. All 29 observed SDK distributions
  are recorded in JSON and pinned in `requirements.txt`. A workspace-local
  `uv==0.12.23` bootstrap obtained this Python because system Python was 3.9.6.
  No global Python installation or user configuration was changed.
- Both initialized processes negotiated **`2025-11-25`**. The SDK supports that
  protocol without monkeypatching its version or lifecycle.
- Source revision: **`e265e0209ca56cb63c57ce238d7a78df8d9abf76`**.
  The tree was dirty only with this new evidence directory at test time.
  JSON records start/end Git status, per-source-file SHA-256, aggregate
  production-input hash, and SHA-256 for all three locally release-built
  executables. Production source and binary hashes remained unchanged.
- The source aggregate is
  `50aaf5ccd6796e43f92f48466b4cac177318c3566df56d4b8a4867f04a2451ba`;
  `bif-mcp` SHA-256 is
  `783768bed7c71cac1c623472384c34028ee49ba959ab4ece322ae25ca1e1da98`.
  The source aggregate covers Git-tracked `src/`, `migrations/`, Cargo
  manifest/lockfile, and Rust toolchain pin; it is not a hash of all documentation.

The production `bif-benchmark-store` generator created **100 items, seed 2003**
in a new `target/independent-client/run-*` directory. Production `bif init`
then initialized explicit configuration with requester `BENCH`. The launch was:

```text
<WORKTREE>/target/release/bif-mcp --config <RUN>/config.toml
```

Its recorded config points at this disposable root, not a live ledger.
CLI subprocess environments are replaced; the SDK's safe inherited environment
excludes `BIF_*`. `HOME`, `XDG_CONFIG_HOME`, `APPDATA`, and `TMPDIR` are redirected
into the fresh root. Evidence replaces absolute workspace/run paths with
`<WORKTREE>`/`<RUN>`; content, opaque tokens, and SDK error data are retained.
The generated stores are intentionally retained under ignored `target/`, never
shared or adopted as real stores.

## Observed coverage

| Workflow | Observation |
| --- | --- |
| Lifecycle/discovery | Two real initializations, four tools, valid strict JSON schemas, mandatory project, read-only/nondestructive annotations, and list/history live-pagination descriptions. |
| List/get/work | Actual summary page and conditional get equality; actual complete work fields; unchanged known-version hit; summary validator misses for work. |
| Selected work | Actual selected complete work equals ready/next ordering's first item, without a claim. |
| Request independence | Actual `core` and `agent-tools` records; returning to `core` yields identical results. |
| Pagination | Actual next list and history pages, bounded records and distinct entries. |
| Restart | SDK stops the old session/process before opening another; different OS PIDs observed; old list/history cursors return identical continuations and old validator still hits. |
| Application errors | Missing item, invalid cursor, and invalid known-version return SDK-accepted `isError: true` with structured BIF error envelopes. |
| Protocol boundary | Missing project, invalid limit, unknown tool, wrong item project, and actor/execution/authorization/root/config overrides raise SDK `McpError` with JSON-RPC `-32602`, not BIF application envelopes. |
| Recovery | A later successful get equals the original result after both error layers. |
| Dual content | All **19** tool results have text JSON equal to `structuredContent`, canonical BIF v2 envelope versions, and matching `ok`/`isError`. |
| Read-only store | Schema plus all rows in **12 SQLite tables** have equal before/after logical inventories, including 100 items, 516 events/operations, and all bookkeeping. |
| Cleanup | Both SDK-owned server processes are absent after their contexts close; stderr is recorded and empty. |

The server does **not** advertise an `outputSchema`. SDK validation therefore
covers MCP response models, while explicit assertions validate the BIF envelopes
and requested projection field sets. The driver decodes the text block as BIF
application JSON only; it does not parse or implement JSON-RPC.

## Retained initial failure

[`results.json`](results.json) is the **first, failed attempt**, with 12 passing
assertions before a driver `KeyError: 'projection'`. The documentation's
illustrative conditional-get example suggested a response discriminator that
the actual wire contract does not have. Summary is six fields
(`id`, `title`, `status`, `priority`, `assignee`, `revision`); work adds
`description`, `acceptance_criteria`, and `status_reason`.

The new driver assertion was corrected to those field sets and rerun against
the same production source/binary hashes. This was a **test assumption error,
not an SDK interoperability or production defect**. The first failed result
and its original driver hash are retained unchanged; the passing rerun records
the corrected driver hash. No production implementation was fixed or changed.
The parent was notified of the misleading illustration separately.

Independent review reproduced all 65 assertions with a fresh release build.
After that review, the driver's final output write was changed to exclusive
creation, preventing simultaneous runs from overwriting the same evidence file.
A focused regression check failed before this change and passed afterward.
The retained SDK transcripts and their driver hashes describe the earlier
tested versions; they have not been rewritten to claim this later safety fix.

## Reproduce from this worktree

Run from the repository root. Downloads are public packages/runtimes; no model
account or credentials are needed. All caches, bytecode, dependency installations,
temporary files, and build outputs stay under the attached worktree's `target/`.

```sh
mkdir -p target/independent-client/tmp target/independent-client/cache
export TMPDIR="$PWD/target/independent-client/tmp"
export PYTHONPYCACHEPREFIX="$PWD/target/independent-client/pycache"
export UV_CACHE_DIR="$PWD/target/independent-client/cache/uv"
export UV_PYTHON_INSTALL_DIR="$PWD/target/independent-client/python"
export UV_PYTHON_BIN_DIR="$PWD/target/independent-client/bin"
export CARGO_TARGET_DIR="$PWD/target"

python3 -m venv target/independent-client/bootstrap
target/independent-client/bootstrap/bin/python -m pip \
  --cache-dir "$PWD/target/independent-client/cache/pip" install uv==0.12.23
target/independent-client/bootstrap/bin/uv venv \
  --python 3.12.15 --managed-python target/independent-client/venv
target/independent-client/bootstrap/bin/uv pip install \
  --python target/independent-client/venv/bin/python \
  -r docs/baselines/v2-phase-d/independent-client/requirements.txt

cargo build --locked --release --bin bif-mcp --bin bif-benchmark-store --bin bif
target/independent-client/venv/bin/python -m py_compile \
  docs/baselines/v2-phase-d/independent-client/run.py
target/independent-client/venv/bin/python \
  docs/baselines/v2-phase-d/independent-client/run.py --help
target/independent-client/venv/bin/python \
  docs/baselines/v2-phase-d/independent-client/run.py \
  --results docs/baselines/v2-phase-d/independent-client/results-new.json
```

Use a **new** evidence filename: the driver refuses to overwrite prior results
and restricts output to this directory. The observed run used
`--results docs/baselines/v2-phase-d/independent-client/results-rerun.json`.
Every subprocess command is limited to 30 seconds; each SDK request to 10
seconds; the complete async SDK workflow to 120 seconds. Delta's terminal
invocation was bounded to 180 seconds. SDK context cleanup closes stdin and
escalates termination if needed.

Syntax compilation and `--help` succeeded. This additional existing suite
also succeeded, **18 passed, zero failures/ignored**:

```sh
cargo test --locked --test mcp_tools --test mcp_process --test mcp_cli_parity
```

## Remaining evidence gap

This independently demonstrates official-client interoperability. It does
**not** establish actual model tool selection, native Delta registration,
configured application-host workflows, token savings, operator compatibility
review, or authorized live rollout. Raw malformed-frame protocol tests remain
the existing Rust suite's responsibility; this driver never substitutes a parser
or writes frames around the SDK. Logical equality does not claim physical
SQLite/WAL file-byte equality.

The release manifest, host evidence registration, approval, existing scripts,
source, tests, and CI were not edited by this worker. The V2-027 application-host
gate and operator approval remain separate and unchanged.
