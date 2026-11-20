//! SG45: real research orchestration with a scripted FakeNode, without a model process.
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::time::Duration;
use steve_core::thread::{ResearchRequest, ResearchSink, Researcher, RunEnd, ToolSeen};
use steve_hub::wire::{
    node_runtime::{NodeDomain, NodeRuntime, Sink},
    research::{NodeResearch, ResearchAdapter},
};
use steve_node_api::{
    testkit::{FakeNode, build},
    *,
};
use steve_runtime::testkit::eventually_sync;
use steve_store::{FnPublisher, Store, StoreError};
struct Domain;
impl NodeDomain for Domain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}
#[derive(Default)]
struct Seen(
    Mutex<Vec<String>>,
    Mutex<Vec<ToolSeen>>,
    Mutex<Vec<Option<u32>>>,
);
impl ResearchSink for Seen {
    fn text(&self, t: &str) {
        self.0.lock().unwrap().push(t.into());
    }
    fn tool(&self, tool: &ToolSeen) {
        self.1.lock().unwrap().push(tool.clone());
    }
    fn tool_running(&self, call: Option<u32>, _: &str, _: &str, _: &str) {
        self.2.lock().unwrap().push(call);
    }
}
fn request(run: &str) -> ResearchRequest {
    let main = steve_core::repo::Bot {
        id: "steve".into(),
        workspace_id: "default".into(),
        name: "Steve".into(),
        role: "r".into(),
        main: true,
        harness: "claude-code".into(),
        model: None,
        reasoning_effort: None,
        rules: String::new(),
        accept_mode: "inherit".into(),
        perm_mode: "default".into(),
        enabled: true,
        revision: 1,
        updated: 0,
        persona: String::new(),
        proactive: 1,
        avatar: Default::default(),
        default_project: None,
        prompt: None,
        device: None,
    };
    ResearchRequest {
        run: run.into(),
        ws: "default".into(),
        mid: "msg_1".into(),
        user: None,
        question: "answer the contract".into(),
        history: vec![],
        main,
        context: String::new(),
        project: None,
        readable: vec![],
        files: vec![],
        earlier_files: vec![],
        resume: None,
        stop: Arc::new(AtomicBool::new(false)),
        mcp: None,
        models: Default::default(),
        ui: false,
        full: false,
        brief: String::new(),
    }
}
#[tokio::test]
async fn sg45_research_forwards_drafts_and_completion_then_releases_admission() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("test.sqlite3"), FnPublisher(|_| {})).unwrap();
    store.ensure_workspace().unwrap();
    store.transaction::<_, StoreError>(|tx| { tx.execute("INSERT INTO thread_runs(id,workspace_id,message_id,status,created) VALUES ('r1','default','msg_1','running',1)",[])?; Ok(()) }).unwrap();
    let node = FakeNode::new("local");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Domain));
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    eventually_sync("research runtime reconciled", || rt.is_reconciled()).await;
    let mut cfg = steve_research::ResearchConfig::new(dir.path().join("scratch"));
    cfg.models = vec!["m1".into()];
    let adapter = Arc::new(
        ResearchAdapter::new(
            cfg,
            steve_core::repo::Db::new(store),
            dir.path().join("data"),
        )
        .on_node(NodeResearch {
            runtime: rt.clone(),
            roots: Arc::new(|| Ok(vec![])),
        }),
    );
    let sink = Arc::new(Seen::default());
    let task = tokio::spawn({
        let a = adapter.clone();
        let s = sink.clone();
        async move { a.research(request("r1"), &*s).await }
    });
    eventually_sync("one research job accepted", || {
        node.log().iter().any(|(m, _)| *m == "start_job")
    })
    .await;
    let snapshot = node
        .snapshot(PageRequest {
            cursor: None,
            limit: 100,
        })
        .await
        .unwrap();
    let exec = snapshot
        .entries
        .iter()
        .find(|e| e.method == "start_job")
        .unwrap()
        .execution
        .clone()
        .unwrap();
    node.draft(&exec, "message", &["draft answer"])
        .await
        .unwrap();
    for phase in [
        ToolPhase::Pending,
        ToolPhase::Running,
        ToolPhase::Running,
        ToolPhase::Succeeded,
        ToolPhase::Succeeded,
    ] {
        let tool = ToolView {
            id: CallId::new("call").unwrap(),
            parent: None,
            origin: ToolOrigin::Harness,
            name: "Read".into(),
            category: ToolCategory::Read,
            title: "read".into(),
            description: None,
            target: Some("src/input.rs".into()),
            phase,
            action: None,
            result: vec![],
            error: None,
            duration_ms: Some(5),
        };
        node.emit(
            Subject::Execution(exec.clone()),
            EventBody::ToolChanged { tool },
        )
        .await
        .unwrap();
    }
    let mut completion = build::done();
    completion.result = build::text("final answer");
    completion.job = Some(JobReport {
        end: JobEnd::Done,
        turns: 2,
        files_read: vec![],
        sandboxed: true,
    });
    node.finish(&exec, completion).await.unwrap();
    let out = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.end, RunEnd::Done);
    assert_eq!(out.text, "final answer");
    assert_eq!(out.turns, 2);
    assert_eq!(out.model.as_deref(), Some("m1"));
    assert!(sink.0.lock().unwrap().iter().any(|v| v == "draft answer"));
    assert_eq!(*sink.2.lock().unwrap(), [Some(1)]);
    let tools = sink.1.lock().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].call, Some(1));
    assert_eq!(tools[0].kind, "read");
    assert!(tools[0].ok);
    assert_eq!(tools[0].ms, 5);
    drop(tools);
    assert!(adapter.shutdown(Duration::ZERO).await);
    let closed = adapter.research(request("r2"), &Seen::default()).await;
    assert_eq!(closed.end, RunEnd::Failed);
    assert!(closed.error.unwrap().contains("关闭"));
    rt.stop("test complete");
}
