//! Todos and goals: fields, per-member access, goal progress, recurrence, undo and the agent's tools.
//! Synthetic data only; no network, harness or model.
use rusqlite::params;
use serde_json::{Value, json};
use steve_digest::{changes, todo, todo_tools};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_store::StoreError;

/// 2027-01-31 10:00 UTC
const NOW: i64 = 1_801_389_600_000;
const DAY: i64 = 86_400_000;

fn env() -> TestEnv {
    let e = TestEnv::new(|_| {});
    e.clock.set(NOW);
    e.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES
                    ('ST-1', 'default', 1, 'task', 'draft the outline', 'run', '执行', 1, 1),
                    ('ST-2', 'default', 2, 'task', 'collect numbers', 'done', '完成', 1, 1),
                    ('ST-3', 'default', 3, 'task', 'old idea', 'drop', '完成', 1, 1);",
            )?;
            Ok(())
        })
        .unwrap();
    e
}

fn add<'a>(
    e: &TestEnv,
    owner: &'a str,
    text: &'a str,
    f: impl FnOnce(&mut todo::NewTodo<'a>),
) -> String {
    e.store()
        .transaction(|tx| {
            let mut t = todo::NewTodo {
                ws: WS,
                owner,
                text,
                source: "you",
                writer: owner,
                ..Default::default()
            };
            f(&mut t);
            todo::create(tx, &t, NOW)
        })
        .unwrap()
}

fn refused<'a>(e: &TestEnv, owner: &'a str, f: impl FnOnce(&mut todo::NewTodo<'a>)) -> String {
    let err = e
        .store()
        .transaction(|tx| {
            let mut t = todo::NewTodo {
                ws: WS,
                owner,
                text: "x",
                source: "you",
                writer: owner,
                ..Default::default()
            };
            f(&mut t);
            todo::create(tx, &t, NOW)
        })
        .unwrap_err();
    match err {
        StoreError::Hook(s) => s,
        e => panic!("{e}"),
    }
}

fn progress(e: &TestEnv, goal: &str) -> Value {
    e.store().reader(|c| todo::progress(c, WS, goal)).unwrap()
}

fn state(e: &TestEnv, id: &str, s: &str, item: Option<&str>) -> Option<String> {
    e.store()
        .transaction(|tx| todo::complete(tx, WS, id, s, item, NOW))
        .unwrap()
}

