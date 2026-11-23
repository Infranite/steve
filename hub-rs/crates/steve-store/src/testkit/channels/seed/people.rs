use super::*;

pub fn seed_everything_but_what_is_referenced(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES
           ('a_other', 'default', 'feishu', 'feishu', 'bob', 'app_bot', 'cli_other', 'Other', 'connect', 1, 1);"#)?;
        c.execute_batch(r#"INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated, account_id) VALUES
           ('sp_elsewhere', 'default', 'feishu', NULL, 'group', 'oc_e', '别处', 'left', 1, 1, 'a_other');"#)?;
        c.execute_batch(r#"INSERT INTO space_members (workspace_id, space_id, account_id, joined) VALUES ('default', 'sp_elsewhere', 'a_cat', 1);"#)?;
        Ok(())
    })
}

pub fn seed_disconnected_identity_by_its_account(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO parties (id, workspace_id, type, name, contact_kind, source, created_by, created, updated) VALUES
           ('old_bot', 'default', 'contact', '旧机器人', 'bot', 'connect', 'usr_dev', 1, 1);"#)?;
        c.execute_batch(r#"INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, display_name, source, created, updated) VALUES
           ('acc_old', 'default', 'feishu_app', 'feishu_app', 'old_bot', 'app_bot', 'cli_old', 'cli_old', '旧应用', 'connect', 1, 1);"#)?;
        Ok(())
    })
}

pub fn seed_only_spaces_no_other_live_identity_sees(
    store: &Store,
    text: &str,
    con: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
           VALUES ('acc_you', 'default', 'feishu', 'feishu', 'bob', 'personal', 'on_you', '你', 'connect', 1, 1);"#)?;
        c.execute(r#"INSERT INTO connections (id, workspace_id, account_id, adapter, auth, config, created_by, created, updated)
           VALUES ('con_you', 'default', 'acc_you', 'feishu_user', 'oauth', ?1, 'usr_dev', 1, 1);"#, params![text])?;
        c.execute(
            r#"INSERT INTO space_identities (space_id, connection_id, workspace_id, joined) VALUES
           ('sp_a', ?1, 'default', 1), ('sp_a', 'con_you', 'default', 1),
           ('sp_c', ?1, 'default', 1), ('sp_c', 'con_gone', 'default', 1);"#,
            params![con],
        )?;
        c.execute_batch(
            r#"UPDATE space_identities SET left_at = 2 WHERE connection_id = 'con_gone';"#,
        )?;
        c.execute_batch(r#"INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, status, created, updated) VALUES
           ('sp_d', 'default', 'feishu', NULL, 'group', 'oc_d', '先由旧连接看到', 'active', 1, 1);"#)?;
        c.execute(r#"INSERT INTO space_identities (space_id, connection_id, workspace_id, joined) VALUES ('sp_d', ?1, 'default', 1);"#, params![con])?;
        Ok(())
    })
}

pub fn seed_party_org_acme(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated)
         VALUES ('org_acme', 'default', 'org', 'Acme', '["艾克美"]', 'usr_dev', 1, 1);"#)?;
        Ok(())
    })
}

pub fn seed_party_org_acme_for_inside_the_sources_scope_it_counts(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(
            r#"INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated)
         VALUES ('org_acme', 'default', 'org', 'Acme', 'usr_dev', 1, 1);"#,
        )?;
        Ok(())
    })
}

