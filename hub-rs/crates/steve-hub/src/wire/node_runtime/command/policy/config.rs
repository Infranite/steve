use super::time::{HintSource, schedule};
use super::{Action, Decision, History, HubTime, Refusal, scheduled};
use steve_node_api::Intent;
use steve_types::Retry;

#[derive(Debug, Clone, Copy)]
pub struct ConfigStale<'a> {
    pub history: History<'a>,
    pub now: HubTime,
    pub node_view_refreshed: bool,
    pub source: HintSource,
}

impl ConfigStale<'_> {
    pub fn decide(self, retry: Retry) -> Decision {
        let action = if self.node_view_refreshed {
            Action::NextRevision
        } else {
            Action::ReadNodeView
        };
        scheduled(action, self.plan(retry))
    }

    fn plan(self, retry: Retry) -> Result<super::Schedule, Refusal> {
        if retry == Retry::Never {
            return Err(Refusal::Never);
        }
        if !matches!(self.history.intent, Intent::Configure(_)) {
            return Err(Refusal::WrongIntent);
        }
        self.history.failed(2, self.now)?;
        let hint = match retry {
            Retry::Later { after_ms } => after_ms,
            _ => None,
        };
        // Stale configuration is refreshed from the Node view before a new operation is planned.
        schedule(self.now, self.source, hint, 0, None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::super::history::{Revision, Settled, Terminal};
    use super::*;
    use steve_node_api::{IntentKey, SessionId};

    #[test]
    fn stale_config_reads_a_fresh_view_before_advancing_and_obeys_a_two_retry_budget() {
        let intent = Intent::Configure(SessionId::new("s").unwrap());
        let mut rows = vec![];
        for revision in 0..=2 {
            rows.push(Revision {
                key: IntentKey {
                    intent: intent.clone(),
                    revision,
                },
                settled: Some(Settled {
                    terminal: Terminal::Rejected,
                    retry: Retry::Now,
                    at: HubTime(10),
                }),
            });
            let rule = ConfigStale {
                history: History {
                    intent: &intent,
                    revisions: &rows,
                },
                now: HubTime(10),
                node_view_refreshed: false,
                source: HintSource::CommandReceipt,
            };
            assert_eq!(rule.decide(Retry::Never), Decision::Stop(Refusal::Never));
            for retry in [Retry::Now, Retry::Later { after_ms: None }] {
                if revision == 2 {
                    assert_eq!(rule.decide(retry), Decision::Stop(Refusal::Budget));
                } else {
                    assert!(matches!(
                        rule.decide(retry),
                        Decision::Schedule {
                            action: Action::ReadNodeView,
                            ..
                        }
                    ));
                    assert!(matches!(
                        ConfigStale {
                            node_view_refreshed: true,
                            ..rule
                        }
                        .decide(retry),
                        Decision::Schedule {
                            action: Action::NextRevision,
                            ..
                        }
                    ));
                    let restarted = rows.clone();
                    assert_eq!(
                        rule.decide(retry),
                        ConfigStale {
                            history: History {
                                intent: &intent,
                                revisions: &restarted
                            },
                            ..rule
                        }
                        .decide(retry)
                    );
                }
            }
            assert_eq!(
                rule.decide(Retry::Later { after_ms: Some(1) }),
                rule.decide(Retry::Later { after_ms: None })
            );
        }
    }
}
