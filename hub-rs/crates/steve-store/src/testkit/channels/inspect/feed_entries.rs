use super::*;

pub fn feed_entrie_kind(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT e.kind FROM feed_entries e JOIN personal_notifications p ON p.id = e.notification_id"#,
        [],
    )
}
