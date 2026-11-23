//! Compare pure decisions with the existing coordinator's non-text decision formula.
use super::history::{Revision, RevisionWhy, Rule, Settled, Terminal, next_revision};
use super::time::{HintSource, Jitter, NodeTime};
use super::turn::{TurnCapacity, TurnGatewayTransient, TurnInterrupted};
use super::*;
use steve_node_api::{Intent, IntentKey, Outcome, TurnId};
use steve_types::Retry;

fn intent() -> Intent {
    Intent::StartTurn(TurnId::new("t").unwrap())
}
fn history(intent: &Intent, ordinal: u32) -> Vec<Revision> {
    (0..=ordinal)
        .map(|revision| Revision {
            key: IntentKey {
                intent: intent.clone(),
                revision,
            },
            settled: Some(Settled {
                terminal: Terminal::FinishedFailed,
                retry: Retry::Later { after_ms: None },
                at: HubTime(1000),
            }),
        })
        .collect()
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
fn at(decision: Decision) -> Schedule {
    let Decision::Schedule {
        action: Action::NextRevision,
        at,
    } = decision
    else {
        panic!("expected retry: {decision:?}")
    };
    at
}
fn capacity<'a>(intent: &'a Intent, rows: &'a [Revision]) -> TurnCapacity<'a> {
    TurnCapacity {
        history: History {
            intent,
            revisions: rows,
        },
        facts: facts(),
        now: HubTime(2000),
        first_capacity_failure: Some(HubTime(1000)),
        jitter: Jitter::new(1000).unwrap(),
        source: HintSource::JournalEvent { at: NodeTime(2000) },
    }
}

#[test]
fn an_interruption_twenty_minutes_ago_does_not_start_the_capacity_window() {
    let intent = intent();
    let mut rows = history(&intent, 1);
    rows[1].settled.as_mut().unwrap().at = HubTime(1_201_000);
    let base = TurnCapacity {
        now: HubTime(1_201_000),
        first_capacity_failure: None,
        ..capacity(&intent, &rows)
    };
    for first in [None, Some(HubTime(1_201_000))] {
        let rule = TurnCapacity {
            first_capacity_failure: first,
            ..base
        };
        let wait = at(rule.decide(Retry::Now));
        assert_eq!(wait.due, HubTime(1_262_000));
        assert_eq!(wait.deadline, Some(HubTime(2_101_000)));
        let why = RevisionWhy::Policy {
            rule: Rule::TurnCapacity {
                first_capacity_failure: first,
            },
            now: rule.now,
        };
        assert_eq!(next_revision(rule.history, why).unwrap().revision, 2);
        let restarted = rows.clone();
        assert_eq!(
            rule.decide(Retry::Now),
            TurnCapacity {
                history: History {
                    intent: &intent,
                    revisions: &restarted
                },
                ..rule
            }
            .decide(Retry::Now)
        );
    }
    // Conversely, if that old failure WAS capacity, both entry points retain its deadline.
    let old_capacity = TurnCapacity {
        first_capacity_failure: Some(HubTime(1000)),
        ..base
    };
    assert_eq!(
        old_capacity.decide(Retry::Now),
        Decision::Stop(Refusal::Deadline)
    );
    assert_eq!(
        next_revision(
            old_capacity.history,
            RevisionWhy::Policy {
                rule: Rule::TurnCapacity {
                    first_capacity_failure: old_capacity.first_capacity_failure
                },
                now: old_capacity.now,
            }
        ),
        Err(Refusal::Deadline)
    );
}

#[test]
fn jitter_is_validated_once_and_the_full_inclusive_range_is_preserved() {
    for bad in [0, 999, 5001, u64::MAX] {
        assert_eq!(Jitter::new(bad), Err(Refusal::InvalidJitter));
    }
    let intent = intent();
    let rows = history(&intent, 0);
    for ms in 1000..=5000 {
        let jitter = Jitter::new(ms).unwrap();
        assert_eq!(jitter.ms(), ms);
        let wait = at(TurnCapacity {
            jitter,
            ..capacity(&intent, &rows)
        }
        .decide(Retry::Now));
        assert_eq!(wait.due, HubTime(22_000 + ms as i64));
        assert_eq!(wait.jitter_ms, ms);
    }
}

#[test]
fn configuration_pending_is_only_reported_for_an_otherwise_permitted_retry() {
    let intent = intent();
    for (ordinal, now, hint, expected) in [
        (0, 2000, None, Refusal::ConfigurationPending),
        (3, 2000, None, Refusal::Budget),
        (0, 901_000, None, Refusal::Deadline),
        (0, 2000, Some(899_000), Refusal::Deadline),
    ] {
        let rows = history(&intent, ordinal);
        let rule = TurnCapacity {
            now: HubTime(now),
            facts: TurnFacts {
                configuration_pending: true,
                ..facts()
            },
            ..capacity(&intent, &rows)
        };
        assert_eq!(
            rule.decide(Retry::Later { after_ms: hint }),
            Decision::Stop(expected)
        );
    }
    for ordinal in [0, 2] {
        let rows = history(&intent, ordinal);
        let rule = TurnInterrupted {
            history: History {
                intent: &intent,
                revisions: &rows,
            },
            now: HubTime(2000),
            facts: TurnFacts {
                configuration_pending: true,
                ..facts()
            },
        };
        let expected = Decision::Stop(if ordinal == 2 {
            Refusal::Budget
        } else {
            Refusal::ConfigurationPending
        });
        assert_eq!(rule.decide(Retry::Now), expected);
        assert_eq!(
            TurnGatewayTransient {
                history: rule.history,
                now: rule.now,
                facts: rule.facts
            }
            .decide(Retry::Now),
            expected
        );
    }
}

/// The retry rule written out directly, with classification already supplied.
fn expected_retry(f: TurnFacts, service: bool, retryable: bool, ordinal: u32, limit: u32) -> bool {
    let service_gates =
        f.gates.session_open && f.gates.item_in_progress && f.gates.no_pending_decision;
    f.outcome == Outcome::Failed
        && ((service && service_gates) || (!service && retryable && !f.acted))
        && !f.cancelled
        && !f.workflow_blocked
        && ordinal < limit
        && !f.configuration_pending
}

#[test]
fn service_and_gateway_decisions_match_the_existing_predicate_for_the_same_facts() {
    let intent = intent();
    for ordinal in 0..=3 {
        // Each bit independently closes one of the gates, including acted.
        for mask in 0..256 {
            let f = TurnFacts {
                acted: mask & 1 != 0,
                cancelled: mask & 2 != 0,
                outcome: if mask & 4 == 0 {
                    Outcome::Failed
                } else {
                    Outcome::Succeeded
                },
                workflow_blocked: mask & 8 != 0,
                configuration_pending: mask & 16 != 0,
                gates: Gates {
                    session_open: mask & 32 == 0,
                    item_in_progress: mask & 64 == 0,
                    no_pending_decision: mask & 128 == 0,
                },
            };
            let retries = [Retry::Never, Retry::Now, Retry::Later { after_ms: None }];
            for settled_retry in retries {
                let mut rows = history(&intent, ordinal);
                rows.last_mut().unwrap().settled.as_mut().unwrap().retry = settled_retry;
                let h = History {
                    intent: &intent,
                    revisions: &rows,
                };
                for retry in retries {
                    let stream = TurnInterrupted {
                        history: h,
                        facts: f,
                        now: HubTime(2000),
                    }
                    .decide(retry);
                    let gateway = TurnGatewayTransient {
                        history: h,
                        facts: f,
                        now: HubTime(2000),
                    }
                    .decide(retry);
                    let capacity = TurnCapacity {
                        facts: f,
                        ..capacity(&intent, &rows)
                    }
                    .decide(retry);
                    assert_eq!(
                        matches!(stream, Decision::Schedule { .. }),
                        expected_retry(f, true, true, ordinal, 2)
                    );
                    assert_eq!(
                        matches!(gateway, Decision::Schedule { .. }),
                        expected_retry(
                            f,
                            false,
                            retry != Retry::Never && settled_retry != Retry::Never,
                            ordinal,
                            2
                        )
                    );
                    assert_eq!(
                        matches!(capacity, Decision::Schedule { .. }),
                        expected_retry(f, true, true, ordinal, 3)
                    );
                    for decision in [stream, gateway] {
                        if let Decision::Schedule { at, .. } = decision {
                            assert_eq!(at.due, HubTime(2000 + [20_000, 60_000][ordinal as usize]));
                            assert_eq!(at.jitter_ms, 0);
                        }
                    }
                    if let Decision::Schedule { at, .. } = capacity {
                        assert_eq!(
                            at.due,
                            HubTime(3000 + [20_000, 60_000, 120_000][ordinal as usize])
                        );
                        assert_eq!(at.deadline, Some(HubTime(901_000)));
                        assert_eq!(at.jitter_ms, 1000);
                    }
                }
            }
        }
    }
}

#[test]
fn gateway_never_after_acting_budget_and_restart_are_explicit() {
    let intent = intent();
    for ordinal in 0..=2 {
        let rows = history(&intent, ordinal);
        let rule = TurnGatewayTransient {
            history: History {
                intent: &intent,
                revisions: &rows,
            },
            facts: facts(),
            now: HubTime(2000),
        };
        assert_eq!(rule.decide(Retry::Never), Decision::Stop(Refusal::Never));
        let restarted = rows.clone();
        assert_eq!(
            rule.decide(Retry::Now),
            TurnGatewayTransient {
                history: History {
                    intent: &intent,
                    revisions: &restarted
                },
                ..rule
            }
            .decide(Retry::Now)
        );
        if ordinal == 2 {
            assert_eq!(rule.decide(Retry::Now), Decision::Stop(Refusal::Budget));
        } else {
            assert!(matches!(rule.decide(Retry::Now), Decision::Schedule { .. }));
        }
        assert_eq!(
            TurnGatewayTransient {
                facts: TurnFacts {
                    acted: true,
                    ..facts()
                },
                ..rule
            }
            .decide(Retry::Now),
            Decision::Stop(Refusal::Acted)
        );
    }
}

#[test]
fn capacity_now_and_unanchored_hints_keep_the_existing_backoff_and_jitter() {
    let intent = intent();
    for (ordinal, delay) in [(0, 20_000), (1, 60_000), (2, 120_000)] {
        let rows = history(&intent, ordinal);
        let base = capacity(&intent, &rows);
        assert_eq!(
            at(base.decide(Retry::Now)).due,
            HubTime(2000 + delay + 1000)
        );
        assert_eq!(
            base.decide(Retry::Now),
            base.decide(Retry::Later { after_ms: None })
        );
        for source in [HintSource::CommandReceipt, HintSource::CompletionCopy] {
            let rule = TurnCapacity { source, ..base };
            assert_eq!(
                rule.decide(Retry::Later {
                    after_ms: Some(u64::MAX)
                }),
                base.decide(Retry::Now)
            );
        }
    }
}
