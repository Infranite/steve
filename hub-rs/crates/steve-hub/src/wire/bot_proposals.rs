//! A blocked automatic request can propose a Bot on a verified route. Confirmation is a
//! workspace-owner command that creates the Bot and its reviewed grants in one transaction.
use steve_core::CreateItem;
use steve_node_api::HarnessChoice;
use steve_store::{Members, NewBot, Read, StoreError, bot_admin, bot_dispatch};
use steve_types::{
    BotProposal, ConfigEvent, ConfirmBotProposal, PlacementBlocked, PlacementRejection as Reason,
    ProposalTeam,
};

use super::core::RoutedRuntime;
use crate::error::ApiError;

fn owns(r: &mut impl Read, ws: &str, actor: &str) -> steve_store::Result<bool> {
    Ok(Members::new(r).role(ws, actor)?.as_deref() == Some("owner"))
}

fn storage(error: StoreError) -> ApiError {
    tracing::error!("Bot proposal: {error}");
    ApiError::internal(steve_i18n::t!("hub.error.generic"))
}

impl RoutedRuntime {
    pub(super) fn propose_bot(
        &self,
        n: &CreateItem,
        blocked: &PlacementBlocked,
    ) -> steve_store::Result<Option<BotProposal>> {
        let Some(actor) = n.actor.as_deref().or(n.requester.as_deref()) else {
            return Ok(None);
        };
        if n.bot.as_deref().is_some_and(|id| !id.is_empty())
            || !matches!(blocked.reason, Reason::NoCandidates | Reason::Specialty)
        {
            return Ok(None);
        }
        let Some(harnesses) = &self.dispatch_harnesses else {
            return Ok(None);
        };
        self.store.write(|w| {
            if !owns(w, &n.ws, actor)? {
                return Ok(None);
            }
            let node = n.node.as_deref().unwrap_or("local");
            let project_id = n.project.as_ref().and_then(|p| p.as_deref());
            let project = project_id
                .map(|p| self.store.projects().get(p))
                .transpose()?
                .flatten();
            if project_id.is_some() && project.is_none() {
                return Ok(None);
            }
            // A proposal starts in Ask mode. Only a current positive report may support it.
            let mut options = harnesses.for_node(node);
            options.sort_unstable_by(|a, b| a.id.cmp(&b.id));
            let Some(choice) = options
                .into_iter()
                .map(|h| HarnessChoice {
                    harness: h.id,
                    model: h
                        .default_model
                        .filter(|id| h.models.iter().any(|m| &m.id == id)),
                    effort: None,
                })
                .find(|choice| self.verified_proposal_route(&n.ws, project.as_ref(), node, choice))
            else {
                return Ok(None);
            };
            let team = bot_dispatch::selected(w, &n.ws, n.team.as_deref())?.map(|t| ProposalTeam {
                id: t.id,
                name: t.name,
            });
            let draft = BotProposal {
                id: steve_store::uid("bp"),
                revision: 1,
                name: n
                    .required_specialties
                    .first()
                    .unwrap_or(&n.title)
                    .chars()
                    .take(40)
                    .collect(),
                role: n.title.chars().take(200).collect(),
                specialties: bot_dispatch::specialties(n.required_specialties.clone()),
                harness: choice.harness,
                model: choice.model,
                node: node.into(),
                team,
            };
            bot_dispatch::save_proposal(w, &n.ws, actor, project_id, &draft)?;
            Ok(Some(draft))
        })
    }

    fn verified_proposal_route(
        &self,
        ws: &str,
        project: Option<&steve_store::Project>,
        node: &str,
        choice: &HarnessChoice,
    ) -> bool {
        matches!(
            self.assess_node(ws, project, node, choice, "default"),
            Ok(None)
        ) && self.report(node, choice, Some("default")).0.is_none()
    }

