-- Locate commands by their typed fields and record permission modes on the session.
ALTER TABLE node_commands ADD COLUMN session_id TEXT;
ALTER TABLE node_commands ADD COLUMN workspace_resource TEXT;
ALTER TABLE node_commands ADD COLUMN workspace_id TEXT;
ALTER TABLE node_commands ADD COLUMN recovery_target TEXT;
ALTER TABLE node_commands ADD COLUMN abandon_reason TEXT;
ALTER TABLE node_commands ADD COLUMN exec_deadline INTEGER;
ALTER TABLE node_commands ADD COLUMN subject TEXT;
ALTER TABLE node_commands ADD COLUMN requested_mode TEXT
 CHECK(requested_mode IN ('plan','ask','accept_edits','auto','full_access'));

UPDATE node_commands SET
 session_id = CASE WHEN method='release_session' THEN json_extract(request,'$.body')
                   ELSE json_extract(request,'$.body.session') END,
 workspace_resource = CASE WHEN method='prepare_workspace' THEN json_extract(request,'$.body.resource') END,
 workspace_id = CASE WHEN method='tidy_workspace' THEN json_extract(request,'$.body.workspace.id') END,
 recovery_target = CASE WHEN method='recover' THEN json_extract(request,'$.body.target') END,
 abandon_reason = CASE WHEN method='abandon' THEN json_extract(request,'$.body.reason') END,
 exec_deadline = CASE WHEN method='exec' THEN json_extract(request,'$.body.limits.deadline') END,
 subject = json_extract(request,'$.subject'),
 requested_mode = CASE WHEN method IN ('open_session','configure_session')
                       THEN json_extract(request,'$.body.policy.permissions') END;

CREATE INDEX node_commands_session_method ON node_commands(session_id,method,created)
 WHERE session_id IS NOT NULL;
CREATE INDEX node_commands_mode ON node_commands(session_id,created) WHERE requested_mode IS NOT NULL;
CREATE INDEX node_commands_applied_mode ON node_commands(session_id,created)
 WHERE requested_mode IS NOT NULL AND state='finished' AND json_extract(receipt,'$.success')=1;
CREATE INDEX node_commands_prepare_resource ON node_commands(node_id,workspace_resource)
 WHERE method='prepare_workspace';
CREATE INDEX node_commands_tidy_workspace ON node_commands(node_id,workspace_id)
 WHERE method='tidy_workspace';
CREATE INDEX node_commands_recovery_target ON node_commands(recovery_target,node_id)
 WHERE method='recover';
CREATE INDEX node_commands_abandon_target ON node_commands(target_attempt,abandon_reason)
 WHERE method='abandon';

ALTER TABLE runtime_sessions ADD COLUMN applied_mode TEXT
 CHECK(applied_mode IN ('plan','ask','accept_edits','auto','full_access'));
ALTER TABLE runtime_sessions ADD COLUMN desired_mode TEXT
 CHECK(desired_mode IN ('plan','ask','accept_edits','auto','full_access'));
ALTER TABLE runtime_sessions ADD COLUMN sending_mode TEXT
 CHECK(sending_mode IN ('plan','ask','accept_edits','auto','full_access'));
ALTER TABLE runtime_sessions ADD COLUMN mode_problem TEXT CHECK(mode_problem IN ('unknown','refused'));
ALTER TABLE runtime_sessions ADD COLUMN mode_refusal_code TEXT;
ALTER TABLE runtime_sessions ADD COLUMN mode_refusal_detail TEXT;

-- The backfill and later command writes share this projection. A late receipt for an
-- older command can advance the applied mode while leaving a newer request in flight.
CREATE VIEW runtime_mode_projection AS
 SELECT s.session_id, applied.requested_mode AS applied_mode,
        CASE WHEN latest.state IN ('pending','accepted','active')
             THEN latest.requested_mode END AS sending_mode,
        CASE WHEN latest.state='unknown' THEN 'unknown'
             WHEN refused.rowid IS NOT NULL THEN 'refused' END AS problem,
        COALESCE(json_extract(refused.receipt,'$.payload.code'),
                 json_extract(refused.receipt,'$.payload.state.error.code')) AS refusal_code,
        COALESCE(json_extract(refused.receipt,'$.payload.safe_detail'),
                 json_extract(refused.receipt,'$.payload.state.error.safe_detail')) AS refusal_detail
 FROM runtime_sessions s
 LEFT JOIN node_commands latest ON latest.rowid=(
   SELECT c.rowid FROM node_commands c
    WHERE c.session_id=s.session_id AND c.requested_mode IS NOT NULL
    ORDER BY c.created DESC,c.rowid DESC LIMIT 1)
 LEFT JOIN node_commands applied ON applied.rowid=(
   SELECT c.rowid FROM node_commands c
    WHERE c.session_id=s.session_id AND c.requested_mode IS NOT NULL
      AND c.state='finished' AND json_extract(c.receipt,'$.success')=1
    ORDER BY c.created DESC,c.rowid DESC LIMIT 1)
 LEFT JOIN node_commands refused ON refused.rowid=latest.rowid
   AND latest.state IN ('finished','rejected') AND latest.rowid IS NOT applied.rowid;

UPDATE runtime_sessions AS s SET
 applied_mode=p.applied_mode, desired_mode=json_extract(s.desired_config,'$.policy.permissions'),
 sending_mode=p.sending_mode, mode_problem=p.problem,
 mode_refusal_code=p.refusal_code, mode_refusal_detail=p.refusal_detail
 FROM runtime_mode_projection p WHERE p.session_id=s.session_id;
