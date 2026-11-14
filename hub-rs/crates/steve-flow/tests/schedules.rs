//! S7 acceptance plus the REST rules, on a temp store with a fake clock,
//! a fake model gateway and a fake steve-core.

use chrono::{TimeZone, Utc};
use rusqlite::params;
use serde_json::{Value, json};
use steve_flow::scheduler::{self, claim_due, drive, recover, start_manual};
use steve_flow::testkit::{CoreCall, FlowEnv};
use steve_flow::{RunStatus, budget, digest, on_session_cost, runs};
use steve_modules::GatewayOut;
use steve_modules::testkit::{ME, WS, WS2, call, caller};

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

/// 2026-11-02 08:00 in Asia/Taipei (a Monday).
fn monday_8am() -> i64 {
    Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0)
        .unwrap()
        .timestamp_millis()
}

fn env() -> FlowEnv {
    let e = FlowEnv::new();
    e.env.clock.set(monday_8am());
    e
}

async fn create(e: &FlowEnv, body: Value) -> Value {
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/schedules",
        Some(&body),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v
}

fn daily(mode: &str) -> Value {
    json!({"name": "离开期间汇报", "when": "每天 08:30", "what": "汇报离开期间的事", "bot": "steve", "mode": mode})
}

fn runs_of(e: &FlowEnv, flow: &str) -> Vec<runs::RunRow> {
    e.env
        .store()
        .reader(|c| runs::page(c, WS, flow, None, 50))
        .unwrap()
}

fn next_at(e: &FlowEnv, flow: &str) -> Option<i64> {
    e.env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT next_at FROM flow_triggers WHERE flow_id = ?",
                [flow],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

async fn drive_all(e: &FlowEnv, ids: &[String]) {
    for id in ids {
        drive(&e.flow, id).await.unwrap();
    }
}

#[tokio::test]
async fn create_computes_next_run_and_describe() {
    let e = env();
    let s = create(&e, daily("report")).await;
    assert_eq!(s["cron"], "30 8 * * *");
    assert_eq!(s["describe"], "每天 08:30");
    assert_eq!(s["when"], "每天 08:30");
    assert_eq!(s["tz"], "Asia/Taipei");
    assert_eq!(s["budget_usd"], 1.0);
    assert_eq!(s["missed"], "catch_up_once");
    assert_eq!(s["overlap"], "skip");
    assert_eq!(s["enabled"], true);
    assert_eq!(s["last"], Value::Null);
    assert_eq!(s["next_at"], monday_8am() + 30 * MIN);
    // the graph and the trigger index
    let (graph, kind, version): (String, String, i64) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT graph, kind, version FROM flows WHERE id = ?",
                [s["id"].as_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    let g: Value = serde_json::from_str(&graph).unwrap();
    assert_eq!((kind.as_str(), version), ("schedule", 1));
    assert_eq!(g["nodes"][0]["config"]["source"], "time");
    assert_eq!(g["nodes"][1]["config"]["action"], "report");
    assert_eq!(g["edges"][0], json!({"from": "t", "to": "a"}));
}

#[tokio::test]
async fn validation_and_ownership() {
    let e = env();
    let r = e.router();
    let me = caller(WS, ME);
    let bad = |b: Value| {
        let r = r.clone();
        let me = me.clone();
        async move { call(&r, Some(&me), "POST", "/schedules", Some(&b)).await }
    };
    let (st, v) = bad(json!({"name": "", "what": "x", "bot": "steve", "mode": "item"})).await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "参数不对：name、when")
    );
    let (st, v) = bad(
        json!({"name": "a", "cron": "0 0 9 * * *", "what": "x", "bot": "steve", "mode": "item"}),
    )
    .await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "不支持这种 cron 写法：0 0 9 * * *")
    );
    let (st, v) = bad(
        json!({"name": "a", "cron": "*/2 * * * *", "what": "x", "bot": "steve", "mode": "item"}),
    )
    .await;
    assert_eq!((st, v["error"].as_str().unwrap()), (400, "最短间隔 5 分钟"));
    let (st, v) =
        bad(json!({"name": "a", "when": "每天 9 点", "what": "x", "bot": "w2bot", "mode": "item"}))
            .await;
    assert_eq!((st, v["error"].as_str().unwrap()), (404, "没有这个 Bot"));
    let (st, _) = bad(json!({"name": "a", "when": "每天 9 点", "what": "x", "bot": "steve", "mode": "item", "project_id": "prj_x"})).await;
    assert_eq!(st, 404);
    let (st, _) = bad(json!({"name": "a", "when": "每天 9 点", "what": "x", "bot": "steve", "mode": "item", "budget_usd": 51})).await;
    assert_eq!(st, 400);
    let (st, _) = bad(
        json!({"name": "a", "when": "每天 9 点", "what": "x", "bot": "steve", "mode": "weekly"}),
    )
    .await;
    assert_eq!(st, 400);
    // another workspace cannot see it
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap();
    let (st, v) = call(
        &r,
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/schedules/{id}"),
        None,
    )
    .await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (404, "没有这个定时任务")
    );
    let (st, _) = call(&r, None, "GET", "/schedules", None).await;
    assert_eq!(st, 401);
    let (_, list) = call(&r, Some(&caller(WS2, ME)), "GET", "/schedules", None).await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn parse_rules_then_model() {
    let e = env();
    let r = e.router();
    let me = caller(WS, ME);
    let (st, v) = call(
        &r,
        Some(&me),
        "POST",
        "/schedules/parse",
        Some(&json!({"when": "工作日早上九点半"})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(
        (v["cron"].as_str(), v["describe"].as_str(), v["by"].as_str()),
        (Some("30 9 * * 1-5"), Some("工作日 09:30"), Some("rule"))
    );
    assert_eq!(v["next"].as_array().unwrap().len(), 3);
    assert_eq!(v["next"][0], monday_8am() + 90 * MIN);
    assert!(e.env.gateway.calls.lock().unwrap().is_empty());
    // not a rule: the model (fast tier) answers, and its answer is validated
    e.env.gateway.reply("```\n0 7 * * 2\n```");
    let (st, v) = call(
        &r,
        Some(&me),
        "POST",
        "/schedules/parse",
        Some(&json!({"when": "每逢周二清早七点"})),
    )
    .await;
    assert_eq!(
        (st, v["cron"].as_str(), v["by"].as_str()),
        (200, Some("0 7 * * 2"), Some("model"))
    );
    assert_eq!(
        e.env.gateway.calls.lock().unwrap()[0].0,
        steve_modules::Tier::Fast
    );
    e.env.gateway.reply("我不知道");
    let (st, v) = call(
        &r,
        Some(&me),
        "POST",
        "/schedules/parse",
        Some(&json!({"when": "看心情"})),
    )
    .await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "没看懂这个时间，换个说法，例如“每周一 09:00”")
    );
    let (_, v) = call(
        &r,
        Some(&me),
        "POST",
        "/schedules/parse",
        Some(&json!({"when": "每周一"})),
    )
    .await;
    assert_eq!(
        (v["cron"].as_str(), v["assumed"].as_str()),
        (Some("0 9 * * 1"), Some("没说几点，按 09:00"))
    );
}

