use super::*;
use crate::disk_observation::{Interpretation, Phase, Policy};

fn record(operation: &str, incarnation: u64) -> Record {
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
        resource: "item:statistics-fault-fixture".into(),
        workspace_incarnation: 1,
        workspace_revision: Revision::new("r1").unwrap(),
        node_incarnation: incarnation,
        phase: Phase::Finished,
        triggered_at: 1000,
        sampled_at: None,
        completed_at: None,
        policy: Policy::default(),
        state: State::Pending,
        path: "/unused-statistics-fault-fixture".into(),
        requires_root: true,
    }
}

fn retained_record(incarnation: u64) -> Record {
    let mut r = record("retained", incarnation);
    r.state = State::Unavailable(Unavailable::Disabled);
    r.completed_at = Some(1100);
    r
}

fn record_that_expires_older_rows(incarnation: u64) -> Record {
    let mut r = record("rejected", incarnation);
    r.triggered_at = 5000;
    r.policy.keep_ms = 10;
    r
}

fn marker(journal: &Journal, key: &str) -> Option<i64> {
    journal
        .read(|t| {
            t.c()
                .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                .optional()
                .map_err(internal)
        })
        .unwrap()
}

#[test]
fn disk_statistics_abort_restores_retention_and_commits_business_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    let retained = retained_record(incarnation);
    journal
        .tx(|t| {
            t.queue_disk_record(&retained)?;
            t.c()
                .execute_batch(
                    "CREATE TRIGGER reject_disk_insert BEFORE INSERT ON disk_observations
                     BEGIN SELECT RAISE(ABORT, 'injected statistics insert failure'); END;",
                )
                .map_err(internal)
        })
        .unwrap();

    journal
        .tx(|t| {
            t.bump("business_before_statistics")?;
            // Retention deletes the old completed row before this INSERT reaches the trigger.
            // A successful fallback must undo that DELETE as well as the failed INSERT.
            t.optional_disk_update(|| {
                t.queue_disk_record(&record_that_expires_older_rows(incarnation))
            })?;
            assert!(!t.c().is_autocommit());
            t.bump("business_after_statistics")?;
            Ok(())
        })
        .unwrap();

    assert_eq!(marker(&journal, "business_before_statistics"), Some(1));
    assert_eq!(marker(&journal, "business_after_statistics"), Some(1));
    let page = journal
        .read(|t| t.disk_records(journal.id(), 0, 10))
        .unwrap();
    assert_eq!(page.records, vec![retained]);
    assert_eq!(page.skipped, 0);
}

#[test]
fn disk_statistics_rollback_does_not_downgrade_a_lost_business_transaction() {
    let tmp = tempfile::tempdir().unwrap();
    let (journal, incarnation) = Journal::open(&tmp.path().join("journal.sqlite3")).unwrap();
    let retained = retained_record(incarnation);
    journal
        .tx(|t| {
            t.queue_disk_record(&retained)?;
            t.c()
                .execute_batch(
                    "CREATE TRIGGER reject_disk_insert BEFORE INSERT ON disk_observations
                     BEGIN SELECT RAISE(ROLLBACK, 'injected outer transaction rollback'); END;",
                )
                .map_err(internal)
        })
        .unwrap();

    let result = journal.tx(|t| {
        t.bump("business_before_statistics")?;
        t.optional_disk_update(|| {
            t.queue_disk_record(&record_that_expires_older_rows(incarnation))
        })?;
        // Swallowing ROLLBACK could otherwise commit this write in autocommit mode even if
        // Journal::tx later reports that its final COMMIT no longer has a transaction.
        t.bump("business_after_statistics")?;
        Ok(())
    });
    assert!(result.is_err(), "a lost business transaction must fail");
    assert_eq!(marker(&journal, "business_before_statistics"), None);
    assert_eq!(marker(&journal, "business_after_statistics"), None);
    assert_eq!(
        journal
            .read(|t| t.disk_records(journal.id(), 0, 10))
            .unwrap()
            .records,
        vec![retained]
    );
}

#[test]
fn disk_statistics_update_abort_preserves_restart_recovery_and_incarnation() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("journal.sqlite3");
    let (journal, incarnation) = Journal::open(&path).unwrap();
    let id = journal.id().to_string();
    let pending = record("running", incarnation);
    journal
        .tx(|t| {
            t.accept(
                &pending.operation,
                &Op {
                    method: "start_turn".into(),
                    digest: Digest::sha256(b"restart statistics fault fixture"),
                    receipt: Receipt {
                        operation: pending.operation.clone(),
                        state: Observed::Running,
                    },
                    execution: Some(pending.execution.clone()),
                    workspace: Some(pending.workspace.clone()),
                    cancel: false,
                },
                None,
                1000,
            )?;
            t.queue_disk_record(&pending)?;
            // A persistent trigger also faults the fresh connection opened during restart.
            t.c()
                .execute_batch(
                    "CREATE TRIGGER reject_disk_update BEFORE UPDATE ON disk_observations
                     BEGIN SELECT RAISE(ABORT, 'injected statistics recovery failure'); END;",
                )
                .map_err(internal)
        })
        .unwrap();
    drop(journal);

    let (journal, restarted) = Journal::open(&path).unwrap();
    assert_eq!(journal.id(), id);
    assert_eq!(restarted, incarnation + 1);
    assert!(matches!(
        journal
            .read(|t| t.op(&pending.operation))
            .unwrap()
            .unwrap()
            .receipt
            .state,
        Observed::Unknown { .. }
    ));
    assert_eq!(
        journal
            .read(|t| t.disk_records(journal.id(), 0, 10))
            .unwrap()
            .records,
        vec![pending.clone()],
        "failed optional recovery preserves the original observation instead of fabricating one"
    );

    journal
        .tx(|t| {
            t.c()
                .execute_batch("DROP TRIGGER reject_disk_update")
                .map_err(internal)
        })
        .unwrap();
    drop(journal);
    let (journal, recovered) = Journal::open(&path).unwrap();
    assert_eq!(recovered, restarted + 1);
    let page = journal
        .read(|t| t.disk_records(journal.id(), 0, 10))
        .unwrap();
    assert_eq!(page.records.len(), 1);
    let record = &page.records[0];
    assert_eq!(record.operation, pending.operation);
    assert_eq!(record.node_incarnation, incarnation);
    assert_eq!(record.state, State::Unavailable(Unavailable::Restarted));
    assert_eq!(record.sampled_at, None);
    assert!(record.completed_at.is_some());
}
