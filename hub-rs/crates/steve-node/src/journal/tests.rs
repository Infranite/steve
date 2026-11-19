use super::*;
use serde_json::json;

fn subject() -> Subject {
    Subject::Session {
        session: SessionId::new("session").unwrap(),
    }
}
fn delta(offset: u64, text: &str) -> (Subject, EventBody) {
    (
        subject(),
        EventBody::TextDelta {
            message: "m".into(),
            block: "b".into(),
            utf8_offset: offset,
            text: text.into(),
        },
    )
}
fn plan(text: &str) -> (Subject, EventBody) {
    (
        subject(),
        EventBody::Plan {
            entries: vec![PlanEntry {
                content: text.into(),
                status: "pending".into(),
            }],
        },
    )
}
fn events(j: &Journal) -> Vec<NodeEvent> {
    j.read(|t| t.unacked_past(&HashMap::new(), 1024)).unwrap()
}
fn journal() -> (tempfile::TempDir, Journal) {
    let dir = tempfile::tempdir().unwrap();
    let (j, _) = Journal::open(&dir.path().join("journal.db")).unwrap();
    (dir, j)
}

#[test]
fn one_window_publishes_only_after_commit_and_polling_does_not_flush() {
    let (_dir, j) = journal();
    let before = j.metrics.commits.load(Ordering::Relaxed);
    j.call(|w| {
        w.enqueue(prepare(vec![delta(0, "one"), delta(3, " two")])?)?;
        assert!(w.read(|t| t.heads())?.is_empty());
        assert_eq!(w.metrics.commits.load(Ordering::Relaxed), before);
        Ok(())
    })
    .unwrap();
    // The writer's real timer, with no subsequent event or explicit flush.
    let until = Instant::now() + Duration::from_secs(3);
    while events(&j).is_empty() {
        assert!(Instant::now() < until, "window never committed");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(j.metrics.commits.load(Ordering::Relaxed), before + 1);
    let out = events(&j);
    assert_eq!(out.len(), 1);
    assert!(matches!(&out[0].body, EventBody::TextDelta { text, .. } if text == "one two"));
}

#[test]
fn immediate_events_take_buffered_text_without_waiting_for_timer() {
    let (_dir, j) = journal();
    let immediate = [
        json!({"type":"attempt_started"}),
        json!({"type":"attempt_ended","completion":{"outcome":"succeeded","stop_reason":"end_turn"}}),
        json!({"type":"session_changed","view":{"id":"session","revision":"r1","phase":"ready","effective":{"harness":"test"},"effective_permissions":"ask"}}),
        json!({"type":"permission_requested","request":{"id":"p","execution":{"type":"job","job":"j","attempt":"a"},"invocation":"tool","title":"permission","action":{"type":"unknown","summary":"test"},"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","choices":["reject"],"expires_at":1000}}),
        json!({"type":"permission_resolved","resolution":{"id":"p","execution":{"type":"job","job":"j","attempt":"a"},"action_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","decision_revision":"r1","decision":"reject"}}),
        json!({"type":"operation_changed", "receipt":{"operation":"op", "state":{"state":"accepted"}}}),
        json!({"type":"permission_expired", "id":"p"}),
        json!({"type":"message_completed", "message":"m"}),
    ];
    for body in immediate {
        let body: EventBody = serde_json::from_value(body).unwrap();
        j.call(|w| {
            w.enqueue(prepare(vec![delta(0, "before")])?)?;
            let before = w.read(|t| t.unacked_past(&HashMap::new(), 1024))?.len();
            w.enqueue(prepare(vec![(subject(), body.clone())])?)?;
            let out = w.read(|t| t.unacked_past(&HashMap::new(), 1024))?;
            assert_eq!(out.len(), before + 2);
            assert_eq!(out.last().unwrap().body, body);
            assert!(w.pending.deadline().is_none());
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn coalescing_keeps_survivor_order_and_causal_boundaries() {
    let (_dir, j) = journal();
    j.events(
        vec![plan("old"), delta(0, "a"), plan("new"), delta(1, "b")],
        None,
    )
    .unwrap();
    let boundary = EventBody::MessageCompleted {
        message: "m".into(),
    };
    j.events(vec![(subject(), boundary.clone())], None).unwrap();
    let out = events(&j);
    assert_eq!(
        out.iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
        vec![delta(0, "a").1, plan("new").1, delta(1, "b").1, boundary]
    );
    // A new window cannot merge text backwards through a causal boundary.
    j.events(vec![delta(2, "c")], None).unwrap();
    j.flush().unwrap();
    assert_eq!(events(&j).last().unwrap().body, delta(2, "c").1);
}

#[test]
fn joined_text_preserves_utf8_offsets_json_escaping_and_the_size_cap() {
    let (_dir, j) = journal();
    let parts = ["中", "文🦀", "\"\\\n\r\t\0\u{8}\u{c}\u{1f}", "é"];
    let mut offset = 0;
    let mut input = vec![];
    for text in parts {
        input.push(delta(offset, text));
        offset += text.len() as u64;
    }
    j.events(input, None).unwrap();
    j.flush().unwrap();
    let out = events(&j);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].body, delta(0, &parts.concat()).1);
    let big = "x".repeat(16 * 1024);
    j.events(
        vec![delta(offset, &big), delta(offset + big.len() as u64, "z")],
        None,
    )
    .unwrap();
    j.flush().unwrap();
    assert_eq!(events(&j).len(), 3);
    // Non-contiguous and different-block deltas stay separate.
    j.events(vec![delta(100, "a"), delta(102, "b")], None)
        .unwrap();
    j.flush().unwrap();
    assert_eq!(events(&j).len(), 5);
}

#[test]
fn event_and_byte_thresholds_commit_even_when_coalescing_shrinks_the_buffer() {
    let (_dir, j) = journal();
    j.call(|w| {
        w.enqueue(prepare((0..63).map(|_| plan("small")).collect())?)?;
        assert!(w.heads.is_empty());
        w.enqueue(prepare(vec![plan("small")])?)?;
        assert_eq!(w.heads.values().next().unwrap().produced, 1);
        let large = "x".repeat(140 * 1024);
        w.enqueue(prepare(vec![plan(&large)])?)?;
        assert_eq!(w.heads.values().next().unwrap().produced, 1);
        w.enqueue(prepare(vec![plan(&large)])?)?;
        assert_eq!(w.heads.values().next().unwrap().produced, 2);
        Ok(())
    })
    .unwrap();
}

#[test]
fn writer_failure_is_explicit_and_never_publishes_uncommitted_heads_or_counters() {
    let (_dir, j) = journal();
    let e = j
        .call(|w| {
            w.enqueue(prepare(vec![delta(0, "pending")])?)?;
            w.db.pragma_update(None, "query_only", true)
                .map_err(internal)?;
            w.flush()
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Internal);
    assert!(j.failed());
    assert!(events(&j).is_empty());
    assert!(j.read(|t| t.heads()).unwrap().is_empty());
    j.call(|w| {
        w.db.pragma_update(None, "query_only", false)
            .map_err(internal)
    })
    .unwrap();
    j.flush().unwrap();
    let out = events(&j);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cursor.seq, 1);
    assert!(out[0].id.as_str().ends_with("_1"));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        j.tx::<()>(|_| panic!("caller panic fixture")).unwrap();
    }));
    assert!(panic.is_err());
    j.tx(|_| Ok(())).unwrap();
}

