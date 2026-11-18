-- Live personal-connection receipts used by trigger admission and send-time authorization.
-- Do not infer or backfill consent from old messages, seen_by masks or workspace ownership.
CREATE TABLE chan_trigger_receipts (
  message_id TEXT NOT NULL REFERENCES chan_messages(id) ON DELETE CASCADE,
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  sender_account_id TEXT REFERENCES accounts(id) ON DELETE SET NULL,
  sender_party_id TEXT REFERENCES parties(id) ON DELETE SET NULL,
  from_self INTEGER NOT NULL CHECK(from_self IN (0, 1)),
  PRIMARY KEY(message_id, connection_id)
);
CREATE INDEX chan_trigger_receipts_connection ON chan_trigger_receipts(connection_id);
