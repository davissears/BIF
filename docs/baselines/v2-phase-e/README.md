# V2-032: authentic schema-3 to schema-4 executable proof

**Result:** disposable executable/process proof passes; Phase E operational
release gates remain pending. No production source or migration changed.
Phase D evidence and approval are unchanged and do not approve Phase E.

## Reproduction and provenance

Run from the repository root:

```sh
python3 docs/baselines/v2-phase-e/rehearse.py --output docs/baselines/v2-phase-e/upgrade-proof.json
python3 docs/baselines/v2-phase-e/test_rehearse.py
cargo test --locked --test phase_e_upgrade_proof --test phase_e_schema_guards --test read_release_manifest
```

[The proof](upgrade-proof.json) records the actual commands, stdout/stderr,
exit codes, process IDs/order, full before/upgraded/final persisted snapshots,
hashes and environment. The harness archives exact commit
`d4399a93e49ce8f87e39e3129377821858472807` into disposable scratch, extracts it
without edits, and runs:

```sh
git archive --format=tar --output <scratch>/historical.tar d4399a93e49ce8f87e39e3129377821858472807
tar -xf <scratch>/historical.tar -C <scratch>/historical-source
cargo build --locked --release --bin bif --target-dir <scratch>/historical-target
```

The archive command's source is the attached repository; the historical build's
working directory is `<scratch>/historical-source`, **not the edited checkout**.
Candidate build uses the checkout at
`70d2f572d5d6a004e014a0c6d9f18825ed8021aa` with only proof/test/documentation edits
and a separate `<scratch>/candidate-target`. Exact absolute commands are retained
in JSON. Scratch source/targets/root are discarded at completion; the artifact
retains executable evidence and hashes, not distributable release binaries.

Both builds use Rust/Cargo `1.94.0`, `aarch64-apple-darwin`, release profile and
locked dependencies. Declaration hashes and actual `rustup which` toolchain
executable hashes are recorded. Source-input maps are checked before/after
build and rehearsal; historical inputs cover every archived file, candidate
inputs cover production Rust/migrations/Cargo/toolchain files. Aggregate source
hashes are SHA-256 of sorted-key compact JSON maps of path-to-SHA-256. These
different scopes are explicit, not an assertion that both maps have identical
contents. The host is macOS; Python `3.9.6` / inspection SQLite `3.54.0`.

| Input | Historical | Candidate |
| --- | --- | --- |
| Executable SHA-256 | `863139b0a60ac97088b1b7c939cf234d035c3b63bafdbca7f6a1f62f2919fcc3` | `9df1719a57a4664238ff4b2b2571188d22f7ae645cebfe76d3973af34ad013bb` |
| Source-map SHA-256 | `f79518baea8bac60e6e5732be0db0e54301b2f0028c3f515d82e36e027624d8b` | `c9ebbdf7b7a87780a13f03d6b290da081e5a591c5ed3401494b7a96484328109` |
| Cargo.lock SHA-256 | `2172fe64d42f0a2c678c0c69dbeea33eaad8242ec434520a949a93c29bea39d1` | `99af2058c1bbb6aafb7364b33b8d424dcb6ae70eec30b7ac40573def368f374a` |
| rust-toolchain.toml SHA-256 | `dd510b4a3ab011309bb12f6fc3450596da2080670bd4f922eaff7cad1ca65794` | `dd510b4a3ab011309bb12f6fc3450596da2080670bd4f922eaff7cad1ca65794` |

## Actual process mode and window

Historical writes are **one-shot CLI/RPC**, not a warm writer mode. This rehearsal
uses real one-shot CLI; RPC is not exercised. Historical warm MCP is read-only,
not exercised and not claimed to be fenced as a writer.

1. The real historical binary initializes a new disposable schema-3 store.
2. A parent-owned SQLite `BEGIN IMMEDIATE` lock prevents an old capture from
   completing while the harness spawns that actual binary, sends `SIGSTOP` and
   waits for `waitpid(WUNTRACED)` acknowledgement. This proves OS process stop,
   not readiness at an old SQL statement.
