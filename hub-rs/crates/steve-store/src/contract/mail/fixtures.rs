//! Database states used to set up or observe mail integration tests.
use crate::{Result, Store};
use rusqlite::params;

fn scalar(store: &Store, sql: &str, args: impl rusqlite::Params) -> i64 {
    store
        .sqlite_read(|c| Ok::<_, crate::StoreError>(c.query_row(sql, args, |r| r.get(0))?))
        .expect("fixture query")
}

pub fn last_uid(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COALESCE(last_uid, 0) FROM mail_folders WHERE connection_id = ? AND ext_id = 'INBOX'",
        [key],
    )
}

pub fn running_event_flows(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM flow_runs WHERE event_id = ? AND status IN ('running', 'starting')",
        [key],
    )
}

pub fn flow_enabled(store: &Store, key: &str) -> i64 {
    scalar(store, "SELECT enabled FROM flows WHERE id = ?", [key])
}

pub fn digest_entries(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM digest_entries WHERE workspace_id = ?",
        [key],
    )
}

pub fn open_decisions(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM decisions WHERE status = 'open' AND workspace_id = ?",
        [key],
    )
}

pub fn drafts(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM mail_drafts WHERE workspace_id = ?",
        [key],
    )
}

pub fn flow_step_decisions(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM decisions WHERE subject_type = 'flow_step' AND workspace_id = ?",
        [key],
    )
}

pub fn flow_runs(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM flow_runs WHERE event_id = ?",
        [key],
    )
}

pub fn event_messages(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM mail_messages WHERE event_id = ?",
        [key],
    )
}

pub fn message_copies(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM mail_messages WHERE message_id = ?",
        [key],
    )
}

pub fn linked_messages(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM mail_messages WHERE connection_id = ? AND event_id IS NOT NULL",
        [key],
    )
}

pub fn sync_days(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT sync_days FROM mail_account_settings WHERE connection_id = ?",
        [key],
    )
}

pub fn account_handle_count(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM account_handles WHERE handle = ?",
        [key],
    )
}

pub fn attachment_count(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM mail_attachments WHERE message_id = ?",
        [key],
    )
}

pub fn mail_account_revisions(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM revisions WHERE object_type = 'mail_account' AND object_id = ?",
        [key],
    )
}

pub fn adapter_connections(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM connections WHERE adapter = ?",
        [key],
    )
}

pub fn inbox_synced(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM mail_folders WHERE connection_id = ? AND ext_id = 'INBOX' AND synced IS NOT NULL AND last_uid IS NOT NULL",
        [key],
    )
}

pub fn workspace_files(store: &Store, key: &str) -> i64 {
    scalar(
        store,
        "SELECT count(*) FROM files WHERE workspace_id = ?",
        [key],
    )
}

pub const MAILBOX_TABLES: [&str; 5] = [
    "mail_messages",
    "mail_threads",
    "mail_folders",
    "mail_account_settings",
    "source_settings",
];

pub fn mailbox_rows(store: &Store, conn: &str) -> Vec<(&'static str, i64)> {
    MAILBOX_TABLES
        .into_iter()
        .map(|table| {
            (
                table,
                scalar(
                    store,
                    &format!("SELECT COUNT(*) FROM {table} WHERE connection_id = ?"),
                    [conn],
                ),
            )
        })
        .collect()
}

pub struct DigestAudit(pub Store);

impl Drop for DigestAudit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        self.0.sqlite_read(|c| {
            let tx = c.unchecked_transaction()?;
            let workspaces = tx
                .prepare("SELECT workspace_id FROM chan_messages UNION SELECT workspace_id FROM mail_messages UNION SELECT workspace_id FROM digest_pending UNION SELECT workspace_id FROM digest_pending_convs")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for ws in workspaces {
                let mismatches = steve_store_sqlite::digest::verify_pending(&tx, &ws)?;
                assert!(mismatches.is_empty(), "digest pending mismatch in workspace {ws}: {mismatches:?}");
            }
            Ok::<_, crate::StoreError>(())
        }).unwrap();
    }
}

pub fn approve_without_decision(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE mail_drafts SET status = 'approved', send_at = 0 WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}

pub fn edit_approved_body(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE mail_drafts SET body = '换过的正文', send_at = 0 WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}

