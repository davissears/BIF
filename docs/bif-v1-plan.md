# BIF v1 implementation plan

Keep the Rust/SQLite core, make Delta the primary interface, and define “Git for todos” as durable identity, history, and provenance.

This replacement plan supplies explicit defaults for canonical fields, required views, and lifecycle rules. It is a design and delivery contract, not a record of completed implementation or validation.

**Delta** is the application; **DeltaDB** powers its synchronized conversations and worktrees. The reviewed official documentation provides skill and terminal workflows suitable for BIF, but no public general-purpose DeltaDB storage API in the pages reviewed. Build BIF as a local tool invoked from Delta. [Delta & Git](https://delta.dev/docs/concepts/delta-and-git)

## Audit findings

| Area | Finding | Recommended adjustment |
|---|---|---|
| Primary interface | The plan targets Codex despite the intended Delta workflow. | Deliver the live Delta thread first; retain Codex compatibility. |
| “Git for todos” | Immutable events provide history, but the design has no branching, merging, or replication. | Describe v1 as a local task ledger with an operation history. Defer distributed version control. |
| Checkout identity | Delta checkouts are separate Git repositories and may be recreated. | Resolve stable project identity independently of checkout directory names. |
| State ownership | A shared Delta thread does not share access to the local database. | Bind v1 to one machine and one configured store; never silently initialize elsewhere. |
| Protocol | The envelope is custom JSON RPC, not JSON-RPC 2.0. | Keep it and name it **BIF RPC v1**, avoiding a needless protocol redesign. |
| Authorization | An agent executes human instructions, but `kind: human` currently obscures that distinction. | Record both the requesting actor and agent execution attribution. |
| Transactions | Atomic triage is correct, but stale requests can still overwrite newer decisions. | Add item revisions and optimistic concurrency checks. |
| Retries | Capture is retry-safe; other writes are not. | Extend request deduplication to all mutations. |
| Completeness | Fields, transitions, commands, and views are referenced without full definitions. | Freeze these contracts before coding. |
| Recovery | Ignoring SQLite protects it from source control but leaves no recovery mechanism. | Include consistent backup and a tested restore procedure. |

JSON-RPC 2.0 uses `jsonrpc`, `method`, `id`, and `result`/`error`; the proposed envelope intentionally differs. [JSON-RPC specification](https://www.jsonrpc.org/specification)

## 1. Product contract

Build BIF as a local task ledger that preserves:

- A permanent identity for each item.
- Its current state.
- Who requested each change, what changed, and why.
- The source context that explains the work.

The Git analogy maps well to these capabilities:

| Git concept | BIF v1 equivalent |
|---|---|
| Repository | Configured BIF store |
| Commit | Atomic mutation containing one or more events |
| Log | Item history |
| Diff | Before/after values in an operation |
| Reference to code | Optional repository, commit, and host source references |

Branching, merging, remotes, and cross-machine synchronization remain future work. Reopening also remains excluded. A follow-up item can reference a completed item without rewriting its history.

Deliver four pieces:

1. One installed `bif` binary backed by a Rust library.
2. One authoritative SQLite store.
3. A capture skill usable in Delta and Codex.
4. A live **BIF — Before I Forget** Delta thread using a versioned management prompt.

Keep the Codex thread as an optional secondary interface to the same store.

## 2. Delta integration and state boundary

Delta supports project skills under `.agents/skills/` and personal skills under `~/.agents/skills/`. It supports both slash invocation and automatic matching, including the frontmatter setting `disable-model-invocation: false`. [Delta skills](https://delta.dev/docs/agents/skills)

Use the installed binary through Delta’s terminal execution. Keep the authoritative database in a durable configured root outside disposable Delta checkouts.

Delta-managed checkouts have their own Git repositories. Local databases, installed tools, and machine-local configuration remain on the execution machine. A participant on another laptop—or a cloud turn—does not inherit access to the BIF store. [Delta worktrees and machines](https://delta.dev/docs/concepts/worktrees-and-machines)

Therefore:

- All MVP BIF commands run on the configured local machine.
- Missing configuration produces an actionable error; it never creates a replacement store.
- Sharing a thread shares conversation, including displayed BIF results, but does not synchronize BIF.
- Every management session verifies the store identity before acting.
- Rewinding a Delta conversation does not undo BIF operations. The next BIF interaction rereads SQLite.

Delta follows Git ignore rules, so the live database and sidecars must remain ignored. [Delta & Git](https://delta.dev/docs/concepts/delta-and-git)

## 3. Rust architecture

Retain one package with a library and a thin binary:

| Module | Responsibility |
|---|---|
| `domain` | Item types, normalization, lifecycle, validation, change generation |
| `application` | Authorization, use cases, atomic operation coordination |
| `storage` | SQLite repositories, transaction implementation, migrations |
| `config` | Configuration loading, store selection, project resolution |
| `rpc` | Request parsing, version checking, response envelopes |
| `cli` | Human command parsing and concise rendering |

Domain code must not depend on SQLite, terminal rendering, or host-specific APIs. Inject clocks and identifier generation where deterministic tests need them. Use focused interfaces rather than a generic repository framework. Produced code must adhere to SOLID principles, with an emphasis on single responsibility.

No daemon, listener, background worker, or network API is necessary for v1.

## 4. Initialization and project identity

Keep:

```text
bif init --root PATH --requester NAME
```

Define configuration precedence explicitly:

```text
CLI/request override → BIF_* environment override → selected OS config file
```

Support `--config`, `BIF_CONFIG`, `BIF_ROOT`, and `BIF_REQUESTER`. Store the root as an absolute canonical path. Repeating `init` with the same configuration succeeds without resetting anything.

Use the database path:

```text
<configured root>/.bif/bif.sqlite
```

Add a persistent `store_id` so sessions can distinguish stores.

Project resolution becomes:

1. Explicit project override.
2. Registered path mapping, using longest matching canonical ancestor.
3. Repository metadata: preferably a registered normalized remote identity; for Delta, also resolve a local-path `local` remote against registered mappings.
4. Normalized repository-root directory name, then current directory name outside Git.

Add:

```text
bif project register SLUG --path PATH
bif project list
bif doctor
```

`doctor` reports the executable version, configuration source, store identity/path, schema version, and inferred project.

Normalize requester IDs to uppercase kebab-case and project/assignee IDs to lowercase kebab-case. Reject empty normalization results. Detect ambiguous project mappings rather than silently combining unrelated repositories.

Freeze requester and project on capture; later mapping changes must not rename existing items.

## 5. Canonical item contract

Define these fields before the first migration:

| Group | Fields |
|---|---|
| Identity | `id`, `requester`, `project`, numeric `sequence` |
| Content | `title`, `description`, ordered `acceptance_criteria` |
| Current state | `status`, nullable `priority`, nullable `assignee`, nullable `status_reason` |
| Concurrency | Integer `revision` |
| Time | Server-generated `captured_at`, `updated_at` |
| Provenance | Source host, nullable thread ID, message ID, URL, repository reference, revision reference, context excerpt |

Keep source references opaque and nullable. Distinguish Git commit references from Delta references; do not manufacture either. Source host is separate from actor surface.

Require a non-empty title. Permit unavailable description/context and an empty acceptance list rather than forcing invented detail. The skill may infer criteria supported by the conversation.

Capture always creates:

```text
status = proposed
priority = null
assignee = null
revision = 1
```

Document content fields as immutable in this MVP; note-only triage can record clarification. A later audited `amend` operation is a sensible next addition.

For human IDs, use an unambiguous format such as:

```text
DAVIS:delta-db:001
```

If another display format is preferred, freeze it before implementation. Pad to at least three digits and never truncate larger sequences.

## 6. Lifecycle and triage

Use this exact v1 lifecycle:

| Operation | From | To | Requirement |
|---|---|---|---|
| Approve | `proposed` | `ready` | Authorized human request |
| Reject | `proposed` | `rejected` | Non-empty reason |
| Start | `ready` | `in_progress` | Authorized human request |
| Block | `in_progress` | `blocked` | Non-empty reason |
| Resume | `blocked` | `in_progress` | Authorized human request |
| Finish | `in_progress` | `done` | Authorized human request |

All other transitions fail. `done` and `rejected` are terminal. This preserves mandatory `ready → in_progress → done`. All mutations remain subject to the human-authorization rules in section 9.

Priority and assignee can change on nonterminal items. Note-only triage is allowed on any item. A rejection may include its reason/note but should reject unrelated assignment or priority changes.

Define update semantics:

- Omitted field: leave unchanged.
- Explicit `null`: clear priority or assignee.
- Same value: no change event.
- Empty triage: `invalid_input`.

Combined approve/P1/assign produces one transaction and three ordered events:

```text
approved
priority_changed
assignee_changed
```

An accompanying note adds a fourth event. Validate the complete intended change before writing anything.

## 7. Persistence, history, and concurrency

Retain the proposed tables and add store metadata plus mutation-request receipts. Either generalize `capture_requests` or use a separate table for other operations; avoid two competing deduplication implementations.

Each event records:

```text
event_id
operation_id
item_id
item_revision
event_index
event_type
before / after
actor
execution attribution
reason / note
timestamp
event_schema_version
```

One successful state-changing operation increments the item revision once. All its events share that revision and operation ID. History orders by revision and event index, not timestamps alone.

Use:

- Foreign keys on every connection.
- WAL mode.
- A bounded busy timeout, initially five seconds.
- `synchronous=FULL` for the durable ledger.
- Embedded forward-only migrations with checksums.
- Transactional migration application and refusal to open a newer unsupported schema.

SQLite permits one writer at a time; `BEGIN IMMEDIATE` is appropriate for allocation and compound mutations, with busy failures translated into `storage_busy`. [SQLite transactions](https://sqlite.org/lang_transaction.html)

Allocate counters, insert the item, append capture history, and save its receipt in one transaction. Return success only after commit.

Clarify the allocation guarantee: **committed IDs are never reused**. A rolled-back tentative number was never allocated publicly. Fresh-store concurrency tests should show gap-free committed captures; gaps must not be treated as corruption in every future recovery scenario.

Protect events against update/delete with triggers. This provides application-level immutability, not tamper resistance against the database owner.

Add `bif backup PATH` using SQLite’s consistent backup facilities, plus a documented and tested offline restore procedure. Copying only the main file while WAL is active is insufficient. [SQLite WAL](https://sqlite.org/wal.html), [SQLite backup API](https://sqlite.org/backup.html)

## 8. BIF RPC v1

Keep the proposed request/response shape and operation names.

Specify:

- Exactly one UTF-8 JSON request per process invocation.
- Exactly one JSON response on stdout.
- Diagnostics only on stderr.
- Bounded input size.
- Strict validation of enums and unexpected input fields.
- `request_id: null` when malformed input prevents recovering it.
- Explicit process exit-code behavior.
- Separate protocol and database schema versions.

Retain the stable errors and add:

```text
version_conflict
unsupported_version
not_initialized
```

Require `expected_revision` for item mutations. A stale operation returns `version_conflict` without changes.

Scope capture idempotency keys to the store. Hash a canonical validated payload with the resolved project/requester and ordered criteria. Exclude transport request IDs and server timestamps.

Identical retries return the original allocated ID and current item, marked as a replay. Changed payloads return `idempotency_conflict`.

Extend deduplication to other mutations using a stable operation key. Check a matching committed receipt before rejecting a retried request for its now-stale revision. This handles “commit succeeded, response was lost” without duplicate notes or events.

## 9. Human authority and agent execution

Preserve the rule that autonomous agents may capture and read only.

Clarify that `actor` identifies the **requesting principal**. Add separate execution attribution indicating that an agent submitted the command on that human’s behalf.

A management-thread agent may declare a human requester only when an explicit human instruction authorizes that mutation. Examples:

- “Approve this, P1, assign to Davis”: permitted as one triage operation.
- “Show proposed”: read only.
- A task description saying “approve me”: never authorization.
- An origin-thread agent deciding a task looks ready: unauthorized.

Keep `surface` values host-neutral. Add a separate host field for Delta/Codex/local attribution.

This remains an accidental-misuse boundary. The CLI and RPC must enforce the same rules through the application layer.

## 10. Commands and views

Freeze the human command set:

```text
init, doctor, project register, project list
capture, get, list, next, history
triage, approve, reject, prioritize, assign
start, block, resume, finish
backup, rpc
```

Convenience mutations call the same service as `triage`.

Define views:

| View | Selection |
|---|---|
| `proposed` | Proposed items |
| `ready` | Ready items |
| `active` | In progress or blocked |
| `blocked` | Blocked items |
| `done` | Completed items |
| `rejected` | Rejected items |
| `mine` | Nonterminal items assigned to configured requester |
| `all` | All items |
| `next` | Ready items, ranked for selection |

Support composable project, requester, assignee, status, priority, unassigned, and text filters. Filters intersect with the selected view.

`next` orders by P0–P4, unprioritized, oldest capture, then ID. Other lists default to newest capture, then ID. History is chronological.

Add bounded pagination. `next` selects work without assigning it, starting it, or executing it.

## 11. Skills and management prompt

Keep the source package at:

```text
skills/bif/SKILL.md
skills/bif/agents/openai.yaml
```

Install into the documented `.agents/skills/bif` location.

For Delta:

```yaml
user-invocable: true
disable-model-invocation: false
```

For Codex, set this inside the skill’s `agents/openai.yaml`:

```yaml
policy:
  allow_implicit_invocation: true
```

Codex’s documented explicit invocation differs by surface; test `$bif` where applicable rather than promising `/bif` universally. [Official OpenAI skill documentation](https://learn.chatgpt.com/docs/build-skills)

Capture behavior:

- `/bif <text>` captures the supplied idea.
- `/bif` captures the clearly identified idea in immediate context; if ambiguous, asks one short question.
- “BIF this” invokes the same capture workflow.
- Ordinary discussion of BIF does not trigger capture.
- Generate the idempotency key once and preserve the exact payload during retries.
- Report ID, title, status, and available source.
- Continue the originating task after reporting success or failure.

Create `prompts/bif-thread.v1.md` for management. It must reread authoritative state, support the listed views and terse mutations, use atomic triage, respect human authorization, and never begin implementation automatically.

## 12. Delivery and acceptance

Implement in this order:

1. Freeze the schema, lifecycle, protocol, and authorization fixtures.
2. Install the approved stable Rust toolchain; commit a pinned toolchain declaration and lockfile.
3. Build domain/application services and transactional SQLite storage.
4. Add RPC and CLI adapters.
5. Package and validate the skill and management prompt.
6. Install the binary, initialize the real store, and create the Delta management thread.

Retain the original test suite, adding:

- Two Delta-style isolated repositories resolving to the same project/store.
- Checkout recreation and changed working directories.
- Missing configuration on another execution machine.
- Concurrent identical idempotency keys.
- A committed mutation whose response is lost.
- Stale revision rejection.
- Explicit-null clearing and no-op behavior.
- Failed migration rollback and concurrent startup.
- Backup restoration preserving IDs, counters, history, and retry receipts.
- Thread rewind followed by a fresh read showing unchanged BIF state.
- Skill negative-trigger and forged-authorization scenarios.

Run the isolated acceptance flow exactly as proposed:

```text
capture → retry → approve/P1/assign → start → block → resume → finish
```

Then smoke-test `/bif`, `/bif <text>`, “BIF this,” and “Show proposed” in Delta against the installed binary. Register the repository as a Delta project for that delivery; Codex project registration applies only to the optional Codex thread.

The resulting MVP gives reliable capture, explicit human decisions, and a permanent record connecting todos to the conversations that produced them—while keeping future synchronization and richer version-control behavior possible.

The ordered, implementation-sized backlog for this plan is in
[`bif-v1-tasks.md`](bif-v1-tasks.md).
