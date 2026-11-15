-- 70: opaque Hub-issued connector references. Explicit search/history/read operations may register
-- these without importing messages or scheduling history backfill. A reference grants no access:
-- each use rechecks the caller, current connection/account, operation and conversation disclosure.
CREATE TABLE connector_refs (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('conversation','message','topic','reference')),
  external_id TEXT NOT NULL,
  conversation_id TEXT NOT NULL DEFAULT '',
  source_message_id TEXT NOT NULL DEFAULT '',
  original_type TEXT NOT NULL,
  metadata TEXT NOT NULL DEFAULT '{}',
  expires INTEGER NOT NULL,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE(connection_id, account_id, kind, conversation_id, external_id)
);
CREATE INDEX connector_refs_connection ON connector_refs(workspace_id, connection_id, account_id);
CREATE INDEX connector_refs_expiry ON connector_refs(expires);
