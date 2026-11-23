use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_store_sqlite::digest::{self, Source};

use crate::{Result, Store, W};

pub fn audit(store: &Store) -> Result<()> {
    store.reader(|c| {
        let tx = c.unchecked_transaction()?;
        let workspaces = tx.prepare(
            "SELECT workspace_id FROM chan_messages UNION SELECT workspace_id FROM mail_messages
             UNION SELECT workspace_id FROM digest_pending UNION SELECT workspace_id FROM digest_pending_convs",
        )?.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for ws in workspaces {
            let mismatches = digest::verify_pending(&tx, &ws)?;
            assert!(mismatches.is_empty(), "digest pending mismatch in workspace {ws}: {mismatches:?}");
        }
        Ok(())
    })
}

pub struct ChannelInput<'a> {
    pub id: &'a str,
    pub space: &'a str,
    pub at: i64,
    pub reply: Option<&'a str>,
    pub thread: Option<&'a str>,
    pub text: &'a str,
    pub mentions: &'a Value,
}

pub fn seed_message(w: &mut W<'_>, m: ChannelInput<'_>) -> Result<()> {
    let tx = w.legacy();
    tx.execute("INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id,
        text, mentions, reply_to, thread_key, created) VALUES (?, 'default', ?, ?, 'in', 'alice', ?, ?, ?, ?, ?)",
        params![m.id, m.space, m.id, m.text, m.mentions.to_string(), m.reply, m.thread, m.at])?;
    digest::sync_message(&tx, Source::ChanMessage, m.id, None)?;
    Ok(())
}

pub fn seed_segment(
    w: &mut W<'_>,
    matter: &str,
    id: &str,
    space: &str,
    chains: &Value,
    now: i64,
) -> Result<()> {
    w.legacy().execute("INSERT INTO digest_segments (id, workspace_id, packet_id, conv, first_pos, last_pos,
        first_id, last_id, first_at, last_at, summary, matter_id, parties, chains, writer, created)
        VALUES (?, 'default', 'fixture', ?, 1, 1, 'cm1', 'cm1', ?, ?, 'deploy', ?, '[\"alice\",\"bob\"]', ?, 'fixture', ?)",
        params![id, format!("space:{space}"), now - 100, now - 100, matter, chains.to_string(), now])?;
    Ok(())
}

pub fn change_sender(store: &Store, id: Option<&str>) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE chan_messages SET sender_account_id = 'bot_acc', sender_party_id = 'bob', mentions = '[]'
             WHERE ?1 IS NULL OR id = ?1",
            [id],
        )?;
        Ok(())
    })
}

pub fn set_batching(w: &mut W<'_>, ws: &str, settings: &Value, now: i64) -> Result<()> {
    w.legacy().execute(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, 'digest', ?, ?)
         ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value,updated=excluded.updated",
        params![ws, settings.to_string(), now],
    )?;
    Ok(())
}

pub fn pending_positions(store: &Store, ws: &str) -> Result<(i64, Option<i64>, Option<i64>)> {
    store.reader(|c| {
        Ok(c.query_row(
        "SELECT count(*), min(position), max(position) FROM digest_pending WHERE workspace_id = ?",
        [ws],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
    })
}

pub fn receipt_count(store: &Store, ws: &str) -> Result<i64> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM digest_receipts WHERE workspace_id = ?",
            [ws],
            |r| r.get(0),
        )?)
    })
}

pub fn receipt_count_in(w: &mut W<'_>, ws: &str) -> Result<i64> {
    Ok(w.legacy().query_row(
        "SELECT count(*) FROM digest_receipts WHERE workspace_id = ?",
        [ws],
        |r| r.get(0),
    )?)
}

pub fn message_status(w: &mut W<'_>, id: &str, status: &str) -> Result<()> {
    let tx = w.legacy();
    let old = Source::ChanMessage.scope(&tx, id)?;
    tx.execute(
        "UPDATE chan_messages SET status = ? WHERE id = ?",
        params![status, id],
    )?;
    digest::sync_message(&tx, Source::ChanMessage, id, old.as_ref())?;
    Ok(())
}