#[tokio::test]
async fn due_trigger_starts_exactly_one_run_even_with_two_loops() {
    let e = env();
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap().to_string();
    // not due yet
    assert!(claim_due(&e.flow).unwrap().is_empty());
    e.env.clock.set(monday_8am() + 30 * MIN);
    // two loops claim at the same moment, on two threads
    let (a, b) = std::thread::scope(|sc| {
        let f1 = e.flow.clone();
        let f2 = e.flow.clone();
        let h1 = sc.spawn(move || claim_due(&f1).unwrap());
        let h2 = sc.spawn(move || claim_due(&f2).unwrap());
        (h1.join().unwrap(), h2.join().unwrap())
    });
    assert_eq!(a.len() + b.len(), 1);
    let rs = runs_of(&e, &id);
    assert_eq!(rs.len(), 1);
    assert_eq!(
        (rs[0].trigger_kind.as_str(), rs[0].status.as_str()),
        ("time", "running")
    );
    assert_eq!(rs[0].at(), monday_8am() + 30 * MIN);
    assert_eq!(next_at(&e, &id), Some(monday_8am() + 30 * MIN + DAY));
    // another round at the same time finds nothing
    assert!(claim_due(&e.flow).unwrap().is_empty());
}

#[tokio::test]
async fn a_broken_trigger_does_not_lose_the_others_claims() {
    let e = env();
    let a = create(&e, daily("report")).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = create(&e, daily("report")).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    // `a`'s claim fails every round (its graph cannot be read)
    e.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE flows SET graph = 'not json' WHERE id = ?", [&a])?;
            Ok(())
        })
        .unwrap();
    e.env.clock.set(monday_8am() + 30 * MIN);
    // `b` is claimed and returned (so it is driven), whatever order the two come in
    let ids = claim_due(&e.flow).unwrap();
    assert_eq!(ids.len(), 1);
    let rs = runs_of(&e, &b);
    assert_eq!(rs.len(), 1);
    assert_eq!(
        (rs[0].id.as_str(), rs[0].status.as_str()),
        (ids[0].as_str(), "running")
    );
    // `a`'s claim rolled back: no run, still due
    assert!(runs_of(&e, &a).is_empty());
    assert_eq!(next_at(&e, &a), Some(monday_8am() + 30 * MIN));
    assert!(claim_due(&e.flow).unwrap().is_empty());
}

