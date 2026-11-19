-- 93: how far each user has read a channel conversation in 对话 (the newest message time they were shown), so
-- the conversation list and the channel switcher count what came in since. NULL: never opened here. Personal,
-- like `closed`: it changes nothing in the channel.
ALTER TABLE conversation_preferences ADD COLUMN read_up_to INTEGER;
