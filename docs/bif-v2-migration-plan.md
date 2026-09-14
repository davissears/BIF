# BIF v2 migration plan

**Status:** Proposed; Phase B runtime implementation has not started.

**Purpose:** Deliver faster bounded local reads and lower agent workflow token
cost without weakening durability, authorization, compatibility, or recovery.

This plan refines the staged v2 efficiency proposal. It incorporates the
read-path evaluation and the decision to use **stateless application requests
over a persistent MCP process**. The [v1 plan](bif-v1-plan.md) and
[v1 backlog](bif-v1-tasks.md) remain the historical contract references.

`V2-NNN` identifiers below are document-local planning labels, not captured
BIF ledger item IDs. No assignees, priorities, or delivery dates are implied.

## 1. Delivery rules

- One task should normally produce one reviewable change. Split a task further
  if it requires unrelated changes; do not combine neighboring tasks merely
  because their numbers are adjacent.
- A task is complete only when its stated outcome and verification both exist.
  Behavioral tests belong with implementation, not only in the later gate task.
- Follow dependencies, not numeric order. Work in parallel only where the
  dependency graph and touched interfaces permit it.
- Retain the current domain lifecycle, authorization rules, canonical enums,
  ID formatting, and durability settings unless a task explicitly proposes a
  reviewed contract change.
- Add migrations; never edit checksummed, previously shipped migrations.
- Develop against disposable generated stores. Never initialize, migrate,
  prune, restore, or benchmark someone's active ledger as part of a test.
- Performance reports identify the source revision, fixture seed, hardware,
  filesystem, Rust and SQLite versions, build profile, and measurement method.
- Missing benchmark evidence blocks the corresponding performance claim, not
  unrelated correctness or implementation work.
- Do not ship a codec, cache, daemon, or new domain feature just to complete
  an experiment. A documented decision not to adopt it is a valid outcome.

## 2. Architecture decisions

### 2.1 Preserve the existing application boundaries

Extend the current narrow application/store interfaces with typed read requests
and results. Avoid a repository-wide rewrite or a mandatory all-purpose service
trait. Domain and application code must not depend on MCP or wire-format types.

The storage adapter should decode directly into the requested projection:

```text
CLI or MCP adapter
    -> validated request and trusted execution context
    -> authorized application operation
    -> bounded SQLite query / transactional mutation
    -> typed result
    -> transport serializer
```

Use `ItemSummary`, `ItemWork`, and `ItemAudit` for current-state projections.
Treat history as a separately paginated event collection, not an unbounded
field nested inside every audit item.

### 2.2 Preserve v1 contracts, not v1 inefficiencies

Existing CLI invocations and BIF RPC v1 keep their response shapes, offset
behavior, error conventions, mutation semantics, and one-shot framing.
Explicit v2 read options expose projections and cursors without silently
changing the old `--json` contract.

MCP is a new adapter and entry point, not a reinterpretation of `bif rpc`.
The new binary can serve v1 wire contracts against its upgraded store.
An old binary is not promised access to a newer schema.

V1 capture replay reconstructs a revision-1 item; mutation replay returns the
current item, which can have a later revision. Preserve those operation-specific
legacy behaviors. V2 may return a uniform acknowledgment of the original
committed operation without pretending those legacy responses were identical.

Idempotency keys remain store-global. Preserve payload canonicalization,
including mutation actor/execution attribution, across compatible retries.
Genuine v1 no-ops create neither an operation nor a receipt. Initially retain
that behavior in v2: return an explicit no-op acknowledgment with the observed
revision and no operation/change position. It is not a durably recorded retry
receipt; retry after an intervening change may produce a revision conflict.
Durably deduplicating no-op outcomes would require a separately reviewed
receipt/schema change, not a fictitious mutation or journal entry.

### 2.3 Bound primary rows before hydration

Filtering, ordering, continuation, and `LIMIT limit + 1` execute in SQLite.
Only selected page IDs may drive associated-record loading. Summary reads
must not instantiate complete items or query their criteria/provenance.

Targets count data statements separately from `BEGIN`, `COMMIT`, authorization,
connection initialization, and cursor/metadata checks; reports show both the
data-statement count and total request work.

- Summary page: one primary data query.
- Work page: primary rows plus batched criteria, normally two data queries.
- Audit page: primary rows with one-to-one provenance plus batched criteria,
  normally two data queries.
- History: bounded event query plus an existence check where required.

All queries assembling one result use one short read transaction. Close that
transaction before transport output can block on a slow consumer.

### 2.4 Treat queue cursors as live, not historical

Preserve the current order policies:

- List: capture time descending, canonical item identity ascending.
- Next: priority `P0` through `P4`, then null; capture time ascending;
  canonical item identity ascending.

Do not replace capture time with update time accidentally. SQL must reproduce
the Rust identity comparator, including numeric sequences above 999; plain
display-ID text ordering is not assumed equivalent. Mixed-direction sorts
need direction-aware boundary predicates, not one indiscriminate tuple `>`.

Live queue pages can repeat or miss items when membership or sort keys change.
An unchanged dataset must traverse exactly once. A sequence cutoff against
current rows is not a historical snapshot.

Reconciliation uses an immutable identity order over the full project scope
and replays changes from a position captured before scanning. Exact historical
multi-page snapshots and long-lived read transactions are out of scope.

### 2.5 Make MCP requests independent of conversational state

Requests carry explicit scope, projections, cursors, revision preconditions,
and idempotency keys as applicable. Trusted execution facts come from the
adapter/launcher, not arbitrary tool arguments or task text.

The MCP process may retain its SQLite connection, prepared statements, and
startup configuration. Losing those resources must not lose business state.
Requests cannot depend on an implicit selected project, current item, or
server-held previous result set.

Configuration is pinned for a process lifetime and changed by restarting it.
Mutable project registrations and ledger results are not immutable caches.
Store generation and supported schema must be checked at safe request
boundaries so restore/migration cannot leave a warm server silently stale.

MCP read tools can ship before a trusted host bridge for agent mutations.
Unverifiable mutation authorization must fail closed; process persistence
does not convert an agent into a directly authorized human.

### 2.6 Specify a state feed, not historical event replay

The first synchronization implementation is project-scoped and summary-only.
It returns complete replacement summaries; it is not an arbitrary filtered
feed or a JSON patch stream. Audit/history remains the immutable event API.

For a poll starting after `C`:

1. Capture a ledger high-water position `H` when the poll starts.
2. Select a bounded journal chunk in `(C, H]` for the requested project.
3. Coalesce IDs within that chunk and batch-load their current summaries in
   the same short read transaction used to select the chunk.
