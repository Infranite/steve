use super::*;

pub fn audit_count_by_chan_message_send_orphaned_and_target(
    store: &Store,
    target: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT count(*) FROM audit WHERE action = 'chan_message.send.orphaned' AND target = ?"#,
        [target],
    )
}

pub fn audit_count(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT (SELECT count(*) FROM audit),(SELECT count(*) FROM notification_policies),(SELECT count(*) FROM output_deliveries),(SELECT count(*) FROM parties),(SELECT count(*) FROM chan_messages)"#,
        [],
    )
}

pub fn audit_target_detail_by_chan_message_nothing_left(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT target, detail FROM audit WHERE action = 'chan_message.nothing_left'"#,
        [],
    )
}

pub fn count_audit_by_chan_message_send_refused(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM audit WHERE action = 'chan_message.send.refused'"#,
        [],
    )
}

pub fn count_audit_by_chan_message_send_and_target(store: &Store, target: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM audit WHERE action = 'chan_message.send' AND target = ?"#,
        [target],
    )
}

pub fn count_audit_by_space_patch(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM audit WHERE action = 'space.patch'"#,
        [],
    )
}

pub fn count_audit_by_target_and_action_space_to_flow_space_flow_off(
    store: &Store,
    target: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM audit WHERE target = ? AND action IN ('space.to_flow', 'space.flow_off')"#,
        [target],
    )
}

pub fn audit_action_by_target_and_channel_dead_letter(
    store: &Store,
    target: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT action FROM audit WHERE target = ? AND action = 'channel.dead_letter'"#,
        [target],
    )
}

pub fn count_audit_by_channel_cleanup(store: &Store, snapshot: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM audit WHERE action = 'channel.cleanup' AND json_extract(detail, '$.backup') = ?"#,
        [snapshot],
    )
}

pub fn count_audit_by_channel_create_and_target(store: &Store, target: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM audit WHERE action = 'channel.create' AND target = ?"#,
        [target],
    )
}

pub fn count_audit_by_channel_patch_and_target(store: &Store, target: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM audit WHERE action = 'channel.patch' AND target = ?"#,
        [target],
    )
}
