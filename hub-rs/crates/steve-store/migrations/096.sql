-- 96: whether Steve replies for you in a space on your own account (`space_prefs.reply_as_you`): `draft` (the
-- rules decide, replies wait for your approval; the default, as before), `record` (nothing is drafted, asked or
-- answered for you there, no model reads its messages) or `off` (Steve leaves the space alone on your account:
-- messages are stored only). steve-channels `spaces::ReplyAsYou` reads it.
ALTER TABLE space_prefs ADD COLUMN reply_as_you TEXT NOT NULL DEFAULT 'draft';
