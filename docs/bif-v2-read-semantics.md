# Canonical v2 item read semantics (V2-009)

`application::read_semantics` defines effective filters, key comparison, and
query fingerprint input for the V2-008 typed page requests. This is application
logic, not a wire/MCP schema, SQL implementation, or opaque cursor codec.
The [approved contract](bif-v2-read-contract.md) remains authoritative.

## Effective filters

`EffectiveItemFilters::new` / `from_request` intersects scope with filters:

| View | Allowed statuses | Ownership |
| --- | --- | --- |
| proposed / ready / blocked / done / rejected | corresponding single status | unrestricted |
| active | in_progress, blocked | unrestricted |
| mine | proposed, ready, in_progress, blocked | configured requester as canonical lowercase assignee |
| all | all six statuses | unrestricted |

Explicit status and ownership restrictions intersect the view; they never
override it. `mine --assignee other`, `mine --unassigned`, and incompatible
statuses are empty queries. Explicit assignee plus unassigned is an input error,
even when the view would otherwise be empty. Every contradiction normalizes to
one empty predicate. Status sets have domain order: proposed, ready, in_progress,
blocked, done, rejected. Getters expose canonical project/requester/assignee IDs.

Text is a **literal substring** of title, description, or any one acceptance
criterion. Only ASCII case is folded, matching SQLite's built-in `lower`.
Unicode is otherwise unchanged; `%`, `_`, and brackets are literal characters;
whitespace is significant and is not trimmed. Fields/criteria are not joined.
Do not replace this with `LIKE`, Unicode case folding, tokenization, or FTS.
Absent priority means no priority restriction, not “priority is null.”
`matches` is a reference predicate, not permission to load all rows for paging.

## Sort and exclusive boundary

`ItemListOrdering::sort_spec` is the shared typed specification. `compare_keys`
interprets that specification; `is_after(candidate, boundary)` means strict
`Greater` in result order, never equality. `ItemReadKey::from(&Item)` supplies
metadata for comparison without inventing lexical display-ID ordering.

| Order | Coordinates, in precedence order |
| --- | --- |
| NewestFirst | captured_at DESC, requester ASC, project ASC, numeric sequence ASC |
| Next | priority rank ASC, captured_at ASC, requester ASC, project ASC, numeric sequence ASC |

`priority_rank`: P0=0, P1=1, P2=2, P3=3, P4=4, null=5.
`compare_captured_at` compares opaque timestamp strings as UTF-8 bytes; it does
not parse instants or normalize offsets. `compare_identity` compares canonical
requester/project strings then full-width `u64` sequence. 999 precedes 1000;
values above 2^53 are never converted to floating point.

### Downstream SQLite mapping

Use `i.captured_at COLLATE BINARY`, `i.requester COLLATE BINARY`,
`i.project_id COLLATE BINARY`, and integer `i.sequence`. Priority rank is:

```sql
CASE i.priority
  WHEN 'P0' THEN 0 WHEN 'P1' THEN 1 WHEN 'P2' THEN 2
  WHEN 'P3' THEN 3 WHEN 'P4' THEN 4 ELSE 5
END
```

Apply the same expressions/collations to ORDER BY, equality prefixes, and
boundary comparisons. The exclusive predicate is the OR of lexicographic
terms: equal preceding coordinates AND the current coordinate `>` for ASC or
`<` for DESC. A uniform tuple `>` is incorrect for mixed-direction list order.
Validate persisted enums; the CASE fallback must not legitimize corrupt data.

SQLite's current persisted sequence domain is positive signed INTEGER, at most
i64::MAX. Core keys retain full `u64`. A decoded boundary above i64::MAX must not
wrap, narrow, or be bound as REAL: within an otherwise equal identity prefix,
`sequence > boundary` is false and `sequence = boundary` is false (and
`sequence < boundary` is true). Earlier coordinates still decide ordering.
The later storage adapter must handle that domain explicitly.

## Fingerprint input is not cursor encoding

`ItemQueryFingerprintInput::new(operation, store_identity, schema_version,
request)` normalizes the typed request. `canonical_bytes` supplies deterministic,
unambiguous hash input. Binding includes list/next operation, stable store
identity supplied by the caller, projection schema version, projection, order,
and effective filters. Equivalent views/ASCII text case yield identical input.
Configured requester matters only when it changes effective `mine` ownership.
Limit, decoded boundary, raw view spelling, and transport are intentionally absent.

The byte format starts with `BIF:item-query:1\0`, then operation tag, length-framed
store string, u32 big-endian schema version, projection and ordering tags, status
count and tags, optional project/requester, ownership, optional priority, optional
text. Strings are UTF-8 with u64 big-endian byte lengths. Optional fields use
0=absent/1=present. List/next, summary/work/audit, newest/next and status tags follow
the listed order starting at 0; ownership uses 0=any, 1=assigned plus string,
2=unassigned; priority payload uses its rank. Format changes require a new prefix.

No token, authentication, hash algorithm, key bytes, or authorization decision
is supplied here. A later codec binds/authenticates query input **and** the last
emitted key, preserves unsigned integers, and reauthorizes each continuation.

## Evidence

`tests/v2_read_semantics.rs` compares canonical membership/order against v1 for
every named view and all filter families, including contradictions, mine,
Unicode, literal wildcards, and significant whitespace. Candidate SQLite ORDER
BY and every exclusive boundary are compared with v1 Rust ordering on existing
in-memory migrated stores, including timestamp ties, null priorities, identity
ties, 999/1000, >2^53 integers, and i64::MAX. Candidate SQL also compares
overflow-safe boundaries at i64::MAX+1 and u64::MAX with the core comparator.
