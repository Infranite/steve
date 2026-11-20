//! Same-operation transport retries are separate from policy-created intent revisions.
use super::time::{HintSource, schedule};
use super::{Action, Decision, HubTime, Refusal, scheduled};
use steve_types::Retry;

#[derive(Debug, Clone, Copy)]
pub struct SameOperation {
    pub now: HubTime,
    pub accept_before: HubTime,
    /// Already performed immediate resends of this same envelope.
    pub immediate_retries: u32,
    pub later_retries: u32,
    pub jitter_ms: u64,
    pub source: HintSource,
}

impl SameOperation {
    pub fn decide(self, retry: Retry) -> Decision {
        let (hint, delay, jitter) = match retry {
            Retry::Never => return Decision::Stop(Refusal::Never),
            Retry::Now if self.immediate_retries < 3 => (None, 0, 0),
            Retry::Now | Retry::Later { after_ms: None } => (
                None,
                1000u64
                    .saturating_mul(2u64.saturating_pow(self.later_retries))
                    .min(60_000),
                self.jitter_ms,
            ),
            Retry::Later { after_ms } => (after_ms, 0, self.jitter_ms),
        };
        scheduled(
            Action::ResendSameOperation,
            schedule(
                self.now,
                self.source,
                hint,
                delay,
                jitter,
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
    fn at_most_three_immediate_retries_then_wait_with_the_same_operation() {
        for count in 0..=4 {
            let rule = SameOperation {
                now: HubTime(100),
                accept_before: HubTime(100_000),
                immediate_retries: count,
                later_retries: 0,
                jitter_ms: 7,
                source: HintSource::JournalEvent { at: NodeTime(90) },
            };
            assert_eq!(rule.decide(Retry::Never), Decision::Stop(Refusal::Never));
            let Decision::Schedule { at, action } = rule.decide(Retry::Now) else {
                panic!("expected retry")
            };
            assert_eq!(action, Action::ResendSameOperation);
            assert_eq!(at.due, HubTime(if count < 3 { 100 } else { 1107 }));
            assert_eq!(
                rule.decide(Retry::Later {
                    after_ms: Some(100)
                }),
                Decision::Schedule {
                    action,
                    at: super::super::Schedule {
                        due: HubTime(197),
                        deadline: Some(HubTime(100_000)),
                        jitter_ms: 7,
                    }
                }
            );
            assert_eq!(rule.decide(Retry::Now), rule.decide(Retry::Now));
        }
    }

    #[test]
    fn exponential_delay_caps_and_the_envelope_expires_without_a_final_send() {
        let rule = SameOperation {
            now: HubTime(0),
            accept_before: HubTime(10),
            immediate_retries: 3,
            later_retries: u32::MAX,
            jitter_ms: 0,
            source: HintSource::CommandReceipt,
        };
        assert_eq!(rule.decide(Retry::Now), Decision::Stop(Refusal::Deadline));
        let Decision::Schedule { at, .. } = SameOperation {
            immediate_retries: 0,
            ..rule
        }
        .decide(Retry::Now) else {
            panic!("expected immediate send")
        };
        assert_eq!(at.due, HubTime(0));
        assert_eq!(at.status(HubTime(10)), Due::Expired);
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
    }
}
