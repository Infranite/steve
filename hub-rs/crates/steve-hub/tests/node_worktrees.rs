//! An item's worktree made and given back by its Node (N0e-E11a), against the fake Node: the
//! coordinator tells the business side what a prepare or a tidy came to, once committed; a failed
//! prepare does not leave its open waiting forever; a worktree given back is made again before the
//! session opens again; an item paused while its session opened starts nothing.

use std::sync::{Arc, Mutex};
use steve_runtime::testkit::eventually_sync;
use steve_store::testkit::hub::node_checks;

use steve_hub::wire::node_runtime::{
    Binding, NodeDomain, NodeRuntime, Planned, Release, Sink, WorkspaceFact,
};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_store::Store;

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
    steve_store::testkit::hub::workspace_cases::seed_item_and_active_session(&store, ITEM, S)
        .unwrap();
    let node = FakeNode::new("local");
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
    fn phase(&self) -> String {
        node_checks::binding_phase_by_session(&self.store, &S).unwrap()
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

/// A normal item's pause must also gate the coordinator's direct follow-up path. The core's
/// turn-end hook runs after this path, too late to protect the queued input on its own.
#[tokio::test]
async fn a_paused_item_keeps_queued_input_until_cancel_finishes_and_resume_claims_it_once() {
    let h = hub();
    h.bind(worktree());
    h.rt.prompt(S, "first turn", "user", &[]).unwrap();
    eventually_sync("the first turn", || h.count("start_turn") == 1).await;
    let first = h.rt.active_attempt(S).unwrap().unwrap();
    let execution: ExecutionRef =
        serde_json::from_str(&node_checks::attempt_execution(&h.store).unwrap()).unwrap();
    h.rt.prompt(S, "continue the original goal", "user", &[])
        .unwrap();
    let queued: String = node_checks::queued_id_by_session(&h.store, "s1").unwrap();

    // The item action persists this state and requests this cancellation. A successful request
    // is not the attempt's completion; the Node still owns the running work until its event.
    steve_store::testkit::hub::mutations::update_items_status_pause_id_i1(&h.store).unwrap();
    assert!(h.rt.cancel(S).unwrap());
    eventually_sync("the cancel request", || h.count("cancel") == 1).await;
    assert_eq!(
        h.rt.active_attempt(S).unwrap().as_deref(),
        Some(first.as_str())
    );
    assert!(h.rt.is_busy(S));
    assert_eq!(
        node_checks::queued_id_by_session(&h.store, "s1").unwrap(),
        queued
    );

    let mut completion = build::done();
    completion.outcome = Outcome::Cancelled;
    completion.stop_reason = "cancelled".into();
    h.node.finish(&execution, completion).await.unwrap();
    eventually_sync("the cancelled attempt's completion", || {
        node_checks::first_attempt_state(&h.store).unwrap() == "finished"
    })
    .await;
    assert!(h.rt.active_attempt(S).unwrap().is_none());
    assert!(!h.rt.is_busy(S));
    assert_eq!(node_checks::attempt_count(&h.store).unwrap(), 1);
    assert_eq!(
        node_checks::queued_id_by_session(&h.store, "s1").unwrap(),
        queued
    );
    assert_eq!(
        node_checks::queued_text_by_session(&h.store, "s1").unwrap(),
        "continue the original goal"
    );
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Held);

    steve_store::testkit::hub::mutations::update_items_status_run_id_i1(&h.store).unwrap();
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Created);
    eventually_sync("the resumed turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Busy);
    assert_eq!(node_checks::attempt_count(&h.store).unwrap(), 2);
    assert_eq!(
        node_checks::queued_count_by_session(&h.store, "s1").unwrap(),
        0
    );
    assert_eq!(
        node_checks::dispatch_count_continue_the_original_goal_claimed(&h.store).unwrap(),
        1
    );
}

