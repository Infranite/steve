-- Durable local hand-off. Receipts describe local submission, not external exactly-once delivery. A message whose
-- dispatch was `submitted` when the Hub stopped is queued again with `via = 'resumed:<via>'`: it may already have
-- been acted on, so it goes back to the agent as context to check, never as a new request.
CREATE TABLE IF NOT EXISTS session_resume (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS session_dispatch (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 text TEXT NOT NULL, via TEXT NOT NULL, created INTEGER NOT NULL, files TEXT,
 state TEXT NOT NULL CHECK(state IN ('claimed','submitted','completed','interrupted','cancelled','error')),
 updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS session_dispatch_pending ON session_dispatch(session_id,state);
-- `attempts`: automatic re-answers started for the run; past one, it waits for an explicit rerun (a run that keeps
-- taking the Hub down is not retried forever). `created` is the first interruption (old ones are not revived).
CREATE TABLE IF NOT EXISTS thread_resume (
 run_id TEXT PRIMARY KEY REFERENCES thread_runs(id), created INTEGER NOT NULL,
 attempts INTEGER NOT NULL DEFAULT 0);
