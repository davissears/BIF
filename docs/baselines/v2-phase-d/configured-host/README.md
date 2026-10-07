# Configured Codex host validation

**Status: configured-host validation passed; operator signoff approved.**
[host-evidence.json](host-evidence.json) records 34 passing checks and 26 actual
tool calls through Codex's configured connection. The
[transcript](transcript.json) retains model-visible results, the exact retained
tokens, all 24 application-owned session captures, and setup failures.
The [operator signoff](operator-signoff.json) records Davis's explicit approval
on 2026-10-07 for candidate validation and compatibility review. The release
manifest records that decision; live-store maintenance requires separate
authorization. [operator-review.md](operator-review.md) and the host report
preserve their earlier pending status as historical snapshots. The signoff
binds their hashes and the byte-preserved [reviewed runbook](upgrade-runbook.reviewed.md).

The application reports **Codex desktop 26.930.51102, build 13100** through its
read-only update-status tool. The separately installed CLI reports 0.160.0;
that CLI version is not the desktop application version.

The release build succeeded at source revision
`b97cd89f26838b577763d5f6f30d18afc873c669`. The candidate `bif-mcp` SHA-256 is
`783768bed7c71cac1c623472384c34028ee49ba959ab4ece322ae25ca1e1da98`.
The production input aggregate is
`50aaf5ccd6796e43f92f48466b4cac177318c3566df56d4b8a4867f04a2451ba`.
Both match the earlier independent SDK run. Codex accepted negotiated
`2025-11-25`, exposed exactly four read tools, and executed the required workflows.
[preparation.json](preparation.json) preserves the earlier preparation snapshot,
including its then-pending results; it is not the final workflow report.

## Application setup

The ignored disposable run is
`target/configured-mcp-host/run-fih71bqq`; it contains frozen executable copies,
an explicit BIF config, `launch.json`, `codex-mcp.toml`, and the pre-read logical
inventory. All 100 items are synthetic. The fixture uses seed 2003, digest
`b819125481255ec7`, five projects, and schema 3. No live ledger was opened.

Copy this run's `codex-mcp.toml` into the application's MCP configuration and
restart MCP through the application. Official documentation describes
[shared MCP configuration and the desktop Restart control](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).
Actual configuration must be checked after setup, rather than inferred from
this proposed snippet. No Codex user configuration has been modified here.

Computer Use returned: `Computer Use is not allowed to use the app
'com.openai.codex' for safety reasons.` The application exposes no settings or
MCP-restart tool in this session, so these controls require the user. Do not
replace them with a standalone MCP client or manipulate the application's UI
through another automation interface.

The configured command runs [stdio_tap.py](stdio_tap.py). This launcher checks
the frozen binary hash, starts that exact `bif-mcp` with explicit `--config`,
`--root`, and `--requester`, and replaces its environment with fixture-local
settings. CLI root/requester arguments take precedence over ambient overrides.
It generates no JSON-RPC messages and contains no MCP client or SDK. Codex owns
initialization, discovery, calls, and restart. Record the recorder's involvement
in the final evidence; its process is an additional part of the tested launch.

**Historical recorder source:** the configured-host captures used the exact bytes
now preserved in [stdio_tap.observed.py](stdio_tap.observed.py), SHA-256
`309d845bb3ebbd52761c8d301980c42f8987f3bafe3724d955f5b679baa781a6`.
Their recorded `stdio_tap.py` paths and launcher hashes describe that historical
launch, not the current file's bytes. The current [stdio_tap.py](stdio_tap.py),
SHA-256 `a84fa1a5d1f83ac364885c3a9cf6bbf87418d17fa0c91149ff71735e2283d086`,
adds independent shutdown supervision: two seconds of termination grace, a
bounded two-second kill/reap wait, and a one-second output drain on shutdown.
Normal EOF still drains losslessly. [Generic fixture tests](test_stdio_tap.py)
cover ignored SIGTERM, pipe backpressure, and normal exit; they are not a rerun
through Codex. No historical captures or provenance have been rewritten, and
the observed original server's cooperative SIGTERM exit -15 remains historical
evidence, not evidence that Codex tested this fixed recorder.

