//! Contract tests for unified WaitContract write enforcement, named workspace settings,
//! typed views, and deterministic due timeout handling.

use serde_json::json;
use steve_store::testkit::SqliteFixtures;
use steve_store::views::Views;
use steve_store::{
    FlowWaits, NewBot, NewDecision, NewItem, NewWait, StoreError, WS, github, mail, operation_repo,
    wait_repo,
};
use steve_types::{
    CloseKind, CloseRequest, DecisionPrompt, DecisionStatus, ItemPhase, Millis, Said, WaitContract,
    WaitKind, WaitOwner, WaitTimeoutAction, wait,
};

mod common;
use common::{fresh, task_tags};

fn seed_item(store: &steve_store::Store, title: &str) -> String {
    store
        .bots()
        .seed(
            "dev",
            false,
            &NewBot {
                name: "Dev".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    store
        .items()
        .create(
            WS,
            &NewItem {
                tags: task_tags(store, WS),
                title: title.into(),
                goal: "goal".into(),
                bot_id: "dev".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id
}

#[test]
fn write_paths_enforce_valid_wait_contracts_and_named_workspace_limits() {
    let t = fresh();
    let s = &t.store;
    let item = seed_item(s, "Wait bounds");

    // Configure custom named workspace settings (including one below min to prove clamping).
    let mut cfg = serde_json::Map::new();
    cfg.insert(wait::SETTING_WAIT_DECISION_MS.into(), json!(120_000));
    cfg.insert(wait::SETTING_WAIT_ITEM_OPERATION_MS.into(), json!(500));
    cfg.insert(wait::SETTING_WAIT_FLOW_MS.into(), json!(300_000));
    s.settings().set(WS, &cfg, "me").unwrap();

    let limits = s.read(|r| wait_repo::limits(r, WS)).unwrap();
    assert_eq!(limits.decision_ms, 120_000);
    assert_eq!(limits.item_operation_ms, wait::WAIT_LIMIT_MIN_MS);
    assert_eq!(limits.flow_wait_ms, 300_000);

    // Reject an invalid explicit wait contract (deadline precedes started_at).
    let invalid = WaitContract {
        owner: WaitOwner::Owner,
        started_at: Millis(5_000),
        deadline: Millis(4_000),
        on_timeout: WaitTimeoutAction::Close,
    };
    let err = s
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "ask".into(),
            subject_type: "item".into(),
            subject_id: item.clone(),
            item_id: Some(item.clone()),
            question: "invalid".into(),
            requested_by: "dev".into(),
            wait: Some(invalid),
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(err, StoreError::Rejected(_)), "{err:?}");

    // Default decision contract picks up workspace wait.decision_ms (120_000 ms).
    let dec = s
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "ask".into(),
            subject_type: "item".into(),
            subject_id: item.clone(),
            item_id: Some(item.clone()),
            question: "valid".into(),
            requested_by: "dev".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    assert_eq!(dec.wait.owner, WaitOwner::Owner);
    assert_eq!(dec.wait.deadline.0 - dec.wait.started_at.0, 120_000);
    assert_eq!(dec.wait.on_timeout, WaitTimeoutAction::Close);

    // DecisionView carries the typed WaitContract.
    let view = s
        .read(|r| r.decision_views().of(WS, &[dec.id.clone()]))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(view.wait, Some(dec.wait.clone()));

    // Operation picks up clamped wait.operation_ms (WAIT_LIMIT_MIN_MS).
    let (op, _) = s
        .write(|w| {
            operation_repo::add(
                w,
                &item,
                1,
                1,
                "release",
                None,
                true,
                "pending",
                "rel",
                &json!({}),
            )
        })
        .unwrap();
    assert_eq!(op.wait.owner, WaitOwner::system("release"));
    assert_eq!(
        op.wait.deadline.0 - op.wait.started_at.0,
        wait::WAIT_LIMIT_MIN_MS
    );
    assert_eq!(op.wait.on_timeout, WaitTimeoutAction::EscalateToOwner);

    // FlowWaits::put never leaves until NULL and uses wait.flow_ms.
    let now = steve_store::now();
    s.fixture_write(|c| {
        c.execute_batch(&format!(
            "INSERT INTO flows(id,workspace_id,name,kind,enabled,version,graph,origin,created_by,created,updated)
               VALUES('fl_1','{WS}','F','flow',1,1,'{{}}','me','usr_1',{now},{now});
             INSERT INTO flow_runs(id,workspace_id,flow_id,flow_version,trigger_kind,status,cursor,context,started)
               VALUES('fr_1','{WS}','fl_1',1,'manual','waiting','n1','{{}}',{now});"
        ))?;
        Ok(())
    })
    .unwrap();
    s.write(|w| {
        FlowWaits(w).put(&NewWait {
            run: "fr_1",
            node: "n1",
            ws: WS,
            mode: "decision",
            until: None,
            matcher: &json!({}),
            decision: Some(&dec.id),
            now,
        })
    })
    .unwrap();
    let fw = s.read(|r| FlowWaits(r).get("fr_1", "n1")).unwrap().unwrap();
    assert_eq!(fw.wait.owner, WaitOwner::Owner);
    assert_eq!(fw.wait.deadline, Millis(now + 300_000));
    assert_eq!(
        s.read(|r| FlowWaits(r).next_deadline(now)).unwrap(),
        Some(now + 300_000)
    );
}

#[test]
fn unified_due_sweep_handles_decision_dependency_operation_github_and_mail_timeouts() {
    let t = fresh();
    let s = &t.store;
    let item_closing = seed_item(s, "Closing accept timeout");
    let item_dep = seed_item(s, "Dependency wait timeout");
    let item_prereq = seed_item(s, "Prereq");
    let item_op = seed_item(s, "Operation timeout");

    let base = 1_795_000_000_000_i64;
    let deadline = base + 60_000;

    // 1. Closing item waiting on an accept card that times out -> expires card & returns item to active.
    let accept_card = s
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "accept".into(),
            subject_type: "item".into(),
            subject_id: item_closing.clone(),
            item_id: Some(item_closing.clone()),
            question: "Accept?".into(),
            options: Some(vec!["pass".into(), "rework".into()]),
            requested_by: "dev".into(),
            wait: Some(
                WaitContract::new(
                    WaitOwner::Owner,
                    Millis(base),
                    Millis(deadline),
                    WaitTimeoutAction::Close,
                )
                .unwrap(),
            ),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    let close_req = CloseRequest {
        kind: CloseKind::Complete,
        reason: "done".into(),
        by: "dev".into(),
        at: base,
        frozen_revision: 1,
        scope_revision: 0,
        decision: Some(accept_card.id.clone()),
        ..Default::default()
    };
    s.fixture_write(|c| {
        c.execute(
            "UPDATE items SET phase='closing', closing=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&close_req).unwrap(), item_closing],
        )?;
        Ok(())
    })
    .unwrap();

    // 2. Prerequisite wait on item_dep -> times out and escalates to owner card.
    let mut dep_cfg = serde_json::Map::new();
    dep_cfg.insert(wait::SETTING_WAIT_DEPENDENCY_MS.into(), json!(60_000));
    s.settings().set(WS, &dep_cfg, "me").unwrap();
    s.deps().add(&item_dep, &item_prereq).unwrap();
    s.dependency_waits().arm(&item_dep).unwrap();
    s.fixture_write(|c| {
        c.execute(
            "UPDATE item_dependency_waits SET wait_started_at=?1, wait_deadline=?2 WHERE item_id=?3",
            rusqlite::params![base, deadline, item_dep],
        )?;
        Ok(())
    })
    .unwrap();
    let dep_view = s
        .read(|r| r.item_views().of(WS, &[item_dep.clone()]))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        dep_view.prerequisite_wait.as_ref().map(|w| w.deadline),
        Some(Millis(deadline))
    );

    // 3. Item operation (running) on item_op -> transitions to unknown and escalates to owner.
    let (op, _) = s
        .write(|w| {
            operation_repo::add(
                w,
                &item_op,
                1,
                1,
                "release",
                None,
                true,
                "running",
                "rel",
                &json!({}),
            )
        })
        .unwrap();
    s.fixture_write(|c| {
        c.execute(
            "UPDATE item_operations SET wait_started_at=?1, wait_deadline=?2 WHERE id=?3",
            rusqlite::params![base, deadline, op.id],
        )?;
        Ok(())
    })
    .unwrap();

    // 4. GitHub effect (pending) -> stops retrying (next_at=NULL), becomes failed, escalates.
    s.write(|w| github::insert_effect(w, WS, &accept_card.id, "open", base))
        .unwrap();
    s.fixture_write(|c| {
        c.execute(
            "UPDATE gh_effects SET wait_started_at=?1, wait_deadline=?2 WHERE decision_id=?3",
            rusqlite::params![base, deadline, accept_card.id],
        )?;
        Ok(())
    })
    .unwrap();

    // 5. Mail drafts: pending (closes & discards) and unknown (escalates without discarding).
    s.fixture_write(|c| {
        c.execute_batch(&format!(
            "INSERT INTO systems(id,workspace_id,name,kind,family,created) VALUES('mail','{WS}','Mail','comm','email',{base});
             INSERT INTO parties(id,workspace_id,type,name,contact_kind,created_by,created,updated)
               VALUES('pt_m','{WS}','contact','Owner','real','usr_1',{base},{base});
             INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated)
               VALUES('acc_m','{WS}','mail','email','pt_m','personal','me@x.test','me@x.test','connect',{base},{base});
             INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated)
               VALUES('con_m','{WS}','acc_m','imap','credential','usr_1',{base},{base});"
        ))?;
        Ok(())
    })
    .unwrap();
    let draft_base =
        |id: &str, status: &str, on_timeout: WaitTimeoutAction, owner: WaitOwner| mail::DraftRow {
            id: id.into(),
            workspace_id: WS.into(),
            connection_id: "con_m".into(),
            thread_id: None,
            reply_to_message: None,
            mode: "new".into(),
            to: vec![],
            cc: vec![],
            bcc: vec![],
            subject: format!("Mail {id}"),
            body: "Hi".into(),
            files: vec![],
            author: "steve".into(),
            item_id: None,
            session_id: None,
            run_id: None,
            decision_id: None,
            basis: None,
            content_hash: format!("h_{id}"),
            status: status.into(),
            send_at: None,
            sent_message: None,
            error: None,
            created: base,
            updated: base,
            sent: None,
            delivery_version: None,
            wait: Some(
                WaitContract::new(owner, Millis(base), Millis(deadline), on_timeout).unwrap(),
            ),
        };
    s.write(|w| {
        mail::insert_draft(
            w,
            &draft_base(
                "md_close",
                "pending",
                WaitTimeoutAction::Close,
                WaitOwner::Owner,
            ),
        )?;
        mail::insert_draft(
            w,
            &draft_base(
                "md_esc",
                "unknown",
                WaitTimeoutAction::EscalateToOwner,
                WaitOwner::system("mail"),
            ),
        )
    })
    .unwrap();

    assert_eq!(s.read(|r| wait_repo::next_due(r)).unwrap(), Some(deadline));

    // Nothing fires before deadline.
    assert!(
        s.read(|r| wait_repo::due_decisions(r, deadline - 1, 10))
            .unwrap()
            .is_empty()
    );

    // Run unified timeout handlers at deadline.
    let due = s
        .read(|r| wait_repo::due_decisions(r, deadline, 10))
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, accept_card.id);
    assert!(
        s.write(|w| wait_repo::expire_core_decision(w, &accept_card.id, deadline))
            .unwrap()
    );
    assert_eq!(
        s.write(|w| wait_repo::expire_dependency_waits(w, deadline, 10))
            .unwrap(),
        1
    );
    assert_eq!(
        s.write(|w| wait_repo::expire_item_operations(w, deadline, 10))
            .unwrap(),
        1
    );
    assert_eq!(
        s.write(|w| wait_repo::expire_gh_effects(w, deadline, 10))
            .unwrap(),
        1
    );
    assert_eq!(
        s.write(|w| wait_repo::expire_mail_drafts(w, deadline, 10))
            .unwrap(),
        2
    );

    // Verify outcomes.
    let expired_accept = s.decisions().get(&accept_card.id).unwrap().unwrap();
    assert_eq!(expired_accept.status, DecisionStatus::Expired);
    let closing_item = s.items().get(&item_closing).unwrap().unwrap();
    assert_eq!(closing_item.phase, ItemPhase::Active);
    assert!(closing_item.closing.is_none());

    let dep_wait = s.dependency_waits().get(&item_dep).unwrap().unwrap();
    assert_eq!(dep_wait.state, "timed_out");
    let dep_cards = s
        .read(|r| r.decision_views().of_item(WS, &item_dep))
        .unwrap();
    assert_eq!(dep_cards.len(), 1);
    assert_eq!(
        dep_cards[0].question,
        Said::Key {
            template: DecisionPrompt::WaitTimeout {
                wait_kind: WaitKind::Dependency,
                subject: item_dep.clone(),
            }
        }
    );

    let timed_op = s.read(|r| operation_repo::get(r, &op.id)).unwrap().unwrap();
    assert_eq!(timed_op.status, "unknown");
    let op_cards = s
        .read(|r| r.decision_views().of_item(WS, &item_op))
        .unwrap();
    assert_eq!(op_cards.len(), 1);
    assert_eq!(
        op_cards[0].question,
        Said::Key {
            template: DecisionPrompt::WaitTimeout {
                wait_kind: WaitKind::ItemOperation,
                subject: "release".into(),
            }
        }
    );

    let gh = s
        .read(|r| github::effect_of(r, &accept_card.id))
        .unwrap()
        .unwrap();
    assert_eq!(gh.state, "failed");
    assert!(gh.next_at.is_none());

    let md_close = s.read(|r| mail::draft(r, "md_close")).unwrap().unwrap();
    assert_eq!(md_close.status, "discarded");
    let md_esc = s.read(|r| mail::draft(r, "md_esc")).unwrap().unwrap();
    assert_eq!(md_esc.status, "unknown");
    assert!(md_esc.decision_id.is_some());

    // Second sweep at same timestamp is a no-op (no duplicate escalations).
    assert_eq!(
        s.write(|w| wait_repo::expire_dependency_waits(w, deadline, 10))
            .unwrap(),
        0
    );
    assert_eq!(
        s.write(|w| wait_repo::expire_item_operations(w, deadline, 10))
            .unwrap(),
        0
    );
    assert_eq!(
        s.write(|w| wait_repo::expire_gh_effects(w, deadline, 10))
            .unwrap(),
        0
    );
    assert_eq!(
        s.write(|w| wait_repo::expire_mail_drafts(w, deadline, 10))
            .unwrap(),
        0
    );
    // Remaining next_due is strictly in the future (from the newly opened escalation cards).
    assert!(s.read(|r| wait_repo::next_due(r)).unwrap().unwrap() > deadline);
}
