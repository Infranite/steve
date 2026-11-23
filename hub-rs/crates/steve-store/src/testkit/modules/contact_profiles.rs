use super::{execute, scalar};
use crate::{Result, Store};

pub fn seed_profile_connection(store: &Store) -> Result<()> {
    execute(store, "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
               SELECT 'acc_con_fs', 'default', 'feishu_app', 'feishu_app', id, 'app_bot', 'cli_x', 'cli_x', 'connect', 1, 1
                 FROM parties WHERE workspace_id = 'default' AND org_kind = 'self';
             INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
               VALUES ('con_fs', 'default', 'acc_con_fs', 'feishu', 'credential', '[\"msg.receive\",\"directory.read\"]', 'usr_dev', 1, 1);")
}

pub fn profile_party(store: &Store, id: &str) -> Result<String> {
    scalar(
        store,
        "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id WHERE h.family = 'feishu_user' AND h.handle = ?",
        rusqlite::params![id],
    )
}

pub fn profile_account(store: &Store, id: &str) -> Result<String> {
    scalar(
        store,
        "SELECT h.account_id FROM account_handles h WHERE h.family = 'feishu_user' AND h.handle = ?",
        rusqlite::params![id],
    )
}

pub fn own_organization(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT id FROM parties WHERE workspace_id = 'default' AND org_kind = 'self'",
        [],
    )
}

pub fn manual_job_title(store: &Store, hanmeimei: &str, org: &str) -> Result<()> {
    execute(store, &format!("UPDATE links SET props = json_set(props, '$.title', '首席架构师') WHERE subject_id = '{hanmeimei}' AND object_id = '{org}' AND predicate = 'works_at';
         INSERT INTO revisions (workspace_id, object_type, object_id, version, patch, author, created)
           VALUES ('default', 'party', '{hanmeimei}', 99,
             json('{{\"before\":{{\"job\":{{\"org_id\":\"{org}\",\"props\":{{\"title\":\"工程师\"}}}}}},\"after\":{{\"job\":{{\"org_id\":\"{org}\",\"props\":{{\"title\":\"首席架构师\"}}}}}}}}'), 'usr_dev', 1);"))
}

pub fn job_properties(store: &Store, zhangwei: &str, org: &str) -> Result<String> {
    scalar(
        store,
        "SELECT props FROM links WHERE subject_id = ? AND object_id = ? AND predicate = 'works_at'",
        rusqlite::params![zhangwei, org],
    )
}

pub fn clear_job_title(store: &Store, hanmeimei: &str, org: &str) -> Result<()> {
    execute(
        store,
        &format!(
            "UPDATE links SET props = json_set(props, '$.title', NULL) WHERE subject_id = '{hanmeimei}' AND object_id = '{org}'"
        ),
    )
}

pub fn missing_profile_permission(store: &Store) -> Result<()> {
    execute(
        store,
        "UPDATE connections SET config = '{\"permissions\":[{\"scope\":\"tenant:tenant:readonly\",\"status\":\"missing\"}]}' WHERE id = 'con_fs'",
    )
}

pub fn enable_profile_adapter(store: &Store) -> Result<()> {
    execute(
        store,
        "UPDATE connections SET adapter = 'feishu_app', status = 'ok' WHERE id = 'con_fs'",
    )
}