Each application-owned launch creates a private, separate `transcripts/session-*`
directory containing `client-to-server.bin`, `server-to-client.bin`,
`server-stderr.bin`, and `session.json`. The raw transport streams are copied
without line parsing, reserialization, or additional stdout. Metadata retains
the launch, binary/launcher/config hashes, process IDs, start/end times, exit
status, and forwarding errors. A still-running session has no completion claim.

## Workflow to execute through the application

1. Discover the four read tools and inspect their schemas/annotations. Confirm
   `bif_list`, `bif_get`, `bif_history`, and `bif_selected_work`, with no mutation
   tool. Match the actual initialize request/response and subsequent initialized
   notification in the recorder to establish negotiated `2025-11-25`.
2. Call list for `core` and `agent-tools` with explicit project, `view=all`,
   `ordering=newest_first`, `projection=summary`, and `limit=2`. Return to `core`
   and compare. Obtain actual item IDs from those results.
3. Call get for summary, work, and audit projections. Retain the entire returned
   item and validator. Check a matching conditional hit and a validator from a
   different projection producing a modified result.
4. Choose an actual item with enough history for two pages. Call history with
   `limit=2`. Retain the list and history cursors and matching continuation
   responses. Call selected-work for `core`; compare the complete item with
   list using `view=ready`, `ordering=next`, `projection=work`, and `limit=1`.
5. Exercise invalid-cursor and invalid-validator application errors and a later
   successful read. Retain the application's displayed results alongside raw
   transport results, including dual text/structured content and `isError`.
6. Retain exact cursor/validator strings outside the server, restart MCP through
   the application, and repeat both continuations and the conditional get.
   Check matching pages and a not-modified hit, distinct server PIDs, and the
   prior session's completed shutdown metadata. Compare the logical inventory
   with `inventory-before.json` before any deliberate fixture mutation.
7. For the runbook's changed-item miss, use a fresh production CLI get followed
   by a revision-checked priority change with a fresh idempotency key, exclusively
   against this disposable root and its frozen CLI. Record that deliberate write
   separately. Through Codex, confirm the old validator now produces modified
   content and the new revision. Do not conflate the expected fixture write
   with the earlier read-only inventory comparison.
8. Record application-controlled shutdown of the original workflow process,
   and disclose whether the new disposable workflow server remains active.
   Sanitize only workspace/run paths in retained transcripts; preserve tokens,
   requests, failures, responses, and process evidence. Check source and binary
   hashes again. The observed original server ended on application SIGTERM with
   exit -15 and no relay errors/stderr; its process is absent. The final server
   remains active on the disposable root, so no final-session shutdown is claimed.

## Reproduce the retained inventory comparison

The four original synthetic snapshots are retained byte-for-byte in
[inventory/](inventory/): before, after reads, after restart, and after the
deliberate fixture write. Each contains the full SQL schema (including column
order), `user_version`, `application_id`, and every row of all 12 tables.
This closes a repository-retention/reproducibility omission; the original
read/restart no-write comparison passed. No new host run was performed.

[inventory-retention.json](inventory-retention.json) records relative paths,
file-byte SHA-256 values, canonical JSON SHA-256 values, and the precise
serialization algorithm. The first three file hashes are
`07ac11dd7245b4994d2714834ca8be12bd8a56731405ee272b97dbffb5458e3e`;
the after-write file hash is
`3a62ca03a094e8351efdee138ef4738a1bccdf8fdbd69a95662d60c27bc3dc91`.
The preparation snapshot's `logical_inventory_sha256` is **not** a pretty-file
hash: it hashes UTF-8 bytes from Python
`json.dumps(value, sort_keys=True, separators=(',', ':'))`, with default ASCII
escaping and no trailing newline. That first-three canonical hash is
`4a8002998d4b6d36f77e4060962f104e7d7754fb90b3d77c5fa751545820b59d`.

