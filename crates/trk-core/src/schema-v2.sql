-- Required issue types replace authored agent assignments. The migration hook
-- preserves routing in global type files, fills this column, then drops agent.
ALTER TABLE issue ADD COLUMN type TEXT NOT NULL DEFAULT 'code-change'
  CHECK (length(trim(type)) > 0);
