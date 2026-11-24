-- Ordered fallback execution surfaces (harness, model, reasoning_effort) per Bot.
CREATE TABLE bot_fallback_surfaces (
    bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    harness TEXT NOT NULL,
    model TEXT,
    reasoning_effort TEXT,
    PRIMARY KEY (bot_id, ordinal)
);