4. Return replacements, journal coverage, and an opaque continuation.
5. Continue toward the same `H`; a later poll may choose a new high water.

Let `R` be the last consumed journal sequence when another matching chunk
exists. Coverage advances to `R` in that case, or to `H` when the poll is
exhausted, including an empty project result. Coalescing must not advance past
unconsumed records. Limit journal rows examined/consumed as well as output
items and bytes; these are different bounds.

Hydrated current state can be newer than `H`. `through` describes covered
journal positions, not an as-of snapshot of returned objects. Consumers replace
objects monotonically by revision and atomically persist replacements and the
new cursor. Use one in-flight poll per mirror and compare the response's source
checkpoint and client-local bootstrap identity with the currently persisted
ones before committing it. A delayed response must not move the checkpoint
backward or modify a replacement baseline. Same-revision equivalent replay
within the accepted response is harmless.

The initial domain still has no delete, reopen, or project-move operation.
Rejected items remain items. Specify tombstone envelopes and consumer behavior,
but do not invent a destructive mutation to exercise them. A future deletion
feature must retain audit identity, allocate a higher revision, persist the
tombstone transactionally, and pass the deletion tests before it is enabled.
An unexpectedly missing current row is corruption, not an inferred tombstone.

### 2.7 Journal identity and recovery are part of correctness

- Use one change position per committed item operation, not per component
  event in a compound mutation.
- Use a non-reusing committed sequence allocator, such as an
  `INTEGER PRIMARY KEY AUTOINCREMENT` journal.
- Link journal metadata to existing operations; do not duplicate full audit
  payloads or attempt to infer global ordering from timestamps.
- Scope sequences to a persisted store identity and generation/epoch.
- Idempotent replay and genuine no-ops allocate no new state-change position.
  A note that advances item revision still requires synchronization.
- Initial upgrade starts a new generation with a full-bootstrap requirement;
  it need not backfill a fictional chronological order for historical events.
- Retention records a durable coverage floor. An old cursor fails explicitly.
- Restore of an older backup rotates the generation through a documented
  maintenance path. Silent out-of-band replacement cannot be magically detected
  by a process that has no external history of the store.
- Keep WAL, foreign keys, and the existing `synchronous=FULL` policy.

### 2.8 Bound payloads without silently dropping content

Start with the existing 100-item API page cap unless measurements justify a
reviewed change. Define byte and field-size handling separately: a bounded
item count does not bound a huge title or criteria collection.

Page-boundary byte truncation may stop before a complete item and return a
continuation after the last included item. Never truncate a field silently.
If one complete record exceeds the supported response budget, return a stable
actionable error; the consumer must not advance its cursor past that record.
Legacy v1 output retains its compatibility behavior.

### 2.9 Keep state ownership explicit

SQLite owns durable ledger truth. Cursors own continuation positions.
A host/client mirror owns the baseline necessary to interpret deltas.
An empty delta is useful only when that baseline still exists.

Provide a reference consumer and integration contract, not a generalized
server item cache. After context loss, a host must supply selected current
state from its mirror or perform a fresh bounded read. It must not present
an empty delta as a complete queue to a fresh agent.

## 3. Delivery milestones

| Milestone | Required outcome | Gate |
| --- | --- | --- |
| Baseline | Reproducible current-state and workflow measurements | V2-006 |
| Read release / v2.0 candidate | Bounded projections, indexed live cursors, legacy compatibility, persistent read-only MCP | V2-027 |
| Sync release / v2.1 candidate | Transactional journal, explicit recovery, reference consumer, project-summary synchronization | V2-044 |
| Mutation/workflow extension | Compact acknowledgments; trusted MCP mutations only for a verified host | V2-049; also V2-048 if MCP mutations are advertised |
| Optional codec release | Evidence-based codec decision; JSON always remains available | V2-053 |
| Migration readiness | Upgrade/restore documentation, automated gates, complete release evidence | V2-056 |

These labels describe delivery scope, not current Cargo version numbers or
permission to publish. Read improvements can ship without waiting for sync
or codec work. Apply the relevant upgrade/rollback documentation and CI gates
to every milestone, not only the final release.

## 4. Task backlog

### Phase A — Freeze contracts and establish evidence

#### V2-001 — Freeze compatibility and v2 request fixtures

**Depends on:** None.

**Scope:** Record the current CLI/RPC contracts and approved v2 projection,
ordering, error, and payload-boundary rules. Choose explicit v2 CLI opt-in
syntax; reject incompatible offset/cursor combinations. Document exact field
names and enum values, rather than the proposal's illustrative `active` status.

**Outcome:** Versioned fixtures distinguish preserved v1 behavior from new v2
behavior. Summary/work/audit and history are unambiguous.

**Verification:** Existing fixtures remain unchanged; new fixtures cover nulls,
P4, large numeric IDs, empty results, unknown fields, and oversized records.

#### V2-002 — Make disposable test stores collision-safe

**Depends on:** None.

**Scope:** Replace collision-prone temporary-directory construction with a
shared, safely unique test helper. Never recursively clean up a remembered
pathname: portable APIs cannot conditionally remove the originally created
directory after its name is replaced, so dropped test directories remain as
owned orphans rather than risking deletion of an unowned replacement.

**Outcome:** Parallel baseline and integration runs do not fail because two
tests selected the same timestamp-derived directory. Test artifacts can remain
in the platform temporary directory until its normal external cleanup.

**Verification:** Repeated parallel `cargo test --locked` runs pass without
`AlreadyExists` fixture failures; synchronized rename/recreate/drop coverage
proves a replacement ledger and the renamed owned orphan both survive. The
benchmark harness applies the same rule to its `bif-measurement-*` directories:
it retains the create-new snapshot handle and truncates only that exact file on
Drop, leaving the directory and any SQLite sidecars for external temporary-file
cleanup. Normal completion therefore leaves an empty main file rather than a
full 100k snapshot; crashes and SQLite sidecar-cleanup failures can still leave
larger owned orphans.

#### V2-003 — Generate representative benchmark stores

**Depends on:** V2-001, V2-002.

**Scope:** Add seeded disposable datasets with 100, 10,000, and 100,000 items.
Include multiple/skewed projects, P0-P4/null, sparse filters, long text, many
criteria, large histories, equal timestamps, and hot-item mutation bursts.
Generate valid domain/audit data rather than impossible random row combinations.

**Outcome:** One documented developer command regenerates each fixture and
records row counts, distributions, and seed.

**Verification:** Sample canonical loads and integrity checks pass; repeating a
seed gives the same logical contents. Large fixtures stay out of ordinary CI.

