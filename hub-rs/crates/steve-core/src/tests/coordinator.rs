//! Steve as workspace coordinator: main bot is never an automatic executor candidate; when no
//! eligible executor exists, Steve opens one deduplicated AskOwner decision card via W3.3
//! settlement, and "later" runs waiting items on Steve while switching the workspace policy.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use steve_modules::Caller;
use steve_store::{
    BotPatch, NewBot, WS, bot_admin,
    bot_dispatch::{self, Member},
};
use steve_types::{
    BotProposal, CoordinatorDeferSource, DecisionPrompt, DecisionStatus, NO_EXECUTOR,
    NoExecutorPolicy, PlacementReason, PlacementRejection, ProposalTeam,
};

use crate::{CreateItem, DecisionCommand, ItemAction, testkit::Kit};

fn seed_owner(k: &Kit) -> (Caller, Caller) {
    steve_store::testkit::hub::mutations::seed_users(k.db().store()).unwrap();
    let owner = Caller {
        ws: WS.into(),
        me: "u".into(),
    };
    let member = Caller {
        ws: WS.into(),
        me: "other".into(),
    };
    (owner, member)
}

fn open_no_executor_cards(k: &Kit) -> Vec<crate::repo::Decision> {
    k.db()
        .store()
        .decisions()
        .open(WS)
        .unwrap()
        .into_iter()
        .filter(|d| d.subject_type == NO_EXECUTOR && d.status == DecisionStatus::Open)
        .collect()
}

fn set_enabled(k: &Kit, bot: &str, enabled: bool) {
    k.db()
        .store()
        .bots()
        .patch(
            bot,
            &BotPatch {
                enabled: Some(enabled),
                ..Default::default()
            },
            "u",
        )
        .unwrap();
}

