use super::*;

pub fn space_id_by_external(store: &Store, external_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM spaces WHERE external_id = ? ORDER BY created LIMIT 1"#,
        [external_id],
    )
}

pub fn space_id_by_d_self_and_connection(store: &Store, connection_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM spaces WHERE external_id = 'D_SELF' AND connection_id = ?"#,
        [connection_id],
    )
}

pub fn space_id_by_external_row(store: &Store, external_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM spaces WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn space_id_by_d_a(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM spaces WHERE external_id = 'D_A'"#,
        [],
    )
}

pub fn space_id_by_d_alert(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM spaces WHERE external_id = 'D_ALERT'"#,
        [],
    )
}

pub fn space_name(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(store, r#"SELECT name FROM spaces WHERE id = ?"#, [id])
}

pub fn space_id_by_c_ops(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM spaces WHERE external_id='C_OPS'"#,
        [],
    )
}

pub fn space_name_kind_member_count_has_external(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT s.name, s.kind, s.member_count, s.has_external, ss.configured, ss.disclosure, s.system_id FROM spaces s
           JOIN space_settings ss ON ss.space_id = s.id WHERE s.id = ?"#,
        [id],
    )
}

pub fn space_id_by_g1(store: &Store) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT id FROM spaces WHERE external_id = 'G1'"#,
        [],
    )
}

pub fn space_id_by_external_value(store: &Store, external_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT id FROM spaces WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn space_member_count(store: &Store, external_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM space_members WHERE space_id = (SELECT id FROM spaces WHERE external_id = ?)"#,
        [external_id],
    )
}

pub fn space_id_name_kind_system_id_by_external(
    store: &Store,
    external_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT s.id, s.name, s.kind, s.system_id, s.member_count, ss.configured FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.external_id = ?"#,
        [external_id],
    )
}

pub fn space_name_kind_by_external(store: &Store, external_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT name, kind FROM spaces WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn space_has_external_disclosure_by_external(
    store: &Store,
    external_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT s.has_external, ss.disclosure FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.external_id = ?"#,
        [external_id],
    )
}

pub fn space_id_by_g1_rows(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT id FROM spaces WHERE external_id = 'G1' ORDER BY created"#,
        [],
    )
}

pub fn space_identity_connection_id_ordinal_owner_user_by_space(
    store: &Store,
    space_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT connection_id, ordinal, owner_user FROM space_identities WHERE space_id = ? ORDER BY ordinal"#,
        [space_id],
    )
}

pub fn space_id_name_kind_status_by_external(
    store: &Store,
    external_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT s.id, s.name, s.kind, s.status, s.has_external, s.member_count, ss.configured, ss.disclosure
           FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.external_id = ?"#,
        [external_id],
    )
}

pub fn space_id_by_external_rows(store: &Store, external_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT id FROM spaces WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn space_name_kind_by_c_42(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT name, kind FROM spaces WHERE external_id = 'c_42'"#,
        [],
    )
}

pub fn space_member_role_by_issue_9(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT sm.role FROM space_members sm JOIN spaces s ON s.id=sm.space_id JOIN accounts a ON a.id=sm.account_id JOIN accounts alias ON alias.party_id=a.party_id WHERE s.external_id='issue-9' AND alias.handle='contributor@example.org'"#,
        [],
    )
}

pub fn count_spaces_by_connection(store: &Store, connection_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn count_spaces_by_id(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT has_external FROM spaces WHERE id = ?"#,
        [id],
    )
}

pub fn count_spaces_by_id_count(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE name = '平台值班群（夜班）' AND id = ?"#,
        [id],
    )
}

pub fn count_spaces_by_left_and_id(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE status = 'left' AND id = ?"#,
        [id],
    )
}

pub fn count_spaces_by_active_and_id(store: &Store, id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE status = 'active' AND id = ?"#,
        [id],
    )
}

pub fn count_spaces_by_archived_and_external(store: &Store, external_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE status = 'archived' AND external_id = ?"#,
        [external_id],
    )
}

pub fn space_connection_id(store: &Store, id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT connection_id FROM spaces WHERE id = ?"#,
        [id],
    )
}

pub fn space_member_count_by_external(store: &Store, external_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT member_count FROM spaces WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn space_name_kind_disclosure(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT s.name, s.kind, ss.disclosure FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.id = ?"#,
        [id],
    )
}

