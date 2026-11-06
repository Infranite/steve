//! S2 acceptance: the prototype's r1 (邮件分诊) and r6 (报销单处理) save
//! and dry-run along the prototype's paths; cycles and loose nodes are 400; `approve` → 提交
//! continues; `wait(event)` wakes on an event; a restart at an effectful node fails the run with a
//! decision; `item.create` links the item `from` the event; an outward action without approval stops
//! at a decision. `bot`, `script`, `http` are stand-ins (S3/S4 own them).

use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;
use serde_json::{Value, json};
use steve_flow::nodes::Grant;
use steve_flow::testkit::{CoreCall, FakeSend, FlowEnv, StubNode};
use steve_flow::{EventSink, RawEvent, engine, scheduler};
use steve_modules::testkit::{ME, WS, WS2, call, caller};

struct Env {
    e: FlowEnv,
    bot: Arc<StubNode>,
    http: Arc<StubNode>,
    reply: Arc<FakeSend>,
}

fn env() -> Env {
    let bot = Arc::new(StubNode::new("bot", json!({ "reason": "对方在等回复" })));
    let script = Arc::new(StubNode::new("script", json!({})).from_path("event.payload.invoice"));
    let http = Arc::new(
        StubNode::new(
            "http",
            json!({ "claimId": "CL-20261031-0042", "status": "submitted" }),
        )
        .effect(Some(Grant::new(
            "http.effect",
            "host",
            "finance.example.com",
        ))),
    );
    let reply = Arc::new(FakeSend::new("reply_as"));
    let e = FlowEnv::with(
        Vec::new(),
        vec![bot.clone(), script, http.clone()],
        vec![reply.clone()],
    );
    Env {
        e,
        bot,
        http,
        reply,
    }
}

async fn api(e: &FlowEnv, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call(
        &e.router(),
        Some(&caller(WS, ME)),
        method,
        path,
        body.as_ref(),
    )
    .await
}

/// r1 邮件分诊 of the prototype (auto-data.ts), with real configs.
fn r1() -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "邮件 · 收到新邮件", "note": "两个账号 · 收件箱、重要",
             "config": {"source": "mail", "types": ["mail.new"], "scopes": ["INBOX", "IMPORTANT"]}},
            {"id": "f", "kind": "filter", "title": "排除通知类发件人", "note": "noreply、GitHub、订阅",
             "config": {"rules": [{"label": "通知类", "when": "event.payload.from matches \"(?i)noreply|no-reply|notifications@github\" or event.payload.list_unsubscribe"}], "else_label": "其余"}},
            {"id": "b", "kind": "bot", "title": "邮件 Bot 判断", "note": "需要回复 / 有截止或涉及钱 / 其他",
             "config": {"instruction": "判断这封邮件要怎么处理", "branches": ["需要回复", "有截止或涉及钱", "其他"]}},
            {"id": "a1", "kind": "action", "title": "起草回复，等你确认", "note": "进待处理 · 审批",
             "config": {"action": "reply_as", "text": "{{steps.b.draft}}"}},
            {"id": "a2", "kind": "action", "title": "建事项", "note": "先给方案",
             "config": {"action": "item.create", "mode": "propose"}},
            {"id": "a3", "kind": "action", "title": "进汇报", "note": "离开期间汇报",
             "config": {"action": "digest", "text": "{{event.summary}}"}},
            {"id": "a4", "kind": "action", "title": "忽略", "note": "记录原因",
             "config": {"action": "ignore", "reason": "通知类邮件"}}
        ],
        "edges": [
            {"from": "t", "to": "f", "label": ""}, {"from": "f", "to": "b", "label": "其余"}, {"from": "f", "to": "a4", "label": "通知类"},
            {"from": "b", "to": "a1", "label": "需要回复"}, {"from": "b", "to": "a2", "label": "有截止或涉及钱"}, {"from": "b", "to": "a3", "label": "其他"}
        ]
    })
}

/// r6 报销单处理 of the prototype.
fn r6() -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "邮件 · 带附件", "note": "收件箱 · 附件是 PDF 或 OFD",
             "config": {"source": "mail", "types": ["mail.attachment"]}},
            {"id": "p", "kind": "script", "title": "解析发票",
             "config": {"code": "export const handler = async (input) => input.attachments.find(a => a.type === 'invoice')"}},
            {"id": "tb", "kind": "table", "title": "按金额和类别分级",
             "config": {"hit": "first", "inputs": ["amount", "category"], "outputs": ["level"],
                        "rows": [["<= 500", "\"交通\", \"餐饮\"", "\"auto\""], ["<= 5000", "", "\"approve\""], ["", "", "\"manual\""]]}},
            {"id": "h1", "kind": "http", "title": "提交到报销系统",
             "config": {"method": "POST", "url": "https://finance.example.com/api/claims", "effect": true,
                        "body": "{ \"amount\": {{steps.p.amount}}, \"category\": \"{{steps.p.category}}\", \"invoice\": \"{{event.payload.attachments[0].url}}\" }"}},
            {"id": "ap", "kind": "approve", "title": "等你审批",
             "config": {"question": "这张发票超过 500 元（{{steps.p.amount}} 元），要提交吗？", "options": ["提交", "不提交"]}},
            {"id": "w", "kind": "wait", "title": "等报销系统回调",
             "config": {"mode": "event", "match": {"source": "webhook", "type": "claim.paid", "when": "event.payload.claim == steps.h1.claimId"}, "timeout": "3d", "timeout_label": "超时"}},
            {"id": "a1", "kind": "action", "title": "进汇报", "note": "报销已到账", "config": {"action": "digest", "text": "报销已到账：{{steps.p.seller}} {{steps.p.amount}} 元"}},
            {"id": "a2", "kind": "action", "title": "提醒你", "note": "超过 5000 元，走线下流程", "config": {"action": "notify", "text": "超过 5000 元，走线下流程"}}
        ],
        "edges": [
            {"from": "t", "to": "p"}, {"from": "p", "to": "tb"}, {"from": "tb", "to": "h1", "label": "auto"}, {"from": "tb", "to": "ap", "label": "approve"},
            {"from": "tb", "to": "a2", "label": "manual"}, {"from": "ap", "to": "h1", "label": "提交"}, {"from": "h1", "to": "w"}, {"from": "w", "to": "a1", "label": "已到账"}
        ]
    })
}

