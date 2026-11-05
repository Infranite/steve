-- D3: Hub-owned execution intents and durable Node facts. Number assigned on merge.
-- min_reader is raised by the migration runner: older recovery code must not open this schema.
CREATE TABLE runtime_sessions (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id),
 backend TEXT NOT NULL DEFAULT 'node' CHECK(backend='node'),
 node_id TEXT NOT NULL, workspace_ref TEXT NOT NULL, node_session_id TEXT NOT NULL,
 config_revision TEXT NOT NULL, phase TEXT NOT NULL,
 active_attempt TEXT, release_pending INTEGER NOT NULL DEFAULT 0 CHECK(release_pending IN (0,1)),
 revision INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL,
 UNIQUE(node_id,node_session_id));
CREATE TABLE runtime_turns (
 turn_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES runtime_sessions(session_id),
 state TEXT NOT NULL, active_attempt TEXT, retry_ordinal INTEGER NOT NULL DEFAULT 0,
 revision INTEGER NOT NULL DEFAULT 0, applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0,1)),
 created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE UNIQUE INDEX runtime_one_open_turn ON runtime_turns(session_id) WHERE state <> 'finished';
CREATE TABLE runtime_attempts (
 attempt_id TEXT PRIMARY KEY, turn_id TEXT NOT NULL REFERENCES runtime_turns(turn_id),
 ordinal INTEGER NOT NULL CHECK(ordinal>=0), node_id TEXT NOT NULL,
 operation_id TEXT NOT NULL UNIQUE, execution TEXT NOT NULL, spec TEXT NOT NULL, digest TEXT NOT NULL,
 state TEXT NOT NULL, receipt TEXT, completion TEXT, cancel_requested INTEGER NOT NULL DEFAULT 0,
 revision INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL,
 UNIQUE(turn_id,ordinal));
CREATE UNIQUE INDEX runtime_one_open_attempt ON runtime_attempts(turn_id) WHERE state <> 'finished';
CREATE TABLE node_commands (
 operation_id TEXT PRIMARY KEY, node_id TEXT NOT NULL, method TEXT NOT NULL,
 request TEXT NOT NULL, digest TEXT NOT NULL,
 target_attempt TEXT REFERENCES runtime_attempts(attempt_id),
 dependency_operation TEXT REFERENCES node_commands(operation_id),
 state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','accepted','active','unknown','finished','rejected')),
 receipt TEXT, revision INTEGER NOT NULL DEFAULT 0,
 retry_at INTEGER NOT NULL DEFAULT 0, lease_owner TEXT, lease_until INTEGER,
 lease_generation INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE INDEX node_commands_ready ON node_commands(state,retry_at,lease_until);
CREATE TABLE node_streams (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL,
 durable INTEGER NOT NULL DEFAULT 0 CHECK(durable>=0),
 applied INTEGER NOT NULL DEFAULT 0 CHECK(applied>=0 AND applied<=durable),
 PRIMARY KEY(node_id,stream,epoch));
CREATE TABLE node_event_inbox (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq>0),
 event_id TEXT NOT NULL, payload TEXT NOT NULL, applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0,1)),
 received INTEGER NOT NULL,
 PRIMARY KEY(node_id,stream,epoch,seq), UNIQUE(node_id,event_id),
 FOREIGN KEY(node_id,stream,epoch) REFERENCES node_streams(node_id,stream,epoch));
ALTER TABLE session_dispatch ADD COLUMN turn_id TEXT REFERENCES runtime_turns(turn_id);
ALTER TABLE session_dispatch ADD COLUMN backend TEXT NOT NULL DEFAULT 'legacy' CHECK(backend IN ('legacy','node'));
CREATE INDEX session_dispatch_turn ON session_dispatch(turn_id,backend,state);
ALTER TABLE events ADD COLUMN format TEXT NOT NULL DEFAULT 'legacy.acp';
ALTER TABLE events ADD COLUMN source_node TEXT;
ALTER TABLE events ADD COLUMN source_stream TEXT;
ALTER TABLE events ADD COLUMN source_epoch TEXT;
ALTER TABLE events ADD COLUMN source_seq INTEGER CHECK(
 (source_seq IS NULL AND source_node IS NULL AND source_stream IS NULL AND source_epoch IS NULL)
 OR (source_seq IS NOT NULL AND source_seq>0 AND source_node IS NOT NULL AND source_stream IS NOT NULL AND source_epoch IS NOT NULL));
CREATE UNIQUE INDEX events_node_source ON events(source_node,source_stream,source_epoch,source_seq)
 WHERE source_node IS NOT NULL;
ALTER TABLE agent_turns ADD COLUMN node_attempt_id TEXT REFERENCES runtime_attempts(attempt_id);
ALTER TABLE agent_turns ADD COLUMN logical_turn_id TEXT REFERENCES runtime_turns(turn_id);
CREATE UNIQUE INDEX agent_turns_node_attempt ON agent_turns(node_attempt_id) WHERE node_attempt_id IS NOT NULL;
