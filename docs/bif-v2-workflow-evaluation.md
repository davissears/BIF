# BIF host workflow evaluation

**Status:** Phase A fixture for V2-005. It defines trials; it does not implement
v2 reads or claim a token reduction.

The normative fixture is
[`fixtures/bif-v2-workflow-evaluation.json`](fixtures/bif-v2-workflow-evaluation.json)
and its shape is described by
[`fixtures/bif-v2-workflow-evaluation.schema.json`](fixtures/bif-v2-workflow-evaluation.schema.json).
All task content is synthetic and non-private.
The V2-006 dry-run summary is included in the
[v2 pre-Phase-B baseline](baselines/v2-pre-phase-b/README.md).

## Baselines and variants

Every workflow has two recorded v1 scenario baselines:

- `v1_full_json` uses the existing complete `--json` representation for reads.
  V1 mutations have no JSON output option, so mutation calls record their real
  production human response while retaining the read representation that drove
  the decision.
- `v1_compact_human` uses the current compact human queue rendering. Human
  output is a separate baseline, not a v2 projection approximation.

A `v2_contract` variant appears only for `get`, `list`, `next`, or `history`
shapes frozen by V2-001. It is marked `not_implemented`; fixture response
content is contract-derived expected content, not runtime evidence. Workflows
that require mutations mark the mutation call `not_implemented` because the
frozen v2 contract specifies reads only. Refresh measurements are
`not_measured` because Phase A has no live cursor implementation.

Each workflow declares an initial store, ordered calls, a stopping condition,
and machine-checkable success assertions. The agent-handoff outcome is a
read-derived artifact containing the item ID, revision, and next acceptance
criterion; BIF has no handoff mutation, and this fixture claims none. Item IDs
and revisions are records in the deterministic V2-003
100-item fixture (seed 2003, logical digest `73701e6b7b09a5e1`); no database
binary is checked in. Reset by regenerating that store before each variant.
For executable CLI trials, generate it as `<root>/.bif/bif.sqlite`, pass
`--root <root> --requester BENCH`, and preserve each request envelope's explicit
`--project` filter. The checked-in v1 read responses were captured with exactly
that configuration; omitting the project filter can resolve the repository's
unrelated project and legitimately return an empty queue.

## Accounting boundary

A call accounts separately for the host-provided tool schema, model-produced
request envelope, host-injected response envelope, response content, and any
model-produced follow-up. `proxy_utf8_bytes` is the UTF-8 length of those exact
fixture strings and `total_proxy_utf8_bytes` is their sum. These values are
transport-size proxies only: **they are not tokens**.

Prompt, completion, cached, and uncached tokens use tagged measurements. They
are `unknown` until a real host reports them; an unavailable duration or
correctness observation is `not_measured`, never zero. Cache semantics,
tokenizer, model, host, and provenance are likewise explicit.

The checked-in scenario dry run has provenance `local_fixture_validation`. It
proves that every ordered call and envelope component is represented,
proxy-byte arithmetic is exact, v1 reads match the production CLI on a freshly
generated store, every declared guarded mutation executes from the declared
revisions, and supported variants respect V2-001. Failure injection and retry
transcripts are not claimed: the deterministic local store cannot honestly
record transport failures such as `storage_busy` or `invalid_cursor`.
`error_retry_specifications` are normative test plans, separate from the dry-run
calls. Each freezes the trigger, request shape, error assertions, retryability,
policy, and next-request behavior, while requiring
`execution_state: not_executed` and empty `attempts` and `evidence`. Those plans
meet the workflow error/retry specification requirement without presenting
synthetic failures as observations. Every workflow also has a structurally
valid, independently checkable expected outcome. It does not prove
model correctness, latency, token counts, cache behavior, or runtime v2
support.

## Approved real-measurement process

1. Copy the fixture to an ignored results location such as
   `target/bif-evaluations/<run-id>.json`; never replace the normative fixture
   with host observations.
2. Generate the V2-003 store locally with the documented seed and verify its
   logical digest. Reset to the declared initial state before each variant.
3. Configure user-provided host/model credentials only through that host's
   environment or credential store outside this repository. Never put keys,
   session material, private prompts, or raw private responses in Git.
4. Record the exact host/version, model identifier, tokenizer and
   version/config, cache accounting semantics, trial timestamp, store digest,
   command/build revision, and whether each component was host-observed or
   locally derived.
5. Run cold-cache and warm-cache trials separately. Preserve every tool schema,
   request, host response envelope/content, follow-up, error, and retry in
   order. Do not infer cached tokens by subtraction unless the host explicitly
   defines that method.
6. Record host-reported token fields and completion time verbatim. Use
   `unknown` when the host omits a value and `not_measured` when the trial did
   not measure it. Never substitute byte proxies or estimates.
7. Evaluate the fixture assertions against the final store/result and record
   task correctness independently from transport success. Keep failures and
   retries; do not report only successful attempts.
8. Validate the result locally with the focused fixture test before sharing a
   redacted artifact. Sending data to a model service requires the user's
   normal host authorization and is intentionally outside this repository's
   automated tests.

Phase B runtime reads, MCP, codecs, and performance claims remain out of scope.
