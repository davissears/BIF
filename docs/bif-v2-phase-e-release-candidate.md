# Phase E release-candidate PR draft and checklist

## Current decision

The inactive schema-4 journal foundation is ready for PR preparation, **not a
Phase E release or live rollout**. `release_approved=false`, `sync_enabled=0`
and `bootstrap_required=1` remain unchanged. Phase F sync, expiry, bootstrap and
reconciliation are deferred; they do not block review of this inactive foundation.

This file is a local draft, not a submitted PR or publication authorization.
The configured source remote is `origin` (`https://github.com/davissears/BIF.git`);
`local` is the primary-checkout backlink and must not be used for publication.
Read-only GitHub inspection found no workflow runs for `eedcea4` and no matching
Phase E PR. The required Ubuntu/macOS results are therefore still pending.

### Verified Git and review state

- Initial base: `d4399a93e49ce8f87e39e3129377821858472807`.
- Foundation batch head: `cd4a50579ca6016c117c7d8463a60ab8096bc95a`.
  [Independent aggregate review](baselines/v2-phase-e/review-record.json) found
  no actionable findings across all 143 changed paths and approved local
  integration only.
- Cleanup head: `eedcea4a731b61432cddb1f5169e25a177d85739`, descending from the
  batch. It removes 21 failed-attempt payloads, retains recovery indexes and
  updates the report; accepted evidence and bound inputs are unchanged.
  Independent PR-reviewer review passed with no actionable findings, including
  exact recovery snippets, historical hashes and links. The review record above
  retains this separate evidence-storage scope; it is not release approval.
- Working branch for this continuation: `release/phase-e-rc`, based on that
  cleanup without rewriting history. At initial inspection, both this clone's
  and the primary checkout's `main` refs were still at `d4399a9`, contrary to
  the earlier handoff's local-main claim. Do not assume either has integrated
  the batch or cleanup. No integration is authorized by this document.

## Proposed PR

**Title:** Prepare inactive schema-4 journal foundation

**Base:** `main` in `davissears/BIF`, after verifying its current tip.

### Summary

- Add migration 4 and inactive generation/journal metadata, with no historical
  timestamp backfill or synchronization activation.
- Journal successful captures and effective mutations atomically once per
  operation; preserve replay/no-op semantics, immutable audit and receipts.
- Bind supported readers, cursors, validators and writer handles to generation;
  reject incompatible schema/generation before replay or new writes.
- Add explicit offline current-schema backup/restore with independently rotated
  generations and preserved later state, plus global logical journal retention.
- Retain authentic historical-binary refusal and rollback proof, frozen paired
  write-cost measurements, process-crash controls, a runbook and CI.
- Keep failed-attempt payloads recoverable from Git rather than duplicated in
  the current tree. Do not squash away their recovery commit without revisiting
  the documented retrieval contract.

### Evidence and validation

The [accepted report](baselines/v2-phase-e/final-gate/REPORT.md) and its linked
artifacts bind production inputs at `f0cd787` and protocol SHA256
`dff0bd5e5413a0a4dfcb2eda0b23494fd1d5189047ecf572f2546cb13ea0be5e`.
All 18 workloads / 630 warmup-plus-measured pairs were `no_trigger` under the
frozen criterion. This is one-host, warm-cache macOS evidence, not Ubuntu,
configured-host, live-window or power-loss approval.

The prior independent batch review reports `cargo test --locked` with 412
passed, 6 ignored and 0 failed, plus formatting, Clippy, both Python control
suites and exact recorded-evidence validation. Four ignored entries are child
harnesses exercised by parent tests; two are manual measurements. These are
recorded prior results, not a claim that GitHub CI has run on this branch.

This continuation also executed these checks locally on macOS arm64 after the
documentation-only readiness changes:

