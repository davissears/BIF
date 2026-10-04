# BIF v2 Phase C read-path comparison evidence

**Status: V2-020 measured on the final integrated dirty production snapshot,
release profile, all three required scales.** The measurement gate passed:
original logical digests, cursor/offset ID equivalence, bounded SELECT counts,
unchanged source/metadata, and persistence checks. Batching and projections are
supported by actual evidence; universal constant-work pagination is **not**.
Legacy canonical ordering, sparse/text predicates, and same-priority `next`
retain material residual work. See the follow-up decisions below.

Compact raw results, source hashes, environment and the reproduction runbook are in
[`baselines/v2-phase-c/`](baselines/v2-phase-c/README.md).
The original comparison point remains
[`baselines/v2-pre-phase-b/README.md`](baselines/v2-pre-phase-b/README.md).
Phase B's index/serializer evidence remains separate in
[`bif-v2-phase-b-evidence.md`](bif-v2-phase-b-evidence.md).
Existing migration/status documents are not changed by this work.

## Reproducible comparison support

The opt-in `tests/v2_read_comparison.rs` generates only disposable 100 / 10k /
100k seed-2003 stores under a newly claimed test directory. It accepts no
caller-supplied database path. Focused percentile/digest-pin tests were written
before the helper implementation; the initial disk block prevented a red run.
After integration, six measurement-only Rust typing errors were fixed using
owned SQL strings and checked conversions from signed SQLite integers.
Release support tests passed (2), the ignored comparison passed (1), the
existing serializer/allocation tests passed (15/3), and the focused benchmark
evidence test passed (1). These results are in
[`verification.json`](baselines/v2-phase-c/verification.json); parent-owned
core/CLI/storage suites were not duplicated here.

All deterministic inputs reproduced these original logical digests:

| Items | Seed | Published pre-B logical digest |
| ---: | ---: | --- |
| 100 | 2003 | `b819125481255ec7` |
| 10,000 | 2003 | `957657e192519763` |
| 100,000 | 2003 | `87a07c0632977092` |

The test validated generated metadata against those pins, verified source and
read-snapshot logical digests before and after measurements, verified unchanged
metadata bytes, and checked unchanged implementation SHA-256 during the run.
Read snapshots reuse the existing index experiment's exclusive file claim and
SQLite online backup pattern. Startup/write measurements invoke the existing
`bif-benchmark` harness, which retains its safe-source/backup/digest/publication
handling. The harness now requires one bounded primary SELECT plus one optional
batched criteria SELECT, instead of historical `1 + 2M` list hydration.

Commands actually executed, with a separate release target to avoid duplicate
debug builds and the parent's shared target:

```sh
CARGO_TARGET_DIR=target/phase-c-evidence cargo test --release --locked \
  --test v2_read_comparison
CARGO_TARGET_DIR=target/phase-c-evidence cargo test --release --locked \
  --test benchmark_harness \
  report_uses_snapshot_and_contains_review_evidence -- --exact
CARGO_TARGET_DIR=target/phase-c-evidence cargo test --release --locked \
  --test v2_read_comparison \
  publish_phase_c_read_comparison -- --exact --ignored --nocapture
CARGO_TARGET_DIR=target/phase-c-evidence cargo test --release --locked \
  --test v2_response_serialization \
  --test v2_response_allocations -- --nocapture
```

The comparison used the default 20 warm samples per case and recorded an
additional first-read observation separately. The baseline rerun uses the
same 20 samples for each scale. Percentiles use nearest rank, and the raw
execution-order samples remain available. The allocation tests are existing
synthetic serializer evidence, not whole-read allocator instrumentation.

## Measurement scope

