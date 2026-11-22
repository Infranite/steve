//! S10 acceptance: after a Bot is deleted its flow references are the
//! heir and each flow has one more version; `item.done` starts a flow; `steve_event_get` cannot read
//! the events of another item. Plus `GET /bots/:id/flows` and `digest::take_now`.

use steve_runtime::testkit::eventually_sync;

use serde_json::{Value, json};
use steve_flow::item_events::ItemEvent;
use steve_flow::mcp::{self, EVENT_GET, NOT_LINKED, PURGED};
use steve_flow::testkit::FlowEnv;
use steve_flow::{EventSink, Ingested, RawEvent, digest, ingest_item_event, on_bot_deleted};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, WS, WS2, call, caller};
use steve_store::base::ObjRef;
use steve_store::base::links::{Links, NewLink};
use steve_store::{FlowRows, FlowSources, RunRows, SignalRows, testkit::flow as fixtures};

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

async fn create_flow(e: &FlowEnv, name: &str, graph: Value) -> String {
    let (st, v) = api(
        e,
        "POST",
        "/flows",
        Some(json!({ "name": name, "graph": graph })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (st, v) = api(
        e,
        "PATCH",
        &format!("/flows/{id}"),
        Some(json!({ "enabled": true })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    id
}

/// mail → bot(ops) → item.create(bot ops)
fn ops_graph() -> Value {
    json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "mail", "types": ["mail.new"]}},
            {"id": "b", "kind": "bot", "config": {"bot_id": "ops", "instruction": "要不要处理", "branches": ["处理"]}},
            {"id": "a", "kind": "action", "config": {"action": "item.create", "mode": "start", "bot": "ops"}}
        ],
        "edges": [{"from": "t", "to": "b"}, {"from": "b", "to": "a", "label": "处理"}]
    })
}

fn version(e: &FlowEnv, id: &str) -> i64 {
    e.flow_row(id).version
}