pub fn seed_untracked_or_as_you_text_to_your_own_people_is_checked_by_the(
    store: &Store,
) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated)
           SELECT 'pt_' || a.id, 'default', 'contact', a.id, 'usr_dev', 1, 1 FROM accounts a
            WHERE a.party_id IS NULL AND a.id IN (SELECT sm.account_id FROM space_members sm JOIN spaces s ON s.id = sm.space_id WHERE s.external_id = 'C_DEV');"#)?;
        c.execute_batch(r#"UPDATE accounts SET party_id = 'pt_' || id
            WHERE party_id IS NULL AND id IN (SELECT sm.account_id FROM space_members sm JOIN spaces s ON s.id = sm.space_id WHERE s.external_id = 'C_DEV');"#)?;
        c.execute_batch(r#"INSERT INTO users (id, workspace_id, subject, name, role, created)
           SELECT 'usr_' || p.id, 'default', 'sub_' || p.id, p.name, 'member', 1 FROM parties p
            WHERE p.user_id IS NULL AND p.id IN (SELECT a.party_id FROM accounts a JOIN space_members sm ON sm.account_id = a.id JOIN spaces s ON s.id = sm.space_id WHERE s.external_id = 'C_DEV');"#)?;
        c.execute_batch(r#"UPDATE parties SET user_id = coalesce(user_id, 'usr_' || id)
            WHERE id IN (SELECT a.party_id FROM accounts a JOIN space_members sm ON sm.account_id = a.id JOIN spaces s ON s.id = sm.space_id WHERE s.external_id = 'C_DEV');"#)?;
        c.execute_batch(r#"UPDATE spaces SET member_count = (SELECT count(*) FROM space_members sm JOIN accounts a ON a.id = sm.account_id
            WHERE sm.space_id = spaces.id AND a.kind NOT IN ('app_bot', 'bot')), has_external = 0 WHERE external_id = 'C_DEV';"#)?;
        Ok(())
    })
}

pub fn remove_workspace_member(store: &Store, ws: &str, me: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"DELETE FROM workspace_members WHERE workspace_id=?1 AND user_id=?2"#,
            params![ws, me],
        )?;
        Ok(())
    })
}

pub fn seed_output_pipeline_manual_preference_wins_but_replies_default_to(
    store: &Store,
    ws: &str,
    me: &str,
    one_message_id: &str,
    two_message_id: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO parties(id,workspace_id,type,name,created_by,created,updated) VALUES ('output-person',?1,'contact','Recipient',?2,1,1);"#, params![ws, me])?;
        c.execute(r#"UPDATE accounts SET party_id='output-person' WHERE id IN (SELECT sender_account_id FROM chan_messages WHERE id IN (?1,?2));"#, params![one_message_id, two_message_id])?;
        c.execute(
            r#"UPDATE chan_messages SET sender_party_id='output-person' WHERE id IN (?1,?2);"#,
            params![one_message_id, two_message_id],
        )?;
        Ok(())
    })
}

pub fn seed_notifications_reject_fake_self_service_and_keep_seen_personal(
    store: &Store,
) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('notification-other','default','notification-other','Other','member',1);"#)?;
        c.execute_batch(r#"INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('default','notification-other','member',1);"#)?;
        Ok(())
    })
}

pub fn remove_workspace_member_for_notifications_revalidate_disable_de(
    store: &Store,
    me: &str,
    ws: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"DELETE FROM workspace_members WHERE user_id=?1 AND workspace_id=?2"#,
            params![me, ws],
        )?;
        Ok(())
    })
}

pub fn clear_party_user_id(store: &Store, p_party_id_unwrap: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE parties SET user_id=NULL WHERE id=?1"#,
            params![p_party_id_unwrap],
        )?;
        Ok(())
    })
}

pub fn seed_notifications_claim_then_completed_policy_put_or_dnd_prevents(
    store: &Store,
    me: &str,
    party: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE parties SET user_id=NULL WHERE user_id=?1;"#,
            params![me],
        )?;
        c.execute(
            r#"UPDATE parties SET user_id=?1 WHERE id=?2;"#,
            params![me, party],
        )?;
        Ok(())
    })
}

pub fn seed_user_u_li(store: &Store, ws: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('u_li', ?1, 'li', '李雷', 'member', 1)"#, params![ws])?;
        Ok(())
    })
}

pub fn seed_workspace_member(store: &Store, ws: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?1, 'u_li', 'member', 1)"#, params![ws])?;
        Ok(())
    })
}

