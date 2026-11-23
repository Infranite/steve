use super::*;

pub fn account_party_id_by_handle(store: &Store, handle: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT party_id FROM accounts WHERE handle = ?"#,
        [handle],
    )
}

pub fn party_id_by_workspace_and_user(
    store: &Store,
    workspace_id: &str,
    user_id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM parties WHERE workspace_id = ? AND user_id = ?"#,
        [workspace_id, user_id],
    )
}

pub fn party_id_by_user_and_contact(store: &Store, user_id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT id FROM parties WHERE user_id = ? AND type = 'contact'"#,
        [user_id],
    )
}

pub fn account_handle_system_id_party_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT handle, system_id, party_id FROM accounts WHERE id = ?"#,
        [id],
    )
}

pub fn party_contact_kind_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT contact_kind, status FROM parties WHERE id = ?"#,
        [id],
    )
}

pub fn party_status_merged_into(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT status, merged_into FROM parties WHERE id = ?"#,
        [id],
    )
}

pub fn party_contact_kind_source(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT contact_kind, source FROM parties WHERE id = ?"#,
        [id],
    )
}

pub fn account_party_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(store, r#"SELECT party_id FROM accounts WHERE id = ?"#, [id])
}

pub fn account_display_name(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT a.display_name FROM accounts a JOIN connections c ON c.account_id=a.id WHERE c.id=?"#,
        [id],
    )
}

pub fn account_party_id_row(store: &Store) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT party_id FROM accounts WHERE handle='uhan'"#,
        [],
    )
}

pub fn account_handle_display_name_party_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT handle, display_name, party_id FROM accounts WHERE id = ?"#,
        [id],
    )
}

pub fn account_display_name_row(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT display_name FROM accounts WHERE id = ?"#,
        [id],
    )
}

pub fn account_handle_display_name_kind_system_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT a.handle, a.display_name, a.kind, a.system_id FROM accounts a WHERE a.id = ?"#,
        [id],
    )
}

pub fn handle_group_concathandle_by_connection(
    store: &Store,
    connection_id: &str,
) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT group_concat(handle) FROM account_handles WHERE connection_id = ?"#,
        [connection_id],
    )
}

pub fn account_handle_system_id_user_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT a.handle, a.system_id, p.user_id FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?"#,
        [id],
    )
}

pub fn account_handle_system_id(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(
        store,
        r#"SELECT handle, system_id FROM accounts WHERE id = ?"#,
        [id],
    )
}

pub fn party_name(store: &Store, id: &str) -> Result<Value> {
    json_cell(store, r#"SELECT name FROM parties WHERE id = ?"#, [id])
}

pub fn handle_display_name_name_coalescep_id_by_handle(
    store: &Store,
    handle: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT a.display_name, p.name, coalesce(p.contact_kind, ''), p.id FROM account_handles x
           JOIN accounts a ON a.id = x.account_id JOIN parties p ON p.id = a.party_id
          WHERE x.handle = ?"#,
        [handle],
    )
}

pub fn link_name_source_by_subject_and_works_at(
    store: &Store,
    subject_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT o.name, l.source FROM links l JOIN parties o ON o.id = l.object_id
          WHERE l.subject_id = ? AND l.predicate = 'works_at'"#,
        [subject_id],
    )
}

pub fn profile_gaps_next_check_source_by_party(
    store: &Store,
    party_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT ap.gaps, ap.next_check, ap.source FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?"#,
        [party_id],
    )
}

pub fn revision_patch_by_object(store: &Store, object_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT patch FROM revisions WHERE object_id = ? ORDER BY id DESC LIMIT 1"#,
        [object_id],
    )
}

pub fn link_dept_by_works_at_and_source(store: &Store, source: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT json_extract(props, '$.dept') FROM links WHERE predicate = 'works_at' AND source = ?"#,
        [source],
    )
}

pub fn profile_gaps_checked_by_handle(store: &Store, handle: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT ap.gaps, ap.checked FROM account_profiles ap JOIN account_handles x ON x.account_id = ap.account_id WHERE x.handle = ?"#,
        [handle],
    )
}

pub fn profile_checked_next_check_gaps_by_party(
    store: &Store,
    party_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT ap.checked, ap.next_check, ap.gaps FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?"#,
        [party_id],
    )
}

