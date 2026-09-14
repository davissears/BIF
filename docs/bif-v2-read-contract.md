# BIF v2 read contract

**Status:** Approved contract for V2-001; runtime implementation is deferred.

This document freezes BIF v1 compatibility and the first BIF v2 read contract.
Its machine-readable companion is
[`fixtures/bif-v2-read-contract.json`](fixtures/bif-v2-read-contract.json).
That fixture is normative where an example and prose could otherwise differ.

## Compatibility boundary

Invocations without an explicit v2 selector remain v1:

- existing CLI spelling, human output, `--json` output, offset pagination, exit
  codes, and errors do not change;
- `bif rpc` remains one-shot BIF RPC v1 with the envelopes and strict input
  rules in [`fixtures/bif-v1-rpc.json`](fixtures/bif-v1-rpc.json);
- the four `bif-v1-*.json` fixtures remain byte-for-byte unchanged;
- v1 `get`, `list`, and `next` continue to return complete v1 items, and v1
  `history` remains an unbounded complete history;
- v1 has no response-byte budget and does not acquire v2 cursor or projection
  fields.

The v2 CLI is selected only by placing `--api-version 2` before the read
command:

```text
bif --api-version 2 get ITEM_ID [--projection summary|work|audit] --json
bif --api-version 2 list [VIEW] [FILTERS] [--projection summary|work|audit] \
    [--limit N] [--cursor TOKEN] --json
bif --api-version 2 next [FILTERS] [--projection summary|work|audit] \
    [--limit N] [--cursor TOKEN] --json
bif --api-version 2 history ITEM_ID [--limit N] [--cursor TOKEN] --json
```

`--api-version` after the command is invalid. V2 reads require `--json`;
human v2 rendering is not part of this contract. `--offset` is a v1-only
option: v2 rejects it whether or not a cursor is also present. Supplying both
`--offset` and `--cursor` is always `invalid_input`. `history` rejects
`--projection`; `get` rejects `--cursor`, `--limit`, and `--offset`.

This syntax reserves an explicit compatibility boundary without implementing
Phase B projections, SQL changes, or cursor handling in V2-001.

The corresponding validated application requests have these exact fields:

- `get`: `item_id`, `projection`;
- `list`: `view`, `project`, `requester`, `assignee`, `status`, `priority`,
  `unassigned`, `text`, `projection`, `limit`, `cursor`;
- `next`: `project`, `requester`, `assignee`, `status`, `priority`,
  `unassigned`, `text`, `projection`, `limit`, `cursor`;
- `history`: `item_id`, `limit`, `cursor`.

Optional filters and cursors may be absent. `view` defaults to `all`,
`unassigned` defaults to `false`, `projection` defaults to `summary`, and
`limit` defaults to 20. Limits are integers from 1 through 100. `assignee` and
`unassigned: true` are mutually exclusive.

V2 does not add a BIF RPC protocol version: `bif rpc` continues to accept only
RPC v1. The future MCP adapter may wrap these application results in MCP
protocol framing, but must not change their field semantics.

## Projection schemas

Projection names are the exact, case-sensitive values `summary`, `work`, and
`audit`. Every listed field is required and no other field is emitted at
projection schema version 1. Nullable fields are encoded as JSON `null`, not
omitted. Arrays are present even when empty.

`summary` contains, in serializer order:

```text
id, title, status, priority, assignee, revision
```

`work` contains the summary fields followed by:

```text
description, acceptance_criteria, status_reason
```

`audit` contains the work fields followed by:

```text
requester, project, sequence, captured_at, updated_at, provenance
```

`provenance` always contains:

```text
source_host, thread_id, message_id, url,
repository_reference, revision_reference, context_excerpt
```

All provenance fields are nullable. `acceptance_criteria` is an ordered array
of strings. `revision` and `sequence` are positive JSON integers and must be
handled as unsigned 64-bit values; implementations must not narrow them or
round-trip them through IEEE-754 floating point.

Closed enum values are:

- `status`: `proposed`, `ready`, `in_progress`, `blocked`, `done`, `rejected`;
- `priority`: `P0`, `P1`, `P2`, `P3`, `P4` (or `null`);
- `source_host`: `delta`, `codex`, `local` (or `null`);
- `projection`: `summary`, `work`, `audit`;
- `event_type`: `captured`, `approved`, `rejected`, `started`, `blocked`,
  `resumed`, `finished`, `priority_changed`, `assignee_changed`, `note_added`;
- actor `kind`: `human`, `agent`;
- execution `kind`: `direct`, `agent`.

Enum matching is exact and case-sensitive.

## Result and history semantics

