-- 44: the inbound watermark of channel messages. `connections.enabled_at` is when a connection was
-- last turned on (connected again, resumed, a capability added; NULL: since `created`). On
-- `channel_settings`, `late_ms` is how late a message may arrive and still be acted on (NULL: the
-- default, 30 minutes), and `history_before` / `history_late` / `history_at` count the messages only
-- recorded as history because they were written before the connection was on or arrived too late.
-- `chan_messages.history` says which (`before` | `late`; NULL: a live message). An older build
-- ignores all of them.
ALTER TABLE connections ADD COLUMN enabled_at INTEGER;
ALTER TABLE channel_settings ADD COLUMN late_ms INTEGER;
ALTER TABLE channel_settings ADD COLUMN history_before INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channel_settings ADD COLUMN history_late INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channel_settings ADD COLUMN history_at INTEGER;
ALTER TABLE chan_messages ADD COLUMN history TEXT;
