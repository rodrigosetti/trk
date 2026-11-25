-- Existing closed issues remain pending until their resources are inspected.
ALTER TABLE issue ADD COLUMN gc_collected_at TEXT;
CREATE INDEX issue_gc_pending ON issue(closed_at, id)
  WHERE state = 'closed' AND gc_collected_at IS NULL;

CREATE TABLE gc_reservation (
  issue_id TEXT PRIMARY KEY REFERENCES issue(id),
  lock_id  INTEGER NOT NULL CHECK (lock_id = 1)
           REFERENCES check_lock(id) ON DELETE CASCADE
);

CREATE TRIGGER issue_gc_reserved BEFORE UPDATE OF state ON issue
WHEN OLD.state = 'closed' AND NEW.state <> 'closed'
  AND EXISTS (SELECT 1 FROM gc_reservation WHERE issue_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'issue garbage collection is running; retry after it finishes'); END;

CREATE TRIGGER issue_gc_reopen AFTER UPDATE OF state ON issue
WHEN OLD.state = 'closed' AND NEW.state <> 'closed'
BEGIN UPDATE issue SET gc_collected_at = NULL WHERE id = NEW.id; END;