#[test]
fn fields_are_checked_as_a_whole() {
    let e = env();
    let goal = add(&e, ME, "Ship the Q4 review", |t| {
        t.kind = "goal";
        t.target_date = Some("2027-03-01");
    });
    let others = add(&e, OTHER, "Their goal", |t| t.kind = "goal");
    assert_eq!(refused(&e, ME, |t| t.text = "  "), "todo.text");
    assert_eq!(refused(&e, ME, |t| t.kind = "epic"), "todo.kind");
    assert_eq!(
        refused(&e, ME, |t| t.due_date = Some("2027-2-1")),
        "todo.date"
    );
    assert_eq!(
        refused(&e, ME, |t| {
            t.due = Some(NOW);
            t.due_date = Some("2027-02-01");
        }),
        "todo.due_both"
    );
    assert_eq!(
        refused(&e, ME, |t| {
            t.kind = "goal";
            t.due_date = Some("2027-02-01");
        }),
        "todo.goal_fields"
    );
    assert_eq!(
        refused(&e, ME, |t| t.target_date = Some("2027-02-01")),
        "todo.target_date"
    );
    // a parent is one of your own goals, never someone else's or a plain todo
    assert_eq!(refused(&e, ME, |t| t.parent = Some(&others)), "todo.parent");
    let plain = add(&e, ME, "plain", |_| {});
    assert_eq!(refused(&e, ME, |t| t.parent = Some(&plain)), "todo.parent");
    assert_eq!(refused(&e, ME, |t| t.pri = 4), "todo.pri");
    assert_eq!(
        refused(&e, ME, |t| t.recur = Some("FREQ=WEEKLY")),
        "todo.recur_due"
    );
    assert_eq!(
        refused(&e, ME, |t| {
            t.recur = Some("FREQ=HOURLY");
            t.due = Some(NOW);
        }),
        "todo.recur"
    );
    let child = add(&e, ME, "Write the draft", |t| {
        t.parent = Some(&goal);
        t.due_date = Some("2027-02-05");
        t.recur = Some("freq=weekly;interval=2");
    });
    let (_, f, _) = e
        .store()
        .reader(|c| todo::fields(c, WS, &child))
        .unwrap()
        .unwrap();
    assert_eq!(f.recur.as_deref(), Some("FREQ=WEEKLY;INTERVAL=2"));
    let row = e
        .store()
        .reader(|c| todo::get(c, WS, &child))
        .unwrap()
        .unwrap();
    assert_eq!(row["due_kind"], "date");
    assert_eq!(row["parent"]["id"], json!(goal));
    // a goal with todos under it stays a goal
    let err = e
        .store()
        .transaction(|tx| {
            todo::edit(
                tx,
                WS,
                &goal,
                &todo::Patch {
                    kind: Some("todo".into()),
                    target_date: Some(None),
                    ..Default::default()
                },
                NOW,
            )
        })
        .unwrap_err();
    assert!(matches!(err, StoreError::Hook(s) if s == "todo.has_children"));
    // a goal is not handed to a Bot whole
    let err = e
        .store()
        .transaction(|tx| todo::set_state(tx, WS, &goal, "handed", Some("ST-1"), NOW))
        .unwrap_err();
    assert!(matches!(err, StoreError::Hook(s) if s == "todo.goal_hand"));
    // a message's deadline still needs its words; a hand-set day does not
    assert!(
        refused(&e, ME, |t| {
            t.source = "steve";
            t.due_date = Some("2027-02-01");
        })
        .contains("source words")
    );
}