| Area | Cases and scope |
| --- | --- |
| Projection/batching | Summary, work, audit, limits 1/10/100; actual production adapter and query plans; primary and criteria callback counters separated |
| Named shapes | Ready/next, active/list, mine/list |
| Sparse/text filtering | `rare-project` and literal `Needle-filter`; both v2 summary and bounded legacy result; correlated text predicates are not child-hydration SELECTs |
| Deep list | Last-row key at 90% of items, limit 10; summary/work/audit versus legacy offset at the same depth; exact returned IDs must match |
| Same-priority next | Ready/P4 first and 90%-deep work page, limit 10; actual last-row key versus matching legacy offset; report partition scans separately |
| History | Highest-event-count generated item, limit 10, first and after-10 revision/event-index pages |
| SQL work | Actual traced data SQL shapes; data/all statement counts; primary/criteria/other row callbacks; SQLite VM/fullscan/sort counters including transactions |
| Warm read timing | Storage selection/assembly plus tracing; excludes startup, authorization, cursor decoding, and serialization |
| Delivery timing/bytes | Real v2 cursor encoding and complete typed success envelope/newline; v1 dynamic result JSON without transport framing |
| Startup/write/storage | Original safe harness rerun, fresh-snapshot opens, WAL/FULL durable note mutations, reopen verification, DB/WAL size; read snapshot allocated/occupied page bytes |
| Provenance | Actual revision, dirty status, implementation/migration/dependency/harness SHA-256, Rust/Cargo, bundled SQLite, `uname`, test argv and selected sample/scale controls |

No `ANALYZE` or prepared-statement cache is used. Case order is fixed, not
randomized. Digest validation, backups, plans, equivalence checks, and untimed
boundary traversal warm the filesystem cache. A “first read” is therefore
session-first, **not cold**. The baseline's startup-open samples are distinct
from warm reads but are also not filesystem-cold.

## Observed projection and batching evidence

The tables below round nanoseconds to microseconds, except where marked ms.
Raw JSON retains every execution-order sample, nearest-rank p50/p95, actual SQL,
plans, per-sample counters and the separate session-first observations.

First newest-first page, limit 100; read excludes serialization:

| Items | Projection | Read p50/p95 (µs) | Serialize p50/p95 (µs) | Response bytes | Data SELECTs | Primary / criteria callbacks | VM / fullscan / sorts |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | Summary | 96.666 / 103.583 | 55.334 / 57.875 | 13,939 | 1 | 100 / 0 | 1,415 / 99 / 0 |
| 100 | Work | 341.667 / 369.584 | 103.041 / 113.667 | 53,133 | 2 | 100 / 339 | 4,461 / 99 / 0 |
| 100 | Audit | 406.541 / 424.416 | 202.875 / 212.459 | 86,972 | 2 | 100 / 339 | 6,063 / 99 / 0 |
| 10,000 | Summary | 96.666 / 101.625 | 54.250 / 59.625 | 15,019 | 1 | 101 / 0 | 1,428 / 100 / 0 |
| 10,000 | Work | 343.958 / 352.792 | 103.584 / 112.792 | 52,727 | 2 | 101 / 328 | 4,410 / 100 / 0 |
| 10,000 | Audit | 403.708 / 435.750 | 196.625 / 208.625 | 87,206 | 2 | 101 / 328 | 6,027 / 100 / 0 |
| 100,000 | Summary | 95.125 / 102.458 | 53.458 / 58.959 | 15,367 | 1 | 101 / 0 | 1,428 / 100 / 0 |
| 100,000 | Work | 318.875 / 333.167 | 100.542 / 108.959 | 49,326 | 2 | 101 / 303 | 4,261 / 100 / 0 |
| 100,000 | Audit | 398.167 / 433.375 | 201.041 / 219.875 | 84,160 | 2 | 101 / 303 | 5,878 / 100 / 0 |

Limits 1 and 10 and ready/active/mine cases are also published. Summary used
one primary SELECT; work/audit and nonempty legacy pages used two data SELECTs,
not the historical list baseline's 201 / 20,001 / 200,001 statements.
Primary callbacks were at most limit + 1 for projection pages, with batched
criteria callbacks reported separately. Fullscan 100 here describes the
bounded sequential index steps of a 101-row page, not 100,000 visited items.

At 100k, current legacy first-list limit 100 emitted 83,364 dynamic-result JSON
bytes, used 2 SELECTs, 5,404,880 VM steps, 99,999 fullscan steps and 1 sort;
read p50/p95 was 153.411 / 157.918 ms. This is final canonical legacy SQL,
not the earlier incompatible raw-column ordering. V1 result JSON has no
transport envelope; v2 bytes include its typed success envelope, newline and
real cursor encoding. Bytes therefore show different projection/delivery
shapes, not equivalent-format compression or model token counts.

