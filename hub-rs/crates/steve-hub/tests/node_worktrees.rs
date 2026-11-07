//! An item's worktree made and given back by its Node (N0e-E11a), against the fake Node: the
//! coordinator tells the business side what a prepare or a tidy came to, once committed; a failed
//! prepare does not leave its open waiting forever; a worktree given back is made again before the
//! session opens again; an item paused while its session opened starts nothing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_hub::wire::node_runtime::{
    Binding, NodeDomain, NodeRuntime, Planned, Release, Sink, WorkspaceFact,
};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_store::{Store, StoreError};

#[derive(Default)]
struct Facts {
    workspace: Mutex<Vec<WorkspaceFact>>,
    ends: Mutex<Vec<(String, String, Option<String>)>>,
}

impl NodeDomain for Facts {
    fn turn_ended(&self, session: &str, _: Option<&str>, stop: &str, error: Option<String>) {
        self.ends
            .lock()
            .unwrap()
            .push((session.into(), stop.into(), error));
    }
    fn workspace_changed(&self, fact: WorkspaceFact) {
        self.workspace.lock().unwrap().push(fact);
    }
}

const S: &str = "s1";
const ITEM: &str = "i1";

struct H {
    _dir: tempfile::TempDir,
    store: Store,
    node: Arc<FakeNode>,
    rt: Arc<NodeRuntime>,
    facts: Arc<Facts>,
}

fn hub() -> H {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute_batch(&format!(
                "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('{ITEM}','default',1,'t','run','执行',1,1);
                 INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('{S}','default','{ITEM}','claude-code','.','active',1,1);"
            ))?;
            Ok(())
        })
        .unwrap();
    let node = FakeNode::new("n1");
    let facts = Arc::new(Facts::default());
    let rt = NodeRuntime::new(store.clone(), node.clone(), facts.clone());
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    H {
        _dir: dir,
        store,
        node,
        rt,
        facts,
    }
}

fn root(id: &str, rel: &str) -> RootPath {
    RootPath {
        root: RootId::new(id).unwrap(),
        rel: RelPath::new(rel).unwrap(),
    }
}

fn worktree() -> PrepareKind {
    PrepareKind::Worktree {
        repository: root("repo", ""),
        branch: format!("steve/{ITEM}"),
        base: None,
        at: Some(root("worktrees", ITEM)),
    }
}

impl H {
    fn bind(&self, prepare: PrepareKind) {
        self.rt
            .bind(&Binding {
                session: S.into(),
                resource: format!("item:{ITEM}"),
                prepare,
                choice: build::choice(),
                policy: build::policy(),
            })
            .unwrap();
    }
    fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
    fn exec(&self, sql: &str) {
        self.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
    }
    fn phase(&self) -> String {
        self.q(&format!(
            "SELECT phase FROM runtime_sessions WHERE session_id='{S}'"
        ))
    }
    fn methods(&self) -> Vec<&'static str> {
        self.node.log().into_iter().map(|(m, _)| m).collect()
    }
    fn count(&self, m: &str) -> usize {
        self.methods().iter().filter(|x| **x == m).count()
    }
    fn facts(&self) -> Vec<WorkspaceFact> {
        self.facts.workspace.lock().unwrap().clone()
    }
    fn prepared(&self) -> usize {
        self.facts()
            .iter()
            .filter(|f| matches!(f, WorkspaceFact::Prepared { session, .. } if session == S))
            .count()
    }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A normal item's pause must also gate the coordinator's direct follow-up path. The core's
/// turn-end hook runs after this path, too late to protect the queued input on its own.
#[tokio::test]
async fn a_paused_item_keeps_queued_input_until_cancel_finishes_and_resume_claims_it_once() {
    let h = hub();
    h.bind(worktree());
    h.rt.prompt(S, "first turn", "user", &[]).unwrap();
    until("the first turn", || h.count("start_turn") == 1).await;
    let first = h.rt.active_attempt(S).unwrap().unwrap();
    let execution: ExecutionRef =
        serde_json::from_str(&h.q::<String>("SELECT execution FROM runtime_attempts LIMIT 1"))
            .unwrap();
    h.rt.prompt(S, "continue the original goal", "user", &[])
        .unwrap();
    let queued: String = h.q("SELECT id FROM queued WHERE session_id='s1'");

    // The item action persists this state and requests this cancellation. A successful request
    // is not the attempt's completion; the Node still owns the running work until its event.
    h.exec("UPDATE items SET status='pause' WHERE id='i1'");
    assert!(h.rt.cancel(S).unwrap());
    until("the cancel request", || h.count("cancel") == 1).await;
    assert_eq!(
        h.rt.active_attempt(S).unwrap().as_deref(),
        Some(first.as_str())
    );
    assert!(h.rt.is_busy(S));
    assert_eq!(
        h.q::<String>("SELECT id FROM queued WHERE session_id='s1'"),
        queued
    );

    let mut completion = build::done();
    completion.outcome = Outcome::Cancelled;
    completion.stop_reason = "cancelled".into();
    h.node.finish(&execution, completion).await.unwrap();
    until("the cancelled attempt's completion", || {
        h.q::<String>("SELECT state FROM runtime_attempts ORDER BY created LIMIT 1") == "finished"
    })
    .await;
    assert!(h.rt.active_attempt(S).unwrap().is_none());
    assert!(!h.rt.is_busy(S));
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts"), 1);
    assert_eq!(
        h.q::<String>("SELECT id FROM queued WHERE session_id='s1'"),
        queued
    );
    assert_eq!(
        h.q::<String>("SELECT text FROM queued WHERE session_id='s1'"),
        "continue the original goal"
    );
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Held);

    h.exec("UPDATE items SET status='run' WHERE id='i1'");
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Created);
    until("the resumed turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Busy);
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts"), 2);
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM queued WHERE session_id='s1'"),
        0
    );
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM session_dispatch WHERE text='continue the original goal' AND state='claimed'"),
        1
    );
}

