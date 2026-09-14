# BIF v2 pre-Phase-B baseline

**Measured:** 2026-09-13 UTC by the V2-004 release harness, before any Phase B read
implementation.

This directory is the V2-006 release baseline. The compact `generator-*.json`
files are the sidecar metadata emitted by the deterministic V2-003 generator.
The `measurement-*.json` files are the complete raw V2-004 harness results.
SQLite fixtures and build output are intentionally excluded.

## Provenance and reproduction

All three runs used anchored revision
`a3cc0873744eb88fd2b07adf6d6daefe7d0856b7`, but the harness recorded
`dirty: true`. The dirty tree contained the independently approved Phase A
V2-003/V2-004/V2-005 implementation and documentation. Consequently these
artifacts describe that exact working tree, not the clean anchored commit and
not a revision-only comparison point. Regenerate after checkout rather than
assuming the commit alone reproduces the implementation.

Reference environment:

- Apple arm64, `aarch64-apple-darwin`
- macOS/Darwin 25.6.0, kernel
  `25.6.0 Darwin Kernel Version 25.6.0: Fri Jul 31 19:11:03 PDT 2026; root:xnu-12377.161.14~5/RELEASE_ARM64_T8132`
- `rustc 1.94.0 (4a4ef493e 2026-03-02)`, release profile
- bundled SQLite 3.51.1
- WAL journal, foreign keys ON, `synchronous=FULL`
- no OS cache eviction; warm operations reused one connection and OS cache.
  Filesystem-cold evidence is unavailable.

From the repository root, reproduce the inputs and measurements:

```sh
rm -rf target/bif-v2-006-refresh
mkdir -p target/bif-v2-006-refresh docs/baselines/v2-pre-phase-b
cargo run --release --locked --bin bif-benchmark-store -- 100 --seed 2003 --output target/bif-v2-006-refresh/100.sqlite3
jq -c . target/bif-v2-006-refresh/100.sqlite3.metadata.json > docs/baselines/v2-pre-phase-b/generator-100.json
cargo run --release --locked --bin bif-benchmark-store -- 10000 --seed 2003 --output target/bif-v2-006-refresh/10000.sqlite3
jq -c . target/bif-v2-006-refresh/10000.sqlite3.metadata.json > docs/baselines/v2-pre-phase-b/generator-10000.json
cargo run --release --locked --bin bif-benchmark-store -- 100000 --seed 2003 --output target/bif-v2-006-refresh/100000.sqlite3
jq -c . target/bif-v2-006-refresh/100000.sqlite3.metadata.json > docs/baselines/v2-pre-phase-b/generator-100000.json
cargo run --release --locked --bin bif-benchmark -- target/bif-v2-006-refresh/100.sqlite3 --samples 20 --output target/bif-v2-006-refresh/measurement-100.json
cp target/bif-v2-006-refresh/measurement-100.json docs/baselines/v2-pre-phase-b/measurement-100.json
cargo run --release --locked --bin bif-benchmark -- target/bif-v2-006-refresh/10000.sqlite3 --samples 3 --output target/bif-v2-006-refresh/measurement-10000.json
cp target/bif-v2-006-refresh/measurement-10000.json docs/baselines/v2-pre-phase-b/measurement-10000.json
cargo run --release --locked --bin bif-benchmark -- target/bif-v2-006-refresh/100000.sqlite3 --samples 1 --output target/bif-v2-006-refresh/measurement-100000.json
cp target/bif-v2-006-refresh/measurement-100000.json docs/baselines/v2-pre-phase-b/measurement-100000.json
```

Compare the logical digest and distributions, not physical SQLite bytes. Each
raw result embeds its actual binary argv; the `cargo run` wrappers above are the
commands used to invoke it. The generator elapsed times were 36 ms, 1,733 ms,
and 17,871 ms respectively,
but are fixture-construction observations, not read benchmarks.

## Fixtures

| Items | Logical digest | DB bytes | Events / operations | Criteria | Status distribution (proposed/ready/in progress/blocked/done/rejected) |
|---:|---|---:|---:|---:|---|
| 100 | `b819125481255ec7` | 466,944 | 516 / 516 | 339 | 14 / 29 / 15 / 9 / 23 / 10 |
| 10,000 | `957657e192519763` | 31,993,856 | 50,334 / 50,334 | 31,027 | 1,765 / 2,026 / 1,976 / 988 / 2,411 / 834 |
| 100,000 | `87a07c0632977092` | 324,333,568 | 503,986 / 503,986 | 306,795 | 17,696 / 20,171 / 19,884 / 9,967 / 24,183 / 8,099 |

The raw generator records additionally preserve project, priority, assignee,
and sparse-marker distributions, verified sample IDs, row counts, digest
algorithm, and `integrity_check: "ok"`.

## Release measurements

