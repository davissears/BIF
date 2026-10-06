# Operator review: configured BIF MCP candidate

**Technical result: passed. Operator decision: pending.**

Review [the recorded host evidence](host-evidence.json),
[the full transcript and raw capture references](transcript.json),
[the logical inventory comparison](inventory-verification.json), and
[the upgrade runbook](../../../bif-upgrade-runbook.md#phase-d-indexed-read-candidate).
The release manifest is `awaiting_operator_review`, with `approved=false`
and `approval=null`. No live-store authorization exists.

## Exact candidate and observed result

- Source revision: `b97cd89f26838b577763d5f6f30d18afc873c669`.
- `bif-mcp` SHA-256: `783768bed7c71cac1c623472384c34028ee49ba959ab4ece322ae25ca1e1da98`.
- Host: Codex desktop **26.930.51102**, build **13100**; its MCP client reports **0.160.0**.
- Protocol: the application offered `2025-06-18`; the server selected
  `2025-11-25`, which the application accepted before discovery and tool calls.
- **34 checks passed, zero failed** for the required final workflow assertions.
  All **26 model-visible tool calls** match raw requests/responses on the
  configured application transport. All **24 sessions**, including discovery
  probes and unsupported optional-resource errors, are retained.
- List covered `core` and `agent-tools` with bounded pagination and request
  independence. Get covered summary/work/audit, matching validators,
  cross-projection misses, and recovery after application/protocol errors.
  History covered two event pages. Selected-work matched the first ready item
  under next ordering without a claim or assignment.
- The application stopped server PID **55242** (SIGTERM, exit -15, no relay
  errors or stderr) and launched **58504**. Retained list/history continuations
  matched exactly, and the old matching validator returned `not_modified`.
- Schema plus all rows in **12 tables** were unchanged during reads/restart.
  A separate guarded CLI change on synthetic `BENCH:core:026` moved priority
  P1 to P4 and revision 4 to 5. Only that item and its one event/operation/receipt
  changed. Through Codex, the old validator missed and the new validator hit.

The launcher is a transparent stdio recorder, not an MCP client. Its exact
hash/configuration are recorded; this validates the configured instrumented
launch, not a bare direct-binary launch. The final workflow server remains
active on the disposable root. Only the original server's shutdown is claimed.
Two CLI setup assumptions failed before any mutation and are disclosed in the
transcript; the corrected changed-item workflow passed. No runtime or protocol
implementation was changed to obtain these results.

## Runbook review

The runbook explicitly identifies these operator obligations and limitations:

- Ordinary candidate reads and doctor can migrate a store. Production schema-2
  to schema-3 opening needs a stopped maintenance window and separate approval
  for the exact binary, schema, root, verified backup, and launch configuration.
- Old schema-2 binaries reject schema 3. Take a SQLite-consistent online backup,
  retain it pristine, rehearse in another root, and retain the actual approved
  production executable/configuration. The existing historical `45ed7fc`
  rehearsal does not satisfy that production inventory requirement.
- Stop all readers/writers and persistent MCP processes for upgrade/restore;
  restart them through their configured launchers. Restore into a separate root
  and explicitly accept or reconcile writes lost after the backup. No in-place
  downmigration is supported.
- Until Phase E adds persisted restore-generation detection, invalidate every
  retained cursor and validator after restore, even if store ID/revision match.
- The cursor format is unchanged but permits up to 1 MiB encoded data. A prior
  binary with a 16 KiB ceiling cannot safely resume larger continuations.
- Project arguments are query scope, not ACLs. This release exposes only four
  read tools. It grants no claim, assignment, mutation bridge, synchronization,
  newer MCP protocol support, or measured model-token reduction.

These requirements are explicit in the runbook. The configured-host correctness
check closes the recorded application evidence gap for this candidate/version;
production inventory, backup rehearsal and authorization remain live-maintenance
responsibilities.

## Requested signoff

An operator may accept this candidate's configured-host correctness evidence
and the runbook compatibility review, or hold it for specified changes. Record
operator identity, decision time, candidate hash, evidence/runbook hashes,
decision, scope and restrictions only after the operator supplies a decision.
A passing CI run, artifact presence or agent review is not that decision.

**Scope of this signoff:** read-candidate evidence and compatibility review only.
It is separate from permission to open, upgrade, replace or mutate any live store.

Reviewed artifact hashes:

- Host evidence SHA-256: `cef2fcdcff7f7eb4002f948513693f2f7215b2115050c499dad9d5f624c13d60`.
- Upgrade runbook SHA-256: `518853704b5236f7f198bffaa33d4d1fb5268434c33c271c0967d925ea52df3e`.
