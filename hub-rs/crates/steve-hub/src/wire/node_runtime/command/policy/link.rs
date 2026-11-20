//! The current coordinator waits five seconds after any reconciliation error.
//! The design's 200ms-30s exponential alternative is UNAPPROVED; W3.3s must not wire it.
use super::time::{HintSource, schedule};
use super::{Action, Decision, HubTime, Refusal, scheduled};
use steve_types::Retry;

#[derive(Debug, Clone, Copy)]
pub struct LinkReconcile {
    /// Coordinator/link lifetime, not whether the remote Node is currently online.
    pub link_open: bool,
    pub succeeded: bool,
    pub now: HubTime,
}

impl LinkReconcile {
    pub fn decide(self, _retry: Retry) -> Decision {
        if !self.link_open {
            return Decision::Stop(Refusal::LinkClosed);
        }
        if self.succeeded {
            return Decision::Stop(Refusal::Complete);
        }
        // No command retry classification or unanchored provider hint controls this loop.
        scheduled(
            Action::Reconcile,
            schedule(self.now, HintSource::CommandReceipt, None, 5000, None, None),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_keeps_five_seconds_for_every_retry_class_until_closed_or_done() {
        let rule = LinkReconcile {
            link_open: true,
            succeeded: false,
            now: HubTime(100),
        };
        for retry in [
            Retry::Never,
            Retry::Now,
            Retry::Later { after_ms: None },
            Retry::Later {
                after_ms: Some(u64::MAX),
            },
        ] {
            let expected = Decision::Schedule {
                action: Action::Reconcile,
                at: super::super::Schedule {
                    due: HubTime(5100),
                    deadline: None,
                    jitter_ms: 0,
                },
            };
            assert_eq!(rule.decide(retry), expected);
            let restarted = LinkReconcile {
                link_open: rule.link_open,
                succeeded: rule.succeeded,
                now: rule.now,
            };
            assert_eq!(rule.decide(retry), restarted.decide(retry));
            assert_eq!(
                LinkReconcile {
                    link_open: false,
                    ..rule
                }
                .decide(retry),
                Decision::Stop(Refusal::LinkClosed)
            );
            assert_eq!(
                LinkReconcile {
                    succeeded: true,
                    ..rule
                }
                .decide(retry),
                Decision::Stop(Refusal::Complete)
            );
        }
    }
}
