-- 102: alarms (steve-flow `alarms`). `time_alarms`: every alarm a person has, each firing at most once: a
-- todo's reminder (`todo`), a snooze wake-up (`snooze`); later the morning brief, meeting preparation and
-- meeting reminders. Its identity is (workspace, owner, kind, subject, planned time): moving a reminder
-- cancels its row and adds another, moving it back brings the cancelled row back, and a row that fired
-- never fires again. `stale_at`: past it the alarm is dropped (`stale`) instead of firing; `hold_until`:
-- the end of the owner's quiet hours it waits for; `run_ref`: the outbox event it fired as.
CREATE TABLE time_alarms (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  owner_user TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('brief','prep','remind','todo','snooze')),
  subject TEXT NOT NULL,
  planned_for INTEGER NOT NULL,
  fire_at INTEGER NOT NULL,
  stale_at INTEGER,
  hold_until INTEGER,
  state TEXT NOT NULL DEFAULT 'planned' CHECK(state IN ('planned','fired','stale','cancelled')),
  run_ref TEXT,
  fired_at INTEGER,
  created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  UNIQUE(workspace_id, owner_user, kind, subject, planned_for)
);
CREATE INDEX time_alarms_due ON time_alarms(fire_at) WHERE state = 'planned';
CREATE INDEX time_alarms_subject ON time_alarms(workspace_id, subject);
-- Subjects whose alarms are to be planned again. Written by triggers on the subject's table, so every
-- write path counts (REST, agent tools, undo, a recurring todo's next one, a row written directly).
CREATE TABLE time_alarm_dirty (
  workspace_id TEXT NOT NULL,
  subject TEXT NOT NULL,
  PRIMARY KEY(workspace_id, subject)
);
CREATE TRIGGER todo_alarms_insert AFTER INSERT ON todos
WHEN NEW.remind_at IS NOT NULL OR NEW.snooze_until IS NOT NULL BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'todo:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER todo_alarms_update AFTER UPDATE OF state, remind_at, snooze_until, owner, workspace_id ON todos
WHEN NEW.state IS NOT OLD.state OR NEW.remind_at IS NOT OLD.remind_at OR NEW.snooze_until IS NOT OLD.snooze_until
  OR NEW.owner IS NOT OLD.owner OR NEW.workspace_id IS NOT OLD.workspace_id BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (NEW.workspace_id, 'todo:' || NEW.id)
    ON CONFLICT DO NOTHING;
END;
CREATE TRIGGER todo_alarms_delete AFTER DELETE ON todos BEGIN
  INSERT INTO time_alarm_dirty(workspace_id, subject)
    SELECT OLD.workspace_id, 'todo:' || OLD.id
     WHERE EXISTS(SELECT 1 FROM time_alarms WHERE workspace_id = OLD.workspace_id AND subject = 'todo:' || OLD.id)
    ON CONFLICT DO NOTHING;
END;
-- the reminders and snoozes written before this version get planned once
INSERT INTO time_alarm_dirty(workspace_id, subject)
  SELECT workspace_id, 'todo:' || id FROM todos
   WHERE state = 'open' AND (remind_at IS NOT NULL OR snooze_until IS NOT NULL);

-- One axis says what a personal notification is about: its topic (steve-channels `notifications::Topic`; the
-- alarm topics in steve-flow `alarms::TOPICS`). `work.item.done` as before; `todo.due` (a todo's reminder) and
-- `snooze.wake` (a snoozed todo is back); `contact.starred` (a starred contact's message to you, 101). The topic
-- alone decides how it is sent and its feed kind. 101's `personal_notifications.kind` is folded in and is legacy
-- from here on: it is kept (no column is dropped) but never written again, and nothing reads it.
ALTER TABLE personal_notifications ADD COLUMN topic TEXT NOT NULL DEFAULT 'work.item.done'
  CHECK(topic IN ('work.item.done','todo.due','snooze.wake','contact.starred'));
UPDATE personal_notifications SET topic = 'contact.starred' WHERE kind = 'starred';
-- The feed reads the alarm topics as the kind `reminder`: the table is rebuilt to allow it next to 101's
-- `starred` (db.rs runs this with foreign_keys off; `feed_reads` and 098's `attention_feed_unread` refer to it,
-- by name), its sequence carried over, and 098's triggers on it created again. The insert trigger maps every
-- topic by name, with no fallback: a topic it does not name gets no kind and the insert fails.
DROP TRIGGER notification_feed_insert;
CREATE TABLE feed_entries_new (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  audience TEXT NOT NULL CHECK(audience IN ('workspace','user')),
  recipient_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('report','completion','starred','reminder')),
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
     OR (kind IN ('completion','starred','reminder') AND source_type='notification' AND audience='user' AND content IS NULL AND result IS NULL)),
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
-- dropping the old table dropped its triggers: the unread index of 098 is kept up by the same ones (its
-- rows carry the entry ids, which the copy kept)
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
        WHEN 'snooze.wake' THEN 'reminder' WHEN 'contact.starred' THEN 'starred' END,
      'notification',json_array(NEW.user_id,NEW.event_id),NEW.id,NEW.id,
      (SELECT json_extract(o.payload,'$.item_id') FROM outbox_events o WHERE o.id=NEW.event_id AND o.workspace_id=NEW.workspace_id AND o.topic='work.item.done'),
      NEW.created) ON CONFLICT(workspace_id,source_type,source_key) DO NOTHING;
END;
-- 101's purge of a starred contact's notification copies, now found by topic.
DROP TRIGGER starred_source_purged;
DROP TRIGGER starred_source_deleted;
CREATE TRIGGER starred_source_purged AFTER UPDATE OF text ON chan_messages
WHEN NEW.text IS NULL AND OLD.text IS NOT NULL AND NEW.direction = 'in' BEGIN
  UPDATE chan_messages SET text = NULL, attachments = '[]',
         purged = coalesce(NEW.purged, CAST(strftime('%s', 'now') AS INTEGER) * 1000),
         status = CASE WHEN status = 'pending' THEN 'discarded' ELSE status END
   WHERE id IN (SELECT od.message_id FROM personal_notifications n JOIN output_deliveries od ON od.id = n.delivery_id
                 WHERE n.topic = 'contact.starred' AND n.message_id = NEW.id);
  UPDATE output_deliveries SET intent = json_set(intent, '$.content', json('{}'))
   WHERE id IN (SELECT delivery_id FROM personal_notifications WHERE topic = 'contact.starred' AND message_id = NEW.id);
  UPDATE personal_notifications SET body = NULL, state = 'purged' WHERE topic = 'contact.starred' AND message_id = NEW.id;
END;
CREATE TRIGGER starred_source_deleted BEFORE DELETE ON chan_messages
WHEN OLD.direction = 'in' BEGIN
  UPDATE chan_messages SET text = NULL, attachments = '[]', purged = CAST(strftime('%s', 'now') AS INTEGER) * 1000,
         status = CASE WHEN status = 'pending' THEN 'discarded' ELSE status END
   WHERE id IN (SELECT od.message_id FROM personal_notifications n JOIN output_deliveries od ON od.id = n.delivery_id
                 WHERE n.topic = 'contact.starred' AND n.message_id = OLD.id);
  UPDATE output_deliveries SET intent = json_set(intent, '$.content', json('{}'))
   WHERE id IN (SELECT delivery_id FROM personal_notifications WHERE topic = 'contact.starred' AND message_id = OLD.id);
  UPDATE personal_notifications SET body = NULL, state = 'purged' WHERE topic = 'contact.starred' AND message_id = OLD.id;
END;
