-- 18: ownership (modules/ownership.md §2): who is responsible (负责人) for an account, connection, device, bot,
-- persona, bot party or space. New table only, all IF NOT EXISTS, nothing existing is altered. The INSERTs
-- below are the one-time backfill of §3 (“Seed / backfill”): they only add rows to the new table, link an
-- object only while it has no steward at all, and running them again adds nothing.
CREATE TABLE IF NOT EXISTS stewards (
  workspace_id TEXT NOT NULL,
  object_type TEXT NOT NULL,      -- account|connection|device|bot|persona|party|space
  object_id TEXT NOT NULL,
  person_id TEXT NOT NULL REFERENCES parties(id),
  role TEXT NOT NULL DEFAULT 'owner',   -- owner(负责人)|maintainer(维护人)|user(使用人)
  source TEXT NOT NULL,           -- manual|connect|sync|chat|inferred|seed
  confidence REAL NOT NULL DEFAULT 1, status TEXT NOT NULL DEFAULT 'confirmed',  -- confirmed|proposed
  source_ref TEXT, decision_id TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, object_type, object_id, person_id, role));
CREATE INDEX IF NOT EXISTS stewards_person ON stewards(workspace_id, person_id);

-- “我” of every workspace: the contact of its first owner.
-- Steve's bots (home workspace and every workspace they joined) are owned by “我”.
INSERT OR IGNORE INTO stewards (workspace_id, object_type, object_id, person_id, role, source, created, updated)
SELECT bw.workspace_id, 'bot', b.id, p.id, 'owner', 'seed',
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM bots b
JOIN (SELECT id AS bot_id, workspace_id FROM bots UNION SELECT bot_id, workspace_id FROM bot_workspaces) bw ON bw.bot_id = b.id
JOIN parties p ON p.workspace_id = bw.workspace_id AND p.type = 'contact' AND p.status = 'active'
  AND p.user_id = (SELECT m.user_id FROM workspace_members m WHERE m.workspace_id = bw.workspace_id AND m.role = 'owner' ORDER BY m.created LIMIT 1)
WHERE b.deleted = 0
  AND NOT EXISTS (SELECT 1 FROM stewards s WHERE s.workspace_id = bw.workspace_id AND s.object_type = 'bot' AND s.object_id = b.id);

-- Personas are owned by “我”.
INSERT OR IGNORE INTO stewards (workspace_id, object_type, object_id, person_id, role, source, created, updated)
SELECT x.workspace_id, 'persona', x.id, p.id, 'owner', 'seed',
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM personas x
JOIN parties p ON p.workspace_id = x.workspace_id AND p.type = 'contact' AND p.status = 'active'
  AND p.user_id = (SELECT m.user_id FROM workspace_members m WHERE m.workspace_id = x.workspace_id AND m.role = 'owner' ORDER BY m.created LIMIT 1)
WHERE x.deleted = 0
  AND NOT EXISTS (SELECT 1 FROM stewards s WHERE s.workspace_id = x.workspace_id AND s.object_type = 'persona' AND s.object_id = x.id);

-- The Hub device is owned by “我” of every workspace (devices belong to no workspace).
INSERT OR IGNORE INTO stewards (workspace_id, object_type, object_id, person_id, role, source, created, updated)
SELECT p.workspace_id, 'device', d.id, p.id, 'owner', 'seed',
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM devices d
JOIN parties p ON p.type = 'contact' AND p.status = 'active'
  AND p.user_id = (SELECT m.user_id FROM workspace_members m WHERE m.workspace_id = p.workspace_id AND m.role = 'owner' ORDER BY m.created LIMIT 1)
WHERE d.id = 'hub'
  AND NOT EXISTS (SELECT 1 FROM stewards s WHERE s.workspace_id = p.workspace_id AND s.object_type = 'device' AND s.object_id = d.id);

-- A connection is owned by the person whose account it is, otherwise by “我” of the user who connected it.
INSERT OR IGNORE INTO stewards (workspace_id, object_type, object_id, person_id, role, source, created, updated)
SELECT c.workspace_id, 'connection', c.id, COALESCE(
         (SELECT a.party_id FROM accounts a JOIN parties ap ON ap.id = a.party_id
           WHERE a.id = c.account_id AND ap.type = 'contact' AND ap.status = 'active' AND COALESCE(ap.contact_kind, '') NOT IN ('bot', 'virtual')),
         (SELECT p.id FROM parties p WHERE p.workspace_id = c.workspace_id AND p.type = 'contact' AND p.status = 'active' AND p.user_id = c.created_by)),
       'owner', 'seed',
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER), CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM connections c
WHERE c.status != 'revoked'
  AND COALESCE(
         (SELECT a.party_id FROM accounts a JOIN parties ap ON ap.id = a.party_id
           WHERE a.id = c.account_id AND ap.type = 'contact' AND ap.status = 'active' AND COALESCE(ap.contact_kind, '') NOT IN ('bot', 'virtual')),
         (SELECT p.id FROM parties p WHERE p.workspace_id = c.workspace_id AND p.type = 'contact' AND p.status = 'active' AND p.user_id = c.created_by)) IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM stewards s WHERE s.workspace_id = c.workspace_id AND s.object_type = 'connection' AND s.object_id = c.id);
