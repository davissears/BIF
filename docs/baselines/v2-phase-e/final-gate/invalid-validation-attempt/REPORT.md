# V2-035 final journal-foundation evidence

## Disposition

**Recommend approval of the inactive storage foundation, subject to independent
review. No live Phase E release, sync activation or Phase F approval.**
No production change was necessary: the production inputs match
`f0cd787f64a6bd3d301f11e616bb587e112a14fa` exactly. V2-054 runbook and V2-055 CI
foundation are verified prerequisite document labels, not ledger IDs. Phase D's
approved read-candidate evidence remains separate and unchanged.

The authentic historical baseline is unmodified
`d4399a93e49ce8f87e39e3129377821858472807`, extracted with `git archive`, built
with its own locked dependencies and a separate release target. Both builds use
Rust/Cargo 1.94.0 and bundled SQLite 3.51.1; Python 3.9.6 and diagnostic SQLite
3.54.0 run on macOS 27.0.1 arm64, APFS-backed disposable `target/` paths.
Exact executable, toolchain, archive, source, lock, test and harness SHA256s are in
the [protocol](protocol.json). The historical/candidate executable hashes are
also checked unchanged after execution.

## Evidence inventory

- [Current upgrade proof](upgrade-proof.json): actual old one-shot writer OS
  stop/continue/drain/reap before candidate migration; five blocked disposable
  launcher restarts; fresh old doctor/get/capture refuse schema 4 without changing
  persisted tables/catalog. Not retroactive fencing of an old warm connection.
- [Final proof](proof.json): fresh migrations 1–4, exact histories/checksums;
  genuine old schema-3 backup/rollback into another root and old CLI
  doctor/get/history/receipt replay; current-schema backup with a journal row,
  two identical-input restores with three distinct generations, verified
  preservation of each complete later target, unchanged backup bytes;
  generation-bound prune from `(0,3]` to `(2,3]`, with immutable audit, receipts,
  allocator and every other table unchanged.
- The exact documented read-only diagnostic returned exit 0 and
  `f5da4d3bfdfd574cc4f5a6ba5be95147|0|3`, with empty stderr, on the intended
  disposable APFS-backed path. No writable-open substitution or live-config
  workaround was used. **Prior DeltaFS open-14 remains filesystem-specific
  unverified behavior**, not a production/API failure fixed by this task.
- [Validation results](validation-results.json) name actual newly executed
  commands, outcomes, counts and full-log SHA256s. They include actual
  process-kill/reopen/retry, migration preflight/checksum refusal, injected
  journal/H failures, capture project/counter rollback, duplicate keys,
  note-only/compound-once, replay/no-op-none, retention, backup/restore and token
  invalidation. This does not inherit the old 43-test audit.
  Final executions passed: Python archived controls 4/4, final-gate controls 5/5;
  focused storage/migration/maintenance command 60 passed (4 test-only child
  entries ignored at top level); proof/schema/manifest/new-gate command 15 passed;
  `cargo test --locked` 411 passed, 6 ignored, 0 failed. Four ignored entries are
  children invoked by parent process-crash tests; two are unrelated manual
  Phase C/index measurement tests not run by that command. Formatting passed;
  Clippy passed with pre-existing warnings, retained verbatim in validation-07.
- [Frozen protocol](protocol.json) SHA256:
  `ec52a4cf676f64ae6d6ace299b604924337ad79caa55caeb904354b8a4647cc8`.
  [Measurements](measurements.json) bind that hash and start after its freeze.
  The fixtures reuse the unchanged historical deterministic generator, seed
  2003, with identical legacy-table digests before measurement:

  | Items | Generator logical digest |
  | ---: | --- |
  | 100 | `b819125481255ec7` |
  | 10,000 | `957657e192519763` |
  | 100,000 | `87a07c0632977092` |

## Write-cost results

All six workloads have 5 warmup pairs plus 30 measured pairs, alternating
baseline/candidate order. No measured pair was dropped. CLI wall latency includes
spawn, startup, migration/checksum preflight, supported operation, response and
reap; reads used to construct arguments and SQL observations are outside timing.
These are generated, warm filesystem-cache, one-host observations, not isolated
transaction latency or a throughput/concurrency benchmark.

The table gives median candidate/baseline paired ratio and its deterministic
95% paired percentile-bootstrap interval. The protocol specifies
`random.Random(2003)`, 10,000 resamples of 30 pairs, sorted indices 249 and 9749;
per-binary median and nearest-rank p95 and every raw sample are in measurements.

