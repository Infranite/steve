use super::*;
use steve_node_api::testkit::{FakeNode, build};

/// Counts what the runtime books after commits.
#[derive(Default)]
struct Seen {
    finished: Mutex<Vec<String>>,
    ended: Mutex<Vec<String>>,
    started: Mutex<Vec<String>>,
}

impl NodeDomain for Seen {
    fn turn_ended(&self, session: &str, _: Option<&str>, _: &str, _: Option<String>) {
        self.ended.lock().unwrap().push(session.into());
    }
    fn attempt_started(&self, _: &str, attempt: &str, _: i64, _: Option<i64>, _: i64) {
        self.started.lock().unwrap().push(attempt.into());
    }
    fn attempt_finished(&self, _: &str, attempt: &str, _: &Completion, _: &AttemptFacts, _: i64) {
        self.finished.lock().unwrap().push(attempt.into());
    }
}

/// A cause the Node decided on is told in our words (zh-CN even inside an en request: it is
/// stored); a harness's own words pass as they came.
#[tokio::test]
async fn a_nodes_stop_reason_is_said_in_chinese() {
    steve_i18n::scope(steve_i18n::EN, async {
        assert_eq!(
            said(&NodeStop::Deadline.error()),
            "运行超过了时限，Node 已停止它"
        );
        assert_eq!(
            said(&NodeStop::OutputLimit.error()),
            "输出超过了这次运行的上限，Node 已停止它"
        );
        assert_eq!(
            said(&NodeStop::IngressStalled { secs: 90 }.error()),
            "Node 连续 90 秒没能接收 harness 的输出（它的本地运行记录或磁盘卡住了），已中断这次运行"
        );
        // the very words, but another code: not ours to rephrase
        let other = ApiError::new(ErrorCode::Stale, NodeStop::HarnessGone.detail());
        assert_eq!(said(&other), "the harness is gone");
        let harness = ApiError::new(ErrorCode::Internal, "rate limited, try later");
        assert_eq!(said(&harness), "rate limited, try later");
    })
    .await;
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn adapter_exit_evidence_follows_commit_and_replay_without_claiming_the_current_attempt() {
    let h = super::super::node_tools::tests::fixture(vec![]).await;
    until("current attempt running", || {
        h.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT state='running' FROM runtime_attempts WHERE attempt_id=?",
                    [h.exec.attempt().as_str()],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap()
    })
    .await;
    let current = h._rt.active_attempt("s1").unwrap();
    assert_eq!(current.as_deref(), Some(h.exec.attempt().as_str()));
    let source_at = now() - 60_000;
    let batch = EventBatch {
        events: vec![NodeEvent {
            schema: 1,
            id: EventId::new("exit-from-retired-adapter").unwrap(),
            at: Timestamp(source_at),
            cursor: Watermark {
                stream: StreamId::new("exit-evidence").unwrap(),
                epoch: Epoch::new("old-node-epoch").unwrap(),
                seq: 1,
            },
            // A delayed process exit describes its session, not whichever attempt runs now.
            subject: Subject::Session {
                session: SessionId::new("s1").unwrap(),
            },
            body: EventBody::Diagnostic {
                severity: Severity::Error,
                code: "adapter_exited".into(),
                safe_text: "Adapter process exited.".into(),
                details: Some(DiagnosticDetails {
                    exit_code: Some(0),
                    signal: None,
                    generation: Some(7),
                    parse_errors: 3,
                    reader_failed: true,
                    drain_timed_out: true,
                    wait_error: true,
                }),
            },
        }],
    };
    let evidence = || {
        h.store
            .diagnostics()
            .events(
                "default",
                "i1",
                steve_store::diagnostics::Filter {
                    kind: Some("adapter.exited".into()),
                    limit: 20,
                    ..Default::default()
                },
            )
            .unwrap()
    };
    let pending = || {
        h.store.diagnostics().explain("default", "i1").unwrap()["coverage"]["pending_count"] == 0
    };
    let applied = || {
        h.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
        "SELECT applied FROM node_event_inbox WHERE event_id='exit-from-retired-adapter'",
        [], |r| r.get::<_, i64>(0),
    )?)
            })
            .unwrap()
    };

    // Fail after the inbox row was marked, at the enclosing transaction's watermark update.
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER refuse_exit_apply BEFORE UPDATE OF applied ON node_streams
            WHEN NEW.stream='exit-evidence' AND NEW.applied>OLD.applied
            BEGIN SELECT RAISE(ABORT, 'test apply rollback'); END;",
            )?;
            Ok(())
        })
        .unwrap();
    let first_ack = h._rt.receive(&batch).unwrap();
    assert_eq!(h._rt.receive(&batch).unwrap(), first_ack);
    h._rt.apply_some();
    until("recorder settled after rolled-back apply", pending).await;
    assert_eq!(applied(), 0);
    assert!(
        evidence()["events"].as_array().unwrap().is_empty(),
        "rolled-back facts must not become diagnostic evidence"
    );

    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute_batch("DROP TRIGGER refuse_exit_apply")?;
            Ok(())
        })
        .unwrap();
    h._rt.apply_some();
    until("committed exit evidence", || {
        evidence()["events"].as_array().unwrap().len() == 1
    })
    .await;
    assert_eq!(applied(), 1);
    assert_eq!(h._rt.receive(&batch).unwrap(), first_ack);
    h._rt.apply_some();
    until("recorder settled after replay", pending).await;
    let page = evidence();
    let events = page["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        1,
        "a resent Node envelope must not count twice"
    );
    let event = &events[0];
    assert_eq!(event["session_id"], "s1");
    assert_eq!(event["cause_id"], "exit-from-retired-adapter");
    assert!(event["attempt_id"].is_null() && event["operation_id"].is_null());
    assert_eq!(event["severity"], "error");
    assert!(event["at"].as_i64().unwrap() >= source_at + 60_000);
    let details = &event["details"];
    assert_eq!(details["source_at"], source_at);
    assert_eq!(details["source_node"], "local");
    assert_eq!(details["source_epoch"], "old-node-epoch");
    assert_eq!(details["generation"], 7);
    assert_eq!(details["exit_code"], 0);
    assert_eq!(details["parse_errors"], 3);
    assert_eq!(details["reader_failed"], true);
    assert_eq!(details["wait_error"], true);
    assert_eq!(details["drain_timed_out"], true);
    assert_eq!(h._rt.active_attempt("s1").unwrap(), current);
}

