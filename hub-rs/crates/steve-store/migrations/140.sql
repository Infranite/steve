-- 140: File-as-single-source-of-truth memory architecture (Memory Card 2).
-- 1. Sources carry explicit role (`knowledge` | `memory`) and write_mode (`read_only` | `propose` | `direct_write`).
-- 2. Sidecar-only `memory_entries` (no duplicate prose body), `memory_evidence`, 3-gate `observations` buffer,
--    permanent `rejected_patterns` tombstones, and auditable/revertible `memory_revisions`.
-- 3. `project_proposals` supports nullable `project_id` and `target_path` for cross-repo `skill_md` proposals.
-- 4. Supersede legacy open memory confirmation cards (`decisions.subject_type = 'mem'`).

ALTER TABLE kb_sources ADD COLUMN role TEXT NOT NULL DEFAULT 'knowledge'
    CHECK (role IN ('knowledge', 'memory'));
ALTER TABLE kb_sources ADD COLUMN write_mode TEXT NOT NULL DEFAULT 'read_only'
    CHECK (write_mode IN ('read_only', 'propose', 'direct_write'));

UPDATE kb_sources
   SET role = 'memory', write_mode = 'direct_write'
 WHERE kind = 'mems';

CREATE TABLE memory_entries (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    source_id TEXT NOT NULL REFERENCES kb_sources(id) ON DELETE CASCADE,
    doc_id TEXT REFERENCES kb_docs(id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,
    block_anchor TEXT NOT NULL,
    obsidian_id TEXT,
    heading_path TEXT NOT NULL DEFAULT '',
    line_start INTEGER NOT NULL DEFAULT 1,
    line_end INTEGER NOT NULL DEFAULT 1,
    content_sha256 TEXT NOT NULL,
    simhash INTEGER NOT NULL DEFAULT 0,
    bigram_fingerprint TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'fact'
        CHECK (kind IN ('rule', 'preference', 'fact', 'event', 'overview')),
    subject_type TEXT NOT NULL DEFAULT 'me',
    subject_id TEXT NOT NULL DEFAULT 'me',
    scope_hint TEXT NOT NULL DEFAULT '',
    confidence_tier TEXT NOT NULL DEFAULT 'verified'
        CHECK (confidence_tier IN ('verified', 'corroborated', 'provisional', 'contested')),
    origin_class TEXT NOT NULL DEFAULT 'owner_file'
        CHECK (origin_class IN (
            'owner_file',
            'owner_explicit',
            'owner_rework',
            'owner_draft_diff',
            'owner_edited',
            'implicit_pattern',
            'external_entity'
        )),
    source_tier INTEGER NOT NULL DEFAULT 1 CHECK (source_tier IN (1, 2, 3)),
    episode_count INTEGER NOT NULL DEFAULT 1,
    correction_count INTEGER NOT NULL DEFAULT 0,
    has_conflict INTEGER NOT NULL DEFAULT 0 CHECK (has_conflict IN (0, 1)),
    level TEXT NOT NULL DEFAULT 'reasoning'
        CHECK (level IN ('public', 'conclusion', 'reasoning', 'never')),
    visibility TEXT NOT NULL DEFAULT 'private',
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'stale', 'deleted')),
    use_count INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER,
    verified_at INTEGER,
    created_by TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (source_id, rel_path, block_anchor)
);

CREATE INDEX memory_entries_ws_status
    ON memory_entries(workspace_id, status, updated_at);
CREATE INDEX memory_entries_source_path
    ON memory_entries(source_id, rel_path, status);
CREATE INDEX memory_entries_subject
    ON memory_entries(workspace_id, subject_type, subject_id, status);
CREATE INDEX memory_entries_sha
    ON memory_entries(workspace_id, content_sha256);

CREATE TABLE memory_evidence (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    entry_id TEXT NOT NULL REFERENCES memory_entries(id) ON DELETE CASCADE,
    origin_kind TEXT NOT NULL,
    origin_id TEXT NOT NULL DEFAULT '',
    origin_label TEXT NOT NULL DEFAULT '',
    episode_key TEXT NOT NULL,
    quote_hash TEXT NOT NULL DEFAULT '',
    rationale TEXT NOT NULL DEFAULT '',
    source_tier INTEGER NOT NULL DEFAULT 1 CHECK (source_tier IN (1, 2, 3)),
    created_at INTEGER NOT NULL,
    UNIQUE (entry_id, episode_key)
);

CREATE INDEX memory_evidence_entry
    ON memory_evidence(entry_id, created_at);

