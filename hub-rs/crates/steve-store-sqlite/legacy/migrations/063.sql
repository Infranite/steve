-- 63: a Bot's skills: Markdown instructions its harness reads as `<name>/SKILL.md` (front matter
-- `name`, `description`) where its adapter declares it reads skills, written by the session's Node
-- before `session/new|load`. Only enabled ones are given. New table only.
CREATE TABLE IF NOT EXISTS bot_skills (
  id TEXT PRIMARY KEY,                          -- bskill_…
  workspace_id TEXT NOT NULL,                   -- where it was added
  bot_id TEXT NOT NULL REFERENCES bots(id),
  name TEXT NOT NULL,                           -- the skill's directory and front matter name
  description TEXT NOT NULL,                    -- when to use it: what the harness decides by
  content TEXT NOT NULL DEFAULT '',             -- the SKILL.md body (Markdown)
  enabled INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (bot_id, name));