fn invoice(amount: f64, category: &str) -> Value {
    json!({ "amount": amount, "category": category, "seller": "滴滴出行", "date": "2026-10-30" })
}

async fn create_flow(e: &FlowEnv, name: &str, graph: Value) -> String {
    let (st, v) = api(
        e,
        "POST",
        "/flows",
        Some(json!({ "name": name, "graph": graph })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v["id"].as_str().unwrap().to_string()
}

async fn enable(e: &FlowEnv, id: &str) {
    let (st, v) = api(
        e,
        "PATCH",
        &format!("/flows/{id}"),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
}

/// The nodes a run went through, in order (a node that waited and then went on counts once).
fn path_of(run: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in run["steps"].as_array().unwrap() {
        let n = s["node_id"].as_str().unwrap().to_string();
        if out.last() != Some(&n) {
            out.push(n);
        }
    }
    out
}

fn mail(id: &str, summary: &str, payload: Value) -> RawEvent {
    let mut ev = RawEvent::new("mail", "mail.new", id, steve_store::now(), summary);
    ev.scope_ext_id = Some("INBOX".into());
    ev.payload = payload;
    ev
}

async fn until<F: Fn() -> bool>(f: F) {
    for _ in 0..100 {
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

fn run_status(e: &FlowEnv, id: &str) -> String {
    q(e, "SELECT status FROM flow_runs WHERE id = ?", [id])
}

fn open_decision(e: &FlowEnv, subject_type: &str) -> Option<String> {
    e.env
        .store()
        .with_conn(|c| {
            Ok(rusqlite::OptionalExtension::optional(c.query_row(
                "SELECT id FROM decisions WHERE subject_type = ? AND status = 'open' ORDER BY created DESC, rowid DESC LIMIT 1",
                [subject_type],
                |r| r.get::<_, String>(0),
            ))?)
        })
        .unwrap()
}

async fn answer(e: &FlowEnv, id: &str, choice: &str) {
    steve_modules::decisions::dispatch(&e.env.modules, &caller(WS, ME), id, choice, "")
        .expect("ours")
        .expect("resolved");
    e.flow.run_followups().await;
}

fn event_outcome(e: &FlowEnv, id: &str) -> (String, String) {
    e.env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT outcome, outcome_label FROM ext_events WHERE id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap()
}

#[tokio::test]
async fn r1_saves_and_dry_runs_like_the_prototype() {
    let Env { e, reply, .. } = env();
    let id = create_flow(&e, "邮件分诊", r1()).await;
    // the prototype is at v3: two more saves
    for note in ["", "改了过滤"] {
        let (st, v) = api(
            &e,
            "PUT",
            &format!("/flows/{id}"),
            Some(json!({ "graph": r1(), "note": note })),
        )
        .await;
        assert_eq!(st, 200, "{v}");
    }
    let (_, flow) = api(&e, "GET", &format!("/flows/{id}"), None).await;
    assert_eq!(flow["version"], 3);
    assert_eq!(flow["versions"].as_array().unwrap().len(), 3);
    assert_eq!(
        (flow["uses_ai"].as_bool(), flow["outcomes"].as_i64()),
        (Some(true), Some(4))
    );
    assert_eq!(
        flow["trigger"],
        json!({"source": "mail", "types": ["mail.new"]})
    );
    assert_eq!(flow["enabled"], false);

    // no sample, no prefer: the prototype walks the first edge everywhere → t f b a1
    let sample = json!({ "summary": "Sarah（Acme）：周五评审能改到下午 2 点吗？", "payload": { "from": "sarah@acme.com" } });
    let (st, run) = api(
        &e,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({ "sample": sample })),
    )
    .await;
    assert_eq!(st, 200, "{run}");
    assert_eq!(run["dry_run"], true);
    assert_eq!(path_of(&run), ["t", "f", "b", "a1"]);
    assert_eq!(
        run["steps"][3]["output"]["skipped"],
        "对外动作，试运行时不执行"
    );
    assert_eq!(reply.sent(), 0, "nothing goes out in a dry run");
    // a notification sender goes the 通知类 branch
    let sample = json!({ "summary": "GitHub：PR #12 新评论", "payload": { "from": "notifications@github.com" } });
    let (_, run) = api(
        &e,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({ "sample": sample })),
    )
    .await;
    assert_eq!(path_of(&run), ["t", "f", "a4"]);
    // prefer picks the bot's branch
    let (_, run) = api(
        &e,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({ "sample": {"payload": {"from": "bill@cloud.com"}}, "prefer": ["a3"] })),
    )
    .await;
    assert_eq!(path_of(&run), ["t", "f", "b", "a3"]);
    // dry runs leave no items, decisions or digest entries
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM decisions", []), 0);
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM digest_entries", []), 0);
    assert!(e.core.created().is_empty());
}

#[tokio::test]
async fn r6_saves_and_dry_runs_like_the_prototype() {
    let Env { e, http, .. } = env();
    let id = create_flow(&e, "报销单处理", r6()).await;
    // trial: sample 滴滴出行电子发票 ¥86.50, prefer ["tb", "h1"] → t p tb h1 w a1
    let body = json!({
        "sample": { "summary": "滴滴出行电子发票 ¥86.50", "payload": { "invoice": invoice(86.5, "交通"), "attachments": [{"url": "https://x/inv.pdf"}] } },
        "prefer": ["tb", "h1"]
    });
    let (st, run) = api(&e, "POST", &format!("/flows/{id}/dry-run"), Some(body)).await;
    assert_eq!(st, 200, "{run}");
    assert_eq!(path_of(&run), ["t", "p", "tb", "h1", "w", "a1"]);
    let steps = run["steps"].as_array().unwrap();
    assert_eq!(steps[1]["output"]["amount"], 86.5);
    assert_eq!(steps[2]["output"], json!({"level": "auto"}));
    assert_eq!(steps[2]["branch"], "auto");
    assert_eq!(steps[3]["output"]["skipped"], "对外动作，试运行时不执行");
    assert_eq!(http.runs(), 0);
    // bigger amounts go to approval or the manual path
    let (_, run) = api(
        &e,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({ "sample": { "payload": { "invoice": invoice(1200.0, "住宿") } } })),
    )
    .await;
    assert_eq!(path_of(&run), ["t", "p", "tb", "ap", "h1", "w", "a1"]);
    let (_, run) = api(
        &e,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({ "sample": { "payload": { "invoice": invoice(8000.0, "设备") } } })),
    )
    .await;
    assert_eq!(path_of(&run), ["t", "p", "tb", "a2"]);
    // the run is listed with the flow, as a dry run
    let (_, runs) = api(&e, "GET", &format!("/flows/{id}/runs"), None).await;
    assert_eq!(runs.as_array().unwrap().len(), 3);
    assert!(
        runs.as_array()
            .unwrap()
            .iter()
            .all(|r| r["dry_run"] == true)
    );
}

