-- Knowledge redefinition:
-- 1. Project-level AGENTS.md proposals live in their own table, separate from personal knowledge.
-- 2. Codebase engineering facts extracted from items or unanchored git-project candidates migrate
--    into project_proposals (never modifying any repository directly).
-- 3. Owner preferences/rules previously saved as fact are reclassified and relabeled.
-- 4. Work-record sources (items, chat) are withdrawn from the knowledge index while keeping the
--    underlying items and messages tables intact.

CREATE TABLE IF NOT EXISTS project_proposals (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL DEFAULT 'agents_md',
    text TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    evidence TEXT NOT NULL DEFAULT '',
    origin_kind TEXT NOT NULL DEFAULT '',
    origin_id TEXT,
    origin_label TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending',
    decision_id TEXT,
    writer TEXT NOT NULL DEFAULT 'steve',
    created_by TEXT NOT NULL DEFAULT '',
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS project_proposals_ws_proj
    ON project_proposals(workspace_id, project_id, status, updated);
CREATE INDEX IF NOT EXISTS project_proposals_ws_status
    ON project_proposals(workspace_id, status, updated);

CREATE TEMP TABLE _mig125_eng (
    mem_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL
);

INSERT INTO _mig125_eng (mem_id, project_id)
SELECT m.id, proj_id
  FROM (
    SELECT
        m.id,
        COALESCE(
            (SELECT a.object_id FROM mem_about a
               JOIN projects p ON p.id = a.object_id AND p.workspace_id = m.workspace_id AND p.kind = 'git'
              WHERE a.mem_id = m.id AND a.object_type = 'project'
              LIMIT 1),
            (SELECT i.project_id FROM items i
               JOIN projects p ON p.id = i.project_id AND p.workspace_id = m.workspace_id AND p.kind = 'git'
              WHERE m.source_type = 'item' AND i.id = m.source_id
              LIMIT 1),
            (SELECT p.id FROM projects p
              WHERE p.workspace_id = m.workspace_id AND p.kind = 'git' AND p.archived = 0
                AND instr(lower(m.text), lower(p.name)) > 0
              ORDER BY length(p.name) DESC
              LIMIT 1)
        ) AS proj_id
      FROM mems m
     WHERE m.kind = 'fact'
       AND m.source_type != 'me'
       AND NOT EXISTS (
           SELECT 1 FROM mem_about a WHERE a.mem_id = m.id AND a.object_type = 'party'
       )
       AND (
           m.source_type = 'item'
           OR (m.status = 'candidate' AND NOT EXISTS (SELECT 1 FROM mem_about a WHERE a.mem_id = m.id))
       )
  ) AS picked
  JOIN mems m ON m.id = picked.id
 WHERE picked.proj_id IS NOT NULL;

INSERT OR IGNORE INTO project_proposals (
    id, workspace_id, project_id, kind, text, reason, evidence,
    origin_kind, origin_id, origin_label, status, decision_id,
    writer, created_by, created, updated
)
SELECT
    'prp_' || m.id,
    m.workspace_id,
    e.project_id,
    'agents_md',
    m.text,
    '',
    COALESCE((SELECT c.evidence FROM mem_candidates c WHERE c.mem_id = m.id), ''),
    COALESCE((SELECT c.origin FROM mem_candidates c WHERE c.mem_id = m.id), m.source_type),
    COALESCE((SELECT c.origin_id FROM mem_candidates c WHERE c.mem_id = m.id), m.source_id),
    COALESCE(NULLIF((SELECT c.origin_label FROM mem_candidates c WHERE c.mem_id = m.id), ''), m.source_label),
    'pending',
    NULL,
    m.writer,
    m.created_by,
    m.created,
    m.updated
  FROM _mig125_eng e
  JOIN mems m ON m.id = e.mem_id;

UPDATE decisions
   SET status = 'superseded', resolved = created
 WHERE status = 'open'
   AND subject_type = 'mem'
   AND subject_id IN (SELECT mem_id FROM _mig125_eng);

DELETE FROM kb_fts
 WHERE rowid IN (
     SELECT id FROM kb_chunks
      WHERE obj_type = 'mem' AND obj_id IN (SELECT mem_id FROM _mig125_eng)
 );
DELETE FROM kb_vectors
 WHERE chunk_id IN (
     SELECT id FROM kb_chunks
      WHERE obj_type = 'mem' AND obj_id IN (SELECT mem_id FROM _mig125_eng)
 );
DELETE FROM kb_chunks
 WHERE obj_type = 'mem' AND obj_id IN (SELECT mem_id FROM _mig125_eng);
DELETE FROM mem_candidates WHERE mem_id IN (SELECT mem_id FROM _mig125_eng);
DELETE FROM mem_supersedes
 WHERE mem_id IN (SELECT mem_id FROM _mig125_eng)
    OR old_id IN (SELECT mem_id FROM _mig125_eng);
DELETE FROM mem_about WHERE mem_id IN (SELECT mem_id FROM _mig125_eng);
DELETE FROM mems WHERE id IN (SELECT mem_id FROM _mig125_eng);

DROP TABLE _mig125_eng;

-- Reclassify owner rules and preferences previously recorded as fact.
UPDATE mems
   SET kind = 'rule'
 WHERE kind = 'fact'
   AND source_type = 'me'
   AND (
       instr(text, '先建') > 0
       OR instr(text, '要显示') > 0
       OR instr(text, '必须') > 0
       OR instr(text, '一律') > 0
       OR instr(text, '禁止') > 0
   );

UPDATE mems
   SET kind = 'preference'
 WHERE kind = 'fact'
   AND source_type = 'me'
   AND (
       instr(text, '希望') > 0
       OR instr(text, '优先') > 0
       OR instr(text, '要有') > 0
       OR instr(text, '要分开') > 0
   );

UPDATE kb_chunks
   SET label = '长期记忆' || substr(label, 5)
 WHERE obj_type = 'mem'
   AND label LIKE '知识条目%'
   AND obj_id IN (SELECT id FROM mems WHERE kind IN ('preference', 'rule'));

-- Withdraw items and chat from the knowledge index; original items and messages remain untouched.
DELETE FROM kb_fts
 WHERE rowid IN (SELECT id FROM kb_chunks WHERE source_kind IN ('items', 'chat'));
DELETE FROM kb_vectors
 WHERE chunk_id IN (SELECT id FROM kb_chunks WHERE source_kind IN ('items', 'chat'));
DELETE FROM kb_chunks
 WHERE source_kind IN ('items', 'chat');
DELETE FROM kb_doc_about
 WHERE doc_id IN (
     SELECT d.id FROM kb_docs d
       JOIN kb_sources s ON s.id = d.source_id
      WHERE s.kind IN ('items', 'chat')
 );
DELETE FROM kb_docs
 WHERE source_id IN (SELECT id FROM kb_sources WHERE kind IN ('items', 'chat'));
DELETE FROM kb_sources
 WHERE kind IN ('items', 'chat');
DELETE FROM kb_marks
 WHERE name LIKE 'sync:chat:%';
