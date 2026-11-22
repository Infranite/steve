-- 115: the Hub applies a Node's events as it receives them and keeps no copy of them. The inbox and
-- the quarantine go; what was read back from them is kept where it is used, as events are applied:
-- a session's last resume handle and the Node views seen since its last open (runtime_sessions), an
-- attempt's last agent activity (runtime_attempts), the attempt a session event came from (events),
-- what an attempt did so far (runtime_attempt_facts) and the last running cost of a harness process
-- (runtime_cost_marks). A stream keeps one position, what is applied; a fault names its session,
-- the event and why.
ALTER TABLE runtime_sessions ADD COLUMN node_resume TEXT;
ALTER TABLE runtime_sessions ADD COLUMN node_views TEXT NOT NULL DEFAULT '[]';
ALTER TABLE runtime_attempts ADD COLUMN active_at INTEGER;
ALTER TABLE events ADD COLUMN source_attempt TEXT;
CREATE TABLE runtime_attempt_facts (
 attempt_id TEXT PRIMARY KEY REFERENCES runtime_attempts(attempt_id) ON DELETE CASCADE,
 tools TEXT NOT NULL DEFAULT '{}', permissions INTEGER NOT NULL DEFAULT 0, model TEXT, cost REAL);
CREATE TABLE runtime_cost_marks (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL, meter_epoch TEXT NOT NULL,
 cost REAL NOT NULL, PRIMARY KEY(node_id,stream,epoch,meter_epoch)) WITHOUT ROWID;

-- once, from the inbox: the views and resume handles of the sessions, and the attempts of the
-- session events it still names
UPDATE runtime_sessions SET
 node_revision = COALESCE(node_revision, (
   SELECT json_extract(e.payload,'$.body.view.revision') FROM node_event_inbox e
    WHERE e.node_id=runtime_sessions.node_id AND json_extract(e.payload,'$.body.type')='session_changed'
      AND json_extract(e.payload,'$.body.view.id')=runtime_sessions.session_id
    ORDER BY e.received DESC, e.rowid DESC LIMIT 1)),
 node_resume = (
   SELECT json_extract(e.payload,'$.body.view.resume') FROM node_event_inbox e
    WHERE e.node_id=runtime_sessions.node_id AND json_extract(e.payload,'$.body.type')='session_changed'
      AND json_extract(e.payload,'$.body.view.id')=runtime_sessions.session_id
      AND json_extract(e.payload,'$.body.view.resume') IS NOT NULL
    ORDER BY e.received DESC, e.seq DESC LIMIT 1);
UPDATE runtime_sessions SET node_views = json_array(o.revision)
  FROM (SELECT json_extract(c.request,'$.body.session') AS session,
               json_extract(c.receipt,'$.payload.state.result.revision') AS revision,
               ROW_NUMBER() OVER (PARTITION BY json_extract(c.request,'$.body.session') ORDER BY c.created DESC, c.rowid DESC) AS n
          FROM node_commands c
         WHERE c.method='open_session' AND c.state='finished' AND json_extract(c.receipt,'$.success')=1) AS o
 WHERE o.n=1 AND o.session=runtime_sessions.session_id AND runtime_sessions.phase NOT IN ('unopened','released')
   AND EXISTS(SELECT 1 FROM node_event_inbox e
               WHERE e.node_id=runtime_sessions.node_id AND json_extract(e.payload,'$.body.type')='session_changed'
                 AND json_extract(e.payload,'$.body.view.id')=runtime_sessions.session_id
                 AND json_extract(e.payload,'$.body.view.revision')=o.revision);
UPDATE events SET source_attempt = i.attempt
  FROM (SELECT node_id, stream, epoch, seq, json_extract(payload,'$.subject.attempt') AS attempt
          FROM node_event_inbox WHERE json_extract(payload,'$.subject.attempt') IS NOT NULL) AS i
 WHERE events.source_node IS NOT NULL AND events.source_node=i.node_id AND events.source_stream=i.stream
   AND events.source_epoch=i.epoch AND events.source_seq=i.seq;

CREATE TABLE node_streams_115 (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL,
 applied INTEGER NOT NULL DEFAULT 0 CHECK(applied>=0),
 session_id TEXT,
 fault TEXT CHECK(fault IS NULL OR fault IN ('failed','gap','ahead')),
 fault_event TEXT, fault_reason TEXT,
 PRIMARY KEY(node_id,stream,epoch));
INSERT INTO node_streams_115(node_id,stream,epoch,applied,session_id,fault,fault_event,fault_reason)
SELECT s.node_id, s.stream, s.epoch, s.applied,
  COALESCE(
   (SELECT json_extract(e.payload,'$.subject.session') FROM node_event_inbox e
     WHERE e.node_id=s.node_id AND e.stream=s.stream AND e.epoch=s.epoch
       AND json_extract(e.payload,'$.subject.session') IS NOT NULL LIMIT 1),
   (SELECT json_extract(q.payload,'$.subject.session') FROM node_event_quarantine q
     WHERE q.node_id=s.node_id AND q.stream=s.stream AND q.epoch=s.epoch
       AND json_extract(q.payload,'$.subject.session') IS NOT NULL LIMIT 1)),
  CASE s.fault WHEN 'conflict' THEN 'failed' ELSE s.fault END,
  (SELECT q.event_id FROM node_event_quarantine q
    WHERE q.node_id=s.node_id AND q.stream=s.stream AND q.epoch=s.epoch ORDER BY q.seq LIMIT 1),
  (SELECT q.reason FROM node_event_quarantine q
    WHERE q.node_id=s.node_id AND q.stream=s.stream AND q.epoch=s.epoch ORDER BY q.seq LIMIT 1)
 FROM node_streams s;
DROP TABLE node_event_quarantine;
DROP TABLE node_event_inbox;
DROP TABLE node_streams;
PRAGMA legacy_alter_table=ON;
ALTER TABLE node_streams_115 RENAME TO node_streams;
PRAGMA legacy_alter_table=OFF;
CREATE INDEX node_streams_faulted ON node_streams(node_id) WHERE fault IS NOT NULL;
