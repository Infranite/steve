-- Ordinary Node RPC errors now use the shared contract. Convert retained receipts in place.
-- The old after_reconcile hint never proves that the operation can be resent.
CREATE TEMP TABLE ordinary_rpc_errors (table_name TEXT, column_name TEXT, id INTEGER, path TEXT, value TEXT);
INSERT INTO ordinary_rpc_errors
 SELECT 'node_commands', 'receipt', rowid, '$.payload', json_extract(receipt,'$.payload')
 FROM node_commands WHERE json_type(receipt,'$.payload.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'node_commands', 'receipt', rowid, '$.payload.state.error', json_extract(receipt,'$.payload.state.error')
 FROM node_commands WHERE json_type(receipt,'$.payload.state.error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'node_commands', 'receipt', rowid, '$.payload.state.result.error', json_extract(receipt,'$.payload.state.result.error')
 FROM node_commands WHERE json_type(receipt,'$.payload.state.result.error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'node_commands', 'receipt', rowid, '$.last_error', json_extract(receipt,'$.last_error')
 FROM node_commands WHERE json_type(receipt,'$.last_error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'runtime_attempts', 'receipt', rowid, '$.state.error', json_extract(receipt,'$.state.error')
 FROM runtime_attempts WHERE json_type(receipt,'$.state.error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'runtime_attempts', 'receipt', rowid, '$.state.result.error', json_extract(receipt,'$.state.result.error')
 FROM runtime_attempts WHERE json_type(receipt,'$.state.result.error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'runtime_attempts', 'completion', rowid, '$.error', json_extract(completion,'$.error')
 FROM runtime_attempts WHERE json_type(completion,'$.error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'runtime_job_attempts', 'receipt', rowid, '$.state.error', json_extract(receipt,'$.state.error')
 FROM runtime_job_attempts WHERE json_type(receipt,'$.state.error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'runtime_job_attempts', 'receipt', rowid, '$.state.result.error', json_extract(receipt,'$.state.result.error')
 FROM runtime_job_attempts WHERE json_type(receipt,'$.state.result.error.retry')='text';
INSERT INTO ordinary_rpc_errors
 SELECT 'runtime_job_attempts', 'completion', rowid, '$.error', json_extract(completion,'$.error')
 FROM runtime_job_attempts WHERE json_type(completion,'$.error.retry')='text';

UPDATE ordinary_rpc_errors SET value=json_object(
 'code', CASE json_extract(value,'$.code')
   WHEN 'offline' THEN 'unavailable' WHEN 'denied' THEN 'forbidden'
   WHEN 'expired' THEN 'stale' WHEN 'downgrade' THEN 'invalid'
   WHEN 'busy' THEN 'busy' WHEN 'unsupported' THEN 'unsupported'
   WHEN 'stale' THEN 'stale' WHEN 'conflict' THEN 'conflict'
   WHEN 'exhausted' THEN 'exhausted' WHEN 'invalid' THEN 'invalid'
   WHEN 'outcome_unknown' THEN 'outcome_unknown' WHEN 'gap' THEN 'gap'
   ELSE 'internal' END,
 'retry', CASE WHEN json_extract(value,'$.retry')='same_operation'
   AND json_extract(value,'$.code') NOT IN ('outcome_unknown','gap')
   AND (path='$.payload' OR path='$.last_error')
   THEN json_object('kind','later','after_ms',NULL) ELSE json_object('kind','never') END,
 'reason', CASE json_extract(value,'$.code') WHEN 'downgrade' THEN 'node.downgrade' END,
 'params', json_patch(
   CASE WHEN json_extract(value,'$.operation') IS NULL THEN '{}' ELSE
     json_object('operation',json_extract(value,'$.operation')) END,
   CASE WHEN json_extract(value,'$.state') IS NULL THEN '{}' ELSE
     json_object('state',json_extract(value,'$.state')) END),
 'safe_detail', COALESCE(json_extract(value,'$.safe_detail'),''));
CREATE INDEX ordinary_rpc_error_source ON ordinary_rpc_errors(table_name,column_name,path,id);
UPDATE node_commands SET receipt=json_set(receipt,'$.payload',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='node_commands' AND e.column_name='receipt'
   AND e.path='$.payload' AND e.id=node_commands.rowid;
UPDATE node_commands SET receipt=json_set(receipt,'$.payload.state.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='node_commands' AND e.column_name='receipt'
   AND e.path='$.payload.state.error' AND e.id=node_commands.rowid;
UPDATE node_commands SET receipt=json_set(receipt,'$.payload.state.result.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='node_commands' AND e.column_name='receipt'
   AND e.path='$.payload.state.result.error' AND e.id=node_commands.rowid;
UPDATE node_commands SET receipt=json_set(receipt,'$.last_error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='node_commands' AND e.column_name='receipt'
   AND e.path='$.last_error' AND e.id=node_commands.rowid;
UPDATE runtime_attempts SET receipt=json_set(receipt,'$.state.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='runtime_attempts' AND e.column_name='receipt'
   AND e.path='$.state.error' AND e.id=runtime_attempts.rowid;
UPDATE runtime_attempts SET receipt=json_set(receipt,'$.state.result.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='runtime_attempts' AND e.column_name='receipt'
   AND e.path='$.state.result.error' AND e.id=runtime_attempts.rowid;
UPDATE runtime_attempts SET completion=json_set(completion,'$.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='runtime_attempts' AND e.column_name='completion'
   AND e.path='$.error' AND e.id=runtime_attempts.rowid;
UPDATE runtime_job_attempts SET receipt=json_set(receipt,'$.state.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='runtime_job_attempts' AND e.column_name='receipt'
   AND e.path='$.state.error' AND e.id=runtime_job_attempts.rowid;
UPDATE runtime_job_attempts SET receipt=json_set(receipt,'$.state.result.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='runtime_job_attempts' AND e.column_name='receipt'
   AND e.path='$.state.result.error' AND e.id=runtime_job_attempts.rowid;
UPDATE runtime_job_attempts SET completion=json_set(completion,'$.error',json(e.value))
 FROM ordinary_rpc_errors e
 WHERE e.table_name='runtime_job_attempts' AND e.column_name='completion'
   AND e.path='$.error' AND e.id=runtime_job_attempts.rowid;

DROP TABLE ordinary_rpc_errors;

UPDATE runtime_sessions AS s SET mode_refusal_code=p.refusal_code
 FROM runtime_mode_projection p WHERE p.session_id=s.session_id;

-- Older commands cannot be replayed into a replaced Node journal.
UPDATE node_commands SET state='unknown', maybe_sent=1,
 lease_owner=NULL, lease_until=NULL, revision=revision+1,
 receipt=json_object('success',json('false'),'payload',json_object(
   'operation',operation_id,'state',json_object('state','unknown','reason','node_protocol_changed')))
 WHERE state IN ('pending','accepted','active');
UPDATE runtime_attempts SET state='unknown', revision=revision+1 WHERE state <> 'finished';
UPDATE runtime_job_attempts SET state='unknown', revision=revision+1 WHERE state <> 'finished';
UPDATE runtime_sessions AS s SET sending_mode=p.sending_mode, mode_problem=p.problem
 FROM runtime_mode_projection p WHERE p.session_id=s.session_id;

ALTER TABLE node_commands ADD COLUMN intent_purpose TEXT;
ALTER TABLE node_commands ADD COLUMN intent_subject TEXT;
ALTER TABLE node_commands ADD COLUMN intent_revision INTEGER CHECK(intent_revision>=0);

-- Preserve command and receipt identity across the upgrade. A command whose old subject cannot
-- be determined stays non-replayable and supplies no positive evidence.
WITH subjects AS (
 SELECT c.rowid AS id,
   CASE c.method WHEN 'open_session' THEN 'open' WHEN 'release_session' THEN 'release'
     WHEN 'configure_session' THEN 'configure' WHEN 'start_turn' THEN 'start_turn'
     WHEN 'prepare_workspace' THEN 'prepare' WHEN 'tidy_workspace' THEN 'tidy' END AS purpose,
   CASE c.method
     WHEN 'start_turn' THEN json_extract(c.request,'$.body.turn')
     WHEN 'tidy_workspace' THEN length(c.node_id)||':'||c.node_id||':'||
                    length(c.workspace_id)||':'||c.workspace_id
     WHEN 'prepare_workspace' THEN (SELECT session_id FROM node_commands o
                     WHERE o.dependency_operation=c.operation_id AND o.method='open_session'
                     ORDER BY o.created,o.rowid LIMIT 1)
     ELSE c.session_id END AS subject
 FROM node_commands c
), numbered AS (
 SELECT id,purpose,subject,
   row_number() OVER(PARTITION BY purpose,subject ORDER BY id)-1 AS revision
 FROM subjects WHERE purpose IS NOT NULL AND subject IS NOT NULL
)
UPDATE node_commands AS c SET intent_purpose=n.purpose, intent_subject=n.subject,
 intent_revision=n.revision FROM numbered n WHERE c.rowid=n.id;
CREATE UNIQUE INDEX node_commands_intent ON node_commands(intent_purpose,intent_subject,intent_revision)
 WHERE intent_purpose IS NOT NULL;

CREATE TABLE runtime_turn_progress (
 turn_id TEXT PRIMARY KEY REFERENCES runtime_turns(turn_id) ON DELETE CASCADE,
 summary TEXT NOT NULL,
 decision_id TEXT REFERENCES decisions(id)
);

ALTER TABLE workflow_units ADD COLUMN verified_output TEXT;
ALTER TABLE workflow_units ADD COLUMN inputs TEXT;
CREATE TABLE workflow_review_findings (
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 fingerprint TEXT NOT NULL,
 scope TEXT NOT NULL,
 findings TEXT NOT NULL,
 applied_revision INTEGER,
 PRIMARY KEY(workflow_id,fingerprint)
);
