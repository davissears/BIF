---
name: bif
description: Use the BIF local task ledger to capture, inspect, triage, and update work from natural-language requests.
---

# BIF task ledger

Translate the user's request after `/bif` into BIF CLI operations. BIF is a
machine-local, durable ledger: its writes are not reverted with the conversation
or Git worktree.

## Operating rules

1. Run commands from the project relevant to the request so BIF can resolve its
   registered project. Prefer the installed `bif` executable. In this repository
   only, if it is not installed, use `cargo run --locked --` as the command
   prefix.
2. For reads, request `--json` and summarize the useful result rather than
   pasting raw JSON.
3. Before a write, make sure the requested action is unambiguous. Reads do not
   need confirmation. An explicit request to capture, approve, reject,
   prioritize, assign, start, block, resume, finish, or triage is authorization
   for that write; ask one focused question if required information is missing.
4. Never run `bif init` or register a project without explicit user approval.
   If configuration is missing, run `bif doctor`, report the problem, and give
   the exact setup command that would resolve it.
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
- Show one item: `bif get ITEM_ID --json`
- Show its audit trail: `bif history ITEM_ID --json`
- Browse a queue: `bif list VIEW --json` where `VIEW` is one of `proposed`,
  `ready`, `active`, `blocked`, `done`, `mine`, or `all`
- Choose ready work: `bif next --json`
- Review and update several fields atomically: `bif triage`
- Apply one lifecycle or ownership change: `bif approve`, `reject`,
  `prioritize`, `assign`, `start`, `block`, `resume`, or `finish`

Use filters the user supplies, including `--project`, `--requester`,
`--assignee`, `--status`, `--priority`, and `--text`. Use `--limit` and
`--offset` instead of silently truncating a large result.
The `mine` view means items assigned to the configured requester; it does not
include unassigned items merely because that requester captured them.

If `/bif` has no request, ask what the user wants to capture, inspect, or
change. Do not guess a mutation.

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
chosen assignee is not null, using the captured item's revision and a separate
fresh idempotency key.

For work originating in this conversation, pass `--source-host delta`. Include
`--thread-id`, `--message-id`, or `--url` only when the actual value is
available. Never make up source identifiers. Add a short `--context-excerpt`
when it will help a later reader understand why the item exists. When useful
and available, provenance may also include the repository identity and current
Git revision through `--repository-reference` and `--revision-reference`.

Before capturing, use a narrow `bif list all --text KEYWORDS --json` search when
there is a meaningful chance the same task already exists. If there is a likely
duplicate, show it and ask whether to capture another rather than creating one
silently.

## Mutating an existing item

Every lifecycle, assignment, priority, and triage mutation uses optimistic
concurrency:

1. Immediately before the mutation, run `bif get ITEM_ID --json`.
2. Verify that the current state permits the requested transition.
3. Use the returned revision as `--expected-revision`.
4. Add a fresh `--idempotency-key`.
5. If BIF reports a revision conflict, reread the item and explain what changed.
   Do not automatically force or repeat a now-stale decision.

Use `triage` when lifecycle action, priority, assignee, and/or note belong to one
decision and should succeed atomically. `reject` and `block` require a reason.
Priorities are `P0` through `P4`; `triage` also accepts `clear` for priority or
assignee.

## Response

State the outcome first. For a successful write, report the item ID, resulting
status, and revision, plus any priority or assignee that changed. For reads,
summarize the matching items and mention pagination when more may exist. For an
error, do not claim a write succeeded; report the actionable error and the next
safe step.
