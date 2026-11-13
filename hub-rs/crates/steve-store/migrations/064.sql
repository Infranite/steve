-- 64: what an agent changed in the memories (`mem_changes`), so every change is listed by the run
-- that made it and can be undone or answered; and a candidate that replaces more than one memory
-- (`mem_supersedes`: a merge). New tables only.
CREATE TABLE IF NOT EXISTS mem_changes (
  id TEXT PRIMARY KEY,                          -- mch_…
  workspace_id TEXT NOT NULL,
  run_id TEXT NOT NULL,                         -- the item (or session) of the agent run that made it
  writer TEXT NOT NULL,                         -- the bot
  op TEXT NOT NULL,                             -- merge|resolve|flag|stale|about|source
  mem_id TEXT NOT NULL,                         -- the memory changed; merge: the new candidate
  targets TEXT NOT NULL DEFAULT '[]',           -- the other memories involved (merged, retired, in conflict)
  before TEXT NOT NULL DEFAULT '{}',            -- what undo puts back
  after TEXT NOT NULL DEFAULT '{}',
  reason TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL DEFAULT 'applied',        -- applied|undone; flag: open|kept|dismissed; merge follows its candidate
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS mem_changes_run ON mem_changes(workspace_id, run_id, created);
CREATE INDEX IF NOT EXISTS mem_changes_mem ON mem_changes(mem_id);
CREATE TABLE IF NOT EXISTS mem_supersedes (
  mem_id TEXT NOT NULL,                         -- the candidate
  old_id TEXT NOT NULL,                         -- a confirmed memory it replaces once confirmed
  PRIMARY KEY (mem_id, old_id));
