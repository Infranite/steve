-- Keep the installation-wide daily backup policy separately from workspace settings.
CREATE TABLE backup_policy (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    keep_days INTEGER NOT NULL CHECK (keep_days BETWEEN 1 AND 365)
);

WITH legacy(value) AS (
    SELECT CASE WHEN json_valid(value) THEN value ELSE '{}' END
    FROM settings WHERE workspace_id = 'default' AND key = 'backup.daily'
)
INSERT INTO backup_policy (singleton, enabled, keep_days)
SELECT 1, json_extract(value, '$.enabled'), json_extract(value, '$.keep_days')
FROM legacy
WHERE json_type(value, '$.enabled') IN ('true', 'false')
  AND json_type(value, '$.keep_days') = 'integer'
  AND json_extract(value, '$.keep_days') BETWEEN 1 AND 365;

DELETE FROM settings WHERE workspace_id = 'default' AND key = 'backup.daily';
