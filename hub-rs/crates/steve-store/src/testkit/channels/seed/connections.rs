use super::*;

pub fn set_channel_trigger(store: &Store, v: &str, me: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE channel_settings SET trigger = ?1 WHERE connection_id = ?2"#,
            params![v, me],
        )?;
        Ok(())
    })
}

pub fn set_channel_trigger_for_nothing_reaches_the_adapter_without_a(
    store: &Store,
    me: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE channel_settings SET trigger = '{"on":true,"words":["/bot"],"where":["dm:*"]}' WHERE connection_id = ?1"#, params![me])?;
        Ok(())
    })
}

pub fn set_connection_status_paused(store: &Store, me: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE connections SET status = 'paused' WHERE id = ?1"#,
            params![me],
        )?;
        Ok(())
    })
}

pub fn set_connection_status_ok(store: &Store, me: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE connections SET status = 'ok' WHERE id = ?1"#,
            params![me],
        )?;
        Ok(())
    })
}

pub fn set_connection_caps_msg_receive(store: &Store, me: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE connections SET caps = '["msg.receive"]' WHERE id = ?1"#,
            params![me],
        )?;
        Ok(())
    })
}

pub fn set_connection_revision(store: &Store, me: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE connections SET revision=revision+1 WHERE id=?1"#,
            params![me],
        )?;
        Ok(())
    })
}

pub fn set_connection_caps(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE connections SET caps = '["msg.receive","msg.send_as_user"]' WHERE id = ?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_connection_enabled_at(store: &Store, enabled_at: &str, id: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            r#"UPDATE connections SET enabled_at = ?1 WHERE id = ?2"#,
            params![enabled_at, id],
        )?;
        Ok(())
    })
}

pub fn set_channel_late_ms_value_300000(store: &Store, connection_id: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            r#"UPDATE channel_settings SET late_ms = 300000 WHERE connection_id = ?1"#,
            params![connection_id],
        )?;
        Ok(())
    })
}

pub fn seed_prompt_has_seven_layers_in_order(
    store: &Store,
    connection: &str,
    space: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE channel_settings SET duty = '账号职责：只答运维问题' WHERE connection_id = ?1;"#, params![connection])?;
        c.execute(
            r#"UPDATE space_settings SET duty = '空间职责：部署窗口是周四' WHERE space_id = ?1;"#,
            params![space],
        )?;
        Ok(())
    })
}

pub fn set_channel_card_ttl_h(store: &Store, v: Option<i64>, connection_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE channel_settings SET card_ttl_h = ?1 WHERE connection_id = ?2"#,
            params![v, connection_id],
        )?;
        Ok(())
    })
}

pub fn set_channel_card_ttl_h_value_1(store: &Store, connection_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE channel_settings SET card_ttl_h = 1 WHERE connection_id = ?1"#,
            params![connection_id],
        )?;
        Ok(())
    })
}

pub fn set_connection_status_paused_for_connections_listed_and_changed_mean(
    store: &Store,
) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"UPDATE connections SET status = 'paused' WHERE id = 'con_a'"#)?;
        Ok(())
    })
}

pub fn set_connection_status_ok_for_connections_listed_and_changed_mean(
    store: &Store,
) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"UPDATE connections SET status = 'ok' WHERE id = 'con_a'"#)?;
        Ok(())
    })
}

pub fn remove_connection(store: &Store) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"DELETE FROM connections WHERE id = 'con_b'"#)?;
        Ok(())
    })
}

pub fn set_connection_credential_item_vi_con_a_2(store: &Store) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(
            r#"UPDATE connections SET credential_item = 'vi_con_a_2' WHERE id = 'con_a'"#,
        )?;
        Ok(())
    })
}

pub fn set_connection_config(store: &Store) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"UPDATE connections SET config = '{"x":1}' WHERE id = 'con_a'"#)?;
        Ok(())
    })
}

pub fn set_connection_credential_item_vi_2(store: &Store) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(
            r#"UPDATE connections SET credential_item = 'vi_2' WHERE id = 'con_slow'"#,
        )?;
        Ok(())
    })
}

pub fn set_connection_status_paused_for_connections_paired_with_a_bot_follo(
    store: &Store,
) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"UPDATE connections SET status = 'paused' WHERE id = 'con_bot'"#)?;
        Ok(())
    })
}

pub fn set_connection_status_ok_for_connections_paired_with_a_bot_follo(
    store: &Store,
) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"UPDATE connections SET status = 'ok' WHERE id = 'con_bot'"#)?;
        Ok(())
    })
}

pub fn set_connection_caps_for_connections_paired_with_a_bot_follo(store: &Store) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"UPDATE connections SET caps = '[]' WHERE id = 'con_me'"#)?;
        Ok(())
    })
}

pub fn set_connection_status_error(store: &Store) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"UPDATE connections SET status = 'error' WHERE id = 'con_bot'"#)?;
        Ok(())
    })
}

pub fn seed_cursor_con_me(store: &Store, ws: &str) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute(r#"INSERT INTO chan_cursors (connection_id, workspace_id, container, kind, chat_id, cursor, created, updated)
           VALUES ('con_me', ?1, 'oc_poll', 'chat', 'oc_poll', 5, 1, 1)"#, params![ws])?;
        Ok(())
    })
}

pub fn seed_read_views_are_bound_to_one_connection(store: &Store) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute_batch(r#"DELETE FROM chan_cursors WHERE connection_id = 'con_me';"#)?;
        c.execute_batch(r#"DELETE FROM connections WHERE id = 'con_me'"#)?;
        Ok(())
    })
}

pub fn pause_attention_bot(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE connections SET status='paused' WHERE id='bot'", [])?;
        Ok(())
    })
}

pub fn revoke_attention_owner(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE connections SET status='revoked' WHERE id='own'", [])?;
        Ok(())
    })
}

pub fn permit_receive_only(store: &Store, connection: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE connections SET caps = '[\"msg.receive\"]' WHERE id = ?",
            [connection],
        )?;
        Ok(())
    })
}

pub fn move_connection_to_other_workspace(store: &Store, connection: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE connections SET workspace_id = 'w2' WHERE id = ?",
            [connection],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn return_connection_to_default_workspace(store: &Store, connection: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE connections SET workspace_id = 'default' WHERE id = ?",
            [connection],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn link_personal_connection(store: &Store, bot: &str, own: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE connections SET config = json_object('app_connection', ?) WHERE id = ?",
            rusqlite::params![bot, own],
        )?;
        Ok(())
    })
}

pub fn set_connection_seniority(store: &Store, earlier: &str, later: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE connections SET created=10 WHERE id=?", [earlier])?;
        tx.conn
            .execute("UPDATE connections SET created=20 WHERE id=?", [later])?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn set_fixture_connection_adapter(
    store: &Store,
    con: &str,
    adapter: &str,
    config: &str,
) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE connections SET adapter = ?2, config = ?3 WHERE id = ?1",
            rusqlite::params![con, adapter, config],
        )?;
        Ok(())
    })
}

pub fn set_fixture_connection_owner(w: &mut crate::W<'_>, id: &str, owner: &str) -> Result<()> {
    w.fixture_connection().execute(
        "UPDATE connections SET created_by = ? WHERE id = ?",
        [owner, id],
    )?;
    Ok(())
}

pub fn set_api_base(store: &Store, connection: &str, base: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            "UPDATE connections SET config=json_set(config,'$.api_base',?) WHERE id=?",
            [base, connection],
        )?;
        Ok(())
    })
}