    pub(super) fn confirm_bot_proposal(
        &self,
        ws: &str,
        actor: &str,
        id: &str,
        edit: ConfirmBotProposal,
    ) -> Result<String, ApiError> {
        self.store
            .write(|w| {
                if !owns(w, ws, actor)? {
                    return Err(ConfirmError::Api(ApiError::Json(
                        403,
                        steve_i18n::t!("hub.dispatch.owner_only"),
                    )));
                }
                let Some(proposal) = bot_dispatch::proposal(w, ws, actor, id)? else {
                    return Err(ConfirmError::Api(ApiError::not_found(steve_i18n::t!(
                        "hub.dispatch.proposal_missing"
                    ))));
                };
                let draft = proposal.draft;
                if edit.expected_revision != draft.revision {
                    return Err(ConfirmError::Api(ApiError::Json(
                        409,
                        steve_i18n::t!("hub.dispatch.proposal_stale"),
                    )));
                }
                if let Some(bot) = proposal.confirmed_bot {
                    return Ok(bot);
                }
                let mut team = draft
                    .team
                    .map(|t| bot_dispatch::selected(w, ws, Some(&t.id)))
                    .transpose()
                    .map_err(|error| match error {
                        StoreError::NotFound(_) => ConfirmError::Api(ApiError::Json(
                            409,
                            steve_i18n::t!("hub.dispatch.proposal_stale"),
                        )),
                        other => ConfirmError::Store(other),
                    })?
                    .flatten();
                let project = proposal
                    .project
                    .as_deref()
                    .map(|p| self.store.projects().get(p))
                    .transpose()?
                    .flatten();
                let choice = HarnessChoice {
                    harness: edit.harness,
                    model: edit.model,
                    effort: None,
                };
                let node = draft.node.as_str();
                if (proposal.project.is_some() && project.is_none())
                    || !self.verified_proposal_route(ws, project.as_ref(), node, &choice)
                {
                    return Err(ConfirmError::Api(ApiError::Json(
                        409,
                        steve_i18n::t!("hub.dispatch.not_candidate"),
                    )));
                }
                let bot = self
                    .store
                    .bots()
                    .create(
                        ws,
                        &NewBot {
                            name: edit.name.trim().to_owned(),
                            role: Some(edit.role),
                            specialties: Some(bot_dispatch::specialties(edit.specialties)),
                            harness: choice.harness,
                            model: choice.model,
                            perm_mode: Some("ask".into()),
                            ..Default::default()
                        },
                        actor,
                    )?
                    .ok_or(StoreError::NotFound("Created Bot"))?;
                bot_admin::set_bindings(w, ws, &bot.id, &[node.into()])?;
                if let Some(team) = &mut team {
                    team.members.push(bot_dispatch::Member {
                        bot: bot.id.clone(),
                        nodes: None,
                    });
                    bot_admin::save(w, ws, Some(&team.id), &team.name, &team.members)?;
                    w.emit(ConfigEvent::Settings {
                        ws: ws.into(),
                        id: ws.into(),
                    });
                }
                bot_dispatch::confirm(w, id, &bot.id)?;
                Ok(bot.id)
            })
            .map_err(|e| match e {
                ConfirmError::Api(e) => e,
                ConfirmError::Store(e) => storage(e),
            })
    }
}

enum ConfirmError {
    Api(ApiError),
    Store(StoreError),
}

impl From<StoreError> for ConfirmError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc, time::Duration};

    use steve_core::{Core, CoreConfig};
    use steve_node_api::{
        NodeId,
        testkit::{FakeNode, build},
    };
    use steve_store::{
        NewBot, NoPublisher, Store, WS, bot_admin,
        bot_dispatch::{self, Member},
    };
    use steve_types::ConfirmBotProposal;

    use crate::wire::{
        core::{HubHost, RoutedRuntime},
        host_node::{HostNode, ServiceNode},
        node_runtime::{NodeRuntime, Sink},
        workspaces::{LocalFace, Workspaces},
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
            panic!("this proposal fixture must not call a model")
        }
    }

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
        store: Store,
    }

    impl Fixture {
        fn new(dir: &Path) -> Self {
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
            let node = FakeNode::new("local");
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
                store: store.clone(),
                host: host.clone(),
                workspaces,
                worktrees: Some(cfg.worktrees.clone()),
                dispatch_harnesses: Some(
                    std::thread::spawn(|| {
                        let choice = build::choice();
                        tokio::runtime::Runtime::new().unwrap().block_on(
                            crate::harnesses::ready_locally(
                                &[&choice.harness],
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
                store,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn confirming_a_bot_proposal_twice_creates_one_bot_and_one_reviewed_grant() {
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
            tokio::task::spawn_blocking(move || {
                rt.confirm_bot_proposal(WS, "u", &id, edit).unwrap()
            })
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
}
