//! End-to-end graph fixtures: the real Core, routed runtime and durable Node coordinator, with
//! the protocol's strict in-memory Node in place of a harness (no model, login or service needed).
use super::{
    core::{HubHost, RoutedRuntime},
    host_node::{HostNode, ServiceNode},
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
use steve_store::{NewBot, NewItem, NewSession, NoPublisher, Store, UnitSpec, WS, WorkUnit};

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
/// The fixture's local Node, answered in this process: it is reached without a link, so the
/// dispatch probes it as the in-process Node.
struct InProcess(Arc<ServiceNode>);
impl HostNode for InProcess {
    fn id(&self) -> &NodeId {
        self.0.id()
    }
    fn node(&self) -> Arc<dyn steve_node_api::Node> {
        self.0.node()
    }
    fn set_roots(&self, roots: Vec<steve_node::Root>) {
        self.0.set_roots(roots)
    }
    fn allows(&self, at: &steve_node_api::RootPath) -> bool {
        self.0.allows(at)
    }
    fn holds(&self, w: &steve_node_api::WorkspaceRef, path: &Path, adopted: bool) -> bool {
        self.0.holds(w, path, adopted)
    }
    fn local_dir(&self, view: &steve_node_api::WorkspaceView) -> Option<steve_node::LocalDir> {
        self.0.local_dir(view)
    }
    fn in_process(&self) -> bool {
        true
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
        Self::attach_enabled(dir, store, node, true)
    }
    fn attach_enabled(dir: &Path, store: Store, node: Arc<FakeNode>, enabled: bool) -> Self {
        let host = Arc::new(HubHost {
            store: store.clone(),
            data_dir: dir.join("data"),
            core: Default::default(),
            flow: Default::default(),
            approval: Default::default(),
            local_node: Default::default(),
        });
        let local: Arc<dyn HostNode> = Arc::new(InProcess(ServiceNode::new(
            NodeId::new("local").unwrap(),
            node.clone(),
            dir,
        )));
        assert!(host.local_node.set(local.clone()).is_ok());
        let runtime = NodeRuntime::new(store.clone(), node.clone(), host.clone());
        node.connect(Arc::new(Sink(Arc::downgrade(&runtime))));
        let mut cfg = CoreConfig::for_home(dir);
        cfg.dynamic_workflows = enabled;
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
            store: store.clone(),
            host: host.clone(),
            workspaces,
            worktrees: Some(cfg.worktrees.clone()),
            // the fixture's harness is ready on the local Node, as its probe would say
            dispatch_harnesses: Some(
                std::thread::spawn(|| {
                    let choice = build::choice();
                    tokio::runtime::Runtime::new().unwrap().block_on(
                        crate::harnesses::ready_locally(
                            &[&choice.harness],
                            // the Bots' model: the fixture's own and the configured default
                            &[
                                choice.model.as_deref().unwrap_or("default"),
                                "claude-opus-5-5",
                            ],
                        ),
                    )
                })
                .join()
                .unwrap(),
            ),
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
        let store = Store::open(dir.join("steve.sqlite3"), NoPublisher).unwrap();
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
        steve_store::testkit::hub::mutations::seed_users_for_new(&store).unwrap();
        Self::attach(dir, store, FakeNode::new("local"))
    }
    fn parent(&self, requester: Option<&str>) -> String {
        self.store
            .items()
            .create(
                WS,
                &NewItem {
                    tags: vec![
                        self.store
                            .tags()
                            .kinds(WS)
                            .unwrap()
                            .into_iter()
                            .find(|tag| tag.name == "feat")
                            .unwrap()
                            .id,
                    ],
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
        until(|| {
            steve_store::testkit::hub::node_checks::running_attempt_count(&self.store, &sid)
                .unwrap()
                == 1
        })
        .await;
        let raw: String =
            steve_store::testkit::hub::hub_fixtures::attempt_a_execution_running(&self.store, &sid)
                .unwrap();
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
                    scope: None,
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
                .is_closed()
        })
        .await;
    }
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
        let binding: String =
            steve_store::testkit::hub::hub_fixtures::binding_principal(&f.store, &sid).unwrap();
        assert_eq!(binding, requester.unwrap_or("svc:default"));
        let request: String = steve_store::testkit::hub::node_checks::command_request_by_method(
            &f.store,
            "open_session",
        )
        .unwrap();
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
            steve_store::testkit::hub::node_checks::runtime_turns_count(&f.store).unwrap(),
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
        steve_store::testkit::hub::node_checks::runtime_turns_count(&again.store).unwrap(),
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
            .read(|c| steve_store::runtime::released_idle(c, &session.id))
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
    assert_eq!(
        next.item_id, claimed.item_id,
        "the retry runs in the unit's own child"
    );
    assert_ne!(next.session_id.as_deref(), Some(session.id.as_str()));
    assert_eq!(next.attempt, 2);
    again.execution(&parent, "work").await;
    for _ in 0..3 {
        again.core.lifecycle_tick().unwrap();
    }
    assert_eq!(
        steve_store::testkit::hub::node_checks::runtime_turns_count(&again.store).unwrap(),
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
async fn default_off_restart_retires_followups_when_its_session_ends_and_preserves_results() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let parent = f.parent(Some("requester"));
    f.plan(&parent);
    let execution = f.execution(&parent, "work").await;
    let sid = f.unit(&parent, "work").session_id.unwrap();
    f.routed
        .prompt(&sid, "retained followup", "user", &[])
        .unwrap();
    f.routed.node.as_ref().unwrap().stop("simulate restart");
    let off = Fixture::attach_enabled(dir.path(), f.store.clone(), f.node.clone(), false);
    assert!(
        off.routed
            .prompt(&sid, "must not start", "user", &[])
            .is_err()
    );
    assert!(off.routed.check_workflow_execution(&parent).is_err());
    assert_eq!(
        steve_store::testkit::hub::node_checks::queued_count(&off.store).unwrap(),
        1
    );
    off.core.lifecycle_tick().unwrap();
    off.node.finish(&execution, build::done()).await.unwrap();
    until(|| {
        off.routed
            .node
            .as_ref()
            .unwrap()
            .active_attempt(&sid)
            .unwrap()
            .is_none()
            && off
                .store
                .sessions()
                .get(&sid)
                .unwrap()
                .is_some_and(|s| s.status == "ended")
    })
    .await;
    assert_eq!(
        off.routed.node.as_ref().unwrap().flush(&sid, None).unwrap(),
        super::node_runtime::Planned::Held
    );
    assert_eq!(
        steve_store::testkit::hub::node_checks::runtime_turns_count(&off.store).unwrap(),
        1
    );
    assert_eq!(
        steve_store::testkit::hub::node_checks::queued_count(&off.store).unwrap(),
        0
    );
    assert!(off.unit(&parent, "review").item_id.is_none());
    assert_eq!(off.core.workflow_get(&parent).unwrap()["enabled"], false);
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
    steve_store::testkit::hub::mutations::update_items_requested_by_requester(&f.store).unwrap();
    f.core.lifecycle_tick().unwrap();
    let execution = f.execution(&parent, "work").await;
    let u = f.unit(&parent, "work");
    let sid = u.session_id.unwrap();
    f.routed
        .prompt(&sid, "Queued followup", "user", &[])
        .unwrap();
    for change in [
        steve_store::testkit::hub::workflow_cases::PrincipalGate::Requester,
        steve_store::testkit::hub::workflow_cases::PrincipalGate::Principal,
        steve_store::testkit::hub::workflow_cases::PrincipalGate::DisabledBot,
        steve_store::testkit::hub::workflow_cases::PrincipalGate::Membership,
    ] {
        let item = u.item_id.as_deref().unwrap();
        steve_store::testkit::hub::workflow_cases::set_principal_gate(&f.store, item, change, true)
            .unwrap();
        assert!(
            f.routed.check_workflow_execution(&parent).is_err(),
            "{change:?}"
        );
        assert!(
            f.routed.prompt(&sid, "Must refuse", "user", &[]).is_err(),
            "{change:?}"
        );
        assert_eq!(
            f.routed.node.as_ref().unwrap().flush(&sid, None).unwrap(),
            super::node_runtime::Planned::Held,
            "{change:?}"
        );
        steve_store::testkit::hub::workflow_cases::set_principal_gate(
            &f.store, item, change, false,
        )
        .unwrap();
    }
    // A membership change after queueing also holds the Node's own fact-driven planner.
    steve_store::testkit::hub::mutations::clear_workspace_members_user_id_requester(&f.store)
        .unwrap();
    assert_eq!(
        steve_store::testkit::hub::node_checks::queued_count(&f.store).unwrap(),
        1
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
            && f.store
                .sessions()
                .get(&sid)
                .unwrap()
                .is_some_and(|s| s.status == "ended")
    })
    .await;
    assert_eq!(
        steve_store::testkit::hub::node_checks::runtime_turns_count(&f.store).unwrap(),
        1
    );
    // The invalid principal prevented the followup turn. Once the graph retires this session,
    // its unsent input must be discarded rather than left on an ended session (ST-174).
    assert_eq!(
        steve_store::testkit::hub::node_checks::queued_count(&f.store).unwrap(),
        0
    );
    steve_store::testkit::hub::mutations::seed_workspace_members_for_admission_and_queued_followups_refuse_unknown_changed_principals_bots_and_nodes(&f.store).unwrap();
    f.routed.node.as_ref().unwrap().stop("Node unavailable");
    assert!(f.routed.check_workflow_execution(&parent).is_err());
    assert!(
        f.routed
            .prompt(&sid, "No Node fallback", "user", &[])
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn confirming_a_bot_proposal_twice_creates_one_bot_and_one_reviewed_grant() {
    use steve_store::{
        bot_admin,
        bot_dispatch::{self, Member},
    };
    use steve_types::ConfirmBotProposal;

    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    steve_store::testkit::hub::mutations::seed_users(&f.store).unwrap();
    let team = f
        .store
        .write(|w| {
            let members = [Member {
                bot: "reviewer".into(),
                nodes: None,
            }];
            let team = bot_admin::save(w, WS, None, "Review Team", &members)?;
            bot_admin::set_default(w, WS, Some(&team))?;
            Ok::<_, steve_store::StoreError>(team)
        })
        .unwrap();
    let request = steve_core::CreateItem {
        ws: WS.into(),
        actor: Some("u".into()),
        title: "Release evidence".into(),
        goal: "Collect and verify release evidence".into(),
        project: Some(None),
        required_specialties: vec!["release evidence".into()],
        propose: false,
        ..Default::default()
    };
    let count = f.store.bots().list(Some(WS)).unwrap().len();
    let unassigned = f.core.create_item(request.clone()).unwrap();
    assert_eq!(unassigned.bot_id, None);
    let card = f
        .store
        .decisions()
        .open(WS)
        .unwrap()
        .into_iter()
        .find(|d| {
            d.subject_type == steve_types::NO_EXECUTOR
                && d.status == steve_types::DecisionStatus::Open
        })
        .expect("no_executor card opened with Bot proposal");
    let Some(steve_types::DecisionPrompt::NoExecutor {
        proposal: Some(draft),
        ..
    }) = card.question_template.clone()
    else {
        panic!("expected proposal on no_executor card: {card:?}");
    };
    assert_eq!(
        draft.team.as_ref().map(|t| t.id.as_str()),
        Some(team.as_str())
    );
    assert_eq!(f.store.bots().list(Some(WS)).unwrap().len(), count);
    let edit = || ConfirmBotProposal {
        expected_revision: draft.revision,
        name: "Release evidence".into(),
        role: draft.role.clone(),
        specialties: draft.specialties.clone(),
        harness: draft.harness.clone(),
        model: draft.model.clone(),
    };
    assert!(
        f.routed
            .confirm_bot_proposal(WS, "other", &draft.id, edit())
            .is_err()
    );
    let mut invalid = edit();
    invalid.model = Some("missing".into());
    assert!(
        f.routed
            .confirm_bot_proposal(WS, "u", &draft.id, invalid)
            .is_err()
    );
    assert_eq!(f.store.bots().list(Some(WS)).unwrap().len(), count);
    let confirm = |edit| {
        let rt = f.routed.clone();
        let id = draft.id.clone();
        tokio::task::spawn_blocking(move || rt.confirm_bot_proposal(WS, "u", &id, edit).unwrap())
    };
    let a = confirm(edit());
    let b = confirm(edit());
    let opened = a.await.unwrap();
    assert_eq!(opened, b.await.unwrap());
    assert_eq!(f.store.bots().list(Some(WS)).unwrap().len(), count + 1);
    let active = f
        .store
        .read(|r| bot_dispatch::selected(r, WS, None))
        .unwrap()
        .unwrap();
    assert_eq!(active.id, team);
    assert_eq!(active.members.iter().filter(|m| m.bot == opened).count(), 1);
    // Settling the open no_executor card via confirm_proposal reuses the confirmed Bot and starts the waiting item.
    let resolved = f
        .core
        .decision_command(
            &steve_modules::Caller {
                ws: WS.into(),
                me: "u".into(),
            },
            &card.id,
            Some(card.revision),
            steve_core::DecisionCommand::Resolve {
                choice: "confirm_proposal".into(),
                text: String::new(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, steve_types::DecisionStatus::Resolved);
    assert_eq!(
        f.store
            .items()
            .get(&unassigned.id)
            .unwrap()
            .unwrap()
            .bot_id
            .as_deref(),
        Some(opened.as_str())
    );
    assert_eq!(
        f.core.create_item(request).unwrap().bot_id.as_deref(),
        Some(opened.as_str())
    );
    f.routed.node.as_ref().unwrap().stop("test complete");
}
