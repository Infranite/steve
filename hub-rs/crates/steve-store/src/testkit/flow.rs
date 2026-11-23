//! Database fixtures shared by the Flow integration tests.

pub mod due;

use serde::de::DeserializeOwned;

use crate::{Result, Store};
use rusqlite::params;

pub fn outbox_payloads<T: DeserializeOwned>(store: &Store, topic: &str) -> Result<Vec<T>> {
    store.sqlite_read(|c| {
        let mut query =
            c.prepare_cached("SELECT payload FROM outbox_events WHERE topic = ? ORDER BY seq")?;
        let raw = query
            .query_map([topic], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.into_iter()
            .map(|payload| Ok(serde_json::from_str(&payload)?))
            .collect()
    })
}

pub fn signal_without_connection(store: &Store, id: &str, visibility: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE ext_events SET connection_id = NULL, visibility = ? WHERE id = ?",
            rusqlite::params![visibility, id],
        )?;
        Ok(())
    })
}

pub struct FixtureItem<'a> {
    pub ws: &'a str,
    pub title: &'a str,
    pub bot: &'a str,
    pub project: Option<&'a str>,
    pub source: &'a serde_json::Value,
    pub requester: Option<&'a str>,
    pub tag_ids: &'a [String],
    pub propose: bool,
    pub accept_auto: bool,
}

pub fn create_item(store: &Store, req: FixtureItem<'_>) -> Result<String> {
    store.write_locked(|c| {
                let no: i64 = c.query_row(
                    "SELECT coalesce(max(no), 0) + 1 FROM items WHERE workspace_id = ?",
                    [&req.ws],
                    |r| r.get(0),
                )?;
                let id = format!("ST-{no}");
                let now = crate::now();
                c.execute(
                    "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, accept_mode, project_id, source, requested_by, created, updated) VALUES (?, ?, ?, ?, ?, '', ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        id,
                        req.ws,
                        no,
                        req.title,
                        if req.propose { "wait" } else { "run" },
                        req.bot,
                        if req.accept_auto { Some("auto") } else { None },
                        req.project,
                        req.source.to_string(),
                        req.requester,
                        now,
                        now
                    ],
                )?;
                for t in req.tag_ids {
                    c.execute("INSERT INTO item_tags (item_id, tag_id) VALUES (?, ?)", params![id, t])?;
                }
                Ok(id)
            })
}

pub fn set_item_status(store: &Store, item: &str, status: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE items SET status = ? WHERE id = ?", (status, item))?;
        Ok(())
    })
}

pub fn seed_workspace(store: &Store) -> Result<()> {
    store
            .write_locked(|c| {
                c.execute_batch(
                    "INSERT INTO bots (id, workspace_id, name, main, harness, enabled, updated) VALUES ('steve', 'default', 'Steve', 1, 'claude-code', 1, 1), ('ops', 'default', '运维', 0, 'claude-code', 1, 1), ('off', 'default', '停用的', 0, 'claude-code', 0, 1), ('w2bot', 'w2', '别处', 0, 'claude-code', 1, 1);
                     INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1), ('steve', 'w2', 1), ('ops', 'default', 1), ('off', 'default', 1), ('w2bot', 'w2', 1);
                     INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES ('prj_a', 'default', 'A', '/tmp/a', 1, 1);
                     INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_a', 'default', '周报', 'topic', '', 'usr_dev', 1);",
                )?;
                Ok(())
            })
}

pub fn event_connection(
    store: &Store,
    ws: &str,
    adapter: &str,
    owner: Option<&str>,
    created_by: &str,
) -> Result<String> {
    let con = crate::uid("con");
    let acc = crate::uid("acc");
    let party = crate::uid("ct");
    store.write_locked(|c| {
                match owner {
                    Some(u) => c.execute(
                        "INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated) VALUES (?, ?, 'contact', '我', ?, ?, 1, 1)",
                        params![party, ws, u, u],
                    )?,
                    None => c.execute(
                        "INSERT INTO parties (id, workspace_id, type, name, org_kind, created_by, created, updated) VALUES (?, ?, 'org', '本公司', 'self', ?, 1, 1)",
                        params![party, ws, created_by],
                    )?,
                };
                c.execute(
                    "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES (?, ?, 'gmail', 'email', ?, 'personal', ?, ?, 'connect', 1, 1)",
                    params![acc, ws, party, format!("{con}@example.com"), format!("{con}@example.com")],
                )?;
                c.execute(
                    "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated) VALUES (?, ?, ?, ?, 'none', '[\"events\"]', ?, 1, 1)",
                    params![con, ws, acc, adapter, created_by],
                )?;
                Ok(())
            })
        ?;
    Ok(con)
}

