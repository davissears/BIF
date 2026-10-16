# BIF v2 migration plan

## Before Phase F — Required preparation

These tasks amend the Phase F contract and implement its activation prerequisite;
they do not approve a sync release or live-store activation.
`F-PNN` identifiers are document-local planning labels.
Complete them before starting V2-036–V2-044. Existing Phase E evidence remains
historical evidence, not proof of a synchronization runtime.

**Required order and completion evidence:**

1. **Before runtime implementation:** Complete F-P01's fixture/schema and recovery
   contract amendments, then F-P02's model corrections and regression cases.
   Review them together against this plan and existing application semantics;
   resolve contradictions before treating the amended contract as frozen.
   Passing the existing, unamended tests does not satisfy this gate.
2. **First runtime prerequisite:** After F-P01/F-P02 verification passes and
   V2-035's storage-foundation gate is satisfied, implement F-P03 with focused
   tests written first. Verify activation/recovery on disposable stores and
   review the change before marking the prerequisite complete.
3. **Before the main Phase F implementation:** Confirm F-P01–F-P03 outcomes and
   verification evidence are complete, then start V2-036–V2-044 in dependency
   order, including V2-040A. Do not defer task-level tests to the stress gate.
4. **Before release or live use:** Complete V2-044's evidence and operational
   decisions, and obtain separate authorization for live-store operations.
   Choosing an actual deployment path, owner, retention policy, or maintenance
   window is not required to begin the contract/model preparation.

Notation: `S` is bootstrap start, `C` is durably applied coverage, `F` is the
retention floor, and `H` is a poll's fixed high water. These are server/model
positions; consumers never decode or order opaque cursor tokens.

**Confirmed decisions:**

- Activation/reactivation requires stopping source processes and running an
  explicit offline maintenance command, consistent with supported restore.
- The consumer requires an explicit client-owned SQLite mirror path; it does
  not choose a machine-local default.

#### F-P01 — Freeze activation and mirror lifecycle contracts

**Depends on:** V2-028, V2-029, V2-033.

**Scope:** Update the sync fixture/schema and recovery documentation to define:

- Explicit authorized offline activation with source processes stopped, an
  expected generation, prerequisites proving supported writers journal, atomic
  enablement, and retry/crash behavior.
  Distinguish initial journal/schema creation from enabling sync service:
  service activation preserves generation, floor, high water, and client state.
  Restore rotates generation and disables service; restart does neither.
- Server availability is independent of mirror completeness. Keep the legacy
  `bootstrap_required` column for compatibility, but exclude it from new
  runtime client-progress decisions. Never clear it because a client finished.
- Typed client status: published-baseline completeness, rebuild phase, last
  successful finite catch-up, last failure/recovery action, and last-observed
  source compatibility. Define transitions for transient failure, expiration,
  generation mismatch, missing baseline, corruption, and unfit records.
- Stable error details and precedence: authorize normally; report incompatible
  source identity before service-disabled status where identity is supplied.
  All scan/poll requests, including initial scan, fail closed while disabled.
  An unfit-record error identifies the item, required bytes, and effective budget.
- One writer owner across polling and reconciliation; persisted staging
  identity; request-attempt guards for both success and failure callbacks.
  Serialize scan/poll requests within that owner. A rebuilding mirror keeps
  its previous usable baseline readable but not updating; detected corruption
  prevents serving that baseline, and incompatible history stays labeled.
- During scan validate original start `S`; during replay validate the durably
  applied checkpoint `C`. `S < F <= C` is valid; `C < F` requires rebuilding.

**Outcome:** One normative transition/error contract, usable by both adapters
and the consumer, with no unresolved activation or mirror-location choice.
Status means last observed coverage, never guaranteed current state.

**Verification:** Example cases validate against the strict schema for disabled
service, independent clients, restore/reactivation, empty versus missing baseline,
abandoned callbacks, and permanently unfit summaries. F-P02 checks their modeled
behavior; neither task establishes runtime correctness.

#### F-P02 — Correct and extend the synchronization test oracle

**Depends on:** F-P01.

**Scope:** Update focused contract-model tests before runtime implementation.
Match existing ready/mine/active membership; `mine` excludes done/rejected.
Add competing bootstrap publication, delayed success/error, duplicate no-change,
`S < F <= C`, `C == F`, `C < F`, and unfit-single-summary cases. Preserve tests
for coverage bounds, replay, baseline loss, and revision conflicts.

