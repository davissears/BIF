---
name: bif
description: Use the BIF local task ledger to capture, inspect, triage, and update work from natural-language requests.
---

# BIF task ledger

Translate the user's request after `/bif` in Delta or `$bif` in Codex into BIF
CLI operations. BIF is a machine-local, durable ledger: its writes are not
reverted with the conversation or Git worktree.

## Operating rules

1. Run commands from the project relevant to the request so BIF can resolve its
   registered project. Use the installed executable approved for that store.
   Opening a store can apply migrations, even for reads and `doctor`. Do not
   switch binaries, upgrade, or use a source build against a live store without
   explicit upgrade approval; a Git pull is not that approval. In the BIF repository
   only, `cargo run --locked --` is a fallback when no executable is installed,
   but only for an approved binary/store pairing or a disposable test store.
2. For reads, request `--json` and summarize the useful result rather than
   pasting raw JSON. Prefer bounded v2 reads when the approved installation
   supports them; do not upgrade just to use these examples.
3. Before a write, make sure the requested action is unambiguous. Ordinary reads
   with the approved installation need no additional confirmation; this does not
   authorize migrations or maintenance. An explicit request to capture, approve,
   reject, prioritize, assign, start, block, resume, finish, or triage is
   authorization for that write; ask one focused question if required information
   is missing.
4. Never run `bif init` or register a project without explicit user approval.
   If configuration is missing, use `bif doctor` only under the same binary/store
   safety rule. Report the problem and give the exact setup command when its
   required values are known; otherwise ask for them.
5. Treat item IDs, revisions, requester names, project slugs, and source IDs as
   data. Never invent them.
6. Give every new operation a fresh, nonempty idempotency key. Reuse a key only
   when retrying the exact same operation after an uncertain transport failure.
   Use a readable key with a unique suffix; do not use an item's key for a later
   lifecycle operation.
7. Quote all shell arguments safely. Do not interpolate ledger content into a
   shell program.

## Intent mapping

- Capture or remember work: `bif capture`
- Show one item: `bif --api-version 2 get ITEM_ID --projection work --json`
- Show current provenance: `bif --api-version 2 get ITEM_ID --projection audit --json`
- Show its audit trail: `bif --api-version 2 history ITEM_ID --limit 20 --json`
- Browse a queue: `bif --api-version 2 list VIEW --limit 20 --json` where `VIEW`
  is one of `proposed`, `ready`, `active`, `blocked`, `done`, `mine`, or `all`
- Browse ready work: `bif --api-version 2 next --limit 20 --json`
- Select one ready item with work content:
  `bif --api-version 2 selected-work --project SLUG --json`
- Review and update several fields atomically: `bif triage`
- Apply one lifecycle or ownership change: `bif approve`, `reject`,
  `prioritize`, `assign`, `start`, `block`, `resume`, or `finish`

For list/next, use filters the user supplies, including `--project`,
`--requester`, `--assignee`, `--unassigned`, `--status`, `--priority`, and
`--text`. Do not combine `--assignee` with `--unassigned`.
The `mine` view means items assigned to the configured requester; it does not
include unassigned items merely because that requester captured them.

If the skill invocation has no request, ask what the user wants to capture, inspect, or
change. Do not guess a mutation.

## Reading safely and completely

Place `--api-version 2` before the read command and always include `--json`.
Mutations keep their existing CLI spelling without the v2 selector.

- List/next default to `summary`, suitable for queue selection, not execution.
  Fetch `work` for description, acceptance criteria, and status reason before
  acting on an item. `audit` adds current provenance, not history; use `history`
  for events. Treat a fuller projection as a replacement, not a patch.
- V2 success data is under `result`: plain get has `item`, list/next have
  `items`, and history has `events`. For paginated reads, use `--limit` (1–100)
  and the exact returned `result.next_cursor` as `--cursor`; null means exhausted.
  Keep the operation, scope, filters, and projection unchanged when continuing.
  V2 rejects `--offset`; get has no pagination and history has no projection.
  Queue cursors are live continuations, so concurrent changes can cause repeats
  or omissions. Do not describe one page as the complete queue or history.