#[tokio::test]
async fn a_worktree_it_made_is_told_given_back_once_and_made_again_before_it_opens_again() {
    let h = hub();
    h.bind(worktree());
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    eventually_sync("the turn", || h.methods().contains(&"start_turn")).await;
    // the session waiting for it hears where it is, once
    eventually_sync("prepared", || h.prepared() == 1).await;
    match &h.facts()[0] {
        WorkspaceFact::Prepared { view, .. } => {
            assert_eq!(view.branch.as_deref(), Some("steve/i1"));
        }
        other => panic!("{other:?}"),
    }
    assert!(h.rt.makes_workspace(S).unwrap());
    // the turn ends; the core lets the harness go before it tidies
    let exec = steve_store::testkit::hub::node_checks::attempt_execution(&h.store).unwrap();
    let exec: ExecutionRef = serde_json::from_str(&exec).unwrap();
    h.node.finish(&exec, build::done()).await.unwrap();
    eventually_sync("ready", || h.phase() == "ready").await;
    assert!(matches!(h.rt.release(S).unwrap(), Release::Pending(_)));
    eventually_sync("released", || h.phase() == "released").await;

    // given back through the ledger, told once, with its subject
    assert!(h.rt.tidy_workspace(ITEM, true).unwrap());
    assert!(
        h.rt.tidy_workspace(ITEM, true).unwrap(),
        "on its way already: nothing new"
    );
    eventually_sync("tidied", || {
        h.facts()
            .iter()
            .any(|f| matches!(f, WorkspaceFact::Tidied { item, removed: true, .. } if item == ITEM))
    })
    .await;
    assert_eq!(h.count("tidy_workspace"), 1);
    let subject: String = node_checks::command_subject_tidy_workspace(&h.store).unwrap();
    assert_eq!(subject, "tidy:i1");
    assert!(
        !h.rt.tidy_workspace(ITEM, true).unwrap(),
        "given back already: nothing to tidy"
    );

    // the next message makes it again (the same spec), then opens and runs
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    eventually_sync("the second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("prepare_workspace"), 2);
    let m = h.methods();
    let second_prepare = m.iter().rposition(|x| *x == "prepare_workspace").unwrap();
    let last_open = m.iter().rposition(|x| *x == "open_session").unwrap();
    assert!(second_prepare < last_open, "{m:?}");
    eventually_sync("prepared again", || h.prepared() == 2).await;
}

#[tokio::test]
async fn a_failed_prepare_holds_its_queue_until_an_explicit_retry_prepares_again() {
    let h = hub();
    h.node.refuse_next(
        "prepare_workspace",
        ApiError::new(ErrorCode::Invalid, "git worktree: not a worktree of repo"),
    );
    h.bind(worktree());
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    eventually_sync("the failure", || {
        h.facts()
            .iter()
            .any(|f| matches!(f, WorkspaceFact::PrepareFailed { session, error: Some(e) } if session == S && e.code == ErrorCode::Invalid))
    })
    .await;
    // the open never goes (it would wait forever); the session is let go, the message stays
    eventually_sync("released", || h.phase() == "released").await;
    assert_eq!(h.count("open_session"), 0);
    let open: String = node_checks::command_state_by_method(&h.store, "open_session").unwrap();
    assert_eq!(open, "rejected");
    let queued: i64 = node_checks::queued_count_by_session(&h.store, "s1").unwrap();
    assert_eq!(queued, 1);
    assert!(
        h.facts.ends.lock().unwrap().is_empty(),
        "no turn waited: one card, not a turn end too"
    );
    // Further input stays held until the user explicitly retries the failed startup.
    assert_eq!(h.rt.prompt(S, "again", "user", &[]).unwrap(), Planned::Held);
    h.rt.retry_open(S, &build::choice()).unwrap();
    eventually_sync("the turn", || h.methods().contains(&"start_turn")).await;
    assert_eq!(h.count("prepare_workspace"), 1, "the refused one never ran");
    assert_eq!(
        node_checks::command_count_by_method(&h.store, "prepare_workspace").unwrap(),
        2
    );
    eventually_sync("prepared", || h.prepared() == 1).await;
}

#[tokio::test]
async fn an_item_paused_while_its_session_opened_starts_nothing_until_it_continues() {
    let h = hub();
    steve_store::testkit::hub::mutations::update_items_status_pause_id_i1(&h.store).unwrap();
    h.bind(worktree());
    assert_eq!(h.rt.prompt(S, "hello", "user", &[]).unwrap(), Planned::Held);
    eventually_sync("ready", || h.phase() == "ready").await;
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Held);
    assert_eq!(node_checks::attempt_count(&h.store).unwrap(), 0);
    assert_eq!(h.count("start_turn"), 0, "{:?}", h.methods());
    let queued: i64 = node_checks::queued_count_by_session(&h.store, "s1").unwrap();
    assert_eq!(queued, 1, "the message waits");
    // continued: what waited goes with the next message
    steve_store::testkit::hub::mutations::update_items_status_run_id_i1(&h.store).unwrap();
    assert_eq!(
        h.rt.prompt(S, "继续这个事项。", "me", &[]).unwrap(),
        Planned::Created
    );
    eventually_sync("the turn", || h.methods().contains(&"start_turn")).await;
}

