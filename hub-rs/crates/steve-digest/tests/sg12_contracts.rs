//! SG12 characterization: rule output, paging boundaries and alert state before refactoring.
#[path = "../../steve-channels/tests/support/digest_env.rs"]
mod digest_env;

use chrono::{NaiveDate, TimeZone};
use digest_env::TestEnv;
use rusqlite::params;
use serde_json::json;
use steve_digest::{ConvRef, alerts, brief, conv, todo::Zone};
use steve_modules::testkit::{ME, OTHER, WS};
use steve_store::StoreError;

fn local(day: u32, hour: u32, minute: u32) -> i64 {
    chrono_tz::Asia::Taipei
        .from_local_datetime(
            &NaiveDate::from_ymd_opt(2027, 2, day)
                .unwrap()
                .and_hms_opt(hour, minute, 0)
                .unwrap(),
        )
        .single()
        .unwrap()
        .timestamp_millis()
}

fn sql(e: &TestEnv, statement: &str, values: impl rusqlite::Params) {
    e.store()
        .with_conn(|c| {
            c.execute(statement, values)?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn sg12_brief_deadline_and_snooze_precedence_table() {
    let e = TestEnv::new(|_| {});
    let now = local(3, 8, 30);
    // (id, date, instant, starred, snooze): dates take precedence over instants.
    for (id, date, due, star, snooze) in [
        (
            "date-past",
            Some("2027-02-02"),
            Some(local(4, 18, 0)),
            true,
            None,
        ),
        ("date-today", Some("2027-02-03"), Some(now - 1), false, None),
        ("instant-past", None, Some(local(2, 18, 0)), false, None),
        ("instant-today-past", None, Some(now - 60_000), false, None),
        ("instant-now", None, Some(now), false, None),
        ("instant-end", None, Some(local(4, 0, 0)), true, None),
        ("future-date", Some("2027-02-04"), Some(now - 1), true, None),
        (
            "bad-date",
            Some("not-a-date"),
            Some(now + 60_000),
            false,
            None,
        ),
        ("snoozed", Some("2027-02-02"), None, true, Some(now + 1)),
        ("awake", Some("2027-02-03"), None, false, Some(now)),
        ("undated", None, None, false, None),
    ] {
        sql(&e, "INSERT INTO todos(id,workspace_id,owner,text,source,writer,created,updated,due_date,due,star,snooze_until)
            VALUES(?,?,?,?,'steve',?,1,1,?,?,?,?)", params![id,WS,ME,format!("  {id}  "),ME,date,due,star,snooze]);
    }
    for (id, owner, state, ws) in [
        ("other", OTHER, "open", WS),
        ("closed", ME, "done", WS),
        ("foreign", ME, "open", "w2"),
    ] {
        sql(&e, "INSERT INTO todos(id,workspace_id,owner,text,source,writer,created,updated,due_date,state)
            VALUES(?,?,?,'excluded','steve',?,1,1,'2027-02-02',?)", params![id,ws,owner,ME,state]);
    }
    let agenda = e
        .store()
        .reader(|c| brief::day_agenda(c, WS, ME, Zone::new(chrono_tz::Asia::Taipei), now))
        .unwrap();
    fn lines(xs: &[brief::TodoLine]) -> Vec<(&str, &str, Option<&str>)> {
        xs.iter()
            .map(|x| (x.id.as_str(), x.text.as_str(), x.when.as_deref()))
            .collect::<Vec<_>>()
    }
    assert_eq!(
        lines(&agenda.overdue),
        vec![
            ("date-past", "date-past", Some("02-02")),
            ("instant-past", "instant-past", Some("02-02 18:00")),
            ("instant-today-past", "instant-today-past", Some("08:29")),
        ]
    );
    assert_eq!(
        lines(&agenda.today),
        vec![
            ("instant-now", "instant-now", Some("08:30")),
            ("awake", "awake", None),
            ("date-today", "date-today", None),
            ("bad-date", "bad-date", Some("08:31")),
        ]
    );
    assert_eq!(
        lines(&agenda.starred),
        vec![
            ("instant-end", "instant-end", None),
            ("future-date", "future-date", None)
        ]
    );
    assert_eq!(
        (agenda.day.as_str(), agenda.at, agenda.since),
        ("2027-02-03", now, now - brief::SINCE_DEFAULT_MS)
    );
}

#[test]
fn sg12_brief_goal_week_caps_and_item_since_boundaries() {
    let e = TestEnv::new(|_| {});
    let now = local(3, 8, 30);
    let since = now - 100;
    for (id, day, created) in [
        ("prior", "2027-02-02", since),
        ("same-day", "2027-02-03", now - 1),
    ] {
        sql(
            &e,
            "INSERT INTO briefings(id,workspace_id,user_id,day,event_id,material,pushed,created) VALUES(?,?,?,?,?,'{}',0,?)",
            params![id, WS, ME, day, id, created],
        );
    }
    for n in 0..52 {
        sql(
            &e,
            "INSERT INTO todos(id,workspace_id,owner,text,source,writer,created,updated,star,pri)
            VALUES(?,?,?,?,'steve',?,1,1,1,?)",
            params![format!("star-{n:02}"), WS, ME, "star", ME, n],
        );
    }
    for (id, date) in [
        ("goal-old", "2027-01-01"),
        ("goal-sunday", "2027-02-07"),
        ("goal-next-week", "2027-02-08"),
        ("goal-invalid", "bad"),
    ] {
        sql(&e, "INSERT INTO todos(id,workspace_id,owner,text,source,writer,created,updated,kind,target_date)
            VALUES(?,?,?,?,'steve',?,1,1,'goal',?)", params![id,WS,ME,id,ME,date]);
    }
    for (id, state) in [
        ("done-child", "done"),
        ("drop-child", "dropped"),
        ("open-child", "open"),
    ] {
        sql(&e, "INSERT INTO todos(id,workspace_id,owner,text,source,writer,created,updated,parent_id,state)
            VALUES(?,?,?,'child','steve',?,1,1,'goal-sunday',?)", params![id,WS,ME,ME,state]);
    }
    for (n, status, archived, closed, updated, owner) in [
        (1, "run", false, None, now, ME),
        (2, "run", true, None, now, ME),
        (3, "fail", false, None, now - 1, ME),
        (4, "pause", false, None, now, ME),
        (5, "fail", true, None, now, ME),
        (6, "fail", false, None, now, OTHER),
        (7, "done", false, Some(since), now, ME),
        (8, "done", false, Some(since - 1), now, ME),
        (9, "done", true, None, since, ME),
    ] {
        sql(&e, "INSERT INTO items(id,workspace_id,no,title,status,stage,archived,closed_at,created,updated,requested_by)
            VALUES(?,?,?,' title ',?,'',?,?,1,?,?)", params![format!("SG12-{n}"),WS,n,status,archived,closed,updated,owner]);
    }
    let a = e
        .store()
        .reader(|c| brief::day_agenda(c, WS, ME, Zone::new(chrono_tz::Asia::Taipei), now))
        .unwrap();
    assert_eq!(
        a.since, since,
        "a same-day brief does not shorten the lookback"
    );
    assert_eq!(a.starred.len(), brief::LIST_MAX);
    assert_eq!(
        (
            a.starred.first().unwrap().id.as_str(),
            a.starred.last().unwrap().id.as_str()
        ),
        ("star-51", "star-02")
    );
    assert_eq!(
        a.goals
            .iter()
            .map(|g| (g.id.as_str(), g.done, g.counted))
            .collect::<Vec<_>>(),
        vec![("goal-old", 0, 0), ("goal-sunday", 1, 2)]
    );
    assert_eq!((a.running, a.finished), (1, 2));
    assert_eq!(
        a.stuck
            .iter()
            .map(|i| (i.id.as_str(), i.title.as_str(), i.status.as_str()))
            .collect::<Vec<_>>(),
        vec![("SG12-4", "title", "pause"), ("SG12-3", "title", "fail")]
    );
    assert!(
        a.is_quiet(),
        "stars/goals/stuck alone do not turn a quiet day into a push"
    );
}

fn conversation(e: &TestEnv) {
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated)
            VALUES('sg12-room','default','test','group','sg12','Room',1,1);
            INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated)
            VALUES('sg12-owner','default','person','Owner','usr_dev','usr_dev',1,1),
                  ('sg12-peer','default','person','Peer',NULL,'usr_dev',1,1);
            INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,display_name,source,created,updated)
            VALUES('sg12-owner-account','default','test','other','sg12-owner','personal','owner','Owner','','fixture',1,1),
                  ('sg12-bot','default','test','other','sg12-peer','bot','bot','Handle','Bot display','fixture',1,1);")?;
        Ok(())
    }).unwrap();
}

#[test]
fn sg12_conversation_paging_and_identity_mapping_table() {
    let e = TestEnv::new(|_| {});
    conversation(&e);
    let mut ids = Vec::new();
    for (ext, at, account, party, reply, thread) in [
        ("a", 10, Some("sg12-owner-account"), None, None, Some("a")),
        ("b", 10, Some("sg12-bot"), None, Some("a"), Some("topic")),
        ("c", 20, None, Some("sg12-peer"), None, None),
    ] {
        ids.push(e.store().transaction(|tx| {
            steve_channels::messages::insert_inbound(tx, &steve_channels::messages::NewInbound {
                ws:WS,space_id:"sg12-room",external_id:ext,thread_key:thread,reply_to:reply,
                sender_account_id:account,sender_party_id:party,text:ext,
                attachments:&json!([{"type":"file","key":"attachment"}]),
                mentions:&json!([{"account_id":"sg12-owner-account","name":" supplied "},{"name":"fallback"}]),
                created:at,content_key:None,fallback_from:None,history:None,seen_by:0,
            }).map(|id|id.unwrap()).map_err(|e|StoreError::Hook(e.to_string()))
        }).unwrap());
    }
    let cv = ConvRef::new("space", "sg12-room");
    e.store()
        .reader(|c| {
            let mut expected = ids[..2].to_vec();
            expected.sort();
            expected.push(ids[2].clone());
            let all = conv::read_after(c, WS, &cv, 0, "", 10)?;
            assert_eq!(
                all.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                expected
            );
            for (after, id, limit, wanted) in [
                (10, expected[0].as_str(), 10, expected[1..].to_vec()),
                (10, expected[1].as_str(), 1, vec![ids[2].clone()]),
                (20, ids[2].as_str(), 10, vec![]),
            ] {
                assert_eq!(
                    conv::read_after(c, WS, &cv, after, id, limit)?
                        .iter()
                        .map(|m| m.id.clone())
                        .collect::<Vec<_>>(),
                    wanted
                );
            }
            assert_eq!(
                conv::read_before(c, WS, &cv, 10, &expected[1], 1)?[0].id,
                expected[1]
            );
            assert!(conv::read_before(c, WS, &cv, 20, &ids[2], 0)?.is_empty());
            assert!(conv::read_after(c, "w2", &cv, 0, "", 10)?.is_empty());
            let own = all
                .iter()
                .find(|m| m.ext_id.as_deref() == Some("a"))
                .unwrap();
            assert_eq!(
                own.sender,
                conv::Person {
                    party: Some("sg12-owner".into()),
                    account: Some("sg12-owner-account".into()),
                    name: "Owner".into(),
                    mine: true,
                    bot: false
                }
            );
            assert_eq!(own.thread, None);
            let bot = all
                .iter()
                .find(|m| m.ext_id.as_deref() == Some("b"))
                .unwrap();
            assert!(bot.sender.bot && !bot.sender.mine);
            assert_eq!(
                (
                    bot.sender.name.as_str(),
                    bot.reply_to.as_deref(),
                    bot.thread.as_deref()
                ),
                ("Bot display", Some(ids[0].as_str()), Some("topic"))
            );
            assert_eq!(bot.mentions[0].name, "supplied");
            assert!(bot.mentions[0].mine);
            assert_eq!(bot.mentions[1].name, "fallback");
            assert_eq!(
                bot.attachments,
                json!([{"type":"file","key":"attachment"}])
                    .as_array()
                    .unwrap()
                    .clone()
            );
            let unread = conv::unread(c, WS, &cv, 10)?;
            assert_eq!(
                unread.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                expected,
                "inbox positions preserve source time and message-id tie order"
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

fn alert(
    id: &str,
    at: i64,
    text: &str,
    bot: bool,
    party: Option<&str>,
    reply: Option<&str>,
    thread: Option<&str>,
) -> conv::Msg {
    conv::Msg {
        id: id.into(),
        ext_id: Some(id.into()),
        at,
        text: text.into(),
        sender: conv::Person {
            bot,
            party: party.map(str::to_string),
            ..Default::default()
        },
        mentions: vec![],
        reply_to: reply.map(str::to_string),
        thread: thread.map(str::to_string),
        attachments: vec![],
        subject: None,
    }
}

#[test]
fn sg12_alert_anomalies_handlers_and_cumulative_rollup_table() {
    let e = TestEnv::new(|_| {});
    let cv = ConvRef::new("space", "sg12-alerts");
    let messages = vec![
        alert(
            "a",
            10,
            "[FIRING] disk 95%",
            true,
            None,
            None,
            Some("thread-a"),
        ),
        alert(
            "b",
            11,
            "[RESOLVED] disk 20%",
            true,
            None,
            None,
            Some("thread-a"),
        ),
        alert(
            "c",
            12,
            "[FIRING] cpu 90%",
            true,
            None,
            None,
            Some("thread-c"),
        ),
        alert(
            "d",
            13,
            "checking",
            false,
            Some("handler"),
            Some("a"),
            Some("thread-c"),
        ),
        alert(
            "e",
            14,
            "checking again",
            false,
            Some("handler"),
            None,
            Some("thread-a"),
        ),
        alert("f", 15, "discussion", false, None, Some("missing"), None),
        alert(
            "g",
            16,
            "[FIRING] cpu 99%",
            true,
            None,
            None,
            Some("thread-c"),
        ),
    ];
    let first = e
        .store()
        .transaction(|tx| alerts::aggregate(tx, WS, &cv, &messages))
        .unwrap();
    assert_eq!(first.alerts, 4);
    assert_eq!(first.anomalies, vec![0, 3, 4, 5, 6]);
    assert_eq!(
        first.rollup,
        json!([
            {"category":"cpu #%","fired":2,"recovered":0,"open":true,"handlers":[],"handler_names":[],"first_at":12,"last_at":16,"last_id":"g"},
            {"category":"disk #%","fired":1,"recovered":1,"open":false,"handlers":["handler"],"handler_names":[],"first_at":10,"last_at":11,"last_id":"b"}
        ])
    );
    // Malformed old handler JSON falls back to empty; an older first_at stays authoritative.
    sql(
        &e,
        "UPDATE alert_rollups SET handlers='broken',first_at=1 WHERE workspace_id=? AND conv=? AND category='disk #%'",
        params![WS, cv.key()],
    );
    let next = [
        alert("h", 30, "[FIRING] disk 96%", true, None, None, None),
        alert("i", 31, "handled", false, Some("second"), Some("h"), None),
    ];
    let second = e
        .store()
        .transaction(|tx| alerts::aggregate(tx, WS, &cv, &next))
        .unwrap();
    assert_eq!((second.alerts, second.anomalies), (1, vec![0, 1]));
    assert_eq!(
        second.rollup[0],
        json!({"category":"disk #%","fired":2,"recovered":1,"open":true,"handlers":["second"],"handler_names":[],"first_at":1,"last_at":30,"last_id":"h"})
    );
    let empty = e
        .store()
        .transaction(|tx| alerts::aggregate(tx, WS, &cv, &[]))
        .unwrap();
    assert_eq!((empty.alerts, empty.anomalies), (0, vec![]));
    assert_eq!(empty.rollup, second.rollup);
}

#[test]
fn sg12_brief_render_quiet_short_circuit_table() {
    for (day, running, unread, star, expected) in [
        (
            "2027-02-03",
            0,
            0,
            false,
            "晨报 · 02-03 周三\n今天没有安排。",
        ),
        (
            "2027-02-03",
            3,
            4,
            false,
            "晨报 · 02-03 周三\n今天没有安排。",
        ),
        ("bad-date", 0, 0, false, "晨报 ·  \n今天没有安排。"),
        (
            "2027-02-03",
            0,
            2,
            true,
            "晨报 · 02-03 周三\n今天没有安排。\n\n星标\n- ★ 收藏\n\n信息流里 2 条通知未读",
        ),
    ] {
        let mut agenda = brief::Agenda {
            day: day.into(),
            running,
            unread,
            ..Default::default()
        };
        if star {
            agenda.starred.push(brief::TodoLine {
                id: "star".into(),
                text: "收藏".into(),
                when: None,
                star: true,
                pri: 0,
            });
        }
        assert_eq!(steve_i18n::stored(|| brief::render(&agenda)), expected);
    }
}

#[test]
fn sg12_brief_render_section_caps_targets_statuses_and_tail() {
    let agenda = brief::Agenda {
        day: "2027-02-03".into(),
        today: vec![brief::TodoLine {
            id: "today".into(),
            text: "今天".into(),
            when: None,
            star: false,
            pri: 0,
        }],
        starred: (0..6)
            .map(|n| brief::TodoLine {
                id: format!("star-{n}"),
                text: format!("收藏{n}"),
                when: Some("18:00".into()),
                star: true,
                pri: 0,
            })
            .collect(),
        goals: (0..6)
            .map(|n| brief::GoalLine {
                id: format!("goal-{n}"),
                text: format!("目标{n}"),
                target: if n == 0 {
                    "bad-target".into()
                } else {
                    "2027-02-07".into()
                },
                done: 1,
                counted: if n == 0 { 0 } else { 2 },
            })
            .collect(),
        stuck: (0..6)
            .map(|n| brief::ItemLine {
                id: format!("ST-{n}"),
                title: format!("事项{n}"),
                status: ["fail", "pause", "other"][n % 3].into(),
            })
            .collect(),
        pending: 1,
        running: 2,
        finished: 3,
        unread: 2,
        ..Default::default()
    };
    assert_eq!(
        steve_i18n::stored(|| brief::render(&agenda)),
        concat!(
            "晨报 · 02-03 周三\n今天到期 1 个待办，1 张待处理等你拍板，6 个事项卡住了。",
            "\n\n今天到期\n- 今天",
            "\n\n星标\n- ★ 收藏0 · 18:00\n- ★ 收藏1 · 18:00\n- ★ 收藏2 · 18:00\n- ★ 收藏3 · 18:00\n- ★ 收藏4 · 18:00\n还有 1 项",
            "\n\n本周目标\n- 目标0 · bad-target · 还没拆\n- 目标1 · 02-07 · 1/2\n- 目标2 · 02-07 · 1/2\n- 目标3 · 02-07 · 1/2\n- 目标4 · 02-07 · 1/2\n还有 1 项",
            "\n\n事项\n进行中 2 个，上次晨报以来完成 3 个\n- ST-0 事项0 · 失败\n- ST-1 事项1 · 暂停\n- ST-2 事项2 · other\n- ST-3 事项3 · 失败\n- ST-4 事项4 · 暂停\n还有 1 项",
            "\n\n待处理 1 张，信息流里 2 条通知未读"
        )
    );
}
