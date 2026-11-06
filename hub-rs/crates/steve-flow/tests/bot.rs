//! S4 acceptance: with a scripted gateway returning fixed JSON, the bot
//! node's branch and reason reach the event; a gateway timeout retries once 10 minutes later; a
//! drafted graph validates and is stored disabled; `edit-draft` returns a graph without saving.
//! No real model is ever called.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;
use serde_json::{Value, json};
use steve_flow::nodes::bot::{self, BotNode, Guard};
use steve_flow::testkit::FlowEnv;
use steve_flow::{EventSink, RawEvent, Scheduler};
use steve_modules::Tier;
use steve_modules::testkit::{ME, WS, WS2, call, caller};

struct SecretGuard;
impl Guard for SecretGuard {
    fn scan(&self, text: &str) -> Vec<Range<usize>> {
        text.match_indices("sk-live-123456")
            .map(|(i, m)| i..i + m.len())
            .collect()
    }
}

fn env() -> FlowEnv {
    let e = FlowEnv::with(
        Vec::new(),
        vec![Arc::new(BotNode::new(Arc::new(SecretGuard)))],
        Vec::new(),
    );
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE bots SET role = '运维值班', rules = '只看告警，别的都忽略' WHERE id = 'ops'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    e
}

async fn api(e: &FlowEnv, ws: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call(
        &e.router(),
        Some(&caller(ws, ME)),
        method,
        path,
        body.as_ref(),
    )
    .await
}

/// trigger(mail) → bot → notify | ignore.
fn triage(bot_cfg: Value) -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "邮件 · 收到新邮件", "config": {"source": "mail", "types": ["mail.new"]}},
            {"id": "b", "kind": "bot", "title": "邮件 Bot 判断", "config": bot_cfg, "on_fail": "stop"},
            {"id": "n", "kind": "action", "title": "提醒你", "config": {"action": "notify", "text": "{{steps.b.draft}}"}},
            {"id": "i", "kind": "action", "title": "忽略", "config": {"action": "ignore", "reason": "不用管"}}
        ],
        "edges": [
            {"from": "t", "to": "b"},
            {"from": "b", "to": "n", "label": "需要回复"},
            {"from": "b", "to": "i", "label": "其他"}
        ]
    })
}

fn bot_cfg() -> Value {
    json!({"instruction": "判断这封邮件要不要回复", "branches": ["需要回复", "其他"], "fields": ["draft"]})
}

