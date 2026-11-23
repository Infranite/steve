use super::*;

pub fn message_sender_account_id_outcome_label_by_external(
    store: &Store,
    external_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.sender_account_id, e.outcome_label FROM chan_messages m JOIN ext_events e ON e.id = m.event_id WHERE m.external_id = ? AND e.outcome_label IS NOT NULL"#,
        [external_id],
    )
}

pub fn message_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_decision_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT decision_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_space_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT space_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_composed(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT composed FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_text(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT text FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_count_by_draft(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT count(*) FROM chan_messages m JOIN reply_basis b ON b.id = m.basis_id
             WHERE m.status = 'draft' AND b.verdict = 'checking'"#,
        [],
    )
}

pub fn message_status_external_id_persona_id_on_behalf(
    store: &Store,
    id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, external_id, persona_id, on_behalf, text, sent IS NOT NULL FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_on_behalf_persona_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT on_behalf, persona_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_text_reply_to(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT text, reply_to FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_thread_key(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT thread_key FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_count_by_outgoing(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out'"#,
        [],
    )
}

pub fn message_external_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT external_id FROM chan_messages WHERE id=?"#,
        [id],
    )
}

pub fn message_basis_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT basis_id FROM chan_messages WHERE id=?"#,
        [id],
    )
}

pub fn message_direction_text_status_sender_party_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT direction, text, status, sender_party_id, event_id, external_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_mentions(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT mentions FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_status_text_persona_id_reply_to(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, text, persona_id, reply_to, decision_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_status_text_on_behalf_persona_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, text, on_behalf, persona_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_status_text(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, text FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_status_text_reply_to(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, text, reply_to FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_reply_to_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT reply_to, status FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_text_persona_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT text, persona_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_space_id_reply_to_text_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT space_id, reply_to, text, status FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_attachments(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT attachments FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_sender_account_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT sender_account_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_created(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT created FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_space_id_by_ids(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT space_id FROM chan_messages WHERE id = (SELECT id FROM chan_messages WHERE text = '先放一放')"#,
        [],
    )
}

pub fn message_space_id_row(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT space_id FROM chan_messages WHERE text = '看情况吧'"#,
        [],
    )
}

pub fn message_created_row(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT created FROM chan_messages WHERE text = '等它再跑跑'"#,
        [],
    )
}

pub fn chan_reaction_state_by_message(store: &Store, message_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT state FROM chan_reactions WHERE message_id = ?"#,
        [message_id],
    )
}

pub fn message_decision_id_value(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT decision_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_sender_party_id_by_external(store: &Store, external_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT sender_party_id FROM chan_messages WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn message_space_id_value(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT space_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_id_by_external(store: &Store, external_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT id FROM chan_messages WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn message_status_value(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT status FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_text_handle_display_name_system_id_by_external(
    store: &Store,
    external_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.text, a.handle, a.display_name, a.system_id, m.mentions, m.thread_key, m.event_id FROM chan_messages m
           JOIN accounts a ON a.id = m.sender_account_id WHERE m.external_id = ?"#,
        [external_id],
    )
}

pub fn message_text_seen_by_by_incoming(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT text, seen_by FROM chan_messages WHERE direction = 'in' ORDER BY created, rowid"#,
        [],
    )
}

pub fn message_external_id_by_incoming(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT external_id FROM chan_messages WHERE direction = 'in' ORDER BY created, rowid"#,
        [],
    )
}

pub fn message_direction_status_on_behalf_persona_id_by_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT direction, status, on_behalf, persona_id, reply_to, decision_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_on_behalf_name_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.on_behalf, p.name FROM chan_messages m JOIN personas p ON p.id = m.persona_id WHERE m.id = ?"#,
        [id],
    )
}

pub fn message_count_by_outgoing_rows(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND reply_to IN ('n1', 'n2')"#,
        [],
    )
}

pub fn count_chan_messages_by_space_and_outgoing(store: &Store, space_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE space_id = ? AND direction = 'out'"#,
        [space_id],
    )
}

pub fn count_chan_messages_by_outgoing_and_draft(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND status = 'draft'"#,
        [],
    )
}

pub fn count_chan_messages_by_outgoing(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out'"#,
        [],
    )
}

pub fn count_chan_messages(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM chan_messages"#, [])
}