/// A Node fact starts follow-ups inside the coordinator, without the public runtime router.
/// The durable pause must hold that path too, while preserving its queued input for resume.
#[tokio::test]
async fn workflow_pause_holds_node_fact_followups_until_explicit_resume() {
    let h = super::super::node_tools::tests::fixture(vec![]).await;
    until("first turn reached Node", || {
        h._node
            .log()
            .iter()
            .any(|(method, _)| *method == "start_turn")
    })
    .await;
    h.store.transaction::<_, StoreError>(|tx| {
        tx.execute_batch(
            "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
             VALUES ('parent','default',2,'主事项','run','执行',1,1);
             INSERT INTO item_goals(item_id,version,text,created)
             VALUES ('parent',1,'验证暂停和恢复',1);
             INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
             VALUES ('parent',1,2,3,1,1);
             INSERT INTO workflow_units(workflow_id,id,position,spec,status,item_id,session_id)
             VALUES ('parent','work',0,'{\"id\":\"work\",\"title\":\"工作\",\"goal\":\"测试\",\"bot_id\":\"dev\"}','running','i1','s1');",
        )?;
        Ok(())
    }).unwrap();
    h.store.bots().seed("dev", false, &steve_store::NewBot { name: "dev".into(), harness: "fixture".into(), ..Default::default() }, "default").unwrap();
    h.store.with_conn(|c| Ok(c.execute_batch("UPDATE items SET bot_id='dev' WHERE id='i1'; UPDATE sessions SET bot_id='dev' WHERE id='s1';")?)).unwrap();
    h._rt.prompt("s1", "排队补充", "user", &[]).unwrap();
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE workflows SET paused=1 WHERE item_id='parent'", [])?;
            Ok(())
        })
        .unwrap();
    h._node
        .finish(
            &h.exec,
            Completion {
                // A transient failure can beat the pause's cancellation command. Neither its
                // automatic retry nor the queued follow-up may start while the graph is paused.
                outcome: Outcome::Failed,
                stop_reason: "error".into(),
                error: None,
                result: vec![],
                continuation: None,
                transient: true,
                acted: false,
                job: None,
            },
        )
        .await
        .unwrap();
    until("paused attempt ended", || {
        h._rt.active_attempt("s1").unwrap().is_none()
    })
    .await;
    let counts = || {
        h.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT (SELECT COUNT(*) FROM runtime_attempts),
                        (SELECT COUNT(*) FROM queued WHERE session_id='s1')",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )?)
            })
            .unwrap()
    };
    assert_eq!(counts(), (1, 1));
    assert_eq!(h._rt.flush("s1", None).unwrap(), Planned::Held);
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE workflows SET paused=0 WHERE item_id='parent'", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(h._rt.flush("s1", None).unwrap(), Planned::Created);
    assert_eq!(counts(), (2, 0));
}