pub fn move_message(w: &mut W<'_>, id: &str, space: &str, at: i64) -> Result<()> {
    let tx = w.legacy();
    let old = Source::ChanMessage.scope(&tx, id)?;
    tx.execute(
        "UPDATE chan_messages SET space_id = ?, created = ? WHERE id = ?",
        params![space, at, id],
    )?;
    digest::sync_message(&tx, Source::ChanMessage, id, old.as_ref())?;
    Ok(())
}

pub fn remove_message(w: &mut W<'_>, id: &str) -> Result<()> {
    let tx = w.legacy();
    let old = Source::ChanMessage.scope(&tx, id)?;
    tx.execute("DELETE FROM chan_messages WHERE id = ?", [id])?;
    digest::remove_message(&tx, old.into_iter().map(|scope| (scope, id.to_owned())))?;
    Ok(())
}

pub fn set_packet_candidates(store: &Store, id: &str, candidates: &Value) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE digest_packets SET candidates = ? WHERE id = ?",
            params![candidates.to_string(), id],
        )?;
        Ok(())
    })
}

pub fn summary_depth(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE digest_packets SET facts = json_set(facts, '$.conv.depth', 'summary')",
            [],
        )?;
        Ok(())
    })
}

pub fn seed_workspace(w: &mut W<'_>, id: &str) -> Result<()> {
    w.legacy().execute(
        "INSERT INTO workspaces (id,name,created) VALUES (?, 'fixture', 1)",
        [id],
    )?;
    Ok(())
}

pub fn packet_ids(store: &Store, state: &str) -> Result<Vec<String>> {
    store.reader(|c| {
        Ok(
            c.prepare("SELECT id FROM digest_packets WHERE state = ? ORDER BY id")?
                .query_map([state], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?,
        )
    })
}

pub fn exhaust_packet(store: &Store, id: &str, attempts: i64) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE digest_packets SET attempts = ? WHERE id = ?",
            params![attempts, id],
        )?;
        Ok(())
    })
}

pub fn supersedes(store: &Store, mem: &str) -> Result<Option<String>> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT supersedes FROM mem_candidates WHERE mem_id = ?",
            [mem],
            |r| r.get(0),
        )
        .optional()?
        .flatten())
    })
}

pub fn conversations(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
            VALUES ('s1', 'default', 'test', 'group', 'g1', 'Project', 1, 1), ('s2', 'default', 'test', 'dm', 'g2', 'DM', 1, 1),
                   ('foreign', 'w2', 'test', 'group', 'g3', 'Foreign', 1, 1);
            INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated)
            VALUES ('me', 'default', 'person', 'Owner', 'usr_dev', 'usr_dev', 1, 1),
                   ('alice', 'default', 'person', 'Alice', NULL, 'usr_dev', 1, 1),
                   ('bob', 'default', 'person', 'Bob', NULL, 'usr_dev', 1, 1);
            INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('owner_acc', 'default', 'test', 'other', 'me', 'personal', 'owner', 'Owner', 'me', 1, 1);")?;
        Ok(())
    })
}

pub fn mail_thread(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch("INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
            VALUES ('mailconn', 'default', 'owner_acc', 'fixture', 'none', 'usr_dev', 1, 1);
            INSERT INTO mail_threads (id, workspace_id, connection_id, thread_key, subject, last_at, created_by, created, updated)
            VALUES ('mt1', 'default', 'mailconn', 'thread', 'Contract', 1, 'usr_dev', 1, 1);
            INSERT INTO mail_messages (id, workspace_id, connection_id, thread_id, ext_id, message_id, direction, from_addr,
                from_party_id, from_name, body_new, date, created, to_addrs)
            VALUES ('mail1', 'default', 'mailconn', 'mt1', 'ext1', '<root>', 'in', 'alice@example.com',
                'alice', 'Alice', 'first', 10, 10, '[{\"account_id\":\"owner_acc\",\"name\":\"Owner\"}]'),
                ('mail2', 'default', 'mailconn', 'mt1', 'ext2', '<reply>', 'in', 'alice@example.com',
                'alice', 'Alice', 'second', 10, 10, '[]');
            UPDATE mail_messages SET in_reply_to = '<root>', refs = '[\"<root>\"]' WHERE id = 'mail2';
            INSERT INTO mail_attachments (id, workspace_id, message_id, part, name, mime, size, created)
            VALUES ('attachment', 'default', 'mail2', '1.2', 'contract.pdf', 'application/pdf', 1024, 10);")?;
        for id in ["mail1", "mail2"] {
            digest::sync_message(tx, Source::MailMessage, id, None)?;
        }
        Ok(())
    })
}

