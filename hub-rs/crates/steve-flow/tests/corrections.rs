//! S8 acceptance: after “以后这类忽略” a new event of the same group is
//! ignored; undoing the changeset brings the old graph back; correcting an event no flow handled
//! makes a `correction` flow; “转成事项” makes the event's outcome `item`.

use serde_json::{Value, json};
use steve_flow::testkit::FlowEnv;
use steve_flow::{EventSink, RawEvent};
use steve_modules::testkit::{ME, WS, WS2, call, caller};
use steve_store::testkit::flow as fixtures;

async fn api(
    e: &FlowEnv,
    who: (&str, &str),
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    call(
        &e.router(),
        Some(&caller(who.0, who.1)),
        method,
        path,
        Some(&body),
    )
    .await
}

fn mail(id: &str, group: Option<&str>, thread: Option<&str>, summary: &str) -> RawEvent {
    let mut ev = RawEvent::new("mail", "mail.new", id, steve_store::now(), summary);
    ev.scope_ext_id = Some("INBOX".into());
    ev.group_key = group.map(String::from);
    ev.thread_key = thread.map(String::from);
    ev.payload = json!({ "text": format!("原文：{summary}") });
    ev
}

async fn ingest(e: &FlowEnv, ev: RawEvent) -> String {
    let r = e.flow.ingest(WS, None, ev).await.unwrap();
    r.id().unwrap().to_string()
}

fn outcome(e: &FlowEnv, id: &str) -> String {
    e.signal_row(id).outcome
}

async fn settled(e: &FlowEnv, id: &str, want: &str) {
    steve_runtime::testkit::eventually_sync(format!("event {id} outcome becomes {want}"), || {
        outcome(e, id) == want
    })
    .await;
}

