# BIF v2 Phase B implementation evidence

This is implementation evidence, not the Phase C V2-020 read-path comparison
or a release approval. The pre-Phase-B baseline remains
[`baselines/v2-pre-phase-b/README.md`](baselines/v2-pre-phase-b/README.md).
No active ledger is used by these tests or measurements.

## Typed serializer allocation checks (V2-013)

Measured on 2026-10-03 UTC with Rust `1.94.0 (4a4ef493e 2026-03-02)`,
Darwin `27.0.0 arm64`, Apple M4, 16 GiB RAM, in the default debug/test profile.
These are thread-local allocation counts for synthetic in-memory domain
fixtures, not disk, SQLite, elapsed-time, model-token, or completion measurements.
Fixture construction and projection conversion occur outside the allocation
window; both paths write to `io::sink()`.

Reproduce:

```sh
cargo test --locked --test v2_response_serialization \
  --test v2_response_allocations -- --nocapture
```

All 15 serialization tests and 3 allocation tests passed in the integrated
checkout. The serialization tests cover exact frozen fields/order, explicit
nulls, all canonical enums, Unicode/escaping, exact unsigned integers, empty
results, complete-record budgets, varying cursor overhead, last-emitted-row
continuation, oversized first records, and callback/output errors.

The allocation checks measure calls and cumulative requested allocation bytes,
not peak resident memory or allocator overhead:

| Case | Encoded JSON bytes | Allocation calls | Requested bytes | Largest allocation |
| --- | ---: | ---: | ---: | ---: |
| Typed v2 audit get | 98,920 | 1 | 98,921 | 98,921 |
| V1 dynamic get-result serialization | Not separately measured | 141 | 205,194 | 65,536 |
| Oversized typed audit get (counting rejection) | 2,654,824 required | 2 | 24 | 16 |

The audit fixture has a 65,536-character description, populated criteria, and
all-null provenance. The typed path includes a complete buffered v2 success
envelope and newline; the legacy comparison creates the v1 dynamic get-result tree and
serializes it directly without protocol framing or a response buffer. These
are different delivery paths, not byte-identical encodings. The results
demonstrate removal of the dynamic tree and bounded output allocation; they
are not a latency ratio, whole-read allocation comparison, or promised
percentage improvement.

The oversized fixture contains repeated multibyte and escaped characters.
An independent post-measurement oracle confirms the exact required JSON size.
A separate omission test checks that counting a large next record does not
allocate its encoded copy when only the first complete record fits.

Provenance: base commit `45ed7fcc85d5e0f9b2385a4a2198269154ce832c` plus the
uncommitted Phase B changes. The measured serializer/test snapshots have SHA-256:

```text
de37d4894ac2f8d66f2f66000f8d5f6772e04f1f2cfd112d81ac4e20ceb9f252  src/v2_response.rs
ee859180e294878309778320f73eaf6336aa5c4594ffd878049fea841d342e64  tests/v2_response_allocations.rs
d16e29bb0be43e3361d6fa7d52e6ea1534fa2aa2661c73cc497d503f72a6f950  tests/v2_response_serialization.rs
```

Independent serializer review completed without actionable findings and reran
all 18 focused tests successfully. If these sources change, rerun rather than
attributing the recorded numbers to the changed implementation.

## Storage and index evidence

The production adapter's twelve focused tests passed in release profile after
the exact persisted-identity correction, which passed independent final code
review. At requested limits 1/10/100, summary pages use one
primary data statement; nonempty work/audit pages use that statement plus one
batched criteria statement. Empty pages skip child loading. Primary output is limited to
`limit + 1`, and the sentinel is removed before child hydration. A deterministic
writer interleaving test verifies one snapshot across primary and criteria reads.
Audit never queries history. These are statement/returned-row and assembly
guarantees, not a bound on SQLite scan work.

### Index experiment and selected migration (V2-010)

Raw plans, counters, timing samples, source hashes, and provenance are in
[`baselines/v2-phase-b-indexes.json`](baselines/v2-phase-b-indexes.json).
Measurements were rerun on 2026-10-04 UTC after the persisted-identity fix
(05:19:06–05:19:52 UTC including fixture generation/build), on the Apple M4 /
16 GiB / Darwin 27.0.0 arm64 machine, on APFS, with Rust 1.94.0 and bundled
SQLite 3.51.1 in release profile. The macOS product version was 27.0.1
(build 26A434); Cargo was 1.94.0 (85eff7c80 2026-01-15). This SQLite version is
distinct from the system Python SQLite 3.54.0 used for the runbook rehearsal.

The newly generated 10k-item, seed-2003 fixture has logical digest
`957657e192519763` and store identity `benchmark-00000000000007d3-10000`. The
experiment opens it read-only, verifies its generated identity/digest, uses
SQLite online backup into exclusively claimed disposable copies, and verifies
the source digest again afterward. Every candidate must return exactly the
same pages as the no-read-index copy.

