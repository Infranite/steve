-- Migrate workspace permission policies to the three-level Permissions model
-- (`ask`, `auto`, `full_access`) and record the applied permission mode on each
-- counted logical turn (`approval_turns.mode`).
CREATE TABLE approval_policies_new (
  workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id),
  mode TEXT NOT NULL CHECK(mode IN ('ask','auto','full_access')),
  revision INTEGER NOT NULL CHECK(revision > 0),
  actor TEXT NOT NULL
);

INSERT INTO approval_policies_new (workspace_id, mode, revision, actor)
SELECT
  workspace_id,
  CASE mode
    WHEN 'ask_owner' THEN 'ask'
    WHEN 'harness_auto' THEN 'auto'
    WHEN 'ask' THEN 'ask'
    WHEN 'auto' THEN 'auto'
    WHEN 'full_access' THEN 'full_access'
    ELSE 'ask'
  END,
  revision,
  actor
FROM approval_policies;

DROP TABLE approval_policies;
PRAGMA legacy_alter_table=ON;
ALTER TABLE approval_policies_new RENAME TO approval_policies;
PRAGMA legacy_alter_table=OFF;

-- Existing rows in approval_turns were recorded exclusively for native auto turns.
ALTER TABLE approval_turns
  ADD COLUMN mode TEXT NOT NULL DEFAULT 'auto'
  CHECK(mode IN ('ask','auto','full_access'));
