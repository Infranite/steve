-- 60: a research run's session is its harness adapter's (harness parity S6): the session id under
-- a neutral name, and the adapter it belongs to, so a follow-up continues it only on the same one.
-- Runs from before ran on Claude Code. (`claude_session` stays, unused: renaming or dropping a
-- column makes SQLite check every trigger against tables an older schema may not have yet.)
ALTER TABLE thread_runs ADD COLUMN session TEXT;
ALTER TABLE thread_runs ADD COLUMN harness TEXT;
UPDATE thread_runs SET session = claude_session, harness = 'claude-code' WHERE claude_session IS NOT NULL;