/// `t → d (digest)`, enabled.
async fn digest_flow(e: &FlowEnv) -> (String, Value) {
    let graph = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "title": "邮件", "config": {"source": "mail", "types": ["mail.new"]}},
            {"id": "d", "kind": "action", "title": "进汇报", "config": {"action": "digest"}}
        ],
        "edges": [{"from": "t", "to": "d", "label": ""}]
    });
    let (st, v) = api(
        e,
        (WS, ME),
        "POST",
        "/flows",
        json!({"name": "邮件汇报", "graph": graph}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (st, _) = api(
        e,
        (WS, ME),
        "PATCH",
        &format!("/flows/{id}"),
        json!({"enabled": true}),
    )
    .await;
    assert_eq!(st, 200);
    let (_, v) = api(e, (WS, ME), "GET", &format!("/flows/{id}"), json!({})).await;
    (id, v["graph"].clone())
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
async fn ignore_group_then_undo_restores_the_graph() {
    let e = FlowEnv::new();
    let (fid, original) = digest_flow(&e).await;
    let m1 = ingest(&e, mail("m1", Some("acme"), Some("th1"), "Acme：周报")).await;
    settled(&e, &m1, "digest").await;

    let (st, v) = api(
        &e,
        (WS, ME),
        "POST",
        &format!("/signals/{m1}/correct"),
        json!({"action": "ignore"}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["flow_id"], json!(fid));
    let cs = v["changeset_id"].as_str().unwrap().to_string();

    // a new version by steve, with the exception right after the trigger
    let f = flow(&e, &fid).await;
    assert_eq!(f["version"], 2);
    assert_eq!(f["versions"][0]["author"], "steve");
    assert_eq!(f["versions"][0]["note"], "按你的纠正");
    let nodes = f["graph"]["nodes"].as_array().unwrap();
    let filter = nodes.iter().find(|n| n["kind"] == "filter").unwrap();
    assert_eq!(filter["title"], "例外：同组（acme）");
    assert_eq!(
        filter["config"]["rules"][0]["when"],
        r#"event.group_key == "acme""#
    );
    // the rule memory and the correction row
    let mem = fixtures::rule_texts(e.env.store()).unwrap().remove(0);
    assert_eq!(mem, "邮件里「Acme：周报」这类以后忽略");
    let edits = fixtures::corrections(e.env.store(), &m1).unwrap();
    assert_eq!(
        edits
            .iter()
            .filter(|edit| edit.action == "ignore"
                && edit.scope == "group"
                && edit.change.as_deref() == Some(cs.as_str()))
            .count(),
        1
    );

    // same group → ignored; another group still goes to the digest
    let m2 = ingest(&e, mail("m2", Some("acme"), Some("th2"), "Acme：又一封")).await;
    settled(&e, &m2, "ignored").await;
    let m3 = ingest(&e, mail("m3", Some("beta"), None, "Beta：周报")).await;
    settled(&e, &m3, "digest").await;

    // undo: the old graph is back (as a new version), the memory is gone
    steve_modules::changes::undo(&e.env.modules, &caller(WS, ME), &cs).unwrap();
    let f = flow(&e, &fid).await;
    assert_eq!(f["version"], 3);
    assert_eq!(f["versions"][0]["note"], "撤销纠正");
    assert_eq!(f["graph"], original);
    let n = fixtures::rule_texts(e.env.store()).unwrap().len();
    assert_eq!(n, 0);
    let m4 = ingest(&e, mail("m4", Some("acme"), None, "Acme：第三封")).await;
    settled(&e, &m4, "digest").await;
}

#[tokio::test]
async fn corrections_stack_and_scope_one_uses_the_thread() {
    let e = FlowEnv::new();
    let (fid, _) = digest_flow(&e).await;
    let m1 = ingest(&e, mail("m1", Some("acme"), Some("th1"), "Acme：周报")).await;
    settled(&e, &m1, "digest").await;
    let (st, v) = api(
        &e,
        (WS, ME),
        "POST",
        &format!("/signals/{m1}/correct"),
        json!({"action": "remind", "scope": "one"}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = api(
        &e,
        (WS, ME),
        "POST",
        &format!("/signals/{m1}/correct"),
        json!({"action": "ignore", "scope": "group"}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(flow(&e, &fid).await["version"], 3);
    // the newest exception is checked first: the group's ignore wins over the thread's remind
    let m2 = ingest(&e, mail("m2", Some("acme"), Some("th1"), "Acme：回复")).await;
    settled(&e, &m2, "ignored").await;
    // the thread alone, in another group → reminded passively in Feed (zero decisions opened)
    let m3 = ingest(&e, mail("m3", Some("beta"), Some("th1"), "转发：周报")).await;
    settled(&e, &m3, "need").await;
    let open = fixtures::decisions_for(e.env.store(), "signal", &m3)
        .unwrap()
        .iter()
        .filter(|d| d.status == steve_types::DecisionStatus::Open)
        .count();
    assert_eq!(open, 0);
    let entries = e
        .env
        .store()
        .write(|w| steve_store::feed::list(w, WS, ME, &Default::default(), None, 10, 0))
        .unwrap()
        .entries;
    assert!(
        entries
            .iter()
            .any(|entry| entry.title.contains("转发：周报")),
        "expected passive Feed reminder for m3, got {entries:?}"
    );
}

#[tokio::test]
async fn an_unhandled_event_gets_a_correction_flow() {
    let e = FlowEnv::new();
    let m1 = ingest(&e, mail("m1", Some("acme"), None, "Acme：账单")).await;
    settled(&e, &m1, "recorded").await;
    let (st, v) = api(
        &e,
        (WS, ME),
        "POST",
        &format!("/signals/{m1}/correct"),
        json!({"action": "remind"}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let fid = v["flow_id"].as_str().unwrap().to_string();
    let cs = v["changeset_id"].as_str().unwrap().to_string();
    let f = flow(&e, &fid).await;
    assert_eq!(f["origin"], "correction");
    assert_eq!(f["origin_label"], "按你的纠正");
    assert_eq!(f["enabled"], true);
    assert_eq!(f["version"], 1);
    assert_eq!(f["versions"][0]["author"], "steve");
    assert_eq!(f["trigger"]["source"], "mail");
    assert_eq!(f["trigger"]["types"], json!(["mail.new"]));

    let m2 = ingest(&e, mail("m2", Some("acme"), None, "Acme：新账单")).await;
    settled(&e, &m2, "need").await;
    // other groups end at the filter: only recorded
    let m3 = ingest(&e, mail("m3", Some("beta"), None, "Beta：账单")).await;
    settled(&e, &m3, "recorded").await;

    // undo deletes the flow
    steve_modules::changes::undo(&e.env.modules, &caller(WS, ME), &cs).unwrap();
    let (st, _) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "GET",
        &format!("/flows/{fid}"),
        None,
    )
    .await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn undo_refuses_after_the_flow_was_saved_again() {
    let e = FlowEnv::new();
    let (fid, original) = digest_flow(&e).await;
    let m1 = ingest(&e, mail("m1", Some("acme"), None, "Acme：周报")).await;
    settled(&e, &m1, "digest").await;
    let (_, v) = api(
        &e,
        (WS, ME),
        "POST",
        &format!("/signals/{m1}/correct"),
        json!({"action": "ignore"}),
    )
    .await;
    let cs = v["changeset_id"].as_str().unwrap().to_string();
    let (st, _) = api(
        &e,
        (WS, ME),
        "PUT",
        &format!("/flows/{fid}"),
        json!({"graph": original}),
    )
    .await;
    assert_eq!(st, 200);
    let err = steve_modules::changes::undo(&e.env.modules, &caller(WS, ME), &cs).unwrap_err();
    assert!(err.to_string().contains("这次改动已经不能撤销了"), "{err}");
    // the memory stays: the undo was rolled back as a whole
    let n = fixtures::rule_texts(e.env.store()).unwrap().len();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn to_item_makes_an_item_once() {
    let e = FlowEnv::new();
    let m1 = ingest(&e, mail("m1", None, None, "Sarah：合同请在周五前回签")).await;
    settled(&e, &m1, "recorded").await;
    let (st, v) = api(
        &e,
        (WS, ME),
        "POST",
        &format!("/signals/{m1}/correct"),
        json!({"action": "to_item"}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let item = v["item_id"].as_str().unwrap().to_string();
    assert!(v.get("changeset_id").is_none());
    assert_eq!(outcome(&e, &m1), "item");
    let req = &e.core.created()[0];
    assert_eq!(req.source, ("邮件".to_string(), "sig".to_string()));
    assert!(!req.propose);
    assert_eq!(req.bot, None);
    assert!(req.goal.contains("原文：Sarah"));
    assert_eq!(
        fixtures::item_sources(e.env.store(), WS, "event", &m1).unwrap(),
        [item.clone()]
    );
    let edits = fixtures::corrections(e.env.store(), &m1).unwrap();
    assert_eq!(
        edits
            .iter()
            .filter(|edit| edit.action == "to_item" && edit.item.as_deref() == Some(item.as_str()))
            .count(),
        1
    );
    let (st, v) = api(
        &e,
        (WS, ME),
        "POST",
        &format!("/signals/{m1}/correct"),
        json!({"action": "to_item"}),
    )
    .await;
    assert_eq!(
        (st, v["error"].as_str()),
        (409, Some("这条信号已经转成事项了"))
    );
}

#[tokio::test]
async fn bad_requests() {
    let e = FlowEnv::new();
    let m1 = ingest(&e, mail("m1", None, None, "无分组")).await;
    settled(&e, &m1, "recorded").await;
    let path = format!("/signals/{m1}/correct");
    let (st, v) = api(&e, (WS, ME), "POST", &path, json!({"action": "forget"})).await;
    assert_eq!((st, v["error"].as_str()), (400, Some("参数不对：action")));
    let (st, v) = api(
        &e,
        (WS, ME),
        "POST",
        &path,
        json!({"action": "ignore", "scope": "all"}),
    )
    .await;
    assert_eq!((st, v["error"].as_str()), (400, Some("参数不对：scope")));
    // no group, no sender, no thread: nothing to recognise later events by
    for scope in ["group", "one"] {
        let (st, v) = api(
            &e,
            (WS, ME),
            "POST",
            &path,
            json!({"action": "ignore", "scope": scope}),
        )
        .await;
        assert_eq!(
            (st, v["error"].as_str()),
            (400, Some("这条信号认不出同类，没法按这个范围纠正"))
        );
    }
    let n = fixtures::flow_count(e.env.store()).unwrap();
    assert_eq!(n, 0);
    // another workspace
    let (st, _) = api(&e, (WS2, ME), "POST", &path, json!({"action": "to_item"})).await;
    assert_eq!(st, 404);
    let (st, _) = api(
        &e,
        (WS, ME),
        "POST",
        "/signals/evt_nope/correct",
        json!({"action": "ignore"}),
    )
    .await;
    assert_eq!(st, 404);
}
