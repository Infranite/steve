-- 11: channel poll cursors (Feishu self poller, feishu-lark-cli.md §2.3, slice F8).
-- Rust Hub only; additive, nothing existing is altered.

-- 每个轮询连接、每个会话或话题一行：上次读到哪里
CREATE TABLE IF NOT EXISTS chan_cursors (
  connection_id TEXT NOT NULL REFERENCES connections(id), workspace_id TEXT NOT NULL,
  container TEXT NOT NULL,                     -- 会话 chat_id（oc_…）或话题 thread_id（omt_…）
  kind TEXT NOT NULL DEFAULT 'chat',           -- chat|thread
  chat_id TEXT NOT NULL,                       -- 所在会话（kind=chat 时等于 container）
  cursor INTEGER,                              -- 读到的最大 create_time（毫秒）；NULL = 还没读成功过（第一次读只记游标）
  boundary TEXT NOT NULL DEFAULT '[]',         -- create_time 正好等于 cursor 的消息 id，下次不再处理
  last_new INTEGER,                            -- 上次读到新消息的时间（话题按它判断是否还活跃）
  polled INTEGER,                              -- 上次读这个容器的时间
  error TEXT, error_at INTEGER,                -- 读不到（例如机器人不在的群，飞书不提供）
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  PRIMARY KEY (connection_id, container));
CREATE INDEX IF NOT EXISTS chan_cursors_kind ON chan_cursors(connection_id, kind, last_new);