pub fn decisions_for(
    store: &Store,
    subject_type: &str,
    subject: &str,
) -> Result<Vec<crate::Decision>> {
    store.read(|r| {
        use crate::contract::Q;
        use sea_query::{Expr, ExprTrait, Order, Query};
        let ids = r.all(
            Query::select()
                .from("decisions")
                .column("id")
                .and_where(Expr::col("subject_type").eq(subject_type))
                .and_where(Expr::col("subject_id").eq(subject))
                .order_by("created", Order::Asc),
            |row| row.get::<String>(0),
        )?;
        ids.iter()
            .map(|id| crate::repo::must(crate::DecisionRows(r).get(id)?))
            .collect()
    })
}

pub fn flow_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT count(*) FROM flows", [], |r| r.get(0))?))
}

pub fn enabled_trigger_count(store: &Store, id: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM flow_triggers WHERE flow_id = ? AND enabled = 1",
            [id],
            |r| r.get(0),
        )?)
    })
}

pub struct Correction {
    pub action: String,
    pub scope: String,
    pub change: Option<String>,
    pub item: Option<String>,
}

pub fn corrections(store: &Store, event: &str) -> Result<Vec<Correction>> {
    store.sqlite_read(|c| {
        let mut query = c.prepare("SELECT action, scope, changeset_id, work_item_id FROM signal_corrections WHERE event_id = ? ORDER BY created")?;
        Ok(query.query_map([event], |r| Ok(Correction {
            action: r.get(0)?, scope: r.get(1)?, change: r.get(2)?, item: r.get(3)?,
        }))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn rule_texts(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|c| {
        let mut query = c.prepare("SELECT text FROM mems WHERE kind = 'rule' ORDER BY created")?;
        Ok(query
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn item_sources(store: &Store, ws: &str, object_type: &str, id: &str) -> Result<Vec<String>> {
    store.read(|r| {
        Ok(crate::base::links::Links(r)
            .sources(ws, "item", "from", object_type, [id])?
            .into_iter()
            .map(|(_, item, _)| item)
            .collect())
    })
}

pub fn fixture_item(
    store: &Store,
    ws: &str,
    id: &str,
    no: i64,
    title: &str,
    status: &str,
) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, created, updated) VALUES (?, ?, ?, ?, ?, '', 'steve', 1, 1)",
            params![id, ws, no, title, status],
        )?;
        Ok(())
    })
}

pub fn latest_receipt(store: &Store) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT outcome FROM outbox_receipts ORDER BY event_seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn applied_receipt(
    w: &mut crate::W<'_>,
    event: &crate::outbox::Event,
    consumer: &str,
    outcome: &str,
) -> Result<()> {
    crate::outbox::record_test_receipt(w, event, consumer, outcome)
}

pub fn event_ids(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|c| {
        let mut stmt = c.prepare("SELECT id FROM ext_events ORDER BY received, id")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn runs_in_status(store: &Store, status: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM flow_runs WHERE status = ?",
            [status],
            |r| r.get(0),
        )?)
    })
}

pub fn wait_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT count(*) FROM flow_waits", [], |r| r.get(0))?))
}

pub fn waiting_for_item(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch(
            "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES ('run_w', 'default', 'flw_w', 1, 'event', 'waiting', 1);
             INSERT INTO flow_waits (run_id, node_id, workspace_id, mode, match, created) VALUES ('run_w', 'w', 'default', 'event', '{\"source\":\"item\"}', 1);",
        )?;
        Ok(())
    })
}

pub fn set_run_table_available(store: &Store, available: bool) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch(if available {
            "ALTER TABLE review_missing_runs RENAME TO flow_runs"
        } else {
            "ALTER TABLE flow_runs RENAME TO review_missing_runs"
        })?;
        Ok(())
    })
}

