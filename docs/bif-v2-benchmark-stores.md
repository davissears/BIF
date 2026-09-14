# BIF v2 benchmark stores

Phase A's benchmark inputs are deterministic logical stores, not checked-in
database binaries. The fixture builder supports exactly 100, 10,000, and
100,000 items. It uses valid lifecycle paths and audit events while deliberately
including skewed projects, every priority plus null, sparse text markers, long
descriptions, zero-to-many criteria, equal timestamp buckets, substantial
histories, and mutation bursts on a stable hot set.

The V2-006 release run and raw artifacts are published in the
[v2 pre-Phase-B baseline](baselines/v2-pre-phase-b/README.md).

Generate any fixture with the same developer command:

```console
cargo run --release --locked --bin bif-benchmark-store -- 100 --seed 2003 --output target/bif-bench/100.sqlite3
cargo run --release --locked --bin bif-benchmark-store -- 10000 --seed 2003 --output target/bif-bench/10000.sqlite3
cargo run --release --locked --bin bif-benchmark-store -- 100000 --seed 2003 --output target/bif-bench/100000.sqlite3
```

Omit `--output` to allocate a collision-safe directory under the system
temporary directory. Existing outputs are never replaced. Each run writes
`<database>.metadata.json` and prints the same machine-readable record,
including the seed, logical digest, row counts, status/priority/project/assignee
distributions, elapsed time, integrity result, and canonically loaded samples.

The main database and metadata path are claimed with atomic create-new opens.
SQLite generation happens at a separate basename inside an invocation-owned
private staging directory in the output directory. After checkpointing,
verification, and closing that connection, the completed database is copied
through the already claimed final file handle and synced; metadata is then
written and synced through its claimed handle. Before reporting success, the
generator verifies that both final pathnames still identify those claimed open
files. SQLite is therefore never asked
to open the requested final basename and never inspects, creates, truncates, or
removes adjacent final-name `-wal` or `-shm` paths. Existing database, metadata,
WAL, and SHM paths are never adopted or replaced. For an explicit output,
failure cleanup deliberately does not unlink either claimed final name:
portable filesystems cannot conditionally unlink a pathname only if it still
identifies a particular open file, so unlinking could delete a third-party
replacement. A failed explicit invocation can consequently leave an
invocation-owned empty or partial database and/or metadata claim for manual
removal. Omitted-output failures remain fully cleanable because they recursively
remove only their invocation-owned outer directory; private staging directories
are automatically removed in both modes.

The generator bulk-builds one transaction through fixture-only infrastructure
rather than weakening production durability or spending one durable transaction
per synthetic mutation. Its rows model supported capture, lifecycle, priority,
assignment, and note operations. It then uses production item and history
loaders for samples and runs `PRAGMA integrity_check`.

An identical size and seed has an identical `logical_digest`; SQLite store
identity is pinned as fixture data, while migration bookkeeping and physical
page/WAL details are intentionally excluded. Ordinary CI should run only the
100-item determinism test. Generate 10k and 100k stores explicitly for local
benchmarking.

## Measure a store

Build and run the measurement harness in release mode. Raw reports are JSON and
should normally be written below `target/` (machine-specific results are not
source artifacts). The report's parent directory must already exist; the
harness never creates output directories:

```console
cargo run --release --locked --bin bif-benchmark -- target/bif-bench/100.sqlite3 --samples 20 --output target/bif-bench/100.$(git rev-parse --short HEAD).json
cargo run --release --locked --bin bif-benchmark -- target/bif-bench/10000.sqlite3 --samples 3 --output target/bif-bench/10000.$(git rev-parse --short HEAD).json
cargo run --release --locked --bin bif-benchmark -- target/bif-bench/100000.sqlite3 --samples 1 --output target/bif-bench/100000.$(git rev-parse --short HEAD).json
```

The practical starting points are 20/3/1 samples for 100/10k/100k items. Increase
them only after measuring the local run cost; the harness rejects more than
1,000 samples. For a quick release smoke check, use `--samples 1`. The report
format is `bif-v2-measurement-v2`. It records anchored Git state, full Rust
target, SQLite version, host/OS, exact command, cache procedure, fixture
size/seed/digest, and every raw timing sample with p50 and p95.
Percentiles use nearest rank: sort $N$ values and select the one-based value at
rank $\lceil pN\rceil$.

