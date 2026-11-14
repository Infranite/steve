-- 67: the digest base (chat-knowledge phase B): conversation profiles and digest settings, the
-- per-conversation cursor, the durable digest packet queue, conversation summaries, alert rollups,
-- matters (事) with their segments across conversations, personal todos (我的待办), the message
-- ranges a memory came from, and the log of every digest write (undo). New tables only.
--
-- A conversation is named by `conv` = `<type>:<id>`, the same (type, id) a memory or link points at:
-- `space:<spaces.id>` (a group, DM or topic), `mail_thread:<mail_threads.id>`; connectors add more
-- types later. Every id in these tables is Steve's own (message rows, parties, items), never a
-- model's.

-- 会话画像与梳理设置. NULL settings follow the kind's defaults (steve-digest `profile::defaults`).
CREATE TABLE IF NOT EXISTS conv_profiles (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL,
  kind TEXT,                                   -- project|team|feed|dm|mail|pr|issue; NULL = not judged yet
  kind_source TEXT NOT NULL DEFAULT 'rule',    -- rule|model|you
  window_size INTEGER, quiet_ms INTEGER, max_wait_ms INTEGER,
  depth TEXT,                                  -- full|summary|alerts|record|paused
  daily_budget INTEGER,                        -- estimated tokens of packets per day
  priority INTEGER,                            -- 0..3
  notify TEXT,                                 -- now|digest|record
  notify_source TEXT NOT NULL DEFAULT 'default', -- default|learned|you
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, conv));

-- 每会话游标: the last message cut into a window (`at`, `msg_id`), what is spent today.
CREATE TABLE IF NOT EXISTS digest_cursors (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL,
  at INTEGER NOT NULL, msg_id TEXT NOT NULL DEFAULT '',
  since INTEGER NOT NULL,                     -- fixed initial lookback floor; never slides on retry
  windows INTEGER NOT NULL DEFAULT 0,          -- windows cut so far
  day TEXT NOT NULL DEFAULT '', spent INTEGER NOT NULL DEFAULT 0,   -- local day, tokens queued that day
  updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, conv));

-- A message can arrive late (or be pulled from history) with an older provider timestamp. Receipts,
-- committed with the cursor and packet, prevent a timestamp high-water mark from silently dropping it.
CREATE TABLE IF NOT EXISTS digest_receipts (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, message_id TEXT NOT NULL,
  seq INTEGER NOT NULL, created INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, conv, message_id));

-- 梳理包: one window of one conversation, with its deterministic facts and candidates.
CREATE TABLE IF NOT EXISTS digest_packets (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, conv TEXT NOT NULL,
  seq INTEGER NOT NULL,                        -- the conversation's window number
  kind TEXT NOT NULL DEFAULT 'window',         -- window|alerts (only the anomalies of an alert window)
  trigger TEXT NOT NULL,                       -- size|quiet|max_wait|manual
  first_id TEXT NOT NULL, last_id TEXT NOT NULL, first_at INTEGER NOT NULL, last_at INTEGER NOT NULL,
  count INTEGER NOT NULL, tokens INTEGER NOT NULL, priority INTEGER NOT NULL DEFAULT 1,
  facts TEXT NOT NULL, candidates TEXT NOT NULL DEFAULT '[]',
  state TEXT NOT NULL DEFAULT 'queued',        -- queued|leased|done|failed
  batch_id TEXT, lease_until INTEGER, attempts INTEGER NOT NULL DEFAULT 0, error TEXT,
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (workspace_id, conv, seq));
CREATE INDEX IF NOT EXISTS digest_packets_state ON digest_packets(workspace_id, state, priority, created);

-- 分块: a packet's text cut by token budget at reply-chain/topic/time-gap boundaries.
CREATE TABLE IF NOT EXISTS digest_chunks (
  packet_id TEXT NOT NULL REFERENCES digest_packets(id) ON DELETE CASCADE, n INTEGER NOT NULL,
  first_pos INTEGER NOT NULL, last_pos INTEGER NOT NULL, overlap INTEGER NOT NULL DEFAULT 0,
  tokens INTEGER NOT NULL, text TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued',        -- queued|done|failed
  attempts INTEGER NOT NULL DEFAULT 0, error TEXT, updated INTEGER NOT NULL,
  PRIMARY KEY (packet_id, n));

-- 段: what the digest Bot wrote for positions of a packet (summary, tags, the matter).
CREATE TABLE IF NOT EXISTS digest_segments (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, packet_id TEXT NOT NULL, conv TEXT NOT NULL,
  chunk INTEGER NOT NULL DEFAULT 0, first_pos INTEGER NOT NULL, last_pos INTEGER NOT NULL,
  first_id TEXT NOT NULL, last_id TEXT NOT NULL, first_at INTEGER NOT NULL, last_at INTEGER NOT NULL,
  summary TEXT NOT NULL, tags TEXT NOT NULL DEFAULT '[]', matter_id TEXT,
  chains TEXT NOT NULL DEFAULT '[]',           -- the reply chains and topics its messages belong to
  parties TEXT NOT NULL DEFAULT '[]',          -- the party ids of its senders and @ parties
  writer TEXT NOT NULL, created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS digest_segments_packet ON digest_segments(packet_id, chunk, first_pos);
CREATE INDEX IF NOT EXISTS digest_segments_matter ON digest_segments(matter_id, first_at);
CREATE INDEX IF NOT EXISTS digest_segments_conv ON digest_segments(workspace_id, conv, first_at);

-- 会话摘要: rewritten once per window; every version kept (undo, history).
CREATE TABLE IF NOT EXISTS conv_summaries (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, version INTEGER NOT NULL,
  text TEXT NOT NULL, rules TEXT NOT NULL DEFAULT '',   -- rules: the alert rollup, written by code
  packet_id TEXT, writer TEXT NOT NULL, created INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, conv, version));

