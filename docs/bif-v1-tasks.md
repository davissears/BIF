# BIF v1 task backlog

This backlog decomposes the [BIF v1 implementation plan](bif-v1-plan.md) into
small, ordered tasks. Each task should produce one reviewable change and meet
its own acceptance check. A task may be split further during implementation,
but unrelated tasks should not be combined.

## Working agreement

- Complete tasks in numeric order unless every listed dependency is already
  satisfied.
- Keep tests for a behavior in the same change as that behavior.
- Treat the task's acceptance check as its definition of done.
- Do not include branching, merging, remotes, cross-machine synchronization,
  reopening, content amendment, a daemon, or a network API in this MVP.

## 1. Contracts and project skeleton

### BIF-001 — Record canonical enums and identifiers

**Scope:** Document the exact status, priority, event-type, actor-surface,
source-host, and error-code values, plus requester/project/assignee
normalization and item ID formatting.

**Acceptance:** One fixture or specification file contains every accepted
value and representative valid and invalid identifiers.

### BIF-002 — Record lifecycle fixtures

**Depends on:** BIF-001

**Scope:** Add a table-driven fixture for every allowed and rejected status
transition, including required reasons and terminal-state behavior.

**Acceptance:** The fixture covers the complete status cross-product and can
later be consumed by domain and adapter tests.

### BIF-003 — Record RPC fixtures

**Depends on:** BIF-001

**Scope:** Freeze the BIF RPC v1 request and response examples, version fields,
exit-code mapping, maximum input size, and unexpected-field behavior.

**Acceptance:** Fixtures cover one successful request and every stable error
envelope without requiring storage.

### BIF-004 — Record authorization fixtures

**Depends on:** BIF-001

**Scope:** Define allowed read/capture actions and human-authorized mutations,
including agent execution attribution and forged-authorization examples.

**Acceptance:** Each command category has at least one allowed and one denied
fixture.

### BIF-005 — Create the Rust package

**Scope:** Add one Rust package with a library, a thin `bif` binary, a pinned
stable toolchain, and a committed lockfile.

**Acceptance:** `cargo build --locked` and an empty library test suite pass.

### BIF-006 — Create module boundaries

**Depends on:** BIF-005

**Scope:** Add empty `domain`, `application`, `storage`, `config`, `rpc`, and
`cli` modules with dependency-direction documentation.

**Acceptance:** The crate compiles and domain code has no dependency on the
other five modules.

## 2. Domain model

### BIF-007 — Implement normalized IDs

**Depends on:** BIF-001, BIF-006

**Scope:** Implement requester, project, and assignee value objects and their
normalization rules.

**Acceptance:** Tests consume the valid and invalid identifier cases from
BIF-001.

### BIF-008 — Implement item identity formatting

**Depends on:** BIF-007

**Scope:** Implement sequence validation and the
`REQUESTER:project:NNN` display format without truncating larger sequences.

**Acceptance:** Tests cover zero rejection, three-digit padding, and values
above 999.

### BIF-009 — Implement canonical item types

**Depends on:** BIF-007, BIF-008

**Scope:** Add the item, provenance, status, priority, and immutable content
types with no persistence concerns.

**Acceptance:** Construction rejects an empty title and permits absent
description, context, references, and acceptance criteria.

### BIF-010 — Implement capture normalization

**Depends on:** BIF-009

**Scope:** Create a domain capture operation that sets `proposed`, null
priority/assignee, revision 1, and injected IDs/timestamps.

**Acceptance:** A deterministic unit test verifies every initial field.

### BIF-011 — Implement lifecycle transitions

**Depends on:** BIF-002, BIF-009

**Scope:** Implement approve, reject, start, block, resume, and finish domain
transitions.

**Acceptance:** Table-driven tests pass every BIF-002 lifecycle fixture.

### BIF-012 — Implement triage field changes

**Depends on:** BIF-009

**Scope:** Implement priority, assignee, and note changes, including omitted,
explicit-null, same-value, terminal-item, and rejection rules.

