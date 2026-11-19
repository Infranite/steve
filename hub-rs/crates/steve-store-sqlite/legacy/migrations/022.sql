-- Session-owned, content-addressed large event strings. Kept in SQLite so an online
-- database backup includes the full transcript atomically. No existing event is rewritten.
CREATE TABLE IF NOT EXISTS event_payloads (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    hash TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, hash)
);
CREATE TABLE IF NOT EXISTS event_payload_refs (
    event_seq INTEGER NOT NULL REFERENCES events(seq) ON DELETE CASCADE,
    path TEXT NOT NULL,
    session_id TEXT NOT NULL,
    hash TEXT NOT NULL,
    PRIMARY KEY (event_seq, path),
    FOREIGN KEY (session_id, hash) REFERENCES event_payloads(session_id, hash)
);
CREATE INDEX IF NOT EXISTS event_payload_refs_payload ON event_payload_refs(session_id, hash);
-- Remove a payload when its last event reference disappears, including event-only cleanup.
CREATE TRIGGER IF NOT EXISTS event_payload_refs_gc AFTER DELETE ON event_payload_refs
BEGIN
    DELETE FROM event_payloads WHERE session_id = OLD.session_id AND hash = OLD.hash
      AND NOT EXISTS (SELECT 1 FROM event_payload_refs WHERE session_id = OLD.session_id AND hash = OLD.hash);
END;
