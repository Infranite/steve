-- 87: file deliveries without an item (a user's own uploads, Bot sessions outside an item), the user who
-- manages such a delivery, and the per-delivery switch for interactive HTML previews (#205). SQLite
-- cannot drop NOT NULL in place, so the table is rebuilt; delivery_shares keeps referencing it by name.
-- `deliveries` has no trigger or view referring to it; db.rs runs this with foreign_keys off.
CREATE TABLE deliveries_new (
 id TEXT PRIMARY KEY,
 url TEXT NOT NULL UNIQUE,
 workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
 item_id TEXT REFERENCES items(id) ON DELETE CASCADE,
 session_id TEXT,
 owner_id TEXT,
 title TEXT NOT NULL,
 name TEXT NOT NULL,
 mime TEXT NOT NULL,
 size INTEGER NOT NULL CHECK(size >= 0),
 created INTEGER NOT NULL,
 entrypoint TEXT,
 sha256 TEXT NOT NULL,
 manifest TEXT NOT NULL,
 preview TEXT NOT NULL,
 interactive INTEGER NOT NULL DEFAULT 0 CHECK(interactive IN (0, 1)));
INSERT INTO deliveries_new (id, url, workspace_id, item_id, session_id, owner_id, title, name, mime, size,
    created, entrypoint, sha256, manifest, preview, interactive)
  SELECT id, url, workspace_id, item_id, session_id, NULL, title, name, mime, size, created, entrypoint,
    sha256, manifest, preview, 0
  FROM deliveries;
DROP TABLE deliveries;
PRAGMA legacy_alter_table=ON;
ALTER TABLE deliveries_new RENAME TO deliveries;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX deliveries_item ON deliveries(item_id, created);
CREATE INDEX deliveries_owner ON deliveries(workspace_id, owner_id, created);
