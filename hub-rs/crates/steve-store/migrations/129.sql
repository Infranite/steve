-- Migrate bots.perm_mode from harness-specific (Claude Code) mode names to Steve's canonical
-- PermissionMode vocabulary (`plan`, `ask`, `accept_edits`, `auto`, `full_access`), and change
-- the column default from `'default'` to `'ask'`.
--
-- Mapping:
--   'plan'                              -> 'plan'
--   'default' | 'ask'                   -> 'ask'
--   'acceptEdits' | 'accept_edits'      -> 'accept_edits'
--   'auto'                              -> 'auto'
--   'bypassPermissions' | 'full_access' -> 'full_access'
--   any other retired/unknown value     -> 'ask'
CREATE TABLE bots_new (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT '',
  main INTEGER NOT NULL DEFAULT 0,
  harness TEXT NOT NULL,
  model TEXT,
  rules TEXT NOT NULL DEFAULT '',
  accept_mode TEXT NOT NULL DEFAULT 'manual',
  perm_mode TEXT NOT NULL DEFAULT 'ask',
  project_dir TEXT,
  worktree INTEGER NOT NULL DEFAULT 0,
  enabled INTEGER NOT NULL DEFAULT 1,
  revision INTEGER NOT NULL DEFAULT 1,
  updated INTEGER NOT NULL,
  persona TEXT NOT NULL DEFAULT '',
  proactive INTEGER NOT NULL DEFAULT 1,
  avatar TEXT NOT NULL DEFAULT '{}',
  default_project TEXT,
  prompt TEXT,
  device TEXT,
  deleted INTEGER NOT NULL DEFAULT 0,
  owner TEXT,
  reasoning_effort TEXT,
  specialties TEXT NOT NULL DEFAULT '[]'
);

INSERT INTO bots_new (
  id, workspace_id, name, role, main, harness, model, rules, accept_mode, perm_mode,
  project_dir, worktree, enabled, revision, updated, persona, proactive, avatar,
  default_project, prompt, device, deleted, owner, reasoning_effort, specialties
)
SELECT
  id, workspace_id, name, role, main, harness, model, rules, accept_mode,
  CASE perm_mode
    WHEN 'plan' THEN 'plan'
    WHEN 'default' THEN 'ask'
    WHEN 'ask' THEN 'ask'
    WHEN 'acceptEdits' THEN 'accept_edits'
    WHEN 'accept_edits' THEN 'accept_edits'
    WHEN 'auto' THEN 'auto'
    WHEN 'bypassPermissions' THEN 'full_access'
    WHEN 'full_access' THEN 'full_access'
    ELSE 'ask'
  END,
  project_dir, worktree, enabled, revision, updated, persona, proactive, avatar,
  default_project, prompt, device, deleted, owner, reasoning_effort, specialties
FROM bots;

DROP TABLE bots;
PRAGMA legacy_alter_table=ON;
ALTER TABLE bots_new RENAME TO bots;
PRAGMA legacy_alter_table=OFF;
