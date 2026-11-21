-- trk schema. One issue type, four states: open, claimed, waiting, closed.
-- Connection pragmas (set on every open, not here): journal_mode=WAL,
-- synchronous=NORMAL, foreign_keys=ON, busy_timeout=5000.

-- ============================================================
-- Projects, repos, actors
-- ============================================================

CREATE TABLE project (
  id          TEXT PRIMARY KEY            -- slug, also the issue-id prefix: 'orch'
              CHECK (id GLOB '[a-z]*' AND id NOT GLOB '*[^a-z0-9]*'),
  name        TEXT NOT NULL,
  created_at  TEXT NOT NULL
);

CREATE TABLE repo (
  id          INTEGER PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES project(id),
  remote_url  TEXT UNIQUE,                -- normalized: 'github.com/acme/orch'
  local_path  TEXT UNIQUE,                -- the checkout on this machine
  CHECK (remote_url IS NOT NULL OR local_path IS NOT NULL)
);

CREATE TABLE harness (
  name        TEXT PRIMARY KEY            -- 'claude-code', 'codex', ...
              CHECK (name GLOB '[a-z]*' AND name NOT GLOB '*[^a-z0-9-]*')
);

CREATE TABLE actor (
  id            TEXT PRIMARY KEY,         -- 'human:<email>' | 'agent:<harness>:<session>'
  kind          TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
  harness       TEXT REFERENCES harness(name),
  on_behalf_of  TEXT REFERENCES actor(id),
  first_seen    TEXT NOT NULL,
  meta          TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(meta)),
  CHECK (
    (kind = 'human' AND harness IS NULL AND id GLOB 'human:?*')
    OR
    (kind = 'agent' AND harness IS NOT NULL AND id GLOB 'agent:' || harness || ':?*')
  )
);

-- ============================================================
-- Issues
-- ============================================================

-- open    : nobody is working on it
-- claimed : exactly one active claim (enforced in core, both directions)
-- waiting : the session ended; `waiting_check` decides what happens next
-- closed  : done or abandoned
CREATE TABLE issue (
  id               TEXT PRIMARY KEY,      -- '<project>-<base32>': 'orch-4kq'
  project_id       TEXT NOT NULL REFERENCES project(id),
  repo_id          INTEGER REFERENCES repo(id),
  state            TEXT NOT NULL DEFAULT 'open'
                   CHECK (state IN ('open', 'claimed', 'waiting', 'closed')),
  title            TEXT NOT NULL CHECK (length(title) > 0),
  description      TEXT NOT NULL DEFAULT '',
  priority         TEXT NOT NULL DEFAULT 'P2' CHECK (priority IN ('P0', 'P1', 'P2', 'P3')),
  agent            TEXT,                  -- agent definition for `trk run`; NULL = default
  close_reason     TEXT CHECK (close_reason IN ('done', 'abandoned')),
  -- waiting
  waiting_reason   TEXT,
  waiting_check    TEXT,                  -- sh command; NULL = waits on a human
  check_interval   INTEGER,               -- seconds between checks
  next_check_at    TEXT,
  last_check_at    TEXT,
  last_check_exit  INTEGER,
  wait_cwd         TEXT,                  -- where the check runs: the claim's worktree
  -- system
  created_by       TEXT NOT NULL REFERENCES actor(id),
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  closed_at        TEXT,
  CHECK ((state = 'closed') = (close_reason IS NOT NULL)),
  CHECK ((state = 'closed') = (closed_at IS NOT NULL)),
  CHECK (state = 'waiting' OR (waiting_check IS NULL AND waiting_reason IS NULL AND next_check_at IS NULL))
);
CREATE INDEX issue_project_state ON issue(project_id, state);
CREATE INDEX issue_updated       ON issue(updated_at);
CREATE INDEX issue_due           ON issue(next_check_at) WHERE state = 'waiting';

-- src blocks dst: dst is not ready until src is closed.
CREATE TABLE dep (
  src         TEXT NOT NULL REFERENCES issue(id),
  dst         TEXT NOT NULL REFERENCES issue(id),
  created_by  TEXT NOT NULL REFERENCES actor(id),
  created_at  TEXT NOT NULL,
  PRIMARY KEY (src, dst),
  CHECK (src <> dst)
);
CREATE INDEX dep_by_dst ON dep(dst);

