//! Typed statistics fixtures and direct checks of persisted projections.
use crate::{Result, Store};
use rusqlite::params;

pub fn seed_active(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        Ok(c.execute_batch("INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"Asia/Shanghai\"', 0);")?)
    })
}

pub fn seed_events(s: &Store, m: impl Fn(i64) -> i64) -> Result<()> {
    let sql = format!(
        "INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES ('p1', 'default', 'one', '/tmp/p1', 0, 0), ('p2', 'default', 'two', '/tmp/p2', 0, 0);
         INSERT INTO items(id, workspace_id, no, title, phase, created, updated, project_id, outcome) VALUES ('ST-1', 'default', 1, 'one', 'closed', {c}, {c}, 'p1', 'completed'),('ST-2', 'default', 2, 'two', 'active', {c}, {c}, 'p2', NULL);
         INSERT INTO agent_turns (id, workspace_id, session_id, item_id, project_id, harness, started, ended, stop_reason, transient, tools, tool_errors) VALUES
           ('tn_1', 'default', 's1', 'ST-1', 'p1', 'claude-code', {t1}, {t1e}, 'end_turn', 0, 3, 0),
           ('tn_2', 'default', 's2', 'ST-2', 'p2', 'claude-code', {t2}, {t2e}, 'error', 0, 1, 1),
           ('tn_3', 'default', 's2', 'ST-2', 'p2', 'claude-code', {t3}, NULL, NULL, 0, 0, 0);
         INSERT INTO usage_records (workspace_id, ts, day, source, purpose, harness, model, turn_id, session_id, item_id, input, output, cache_read, cache_write) VALUES
           ('default', {t1e}, '2026-11-01', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 'tn_1', 's1', 'ST-1', 100, 20, 1000, 0);
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, result, requested_by, created, resolved) VALUES
           ('d1', 'default', 'approve', 'item', 'ST-1', 'ST-1', 'q', 'resolved', '{{\"decided_by\":\"yolo\"}}', 'dev', {d1}, {d1r}),
           ('d2', 'default', 'fail', 'item', 'ST-2', 'ST-2', 'q', 'open', NULL, 'dev', {d2}, NULL);
         INSERT INTO item_marks (workspace_id, item_id, ts, kind, from_v, to_v, actor) VALUES
           ('default', 'ST-1', {k1}, 'status', 'active', 'completed', 'usr_1'),
           ('default', 'ST-2', {k2}, 'release', NULL, 'rolled_back', 'steve');",
        c = m(0),
        t1 = m(5),
        t1e = m(15),
        t2 = m(30),
        t2e = m(32),
        t3 = m(50),
        d1 = m(16),
        d1r = m(18),
        d2 = m(33),
        k1 = m(20),
        k2 = m(40),
    );
    s.write_locked(|c| Ok(c.execute_batch(&sql)?))
}

/// (id, item, kind, created, resolved, decided by YOLO)
pub type Card<'a> = (&'a str, Option<&'a str>, &'a str, i64, Option<i64>, bool);