`get` returns `{"item": PROJECTION}`. `list` and `next` return
`{"items": [PROJECTION...], "next_cursor": STRING_OR_NULL}`. An empty page is
exactly `{"items": [], "next_cursor": null}`.

The v2 CLI writes exactly one JSON envelope and one trailing newline. A success
envelope contains exactly `api_version`, `schema_version`, `ok`, and `result`;
their values are `2`, `1`, `true`, and the operation result above. An error
envelope contains exactly `api_version`, `schema_version`, `ok`, and `error`;
their values are `2`, `1`, `false`, and the error object defined below.
Diagnostics go only to stderr. No partial envelope is a successful response.

`summary` is sufficient for queue selection only. `work` is the complete
current-state content normally needed to execute a selected item. `audit` is
the complete current item and provenance at one read snapshot; it does **not**
contain history. A more detailed projection is a replacement object, not a
patch over a less detailed projection.

History is a separate paginated collection:

```text
{"item_id": ITEM_ID, "events": [EVENT...], "next_cursor": STRING_OR_NULL}
```

Every event contains exactly:

```text
operation_id, event_id, item_revision, event_index, event_type,
before, after, actor, execution, reason, note, occurred_at, schema_version
```

`before`, `after`, `reason`, and `note` are nullable but never omitted.
`actor` contains exactly `kind`, `id`, `surface`, and `host`. Direct execution
contains exactly `kind`, `surface`, and `host`; agent execution additionally
contains `agent_id`. A missing item is `not_found`; an existing item with no
events returns an empty successful page.

## Deterministic order and live pagination

List order is:

1. `captured_at` descending;
2. requester ascending;
3. project ascending;
4. numeric sequence ascending.

Next order is:

1. priority rank `P0`, `P1`, `P2`, `P3`, `P4`, then `null`;
2. `captured_at` ascending;
3. requester ascending;
4. project ascending;
5. numeric sequence ascending.

History order is `(item_revision ascending, event_index ascending)`.
Identity comparison uses canonical requester/project strings and the numeric
`u64` sequence. Display-ID lexical order is not valid: sequence `999` sorts
before `1000`.

Cursors are opaque, versioned, and bound to operation kind, store, effective
scope and filters, order, projection schema, and the last returned record.
They describe a live continuation, not a historical snapshot. With unchanged
data, traversal has no duplicates or omissions. With concurrent changes,
membership or sort-key movement may cause repeats or omissions; callers that
need reconciliation must use the later synchronization contract.

## Strict input, errors, and payload boundaries

Every v2 input object rejects duplicate or unknown fields with `invalid_input`.
Unknown response fields are reserved for a later response schema version and
are not emitted by schema version 1. Consumers must select behavior by the
declared schema version rather than guessing from extra keys.

V2 read error codes are the exact values:

```text
invalid_input, not_found, unauthorized, unsupported_version,
not_initialized, storage_busy, invalid_cursor, payload_too_large, internal
```

The JSON error object always contains exactly `code`, `message`, and `details`.
`message` is nonempty diagnostic text and is not stable for matching.
`details` is always an object. Stable details are specified in the fixture.
Validation failures occur before storage access; authorization is re-evaluated
for every request, including cursor continuation.

Existing error exit codes stay unchanged. `invalid_cursor` exits 2 because it
is invalid request input; `payload_too_large` exits 11. Error responses recover
no request ID because the v2 CLI has no request-ID field.

The maximum encoded v2 request and response are each 1,048,576 bytes, measured
as UTF-8 bytes. Item count remains capped at 100. Implementations select at
most `limit + 1` primary rows, then include only complete records that fit.
They never truncate a string, array, object, UTF-8 sequence, or history event.

When complete records fit only up to a page boundary, the response succeeds
with a cursor derived from the last record actually returned. If one record
cannot fit in an otherwise empty response, the request fails with
`payload_too_large`; no success prefix is written and a supplied cursor is not
advanced. The error details contain `record_kind`, `record_id`,
`maximum_response_bytes`, and `minimum_required_bytes`. Request bodies above
the request limit fail with `invalid_input` before request ID recovery.

The oversized-record fixture freezes a compact-JSON construction recipe rather
than checking a megabyte-scale literal into source. Validation replaces the
work item's `description` with exactly 1,048,576 ASCII `x` characters, wraps it
in the specified v2 `get` success envelope, serializes it with compact
`serde_json`, and measures the resulting UTF-8 bytes. That measured value is
the expected error's `minimum_required_bytes`.

These are adapter-boundary rules only. V2-001 deliberately adds no runtime
projection, serialization, pagination, or query implementation.