From the repository root, using only the Python standard library:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 docs/baselines/v2-phase-d/configured-host/test_verify_inventory.py
PYTHONDONTWRITEBYTECODE=1 python3 docs/baselines/v2-phase-d/configured-host/verify_inventory.py
```

The [verifier](verify_inventory.py) checks both hashes, preparation provenance,
and complete row/schema equality through reads and restart. It reproduces the
frozen [inventory-verification.json](inventory-verification.json) counts and
field/table differences, matching the deliberate change to the existing
[transcript](transcript.json)'s CLI capture and corrected host get:
`BENCH:core:026` priority P1 to P4, revision 4 to 5, and `updated_at`; one linked
`priority_changed` event, one triage operation (expected revision 4, resulting
revision 5), and one receipt with the recorded idempotency key. All other item
fields, historical event/operation rows, eight other tables, and schema/metadata
must remain unchanged. The [focused tests](test_verify_inventory.py) reject
same-row-count corruption and unintended after-write mutations independently
of the file-hash gate.

These commands read retained JSON only; they do not open SQLite, recreate the
fixture, launch Codex/BIF, or touch host/store configuration. The original
preparation, comparison, host report, transcript, operator review, and raw
sessions remain frozen. Offline reproduction is not a new configured-host
validation, operator signoff, release approval, or live-store authorization.

## Evidence registration and operator review

After actual successful workflows, create a repository-relative JSON
artifact with `kind=real_configured_mcp_host`, actual host name/version,
sanitized configuration, negotiated protocol, source revision, binary hash,
execution timestamp, and per-workflow status/transcript paths. Register that
artifact in `docs/bif-v2-release-evidence.json` according to its existing rule.
Before an operator decision, the release gate is `awaiting_operator_review`,
with `approved=false` and `approval=null`; successful correctness evidence is
not operator approval. It is now `approved`, with the separate human decision
referenced by `approval`. The manifest check requires that record and verifies
its candidate binding and scope; CI cannot supply the decision.

Review the artifact together with
[the Phase D upgrade runbook](../../../bif-upgrade-runbook.md#phase-d-indexed-read-candidate).
The existing runbook explicitly covers automatic migration during reads,
old-binary/schema incompatibility, SQLite-consistent backups, stopping every
reader/writer and persistent MCP process, later-write loss on restore, the
cursor-size rollback limitation, and unconditional token invalidation after
restore until Phase E provides restore-generation detection. The archived
schema-2 rehearsal binary is historical, not the operator's retained production
binary. Confirm those limits when requesting explicit operator signoff.

Record the operator's identity, time, exact candidate hash, evidence/runbook
reviewed, decision, and restrictions in a separate signoff artifact only after
the operator supplies that decision. This signoff concerns candidate validation
and compatibility review. It grants no permission to open, upgrade, replace, or
mutate a live store; live maintenance needs separate authorization naming the
binary, store root, schema, verified backup, and maintenance window.

[operator-signoff.json](operator-signoff.json) records the supplied approval,
its exact wording, date and recording time. It binds the observed candidate
and the original reviewed artifact hashes. The runbook snapshot retains the
reviewed bytes while the current runbook's status header reflects this later
decision. No new configured-host run or production maintenance is claimed.

## Preparation verification

`python3 docs/baselines/v2-phase-d/configured-host/test_stdio_tap.py` passed
three focused checks: byte preservation on a large generic stream, refusal of
a changed executable, and child cleanup on host termination. Tests were written
first and initially failed because the recorder did not exist. They never used
BIF or an MCP client and do not count as configured-host evidence.

The original `cargo test --locked --test read_release_manifest` run passed
three tests before signoff. The subsequent operator-signoff change adds focused
coverage for an explicit decision, evidence without a decision, missing approval,
candidate mismatch and unauthorized live-maintenance scope. These checks are
separate from the application-owned workflow results and operator decision.

The initial Python SQLite read-only inventory open failed on the disposable
WAL database. Opening the existing database with `mode=rw` allowed sidecar
creation and a read transaction; no application write or BIF migration was
issued by that inventory step. This matches the runbook's documented WAL
limitation. Two later CLI setup assumptions also failed before any write:
unsupported `--requester` on v2 get, and comparing its `result` wrapper instead
of `result.item`. Those failures and the initial unchanged application follow-ups
are retained in `transcript.json`. The corrected setup changed only the synthetic
item's priority/revision/time and one event/operation/receipt; old-validator miss
and new-validator hit then passed through Codex. No host failure has been hidden
or converted to a passed result.
