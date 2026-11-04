-- 031: merge lanes (stage 2 of the merge window, steve-merge-window-20261104/PLAN.md). One lane per target
-- (normalized upstream + branch), across workspaces. The window's rules are steve-core `lane::batch`; its
-- state is kept here as that state machine's own snapshot (`state`), changed in the same transaction as
-- the rows below. Requests and units are the facts the API shows and items are told about.
CREATE TABLE IF NOT EXISTS lanes (
  id TEXT PRIMARY KEY,                          -- ln_…
  upstream TEXT NOT NULL, branch TEXT NOT NULL,
  window_ms INTEGER, max_members INTEGER,       -- NULL: the default (lane::batch::Policy)
  state TEXT NOT NULL DEFAULT '{}',             -- lane::batch::Lane, serialized
  created INTEGER NOT NULL, updated INTEGER NOT NULL,
  UNIQUE (upstream, branch));

CREATE TABLE IF NOT EXISTS lane_requests (
  id TEXT PRIMARY KEY,                          -- lr_…
  lane_id TEXT NOT NULL REFERENCES lanes(id),
  workspace_id TEXT, item_id TEXT,              -- an item's request; NULL for an outside one
  source TEXT NOT NULL,                         -- item | external
  dir TEXT NOT NULL, branch TEXT NOT NULL, head TEXT NOT NULL,
  arrived INTEGER NOT NULL,
  unit INTEGER,                                 -- the unit it merges in (NULL while queued)
  released_in INTEGER,                          -- the unit its release goes out with
  state TEXT NOT NULL,                          -- queued | collecting | merging | merged | released | left
  reason TEXT, updated INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS lane_requests_lane ON lane_requests(lane_id, state);
CREATE INDEX IF NOT EXISTS lane_requests_item ON lane_requests(item_id);

CREATE TABLE IF NOT EXISTS lane_units (
  lane_id TEXT NOT NULL REFERENCES lanes(id), no INTEGER NOT NULL,   -- shown as MQ-<no>
  state TEXT NOT NULL,                          -- collecting | merging | releasing | done
  branch TEXT, version TEXT, outcome TEXT,      -- the batch branch; the version it released; ok | failed | …
  opened INTEGER NOT NULL, ended INTEGER,
  PRIMARY KEY (lane_id, no));
