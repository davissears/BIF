# BIF upgrade and rollback runbook

**Status:** Initial V2-054 operator checklist. Backup and integrity procedures
below were rehearsed on a generated v1 store; release-specific upgrade and
rollback rehearsals remain deferred: V2-027 owns the indexed-read release,
V2-035 owns the journal release, and V2-033 implements restore-generation reset.

## Compatibility and maintenance window

V1 CLI and RPC response compatibility does not imply old-binary/new-schema
compatibility. BIF checks migration versions and checksums; an old binary
rejects a newer schema. Production connection opening automatically applies
embedded migrations, including during ordinary reads and `doctor`. Do not
point a candidate binary at the real store just to inspect it.

The `bifc1.` hex cursor format is unchanged, but its encoded size ceiling is now
1 MiB. Earlier binaries with a 16 KiB ceiling cannot resume larger continuations
after rollback and may fail to generate continuations for supported long IDs.
Restarting pagination does not remove that old-binary limitation; use a binary
with the compatible cursor budget to traverse those records.

Inventory the exact CLI executable paths, host launch commands, scheduled jobs,
configuration files, root overrides, and all processes opening the store.
Record binary hashes, source revisions, the database path, store ID, migration
versions/checksums, and a maintenance owner. Retain the old binary and config.
The configured database is `ROOT/.bif/bif.sqlite`, not the repository itself.

Stop writers and prevent launchers from restarting them. Pause readers too
before schema migration or restore. Future persistent MCP servers must be
stopped and restarted: configuration and schema resources are pinned for the
process lifetime. There is no MCP server or BIF-specific backup/restore command
in the initial release; do not invent one in a launch checklist.

## SQLite-consistent backup: existing executable procedure

Use SQLite's online backup API, not a filesystem copy of a live main database.
Committed data may still be in the WAL. Never discard WAL/SHM files to make a
backup appear readable. The source and destination directories must be trusted,
and no other process may replace their entries during this procedure.

The example requires Python 3 with `sqlite3`. Set `SOURCE` to the inventoried
database. For rehearsals, use only a generated disposable fixture. Choose a
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
2. Take and verify the stopped-store backup above. Keep it pristine.
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

V2-027 finalizes the indexed-read release runbook and rehearses its schema
upgrade and rollback. V2-035 extends the checklist with journal-release
procedures and rehearses migration, backup, and restore. V2-033 implements the
restore-generation reset used by that later restore workflow. All these
release-specific rehearsals remain deferred; this initial checklist verifies
only the backup procedure. Index-only Phase B migrations still need this
checklist and migration tests, but do not claim those later features exist.

## Rollback is recovery, not transparent undo

Stop all processes again and preserve a SQLite-consistent copy of the
post-upgrade store before any restore. Switching back to an old executable
alone cannot make it read a newer schema. There is no supported in-place
down-migration.

Restoring the pre-upgrade backup discards every later write unless those writes
are separately preserved and reconciled. The operator must explicitly accept
that loss or approve a reconciliation plan. Restore into a new root, verify it,
and change launch/config paths as one maintenance decision; do not overwrite
an active database or leave stale WAL/SHM files beside a replacement. Keep both
stores and inventories until the operator approves their disposition.

After restore, verify metadata, integrity, full reads and history using the old
binary against the restored root. Rebuild future mirrors and invalidate their
cursors according to the release's generation-aware procedure; store-ID-only
binding is not a promise of restore detection.

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