#[test]
fn goal_progress_counts_children_and_linked_items_once() {
    let e = env();
    let goal = add(&e, ME, "Launch", |t| t.kind = "goal");
    // nothing under it yet: not 0%, not split
    let p = progress(&e, &goal);
    assert_eq!(p["total"], 0);
    assert!(p["pct"].is_null());
    let a = add(&e, ME, "a", |t| t.parent = Some(&goal));
    let b = add(&e, ME, "b", |t| t.parent = Some(&goal));
    let c = add(&e, ME, "c", |t| t.parent = Some(&goal));
    let d = add(&e, ME, "d", |t| t.parent = Some(&goal));
    state(&e, &a, "done", None);
    state(&e, &b, "dropped", None);
    // handed: counts as its item (ST-2 is done); the same item linked too counts once
    state(&e, &c, "handed", Some("ST-2"));
    e.store()
        .transaction(|tx| {
            assert!(todo::link_item(tx, WS, &goal, "ST-2", true)?);
            assert!(todo::link_item(tx, WS, &goal, "ST-1", true)?);
            assert!(todo::link_item(tx, WS, &goal, "ST-3", true)?);
            assert!(todo::link_item(tx, WS, &d, "ST-1", true).is_err());
            Ok::<_, StoreError>(())
        })
        .unwrap();
    let p = progress(&e, &goal);
    // a done, b dropped, c→ST-2 done, d open, ST-1 open, ST-3 dropped
    assert_eq!(
        (
            p["done"].as_i64(),
            p["dropped"].as_i64(),
            p["open"].as_i64(),
            p["total"].as_i64()
        ),
        (Some(2), Some(2), Some(2), Some(6))
    );
    assert_eq!(p["pct"], 50);
    let listed = e
        .store()
        .reader(|c| {
            todo::list_by(
                c,
                WS,
                ME,
                &todo::Filter {
                    kind: Some("goal".into()),
                    limit: 10,
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["progress"]["pct"], 50);
    // everything dropped: nothing counts
    let g2 = add(&e, ME, "Abandoned", |t| t.kind = "goal");
    let x = add(&e, ME, "x", |t| t.parent = Some(&g2));
    state(&e, &x, "dropped", None);
    assert!(progress(&e, &g2)["pct"].is_null());
}

#[test]
fn recurrence_steps_from_its_anchor_to_the_first_occurrence_not_past() {
    let utc = todo::Zone::Tz(chrono_tz::UTC);
    // monthly on the 31st: February takes its last day, March is the 31st again
    let f = todo::Fields {
        text: "pay rent".into(),
        kind: "todo".into(),
        due_date: Some("2027-01-31".into()),
        recur: Some("FREQ=MONTHLY".into()),
        remind_at: Some(NOW - 2 * 3_600_000),
        ..Default::default()
    };
    let n = todo::next_occurrence(utc, &f, NOW).unwrap();
    assert_eq!(n.due_date.as_deref(), Some("2027-02-28"));
    assert_eq!(n.remind_at, Some(NOW - 2 * 3_600_000 + 28 * DAY));
    let mut later = f.clone();
    later.due_date = Some("2026-12-31".into());
    // done late: steps over the past ones, and the anchor keeps the 31st
    let n = todo::next_occurrence(utc, &later, NOW + 40 * DAY).unwrap();
    assert_eq!(n.due_date.as_deref(), Some("2027-03-31"));
    // a moment, weekly, done two weeks late
    let t = todo::Fields {
        text: "standup notes".into(),
        kind: "todo".into(),
        due: Some(NOW - 15 * DAY),
        recur: Some("FREQ=WEEKLY".into()),
        ..Default::default()
    };
    let n = todo::next_occurrence(utc, &t, NOW).unwrap();
    assert_eq!(n.due, Some(NOW + 6 * DAY));
    // not recurring: nothing next
    let mut once = t.clone();
    once.recur = None;
    assert!(todo::next_occurrence(utc, &once, NOW).is_none());
}

#[test]
fn local_times_in_a_daylight_saving_gap_move_past_it() {
    let ny = todo::Zone::Tz(chrono_tz::America::New_York);
    let gap = chrono::NaiveDate::from_ymd_opt(2027, 3, 14)
        .unwrap()
        .and_hms_opt(2, 30, 0)
        .unwrap();
    let ms = ny.instant(gap).unwrap();
    assert_eq!(ny.show(ms).unwrap(), "2027-03-14T03:00:00-04:00");
    // a repeated hour takes its first occurrence
    let twice = chrono::NaiveDate::from_ymd_opt(2027, 11, 7)
        .unwrap()
        .and_hms_opt(1, 30, 0)
        .unwrap();
    assert_eq!(
        ny.show(ny.instant(twice).unwrap()).unwrap(),
        "2027-11-07T01:30:00-04:00"
    );
    // the person's notification time zone, else the Hub's
    let e = env();
    e.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO notification_policies (workspace_id, user_id, enabled, mode, timezone, consented_at, consent_since_seq, updated)
                 VALUES (?, ?, 0, 'log', 'Asia/Shanghai', 1, 0, 1)",
                params![WS, ME],
            )?;
            Ok(())
        })
        .unwrap();
    e.store()
        .reader(|c| {
            assert_eq!(todo::Zone::of(c, WS, ME).name(), "Asia/Shanghai");
            assert_eq!(todo::Zone::of(c, WS, OTHER).name(), "hub-local");
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn completing_a_recurring_todo_makes_the_next_and_undo_takes_it_back() {
    let e = env();
    let id = add(&e, ME, "water the plants", |t| {
        t.due_date = Some("2027-01-30");
        t.recur = Some("FREQ=DAILY;INTERVAL=3");
    });
    let (change, next) = e
        .store()
        .transaction(|tx| {
            let t = todo::get(tx, WS, &id)?.unwrap();
            let next = todo::complete(tx, WS, &id, "done", None, NOW)?;
            let nu = todo::fields(tx, WS, next.as_ref().unwrap())?.unwrap().2;
            let ch = changes::record(
                tx, WS, ME, ME, "todo_state", &id, None,
                &json!({"state": t["state"], "item": t["item_id"]}),
                &json!({"todo": id, "state": "done", "item": null, "next": next, "next_updated": nu}),
                NOW,
            )?;
            Ok::<_, StoreError>((ch, next))
        })
        .unwrap();
    let next = next.unwrap();
    let n = e
        .store()
        .reader(|c| todo::get(c, WS, &next))
        .unwrap()
        .unwrap();
    assert_eq!(n["due_date"], "2027-02-02");
    assert_eq!(n["state"], "open");
    // done again: no second next
    assert!(state(&e, &id, "done", None).is_none());
    changes::undo(&e.modules, WS, ME, &change).unwrap();
    assert!(
        e.store()
            .reader(|c| todo::get(c, WS, &next))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        e.store()
            .reader(|c| todo::current(c, WS, &id))
            .unwrap()
            .unwrap()
            .0,
        "open"
    );
}

#[tokio::test]
async fn every_member_has_their_own_todos_and_nobody_elses() {
    let e = env();
    let app = steve_digest::routes().with_state(e.modules.clone());
    let (me, other) = (caller(WS, ME), caller(WS, OTHER));
    // a member (not the owner) uses todos, but still not the owner-only digest data
    let (s, b) = call(
        &app,
        Some(&other),
        "POST",
        "/digest/todos",
        Some(&json!({"text": "renew passport", "due_date": "2027-02-10", "pri": 2, "star": true})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let theirs = b["id"].as_str().unwrap().to_string();
    let their_change = b["change"].as_str().unwrap().to_string();
    assert_eq!(
        call(&app, Some(&other), "GET", "/digest/packets", None)
            .await
            .0,
        403
    );
    let (s, b) = call(&app, Some(&other), "GET", "/digest/todos", None).await;
    assert_eq!(s, 200);
    assert_eq!(b["todos"][0]["star"], true);
    assert_eq!(b["todos"][0]["due_kind"], "date");
    // the owner sees none of it: not listed, not readable, not changeable, not undoable
    let (_, b) = call(&app, Some(&me), "GET", "/digest/todos", None).await;
    assert_eq!(b["todos"], json!([]));
    for (m, path, body) in [
        ("GET", format!("/digest/todos/{theirs}"), None),
        (
            "PATCH",
            format!("/digest/todos/{theirs}"),
            Some(json!({"text": "mine now"})),
        ),
        (
            "POST",
            format!("/digest/todos/{theirs}"),
            Some(json!({"state": "done"})),
        ),
        (
            "POST",
            format!("/digest/todos/{theirs}/items"),
            Some(json!({"item": "ST-1"})),
        ),
        ("POST", format!("/digest/changes/{their_change}/undo"), None),
    ] {
        assert_eq!(
            call(&app, Some(&me), m, &path, body.as_ref()).await.0,
            404,
            "{m} {path}"
        );
    }
    let (_, b) = call(&app, Some(&me), "GET", "/digest/changes", None).await;
    assert!(
        b["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["target"] != json!(theirs))
    );
    // they change it, then undo the change
    e.clock.set(NOW + 1000);
    let (s, b) = call(
        &app,
        Some(&other),
        "PATCH",
        &format!("/digest/todos/{theirs}"),
        Some(&json!({"text": "renew passport and ID", "due": NOW + DAY})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let edit = b["change"].as_str().unwrap().to_string();
    let (_, t) = call(
        &app,
        Some(&other),
        "GET",
        &format!("/digest/todos/{theirs}"),
        None,
    )
    .await;
    assert_eq!(
        (t["due_kind"].as_str(), t["due_date"].as_str()),
        (Some("time"), None)
    );
    let (s, b) = call(
        &app,
        Some(&other),
        "POST",
        &format!("/digest/changes/{edit}/undo"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let (_, t) = call(
        &app,
        Some(&other),
        "GET",
        &format!("/digest/todos/{theirs}"),
        None,
    )
    .await;
    assert_eq!(t["text"], "renew passport");
    assert_eq!(t["due_date"], "2027-02-10");
    // a refused value says why
    let (s, b) = call(
        &app,
        Some(&other),
        "PATCH",
        &format!("/digest/todos/{theirs}"),
        Some(&json!({"target_date": "2027-03-01"})),
    )
    .await;
    assert_eq!(s, 400);
    assert!(
        b.to_string().contains("goal") || b.to_string().contains("目标"),
        "{b}"
    );
    // a goal page lists its children with progress
    let (_, g) = call(
        &app,
        Some(&other),
        "POST",
        "/digest/todos",
        Some(&json!({"text": "Move house", "kind": "goal", "target_date": "2027-06-30"})),
    )
    .await;
    let goal = g["id"].as_str().unwrap();
    let (s, _) = call(
        &app,
        Some(&other),
        "PATCH",
        &format!("/digest/todos/{theirs}"),
        Some(&json!({"parent": goal})),
    )
    .await;
    assert_eq!(s, 200);
    let (s, b) = call(
        &app,
        Some(&other),
        "POST",
        &format!("/digest/todos/{goal}/items"),
        Some(&json!({"item": "ST-2"})),
    )
    .await;
    assert_eq!((s, &b["changed"]), (200, &json!(true)), "{b}");
    let (_, g) = call(
        &app,
        Some(&other),
        "GET",
        &format!("/digest/todos/{goal}"),
        None,
    )
    .await;
    assert_eq!(g["children"].as_array().unwrap().len(), 1);
    assert_eq!(
        g["items"],
        json!(["ST-2"]),
        "the goal page lists its linked items"
    );
    assert_eq!(g["progress"]["pct"], 50);
    let (_, b) = call(
        &app,
        Some(&other),
        "GET",
        &format!("/digest/todos?parent={goal}"),
        None,
    )
    .await;
    assert_eq!(b["todos"][0]["id"], json!(theirs));
}

fn cx(user: &str) -> McpCx {
    McpCx {
        ws: WS.into(),
        user: user.into(),
        bot_id: "bot_helper".into(),
        item_id: Some("ST-1".into()),
        session_id: Some("ses_1".into()),
    }
}

fn tool(e: &TestEnv, who: &str, name: &str, args: Value) -> Result<Value, String> {
    todo_tools::call_sync(&e.modules, &cx(who), name, &args)
        .map(|s| serde_json::from_str(&s).unwrap())
        .map_err(|e| e.to_string())
}

#[test]
fn agent_tools_act_for_the_sessions_person_only_and_are_undoable() {
    let e = env();
    // a session for the workspace, not a person: no todos to read or write
    let svc = format!("svc:{WS}");
    for name in [todo_tools::LIST, todo_tools::ADD] {
        let err = tool(&e, &svc, name, json!({"text": "x"})).unwrap_err();
        assert!(err.contains("belong to a person"), "{err}");
    }
    let names: Vec<String> = todo_tools::tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.iter().all(|n| todo_tools::handles(n)) && names.len() == 4);
    // times are explicit and echoed back
    let err = tool(
        &e,
        ME,
        todo_tools::ADD,
        json!({"text": "call the bank", "due": "next friday"}),
    )
    .unwrap_err();
    assert!(err.contains("RFC 3339"), "{err}");
    let added = tool(
        &e,
        ME,
        todo_tools::ADD,
        json!({"text": "call the bank", "due": "2027-02-05T17:00:00+08:00", "pri": 1}),
    )
    .unwrap();
    let id = added["todo"]["id"].as_str().unwrap().to_string();
    assert_eq!(added["todo"]["source"], "agent");
    let due = added["todo"]["due"].as_str().unwrap();
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(due)
            .unwrap()
            .timestamp_millis(),
        chrono::DateTime::parse_from_rfc3339("2027-02-05T17:00:00+08:00")
            .unwrap()
            .timestamp_millis()
    );
    let goal = tool(
        &e,
        ME,
        todo_tools::ADD,
        json!({"text": "Sort out finances", "kind": "goal", "target_date": "2027-04-01"}),
    )
    .unwrap();
    let goal_id = goal["todo"]["id"].as_str().unwrap();
    let up = tool(
        &e,
        ME,
        todo_tools::UPDATE,
        json!({"todo": id, "goal": goal_id, "due": "2027-02-06", "star": true}),
    )
    .unwrap();
    assert_eq!(up["todo"]["due"], "2027-02-06");
    assert_eq!(up["todo"]["goal"]["id"], json!(goal_id));
    let listed = tool(&e, ME, todo_tools::LIST, json!({"goal": goal_id})).unwrap();
    assert_eq!(listed["todos"].as_array().unwrap().len(), 1);
    let done = tool(&e, ME, todo_tools::COMPLETE, json!({"todo": id})).unwrap();
    assert_eq!(done["todo"]["state"], "done");
    let g = tool(&e, ME, todo_tools::LIST, json!({"kind": "goal"})).unwrap();
    assert_eq!(g["todos"][0]["progress"]["pct"], 100);
    // another member's todos are not there for this person's agent
    let theirs = add(&e, OTHER, "their errand", |_| {});
    for (name, args) in [
        (
            todo_tools::UPDATE,
            json!({"todo": theirs, "text": "hijacked"}),
        ),
        (todo_tools::COMPLETE, json!({"todo": theirs})),
    ] {
        assert!(tool(&e, ME, name, args).is_err());
    }
    assert!(
        tool(
            &e,
            ME,
            todo_tools::UPDATE,
            json!({"todo": id, "goal": theirs})
        )
        .is_err()
    );
    let mine = tool(&e, ME, todo_tools::LIST, json!({"state": "all"})).unwrap();
    assert!(
        mine["todos"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["id"] != json!(theirs))
    );
    // every change is undoable, newest first
    let done_change = done["change"].as_str().unwrap();
    changes::undo(&e.modules, WS, ME, done_change).unwrap();
    changes::undo(&e.modules, WS, ME, up["change"].as_str().unwrap()).unwrap();
    let t = e
        .store()
        .reader(|c| todo::get(c, WS, &id))
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            t["state"].as_str(),
            t["star"].as_bool(),
            t["parent"].is_null()
        ),
        (Some("open"), Some(false), true)
    );
    changes::undo(&e.modules, WS, ME, added["change"].as_str().unwrap()).unwrap();
    assert!(
        e.store()
            .reader(|c| todo::get(c, WS, &id))
            .unwrap()
            .is_none()
    );
    // the goal with nothing under it any more can go too
    changes::undo(&e.modules, WS, ME, goal["change"].as_str().unwrap()).unwrap();
}

#[test]
fn a_goal_with_todos_under_it_is_not_deleted_by_undo() {
    let e = env();
    let g = tool(
        &e,
        ME,
        todo_tools::ADD,
        json!({"text": "Learn Rust", "kind": "goal"}),
    )
    .unwrap();
    let gid = g["todo"]["id"].as_str().unwrap();
    tool(
        &e,
        ME,
        todo_tools::ADD,
        json!({"text": "read the book", "goal": gid}),
    )
    .unwrap();
    assert!(changes::undo(&e.modules, WS, ME, g["change"].as_str().unwrap()).is_err());
    assert!(
        e.store()
            .reader(|c| todo::get(c, WS, gid))
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_members_todo_links_to_nothing_of_the_owners_digest() {
    let e = env();
    // the owner's digest: a matter, a message in a space, a person
    e.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
                    VALUES ('s1', 'default', 'test', 'group', 'g1', 'Board room', 1, 1);
                 INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated)
                    VALUES ('me', 'default', 'person', 'Owner', 'usr_dev', 'usr_dev', 1, 1),
                           ('alice', 'default', 'person', 'Alice', NULL, 'usr_dev', 1, 1);
                 INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
                    VALUES ('owner_acc', 'default', 'test', 'other', 'me', 'personal', 'owner', 'Owner', 'me', 1, 1);
                 INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id, text, mentions, created)
                    VALUES ('m1', 'default', 's1', 'm1', 'in', 'alice', 'the merger closes in March', '[]', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let matter = e
        .store()
        .transaction(|tx| {
            steve_digest::matter::create(tx, WS, "Secret merger", ME, NOW)
                .map_err(|e| StoreError::Hook(format!("{e:?}")))
        })
        .unwrap();
    let app = steve_digest::routes().with_state(e.modules.clone());
    let (me, other) = (caller(WS, ME), caller(WS, OTHER));
    // the owner links them and sees them
    let (s, b) = call(
        &app,
        Some(&me),
        "POST",
        "/digest/todos",
        Some(&json!({"text": "prepare the merger memo", "matter": matter,
            "refs": [[{"type": "space", "id": "s1"}, "m1"]], "parties": ["alice"]})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let (_, t) = call(
        &app,
        Some(&me),
        "GET",
        &format!("/digest/todos/{}", b["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(t["matter"]["title"], "Secret merger");
    assert_eq!(t["sources"][0]["name"], "Board room");
    assert_eq!(t["people"][0]["name"], "Alice");
    // another member attaches none of them, by REST or by their agent
    for body in [
        json!({"text": "peek", "matter": matter}),
        json!({"text": "peek", "refs": [[{"type": "space", "id": "s1"}, "m1"]]}),
        json!({"text": "peek", "parties": ["alice"]}),
    ] {
        let (s, b) = call(&app, Some(&other), "POST", "/digest/todos", Some(&body)).await;
        assert_eq!(s, 400, "{body} {b}");
        assert!(
            b.to_string().contains("owner") || b.to_string().contains("所有者"),
            "{b}"
        );
    }
    let (_, b) = call(
        &app,
        Some(&other),
        "POST",
        "/digest/todos",
        Some(&json!({"text": "mine"})),
    )
    .await;
    let theirs = b["id"].as_str().unwrap().to_string();
    let (s, _) = call(
        &app,
        Some(&other),
        "PATCH",
        &format!("/digest/todos/{theirs}"),
        Some(&json!({"matter": matter})),
    )
    .await;
    assert_eq!(s, 400);
    for (name, args) in [
        (todo_tools::ADD, json!({"text": "peek", "matter": matter})),
        (
            todo_tools::UPDATE,
            json!({"todo": theirs, "matter": matter}),
        ),
    ] {
        let err = tool(&e, OTHER, name, args).unwrap_err();
        assert!(
            err.contains("owner") || err.contains("所有者"),
            "{name}: {err}"
        );
    }
    // and a member's todo that holds them anyway shows none of their content
    e.store()
        .with_conn(|c| {
            c.execute(
                "UPDATE todos SET matter_id = ? WHERE id = ?",
                params![matter, theirs],
            )?;
            c.execute(
                "INSERT INTO todo_refs (todo_id, conv, message_id) VALUES (?, 'space:s1', 'm1')",
                [&theirs],
            )?;
            c.execute(
                "INSERT INTO todo_parties (todo_id, party_id) VALUES (?, 'alice')",
                [&theirs],
            )?;
            Ok(())
        })
        .unwrap();
    let (_, t) = call(
        &app,
        Some(&other),
        "GET",
        &format!("/digest/todos/{theirs}"),
        None,
    )
    .await;
    let shown = t.to_string();
    for secret in ["Secret merger", "Board room", "merger closes", "Alice"] {
        assert!(!shown.contains(secret), "{secret} in {shown}");
    }
    // their own todo stays editable with what it holds
    let (s, b) = call(
        &app,
        Some(&other),
        "PATCH",
        &format!("/digest/todos/{theirs}"),
        Some(&json!({"text": "mine, renamed"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
}
