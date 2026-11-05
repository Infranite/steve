mod common;
use common::*;
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_store::{StoreError, Tx, db, jobs::*, runtime};

fn job(id: &str) -> NewJob<'_> {
    NewJob {
        id,
        kind: JobKind::Think,
        purpose: "answer",
        workspace: "default",
        node: "n",
        thread_run: None,
    }
}

struct Spec {
    execution: Value,
    spec: Value,
    request: Value,
}
fn spec(job: &str, attempt: &str) -> Spec {
    Spec {
        execution: json!({"type":"job","job":job,"attempt":attempt}),
        spec: json!({"job":job,"model":"m"}),
        request: json!({"spec":{"job":job},"attempt":attempt}),
    }
}
fn attempt<'a>(id: &'a str, ordinal: i64, op: &'a str, s: &'a Spec) -> NewJobAttempt<'a> {
    NewJobAttempt {
        id,
        ordinal,
        model: "m",
        operation: op,
        execution: &s.execution,
        spec: &s.spec,
        digest: "d",
    }
}
fn start(s: &Spec) -> StartJob<'_> {
    StartJob {
        request: &s.request,
        digest: "d",
    }
}
/// Job `j` with attempt `a0` under operation `op0`.
fn create(tx: &Tx<'_>, j: &str) -> steve_store::Result<bool> {
    let s = spec(j, &format!("{j}-a0"));
    create_job_in(
        tx,
        &job(j),
        &attempt(&format!("{j}-a0"), 0, &format!("{j}-op0"), &s),
        &start(&s),
        1,
    )
}
fn next(tx: &Tx<'_>, j: &str, ordinal: i64) -> steve_store::Result<bool> {
    let a = format!("{j}-a{ordinal}");
    let s = spec(j, &a);
    next_attempt_in(
        tx,
        j,
        &attempt(&a, ordinal, &format!("{j}-op{ordinal}"), &s),
        &start(&s),
        2,
    )
}
fn finish(tx: &Tx<'_>, a: &str) -> steve_store::Result<bool> {
    apply_attempt_in(
        tx,
        a,
        AttemptState::Finished,
        None,
        Some(&json!({"result":[{"type":"text","text":"ok"}]})),
        3,
    )
}
fn is_conflict<T: std::fmt::Debug>(r: steve_store::Result<T>, want: &str) {
    match r {
        Err(StoreError::Conflict(d)) => assert!(d.contains(want), "{d}"),
        other => panic!("expected conflict {want:?}, got {other:?}"),
    }
}

