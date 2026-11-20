use super::time::{HintSource, schedule};
use super::{Action, Decision, HubTime, Refusal, scheduled};
use steve_types::Retry;

#[derive(Debug, Clone, Copy)]
pub struct LinkReconcile {
    pub connected: bool,
    pub succeeded: bool,
    pub failures: u32,
    pub now: HubTime,
    pub jitter_ms: u64,
}

impl LinkReconcile {
    pub fn decide(self, retry: Retry) -> Decision {
        if !self.connected {
            return Decision::Stop(Refusal::LinkClosed);
        }
        if self.succeeded {
            return Decision::Stop(Refusal::Complete);
        }
        let delay = match retry {
            Retry::Never => return Decision::Stop(Refusal::Never),
            Retry::Now => 0,
            // Link failures have no Node event anchor; relative command advice is inapplicable.
            Retry::Later { after_ms: Some(_) } => {
                return Decision::Stop(Refusal::MissingEventAnchor);
            }
            Retry::Later { after_ms: None } => 200u64
                .saturating_mul(2u64.saturating_pow(self.failures))
                .min(30_000),
        };
        let delay = delay.saturating_add(self.jitter_ms).min(30_000);
        scheduled(
            Action::Reconcile,
            schedule(self.now, HintSource::CommandReceipt, None, delay, 0, None),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_backs_off_without_spending_a_command_revision() {
        for (failures, delay) in [
            (0, 200),
            (1, 400),
            (7, 25_600),
            (8, 30_000),
            (u32::MAX, 30_000),
        ] {
            let rule = LinkReconcile {
                connected: true,
                succeeded: false,
                failures,
                now: HubTime(100),
                jitter_ms: 0,
            };
            assert_eq!(rule.decide(Retry::Never), Decision::Stop(Refusal::Never));
            assert!(
                matches!(rule.decide(Retry::Now), Decision::Schedule { at, .. } if at.due == HubTime(100))
            );
            let expected = Decision::Schedule {
                action: Action::Reconcile,
                at: super::super::Schedule {
                    due: HubTime(100 + delay),
                    deadline: None,
                    jitter_ms: 0,
                },
            };
            assert_eq!(rule.decide(Retry::Later { after_ms: None }), expected);
            assert_eq!(
                rule.decide(Retry::Later { after_ms: None }),
                rule.decide(Retry::Later { after_ms: None })
            );
            assert_eq!(
                LinkReconcile {
                    connected: false,
                    ..rule
                }
                .decide(Retry::Now),
                Decision::Stop(Refusal::LinkClosed)
            );
            assert_eq!(
                LinkReconcile {
                    succeeded: true,
                    ..rule
                }
                .decide(Retry::Now),
                Decision::Stop(Refusal::Complete)
            );
        }
    }
}
