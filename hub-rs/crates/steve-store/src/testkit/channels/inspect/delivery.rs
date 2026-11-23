use super::*;

pub fn output_delivery_intent(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT d.intent FROM output_deliveries d JOIN personal_notifications p ON p.delivery_id = d.id WHERE p.topic = 'contact.starred'"#,
        [],
    )
}

pub fn output_delivery_message_id(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT d.message_id FROM output_deliveries d JOIN personal_notifications p ON p.delivery_id = d.id WHERE p.topic = 'contact.starred'"#,
        [],
    )
}

pub fn output_delivery_source_message_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT source_message_id FROM output_deliveries WHERE id=?"#,
        [id],
    )
}

pub fn output_delivery_content(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT json_extract(intent,'$.content') FROM output_deliveries WHERE id=?"#,
        [id],
    )
}

pub fn output_delivery_space_id(store: &Store) -> Result<Vec<Value>> {
    row(store, r#"SELECT space_id FROM output_deliveries"#, [])
}

pub fn count_output_deliveries(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM output_deliveries"#, [])
}

pub fn count_output_deliveries_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_deliveries WHERE attempts=1"#,
        [],
    )
}

pub fn count_output_deliveries_count_2(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT sum(attempts) FROM output_deliveries"#, [])
}

pub fn count_output_deliveries_by_sent(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_deliveries d JOIN chan_messages m ON m.id=d.message_id WHERE d.idempotency_key='ordinary-after-50' AND m.status='sent'"#,
        [],
    )
}

pub fn count_chan_send_state_by_message(store: &Store, message_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_send_state WHERE message_id=? AND first_at IS NOT NULL"#,
        [message_id],
    )
}

pub fn count_output_deliveries_count_3(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_deliveries WHERE intent LIKE '%青蛙%'"#,
        [],
    )
}

pub fn count_output_deliveries_count_4(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_deliveries WHERE intent LIKE '%青蛙%' OR intent LIKE '%松鼠%'"#,
        [],
    )
}

pub fn count_output_deliveries_by_output_delete(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_deliveries WHERE id='output-delete' AND replied_at=7 AND reply_message_id='m1'"#,
        [],
    )
}

pub fn count_output_deliveries_by_output_delete_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_deliveries WHERE id='output-delete'"#,
        [],
    )
}

pub fn count_output_deliveries_by_output_keep(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM output_deliveries WHERE id='output-keep' AND reply_message_id IS NULL AND replied_at=7"#,
        [],
    )
}

pub fn output_delivery_content_by_output_delete(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT json_extract(intent,'$.content') FROM output_deliveries WHERE id='output-delete'"#,
        [],
    )
}

pub fn output_delivery_fingerprint_by_output_delete(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT fingerprint FROM output_deliveries WHERE id='output-delete'"#,
        [],
    )
}

pub fn output_delivery_markdown_by_output_keep(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT json_extract(intent,'$.content.markdown') FROM output_deliveries WHERE id='output-keep'"#,
        [],
    )
}

pub fn checking_basis_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| {
        c.query_row(
            "SELECT count(*) FROM chan_messages m JOIN reply_basis b ON b.id = m.basis_id
                      WHERE m.status = 'draft' AND b.verdict = 'checking'",
            [],
            |r| r.get(0),
        )
        .map_err(crate::StoreError::from)
    })
}

pub fn decision_basis_is_checking(store: &Store, decision: &str) -> Result<bool> {
    store.sqlite_read(|c| {
            Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM chan_messages m JOIN reply_basis b ON b.id=m.basis_id WHERE m.decision_id=? AND b.verdict='checking')",[decision],|r|r.get(0))?)
        })
}
