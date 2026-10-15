-- Prepared storage only: ordinary open never activates synchronization.
-- Rust inserts an independent OS-random 128-bit generation in this transaction.
-- F and H are store-global, not project-local; no timestamp-ordered audit backfill.
CREATE TABLE journal_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    generation TEXT NOT NULL CHECK (
        length(generation) = 32 AND generation NOT GLOB '*[^0-9a-f]*'
    ),
    retention_floor INTEGER NOT NULL CHECK (retention_floor >= 0),
    high_water INTEGER NOT NULL CHECK (high_water >= retention_floor),
    sync_enabled INTEGER NOT NULL CHECK (sync_enabled IN (0, 1)),
    bootstrap_required INTEGER NOT NULL CHECK (bootstrap_required IN (0, 1))
) STRICT;

CREATE TRIGGER journal_metadata_monotonic
BEFORE UPDATE ON journal_metadata
WHEN NEW.generation = OLD.generation
 AND (NEW.retention_floor < OLD.retention_floor OR NEW.high_water < OLD.high_water)
BEGIN
    SELECT RAISE(ABORT, 'journal coverage cannot decrease within a generation');
END;

-- SQLite AUTOINCREMENT remembers committed allocations even after prune-all.
-- Signed i64::MAX is the allocator limit; never fall back to random ROWIDs.
CREATE TABLE change_journal (
    change_seq INTEGER PRIMARY KEY AUTOINCREMENT CHECK (change_seq > 0),
    operation_id TEXT NOT NULL UNIQUE REFERENCES operations(operation_id),
    item_id TEXT NOT NULL REFERENCES items(item_id),
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    item_revision INTEGER NOT NULL CHECK (item_revision > 0)
) STRICT;

CREATE INDEX idx_change_journal_project_seq ON change_journal (project_id, change_seq);

-- One INSERT statement owns both the linked row and H. ABORT undoes the entire
-- statement, including trigger effects, even if the outer caller later commits.
CREATE TRIGGER change_journal_append
AFTER INSERT ON change_journal
BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM journal_metadata WHERE singleton = 1
         AND NEW.change_seq > high_water
    ) THEN RAISE(ABORT, 'journal metadata missing or allocation not increasing') END;
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM operations o JOIN items i ON i.item_id = o.item_id
         WHERE o.operation_id = NEW.operation_id AND o.item_id = NEW.item_id
          AND o.item_revision = NEW.item_revision AND i.project_id = NEW.project_id
    ) THEN RAISE(ABORT, 'journal operation identity mismatch') END;
    UPDATE journal_metadata SET high_water = NEW.change_seq WHERE singleton = 1;
END;

CREATE TRIGGER change_journal_reject_update
BEFORE UPDATE ON change_journal
BEGIN
    SELECT RAISE(ABORT, 'journal rows cannot be amended');
END;
