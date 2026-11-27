use super::*;
use steve_node_api::testkit::{FakeNode, build};
use steve_store::testkit::hub::node_checks;
use steve_store::testkit::hub::node_planning as planning_fixture;

use steve_runtime::FakeClock;

/// Counts what the runtime books after commits.
#[derive(Default)]
struct Seen {
    finished: Mutex<Vec<String>>,
    ended: Mutex<Vec<String>>,
    ended_reasons: Mutex<Vec<String>>,
    started: Mutex<Vec<String>>,
    workspaces: Mutex<Vec<WorkspaceFact>>,
}

impl NodeDomain for Seen {
    fn workspace_changed(&self, fact: WorkspaceFact) {
        self.workspaces.lock().unwrap().push(fact);
    }
    fn turn_ended(&self, session: &str, _: Option<&str>, stop_reason: &str, _: Option<String>) {
        self.ended.lock().unwrap().push(session.into());
        self.ended_reasons.lock().unwrap().push(stop_reason.into());
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
        let other = Error::new(Code::Stale, NodeStop::HarnessGone.detail());
        assert_eq!(said(&other), "the harness is gone");
        let changed = NodeReason::workspace_binding_conflict(
            "this resource's workspace was prepared otherwise",
        );
        assert!(workspace_binding_conflict(&changed));
        assert_eq!(
            said(&changed),
            steve_i18n::with(steve_i18n::ZH_CN, || steve_i18n::t!(
                "hub.node.workspace_conflict"
            ))
        );
        assert!(!workspace_binding_conflict(&Error::new(
            Code::Internal,
            changed.safe_detail.clone()
        )));
        let harness = Error::new(Code::Internal, "rate limited, try later");
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

/// An attempt whose harness never listed Steve's tools is said once in its session's timeline,
/// however many such attempts the Node reports.
#[tokio::test]
async fn missing_steve_tools_are_said_once_per_session() {
    let h = super::super::node_tools::tests::fixture(vec![]).await;
    let missing = |seq: u64| NodeEvent {
        schema: 1,
        id: EventId::new(format!("tools-missing-{seq}")).unwrap(),
        at: Timestamp(now()),
        cursor: Watermark {
            stream: StreamId::new("tools-missing").unwrap(),
            epoch: Epoch::new("e1").unwrap(),
            seq,
        },
        subject: Subject::Session {
            session: SessionId::new("s1").unwrap(),
        },
        body: EventBody::Diagnostic {
            severity: Severity::Warning,
            code: STEVE_TOOLS_MISSING.into(),
            safe_text: "never listed".into(),
            details: None,
        },
    };
    let said = || {
        steve_store::testkit::hub::node_fixtures::diagnostic_notice_count(
            &h.store,
            STEVE_TOOLS_MISSING,
        )
        .unwrap()
    };
    for seq in 1..=2 {
        h._rt
            .receive(&EventBatch {
                events: vec![missing(seq)],
            })
            .unwrap();
    }
    until("both applied", || {
        steve_store::testkit::hub::node_fixtures::tools_missing_watermark(&h.store).unwrap() == 2
    })
    .await;
    assert_eq!(said(), 1);
}

/// Auto-mode classifier unavailability diagnostics deduplicate warnings per turn, escalate once
/// when the harness stops auto mode, record structured diagnostic evidence, and allow a fresh
/// warning in a subsequent turn.
#[tokio::test]
async fn auto_mode_classifier_unavailable_warns_escalates_and_records_diagnostics() {
    let h = super::super::node_tools::tests::fixture(vec![]).await;
    let diag = |seq: u64, severity: Severity| NodeEvent {
        schema: 1,
        id: EventId::new(format!("auto-classifier-{seq}")).unwrap(),
        at: Timestamp(now()),
        cursor: Watermark {
            stream: StreamId::new("auto-classifier").unwrap(),
            epoch: Epoch::new("e1").unwrap(),
            seq,
        },
        subject: Subject::Execution(h.exec.clone()),
        body: EventBody::Diagnostic {
            severity,
            code: AUTO_MODE_CLASSIFIER_UNAVAILABLE.into(),
            safe_text: "classifier returned no verdict".into(),
            details: None,
        },
    };
    let said = || {
        steve_store::testkit::hub::node_fixtures::diagnostic_notice_count(
            &h.store,
            AUTO_MODE_CLASSIFIER_UNAVAILABLE,
        )
        .unwrap()
    };

    // Two Warning diagnostics in the same turn produce only one timeline warning.
    h._rt
        .receive(&EventBatch {
            events: vec![diag(1, Severity::Warning), diag(2, Severity::Warning)],
        })
        .unwrap();
    assert_eq!(said(), 1);

    // Escalating to Error (auto mode stopped) records once more in the same turn; a duplicate Error
    // is deduplicated.
    h._rt
        .receive(&EventBatch {
            events: vec![diag(3, Severity::Error), diag(4, Severity::Error)],
        })
        .unwrap();
    assert_eq!(said(), 2);

    // Structured diagnostic records are persisted for item debugging.
    until("classifier diagnostic evidence persisted", || {
        h.store
            .diagnostics()
            .events(
                "default",
                "i1",
                steve_store::diagnostics::Filter {
                    kind: Some("harness.auto_mode_classifier_unavailable".into()),
                    limit: 20,
                    ..Default::default()
                },
            )
            .unwrap()["events"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    })
    .await;

    // After a turn_end boundary, a subsequent turn can emit a fresh warning.
    h.store
        .write(|w| {
            w.events().add(
                "s1",
                "turn_end",
                json!({ "stopReason": "end_turn" }),
                None,
                now(),
            )?;
            Ok::<(), StoreError>(())
        })
        .unwrap();
    h._rt
        .receive(&EventBatch {
            events: vec![diag(5, Severity::Warning)],
        })
        .unwrap();
    assert_eq!(said(), 3);

    // Verify stored system event carries the harness display name (never hardcoded in Hub logic).
    let latest = h
        .store
        .read(|r| steve_store::session_repo::latest_system_text(r, "s1"))
        .unwrap()
        .expect("latest system text");
    assert!(
        latest.contains("自动模式不可用（分类器无判定）"),
        "unexpected stored notice text: {latest}"
    );
}

#[tokio::test]
async fn adapter_exit_evidence_follows_commit_and_replay_without_claiming_the_current_attempt() {
    let h = super::super::node_tools::tests::fixture(vec![]).await;
    until("current attempt running", || {
        steve_store::testkit::hub::node_fixtures::is_running_attempt(
            &h.store,
            h.exec.attempt().as_str(),
        )
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
    let applied =
        || steve_store::testkit::hub::node_fixtures::adapter_exit_watermark(&h.store).unwrap();

    // Fail after the event was applied, at the enclosing transaction's position update.
    steve_store::testkit::hub::mutations::create_refuse_exit_apply(&h.store).unwrap();
    assert!(h._rt.receive(&batch).is_err());
    until("recorder settled after rolled-back apply", pending).await;
    assert_eq!(applied(), 0);
    assert!(
        evidence()["events"].as_array().unwrap().is_empty(),
        "rolled-back facts must not become diagnostic evidence"
    );

    steve_store::testkit::hub::mutations::remove_refuse_exit_apply(&h.store).unwrap();
    let first_ack = h._rt.receive(&batch).unwrap();
    until("committed exit evidence", || {
        evidence()["events"].as_array().unwrap().len() == 1
    })
    .await;
    assert_eq!(applied(), 1);
    assert_eq!(h._rt.receive(&batch).unwrap(), first_ack);
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
async fn item_pause_holds_node_fact_followups_until_explicit_resume() {
    let h = super::super::node_tools::tests::fixture(vec![]).await;
    until("first turn reached Node", || {
        h._node
            .log()
            .iter()
            .any(|(method, _)| *method == "start_turn")
    })
    .await;
    h._rt.prompt("s1", "排队补充", "user", &[]).unwrap();
    steve_store::testkit::hub::mutations::update_items_status_pause_id_i1(&h.store).unwrap();
    h._node
        .finish(
            &h.exec,
            Completion {
                // A transient failure can beat the pause's cancellation command. Neither its
                // automatic retry nor the queued follow-up may start while the item is paused.
                outcome: Outcome::Failed,
                stop_reason: "error".into(),
                error: Some(Error::later(
                    Code::Unavailable,
                    "temporary provider refusal",
                )),
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
    let counts =
        || steve_store::testkit::hub::node_fixtures::attempt_and_queue_counts(&h.store).unwrap();
    assert_eq!(counts(), (1, 1));
    assert_eq!(h._rt.flush("s1", None).unwrap(), Planned::Held);
    steve_store::testkit::hub::mutations::update_items_status_run_id_i1(&h.store).unwrap();
    assert_eq!(h._rt.flush("s1", None).unwrap(), Planned::Created);
    assert_eq!(counts(), (2, 0));
}

/// Review L4: an attempt the Hub settles on its own word is booked only once
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
    steve_store::testkit::hub::node_fixtures::seed_sessions(&store, &S).unwrap();
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
    rt.write(|tx| {
        rt.settle_in(tx, S, &attempt, &completion)?;
        // the upkeep worker drains between the settle and its commit
        rt.tell_later();
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
    let r = rt.write(|tx| {
        rt.settle_in(tx, S, &attempt, &completion)?;
        Err::<(), _>(StoreError::Conflict("rolled back".into()))
    });
    assert!(r.is_err());
    rt.tell_later();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(seen.finished.lock().unwrap().len(), 1);
    assert_eq!(seen.ended.lock().unwrap().len(), 1);
}

/// The role of a delta is found at the same cost however long its message already is: from
/// memory, else through the index on `message` rows (never walking back through the deltas).
#[test]
fn a_deltas_role_costs_the_same_however_long_its_message() {
    const S: &str = "s1";
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    let add = |w: &mut steve_store::W<'_>, kind: &str, data: Value| {
        w.events().add(S, kind, data, None, 1).map(|_| ())
    };
    let steps = |n_before: usize| -> (Option<Role>, i32) {
        store
            .write(|w| {
                for i in 0..n_before {
                    add(
                        w,
                        "text",
                        json!({"role": "thought", "message": "m1", "text": format!("t{i} ")}),
                    )?;
                }
                node_sessions::observations::message_role_steps(w, S, "m1")
            })
            .unwrap()
    };
    store
        .write(|w| {
            steve_store::testkit::hub::node_fixtures::seed_active_session(w, S)?;
            add(w, "message", json!({"message": "m0", "role": "user"}))?;
            add(w, "message", json!({"message": "m1", "role": "thought"}))
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
        .write(|w| node_sessions::observations::message_role_steps(w, S, "m0"))
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
    steve_store::testkit::hub::node_fixtures::seed_sessions(&store, &S).unwrap();
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
    let exec: ExecutionRef = serde_json::from_str(
        &steve_store::testkit::hub::node_checks::attempt_execution(&store).unwrap(),
    )
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
    let roles =
        || -> Vec<String> { steve_store::testkit::hub::node_fixtures::text_roles(&store).unwrap() };
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

#[tokio::test]
async fn device_pause_keeps_existing_attempt_and_retry_then_holds_followup_until_resume() {
    let mut h = super::super::node_tools::tests::fixture(vec![]).await;
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    // Reassemble the coordinator over the same durable running attempt, with a controllable
    // retry clock. Keep the Node and domain used by the shared tool fixture.
    let rt = NodeRuntime::with_clock(
        h.store.clone(),
        h._node.clone(),
        Arc::new(super::super::node_tools::tests::Ends),
        clock.clone(),
    );
    let old = Arc::downgrade(&h._rt);
    h._rt = rt;
    until("previous coordinator dropped", || old.upgrade().is_none()).await;
    h._node.connect(Arc::new(Sink(Arc::downgrade(&h._rt))));
    h._rt.start();
    until("coordinator reconciled", || h._rt.is_reconciled()).await;
    until("first attempt reached Node", || {
        h._node.log().iter().any(|(m, _)| *m == "start_turn")
    })
    .await;
    h.store
        .devices()
        .upsert(&steve_store::DeviceInfo {
            id: "hub".into(),
            name: "test".into(),
            ..Default::default()
        })
        .unwrap();
    h.store
        .devices()
        .patch(
            "hub",
            &steve_store::DevicePatch {
                status: Some("暂停接活".into()),
                max_sessions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        h._rt.active_attempt("s1").unwrap().as_deref(),
        Some(h.exec.attempt().as_str())
    );
    h._rt.prompt("s1", "followup", "user", &[]).unwrap();
    h._node
        .finish(
            &h.exec,
            Completion {
                outcome: Outcome::Failed,
                stop_reason: "error".into(),
                error: Some(Error::later(
                    Code::Unavailable,
                    "temporary provider refusal",
                )),
                result: vec![],
                continuation: None,
                transient: true,
                acted: false,
                job: None,
            },
        )
        .await
        .unwrap();
    until("same turn retry planned despite device pause", || {
        h._rt
            .active_attempt("s1")
            .unwrap()
            .is_some_and(|a| a != h.exec.attempt().as_str())
    })
    .await;
    let retry: ExecutionRef = serde_json::from_str(
        &steve_store::testkit::hub::node_fixtures::retry_execution(&h.store).unwrap(),
    )
    .unwrap();
    let (
        ExecutionRef::Turn {
            turn: retry_turn, ..
        },
        ExecutionRef::Turn {
            turn: first_turn, ..
        },
    ) = (&retry, &h.exec)
    else {
        panic!("expected turn attempts")
    };
    assert_eq!(retry_turn, first_turn);
    let at: i64 = steve_store::testkit::hub::node_fixtures::command_retry_at(
        &h.store,
        retry.attempt().as_str(),
    )
    .unwrap();
    assert_eq!(at, clock.now() + 20_000);
    clock.advance_to(at - 1);
    until("sender observed time before deadline", || {
        clock.observed(at - 1)
    })
    .await;
    assert_eq!(
        h._node
            .log()
            .iter()
            .filter(|(m, _)| *m == "start_turn")
            .count(),
        1
    );
    clock.advance_to(at);
    until("retry reached Node while device paused", || {
        h._node
            .log()
            .iter()
            .filter(|(m, _)| *m == "start_turn")
            .count()
            == 2
    })
    .await;
    h._node
        .finish(
            &retry,
            Completion {
                outcome: Outcome::Succeeded,
                stop_reason: "end_turn".into(),
                error: None,
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            },
        )
        .await
        .unwrap();
    until("retry completed", || {
        h._rt.active_attempt("s1").unwrap().is_none()
    })
    .await;
    assert_eq!(h._rt.flush("s1", None).unwrap(), Planned::Held);
    let (queued, attempts, node, status) =
        steve_store::testkit::hub::node_fixtures::paused_session_state(&h.store).unwrap();
    assert_eq!(queued, 1);
    assert_eq!(attempts, 2);
    assert_eq!(node, "local");
    assert_ne!(status, "error");
    h.store
        .devices()
        .patch(
            "hub",
            &steve_store::DevicePatch {
                status: Some("正常".into()),
                max_sessions: Some(32),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(h._rt.flush("s1", None).unwrap(), Planned::Created);
}

// No workers or sleeps: tests drive each lease/receipt and periodic sweep explicitly.
fn retry_fixture() -> (
    tempfile::TempDir,
    Arc<FakeNode>,
    Arc<NodeRuntime>,
    Arc<Seen>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    steve_store::testkit::hub::mutations::seed_items_for_retry_fixture(&store).unwrap();
    let node = FakeNode::new("n1");
    let seen = Arc::new(Seen::default());
    let rt = NodeRuntime::new(store, node.clone(), seen.clone());
    rt.reconciled.store(true, Ordering::SeqCst);
    rt.bind(&Binding {
        session: "s1".into(),
        resource: "item:i1".into(),
        prepare: serde_json::from_value(json!({"type":"worktree",
            "repository":{"root":"root","rel":"repo"},"branch":"test"}))
        .unwrap(),
        choice: build::choice(),
        policy: build::policy(),
    })
    .unwrap();
    (dir, node, rt, seen)
}

fn retry_count(rt: &NodeRuntime, method: &str) -> i64 {
    node_checks::command_count_by_method(&rt.store, method).unwrap()
}

fn retry_failed(rt: &NodeRuntime, op: String, error: Error) {
    let mut receipt = Receipt {
        operation: idv(op).unwrap(),
        state: Observed::Accepted,
    };
    rt.write(|tx| {
        rt.receipt_in(tx, &receipt)?;
        receipt.state = Observed::Finished {
            outcome: Outcome::Failed,
            result: None,
            error: Some(error),
        };
        rt.receipt_in(tx, &receipt)?;
        Ok(())
    })
    .unwrap();
}

#[tokio::test]
async fn tidy_failure_stays_in_one_revision_until_a_new_prepare() {
    for rejected in [false, true] {
        let (_dir, node, rt, seen) = retry_fixture();
        assert!(rt.send_one().await); // prepare
        assert!(rt.send_one().await); // open
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = attempts.clone();
        node.set_hook(move |point| {
            if matches!(
                point,
                steve_node_api::testkit::Point::BeforeAccept("tidy_workspace")
            ) {
                counted.fetch_add(1, Ordering::Relaxed);
            }
        });
        assert!(rt.tidy_session_workspace("s1", true).unwrap());
        let error = Error::new(Code::Internal, "dirty worktree retained");
        if rejected {
            node.refuse_next("tidy_workspace", error);
            assert!(rt.send_one().await);
        } else {
            let op =
                node_checks::command_operation_id_by_method(&rt.store, "tidy_workspace").unwrap();
            retry_failed(&rt, op, error);
        }
        assert!(seen.workspaces.lock().unwrap().iter().any(|f| matches!(f,
            WorkspaceFact::Tidied { removed: false, note: Some(note), .. } if note == "dirty worktree retained")));
        for _ in 0..17 {
            assert!(rt.tidy_session_workspace("s1", true).unwrap());
            assert!(!rt.send_one().await);
        }
        assert_eq!(attempts.load(Ordering::Relaxed), usize::from(rejected));
        assert_eq!(retry_count(&rt, "tidy_workspace"), 1);
        planning_fixture::clone_prepare(&rt.store, "prepare:s1:r0", "new_prepare").unwrap();
        assert!(rt.tidy_session_workspace("s1", true).unwrap());
        assert_eq!(retry_count(&rt, "tidy_workspace"), 2);
    }
}

#[tokio::test]
async fn open_never_failure_blocks_all_reopen_callers_until_explicit_retry() {
    for rejected in [false, true] {
        let (_dir, node, rt, _seen) = retry_fixture();
        assert!(rt.send_one().await); // prepare
        assert!(rt.send_one().await); // first open succeeds
        assert_eq!(
            rt.prompt("s1", "first", "user", &[]).unwrap(),
            Planned::Created
        );
        node.refuse_next(
            "start_turn",
            steve_types::devices::NodeReason::session_not_open("session not open"),
        );
        assert!(rt.send_one().await); // the refused-start path calls reopen_in
        let failed_op: String =
            node_checks::last_inserted_command_id_by_method(&&rt.store, "open_session").unwrap();
        let error = Error::new(Code::Invalid, "unsupported effort");
        if rejected {
            node.refuse_next("open_session", error);
            assert!(rt.send_one().await);
        } else {
            retry_failed(&rt, failed_op.clone(), error);
        }
        assert_eq!(
            rt.prompt("s1", "queued", "user", &[]).unwrap(),
            Planned::Held
        );
        for _ in 0..3 {
            assert_eq!(rt.flush("s1", None).unwrap(), Planned::Held); // plan.rs released + queued
            rt.write(|tx| rt.reopen_in(tx, "s1")).unwrap(); // refused-start caller's common guard
        }
        assert_eq!(retry_count(&rt, "open_session"), 2);
        rt.retry_open("s1", &build::choice()).unwrap();
        assert_eq!(retry_count(&rt, "open_session"), 3);
        assert!(rt.store.sessions().stop("s1").unwrap().is_none());
    }
}

#[tokio::test]
async fn outcome_unknown_never_stops_leasing_and_can_be_settled_by_reconciliation() {
    let (_dir, node, rt, _seen) = retry_fixture();
    let mut error = Error::new(Code::OutcomeUnknown, "reply lost; do not retry");
    error.retry = Retry::Never;
    node.refuse_next("prepare_workspace", error);
    assert!(rt.send_one().await);
    for _ in 0..3 {
        planning_fixture::make_commands_due(&rt.store).unwrap();
        assert!(!rt.send_one().await);
    }
    let (state, maybe_sent, generation): (String, bool, i64) =
        steve_store::testkit::hub::node_fixtures::fixture_prepare_state(&rt.store).unwrap();
    assert_eq!(
        (state.as_str(), maybe_sent, generation),
        ("unknown", true, 1)
    );
    // The uncertain send may actually have run: put its result into the Node's ledger only.
    let request: String =
        node_checks::command_request_by_operation(&&rt.store, "prepare:s1:r0").unwrap();
    node.prepare_workspace(serde_json::from_str(&request).unwrap())
        .await
        .unwrap();
    rt.reconcile().await.unwrap();
    assert!(rt.send_one().await); // its dependent open can now go
    assert_eq!(retry_count(&rt, "prepare_workspace"), 1);
    assert_eq!(node.log().len(), 2); // one prepare and its dependent open
}

#[tokio::test]
async fn fifth_structured_failure_holds_one_turn_and_sends_one_cancel() {
    let (_dir, node, mut rt, seen) = retry_fixture();
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    assert!(rt.send_one().await);
    assert!(rt.send_one().await);
    rt.prompt("s1", "work", "user", &[]).unwrap();
    assert!(rt.send_one().await);
    let execution: ExecutionRef =
        serde_json::from_str(&node_checks::attempt_execution(&rt.store).unwrap()).unwrap();
    let tool = |n| ToolView {
        id: CallId::new(format!("c{n}")).unwrap(),
        parent: None,
        origin: ToolOrigin::Harness,
        name: "Bash".into(),
        category: ToolCategory::Exec,
        title: "Run a check".into(),
        description: None,
        target: None,
        phase: ToolPhase::Failed,
        action: None,
        evidence: Some(ToolEvidence::Failed {
            signature: Digest::sha256(b"failed test command"),
        }),
        result: vec![],
        error: None,
        duration_ms: None,
    };
    let cards = |rt: &NodeRuntime| {
        node_checks::decision_count_by_subject_type(
            &rt.store,
            steve_types::orchestration::TURN_STALL,
        )
        .unwrap()
    };
    for n in [1, 1, 2, 3, 4, 5, 5, 6] {
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::ToolChanged { tool: tool(n) },
        )
        .await
        .unwrap();
        if n == 4 {
            assert_eq!(retry_count(&rt, "cancel"), 0);
            assert_eq!(cards(&rt), 0);
            rt = NodeRuntime::new(rt.store.clone(), node.clone(), seen.clone());
            node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
            rt.reconcile_again().await.unwrap();
        }
    }
    assert_eq!(cards(&rt), 1);
    assert_eq!(retry_count(&rt, "cancel"), 1);
    assert!(rt.send_one().await);
    assert!(!rt.send_one().await);
    assert_eq!(
        node.log()
            .iter()
            .filter(|(method, _)| *method == "cancel")
            .count(),
        1
    );
    assert_eq!(
        rt.prompt("s1", "follow up", "user", &[]).unwrap(),
        Planned::Held
    );
    node.restart();
    let restarted = NodeRuntime::new(rt.store.clone(), node.clone(), seen);
    node.connect(Arc::new(Sink(Arc::downgrade(&restarted))));
    restarted.reconcile_again().await.unwrap();
    restarted.observe_turns().unwrap();
    assert_eq!(cards(&restarted), 1);
    assert_eq!(retry_count(&restarted, "cancel"), 1);
}

#[tokio::test]
async fn a_turn_holds_within_one_tick_after_observed_progress_stops() {
    let (_dir, node, original, seen) = retry_fixture();
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    let rt = NodeRuntime::with_clock(original.store.clone(), node.clone(), seen, clock.clone());
    rt.reconciled.store(true, Ordering::SeqCst);
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    assert!(rt.send_one().await);
    assert!(rt.send_one().await);
    rt.prompt("s1", "work", "user", &[]).unwrap();
    assert!(rt.send_one().await);
    let execution: ExecutionRef =
        serde_json::from_str(&node_checks::attempt_execution(&rt.store).unwrap()).unwrap();
    let workspace: WorkspaceRef = serde_json::from_str(
        &node_checks::binding_workspace_ref_by_session(&rt.store, "s1").unwrap(),
    )
    .unwrap();
    let cards = || {
        node_checks::decision_count_by_subject_type(
            &rt.store,
            steve_types::orchestration::TURN_STALL,
        )
        .unwrap()
    };
    for i in 0..40 {
        clock.advance(14 * 60_000);
        rt.observe_turns().unwrap();
        assert_eq!(cards(), 0);
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::WorkspaceChanged {
                workspace: workspace.clone(),
                revision: idv(format!("rev-{i}")).unwrap(),
            },
        )
        .await
        .unwrap();
    }

    let mut permission = build::permission("stall-approval", execution.clone(), "echo approved");
    permission.expires_at = Timestamp(clock.now() + 35 * 60_000);
    node.ask_permission(permission).await.unwrap();
    rt.observe_turns().unwrap();
    clock.advance(30 * 60_000);
    rt.observe_turns().unwrap();
    assert_eq!(cards(), 0);
    let decision = node_checks::decision_id_by_status(&rt.store, "open").unwrap();
    assert!(
        rt.answer_permission(steve_core::PermissionAnswer::resolve(
            &decision,
            1,
            "owner",
            "allow_once",
            ""
        ))
        .unwrap()
    );
    assert!(rt.send_one().await);

    rt.reconciled.store(false, Ordering::SeqCst);
    rt.observe_turns().unwrap();
    clock.advance(48 * 60 * 60_000);
    rt.observe_turns().unwrap();
    assert_eq!(cards(), 0);
    rt.reconciled.store(true, Ordering::SeqCst);
    rt.observe_turns().unwrap();
    let deadline = clock.now() + 15 * 60_000;
    clock.advance_to(deadline - 100);
    rt.upkeep(false);
    assert_eq!(cards(), 0);
    clock.advance_to(deadline + 900);
    rt.upkeep(false);
    assert_eq!(cards(), 1);
    assert_eq!(retry_count(&rt, "cancel"), 1);
    assert_eq!(rt.flush("s1", None).unwrap(), Planned::Held);
}

#[tokio::test]
async fn seventeen_permanent_refusals_stay_single_across_48_hours_of_wakes() {
    use steve_node_api::testkit::Point;
    let (_dir, node, original, seen) = retry_fixture();
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    let store = original.store.clone();
    let mut rt = NodeRuntime::with_clock(store.clone(), node.clone(), seen.clone(), clock.clone());
    rt.reconcile_again().await.unwrap();
    let sends = Arc::new(Mutex::new(Vec::new()));
    let calls = sends.clone();
    node.set_hook(move |point| {
        if let Point::BeforeAccept("open_session" | "tidy_workspace") = point {
            calls.lock().unwrap().push(point);
        }
    });
    node.refuse(
        "open_session",
        Some(Error::new(Code::Invalid, "permanent open refusal")),
    );
    node.refuse(
        "tidy_workspace",
        Some(Error::new(Code::Conflict, "dirty worktree")),
    );
    let sessions: Vec<_> = (1..=17).map(|id| format!("s{id}")).collect();
    for (index, session) in sessions.iter().enumerate().skip(1) {
        let item = format!("i{}", index + 1);
        steve_store::testkit::hub::workspace_cases::seed_item_and_active_session(
            &store, &item, session,
        )
        .unwrap();
        rt.bind(&Binding {
            session: session.clone(),
            resource: format!("item:{item}"),
            prepare: serde_json::from_value(
                json!({"type":"worktree", "repository":{"root":"root","rel":"repo"},"branch":item}),
            )
            .unwrap(),
            choice: build::choice(),
            policy: build::policy(),
        })
        .unwrap();
    }
    for _ in 0..34 {
        assert!(rt.send_one().await);
    }
    for session in &sessions {
        assert!(rt.tidy_session_workspace(session, true).unwrap());
    }
    for _ in 0..17 {
        assert!(rt.send_one().await);
    }
    for n in 0..86_400 {
        clock.advance(2_000);
        rt.wake_send.notify_one();
        if n == 43_199 {
            node.restart();
            rt = NodeRuntime::with_clock(store.clone(), node.clone(), seen.clone(), clock.clone());
            rt.reconcile_again().await.unwrap();
        }
        assert!(!rt.send_one().await);
        // Sweep worktrees on the housekeeping cadence as well.
        if n % 60 == 0 || n == 86_399 {
            rt.plan_waiting();
            for session in &sessions {
                assert!(rt.tidy_session_workspace(session, true).unwrap());
            }
        }
    }
    for method in ["open_session", "tidy_workspace"] {
        assert_eq!(retry_count(&rt, method), 17);
        assert_eq!(
            sends
                .lock()
                .unwrap()
                .iter()
                .filter(|p| **p == Point::BeforeAccept(method))
                .count(),
            17
        );
    }
    assert_eq!(retry_count(&rt, "start_turn"), 0);
    assert_eq!(retry_count(&rt, "start_job"), 0);
}

#[tokio::test]
async fn one_turn_shares_four_starts_and_sixteen_sends_across_failures() {
    use steve_node_api::testkit::Point;
    let (_dir, node, original, seen) = retry_fixture();
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    let rt = NodeRuntime::with_clock(original.store.clone(), node.clone(), seen, clock.clone());
    rt.reconciled.store(true, Ordering::SeqCst);
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    assert!(rt.send_one().await);
    assert!(rt.send_one().await);
    rt.prompt("s1", "one logical turn", "user", &[]).unwrap();
    let sends = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = sends.clone();
    node.set_hook(move |point| {
        if point == Point::BeforeAccept("start_turn") {
            counted.fetch_add(1, Ordering::Relaxed);
        }
    });
    for next_delay in [20_000, 60_000, 120_000, 0] {
        for _ in 0..3 {
            node.refuse_next("start_turn", Error::later(Code::Busy, "before admission"));
            assert!(rt.send_one().await);
            clock.advance(10_000);
        }
        assert!(rt.send_one().await);
        let attempts =
            steve_store::testkit::hub::node_fixtures::ordered_attempt_executions(&rt.store)
                .unwrap();
        let execution: ExecutionRef = serde_json::from_str(attempts.last().unwrap()).unwrap();
        let mut completion = build::done();
        completion.outcome = Outcome::Failed;
        completion.stop_reason = "error".into();
        completion.error = Some(Error::later(Code::Unavailable, "known retryable failure"));
        node.finish(&execution, completion).await.unwrap();
        clock.advance(next_delay);
    }
    assert!(!rt.send_one().await);
    assert_eq!(retry_count(&rt, "start_turn"), 4);
    assert_eq!(sends.load(Ordering::Relaxed), 16);
    assert_eq!(
        node.log()
            .iter()
            .filter(|(method, _)| *method == "start_turn")
            .count(),
        4
    );
}

#[tokio::test]
async fn open_session_carries_system_context_and_preserves_it_across_reopen() {
    let (_dir, node, original, seen) = retry_fixture();
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    let store = original.store.clone();
    let rt = NodeRuntime::with_clock(store.clone(), node.clone(), seen, clock);
    rt.reconciled.store(true, Ordering::SeqCst);
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));

    steve_store::testkit::hub::workspace_cases::seed_item_and_active_session(
        &store, "ST-CTX", "s-ctx",
    )
    .unwrap();
    rt.bind_after_with_context(
        &Binding {
            session: "s-ctx".into(),
            resource: "item:ST-CTX".into(),
            prepare: serde_json::from_value(
                json!({"type":"existing", "dir":{"root":"root","rel":"proj"}}),
            )
            .unwrap(),
            choice: build::choice(),
            policy: build::policy(),
        },
        None,
        Some("[K1] Owner rule: keep changes focused."),
    )
    .unwrap();
    while rt.send_one().await {}

    let first_open = rt.open_of("s-ctx").unwrap();
    assert_eq!(
        first_open.system_context.as_deref(),
        Some("[K1] Owner rule: keep changes focused.")
    );
    assert!(
        node.log()
            .iter()
            .any(|(method, _)| *method == "open_session")
    );

    // Release and reopen (new prompt): persisted system_context stays intact on the reopen command.
    rt.release("s-ctx").unwrap();
    while rt.send_one().await {}
    rt.prompt("s-ctx", "follow up", "user", &[]).unwrap();
    while rt.send_one().await {}
    let reopen_open = rt.open_of("s-ctx").unwrap();
    assert_eq!(
        reopen_open.system_context.as_deref(),
        Some("[K1] Owner rule: keep changes focused.")
    );
}

#[tokio::test]
async fn upgrade_only_node_skips_reconcile_until_normal_link() {
    use steve_node_api::upgrade_v1;
    use steve_node_wire::{Conn, ConnOptions, Handler, NodeServer, RemoteNode};

    struct CountCalls(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait]
    impl Handler for CountCalls {
        async fn handle(&self, _method: &str, _params: Value) -> ApiResult<Value> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        }
    }

    let (_dir, fake, original, seen) = retry_fixture();
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    let remote = RemoteNode::new(NodeId::new("n_upgrade").unwrap());

    let (up_hub, up_peer) = steve_node_wire::testkit::pair();
    let upgrade_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let up_peer_conn = Conn::spawn(
        up_peer.sink,
        up_peer.stream,
        1,
        Arc::new(CountCalls(upgrade_calls.clone())),
        ConnOptions {
            heartbeat: Duration::from_secs(600),
            dead_after: Duration::from_secs(600),
            side: "n",
            mode: upgrade_v1::Mode::Upgrade,
        },
    );
    let up_hub_conn = Conn::spawn(
        up_hub.sink,
        up_hub.stream,
        1,
        Arc::new(CountCalls(Arc::new(std::sync::atomic::AtomicUsize::new(0)))),
        ConnOptions {
            heartbeat: Duration::from_secs(600),
            dead_after: Duration::from_secs(600),
            side: "h",
            mode: upgrade_v1::Mode::Upgrade,
        },
    );
    assert!(remote.attach(up_hub_conn.clone(), false));
    assert!(remote.is_upgrade_only());

    let rt = NodeRuntime::with_clock(original.store.clone(), remote.clone(), seen, clock.clone());
    rt.start();

    // Advance past several 5s reconcile ticks while the link is upgrade-only.
    for _ in 0..4 {
        tokio::task::yield_now().await;
        clock.advance(5_000);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !rt.is_reconciled(),
        "upgrade-only node must stay unreconciled"
    );
    assert_eq!(
        upgrade_calls.load(Ordering::SeqCst),
        0,
        "upgrade-only node must not invoke ordinary snapshot/reconcile RPCs"
    );

    // Once the Node upgrades and reconnects normally, reconciliation completes on the next tick.
    let (norm_hub, norm_peer) = steve_node_wire::testkit::pair();
    let norm_peer_conn = Conn::spawn(
        norm_peer.sink,
        norm_peer.stream,
        2,
        NodeServer::new(fake),
        ConnOptions {
            heartbeat: Duration::from_secs(600),
            dead_after: Duration::from_secs(600),
            side: "n",
            mode: upgrade_v1::Mode::Normal,
        },
    );
    let norm_hub_conn = Conn::spawn(
        norm_hub.sink,
        norm_hub.stream,
        2,
        Arc::new(CountCalls(Arc::new(std::sync::atomic::AtomicUsize::new(0)))),
        ConnOptions {
            heartbeat: Duration::from_secs(600),
            dead_after: Duration::from_secs(600),
            side: "h",
            mode: upgrade_v1::Mode::Normal,
        },
    );
    assert!(remote.attach(norm_hub_conn.clone(), false));
    assert!(!remote.is_upgrade_only());

    clock.advance(5_000);
    until("reconciled after switching to normal link", || {
        rt.is_reconciled()
    })
    .await;

    up_hub_conn.close(None, steve_node_wire::close::GOODBYE, "test done");
    up_peer_conn.close(None, steve_node_wire::close::GOODBYE, "test done");
    norm_hub_conn.close(None, steve_node_wire::close::GOODBYE, "test done");
    norm_peer_conn.close(None, steve_node_wire::close::GOODBYE, "test done");
}

#[tokio::test]
async fn notify_linked_wakes_initial_reconcile_without_waiting_for_five_second_tick() {
    use steve_node_api::upgrade_v1;
    use steve_node_wire::{Conn, ConnOptions, Handler, NodeServer, RemoteNode};

    struct Noop;
    #[async_trait]
    impl Handler for Noop {
        async fn handle(&self, _method: &str, _params: Value) -> ApiResult<Value> {
            Ok(json!({}))
        }
    }

    let (_dir, fake, original, seen) = retry_fixture();
    let t0 = steve_store::now();
    let clock = Arc::new(FakeClock::new(t0));
    let remote = RemoteNode::new(NodeId::new("n_boot").unwrap());
    let rt = NodeRuntime::with_clock(original.store.clone(), remote.clone(), seen, clock.clone());
    let mut ledger = rt.subscribe_ledger();
    rt.start();

    // Let the initial reconcile attempt fail with `Unavailable` and park on the 5s retry sleep.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert!(
        !rt.is_reconciled(),
        "unlinked node must remain unreconciled before link attach"
    );

    let (hub_end, peer_end) = steve_node_wire::testkit::pair();
    let peer_conn = Conn::spawn(
        peer_end.sink,
        peer_end.stream,
        1,
        NodeServer::new(fake),
        ConnOptions {
            heartbeat: Duration::from_secs(600),
            dead_after: Duration::from_secs(600),
            side: "n",
            mode: upgrade_v1::Mode::Normal,
        },
    );
    let hub_conn = Conn::spawn(
        hub_end.sink,
        hub_end.stream,
        1,
        Arc::new(Noop),
        ConnOptions {
            heartbeat: Duration::from_secs(600),
            dead_after: Duration::from_secs(600),
            side: "h",
            mode: upgrade_v1::Mode::Normal,
        },
    );
    assert!(remote.attach(hub_conn.clone(), false));
    rt.notify_linked();

    // Never advance `FakeClock`: reconciliation must wake immediately from `notify_linked` and
    // publish a ledger revision when `reconciled` flips to true.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !rt.is_reconciled() {
            ledger.changed().await.expect("ledger channel open");
        }
    })
    .await
    .expect("notify_linked must wake initial reconcile without waiting for the 5s timer");
    assert_eq!(
        clock.now(),
        t0,
        "FakeClock must not have advanced during event-driven reconciliation"
    );

    hub_conn.close(None, steve_node_wire::close::GOODBYE, "test done");
    peer_conn.close(None, steve_node_wire::close::GOODBYE, "test done");
}

async fn start_reconciled_turn() -> (
    tempfile::TempDir,
    Arc<FakeNode>,
    Arc<NodeRuntime>,
    Arc<Seen>,
    Arc<FakeClock>,
    ExecutionRef,
) {
    let (dir, node, original, seen) = retry_fixture();
    let clock = Arc::new(FakeClock::new(steve_store::now()));
    let rt = NodeRuntime::with_clock(
        original.store.clone(),
        node.clone(),
        seen.clone(),
        clock.clone(),
    );
    rt.reconciled.store(true, Ordering::SeqCst);
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    assert!(rt.send_one().await);
    assert!(rt.send_one().await);
    rt.prompt("s1", "long task", "user", &[]).unwrap();
    assert!(rt.send_one().await);
    let execution: ExecutionRef =
        serde_json::from_str(&node_checks::attempt_execution(&rt.store).unwrap()).unwrap();
    (dir, node, rt, seen, clock, execution)
}

fn stall_cards(rt: &NodeRuntime) -> i64 {
    node_checks::decision_count_by_subject_type(&rt.store, steve_types::orchestration::TURN_STALL)
        .unwrap()
}

#[tokio::test]
async fn agy_background_task_survives_past_fifteen_minutes() {
    let (_dir, node, rt, _seen, clock, execution) = start_reconciled_turn().await;
    let mut norm =
        steve_acp::normalize::Normalizer::new("/tmp/repo", "a1", Epoch::new("m1").unwrap())
            .for_harness("antigravity")
            .root_session("agy-1");

    for frame in [
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-bg-1",
            "name": "run_command",
            "kind": "execute",
            "status": "in_progress",
            "title": "cargo test --workspace",
            "rawInput": {
                "CommandLine": "cargo test --workspace",
                "WaitMsBeforeAsync": 500,
                "toolSummary": "Workspace test run"
            }
        }),
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-bg-1",
            "name": "run_command",
            "kind": "execute",
            "status": "completed",
            "title": "cargo test --workspace",
            "rawInput": {
                "CommandLine": "cargo test --workspace",
                "WaitMsBeforeAsync": 500,
                "toolSummary": "Workspace test run"
            },
            "rawOutput": {
                "output": "Tool is running as a background task with task id: agy-1/task-7"
            }
        }),
    ] {
        for fact in norm.update_for_session(Some("agy-1"), &frame) {
            if let steve_acp::normalize::Fact::Event(body) = fact {
                node.emit(Subject::Execution(execution.clone()), body)
                    .await
                    .unwrap();
            }
        }
    }

    for _ in 0..20 {
        clock.advance(60_000);
        rt.observe_turns().unwrap();
    }
    assert_eq!(
        stall_cards(&rt),
        0,
        "active agy background task must not be killed at 15 minutes"
    );
    assert_eq!(retry_count(&rt, "cancel"), 0);
}

#[tokio::test]
async fn agy_async_subagent_survives_past_fifteen_minutes() {
    let (_dir, node, rt, _seen, clock, execution) = start_reconciled_turn().await;
    let mut norm =
        steve_acp::normalize::Normalizer::new("/tmp/repo", "a1", Epoch::new("m1").unwrap())
            .for_harness("antigravity")
            .root_session("agy-1");

    for frame in [
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-sub-1",
            "name": "invoke_subagent",
            "kind": "other",
            "status": "in_progress",
            "title": "Run invoke_subagent",
            "rawInput": {
                "TypeName": "research",
                "Role": "Codebase Researcher",
                "Prompt": "Inspect all crates and report findings."
            }
        }),
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-sub-1",
            "name": "invoke_subagent",
            "kind": "other",
            "status": "completed",
            "title": "Run invoke_subagent",
            "rawInput": {
                "TypeName": "research",
                "Role": "Codebase Researcher",
                "Prompt": "Inspect all crates and report findings."
            }
        }),
    ] {
        for fact in norm.update_for_session(Some("agy-1"), &frame) {
            if let steve_acp::normalize::Fact::Event(body) = fact {
                node.emit(Subject::Execution(execution.clone()), body)
                    .await
                    .unwrap();
            }
        }
    }

    for _ in 0..20 {
        clock.advance(60_000);
        rt.observe_turns().unwrap();
    }
    assert_eq!(
        stall_cards(&rt),
        0,
        "running agy subagent must not be killed at 15 minutes"
    );
    assert_eq!(retry_count(&rt, "cancel"), 0);
}

