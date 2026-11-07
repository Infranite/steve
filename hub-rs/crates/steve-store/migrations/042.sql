-- 42: what acting on a sent message again needs (recall, reactions). `sent_via` is the connection
-- a message went out through (recalled through the same one even after the space's voice changed;
-- NULL for rows sent before: their space's voice). `ext_created` is the provider's create time of
-- a sent message when it gave one. `chan_message_seen` keeps the remote's delivery id of a message
-- per connection (a send's own id when the remote has no message ids). An older build ignores all
-- three.
ALTER TABLE chan_messages ADD COLUMN sent_via TEXT;
ALTER TABLE chan_messages ADD COLUMN ext_created INTEGER;
CREATE TABLE IF NOT EXISTS chan_message_seen (
  connection_id TEXT NOT NULL,
  source_id TEXT NOT NULL,
  message_id TEXT NOT NULL REFERENCES chan_messages(id) ON DELETE CASCADE,
  PRIMARY KEY (connection_id, source_id));
CREATE INDEX IF NOT EXISTS chan_message_seen_message ON chan_message_seen (message_id);