The generator atomically claims the explicit database and adjacent
`<database>.metadata.json` paths with create-new semantics. SQLite generation
uses a random basename distinct from the final database/WAL/SHM basenames in a
private staging directory. Immediately before SQLite opens that ambient path,
the generator checks that the staged pathname and retained handle still have
the same identity. The bundled SQLite Unix VFS then opens with
`SQLITE_OPEN_NOFOLLOW`, which rejects a symbolic component. Generation fails
closed before claiming outputs on non-Unix targets; no Windows reparse-point
protection is claimed. Same-file enforcement is supported on Unix filesystems
that expose stable device/inode identity through `stat`/`fstat`; other
filesystems receive only best-effort identity checks and are unsupported.

There is necessarily an interval between the same-file check and SQLite's
pathname open. Phase A supports accidental concurrency and no-clobber behavior
for public outputs, but assumes no same-user actor mutates the random private
staging directory or entry while the generator runs. Fully defending
regular-file or hard-link replacement in that interval requires binding SQLite
to the retained handle with a custom VFS and is outside Phase A. This is not an
arbitrary hostile/concurrent private-staging guarantee.

The checkpointed, verified database is published through the staged file handle
claimed with create-new semantics before SQLite opened it and through the
claimed final handle. The staged entry is checked again after SQLite closes and,
together with both final pathnames and the staging pathname, before success is
reported. Under the private-staging trust assumption above, SQLite therefore
never opens the final basename or touches unowned adjacent WAL/SHM paths, and a
substituted staged entry cannot become publication input. These guarantees do
not cover the documented regular-file or hard-link replacement in the
check/open interval.

Directory cleanup never recursively removes either staging or omitted-output
directories. Path-based `remove_dir_all` is vulnerable to pathname replacement,
and `cap-std` documents that even its open-directory recursive removal is not
guaranteed atomic with a concurrent rename. The generator therefore closes its
directory capabilities and preserves clearly named `.bif-benchmark-stage-*` or
`bif-benchmark-*` owned orphans for manual removal. After successful
publication, it truncates the staged main database only through the exact open
file handle used as the publication source; failures can leave a partial or
complete staged database. There is no pathname cleanup fallback. Pre-existing
database, metadata, WAL, and SHM paths are preserved byte-for-byte.
Explicit-output failure cleanup never
unlinks claimed final database or metadata names,
because a concurrently replaced pathname cannot be conditionally unlinked
portably; a failed run can leave an owned empty or partial claim for manual
removal rather than risk deleting an unowned replacement. The metadata names
the canonical digest algorithm/version. Its
digest covers `store_metadata`, projects, requester/project counters, items,
criteria, provenance, operations, and events in stable column/value order.

#### V2-004 — Instrument local read and write measurements

**Depends on:** V2-003.

**Scope:** Add a developer benchmark harness for statement counts, rows
returned, available SQLite work counters, DB/assembly/serialization time,
allocations, response bytes, DB/WAL size, and representative write latency.
Separate startup, warm execution, and actual filesystem-cold measurements.

**Outcome:** Machine-readable results can compare two source revisions without
changing durability or compiling timing-sensitive CI assertions into tests.

**Verification:** A known small fixture demonstrates the current `1 + 2M`
list data-query growth. Release-build runs report p50/p95 and sample counts.

Before publication, the harness writes the complete report to a
destination-directory temporary file and syncs the file. No existing
destination is overwritten. Publication atomicity is platform/filesystem
dependent: a hard-link/unlink fallback can leave the original owned temporary
link after interruption or unlink failure. The parent directory is not synced,
so this is not a crash-durability guarantee.

#### V2-005 — Define host-level workflow/token fixtures

**Depends on:** V2-001, V2-003.

**Scope:** Define planning, select-and-execute, triage, refresh, recovery, and
agent-handoff workflows with independently checkable successful outcomes.
Record the target host/model/tokenizer when available. Include v1 full JSON
and current compact human queue output as distinct baselines.

**Outcome:** A workflow evaluation specification includes schemas, requests,
host-injected responses, follow-up calls, errors, retries, cached/uncached
tokens, and completion time.

**Verification:** A recorded dry run accounts for all envelopes and identifies
unmeasured host/model data explicitly. Use synthetic content and user-approved
model access; never commit credentials or send private ledger data for a test.

#### V2-006 — Publish the baseline report

**Depends on:** V2-004, V2-005.

**Scope:** Run the local baseline matrix and any available authorized workflow
measurements before rewriting the read path. Record provenance and raw results.

**Outcome:** A checked-in report establishes measured baselines, known test
limitations, reference-machine details, and which token figures remain unknown.
The published evidence is the
[v2 pre-Phase-B baseline](baselines/v2-pre-phase-b/README.md).

**Verification:** Another developer can reproduce the commands and interpret
the results. No predicted percentage is presented as a measured improvement.
Raw harness results retain the V2-004 report-publication guarantees and
limitations; checking them into Git does not strengthen the harness's
platform/filesystem-dependent publication or crash-durability properties.

### Phase B — Implement bounded typed reads

#### V2-007 — Define typed projection results

**Depends on:** V2-001, V2-006.

**Scope:** Add distinct summary, work, and audit read types with documented
replacement/null semantics. Reuse canonical value types; keep transport types
out of the application layer.

**Outcome:** Callers can request work-relevant content without audit metadata,
or summary fields without descriptions, criteria, or provenance.

**Verification:** Type/fixture tests cover every field and show that summary
serialization cannot accidentally expose full-item content.

#### V2-008 — Add bounded read-store interfaces

**Depends on:** V2-007.

**Scope:** Add typed page requests/results and narrow projection read ports.
Do not carry forward a default implementation that materializes all items
before paginating. Keep legacy interfaces temporarily for adapter migration.

**Outcome:** A page's limits and ordering reach the SQLite adapter explicitly.

**Verification:** Fake-store application tests prove validation/authorization
occur before storage access and that no complete-loader fallback is required.

#### V2-009 — Encode canonical filters and sort boundaries

**Depends on:** V2-008.

**Scope:** Implement normalized effective filters, named-view expansion,
priority ranks, timestamp comparison rules, and direction-aware sort keys.
Preserve substring-search semantics; add no dependency graph or heuristic
`actionable` filter.

**Outcome:** SQL ordering, cursor boundary construction, and query fingerprint
normalization share one documented semantic definition.

**Verification:** Differential tests compare existing Rust order with candidate
SQL for equal timestamps, null priorities, sequences 999/1000, multiple
requesters/projects, and ascending/descending fields.

#### V2-010 — Implement indexed direct summary selection