pub fn set_party_user_id_u_li(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE parties SET user_id = 'u_li' WHERE id = (SELECT sender_party_id FROM chan_messages WHERE external_id = 'w2')"#)?;
        Ok(())
    })
}

pub fn set_party_user_id_u_li_for_a_trigger_word_a_bot_receives_is_no(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE parties SET user_id = 'u_li' WHERE id = (SELECT sender_party_id FROM chan_messages WHERE external_id = 'o1')"#)?;
        Ok(())
    })
}

pub fn seed_party_org_acme_for_the_conversation_card_says_who_asks(
    store: &Store,
    ws: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO parties (id, workspace_id, type, name, org_kind, created_by, created, updated) VALUES ('org_acme', ?1, 'org', 'ACME 客户', 'customer', 'u', 1, 1)"#, params![ws])?;
        Ok(())
    })
}

pub fn seed_link(store: &Store, ws: &str, party: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES (?1, 'party', ?2, 'works_at', 'party', 'org_acme', 'manual', 1)"#, params![ws, party])?;
        Ok(())
    })
}

pub fn set_party_visibility_private(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE parties SET visibility = 'private' WHERE id = 'org_acme'"#)?;
        Ok(())
    })
}

pub fn set_link_props(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(
            r#"UPDATE links SET props = '{"to":"2025-01"}' WHERE object_id = 'org_acme'"#,
        )?;
        Ok(())
    })
}

pub fn set_party_user_id_u_li_for_an_incomplete_member_list_widens_no(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE parties SET user_id = 'u_li' WHERE id = (SELECT sender_party_id FROM chan_messages WHERE external_id = 'i1')"#)?;
        Ok(())
    })
}

pub fn seed_party_org_acme_for_make_external(store: &Store, ws: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT OR IGNORE INTO parties (id, workspace_id, type, name, org_kind, created_by, created, updated) VALUES ('org_acme', ?1, 'org', 'ACME 客户', 'customer', 'u', 1, 1)"#, params![ws])?;
        Ok(())
    })
}

pub fn remove_link(store: &Store, subject_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"DELETE FROM links WHERE subject_id = ?1 AND predicate = 'works_at'"#,
            params![subject_id],
        )?;
        Ok(())
    })
}

pub fn set_account_verified_value_0(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"UPDATE accounts SET verified = 0 WHERE id = (SELECT account_id FROM connections WHERE id = ?1)"#, params![id])?;
        Ok(())
    })
}

pub fn set_party_aliases_lee_corp_example(store: &Store, id: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            r#"UPDATE parties SET aliases = '["Lee@Corp.example"]' WHERE id = ?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_party_status_merged(store: &Store, merged_into: &str, id: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            r#"UPDATE parties SET status = 'merged', merged_into = ?1 WHERE id = ?2"#,
            params![merged_into, id],
        )?;
        Ok(())
    })
}

pub fn set_party_rels_customer(store: &Store, cust_party: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE parties SET rels = '["customer"]' WHERE id = ?1"#,
            params![cust_party],
        )?;
        Ok(())
    })
}

pub fn seed_link_for_space_members_paged_with_contacts_a(
    store: &Store,
    alice_party: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, props, source, created)
         SELECT 'default', 'party', ?1, 'works_at', 'party', id, '{}', 'sync', 1 FROM parties
          WHERE workspace_id = 'default' AND type = 'org' AND org_kind = 'self'"#, params![alice_party])?;
        Ok(())
    })
}

pub fn set_party_important_value_1(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE parties SET important = 1 WHERE id = ?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_account_party_id(store: &Store, party_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE accounts SET party_id = ?1 WHERE handle = 'uself'"#,
            params![party_id],
        )?;
        Ok(())
    })
}

pub fn set_bot_model_fixture_model(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE bots SET model='fixture-model' WHERE id='ops'"#)?;
        Ok(())
    })
}

