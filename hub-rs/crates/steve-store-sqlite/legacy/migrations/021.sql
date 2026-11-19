-- 21: the durable outbox (steve-architecture-dialogue-20261103, first slice). A fact a domain commits
-- together with its state change (`work.item.done`), handed to every registered consumer at least once.
-- Consumers are idempotent by `(event_seq, consumer_id)`: a consumer's business writes and its receipt
-- commit in one transaction, the lease only coordinates who runs it. CREATE … IF NOT EXISTS only, so the
-- migration can run twice.
CREATE TABLE IF NOT EXISTS outbox_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,             -- evt_… (stable; consumers key their own idempotency on it)
  workspace_id TEXT NOT NULL,
  topic TEXT NOT NULL,                 -- work.item.done
  version INTEGER NOT NULL,            -- payload schema version of the topic
  payload TEXT NOT NULL,               -- JSON
  created INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS outbox_events_topic ON outbox_events(topic, seq);
-- A consumer receives the events after `since_seq` (0 for consumers shipped with their topic); history is replayed
-- only by an explicit backfill. `enabled = 0` pauses it: it keeps its place and what it owes is kept.
CREATE TABLE IF NOT EXISTS outbox_consumers (
  id TEXT PRIMARY KEY,                 -- automation.item_trigger, work.tidy
  topic TEXT NOT NULL,                 -- the one topic it subscribes to
  since_seq INTEGER NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  registered INTEGER NOT NULL);
-- Done: one row per (event, consumer), written in the consumer's own transaction.
CREATE TABLE IF NOT EXISTS outbox_receipts (
  event_seq INTEGER NOT NULL REFERENCES outbox_events(seq),
  consumer_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  done INTEGER NOT NULL,
  outcome TEXT,                        -- what the delivery did (没有监听, evt_…), also when it changed nothing
  PRIMARY KEY (event_seq, consumer_id));
-- In progress or failing: who holds it until when, attempts, the last error, when to try again. `dead = 1`
-- after too many attempts: never retried by itself and never deleted by retention, a person decides.
CREATE TABLE IF NOT EXISTS outbox_leases (
  event_seq INTEGER NOT NULL REFERENCES outbox_events(seq),
  consumer_id TEXT NOT NULL,
  owner TEXT,                          -- the dispatcher instance (process start id) holding the lease
  lease_until INTEGER NOT NULL DEFAULT 0,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  next_at INTEGER NOT NULL DEFAULT 0,
  dead INTEGER NOT NULL DEFAULT 0,
  updated INTEGER NOT NULL,
  PRIMARY KEY (event_seq, consumer_id));
CREATE INDEX IF NOT EXISTS outbox_leases_dead ON outbox_leases(dead) WHERE dead = 1;
