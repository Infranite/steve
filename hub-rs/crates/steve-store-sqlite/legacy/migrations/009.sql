-- 9: email: account settings, folders, threads, messages, attachments, drafts.
-- Spec: steve-rs-plan/modules/email.md §1 (mail_account_settings), §2, DDL verbatim. Rust Hub only; additive, nothing existing is altered.

CREATE TABLE IF NOT EXISTS mail_account_settings (
  connection_id TEXT PRIMARY KEY REFERENCES connections(id), workspace_id TEXT NOT NULL,
  signature TEXT NOT NULL DEFAULT '',          -- 起草时附在末尾（原型 mail 插件“签名”）
  duty TEXT NOT NULL DEFAULT '',               -- 这个邮箱的职责与写信习惯（提示词第 4 层）
  send_delay_s INTEGER NOT NULL DEFAULT 10,    -- 批准后延迟发送，期间可以撤销（0..60）
  archive_on_done INTEGER NOT NULL DEFAULT 0,  -- 事项完成后归档这串往来（原型“自动归档已处理的邮件”）
  sync_days INTEGER NOT NULL DEFAULT 30,       -- 接入时回溯多少天
  append_sent TEXT NOT NULL DEFAULT 'auto',    -- IMAP：发出后是否写入“已发送”（auto = Gmail 主机不写，其余写）
  revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS mail_folders (
  connection_id TEXT NOT NULL REFERENCES connections(id), workspace_id TEXT NOT NULL,
  ext_id TEXT NOT NULL,                        -- IMAP 文件夹名（修改版 UTF-7 解码后）| Gmail label id | Graph folder id
  name TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT 'other',          -- inbox|sent|drafts|archive|trash|junk|important|all|other
  uidvalidity INTEGER, last_uid INTEGER, modseq INTEGER,        -- IMAP
  cursor TEXT,                                 -- Graph 每个文件夹的 deltaLink（Gmail 的 historyId 在 source_settings.cursor）
  synced INTEGER, PRIMARY KEY (connection_id, ext_id));

CREATE TABLE IF NOT EXISTS mail_threads (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, connection_id TEXT NOT NULL REFERENCES connections(id),
  thread_key TEXT NOT NULL,                    -- Gmail threadId | Graph conversationId | IMAP：X-GM-THRID 或根 Message-ID
  subject TEXT NOT NULL, last_at INTEGER NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, unread INTEGER NOT NULL DEFAULT 0,
  participants TEXT NOT NULL DEFAULT '[]',     -- 账号 id
  state TEXT NOT NULL DEFAULT 'open',          -- open|needs_reply|waiting|done|muted
  visibility TEXT NOT NULL DEFAULT 'private', created_by TEXT NOT NULL,
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (connection_id, thread_key));
CREATE INDEX IF NOT EXISTS mail_threads_ws ON mail_threads(workspace_id, last_at);

CREATE TABLE IF NOT EXISTS mail_messages (
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
CREATE INDEX IF NOT EXISTS mail_messages_thread ON mail_messages(thread_id, date);
CREATE INDEX IF NOT EXISTS mail_messages_mid ON mail_messages(workspace_id, message_id);
CREATE INDEX IF NOT EXISTS mail_messages_from ON mail_messages(workspace_id, from_party_id, date);

CREATE TABLE IF NOT EXISTS mail_attachments (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, message_id TEXT NOT NULL REFERENCES mail_messages(id),
  part TEXT NOT NULL,                          -- MIME 部件路径 "1.2"（Graph：attachment id）
  name TEXT NOT NULL, mime TEXT NOT NULL, size INTEGER NOT NULL,
  content_id TEXT, inline INTEGER NOT NULL DEFAULT 0,
  file_id TEXT,                                -- 落成附件（files 表）后才有
  created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS mail_attachments_msg ON mail_attachments(message_id);

CREATE TABLE IF NOT EXISTS mail_drafts (
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
CREATE INDEX IF NOT EXISTS mail_drafts_ws ON mail_drafts(workspace_id, status, updated);
CREATE INDEX IF NOT EXISTS mail_drafts_send ON mail_drafts(status, send_at);
