-- 027, item columns (see 027.sql). Runs once, in the migration transaction, when `items.phase` is missing;
-- the backfill turns existing items into what they already were and emits nothing.
ALTER TABLE items ADD COLUMN phase TEXT NOT NULL DEFAULT 'active';
ALTER TABLE items ADD COLUMN cycle INTEGER NOT NULL DEFAULT 1;
ALTER TABLE items ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE items ADD COLUMN outcome TEXT;
ALTER TABLE items ADD COLUMN acceptance TEXT;
-- JSON while phase = 'closing': kind, reason, by, at, frozen revision and fingerprint, policy, accepted,
-- the acceptance decision and the required operations of this revision.
ALTER TABLE items ADD COLUMN closing TEXT;
ALTER TABLE items ADD COLUMN closed_at INTEGER;
ALTER TABLE items ADD COLUMN closed_reason TEXT;

-- existing items: what they already were, without inventing anything
UPDATE items SET phase = 'closed', outcome = 'completed', closed_at = updated WHERE status = 'done';
UPDATE items SET phase = 'closed', outcome = 'cancelled', closed_at = updated WHERE status = 'drop';
UPDATE items SET phase = 'ready'
 WHERE status NOT IN ('done', 'drop') AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.item_id = items.id);
INSERT OR IGNORE INTO item_cycles (item_id, cycle, revision, outcome, acceptance, reason, closed, source)
 SELECT id, 1, 0, outcome, NULL, now, closed_at, 'migrated' FROM items WHERE phase = 'closed';
