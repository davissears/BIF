-- BIF v1's initial durable data model. Migration execution and bookkeeping are
-- deliberately owned by BIF-020; this file is the schema input to that work.

CREATE TABLE store_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    store_id TEXT NOT NULL UNIQUE CHECK (length(store_id) > 0),
    created_at TEXT NOT NULL CHECK (length(created_at) > 0)
);

-- Generate the identity as part of fresh schema creation so opening sessions
-- only ever read an already-persisted value.
INSERT INTO store_metadata (singleton, store_id, created_at)
VALUES (
    1,
    lower(hex(randomblob(4))) || '-' ||
        lower(hex(randomblob(2))) || '-4' ||
        substr(lower(hex(randomblob(2))), 2) || '-' ||
        substr('89ab', abs(random()) % 4 + 1, 1) ||
        substr(lower(hex(randomblob(2))), 2) || '-' ||
        lower(hex(randomblob(6))),
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

CREATE TABLE projects (
    project_id TEXT PRIMARY KEY CHECK (
        length(project_id) > 0 AND project_id = lower(project_id)
    ),
    created_at TEXT NOT NULL CHECK (length(created_at) > 0)
);

CREATE TABLE project_path_mappings (
    canonical_path TEXT PRIMARY KEY CHECK (length(canonical_path) > 0),
    project_id TEXT NOT NULL REFERENCES projects(project_id)
);

CREATE TABLE project_remote_mappings (
    normalized_remote TEXT PRIMARY KEY CHECK (length(normalized_remote) > 0),
    project_id TEXT NOT NULL REFERENCES projects(project_id)
);

CREATE TABLE requester_project_counters (
    requester TEXT NOT NULL CHECK (
        length(requester) > 0 AND requester = upper(requester)
    ),
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    next_sequence INTEGER NOT NULL CHECK (next_sequence > 0),
    PRIMARY KEY (requester, project_id)
);

CREATE TABLE items (
    item_id TEXT PRIMARY KEY CHECK (length(item_id) > 0),
    requester TEXT NOT NULL CHECK (
        length(requester) > 0 AND requester = upper(requester)
    ),
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    title TEXT NOT NULL CHECK (length(title) > 0),
    description TEXT,
    status TEXT NOT NULL CHECK (
        status IN ('proposed', 'ready', 'in_progress', 'blocked', 'done', 'rejected')
    ),
    priority TEXT CHECK (priority IS NULL OR priority IN ('P0', 'P1', 'P2', 'P3', 'P4')),
    assignee TEXT CHECK (
        assignee IS NULL OR (length(assignee) > 0 AND assignee = lower(assignee))
    ),
    status_reason TEXT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    captured_at TEXT NOT NULL CHECK (length(captured_at) > 0),
    updated_at TEXT NOT NULL CHECK (length(updated_at) > 0),
    UNIQUE (requester, project_id, sequence)
);

CREATE TABLE item_acceptance_criteria (
    item_id TEXT NOT NULL REFERENCES items(item_id) ON DELETE CASCADE,
    criterion_index INTEGER NOT NULL CHECK (criterion_index >= 0),
    criterion TEXT NOT NULL,
    PRIMARY KEY (item_id, criterion_index)
);

CREATE TABLE item_provenance (
    item_id TEXT PRIMARY KEY REFERENCES items(item_id) ON DELETE CASCADE,
    source_host TEXT CHECK (
        source_host IS NULL OR source_host IN ('delta', 'codex', 'local')
    ),
    thread_id TEXT,
    message_id TEXT,
    url TEXT,
    repository_reference TEXT,
    revision_reference TEXT,
    context_excerpt TEXT
);

CREATE TABLE operations (
    operation_id TEXT PRIMARY KEY CHECK (length(operation_id) > 0),
    item_id TEXT NOT NULL REFERENCES items(item_id),
    operation_type TEXT NOT NULL CHECK (
        operation_type IN (
            'capture', 'triage', 'approve', 'reject', 'prioritize', 'assign',
            'start', 'block', 'resume', 'finish'
        )
    ),
    expected_revision INTEGER CHECK (expected_revision IS NULL OR expected_revision > 0),
    item_revision INTEGER NOT NULL CHECK (item_revision > 0),
    occurred_at TEXT NOT NULL CHECK (length(occurred_at) > 0),
    UNIQUE (operation_id, item_revision),
    UNIQUE (item_id, item_revision)
);

CREATE TABLE events (
    event_id TEXT PRIMARY KEY CHECK (length(event_id) > 0),
    operation_id TEXT NOT NULL REFERENCES operations(operation_id),
    item_id TEXT NOT NULL REFERENCES items(item_id),
    item_revision INTEGER NOT NULL CHECK (item_revision > 0),
    event_index INTEGER NOT NULL CHECK (event_index >= 0),
    event_type TEXT NOT NULL CHECK (
        event_type IN (
            'captured', 'approved', 'rejected', 'started', 'blocked', 'resumed',
            'finished', 'priority_changed', 'assignee_changed', 'note_added'
        )
    ),
    before_value TEXT,
    after_value TEXT,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'agent')),
    actor_id TEXT NOT NULL CHECK (length(actor_id) > 0),
    actor_surface TEXT NOT NULL CHECK (actor_surface IN ('thread', 'cli', 'rpc')),
    actor_host TEXT NOT NULL CHECK (actor_host IN ('delta', 'codex', 'local')),
    execution_kind TEXT NOT NULL CHECK (execution_kind IN ('direct', 'agent')),
    execution_agent_id TEXT,
    execution_surface TEXT NOT NULL CHECK (execution_surface IN ('thread', 'cli', 'rpc')),
    execution_host TEXT NOT NULL CHECK (execution_host IN ('delta', 'codex', 'local')),
    reason TEXT,
    note TEXT,
    occurred_at TEXT NOT NULL CHECK (length(occurred_at) > 0),
    event_schema_version INTEGER NOT NULL CHECK (event_schema_version > 0),
    CHECK (
        (execution_kind = 'direct' AND execution_agent_id IS NULL) OR
        (execution_kind = 'agent' AND length(execution_agent_id) > 0)
    ),
    UNIQUE (operation_id, event_index),
    UNIQUE (item_id, item_revision, event_index),
    FOREIGN KEY (operation_id, item_revision)
        REFERENCES operations(operation_id, item_revision)
);

CREATE TABLE mutation_receipts (
    mutation_key TEXT PRIMARY KEY CHECK (length(mutation_key) > 0),
    mutation_type TEXT NOT NULL CHECK (
        mutation_type IN (
            'capture', 'triage', 'approve', 'reject', 'prioritize', 'assign',
            'start', 'block', 'resume', 'finish'
        )
    ),
    payload_hash TEXT NOT NULL CHECK (length(payload_hash) > 0),
    operation_id TEXT NOT NULL UNIQUE REFERENCES operations(operation_id),
    item_id TEXT NOT NULL REFERENCES items(item_id),
    response_json TEXT NOT NULL CHECK (length(response_json) > 0),
    created_at TEXT NOT NULL CHECK (length(created_at) > 0)
);
