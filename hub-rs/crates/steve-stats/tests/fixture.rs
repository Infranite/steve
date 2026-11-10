//! The metrics on a small database shaped like production, with the expected figures worked out
//! by hand below.
//!
//! All times are minutes after 2026-11-01 00:00 UTC; the workspace's `stats.tz` is UTC.

use steve_stats::metrics::{By, Query, breakdown, daily, prices_of, summary, wip_by_day};
use steve_stats::{refresh, views};
use steve_store::{NewBot, NoPublisher, Store, WS};

const T0: i64 = 1_793_491_200_000;
const MIN: i64 = 60_000;

/// The day's turns, the sessions' own dollar figures and the items' marks (times as [`m`] gives
/// them): what the Hub records while the items below happen.
const RECORDED: &str = r#"
INSERT INTO agent_turns (id, workspace_id, session_id, item_id, bot_id, harness, user_seq, attempt, started, ended, stop_reason, error, transient, tools, tool_errors, tool_kinds, permissions) VALUES
  ('tn_1', 'default', 's1', 'ST-1', 'dev', 'claude-code', 1, 0, 1793491260000, 1793491800000, 'end_turn', NULL, 0, 1, 1, '{"execute":1,"tool:Bash":1}', 0),
  ('tn_2', 'default', 's2', 'ST-2', 'dev', 'claude-code', 5, 0, 1793493060000, 1793493600000, 'end_turn', NULL, 0, 0, 0, '{}', 0),
  ('tn_3', 'default', 's2', 'ST-2', 'dev', 'claude-code', 7, 0, 1793494200000, 1793494320000, 'error', '推理网关暂时不可用，5 秒后自动重试（第 1 次）', 1, 0, 0, '{}', 0),
  ('tn_4', 'default', 's2', 'ST-2', 'dev', 'claude-code', NULL, 1, 1793494320000, 1793494800000, 'end_turn', NULL, 0, 0, 0, '{}', 0),
  ('tn_5', 'default', 's3', 'ST-3', 'dev', 'claude-code', 10, 0, 1793497260000, 1793497500000, 'end_turn', NULL, 0, 0, 0, '{}', 0),
  ('tn_6', 'default', 's5', 'ST-5', 'dev', 'claude-code', 12, 0, 1793509260000, 1793509260000, 'interrupted', NULL, 0, 0, 0, '{}', 0);
INSERT INTO usage_records (workspace_id, ts, day, source, purpose, harness, model, session_id, item_id, bot_id, cost_reported, ok, attempts) VALUES
  ('default', 1793492400000, '2026-11-01', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 's1', 'ST-1', 'dev', 1.5, 1, 1),
  ('default', 1793496300000, '2026-11-01', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 's2', 'ST-2', 'dev', 2.0, 1, 1),
  ('default', 1793497500000, '2026-11-01', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 's3', 'ST-3', 'dev', NULL, 1, 1),
  ('default', 1793509500000, '2026-11-01', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 's5', 'ST-5', 'dev', 0.25, 1, 1);
