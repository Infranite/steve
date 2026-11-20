use std::sync::{Arc, Mutex};

use steve_hub::wire::node_runtime::{AttemptFacts, Binding, NodeDomain, NodeRuntime, Sink};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_runtime::{FakeClock, testkit::eventually_sync};
use steve_store::{NoPublisher, Store, StoreError};

pub const SESSION: &str = "sg36-session";

#[derive(Default)]
pub struct Domain {
    pub facts: Mutex<Vec<AttemptFacts>>,
}
impl NodeDomain for Domain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
    fn attempt_finished(&self, _: &str, _: &str, _: &Completion, facts: &AttemptFacts, _: i64) {
        self.facts.lock().unwrap().push(facts.clone());
    }
}

pub struct Hub {
    pub at: i64,
    pub rt: Arc<NodeRuntime>,
    pub node: Arc<FakeNode>,
    pub store: Store,
    pub domain: Arc<Domain>,
    _dir: tempfile::TempDir,
}
impl Hub {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("sg36.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store.transaction::<_, StoreError>(|tx| {
            tx.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)", [SESSION])?;
            Ok(())
        }).unwrap();
        // Command payloads use wall-clock deadlines; freeze both peers at creation time.
        let at = steve_store::now();
        let node = FakeNode::new("sg36-node");
        node.set_now(at);
        let domain = Arc::new(Domain::default());
        let rt = NodeRuntime::with_clock(
            store.clone(),
            node.clone(),
            domain.clone(),
            Arc::new(FakeClock::new(at)),
        );
        node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
        rt.start();
        eventually_sync(
            "the coordinator finished its initial reconciliation",
            || rt.is_reconciled(),
        )
        .await;
        Self {
            at,
            rt,
            node,
            store,
            domain,
            _dir: dir,
        }
    }
    pub fn bind(&self) {
        self.rt
            .bind(&Binding {
                session: SESSION.into(),
                resource: format!("item:{SESSION}"),
                prepare: PrepareKind::Scratch,
                choice: build::choice(),
                policy: build::policy(),
            })
            .unwrap();
    }
    pub fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
    pub fn phase(&self) -> String {
        self.q("SELECT phase FROM runtime_sessions")
    }
    pub fn state(&self, method: &str) -> String {
        self.store.reader(|c| Ok::<_, StoreError>(c.query_row(
            "SELECT state FROM node_commands WHERE method=? ORDER BY created DESC,rowid DESC LIMIT 1",
            [method], |r| r.get(0),
        )?)).unwrap()
    }
    pub fn count(&self, method: &str) -> usize {
        self.node.log().iter().filter(|(m, _)| *m == method).count()
    }
    pub async fn ready(&self) {
        eventually_sync("the prepared session is ready for input", || {
            self.phase() == "ready"
        })
        .await;
    }
    pub async fn turn(&self) -> ExecutionRef {
        self.rt
            .prompt(SESSION, "fixture input", "user", &[])
            .unwrap();
        eventually_sync("the Node started the planned attempt", || {
            self.q::<i64>("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 1
        })
        .await;
        let execution: String = self
            .q("SELECT execution FROM runtime_attempts ORDER BY created DESC,rowid DESC LIMIT 1");
        serde_json::from_str(&execution).unwrap()
    }
    pub async fn emit(&self, execution: &ExecutionRef, body: EventBody) {
        self.node
            .emit(Subject::Execution(execution.clone()), body)
            .await
            .unwrap();
        // Wait on the durable apply watermark, including events that intentionally add no units.
        eventually_sync("all accepted fixture events reached ordered apply", || {
            self.q::<i64>("SELECT COUNT(*) FROM node_streams WHERE durable>applied") == 0
        })
        .await;
    }
    pub async fn finish(&self, execution: &ExecutionRef) {
        let before = self.domain.facts.lock().unwrap().len();
        self.node.finish(execution, build::done()).await.unwrap();
        eventually_sync(
            "the finished attempt published its committed accounting facts",
            || self.domain.facts.lock().unwrap().len() == before + 1,
        )
        .await;
    }
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.rt.stop("fixture finished");
    }
}
