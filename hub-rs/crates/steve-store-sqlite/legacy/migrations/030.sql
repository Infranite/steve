-- 30: what a channel send needs to be tried again and accounted for (F07, F08): the authorization
-- it goes out under (so a message without a decision, a rule's or a trigger's reply, is found again
-- after a restart), when it was first sent, and the exact text and mentions of that first try (a
-- repeat with the provider's dedupe key must be the same message). chan_messages.status gains
-- `unknown`: sent, no answer. CREATE … IF NOT EXISTS only, so it can run twice.
CREATE TABLE IF NOT EXISTS chan_send_state (
  message_id TEXT PRIMARY KEY REFERENCES chan_messages(id) ON DELETE CASCADE,
  auth TEXT NOT NULL,                           -- JSON of steve_channels::Auth
  first_at INTEGER,                             -- the first try's start (the dedupe window counts from here)
  attempts INTEGER NOT NULL DEFAULT 0,
  text TEXT,                                    -- what the first try sent
  at TEXT,                                      -- JSON: the mentions it carried
  last_error TEXT);
-- A message claimed when the Hub stopped (pending with a claim) may be out: unknown, you are asked.
UPDATE chan_messages SET status = 'unknown', sent = NULL
 WHERE direction = 'out' AND status = 'pending' AND sent IS NOT NULL;
