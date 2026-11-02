-- 016: thread routing (modules/thread-routing.md §7.1). New tables only, all IF NOT EXISTS, nothing existing is
-- altered, and nothing here depends on 013–015.
CREATE TABLE IF NOT EXISTS route_log (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  message_id TEXT NOT NULL,
  reply_id TEXT,
  model TEXT, model_kind TEXT NOT NULL,
  kind TEXT NOT NULL,
  confidence REAL, signals TEXT NOT NULL DEFAULT '[]', guard TEXT,
  fallback INTEGER NOT NULL DEFAULT 0, item_id TEXT, ms INTEGER NOT NULL,
  override TEXT,
  override_at INTEGER, outcome TEXT,
  created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS route_log_ws ON route_log(workspace_id, created);
CREATE UNIQUE INDEX IF NOT EXISTS route_log_msg ON route_log(message_id);

CREATE TABLE IF NOT EXISTS thread_runs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  message_id TEXT NOT NULL, reply_id TEXT, prev_run TEXT,
  user_id TEXT, project_id TEXT, model TEXT, claude_session TEXT,
  status TEXT NOT NULL,
  token_hash TEXT,
  tools INTEGER NOT NULL DEFAULT 0, turns INTEGER NOT NULL DEFAULT 0,
  first_text_ms INTEGER, error TEXT,
  created INTEGER NOT NULL, started INTEGER, ended INTEGER);
CREATE INDEX IF NOT EXISTS thread_runs_ws ON thread_runs(workspace_id, status, created);

CREATE TABLE IF NOT EXISTS thread_items (
  item_id TEXT PRIMARY KEY REFERENCES items(id), workspace_id TEXT NOT NULL,
  origin_msg TEXT NOT NULL, reply_msg TEXT, from_question INTEGER NOT NULL DEFAULT 0,
  follow INTEGER NOT NULL DEFAULT 1, last_post INTEGER, created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS thread_items_ws ON thread_items(workspace_id, created);
