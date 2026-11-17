-- Shadow facts of an item's work (#327): what was delivered, what the work waits on, what a judge
-- concluded, and the operator's overrides of the named work policy. Additive and empty at migration:
-- nothing acts on them yet; the derived state is computed from them next to today's phase/status.
-- Each fact names the cycle (and, where it has one, the session and turn) it belongs to, so a late
-- fact of an earlier cycle or turn is told apart from the current one.

-- What a delivery consists of, one fact per row. `provenance` says how far it can be trusted:
-- `verified` (the Hub looked itself: git, the host, its own version), `reported` (the executor said
-- so), `unknown` (nobody could tell). Only verified facts may ever count as hard facts.
CREATE TABLE item_delivery_facts (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT,
  turn_id TEXT,
  kind TEXT NOT NULL CHECK(kind IN ('commit','tree','pr','ci','build','deploy','review','approval')),
  -- the commit the fact is about (a tree, a PR, a check, a review or an approval of that head)
  head TEXT,
  -- what it names: a tree hash, a PR number, a check, a build, a version, a reviewer or approver
  ref TEXT,
  state TEXT NOT NULL,
  provenance TEXT NOT NULL CHECK(provenance IN ('verified','reported','unknown')),
  source TEXT NOT NULL,
  observed_at INTEGER NOT NULL
);
CREATE INDEX item_delivery_facts_item ON item_delivery_facts(item_id,cycle,observed_at);

-- What the work waits on and when it is looked at again. `wakes` counts the looks so far, `cap` is
-- how many it may have before it is somebody's to decide (NULL: no cap).
CREATE TABLE item_waits (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT,
  waits_on TEXT NOT NULL CHECK(waits_on IN
    ('you','external','item','transient','quota','pr','ci','build','deploy','time')),
  what TEXT NOT NULL,
  ref TEXT,
  next_check INTEGER NOT NULL,
  fallback_at INTEGER,
  wakes INTEGER NOT NULL DEFAULT 0 CHECK(wakes>=0),
  cap INTEGER CHECK(cap IS NULL OR cap>=0),
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  resolved_at INTEGER,
  resolution TEXT
);
CREATE INDEX item_waits_open ON item_waits(item_id,cycle) WHERE resolved_at IS NULL;

-- One row per completion judgement of one (item, cycle, session, logical turn, scope revision).
-- Empty until a judge exists.
CREATE TABLE item_judgements (
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  scope_revision INTEGER NOT NULL DEFAULT 0,
  verdict TEXT NOT NULL CHECK(verdict IN ('done','not_done','waiting','needs_you','unknown')),
  basis TEXT NOT NULL DEFAULT '{}',
  harness TEXT,
  model TEXT,
  created INTEGER NOT NULL,
  UNIQUE(item_id,cycle,session_id,turn_id,scope_revision)
);

-- The operator's overrides of the work policy. The names and defaults live in code
-- (`steve-core` work_policy); a name the code does not know is ignored and reported.
CREATE TABLE work_policy (
  name TEXT PRIMARY KEY,
  value INTEGER NOT NULL,
  updated INTEGER NOT NULL
);

-- The newest logical turn of a session (the turn a judgement or a late event is matched against).
CREATE INDEX runtime_turns_session ON runtime_turns(session_id,created);