pub fn require_initial_run_error(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("CREATE TRIGGER require_start_error BEFORE INSERT ON flow_runs WHEN NEW.status='failed' AND NEW.error IS NULL BEGIN SELECT RAISE(ABORT,'missing initial error'); END")?;
        Ok(())
    })
}

pub fn reject_run_updates(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("CREATE TRIGGER reject_run_update BEFORE UPDATE ON flow_runs BEGIN SELECT RAISE(ABORT,'history unavailable'); END")?;
        Ok(())
    })
}

pub fn startup_flow(store: &Store, ws: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("INSERT INTO flows (id,workspace_id,kind,name,graph,origin,created_by,created,updated) VALUES ('flow_start',?,'schedule','fixture','{}','me','usr_dev',0,0)", [ws])?;
        Ok(())
    })
}

pub fn set_run_context_field(
    store: &Store,
    id: &str,
    path: &str,
    value: &serde_json::Value,
) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE flow_runs SET context=json_set(context,?1,json(?2)) WHERE id=?3",
            params![path, value.to_string(), id],
        )?;
        Ok(())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamagedRunColumn {
    Cost,
    Started,
    Status,
    Error,
    Id,
    Context,
}

pub fn damage_startup_run(store: &Store, id: &str, column: DamagedRunColumn) -> Result<()> {
    let change = match column {
        DamagedRunColumn::Cost => "cost=x'ff'",
        DamagedRunColumn::Started => "started=x'ff'",
        DamagedRunColumn::Status => "status=x'ff'",
        DamagedRunColumn::Error => "error=x'ff'",
        DamagedRunColumn::Id => "id=x'ff'",
        DamagedRunColumn::Context => "context=CAST(context AS BLOB)",
    };
    store.write(|tx| {
        tx.conn.execute("UPDATE flow_runs SET context=json_set(context,'$.startup.attempts','broken') WHERE id=?", [id])?;
        tx.conn.execute(&format!("UPDATE flow_runs SET {change} WHERE id=?"), [id])?;
        Ok(())
    })
}

pub fn set_raw_run_context(store: &Store, id: &str, context: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE flow_runs SET context=? WHERE id=?", [context, id])?;
        Ok(())
    })
}

pub fn set_raw_flow_graph(store: &Store, id: &str, graph: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE flows SET graph=? WHERE id=?", [graph, id])?;
        Ok(())
    })
}

pub fn set_run_event(store: &Store, id: &str, event: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE flow_runs SET event_id=? WHERE id=?", [event, id])?;
        Ok(())
    })
}

pub fn set_event_capability(store: &Store, id: &str, enabled: bool) -> Result<()> {
    let caps = if enabled { r#"["events"]"# } else { "[]" };
    store.write_locked(|c| {
        c.execute("UPDATE connections SET caps=? WHERE id=?", [caps, id])?;
        Ok(())
    })
}

pub fn set_connection_status(store: &Store, id: &str, status: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE connections SET status=? WHERE id=?", [status, id])?;
        Ok(())
    })
}

pub fn pause_source(store: &Store, id: &str, paused: bool) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE source_settings SET paused=? WHERE connection_id=?",
            params![paused, id],
        )?;
        Ok(())
    })
}

pub fn touch_connection(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE connections SET updated=updated+1 WHERE id=?", [id])?;
        Ok(())
    })
}

pub fn set_connections_available(store: &Store, available: bool) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch(if available {
            "ALTER TABLE offline_connections RENAME TO connections"
        } else {
            "ALTER TABLE connections RENAME TO offline_connections"
        })?;
        Ok(())
    })
}

