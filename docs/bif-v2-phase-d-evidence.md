# Phase D read-release evidence

**Decision: local evidence recorded; release NOT approved.** V2-021..026 have
implementation, local tests and generated-store measurements. V2-027 remains
blocked because **no real configured MCP host has performed the required
list/get/history/restart workflow**. The [release manifest](bif-v2-release-evidence.json)
is the machine-readable gate. Neither protocol subprocess tests, this custom
benchmark client nor green CI count as that host smoke.

No model-token evidence is available; no token-saving claim is made.
Synchronization, mutation MCP, persisted generation, codecs and live-store
rollout are outside this release.

## Source and measurement identity

Measurements used the production source after the parent protocol fix was
available: Git base `c5605ce8611ece07f0c64e7ff09cd97eec1b0407`, **dirty**, plus
the exact parent's `src/mcp.rs` change to typed optional error IDs. Uncorrelated
errors omit `id`; malformed envelopes retain only valid IDs; overflow fallback
validates its ID. The final parent `src/mcp/tools.rs` list/history descriptions
also explicitly state that continuation is live pagination, not a snapshot.
After that last metadata edit, release binaries, all three timing artifacts
and the executable rehearsal were rebuilt/rerun. No control, queue, SQLite or
application semantics were changed by either catch-up. Documentation was also
uncommitted.

This is **not a clean final Git revision claim**. The artifacts identify the
actual measured source and executables. Documentation and later test-only edits
do not retroactively change those measurements; any later production edit needs
a new measurement or explicit disclosure.

- [Build provenance](baselines/v2-phase-d/build-provenance.json) records
  release build commands, Rust/Cargo, bundled SQLite probe, candidate and
  archived old source hashes, old archive hash and binary hashes.
- [100-item artifact](baselines/v2-phase-d/mcp-100.json),
  [10,000-item artifact](baselines/v2-phase-d/mcp-10000.json) and
  [100,000-item artifact](baselines/v2-phase-d/mcp-100000.json) contain machine
  metadata, source revision/dirty status/porcelain, production input SHA-256
  (all Rust sources, Cargo inputs, toolchain and migration SQL), unchanged
  start/end source hashes, harness hash, invocation and raw samples.
- All three runs used identical production inputs and executables.
  `src/mcp.rs` SHA-256:
  `3b0085ac130239603eb73c7ab1918abd72888b5da3487875295635fff56d6a4a`.

| Executable | SHA-256 |
| --- | --- |
| `bif` | `863139b0a60ac97088b1b7c939cf234d035c3b63bafdbca7f6a1f62f2919fcc3` |
| `bif-mcp` | `783768bed7c71cac1c623472384c34028ee49ba959ab4ece322ae25ca1e1da98` |
| `bif-benchmark-store` | `fa59b068553c5846e60e982e14c9fc8c9748179e56ae6114d6e290265a2408a6` |

## Reproduction and safety

The [measurement harness](baselines/v2-phase-d/measure.py) creates a new private
root beneath the attached checkout's `target/phase-d-measurements` for each run.
It has no existing-ledger argument, strips `BIF_ROOT`, `BIF_CONFIG` and
`BIF_REQUESTER`, and uses explicit disposable configuration for every process.
The [rehearsal harness](baselines/v2-phase-d/rehearse.py) likewise creates only
new roots beneath `target/phase-d-rehearsals`. It never overwrites an active
store or down-migrates. No live config was modified and no user ledger was
initialized. Generated temporary stores are removed on normal completion;
published JSON preserves the observations, not restorable database backups.

```sh
cargo build --locked --release --bins
for size in 100 10000 100000; do
  PYTHONDONTWRITEBYTECODE=1 python3 docs/baselines/v2-phase-d/measure.py \
    --items "$size" --seed 2003 --requests 1000 --startups 20 \
    --sqlite-version 3.51.1 > "docs/baselines/v2-phase-d/mcp-$size.json"
done
```

`--sqlite-version` is recorded metadata, not runtime discovery by the Python
client. Here it was independently obtained from the production Rust
`bif-benchmark` provenance on a fresh 100-item fixture (one sample); that probe
is embedded in build provenance. That version probe preceded the catalog-only
description edit; Cargo/SQLite inputs remained unchanged. Future runs must
verify their own bundled version instead of blindly copying this argument.

### Environment and workload

Apple M4, 10 reported CPUs, 16 GiB memory, arm64 macOS 27.0.1, APFS;
Rust 1.94.0 (`4a4ef493e`), optimized release profile; bundled production SQLite
3.51.1; Python 3.9.6 with SQLite 3.54.0 for integrity/checkpoint/backup.
The filesystem report records 96% capacity used at measurement time.
No OS-cache eviction or machine-idleness guarantee was attempted.