**Depends on:** V2-009, V2-054.

**Scope:** Select only summary columns and necessary sort keys with SQL filters,
ordering, and `limit + 1`. Add a new migration containing only indexes justified
by the ready/list/active/mine benchmark shapes.

**Outcome:** Summary selection constructs no complete items and never loads
criteria or provenance unless an explicit text predicate needs criteria search.

**Verification:** One primary data statement at page sizes 1, 10, and 100;
documented `EXPLAIN QUERY PLAN` results for single- and multi-status queries,
sparse filters, and null-last order. Measure write and DB-size overhead.

#### V2-011 — Batch-load work projections

**Depends on:** V2-010.

**Scope:** Query selected item content, then criteria for only selected IDs,
ordered by criterion index. Use a short shared read transaction and bounded
parameter lists; do not hydrate the sentinel row unnecessarily.

**Outcome:** Work pages normally use two data queries, independently of page
length, with complete ordered criteria and no provenance reads.

**Verification:** Page sizes 1/10/100 have fixed counts; empty pages skip needless
queries; concurrent-writer tests never assemble a mixed-snapshot item.

#### V2-012 — Batch-load audit projections

**Depends on:** V2-011.

**Scope:** Join one-to-one provenance into selected primary rows and reuse
batched criteria assembly. Preserve missing/corrupt persisted-data errors.
Do not include all history events.

**Outcome:** Complete current audit pages normally use two data queries without
changing canonical field meaning or provenance completeness.

**Verification:** Differential full-object fixtures match v1 data; history query
counts stay zero for current audit reads; page-level snapshot tests pass.

#### V2-013 — Serialize typed v2 results directly

**Depends on:** V2-007, V2-012.

**Scope:** Add compact typed response serializers and bounded serialization
buffers at the adapter boundary. Do not convert projections through
`serde_json::Value`. Define omission only where replacement semantics allow it.

**Outcome:** JSON responses follow frozen v2 fixtures without an intermediate
dynamic object tree or a read transaction held during output.

**Verification:** Semantic JSON equivalence, escaping, Unicode, null clearing,
output-budget errors, and allocation comparisons are covered. Write failures
do not create successful-looking partial protocol responses.

### Phase C — Add cursors and preserve legacy reads

#### V2-014 — Implement the common opaque cursor envelope

**Depends on:** V2-009.

**Scope:** Add bounded versioned decoding with cursor-kind, store identity,
scope fingerprint, and sort/projection version fields. Reserve generation
support for the journal migration. Bind resolved `mine` identity, not just its
view name. Treat decoded values as untrusted parameter data.

**Outcome:** Malformed, wrong-kind, wrong-store, wrong-query, and unsupported
version cursors return stable errors and restart guidance.

**Verification:** Round trips, oversized input, invalid types, numeric overflow,
cursor swaps, and authorization reevaluation are tested. Document whether the
local-only trust model needs a MAC; never treat a MAC as authorization.

#### V2-015 — Add live keyset continuation to summary/work/audit

**Depends on:** V2-010, V2-012, V2-014.

**Scope:** Add direction-aware boundary predicates and `limit + 1` continuation
using the existing canonical orders and indexes. Derive the boundary from the
last actually returned record, including byte-budget stops.

**Outcome:** V2 pages continue without SQL OFFSET and make no historical
snapshot claim.

**Verification:** Static traversal has zero gaps/duplicates; insert, delete,
priority change, moved boundary, and invalid-cursor fixtures demonstrate the
documented live behavior. Deep-page measurements report rows/work, not just
elapsed time.

#### V2-016 — Paginate item history

**Depends on:** V2-013, V2-014.

**Scope:** Add a history page request using `(item_revision, event_index)` as
the append-only boundary. Reuse the existing supporting unique key and typed
event decoding.

**Outcome:** V2 history is bounded independently of an item's lifetime event
count and distinguishes a missing item from an empty history page.

**Verification:** Equal timestamps, compound operations, append between pages,
malformed event data, final pages, and oversized individual events are tested.

#### V2-017 — Route v1 reads through bounded loaders

**Depends on:** V2-012, V2-013.

**Scope:** Implement SQL-bounded legacy offset selection and reconstruct the
existing CLI/RPC full JSON shape. Retain one-shot RPC framing, legacy offsets,
history behavior, error mapping, and human formatting.

**Outcome:** Existing scripts work unchanged while list hydration is limited
to the selected page; v1 does not inherit v2-only fields or response budgets.

**Verification:** All v1 golden fixtures and integration tests pass. Legacy
offset remains explicitly documented as potentially expensive at depth.

#### V2-018 — Expose explicit v2 CLI reads

**Depends on:** V2-013, V2-015, V2-016, V2-017.

**Scope:** Implement the v2 opt-in syntax fixed in V2-001 for projection reads,
live cursors, and paginated history. Keep normal CLI invocation direct to core.

**Outcome:** A CLI consumer can perform complete v2 reads without MCP.

**Verification:** Process tests cover every projection, cursor resumption,
incompatible options, invalid input, clean stdout, and stable exit codes.

#### V2-019 — Enforce bounded-read regression tests

**Depends on:** V2-018.

**Scope:** Add small deterministic CI tests for query counts, projection
boundaries, ordering equivalence, snapshot assembly, and selected query plans.
Prefer plan properties over brittle full-plan string snapshots.

**Outcome:** CI detects reintroduced per-item loading or in-memory full-result
pagination without depending on machine-specific timing thresholds.

**Verification:** Deliberately restoring an N+1/fallback implementation makes
the relevant test fail. Count text-search predicates separately from hydration.

#### V2-020 — Publish read-path comparison results

**Depends on:** V2-006, V2-019.

**Scope:** Rerun cold/warm reads, deep pages, sparse filters, allocations,
database size, and writes on the original fixture seeds and machine.

**Outcome:** A comparison report attributes improvements to SQL bounds,
projections, batching, and indexes rather than claiming one combined magic
speedup. Query shapes missing targets have specific follow-up decisions.

**Verification:** Raw evidence reproduces the report; no SQL-count reduction is
misrepresented as an already-measured latency or token reduction.

### Phase D — Add persistent, request-stateless MCP reads

#### V2-021 — Introduce a reusable local application session

**Depends on:** V2-008, V2-014, V2-017.

**Scope:** Reuse an open connection, prepared statements, and pinned startup
configuration for sequential requests. Keep CLI/RPC v1 one-shot entry points.
Add schema/store identity checks and a restart-required outcome.

**Outcome:** Process restart loses only warm resources; no query meaning,
authorization, or continuation depends on hidden session data.

