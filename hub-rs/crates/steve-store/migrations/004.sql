
  -- 4: shared base (memories, entity tags, persisted changesets) + contacts, organizations and teams.
  -- Spec: steve-rs-plan/modules/contacts.md §1.1, §1.2. Rust Hub only; additive, nothing existing is altered.

  CREATE TABLE IF NOT EXISTS mems (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    source_type TEXT NOT NULL,
    source_id TEXT, source_label TEXT NOT NULL DEFAULT '',
    visibility TEXT NOT NULL DEFAULT 'private',
    level TEXT NOT NULL DEFAULT 'conclusion',
    status TEXT NOT NULL DEFAULT 'confirmed',
    valid_until INTEGER, writer TEXT NOT NULL,
    decision_id TEXT, created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);
  CREATE INDEX IF NOT EXISTS mems_ws ON mems(workspace_id, status, updated);
  CREATE TABLE IF NOT EXISTS mem_about (
    mem_id TEXT NOT NULL REFERENCES mems(id), workspace_id TEXT NOT NULL,
    object_type TEXT NOT NULL, object_id TEXT NOT NULL,
    PRIMARY KEY (mem_id, object_type, object_id));
  CREATE INDEX IF NOT EXISTS mem_about_obj ON mem_about(workspace_id, object_type, object_id);

  CREATE TABLE IF NOT EXISTS entity_tags (
    workspace_id TEXT NOT NULL, tag_id TEXT NOT NULL REFERENCES tags(id),
    object_type TEXT NOT NULL, object_id TEXT NOT NULL, created INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, object_type, object_id, tag_id));
  CREATE INDEX IF NOT EXISTS entity_tags_tag ON entity_tags(tag_id);

  CREATE TABLE IF NOT EXISTS changesets (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    target_type TEXT NOT NULL, target_id TEXT, target_revision INTEGER,
    title TEXT NOT NULL, href TEXT NOT NULL DEFAULT '',
    changes TEXT NOT NULL DEFAULT '[]', ops TEXT NOT NULL DEFAULT '[]', before TEXT,
    status TEXT NOT NULL DEFAULT 'draft',
    origin TEXT NOT NULL,
    request TEXT NOT NULL DEFAULT '', basis TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL, created INTEGER NOT NULL, applied INTEGER, undone INTEGER);
  CREATE INDEX IF NOT EXISTS changesets_ws ON changesets(workspace_id, status, created);

  CREATE INDEX IF NOT EXISTS links_object ON links(workspace_id, object_type, object_id);

  -- lookups by item / session that /api/state runs for every item (full scans + temp B-tree sorts without these)
  CREATE INDEX IF NOT EXISTS sessions_item ON sessions(item_id, no);
  CREATE INDEX IF NOT EXISTS decisions_item ON decisions(item_id, created);
  CREATE INDEX IF NOT EXISTS queued_session ON queued(session_id, created);

  CREATE TABLE IF NOT EXISTS parties (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    type TEXT NOT NULL,
    name TEXT NOT NULL, aliases TEXT NOT NULL DEFAULT '[]', avatar TEXT NOT NULL DEFAULT '{}',
    rels TEXT NOT NULL DEFAULT '[]',
    important INTEGER NOT NULL DEFAULT 0, note TEXT NOT NULL DEFAULT '',
    contact_kind TEXT,
    org_kind TEXT,
    user_id TEXT REFERENCES users(id),
    visibility TEXT NOT NULL DEFAULT 'workspace',
    status TEXT NOT NULL DEFAULT 'active',
    merged_into TEXT,
    source TEXT NOT NULL DEFAULT 'manual',
    source_ref TEXT, revision INTEGER NOT NULL DEFAULT 1,
    created_by TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);
  CREATE INDEX IF NOT EXISTS parties_ws ON parties(workspace_id, type, status, name);
  CREATE UNIQUE INDEX IF NOT EXISTS parties_me ON parties(workspace_id, user_id) WHERE user_id IS NOT NULL;

  CREATE TABLE IF NOT EXISTS org_domains (
    workspace_id TEXT NOT NULL, domain TEXT NOT NULL,
    org_id TEXT NOT NULL REFERENCES parties(id), created INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, domain));

  CREATE TABLE IF NOT EXISTS team_details (
    party_id TEXT PRIMARY KEY REFERENCES parties(id), workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    parent_id TEXT REFERENCES parties(id), org_id TEXT REFERENCES parties(id),
    purpose TEXT NOT NULL DEFAULT '', space_id TEXT,
    sync_connection TEXT, external_id TEXT, synced INTEGER);
  CREATE UNIQUE INDEX IF NOT EXISTS team_ext ON team_details(workspace_id, sync_connection, external_id) WHERE external_id IS NOT NULL;
  CREATE INDEX IF NOT EXISTS team_parent ON team_details(workspace_id, parent_id);

  CREATE TABLE IF NOT EXISTS team_members (
    workspace_id TEXT NOT NULL, team_id TEXT NOT NULL REFERENCES parties(id),
    contact_id TEXT NOT NULL REFERENCES parties(id),
    role TEXT NOT NULL DEFAULT 'member',
    source TEXT NOT NULL DEFAULT 'manual',
    joined INTEGER NOT NULL, PRIMARY KEY (team_id, contact_id));
  CREATE INDEX IF NOT EXISTS team_members_contact ON team_members(workspace_id, contact_id);

  CREATE TABLE IF NOT EXISTS attr_defs (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    applies TEXT NOT NULL,
    key TEXT NOT NULL, label TEXT NOT NULL,
    type TEXT NOT NULL,
    options TEXT NOT NULL DEFAULT '[]', default_level TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created INTEGER NOT NULL, UNIQUE (workspace_id, key));

  CREATE TABLE IF NOT EXISTS attr_values (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, party_id TEXT NOT NULL REFERENCES parties(id),
    key TEXT NOT NULL, label TEXT NOT NULL,
    value TEXT NOT NULL,
    source TEXT NOT NULL, source_label TEXT NOT NULL DEFAULT '', source_ref TEXT,
    level TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'confirmed',
    decision_id TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL);
  CREATE INDEX IF NOT EXISTS attr_values_party ON attr_values(workspace_id, party_id, key);

  CREATE TABLE IF NOT EXISTS merge_proposals (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    from_id TEXT NOT NULL REFERENCES parties(id), into_id TEXT NOT NULL REFERENCES parties(id),
    account_id TEXT, reason TEXT NOT NULL, confidence REAL NOT NULL,
    decision_id TEXT, status TEXT NOT NULL DEFAULT 'open',
    created INTEGER NOT NULL, resolved INTEGER);
  CREATE UNIQUE INDEX IF NOT EXISTS merge_open ON merge_proposals(workspace_id, from_id, into_id) WHERE status = 'open';

  CREATE TABLE IF NOT EXISTS merges (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    from_id TEXT NOT NULL, into_id TEXT NOT NULL,
    moved TEXT NOT NULL,
    actor TEXT NOT NULL, created INTEGER NOT NULL, split INTEGER);

  INSERT OR IGNORE INTO attr_defs (id, workspace_id, applies, key, label, type, options, default_level, source, status, created) VALUES
    ('ad_phone', '*', 'both', 'phone', '电话', 'phone', '[]', 'reasoning', 'builtin', 'active', 0),
    ('ad_address', '*', 'both', 'address', '地址', 'address', '[]', 'reasoning', 'builtin', 'active', 0),
    ('ad_birthday', '*', 'contact', 'birthday', '生日', 'date', '[]', 'reasoning', 'builtin', 'active', 0),
    ('ad_tz', '*', 'contact', 'tz', '时区', 'text', '[]', 'public', 'builtin', 'active', 0),
    ('ad_lang', '*', 'contact', 'lang', '语言', 'text', '[]', 'public', 'builtin', 'active', 0),
    ('ad_call', '*', 'contact', 'call', '偏好的称呼', 'text', '[]', 'public', 'builtin', 'active', 0),
    ('ad_website', '*', 'org', 'website', '网站', 'url', '[]', 'public', 'builtin', 'active', 0),
    ('ad_contract', '*', 'org', 'contract', '合同', 'text', '[]', 'reasoning', 'builtin', 'active', 0);