-- 告警汇总: per alert category of a feed conversation.
CREATE TABLE IF NOT EXISTS alert_rollups (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, category TEXT NOT NULL,
  fired INTEGER NOT NULL DEFAULT 0, recovered INTEGER NOT NULL DEFAULT 0,
  open INTEGER NOT NULL DEFAULT 0,             -- 1: the last one of this category is not recovered
  handlers TEXT NOT NULL DEFAULT '[]',         -- party ids that answered in the thread
  first_at INTEGER NOT NULL, last_at INTEGER NOT NULL, last_id TEXT NOT NULL,
  PRIMARY KEY (workspace_id, conv, category));

-- 事: one thing that happens across conversations.
CREATE TABLE IF NOT EXISTS matters (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  title TEXT NOT NULL, summary TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL DEFAULT 'open',          -- open|done|dropped|merged
  merged_into TEXT,
  created_by TEXT NOT NULL,                    -- steve | the user | a bot
  first_at INTEGER, last_at INTEGER,
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS matters_ws ON matters(workspace_id, state, last_at);
CREATE TABLE IF NOT EXISTS matter_parties (
  matter_id TEXT NOT NULL REFERENCES matters(id) ON DELETE CASCADE, party_id TEXT NOT NULL,
  messages INTEGER NOT NULL DEFAULT 0, last_at INTEGER NOT NULL,
  PRIMARY KEY (matter_id, party_id));
CREATE INDEX IF NOT EXISTS matter_parties_party ON matter_parties(party_id);
-- split, merge, 不是同一件 and the other changes of a matter
CREATE TABLE IF NOT EXISTS matter_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, matter_id TEXT NOT NULL,
  op TEXT NOT NULL,                            -- create|assign|unassign|merge|split|not_same|link|unlink|state|rename
  other TEXT,                                  -- the other matter, segment or linked object
  detail TEXT NOT NULL DEFAULT '{}', actor TEXT NOT NULL, created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS matter_events_matter ON matter_events(matter_id, created);
CREATE TABLE IF NOT EXISTS matter_not_same (
  workspace_id TEXT NOT NULL, a TEXT NOT NULL, b TEXT NOT NULL,   -- a < b
  actor TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (workspace_id, a, b));
CREATE VIRTUAL TABLE IF NOT EXISTS matters_fts USING fts5(
  seg, tokenize = 'unicode61 remove_diacritics 2', content = '', contentless_delete = 1);
-- the fts rowid of each matter (`matters` has no integer key VACUUM keeps)
CREATE TABLE IF NOT EXISTS matters_fts_rows (
  row INTEGER PRIMARY KEY, matter_id TEXT NOT NULL UNIQUE);

-- 我的待办: what you yourself are to do (items stay the work handed to Bots).
CREATE TABLE IF NOT EXISTS todos (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, owner TEXT NOT NULL,
  text TEXT NOT NULL, matter_id TEXT,
  due INTEGER, due_quote TEXT,                 -- only when a message states it: its words
  state TEXT NOT NULL DEFAULT 'open',          -- open|done|dropped|handed (to a Bot, as item_id)
  item_id TEXT,
  source TEXT NOT NULL,                        -- steve|you
  writer TEXT NOT NULL, packet_id TEXT,
  created INTEGER NOT NULL, updated INTEGER NOT NULL, closed INTEGER);
CREATE INDEX IF NOT EXISTS todos_ws ON todos(workspace_id, owner, state, created);
CREATE TABLE IF NOT EXISTS todo_refs (
  todo_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE, conv TEXT NOT NULL, message_id TEXT NOT NULL,
  PRIMARY KEY (todo_id, conv, message_id));
CREATE TABLE IF NOT EXISTS todo_parties (
  todo_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE, party_id TEXT NOT NULL,
  PRIMARY KEY (todo_id, party_id));

-- 记忆索引: the message ranges a memory came from (its conversations, people and matters are
-- `mem_about` rows of types space / mail_thread / party / matter).
CREATE TABLE IF NOT EXISTS mem_ranges (
  mem_id TEXT NOT NULL REFERENCES mems(id) ON DELETE CASCADE, workspace_id TEXT NOT NULL,
  conv TEXT NOT NULL, first_id TEXT NOT NULL, last_id TEXT NOT NULL,
  first_at INTEGER NOT NULL, last_at INTEGER NOT NULL, packet_id TEXT,
  PRIMARY KEY (mem_id, conv, first_id));
CREATE INDEX IF NOT EXISTS mem_ranges_conv ON mem_ranges(workspace_id, conv, first_at);

-- 整理报告: every write of a digest run, with what undo puts back.
CREATE TABLE IF NOT EXISTS digest_changes (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, run_id TEXT NOT NULL, writer TEXT NOT NULL,
  op TEXT NOT NULL,                            -- summary|segment|matter|todo|todo_state|mem_link|mem_new
  target TEXT NOT NULL, packet_id TEXT,
  before TEXT NOT NULL DEFAULT '{}', after TEXT NOT NULL DEFAULT '{}',
  state TEXT NOT NULL DEFAULT 'applied',       -- applied|undone
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS digest_changes_run ON digest_changes(workspace_id, run_id, created);
