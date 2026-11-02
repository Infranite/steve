//! Statistics writes (stats.md slice ST0): turns, token records and marks keep `stat_daily` equal to
//! the detail rows and flag the item for the `item_stats` refresh.

mod common;

use std::collections::BTreeMap;

use common::*;
use rusqlite::Connection;
use serde_json::json;
use steve_store::{
    ModelUsage, NewBot, NewItem, NewMark, NewUsage, Store, Tokens, TurnBegin, TurnFinish, WS,
};

fn with_steve() -> T {
    let t = fresh();
    t.store
        .bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    t
}

fn item(s: &Store) -> String {
    s.items()
        .create(
            WS,
            &NewItem {
                title: "统计".into(),
                goal: "g".into(),
                bot_id: "steve".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id
}

fn tokens(i: i64, o: i64, cr: Option<i64>, cw: Option<i64>) -> Tokens {
    Tokens {
        input: Some(i),
        output: Some(o),
        cache_read: cr,
        cache_write: cw,
        reasoning: None,
    }
}

fn begin(s: &Store, item: Option<&str>, attempt: i64, started: i64) -> String {
    s.stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: "ses_1".into(),
            item_id: item.map(Into::into),
            bot_id: Some("steve".into()),
            project_id: None,
            harness: "claude-code".into(),
            user_seq: Some(1),
            attempt,
            started,
        })
        .unwrap()
}

fn gateway(model: &str, t: Tokens, ok: bool, day: &str) -> NewUsage {
    NewUsage {
        workspace_id: WS.into(),
        ts: 5,
        day: day.into(),
        source: "gateway".into(),
        purpose: "route".into(),
        harness: "claude-cli".into(),
        turn_id: None,
        session_id: None,
        item_id: None,
        bot_id: None,
        project_id: None,
        usage: ModelUsage {
            model: model.into(),
            tokens: t,
            cost_reported: Some(0.25),
            detail: serde_json::Value::Null,
        },
        duration_ms: Some(900),
        ok,
        attempts: 2,
    }
}

