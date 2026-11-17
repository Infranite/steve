//! The summary's "工作流" section on one work graph and one plain item, with the expected figures
//! worked out by hand below. All times are minutes after 2026-11-01 00:00 UTC; `stats.tz` is UTC.
//!
//! ST-1 is the graph's item (its coordinator session `sc`); its units are `work` (ST-2), `review`
//! (ST-3) and `integrate` (ST-4). The review failed the first work: work was reset and both ran a
//! second time. ST-5 is an ordinary item.

use steve_stats::metrics::{Query, summary};
use steve_stats::workflow::LONG_TURN_MS;
use steve_store::{NoPublisher, Store, WS};

const T0: i64 = 1_793_491_200_000;
const MIN: i64 = 60_000;

fn m(x: i64) -> i64 {
    T0 + x * MIN
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    let units = r#"[{"id":"work","kind":"work"},{"id":"review","kind":"review"},{"id":"integrate","kind":"integrate"}]"#;
    let mut sql = String::from(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"UTC\"', 0);\n",
    );
    for (no, source) in [
        (1, r#"["对话","chat"]"#),
        (2, r#"["协作工作","workflow"]"#),
        (3, r#"["协作工作","workflow"]"#),
        (4, r#"["协作工作","workflow"]"#),
        (5, r#"["对话","chat"]"#),
    ] {
        sql.push_str(&format!(
            "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, source, created, updated)
             VALUES ('ST-{no}', 'default', {no}, 't', 'done', '执行', 'dev', '{source}', {t}, {t});\n",
            t = m(0)
        ));
    }
    sql.push_str(&format!(
        "INSERT INTO workflows (item_id, revision, max_parallel, max_attempts, created, updated) VALUES ('ST-1', 12, 2, 3, {t0}, {t0});
         INSERT INTO workflow_units (workflow_id, id, position, spec, status, item_id, attempt) VALUES
           ('ST-1', 'work', 0, '{{}}', 'succeeded', 'ST-2', 2),
           ('ST-1', 'review', 1, '{{}}', 'succeeded', 'ST-3', 2),
           ('ST-1', 'integrate', 2, '{{}}', 'succeeded', 'ST-4', 1);
         INSERT INTO workflow_attempts (item_id, attempt, workflow_id, unit_id, snapshot, created) VALUES
           ('ST-2', 1, 'ST-1', 'work', '{{}}', {r}), ('ST-3', 1, 'ST-1', 'review', '{{}}', {r});
         INSERT INTO workflow_history (workflow_id, revision, action, reason, unit_id, detail, created) VALUES
           ('ST-1', 1, 'plan', 'p', NULL, '{{\"units\":{units}}}', {t0}),
           ('ST-1', 2, 'claim', 'c', 'work', '{{\"attempt\":1}}', {}),
           ('ST-1', 3, 'deliver', 'd', 'work', '{{\"status\":\"succeeded\"}}', {}),
           ('ST-1', 4, 'claim', 'c', 'review', '{{\"attempt\":1}}', {}),
           ('ST-1', 5, 'deliver', 'd', 'review', '{{\"status\":\"failed\"}}', {}),
           ('ST-1', 6, 'reset', 'r', 'work', '{{}}', {r}),
           ('ST-1', 7, 'claim', 'c', 'work', '{{\"attempt\":2}}', {}),
           ('ST-1', 8, 'deliver', 'd', 'work', '{{\"status\":\"succeeded\"}}', {}),
           ('ST-1', 9, 'claim', 'c', 'review', '{{\"attempt\":2}}', {}),
           ('ST-1', 10, 'deliver', 'd', 'review', '{{\"status\":\"succeeded\"}}', {}),
           ('ST-1', 11, 'claim', 'c', 'integrate', '{{\"attempt\":1}}', {}),
           ('ST-1', 12, 'deliver', 'd', 'integrate', '{{\"status\":\"succeeded\"}}', {});\n",
        m(1), m(10), m(11), m(20), m(22), m(30), m(31), m(40), m(41), m(50),
        t0 = m(0),
        r = m(21),
    ));
    // sessions: (id, item, created, first_tool_at); sw1's first tool call is only an event
    for (id, item, created, first) in [
        ("sc", "ST-1", m(0), Some(m(2))),
        ("sw1", "ST-2", m(1), None),
        ("sr1", "ST-3", m(11), None),
        ("sw2", "ST-2", m(22), Some(m(23))),
        ("s5", "ST-5", m(60), Some(m(61))),
    ] {
        sql.push_str(&format!(
            "INSERT INTO sessions (id, workspace_id, item_id, bot_id, harness, cwd, status, created, updated, first_tool_at)
             VALUES ('{id}', 'default', '{item}', 'dev', 'claude-code', '/', 'ended', {created}, {created}, {});\n",
            first.map_or("NULL".into(), |f| f.to_string())
        ));
    }
    sql.push_str(&format!(
        "INSERT INTO events (session_id, ts, kind, data) VALUES ('sw1', {}, 'tool_call', '{{}}'), ('sw1', {}, 'tool_call', '{{}}');
         INSERT INTO item_stats (item_id, workspace_id, created, accepted, done_at, updated) VALUES
           ('ST-1', 'default', {t0}, {}, {}, {t0}), ('ST-5', 'default', {t0}, NULL, {}, {t0});
         INSERT INTO usage_records (workspace_id, ts, day, source, purpose, harness, item_id, input) VALUES
           ('default', {}, '2026-11-01', 'agent', 'turn', 'claude-code', 'ST-1', 400),
           ('default', {}, '2026-11-01', 'agent', 'turn', 'claude-code', 'ST-2', 600);
         INSERT INTO agent_turns (id, workspace_id, session_id, item_id, bot_id, harness, started, ended) VALUES
           ('tn_1', 'default', 'sc', 'ST-1', 'dev', 'claude-code', {}, {}),
           ('tn_2', 'default', 'sc', 'ST-1', 'dev', 'claude-code', {}, {});
         INSERT INTO item_marks (workspace_id, item_id, ts, kind, to_v, actor, detail) VALUES
           ('default', 'ST-1', {}, 'wake', 'succeeded', 'steve', '{{}}'),
           ('default', 'ST-1', {}, 'wake', 'failed', 'steve', '{{}}'),
           ('default', 'ST-1', {}, 'wake', 'succeeded', 'steve', '{{}}'),
           ('default', 'ST-1', {}, 'invalidate', 'plan', 'steve', '{{}}'),
           ('default', 'ST-1', {}, 'merge', 'merged', 'steve', '{{}}'),
           ('default', 'ST-2', {}, 'reject', 'unit_evidence', 'dev', '{{}}'),
           ('default', 'ST-5', {}, 'deliver', 'auto', 'dev', '{{}}');
         INSERT INTO chan_messages (id, workspace_id, space_id, external_id, reply_to, direction, status, created, sent) VALUES
           ('mi', 'default', 'sp', 'om_1', NULL, 'in', 'received', {}, NULL),
           ('mo1', 'default', 'sp', 'om_2', 'om_1', 'out', 'sent', {}, {}),
           ('mo2', 'default', 'sp', 'om_3', 'om_1', 'out', 'sent', {}, {});
         PRAGMA foreign_keys = ON;",
        m(3), m(4),
        m(60), m(70), m(65),
        m(5), m(25),
        m(0), m(0) + LONG_TURN_MS + 10 * MIN, m(55), m(60),
        m(10), m(20), m(50), m(52), m(68), m(29), m(64),
        m(100), m(101), m(102), m(104), m(105),
        t0 = m(0),
    ));
    s.with_conn(|c| Ok(c.execute_batch(&sql)?)).unwrap();
    (dir, s)
}

fn q(bot: Option<&str>) -> Query {
    Query {
        workspace_id: WS.into(),
        from: "2026-11-01".into(),
        to: "2026-11-01".into(),
        bot: bot.map(Into::into),
        ..Query::default()
    }
}

#[test]
fn the_workflow_section_reads_the_graph_the_marks_and_the_sessions() {
    let (_d, s) = store();
    let sum = s.with_conn(|c| Ok(summary(c, &q(None))?.unwrap())).unwrap();
    let w = &sum.workflow;
    // five attempts of three units; the second work and review are retries
    assert_eq!((w.graphs, w.units, w.attempts, w.retries), (1, 3, 5, 2));
    assert_eq!(w.retry_rate, Some(0.4));
    assert_eq!(
        (w.attempts_per_unit.sum, w.attempts_per_unit.p50),
        (5, Some(2))
    );
    assert_eq!((w.resets, w.unit_deliveries), (1, 5));
    // first attempts: work passed, review did not, integrate passed
    let ft = |k: &str| (w.first_try[k].delivered, w.first_try[k].passed);
    assert_eq!(
        (ft("work"), ft("review"), ft("integrate")),
        ((1, 1), (1, 0), (1, 1))
    );
    assert_eq!(w.first_try["review"].rate, Some(0.0));
    // ST-1: its own session and its units' three; ST-5: one
    assert_eq!(w.sessions_per_item.n, 2);
    assert_eq!(w.sessions_per_item.sum, 5);
    assert_eq!((w.coordinator_tokens, w.unit_tokens), (400, 600));
    assert_eq!(w.coordinator_share, Some(0.4));
    assert_eq!((w.coordinator_turn_ms.n, w.coordinator_long_turns), (2, 1));
    assert_eq!(
        w.wakes
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        [("failed", 1), ("succeeded", 2)]
    );
    assert_eq!(w.invalidations.get("plan"), Some(&1));
    // accepted at 60, merged at 68
    assert_eq!((w.merged, w.accept_to_merge.p50), (1, Some(8 * MIN)));
    // one refusal against one item delivery and five unit deliveries
    assert_eq!((w.rejected, w.delivered), (1, 6));
    assert_eq!(w.rejects.get("unit_evidence"), Some(&1));
    assert_eq!(w.reject_rate, Some(1.0 / 7.0));
    // the first reply counts, two minutes after the message
    assert_eq!((w.reply_latency.n, w.reply_latency.p50), (1, Some(2 * MIN)));
    // sc 2, sw1 2 (its first tool_call event), sw2 1, s5 1; sr1 made none
    assert_eq!(
        (w.first_tool.n, w.first_tool.sum, w.no_tool),
        (4, 6 * MIN, 1)
    );
    // the two kinds of rework side by side: no acceptance rework here, two unit retries
    assert_eq!((sum.quality.reworks, sum.quality.unit_retries), (0, 2));
}

#[test]
fn the_filters_go_through_the_graphs_item() {
    let (_d, s) = store();
    let w = s
        .with_conn(|c| Ok(summary(c, &q(Some("other")))?.unwrap().workflow))
        .unwrap();
    assert_eq!((w.graphs, w.attempts, w.rejected, w.merged), (0, 0, 0, 0));
    // replies follow the workspace only
    assert_eq!(w.reply_latency.n, 1);
}
