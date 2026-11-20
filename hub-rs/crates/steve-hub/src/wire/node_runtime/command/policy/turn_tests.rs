use super::super::Gates;
use super::super::history::{Revision, Settled, Terminal};
use super::super::time::{Delay, Due, Jitter, NodeTime};
use super::*;
use steve_node_api::{IntentKey, Outcome, TurnId};

fn intent() -> Intent {
    Intent::StartTurn(TurnId::new("t").unwrap())
}
fn facts() -> TurnFacts {
    TurnFacts {
        acted: false,
        outcome: Outcome::Failed,
        cancelled: false,
        gates: Gates {
            session_open: true,
            item_in_progress: true,
            no_pending_decision: true,
        },
        workflow_blocked: false,
        configuration_pending: false,
    }
}
fn rows(intent: &Intent, last: u32) -> Vec<Revision> {
    (0..=last)
        .map(|revision| Revision {
            key: IntentKey {
                intent: intent.clone(),
                revision,
            },
            settled: Some(Settled {
                terminal: Terminal::FinishedFailed,
                retry: Retry::Later { after_ms: None },
                at: HubTime(1000 + i64::from(revision)),
            }),
        })
        .collect()
}
fn capacity<'a>(intent: &'a Intent, rows: &'a [Revision]) -> TurnCapacity<'a> {
    TurnCapacity {
        history: History {
            intent,
            revisions: rows,
        },
        facts: facts(),
        now: HubTime(2000),
        jitter: Jitter::new(1000).unwrap(),
        first_capacity_failure: Some(HubTime(1000)),
        source: HintSource::JournalEvent { at: NodeTime(2000) },
    }
}
fn at(decision: Decision) -> super::super::Schedule {
    let Decision::Schedule {
        action: Action::NextRevision,
        at,
    } = decision
    else {
        panic!("expected revision: {decision:?}")
    };
    at
}

#[test]
fn capacity_has_three_retries_and_is_deterministic_across_reconstructed_history() {
    let intent = intent();
    for ordinal in 0..=3 {
        let rows = rows(&intent, ordinal);
        let rule = capacity(&intent, &rows);
        assert_eq!(rule.decide(Retry::Never), Decision::Stop(Refusal::Never));
        for retry in [
            Retry::Now,
            Retry::Later { after_ms: None },
            Retry::Later {
                after_ms: Some(30_000),
            },
        ] {
            let decision = rule.decide(retry);
            let restarted = rows.clone();
            assert_eq!(decision, capacity(&intent, &restarted).decide(retry));
            if ordinal == 3 {
                assert_eq!(decision, Decision::Stop(Refusal::Budget));
            } else {
                let schedule = at(decision);
                let expected = match retry {
                    Retry::Later { after_ms: Some(_) } => 33_000,
                    _ => 3000 + CAPACITY_DELAYS_MS[ordinal as usize] as i64,
                };
                assert_eq!(schedule.due, HubTime(expected));
                assert_eq!(schedule.deadline, Some(HubTime(901_000)));
            }
        }
    }
}

#[test]
fn every_business_gate_cancellation_and_outcome_is_an_explicit_input() {
    let intent = intent();
    let rows = rows(&intent, 0);
    let rule = capacity(&intent, &rows);
    for facts in [
        TurnFacts {
            cancelled: true,
            ..facts()
        },
        TurnFacts {
            outcome: Outcome::Succeeded,
            ..facts()
        },
        TurnFacts {
            outcome: Outcome::Cancelled,
            ..facts()
        },
        TurnFacts {
            workflow_blocked: true,
            ..facts()
        },
        TurnFacts {
            configuration_pending: true,
            ..facts()
        },
        TurnFacts {
            gates: Gates {
                session_open: false,
                ..facts().gates
            },
            ..facts()
        },
        TurnFacts {
            gates: Gates {
                item_in_progress: false,
                ..facts().gates
            },
            ..facts()
        },
        TurnFacts {
            gates: Gates {
                no_pending_decision: false,
                ..facts().gates
            },
            ..facts()
        },
    ] {
        for retry in [Retry::Never, Retry::Now, Retry::Later { after_ms: None }] {
            assert!(matches!(
                TurnCapacity { facts, ..rule }.decide(retry),
                Decision::Stop(_)
            ));
        }
    }
    assert!(matches!(
        TurnCapacity {
            facts: TurnFacts {
                acted: true,
                ..facts()
            },
            ..rule
        }
        .decide(Retry::Now),
        Decision::Schedule { .. }
    ));
}

