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
