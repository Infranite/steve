//! End-to-end graph fixtures: the real Core, routed runtime and durable Node coordinator, with
//! the protocol's strict in-memory Node in place of a harness (no model, login or service needed).
use super::{
    core::{HubHost, RoutedRuntime},
    host_node::ServiceNode,
    node_runtime::{NodeRuntime, Sink},
    workspaces::{LocalFace, Workspaces},
};
use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};
use steve_core::{AgentRuntime, Check, Core, CoreConfig, Output};
use steve_node_api::{
    ExecutionRef, NodeId,
    testkit::{FakeNode, build},
};
use steve_store::{
    NewBot, NewItem, NewSession, NoPublisher, Store, StoreError, UnitSpec, WS, WorkUnit,
};

struct NoModels;
#[async_trait::async_trait]
impl steve_core::ModelGateway for NoModels {
    async fn call(
        &self,
        _: &str,
        _: steve_core::Tier,
        _: Duration,
        _: &steve_core::gateway::ThinkTools,
    ) -> steve_core::Result<steve_core::gateway::Printed> {
        panic!("this workflow fixture must not call a model")
    }
}
struct Fixture {
    core: Core,
    routed: Arc<RoutedRuntime>,
    node: Arc<FakeNode>,
    store: Store,
}
impl Fixture {
    fn attach(dir: &Path, store: Store, node: Arc<FakeNode>) -> Self {
        let host = Arc::new(HubHost {
            store: store.clone(),
            data_dir: dir.join("data"),
            core: Default::default(),
            flow: Default::default(),
            yolo: Default::default(),
            local_node: Default::default(),
        });
        let local = ServiceNode::new(NodeId::new("local").unwrap(), node.clone(), dir);
        assert!(host.local_node.set(local.clone()).is_ok());
        let runtime = NodeRuntime::new(store.clone(), node.clone(), host.clone());
        node.connect(Arc::new(Sink(Arc::downgrade(&runtime))));
        let cfg = CoreConfig::for_home(dir);
        let workspaces = Workspaces::new(
            store.clone(),
            Some(LocalFace {
                node: local,
                runtime: runtime.clone(),
            }),
            None,
            vec![cfg.scratch.clone(), cfg.worktrees.clone()],
        );
        let routed = Arc::new(RoutedRuntime {
            node: Some(runtime.clone()),
            fleet: None,
            bind_remote: None,
            spread: false,
            store: store.clone(),
            host: host.clone(),
            workspaces,
            worktrees: Some(cfg.worktrees.clone()),
        });
        let core = Core::new(
            steve_core::repo::Db::new(store.clone()),
            routed.clone(),
            Arc::new(NoModels),
            cfg,
        );
        assert!(host.core.set(core.clone()).is_ok());
        runtime.start();
        Self {
            core,
            routed,
            node,
            store,
        }
    }
    fn new(dir: &Path) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("steve_hub=debug")
            .with_test_writer()
            .try_init();
        let store = Store::open(dir.join("hub.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        for id in ["steve", "writer", "reviewer"] {
            store
                .bots()
                .seed(
                    id,
                    id == "steve",
                    &NewBot {
                        name: id.into(),
                        harness: build::choice().harness,
                        ..Default::default()
                    },
                    WS,
                )
                .unwrap();
        }
        sql(&store, "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('requester','default','requester','Requester','member',1),('other','default','other','Other','member',1);
            INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','requester','member',1),('default','other','member',1);");
        Self::attach(dir, store, FakeNode::new("local"))
    }
    fn parent(&self, requester: Option<&str>) -> String {
        self.store
            .items()
            .create(
                WS,
                &NewItem {
                    title: "Test graph".into(),
                    goal: "Complete, review and integrate".into(),
                    bot_id: "writer".into(),
                    requested_by: requester.map(str::to_string),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
            .id
    }
    fn specs() -> Vec<UnitSpec> {
        [
            ("work", "writer", "work", vec![]),
            ("review", "reviewer", "review", vec!["work".into()]),
            ("integrate", "writer", "integrate", vec!["review".into()]),
        ]
        .into_iter()
        .map(|(id, bot, kind, depends_on)| UnitSpec {
            id: id.into(),
            title: id.into(),
            goal: format!("Complete {id}"),
            bot_id: bot.into(),
            kind: kind.into(),
            depends_on,
            resources: vec![],
            required: true,
        })
        .collect()
    }
    fn plan(&self, parent: &str) {
        let view = self.core.workflow_change(parent, json!({"action":"plan","reason":"Split work","expected_revision":0,"units":Self::specs(),"max_parallel":1}), "requester").unwrap();
        assert_eq!(view["execution_ready"], true, "{view}");
    }
    fn unit(&self, parent: &str, id: &str) -> WorkUnit {
        self.store
            .workflows()
            .get(parent)
            .unwrap()
            .unwrap()
            .units
            .into_iter()
            .find(|u| u.spec.id == id)
            .unwrap()
    }
    async fn execution(&self, parent: &str, id: &str) -> ExecutionRef {
        until(|| {
            self.unit(parent, id)
                .session_id
                .as_deref()
                .is_some_and(|s| {
                    self.routed
                        .node
                        .as_ref()
                        .unwrap()
                        .active_attempt(s)
                        .unwrap()
                        .is_some()
                })
        })
        .await;
        let sid = self.unit(parent, id).session_id.unwrap();
        until(|| value::<i64>(&self.store, &format!("SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id='{sid}' AND a.state='running'"))==1).await;
        let raw: String = self.store.reader(|c| Ok::<_,StoreError>(c.query_row("SELECT a.execution FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id=? AND a.state='running'",[sid],|r|r.get(0))?)).unwrap();
        serde_json::from_str(&raw).unwrap()
    }
    async fn deliver(&self, parent: &str, id: &str) {
        let execution = self.execution(parent, id).await;
        let u = self.unit(parent, id);
        let session = self
            .store
            .sessions()
            .get(u.session_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        self.core
            .tool_deliver(
                &session,
                "Fixture result verified",
                &[Output {
                    kind: "file".into(),
                    title: format!("{id} result"),
                    location: Some(format!("{id}.txt")),
                    role: None,
                }],
                &[Check {
                    name: "Fixture operation".into(),
                    passed: true,
                    operated: true,
                }],
                &[],
            )
            .unwrap();
        self.node.finish(&execution, build::done()).await.unwrap();
        until(|| {
            self.store
                .items()
                .get(u.item_id.as_deref().unwrap())
                .unwrap()
                .unwrap()
                .phase
                == "closed"
        })
        .await;
    }
}
fn sql(store: &Store, text: &str) {
    store.with_conn(|c| Ok(c.execute_batch(text)?)).unwrap();
}
fn value<T: rusqlite::types::FromSql>(store: &Store, query: &str) -> T {
    store
        .reader(|c| Ok::<_, StoreError>(c.query_row(query, [], |r| r.get(0))?))
        .unwrap()
}
async fn until(f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("workflow fixture timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_principal_graph_runs_through_node_dependencies_review_and_acceptance() {
    for requester in [Some("requester"), None] {
        let dir = tempfile::tempdir().unwrap();
        let f = Fixture::new(dir.path());
        let parent = f.parent(requester);
        f.plan(&parent);
        assert_eq!(f.unit(&parent, "review").status, "pending");
        let work = f.unit(&parent, "work");
        let sid = work.session_id.as_deref().unwrap();
        let binding: String = f
            .store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT principal FROM runtime_sessions WHERE session_id=?",
                    [sid],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(binding, requester.unwrap_or("svc:default"));
        let request: String = value(
            &f.store,
            "SELECT request FROM node_commands WHERE method='open_session' LIMIT 1",
        );
        let open: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(open["body"]["policy"]["sandbox"], json!({"type":"none"}));
        for id in ["work", "review", "integrate"] {
            f.deliver(&parent, id).await;
        }
        assert!(
            f.store
                .workflows()
                .get(&parent)
                .unwrap()
                .unwrap()
                .units
                .iter()
                .all(|u| u.status == "succeeded")
        );
        assert_eq!(
            value::<i64>(&f.store, "SELECT COUNT(*) FROM runtime_turns"),
            3
        );
        assert_eq!(
            f.node
                .log()
                .iter()
                .filter(|(m, _)| *m == "start_turn")
                .count(),
            3
        );
        f.routed.node.as_ref().unwrap().stop("fixture complete");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_adopts_unbound_session_and_retry_does_not_duplicate_children_or_turns() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let parent = f.parent(Some("requester"));
    f.store
        .workflows()
        .replace(&parent, 0, "Saved plan", &Fixture::specs(), 1, 3)
        .unwrap();
    let claimed = f.store.workflows().claim(&parent, "work").unwrap();
    let session = f
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: claimed.item_id.clone(),
            bot_id: Some("writer".into()),
            harness: build::choice().harness,
            ..Default::default()
        })
        .unwrap();
    f.routed
        .node
        .as_ref()
        .unwrap()
        .stop("simulated Hub restart before Node bind");
    let recovered = Fixture::attach(dir.path(), f.store.clone(), f.node.clone());
    recovered.core.lifecycle_tick().unwrap();
    let execution = recovered.execution(&parent, "work").await;
    assert_eq!(
        recovered.unit(&parent, "work").session_id.as_deref(),
        Some(session.id.as_str())
    );
    assert_eq!(
        recovered
            .store
            .sessions()
            .of_item(claimed.item_id.as_deref().unwrap())
            .unwrap()
            .len(),
        1
    );
    // Reconciliation sees the Node's active turn after another restart: no second execution.
    recovered
        .routed
        .node
        .as_ref()
        .unwrap()
        .stop("second restart during turn");
    let again = Fixture::attach(dir.path(), recovered.store.clone(), recovered.node.clone());
    until(|| again.routed.is_busy(&session.id)).await;
    again.core.lifecycle_tick().unwrap();
    assert_eq!(
        value::<i64>(&again.store, "SELECT COUNT(*) FROM runtime_turns"),
        1
    );
    again
        .core
        .tool_workflow(
            &session,
            json!({"action":"fail","reason":"Review requests a retry"}),
        )
        .unwrap();
    again.node.finish(&execution, build::done()).await.unwrap();
    // Turn completion briefly leaves the runtime ready before Core closes the failed child and
    // queues its release. Wait for that release, not the transient idle gap before it starts.
    until(|| {
        again
            .store
            .reader(|c| steve_store::runtime::released_idle(c, &session.id))
            .unwrap()
            && !again.routed.uncertain(&session.id)
    })
    .await;
    let revision = again
        .store
        .workflows()
        .get(&parent)
        .unwrap()
        .unwrap()
        .revision;
    again.core.workflow_change(&parent,json!({"action":"retry","expected_revision":revision,"unit_id":"work","reason":"Apply review"}),"requester").unwrap();
    let next = again.unit(&parent, "work");
    assert_ne!(next.item_id, claimed.item_id);
    assert_eq!(next.attempt, 2);
    again.execution(&parent, "work").await;
    for _ in 0..3 {
        again.core.lifecycle_tick().unwrap();
    }
    assert_eq!(
        value::<i64>(&again.store, "SELECT COUNT(*) FROM runtime_turns"),
        2
    );
    assert!(
        again
            .routed
            .prompt(&session.id, "stale retry", "user", &[])
            .is_err()
    );
    again.routed.node.as_ref().unwrap().stop("fixture complete");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_and_queued_followups_refuse_unknown_changed_principals_bots_and_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let parent = f.parent(Some("unknown"));
    f.store
        .workflows()
        .replace(&parent, 0, "Saved plan", &Fixture::specs(), 1, 3)
        .unwrap();
    assert!(
        f.routed
            .check_workflow_execution(&parent)
            .unwrap_err()
            .to_string()
            .contains("主体")
    );
    f.core.lifecycle_tick().unwrap();
    assert!(f.unit(&parent, "work").item_id.is_none());
    sql(&f.store, "UPDATE items SET requested_by='requester'");
    f.core.lifecycle_tick().unwrap();
    let execution = f.execution(&parent, "work").await;
    let u = f.unit(&parent, "work");
    let sid = u.session_id.unwrap();
    f.routed
        .prompt(&sid, "Queued followup", "user", &[])
        .unwrap();
    for (change,restore) in [
        (format!("UPDATE items SET requested_by='other' WHERE id='{}'",u.item_id.as_deref().unwrap()),format!("UPDATE items SET requested_by='requester' WHERE id='{}'",u.item_id.as_deref().unwrap())),
        ("UPDATE runtime_sessions SET principal='other'".into(),"UPDATE runtime_sessions SET principal='requester'".into()),
        ("UPDATE bots SET enabled=0 WHERE id='writer'".into(),"UPDATE bots SET enabled=1 WHERE id='writer'".into()),
        ("DELETE FROM workspace_members WHERE user_id='requester'".into(),"INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','requester','member',1)".into()),
    ] {
        sql(&f.store,&change);
        assert!(f.routed.check_workflow_execution(&parent).is_err(),"{change}");
        assert!(f.routed.prompt(&sid,"Must refuse","user",&[]).is_err(),"{change}");
        assert_eq!(f.routed.node.as_ref().unwrap().flush(&sid,None).unwrap(),super::node_runtime::Planned::Held,"{change}");
        sql(&f.store,&restore);
    }
    // A membership change after queueing also holds the Node's own fact-driven planner.
    sql(
        &f.store,
        "DELETE FROM workspace_members WHERE user_id='requester'",
    );
    f.node.finish(&execution, build::done()).await.unwrap();
    until(|| {
        f.routed
            .node
            .as_ref()
            .unwrap()
            .active_attempt(&sid)
            .unwrap()
            .is_none()
    })
    .await;
    assert_eq!(
        value::<i64>(&f.store, "SELECT COUNT(*) FROM runtime_turns"),
        1
    );
    assert_eq!(value::<i64>(&f.store, "SELECT COUNT(*) FROM queued"), 1);
    sql(
        &f.store,
        "INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','requester','member',1)",
    );
    f.routed.node.as_ref().unwrap().stop("Node unavailable");
    assert!(f.routed.check_workflow_execution(&parent).is_err());
    assert!(
        f.routed
            .prompt(&sid, "No Node fallback", "user", &[])
            .is_err()
    );
}
