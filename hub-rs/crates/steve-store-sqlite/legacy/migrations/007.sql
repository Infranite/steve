-- 7: channels & personas (incl. Feishu): channel settings, spaces, members, space settings, channel messages, personas, bindings, reply basis.
-- Spec: steve-rs-plan/modules/channels-personas.md §1 (feishu.md adds no tables), DDL verbatim. Rust Hub only; additive, nothing existing is altered.

-- 接入账号上的渠道设置（每个有 msg.receive 的连接一行）
CREATE TABLE IF NOT EXISTS channel_settings (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id), workspace_id TEXT NOT NULL,
  mode TEXT NOT NULL,                          -- self（你本人）|bot（应用机器人）
  duty TEXT NOT NULL DEFAULT '',               -- 账号默认职责
  rules TEXT NOT NULL,                         -- 默认响应规则（§2.4）
  trigger TEXT NOT NULL DEFAULT '{"on":false,"words":["/bot"],"where":[],"mark":true}',   -- 只有 self：触发语
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS spaces (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  system_id TEXT NOT NULL, connection_id TEXT NOT NULL REFERENCES connections(id),
  kind TEXT NOT NULL,                          -- group|dm|channel|thread|mail_thread|meeting
  external_id TEXT NOT NULL, name TEXT NOT NULL,
  member_count INTEGER NOT NULL DEFAULT 0, has_external INTEGER NOT NULL DEFAULT 0,
  meeting TEXT,                                -- 会议：{"start":0,"end":0,"where":"","notes":""}
  status TEXT NOT NULL DEFAULT 'active',       -- active|left|archived
  last_message INTEGER, created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (workspace_id, connection_id, external_id));
CREATE INDEX IF NOT EXISTS spaces_conn ON spaces(workspace_id, connection_id, status);

CREATE TABLE IF NOT EXISTS space_members (
  workspace_id TEXT NOT NULL, space_id TEXT NOT NULL REFERENCES spaces(id),
  account_id TEXT NOT NULL REFERENCES accounts(id), role TEXT NOT NULL DEFAULT 'member',   -- owner|admin|member|bot
  joined INTEGER NOT NULL, PRIMARY KEY (space_id, account_id));
CREATE INDEX IF NOT EXISTS space_members_acc ON space_members(workspace_id, account_id);

CREATE TABLE IF NOT EXISTS space_settings (
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

CREATE TABLE IF NOT EXISTS chan_messages (
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
  purged INTEGER, created INTEGER NOT NULL, sent INTEGER);
CREATE INDEX IF NOT EXISTS chan_messages_space ON chan_messages(workspace_id, space_id, created);
CREATE INDEX IF NOT EXISTS chan_messages_sender ON chan_messages(workspace_id, sender_party_id, created);
CREATE UNIQUE INDEX IF NOT EXISTS chan_messages_ext ON chan_messages(space_id, external_id) WHERE external_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS personas (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  name TEXT NOT NULL, avatar TEXT NOT NULL DEFAULT '{}',          -- {"seed":"值班小运"}
  bot_id TEXT NOT NULL REFERENCES bots(id),
  intro TEXT NOT NULL DEFAULT '', soul TEXT NOT NULL DEFAULT '',
  reveal TEXT NOT NULL DEFAULT '{"name":true,"role":true,"ai":true,"steve":true,"owner":true}',
  signature TEXT NOT NULL DEFAULT '', is_default INTEGER NOT NULL DEFAULT 0,   -- 每个工作空间一个默认身份“Steve”
  deleted INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL, updated INTEGER NOT NULL);

-- 身份绑定：账号默认身份（space_id=''）或某个空间单独换身份
CREATE TABLE IF NOT EXISTS persona_bindings (
  workspace_id TEXT NOT NULL, account_id TEXT NOT NULL REFERENCES accounts(id),
  space_id TEXT NOT NULL DEFAULT '', persona_id TEXT NOT NULL REFERENCES personas(id),
  created INTEGER NOT NULL, PRIMARY KEY (workspace_id, account_id, space_id));

-- 依据记录：一次回复用了哪些知识、各自级别和处理方式
CREATE TABLE IF NOT EXISTS reply_basis (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, message_id TEXT NOT NULL,
  entries TEXT NOT NULL,                       -- [{"source_type","source_id","source_label","level","effective","used","out":"verbatim|conclusion|reasoning|blocked"}]
  rewrites TEXT NOT NULL DEFAULT '[]',         -- [{"from","to","reason"}]
  verdict TEXT NOT NULL,                       -- pass|rewritten|drafted
  answer_model TEXT, check_model TEXT, created INTEGER NOT NULL);
