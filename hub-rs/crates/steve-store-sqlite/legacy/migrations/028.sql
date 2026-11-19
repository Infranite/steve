-- 28: a GitHub delivery is `received` (gh_deliveries.status) until it was processed (Y1). The
-- webhook answers 202 only after the row and what processing it needs are committed; the worker,
-- and on start the recovery, process what is still `received`, so a restart between accepting and
-- processing loses nothing. A failed processing is tried again after a back-off; the input is
-- deleted once processed, and kept with a delivery that failed for good. Polled views use the same rows.
-- A table of its own rather than new columns: CREATE … IF NOT EXISTS only, so it can run twice.
CREATE TABLE IF NOT EXISTS gh_delivery_inputs (
  delivery_id TEXT PRIMARY KEY REFERENCES gh_deliveries(delivery_id) ON DELETE CASCADE,
  source TEXT NOT NULL,                         -- JSON: {"app":{ws}} | {"conn":{ws,connection_id}} | {"poll":{repo_id}}
  payload TEXT NOT NULL,                        -- JSON
  attempts INTEGER NOT NULL DEFAULT 0,          -- failed processings so far (kept `received`, tried again)
  next_at INTEGER NOT NULL DEFAULT 0);          -- not before (back-off)
