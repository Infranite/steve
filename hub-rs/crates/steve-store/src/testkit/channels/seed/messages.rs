use super::*;

pub fn clear_message_decision_id(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE chan_messages SET decision_id=NULL WHERE id='m3';"#)?;
        Ok(())
    })
}

pub fn set_message_status_pending(store: &Store, message_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET status = 'pending' WHERE id = ?1"#,
            params![message_id],
        )?;
        Ok(())
    })
}

pub fn set_message_text(store: &Store, message_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET text = '偷偷改过的' WHERE id = ?1"#,
            params![message_id],
        )?;
        Ok(())
    })
}

pub fn set_message_status_sent(store: &Store, m: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET status = 'sent', sent_via = NULL WHERE id = ?1"#,
            params![m],
        )?;
        Ok(())
    })
}

pub fn clear_message_external_id(store: &Store, outgoing: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET external_id = NULL WHERE id = ?1"#,
            params![outgoing],
        )?;
        Ok(())
    })
}

pub fn clear_message_basis_id(store: &Store, old: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET basis_id=NULL WHERE id=?1"#,
            params![old],
        )?;
        Ok(())
    })
}

pub fn set_message_basis_id(store: &Store, basis: &str, message: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET basis_id=?1 WHERE id=?2"#,
            params![basis, message],
        )?;
        Ok(())
    })
}

pub fn set_message_composed(store: &Store, old: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET composed='{"style_samples":["旧语气样本"]}' WHERE id=?1"#,
            params![old],
        )?;
        Ok(())
    })
}

pub fn set_message_sent(store: &Store, now: i64) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET sent=?1 WHERE id='contract-direct'"#,
            params![now],
        )?;
        Ok(())
    })
}

pub fn set_message_status_failed(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE chan_messages SET status='failed' WHERE id='contract-direct'"#)?;
        Ok(())
    })
}

pub fn clear_message_external_id_for_echoes_and_automatic_replies_are_no(
    store: &Store,
    id: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET external_id = NULL, status = 'pending' WHERE id = ?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn clear_message_text(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET text = NULL, purged = 9 WHERE id = ?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn set_message_direction_outgoing(store: &Store, id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET direction = 'out', status = 'draft' WHERE id = ?1"#,
            params![id],
        )?;
        Ok(())
    })
}

pub fn seed_message_m_keep(store: &Store, ws: &str, c_space_id: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO chan_messages (id, workspace_id, space_id, direction, text, created) VALUES ('m_keep', ?1, ?2, 'in', '留着', 1)"#, params![ws, c_space_id])?;
        Ok(())
    })
}

pub fn clear_message_text_for_purging_the_message_purges_every_co(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE chan_messages SET text = NULL, attachments = '[]', purged = 1 WHERE external_id IN ('s1', 's2')"#)?;
        Ok(())
    })
}

pub fn seed_message_a1(
    store: &Store,
    ws: &str,
    space: &str,
    lilei: &str,
    created: i64,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id, on_behalf, text, attachments, status, created)
             VALUES ('a1', ?1, ?2, 'a1', 'in', ?3, 'none', '补充一句', '[]', 'received', ?4);"#, params![ws, space, lilei, created])?;
        Ok(())
    })
}

pub fn set_message_created(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch(r#"UPDATE chan_messages SET created = created - 1800000,
                ext_created = CASE WHEN ext_created IS NULL THEN NULL ELSE ext_created - 1800000 END;"#)?;
        Ok(())
    })
}

pub fn seed_your_own_account_writes_like_you_not_like_a_bot(
    store: &Store,
    sent_at: i64,
    decision_at: i64,
    resolved_at: i64,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET created = ?1, status = 'discarded' WHERE id = 'v6';"#,
            params![sent_at],
        )?;
        c.execute(
            r#"UPDATE decisions SET created = ?1, resolved = ?2 WHERE id = 'd_v6';"#,
            params![decision_at, resolved_at],
        )?;
        Ok(())
    })
}

pub fn clear_message_text_for_a_purge_a_removal_or_a_move_while_d(
    store: &Store,
    msg: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET text = NULL, attachments = '[]', purged = 1 WHERE id = ?1"#,
            params![msg],
        )?;
        Ok(())
    })
}

pub fn set_message_space_id(store: &Store, other: &str, msg: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET space_id = ?1 WHERE id = ?2"#,
            params![other, msg],
        )?;
        Ok(())
    })
}

pub fn clear_message_text_for_purged_rows_left_by_an_older_sweep(
    store: &Store,
    msg: &str,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            r#"UPDATE chan_messages SET text = NULL, purged = 5 WHERE id = ?1"#,
            params![msg],
        )?;
        Ok(())
    })
}

