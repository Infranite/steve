-- Keep the existing skill row as the editor head; execution reads only the published snapshot.
ALTER TABLE skills ADD COLUMN head_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE skills ADD COLUMN published_version INTEGER;
ALTER TABLE skills ADD COLUMN publication INTEGER NOT NULL DEFAULT 0;

CREATE TABLE skill_versions (
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    content TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    saved_by TEXT,
    note TEXT NOT NULL DEFAULT '',
    first_published_at INTEGER,
    PRIMARY KEY (skill_id, version)
);
INSERT INTO skill_versions
    (skill_id, version, name, description, content, saved_at, first_published_at)
SELECT id, 1, name, description, content, updated, updated FROM skills;
UPDATE skills SET published_version = 1, publication = 1;

-- Proactive distribution: durable desired identity, and evidence from the authenticated Node.
-- No draft or Markdown is stored in the delivery queue.
CREATE TABLE node_skill_inventory (
    node_id TEXT PRIMARY KEY,
    link TEXT NOT NULL,
    features TEXT NOT NULL,
    seen INTEGER NOT NULL
);
CREATE TABLE node_skill_targets (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    bot_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    adapter TEXT NOT NULL,
    generation INTEGER NOT NULL,
    digest TEXT NOT NULL,
    acknowledged TEXT,
    acknowledged_generation INTEGER,
    acknowledged_link TEXT,
    acknowledged_at INTEGER,
    versions TEXT NOT NULL DEFAULT '[]',
    error TEXT,
    PRIMARY KEY (workspace_id, bot_id, node_id)
);
CREATE INDEX node_skill_targets_node ON node_skill_targets(node_id);
ALTER TABLE runtime_sessions ADD COLUMN skill_snapshot_digest TEXT;
ALTER TABLE runtime_sessions ADD COLUMN skill_expected TEXT;
ALTER TABLE runtime_sessions ADD COLUMN skill_use_receipt TEXT;
