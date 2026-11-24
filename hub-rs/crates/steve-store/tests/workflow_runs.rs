//! SQLite ledger contract tests for WF-3's 5 tables (`wf_runs`, `wf_script_versions`,
//! `wf_calls`, `wf_call_attempts`, `wf_version_calls`) and 4-rule independent final review gate.

mod common;

use common::fresh;
use steve_store::StoreError;
use steve_store::workflow_runs::{
    NewWfAttempt, NewWfCall, ReviewGateFailure, ReviewGateVerdict, WfAttemptStatus, WfCallStatus,
    WfRunStatus, WorkflowRuns,
};

#[test]
fn enforces_single_active_and_single_succeeded_attempt_unique_indexes() {
    let t = fresh();
    let (run_id, call_id) = t
        .store
        .write(|w| {
            let run = WorkflowRuns(w).create_run(
                "ws1",
                "item-1",
                1,
                1,
                "export default async () => 1",
                "sha-v1",
                Some("sess-1"),
            )?;
            let (call, created) = WorkflowRuns(w).ensure_call(&NewWfCall {
                run_id: &run.id,
                workspace_id: "ws1",
                identity_hash: "id-hash-1",
                phase: Some("impl"),
                title: Some("Step 1"),
                prompt: "do work",
                options_canonical_json: "{}",
                schema_json: None,
                occurrence: 0,
                is_review: false,
                mutex_key: Some("deploy-staging"),
            })?;
            assert!(created);
            WorkflowRuns(w).create_attempt(&NewWfAttempt {
                call_id: &call.id,
                run_id: &run.id,
                parent_item_id: "item-1",
                parent_cycle: 1,
                attempt_no: 1,
                child_item_id: Some("child-1"),
                bot_id: Some("bot-coder"),
                placement_id: Some("pl-1"),
                harness: Some("claude-code"),
                model: Some("sonnet"),
                status: WfAttemptStatus::Dispatched,
                dispatched_at_revision: 1,
                error_code: None,
                error_message: None,
            })?;
            Ok::<_, StoreError>((run.id, call.id))
        })
        .unwrap();

    // Second concurrent active attempt on the same call MUST violate `wf_call_attempts_one_active`.
    let dup_active = t.store.write(|w| {
        WorkflowRuns(w).create_attempt(&NewWfAttempt {
            call_id: &call_id,
            run_id: &run_id,
            parent_item_id: "item-1",
            parent_cycle: 1,
            attempt_no: 2,
            child_item_id: Some("child-2"),
            bot_id: Some("bot-coder"),
            placement_id: None,
            harness: None,
            model: None,
            status: WfAttemptStatus::Dispatched,
            dispatched_at_revision: 1,
            error_code: None,
            error_message: None,
        })
    });
    assert!(
        dup_active.is_err(),
        "duplicate active attempt must fail DB unique index"
    );

    // Stage closing on delivery_revision = 3; finalizing with wrong delivery_revision = 2 must fail.
    t.store
        .write(|w| {
            WorkflowRuns(w).stage_attempt_closing("child-1", 3, r#"{"ok":true}"#, r#"["a1"]"#)
        })
        .unwrap();
    let wrong_rev = t
        .store
        .write(|w| WorkflowRuns(w).finalize_attempt("child-1", 2, "completed", None, None));
    assert!(matches!(wrong_rev, Err(StoreError::Conflict(_))));

    // Finalizing with exact delivery_revision = 3 succeeds atomically.
    let fin = t
        .store
        .write(|w| WorkflowRuns(w).finalize_attempt("child-1", 3, "completed", None, None))
        .unwrap()
        .expect("finalized");
    assert_eq!(fin.status, WfAttemptStatus::Succeeded);
    assert_eq!(fin.new_run_revision, 2);

    // Second succeeded attempt on the same call MUST violate `wf_call_attempts_one_succeeded`.
    let dup_succ = t.store.write(|w| {
        WorkflowRuns(w).create_attempt(&NewWfAttempt {
            call_id: &call_id,
            run_id: &run_id,
            parent_item_id: "item-1",
            parent_cycle: 1,
            attempt_no: 2,
            child_item_id: Some("child-3"),
            bot_id: Some("bot-coder"),
            placement_id: None,
            harness: None,
            model: None,
            status: WfAttemptStatus::Succeeded,
            dispatched_at_revision: 2,
            error_code: None,
            error_message: None,
        })
    });
    assert!(
        dup_succ.is_err(),
        "duplicate succeeded attempt must fail DB unique index"
    );

    // Mutex lock clears once call settled.
    let locked = t
        .store
        .read(|r| WorkflowRuns(r).has_active_mutex("ws1", "deploy-staging", "other-call"))
        .unwrap();
    assert!(!locked);
}

#[test]
fn review_gate_enforces_all_four_hard_rules_and_cycle_budget_persists_across_script_versions() {
    let t = fresh();
    let (run_id, work_call_id) = t
        .store
        .write(|w| {
            let run = WorkflowRuns(w).create_run("ws1", "parent-1", 1, 1, "v1", "sha1", None)?;
            let (work, _) = WorkflowRuns(w).ensure_call(&NewWfCall {
                run_id: &run.id,
                workspace_id: "ws1",
                identity_hash: "work-hash",
                phase: Some("build"),
                title: Some("Build"),
                prompt: "build",
                options_canonical_json: "{}",
                schema_json: None,
                occurrence: 0,
                is_review: false,
                mutex_key: None,
            })?;
            WorkflowRuns(w).record_version_call(&run.id, 1, 0, &work.id, None)?;
            WorkflowRuns(w).create_attempt(&NewWfAttempt {
                call_id: &work.id,
                run_id: &run.id,
                parent_item_id: "parent-1",
                parent_cycle: 1,
                attempt_no: 1,
                child_item_id: Some("c-work"),
                bot_id: Some("bot-dev"),
                placement_id: None,
                harness: None,
                model: None,
                status: WfAttemptStatus::Dispatched,
                dispatched_at_revision: 1,
                error_code: None,
                error_message: None,
            })?;
            Ok::<_, StoreError>((run.id, work.id))
        })
        .unwrap();

    // Active non-review work blocks review gate (Rule 4).
    assert_eq!(
        t.store
            .read(|r| WorkflowRuns(r).verify_independent_review(&run_id))
            .unwrap(),
        ReviewGateVerdict::Blocked {
            reason: ReviewGateFailure::ActiveWorkRemaining
        }
    );

    // Settle work call -> now MissingReview.
    let work_settled_rev = t
        .store
        .write(|w| {
            WorkflowRuns(w).stage_attempt_closing("c-work", 1, r#"{"done":true}"#, "[]")?;
            let fin = WorkflowRuns(w)
                .finalize_attempt("c-work", 1, "completed", None, None)?
                .unwrap();
            Ok::<_, StoreError>(fin.new_run_revision)
        })
        .unwrap();
    assert_eq!(
        t.store
            .read(|r| WorkflowRuns(r).verify_independent_review(&run_id))
            .unwrap(),
        ReviewGateVerdict::Blocked {
            reason: ReviewGateFailure::MissingReview
        }
    );

    // Same-bot review fails Rule 1.
    t.store
        .write(|w| {
            let (rev1, _) = WorkflowRuns(w).ensure_call(&NewWfCall {
                run_id: &run_id,
                workspace_id: "ws1",
                identity_hash: "rev-same-bot",
                phase: Some("review"),
                title: Some("Self Review"),
                prompt: "review",
                options_canonical_json: r#"{"review":true}"#,
                schema_json: None,
                occurrence: 0,
                is_review: true,
                mutex_key: None,
            })?;
            WorkflowRuns(w).create_attempt(&NewWfAttempt {
                call_id: &rev1.id,
                run_id: &run_id,
                parent_item_id: "parent-1",
                parent_cycle: 1,
                attempt_no: 1,
                child_item_id: Some("c-rev-same"),
                bot_id: Some("bot-dev"),
                placement_id: None,
                harness: None,
                model: None,
                status: WfAttemptStatus::Dispatched,
                dispatched_at_revision: work_settled_rev + 1,
                error_code: None,
                error_message: None,
            })?;
            WorkflowRuns(w).stage_attempt_closing("c-rev-same", 1, r#"{"approved":true}"#, "[]")?;
            WorkflowRuns(w).finalize_attempt("c-rev-same", 1, "completed", None, None)?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert_eq!(
        t.store
            .read(|r| WorkflowRuns(r).verify_independent_review(&run_id))
            .unwrap(),
        ReviewGateVerdict::Blocked {
            reason: ReviewGateFailure::SameBotAsWorker {
                bot_id: "bot-dev".into()
            }
        }
    );

    // Different bot, but dispatched at stale revision <= work_settled_rev fails Rule 2.
    t.store
        .write(|w| {
            let (rev2, _) = WorkflowRuns(w).ensure_call(&NewWfCall {
                run_id: &run_id,
                workspace_id: "ws1",
                identity_hash: "rev-stale",
                phase: Some("review"),
                title: Some("Stale Review"),
                prompt: "review stale",
                options_canonical_json: r#"{"review":true}"#,
                schema_json: None,
                occurrence: 0,
                is_review: true,
                mutex_key: None,
            })?;
            WorkflowRuns(w).create_attempt(&NewWfAttempt {
                call_id: &rev2.id,
                run_id: &run_id,
                parent_item_id: "parent-1",
                parent_cycle: 1,
                attempt_no: 1,
                child_item_id: Some("c-rev-stale"),
                bot_id: Some("bot-reviewer"),
                placement_id: None,
                harness: None,
                model: None,
                status: WfAttemptStatus::Dispatched,
                dispatched_at_revision: work_settled_rev,
                error_code: None,
                error_message: None,
            })?;
            WorkflowRuns(w).stage_attempt_closing(
                "c-rev-stale",
                1,
                r#"{"approved":true}"#,
                "[]",
            )?;
            WorkflowRuns(w).finalize_attempt("c-rev-stale", 1, "completed", None, None)?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert!(matches!(
        t.store
            .read(|r| WorkflowRuns(r).verify_independent_review(&run_id))
            .unwrap(),
        ReviewGateVerdict::Blocked {
            reason: ReviewGateFailure::SameBotAsWorker { .. }
                | ReviewGateFailure::StaleReview { .. }
        }
    ));

    // Revise script to v2: cycle attempt count persists, hydrated work call records version 1,
    // and fresh independent review passes all 4 rules!
    let fresh_rev_id = t
        .store
        .write(|w| {
            let run = WorkflowRuns(w).revise_script_version(&run_id, "v2", "sha2", None)?;
            assert_eq!(run.active_script_version, 2);
            let orig_ver = WorkflowRuns(w).first_version_of_call(&run_id, &work_call_id)?;
            assert_eq!(orig_ver, Some(1));
            WorkflowRuns(w).record_version_call(&run_id, 2, 0, &work_call_id, orig_ver)?;

            let (rev3, _) = WorkflowRuns(w).ensure_call(&NewWfCall {
                run_id: &run_id,
                workspace_id: "ws1",
                identity_hash: "rev-fresh",
                phase: Some("review"),
                title: Some("Fresh Review"),
                prompt: "review fresh",
                options_canonical_json: r#"{"review":true}"#,
                schema_json: None,
                occurrence: 0,
                is_review: true,
                mutex_key: None,
            })?;
            WorkflowRuns(w).create_attempt(&NewWfAttempt {
                call_id: &rev3.id,
                run_id: &run_id,
                parent_item_id: "parent-1",
                parent_cycle: 1,
                attempt_no: 1,
                child_item_id: Some("c-rev-fresh"),
                bot_id: Some("bot-reviewer"),
                placement_id: None,
                harness: None,
                model: None,
                status: WfAttemptStatus::Dispatched,
                dispatched_at_revision: run.run_revision,
                error_code: None,
                error_message: None,
            })?;
            WorkflowRuns(w).stage_attempt_closing(
                "c-rev-fresh",
                1,
                r#"{"approved":true}"#,
                "[]",
            )?;
            WorkflowRuns(w).finalize_attempt("c-rev-fresh", 1, "completed", None, None)?;
            Ok::<_, StoreError>(rev3.id)
        })
        .unwrap();

    assert_eq!(
        t.store
            .read(|r| WorkflowRuns(r).count_cycle_attempts("parent-1", 1))
            .unwrap(),
        4
    );
    assert_eq!(
        t.store
            .read(|r| WorkflowRuns(r).verify_independent_review(&run_id))
            .unwrap(),
        ReviewGateVerdict::Verified {
            review_call_id: fresh_rev_id.clone()
        }
    );

    let completed = t
        .store
        .write(|w| {
            WorkflowRuns(w).complete_run(&run_id, r#"{"shipped":true}"#, Some(&fresh_rev_id))
        })
        .unwrap();
    assert_eq!(completed.status, WfRunStatus::Completed);
    assert_eq!(
        t.store
            .read(|r| WorkflowRuns(r).get_call(&work_call_id))
            .unwrap()
            .unwrap()
            .status,
        WfCallStatus::Completed
    );
}
