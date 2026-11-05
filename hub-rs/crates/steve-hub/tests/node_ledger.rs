//! The ledger of workspace operations that must not run twice (exec, git_apply), against the fake
//! Node: written before it is sent, sent once, and never sent blind after an uncertain send.

use std::sync::Arc;
use std::time::Duration;

use steve_hub::wire::node_runtime::{AttemptFacts, NodeDomain, NodeRuntime, Sink};
use steve_node_api::testkit::{FakeNode, Scripted, build};
use steve_node_api::*;
use steve_store::{Store, StoreError};

struct Quiet;

impl NodeDomain for Quiet {
    fn turn_ended(&self, _: &str, _: &str, _: Option<String>) {}
    fn attempt_started(&self, _: &str, _: &str, _: i64, _: Option<i64>, _: i64) {}
    fn attempt_finished(&self, _: &str, _: &str, _: &Completion, _: &AttemptFacts, _: i64) {}
}

struct H {
    _dir: tempfile::TempDir,
    store: Store,
    node: Arc<FakeNode>,
}

const RESOURCE: &str = "project:p1";

async fn hub() -> H {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    let node = FakeNode::new("n1");
    node.prepare_workspace(build::cmd("op_w", build::prepare(RESOURCE)))
        .await
        .unwrap();
    H {
        _dir: dir,
        store,
        node,
    }
}

impl H {
    /// A coordinator on the store and the Node, not started (a Hub that has not booted yet).
    fn coordinator(&self) -> Arc<NodeRuntime> {
        let rt = NodeRuntime::new(self.store.clone(), self.node.clone(), Arc::new(Quiet));
        self.node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
        rt
    }

    fn execs(&self) -> usize {
        self.node
            .log()
            .into_iter()
            .filter(|(m, _)| *m == "exec")
            .count()
    }

    fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
}

fn spec(command: &str) -> ExecSpec {
    ExecSpec {
        workspace: build::workspace("n1", RESOURCE),
        command: command.into(),
        cwd: None,
        policy: build::policy(),
        limits: build::limits(),
        env: vec![],
        secrets: vec![],
    }
}

async fn settled(rt: &NodeRuntime, op: &str) -> steve_hub::wire::node_runtime::LedgerEntry {
    for _ in 0..1000 {
        if let Some(e) = rt.ledger(op).unwrap()
            && e.is_settled()
        {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{op} never settled: {:?}", rt.ledger(op).unwrap());
}

fn exec_result(e: &steve_hub::wire::node_runtime::LedgerEntry) -> Option<ExecResult> {
    match &e.receipt.as_ref()?.state {
        Observed::Finished {
            result: Some(OperationResult::Exec(r)),
            ..
        } => Some(r.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn written_before_the_crash_it_is_sent_once_after_the_restart() {
    let h = hub().await;
    h.node.answer_exec(ExecResult {
        exit_code: Some(0),
        stdout: "built".into(),
        stderr: String::new(),
        truncated: false,
    });
    // the Hub writes the operation and stops before its sender ran
    let first = h.coordinator();
    assert!(
        first
            .submit("op_x1", "flow_run:r1", "exec", spec("make"))
            .unwrap()
    );
    drop(first);
    assert_eq!(h.execs(), 0);
    let subject: String = h.q(
        "SELECT json_extract(request,'$.subject') FROM node_commands WHERE operation_id='op_x1'",
    );
    assert_eq!(subject, "flow_run:r1");

    let rt = h.coordinator();
    // what the caller asks again after the restart is the same operation, not a second one
    assert!(
        !rt.submit("op_x1", "flow_run:r1", "exec", spec("make"))
            .unwrap()
    );
    assert!(
        rt.submit("op_x1", "flow_run:r1", "exec", spec("rm -rf"))
            .is_err(),
        "the same id for another command"
    );
    rt.start();
    let e = settled(&rt, "op_x1").await;
    assert_eq!(e.state, "finished");
    assert_eq!(e.subject, "flow_run:r1");
    assert_eq!(exec_result(&e).unwrap().stdout, "built");
    assert_eq!(h.execs(), 1);
    // nothing goes again later
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.execs(), 1);
}

#[tokio::test]
async fn a_lost_reply_is_asked_about_never_run_again() {
    let h = hub().await;
    h.node.script_once("exec", Scripted::DropReply);
    let rt = h.coordinator();
    rt.start();
    rt.submit("op_x2", "push:i1", "exec", spec("git push"))
        .unwrap();
    let e = settled(&rt, "op_x2").await;
    assert_eq!(e.state, "finished");
    assert!(exec_result(&e).is_some());
    assert_eq!(
        h.execs(),
        1,
        "ran once; the Hub asked instead of sending again"
    );
}

#[tokio::test]
async fn maybe_sent_with_no_record_on_the_node_is_unknown_and_never_sent() {
    let h = hub().await;
    let first = h.coordinator();
    first
        .submit(
            "op_x3",
            "lane:l1:u1",
            "exec",
            spec("python3 ops/cut-release.py cut"),
        )
        .unwrap();
    drop(first);
    // the Hub had leased it (it may have been sent) when it stopped
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "UPDATE node_commands SET maybe_sent=1 WHERE operation_id='op_x3'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let rt = h.coordinator();
    rt.start();
    let e = settled(&rt, "op_x3").await;
    assert_eq!(e.state, "unknown");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.execs(), 0, "an uncertain operation is never re-executed");
    assert_eq!(rt.ledger("op_x3").unwrap().unwrap().state, "unknown");
}

#[tokio::test]
async fn sent_before_the_crash_it_is_taken_from_the_node() {
    let h = hub().await;
    let first = h.coordinator();
    first
        .submit("op_x4", "vault_use:u1", "exec", spec("deploy"))
        .unwrap();
    drop(first);
    // it reached the Node, and the Hub stopped before it recorded the answer
    let request: String = h.q("SELECT request FROM node_commands WHERE operation_id='op_x4'");
    let c: Command<ExecSpec> = serde_json::from_str(&request).unwrap();
    h.node.exec(c).await.unwrap();
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "UPDATE node_commands SET maybe_sent=1 WHERE operation_id='op_x4'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let rt = h.coordinator();
    rt.start();
    let e = settled(&rt, "op_x4").await;
    assert_eq!(e.state, "finished");
    assert_eq!(h.execs(), 1);
}

#[tokio::test]
async fn only_workspace_effects_with_a_subject_go_through_the_ledger() {
    let h = hub().await;
    let rt = h.coordinator();
    assert!(rt.submit("op_y1", "nothing", "exec", spec("x")).is_err());
    assert!(
        rt.submit("op_y2", "flow_run:r1", "start_turn", spec("x"))
            .is_err()
    );
    assert!(rt.ledger("op_y1").unwrap().is_none());
}