pub fn event_external_ids(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|c| {
        let mut stmt = c.prepare("SELECT external_id FROM ext_events ORDER BY external_id")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn synced_connections(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|c| {
        let mut stmt =
            c.prepare("SELECT id FROM connections WHERE last_sync IS NOT NULL ORDER BY id")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn connection_last_sync(store: &Store, id: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(
            c.query_row("SELECT last_sync FROM connections WHERE id=?", [id], |r| {
                r.get(0)
            })?,
        )
    })
}

pub fn run_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT count(*) FROM flow_runs", [], |r| r.get(0))?))
}

pub fn only_run(store: &Store) -> Result<crate::RunRow> {
    let id: String = store.sqlite_read(|c| -> Result<_> {
        Ok(c.query_row("SELECT id FROM flow_runs", [], |r| r.get(0))?)
    })?;
    store.read(|r| crate::repo::must(crate::RunRows(r).get(&id)?))
}

pub fn set_triage_bot(store: &Store, ws: &str, id: &str, bot: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "INSERT INTO source_settings (connection_id, workspace_id, triage_bot, updated) VALUES (?, ?, ?, 1)",
            params![id, ws, bot],
        )?;
        Ok(())
    })
}

pub fn revisions_by(store: &Store, kind: &str, id: &str, author: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM revisions WHERE object_type = ? AND object_id = ? AND author = ?",
            [kind, id, author],
            |r| r.get(0),
        )?)
    })
}

pub fn event_outcomes(store: &Store) -> Result<Vec<(String, String, String, String)>> {
    store.write_locked(|c| {
        let mut st = c.prepare(
            "SELECT external_id, type, outcome, outcome_label FROM ext_events ORDER BY rowid",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

pub fn seed_handled_signal(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
            c.execute_batch(&format!(
                "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, event_id, status, context, started) VALUES ('run_x', 'default', 'flw_x', 1, 'event', '{id}', 'done', '{{\"event\":{{\"payload\":\"secret text\"}}}}', 1);
                 INSERT INTO flow_run_steps (run_id, seq, workspace_id, node_id, status, input, output, ms, started) VALUES ('run_x', 1, 'default', 't', 'ok', '{{\"a\":1}}', '{{\"b\":2}}', 0.5, 1);"
            ))?;
            Ok(())
        })
}

pub fn raw_retention_settings(store: &Store) -> Result<()> {
    store.write_locked(|c| {
            c.execute(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'keepRaw', '\"30\"', 1), ('w2', 'keepRaw', '12', 1)",
                [],
            )?;
            Ok(())
        })
}

pub fn seed_signal_needs_reply(store: &Store) -> Result<()> {
    store.write_locked(|c| {
            c.execute(
                "UPDATE ext_events SET outcome = 'need', outcome_label = '等你确认回复' WHERE external_id = 'a'",
                [],
            )?;
            Ok(())
        })
}

pub fn signal_timezone_boundary(store: &Store, ws: &str, user: &str, now: i64) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE ext_events SET received = ?", [now - 12 * 3_600_000])?;
        c.execute("INSERT INTO settings (workspace_id,key,value,updated) VALUES (?, 'tz', '\"Europe/London\"',1)", [ws])?;
        c.execute("INSERT INTO notification_policies (workspace_id,user_id,timezone,enabled,mode,consented_at,consent_since_seq,updated) VALUES (?,?,'Asia/Tokyo',1,'now',1,0,1)", params![ws, user])?;
        Ok(())
    })
}

pub fn clear_notification_policies(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute("DELETE FROM notification_policies", [])?;
        Ok(())
    })
}

pub fn clear_workspace_timezones(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute("DELETE FROM settings WHERE key = 'tz'", [])?;
        Ok(())
    })
}

pub fn seed_mail_trigger(
    store: &Store,
    id: &str,
    types: &serde_json::Value,
    conns: &serde_json::Value,
) -> Result<()> {
    store.write_locked(|c| {
            c.execute(
                "INSERT INTO flows (id, workspace_id, kind, name, enabled, graph, version, origin, created_by, created, updated) VALUES (?, 'default', 'flow', '邮件分诊', 1, '{\"nodes\":[{\"id\":\"t\",\"kind\":\"trigger\"}],\"edges\":[]}', 2, 'me', 'usr_dev', 1, 1)",
                [id],
            )?;
            c.execute(
                "INSERT INTO flow_triggers (id, workspace_id, flow_id, node_id, kind, source, types, connections, enabled) VALUES (?, 'default', ?, 't', 'event', 'mail', ?, ?, 1)",
                params![format!("trg_{id}"), id, types.to_string(), conns.to_string()],
            )?;
            Ok(())
        })
}

