//! Outbox state observed by the Hub integration tests.

use std::path::Path;

use crate::Result;

macro_rules! scalar {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty, $sql:literal $(,)?) => {
        pub fn $name(path: &Path, $($arg: $ty),*) -> Result<$ret> {
            let db = super::inspect_database(path)?;
            Ok(db.query_row($sql, rusqlite::params![$($arg),*], |row| row.get(0))?)
        }
    };
}

scalar!(audit_count() -> i64, "SELECT COUNT(*) FROM audit");

scalar!(
    chan_messages_status_by_id(id: &str) -> String,
    "SELECT status FROM chan_messages WHERE id=?1"
);

scalar!(
    ext_events_count_by_type(kind: &str) -> i64,
    "SELECT COUNT(*) FROM ext_events WHERE type=?1"
);

scalar!(flow_runs_count() -> i64, "SELECT COUNT(*) FROM flow_runs");

scalar!(
    flow_runs_count_by_status(status: &str) -> i64,
    "SELECT COUNT(*) FROM flow_runs WHERE status=?1"
);

scalar!(items_count_st_90_closed_done() -> i64, "SELECT COUNT(*) FROM items WHERE id='ST-90' AND phase='closed' AND outcome='completed'");

scalar!(notification_policies_count() -> i64, "SELECT COUNT(*) FROM notification_policies");

scalar!(outbox_consumers_count_work_item_done_with_since_seq() -> i64, "SELECT COUNT(*) FROM outbox_consumers WHERE topic='work.item.done' AND since_seq=0");

scalar!(outbox_consumers_count_work_item_lifecycle_with_since_seq() -> i64, "SELECT COUNT(*) FROM outbox_consumers WHERE topic='work.item.lifecycle' AND since_seq=0");

scalar!(
    outbox_events_count_by_topic(topic: &str) -> i64,
    "SELECT COUNT(*) FROM outbox_events WHERE topic=?1"
);

scalar!(outbox_leases_count_with_dead_with_attempts() -> i64, "SELECT COUNT(*) FROM outbox_leases WHERE dead=0 AND attempts=0");

scalar!(outbox_receipts_count() -> i64, "SELECT COUNT(*) FROM outbox_receipts");

scalar!(
    outbox_receipts_count_by_consumer(consumer: &str) -> i64,
    "SELECT COUNT(*) FROM outbox_receipts WHERE consumer_id=?1"
);

scalar!(output_deliveries_count() -> i64, "SELECT COUNT(*) FROM output_deliveries");

scalar!(output_deliveries_count_with_attempts() -> i64, "SELECT COUNT(*) FROM output_deliveries WHERE attempts=1");

scalar!(output_deliveries_m_decision_id() -> String, "SELECT m.decision_id FROM output_deliveries d JOIN chan_messages m ON m.id=d.message_id");

scalar!(output_deliveries_m_status() -> String, "SELECT m.status FROM output_deliveries d JOIN chan_messages m ON m.id=d.message_id");

scalar!(parties_count() -> i64, "SELECT COUNT(*) FROM parties");

scalar!(parties_id_dev_local_for_selected_row() -> String, "SELECT id FROM parties WHERE user_id=(SELECT id FROM users WHERE subject='dev:local')");

scalar!(personal_notifications_count() -> i64, "SELECT COUNT(*) FROM personal_notifications");

scalar!(personal_notifications_count_with_value() -> i64, "SELECT COUNT(*) FROM personal_notifications WHERE delivery_id IS NOT NULL");

scalar!(
    users_id_by_subject(subject: &str) -> String,
    "SELECT id FROM users WHERE subject=?1"
);

scalar!(workspaces_id() -> String, "SELECT id FROM workspaces LIMIT 1");

pub fn seed_finish_item(path: &Path, bot: &str) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO items(id, workspace_id, no, title, phase, bot_id, created, updated) VALUES ('ST-90', 'default', 90, '写周报', 'active', ?, 1, 1)", [bot])?;
    Ok(())
}

