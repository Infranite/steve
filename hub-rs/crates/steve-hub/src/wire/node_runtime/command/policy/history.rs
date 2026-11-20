use super::{HubTime, Refusal};
use steve_node_api::{Intent, IntentKey};
use steve_types::{DecisionId, Retry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    FinishedOk,
    FinishedFailed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settled {
    pub terminal: Terminal,
    pub retry: Retry,
    /// Hub settlement time, not Node event time.
    pub at: HubTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub key: IntentKey,
    pub settled: Option<Settled>,
}

#[derive(Debug, Clone, Copy)]
pub struct History<'a> {
    pub intent: &'a Intent,
    /// Ordered complete history for this intent, starting at revision zero.
    pub revisions: &'a [Revision],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    TurnCapacity {
        first_capacity_failure: Option<HubTime>,
    },
    TurnInterrupted,
    TurnGatewayTransient,
    ConfigStale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionWhy {
    Policy { rule: Rule, now: HubTime },
    UserAction { decision_id: DecisionId },
}

impl<'a> History<'a> {
    pub fn last(self) -> Result<Option<&'a Revision>, Refusal> {
        for (i, row) in self.revisions.iter().enumerate() {
            if row.key.intent != *self.intent
                || usize::try_from(row.key.revision) != Ok(i)
                || (i + 1 < self.revisions.len() && row.settled.is_none())
            {
                return Err(Refusal::InvalidHistory);
            }
        }
        Ok(self.revisions.last())
    }

    /// Validate failure and budget only; the selected rule owns retry eligibility.
    pub fn failed(self, max_retries: u32, now: HubTime) -> Result<Settled, Refusal> {
        let last = self.last()?.ok_or(Refusal::InvalidHistory)?;
        let end = last.settled.ok_or(Refusal::InvalidHistory)?;
        if end.terminal == Terminal::FinishedOk {
            return Err(Refusal::NotFailed);
        }
        if last.key.revision >= max_retries {
            return Err(Refusal::Budget);
        }
        if self
            .revisions
            .iter()
            .any(|r| r.settled.is_some_and(|s| s.at > now))
        {
            return Err(Refusal::InvalidTime);
        }
        Ok(end)
    }
}

