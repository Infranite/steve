//! `automation.item_trigger` through the dispatcher: `work.item.done` from the outbox becomes the
//! `item.done` event and its runs once, the receipt says what happened, and an executor that cannot wake
//! a wait in the transaction is refused on this path.

use std::sync::Arc;

use serde_json::{Value, json};
use steve_flow::testkit::FlowEnv;
use steve_flow::{FlowCx, FlowError, ItemTrigger, RunExecutor, RunStatus};
use steve_modules::testkit::{ME, WS, call, caller};
use steve_store::outbox::{self, ConsumeError, Consumer, Dispatcher, Event};
use steve_store::{SignalRows, testkit::flow as fixtures};

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

fn done(e: &FlowEnv, item: &str) -> Event {
    fixtures::fixture_item(
        e.env.store(),
        WS,
        item,
        item.trim_start_matches("ST-").parse().unwrap(),
        "写周报",
        "done",
    )
    .unwrap();
    e.env
        .store()
        .transaction(|tx| {
            outbox::emit(
                tx,
                WS,
                steve_work_api::ITEM_DONE,
                steve_work_api::ITEM_DONE_V,
                &json!({"item_id": item, "title": "写周报", "status": "done", "bot_id": "steve", "project_id": null, "source": ["对话", "chat"], "cycle": 1, "revision": 1}),
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
        fixtures::latest_receipt(e.env.store()).unwrap(),
        "没有流程在听"
    );
    assert_eq!(fixtures::event_ids(e.env.store()).unwrap().len(), 0);

    flow(&e, serde_json::from_str(DIGEST).unwrap()).await;
    let ev = done(&e, "ST-2");
    assert_eq!(d.pump(e.env.store(), 2).unwrap().done, 1);
    assert_eq!(d.pump(e.env.store(), 3).unwrap().done, 0);
    assert_eq!(fixtures::event_ids(e.env.store()).unwrap().len(), 1);
    assert_eq!(fixtures::run_count(e.env.store()).unwrap(), 1);
    let id = fixtures::event_ids(e.env.store()).unwrap().remove(0);
    let external: String = e
        .env
        .store()
        .read::<_, steve_store::StoreError>(|r| {
            Ok(SignalRows(r).event(&id)?.unwrap().external_id.unwrap())
        })
        .unwrap();
    assert_eq!(external, format!("ST-2:item.done:{}", ev.id));
    let outcome: String = fixtures::latest_receipt(e.env.store()).unwrap();
    assert!(outcome.contains("1 个流程"), "{outcome}");
    steve_runtime::testkit::eventually_sync("the triggered run finishes", || {
        fixtures::only_run(e.env.store()).unwrap().status == "done"
    })
    .await;
    assert_eq!(
        fixtures::only_run(e.env.store()).unwrap().status,
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
        let a = e.env.store().write(|tx| c.apply(tx, &ev)).unwrap();
        drop(a.after);
    }
    assert_eq!(fixtures::event_ids(e.env.store()).unwrap().len(), 1);
    assert_eq!(fixtures::run_count(e.env.store()).unwrap(), 1);
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
    let e = FlowEnv::new();
    let fx = steve_flow::FlowSetup::new(e.core.clone())
        .build(e.env.modules.clone(), Arc::new(AfterCommitOnly));
    fixtures::waiting_for_item(e.env.store()).unwrap();
    let ev = done(&e, "ST-1");
    let c = ItemTrigger::new(fx);
    let r = e.env.store().write(|tx| c.apply(tx, &ev).map(|_| ()));
    assert!(matches!(r, Err(ConsumeError::Reject(_))), "{:?}", r.err());
    assert_eq!(
        fixtures::event_ids(e.env.store()).unwrap().len(),
        0,
        "rolled back"
    );
    assert_eq!(e.run_row("run_w").status, "waiting");
}
