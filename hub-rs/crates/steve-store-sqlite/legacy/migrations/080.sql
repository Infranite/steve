-- Core's in-process ACP capture must survive projection failure just like the Node inbox does.
CREATE TABLE gateway_local_inbox (
 attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id), report_id TEXT NOT NULL,
 observed_at INTEGER NOT NULL, payload TEXT NOT NULL, applied INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(attempt_id,report_id));
CREATE INDEX gateway_local_pending ON gateway_local_inbox(attempt_id) WHERE applied=0;
CREATE TABLE gateway_local_ready (
 attempt_id TEXT PRIMARY KEY REFERENCES gateway_attempts(attempt_id), tried INTEGER NOT NULL DEFAULT 0);
CREATE INDEX gateway_local_round ON gateway_local_ready(tried);
-- Caller outcomes are durable facts too, even when rebuilding their usage projection fails.
ALTER TABLE gateway_calls ADD COLUMN projection_dirty INTEGER NOT NULL DEFAULT 1;
CREATE INDEX gateway_projection_pending ON gateway_calls(projection_dirty) WHERE projection_dirty=1;

-- Billing ownership outlives runtime Job pruning; retain the authenticated source identity.
ALTER TABLE gateway_job_accounts ADD COLUMN node_id TEXT;
ALTER TABLE gateway_job_accounts ADD COLUMN job_id TEXT;
UPDATE gateway_job_accounts AS g SET
 node_id=(SELECT node_id FROM runtime_job_attempts r WHERE r.attempt_id=g.job_attempt_id),
 job_id=(SELECT job_id FROM runtime_job_attempts r WHERE r.attempt_id=g.job_attempt_id);