INSERT INTO item_marks (workspace_id, item_id, ts, kind, from_v, to_v, actor, ref, detail) VALUES
  ('default', 'ST-1', 1793491200000, 'status', NULL, 'run', '', NULL, '{}'),
  ('default', 'ST-1', 1793491560000, 'fail', NULL, 'error', 'steve', 'f1', '{"why":"会话出错：boom"}'),
  ('default', 'ST-1', 1793491800000, 'deliver', NULL, 'manual', 'dev', 'a1', '{}'),
  ('default', 'ST-1', 1793492400000, 'status', 'run', 'done', 'usr_1', NULL, '{}'),
  ('default', 'ST-2', 1793493000000, 'status', NULL, 'run', '', NULL, '{}'),
  ('default', 'ST-2', 1793493600000, 'deliver', NULL, 'manual', 'dev', 'b1', '{}'),
  ('default', 'ST-2', 1793494800000, 'deliver', NULL, 'manual', 'dev', 'b2', '{}'),
  ('default', 'ST-2', 1793495700000, 'fail', NULL, 'release', 'steve', 'f2', '{"why":"发布  失败，已回到 v1：健康检查没有通过"}'),
  ('default', 'ST-2', 1793495700000, 'release', NULL, 'rolled_back', 'steve', 'r1', '{}'),
  ('default', 'ST-2', 1793496300000, 'status', 'run', 'done', 'steve', NULL, '{}'),
  ('default', 'ST-2', 1793496300000, 'release', NULL, 'released', 'steve', 'r2', '{}'),
  ('default', 'ST-3', 1793497200000, 'status', NULL, 'run', '', NULL, '{}'),
  ('default', 'ST-3', 1793497500000, 'deliver', NULL, 'auto', 'dev', NULL, '{}'),
  ('default', 'ST-3', 1793497501000, 'status', 'run', 'done', 'steve', NULL, '{}'),
  ('default', 'ST-4', 1793503200000, 'status', NULL, 'run', '', NULL, '{}'),
  ('default', 'ST-4', 1793503260000, 'status', 'run', 'pause', 'usr_1', NULL, '{}'),
  ('default', 'ST-4', 1793503320000, 'status', 'pause', 'drop', 'usr_1', NULL, '{}'),
  ('default', 'ST-5', 1793509200000, 'status', NULL, 'run', '', NULL, '{}'),
  ('default', 'ST-5', 1793509800000, 'fail', NULL, 'stall', 'steve', 'f5', '{"why":"它停下了：会话空闲"}');
"#;

