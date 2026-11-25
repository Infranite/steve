-- 140: one-time retirement of non-terminal child items left behind by the retired dynamic
-- work-graph engine (`source[1] = "workflow"`). Replaces the per-startup runtime scan while
-- keeping closed history, artifacts, and script-workflow (`source[1] = "script"`) items intact.

CREATE TEMP TABLE legacy_wf_children AS
 SELECT id, workspace_id, cycle, revision, updated
 FROM items
 WHERE phase <> 'closed'
   AND json_valid(source)
   AND json_extract(source, '$[1]') = 'workflow';

INSERT INTO item_cycles (item_id, cycle, revision, outcome, acceptance, reason, closed, source)
 SELECT id, cycle, revision, 'cancelled', NULL, 'legacy_workflow_retired', updated, 'migrated'
 FROM legacy_wf_children
 WHERE true
 ON CONFLICT(item_id, cycle) DO NOTHING;

UPDATE items
 SET phase = 'closed',
     outcome = 'cancelled',
     acceptance = NULL,
     closed_reason = 'legacy_workflow_retired',
     closed_at = COALESCE(closed_at, updated),
     closing = NULL
 WHERE id IN (SELECT id FROM legacy_wf_children);

UPDATE decisions
 SET status = 'expired',
     resolved = COALESCE(resolved, (SELECT updated FROM legacy_wf_children l WHERE l.id = decisions.item_id)),
     result = '{"closed":"cancelled","reason":"legacy_workflow_retired"}'
 WHERE status = 'open'
   AND item_id IN (SELECT id FROM legacy_wf_children);

INSERT INTO session_dispatch (id, session_id, text, via, created, files, state, updated)
 SELECT q.id, q.session_id, q.text, q.via, q.created, q.files, 'cancelled', l.updated
 FROM queued q
 JOIN sessions s ON s.id = q.session_id
 JOIN legacy_wf_children l ON l.id = s.item_id
 WHERE q.via NOT IN ('steve', 'resumed:steve')
 ON CONFLICT(id) DO UPDATE SET state = 'cancelled', updated = excluded.updated
 WHERE session_dispatch.state = 'claimed';

DELETE FROM queued
 WHERE session_id IN (
   SELECT s.id FROM sessions s JOIN legacy_wf_children l ON l.id = s.item_id
 );

DELETE FROM session_resume
 WHERE session_id IN (
   SELECT s.id FROM sessions s JOIN legacy_wf_children l ON l.id = s.item_id
 );

UPDATE runtime_attempts
 SET cancel_requested = 1,
     revision = revision + 1,
     updated = COALESCE((
       SELECT l.updated FROM runtime_turns t
       JOIN sessions s ON s.id = t.session_id
       JOIN legacy_wf_children l ON l.id = s.item_id
       WHERE t.turn_id = runtime_attempts.turn_id
     ), updated)
 WHERE state <> 'finished'
   AND cancel_requested = 0
   AND turn_id IN (
     SELECT t.turn_id FROM runtime_turns t
     JOIN sessions s ON s.id = t.session_id
     JOIN legacy_wf_children l ON l.id = s.item_id
   );

UPDATE gh_effects
 SET state = CASE WHEN state = 'pending' THEN 'failed' ELSE state END,
     next_at = NULL,
     last_error = 'legacy_workflow_retired'
 WHERE state IN ('pending', 'unknown')
   AND kind IN ('open', 'merge')
   AND decision_id IN (
     SELECT id FROM decisions WHERE item_id IN (SELECT id FROM legacy_wf_children)
   );

UPDATE sessions
 SET status = 'ended'
 WHERE status <> 'ended'
   AND item_id IN (SELECT id FROM legacy_wf_children);

DELETE FROM item_dependency_waits
 WHERE state = 'waiting'
   AND EXISTS (
     SELECT 1 FROM legacy_wf_children l
     WHERE l.id = item_dependency_waits.item_id AND l.cycle = item_dependency_waits.cycle
   );

DELETE FROM item_placement_waits
 WHERE item_id IN (SELECT id FROM legacy_wf_children);

UPDATE item_operations
 SET required = 0,
     status = CASE WHEN status IN ('pending', 'running') THEN 'waived' ELSE status END
 WHERE EXISTS (
   SELECT 1 FROM legacy_wf_children l
   WHERE l.id = item_operations.item_id AND l.cycle = item_operations.cycle
 );

DROP TABLE legacy_wf_children;
