-- Default existing issues to normal priority; priority is stored with shared fields.
UPDATE issue
SET fields = json_set(fields, '$.priority', 'P2')
WHERE json_type(fields, '$.priority') IS NULL;
