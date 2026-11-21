//! Same-operation resends wait as today; Retry::Now is not permission to skip the delay.
use super::time::{HintSource, schedule};
use super::{Action, Decision, HubTime, Refusal, scheduled};
use steve_types::Retry;

/// The existing sender distinguishes capacity refusals from its ordinary 1s resend delay.
#[derive(Debug, Clone, Copy)]
pub enum ResendBackoff {
    Capacity { previous_refusals: u32 },
    Ordinary,
}

#[derive(Debug, Clone, Copy)]
pub struct SameOperation {
    pub now: HubTime,
    pub accept_before: HubTime,
    pub backoff: ResendBackoff,
    pub source: HintSource,
}

impl SameOperation {
    pub fn decide(self, retry: Retry) -> Decision {
        if retry == Retry::Never {
            return Decision::Stop(Refusal::Never);
        }
        let hint = match retry {
            Retry::Later { after_ms } => after_ms,
            _ => None,
        };
        let delay = match self.backoff {
            ResendBackoff::Capacity { previous_refusals } => 2000u64
                .saturating_mul(2u64.saturating_pow(previous_refusals))
                .min(60_000),
            ResendBackoff::Ordinary => 1000,
        };
        scheduled(
            Action::ResendSameOperation,
            schedule(
                self.now,
                self.source,
                hint,
                delay,
                None,
                Some(self.accept_before),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::time::{Due, NodeTime};
    use super::*;

    #[test]
    fn now_waits_like_later_and_capacity_refusal_backoff_starts_at_two_seconds() {
        for (later_retries, delay) in [(0, 2000), (1, 4000), (5, 60_000), (u32::MAX, 60_000)] {
            let rule = SameOperation {
                now: HubTime(100),
                accept_before: HubTime(100_000),
                backoff: ResendBackoff::Capacity {
                    previous_refusals: later_retries,
                },
                source: HintSource::JournalEvent { at: NodeTime(90) },
            };
            assert_eq!(rule.decide(Retry::Never), Decision::Stop(Refusal::Never));
            let Decision::Schedule { at, action } = rule.decide(Retry::Now) else {
                panic!("expected retry")
            };
            assert_eq!(action, Action::ResendSameOperation);
            assert_eq!(at.due, HubTime(100 + delay));
            assert_eq!(
                rule.decide(Retry::Now),
                rule.decide(Retry::Later { after_ms: None })
            );
            assert_eq!(
                rule.decide(Retry::Later {
                    after_ms: Some(100)
                }),
                Decision::Schedule {
                    action,
                    at: super::super::Schedule {
                        due: HubTime(190),
                        deadline: Some(HubTime(100_000)),
                        jitter_ms: 0,
                    }
                }
            );
            for source in [HintSource::CommandReceipt, HintSource::CompletionCopy] {
                let unanchored = SameOperation { source, ..rule };
                assert_eq!(
                    unanchored.decide(Retry::Later {
                        after_ms: Some(u64::MAX)
                    }),
                    rule.decide(Retry::Now)
                );
            }
        }
    }

    #[test]
    fn exponential_delay_caps_and_the_envelope_expires_without_a_final_send() {
        let rule = SameOperation {
            now: HubTime(0),
            accept_before: HubTime(10),
            backoff: ResendBackoff::Capacity {
                previous_refusals: u32::MAX,
            },
            source: HintSource::CommandReceipt,
        };
        assert_eq!(rule.decide(Retry::Now), Decision::Stop(Refusal::Deadline));
        assert_eq!(
            SameOperation {
                now: HubTime(10),
                ..rule
            }
            .decide(Retry::Now),
            Decision::Stop(Refusal::Deadline)
        );
        let Decision::Schedule { at, .. } = SameOperation {
            accept_before: HubTime(1_000_000),
            ..rule
        }
        .decide(Retry::Now) else {
            panic!("expected wait")
        };
        assert_eq!(at.due, HubTime(60_000));
        assert_eq!(at.status(HubTime(1_000_000)), Due::Expired);
    }

    #[test]
    fn ordinary_retry_advice_keeps_the_existing_one_second_sender_delay() {
        let rule = SameOperation {
            now: HubTime(100),
            accept_before: HubTime(10_000),
            backoff: ResendBackoff::Ordinary,
            source: HintSource::CommandReceipt,
        };
        for retry in [
            Retry::Now,
            Retry::Later { after_ms: None },
            Retry::Later {
                after_ms: Some(u64::MAX),
            },
        ] {
            let Decision::Schedule { at, .. } = rule.decide(retry) else {
                panic!("expected retry")
            };
            assert_eq!(at.due, HubTime(1100));
            assert_eq!(at.jitter_ms, 0);
        }
    }
}
