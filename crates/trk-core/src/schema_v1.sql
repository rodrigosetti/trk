-- trk schema v1. Applied by rusqlite_migration; user_version = 1.
-- Connection pragmas (set on every open, not here): journal_mode=WAL,
-- synchronous=NORMAL, foreign_keys=ON, busy_timeout=5000.

-- ============================================================
-- Registry: projects, repos, actors
-- ============================================================

CREATE TABLE project (
  id          TEXT PRIMARY KEY
              CHECK (id GLOB '[a-z]*' AND id NOT GLOB '*[^a-z0-9]*'),
  name        TEXT NOT NULL,
  created_at  TEXT NOT NULL
);

CREATE TABLE repo (
  id          INTEGER PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES project(id),
  remote_url  TEXT UNIQUE,
  local_path  TEXT UNIQUE,
  CHECK (remote_url IS NOT NULL OR local_path IS NOT NULL)
);

CREATE TABLE harness (
  name        TEXT PRIMARY KEY
              CHECK (name GLOB '[a-z]*' AND name NOT GLOB '*[^a-z0-9-]*')
);

CREATE TABLE actor (
  id            TEXT PRIMARY KEY,
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
-- Registry: the configurable schema
-- ============================================================

-- agent: the type's routing.agent; tools: {status: {name, description}}
-- naming the generated transition tools.
CREATE TABLE issue_type (
  name         TEXT PRIMARY KEY,
  description  TEXT,
  agent        TEXT,
  tools        TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(tools))
);

CREATE TABLE status (
  type      TEXT NOT NULL REFERENCES issue_type(name),
  name      TEXT NOT NULL,
  category  TEXT NOT NULL CHECK (category IN ('open', 'active', 'closed')),
  ord       INTEGER NOT NULL,
  PRIMARY KEY (type, name)
);
CREATE UNIQUE INDEX status_one_open   ON status(type) WHERE category = 'open';
CREATE UNIQUE INDEX status_one_closed ON status(type) WHERE category = 'closed';

CREATE TABLE transition (
  type         TEXT NOT NULL,
  from_status  TEXT NOT NULL,
  to_status    TEXT NOT NULL,
  PRIMARY KEY (type, from_status, to_status),
  FOREIGN KEY (type, from_status) REFERENCES status(type, name),
  FOREIGN KEY (type, to_status)   REFERENCES status(type, name)
);

CREATE TABLE resolution (
  type  TEXT NOT NULL REFERENCES issue_type(name),
  name  TEXT NOT NULL,
  PRIMARY KEY (type, name)
);

CREATE TABLE field (
  name         TEXT PRIMARY KEY CHECK (name GLOB '[a-z]*' AND name NOT GLOB '*[^a-z0-9_]*'),
  kind         TEXT NOT NULL CHECK (kind IN (
                 'text', 'markdown', 'int', 'bool', 'date', 'datetime',
                 'url', 'enum', 'actor', 'repo', 'link', 'list')),
  config       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config)),
  shared       INTEGER NOT NULL DEFAULT 0 CHECK (shared IN (0, 1)),
  description  TEXT
);

CREATE TABLE type_field (
  type                TEXT NOT NULL REFERENCES issue_type(name),
  field               TEXT NOT NULL REFERENCES field(name),
  required_at_create  INTEGER NOT NULL DEFAULT 0 CHECK (required_at_create IN (0, 1)),
  default_value       TEXT CHECK (default_value IS NULL OR json_valid(default_value)),
  PRIMARY KEY (type, field)
);

CREATE TABLE dep_kind (
  name           TEXT PRIMARY KEY,
  inverse_label  TEXT NOT NULL,
  blocks_ready   INTEGER NOT NULL DEFAULT 0 CHECK (blocks_ready IN (0, 1)),
  acyclic        INTEGER NOT NULL DEFAULT 1 CHECK (acyclic IN (0, 1)),
  same_type      INTEGER NOT NULL DEFAULT 0 CHECK (same_type IN (0, 1)),
  description    TEXT
);

CREATE TABLE dep_kind_types (
  kind      TEXT NOT NULL REFERENCES dep_kind(name),
  src_type  TEXT NOT NULL,
  dst_type  TEXT NOT NULL,
  PRIMARY KEY (kind, src_type, dst_type)
);

CREATE TABLE requirement (
  id              INTEGER PRIMARY KEY,
  type            TEXT NOT NULL REFERENCES issue_type(name),
  to_status       TEXT NOT NULL,
  resolution      TEXT,
  kind            TEXT NOT NULL CHECK (kind IN ('field', 'min_items', 'out_edge', 'in_edges_closed')),
  field           TEXT REFERENCES field(name),
  n               INTEGER,
  dep_kind        TEXT REFERENCES dep_kind(name),
  src_resolution  TEXT,
  FOREIGN KEY (type, to_status) REFERENCES status(type, name),
  CHECK (
    (kind = 'field'           AND field IS NOT NULL) OR
    (kind = 'min_items'       AND field IS NOT NULL AND n IS NOT NULL) OR
    (kind = 'out_edge'        AND dep_kind IS NOT NULL) OR
    (kind = 'in_edges_closed' AND dep_kind IS NOT NULL)
  )
);
CREATE INDEX requirement_lookup ON requirement(type, to_status);

