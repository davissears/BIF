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
SQLite generation uses a random basename that cannot equal the final database,
WAL, or SHM basename inside a private staging directory in the output
directory. Immediately before opening SQLite, it compares the staged pathname
with the retained create-new file handle. The bundled SQLite Unix VFS then opens
the staging path with `SQLITE_OPEN_NOFOLLOW`, rejecting a symbolic pathname
component. The generator fails before claiming outputs on non-Unix targets and
does not claim Windows reparse-point protection. Same-file checks are supported
on Unix filesystems that provide stable device/inode identity through
`stat`/`fstat`; other filesystems receive only best-effort identity checks and
are unsupported.

The identity check and SQLite's ambient-path open cannot be atomic. This Phase A
developer tool supports accidental concurrency and no-clobber behavior for
public outputs, but assumes no same-user actor changes its random private
staging directory or entry while it runs. A regular-file or hard-link
replacement in the check/open interval can redirect SQLite. Eliminating that
residual risk requires a handle-bound custom SQLite VFS and is outside Phase A;
the generator does not promise safety against arbitrary hostile or concurrent
private-staging mutation.

After checkpointing, verification, and closing that connection, the generator
checks the staged identity again and publishes from the retained handle. It
copies through the already claimed final file handle and syncs it; metadata is
then written and synced through its claimed handle. Before reporting success,
the generator verifies that the staging directory, staged entry, and both final
pathnames still identify their claimed open handles. A replaced staged entry
cannot substitute publication bytes. Under the private-staging trust assumption
above, SQLite is therefore never asked to open the requested final basename and
never inspects, creates, truncates, or removes adjacent final-name `-wal` or
`-shm` paths. Existing database, metadata, WAL, and SHM paths are never adopted
or replaced by the generator outside the documented check/open residual risk.
For an explicit output, failure cleanup deliberately does not unlink either
claimed final name:
portable filesystems cannot conditionally unlink a pathname only if it still
identifies a particular open file, so unlinking could delete a third-party
replacement. A failed explicit invocation can consequently leave an
invocation-owned empty or partial database and/or metadata claim for manual
removal.

The generator never recursively removes staging or omitted-output directories.
Path-based `remove_dir_all` can target a pathname replacement, while `cap-std`
documents that even its `remove_open_dir_all` operation is not guaranteed
atomic with a concurrent rename. `cap-tempfile` uses that operation and also
deliberately exposes no ambient path for SQLite's default VFS, so it cannot
provide both required guarantees here. The generator uses `tempfile` only for
collision-safe staging allocation, immediately disarms its pathname-based
destructor, and retains a `cap-std` directory solely to claim the staged entry
relative to the directory capability and perform the pre-open and completion
same-file checks.

Every run can leave a clearly named owned `.bif-benchmark-stage-*` directory.
After success, the staged main database is truncated through the exact open
file handle used for publication, reducing that orphan without reopening or
unlinking a path. A failure can leave a partial or complete staged database.
Omitted-output failures additionally leave their owned `bif-benchmark-*` outer
directory, claimed database, and metadata files. These orphans require manual
removal after ensuring no concurrent generator still owns them; preserving them
is the portable replacement-safe policy.

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

Each startup sample and the measured run atomically allocate a
`bif-measurement-*` directory and claim `store.sqlite3` with create-new
semantics. The harness never recursively removes those remembered pathnames:
Drop truncates the snapshot only through the exact file handle retained since
creation, then leaves the directory for the platform's external temporary-file
cleanup. A normal SQLite close removes or truncates its transient WAL/SHM state,
so completed runs leave an empty main database and usually no meaningful
sidecar data. If SQLite cannot clean a sidecar, or the process exits before
Drop, a sidecar or full snapshot can remain. The harness does not unlink those
paths because a concurrent rename and replacement cannot be distinguished
portably from the object originally created.

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

The source fixture is opened read-only for metadata validation and as the source
of SQLite online backups; the harness never copies DB, WAL, or SHM files. A
trustworthy run requires a disposable source whose logical state and generator
metadata remain immutable for the entire harness invocation: do not run a
writer, replace the database or sidecar, or otherwise reuse the fixture
concurrently. Each online backup captures committed active-WAL content
consistently, but the startup samples and final measured snapshot are separate
backups and can observe different committed states if that precondition is
violated.