pub fn seed_time(s: &Store, cards: &[Card<'_>]) -> Result<()> {
    let mut sql = String::from(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"Asia/Shanghai\"', 0);
         INSERT INTO sessions (id, workspace_id, harness, cwd, status, created, updated) VALUES ('s1', 'default', 'claude-code', '/tmp', 'idle', 0, 0);
         INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('ST-1', 'default', 1, 't1', 'active', 0, 0);
         INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('ST-2', 'default', 2, 't2', 'active', 0, 0);\n",
    );
    for (id, item, kind, created, resolved, yolo) in cards {
        let item = item.map_or("NULL".into(), |i| format!("'{i}'"));
        let resolved = resolved.map_or("NULL".into(), |r| r.to_string());
        let status = if resolved == "NULL" {
            "open"
        } else {
            "resolved"
        };
        let result = if *yolo {
            "'{\"decided_by\":\"yolo\"}'"
        } else {
            "NULL"
        };
        sql.push_str(&format!(
            "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, result, requested_by, created, resolved)
             VALUES ('{id}', 'default', '{kind}', 'item', 'x', {item}, '?', '{status}', {result}, 'dev', {created}, {resolved});\n"
        ));
    }
    s.write_locked(|c| Ok(c.execute_batch(&sql)?))
}

pub fn seed_delivery(s: &Store, m: impl Fn(i64) -> i64) -> Result<()> {
    let mut sql = String::from(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"UTC\"', 0);\n",
    );
    for no in [1, 5] {
        sql.push_str(&format!(
            "INSERT INTO items(id, workspace_id, no, title, phase, bot_id, source, created, updated, outcome) VALUES ('ST-{no}', 'default', {no}, 't', 'closed', 'dev', '[\"对话\",\"chat\"]', {t}, {t}, 'completed');\n",
            t = m(0)
        ));
    }
    // sessions: (id, item, created, first_tool_at); sw1's first tool call is only an event
    for (id, item, created, first) in [
        ("sc", "ST-1", m(0), Some(m(2))),
        ("sw1", "ST-1", m(1), None),
        ("sr1", "ST-1", m(11), None),
        ("s5", "ST-5", m(60), Some(m(61))),
    ] {
        sql.push_str(&format!(
            "INSERT INTO sessions (id, workspace_id, item_id, bot_id, harness, cwd, status, created, updated, first_tool_at)
             VALUES ('{id}', 'default', '{item}', 'dev', 'claude-code', '/', 'ended', {created}, {created}, {});\n",
            first.map_or("NULL".into(), |f| f.to_string())
        ));
    }
    // Keep sr1 on a separate open item so ST-1 has 2 sessions and ST-5 has 1, while sr1 still tests no_tool.
    sql.push_str(&format!(
        "INSERT INTO items(id, workspace_id, no, title, phase, bot_id, source, created, updated, outcome) VALUES ('ST-2', 'default', 2, 't', 'active', 'dev', '[\"对话\",\"chat\"]', {t0}, {t0}, NULL);
         UPDATE sessions SET item_id='ST-2' WHERE id='sr1';
         INSERT INTO events (session_id, ts, kind, data) VALUES ('sw1', {}, 'tool_call', '{{}}'), ('sw1', {}, 'tool_call', '{{}}');
         INSERT INTO item_stats (item_id, workspace_id, created, accepted, done_at, updated) VALUES
           ('ST-1', 'default', {t0}, {}, {}, {t0}), ('ST-5', 'default', {t0}, NULL, {}, {t0});
         INSERT INTO item_marks (workspace_id, item_id, ts, kind, to_v, actor, detail) VALUES
           ('default', 'ST-1', {}, 'invalidate', 'plan', 'steve', '{{}}'),
           ('default', 'ST-1', {}, 'merge', 'merged', 'steve', '{{}}'),
           ('default', 'ST-1', {}, 'reject', 'design_file', 'dev', '{{}}'),
           ('default', 'ST-5', {}, 'deliver', 'auto', 'dev', '{{}}');
         INSERT INTO chan_messages (id, workspace_id, space_id, external_id, reply_to, direction, status, created, sent) VALUES
           ('mi', 'default', 'sp', 'om_1', NULL, 'in', 'received', {}, NULL),
           ('mo1', 'default', 'sp', 'om_2', 'om_1', 'out', 'sent', {}, {}),
           ('mo2', 'default', 'sp', 'om_3', 'om_1', 'out', 'sent', {}, {});
         PRAGMA foreign_keys = ON;",
        m(3), m(4),
        m(60), m(70), m(65),
        m(52), m(68), m(29), m(64),
        m(100), m(101), m(102), m(104), m(105),
        t0 = m(0),
    ));
    s.write_locked(|c| Ok(c.execute_batch(&sql)?))
}

pub fn seed_hourly(s: &Store, items: [(i64, i64, Option<i64>); 5]) -> Result<()> {
    let mut sql = String::from(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"Asia/Shanghai\"', 0);
         INSERT INTO sessions (id, workspace_id, bot_id, harness, cwd, status, created, updated) VALUES ('s1', 'default', 'dev', 'claude-code', '/tmp', 'idle', 0, 0);\n",
    );
    for (no, created, done) in items {
        let (phase, outcome) = if done.is_some() {
            ("closed", "'completed'")
        } else {
            ("active", "NULL")
        };
        sql.push_str(&format!(
            "INSERT INTO items(id, workspace_id, no, title, phase, outcome, bot_id, created, updated) VALUES ('ST-{no}', 'default', {no}, 't{no}', '{phase}', {outcome}, 'dev', {created}, {created});\n"
        ));
    }
    s.write_locked(|c| Ok(c.execute_batch(&sql)?))
}

/// `(day, source, purpose, harness, model, bot, turns, turn_ms, interrupted, calls, input)` of a cell.
pub type CubeRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
);