**Acceptance:** Tests distinguish clearing, no-op, invalid empty triage, and
note-only triage.

### BIF-013 — Generate ordered domain events

**Depends on:** BIF-011, BIF-012

**Scope:** Convert one validated mutation into ordered events with before/after
values; do not persist them.

**Acceptance:** Approve/P1/assign/note yields exactly four events in the
specified order and increments the item revision once.

## 3. Configuration and project resolution

### BIF-014 — Load configuration with precedence

**Depends on:** BIF-006, BIF-007

**Scope:** Load `--config`, `BIF_CONFIG`, `BIF_ROOT`, and `BIF_REQUESTER` using
the documented CLI → environment → file precedence.

**Acceptance:** Tests prove each precedence boundary and return
`not_initialized` when configuration is absent.

### BIF-015 — Canonicalize the store root

**Depends on:** BIF-014

**Scope:** Resolve the configured root to an absolute canonical path and derive
only `<root>/.bif/bif.sqlite`.

**Acceptance:** Equivalent relative and absolute roots resolve to the same
database path; no missing store is created.

### BIF-016 — Resolve registered project paths

**Depends on:** BIF-007, BIF-015

**Scope:** Resolve canonical working paths by longest registered ancestor and
reject ambiguous mappings.

**Acceptance:** Tests cover nested mappings, unrelated paths, and ambiguous
registrations.

### BIF-017 — Resolve Git project identity

**Depends on:** BIF-016

**Scope:** Add normalized remote matching and Delta `local` remote path
resolution.

**Acceptance:** Two isolated Delta-style repositories resolve to the same
registered project.

### BIF-018 — Add project fallback resolution

**Depends on:** BIF-017

**Scope:** Fall back to normalized repository-root name, then current-directory
name when outside Git.

**Acceptance:** Tests cover checkout recreation and changed working
directories.

## 4. SQLite foundation

### BIF-019 — Add the initial schema migration

**Depends on:** BIF-003, BIF-009

**Scope:** Add tables for store metadata, projects, counters, items, operations,
events, and mutation receipts with required keys and constraints.

**Acceptance:** A fresh database exposes the complete schema and a persistent
`store_id`.

### BIF-020 — Build the migration runner

**Depends on:** BIF-019

**Scope:** Apply embedded, checksummed, forward-only migrations transactionally
and reject a newer unsupported schema.

**Acceptance:** Tests cover fresh install, repeat startup, checksum mismatch,
newer schema, and failed-migration rollback.

### BIF-021 — Configure every SQLite connection

**Depends on:** BIF-020

**Scope:** Enable foreign keys, WAL, `synchronous=FULL`, and a five-second busy
timeout in one connection factory.

**Acceptance:** A connection-level test reads back all four settings.

### BIF-022 — Protect event immutability

**Depends on:** BIF-019, BIF-020

**Scope:** Add schema triggers that reject event updates and deletes.

**Acceptance:** Insert succeeds while update and delete fail in integration
tests.

### BIF-023 — Persist project registrations

**Depends on:** BIF-016, BIF-021

**Scope:** Store and list project slug/path/remote mappings.

**Acceptance:** Registrations survive reopening the database and conflicting
mappings are rejected.

## 5. Application writes

### BIF-024 — Implement authorization policy

**Depends on:** BIF-004, BIF-006

**Scope:** Enforce read/capture versus human-authorized mutation rules in the
application layer.

**Acceptance:** Tests consume all BIF-004 fixtures without invoking CLI or RPC
code.

### BIF-025 — Implement transactional capture

**Depends on:** BIF-010, BIF-021

**Scope:** In one `BEGIN IMMEDIATE` transaction, allocate a sequence, insert
the item and capture event, and commit.

**Acceptance:** Concurrent fresh-store captures produce unique, gap-free
committed IDs and complete capture history.

### BIF-026 — Add capture idempotency

**Depends on:** BIF-025

**Scope:** Canonicalize the validated capture payload, persist its receipt, and
handle replay versus payload conflict.

**Acceptance:** Concurrent identical keys return one item; changed payloads
return `idempotency_conflict`.