| Workload | 100 | 10k | 100k |
| --- | --- | --- | --- |
| Capture | 1.004 [0.991, 1.025] | 1.003 [0.990, 1.016] | 1.020 [1.008, 1.036] |
| Effective priority | 1.059 [1.031, 1.084] | 1.054 [1.036, 1.073] | 1.052 [1.038, 1.071] |
| Note-only | 1.061 [1.049, 1.087] | 1.056 [1.038, 1.069] | 1.060 [1.052, 1.075] |
| Compound | 1.058 [1.047, 1.075] | 1.056 [1.041, 1.072] | 1.049 [1.040, 1.060] |
| Receipt replay | 1.009 [0.990, 1.031] | 1.014 [0.995, 1.029] | 1.006 [0.981, 1.026] |
| Genuine no-op | 1.029 [1.017, 1.038] | 1.026 [1.017, 1.040] | 1.038 [1.020, 1.057] |

**Disposition:** none reaches the predeclared >=10% median plus wholly-above-
parity interval trigger, nor its >=10% inconclusive-repeat band. Effective
mutation overhead is measurable (~4.9–6.1% paired medians), not zero. Capture
medians are ~13.7–14.0 ms; effective/note/compound candidate medians ~3.38–3.52 ms.
The journal's extra linked insert/metadata validation/coverage update is
consistent with the observed overhead and additional WAL bytes; this is an
explanation, not isolated causal profiling. No performance-only production
optimization is justified by the predeclared criterion.

Both observers query SQLite before setup; an explicit initial
`PRAGMA wal_checkpoint(TRUNCATE)` follows setup. There are no active reader
transactions or external readers. Production WAL/FULL and default 1000-page
auto-checkpoint settings are not changed. At 100k, capture's before/after/
post-explicit-checkpoint observations illustrate why main and WAL cannot be
combined or mistaken for retained logical size:

| Binary | Before main / WAL bytes | After workload main / WAL | After explicit TRUNCATE main / WAL |
| --- | ---: | ---: | ---: |
| Baseline | 344,997,888 / 0 | 344,997,888 / 3,452,592 | 345,059,328 / 0 |
| Candidate | 345,018,368 / 0 | 345,079,808 / 4,173,592 | 345,079,808 / 0 |

Candidate capture crossed the production auto-checkpoint threshold during this
workload; the WAL may retain allocated bytes even after an automatic checkpoint.
Explicit final TRUNCATE succeeded with no busy reader. After all effective
workloads, the candidate main DB is 49,152 bytes larger than baseline at every
fixture size. Replay and genuine no-op add no journal rows or observed WAL bytes.
Pruning bounds logical coverage, **not physical file shrinkage**.

## Attempts and issue disposition

Tests were written first. The initial Python/final-gate Rust check failed because
`gate` was missing; the new manifest assertion separately failed on absent
`phase_e.final_gate`. Focused regression controls were also red before adding
Rust C-string version parsing and symmetric observer initialization.

Two pre-freeze harness attempts stopped without timing: missing isolated rollback
config, then SQLite provenance parsing expecting byte strings rather than Rust
`c"..."` bindings. Both were corrected without production changes or fixture/
protocol deviation; their upgrade proofs remain `attempt-1-upgrade-proof.json`
and `attempt-2-upgrade-proof.json`. The first complete timing run was invalid:
only the candidate observer had established SQLite access during capture.
Its protocol/raw results are retained under
[`invalid-observer-attempt/`](invalid-observer-attempt/README.md) and excluded from
the conclusions above. After a focused failing control and symmetric
initialization/checkpoint fix, the **entire** protocol was refrozen and every
size/workload rerun. No invalid-run sample was selected for the final result.
The output-path safety check refused to overwrite that provisional evidence;
no timing ran on that refusal or subsequent filesystem-only checks.

No material out-of-scope production blocker was found. No arbitrary issue-count
threshold was used. Ledger/Git operations remain coordinator-owned.

## Residual release limits

- `sync_enabled=0`, `bootstrap_required=1`: no activation, successful bootstrap,
  sync-cursor expiry or reconciliation. Phase F V2-036/V2-039 is explicitly
  deferred, not a circular dependency of this foundation gate.
- Process crashes/reopen/retry are not power-loss/torn-write/fault-injecting-VFS
  evidence. Maintenance kill tests cover the actual SQLite publication primitive
  boundaries, **not an instrumented production CLI interior**.
- The disposable serial launcher proves only its own gate, not a live launcher/
  service inventory, hostile/out-of-band writers or live shutdown enforcement.
- No live-store/config/ledger operation, production maintenance, publication,
  push, activation or configured Phase E MCP host approval. Cross-platform CI is
  registered; this local execution is macOS only, not an executed Ubuntu claim.
- Random 128-bit generation collisions are assumed negligible, not impossible.
  Saved later writes are retained, not reconciled. Retention is logical.
- New production/harness/test changes intentionally invalidate this pinned
  evidence until explicitly refreshed; passing CI is not human release approval.