#[test]
fn applies_on_a_039_database_and_keeps_its_commands() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(39);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..39] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch("INSERT INTO node_commands(operation_id,node_id,method,request,digest,created,updated) VALUES ('old','n','open_session','{}','d',1,1);").unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().min_reader, db::SCHEMA_VERSION);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM node_commands WHERE operation_id='old' AND target_job_attempt IS NULL AND state='pending'"
        ),
        1
    );
    assert_eq!(count(&c, "SELECT count(*) FROM runtime_jobs"), 0);
    assert_eq!(
        count(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
}

#[test]
fn a_job_its_first_attempt_and_its_start_command_commit_together() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            assert!(create(tx, "j")?);
            // the same again changes nothing
            assert!(!create(tx, "j")?);
            let j = job_in(tx, "j")?.unwrap();
            assert_eq!(
                (j.kind, j.purpose.as_str(), j.node.as_str(), j.finished),
                (JobKind::Think, "answer", "n", false)
            );
            let a = attempt_in(tx, "j-a0")?.unwrap();
            assert_eq!(
                (a.job.as_str(), a.ordinal, a.state, a.operation.as_str()),
                ("j", 0, AttemptState::Planned, "j-op0")
            );
            // the outbox row is linked to its job attempt and leased like any other command
            let target: (String, String, Option<String>) = tx.query_row(
                "SELECT method,target_job_attempt,target_attempt FROM node_commands WHERE operation_id='j-op0'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(target, (START_JOB.into(), "j-a0".into(), None));
            let lease = runtime::lease_command_in(tx, "n", "w", 1, 10)?.unwrap();
            assert_eq!((lease.operation.as_str(), lease.method.as_str()), ("j-op0", START_JOB));
            assert_eq!(lease.request, spec("j", "j-a0").request);
            Ok(())
        })
        .unwrap();
    // a failure after the insert rolls all three back
    let r = t.store.transaction::<(), StoreError>(|tx| {
        create(tx, "k")?;
        Err(StoreError::Conflict("later step failed".into()))
    });
    assert!(r.is_err());
    t.store
        .with_conn(|c| {
            assert_eq!(
                count(c, "SELECT count(*) FROM runtime_jobs WHERE job_id='k'"),
                0
            );
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM node_commands WHERE operation_id='k-op0'"
                ),
                0
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn identities_are_immutable() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create(tx, "j")?;
            let mut other = job("j");
            other.purpose = "eval";
            let s = spec("j", "j-a0");
            is_conflict(
                create_job_in(tx, &other, &attempt("j-a0", 0, "j-op0", &s), &start(&s), 1),
                "job identity reused",
            );
            let changed = Spec {
                spec: json!({"other":true}),
                ..spec("j", "j-a0")
            };
            is_conflict(
                create_job_in(
                    tx,
                    &job("j"),
                    &attempt("j-a0", 0, "j-op0", &changed),
                    &start(&changed),
                    1,
                ),
                "job attempt identity reused",
            );
            let resent = Spec {
                request: json!({"other":true}),
                ..spec("j", "j-a0")
            };
            is_conflict(
                create_job_in(
                    tx,
                    &job("j"),
                    &attempt("j-a0", 0, "j-op0", &resent),
                    &start(&resent),
                    1,
                ),
                "operation identity reused",
            );
            // a session command cannot reuse a job attempt's operation id
            is_conflict(
                runtime::insert_command_in(
                    tx,
                    &runtime::NewCommand {
                        operation: "j-op0",
                        node: "n",
                        method: START_JOB,
                        request: &spec("j", "j-a0").request,
                        digest: "d",
                        target_attempt: None,
                        dependency: None,
                    },
                    1,
                ),
                "operation identity reused",
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn one_open_attempt_per_job_in_ordinal_sequence() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            let s = spec("j", "j-a1");
            is_conflict(
                create_job_in(tx, &job("j"), &attempt("j-a1", 1, "j-op1", &s), &start(&s), 1),
                "starts with attempt 0",
            );
            create(tx, "j")?;
            is_conflict(next(tx, "j", 1), "already has an open attempt");
            // the index holds even past the repo
            assert!(
                tx.execute(
                    "INSERT INTO runtime_job_attempts(attempt_id,job_id,ordinal,model,node_id,operation_id,execution,spec,digest,created,updated) VALUES ('x','j',1,'m','n','x','{}','{}','d',1,1)",
                    [],
                )
                .is_err()
            );
            assert!(finish(tx, "j-a0")?);
            is_conflict(next(tx, "j", 2), "out of sequence");
            assert!(next(tx, "j", 1)?);
            assert!(!next(tx, "j", 1)?);
            assert!(apply_attempt_in(tx, "j-a1", AttemptState::Unknown, None, None, 3)?);
            assert!(finish(tx, "j-a1")?);
            assert!(next(tx, "j", 2)?);
            let ordinals: Vec<_> = attempts_of_job_in(tx, "j")?
                .into_iter()
                .map(|a| (a.ordinal, a.state))
                .collect();
            assert_eq!(
                ordinals,
                [
                    (0, AttemptState::Finished),
                    (1, AttemptState::Finished),
                    (2, AttemptState::Planned)
                ]
            );
            is_conflict(finish_job_in(tx, "j", 4), "still has an open attempt");
            assert!(finish(tx, "j-a2")?);
            assert!(finish_job_in(tx, "j", 4)?);
            assert!(!finish_job_in(tx, "j", 4)?);
            is_conflict(next(tx, "j", 3), "job is finished");
            is_conflict(next(tx, "nope", 0), "unknown job");
            Ok(())
        })
        .unwrap();
}

