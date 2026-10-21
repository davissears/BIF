# BIF upgrade and rollback runbook

**Status:** V2-054 operator checklist, Phase D indexed-read candidate procedure,
V2-032 archived schema-3 to schema-4 proof, and the separate V2-035
[final foundation gate](baselines/v2-phase-e/final-gate/REPORT.md). The required Phase D
[Codex configured-host evidence](baselines/v2-phase-d/configured-host/host-evidence.json)
is recorded; the
[operator signoff](baselines/v2-phase-d/configured-host/operator-signoff.json)
approves candidate validation and compatibility review. Live-store maintenance
requires separate authorization. The signoff retains the exact reviewed runbook
as a separate snapshot; it does not approve the Phase E journal candidate.
See the
[release manifest](bif-v2-release-evidence.json) and
[Phase D evidence](bif-v2-phase-d-evidence.md). The separate
[Phase E proof](baselines/v2-phase-e/README.md) records only disposable executable
and process evidence. V2-033 adds offline current-schema backup/restore and
restore-generation reset; V2-034 adds explicit global journal pruning below.
F-P03 adds explicit offline activation and a reusable snapshot guard below.
Sync adapters, reconciliation, journal rollout and live operational release gates
remain unfinished.

## Compatibility and maintenance window

V1 CLI and RPC response compatibility does not imply old-binary/new-schema
compatibility. BIF checks migration versions and checksums; an old binary
rejects a newer schema. Production connection opening automatically applies
embedded migrations, including during ordinary reads and `doctor`. Do not
point a candidate binary at the real store just to inspect it.

The `bifc1.` prefix is unchanged, but tokens now require persisted generation,
and generation-less legacy tokens cannot continue pagination. The encoded size ceiling is now
1 MiB. Earlier binaries with a 16 KiB ceiling cannot resume larger continuations
after rollback and may fail to generate continuations for supported long IDs.
Restarting pagination does not remove that old-binary limitation; use a binary
with the compatible cursor budget to traverse those records.

Inventory the exact CLI executable paths, host launch commands, scheduled jobs,
configuration files, root overrides, and all processes opening the store.
Record binary hashes, source revisions, the database path, store ID, migration
versions/checksums, and a maintenance owner. Retain the old binary and config.
The configured database is `ROOT/.bif/bif.sqlite`, not the repository itself.

Before candidate opening, establish a maintenance window covering every process
and launcher in that inventory:

1. Disable automatic restarts and new launches (services, scheduled jobs, host
   launchers and direct CLI/RPC entry points). Keep restart prevention in force
   throughout drain, backup, migration and verification.
2. Stop accepting writes, allow authorized in-flight one-shot CLI/RPC writes to
   finish or explicitly terminate them, and **wait/reap or obtain the service
   manager's equivalent exit confirmation**. Check that no old writer remains.
   An intended stop command or a fixed sleep is not drain evidence.
3. Stop/reap readers too, including persistent `bif-mcp` servers; their
   configuration and schema resources are pinned for the process lifetime.
   Only after confirmed drain may maintenance or candidate migration proceed.
4. Verify the candidate, replace all inventoried launch commands with the chosen
   executable, confirm no old process remains, and only then remove maintenance
   restart prevention and restart the approved services.

The old-binary newer-schema check is a **fresh-open check, not retroactive
fencing** of a connection already open at migration time. The authentic schema-3
historical source has one-shot CLI/RPC writers, not a warm writer mode; its warm
MCP process is read-only. Do not claim a hypothetical old warm writer will be
fenced. Current supported capture/mutation repositories check pinned
generation/schema inside `BEGIN IMMEDIATE`, before replay/no-op/effective writes.
Those guards do not retrofit old binaries, enforce the operator window, or
protect direct SQL. Direct SQL and out-of-band writers are unsupported.

## Current-schema offline backup and restore (V2-033)

The CLI and `storage::maintenance::{backup, restore}` API both require the
operator window above: **stop/reap every reader and writer, and disable automatic
restarts and new launches** through verification. `--offline` is a required
explicit acknowledgement, **not proof that anything has shut down**. A SQLite
lock, a generation guard, or a successful command cannot prove shutdown either.
Keep all source/output directories trusted, private as appropriate, and unchanged
by other actors. Do not use this procedure on a live store without separate
authorization.

Set `DB` to the exact existing database (normally `ROOT/.bif/bif.sqlite`), `BACKUP`
to a **new** backup file in an existing trusted directory, and `LATER` to a
different **new** preservation file. Maintenance ignores configuration/environment
store selection and accepts no `--root` override.

```sh
# After confirmed stop/reap and restart prevention:
bif maintenance backup --store "$DB" --output "$BACKUP" --offline

# A separately authorized recovery decision; keep BACKUP pristine:
bif maintenance restore --store "$DB" --from "$BACKUP" \
    --preserve-later "$LATER" --offline
```

Both commands require the exact current embedded migration/name/checksum history
and SQLite schema; old, future, missing/gapped or malformed histories are refused
without implicit migration/down-migration. Full integrity and foreign-key checks,
store identity, journal metadata, retained `(F,H]` coverage/operation links and the
durable allocator are validated. Arbitrary extra schema objects are unsupported.
Historical schema-2/schema-3 backups cannot be restored using this command.

