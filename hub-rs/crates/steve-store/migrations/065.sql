-- 65: the capabilities library (能力). MCP servers, skills and remote A2A agents are defined once per
-- workspace and assigned per Bot (`bot_capabilities`); a Bot's sessions get the enabled entries it is
-- assigned. 62's `bot_mcp_servers` and 63's `bot_skills` move here with their ids, so a secret's
-- reference (`<server>#<label>`) and its vault item stay the same; a name two Bots both used in one
-- workspace gets `-2`, `-3`… on the later ones. The old tables go.
CREATE TABLE IF NOT EXISTS mcp_servers (
  id TEXT PRIMARY KEY,                          -- bmcp_…
  workspace_id TEXT NOT NULL,                   -- its library; its credential item is in this vault
  name TEXT NOT NULL,                           -- the MCP server name the harness sees, once per workspace
  transport TEXT NOT NULL,                      -- stdio | http | sse
  command TEXT,                                 -- stdio
  args TEXT NOT NULL DEFAULT '[]',              -- stdio, JSON list of strings
  env TEXT NOT NULL DEFAULT '[]',               -- stdio: [{"name","value"} | {"name","secret":true}]
  url TEXT,                                     -- http | sse
  headers TEXT NOT NULL DEFAULT '[]',           -- http | sse, like env
  credential_item TEXT,                         -- vault_items.id holding the secret values, if any
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS mcp_servers_ws ON mcp_servers(workspace_id, name);

CREATE TABLE IF NOT EXISTS skills (
  id TEXT PRIMARY KEY,                          -- bskill_…
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,                           -- the skill's directory and front matter name
  description TEXT NOT NULL,                    -- when to use it
  content TEXT NOT NULL DEFAULT '',             -- the SKILL.md body (Markdown)
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS skills_ws ON skills(workspace_id, name);

-- a remote agent, added by its Agent Card URL; `card` is the card as last fetched. `auth`:
-- {"scheme":"none"} | {"scheme":"bearer"} | {"scheme":"header","header":"X-Api-Key"}; the secret itself is
-- the field `auth` of `credential_item` in the vault.
CREATE TABLE IF NOT EXISTS a2a_agents (
  id TEXT PRIMARY KEY,                          -- a2a_…
  workspace_id TEXT NOT NULL,
  name TEXT NOT NULL,                           -- the key of its tool (steve_a2a_<name>), once per workspace
  card_url TEXT NOT NULL,
  card TEXT NOT NULL DEFAULT '{}',
  auth TEXT NOT NULL DEFAULT '{"scheme":"none"}',
  credential_item TEXT,
  fetched INTEGER NOT NULL,
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS a2a_agents_ws ON a2a_agents(workspace_id, name);

-- which library entries a Bot uses, and whether each is switched on for it
CREATE TABLE IF NOT EXISTS bot_capabilities (
  bot_id TEXT NOT NULL REFERENCES bots(id),
  kind TEXT NOT NULL,                           -- mcp | skill | a2a
  entry_id TEXT NOT NULL,                       -- mcp_servers.id | skills.id | a2a_agents.id
  enabled INTEGER NOT NULL DEFAULT 1,
  created INTEGER NOT NULL,
  PRIMARY KEY (bot_id, kind, entry_id));
CREATE INDEX IF NOT EXISTS bot_capabilities_entry ON bot_capabilities(kind, entry_id);

INSERT INTO mcp_servers (id, workspace_id, name, transport, command, args, env, url, headers, credential_item, created, updated)
  SELECT id, workspace_id,
         CASE WHEN n = 1 THEN name ELSE name || '-' || n END,
         transport, command, args, env, url, headers, credential_item, created, updated
    FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY workspace_id, lower(name) ORDER BY created, id) AS n
            FROM bot_mcp_servers);
INSERT INTO bot_capabilities (bot_id, kind, entry_id, enabled, created)
  SELECT bot_id, 'mcp', id, enabled, created FROM bot_mcp_servers;

INSERT INTO skills (id, workspace_id, name, description, content, created, updated)
  SELECT id, workspace_id,
         CASE WHEN n = 1 THEN name ELSE name || '-' || n END,
         description, content, created, updated
    FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY workspace_id, name ORDER BY created, id) AS n
            FROM bot_skills);
INSERT INTO bot_capabilities (bot_id, kind, entry_id, enabled, created)
  SELECT bot_id, 'skill', id, enabled, created FROM bot_skills;

DROP TABLE bot_mcp_servers;
DROP TABLE bot_skills;