Times below are wall-clock p50/p95 in microseconds. Percentiles use the
documented nearest-rank method. Bytes are serialized production result bytes
(p50/p95 were identical), excluding the outer RPC transport envelope.

| Items (samples) | Operation | Wall p50 / p95 µs | Response bytes | Data statements/sample | Matches/sample |
|---:|---|---:|---:|---:|---:|
| 100 (20) | get | 18.583 / 32.208 | 836 | 2 | 1 |
| | history | 27.042 / 32.500 | 1,576 | 2 | 4 |
| | list all | 1,571.000 / 1,785.000 | 86,915 | 201 | 100 |
| | mutation note | 152.417 / 225.166 | 828 | 7 | 1 |
| 10,000 (3) | get | 30.167 / 47.500 | 837 | 2 | 1 |
| | history | 103.917 / 126.375 | 11,435 | 2 | 28 |
| | list all | 138,747.375 / 142,708.500 | 86,414 | 20,001 | 100 |
| | mutation note | 168.625 / 663.459 | 828 | 7 | 1 |
| 100,000 (1) | get | 50.583 / 50.583 | 837 | 2 | 1 |
| | history | 131.666 / 131.666 | 11,435 | 2 | 28 |
| | list all | 1,467,220.333 / 1,467,220.333 | 83,364 | 200,001 | 100 |
| | mutation note | 486.958 / 486.958 | 828 | 7 | 1 |

Startup-open p50/p95 was 303.250/373.833 µs (20 samples), 437.666/465.458 µs
(3), and 506.917/506.917 µs (1). It measures opening, configuring, and
migrating fresh snapshots, but is not filesystem-cold.

The dominant query-growth evidence is the known current hydration behavior:
list-all executes exactly $1 + 2M$ data statements for $M$ store matches:
201, 20,001, and 200,001. Although response projection is limited to the
production 100-item page, current code first selects all matching IDs and then
hydrates each match. Raw per-statement counters, row callbacks, VM steps,
full-scan steps, profile samples, serialization samples, and residual timings
are retained in each measurement file.

SQLite PROFILE has millisecond resolution on this platform and many short
statements report zero; zero profile time is not zero database work. Use wall
time for the table above and consult `profile_zero_samples` and
`profile_exceeded_wall_samples` before interpreting profile/residual values.

## Writes, persistence, and storage

Each sample performed one real note mutation under WAL/FULL. After closing and
reopening, the harness verified the expected revision and note-event count,
`integrity_check: "ok"`, and zero foreign-key violations:

| Items | Expected/observed revision | Note events | DB before → after | WAL before → after |
|---:|---:|---:|---:|---|
| 100 | 24 / 24 | 20 | 466,944 → 483,328 bytes | present, 0 bytes → missing |
| 10,000 | 31 / 31 | 3 | 31,993,856 → 31,993,856 bytes | present, 0 bytes → missing |
| 100,000 | 29 / 29 | 1 | 324,333,568 → 324,333,568 bytes | present, 0 bytes → missing |

This is persistence/write-path evidence, not proof of crash-proof durability.
Sizes are disposable measured snapshots; source fixtures were not mutated.

## V2-005 workflow dry run

The checked-in [workflow evaluation](../../bif-v2-workflow-evaluation.md) has
six scenarios: planning, select-and-execute, triage, refresh, recovery, and
agent handoff. Its `local_fixture_validation` dry run replayed the implemented
v1 full-JSON and compact-human calls against a freshly generated 100-item
production fixture. It validates envelopes, exact UTF-8 proxy-byte arithmetic,
guarded mutations, declared outcomes, and the pinned digest
`b819125481255ec7`.

This is strictly separated from:

- six error/retry specifications, all `not_executed` with empty attempts and
  evidence;
- v2 contract calls marked `not_implemented` (or partially specified), which
  are expected contract content rather than runtime observations;
- unknown host, model, tokenizer, token, cache, and completion-time values.

Proxy UTF-8 bytes are transport-size proxies, **not tokens**. No external model
trial was authorized or run. No predicted percentage is presented here as a
measured improvement.

## Limitations and interpretation

- Sample counts 20/3/1 are practical release observations; one sample has no
  distribution and three samples provide weak tail evidence.
- The checkout was dirty, so timings cannot be attributed to the anchored
  commit alone.
- No OS cache eviction, randomized operation order, repeated machines,
  confidence intervals, load/concurrency, power/thermal controls, or
  filesystem-cold runs were performed.
- Results are one Apple arm64 machine and should compare only with same-host,
  same-cache-procedure runs regenerated from matching logical fixtures.
- Allocator-wide allocation counts are unavailable.
- Response bytes exclude transport envelopes and are not tokens.
- Persistence checks do not simulate power loss or process crashes.
- Physical DB/WAL size can vary across SQLite/platform versions even when the
  canonical logical digest matches.
- No Phase B runtime, host workflow, token, cache, or completion measurement is
  included.
