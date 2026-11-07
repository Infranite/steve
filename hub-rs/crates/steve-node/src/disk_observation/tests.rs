use super::*;
use crate::disk_usage::{Incomplete, Outcome, Usage};
use crate::journal::Journal;

fn record(operation: &str, phase: Phase, incarnation: u64) -> Record {
    Record {
        interpretation: Interpretation::AsynchronousUnattributedPathStock,
        operation: OperationId::new(operation).unwrap(),
        execution: ExecutionRef::Turn {
            session: SessionId::new("session").unwrap(),
            turn: TurnId::new(format!("turn_{operation}")).unwrap(),
            attempt: AttemptId::new(format!("attempt_{operation}")).unwrap(),
        },
        workspace: WorkspaceRef {
            node: NodeId::new("node").unwrap(),
            id: WorkspaceId::new("work").unwrap(),
        },
        resource: "item:original-resource".into(),
        workspace_incarnation: 1,
        workspace_revision: Revision::new("r1").unwrap(),
        node_incarnation: incarnation,
        phase,
        triggered_at: 1000,
        sampled_at: None,
        completed_at: None,
        policy: Policy::default(),
        state: State::Pending,
        path: "/unused-test-path".into(),
        requires_root: true,
    }
}

fn done(mut record: Record) -> Record {
    record.completed_at = Some(1100);
    record.state = State::Observed(Measurement {
        outcome: Outcome::Incomplete {
            observed: Usage {
                apparent_bytes: u64::MAX,
                allocated_bytes: 512,
                inodes: 1,
            },
            reason: Incomplete::EntryBudget,
        },
        entries: record.policy.budget.entries,
        elapsed: Duration::from_millis(1),
        missing_roots: 2,
        device: None,
    });
    record
}

#[test]
fn configuration_is_opt_in_validated_and_all_numbers_are_configurable() {
    assert_eq!(Policy::configured(|_| None).unwrap(), None);
    let values = std::collections::HashMap::from([
        ("STEVE_DISK_OBSERVE", "on"),
        ("STEVE_DISK_OBSERVE_ENTRIES", "12345"),
        ("STEVE_DISK_OBSERVE_DEPTH", "123"),
        ("STEVE_DISK_OBSERVE_MS", "1234"),
        ("STEVE_DISK_OBSERVE_ROWS", "123456"),
        ("STEVE_DISK_OBSERVE_KEEP_MS", "1234567"),
    ]);
    let policy = Policy::configured(|key| values.get(key).map(|s| s.to_string()))
        .unwrap()
        .unwrap();
    assert_eq!(policy.budget.entries, 12345);
    assert_eq!(policy.budget.depth, 123);
    assert_eq!(policy.budget.elapsed, Duration::from_millis(1234));
    assert_eq!(policy.keep_rows, 123456);
    assert_eq!(policy.keep_ms, 1234567);
    assert!(
        Policy::configured(|key| (key == "STEVE_DISK_OBSERVE").then(|| "maybe".into())).is_err()
    );
    for bad in ["0", "-1", "18446744073709551616", "not-a-number"] {
        assert!(
            Policy::configured(|key| match key {
                "STEVE_DISK_OBSERVE" => Some("on".into()),
                "STEVE_DISK_OBSERVE_ENTRIES" => Some(bad.into()),
                _ => None,
            })
            .is_err()
        );
    }
}

#[test]
fn record_is_atomic_with_its_transaction_and_resends_do_not_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    let r = record("op", Phase::Finished, incarnation);
    let result: ApiResult<()> = journal.tx(|t| {
        t.queue_disk_record(&r)?;
        Err(internal("rollback the phase"))
    });
    assert!(result.is_err());
    assert!(
        journal
            .read(|t| t.disk_records(journal.id(), 0, 10))
            .unwrap()
            .records
            .is_empty()
    );
    journal
        .tx(|t| {
            t.queue_disk_record(&r)?;
            t.queue_disk_record(&r)
        })
        .unwrap();
    let page = journal
        .read(|t| t.disk_records(journal.id(), 0, 10))
        .unwrap();
    assert_eq!(page.records, vec![r]);
    assert_eq!(page.skipped, 0);
    assert_eq!(page.journal, journal.id());
}