#[test]
fn progress_is_monotonic() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create(tx, "j")?;
            let accepted = json!({"accepted":true});
            assert!(apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Running,
                None,
                None,
                2
            )?);
            // a late Accepted does not move Running back, but its receipt is kept
            assert!(apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Accepted,
                Some(&accepted),
                None,
                2
            )?);
            assert!(!apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Accepted,
                Some(&json!({"again":1})),
                None,
                2
            )?);
            let a = attempt_in(tx, "j-a0")?.unwrap();
            assert_eq!(
                (a.state, a.receipt.as_ref()),
                (AttemptState::Running, Some(&accepted))
            );
            // unknown only closes with a terminal fact
            assert!(apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Unknown,
                None,
                None,
                2
            )?);
            assert!(!apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Running,
                None,
                None,
                2
            )?);
            is_conflict(
                apply_attempt_in(tx, "j-a0", AttemptState::Running, None, Some(&json!({})), 2),
                "only a finished attempt",
            );
            assert!(finish(tx, "j-a0")?);
            let revision = attempt_in(tx, "j-a0")?.unwrap().revision;
            // a late Accepted does not move Finished backwards; the same completion is a no-op
            assert!(!apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Accepted,
                Some(&accepted),
                None,
                4
            )?);
            assert!(!apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Running,
                None,
                None,
                4
            )?);
            assert!(!finish(tx, "j-a0")?);
            is_conflict(
                apply_attempt_in(
                    tx,
                    "j-a0",
                    AttemptState::Finished,
                    None,
                    Some(&json!({"other":1})),
                    4,
                ),
                "contradictory job completion",
            );
            let a = attempt_in(tx, "j-a0")?.unwrap();
            assert_eq!((a.state, a.revision), (AttemptState::Finished, revision));
            assert_eq!(
                a.completion,
                Some(json!({"result":[{"type":"text","text":"ok"}]}))
            );
            is_conflict(
                apply_attempt_in(tx, "nope", AttemptState::Running, None, None, 4),
                "unknown job attempt",
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn cancel_intent_and_open_attempts_at_boot() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create(tx, "j")?;
            create(tx, "k")?;
            assert!(finish(tx, "k-a0")?);
            assert!(cancel_attempt_in(tx, "j-a0", 2)?);
            assert!(!cancel_attempt_in(tx, "j-a0", 2)?);
            assert!(!cancel_attempt_in(tx, "k-a0", 2)?);
            let open = open_attempts_in(tx, "n")?;
            assert_eq!(open.len(), 1);
            assert_eq!(
                (open[0].id.as_str(), open[0].cancel_requested),
                ("j-a0", true)
            );
            assert!(open_attempts_in(tx, "other")?.is_empty());
            // the cancel goes out as another command on the same attempt
            assert!(insert_job_command_in(
                tx,
                &NewJobCommand {
                    operation: "j-cancel",
                    attempt: "j-a0",
                    method: "cancel",
                    request: &json!({"operation":"j-op0"}),
                    digest: "c",
                },
                2
            )?);
            is_conflict(
                insert_job_command_in(
                    tx,
                    &NewJobCommand {
                        operation: "x",
                        attempt: "nope",
                        method: "cancel",
                        request: &json!({}),
                        digest: "c",
                    },
                    2,
                ),
                "unknown job attempt",
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_command_targets_a_session_attempt_or_a_job_attempt() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create(tx, "j")?;
            assert!(
                tx.execute(
                    "UPDATE node_commands SET target_attempt='j-a0' WHERE operation_id='j-op0'",
                    [],
                )
                .is_err()
            );
            Ok(())
        })
        .unwrap();
}

fn settle(tx: &Tx<'_>, op: &str) -> steve_store::Result<()> {
    runtime::merge_receipt_in(tx, op, runtime::ReceiptState::Finished, true, &json!({}), 3)?;
    Ok(())
}

