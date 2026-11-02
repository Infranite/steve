
  CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT NOT NULL, created INTEGER NOT NULL);
  CREATE TABLE users (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id), subject TEXT NOT NULL UNIQUE, name TEXT NOT NULL, email TEXT, role TEXT NOT NULL, created INTEGER NOT NULL);
  CREATE TABLE auth_sessions (token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id), created INTEGER NOT NULL, expires INTEGER NOT NULL);
  CREATE TABLE counters (workspace_id TEXT NOT NULL, name TEXT NOT NULL, value INTEGER NOT NULL, PRIMARY KEY (workspace_id, name));

  CREATE TABLE bots (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, name TEXT NOT NULL, role TEXT NOT NULL DEFAULT '', main INTEGER NOT NULL DEFAULT 0,
    harness TEXT NOT NULL, model TEXT, rules TEXT NOT NULL DEFAULT '', accept_mode TEXT NOT NULL DEFAULT 'manual', perm_mode TEXT NOT NULL DEFAULT 'default', project_dir TEXT, worktree INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);

  CREATE TABLE items (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, no INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'task', title TEXT NOT NULL,
    status TEXT NOT NULL, stage TEXT NOT NULL, bot_id TEXT REFERENCES bots(id), project_dir TEXT, cwd TEXT, branch TEXT,
    plan TEXT NOT NULL DEFAULT '[]', now TEXT NOT NULL DEFAULT '', accept_mode TEXT, priority TEXT NOT NULL DEFAULT '中',
    archived INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL, UNIQUE (workspace_id, no));
  CREATE TABLE item_goals (item_id TEXT NOT NULL REFERENCES items(id), version INTEGER NOT NULL, text TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (item_id, version));
  CREATE TABLE timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, item_id TEXT NOT NULL REFERENCES items(id), ts INTEGER NOT NULL, actor TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL, session_id TEXT);
  CREATE INDEX timeline_item ON timeline(item_id, id);
  CREATE TABLE artifacts (id TEXT PRIMARY KEY, item_id TEXT NOT NULL REFERENCES items(id), kind TEXT NOT NULL, title TEXT NOT NULL, location TEXT, created INTEGER NOT NULL);

  CREATE TABLE sessions (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT REFERENCES items(id), no INTEGER NOT NULL DEFAULT 1, bot_id TEXT, harness TEXT NOT NULL, model TEXT,
    cwd TEXT NOT NULL, acp_session_id TEXT, status TEXT NOT NULL, used INTEGER, size INTEGER, cost REAL, token_hash TEXT, title TEXT NOT NULL DEFAULT '',
    created INTEGER NOT NULL, updated INTEGER NOT NULL);
  CREATE TABLE events (seq INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id), ts INTEGER NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL);
  CREATE INDEX events_session ON events(session_id, seq);
  CREATE TABLE queued (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id), text TEXT NOT NULL, via TEXT NOT NULL, created INTEGER NOT NULL);

  CREATE TABLE decisions (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, kind TEXT NOT NULL, subject_type TEXT NOT NULL, subject_id TEXT NOT NULL, item_id TEXT,
    session_id TEXT, question TEXT NOT NULL, draft TEXT NOT NULL DEFAULT '', options TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL, result TEXT,
    requested_by TEXT NOT NULL, created INTEGER NOT NULL, resolved INTEGER);
  CREATE INDEX decisions_open ON decisions(workspace_id, status, created);

  CREATE TABLE messages (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, space TEXT NOT NULL, author TEXT NOT NULL, text TEXT NOT NULL, item_id TEXT, created INTEGER NOT NULL);
  CREATE INDEX messages_space ON messages(workspace_id, space, created);

  CREATE TABLE links (workspace_id TEXT NOT NULL, subject_type TEXT NOT NULL, subject_id TEXT NOT NULL, predicate TEXT NOT NULL, object_type TEXT NOT NULL, object_id TEXT NOT NULL,
    props TEXT NOT NULL DEFAULT '{}', source TEXT NOT NULL, confidence REAL NOT NULL DEFAULT 1, created INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, subject_type, subject_id, predicate, object_type, object_id));
  CREATE TABLE revisions (id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, object_type TEXT NOT NULL, object_id TEXT NOT NULL, version INTEGER NOT NULL, patch TEXT NOT NULL, author TEXT NOT NULL, created INTEGER NOT NULL);
  CREATE TABLE audit (id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL, ts INTEGER NOT NULL, actor TEXT NOT NULL, action TEXT NOT NULL, target TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '{}');
  