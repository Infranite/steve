-- Clean retired YOLO rows and backfill explicit bot_node_bindings so runtime paths need no
-- read-time legacy fallback or YOLO approval guards.
UPDATE chan_messages SET status = 'discarded'
 WHERE status = 'pending' AND sent IS NULL
   AND decision_id IN (SELECT id FROM decisions WHERE json_extract(result, '$.decided_by') = 'yolo');

UPDATE mail_drafts SET status = 'discarded', send_at = NULL
 WHERE status = 'approved'
   AND decision_id IN (SELECT id FROM decisions WHERE json_extract(result, '$.decided_by') = 'yolo');

UPDATE decisions
   SET status = 'expired',
       result = json_remove(result, '$.decided_by', '$.yolo_reason', '$.hash', '$.send_at', '$.content_hash')
 WHERE json_extract(result, '$.decided_by') = 'yolo';

DELETE FROM audit WHERE action LIKE 'yolo.%';
UPDATE audit SET actor = 'steve' WHERE actor = 'yolo';

INSERT OR IGNORE INTO bot_node_bindings (workspace_id, bot_id, nodes)
SELECT workspace_id, bot_id, '["local"]' FROM bot_workspaces;