### BIF-027 — Implement transactional triage

**Depends on:** BIF-013, BIF-024, BIF-025

**Scope:** Validate the full intended change, update the item once, and append
all ordered events in one transaction.

**Acceptance:** Any invalid component leaves both item and history unchanged;
a successful compound change increments revision once.

### BIF-028 — Add optimistic concurrency

**Depends on:** BIF-027

**Scope:** Require and compare `expected_revision` for every item mutation.

**Acceptance:** A stale mutation returns `version_conflict` without writing an
item or event.

### BIF-029 — Add mutation idempotency

**Depends on:** BIF-026, BIF-028

**Scope:** Generalize receipts to all mutations and check a committed matching
receipt before stale-revision validation.

**Acceptance:** Retrying after a simulated lost response returns the original
result without duplicate events.

### BIF-030 — Map SQLite busy failures

**Depends on:** BIF-025, BIF-027

**Scope:** Translate exhausted busy-timeout errors at the storage boundary to
the stable `storage_busy` application error.

**Acceptance:** A lock-contention integration test observes `storage_busy`
rather than a raw SQLite error.

## 6. Application reads

### BIF-031 — Read one item

**Depends on:** BIF-021, BIF-025

**Scope:** Load an item by ID with all canonical fields.

**Acceptance:** A captured and triaged item round-trips without field loss.

### BIF-032 — Read item history

**Depends on:** BIF-027

**Scope:** Load immutable history ordered by revision then event index.

**Acceptance:** A compound operation is returned in semantic order even when
event timestamps are equal.

### BIF-033 — Implement named view selection

**Depends on:** BIF-031

**Scope:** Add proposed, ready, active, blocked, done, rejected, mine, and all
view predicates.

**Acceptance:** One mixed-status fixture produces the expected IDs for every
view.

### BIF-034 — Add list filters

**Depends on:** BIF-033

**Scope:** Add intersecting project, requester, assignee, status, priority,
unassigned, and text filters.

**Acceptance:** Tests prove filters intersect both each other and the selected
view.

### BIF-035 — Add ordering and pagination

**Depends on:** BIF-034

**Scope:** Add bounded pagination, newest-first default ordering, and `next`
ordering by priority, age, then ID.

**Acceptance:** Stable multi-page tests show no missing or repeated items;
`next` performs no mutation.

## 7. Adapters

### BIF-036 — Implement the RPC transport shell

**Depends on:** BIF-003, BIF-006

**Scope:** Read exactly one bounded UTF-8 JSON request, validate its envelope,
and emit exactly one response on stdout with diagnostics on stderr.

**Acceptance:** Transport tests cover malformed UTF-8/JSON, oversized input,
unsupported version, extra fields, null request ID, and exit codes.

### BIF-037 — Connect read operations to RPC

**Depends on:** BIF-031 through BIF-036

**Scope:** Map get, list, next, and history RPC requests to application reads.

**Acceptance:** BIF-003 success and error fixtures pass for read operations.

### BIF-038 — Connect mutation operations to RPC

**Depends on:** BIF-029, BIF-030, BIF-036

**Scope:** Map capture and triage-family RPC requests to application writes,
including actor and execution attribution.

**Acceptance:** BIF-003 and BIF-004 fixtures pass for mutation operations.

### BIF-039 — Add setup CLI commands

**Depends on:** BIF-014 through BIF-023

**Scope:** Implement `init`, `doctor`, `project register`, and `project list`
with concise human rendering.

**Acceptance:** Repeated identical initialization is harmless; `doctor`
reports version, config source, store ID/path, schema version, and project.

### BIF-040 — Add read CLI commands

**Depends on:** BIF-037

**Scope:** Implement `get`, `list`, `next`, and `history` as adapters over the
same application services used by RPC.

**Acceptance:** CLI integration tests cover every view, filters, ordering, and
pagination.

### BIF-041 — Add capture CLI command

**Depends on:** BIF-038

**Scope:** Implement `capture`, including provenance fields and idempotency key.