The harness attaches SQLite `trace_v2` only to its developer process. It counts
statements classified as data statements separately from setup statements,
SQLite row callbacks, profile time, and available per-statement counters
(full-scan steps, sorts, automatic-index rows, VM steps, reprepares, runs, and
filter hits/misses). Aggregate totals and the complete raw counter record for
each sample are retained. Production connection setup remains `WAL`, foreign keys
on, and `synchronous=FULL`. The deterministic list-all case asserts current
hydration behavior: one selection query plus two load queries for each of $M$
matches, or `1 + 2M` per sample. No text-search or setup statement is included
in that assertion. SQL metrics, timing, and byte distributions all retain each
individual sample.

The source fixture is opened read-only solely as the source of a SQLite online
backup; the harness never copies DB, WAL, or SHM files. The online backup does
not change the logical or durable source database or its WAL contents, and it
captures committed active-WAL content consistently. SQLite may, however, update
transient read-mark and coordination bytes in an existing `-shm` file while a
reader uses an active WAL. That SHM activity is neither a durability-setting
change nor a logical fixture mutation: SHM is a reconstructible coordination
index, while committed content remains in the database and WAL. Tests therefore
require source DB/WAL bytes and metadata and the generator sidecar to remain
unchanged, require the SHM path to be neither created nor removed unexpectedly,
permit its transient contents or metadata to change, and independently compare
the source's canonical logical digest and content before and after measurement.

The backup's independently computed canonical digest must match the generator
sidecar before measurement. The sidecar's format, algorithm, database path, item
count, row counts, distributions, recorded `integrity_check: "ok"` status, and
digest are validated against the source; stale sidecars are rejected. This
binding step validates the recorded integrity status but does not independently
recompute `integrity_check`. An independent integrity and foreign-key check runs
later on the measured snapshot as part of persistence verification. Every
startup sample and the measured operations use an atomically owned disposable
directory; cleanup only removes that directory. Sizes identify the disposable
measured database and distinguish before/after writes as well as a missing WAL
from a present zero-byte WAL.

Source identity is canonical filesystem identity. When the database argument is
a file symlink, the harness snapshots the canonical target and discovers
generator provenance only at `<canonical-database>.metadata.json`; the
sidecar's recorded database path must resolve to that same canonical file. A
distinct `<lexical-alias>.metadata.json` is rejected as ambiguous even if its
contents happen to match, rather than silently choosing between provenance
records. Report output validation protects the database, WAL, SHM, and metadata
companion names for both the canonical target and the exact lexical alias used
by the invocation. Output parents must still exist, and publication remains an
atomic no-clobber operation.

`startup_open` measures opening, configuring, and migrating a fresh snapshot in
the harness process. `warm_connection_and_os_cache` reuses one connection and
the existing OS cache. Neither is described as filesystem-cold. True filesystem-cold
measurement is reported unavailable because cache eviction is platform
controlled. If a platform owner can safely evict caches, run that
platform-specific operation before each single-sample invocation and retain the
procedure alongside the raw JSON. Closing a connection or starting a process
does not establish cold-cache behavior.

Allocator-wide allocation counts are explicitly unavailable: BIF does not
install a counting global allocator, and the harness does not estimate them.
SQLite PROFILE time is retained per sample. The subtraction is named
`non_profiled_wall_residual_nanoseconds`: it includes all non-profiled work,
trace callbacks, and clock overhead and is not claimed to be pure assembly.
Samples where PROFILE exceeds wall time are explicitly indexed. Zero PROFILE
samples are also indexed because SQLite's PROFILE clock can round
short statements to zero; those values must not be interpreted as zero DB work.
Serialization uses the same production result projection functions as current
v1 CLI/RPC: get includes `{"item": ...}`, history includes `{"events": ...}`,
and list uses the application's default 100-item page and its real
`next_offset`. Measured bytes exclude the outer RPC transport envelope
(`protocol_version`, `request_id`, `ok`, and `result`); that exclusion is
recorded in every operation.

After writes, the harness closes and reopens the snapshot, loads the item and
history through production readers, checks expected revisions and note events,
and runs SQLite integrity and foreign-key checks. This is **persistence
verification**, not evidence of crash-proof durability.

### Compare revisions

1. At revision A, generate a store with the pinned seed, run the release
   harness, and retain both generator metadata and measurement JSON.
2. At revision B, regenerate the same size and seed, verify `logical_digest`
   matches, and run with the same sample count and host/cache state.
3. Compare like-named operation distributions, counters, response bytes, and
   store sizes. Retain raw files for V2-006 and record host/cache procedure
   separately. A dirty report is not revision-only evidence.

Timing thresholds deliberately stay out of ordinary CI. CI checks JSON shape
and the deterministic `1 + 2M` statement invariant on the 100-item fixture;
larger stores and timing interpretation remain explicit developer work.
