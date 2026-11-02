//! Independent review probes of the outbox (written by the review worker on an isolated snapshot).
use serde_json::{Value, json};
use steve_flow::{EventSink, ItemTrigger, RawEvent, engine, testkit::FlowEnv};
use steve_modules::testkit::{ME, WS, WS2, call, caller};
use steve_store::outbox::{self, Consumer, Event};

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
fn q<T: rusqlite::types::FromSql>(e: &FlowEnv, sql: &str) -> T {
    e.env
        .store()
        .reader(|c| -> steve_store::Result<T> { Ok(c.query_row(sql, [], |r| r.get(0))?) })
        .unwrap()
}
fn event(e: &FlowEnv, ws: &str) -> Event {
    e.env.store().with_conn(|c|{c.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES ('review-item',?,777,'current title','done','','steve',1,1)",[WS])?;Ok(())}).unwrap();
    e.env.store().transaction(|tx|outbox::emit(tx,ws,"work.item.done",1,&json!({"item_id":"review-item","title":"snapshot title","status":"done","bot_id":"steve","project_id":null,"source":null}))).unwrap()
}
fn apply_without_after(e: &FlowEnv, ev: &Event) {
    let c = ItemTrigger::new(e.flow.clone());
    let applied=e.env.store().transaction(|tx|->Result<_,outbox::ConsumeError>{
   let a=c.apply(tx,ev)?;
   tx.execute("INSERT INTO outbox_receipts(event_seq,consumer_id,workspace_id,done,outcome) VALUES (?,?,?,?,?)",rusqlite::params![ev.seq,c.id(),ev.workspace_id,1,a.outcome])?;
   Ok(a)
 }).unwrap();
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
        q::<i64>(&e, "SELECT count(*) FROM flow_runs WHERE status='running'"),
        1
    );
    assert_eq!(
        q::<String>(&e, "SELECT json_extract(payload,'$.title') FROM ext_events"),
        "snapshot title"
    );
    let ids = engine::recover(&e.flow).unwrap();
    assert_eq!(ids.len(), 1);
    engine::drive(&e.flow, &ids[0]).await.unwrap();
    assert_eq!(q::<String>(&e, "SELECT status FROM flow_runs"), "done");
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
    for _ in 0..100 {
        if q::<i64>(&e, "SELECT count(*) FROM flow_runs WHERE status='waiting'") == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM flow_waits"), 1);
    let ev = event(&e, WS);
    apply_without_after(&e, &ev);
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM flow_waits"), 0);
    assert_eq!(q::<String>(&e, "SELECT status FROM flow_runs"), "running");
    let ids = engine::recover(&e.flow).unwrap();
    assert_eq!(ids.len(), 1);
    engine::drive(&e.flow, &ids[0]).await.unwrap();
    assert_eq!(q::<String>(&e, "SELECT status FROM flow_runs"), "done");
}
#[tokio::test]
async fn item_from_another_workspace_has_no_consumer_writes() {
    let e = FlowEnv::new();
    let ev = event(&e, WS2);
    let c = ItemTrigger::new(e.flow.clone());
    let r = e.env.store().transaction(|tx| c.apply(tx, &ev));
    assert!(matches!(r, Err(outbox::ConsumeError::Reject(_))));
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM ext_events"), 0);
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM flow_runs"), 0);
}
#[tokio::test]
async fn recovery_gate_stays_closed_on_failure_and_opens_after_repair() {
    let e = FlowEnv::new();
    let sched = steve_flow::Scheduler::new(e.flow.clone());
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch("ALTER TABLE flow_runs RENAME TO review_missing_runs")?;
            Ok(())
        })
        .unwrap();
    assert!(sched.boot().await.is_err());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), e.flow.recovered())
            .await
            .is_err()
    );
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch("ALTER TABLE review_missing_runs RENAME TO flow_runs")?;
            Ok(())
        })
        .unwrap();
    sched.boot().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_millis(10), e.flow.recovered())
        .await
        .unwrap();
}
