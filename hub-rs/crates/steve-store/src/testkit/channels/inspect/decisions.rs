use super::*;

pub fn decision_id_by_subject_and_fail_and_open(
    store: &Store,
    subject_id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM decisions WHERE subject_id = ? AND kind = 'fail' AND status = 'open'"#,
        [subject_id],
    )
}

pub fn decision_status_result(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, result FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_status_question_draft_options(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, question, draft, options, result FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_count_by_chan_message_and_ask_and_open(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'open'"#,
        [],
    )
}

pub fn decision_composed(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT coalesce(json_extract(result, '$.composed'), '') FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_question(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT question FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_question_draft_options(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT question, draft, options FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_kind_question_options(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT kind, question, options FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_question_draft(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT question, draft FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_question_options(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT question, options FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_kind_question_draft(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT kind, question, draft FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_draft_options(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT draft, options FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_question_draft_options_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT question, draft, options, status FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_draft_options_question(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT draft, options, question FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_options(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(store, r#"SELECT options FROM decisions WHERE id = ?"#, [id])
}

pub fn decision_question_options_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT question, options, status FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_question_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT question, status FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_item(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT json_extract(result,'$.item') FROM decisions WHERE id=?"#,
        [id],
    )
}

pub fn decision_kind_subject_type_subject_id_question(
    store: &Store,
    id: &str,
) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT kind, subject_type, subject_id, question, options, status, item_id FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_kind_subject_type_question_draft(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT kind, subject_type, question, draft FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_kind_question(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT kind, question FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_count_by_subject_and_approve(store: &Store, subject_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_id = ? AND kind = 'approve'"#,
        [subject_id],
    )
}

pub fn decision_id_by_subject_and_open(store: &Store, subject_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT id FROM decisions WHERE subject_id = ? AND status = 'open'"#,
        [subject_id],
    )
}

pub fn decision_question_by_space_and_subject(
    store: &Store,
    subject_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT question FROM decisions WHERE subject_type = 'space' AND subject_id = ?"#,
        [subject_id],
    )
}

pub fn decision_kind_subject_type_question_options_by_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT kind, subject_type, question, options, draft FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn count_decisions_by_workspace(store: &Store, workspace_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE workspace_id = ?"#,
        [workspace_id],
    )
}

pub fn count_decisions_by_approve(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE kind = 'approve'"#,
        [],
    )
}

pub fn count_decisions_by_open(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE status = 'open'"#,
        [],
    )
}

pub fn decision_options_question_by_connection_and_fail(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT d.options, d.question FROM decisions d JOIN chan_messages m ON m.id = d.subject_id
              JOIN spaces s ON s.id = m.space_id WHERE s.connection_id = ? AND d.kind = 'fail'"#,
        [connection_id],
    )
}

pub fn decision_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(store, r#"SELECT status FROM decisions WHERE id = ?"#, [id])
}

pub fn decision_draft(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(store, r#"SELECT draft FROM decisions WHERE id=?"#, [id])
}

pub fn decision_count_by_id_dbm_and_open(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT count(*) FROM decisions WHERE id LIKE 'dbm%' AND status = 'open'"#,
        [],
    )
}

pub fn decision_count_by_connection_and_subject(
    store: &Store,
    subject_id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT COUNT(*) FROM decisions WHERE subject_type = 'connection' AND subject_id = ?"#,
        [subject_id],
    )
}

pub fn decision_count_by_open_and_approve(store: &Store) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE status = 'open' AND kind = 'approve'"#,
        [],
    )
}

pub fn decision_status_value(store: &Store, id: &str) -> Result<Value> {
    json_cell(store, r#"SELECT status FROM decisions WHERE id = ?"#, [id])
}

pub fn decision_question_value(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT question FROM decisions WHERE id = ?"#,
        [id],
    )
}

pub fn decision_kind_by_subject_and_open(store: &Store, subject_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT kind FROM decisions WHERE subject_id = ? AND status = 'open'"#,
        [subject_id],
    )
}

pub fn decision_draft_value(store: &Store, id: &str) -> Result<Value> {
    json_cell(store, r#"SELECT draft FROM decisions WHERE id = ?"#, [id])
}

pub fn decision_count_by_chan_message_and_subject_and_ask(
    store: &Store,
    subject_id: &str,
) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type='chan_message' AND subject_id=? AND kind='ask'"#,
        [subject_id],
    )
}

pub fn decision_count_by_chan_message_and_approve(store: &Store) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type='chan_message' AND kind='approve'"#,
        [],
    )
}