#[tokio::test]
async fn long_running_compile_tool_survives_past_fifteen_minutes() {
    let (_dir, node, rt, _seen, clock, execution) = start_reconciled_turn().await;
    node.emit(
        Subject::Execution(execution.clone()),
        EventBody::ToolChanged {
            tool: ToolView {
                id: CallId::new("call-build-1").unwrap(),
                parent: None,
                origin: ToolOrigin::Harness,
                name: "run_command".into(),
                category: ToolCategory::Exec,
                title: "cargo build --release".into(),
                description: None,
                target: Some("cargo build --release".into()),
                phase: ToolPhase::Running,
                action: Some(Action::Exec {
                    command: "cargo build --release".into(),
                    cwd: None,
                    network: NetworkScope::Unrestricted,
                }),
                evidence: None,
                result: vec![],
                error: None,
                duration_ms: None,
            },
        },
    )
    .await
    .unwrap();

    for _ in 0..20 {
        clock.advance(60_000);
        rt.observe_turns().unwrap();
    }
    assert_eq!(
        stall_cards(&rt),
        0,
        "long-running compile command must not be killed at 15 minutes"
    );
    assert_eq!(retry_count(&rt, "cancel"), 0);
}

#[tokio::test]
async fn pure_steve_mcp_sequence_survives_past_fifteen_minutes() {
    let (_dir, node, rt, _seen, clock, execution) = start_reconciled_turn().await;
    for step in 0..5 {
        clock.advance(4 * 60_000);
        rt.observe_turns().unwrap();
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::ToolChanged {
                tool: ToolView {
                    id: CallId::new(format!("call-mcp-{step}")).unwrap(),
                    parent: None,
                    origin: ToolOrigin::Steve,
                    name: "steve_item_read".into(),
                    category: ToolCategory::Steve,
                    title: format!("Read ST-{step}"),
                    description: None,
                    target: None,
                    phase: ToolPhase::Succeeded,
                    action: Some(Action::Tool {
                        name: "steve_item_read".into(),
                        args: json!({ "id": format!("ST-{step}") }),
                    }),
                    evidence: None,
                    result: vec![Part::Text {
                        text: format!("item ST-{step} details"),
                    }],
                    error: None,
                    duration_ms: Some(120),
                },
            },
        )
        .await
        .unwrap();
    }
    clock.advance(4 * 60_000);
    rt.observe_turns().unwrap();
    assert_eq!(
        stall_cards(&rt),
        0,
        "distinct Steve MCP calls across 24 minutes must not be killed at 15 minutes"
    );
    assert_eq!(retry_count(&rt, "cancel"), 0);
}