async fn flow(e: &FlowEnv, graph: Value) -> String {
    let (st, v) = api(
        e,
        WS,
        "POST",
        "/flows",
        Some(json!({"name": "邮件分诊", "graph": graph})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (st, v) = api(
        e,
        WS,
        "PATCH",
        &format!("/flows/{id}"),
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    id
}

async fn ingest(e: &FlowEnv, ext: &str, summary: &str, text: &str) -> String {
    let mut ev = RawEvent::new("mail", "mail.new", ext, steve_store::now(), summary);
    ev.scope_ext_id = Some("INBOX".into());
    ev.payload = json!({ "text": text });
    let r = e.flow.ingest(WS, None, ev).await.unwrap();
    r.id().unwrap().to_string()
}

async fn until<F: Fn() -> bool>(f: F) {
    for _ in 0..150 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition never held");
}

fn q<T: rusqlite::types::FromSql>(e: &FlowEnv, sql: &str, p: impl rusqlite::Params) -> T {
    e.env
        .store()
        .with_conn(|c| Ok(c.query_row(sql, p, |r| r.get(0))?))
        .unwrap()
}

fn outcome(e: &FlowEnv, evt: &str) -> String {
    q(e, "SELECT outcome FROM ext_events WHERE id = ?", [evt])
}

fn run_of(e: &FlowEnv, evt: &str) -> String {
    q(
        e,
        "SELECT id FROM flow_runs WHERE event_id = ? AND dry_run = 0",
        [evt],
    )
}

#[tokio::test]
async fn branch_and_reason_reach_the_event() {
    let e = env();
    flow(&e, triage(bot_cfg())).await;
    e.env.gateway.script(
        bot::PROMPT_START,
        r#"{"branch":"需要回复","reason":"对方在等回复，涉及你的日程。","fields":{"draft":"可以，改到 2 点。"}}"#,
    );
    let evt = ingest(
        &e,
        "m1",
        "Sarah：周五能改到 2 点吗？",
        "密钥 sk-live-123456 别外传",
    )
    .await;
    until(|| outcome(&e, &evt) == "need").await;
    let (reason, handled_by): (String, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT reason, handled_by FROM ext_events WHERE id = ?",
                [&evt],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(reason, "对方在等回复，涉及你的日程。");
    assert_eq!(
        handled_by, "steve",
        "no bot_id, no triage bot: the main bot"
    );
    // the notify text came from the extracted field
    let question: String = q(
        &e,
        "SELECT question FROM decisions WHERE subject_type = 'signal' AND subject_id = ?",
        [&evt],
    );
    assert_eq!(question, "可以，改到 2 点。");
    // one fast-tier call; the prompt has the event, the branches, never the secret
    let calls = e.env.gateway.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, Tier::Fast);
    let p = &calls[0].1;
    assert!(p.starts_with("你是「Steve」"), "{p}");
    assert!(p.contains("Sarah：周五能改到 2 点吗？"));
    assert!(p.contains(r#"["需要回复","其他"]"#));
    assert!(!p.contains("sk-live-123456"));
    assert!(p.contains(bot::REDACTED));
    // the step output carries branch, reason and fields
    let run = run_of(&e, &evt);
    let out: String = q(
        &e,
        "SELECT output FROM flow_run_steps WHERE run_id = ? AND node_id = 'b'",
        [&run],
    );
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["branch"], "需要回复");
    assert_eq!(out["draft"], "可以，改到 2 点。");
    assert_eq!(out["bot"], "steve");
}

#[tokio::test]
async fn configured_bot_brings_its_duty_and_rules() {
    let e = env();
    let mut cfg = bot_cfg();
    cfg["bot_id"] = json!("ops");
    flow(&e, triage(cfg)).await;
    e.env
        .gateway
        .reply(r#"{"branch":"其他","reason":"不是告警"}"#);
    let evt = ingest(&e, "m2", "订阅：本周新闻", "新闻").await;
    until(|| outcome(&e, &evt) == "ignored").await;
    let p = e.env.gateway.last_prompt().unwrap();
    assert!(p.starts_with("你是「运维」"), "{p}");
    assert!(p.contains("你的职责：运维值班"));
    assert!(p.contains("只看告警，别的都忽略"));
    assert_eq!(
        q::<String>(&e, "SELECT handled_by FROM ext_events WHERE id = ?", [&evt]),
        "ops"
    );
    // a bot of another workspace cannot be referenced
    let mut cfg = bot_cfg();
    cfg["bot_id"] = json!("w2bot");
    let (st, v) = api(
        &e,
        WS,
        "POST",
        "/flows",
        Some(json!({"name": "x", "graph": triage(cfg)})),
    )
    .await;
    assert_eq!(st, 400);
    assert_eq!(v["error"], "节点「邮件 Bot 判断」：没有这个 Bot");
}

#[tokio::test]
async fn branch_outside_the_list_follows_on_fail() {
    let e = env();
    // stop: the run fails
    flow(&e, triage(bot_cfg())).await;
    e.env.gateway.reply(r#"{"branch":"转给财务","reason":"?"}"#);
    let evt = ingest(&e, "m3", "发票", "").await;
    until(|| outcome(&e, &evt) == "failed").await;
    let err: String = q(&e, "SELECT error FROM flow_runs WHERE event_id = ?", [&evt]);
    assert_eq!(err, "Bot 选的分支「转给财务」不在列表里");
    // branch:其他 takes that edge instead
    let mut g = triage(bot_cfg());
    g["nodes"][1]["on_fail"] = json!("branch:其他");
    g["nodes"][0]["config"]["types"] = json!(["mail.reply"]);
    flow(&e, g).await;
    e.env.gateway.reply("我不知道");
    let mut ev = RawEvent::new("mail", "mail.reply", "m4", steve_store::now(), "回复");
    ev.scope_ext_id = Some("INBOX".into());
    let evt = e.flow.ingest(WS, None, ev).await.unwrap();
    let evt = evt.id().unwrap().to_string();
    until(|| outcome(&e, &evt) == "ignored").await;
}

#[tokio::test]
async fn gateway_timeout_retries_once_ten_minutes_later() {
    let e = env();
    flow(&e, triage(bot_cfg())).await;
    let sched = Scheduler::new(e.flow.clone());
    // first: times out twice → retried once, then failed
    e.env.gateway.fail("timeout");
    e.env.gateway.fail("timeout");
    let evt = ingest(&e, "m5", "Sarah：在吗", "").await;
    let run = {
        until(|| {
            e.env
                .store()
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT count(*) FROM flow_runs WHERE event_id = ? AND status = 'waiting'",
                        [&evt],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .unwrap()
                == 1
        })
        .await;
        run_of(&e, &evt)
    };
    let now = e.flow.now();
    let retry_at: i64 = q(&e, "SELECT retry_at FROM flow_runs WHERE id = ?", [&run]);
    assert_eq!(retry_at, now + 10 * 60 * 1000);
    assert_eq!(outcome(&e, &evt), "pending");
    // not yet due
    e.env.clock.advance(9 * 60 * 1000);
    assert!(sched.round().await.unwrap().is_empty());
    e.env.clock.advance(60 * 1000);
    for h in sched.round().await.unwrap() {
        h.await.unwrap().unwrap();
    }
    let status: String = q(&e, "SELECT status FROM flow_runs WHERE id = ?", [&run]);
    assert_eq!(status, "failed");
    assert_eq!(outcome(&e, &evt), "failed");
    assert_eq!(e.env.gateway.calls.lock().unwrap().len(), 2);
    let label: String = q(
        &e,
        "SELECT outcome_label FROM flow_runs WHERE id = ?",
        [&run],
    );
    assert_eq!(label, "Steve 没有及时给出判断");
    // second: the retry answers → the run goes on
    e.env.gateway.fail("timeout");
    e.env.gateway.reply(r#"{"branch":"其他","reason":"闲聊"}"#);
    let evt = ingest(&e, "m6", "周末愉快", "").await;
    until(|| {
        e.env
            .store()
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM flow_runs WHERE event_id = ? AND retry_at IS NOT NULL",
                    [&evt],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap()
            == 1
    })
    .await;
    e.env.clock.advance(10 * 60 * 1000);
    for h in sched.round().await.unwrap() {
        h.await.unwrap().unwrap();
    }
    assert_eq!(outcome(&e, &evt), "ignored");
    assert_eq!(
        q::<String>(&e, "SELECT reason FROM ext_events WHERE id = ?", [&evt]),
        "不用管",
        "the ignore action's reason comes last"
    );
}

#[tokio::test]
async fn dry_runs_never_call_the_model() {
    let e = env();
    let id = flow(&e, triage(bot_cfg())).await;
    let (st, v) = api(
        &e,
        WS,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({"sample": {"summary": "x"}, "prefer": ["i"]})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let nodes: Vec<&str> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["node_id"].as_str().unwrap())
        .collect();
    assert_eq!(nodes, ["t", "b", "i"]);
    assert!(e.env.gateway.calls.lock().unwrap().is_empty());
}

/// A drafted answer: trigger (source left out) → bot → ignore | item.create.
fn drafted() -> Value {
    json!({
        "name": "GitHub 检查失败分诊",
        "description": "检查失败时判断要不要建事项",
        "graph": {
            "nodes": [
                {"id": "t", "kind": "trigger", "title": "GitHub · 检查失败", "config": {"types": ["github.check_failed"]}},
                {"id": "b", "kind": "bot", "title": "判断要不要处理", "config": {"bot_id": "ops", "instruction": "是不是主分支上的失败", "branches": ["要处理", "不用"]}},
                {"id": "a", "kind": "action", "title": "建事项", "config": {"action": "item.create", "mode": "start"}},
                {"id": "i", "kind": "action", "title": "忽略", "config": {"action": "ignore", "reason": "不是主分支"}}
            ],
            "edges": [
                {"from": "t", "to": "b"}, {"from": "b", "to": "a", "label": "要处理"}, {"from": "b", "to": "i", "label": "不用"}
            ]
        }
    })
}

#[tokio::test]
async fn draft_validates_and_stores_a_disabled_flow() {
    let e = env();
    e.env
        .gateway
        .reply(format!("好的，这是草稿：\n{}", drafted()));
    let (st, v) = api(
        &e,
        WS,
        "POST",
        "/flows/draft",
        Some(json!({"source": "github", "text": "GitHub 检查失败时，主分支的就建事项交给运维"})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["name"], "GitHub 检查失败分诊");
    assert_eq!(v["enabled"], false);
    assert_eq!(v["origin_label"], "Steve 起草，待你确认");
    assert_eq!(v["graph"]["nodes"][0]["config"]["source"], "github");
    assert_eq!(v["version"], 1);
    let origin: String = q(
        &e,
        "SELECT origin FROM flows WHERE id = ?",
        [v["id"].as_str().unwrap()],
    );
    assert_eq!(origin, "steve");
    let enabled: i64 = q(
        &e,
        "SELECT count(*) FROM flow_triggers WHERE flow_id = ? AND enabled = 1",
        [v["id"].as_str().unwrap()],
    );
    assert_eq!(enabled, 0);
    let calls = e.env.gateway.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, Tier::Smart);
    let p = &calls[0].1;
    assert!(p.starts_with("你在为 Steve 起草一条自动化流程"));
    assert!(p.contains("bot {bot_id?, instruction, branches:[label], fields?:[name]}"));
    assert!(p.contains("- ops（运维）运维值班"));
    assert!(!p.contains("w2bot"), "only this workspace's bots");
}

#[tokio::test]
async fn draft_repairs_once_then_refuses() {
    let e = env();
    // a cycle first, then the fixed graph
    let mut bad = drafted();
    bad["graph"]["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from": "a", "to": "b", "label": ""}));
    e.env.gateway.reply(bad.to_string());
    e.env.gateway.reply(drafted().to_string());
    let body = json!({"source": "github", "text": "检查失败就建事项"});
    let (st, v) = api(&e, WS, "POST", "/flows/draft", Some(body.clone())).await;
    assert_eq!(st, 200, "{v}");
    let p = e.env.gateway.last_prompt().unwrap();
    assert!(p.contains("没有通过检查：流程里不能有环"), "{p}");
    // wrong twice: 400, nothing stored
    let n0: i64 = q(&e, "SELECT count(*) FROM flows", []);
    e.env.gateway.reply(bad.to_string());
    e.env.gateway.reply(bad.to_string());
    let (st, v) = api(&e, WS, "POST", "/flows/draft", Some(body.clone())).await;
    assert_eq!(st, 400);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .starts_with("Steve 起草的流程没有通过检查：流程里不能有环"),
        "{v}"
    );
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM flows", []), n0);
    // gateway down: 400
    e.env.gateway.fail("down");
    let (st, v) = api(&e, WS, "POST", "/flows/draft", Some(body)).await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "Steve 现在起草不了，稍后再试")
    );
    // input rules
    let (st, v) = api(
        &e,
        WS,
        "POST",
        "/flows/draft",
        Some(json!({"source": "github", "text": "x"})),
    )
    .await;
    assert_eq!((st, v["error"].as_str().unwrap()), (400, "参数不对：text"));
    let (st, v) = api(
        &e,
        WS,
        "POST",
        "/flows/draft",
        Some(json!({"source": "fax", "text": "收到传真就提醒我"})),
    )
    .await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "参数不对：source")
    );
}