#[test]
fn prune_keeps_open_jobs_and_live_commands() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            // open: never pruned
            create(tx, "open")?;
            // finished, its command settled: pruned
            create(tx, "done")?;
            finish(tx, "done-a0")?;
            settle(tx, "done-op0")?;
            finish_job_in(tx, "done", 5)?;
            // finished, but its start command may still reach the Node
            create(tx, "sent")?;
            finish(tx, "sent-a0")?;
            finish_job_in(tx, "sent", 5)?;
            // finished, settled, but another command depends on it
            create(tx, "dep")?;
            finish(tx, "dep-a0")?;
            settle(tx, "dep-op0")?;
            finish_job_in(tx, "dep", 5)?;
            runtime::insert_command_in(
                tx,
                &runtime::NewCommand {
                    operation: "after",
                    node: "n",
                    method: "open_session",
                    request: &json!({}),
                    digest: "d",
                    target_attempt: None,
                    dependency: Some("dep-op0"),
                },
                5,
            )?;
            // finished too recently
            create(tx, "fresh")?;
            finish(tx, "fresh-a0")?;
            settle(tx, "fresh-op0")?;
            finish_job_in(tx, "fresh", 50)?;
            assert_eq!(prune_jobs_in(tx, 10, 100)?, 1);
            assert!(job_in(tx, "done")?.is_none());
            assert!(attempt_in(tx, "done-a0")?.is_none());
            assert!(runtime::command_in(tx, "done-op0")?.is_none());
            for kept in ["open", "sent", "dep", "fresh"] {
                assert!(job_in(tx, kept)?.is_some(), "{kept}");
            }
            assert!(attempt_in(tx, "open-a0")?.is_some());
            assert_eq!(prune_jobs_in(tx, 10, 100)?, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prune_takes_a_whole_job_with_every_attempt_and_command() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create(tx, "j")?;
            finish(tx, "j-a0")?;
            next(tx, "j", 1)?;
            insert_job_command_in(
                tx,
                &NewJobCommand {
                    operation: "j-cancel",
                    attempt: "j-a1",
                    method: "cancel",
                    request: &json!({}),
                    digest: "c",
                },
                2,
            )?;
            finish(tx, "j-a1")?;
            for op in ["j-op0", "j-op1", "j-cancel"] {
                settle(tx, op)?;
            }
            finish_job_in(tx, "j", 5)?;
            create(tx, "k")?;
            finish(tx, "k-a0")?;
            settle(tx, "k-op0")?;
            finish_job_in(tx, "k", 6)?;
            // oldest first, within the limit
            assert_eq!(prune_jobs_in(tx, 10, 1)?, 1);
            assert!(job_in(tx, "j")?.is_none());
            assert!(job_in(tx, "k")?.is_some());
            assert_eq!(
                count(
                    tx,
                    "SELECT count(*) FROM node_commands WHERE operation_id LIKE 'j-%'"
                ),
                0
            );
            assert_eq!(prune_jobs_in(tx, 10, 1)?, 1);
            assert_eq!(count(tx, "SELECT count(*) FROM runtime_job_attempts"), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_attempt_is_started_only_under_its_own_operation_id() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create(tx, "j")?;
            let s = spec("j", "j-a0");
            let second = NewJobCommand {
                operation: "j-op-again",
                attempt: "j-a0",
                method: START_JOB,
                request: &s.request,
                digest: "d",
            };
            is_conflict(
                insert_job_command_in(tx, &second, 2),
                "only under its own operation id",
            );
            // the attempt's own start again is the idempotent replay
            let own = NewJobCommand {
                operation: "j-op0",
                ..second
            };
            assert!(!insert_job_command_in(tx, &own, 2)?);
            assert_eq!(
                count(
                    tx,
                    "SELECT count(*) FROM node_commands WHERE target_job_attempt='j-a0'"
                ),
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_cancelled_job_never_falls_back() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            create(tx, "j")?;
            assert!(cancel_attempt_in(tx, "j-a0", 2)?);
            // the cancelled attempt ends the way a fallback would react to
            assert!(apply_attempt_in(
                tx,
                "j-a0",
                AttemptState::Finished,
                None,
                Some(&json!({"error":{"kind":"transient"}})),
                3
            )?);
            is_conflict(next(tx, "j", 1), "job was cancelled");
            assert_eq!(attempts_of_job_in(tx, "j")?.len(), 1);
            assert_eq!(
                count(
                    tx,
                    "SELECT count(*) FROM node_commands WHERE operation_id LIKE 'j-%'"
                ),
                1
            );
            // an uncancelled job still falls back
            create(tx, "k")?;
            assert!(finish(tx, "k-a0")?);
            assert!(next(tx, "k", 1)?);
            Ok(())
        })
        .unwrap();
}
