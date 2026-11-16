-- 86: one space per platform chat, whichever identities see it; what each message is visible by.
-- `space_identities.ordinal` numbers the identities of a space (0–61, stable for the space's life:
-- identities are only deleted with their space); `owner_user` is the user whose personal account
-- the identity is (NULL: a workspace identity, a bot). `chan_messages.seen_by` is the set of
-- identities that received or sent the message, one bit per ordinal: a viewer sees a message when
-- one of them is not someone else's personal identity. 0: seen by the space as a whole (no identity
-- known). Rows written before take every identity their space had, so nobody sees more or less than
-- before. `space_identities.platform` is the provider the identity's chats belong to (`feishu`),
-- kept as it sees the space: an identity of the same platform joins the space instead of making one.
-- (On the identity, not on `spaces`: 012 rebuilds `spaces`.) A channel service's comes from its
-- connection's `config.platform`; a built-in adapter's is learned when it next sees the space.
ALTER TABLE space_identities ADD COLUMN ordinal INTEGER;
ALTER TABLE space_identities ADD COLUMN owner_user TEXT;
ALTER TABLE space_identities ADD COLUMN platform TEXT;
ALTER TABLE chan_messages ADD COLUMN seen_by INTEGER NOT NULL DEFAULT 0;
UPDATE space_identities SET ordinal = (
  SELECT count(*) FROM space_identities o WHERE o.space_id = space_identities.space_id
     AND (o.joined < space_identities.joined
          OR (o.joined = space_identities.joined AND o.connection_id < space_identities.connection_id)));
UPDATE space_identities SET ordinal = NULL WHERE ordinal >= 62;
UPDATE space_identities SET owner_user = coalesce(
  (SELECT p.user_id FROM connections c JOIN accounts a ON a.id = c.account_id JOIN parties p ON p.id = a.party_id
    WHERE c.id = space_identities.connection_id),
  (SELECT p.user_id FROM spaces s JOIN accounts a ON a.id = s.account_id JOIN parties p ON p.id = a.party_id
    WHERE s.id = space_identities.space_id AND NOT EXISTS (SELECT 1 FROM connections c WHERE c.id = space_identities.connection_id)));
CREATE UNIQUE INDEX IF NOT EXISTS space_identities_ordinal ON space_identities(space_id, ordinal) WHERE ordinal IS NOT NULL;
UPDATE space_identities SET platform = (
  SELECT json_extract(c.config, '$.platform') FROM connections c
   WHERE c.id = space_identities.connection_id AND json_valid(c.config));
UPDATE chan_messages SET seen_by = coalesce(
  (SELECT sum(1 << i.ordinal) FROM space_identities i WHERE i.space_id = chan_messages.space_id AND i.ordinal IS NOT NULL), 0);