pub fn set_draft_session(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE mail_drafts SET session_id = 'ses_1' WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}

pub fn mark_sending(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE mail_drafts SET status = 'sending' WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}

pub fn add_mailbox_alias(store: &Store, ws: &str, conn: &str) -> Result<()> {
    store.write_locked(|c| {
            let acc: String = c.query_row(
                "SELECT account_id FROM connections WHERE id = ?",
                [conn],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO account_handles (workspace_id, family, handle, account_id, created) VALUES (?, 'email', 'peng@alias.example', ?, 1)",
                params![ws, acc],
            )?;
            Ok(())
        })
}

pub fn reject_outgoing_record(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("CREATE TRIGGER reject_outgoing BEFORE INSERT ON mail_messages WHEN NEW.direction = 'out' BEGIN SELECT RAISE(ABORT, 'test outgoing record fault'); END")?;
        Ok(())
    })
}

pub fn reject_failure_record(store: &Store, conn: &str) -> Result<()> {
    store.write_locked(|c| {
        // The approval and the connection are still valid (so the draft is claimed), but its adapter
        // is gone, so preparing the submission fails after the claim.
        c.execute("UPDATE connections SET adapter = 'gone' WHERE id = ?", [conn])?;
        c.execute_batch("CREATE TRIGGER reject_failure BEFORE UPDATE OF status ON mail_drafts WHEN NEW.status = 'failed' BEGIN SELECT RAISE(ABORT, 'test failure record fault'); END")?;
        Ok(())
    })
}

pub fn give_acme_basis(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
            c.execute_batch(
                "INSERT INTO kb_chunks (workspace_id, obj_type, obj_id, seq, heading, text, visibility, level, created_by, source_kind, label, updated)
                 VALUES ('default', 'chunk', 'kd_acme', 3, '', 'Acme 的预算是 30万', 'private', 'reasoning', 'usr_dev', 'note', '笔记 · Acme', 1);",
            )?;
            c.execute(
                "UPDATE mail_drafts SET basis = json_set(basis, '$.entries', json(?)) WHERE id = ?",
                params![
                    r#"[{"source_type":"chunk","source_id":"kd_acme","source_label":"笔记 · Acme","level":"reasoning","effective":"reasoning","used":true,"out":"reasoning","seq":3}]"#,
                    id
                ],
            )?;
            Ok(())
        })
}

pub fn remove_acme_basis(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
            c.execute_batch("DELETE FROM kb_chunks WHERE obj_id = 'kd_acme';")?;
            c.execute(
                "UPDATE mail_drafts SET basis = json_set(basis, '$.entries', json(?)) WHERE id = ?",
                params![
                    r#"[{"source_type":"chunk","source_id":"kd_acme","source_label":"笔记 · Acme","level":"reasoning","effective":"reasoning","used":true,"out":"reasoning","seq":3}]"#,
                    id
                ],
            )?;
            Ok(())
        })
}

pub fn triage_bots(store: &Store) -> Result<()> {
    store.write_locked(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, role, main, harness, enabled, updated) VALUES ('steve', 'default', 'Steve', '', 1, 'claude-code', 1, 1), ('mail', 'default', '邮件', '处理邮件', 0, 'claude-code', 1, 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1), ('mail', 'default', 1);",
            )?;
            Ok(())
        })
}

pub fn delete_flow(store: &Store, flow: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE flows SET deleted = 1 WHERE id = ?", [flow])?;
        Ok(())
    })
}

pub fn enable_archive(store: &Store, conn: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE mail_account_settings SET archive_on_done = 1 WHERE connection_id = ?",
            [conn],
        )?;
        Ok(())
    })
}

pub fn reset_mail_event_delivery(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch(
            "DELETE FROM outbox_receipts WHERE consumer_id = 'mail.events';
                 DELETE FROM outbox_leases WHERE consumer_id = 'mail.events';
                 UPDATE mail_messages SET event_id = NULL;
                 UPDATE source_settings SET paused = 1;",
        )?;
        Ok(())
    })
}