| Check | Result |
| --- | --- |
| `cargo test --locked` | 412 passed, 6 ignored, 0 failed |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --locked --all-targets` | Passed with existing warnings |
| `python3 docs/baselines/v2-phase-e/test_rehearse.py` | 4 passed |
| `python3 docs/baselines/v2-phase-e/final-gate/test_gate.py` | 22 passed, including recorded-evidence validation |
| `python3 docs/baselines/v2-phase-e/final-gate/gate.py --validate --output docs/baselines/v2-phase-e/final-gate` | Passed |
| Documentation/provenance checks | All 41 production and 70 harness/test/workflow bindings unchanged; accepted protocol/proofs/measurements/validation manifest byte-identical to cleanup |

The standalone Python suites were run before these documentation edits and
re-exercised by `cargo test --locked` afterward. No historical build, full timing
protocol, configured host, Ubuntu or power-loss test was rerun here.

Before submission, obtain a PR-reviewer decision covering the final readiness
diff in addition to the recorded batch and cleanup reviews. After explicit publication
authorization, push the candidate branch to the verified source remote and
open a **draft** PR; require both `Check on ubuntu-latest` and
`Check on macos-latest` on the actual PR revision. Passing CI permits review,
not Phase E release approval. Do not merge, tag or publish a release without
separate authorization.

### Compatibility and risk disclosure

Ordinary candidate reads and `doctor` can apply migrations. Schema-3 binaries
refuse schema 4 on fresh open; that does not fence already-open old connections.
`--offline` acknowledges a prerequisite, not proof of shutdown or exclusion.
Rollback is recovery, not transparent undo: preserve later writes and agree
on loss/reconciliation before replacement. Retention limits logical coverage,
not physical file size. The DeltaFS read-only diagnostic limitation remains
unverified there; successful APFS evidence is not a workaround or universal claim.

## Remaining release checklist

The gate identifiers below are the unchanged
`phase_e.operational_release_gates` in the [manifest](bif-v2-release-evidence.json).
All five are **open**. Missing user-controlled setup is a blocker, not permission
to infer approval or change configuration.

| Gate | Missing evidence / decision | Next owner and safe boundary |
| --- | --- | --- |
| `live_operator_binary_service_launcher_inventory` | Actual retained executable paths/hashes/revisions; every service, scheduled job, MCP host and direct launcher; sanitized config/root overrides, store identity/schema and maintenance owner. Disposable historical builds are not that inventory. | Operator supplies or authorizes collection of the exact inventory. Do not open the live store with a candidate to inspect it. Keep credentials and sensitive inventory out of a public PR. |
| `live_stop_drain_prevent_restart_window` | Approved window and actual stop, drain/reap, reader shutdown and restart-prevention evidence for every inventoried entry, maintained through verification. A sleep or disposable sentinel is insufficient. | Operator chooses the window and authorizes the operations. No live shutdown or launcher edit has been requested here. |
| `explicit_phase_e_operator_authorization` | A human decision naming the exact candidate executable/hash, schema 4, root, verified pristine backup, preservation/recovery plan and maintenance window. Phase D signoff does not apply. | Operator explicitly authorizes Phase E after reviewing its evidence and remaining gates; live maintenance still needs its exact scope. |
| `configured_host_and_platform_validation` | Current Phase E candidate in the intended configured host against an authorized disposable root; CLI/MCP list/get/history, conditional reads, cursors and restart/recovery behavior; exact read-only diagnostic on the deployment filesystem; required Ubuntu/macOS CI on the PR revision. | Operator supplies host/target platform and permits the disposable-host setup. Agent can run repository CI after publication is authorized. Historical Phase D host evidence and local macOS tests do not close this gate. |
| `power_loss_and_durable_recovery_validation` | Target filesystem/storage and durable-backup policy; agreed fault/power-loss recovery method, integrity/state checks and retained results demonstrating the promised durability boundary. Process-kill/reopen tests are not torn-write or power-loss evidence. | Operator identifies the deployment/storage target and approves a safe isolated test environment and protocol. Do not interrupt power or fault a live store; do not silently weaken the gate. |

### Decisions needed to continue

1. **PR publication:** authorize pushing `release/phase-e-rc` to `origin` and
   opening a draft PR to `main` after the final PR-reviewer check. This does not
   authorize merging, tagging, releasing or live maintenance.
2. **Deployment scope:** identify the intended OS/filesystem and actual configured
   MCP host/version, plus the owner of the retained binary/launcher inventory.
   Supply sanitized inventory or explicitly authorize a scoped read-only
   collection method that cannot migrate the live store.
3. **Operational validation:** choose a disposable host/storage environment and
   agree on the durable/power-loss recovery test plan. Any live window and exact
   Phase E authorization remain separate later decisions.