#[tokio::test]
async fn invalid_graphs_are_400() {
    let Env { e, .. } = env();
    let id = create_flow(&e, "x", r1()).await;
    let put = |g: Value| {
        let e = &e;
        let id = id.clone();
        async move {
            api(
                e,
                "PUT",
                &format!("/flows/{id}"),
                Some(json!({ "graph": g })),
            )
            .await
        }
    };
    let mut g = r1();
    g["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from": "b", "to": "f", "label": "再看看"}));
    let (st, v) = put(g).await;
    assert_eq!(st, 400);
    assert!(
        v["error"].as_str().unwrap().starts_with("流程里不能有环"),
        "{v}"
    );

    let mut g = r1();
    g["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "lost", "kind": "action", "config": {"action": "ignore"}}));
    let (st, v) = put(g).await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "有 1 个节点没有连上")
    );

    let mut g = r1();
    g["nodes"][1]["kind"] = json!("trigger");
    let (st, v) = put(g).await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "流程要有且只有一个触发节点")
    );

    let mut g = r1();
    g["nodes"][1]["config"]["rules"][0]["when"] = json!("event.payload.from ==");
    let (st, v) = put(g).await;
    assert_eq!(st, 400);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .starts_with("节点「排除通知类发件人」：第 1 条条件"),
        "{v}"
    );

    let mut g = r1();
    g["nodes"][2]["kind"] = json!("teleport");
    let (st, v) = put(g).await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "不认识的节点种类：teleport")
    );

    let mut g = r1();
    g["nodes"][2]["id"] = json!("Bad-Id");
    let (st, _) = put(g).await;
    assert_eq!(st, 400);

    let mut g = r1();
    g["edges"][0]["to"] = json!("nowhere");
    let (st, v) = put(g).await;
    assert_eq!(
        (st, v["error"].as_str().unwrap()),
        (400, "连线指向不存在的节点「nowhere」")
    );

    // nothing was saved
    let (_, flow) = api(&e, "GET", &format!("/flows/{id}"), None).await;
    assert_eq!(flow["version"], 1);
    let (st, _) = api(
        &e,
        "POST",
        "/flows",
        Some(json!({ "name": "", "graph": r1() })),
    )
    .await;
    assert_eq!(st, 400);
}