pub fn link_agent_thread(store: &Store, thread: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("INSERT INTO bots (id,workspace_id,name,harness,model,updated) VALUES ('mail','default','Mail','fixture','writer',1);
            INSERT INTO items (id,workspace_id,no,kind,title,status,stage,created,updated,requested_by) VALUES ('ST-1','default',1,'task','Mail','open','',1,1,'usr_dev');
            INSERT INTO sessions (id,workspace_id,item_id,bot_id,harness,model,cwd,status,created,updated) VALUES ('ses_mail','default','ST-1','mail','fixture','writer','','running',1,1);")?;
        c.execute("INSERT INTO links (workspace_id,subject_type,subject_id,predicate,object_type,object_id,source,created) VALUES ('default','item','ST-1','about','mail_thread',?,'fixture',1)", [thread])?;
        Ok(())
    })
}

pub fn set_reviewer_model(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE mail_drafts SET basis = ? WHERE id = ?",
            params![r#"{"answer_model":"reviewer"}"#, id],
        )?;
        Ok(())
    })
}

pub fn give_restricted_basis(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("INSERT INTO kb_chunks (workspace_id,obj_type,obj_id,seq,heading,text,visibility,level,created_by,source_kind,label,updated)
            VALUES ('default','chunk','restricted',0,'','内部预算 420万元','private','reasoning','usr_dev','note','Restricted',1)")?;
        c.execute("UPDATE mail_drafts SET basis = ? WHERE id = ?", params![r#"{"entries":[{"source_type":"chunk","source_id":"restricted","source_label":"Restricted","level":"reasoning","effective":"reasoning","used":true,"out":"reasoning","seq":0}]}"#,id])?;
        Ok(())
    })
}

pub fn delete_restricted_note(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute("DELETE FROM kb_chunks WHERE obj_id='restricted'", [])?;
        Ok(())
    })
}

pub fn change_disclosure_inputs(
    store: &Store,
    mutation: &str,
    id: &str,
    connection: &str,
) -> Result<()> {
    store.write_locked(|c| {
        match mutation {
            "body" => {
                c.execute(
                    "UPDATE mail_drafts SET body='changed during check' WHERE id=?",
                    [id],
                )?;
            }
            "to_addrs" | "cc_addrs" | "bcc_addrs" => {
                c.execute(
                    &format!("UPDATE mail_drafts SET {mutation}=? WHERE id=?"),
                    params![r#"[{"addr":"changed@outside.example"}]"#, id],
                )?;
            }
            "approval" => {
                c.execute(
                    "UPDATE decisions SET status='expired' WHERE subject_id=? AND kind='approve'",
                    [id],
                )?;
            }
            "caps" => {
                c.execute(
                    "UPDATE connections SET caps='[\"mail.read\"]' WHERE id=?",
                    [connection],
                )?;
            }
            "paused" => {
                c.execute(
                    "UPDATE connections SET status='paused' WHERE id=?",
                    [connection],
                )?;
            }
            "knowledge" => {
                c.execute(
                    "UPDATE kb_chunks SET level='never' WHERE obj_id='restricted'",
                    [],
                )?;
            }
            _ => unreachable!(),
        }
        Ok(())
    })
}

pub fn set_thread_folders(store: &Store, needs_reply: &str, waiting: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE mail_threads SET state = 'needs_reply' WHERE id = ?",
            [needs_reply],
        )?;
        c.execute(
            "UPDATE mail_threads SET state = 'waiting' WHERE id = ?",
            [waiting],
        )?;
        Ok(())
    })
}

pub fn invalidate_doc(store: &Store, doc: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE kb_docs SET sha256 = '' WHERE id = ?", [doc])?;
        Ok(())
    })
}

pub fn new_session(
    store: &Store,
    id: &str,
    ws: &str,
    item: &str,
    cwd: &std::path::Path,
) -> Result<()> {
    store.write_locked(|c| {
            c.execute(
                "INSERT INTO sessions (id, workspace_id, item_id, no, bot_id, harness, cwd, status, created, updated) VALUES (?, ?, ?, 1, 'mail', 'claude', ?, 'running', 1, 1)",
                params![id, ws, item, cwd.to_string_lossy()],
            )?;
            Ok(())
        })
}

pub fn mcp_bot(store: &Store) -> Result<()> {
    store.write_locked(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('mail', 'default', '邮件', 'claude-code', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('mail', 'default', 1);",
            )?;
            Ok(())
        })
}

