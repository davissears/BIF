# Phase F prerequisite review

The fresh independent review **approved the technical Phase F entry gate with
no actionable findings**. It reviewed clean HEAD
`abf2cd50d5c1086d2051466a34a8499295e2495f` against initial batch base
`0f24236fcd4629e503a27b295d36224e789641c8`. This document records that completed
review; the aggregate batch review remains separate.

| Commit | Reviewed scope |
| --- | --- |
| `a037226` | Exact planning input |
| `5018a80` | F-P01 contract, schema, and runbook |
| `1bc63e5` | F-P02 behavioral oracle |
| `e477819` | User-authorized historical Phase E validator remedy |
| `abf2cd5` | F-P03 activation and snapshot guard |

F-P01, F-P02, F-P03, and the historical remedy each received independent
approval before this prerequisite review.

## Basis for approval

The [required preparation and sections 2.6–2.9 of the plan](bif-v2-migration-plan.md)
agree with the [contract fixture](fixtures/bif-v2-sync-contract.json),
[schema](fixtures/bif-v2-sync-contract.schema.json), and
[activation and recovery runbook](bif-upgrade-runbook.md): activation is explicit,
offline, and bound to an expected generation. Source availability is independent
of client completeness. Publication and callbacks are guarded, recovery is typed,
replay validates durable C, and permanently unfit records have explicit handling.

F-P02 uses the application's
[`EffectiveItemFilters::matches`](../src/application/read_semantics.rs) for the
ready/mine/active membership matrix, including exclusion of done and rejected
items from mine. The [contract model tests](../tests/v2_sync_contract_fixtures.rs)
and [named-view tests](../tests/named_views.rs) cover the intended membership
semantics. Model evidence is distinct from the disposable SQLite and CLI evidence.

F-P03 [activation](../src/storage/maintenance.rs) takes an immediate transaction,
validates exact schema and migration history, integrity, identity, journal coverage
and allocator state, and the expected generation. It changes only `sync_enabled`,
revalidates, and commits. Tests cover rejection, retry, rollback after the flag
change, a committed activation whose response is lost, restart, and a restored
store remaining disabled until reactivation with its new generation.
[`JournalSnapshot::require_sync`](../src/storage/journal.rs) checks identity before
disabled status in the same snapshot as metadata and rows, after caller
authorization.

Tests-first evidence was independently corroborated through BIF-011 history
revisions 8/10, BIF-012 revision 6, and BIF-013's preserved Delta worker transcript:
intended contract and membership failures, rejection of a deliberately regressed
model, and missing activation/guard API and CLI failures preceded implementation.

## Verification evidence

The review inspected the evidence below and did not duplicate the full suite.
These results were produced on source content subsequently committed as
`abf2cd5`; the full suite was **not** run from that clean commit itself.

| Command or review evidence | Result |
| --- | --- |
| `cargo test --locked --offline --test v2_sync_contract_fixtures --test named_views` | 30 model tests and 3 application tests passed |
| `cargo test --locked --test maintenance_cli --test maintenance_process --test maintenance_restore --test journal_storage --test migrations --test capture_journal --test mutation_journal` | 55 passed; 3 ignored |
| Independent F-P03 review checks | 32 passed; 3 ignored |
| `cargo test --locked` | 433 passed; 0 failed; 7 ignored |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --locked --all-targets` | Exit 0; pre-existing warnings |
| `python3 docs/baselines/v2-phase-e/final-gate/test_gate.py` | 26 passed |
| `cargo test --locked --test phase_e_final_gate` | 1 passed |

The seven full-suite ignored tests comprise five child entries invoked by parent
tests and two manual measurement tests. The historical validator counts above
must not be added to the 433 full-suite passes. The preserved external recovery
log, `bif-delta-phase-f-20261020/task013-full-suite-after-history-fix.log`, has
SHA-256 `83e26c968b760fabd082baee948002134c49175d33fe3d8acfe0112afcf2c792`.

The historical remedy checks all four pins plus provenance, semantics, and
statistics while retaining strict validation for new measurements. No historical
JSON was rewritten. The [Phase E report](baselines/v2-phase-e/final-gate/REPORT.md)
supports the historical V2-035 storage foundation only; it supplies no new timing
or current synchronization-performance evidence.

## Entry boundary

The technical gate permits this dependency-respecting implementation order:
V2-036 → V2-037 → V2-038 → V2-039 → V2-040 → V2-040A → V2-041 → V2-042 →
V2-043 → V2-044. V2-039 can also follow V2-036 independently of V2-037/V2-038.
Each task retains its dependency gates and requires focused tests before its
implementation is accepted.

This review does not authorize executing those tasks, merging, pushing, releasing,
or activating a live store. Actual cursor decoding, scan/poll adapters, persistent
mirror and locking, end-to-end integration, stress tests, and current
synchronization-performance measurements remain future work. Process-crash
evidence does not establish physical-power-loss durability. Operators must still
stop and reap the source, prevent restart, and inventory deployed writer
participation before activation. This is technical prerequisite approval, with
no live or runtime approval claim.