pub fn seed_message(store: &Store, id: &str, id2: &str) -> Result<()> {
    project_write(store, |c| {
        c.execute(r#"INSERT INTO chan_messages (id, workspace_id, space_id, thread_key, direction, sender_account_id,
                 sender_party_id, text, status, created, external_id)
             SELECT ?1, workspace_id, space_id, ?1, direction, sender_account_id, sender_party_id, text, status, created,
                 ?1 FROM chan_messages WHERE id = ?2"#, params![id, id2])?;
        Ok(())
    })
}

pub fn set_message_attachments_for_a_resource_is_fetched_only_by_the_adap(
    store: &Store,
    msg: &str,
) -> Result<()> {
    fixture_write(store, |c| {
        c.execute(r#"UPDATE chan_messages SET attachments = json_set(attachments, '$[0].adapter', 'other_side') WHERE id = ?1"#, params![msg])?;
        Ok(())
    })
}

pub fn seed_outbound_state(
    store: &Store,
    id: &str,
    space: &str,
    status: &str,
    at: i64,
) -> Result<()> {
    store.write(|c| {
                c.conn.execute(
                    "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, status, created, sent)
                     VALUES (?1, 'default', ?2, ?3, 'out', '好的', ?4, ?5, ?5)",
                    params![id, space, format!("fm_{id}"), status, at],
                )?;
                steve_store_sqlite::digest::sync_message(c.conn, steve_store_sqlite::digest::Source::ChanMessage, id, None)?;
                Ok(())
            })
}

pub fn seed_persona_reply(store: &Store, space: &str, account: &str) -> Result<()> {
    store.write(|c| {
            c.conn.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id, on_behalf, text, status, created, sent)
                 VALUES ('cm_out1', 'default', ?, 'fm_1', 'out', ?, 'persona', '已处理', 'sent', 1, 1)",
                params![space, account],
            )?;
                steve_store_sqlite::digest::sync_message(c.conn, steve_store_sqlite::digest::Source::ChanMessage, "cm_out1", None)?;
            Ok(())
        })
}

pub fn seed_recent_outbound(store: &Store, space: &str, now: i64) -> Result<()> {
    store.write(|c| {
            for i in 0..50 {
                c.conn.execute(
                    "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, status, created, sent)
                     VALUES (?, 'default', ?, ?, 'out', 'x', 'sent', ?, ?)",
                    params![format!("cm_o{i}"), space, format!("fm_{i}"), now, now],
                )?;
                steve_store_sqlite::digest::sync_message(c.conn, steve_store_sqlite::digest::Source::ChanMessage, &format!("cm_o{i}"), None)?;
            }
            Ok(())
        })
}

pub fn mark_message_purged(store: &Store, id: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE chan_messages SET purged=1 WHERE id=?", [id])?;
        Ok(())
    })
}

pub fn clear_purged_message(store: &Store, id: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE chan_messages SET text=NULL,attachments='[]',purged=1 WHERE id=?",
            [id],
        )?;
        Ok(())
    })
}

pub fn forget_seen_message(store: &Store, connection: &str, message: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "DELETE FROM chan_message_seen WHERE connection_id=? AND message_id=?",
            rusqlite::params![connection, message],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn reset_fixture_seen_by(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE chan_messages SET seen_by = 4 WHERE text = '我先看一下'",
            [],
        )?;
        Ok(())
    })
}

pub fn mark_reaction_done(store: &Store, message: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE chan_reactions SET state='done',answer_id=NULL WHERE message_id=?",
            [message],
        )?;
        Ok(())
    })
}

pub fn seed_attention_history(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute_batch("INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,history,sender_party_id,created) VALUES
          ('out','default','dm','out','sent',NULL,NULL,101),
          ('history','default','dm','in','old','sync',NULL,102),
          ('self','default','dm','in','own',NULL,'mine',103);")?;
        Ok(())
    })
}

pub fn seed_live_attention_message(store: &Store) -> Result<()> {
    project_write(store, |c| {
        c.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,created) VALUES('new-live','default','dm','in','New',104)",[])?;
        Ok(())
    })
}

pub fn seed_earlier_bot_reply(store: &Store, ws: &str, space: &str) -> Result<()> {
    project_write(store, |cn| {
        cn.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, status, created, sent)
                 VALUES ('cm_sent1', ?, ?, 'fm_1', 'out', '备份已恢复', 'sent', 1, 1)",
                rusqlite::params![ws, space],
            )?;
        Ok(())
    })
}

