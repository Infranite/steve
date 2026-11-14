-- Reading is distinct from external delivery. Notification entries retain only the source id,
-- never a second title, excerpt, body or delivery state outside its retention contract.
CREATE TABLE feed_entries (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  audience TEXT NOT NULL CHECK(audience IN ('workspace','user')),
  recipient_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('report','completion')),
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
     OR (kind='completion' AND source_type='notification' AND audience='user' AND content IS NULL AND result IS NULL)),
  UNIQUE(workspace_id,source_type,source_key)
);
CREATE INDEX feed_scope ON feed_entries(workspace_id,kind,seq);
CREATE INDEX feed_recipient ON feed_entries(workspace_id,recipient_id,seq);
CREATE TABLE feed_reads (
  workspace_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  entry_id TEXT NOT NULL REFERENCES feed_entries(id) ON DELETE CASCADE,
  read_at INTEGER NOT NULL,
  PRIMARY KEY(workspace_id,user_id,entry_id),
  FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id) ON DELETE CASCADE
);
-- Opaque capabilities are bound to the caller, filters and upper bound, not supplied sequence
-- numbers. They authorize no data by themselves: every use rechecks live membership/audience.
CREATE TABLE feed_windows (
  token TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  kind TEXT,
  source TEXT,
  unread INTEGER NOT NULL,
  upper_seq INTEGER NOT NULL,
  before_seq INTEGER,
  expires INTEGER NOT NULL,
  FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id) ON DELETE CASCADE
);
CREATE INDEX feed_windows_expiry ON feed_windows(expires);
CREATE INDEX feed_windows_scope ON feed_windows(workspace_id,user_id,kind,source,unread,upper_seq,before_seq,expires);
INSERT INTO feed_entries(id,workspace_id,audience,recipient_id,kind,source_type,source_key,source_id,notification_id,item_id,published_at)
  SELECT 'feed_'||id,workspace_id,'user',user_id,'completion','notification',json_array(user_id,event_id),id,id,(SELECT json_extract(o.payload,'$.item_id') FROM outbox_events o WHERE o.id=personal_notifications.event_id AND o.workspace_id=personal_notifications.workspace_id AND o.topic='work.item.done'),created
  FROM personal_notifications ORDER BY created,id;
INSERT INTO feed_reads(workspace_id,user_id,entry_id,read_at)
  SELECT n.workspace_id,n.user_id,'feed_'||n.id,n.seen_at FROM personal_notifications n
  JOIN workspace_members m ON m.workspace_id=n.workspace_id AND m.user_id=n.user_id WHERE n.seen_at IS NOT NULL;
-- Future intake is still exclusively the explicit-consent personal notification producer.
CREATE TRIGGER notification_feed_insert AFTER INSERT ON personal_notifications BEGIN
  INSERT INTO feed_entries(id,workspace_id,audience,recipient_id,kind,source_type,source_key,source_id,notification_id,item_id,published_at)
    VALUES('feed_'||NEW.id,NEW.workspace_id,'user',NEW.user_id,'completion','notification',json_array(NEW.user_id,NEW.event_id),NEW.id,NEW.id,(SELECT json_extract(o.payload,'$.item_id') FROM outbox_events o WHERE o.id=NEW.event_id AND o.workspace_id=NEW.workspace_id AND o.topic='work.item.done'),NEW.created) ON CONFLICT(workspace_id,source_type,source_key) DO NOTHING;
END;
-- The only read fact now lives in feed_reads. Legacy APIs delegate to it.
UPDATE personal_notifications SET seen_at=NULL;
