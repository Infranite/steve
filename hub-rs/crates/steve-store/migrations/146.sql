-- Incoming channel reactions (#277), durable reply-to-source links,
-- burst grouping for multi-topic replies (#268), and configurable [AI] mark preference.

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
    PRIMARY KEY (
        workspace_id,
        connection_id,
        space_external_id,
        message_external_id,
        message_source_id,
        actor_account_id,
        emoji
    )
);
CREATE INDEX chan_message_reactions_message
    ON chan_message_reactions(message_id, actor_account_id, emoji, action_at);
CREATE INDEX chan_message_reactions_unresolved
    ON chan_message_reactions(connection_id, message_external_id, message_source_id)
    WHERE message_id IS NULL;

CREATE TABLE chan_reply_sources (
    out_message_id TEXT PRIMARY KEY REFERENCES chan_messages(id) ON DELETE CASCADE,
    source_message_id TEXT NOT NULL REFERENCES chan_messages(id) ON DELETE CASCADE
);
CREATE INDEX chan_reply_sources_source ON chan_reply_sources(source_message_id);

-- Multi-topic drafts generated for one burst share a burst_id so sibling drafts
-- are not retired by each other.
ALTER TABLE chan_messages ADD COLUMN burst_id TEXT;

-- Owner preference for the [AI] prefix on replies sent from a personal account
-- (1 = on by default, 0 = off). Space preference overrides when non-null.
ALTER TABLE channel_settings ADD COLUMN ai_mark INTEGER NOT NULL DEFAULT 1 CHECK (ai_mark IN (0, 1));
ALTER TABLE space_prefs ADD COLUMN ai_mark INTEGER CHECK (ai_mark IN (0, 1));
