-- The item lifecycle (CLAUDE-LIFECYCLE-DISCUSSION.txt, rounds 3–4): one phase per item
-- (ready → active → closing → closed), the cycle and the result revision within it, the closing intent
-- while it waits to be committed, and the outcome / acceptance once closed. `status` stays as the legacy
-- projection the lifecycle writes; it is not a second source of truth.
-- The item columns, their one-time backfill and the `items_closing` index are in 027_items.sql, applied by
-- `run_pending` once (when `items.phase` is missing), like `reasoning_effort`: this file stays rerunnable.
-- Each cycle once it ended: never rewritten. `source` = 'live' (closed by the lifecycle) or 'migrated'
-- (closed before it existed: acceptance unknown, no event was or will be emitted for it).
CREATE TABLE IF NOT EXISTS item_cycles (
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 revision INTEGER NOT NULL,
 outcome TEXT NOT NULL,
 acceptance TEXT,
 reason TEXT,
 closed INTEGER NOT NULL,
 source TEXT NOT NULL DEFAULT 'live',
 PRIMARY KEY (item_id, cycle));

-- Operations with an effect outside Steve that a cycle's result depends on (a release, a module's wrap-up
-- such as a PR merge, the consultation of the acceptance hooks): durable, with a stable effect identity so
-- a repeated or late result is matched to the operation it belongs to and never redone or reused for
-- another cycle. `required` = part of the closing gate of (cycle, revision).
CREATE TABLE IF NOT EXISTS item_operations (
 id TEXT PRIMARY KEY,
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 cycle INTEGER NOT NULL,
 revision INTEGER NOT NULL,
 kind TEXT NOT NULL,
 effect_id TEXT,
 required INTEGER NOT NULL DEFAULT 0,
 status TEXT NOT NULL CHECK(status IN ('pending','running','succeeded','failed','unknown','waived')),
 label TEXT NOT NULL DEFAULT '',
 evidence TEXT NOT NULL DEFAULT '{}',
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS item_operations_item ON item_operations(item_id, cycle);
CREATE UNIQUE INDEX IF NOT EXISTS item_operations_effect ON item_operations(item_id, kind, effect_id) WHERE effect_id IS NOT NULL;

