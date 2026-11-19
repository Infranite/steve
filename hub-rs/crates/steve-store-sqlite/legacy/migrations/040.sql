-- N0d-5: Hub-owned job intents (a think call or a research run done on a Node) and their attempts,
-- one model each; a fallback is the next attempt. Only written when the Node job path is on.
-- thread_run_id: the research thread run a job serves, a plain id (the thread ledger keeps its own rows).
CREATE TABLE runtime_jobs (
 job_id TEXT PRIMARY KEY,
 kind TEXT NOT NULL CHECK(kind IN ('think','research')),
 purpose TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 node_id TEXT NOT NULL,
 thread_run_id TEXT,
 state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','finished')),
 created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX runtime_jobs_finished ON runtime_jobs(updated) WHERE state='finished';
CREATE TABLE runtime_job_attempts (
 attempt_id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES runtime_jobs(job_id),
 ordinal INTEGER NOT NULL CHECK(ordinal>=0), model TEXT NOT NULL, node_id TEXT NOT NULL,
 operation_id TEXT NOT NULL UNIQUE, execution TEXT NOT NULL, spec TEXT NOT NULL, digest TEXT NOT NULL,
 state TEXT NOT NULL DEFAULT 'planned' CHECK(state IN ('planned','accepted','running','unknown','finished')),
 receipt TEXT, completion TEXT,
 cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
 revision INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL,
 UNIQUE(job_id,ordinal));
CREATE UNIQUE INDEX runtime_one_open_job_attempt ON runtime_job_attempts(job_id) WHERE state <> 'finished';
CREATE INDEX runtime_job_attempts_open ON runtime_job_attempts(node_id) WHERE state <> 'finished';
-- A command targets a session attempt or a job attempt, never both.
ALTER TABLE node_commands ADD COLUMN target_job_attempt TEXT REFERENCES runtime_job_attempts(attempt_id)
 CHECK(target_job_attempt IS NULL OR target_attempt IS NULL);
CREATE INDEX node_commands_job_attempt ON node_commands(target_job_attempt) WHERE target_job_attempt IS NOT NULL;
