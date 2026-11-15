-- 67: how many sessions each device runs at once (`max_sessions`), now enforced by the Hub: 32 by
-- default, its owner may set 1–256. The Hub's own Node reads the Hub's `devices` row, a paired Node
-- its `nodes` row. Before 67 the device's number was only shown (4); a row still at 4 moves to 32.
-- `devices` is rebuilt for its new default and range (it has no index, trigger or foreign key; db.rs
-- runs this with foreign_keys off).
CREATE TABLE devices_new (id TEXT PRIMARY KEY, name TEXT NOT NULL, roles TEXT NOT NULL,
  os TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT '正常',
  max_sessions INTEGER NOT NULL DEFAULT 32 CHECK(max_sessions BETWEEN 1 AND 256),
  seen INTEGER NOT NULL, created INTEGER NOT NULL);
INSERT INTO devices_new (id, name, roles, os, version, status, max_sessions, seen, created)
  SELECT id, name, roles, os, version, status,
    CASE WHEN max_sessions = 4 THEN 32 ELSE min(max(max_sessions, 1), 256) END, seen, created
  FROM devices;
DROP TABLE devices;
-- The fresh-install runner adds conditional item lifecycle columns after the numbered SQL.
-- Do not revalidate unrelated item triggers (which already refer to those columns) while renaming
-- this unreferenced table. No schema references need rewriting for devices.
PRAGMA legacy_alter_table=ON;
ALTER TABLE devices_new RENAME TO devices;
PRAGMA legacy_alter_table=OFF;
ALTER TABLE nodes ADD COLUMN max_sessions INTEGER NOT NULL DEFAULT 32 CHECK(max_sessions BETWEEN 1 AND 256);

-- Dispatch and live room counts must not scan historical commands.
CREATE INDEX node_commands_session_room ON node_commands(node_id, state)
  WHERE method='start_turn' AND state IN ('pending','accepted','active','unknown');
