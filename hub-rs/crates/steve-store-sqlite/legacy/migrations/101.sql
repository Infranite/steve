-- 101: a starred contact's message to you on your own account becomes a personal notification (steve-channels
-- `notifications::starred`), through your notification policy and its route. `personal_notifications.kind`:
-- `completion` (a work item done, as before) or `starred`; `message_id`: the inbound message it is about (whether you
-- answered it yourself is asked again before it goes out). `notification_policies.starred_in_quiet`: a starred
-- contact's message is sent within your quiet hours too (off by default). The feed reads it as its own kind: the
-- table is rebuilt to allow `starred` (db.rs runs this with foreign_keys off; only `feed_reads` refers to it, by
-- name), its sequence carried over, and the insert trigger names the kind. 098's unread index (`attention_feed_unread`)
-- keeps its rows: the entries keep their ids and numbers, its foreign key names the table, and the triggers 098 put
-- on `feed_entries` (dropped with the old table) are created again as they were.
ALTER TABLE personal_notifications ADD COLUMN kind TEXT NOT NULL DEFAULT 'completion' CHECK(kind IN ('completion','starred'));
ALTER TABLE personal_notifications ADD COLUMN message_id TEXT;
ALTER TABLE notification_policies ADD COLUMN starred_in_quiet INTEGER NOT NULL DEFAULT 0;
DROP TRIGGER notification_feed_insert;
CREATE TABLE feed_entries_new (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  audience TEXT NOT NULL CHECK(audience IN ('workspace','user')),
  recipient_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('report','completion','starred')),
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
     OR (kind IN ('completion','starred') AND source_type='notification' AND audience='user' AND content IS NULL AND result IS NULL)),
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
    VALUES('feed_'||NEW.id,NEW.workspace_id,'user',NEW.user_id,NEW.kind,'notification',json_array(NEW.user_id,NEW.event_id),NEW.id,NEW.id,(SELECT json_extract(o.payload,'$.item_id') FROM outbox_events o WHERE o.id=NEW.event_id AND o.workspace_id=NEW.workspace_id AND o.topic='work.item.done'),NEW.created) ON CONFLICT(workspace_id,source_type,source_key) DO NOTHING;
END;
-- A starred contact's notification copies part of their message, with who and where, into its body, its own outbound
-- message to you and that delivery's content. Purging the message's text (retention, 删除消息正文) or deleting it
-- (清理记录) purges every copy too, and one not sent yet is let go: never a second body outside its retention contract.
CREATE TRIGGER starred_source_purged AFTER UPDATE OF text ON chan_messages
WHEN NEW.text IS NULL AND OLD.text IS NOT NULL AND NEW.direction = 'in' BEGIN
  UPDATE chan_messages SET text = NULL, attachments = '[]',
         purged = coalesce(NEW.purged, CAST(strftime('%s', 'now') AS INTEGER) * 1000),
         status = CASE WHEN status = 'pending' THEN 'discarded' ELSE status END
   WHERE id IN (SELECT od.message_id FROM personal_notifications n JOIN output_deliveries od ON od.id = n.delivery_id
                 WHERE n.kind = 'starred' AND n.message_id = NEW.id);
  UPDATE output_deliveries SET intent = json_set(intent, '$.content', json('{}'))
   WHERE id IN (SELECT delivery_id FROM personal_notifications WHERE kind = 'starred' AND message_id = NEW.id);
  UPDATE personal_notifications SET body = NULL, state = 'purged' WHERE kind = 'starred' AND message_id = NEW.id;
END;
CREATE TRIGGER starred_source_deleted BEFORE DELETE ON chan_messages
WHEN OLD.direction = 'in' BEGIN
  UPDATE chan_messages SET text = NULL, attachments = '[]', purged = CAST(strftime('%s', 'now') AS INTEGER) * 1000,
         status = CASE WHEN status = 'pending' THEN 'discarded' ELSE status END
   WHERE id IN (SELECT od.message_id FROM personal_notifications n JOIN output_deliveries od ON od.id = n.delivery_id
                 WHERE n.kind = 'starred' AND n.message_id = OLD.id);
  UPDATE output_deliveries SET intent = json_set(intent, '$.content', json('{}'))
   WHERE id IN (SELECT delivery_id FROM personal_notifications WHERE kind = 'starred' AND message_id = OLD.id);
  UPDATE personal_notifications SET body = NULL, state = 'purged' WHERE kind = 'starred' AND message_id = OLD.id;
END;
