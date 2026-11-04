-- 32: model prices synced from Models.dev (steve_stats::catalog). New tables only, IF NOT EXISTS.
-- One row per provider model and price change: the first sync that lists a model writes `since` 0
-- (it prices all of its history), a later change is written with the time it was seen and prices
-- only the days after that, so the estimates of days already counted do not move.
CREATE TABLE IF NOT EXISTS price_catalog (
  provider TEXT NOT NULL,                       -- Models.dev provider id (`anthropic`, `openai`, …)
  model TEXT NOT NULL,                          -- the model id under it, lower-cased
  since INTEGER NOT NULL,
  input REAL NOT NULL, output REAL NOT NULL,    -- USD per million tokens
  cache_read REAL, cache_write REAL,            -- NULL = not listed
  tiered INTEGER NOT NULL DEFAULT 0,            -- also lists long-context or tier prices (not applied)
  name TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (provider, model, since));
-- The state of each source's sync: the last success, the last try and its error (NULL when it worked).
CREATE TABLE IF NOT EXISTS price_sync (
  source TEXT PRIMARY KEY,
  synced INTEGER, tried INTEGER, error TEXT,
  etag TEXT,
  models INTEGER NOT NULL DEFAULT 0);