**Acceptance:** A CLI retry returns the original ID and identifies the response
as a replay.

### BIF-042 — Add mutation CLI commands

**Depends on:** BIF-038

**Scope:** Implement `triage`, approve, reject, prioritize, assign, start,
block, resume, and finish as adapters over one triage service.

**Acceptance:** Convenience commands and equivalent `triage` input produce the
same item and events.

### BIF-043 — Expose RPC through the CLI

**Depends on:** BIF-036 through BIF-038

**Scope:** Add `bif rpc` without duplicating protocol handling.

**Acceptance:** One process invocation accepts one request and produces only
one JSON response on stdout.

## 8. Recovery

### BIF-044 — Implement consistent backup

**Depends on:** BIF-021, BIF-029

**Scope:** Add `bif backup PATH` using SQLite's backup facilities.

**Acceptance:** Backup succeeds while WAL is active and opens as a valid BIF
store.

### BIF-045 — Document offline restore

**Depends on:** BIF-044

**Scope:** Document a stop, replace, verify, and rollback procedure; do not add
an online restore command.

**Acceptance:** Following the procedure restores store ID, items, counters,
history, and mutation receipts in an automated test.

## 9. Delta and Codex integration

### BIF-046 — Author the capture skill

**Depends on:** BIF-041, BIF-043

**Scope:** Add `skills/bif/SKILL.md` and `agents/openai.yaml` with explicit and
implicit invocation metadata and the capture-only workflow.

**Acceptance:** Static validation confirms the documented source layout and
invocation settings.

### BIF-047 — Test skill triggering and authority

**Depends on:** BIF-046

**Scope:** Add fixtures for `/bif`, `/bif <text>`, “BIF this,” ambiguous
context, ordinary BIF discussion, and forged mutation authorization.

**Acceptance:** Positive cases capture once, ambiguity asks one short question,
negative cases do not capture, and the skill cannot authorize mutations.

### BIF-048 — Author the management prompt

**Depends on:** BIF-040, BIF-042, BIF-043

**Scope:** Add `prompts/bif-thread.v1.md` covering store verification, fresh
reads, views, terse mutations, atomic triage, and human authority.

**Acceptance:** Prompt tests show that it never implements work automatically
and never treats task content as mutation authorization.

### BIF-049 — Add skill installation instructions

**Depends on:** BIF-046

**Scope:** Document installation into `.agents/skills/bif` and optional Codex
use without promising unsupported slash-command behavior.

**Acceptance:** A clean test location can install and discover the packaged
skill by following only the documentation.

## 10. MVP acceptance and local delivery

### BIF-050 — Run isolated lifecycle acceptance

**Depends on:** BIF-001 through BIF-049

**Scope:** Automate
capture → retry → approve/P1/assign → start → block → resume → finish.

**Acceptance:** Final state, revision, ordered history, attribution, and retry
receipts all match the frozen contracts.

### BIF-051 — Test missing machine-local state

**Depends on:** BIF-039, BIF-043

**Scope:** Run CLI and RPC commands with no local configuration.

**Acceptance:** Both return actionable `not_initialized` errors and create no
store.

### BIF-052 — Test concurrent startup

**Depends on:** BIF-020, BIF-039

**Scope:** Start two processes against a fresh configured store.

**Acceptance:** Migrations run once, both processes finish safely, and the
schema checksum remains valid.

### BIF-053 — Install and initialize the local MVP

**Depends on:** BIF-050 through BIF-052

**Scope:** Install the pinned binary, initialize one authoritative store, and
register the delivery repository.

**Acceptance:** `bif doctor` reports the expected executable, store ID, schema,
and project from the intended local machine.

### BIF-054 — Create and smoke-test the Delta thread

**Depends on:** BIF-047 through BIF-049, BIF-053

**Scope:** Create **BIF — Before I Forget** with the versioned prompt and test
the four required Delta interactions against the installed store.

**Acceptance:** `/bif`, `/bif <text>`, “BIF this,” and “Show proposed” work;
rewinding the conversation and reading again leaves authoritative state
unchanged.