pub fn set_account_display_name(store: &Store, account: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE accounts SET display_name = '7300000000000000001' WHERE id = ?1"#,
            params![account],
        )?;
        Ok(())
    })
}

pub fn set_bot_role_for_your_own_account_writes_like_you_not_l(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE bots SET role = '你的助手：理解需求、分派、汇报', persona = '沉稳、简洁，先说结论',
                         rules = '建事项交给合适的 Bot', model = 'm-pinned' WHERE id = 'steve';"#)?;
        Ok(())
    })
}

pub fn seed_fast(
    store: &Store,
    text: &str,
    ws: &str,
    text2: &str,
    text3: &str,
    id: &str,
    adapter: &str,
    text4: &str,
    config: &str,
) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute(r#"INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
               VALUES (?1, ?2, 'test', 'other', ?3, 'bot', ?4, ?4, 'connect', 1, 1);"#, params![text, ws, text2, text3])?;
        c.execute(r#"INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, credential_item, config, created_by, created, updated)
               VALUES (?1, ?2, ?3, ?4, 'credential', '["msg.receive"]', ?5, ?6, 'usr_dev', 1, 1)"#, params![id, ws, text, adapter, text4, config])?;
        Ok(())
    })
}

pub fn seed_read_views_are_bound_to_one_connection(store: &Store, ws: &str) -> Result<()> {
    unchecked_relations(store, |c| {
        c.execute(r#"INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES
           ('acc_f', ?1, 'feishu', 'feishu_user', 'pty_ann', 'personal', 'on_ann', 'on_ann', 'chat', 1, 1),
           ('acc_m', ?1, 'mail', 'email', 'pty_ann', 'work_email', 'ann@example.com', 'ann@example.com', 'chat', 2, 2);"#, params![ws])?;
        c.execute(r#"INSERT INTO account_handles (workspace_id, family, handle, account_id, connection_id, created) VALUES
           (?1, 'feishu_user', 'on_ann', 'acc_f', NULL, 1),
           (?1, 'feishu_user', 'ou_ann_bot2', 'acc_f', 'con_bot2', 2),
           (?1, 'feishu_user', 'ou_ann_other', 'acc_f', 'con_x', 3),
           (?1, 'email', 'ann@example.com', 'acc_m', NULL, 2)"#, params![ws])?;
        Ok(())
    })
}

#[derive(Clone, Copy)]
pub struct Bot<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub name: &'a str,
    pub main: bool,
    pub persona: &'a str,
    pub role: &'a str,
    pub rules: &'a str,
    pub model: Option<&'a str>,
    pub joined: i64,
}

impl Default for Bot<'static> {
    fn default() -> Self {
        Self {
            id: "steve",
            ws: "default",
            name: "Steve",
            main: true,
            persona: "",
            role: "",
            rules: "",
            model: None,
            joined: 1,
        }
    }
}

pub const OPS: Bot<'static> = Bot {
    id: "ops",
    ws: "default",
    name: "运维",
    main: false,
    persona: "",
    role: "",
    rules: "",
    model: None,
    joined: 2,
};

pub const ON_CALL: Bot<'static> = Bot {
    persona: "冷静",
    role: "负责告警排查与部署",
    rules: "先查日志再下结论",
    ..OPS
};

pub fn bots(store: &Store, bots: &[Bot<'_>]) -> Result<()> {
    fixture_write(store, |c| {
        for bot in bots {
            c.execute(
                "INSERT INTO bots (id,workspace_id,name,main,harness,persona,role,rules,model,updated)
                 VALUES (?,?,?,?,'claude',?,?,?,?,1)",
                params![bot.id, bot.ws, bot.name, bot.main, bot.persona, bot.role, bot.rules, bot.model],
            )?;
            c.execute(
                "INSERT INTO bot_workspaces (bot_id,workspace_id,created) VALUES (?,?,?)",
                params![bot.id, bot.ws, bot.joined],
            )?;
        }
        Ok(())
    })
}

pub fn ops_tag(store: &Store) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            "INSERT INTO tags (id,workspace_id,name,cat,description,created_by,created)
                   VALUES ('tag_ops','default','运维','领域','','usr_dev',1)",
            [],
        )?;
        Ok(())
    })
}

