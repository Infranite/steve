
  CREATE TABLE files (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, sha256 TEXT NOT NULL, name TEXT NOT NULL, mime TEXT NOT NULL, size INTEGER NOT NULL, created_by TEXT NOT NULL, created INTEGER NOT NULL);
  CREATE INDEX files_ws ON files(workspace_id, sha256);
  ALTER TABLE timeline ADD COLUMN files TEXT;
  ALTER TABLE queued ADD COLUMN files TEXT;
  