The fresh source is
`target/bif-phase-b-evidence/fixture-identity-fixed-20261004T051906Z.sqlite`;
historical fixtures and the active ledger were not used or changed. Full raw
generator and experiment output is retained locally in
`target/bif-phase-b-evidence/generator-20261004T051906Z.log` and
`target/bif-phase-b-evidence/experiment-20261004T051906Z.log`. Machine/toolchain
and filesystem observations are in the corresponding `metadata-` and
`filesystem-` logs; the twelve-test output is in `storage-tests-20261004T051906Z.log`.
These `target/` artifacts are disposable local files, not committed evidence.
The JSON preserves all 147 read samples and 90 durable write samples.

The measured source snapshot is base commit
`45ed7fcc85d5e0f9b2385a4a2198269154ce832c` plus uncommitted Phase B changes:

```text
c862f127e583010af9c377ec143444e2382089ff5275aff6312d13c56faac186  src/application/read_semantics.rs
c434de2667d4a7fdb46a3e7c5be10c0710359a4667cddf44217a029b9a78e681  src/storage/projection_reads.rs
19e9e1a854550db08d446b149f1250904701e9a9136eb01dd10581d43e14c29a  tests/v2_index_evidence.rs
18e0648e7f4beed109743b2de3a0dfb6bd733ca1fbce593b8a8da0eb9032d3d5  migrations/0003_projection_read_indexes.sql
2172fe64d42f0a2c678c0c69dbeea33eaad8242ec434520a949a93c29bea39d1  Cargo.lock
```

Reproduce with a new output basename; the generator refuses existing outputs:

```sh
mkdir -p target/bif-phase-b-evidence
cargo run --release --locked --bin bif-benchmark-store -- \
  10000 --seed 2003 --output target/bif-phase-b-evidence/fixture-new.sqlite
BIF_INDEX_FIXTURE="$PWD/target/bif-phase-b-evidence/fixture-new.sqlite" \
  cargo test --release --locked --test v2_index_evidence -- --ignored --nocapture
```

The ignored experiment explains and executes the actual production query,
not a reconstructed SQL approximation. It compares no read indexes, list/ready
indexes only, and all four indexes from migration 0003. Only disposable copies
drop indexes; migration history is not rewritten. Each copy runs `ANALYZE`.
Prepared-statement caching is disabled. Seven consecutive session reads per
case include the first execution; no OS cache eviction is attempted. Startup,
backup, migration, authorization and serialization are outside read timing.
This is **not** the old v1 N+1 baseline or the V2-020 end-to-end comparison.

| Summary shape, limit 100 | No-index VM steps | Two-index VM steps | Four-index VM steps | Four-index plan |
| --- | ---: | ---: | ---: | --- |
| All/list | 210,924 | 1,428 | 1,428 | Ordered list index scan |
| Ready/next | 63,747 | 1,529 | 1,529 | Ordered ready expression index scan |
| Active/list | 98,230 | 2,776 | 1,428 | Ordered active partial index scan |
| Mine/list | 118,029 | 6,805 | 1,531 | Mine partial index search by assignee |
| Blocked/list | 50,685 | 5,279 | 5,279 | List index scan with residual status filter |
| Sparse literal text | 418,320 | 428,133 | 428,133 | List index scan plus correlated criteria search |
| Ready/null-priority boundary | 105,704 | 21,724 | 21,724 | Ready index scan with residual boundary |

All 21 query plans and work-counter records match the earlier trial; the timing
samples below are new measurements of the corrected adapter, not relabelled
pre-correction samples.

| Summary shape, limit 100 | No-index median read (ms) | Two-index median read (ms) | Four-index median read (ms) |
| --- | ---: | ---: | ---: |
| All/list | 7.012834 | 0.085583 | 0.082500 |
| Ready/next | 0.714208 | 0.088750 | 0.087459 |
| Active/list | 2.514542 | 0.110333 | 0.083333 |
| Mine/list | 1.993750 | 0.163167 | 0.089542 |
| Blocked/list | 1.055667 | 0.181708 | 0.186417 |
| Sparse literal text | 10.414625 | 11.287041 | 11.383750 |
| Ready/null-priority boundary | 0.852333 | 0.176917 | 0.177250 |

All cases use one data statement plus `BEGIN`/`COMMIT`: three total traced
statements. SQLite VM counters include those transaction statements. Primary
row callbacks are 101 (including lookahead), except sparse text, which returns
11. The four common shapes have no temporary ORDER BY sort with the selected
indexes. Separate plan tests cover summary/work/audit, before and after
`ANALYZE`, using plan properties rather than exact plan snapshots.

**Decision:** ship four indexes. The two-index alternative already removes
sorting, but active and mine partial indexes further reduce deterministic VM
work for the named workloads. No extra index is added for the sparse-text case
or the residual boundary case.

