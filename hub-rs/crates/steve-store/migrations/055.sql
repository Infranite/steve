-- Access tokens a user signs clients in with (only their sha256 is kept), and the clients signed in:
-- each has its own secret, its kind and the route it last came by.
CREATE TABLE access_tokens (
 id TEXT PRIMARY KEY,
 user_id TEXT NOT NULL REFERENCES users(id),
 name TEXT NOT NULL,
 hash TEXT NOT NULL UNIQUE,
 created INTEGER NOT NULL,
 expires INTEGER,
 last_used INTEGER,
 revoked INTEGER
);
CREATE INDEX access_tokens_user ON access_tokens(user_id);
CREATE TABLE clients (
 id TEXT PRIMARY KEY,
 user_id TEXT NOT NULL REFERENCES users(id),
 token_id TEXT REFERENCES access_tokens(id),
 secret_hash TEXT NOT NULL,
 kind TEXT NOT NULL,
 name TEXT NOT NULL,
 platform TEXT NOT NULL DEFAULT '',
 version TEXT NOT NULL DEFAULT '',
 origin TEXT,
 created INTEGER NOT NULL,
 last_seen INTEGER,
 last_route TEXT,
 last_ip TEXT,
 revoked INTEGER
);
CREATE INDEX clients_user ON clients(user_id);
