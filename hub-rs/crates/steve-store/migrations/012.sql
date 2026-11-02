-- 12: spaces outlive their connection (channels-personas.md §2.2 “移除接入账号：…空间 status=left，
-- 空间设置删除；消息和事项保留”). Rust Hub only.
-- SQLite cannot alter a foreign key, so `spaces` is rebuilt (create new, copy, drop, rename,
-- recreate the index; it has no triggers or FTS): `connection_id` becomes nullable and is set to
-- NULL when the connection row goes, and `account_id` remembers whose account the space was on,
-- so a disconnected personal account's spaces stay private (`spaces::VISIBLE`).
-- space_members, space_settings and chan_messages reference `spaces(id)` by name and keep pointing
-- at the rebuilt table. db.rs runs this with foreign_keys off and checks foreign_key_check before
-- committing (the SQLite “making other kinds of table schema changes” procedure).
CREATE TABLE spaces_new (
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
INSERT INTO spaces_new (id, workspace_id, system_id, connection_id, kind, external_id, name, member_count,
    has_external, meeting, status, last_message, created, updated)
  SELECT id, workspace_id, system_id, connection_id, kind, external_id, name, member_count,
    has_external, meeting, status, last_message, created, updated FROM spaces;
DROP TABLE spaces;
ALTER TABLE spaces_new RENAME TO spaces;
CREATE INDEX IF NOT EXISTS spaces_conn ON spaces(workspace_id, connection_id, status);