pub fn seed_health_session(
    path: &Path,
    id: &str,
    ws: &str,
    status: &str,
    readonly: i64,
) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO sessions (id, workspace_id, harness, cwd, status, readonly, created, updated) VALUES (?, ?, 'claude', '', ?, ?, 0, 0)", rusqlite::params![id, ws, status, readonly])?;
    Ok(())
}

pub fn seed_health_queue(path: &Path, id: &str) -> Result<()> {
    super::api::writable(path)?.execute(
        "INSERT INTO queued (id, session_id, text, via, created) VALUES (?, ?, 'x', 'web', 0)",
        [&format!("q-{id}"), id],
    )?;
    Ok(())
}

pub fn seed_health_binding(path: &Path, id: &str, node: &str, phase: &str) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO runtime_sessions (session_id, node_id, workspace_ref, node_session_id, config_revision, phase, updated) VALUES (?, ?, 'w', ?, 'r', ?, 0)", [id, node, id, phase])?;
    Ok(())
}

pub fn seed_unknown_turn(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO runtime_turns (turn_id, session_id, state, created, updated) VALUES ('t1', 'hz-remote', 'unknown', 0, 0)", [])?;
    Ok(())
}

pub fn seed_unknown_attempt(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO runtime_attempts (attempt_id, turn_id, ordinal, node_id, operation_id, execution, spec, digest, state, created, updated) VALUES ('a1', 't1', 0, 'remote-1', 'op1', 'e', '{}', 'd', 'unknown', 0, 0)", [])?;
    Ok(())
}

pub fn set_lark_connection(path: &Path, connection: &str) -> Result<()> {
    super::api::writable(path)?.execute("UPDATE connections SET adapter = 'feishu_app', config = '{\"domain\":\"lark\"}' WHERE id = ?", [connection])?;
    Ok(())
}

pub fn seed_chat_space(path: &Path, connection: &str) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, member_count, has_external, created, updated)
         VALUES ('sp_open', 'default', 'feishu', ?1, 'group', 'oc_0000test0002', '测试群', 2, 0, 1, 1)", [connection])?;
    Ok(())
}

pub fn seed_notification_item(path: &Path, bot: &str) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO items(id, workspace_id, no, title, phase, bot_id, created, updated) VALUES ('ST-91','default',91,'Real completion notification','active',?,1,1)", [bot])?;
    Ok(())
}

pub fn make_reply_due(path: &Path, card: &str) -> Result<()> {
    super::api::writable(path)?.execute(
        "UPDATE decisions SET result=json_set(result,'$.send_at',0) WHERE id=?",
        [card],
    )?;
    Ok(())
}

pub fn seed_notification_workspace(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute(
        "INSERT INTO workspaces(id,name,created) VALUES('notification-empty','Empty',1)",
        [],
    )?;
    Ok(())
}

pub fn seed_notification_member(path: &Path, owner: &str) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('notification-empty',?,'owner',1)", [owner])?;
    Ok(())
}

pub fn detach_party_user(path: &Path, party: &str) -> Result<()> {
    super::api::writable(path)?.execute("UPDATE parties SET user_id=NULL WHERE id=?", [party])?;
    Ok(())
}

pub fn remove_notification_member(path: &Path, user: &str) -> Result<()> {
    super::api::writable(path)?.execute(
        "DELETE FROM workspace_members WHERE workspace_id='default' AND user_id=?",
        [user],
    )?;
    Ok(())
}

pub fn seed_orphan_draft(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute_batch("PRAGMA foreign_keys = OFF;
         INSERT INTO chan_messages (id, workspace_id, space_id, direction, text, status, decision_id, created)
         VALUES ('cm_d1', 'default', 'sp_x', 'out', '要我处理吗？', 'draft', 'dc_d1', 1);
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, options, status, requested_by, created)
         VALUES ('dc_d1', 'default', 'approve', 'chan_message', 'cm_d1', '以机器人发到「群」', '[\"yes\",\"no\"]', 'open', 'steve', 1);")?;
    Ok(())
}