/// Only this function advances the ordinal; it rechecks a policy's revision/time budget.
/// Business eligibility is still checked by the rule before this function is called.
pub fn next_revision(history: History<'_>, why: RevisionWhy) -> Result<IntentKey, Refusal> {
    if let RevisionWhy::Policy { rule, now } = why {
        let limit = match (rule, history.intent) {
            (Rule::TurnCapacity { .. }, Intent::StartTurn(_)) => 3,
            (Rule::TurnInterrupted | Rule::TurnGatewayTransient, Intent::StartTurn(_)) => 2,
            (Rule::ConfigStale, Intent::Configure(_)) => 2,
            _ => return Err(Refusal::WrongIntent),
        };
        if !history.revisions.is_empty() {
            history.failed(limit, now)?;
            if let Rule::TurnCapacity {
                first_capacity_failure,
            } = rule
            {
                super::time::capacity_deadline(now, first_capacity_failure)?;
            }
        }
    }
    let revision = match history.last()? {
        None => 0,
        Some(last) => {
            let end = last.settled.ok_or(Refusal::InvalidHistory)?;
            // Service/stale-config rules ignore Retry; ordinary gateway failures still obey it.
            if end.retry == Retry::Never
                && !matches!(
                    why,
                    RevisionWhy::UserAction { .. }
                        | RevisionWhy::Policy {
                            rule: Rule::TurnCapacity { .. }
                                | Rule::TurnInterrupted
                                | Rule::ConfigStale,
                            ..
                        }
                )
            {
                return Err(Refusal::Never);
            }
            last.key.revision.checked_add(1).ok_or(Refusal::Budget)?
        }
    };
    if let RevisionWhy::UserAction { ref decision_id } = why {
        if decision_id.as_str().is_empty() {
            return Err(Refusal::InvalidHistory);
        }
    }
    Ok(IntentKey {
        intent: history.intent.clone(),
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_revision_entry_point_rechecks_policy_budgets_and_intent_ownership() {
        let intent = Intent::StartTurn(steve_node_api::TurnId::new("t").unwrap());
        let mut rows: Vec<_> = (0..=3)
            .map(|revision| Revision {
                key: IntentKey {
                    intent: intent.clone(),
                    revision,
                },
                settled: Some(Settled {
                    terminal: Terminal::FinishedFailed,
                    retry: Retry::Now,
                    at: HubTime(1000 + i64::from(revision)),
                }),
            })
            .collect();
        let policy = |rule, now| RevisionWhy::Policy {
            rule,
            now: HubTime(now),
        };
        assert_eq!(
            next_revision(
                History {
                    intent: &intent,
                    revisions: &rows
                },
                policy(
                    Rule::TurnCapacity {
                        first_capacity_failure: Some(HubTime(1000))
                    },
                    2000
                )
            ),
            Err(Refusal::Budget)
        );
        rows.pop();
        assert_eq!(
            next_revision(
                History {
                    intent: &intent,
                    revisions: &rows
                },
                policy(
                    Rule::TurnCapacity {
                        first_capacity_failure: Some(HubTime(1000))
                    },
                    901_000
                )
            ),
            Err(Refusal::Deadline)
        );
        assert_eq!(
            next_revision(
                History {
                    intent: &intent,
                    revisions: &rows
                },
                policy(Rule::ConfigStale, 2000)
            ),
            Err(Refusal::WrongIntent)
        );
        assert_eq!(
            next_revision(
                History {
                    intent: &intent,
                    revisions: &rows
                },
                policy(
                    Rule::TurnCapacity {
                        first_capacity_failure: Some(HubTime(1000))
                    },
                    900_999
                )
            )
            .unwrap()
            .revision,
            3
        );
        let duplicate = [rows[0].clone(), rows[0].clone()];
        assert_eq!(
            History {
                intent: &intent,
                revisions: &duplicate
            }
            .last(),
            Err(Refusal::InvalidHistory)
        );
    }

    #[test]
    fn a_gateway_terminal_never_requires_an_explicit_user_decision() {
        let intent = Intent::StartTurn(steve_node_api::TurnId::new("t").unwrap());
        let mut rows = vec![];
        assert_eq!(
            next_revision(
                History {
                    intent: &intent,
                    revisions: &rows
                },
                RevisionWhy::Policy {
                    rule: Rule::TurnGatewayTransient,
                    now: HubTime(1)
                }
            )
            .unwrap()
            .revision,
            0
        );
        rows.push(Revision {
            key: IntentKey {
                intent: intent.clone(),
                revision: 0,
            },
            settled: None,
        });
        assert_eq!(
            next_revision(
                History {
                    intent: &intent,
                    revisions: &rows
                },
                RevisionWhy::Policy {
                    rule: Rule::TurnGatewayTransient,
                    now: HubTime(1)
                }
            ),
            Err(Refusal::InvalidHistory)
        );
        rows[0].settled = Some(Settled {
            terminal: Terminal::Rejected,
            retry: Retry::Never,
            at: HubTime(1),
        });
        assert_eq!(
            next_revision(
                History {
                    intent: &intent,
                    revisions: &rows
                },
                RevisionWhy::Policy {
                    rule: Rule::TurnGatewayTransient,
                    now: HubTime(1)
                }
            ),
            Err(Refusal::Never)
        );
        let key = next_revision(
            History {
                intent: &intent,
                revisions: &rows,
            },
            RevisionWhy::UserAction {
                decision_id: "d".into(),
            },
        )
        .unwrap();
        assert_eq!(key.revision, 1);
        assert_eq!(key.intent, intent);
    }

    #[test]
    fn service_and_stale_config_never_histories_can_advance_within_the_same_budgets() {
        let turn = Intent::StartTurn(steve_node_api::TurnId::new("t").unwrap());
        let config = Intent::Configure(steve_node_api::SessionId::new("s").unwrap());
        for (intent, rule, limit) in [
            (
                &turn,
                Rule::TurnCapacity {
                    first_capacity_failure: Some(HubTime(1)),
                },
                3,
            ),
            (&turn, Rule::TurnInterrupted, 2),
            (&config, Rule::ConfigStale, 2),
        ] {
            for terminal in [
                Terminal::FinishedFailed,
                Terminal::Rejected,
                Terminal::FinishedOk,
            ] {
                let mut rows = vec![];
                for revision in 0..=limit {
                    rows.push(Revision {
                        key: IntentKey {
                            intent: intent.clone(),
                            revision,
                        },
                        settled: Some(Settled {
                            terminal,
                            retry: Retry::Never,
                            at: HubTime(1),
                        }),
                    });
                    let h = History {
                        intent,
                        revisions: &rows,
                    };
                    let advance = |now| {
                        next_revision(
                            h,
                            RevisionWhy::Policy {
                                rule,
                                now: HubTime(now),
                            },
                        )
                    };
                    let expected = if terminal == Terminal::FinishedOk {
                        Err(Refusal::NotFailed)
                    } else if revision == limit {
                        Err(Refusal::Budget)
                    } else {
                        Ok(IntentKey {
                            intent: intent.clone(),
                            revision: revision + 1,
                        })
                    };
                    assert_eq!(advance(1), expected);
                    if terminal != Terminal::FinishedOk && revision < limit {
                        assert_eq!(advance(0), Err(Refusal::InvalidTime));
                        if matches!(rule, Rule::TurnCapacity { .. }) {
                            assert_eq!(advance(900_001), Err(Refusal::Deadline));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mixed_duplicate_and_gapped_histories_are_not_budgets() {
        let intent = Intent::StartTurn(steve_node_api::TurnId::new("t").unwrap());
        for revision in [1, u32::MAX] {
            let rows = [Revision {
                key: IntentKey {
                    intent: intent.clone(),
                    revision,
                },
                settled: None,
            }];
            assert_eq!(
                History {
                    intent: &intent,
                    revisions: &rows
                }
                .last(),
                Err(Refusal::InvalidHistory)
            );
        }
        let other = Intent::Open(steve_node_api::SessionId::new("s").unwrap());
        let rows = [Revision {
            key: IntentKey {
                intent: other,
                revision: 0,
            },
            settled: None,
        }];
        assert_eq!(
            History {
                intent: &intent,
                revisions: &rows
            }
            .last(),
            Err(Refusal::InvalidHistory)
        );
    }
}