/// `stat_daily` as the live writer left it, one [`CubeRow`] per cell.
pub fn cube(s: &Store) -> Result<Vec<CubeRow>> {
    s.write_locked(|c| {
        let mut st = c.prepare(
            "SELECT day, source, purpose, harness, model, bot_id, turns, turn_ms, interrupted, calls, input FROM stat_daily
              WHERE workspace_id = 'default' ORDER BY day, source, purpose, harness, model, bot_id, project_id",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

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
  ('default', 'ST-1', 1793491200000, 'status', NULL, 'active', '', NULL, '{}'),
  ('default', 'ST-1', 1793491560000, 'fail', NULL, 'error', 'steve', 'f1', '{"why":"会话出错：boom"}'),
  ('default', 'ST-1', 1793491800000, 'deliver', NULL, 'manual', 'dev', 'a1', '{}'),
  ('default', 'ST-1', 1793492400000, 'status', 'active', 'completed', 'usr_1', NULL, '{}'),
  ('default', 'ST-2', 1793493000000, 'status', NULL, 'active', '', NULL, '{}'),
  ('default', 'ST-2', 1793493600000, 'deliver', NULL, 'manual', 'dev', 'b1', '{}'),
  ('default', 'ST-2', 1793494800000, 'deliver', NULL, 'manual', 'dev', 'b2', '{}'),
  ('default', 'ST-2', 1793495700000, 'fail', NULL, 'release', 'steve', 'f2', '{"why":"发布  失败，已回到 v1：健康检查没有通过"}'),
  ('default', 'ST-2', 1793495700000, 'release', NULL, 'rolled_back', 'steve', 'r1', '{}'),
  ('default', 'ST-2', 1793496300000, 'status', 'active', 'completed', 'steve', NULL, '{}'),
  ('default', 'ST-2', 1793496300000, 'release', NULL, 'released', 'steve', 'r2', '{}'),
  ('default', 'ST-3', 1793497200000, 'status', NULL, 'active', '', NULL, '{}'),
  ('default', 'ST-3', 1793497500000, 'deliver', NULL, 'auto', 'dev', NULL, '{}'),
  ('default', 'ST-3', 1793497501000, 'status', 'active', 'completed', 'steve', NULL, '{}'),
  ('default', 'ST-4', 1793503200000, 'status', NULL, 'active', '', NULL, '{}'),
  ('default', 'ST-4', 1793503260000, 'status', 'active', 'paused', 'usr_1', NULL, '{}'),
  ('default', 'ST-4', 1793503320000, 'status', 'paused', 'cancelled', 'usr_1', NULL, '{}'),
  ('default', 'ST-5', 1793509200000, 'status', NULL, 'active', '', NULL, '{}'),
  ('default', 'ST-5', 1793509800000, 'fail', NULL, 'stall', 'steve', 'f5', '{"why":"它停下了：会话空闲"}');
"#;

pub fn seed_fixture(s: &Store, m: impl Fn(i64) -> i64) -> Result<()> {
    let mut sql = String::from(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"UTC\"', 0);\n",
    );
    let item = |sql: &mut String, no: i64, outcome: Option<&str>, created: i64, updated: i64| {
        let phase = if outcome.is_some() {
            "closed"
        } else {
            "active"
        };
        let outcome = outcome.map_or_else(|| "NULL".into(), |value| format!("'{value}'"));
        sql.push_str(&format!(
            "INSERT INTO items(id, workspace_id, no, title, phase, outcome, bot_id, created, updated) VALUES ('ST-{no}', 'default', {no}, 't{no}', '{phase}', {outcome}, 'dev', {created}, {updated});\n"
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
        let result = if result.is_empty() {
            "NULL".into()
        } else {
            format!("'{result}'")
        };
        sql.push_str(&format!(
            "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, result, requested_by, created, resolved)
             VALUES ('{id}', 'default', '{kind}', '{subject}', '{item}', '{item}', '{question}', '{status}', {result}, 'dev', {created}, {resolved});\n"
        ));
    };

    // ST-1: one turn with a failed tool call, an ask and a fail card overlapping it, delivered and passed.
    item(&mut sql, 1, Some("completed"), m(0), m(25));
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
    item(&mut sql, 2, Some("completed"), m(30), m(86));
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
    item(&mut sql, 3, Some("completed"), m(100), m(106));
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
    item(&mut sql, 4, Some("cancelled"), m(200), m(202));
    sql.push_str(&format!(
        "INSERT INTO audit (workspace_id, ts, actor, action, target) VALUES ('default', {}, 'usr_1', 'item.pause', 'ST-4'), ('default', {}, 'usr_1', 'item.drop', 'ST-4');\n",
        m(201),
        m(202)
    ));

    // ST-5: still running, a turn the Hub never saw end, and a stall card.
    item(&mut sql, 5, None, m(300), m(310));
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
    s.write_locked(|c| Ok(c.execute_batch(&sql)?))
}

pub fn snapshot(s: &Store) -> Result<String> {
    s.write_locked(|c| {
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
            q("SELECT item_id, first_turn, first_deliver, last_deliver, accepted, done_at, dropped_at, lead_ms, cycle_ms, CASE WHEN done_at IS NULL AND dropped_at IS NULL THEN NULL ELSE wait_ms END, agent_ms, turns, delivers, reworks, first_pass, fails, stalls, releases, release_fails, decisions FROM item_stats ORDER BY item_id"), // an open card's wait grows with the clock
        ]
        .join("\n--\n"))
    })
}

pub fn dropped_status_marks(s: &Store) -> Result<Vec<(Option<String>, Option<String>)>> {
    s
        .write_locked(|c| {
            Ok(c.prepare("SELECT from_v, to_v FROM item_marks WHERE item_id = 'ST-4' AND kind = 'status' ORDER BY ts")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
}

pub fn item_totals(s: &Store) -> Result<Vec<ItemTotals>> {
    s
        .write_locked(|c| {
            Ok(c.prepare("SELECT item_id, lead_ms, wait_ms, agent_ms, input + output + cache_read + cache_write FROM item_stats")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
}

pub type ItemTotals = (String, Option<i64>, Option<i64>, Option<i64>, i64);

pub fn resolve_question(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "UPDATE decisions SET resolved = ? WHERE id = 'q1'",
            [crate::now()],
        )?;
        Ok(())
    })
}

pub fn seed_tree(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        Ok(c.execute_batch(
            r#"UPDATE items SET parent = '{"item":"ST-1"}' WHERE id IN ('ST-2', 'ST-3');
               UPDATE items SET parent = '{"item":"ST-3","session":"s3","at":2}' WHERE id = 'ST-5';
               UPDATE items SET parent = 'not json' WHERE id = 'ST-4';"#,
        )?)
    })
}

pub fn seed_tree_chain(s: &Store, depth: i64) -> Result<()> {
    let chain = (6..=6 + depth + 1)
        .map(|no| {
            format!(
                "INSERT INTO items(id, workspace_id, no, title, phase, bot_id, created, updated, parent) VALUES ('ST-{no}', 'default', {no}, 't', 'active', 'dev', 0, 0, '{{\"item\":\"ST-{}\"}}');",
                no - 1
            )
        })
        .collect::<String>();
    s.write_locked(|c| Ok(c.execute_batch(&chain)?))
}

pub fn seed_old_meter(s: &Store, turn: &str, report: &impl serde::Serialize) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "UPDATE usage_meters SET payload=? WHERE turn_id=?",
            params![serde_json::to_string(report)?, turn],
        )?;
        Ok(())
    })
}

pub fn realtime_attempt(
    s: &Store,
    id: &str,
    session: &str,
    ws: &str,
    project: &str,
    bot: &str,
    start: i64,
) -> Result<()> {
    s.write_locked(|c| {
            c.execute("INSERT OR IGNORE INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,?,'test','.','running',?,?)",params![session,ws,start,start])?;
            c.execute("INSERT OR IGNORE INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?,'n','{}',?,'r','busy',?)",params![session,session,start])?;
            c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?,?,'finished',?,?)",params![id,session,start,start])?;
            c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES (?,?,0,'n',?,'{}','{}','d','running',?,?)",params![id,id,id,start,start])?;
            c.execute("INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,project_id,bot_id,node_id,started) VALUES (?,?,?,?,?,'n',?)",params![id,session,ws,project,bot,start])?;
            Ok(())
        })
}

