-- 19: one space per chat, and a space's own axes (domain-model.md §1.4, branch fix/channels-ux).
--
-- A space exists once per (installation, external id), however many identities see it. The installation of a
-- connection is its own id, except a Feishu user connection (`feishu_user`), whose installation is the app
-- connection in `config.app_connection` (`installation_of` in steve-channels/src/spaces.rs is the same rule).
-- `spaces.connection_id` now means “first seen via”; which identities see the space is `space_identities`.
--
-- Sibling rows that existed before (your self poller and a bot each kept their own row for the same chat) are
-- merged into the oldest one: every reference is repointed, and `space_merges` records each merge (plus an audit
-- row `space.merge`). Messages stored in both copies keep both rows; the second copy's external id gets a
-- `#dup:<id>` suffix so the (space, external id) index stays unique.
--
-- space_prefs: attention realtime（实时，default）| quiet（不提醒：记录、可搜索，不进待处理、不起草、不提醒）| blocked（屏蔽：
-- 不处理、不存新消息，列表默认隐藏）; voice = the connection that speaks for Steve here, NULL = automatic (a bot in the
-- space, else your own account). 静默 stays space_settings.muted (Steve's role 只记录), not an attention level.
--
-- New tables are IF NOT EXISTS, every statement can run again and then changes nothing.
CREATE TABLE IF NOT EXISTS space_identities (
  space_id TEXT NOT NULL REFERENCES spaces(id), connection_id TEXT NOT NULL, workspace_id TEXT NOT NULL,
  joined INTEGER NOT NULL, left_at INTEGER,     -- NULL = still sees it
  PRIMARY KEY (space_id, connection_id));
CREATE INDEX IF NOT EXISTS space_identities_conn ON space_identities(connection_id, space_id);

CREATE TABLE IF NOT EXISTS space_prefs (
  space_id TEXT PRIMARY KEY REFERENCES spaces(id), workspace_id TEXT NOT NULL,
  attention TEXT NOT NULL DEFAULT 'realtime',
  voice TEXT,
  updated INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS space_merges (
  from_id TEXT PRIMARY KEY, into_id TEXT NOT NULL, workspace_id TEXT NOT NULL,
  installation TEXT NOT NULL, external_id TEXT NOT NULL, merged INTEGER NOT NULL);

DROP TABLE IF EXISTS temp.space_inst;
CREATE TEMP TABLE space_inst AS
  SELECT s.id, s.workspace_id, s.external_id, s.created,
         CASE WHEN c.adapter = 'feishu_user' AND json_valid(c.config)
                   AND coalesce(json_extract(c.config, '$.app_connection'), '') <> ''
              THEN json_extract(c.config, '$.app_connection') ELSE c.id END AS inst
    FROM spaces s JOIN connections c ON c.id = s.connection_id;

INSERT OR IGNORE INTO space_merges (from_id, into_id, workspace_id, installation, external_id, merged)
  SELECT a.id, k.id, a.workspace_id, a.inst, a.external_id, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
    FROM space_inst a
    JOIN space_inst k ON k.id = (SELECT b.id FROM space_inst b
                                  WHERE b.workspace_id = a.workspace_id AND b.inst = a.inst AND b.external_id = a.external_id
                                  ORDER BY b.created, b.id LIMIT 1)
   WHERE k.id <> a.id;
DROP TABLE IF EXISTS temp.space_inst;

-- which identities see each space (merged rows count for the space they went into)
INSERT OR IGNORE INTO space_identities (space_id, connection_id, workspace_id, joined, left_at)
  SELECT coalesce(m.into_id, s.id), s.connection_id, s.workspace_id, s.created,
         CASE WHEN s.status = 'active' THEN NULL ELSE s.updated END
    FROM spaces s LEFT JOIN space_merges m ON m.from_id = s.id
   WHERE s.connection_id IS NOT NULL;

-- the kept row takes what the others knew
UPDATE spaces SET
  last_message = (SELECT max(coalesce(x.last_message, 0)) FROM spaces x LEFT JOIN space_merges m ON m.from_id = x.id
                   WHERE x.id = spaces.id OR m.into_id = spaces.id),
  has_external = (SELECT max(x.has_external) FROM spaces x JOIN space_merges m ON m.from_id = x.id WHERE m.into_id = spaces.id) | has_external,
  member_count = max(member_count, (SELECT max(x.member_count) FROM spaces x JOIN space_merges m ON m.from_id = x.id WHERE m.into_id = spaces.id)),
  name = CASE WHEN trim(name) IN ('', '私聊')
              THEN coalesce((SELECT x.name FROM spaces x JOIN space_merges m ON m.from_id = x.id
                              WHERE m.into_id = spaces.id AND trim(x.name) NOT IN ('', '私聊') ORDER BY x.created LIMIT 1), name)
              ELSE name END,
  status = CASE WHEN status <> 'active' AND EXISTS (SELECT 1 FROM spaces x JOIN space_merges m ON m.from_id = x.id
                                                   WHERE m.into_id = spaces.id AND x.status = 'active') THEN 'active' ELSE status END
 WHERE id IN (SELECT into_id FROM space_merges) AND EXISTS (SELECT 1 FROM space_merges m JOIN spaces x ON x.id = m.from_id WHERE m.into_id = spaces.id);

-- messages
UPDATE chan_messages SET external_id = external_id || '#dup:' || id
 WHERE space_id IN (SELECT from_id FROM space_merges) AND external_id IS NOT NULL
   AND EXISTS (SELECT 1 FROM chan_messages t JOIN space_merges m ON m.into_id = t.space_id
                WHERE m.from_id = chan_messages.space_id AND t.external_id = chan_messages.external_id);
UPDATE chan_messages SET space_id = (SELECT into_id FROM space_merges WHERE from_id = chan_messages.space_id)
 WHERE space_id IN (SELECT from_id FROM space_merges);

-- members, settings, bindings
INSERT OR IGNORE INTO space_members (workspace_id, space_id, account_id, role, joined)
  SELECT x.workspace_id, m.into_id, x.account_id, x.role, x.joined FROM space_members x JOIN space_merges m ON m.from_id = x.space_id;
DELETE FROM space_members WHERE space_id IN (SELECT from_id FROM space_merges);
-- settings you made (configured, or muted) win over a copy nobody touched
DELETE FROM space_settings
 WHERE space_id IN (SELECT into_id FROM space_merges) AND configured = 0 AND muted = 0
   AND EXISTS (SELECT 1 FROM space_settings x JOIN space_merges m ON m.from_id = x.space_id
                WHERE m.into_id = space_settings.space_id AND (x.configured = 1 OR x.muted = 1));
INSERT OR IGNORE INTO space_settings (space_id, workspace_id, duty, rules, flow_id, muted, collab, memory_scope, disclosure, item_defaults, configured, revision, updated)
  SELECT m.into_id, x.workspace_id, x.duty, x.rules, x.flow_id, x.muted, x.collab, x.memory_scope, x.disclosure, x.item_defaults, x.configured, x.revision, x.updated
    FROM space_settings x JOIN space_merges m ON m.from_id = x.space_id;
DELETE FROM space_settings WHERE space_id IN (SELECT from_id FROM space_merges);
UPDATE OR IGNORE persona_bindings SET space_id = (SELECT into_id FROM space_merges WHERE from_id = persona_bindings.space_id)
 WHERE space_id IN (SELECT from_id FROM space_merges);
DELETE FROM persona_bindings WHERE space_id IN (SELECT from_id FROM space_merges);
UPDATE team_details SET space_id = (SELECT into_id FROM space_merges WHERE from_id = team_details.space_id)
 WHERE space_id IN (SELECT from_id FROM space_merges);

-- events, decisions, revisions
UPDATE ext_events SET space_id = (SELECT into_id FROM space_merges WHERE from_id = ext_events.space_id)
 WHERE space_id IN (SELECT from_id FROM space_merges);
UPDATE ext_events SET group_key = (SELECT into_id FROM space_merges WHERE from_id = ext_events.group_key)
 WHERE group_key IN (SELECT from_id FROM space_merges);
UPDATE ext_events SET visibility = 'space:' || (SELECT into_id FROM space_merges WHERE 'space:' || from_id = ext_events.visibility)
 WHERE visibility IN (SELECT 'space:' || from_id FROM space_merges);
UPDATE decisions SET subject_id = (SELECT into_id FROM space_merges WHERE from_id = decisions.subject_id)
 WHERE subject_type = 'space' AND subject_id IN (SELECT from_id FROM space_merges);
UPDATE revisions SET object_id = (SELECT into_id FROM space_merges WHERE from_id = revisions.object_id)
 WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);

-- references by (type, id)
UPDATE OR IGNORE links SET subject_id = (SELECT into_id FROM space_merges WHERE from_id = links.subject_id)
 WHERE subject_type = 'space' AND subject_id IN (SELECT from_id FROM space_merges);
DELETE FROM links WHERE subject_type = 'space' AND subject_id IN (SELECT from_id FROM space_merges);
UPDATE OR IGNORE links SET object_id = (SELECT into_id FROM space_merges WHERE from_id = links.object_id)
 WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
DELETE FROM links WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
UPDATE OR IGNORE mem_about SET object_id = (SELECT into_id FROM space_merges WHERE from_id = mem_about.object_id)
 WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
DELETE FROM mem_about WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
UPDATE OR IGNORE entity_tags SET object_id = (SELECT into_id FROM space_merges WHERE from_id = entity_tags.object_id)
 WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
DELETE FROM entity_tags WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
UPDATE OR IGNORE stewards SET object_id = (SELECT into_id FROM space_merges WHERE from_id = stewards.object_id)
 WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
DELETE FROM stewards WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
UPDATE OR IGNORE yolo_scopes SET object_id = (SELECT into_id FROM space_merges WHERE from_id = yolo_scopes.object_id)
 WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
DELETE FROM yolo_scopes WHERE object_type = 'space' AND object_id IN (SELECT from_id FROM space_merges);
UPDATE mems SET visibility = 'space:' || (SELECT into_id FROM space_merges WHERE 'space:' || from_id = mems.visibility)
 WHERE visibility IN (SELECT 'space:' || from_id FROM space_merges);

-- ids inside text (flow graphs and triggers, settings such as notifyVia); ids are random, so a plain replace is exact
UPDATE flows SET graph = (SELECT replace(flows.graph, m.from_id, m.into_id) FROM space_merges m WHERE instr(flows.graph, m.from_id) > 0 LIMIT 1)
 WHERE EXISTS (SELECT 1 FROM space_merges m WHERE instr(flows.graph, m.from_id) > 0);
UPDATE flow_versions SET graph = (SELECT replace(flow_versions.graph, m.from_id, m.into_id) FROM space_merges m WHERE instr(flow_versions.graph, m.from_id) > 0 LIMIT 1)
 WHERE EXISTS (SELECT 1 FROM space_merges m WHERE instr(flow_versions.graph, m.from_id) > 0);
UPDATE flow_triggers SET spaces = (SELECT replace(flow_triggers.spaces, m.from_id, m.into_id) FROM space_merges m WHERE instr(flow_triggers.spaces, m.from_id) > 0 LIMIT 1)
 WHERE EXISTS (SELECT 1 FROM space_merges m WHERE instr(flow_triggers.spaces, m.from_id) > 0);
UPDATE settings SET value = (SELECT replace(settings.value, m.from_id, m.into_id) FROM space_merges m WHERE instr(settings.value, m.from_id) > 0 LIMIT 1)
 WHERE EXISTS (SELECT 1 FROM space_merges m WHERE instr(settings.value, m.from_id) > 0);

-- trace, then the merged rows go
INSERT INTO audit (workspace_id, ts, actor, action, target, detail)
  SELECT m.workspace_id, m.merged, 'steve', 'space.merge', m.from_id,
         json_object('into', m.into_id, 'installation', m.installation, 'external_id', m.external_id)
    FROM space_merges m
   WHERE EXISTS (SELECT 1 FROM spaces WHERE id = m.from_id);
DELETE FROM spaces WHERE id IN (SELECT from_id FROM space_merges);
