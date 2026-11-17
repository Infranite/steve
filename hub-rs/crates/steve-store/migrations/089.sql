-- 89: writes a channel refused for good, remembered (ext_ws protocol §3.6): a read-only connection
-- (`space_id` = '': every chat of it) or one chat that refuses writes. Until `until` the Hub drafts no
-- automatic replies there and a refused message opens no card of its own; `noticed_at` is when the one
-- card explaining it was opened (at most one per connection or chat a day, even across a lift).
-- `until` = 0: lifted (the provider allows writing again, or a write went through).
CREATE TABLE IF NOT EXISTS chan_write_blocks (
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  space_id TEXT NOT NULL DEFAULT '',
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  reason TEXT NOT NULL,
  blocked_at INTEGER NOT NULL,
  until INTEGER NOT NULL,
  noticed_at INTEGER,
  PRIMARY KEY (connection_id, space_id));