#[tokio::test]
async fn hub_down_three_days_catch_up_once() {
    let e = env();
    let s = create(&e, daily("item_auto")).await;
    let id = s["id"].as_str().unwrap().to_string();
    // down from before Monday 08:30 until Wednesday 09:00: Mon, Tue, Wed 08:30 missed
    e.env.clock.set(monday_8am() + 2 * DAY + HOUR);
    let ids = claim_due(&e.flow).unwrap();
    assert_eq!(ids.len(), 1);
    drive_all(&e, &ids).await;
    let rs = runs_of(&e, &id);
    assert_eq!(rs.len(), 1);
    let r = &rs[0];
    assert_eq!(
        (r.trigger_kind.as_str(), r.status.as_str()),
        ("catch_up", "done")
    );
    assert_eq!(r.context["missed"], 3);
    assert_eq!(r.at(), monday_8am() + 30 * MIN);
    assert!(
        r.outcome_label.starts_with("补跑（错过 3 次）建立 ST-1"),
        "{}",
        r.outcome_label
    );
    let goal = &e.core.created()[0].goal;
    assert!(
        goal.contains("计划时间 2026-11-02 08:30，补跑，错过 3 次"),
        "{goal}"
    );
    // next run is the first after now: Thursday 08:30
    assert_eq!(next_at(&e, &id), Some(monday_8am() + 3 * DAY + 30 * MIN));
    assert!(claim_due(&e.flow).unwrap().is_empty());
}

#[tokio::test]
async fn hub_down_three_days_skip() {
    let e = env();
    let mut b = daily("item_auto");
    b["missed"] = json!("skip");
    let s = create(&e, b).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env.clock.set(monday_8am() + 2 * DAY + HOUR);
    assert!(claim_due(&e.flow).unwrap().is_empty());
    let rs = runs_of(&e, &id);
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0].status, "skipped");
    assert_eq!(rs[0].outcome_label, "Hub 没在运行，错过 3 次，按设置跳过");
    assert!(e.core.created().is_empty());
    assert_eq!(next_at(&e, &id), Some(monday_8am() + 3 * DAY + 30 * MIN));
    // `last` ignores skipped rows
    let (_, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/schedules/{id}"),
        None,
    )
    .await;
    assert_eq!(v["last"], Value::Null);
    assert_eq!(v["runs"][0]["status"], "skipped");
}

#[tokio::test]
async fn boot_applies_missed_policy() {
    let e = env();
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env.gateway.script("你是 Steve", "汇报正文");
    e.env.clock.set(monday_8am() + DAY + HOUR);
    let handles = scheduler::Scheduler::new(e.flow.clone())
        .boot()
        .await
        .unwrap();
    assert_eq!(handles.len(), 1);
    for h in handles {
        assert_eq!(h.await.unwrap().unwrap(), RunStatus::Done);
    }
    let rs = runs_of(&e, &id);
    assert_eq!((rs.len(), rs[0].trigger_kind.as_str()), (1, "catch_up"));
    assert_eq!(rs[0].outcome_label, "补跑（错过 2 次）已发布到信息流");
}

#[tokio::test]
async fn disabled_time_is_not_missed() {
    let e = env();
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let r = e.router();
    let me = caller(WS, ME);
    let (_, v) = call(
        &r,
        Some(&me),
        "PATCH",
        &format!("/schedules/{id}"),
        Some(&json!({"enabled": false})),
    )
    .await;
    assert_eq!(
        (v["enabled"].as_bool(), v["next_at"].as_i64()),
        (Some(false), None)
    );
    e.env.clock.set(monday_8am() + 5 * DAY);
    assert!(claim_due(&e.flow).unwrap().is_empty());
    let (_, v) = call(
        &r,
        Some(&me),
        "PATCH",
        &format!("/schedules/{id}"),
        Some(&json!({"enabled": true})),
    )
    .await;
    assert_eq!(v["next_at"], monday_8am() + 5 * DAY + 30 * MIN);
    assert_eq!(v["version"], 1, "enabling is not a new version");
    assert!(claim_due(&e.flow).unwrap().is_empty());
    assert!(runs_of(&e, &id).is_empty());
}

#[tokio::test]
async fn patch_time_recomputes_and_versions() {
    let e = env();
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap();
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/schedules/{id}"),
        Some(&json!({"when": "每周五 9 点", "budget_usd": 0.5, "overlap": "run"})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (
            v["cron"].as_str(),
            v["describe"].as_str(),
            v["when"].as_str()
        ),
        (Some("0 9 * * 5"), Some("每周五 09:00"), Some("每周五 9 点"))
    );
    assert_eq!(v["version"], 2);
    assert_eq!(v["budget_usd"], 0.5);
    // Friday 2026-11-06 09:00 Taipei
    assert_eq!(v["next_at"], monday_8am() + 4 * DAY + HOUR);
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "DELETE",
        &format!("/schedules/{id}"),
        None,
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/schedules/{id}"),
        None,
    )
    .await;
    assert_eq!(st, 404);
    let n: i64 = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM flow_triggers WHERE flow_id = ?",
                [id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn overlap_skip_while_previous_is_running() {
    let e = env();
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env.clock.set(monday_8am() + 30 * MIN);
    let first = claim_due(&e.flow).unwrap();
    assert_eq!(first.len(), 1);
    // still running a day later (not driven)
    e.env.clock.set(monday_8am() + DAY + 30 * MIN);
    assert!(claim_due(&e.flow).unwrap().is_empty());
    let rs = runs_of(&e, &id);
    assert_eq!(rs.len(), 2);
    assert_eq!(
        (rs[0].status.as_str(), rs[0].outcome_label.as_str()),
        ("skipped", "上一次还没结束，跳过")
    );
    // overlap=run starts anyway
    let (_, _) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/schedules/{id}"),
        Some(&json!({"overlap": "run"})),
    )
    .await;
    e.env.clock.set(monday_8am() + 2 * DAY + 30 * MIN);
    assert_eq!(claim_due(&e.flow).unwrap().len(), 1);
}