pub fn seed_conversation_outbound(
    store: &Store,
    ws: &str,
    space: &str,
    id: &str,
    status: &str,
    approval: Option<(&str, &str)>,
) -> Result<()> {
    project_write(store, |c| {
        let decision = approval.map(|_| format!("decision-{id}"));
        c.execute("INSERT INTO chan_messages (id, workspace_id, space_id, direction, text, status, decision_id, created)
            VALUES (?1, ?2, ?3, 'out', 'reply', ?4, ?5, 2)", params![id, ws, space, status, decision])?;
        if let Some((kind, state)) = approval {
            c.execute("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, requested_by, status, created)
                VALUES (?1, ?2, ?3, 'chan_message', ?4, 'Send?', 'steve', ?5, 2)", params![decision, ws, kind, id, state])?;
        }
        Ok(())
    })
}

pub fn seed_incoming_after_sent(store: &Store, ws: &str) -> Result<()> {
    project_write(store, |c| {
        // Draft-only channel records need not have any internal conversation turns.
        c.execute("DELETE FROM messages WHERE space = 'chan:draft'", [])?;
        // Latest-author filtering would lose a conversation with a reply followed by incoming text.
        c.execute("INSERT INTO chan_messages (id, workspace_id, space_id, direction, text, status, created)
            VALUES ('new-incoming', ?1, 'sent', 'in', 'new incoming', 'received', 20)", [ws])?;
        // Retaining the sending fact does not depend on retaining the message body.
        c.execute(
            "UPDATE chan_messages SET text = NULL, purged = 10 WHERE id = 'out-sent'",
            [],
        )?;
        Ok(())
    })
}

pub fn seed_owner_message(
    store: &Store,
    ws: &str,
    space: &str,
    account: &str,
    party: &str,
    now: i64,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
                    "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id,
                        sender_party_id, text, status, created)
                     VALUES (?, ?, ?, ?, 'in', ?, ?, '我来吧', 'received', ?)",
                    rusqlite::params![
                        crate::uid("cm"),
                        ws,
                        space,
                        format!("elsewhere_{now}"),
                        account,
                        party,
                        now
                    ],
                )?;
        Ok(())
    })
}

pub fn seed_followup_copy(
    store: &Store,
    id: &str,
    like: &str,
    external: &str,
    secs: i64,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id,
                    sender_party_id, text, status, created)
                 SELECT ?, workspace_id, space_id, ?, 'in', sender_account_id, sender_party_id, '还有个事', 'received',
                        created + ?
                   FROM chan_messages WHERE external_id = ? AND direction = 'in'",
                rusqlite::params![id, external, secs * 1000, like],
            )?;
        Ok(())
    })
}

pub fn set_fixture_message_state(
    store: &Store,
    id: &str,
    status: &str,
    sent: Option<i64>,
    created: Option<i64>,
) -> Result<()> {
    project_write(store, |c| {
        c.execute(
            "UPDATE chan_messages SET status=?2,sent=?3,created=coalesce(?4,created) WHERE id=?1",
            rusqlite::params![id, status, sent, created],
        )?;
        Ok(())
    })
}

pub fn set_image_for_retry(store: &Store, message: &str, key: &str) -> Result<()> {
    project_write(store, |c| {
        let attachments = serde_json::json!([{"type":"image", "source":"message", "key":key}]);
        c.execute(
            "UPDATE chan_messages SET text='', attachments=?, purged=NULL WHERE id=?",
            params![attachments.to_string(), message],
        )?;
        Ok(())
    })
}

pub struct HistoryMessage<'a> {
    pub id: &'a str,
    pub direction: &'a str,
    pub status: &'a str,
    pub account: &'a str,
    pub text: &'a str,
    pub attachments: &'a str,
    pub created: i64,
    pub sent: Option<i64>,
}

pub fn seed_history(
    store: &Store,
    ws: &str,
    space: &str,
    history: &[HistoryMessage<'_>],
) -> Result<()> {
    project_write(store, |c| {
        for m in history {
            c.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id,
                direction, sender_account_id, on_behalf, text, attachments, status, created, sent)
                VALUES (?1, ?2, ?3, ?1, ?4, ?5, 'none', ?6, ?7, ?8, ?9, ?10)",
                params![
                    m.id,
                    ws,
                    space,
                    m.direction,
                    m.account,
                    m.text,
                    m.attachments,
                    m.status,
                    m.created,
                    m.sent
                ],
            )?;
        }
        Ok(())
    })
}

pub struct Verdict<'a> {
    pub id: &'a str,
    pub space: &'a str,
    pub draft: &'a str,
    pub result: &'a str,
}

pub fn seed_verdicts(
    store: &Store,
    ws: &str,
    subject_type: &str,
    verdicts: &[Verdict<'_>],
) -> Result<()> {
    project_write(store, |c| {
        for v in verdicts {
            c.execute("INSERT INTO chan_messages (id, workspace_id, space_id, direction, on_behalf, text, status, created)
                VALUES (?, ?, ?, 'out', 'none', ?, 'sent', 1)", params![v.id, ws, v.space, v.draft])?;
            c.execute(
                "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id,
                question, draft, options, status, result, requested_by, created, resolved)
                VALUES (?, ?, 'approve', ?, ?, 'q', ?, '[]', 'resolved', ?, 'steve', 1, 2)",
                params![
                    format!("d_{}", v.id),
                    ws,
                    subject_type,
                    v.id,
                    v.draft,
                    v.result
                ],
            )?;
        }
        Ok(())
    })
}