pub fn realtime_sample(
    s: &Store,
    id: &str,
    seq: i64,
    at: i64,
    units: i64,
    kind: &str,
    received: i64,
) -> Result<()> {
    s.write_locked(|c| {
            c.execute("INSERT INTO realtime_output_samples(node_id,stream,epoch,seq,event_id,attempt_id,at,source_at,received_at,kind,units) VALUES ('n',?,'e',?,?,?,?,?,?,?,?)",params![id,seq,format!("{id}-{seq}"),id,at,at,received,kind,units])?;
            Ok(())
        })
}

pub fn realtime_end(s: &Store, id: &str, at: i64) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "UPDATE realtime_output_attempts SET ended=? WHERE attempt_id=?",
            params![at, id],
        )?;
        c.execute(
            "UPDATE runtime_attempts SET state='finished',updated=? WHERE attempt_id=?",
            params![at, id],
        )?;
        Ok(())
    })
}

pub fn move_session(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "UPDatE sessions SET workspace_id='elsewhere' WHERE id='s'",
            [],
        )?;
        Ok(())
    })
}

pub fn remove_baseline(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "DELETE FROM realtime_output_attempts WHERE attempt_id='b'",
            [],
        )?;
        Ok(())
    })
}

pub fn settle_attempt(s: &Store, at: i64) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "UPDatE runtime_attempts SET state='finished',updated=? WHERE attempt_id='a'",
            [at],
        )?;
        Ok(())
    })
}