## Deep pages, filters and history

Newest-first at 90% depth, limit 10; exact legacy offset IDs equalled cursor IDs:

| Items | Summary cursor read p50/p95 (µs) | Summary VM / fullscan / sorts | Legacy offset read p50/p95 (ms) | Legacy VM / fullscan / sorts |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 42.750 / 45.208 | 306 / 0 / 0 | 0.182 / 0.186 | 6,027 / 99 / 1 |
| 10,000 | 40.958 / 42.083 | 341 / 0 / 0 | 26.200 / 27.778 | 531,548 / 9,999 / 1 |
| 100,000 | 40.750 / 41.292 | 343 / 0 / 0 | 302.122 / 653.367 | 5,310,578 / 99,999 / 1 |

Summary and legacy return the same IDs, not the same fields. The fairer
full-field audit deep page at 100k used 2 SELECTs, 11 primary / 36 criteria
callbacks, 851 VM steps and zero fullscan/sorts; read p50/p95 was
86.250 / 88.875 µs. Work was 67.958 / 70.750 µs, 674 VM steps.

Important counterexamples at 100k:

| Case | Read p50/p95 (ms) | Data SELECTs | Primary / criteria callbacks | VM / fullscan / sorts |
| --- | ---: | ---: | ---: | ---: |
| Sparse project summary | 0.438 / 0.446 | 1 | 101 / 0 | 48,215 / 11,087 / 100 |
| Literal-text summary | 132.848 / 169.581 | 1 | 101 / 0 | 4,258,963 / 99,995 / 0 |
| Ready/P4 next first work | 7.987 / 8.479 | 2 | 11 / 26 | 48,625 / 12,048 / 0 |
| Ready/P4 next 90%-deep work | 2.401 / 2.615 | 2 | 11 / 27 | 81,891 / 0 / 0 |
| Legacy Ready/P4 matching offset | 16.213 / 16.649 | 2 | 11 / 27 | 294,157 / 15,745 / 3,215 |

The same-priority deep v2 case grows from 9,093 VM steps at 10k to 81,891 at
100k even with zero fullscan steps. First/deep timings are not monotonic:
they use different planner paths, but neither proves constant partition work.
Literal-text summary grows from 4,250 / 428,133 / 4,258,963 VM steps over the
three sizes. Its correlated criteria predicate is inside the primary SELECT,
not per-item hydration. Bounded returned rows do not bound predicate work.

At 100k, history first/after-10 pages read at 28.416 / 30.625 and
30.208 / 30.875 µs p50/p95, respectively. Both used 2 data SELECTs (existence
check plus bounded ordered events), 12 primary callbacks (one existence result
plus 11 events), 295 / 356 VM steps, and zero fullscan/sorts.
Response sizes were 4,727 / 4,830 bytes with actual history cursor encoding.

## Startup, durable writes, footprint and allocations

These are the original safe harness reruns, not the projection selection timer.
Each sampled 20 fresh-snapshot startup opens and 20 WAL/FULL durable note
mutations. Reopen verified expected revisions, exactly 20 benchmark-note
events, integrity `ok` and zero foreign-key violations at each scale.

| Items | DB bytes before / after writes | Startup p50/p95 (µs) | Note p50/p95 (µs) |
| ---: | ---: | ---: | ---: |
| 100 | 495,616 / 512,000 | 489.458 / 702.542 | 223.541 / 398.500 |
| 10,000 | 34,037,760 / 34,058,240 | 457.916 / 503.709 | 192.917 / 301.375 |
| 100,000 | 344,993,792 / 345,010,176 | 518.625 / 639.584 | 176.500 / 772.750 |

All read snapshots had 4,096-byte pages and zero freelist pages; allocated and
occupied page bytes equal the “before” column. Harness WAL was present/zero
before writes and missing after checkpoint/close. These snapshots do not
isolate index footprint/write amplification; Phase B's no/two/four-index
experiment remains the separate causal evidence.

