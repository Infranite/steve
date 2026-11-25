-- Migrate retired YOLO approval records without rewriting historical outcomes, and backfill
-- explicit bot_node_bindings so runtime paths need no read-time legacy fallback.

-- Only discard outbound channel messages that are still pending and have not been sent
-- (`status = 'pending' AND sent IS NULL`); sent or in-flight messages are left untouched.
UPDATE chan_messages SET status = 'discarded'
 WHERE status = 'pending' AND sent IS NULL
   AND decision_id IN (SELECT id FROM decisions WHERE json_extract(result, '$.decided_by') = 'yolo');

-- Only discard queued mail drafts that have not been sent (`status = 'approved' AND sent IS NULL`);
-- already-sending (`status = 'sending'`) and already-sent (`status = 'sent'` / `sent IS NOT NULL`)
-- drafts are left untouched.
UPDATE mail_drafts SET status = 'discarded', send_at = NULL
 WHERE status = 'approved' AND sent IS NULL
   AND decision_id IN (SELECT id FROM decisions WHERE json_extract(result, '$.decided_by') = 'yolo');

-- Keep historical decision `status` unchanged (already-settled rows stay `resolved`). Strip
-- retired YOLO engine keys from `result` and attribute automatic settlement to Steve
-- (`$.decided_by = 'steve'` and `resolved_by = 'steve'`, matching `audit.actor: 'yolo' -> 'steve'`).
UPDATE decisions
   SET result = json_set(
         json_remove(
           result,
           '$.yolo_action',
           '$.yolo_reason',
           '$.yolo_rule',
           '$.hash',
           '$.send_at',
           '$.content_hash'
         ),
         '$.decided_by',
         'steve'
       ),
       resolved_by = CASE WHEN resolved_by = 'yolo' THEN 'steve' ELSE resolved_by END
 WHERE json_extract(result, '$.decided_by') = 'yolo';

UPDATE decisions SET resolved_by = 'steve' WHERE resolved_by = 'yolo';

DELETE FROM audit WHERE action LIKE 'yolo.%';
UPDATE audit SET actor = 'steve' WHERE actor = 'yolo';

-- Backfill explicit bot_node_bindings for every (workspace_id, bot_id) without one.
-- Preserves the post-116 default (`DEFAULT_NODES = ["local"]`) first, and appends any distinct
-- active, non-revoked remote Node that this Bot actually ran sessions on in that workspace so
-- existing remote-capable Bots are not locked to local-only.
INSERT OR IGNORE INTO bot_node_bindings (workspace_id, bot_id, nodes)
SELECT w.workspace_id,
       w.bot_id,
       (
         SELECT json_group_array(node_id)
           FROM (
             SELECT node_id
               FROM (
                 SELECT 'local' AS node_id, 0 AS ord
                 UNION ALL
                 SELECT DISTINCT r.node_id AS node_id, 1 AS ord
                   FROM sessions s
                   JOIN runtime_sessions r ON r.session_id = s.id
                   JOIN nodes n ON n.node_id = r.node_id
                  WHERE s.workspace_id = w.workspace_id
                    AND s.bot_id = w.bot_id
                    AND r.node_id <> 'local'
                    AND n.local = 0
                    AND n.state = 'active'
                    AND n.revoked_at IS NULL
               )
              ORDER BY ord, node_id
           )
       )
  FROM bot_workspaces w;