#[tokio::test]
async fn run_now_keeps_next_at() {
    let e = env();
    e.env.gateway.script("你是 Steve", "汇报正文");
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let before = next_at(&e, &id);
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        &format!("/schedules/{id}/run"),
        None,
    )
    .await;
    assert_eq!(st, 200);
    let run_id = v["run_id"].as_str().unwrap().to_string();
    // the route spawned the drive; driving again is a no-op once it is done
    for _ in 0..100 {
        if runs_of(&e, &id)[0].status != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    drive(&e.flow, &run_id).await.unwrap();
    let rs = runs_of(&e, &id);
    assert_eq!(
        (rs[0].trigger_kind.as_str(), rs[0].status.as_str()),
        ("manual", "done")
    );
    assert_eq!(rs[0].at(), monday_8am());
    assert_eq!(next_at(&e, &id), before);
    // the overlap rule applies to manual runs too
    let (run2, started) = start_manual(&e.flow, WS, &id).unwrap();
    assert!(started);
    let (run3, started) = start_manual(&e.flow, WS, &id).unwrap();
    assert!(!started);
    let r3 = e
        .env
        .store()
        .reader(|c| runs::get(c, &run3))
        .unwrap()
        .unwrap();
    assert_eq!(r3.status, "skipped");
    assert_ne!(run2, run3);
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS2, ME)),
        "POST",
        &format!("/schedules/{id}/run"),
        None,
    )
    .await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn item_auto_creates_linked_auto_accept_item() {
    let e = env();
    let mut b = daily("item_auto");
    b["name"] = json!("整理周报");
    b["project_id"] = json!("prj_a");
    b["tags"] = json!(["tag_a"]);
    let s = create(&e, b).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    drive_all(&e, &ids).await;
    let req = &e.core.created()[0];
    assert_eq!(req.title, "整理周报 · 11-02");
    assert_eq!(
        req.goal,
        "汇报离开期间的事\n\n（定时任务「整理周报」，计划时间 2026-11-02 08:30）"
    );
    assert_eq!(
        (req.bot.as_str(), req.project.as_deref(), req.propose),
        ("steve", Some("prj_a"), false)
    );
    assert_eq!(req.tag_ids, vec!["tag_a".to_string()]);
    let (accept, source): (Option<String>, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT accept_mode, source FROM items WHERE id = 'ST-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(accept.as_deref(), Some("auto"));
    assert_eq!(
        serde_json::from_str::<Value>(&source).unwrap(),
        json!(["定时", "sched"])
    );
    let run = &runs_of(&e, &id)[0];
    let link: i64 = e.env.store().with_conn(|c| Ok(c.query_row(
        "SELECT count(*) FROM links WHERE workspace_id = 'default' AND subject_type = 'item' AND subject_id = 'ST-1' AND predicate = 'from' AND object_type = 'flow_run' AND object_id = ?",
        [&run.id], |r| r.get(0))?)).unwrap();
    assert_eq!(link, 1);
    assert_eq!(
        (
            run.status.as_str(),
            run.outcome_label.as_str(),
            run.work_item_id.as_deref()
        ),
        ("done", "建立 ST-1", Some("ST-1"))
    );
    // the next run tells the item what happened last time
    e.env.clock.set(monday_8am() + DAY + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    drive_all(&e, &ids).await;
    assert!(
        e.core.created()[1]
            .goal
            .ends_with("。上次结果：建立 ST-1）")
    );
    // the schedule shows its last run
    let (_, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/schedules/{id}"),
        None,
    )
    .await;
    assert_eq!(v["last"]["text"], "建立 ST-2");
    assert_eq!(v["last"]["item_id"], "ST-2");
    assert_eq!(v["runs"].as_array().unwrap().len(), 2);
    let (_, page) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/schedules/{id}/runs?limit=1"),
        None,
    )
    .await;
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["trigger"], "time");
    let before = page[0]["started"].as_i64().unwrap();
    let (_, page2) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/schedules/{id}/runs?before={before}"),
        None,
    )
    .await;
    assert_eq!(page2[0]["item_id"], "ST-1");
}

#[tokio::test]
async fn propose_and_unavailable_bot() {
    let e = env();
    let mut b = daily("propose");
    b["bot"] = json!("off");
    let s = create(&e, b).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    drive_all(&e, &ids).await;
    let req = &e.core.created()[0];
    assert!(req.propose);
    assert!(!req.accept_auto);
    assert_eq!(req.bot, "steve");
    assert_eq!(
        runs_of(&e, &id)[0].outcome_label,
        "提了方案 ST-1（停用的 不可用，交给了 Steve）"
    );
}

