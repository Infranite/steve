//! The actual Normalizer -> durable inbox -> ordered apply -> stats read path, with no model,
//! network, Node worker or production state. Source and receipt times are deliberately different.
use super::*;
use steve_node_api::testkit::FakeNode;

#[path = "../../../tests/node_runtime/clock.rs"]
mod test_clock;
use steve_stats::realtime::{Quality, Realtime, Scope};
use test_clock::ManualClock;

struct Noop;
impl NodeDomain for Noop {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}

struct Collected {
    _dir: tempfile::TempDir,
    store: Store,
    rt: Arc<NodeRuntime>,
    at: i64,
    seq: u64,
}

impl Collected {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("db.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        // receipt times come from this clock, so a slow machine cannot push them past `at`
        let clock = Arc::new(ManualClock::new());
        let at = (clock.now_ms().div_euclid(1000) + 1) * 1000;
        clock.advance_to(at);
        store.with_conn(|c| {
            c.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s','default','custom','.','running',?1,?1)",[at-70_000])?;
            c.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated) VALUES ('s','n','{}','s','r','active','a',?)",[at-70_000])?;
            c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,active_attempt,created,updated) VALUES ('t','s','active','a',?1,?1)",[at-70_000])?;
            c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a','t',0,'n','op','{}','{}','d','planned',?1,?1)",[at-70_000])?;
            c.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_attempt,state,created,updated) VALUES ('op','n','start_turn','{}','d','a','finished',?1,?1)",[at-70_000])?;
            Ok(())
        }).unwrap();
        let rt = NodeRuntime::with_clock(store.clone(), FakeNode::new("n"), Arc::new(Noop), clock);
        Self {
            _dir: dir,
            store,
            rt,
            at,
            seq: 0,
        }
    }
    fn event(&mut self, at: i64, body: EventBody) -> EventBatch {
        self.seq += 1;
        EventBatch {
            events: vec![NodeEvent {
                schema: 1,
                id: idv(format!("ev{}", self.seq)).unwrap(),
                at: Timestamp(at),
                cursor: Watermark {
                    stream: idv("st").unwrap(),
                    epoch: idv("e").unwrap(),
                    seq: self.seq,
                },
                subject: Subject::Execution(ExecutionRef::Turn {
                    session: idv("s").unwrap(),
                    turn: idv("t").unwrap(),
                    attempt: idv("a").unwrap(),
                }),
                body,
            }],
        }
    }
    fn ingest(&mut self, at: i64, body: EventBody) -> EventBatch {
        let batch = self.event(at, body);
        self.rt.receive(&batch).unwrap();
        self.rt.apply_some();
        batch
    }
    fn read(&self, at: i64) -> Realtime {
        self.store
            .reader(|c| {
                steve_stats::realtime::read(
                    c,
                    Scope {
                        workspace: "default",
                        project: None,
                        bot: None,
                    },
                    at,
                )
            })
            .unwrap()
    }
    fn units(&self) -> i64 {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT COALESCE(SUM(units),0) FROM realtime_output_samples",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }
    fn text(&mut self, at: i64, offset: u64, text: &str) -> EventBatch {
        self.ingest(
            at,
            EventBody::TextDelta {
                message: "m".into(),
                block: "b".into(),
                utf8_offset: offset,
                text: text.into(),
            },
        )
    }
    fn start(&mut self, at: i64) {
        self.ingest(at, EventBody::AttemptStarted);
        self.ingest(
            at,
            EventBody::MessageStarted {
                message: "m".into(),
                block: "b".into(),
                role: Role::Assistant,
            },
        );
    }
}

fn stopped() -> EventBody {
    EventBody::AttemptEnded {
        completion: Completion {
            outcome: Outcome::Succeeded,
            stop_reason: "end_turn".into(),
            error: None,
            result: vec![],
            continuation: None,
            transient: false,
            acted: false,
            job: None,
        },
    }
}

