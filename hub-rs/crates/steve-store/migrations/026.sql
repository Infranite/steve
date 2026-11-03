-- Personal conversation-list preferences. Closing never alters channel state or messages.
CREATE TABLE IF NOT EXISTS conversation_preferences (
 workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
 user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
 closed INTEGER NOT NULL DEFAULT 0 CHECK(closed IN (0, 1)),
 PRIMARY KEY (workspace_id, user_id, space_id));
