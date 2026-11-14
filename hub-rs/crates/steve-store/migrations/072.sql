-- Explicit personal consent is the only notification recipient source. No workspace owner fallback.
CREATE TABLE notification_policies (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
  mode TEXT NOT NULL CHECK(mode IN ('now','digest','log')),
  party_id TEXT,
  space_id TEXT,
  timezone TEXT NOT NULL,
  quiet_start INTEGER,
  quiet_end INTEGER,
  consented_at INTEGER NOT NULL,
  consent_since_seq INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  PRIMARY KEY(workspace_id,user_id),
  CHECK((quiet_start IS NULL AND quiet_end IS NULL) OR
        (quiet_start BETWEEN 0 AND 1439 AND quiet_end BETWEEN 0 AND 1439 AND quiet_start <> quiet_end))
);
-- This is personal inbox/policy state, not a second send ledger. The only delivery is E1/C6's.
-- Event ids remain as tombstone keys after the source outbox's ordinary retention sweep.
CREATE TABLE personal_notifications (
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
  UNIQUE(workspace_id,user_id,event_id)
);
CREATE INDEX personal_notifications_due ON personal_notifications(state,next_attempt_at,created);
CREATE INDEX personal_notifications_delivery ON personal_notifications(delivery_id) WHERE delivery_id IS NOT NULL;
-- Purging/deleting an outbound receipt must not retain another copy of its body or resend it.
CREATE TRIGGER notification_delivery_deleted BEFORE DELETE ON output_deliveries BEGIN
  UPDATE personal_notifications SET body=NULL,state='purged' WHERE delivery_id=OLD.id;
END;
CREATE TRIGGER notification_delivery_purged AFTER UPDATE OF intent ON output_deliveries
WHEN json_type(NEW.intent,'$.content.markdown') IS NULL BEGIN
  UPDATE personal_notifications SET body=NULL,state='purged' WHERE delivery_id=NEW.id;
END;
-- The ordinary channel retention sweep also purges semantic intent/inbox body copies.
CREATE TRIGGER notification_message_purged AFTER UPDATE OF text ON chan_messages
WHEN NEW.text IS NULL AND OLD.text IS NOT NULL BEGIN
  UPDATE output_deliveries SET intent=json_set(intent,'$.content',json('{}')) WHERE message_id=NEW.id;
END;