pub fn party_name(store: &Store, id: &str) -> String {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT name FROM parties WHERE id = ?",
                [id],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn source_scopes(store: &Store, conn: &str) -> String {
    store
        .sqlite_read(|c| {
            Ok::<String, crate::StoreError>(c.query_row(
                "SELECT scopes FROM source_settings WHERE connection_id = ?",
                [conn],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn event_ids(store: &Store) -> Vec<String> {
    store
        .sqlite_read(|c| {
            let mut st = c.prepare("SELECT id FROM ext_events ORDER BY received, rowid")?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok::<_, crate::StoreError>(v)
        })
        .unwrap()
}

pub fn event(store: &Store, id: &str) -> serde_json::Value {
    store.sqlite_read(|c| {
                Ok::<_, crate::StoreError>(c.query_row(
                    "SELECT id, type, outcome, outcome_label, decision_id, work_item_id, thread_key FROM ext_events WHERE id = ?",
                    [id],
                    |r| {
                        Ok(serde_json::json!({
                            "id": r.get::<_, String>(0)?, "type": r.get::<_, String>(1)?,
                            "outcome": r.get::<_, String>(2)?, "label": r.get::<_, String>(3)?,
                            "decision_id": r.get::<_, Option<String>>(4)?,
                            "item_id": r.get::<_, Option<String>>(5)?,
                            "thread_key": r.get::<_, Option<String>>(6)?,
                        }))
                    },
                )?)
            })
        .unwrap()
}

pub fn message_with_subject(store: &Store, conn: &str, subject: &str) -> String {
    store.sqlite_read(|c| {
                Ok::<_, crate::StoreError>(c.query_row(
                    "SELECT id FROM mail_messages WHERE connection_id = ?1 AND subject LIKE ?2 || '%' ORDER BY date LIMIT 1",
                    [conn, subject],
                    |r| r.get(0),
                )?)
            })
        .unwrap()
}

pub fn flow_by_origin(store: &Store, origin: &str) -> String {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT id FROM flows WHERE origin = ?",
                [origin],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn decision_state(store: &Store, id: &str) -> (String, String, String) {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT kind, subject_type, status FROM decisions WHERE id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap()
}

pub fn event_steps(store: &Store, event: &str) -> Vec<String> {
    store.sqlite_read(|c| {
            let mut st = c.prepare(
                "SELECT s.node_id FROM flow_run_steps s JOIN flow_runs r ON r.id = s.run_id WHERE r.event_id = ? ORDER BY s.seq",
            )?;
            let v = st
                .query_map([event], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok::<_, crate::StoreError>(v)
        })
        .unwrap()
}

pub fn draft_step_output(store: &Store, run: &str) -> String {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT output FROM flow_run_steps WHERE run_id = ? AND node_id = 'r'",
                [run],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn item_link_count(store: &Store, item: &str, pred: &str, ty: &str, id: &str) -> i64 {
    store.sqlite_read(|c| {
                Ok::<_, crate::StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM links WHERE subject_type = 'item' AND subject_id = ?1 AND predicate = ?2 AND object_type = ?3 AND object_id = ?4",
                    [item, pred, ty, id],
                    |r| r.get(0),
                )?)
            })
        .unwrap()
}

pub fn triage_bot(store: &Store, conn: &str) -> Option<String> {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT triage_bot FROM source_settings WHERE connection_id = ?",
                [conn],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn item_drafts(store: &Store, item: &str) -> Vec<String> {
    store
        .sqlite_read(|c| {
            let mut st =
                c.prepare("SELECT id FROM mail_drafts WHERE item_id = ? ORDER BY created, rowid")?;
            let v = st
                .query_map([item], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok::<_, crate::StoreError>(v)
        })
        .unwrap()
}

pub fn attachment_file(store: &Store, part: &str) -> Option<String> {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT file_id FROM mail_attachments WHERE id = ?",
                [part],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn candidate_mems(store: &Store) -> Vec<(String, String, String, String, String)> {
    store.sqlite_read(|c| {
            let mut st = c.prepare(
                "SELECT m.text, m.level, m.status, mc.origin, mc.origin_id FROM mems m JOIN mem_candidates mc ON mc.mem_id = m.id ORDER BY m.rowid",
            )?;
            let v = st
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, crate::StoreError>(v)
        })
        .unwrap()
}

pub fn doc_count(store: &Store, thread: &str) -> i64 {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT COUNT(*) FROM kb_docs WHERE ref = ?",
                [thread],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn thread_patch_audits(store: &Store, thread: &str) -> i64 {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT COUNT(*) FROM audit WHERE action = 'mail_thread.patch' AND target = ?",
                [thread],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn mailbox_messages(store: &Store, conn: &str) -> Vec<String> {
    store
        .sqlite_read(|c| {
            let mut st = c.prepare(
                "SELECT id FROM mail_messages WHERE connection_id = ? ORDER BY date, rowid",
            )?;
            let v = st
                .query_map([conn], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok::<_, crate::StoreError>(v)
        })
        .unwrap()
}

pub fn fail_cursor_message(store: &Store, fail: bool) -> Result<()> {
    store.write_locked(|c| {
        let sql = if fail {
            "CREATE TRIGGER fail_cursor_message BEFORE INSERT ON mail_messages WHEN NEW.ext_id LIKE '%:52' BEGIN SELECT RAISE(FAIL, 'injected store failure'); END;"
        } else {
            "DROP TRIGGER fail_cursor_message"
        };
        c.execute_batch(sql)?;
        Ok(())
    })
}

pub fn open_failure(store: &Store, id: &str) -> String {
    store
        .sqlite_read(|c| {
            Ok::<_, crate::StoreError>(c.query_row(
                "SELECT id FROM decisions WHERE subject_id=? AND kind='fail' AND status='open'",
                [id],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

pub fn pending_mail_events(store: &Store) -> i64 {
    scalar(
        store,
        "SELECT COUNT(*) FROM outbox_events e WHERE e.topic = 'mail.event.ready'
      AND NOT EXISTS (SELECT 1 FROM outbox_receipts r WHERE r.event_seq = e.seq)",
        [],
    )
}

pub fn first_attachment(store: &Store, thread: &str) -> String {
    store.sqlite_read(|c| {
        Ok::<_, crate::StoreError>(c.query_row(
            "SELECT a.id FROM mail_attachments a JOIN mail_messages m ON m.id = a.message_id WHERE m.thread_id = ?",
            [thread], |r| r.get(0),
        )?)
    }).unwrap()
}

pub fn disable_flow(store: &Store, origin: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE flows SET enabled = 0 WHERE origin = ?", [origin])?;
        tx.conn.execute(
            "DELETE FROM flow_triggers WHERE flow_id IN (SELECT id FROM flows WHERE origin = ?)",
            [origin],
        )?;
        Ok(())
    })
}

pub fn item(store: &Store, ws: &str, id: &str, no: i64, title: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated)
            VALUES (?, ?, ?, 'task', ?, 'open', '', 1, 1)",
            params![id, ws, no, title],
        )?;
        Ok(())
    })
}

pub fn send_audits(store: &Store, action: &str) -> Vec<serde_json::Value> {
    store
        .sqlite_read(|c| {
            let mut st = c.prepare("SELECT detail FROM audit WHERE action = ? ORDER BY rowid")?;
            let details = st
                .query_map([action], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, crate::StoreError>(
                details
                    .iter()
                    .map(|s| serde_json::from_str(s).unwrap())
                    .collect(),
            )
        })
        .unwrap()
}

pub fn set_connection(store: &Store, conn: &str, status: &str, caps: &[&str]) -> Result<()> {
    let caps = serde_json::to_string(caps).unwrap();
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE connections SET status = ?, caps = ? WHERE id = ?",
            params![status, caps, conn],
        )?;
        Ok(())
    })
}

pub fn all_drafts(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|c| {
        let mut st = c.prepare("SELECT id FROM mail_drafts ORDER BY rowid")?;
        Ok(st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })
}

pub fn decisions_of_draft(store: &Store, id: &str) -> Result<Vec<crate::Decision>> {
    store.sqlite_read(|c| {
        let mut st = c.prepare("SELECT id FROM decisions WHERE subject_id=? ORDER BY rowid")?;
        let ids = st
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| crate::base::decisions::get(c, id).map(Option::unwrap))
            .collect()
    })
}
