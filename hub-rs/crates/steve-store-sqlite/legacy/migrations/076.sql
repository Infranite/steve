-- Upgrade evidence is independent of collection coverage. An applied Node end proves only
-- that the unknown execution had stopped by its receipt time, never a text/token baseline.
ALTER TABLE realtime_output_attempts ADD COLUMN stop_evidence TEXT;
ALTER TABLE realtime_output_attempts ADD COLUMN not_started INTEGER NOT NULL DEFAULT 0
 CHECK(not_started IN (0,1));

INSERT OR IGNORE INTO realtime_output_attempts
 (attempt_id,session_id,workspace_id,project_id,bot_id,node_id,started,ended,baseline,stop_evidence)
 SELECT a.attempt_id,s.id,s.workspace_id,i.project_id,s.bot_id,a.node_id,a.created,
        MAX(a.created,MAX(e.received)),0,'node_event'
 FROM runtime_attempts a JOIN runtime_turns t USING(turn_id)
 JOIN sessions s ON s.id=t.session_id
 LEFT JOIN items i ON i.id=s.item_id AND i.workspace_id=s.workspace_id
 JOIN node_event_inbox e ON e.node_id=a.node_id
  AND json_extract(e.payload,'$.subject.attempt')=a.attempt_id
  AND json_extract(e.payload,'$.subject.session')=s.id
  AND json_extract(e.payload,'$.subject.turn')=t.turn_id
 WHERE s.readonly=0 AND e.applied=1 AND e.received>0
  AND json_extract(e.payload,'$.subject.of')='execution'
  AND json_extract(e.payload,'$.subject.type')='turn'
  AND json_extract(e.payload,'$.body.type')='attempt_ended'
 GROUP BY a.attempt_id;

-- Match the command, Node, attempt and typed receipt. A Hub status/updated value alone is not
-- proof. maybe_sent=0 plus a persisted refusal proves no dispatch; no unknown interval exists.
INSERT OR IGNORE INTO realtime_output_attempts
 (attempt_id,session_id,workspace_id,project_id,bot_id,node_id,started,ended,baseline,stop_evidence,not_started)
 SELECT a.attempt_id,s.id,s.workspace_id,i.project_id,s.bot_id,a.node_id,a.created,
        MAX(a.created,c.updated),0,
        CASE WHEN c.state='rejected' THEN 'unsent' ELSE 'node_receipt' END,
        c.state='rejected'
 FROM runtime_attempts a JOIN runtime_turns t USING(turn_id)
 JOIN sessions s ON s.id=t.session_id
 LEFT JOIN items i ON i.id=s.item_id AND i.workspace_id=s.workspace_id
 JOIN node_commands c ON c.operation_id=a.operation_id AND c.node_id=a.node_id
  AND c.target_attempt=a.attempt_id AND c.method='start_turn'
 WHERE s.readonly=0 AND c.updated>0 AND (
  (c.state='finished' AND json_extract(c.receipt,'$.payload.operation')=c.operation_id
   AND json_extract(c.receipt,'$.payload.state.state')='finished'
   AND json_extract(c.receipt,'$.payload.state.outcome') IN ('succeeded','failed','cancelled','interrupted'))
  OR (c.state='rejected' AND c.maybe_sent=0 AND
   (json_extract(c.receipt,'$.payload.reason')='cancelled before it was sent'
    OR (json_type(c.receipt,'$.payload.code')='text' AND json_type(c.receipt,'$.payload.retry')='text'))));
