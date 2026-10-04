# Phase C comparison artifacts

This directory contains the compact raw V2-020 evidence described in
[`../../bif-v2-phase-c-evidence.md`](../../bif-v2-phase-c-evidence.md).
**All three required release comparisons completed** on the final integrated
dirty snapshot: 100 / 10,000 / 100,000 items, seed 2003, 20 warm samples per
case plus separately recorded session-first observations. Original pre-B
logical digests matched; source and metadata stayed unchanged.

These artifacts describe PR #3's implementation (merge `474d826`). Later
correctness fixes can change the recorded inputs; the hash verification below
is for that historical snapshot, not an assertion that newer `main` is identical.
Do not rewrite the preserved hashes or attribute these timings to changed code
without rerunning the comparison.

| Artifact | Contents |
| --- | --- |
| `generator-{100,10000,100000}.json` | Actual generator metadata: seed, logical digest, row counts/distributions, generation duration and integrity |
| `measurement-{100,10000,100000}.json` | Complete original safe baseline-harness reruns: startup, reads, bytes, SQL work, durable writes and reopen verification |
| `read-comparison-{100,10000,100000}.json` | Actual production projection/history comparisons, real cursor bytes, raw timings, per-sample work, plans and legacy offset equivalence |
| `provenance-start.json`, `provenance-end.json` | Full source commit/dirty status, toolchain/SQLite/OS, SHA-256, executable argv and controls |
| `source-sha256.json` | Convenient path-to-hash map of the 31 measured production/migration/dependency/harness files |
| `formatting-provenance.json` | Measured vs final test hash for one post-run rustfmt line wrap; all other 30 inputs remain exact |
| `environment.json`, `schema.json` | Observed hardware/OS/APFS/build commands and actual generated source schema catalogs |
| `verification.json` | Executed commands/results and genuine existing synthetic release allocation diagnostics |
| `preparation.json` | Preparation record updated to measured status; remaining unknowns explicitly retained |

Raw JSON is compact, not shortened: all execution-order samples and session-first
results remain. Historical baseline artifacts/tests were not changed.
Whole-read allocations, filesystem-cold behavior, host/model tokens and cache
telemetry are unavailable; serializer tests do not fill those gaps.

## Reproduction

The ignored integration measurement creates fresh 100 / 10,000 / 100,000-item
seed-2003 inputs itself, in a newly owned directory beneath
`target/bif-phase-c-measurements/`. It accepts no existing database path and
never opens the active ledger. It leaves its disposable directory for explicit
cleanup rather than recursively deleting a path that might have been replaced.
Allow several GiB of free space for the three inputs, their read snapshots,
baseline temporary snapshots, and build output.

Use a separate release target directory; no debug measurement is needed:

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
  --test v2_response_serialization --test v2_response_allocations -- --nocapture
```

The last command prints the exclusively created artifact directory. Default
sample count is 20 warm samples per case, plus a separately recorded first read
per case; the baseline rerun uses 20 samples. For an initial small-input check:

```sh
BIF_READ_COMPARISON_SIZES=100 BIF_READ_COMPARISON_SAMPLES=2 \
  CARGO_TARGET_DIR=target/phase-c-evidence \
  cargo test --release --locked --test v2_read_comparison \
  publish_phase_c_read_comparison -- --exact --ignored --nocapture
```

The only permitted sizes are `100,10000,100000`; the seed is fixed. The first
read is not filesystem-cold: generation, backup, digest checks, plans, page
equivalence checks, and boundary traversal have already accessed the fixture.
There is no cache eviction or `ANALYZE`.

The test publishes compact JSON with `create_new` and `sync_all`. It refuses
existing destinations and asserts unchanged source hashes during each run.
An interrupted write can leave a partial new artifact; a passing test and
parseable files are prerequisites for publication. This is not atomic or
crash-durable report publication. The original baseline harness retains its
own stronger safe-source/output handling unchanged except the intentional
bounded-list statement expectation.

For each successful rerun, retain these files in a **new** artifact directory;
do not overwrite this measured snapshot:

- `measurement-{100,10000,100000}.json`: complete original harness reruns,
  including startup, statement/row/VM/fullscan counters, read/serialization
  samples, response bytes, DB/WAL sizes, durable note latency, and reopen
  integrity/revision/event verification.
- `read-comparison-{100,10000,100000}.json`: generator metadata, pinned
  pre-B logical digest comparison, real v2 cursor/envelope bytes, production
  query plans, all projection/deep/sparse/history samples and work counters,
  source commit/status/SHA-256 and environment.
- `provenance-start.json` and `provenance-end.json`: source hashes, dirty
  status, revision, toolchain, SQLite, OS, exact test argv and environment
  controls. Capture CPU/RAM, OS product version and filesystem separately
  when actually observed; `uname` alone does not establish them.

This run's disposable directory was
`target/bif-phase-c-measurements/bif-owned-test-directory-88173-0`, about
760 MB including the source/read DBs and companion files. It was preserved
for parent-coordinated safe cleanup, not recursively deleted. Build output
is separately under `target/phase-c-evidence/`; roughly 12 GiB remained after
the run. No fixture database is published here.

For a rerun, set `RUN` to the exact printed directory and `DEST` to a new
artifact directory, then inspect complete files before copying JSON only:

```sh
RUN=target/bif-phase-c-measurements/bif-owned-test-directory-PID-SEQUENCE
DEST=docs/baselines/v2-phase-c-rerun
mkdir "$DEST"
for n in 100 10000 100000; do
  jq -c . "$RUN/$n.sqlite3.metadata.json" > "$DEST/generator-$n.json"
  jq -c . "$RUN/measurement-$n.json" > "$DEST/measurement-$n.json"
  jq -c . "$RUN/read-comparison-$n.json" > "$DEST/read-comparison-$n.json"
done
jq -c . "$RUN/provenance-start.json" > "$DEST/provenance-start.json"
jq -c . "$RUN/provenance-end.json" > "$DEST/provenance-end.json"
```

Publication is a review step, not part of the ignored test. Do not label a
small-input check as completion of the three-scale comparison. Historical
pre-B artifacts are immutable. Do not form latency ratios against them across
changed OS/machine/cache procedures.

## Verify the measured implementation after integration

HEAD is provenance, not a clean-release identity: this was a dirty snapshot.
Compare the recorded file hashes after merging/formatting. A changed production
input requires review and potentially a rerun, not rewriting old provenance:
the only accepted exception here is the explicitly verified test-only line
wrap in `formatting-provenance.json`. The original measured manifest is intact.

```sh
python3 - <<'PY'
import hashlib, json, pathlib
manifest = json.loads(pathlib.Path(
    "docs/baselines/v2-phase-c/source-sha256.json").read_text())
formatting = json.loads(pathlib.Path(
    "docs/baselines/v2-phase-c/formatting-provenance.json").read_text())
assert manifest["files"][formatting["path"]] == formatting["measured_sha256"]
for path, expected in manifest["files"].items():
    if path == formatting["path"]:
        expected = formatting["formatted_sha256"]
    actual = hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
    assert actual == expected, f"measured source changed: {path}"
print("All 30 production/support hashes and documented formatted test hash match")
PY
```