fn add_session(e: &FlowEnv, sid: &str, item: &str, cost: f64) {
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO sessions (id, workspace_id, item_id, harness, cwd, status, cost, created, updated) VALUES (?, 'default', ?, 'claude-code', '/tmp', 'running', ?, 1, 1) ON CONFLICT(id) DO UPDATE SET cost = excluded.cost",
                params![sid, item, cost],
            )?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn budget_pauses_and_continue_once_doubles() {
    let e = env();
    let mut b = daily("item_auto");
    b["name"] = json!("整理周报");
    b["budget_usd"] = json!(0.5);
    let s = create(&e, b).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    drive_all(&e, &ids).await;
    let run_id = runs_of(&e, &id)[0].id.clone();

    add_session(&e, "ses_1", "ST-1", 0.3);
    assert_eq!(on_session_cost(&e.flow, "ses_1").await, None);
    add_session(&e, "ses_2", "ST-1", 0.4);
    let dec = on_session_cost(&e.flow, "ses_2")
        .await
        .expect("over budget");
    let r = e
        .env
        .store()
        .reader(|c| runs::get(c, &run_id))
        .unwrap()
        .unwrap();
    assert!((r.cost - 0.7).abs() < 1e-9);
    assert!(e.core.calls().contains(&CoreCall::Pause("ST-1".into())));
    let d = e
        .env
        .store()
        .reader(|c| steve_store::base::decisions::get(c, &dec))
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            d.kind.as_str(),
            d.subject_type.as_str(),
            d.subject_id.as_str(),
            d.item_id.as_deref()
        ),
        ("fail", "flow_run", run_id.as_str(), Some("ST-1"))
    );
    assert_eq!(
        d.question,
        "「整理周报」这次花了 $0.70，超过预算 $0.50，已暂停 ST-1"
    );
    assert_eq!(d.options, vec!["continue_once".to_string(), "stop".into()]);
    // more cost under the same limit does not open another decision
    add_session(&e, "ses_2", "ST-1", 0.6);
    assert_eq!(on_session_cost(&e.flow, "ses_2").await, None);

    let res = steve_modules::decisions::dispatch(
        &e.env.modules,
        &caller(WS, ME),
        &dec,
        budget::CONTINUE_ONCE,
        "",
    )
    .unwrap();
    assert!(res.is_ok());
    e.flow.run_followups().await;
    assert!(e.core.calls().contains(&CoreCall::Resume("ST-1".into())));
    let r = e
        .env
        .store()
        .reader(|c| runs::get(c, &run_id))
        .unwrap()
        .unwrap();
    // spent $0.90 against $0.50: the limit becomes max(0.5 × 2, 0.9 × 2) = $1.80, not $1.00
    assert!((r.context["budget_limit"].as_f64().unwrap() - 1.8).abs() < 1e-9);
    add_session(&e, "ses_3", "ST-1", 0.2);
    assert_eq!(
        on_session_cost(&e.flow, "ses_3").await,
        None,
        "$1.10 is under $1.80"
    );
    // over the raised limit: a second decision
    add_session(&e, "ses_3", "ST-1", 1.0);
    let dec2 = on_session_cost(&e.flow, "ses_3")
        .await
        .expect("over the raised budget");
    assert_ne!(dec, dec2);
    let res = steve_modules::decisions::dispatch(
        &e.env.modules,
        &caller(WS, ME),
        &dec2,
        budget::STOP,
        "",
    )
    .unwrap();
    assert!(res.is_ok());
    e.flow.run_followups().await;
    let resumes = e
        .core
        .calls()
        .iter()
        .filter(|c| matches!(c, CoreCall::Resume(_)))
        .count();
    assert_eq!(resumes, 1, "stop keeps the item paused");
    // shown on the schedule
    let (_, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/schedules/{id}"),
        None,
    )
    .await;
    assert!((v["last"]["cost_usd"].as_f64().unwrap() - 1.9).abs() < 1e-9);
}

#[tokio::test]
async fn the_default_budget_follows_the_mode() {
    let e = env();
    for (mode, usd) in [
        ("item_auto", 20.0),
        ("item", 20.0),
        ("propose", 1.0),
        ("report", 1.0),
    ] {
        let mut b = daily(mode);
        b["name"] = json!(format!("预算 {mode}"));
        let s = create(&e, b).await;
        assert_eq!(s["budget_usd"], usd, "{mode}");
    }
    // a budget you give is kept
    let mut b = daily("item_auto");
    b["name"] = json!("自己定");
    b["budget_usd"] = json!(3);
    assert_eq!(create(&e, b).await["budget_usd"], 3.0);
    assert_eq!(budget::raised_limit(1.0, 0.0), 2.0);
    assert_eq!(budget::raised_limit(1.0, 9.0), 18.0);
}

#[tokio::test]
async fn budget_zero_is_unlimited() {
    let e = env();
    let mut b = daily("item");
    b["budget_usd"] = json!(0);
    create(&e, b).await;
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    drive_all(&e, &ids).await;
    add_session(&e, "ses_1", "ST-1", 40.0);
    assert_eq!(on_session_cost(&e.flow, "ses_1").await, None);
    // a session of an item that came from no schedule
    assert_eq!(on_session_cost(&e.flow, "ses_nope").await, None);
}