pub fn settle_baselineless_attempt(s: &Store, at: i64) -> Result<()> {
    s.write_locked(|c| {
        c.execute(
            "DELETE FROM realtime_output_attempts WHERE attempt_id='old'",
            [],
        )?;
        c.execute(
            "UPDatE runtime_attempts SET state='finished',updated=? WHERE attempt_id='old'",
            [at],
        )?;
        Ok(())
    })
}

pub fn seed_uninstrumented_session(s: &Store, at: i64) -> Result<()> {
    s.write_locked(|c| {
        c.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('legacy','default','custom','.','running',?,?)", params![at-60_000,at-1_000])?;
        Ok(())
    })
}

pub fn end_uninstrumented_session(s: &Store) -> Result<()> {
    s.write_locked(|c| {
        c.execute("UPDatE sessions SET status='ended' WHERE id='legacy'", [])?;
        Ok(())
    })
}

pub struct RealtimeV73(rusqlite::Connection);
impl RealtimeV73 {
    pub fn new(path: &std::path::Path, at: i64) -> Self {
        use rusqlite::Connection;
        let c = Connection::open(path).unwrap();
        c.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL)")
            .unwrap();
        for sql in &crate::db::MIGRATIONS[..73] {
            c.execute_batch(sql).unwrap();
        }
        c.execute_batch("INSERT INTO schema_version VALUES(73); INSERT INTO workspaces(id,name,created) VALUES('default','test',1);").unwrap();
        let f = Self(c);
        f.attempt("a", at);
        f
    }
    fn attempt(&self, id: &str, at: i64) {
        self.0.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES(?1,'default','custom','.','ended',?2,?3)",params![id,at-100_000,at]).unwrap();
        self.0.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES(?1,'n','{}',?1,'r','idle',?2)",params![id,at]).unwrap();
        self.0.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES(?1,?1,'finished',?2,?3)",params![id,at-100_000,at]).unwrap();
        self.0.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES(?1,?1,0,'n',?1,'{}','{}','d','finished',?2,?3)",params![id,at-100_000,at]).unwrap();
    }
    pub fn command(&self, id: &str, state: &str, sent: i64, payload: serde_json::Value, at: i64) {
        let receipt = (!payload.is_null())
            .then(|| serde_json::json!({"success":false,"payload":payload}).to_string());
        self.0.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_attempt,state,receipt,maybe_sent,created,updated) VALUES(?1,'n','start_turn','{}','d',?1,?2,?3,?4,?5,?6)",params![id,state,receipt,sent,at-100_000,at-20_000]).unwrap();
    }

    pub fn stop(
        &self,
        node: &str,
        session: &str,
        applied: i64,
        source_at: Option<i64>,
        received_at: i64,
    ) {
        self.0.execute("INSERT INTO node_streams(node_id,stream,epoch,durable,applied) VALUES(?1,'s','e',1,?2)",params![node,applied]).unwrap();
        let mut event = serde_json::json!({"subject":{"of":"execution","type":"turn","session":session,"turn":"a","attempt":"a"},"body":{"type":"attempt_ended"}});
        if let Some(at) = source_at {
            event["at"] = at.into();
        }
        self.0
            .execute(
                "INSERT INTO node_event_inbox VALUES(?1,'s','e',1,'stop',?2,?3,?4)",
                params![node, event.to_string(), applied, received_at],
            )
            .unwrap();
    }
}