pub fn alert_sender(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('bot_acc', 'default', 'test', 'other', 'bob', 'bot', 'bot', 'Bot', 'me', 1, 1);")?;
        Ok(())
    })
}

pub fn other_account(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated)
            VALUES ('other_person', 'default', 'person', 'Other', 'usr_other', 'usr_other', 1, 1);
            INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
            VALUES ('other_acc', 'default', 'test', 'other', 'other_person', 'personal', 'other', 'Other', 'me', 1, 1);
            UPDATE spaces SET account_id = 'other_acc' WHERE id = 's2';")?;
        Ok(())
    })
}

pub fn ts5_space(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated) VALUES('ts5-space','default','test','group','ts5','TS5',1,1);
        INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated) VALUES('ts5-owner','default','person','Owner','usr_dev','usr_dev',1,1),('ts5-bot','default','person','',NULL,'usr_dev',1,1);
        INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,display_name,source,created,updated) VALUES('ts5-bot-account','default','test','other','ts5-bot','app_bot','bot','fallback','Persona','fixture',1,1); ")?;
        Ok(())
    })
}

pub fn ts5_channels(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated) VALUES('ts5-channel-conn','default','ts5-bot-account','fixture','none','usr_dev',1,1);
        INSERT INTO bots(id,workspace_id,name,harness,enabled,updated) VALUES('ts5-bot','default','TS5','claude-code',1,1);
        INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES('ts5-bot','default',1);")?;
        Ok(())
    })
}

pub fn ts5_mail(store: &Store, at: i64) -> Result<()> {
    use crate::mail::{self, MessageRow, PartMeta, ThreadRow};
    use crate::{ConnRow, Connections};

    store.write(|w| {
        let ws = "default";
        let conn = "ts5-mail-conn";
        let thread = "ts5-mail";
        Connections(w).insert(&ConnRow {
            id: conn.into(),
            workspace_id: ws.into(),
            account_id: "ts5-bot-account".into(),
            adapter: "fixture".into(),
            auth: "none".into(),
            granted_scopes: vec![],
            caps: vec![],
            credential_item: None,
            status: "ok".into(),
            health: Default::default(),
            config: serde_json::json!({}),
            last_sync: None,
            created_by: "usr_dev".into(),
            revision: 1,
            created: 1,
            updated: 1,
        })?;
        mail::insert_thread(
            w,
            &ThreadRow {
                id: thread.into(),
                workspace_id: ws.into(),
                connection_id: conn.into(),
                thread_key: "thread".into(),
                subject: "TS5".into(),
                last_at: 1,
                count: 0,
                unread: 0,
                participants: vec![],
                state: "waiting".into(),
                visibility: "private".into(),
                created_by: "usr_dev".into(),
            },
            1,
        )?;
        // Reverse insertion order verifies that equal-time cursors sort by id.
        for (id, name, bulk) in [
            ("mail-c", "  ", false),
            ("mail-b", "Bulk Sender", true),
            ("mail-a", "Alice", false),
        ] {
            mail::insert_message(
                w,
                &MessageRow {
                    id: id.into(),
                    workspace_id: ws.into(),
                    connection_id: conn.into(),
                    thread_id: thread.into(),
                    ext_id: id.into(),
                    message_id: Some(format!("<{id}>")),
                    in_reply_to: None,
                    refs: vec![],
                    folders: vec![],
                    flags: vec![],
                    direction: "in".into(),
                    from_addr: "sender@example.invalid".into(),
                    from_name: name.into(),
                    from_account_id: None,
                    from_party_id: None,
                    reply_to: None,
                    to_addrs: serde_json::json!([]),
                    cc_addrs: serde_json::json!([]),
                    subject: "TS5".into(),
                    snippet: "body".into(),
                    date: at,
                    size: 4,
                    has_attachments: bulk,
                    bulk,
                    body_text: Some("body".into()),
                    body_new: None,
                    has_html: false,
                    raw_path: None,
                    purged: None,
                    event_id: None,
                    status: "received".into(),
                    created: at,
                },
            )?;
        }
        for (name, part, mime, inline) in [
            ("file", "2", "application/pdf", false),
            ("inline", "1", "image/png", true),
            ("image", "3", "image/png", false),
        ] {
            mail::insert_attachment(
                w,
                ws,
                "mail-b",
                &PartMeta {
                    part: part.into(),
                    name: format!("{name}.bin"),
                    mime: mime.into(),
                    size: 7,
                    content_id: None,
                    inline,
                },
                at,
            )?;
        }
        Ok(())
    })
}