CREATE TABLE observations (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    kind TEXT NOT NULL CHECK (kind IN ('rule', 'preference', 'fact', 'event')),
    subject_type TEXT NOT NULL DEFAULT 'me',
    subject_id TEXT NOT NULL DEFAULT 'me',
    scope_hint TEXT NOT NULL DEFAULT '',
    target_rel_path TEXT NOT NULL,
    proposed_text TEXT NOT NULL,
    why TEXT NOT NULL DEFAULT '',
    content_sha256 TEXT NOT NULL,
    simhash INTEGER NOT NULL DEFAULT 0,
    bigram_fingerprint TEXT NOT NULL DEFAULT '',
    origin_class TEXT NOT NULL,
    source_tier INTEGER NOT NULL CHECK (source_tier IN (1, 2, 3)),
    episode_keys TEXT NOT NULL DEFAULT '[]',
    episode_count INTEGER NOT NULL DEFAULT 1,
    evidence_refs TEXT NOT NULL DEFAULT '[]',
    level TEXT NOT NULL DEFAULT 'reasoning'
        CHECK (level IN ('public', 'conclusion', 'reasoning', 'never')),
    visibility TEXT NOT NULL DEFAULT 'private',
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'promoted', 'expired', 'rejected')),
    promoted_entry_id TEXT REFERENCES memory_entries(id) ON DELETE SET NULL,
    created_by TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX observations_ws_status
    ON observations(workspace_id, status, expires_at);
CREATE INDEX observations_ws_subject
    ON observations(workspace_id, subject_type, subject_id, status);

CREATE TABLE rejected_patterns (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    subject_type TEXT NOT NULL DEFAULT 'me',
    subject_id TEXT NOT NULL DEFAULT 'me',
    content_sha256 TEXT NOT NULL,
    simhash INTEGER NOT NULL DEFAULT 0,
    bigram_fingerprint TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL
        CHECK (reason IN ('owner_deleted', 'owner_reverted', 'superseded_rejected')),
    entry_id TEXT,
    created_by TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    UNIQUE (workspace_id, content_sha256)
);

CREATE INDEX rejected_patterns_ws
    ON rejected_patterns(workspace_id, subject_type, subject_id);

CREATE TABLE memory_revisions (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    source_id TEXT NOT NULL REFERENCES kb_sources(id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,
    entry_id TEXT REFERENCES memory_entries(id) ON DELETE SET NULL,
    op TEXT NOT NULL CHECK (op IN ('add', 'update', 'delete', 'overview_refresh')),
    write_mode TEXT NOT NULL CHECK (write_mode IN ('propose', 'direct_write')),
    status TEXT NOT NULL CHECK (status IN ('proposed', 'applied', 'reverted', 'dismissed')),
    heading_path TEXT NOT NULL DEFAULT '',
    before_block TEXT NOT NULL DEFAULT '',
    after_block TEXT NOT NULL DEFAULT '',
    diff_preview TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL DEFAULT '',
    origin_class TEXT NOT NULL DEFAULT '',
    actor TEXT NOT NULL DEFAULT 'steve',
    created_at INTEGER NOT NULL,
    applied_at INTEGER,
    reverted_at INTEGER
);

CREATE INDEX memory_revisions_ws_status
    ON memory_revisions(workspace_id, status, created_at);
CREATE INDEX memory_revisions_source_path
    ON memory_revisions(source_id, rel_path, created_at);

CREATE TABLE project_proposals_new (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL DEFAULT 'agents_md' CHECK (kind IN ('agents_md', 'skill_md')),
    target_path TEXT NOT NULL DEFAULT 'AGENTS.md',
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

INSERT INTO project_proposals_new (
    id, workspace_id, project_id, kind, target_path, text, reason, evidence,
    origin_kind, origin_id, origin_label, status, decision_id, writer, created_by, created, updated
)
SELECT
    id, workspace_id, project_id, kind, 'AGENTS.md', text, reason, evidence,
    origin_kind, origin_id, origin_label, status, decision_id, writer, created_by, created, updated
  FROM project_proposals;

DROP TABLE project_proposals;
ALTER TABLE project_proposals_new RENAME TO project_proposals;

CREATE INDEX project_proposals_ws_proj
    ON project_proposals(workspace_id, project_id, status, updated);
CREATE INDEX project_proposals_ws_status
    ON project_proposals(workspace_id, status, updated);

UPDATE decisions
   SET status = 'superseded',
       resolved = COALESCE(resolved, created)
 WHERE status = 'open'
   AND subject_type = 'mem';
