-- 103: the morning brief (steve-digest `brief`). A brief is a personal schedule of mode `brief`
-- (`flows.kind = 'schedule'`); its time is expanded into `time_alarms` rows (kind `brief`, subject
-- `brief:<flow id>`), so it has no `flow_triggers` row. The triggers below mark a brief to be planned
-- again whenever what its alarms depend on changes: the schedule itself (time, switch, deletion), its
-- owner's notification time zone, the workspace time zone, the owner leaving the workspace.
CREATE TRIGGER brief_alarms_flow_insert AFTER INSERT ON flows
WHEN NEW.kind = 'schedule' AND CASE WHEN json_valid(NEW.schedule) THEN json_extract(NEW.schedule, '$.mode') END = 'brief' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'brief:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_flow_update AFTER UPDATE OF enabled, deleted, schedule, workspace_id ON flows
WHEN NEW.kind = 'schedule' AND (CASE WHEN json_valid(NEW.schedule) THEN json_extract(NEW.schedule, '$.mode') END = 'brief'
  OR CASE WHEN json_valid(OLD.schedule) THEN json_extract(OLD.schedule, '$.mode') END = 'brief') BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'brief:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_flow_delete AFTER DELETE ON flows
WHEN OLD.kind = 'schedule' AND CASE WHEN json_valid(OLD.schedule) THEN json_extract(OLD.schedule, '$.mode') END = 'brief' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (OLD.workspace_id, 'brief:' || OLD.id)
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_policy_insert AFTER INSERT ON notification_policies BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief' AND json_extract(f.schedule, '$.requester') = NEW.user_id
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_policy_update AFTER UPDATE OF timezone ON notification_policies
WHEN NEW.timezone IS NOT OLD.timezone BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief' AND json_extract(f.schedule, '$.requester') = NEW.user_id
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_tz_insert AFTER INSERT ON settings WHEN NEW.key = 'tz' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief'
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_tz_update AFTER UPDATE OF value ON settings
WHEN NEW.key = 'tz' AND NEW.value IS NOT OLD.value BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = NEW.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief'
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_tz_delete AFTER DELETE ON settings WHEN OLD.key = 'tz' BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = OLD.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief'
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER brief_alarms_member_delete AFTER DELETE ON workspace_members BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT f.workspace_id, 'brief:' || f.id FROM flows f
     WHERE f.workspace_id = OLD.workspace_id AND f.kind = 'schedule' AND json_valid(f.schedule)
       AND json_extract(f.schedule, '$.mode') = 'brief' AND json_extract(f.schedule, '$.requester') = OLD.user_id
    ON CONFLICT DO NOTHING;
END;

-- One brief per member and local day (`day`, on their clock): what it said (`material`, the rule-made
-- sections, JSON), the outbox event it was made for, and whether it was sent to their DM (`pushed`) or
-- kept in their feed only. Kept as long as a notification's body (30 days).
CREATE TABLE briefings (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL,
  day TEXT NOT NULL,
  schedule_id TEXT,
  event_id TEXT NOT NULL,
  material TEXT NOT NULL,
  pushed INTEGER NOT NULL CHECK(pushed IN (0,1)),
  created INTEGER NOT NULL,
  UNIQUE(workspace_id, user_id, day)
);
CREATE INDEX briefings_created ON briefings(created);

-- The topic `briefing.ready` and the feed kind `briefing`. Both tables are rebuilt to allow them (db.rs
-- runs this with foreign_keys off; `feed_entries`, `feed_reads` and 098's `attention_feed_unread` refer to
-- them by name), every column, row, number and read mark kept: 101's legacy `kind` and its `message_id`,
-- 102's `topic`. The triggers dropped with the old tables are made again as they were, the feed insert
-- trigger mapping every topic by name with no fallback. The triggers on other tables that read
-- `personal_notifications` (072's `notification_delivery_deleted`, 102's `starred_source_*`) name it and
-- the columns it keeps, so they are left as they are.
CREATE TABLE personal_notifications_new (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL,
  delivery_key TEXT NOT NULL UNIQUE,
  mode TEXT NOT NULL CHECK(mode IN ('now','digest','log')),
  body TEXT,
  delivery_id TEXT REFERENCES output_deliveries(id) ON DELETE SET NULL,
  state TEXT NOT NULL CHECK(state IN ('waiting','held','submitted','cancelled','purged')),
  seen_at INTEGER,
  next_attempt_at INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT 'completion' CHECK(kind IN ('completion','starred')),
  message_id TEXT,
  topic TEXT NOT NULL DEFAULT 'work.item.done'
    CHECK(topic IN ('work.item.done','todo.due','snooze.wake','contact.starred','briefing.ready')),
  UNIQUE(workspace_id,user_id,event_id)
);
INSERT INTO personal_notifications_new(id,workspace_id,user_id,event_id,delivery_key,mode,body,delivery_id,state,seen_at,next_attempt_at,created,kind,message_id,topic)
  SELECT id,workspace_id,user_id,event_id,delivery_key,mode,body,delivery_id,state,seen_at,next_attempt_at,created,kind,message_id,topic
  FROM personal_notifications;