pub fn seed_signal_run(store: &Store, row: &SignalRunFixture<'_>) -> Result<()> {
    store.write_locked(|c| {
                c.execute(
                    "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, event_id, status, outcome, outcome_label, context, work_item_id, started) VALUES (?, 'default', ?, 1, 'event', ?, ?, ?, ?, ?, ?, 1)",
                    params![row.rid, format!("flw_{}",row.rid), row.id, row.status, row.outcome, row.label, row.ctx.to_string(), (row.outcome == Some("item")).then_some("ST-9")],
                )?;
                Ok(())
            })
}

pub fn signal_settlement(
    store: &Store,
    id: &str,
) -> Result<(String, String, String, Option<String>, Option<String>)> {
    store.write_locked(|c| {
                Ok(c.query_row(
                    "SELECT outcome, outcome_label, reason, run_id, work_item_id FROM ext_events WHERE id = ?",
                    [&id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                        ))
                    },
                )?)
            })
}

pub fn reject_second_signal(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch(
            "CREATE TRIGGER no_b BEFORE INSERT ON ext_events WHEN NEW.external_id = 'b'
                 BEGIN SELECT RAISE(ABORT, 'test store fault'); END",
        )?;
        Ok(())
    })
}

pub fn allow_second_signal(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("DROP TRIGGER no_b")?;
        Ok(())
    })
}

pub fn seed_mail_and_github_waits(store: &Store) -> Result<()> {
    store.write_locked(|c| {
            c.execute_batch(
                "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES ('run_w', 'default', 'flw_w', 1, 'event', 'waiting', 1), ('run_v', 'default', 'flw_v', 1, 'event', 'waiting', 1);
                 INSERT INTO flow_waits (run_id, node_id, workspace_id, mode, match, created) VALUES ('run_w', 'w', 'default', 'event', '{\"source\":\"mail\",\"type\":\"mail.reply\"}', 1), ('run_v', 'v', 'default', 'event', '{\"source\":\"github\"}', 1);",
            )?;
            Ok(())
        })
}

pub struct SignalRunFixture<'a> {
    pub rid: &'a str,
    pub id: &'a str,
    pub status: &'a str,
    pub outcome: Option<&'a str>,
    pub label: &'a str,
    pub ctx: &'a serde_json::Value,
}

pub fn latest_open_decision(store: &Store, subject_type: &str) -> Result<Option<String>> {
    store.write_locked(|c| {
            Ok(rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT id FROM decisions WHERE subject_type = ? AND status = 'open' ORDER BY created DESC, rowid DESC LIMIT 1",
                [subject_type],
                |r| r.get::<_, String>(0),
            ))?)
        })
}

pub fn trigger_types_enabled(store: &Store, id: &str) -> Result<(String, bool)> {
    store.write_locked(|c| {
        Ok(c.query_row(
            "SELECT types, enabled FROM flow_triggers WHERE flow_id = ?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn decision_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT count(*) FROM decisions", [], |r| r.get(0))?))
}

pub fn trigger_count(store: &Store, flow: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM flow_triggers WHERE flow_id = ?",
            [flow],
            |r| r.get(0),
        )?)
    })
}

pub fn wait_mode(store: &Store, run: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(
            c.query_row("SELECT mode FROM flow_waits WHERE run_id = ?", [run], |r| {
                r.get(0)
            })?,
        )
    })
}

pub fn waits_for(store: &Store, run: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM flow_waits WHERE run_id = ?",
            [run],
            |r| r.get(0),
        )?)
    })
}

pub fn digest_text(store: &Store, run: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT text FROM digest_entries WHERE run_id = ?",
            [run],
            |r| r.get(0),
        )?)
    })
}

pub fn event_id_by_external(store: &Store, external: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT id FROM ext_events WHERE external_id = ?",
            [external],
            |r| r.get(0),
        )
        .optional()?)
    })
}

pub struct FlowGrantFixture<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub flow: &'a str,
    pub capability: &'a str,
    pub resource_type: &'a str,
    pub resource: &'a str,
    pub until: i64,
    pub actor: &'a str,
}

