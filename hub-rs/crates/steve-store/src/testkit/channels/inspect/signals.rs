use super::*;

pub fn signal_outcome_label_by_external(
    store: &Store,
    external_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT e.outcome_label FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.external_id = ? AND e.outcome_label IS NOT NULL"#,
        [external_id],
    )
}

pub fn signal_outcome_outcome_label(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT outcome, outcome_label FROM ext_events WHERE id = ?"#,
        [id],
    )
}

pub fn signal_outcome(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT outcome FROM ext_events WHERE id = ?"#,
        [id],
    )
}

pub fn signal_outcome_outcome_label_reason(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT e.outcome, e.outcome_label, e.reason FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"#,
        [id],
    )
}

pub fn signal_source_kind_space_id_message_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT source, type, space_id, message_id, actor_party_id, connection_id, summary, outcome_label FROM ext_events WHERE id = ?"#,
        [id],
    )
}

pub fn signal_outcome_work_item_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT e.outcome, e.work_item_id FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"#,
        [id],
    )
}

pub fn signal_kind_source_space_id_summary_by_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT type, source, space_id, summary FROM ext_events WHERE id = ?"#,
        [id],
    )
}

pub fn signal_outcome_outcome_label_reason_by_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT e.outcome, e.outcome_label, e.reason FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"#,
        [id],
    )
}

pub fn signal_outcome_outcome_label_decision_id_reason_by_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT e.outcome, e.outcome_label, e.decision_id, e.reason FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"#,
        [id],
    )
}

pub fn signal_payload_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT e.payload FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"#,
        [id],
    )
}

pub fn count_ext_events(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM ext_events"#, [])
}

pub fn signal_id_payload(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT id,payload FROM ext_events WHERE source='connector'"#,
        [],
    )
}

pub fn signal_outcome_label(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT e.outcome_label FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"#,
        [id],
    )
}

pub fn signal_outcome_outcome_label_single_row(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT outcome, outcome_label FROM ext_events WHERE id = ?"#,
        [id],
    )
}

pub fn signal_outcome_label_single_row(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT outcome_label FROM ext_events WHERE id = ?"#,
        [id],
    )
}

pub fn signal_kind(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(store, r#"SELECT type FROM ext_events WHERE id = ?"#, [id])
}

pub fn signal_visibility(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT visibility FROM ext_events WHERE id = ?"#,
        [id],
    )
}

pub fn signal_kind_by_at_1(store: &Store, at_1: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT type FROM ext_events WHERE ?1 = ?1"#,
        [at_1],
    )
}

pub fn signal_outcome_outcome_label_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT e.outcome, e.outcome_label FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"#,
        [id],
    )
}

pub fn count_ext_events_by_outcome_label(store: &Store, outcome_label: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM ext_events WHERE source = 'im' AND outcome = 'recorded' AND outcome_label = ?"#,
        [outcome_label],
    )
}

pub fn count_ext_events_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM ext_events WHERE source = 'im' AND outcome <> 'recorded'"#,
        [],
    )
}

pub fn count_ext_events_count_3(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM ext_events WHERE source = 'im'"#,
        [],
    )
}

pub fn count_ext_events_by_x1(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM ext_events WHERE external_id = 'x1'"#,
        [],
    )
}

pub fn count_ext_events_by_ev1(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM ext_events WHERE id = 'ev1' AND payload IS NULL AND payload_purged IS NOT NULL AND summary <> ''"#,
        [],
    )
}

pub fn count_ext_events_by_ev1_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM ext_events WHERE id = 'ev1'"#,
        [],
    )
}

pub fn count_ext_events_count_4(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM ext_events WHERE decision_id IN ('d1','d2')"#,
        [],
    )
}

pub fn message_outcome(store: &Store, message: &str) -> Result<(Value, Value, Value, Value)> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT e.outcome, e.outcome_label, e.reason, e.decision_id
         FROM ext_events e JOIN chan_messages m ON m.event_id=e.id WHERE m.id=?",
            [message],
            |r| {
                Ok((
                    read_cell(r, 0)?,
                    read_cell(r, 1)?,
                    read_cell(r, 2)?,
                    read_cell(r, 3)?,
                ))
            },
        )?)
    })
}