#[tokio::test]
async fn versions_triggers_rollback_and_ownership() {
    let Env { e, .. } = env();
    let id = create_flow(&e, "邮件分诊", r1()).await;
    let trig = |e: &FlowEnv| -> (String, bool) {
        e.env
            .store()
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT types, enabled FROM flow_triggers WHERE flow_id = ?",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap()
    };
    assert_eq!(trig(&e), ("[\"mail.new\"]".into(), false));
    enable(&e, &id).await;
    assert!(trig(&e).1);
    let mut g = r1();
    g["nodes"][0]["config"]["types"] = json!(["mail.new", "mail.reply"]);
    api(
        &e,
        "PUT",
        &format!("/flows/{id}"),
        Some(json!({ "graph": g })),
    )
    .await;
    assert_eq!(trig(&e), ("[\"mail.new\",\"mail.reply\"]".into(), true));
    let (st, flow) = api(
        &e,
        "POST",
        &format!("/flows/{id}/rollback"),
        Some(json!({ "version": 1 })),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(flow["version"], 3);
    assert_eq!(flow["versions"][0]["note"], "回到 v1");
    assert_eq!(trig(&e).0, "[\"mail.new\"]");
    // disabling is not a version
    api(
        &e,
        "PATCH",
        &format!("/flows/{id}"),
        Some(json!({ "enabled": false })),
    )
    .await;
    let (_, flow) = api(&e, "GET", &format!("/flows/{id}"), None).await;
    assert_eq!((flow["version"].as_i64(), trig(&e).1), (Some(3), false));
    // revisions are written
    assert_eq!(
        q::<i64>(
            &e,
            "SELECT count(*) FROM revisions WHERE object_type = 'flow' AND object_id = ?",
            [&id]
        ),
        2
    );
    // duplicate
    let (_, dup) = api(&e, "POST", &format!("/flows/{id}/duplicate"), None).await;
    assert_eq!(
        (dup["name"].as_str(), dup["enabled"].as_bool()),
        (Some("邮件分诊 副本"), Some(false))
    );
    // other workspace: 404
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/flows/{id}"),
        None,
    )
    .await;
    assert_eq!(st, 404);
    // list by kind
    let (_, list) = api(&e, "GET", "/flows?kind=flow", None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    // delete drops the triggers
    let (st, _) = api(&e, "DELETE", &format!("/flows/{id}"), None).await;
    assert_eq!(st, 200);
    assert_eq!(
        q::<i64>(
            &e,
            "SELECT count(*) FROM flow_triggers WHERE flow_id = ?",
            [&id]
        ),
        0
    );
    let (st, _) = api(&e, "GET", &format!("/flows/{id}"), None).await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn r1_real_runs_ignore_and_create_items_linked_from_the_event() {
    let Env { e, bot, .. } = env();
    let id = create_flow(&e, "邮件分诊", r1()).await;
    enable(&e, &id).await;
    // a notification is ignored
    let evt = e
        .flow
        .ingest(
            WS,
            None,
            mail(
                "m1",
                "GitHub：PR #12 新评论",
                json!({"from": "notifications@github.com"}),
            ),
        )
        .await
        .unwrap();
    let evt = evt.id().unwrap().to_string();
    until(|| event_outcome(&e, &evt).0 == "ignored").await;
    assert_eq!(
        q::<String>(&e, "SELECT reason FROM ext_events WHERE id = ?", [&evt]),
        "通知类邮件"
    );
    // money and deadlines: an item, proposed, linked from the event
    bot.set_branch(Some("有截止或涉及钱"));
    let evt = e
        .flow
        .ingest(
            WS,
            None,
            mail(
                "m2",
                "云服务 10 月账单 ¥312，11-05 前付款",
                json!({"from": "billing@cloud.com", "text": "请在 11-05 前付款"}),
            ),
        )
        .await
        .unwrap();
    let evt = evt.id().unwrap().to_string();
    until(|| event_outcome(&e, &evt).0 == "item").await;
    let req = &e.core.created()[0];
    assert!(req.propose);
    assert_eq!(req.source, ("邮件".to_string(), "sig".to_string()));
    assert_eq!(req.bot, "steve");
    assert!(req.goal.contains("请在 11-05 前付款"));
    let item = req_item(&e, &evt);
    assert_eq!(item, "ST-1");
    assert_eq!(event_outcome(&e, &evt).1, "提了方案 ST-1");
    // a reply needs your approval: the outward reply waits at a decision
    bot.set_branch(Some("需要回复"));
    let evt = e
        .flow
        .ingest(
            WS,
            None,
            mail(
                "m3",
                "Sarah：周五能改到 2 点吗？",
                json!({"from": "sarah@acme.com"}),
            ),
        )
        .await
        .unwrap();
    let evt = evt.id().unwrap().to_string();
    until(|| event_outcome(&e, &evt).0 == "need").await;
    assert!(open_decision(&e, "flow_step").is_some());
}

/// The item linked `from` an event.
fn req_item(e: &FlowEnv, evt: &str) -> String {
    q(
        e,
        "SELECT subject_id FROM links WHERE subject_type = 'item' AND predicate = 'from' AND object_type = 'event' AND object_id = ?",
        [evt],
    )
}

#[tokio::test]
async fn r6_approve_submit_then_wait_event_wakes_the_run() {
    let Env { e, http, .. } = env();
    let id = create_flow(&e, "报销单处理", r6()).await;
    enable(&e, &id).await;
    let mut ev = RawEvent::new(
        "mail",
        "mail.attachment",
        "inv1",
        steve_store::now(),
        "酒店发票 ¥1200",
    );
    ev.payload = json!({ "invoice": invoice(1200.0, "住宿") });
    let evt = e
        .flow
        .ingest(WS, None, ev)
        .await
        .unwrap()
        .id()
        .unwrap()
        .to_string();
    until(|| event_outcome(&e, &evt).0 == "need").await;
    let run: String = q(&e, "SELECT id FROM flow_runs WHERE event_id = ?", [&evt]);
    assert_eq!(run_status(&e, &run), "waiting");
    let dec = open_decision(&e, "flow_step").expect("approval");
    let (kind, subject, question, options): (String, String, String, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT kind, subject_id, question, options FROM decisions WHERE id = ?",
                [&dec],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!((kind.as_str(), subject), ("approve", format!("{run}:ap")));
    assert_eq!(question, "这张发票超过 500 元（1200 元），要提交吗？");
    assert_eq!(options, "[\"提交\",\"不提交\"]");
    assert_eq!(
        event_outcome(&e, &evt).1,
        "等你审批：这张发票超过 500 元（1200 元），要提交吗？"
    );
    assert_eq!(http.runs(), 0);

    // 提交: the run goes on, the effect runs (you approved it), and waits for the callback
    answer(&e, &dec, "提交").await;
    assert_eq!(http.runs(), 1);
    assert_eq!(run_status(&e, &run), "waiting");
    let mode: String = q(&e, "SELECT mode FROM flow_waits WHERE run_id = ?", [&run]);
    assert_eq!(mode, "event");
    assert_eq!(
        event_outcome(&e, &evt).0,
        "pending",
        "the approval label went with the answer"
    );

    // an unrelated callback does not wake it (match.when)
    let mut cb = RawEvent::new(
        "webhook",
        "claim.paid",
        "cb0",
        steve_store::now(),
        "到账 CL-OTHER",
    );
    cb.payload = json!({ "claim": "CL-OTHER" });
    e.flow.ingest(WS, None, cb).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(run_status(&e, &run), "waiting");
    // the right one does
    let mut cb = RawEvent::new(
        "webhook",
        "claim.paid",
        "cb1",
        steve_store::now(),
        "到账 CL-20261031-0042",
    );
    cb.payload = json!({ "claim": "CL-20261031-0042" });
    e.flow.ingest(WS, None, cb).await.unwrap();
    until(|| run_status(&e, &run) == "done").await;
    let (_, v) = api(&e, "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(path_of(&v), ["t", "p", "tb", "ap", "h1", "w", "a1"]);
    let text: String = q(
        &e,
        "SELECT text FROM digest_entries WHERE run_id = ?",
        [&run],
    );
    assert_eq!(text, "报销已到账：滴滴出行 1200 元");
    until(|| event_outcome(&e, &evt).0 == "digest").await;
}

#[tokio::test]
async fn outward_action_without_approval_stops_at_a_decision() {
    let Env { e, reply, http, .. } = env();
    let g = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "manual"}},
            {"id": "s", "kind": "action", "title": "回复对方", "config": {"action": "reply_as", "text": "好的，{{input.who}}"}}
        ],
        "edges": [{"from": "t", "to": "s"}]
    });
    let id = create_flow(&e, "直接回复", g).await;
    let (st, v) = api(
        &e,
        "POST",
        &format!("/flows/{id}/run"),
        Some(json!({ "input": { "who": "Sarah" } })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let run = v["run_id"].as_str().unwrap().to_string();
    until(|| run_status(&e, &run) == "waiting").await;
    assert_eq!(reply.sent(), 0, "nothing was sent");
    let dec = open_decision(&e, "flow_step").expect("an approval");
    let (kind, question): (String, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT kind, question FROM decisions WHERE id = ?",
                [&dec],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(kind, "approve");
    assert_eq!(
        question,
        "「直接回复」要执行「回复对方」，这是对外动作。要执行吗？"
    );
    // a decision cannot be answered twice; 不执行 ends the run without sending
    answer(&e, &dec, "不执行").await;
    assert_eq!(run_status(&e, &run), "done");
    assert_eq!(reply.sent(), 0);
    assert_eq!(
        q::<String>(
            &e,
            "SELECT outcome_label FROM flow_runs WHERE id = ?",
            [&run]
        ),
        "你没有批准「回复对方」，没有执行"
    );
    let again =
        steve_modules::decisions::dispatch(&e.env.modules, &caller(WS, ME), &dec, "执行", "")
            .unwrap();
    assert!(again.is_err());

    // 执行 sends exactly once
    let (_, v) = api(
        &e,
        "POST",
        &format!("/flows/{id}/run"),
        Some(json!({ "input": { "who": "Sarah" } })),
    )
    .await;
    let run = v["run_id"].as_str().unwrap().to_string();
    until(|| run_status(&e, &run) == "waiting").await;
    let dec = open_decision(&e, "flow_step").unwrap();
    answer(&e, &dec, "执行").await;
    assert_eq!(run_status(&e, &run), "done");
    assert_eq!(reply.sent(), 1);
    assert_eq!(reply.sent.lock().unwrap()[0]["text"], "好的，Sarah");

    // with a grant for the host, an effectful http node runs without asking
    let g = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "manual"}},
            {"id": "h", "kind": "http", "config": {"method": "POST", "url": "https://finance.example.com/api/claims", "effect": true}}
        ],
        "edges": [{"from": "t", "to": "h"}]
    });
    let id = create_flow(&e, "提交", g).await;
    let (_, v) = api(&e, "POST", &format!("/flows/{id}/run"), None).await;
    let run = v["run_id"].as_str().unwrap().to_string();
    until(|| run_status(&e, &run) == "waiting").await;
    assert_eq!(http.runs(), 0);
    e.env.store().with_conn(|c| Ok(c.execute(
        "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id, scope, until, created_by, created) VALUES ('gr_1', 'default', 'flow', ?, 'http.effect', 'host', 'finance.example.com', json_object('flow_version', 1), ?, 'usr_dev', 1)",
        params![id, i64::MAX])?)).unwrap();
    let (_, v) = api(&e, "POST", &format!("/flows/{id}/run"), None).await;
    let run = v["run_id"].as_str().unwrap().to_string();
    until(|| run_status(&e, &run) == "done").await;
    assert_eq!(http.runs(), 1);
}

