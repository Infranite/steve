mod common;
use common::*;
use rusqlite::{Connection, params};
use serde_json::json;
use steve_store::{Result, StoreError, db, runtime::*};

fn seed(c: &Connection) {
    c.execute_batch("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s','default','test','.','active',1,1),('legacy','default','test','.','active',1,1);
        INSERT INTO queued(id,session_id,text,via,created,files) VALUES ('q','s','hello','user',1,'[]');").unwrap();
}
fn ready(t: &T) {
    t.store
        .transaction::<_, StoreError>(|tx| {
            seed(tx);
            assert!(bind_session_in(
                tx,
                &SessionBinding {
                    session: "s",
                    node: "n",
                    workspace: &json!({"id":"w"}),
                    node_session: "ns",
                    config_revision: "c"
                },
                1
            )?);
            assert!(session_cas_in(
                tx,
                "s",
                0,
                &SessionUpdate {
                    phase: "ready",
                    active_attempt: None,
                    release_pending: false
                },
                1
            )?);
            Ok(())
        })
        .unwrap();
}
fn attempt(tx: &steve_store::Tx<'_>) -> Result<()> {
    assert!(insert_attempt_in(
        tx,
        &NewAttempt {
            id: "a",
            turn: "t",
            ordinal: 0,
            node: "n",
            operation: "op",
            execution: &json!({"attempt":"a"}),
            spec: &json!({"prompt":"hello"}),
            digest: "digest"
        },
        2
    )?);
    Ok(())
}
fn command(tx: &steve_store::Tx<'_>, op: &str, dependency: Option<&str>) -> Result<bool> {
    insert_command_in(
        tx,
        &NewCommand {
            operation: op,
            node: "n",
            method: "run_attempt",
            request: &json!({"a":1}),
            digest: "digest",
            target_attempt: None,
            dependency,
        },
        1,
    )
}

#[test]
fn upgrade_preserves_old_rows_and_requires_new_reader() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(33);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..33] {
        c.execute_batch(m).unwrap();
    }
    seed(&c);
    c.execute_batch("INSERT INTO session_dispatch(id,session_id,text,via,created,state,updated) VALUES ('old','legacy','old text','user',1,'submitted',1);
      INSERT INTO events(session_id,ts,kind,data) VALUES ('legacy',1,'system','{}');
      INSERT INTO agent_turns(id,workspace_id,session_id,harness,started) VALUES ('tn','default','legacy','test',1);").unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().min_reader, db::SCHEMA_VERSION);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM queued WHERE id='q' AND text='hello' AND files='[]'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM session_dispatch WHERE id='old' AND text='old text' AND backend='legacy' AND turn_id IS NULL AND state='submitted'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM events WHERE format='legacy.acp' AND source_node IS NULL AND data='{}'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM agent_turns WHERE id='tn' AND node_attempt_id IS NULL AND logical_turn_id IS NULL"
        ),
        1
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
}

