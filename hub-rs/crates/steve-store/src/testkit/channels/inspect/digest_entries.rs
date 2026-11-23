use super::*;

pub fn digest_entrie_text_by_workspace(
    store: &Store,
    workspace_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT text FROM digest_entries WHERE workspace_id = ? AND text LIKE '%可能不全%'"#,
        [workspace_id],
    )
}

pub fn count_digest_entries(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM digest_entries WHERE text LIKE '%不提醒%' AND taken IS NULL"#,
        [],
    )
}
