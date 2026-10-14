# BIF upgrade and rollback runbook

**Status:** V2-054 operator checklist, Phase D indexed-read candidate procedure,
and V2-032 disposable Phase E schema-3 to schema-4 proof. The required Phase D
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
Journal rollout, activation, reconciliation and
live operational release gates remain unfinished.

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
V2-035 will add journal-release procedures. V2-033's current-schema restore above
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