pub fn seed_baseline(s: &Store, started: i64, baseline: i64) -> Result<()> {
    s.write_locked(|c|{c.execute("INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,node_id,started,baseline) VALUES('a','a','default','n',?1,?2)",params![started,baseline])?;Ok(())})
}

pub fn merge_terminal_receipt(s: &Store, payload: &serde_json::Value, at: i64) -> Result<()> {
    use crate::runtime::{self, ReceiptState};
    s.write(|tx| {
        assert!(runtime::merge_receipt_in(
            tx,
            "a",
            ReceiptState::Finished,
            true,
            payload,
            at - 20_000
        )?);
        assert!(!runtime::merge_receipt_in(
            tx,
            "a",
            ReceiptState::Finished,
            true,
            payload,
            at
        )?);
        Ok::<_, crate::StoreError>(())
    })
}

pub fn input_total(s: &Store) -> Result<i64> {
    s.sqlite_read::<_, crate::StoreError>(|c| {
        Ok(c.query_row(
            "SELECT COALESCE(SUM(input),0) FROM usage_records",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn gateway_identity_count(s: &Store) -> Result<(i64, i64)> {
    s.sqlite_read(|c| {
        Ok((
            c.query_row("SELECT COUNT(*) FROM gateway_usage_reports", [], |r| {
                r.get(0)
            })?,
            c.query_row("SELECT COUNT(*) FROM gateway_usage_meters", [], |r| {
                r.get(0)
            })?,
        ))
    })
}

pub fn gateway_cancellation(s: &Store) -> Result<(bool, i64, i64)> {
    s.sqlite_read(|c| Ok(c.query_row("SELECT ok,(SELECT COUNT(*) FROM usage_records),(SELECT source_at FROM gateway_usage_reports WHERE report_id='live') FROM gateway_calls", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?))
}

pub type GatewayUsage = (String, i64, Option<i64>);
pub fn gateway_usage(s: &Store) -> Result<Vec<GatewayUsage>> {
    s.sqlite_read(|c| {
        Ok(
            c.prepare("SELECT detail,attempts,duration_ms FROM usage_records ORDER BY id")?
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        )
    })
}

pub fn gateway_previous(s: &Store) -> Result<String> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT previous_attempt FROM gateway_attempts WHERE attempt_id='retry'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn gateway_call_count(s: &Store) -> Result<(i64, i64)> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(DISTINCT json_extract(detail,'$.call_id')), MIN(ts) FROM usage_records",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn gateway_source_day(s: &Store) -> Result<String> {
    s.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT day FROM gateway_calls WHERE call_id='not-dispatched'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn gateway_daily(s: &Store) -> Result<(i64, i64)> {
    s.sqlite_read(|c| {
        Ok((
            c.query_row(
                "SELECT COUNT(*) FROM usage_records WHERE day='2026-10-31'",
                [],
                |r| r.get(0),
            )?,
            c.query_row("SELECT SUM(input) FROM stat_daily", [], |r| r.get(0))?,
        ))
    })
}

pub fn assign_project(s: &Store, item: &str, project: &str) -> Result<()> {
    s.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET project_id=? WHERE id=?",
            params![project, item],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn reassign_bot(s: &Store, item: &str) -> Result<()> {
    s.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET project_id=NULL,bot_id='changed' WHERE id=?",
            [item],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}

pub fn install_fail_projection(s: &Store) -> Result<()> {
    s.write_locked(|c| Ok(c.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON usage_records BEGIN SELECT RAISE(ABORT,'projection fault'); END")?))
}

pub fn remove_fail_projection(s: &Store) -> Result<()> {
    s.write_locked(|c| Ok(c.execute_batch("DROP TRIGGER fail_projection")?))
}

pub fn install_fail_first(s: &Store) -> Result<()> {
    s.write_locked(|c| Ok(c.execute_batch("CREATE TRIGGER fail_first BEFORE INSERT ON gateway_usage_reports WHEN NEW.report_id='c' BEGIN SELECT RAISE(ABORT,'first report fault'); END")?))
}

pub fn remove_fail_first(s: &Store) -> Result<()> {
    s.write_locked(|c| Ok(c.execute_batch("DROP TRIGGER fail_first")?))
}

pub fn gateway_unapplied(s: &Store) -> Result<(i64, i64, bool)> {
    s.sqlite_read(|c| {
        Ok((
            c.query_row("SELECT COUNT(*) FROM gateway_usage_reports", [], |r| {
                r.get(0)
            })?,
            c.query_row("SELECT applied FROM gateway_local_inbox", [], |r| r.get(0))?,
            c.query_row(
                "SELECT ended IS NOT NULL AND ok=0 AND projection_dirty=1 FROM gateway_calls",
                [],
                |r| r.get(0),
            )?,
        ))
    })
}

pub fn gateway_recovered(s: &Store) -> Result<(i64, i64, String, i64)> {
    s.sqlite_read(|c| Ok(c.query_row(
        "SELECT ok,(SELECT applied FROM gateway_local_inbox),json_extract(detail,'$.call_id'),(SELECT observed_at FROM gateway_local_inbox) FROM usage_records",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))
    )?))
}
