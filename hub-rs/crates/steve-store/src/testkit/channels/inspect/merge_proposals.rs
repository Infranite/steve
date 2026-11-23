use super::*;

pub fn merge_proposal_reason_by_from_and_into(
    store: &Store,
    from_id: &str,
    into_id: &str,
) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT reason FROM merge_proposals WHERE from_id = ? AND into_id = ?"#,
        [from_id, into_id],
    )
}

pub fn count_merge_proposals_by_from_and_into(
    store: &Store,
    from_id: &str,
    into_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM merge_proposals WHERE from_id = ? AND into_id = ? AND decision_id IS NOT NULL"#,
        [from_id, into_id],
    )
}

pub fn count_merge_proposals(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM merge_proposals"#, [])
}