#[tokio::test]
async fn plan_and_read_search_advance_progress_while_spin_loop_still_stalls_at_fifteen_minutes() {
    let (_dir, node, rt, _seen, clock, execution) = start_reconciled_turn().await;

    // Distinct Plan updates and Read/Search calls across 20 minutes keep the turn alive.
    for step in 0..5 {
        clock.advance(4 * 60_000);
        rt.observe_turns().unwrap();
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::Plan {
                entries: vec![PlanEntry {
                    content: format!("Inspect module {step}"),
                    status: if step > 0 {
                        "completed".into()
                    } else {
                        "in_progress".into()
                    },
                }],
            },
        )
        .await
        .unwrap();
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::ToolChanged {
                tool: ToolView {
                    id: CallId::new(format!("call-read-{step}")).unwrap(),
                    parent: None,
                    origin: ToolOrigin::Harness,
                    name: "read_file".into(),
                    category: ToolCategory::Read,
                    title: format!("Read src/m{step}.rs"),
                    description: None,
                    target: Some(format!("src/m{step}.rs")),
                    phase: ToolPhase::Succeeded,
                    action: Some(Action::Read {
                        paths: vec![PathRef {
                            workspace: WorkspaceRef {
                                node: NodeId::new("local").unwrap(),
                                id: WorkspaceId::new("w1").unwrap(),
                            },
                            rel: RelPath::new(format!("src/m{step}.rs")).unwrap(),
                        }],
                    }),
                    evidence: None,
                    result: vec![Part::Text {
                        text: format!("contents of module {step}"),
                    }],
                    error: None,
                    duration_ms: Some(40),
                },
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(stall_cards(&rt), 0);

    // Repeating the exact same Read + Plan without any workspace mutation is deduplicated and
    // caught at 15 minutes.
    for min in 1..=15 {
        clock.advance(60_000);
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::Plan {
                entries: vec![PlanEntry {
                    content: "Inspect module 4".into(),
                    status: "completed".into(),
                }],
            },
        )
        .await
        .unwrap();
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::ToolChanged {
                tool: ToolView {
                    id: CallId::new(format!("call-spin-{min}")).unwrap(),
                    parent: None,
                    origin: ToolOrigin::Harness,
                    name: "read_file".into(),
                    category: ToolCategory::Read,
                    title: "Read src/m4.rs".into(),
                    description: None,
                    target: Some("src/m4.rs".into()),
                    phase: ToolPhase::Succeeded,
                    action: Some(Action::Read {
                        paths: vec![PathRef {
                            workspace: WorkspaceRef {
                                node: NodeId::new("local").unwrap(),
                                id: WorkspaceId::new("w1").unwrap(),
                            },
                            rel: RelPath::new("src/m4.rs").unwrap(),
                        }],
                    }),
                    evidence: None,
                    result: vec![Part::Text {
                        text: "contents of module 4".into(),
                    }],
                    error: None,
                    duration_ms: Some(40),
                },
            },
        )
        .await
        .unwrap();
        rt.observe_turns().unwrap();
    }
    assert_eq!(
        stall_cards(&rt),
        1,
        "spin loop must still stall at 15 minutes"
    );
    assert_eq!(retry_count(&rt, "cancel"), 1);
}

