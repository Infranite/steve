use super::*;

pub fn merge_count_by_from(store: &Store, from_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT COUNT(*) FROM merges WHERE from_id = ?"#,
        [from_id],
    )
}
