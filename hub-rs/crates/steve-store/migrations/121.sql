-- Old approvals are not authority to send anything after the cutover. Leave sent and in-flight
-- (outcome to reconcile) records alone. Retain decisions, audit and external references.
UPDATE chan_messages SET status = 'discarded'
 WHERE status = 'pending' AND sent IS NULL
 AND decision_id IN (SELECT id FROM decisions WHERE json_extract(result, '$.decided_by') = 'yolo');
UPDATE mail_drafts SET status = 'discarded', send_at = NULL
 WHERE status = 'approved'
 AND decision_id IN (SELECT id FROM decisions WHERE json_extract(result, '$.decided_by') = 'yolo');
UPDATE decisions SET status = 'expired'
 WHERE status = 'open' AND subject_type = 'release'
 AND json_extract(result, '$.decided_by') = 'yolo';

-- Cancel only old auto answers proven never to have reached a Node. An accepted or uncertain
-- operation is left for normal reconciliation; the migration does not guess its outcome.
CREATE TEMP TABLE retired_approval_commands AS
 SELECT p.decision_id, p.answer_operation
 FROM runtime_permissions p
 JOIN node_commands c ON c.operation_id = p.answer_operation
 JOIN decisions d ON d.id = p.decision_id
 WHERE p.state = 'answering' AND c.method = 'resolve_permission'
 AND c.state = 'pending' AND c.maybe_sent = 0
 AND (json_extract(d.result, '$.decided_by') = 'yolo' OR EXISTS (
     SELECT 1 FROM yolo_actions a WHERE a.decision_id = d.id
     AND a.outcome IN ('running','decided')
 ));
UPDATE node_commands SET state = 'rejected', revision = revision + 1
 WHERE operation_id IN (SELECT answer_operation FROM retired_approval_commands);
UPDATE runtime_permissions SET state = 'expired', revision = revision + 1
 WHERE decision_id IN (SELECT decision_id FROM retired_approval_commands);
UPDATE decisions SET status = 'expired' WHERE status = 'open'
 AND id IN (SELECT decision_id FROM retired_approval_commands);
DROP TABLE retired_approval_commands;

DROP TABLE yolo_actions;
DROP TABLE yolo_windows;
DROP TABLE yolo_scopes;
DELETE FROM settings WHERE key LIKE 'yolo.%' OR key IN ('yolo', 'models.yolo');
DELETE FROM revisions WHERE object_type = 'yolo_scope';
-- Fresh installs add the conditional item lifecycle columns after replaying the numbered
-- SQL. The pre-existing item triggers cannot yet be reparsed by ALTER TABLE DROP COLUMN.
-- Rebuild this unreferenced table, keeping every other statistic and its indexes.
CREATE TABLE item_stats_new (
  item_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
  created INTEGER NOT NULL, first_turn INTEGER, first_deliver INTEGER, last_deliver INTEGER,
  accepted INTEGER, done_at INTEGER, dropped_at INTEGER,
  lead_ms INTEGER, cycle_ms INTEGER, wait_ms INTEGER, agent_ms INTEGER,
  turns INTEGER NOT NULL DEFAULT 0, delivers INTEGER NOT NULL DEFAULT 0, reworks INTEGER NOT NULL DEFAULT 0,
  first_pass INTEGER,
  fails INTEGER NOT NULL DEFAULT 0, stalls INTEGER NOT NULL DEFAULT 0,
  releases INTEGER NOT NULL DEFAULT 0, release_fails INTEGER NOT NULL DEFAULT 0,
  decisions INTEGER NOT NULL DEFAULT 0,
  input INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
  cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0,
  cost_reported REAL NOT NULL DEFAULT 0, models TEXT NOT NULL DEFAULT '{}',
  dirty INTEGER NOT NULL DEFAULT 1, updated INTEGER NOT NULL);
INSERT INTO item_stats_new (
  item_id, workspace_id, created, first_turn, first_deliver, last_deliver,
  accepted, done_at, dropped_at, lead_ms, cycle_ms, wait_ms, agent_ms,
  turns, delivers, reworks, first_pass, fails, stalls, releases, release_fails, decisions,
  input, output, cache_read, cache_write, cost_reported, models, dirty, updated)
 SELECT item_id, workspace_id, created, first_turn, first_deliver, last_deliver,
  accepted, done_at, dropped_at, lead_ms, cycle_ms, wait_ms, agent_ms,
  turns, delivers, reworks, first_pass, fails, stalls, releases, release_fails, decisions,
  input, output, cache_read, cache_write, cost_reported, models, dirty, updated
 FROM item_stats;
DROP TABLE item_stats;
PRAGMA legacy_alter_table=ON;
ALTER TABLE item_stats_new RENAME TO item_stats;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX item_stats_done ON item_stats(workspace_id, done_at);
CREATE INDEX item_stats_dirty ON item_stats(dirty) WHERE dirty = 1;

-- Retire Bot-level widening, keeping read-only Plan and the separately validated
-- native-only admission setting. The latter is ignored for ordinary harnesses.
UPDATE bots SET perm_mode = 'default' WHERE perm_mode IN ('auto', 'acceptEdits', 'dontAsk');

-- No old on/full setting can grant this new authority.
CREATE TABLE approval_policies (
  workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id),
  mode TEXT NOT NULL CHECK(mode IN ('ask_owner','harness_auto')),
  revision INTEGER NOT NULL CHECK(revision > 0),
  actor TEXT NOT NULL
);

-- Facts are scoped, replay-safe and contain no tool arguments.
CREATE TABLE approval_turns (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT NOT NULL,
  created INTEGER NOT NULL,
  PRIMARY KEY (session_id, turn_id)
);
-- A pre-cutover request has no observed resolver; keep it instead of retroactively
-- attributing it to the new policy.
ALTER TABLE runtime_permissions ADD COLUMN resolver TEXT NOT NULL DEFAULT 'legacy';