/// Review L4: an attempt the Hub settles on its own word reaches the apply worker only once
/// its transaction committed. The worker draining its queue while that transaction is still
/// open (it runs on another thread) used to drop the settle for good, its statistics row
/// left open forever.
#[tokio::test]
async fn a_settle_drained_before_its_commit_still_ends_its_row() {
    const S: &str = "s1";
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(|_| {}),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                [S],
            )?;
            Ok(())
        })
        .unwrap();
    let node = FakeNode::new("n1");
    let seen = Arc::new(Seen::default());
    let rt = NodeRuntime::new(store.clone(), node.clone(), seen.clone());
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    rt.bind(&Binding {
        session: S.into(),
        resource: format!("item:{S}"),
        prepare: PrepareKind::Scratch,
        choice: build::choice(),
        policy: build::policy(),
    })
    .unwrap();
    rt.prompt(S, "first", "user", &[]).unwrap();
    until("started", || !seen.started.lock().unwrap().is_empty()).await;
    let attempt = seen.started.lock().unwrap()[0].clone();

    let completion = Completion {
        outcome: Outcome::Interrupted,
        stop_reason: "error".into(),
        error: None,
        result: vec![],
        continuation: None,
        transient: false,
        acted: false,
        job: None,
    };
    rt.tx(|tx| {
        rt.settle_in(tx, S, &attempt, &completion)?;
        // the apply worker drains between the settle and its commit
        rt.book_settled();
        assert!(
            seen.finished.lock().unwrap().is_empty(),
            "booked before the commit"
        );
        Ok(())
    })
    .unwrap();
    until("row ended", || !seen.finished.lock().unwrap().is_empty()).await;
    until("turn told", || !seen.ended.lock().unwrap().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *seen.finished.lock().unwrap(),
        std::slice::from_ref(&attempt),
        "once"
    );
    assert_eq!(*seen.ended.lock().unwrap(), [S.to_string()], "once");

    // a settle whose transaction rolls back is never booked
    let r = rt.tx(|tx| {
        rt.settle_in(tx, S, &attempt, &completion)?;
        Err::<(), _>(StoreError::Conflict("rolled back".into()))
    });
    assert!(r.is_err());
    rt.book_settled();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(seen.finished.lock().unwrap().len(), 1);
    assert_eq!(seen.ended.lock().unwrap().len(), 1);
}

/// A receiver commits the next batch immediately after the apply worker observed an empty
/// stream. That new batch must stay dirty even if it is the last thing the Node ever sends.
#[test]
fn a_batch_received_after_an_empty_observation_is_not_forgotten() {
    const S: &str = "s1";
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                [S],
            )?;
            Ok(())
        })
        .unwrap();
    let rt = NodeRuntime::new(
        store.clone(),
        FakeNode::new("n1"),
        Arc::new(Seen::default()),
    );
    // No background workers: the two passes and the receiving interleaving are deterministic.
    let batch = |seq, body| EventBatch {
        events: vec![NodeEvent {
            schema: 1,
            id: idv(format!("event_{seq}")).unwrap(),
            at: Timestamp(seq as i64),
            cursor: Watermark {
                stream: idv("stream1").unwrap(),
                epoch: idv("epoch1").unwrap(),
                seq,
            },
            subject: Subject::Session {
                session: idv(S).unwrap(),
            },
            body,
        }],
    };
    let head = || {
        store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT durable,applied FROM node_streams WHERE node_id='n1' AND stream='stream1' AND epoch='epoch1'",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )?)
            })
            .unwrap()
    };
    rt.receive(&batch(
        1,
        EventBody::MessageStarted {
            message: "m1".into(),
            block: "b1".into(),
            role: Role::Assistant,
        },
    ))
    .unwrap();
    let mut inserted = false;
    assert!(rt.apply_some_observed(|| {
        assert!(!inserted, "only the first pass uses the observer");
        inserted = true;
        assert_eq!(head(), (1, 1));
        // Before the fix, this insert was erased by the outer Ok(None) branch's dirty.remove.
        rt.receive(&batch(
            2,
            EventBody::TextDelta {
                message: "m1".into(),
                block: "b1".into(),
                utf8_offset: 0,
                text: "perm:selected:once".into(),
            },
        ))
        .unwrap();
        // A newer reconcile can register this head before the old empty observation's
        // caught-up callback runs. That callback must not release the new execution hold.
        rt.lagging
            .lock()
            .unwrap()
            .insert(S.into(), ("stream1".into(), "epoch1".into(), 2));
    }));
    assert!(inserted);
    assert_eq!(
        head(),
        (2, 1),
        "received durably but not projected in the old pass"
    );
    assert!(
        rt.dirty
            .lock()
            .unwrap()
            .contains(&("stream1".into(), "epoch1".into()))
    );
    assert!(
        rt.is_uncertain(S),
        "the old empty observation must not release a hold on the new head"
    );
    assert!(
        rt.apply_some(),
        "the next pass needs no third event, retry or restart"
    );
    assert_eq!(head(), (2, 2));
    assert!(!rt.is_uncertain(S), "the new head was actually applied");
    let projected: i64 = store
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT count(*) FROM events WHERE session_id=? AND format='steve.v1' AND kind='text' AND instr(data,'perm:selected:once')>0",
                [S],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(projected, 1);
}