#[tokio::test]
async fn edit_draft_returns_a_graph_without_saving() {
    let e = env();
    let id = flow(&e, triage(bot_cfg())).await;
    let mut edited = triage(bot_cfg());
    edited["nodes"][3]["config"]["reason"] = json!("周末不处理");
    e.env
        .gateway
        .reply(json!({"graph": edited, "note": "忽略的理由改成“周末不处理”"}).to_string());
    let (st, v) = api(
        &e,
        WS,
        "POST",
        &format!("/flows/{id}/edit-draft"),
        Some(json!({"text": "忽略的理由写周末不处理", "graph": triage(bot_cfg())})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["note"], "忽略的理由改成“周末不处理”");
    assert_eq!(v["graph"]["nodes"][3]["config"]["reason"], "周末不处理");
    let p = e.env.gateway.last_prompt().unwrap();
    assert!(p.starts_with("你在修改 Steve 的一条自动化流程「邮件分诊」"));
    assert!(p.contains("忽略的理由写周末不处理"));
    // not saved
    let (version, graph): (i64, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT version, graph FROM flows WHERE id = ?",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(version, 1);
    assert!(graph.contains("不用管"));
    // another workspace: 404; not a graph: 400
    let body = json!({"text": "改一下", "graph": triage(bot_cfg())});
    let (st, _) = api(
        &e,
        WS2,
        "POST",
        &format!("/flows/{id}/edit-draft"),
        Some(body),
    )
    .await;
    assert_eq!(st, 404);
    let (st, v) = api(
        &e,
        WS,
        "POST",
        &format!("/flows/{id}/edit-draft"),
        Some(json!({"text": "改一下", "graph": {"nodes": 3}})),
    )
    .await;
    assert_eq!((st, v["error"].as_str().unwrap()), (400, "图的格式不对"));
}

// ---------- drafts the validator used to refuse (shapes of real model answers) ----------

async fn draft_with(e: &FlowEnv, answer: Value, text: &str) -> Value {
    e.env.gateway.reply(answer.to_string());
    let (st, v) = api(
        e,
        WS,
        "POST",
        "/flows/draft",
        Some(json!({"source": "github", "text": text})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    // repaired on the first answer: no second round with the model
    assert_eq!(e.env.gateway.calls.lock().unwrap().len(), 1);
    v
}

fn node<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["graph"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn draft_renames_reserved_bot_fields() {
    let e = env();
    let answer = json!({
        "name": "acme-web PR 检查失败处理",
        "description": "lint 交给前端修，测试失败先判断是不是偶发",
        "graph": {
            "nodes": [
                {"id": "t", "kind": "trigger", "title": "GitHub · PR 检查失败", "config": {"source": "github", "types": ["github.check_failed"], "scopes": ["acme/acme-web"]}},
                {"id": "kind", "kind": "bot", "title": "判断失败类型", "config": {"instruction": "是 lint/格式问题还是测试失败", "branches": ["lint", "测试失败"]}},
                {"id": "fix", "kind": "action", "title": "交给前端 Bot 修", "config": {"action": "item.create", "mode": "start", "bot": "ops", "title": "修 lint：{{event.summary}}"}},
                {"id": "flaky", "kind": "bot", "title": "判断测试是否偶发", "config": {"bot_id": "ops", "instruction": "看日志判断是不是偶发失败，并给出分支名", "branches": ["偶发", "不是偶发"], "fields": ["branch", "reason"]}},
                {"id": "rerun", "kind": "http", "title": "重跑检查", "config": {"method": "POST", "url": "https://api.github.com/repos/acme/acme-web/actions/runs/{{event.payload.run_id}}/rerun", "effect": true}},
                {"id": "item", "kind": "action", "title": "建事项给前端 Bot", "config": {"action": "item.create", "bot": "ops", "title": "测试失败：{{steps.flaky.branch}}", "goal": "{{steps.flaky.reason}}"}},
                {"id": "tell", "kind": "action", "title": "群里通知我", "config": {"action": "notify", "text": "{{steps.flaky.branch}} 的测试失败了：{{steps.flaky.fields.reason}}"}},
                {"id": "ok", "kind": "approve", "title": "要在 PR 里留言吗", "config": {"question": "在 PR 里留言说明？", "options": ["留言", "不用"]}},
                {"id": "say", "kind": "http", "title": "在 PR 里留言", "config": {"method": "POST", "url": "https://api.github.com/repos/acme/acme-web/issues/{{event.payload.number}}/comments", "effect": true, "body": {"body": "{{steps.flaky.reason}}"}}}
            ],
            "edges": [
                {"from": "t", "to": "kind"},
                {"from": "kind", "to": "fix", "label": "lint"},
                {"from": "kind", "to": "flaky", "label": "测试失败"},
                {"from": "flaky", "to": "rerun", "label": "偶发"},
                {"from": "flaky", "to": "item", "label": "不是偶发"},
                {"from": "item", "to": "tell"},
                {"from": "tell", "to": "ok"},
                {"from": "ok", "to": "say", "label": "留言"}
            ]
        }
    });
    let v = draft_with(
        &e,
        answer,
        "GitHub 上 acme-web 的 PR 检查失败时：如果是 lint 或格式问题，交给前端 Bot 修；如果是测试失败，先让 Bot 判断是不是偶发，偶发就重跑，不是就建事项给前端 Bot，并在群里通知我；需要我批准后才在 PR 里留言。",
    )
    .await;
    assert_eq!(
        node(&v, "flaky")["config"]["fields"],
        json!(["branch_name", "reason_text"])
    );
    assert_eq!(
        node(&v, "item")["config"]["title"],
        "测试失败：{{steps.flaky.branch_name}}"
    );
    assert_eq!(
        node(&v, "item")["config"]["goal"],
        "{{steps.flaky.reason_text}}"
    );
    assert_eq!(
        node(&v, "tell")["config"]["text"],
        "{{steps.flaky.branch_name}} 的测试失败了：{{steps.flaky.fields.reason_text}}"
    );
    assert_eq!(
        node(&v, "say")["config"]["body"]["body"],
        "{{steps.flaky.reason_text}}"
    );
    assert_eq!(
        v["draft_notes"],
        json!([
            "「判断测试是否偶发」：字段名 branch 是保留的，改成了 branch_name",
            "「判断测试是否偶发」：字段名 reason 是保留的，改成了 reason_text"
        ])
    );
    assert!(
        node(&v, "flaky")["note"]
            .as_str()
            .unwrap()
            .starts_with("Steve 起草时改过：字段名 branch 是保留的"),
        "{v}"
    );
    // the stored flow is the repaired one
    let (st, f) = api(
        &e,
        WS,
        "GET",
        &format!("/flows/{}", v["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(st, 200, "{f}");
    assert_eq!(
        node(&f, "flaky")["config"]["fields"],
        json!(["branch_name", "reason_text"])
    );
    // the prompt told the model both constraints
    let p = e.env.gateway.last_prompt().unwrap();
    assert!(p.contains("fields 不能叫 branch、reason"), "{p}");
    assert!(
        p.contains("item.create 的 accept 只能是 auto、inherit"),
        "{p}"
    );
}

#[tokio::test]
async fn draft_maps_an_unknown_acceptance_to_inherit() {
    let e = env();
    let answer = json!({
        "name": "PR 检查失败分诊",
        "description": "偶发的记一笔，真失败的建事项",
        "graph": {
            "nodes": [
                {"id": "t", "kind": "trigger", "title": "GitHub · 检查失败", "config": {"source": "github", "types": ["github.check_failed"]}},
                {"id": "why", "kind": "bot", "title": "看一下失败原因", "config": {"instruction": "偶发还是真的失败", "branches": ["偶发", "真的失败"], "fields": ["summary"]}},
                {"id": "note", "kind": "action", "title": "记一笔", "config": {"action": "digest", "text": "偶发失败：{{event.summary}}"}},
                {"id": "item", "kind": "action", "title": "建事项交给前端 Bot", "config": {"action": "item.create", "mode": "start", "bot": "frontend", "accept": "manual", "title": "{{steps.why.summary}}"}},
                {"id": "ok", "kind": "approve", "title": "等你批准", "config": {"question": "在 PR 里留言说明？", "options": ["留言", "不用"]}},
                {"id": "say", "kind": "http", "title": "在 PR 里留言", "config": {"method": "POST", "url": "https://api.github.com/repos/acme/acme-web/issues/{{event.payload.number}}/comments", "body": {"body": "{{steps.why.reason}}"}}}
            ],
            "edges": [
                {"from": "t", "to": "why"},
                {"from": "why", "to": "note", "label": "偶发"},
                {"from": "why", "to": "item", "label": "真的失败"},
                {"from": "item", "to": "ok"},
                {"from": "ok", "to": "say", "label": "留言"}
            ]
        }
    });
    let v = draft_with(
        &e,
        answer,
        "acme-web 的 PR 检查失败时，让 Bot 看一下失败原因：偶发失败就记一笔；真的失败就建事项交给前端 Bot，并等我批准后在 PR 里留言说明。",
    )
    .await;
    let item = node(&v, "item");
    assert_eq!(item["config"]["accept"], "inherit");
    assert!(item["config"].get("bot").is_none(), "{item}");
    assert_eq!(node(&v, "say")["config"]["effect"], true);
    let notes: Vec<&str> = v["draft_notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert_eq!(
        notes,
        [
            "「建事项交给前端 Bot」：验收方式「manual」不支持，改成了 inherit（沿用 Bot 或项目的验收方式）",
            "「建事项交给前端 Bot」：没有 Bot「frontend」，改成交给默认的 Bot",
            "「在 PR 里留言」：POST 请求会改动外部系统，标成了有副作用（运行前要审批）"
        ]
    );
    assert!(
        item["note"]
            .as_str()
            .unwrap()
            .starts_with("Steve 起草时改过：验收方式「manual」不支持")
    );
}

#[tokio::test]
async fn draft_maps_acceptance_in_a_short_triage() {
    let e = env();
    let answer = json!({
        "name": "PR 检查失败分诊",
        "description": "偶发忽略，真失败建事项",
        "graph": {
            "nodes": [
                {"id": "t", "kind": "trigger", "title": "GitHub · 检查失败", "config": {"types": ["github.check_failed"]}},
                {"id": "b", "kind": "bot", "title": "判断失败类型", "config": {"instruction": "偶发还是真失败", "branches": ["偶发", "真失败"]}},
                {"id": "i", "kind": "action", "title": "忽略", "config": {"action": "ignore", "reason": "偶发"}},
                {"id": "a", "kind": "action", "title": "建事项交给前端 Bot", "config": {"action": "item.create", "bot": "ops", "accept": "review"}}
            ],
            "edges": [
                {"from": "t", "to": "b"}, {"from": "b", "to": "i", "label": "偶发"}, {"from": "b", "to": "a", "label": "真失败"}
            ]
        }
    });
    let v = draft_with(
        &e,
        answer,
        "PR 检查失败时，让 Bot 判断失败类型，偶发的忽略，真失败的建事项交给前端 Bot。",
    )
    .await;
    assert_eq!(node(&v, "a")["config"]["accept"], "inherit");
    assert_eq!(node(&v, "a")["config"]["bot"], "ops");
    assert_eq!(v["draft_notes"].as_array().unwrap().len(), 1);
    assert_eq!(v["graph"]["nodes"][0]["config"]["source"], "github");
}

#[tokio::test]
async fn draft_retry_names_the_node_its_config_and_the_rules() {
    let e = env();
    let mut bad = drafted();
    bad["graph"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "w", "kind": "wait", "title": "等一会", "config": {"mode": "sleep", "delay": "1h"}}));
    bad["graph"]["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from": "i", "to": "w"}));
    e.env.gateway.reply(bad.to_string());
    e.env.gateway.reply(drafted().to_string());
    let (st, v) = api(
        &e,
        WS,
        "POST",
        "/flows/draft",
        Some(json!({"source": "github", "text": "检查失败就建事项"})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["draft_notes"], json!([]));
    let p = e.env.gateway.last_prompt().unwrap();
    assert!(
        p.contains("没有通过检查：节点「等一会」：mode 只能是 delay、until 或 event"),
        "{p}"
    );
    assert!(
        p.contains("- 节点 id「w」（wait，「等一会」）：mode 只能是 delay、until 或 event"),
        "{p}"
    );
    assert!(
        p.contains("它现在的 config：{\"mode\":\"sleep\",\"delay\":\"1h\"}"),
        "{p}"
    );
    assert!(
        p.contains("wait 的规则：mode 只能是 delay、until、event"),
        "{p}"
    );
}

#[tokio::test]
async fn edit_draft_repairs_and_returns_the_notes() {
    let e = env();
    let id = flow(&e, triage(bot_cfg())).await;
    let mut edited = triage(bot_cfg());
    edited["nodes"][1]["config"]["fields"] = json!(["draft", "reason"]);
    e.env
        .gateway
        .reply(json!({"graph": edited, "note": "加了理由字段"}).to_string());
    let (st, v) = api(
        &e,
        WS,
        "POST",
        &format!("/flows/{id}/edit-draft"),
        Some(json!({"text": "再提取一下理由", "graph": triage(bot_cfg())})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["note"], "加了理由字段");
    assert_eq!(
        v["graph"]["nodes"][1]["config"]["fields"],
        json!(["draft", "reason_text"])
    );
    assert_eq!(
        v["draft_notes"],
        json!(["「邮件 Bot 判断」：字段名 reason 是保留的，改成了 reason_text"])
    );
}
