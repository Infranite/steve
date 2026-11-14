-- 69: tool search. A session lists Steve's core tools and the ones its Bot pinned; every other
-- tool is found with steve_tools_search and run with steve_tool_call.
-- `bot_tool_pins`: what a Bot keeps loaded in every session. `kind`: `tool` (a tool by its name, a
-- built-in one), `mcp` (every tool of a library MCP server, `target` = mcp_servers.id) or `a2a` (a
-- remote agent's tool, `target` = a2a_agents.id).
CREATE TABLE IF NOT EXISTS bot_tool_pins (
  bot_id TEXT NOT NULL REFERENCES bots(id),
  kind TEXT NOT NULL CHECK (kind IN ('tool', 'mcp', 'a2a')),
  target TEXT NOT NULL,
  created INTEGER NOT NULL,
  PRIMARY KEY (bot_id, kind, target));
-- `tool_usage`: per Bot, tool and day (days since the Unix epoch, UTC), how often the tool came up in
-- a search and how often it was called (directly or through steve_tool_call). Kept for 90 days.
CREATE TABLE IF NOT EXISTS tool_usage (
  bot_id TEXT NOT NULL,
  tool TEXT NOT NULL,
  day INTEGER NOT NULL,
  searched INTEGER NOT NULL DEFAULT 0,
  called INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (bot_id, tool, day));
CREATE INDEX IF NOT EXISTS tool_usage_day ON tool_usage(day);