| Index set | Occupied SQLite page bytes | Allocated main-database bytes | Median durable write (ms) |
| --- | ---: | ---: | ---: |
| None | 32,030,720 | 34,037,760 | 0.2894375 |
| List + ready | 32,636,928 | 34,037,760 | 0.3021045 |
| All four | 33,169,408 | 34,037,760 | 0.3282495 |

Occupied bytes are `(page_count - freelist_count) * page_size`, not logical
payload size. At 4,096 bytes/page, the allocated size corresponds to 8,310
pages in every variant; the occupied sizes correspond to 7,820 / 7,968 / 8,098
pages, leaving 490 / 342 / 212 free pages respectively. These values are
measured before the write trials. Copies are first opened by the current binary,
then experimental indexes are dropped/recreated; freed pages remain allocated. Therefore the
physical file difference is not the total index footprint. The selected set
adds 1,138,688 occupied bytes (about 3.55%) over no read indexes; active/mine add
532,480 over the two-index alternative.

This fresh source was generated with migrations 0001–0003 already applied,
unlike the earlier schema-2 fixture. Allocated bytes therefore differ from the
earlier 33,116,160 (none/two) and 33,169,408 (four), while occupied bytes match.
The logical digest remains identical; physical layout and freelist reuse are
not part of that digest. The larger allocated size is not evidence of a
storage-footprint cost from the identity decoder check.

Write samples are 30 normal, authorized, idempotent priority/assignee/note
triages on one ready item per copy, with fresh keys and revision preconditions,
using production mutation persistence, WAL and synchronous FULL. All advance
revision once, create the expected note events, and pass integrity/FK checks.
This narrow sequential sample is noisy and includes outliers; its medians are
observations, **not** a stable percentage penalty or a release performance gate.
It does not cover every lifecycle/capture workload or WAL-growth behavior.
The APFS data volume reported 99% capacity and about 3.3 GB free during this
run; this is another reason not to generalize the write latencies.

### Known limitations and handoff

- Sparse literal substring search still scans all 10k items; it retains v1's
  meaning and does not gain FTS/LIKE semantics. In this trial its indexed VM
  work and latency were worse than the unindexed bounded query.
- The null-priority continuation still traverses earlier ready rows:
  1,732 fullscan steps versus 100 for the first ready page. The blocked filter
  records 1,012 steps. Neither is claimed to have constant deep-page scan work.
- Deep/cold/warm comparisons on the original full fixture matrix, broader
  allocation/write/WAL measurements, and any follow-up index/boundary decisions
  belong to V2-020. No model-token, MCP-host, or codec advantage is measured here.
- The V2-054 [upgrade runbook](bif-upgrade-runbook.md) is approved as an initial
  prerequisite, not as production upgrade/restore or release approval.
- Phase C remains unimplemented: no opaque cursor codec, paginated history
  storage, v1 bounded-loader routing, or v2 CLI opt-in has shipped in this work.

## Integrated verification

`cargo test --locked --no-fail-fast` passed on the corrected snapshot:
**217 passed, zero failed, one ignored**. The independent final code reviewer
also reproduced that result, and formatting and diff checks passed.
The evidence refresh independently ran
`cargo test --release --locked --test v2_projection_storage` (twelve passed) and
the ignored index experiment explicitly in release profile (one passed).
JSON schema/sample counts, recorded source hashes, occupied-byte arithmetic,
document medians, and diff checks passed. Existing v1 fixtures and migrations
0001/0002 remain unchanged. Clippy is unavailable in the pinned installed
toolchain; no components were installed.

The application/serialization/storage/runbook units and index-migration
checks passed earlier independent review. Final code review approved the
identity correction and independently exercised 78 additional identity-error
reads, including CHECK/FK-enabled NUL, tab, punctuation, and Unicode separators.
That probe verified rollback on get errors and repaired reads and writes on
the same connection afterward.

The corruption-handling gap found during final review is now addressed:
selected persisted requester/project columns must equal their canonical
`RequesterId`/`ProjectId` values exactly, in addition to matching the canonical
`item_id`. Mixed-case or whitespace-normalized identities are rejected rather
than emitting a continuation key different from the coordinates SQL compares.
Three test-first regressions cover get and both page orderings across all
projections, constraint-enabled `BOB ` repeat-boundary rejection, and malformed
lookahead identities. A malformed sentinel still only establishes `has_more`;
it is removed before projection validation and child hydration, then rejected
if continuation selects it as an emitted row. Error paths release the snapshot.

The measurements above now identify and exercise the corrected source snapshot.
Independent evidence review approved the refreshed artifacts and reproduced the
release experiment on another freshly generated 10k/seed-2003 fixture. All 21
plans, complete deterministic counters, and allocated/occupied footprints
matched. Recorded hashes, sample counts, medians, and footprint arithmetic were
also verified; elapsed timings are observations, not reproducible constants.
Work stops after Phase B; Phase C remains deferred.
