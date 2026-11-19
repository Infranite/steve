-- A durable insertion-ordered inbox: only unconsumed messages are polled by the digest.
-- An explicit INTEGER key survives VACUUM; AUTOINCREMENT prevents reuse after deletion.
CREATE TABLE digest_pending (
  position INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, message_id TEXT NOT NULL, at INTEGER NOT NULL,
  UNIQUE (workspace_id, conv, message_id));
CREATE INDEX digest_pending_conv ON digest_pending(workspace_id, conv, at, message_id);

-- Waiting conversations have constant-sized scheduling metadata. Polling a paused or budget-deferred
-- conversation must not enumerate its pending messages again. Boundary lookups use the inbox index.
CREATE TABLE digest_pending_convs (
  workspace_id TEXT NOT NULL, conv TEXT NOT NULL, messages INTEGER NOT NULL,
  oldest INTEGER, newest INTEGER,
  PRIMARY KEY (workspace_id, conv));
CREATE TRIGGER digest_pending_insert AFTER INSERT ON digest_pending
BEGIN
  INSERT INTO digest_pending_convs (workspace_id, conv, messages, oldest, newest)
  VALUES (NEW.workspace_id, NEW.conv, 1, NEW.at, NEW.at)
  ON CONFLICT (workspace_id, conv) DO UPDATE SET messages = messages + 1,
    oldest = min(oldest, NEW.at), newest = max(newest, NEW.at);
END;
CREATE TRIGGER digest_pending_delete AFTER DELETE ON digest_pending
BEGIN
  UPDATE digest_pending_convs SET messages = messages - 1,
    oldest = (SELECT min(at) FROM digest_pending WHERE workspace_id = OLD.workspace_id AND conv = OLD.conv),
    newest = (SELECT max(at) FROM digest_pending WHERE workspace_id = OLD.workspace_id AND conv = OLD.conv)
  WHERE workspace_id = OLD.workspace_id AND conv = OLD.conv;
  DELETE FROM digest_pending_convs WHERE workspace_id = OLD.workspace_id AND conv = OLD.conv AND messages = 0;
END;

-- A receipt is the acknowledgement of exactly one insertion position. This also keeps other
-- receipt writers atomic with intake, without relying on a later cleanup pass.
CREATE TRIGGER digest_receipt_ack AFTER INSERT ON digest_receipts
BEGIN
  DELETE FROM digest_pending WHERE workspace_id = NEW.workspace_id
    AND conv = NEW.conv AND message_id = NEW.message_id;
END;

-- Preserve the initial lookback of existing cursors once, during upgrade.
INSERT INTO digest_pending (workspace_id, conv, message_id, at)
SELECT m.workspace_id, 'space:' || m.space_id, m.id, m.created FROM chan_messages m
LEFT JOIN digest_cursors c ON c.workspace_id = m.workspace_id AND c.conv = 'space:' || m.space_id
WHERE m.status IN ('received', 'sent') AND (c.since IS NULL OR m.created >= c.since)
  AND NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = m.workspace_id
    AND r.conv = 'space:' || m.space_id AND r.message_id = m.id);

CREATE TRIGGER digest_chan_messages_insert AFTER INSERT ON chan_messages
WHEN NEW.status IN ('received', 'sent')
BEGIN
  INSERT OR IGNORE INTO digest_pending (workspace_id, conv, message_id, at)
  SELECT NEW.workspace_id, 'space:' || NEW.space_id, NEW.id, NEW.created
  WHERE NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = NEW.workspace_id
    AND r.conv = 'space:' || NEW.space_id AND r.message_id = NEW.id);
END;

-- A queued outbound message becomes eligible on send; moves and recalls must also update the inbox.
CREATE TRIGGER digest_chan_messages_update AFTER UPDATE OF status, workspace_id, space_id, created ON chan_messages
BEGIN
  DELETE FROM digest_pending WHERE workspace_id = OLD.workspace_id
    AND conv = 'space:' || OLD.space_id AND message_id = OLD.id;
  INSERT OR IGNORE INTO digest_pending (workspace_id, conv, message_id, at)
  SELECT NEW.workspace_id, 'space:' || NEW.space_id, NEW.id, NEW.created
  WHERE NEW.status IN ('received', 'sent')
    AND NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = NEW.workspace_id
      AND r.conv = 'space:' || NEW.space_id AND r.message_id = NEW.id);
END;

CREATE TRIGGER digest_chan_messages_delete AFTER DELETE ON chan_messages
BEGIN
  DELETE FROM digest_pending WHERE workspace_id = OLD.workspace_id
    AND conv = 'space:' || OLD.space_id AND message_id = OLD.id;
END;

-- Preserve the initial lookback of existing cursors once, during upgrade.
INSERT INTO digest_pending (workspace_id, conv, message_id, at)
SELECT m.workspace_id, 'mail_thread:' || m.thread_id, m.id, m.date FROM mail_messages m
LEFT JOIN digest_cursors c ON c.workspace_id = m.workspace_id AND c.conv = 'mail_thread:' || m.thread_id
WHERE m.status IN ('received', 'sent') AND (c.since IS NULL OR m.date >= c.since)
  AND NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = m.workspace_id
    AND r.conv = 'mail_thread:' || m.thread_id AND r.message_id = m.id);

CREATE TRIGGER digest_mail_messages_insert AFTER INSERT ON mail_messages
WHEN NEW.status IN ('received', 'sent')
BEGIN
  INSERT OR IGNORE INTO digest_pending (workspace_id, conv, message_id, at)
  SELECT NEW.workspace_id, 'mail_thread:' || NEW.thread_id, NEW.id, NEW.date
  WHERE NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = NEW.workspace_id
    AND r.conv = 'mail_thread:' || NEW.thread_id AND r.message_id = NEW.id);
END;

-- A queued outbound message becomes eligible on send; moves and recalls must also update the inbox.
CREATE TRIGGER digest_mail_messages_update AFTER UPDATE OF status, workspace_id, thread_id, date ON mail_messages
BEGIN
  DELETE FROM digest_pending WHERE workspace_id = OLD.workspace_id
    AND conv = 'mail_thread:' || OLD.thread_id AND message_id = OLD.id;
  INSERT OR IGNORE INTO digest_pending (workspace_id, conv, message_id, at)
  SELECT NEW.workspace_id, 'mail_thread:' || NEW.thread_id, NEW.id, NEW.date
  WHERE NEW.status IN ('received', 'sent')
    AND NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = NEW.workspace_id
      AND r.conv = 'mail_thread:' || NEW.thread_id AND r.message_id = NEW.id);
END;

CREATE TRIGGER digest_mail_messages_delete AFTER DELETE ON mail_messages
BEGIN
  DELETE FROM digest_pending WHERE workspace_id = OLD.workspace_id
    AND conv = 'mail_thread:' || OLD.thread_id AND message_id = OLD.id;
END;

