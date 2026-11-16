-- 96: whether Steve replies for you in a space on your own account (`space_prefs.reply_as_you`): `draft` (the
-- rules decide, replies wait for your approval; the default, as before), `record` (nothing is drafted, asked or
-- answered for you there and no rule or flow runs; a reply in an item's thread still reaches that item) or `off`
-- (Steve leaves the space alone on your account: messages are stored only). steve-channels `spaces::ReplyAsYou`
-- reads it. And `chan_messages.turn_of`: the later message of the same sender whose turn answers this one
-- (steve-channels `presence::take_turn`), so no reply of its own is opened, approved by YOLO or sent.
ALTER TABLE space_prefs ADD COLUMN reply_as_you TEXT NOT NULL DEFAULT 'draft';
ALTER TABLE chan_messages ADD COLUMN turn_of TEXT;
