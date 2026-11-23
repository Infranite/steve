use super::*;

pub fn count_outbox_receipts(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM outbox_receipts"#, [])
}