#[tokio::test]
async fn report_publishes_to_feed_without_chat_and_takes_digest() {
    let e = env();
    e.env.gateway.script_out(
        "你是 Steve",
        GatewayOut {
            text: "  昨晚 ST-1 做完了。 ".into(),
            cost_usd: Some(0.02),
            model: "fake".into(),
            ..GatewayOut::default()
        },
    );
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let now = e.now();
    e.env
        .store()
        .transaction(|tx| digest::add(tx, WS, "Acme 回信了", None, None, now))
        .unwrap();
    e.env
        .store()
        .transaction(|tx| digest::add(tx, WS2, "别的空间", None, None, now))
        .unwrap();
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    drive_all(&e, &ids).await;
    let posts = e.core.posts();
    assert!(posts.is_empty());
    let run = &runs_of(&e, &id)[0];
    let result = e
        .env
        .store()
        .reader(|c| steve_store::feed::report_result(c, WS, &run.id, "a"))
        .unwrap();
    assert_eq!(result.unwrap()["text"], "昨晚 ST-1 做完了。");
    assert_eq!(
        (run.status.as_str(), run.outcome_label.as_str()),
        ("done", "已发布到信息流")
    );
    assert!((run.cost - 0.02).abs() < 1e-9);
    let prompt = e.env.gateway.last_prompt().unwrap();
    assert!(prompt.contains("要求：汇报离开期间的事"));
    assert!(prompt.contains("现在：0 件等你"));
    assert!(prompt.contains("- Acme 回信了"));
    assert!(!prompt.contains("别的空间"));
    assert_eq!(
        e.env.gateway.calls.lock().unwrap()[0].0,
        steve_modules::Tier::Smart
    );
    assert!(
        e.env
            .store()
            .reader(|c| digest::pending(c, WS, 0))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        e.env
            .store()
            .reader(|c| digest::pending(c, WS2, 0))
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn report_retries_once_after_ten_minutes() {
    let e = env();
    e.env.gateway.script_fail("你是 Steve", "timeout");
    let s = create(&e, daily("report")).await;
    let id = s["id"].as_str().unwrap().to_string();
    let now = e.now();
    e.env
        .store()
        .transaction(|tx| digest::add(tx, WS, "留着", None, None, now))
        .unwrap();
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    assert_eq!(drive(&e.flow, &ids[0]).await.unwrap(), RunStatus::Waiting);
    let r = &runs_of(&e, &id)[0];
    assert_eq!((r.retry_at, r.attempts), (Some(monday_8am() + 40 * MIN), 1));
    e.env.clock.set(monday_8am() + 39 * MIN);
    assert!(claim_due(&e.flow).unwrap().is_empty());
    e.env.clock.set(monday_8am() + 40 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    assert_eq!(ids, vec![r.id.clone()]);
    assert_eq!(drive(&e.flow, &ids[0]).await.unwrap(), RunStatus::Failed);
    let r = &runs_of(&e, &id)[0];
    assert_eq!(r.error.as_deref(), Some("写汇报失败：timeout"));
    assert!(e.core.posts().is_empty());
    // nothing was taken
    assert_eq!(
        e.env
            .store()
            .reader(|c| digest::pending(c, WS, 0))
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn recover_marks_interrupted_runs_failed() {
    let e = env();
    let s = create(&e, daily("item_auto")).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env.clock.set(monday_8am() + 30 * MIN);
    claim_due(&e.flow).unwrap();
    assert_eq!(recover(&e.flow).unwrap(), 1);
    let r = &runs_of(&e, &id)[0];
    assert_eq!(
        (r.status.as_str(), r.error.as_deref()),
        ("failed", Some("Hub 重启时这一步可能没有完成"))
    );
    assert!(e.core.created().is_empty(), "not replayed");
}

#[tokio::test]
async fn retention_keeps_90_days_or_200_runs() {
    let e = env();
    let a = create(&e, daily("report")).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = create(&e, daily("report")).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let now = e.now();
    e.env
        .store()
        .with_conn(|c| {
            for i in 0..210 {
                c.execute("INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES (?, 'default', ?, 1, 'time', 'done', ?)", params![format!("run_a{i}"), a, now - 100 * DAY + i])?;
                c.execute("INSERT INTO flow_run_steps (run_id, seq, workspace_id, node_id, status, started) VALUES (?, 1, 'default', 't', 'ok', 1)", [format!("run_a{i}")])?;
            }
            for i in 0..5 {
                c.execute("INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES (?, 'default', ?, 1, 'time', 'done', ?)", params![format!("run_r{i}"), a, now - DAY + i])?;
            }
            for i in 0..10 {
                c.execute("INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES (?, 'default', ?, 1, 'time', 'done', ?)", params![format!("run_b{i}"), b, now - 300 * DAY + i])?;
            }
            Ok(())
        })
        .unwrap();
    let n = e
        .env
        .store()
        .transaction(|tx| runs::prune(tx, now))
        .unwrap();
    assert_eq!(n, 15);
    let count = |sql: &str| -> i64 {
        e.env
            .store()
            .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    };
    assert_eq!(
        count(&format!(
            "SELECT count(*) FROM flow_runs WHERE flow_id = '{a}'"
        )),
        200
    );
    assert_eq!(
        count(&format!(
            "SELECT count(*) FROM flow_runs WHERE flow_id = '{b}'"
        )),
        10
    );
    // the oldest went, with their steps
    assert_eq!(
        count("SELECT count(*) FROM flow_runs WHERE id IN ('run_a0','run_a14')"),
        0
    );
    assert_eq!(
        count("SELECT count(*) FROM flow_runs WHERE id = 'run_a15'"),
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM flow_run_steps WHERE run_id LIKE 'run_a%'"),
        195
    );
}

#[tokio::test]
async fn events_are_scoped_to_the_workspace() {
    let e = env();
    e.env.take_events();
    create(&e, daily("report")).await;
    let ev = e.env.take_events();
    assert!(
        ev.iter()
            .any(|x| matches!(x, steve_store::ChangeEvent::Flow { ws, .. } if ws == WS))
    );
}

#[tokio::test]
async fn explicit_requester_reaches_item_without_changing_legacy_service_schedules() {
    // The fixture has two members: personal execution is not a single-user shortcut.
    let e = env();
    for requester in [None, Some(ME)] {
        let mut body = daily("item_auto");
        if let Some(user) = requester {
            body["requester"] = json!(user);
        }
        let s = create(&e, body).await;
        assert_eq!(s["requester"], json!(requester));
        let (run, go) = start_manual(&e.flow, WS, s["id"].as_str().unwrap()).unwrap();
        assert!(go);
        drive(&e.flow, &run).await.unwrap();
        let row = e
            .env
            .store()
            .reader(|c| runs::get(c, &run))
            .unwrap()
            .unwrap();
        let item = row.work_item_id.unwrap();
        let actual: Option<String> = e
            .env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT requested_by FROM items WHERE id = ?",
                    [&item],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(actual.as_deref(), requester);
    }
}

#[tokio::test]
async fn requester_authorizes_only_self_and_only_self_can_edit_personal_schedule() {
    use steve_modules::testkit::OTHER;
    let e = env();
    let mut body = daily("item");
    body["requester"] = json!(OTHER);
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/schedules",
        Some(&body),
    )
    .await;
    assert_eq!(st, 403);
    body["requester"] = json!(ME);
    let s = create(&e, body.clone()).await;
    let path = format!("/schedules/{}", s["id"].as_str().unwrap());
    for patch in [
        json!({"what": "use someone else's access"}),
        json!({"requester": null}),
    ] {
        let (st, _) = call(
            &e.router(),
            Some(&caller(WS, OTHER)),
            "PATCH",
            &path,
            Some(&patch),
        )
        .await;
        assert_eq!(st, 403);
    }
    body["mode"] = json!("report");
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/schedules",
        Some(&body),
    )
    .await;
    assert_eq!(st, 400);
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        &path,
        Some(&json!({"what": "updated"})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(v["requester"], ME);
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        &path,
        Some(&json!({"requester": null})),
    )
    .await;
    assert_eq!(st, 200);
    assert!(v["requester"].is_null());
}

#[tokio::test]
async fn removed_requester_and_old_graph_cannot_borrow_personal_execution() {
    let e = env();
    let s = create(&e, daily("item")).await;
    let id = s["id"].as_str().unwrap();
    let (old_run, _) = start_manual(&e.flow, WS, id).unwrap();
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/schedules/{id}"),
        Some(&json!({"requester": ME})),
    )
    .await;
    assert_eq!(st, 200);
    let _ = drive(&e.flow, &old_run).await;
    assert!(
        e.core
            .calls()
            .iter()
            .all(|c| !matches!(c, CoreCall::Create(_)))
    );
    let (run, go) = start_manual(&e.flow, WS, id).unwrap();
    assert!(go);
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "DELETE FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                params![WS, ME],
            )?;
            Ok(())
        })
        .unwrap();
    let _ = drive(&e.flow, &run).await;
    assert!(
        e.core
            .calls()
            .iter()
            .all(|c| !matches!(c, CoreCall::Create(_)))
    );
}

