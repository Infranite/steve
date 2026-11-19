-- Immutable Hub-owned deliveries and independent user-authorized, expiring shares (ST-73).
CREATE TABLE deliveries (
 id TEXT PRIMARY KEY,
 url TEXT NOT NULL UNIQUE,
 workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 session_id TEXT NOT NULL,
 title TEXT NOT NULL,
 name TEXT NOT NULL,
 mime TEXT NOT NULL,
 size INTEGER NOT NULL CHECK(size >= 0),
 created INTEGER NOT NULL,
 entrypoint TEXT,
 sha256 TEXT NOT NULL,
 manifest TEXT NOT NULL,
 preview TEXT NOT NULL
);
CREATE INDEX deliveries_item ON deliveries(item_id, created);
CREATE TABLE delivery_shares (
 token TEXT PRIMARY KEY,
 delivery_id TEXT NOT NULL REFERENCES deliveries(id) ON DELETE CASCADE,
 created_by TEXT NOT NULL REFERENCES users(id),
 created INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 revoked_at INTEGER
);
CREATE INDEX delivery_shares_delivery ON delivery_shares(delivery_id, created);
