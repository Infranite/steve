use super::*;

pub fn set_space_name(store: &Store, w_space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE spaces SET name = '私聊' WHERE id = ?1"#,
            params![w_space_id],
        )?;
        Ok(())
    })
}

pub fn set_space_identity_left_at_value_2(store: &Store, plan_space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE space_identities SET left_at=2 WHERE space_id=?1"#,
            params![plan_space_id],
        )?;
        Ok(())
    })
}

pub fn remove_space_member(store: &Store, two_space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"DELETE FROM space_members WHERE space_id=?1 AND account_id IN (SELECT id FROM accounts WHERE party_id='output-person')"#, params![two_space_id])?;
        Ok(())
    })
}

pub fn seed_space_member(store: &Store, pinned: &str, ws: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO space_members(space_id,workspace_id,account_id,role,joined) SELECT ?1,?2,id,'member',1 FROM accounts WHERE handle='uhan' LIMIT 1"#, params![pinned, ws])?;
        Ok(())
    })
}

pub fn seed_notification_targets_are_explicit_sorted_unique_and_read_only(
    store: &Store,
    ws: &str,
    second_space_id: &str,
    pm: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"INSERT OR IGNORE INTO space_members(workspace_id,space_id,account_id,joined)
         SELECT ?1,?2,id,1 FROM accounts WHERE handle='upx';"#,
            params![ws, second_space_id],
        )?;
        c.execute(
            r#"INSERT OR IGNORE INTO space_members(workspace_id,space_id,account_id,role,joined)
         SELECT ?1,?2,account_id,'bot',1 FROM connections WHERE id=?3;"#,
            params![ws, second_space_id, pm],
        )?;
        c.execute_batch(r#"UPDATE spaces SET member_count=999 WHERE kind='dm';"#)?;
        Ok(())
    })
}

pub fn set_space_has_external_value_1(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE spaces SET has_external=1 WHERE external_id='C_OPS'"#)?;
        Ok(())
    })
}

pub fn set_space_identity_left_at_value_1(store: &Store, space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE space_identities SET left_at = 1 WHERE space_id = ?1"#,
            params![space_id],
        )?;
        Ok(())
    })
}

pub fn clear_space_connection_id(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE spaces SET connection_id = NULL, status = 'left' WHERE id = ?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_space_member_count_value_0(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE spaces SET member_count = 0 WHERE external_id = 'D_ME'"#)?;
        Ok(())
    })
}

pub fn set_space_member_count(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(
            r#"UPDATE spaces SET member_count = member_count + 3 WHERE external_id = 'G_C'"#,
        )?;
        Ok(())
    })
}

pub fn set_space_member_count_for_an_incomplete_member_list_widens_no(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(
            r#"UPDATE spaces SET member_count = member_count + 2 WHERE external_id = 'G_I'"#,
        )?;
        Ok(())
    })
}

pub fn set_space_member_count_value_0_for_a_group_without_a_member_count_gets(
    store: &Store,
) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE spaces SET member_count = 0 WHERE external_id = 'G_S'"#)?;
        Ok(())
    })
}

pub fn remove_space_member_for_a_group_without_a_member_count_gets(
    store: &Store,
    id: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"DELETE FROM space_members WHERE space_id = (SELECT id FROM spaces WHERE external_id = 'G_S') AND account_id <> (SELECT account_id FROM connections WHERE id = ?1)"#, params![id])?;
        Ok(())
    })
}

pub fn set_space_settings_disclosure_never(store: &Store, space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE space_settings SET disclosure = 'never' WHERE space_id = ?1"#,
            params![space_id],
        )?;
        Ok(())
    })
}

pub fn set_space_settings_disclosure_never_for_steve_im_read_reads_through_the_ada(
    store: &Store,
    space_id: &str,
) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            r#"UPDATE space_settings SET disclosure = 'never' WHERE space_id = ?1"#,
            params![space_id],
        )?;
        Ok(())
    })
}

pub fn set_space_status_archived(store: &Store, con: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE spaces SET status = 'archived' WHERE id = (SELECT min(id) FROM spaces WHERE connection_id = ?1)"#, params![con])?;
        Ok(())
    })
}

pub fn set_space_name_for_new_group_decision_quiet_block_and(
    store: &Store,
    c_space_id: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE spaces SET name = '线上告警通知' WHERE id = ?1"#,
            params![c_space_id],
        )?;
        Ok(())
    })
}

pub fn seed_space_preference(store: &Store, space: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO space_prefs (space_id, workspace_id, reply_as_you, updated) VALUES (?1, 'default', 'record', 1)
           ON CONFLICT (space_id) DO UPDATE SET reply_as_you = 'record'"#, params![space])?;
        Ok(())
    })
}