**Verification:** Interleave requests for two projects, restart the session,
resume a valid cursor, and change schema/store identity. No mutable item cache
or open transaction survives between requests.

#### V2-022 — Add MCP stdio lifecycle and bounded framing

**Depends on:** V2-021.

**Scope:** Select and record an MCP protocol/SDK version compatible with the
pinned Rust toolchain. Add a separate entry point, initialization, tool
discovery, bounded messages, stderr diagnostics, cancellation, and shutdown.
Serialize DB work initially rather than introducing a connection pool.

**Outcome:** One process handles multiple protocol-valid requests without
changing BIF RPC v1 or adding MCP dependencies to core types.

**Verification:** Protocol tests cover malformed/oversized requests, disconnect,
cancellation, stdout cleanliness, and a slow reader without a long DB snapshot.

#### V2-023 — Map a small MCP read tool surface

**Depends on:** V2-018, V2-022.

**Scope:** Map list/get/history to the same authorized core operations with
explicit project scope, named projections, and opaque cursors. Keep compact
JSON as the only initial codec and avoid exposing internal helper operations
as separate tools.

**Outcome:** MCP and CLI return semantically equivalent v2 results. Tool
descriptions state live-pagination and scope semantics.

**Verification:** Adapter parity tests cover filters, errors, projections,
cursor reuse, and untrusted execution metadata. Record actual schema size.

#### V2-024 — Add conditional single-item reads

**Depends on:** V2-013, V2-018, V2-023.

**Scope:** Return and accept an opaque known-version validator bound to store,
item, projection/schema version, and revision, plus generation once supported.
Return typed `not_modified` only for a matching validator. Check permission and
existence first; avoid criteria/provenance work on a hit. A summary validator
must not suppress a first work read at the same item revision.

**Outcome:** A consumer retaining the requested projection can refresh it
without receiving the same fields again.

**Verification:** Unchanged, changed, missing, wrong-store, and incompatible
projection/version cases are covered in both adapters and query counts.

#### V2-025 — Add a bounded selected-work read

**Depends on:** V2-011, V2-018, V2-023.

**Scope:** Expose one named operation selecting the next ready work projection
under existing queue policy. It is a read, not an assignment or claim, and
does not add a generic workflow query language.

**Outcome:** The execute-next workflow can obtain its selected task content in
one tool call instead of list-then-get.

**Verification:** One-call and existing two-call paths select equivalent work;
empty queues, concurrent changes, authorization, and revision conflicts at a
later start operation behave explicitly.

#### V2-026 — Verify warm-process and restart behavior

**Depends on:** V2-020, V2-024, V2-025.

**Scope:** Exercise realistic persistent read loops, caller disconnects,
request cancellation, concurrent CLI writes, bounded memory, and recovery
after server restart. Benchmark startup versus warm requests separately.

**Outcome:** Warm execution has measured resource/latency behavior and no
cross-request transaction or project-state leakage.

**Verification:** An existing cursor resumes after restart; cancellation does
not poison the connection; WAL growth and busy behavior remain bounded under
the test workload. Timing claims include the measured environment.

#### V2-027 — Gate the independent read release

**Depends on:** V2-019, V2-020, V2-026, V2-054, V2-055.

**Scope:** Review compatibility, the read benchmark report, MCP host smoke
tests, and the per-release upgrade/rollback instructions from V2-054. Finalize
the read-release runbook and required CI manifest, then rehearse the indexed
schema upgrade and rollback on a disposable pre-upgrade store.

**Outcome:** Approve or reject a read-focused v2.0 candidate with read-only MCP,
independently of sync, mutation MCP, or codec experiments. Existing CLI/RPC
mutations remain available.

**Verification:** All v1 and v2 read tests pass; at least one real configured
MCP host completes list/get/history/restart workflows. Missing model token
evidence is disclosed and does not become an unsupported release claim.

### Phase E — Introduce durable change tracking safely

#### V2-028 — Freeze synchronization and recovery fixtures

**Depends on:** V2-001, V2-014.

**Scope:** Turn sections 2.6-2.9 into exact request/response and state-machine
fixtures: fixed poll high water, covered position, complete replacement,
revision monotonicity, bootstrap, generation reset, and retention failure.

**Outcome:** There is one implementable state-feed contract, not competing
event replay, patch, and historical-snapshot interpretations.

**Verification:** Hand-worked timelines include A updated twice, a newer
hydrated version, no changes, other-project changes, queue exit, and lost
client baseline. Tombstone fixtures are clearly marked future-domain cases.

#### V2-029 — Add journal and generation schema

**Depends on:** V2-028, V2-054.

**Scope:** Add a transactional migration for store generation, durable
retention/high-water metadata, and a non-reusing operation-linked journal
with a measured `(project_id, change_seq)` index. Start a new generation
without ordering old audit events by timestamp.

**Outcome:** Fresh and upgraded stores have explicit sync identity and durable
sequence/coverage metadata. Sync endpoints remain disabled until writers
participate.

**Verification:** Migration rollback, repeat open, allocator overflow, emptied
journal, malformed metadata, and v1-store upgrade fixtures pass. Existing
immutable events, their protection triggers, and receipts remain intact.
Generate/review the new embedded checksum and reject missing/gapped migration
history through an explicit preflight policy rather than relying on MAX(version).

#### V2-057 — Bind existing readers to ledger generation

**Depends on:** V2-014, V2-021, V2-024, V2-029.

**Scope:** Upgrade list/history cursors, conditional validators, and warm-session
identity checks to include the persisted generation. Reject generation-less
read-release tokens with explicit restart/refetch guidance. Check identity in
the same transaction as cursor-dependent reads or mutation acceptance.

**Outcome:** Adding synchronization does not leave earlier read APIs or warm
connections able to accept stale identities after supported restore.

**Verification:** Page/history continuation and conditional reads succeed
across ordinary process restart but fail after generation rotation. A warm
session requests restart; an old validator never yields `not_modified` for
different restored content at the same item revision.

#### V2-030 — Journal successful captures atomically

**Depends on:** V2-029.

**Scope:** Append one operation-linked journal record and update coverage
metadata inside the existing capture transaction.

**Outcome:** Every new captured item has a durable change position; capture
replay allocates neither a new item nor a new journal record.

**Verification:** Capture, concurrent duplicate keys, rollback on journal
failure, and crash/reopen tests prove item/audit/receipt/journal atomicity.

#### V2-031 — Journal effective mutations atomically

**Depends on:** V2-029.

**Scope:** Add one change record per committed item operation, including
compound triage and note-driven revision changes. Preserve replay-before-stale
revision checks and genuine no-op behavior.