#[test]
fn same_session_concurrent_claim_has_one_winner_and_one_durable_batch() {
    let t = fresh();
    ready(&t);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let joins: Vec<_> = (0..2)
        .map(|n| {
            let s = t.store.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                s.transaction::<_, StoreError>(|tx| claim_in(tx, "s", &format!("t{n}"), 2))
            })
        })
        .collect();
    let results: Vec<_> = joins.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    t.store
        .with_conn(|c| {
            assert_eq!(count(c, "SELECT count(*) FROM runtime_turns"), 1);
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM session_dispatch WHERE backend='node'"
                ),
                1
            );
            assert_eq!(count(c, "SELECT count(*) FROM queued"), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn plan_rolls_back_as_a_unit_and_reclaim_is_idempotent() {
    let t = fresh();
    ready(&t);
    let failed: Result<()> = t.store.transaction(|tx| {
        claim_in(tx, "s", "t", 2)?;
        attempt(tx)?;
        command(tx, "op", None)?;
        Err(StoreError::Conflict("injected before commit".into()))
    });
    assert!(failed.is_err());
    t.store
        .with_conn(|c| {
            assert_eq!(count(c, "SELECT count(*) FROM queued"), 1);
            assert_eq!(count(c, "SELECT count(*) FROM runtime_turns"), 0);
            assert_eq!(count(c, "SELECT count(*) FROM runtime_attempts"), 0);
            assert_eq!(count(c, "SELECT count(*) FROM node_commands"), 0);
            Ok(())
        })
        .unwrap();
    t.store
        .transaction::<_, StoreError>(|tx| {
            let xs = claim_in(tx, "s", "t", 2)?;
            assert_eq!(xs.len(), 1);
            assert_eq!(claim_in(tx, "s", "t", 3)?, xs);
            attempt(tx)?;
            assert!(attempt_cas_in(tx, "a", 0, "unknown", None, None, 3)?);
            assert!(!attempt_cas_in(tx, "a", 0, "running", None, None, 4)?);
            assert!(!attempt_cas_in(tx, "a", 1, "running", None, None, 4)?);
            assert!(claim_in(tx, "s", "next", 4).is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn legacy_recovery_and_receipts_do_not_touch_node_dispatch() {
    let t = fresh();
    ready(&t);
    t.store.transaction::<_,StoreError>(|tx| {
        claim_in(tx,"s","t",2)?;
        dispatch_receipt_in(tx,"t","submitted",2)?;
        tx.execute("INSERT INTO session_dispatch(id,session_id,text,via,created,state,updated) VALUES ('old','legacy','legacy text','user',1,'submitted',1)",[])?;
        Ok(())
    }).unwrap();
    assert!(t.store.queued().claim("s").is_err());
    assert!(t.store.queued().submitted("s").is_err());
    assert!(t.store.queued().defer_claimed("s").is_err());
    t.store.queued().receipt("s", "completed").unwrap();
    t.store.queued().recover_dispatches().unwrap();
    t.store.queued().recover_dispatches().unwrap();
    t.store.with_conn(|c| {
        assert_eq!(count(c,"SELECT count(*) FROM session_dispatch WHERE backend='node' AND state='submitted' AND turn_id='t'"),1);
        assert_eq!(count(c,"SELECT count(*) FROM queued WHERE session_id='s'"),0);
        assert_eq!(count(c,"SELECT count(*) FROM session_resume WHERE session_id='s'"),0);
        assert_eq!(count(c,"SELECT count(*) FROM queued WHERE session_id='legacy' AND via='resumed:user'"),1);
        Ok(())
    }).unwrap();
}

#[test]
fn inbox_conflicts_gaps_duplicates_and_transactional_apply() {
    let t = fresh();
    let k = StreamKey {
        node: "n",
        stream: "s",
        epoch: "e",
    };
    t.store
        .transaction::<_, StoreError>(|tx| {
            inbox_insert_in(tx, k, 3, "e3", &json!({"n":3}), 1)?;
            assert_eq!(
                watermarks_in(tx, k)?,
                Watermarks {
                    durable: 0,
                    applied: 0
                }
            );
            inbox_insert_in(tx, k, 1, "e1", &json!({"n":1}), 1)?;
            assert_eq!(watermarks_in(tx, k)?.durable, 1);
            assert!(!inbox_insert_in(tx, k, 1, "e1", &json!({"n":1}), 2)?);
            assert!(inbox_insert_in(tx, k, 1, "e1", &json!({"n":99}), 2).is_err());
            assert!(inbox_insert_in(tx, k, 2, "e1", &json!({"n":1}), 2).is_err());
            assert!(mark_applied_in(tx, k, 3).is_err());
            inbox_insert_in(tx, k, 2, "e2", &json!({"n":2}), 2)?;
            assert_eq!(watermarks_in(tx, k)?.durable, 3);
            assert!(mark_applied_in(tx, k, 1)?);
            assert!(!mark_applied_in(tx, k, 1)?);
            Ok(())
        })
        .unwrap();
    let failed: Result<()> = t.store.transaction(|tx| {
        mark_applied_in(tx, k, 2)?;
        Err(StoreError::Conflict("projection failure".into()))
    });
    assert!(failed.is_err());
    t.store
        .transaction::<_, StoreError>(|tx| {
            assert_eq!(
                watermarks_in(tx, k)?,
                Watermarks {
                    durable: 3,
                    applied: 1
                }
            );
            mark_applied_in(tx, k, 2)?;
            mark_applied_in(tx, k, 3)?;
            let other = StreamKey { epoch: "e2", ..k };
            inbox_insert_in(tx, other, 2, "other", &json!({}), 3)?;
            assert_eq!(watermarks_in(tx, other)?.durable, 0);
            Ok(())
        })
        .unwrap();
}

/// Issue: an applied event pruned from the inbox and delivered again was stored again (applied=0),
/// to be applied a second time.
#[test]
fn a_pruned_redelivery_is_not_stored_again() {
    let t = fresh();
    let k = StreamKey {
        node: "n",
        stream: "s",
        epoch: "e",
    };
    t.store
        .transaction::<_, StoreError>(|tx| {
            assert!(inbox_insert_in(tx, k, 1, "e1", &json!({"n":1}), 1)?);
            assert!(inbox_insert_in(tx, k, 2, "e2", &json!({"n":2}), 1)?);
            assert!(mark_applied_in(tx, k, 1)?);
            assert!(mark_applied_in(tx, k, 2)?);
            // as the retention prune does
            tx.execute(
                "DELETE FROM node_event_inbox WHERE node_id='n' AND applied=1 AND seq=1",
                [],
            )?;
            assert!(!inbox_insert_in(tx, k, 1, "e1", &json!({"n":1}), 9)?);
            // its payload can no longer be compared: nothing is stored either way
            assert!(!inbox_insert_in(tx, k, 1, "e1", &json!({"n":99}), 9)?);
            let rows: i64 = tx.query_row(
                "SELECT COUNT(*) FROM node_event_inbox WHERE node_id='n' AND seq=1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(rows, 0);
            assert_eq!(
                watermarks_in(tx, k)?,
                Watermarks {
                    durable: 2,
                    applied: 2
                }
            );
            // the kept row still compares, and the next position is new
            assert!(inbox_insert_in(tx, k, 2, "e2", &json!({"n":99}), 9).is_err());
            assert!(inbox_insert_in(tx, k, 3, "e3", &json!({"n":3}), 9)?);
            assert_eq!(watermarks_in(tx, k)?.durable, 3);
            Ok(())
        })
        .unwrap();
}

/// Issue: a send refused after an uncertain one reset maybe_sent to 0, so a stop settled on the Hub
/// a command whose first send may still reach the Node.
#[test]
fn maybe_sent_stays_once_a_send_was_uncertain() {
    let t = fresh();
    let maybe_sent = |tx: &steve_store::Tx<'_>| -> Result<i64> {
        Ok(tx.query_row(
            "SELECT maybe_sent FROM node_commands WHERE operation_id='op'",
            [],
            |r| r.get(0),
        )?)
    };
    t.store
        .transaction::<_, StoreError>(|tx| {
            command(tx, "op", None)?;
            assert_eq!(maybe_sent(tx)?, 0);
            // refused before acceptance: never sent
            let l = lease_command_in(tx, "w", 1, 10)?.unwrap();
            assert!(!l.maybe_sent);
            assert_eq!(maybe_sent(tx)?, 1, "in flight");
            assert!(retry_command_in(tx, &l, 2, 2, false)?);
            assert_eq!(maybe_sent(tx)?, 0);
            // uncertain, then refused: still maybe sent
            let l = lease_command_in(tx, "w", 3, 10)?.unwrap();
            assert!(retry_command_in(tx, &l, 4, 4, true)?);
            assert_eq!(maybe_sent(tx)?, 1);
            let l = lease_command_in(tx, "w", 5, 10)?.unwrap();
            assert!(l.maybe_sent);
            assert!(retry_command_in(tx, &l, 6, 6, false)?);
            assert_eq!(maybe_sent(tx)?, 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn receipts_never_regress_and_terminal_conflicts_are_visible() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            assert!(command(tx, "op", None)?);
            assert!(!command(tx, "op", None)?);
            assert!(
                insert_command_in(
                    tx,
                    &NewCommand {
                        operation: "op",
                        node: "n",
                        method: "cancel",
                        request: &json!({"a":1}),
                        digest: "digest",
                        target_attempt: None,
                        dependency: None
                    },
                    1
                )
                .is_err()
            );
            assert!(merge_receipt_in(
                tx,
                "op",
                ReceiptState::Active,
                false,
                &json!({"running":true}),
                2
            )?);
            assert!(!merge_receipt_in(
                tx,
                "op",
                ReceiptState::Accepted,
                false,
                &json!({}),
                3
            )?);
            assert!(
                merge_receipt_in(tx, "op", ReceiptState::Rejected, false, &json!({}), 3).is_err()
            );
            assert!(merge_receipt_in(
                tx,
                "op",
                ReceiptState::Unknown,
                false,
                &json!({}),
                3
            )?);
            assert!(!merge_receipt_in(
                tx,
                "op",
                ReceiptState::Active,
                false,
                &json!({}),
                4
            )?);
            assert!(merge_receipt_in(
                tx,
                "op",
                ReceiptState::Finished,
                true,
                &json!({"ok":true}),
                4
            )?);
            assert!(!merge_receipt_in(
                tx,
                "op",
                ReceiptState::Accepted,
                false,
                &json!({}),
                5
            )?);
            assert!(!merge_receipt_in(
                tx,
                "op",
                ReceiptState::Finished,
                true,
                &json!({"ok":true}),
                5
            )?);
            assert!(
                merge_receipt_in(
                    tx,
                    "op",
                    ReceiptState::Finished,
                    false,
                    &json!({"error":true}),
                    5
                )
                .is_err()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn leases_fence_stale_workers_and_wait_for_successful_dependencies() {
    let t = fresh();
    t.store
        .transaction::<_, StoreError>(|tx| {
            command(tx, "open", None)?;
            command(tx, "run", Some("open"))?;
            let a = lease_command_in(tx, "worker", 1, 10)?.unwrap();
            assert_eq!(a.operation, "open");
            assert!(lease_command_in(tx, "other", 2, 11)?.is_none());
            let b = lease_command_in(tx, "worker", 10, 20)?.unwrap();
            assert!(b.generation > a.generation);
            assert!(!retry_command_in(tx, &a, 11, 12, true)?);
            assert!(retry_command_in(tx, &b, 11, 12, true)?);
            assert!(lease_command_in(tx, "x", 11, 20)?.is_none());
            merge_receipt_in(tx, "open", ReceiptState::Finished, true, &json!({}), 12)?;
            assert_eq!(lease_command_in(tx, "x", 12, 22)?.unwrap().operation, "run");
            command(tx, "failed", None)?;
            command(tx, "blocked", Some("failed"))?;
            merge_receipt_in(tx, "failed", ReceiptState::Finished, false, &json!({}), 13)?;
            assert!(lease_command_in(tx, "x", 13, 23)?.is_none());
            Ok(())
        })
        .unwrap();
}

#[test]
fn source_and_statistics_keys_cannot_double_apply() {
    let t = fresh();
    ready(&t);
    t.store.transaction::<_,StoreError>(|tx| {
        claim_in(tx,"s","t",2)?;attempt(tx)?;
        tx.execute("INSERT INTO events(session_id,ts,kind,data,format,source_node,source_stream,source_epoch,source_seq) VALUES ('s',1,'text','{}','steve.v1','n','s','e',1)",[])?;
        assert!(tx.execute("INSERT INTO events(session_id,ts,kind,data,format,source_node,source_stream,source_epoch,source_seq) VALUES ('s',1,'text','{}','steve.v1','n','s','e',1)",[]).is_err());
        for id in ["tn1","tn2"] {
            let r=tx.execute("INSERT INTO agent_turns(id,workspace_id,session_id,harness,started,node_attempt_id,logical_turn_id) VALUES (?,'default','s','test',1,'a','t')",params![id]);
            assert_eq!(r.is_ok(),id=="tn1");
        }
        Ok(())
    }).unwrap();
}

#[test]
fn projection_cas_terminal_and_identity_guards() {
    let t = fresh();
    ready(&t);
    t.store
        .transaction::<_, StoreError>(|tx| {
            claim_in(tx, "s", "t", 2)?;
            attempt(tx)?;
            let active = SessionUpdate {
                phase: "active",
                active_attempt: Some("a"),
                release_pending: false,
            };
            assert!(!session_cas_in(tx, "s", 1, &active, 3)?); // claim already advanced revision
            assert!(session_cas_in(tx, "s", 2, &active, 3)?);
            assert!(
                session_cas_in(
                    tx,
                    "s",
                    3,
                    &SessionUpdate {
                        phase: "ready",
                        active_attempt: None,
                        release_pending: false
                    },
                    3
                )
                .is_err()
            );
            let running = TurnUpdate {
                state: "running",
                active_attempt: Some("a"),
                retry_ordinal: 0,
                applied: false,
            };
            assert!(turn_cas_in(tx, "t", 0, &running, 3)?);
            assert!(!turn_cas_in(tx, "t", 0, &running, 4)?);
            let end = TurnUpdate {
                state: "finished",
                active_attempt: None,
                retry_ordinal: 0,
                applied: true,
            };
            assert!(turn_cas_in(tx, "t", 1, &end, 4).is_err());
            assert!(cancel_attempt_in(tx, "a", 0, 4)?);
            assert!(attempt_cas_in(
                tx,
                "a",
                1,
                "finished",
                None,
                Some(&json!({"cancelled":true})),
                5
            )?);
            assert!(!attempt_cas_in(tx, "a", 2, "running", None, None, 6)?);
            assert!(turn_cas_in(tx, "t", 1, &end, 5)?);
            assert!(!turn_cas_in(tx, "t", 2, &running, 6)?);
            assert!(session_cas_in(
                tx,
                "s",
                3,
                &SessionUpdate {
                    phase: "ready",
                    active_attempt: None,
                    release_pending: false
                },
                6
            )?);
            command(tx, "op", None)?;
            assert_eq!(command_in(tx, "op")?.unwrap().state, "pending");
            let k = StreamKey {
                node: "n",
                stream: "events",
                epoch: "e",
            };
            inbox_insert_in(tx, k, 2, "e2", &json!({"seq":2}), 7)?;
            assert!(next_event_in(tx, k)?.is_none());
            inbox_insert_in(tx, k, 1, "e1", &json!({"seq":1}), 7)?;
            assert_eq!(next_event_in(tx, k)?.unwrap().seq, 1);
            mark_applied_in(tx, k, 1)?;
            assert_eq!(next_event_in(tx, k)?.unwrap().payload, json!({"seq":2}));
            Ok(())
        })
        .unwrap();
}

#[test]
fn permission_rows_replay_cas_due_and_per_attempt() {
    let t = fresh();
    ready(&t);
    t.store
        .transaction::<_, StoreError>(|tx| {
            claim_in(tx, "s", "t", 2)?;
            attempt(tx)?;
            let p =
                |decision: &'static str, permission: &'static str, expires_at: i64| NewPermission {
                    decision,
                    node: "n",
                    session: "s",
                    attempt: "a",
                    permission,
                    request: &serde_json::Value::Null,
                    expires_at,
                };
            assert!(insert_permission_in(tx, &p("d1", "p1", 100), 1)?);
            // the same request of the same attempt again (a replay) is not a second card
            assert!(!insert_permission_in(tx, &p("d1b", "p1", 100), 1)?);
            assert!(insert_permission_in(tx, &p("d2", "p2", 500), 1)?);
            let r = permission_in(tx, "n", "a", "p1")?.unwrap();
            assert_eq!((r.decision.as_str(), r.state.as_str()), ("d1", "waiting"));
            assert!(permission_in(tx, "n", "other", "p1")?.is_none());
            let due: Vec<String> = due_permissions_in(tx, 200, 10)?
                .into_iter()
                .map(|r| r.decision)
                .collect();
            assert_eq!(due, ["d1"]);
            assert!(permission_cas_in(
                tx,
                "d1",
                r.revision,
                "answering",
                None,
                2
            )?);
            // a stale revision changes nothing
            assert!(!permission_cas_in(
                tx, "d1", r.revision, "expired", None, 2
            )?);
            assert!(
                due_permissions_in(tx, 1_000, 10)?
                    .iter()
                    .all(|r| r.decision == "d2")
            );
            assert_eq!(permissions_of_attempt_in(tx, "a")?.len(), 2);
            let r2 = permission_of_decision_in(tx, "d2")?.unwrap();
            assert!(permission_cas_in(
                tx,
                "d2",
                r2.revision,
                "applied",
                None,
                3
            )?);
            let open: Vec<String> = permissions_of_attempt_in(tx, "a")?
                .into_iter()
                .map(|r| r.decision)
                .collect();
            assert_eq!(open, ["d1"]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn desired_config_round_trip_leaves_the_revision_and_configuring_is_a_phase() {
    let t = fresh();
    ready(&t);
    t.store
        .transaction::<_, StoreError>(|tx| {
            assert_eq!(desired_config_in(tx, "s")?, None);
            let rev: i64 = tx.query_row(
                "SELECT revision FROM runtime_sessions WHERE session_id='s'",
                [],
                |r| r.get(0),
            )?;
            set_desired_config_in(tx, "s", Some(&json!({"x": 1})), 5)?;
            assert_eq!(desired_config_in(tx, "s")?, Some(json!({"x": 1})));
            let after: i64 = tx.query_row(
                "SELECT revision FROM runtime_sessions WHERE session_id='s'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(rev, after);
            set_desired_config_in(tx, "s", None, 6)?;
            assert_eq!(desired_config_in(tx, "s")?, None);
            let configuring = SessionUpdate {
                phase: "configuring",
                active_attempt: None,
                release_pending: false,
            };
            assert!(session_cas_in(tx, "s", after, &configuring, 7)?);
            let bad = SessionUpdate {
                phase: "nonsense",
                active_attempt: None,
                release_pending: false,
            };
            assert!(session_cas_in(tx, "s", after + 1, &bad, 7).is_err());
            Ok(())
        })
        .unwrap();
}