#[tokio::test]
async fn hung_active_subagent_stalls_at_extended_limit_with_real_blocker_and_typed_stop_reason() {
    let (_dir, node, rt, seen, clock, execution) = start_reconciled_turn().await;
    let mut norm =
        steve_acp::normalize::Normalizer::new("/tmp/repo", "a1", Epoch::new("m1").unwrap())
            .for_harness("antigravity")
            .root_session("agy-1");

    for frame in [
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-sub-hung",
            "name": "invoke_subagent",
            "kind": "other",
            "status": "in_progress",
            "title": "Run invoke_subagent",
            "rawInput": {
                "TypeName": "research",
                "Role": "Codebase Researcher",
                "Prompt": "Deep audit"
            }
        }),
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-sub-hung",
            "name": "invoke_subagent",
            "kind": "other",
            "status": "completed",
            "title": "Run invoke_subagent",
            "rawInput": {
                "TypeName": "research",
                "Role": "Codebase Researcher",
                "Prompt": "Deep audit"
            },
            "rawOutput": {
                "conversationId": "sub-conv-99"
            }
        }),
    ] {
        for fact in norm.update_for_session(Some("agy-1"), &frame) {
            if let steve_acp::normalize::Fact::Event(body) = fact {
                node.emit(Subject::Execution(execution.clone()), body)
                    .await
                    .unwrap();
            }
        }
    }

    // Active subagent survives 59 minutes, then stalls at the 60-minute active-work ceiling.
    for _ in 0..59 {
        clock.advance(60_000);
        rt.observe_turns().unwrap();
    }
    assert_eq!(stall_cards(&rt), 0);

    clock.advance(60_000);
    rt.observe_turns().unwrap();
    assert_eq!(stall_cards(&rt), 1);
    assert_eq!(retry_count(&rt, "cancel"), 1);

    let decisions = rt.store.decisions().open(steve_store::WS).unwrap();
    let stall_card = decisions
        .iter()
        .find(|d| d.subject_type == steve_types::orchestration::TURN_STALL)
        .expect("turn_stall decision card");
    assert_eq!(
        stall_card.question_template,
        Some(steve_types::DecisionPrompt::TurnNoProgress {
            minutes: Some(60),
            blocker: Some(steve_types::StallBlocker {
                kind: steve_types::StallBlockerKind::Subagent,
                label: "Codebase Researcher".into(),
            }),
        })
    );

    // When the Node confirms cancellation with generic `"cancelled"`, Hub rewrites it to the
    // strongly-typed stall stop reason across turn_end and NodeDomain::turn_ended.
    node.emit(
        Subject::Execution(execution.clone()),
        EventBody::AttemptEnded {
            completion: Completion {
                outcome: Outcome::Cancelled,
                stop_reason: "cancelled".into(),
                error: None,
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            },
        },
    )
    .await
    .unwrap();
    rt.tell_later();

    assert_eq!(
        seen.ended_reasons.lock().unwrap().as_slice(),
        &[steve_types::orchestration::STOP_REASON_STALL_NO_PROGRESS.to_owned()]
    );
    let events = rt.store.events().after("s1", 0, 100).unwrap();
    let turn_end = events
        .iter()
        .rev()
        .find(|e| e.kind == "turn_end")
        .expect("turn_end event");
    assert_eq!(
        turn_end.data["stopReason"],
        steve_types::orchestration::STOP_REASON_STALL_NO_PROGRESS
    );
}

