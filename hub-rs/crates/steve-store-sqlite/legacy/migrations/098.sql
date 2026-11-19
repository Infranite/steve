-- Personal attention metadata only: no message bodies, decisions or Feed copies.
-- Reply history is deliberately prospective: no reply-history scan/backfill.
-- Existing Feed receives the one-time metadata-only unread index defined below.
CREATE TABLE attention_meta (id INTEGER PRIMARY KEY CHECK(id=1), epoch TEXT NOT NULL);
INSERT INTO attention_meta VALUES (1, lower(hex(randomblob(16))));
CREATE TABLE thread_reply_commits (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id TEXT NOT NULL UNIQUE REFERENCES messages(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL REFERENCES thread_runs(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL
);
CREATE INDEX thread_reply_commits_person ON thread_reply_commits(workspace_id,user_id,seq);
CREATE TABLE thread_reply_reads (
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL, space TEXT NOT NULL CHECK(space='steve'),
  through_seq INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(workspace_id,user_id,space)
);
-- Both short answers and research persist a named run_receipt before completing their run.
-- Looking up that explicit reply_id avoids any historical message scan on every final commit.
CREATE TRIGGER attention_final_reply AFTER UPDATE ON thread_runs
WHEN NEW.status IN ('done','stopped','budget','violated','failed') AND NEW.user_id IS NOT NULL
  AND (OLD.status<>NEW.status OR OLD.reply_id IS NOT NEW.reply_id)
BEGIN
  INSERT OR IGNORE INTO thread_reply_commits(message_id,run_id,workspace_id,user_id)
  SELECT m.id, NEW.id, NEW.workspace_id, NEW.user_id FROM messages m
  JOIN messages origin ON origin.id=NEW.message_id
  WHERE m.id=NEW.reply_id AND m.workspace_id=NEW.workspace_id AND m.space='steve' AND m.author='steve'
    AND origin.workspace_id=NEW.workspace_id AND origin.space='steve' AND origin.author='me'
    AND json_valid(m.meta) AND json_extract(m.meta,'$.run')=NEW.id
    AND json_extract(m.meta,'$.run_receipt')=1 AND json_extract(m.meta,'$.origin')=NEW.message_id;
END;
-- Expiring opaque pagination/read capabilities, not an event store. A digest pins source membership;
-- changed sources force an explicit snapshot restart instead of silently skipping entries.
CREATE TABLE attention_windows (
  token TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, user_id TEXT NOT NULL,
  kind TEXT NOT NULL, revision TEXT NOT NULL, upper_seq INTEGER NOT NULL,
  position INTEGER NOT NULL, expires INTEGER NOT NULL
);
CREATE INDEX attention_windows_scope ON attention_windows(workspace_id,user_id,kind,revision,position,expires);
CREATE INDEX attention_windows_expiry ON attention_windows(expires);

-- A materialized unread INDEX, not a Feed/event copy: only source references and sequence.
-- Authoritative Feed/read/membership writes maintain it in the same SQLite transaction.
-- Scope version/count make unchanged polling O(1); keyset pages touch at most limit+1 candidates.
-- Existing Feed is indexed once at migration; no historical scan on polling or process startup.
CREATE TABLE attention_feed_scopes (
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL,
  revision TEXT NOT NULL DEFAULT (lower(hex(randomblob(16)))),
  unread_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(workspace_id,user_id),
  FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id) ON DELETE CASCADE
);
CREATE TABLE attention_feed_unread (
  workspace_id TEXT NOT NULL, user_id TEXT NOT NULL,
  seq INTEGER NOT NULL, entry_id TEXT NOT NULL REFERENCES feed_entries(id) ON DELETE CASCADE,
  PRIMARY KEY(workspace_id,user_id,seq),
  FOREIGN KEY(workspace_id,user_id) REFERENCES attention_feed_scopes(workspace_id,user_id) ON DELETE CASCADE
);
CREATE INDEX attention_feed_unread_entry ON attention_feed_unread(entry_id);
CREATE TRIGGER attention_feed_count_add AFTER INSERT ON attention_feed_unread BEGIN
  UPDATE attention_feed_scopes SET unread_count=unread_count+1,revision=lower(hex(randomblob(16)))
    WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id;
END;
CREATE TRIGGER attention_feed_count_remove AFTER DELETE ON attention_feed_unread BEGIN
  UPDATE attention_feed_scopes SET unread_count=unread_count-1,revision=lower(hex(randomblob(16)))
    WHERE workspace_id=OLD.workspace_id AND user_id=OLD.user_id;
END;
INSERT INTO attention_feed_scopes(workspace_id,user_id) SELECT workspace_id,user_id FROM workspace_members;
INSERT INTO attention_feed_unread
  SELECT e.workspace_id,m.user_id,e.seq,e.id FROM feed_entries e
  JOIN workspace_members m ON m.workspace_id=e.workspace_id AND (e.audience='workspace' OR e.recipient_id=m.user_id)
  WHERE NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=e.workspace_id AND r.user_id=m.user_id AND r.entry_id=e.id);
CREATE TRIGGER attention_feed_member_add AFTER INSERT ON workspace_members BEGIN
  INSERT INTO attention_feed_scopes(workspace_id,user_id) VALUES(NEW.workspace_id,NEW.user_id);
  INSERT INTO attention_feed_unread SELECT e.workspace_id,NEW.user_id,e.seq,e.id FROM feed_entries e
    WHERE e.workspace_id=NEW.workspace_id AND (e.audience='workspace' OR e.recipient_id=NEW.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=e.workspace_id AND r.user_id=NEW.user_id AND r.entry_id=e.id);
END;
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
CREATE TRIGGER attention_feed_read_add AFTER INSERT ON feed_reads BEGIN
  DELETE FROM attention_feed_unread WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id
    AND seq=(SELECT seq FROM feed_entries WHERE id=NEW.entry_id);
END;
CREATE TRIGGER attention_feed_read_remove AFTER DELETE ON feed_reads BEGIN
  INSERT OR IGNORE INTO attention_feed_unread SELECT e.workspace_id,OLD.user_id,e.seq,e.id FROM feed_entries e
    JOIN workspace_members m ON m.workspace_id=e.workspace_id AND m.user_id=OLD.user_id
    WHERE e.id=OLD.entry_id AND e.workspace_id=OLD.workspace_id AND (e.audience='workspace' OR e.recipient_id=OLD.user_id);
END;
CREATE TRIGGER attention_feed_read_update AFTER UPDATE ON feed_reads BEGIN
  INSERT OR IGNORE INTO attention_feed_unread SELECT e.workspace_id,OLD.user_id,e.seq,e.id FROM feed_entries e
    JOIN workspace_members m ON m.workspace_id=e.workspace_id AND m.user_id=OLD.user_id
    WHERE e.id=OLD.entry_id AND e.workspace_id=OLD.workspace_id AND (e.audience='workspace' OR e.recipient_id=OLD.user_id)
      AND NOT EXISTS(SELECT 1 FROM feed_reads r WHERE r.workspace_id=e.workspace_id AND r.user_id=OLD.user_id AND r.entry_id=e.id);
  DELETE FROM attention_feed_unread WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id
    AND seq=(SELECT seq FROM feed_entries WHERE id=NEW.entry_id);
END;
