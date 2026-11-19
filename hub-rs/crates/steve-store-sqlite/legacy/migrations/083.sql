-- 83: a work unit keeps one child item across its retries (#182): a retry is a new cycle and session
-- of the same item, so a retired attempt is (item, attempt), no longer the item alone. Rows written
-- before keep their item and take the attempt their snapshot recorded (one row per item, so unique).
-- `workflow_attempts` has no index, trigger or view referring to it; db.rs runs this with
-- foreign_keys off.
CREATE TABLE workflow_attempts_new (
 item_id TEXT NOT NULL REFERENCES items(id),
 attempt INTEGER NOT NULL CHECK(attempt >= 0),
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 unit_id TEXT NOT NULL, snapshot TEXT NOT NULL, created INTEGER NOT NULL,
 PRIMARY KEY(item_id, attempt));
INSERT INTO workflow_attempts_new (item_id, attempt, workflow_id, unit_id, snapshot, created)
  SELECT item_id, max(coalesce(json_extract(snapshot, '$.attempt'), 0), 0), workflow_id, unit_id,
    snapshot, created
  FROM workflow_attempts;
DROP TABLE workflow_attempts;
PRAGMA legacy_alter_table=ON;
ALTER TABLE workflow_attempts_new RENAME TO workflow_attempts;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX workflow_attempts_graph ON workflow_attempts(workflow_id, created);
