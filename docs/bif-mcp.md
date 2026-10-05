# Read-only BIF MCP

`bif-mcp` is a separate persistent stdio entry point. `bif rpc` remains the
one-shot custom BIF RPC v1 interface. Neither command interprets the other's
framing or changes the existing mutation contract.

## Protocol decision

This adapter pins the **2025-11-25** MCP specification and its initialize-based
lifecycle. It does **not** claim to implement the current-latest MCP protocol:
later MCP versions have different discovery/versioning semantics. A host must
support the pinned version and complete initialization before invoking tools.

The implementation uses BIF's existing `serde`/`serde_json` dependencies directly,
not an MCP SDK. For four synchronous read tools, a bounded stdio reader and one
serialized SQLite worker keep the dependency/runtime boundary small. The
alternative official Rust SDK adds broader protocol/version support and an
async runtime; reconsider it when adding transports or newer lifecycle support,
not by silently treating future protocol versions as compatible.
The Rust toolchain remains pinned at 1.94.0. MCP types stay in the delivery
adapter, not the domain or application layers.

Normative pinned references:

- [Lifecycle and version negotiation](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
- [Stdio transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [Tool discovery, calls and results](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
- [Cancellation](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/cancellation)
- [JSON-RPC 2.0](https://www.jsonrpc.org/specification)

## Launch and trust boundary

```sh
cargo build --locked --release --bin bif-mcp
target/release/bif-mcp --config /absolute/path/to/config.toml
```

Startup accepts `--config PATH`, `--root PATH`, and `--requester ID`, with the
same configuration/environment precedence as BIF. Configuration is pinned
until process restart. Use absolute paths in host launch configuration.
Normal configuration loading does not initialize a missing store.
Opening an existing store applies supported migrations, so follow the
[upgrade runbook](bif-upgrade-runbook.md) before pointing a candidate at it.

The local launcher grants the process access to the configured ledger. Project
arguments are explicit query scope, **not project ACLs**; the existing domain
read policy permits correctly attributed agent reads across that ledger.
Tool arguments and MCP `clientInfo` cannot grant human authorization or select
trusted execution metadata. The adapter supplies fixed local agent attribution.
There is no mutation tool or trusted mutation bridge in this release.

## Tools

Discovery returns a static tool set with strict argument schemas and read-only
annotations. Do not treat annotations as authentication.

| Tool | Required arguments | Read operation |
| --- | --- | --- |
| `bif_list` | `project` | Bounded live queue page, named view and filters, requested projection. |
| `bif_get` | `project`, `item_id` | Conditional single-item projection with an opaque version validator. |
| `bif_history` | `project`, `item_id` | Separate bounded immutable event page. |
| `bif_selected_work` | `project` | One next-ready work projection; no assignment or claim. |

List/get default to `summary`; projections are `summary`, `work`, and `audit`.
List/history page limits default to 20 and range from 1 through 100. List
supports the existing named views and requester, assignee, status, priority,
unassigned, and literal text filters. List/history accept an opaque `cursor`.
Get accepts `known_version`. Discovery is the exact source for argument names,
enums and defaults. Unknown tool arguments, including actor, execution,
authorization, root, and config, are rejected. Item IDs must belong to the
supplied project for get/history.

An MCP tool result contains the canonical compact BIF v2 envelope as
`structuredContent` and the same JSON serialized in a text content block.
Application errors use `isError: true` with the normal v2 error envelope.
Malformed protocol messages, unknown methods/tools, and invalid MCP parameters
use JSON-RPC errors instead. Correlated errors retain a valid string/integer
request ID; errors without a valid correlation omit `id` under the pinned MCP
schema (they do not emit `id: null`). Stdout contains only newline-framed JSON-RPC;
diagnostics go to stderr.

## Framing and resource limits

| Bound | Value |
| --- | --- |
| Complete input/output JSON-RPC frame, including newline | 1,048,576 bytes |
| Inner BIF tool response JSON value | 131,072 bytes |
| Encoded JSON request ID | 1,024 bytes |
| Queued DB jobs / outgoing frames | 8 / 8 |
| Outstanding request-ID registry | 32 entries |
| SQLite prepared-statement cache | 64 entries |

The smaller inner budget reserves room for both structured content and its
JSON-string-escaped text representation. The adapter checks the **exact whole
frame** before writing it. Thus a large CLI projection can fit the CLI budget
but fail MCP's smaller budget; MCP pages can also stop earlier than CLI pages.
Both preserve complete records, use the last emitted continuation boundary,
and report their effective budget in `payload_too_large` details.

Input is bounded before JSON parsing; duplicate object keys are rejected at
every depth by the same strict JSON syntax helper used by RPC v1. IDs are
strings or integers, not null, booleans, or fractional numbers. Batches and
unsupported protocol features are rejected rather than silently implemented.
An outstanding ID cannot be reused before its response or cancellation finishes.
Queue saturation can return a server overload error; an exhausted output queue
closes the adapter rather than retaining unbounded frames behind a slow reader.
Clients should restart/retry independent reads after disconnect.

### Conditional get

The initial `bif_get` result is:

```json
{
  "api_version": 2,
  "schema_version": 1,
  "ok": true,
  "result": {
    "outcome": "modified",
    "version": "OPAQUE_TOKEN",
    "item": {"projection": "summary", "id": "DAVIS:my-project:001"}
  }
}
```

The item above is illustrative, not a complete projection fixture. Retain the
**complete** returned projection and its validator. Supplying it as
`known_version` for an unchanged matching projection returns:

```json
{
  "api_version": 2,
  "schema_version": 1,
  "ok": true,
  "result": {
    "outcome": "not_modified",
    "version": "OPAQUE_TOKEN",
    "item": null
  }
}
```

Validators bind store identity, item identity, projection/schema version and
revision. They are not authorization capabilities or signed proofs. Permission
and existence are checked first. A well-formed validator for another store,
item, projection, schema or revision is a cache miss (`modified`); malformed or
unknown-version validators are `invalid_input`. A missing item is `not_found`,
even with a malformed validator. A hit reads revision only and does not load
criteria or provenance.

Equivalent CLI calls are `bif --api-version 2 get ITEM_ID --conditional --json`
and `get ITEM_ID --known-version TOKEN --json`, with the same projection.
Plain v2 `get` retains its pre-Phase-D result shape.

### Selected work

`bif_selected_work` uses the existing next queue policy: ready items, priority
P0 through P4 then null, capture time ascending, canonical identity ascending.
It returns `result: {"outcome":"selected","item":WORK_PROJECTION}` or
`result: {"outcome":"empty","item":null}`. It never changes the ledger.
The equivalent CLI command is
`bif --api-version 2 selected-work --project PROJECT --json`.

Use the observed revision when starting the returned task separately. Another
writer may start or change it first; a later start can return `version_conflict`.
This operation provides selection, not reservation or execution authorization.

## Continuation and restart

Each request carries scope and continuation explicitly. There is no current
project, current task, retained result set, or mutable item cache. The process
retains only pinned configuration, connection identity and prepared statements.
All assembled records come from a short SQLite snapshot that closes before
transport output. Slow readers do not keep a WAL snapshot alive.

Queue cursors are live continuations, not historical snapshots. Keep the
operation, effective scope/filters, projection and ordering unchanged. A normal
process restart preserves valid cursors and validators. Concurrent membership
or sort-key changes may cause repeats or omissions; history uses append-only
revision/event order. See the [v2 read contract](bif-v2-read-contract.md).

Changes to schema, migration history, store identity, or database-file identity
make a warm session return `restart_required` with
`details.restart_required: true`; it does not reopen silently. Stop the host's
server and restart it against the intended store/configuration.

**Restore limitation:** no persisted generation exists until Phase E. An
in-place unsupported restore preserving file identity, store ID and revisions
cannot be reliably recognized by these tokens. Stop all servers before restore,
restart them afterwards, and invalidate retained validators/pagination. Do not
use old tokens to establish restored content equality.

## Cancellation and shutdown

Cancel an in-flight call using `notifications/cancelled` and its original
request ID. A separate bounded reader can interrupt active SQLite work; DB
operations remain serialized. Accepted cancellation suppresses the response
and releases request resources. Unknown/completed/malformed cancellation is
ignored as a normal race, and initialization cannot be cancelled.

There is no MCP `shutdown` method. Close stdin to disconnect and stop the
server. Disconnect cancels active/queued work and discards pending output,
instead of draining requests after the caller has left. Hosts may terminate a
process that does not exit within their deadline.
Never write logs or prompts to stdout.

## Release verification

Local protocol/process tests are not real configured-host evidence.
The [Phase D evidence report](bif-v2-phase-d-evidence.md) and
[release manifest](bif-v2-release-evidence.json) distinguish them.
The independent read-release gate remains blocked until an operator records a
real MCP host's version, exact launch configuration and list/get/history/restart
workflow. No model-token reduction is claimed without model evidence.
