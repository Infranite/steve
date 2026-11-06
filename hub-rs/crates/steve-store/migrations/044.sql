-- 44: the `message` row that began a message, by session and message id. Each text delta of a Node
-- message takes its role from it; without this index the lookup walked back through every delta of
-- the message (quadratic in its length). Only `message` rows are indexed, so it stays small.
CREATE INDEX IF NOT EXISTS events_message_start ON events (session_id, json_extract(data, '$.message'))
  WHERE kind = 'message' AND format = 'steve.v1';