pub fn seed_chat_decisions(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute_batch("INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, status, decision_id, created)
         VALUES ('cm_open', 'default', 'sp_open', 'om_0000test0002', 'in', '看一下', 'received', 'dc_open', 1);
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, options, status, requested_by, created)
         VALUES ('dc_open', 'default', 'ask', 'chan_message', 'cm_open', '「测试群」里 有人 找你：看一下', '[\"reply\",\"ignore\"]', 'open', 'steve', 1),
                ('dc_other', 'default', 'ask', 'space', 'sp_open', '新群', '[\"open\"]', 'open', 'steve', 1);")?;
    Ok(())
}

pub fn seed_dead_deliveries(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute_batch("INSERT INTO outbox_events (id, workspace_id, topic, version, payload, created) VALUES
           ('evt_d1', 'default', 'test.topic', 1, '{}', 1), ('evt_d2', 'default', 'test.topic', 1, '{}', 2);
         INSERT INTO outbox_leases (event_seq, consumer_id, attempts, last_error, next_at, dead, updated)
           SELECT seq, 'test.gone', 3, 'GitHub 503', 0, 1, created FROM outbox_events WHERE id IN ('evt_d1', 'evt_d2');")?;
    Ok(())
}

pub fn notification_party(store: &crate::Store, ws: &str, user: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT id FROM parties WHERE workspace_id=? AND user_id=?",
            [ws, user],
            |r| r.get(0),
        )?)
    })
}

pub fn seed_notice_workspace(store: &crate::Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1)",
            [],
        )?;
        Ok(())
    })
}

/// Seeds three open decisions for maintenance_gate tests:
/// - `dc_running_perm`: waiting permission card on a currently running Node turn/attempt
/// - `dc_finished_perm`: waiting permission card whose Node turn/attempt has already finished
/// - `dc_plain_ask`: ordinary non-permission decision
pub fn seed_maintenance_decision_cases(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute_batch(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, options, status, requested_by, created) VALUES
           ('dc_running_perm', 'default', 'approve', 'session', 'sess_run', 'allow bash?', '[\"allow_once\",\"deny\"]', 'open', 'steve', 1),
           ('dc_finished_perm', 'default', 'approve', 'session', 'sess_fin', 'stale perm?', '[\"allow_once\",\"deny\"]', 'open', 'steve', 1),
           ('dc_plain_ask', 'default', 'ask', 'item', 'item_1', 'proceed?', '[\"yes\",\"no\"]', 'open', 'steve', 1);
         INSERT INTO runtime_sessions (session_id, backend, node_id, workspace_ref, node_session_id, config_revision, phase, revision, updated) VALUES
           ('sess_run', 'node', 'local', 'local/w1', 'ns_run', 'r1', 'ready', 1, 1),
           ('sess_fin', 'node', 'local', 'local/w2', 'ns_fin', 'r1', 'ready', 1, 1);
         INSERT INTO runtime_turns (turn_id, session_id, state, active_attempt, retry_ordinal, revision, applied, created, updated) VALUES
           ('turn_run', 'sess_run', 'running', 'att_run', 0, 1, 0, 1, 1),
           ('turn_fin', 'sess_fin', 'finished', 'att_fin', 0, 1, 1, 1, 1);
         INSERT INTO runtime_attempts (attempt_id, turn_id, ordinal, node_id, operation_id, execution, spec, digest, state, cancel_requested, revision, created, updated) VALUES
           ('att_run', 'turn_run', 0, 'local', 'op_run', 'turn', '{}', 'd1', 'running', 0, 1, 1, 1),
           ('att_fin', 'turn_fin', 0, 'local', 'op_fin', 'turn', '{}', 'd2', 'finished', 0, 1, 1, 1);
         INSERT INTO runtime_permissions (decision_id, node_id, session_id, attempt_id, permission_id, request, expires_at, state, revision, created, updated) VALUES
           ('dc_running_perm', 'local', 'sess_run', 'att_run', 'perm_run', '{}', 9999999999999, 'waiting', 1, 1, 1),
           ('dc_finished_perm', 'local', 'sess_fin', 'att_fin', 'perm_fin', '{}', 9999999999999, 'waiting', 1, 1, 1);",
    )?;
    Ok(())
}