**Outcome:** Synchronization sees every committed revision without multiplying
one compound operation into several state-feed records.

**Verification:** All mutation paths, same-value no-ops, note-only changes,
idempotent retries, stale revisions, and journal insertion failures are tested.

#### V2-032 — Enforce the supported-writer boundary

**Depends on:** V2-030, V2-031, V2-057.

**Scope:** Prove old binaries refuse newer schema on fresh open; require an
exclusive operational maintenance window for upgrade, including stopping
already-running writers. Make new warm sessions reject unsupported schema
or generation changes before accepting another operation.

**Outcome:** The documented supported write paths cannot commit synchronized
state without journaling. Direct SQL and older already-open writers are not
silently treated as safe.

**Verification:** A real pre-upgrade binary fixture is refused after upgrade;
warm-session and mixed-process tests exercise maintenance/restart behavior.
Rollback injection proves no half-journaled operation survives.

#### V2-033 — Implement restore-generation reset

**Depends on:** V2-029, V2-032, V2-057.

**Scope:** Add an explicit offline maintenance path to rotate generation after
a supported backup restore, validate integrity, and require client bootstrap.
Use SQLite-consistent backup/restore procedures rather than copying an active
main file without its WAL state.

**Outcome:** A restored older store cannot accept cursors from the newer
generation through the supported restore workflow.

**Verification:** Restore an earlier disposable backup after later mutations;
old page/sync cursors fail and a new bootstrap succeeds. No down-migration or
automatic deletion of the later store is performed.

#### V2-034 — Implement explicit journal retention

**Depends on:** V2-029, V2-032.

**Scope:** Implement transactional prefix pruning and a durable retention
floor as an explicit maintenance operation, not an automatic v2 default.
Preserve audit events, sequence high water, and generation. Document that
in-flight clients can expire and must restart.

**Outcome:** Journal size can be controlled without silent gaps or sequence
reuse; callers can reliably detect unavailable coverage.

**Verification:** Prune none/some/all retained rows, restart, append, and compare
positions. Interleave pruning with page reads; each read either has complete
retained coverage or reports expiration, never a successful partial gap.

#### V2-035 — Gate the journal migration

**Depends on:** V2-032, V2-033, V2-034, V2-054, V2-055.

**Scope:** Run fresh/upgrade/crash/backup/restore checks plus write-latency,
DB-size, WAL, and idempotency regressions on representative stores. Extend
the initial runbook and CI framework with the exact journal-release procedures
and checks; rehearse them against a disposable pre-upgrade store.

**Outcome:** A journal migration report approves the storage foundation or
identifies a blocking correctness/write-cost issue before sync is enabled.

**Verification:** Every supported committed revision after tracking activation
has exactly one applicable operation change record; no old receipt or audit
history is rewritten.

### Phase F — Implement synchronization and its consumer

#### V2-036 — Implement scoped sync cursors

**Depends on:** V2-028, V2-029.

**Scope:** Add a distinct sync cursor carrying store/generation, project,
summary schema version, consumed position, and fixed high water while a poll
is being continued. Validate retained coverage within the read transaction.

**Outcome:** Sync cursors cannot be mistaken for page cursors, moved between
stores/projects, or used after incompatible recovery/schema changes.

**Verification:** Wrong-kind/version/scope, malformed position, future position,
expired floor, generation change, and restart fixtures pass.

#### V2-037 — Select bounded journal windows

**Depends on:** V2-035, V2-036.

**Scope:** Implement indexed `(C, H]` chunk selection, sentinel detection,
within-chunk ID coalescing, and exact coverage advancement. Measure the
no-change path and avoid rescanning irrelevant project history.

**Outcome:** A finite poll finishes at its fixed `H`, even with new concurrent
writes, and duplicate hot-item changes do not require duplicate output rows
within the chunk.

**Verification:** Sparse projects, sequence gaps, hot items, limits of one,
empty results, and continual writes cannot stall or skip the consumed interval.

#### V2-038 — Hydrate complete sync replacements

**Depends on:** V2-010, V2-013, V2-037.

**Scope:** Batch-load current summaries for the selected IDs in the same short
read transaction as journal selection. Include revision-only changes and
explicit replacement semantics. On output overflow, reduce consumed coverage
safely or fail without advancing it.

**Outcome:** Each returned summary is valid current state at the page's read
snapshot, potentially newer than the fixed poll high water; coverage is not
misrepresented as historical object versioning.

**Verification:** Two updates to one item, explicit clears, a newer version
between chunks, oversized rows, and unexpected missing item rows are covered.
Future tombstone envelopes are tested at the pure consumer/codec boundary,
not produced by treating rejection as deletion.

#### V2-039 — Add immutable-order bootstrap scanning

**Depends on:** V2-015, V2-028, V2-036.

**Scope:** Add a project-summary scan ordered only by immutable canonical
identity. Its initial response includes a sync starting position captured
before primary-row scanning. Bind page continuation to the same scope and
generation, without claiming historical visibility. Pair the scan with a
measured index supporting project scope and immutable identity order; do not
assume a queue's priority/time index also serves this traversal.

**Outcome:** Consumers can build a fresh baseline and replay changes that
happened during traversal.

**Verification:** Insert before/after boundary, update already-read items,
empty projects, restart, generation change, and retention expiry demonstrate
either eventual complete state or an explicit bootstrap restart. Query plans
and deep-scan measurements verify the immutable-order access path.

#### V2-040 — Implement a reference durable sync consumer

**Depends on:** V2-028, V2-038, V2-039.

**Scope:** Add a reusable example/test client maintaining project summaries
and cursor in a separate client-owned store. Apply complete replacements
monotonically by revision and commit state plus cursor atomically. Serialize
polls per mirror and accept a response only if its source checkpoint and
client-local bootstrap identity still match persisted state.

**Outcome:** The client can recover from duplicate responses, interruption,
and out-of-order hydrated revisions without regressing state.

**Verification:** Inject crashes before/after client commit, duplicate chunks,
equivalent same-revision updates, conflicting same-revision data, and synthetic
tombstones. Deliver an older response after a newer checkpoint or replacement
bootstrap commits; neither state nor checkpoint may regress. A missing
baseline triggers bootstrap rather than delta-only use.

#### V2-041 — Coordinate reconciliation and queue membership

**Depends on:** V2-040.

**Scope:** Build a fresh mirror via scan then replay from the initial position;
publish it only after reaching a chosen finite catch-up high water. Replace
old mirror contents instead of retaining items absent from a complete new
baseline. Derive ready/mine/active views from project state.

