-- 29: a GitHub write you approved (open a PR, merge, comment) is a row from the answer's
-- transaction on, not a task in memory (Y2): pending → running → done | failed, or unknown when
-- GitHub's answer was lost (then reconciled with what GitHub shows before anything is sent again).
-- A restart turns `running` into `unknown`. CREATE … IF NOT EXISTS only, so it can run twice.
CREATE TABLE IF NOT EXISTS gh_effects (
  id TEXT PRIMARY KEY,                          -- ghe_…
  workspace_id TEXT NOT NULL,
  decision_id TEXT NOT NULL UNIQUE,             -- the answered gh_pr_open | gh_pr_merge | gh_comment
  kind TEXT NOT NULL,                           -- open | merge | comment
  state TEXT NOT NULL,                          -- pending | running | done | failed | unknown
  attempts INTEGER NOT NULL DEFAULT 0,
  next_at INTEGER,                              -- when to try (again); NULL: not by itself (a person decides)
  last_error TEXT,
  result TEXT NOT NULL DEFAULT '{}',            -- what happened on GitHub: {"pr":n} | {"comment":id} | {"merged":sha}
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS gh_effects_due ON gh_effects(next_at) WHERE state IN ('pending', 'unknown');