#[test]
fn exhausted_buffer_still_allows_a_terminal_receipt() {
    let (_dir, j) = journal();
    j.call(|w| {
        w.storage.quota = w.used + 16384;
        w.refresh()
    })
    .unwrap();
    assert_eq!(
        j.events(vec![delta(0, "over budget")], None)
            .unwrap_err()
            .code,
        ErrorCode::Exhausted
    );
    assert!(!j.over_limit());
    j.events(
        vec![(
            subject(),
            EventBody::MessageCompleted {
                message: "m".into(),
            },
        )],
        None,
    )
    .unwrap();
    assert_eq!(events(&j).len(), 1);
}

#[test]
fn cached_limit_check_does_not_wait_for_the_connection_owner() {
    let (_dir, j) = journal();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            j.call(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        // The worker cannot execute a read/PRAGMA/statvfs here. This must still finish.
        assert!(!j.over_limit());
        assert!(!j.failed());
        release_tx.send(()).unwrap();
    });
}

fn tool(id: &str, title: &str, phase: &str) -> (Subject, EventBody) {
    (
        subject(),
        serde_json::from_value(json!({"type":"tool_changed","tool":{
            "id":id,"origin":"harness","name":"tool","category":"other","title":title,"phase":phase
        }}))
        .unwrap(),
    )
}
fn usage(n: u64, scope: UsageScope, measurement: Measurement) -> (Subject, EventBody) {
    (
        subject(),
        EventBody::Usage {
            report: UsageReport {
                report_id: format!("r{n}"),
                model: None,
                meter_epoch: Epoch::new("meter").unwrap(),
                scope,
                measurement,
                tokens: Tokens {
                    input: Some(n),
                    ..Tokens::default()
                },
                context_used: None,
                context_limit: None,
                cost: None,
            },
        },
    )
}