**Outcome:** Reconciliation converges when writes quiesce, and a task leaving
ready or changing assignee disappears from the corresponding local queue.

**Verification:** Compare mirror state with a complete database read after
adversarial interleavings; test empty reconciliation, interrupted bootstrap,
retention reset, client corruption reset, and baseline loss after agent handoff.

#### V2-042 — Expose scan and sync through CLI and MCP

**Depends on:** V2-023, V2-038, V2-039, V2-041.

**Scope:** Add explicit v2 scan/sync interfaces sharing the same core. Document
which host/client retains the baseline and how a fresh model receives selected
current state rather than an unexplained delta.

**Outcome:** Both transports can bootstrap and resume without server-held
consumer sessions. The reference consumer runs against either adapter.

**Verification:** Process/host tests resume after server restart, reject stale
cursors, handle no-change polls, and expose identical semantic results.

#### V2-043 — Stress synchronization correctness

**Depends on:** V2-042.

**Scope:** Add model/state-machine and multi-process tests interleaving captures,
mutations, polling, coalescing, bootstrap, crashes, pruning, and restore.
Keep test histories reproducible by seed.

**Outcome:** Sync correctness is established beyond isolated happy-path unit
tests and all failures retain a replayable operation trace.

**Verification:** After quiescence, the reference mirror equals current scoped
database state. There are no lost committed revisions, silently ignored
expiration gaps, or unauthorized cursor-based reads.

#### V2-044 — Gate the sync release

**Depends on:** V2-027, V2-035, V2-043.

**Scope:** Benchmark no-change and 1%/5%/25%-changed loops, hot-item updates,
reconciliation, output size, journal growth, and writes. Run at least one
configured host workflow with an explicit baseline owner.

**Outcome:** Approve or reject the sync release with recovery documentation,
measured benefits, and supported scope stated as project-summary state sync.

**Verification:** Correctness stress tests pass; workflow accounting includes
bootstrap/recovery costs. Do not claim that an empty delta works for a fresh
agent with no prior state.

### Phase G — Reduce mutation/workflow round trips safely

#### V2-045 — Freeze compact mutation acknowledgment semantics

**Depends on:** V2-001, V2-028.

**Scope:** Specify a typed v2 acknowledgment from durable operation/receipt
facts: item ID, committed revision, operation identity as needed, replay flag,
and an optional valid change position after journaling exists. Distinguish
original committed revision from a later current item revision. Define an
explicit no-op variant without a durable operation/change position, following
section 2.2. Preserve store-global key conflicts and attribution-sensitive
mutation hashes.

**Outcome:** Retrying a successful v2 operation has a stable meaning even after
another mutation changed the item. V1 replay output remains unchanged.

**Verification:** Fixtures cover retries after later writes, receipt conflicts,
pre-journal receipts, no-ops, and response loss. Presentation choices do not
accidentally change mutation identity or authorize additional changes.

#### V2-046 — Add compact acknowledgments to the v2 CLI

**Depends on:** V2-013, V2-018, V2-031, V2-045.

**Scope:** Return typed acknowledgments without loading/serializing a full item
solely to confirm success. Keep legacy capture's revision-1 reconstruction and
legacy mutation's full current-item replay response.

**Outcome:** V2 mutations can acknowledge known work compactly while preserving
authorization, revision checks, receipt behavior, and atomic journal writes.

**Verification:** Capture and mutation replay parity, original-versus-current
revision, no-op, stale-precondition, and uncertain transport outcome tests pass.

#### V2-047 — Define and test a trusted MCP mutation bridge

**Depends on:** V2-022, V2-045.

**Scope:** Document one concrete supported host's trusted execution and scoped
human-authorization channel. Keep launcher identity separate from assignee
and request-supplied actor metadata. If the host cannot supply trustworthy
authorization, leave lifecycle mutation tools unavailable. Map MCP to the
existing persisted `rpc` surface and supported host attribution values; adding
an `mcp` surface enum would require a deliberate domain/schema change. Keep
trusted attribution stable across reconnect/retry for the same operation.

**Outcome:** There is an auditable allow/deny contract, not an assumption that
an MCP tool argument proves human consent.

**Verification:** Forged actor/authorization fields, concealed agent execution,
scope escalation, expired authorization where applicable, and valid instructed
operations exercise core authorization. A blocked host integration is reported,
not bypassed.

#### V2-048 — Expose authorized MCP mutations

**Depends on:** V2-023, V2-046, V2-047.

**Scope:** Add a small mutation tool surface backed by the trusted bridge and
compact acknowledgments. Preserve named compound triage atomicity; do not
introduce arbitrary multi-item mutation batches.

**Outcome:** A supported host can safely mutate through the same core as CLI,
with durable retries and no hidden server-side selected-item state.

**Verification:** Real host instruction, revision conflict, duplicate request,
server restart after commit-before-response, denied scope, and capture
attribution tests pass. Unsupported hosts remain read-only.

#### V2-049 — Evaluate complete execution workflows

**Depends on:** V2-005, V2-024, V2-025, V2-046.

**Scope:** Compare select/get/start/finish, compound triage, conditional reads,
and acknowledgment workflows with v1. Include MCP mutation results only when
V2-048 is complete for the evaluated host.

**Outcome:** A report identifies which named operations reduce total calls and
tokens without hiding information needed to perform work.

**Verification:** Success is scored against task outcomes, not merely smaller
responses. Report unsupported host cases explicitly; defer unjustified tools.

### Phase H — Evaluate optional bulk codecs

#### V2-050 — Add experimental shared-schema JSON

**Depends on:** V2-013, V2-049.

**Scope:** Implement an opt-in codec for homogeneous summaries/history with
versioned columns and explicit scalar/null types. Keep it outside core types
and do not expand every tool schema with codec options prematurely.

**Outcome:** The same typed projection can be encoded as compact object JSON
or schema-once rows for controlled comparisons.

**Verification:** Round-trip/equivalence tests cover empty rows, nulls, Unicode,
commas, quotes, newlines, mixed priorities, and adversarial title content.

#### V2-051 — Add or reject an experimental TOON codec

**Depends on:** V2-050.

**Scope:** Review the actual library/version, grammar, license, dependency
cost, and supported shapes. If acceptable, implement an opt-in boundary codec;
otherwise record why the experiment stops.

**Outcome:** TOON is either testable against the exact same projections or
explicitly deferred. It is never required for storage or mutation correctness.

**Verification:** Equivalent escaping/null/type fixtures and supported
encode/decode checks pass. Unsupported nested structures retain JSON.

#### V2-052 — Run host-level codec trials

**Depends on:** V2-005, V2-044, V2-049, V2-050, V2-051.

