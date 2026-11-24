-- Rebuild without renaming the old table: inbox references must keep naming event.
DROP VIEW note;
CREATE TABLE event_chat (
  seq       INTEGER PRIMARY KEY,
  issue_id  TEXT REFERENCES issue(id),
  actor     TEXT NOT NULL REFERENCES actor(id),
  at        TEXT NOT NULL,
  op        TEXT NOT NULL CHECK (op IN (
              'create', 'set', 'claim', 'release', 'wait', 'check', 'close',
              'reopen', 'dep_add', 'dep_rm', 'note', 'resume_chat')),
  payload   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload))
);
INSERT INTO event_chat SELECT seq, issue_id, actor, at, op, payload FROM event;
DROP TABLE event;
ALTER TABLE event_chat RENAME TO event;
CREATE INDEX event_by_issue ON event(issue_id, seq);
CREATE TRIGGER event_no_update BEFORE UPDATE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
CREATE TRIGGER event_no_delete BEFORE DELETE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
CREATE TRIGGER note_fts_insert AFTER INSERT ON event WHEN NEW.op = 'note' BEGIN
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.issue_id, 'note', json_extract(NEW.payload, '$.body'));
END;
CREATE VIEW note AS
SELECT seq, issue_id, actor, at,
       json_extract(payload, '$.body') AS body,
       json_extract(payload, '$.amends') AS amends
FROM event WHERE op = 'note';