#[test]
fn four_dispatch_topologies_never_auto_pick_main_bot() {
    let k = Kit::new();
    // 1. No Team + ordinary Bot present -> picks ordinary Bot ("dev"), never Steve.
    let no_team_worker = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "No team with dev".into(),
            goal: "Build feature".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(no_team_worker.bot_id.as_deref(), Some("dev"));
    assert!(open_no_executor_cards(&k).is_empty());

    // 2. No Team + only Steve (dev disabled) -> item created unassigned, 1 no_executor card opened.
    set_enabled(&k, "dev", false);
    let only_steve = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Only Steve available".into(),
            goal: "Write release notes".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(only_steve.bot_id, None);
    assert!(k.db().sessions_of_item(&only_steve.id).unwrap().is_empty());
    let cards = open_no_executor_cards(&k);
    assert_eq!(cards.len(), 1);
    let Some(DecisionPrompt::NoExecutor {
        items, rejected, ..
    }) = &cards[0].question_template
    else {
        panic!("expected NoExecutor prompt: {:?}", cards[0]);
    };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id.as_str(), only_steve.id);
    assert!(
        rejected
            .iter()
            .any(|b| b.bot.as_ref().map(|id| id.as_str()) == Some("dev")
                && b.reason == PlacementRejection::Bot)
    );

    // 3. Team with eligible member -> picks Team member even while another unassigned item waits.
    k.db()
        .store()
        .bots()
        .seed(
            "worker",
            false,
            &NewBot {
                name: "Worker".into(),
                harness: "fixture".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    let team_id = k
        .db()
        .store()
        .write(|w| {
            let id = bot_admin::save(
                w,
                WS,
                None,
                "Delivery Team",
                &[Member {
                    bot: "worker".into(),
                    nodes: None,
                }],
            )?;
            bot_admin::set_default(w, WS, Some(&id))?;
            Ok::<_, steve_store::StoreError>(id)
        })
        .unwrap();
    let with_team = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Team with eligible member".into(),
            goal: "Ship update".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(with_team.bot_id.as_deref(), Some("worker"));

    // 4. Team with no eligible member -> item unassigned, merges into open workspace card.
    set_enabled(&k, "worker", false);
    let empty_team_item = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Team member disabled".into(),
            goal: "Audit pipeline".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(empty_team_item.bot_id, None);
    let cards = open_no_executor_cards(&k);
    assert_eq!(cards.len(), 1);
    let Some(DecisionPrompt::NoExecutor { items, team, .. }) = &cards[0].question_template else {
        panic!("expected NoExecutor prompt");
    };
    assert_eq!(items.len(), 2);
    assert_eq!(team.as_ref().map(|t| t.id.as_str()), Some(team_id.as_str()));

    // Explicit selection of Steve remains allowed even under a default Team.
    let explicit_steve = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Explicit Steve".into(),
            goal: "Coordinate".into(),
            bot: Some("steve".into()),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(explicit_steve.bot_id.as_deref(), Some("steve"));
}

#[test]
fn five_unassigned_items_dedup_to_one_card_and_settle_together() {
    let k = Kit::new();
    let (owner, _) = seed_owner(&k);
    set_enabled(&k, "dev", false);

    let mut ids = Vec::new();
    for idx in 1..=5 {
        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: format!("Waiting item {idx}"),
                goal: format!("Do task {idx}"),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(item.bot_id, None);
        assert!(k.db().sessions_of_item(&item.id).unwrap().is_empty());
        ids.push(item.id);
    }

    let cards = open_no_executor_cards(&k);
    assert_eq!(cards.len(), 1, "5 no-executor items must share 1 card");
    let card = &cards[0];
    let Some(DecisionPrompt::NoExecutor { items, .. }) = &card.question_template else {
        panic!("expected NoExecutor template");
    };
    assert_eq!(items.len(), 5);

    // Enable dev and settle once via assign_bot -> all 5 items start on dev.
    set_enabled(&k, "dev", true);
    let resolved = k
        .core
        .decision_command(
            &owner,
            &card.id,
            Some(card.revision),
            DecisionCommand::Resolve {
                choice: "assign_bot".into(),
                text: "dev".into(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, DecisionStatus::Resolved);
    assert!(open_no_executor_cards(&k).is_empty());

    for id in &ids {
        let item = k.db().item(id).unwrap().unwrap();
        assert_eq!(item.bot_id.as_deref(), Some("dev"));
        assert_eq!(k.db().sessions_of_item(id).unwrap().len(), 1);
        let initial = k
            .db()
            .store()
            .read(|r| bot_dispatch::initial(r, id))
            .unwrap()
            .unwrap();
        assert_eq!(initial.explanation.reason, PlacementReason::Explicit);
    }
}

#[test]
fn later_switches_policy_to_steve_until_reverted_and_qualified_bot_still_wins() {
    let k = Kit::new();
    let (owner, member) = seed_owner(&k);
    set_enabled(&k, "dev", false);

    let first = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "First wait".into(),
            goal: "Task 1".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    let second = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Second wait".into(),
            goal: "Task 2".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    let card = open_no_executor_cards(&k).pop().unwrap();

    // Non-owner cannot settle the AskOwner card.
    let err = k
        .core
        .decision_command(
            &member,
            &card.id,
            Some(card.revision),
            DecisionCommand::Resolve {
                choice: "later".into(),
                text: String::new(),
            },
        )
        .unwrap_err();
    assert!(matches!(err, crate::CoreError::Status(403, _)), "{err}");

    // Owner chooses "later": both waiting items run on Steve and policy flips to Steve.
    let resolved = k
        .core
        .decision_command(
            &owner,
            &card.id,
            Some(card.revision),
            DecisionCommand::Resolve {
                choice: "later".into(),
                text: String::new(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, DecisionStatus::Resolved);
    assert_eq!(
        k.db()
            .store()
            .read(|r| bot_dispatch::no_executor_policy(r, WS))
            .unwrap(),
        NoExecutorPolicy::Steve
    );
    for id in [&first.id, &second.id] {
        let item = k.db().item(id).unwrap().unwrap();
        assert_eq!(item.bot_id.as_deref(), Some("steve"));
        assert_eq!(k.db().sessions_of_item(id).unwrap().len(), 1);
        let fact = k
            .db()
            .store()
            .read(|r| bot_dispatch::initial(r, id))
            .unwrap()
            .unwrap();
        assert_eq!(
            fact.explanation.reason,
            PlacementReason::CoordinatorDeferred
        );
        assert_eq!(
            fact.explanation.deferred_by,
            Some(CoordinatorDeferSource::Card)
        );
        assert_eq!(fact.explanation.fallback, Some(PlacementRejection::Bot));
    }

    // Subsequent no-executor item executes directly on Steve without opening a card.
    let direct = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Direct Steve".into(),
            goal: "Task 3".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(direct.bot_id.as_deref(), Some("steve"));
    assert!(open_no_executor_cards(&k).is_empty());
    let fact = k
        .db()
        .store()
        .read(|r| bot_dispatch::initial(r, &direct.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        fact.explanation.reason,
        PlacementReason::CoordinatorDeferred
    );
    assert_eq!(
        fact.explanation.deferred_by,
        Some(CoordinatorDeferSource::Setting)
    );

    // Once a qualified ordinary Bot is available, normal ranking wins even with policy=steve.
    set_enabled(&k, "dev", true);
    let qualified = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Qualified wins".into(),
            goal: "Task 4".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(qualified.bot_id.as_deref(), Some("dev"));

    // Revert setting back to "ask" and disable dev -> fresh card opens again.
    k.db()
        .store()
        .write(|w| bot_dispatch::set_no_executor_policy(w, WS, NoExecutorPolicy::Ask, "u"))
        .unwrap();
    set_enabled(&k, "dev", false);
    let ask_again = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Ask restored".into(),
            goal: "Task 5".into(),
            project: Some(None),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ask_again.bot_id, None);
    let reopened = open_no_executor_cards(&k);
    assert_eq!(reopened.len(), 1);
    assert_ne!(reopened[0].id, card.id);
}

#[test]
fn confirm_proposal_creates_bot_adds_to_team_and_starts_waiting_items() {
    let k = Kit::new();
    let (owner, _) = seed_owner(&k);
    set_enabled(&k, "dev", false);
    let team_id = k
        .db()
        .store()
        .write(|w| {
            let id = bot_admin::save(
                w,
                WS,
                None,
                "Audit Team",
                &[Member {
                    bot: "dev".into(),
                    nodes: None,
                }],
            )?;
            bot_admin::set_default(w, WS, Some(&id))?;
            Ok::<_, steve_store::StoreError>(id)
        })
        .unwrap();
    k.rt.st().bot_proposal = Some(BotProposal {
        id: "bp_sec".into(),
        revision: 1,
        name: "Security".into(),
        role: "Security review".into(),
        specialties: vec!["security".into()],
        harness: "fixture".into(),
        model: None,
        node: "local".into(),
        team: Some(ProposalTeam {
            id: team_id.clone(),
            name: "Audit Team".into(),
        }),
    });

    let item = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            actor: Some("u".into()),
            title: "Security audit".into(),
            goal: "Audit auth".into(),
            project: Some(None),
            required_specialties: vec!["security".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(item.bot_id, None);

    let card = open_no_executor_cards(&k).pop().unwrap();
    assert!(card.options.contains(&"confirm_proposal".to_string()));
    let resolved = k
        .core
        .decision_command(
            &owner,
            &card.id,
            Some(card.revision),
            DecisionCommand::Resolve {
                choice: "confirm_proposal".into(),
                text: String::new(),
            },
        )
        .unwrap();
    assert_eq!(resolved.status, DecisionStatus::Resolved);
    let created_bot = resolved.result["bot"].as_str().unwrap().to_string();
    let updated = k.db().item(&item.id).unwrap().unwrap();
    assert_eq!(updated.bot_id.as_deref(), Some(created_bot.as_str()));
    assert_eq!(k.db().sessions_of_item(&item.id).unwrap().len(), 1);
    let active_team = k
        .db()
        .store()
        .read(|r| bot_dispatch::selected(r, WS, Some(&team_id)))
        .unwrap()
        .unwrap();
    assert!(active_team.member(&created_bot).is_some());
}

#[test]
fn manual_reassign_and_archive_reconcile_open_no_executor_card_and_emit_metrics() {
    #[derive(Default)]
    struct CounterMap(Arc<Mutex<BTreeMap<String, u64>>>);

    struct Inc(Arc<Mutex<BTreeMap<String, u64>>>, String);
    impl metrics::CounterFn for Inc {
        fn increment(&self, value: u64) {
            *self.0.lock().unwrap().entry(self.1.clone()).or_default() += value;
        }
        fn absolute(&self, value: u64) {
            self.0.lock().unwrap().insert(self.1.clone(), value);
        }
    }
    impl metrics::Recorder for CounterMap {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn register_counter(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            let mut labels: Vec<String> = key
                .labels()
                .map(|l| format!("{}={}", l.key(), l.value()))
                .collect();
            labels.sort();
            let id = if labels.is_empty() {
                key.name().to_string()
            } else {
                format!("{}[{}]", key.name(), labels.join(","))
            };
            metrics::Counter::from_arc(Arc::new(Inc(self.0.clone(), id)))
        }
        fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::noop()
        }
        fn register_histogram(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::noop()
        }
    }

    let recorder = CounterMap::default();
    let counts = recorder.0.clone();
    metrics::with_local_recorder(&recorder, || {
        let k = Kit::new();
        let (owner, _) = seed_owner(&k);
        set_enabled(&k, "dev", false);

        let a = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Wait A".into(),
                goal: "A".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        let b = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Wait B".into(),
                goal: "B".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(open_no_executor_cards(&k).len(), 1);

        // Manually picking Steve reconciles the card down to 1 item (B).
        k.core.reassign(&a.id, "steve", "u").unwrap();
        let cards = open_no_executor_cards(&k);
        assert_eq!(cards.len(), 1);
        let Some(DecisionPrompt::NoExecutor { items, .. }) = &cards[0].question_template else {
            panic!("expected NoExecutor");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_str(), b.id);

        // Archiving the last waiting item supersedes the open card automatically.
        k.core.item_action(&b.id, ItemAction::Archive, "u").unwrap();
        assert!(open_no_executor_cards(&k).is_empty());

        // Trigger another no-executor item and settle with "later" -> then a setting-driven run.
        let c = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Wait C".into(),
                goal: "C".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        let card = open_no_executor_cards(&k).pop().unwrap();
        k.core
            .decision_command(
                &owner,
                &card.id,
                Some(card.revision),
                DecisionCommand::Resolve {
                    choice: "later".into(),
                    text: String::new(),
                },
            )
            .unwrap();
        assert_eq!(
            k.db().item(&c.id).unwrap().unwrap().bot_id.as_deref(),
            Some("steve")
        );
        let _d = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Direct D".into(),
                goal: "D".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
    });

    let snap = counts.lock().unwrap().clone();
    assert_eq!(
        snap.get("steve_dispatch_no_executor_total[policy=ask,reason=bot]")
            .copied(),
        Some(3),
        "{snap:?}"
    );
    assert_eq!(
        snap.get("steve_dispatch_no_executor_total[policy=steve,reason=bot]")
            .copied(),
        Some(1),
        "{snap:?}"
    );
    assert_eq!(
        snap.get("steve_no_executor_decisions_opened_total")
            .copied(),
        Some(2),
        "{snap:?}"
    );
    assert_eq!(
        snap.get("steve_no_executor_decisions_settled_total[choice=supersede]")
            .copied(),
        Some(1),
        "{snap:?}"
    );
    assert_eq!(
        snap.get("steve_no_executor_decisions_settled_total[choice=later]")
            .copied(),
        Some(1),
        "{snap:?}"
    );
    assert_eq!(
        snap.get("steve_coordinator_self_executions_total[source=card]")
            .copied(),
        Some(1),
        "{snap:?}"
    );
    assert_eq!(
        snap.get("steve_coordinator_self_executions_total[source=setting]")
            .copied(),
        Some(1),
        "{snap:?}"
    );
}