pub fn goal_items(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch("INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES
                    ('ST-1', 'default', 1, 'task', 'draft the outline', 'run', '执行', 1, 1),
                    ('ST-2', 'default', 2, 'task', 'collect numbers', 'done', '完成', 1, 1),
                    ('ST-3', 'default', 3, 'task', 'old idea', 'drop', '完成', 1, 1);")?;
        Ok(())
    })
}

pub fn notification_policy(
    store: &Store,
    ws: &str,
    user: &str,
    enabled: bool,
    tz: &str,
    quiet: Option<(u16, u16)>,
) -> Result<()> {
    store.with_conn(|c| {
        c.execute("INSERT INTO notification_policies (workspace_id, user_id, enabled, mode, timezone, quiet_start, quiet_end, consented_at, consent_since_seq, updated)
            VALUES (?, ?, ?, ?, ?, ?, ?, 1, 0, 1)
            ON CONFLICT(workspace_id, user_id) DO UPDATE SET timezone=excluded.timezone,quiet_start=excluded.quiet_start,quiet_end=excluded.quiet_end",
            params![ws,user,enabled,if enabled {"now"} else {"log"},tz,quiet.map(|q|q.0),quiet.map(|q|q.1)])?;
        Ok(())
    })
}

pub fn bot_space(w: &mut W<'_>) -> Result<()> {
    w.legacy().execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
        VALUES ('s1', 'default', 'test', 'group', 'g1', 'Project', 1, 1);
        INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated)
        VALUES ('alice', 'default', 'person', 'Alice', 'usr_dev', 1, 1);")?;
    Ok(())
}

pub fn team_followup(store: &Store, at: i64) -> Result<()> {
    store.write(|w| {
        w.legacy().execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
            VALUES ('s2','default','test','group','g2','Other team',1,1)")?;
        seed_message(w, ChannelInput {id:"m4",space:"s2",at,reply:None,thread:None,text:"Same follow up",mentions:&json!([])})
    })
}

pub fn save_counts(store: &Store) -> Result<(i64, i64, i64)> {
    store.reader(|c| {
        Ok((
            c.query_row("SELECT count(*) FROM digest_segments", [], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM matters", [], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM digest_changes", [], |r| r.get(0))?,
        ))
    })
}

pub fn create_memory(
    store: &Store,
    ws: &str,
    memory: &crate::base::mems::NewMem,
) -> Result<crate::base::mems::Mem> {
    store.transaction(|tx| crate::base::mems::create(tx, ws, memory))
}