#[tokio::test]
async fn bot_deleted_hands_flows_to_heir() {
    let e = FlowEnv::new();
    let used = create_flow(&e, "运维分诊", ops_graph()).await;
    let other = create_flow(
        &e,
        "只记汇报",
        json!({"nodes": [{"id": "t", "kind": "trigger", "config": {"source": "mail"}},
                         {"id": "d", "kind": "action", "config": {"action": "digest", "text": "x"}}],
               "edges": [{"from": "t", "to": "d"}]}),
    )
    .await;
    // a flow whose bot node falls back to the source's triage bot
    let con = e.connection(WS, "fake_mail", None, ME);
    let triaged = create_flow(
        &e,
        "按源判断",
        json!({"nodes": [{"id": "t", "kind": "trigger", "config": {"source": "mail", "connections": [con]}},
                         {"id": "b", "kind": "bot", "config": {"instruction": "看看", "branches": ["忽略"]}},
                         {"id": "i", "kind": "action", "config": {"action": "ignore", "reason": "x"}}],
               "edges": [{"from": "t", "to": "b"}, {"from": "b", "to": "i", "label": "忽略"}]}),
    )
    .await;
    fixtures::set_triage_bot(e.env.store(), WS, &con, "ops").unwrap();
    let (st, sched) = api(
        &e,
        "POST",
        "/schedules",
        Some(json!({"name": "周报", "when": "每周一 09:00", "what": "整理周报", "bot": "ops", "mode": "item"})),
    )
    .await;
    assert_eq!(st, 200, "{sched}");
    let sched_id = sched["id"].as_str().unwrap().to_string();

    // the Bot page lists what uses it
    let (st, v) = api(&e, "GET", "/bots/ops/flows", None).await;
    assert_eq!(st, 200, "{v}");
    let mut ids: Vec<&str> = v["flows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    ids.sort();
    let mut want = vec![used.as_str(), triaged.as_str()];
    want.sort();
    assert_eq!(ids, want);
    assert_eq!(v["schedules"][0]["id"], json!(sched_id));
    let (st, v) = api(&e, "GET", "/bots/w2bot/flows", None).await;
    assert_eq!((st, v["error"].as_str()), (404, Some("没有这个 Bot")));

    let (v_used, v_other, v_sched) = (
        version(&e, &used),
        version(&e, &other),
        version(&e, &sched_id),
    );
    let now = e.now();
    let moved = e
        .env
        .store()
        .write(|tx| on_bot_deleted(tx, WS, "ops", "steve", now))
        .unwrap();
    assert_eq!(moved.flows, vec![used.clone()]);
    assert_eq!(moved.schedules, vec![sched_id.clone()]);
    assert_eq!(moved.sources, vec![con.clone()]);

    // one more version, by steve, with the note; the references are the heir
    assert_eq!(version(&e, &used), v_used + 1);
    assert_eq!(version(&e, &other), v_other);
    assert_eq!(version(&e, &sched_id), v_sched + 1);
    let (_, f) = api(&e, "GET", &format!("/flows/{used}"), None).await;
    assert_eq!(f["graph"]["nodes"][1]["config"]["bot_id"], "steve");
    assert_eq!(f["graph"]["nodes"][2]["config"]["bot"], "steve");
    assert_eq!(f["versions"][0]["author"], "steve");
    assert_eq!(f["versions"][0]["note"], "运维 已删除，交给 Steve");
    let (_, s) = api(&e, "GET", &format!("/schedules/{sched_id}"), None).await;
    assert_eq!(s["bot"], "steve");
    let versions = e
        .env
        .store()
        .read(|r| FlowRows(r).versions(&sched_id))
        .unwrap();
    let note = &versions[0].note;
    assert_eq!(note, "运维 已删除，交给 Steve");
    let triage = e
        .env
        .store()
        .read(|r| FlowSources(r).triage(WS, &con))
        .unwrap();
    assert_eq!(triage, None);
    let revs = fixtures::revisions_by(e.env.store(), "flow", &used, "steve").unwrap();
    assert_eq!(revs, 1);

    // nothing left to move
    let again = e
        .env
        .store()
        .write(|tx| on_bot_deleted(tx, WS, "ops", "steve", now))
        .unwrap();
    assert!(again.flows.is_empty() && again.schedules.is_empty() && again.sources.is_empty());
    let (_, v) = api(&e, "GET", "/bots/ops/flows", None).await;
    assert_eq!(v, json!({"flows": [], "schedules": []}));
}

fn item(e: &FlowEnv, ws: &str, id: &str, no: i64, status: &str) {
    fixtures::fixture_item(e.env.store(), ws, id, no, "写周报", status).unwrap();
}

#[tokio::test]
async fn item_done_triggers_a_flow() {
    let e = FlowEnv::new();
    item(&e, WS, "ST-1", 1, "done");
    // nothing listens: nothing stored
    assert_eq!(
        ingest_item_event(&e.flow, WS, "ST-1", ItemEvent::Done)
            .await
            .unwrap(),
        None
    );
    assert_eq!(fixtures::event_ids(e.env.store()).unwrap().len(), 0);

    let fid = create_flow(
        &e,
        "完成后记一笔",
        json!({"nodes": [{"id": "t", "kind": "trigger", "config": {"source": "item", "types": ["item.done"]}},
                         {"id": "d", "kind": "action", "config": {"action": "digest", "text": "{{event.payload.item_id}} 做完了"}}],
               "edges": [{"from": "t", "to": "d"}]}),
    )
    .await;
    let got = ingest_item_event(&e.flow, WS, "ST-1", ItemEvent::Done)
        .await
        .unwrap()
        .unwrap();
    let Ingested::Stored { id, runs } = got else {
        panic!("not stored: {got:?}")
    };
    assert_eq!(runs.len(), 1);
    let event = e
        .env
        .store()
        .read(|r| SignalRows(r).event(&id))
        .unwrap()
        .unwrap();
    let (source, ty, summary) = (event.source, event.kind, event.summary);
    assert_eq!(
        (source.as_str(), ty.as_str(), summary.as_str()),
        ("item", "item.done", "ST-1「写周报」完成了")
    );
    let run = runs[0].clone();
    eventually_sync("The flow run records a done status.", || {
        e.run_row(&run).status == "done"
    })
    .await;
    let flow: String = e.run_row(&run).flow_id;
    assert_eq!(flow, fid);
    let notes = e
        .env
        .store()
        .read(|c| steve_store::DigestNotes(c).pending(WS, 0))
        .unwrap();
    assert_eq!(notes[0].text, "ST-1 做完了");

    // dropped is another type: stored (a trigger listens to items) but starts nothing
    item(&e, WS, "ST-2", 2, "drop");
    let got = ingest_item_event(&e.flow, WS, "ST-2", ItemEvent::Dropped)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(got, Ingested::Stored { ref runs, .. } if runs.is_empty()),
        "{got:?}"
    );

    // an item of another workspace
    item(&e, WS2, "ST-9", 9, "done");
    let err = ingest_item_event(&e.flow, WS, "ST-9", ItemEvent::Done)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "没有这个事项");
}

#[tokio::test]
async fn item_made_by_a_flow_does_not_restart_it() {
    let e = FlowEnv::new();
    item(&e, WS, "ST-1", 1, "done");
    let fid = create_flow(
        &e,
        "完成后复盘",
        json!({"nodes": [{"id": "t", "kind": "trigger", "config": {"source": "item", "types": ["item.done"]}},
                         {"id": "a", "kind": "action", "config": {"action": "item.create", "mode": "start", "title": "复盘"}}],
               "edges": [{"from": "t", "to": "a"}]}),
    )
    .await;
    let Some(Ingested::Stored { runs, .. }) =
        ingest_item_event(&e.flow, WS, "ST-1", ItemEvent::Done)
            .await
            .unwrap()
    else {
        panic!("not stored")
    };
    let run = runs[0].clone();
    eventually_sync("The flow run records a done status.", || {
        e.run_row(&run).status == "done"
    })
    .await;
    let made = e.run_row(&run).work_item_id.unwrap();
    assert_eq!(made, "ST-2");
    fixtures::set_item_status(e.env.store(), "ST-2", "done").unwrap();
    let got = ingest_item_event(&e.flow, WS, "ST-2", ItemEvent::Done)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(got, Ingested::Stored { ref runs, .. } if runs.is_empty()),
        "{got:?}"
    );
    assert_eq!(
        e.env
            .store()
            .read(|r| Ok(RunRows(r).activity(WS, &fid, 0)?.0))
            .unwrap(),
        1
    );
}

