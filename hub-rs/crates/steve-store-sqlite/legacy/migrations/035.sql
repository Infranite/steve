-- 35: indexes for lookups that scanned whole tables, often on a timer or per message. Indexes only,
-- no table changes; each one is `IF NOT EXISTS`, so running the file again changes nothing. A
-- partial index is used where every query of it carries the matching literal term.

-- channels: the send queue (every 2 s), a space's messages, a chat by its provider id, a handle
-- without its family, and a message's reply basis
CREATE INDEX IF NOT EXISTS chan_messages_outbox ON chan_messages(status, created) WHERE direction = 'out';
CREATE INDEX IF NOT EXISTS chan_messages_space_created ON chan_messages(space_id, created, id);
CREATE INDEX IF NOT EXISTS spaces_ext ON spaces(workspace_id, external_id);
CREATE INDEX IF NOT EXISTS account_handles_handle ON account_handles(workspace_id, handle);
CREATE INDEX IF NOT EXISTS reply_basis_msg ON reply_basis(message_id, created);

-- decisions about one subject, and the stats refresher's "anything new since" poll (every 2 s)
CREATE INDEX IF NOT EXISTS decisions_subject ON decisions(subject_type, subject_id, status);
CREATE INDEX IF NOT EXISTS decisions_created_item ON decisions(created) WHERE item_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS decisions_resolved_item ON decisions(resolved) WHERE item_id IS NOT NULL;

-- flows: a flow's runs without its workspace, the runs of an event, a connection's events
CREATE INDEX IF NOT EXISTS flow_runs_flow_started ON flow_runs(flow_id, started);
CREATE INDEX IF NOT EXISTS flow_runs_event ON flow_runs(event_id) WHERE event_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ext_events_conn_type ON ext_events(connection_id, type, received) WHERE connection_id IS NOT NULL;

-- mail: a Message-ID within a mailbox, messages due to expire, a thread's drafts
CREATE INDEX IF NOT EXISTS mail_messages_conn_mid ON mail_messages(connection_id, message_id);
CREATE INDEX IF NOT EXISTS mail_messages_expire ON mail_messages(workspace_id, date) WHERE purged IS NULL;
CREATE INDEX IF NOT EXISTS mail_drafts_thread ON mail_drafts(thread_id, status);

-- GitHub deliveries still to process (every 30 s)
CREATE INDEX IF NOT EXISTS gh_deliveries_pending ON gh_deliveries(received) WHERE status = 'received';

-- vault: waiting uses to expire, and old uses to delete (hourly)
CREATE INDEX IF NOT EXISTS secret_uses_status ON secret_uses(status, created);
CREATE INDEX IF NOT EXISTS secret_uses_created ON secret_uses(created);

-- sessions and thread runs: MCP token lookups (every MCP call) and the newest sessions first
CREATE INDEX IF NOT EXISTS sessions_token ON sessions(token_hash) WHERE token_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS sessions_ws_updated ON sessions(workspace_id, updated);
CREATE INDEX IF NOT EXISTS sessions_updated ON sessions(updated);
CREATE INDEX IF NOT EXISTS thread_runs_token ON thread_runs(token_hash) WHERE token_hash IS NOT NULL;

-- stats: a session's turns, a turn's usage, the user's own messages in a period
CREATE INDEX IF NOT EXISTS agent_turns_session ON agent_turns(session_id, started);
CREATE INDEX IF NOT EXISTS usage_turn ON usage_records(turn_id) WHERE turn_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS timeline_me_ts ON timeline(ts, item_id) WHERE kind = 'me' AND actor = 'me';

-- items: who created one (audit), plan rounds and other revisions, outputs, tags and dependents
CREATE INDEX IF NOT EXISTS audit_target ON audit(target, action, id);
CREATE INDEX IF NOT EXISTS revisions_object ON revisions(object_type, object_id, version);
CREATE INDEX IF NOT EXISTS artifacts_item ON artifacts(item_id, created);
CREATE INDEX IF NOT EXISTS item_tags_tag ON item_tags(tag_id);
CREATE INDEX IF NOT EXISTS item_deps_dep ON item_deps(dep_id);

-- YOLO: a window's actions (every state read) and a decision's
CREATE INDEX IF NOT EXISTS yolo_actions_window ON yolo_actions(window_id, outcome);
CREATE INDEX IF NOT EXISTS yolo_actions_decision ON yolo_actions(decision_id, created) WHERE decision_id IS NOT NULL;
