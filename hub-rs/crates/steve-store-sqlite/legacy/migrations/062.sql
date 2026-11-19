-- 62: a Bot's own MCP servers, handed to its harness in every session's ACP `session/new|load`
-- (`mcpServers`) next to Steve's own tools. `env` and `headers` are JSON lists of
-- `{"name","value"}` or, for a secret, `{"name","secret":true}`: a secret's value is never here but in
-- the workspace's system vault, as a field of the server's `credential_item` (labels `env:<name>` and
-- `header:<name>`), taken by the session's Node when the harness opens. New table only.
CREATE TABLE IF NOT EXISTS bot_mcp_servers (
  id TEXT PRIMARY KEY,                          -- bmcp_…
  workspace_id TEXT NOT NULL,                   -- where it was added: its credential item is in this vault
  bot_id TEXT NOT NULL REFERENCES bots(id),
  name TEXT NOT NULL,                           -- the MCP server name the harness sees
  transport TEXT NOT NULL,                      -- stdio | http | sse
  command TEXT,                                 -- stdio
  args TEXT NOT NULL DEFAULT '[]',              -- stdio, JSON list of strings
  env TEXT NOT NULL DEFAULT '[]',               -- stdio
  url TEXT,                                     -- http | sse
  headers TEXT NOT NULL DEFAULT '[]',           -- http | sse
  credential_item TEXT,                         -- vault_items.id holding the secret values, if any
  enabled INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (bot_id, name));