#[tokio::test]
async fn repeated_failure_cancel_emits_typed_stall_stop_reason() {
    let (_dir, node, rt, seen, _clock, execution) = start_reconciled_turn().await;
    for step in 0..5 {
        node.emit(
            Subject::Execution(execution.clone()),
            EventBody::ToolChanged {
                tool: ToolView {
                    id: CallId::new(format!("call-fail-{step}")).unwrap(),
                    parent: None,
                    origin: ToolOrigin::Steve,
                    name: "steve_item_read".into(),
                    category: ToolCategory::Steve,
                    title: "Read ST-999".into(),
                    description: None,
                    target: None,
                    phase: ToolPhase::Failed,
                    action: Some(Action::Tool {
                        name: "steve_item_read".into(),
                        args: json!({ "id": "ST-999" }),
                    }),
                    evidence: None,
                    result: vec![],
                    error: Some("not found".into()),
                    duration_ms: Some(10),
                },
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(stall_cards(&rt), 1);
    assert_eq!(retry_count(&rt, "cancel"), 1);

    node.emit(
        Subject::Execution(execution.clone()),
        EventBody::AttemptEnded {
            completion: Completion {
                outcome: Outcome::Cancelled,
                stop_reason: "cancelled".into(),
                error: None,
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            },
        },
    )
    .await
    .unwrap();
    rt.tell_later();

    assert_eq!(
        seen.ended_reasons.lock().unwrap().as_slice(),
        &[steve_types::orchestration::STOP_REASON_STALL_REPEATED_FAILURE.to_owned()]
    );
}