pub fn decision_count_by_approve_and_chan_message(store: &Store) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE kind='approve' AND subject_type='chan_message'"#,
        [],
    )
}

pub fn decision_kind(store: &Store, id: &str) -> Result<Value> {
    json_cell(store, r#"SELECT kind FROM decisions WHERE id=?"#, [id])
}

pub fn decision_count_by_subject_and_fail(store: &Store, subject_id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_id = ? AND kind = 'fail'"#,
        [subject_id],
    )
}

pub fn count_decisions_by_subject_and_open(store: &Store, subject_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_id = ? AND status = 'open'"#,
        [subject_id],
    )
}

pub fn count_decisions_by_workspace_and_subject_type_party_merge(
    store: &Store,
    workspace_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE workspace_id = ? AND subject_type <> 'party_merge'"#,
        [workspace_id],
    )
}

pub fn count_decisions_by_party_merge_and_open(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type = 'party_merge' AND status = 'open'"#,
        [],
    )
}

pub fn count_decisions_by_space(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type = 'space'"#,
        [],
    )
}

pub fn count_decisions_by_space_and_at_1(store: &Store, at_1: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type = 'space' AND ?1 = ?1"#,
        [at_1],
    )
}

pub fn decision_question_options_by_space_and_subject(
    store: &Store,
    subject_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT question, options FROM decisions WHERE subject_type = 'space' AND subject_id = ?"#,
        [subject_id],
    )
}

pub fn count_decisions_by_ids(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE id IN ('d1','d2')"#,
        [],
    )
}

pub fn count_decisions_by_shared(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE id='shared'"#,
        [],
    )
}

pub fn count_decisions_by_ids_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE id IN ('foreign','item_decision')"#,
        [],
    )
}

pub fn count_yolo_actions_by_ya1(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM yolo_actions WHERE id='ya1' AND decision_id='d1' AND undo IS NOT NULL"#,
        [],
    )
}

pub fn count_yolo_actions_by_ids(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM yolo_actions WHERE id IN ('ya1','ya2','ya3')"#,
        [],
    )
}

pub fn count_yolo_actions_by_ids_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM yolo_actions WHERE id IN ('ya1','ya2','ya3') AND (decision_id IS NOT NULL OR undo IS NOT NULL)"#,
        [],
    )
}

pub fn count_decisions_by_d1(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM decisions WHERE id='d1'"#, [])
}

pub fn count_decisions_by_id_and_expired(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE id = ? AND status = 'expired' AND json_extract(result, '$.reason') = 'disbanded'"#,
        [id],
    )
}

pub fn count_decisions_by_space_and_workspace(store: &Store, workspace_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type = 'space' AND workspace_id = ?"#,
        [workspace_id],
    )
}

pub fn count_decisions_by_id(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE id = ? AND options = '["open","mute","quiet","block"]'"#,
        [id],
    )
}

pub fn count_decisions_by_id_and_expired_count(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE id = ? AND status = 'expired'"#,
        [id],
    )
}

pub fn count_decisions_by_space_and_open_and_subject(
    store: &Store,
    subject_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM decisions WHERE subject_type = 'space' AND status = 'open' AND subject_id = ?"#,
        [subject_id],
    )
}

pub fn yolo_action_reason_by_ya1(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT reason FROM yolo_actions WHERE id='ya1'"#,
        [],
    )
}

pub fn yolo_action_decision_id_by_ya1(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT decision_id FROM yolo_actions WHERE id='ya1'"#,
        [],
    )
}

pub fn status(store: &Store, decision: &str) -> Result<String> {
    store.reader(|c| {
        Ok::<_, crate::StoreError>(c.query_row(
            "SELECT status FROM decisions WHERE id = ?",
            [decision],
            |r| r.get(0),
        )?)
    })
}

pub fn connection_decision(store: &Store, connection: &str) -> Result<Option<Vec<Value>>> {
    let result = rows(
        store,
        "SELECT question, options, status FROM decisions
        WHERE subject_type='connection' AND subject_id=? ORDER BY created DESC LIMIT 1",
        [connection],
    )?;
    Ok(result.into_iter().next())
}

pub fn approvals(store: &Store) -> Result<Vec<crate::Decision>> {
    let ids = texts(
        store,
        "SELECT id FROM decisions WHERE subject_type='chan_message' ORDER BY created,rowid",
        [],
    )?;
    store.read(|r| {
        let mut decisions = Vec::new();
        for id in ids {
            if let Some(decision) = crate::decision_repo::get(r, &id)? {
                decisions.push(decision);
            }
        }
        Ok(decisions)
    })
}