#[test]
fn snapshots_replace_at_their_latest_position_without_crossing_scope_or_final_boundaries() {
    let (_dir, j) = journal();
    let input = vec![
        tool("a", "old", "pending"),
        tool("b", "other", "running"),
        tool("a", "new", "running"),
        usage(1, UsageScope::Main, Measurement::Cumulative),
        usage(2, UsageScope::All, Measurement::Cumulative),
        usage(3, UsageScope::Main, Measurement::Cumulative),
        usage(4, UsageScope::All, Measurement::Final),
    ];
    j.events(input, None).unwrap();
    assert_eq!(
        events(&j).into_iter().map(|e| e.body).collect::<Vec<_>>(),
        vec![
            tool("b", "other", "running").1,
            tool("a", "new", "running").1,
            usage(2, UsageScope::All, Measurement::Cumulative).1,
            usage(3, UsageScope::Main, Measurement::Cumulative).1,
            usage(4, UsageScope::All, Measurement::Final).1
        ]
    );
    j.events(vec![tool("a", "done", "succeeded")], None)
        .unwrap();
    assert_eq!(events(&j).len(), 6);
    // Independent additive reports must not silently disappear when a provider emits deltas.
    j.events(
        vec![
            usage(5, UsageScope::All, Measurement::Delta),
            usage(6, UsageScope::All, Measurement::Delta),
        ],
        None,
    )
    .unwrap();
    j.flush().unwrap();
    assert_eq!(events(&j).len(), 8);
}

#[test]
fn a_rejected_state_transaction_keeps_previously_accepted_stream_data() {
    let (_dir, j) = journal();
    j.call(|w| {
        w.enqueue(prepare(vec![delta(0, "retained")])?)?;
        let e = w
            .write::<()>(|_| Err(NodeStop::JournalBudget.error()))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Exhausted);
        assert!(w.heads.is_empty());
        assert_eq!(w.pending.events.len(), 1);
        w.flush()
    })
    .unwrap();
    assert_eq!(events(&j)[0].body, delta(0, "retained").1);
    assert!(events(&j)[0].id.as_str().ends_with("_1"));
}

#[test]
fn state_changes_take_pending_data_in_one_full_synchronous_transaction() {
    let (_dir, j) = journal();
    j.call(|w| {
        assert_eq!(
            w.db.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            w.db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            VERSION
        );
        let before = w.metrics.commits.load(Ordering::Relaxed);
        w.enqueue(prepare(vec![delta(0, "pending")])?)?;
        w.write(|t| {
            assert_eq!(t.heads()?[0].produced, 1);
            t.bump("state_fixture")
        })?;
        assert_eq!(w.metrics.commits.load(Ordering::Relaxed), before + 1);
        assert!(w.pending.deadline().is_none());
        Ok(())
    })
    .unwrap();
    assert_eq!(events(&j).len(), 1);
}

#[test]
fn different_sessions_share_the_same_transaction_and_numbering_cache() {
    let (_dir, j) = journal();
    j.call(|w| {
        let before = w.metrics.commits.load(Ordering::Relaxed);
        for i in 0..64 {
            let mut event = plan(&format!("{i}"));
            event.0 = Subject::Session {
                session: SessionId::new(format!("session_{}", i % 2)).unwrap(),
            };
            w.enqueue(prepare(vec![event])?)?;
        }
        assert_eq!(w.metrics.commits.load(Ordering::Relaxed), before + 1);
        assert_eq!(w.heads.len(), 2);
        for head in w.heads.values() {
            assert_eq!(head.produced, 1);
        }
        assert_eq!(*w.counters.get("event").unwrap(), 2);
        Ok(())
    })
    .unwrap();
}

