use super::*;

pub fn count_vault_items_by_active(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT COUNT(*) FROM vault_items WHERE template = 'connection_credential' AND status = 'active'"#,
        [],
    )
}