pub fn profile_checked_next_check_gaps_source_by_party(
    store: &Store,
    party_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT ap.checked, ap.next_check, ap.gaps, ap.source FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?"#,
        [party_id],
    )
}

pub fn profile_checked_by_party(store: &Store, party_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT checked FROM account_profiles WHERE account_id=(SELECT id FROM accounts WHERE party_id=? AND system_id='feishu')"#,
        [party_id],
    )
}

pub fn count_parties_by_contact(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM parties WHERE type = 'contact'"#,
        [],
    )
}

pub fn handle_account_id_by_handle(store: &Store, handle: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT account_id FROM account_handles WHERE handle = ?"#,
        [handle],
    )
}

pub fn account_count_by_user(store: &Store, user_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT count(*) FROM accounts a JOIN parties p ON p.id = a.party_id
          WHERE a.system_id = 'mail' AND a.handle = 'ada@example.com' AND p.user_id = ?"#,
        [user_id],
    )
}

pub fn account_contact_kind_user_id(store: &Store) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT p.contact_kind,p.user_id FROM accounts a JOIN parties p ON p.id=a.party_id
        WHERE a.system_id='mail' AND a.handle='ada@example.com'"#,
        [],
    )
}

pub fn count_team_members(store: &Store) -> Result<i64> {
    cell(store, r#"SELECT COUNT(*) FROM team_members"#, [])
}

pub fn party_status(store: &Store, id: &str) -> Result<Vec<Value>> {
    row(store, r#"SELECT status FROM parties WHERE id = ?"#, [id])
}

pub fn link_count_by_from_and_object(store: &Store, object_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM links WHERE subject_id = 'ST-7' AND predicate = 'from' AND object_id = ?"#,
        [object_id],
    )
}

pub fn party_count_by_user(store: &Store, user_id: &str) -> Result<Value> {
    json_cell(
        store,
        r#"SELECT count(*) FROM parties WHERE user_id = ?"#,
        [user_id],
    )
}

pub fn party_status_value(store: &Store, id: &str) -> Result<Value> {
    json_cell(store, r#"SELECT status FROM parties WHERE id = ?"#, [id])
}

pub fn party_name_contact_kind(store: &Store, id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT name, contact_kind FROM parties WHERE id = ?"#,
        [id],
    )
}

pub fn account_party_id_by_system(store: &Store, system_id: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT party_id FROM accounts WHERE system_id = ? AND handle = 'u_int_1'"#,
        [system_id],
    )
}

pub fn account_party_id_by_handle_single_row(store: &Store, handle: &str) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT party_id FROM accounts WHERE system_id = 'mail' AND handle = ?"#,
        [handle],
    )
}

pub fn account_party_idfamily(store: &Store) -> Result<Vec<Value>> {
    single_row(
        store,
        r#"SELECT party_id || ' ' || family FROM accounts WHERE system_id = 'probe_internal' AND handle = 'U-Int-1'"#,
        [],
    )
}

pub fn link_title_by_subject_and_works_at(
    store: &Store,
    subject_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT json_extract(props, '$.title') FROM links WHERE subject_id = ? AND predicate = 'works_at'"#,
        [subject_id],
    )
}

pub fn account_handle_by_party(store: &Store, party_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT handle FROM accounts WHERE party_id = ? AND family = 'email'"#,
        [party_id],
    )
}

pub fn profile_gaps_by_party(store: &Store, party_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT gaps FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?"#,
        [party_id],
    )
}

pub fn party_org_kind_by_name(store: &Store, name: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT org_kind FROM parties WHERE name = ?"#,
        [name],
    )
}

pub fn handle_count_by_handle(store: &Store, handle: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT count(*) FROM account_handles WHERE family = 'feishu_tenant' AND handle = ?"#,
        [handle],
    )
}

pub fn party_source_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(store, r#"SELECT source FROM parties WHERE id = ?"#, [id])
}

pub fn party_avatar_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(store, r#"SELECT avatar FROM parties WHERE id = ?"#, [id])
}

pub fn link_org_kind_by_subject_and_works_at(
    store: &Store,
    subject_id: &str,
) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT o.org_kind FROM links l JOIN parties o ON o.id = l.object_id WHERE l.subject_id = ? AND l.predicate = 'works_at'"#,
        [subject_id],
    )
}

pub fn profile_source_by_party(store: &Store, party_id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT source FROM account_profiles WHERE account_id=(SELECT id FROM accounts WHERE party_id=? AND system_id='feishu')"#,
        [party_id],
    )
}