CREATE TABLE claim (
  id              INTEGER PRIMARY KEY,
  issue_id        TEXT NOT NULL REFERENCES issue(id),
  actor           TEXT NOT NULL REFERENCES actor(id),
  claimed_at      TEXT NOT NULL,
  lease_until     TEXT,                   -- NULL = no expiry
  worktree        TEXT,                   -- absolute path of the checkout used, if any
  host            TEXT,                   -- machine the claim was made on
  agent           TEXT,                   -- agent definition name, for agent actors
  terminal        TEXT,                   -- launcher terminal name set by `trk run`
  released_at     TEXT,
  release_reason  TEXT CHECK (release_reason IN
                    ('closed', 'waiting', 'unclaimed', 'expired', 'stolen')),
  CHECK ((released_at IS NULL) = (release_reason IS NULL))
);
CREATE UNIQUE INDEX claim_one_active ON claim(issue_id) WHERE released_at IS NULL;
CREATE INDEX claim_active_lease ON claim(lease_until) WHERE released_at IS NULL;
CREATE INDEX claim_by_actor ON claim(actor, released_at);

-- Append-only history. Notes are events with op = 'note'.
CREATE TABLE event (
  seq       INTEGER PRIMARY KEY,
  issue_id  TEXT REFERENCES issue(id),
  actor     TEXT NOT NULL REFERENCES actor(id),
  at        TEXT NOT NULL,                -- RFC 3339 UTC
  op        TEXT NOT NULL CHECK (op IN (
              'create', 'set', 'claim', 'release', 'wait', 'check', 'close',
              'reopen', 'dep_add', 'dep_rm', 'note')),
  payload   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload))
);
CREATE INDEX event_by_issue ON event(issue_id, seq);

CREATE TRIGGER event_no_update BEFORE UPDATE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
CREATE TRIGGER event_no_delete BEFORE DELETE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;

-- Issues are never deleted; they are closed.
CREATE TRIGGER issue_no_delete BEFORE DELETE ON issue
BEGIN SELECT RAISE(ABORT, 'issues are closed, not deleted'); END;

-- Delivery state of `trk note --to-agent` messages. The note itself is an
-- event; this table only records when the MCP server handed it over.
CREATE TABLE inbox (
  note_seq      INTEGER PRIMARY KEY REFERENCES event(seq),
  issue_id      TEXT NOT NULL REFERENCES issue(id),
  delivered_at  TEXT,
  delivered_to  TEXT REFERENCES actor(id)
);
CREATE INDEX inbox_pending ON inbox(issue_id) WHERE delivered_at IS NULL;

-- At most one `trk check` runs at a time, so checks (merges above all) are serial.
CREATE TABLE check_lock (
  id          INTEGER PRIMARY KEY CHECK (id = 1),
  pid         INTEGER NOT NULL,
  host        TEXT,
  started_at  TEXT NOT NULL
);

-- ============================================================
-- Full-text search (title, description, notes)
-- ============================================================

CREATE VIRTUAL TABLE search USING fts5(
  issue_id UNINDEXED, source UNINDEXED, body,
  tokenize = 'porter unicode61'
);

CREATE TRIGGER issue_fts_insert AFTER INSERT ON issue BEGIN
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.id, 'issue', NEW.title || ' ' || NEW.description);
END;
CREATE TRIGGER issue_fts_update AFTER UPDATE OF title, description ON issue BEGIN
  DELETE FROM search WHERE issue_id = NEW.id AND source = 'issue';
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.id, 'issue', NEW.title || ' ' || NEW.description);
END;
CREATE TRIGGER note_fts_insert AFTER INSERT ON event WHEN NEW.op = 'note' BEGIN
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.issue_id, 'note', json_extract(NEW.payload, '$.body'));
END;

-- ============================================================
-- Views
-- ============================================================

CREATE VIEW note AS
SELECT seq, issue_id, actor, at,
       json_extract(payload, '$.body')   AS body,
       json_extract(payload, '$.amends') AS amends
FROM event WHERE op = 'note';

CREATE VIEW active_claim AS
SELECT * FROM claim WHERE released_at IS NULL;

CREATE VIEW issue_view AS
SELECT i.*, c.actor AS claimant, c.lease_until
FROM issue i
LEFT JOIN active_claim c ON c.issue_id = i.id;

-- Open and every blocker closed.
CREATE VIEW ready AS
SELECT i.* FROM issue i
WHERE i.state = 'open'
  AND NOT EXISTS (
    SELECT 1 FROM dep d JOIN issue b ON b.id = d.src
    WHERE d.dst = i.id AND b.state <> 'closed'
  );
