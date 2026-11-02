-- 5: accounts & vault: systems, accounts, handles, connections, match rules, vault keys/vaults/templates/items/fields, grants, secret uses, passkeys.
-- Spec: steve-rs-plan/modules/accounts-vault.md §1, DDL verbatim. Rust Hub only; additive, nothing existing is altered.

CREATE TABLE IF NOT EXISTS systems (
  id TEXT NOT NULL, workspace_id TEXT NOT NULL,                     -- '*' = 内置
  name TEXT NOT NULL, kind TEXT NOT NULL,      -- comm|calendar|code|cloud|finance|steve
  family TEXT NOT NULL,                        -- 句柄族：email|phone|feishu_user|feishu_app|slack_user|github_user|aws_account|card|other
  plugin TEXT,                                 -- 提供适配器的插件（内置写 builtin:<name>）
  handle_rule TEXT NOT NULL DEFAULT '{}',      -- {"normalize":"email|phone|lower|none","pattern":"…"}
  created INTEGER NOT NULL, PRIMARY KEY (workspace_id, id));

CREATE TABLE IF NOT EXISTS accounts (
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
CREATE INDEX IF NOT EXISTS accounts_party ON accounts(workspace_id, party_id);
-- 所有句柄（含主句柄、飞书各应用下不同的 open_id）都在这里，识别只查这张表
CREATE TABLE IF NOT EXISTS account_handles (
  workspace_id TEXT NOT NULL, family TEXT NOT NULL, handle TEXT NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id), connection_id TEXT,   -- 只在某个应用下有效的句柄（open_id）
  created INTEGER NOT NULL, PRIMARY KEY (workspace_id, family, handle));
CREATE INDEX IF NOT EXISTS account_handles_acc ON account_handles(account_id);

CREATE TABLE IF NOT EXISTS connections (
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
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS connections_ws ON connections(workspace_id, status);

CREATE TABLE IF NOT EXISTS match_rules (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, family TEXT NOT NULL,
  kind TEXT NOT NULL,                          -- email_prefix（以后插件可加）
  config TEXT NOT NULL,                        -- {"same_org_only":true,"exclude":["admin","info","noreply","support","hello"],"min_len":3}
  enabled INTEGER NOT NULL DEFAULT 1, created INTEGER NOT NULL, updated INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS vault_keys (
  workspace_id TEXT NOT NULL, version INTEGER NOT NULL,
  keychain_account TEXT NOT NULL,              -- 钥匙串条目的 account 名
  fingerprint TEXT NOT NULL,                   -- hex(sha256("steve/kek-fp" || KEK))[:16]
  status TEXT NOT NULL,                        -- active|retiring|retired
  created INTEGER NOT NULL, retired INTEGER, PRIMARY KEY (workspace_id, version));

CREATE TABLE IF NOT EXISTS vaults (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL,
  scope TEXT NOT NULL,                         -- personal|shared|system（system 存连接凭据，界面不列出）
  owner_user TEXT,                             -- personal 才有
  backend TEXT NOT NULL DEFAULT 'builtin',     -- builtin|external:<plugin>
  created INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS vault_templates (
  id TEXT NOT NULL, workspace_id TEXT NOT NULL, name TEXT NOT NULL,
  fields TEXT NOT NULL,                        -- [{"label":"密码","type":"password","secret":true}]
  default_sensitivity TEXT NOT NULL, source TEXT NOT NULL, PRIMARY KEY (workspace_id, id));

CREATE TABLE IF NOT EXISTS vault_items (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, vault_id TEXT NOT NULL REFERENCES vaults(id),
  template TEXT NOT NULL, title TEXT NOT NULL, site TEXT NOT NULL DEFAULT '',
  account_id TEXT REFERENCES accounts(id),
  sensitivity TEXT NOT NULL,                   -- normal|sensitive|high
  dek_wrapped BLOB, kek_version INTEGER,       -- builtin 后端才有；删除条目时置空 = 销毁
  external_ref TEXT,                           -- 外部后端的引用
  status TEXT NOT NULL DEFAULT 'active',       -- active|deleted
  created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS vault_items_ws ON vault_items(workspace_id, vault_id, status);

CREATE TABLE IF NOT EXISTS vault_fields (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT NOT NULL REFERENCES vault_items(id),
  label TEXT NOT NULL, type TEXT NOT NULL,     -- text|password|otp|card_number|date|email|url|file
  secret INTEGER NOT NULL,
  value_plain TEXT,                            -- 只有非机密字段
  value_ct BLOB,                               -- 只有机密字段：nonce(24) || 密文
  hint TEXT NOT NULL DEFAULT '',               -- 卡号：•••• 4821；其他机密：空
  sort INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS vault_fields_item ON vault_fields(item_id, sort);

CREATE TABLE IF NOT EXISTS grants (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  subject_type TEXT NOT NULL, subject_id TEXT NOT NULL,             -- bot|persona|plugin|device
  capability TEXT NOT NULL,                    -- secret.use（其他模块以后复用：http.effect、cmd.run……）
  resource_type TEXT NOT NULL, resource_id TEXT NOT NULL,           -- vault_item|host|project
  scope TEXT NOT NULL DEFAULT '{}',            -- {"project_id":"prj_…"} 或 {"site":"github.com"}
  until INTEGER NOT NULL, created_by TEXT NOT NULL, decision_id TEXT,
  revoked INTEGER, created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS grants_lookup ON grants(workspace_id, capability, resource_id, subject_id);

CREATE TABLE IF NOT EXISTS secret_uses (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT NOT NULL,
  actor_type TEXT NOT NULL, actor_id TEXT NOT NULL,                 -- bot|me|steve|flow
  session_id TEXT, work_item_id TEXT, run_id TEXT,
  purpose TEXT NOT NULL,
  mode TEXT NOT NULL,                          -- env|file|stdin|internal|view|copy|browser_fill
  command TEXT,                                -- 命令原文（不含机密；含机密的部分打码）
  fields TEXT NOT NULL DEFAULT '[]',           -- 用到的字段 label
  decision_id TEXT, grant_id TEXT, device_id TEXT NOT NULL DEFAULT 'hub',
  status TEXT NOT NULL,                        -- pending|denied|running|ok|failed|expired
  exit_code INTEGER, created INTEGER NOT NULL, finished INTEGER);
CREATE INDEX IF NOT EXISTS secret_uses_item ON secret_uses(workspace_id, item_id, created);

CREATE TABLE IF NOT EXISTS passkeys (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, user_id TEXT NOT NULL REFERENCES users(id),
  name TEXT NOT NULL, rp_id TEXT NOT NULL, credential TEXT NOT NULL,   -- webauthn-rs 序列化的 Passkey（公钥，不是机密）
  created INTEGER NOT NULL, last_used INTEGER);