pub fn set_space_preference_reply_as_you_off(store: &Store, space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE space_prefs SET reply_as_you = 'off' WHERE space_id = ?1"#,
            params![space_id],
        )?;
        Ok(())
    })
}

pub fn set_space_preference_reply_as_you_draft(store: &Store, space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE space_prefs SET reply_as_you = 'draft' WHERE space_id = ?1"#,
            params![space_id],
        )?;
        Ok(())
    })
}

pub fn set_space_preference_attention_quiet(store: &Store, space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE space_prefs SET attention = 'quiet' WHERE space_id = ?1"#,
            params![space_id],
        )?;
        Ok(())
    })
}

pub fn seed_space_preference_for_a_bots_notice_becomes_an_item_by_it(
    store: &Store,
    space: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO space_prefs (space_id, workspace_id, bot_notices, updated) VALUES (?1, 'default', 'record', 1)
           ON CONFLICT (space_id) DO UPDATE SET bot_notices = 'record'"#, params![space])?;
        Ok(())
    })
}

pub fn set_space_preference_bot_notices_item(store: &Store, space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE space_prefs SET bot_notices = 'item' WHERE space_id = ?1"#,
            params![space_id],
        )?;
        Ok(())
    })
}

pub fn seed_read_views_are_bound_to_one_connection(store: &Store, ws: &str) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute(r#"INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated) VALUES
           ('sp_all', ?1, 'test', 'con_bot', 'group', 'oc_all', '全收', 'active', 1, 1),
           ('sp_at', ?1, 'test', 'con_bot2', 'group', 'oc_at', '只收提到', 'active', 1, 1),
           ('sp_mine', ?1, 'test', 'con_me', 'group', 'oc_mine', '我的', 'active', 1, 1);"#, params![ws])?;
        c.execute(r#"INSERT INTO space_identities (space_id, connection_id, workspace_id, joined) VALUES
           ('sp_all', 'con_bot', ?1, 1), ('sp_at', 'con_bot2', ?1, 1), ('sp_mine', 'con_me', ?1, 1);"#, params![ws])?;
        c.execute(r#"INSERT INTO chan_messages (id, workspace_id, space_id, external_id, thread_key, direction, sender_account_id, text, status, created)
           VALUES ('cm_1', ?1, 'sp_all', 'om_1', 'om_1', 'in', 'acc_con_bot2', '你好', 'received', 1700000000123)"#, params![ws])?;
        Ok(())
    })
}
pub fn seed_cleanup_spaces(
    store: &Store,
    prefix: &str,
    connection: Option<&str>,
    account: &str,
) -> Result<()> {
    let p = prefix.replace('\'', "''");
    let acc = account.replace('\'', "''");
    let conn = connection
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .unwrap_or_else(|| "NULL".into());
    project_write(store, |c| {
        c.execute_batch(&format!(
            "INSERT INTO parties (id, workspace_id, type, name, contact_kind, source, created_by, created, updated) VALUES
               ('{p}ann', 'default', 'contact', 'Ann', 'unconfirmed', 'identify', 'usr_dev', 1, 1),
               ('{p}bob', 'default', 'contact', 'Bob', NULL, 'manual', 'usr_dev', 1, 1),
               ('{p}cat', 'default', 'contact', 'Cat', 'unconfirmed', 'identify', 'usr_dev', 1, 1);
             INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES
               ('{p}a_ann', 'default', 'feishu', 'feishu', '{p}ann', 'personal', '{p}on_ann', 'Ann', 'identify', 1, 1),
               ('{p}a_bob', 'default', 'feishu', 'feishu', '{p}bob', 'personal', '{p}on_bob', 'Bob', 'identify', 1, 1),
               ('{p}a_cat', 'default', 'feishu', 'feishu', '{p}cat', 'personal', '{p}on_cat', 'Cat', 'identify', 1, 1);
             INSERT INTO account_handles (workspace_id, family, handle, account_id, created) VALUES
               ('default', 'feishu', '{p}on_ann', '{p}a_ann', 1);
             INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES
               ('default', 'party', '{p}ann', 'works_at', 'party', '{p}bob', 'infer', 1);
             INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated, account_id) VALUES
               ('{p}sp_a', 'default', 'feishu', {conn}, 'group', '{p}oc_a', '客户群', 'active', 1, 1, '{acc}'),
               ('{p}sp_b', 'default', 'feishu', {conn}, 'group', '{p}oc_b', '项目群', 'active', 1, 1, '{acc}'),
               ('{p}sp_c', 'default', 'feishu', {conn}, 'group', '{p}oc_c', '空群', 'active', 1, 1, '{acc}');
             INSERT INTO space_members (workspace_id, space_id, account_id, joined) VALUES
               ('default', '{p}sp_a', '{p}a_ann', 1), ('default', '{p}sp_a', '{p}a_bob', 1), ('default', '{p}sp_a', '{p}a_cat', 1),
               ('default', '{p}sp_b', '{p}a_cat', 1);
             INSERT INTO chan_messages (id, workspace_id, space_id, direction, sender_account_id, sender_party_id, text, attachments, work_item_id, created) VALUES
               ('{p}m1', 'default', '{p}sp_a', 'in', '{p}a_ann', '{p}ann', '合同下周签', '[{{\"file\":\"fil_1\"}}]', NULL, 1),
               ('{p}m2', 'default', '{p}sp_a', 'in', '{p}a_bob', '{p}bob', '好的', '[]', NULL, 2),
               ('{p}m3', 'default', '{p}sp_b', 'in', '{p}a_cat', '{p}cat', '帮我跟进报价', '[]', 'it_1', 3),
               ('{p}m4', 'default', '{p}sp_b', 'in', '{p}a_cat', '{p}cat', '顺便问下', '[]', NULL, 4);
             INSERT INTO ext_events (id, workspace_id, connection_id, source, type, dedupe_key, space_id, message_id, occurred, received, summary, payload) VALUES
               ('{p}ev1', 'default', {conn}, 'im', 'im.message', '{p}d1', '{p}sp_a', '{p}m1', 1, 1, 'Ann：合同下周签', '{{\"text\":\"合同下周签\"}}');"
        ))?;
        Ok(())
    })
}