pub fn flow_grant(store: &Store, grant: FlowGrantFixture<'_>) -> Result<()> {
    store.write(|w| flow_grant_in(w, grant))
}

pub fn flow_grant_in(w: &mut crate::W<'_>, grant: FlowGrantFixture<'_>) -> Result<()> {
    use sea_query::Query;
    w.exec(
        Query::insert()
            .into_table("grants")
            .columns([
                "id",
                "workspace_id",
                "subject_type",
                "subject_id",
                "capability",
                "resource_type",
                "resource_id",
                "scope",
                "until",
                "created_by",
                "created",
            ])
            .values_panic([
                grant.id.into(),
                grant.ws.into(),
                "flow".into(),
                grant.flow.into(),
                grant.capability.into(),
                grant.resource_type.into(),
                grant.resource.into(),
                r#"{"flow_version":1}"#.into(),
                grant.until.into(),
                grant.actor.into(),
                1_i64.into(),
            ]),
    )?;
    Ok(())
}

pub fn restart_signal(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute("INSERT INTO ext_events (id, workspace_id, source, type, dedupe_key, occurred, received, summary) VALUES ('evt_x', 'default', 'mail', 'mail.new', 'x', 1, 1, '你好')", [])?;
        Ok(())
    })
}

pub fn successful_secret_uses(store: &Store, flow: &str, first: &str, second: &str) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row(
        "SELECT count(*) FROM secret_uses WHERE actor_type = 'flow' AND actor_id = ? AND status = 'ok' AND mode = 'internal' AND run_id IN (?, ?)",
        [flow, first, second], |r| r.get(0),
    )?))
}

pub fn secret_use_grant(store: &Store, run: &str) -> Result<Option<String>> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT grant_id FROM secret_uses WHERE run_id = ?",
            [run],
            |r| r.get(0),
        )?)
    })
}

pub fn host_and_secret_grants(
    store: &Store,
    flow: &str,
    decision: &str,
    secret: &str,
) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row(
        "SELECT COUNT(*) FROM grants WHERE subject_type = 'flow' AND subject_id = ? AND decision_id = ?
         AND ((capability = 'http.effect' AND resource_id = 'api.test')
           OR (capability = 'secret.use' AND resource_id = ?))",
        [flow, decision, secret], |r| r.get(0),
    )?))
}

pub fn audit_count(store: &Store, action: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM audit WHERE action = ?",
            [action],
            |r| r.get(0),
        )?)
    })
}

pub fn code_node_project(store: &Store, ws: &str, setting: &str, path: &str) -> Result<()> {
    store.write_locked(|c| {
            c.execute(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, 1)",
                params![ws, setting, serde_json::json!(["api.test", "other.test"]).to_string()],
            )?;
            c.execute(
                "INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES ('prj_t', ?, 'T', ?, 1, 1)",
                params![ws, path],
            )?;
            Ok(())
        })
}