pub fn seed_ops_team(store: &Store, team: &str, ws: &str, owner: &str) -> Result<()> {
    store.with_conn(|c| {
            c.execute(
                "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES (?, ?, 'team', '运维组', ?, 1, 1)",
                rusqlite::params![team, ws, owner],
            )?;
            Ok(())
        })
}

pub fn confirm_contact_kind(store: &Store, party: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE parties SET contact_kind = 'real' WHERE id = ?",
            [party],
        )?;
        Ok(())
    })
}

pub fn set_user_external_scope(store: &Store, user: &str, space: &str) -> Result<()> {
    store.with_conn(|c| {
        // Replace the scope with a valid current fact, not guessed prose.
        c.execute("UPDATE attr_values SET value = ? WHERE key = 'call_ext_scope' AND party_id IN (SELECT id FROM parties WHERE user_id = ?)", rusqlite::params![serde_json::json!({"kind": "space", "id": space, "name": "Member renamed here"}).to_string(), user])?;
        Ok(())
    })
}

pub fn clear_user_external_name(store: &Store, user: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute("DELETE FROM attr_values WHERE key IN ('call_external', 'call_ext_scope') AND party_id IN (SELECT id FROM parties WHERE user_id = ?)", [user])?;
        Ok(())
    })
}

pub fn unverify_account(store: &Store, account: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute("UPDATE accounts SET verified = 0 WHERE id = ?", [account])?;
        Ok(())
    })
}

pub fn associate_verified_account_with_org(store: &Store, ws: &str, account: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute("UPDATE accounts SET verified = 1, party_id = (SELECT id FROM parties WHERE workspace_id = ?1 AND type = 'org' LIMIT 1) WHERE id = ?2", rusqlite::params![ws, account])?;
        Ok(())
    })
}

pub fn assign_lilei_contact(store: &Store, party: &str, ws: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE accounts SET party_id=? WHERE handle='ulilei' AND workspace_id=?",
            rusqlite::params![party, ws],
        )?;
        Ok(())
    })
}

pub fn rename_ada_at_work(store: &Store, connection: &str) -> Result<usize> {
    store.transaction(|tx| { Ok(tx.execute("UPDATE accounts SET display_name = 'Ada (work)' WHERE id = (SELECT account_id FROM connections WHERE id = ?)", [connection])?) })
}

pub fn replace_feishu_display_name(store: &Store, party: &str, name: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE parties SET name = ? WHERE id = ?",
            rusqlite::params![name, party],
        )?;
        tx.execute(
            "UPDATE accounts SET display_name = ? WHERE party_id = ? AND system_id = 'feishu'",
            rusqlite::params![name, party],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn clear_profile_retry_schedule(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("UPDATE account_profiles SET next_check=NULL", [])?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn confirm_team_member(store: &Store, ws: &str, team: &str, party: &str) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(
            "UPDATE parties SET contact_kind = 'real' WHERE id = ?",
            [party],
        )?;
        c.execute("INSERT INTO team_members (workspace_id, team_id, contact_id, joined) VALUES (?, ?, ?, 1)",
            [ws, team, party])?;
        Ok(())
    })
}

pub fn set_contact_kind(w: &mut crate::W<'_>, party: &str, kind: &str) -> Result<()> {
    w.fixture_connection().execute(
        "UPDATE parties SET contact_kind = ? WHERE id = ?",
        [kind, party],
    )?;
    Ok(())
}

pub fn set_contact_source(w: &mut crate::W<'_>, party: &str, source: &str) -> Result<()> {
    w.fixture_connection().execute(
        "UPDATE parties SET source = ? WHERE id = ?",
        [source, party],
    )?;
    Ok(())
}
