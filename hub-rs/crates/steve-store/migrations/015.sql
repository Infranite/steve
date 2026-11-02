-- 15: statistics (modules/stats.md §2.1, slice ST0). Rust Hub only; new tables only, all IF NOT EXISTS,
-- nothing existing is altered, and nothing here refers to the tables of 013/014.

-- 每个 prompt 请求一行（含 503 重试的那几次和被中断的），开始时插入、结束时补全
CREATE TABLE IF NOT EXISTS agent_turns (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  session_id TEXT NOT NULL, item_id TEXT, bot_id TEXT, project_id TEXT, harness TEXT NOT NULL,
  user_seq INTEGER,
  attempt INTEGER NOT NULL DEFAULT 0,
  started INTEGER NOT NULL, ended INTEGER,
  stop_reason TEXT,
  error TEXT, transient INTEGER NOT NULL DEFAULT 0,
  tools INTEGER NOT NULL DEFAULT 0, tool_errors INTEGER NOT NULL DEFAULT 0,
  tool_kinds TEXT NOT NULL DEFAULT '{}',
  permissions INTEGER NOT NULL DEFAULT 0,
  backfill INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS agent_turns_ws ON agent_turns(workspace_id, started);
CREATE INDEX IF NOT EXISTS agent_turns_item ON agent_turns(item_id, started);
CREATE INDEX IF NOT EXISTS agent_turns_open ON agent_turns(ended) WHERE ended IS NULL;

-- token 明细：agent 回合按模型一行，网关调用一行；token 列 NULL = 没上报
CREATE TABLE IF NOT EXISTS usage_records (
  id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL,
  ts INTEGER NOT NULL, day TEXT NOT NULL,
  source TEXT NOT NULL,
  purpose TEXT NOT NULL,
  harness TEXT NOT NULL,
  model TEXT NOT NULL DEFAULT '',
  turn_id TEXT, session_id TEXT, item_id TEXT, bot_id TEXT, project_id TEXT,
  input INTEGER, output INTEGER, cache_read INTEGER, cache_write INTEGER, reasoning INTEGER,
  cost_reported REAL,
  duration_ms INTEGER, ok INTEGER NOT NULL DEFAULT 1,
  attempts INTEGER NOT NULL DEFAULT 1,
  detail TEXT NOT NULL DEFAULT '{}');
CREATE INDEX IF NOT EXISTS usage_ws_day ON usage_records(workspace_id, day);
CREATE INDEX IF NOT EXISTS usage_item ON usage_records(item_id, ts);
CREATE INDEX IF NOT EXISTS usage_session ON usage_records(session_id, ts);

-- 事项的结构化节点（不再靠时间线文本）
CREATE TABLE IF NOT EXISTS item_marks (
  id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, item_id TEXT NOT NULL,
  ts INTEGER NOT NULL,
  kind TEXT NOT NULL,
  from_v TEXT, to_v TEXT,
  actor TEXT NOT NULL,
  ref TEXT,
  detail TEXT NOT NULL DEFAULT '{}', backfill INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS item_marks_item ON item_marks(item_id, ts);
CREATE INDEX IF NOT EXISTS item_marks_ws ON item_marks(workspace_id, kind, ts);

-- 按天汇总；与 usage_records / agent_turns 在同一个写事务里 UPSERT
CREATE TABLE IF NOT EXISTS stat_daily (
  workspace_id TEXT NOT NULL, day TEXT NOT NULL,
  source TEXT NOT NULL, purpose TEXT NOT NULL, harness TEXT NOT NULL, model TEXT NOT NULL,
  bot_id TEXT NOT NULL DEFAULT '', project_id TEXT NOT NULL DEFAULT '',
  input INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
  cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0, reasoning INTEGER NOT NULL DEFAULT 0,
  hit_base INTEGER NOT NULL DEFAULT 0,
  cost_reported REAL NOT NULL DEFAULT 0, calls INTEGER NOT NULL DEFAULT 0, failed_calls INTEGER NOT NULL DEFAULT 0,
  turns INTEGER NOT NULL DEFAULT 0, turn_ms INTEGER NOT NULL DEFAULT 0, turn_errors INTEGER NOT NULL DEFAULT 0,
  retries INTEGER NOT NULL DEFAULT 0, interrupted INTEGER NOT NULL DEFAULT 0,
  tools INTEGER NOT NULL DEFAULT 0, tool_errors INTEGER NOT NULL DEFAULT 0, tool_kinds TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (workspace_id, day, source, purpose, harness, model, bot_id, project_id));

-- 每件事项一行的事实表（1:1 扩展表，不改 items）
CREATE TABLE IF NOT EXISTS item_stats (
  item_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  created INTEGER NOT NULL, first_turn INTEGER, first_deliver INTEGER, last_deliver INTEGER,
  accepted INTEGER, done_at INTEGER, dropped_at INTEGER,
  lead_ms INTEGER, cycle_ms INTEGER, wait_ms INTEGER, agent_ms INTEGER,
  turns INTEGER NOT NULL DEFAULT 0, delivers INTEGER NOT NULL DEFAULT 0, reworks INTEGER NOT NULL DEFAULT 0,
  first_pass INTEGER,
  fails INTEGER NOT NULL DEFAULT 0, stalls INTEGER NOT NULL DEFAULT 0,
  releases INTEGER NOT NULL DEFAULT 0, release_fails INTEGER NOT NULL DEFAULT 0,
  decisions INTEGER NOT NULL DEFAULT 0, yolo_decided INTEGER NOT NULL DEFAULT 0,
  input INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
  cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0,
  cost_reported REAL NOT NULL DEFAULT 0, models TEXT NOT NULL DEFAULT '{}',
  dirty INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS item_stats_done ON item_stats(workspace_id, done_at);
CREATE INDEX IF NOT EXISTS item_stats_dirty ON item_stats(dirty) WHERE dirty = 1;
