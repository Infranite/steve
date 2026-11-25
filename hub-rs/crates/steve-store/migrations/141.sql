-- 141: attribute usage_records, agent_turns, gateway_calls, and stat_daily to Node (`node_id`).

ALTER TABLE agent_turns ADD COLUMN node_id TEXT NOT NULL DEFAULT '';
ALTER TABLE gateway_calls ADD COLUMN node_id TEXT NOT NULL DEFAULT '';
ALTER TABLE usage_records ADD COLUMN node_id TEXT NOT NULL DEFAULT '';

-- Backfill historical node_id from runtime_sessions and gateway_job_accounts; unknown history stays ''.
UPDATE agent_turns
SET node_id = COALESCE(
  (SELECT r.node_id FROM runtime_sessions r WHERE r.session_id = agent_turns.session_id),
  ''
)
WHERE node_id = '';

UPDATE gateway_calls
SET node_id = COALESCE(
  (SELECT g.node_id
   FROM gateway_job_accounts g
   JOIN gateway_attempts a ON a.attempt_id = g.attempt_id
   WHERE a.call_id = gateway_calls.call_id AND g.node_id IS NOT NULL AND g.node_id <> ''
   ORDER BY a.ordinal DESC LIMIT 1),
  ''
)
WHERE node_id = '';

UPDATE usage_records
SET node_id = COALESCE(
  (SELECT r.node_id FROM runtime_sessions r WHERE r.session_id = usage_records.session_id),
  (SELECT gc.node_id FROM gateway_calls gc WHERE gc.call_id = usage_records.live_call_id),
  ''
)
WHERE node_id = '';

CREATE INDEX IF NOT EXISTS usage_ws_day_node ON usage_records(workspace_id, day, node_id);
CREATE INDEX IF NOT EXISTS usage_node_day ON usage_records(node_id, day);

-- Rebuild stat_daily with node_id in its primary key.
CREATE TABLE stat_daily_141 (
  workspace_id TEXT NOT NULL, day TEXT NOT NULL,
  source TEXT NOT NULL, purpose TEXT NOT NULL, harness TEXT NOT NULL, model TEXT NOT NULL,
  bot_id TEXT NOT NULL DEFAULT '', project_id TEXT NOT NULL DEFAULT '',
  node_id TEXT NOT NULL DEFAULT '',
  input INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
  cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0, reasoning INTEGER NOT NULL DEFAULT 0,
  hit_base INTEGER NOT NULL DEFAULT 0,
  cost_reported REAL NOT NULL DEFAULT 0, calls INTEGER NOT NULL DEFAULT 0, failed_calls INTEGER NOT NULL DEFAULT 0,
  turns INTEGER NOT NULL DEFAULT 0, turn_ms INTEGER NOT NULL DEFAULT 0, turn_errors INTEGER NOT NULL DEFAULT 0,
  retries INTEGER NOT NULL DEFAULT 0, interrupted INTEGER NOT NULL DEFAULT 0,
  tools INTEGER NOT NULL DEFAULT 0, tool_errors INTEGER NOT NULL DEFAULT 0, tool_kinds TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (workspace_id, day, source, purpose, harness, model, bot_id, project_id, node_id)
);

-- Preserve single-node attribution when all matching usage_records agree on one non-empty node_id;
-- multi-node or pre-detail legacy aggregates remain intact under node_id = '' (splittable via rebuild).
INSERT INTO stat_daily_141 (
  workspace_id, day, source, purpose, harness, model, bot_id, project_id, node_id,
  input, output, cache_read, cache_write, reasoning, hit_base,
  cost_reported, calls, failed_calls,
  turns, turn_ms, turn_errors, retries, interrupted,
  tools, tool_errors, tool_kinds
)
SELECT
  s.workspace_id, s.day, s.source, s.purpose, s.harness, s.model, s.bot_id, s.project_id,
  COALESCE(
    (SELECT CASE WHEN COUNT(DISTINCT u.node_id) = 1 THEN MAX(u.node_id) ELSE '' END
     FROM usage_records u
     WHERE u.workspace_id = s.workspace_id
       AND u.day = s.day
       AND u.source = s.source
       AND u.purpose = s.purpose
       AND u.harness = s.harness
       AND u.model = s.model
       AND COALESCE(u.bot_id, '') = s.bot_id
       AND COALESCE(u.project_id, '') = s.project_id
       AND u.node_id <> ''),
    ''
  ),
  s.input, s.output, s.cache_read, s.cache_write, s.reasoning, s.hit_base,
  s.cost_reported, s.calls, s.failed_calls,
  s.turns, s.turn_ms, s.turn_errors, s.retries, s.interrupted,
  s.tools, s.tool_errors, s.tool_kinds
FROM stat_daily s;

DROP TABLE stat_daily;
ALTER TABLE stat_daily_141 RENAME TO stat_daily;
