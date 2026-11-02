-- 6: signals & automation (incl. schedules): source settings, external events, flows, versions, triggers, runs, steps, waits, corrections, digest.
-- Spec: steve-rs-plan/modules/signals-automation.md §1 (schedules.md §2 adds no tables), DDL verbatim. Rust Hub only; additive, nothing existing is altered.

-- 每个事件源连接的接收设置（事件源的设置回到各自的账号）
CREATE TABLE IF NOT EXISTS source_settings (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id), workspace_id TEXT NOT NULL,
  paused INTEGER NOT NULL DEFAULT 0,           -- 暂停期间不接收，恢复后不补收
  triage_bot TEXT REFERENCES bots(id),         -- “先交给谁判断”，NULL = 主 Bot
  types TEXT NOT NULL DEFAULT '{}',            -- {"mail.new":{"on":true},"cal.before":{"on":true,"param":"30m"}}
  scopes TEXT NOT NULL DEFAULT '[]',           -- [{"ext_id":"INBOX","name":"收件箱","mode":"receive|record|ignore"}]
  cursor TEXT,                                 -- 适配器的增量同步游标
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS ext_events (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  connection_id TEXT,                          -- 定时、手动、事项事件没有
  source TEXT NOT NULL,                        -- mail|cal|im|github|webhook|time|item|manual
  type TEXT NOT NULL,                          -- mail.new、cal.before、im.mention、github.check_failed、webhook.alert、item.done……
  external_id TEXT, dedupe_key TEXT NOT NULL,
  thread_key TEXT, group_key TEXT,             -- 同一往来 / 同类合并
  scope_ext_id TEXT, space_id TEXT, message_id TEXT,                -- 渠道消息对应的空间和消息（迁移 006）
  actor_account_id TEXT, actor_party_id TEXT,
  occurred INTEGER NOT NULL, received INTEGER NOT NULL,
  summary TEXT NOT NULL,                       -- 一行摘要，永久保留（≤200 字）
  payload TEXT,                                -- 原文（JSON），保留期限到了置 NULL
  payload_purged INTEGER,
  visibility TEXT NOT NULL DEFAULT 'workspace',
  -- 分诊结果（信号页直接读，避免每行 join 运行）
  outcome TEXT NOT NULL DEFAULT 'pending',     -- pending|need|item|linked|digest|ignored|recorded|failed
  outcome_label TEXT NOT NULL DEFAULT '',      -- “等你确认回复”“建了事项”“已忽略”……
  reason TEXT NOT NULL DEFAULT '',             -- Bot 判断给的理由
  handled_by TEXT, flow_id TEXT, run_id TEXT, work_item_id TEXT, decision_id TEXT,
  UNIQUE (workspace_id, dedupe_key));
CREATE INDEX IF NOT EXISTS ext_events_list ON ext_events(workspace_id, received);
CREATE INDEX IF NOT EXISTS ext_events_group ON ext_events(workspace_id, group_key, received);
CREATE INDEX IF NOT EXISTS ext_events_purge ON ext_events(payload_purged, received);

