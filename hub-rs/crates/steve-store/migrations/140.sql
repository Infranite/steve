-- Canonicalize reasoning_effort to Steve's five levels (`default`, `low`, `medium`, `high`, `max`)
-- across bots, bot_fallback_surfaces, sessions, settings (models.*), bot_placements, and
-- runtime_sessions, and add sessions.effort_mapping to persist adapter mapping outcomes.
--
-- Deterministic legacy mapping:
--   minimal | low                                 -> low
--   medium | med                                  -> medium
--   high                                          -> high
--   xhigh | max | ultra                           -> max
--   null | empty | none | off | default | unknown -> default

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
  reasoning_effort TEXT NOT NULL DEFAULT 'default',
  specialties TEXT NOT NULL DEFAULT '[]'
);

INSERT INTO bots_new (
  id, workspace_id, name, role, main, harness, model, rules, accept_mode, perm_mode,
  enabled, revision, updated, persona, proactive, avatar,
  default_project, prompt, device, deleted, owner, reasoning_effort, specialties
)
SELECT
  id, workspace_id, name, role, main, harness, model, rules, accept_mode, perm_mode,
  enabled, revision, updated, persona, proactive, avatar,
  default_project, prompt, device, deleted, owner,
  CASE lower(trim(coalesce(reasoning_effort, '')))
    WHEN 'minimal' THEN 'low'
    WHEN 'low' THEN 'low'
    WHEN 'medium' THEN 'medium'
    WHEN 'med' THEN 'medium'
    WHEN 'high' THEN 'high'
    WHEN 'xhigh' THEN 'max'
    WHEN 'max' THEN 'max'
    WHEN 'ultra' THEN 'max'
    ELSE 'default'
  END,
  specialties
FROM bots;

DROP TABLE bots;
PRAGMA legacy_alter_table=ON;
ALTER TABLE bots_new RENAME TO bots;
PRAGMA legacy_alter_table=OFF;

CREATE TABLE bot_fallback_surfaces_new (
  bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  harness TEXT NOT NULL,
  model TEXT,
  reasoning_effort TEXT NOT NULL DEFAULT 'default',
  PRIMARY KEY (bot_id, ordinal)
);

INSERT INTO bot_fallback_surfaces_new (bot_id, ordinal, harness, model, reasoning_effort)
SELECT
  bot_id,
  ordinal,
  harness,
  model,
  CASE lower(trim(coalesce(reasoning_effort, '')))
    WHEN 'minimal' THEN 'low'
    WHEN 'low' THEN 'low'
    WHEN 'medium' THEN 'medium'
    WHEN 'med' THEN 'medium'
    WHEN 'high' THEN 'high'
    WHEN 'xhigh' THEN 'max'
    WHEN 'max' THEN 'max'
    WHEN 'ultra' THEN 'max'
    ELSE 'default'
  END
FROM bot_fallback_surfaces;

DROP TABLE bot_fallback_surfaces;
PRAGMA legacy_alter_table=ON;
ALTER TABLE bot_fallback_surfaces_new RENAME TO bot_fallback_surfaces;
PRAGMA legacy_alter_table=OFF;

UPDATE sessions
SET reasoning_effort = CASE lower(trim(coalesce(reasoning_effort, '')))
  WHEN 'minimal' THEN 'low'
  WHEN 'low' THEN 'low'
  WHEN 'medium' THEN 'medium'
  WHEN 'med' THEN 'medium'
  WHEN 'high' THEN 'high'
  WHEN 'xhigh' THEN 'max'
  WHEN 'max' THEN 'max'
  WHEN 'ultra' THEN 'max'
  ELSE 'default'
END;

ALTER TABLE sessions ADD COLUMN effort_mapping TEXT;

UPDATE settings
SET value = json_set(
  value,
  '$.effort',
  CASE lower(trim(coalesce(json_extract(value, '$.effort'), '')))
    WHEN 'minimal' THEN 'low'
    WHEN 'low' THEN 'low'
    WHEN 'medium' THEN 'medium'
    WHEN 'med' THEN 'medium'
    WHEN 'high' THEN 'high'
    WHEN 'xhigh' THEN 'max'
    WHEN 'max' THEN 'max'
    WHEN 'ultra' THEN 'max'
    ELSE 'default'
  END
)
WHERE key LIKE 'models.%'
  AND json_valid(value)
  AND json_type(value) = 'object'
  AND json_type(value, '$.effort') IS NOT NULL;

UPDATE bot_placements
SET fact = json_set(
  fact,
  '$.reasoning_effort',
  CASE lower(trim(coalesce(json_extract(fact, '$.reasoning_effort'), '')))
    WHEN 'minimal' THEN 'low'
    WHEN 'low' THEN 'low'
    WHEN 'medium' THEN 'medium'
    WHEN 'med' THEN 'medium'
    WHEN 'high' THEN 'high'
    WHEN 'xhigh' THEN 'max'
    WHEN 'max' THEN 'max'
    WHEN 'ultra' THEN 'max'
    ELSE 'default'
  END
)
WHERE json_valid(fact)
  AND json_type(fact) = 'object';

UPDATE runtime_sessions
SET desired_config = json_set(
  desired_config,
  '$.choice.effort',
  CASE lower(trim(coalesce(json_extract(desired_config, '$.choice.effort'), '')))
    WHEN 'minimal' THEN 'low'
    WHEN 'low' THEN 'low'
    WHEN 'medium' THEN 'medium'
    WHEN 'med' THEN 'medium'
    WHEN 'high' THEN 'high'
    WHEN 'xhigh' THEN 'max'
    WHEN 'max' THEN 'max'
    WHEN 'ultra' THEN 'max'
    ELSE 'default'
  END
)
WHERE desired_config IS NOT NULL
  AND json_valid(desired_config)
  AND json_type(desired_config, '$.choice.effort') IS NOT NULL
  AND json_type(desired_config, '$.choice.effort') != 'null';
