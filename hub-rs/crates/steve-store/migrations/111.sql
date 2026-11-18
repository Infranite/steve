-- Incoming reactions are independent of Steve's outgoing acknowledgement lifecycle.
-- Keep removal tombstones so an old create delivery cannot make a removed reaction live again.
CREATE TABLE chan_message_reactions (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    connection_id TEXT NOT NULL,
    space_external_id TEXT NOT NULL DEFAULT '',
    message_external_id TEXT NOT NULL DEFAULT '',
    message_source_id TEXT NOT NULL DEFAULT '',
    message_id TEXT REFERENCES chan_messages(id) ON DELETE CASCADE,
    actor_account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    emoji TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('manual', 'automated', 'unknown')),
    removed INTEGER NOT NULL CHECK (removed IN (0, 1)),
    action_at INTEGER NOT NULL,
    time_source TEXT NOT NULL DEFAULT 'provider' CHECK (time_source IN ('provider', 'observed')),
    source_revision INTEGER CHECK (source_revision > 0),
    PRIMARY KEY (workspace_id, connection_id, space_external_id, message_external_id,
                 message_source_id, actor_account_id, emoji)
);
CREATE INDEX chan_message_reactions_message ON chan_message_reactions(message_id, actor_account_id, emoji, action_at);
CREATE INDEX chan_message_reactions_unresolved ON chan_message_reactions(connection_id, message_external_id, message_source_id)
    WHERE message_id IS NULL;

-- A reply's internal source is independent of an optional platform quote id. It must survive
-- approval/restart so source-id-only channels get the same presence and already-answered gates.
CREATE TABLE chan_reply_sources (
    out_message_id TEXT PRIMARY KEY REFERENCES chan_messages(id) ON DELETE CASCADE,
    source_message_id TEXT NOT NULL REFERENCES chan_messages(id) ON DELETE CASCADE
);
CREATE INDEX chan_reply_sources_source ON chan_reply_sources(source_message_id);