#[tokio::test]
async fn restart_at_an_effectful_node_fails_with_a_decision() {
    let Env { e, reply, .. } = env();
    let g = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "mail", "types": ["mail.new"]}},
            {"id": "f", "kind": "filter", "config": {"rules": [], "else_label": ""}},
            {"id": "s", "kind": "action", "title": "回复对方", "config": {"action": "reply_as", "text": "收到"}}
        ],
        "edges": [{"from": "t", "to": "f"}, {"from": "f", "to": "s"}]
    });
    let id = create_flow(&e, "自动回复", g).await;
    // two runs left `running` by a crash: one at the effect (after you approved it), one before
    let (stuck, safe) = e.env.store().transaction(|tx| -> steve_flow::FlowResult<(String, String)> {
        tx.execute("INSERT INTO ext_events (id, workspace_id, source, type, dedupe_key, occurred, received, summary) VALUES ('evt_x', 'default', 'mail', 'mail.new', 'x', 1, 1, '你好')", [])?;
        let f = steve_flow::flows::own(tx, WS, &id)?;
        let a = engine::open_run(tx, &f, "event", json!({"event": {"id": "evt_x"}}), Some("evt_x"), false, None, 0, 1)?;
        let b = engine::open_run(tx, &f, "event", json!({"event": {"id": "evt_x"}}), Some("evt_x"), false, None, 0, 1)?;
        tx.execute("UPDATE flow_runs SET cursor = 's', context = json_set(context, '$.approved.s', json('true')) WHERE id = ?", [&a])?;
        tx.execute("UPDATE flow_runs SET cursor = 'f' WHERE id = ?", [&b])?;
        Ok((a, b))
    }).unwrap();
    let go = engine::recover(&e.flow).unwrap();
    assert_eq!(go, vec![safe.clone()]);
    let (status, error, outcome): (String, String, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status, error, outcome FROM flow_runs WHERE id = ?",
                [&stuck],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (status.as_str(), error.as_str(), outcome.as_str()),
        ("failed", "Hub 重启时这一步可能没有完成", "need")
    );
    assert_eq!(reply.sent(), 0, "not replayed");
    assert_eq!(event_outcome(&e, "evt_x").0, "need");
    let dec = open_decision(&e, "flow_run").expect("fail decision");
    let (kind, subject, options): (String, String, String) = e
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT kind, subject_id, options FROM decisions WHERE id = ?",
                [&dec],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (kind.as_str(), subject, options.as_str()),
        ("fail", stuck.clone(), "[\"retry\",\"drop\"]")
    );
    // retry: your answer approves the step, it runs once
    answer(&e, &dec, "retry").await;
    assert_eq!(run_status(&e, &stuck), "done");
    assert_eq!(reply.sent(), 1);
    // the safe one continues and, unapproved, stops at a decision
    scheduler::drive(&e.flow, &safe).await.unwrap();
    assert_eq!(run_status(&e, &safe), "waiting");
    assert_eq!(reply.sent(), 1);
}

