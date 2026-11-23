use super::*;

pub fn connection_id_by_account(store: &Store, account_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM connections WHERE account_id = ?"#,
        [account_id],
    )
}

pub fn connection_config(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT config FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_caps(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(store, r#"SELECT caps FROM connections WHERE id = ?"#, [id])
}

pub fn connection_caps_granted_scopes_config(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT caps, granted_scopes, config FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_account_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT account_id FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_credential_item(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT credential_item FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_id_caps_credential_item_by_account(
    store: &Store,
    account_id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id, caps, credential_item FROM connections WHERE account_id = ?"#,
        [account_id],
    )
}

pub fn connection_id_caps_by_account(store: &Store, account_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id, caps FROM connections WHERE account_id = ?"#,
        [account_id],
    )
}

pub fn connection_id_config_by_account(store: &Store, account_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id, config FROM connections WHERE account_id = ?"#,
        [account_id],
    )
}

pub fn connection_revision(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT revision FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_status_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT status FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_fails_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT json_extract(health, '$.fails') FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_id_by_account_rows(store: &Store, account_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT id FROM connections WHERE account_id = ?"#,
        [account_id],
    )
}

pub fn connection_caps_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(store, r#"SELECT caps FROM connections WHERE id = ?"#, [id])
}

pub fn connection_config_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT config FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_missing_permissions_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT json_extract(config, '$.missing_permissions') FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_directory_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT json_extract(config, '$.directory') FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_missing_scopes_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT json_extract(config, '$.directory.missing_scopes') FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_handle_config_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT a.handle, c.config FROM connections c JOIN accounts a ON a.id = c.account_id WHERE c.id = ?"#,
        [id],
    )
}

pub fn connection_display_name_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT a.display_name FROM connections c JOIN accounts a ON a.id = c.account_id WHERE c.id = ?"#,
        [id],
    )
}

pub fn connection_health_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT health FROM connections WHERE id = ?"#,
        [id],
    )
}

pub fn connection_kind_contact_kind_user_id_caps_by_id(
    store: &Store,
    id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT a.kind,p.contact_kind,p.user_id,c.caps FROM connections c
        JOIN accounts a ON a.id=c.account_id JOIN parties p ON p.id=a.party_id WHERE c.id=?"#,
        [id],
    )
}

pub fn connection_party_id_adapter_caps_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT a.party_id,c.adapter,c.caps FROM connections c JOIN accounts a ON a.id=c.account_id WHERE c.id=?"#,
        [id],
    )
}

pub fn connection_status_by_con_f(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT status FROM connections WHERE id = 'con_f'"#,
        [],
    )
}

pub fn connection_status_by_con_p(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT status FROM connections WHERE id = 'con_p'"#,
        [],
    )
}

pub fn connection_status_by_con_r(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT status FROM connections WHERE id = 'con_r'"#,
        [],
    )
}

pub fn channel_settings_mode_by_connection(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT mode FROM channel_settings WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn count_connections(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT COUNT(*) FROM connections"#, [])
}

pub fn count_channel_settings_by_connection(store: &Store, connection_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT history_late FROM channel_settings WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn count_channel_settings_by_connection_count(
    store: &Store,
    connection_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT history_before FROM channel_settings WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn channel_settings_mode_by_connection_rows(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT mode FROM channel_settings WHERE connection_id=?"#,
        [connection_id],
    )
}

pub fn connection_id(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(store, r#"SELECT id FROM connections"#, [])
}

pub fn connection_count(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(store, r#"SELECT count(*) FROM connections"#, [])
}

pub fn count_connections_by_workspace(store: &Store, workspace_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM connections WHERE workspace_id = ?"#,
        [workspace_id],
    )
}

pub fn count_channel_settings_by_connection_count_2(
    store: &Store,
    connection_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM channel_settings WHERE connection_id = ? AND mode = 'bot'"#,
        [connection_id],
    )
}

pub fn count_channel_settings_by_connection_count_3(
    store: &Store,
    connection_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM channel_settings WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn count_channel_settings_by_connection_count_4(
    store: &Store,
    connection_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM channel_settings WHERE connection_id = ? AND mode = 'self'"#,
        [connection_id],
    )
}

pub fn channel_settings_mode_by_con_a(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT mode FROM channel_settings WHERE connection_id = 'con_a'"#,
        [],
    )
}

pub fn connection_status_by_con_a(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT status FROM connections WHERE id = 'con_a'"#,
        [],
    )
}

pub fn connection_last_error_by_con_a(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT coalesce(json_extract(health, '$.last_error'), '') FROM connections WHERE id = 'con_a'"#,
        [],
    )
}

pub fn connection_last_error_by_con_f(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT json_extract(health, '$.last_error') FROM connections WHERE id = 'con_f'"#,
        [],
    )
}

pub fn channel_settings_mode_by_con_me(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT mode FROM channel_settings WHERE connection_id = 'con_me'"#,
        [],
    )
}

pub fn connection_fails_by_con_a(store: &Store) -> Result<String> {
    cell(
        store,
        r#"SELECT CAST(coalesce(json_extract(health,'$.fails'),0) AS TEXT) FROM connections WHERE id='con_a'"#,
        [],
    )
}

pub fn feishu_user_connection(store: &Store) -> Result<String> {
    store.reader(|c| {
        Ok::<_, crate::StoreError>(c.query_row(
            "SELECT id FROM connections WHERE adapter = 'feishu_user'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn account_id(store: &Store, connection: &str) -> Result<String> {
    cell(
        store,
        "SELECT account_id FROM connections WHERE id = ?",
        [connection],
    )
}

pub fn count_by_id(store: &Store, connection: &str) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM connections WHERE id = ?",
        [connection],
    )
}
