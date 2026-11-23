//! Shared persistence fixture setup for Vault key and passkey tests.

use rusqlite::params;

use crate::{Result, Store};

pub fn auth_session(
    store: &Store,
    user: &str,
    token: &str,
    created: i64,
    expires: i64,
) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "INSERT INTO auth_sessions (token_hash, user_id, created, expires) VALUES (?, ?, ?, ?)",
            params![crate::sha(token), user, created, expires],
        )?;
        Ok(())
    })
}

pub fn use_count(
    store: &Store,
    mode: Option<&str>,
    actor: Option<&str>,
    status: Option<&str>,
) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT count(*) FROM secret_uses
         WHERE (?1 IS NULL OR mode = ?1)
           AND (?2 IS NULL OR (actor_type = 'me' AND actor_id = ?2))
           AND (?3 IS NULL OR status = ?3)",
            params![mode, actor, status],
            |row| row.get(0),
        )?)
    })
}

pub fn seed_rotation_vaults(store: &Store, ws: &str) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "INSERT INTO vaults (id, workspace_id, name, scope, created) VALUES ('vlt_s', ?, '共享', 'shared', 1), ('vlt_sys', ?, '系统', 'system', 1)",
            [ws, ws],
        )?;
        Ok(())
    })
}

pub struct EncryptedField<'a> {
    pub ws: &'a str,
    pub actor: &'a str,
    pub item: &'a str,
    pub field: &'a str,
    pub wrapped: &'a [u8],
    pub version: i64,
    pub ciphertext: &'a [u8],
    pub at: i64,
}

pub fn seed_encrypted_field(w: &mut crate::W<'_>, field: &EncryptedField<'_>) -> Result<()> {
    use sea_query::Query;
    w.exec(
        Query::insert()
            .into_table("vault_items")
            .columns([
                "id",
                "workspace_id",
                "vault_id",
                "template",
                "title",
                "sensitivity",
                "dek_wrapped",
                "kek_version",
                "created_by",
                "created",
                "updated",
            ])
            .values_panic([
                field.item.into(),
                field.ws.into(),
                "vlt_s".into(),
                "login".into(),
                "GitHub".into(),
                "sensitive".into(),
                field.wrapped.to_vec().into(),
                field.version.into(),
                field.actor.into(),
                field.at.into(),
                field.at.into(),
            ]),
    )?;
    w.exec(
        Query::insert()
            .into_table("vault_fields")
            .columns([
                "id",
                "workspace_id",
                "item_id",
                "label",
                "type",
                "secret",
                "value_ct",
                "updated",
            ])
            .values_panic([
                field.field.into(),
                field.ws.into(),
                field.item.into(),
                "密码".into(),
                "password".into(),
                1_i64.into(),
                field.ciphertext.to_vec().into(),
                field.at.into(),
            ]),
    )?;
    Ok(())
}

pub fn encrypted_field(store: &Store, item: &str, field: &str) -> Result<(Vec<u8>, i64, Vec<u8>)> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT i.dek_wrapped, i.kek_version, f.value_ct
           FROM vault_items i JOIN vault_fields f ON f.item_id = i.id
          WHERE i.id = ? AND f.id = ?",
            [item, field],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    })
}

pub fn init_audit_count(store: &Store, ws: &str) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT count(*) FROM audit WHERE action = 'vault.init' AND workspace_id = ?",
            [ws],
            |row| row.get(0),
        )?)
    })
}

pub fn make_system_item(store: &Store, item: &str) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "UPDATE vault_items SET vault_id = 'vlt_sys' WHERE id = ?",
            [item],
        )?;
        Ok(())
    })
}

pub fn copy_field_ciphertext(store: &Store, from: &str, to: &str) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute("UPDATE vault_fields SET value_ct = (SELECT value_ct FROM vault_fields WHERE id = ?) WHERE id = ?", [from, to])?;
        Ok(())
    })
}

pub fn copy_wrapped_key(store: &Store, from: &str, to: &str) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute("UPDATE vault_items SET dek_wrapped = (SELECT dek_wrapped FROM vault_items WHERE id = ?) WHERE id = ?", [from, to])?;
        Ok(())
    })
}

pub fn items_with_key_version(store: &Store, version: i64) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT count(*) FROM vault_items WHERE kek_version = ?",
            [version],
            |row| row.get(0),
        )?)
    })
}

pub fn corrupt_wrapped_key(store: &Store, item: &str) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "UPDATE vault_items SET dek_wrapped = zeroblob(72) WHERE id = ?",
            [item],
        )?;
        Ok(())
    })
}

pub fn clear_wrapped_key(store: &Store, item: &str) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "UPDATE vault_items SET dek_wrapped = NULL WHERE id = ?",
            [item],
        )?;
        Ok(())
    })
}

pub fn seed_vault_search_account(store: &Store) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute_batch(
            "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('ct_me', 'default', 'contact', '我', 'usr_dev', 1, 1);
             INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, display_name, source, created, updated)
             VALUES ('acc_gh', 'default', 'github', 'github_user', 'ct_me', 'personal', 'octo-handle', 'octo-handle', '', 'me', 1, 1);",
        )?;
        Ok(())
    })
}

pub fn seed_vault_workspace(store: &Store) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "INSERT INTO workspaces (id, name, created) VALUES ('w3', 'x', 1)",
            [],
        )?;
        Ok(())
    })
}

