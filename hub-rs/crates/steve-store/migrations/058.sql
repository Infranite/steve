-- The merge queue (E1): merging and releasing apart. Per target (normalized upstream + branch) a queue
-- merges continuously (steve-core `mq::queue`) and a release train releases what merged (`mq::train`).
-- No state machine snapshot: the state is these rows, changed in one transaction per event. The lanes of
-- 031 are converted at boot once the queue is on (each lane once: `legacy_lane`) and stay, read only,
-- for one version.
CREATE TABLE IF NOT EXISTS mq_targets (
  id TEXT PRIMARY KEY,                          -- mq_…
  upstream TEXT NOT NULL, branch TEXT NOT NULL,
  mode TEXT,                                    -- push | host_pr | host_queue | NULL (not known)
  mode_evidence TEXT,                           -- what the mode was read from (JSON)
  node_id TEXT,                                 -- the Node its staging is kept on (sticky)
  granted_by TEXT, granted_at INTEGER,          -- allowed to merge into it (once per target)
  policy TEXT NOT NULL DEFAULT '{}',            -- steve-core mq::policy::Policy, fields left out are defaults
  let_through INTEGER NOT NULL DEFAULT 0,       -- paused, but one batch may start
  next_batch INTEGER NOT NULL DEFAULT 0,        -- the last MQ-<n> given (continues a lane's numbers)
  next_train INTEGER NOT NULL DEFAULT 0,        -- the last RT-<n> given
  legacy_lane TEXT UNIQUE,                      -- the 031 lane it was converted from
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (upstream, branch));

CREATE TABLE IF NOT EXISTS mq_entries (
  id TEXT PRIMARY KEY,                          -- me_…
  target_id TEXT NOT NULL REFERENCES mq_targets(id),
  workspace_id TEXT, item_id TEXT, cycle INTEGER,
  key TEXT NOT NULL,                            -- what dependencies name (the item; the entry for an outside one)
  after TEXT NOT NULL DEFAULT '[]',             -- keys it comes after (JSON)
  head TEXT NOT NULL, branch TEXT,              -- the accepted commit and the branch it is on
  consent TEXT,                                 -- the card that agreed to merge it (decision id)
  node_id TEXT,                                 -- the Node its commit is on
  state TEXT NOT NULL,                          -- queued | batched | landed | released | returned | withdrawn
  batch_no INTEGER, train_no INTEGER,
  landed_commit TEXT, landed_at INTEGER,
  release_now INTEGER NOT NULL DEFAULT 0,       -- "merge and release now": its train closes once it lands
  reason TEXT,
  legacy_id TEXT UNIQUE,                        -- the 031 request it was converted from
  arrived INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS mq_entries_live ON mq_entries(item_id, cycle)
  WHERE state IN ('queued', 'batched', 'landed');
CREATE INDEX IF NOT EXISTS mq_entries_target ON mq_entries(target_id, state);
CREATE INDEX IF NOT EXISTS mq_entries_item ON mq_entries(item_id);

CREATE TABLE IF NOT EXISTS mq_batches (
  target_id TEXT NOT NULL REFERENCES mq_targets(id), no INTEGER NOT NULL,   -- shown as MQ-<no>
  parent INTEGER, root INTEGER NOT NULL,        -- a half of `parent`; the batch the bisection started from
  state TEXT NOT NULL,                          -- running | waiting | unknown | landed | split | returned
  step TEXT,                                    -- while running: integrating | checking | pushing
  base TEXT, tip TEXT,
  members TEXT NOT NULL,                        -- entry ids, in order (JSON)
  checks TEXT, pr TEXT, outcome TEXT,
  reruns INTEGER NOT NULL DEFAULT 0,
  opened INTEGER NOT NULL, ended INTEGER,
  PRIMARY KEY (target_id, no));

CREATE TABLE IF NOT EXISTS mq_trains (
  target_id TEXT NOT NULL REFERENCES mq_targets(id), no INTEGER NOT NULL,   -- shown as RT-<no>
  state TEXT NOT NULL,                          -- open | awaiting | cutting | released | failed | unknown | halted | dropped
  now INTEGER NOT NULL DEFAULT 0,               -- you asked to release it as soon as it may
  tip TEXT, version TEXT, tag TEXT, decision_id TEXT,
  opened INTEGER NOT NULL, last INTEGER NOT NULL, closes_at INTEGER, ended INTEGER,
  PRIMARY KEY (target_id, no));

-- Each outward or lengthy step is a Node operation (`node_commands.operation_id`): after a restart its
-- state there says whether to take its result, reconcile, or wait.
CREATE TABLE IF NOT EXISTS mq_steps (
  operation_id TEXT PRIMARY KEY,
  target_id TEXT NOT NULL, batch_no INTEGER, train_no INTEGER,
  kind TEXT NOT NULL,                           -- prepare | merge | check | push | pr | cut | reconcile
  state TEXT NOT NULL, result TEXT,
  created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS mq_steps_batch ON mq_steps(target_id, batch_no);

-- What the queue did, for tuning its policy: one row per fact (a batch started or ended, a check, a
-- card and who answered it, a train, what a batch used).
CREATE TABLE IF NOT EXISTS mq_stats (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  target_id TEXT NOT NULL, at INTEGER NOT NULL,
  kind TEXT NOT NULL,
  batch_no INTEGER, train_no INTEGER,
  data TEXT NOT NULL DEFAULT '{}');
CREATE INDEX IF NOT EXISTS mq_stats_target ON mq_stats(target_id, at);
