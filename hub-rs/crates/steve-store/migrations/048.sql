-- 44: the model a turn is booked under in `stat_daily`, written when the turn ends (the live writer's
-- choice: its first token record, or what the runner reported). The rebuild and the hourly view read it,
-- so they book a turn where the live writer did. NULL for turns ended before: those are attributed as
-- before (first token record, else the session's model, else the Bot's). An older build ignores it.
ALTER TABLE agent_turns ADD COLUMN model TEXT;