pub fn count_chan_messages_by_space(store: &Store, space_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE space_id = ?"#,
        [space_id],
    )
}

pub fn incoming_with_signal(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'in' AND event_id IS NOT NULL"#,
        [],
    )
}

pub fn message_text_history_coalescee_by_external(
    store: &Store,
    external_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.text, m.history, coalesce(e.outcome_label, '') FROM chan_messages m
               JOIN spaces s ON s.id = m.space_id LEFT JOIN ext_events e ON e.id = m.event_id
              WHERE s.external_id = ? ORDER BY m.created, m.rowid"#,
        [external_id],
    )
}

pub fn message_id_attachments_kind_thread_key_by_pr_7(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.id,m.attachments,s.kind,m.thread_key,m.reply_to FROM chan_messages m JOIN spaces s ON s.id=m.space_id WHERE s.external_id='pr-7'"#,
        [],
    )
}

pub fn message_id_external_id_source_id_by_connection(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.id,m.external_id,x.source_id FROM chan_messages m JOIN chan_message_seen x ON x.message_id=m.id WHERE x.connection_id=? ORDER BY m.created,m.id"#,
        [connection_id],
    )
}

pub fn message_id_space_id_by_issue_9(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.id,m.space_id FROM chan_messages m JOIN spaces s ON s.id=m.space_id WHERE s.external_id='issue-9'"#,
        [],
    )
}

pub fn message_id_by_ro_chat_and_incoming_and_text(
    store: &Store,
    text: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.id FROM chan_messages m JOIN spaces s ON s.id=m.space_id
                  WHERE s.external_id='ro_chat' AND m.direction='in' AND m.text=?"#,
        [text],
    )
}

pub fn message_id_space_id_by_bot_chat(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT m.id,m.space_id FROM chan_messages m JOIN spaces s ON s.id=m.space_id
        WHERE s.external_id='bot_chat'"#,
        [],
    )
}

pub fn seen_message_source_id_by_connection_and_message(
    store: &Store,
    connection_id: &str,
    message_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT source_id FROM chan_message_seen WHERE connection_id=? AND message_id=?"#,
        [connection_id, message_id],
    )
}

pub fn message_on_behalf(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT on_behalf FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_sent_via_ext_created(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT sent_via, ext_created FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_external_id_sent_via_ext_created_thread_key(
    store: &Store,
    id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT external_id, sent_via, ext_created, thread_key FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn seen_message_connection_id_source_id_by_message(
    store: &Store,
    message_id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT connection_id, source_id FROM chan_message_seen WHERE message_id = ?"#,
        [message_id],
    )
}

pub fn message_external_id_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT external_id, status FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_status_by_outgoing(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status FROM chan_messages WHERE direction='out'"#,
        [],
    )
}

pub fn message_text_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT text, status FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn chan_reaction_answer_id_by_message(store: &Store, message_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT answer_id FROM chan_reactions WHERE message_id = ?"#,
        [message_id],
    )
}

pub fn chan_reaction_error_by_message(store: &Store, message_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT error FROM chan_reactions WHERE message_id = ?"#,
        [message_id],
    )
}

pub fn message_count_by_outgoing_value(store: &Store) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out'"#,
        [],
    )
}

pub fn chan_reaction_count_by_message(store: &Store, message_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM chan_reactions WHERE message_id = ?"#,
        [message_id],
    )
}

pub fn message_work_item_id(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT work_item_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_history(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT history FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_thread_key_single_row(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT thread_key FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_text_purged(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT text, purged FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_text_single_row(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT text FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_work_item_id_row(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT work_item_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_decision_id_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT decision_id FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn message_thread_key_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT thread_key FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn count_chan_messages_by_id(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE id = ?"#,
        [id],
    )
}

pub fn count_chan_messages_by_pending_and_outgoing(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE status = 'pending' AND direction = 'out'"#,
        [],
    )
}

pub fn count_chan_messages_by_space_and_o2(store: &Store, space_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE space_id = ? AND external_id = 'o2'"#,
        [space_id],
    )
}

pub fn count_chan_messages_by_outgoing_and_pending(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction='out' AND status='pending'"#,
        [],
    )
}

pub fn count_chan_messages_by_outgoing_count_2(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction='out' AND sent IS NOT NULL"#,
        [],
    )
}

pub fn count_chan_messages_by_incoming(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'in' AND history = 'late'"#,
        [],
    )
}

