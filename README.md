# BIF — Before I Forget

<img src="assets/branding/bif-logo.png" alt="Pixel-art portrait of Biff Tannen" width="128" height="128">

BIF is a local task ledger built with Rust and SQLite. Capture work before you
forget it, triage it into actionable tasks, and keep a durable history of who
requested each change and why.

The repository provides a `bif` command-line tool and a Rust library. It also
exposes **BIF RPC v1**, a one-shot JSON interface for automation and agent
integrations. This is a custom protocol, not JSON-RPC 2.0.

## Install

Install [Rust through rustup](https://rustup.rs/) and a native C toolchain
(SQLite is bundled and compiled during the build). The repository pins Rust
1.94.0 in `rust-toolchain.toml`.

From this checkout:

```sh
cargo build --locked
cargo install --path . --locked
```

Ensure Cargo's binary directory is on your `PATH`. To run without installing,
use `cargo run --locked -- <command>` instead of `bif <command>`.

## Quick start

The following examples use a POSIX shell. Choose a durable store directory
outside disposable agent checkouts. Replace `your-name` with your requester
identity.

```sh
# Initialize the store and write configuration.
mkdir -p "$HOME/bif-store"
bif init --root "$HOME/bif-store" --requester your-name
bif doctor

# Run from the project directory you want to track.
bif project register my-project --path "$PWD"
bif project list

# Capture creates a proposed item.
bif capture "Document the release process" \
  --project my-project \
  --description "Explain how to build, verify, and release." \
  --acceptance "A new contributor can follow the documented steps." \
  --idempotency-key release-docs-001

bif list proposed --project my-project
```

Copy the item ID printed by `capture`. Use `get` to read its current revision
before changing it:

```sh
bif get ITEM_ID
bif approve ITEM_ID --expected-revision REVISION --idempotency-key approve-docs-001
bif next --project my-project
bif history ITEM_ID
```

`ITEM_ID` and `REVISION` are placeholders, not literal arguments. Each successful
mutation advances the item revision; reread the item before the next change.
Reuse an idempotency key only when retrying the same request, and choose a new
key for a new operation.

## Example workflows

These recipes show common ways to use BIF after completing the quick start.
Values such as `ITEM_ID`, `REVISION`, and `ASSIGNEE` are placeholders. Run
`bif get ITEM_ID` before each mutation and substitute the revision it reports;
every successful mutation advances that revision.

### Capture now, review later

Use the proposed queue as an inbox for ideas, bugs, and follow-up work. Capture
enough context to make the item understandable without interrupting your current
task:

```sh
bif capture "Investigate slow dashboard queries" \
  --description "The dashboard took about eight seconds to load during testing." \
  --acceptance "The cause is documented." \
  --acceptance "A fix or a scoped follow-up is recorded." \
  --idempotency-key dashboard-query-capture-001

# Review the inbox later.
bif list proposed
bif get ITEM_ID

# Keep actionable work...
bif approve ITEM_ID \
  --expected-revision REVISION \
  --idempotency-key dashboard-query-approve-001

# ...or reject an item that should not enter the ready queue.
bif reject ITEM_ID "No longer reproducible" \
  --expected-revision REVISION \
  --idempotency-key dashboard-query-reject-001
```

The `approve` and `reject` commands above are alternative outcomes for a
proposed item, not sequential steps.

### Triage an item in one operation

Use `triage` when review should approve, prioritize, assign, and annotate an
item atomically:

```sh
bif get ITEM_ID
bif triage ITEM_ID \
  --action approve \
  --priority P1 \
  --assignee ASSIGNEE \
  --note "Schedule for the next release." \
  --expected-revision REVISION \
  --idempotency-key release-triage-001
```

If any part of the operation fails, none of its changes are committed. Triage
can also change only selected fields. Pass `--priority clear` or
`--assignee clear` to remove an existing value.

### Work an item through its lifecycle

`next` selects ready work. Start it, record blockers if necessary, resume it,
and finish it when its acceptance criteria are met:

```sh
bif next --project my-project --limit 1

bif get ITEM_ID
bif start ITEM_ID \
  --expected-revision REVISION_1 \
  --idempotency-key work-start-001

bif get ITEM_ID
bif block ITEM_ID "Waiting for the API contract" \
  --expected-revision REVISION_2 \
  --idempotency-key work-block-001

bif get ITEM_ID
bif resume ITEM_ID \
  --expected-revision REVISION_3 \
  --idempotency-key work-resume-001

bif get ITEM_ID
bif finish ITEM_ID \
  --expected-revision REVISION_4 \
  --idempotency-key work-finish-001

bif history ITEM_ID
```

Skip `block` and `resume` when work proceeds without interruption. `history`
provides the durable record of the lifecycle and triage changes.

### Review personal and project queues

Named views and filters support daily planning, stand-ups, and backlog review:

```sh
# Work assigned to the configured requester.
bif list mine

# Current work and blockers for one project.
bif list active --project my-project
bif list blocked --project my-project

# High-priority ready work.
bif list ready --project my-project --priority P0
bif list ready --project my-project --priority P1

# Search across item text and inspect recently completed work.
bif list all --text "release"
bif list done --project my-project --limit 20
```

Use `--json` on these commands when feeding the results to a script or agent.
Use `--offset` with `--limit` to page through a larger queue.

### Preserve context across an agent handoff

When an agent captures work from a conversation, attach source provenance so a
later human or agent can understand where it came from:

```sh
bif capture "Add retry guidance to the API documentation" \
  --project my-project \
  --source-host delta \
  --thread-id THREAD_ID \
  --message-id MESSAGE_ID \
  --url SOURCE_URL \
  --repository-reference REPOSITORY_REF \
  --revision-reference REVISION_REF \
  --context-excerpt "Retries must reuse the original idempotency key." \
  --idempotency-key api-retry-guidance-001

bif get ITEM_ID --json
bif history ITEM_ID --json
```

This records a reference to the handoff context in the local ledger; it does not
copy or synchronize the conversation itself. Use `--source-host codex` for
Codex-originated work or `--source-host local` for other local capture.

## Commands

| Command | Purpose |
| --- | --- |
| `init`, `doctor` | Configure a store and inspect its health. |
| `project register`, `project list` | Associate local directories with stable project identities. |
| `capture TITLE` | Create a proposed item with optional description, acceptance criteria, and source context. |
| `get ITEM_ID` | Inspect an item's current state and revision. |
| `list [VIEW]` | Browse items with filters and pagination. |
| `next` | Select ready work. |
| `history ITEM_ID` | Read an item's operation history. |
| `approve`, `reject` | Accept or reject proposed work. |
| `prioritize`, `assign` | Set priority or assignee. |
| `start`, `block`, `resume`, `finish` | Record progress through the lifecycle. |
| `triage` | Apply lifecycle, priority, assignee, and note changes atomically. |
| `rpc` | Read one BIF RPC v1 request from stdin and write one response to stdout. |

Read commands (`get`, `list`, `next`, and `history`) support `--json`. List
filters include `--project`, `--requester`, `--assignee`, `--status`,
`--priority`, and `--text`; use `--limit` and `--offset` for pagination.
Priorities range from `P0` to `P4`.

Lifecycle and triage mutations require `--expected-revision` and
`--idempotency-key`. `reject` and `block` also require a reason. Capture accepts
repeated `--acceptance` options and provenance such as `--source-host`,
`--thread-id`, `--message-id`, and `--url`.

Run `bif` without arguments to print the CLI usage summary (this exits with a
usage-error status).

## Configuration and storage

`bif init` writes a configuration file containing `root` and `requester`, and
prints both the configuration and database paths. To select a configuration
explicitly, pass `--config PATH` or set `BIF_CONFIG`.

For commands that accept them, `--root` and `--requester` override configuration
values. `BIF_ROOT` and `BIF_REQUESTER` provide environment overrides; command-line
values take precedence over environment values, which take precedence over the
file.

The database lives at `<root>/.bif/bif.sqlite`. BIF retains current item state,
immutable events, and retry records in SQLite. Project registration avoids
relying on checkout directory names; capture can also select a project explicitly
with `--project`.

**The store is machine-local.** Sharing a Delta or Codex conversation does not
synchronize the database, and rewinding a conversation does not undo ledger
operations. Keep the database and its SQLite sidecar files out of source control.
Cross-machine synchronization, branching, merging, and reopening completed work
are outside the v1 scope.

## Development

```sh
cargo build --locked
cargo test --locked
```

The code is organized into domain rules, application use cases, SQLite storage,
configuration, and CLI/RPC adapters under `src/`. Database migrations live in
`migrations/`; integration tests live in `tests/`.

Further documentation:

- [v1 implementation plan](docs/bif-v1-plan.md): architecture and intended contracts.
- [v1 task backlog](docs/bif-v1-tasks.md): delivery scope and acceptance checks.
- [Contract fixtures](docs/fixtures/): canonical values, lifecycle rules, RPC
  envelopes, and authorization examples.

The plan and backlog describe intended delivery, not a declaration that every
planned integration or recovery feature is already implemented.