-- ============================================================
-- Data
-- ============================================================

CREATE TABLE issue (
  id          TEXT PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES project(id),
  type        TEXT NOT NULL REFERENCES issue_type(name),
  status      TEXT NOT NULL,
  resolution  TEXT,
  title       TEXT NOT NULL CHECK (length(title) > 0),
  context     TEXT NOT NULL CHECK (length(context) > 0),
  outcome     TEXT,
  owner       TEXT REFERENCES actor(id),
  links       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(links) AND json_type(links) = 'array'),
  tags        TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags) AND json_type(tags) = 'array'),
  fields      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(fields) AND json_type(fields) = 'object'),
  created_by  TEXT NOT NULL REFERENCES actor(id),
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  closed_at   TEXT,
  FOREIGN KEY (type, status)     REFERENCES status(type, name),
  FOREIGN KEY (type, resolution) REFERENCES resolution(type, name)
);
CREATE INDEX issue_project_status ON issue(project_id, status);
CREATE INDEX issue_type_status    ON issue(type, status);
CREATE INDEX issue_updated        ON issue(updated_at);

CREATE TABLE dep (
  src         TEXT NOT NULL REFERENCES issue(id),
  dst         TEXT NOT NULL REFERENCES issue(id),
  kind        TEXT NOT NULL REFERENCES dep_kind(name),
  created_by  TEXT NOT NULL REFERENCES actor(id),
  created_at  TEXT NOT NULL,
  PRIMARY KEY (src, dst, kind),
  CHECK (src <> dst)
);
CREATE INDEX dep_by_dst ON dep(dst, kind);

CREATE TABLE claim (
  id              INTEGER PRIMARY KEY,
  issue_id        TEXT NOT NULL REFERENCES issue(id),
  actor           TEXT NOT NULL REFERENCES actor(id),
  claimed_at      TEXT NOT NULL,
  lease_until     TEXT,
  worktree        TEXT,
  host            TEXT,
  agent           TEXT,
  terminal        TEXT,
  released_at     TEXT,
  release_reason  TEXT CHECK (release_reason IN ('closed', 'unclaimed', 'expired', 'stolen')),
  CHECK ((released_at IS NULL) = (release_reason IS NULL))
);
CREATE UNIQUE INDEX claim_one_active ON claim(issue_id) WHERE released_at IS NULL;
CREATE INDEX claim_active_lease ON claim(lease_until) WHERE released_at IS NULL;
CREATE INDEX claim_by_actor ON claim(actor, released_at);

CREATE TABLE event (
  seq       INTEGER PRIMARY KEY,
  issue_id  TEXT REFERENCES issue(id),
  actor     TEXT NOT NULL REFERENCES actor(id),
  at        TEXT NOT NULL,
  op        TEXT NOT NULL CHECK (op IN (
              'create', 'set', 'move', 'claim', 'release', 'close',
              'dep_add', 'dep_rm', 'note', 'schema')),
  payload   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload))
);
CREATE INDEX event_by_issue ON event(issue_id, seq);

CREATE TRIGGER event_no_update BEFORE UPDATE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
CREATE TRIGGER event_no_delete BEFORE DELETE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;

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

-- ============================================================
-- Full-text search (title, context, outcome, notes)
-- ============================================================

CREATE VIRTUAL TABLE search USING fts5(
  issue_id UNINDEXED, source UNINDEXED, body,
  tokenize = 'porter unicode61'
);

CREATE TRIGGER issue_fts_insert AFTER INSERT ON issue BEGIN
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.id, 'issue', NEW.title || ' ' || NEW.context || ' ' || coalesce(NEW.outcome, ''));
END;
CREATE TRIGGER issue_fts_update AFTER UPDATE OF title, context, outcome ON issue BEGIN
  DELETE FROM search WHERE issue_id = NEW.id AND source = 'issue';
  INSERT INTO search (issue_id, source, body)
  VALUES (NEW.id, 'issue', NEW.title || ' ' || NEW.context || ' ' || coalesce(NEW.outcome, ''));
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
SELECT i.*, s.category AS status_category, c.actor AS claimant, c.lease_until
FROM issue i
JOIN status s ON s.type = i.type AND s.name = i.status
LEFT JOIN active_claim c ON c.issue_id = i.id;

CREATE VIEW ready AS
SELECT i.* FROM issue_view i
WHERE i.status_category = 'open'
  AND i.claimant IS NULL
  AND NOT EXISTS (
    SELECT 1
    FROM dep d
    JOIN dep_kind k ON k.name = d.kind AND k.blocks_ready = 1
    JOIN issue_view b ON b.id = d.src
    WHERE d.dst = i.id AND b.status_category <> 'closed'
  );

CREATE VIEW milestone_progress AS
SELECT m.id,
       count(b.id)                                              AS total,
       count(b.id) FILTER (WHERE b.status_category = 'closed') AS closed
FROM issue_view m
LEFT JOIN dep d ON d.dst = m.id AND d.kind = 'blocks'
LEFT JOIN issue_view b ON b.id = d.src
WHERE m.type = 'milestone'
GROUP BY m.id;
