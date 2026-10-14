# Invalid validation attempt — superseded observations

These observations are superseded as acceptance evidence because of two
validator gaps: incomplete required-table fixture digest coverage (including the
omitted requester/project allocator) and missing command-semantic validation.
This archive records the state before the fixture-digest remediation; it does
not claim either gap was already fixed or that a replacement run has occurred.

The 16 artifacts were copied byte-for-byte from the final-gate top level.
Embedded paths, hashes, results and the report's original claims are untouched
historical content, not current approval. `inventory.json` maps original
repository-relative paths to SHA256, sorted by path; it excludes itself and
this archival README. Neither `invalid-observer-attempt/` nor
`attempt-1-upgrade-proof.json` / `attempt-2-upgrade-proof.json` was changed.

The top-level frozen evidence intentionally remains stale and must fail harness
binding after remediation. No hashes, samples or validation results are refreshed
here; a later complete rerun must freeze a new protocol.
