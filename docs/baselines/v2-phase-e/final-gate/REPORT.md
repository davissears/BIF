# V2-035 final journal-foundation evidence

## Disposition

**Fresh evidence is ready for independent aggregate review, not aggregate approval.
No live Phase E release, sync activation or Phase F approval.**
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

This is the fresh remediation Step 3 run at clean starting HEAD
`478aa623a229a04bd4a714f2a907728fa1f2944d`, after independently approved
contract correction `55b0a4d` and documentation `478aa62`. Initial batch/main base
remains `d4399a93e49ce8f87e39e3129377821858472807`. The contract regression
changed a bound Rust test, intentionally superseding the prior source binding.
Prior current evidence/report is preserved in Git `d66a6ae`; its raw measurements
are not retrospectively invalidated. No sample or claim is transplanted here.
The reviewed `gate.py`, `test_gate.py`, all bound Rust tests and workflows
remained frozen throughout this run. Their exact input hashes are recorded in
the protocol; the two Python SHA256s are respectively
`db450e1283c07a4b07693133f5930fab63c74f00d5c370bf95c8bfab567ca790` and
`9e6e84158a65ce0f79da52d8e614901bf40ff94da67a706637feec74ae5b80b3`.
The exact current-output command was:

```sh
python3 docs/baselines/v2-phase-e/final-gate/gate.py --output docs/baselines/v2-phase-e/final-gate --work-dir target/phase-e-final-gate-aggregate-correction-1
```

The work directory was verified unused. Generation exited 0 in 84.942727 seconds
([validation-23](validation-23.json)). Protocol freeze time `1792021905441229000`
precedes timing start `1792021905442203000`; timing ended
`1792021920041812000` (Unix nanoseconds). All four current JSON files were
generated together by that command, not transplanted from another attempt.

## Evidence storage

Failed-run payloads and obsolete harness copies are retained in Git commit
`cd4a50579ca6016c117c7d8463a60ab8096bc95a`, rather than duplicated in the current
source tree. Each attempt's README below provides read-only retrieval commands.
The accepted generated evidence, active harnesses, validation manifest and logs
are unchanged by this cleanup; no measurements were rerun.

Earlier inventory and companion logs are immutable observations of the
pre-cleanup tree and report at that commit. Their original paths and hashes
refer to that Git version, not a requirement to retain failed-run payloads in
the current tree.

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
  `bc9d4fd777a54b5f7140cc8b63bd294c|0|3`, with empty stderr, on the intended
  disposable APFS-backed path. No writable-open substitution or live-config
  workaround was used. **Prior DeltaFS open-14 remains filesystem-specific
  unverified behavior**, not a production/API failure fixed by this task.
- [Validation results](validation-results.json) name actual newly executed
  commands, outcomes, counts and full-log SHA256s. They include actual
  process-kill/reopen/retry, migration preflight/checksum refusal, injected
  journal/H failures, capture project/counter rollback, duplicate keys,
  note-only/compound-once, replay/no-op-none, retention, backup/restore and token
  invalidation. This does not inherit previous validation results.
  Fresh executions passed: Python archived controls 4/4, final-gate controls 22/22;
  exact sync-contract/restore/retention command 32 passed;
  focused storage/migration/maintenance command 60 passed (4 test-only child
  entries ignored at top level); proof/schema/manifest/new-gate command 15 passed;
  `cargo test --locked` 412 passed, 6 ignored, 0 failed. Four ignored entries are
  children invoked by parent process-crash tests; two are unrelated manual
  Phase C/index measurement tests not run by that command. Formatting passed;
  Clippy passed with pre-existing warnings, retained verbatim in validation-34.
- [Fresh-evidence adversarial controls](validation-26.json) re-execute the exact
  stdin preserved in historical validation-11 and call full
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
- At evidence-generation time, [pre-run inventory](validation-22.json) and
  [preservation validation](validation-37.json) verified all 345 immutable tracked
  files byte-identically, including validation-01–21, both failed-run archives
  and the earlier attempt upgrade proofs. The failed-run payloads are now
  recoverable from the Git version identified above.
  Immutable inventory SHA256:
  `15facb6b17b1b46cc0887b46a6d0718c175b4c1313aa4398d9e0e9b9ebc56de1`.
  All production and bound inputs match the pre-run inventory; production,
  historical Phase D and task007 inputs also compare unchanged to `f0cd787`.
  No ledger/Git write occurred in this worker.