#[test]
fn interrupted_turns_keep_twenty_and_sixty_seconds_even_after_acting() {
    let intent = intent();
    for ordinal in 0..=2 {
        let rows = rows(&intent, ordinal);
        let cap = capacity(&intent, &rows);
        let rule = TurnInterrupted {
            history: cap.history,
            facts: cap.facts,
            now: cap.now,
        };
        for retry in [Retry::Never, Retry::Later { after_ms: None }, Retry::Now] {
            let decision = rule.decide(retry);
            let restarted = rows.clone();
            assert_eq!(
                decision,
                TurnInterrupted {
                    history: History {
                        intent: &intent,
                        revisions: &restarted
                    },
                    ..rule
                }
                .decide(retry)
            );
            if retry == Retry::Never {
                assert_eq!(decision, Decision::Stop(Refusal::Never));
            } else if ordinal == 2 {
                assert_eq!(decision, Decision::Stop(Refusal::Budget));
            } else {
                assert_eq!(
                    at(decision).due,
                    HubTime(2000 + INTERRUPTED_DELAYS_MS[ordinal as usize] as i64)
                );
            }
        }
        assert_eq!(
            TurnInterrupted {
                facts: TurnFacts {
                    acted: true,
                    ..facts()
                },
                ..rule
            }
            .decide(Retry::Now),
            rule.decide(Retry::Now)
        );
    }
    for retry in [Retry::Never, Retry::Later { after_ms: None }, Retry::Now] {
        let rule = TurnInterruptedAfterActing {
            facts: TurnFacts {
                acted: true,
                ..facts()
            },
        };
        let decision = rule.decide(retry);
        assert_eq!(decision, rule.decide(retry));
        assert_eq!(decision, Decision::CheckBeforeContinuing);
        assert_eq!(
            TurnInterruptedAfterActing { facts: facts() }.decide(retry),
            Decision::Stop(Refusal::NotActed)
        );
    }
}

#[test]
fn a_past_hint_uses_now_plus_jitter_and_never_sends_at_the_ceiling() {
    let intent = intent();
    let rows = rows(&intent, 0);
    let base = capacity(&intent, &rows);
    let retry = Retry::Later { after_ms: Some(0) };
    let wait = at(TurnCapacity {
        source: HintSource::JournalEvent {
            at: NodeTime(-1_000_000),
        },
        ..base
    }
    .decide(retry));
    assert_eq!(wait.due, HubTime(3000));
    for now in [900_999, 901_000, 901_001] {
        let decision = TurnCapacity {
            now: HubTime(now),
            ..base
        }
        .decide(retry);
        assert_eq!(decision, Decision::Stop(Refusal::Deadline));
    }
}

#[test]
fn positive_and_negative_node_skew_are_clamped_by_hub_bounds() {
    let intent = intent();
    let rows = rows(&intent, 0);
    let base = capacity(&intent, &rows);
    for (skew, expected) in [
        (-50_000, 3000),
        (0, 33_000),
        (10_000, 43_000),
        (2_000_000, 901_000),
    ] {
        let rule = TurnCapacity {
            source: HintSource::JournalEvent {
                at: NodeTime(2000 + skew),
            },
            ..base
        };
        if skew == 2_000_000 {
            assert_eq!(
                rule.decide(Retry::Later {
                    after_ms: Some(30_000)
                }),
                Decision::Stop(Refusal::Deadline)
            );
            continue;
        }
        let wait = at(rule.decide(Retry::Later {
            after_ms: Some(30_000),
        }));
        assert_eq!(wait.due, HubTime(expected));
        assert!(wait.due >= base.now && wait.due <= wait.deadline.unwrap());
    }
}

#[test]
fn recovery_at_or_past_the_ceiling_never_creates_a_send_plan() {
    let intent = intent();
    let rows = rows(&intent, 0);
    let rule = capacity(&intent, &rows);
    // Event at 2000, deadline at 901000: exact recovery, later recovery, jitter crossing.
    for after_ms in [899_000, 899_001, 898_999] {
        assert_eq!(
            rule.decide(Retry::Later {
                after_ms: Some(after_ms)
            }),
            Decision::Stop(Refusal::Deadline)
        );
    }
    // A genuinely earlier recovery may send; it was not squeezed down from the ceiling.
    let before = at(TurnCapacity { ..rule }.decide(Retry::Later {
        after_ms: Some(897_999),
    }));
    assert_eq!(before.due, HubTime(900_999));
    assert_eq!(before.status(HubTime(900_999)), Due::Ready);
    assert_eq!(before.status(HubTime(901_000)), Due::Expired);
}