fn m(x: i64) -> i64 {
    T0 + x * MIN
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s.bots()
        .seed(
            "dev",
            false,
            &NewBot {
                name: "开发".into(),
                harness: "claude-code".into(),
                model: Some("claude-opus-5-5".into()),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    let mut sql = String::from(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"UTC\"', 0);\n",
    );
    let item = |sql: &mut String, no: i64, status: &str, created: i64, updated: i64| {
        sql.push_str(&format!(
            "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, created, updated) VALUES ('ST-{no}', 'default', {no}, 't{no}', '{status}', '执行', 'dev', {created}, {updated});\n"
        ));
    };
    let session = |sql: &mut String, id: &str, item: &str, cost: &str, updated: i64| {
        sql.push_str(&format!(
            "INSERT INTO sessions (id, workspace_id, item_id, bot_id, harness, model, cwd, status, cost, created, updated) VALUES ('{id}', 'default', '{item}', 'dev', 'claude-code', NULL, '/tmp', 'idle', {cost}, {updated}, {updated});\n"
        ));
    };
    let ev = |sql: &mut String, s: &str, ts: i64, kind: &str, data: &str| {
        sql.push_str(&format!(
            "INSERT INTO events (session_id, ts, kind, data) VALUES ('{s}', {ts}, '{kind}', '{data}');\n"
        ));
    };
    let tl = |sql: &mut String, item: &str, ts: i64, actor: &str, kind: &str, text: &str| {
        sql.push_str(&format!(
            "INSERT INTO timeline (item_id, ts, actor, kind, text) VALUES ('{item}', {ts}, '{actor}', '{kind}', '{text}');\n"
        ));
    };
    #[allow(clippy::too_many_arguments)]
    let dec = |sql: &mut String,
               id: &str,
               item: &str,
               kind: &str,
               subject: &str,
               question: &str,
               status: &str,
               result: &str,
               created: i64,
               resolved: Option<i64>| {
        let resolved = resolved.map_or("NULL".to_string(), |r| r.to_string());
        sql.push_str(&format!(
            "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, result, requested_by, created, resolved)
             VALUES ('{id}', 'default', '{kind}', '{subject}', '{item}', '{item}', '{question}', '{status}', '{result}', 'dev', {created}, {resolved});\n"
        ));
    };

    // ST-1: one turn with a failed tool call, an ask and a fail card overlapping it, delivered and passed.
    item(&mut sql, 1, "done", m(0), m(25));
    session(&mut sql, "s1", "ST-1", "1.5", m(20));
    ev(&mut sql, "s1", m(1), "user", r#"{"text":"go"}"#);
    ev(
        &mut sql,
        "s1",
        m(2),
        "tool_call",
        r#"{"toolCallId":"a","kind":"execute","_meta":{"claudeCode":{"toolName":"Bash"}}}"#,
    );
    ev(
        &mut sql,
        "s1",
        m(3),
        "tool_call_update",
        r#"{"toolCallId":"a","status":"failed"}"#,
    );
    ev(
        &mut sql,
        "s1",
        m(10),
        "turn_end",
        r#"{"stopReason":"end_turn"}"#,
    );
    tl(&mut sql, "ST-1", m(5), "me", "me", "补充一下");
    tl(&mut sql, "ST-1", m(10), "dev", "sys", "交付，等你验收");
    tl(&mut sql, "ST-1", m(20), "usr_1", "me", "验收通过");
    dec(
        &mut sql,
        "q1",
        "ST-1",
        "ask",
        "question",
        "选哪个",
        "resolved",
        r#"{"choice":"a"}"#,
        m(4),
        Some(m(8)),
    );
    dec(
        &mut sql,
        "f1",
        "ST-1",
        "fail",
        "item",
        "会话出错：boom",
        "resolved",
        r#"{"choice":"continue"}"#,
        m(6),
        Some(m(12)),
    );
    dec(
        &mut sql,
        "a1",
        "ST-1",
        "accept",
        "item",
        "做完了，请验收",
        "resolved",
        r#"{"choice":"pass"}"#,
        m(10),
        Some(m(20)),
    );

    // ST-2: a rework, a gateway retry, a release that rolled back, then one that went out.
    item(&mut sql, 2, "done", m(30), m(86));
    session(&mut sql, "s2", "ST-2", "2.0", m(85));
    ev(&mut sql, "s2", m(31), "user", r#"{"text":"go"}"#);
    ev(
        &mut sql,
        "s2",
        m(40),
        "turn_end",
        r#"{"stopReason":"end_turn"}"#,
    );
    ev(&mut sql, "s2", m(50), "user", r#"{"text":"返工"}"#);
    ev(
        &mut sql,
        "s2",
        m(52),
        "system",
        r#"{"text":"推理网关暂时不可用，5 秒后自动重试（第 1 次）"}"#,
    );
    ev(
        &mut sql,
        "s2",
        m(60),
        "turn_end",
        r#"{"stopReason":"end_turn"}"#,
    );
    tl(&mut sql, "ST-2", m(40), "dev", "sys", "交付，等你验收");
    tl(&mut sql, "ST-2", m(60), "dev", "sys", "交付，等你验收");
    tl(&mut sql, "ST-2", m(70), "usr_1", "me", "验收通过");
    tl(
        &mut sql,
        "ST-2",
        m(75),
        "steve",
        "sys",
        "发布  失败，已回到 v1：健康检查没有通过",
    );
    tl(&mut sql, "ST-2", m(85), "steve", "sys", "已发布新版本 v2");
    dec(
        &mut sql,
        "b1",
        "ST-2",
        "accept",
        "item",
        "做完了，请验收",
        "resolved",
        r#"{"choice":"rework"}"#,
        m(40),
        Some(m(45)),
    );
    dec(
        &mut sql,
        "y1",
        "ST-2",
        "approve",
        "tool_call",
        "rm",
        "resolved",
        r#"{"choice":"yes","decided_by":"yolo"}"#,
        m(55),
        Some(m(56)),
    );
    dec(
        &mut sql,
        "b2",
        "ST-2",
        "accept",
        "item",
        "做完了，请验收",
        "resolved",
        r#"{"choice":"pass"}"#,
        m(60),
        Some(m(70)),
    );
    dec(
        &mut sql,
        "r1",
        "ST-2",
        "approve",
        "release",
        "发布新版本（P · 分支 x）",
        "resolved",
        r#"{"approved":true,"text":""}"#,
        m(70),
        Some(m(72)),
    );
    dec(
        &mut sql,
        "f2",
        "ST-2",
        "fail",
        "item",
        "发布  失败，已回到 v1：健康检查没有通过",
        "resolved",
        r#"{"choice":"continue"}"#,
        m(75),
        Some(m(80)),
    );
    dec(
        &mut sql,
        "r2",
        "ST-2",
        "approve",
        "release",
        "发布新版本（P · 分支 x）",
        "resolved",
        r#"{"approved":true,"outcome":"released"}"#,
        m(80),
        Some(m(81)),
    );

    // ST-3: accepted by itself.
    item(&mut sql, 3, "done", m(100), m(106));
    session(&mut sql, "s3", "ST-3", "NULL", m(105));
    ev(&mut sql, "s3", m(101), "user", r#"{"text":"go"}"#);
    ev(
        &mut sql,
        "s3",
        m(105),
        "turn_end",
        r#"{"stopReason":"end_turn"}"#,
    );
    tl(&mut sql, "ST-3", m(105), "dev", "sys", "交付，等你验收");
    tl(
        &mut sql,
        "ST-3",
        m(105) + 1_000,
        "steve",
        "sys",
        "自检全部通过，自动验收",
    );

    // ST-4: paused, then dropped.
    item(&mut sql, 4, "drop", m(200), m(202));
    sql.push_str(&format!(
        "INSERT INTO audit (workspace_id, ts, actor, action, target) VALUES ('default', {}, 'usr_1', 'item.pause', 'ST-4'), ('default', {}, 'usr_1', 'item.drop', 'ST-4');\n",
        m(201),
        m(202)
    ));

    // ST-5: still running, a turn the Hub never saw end, and a stall card.
    item(&mut sql, 5, "run", m(300), m(310));
    session(&mut sql, "s5", "ST-5", "0.25", m(305));
    ev(&mut sql, "s5", m(301), "user", r#"{"text":"go"}"#);
    dec(
        &mut sql,
        "f5",
        "ST-5",
        "fail",
        "stall",
        "它停下了：会话空闲",
        "open",
        "",
        m(310),
        None,
    );

    sql.push_str(RECORDED);
    s.with_conn(|c| Ok(c.execute_batch(&sql)?)).unwrap();
    refresh::rebuild(&s, WS).unwrap();
    (dir, s)
}

fn snapshot(s: &Store) -> String {
    s.with_conn(|c| {
        let q = |sql: &str| -> String {
            c.prepare(sql)
                .unwrap()
                .query_map([], |r| {
                    let n = r.as_ref().column_count();
                    Ok((0..n)
                        .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok([
            q("SELECT session_id, user_seq, attempt, started, ended, stop_reason, error, transient, tools, tool_errors, tool_kinds, permissions FROM agent_turns ORDER BY session_id, started, attempt"),
            q("SELECT item_id, ts, kind, from_v, to_v, actor, ref, detail FROM item_marks ORDER BY item_id, ts, kind"),
            q("SELECT * FROM stat_daily ORDER BY 1, 2, 3, 4, 5, 6, 7, 8"),
            q("SELECT workspace_id, ts, day, source, purpose, harness, model, session_id, item_id, cost_reported FROM usage_records ORDER BY session_id"),
            q("SELECT item_id, first_turn, first_deliver, last_deliver, accepted, done_at, dropped_at, lead_ms, cycle_ms, CASE WHEN done_at IS NULL AND dropped_at IS NULL THEN NULL ELSE wait_ms END, agent_ms, turns, delivers, reworks, first_pass, fails, stalls, releases, release_fails, decisions, yolo_decided FROM item_stats ORDER BY item_id"), // an open card's wait grows with the clock
        ]
        .join("\n--\n"))
    })
    .unwrap()
}

fn day_query() -> Query {
    Query {
        workspace_id: WS.into(),
        from: "2026-11-01".into(),
        to: "2026-11-01".into(),
        ..Query::default()
    }
}

#[test]
fn metrics_match_the_hand_count() {
    // turns: ST-1 one, ST-2 three (the retry splits one), ST-3 one, ST-5 one
    let (_d, s) = store();

    let sm = s.with_conn(|c| summary(c, &day_query())).unwrap().unwrap();
    assert_eq!(sm.tz, "UTC");

    let th = &sm.throughput;
    assert_eq!(
        (th.created, th.done, th.dropped, th.wip, th.net),
        (5, 3, 1, 1, 1)
    );
    assert_eq!(th.drop_rate, Some(0.25));
    assert_eq!(
        (th.delivered, th.delivered_auto, th.delivered_manual),
        (4, 1, 3)
    );

    // D1: ST-1 20 min, ST-2 55 min, ST-3 5 min 1 s
    let lead = &sm.time.lead;
    assert_eq!(lead.n, 3);
    assert_eq!(lead.p50, Some(20 * MIN));
    assert_eq!(lead.p85, Some(55 * MIN));
    assert_eq!(lead.avg, Some((75 * MIN + 5 * MIN + 1_000) as f64 / 3.0));
    // D4: ST-1 = [4, 20] (ask 4–8, fail 6–12, accept 10–20 overlap); ST-2 = 5 + 10 + 2 + 5 + 1, the YOLO card not counted
    assert_eq!(sm.time.wait.sum, (16 + 23) * MIN);
    // D5: ST-1 9 min, ST-2 9 + 2 + 8, ST-3 4
    assert_eq!(sm.time.agent.sum, (9 + 19 + 4) * MIN);
    // D2: accepted delivery − first turn: 9, 29, 4 min
    assert_eq!(sm.time.cycle.sum, (9 + 29 + 4) * MIN);
    assert_eq!(sm.time.cycle.p50, Some(9 * MIN));
    // D7, D8
    assert_eq!((sm.time.releases, sm.time.released), (2, 1));
    assert_eq!(sm.time.release_success_rate, Some(0.5));
    assert_eq!(sm.time.accept_to_release.p50, Some(15 * MIN));

    let q = &sm.quality;
    assert_eq!((q.first_pass, q.first_pass_n), (Some(2.0 / 3.0), 3));
    assert_eq!(q.reworks, 1);
    assert_eq!(q.rollbacks, 1);
    assert_eq!(
        q.fails
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        [
            ("error", 1),
            ("other", 0),
            ("release", 1),
            ("restart", 0),
            ("stall", 1)
        ]
    );
    assert_eq!(q.stalled_then_done, Some(0.0));
    assert_eq!(q.turn_error_rate, Some(0.0));

    let h = &sm.human;
    assert_eq!(h.decisions, 10);
    assert_eq!(h.by_kind["accept/item"], 3);
    assert_eq!(h.by_kind["fail/item"], 2);
    assert_eq!(h.by_kind["fail/stall"], 1);
    assert_eq!(h.by_kind["approve/release"], 2);
    assert_eq!(h.by_kind["approve/tool_call"], 1);
    assert_eq!(h.by_kind["ask/question"], 1);
    // 8 resolved by a person (the YOLO one and the open stall card are not)
    assert_eq!(h.response.n, 8);
    assert_eq!(h.messages, 1);
    // migration 14 (YOLO) is in: H4 is reported, from the ledger, which this fixture leaves empty
    // (y1 only carries `decided_by: yolo` on the decision)
    let y = h.yolo.as_ref().expect("yolo_actions exists");
    assert_eq!(
        (y.decided, y.declined, y.undone, y.rate),
        (0, 0, 0, Some(0.0))
    );

    let a = &sm.agents;
    assert_eq!((a.turns, a.retries, a.interrupted), (5, 1, 1));
    assert_eq!(
        (a.tools, a.tool_kinds["execute"], a.tool_kinds["tool:Bash"]),
        (1, 1, 1)
    );
    assert_eq!(a.tool_error_rate, Some(1.0));
    assert_eq!(a.retry_success, Some(1.0));

    // the sessions' own dollar figures, with no token counts
    assert!((sm.usage.reported - 3.75).abs() < 1e-9);
    assert_eq!(sm.usage.total, 0);
    assert_eq!(sm.usage.hit_rate, None);
    assert_eq!(sm.since.tokens.as_deref(), Some("2026-11-01"));

    // marks of the dropped item: run → pause → drop
    let st4: Vec<(Option<String>, Option<String>)> = s
        .with_conn(|c| {
            Ok(c.prepare("SELECT from_v, to_v FROM item_marks WHERE item_id = 'ST-4' AND kind = 'status' ORDER BY ts")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .unwrap();
    assert_eq!(
        st4,
        [
            (None, Some("run".into())),
            (Some("run".into()), Some("pause".into())),
            (Some("pause".into()), Some("drop".into()))
        ]
    );

    // WIP at the end of the day: only ST-5
    let wip = s.with_conn(|c| wip_by_day(c, &day_query())).unwrap();
    assert_eq!(wip, [("2026-11-01".to_string(), 1)]);

    // the drill-down lists hold exactly the items behind the counts
    let ids = |only: &str| -> Vec<String> {
        let mut v: Vec<String> = s
            .with_conn(|c| views::items(c, &day_query(), "est", Some(only), None, 100, 0))
            .unwrap()
            .rows
            .into_iter()
            .map(|r| r.item_id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(ids("done").len() as i64, th.done);
    assert_eq!(ids("created").len() as i64, th.created);
    assert_eq!(ids("done"), ["ST-1", "ST-2", "ST-3"]);
}

#[test]
fn a_rebuild_changes_nothing() {
    let (_d, s) = store();
    let a = snapshot(&s);
    refresh::rebuild(&s, WS).unwrap();
    assert_eq!(a, snapshot(&s));
}

#[test]
fn invariants_hold() {
    let (_d, s) = store();
    // token records on top, for the breakdown sums
    let t = steve_store::Tokens {
        input: Some(100),
        output: Some(50),
        cache_read: Some(900),
        cache_write: Some(10),
        reasoning: None,
    };
    for (model, source, bot) in [
        ("claude-opus-5-5", "agent", Some("dev")),
        ("claude-haiku-5-5", "gateway", None),
        ("gpt-9", "agent", Some("dev")),
    ] {
        s.stats()
            .usage_add(&steve_store::NewUsage {
                workspace_id: WS.into(),
                ts: m(400),
                day: "2026-11-01".into(),
                source: source.into(),
                purpose: "turn".into(),
                harness: "claude-code".into(),
                turn_id: None,
                session_id: None,
                item_id: Some("ST-1".into()),
                bot_id: bot.map(Into::into),
                project_id: None,
                usage: steve_store::ModelUsage {
                    model: model.into(),
                    tokens: t,
                    ..Default::default()
                },
                duration_ms: None,
                ok: true,
                attempts: 1,
            })
            .unwrap();
    }
    refresh::refresh_all(&s).unwrap();
    let q = day_query();
    let (sm, rows, prices) = s
        .with_conn(|c| Ok((summary(c, &q)?.unwrap(), daily(c, &q)?, prices_of(c, WS)?)))
        .unwrap();
    for by in [
        By::Model,
        By::Bot,
        By::Project,
        By::Source,
        By::Purpose,
        By::Harness,
        By::Day,
    ] {
        let parts = breakdown(&rows, by, &steve_stats::PriceBook::new(prices.clone()));
        assert_eq!(
            parts.iter().map(|r| r.usage.total).sum::<i64>(),
            sm.usage.total
        );
        assert_eq!(
            parts.iter().map(|r| r.usage.turns).sum::<i64>(),
            sm.usage.turns
        );
        let est: f64 = parts.iter().map(|r| r.usage.est).sum();
        assert!((est - sm.usage.est).abs() < 1e-9);
        let rep: f64 = parts.iter().map(|r| r.usage.reported).sum();
        assert!((rep - sm.usage.reported).abs() < 1e-9);
    }
    assert_eq!(sm.usage.total, 3 * 1060);
    assert_eq!(sm.spend.unpriced_tokens, 1060);
    assert_eq!(sm.usage.hit_rate, Some(2700.0 / 3030.0));
    assert_eq!(sm.since.tokens.as_deref(), Some("2026-11-01"));
    // a filter is a subset that adds up the same way
    let only_dev = Query {
        bot: Some("dev".into()),
        ..day_query()
    };
    let dev = s.with_conn(|c| summary(c, &only_dev)).unwrap().unwrap();
    assert_eq!(dev.usage.total, 2 * 1060);

    // D4 ≤ D1 and D5 ≤ D1 for every finished item; the per-item tokens include the live records
    type Facts = (String, Option<i64>, Option<i64>, Option<i64>, i64);
    let rows: Vec<Facts> = s
        .with_conn(|c| {
            Ok(c.prepare("SELECT item_id, lead_ms, wait_ms, agent_ms, input + output + cache_read + cache_write FROM item_stats")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .unwrap();
    for (id, lead, wait, agent, tokens) in rows {
        if let Some(l) = lead {
            assert!(wait.unwrap_or(0) <= l && agent.unwrap_or(0) <= l, "{id}");
        }
        assert_eq!(tokens, if id == "ST-1" { 3 * 1060 } else { 0 }, "{id}");
    }
    assert_eq!(sm.per_item.tokens.sum, 3 * 1060);
}

#[test]
fn idle_refresher_does_not_wait_for_the_store_writer() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (_dir, s) = store();
    refresh::refresh_all(&s).unwrap();
    let mut refresher = steve_stats::Refresher::new();
    assert_eq!(refresher.tick(&s, 200).unwrap(), 0);

    let held = s.clone();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        held.with_conn(|_| {
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
        .unwrap();
    });
    locked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let refresh = std::thread::spawn(move || {
        done_tx.send(refresher.tick(&s, 200)).unwrap();
    });
    // Always release the writer before asserting, so a regression fails rather than hangs.
    let result = done_rx.recv_timeout(Duration::from_secs(2));
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    refresh.join().unwrap();
    assert_eq!(result.expect("idle tick waited for the writer").unwrap(), 0);
}

#[test]
fn refresher_still_processes_dirty_rows_and_new_decisions() {
    let (_dir, s) = store();
    refresh::refresh_all(&s).unwrap();
    let mut refresher = steve_stats::Refresher::new();
    assert_eq!(refresher.tick(&s, 200).unwrap(), 0);
    s.stats().touch_item("ST-1").unwrap();
    assert!(s.stats().refresh_pending(0).unwrap());
    assert_eq!(refresher.tick(&s, 200).unwrap(), 1);
    assert!(!s.stats().refresh_pending(0).unwrap());
    s.with_conn(|c| {
        c.execute(
            "UPDATE decisions SET resolved = ? WHERE id = 'q1'",
            [steve_store::now()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        s.stats()
            .refresh_pending(steve_store::now() - 1_000)
            .unwrap()
    );
    assert_eq!(refresher.tick(&s, 200).unwrap(), 1);
    assert!(!s.stats().refresh_pending(0).unwrap());
}