#[tokio::test]
async fn item_link_by_ref_supplements_and_falls_back() {
    let Env { e, .. } = env();
    let g = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "github", "types": ["github.check_failed"]}},
            {"id": "l", "kind": "action", "config": {"action": "item.link", "by": "ref", "fallback": "create"}}
        ],
        "edges": [{"from": "t", "to": "l"}]
    });
    let id = create_flow(&e, "检查失败", g).await;
    enable(&e, &id).await;
    let ev =
        |n: &str, s: &str| RawEvent::new("github", "github.check_failed", n, steve_store::now(), s);
    // nothing to link to: a new item
    let a = e
        .flow
        .ingest(WS, None, ev("c1", "PR #12 检查失败"))
        .await
        .unwrap()
        .id()
        .unwrap()
        .to_string();
    until(|| event_outcome(&e, &a).0 == "item").await;
    assert_eq!(req_item(&e, &a), "ST-1");
    // mentions ST-1: supplement + link
    let b = e
        .flow
        .ingest(WS, None, ev("c2", "ST-1 的 PR 检查又失败了"))
        .await
        .unwrap()
        .id()
        .unwrap()
        .to_string();
    until(|| event_outcome(&e, &b).0 == "linked").await;
    assert_eq!(event_outcome(&e, &b).1, "关联到 ST-1");
    assert!(e.core.calls().iter().any(|c| matches!(c, CoreCall::Supplement { item, text } if item == "ST-1" && text.starts_with("GitHub：ST-1 的 PR"))));
    assert_eq!(req_item(&e, &b), "ST-1");
    assert_eq!(e.core.created().len(), 1);
}

#[tokio::test]
async fn notify_to_item_wait_delay_and_sub_flows() {
    let Env { e, .. } = env();
    // a child that ignores, called by a parent that waits 30m and then notifies
    let child = create_flow(&e, "子流程", json!({
        "nodes": [{"id": "t", "kind": "trigger", "config": {"source": "manual"}},
                  {"id": "i", "kind": "action", "config": {"action": "ignore", "reason": "{{input.why}}"}}],
        "edges": [{"from": "t", "to": "i"}]
    })).await;
    let parent = create_flow(&e, "提醒", json!({
        "nodes": [{"id": "t", "kind": "trigger", "config": {"source": "mail", "types": ["mail.new"]}},
                  {"id": "c", "kind": "flow", "config": {"flow_id": child, "input": {"why": "{{event.summary}}"}}},
                  {"id": "w", "kind": "wait", "config": {"mode": "delay", "delay": "30m"}},
                  {"id": "n", "kind": "action", "config": {"action": "notify", "text": "记得看：{{event.summary}}"}}],
        "edges": [{"from": "t", "to": "c"}, {"from": "c", "to": "w"}, {"from": "w", "to": "n"}]
    })).await;
    enable(&e, &parent).await;
    let evt = e
        .flow
        .ingest(
            WS,
            None,
            mail("n1", "合同终稿", json!({"text": "附件是终稿"})),
        )
        .await
        .unwrap()
        .id()
        .unwrap()
        .to_string();
    let run: String = q(
        &e,
        "SELECT id FROM flow_runs WHERE event_id = ? AND parent_run IS NULL",
        [&evt],
    );
    until(|| run_status(&e, &run) == "waiting").await;
    let child_out: String = q(
        &e,
        "SELECT json_extract(context, '$.steps.c.reason') FROM flow_runs WHERE id = ?",
        [&run],
    );
    assert_eq!(child_out, "合同终稿");
    // too early: nothing; after 30 minutes the scheduler wakes it
    assert!(scheduler::claim_due(&e.flow).unwrap().is_empty());
    e.env.clock.advance(31 * 60_000);
    let ids = scheduler::claim_due(&e.flow).unwrap();
    assert_eq!(ids, vec![run.clone()]);
    scheduler::drive(&e.flow, &run).await.unwrap();
    assert_eq!(run_status(&e, &run), "done");
    until(|| event_outcome(&e, &evt).0 == "need").await;
    let dec = open_decision(&e, "signal").expect("notify decision");
    assert_eq!(
        q::<String>(&e, "SELECT question FROM decisions WHERE id = ?", [&dec]),
        "记得看：合同终稿"
    );
    // 转成事项
    answer(&e, &dec, "to_item").await;
    assert_eq!(event_outcome(&e, &evt).0, "item");
    assert_eq!(req_item(&e, &evt), "ST-1");
    assert_eq!(e.core.created()[0].goal, "合同终稿\n\n附件是终稿");
}

