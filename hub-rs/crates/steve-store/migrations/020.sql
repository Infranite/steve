-- 20: IM reactions (channels-personas.md §2.11): the “收到 → 完成/失败” reaction Steve keeps on an inbound channel
-- message it answers. One row per inbound `chan_messages` row Steve decided to respond to; a side table rather
-- than new columns on `chan_messages`, so the migration stays CREATE … IF NOT EXISTS only and can run twice.
CREATE TABLE IF NOT EXISTS chan_reactions (
  message_id TEXT PRIMARY KEY REFERENCES chan_messages(id), workspace_id TEXT NOT NULL,
  state TEXT NOT NULL,                 -- ack|done|failed|cleared
  reaction_id TEXT,                    -- the provider's id of the reaction shown now (NULL when adding it failed)
  emoji TEXT,                          -- the provider's emoji type shown now (Feishu `OnIt`, `DONE`, `CrossMark`)
  as_user INTEGER NOT NULL DEFAULT 0,  -- added with your own user token (`/bot` on your account), not the bot's
  answer_id TEXT,                      -- the outbound `chan_messages` row whose sending completes it
  error TEXT, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS chan_reactions_answer ON chan_reactions(answer_id) WHERE answer_id IS NOT NULL;
