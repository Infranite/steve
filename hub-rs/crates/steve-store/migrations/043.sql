-- 43: messages of an adapter without platform message ids. `content_key` is a versioned hash of a
-- message's sender, text and attachment kinds: a copy without a provider id is matched to another
-- copy of the same message by it (same space, close in time), through the index below.
-- `fallback_from` is the connection that supplied a copy without a provider id, so its next
-- identical message is never taken for this one. An older build ignores both.
ALTER TABLE chan_messages ADD COLUMN content_key TEXT;
ALTER TABLE chan_messages ADD COLUMN fallback_from TEXT;
CREATE INDEX IF NOT EXISTS chan_messages_content_key ON chan_messages (space_id, content_key, created)
  WHERE content_key IS NOT NULL;