pub fn audit_count(store: &Store, action: &str, target: &str) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT count(*) FROM audit WHERE action = ? AND target = ?",
            [action, target],
            |row| row.get(0),
        )?)
    })
}

pub fn audit_detail(store: &Store, action: &str, target: Option<&str>) -> Result<String> {
    store.sqlite_read(|conn| Ok(conn.query_row(
        "SELECT detail FROM audit WHERE action = ?1 AND (?2 IS NULL OR target = ?2) ORDER BY id LIMIT 1",
        params![action, target], |row| row.get(0),
    )?))
}

pub fn all_audit_details(store: &Store) -> Result<String> {
    store.sqlite_read(|conn| {
        Ok(
            conn.query_row("SELECT group_concat(detail) FROM audit", [], |row| {
                row.get(0)
            })?,
        )
    })
}

pub fn revoked_grants(store: &Store, item: &str) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT count(*) FROM grants WHERE resource_id = ? AND revoked IS NOT NULL",
            [item],
            |row| row.get(0),
        )?)
    })
}

pub fn vault_item_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row("SELECT count(*) FROM vault_items", [], |row| row.get(0))?)
    })
}

pub type AccessLog = (String, String, String, String);

pub fn access_log(store: &Store) -> Result<Vec<AccessLog>> {
    store.sqlite_read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT actor_type, actor_id, mode, fields FROM secret_uses ORDER BY created, rowid",
        )?;
        Ok(stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn item_use_count(
    store: &Store,
    item: &str,
    mode: Option<&str>,
    status: &str,
    actor: Option<&str>,
) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT count(*) FROM secret_uses WHERE item_id = ?1 AND status = ?2
           AND (?3 IS NULL OR mode = ?3) AND (?4 IS NULL OR actor_id = ?4)",
            params![item, status, mode, actor],
            |row| row.get(0),
        )?)
    })
}

pub fn first_failed_use_finished(store: &Store, item: &str) -> Result<i64> {
    store.sqlite_read(|conn| Ok(conn.query_row(
        "SELECT finished FROM secret_uses WHERE item_id = ? AND status = 'failed' ORDER BY rowid LIMIT 1",
        [item], |row| row.get(0),
    )?))
}

pub fn corrupt_fields(store: &Store, item: &str) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "UPDATE vault_fields SET value_ct = zeroblob(64) WHERE item_id = ?",
            [item],
        )?;
        Ok(())
    })
}

pub fn age_uses(store: &Store, item: &str, age: i64) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute("UPDATE secret_uses SET created = created - ?1, finished = finished - ?1 WHERE item_id = ?2", params![age, item])?;
        Ok(())
    })
}

pub fn checkpoint(store: &Store) -> Result<()> {
    store.write_locked(|conn| {
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    })
}

pub struct UseContext<'a> {
    pub ws: &'a str,
    pub bot: &'a str,
    pub item: &'a str,
    pub project: &'a str,
    pub session: &'a str,
    pub workdir: &'a str,
}

pub fn seed_use_context(store: &Store, ctx: &UseContext<'_>) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute(
            "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES (?, ?, '前端', 'claude', 1)",
            params![ctx.bot, ctx.ws],
        )?;
        conn.execute(
            "INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, 1)",
            params![ctx.bot, ctx.ws],
        )?;
        conn.execute(
            "INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES (?, ?, 'steve', ?, 1, 1)",
            params![ctx.project, ctx.ws, ctx.workdir],
        )?;
        conn.execute(
            "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, project_id, created, updated)
             VALUES (?, ?, 1, '推送分支', 'run', '执行', ?, ?, 1, 1)",
            params![ctx.item, ctx.ws, ctx.bot, ctx.project],
        )?;
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, item_id, bot_id, harness, cwd, status, created, updated)
             VALUES (?, ?, ?, ?, 'claude', ?, 'running', 1, 1)",
            params![ctx.session, ctx.ws, ctx.item, ctx.bot, ctx.workdir],
        )?;
        Ok(())
    })
}

pub fn latest_use(store: &Store) -> Result<String> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT id FROM secret_uses ORDER BY created DESC, rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )?)
    })
}

pub fn decision_count(store: &Store, subject_type: &str, status: Option<&str>) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
        "SELECT count(*) FROM decisions WHERE subject_type = ?1 AND (?2 IS NULL OR status = ?2)",
        params![subject_type, status], |row| row.get(0),
    )?)
    })
}

pub fn other_grants(store: &Store, item: &str, ignored: &str) -> Result<i64> {
    store.sqlite_read(|conn| {
        Ok(conn.query_row(
            "SELECT count(*) FROM grants WHERE resource_id = ? AND id != ?",
            [item, ignored],
            |row| row.get(0),
        )?)
    })
}

pub fn seed_use_retention(
    store: &Store,
    ws: &str,
    item: &str,
    bot: &str,
    old: &str,
    day: i64,
) -> Result<()> {
    store.write_locked(|conn| {
        conn.execute("UPDATE secret_uses SET created = created - ? WHERE id = ?", params![2 * day, old])?;
        conn.execute(
            "INSERT INTO secret_uses (id, workspace_id, item_id, actor_type, actor_id, purpose, mode, status, created)
             VALUES ('su_ancient', ?, ?, 'bot', ?, 'x', 'env', 'ok', ?)",
            params![ws, item, bot, crate::now() - 400 * day],
        )?;
        Ok(())
    })
}
