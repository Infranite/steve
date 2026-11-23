use super::*;

pub fn notice_state(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT state FROM personal_notifications WHERE topic = 'contact.starred'"#,
        [],
    )
}

pub fn count_personal_notifications(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM personal_notifications"#, [])
}

pub fn count_personal_notifications_by_held(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE state='held'"#,
        [],
    )
}

pub fn count_personal_notifications_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE seen_at IS NOT NULL"#,
        [],
    )
}

pub fn count_personal_notifications_by_purged(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE body IS NULL AND state='purged'"#,
        [],
    )
}

pub fn count_personal_notifications_by_purged_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE delivery_id IS NULL AND body IS NULL AND state='purged'"#,
        [],
    )
}

pub fn count_personal_notifications_by_cancelled(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE state='cancelled'"#,
        [],
    )
}

pub fn count_personal_notifications_by_cancelled_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE topic = 'contact.starred' AND state = 'cancelled'"#,
        [],
    )
}

pub fn count_personal_notifications_by_held_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE topic = 'contact.starred' AND state = 'held'"#,
        [],
    )
}

pub fn count_personal_notifications_count_2(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE topic = 'contact.starred' AND body IS NOT NULL"#,
        [],
    )
}

pub fn count_personal_notifications_count_3(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM personal_notifications WHERE topic = 'contact.starred' AND (body IS NOT NULL OR state <> 'purged')"#,
        [],
    )
}
