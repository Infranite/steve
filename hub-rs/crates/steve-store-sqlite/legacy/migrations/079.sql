-- One-shot calls own their bill independently of the caller's future and Node job retention.
CREATE TABLE gateway_calls (
 call_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, item_id TEXT, bot_id TEXT, project_id TEXT,
 purpose TEXT NOT NULL, started INTEGER NOT NULL, day TEXT NOT NULL,
 ended INTEGER, ok INTEGER NOT NULL DEFAULT 0, model TEXT NOT NULL DEFAULT '',
 harness TEXT NOT NULL DEFAULT '', attempts INTEGER NOT NULL DEFAULT 0,
 duration_ms INTEGER NOT NULL DEFAULT 0, failures TEXT NOT NULL DEFAULT '[]',
 fallback_usage TEXT NOT NULL DEFAULT '[]');
CREATE TABLE gateway_attempts (
 attempt_id TEXT PRIMARY KEY, call_id TEXT NOT NULL REFERENCES gateway_calls(call_id),
 ordinal INTEGER NOT NULL, previous_attempt TEXT REFERENCES gateway_attempts(attempt_id),
 harness TEXT NOT NULL, model TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER,
 ok INTEGER, error TEXT, UNIQUE(call_id, ordinal));
CREATE TABLE gateway_usage_reports (
 attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id), report_id TEXT NOT NULL,
 source_at INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(attempt_id, report_id));
CREATE TABLE gateway_usage_meters (
 attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id),
 meter_epoch TEXT NOT NULL, model TEXT NOT NULL, scope TEXT NOT NULL, payload TEXT NOT NULL,
 PRIMARY KEY(attempt_id, meter_epoch, model, scope));
-- Does not refer to runtime_job_attempts: jobs/inbox may be pruned while report identity survives.
CREATE TABLE gateway_job_accounts (
 job_attempt_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL REFERENCES gateway_attempts(attempt_id),
 finish_on_end INTEGER NOT NULL DEFAULT 0 CHECK(finish_on_end IN (0,1)));
ALTER TABLE usage_records ADD COLUMN live_call_id TEXT REFERENCES gateway_calls(call_id);
CREATE INDEX usage_live_call ON usage_records(live_call_id) WHERE live_call_id IS NOT NULL;
