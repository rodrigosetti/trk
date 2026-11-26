-- Recent executions are a bounded journal, separate from append-only lifecycle events.
CREATE TABLE check_run (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  issue_id TEXT NOT NULL REFERENCES issue(id),
  result TEXT NOT NULL CHECK (result IN ('passed', 'retry', 'woken', 'failed', 'skipped')),
  at TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  exit INTEGER,
  timed_out INTEGER NOT NULL,
  output TEXT NOT NULL,
  output_truncated INTEGER NOT NULL
);