fn mail(id: &str, summary: &str, text: &str) -> RawEvent {
    let mut ev = RawEvent::new("mail", "mail.new", id, 1_793_000_000_000, summary);
    ev.payload = json!({ "text": text });
    ev
}

fn mcp_cx(item: Option<&str>) -> McpCx {
    McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "steve".into(),
        item_id: item.map(String::from),
        session_id: Some("ses_1".into()),
    }
}

#[tokio::test]
async fn event_get_reads_only_the_items_events() {
    let e = FlowEnv::new();
    let a = e
        .flow
        .ingest(
            WS,
            None,
            mail("m1", "Sarah：周五评审改时间", "能改到下午 2 点吗？"),
        )
        .await
        .unwrap();
    let b = e
        .flow
        .ingest(WS, None, mail("m2", "别人的邮件", "不该看到"))
        .await
        .unwrap();
    let (a, b) = (a.id().unwrap().to_string(), b.id().unwrap().to_string());
    item(&e, WS, "ST-1", 1, "run");
    item(&e, WS, "ST-2", 2, "run");
    e.env
        .store()
        .write(|w| -> steve_store::Result<()> {
            Links(w).add(
                WS,
                &NewLink::new(
                    ObjRef::new("item", "ST-1"),
                    "from",
                    ObjRef::new("event", &a),
                    "sig",
                ),
            )?;
            Links(w).add(
                WS,
                &NewLink::new(
                    ObjRef::new("item", "ST-2"),
                    "from",
                    ObjRef::new("event", &b),
                    "sig",
                ),
            )?;
            Ok(())
        })
        .unwrap();

    let cx = mcp_cx(Some("ST-1"));
    let out = mcp::call(&e.flow, &cx, EVENT_GET, &json!({}))
        .await
        .unwrap();
    assert!(out.contains("Summary: Sarah：周五评审改时间"), "{out}");
    assert!(out.contains("能改到下午 2 点吗？"), "{out}");
    let same = mcp::call(&e.flow, &cx, EVENT_GET, &json!({ "event": a }))
        .await
        .unwrap();
    assert_eq!(same, out);
    // another item's event, a made-up id: refused
    let err = mcp::call(&e.flow, &cx, EVENT_GET, &json!({ "event": b }))
        .await
        .unwrap_err();
    assert_eq!(err, NOT_LINKED);
    assert!(!err.contains("不该看到"));
    assert_eq!(
        mcp::call(&e.flow, &cx, EVENT_GET, &json!({ "event": "evt_x" }))
            .await
            .unwrap_err(),
        NOT_LINKED
    );
    // same item id seen from another workspace has no links there
    let mut other = mcp_cx(Some("ST-1"));
    other.ws = WS2.into();
    assert!(
        mcp::call(&e.flow, &other, EVENT_GET, &json!({}))
            .await
            .is_err()
    );
    // no item; a bad argument
    assert!(
        mcp::call(&e.flow, &mcp_cx(None), EVENT_GET, &json!({}))
            .await
            .is_err()
    );
    let bad = mcp::call(&e.flow, &cx, EVENT_GET, &json!({ "event": 3 }))
        .await
        .unwrap_err();
    assert!(bad.contains("expected string at event"), "{bad}");

    // past retention only the summary is left
    e.env
        .store()
        .write(|w| SignalRows(w).purge_payloads(&[a], e.now()))
        .unwrap();
    let out = mcp::call(&e.flow, &cx, EVENT_GET, &json!({}))
        .await
        .unwrap();
    assert_eq!(out, format!("{PURGED}Sarah：周五评审改时间"));
    assert!(mcp::handles(EVENT_GET) && !mcp::handles("steve_x"));
    assert_eq!(
        mcp::tools()[0]["description"],
        "Read the original email, message or alert this item came from."
    );
}

#[tokio::test]
async fn digest_take_now_takes_once() {
    let e = FlowEnv::new();
    let now = e.now();
    e.env
        .store()
        .write(|tx| {
            steve_store::DigestNotes(tx).add(WS, "Acme 回信了", None, None, now)?;
            steve_store::DigestNotes(tx).add(WS2, "别的空间", None, None, now)
        })
        .unwrap();
    let got = digest::take_now(&e.flow, WS, 0).unwrap();
    assert_eq!(
        got.iter().map(|d| d.text.as_str()).collect::<Vec<_>>(),
        vec!["Acme 回信了"]
    );
    assert!(digest::take_now(&e.flow, WS, 0).unwrap().is_empty());
    assert_eq!(digest::take_now(&e.flow, WS2, 0).unwrap().len(), 1);
}
