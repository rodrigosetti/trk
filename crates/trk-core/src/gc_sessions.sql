-- Earlier GC completion did not account for per-session files.
UPDATE issue SET gc_collected_at = NULL
WHERE gc_collected_at IS NOT NULL
  AND EXISTS (SELECT 1 FROM claim
              WHERE claim.issue_id = issue.id AND claim.actor LIKE 'agent:%');