pub fn party_contact_kind_by_id(store: &Store, id: &str) -> Result<Vec<Vec<Value>>> {
    rows(
        store,
        r#"SELECT contact_kind FROM parties WHERE id = ?"#,
        [id],
    )
}

pub fn count_revisions_by_object(store: &Store, object_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM revisions WHERE object_type = 'space' AND object_id = ?"#,
        [object_id],
    )
}

pub fn count_links_by_item_and_subject_and_from(
    store: &Store,
    subject_id: &str,
    object_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM links WHERE subject_type = 'item' AND subject_id = ? AND predicate = 'from' AND object_type = 'chan_message' AND object_id = ?"#,
        [subject_id, object_id],
    )
}

pub fn count_links_by_item_and_subject_and_raised_by(
    store: &Store,
    subject_id: &str,
    object_id: &str,
) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM links WHERE subject_type = 'item' AND subject_id = ? AND predicate = 'raised_by' AND object_type = 'party' AND object_id = ?"#,
        [subject_id, object_id],
    )
}

pub fn count_parties_by_ann(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM parties WHERE id = 'ann'"#,
        [],
    )
}

pub fn count_accounts_by_a_ann(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM accounts WHERE id = 'a_ann'"#,
        [],
    )
}

pub fn count_account_handles(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM account_handles WHERE account_id = 'a_ann'"#,
        [],
    )
}

pub fn count_links(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM links WHERE subject_id = 'ann'"#,
        [],
    )
}

pub fn count_parties_by_ids(store: &Store) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM parties WHERE id IN ('bob', 'cat')"#,
        [],
    )
}

pub fn count_revisions_by_object_count(store: &Store, object_id: &str) -> Result<i64> {
    cell(
        store,
        r#"SELECT count(*) FROM revisions WHERE object_type = 'channel' AND object_id = ?"#,
        [object_id],
    )
}

pub fn default_persona_count(store: &Store, ws: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok::<_, crate::StoreError>(c.query_row(
            "SELECT count(*) FROM personas WHERE workspace_id = ? AND is_default = 1",
            [ws],
            |r| r.get(0),
        )?)
    })
}

pub fn persona_revisions(store: &Store, persona: &str) -> Result<Vec<i64>> {
    store.sqlite_read(|c| {
            let mut st = c.prepare(
                "SELECT version FROM revisions WHERE object_type = 'persona' AND object_id = ? ORDER BY id",
            )?;
            Ok::<_, crate::StoreError>(
                st.query_map([persona], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            )
        })
}

pub fn xiaoli_contact(store: &Store, ws: &str) -> Result<String> {
    store.sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT id FROM parties WHERE workspace_id = ? AND type = 'contact' AND name = '王小丽'",
                [ws],
                |r| r.get(0),
            )?)
        })
}

pub fn self_party(store: &Store, ws: &str, user: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok::<_, crate::StoreError>(c.query_row(
            "SELECT id FROM parties WHERE workspace_id = ? AND user_id = ?",
            [ws, user],
            |r| r.get(0),
        )?)
    })
}

pub fn account_count(store: &Store, account: &str) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM accounts WHERE id = ?",
        [account],
    )
}

pub fn department_member_names(store: &Store, department: &str) -> Result<Vec<String>> {
    texts(
        store,
        "SELECT p.name FROM team_members m JOIN team_details d ON d.party_id = m.team_id
        JOIN parties p ON p.id = m.contact_id WHERE d.external_id = ? ORDER BY p.name",
        [department],
    )
}

pub fn department_status(store: &Store, department: &str) -> Result<Option<String>> {
    Ok(texts(
        store,
        "SELECT p.status FROM team_details d
        JOIN parties p ON p.id=d.party_id WHERE d.external_id=?",
        [department],
    )?
    .pop())
}

pub fn department_member_roles(store: &Store, department: &str) -> Result<Vec<String>> {
    texts(
        store,
        "SELECT m.role FROM team_members m
        JOIN team_details d ON d.party_id=m.team_id WHERE d.external_id=?",
        [department],
    )
}

pub fn named_contact_count(store: &Store, ws: &str, name: &str) -> Result<i64> {
    cell(
        store,
        "SELECT count(*) FROM parties WHERE type='contact' AND name=? AND workspace_id=?",
        [name, ws],
    )
}