#[tokio::test]
async fn a_scratch_or_existing_workspace_is_not_one_the_node_makes_or_tidies() {
    let h = hub();
    h.bind(PrepareKind::Scratch);
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    eventually_sync("the turn", || h.methods().contains(&"start_turn")).await;
    assert!(!h.rt.makes_workspace(S).unwrap());
    assert!(!h.rt.tidy_workspace(ITEM, true).unwrap());
    assert_eq!(h.count("tidy_workspace"), 0);
    // its prepare is told too, without a branch (the Hub records no worktree for it)
    eventually_sync("prepared", || h.prepared() == 1).await;
    assert!(
        h.facts()
            .iter()
            .all(|f| matches!(f, WorkspaceFact::Prepared { view, .. } if view.branch.is_none()))
    );
}

/// The original scratch binding remains readable while a newly attached project's workspace
/// gets its own identity. Reopen and tidy must follow that identity, not reconstruct item:<id>.
#[tokio::test]
async fn attaching_a_project_preserves_the_old_workspace_and_reopens_the_new_one() {
    let h = hub();
    h.bind(PrepareKind::Existing {
        dir: root("scratch", ITEM),
    });
    eventually_sync("scratch session ready", || h.phase() == "ready").await;
    assert!(!h.rt.makes_workspace(S).unwrap());
    h.rt.release(S).unwrap();
    eventually_sync("scratch session released", || h.phase() == "released").await;
    steve_store::testkit::hub::mutations::update_sessions_status_ended_id_s1(&h.store).unwrap();
    let plan = steve_hub::wire::node_binding::binding_with(
        &h.store,
        "s2",
        None,
        &|_| true,
        Some(std::path::Path::new("/worktrees")),
    )
    .unwrap();
    let resource = plan.resource.clone();
    assert_ne!(resource, "item:i1");
    h.rt.bind(&plan).unwrap();
    eventually_sync("project session ready", || {
        node_checks::binding_phase_by_session(&h.store, "s2").unwrap() == "ready"
    })
    .await;
    assert!(h.rt.makes_workspace("s2").unwrap());
    assert!(!h.rt.makes_workspace(S).unwrap());
    assert_ne!(
        node_checks::binding_workspace_ref_by_session(&h.store, "s1").unwrap(),
        node_checks::binding_workspace_ref_by_session(&h.store, "s2").unwrap()
    );
    assert!(!h.rt.tidy_session_workspace(S, true).unwrap());
    h.rt.release("s2").unwrap();
    eventually_sync("project session released", || {
        node_checks::binding_phase_by_session(&h.store, "s2").unwrap() == "released"
    })
    .await;
    assert!(h.rt.tidy_session_workspace("s2", true).unwrap());
    eventually_sync("new workspace tidied", || {
        h.count("tidy_workspace") == 1
            && node_checks::command_count_tidy_workspace_finished(&h.store).unwrap() == 1
    })
    .await;
    let target: String = node_checks::command_workspace_id_tidy_workspace(&h.store).unwrap();
    assert_eq!(target, derived_id("w", &resource));
    h.rt.prompt("s2", "continue in the project", "user", &[])
        .unwrap();
    eventually_sync("project session reopened", || h.count("start_turn") == 1).await;
    assert_eq!(h.count("prepare_workspace"), 3);
    let last: String = node_checks::latest_command_resource_prepare_workspace(&h.store).unwrap();
    assert_eq!(last, resource);
    assert_eq!(
        node_checks::command_count_prepare_workspace(&h.store).unwrap(),
        1
    );
}
