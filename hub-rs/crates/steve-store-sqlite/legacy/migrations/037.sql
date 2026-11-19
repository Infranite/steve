-- 37: D5. A Node permission request as the Hub holds it (its card, attempt, expiry and answer), and
-- a Node session's desired harness config, applied at the next idle boundary.
-- States: waiting (open, no answer command), answering (an answer is queued or sent), applied (the
-- Node took it), expired (the Hub's clock, the Node's expiry or the attempt's end), refused (the
-- Node rejected the answer).
CREATE TABLE runtime_permissions (
 decision_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 session_id TEXT NOT NULL REFERENCES runtime_sessions(session_id),
 attempt_id TEXT NOT NULL REFERENCES runtime_attempts(attempt_id),
 permission_id TEXT NOT NULL,
 request TEXT NOT NULL,
 expires_at INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('waiting','answering','applied','expired','refused')),
 answer_operation TEXT REFERENCES node_commands(operation_id),
 revision INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL,
 UNIQUE(node_id, attempt_id, permission_id));
CREATE INDEX runtime_permissions_due ON runtime_permissions(expires_at) WHERE state='waiting';
CREATE INDEX runtime_permissions_attempt ON runtime_permissions(attempt_id, state);
CREATE INDEX runtime_permissions_answer ON runtime_permissions(answer_operation) WHERE answer_operation IS NOT NULL;
ALTER TABLE runtime_sessions ADD COLUMN desired_config TEXT;
-- A command whose last send may have reached the Node (leased, or answered OutcomeUnknown): 0 only
-- while every send so far was refused before acceptance, so a cancel may settle it on the Hub.
ALTER TABLE node_commands ADD COLUMN maybe_sent INTEGER NOT NULL DEFAULT 0 CHECK(maybe_sent IN (0,1));
-- The revision of the Node's last view of the session, as applied from its stream (the inbox it
-- came from is pruned).
ALTER TABLE runtime_sessions ADD COLUMN node_revision TEXT;