/// The role of a delta is found at the same cost however long its message already is: from
/// memory, else through the index on `message` rows (never walking back through the deltas).
#[test]
fn a_deltas_role_costs_the_same_however_long_its_message() {
    const S: &str = "s1";
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    let add = |tx: &Tx, kind: &str, data: Value| {
        rt::add_event_in(tx, S, kind, data, V1, None, 1).map(|_| ())
    };
    let steps = |n_before: usize| -> (Option<Role>, i32) {
        store
            .transaction::<_, StoreError>(|tx| {
                for i in 0..n_before {
                    add(
                        tx,
                        "text",
                        json!({"role": "thought", "message": "m1", "text": format!("t{i} ")}),
                    )?;
                }
                role_lookup_in(tx, S, "m1")
            })
            .unwrap()
    };
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                [S],
            )?;
            add(tx, "message", json!({"message": "m0", "role": "user"}))?;
            add(tx, "message", json!({"message": "m1", "role": "thought"}))
        })
        .unwrap();
    let (role, short) = steps(10);
    assert_eq!(role, Some(Role::Thought));
    let (role, long) = steps(5000);
    assert_eq!(role, Some(Role::Thought));
    assert_eq!(
        long, short,
        "VM steps after 5010 deltas ({long}) and after 10 ({short})"
    );
    // another session's or another message's rows do not count either
    let (role, _) = store
        .transaction::<_, StoreError>(|tx| role_lookup_in(tx, S, "m0"))
        .unwrap();
    assert_eq!(role, Some(Role::User));
}

/// A delta's role comes from memory while the coordinator remembers its message, from the
/// store once it does not (a restart): either way the one its message began with.
#[tokio::test]
async fn a_delta_keeps_its_messages_role_with_or_without_memory() {
    const S: &str = "s1";
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                [S],
            )?;
            Ok(())
        })
        .unwrap();
    let node = FakeNode::new("n1");
    let seen = Arc::new(Seen::default());
    let rt = NodeRuntime::new(store.clone(), node.clone(), seen.clone());
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    rt.bind(&Binding {
        session: S.into(),
        resource: format!("item:{S}"),
        prepare: PrepareKind::Scratch,
        choice: build::choice(),
        policy: build::policy(),
    })
    .unwrap();
    rt.prompt(S, "first", "user", &[]).unwrap();
    until("started", || !seen.started.lock().unwrap().is_empty()).await;
    let exec: ExecutionRef = store
        .reader(|c| {
            Ok::<_, StoreError>(
                c.query_row("SELECT execution FROM runtime_attempts", [], |r| {
                    r.get::<_, String>(0)
                })?,
            )
        })
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap();
    let subject = Subject::Execution(exec);
    node.emit(
        subject.clone(),
        EventBody::MessageStarted {
            message: "m1".into(),
            block: "b1".into(),
            role: Role::Thought,
        },
    )
    .await
    .unwrap();
    let delta = |at: u64| EventBody::TextDelta {
        message: "m1".into(),
        block: "b1".into(),
        utf8_offset: at,
        text: "x".into(),
    };
    node.emit(subject.clone(), delta(0)).await.unwrap();
    let roles = || -> Vec<String> {
        store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT json_extract(data,'$.role') FROM events WHERE kind='text' ORDER BY seq",
                )?;
                let v = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap()
    };
    until("first delta", || roles().len() == 1).await;
    assert!(
        rt.roles
            .lock()
            .unwrap()
            .contains_key(&(S.to_string(), "m1".to_string()))
    );
    // forgotten (as after a restart): found again in the store
    rt.roles.lock().unwrap().clear();
    node.emit(subject.clone(), delta(1)).await.unwrap();
    until("second delta", || roles().len() == 2).await;
    assert_eq!(roles(), ["thought", "thought"]);
}

#[test]
fn exhausted_delays_double_from_two_seconds_up_to_a_minute() {
    let d: Vec<i64> = (1..=8).map(super::exhausted_delay).collect();
    assert_eq!(
        d,
        [2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000, 60_000]
    );
}
