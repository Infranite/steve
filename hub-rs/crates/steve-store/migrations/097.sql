-- 97: what Steve decided about a message on your own account. `chan_messages.composed`: the composer's answer kept with
-- the draft it wrote (outcome, reason, needs_commitment …; steve-channels `deliver::Generation`), so YOLO approves on your
-- own account only a draft the composer wrote and never one that would promise something for you. And
-- `space_prefs.bot_notices`: what a bot's message in the space becomes on your own account: `auto` (in a private chat
-- with the bot, a notice that carries an action link becomes a proposed item; the default), `item` (every bot message
-- here does) or `record` (recorded only, as before). steve-channels `spaces::BotNotices` reads it.
ALTER TABLE chan_messages ADD COLUMN composed TEXT;
ALTER TABLE space_prefs ADD COLUMN bot_notices TEXT NOT NULL DEFAULT 'auto';
-- `channel_settings.card_ttl_h`: on your own account, the hours after which a card about one of its chats that nobody
-- answered closes; NULL (the default): never on time. A starred contact's card and one asking you to commit never do.
ALTER TABLE channel_settings ADD COLUMN card_ttl_h INTEGER;
-- The channel queue's sweeps (steve-channels `Outbox::send_due`): open message cards past their account's lifetime, and
-- cards you answered with how to reply that wait to be drafted.
CREATE INDEX IF NOT EXISTS decisions_chan_open ON decisions(created) WHERE subject_type = 'chan_message' AND status = 'open';
CREATE INDEX IF NOT EXISTS decisions_chan_answered ON decisions(resolved) WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'resolved';