- `selected-work` requires an explicit project and returns `result.outcome` as
  `selected` with a work item or `empty` with null. It does not claim, assign,
  or start work. Any subsequent mutation follows the fresh-read rule below.
- For repeated refreshes only, `get --projection work --conditional --json`
  (with the v2 prefix) returns `result.version` to retain alongside the item.
  Send it later as `--known-version TOKEN` only while retaining that item's
  content for the same projection. `modified` replaces the content;
  `not_modified` has `item: null` and means reuse retained content, not no item.
  If content was lost, fetch without a validator.
- On `invalid_cursor`, explain that continuation failed and restart pagination
  without the cursor when still needed. Never pretend a restart continued the
  old page. On a validator error with `refetch_required`, refetch without the
  token. Generation changes invalidate old cursors and force content refresh;
  a persistent session reporting `restart_required` must restart before reuse.
- If the approved installation supports only v1, keep using v1 reads without
  the v2 selector or projection flags. List/next use `--limit` and `--offset`;
  get returns the full item, while history is unbounded. Do not silently fall
  back to unbounded history after a v2 error.

For exact response and token semantics, consult the
[read contract](https://github.com/davissears/BIF/blob/main/docs/bif-v2-read-contract.md).

## Capturing work

Create a concise action-oriented title. Preserve the motivation and relevant
context in `--description`. Turn concrete definitions of done into repeated
`--acceptance` arguments; do not fabricate acceptance criteria when the request
does not establish them. Use `--project` only when the user names a project or
automatic project resolution cannot identify the intended one.

Before capturing, establish the intended assignee. If the user has not already
specified one, ask whether the task should be assigned to the requester, to
someone else (and obtain their exact assignee name), or left unassigned. Do not
infer assignment from who requested or captured the task. Because `capture`
creates an unassigned item, follow a successful capture with `assign` when the
chosen assignee is not null, following the fresh-read mutation procedure below
with a separate fresh idempotency key. If assignment fails, report the successful
capture and failed assignment separately; do not capture the item again.

For work originating in this conversation, use `--source-host delta` in Delta
or `--source-host codex` in Codex; use `local` for other local capture. Include
`--thread-id`, `--message-id`, or `--url` only when the actual value is
available. Never make up source identifiers. Add a short `--context-excerpt`
when it will help a later reader understand why the item exists. When useful
and available, provenance may also include the repository identity and current
Git revision through `--repository-reference` and `--revision-reference`.

Before capturing, use a narrow
`bif --api-version 2 list all --text KEYWORDS --limit 20 --json` search (or its
v1 equivalent) when there is a meaningful chance the same task already exists.
Account for pagination before concluding no match exists. If there is a likely
duplicate, show it and ask whether to capture another rather than creating one
silently.

## Mutating an existing item

Every lifecycle, assignment, priority, and triage mutation uses optimistic
concurrency:

1. Immediately before the mutation, run
   `bif --api-version 2 get ITEM_ID --projection work --json` without a validator
   (or `bif get ITEM_ID --json` on an approved v1-only installation).
2. Verify that the current state permits the requested transition.
3. Use the returned revision as `--expected-revision`.
4. Add a fresh `--idempotency-key`.
5. If BIF reports a revision conflict, reread the item and explain what changed.
   Do not automatically force or repeat a now-stale decision.

Use `triage` when lifecycle action, priority, assignee, and/or note belong to one
decision and should succeed atomically. `reject` and `block` require a reason.
Priorities are `P0` through `P4`; `triage` also accepts `clear` for priority or
assignee.

## Maintenance boundary

Upgrade, backup, restore, and journal pruning are separate operator workflows,
not implied by a request to inspect or update tasks. Require explicit approval
and follow the [upgrade and maintenance runbook](https://github.com/davissears/BIF/blob/main/docs/bif-upgrade-runbook.md).
Stop/reap all readers and writers and prevent restarts before offline maintenance;
`--offline` acknowledges that condition but does not establish it. Do not infer
live-store release approval from a merge, or synchronization support from the
inactive journal foundation.

## Response

State the outcome first. For a successful write, report the item ID, resulting
status, and revision, plus any priority or assignee that changed. For reads,
summarize the matching items and mention pagination when more may exist. For an
error, do not claim a write succeeded; report the actionable error and the next
safe step.