pub fn flow_grant_details(store: &Store, flow: &str) -> Result<(String, String, i64, String)> {
    store.write_locked(|c| {
            Ok(c.query_row(
                "SELECT capability, resource_id, json_extract(scope, '$.flow_version'), decision_id FROM grants WHERE subject_type = 'flow' AND subject_id = ?",
                [flow],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
}

pub fn secret_release_fault(
    store: &Store,
    first: &str,
    second: &str,
    approval: bool,
) -> Result<()> {
    store.write_locked(|c| {
            let mutation=if approval {format!("UPDATE grants SET revoked=1 WHERE resource_id='{}';",second)}else{format!("UPDATE vault_items SET status='deleted' WHERE id='{}';",second)};
            c.execute_batch(&format!("CREATE TRIGGER ts5_second_release_fault AFTER INSERT ON secret_uses WHEN NEW.item_id='{}' BEGIN {mutation} END",first))?;Ok(())
        })
}

pub type SecretUseResult = (String, String, Option<i64>, Option<String>);

pub fn secret_use_results(store: &Store) -> Result<Vec<SecretUseResult>> {
    store.sqlite_read(|c| {
        let mut stmt =
            c.prepare("SELECT item_id,status,finished,run_id FROM secret_uses ORDER BY rowid")?;
        Ok(stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn correction_signal(w: &mut crate::W<'_>, ws: &str, flow: Option<&str>) -> Result<()> {
    use sea_query::Query;
    w.exec(
        Query::insert()
            .into_table("ext_events")
            .columns([
                "id",
                "workspace_id",
                "source",
                "type",
                "dedupe_key",
                "group_key",
                "summary",
                "occurred",
                "received",
                "outcome",
                "flow_id",
            ])
            .values_panic([
                "ts5-event".into(),
                ws.into(),
                "mail".into(),
                "mail.new".into(),
                "ts5".into(),
                "ts5-group".into(),
                "Fixture summary".into(),
                1_i64.into(),
                1_i64.into(),
                "recorded".into(),
                flow.into(),
            ]),
    )?;
    Ok(())
}

pub fn trigger_next_at(store: &Store, flow: &str) -> Result<Option<i64>> {
    store.write_locked(|c| {
        Ok(c.query_row(
            "SELECT next_at FROM flow_triggers WHERE flow_id = ?",
            [flow],
            |r| r.get(0),
        )?)
    })
}

pub fn close_item(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE items SET phase = 'closed', status = 'done' WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}

pub fn timeline_text(store: &Store, item: &str) -> Result<Vec<String>> {
    store.write_locked(|c| {
        let mut q = c.prepare("SELECT text FROM timeline WHERE item_id = ? ORDER BY id")?;
        let v = q
            .query_map([item], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    })
}

pub fn scheduled_item_origin(store: &Store) -> Result<(Option<String>, String)> {
    store.write_locked(|c| {
        Ok(c.query_row(
            "SELECT accept_mode, source FROM items WHERE id = 'ST-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn seed_session_cost(store: &Store, sid: &str, item: &str, cost: f64) -> Result<()> {
    store.write_locked(|c| {
            c.execute(
                "INSERT INTO sessions (id, workspace_id, item_id, harness, cwd, status, cost, created, updated) VALUES (?, 'default', ?, 'claude-code', '/tmp', 'running', ?, 1, 1) ON CONFLICT(id) DO UPDATE SET cost = excluded.cost",
                params![sid, item, cost],
            )?;
            Ok(())
        })
}

pub fn seed_schedule_retention(store: &Store, a: &str, b: &str, now: i64, day: i64) -> Result<()> {
    store.write_locked(|c| {
            for i in 0..210 {
                c.execute("INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES (?, 'default', ?, 1, 'time', 'done', ?)", params![format!("run_a{i}"), a, now - 100 * day + i])?;
                c.execute("INSERT INTO flow_run_steps (run_id, seq, workspace_id, node_id, status, started) VALUES (?, 1, 'default', 't', 'ok', 1)", [format!("run_a{i}")])?;
            }
            for i in 0..5 {
                c.execute("INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES (?, 'default', ?, 1, 'time', 'done', ?)", params![format!("run_r{i}"), a, now - day + i])?;
            }
            for i in 0..10 {
                c.execute("INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES (?, 'default', ?, 1, 'time', 'done', ?)", params![format!("run_b{i}"), b, now - 300 * day + i])?;
            }
            Ok(())
        })
}

pub fn item_requester(store: &Store, item: &str) -> Result<Option<String>> {
    store.sqlite_read(|c| {
        Ok::<_, crate::StoreError>(c.query_row(
            "SELECT requested_by FROM items WHERE id = ?",
            [&item],
            |r| r.get(0),
        )?)
    })
}

pub fn remove_workspace_member(store: &Store, ws: &str, user: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "DELETE FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
            params![ws, user],
        )?;
        Ok(())
    })
}

pub fn reject_digest_update(store: &Store) -> Result<()> {
    store.write_locked(|c| {c.execute_batch("CREATE TRIGGER fail_digest BEFORE UPDATE ON digest_entries BEGIN SELECT RAISE(ABORT,'injected digest failure'); END;")?;Ok(())})
}

pub fn allow_digest_update(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("DROP TRIGGER fail_digest")?;
        Ok(())
    })
}

pub fn schedule_retained_steps(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM flow_run_steps WHERE run_id LIKE 'run_a%'",
            [],
            |r| r.get(0),
        )?)
    })
}
