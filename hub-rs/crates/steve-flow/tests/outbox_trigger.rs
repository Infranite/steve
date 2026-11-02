//! `automation.item_trigger` through the dispatcher: `work.item.done` from the outbox becomes the
//! `item.done` event and its runs once, the receipt says what happened, and an executor that cannot wake
//! a wait in the transaction is refused on this path.

use std::sync::Arc;

use serde_json::{Value, json};
use steve_flow::testkit::FlowEnv;
use steve_flow::{FlowCx, FlowError, ItemTrigger, RunExecutor, RunStatus};
use steve_modules::testkit::{ME, WS, call, caller};
use steve_store::outbox::{self, ConsumeError, Consumer, Dispatcher, Event};

async fn flow(e: &FlowEnv, graph: Value) {
    let (s, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "POST",
        "/flows",
        Some(&json!({"name": "f", "graph": graph})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    let (s, v) = call(
        &e.router(),
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/flows/{id}"),
        Some(&json!({"enabled": true})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
}

fn q<T: rusqlite::types::FromSql>(e: &FlowEnv, sql: &str) -> T {
    e.env
        .store()
        .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn done(e: &FlowEnv, item: &str) -> Event {
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, created, updated) VALUES (?1, ?2, ?3, '写周报', 'done', '', 'steve', 1, 1)",
                rusqlite::params![item, WS, item.trim_start_matches("ST-").parse::<i64>().unwrap()],
            )?;
            Ok(())
        })
        .unwrap();
    e.env
        .store()
        .transaction(|tx| {
            outbox::emit(
                tx,
                WS,
                steve_work_api::ITEM_DONE,
                steve_work_api::ITEM_DONE_V,
                &json!({"item_id": item, "title": "写周报", "status": "done", "bot_id": "steve", "project_id": null, "source": ["对话", "chat"]}),
            )
        })
        .unwrap()
}

const DIGEST: &str = r#"{"nodes": [{"id": "t", "kind": "trigger", "config": {"source": "item", "types": ["item.done"]}},
    {"id": "d", "kind": "action", "config": {"action": "digest", "text": "{{event.payload.item_id}} 做完了"}}],
    "edges": [{"from": "t", "to": "d"}]}"#;

#[tokio::test]
async fn delivered_once_with_its_outcome_on_the_receipt() {
    let e = FlowEnv::new();
    let d = Dispatcher::new(vec![ItemTrigger::new(e.flow.clone())]);
    d.register(e.env.store()).unwrap();
    // nothing listens: received, recorded as such, nothing stored
    done(&e, "ST-1");
    assert_eq!(d.pump(e.env.store(), 1).unwrap().done, 1);
    assert_eq!(
        q::<String>(&e, "SELECT outcome FROM outbox_receipts"),
        "没有流程在听"
    );
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM ext_events"), 0);

    flow(&e, serde_json::from_str(DIGEST).unwrap()).await;
    let ev = done(&e, "ST-2");
    assert_eq!(d.pump(e.env.store(), 2).unwrap().done, 1);
    assert_eq!(d.pump(e.env.store(), 3).unwrap().done, 0);
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM ext_events"), 1);
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM flow_runs"), 1);
    let external: String = q(&e, "SELECT external_id FROM ext_events");
    assert_eq!(external, format!("ST-2:item.done:{}", ev.id));
    let outcome: String = q(
        &e,
        "SELECT outcome FROM outbox_receipts ORDER BY event_seq DESC LIMIT 1",
    );
    assert!(outcome.contains("1 个流程"), "{outcome}");
    for _ in 0..100 {
        if q::<String>(&e, "SELECT status FROM flow_runs") == "done" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        q::<String>(&e, "SELECT status FROM flow_runs"),
        "done",
        "driven after the commit"
    );
}

#[tokio::test]
async fn the_same_fact_twice_is_one_event() {
    let e = FlowEnv::new();
    flow(&e, serde_json::from_str(DIGEST).unwrap()).await;
    let ev = done(&e, "ST-1");
    let c = ItemTrigger::new(e.flow.clone());
    // applied twice without a receipt between (a receipt lost in a restore): the event's id dedupes
    for _ in 0..2 {
        let a = e.env.store().transaction(|tx| c.apply(tx, &ev)).unwrap();
        drop(a.after);
    }
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM ext_events"), 1);
    assert_eq!(q::<i64>(&e, "SELECT count(*) FROM flow_runs"), 1);
}

/// Resumes only after the commit, like an executor from before `wake_in`.
struct AfterCommitOnly;

#[async_trait::async_trait]
impl RunExecutor for AfterCommitOnly {
    async fn drive(&self, _: &FlowCx, _: &str) -> Result<RunStatus, FlowError> {
        Ok(RunStatus::Done)
    }
}

#[tokio::test]
async fn an_executor_that_cannot_wake_in_the_transaction_is_refused() {
    let base = FlowEnv::new();
    let fx = steve_flow::FlowSetup::new(base.core.clone())
        .build(base.env.modules.clone(), Arc::new(AfterCommitOnly));
    base.env
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, started) VALUES ('run_w', 'default', 'flw_w', 1, 'event', 'waiting', 1);
                 INSERT INTO flow_waits (run_id, node_id, workspace_id, mode, match, created) VALUES ('run_w', 'w', 'default', 'event', '{\"source\":\"item\"}', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let ev = done(&base, "ST-1");
    let c = ItemTrigger::new(fx);
    let r = base
        .env
        .store()
        .transaction(|tx| c.apply(tx, &ev).map(|_| ()));
    assert!(matches!(r, Err(ConsumeError::Reject(_))), "{:?}", r.err());
    assert_eq!(
        q::<i64>(&base, "SELECT count(*) FROM ext_events"),
        0,
        "rolled back"
    );
    assert_eq!(
        q::<String>(&base, "SELECT status FROM flow_runs WHERE id = 'run_w'"),
        "waiting"
    );
}