**Outcome:** Fixtures and models enforce the amended contract without becoming
a competing definition of domain semantics. Coalescing need not deliver every
intermediate revision; it must not skip required coverage.

**Verification:** Tests reject deliberately regressed membership, publication,
coverage, and freshness transitions. Run
`cargo test --locked --offline --test v2_sync_contract_fixtures --test named_views`.
Report model-only coverage separately from storage/runtime coverage.

#### F-P03 — Implement explicit sync activation and recovery controls

**Depends on:** V2-035, F-P01, F-P02.

**Scope:** Implement the explicit offline maintenance activation command,
requiring stopped source processes, expected-generation validation,
writer-participation prerequisites, and an atomic availability transition.
Add a storage guard reusable by scan/poll reads in their own read snapshot.
Update the maintenance runbook. Do not activate on ordinary open, migration,
client bootstrap, or restore; do not implement online pruning or a client registry.

**Outcome:** A prepared source can be explicitly enabled and a restored source
explicitly re-enabled without changing journal history or marking clients ready.
The reusable guard enforces availability and identity-error precedence; cursor
decoding and end-to-end enforcement remain V2-036/V2-042 work.

**Verification:** On disposable stores test prerequisite/generation rejection,
retry, rollback on failure, restart persistence, and restore → disabled → explicit
reactivation. Add adapter-level enforcement tests with V2-042. No live-store
operation is authorized by completing this task.

## Current delivery status

**Status:** Phase D read implementation, local evidence, and real configured MCP
host workflows are recorded; the operator has approved candidate validation and
compatibility review. Live-store maintenance requires separate authorization.

V2-007 through V2-020 are implemented. The
[Phase B evidence report](bif-v2-phase-b-evidence.md) records bounded query,
snapshot, serialization, index-selection and cost evidence with limitations.
The [Phase C evidence report](bif-v2-phase-c-evidence.md) adds strict opaque
cursors, live queue/history continuation, bounded compatible v1 pages, explicit
v2 CLI reads, regression gates, and the three-scale release comparison.
The [upgrade and rollback runbook](bif-upgrade-runbook.md) satisfies the
reviewed V2-054 prerequisite and now includes the indexed-read candidate.
V2-021 through V2-026 are implemented with local protocol/session coverage,
three-scale release-profile startup/warm measurements and a disposable
historical-binary upgrade/rollback rehearsal. See the
[Phase D evidence report](bif-v2-phase-d-evidence.md) and machine-readable
[release manifest](bif-v2-release-evidence.json). V2-027's candidate gate is **complete**:
the required [real configured-host workflows](baselines/v2-phase-d/configured-host/host-evidence.json)
are recorded, and [operator signoff](baselines/v2-phase-d/configured-host/operator-signoff.json)
accepts candidate validation and compatibility review. The release gate is
`approved`, with `approved=true` and `approval` referencing the decision;
real-ledger rollout still requires separate authorization.
The historical local measurement tree is dirty `c5605ce` plus the parent protocol
error-ID fix and live-pagination discovery descriptions;
artifact hashes identify its actual production inputs, not a clean final
revision. The old binary is an archived historical schema-2 build, not a
retained operator-approved production executable. The Phase E storage foundation
keeps `sync_enabled=0` and `bootstrap_required=1`; actual sync endpoints,
bootstrap and reconciliation remain Phase F. No model-token savings have been
measured.
V2 rejects noncanonical persisted requester/project coordinates before
returning projection keys; v1 retains its accepted normalization and raw-filter
behavior. Comparison evidence reports residual legacy sorting, sparse/text
scans, and depth-sensitive next priority groups rather than claiming universal
constant-work pagination.

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

Wire consumers must use explicit coverage and continuation, not infer coverage
from dense sequence numbering or the number of replacements. Project-filtered
pages can be sparse even when the global journal is contiguous. The abstract
synchronization contract also permits gaps in global positions; its gapped
fixtures do not assert that current maintenance accepts such a store.

Hydrated current state can be newer than `H`. `through` describes covered
journal positions, not an as-of snapshot of returned objects. Consumers replace
objects monotonically by revision and atomically persist replacements and the
new cursor. Use one cross-process writer owner per mirror across polling,
bootstrap, and publication, with one active scan/poll request within that owner.
Compare the response's source checkpoint and client-local baseline/staging
identity with persisted state before committing.
Guard status changes with local request-attempt ownership too: duplicate
no-change responses can share a checkpoint without being fresh observations.
A delayed success or failure must not modify a replacement baseline or its
health. Same-revision equivalent replay is harmless; conflicting data at the
same revision aborts the entire commit.

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