**Scope:** Compare available codecs on repeated planning/refresh/history
workflows with actual host-injected schemas and envelopes. Include 1/10/50/100
item shapes; test 500-row codec fixtures separately without raising the API
page cap implicitly.

**Outcome:** Results include total tokens, calls, successful completion,
interpretation errors, serialization cost, allocations, and debugging costs.

**Verification:** Predeclare repetition and task-success non-inferiority
criteria. Include tokenizer/model versions and variability; byte savings alone
cannot satisfy the gate.

#### V2-053 — Select defaults from evidence

**Depends on:** V2-052.

**Scope:** Publish the adoption decision for each tested shape. Treat the
proposal's 15% median token advantage as a candidate threshold, conditional
on task quality, operational cost, and repeatable evidence.

**Outcome:** JSON remains the default unless another representation has an
established workflow advantage. A codec may remain opt-in or be removed.

**Verification:** Defaults match the report and adapter-equivalence tests.
No release depends on making TOON win.

### Phase I — Maintain release and operator readiness

#### V2-054 — Draft the initial upgrade and rollback runbook

**Depends on:** V2-001.

**Scope:** Before the first schema change, document how to inventory old
binaries/processes, stop writers, take and verify a
SQLite-consistent backup, upgrade a disposable copy, perform integrity checks,
upgrade the real store only through an operator-authorized workflow, and
verify CLI/host reads and writes. Separate existing executable procedures from
later milestone-specific steps that do not yet exist.

**Outcome:** Operators understand wire compatibility versus binary/schema
compatibility, maintenance downtime, required server restarts, and rollback.
This initial deliverable can complete before V2-010; V2-027 and V2-035 own
their concrete release-specific updates and rehearsals.

**Verification:** Rehearse the existing backup/integrity workflow on a disposable
v1 store and review the upgrade checklist before schema changes. State
that restoring a pre-upgrade backup discards later writes unless separately
preserved/reconciled; never present destructive restore as a transparent undo.

#### V2-055 — Establish the CI and release-evidence framework

**Depends on:** V2-002, V2-004.

**Scope:** Run the existing fast suite and baseline instrumentation in a
documented CI framework with scheduled large-fixture comparisons and a
machine-readable release-evidence manifest. Define where later tasks register
their functional/query-count/migration/protocol checks and host evidence.

**Outcome:** CI distinguishes correctness failures, structural performance
regressions, environment-sensitive measurements, and optional experiments.
The framework completes now; each behavior task adds its actual checks and
V2-027/V2-035/V2-044 validate the applicable release manifest.

**Verification:** The matrix records required checks per release; benchmark
artifacts include source provenance. Ordinary CI needs no private ledger,
model credential, or arbitrary absolute timing threshold.

#### V2-056 — Publish milestone evidence and user guidance

**Depends on:** V2-054, V2-055, and the gate for the milestone being released.

**Scope:** Update README, CLI help, protocol documentation, examples, and
agent integration guidance to describe only capabilities actually delivered.
Attach benchmark/correctness reports and remaining limitations.

**Outcome:** A reviewer can determine what shipped, how to migrate, how to
resume after restart, where client state lives, and which performance claims
are supported.

**Verification:** Run documented fresh-install, v1 compatibility, v2 read,
restart, and milestone-specific recovery examples in disposable environments.
No automatic package publication or live-store migration is part of this task.

## 5. Parallel work and critical dependencies

- V2-001 and V2-002 can start independently. Fixture generation follows both.
- Typed read work begins after the baseline report, preventing accidental loss
  of the before measurements.
- Cursor primitives can proceed alongside bounded work/audit hydration after
  filter/order semantics are fixed.
- The reusable session/MCP lifecycle can proceed alongside read integration;
  MCP tool mapping waits for working core operations.
- Sync contract work can proceed after cursor semantics are fixed. Do not
  enable sync endpoints until every supported writer journals atomically.
- V2-057 is deliberately placed next to the generation migration it consumes;
  it upgrades existing readers before restore verification or journal release.
- Capture and mutation journaling can be separate changes against one reviewed
  schema; coordinate edits to the storage module.
- Compact acknowledgment and host-authorization design can proceed in parallel
  with sync read implementation; neither may redefine v1 replay semantics.
- Initial runbook/CI framework tasks finish before their dependents; subsequent
  behavior tasks and release gates own concrete additions and rehearsals.
- Codec experiments follow usable workflows. They are not on the read or sync
  correctness critical path.

## 6. Release acceptance matrix

| Area | Required evidence |
| --- | --- |
| Compatibility | Existing CLI and RPC v1 golden/process tests pass using the new binary |
| Bounded reads | Fixed small data-query counts and no whole-result hydration |
| Ordering | SQL matches canonical priority, time, and identity comparison |
| Pagination | Exact static traversal; documented and tested live mutation behavior |
| Serialization | Typed v2 responses, explicit null semantics, bounded output/error behavior |
| Local speed | Reproducible before/after timings and query work, including write costs |
| MCP | Same core semantics, warm resource reuse, stateless request meaning, restart tests |
| Authorization | Trusted context separate from arguments; unsupported mutation hosts fail closed |
| Journal | Atomic revision/change records, replay/no-op behavior, non-reused committed positions |
| Synchronization | Complete covered intervals, monotonic consumer application, quiescent convergence |
| Recovery | Cursor expiration, consistent backup/restore, generation reset, full bootstrap |
| Token efficiency | Actual configured-host workflow accounting, including baseline loss/recovery |
| Codecs | Equivalent information and measured task quality, not only smaller byte counts |

The original percentage goals remain investigation targets: 40% fewer tokens
for summary queues, 50% with an adopted bulk codec, 90% for no-change refresh,
80% for 5%-changed refresh, and 2x faster lists where reconstruction dominates.
Report achieved results per workload; do not average away a failed correctness
case or silently change the baseline to hit a number.

## 7. Explicitly deferred scope

- A separate `bifd` daemon, HTTP transport, remote/multi-machine replication.
- Historical as-of item snapshots or cross-call SQLite read transactions.
- General server item caches, connection pools, materialized summaries.
- FTS5 until substring-search profiling justifies both cost and semantic change.
- Arbitrary field masks, arbitrary query languages, dependency graph features.
- Delete, restore-deleted-item, reopen, and project-move domain operations.
  Offline database backup restoration is a separate operational concern.
- Arbitrary multi-item mutation batches and automatic journal pruning.
- TOON as a required format or codec-specific domain/storage representations.

Each needs its own scope, correctness contract, measured justification, and
reviewed tasks rather than being smuggled into this migration.