| Initial items | Seed | Logical fixture digest |
| ---: | ---: | --- |
| 100 | 2003 | `b819125481255ec7` |
| 10,000 | 2003 | `957657e192519763` |
| 100,000 | 2003 | `87a07c0632977092` |

Each fixture run contains:

1. **20 server startups**, each through initialize and first 20-item core list;
   and 20 separate one-shot v2 CLI 20-item core lists.
2. A separate initialized warm process, discovery, one-item list/cursor and
   work get/validator setup before timing.
3. **1,000 sequential mixed warm calls**: 250 each list (20 summaries),
   conditional work-get **hit**, history (limit 20 for one retained item) and
   selected-work. One request is in flight at a time.
4. **20 separate CLI capture processes**, sequentially launched by one writer
   thread overlapping the read loop—not 20 simultaneously active writers.
   Every command succeeds; 20 durable rows are counted, and the warm server
   observes the captured tasks. The thread may finish after the read loop.
5. RSS and WAL file-size samples at setup/every 100 calls; writer join,
   integrity and truncate-checkpoint; cursor continuation before/after process
   restart and validator hit after restart.

Raw startup/CLI latency, per-tool warm latency and complete wire bytes, and CLI
capture latency are retained. Latency uses Python monotonic `perf_counter_ns`,
from request send through receipt of a complete line (before Python JSON
decoding), including local transport/server scheduling/encoding. CLI wall time
includes process completion. The harness's `p95` is sorted zero-based index
`floor(0.95 * n)` capped at `n - 1`; at 20 samples it is the **maximum**, not
the Phase C nearest-rank convention. Raw arrays allow recomputation.

## Timings: startup is not warm latency

Startup includes launch, initialization and the first list. CLI includes one
launch/read/exit. They are separate workflows, not interchangeable throughput
measurements.

| Items | MCP startup median / p95, ms | One-shot CLI list median / p95, ms |
| ---: | ---: | ---: |
| 100 | 3.236 / 172.359 | 3.761 / 10.524 |
| 10,000 | 3.069 / 3.890 | 3.402 / 3.748 |
| 100,000 | 3.155 / 3.661 | 3.400 / 3.924 |

The 100-item startup maximum was 172.359 ms; it is retained, not discarded.
Its cause was not isolated. These 20-sample distributions are insufficient
for a tail-latency guarantee or startup-speedup claim.

Warm values below are **median / p95 microseconds**, 250 samples per cell.

| Items | List | Conditional get hit | History | Selected work |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 129.375 / 180.416 | 62.209 / 87.083 | 84.563 / 111.875 | 84.709 / 118.583 |
| 10,000 | 134.709 / 185.375 | 64.688 / 83.417 | 123.917 / 154.375 | 89.063 / 114.375 |
| 100,000 | 133.396 / 188.500 | 66.563 / 89.292 | 106.751 / 139.291 | 87.854 / 116.542 |

The warm measurements support reuse on these generated bounded queries. They
are not universal constant-work pagination, cold-storage performance, concurrent
reader throughput or sustained-write capacity evidence. In particular, get is
a retained-validator hit, **not** a modified/full-work fetch. Different fixture
history/selected content sizes explain different work; no equal-size assumption
is made.

## Wire, memory, WAL and restart observations

Discovery exposes four read tools. Input schemas total **2,057 compact UTF-8
bytes**. The complete `tools/list` JSON-RPC line is **3,103 bytes**, including
metadata/descriptions/annotations and newline. These are not token counts.

Median complete output-frame sizes include text plus structured content, JSON-RPC
envelope and newline:

| Items | List bytes | Get-hit bytes | History bytes | Selected-work bytes |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 7,415 | 557 | 2,041 | 1,703 |
| 10,000 | 7,598 | 569 | 7,279 | 1,153 |
| 100,000 | 7,779 | 577 | 4,623 | 1,153 |

The largest measured tool frame was 7,933 bytes. This workload does not exercise
the maximum-size frame; deterministic payload/framing tests cover the bounds.
Wire size varies with content, concurrent captures and numeric request-ID length.

| Items | Sampled server RSS range, KiB | WAL before truncate, bytes | WAL after truncate |
| ---: | ---: | ---: | ---: |
| 100 | 4,144–5,296 | 1,672,752 | 0 |
| 10,000 | 4,176–5,456 | 1,882,872 | 0 |
| 100,000 | 4,160–5,488 | 1,989,992 | 0 |

All runs returned checkpoint `[0, 0, 0]` while the warm server remained alive,
integrity `ok`, 20 durable CLI captures, identical cursor continuation across
restart and a `not_modified` validator hit after restart. This demonstrates no
lingering snapshot blocked that checkpoint in this finite workload; it does
not prove a memory peak bound or indefinitely bounded WAL. RSS grew modestly
through the run; sampling every 100 calls cannot rule out intermediate peaks.
No busy error occurred in this workload; no contention stress limit is claimed.