#[test]
fn clean_drop_flushes_the_tail_before_reopening_the_same_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.db");
    let (j, _) = Journal::open(&path).unwrap();
    let id = j.read(|t| t.journal_id()).unwrap();
    j.events(vec![delta(0, "tail")], None).unwrap();
    drop(j);
    let (j, _) = Journal::open(&path).unwrap();
    assert_eq!(j.read(|t| t.journal_id()).unwrap(), id);
    let retained = events(&j);
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].body, delta(0, "tail").1);
}

#[test]
fn causal_boundaries_in_one_update_prevent_replacement_across_the_boundary() {
    let (_dir, j) = journal();
    let boundary = (
        subject(),
        EventBody::MessageCompleted {
            message: "m".into(),
        },
    );
    let input = vec![
        plan("before"),
        boundary.clone(),
        plan("after"),
        tool("t", "done", "succeeded"),
        plan("later"),
    ];
    let expected = input
        .iter()
        .map(|(_, body)| body.clone())
        .collect::<Vec<_>>();
    j.events(input, None).unwrap();
    assert_eq!(
        events(&j).into_iter().map(|e| e.body).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn delivery_ack_and_pruning_do_not_pull_new_streaming_data_out_of_its_window() {
    let (_dir, j) = journal();
    j.events(
        vec![(
            subject(),
            EventBody::MessageCompleted {
                message: "previous".into(),
            },
        )],
        None,
    )
    .unwrap();
    let mark = events(&j)[0].cursor.clone();
    j.call(|w| {
        w.enqueue(prepare(vec![delta(0, "next window")])?)?;
        let ack = EventAck {
            durable: vec![mark],
        };
        assert!(w.tx(|t| t.ack(&ack))?);
        w.tx(|t| t.prune(crate::now()))?;
        assert_eq!(w.pending.events.len(), 1);
        assert_eq!(w.heads.values().next().unwrap().produced, 1);
        assert_eq!(w.heads.values().next().unwrap().acknowledged, 1);
        w.flush()
    })
    .unwrap();
    let out = events(&j);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].cursor.seq, 2);
    assert_eq!(out[0].body, delta(0, "next window").1);
}