#[tokio::test]
async fn a_worktree_it_made_is_told_given_back_once_and_made_again_before_it_opens_again() {
    let h = hub();
    h.bind(worktree());
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("the turn", || h.methods().contains(&"start_turn")).await;
    // the session waiting for it hears where it is, once
    until("prepared", || h.prepared() == 1).await;
    match &h.facts()[0] {
        WorkspaceFact::Prepared { view, .. } => {
            assert_eq!(view.branch.as_deref(), Some("steve/i1"));
        }
        other => panic!("{other:?}"),
    }
    assert!(h.rt.makes_workspace(S).unwrap());
    // the turn ends; the core lets the harness go before it tidies
    let exec = h
        .store
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT execution FROM runtime_attempts LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
        .unwrap();
    let exec: ExecutionRef = serde_json::from_str(&exec).unwrap();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("ready", || h.phase() == "ready").await;
    assert!(matches!(h.rt.release(S).unwrap(), Release::Pending(_)));
    until("released", || h.phase() == "released").await;

    // given back through the ledger, told once, with its subject
    assert!(h.rt.tidy_workspace(ITEM, true).unwrap());
    assert!(
        h.rt.tidy_workspace(ITEM, true).unwrap(),
        "on its way already: nothing new"
    );
    until("tidied", || {
        h.facts()
            .iter()
            .any(|f| matches!(f, WorkspaceFact::Tidied { item, removed: true, .. } if item == ITEM))
    })
    .await;
    assert_eq!(h.count("tidy_workspace"), 1);
    let subject: String = h.q(
        "SELECT json_extract(request,'$.subject') FROM node_commands WHERE method='tidy_workspace'",
    );
    assert_eq!(subject, "tidy:i1");
    assert!(
        !h.rt.tidy_workspace(ITEM, true).unwrap(),
        "given back already: nothing to tidy"
    );

    // the next message makes it again (the same spec), then opens and runs
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("the second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("prepare_workspace"), 2);
    let m = h.methods();
    let second_prepare = m.iter().rposition(|x| *x == "prepare_workspace").unwrap();
    let last_open = m.iter().rposition(|x| *x == "open_session").unwrap();
    assert!(second_prepare < last_open, "{m:?}");
    until("prepared again", || h.prepared() == 2).await;
}

#[tokio::test]
async fn a_failed_prepare_ends_its_open_and_the_next_message_prepares_again() {
    let h = hub();
    h.node.refuse_next(
        "prepare_workspace",
        ApiError::new(ErrorCode::Invalid, "git worktree: not a worktree of repo"),
    );
    h.bind(worktree());
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("the failure", || {
        h.facts()
            .iter()
            .any(|f| matches!(f, WorkspaceFact::PrepareFailed { session, error: Some(e) } if session == S && e.code == ErrorCode::Invalid))
    })
    .await;
    // the open never goes (it would wait forever); the session is let go, the message stays
    until("released", || h.phase() == "released").await;
    assert_eq!(h.count("open_session"), 0);
    let open: String = h.q("SELECT state FROM node_commands WHERE method='open_session'");
    assert_eq!(open, "rejected");
    let queued: i64 = h.q("SELECT COUNT(*) FROM queued WHERE session_id='s1'");
    assert_eq!(queued, 1);
    assert!(
        h.facts.ends.lock().unwrap().is_empty(),
        "no turn waited: one card, not a turn end too"
    );
    // the next message prepares again
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("the turn", || h.methods().contains(&"start_turn")).await;
    assert_eq!(h.count("prepare_workspace"), 1, "the refused one never ran");
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM node_commands WHERE method='prepare_workspace'"),
        2
    );
    until("prepared", || h.prepared() == 1).await;
}

#[tokio::test]
async fn an_item_paused_while_its_session_opened_starts_nothing_until_it_continues() {
    let h = hub();
    h.exec("UPDATE items SET status='pause' WHERE id='i1'");
    h.bind(worktree());
    assert_eq!(h.rt.prompt(S, "hello", "user", &[]).unwrap(), Planned::Held);
    until("ready", || h.phase() == "ready").await;
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Held);
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts"), 0);
    assert_eq!(h.count("start_turn"), 0, "{:?}", h.methods());
    let queued: i64 = h.q("SELECT COUNT(*) FROM queued WHERE session_id='s1'");
    assert_eq!(queued, 1, "the message waits");
    // continued: what waited goes with the next message
    h.exec("UPDATE items SET status='run' WHERE id='i1'");
    assert_eq!(
        h.rt.prompt(S, "继续这个事项。", "me", &[]).unwrap(),
        Planned::Created
    );
    until("the turn", || h.methods().contains(&"start_turn")).await;
}

#[tokio::test]
async fn a_scratch_or_existing_workspace_is_not_one_the_node_makes_or_tidies() {
    let h = hub();
    h.bind(PrepareKind::Scratch);
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("the turn", || h.methods().contains(&"start_turn")).await;
    assert!(!h.rt.makes_workspace(S).unwrap());
    assert!(!h.rt.tidy_workspace(ITEM, true).unwrap());
    assert_eq!(h.count("tidy_workspace"), 0);
    // its prepare is told too, without a branch (the Hub records no worktree for it)
    until("prepared", || h.prepared() == 1).await;
    assert!(
        h.facts()
            .iter()
            .all(|f| matches!(f, WorkspaceFact::Prepared { view, .. } if view.branch.is_none()))
    );
}
