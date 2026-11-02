-- 8: knowledge: sources, docs, chunks, FTS index (contentless_delete, needs SQLite >= 3.43), vectors, uses, memory candidates, marks.
-- Spec: steve-rs-plan/modules/knowledge.md §1, DDL verbatim. Rust Hub only; additive, nothing existing is altered.

CREATE TABLE IF NOT EXISTS kb_sources (
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
CREATE INDEX IF NOT EXISTS kb_sources_ws ON kb_sources(workspace_id, kind);

CREATE TABLE IF NOT EXISTS kb_docs (
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
CREATE INDEX IF NOT EXISTS kb_docs_ws ON kb_docs(workspace_id, status, updated);

CREATE TABLE IF NOT EXISTS kb_doc_about (
  doc_id TEXT NOT NULL REFERENCES kb_docs(id), workspace_id TEXT NOT NULL,
  object_type TEXT NOT NULL, object_id TEXT NOT NULL,          -- 同 mem_about：party|item|space|bot|persona|me|project
  PRIMARY KEY (doc_id, object_type, object_id));
CREATE INDEX IF NOT EXISTS kb_doc_about_obj ON kb_doc_about(workspace_id, object_type, object_id);

-- 检索单元：文档片段、记忆、Bot 记忆。可见范围和级别冗余存在这里，过滤只看这一张表
CREATE TABLE IF NOT EXISTS kb_chunks (
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
CREATE INDEX IF NOT EXISTS kb_chunks_ws ON kb_chunks(workspace_id, obj_type, obj_id);

CREATE VIRTUAL TABLE IF NOT EXISTS kb_fts USING fts5(
  seg, title, tokenize = 'unicode61 remove_diacritics 2', content = '', contentless_delete = 1);

CREATE TABLE IF NOT EXISTS kb_vectors (                       -- 第二期（§3.4），第一期为空表
  chunk_id INTEGER PRIMARY KEY REFERENCES kb_chunks(id),
  model TEXT NOT NULL, dim INTEGER NOT NULL, vec BLOB NOT NULL, created INTEGER NOT NULL);

-- 依据记录（非渠道场景；渠道出站仍写 reply_basis）
CREATE TABLE IF NOT EXISTS kb_uses (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  consumer_type TEXT NOT NULL,                 -- session|chat|draft|flow|search
  consumer_id TEXT NOT NULL,                   -- ses_… | msg id | md_… | run_… | user id
  query TEXT NOT NULL,                         -- 检索词（已过 guard::scan）
  hits TEXT NOT NULL,                          -- [{"n":1,"obj_type","obj_id","seq","score","level","label"}]，不含正文
  created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS kb_uses_consumer ON kb_uses(workspace_id, consumer_type, consumer_id);

-- 候选记忆的附加信息（候选本身是 mems.status='candidate'）
CREATE TABLE IF NOT EXISTS mem_candidates (
  mem_id TEXT PRIMARY KEY REFERENCES mems(id), workspace_id TEXT NOT NULL,
  origin TEXT NOT NULL,                        -- item|chat|mail|tool|space
  origin_id TEXT NOT NULL, origin_label TEXT NOT NULL,          -- “ST-12 交付时”“10-31 的对话”
  evidence TEXT NOT NULL DEFAULT '',           -- 原话摘录（≤300 字）
  supersedes TEXT,                             -- 与之冲突的已确认记忆；确认后它变 stale
  created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS kb_marks (                         -- 抽取、同步的进度
  workspace_id TEXT NOT NULL, name TEXT NOT NULL,              -- extract:item:ST-12、extract:chat:2026-10-31、sync:bot_mems
  value TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (workspace_id, name));
