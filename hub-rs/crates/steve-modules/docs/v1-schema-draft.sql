-- Draft for M0 (migration 005): the tables slice V1 (vault keys, envelope encryption, rotation) reads
-- and writes. Copied verbatim from steve-rs-plan/modules/accounts-vault.md §1; M0 owns the real 005.sql.
-- vault_items.account_id references accounts(id), so 005 must create `accounts` before `vault_items`
-- (the V1 tests create a stub `accounts` table first). Not a migration: nothing registers this file.

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