## Disposable historical-binary upgrade and rollback

[Old-binary rehearsal JSON](baselines/v2-phase-d/old-binary-rehearsal.json)
contains actual CLI get/history output before upgrade and after new/old
mutations, migration checksums, store ID, table counts, integrity/foreign-key
checks, source/binary hashes, machine metadata and the old refusal diagnostic.
The existing runbook identified the historical schema-2 baseline:
`45ed7fcc85d5e0f9b2385a4a2198269154ce832c`.

**Approval limitation:** this was built from archived historical source. It is
not the operator's retained approved production executable; no such revision
was supplied. It adds genuine executable compatibility evidence but cannot
stand in for actual production-binary/configuration inventory or live rollout
approval.

```sh
# Run only in the attached checkout; extraction/build remain under target/.
mkdir -p target/phase-d-old-source
git archive 45ed7fcc85d5e0f9b2385a4a2198269154ce832c |
  tar -x -C target/phase-d-old-source
cargo build --locked --release \
  --manifest-path target/phase-d-old-source/Cargo.toml \
  --bin bif --bin bif-benchmark-store --target-dir target/phase-d-old-build
PYTHONDONTWRITEBYTECODE=1 python3 docs/baselines/v2-phase-d/rehearse.py \
  --old-binaries target/phase-d-old-build/release \
  --old-revision 45ed7fcc85d5e0f9b2385a4a2198269154ce832c \
  > docs/baselines/v2-phase-d/old-binary-rehearsal.json
```

Actual execution used `git archive` into a scratch tar before extraction; its
SHA-256 was `d28caa11a91db0f26ba1998b56f40d7fd1ddc33eb835519f88da633614147580`.
The old `bif` executable SHA-256 was
`828157f672951331e80d029e56fde74c80a0cb8cfd35ace78e687c52dc2fd247`;
build provenance retains the old fixture-builder hash and archived inputs too.

The old builder generated 100 seed-2003 items at schema 2. The old CLI captured
one probe, producing baseline 101 items / 339 criteria / 101 provenance /
517 operations / 517 events / 1 receipt. Python online backup created a pristine
schema-2 copy and then a separate candidate. New production initialization
applied only migration 3; store ID, baseline counts, get and history matched.
Candidate approval at revision 1 persisted revision 2/ready across CLI reopen;
old capture replay succeeded. Operations/events/receipts became 518/518/2.

The old executable rejected schema 3 with
`database schema version 3 is newer than supported version 2`.
The pristine backup still matched baseline. Restoring that backup into a new
root reopened with the old binary and matched baseline get/history/inventory.
Old capture replay succeeded, and a new old-binary revision-checked approval
persisted ready/revision 2 across old get/history reopening.

The restore deliberately discarded the candidate-only approval; it did not
reverse the upgraded store. Read-release rollback is recovery with potential
later-write loss, not transparent undo. The deterministic
[rehearsal test](../tests/read_release_rehearsal.rs) additionally compares all
application table rows and pre-existing schema before/after the indexed upgrade
and pristine restore. The executable harness checks inventories and the probe's
CLI content; it is not a complete byte-for-byte database comparison.

## Verification and remaining operator gates

Executed successfully on this recorded snapshot:

- `cargo build --locked --release --bins`
- `cargo fmt --all --check`
- `cargo clippy --locked --all-targets` (existing warnings remain, including
  MCP test style warnings; no warning-free claim)
- `cargo test --locked` (all nonignored tests passed)
- `cargo test --locked --test read_release_manifest --test read_release_rehearsal`

Local coverage includes pinned lifecycle/version negotiation, strict framing
and arguments, cancellation/duplicate IDs and later reads, disconnect, overload/
backpressure, complete record budgets, explicit project interleaving, identity
invalidation, cursor restart, conditional existence/authorization/binding,
revision-only hits, and no leaked transaction after error. The test suite and
CI registration are the sources of truth, not a duplicate fixed test inventory.

**Still required before release:** an operator configures a real MCP host
supporting `2025-11-25` using the chosen executable and explicit disposable
config, records sanitized host name/version/configuration and transcript
evidence for list/get/history/restart, and reviews compatibility, measurements
and the [candidate runbook](bif-upgrade-runbook.md#phase-d-indexed-read-candidate).
The manifest describes the host artifact format. Do not mark custom harness
output as a host transcript or auto-approve from CI.

For an actual store, separately inventory/rehearse the retained production old
binary and launch config, stop all readers/writers, verify a SQLite-consistent
backup, obtain explicit upgrade/restore authorization and accept/reconcile any
later-write loss. Restore requires unconditional MCP restart and invalidation
of retained validators/cursors: persisted restore-generation detection remains
Phase E work.
