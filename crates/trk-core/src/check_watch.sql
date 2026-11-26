-- Watchers remain visible between queue passes and do not own the queue lock.
CREATE TABLE check_watch (
  pid           INTEGER NOT NULL,
  host          TEXT NOT NULL DEFAULT '',
  started_at    TEXT NOT NULL,
  heartbeat_at  TEXT NOT NULL,
  PRIMARY KEY (pid, host)
);