#[tokio::test]
async fn cancel_and_manual_rules() {
    let Env { e, .. } = env();
    let id = create_flow(&e, "邮件分诊", r1()).await;
    let (st, v) = api(&e, "POST", &format!("/flows/{id}/run"), None).await;
    assert_eq!(
        (st, v["error"].as_str()),
        (400, Some("这条流程由事件触发，不能手动运行"))
    );
    let w = create_flow(&e, "等", json!({
        "nodes": [{"id": "t", "kind": "trigger", "config": {"source": "manual"}},
                  {"id": "w", "kind": "wait", "config": {"mode": "until", "until": "next_workday 09:00"}}],
        "edges": [{"from": "t", "to": "w"}]
    })).await;
    let (_, v) = api(&e, "POST", &format!("/flows/{w}/run"), None).await;
    let run = v["run_id"].as_str().unwrap().to_string();
    until(|| run_status(&e, &run) == "waiting").await;
    let (st, v) = api(&e, "POST", &format!("/runs/{run}/cancel"), None).await;
    assert_eq!((st, v["status"].as_str()), (200, Some("cancelled")));
    assert_eq!(
        q::<i64>(
            &e,
            "SELECT count(*) FROM flow_waits WHERE run_id = ?",
            [&run]
        ),
        0
    );
    let (st, _) = api(&e, "POST", &format!("/runs/{run}/cancel"), None).await;
    assert_eq!(st, 409);
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/runs/{run}"),
        None,
    )
    .await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn a_version_reads_back_its_graph() {
    let Env { e, .. } = env();
    let id = create_flow(&e, "邮件分诊", r1()).await;
    let mut g = r1();
    g["nodes"][0]["config"]["types"] = json!(["mail.new", "mail.reply"]);
    let (st, v) = api(
        &e,
        "PUT",
        &format!("/flows/{id}"),
        Some(json!({ "graph": g, "note": "也看回复" })),
    )
    .await;
    assert_eq!(st, 200, "{v}");

    let (st, v1) = api(&e, "GET", &format!("/flows/{id}/versions/1"), None).await;
    assert_eq!(st, 200, "{v1}");
    assert_eq!(v1["version"], 1);
    assert_eq!(
        v1["graph"]["nodes"][0]["config"]["types"],
        json!(["mail.new"])
    );
    let (_, v2) = api(&e, "GET", &format!("/flows/{id}/versions/2"), None).await;
    assert_eq!(
        (
            v2["version"].as_i64(),
            v2["note"].as_str(),
            v2["author"].as_str()
        ),
        (Some(2), Some("也看回复"), Some(ME))
    );
    assert_eq!(
        v2["graph"]["nodes"][0]["config"]["types"],
        json!(["mail.new", "mail.reply"])
    );

    // no such version, not a number, another workspace's flow: 404
    for path in [
        format!("/flows/{id}/versions/9"),
        format!("/flows/{id}/versions/x"),
    ] {
        let (st, v) = api(&e, "GET", &path, None).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (404, Some("没有这个版本")),
            "{path}"
        );
    }
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/flows/{id}/versions/1"),
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (st, v) = api(&e, "GET", "/flows/flw_nope/versions/1", None).await;
    assert_eq!((st, v["error"].as_str()), (404, Some("没有这个流程")));
}

#[tokio::test]
async fn dry_runs_from_an_event_keep_it_and_history_filters_them() {
    let Env { e, .. } = env();
    let id = create_flow(&e, "邮件分诊", r1()).await;
    // the flow is off: the event is stored, nothing runs
    let evt = e
        .flow
        .ingest(
            WS,
            None,
            mail(
                "m1",
                "GitHub：PR #12 新评论",
                json!({"from": "notifications@github.com"}),
            ),
        )
        .await
        .unwrap();
    let evt = evt.id().unwrap().to_string();

    // asked for by id, or the latest the trigger takes: the run keeps the event
    let (st, run) = api(
        &e,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({ "event_id": evt })),
    )
    .await;
    assert_eq!(st, 200, "{run}");
    assert_eq!(
        (run["dry_run"].as_bool(), run["event_id"].as_str()),
        (Some(true), Some(evt.as_str()))
    );
    let (_, run) = api(&e, "POST", &format!("/flows/{id}/dry-run"), None).await;
    assert_eq!(run["event_id"], json!(evt), "{run}");
    // a sample is no event
    let (_, run) = api(
        &e,
        "POST",
        &format!("/flows/{id}/dry-run"),
        Some(json!({ "sample": {"payload": {"from": "a@b.com"}} })),
    )
    .await;
    assert!(run["event_id"].is_null(), "{run}");
    // a dry run does not settle the event
    assert_eq!(
        q::<i64>(
            &e,
            "SELECT count(*) FROM flow_runs WHERE event_id = ? AND dry_run = 0",
            [&evt]
        ),
        0
    );

    // one real run
    enable(&e, &id).await;
    let real = e
        .flow
        .ingest(
            WS,
            None,
            mail(
                "m2",
                "GitHub：PR #13 新评论",
                json!({"from": "notifications@github.com"}),
            ),
        )
        .await
        .unwrap();
    let real = real.id().unwrap().to_string();
    until(|| event_outcome(&e, &real).0 == "ignored").await;

    let runs = |v: &Value| -> Vec<(bool, Option<String>)> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["dry_run"].as_bool().unwrap(),
                    r["event_id"].as_str().map(String::from),
                )
            })
            .collect()
    };
    let (st, all) = api(&e, "GET", &format!("/flows/{id}/runs"), None).await;
    assert_eq!(st, 200, "{all}");
    assert_eq!(all.as_array().unwrap().len(), 4);
    let (_, dry) = api(&e, "GET", &format!("/flows/{id}/runs?dry=1"), None).await;
    let dry = runs(&dry);
    assert_eq!(dry.len(), 3);
    assert!(dry.iter().all(|r| r.0));
    let (_, live) = api(&e, "GET", &format!("/flows/{id}/runs?dry=0"), None).await;
    assert_eq!(runs(&live), [(false, Some(real))]);
    let (st, v) = api(&e, "GET", &format!("/flows/{id}/runs?dry=yes"), None).await;
    assert_eq!(st, 400, "{v}");
}

