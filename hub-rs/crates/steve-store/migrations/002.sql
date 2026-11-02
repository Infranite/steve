
  ALTER TABLE workspaces ADD COLUMN description TEXT NOT NULL DEFAULT '';
  ALTER TABLE workspaces ADD COLUMN item_prefix TEXT NOT NULL DEFAULT 'ST';
  ALTER TABLE workspaces ADD COLUMN sort INTEGER NOT NULL DEFAULT 0;
  CREATE TABLE workspace_members (workspace_id TEXT NOT NULL REFERENCES workspaces(id), user_id TEXT NOT NULL REFERENCES users(id), role TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (workspace_id, user_id));
  INSERT INTO workspace_members (workspace_id, user_id, role, created) SELECT workspace_id, id, role, created FROM users;

  CREATE TABLE projects (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id), name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
    device_id TEXT NOT NULL DEFAULT 'hub', path TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'dir', worktree INTEGER NOT NULL DEFAULT 0, default_bot TEXT,
    release TEXT, archived INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL, UNIQUE (workspace_id, name));

  ALTER TABLE bots ADD COLUMN persona TEXT NOT NULL DEFAULT '';
  ALTER TABLE bots ADD COLUMN proactive INTEGER NOT NULL DEFAULT 1;
  ALTER TABLE bots ADD COLUMN avatar TEXT NOT NULL DEFAULT '{}';
  ALTER TABLE bots ADD COLUMN default_project TEXT;
  ALTER TABLE bots ADD COLUMN prompt TEXT;
  ALTER TABLE bots ADD COLUMN device TEXT;
  ALTER TABLE bots ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
  CREATE TABLE bot_workspaces (bot_id TEXT NOT NULL REFERENCES bots(id), workspace_id TEXT NOT NULL REFERENCES workspaces(id), duty TEXT NOT NULL DEFAULT '', created INTEGER NOT NULL, PRIMARY KEY (bot_id, workspace_id));
  INSERT INTO bot_workspaces (bot_id, workspace_id, created) SELECT id, workspace_id, updated FROM bots;
  CREATE TABLE bot_memories (id TEXT PRIMARY KEY, bot_id TEXT NOT NULL REFERENCES bots(id), workspace_id TEXT NOT NULL, text TEXT NOT NULL, source TEXT NOT NULL, created INTEGER NOT NULL);
  CREATE INDEX bot_memories_bot ON bot_memories(bot_id, workspace_id, created);

  ALTER TABLE items ADD COLUMN project_id TEXT REFERENCES projects(id);
  ALTER TABLE items ADD COLUMN star INTEGER NOT NULL DEFAULT 0;
  ALTER TABLE items ADD COLUMN due TEXT;
  ALTER TABLE items ADD COLUMN snooze TEXT;
  ALTER TABLE items ADD COLUMN source TEXT NOT NULL DEFAULT '["对话","chat"]';
  ALTER TABLE items ADD COLUMN parent TEXT;
  CREATE TABLE item_deps (item_id TEXT NOT NULL REFERENCES items(id), dep_id TEXT NOT NULL REFERENCES items(id), created INTEGER NOT NULL, PRIMARY KEY (item_id, dep_id));
  CREATE TABLE tags (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL, cat TEXT NOT NULL, description TEXT NOT NULL, created_by TEXT NOT NULL, created INTEGER NOT NULL, UNIQUE (workspace_id, name));
  CREATE TABLE item_tags (item_id TEXT NOT NULL REFERENCES items(id), tag_id TEXT NOT NULL REFERENCES tags(id), PRIMARY KEY (item_id, tag_id));

  CREATE TABLE settings (workspace_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (workspace_id, key));
  CREATE TABLE devices (id TEXT PRIMARY KEY, name TEXT NOT NULL, roles TEXT NOT NULL, os TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT '正常',
    max_sessions INTEGER NOT NULL DEFAULT 4, seen INTEGER NOT NULL, created INTEGER NOT NULL);
  ALTER TABLE sessions ADD COLUMN device_id TEXT NOT NULL DEFAULT 'hub';
  ALTER TABLE sessions ADD COLUMN source TEXT NOT NULL DEFAULT 'Steve';
  ALTER TABLE sessions ADD COLUMN readonly INTEGER NOT NULL DEFAULT 0;
  ALTER TABLE sessions ADD COLUMN import_key TEXT;
  ALTER TABLE decisions ADD COLUMN undo TEXT;
  ALTER TABLE messages ADD COLUMN meta TEXT;
  