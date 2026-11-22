//! Independent review probes of the outbox (written by the review worker on an isolated snapshot).
use serde_json::{Value, json};
use steve_flow::{EventSink, ItemTrigger, RawEvent, engine, testkit::FlowEnv};
use steve_modules::testkit::{ME, WS, WS2, call, caller};
use steve_store::outbox::{self, Consumer, Event};
use steve_store::{SignalRows, testkit::flow as fixtures};

async fn flow(e: &FlowEnv, graph: Value) -> String {
    let (s, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/flows",
        Some(&json!({"name":"review","graph":graph})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    let (s, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/flows/{id}"),
        Some(&json!({"enabled":true})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    id
}
fn event(e: &FlowEnv, ws: &str) -> Event {
    fixtures::fixture_item(
        e.env.store(),
        WS,
        "review-item",
        777,
        "current title",
        "done",
    )
    .unwrap();
    e.env.store().transaction(|tx|outbox::emit(tx,ws,"work.item.done",1,&json!({"item_id":"review-item","title":"snapshot title","status":"done","bot_id":"steve","project_id":null,"source":null}))).unwrap()
}
fn apply_without_after(e: &FlowEnv, ev: &Event) {
    let c = ItemTrigger::new(e.flow.clone());
    let applied = e
        .env
        .store()
        .transaction(|tx| -> Result<_, outbox::ConsumeError> {
            let a = c.apply(tx, ev)?;
            fixtures::applied_receipt(tx, ev, c.id(), &a.outcome)?;
            Ok(a)
        })
        .unwrap();
    // Model process termination after commit but before AfterCommit is invoked.
    drop(applied.after);
}
#[tokio::test]
async fn committed_trigger_run_recovers_without_after_commit_and_keeps_fact_snapshot() {
    let e = FlowEnv::new();
    flow(&e,json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"item","types":["item.done"]}},{"id":"d","kind":"action","config":{"action":"digest","text":"review"}}],"edges":[{"from":"t","to":"d"}]})).await;
    let ev = event(&e, WS);
    apply_without_after(&e, &ev);
    assert_eq!(
        fixtures::runs_in_status(e.env.store(), "running").unwrap(),
        1
    );
    let signal = fixtures::event_ids(e.env.store()).unwrap().remove(0);
    assert_eq!(
        e.env
            .store()
            .read(|r| Ok(SignalRows(r).event(&signal)?.unwrap().payload["title"].clone()))
            .unwrap(),
        "snapshot title"
    );
    let ids = engine::recover(&e.flow).unwrap();
    assert_eq!(ids.len(), 1);
    engine::drive(&e.flow, &ids[0]).await.unwrap();
    assert_eq!(fixtures::only_run(e.env.store()).unwrap().status, "done");
}
#[tokio::test]
async fn committed_wait_wakeup_recovers_without_after_commit() {
    let e = FlowEnv::new();
    flow(&e,json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"mail","types":["mail.new"]}},{"id":"w","kind":"wait","config":{"mode":"event","match":{"source":"item","type":"item.done"},"timeout":"1d","timeout_label":"timeout"}},{"id":"d","kind":"action","config":{"action":"digest","text":"review"}}],"edges":[{"from":"t","to":"w"},{"from":"w","to":"d"}]})).await;
    e.flow
        .ingest(
            WS,
            None,
            RawEvent::new("mail", "mail.new", "start", 1, "start"),
        )
        .await
        .unwrap();
    steve_runtime::testkit::eventually_sync("the committed run reaches waiting", || {
        fixtures::runs_in_status(e.env.store(), "waiting").unwrap() == 1
    })
    .await;
    assert_eq!(fixtures::wait_count(e.env.store()).unwrap(), 1);
    let ev = event(&e, WS);
    apply_without_after(&e, &ev);
    assert_eq!(fixtures::wait_count(e.env.store()).unwrap(), 0);
    assert_eq!(fixtures::only_run(e.env.store()).unwrap().status, "running");
    let ids = engine::recover(&e.flow).unwrap();
    assert_eq!(ids.len(), 1);
    engine::drive(&e.flow, &ids[0]).await.unwrap();
    assert_eq!(fixtures::only_run(e.env.store()).unwrap().status, "done");
}
#[tokio::test]
async fn item_from_another_workspace_has_no_consumer_writes() {
    let e = FlowEnv::new();
    let ev = event(&e, WS2);
    let c = ItemTrigger::new(e.flow.clone());
    let r = e.env.store().transaction(|tx| c.apply(tx, &ev));
    assert!(matches!(r, Err(outbox::ConsumeError::Reject(_))));
    assert_eq!(fixtures::event_ids(e.env.store()).unwrap().len(), 0);
    assert_eq!(fixtures::run_count(e.env.store()).unwrap(), 0);
}
#[tokio::test]
async fn recovery_gate_stays_closed_on_failure_and_opens_after_repair() {
    let e = FlowEnv::new();
    let sched = steve_flow::Scheduler::new(e.flow.clone());
    fixtures::set_run_table_available(e.env.store(), false).unwrap();
    assert!(sched.boot().await.is_err());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), e.flow.recovered())
            .await
            .is_err()
    );
    fixtures::set_run_table_available(e.env.store(), true).unwrap();
    sched.boot().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(10), e.flow.recovered())
        .await
        .unwrap();
}