#[test]
fn partial_measurements_and_large_byte_values_survive_json_without_precision_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    journal
        .tx(|t| t.queue_disk_record(&record("op", Phase::Finished, incarnation)))
        .unwrap();
    let running = journal.tx(|t| t.claim_disk_record(1050)).unwrap().unwrap();
    let finished = done(running);
    journal.tx(|t| t.finish_disk_record(&finished)).unwrap();
    assert_eq!(
        journal
            .read(|t| t.disk_records(journal.id(), 0, 10))
            .unwrap()
            .records,
        vec![finished]
    );
}

#[test]
fn restart_marks_pending_and_sampling_unavailable_without_resampling() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal.sqlite3");
    let (journal, incarnation) = Journal::open(&path).unwrap();
    let id = journal.id().to_string();
    journal
        .tx(|t| {
            for op in ["running", "pending", "finished"] {
                t.queue_disk_record(&record(op, Phase::Finished, incarnation))?;
            }
            let running = t.claim_disk_record(1050)?.unwrap();
            assert_eq!(running.operation.as_str(), "running");
            let mut completed = t.claim_disk_record(1060)?.unwrap();
            // Complete one claimed record; leave one sampling and one pending.
            completed = done(completed);
            t.finish_disk_record(&completed)
        })
        .unwrap();
    drop(journal);
    let (journal, next) = Journal::open(&path).unwrap();
    assert_eq!(journal.id(), id);
    assert!(next > incarnation);
    let page = journal
        .read(|t| t.disk_records(journal.id(), 0, 10))
        .unwrap();
    assert_eq!(page.records.len(), 3);
    assert_eq!(
        page.records
            .iter()
            .filter(|r| r.state == State::Unavailable(Unavailable::Restarted))
            .count(),
        2
    );
    assert_eq!(
        page.records
            .iter()
            .filter(|r| matches!(r.state, State::Observed(_)))
            .count(),
        1
    );
    assert!(journal.tx(|t| t.claim_disk_record(2000)).unwrap().is_none());
}

#[test]
fn retention_keeps_inflight_work_counts_skips_and_never_resurrects_deleted_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    let mut a = record("a", Phase::Finished, incarnation);
    a.policy.keep_rows = 1;
    let mut b = record("b", Phase::Finished, incarnation);
    b.policy.keep_rows = 1;
    journal
        .tx(|t| {
            t.queue_disk_record(&a)?;
            t.queue_disk_record(&b)
        })
        .unwrap();
    let page = journal
        .read(|t| t.disk_records(journal.id(), 0, 10))
        .unwrap();
    assert_eq!(page.skipped, 1);
    assert_eq!(page.records, vec![a]);
    let cursor = page.next.unwrap();
    let finished = done(journal.tx(|t| t.claim_disk_record(1050)).unwrap().unwrap());
    journal
        .tx(|t| {
            t.finish_disk_record(&finished)?;
            t.queue_disk_record(&b)?;
            t.finish_disk_record(&finished)
        })
        .unwrap();
    let page = journal
        .read(|t| t.disk_records(journal.id(), cursor, 10))
        .unwrap();
    assert_eq!(page.records, vec![b]);
    assert!(page.next.unwrap() > cursor);
}

#[test]
fn age_retention_and_unknown_execution_preserve_their_meaning() {
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    journal
        .tx(|t| {
            t.queue_disk_record(&record("old", Phase::Finished, incarnation))?;
            let old = done(t.claim_disk_record(1050)?.unwrap());
            t.finish_disk_record(&old)?;
            let mut unknown = record("unknown", Phase::Unknown, incarnation);
            unknown.triggered_at = 5000;
            unknown.completed_at = Some(5000);
            unknown.policy.keep_ms = 100;
            unknown.state = State::Unavailable(Unavailable::UnknownExecution);
            t.queue_disk_record(&unknown)?;
            assert!(t.claim_disk_record(5001)?.is_none());
            Ok(())
        })
        .unwrap();
    let page = journal
        .read(|t| t.disk_records(journal.id(), 0, 10))
        .unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(
        page.records[0].state,
        State::Unavailable(Unavailable::UnknownExecution)
    );
}

