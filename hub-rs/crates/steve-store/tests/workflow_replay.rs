//! Synthetic snapshot replay and #503 selective-reopen contract tests.

#[path = "../examples/workflow_token_replay.rs"]
#[allow(dead_code)]
mod replay;

mod common;

use common::*;
use serde_json::json;
use steve_store::testkit::SqliteFixtures;
use steve_store::{
    ItemPatch, NewBot, NewItem, NewProject, NewSession, UnitSpec, WS, WorkUnit, Workflow,
    delivery_repo,
};
use steve_types::orchestration::{Finding, ReviewReceipt};

fn unit(id: &str, bot: &str, kind: &str, deps: &[&str]) -> UnitSpec {
    UnitSpec {
        id: id.into(),
        title: format!("unit {id}"),
        goal: format!("complete {id}"),
        bot_id: bot.into(),
        kind: kind.into(),
        depends_on: deps.iter().map(|s| (*s).into()).collect(),
        resources: vec![],
        required: true,
    }
}

fn seed_bots(t: &T) {
    for id in ["writer", "reviewer", "integrator"] {
        t.store
            .bots()
            .seed(
                id,
                false,
                &NewBot {
                    name: id.into(),
                    harness: "fixture".into(),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
    }
}

fn create_parent(t: &T, with_git_project: bool) -> String {
    let parent = t
        .store
        .items()
        .create(
            WS,
            &NewItem {
                tags: task_tags(&t.store, WS),
                title: "parent".into(),
                goal: "deliver work".into(),
                bot_id: "writer".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id;
    if with_git_project {
        let project = t
            .store
            .projects()
            .create(
                WS,
                &NewProject {
                    name: "repo".into(),
                    kind: "git".into(),
                    path: "repo".into(),
                    worktree: true,
                    ..Default::default()
                },
            )
            .unwrap();
        t.store
            .items()
            .patch(
                &parent,
                &ItemPatch {
                    project_id: Some(Some(project.id)),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    parent
}

fn start_unit(t: &T, parent: &str, id: &str) -> WorkUnit {
    let existing = t
        .store
        .workflows()
        .get(parent)
        .unwrap()
        .unwrap()
        .units
        .into_iter()
        .find(|u| u.spec.id == id && u.status == "pending" && u.item_id.is_some());
    let claimed = match existing {
        Some(u) => {
            let child = u.item_id.unwrap();
            let item = t.store.items().get(&child).unwrap().unwrap();
            t.store
                .write(|w| {
                    steve_store::lifecycle_write_in(
                        w,
                        &child,
                        &steve_store::LifecycleWrite {
                            phase: steve_types::ItemPhase::Ready,
                            cycle: item.cycle + 1,
                            delivery_revision: item.delivery_revision,
                            outcome: item.outcome,
                            acceptance: item.acceptance,
                            closing: item.closing,
                            closed_at: item.closed_at,
                            closed_reason: item.closed_reason,
                        },
                    )?;
                    steve_store::claim_in(w, parent, id)
                })
                .unwrap()
        }
        None => t.store.workflows().claim(parent, id).unwrap(),
    };
    let session = t
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: claimed.item_id.clone(),
            bot_id: Some(claimed.spec.bot_id.clone()),
            harness: "fixture".into(),
            cwd: t.dir.path().to_string_lossy().into(),
            title: id.into(),
            token: format!("tok-{id}-{}", claimed.attempt),
            ..Default::default()
        })
        .unwrap();
    t.store
        .workflows()
        .bind_session(parent, id, claimed.item_id.as_deref().unwrap(), &session.id)
        .unwrap()
}

fn add_delivery(t: &T, unit: &WorkUnit, digest: &str) -> String {
    let id = format!("deliv-{}-{}-{digest}", unit.spec.id, unit.attempt);
    let url = format!("/d/{id}");
    t.store
        .write(|w| {
            delivery_repo::insert(
                w,
                &delivery_repo::Delivery {
                    id,
                    item_id: unit.item_id.clone(),
                    title: "Result".into(),
                    name: "result.txt".into(),
                    mime: "text/plain".into(),
                    size: 16,
                    created: 1,
                    entrypoint: None,
                    preview: json!({}),
                    interactive: false,
                    reply: false,
                    workspace_id: WS.into(),
                    owner_id: None,
                    registered_url: url.clone(),
                    sha256: digest.into(),
                    manifest: Default::default(),
                },
                unit.session_id.as_deref(),
            )
        })
        .unwrap();
    url
}

fn deliver_immutable(t: &T, unit: &WorkUnit, digest: &str, passed: bool) -> Workflow {
    let url = add_delivery(t, unit, digest);
    t.store
        .workflows()
        .deliver(
            unit.item_id.as_deref().unwrap(),
            unit.session_id.as_deref().unwrap(),
            "delivered",
            &json!([{"kind":"file","title":"Result","location":url}]),
            &json!([{"name":"verified","passed":passed,"operated":true}]),
            passed,
        )
        .unwrap()
}

fn deliver_plain(t: &T, unit: &WorkUnit, passed: bool) -> Workflow {
    t.store
        .workflows()
        .deliver(
            unit.item_id.as_deref().unwrap(),
            unit.session_id.as_deref().unwrap(),
            if passed { "ok" } else { "failed" },
            &json!(if passed {
                vec![json!({"kind":"file","title":"Result","location":"out.txt"})]
            } else {
                vec![]
            }),
            &json!([{"name":"verified","passed":passed,"operated":true}]),
            passed,
        )
        .unwrap()
}

/// Real production workflows are Git worktree items (`project_id IS NOT NULL`), and `action=fail`
/// records no delivery artifact. Characterizes the current `apply_review_findings` gate: both
/// conditions yield `ReviewReceipt::Unverified` without modifying the graph.
#[test]
fn apply_review_findings_returns_unverified_on_git_project_items() {
    let t = fresh();
    seed_bots(&t);
    let parent = create_parent(&t, true);
    let specs = vec![
        unit("a", "writer", "work", &[]),
        unit("b", "writer", "work", &[]),
        unit("review", "reviewer", "review", &["a", "b"]),
        unit("integrate", "integrator", "integrate", &["review"]),
    ];
    t.store
        .workflows()
        .replace(&parent, 0, "plan", &specs, 2, 3)
        .unwrap();
    let a = start_unit(&t, &parent, "a");
    deliver_plain(&t, &a, true);
    let b = start_unit(&t, &parent, "b");
    deliver_plain(&t, &b, true);
    let review = start_unit(&t, &parent, "review");
    let evidence = add_delivery(&t, &review, "finding-doc");
    let failed = deliver_plain(&t, &review, false);
    let rev_unit = failed.units.iter().find(|u| u.spec.id == "review").unwrap();
    let scope = failed.review_scope(rev_unit).unwrap();
    let receipt = t
        .store
        .workflows()
        .apply_review_findings(
            &parent,
            &scope,
            &[Finding {
                affected_unit: "a".into(),
                criterion: "verified".into(),
                evidence_ref: evidence,
            }],
            "review failed on a",
        )
        .unwrap();
    assert_eq!(receipt, ReviewReceipt::Unverified);
    let after = t.store.workflows().get(&parent).unwrap().unwrap();
    assert_eq!(after.units[0].status, "succeeded");
    assert_eq!(after.units[2].status, "failed");
}

/// End-to-end synthetic snapshot test for the offline replay tool: builds a pre-#528 snapshot
/// containing a review-failed reset (with target work + failed review + pending integrate) and
/// verifies exact turn attribution and #528 rule replay.
#[test]
fn offline_replay_attributes_tokens_and_evaluates_selective_reopen_on_synthetic_snapshot() {
    let t = fresh();
    seed_bots(&t);
    let parent = create_parent(&t, true);
    let specs = vec![
        unit("a", "writer", "work", &[]),
        unit("b", "writer", "work", &[]),
        unit("review", "reviewer", "review", &["a", "b"]),
        unit("integrate", "integrator", "integrate", &["review"]),
    ];
    t.store
        .workflows()
        .replace(&parent, 0, "plan", &specs, 2, 3)
        .unwrap();
    let a1 = start_unit(&t, &parent, "a");
    deliver_plain(&t, &a1, true);
    let b1 = start_unit(&t, &parent, "b");
    deliver_plain(&t, &b1, true);
    let r1 = start_unit(&t, &parent, "review");
    let g_failed = deliver_plain(&t, &r1, false);

    // Record a pre-#528 `reset` row in workflow_history (invalidating a, review, integrate)
    // and reopen a + review so attempt 2 runs cleanly in the snapshot.
    let invalidated = json!([g_failed.units[0], g_failed.units[2], g_failed.units[3],]);
    t.store
        .fixture_write(|c| {
            c.execute(
                "UPDATE workflows SET revision = revision + 1 WHERE item_id = ?",
                [&parent],
            )?;
            let rev: i64 = c.query_row(
                "SELECT revision FROM workflows WHERE item_id = ?",
                [&parent],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO workflow_history(workflow_id,revision,action,reason,unit_id,detail,created)
                 VALUES (?,?,'reset','review failed','a',?,1)",
                rusqlite::params![parent, rev, json!({"invalidated": invalidated}).to_string()],
            )?;
            c.execute(
                "UPDATE workflow_units SET status='pending',session_id=NULL WHERE workflow_id=? AND id IN ('a','review')",
                [&parent],
            )?;
            Ok(())
        })
        .unwrap();

    let a2 = start_unit(&t, &parent, "a");
    deliver_plain(&t, &a2, true);
    let r2 = start_unit(&t, &parent, "review");
    deliver_plain(&t, &r2, true);
    let i1 = start_unit(&t, &parent, "integrate");
    deliver_plain(&t, &i1, true);

    // Seed agent_turns + usage_records for a2, r2, i1.
    let seed_usage = |unit: &WorkUnit, turn_id: &str, cache_read: i64| {
        let sid = unit.session_id.as_deref().unwrap();
        let item = unit.item_id.as_deref().unwrap();
        t.store
            .fixture_write(|c| {
                c.execute(
                    "INSERT INTO agent_turns(id,workspace_id,session_id,item_id,harness,started)
                     VALUES (?,?,?,?,'fixture',100)",
                    rusqlite::params![turn_id, WS, sid, item],
                )?;
                c.execute(
                    "INSERT INTO usage_records(workspace_id,ts,day,source,purpose,harness,model,turn_id,session_id,item_id,input,cache_read,cache_write,output,ok)
                     VALUES (?,100,'2026-11-15','agent','agent','fixture','m',?,?,?,1000,?,0,500,1)",
                    rusqlite::params![WS, turn_id, sid, item, cache_read],
                )?;
                Ok(())
            })
            .unwrap();
    };
    seed_usage(&a2, "turn-a2", 10_000_000);
    seed_usage(&r2, "turn-r2", 4_000_000);
    seed_usage(&i1, "turn-i1", 3_000_000);

    // Checkpoint WAL before read-only immutable replay.
    t.store
        .fixture_write(|c| {
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .unwrap();

    let summary = replay::replay_snapshot(t.store.path(), "2026-11-11", "2026-11-22");
    assert_eq!(summary.workflows_total, 1);
    assert_eq!(summary.workflows_with_project, 1);
    assert_eq!(summary.review_failed_resets, 1);
    assert_eq!(summary.review_failed_old_invalidated_total, 3);
    assert_eq!(summary.review_failed_old_invalidated_non_pending, 2);
    assert_eq!(summary.review_failed_turns_total.turns, 3);
    assert_eq!(summary.review_failed_turns_total.cache_read, 17_000_000);
    assert_eq!(summary.review_failed_prev_succeeded_turns.turns, 1);
    assert_eq!(
        summary.review_failed_prev_succeeded_turns.cache_read,
        10_000_000
    );
    assert_eq!(
        summary.review_failed_prev_succeeded_same_goal_turns.turns,
        1
    );
    assert_eq!(summary.review_failed_apply_findings_applied, 0);
    assert_eq!(summary.review_failed_apply_findings_unverified, 1);
    assert_eq!(summary.review_failed_genuinely_avoidable_tokens.turns, 0);
    assert_eq!(summary.review_failed_pending_first_run_tokens.turns, 1);
    assert_eq!(
        summary.review_failed_pending_first_run_tokens.cache_read,
        3_000_000
    );
    // Avoid unused warning on helper used in subsequent defect regression tests.
    let _ = deliver_immutable;
}