pub fn space_has_external_disclosure(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT s.has_external, ss.disclosure FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.id = ?"#,
        [id],
    )
}

pub fn space_name_kind(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(store, r#"SELECT name, kind FROM spaces WHERE id = ?"#, [id])
}

pub fn space_member_role_by_space_and_account(
    store: &Store,
    space_id: &str,
    account_id: &str,
) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT role FROM space_members WHERE space_id = ? AND account_id = ?"#,
        [space_id, account_id],
    )
}

pub fn space_coalescemax_by_space(store: &Store, space_id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT coalesce(max(muted), 0) FROM space_settings WHERE space_id = ?"#,
        [space_id],
    )
}

pub fn count_space_members_by_space(store: &Store, space_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM space_members sm JOIN accounts a ON a.id=sm.account_id WHERE sm.space_id=? AND a.handle='uhan'"#,
        [space_id],
    )
}

pub fn count_spaces_by_g1(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT last_message FROM spaces WHERE external_id = 'G1'"#,
        [],
    )
}

pub fn count_space_members_by_space_count(store: &Store, space_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM space_members WHERE space_id = ?"#,
        [space_id],
    )
}

pub fn count_spaces(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT count(*) FROM spaces"#, [])
}

pub fn space_member_account_id(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT sm.account_id FROM space_members sm JOIN accounts a ON a.id=sm.account_id JOIN accounts alias ON alias.party_id=a.party_id WHERE alias.handle='contributor@example.org'"#,
        [],
    )
}

pub fn space_has_external_by_external(store: &Store, external_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT has_external FROM spaces WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn count_spaces_by_ids(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE id IN ('sp_a', 'sp_c')"#,
        [],
    )
}

pub fn count_spaces_by_sp_b(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE id = 'sp_b'"#,
        [],
    )
}

pub fn count_space_members(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM space_members WHERE space_id = 'sp_a'"#,
        [],
    )
}

pub fn count_spaces_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE account_id = 'acc_old'"#,
        [],
    )
}

pub fn count_spaces_by_ids_count(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE id IN ('sp_c', 'sp_d')"#,
        [],
    )
}

pub fn count_space_identities(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM space_identities WHERE space_id IN ('sp_c', 'sp_d')"#,
        [],
    )
}

pub fn count_spaces_by_sp_a(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE id = 'sp_a'"#,
        [],
    )
}

pub fn count_space_settings_by_workspace(store: &Store, workspace_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM space_settings WHERE workspace_id = ?"#,
        [workspace_id],
    )
}

pub fn count_spaces_by_external(store: &Store, external_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces WHERE external_id = ?"#,
        [external_id],
    )
}

pub fn count_spaces_by_connection_count_2(store: &Store, connection_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE ss.configured = 0 AND s.connection_id = ?"#,
        [connection_id],
    )
}

pub fn count_space_members_by_space_count_2(store: &Store, space_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM space_members m JOIN account_handles ah ON ah.account_id = m.account_id
              WHERE m.space_id = ? AND ah.handle = 'on_bob0000000000000000000000000001'"#,
        [space_id],
    )
}

pub fn member_count(store: &Store, space: &str) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM space_members WHERE space_id = ?",
        [space],
    )
}

pub fn latest_revision_patch(store: &Store, space: &str) -> Result<String> {
    store.sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT patch FROM revisions WHERE object_type = 'space' AND object_id = ? ORDER BY id DESC LIMIT 1",
                [space],
                |r| r.get(0),
            )?)
        })
}

pub fn spaces_for_connection(store: &Store, connection: &str) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM spaces WHERE connection_id = ?",
        [connection],
    )
}

pub fn detached_spaces(store: &Store, ws: &str, status: &str) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM spaces
        WHERE connection_id IS NULL AND status = ? AND account_id IS NOT NULL AND workspace_id = ?",
        [status, ws],
    )
}

pub fn count_by_external(store: &Store, chat: &str) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM spaces WHERE external_id = ?",
        [chat],
    )
}

pub fn conversation_history(store: &Store, space: &str) -> Result<(String, String)> {
    store.sqlite_read(|c| Ok(c.query_row(
        "SELECT s.status, m.text FROM spaces s JOIN messages m ON m.space='chan:' || s.id WHERE s.id=?",
        [space], |r| Ok((r.get(0)?, r.get(1)?))
    )?))
}