pub fn count_chan_messages_by_outgoing_and_space(store: &Store, space_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND space_id = ?"#,
        [space_id],
    )
}

pub fn count_chan_messages_by_outgoing_count_3(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND text LIKE '%青蛙%'"#,
        [],
    )
}

pub fn count_chan_messages_by_outgoing_count_4(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND (text LIKE '%青蛙%' OR text LIKE '%松鼠%')"#,
        [],
    )
}

pub fn count_chan_messages_by_external_id_s1_s2(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE external_id IN ('s1', 's2')"#,
        [],
    )
}

pub fn count_chan_messages_by_outgoing_and_status_draft(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND status NOT IN ('draft', 'discarded')"#,
        [],
    )
}

pub fn count_chan_messages_by_outgoing_and_status_pending_sent(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND status IN ('pending', 'sent')"#,
        [],
    )
}

pub fn count_chan_messages_by_outgoing_and_sent(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE direction = 'out' AND status = 'sent'"#,
        [],
    )
}

pub fn count_chan_messages_by_x1(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE external_id = 'x1'"#,
        [],
    )
}

pub fn message_id(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(store, r#"SELECT id FROM chan_messages"#, [])
}

pub fn message_status_decision_id_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT status, decision_id FROM chan_messages WHERE id=?"#,
        [id],
    )
}

pub fn message_status_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT status FROM chan_messages WHERE id=?"#,
        [id],
    )
}

pub fn message_count_by_space(store: &Store, space_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE space_id=?"#,
        [space_id],
    )
}

pub fn count_chan_messages_by_text_null(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE text IS NULL"#,
        [],
    )
}

pub fn count_chan_messages_by_text_null_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE text IS NULL AND attachments = '[]' AND purged IS NOT NULL"#,
        [],
    )
}

pub fn count_chan_messages_count_2(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE space_id = 'sp_a'"#,
        [],
    )
}

pub fn count_chan_messages_by_m4_and_text_null(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE id = 'm4' AND text IS NULL"#,
        [],
    )
}

pub fn count_chan_messages_count_3(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE decision_id IN ('d1','d2')"#,
        [],
    )
}

pub fn count_chan_messages_by_m1(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM chan_messages WHERE id='m1'"#,
        [],
    )
}

pub fn message_text_by_m3(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT text FROM chan_messages WHERE id = 'm3'"#,
        [],
    )
}

pub fn message_decision_id_by_m1(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT decision_id FROM chan_messages WHERE id='m1'"#,
        [],
    )
}

pub fn incoming_count(store: &Store) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM chan_messages WHERE direction = 'in'",
        [],
    )
}

pub fn outgoing_count(store: &Store) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM chan_messages WHERE direction = 'out'",
        [],
    )
}

pub fn contract_messages_with_status(store: &Store, status: &str) -> Result<Vec<String>> {
    store.sqlite_read(|c| {
        let mut st =
            c.prepare("SELECT id FROM chan_messages WHERE id LIKE 'contract-msg-%' AND status=?")?;
        Ok::<_, crate::StoreError>(
            st.query_map([status], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        )
    })
}

pub fn raw_attachments(store: &Store, message: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok::<_, crate::StoreError>(c.query_row(
            "SELECT attachments FROM chan_messages WHERE id = ?",
            [message],
            |r| r.get(0),
        )?)
    })
}

pub fn outgoing_texts(store: &Store) -> Result<Vec<String>> {
    store.write_locked(|c| {
            let mut st = c.prepare(
                "SELECT coalesce(text, '') FROM chan_messages WHERE direction = 'out' ORDER BY created, rowid",
            )?;
            let v = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
}

pub fn received_message(store: &Store, external: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    store.sqlite_read(|c| {
            Ok::<_, crate::StoreError>(
                c.query_row(
                    "SELECT id FROM chan_messages WHERE external_id = ? AND direction = 'in' AND event_id IS NOT NULL",
                    [external],
                    |r| r.get(0),
                )
                .optional()?,
            )
        })
}

pub fn drafts_in_space(store: &Store, space: &str) -> Result<i64> {
    store.read_conn(|c| {
        Ok(c.query_row("SELECT count(*) FROM chan_messages WHERE space_id=? AND direction='out' AND status='draft'", [space], |r| r.get(0))?)
    })
}