#[test]
fn rejected_commands_cannot_reset_the_window_or_erase_its_input_count() {
    let (_dir, j) = journal();
    j.call(|w| {
        w.enqueue(prepare((0..63).map(|_| plan("snapshot")).collect())?)?;
        let deadline = w.pending.deadline();
        assert_eq!(
            w.write::<()>(|_| Err(ApiError::new(ErrorCode::Busy, "admission fixture")))
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        assert_eq!(w.pending.deadline(), deadline);
        assert!(w.heads.is_empty());
        w.enqueue(prepare(vec![plan("last")])?)?;
        assert!(w.pending.events.is_empty());
        assert_eq!(w.heads.values().next().unwrap().produced, 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(events(&j)[0].body, plan("last").1);
}

#[test]
fn a_local_refusal_does_not_change_other_attempts_or_the_window() {
    let (_dir, j) = journal();
    let epoch = j.budget_epoch();
    j.call(|w| {
        w.enqueue(prepare(vec![delta(0, "accepted")])?)?;
        let deadline = w.pending.deadline();
        w.storage.quota = w.used + 16384 + w.pending.bytes() + 1;
        w.refresh()?;
        assert_eq!(
            w.enqueue(prepare(vec![delta(8, "refused")])?)
                .unwrap_err()
                .code,
            ErrorCode::Exhausted
        );
        assert_eq!(w.pending.deadline(), deadline);
        assert_eq!(w.pending.events.len(), 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(j.budget_epoch(), epoch);
    assert!(!j.over_limit());
    assert!(!j.failed());
    let success: Completion =
        serde_json::from_value(json!({"outcome":"succeeded","stop_reason":"end_turn"})).unwrap();
    assert_eq!(j.recorded_completion(epoch, success.clone()), success);
    j.flush().unwrap();
    assert_eq!(events(&j)[0].body, delta(0, "accepted").1);
}

#[test]
fn cumulative_context_and_token_observations_keep_independent_latest_values() {
    let (_dir, j) = journal();
    let mut context = usage(2, UsageScope::Main, Measurement::Cumulative);
    if let EventBody::Usage { report } = &mut context.1 {
        report.tokens = Tokens::default();
        report.context_used = Some(20);
        report.context_limit = Some(100);
    }
    let mut latest_context = context.clone();
    if let EventBody::Usage { report } = &mut latest_context.1 {
        report.report_id = "context_latest".into();
        report.context_used = Some(30);
    }
    j.events(
        vec![
            usage(1, UsageScope::Main, Measurement::Cumulative),
            context,
            latest_context.clone(),
            usage(3, UsageScope::Main, Measurement::Cumulative),
        ],
        None,
    )
    .unwrap();
    j.flush().unwrap();
    assert_eq!(
        events(&j).into_iter().map(|e| e.body).collect::<Vec<_>>(),
        vec![
            latest_context.1,
            usage(3, UsageScope::Main, Measurement::Cumulative).1
        ]
    );
}

#[test]
fn failed_final_usage_stays_with_the_caller_instead_of_duplicating_the_terminal_retry() {
    let (_dir, j) = journal();
    let final_usage = usage(1, UsageScope::Main, Measurement::Final);
    j.call(|w| {
        w.db.pragma_update(None, "query_only", true)
            .map_err(internal)
    })
    .unwrap();
    assert_eq!(
        j.events(vec![final_usage.clone()], None).unwrap_err().code,
        ErrorCode::Internal
    );
    // A known transaction failure has replied; it is not a dead connection owner. The caller
    // retains Final and can retry it with its operation's terminal state once storage recovers.
    assert!(!j.failed());
    j.call(|w| {
        assert!(w.pending.events.is_empty());
        w.db.pragma_update(None, "query_only", false)
            .map_err(internal)
    })
    .unwrap();
    let prepared = prepare(vec![final_usage.clone()]).unwrap();
    j.tx(|t| t.emit(&prepared[0], 1)).unwrap();
    let out = events(&j);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].body, final_usage.1);
}

#[test]
fn accepted_data_refused_after_space_shrinks_cannot_block_an_immediate_control_write() {
    let (_dir, j) = journal();
    let epoch = j.budget_epoch();
    j.call(|w| {
        w.enqueue(prepare(vec![delta(
            0,
            "accepted before the filesystem shrank",
        )])?)?;
        w.storage.quota = w.used;
        w.refresh()?;
        let before = w.metrics.commits.load(Ordering::Relaxed);
        let terminal = prepare(vec![(
            subject(),
            EventBody::MessageCompleted {
                message: "m".into(),
            },
        )])?;
        // This first control call must succeed, without waiting for another retry/window.
        w.enqueue(terminal)?;
        assert!(w.pending.events.is_empty());
        assert_eq!(w.metrics.commits.load(Ordering::Relaxed), before + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(j.budget_epoch(), epoch + 1);
    j.flush().unwrap();
    assert_eq!(j.budget_epoch(), epoch + 1);
    j.call(|w| {
        w.storage.quota = u64::MAX;
        w.refresh()
    })
    .unwrap();
    let success: Completion =
        serde_json::from_value(json!({"outcome":"succeeded","stop_reason":"end_turn"})).unwrap();
    assert_eq!(
        j.recorded_completion(epoch, success.clone()).outcome,
        Outcome::Cancelled
    );
    assert_eq!(
        j.recorded_completion(j.budget_epoch(), success.clone()),
        success
    );
    let out = events(&j);
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0].body, EventBody::MessageCompleted { .. }));
}

#[test]
fn caller_panic_rolls_back_and_retains_the_shared_window() {
    let (_dir, j) = journal();
    j.events(vec![delta(0, "retained")], None).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        j.tx::<()>(|t| {
            assert_eq!(t.heads()?[0].produced, 1);
            t.bump("panic_fixture")?;
            panic!("caller only");
        })
        .unwrap();
    }));
    assert!(panic.is_err());
    assert!(!j.failed());
    assert!(events(&j).is_empty());
    j.flush().unwrap();
    assert_eq!(events(&j).len(), 1);
    j.tx(|_| Ok(())).unwrap();
}

#[test]
fn multiple_closing_facts_fail_atomically_and_retry_once() {
    let (_dir, j) = journal();
    let closing = vec![
        usage(1, UsageScope::Main, Measurement::Final),
        (
            subject(),
            EventBody::MessageCompleted {
                message: "m".into(),
            },
        ),
    ];
    j.call(|w| w.db.execute_batch("CREATE TEMP TRIGGER fail_second BEFORE INSERT ON events WHEN NEW.event LIKE '%message_completed%' BEGIN SELECT RAISE(ABORT, 'second closing fact'); END;").map_err(internal)).unwrap();
    assert!(j.events(closing.clone(), None).is_err());
    assert!(events(&j).is_empty());
    j.call(|w| {
        w.db.execute_batch("DROP TRIGGER fail_second")
            .map_err(internal)
    })
    .unwrap();
    j.events(closing.clone(), None).unwrap();
    j.flush().unwrap();
    assert_eq!(
        events(&j)
            .into_iter()
            .map(|e| (e.subject, e.body))
            .collect::<Vec<_>>(),
        closing
    );
}

#[test]
fn failed_window_is_not_returned_to_the_next_producer() {
    let (_dir, j) = journal();
    j.call(|w| {
        w.enqueue(prepare(vec![delta(0, "first session")])?)?;
        w.db.execute_batch("CREATE TEMP TRIGGER fail_window BEFORE INSERT ON events WHEN NEW.event LIKE '%text_delta%' BEGIN SELECT RAISE(ABORT, 'window failure'); END;").map_err(internal)?;
        assert!(w.flush().is_err());
        Ok(())
    }).unwrap();
    assert!(j.failed());
    // Another session's closing fact gets its own result rather than the stale window error.
    // Removing the injected failure also lets this control transaction carry the retained tail.
    j.call(|w| {
        w.db.execute_batch("DROP TRIGGER fail_window")
            .map_err(internal)
    })
    .unwrap();
    j.events(
        vec![(
            Subject::Session {
                session: SessionId::new("other").unwrap(),
            },
            EventBody::MessageCompleted {
                message: "other".into(),
            },
        )],
        None,
    )
    .unwrap();
    assert_eq!(events(&j).len(), 2);
}

#[test]
fn threshold_flush_errors_stay_on_the_alarm_and_do_not_reject_the_next_session() {
    let (_dir, j) = journal();
    j.call(|w| {
        w.db.execute_batch("CREATE TEMP TRIGGER fail_window BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'window failure'); END;").map_err(internal)?;
        // Ownership has transferred before the threshold triggers an IO failure.
        w.enqueue(prepare((0..64).map(|_| plan("accepted")).collect())?)?;
        assert!(w.metrics.alarm.failed());
        assert!(!w.pending.events.is_empty());
        let mut next = plan("next session");
        next.0 = Subject::Session { session: SessionId::new("other").unwrap() };
        w.enqueue(prepare(vec![next])?)?;
        w.db.execute_batch("DROP TRIGGER fail_window").map_err(internal)?;
        w.flush()
    }).unwrap();
    assert_eq!(events(&j).len(), 2);
}

#[test]
fn a_lost_window_cannot_override_an_explicit_cancel_or_other_stop_reason() {
    let (_dir, j) = journal();
    let epoch = j.budget_epoch();
    j.metrics.alarm.fail(&NodeStop::JournalBudget.error());
    let cancelled: Completion =
        serde_json::from_value(json!({"outcome":"cancelled", "stop_reason":"cancelled"})).unwrap();
    assert_eq!(j.recorded_completion(epoch, cancelled.clone()), cancelled);
    let timeout: Completion =
        serde_json::from_value(json!({"outcome":"timed_out", "stop_reason":"timeout"})).unwrap();
    assert_eq!(j.recorded_completion(epoch, timeout.clone()), timeout);
}

#[test]
fn admission_hysteresis_does_not_stop_work_that_still_fits() {
    let (_dir, j) = journal();
    let epoch = j.budget_epoch();
    j.set_full(true);
    assert!(j.full());
    assert!(!j.over_limit());
    assert!(!j.failed());
    assert_eq!(j.budget_epoch(), epoch);
    j.events(vec![delta(0, "still fits")], None).unwrap();
    j.flush().unwrap();
    assert_eq!(events(&j).len(), 1);
}

#[test]
fn timer_failure_is_reported_by_the_shared_alarm() {
    let (_dir, j) = journal();
    j.call(|w| {
        w.storage.free_space = Some(Arc::new(|| panic!("timer observer fixture")));
        Ok(())
    })
    .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while !j.failed() {
        assert!(Instant::now() < until, "timer exit was not reported");
        std::thread::sleep(Duration::from_millis(5));
    }
    // The connection is still owned by the mutex and poison remains recoverable.
    assert!(j.read(|t| t.heads()).unwrap().is_empty());
}
