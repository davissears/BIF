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

This is the fresh Step 3 run after both independently approved P2 harness
corrections. The reviewed `gate.py`, `test_gate.py`, all bound Rust tests and
workflows remained frozen throughout. Their exact input hashes are recorded in
the protocol; the two Python SHA256s are respectively
`db450e1283c07a4b07693133f5930fab63c74f00d5c370bf95c8bfab567ca790` and
`9e6e84158a65ce0f79da52d8e614901bf40ff94da67a706637feec74ae5b80b3`.
The exact current-output command was:

```sh
python3 docs/baselines/v2-phase-e/final-gate/gate.py --output docs/baselines/v2-phase-e/final-gate --work-dir target/phase-e-final-gate-corrected-run-2
```

The work directory was unused. Protocol freeze time `1792019506513418000`
precedes timing start `1792019506514319000`; timing ended
`1792019519932614000` (Unix nanoseconds). All four current JSON files were
generated together by that command, not transplanted from another attempt.

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
  `2e0185c89f20ddbf2532c2a43f7bafef|0|3`, with empty stderr, on the intended
  disposable APFS-backed path. No writable-open substitution or live-config
  workaround was used. **Prior DeltaFS open-14 remains filesystem-specific
  unverified behavior**, not a production/API failure fixed by this task.
- [Validation results](validation-results.json) name actual newly executed
  commands, outcomes, counts and full-log SHA256s. They include actual
  process-kill/reopen/retry, migration preflight/checksum refusal, injected
  journal/H failures, capture project/counter rollback, duplicate keys,
  note-only/compound-once, replay/no-op-none, retention, backup/restore and token
  invalidation. This does not inherit the old 43-test audit.
  Fresh executions passed: Python archived controls 4/4, final-gate controls 22/22;
  focused storage/migration/maintenance command 60 passed (4 test-only child
  entries ignored at top level); proof/schema/manifest/new-gate command 15 passed;
  `cargo test --locked` 411 passed, 6 ignored, 0 failed. Four ignored entries are
  children invoked by parent process-crash tests; two are unrelated manual
  Phase C/index measurement tests not run by that command. Formatting passed;
  Clippy passed with pre-existing warnings, retained verbatim in validation-18.
- [Fresh-evidence adversarial controls](validation-11.json) call full
  `gate.validate` on temporary copies of this run. Replacing every candidate
  sample argv with its baseline argv is rejected at `candidate 100 capture 0`;
  swapping entire effective/note-only workloads is rejected at
  `baseline 100 effective 0`. Both fail specifically on semantic argv, without
  changing envelope hashes, durations or statistics. The same log verifies the
  real canonical 100-item generator fixture: eight digest keys, a counter-only
  `next_sequence` change detected, and all eight missing tables rejected on
  both independent copies (16 cases). The 22 Python controls additionally
  cover setup/results/context, ordered expected effects, counter-only prepare
  refusal, empty tables and exact required-key coverage.
- [Preservation validation](validation-19.json) verifies all 16 archived
  originals against their inventory and 35 pre-existing non-current-evidence
  files against pre-run hashes, including both earlier attempt upgrade proofs
  and the entire invalid-observer archive. Production, historical Phase D and
  task007 inputs compare unchanged to `f0cd787`. No ledger/Git write occurred.
- [Frozen protocol](protocol.json) SHA256:
  `d9f39fe14da2ee7e5161580bb3055421aeacc695aa4873f48a4e33d89358abb1`.
  [Measurements](measurements.json) bind that hash and start after its freeze.
  The fixtures reuse the unchanged historical deterministic generator, seed
  2003, with identical digests for all eight required legacy tables before
  measurement, including `requester_project_counters`; a missing table fails
  rather than silently dropping a digest key. Generator logical digests:

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
Recorded setup commands and outputs establish the initial state; the frozen
semantic specification derives every subsequent command and expected output,
including exact executable/config/root/requester/cwd binding for both roles.
There are 630 warmup-plus-measured pairs (1,260 workload CLI executions), plus
12 recorded setup commands. No sample is selected from an older run.

The table gives median candidate/baseline paired ratio and its deterministic
95% paired percentile-bootstrap interval. The protocol specifies
`random.Random(2003)`, 10,000 resamples of 30 pairs, sorted indices 249 and 9749;
per-binary median and nearest-rank p95 and every raw sample are in measurements.

| Workload | 100 | 10k | 100k |
| --- | --- | --- | --- |
| Capture | 1.012 [0.993, 1.018] | 1.020 [1.003, 1.027] | 1.015 [1.006, 1.025] |
| Effective priority | 1.084 [1.060, 1.118] | 1.066 [1.046, 1.102] | 1.060 [1.048, 1.080] |
| Note-only | 1.069 [1.054, 1.096] | 1.059 [1.040, 1.075] | 1.063 [1.044, 1.078] |
| Compound | 1.065 [1.051, 1.077] | 1.052 [1.024, 1.083] | 1.061 [1.034, 1.083] |
| Receipt replay | 1.007 [0.998, 1.017] | 1.005 [1.000, 1.018] | 1.007 [0.995, 1.019] |
| Genuine no-op | 1.018 [1.009, 1.041] | 1.052 [1.029, 1.073] | 1.036 [1.015, 1.055] |

**Disposition:** none reaches the predeclared >=10% median plus wholly-above-
parity interval trigger, nor its >=10% inconclusive-repeat band. Effective
mutation overhead is measurable (~5.2–8.4% paired medians), not zero. Capture
candidate medians are ~14.42–14.63 ms; effective/note/compound candidate medians
~3.53–3.63 ms. This is this run's computed disposition, not an inherited
no-trigger claim. The 100-item capture baseline p95 is 33.57 ms; all samples,
including that tail, are retained. No outlier filtering or rerun selection was
used to obtain this result.
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

That former top-level evidence was subsequently invalidated by two P2 review
findings: incomplete canonical fixture-table coverage (allocator omitted) and
missing workload command-semantic binding. Its 16 original artifacts are
preserved byte-identically under
[`invalid-validation-attempt/`](invalid-validation-attempt/README.md), with
their original claims and inventory untouched. Its README describes the state
at archival time, not a claim that current evidence is still stale. Historical
top-level `validation-01` through `validation-08` are retained but superseded;
only `validation-09` onward contributes to the current validation summary.

After both corrections were independently approved, a complete fresh run into
`target/phase-e-final-gate-corrected-evidence-1` passed. A promotion-tool payload
capacity concern was reported before any edit; the coordinator resolved it by
direct generation with the same frozen harness. Only the four stale current
generated JSON files were deleted using `apply_patch`; the exact command above
then rebuilt everything in unused `corrected-run-2` and regenerated current
evidence. The successful target-only run remains disposable prior-run output,
not the source of any current sample or patched hash. No harness/test/workflow
or production input was edited in Step 3.

The historical red-before record is partial and inherited, not a newly executed
red phase. The fresh 22-test suite and the real-evidence negative controls are
new executions; expected attack rejections are not production failures.

No material out-of-scope production blocker was found. No arbitrary issue-count
threshold was used; the cumulative task010 issue count remains 3.
Ledger/Git operations and final independent whole-task010 rereview remain
coordinator-owned. This evidence does not itself approve the whole task.

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