CREATE TABLE IF NOT EXISTS flows (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  kind TEXT NOT NULL DEFAULT 'flow',           -- flow|schedule|response（渠道响应规则转成的流程）
  name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 0,
  graph TEXT NOT NULL,                         -- 当前版本的图（§3.1）
  version INTEGER NOT NULL DEFAULT 1,
  origin TEXT NOT NULL,                        -- me|steve|plugin:<id>|template:<id>|correction
  origin_label TEXT NOT NULL DEFAULT '',       -- “你画的”“Steve 起草，你确认”“GitHub 插件模板”
  schedule TEXT,                               -- kind=schedule 的表单字段（§5）
  deleted INTEGER NOT NULL DEFAULT 0,
  created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS flows_ws ON flows(workspace_id, kind, deleted);

CREATE TABLE IF NOT EXISTS flow_versions (
  flow_id TEXT NOT NULL REFERENCES flows(id), workspace_id TEXT NOT NULL, version INTEGER NOT NULL,
  graph TEXT NOT NULL, author TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL,
  PRIMARY KEY (flow_id, version));

-- 触发器索引：保存流程时从图里的 trigger 节点重建，分发只查这张表
CREATE TABLE IF NOT EXISTS flow_triggers (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, flow_id TEXT NOT NULL REFERENCES flows(id), node_id TEXT NOT NULL,
  kind TEXT NOT NULL,                          -- event|message|time|item|manual
  source TEXT, types TEXT NOT NULL DEFAULT '[]', connections TEXT NOT NULL DEFAULT '[]', scopes TEXT NOT NULL DEFAULT '[]', spaces TEXT NOT NULL DEFAULT '[]',
  cron TEXT, tz TEXT, next_at INTEGER, missed TEXT,          -- missed: catch_up_once|skip
  enabled INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS flow_triggers_event ON flow_triggers(workspace_id, kind, source, enabled);
CREATE INDEX IF NOT EXISTS flow_triggers_time ON flow_triggers(kind, enabled, next_at);

CREATE TABLE IF NOT EXISTS flow_runs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, flow_id TEXT NOT NULL, flow_version INTEGER NOT NULL,
  trigger_kind TEXT NOT NULL, event_id TEXT, dry_run INTEGER NOT NULL DEFAULT 0, parent_run TEXT, depth INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL,                        -- running|waiting|done|failed|cancelled
  outcome TEXT, outcome_label TEXT NOT NULL DEFAULT '',
  cursor TEXT,                                 -- 下一个要执行的节点 id（恢复用）
  context TEXT NOT NULL DEFAULT '{}',          -- {event, steps:{<id>:output}}，敏感值打码后才写
  error TEXT, retry_at INTEGER, attempts INTEGER NOT NULL DEFAULT 0,
  work_item_id TEXT, cost REAL NOT NULL DEFAULT 0,
  started INTEGER NOT NULL, ended INTEGER);
CREATE INDEX IF NOT EXISTS flow_runs_flow ON flow_runs(workspace_id, flow_id, started);
CREATE INDEX IF NOT EXISTS flow_runs_retry ON flow_runs(status, retry_at);

CREATE TABLE IF NOT EXISTS flow_run_steps (
  run_id TEXT NOT NULL REFERENCES flow_runs(id), seq INTEGER NOT NULL, workspace_id TEXT NOT NULL,
  node_id TEXT NOT NULL, status TEXT NOT NULL, -- ok|skipped|waiting|failed
  branch TEXT, input TEXT, output TEXT, ms REAL NOT NULL DEFAULT 0, error TEXT, started INTEGER NOT NULL,
  PRIMARY KEY (run_id, seq));

CREATE TABLE IF NOT EXISTS flow_waits (
  run_id TEXT NOT NULL REFERENCES flow_runs(id), node_id TEXT NOT NULL, workspace_id TEXT NOT NULL,
  mode TEXT NOT NULL,                          -- delay|until|event|decision
  until INTEGER, match TEXT, decision_id TEXT, created INTEGER NOT NULL,
  PRIMARY KEY (run_id, node_id));
CREATE INDEX IF NOT EXISTS flow_waits_until ON flow_waits(until);

CREATE TABLE IF NOT EXISTS signal_corrections (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, event_id TEXT NOT NULL,
  scope TEXT NOT NULL,                         -- one|group
  action TEXT NOT NULL,                        -- remind|ignore|to_item
  flow_id TEXT, changeset_id TEXT, mem_id TEXT, work_item_id TEXT,
  created_by TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS digest_entries (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, event_id TEXT, run_id TEXT,
  text TEXT NOT NULL, taken INTEGER, created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS digest_open ON digest_entries(workspace_id, taken, created);
