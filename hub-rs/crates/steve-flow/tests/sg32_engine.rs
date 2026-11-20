//! SG32: after-drive event settlement precedes parent recovery, with real persisted waits.
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use steve_flow::adapter::Ingested;
use steve_flow::nodes::{NodeKind, NodeOutcome, RunCx, WaitSpec};
use steve_flow::testkit::{FakeEventAdapter, FlowEnv};
use steve_flow::{FlowResult, RunStatus, engine, flows, graph::Graph, ingest, runs};
use steve_modules::testkit::{ME, WS};
const NOW: i64 = 1_795_200_000_000;
#[derive(Default)]
struct Seen {
    event: Mutex<String>,
    observations: Mutex<Vec<(String, Value)>>,
}
struct Probe {
    kind: &'static str,
    seen: Arc<Seen>,
}
#[async_trait]
impl NodeKind for Probe {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn validate(&self, _: &Value) -> Result<(), String> {
        Ok(())
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        match self.kind {
            "sg32_parent_wait" => NodeOutcome::Wait(Box::new(WaitSpec {
                mode: "flow",
                until: None,
                matcher: json!({"child":input["child"]}),
                decision: None,
                output: json!({"waiting":true}),
            })),
            "sg32_observe" => {
                let id = self.seen.event.lock().unwrap().clone();
                let outcome = cx
                    .flow
                    .store()
                    .reader(|c| {
                        Ok::<_, steve_store::StoreError>(c.query_row(
                            "SELECT outcome FROM ext_events WHERE id=?",
                            [id],
                            |r| r.get::<_, String>(0),
                        )?)
                    })
                    .unwrap();
                self.seen
                    .observations
                    .lock()
                    .unwrap()
                    .push((outcome, input.clone()));
                NodeOutcome::next(input.clone())
            }
            _ => {
                if cfg["fail"] == true {
                    return NodeOutcome::Fail("child fixture failure".into());
                }
                cx.effects.outcome = Some("item".into());
                cx.effects.label = Some("child handled".into());
                NodeOutcome::next(json!({"child_result":7}))
            }
        }
    }
}
fn open(
    e: &FlowEnv,
    name: &str,
    g: &Graph,
    cursor: &str,
    event: Option<&str>,
    parent: Option<&str>,
) -> String {
    e.env
        .store()
        .transaction(|tx| -> FlowResult<_> {
            let id = flows::create(
                tx,
                WS,
                ME,
                &flows::NewFlow {
                    kind: "flow",
                    name,
                    description: "",
                    graph: g,
                    origin: "me",
                    origin_label: "",
                    enabled: false,
                },
                NOW,
            )?;
            let f = flows::own(tx, WS, &id)?;
            let id = engine::open_run(
                tx,
                &f,
                "manual",
                json!({"input":{},"steps":{}}),
                event,
                false,
                parent,
                0,
                NOW,
            )?;
            let mut r = runs::get(tx, &id)?.unwrap();
            r.cursor = Some(cursor.into());
            runs::save(tx, &r)?;
            Ok(id)
        })
        .unwrap()
}
fn row(e: &FlowEnv, id: &str) -> runs::RunRow {
    e.env.store().reader(|c| runs::get(c, id)).unwrap().unwrap()
}
async fn setup(fail: bool) -> (FlowEnv, Arc<Seen>, String, String, String) {
    let seen = Arc::new(Seen::default());
    let a = Arc::new(FakeEventAdapter::push());
    let e = FlowEnv::with(
        vec![a],
        vec![
            Arc::new(Probe {
                kind: "sg32_parent_wait",
                seen: seen.clone(),
            }),
            Arc::new(Probe {
                kind: "sg32_observe",
                seen: seen.clone(),
            }),
            Arc::new(Probe {
                kind: "sg32_child",
                seen: seen.clone(),
            }),
        ],
        vec![],
    );
    e.env.clock.set(NOW);
    let conn = e.connection(WS, FakeEventAdapter::ID, Some(ME), ME);
    let ev = FakeEventAdapter::event("mail.new", "sg32-after", "INBOX", "fixture");
    let plan = e
        .env
        .store()
        .transaction(|tx| ingest::ingest_in(tx, &e.flow, WS, &conn, &ev, NOW))
        .unwrap();
    let event = match plan.outcome() {
        Ingested::Stored { id, .. } => id,
        x => panic!("{x:?}"),
    };
    *seen.event.lock().unwrap() = event.clone();
    let parent_graph=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"manual"}},{"id":"w","kind":"sg32_parent_wait","config":{}},{"id":"o","kind":"sg32_observe","config":{}}],"edges":[{"from":"t","to":"w"},{"from":"w","to":"o"}]})).unwrap();
    let parent = open(&e, "parent", &parent_graph, "w", None, None);
    let child_graph=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"manual"}},{"id":"p","kind":"sg32_child","config":{"fail":fail}}],"edges":[{"from":"t","to":"p"}]})).unwrap();
    let child = open(&e, "child", &child_graph, "p", Some(&event), Some(&parent));
    e.env
        .store()
        .transaction(|tx| {
            let mut r = runs::get(tx, &parent)?.unwrap();
            r.context["input"] = json!({"child":child});
            runs::save(tx, &r)
        })
        .unwrap();
    (e, seen, event, parent, child)
}
#[tokio::test]
async fn sg32_after_settles_before_resuming_and_driving_the_parent() {
    let (e, seen, event, parent, child) = setup(false).await;
    assert_eq!(
        engine::drive(&e.flow, &parent).await.unwrap(),
        RunStatus::Waiting
    );
    assert_eq!(
        engine::drive(&e.flow, &child).await.unwrap(),
        RunStatus::Done
    );
    assert_eq!(row(&e, &parent).status, "done");
    assert_eq!(
        *seen.observations.lock().unwrap(),
        [("item".into(), json!({"child_result":7}))]
    );
    let result: String = e
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT outcome FROM ext_events WHERE id=?",
                [event],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(result, "item");
}
#[tokio::test]
async fn sg32_after_child_terminal_status_and_parent_guards() {
    for mode in [
        "failed-child",
        "cancelled-child",
        "cancelled-parent",
        "missing-wait",
    ] {
        let (e, seen, _, parent, child) = setup(mode == "failed-child").await;
        if mode != "missing-wait" {
            assert_eq!(
                engine::drive(&e.flow, &parent).await.unwrap(),
                RunStatus::Waiting
            );
        } else {
            e.env
                .store()
                .transaction(|tx| {
                    let mut r = runs::get(tx, &parent)?.unwrap();
                    r.status = "waiting".into();
                    runs::save(tx, &r)
                })
                .unwrap();
        }
        if mode == "cancelled-parent" {
            e.env
                .store()
                .transaction(|tx| engine::cancel(tx, &runs::get(tx, &parent)?.unwrap(), NOW))
                .unwrap();
        }
        if mode == "cancelled-child" {
            e.env
                .store()
                .transaction(|tx| engine::cancel(tx, &runs::get(tx, &child)?.unwrap(), NOW))
                .unwrap();
        }
        engine::drive(&e.flow, &child).await.unwrap();
        let p = row(&e, &parent);
        assert_eq!(
            p.status,
            match mode {
                "cancelled-parent" => "cancelled",
                "missing-wait" => "waiting",
                _ => "failed",
            },
            "{mode}"
        );
        assert!(seen.observations.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn sg32_after_missing_run_and_dry_event_do_not_settle_real_triage() {
    let (e, _, event, _, child) = setup(false).await;
    e.env
        .store()
        .transaction(|tx| {
            let mut r = runs::get(tx, &child)?.unwrap();
            // Exercise after-drive only: a completed dry run must leave real event triage alone.
            r.status = "done".into();
            r.dry_run = true;
            r.parent_run = None;
            runs::save(tx, &r)
        })
        .unwrap();
    engine::drive(&e.flow, &child).await.unwrap();
    let result: String = e
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT outcome FROM ext_events WHERE id=?",
                [event],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(result, "recorded");
    assert!(engine::drive(&e.flow, "absent-sg32").await.is_err());
}
