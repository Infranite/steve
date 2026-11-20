//! SG32: the legacy linear executor's persisted outcome/retry/cancellation boundaries.
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use steve_flow::exec::{MAX_RETRIES, RETRY_AFTER_MS};
use steve_flow::ports::{Core, ItemRequest};
use steve_flow::testkit::{FakeCore, FlowEnv};
use steve_flow::{
    FlowResult, FlowSetup, GraphEngine, LinearExecutor, RunExecutor, RunStatus, engine, flows,
    graph::Graph, runs,
};
use steve_modules::{
    GatewayOut,
    testkit::{ME, WS},
};
const NOW: i64 = 1_795_200_000_000;
fn open(e: &FlowEnv, cfg: Value, cursor: Option<&str>) -> String {
    let g=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"manual"}},{"id":"a","kind":"action","config":cfg}],"edges":[{"from":"t","to":"a"}]})).unwrap();
    e.env
        .store()
        .transaction(|tx| -> FlowResult<_> {
            let id = flows::create(
                tx,
                WS,
                ME,
                &flows::NewFlow {
                    kind: "flow",
                    name: "SG32 linear",
                    description: "",
                    graph: &g,
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
                json!({"at":NOW,"steps":{},"label_prefix":"prefix:"}),
                None,
                false,
                None,
                0,
                NOW,
            )?;
            let mut r = runs::get(tx, &id)?.unwrap();
            r.cursor = cursor.map(str::to_owned);
            r.cost = 2.0;
            r.retry_at = Some(NOW - 1);
            runs::save(tx, &r)?;
            Ok(id)
        })
        .unwrap()
}
fn row(e: &FlowEnv, id: &str) -> runs::RunRow {
    e.env.store().reader(|c| runs::get(c, id)).unwrap().unwrap()
}
fn steps(e: &FlowEnv, id: &str) -> Vec<(String, String, Option<String>)> {
    e.env
        .store()
        .reader(|c| {
            let mut s = c.prepare(
                "SELECT status,output,error FROM flow_run_steps WHERE run_id=? ORDER BY seq",
            )?;
            Ok::<_, steve_store::StoreError>(
                s.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?,
            )
        })
        .unwrap()
}
#[tokio::test]
async fn sg32_linear_result_and_retry_boundary_table() {
    for (mode, attempts, want) in [
        ("done", 0, RunStatus::Done),
        ("retry", 0, RunStatus::Waiting),
        ("retry", MAX_RETRIES, RunStatus::Failed),
        ("fail", 0, RunStatus::Failed),
    ] {
        let e = FlowEnv::new();
        e.env.clock.set(NOW);
        let cfg = if mode == "fail" {
            json!({"action":"unknown-sg32"})
        } else {
            json!({"action":"report","what":"fixture"})
        };
        if mode == "retry" {
            e.env.gateway.fail("fixture offline");
        } else {
            e.env.gateway.script_out(
                "",
                GatewayOut {
                    text: "fixture report".into(),
                    cost_usd: Some(0.25),
                    ..Default::default()
                },
            );
        }
        let id = open(&e, cfg, Some("a"));
        e.env
            .store()
            .transaction(|tx| {
                let mut r = runs::get(tx, &id)?.unwrap();
                r.attempts = attempts;
                runs::save(tx, &r)
            })
            .unwrap();
        assert_eq!(
            LinearExecutor.drive(&e.flow, &id).await.unwrap(),
            want,
            "{mode}/{attempts}: {:?}",
            row(&e, &id)
        );
        let r = row(&e, &id);
        let s = steps(&e, &id);
        assert_eq!(s.len(), 1);
        if mode == "done" {
            assert_eq!(r.cost, 2.25);
            assert_eq!(r.outcome.as_deref(), Some("report"));
            assert_eq!(r.outcome_label, "prefix:已发布到信息流");
            assert_eq!(r.cursor, None);
            assert_eq!(r.error, None);
            assert_eq!(r.ended, Some(NOW));
            assert_eq!(s[0].0, "ok");
            assert_eq!(
                serde_json::from_str::<Value>(&s[0].1).unwrap()["text"],
                "fixture report"
            );
        } else {
            assert_eq!(r.cost, 2.0);
            assert_eq!(s[0].0, "failed");
            assert_eq!(s[0].1, "null");
            assert!(r.error.is_some());
            if want == RunStatus::Waiting {
                assert_eq!(r.retry_at, Some(NOW + RETRY_AFTER_MS));
                assert_eq!(r.attempts, 1);
                assert_eq!(r.ended, None);
            } else {
                assert_eq!(r.ended, Some(NOW));
            }
        }
    }
}
#[tokio::test]
async fn sg32_linear_nonrunning_empty_cursor_and_unsupported_kind() {
    let e = FlowEnv::new();
    e.env.clock.set(NOW);
    let id = open(&e, json!({"action":"report"}), Some("a"));
    e.env
        .store()
        .transaction(|tx| {
            let mut r = runs::get(tx, &id)?.unwrap();
            r.status = "cancelled".into();
            runs::save(tx, &r)
        })
        .unwrap();
    assert_eq!(
        LinearExecutor.drive(&e.flow, &id).await.unwrap(),
        RunStatus::Cancelled
    );
    assert!(steps(&e, &id).is_empty());
    assert!(e.env.gateway.prompts.lock().unwrap().is_empty());
    let id = open(&e, json!({}), None);
    assert_eq!(
        LinearExecutor.drive(&e.flow, &id).await.unwrap(),
        RunStatus::Done
    );
    assert!(steps(&e, &id).is_empty());
    assert_eq!(row(&e, &id).outcome_label, "prefix:");
    let id = open(&e, json!({}), Some("t"));
    assert_eq!(
        LinearExecutor.drive(&e.flow, &id).await.unwrap(),
        RunStatus::Failed
    );
    assert!(
        row(&e, &id)
            .error
            .unwrap()
            .contains("定时任务不支持「trigger」节点")
    );
    assert_eq!(steps(&e, &id)[0].0, "failed");
}
struct CancelCore {
    base: Arc<FakeCore>,
    store: steve_store::Store,
    run: Mutex<Option<String>>,
}
#[async_trait]
impl Core for CancelCore {
    async fn create_item(&self, r: ItemRequest) -> Result<String, String> {
        self.base.create_item(r).await
    }
    async fn pause_item(&self, w: &str, i: &str) -> Result<(), String> {
        self.base.pause_item(w, i).await
    }
    async fn resume_item(&self, w: &str, i: &str) -> Result<(), String> {
        self.base.resume_item(w, i).await
    }
    async fn post_steve_message(&self, w: &str, t: &str, m: Value) -> Result<(), String> {
        self.base.post_steve_message(w, t, m).await
    }
    async fn digest_text(&self, w: &str) -> Result<String, String> {
        if let Some(id) = self.run.lock().unwrap().take() {
            self.store
                .transaction(|tx| {
                    let r = runs::get(tx, &id)?.unwrap();
                    engine::cancel(tx, &r, NOW - 5)
                })
                .unwrap();
        }
        self.base.digest_text(w).await
    }
}
#[tokio::test]
async fn sg32_linear_cancellation_during_action_preserves_terminal_state_and_records_cost() {
    let e = FlowEnv::new();
    e.env.clock.set(NOW);
    e.env.gateway.script_out(
        "",
        GatewayOut {
            text: "report after cancellation".into(),
            cost_usd: Some(0.25),
            ..Default::default()
        },
    );
    let id = open(&e, json!({"action":"report"}), Some("a"));
    let core = Arc::new(CancelCore {
        base: e.core.clone(),
        store: e.env.store().clone(),
        run: Mutex::new(Some(id.clone())),
    });
    let cx = FlowSetup::new(core).build(e.env.modules.clone(), Arc::new(GraphEngine));
    assert_eq!(
        LinearExecutor.drive(&cx, &id).await.unwrap(),
        RunStatus::Cancelled
    );
    let r = row(&e, &id);
    assert_eq!(r.status, "cancelled");
    assert_eq!(r.ended, Some(NOW - 5));
    assert_eq!(r.outcome, None);
    assert_eq!(r.cost, 2.25, "{r:?}");
    assert_eq!(steps(&e, &id).len(), 1);
}