#[tokio::test]
async fn bot_deletion_revokes_personal_schedule_instead_of_lending_access_to_heir() {
    let e = env();
    let mut body = daily("item");
    body["bot"] = json!("ops");
    body["requester"] = json!(ME);
    let s = create(&e, body).await;
    e.env
        .store()
        .transaction(|tx| steve_flow::on_bot_deleted(tx, WS, "ops", "steve", monday_8am()))
        .unwrap();
    let saved = e
        .env
        .store()
        .reader(|c| steve_flow::schedules::own(c, WS, s["id"].as_str().unwrap()))
        .unwrap();
    assert!(!saved.enabled);
    assert!(saved.spec.requester.is_none());
    assert_eq!(saved.spec.bot, "steve");
}

#[tokio::test]
async fn report_retry_recovers_canonical_result_before_model_or_digest_and_nodes_do_not_collide() {
    use steve_flow::graph::Graph;
    use steve_flow::nodes::{
        Effects, RunCx, action,
        action::{Action, ActionOutcome, Report},
    };
    use steve_flow::{LinearExecutor, RunExecutor};
    let e = env();
    e.env.gateway.script("你是 Steve", "persistent report");
    let s = create(&e, daily("report")).await;
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    // Drive the actual legacy executor; it must use the same node key as GraphEngine.
    assert_eq!(
        LinearExecutor.drive(&e.flow, &ids[0]).await.unwrap(),
        RunStatus::Done
    );
    let mut run = e
        .env
        .store()
        .reader(|c| runs::get(c, &ids[0]))
        .unwrap()
        .unwrap();
    run.cursor = Some("a".into());
    let original = e
        .env
        .store()
        .reader(|c| steve_store::feed::report_result(c, WS, &run.id, "a"))
        .unwrap()
        .unwrap();
    assert!(
        original["feed_entry_id"]
            .as_str()
            .unwrap()
            .starts_with("feed_")
    );
    let before = e.env.gateway.calls.lock().unwrap().len();
    e.env
        .store()
        .transaction(|tx| {
            digest::add(
                tx,
                WS,
                "new material must remain pending",
                None,
                None,
                e.now(),
            )
        })
        .unwrap();
    let output = action::run(
        &e.flow,
        &run,
        &json!({"action":"report","what":"must not run model"}),
    )
    .await;
    let ActionOutcome::Done { output, .. } = output else {
        panic!("retry failed")
    };
    assert_eq!(output, original);
    assert_eq!(e.env.gateway.calls.lock().unwrap().len(), before);
    // The GraphEngine action receives its node explicitly, regardless of the saved run cursor.
    let graph = Graph::from_value(
        &json!({"nodes":[{"id":"b","kind":"action","config":{"action":"report"}}],"edges":[]}),
    )
    .unwrap();
    let mut rcx = RunCx {
        flow: &e.flow,
        run: run.clone(),
        graph: &graph,
        node: &graph.nodes[0],
        dry_run: false,
        sender: Value::Null,
        effects: Effects::default(),
    };
    Report
        .run(
            &mut rcx,
            &json!({"action":"report","what":"second"}),
            &Value::Null,
        )
        .await;
    let second = e
        .env
        .store()
        .reader(|c| steve_store::feed::report_result(c, WS, &run.id, "b"))
        .unwrap()
        .unwrap();
    assert_ne!(second["feed_entry_id"], original["feed_entry_id"]);
    assert_eq!(e.env.gateway.calls.lock().unwrap().len(), before + 1);
    Report
        .run(&mut rcx, &json!({"action":"report"}), &Value::Null)
        .await;
    assert_eq!(e.env.gateway.calls.lock().unwrap().len(), before + 1);
    // A forged workspace fails before even recovering the original output.
    run.workspace_id = WS2.into();
    assert!(matches!(
        action::run(&e.flow, &run, &json!({"action":"report"})).await,
        ActionOutcome::Fail { .. }
    ));
    assert!(e.core.posts().is_empty());
    assert_eq!(s["id"], run.flow_id);
}

