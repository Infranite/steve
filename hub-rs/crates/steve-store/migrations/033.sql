-- 33: what an output is for and what the user marked on it (ST-38). `artifact_roles`: whether the
-- agent delivered an output as a record of the work (a self-check screenshot, a log) or as the result;
-- outputs without a row are judged by their title, like those from before. `annotations`: a note on
-- one output as delivered (the artifact row is its version), at a place in it, a draft until the user
-- sends it to the item; the send that took it is its key, so the same send repeated reaches the item
-- once. CREATE … IF NOT EXISTS only, so it can run twice.
CREATE TABLE IF NOT EXISTS artifact_roles (
  artifact_id TEXT PRIMARY KEY REFERENCES artifacts(id),
  role TEXT NOT NULL);                          -- record | result
CREATE TABLE IF NOT EXISTS annotations (
  id TEXT PRIMARY KEY,                          -- ann_…
  item_id TEXT NOT NULL REFERENCES items(id),
  artifact_id TEXT NOT NULL REFERENCES artifacts(id),
  path TEXT NOT NULL DEFAULT '',                -- a file inside a folder output
  anchor TEXT NOT NULL,                         -- JSON: region {x,y,w,h} | text {quote,…} | page {page} | file
  note TEXT NOT NULL,
  status TEXT NOT NULL,                         -- draft | sending | sent
  send_key TEXT,
  timeline_id INTEGER,                          -- the item's line it was sent as
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  sent INTEGER);
CREATE INDEX IF NOT EXISTS annotations_item ON annotations(item_id, created);
