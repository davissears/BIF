-- Indexes for the measured projection list, next, active, and mine read paths.
CREATE INDEX idx_items_read_list ON items
    (captured_at DESC, requester, project_id, sequence);

CREATE INDEX idx_items_read_ready ON items
    (CASE priority WHEN 'P0' THEN 0 WHEN 'P1' THEN 1 WHEN 'P2' THEN 2
     WHEN 'P3' THEN 3 WHEN 'P4' THEN 4 ELSE 5 END,
     captured_at, requester, project_id, sequence) WHERE status = 'ready';

CREATE INDEX idx_items_read_active ON items
    (captured_at DESC, requester, project_id, sequence)
    WHERE status IN ('in_progress', 'blocked');

CREATE INDEX idx_items_read_mine ON items
    (assignee, captured_at DESC, requester, project_id, sequence)
    WHERE status IN ('proposed', 'ready', 'in_progress', 'blocked');