Backups use the [SQLite Online Backup API](https://www.sqlite.org/backup.html),
including committed WAL frames, and are verified standalone DELETE-journal SQLite
files. Never copy a mainfile/WAL pair yourself or delete WAL/SHM files. An existing
output/preservation destination or sidecar, a path alias (including hard links),
or a collision with an input's SQLite sidecar is rejected. A failed operation
may retain an incomplete claimed output; never treat it as verified, overwrite it,
or reuse that destination on retry.

Restore first copies the input into a private stage, validates it, then draws a
**fresh independent OS-random 128-bit generation on every restore**, including
repeated restores of the very same backup. Independent random collision
probability is assumed negligible, not impossible. In a staging transaction it
changes only generation, `sync_enabled=0`, and `bootstrap_required=1`. Store ID,
schema/migration history, item revisions, audit, receipts, journal rows, H and F
are preserved from the input. Before target publication it creates and verifies
`LATER` as a SQLite-consistent backup of the current target. This preservation is
never overwritten/deleted by the tool, even if publication fails.

Publication uses Online Backup into the existing target inode, not rename or
filesystem replacement. SQLite owns the destination write transaction: a backup
abandoned before `SQLITE_DONE` rolls back; completion commits the whole stage.
No user transaction may already be open on that destination handle. The target
is closed/reopened and validated before success is reported. SQLite advances its
internal `PRAGMA schema_version` cookie during backup; preserving the schema means
unchanged DDL and migrations, **not** preserving that physical cookie. The tool
does not write the cookie back. Incompatible WAL page sizes and busy/locked
transfers fail closed without retry loops.

On any error or interrupted process, keep restart prevention active, retain both
backups, reopen/inspect the target and verify its integrity/metadata/full reads
before deciding whether to retry to a new preservation destination. An error
after publication can mean a complete restore committed but final verification
failed; an error is not an assertion that no commit happened. After verified
restore, restart **all** clients. Old page/history continuations fail as stale;
old item validators must refetch, never falsely return `not_modified`.
Later writes are preserved, **not reconciled or reapplied**. Actual sync expiry,
bootstrap and reconciliation remain V2-036/V2-039 work; this command activates
neither synchronization nor a sync cycle.

Focused disposable tests cover WAL-only committed frames, pre-publication
failures, incomplete Online Backup rollback and killed/reaped test-only backup
processes at before/incomplete/completed boundaries. CLI commands are separately
tested in subprocesses; the crash boundary child tests the actual SQLite primitive,
not an instrumented production CLI interior. This is logical process recovery
evidence, **not physical power-loss/torn-write** or live/platform release approval.
`FULL` SQLite synchronization and output-file sync are not a substitute for the
operator's durable filesystem/directory sync, off-machine retention, encryption
and access-control policy.

## Normative sync activation and mirror recovery contract (F-P01)

F-P01 freezes the
[fixture](fixtures/bif-v2-sync-contract.json) and its
[strict schema](fixtures/bif-v2-sync-contract.schema.json), following the
[preparation plan](bif-v2-migration-plan.md#before-phase-f--required-preparation).
F-P02 supplies behavioral-model checks. F-P03 implements the explicit offline
activation CLI/API and reusable storage read guard. Actual scan/poll adapters,
cursor decoding/enforcement and the mirror still follow in Phase F
(V2-036/V2-042); the wire/mirror behavior below remains their normative contract,
not implemented endpoint evidence. This procedure is for separately authorized
maintenance and disposable rehearsal, not release or live-store authorization.

### Source availability is not client completeness

Initial journal/schema creation establishes an independently random generation,
`F=H=0`, `sync_enabled=0`, and legacy `bootstrap_required=1`. It does not backfill
timestamp-ordered audit history, rewrite receipts, or activate synchronization.
Effective supported capture/mutation writes already append one journal entry in
their shared item/audit/receipt transaction; receipt replay and genuine no-ops
append nothing. Journal participation must continue while service is disabled.

Activation/reactivation is a separate, explicitly authorized **offline**
maintenance operation:

1. Establish the existing maintenance window: inventory every source process and
   launcher, stop/reap all readers and writers (including warm MCP sessions), and
   exclude automatic restarts and new launches through verification.
   An offline acknowledgement or a database lock cannot prove this operator duty.
2. Verify the exact supported schema/migration history, integrity, store identity,
   retained coverage and allocator. Inventory the approved binaries/launch paths
   and establish that every supported effective writer journals in the same
   transaction as the item/audit/receipt changes. A successful schema open alone
   is not proof of deployment participation; direct SQL writers remain unsupported.
3. Supply the deliberately inspected expected generation. In one immediate write
   transaction, validate persisted prerequisites and expected generation, then
   set **only** `sync_enabled=1`. Do not reset generation, `F`, `H`,
   `bootstrap_required`, journal rows, items, audit, receipts or client state.
   Failed validation or failure before commit changes none of them.
4. Verify persisted state before lifting restart prevention. Retrying the same
   expected generation rechecks all prerequisites; already enabled is a successful
   no-op, not a new generation or a bootstrap reset. An interrupted or unacknowledged
   commit may have enabled service: keep the window, reopen and inspect, then
   safely retry. Do not infer rollback from missing acknowledgement.

Use the exact explicit path, ignoring configuration/environment store selection.
The CLI accepts only `--store`, `--expected-generation`, and the required
`--offline` acknowledgement. There are no process-proof flags or markers.
The API is `storage::maintenance::activate(store, expected_generation)` and has
the same operator prerequisites; it cannot establish shutdown or binary inventory.

```sh
# After separately authorized stop/reap, restart exclusion and writer inventory:
# Inspect only; no supported inspect endpoint or direct SQL writes are implied.
sqlite3 -readonly "$DB" \
  'SELECT generation, retention_floor AS F, high_water AS H, sync_enabled, bootstrap_required FROM journal_metadata WHERE singleton = 1;'

# Set GEN deliberately to the inspected generation, not an old backup/token:
bif maintenance activate --store "$DB" --expected-generation "$GEN" --offline

# Reinspect the same path and verify only sync_enabled changed before reopening:
sqlite3 -readonly "$DB" \
  'SELECT generation, retention_floor AS F, high_water AS H, sync_enabled, bootstrap_required FROM journal_metadata WHERE singleton = 1;'
```

Activation opens only an existing regular store using the established trusted
path/sidecar checks, without migration or journal-mode changes. `BEGIN IMMEDIATE`
reserves the writer **before** checking the exact embedded schema and complete
migration/name/checksum history, database integrity and foreign keys, store and
generation identity, retained journal coverage/operation-item-project links and
allocator. It checks the expected generation, sets only `sync_enabled=1` if
disabled, revalidates, and commits. An already-enabled retry still validates
everything and rejects stale generation. Missing/invalid prerequisites are
refused without repair. Busy/locked fails closed without retry loops.

These are **full history/integrity validations**, not constant-cost checks or
bounded sync request work. Validation runs twice under the writer reservation,
including a matching-generation retry; budget the maintenance window for store
size, retained history and temporary reference-schema construction. Locks
serialize SQLite writers but neither prove shutdown nor exclude future process
launches. Operator inventory must verify the actual deployed supported binaries
and launch paths, their transactional journal participation, and exclusion of
unsupported/direct-SQL writers. Do not lift restart prevention on command success
alone without persisted-state verification.

Ordinary open, migration, restart, and client bootstrap completion never activate
service. Restart preserves both generation and availability. Supported restore
independently rotates generation and disables service, even when the same backup
is restored again; its `F`/`H` come from the backup, not the later target.
After restore, inspect the **new** generation and explicitly reactivate under the
same prerequisites. Reactivation preserves that restored history and does not
make any old client compatible or complete.

The recovery order is **restore → disabled → inspect new generation → explicitly
activate → verify persisted state**, under the same uninterrupted maintenance
window. Never reuse the pre-restore generation: it is rejected even if the backup
was enabled. Restore sets `bootstrap_required=1`; activation preserves it along
with restored F/H, journal rows, allocator and all other persisted values.

There is **no server client registry**. `bootstrap_required` remains only for
storage compatibility: no new runtime client-progress decision consults it, and
no client's successful catch-up clears it. Two clients can independently have a
complete baseline and no baseline while the same source is enabled. Each client
owns its own checkpoint and recovery; activation cannot mark either ready.

Disposable runtime tests compare complete table inventories, cover strict CLI
rejection/retry and restored reactivation, and inject failures after flag update
in the private production core using TEMP triggers (without relaxing main-schema
validation). A test-only child calls the actual activation API, signals after
commit but before a simulated operation response, and is forcibly killed/reaped;
fresh inspection and retry retain the committed flag. CLI parsing/execution is
tested separately, not instrumented inside that crash boundary. This is
**process-crash**, not physical power-loss/torn-write or release evidence.

### Fail-closed requests and actionable errors

Every scan/poll request uses normal authorization first. After authorization,
incompatible supplied source identity wins over availability:
`invalid_cursor` with `details={reason:wrong_store|wrong_generation,
restart_required:true}`. Next, disabled service returns `sync_disabled` with
`details={reason:service_disabled,recovery_action:activate_source}`.
Initial scan with no cursor is **also refused**; disabled never means an empty
project, successful no-change response, or checkpoint advancement. Read identity,
availability, retention and coverage in the same read snapshot. Other typed cursor
errors remain as frozen in the fixture. Tokens do not confer authorization.

The implemented storage seam is `JournalSnapshot::require_sync`: the caller
authorizes first and supplies decoded store/generation identity, or `None` for an
initial scan. It returns typed `StoreMismatch`, `GenerationMismatch`, then
`SyncDisabled` errors in that precedence; compatible enabled requests succeed.
Store identity and availability come from the same caller-owned read snapshot
as journal metadata/rows, including when an outer read transaction already exists.
WAL interleaving tests exercise old/fresh snapshots, not permission to leave
processes running during maintenance. This guard does not decode cursors, enforce
adapter/wire policy, or turn raw journal inspection into a sync endpoint.

A complete summary that cannot fit even an otherwise empty supported response
returns the existing `payload_too_large` envelope, not a new truncation format:
`details={record_kind:item,record_id,maximum_response_bytes,minimum_required_bytes}`.
`record_id` identifies the item, `maximum_response_bytes` is the effective
supported budget (at most 1 MiB), and `minimum_required_bytes` counts the complete
UTF-8 response envelope, including continuation metadata. Do not advance scan or
replay past the item, truncate fields, or publish partial success. Retry or rebuild
with unchanged item/budget cannot repair a permanently unfit record: surface
`resolve_record_or_budget`, retain coverage, and require a supported content/budget
resolution rather than a retry loop. This contract does not authorize SQL edits
or increasing the supported budget.

### Caller-owned mirror, status and recovery

Require an **explicit caller-owned separate SQLite mirror path**, with no
machine-local default and no reuse of the source database/sidecars. The fixture's
`/mirrors/...` paths are examples, not deployment choices. Local example client IDs
are not registrations on the server.

One cross-process writer owner per mirror spans polling, reconciliation and
publication. Within it, serialize scan/poll requests. Use short database
transactions and **no transport under a write transaction**. Persist staging
identity before sending a request. On both success **and failure**, compare
request-attempt ownership, baseline/staging identity and captured source checkpoint
with persisted state before changing data, checkpoint or health/status.
An abandoned scan, old poll error or duplicate no-change callback must not alter a
replacement baseline or falsely refresh its health, even if checkpoints match.
Reacquiring ownership after interruption must invalidate the old request attempt.

Keep replacements in staging. Only after scan exhaustion and complete catch-up
to a chosen finite `H`, atomically publish baseline identity, contents/reference,
completeness and checkpoint together. Reconciliation replaces contents rather than
unioning them. While rebuilding, a previous **usable complete** baseline remains
readable but receives no updates; staging is never served as a complete queue.
Incompatible data is explicitly labeled old history, not a usable current-source
baseline. Detected-corrupt data is never served.

The fixture's typed status has:

- `published_baseline`: null when absent, otherwise identity, explicit
  completeness, checkpoint, item count and usability. Complete with zero items
  is not absent; a leftover cursor alone cannot restore a lost baseline.
- `rebuild_phase`: `idle`, `scanning`, `catching_up`, or `blocked`, plus a nullable
  persisted `staging_identity`. This describes rebuilding, not server enablement.
- `last_successful_catch_up`: nullable checkpoint, finite high water and observation
  identifier. It changes only on a successful complete finite catch-up, not on a
  partial page or stale callback. High water is explicit response metadata;
  clients never decode opaque cursors.
- `last_failure`: nullable typed cause and recovery action. A later successful
  finite catch-up clears the active failure; failures retain the prior success
  observation. Local failure categories are not additional server wire error codes.
- `source_compatibility`: `unobserved`, `compatible`, or `incompatible`, always
  **last observed, not current-now**. Transport failure cannot refresh it.

| Observation | Normative recovery |
| --- | --- |
| Transient request failure | Retain usable baseline/checkpoint and last successful catch-up; retry with a fresh attempt. |
| Expired coverage | Discard expired staging, rebuild independently; a usable old complete baseline stays readable without updates. |
| Generation/source incompatibility | Invalidate old continuation/staging; label retained baseline incompatible history and rebuild. Reactivation does not repair it. |
| Wrong source store (`wrong_store`) | Preserve baseline/checkpoint and last successful catch-up as incompatible history, discard staging and block updates. Verify the intended source before rebuilding; never resume that checkpoint against a different store. |
| Missing baseline | Rebuild even if a cursor survives or an empty delta is returned; never infer a complete empty queue. |
| Detected local corruption | Block serving that baseline; repair/quarantine local storage then rebuild. A corrupt retained checkpoint is not evidence of coverage. |
| Permanently unfit summary | Block advancement; expose item/bytes/budget and resolve the record or supported budget. Retry/rebuild alone is not recovery. |
| Disabled compatible source | Preserve local state; require separately authorized source activation, then retry. Identity mismatch still takes precedence. |

A stale complete baseline supports browsing and provisional preparation only.
Eligibility, ownership, authorization and mutation validation remain authoritative
at the source. This contract adds no offline write queue, claim guarantee, ACL
revocation mechanism or detection of arbitrary semantic corruption.

### Scan start versus replay coverage

`S` is the original bootstrap start, `C` the **durably applied** replay coverage,
and `F` the global floor. During scan, validate `S` on every page. After scan,
replay begins at `C=S` and validates the advancing `C`, not the original `S`.

| Phase | Boundary | Outcome |
| --- | --- | --- |
| Scan | `S < F` | Discard staging and rebuild, regardless of any unrelated checkpoint. |
| Scan | `S == F` | Valid scan continuation. |
| Replay | `S < F < C` | Continue; original start being pruned does not invalidate durable replay progress. |
| Replay | `S < F == C` | Continue; the floor is valid coverage. |
| Replay | `C < F` | Discard staging/rebuild; never advance past missing coverage. |

Generation/scope validation and finite high-water bounds still apply. These
positions are server/test semantics, never instructions to parse cursor tokens.
The amended fixture/schema tests check declared examples and closed shapes;
F-P02 must test the state-machine behavior, and later runtime/storage/adapter
tests must establish implementation correctness.

## Explicit offline journal retention (V2-034)

Use `storage::maintenance::prune(store, expected_generation, through)` or the
command below only after establishing the same stop/reap and restart-prevention
window. `--offline` acknowledges the prerequisite; it does not prove shutdown.
Rehearse on disposable stores. This is not live-store authorization.

Set `DB` to the exact existing database, then inspect the binding and coverage
using a read-only SQLite connection. This is diagnostic metadata SQL, not a new
supported inspect endpoint, and **does not authorize direct SQL writes**:

```sh
sqlite3 -readonly "$DB" \
  'SELECT generation, retention_floor AS F, high_water AS H FROM journal_metadata WHERE singleton = 1;'

# Set GEN to the returned generation and deliberately choose N with F <= N <= H:
bif maintenance prune --store "$DB" --expected-generation "$GEN" \
    --through "$N" --offline
```

There is no automatic retention default or project-specific pruning. `N` is an
inclusive signed SQLite integer (0 through `i64::MAX`). The command opens an
existing file without migration or journal-mode changes, acquires `BEGIN IMMEDIATE`,
validates current schema/history, integrity, identity, retained coverage and
allocator inside that transaction, and compares generation and bounds. It deletes
**all projects'** `change_seq <= N` rows and updates **only** `retention_floor`.
It revalidates before committing; any returned transaction error rolls back
both deletion and floor update, including a floor-update failure after deletion.
Busy/locked fails closed without retries. After process interruption, reopen
and inspect the persisted coverage before deciding what remains to do.

F and H are monotonic within a generation. Prune leaves H, AUTOINCREMENT state,
generation, store identity, audit, receipts, and inactive/bootstrap flags unchanged.
Pruning none (`N == F`), some, or all (`N == H`) is valid. Reopen and future append
must allocate above prior committed H, including after prune-all. Old audit and
receipt records intentionally need no new journal links below F; receipt replay
and genuine no-ops must not refill pruned rows.

For journal storage reads, `C == F` is valid and `C < F` is expired, independent
of project filters (even an empty project). In file-backed WAL tests an already
established snapshot sees its complete old metadata/rows; prune-first and fresh
readers see the new floor. This isolation evidence is **not** permission to keep
clients running during maintenance. No new schema, activation, sync endpoints,
actual sync expiry/bootstrap, `VACUUM`, or database-file shrinking is provided.
Full persisted rollback is checked on fresh reopen; floor-update failure injection
uses a TEMP trigger in the same private opened-connection core as the public API,
so exact main-schema validation is not relaxed. These disposable tests do not
prove machine-power-loss or filesystem durability.

## Historical-schema SQLite-consistent backup: executable procedure

Use SQLite's online backup API, not a filesystem copy of a live main database.
Committed data may still be in the WAL. Never discard WAL/SHM files to make a
backup appear readable. The source and destination directories must be trusted,
and no other process may replace their entries during this procedure.

The following retained procedure supports historical-schema upgrade rehearsals
that the current-schema-only BIF maintenance API refuses. It does not implement
restore-generation reset. The example requires Python 3 with `sqlite3`. Set
`SOURCE` to the inventoried database. For rehearsals, use only a generated
disposable fixture. Choose a
private destination on durable storage for a real backup; `target/` below is
only a rehearsal destination.

```sh
(
# Keep failure exits and setup variables out of the calling interactive shell.
fail() { printf '%s\n' "$*" >&2; exit 1; }
[ -n "${SOURCE:-}" ] && [ -f "$SOURCE" ] ||
    fail "SOURCE must name an existing database file"
BACKUP_PARENT="$PWD/target/bif-upgrade-rehearsal"
mkdir -p "$BACKUP_PARENT" || fail "Cannot create the selected backup destination"
BACKUP_PARENT=$(cd "$BACKUP_PARENT" && pwd -P) ||
    fail "Cannot resolve the selected backup destination"
BACKUP_ROOT=$(mktemp -d "$BACKUP_PARENT/backup-XXXXXX") ||
    fail "Cannot allocate a private backup directory"
[ -n "$BACKUP_ROOT" ] || fail "mktemp returned an empty backup directory"
# Accept only an absolute, immediate child of the selected destination.
case "$BACKUP_ROOT" in
    "$BACKUP_PARENT"/backup-*) ;;
    *) fail "Backup directory is outside the selected destination" ;;
esac
BACKUP_NAME=${BACKUP_ROOT#"$BACKUP_PARENT"/}
case "$BACKUP_NAME" in
    */*) fail "Backup directory is not an immediate child of the destination" ;;
esac
[ -d "$BACKUP_ROOT" ] || fail "Backup directory does not exist"
mkdir "$BACKUP_ROOT/.bif" || fail "Cannot create the private database directory"
python3 - "$SOURCE" "$BACKUP_ROOT/.bif/bif.sqlite" <<'PY'
import json, pathlib, sqlite3, sys

source = pathlib.Path(sys.argv[1]).resolve(strict=True)
destination = pathlib.Path(sys.argv[2]).absolute()
# Refuse an existing destination. The parent is private and must stay trusted.
with destination.open("xb"):
    pass
# mode=rw refuses a missing source. WAL access can require creating sidecars;
# this connection runs no BIF migrations or application mutations.
src = sqlite3.connect(source.as_uri() + "?mode=rw", uri=True)
dst = sqlite3.connect(str(destination))
try:
    src.backup(dst)
    def inspect(connection):
        return {
            "integrity": connection.execute("PRAGMA integrity_check").fetchall(),
            "foreign_keys": connection.execute("PRAGMA foreign_key_check").fetchall(),
            "store_id": connection.execute(
                "SELECT store_id FROM store_metadata").fetchone()[0],
            "migrations": connection.execute(
                "SELECT version, name, checksum FROM schema_migrations ORDER BY version"
            ).fetchall(),
            "counts": {
                table: connection.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
                for table in ("items", "item_acceptance_criteria",
                              "item_provenance", "operations", "events")
            },
        }
    before, copied = inspect(src), inspect(dst)
    # Explicit checks remain active under python -O / PYTHONOPTIMIZE.
    if before != copied:
        raise RuntimeError("backup inventory differs; keep writers stopped")
    if copied["integrity"] != [("ok",)]:
        raise RuntimeError(f"integrity check failed: {copied['integrity']!r}")
    if copied["foreign_keys"]:
        raise RuntimeError(f"foreign-key violations: {copied['foreign_keys']!r}")
    print(json.dumps({"sqlite_version": sqlite3.sqlite_version,
                      "backup_path": str(destination),
                      "verification": copied}, indent=2))
finally:
    dst.close()
    src.close()
PY
)
```

A failed run may leave a partial destination. Never use it as a verified backup
or overwrite it on retry; allocate a new private destination. Preserve the
successful verification output (including `backup_path`) alongside the backup
and config inventory. Setup failures stop before Python opens SQLite or claims
a database destination; verification failures raise even under Python optimization
and do not print successful verification JSON.
This example verifies a logical SQLite copy, not disaster-recovery durability:
apply the operator's durable-backup, filesystem sync, off-machine retention,
encryption, and access-control policy before approving a real upgrade.

## Upgrade checklist

1. Review the release's migration SQL, supported schema versions, regression
   tests, and evidence. Confirm maintenance ownership and downtime.
2. Complete the stop/drain/prevent-restart window above, then take and verify
   the stopped-store backup. Keep it pristine and keep restart prevention active.
3. Create a **separate** disposable candidate root using the same backup API
   with the verified backup as source. Never upgrade the only rollback copy.
4. Point the candidate executable at that candidate root with explicit
   `--root` and `--requester`. `doctor` opens and upgrades its database.
   Recheck integrity, foreign keys, unchanged store ID, expected new migrations,
   and full-item/history reads. Exercise authorized capture and revision-checked
   mutation with fresh keys **only on the candidate**. Check actual CLI and host
   RPC v1 workflows, not only library tests.
5. Do not upgrade the real store until the operator explicitly authorizes that
   exact executable, schema, root, backup, and maintenance window. Only then
   open it with the candidate binary. Verify the same metadata/read checks;
   agree on any real-store write probe in advance.
6. Update every launcher to the chosen binary, restart services, and confirm
   there are no old processes. Resume normal work only after verification.

The Phase D and disposable Phase E procedures below supplement this checklist.
V2-035 verifies the journal foundation procedures separately from live release.
V2-033's current-schema restore above
does not exist in the historical read candidate; never use its commands to
restore a historical-schema backup.

## Phase D indexed-read candidate

This candidate supports schema migrations 1, 2 and 3. Migration 3 adds projection
read indexes only; Phase D adds no further migration and no change journal.
Wire-compatible v1 reads/writes remain available in the candidate, but an old
schema-2 binary is not promised access to the upgraded schema-3 database.

Required pre-release checks are registered in
`docs/bif-v2-release-evidence.json`. Run:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets
cargo test --locked
cargo test --locked --test read_release_rehearsal
cargo test --locked --test read_release_manifest
```

The automated rehearsal uses only new disposable roots: construct populated
schema 2, online-backup a pristine copy, upgrade a separate candidate with the
production factory, verify integrity/foreign keys/store identity/durable state,
write only to the candidate, and restore a separate rollback root from the
pristine backup. It never down-migrates or replaces an active database.
Library verification of schema-2 restored content is **not** evidence that an
archived old binary has reopened it; the manifest records that distinction.
The [recorded executable rehearsal](baselines/v2-phase-d/old-binary-rehearsal.json)
now supplements that test with a historical schema-2 build from `45ed7fc`.
It is not the operator's retained approved production binary; that live-rollout
inventory/rehearsal and authorization remain operator responsibilities.

For an actual candidate, also retain the operator's archived old executable and
verify `get`, `history`, capture/replay and a revision-checked mutation against
disposable upgraded/restored roots with their appropriate binaries. Preserve
the exact executable hashes, source revisions, configuration and inventories.
Do not run these write probes against the real store without separate approval.

Before approving V2-027, configure a real MCP host with the candidate executable
and an explicit disposable-store configuration. Record:

1. Host name/version, supported negotiated MCP protocol (`2025-11-25`), launch
   command and configuration path, candidate executable hash and source.
2. `bif_list` for two explicitly named projects, `bif_get` for each projection,
   conditional hit and changed-item miss, and paginated `bif_history`.
3. A valid page cursor and version validator retained by the host/client,
   followed by server restart and matching continuation/refresh.
4. Clean protocol stdout, server shutdown/disconnect and correct host error
   display. Confirm the host exposes no BIF mutation tool.
5. The evidence artifact path and reviewer approval in the release manifest.

Protocol subprocess tests and the benchmark client are **not** substitutes for
this host workflow. Missing model-token evidence is disclosed but does not
block correctness; missing required configured-host evidence does block release.

## Phase E schema-3 to schema-4 proof (V2-032)

The [separate evidence and harness](baselines/v2-phase-e/README.md) build
unmodified historical source
`d4399a93e49ce8f87e39e3129377821858472807` using `git archive`, its exact locked
dependencies and declared Rust toolchain in disposable scratch with a separate
target directory. Source availability alone is not executable evidence. Missing
toolchain, dependency access, authentication or task-required operator setup is
a blocker, not permission to patch the old source or emulate an old client.

Reproduce only on disposable roots (the proof-integrity test binds the recorded
artifact to the current harness/control files, so rerun the rehearsal after
changing those files):

```sh
python3 docs/baselines/v2-phase-e/rehearse.py --output docs/baselines/v2-phase-e/upgrade-proof.json
python3 docs/baselines/v2-phase-e/test_rehearse.py
cargo test --locked --test phase_e_upgrade_proof --test phase_e_schema_guards --test read_release_manifest
```

The real historical executable initializes schema 3. A SQLite writer lock,
OS `SIGSTOP` acknowledgement and `SIGCONT` control an actual old one-shot capture
process without timing sleeps. It successfully finishes and is waited/reaped
**before** candidate migration. The harness does not claim old SQL-statement
readiness at the stop point. Its disposable serial launcher maintenance sentinel
blocks five restart attempts across drain, migration and candidate verification.
It is not a replacement for the live service/launcher inventory or a fence
against outside launchers. No old process is restarted during that window.

Candidate migration preserves full legacy table values; candidate capture and
mutation append journal positions while `sync_enabled=0`, `bootstrap_required=1`.
After the window, fresh old `doctor`, `get` and `capture` opens reject schema 4;
full persisted table values (including receipts, audits, metadata and allocators)
and `sqlite_schema` plus persistent schema/user-version pragmas are unchanged by
every rejection. This proves logical persisted-state preservation and fresh-open
rejection only, not physical WAL/SHM-byte equality or retroactive old-connection
fencing.

Phase E remains **not approved**. Its separate manifest gates require the live
operator's exact retained binary/service/launcher inventory, a verified live
stop/drain/prevent-restart window, explicit Phase E authorization, configured-host
and platform validation, and durable/power-loss recovery validation. Phase D's
read-candidate approval and host evidence do not satisfy these gates. The V2-032
proof itself does not supply rollout/restore tooling or activate synchronization.

## Phase E final foundation gate (V2-035)

The [final report](baselines/v2-phase-e/final-gate/REPORT.md),
[frozen protocol](baselines/v2-phase-e/final-gate/protocol.json),
[current proof](baselines/v2-phase-e/final-gate/proof.json), and
[raw measurements](baselines/v2-phase-e/final-gate/measurements.json) bind
production candidate `f0cd787f64a6bd3d301f11e616bb587e112a14fa` and the exact
harness/tests. This is evidence-only work: no production source or migration
change. The archived Phase D approvals and V2-032/task007 proofs are not replaced.
V2-054 and V2-055 are document-local prerequisite labels, not ledger IDs.

The final gate reruns the authentic `d4399a9` archive/locked-build, actual old
writer stop/drain/reap and disposable restart exclusion. It separately rehearses:

1. Fresh schema-4 initialization and schema-3 upgrade, including recorded
   migration names/checksums. Fresh old opens refuse schema 4; this is not warm
   old-writer fencing.
2. Historical-schema rollback into another root from a pristine schema-3 online
   backup, reopened/read/replayed with the real historical binary. Candidate
   later writes remain in a separately verified current-schema preservation.
3. Current-schema CLI backup including a journal row; two restores of the
   identical immutable input, each drawing an independent generation. Preserve
   and verify the entire later target before each restore. Never delete it as
   cleanup or imply that its writes have been reconciled.
4. Exact `sqlite3 -readonly` metadata inspection above, then generation-bound
   pruning and retained `(F,H]` coverage checks with audit/receipts/allocator
   unchanged. This run's intended disposable path is APFS-backed `target/`.
   The exact recipe passed there with SQLite CLI 3.54.0. The earlier DeltaFS
   read-only open-14 failure is **not resolved for that filesystem** by this
   result; successful CLI/API maintenance alone never proves the diagnostic
   recipe works. Rehearse it on the intended deployment filesystem before
   approval. Do not change live configuration or silently use a writable open
   as a workaround.

Ordinary CI runs deterministic correctness, process-crash/retry tests and
recorded provenance controls. Reproduce runtime evidence only on disposable
stores, using **new** output and work directories:

```sh
python3 docs/baselines/v2-phase-e/test_rehearse.py
python3 docs/baselines/v2-phase-e/final-gate/test_gate.py
cargo test --locked --test phase_e_final_gate --test read_release_manifest
python3 docs/baselines/v2-phase-e/final-gate/gate.py \
  --output target/phase-e-new-evidence \
  --work-dir target/phase-e-new-disposable
```

The manual workflow `.github/workflows/phase-e-evidence.yml` performs the same
runtime command. It does not replace recorded evidence, run on ordinary PRs or
approve release. Before any timed samples the harness freezes both builds,
toolchain/locks/SQLite/platform, comparable seed-2003 100/10k/100k fixtures,
and all relevant harness/test/source hashes. It collects 5 warmups and 30 pairs
per workload with alternating baseline/candidate order. Timing is supported
one-shot CLI wall latency including startup/preflight/response/reap, not isolated
SQL time. Raw pairs, median, nearest-rank p95 and a deterministic 95% paired
bootstrap interval are retained.

The predeclared effective-write trigger is **>=10% paired median slowdown and
95% interval wholly above parity**, requiring explanation/disposition review.
Overlapping intervals at that magnitude are inconclusive and require repeating
the whole protocol, not cherry-picking samples. There is no absolute time gate
in CI. Main-DB and WAL bytes are observed separately before/after each workload
and after explicitly named `wal_checkpoint(TRUNCATE)`. Both idle observers
establish SQLite access before setup, with no active read transaction/external
reader; production WAL/FULL and default 1000-page auto-checkpoint remain unchanged.
Logical retention does not promise file shrinkage.

The report and recorded review cover the inactive foundation only. `sync_enabled=0` and
`bootstrap_required=1` remain mandatory; live release/maintenance remains
unapproved. Actual sync-cursor expiry, successful bootstrap and reconciliation
are deferred to Phase F V2-036/V2-039; they are not prerequisites of this
foundation gate (no dependency cycle). Process-kill evidence is process-crash/
reopen only, not power-loss/torn-write proof. Maintenance interruption tests
exercise the SQLite publication primitive, not an instrumented CLI interior.

### Phase E release-readiness handoff

Independent aggregate review of `d4399a9` through `cd4a505` passed for **local
integration only**; the [review record](baselines/v2-phase-e/review-record.json)
retains the exact range and exclusions. It does not authorize publication,
deployment, live-store opening or maintenance. The later evidence cleanup and
release bookkeeping are separate review scopes.

The [release-candidate draft and checklist](bif-v2-phase-e-release-candidate.md)
identify the missing operator evidence and Ubuntu/macOS CI results. Every
`phase_e.operational_release_gates` entry in the manifest remains open.
Phase D's top-level `release_gate.approved=true` applies only to its historical
schema-3 read candidate; Phase E's separate `release_approved=false` is unchanged.
Complete the Phase E gates and obtain an explicit scoped operator decision
before changing that value. Do not run candidate `doctor` or ordinary reads
against a real store to collect inventory: those opens can migrate it.

## Rollback is recovery, not transparent undo

Stop all processes again and preserve a SQLite-consistent copy of the
post-upgrade store before any restore. Switching back to an old executable
alone cannot make it read a newer schema. There is no supported in-place
down-migration.

Restoring the pre-upgrade backup discards every later write unless those writes
are separately preserved and reconciled. The operator must explicitly accept
that loss or approve a reconciliation plan. For an older-schema rollback, restore
into a new root using the historical SQLite procedure, verify it, and change
launch/config paths as one maintenance decision; current-schema restore above
cannot down-migrate. Do not overwrite an active database or leave stale WAL/SHM
files beside a replacement. Keep both stores and inventories until the operator
approves their disposition.

After restore, verify metadata, integrity, full reads and history using the old
binary against the restored root. For this read candidate, restart all MCP
servers and invalidate retained validators/cursors unconditionally. Current
tokens bind to generation, but unsupported manual/historical restore does not
automatically rotate it. Only the current-schema maintenance restore above
provides a fresh generation. Preserving a backup's old generation manually is
not a promise of restore detection. Future journal releases must also rebuild
mirrors according to their generation-aware procedure.

## Disposable v1 rehearsal evidence

The initial rehearsal used the 100-item fixture, seed `2003`, generated from
base revision `45ed7fc` with only additive application read-port changes present.
No index migration had been added. Python/SQLite `3.54.0` performed online backup.
Source and backup had identical store ID `benchmark-00000000000007d3-100`,
migration versions/checksums 1 and 2, and counts: 100 items, 339 criteria,
100 provenance rows, 516 operations, and 516 events. Both integrity checks
returned `ok`; both foreign-key checks returned no rows.

The system SQLite read-only open initially failed on the generated WAL-mode
source without sidecars; opening the existing source with `mode=rw` succeeded.
The procedure therefore documents sidecar access rather than pretending every
read-only WAL open works. Early failed attempts were retained as disposable
artifacts and not reused as verified backups.

The guarded snippet was subsequently checked with Python `3.9.6` / SQLite
`3.54.0` on a newly generated 100-item, seed `2003` fixture with the same
inventory above. The exact shell snippet succeeded normally and with
`PYTHONOPTIMIZE=1`; its Python body also succeeded with explicit `python3 -O`.
Injected inventory mismatch and non-`ok` integrity results, plus a real
foreign-key violation in a disposable fixture clone, each raised `RuntimeError`
under `-O` with empty stdout (no successful verification JSON). Integrity-result
injection tested the rejection branch, not a physically corrupted database.

Mocked `mktemp` failure, empty/relative/outside/nested/nonexistent output,
parent-resolution failure, and parent/child `mkdir` failure all stopped before
Python invocation or database destination claim. Empty and missing sources
were rejected. An existing database destination and an existing `.bif`
directory were refused without changing their contents. Shell syntax and
calling-shell continuation with unchanged variables/options were checked.
No real ledger was used; these checks verify only backup/setup safety, not
the deferred indexed-read or journal upgrade/rollback rehearsals.
