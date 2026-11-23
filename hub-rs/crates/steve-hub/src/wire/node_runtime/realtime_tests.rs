//! The actual Normalizer -> receive (ordered apply) -> stats read path, with no model,
//! network, Node worker or production state. Source and receipt times are deliberately different.
use super::*;
use steve_node_api::testkit::FakeNode;
use steve_store::testkit::hub::realtime;

use steve_runtime::FakeClock;
use steve_stats::realtime::{Quality, Realtime, Scope};

struct Noop;
impl NodeDomain for Noop {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}

struct Collected {
    _dir: tempfile::TempDir,
    store: Store,
    rt: Arc<NodeRuntime>,
    clock: Arc<FakeClock>,
    at: i64,
    seq: u64,
}

impl Collected {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        // Fixed in 2030: receipt time must not depend on the machine's wall clock.
        let clock = Arc::new(FakeClock::new(1_893_456_000_000));
        let at = (clock.now().div_euclid(1000) + 1) * 1000;
        clock.advance_to(at);
        realtime::seed(&store, at - 70_000).unwrap();
        let rt = NodeRuntime::with_clock(
            store.clone(),
            FakeNode::new("n"),
            Arc::new(Noop),
            clock.clone(),
        );
        Self {
            _dir: dir,
            store,
            rt,
            clock,
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
        batch
    }
    fn read(&self, at: i64) -> Realtime {
        self.store
            .read(|c| {
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
        self.store.read(realtime::units).unwrap()
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
    let row = f.store.read(realtime::last_sample).unwrap().unwrap();
    assert_eq!(row.source, source_at);
    assert_eq!(row.kind, "gap");
    assert!(row.ended >= row.received && row.ended >= f.at - 1_000);
    assert_eq!(row.ended, row.effective);
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
    realtime::forget_attempt(&f.store, f.at - 20_000).unwrap();
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
    let reopened = Store::open(f.store.path(), steve_store::NoPublisher).unwrap();
    let after = reopened
        .read(|c| {
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
    realtime::set_attempt(&f.store, "running", None).unwrap();
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
    assert_eq!(
        f.store.read(realtime::baseline).unwrap(),
        Some((false, f.at))
    );
    assert_eq!(f.read(f.at + 9_000).state, Quality::Unavailable);
    assert_eq!(f.read(f.at + 10_000).state, Quality::Idle);
    let units = f.units();
    f.rt.receive(&stop).unwrap();
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
    assert_eq!(f.store.read(realtime::ends).unwrap(), 1);
}

#[test]
fn realtime_replay_overlap_and_reopen_do_not_duplicate() {
    let mut f = Collected::new();
    f.start(f.at - 20_000);
    let batch = f.text(f.at - 1_000, 0, "abcd中");
    assert_eq!(f.units(), 8);
    let ack = f.rt.receive(&batch).unwrap();
    assert_eq!(ack, f.rt.receive(&batch).unwrap());
    f.text(f.at - 1_000, 0, "abcd中"); // same content, new source position
    f.text(f.at - 1_000, 4, "中文"); // overlaps 中, adds 文 only
    assert_eq!(f.units(), 12);
    f.rt = NodeRuntime::with_clock(
        f.store.clone(),
        FakeNode::new("n"),
        Arc::new(Noop),
        f.clock.clone(),
    );
    f.text(f.at, 10, "efgh"); // block cursor, not in-memory transcript roles, survives replacement
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
    let answer = serde_json::to_value(f.read(f.at)).unwrap();
    assert_eq!(serde_json::to_value(f.read(f.at)).unwrap(), answer);
    f.rt.receive(&batch).unwrap();
    assert_eq!(f.units(), 16);
    let reopened = Store::open(f.store.path(), steve_store::NoPublisher).unwrap();
    let r = reopened
        .read(|c| {
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
    realtime::reject_watermark(&f.store).unwrap();
    let batch = f.event(
        f.at,
        EventBody::TextDelta {
            message: "m".into(),
            block: "b".into(),
            utf8_offset: 0,
            text: "abcd".into(),
        },
    );
    assert!(f.rt.receive(&batch).is_err());
    assert_eq!(f.units(), 0);
    realtime::allow_watermark(&f.store).unwrap();
    f.rt.receive(&batch).unwrap();
    assert_eq!(f.units(), 4);
    f.rt.receive(&batch).unwrap();
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
fn realtime_unknown_attempt_does_not_report_a_complete_rate() {
    let mut f = Collected::new();
    f.start(f.at - 1_000);
    f.text(f.at - 1_000, 0, "abcd");
    assert_eq!(f.read(f.at + 1_000).tps, Some(0.1));
    realtime::set_attempt(&f.store, "unknown", Some(f.at)).unwrap();
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
    realtime::seed_attribution(&f.store).unwrap();
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
    let all = f.read(f.at);
    assert_eq!(
        (all.tps, all.output_sessions, all.coverage.estimated),
        (Some(0.3), Some(2), 2)
    );
    realtime::change_attribution(&f.store).unwrap();
    for (project, bot, rate) in [("p1", "b1", 0.1), ("p2", "b2", 0.2), ("p1", "b2", 0.0)] {
        let r = f
            .store
            .read(|c| {
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