- [Frozen protocol](protocol.json) SHA256:
  `dff0bd5e5413a0a4dfcb2eda0b23494fd1d5189047ecf572f2546cb13ea0be5e`.
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
| Capture | 1.011 [0.997, 1.028] | 1.015 [0.981, 1.038] | 1.021 [0.984, 1.037] |
| Effective priority | 1.066 [1.026, 1.080] | 1.053 [1.017, 1.132] | 1.043 [1.005, 1.072] |
| Note-only | 1.052 [1.012, 1.096] | 1.066 [1.022, 1.096] | 1.067 [1.033, 1.084] |
| Compound | 1.078 [1.047, 1.115] | 1.060 [1.032, 1.078] | 1.071 [1.059, 1.088] |
| Receipt replay | 1.001 [0.982, 1.018] | 1.000 [0.982, 1.016] | 0.997 [0.968, 1.037] |
| Genuine no-op | 1.027 [0.990, 1.042] | 1.039 [1.025, 1.057] | 1.036 [1.011, 1.052] |

**Disposition:** all 18 are `no_trigger`; none reaches the predeclared >=10% median plus wholly-above-
parity interval trigger, nor its >=10% inconclusive-repeat band. Effective
mutation overhead is measurable (~4.3–7.8% paired medians), not zero. Capture
candidate medians are ~15.24–15.54 ms; effective/note/compound candidate medians
~3.63–4.35 ms. This is this run's computed disposition, not an inherited
no-trigger claim. The 10k capture baseline p95 is 43.85 ms; all samples,
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

Historical implementation record only: tests were written first. The initial Python/final-gate Rust check failed because
`gate` was missing; the new manifest assertion separately failed on absent
`phase_e.final_gate`. Focused regression controls were also red before adding
Rust C-string version parsing and symmetric observer initialization.

Two pre-freeze harness attempts stopped without timing: missing isolated rollback
config, then SQLite provenance parsing expecting byte strings rather than Rust
`c"..."` bindings. Both were corrected without production changes or fixture/
protocol deviation; their upgrade proofs remain `attempt-1-upgrade-proof.json`
and `attempt-2-upgrade-proof.json`. The first complete timing run was invalid:
only the candidate observer had established SQLite access during capture.
Its protocol/raw results are retained in Git history; see the
[retrieval instructions](invalid-observer-attempt/README.md). They are excluded
from the conclusions above. After a focused failing control and symmetric
initialization/checkpoint fix, the **entire** protocol was refrozen and every
size/workload rerun. No invalid-run sample was selected for the final result.
The output-path safety check refused to overwrite that provisional evidence;
no timing ran on that refusal or subsequent filesystem-only checks.

That former top-level evidence was subsequently invalidated by two P2 review
findings: incomplete canonical fixture-table coverage (allocator omitted) and
missing workload command-semantic binding. Its 16 original artifacts are
preserved byte-identically in Git history, with their original claims and
inventory untouched; see the [retrieval instructions](invalid-validation-attempt/README.md).
The archived README describes the state at archival time, not a claim that
current evidence is still stale. Historical
top-level `validation-01` through `validation-21` are retained byte-identically.
They are historical observations, not current source-bound acceptance results.

In the earlier harness-correction round, a complete fresh run into
`target/phase-e-final-gate-corrected-evidence-1` passed. A promotion-tool payload
capacity concern was reported before any edit; the coordinator resolved it by
direct generation with the same frozen harness. Only the four stale current
generated JSON files were deleted using `apply_patch`; that round rebuilt
everything in unused `corrected-run-2`. Its source-bound evidence is now
superseded after the approved contract regression. Neither that run nor its
successful target-only predecessor supplies any current sample or patched hash.
The current exact command above rebuilt everything again. No harness/test/workflow,
fixture or production input was edited in this remediation Step 3.

Two new issues were reported immediately and work paused for coordinator approval:

- After generation, the recorded filesystem had 3.5 GiB free at 99% capacity.
  No ENOSPC occurred. The coordinator authorized continuation with existing
  private fixtures, no cleanup/configuration change, and a 2 GiB stop threshold.
  Validation resumed at 7.3 GiB free. Before/after observations in validation-24–37
  remained above 2 GiB; the heavy checks completed with sufficient margin
  (6,903,500,800 bytes after Clippy). The capacity concern is resolved for this
  execution by sufficient margin, not fabricated cleanup.
- Companion audit [validation-36](validation-36.json) verified immutable files
  and results, then its observer stdin requested a nonexistent `toolchain` key
  and exited 1. No frozen harness/evidence failure occurred. After approval,
  only that lookup was changed to the actual recorded `rustc`, `cargo`,
  `active_toolchain`, `toolchain_executables`, `toolchain_sha256` fields.
  [Validation-37](validation-37.json) passed; failed validation-36 remains
  byte-identical, not hidden or rewritten.

The historical red-before record is partial and inherited, not a newly executed
red phase. The fresh 22-test suite and the real-evidence negative controls are
new executions; expected attack rejections are not production failures.

No material out-of-scope production blocker was found. No arbitrary issue-count
threshold was used. Coordinator-reported cumulative counts are task002=3,
task008=2, task010=5; other task counts remain 0.
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