The current supported allocation transaction and prefix-pruning policy provide
a stronger storage invariant than the abstract wire contract: the retained
global range `(F, H]` is contiguous, where `F` is the durable retention floor and
`H` is the durable high water. Current maintenance validates that the retained
row count is `H - F`, alongside operation links and allocator metadata; a hole
in that range fails integrity validation. SQLite `AUTOINCREMENT` alone is not
a universal density proof. Keep both rules: abstract wire consumers tolerate
gaps, while current storage maintenance enforces its contiguous retained range.
See the [maintenance validation and recovery runbook](bif-upgrade-runbook.md#current-schema-offline-backup-and-restore-v2-033).

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
The standalone, transport-neutral consumer owns a separate machine-local SQLite
mirror. It is not Git-backed, a server item cache, or a cross-machine replica.
Server availability, generation, and retention are separate from each mirror's
baseline completeness and checkpoint. No server registry tracks client progress;
clients outside retained coverage rebuild independently.

Build replacements in staging, then atomically publish the baseline identity,
contents/reference, completeness, and checkpoint. Keep a previous complete,
usable baseline readable but pause its updates during rebuilding. Never expose
staging as a complete queue. A cursor without its baseline cannot resume
incremental use.

Expose completeness, health, and last observed coverage through both transports
and agent handoff. A complete stale baseline may support browsing and provisional
preparation, not authoritative eligibility, ownership, authorization, or mutation.
Generation-incompatible data must be identified as old-history data; corrupt
data must not be presented as a usable mirror. Context loss requires selected
current state from a usable baseline or a fresh bounded read, not an empty delta.

Phase F adds no offline write queue, assignment-aware claim, ACL/revocation
system, or external-action guarantee. Existing write validation remains
authoritative. Summary fields support named-view membership, not reconstruction
of canonical list/next ordering. Arbitrary semantic corruption is not automatically
detectable; do not promise more than specified integrity checks and reconciliation.

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

Before metadata validation or backup opens the canonical source through SQLite,
the harness fails closed when the canonical WAL exists without its canonical
SHM. Read-only SQLite can otherwise create that missing public SHM path. The
preflight does not create or remove companion paths; DB-only sources and
active-WAL sources with both companions remain supported, including transient
SHM coordination-byte changes. Canonical/lexical alias handling follows the
documented source and output policy. The check/open interval retains the Phase A
immutable-source precondition rather than claiming atomicity.

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

**Delivery status:** V2-021..026 local implementation/evidence recorded;
V2-027's candidate gate is approved by the operator. The [MCP contract](bif-mcp.md),
[evidence report](bif-v2-phase-d-evidence.md), and
[release manifest](bif-v2-release-evidence.json) distinguish automated
protocol tests and generated-store timings from the separately recorded
[real configured-host evidence](baselines/v2-phase-d/configured-host/host-evidence.json).

#### V2-021 — Introduce a reusable local application session

**Depends on:** V2-008, V2-014, V2-017.

**Status:** Implemented; sticky identity invalidation and restart/continuation
are covered by session and process tests. No mutable item cache is introduced.

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

**Status:** Implemented with pinned MCP `2025-11-25`, direct serde adapter and
bounded queues. Process tests cover framing, lifecycle, cancellation,
disconnect and backpressure; these are not configured-host approval.

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

**Status:** Implemented; four read tools, explicit projects, strict arguments,
CLI/core parity tests. Recorded input schemas total 2,057 compact UTF-8 bytes;
complete discovery frame is 3,103 bytes.

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

**Status:** Implemented and tested. Both transports expose explicit conditional
reads; retained work validators hit across ordinary process restart.
Generation binding remains deferred to Phase E.

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

**Status:** Implemented and tested; selection returns complete work in one
read, without assignment/claim or changing later optimistic concurrency.

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

**Status:** Local evidence recorded at 100/10k/100k seed 2003: 1,000 mixed
warm calls, 20 separate CLI captures and 20 startups per fixture. RSS/WAL are
sampled observations, not peak bounds or sustained-contention guarantees.
Cursor and validator restart checks passed. Source/binary hashes and raw
samples are preserved; configured-host evidence is recorded separately.

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

**Status:** Complete for candidate validation and compatibility review. CI registration,
generated-store timings, runbook and historical schema-2 executable rehearsal
exist. The required real configured MCP host list/get/history/restart workflows
are [recorded](baselines/v2-phase-d/configured-host/host-evidence.json): Codex
desktop executed 26 tool calls with 34 checks passed on candidate `b97cd89`,
without BIF runtime/protocol changes. The
[operator assessment](baselines/v2-phase-d/configured-host/operator-review.md)
preserves the earlier pending decision. The subsequent
[operator signoff](baselines/v2-phase-d/configured-host/operator-signoff.json)
records Davis's explicit approval on 2026-10-07 for the exact candidate and
reviewed artifact hashes. Actual production-binary inventory/rehearsal and separate live-store
authorization remain required before rollout.
There is no model-token evidence and no token-saving claim.

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

**Normative contract:** [Synchronization/recovery fixtures](fixtures/bif-v2-sync-contract.json)
and their [strict schema](fixtures/bif-v2-sync-contract.schema.json) record the
Phase E contract, including exact wire shapes and store-global retention coverage.
F-P01/F-P02 must amend and revalidate them against the revised sections 2.6–2.9
before Phase F runtime implementation; the existing artifacts alone do not
establish agreement with this plan. These are pure contract models, not a
synchronization runtime release.
Phase E runtime checks cover existing page/history/validator/warm-session
generation invalidation and storage read interleavings only. Actual sync
expiry, bootstrap, and reconciliation validation belongs to V2-036, V2-039,
and later Phase F integration; V2-035 must not depend on V2-037.

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
across ordinary process restart. After generation rotation, page/history
cursors invalidate and a warm session requests restart. A well-formed old
validator returns `modified` as a cache miss even at the same item revision;
a generation-less validator remains an actionable `invalid_input` error with
`generation_missing` and `refetch_required: true`, never `not_modified`.

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
existing page/history cursors, validators, and warm sessions invalidate.
Actual sync-cursor rejection and a new bootstrap are validated in Phase F.
No down-migration or automatic deletion of the later store is performed.

#### V2-034 — Implement explicit journal retention

**Depends on:** V2-029, V2-032.

**Scope:** Implement transactional prefix pruning and a durable retention
floor as an explicit maintenance operation, not an automatic v2 default.
Preserve audit events, sequence high water, and generation. Document that
in-flight clients can expire and must restart.

**Outcome:** Journal size can be controlled without silent gaps or sequence
reuse; callers can reliably detect unavailable coverage.

**Verification:** Prune none/some/all retained rows, restart, append, and compare
positions. Interleave pruning with storage reads to prove same-transaction
floor/coverage/row visibility, never a successful partial gap. Actual sync
expiry and bootstrap restart validation are deferred to Phase F.

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

**Entry gate:** F-P01–F-P03 are complete. Use their contract and fixtures as the
source of truth. Each task adds focused behavioral tests before implementation;
V2-043 supplements rather than replaces those tests. Use disposable stores only.

#### V2-036 — Implement scoped sync cursors

**Depends on:** V2-028, V2-029, F-P03.

**Scope:** Add a distinct sync cursor carrying store/generation, project,
summary schema version, consumed position, and fixed high water while a poll
is being continued. Reuse bounded strict parsing and authorization boundaries.
Validate generation and `F <= C <= H <= durable high water` in the read
transaction. Completed checkpoints start a new poll; continuations preserve H.

**Outcome:** Sync cursors cannot be mistaken for page cursors, moved between
stores/projects, or used after incompatible recovery/schema changes. Consumers
can treat all tokens as opaque and compare checkpoints only for equality.

**Verification:** Wrong-kind/version/scope, malformed position, future position,
generation missing/mismatch, `C == F`, `C < F`, and ordinary restart tests pass.
Accepted numeric positions fit storage's supported integer range.

#### V2-037 — Select bounded journal windows

**Depends on:** V2-035, V2-036.

**Scope:** Implement indexed project selection in `(C, H]`. Consume at most
`min(internal_journal_limit, request.limit)` matching rows, capped at 100, plus
at most one unconsumed matching sentinel. Coalesce only the consumed prefix.
Advance through its last position if a matching row remains, otherwise H,
including empty/other-project-only windows. Establish one storage snapshot seam
for metadata, availability checks, selection, and V2-038 hydration.

**Outcome:** Page selection has bounded work and exact coverage. New writes do
not extend a poll's H. No-change reads avoid scanning unrelated project history.

**Verification:** Sparse projects, abstract wire-model sequence gaps, hot items,
limit one, sentinel boundaries, empty results, and concurrent writes preserve
coverage. Record query plans and no-change work. Preserve current storage's
contiguous retained-range validation; gapped fixtures test wire semantics only.

#### V2-038 — Hydrate complete sync replacements

**Depends on:** V2-010, V2-013, V2-037.

**Scope:** Batch-load current summaries for the selected IDs in the same short
read transaction as journal selection; close it before response serialization.
Include explicit null clears and revision-only changes. Enforce existing core
and transport byte budgets. On overflow shorten the journal prefix and recompute
coalescing, coverage, completion, and cursor; never trim only the output array.
If one replacement cannot fit, return the agreed actionable error without success
or checkpoint advancement. Treat a missing current row as corruption.

**Outcome:** Each returned summary is valid current state at the page's read
snapshot and may be newer than H. No required coverage is acknowledged without
its same-or-newer replacement. An immutable unfit summary is an explicit sync
blocker, not something retries or rebuilding are claimed to repair.

**Verification:** Two updates to one item, explicit clears, a newer version
between chunks, exact byte boundaries, hot-item prefix shrinking, an unfit single
record, and missing rows. Synthetic future tombstones stay at the consumer/codec
boundary; rejection remains an upsert.

#### V2-039 — Add immutable-order bootstrap scanning

**Depends on:** V2-015, V2-028, V2-036.

**Scope:** Scan all project summaries in canonical requester/project/numeric
sequence order, not textual item-ID or queue order. Capture start S before first
rows in the same snapshot. Return the same opaque starting checkpoint on every
page, including empty scans; bind continuation to identity boundary, scope,
generation, and S. Validate S against retention on every scan page. Use bounded
responses and the F-P01 errors. Add a measured scan index via an additive
migration; preserve generation and update upgrade tests/runbook accordingly.

**Outcome:** Consumers can build a fresh baseline and replay changes that
happened during traversal. The scan is live, not a historical snapshot.

**Verification:** Insert before/after boundary, update already-read items,
numeric sequence 999/1000, empty projects, restart, generation change, retention
expiry, and oversized records. Measure shallow/deep access before/after ANALYZE;
verify migration preserves existing history and identity.

#### V2-040 — Implement the standalone durable mirror

**Depends on:** V2-028, V2-038, V2-039.

**Scope:** Add the reusable transport-neutral consumer and separate client-owned
SQLite store at an explicit caller-supplied path. Define a typed scan/poll source
port; test it without depending on future CLI/MCP adapters. Persist baseline/staging
identity, completeness, summaries, and opaque checkpoint. Implement one cross-process
writer lock spanning polls and reconciliation, with one active scan/poll request,
short database transactions, and no write transaction across transport calls.
Guard acceptance by persisted identity, source checkpoint, and local request attempt.

Apply higher revisions as full replacements; ignore lower revisions while
advancing accepted coverage; allow equivalent equal revisions and abort the
whole transaction on conflicting equal revisions. Commit state and checkpoint
atomically. Keep synthetic tombstone revision watermarks to prevent resurrection.

**Outcome:** Replay and crashes cannot acknowledge unapplied changes or regress
accepted state. A retained cursor without a complete baseline requires bootstrap.

**Verification:** Inject crashes before/after client commit, duplicate chunks,
revision reordering/conflicts, synthetic tombstones, delayed responses, competing
processes, and baseline loss with surviving cursor. Verify rollback includes
checkpoint and every replacement, not just the conflicting row.

#### V2-040A — Implement mirror health and recovery transitions

**Depends on:** V2-040.

**Scope:** Implement F-P01's typed status independently of baseline contents.
Persist successful finite catch-up and blocked/recovery state atomically with
the transitions they describe. Guard both success and error callbacks by request
attempt and baseline/staging identity. Transient failures retain state; expiration
requests rebuild; known generation mismatch marks old-history data incompatible;
detected corruption, revision conflict, or unfit record blocks advancement.

**Outcome:** Status distinguishes complete empty, incomplete, stale, rebuilding,
incompatible, and blocked mirrors without claiming current-now freshness.
Duplicate responses do not manufacture new successful observations.

**Verification:** Duplicate no-change response, late success after newer failure,
late error after replacement publication, restart, and empty versus missing
baseline. Verify bounded retry/explicit operator recovery for persistent blockers.

#### V2-041 — Coordinate reconciliation and queue membership

**Depends on:** V2-040A.

**Scope:** Hold mirror writer ownership across staged scan, replay, and
publication. Persist a fresh staging identity before accepting pages; keep the
old usable baseline readable with its status but pause its polling. Never serve
detected-corrupt data. Replay from S toward a finite H; after each atomic apply
validate current C, not original S, against retention.
Guard pages and publication against abandoned staging identities. Atomically
replace active identity, contents/reference, completeness, checkpoint, and status.
Clean abandoned staging safely without deleting the published baseline.

Derive ready/mine/active membership using existing application semantics,
including done/rejected exclusion from mine. Do not claim canonical list/next
ordering from summaries that lack capture timestamps.

**Outcome:** A successful rebuild replaces, never unions, baseline contents.
Readers see an entire old or new baseline. Convergence holds after quiescence
and successful catch-up, not under arbitrary write load or resource constraints.

**Verification:** Compare mirror state with a complete database read after
quiescence. Test two competing rebuilds, polling versus publication, abandoned
scan responses, crashes around publication, empty replacement, expiry during
scan/replay, `S < F <= C`, generation reset, and detected corruption recovery.
Test assigned in-progress → done, rejected, and blocked membership.

#### V2-042 — Expose scan and sync through CLI and MCP

**Depends on:** V2-023, V2-038, V2-039, V2-041.

**Scope:** Add explicit v2 scan/sync interfaces over the same application core,
including initial scan's null cursor and normal authorization on every request.
Connect both to the standalone consumer; neither adapter owns a second mirror
implementation. Enforce availability/identity errors from F-P01 and existing
transport budgets. Expose selected baseline data with the same typed completeness,
health, and last-observed catch-up information in CLI, MCP, and agent handoff.

**Outcome:** Both transports can bootstrap and resume without server-held
consumer sessions. A fresh agent receives usable selected state or an explicit
need to fetch/rebuild—not an unexplained delta or a falsely current empty queue.

**Verification:** Process/host tests resume after server restart, reject stale
cursors, handle no-change polls and outages, and propagate identical status.
Test two independent mirrors and restore → disabled → explicit reactivation →
independent rebuilds. Semantic results agree; byte budgets may change page sizes.

#### V2-043 — Stress synchronization correctness

**Depends on:** V2-042.

**Scope:** Add model/state-machine and multi-process tests interleaving captures,
mutations, polling, coalescing, bootstrap, crashes, supported pruning, and restore.
Use an independent complete-read oracle. Record seed and replayable operation
trace. Exercise maintenance through its supported stopped-process/offline path;
do not imply support for online prune/restore. Include competing writers and
delayed success/error callbacks across publication and recovery.

**Outcome:** Sync correctness is established beyond isolated happy-path unit
tests; every failure has a reproducible trace and classified recovery action.

**Verification:** After quiescence, the reference mirror equals current scoped
summaries after successful catch-up. No required journal coverage is silently
skipped; covered changes have the same or newer complete item revision.
Failures never advance checkpoints past applied state or publish incomplete
baselines. Verify authorization boundaries without inventing new ACL guarantees.

#### V2-044 — Gate the sync release

**Depends on:** V2-027, V2-035, V2-043.

**Scope:** Benchmark no-change and 1%/5%/25%-changed loops, hot-item updates,
bootstrap/recovery, output size, journal growth, and source/mirror writes.
Separate distinct items changed from journal operations. Measure scan duration,
retention needs, staging disk use, retry behavior, and unfit-record failures.
Run a configured host workflow with an explicit mirror path, owner, handoff,
and stale/incompatible-state behavior. Report raw evidence and methodology.

**Outcome:** Approve or reject the sync release with recovery documentation,
measured benefits, supported project-summary scope, and operational limits.
State retention/maintenance policy explicitly; bounded history is not automatic
pruning or guaranteed file shrinkage. Live activation needs separate approval.

**Verification:** Correctness stress tests pass; workflow accounting includes
bootstrap/recovery and client persistence costs. Demonstrate blocked-state
reporting rather than endless rebuilds for unfit records. Do not claim measured
model-token savings from bytes, perpetual freshness, intermediate-revision
delivery, guaranteed bootstrap under arbitrary load, or exclusive work ownership.

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
