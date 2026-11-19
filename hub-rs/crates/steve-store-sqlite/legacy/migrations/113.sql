-- Permanent feed failures retain their input; the audit diagnostic is written alongside it.
CREATE TABLE chan_ext_ws_dead_letters (
 connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
 chat_id TEXT NOT NULL,
 source_id TEXT NOT NULL,
 payload TEXT NOT NULL,
 error TEXT NOT NULL,
 created INTEGER NOT NULL,
 PRIMARY KEY (connection_id, chat_id, source_id)
);