/// `stat_daily`'s token and call columns grouped by its key, and the same computed from `usage_records`.
fn cube_and_details(c: &Connection) -> (Vec<String>, Vec<String>) {
    let q = |sql: &str| -> Vec<String> {
        c.prepare(sql)
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let cols = "workspace_id||'|'||day||'|'||source||'|'||purpose||'|'||harness||'|'||model||'|'||bot_id||'|'||project_id||'|'";
    let cube = q(&format!(
        "SELECT {cols}||input||'|'||output||'|'||cache_read||'|'||cache_write||'|'||reasoning||'|'||hit_base||'|'||cost_reported||'|'||calls||'|'||failed_calls FROM stat_daily WHERE calls > 0 ORDER BY 1"
    ));
    let detail = q(&format!(
        "SELECT {cols}||sum(ifnull(input,0))||'|'||sum(ifnull(output,0))||'|'||sum(ifnull(cache_read,0))||'|'||sum(ifnull(cache_write,0))||'|'||sum(ifnull(reasoning,0))||'|'||sum(CASE WHEN input IS NOT NULL AND cache_read IS NOT NULL THEN input+cache_read+ifnull(cache_write,0) ELSE 0 END)||'|'||total(cost_reported)||'|'||count(*)||'|'||sum(1-ok) FROM (SELECT workspace_id, day, source, purpose, harness, model, ifnull(bot_id,'') AS bot_id, ifnull(project_id,'') AS project_id, input, output, cache_read, cache_write, reasoning, cost_reported, ok FROM usage_records) GROUP BY workspace_id, day, source, purpose, harness, model, bot_id, project_id ORDER BY 1"
    ));
    (cube, detail)
}

#[test]
fn turn_writes_rows_per_model_and_books_the_cube() {
    let t = with_steve();
    let s = &t.store;
    let it = item(s);
    s.with_conn(|c| {
        // creating the item already left its first status mark (ST3), which flags the row
        assert_eq!(count(c, "SELECT count(*) FROM item_stats"), 1);
        c.execute("UPDATE item_stats SET dirty = 0", [])?;
        Ok(())
    })
    .unwrap();
    let id = begin(s, Some(&it), 0, 1_000);
    assert!(id.starts_with("tn_"));
    let mut kinds = BTreeMap::new();
    kinds.insert("execute".to_string(), 3);
    kinds.insert("mcp:steve_deliver".to_string(), 1);
    let fin = TurnFinish {
        turn_id: id.clone(),
        ended: 4_000,
        day: "2026-11-01".into(),
        model: "claude-opus-5-5".into(),
        stop_reason: Some("end_turn".into()),
        tools: 4,
        tool_errors: 1,
        tool_kinds: kinds.clone(),
        permissions: 2,
        usage: vec![
            ModelUsage {
                model: "claude-opus-5-5".into(),
                tokens: tokens(10, 200, Some(5_000), Some(300)),
                cost_reported: Some(0.5),
                detail: json!({"scope": "all"}),
            },
            ModelUsage {
                model: "claude-haiku-5-5".into(),
                tokens: tokens(4, 8, None, None),
                cost_reported: None,
                detail: json!({"scope": "all"}),
            },
        ],
        ..TurnFinish::default()
    };
    assert!(s.stats().turn_finish(&fin).unwrap());
    // a second finish counts nothing
    assert!(!s.stats().turn_finish(&fin).unwrap());
    // a second turn on the same day merges the tool counts
    let id2 = begin(s, Some(&it), 1, 5_000);
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: id2,
            ended: 5_500,
            day: "2026-11-01".into(),
            model: "claude-opus-5-5".into(),
            stop_reason: Some("error".into()),
            transient: true,
            tools: 2,
            tool_kinds: [("execute".to_string(), 2)].into(),
            ..TurnFinish::default()
        })
        .unwrap();

    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT count(*) FROM usage_records WHERE turn_id IS NOT NULL"), 2);
        let (stop, perms, tk): (String, i64, String) = c.query_row(
            "SELECT stop_reason, permissions, tool_kinds FROM agent_turns WHERE id = ?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!((stop.as_str(), perms), ("end_turn", 2));
        assert_eq!(tk, r#"{"execute":3,"mcp:steve_deliver":1}"#);
        // the haiku row reported no cache split: NULL, not 0, and outside the hit-rate base
        assert_eq!(
            count(c, "SELECT count(*) FROM usage_records WHERE model = 'claude-haiku-5-5' AND cache_read IS NULL AND item_id IS NOT NULL AND session_id = 'ses_1'"),
            1
        );
        let row: (i64, i64, i64, i64, i64, i64, i64, i64, i64, String) = c.query_row(
            "SELECT turns, turn_ms, turn_errors, retries, tools, tool_errors, hit_base, cache_read, calls, tool_kinds FROM stat_daily WHERE model = 'claude-opus-5-5'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)),
        )?;
        // one first try, one transient retry (not an error); 3000 + 500 ms
        assert_eq!(row.0, 1);
        assert_eq!(row.1, 3_500);
        assert_eq!((row.2, row.3), (0, 1));
        assert_eq!((row.4, row.5), (6, 1));
        assert_eq!(row.6, 10 + 5_000 + 300);
        assert_eq!(row.7, 5_000);
        assert_eq!(row.8, 1);
        assert_eq!(row.9, r#"{"execute":5,"mcp:steve_deliver":1}"#);
        assert_eq!(
            c.query_row("SELECT hit_base FROM stat_daily WHERE model = 'claude-haiku-5-5'", [], |r| r.get::<_, i64>(0))?,
            0
        );
        let (cube, detail) = cube_and_details(c);
        assert_eq!(cube, detail);
        // the item is flagged for the refresh
        assert_eq!(count(c, &format!("SELECT dirty FROM item_stats WHERE item_id = '{it}'")), 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn usage_add_keeps_the_cube_equal_to_the_details() {
    let t = with_steve();
    let s = &t.store;
    let st = s.stats();
    st.usage_add(&gateway(
        "claude-haiku-5-5",
        tokens(2, 4, Some(11_097), Some(5_727)),
        true,
        "2026-11-01",
    ))
    .unwrap();
    st.usage_add(&gateway(
        "claude-haiku-5-5",
        tokens(3, 5, Some(1), Some(0)),
        true,
        "2026-11-01",
    ))
    .unwrap();
    st.usage_add(&gateway(
        "claude-haiku-5-5",
        Tokens::default(),
        false,
        "2026-11-01",
    ))
    .unwrap();
    st.usage_add(&gateway(
        "claude-sonnet-5-5",
        tokens(1, 1, None, None),
        true,
        "2026-11-02",
    ))
    .unwrap();
    let mut other = gateway("", Tokens::default(), true, "2026-11-02");
    other.source = "agent".into();
    other.purpose = "autonomous".into();
    other.bot_id = Some("steve".into());
    st.usage_add(&other).unwrap();
    s.with_conn(|c| {
        let (cube, detail) = cube_and_details(c);
        assert_eq!(cube.len(), 3);
        assert_eq!(cube, detail);
        let (calls, failed, cost): (i64, i64, f64) = c.query_row(
            "SELECT calls, failed_calls, cost_reported FROM stat_daily WHERE day = '2026-11-01'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!((calls, failed), (3, 1));
        assert!((cost - 0.75).abs() < 1e-9);
        assert_eq!(
            count(c, "SELECT count(*) FROM usage_records WHERE detail = '{}'"),
            5
        );
        // no item, nothing flagged
        assert_eq!(count(c, "SELECT count(*) FROM item_stats"), 0);
        Ok(())
    })
    .unwrap();
}

#[test]
fn interrupt_open_closes_only_open_turns() {
    let t = with_steve();
    let s = &t.store;
    let open = begin(s, None, 0, 100);
    let done = begin(s, None, 0, 100);
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: done.clone(),
            ended: 200,
            day: "2026-11-01".into(),
            stop_reason: Some("end_turn".into()),
            ..TurnFinish::default()
        })
        .unwrap();
    // the session's last events before the restart; the Hub stayed down for hours after 400
    s.with_conn(|c| {
        c.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO events (session_id, ts, kind, data) VALUES ('ses_1', 150, 'tool_call', '{}'), ('ses_1', 400, 'agent_message_chunk', '{}');
             PRAGMA foreign_keys = ON;",
        )?;
        Ok(())
    })
    .unwrap();
    let boot = 5 * 3_600_000;
    assert_eq!(
        s.stats()
            .interrupt_open(boot, |_, at| {
                assert_eq!(at, 400, "booked on the day of the last event");
                "2026-11-02".into()
            })
            .unwrap(),
        1
    );
    assert_eq!(
        s.stats()
            .interrupt_open(boot + 100, |_, _| "2026-11-02".into())
            .unwrap(),
        0
    );
    s.with_conn(|c| {
        let (ended, reason): (i64, String) = c.query_row(
            "SELECT ended, stop_reason FROM agent_turns WHERE id = ?",
            [&open],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // the long gap before the restart is not turn time
        assert_eq!((ended, reason.as_str()), (400, "interrupted"));
        let (turns, ms, interrupted): (i64, i64, i64) = c.query_row(
            "SELECT turns, turn_ms, interrupted FROM stat_daily WHERE day = '2026-11-02'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!((turns, ms, interrupted), (1, 300, 1));
        assert_eq!(count(c, "SELECT count(*) FROM usage_records"), 0);
        Ok(())
    })
    .unwrap();
}

#[test]
fn marks_flag_the_item_and_unknown_items_are_ignored() {
    let t = with_steve();
    let s = &t.store;
    let it = item(s);
    s.with_conn(|c| {
        // the row exists since the create mark (ST3); start from clean
        c.execute("UPDATE item_stats SET dirty = 0 WHERE item_id = ?", [&it])?;
        Ok(())
    })
    .unwrap();
    let id = s
        .stats()
        .mark_add(&NewMark {
            workspace_id: WS.into(),
            item_id: it.clone(),
            ts: 10,
            kind: "status".into(),
            from_v: Some("run".into()),
            to_v: Some("done".into()),
            actor: "me".into(),
            ..NewMark::default()
        })
        .unwrap();
    assert!(id > 0);
    s.stats()
        .mark_add(&NewMark {
            workspace_id: WS.into(),
            item_id: "ST-404".into(),
            ts: 10,
            kind: "fail".into(),
            to_v: Some("stall".into()),
            detail: json!({"why": "停了"}),
            ..NewMark::default()
        })
        .unwrap();
    s.with_conn(|c| {
        assert_eq!(
            count(
                c,
                &format!("SELECT dirty FROM item_stats WHERE item_id = '{it}'")
            ),
            1
        );
        assert_eq!(count(c, "SELECT count(*) FROM item_stats"), 1);
        // the create mark plus the two above
        assert_eq!(count(c, "SELECT count(*) FROM item_marks"), 3);
        assert_eq!(
            c.query_row(
                "SELECT detail FROM item_marks WHERE kind = 'fail'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            r#"{"why":"停了"}"#
        );
        Ok(())
    })
    .unwrap();
    s.with_conn(|c| {
        c.execute("UPDATE item_stats SET dirty = 0", [])?;
        Ok(())
    })
    .unwrap();
    s.stats().touch_item(&it).unwrap();
    s.with_conn(|c| {
        assert_eq!(
            count(c, "SELECT count(*) FROM item_stats WHERE dirty = 1"),
            1
        );
        Ok(())
    })
    .unwrap();
}
