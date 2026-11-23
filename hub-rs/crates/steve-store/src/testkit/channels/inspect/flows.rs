use super::*;

pub fn flow_kind_enabled_name_origin_label_by_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT kind, enabled, name, origin_label, graph FROM flows WHERE id = ?"#,
        [id],
    )
}

pub fn flow_trigger_kind_spaces_enabled_by_flow(
    store: &Store,
    flow_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT kind, spaces, enabled FROM flow_triggers WHERE flow_id = ?"#,
        [flow_id],
    )
}

pub fn count_flow_runs_by_id(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM flow_runs r JOIN chan_messages m ON m.event_id = r.event_id WHERE m.id = ?"#,
        [id],
    )
}

pub fn flow_enabled_deleted_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT enabled, deleted FROM flows WHERE id = ?"#,
        [id],
    )
}

pub fn count_flow_runs_by_flow_and_id(store: &Store, flow_id: &str, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM flow_runs r JOIN chan_messages m ON m.event_id = r.event_id WHERE r.flow_id = ? AND m.id = ?"#,
        [flow_id, id],
    )
}
