-- 14: YOLO (modules/yolo.md §6, with the user's decisions of 2026-11-01: no expiry, no budgets).
-- New tables only, all `IF NOT EXISTS`, nothing existing is altered: it does not depend on what 013 (a
-- comment-only placeholder) or anything after 012 creates, and running it twice changes nothing.
CREATE TABLE IF NOT EXISTS yolo_scopes (
  workspace_id TEXT NOT NULL,
  object_type TEXT NOT NULL,                   -- workspace|project|item
  object_id TEXT NOT NULL,                     -- workspace 行 = workspace_id
  mode TEXT NOT NULL,                          -- on|off|inherit（workspace 只有 on|off）
  paused TEXT,                                 -- 熔断 {code, reason, at}，只用于 workspace 行
  reason TEXT NOT NULL DEFAULT '',             -- 例如“没有 worktree”
  set_by TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, object_type, object_id));

CREATE TABLE IF NOT EXISTS yolo_windows (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,           -- yw_…
  started INTEGER NOT NULL, ended INTEGER,
  ended_why TEXT,                                            -- off|tripped
  include_open INTEGER NOT NULL DEFAULT 0,                   -- 打开时选了“也处理现有的待处理卡”
  counters TEXT NOT NULL DEFAULT '{}');                      -- 熔断、频率用的计数缓存
CREATE INDEX IF NOT EXISTS yolo_windows_ws ON yolo_windows(workspace_id, started);

CREATE TABLE IF NOT EXISTS yolo_actions (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, window_id TEXT,   -- ya_…
  decision_id TEXT, item_id TEXT, session_id TEXT,
  kind TEXT NOT NULL, subject_type TEXT NOT NULL,
  rule TEXT NOT NULL,                          -- tool.worktree|action.local|accept.checks|…|limit.<n>|presence
  outcome TEXT NOT NULL,                       -- scheduled|decided|declined|muted|cancelled|failed
  choice TEXT, text TEXT, reason TEXT NOT NULL,
  checks TEXT NOT NULL DEFAULT '{}', cost REAL,
  due_at INTEGER, done_at INTEGER,
  undo TEXT,                                   -- {kind: decision|rollback|recall|reanswer|pause, …}
  undone INTEGER, created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS yolo_actions_ws ON yolo_actions(workspace_id, created);
CREATE INDEX IF NOT EXISTS yolo_actions_item ON yolo_actions(item_id, created);
CREATE INDEX IF NOT EXISTS yolo_actions_due ON yolo_actions(outcome, due_at);