#[tokio::test]
async fn report_publication_and_digest_acknowledgement_roll_back_together() {
    use steve_flow::nodes::action::{self, ActionOutcome};
    let e = env();
    e.env.gateway.script("你是 Steve", "report");
    create(&e, daily("report")).await;
    e.env
        .store()
        .transaction(|tx| digest::add(tx, WS, "pending", None, None, e.now()))
        .unwrap();
    e.env.clock.set(monday_8am() + 30 * MIN);
    let ids = claim_due(&e.flow).unwrap();
    let run = e
        .env
        .store()
        .reader(|c| runs::get(c, &ids[0]))
        .unwrap()
        .unwrap();
    e.env.store().with_conn(|c| {c.execute_batch("CREATE TRIGGER fail_digest BEFORE UPDATE ON digest_entries BEGIN SELECT RAISE(ABORT,'injected digest failure'); END;")?;Ok(())}).unwrap();
    assert!(matches!(
        action::run(&e.flow, &run, &json!({"action":"report"})).await,
        ActionOutcome::Retry { .. }
    ));
    assert!(
        e.env
            .store()
            .reader(|c| steve_store::feed::report_result(
                c,
                WS,
                &run.id,
                run.cursor.as_deref().unwrap()
            ))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        e.env
            .store()
            .reader(|c| digest::pending(c, WS, 0))
            .unwrap()
            .len(),
        1
    );
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER fail_digest")?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        action::run(&e.flow, &run, &json!({"action":"report"})).await,
        ActionOutcome::Done { .. }
    ));
    assert!(
        e.env
            .store()
            .reader(|c| digest::pending(c, WS, 0))
            .unwrap()
            .is_empty()
    );
    assert!(e.core.posts().is_empty());
}
