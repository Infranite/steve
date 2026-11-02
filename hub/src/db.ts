/* SQLite storage. All access goes through repo.ts so the store can move to Postgres without touching callers.
 * Every business table carries workspace_id from day one (multi-tenant later, one workspace now). */
import Database from "better-sqlite3"
import { join } from "node:path"
import { config } from "./config.js"

export const db = new Database(join(config.dataDir, "steve.sqlite3"))
db.pragma("journal_mode = WAL")
db.pragma("foreign_keys = ON")
db.pragma("busy_timeout = 5000")

const MIGRATIONS: string[] = [
  /* 1 — bootstrap slice of the domain model */
  `
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
  `,
  /* 2 — workspaces own projects; bots join workspaces; tags, dependencies, memories, settings, devices */
  `
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
  `,
  /* 3 — attachments: uploaded files, referenced from messages, item comments, queued prompts and session events */
  `
  CREATE TABLE files (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, sha256 TEXT NOT NULL, name TEXT NOT NULL, mime TEXT NOT NULL, size INTEGER NOT NULL, created_by TEXT NOT NULL, created INTEGER NOT NULL);
  CREATE INDEX files_ws ON files(workspace_id, sha256);
  ALTER TABLE timeline ADD COLUMN files TEXT;
  ALTER TABLE queued ADD COLUMN files TEXT;
  `,
]

db.exec("CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL)")
const cur = (db.prepare("SELECT v FROM schema_version").get() as { v: number } | undefined)?.v ?? 0
/* a database newer than this code (e.g. after a release rollback) is opened as is: migrations are additive, and writing
 * the version down would make the newer code rerun a migration that already ran */
if (cur > MIGRATIONS.length) console.warn(`database schema v${cur} is newer than this Hub (v${MIGRATIONS.length}); opening it unchanged`)
else db.transaction(() => {
  for (let i = cur; i < MIGRATIONS.length; i++) db.exec(MIGRATIONS[i])
  if (cur === 0) db.prepare("INSERT INTO schema_version (v) VALUES (?)").run(MIGRATIONS.length)
  else db.prepare("UPDATE schema_version SET v = ?").run(MIGRATIONS.length)
})()

/* TS remains able to open the Rust Hub's newer schema without changing its migration version. */
for (const table of ["bots", "sessions"]) {
  if (!(db.prepare(`PRAGMA table_info(${table})`).all() as { name: string }[]).some((c) => c.name === "reasoning_effort"))
    db.exec(`ALTER TABLE ${table} ADD COLUMN reasoning_effort TEXT`)
}

export const now = () => Date.now()
export const json = <T>(s: string | null | undefined, d: T): T => { try { return s ? (JSON.parse(s) as T) : d } catch { return d } }