Before any SQLite connection opens the canonical source, the harness checks its
canonical `-wal` and `-shm` companion names. WAL without SHM is an unsupported
source state and fails closed: ordinary read-only SQLite opens can create the
missing public SHM path. The preflight only observes those paths and does not
create or remove companions. DB-only sources with neither companion remain
supported, as do active-WAL sources where both companions exist. The check and
SQLite open cannot be atomic; the immutable-source requirement above excludes a
companion change in that interval. A hostile or concurrent path mutation could
therefore bypass or spuriously trigger the check and is outside Phase A.

The online backups do not themselves change the logical or durable source
database or its WAL contents. SQLite may, however, update transient read-mark
and coordination bytes in an existing `-shm` file while a reader uses an active
WAL. That SHM activity is neither a durability-setting change nor a logical
fixture mutation: SHM is a reconstructible coordination index, while committed
content remains in the database and WAL. Tests therefore require source DB/WAL
bytes and metadata and the generator sidecar to remain unchanged, require the
SHM path to be neither created nor removed unexpectedly, permit its transient
contents or metadata to change, and independently compare the source's logical
digest and content before and after measurement.

For a provenance-bound fixture run, before taking startup samples the harness
validates the generator sidecar's format, algorithm, database path, item count,
row counts, distributions, recorded `integrity_check: "ok"` status, and digest
against the source; stale sidecars are rejected. If no sidecar exists beside the
resolved source pathname, the harness permits an unbound store and records no
fixture seed or digest. The binding step validates the recorded integrity status
but does not independently recompute `integrity_check`.

Startup snapshots are not individually digest-validated. In a provenance-bound
run, after all startup samples, the final snapshot used for measured operations
is the only snapshot whose independently computed logical digest is checked
against the generator sidecar. An independent integrity and foreign-key check
runs later on that measured snapshot as part of persistence verification. These
checks detect a mismatched final snapshot, but do not prove that the source
stayed unchanged or that startup and measured snapshots came from one source
state. Every startup sample and the measured operations use an atomically owned
disposable directory and exact retained database handle with the orphan policy
described above. Sizes identify the disposable measured database and
distinguish before/after writes as well as a missing WAL from a present
zero-byte WAL.

Source path canonicalization resolves symlinks. When the database argument is a
file symlink, the harness snapshots the resolved target and discovers generator
provenance only at `<resolved-database>.metadata.json`; the sidecar's recorded
database path must resolve to that same pathname. A distinct
`<lexical-alias>.metadata.json` is rejected as ambiguous even if its contents
happen to match, rather than silently choosing between provenance records.
The WAL-without-SHM preflight likewise derives both companion names from the
resolved target; lexical-alias companions do not substitute for canonical
companions and are not touched.
Report output validation protects the database, WAL, SHM, and metadata companion
names for both the resolved target and the exact lexical alias used by the
invocation.

This is pathname canonicalization, not complete filesystem-object identity:
`canonicalize` cannot discover other hard-link names. A generated database
reached through a hard-link alias therefore does not discover the sidecar beside
the generated pathname and lacks its seed/digest provenance; such aliases are
unsupported for provenance-bound benchmark runs. Output validation also cannot
protect unenumerated hard-link names or companion paths derived from them. Do
not invoke the source through a hard-link alias or place a report at a companion
path derived from any database alias; invoke the generated database pathname
directly, or use a symlink when an alias is required. The harness does not impose
a brittle inode link-count restriction. Output parents must still exist. Before
publication, the harness writes the complete report to a `NamedTempFile` in the
destination directory and syncs that file. `persist_noclobber` never overwrites
an existing destination, but publication atomicity depends on the platform and
filesystem. Its fallback may create the destination as a hard link and then
unlink the original temporary name; interruption or failure to unlink that name
can leave an extra owned temporary link. The harness does not sync the parent
directory, so report publication makes no crash-durability claim.

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