pub fn attention_accounts(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO parties(id,workspace_id,type,name,created_by,created,updated) VALUES('org','default','organization','Org','usr_dev',1,1);
        INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated) VALUES('mine','default','person','Me','usr_dev','usr_dev',1,1);
        INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated) VALUES
          ('shared','default','feishu','feishu_app','org','app_bot','shared','shared','connect',1,1),
          ('private','default','feishu','feishu_user','mine','personal','me','me','connect',1,1);
        INSERT INTO connections(id,workspace_id,account_id,adapter,auth,caps,created_by,created,updated) VALUES
          ('bot','default','shared','feishu_app','none','[\"msg.receive\"]','usr_dev',1,1),
          ('own','default','private','feishu_user','none','[\"msg.receive\"]','usr_dev',1,1);
        INSERT INTO channel_settings(connection_id,workspace_id,mode,rules,updated) VALUES('bot','default','bot','{}',1),('own','default','self','{}',1);")?;
        Ok(())
    })
}

pub fn attention_preferences(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO space_prefs(space_id,workspace_id,attention,updated) VALUES('dm','default','quiet',1),('blocked','default','blocked',1);
          INSERT INTO space_settings(space_id,workspace_id,muted,disclosure,updated) VALUES('dm','default',1,'{}',1);
          INSERT INTO conversation_preferences(workspace_id,user_id,space_id,closed) VALUES('default','usr_dev','closed',1);
          UPDATE spaces SET status='left' WHERE id='left';")?;
        Ok(())
    })
}

pub fn keep_raw_for_30_days(store: &Store) -> Result<()> {
    store.with_conn(|c| {
            c.execute("INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'keepRaw', '\"30\"', 1)", [])?;
            Ok(())
        })
}

pub fn detach_fixture_space(store: &Store, space: &str) -> Result<()> {
    store.with_conn(|c| {
            c.execute(
                "UPDATE spaces SET status = 'left', account_id = (SELECT account_id FROM connections WHERE id = connection_id), connection_id = NULL WHERE id = ?",
                [space],
            )?;
            // what ChannelUsage does to the identities
            c.execute(
                "UPDATE space_identities SET left_at = 1 WHERE space_id = ?",
                [space],
            )?;
            Ok(())
        })
}

pub fn assign_missing_fixture_account(store: &Store, space: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE spaces SET account_id = 'acc_gone' WHERE id = ?",
            [space],
        )?;
        Ok(())
    })
}

pub fn revoke_connection_during_send(store: &Store, id: &str, connection: &str) -> Result<()> {
    store.transaction(|tx| {
            tx.execute(
                "UPDATE spaces SET status = 'left', connection_id = NULL WHERE id = (SELECT space_id FROM chan_messages WHERE id = ?)",
                [id],
            )?;
            tx.execute("UPDATE connections SET status = 'revoked' WHERE id = ?", [connection])?;
            Ok(())
        })
}

pub fn detach_connection_spaces(store: &Store, connection: &str) -> Result<()> {
    store.with_conn(|c| {
            c.execute(
                "UPDATE spaces SET status = 'left', account_id = (SELECT account_id FROM connections WHERE id = ?1), connection_id = NULL WHERE connection_id = ?1",
                [connection],
            )?;
            // what ChannelUsage does to the identities
            c.execute(
                "UPDATE space_identities SET left_at = 1 WHERE connection_id = ?1",
                [connection],
            )?;
            Ok(())
        })
}

