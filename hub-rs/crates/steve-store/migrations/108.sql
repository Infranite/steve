-- Opt-in dispatch. NULL/default absence preserves the pre-Team installation's routing.
CREATE TABLE bot_node_bindings (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
  -- Ordered. An empty configured list is closed, not legacy.
  nodes TEXT NOT NULL,
  PRIMARY KEY(workspace_id, bot_id)
);
CREATE TABLE bot_teams (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  members TEXT NOT NULL
);
CREATE INDEX bot_teams_workspace ON bot_teams(workspace_id);
CREATE TABLE bot_team_defaults (
  workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
  team_id TEXT NOT NULL REFERENCES bot_teams(id) ON DELETE CASCADE
);
CREATE TABLE item_dispatch (
  item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  -- A snapshot of the Team at creation. Current Bot bindings and workspace/Node grants still apply.
  team TEXT,
  source TEXT CHECK(source IN ('default', 'explicit', 'inherited')),
  last_decision TEXT
);

-- Existing paired Nodes retain the installation's legacy automatic spreading behavior. Nodes
-- paired after migration do not gain implicit dispatch simply by connecting and reporting.
CREATE TABLE legacy_spread_nodes (
  node_id TEXT PRIMARY KEY REFERENCES nodes(node_id) ON DELETE CASCADE
);
INSERT INTO legacy_spread_nodes (node_id)
  SELECT node_id FROM nodes WHERE state IN ('active','paused') AND local=0;