/// Holds the store's writer on another thread for `ms`; returns once it is held.
fn hold_writer(e: &FlowEnv, ms: u64) -> std::thread::JoinHandle<()> {
    let store = e.flow.store().clone();
    let (held, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        store
            .transaction(|_| -> steve_store::Result<()> {
                held.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(ms));
                Ok(())
            })
            .unwrap();
    });
    rx.recv().unwrap();
    t
}

#[tokio::test]
async fn an_ingest_given_up_on_mid_write_still_drives_its_runs() {
    let Env { e, .. } = env();
    let id = create_flow(&e, "邮件分诊", r1()).await;
    enable(&e, &id).await;
    // the caller stops waiting (a poll timeout, a dropped hook request) while the write waits
    let holder = hold_writer(&e, 300);
    let ev = mail(
        "cut",
        "GitHub：PR #12 新评论",
        json!({"from": "notifications@github.com"}),
    );
    let r = tokio::time::timeout(Duration::from_millis(50), e.flow.ingest(WS, None, ev)).await;
    assert!(r.is_err(), "the ingest was cut off");
    holder.join().unwrap();
    // the commit landed and the work after it ran: the run is driven and the event settled
    until(|| {
        q::<i64>(
            &e,
            "SELECT count(*) FROM ext_events WHERE external_id = 'cut' AND outcome = 'ignored'",
            [],
        ) == 1
    })
    .await;
    assert_eq!(
        q::<i64>(
            &e,
            "SELECT count(*) FROM flow_runs r JOIN ext_events x ON x.id = r.event_id
              WHERE x.external_id = 'cut' AND r.status = 'done'",
            [],
        ),
        1
    );
}

#[tokio::test]
async fn branch_and_edge_label_mismatches_are_warned_and_logged() {
    let Env { e, .. } = env();
    let graph = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "manual"}},
            {"id": "f", "kind": "filter", "title": "是不是", "config": {"rules": [{"label": "是", "when": "true"}], "else_label": "否"}},
            {"id": "a", "kind": "action", "title": "忽略一", "config": {"action": "ignore", "reason": "一"}},
            {"id": "b", "kind": "action", "title": "忽略二", "config": {"action": "ignore", "reason": "二"}}
        ],
        "edges": [{"from": "t", "to": "f"}, {"from": "f", "to": "a", "label": "yes"}, {"from": "f", "to": "b", "label": "否"}]
    });
    // saved (a warning, not a refusal); the detail lists it: the edge first (strong), then the branch
    let id = create_flow(&e, "分支对不上", graph).await;
    let (_, v) = api(&e, "GET", &format!("/flows/{id}"), None).await;
    let w = v["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(
        (
            w[0]["kind"].as_str(),
            w[0]["label"].as_str(),
            w[0]["strong"].as_bool()
        ),
        (Some("edge"), Some("yes"), Some(true))
    );
    assert_eq!(
        w[0]["text"],
        "「是不是」的连线「yes」对不上任何分支（分支是：是、否），这条线永远走不到"
    );
    assert_eq!(
        (
            w[1]["kind"].as_str(),
            w[1]["label"].as_str(),
            w[1]["strong"].as_bool()
        ),
        (Some("branch"), Some("是"), Some(false))
    );

    // the run takes 是, which has no edge: it ends there and says so
    let (_, v) = api(&e, "POST", &format!("/flows/{id}/run"), None).await;
    let run = v["run_id"].as_str().unwrap().to_string();
    until(|| run_status(&e, &run) == "done").await;
    let label: String = q(
        &e,
        "SELECT outcome_label FROM flow_runs WHERE id = ?",
        [&run],
    );
    assert_eq!(label, "分支「是」没有连线，到「是不是」为止");

    // matching labels: no warnings
    let ok = create_flow(&e, "对得上", json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "manual"}},
            {"id": "f", "kind": "filter", "config": {"rules": [{"label": "是", "when": "true"}], "else_label": "否"}},
            {"id": "a", "kind": "action", "config": {"action": "ignore", "reason": "一"}},
            {"id": "b", "kind": "action", "config": {"action": "ignore", "reason": "二"}}
        ],
        "edges": [{"from": "t", "to": "f"}, {"from": "f", "to": "a", "label": "是"}, {"from": "f", "to": "b", "label": "否"}]
    })).await;
    let (_, v) = api(&e, "GET", &format!("/flows/{ok}"), None).await;
    assert_eq!(v["warnings"], json!([]));
}