3. The disposable launcher's maintenance sentinel is enabled. A real launch
   attempt is blocked before process creation. The lock is released, the actual
   child receives `SIGCONT`, finishes its capture and is waited/reaped with exit 0.
4. With restart prevention still active and no old child left, candidate
   `doctor` upgrades schema 3 to 4. Every legacy table value survives unchanged
   except the expected appended migration record; candidate v1 get/history and
   replay of the historical receipt succeed.
5. Candidate capture and approval persist two journal positions with H=2,
   `sync_enabled=0`, `bootstrap_required=1`. Five restart attempts are blocked
   across before/after drain, before/after migration and after candidate writes.
6. Only after candidate verification is the sentinel removed. Fresh historical
   `doctor`, `get` and `capture` processes each exit 1 with
   `database schema version 4 is newer than supported version 3`.
   Full persisted tables, `sqlite_schema` and persistent pragma hashes are
   identical before/after every rejection, not merely matching counts. The
   inspected pragmas include schema/user version, application ID, page size,
   auto-vacuum, encoding and journal mode. This is logical persisted-state
   equality, not a claim that WAL/SHM or database physical bytes are unchanged.

The serial disposable launcher gate intentionally makes **no atomicity claim
against outside launchers**, direct binary invocation or direct SQL. No old
process starts during the protected window. This is an executed maintenance
example, not a replacement for operator service/launcher inventory.

## Tests-first and current supported write boundary

Focused proof/control and separate manifest tests failed before their artifact,
harness and registration existed. Schema-change stale-handle tests were added
before considering runtime changes and passed the existing guards: capture
replay/new capture and mutation replay/no-op/effective paths reject with
`restart_required` and preserve full persisted state after a DDL schema change.
Direct SQL in those tests is test-only invalidation, not a supported write path.
No concrete guard gap was found, so runtime remains unchanged.

Existing generation transaction ordering, journal append, capture/mutation
rollback (including H-update failure), and controlled subprocess fault suites
remain the supported boundary evidence. They do not retrofit historical code or
turn fresh-open version rejection into retroactive old-connection fencing.

## Validation results

[Recorded validation](validation-results.json) binds the rehearsal proof hash
and retains actual command argv, exit codes, stdout and stderr. All checks exit 0:

```sh
cargo test --locked --test generation_transactions --test journal_storage --test capture_journal --test capture_journal_process --test mutation_journal --test mutation_journal_process
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets
git diff --check
```

- Python controls/integrity: 4 passed.
- New proof/schema/manifest checks: 13 passed.
- Focused existing guard/journal/capture/mutation/process checks: 29 passed,
  2 ignored child-entry helper tests (invoked explicitly by their parent process tests).
- Whole `cargo test --locked`: 388 passed, 4 ignored. Besides those two helper
  entrypoints, `compare_candidate_indexes_on_actual_queries` and
  `publish_phase_c_read_comparison` remain intentionally ignored; this task did
  not run those unrelated index/timing measurements.
- Formatting/diff checks pass. Clippy exits 0 with existing warnings outside the
  changed code; no adjacent runtime cleanup was made.

## Release gates and unverified limits

Separate Phase E manifest registration is **not approved**. Still required:

- Live operator's retained executable hashes, service/launcher/config inventory.
- Verified live stop/drain/prevent-restart window and explicit Phase E authorization.
- Configured-host and other-platform validation for this Phase E candidate.
- Host crash, power-loss, filesystem durability and durable recovery validation.

No sleeps, old-source modifications, simulated old client, hypothetical warm
writer fence, real ledger/store, launcher/config replacement, restore, pruning,
sync activation, generation reset, Phase F work or live migration occurred.
Missing exact toolchain, dependencies/authentication or operator configuration
required for this disposable task would block execution; none was unavailable.
