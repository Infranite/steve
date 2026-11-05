//! Conversational configuration: schedules, event
//! sources and flows, each with a fixed model output: preview lines, apply and undo.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use steve_flow::testkit::{FakeEventAdapter, FlowEnv};
use steve_modules::changes::{self, Origin};
use steve_modules::testkit::{ME, WS, call, caller};
use steve_store::base::Change;

fn env() -> FlowEnv {
    let e = FlowEnv::new();
    e.env.clock.set(
        Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0)
            .unwrap()
            .timestamp_millis(),
    );
    e
}

async fn schedule(e: &FlowEnv, id: &str) -> (u16, Value) {
    call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/schedules/{id}"),
        None,
    )
    .await
}

#[tokio::test]
async fn create_from_a_sentence() {
    let e = env();
    let me = caller(WS, ME);
    e.env.gateway.reply(
        r#"好的：{"ops":[{"op":"schedule.create","name":"整理周报","cron":"0 9 * * 5","what":"整理周报","bot":"Steve","mode":"建事项，自动验收","budget_usd":2}]}"#,
    );
    let p = changes::draft(
        &e.env.modules,
        &me,
        "schedule",
        None,
        "每周五 9 点整理周报交给 Steve，自动验收，预算 2 美元",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(p.title, "新建定时任务");
    assert_eq!(
        p.changes,
        vec![
            Change::new("名字", "—", "整理周报"),
            Change::new("时间", "—", "每周五 09:00（0 9 * * 5）"),
            Change::new("做什么", "—", "整理周报"),
            Change::new("交给谁", "—", "Steve"),
            Change::new("方式", "—", "建事项，自动验收"),
            Change::new("错过了怎么办", "—", "补一次"),
            Change::new("上一次没结束", "—", "跳过"),
            Change::new("每次预算", "—", "$2.00"),
        ]
    );
    let prompt = e.env.gateway.last_prompt().unwrap();
    assert!(prompt.contains("schedule.create"), "{prompt}");
    changes::apply(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let (_, list) = call(&e.router(), Some(&me), "GET", "/schedules", None).await;
    let s = &list[0];
    assert_eq!(
        (
            s["name"].as_str(),
            s["cron"].as_str(),
            s["bot"].as_str(),
            s["mode"].as_str()
        ),
        (
            Some("整理周报"),
            Some("0 9 * * 5"),
            Some("steve"),
            Some("item_auto")
        )
    );
    assert_eq!(s["budget_usd"], 2.0);
    assert!(s["next_at"].is_i64());
    let origin: (String, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT origin, origin_label FROM flows WHERE id = ?",
                [s["id"].as_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(origin, ("steve".into(), "Steve 起草，你确认".into()));
    // undo removes it again
    changes::undo(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let (_, list) = call(&e.router(), Some(&me), "GET", "/schedules", None).await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn patch_time_and_missed() {
    let e = env();
    let me = caller(WS, ME);
    let (_, s) = call(&e.router(), Some(&me), "POST", "/schedules", Some(&json!({"name": "离开期间汇报", "when": "每天 08:30", "what": "汇报", "bot": "steve", "mode": "report"}))).await;
    let id = s["id"].as_str().unwrap().to_string();
    let next_before = s["next_at"].as_i64().unwrap();
    e.env.gateway.reply(r#"{"ops":[{"op":"set","key":"when","value":"每天 9 点"},{"op":"set","key":"missed","value":"跳过"},{"op":"set","key":"bot","value":"不存在的"}]}"#);
    let p = changes::draft(
        &e.env.modules,
        &me,
        "schedule",
        Some(&id),
        "离开期间汇报改成 9 点，错过了就算了",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(p.title, "定时任务「离开期间汇报」");
    assert_eq!(
        p.changes,
        vec![
            Change::new("时间", "每天 08:30", "每天 09:00（0 9 * * *）"),
            Change::new("错过了怎么办", "补一次", "跳过"),
            Change::new("交给谁", "—", "（没认出：不存在的）"),
        ]
    );
    let prompt = e.env.gateway.last_prompt().unwrap();
    assert!(
        prompt.contains("when · 时间（中文说法，例如 每天 09:00） · text · \"每天 08:30\""),
        "{prompt}"
    );
    changes::apply(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let (_, v) = schedule(&e, &id).await;
    assert_eq!(
        (
            v["cron"].as_str(),
            v["missed"].as_str(),
            v["when"].as_str(),
            v["bot"].as_str()
        ),
        (
            Some("0 9 * * *"),
            Some("skip"),
            Some("每天 9 点"),
            Some("steve")
        )
    );
    assert_eq!(v["next_at"], next_before + 30 * 60_000);
    changes::undo(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let (_, v) = schedule(&e, &id).await;
    assert_eq!(
        (v["cron"].as_str(), v["missed"].as_str(), v["when"].as_str()),
        (
            Some("30 8 * * *"),
            Some("catch_up_once"),
            Some("每天 08:30")
        )
    );
    assert_eq!(v["next_at"], next_before);
}

#[tokio::test]
async fn unknown_time_stays_out_of_the_draft() {
    let e = env();
    let me = caller(WS, ME);
    let (_, s) = call(&e.router(), Some(&me), "POST", "/schedules", Some(&json!({"name": "汇报", "when": "每天 08:30", "what": "汇报", "bot": "steve", "mode": "report"}))).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env
        .gateway
        .reply(r#"{"ops":[{"op":"set","key":"when","value":"看心情"}]}"#);
    let p = changes::draft(
        &e.env.modules,
        &me,
        "schedule",
        Some(&id),
        "改成看心情",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        p.changes,
        vec![Change::new("时间", "—", "（没看懂时间：看心情）")]
    );
    changes::apply(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let (_, v) = schedule(&e, &id).await;
    assert_eq!(v["cron"], "30 8 * * *");
}

#[tokio::test]
async fn stale_draft_and_candidates() {
    let e = env();
    let me = caller(WS, ME);
    let (_, s) = call(&e.router(), Some(&me), "POST", "/schedules", Some(&json!({"name": "汇报", "when": "每天 08:30", "what": "汇报", "bot": "steve", "mode": "report"}))).await;
    let id = s["id"].as_str().unwrap().to_string();
    e.env
        .gateway
        .reply(r#"{"ops":[{"op":"set","key":"enabled","value":"否"}]}"#);
    let p = changes::draft(
        &e.env.modules,
        &me,
        "schedule",
        Some(&id),
        "先停一下",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(p.changes, vec![Change::new("启用", "启用", "停用")]);
    // changed in the meantime → 409
    call(
        &e.router(),
        Some(&me),
        "PATCH",
        &format!("/schedules/{id}"),
        Some(&json!({"what": "汇报一下"})),
    )
    .await;
    let err = changes::apply(&e.env.modules, &me, &p.id)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.status(), 409);
    let c = changes::candidates(&e.env.modules, &me).unwrap();
    let lines: Vec<String> = c.iter().map(|c| c.line()).collect();
    assert_eq!(
        lines,
        vec![
            format!("schedule:{id} — 定时任务 汇报"),
            "schedule:new — 新建定时任务".to_string()
        ]
    );
    // another workspace's schedule is not a target
    let err = changes::draft(
        &e.env.modules,
        &caller("w2", ME),
        "schedule",
        Some(&id),
        "x",
        Origin::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), 404);
}

// ---- source (S9) ----

fn source_env() -> (FlowEnv, String) {
    let e = FlowEnv::with_events(vec![Arc::new(FakeEventAdapter::poll_every(
        std::time::Duration::from_secs(120),
    ))]);
    let con = e.connection(WS, FakeEventAdapter::ID, None, ME);
    (e, con)
}

async fn source(e: &FlowEnv) -> Value {
    let (st, v) = call(&e.router(), Some(&caller(WS, ME)), "GET", "/sources", None).await;
    assert_eq!(st, 200, "{v}");
    v[0].clone()
}

#[tokio::test]
async fn source_pause_triage_types_and_scopes() {
    let (e, con) = source_env();
    let me = caller(WS, ME);
    let (st, _) = call(
        &e.router(),
        Some(&me),
        "POST",
        "/sources/fake_mail/refresh-scopes",
        None,
    )
    .await;
    assert_eq!(st, 200);
    let before = source(&e).await;
    e.env.gateway.reply(format!(
        r#"{{"ops":[{{"op":"set","key":"paused","value":"是"}},{{"op":"set","key":"triage_bot","value":"运维"}},{{"op":"set","key":"type:mail.bulk","value":"开"}},{{"op":"set","key":"type:mail.reply","value":"关"}},{{"op":"set","key":"scope:{con}:IMPORTANT","value":"只记录"}},{{"op":"set","key":"type:mail.nope","value":"开"}},{{"op":"set","key":"triage_bot","value":"小王"}}]}}"#
    ));
    let p = changes::draft(
        &e.env.modules,
        &me,
        "source",
        Some("fake_mail"),
        "邮件先停一下，以后交给运维判断；群发邮件也收，回复不用收，重要文件夹只记录",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(p.title, "事件源「邮件」");
    assert_eq!(p.href, "#/auto/source/fake_mail");
    assert_eq!(
        p.changes,
        vec![
            Change::new("暂停接收", "否", "是"),
            Change::new("先交给谁判断", "主 Bot", "运维"),
            Change::new("接收「群发邮件」", "关", "开"),
            Change::new("接收「收到回复」", "开", "关"),
            Change::new("范围「重要」", "接收", "只记录"),
            Change::new("（没认出）", "—", "type:mail.nope: 开"),
            Change::new("先交给谁判断", "—", "（没认出：小王）"),
        ]
    );
    let prompt = e.env.gateway.last_prompt().unwrap();
    assert!(
        prompt.contains("type:mail.new · 接收「收到新邮件」 · enum · 开|关 · \"开\""),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!("scope:{con}:INBOX · 范围「收件箱」 · enum")),
        "{prompt}"
    );
    changes::apply(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let after = source(&e).await;
    assert_eq!(after["paused"], true);
    assert_eq!(after["triage_bot"], "ops");
    let on = |v: &Value, k: &str| {
        v["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["key"] == k)
            .unwrap()["on"]
            .clone()
    };
    assert_eq!(
        (on(&after, "mail.bulk"), on(&after, "mail.reply")),
        (json!(true), json!(false))
    );
    let mode = |v: &Value, ext: &str| {
        v["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["ext_id"] == ext)
            .unwrap()["mode"]
            .clone()
    };
    assert_eq!(mode(&after, "IMPORTANT"), "record");
    assert_eq!(mode(&after, "INBOX"), "receive");
    // undo puts every setting back
    changes::undo(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let undone = source(&e).await;
    for k in ["paused", "triage_bot", "scopes"] {
        assert_eq!(undone[k], before[k], "{k}");
    }
    assert_eq!(
        (on(&undone, "mail.bulk"), on(&undone, "mail.reply")),
        (json!(false), json!(true))
    );
}

#[tokio::test]
async fn source_stale_unknown_and_candidates() {
    let (e, _) = source_env();
    let me = caller(WS, ME);
    e.env
        .gateway
        .reply(r#"{"ops":[{"op":"set","key":"paused","value":"是"}]}"#);
    let p = changes::draft(
        &e.env.modules,
        &me,
        "source",
        Some("fake_mail"),
        "先停一下",
        Origin::default(),
    )
    .await
    .unwrap();
    // changed on the source page in the meantime → 409
    let (st, _) = call(
        &e.router(),
        Some(&me),
        "PATCH",
        "/sources/fake_mail",
        Some(&json!({"triage_bot": "ops"})),
    )
    .await;
    assert_eq!(st, 200);
    let err = changes::apply(&e.env.modules, &me, &p.id)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.status(), 409);
    // an adapter that is not registered is not a target
    let err = changes::draft(
        &e.env.modules,
        &me,
        "source",
        Some("nope"),
        "x",
        Origin::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), 404);
    let lines: Vec<String> = changes::candidates(&e.env.modules, &me)
        .unwrap()
        .iter()
        .map(|c| c.line())
        .collect();
    assert!(
        lines.contains(&"source:fake_mail — 事件源 邮件".to_string()),
        "{lines:?}"
    );
}

// ---- flow (S9) ----

fn github_flow() -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "GitHub 通知", "config": {"source": "github", "types": ["github.comment", "github.check_failed"]}},
            {"id": "n", "kind": "action", "title": "提醒你", "config": {"action": "notify", "text": "GitHub：{{event.summary}}"}}
        ],
        "edges": [{"from": "t", "to": "n"}]
    })
}

async fn create_flow(e: &FlowEnv, name: &str, graph: Value) -> String {
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/flows",
        Some(&json!({"name": name, "graph": graph})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v["id"].as_str().unwrap().to_string()
}

async fn flow(e: &FlowEnv, id: &str) -> Value {
    let (st, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/flows/{id}"),
        None,
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v
}

#[tokio::test]
async fn flow_github_notifications_ignored_from_now_on() {
    let e = env();
    let me = caller(WS, ME);
    let id = create_flow(&e, "GitHub 通知", github_flow()).await;
    e.env.gateway.reply(
        r#"{"ops":[{"op":"node.remove","id":"n"},{"op":"node.add","after":"t","kind":"action","title":"忽略","config":{"action":"ignore","reason":"GitHub 通知以后都忽略"},"edge_label":""}]}"#,
    );
    let p = changes::draft(
        &e.env.modules,
        &me,
        "flow",
        Some(&id),
        "GitHub 通知以后都忽略",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(p.title, "流程「GitHub 通知」");
    assert_eq!(p.href, format!("#/auto/rule/{id}"));
    assert_eq!(
        p.changes,
        vec![
            Change::new("删除节点", "「提醒你」", "—"),
            Change::new(
                "新增节点",
                "—",
                "「忽略」（action），接在「GitHub 通知」后面"
            ),
        ]
    );
    let prompt = e.env.gateway.last_prompt().unwrap();
    assert!(
        prompt.contains("\"id\":\"n\""),
        "the graph is in the prompt: {prompt}"
    );
    assert!(prompt.contains("node.add · "), "{prompt}");
    changes::apply(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let f = flow(&e, &id).await;
    assert_eq!(f["version"], 2);
    assert_eq!(f["versions"][0]["author"], "steve");
    assert_eq!(f["versions"][0]["note"], "按你说的修改");
    assert_eq!(
        f["graph"]["nodes"][1],
        json!({"id": "action", "kind": "action", "title": "忽略", "note": "", "config": {"action": "ignore", "reason": "GitHub 通知以后都忽略"}})
    );
    assert_eq!(
        f["graph"]["edges"],
        json!([{"from": "t", "to": "action", "label": ""}])
    );
    // a trial run now ends in the ignore action
    let (st, run) = call(
        &e.router(),
        Some(&me),
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(&json!({"sample": {"summary": "PR #12 新评论"}})),
    )
    .await;
    assert_eq!(st, 200, "{run}");
    let path: Vec<&str> = run["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["node_id"].as_str().unwrap())
        .collect();
    assert_eq!(path, ["t", "action"]);
    // undo: one more version with the old graph
    changes::undo(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let f = flow(&e, &id).await;
    assert_eq!(f["version"], 3);
    assert_eq!(f["graph"], github_flow_saved());
    assert_eq!(f["versions"][0]["note"], "撤销对话里的改动（回到 v1）");
}

/// [`github_flow`] as stored (defaults filled in).
fn github_flow_saved() -> Value {
    let mut g = github_flow();
    for n in g["nodes"].as_array_mut().unwrap() {
        n["note"] = json!("");
    }
    g["edges"][0]["label"] = json!("");
    g
}

#[tokio::test]
async fn flow_insert_update_and_patch_fields() {
    let e = env();
    let me = caller(WS, ME);
    let id = create_flow(&e, "GitHub 通知", github_flow()).await;
    e.env.gateway.reply(
        r#"{"ops":[{"op":"node.add","after":"t","kind":"filter","title":"只看检查失败","config":{"rules":[{"label":"失败","when":"event.type == \"github.check_failed\""}],"else_label":"其余"},"edge_label":""},{"op":"node.update","id":"n","set":{"title":"提醒我","config":{"text":"检查失败：{{event.summary}}"}}},{"op":"node.update","id":"zz","set":{"title":"x"}},{"op":"set","key":"enabled","value":"是"},{"op":"set","key":"name","value":"GitHub 检查失败"}]}"#,
    );
    let p = changes::draft(
        &e.env.modules,
        &me,
        "flow",
        Some(&id),
        "只提醒检查失败，标题改成提醒我，然后启用，名字改成 GitHub 检查失败",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        p.changes,
        vec![
            Change::new(
                "新增节点",
                "—",
                "「只看检查失败」（filter），接在「GitHub 通知」后面"
            ),
            Change::new("节点「提醒你」的标题", "提醒你", "提醒我"),
            Change::new(
                "节点「提醒你」的 text",
                "GitHub：{{event.summary}}",
                "检查失败：{{event.summary}}"
            ),
            Change::new("修改节点", "—", "（没认出：zz）"),
            Change::new("启用", "否", "是"),
            Change::new("名字", "GitHub 通知", "GitHub 检查失败"),
        ]
    );
    changes::apply(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let f = flow(&e, &id).await;
    assert_eq!(
        (
            f["version"].as_i64(),
            f["enabled"].as_bool(),
            f["name"].as_str()
        ),
        (Some(2), Some(true), Some("GitHub 检查失败"))
    );
    // the filter sits between the trigger and the old successor
    assert_eq!(
        f["graph"]["edges"],
        json!([{"from": "filter", "to": "n", "label": ""}, {"from": "t", "to": "filter", "label": ""}])
    );
    let triggers: i64 = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM flow_triggers WHERE flow_id = ? AND enabled = 1",
                [&id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(triggers, 1);
    changes::undo(&e.env.modules, &me, &p.id).unwrap().unwrap();
    let f = flow(&e, &id).await;
    assert_eq!(
        (
            f["version"].as_i64(),
            f["enabled"].as_bool(),
            f["name"].as_str()
        ),
        (Some(3), Some(false), Some("GitHub 通知"))
    );
    assert_eq!(f["graph"], github_flow_saved());
}

#[tokio::test]
async fn flow_edits_that_do_not_validate_stay_out() {
    let e = env();
    let me = caller(WS, ME);
    let id = create_flow(&e, "GitHub 通知", github_flow()).await;
    e.env.gateway.reply(
        r#"{"ops":[{"op":"edge.add","from":"n","to":"t","label":""},{"op":"node.remove","id":"t"},{"op":"node.add","after":"n","kind":"nope","title":"x"}]}"#,
    );
    let p = changes::draft(
        &e.env.modules,
        &me,
        "flow",
        Some(&id),
        "乱改",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        p.changes,
        vec![Change::new(
            "流程",
            "—",
            "（改不成：触发节点不能有连进来的线）"
        )]
    );
    let ops: String = e
        .env
        .store()
        .with_conn(|c| {
            Ok(
                c.query_row("SELECT ops FROM changesets WHERE id = ?", [&p.id], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(ops, "[]");
    // the unknown parts alone are lines, and nothing changes
    e.env.gateway.reply(
        r#"{"ops":[{"op":"node.remove","id":"t"},{"op":"node.add","after":"n","kind":"nope","title":"x"},{"op":"edge.remove","from":"t","to":"zz"}]}"#,
    );
    let p = changes::draft(
        &e.env.modules,
        &me,
        "flow",
        Some(&id),
        "乱改",
        Origin::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        p.changes,
        vec![
            Change::new("删除节点", "—", "（没认出：t（触发节点不能删））"),
            Change::new("新增节点", "—", "（没认出：节点种类 nope）"),
            Change::new("删除连线", "—", "（没认出：t → zz）"),
        ]
    );
    changes::apply(&e.env.modules, &me, &p.id).unwrap().unwrap();
    assert_eq!(flow(&e, &id).await["version"], 1);
}

#[tokio::test]
async fn flow_stale_schedules_and_candidates() {
    let e = env();
    let me = caller(WS, ME);
    let id = create_flow(&e, "GitHub 通知", github_flow()).await;
    e.env
        .gateway
        .reply(r#"{"ops":[{"op":"node.update","id":"n","set":{"title":"提醒我"}}]}"#);
    let p = changes::draft(
        &e.env.modules,
        &me,
        "flow",
        Some(&id),
        "改个标题",
        Origin::default(),
    )
    .await
    .unwrap();
    // saved on the canvas in the meantime → 409
    let (st, _) = call(
        &e.router(),
        Some(&me),
        "PUT",
        &format!("/flows/{id}"),
        Some(&json!({"graph": github_flow()})),
    )
    .await;
    assert_eq!(st, 200);
    let err = changes::apply(&e.env.modules, &me, &p.id)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.status(), 409);
    // a schedule is edited through `schedule`, another workspace's flow is not there
    let (_, s) = call(&e.router(), Some(&me), "POST", "/schedules", Some(&json!({"name": "汇报", "when": "每天 08:30", "what": "汇报", "bot": "steve", "mode": "report"}))).await;
    let sid = s["id"].as_str().unwrap();
    let err = changes::draft(
        &e.env.modules,
        &me,
        "flow",
        Some(sid),
        "x",
        Origin::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), 400);
    let err = changes::draft(
        &e.env.modules,
        &caller("w2", ME),
        "flow",
        Some(&id),
        "x",
        Origin::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), 404);
    let lines: Vec<String> = changes::candidates(&e.env.modules, &me)
        .unwrap()
        .iter()
        .map(|c| c.line())
        .collect();
    assert!(
        lines.contains(&format!("flow:{id} — 流程 GitHub 通知")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with(&format!("flow:{sid}"))),
        "{lines:?}"
    );
}
