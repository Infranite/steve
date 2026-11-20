-- Generated from the frozen legacy opening flow; STEVE_BASELINE_BLESS=1 regenerates.
CREATE TABLE account_handles (
  workspace_id TEXT NOT NULL, family TEXT NOT NULL, handle TEXT NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id), connection_id TEXT,   -- 只在某个应用下有效的句柄（open_id）
  created INTEGER NOT NULL, PRIMARY KEY (workspace_id, family, handle));

CREATE TABLE account_profiles (
 account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
 workspace_id TEXT NOT NULL,
 checked INTEGER, next_check INTEGER, attempts INTEGER NOT NULL DEFAULT 0,
 source TEXT, gaps TEXT NOT NULL DEFAULT '{}');

-- debt:comms/fk/accounts.(workspace_id) -> workspaces.(id)
CREATE TABLE accounts (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  system_id TEXT NOT NULL, family TEXT NOT NULL,
  party_id TEXT NOT NULL REFERENCES parties(id),
  kind TEXT NOT NULL,                          -- personal|work_email|app_bot|shared_mailbox|bank_card|service|phone|bot|calendar|root|other
  handle TEXT NOT NULL,                        -- 规范化后的主句柄
  handle_display TEXT NOT NULL,                -- 显示用（卡号只存 •••• 4821）
  display_name TEXT NOT NULL DEFAULT '',
  verified INTEGER NOT NULL DEFAULT 1, confidence REAL NOT NULL DEFAULT 1,
  source TEXT NOT NULL,                        -- me|connect|identify|rule:email_prefix|sync|chat
  source_label TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'active',
  revision INTEGER NOT NULL DEFAULT 1, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE alert_rollups (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, category TEXT NOT NULL,
  fired INTEGER NOT NULL DEFAULT 0, recovered INTEGER NOT NULL DEFAULT 0,
  open INTEGER NOT NULL DEFAULT 0,             -- 1: the last one of this category is not recovered
  handlers TEXT NOT NULL DEFAULT '[]',         -- party ids that answered in the thread
  first_at INTEGER NOT NULL, last_at INTEGER NOT NULL, last_id TEXT NOT NULL,
  PRIMARY KEY (workspace_id, conv, category));

-- debt:comms/fk/briefings.(workspace_id) -> workspaces.(id)
CREATE TABLE briefings (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL,
  day TEXT NOT NULL,
  schedule_id TEXT,
  event_id TEXT NOT NULL,
  material TEXT NOT NULL,
  pushed INTEGER NOT NULL CHECK(pushed IN (0,1)),
  created INTEGER NOT NULL,
  UNIQUE(workspace_id, user_id, day)
);

CREATE TABLE cal_attendees (
  event_id TEXT NOT NULL REFERENCES cal_events(id) ON DELETE CASCADE,
  ord INTEGER NOT NULL,
  party_id TEXT,
  email TEXT,
  name TEXT,
  ext_ref TEXT,
  rsvp TEXT,
  organizer INTEGER NOT NULL DEFAULT 0,
  optional INTEGER NOT NULL DEFAULT 0,
  is_self INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (event_id, ord));

CREATE TABLE cal_calendars (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  owner_user TEXT NOT NULL,
  ext_id TEXT NOT NULL,
  name TEXT NOT NULL,
  color TEXT,
  kind TEXT NOT NULL DEFAULT 'primary',        -- primary|shared|team|subscribed|steve
  role TEXT NOT NULL DEFAULT 'reader',         -- what the source lets the owner do: free_busy|reader|writer|owner
  share TEXT NOT NULL DEFAULT 'private',       -- who else sees it at the source: private|busy|details
  party_id TEXT,                               -- whose calendar it is
  party_set INTEGER NOT NULL DEFAULT 0,        -- 1: the owner chose the person, a sync keeps it
  detail TEXT NOT NULL DEFAULT 'full',         -- what Steve keeps: full | busy (times only)
  enabled INTEGER NOT NULL DEFAULT 1,
  tz TEXT,
  sync_cursor TEXT,
  synced_at INTEGER,                           -- the last sync that succeeded
  full_at INTEGER,                             -- the last whole-window pull that succeeded; a day later the next sync pulls it again
  sync_state TEXT NOT NULL DEFAULT 'new',      -- new|ok|error
  sync_error TEXT,
  gone INTEGER NOT NULL DEFAULT 0,             -- the source no longer lists it
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE (connection_id, ext_id));

CREATE TABLE cal_events (
  id TEXT PRIMARY KEY,
  calendar_id TEXT NOT NULL REFERENCES cal_calendars(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  owner_user TEXT NOT NULL,
  meeting_id TEXT,
  ext_id TEXT NOT NULL,
  series_ext_id TEXT,
  original_start TEXT,
  ical_uid TEXT,
  sequence INTEGER,
  etag TEXT,
  version INTEGER NOT NULL DEFAULT 1,
  fingerprint TEXT NOT NULL,
  title TEXT,
  location TEXT,
  meeting_url TEXT,
  excerpt TEXT,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  tz TEXT,
  all_day INTEGER NOT NULL DEFAULT 0,
  start_date TEXT,
  end_date TEXT,
  status TEXT NOT NULL DEFAULT 'confirmed',    -- confirmed|tentative|cancelled
  my_rsvp TEXT,                                -- accepted|declined|tentative|needs_action
  organizer_party TEXT,
  organizer_email TEXT,
  organizer_name TEXT,
  i_am_organizer INTEGER NOT NULL DEFAULT 0,
  can_modify INTEGER NOT NULL DEFAULT 0,
  busy_only INTEGER NOT NULL DEFAULT 0,        -- only the time is known (free/busy)
  origin TEXT NOT NULL DEFAULT 'source',
  deleted_ms INTEGER,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);

CREATE TABLE cal_ics_files (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,
  body TEXT NOT NULL,
  uploaded INTEGER NOT NULL);

CREATE TABLE cal_meetings (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  owner_user TEXT NOT NULL,
  primary_event_id TEXT NOT NULL,
  match_key TEXT NOT NULL,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);

CREATE TABLE cal_not_same (
  workspace_id TEXT NOT NULL,
  owner_user TEXT NOT NULL,
  a TEXT NOT NULL,
  b TEXT NOT NULL,
  created INTEGER NOT NULL,
  PRIMARY KEY (owner_user, a, b));

CREATE TABLE chan_app_registrations (
  id TEXT PRIMARY KEY,                 -- reg_…
  workspace_id TEXT NOT NULL,
  initiator TEXT NOT NULL,             -- users.id of who started it: every route and the resume act as them
  brand_requested TEXT NOT NULL,       -- feishu | lark: the confirmation page's host
  brand TEXT NOT NULL,                 -- the brand polled now (switched at most once on the tenant's report)
  switched INTEGER NOT NULL DEFAULT 0,
  device_item TEXT,                    -- vault item holding the device_code, while pending only
  user_code TEXT,
  verification_uri_complete TEXT,
  interval_s INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,         -- Unix ms, fixed at begin; a resume never extends it
  state TEXT NOT NULL,                 -- pending|credentials_saved|verifying|succeeded|denied|expired|failed|cancelled
  error TEXT,                          -- short, ours (never the provider's text)
  error_kind TEXT,                     -- denied|expired|network|provider|verify_failed|conflict|member_gone|credentials_lost
  app_may_exist INTEGER NOT NULL DEFAULT 0,
  client_id TEXT,                      -- the App ID once issued
  vault_item TEXT,                     -- the credential item while credentials_saved/verifying; handed to the connection
  connection_id TEXT,
  account_id TEXT,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL);

CREATE TABLE chan_cursors (
  connection_id TEXT NOT NULL REFERENCES connections(id), workspace_id TEXT NOT NULL,
  container TEXT NOT NULL,                     -- 会话 chat_id（oc_…）或话题 thread_id（omt_…）
  kind TEXT NOT NULL DEFAULT 'chat',           -- chat|thread
  chat_id TEXT NOT NULL,                       -- 所在会话（kind=chat 时等于 container）
  cursor INTEGER,                              -- 读到的最大 create_time（毫秒）；NULL = 还没读成功过（第一次读只记游标）
  boundary TEXT NOT NULL DEFAULT '[]',         -- create_time 正好等于 cursor 的消息 id，下次不再处理
  last_new INTEGER,                            -- 上次读到新消息的时间（话题按它判断是否还活跃）
  polled INTEGER,                              -- 上次读这个容器的时间
  error TEXT, error_at INTEGER,                -- 读不到（例如机器人不在的群，飞书不提供）
  created INTEGER NOT NULL, updated INTEGER NOT NULL, token TEXT,
  PRIMARY KEY (connection_id, container));

CREATE TABLE chan_ext_ws_dead_letters (
 connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
 chat_id TEXT NOT NULL,
 source_id TEXT NOT NULL,
 payload TEXT NOT NULL,
 error TEXT NOT NULL,
 created INTEGER NOT NULL,
 PRIMARY KEY (connection_id, chat_id, source_id)
);

CREATE TABLE chan_message_seen (
  connection_id TEXT NOT NULL,
  source_id TEXT NOT NULL,
  message_id TEXT NOT NULL REFERENCES chan_messages(id) ON DELETE CASCADE,
  PRIMARY KEY (connection_id, source_id));

CREATE TABLE chan_messages (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, space_id TEXT NOT NULL REFERENCES spaces(id),
  event_id TEXT,                               -- 入站消息对应的 ext_events
  external_id TEXT, thread_key TEXT, reply_to TEXT,
  direction TEXT NOT NULL,                     -- in|out
  sender_account_id TEXT,                      -- 发送账号（出站 = 应用机器人或你本人的账号）
  sender_party_id TEXT,
  persona_id TEXT,                             -- 出站：代表的身份
  on_behalf TEXT NOT NULL DEFAULT 'none',      -- none|persona|proxy（你本人账号代发）
  text TEXT,                                   -- 保留期限到了置 NULL
  attachments TEXT NOT NULL DEFAULT '[]', mentions TEXT NOT NULL DEFAULT '[]',   -- 提到的账号 id、事项 id
  status TEXT NOT NULL DEFAULT 'received',     -- received|draft|pending|sent|failed|recalled|blocked
  decision_id TEXT, work_item_id TEXT, basis_id TEXT,
  purged INTEGER, created INTEGER NOT NULL, sent INTEGER, sent_via TEXT, ext_created INTEGER, content_key TEXT, fallback_from TEXT, history TEXT, seen_by INTEGER NOT NULL DEFAULT 0, turn_of TEXT, composed TEXT);

CREATE TABLE chan_reactions (
  message_id TEXT PRIMARY KEY REFERENCES chan_messages(id), workspace_id TEXT NOT NULL,
  state TEXT NOT NULL,                 -- ack|done|failed|cleared
  reaction_id TEXT,                    -- the provider's id of the reaction shown now (NULL when adding it failed)
  emoji TEXT,                          -- the provider's emoji type shown now (Feishu `OnIt`, `DONE`, `CrossMark`)
  as_user INTEGER NOT NULL DEFAULT 0,  -- added with your own user token (`/bot` on your account), not the bot's
  answer_id TEXT,                      -- the outbound `chan_messages` row whose sending completes it
  error TEXT, updated INTEGER NOT NULL);

CREATE TABLE chan_send_state (
  message_id TEXT PRIMARY KEY REFERENCES chan_messages(id) ON DELETE CASCADE,
  auth TEXT NOT NULL,                           -- JSON of steve_channels::Auth
  first_at INTEGER,                             -- the first try's start (the dedupe window counts from here)
  attempts INTEGER NOT NULL DEFAULT 0,
  text TEXT,                                    -- what the first try sent
  at TEXT,                                      -- JSON: the mentions it carried
  last_error TEXT);

CREATE TABLE chan_trigger_receipts (
  message_id TEXT NOT NULL REFERENCES chan_messages(id) ON DELETE CASCADE,
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  sender_account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL,
  sender_party_id TEXT REFERENCES parties(id) ON DELETE SET NULL,
  from_self INTEGER NOT NULL CHECK(from_self IN (0, 1)),
  PRIMARY KEY(message_id, connection_id)
);

-- debt:comms/fk/chan_write_blocks.(workspace_id) -> workspaces.(id)
CREATE TABLE chan_write_blocks (
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  space_id TEXT NOT NULL DEFAULT '',
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  reason TEXT NOT NULL,
  blocked_at INTEGER NOT NULL,
  until INTEGER NOT NULL,
  noticed_at INTEGER,
  PRIMARY KEY (connection_id, space_id));

CREATE TABLE channel_settings (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id), workspace_id TEXT NOT NULL,
  mode TEXT NOT NULL,                          -- self（你本人）|bot（应用机器人）
  duty TEXT NOT NULL DEFAULT '',               -- 账号默认职责
  rules TEXT NOT NULL,                         -- 默认响应规则（§2.4）
  trigger TEXT NOT NULL DEFAULT '{"on":false,"words":["/bot"],"where":[],"mark":true}',   -- 只有 self：触发语
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL, send_delay_s INTEGER CHECK(send_delay_s IS NULL OR send_delay_s BETWEEN 0 AND 60), late_ms INTEGER, history_before INTEGER NOT NULL DEFAULT 0, history_late INTEGER NOT NULL DEFAULT 0, history_at INTEGER, card_ttl_h INTEGER);

CREATE TABLE connections (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  account_id TEXT NOT NULL UNIQUE REFERENCES accounts(id),
  adapter TEXT NOT NULL,                       -- feishu|gmail|outlook|gcal|github|webhook|…
  auth TEXT NOT NULL,                          -- oauth|credential|none
  granted_scopes TEXT NOT NULL DEFAULT '[]',   -- 提供方授予的原始 scope
  caps TEXT NOT NULL DEFAULT '[]',             -- §2.2 的能力代码，已启用的子集
  credential_item TEXT,                        -- 系统保险库里的条目
  status TEXT NOT NULL DEFAULT 'ok',           -- ok|reauth|error|paused|revoked
  health TEXT NOT NULL DEFAULT '{}',           -- {"last_ok":0,"last_error":"","error_at":0,"fails":0}
  config TEXT NOT NULL DEFAULT '{}',           -- 适配器自己的配置（租户、端点），不放机密
  last_sync INTEGER, created_by TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL, updated INTEGER NOT NULL, enabled_at INTEGER);

-- debt:comms/fk/connector_refs.(workspace_id) -> workspaces.(id)
CREATE TABLE connector_refs (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('conversation','message','topic','reference')),
  external_id TEXT NOT NULL,
  conversation_id TEXT NOT NULL DEFAULT '',
  source_message_id TEXT NOT NULL DEFAULT '',
  original_type TEXT NOT NULL,
  metadata TEXT NOT NULL DEFAULT '{}',
  expires INTEGER NOT NULL,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE(connection_id, account_id, kind, conversation_id, external_id)
);

CREATE TABLE conv_profiles (
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

CREATE TABLE conv_summaries (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, version INTEGER NOT NULL,
  text TEXT NOT NULL, rules TEXT NOT NULL DEFAULT '',   -- rules: the alert rollup, written by code
  packet_id TEXT, writer TEXT NOT NULL, created INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, conv, version));

-- debt:comms/fk/conversation_preferences.(user_id) -> users.(id)
-- debt:comms/fk/conversation_preferences.(workspace_id) -> workspaces.(id)
CREATE TABLE conversation_preferences (
 workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
 user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
 closed INTEGER NOT NULL DEFAULT 0 CHECK(closed IN (0, 1)), read_up_to INTEGER,
 PRIMARY KEY (workspace_id, user_id, space_id));

CREATE TABLE digest_changes (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, run_id TEXT NOT NULL, writer TEXT NOT NULL,
  op TEXT NOT NULL,                            -- summary|segment|matter|todo|todo_state|mem_link|mem_new
  target TEXT NOT NULL, packet_id TEXT,
  before TEXT NOT NULL DEFAULT '{}', after TEXT NOT NULL DEFAULT '{}',
  state TEXT NOT NULL DEFAULT 'applied',       -- applied|undone
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE digest_chunks (
  packet_id TEXT NOT NULL REFERENCES digest_packets(id) ON DELETE CASCADE, n INTEGER NOT NULL,
  first_pos INTEGER NOT NULL, last_pos INTEGER NOT NULL, overlap INTEGER NOT NULL DEFAULT 0,
  tokens INTEGER NOT NULL, text TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued',        -- queued|done|failed
  attempts INTEGER NOT NULL DEFAULT 0, error TEXT, updated INTEGER NOT NULL,
  PRIMARY KEY (packet_id, n));

CREATE TABLE digest_cursors (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL,
  at INTEGER NOT NULL, msg_id TEXT NOT NULL DEFAULT '',
  since INTEGER NOT NULL,                     -- fixed initial lookback floor; never slides on retry
  windows INTEGER NOT NULL DEFAULT 0,          -- windows cut so far
  day TEXT NOT NULL DEFAULT '', spent INTEGER NOT NULL DEFAULT 0,   -- local day, tokens queued that day
  updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, conv));

CREATE TABLE digest_entries (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, event_id TEXT, run_id TEXT,
  text TEXT NOT NULL, taken INTEGER, created INTEGER NOT NULL);

CREATE TABLE digest_packets (
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

CREATE TABLE digest_pending (
  position INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, message_id TEXT NOT NULL, at INTEGER NOT NULL,
  UNIQUE (workspace_id, conv, message_id));

CREATE TABLE digest_pending_convs (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, messages INTEGER NOT NULL,
  oldest INTEGER, newest INTEGER,
  PRIMARY KEY (workspace_id, conv));

CREATE TABLE digest_receipts (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, message_id TEXT NOT NULL,
  seq INTEGER NOT NULL, created INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, conv, message_id));

CREATE TABLE digest_segments (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, packet_id TEXT NOT NULL, conv TEXT NOT NULL,
  chunk INTEGER NOT NULL DEFAULT 0, first_pos INTEGER NOT NULL, last_pos INTEGER NOT NULL,
  first_id TEXT NOT NULL, last_id TEXT NOT NULL, first_at INTEGER NOT NULL, last_at INTEGER NOT NULL,
  summary TEXT NOT NULL, tags TEXT NOT NULL DEFAULT '[]', matter_id TEXT,
  chains TEXT NOT NULL DEFAULT '[]',           -- the reply chains and topics its messages belong to
  parties TEXT NOT NULL DEFAULT '[]',          -- the party ids of its senders and @ parties
  writer TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE ext_events (
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

CREATE TABLE gh_apps (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,             -- ghapp_<app id>
  app_id INTEGER NOT NULL, slug TEXT NOT NULL, owner TEXT NOT NULL, client_id TEXT NOT NULL,
  credential_item TEXT NOT NULL,               -- 系统保险库条目：pem、webhook_secret、client_secret
  html_url TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE (app_id));

CREATE TABLE gh_checks (
  repo_id TEXT NOT NULL REFERENCES gh_repos(id), workspace_id TEXT NOT NULL,
  head_sha TEXT NOT NULL, kind TEXT NOT NULL,  -- check_run|status
  ext_id TEXT NOT NULL,                        -- check run id；status 用 context 名
  name TEXT NOT NULL, status TEXT NOT NULL,    -- queued|in_progress|completed
  conclusion TEXT,                             -- success|failure|cancelled|skipped|neutral|timed_out|action_required
  url TEXT NOT NULL DEFAULT '', run_id INTEGER, job_id INTEGER,  -- Actions：工作流运行与 job
  summary TEXT NOT NULL DEFAULT '', started INTEGER, completed INTEGER, updated INTEGER NOT NULL,
  PRIMARY KEY (repo_id, head_sha, kind, ext_id));

CREATE TABLE gh_deliveries (
  delivery_id TEXT PRIMARY KEY,                -- X-GitHub-Delivery；轮询生成的用 poll:<repo>:<类型>:<id>:<updated>
  workspace_id TEXT, repo_id TEXT, event TEXT NOT NULL, action TEXT,
  status TEXT NOT NULL,                        -- ok|ignored|failed
  error TEXT, received INTEGER NOT NULL, automation TEXT);

CREATE TABLE gh_delivery_inputs (
  delivery_id TEXT PRIMARY KEY REFERENCES gh_deliveries(delivery_id) ON DELETE CASCADE,
  source TEXT NOT NULL,                         -- JSON: {"app":{ws}} | {"conn":{ws,connection_id}} | {"poll":{repo_id}}
  payload TEXT NOT NULL,                        -- JSON
  attempts INTEGER NOT NULL DEFAULT 0,          -- failed processings so far (kept `received`, tried again)
  next_at INTEGER NOT NULL DEFAULT 0);

CREATE TABLE gh_effects (
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

CREATE TABLE gh_issues (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, repo_id TEXT NOT NULL REFERENCES gh_repos(id),
  number INTEGER NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL, state TEXT NOT NULL,
  labels TEXT NOT NULL DEFAULT '[]', author_login TEXT NOT NULL, author_account_id TEXT,
  item_id TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (repo_id, number));

CREATE TABLE gh_prs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, repo_id TEXT NOT NULL REFERENCES gh_repos(id),
  number INTEGER NOT NULL, gh_id INTEGER NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL,
  state TEXT NOT NULL,                         -- open|closed|merged
  draft INTEGER NOT NULL DEFAULT 0,
  head_ref TEXT NOT NULL, head_sha TEXT NOT NULL, base_ref TEXT NOT NULL,
  author_login TEXT NOT NULL, author_account_id TEXT, opened_by TEXT NOT NULL DEFAULT 'other',   -- steve|me|other
  body_excerpt TEXT NOT NULL DEFAULT '',       -- ≤2000 字
  checks_state TEXT NOT NULL DEFAULT 'none',   -- none|pending|success|failure
  review_state TEXT NOT NULL DEFAULT 'none',   -- none|requested|approved|changes_requested|commented
  mergeable TEXT,                              -- clean|dirty|blocked|unknown
  item_id TEXT, link_source TEXT,              -- branch|ref|steve|manual
  created INTEGER NOT NULL, updated INTEGER NOT NULL, merged_at INTEGER, closed_at INTEGER,
  UNIQUE (repo_id, number));

-- debt:comms/fk/gh_repos.(project_id) -> projects.(id)
CREATE TABLE gh_repos (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, connection_id TEXT NOT NULL REFERENCES connections(id),
  gh_id INTEGER NOT NULL, owner TEXT NOT NULL, name TEXT NOT NULL, full_name TEXT NOT NULL,
  default_branch TEXT NOT NULL, private INTEGER NOT NULL,
  project_id TEXT REFERENCES projects(id),     -- 关联的项目（一个项目最多一个仓库）
  ingest TEXT NOT NULL DEFAULT 'app',          -- app（App Webhook）|repo_hook|poll
  hook_id INTEGER, last_delivery INTEGER, poll_state TEXT NOT NULL DEFAULT '{}',   -- {"pulls_etag":"…","issues_since":0}
  settings TEXT NOT NULL DEFAULT '{}',         -- §3.4
  enabled INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (workspace_id, gh_id));

CREATE TABLE kb_chunks (
  id INTEGER PRIMARY KEY,                      -- = kb_fts 的 rowid
  workspace_id TEXT NOT NULL,
  obj_type TEXT NOT NULL,                      -- chunk|mem|bot_mem
  obj_id TEXT NOT NULL,                        -- chunk：kd_…；mem：mm_…；bot_mem：bot_memories.id
  seq INTEGER NOT NULL DEFAULT 0,
  heading TEXT NOT NULL DEFAULT '',            -- “标题 > 小节”
  text TEXT NOT NULL,                          -- 原文片段（≤1200 字），引用时显示
  start_at INTEGER, end_at INTEGER,            -- 在规范化正文里的字符偏移
  visibility TEXT NOT NULL, level TEXT NOT NULL, created_by TEXT NOT NULL,
  source_kind TEXT NOT NULL, label TEXT NOT NULL,              -- 引用显示用：“笔记 · Acme 合同说明”
  updated INTEGER NOT NULL,
  UNIQUE (obj_type, obj_id, seq));

CREATE TABLE kb_doc_about (
  doc_id TEXT NOT NULL REFERENCES kb_docs(id), workspace_id TEXT NOT NULL,
  object_type TEXT NOT NULL, object_id TEXT NOT NULL,          -- 同 mem_about：party|item|space|bot|persona|me|project
  PRIMARY KEY (doc_id, object_type, object_id));

CREATE TABLE kb_docs (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, source_id TEXT NOT NULL REFERENCES kb_sources(id),
  ref TEXT NOT NULL,                           -- 来源内唯一：相对路径 | fil_… | ST-12 | chat:2026-10-31 | note:<id>
  title TEXT NOT NULL, mime TEXT NOT NULL DEFAULT 'text/markdown',
  body TEXT,                                   -- 只有笔记存正文（可编辑）；其他来源的正文在原处，重建索引时重新读
  file_id TEXT,                                -- files 来源
  sha256 TEXT NOT NULL DEFAULT '',             -- 规范化正文的哈希；没变就不重切
  size INTEGER NOT NULL DEFAULT 0, mtime INTEGER,
  visibility TEXT, level TEXT,                 -- NULL = 跟随来源
  tags TEXT NOT NULL DEFAULT '[]',             -- 文档自带的标签（front matter），纯文本
  status TEXT NOT NULL DEFAULT 'active',       -- active|deleted|failed|skipped
  error TEXT, chunks INTEGER NOT NULL DEFAULT 0,
  created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL, indexed INTEGER,
  UNIQUE (source_id, ref));

CREATE VIRTUAL TABLE kb_fts USING fts5(
  seg, title, tokenize = 'unicode61 remove_diacritics 2', content = '', contentless_delete = 1);

CREATE TABLE kb_marks (                         -- 抽取、同步的进度
  workspace_id TEXT NOT NULL, name TEXT NOT NULL,              -- extract:item:ST-12、extract:chat:2026-10-31、sync:bot_mems
  value TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (workspace_id, name));

-- debt:comms/fk/kb_sources.(workspace_id) -> workspaces.(id)
CREATE TABLE kb_sources (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  kind TEXT NOT NULL,                          -- notes|files|folder|items|chat|mems|bot_mems
  name TEXT NOT NULL,
  config TEXT NOT NULL DEFAULT '{}',           -- folder：{"path":"/Users/…/wiki","include":["**/*.md"],"exclude":[".obsidian/**"]}
  visibility TEXT NOT NULL DEFAULT 'private',  -- 新文档的默认可见范围
  level TEXT NOT NULL DEFAULT 'conclusion',    -- 新文档的默认披露级别
  paused INTEGER NOT NULL DEFAULT 0,           -- 暂停 = 不同步、不参与检索
  status TEXT NOT NULL DEFAULT 'ok',           -- ok|syncing|error
  error TEXT, stats TEXT NOT NULL DEFAULT '{}',-- {"docs":0,"chunks":0,"skipped":0,"bytes":0}
  last_sync INTEGER, revision INTEGER NOT NULL DEFAULT 1,
  created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE kb_uses (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  consumer_type TEXT NOT NULL,                 -- session|chat|draft|flow|search
  consumer_id TEXT NOT NULL,                   -- ses_… | msg id | md_… | run_… | user id
  query TEXT NOT NULL,                         -- 检索词（已过 guard::scan）
  hits TEXT NOT NULL,                          -- [{"n":1,"obj_type","obj_id","seq","score","level","label"}]，不含正文
  created INTEGER NOT NULL);

CREATE TABLE kb_vectors (                       -- 第二期（§3.4），第一期为空表
  chunk_id INTEGER PRIMARY KEY REFERENCES kb_chunks(id),
  model TEXT NOT NULL, dim INTEGER NOT NULL, vec BLOB NOT NULL, created INTEGER NOT NULL);

CREATE TABLE mail_account_settings (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id), workspace_id TEXT NOT NULL,
  signature TEXT NOT NULL DEFAULT '',          -- 起草时附在末尾（原型 mail 插件“签名”）
  duty TEXT NOT NULL DEFAULT '',               -- 这个邮箱的职责与写信习惯（提示词第 4 层）
  send_delay_s INTEGER NOT NULL DEFAULT 10,    -- 批准后延迟发送，期间可以撤销（0..60）
  archive_on_done INTEGER NOT NULL DEFAULT 0,  -- 事项完成后归档这串往来（原型“自动归档已处理的邮件”）
  sync_days INTEGER NOT NULL DEFAULT 30,       -- 接入时回溯多少天
  append_sent TEXT NOT NULL DEFAULT 'auto',    -- IMAP：发出后是否写入“已发送”（auto = Gmail 主机不写，其余写）
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

CREATE TABLE mail_attachments (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, message_id TEXT NOT NULL REFERENCES mail_messages(id),
  part TEXT NOT NULL,                          -- MIME 部件路径 "1.2"（Graph：attachment id）
  name TEXT NOT NULL, mime TEXT NOT NULL, size INTEGER NOT NULL,
  content_id TEXT, inline INTEGER NOT NULL DEFAULT 0,
  file_id TEXT,                                -- 落成附件（files 表）后才有
  created INTEGER NOT NULL);

CREATE TABLE mail_drafts (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, connection_id TEXT NOT NULL,
  thread_id TEXT REFERENCES mail_threads(id), reply_to_message TEXT REFERENCES mail_messages(id),
  mode TEXT NOT NULL,                          -- reply|reply_all|forward|new
  to_addrs TEXT NOT NULL, cc_addrs TEXT NOT NULL DEFAULT '[]', bcc_addrs TEXT NOT NULL DEFAULT '[]',
  subject TEXT NOT NULL, body TEXT NOT NULL,   -- 纯文本；发出时生成 text/plain（+ 简单 HTML）
  files TEXT NOT NULL DEFAULT '[]',            -- FileRef[]（delta-attachments.md）
  author TEXT NOT NULL,                        -- me|steve|<bot id>
  item_id TEXT, session_id TEXT, run_id TEXT, decision_id TEXT, basis TEXT,   -- basis：披露检查结果（§5.3）
  content_hash TEXT NOT NULL,                  -- sha256(收件人、主题、正文、附件 id)；决定打开时的版本
  status TEXT NOT NULL DEFAULT 'draft',        -- draft|pending|approved|sending|sent|failed|discarded
  send_at INTEGER, sent_message TEXT, error TEXT,
  created INTEGER NOT NULL, updated INTEGER NOT NULL, sent INTEGER);

CREATE TABLE mail_folders (
  connection_id TEXT NOT NULL REFERENCES connections(id), workspace_id TEXT NOT NULL,
  ext_id TEXT NOT NULL,                        -- IMAP 文件夹名（修改版 UTF-7 解码后）| Gmail label id | Graph folder id
  name TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'other',          -- inbox|sent|drafts|archive|trash|junk|important|all|other
  uidvalidity INTEGER, last_uid INTEGER, modseq INTEGER,        -- IMAP
  cursor TEXT,                                 -- Graph 每个文件夹的 deltaLink（Gmail 的 historyId 在 source_settings.cursor）
  synced INTEGER, PRIMARY KEY (connection_id, ext_id));

CREATE TABLE mail_messages (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, connection_id TEXT NOT NULL,
  thread_id TEXT NOT NULL REFERENCES mail_threads(id),
  ext_id TEXT NOT NULL,                        -- Gmail id | Graph id | IMAP "<folder>:<uidvalidity>:<uid>"
  message_id TEXT, in_reply_to TEXT, refs TEXT NOT NULL DEFAULT '[]',
  folders TEXT NOT NULL DEFAULT '[]', flags TEXT NOT NULL DEFAULT '[]',   -- seen|flagged|answered
  direction TEXT NOT NULL,                     -- in|out
  from_addr TEXT NOT NULL, from_name TEXT NOT NULL DEFAULT '', from_account_id TEXT, from_party_id TEXT,
  reply_to TEXT,
  to_addrs TEXT NOT NULL DEFAULT '[]', cc_addrs TEXT NOT NULL DEFAULT '[]',   -- [{"addr","name","account_id"}]
  subject TEXT NOT NULL DEFAULT '', snippet TEXT NOT NULL DEFAULT '',          -- snippet ≤200 字，永久保留
  date INTEGER NOT NULL, size INTEGER NOT NULL DEFAULT 0,
  has_attachments INTEGER NOT NULL DEFAULT 0, bulk INTEGER NOT NULL DEFAULT 0,  -- List-Unsubscribe / Precedence: bulk
  body_text TEXT,                              -- 纯文本正文（≤200 KB），保留期限到了置 NULL
  body_new TEXT,                               -- 去掉引用的新内容（≤20000 字），给提示词用
  has_html INTEGER NOT NULL DEFAULT 0,
  raw_path TEXT,                               -- data/mail/<ws>/<id[4..6]>/<id>.eml，0600；保留期限到了删除并置 NULL
  purged INTEGER, event_id TEXT,
  status TEXT NOT NULL DEFAULT 'received',     -- received|sent
  created INTEGER NOT NULL, UNIQUE (connection_id, ext_id));

CREATE TABLE mail_threads (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, connection_id TEXT NOT NULL REFERENCES connections(id),
  thread_key TEXT NOT NULL,                    -- Gmail threadId | Graph conversationId | IMAP：X-GM-THRID 或根 Message-ID
  subject TEXT NOT NULL, last_at INTEGER NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, unread INTEGER NOT NULL DEFAULT 0,
  participants TEXT NOT NULL DEFAULT '[]',     -- 账号 id
  state TEXT NOT NULL DEFAULT 'open',          -- open|needs_reply|waiting|done|muted
  visibility TEXT NOT NULL DEFAULT 'private', created_by TEXT NOT NULL,
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (connection_id, thread_key));

CREATE TABLE match_rules (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, family TEXT NOT NULL,
  kind TEXT NOT NULL,                          -- email_prefix（以后插件可加）
  config TEXT NOT NULL,                        -- {"same_org_only":true,"exclude":["admin","info","noreply","support","hello"],"min_len":3}
  enabled INTEGER NOT NULL DEFAULT 1, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE matter_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, matter_id TEXT NOT NULL,
  op TEXT NOT NULL,                            -- create|assign|unassign|merge|split|not_same|link|unlink|state|rename
  other TEXT,                                  -- the other matter, segment or linked object
  detail TEXT NOT NULL DEFAULT '{}', actor TEXT NOT NULL, created INTEGER NOT NULL);

CREATE TABLE matter_not_same (
  workspace_id TEXT NOT NULL, a TEXT NOT NULL, b TEXT NOT NULL,   -- a < b
  actor TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (workspace_id, a, b));

CREATE TABLE matter_parties (
  matter_id TEXT NOT NULL REFERENCES matters(id) ON DELETE CASCADE, party_id TEXT NOT NULL,
  messages INTEGER NOT NULL DEFAULT 0, last_at INTEGER NOT NULL,
  PRIMARY KEY (matter_id, party_id));

CREATE TABLE matters (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  title TEXT NOT NULL, summary TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL DEFAULT 'open',          -- open|done|dropped|merged
  merged_into TEXT,
  created_by TEXT NOT NULL,                    -- steve | the user | a bot
  first_at INTEGER, last_at INTEGER,
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE VIRTUAL TABLE matters_fts USING fts5(
  seg, tokenize = 'unicode61 remove_diacritics 2', content = '', contentless_delete = 1);

CREATE TABLE matters_fts_rows (
  row INTEGER PRIMARY KEY, matter_id TEXT NOT NULL UNIQUE);

CREATE TABLE merge_proposals (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    from_id TEXT NOT NULL REFERENCES parties(id), into_id TEXT NOT NULL REFERENCES parties(id),
    account_id TEXT, reason TEXT NOT NULL, confidence REAL NOT NULL,
    decision_id TEXT, status TEXT NOT NULL DEFAULT 'open',
    created INTEGER NOT NULL, resolved INTEGER);

CREATE TABLE merges (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    from_id TEXT NOT NULL, into_id TEXT NOT NULL,
    moved TEXT NOT NULL,
    actor TEXT NOT NULL, created INTEGER NOT NULL, split INTEGER);

CREATE TABLE org_domains (
    workspace_id TEXT NOT NULL, domain TEXT NOT NULL,
    org_id TEXT NOT NULL REFERENCES parties(id), created INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, domain));

-- debt:comms/fk/output_channel_preferences.(user_id) -> users.(id)
-- debt:comms/fk/output_channel_preferences.(workspace_id) -> workspaces.(id)
CREATE TABLE output_channel_preferences (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  user_id TEXT NOT NULL REFERENCES users(id),
  party_id TEXT NOT NULL REFERENCES parties(id),
  manual_space_id TEXT REFERENCES spaces(id),
  learned_space_id TEXT REFERENCES spaces(id),
  updated INTEGER NOT NULL,
  PRIMARY KEY(workspace_id,user_id,party_id)
);

-- debt:comms/fk/output_deliveries.(requester) -> users.(id)
-- debt:comms/fk/output_deliveries.(workspace_id) -> workspaces.(id)
CREATE TABLE output_deliveries (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  requester TEXT NOT NULL REFERENCES users(id),
  idempotency_key TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  intent TEXT NOT NULL,
  connection_id TEXT NOT NULL,
  space_id TEXT NOT NULL REFERENCES spaces(id),
  source_message_id TEXT,
  message_id TEXT NOT NULL UNIQUE REFERENCES chan_messages(id),
  attempts INTEGER NOT NULL DEFAULT 0,
  last_attempt_at INTEGER,
  last_error TEXT,
  replied_at INTEGER,
  reply_message_id TEXT REFERENCES chan_messages(id),
  created INTEGER NOT NULL,
  UNIQUE(workspace_id, requester, idempotency_key)
);

-- debt:comms/fk/parties.(user_id) -> users.(id)
-- debt:comms/fk/parties.(workspace_id) -> workspaces.(id)
CREATE TABLE parties (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    type TEXT NOT NULL,
    name TEXT NOT NULL, aliases TEXT NOT NULL DEFAULT '[]', avatar TEXT NOT NULL DEFAULT '{}',
    rels TEXT NOT NULL DEFAULT '[]',
    important INTEGER NOT NULL DEFAULT 0, note TEXT NOT NULL DEFAULT '',
    contact_kind TEXT,
    org_kind TEXT,
    user_id TEXT REFERENCES users(id),
    visibility TEXT NOT NULL DEFAULT 'workspace',
    status TEXT NOT NULL DEFAULT 'active',
    merged_into TEXT,
    source TEXT NOT NULL DEFAULT 'manual',
    source_ref TEXT, revision INTEGER NOT NULL DEFAULT 1,
    created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE persona_bindings (
  workspace_id TEXT NOT NULL, account_id TEXT NOT NULL REFERENCES accounts(id),
  space_id TEXT NOT NULL DEFAULT '', persona_id TEXT NOT NULL REFERENCES personas(id),
  created INTEGER NOT NULL, PRIMARY KEY (workspace_id, account_id, space_id));

-- debt:comms/fk/personal_notifications.(user_id) -> users.(id)
-- debt:comms/fk/personal_notifications.(workspace_id) -> workspaces.(id)
CREATE TABLE "personal_notifications" (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL,
  delivery_key TEXT NOT NULL UNIQUE,
  mode TEXT NOT NULL CHECK(mode IN ('now','digest','log')),
  body TEXT,
  delivery_id TEXT REFERENCES output_deliveries(id) ON DELETE SET NULL,
  state TEXT NOT NULL CHECK(state IN ('waiting','held','submitted','cancelled','purged')),
  seen_at INTEGER,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT 'completion' CHECK(kind IN ('completion','starred')),
  message_id TEXT,
  topic TEXT NOT NULL DEFAULT 'work.item.done'
    CHECK(topic IN ('work.item.done','todo.due','snooze.wake','contact.starred','briefing.ready')),
  UNIQUE(workspace_id,user_id,event_id)
);

-- debt:comms/fk/personas.(bot_id) -> bots.(id)
CREATE TABLE personas (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  name TEXT NOT NULL, avatar TEXT NOT NULL DEFAULT '{}',          -- {"seed":"值班小运"}
  bot_id TEXT NOT NULL REFERENCES bots(id),
  intro TEXT NOT NULL DEFAULT '', soul TEXT NOT NULL DEFAULT '',
  reveal TEXT NOT NULL DEFAULT '{"name":true,"role":true,"ai":true,"steve":true,"owner":true}',
  signature TEXT NOT NULL DEFAULT '', is_default INTEGER NOT NULL DEFAULT 0,   -- 每个工作空间一个默认身份“Steve”
  deleted INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE reply_basis (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, message_id TEXT NOT NULL,
  entries TEXT NOT NULL,                       -- [{"source_type","source_id","source_label","level","effective","used","out":"verbatim|conclusion|reasoning|blocked"}]
  rewrites TEXT NOT NULL DEFAULT '[]',         -- [{"from","to","reason"}]
  verdict TEXT NOT NULL,                       -- pass|rewritten|drafted
  answer_model TEXT, check_model TEXT, created INTEGER NOT NULL);

-- debt:comms/fk/source_settings.(triage_bot) -> bots.(id)
CREATE TABLE source_settings (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id), workspace_id TEXT NOT NULL,
  paused INTEGER NOT NULL DEFAULT 0,           -- 暂停期间不接收，恢复后不补收
  triage_bot TEXT REFERENCES bots(id),         -- “先交给谁判断”，NULL = 主 Bot
  types TEXT NOT NULL DEFAULT '{}',            -- {"mail.new":{"on":true},"cal.before":{"on":true,"param":"30m"}}
  scopes TEXT NOT NULL DEFAULT '[]',           -- [{"ext_id":"INBOX","name":"收件箱","mode":"receive|record|ignore"}]
  cursor TEXT,                                 -- 适配器的增量同步游标
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

CREATE TABLE space_identities (
  space_id TEXT NOT NULL REFERENCES spaces(id), connection_id TEXT NOT NULL, workspace_id TEXT NOT NULL,
  joined INTEGER NOT NULL, left_at INTEGER, ordinal INTEGER, owner_user TEXT, platform TEXT,     -- NULL = still sees it
  PRIMARY KEY (space_id, connection_id));

CREATE TABLE space_members (
  workspace_id TEXT NOT NULL, space_id TEXT NOT NULL REFERENCES spaces(id),
  account_id TEXT NOT NULL REFERENCES accounts(id), role TEXT NOT NULL DEFAULT 'member',   -- owner|admin|member|bot
  joined INTEGER NOT NULL, PRIMARY KEY (space_id, account_id));

CREATE TABLE space_merges (
  from_id TEXT PRIMARY KEY, into_id TEXT NOT NULL, workspace_id TEXT NOT NULL,
  installation TEXT NOT NULL, external_id TEXT NOT NULL, merged INTEGER NOT NULL);

CREATE TABLE space_prefs (
  space_id TEXT PRIMARY KEY REFERENCES spaces(id), workspace_id TEXT NOT NULL,
  attention TEXT NOT NULL DEFAULT 'realtime',
  voice TEXT,
  updated INTEGER NOT NULL, reply_as_you TEXT NOT NULL DEFAULT 'draft', bot_notices TEXT NOT NULL DEFAULT 'auto');

CREATE TABLE space_settings (
  space_id TEXT PRIMARY KEY REFERENCES spaces(id), workspace_id TEXT NOT NULL,
  duty TEXT NOT NULL DEFAULT '',               -- 这个群的职责（agents）
  rules TEXT,                                  -- NULL = 跟随账号
  flow_id TEXT,                                -- 转成流程后由流程处理（kind=response）
  muted INTEGER NOT NULL DEFAULT 0, collab INTEGER NOT NULL DEFAULT 0,
  memory_scope TEXT NOT NULL DEFAULT 'space',  -- space（仅本群）|persona（与身份共享）
  disclosure TEXT NOT NULL,                    -- 本群内容在别处能说到什么程度（§2.6 默认值）
  item_defaults TEXT NOT NULL DEFAULT '{}',    -- {"tags":[],"project_id":null,"bot_id":null}
  configured INTEGER NOT NULL DEFAULT 0,       -- 你是否看过/设置过（新群提醒用）
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

CREATE TABLE "spaces" (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  system_id TEXT NOT NULL, connection_id TEXT REFERENCES connections(id) ON DELETE SET NULL,   -- NULL = 接入账号已断开
  kind TEXT NOT NULL,                          -- group|dm|channel|thread|mail_thread|meeting
  external_id TEXT NOT NULL, name TEXT NOT NULL,
  member_count INTEGER NOT NULL DEFAULT 0, has_external INTEGER NOT NULL DEFAULT 0,
  meeting TEXT,                                -- 会议：{"start":0,"end":0,"where":"","notes":""}
  status TEXT NOT NULL DEFAULT 'active',       -- active|left|archived
  last_message INTEGER, created INTEGER NOT NULL, updated INTEGER NOT NULL,
  account_id TEXT,                             -- 断开时记下的接入账号（决定谁能看到）
  UNIQUE (workspace_id, connection_id, external_id));

CREATE TABLE team_details (
    party_id TEXT PRIMARY KEY REFERENCES parties(id), workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    parent_id TEXT REFERENCES parties(id), org_id TEXT REFERENCES parties(id),
    purpose TEXT NOT NULL DEFAULT '', space_id TEXT,
    sync_connection TEXT, external_id TEXT, synced INTEGER);

CREATE TABLE team_members (
    workspace_id TEXT NOT NULL, team_id TEXT NOT NULL REFERENCES parties(id),
    contact_id TEXT NOT NULL REFERENCES parties(id),
    role TEXT NOT NULL DEFAULT 'member',
    source TEXT NOT NULL DEFAULT 'manual',
    joined INTEGER NOT NULL, PRIMARY KEY (team_id, contact_id));

-- debt:comms/fk/time_alarms.(workspace_id) -> workspaces.(id)
CREATE TABLE time_alarms (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  owner_user TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('brief','prep','remind','todo','snooze')),
  subject TEXT NOT NULL,
  planned_for INTEGER NOT NULL,
  fire_at INTEGER NOT NULL,
  stale_at INTEGER,
  hold_until INTEGER,
  state TEXT NOT NULL DEFAULT 'planned' CHECK(state IN ('planned','fired','stale','cancelled')),
  run_ref TEXT,
  fired_at INTEGER,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE(workspace_id, owner_user, kind, subject, planned_for)
);

CREATE TABLE todo_parties (
  todo_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE, party_id TEXT NOT NULL,
  PRIMARY KEY (todo_id, party_id));

CREATE TABLE todo_refs (
  todo_id TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE, conv TEXT NOT NULL, message_id TEXT NOT NULL,
  PRIMARY KEY (todo_id, conv, message_id));

CREATE TABLE todos (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, owner TEXT NOT NULL,
  text TEXT NOT NULL, matter_id TEXT,
  due INTEGER, due_quote TEXT,                 -- only when a message states it: its words
  state TEXT NOT NULL DEFAULT 'open',          -- open|done|dropped|handed (to a Bot, as item_id)
  item_id TEXT,
  source TEXT NOT NULL,                        -- steve|you
  writer TEXT NOT NULL, packet_id TEXT,
  created INTEGER NOT NULL, updated INTEGER NOT NULL, closed INTEGER, kind TEXT NOT NULL DEFAULT 'todo', parent_id TEXT REFERENCES todos(id) ON DELETE SET NULL, target_date TEXT, due_kind TEXT, due_date TEXT, remind_at INTEGER, snooze_until INTEGER, block_event TEXT, pri INTEGER NOT NULL DEFAULT 0, star INTEGER NOT NULL DEFAULT 0, recur TEXT);

CREATE INDEX account_handles_acc ON account_handles(account_id);

CREATE INDEX account_handles_handle ON account_handles(workspace_id, handle);

CREATE INDEX account_profiles_due ON account_profiles(workspace_id, next_check);

CREATE INDEX accounts_party ON accounts(workspace_id, party_id);

CREATE INDEX briefings_created ON briefings(created);

CREATE INDEX cal_attendees_party ON cal_attendees(party_id);

CREATE INDEX cal_calendars_owner ON cal_calendars(workspace_id, owner_user);

CREATE INDEX cal_events_calendar ON cal_events(calendar_id, start_ms);

CREATE INDEX cal_events_meeting ON cal_events(meeting_id);

CREATE INDEX cal_events_owner_time ON cal_events(workspace_id, owner_user, start_ms);

CREATE INDEX cal_meetings_owner ON cal_meetings(workspace_id, owner_user);

CREATE UNIQUE INDEX chan_app_registrations_active ON chan_app_registrations(workspace_id, initiator)
  WHERE state IN ('pending', 'credentials_saved', 'verifying');

CREATE INDEX chan_app_registrations_resume ON chan_app_registrations(state)
  WHERE state IN ('pending', 'credentials_saved', 'verifying');

CREATE INDEX chan_cursors_kind ON chan_cursors(connection_id, kind, last_new);

CREATE INDEX chan_message_seen_message ON chan_message_seen (message_id);

CREATE INDEX chan_messages_content_key ON chan_messages (space_id, content_key, created)
  WHERE content_key IS NOT NULL;

CREATE UNIQUE INDEX chan_messages_ext ON chan_messages(space_id, external_id) WHERE external_id IS NOT NULL;

CREATE INDEX chan_messages_outbox ON chan_messages(status, created) WHERE direction = 'out';

CREATE INDEX chan_messages_sender ON chan_messages(workspace_id, sender_party_id, created);

CREATE INDEX chan_messages_space ON chan_messages(workspace_id, space_id, created);

CREATE INDEX chan_messages_space_created ON chan_messages(space_id, created, id);

CREATE INDEX chan_reactions_answer ON chan_reactions(answer_id) WHERE answer_id IS NOT NULL;

CREATE INDEX chan_trigger_receipts_connection ON chan_trigger_receipts(connection_id);

CREATE INDEX connections_ws ON connections(workspace_id, status);

CREATE INDEX connector_refs_connection ON connector_refs(workspace_id, connection_id, account_id);

CREATE INDEX connector_refs_expiry ON connector_refs(expires);

CREATE INDEX digest_changes_run ON digest_changes(workspace_id, run_id, created);

CREATE INDEX digest_open ON digest_entries(workspace_id, taken, created);

CREATE INDEX digest_packets_state ON digest_packets(workspace_id, state, priority, created);

CREATE INDEX digest_pending_conv ON digest_pending(workspace_id, conv, at, message_id);

CREATE INDEX digest_segments_conv ON digest_segments(workspace_id, conv, first_at);

CREATE INDEX digest_segments_matter ON digest_segments(matter_id, first_at);

CREATE INDEX digest_segments_packet ON digest_segments(packet_id, chunk, first_pos);

CREATE INDEX ext_events_conn_type ON ext_events(connection_id, type, received) WHERE connection_id IS NOT NULL;

CREATE INDEX ext_events_group ON ext_events(workspace_id, group_key, received);

CREATE INDEX ext_events_list ON ext_events(workspace_id, received);

CREATE INDEX ext_events_purge ON ext_events(payload_purged, received);

CREATE INDEX gh_deliveries_automation_event
    ON gh_deliveries(workspace_id, json_extract(automation, '$.event_id'))
    WHERE automation IS NOT NULL;

CREATE INDEX gh_deliveries_automation_key
    ON gh_deliveries(repo_id, json_extract(automation, '$.key'))
    WHERE automation IS NOT NULL;

CREATE INDEX gh_deliveries_pending ON gh_deliveries(received) WHERE status = 'received';

CREATE INDEX gh_deliveries_recv ON gh_deliveries(received);

CREATE INDEX gh_effects_due ON gh_effects(next_at) WHERE state IN ('pending', 'unknown');

CREATE INDEX gh_prs_head ON gh_prs(repo_id, head_sha);

CREATE INDEX gh_prs_item ON gh_prs(workspace_id, item_id);

CREATE UNIQUE INDEX gh_repos_project ON gh_repos(workspace_id, project_id) WHERE project_id IS NOT NULL;

CREATE INDEX kb_chunks_ws ON kb_chunks(workspace_id, obj_type, obj_id);

CREATE INDEX kb_doc_about_obj ON kb_doc_about(workspace_id, object_type, object_id);

CREATE INDEX kb_docs_ws ON kb_docs(workspace_id, status, updated);

CREATE INDEX kb_sources_ws ON kb_sources(workspace_id, kind);

CREATE INDEX kb_uses_consumer ON kb_uses(workspace_id, consumer_type, consumer_id);

CREATE INDEX mail_attachments_msg ON mail_attachments(message_id);

CREATE INDEX mail_drafts_send ON mail_drafts(status, send_at);

CREATE INDEX mail_drafts_thread ON mail_drafts(thread_id, status);

CREATE INDEX mail_drafts_ws ON mail_drafts(workspace_id, status, updated);

CREATE INDEX mail_messages_conn_mid ON mail_messages(connection_id, message_id);

CREATE INDEX mail_messages_expire ON mail_messages(workspace_id, date) WHERE purged IS NULL;

CREATE INDEX mail_messages_from ON mail_messages(workspace_id, from_party_id, date);

CREATE INDEX mail_messages_mid ON mail_messages(workspace_id, message_id);

CREATE INDEX mail_messages_thread ON mail_messages(thread_id, date);

CREATE INDEX mail_threads_ws ON mail_threads(workspace_id, last_at);

CREATE INDEX matter_events_matter ON matter_events(matter_id, created);

CREATE INDEX matter_parties_party ON matter_parties(party_id);

CREATE INDEX matters_ws ON matters(workspace_id, state, last_at);

CREATE UNIQUE INDEX merge_open ON merge_proposals(workspace_id, from_id, into_id) WHERE status = 'open';

CREATE INDEX output_deliveries_workspace ON output_deliveries(workspace_id, created);

CREATE UNIQUE INDEX parties_me ON parties(workspace_id, user_id) WHERE user_id IS NOT NULL;

CREATE INDEX parties_ws ON parties(workspace_id, type, status, name);

CREATE INDEX personal_notifications_delivery ON personal_notifications(delivery_id) WHERE delivery_id IS NOT NULL;

CREATE INDEX personal_notifications_due ON personal_notifications(state,next_attempt_at,created);

CREATE INDEX reply_basis_msg ON reply_basis(message_id, created);

CREATE INDEX space_identities_conn ON space_identities(connection_id, space_id);

CREATE UNIQUE INDEX space_identities_ordinal ON space_identities(space_id, ordinal) WHERE ordinal IS NOT NULL;

CREATE INDEX space_members_acc ON space_members(workspace_id, account_id);

CREATE INDEX spaces_conn ON spaces(workspace_id, connection_id, status);

CREATE INDEX spaces_ext ON spaces(workspace_id, external_id);

CREATE UNIQUE INDEX team_ext ON team_details(workspace_id, sync_connection, external_id) WHERE external_id IS NOT NULL;

CREATE INDEX team_members_contact ON team_members(workspace_id, contact_id);

CREATE INDEX team_parent ON team_details(workspace_id, parent_id);

CREATE INDEX time_alarms_due ON time_alarms(fire_at) WHERE state = 'planned';

CREATE INDEX time_alarms_subject ON time_alarms(workspace_id, subject);

CREATE INDEX todos_parent ON todos(parent_id) WHERE parent_id IS NOT NULL;

CREATE INDEX todos_ws ON todos(workspace_id, owner, state, created);

CREATE TRIGGER notification_delivery_deleted BEFORE DELETE ON output_deliveries BEGIN
  UPDATE personal_notifications SET body=NULL,state='purged' WHERE delivery_id=OLD.id;
END;

CREATE TRIGGER notification_delivery_purged AFTER UPDATE OF intent ON output_deliveries
WHEN json_type(NEW.intent,'$.content.markdown') IS NULL BEGIN
  UPDATE personal_notifications SET body=NULL,state='purged' WHERE delivery_id=NEW.id;
END;

-- debt:comms/trigger/notification_feed_insert
CREATE TRIGGER notification_feed_insert AFTER INSERT ON personal_notifications BEGIN
  INSERT INTO feed_entries(id,workspace_id,audience,recipient_id,kind,source_type,source_key,source_id,notification_id,item_id,published_at)
    VALUES('feed_'||NEW.id,NEW.workspace_id,'user',NEW.user_id,
      CASE NEW.topic WHEN 'work.item.done' THEN 'completion' WHEN 'todo.due' THEN 'reminder'
        WHEN 'snooze.wake' THEN 'reminder' WHEN 'contact.starred' THEN 'starred' WHEN 'briefing.ready' THEN 'briefing' END,
      'notification',json_array(NEW.user_id,NEW.event_id),NEW.id,NEW.id,
      (SELECT json_extract(o.payload,'$.item_id') FROM outbox_events o WHERE o.id=NEW.event_id AND o.workspace_id=NEW.workspace_id AND o.topic='work.item.done'),
      NEW.created) ON CONFLICT(workspace_id,source_type,source_key) DO NOTHING;
END;

CREATE TRIGGER notification_message_purged AFTER UPDATE OF text ON chan_messages
WHEN NEW.text IS NULL AND OLD.text IS NOT NULL BEGIN
  UPDATE output_deliveries SET intent=json_set(intent,'$.content',json('{}')) WHERE message_id=NEW.id;
END;

CREATE TRIGGER starred_source_deleted BEFORE DELETE ON chan_messages
WHEN OLD.direction = 'in' BEGIN
  UPDATE chan_messages SET text = NULL, attachments = '[]', purged = CAST(strftime('%s', 'now') AS INTEGER) * 1000,
         status = CASE WHEN status = 'pending' THEN 'discarded' ELSE status END
   WHERE id IN (SELECT od.message_id FROM personal_notifications n JOIN output_deliveries od ON od.id = n.delivery_id
                 WHERE n.topic = 'contact.starred' AND n.message_id = OLD.id);
  UPDATE output_deliveries SET intent = json_set(intent, '$.content', json('{}'))
   WHERE id IN (SELECT delivery_id FROM personal_notifications WHERE topic = 'contact.starred' AND message_id = OLD.id);
  UPDATE personal_notifications SET body = NULL, state = 'purged' WHERE topic = 'contact.starred' AND message_id = OLD.id;
END;

CREATE TRIGGER starred_source_purged AFTER UPDATE OF text ON chan_messages
WHEN NEW.text IS NULL AND OLD.text IS NOT NULL AND NEW.direction = 'in' BEGIN
  UPDATE chan_messages SET text = NULL, attachments = '[]',
         purged = coalesce(NEW.purged, CAST(strftime('%s', 'now') AS INTEGER) * 1000),
         status = CASE WHEN status = 'pending' THEN 'discarded' ELSE status END
   WHERE id IN (SELECT od.message_id FROM personal_notifications n JOIN output_deliveries od ON od.id = n.delivery_id
                 WHERE n.topic = 'contact.starred' AND n.message_id = NEW.id);
  UPDATE output_deliveries SET intent = json_set(intent, '$.content', json('{}'))
   WHERE id IN (SELECT delivery_id FROM personal_notifications WHERE topic = 'contact.starred' AND message_id = NEW.id);
  UPDATE personal_notifications SET body = NULL, state = 'purged' WHERE topic = 'contact.starred' AND message_id = NEW.id;
END;

-- debt:comms/trigger/todo_alarms_delete
CREATE TRIGGER todo_alarms_delete AFTER DELETE ON todos BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT OLD.workspace_id, 'todo:' || OLD.id
     WHERE EXISTS(SELECT 1 FROM time_alarms WHERE workspace_id = OLD.workspace_id AND subject = 'todo:' || OLD.id)
    ON CONFLICT DO NOTHING;
END;

-- debt:comms/trigger/todo_alarms_insert
CREATE TRIGGER todo_alarms_insert AFTER INSERT ON todos
WHEN NEW.remind_at IS NOT NULL OR NEW.snooze_until IS NOT NULL BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'todo:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;

-- debt:comms/trigger/todo_alarms_update
CREATE TRIGGER todo_alarms_update AFTER UPDATE OF state, remind_at, snooze_until, owner, workspace_id ON todos
WHEN NEW.state IS NOT OLD.state OR NEW.remind_at IS NOT OLD.remind_at OR NEW.snooze_until IS NOT OLD.snooze_until
  OR NEW.owner IS NOT OLD.owner OR NEW.workspace_id IS NOT OLD.workspace_id BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'todo:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;

-- Final fresh-install seeds; historical data backfills are not replayed.
INSERT INTO sqlite_sequence(name,seq) VALUES ('digest_pending',0);