DROP TABLE personal_notifications;
PRAGMA legacy_alter_table=ON;
ALTER TABLE personal_notifications_new RENAME TO personal_notifications;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX personal_notifications_due ON personal_notifications(state,next_attempt_at,created);
CREATE INDEX personal_notifications_delivery ON personal_notifications(delivery_id) WHERE delivery_id IS NOT NULL;

CREATE TABLE feed_entries_new (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  audience TEXT NOT NULL CHECK(audience IN ('workspace','user')),
  recipient_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('report','completion','starred','reminder','briefing')),
  source_type TEXT NOT NULL CHECK(source_type IN ('flow','notification')),
  source_key TEXT NOT NULL,
  source_id TEXT NOT NULL,
  run_id TEXT,
  node_id TEXT,
  item_id TEXT, -- source reference only, never a copied item title or completion body
  notification_id TEXT UNIQUE REFERENCES personal_notifications(id) ON DELETE SET NULL,
  content TEXT,
  result TEXT,
  published_at INTEGER NOT NULL,
  CHECK((audience='workspace' AND recipient_id IS NULL) OR (audience='user' AND recipient_id IS NOT NULL)),
  CHECK((kind='report' AND source_type='flow' AND audience='workspace' AND notification_id IS NULL AND content IS NOT NULL AND result IS NOT NULL AND run_id IS NOT NULL AND node_id IS NOT NULL)
     OR (kind IN ('completion','starred','reminder','briefing') AND source_type='notification' AND audience='user' AND content IS NULL AND result IS NULL)),
  UNIQUE(workspace_id,source_type,source_key)
);
INSERT INTO feed_entries_new(seq,id,workspace_id,audience,recipient_id,kind,source_type,source_key,source_id,run_id,node_id,item_id,notification_id,content,result,published_at)
  SELECT seq,id,workspace_id,audience,recipient_id,kind,source_type,source_key,source_id,run_id,node_id,item_id,notification_id,content,result,published_at
  FROM feed_entries ORDER BY seq;
-- a removed newest entry's number is never given again (feed windows remember upper bounds)
INSERT INTO sqlite_sequence(name,seq) SELECT 'feed_entries_new',0
  WHERE EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='feed_entries')
    AND NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='feed_entries_new');
UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT seq FROM sqlite_sequence WHERE name='feed_entries'),0))
  WHERE name='feed_entries_new';
DROP TABLE feed_entries;
PRAGMA legacy_alter_table=ON;
ALTER TABLE feed_entries_new RENAME TO feed_entries;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX feed_scope ON feed_entries(workspace_id,kind,seq);
CREATE INDEX feed_recipient ON feed_entries(workspace_id,recipient_id,seq);
CREATE TRIGGER attention_feed_entry_add AFTER INSERT ON feed_entries BEGIN
  INSERT INTO attention_feed_unread SELECT NEW.workspace_id,m.user_id,NEW.seq,NEW.id FROM workspace_members m
    WHERE m.workspace_id=NEW.workspace_id AND (NEW.audience='workspace' OR NEW.recipient_id=m.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=NEW.workspace_id AND r.user_id=m.user_id AND r.entry_id=NEW.id);
END;
CREATE TRIGGER attention_feed_entry_before_update BEFORE UPDATE ON feed_entries BEGIN
  DELETE FROM attention_feed_unread WHERE entry_id=OLD.id;
END;
CREATE TRIGGER attention_feed_entry_update AFTER UPDATE ON feed_entries BEGIN
  INSERT INTO attention_feed_unread SELECT NEW.workspace_id,m.user_id,NEW.seq,NEW.id FROM workspace_members m
    WHERE m.workspace_id=NEW.workspace_id AND (NEW.audience='workspace' OR NEW.recipient_id=m.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=NEW.workspace_id AND r.user_id=m.user_id AND r.entry_id=NEW.id);
END;
CREATE TRIGGER notification_feed_insert AFTER INSERT ON personal_notifications BEGIN
  INSERT INTO feed_entries(id,workspace_id,audience,recipient_id,kind,source_type,source_key,source_id,notification_id,item_id,published_at)
    VALUES('feed_'||NEW.id,NEW.workspace_id,'user',NEW.user_id,
      CASE NEW.topic WHEN 'work.item.done' THEN 'completion' WHEN 'todo.due' THEN 'reminder'
        WHEN 'snooze.wake' THEN 'reminder' WHEN 'contact.starred' THEN 'starred' WHEN 'briefing.ready' THEN 'briefing' END,
      'notification',json_array(NEW.user_id,NEW.event_id),NEW.id,NEW.id,
      (SELECT json_extract(o.payload,'$.item_id') FROM outbox_events o WHERE o.id=NEW.event_id AND o.workspace_id=NEW.workspace_id AND o.topic='work.item.done'),
      NEW.created) ON CONFLICT(workspace_id,source_type,source_key) DO NOTHING;
END;