#[test]
fn persistent_skew_can_exhaust_all_three_retries_early_but_never_grows_the_budget() {
    let intent = intent();
    for node_skew in [-1_000_000, 10_000] {
        for ordinal in 0..=3 {
            let rows = rows(&intent, ordinal);
            let rule = TurnCapacity {
                source: HintSource::JournalEvent {
                    at: NodeTime(1000 + node_skew),
                },
                ..capacity(&intent, &rows)
            };
            let decision = rule.decide(Retry::Later {
                after_ms: Some(10_000),
            });
            if ordinal == 3 {
                assert_eq!(decision, Decision::Stop(Refusal::Budget));
            } else {
                let wait = at(decision);
                assert_eq!(wait.deadline, Some(HubTime(901_000)));
                assert!(wait.due.0 <= 22_000);
                assert_ne!(wait.status(HubTime(901_000)), Due::Ready);
            }
        }
    }
}

#[test]
fn journal_append_delay_shifts_absolute_and_delta_hints_on_the_hub_half() {
    let intent = intent();
    let rows = rows(&intent, 0);
    let base = capacity(&intent, &rows);
    let absolute = Delay::absolute(NodeTime(32_000), NodeTime(2000));
    let delta = Delay::delta_seconds(30).unwrap();
    assert_eq!(absolute, delta);
    for delay in [absolute, delta] {
        let early = at(base.decide(Retry::Later {
            after_ms: Some(delay.0),
        }));
        let late = at(TurnCapacity {
            source: HintSource::JournalEvent { at: NodeTime(7000) },
            ..base
        }
        .decide(Retry::Later {
            after_ms: Some(delay.0),
        }));
        assert_eq!(early.due, HubTime(33_000));
        assert_eq!(late.due, HubTime(38_000));
        // after_ms + journal.at cannot recover the original recognition clock fact.
        assert_eq!(late.due.0 - early.due.0, 5000);
    }
}

#[test]
fn delivery_replay_and_restart_use_the_saved_schedule_without_rolling_jitter() {
    let intent = intent();
    let rows = rows(&intent, 0);
    let first = capacity(&intent, &rows);
    let retry = Retry::Later {
        after_ms: Some(30_000),
    };
    let saved = at(first.decide(retry));
    assert_eq!(saved, at(first.decide(retry)));
    let late = TurnCapacity {
        now: HubTime(100_000),
        ..first
    };
    // Recomputing with changed inputs is NOT replay; W3.3s must read the saved value.
    assert_ne!(at(late.decide(retry)).due, saved.due);
    assert_eq!(saved.status(late.now), Due::Ready);
    assert_eq!(saved.status(HubTime(901_000)), Due::Expired);
    let restored = super::super::Schedule {
        due: saved.due,
        deadline: saved.deadline,
        jitter_ms: saved.jitter_ms,
    };
    assert_eq!(restored, saved);
    assert_eq!(restored.status(late.now), saved.status(late.now));
    assert_eq!(
        at(TurnCapacity {
            now: HubTime(20_000),
            ..first
        }
        .decide(retry)),
        saved
    );
}

#[test]
fn unanchored_receipts_and_completion_copies_cannot_consume_after_ms() {
    let intent = intent();
    let rows = rows(&intent, 0);
    let base = capacity(&intent, &rows);
    for source in [HintSource::CommandReceipt, HintSource::CompletionCopy] {
        let rule = TurnCapacity { source, ..base };
        assert_eq!(
            rule.decide(Retry::Later {
                after_ms: Some(30_000)
            }),
            rule.decide(Retry::Later { after_ms: None })
        );
        assert_eq!(
            at(rule.decide(Retry::Later { after_ms: None })).due,
            HubTime(23_000)
        );
    }
}

#[test]
fn invalid_and_overflowed_numeric_hints_do_not_become_immediate_retries() {
    assert_eq!(Delay::delta_seconds(-1), Err(Refusal::InvalidTime));
    assert_eq!(Delay::delta_seconds(i64::MAX), Err(Refusal::InvalidTime));
    assert_eq!(Delay::absolute(NodeTime(1), NodeTime(2000)), Delay(0));
    let intent = intent();
    let rows = rows(&intent, 0);
    let base = capacity(&intent, &rows);
    let expired = TurnCapacity {
        source: HintSource::JournalEvent {
            at: NodeTime(1_000_000),
        },
        ..base
    };
    assert_eq!(
        at(expired.decide(Retry::Later { after_ms: Some(0) })).due,
        HubTime(3000)
    );
    assert_eq!(
        base.decide(Retry::Later {
            after_ms: Some(u64::MAX)
        }),
        Decision::Stop(Refusal::InvalidTime)
    );
    assert_eq!(
        TurnCapacity {
            now: HubTime(0),
            ..base
        }
        .decide(Retry::Now),
        Decision::Stop(Refusal::InvalidTime)
    );
}