#[test]
fn a_late_scan_result_cannot_overwrite_disabled_or_stopping_records() {
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    for (op, why) in [
        ("disabled", Unavailable::Disabled),
        ("stopping", Unavailable::Stopping),
    ] {
        journal
            .tx(|t| {
                t.queue_disk_record(&record(op, Phase::Finished, incarnation))?;
                let sampled = done(t.claim_disk_record(1050)?.unwrap());
                t.interrupt_disk_records(why, 1060)?;
                t.finish_disk_record(&sampled)
            })
            .unwrap();
        let page = journal
            .read(|t| t.disk_records(journal.id(), 0, 10))
            .unwrap();
        let r = page
            .records
            .iter()
            .find(|r| r.operation.as_str() == op)
            .unwrap();
        assert_eq!(r.state, State::Unavailable(why));
        assert_eq!(r.completed_at, Some(1060));
    }
}

#[test]
fn unknown_operation_invalidates_started_even_while_session_keeps_active_execution() {
    use crate::journal::{Op, SessionRow};
    use steve_node_api::testkit::build;
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    let r = record("op", Phase::Started, incarnation);
    let mut open = build::open("session", "node");
    open.workspace = r.workspace.clone();
    journal
        .tx(|t| {
            t.put_session(&SessionRow {
                view: SessionView {
                    id: open.session.clone(),
                    revision: Revision::new("session-r1").unwrap(),
                    phase: SessionPhase::Ready,
                    active: Some(r.execution.clone()),
                    effective: open.choice.clone(),
                    effective_permissions: open.policy.permissions,
                    guarantees: vec![],
                    resume: None,
                },
                open,
                incarnation,
                acp_id: None,
            })?;
            t.accept(
                &r.operation,
                &Op {
                    method: "start_turn".into(),
                    digest: Digest::sha256(b"fixture"),
                    receipt: Receipt {
                        operation: r.operation.clone(),
                        state: Observed::Running,
                    },
                    execution: Some(r.execution.clone()),
                    workspace: Some(r.workspace.clone()),
                    cancel: false,
                },
                None,
                1000,
            )?;
            assert!(phase_current(t, &r)?);
            t.set_state(
                &r.operation,
                &Observed::Unknown {
                    reason: "not confirmed stopped".into(),
                },
            )?;
            assert!(!phase_current(t, &r)?);
            assert!(
                t.session(&SessionId::new("session").unwrap())?
                    .unwrap()
                    .view
                    .active
                    .is_some()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_reused_workspace_id_does_not_revalidate_old_observations() {
    use crate::journal::WorkspaceRow;
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    let r = record("op", Phase::Finished, incarnation);
    journal
        .tx(|t| {
            let mut work = WorkspaceRow {
                resource: r.resource.clone(),
                spec: Digest::sha256(b"workspace"),
                kind: PrepareKind::Scratch,
                repo: None,
                view: WorkspaceView {
                    workspace: r.workspace.clone(),
                    revision: r.workspace_revision.clone(),
                    usable: true,
                    branch: None,
                    display: "work".into(),
                },
                path: r.path.to_str().unwrap().into(),
                incarnation: r.workspace_incarnation,
            };
            t.put_workspace(&work)?;
            assert!(same_workspace(t, &r)?);
            work.incarnation += 1;
            t.put_workspace(&work)?;
            assert!(!same_workspace(t, &r)?);
            work.incarnation -= 1;
            work.view.revision = Revision::new("r2").unwrap();
            t.put_workspace(&work)?;
            assert!(!same_workspace(t, &r)?);
            work.view.revision = r.workspace_revision.clone();
            work.path = "/a-different-workspace".into();
            t.put_workspace(&work)?;
            assert!(!same_workspace(t, &r)?);
            Ok(())
        })
        .unwrap();
}
