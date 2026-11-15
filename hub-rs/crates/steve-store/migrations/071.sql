-- Output intents share the channel outbox's message, decision, retry and recall lifecycle.
-- A key belongs to a real requester in one workspace. Intent and route are immutable snapshots;
-- changing text/recipient under the same key is a conflict, never another send.
CREATE TABLE output_deliveries (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  requester TEXT NOT NULL REFERENCES users(id),
  idempotency_key TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  intent TEXT NOT NULL,
  connection_id TEXT NOT NULL,
  space_id TEXT NOT NULL REFERENCES spaces(id),
  source_message_id TEXT,
  message_id TEXT NOT NULL UNIQUE REFERENCES chan_messages(id),
  attempts INTEGER NOT NULL DEFAULT 0,
  last_attempt_at INTEGER,
  last_error TEXT,
  replied_at INTEGER,
  reply_message_id TEXT REFERENCES chan_messages(id),
  created INTEGER NOT NULL,
  UNIQUE(workspace_id, requester, idempotency_key)
);
CREATE INDEX output_deliveries_workspace ON output_deliveries(workspace_id, created);

-- Manual settings and observations are independent, so learning cannot erase an override.
CREATE TABLE output_channel_preferences (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  user_id TEXT NOT NULL REFERENCES users(id),
  party_id TEXT NOT NULL REFERENCES parties(id),
  manual_space_id TEXT REFERENCES spaces(id),
  learned_space_id TEXT REFERENCES spaces(id),
  updated INTEGER NOT NULL,
  PRIMARY KEY(workspace_id,user_id,party_id)
);
