-- What the sweep would do (#334), in shadow: every 30 s the sweep looks at each open item and decides
-- the one action it would take (wake the agent, raise a card, remind you) and why. Nothing carries
-- it out yet: every action's execution switch is off, so this is a log of decisions, not of effects.
-- One row per (item, cycle, session, turn, scope revision, action, reason, target): a decision seen
-- again on the next pass, or after a restart, is the same row; one that stopped holding is marked
-- cleared, and counted again (`episodes`) if it comes back. The same reason on the same turn about
-- another operation, card or wait, or after the goal changed, is another row. Additive and empty at
-- migration.
CREATE TABLE item_would_do (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  -- '' when the item has no session or its session no logical turn on a Node
  session_id TEXT NOT NULL DEFAULT '',
  turn_id TEXT NOT NULL DEFAULT '',
  -- the item's scope revision (the version of its goal; 0: none) it was decided on
  scope_revision INTEGER NOT NULL DEFAULT 0,
  -- what the decision is about when its reason names one thing: an operation, a card or a wait id
  -- ('' when it is about the item as a whole)
  target TEXT NOT NULL DEFAULT '',
  action TEXT NOT NULL CHECK(action IN ('wake','card','remind')),
  reason TEXT NOT NULL,
  detail TEXT,
  -- the derived state's row and code it was decided on
  row INTEGER NOT NULL,
  code TEXT NOT NULL,
  -- when it came due (the policy's clock), and when the sweep first saw it due
  due_at INTEGER NOT NULL,
  first_seen INTEGER NOT NULL,
  cleared_at INTEGER,
  episodes INTEGER NOT NULL DEFAULT 1 CHECK(episodes>=1),
  -- 0: only observed (its switch was off); a later release sets it when it carries the action out
  executed INTEGER NOT NULL DEFAULT 0 CHECK(executed IN (0,1)),
  UNIQUE(item_id,cycle,session_id,turn_id,scope_revision,action,reason,target)
);
-- an item's log, newest first (also what clears an item's open decisions)
CREATE INDEX item_would_do_item ON item_would_do(item_id,first_seen);