Existing release allocation diagnostics reproduced: for a synthetic 98,920-byte
audit record, typed buffering made 1 allocation / 98,921 bytes versus v1
dynamic serialization's 141 allocations / 205,194 cumulative bytes.
Counting an oversized 2,654,824-byte record made 2 allocations / 24 bytes
(largest 16). These are preassembled serializer tests, **not** whole-read
allocations, sampled allocation percentiles, or peak resident memory.
The page-omission allocation assertion and all existing serializer cases passed.

## Follow-up decisions

1. **Accept the measured batching/projection gate.** One primary SELECT plus
   optional batched criteria, bounded page assembly, real cursor delivery and
   preserved logical fixtures are supported at all required sizes.
2. **Keep canonical legacy semantics; do not advertise bounded scan work.**
   Final canonical item-ID segment/numeric ordering visibly sorts and scans.
   Consider an expression-index or canonical stored-key design only as a
   separate measured compatibility/footprint/write tradeoff.
3. **Track a same-priority `next` seek follow-up.** A tuple/partition seek design
   should be tested against current filters/order before claiming depth
   independence. No unmeasured migration was added to hide this limitation.
4. **Defer search-specific indexing to a workload-driven design.** Sparse
   project and substring text still need residual filtering. Decide supported
   search semantics before introducing FTS or changed substring behavior.
5. **Do not infer causal historical latency speedups.** Projection reduction,
   batching, ordering, indexes and serialization changed together; only current
   same-session cases support the comparisons above.

## V2-019 negative evidence

The parent reported running:

```sh
cargo test --locked --test benchmark_harness \
  report_uses_snapshot_and_contains_review_evidence -- --exact
```

After changing the expected list SELECT counts to `[2, 2]`, the test against
original storage failed with actual `[201, 201]`. This is narrow evidence that
the bounded-list regression gate detects the original per-item loader/fallback.
The expectation update belongs to the parent; other workers' tests are not
edited here. This report did not capture that negative raw log. Its focused
green counterpart passed independently in release. Do not treat the negative
report as a scan-work or timing result.

## Environment and interpretation limits

Observed environment: Apple M4, model Mac16,10, 10 logical CPUs, 17,179,869,184
memory bytes (16 GiB), APFS internal SSD, macOS 27.0.1 build 26A434,
Darwin 27.0.0 arm64, Rust/Cargo 1.94.0, bundled SQLite 3.51.1.
Raw commands are in [`environment.json`](baselines/v2-phase-c/environment.json).
The release run started at Unix time 1791139737 and ended at 1791139816.
Source HEAD was `e5ad32da48e291fe606644671585e1138e036ad7`, **dirty**:
HEAD alone does not identify the implementation.
[`source-sha256.json`](baselines/v2-phase-c/source-sha256.json) pins all 31
implementation/migration/dependency/harness inputs, verified unchanged during
measurement and at initial artifact publication. One post-run rustfmt wrap
changed only the test harness hash; the other 30 production/support hashes
remain exact. [`formatting-provenance.json`](baselines/v2-phase-c/formatting-provenance.json)
records both harness hashes and the exact wrapping-only diff. Reversing that
single wrap reconstructed the measured SHA-256; formatted release support tests
and rustfmt/diff checks passed. No redundant fixture generation was done for
this nonsemantic change, and raw measured provenance was not rewritten.
Full original dirty status is retained in start/end provenance.
The generated schemas match each other; this does
not establish pre-B physical schema equality. Current production indexes are
present, while all three
original logical digests match. See [`schema.json`](baselines/v2-phase-c/schema.json).
Pre-B used Darwin 25.6.0 arm64: **no cross-artifact latency ratio or causal
speedup attribution is supported**.

The following remain explicitly unmeasured:

- Whole-read allocation/peak-memory data (serializer test counts are separate).
- Filesystem-cold data: no legitimate cache eviction was performed.
- Concurrency, crash/power-loss durability, repeated machines, randomized
  order, confidence intervals, thermal/power controls, CLI-process overhead,
  model tokens, host cache behavior and workflow completion.

Response bytes are not tokens. SQLite row callbacks are emitted rows, not
rows visited by the VM; fullscan counters do not count every indexed scan.
Both VM and fullscan evidence are needed. The measurement trace callback
itself has overhead, so these are instrumented observations, not untraced
production latency promises.