pub fn clear_space_settings(store: &Store, space: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute("DELETE FROM space_settings WHERE space_id = ?", [space])?;
        Ok(())
    })
}

pub fn conversation_accounts(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('org', 'default', 'organization', 'Org', 'usr_dev', 1, 1);
        INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated) VALUES ('mine', 'default', 'person', 'Me', 'usr_dev', 'usr_dev', 1, 1);
        INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES ('shared', 'default', 'feishu', 'feishu_app', 'org', 'app_bot', 'shared', 'shared', 'connect', 1, 1), ('private', 'default', 'feishu', 'feishu_user', 'mine', 'personal', 'me', 'me', 'connect', 1, 1);")?;
        Ok(())
    })
}

pub fn conversation_with_turn(
    store: &Store,
    id: &str,
    ws: &str,
    account: Option<&str>,
    created: i64,
) -> Result<()> {
    store.with_conn(|c| {
        c.execute("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, account_id, created, updated) VALUES (?1, ?2, 'feishu', 'group', ?1, ?1, ?3, 1, 1)", params![id, ws, account])?;
        c.execute("INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES (?1, ?2, ?3, 'me', 'preserved message', ?4)", params![format!("msg_{id}"), ws, format!("chan:{id}"), created])?;
        Ok(())
    })
}

pub fn connect_reply_fixtures(store: &Store, ws: &str, user: &str) -> Result<()> {
    store.with_conn(|c| {
            c.execute("INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
                VALUES ('test-channel', ?1, 'shared', 'feishu_app', 'none', ?2, 1, 1)", params![ws, user])?;
            c.execute(
                "UPDATE spaces SET connection_id = 'test-channel' WHERE id LIKE 'replied-%'",
                [],
            )?;
            Ok(())
        })
}

pub fn leave_space_identity(store: &Store, space: &str, connection: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE space_identities SET left_at=1 WHERE space_id=? AND connection_id=?",
            rusqlite::params![space, connection],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn leave_external_chat(store: &Store, chat: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE spaces SET status = 'left' WHERE external_id = ?",
            [chat],
        )?;
        Ok(())
    })
}

pub fn enable_fixture_collaboration(store: &Store, space: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE space_settings SET collab=1 WHERE space_id=?",
            [space],
        )?;
        Ok(())
    })
}

pub fn seed_attention_space(store: &Store, id: &str, kind: &str, connection: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute("INSERT INTO spaces(id,workspace_id,system_id,connection_id,kind,external_id,name,created,updated) VALUES(?1,'default','feishu',?2,?3,?1,?1,1,1)",params![id,connection,kind])?;
        c.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,created) VALUES(?1,'default',?2,'in','Private body',100)",params![format!("message_{id}"),id])?;
        Ok(())
    })
}

pub fn set_private_receive_identity(store: &Store, ws: &str, owner: &str) -> Result<()> {
    project_write(store, |c| {
        // The groups are visible, but these two messages only reached owner's personal identity.
        c.execute("INSERT INTO space_identities (space_id, connection_id, workspace_id, ordinal, owner_user, joined)
            VALUES ('hidden-sent', 'mine', ?2, 0, ?1, 1), ('hidden-draft', 'mine', ?2, 0, ?1, 1),
                ('hidden-sent', 'shared', ?2, 1, NULL, 1), ('hidden-draft', 'shared', ?2, 1, NULL, 1)", params![owner, ws])?;
        c.execute(
            "UPDATE chan_messages SET seen_by = 1 WHERE id IN ('private-out', 'private-draft')",
            [],
        )?;
        Ok(())
    })
}

pub fn drop_detached_account(store: &Store, space: &str) -> Result<()> {
    store.with_conn(|c| {
        c.pragma_update(None, "foreign_keys", false)?;
        let result = c.execute("DELETE FROM account_handles WHERE account_id IN (SELECT account_id FROM spaces WHERE id = ?)", [space])
            .and_then(|_| c.execute("DELETE FROM accounts WHERE id IN (SELECT account_id FROM spaces WHERE id = ?)", [space]));
        c.pragma_update(None, "foreign_keys", true)?;
        result?;
        Ok(())
    })
}

pub fn quiet_named_space(store: &Store, name: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            "INSERT INTO space_prefs (space_id, workspace_id, attention, updated)
            SELECT id, workspace_id, 'quiet', 0 FROM spaces WHERE name = ?",
            [name],
        )?;
        Ok(())
    })
}