pub fn duplicate_hidden_packets(w: &mut W<'_>, packet: &str, count: usize) -> Result<()> {
    let tx = w.legacy();
    for n in 0..count {
        tx.execute("INSERT INTO digest_packets(id,workspace_id,conv,seq,kind,trigger,first_id,last_id,first_at,last_at,count,tokens,priority,facts,candidates,created,updated)
            SELECT ?,workspace_id,?,seq,kind,trigger,first_id,last_id,first_at,last_at,count,tokens,priority+1,facts,candidates,created,updated FROM digest_packets WHERE id=?",
            params![format!("hidden-{n}"), format!("space:hidden-{n}"), packet])?;
    }
    Ok(())
}

pub fn change_all_batching(store: &Store, settings: &Value) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE settings SET value = ? WHERE key = 'digest'",
            [settings.to_string()],
        )?;
        Ok(())
    })
}

pub fn force_todo_sources(w: &mut W<'_>, ws: &str, todo: &str, matter: &str) -> Result<()> {
    let mut todos = crate::digest::Todos(w);
    let mut row = todos.get(ws, todo)?.expect("fixture todo");
    row.fields.matter = Some(matter.into());
    todos.change_fields(ws, todo, &row.fields, row.updated)?;
    todos.add_source(todo, "space:s1", "m1")?;
    todos.add_party(todo, "alice")
}

pub fn schedule_counts(store: &Store, id: &str) -> Result<(i64, i64)> {
    store.reader(|c| Ok((
        c.query_row("SELECT count(*) FROM flow_triggers WHERE flow_id = ?", [id], |r| r.get(0))?,
        c.query_row("SELECT count(*) FROM flows WHERE kind = 'schedule' AND json_extract(schedule, '$.mode') = 'brief'", [], |r| r.get(0))?,
    )))
}

pub type Notification = (String, String, String, String, String, String);

pub fn notifications(store: &Store) -> Result<Vec<Notification>> {
    store.reader(|c| {
        Ok(c.prepare(
        "SELECT n.user_id, n.topic, n.mode, n.state, n.body, f.kind FROM personal_notifications n
        JOIN feed_entries f ON f.notification_id = n.id ORDER BY n.created, n.id",
    )?.query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?
        .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn brief_runs(store: &Store, id: &str) -> Result<Vec<(String, String, String)>> {
    store.reader(|c| Ok(c.prepare(
        "SELECT trigger_kind, status, outcome_label FROM flow_runs WHERE flow_id = ? ORDER BY started, rowid",
    )?.query_map([id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?))
}

pub fn brief_items(store: &Store, ws: &str, owner: &str, other: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT INTO items (id, workspace_id, no, title, status, stage, requested_by, created, updated) VALUES
            ('ST-1', ?1, 1, '迁移脚本', 'run', '', ?2, 1, 1),
            ('ST-2', ?1, 2, '导出报表', 'fail', '', ?2, 1, 1),
            ('ST-3', ?1, 3, '别人的事项', 'fail', '', ?3, 1, 1)", params![ws,owner,other])?;
        tx.execute("INSERT INTO decisions (id, workspace_id, item_id, kind, subject_type, subject_id, question, status, requested_by, created)
            VALUES ('dec_1', ?, 'ST-2', 'ask', 'item', 'ST-2', '怎么办', 'open', 'steve', 1)", [ws])?;
        Ok(())
    })
}

pub fn acknowledge_counted(w: &mut W<'_>, ws: &str, conv: &str, ids: &[String]) -> Result<usize> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let count = Arc::new(AtomicUsize::new(0));
    let writes = count.clone();
    let tx = w.legacy();
    tx.update_hook(Some(
        move |_: rusqlite::hooks::Action, _: &str, table: &str, _: i64| {
            if table == "digest_pending_convs" {
                writes.fetch_add(1, Ordering::SeqCst);
            }
        },
    ))?;
    let result = w.digest_windows().acknowledge(ws, conv, ids, 1, 1);
    tx.update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>)?;
    result?;
    Ok(count.load(Ordering::SeqCst))
}