fn assert_bad_end_keeps_recent_output(relative: bool, source_at: i64) {
    let mut f = Collected::new();
    f.start(f.at - 60_000);
    f.text(f.at - 1_000, 0, &"a".repeat(400));
    let source_at = if relative {
        f.at + source_at
    } else {
        source_at
    };
    f.ingest(source_at, stopped());
    // the receipt's second (the fixture clock stands at `f.at`) is visible a second later
    let at = f.at + 1_000;
    let r = f.read(at);
    assert_eq!(
        (r.state, r.tps, r.output_sessions, r.estimated_tps),
        (Quality::Partial, None, None, Some(10.0)),
        "bad stop time must retain the output and missing coverage"
    );
    f.store.reader(|c| {
        let (ended, effective, source, received, kind): (i64,i64,i64,i64,String) = c.query_row(
            "SELECT a.ended,s.at,s.source_at,s.received_at,s.kind FROM realtime_output_attempts a JOIN realtime_output_samples s USING(attempt_id) ORDER BY s.seq DESC LIMIT 1", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        assert_eq!(source, source_at);
        assert_eq!(kind, "gap");
        assert!(ended >= received && ended >= f.at - 1_000);
        assert_eq!(ended, effective);
        Ok::<_,StoreError>(())
    }).unwrap();
    assert_eq!(f.read(at + 70_000).state, Quality::Idle);
}

#[test]
fn realtime_backwards_end_does_not_erase_recent_output() {
    assert_bad_end_keeps_recent_output(true, -20_000);
}

#[test]
fn realtime_future_end_keeps_gap_until_receipt_tail_expires() {
    assert_bad_end_keeps_recent_output(true, 60_000);
}

#[test]
fn realtime_invalid_end_times_keep_gap_and_source_for_audit() {
    for at in [0, -1, i64::MIN, i64::MAX] {
        assert_bad_end_keeps_recent_output(false, at);
    }
}

#[test]
fn realtime_upgrade_late_node_stop_resolves_only_the_unknown_tail() {
    let mut f = Collected::new();
    f.start(f.at - 60_000);
    // An installation upgrading mid-attempt has runtime intent, but no telemetry baseline.
    f.store
        .with_conn(|c| {
            c.execute(
                "DELETE FROM realtime_output_attempts WHERE attempt_id='a'",
                [],
            )?;
            c.execute(
                "UPDATE runtime_attempts SET state='finished',updated=? WHERE attempt_id='a'",
                [f.at - 20_000],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(f.read(f.at).state, Quality::Unavailable);
    // The source stop is late, but still ordered and valid. Arrival must not shift its boundary.
    let batch = f.ingest(f.at - 5_000, stopped());
    assert_eq!(f.read(f.at).state, Quality::Unavailable);
    let r = f.read(f.at + 5_000);
    assert_eq!(
        (r.state, r.tps, r.output_sessions),
        (Quality::Idle, Some(0.0), Some(0))
    );
    assert_eq!(r.series[0].state, Quality::Unavailable);
    assert_eq!(f.read(f.at + 63_000).series[0].state, Quality::Unavailable);
    assert!(
        f.read(f.at + 64_000)
            .series
            .iter()
            .all(|p| p.state == Quality::Idle)
    );
    let before = serde_json::to_value(f.read(f.at + 5_000)).unwrap();
    f.rt.receive(&batch).unwrap();
    f.rt.apply_some();
    let reopened = Store::open(f.store.path(), steve_store::NoPublisher).unwrap();
    let after = reopened
        .reader(|c| {
            steve_stats::realtime::read(
                c,
                Scope {
                    workspace: "default",
                    project: None,
                    bot: None,
                },
                f.at + 5_000,
            )
        })
        .unwrap();
    assert_eq!(before, serde_json::to_value(after).unwrap());
}

#[test]
fn realtime_upgrade_can_collect_a_suffix_without_inventing_a_baseline() {
    let mut f = Collected::new();
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE runtime_attempts SET state='running' WHERE attempt_id='a'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    // First event after upgrading is a new message, not AttemptStarted.
    f.ingest(
        f.at - 1_000,
        EventBody::MessageStarted {
            message: "m".into(),
            block: "b".into(),
            role: Role::Assistant,
        },
    );
    f.text(f.at - 1_000, 0, &"a".repeat(400));
    let r = f.read(f.at);
    assert_eq!(
        (r.state, r.tps, r.estimated_tps),
        (Quality::Partial, None, Some(10.0))
    );
    let stop = f.ingest(f.at, stopped());
    f.store
        .reader(|c| {
            assert_eq!(
                c.query_row(
                    "SELECT baseline,ended FROM realtime_output_attempts WHERE attempt_id='a'",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                )?,
                (0, f.at)
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert_eq!(f.read(f.at + 9_000).state, Quality::Unavailable);
    assert_eq!(f.read(f.at + 10_000).state, Quality::Idle);
    let units = f.units();
    f.rt.receive(&stop).unwrap();
    f.rt.apply_some();
    assert_eq!(f.units(), units);
}

#[test]
fn realtime_normalized_text_is_persisted_and_final_usage_never_spikes() {
    let mut f = Collected::new();
    f.ingest(f.at - 20_000, EventBody::AttemptStarted);
    let mut normalizer = steve_acp::normalize::Normalizer::new(".", "a", idv("meter").unwrap());
    for text in ["abcd", "中文", "🙂"] {
        for fact in normalizer.update(
            &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}),
        ) {
            if let steve_acp::normalize::Fact::Event(body) = fact {
                f.ingest(f.at - 1_000, body);
            }
        }
    }
    assert_eq!(f.units(), 16); // four estimated tokens, from actual content, not three chunks
    let before = f.read(f.at);
    assert_eq!(
        (before.state, before.tps, before.output_sessions),
        (Quality::Estimated, Some(0.4), Some(1))
    );
    for fact in normalizer.prompt_response(&json!({"usage":{"inputTokens":100,"outputTokens":900_000},"_meta":{"quota":{"model_usage":[{"model":"model","token_count":{"outputTokens":900_000}}]}}})) {
        if let steve_acp::normalize::Fact::Event(body)=fact { f.ingest(f.at,body); }
    }
    assert_eq!(f.units(), 16);
    assert_eq!(f.read(f.at).tps, Some(0.4));
    f.ingest(
        f.at,
        EventBody::AttemptEnded {
            completion: Completion {
                outcome: Outcome::Succeeded,
                stop_reason: "end_turn".into(),
                error: None,
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            },
        },
    );
    assert_eq!(f.read(f.at + 8_000).tps, Some(0.4));
    let ended = f.read(f.at + 9_000);
    assert_eq!(
        (ended.state, ended.tps, ended.output_sessions),
        (Quality::Idle, Some(0.0), Some(0))
    );
    assert_eq!(
        f.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(
                "SELECT COUNT(*) FROM realtime_output_samples WHERE kind='end'",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        1
    );
}

#[test]
fn realtime_replay_overlap_reopen_and_pruned_inbox_do_not_duplicate() {
    let mut f = Collected::new();
    f.start(f.at - 20_000);
    let batch = f.text(f.at - 1_000, 0, "abcd中");
    assert_eq!(f.units(), 8);
    let ack = f.rt.receive(&batch).unwrap();
    assert_eq!(ack, f.rt.receive(&batch).unwrap());
    f.rt.apply_some();
    f.text(f.at - 1_000, 0, "abcd中"); // same content, new source position
    f.text(f.at - 1_000, 4, "中文"); // overlaps 中, adds 文 only
    assert_eq!(f.units(), 12);
    f.rt = NodeRuntime::new(f.store.clone(), FakeNode::new("n"), Arc::new(Noop));
    f.text(f.at, 10, "efgh"); // block cursor, not in-memory transcript roles, survives replacement
    assert_eq!(f.units(), 16);
    assert_eq!(f.read(f.at).tps, Some(0.4));
    // The ordinary inbox cleanup keeps open attempts. After an end, cleanup can remove raw
    // payloads; telemetry and its replay watermarks remain independently readable.
    f.ingest(
        f.at,
        EventBody::AttemptEnded {
            completion: Completion {
                outcome: Outcome::Succeeded,
                stop_reason: "end_turn".into(),
                error: None,
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            },
        },
    );
    let answer = serde_json::to_value(f.read(f.at)).unwrap();
    f.rt.prune_inbox(now() + INBOX_KEEP_MS + 1);
    assert_eq!(serde_json::to_value(f.read(f.at)).unwrap(), answer);
    f.rt.receive(&batch).unwrap();
    f.rt.apply_some();
    assert_eq!(f.units(), 16);
    let reopened = Store::open(f.store.path(), steve_store::NoPublisher).unwrap();
    let r = reopened
        .reader(|c| {
            steve_stats::realtime::read(
                c,
                Scope {
                    workspace: "default",
                    project: None,
                    bot: None,
                },
                f.at,
            )
        })
        .unwrap();
    assert_eq!(serde_json::to_value(r).unwrap(), answer);
}

#[test]
fn realtime_apply_rollback_rolls_back_samples_and_offsets() {
    let mut f = Collected::new();
    f.start(f.at - 1_000);
    f.store.with_conn(|c| {c.execute_batch("CREATE TRIGGER refuse_telemetry BEFORE UPDATE OF applied ON node_streams WHEN NEW.applied>OLD.applied BEGIN SELECT RAISE(ABORT,'rollback sample'); END;")?;Ok(())}).unwrap();
    let batch = f.text(f.at, 0, "abcd");
    assert_eq!(f.units(), 0);
    f.store
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER refuse_telemetry")?;
            Ok(())
        })
        .unwrap();
    f.rt.apply_some();
    assert_eq!(f.units(), 4);
    f.rt.receive(&batch).unwrap();
    f.rt.apply_some();
    assert_eq!(f.units(), 4);
    assert_eq!(f.read(f.at).tps, Some(0.1));
}

#[test]
fn realtime_missing_offsets_and_future_clocks_are_unknown_and_late_text_is_not_shifted() {
    let mut f = Collected::new();
    f.start(f.at - 30_000);
    f.text(f.at - 11_000, 0, "a".repeat(40_000).as_str()); // received now, source outside window
    f.text(f.at - 1_000, 40_000, "abcd");
    assert_eq!(f.read(f.at).tps, Some(0.1));
    f.text(f.at, 40_010, "missing-prefix");
    let gap = f.read(f.at);
    assert_eq!(
        (gap.state, gap.tps, gap.coverage.unavailable),
        (Quality::Partial, None, 1)
    );
    assert_eq!(gap.estimated_tps, Some(0.1));
    let before = f.units();
    f.text(f.at + 60_000, 40_024, "future");
    assert_eq!(f.units(), before);
    assert!(f.read(f.at + 1_000).tps.is_none());
}

#[test]
fn realtime_pending_prefix_and_unknown_attempt_do_not_report_a_complete_rate() {
    let mut f = Collected::new();
    f.start(f.at - 1_000);
    f.text(f.at - 1_000, 0, "abcd");
    let missing = f.event(f.at, EventBody::Plan { entries: vec![] });
    let after = f.event(f.at, EventBody::Plan { entries: vec![] });
    f.rt.receive(&after).unwrap();
    f.rt.apply_some();
    assert_eq!(f.read(f.at + 1_000).state, Quality::Partial);
    f.rt.receive(&missing).unwrap();
    f.rt.apply_some();
    assert_eq!(f.read(f.at + 1_000).tps, Some(0.1));
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE runtime_attempts SET state='unknown',updated=?",
                [f.at],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(f.read(f.at + 1_000).tps, None);
}

#[test]
fn realtime_positive_final_usage_without_streamed_text_is_missing_not_idle() {
    let mut f = Collected::new();
    f.ingest(f.at - 1_000, EventBody::AttemptStarted);
    f.ingest(
        f.at,
        EventBody::Usage {
            report: UsageReport {
                report_id: "final".into(),
                model: None,
                meter_epoch: idv("meter").unwrap(),
                scope: UsageScope::Main,
                measurement: Measurement::Final,
                tokens: Tokens {
                    output: Some(5000),
                    ..Default::default()
                },
                context_used: None,
                context_limit: None,
                cost: None,
            },
        },
    );
    let answer = f.read(f.at);
    assert_eq!(
        (answer.state, answer.tps, answer.output_sessions),
        (Quality::Unavailable, None, None)
    );
    assert_eq!(f.units(), 0);
}

#[test]
fn realtime_concurrent_collection_freezes_project_and_bot_attribution() {
    let mut f = Collected::new();
    f.store.with_conn(|c| {
        c.execute_batch("INSERT INTO projects(id,workspace_id,name,path,created,updated) VALUES ('p1','default','one','.',1,1),('p2','default','two','.',1,1);
            INSERT INTO items(id,workspace_id,no,title,status,stage,project_id,created,updated) VALUES ('i1','default',1,'one','进行中','执行','p1',1,1),('i2','default',2,'two','进行中','执行','p2',1,1);
            UPDATE sessions SET item_id='i1',bot_id='b1' WHERE id='s';
            INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s2','default','i2','b2','custom','.','running',1,1);
            INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated) VALUES ('s2','n','{}','s2','r','active','a2',1);
            INSERT INTO runtime_turns(turn_id,session_id,state,active_attempt,created,updated) VALUES ('t2','s2','active','a2',1,1);
            INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a2','t2',0,'n','op2','{}','{}','d','planned',1,1);")?;
        Ok(())
    }).unwrap();
    f.start(f.at - 1_000);
    f.text(f.at - 1_000, 0, "abcd");
    let bodies = [
        EventBody::AttemptStarted,
        EventBody::MessageStarted {
            message: "other".into(),
            block: "block".into(),
            role: Role::Thought,
        },
        EventBody::TextDelta {
            message: "other".into(),
            block: "block".into(),
            utf8_offset: 0,
            text: "abcdefgh".into(),
        },
    ];
    let events = bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| NodeEvent {
            schema: 1,
            id: idv(format!("other-{i}")).unwrap(),
            at: Timestamp(f.at - 1_000),
            cursor: Watermark {
                stream: idv("other-stream").unwrap(),
                epoch: idv("e").unwrap(),
                seq: i as u64 + 1,
            },
            subject: Subject::Execution(ExecutionRef::Turn {
                session: idv("s2").unwrap(),
                turn: idv("t2").unwrap(),
                attempt: idv("a2").unwrap(),
            }),
            body,
        })
        .collect();
    f.rt.receive(&EventBatch { events }).unwrap();
    f.rt.apply_some();
    let all = f.read(f.at);
    assert_eq!(
        (all.tps, all.output_sessions, all.coverage.estimated),
        (Some(0.3), Some(2), 2)
    );
    f.store.with_conn(|c|{c.execute_batch("UPDATE items SET project_id='p2' WHERE id='i1'; UPDATE sessions SET bot_id='b2' WHERE id='s';")?;Ok(())}).unwrap();
    for (project, bot, rate) in [("p1", "b1", 0.1), ("p2", "b2", 0.2), ("p1", "b2", 0.0)] {
        let r = f
            .store
            .reader(|c| {
                steve_stats::realtime::read(
                    c,
                    Scope {
                        workspace: "default",
                        project: Some(project),
                        bot: Some(bot),
                    },
                    f.at,
                )
            })
            .unwrap();
        assert_eq!(r.tps, Some(rate));
    }
}
