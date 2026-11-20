use super::time::{HintSource, Jitter, capacity_deadline, schedule};
use super::{Action, Decision, History, HubTime, Refusal, TurnFacts, scheduled};
use steve_node_api::Intent;
use steve_types::Retry;

const CAPACITY_DELAYS_MS: [u64; 3] = [20_000, 60_000, 120_000];
const INTERRUPTED_DELAYS_MS: [u64; 2] = [20_000, 60_000];

#[derive(Debug, Clone, Copy)]
pub struct TurnCapacity<'a> {
    pub history: History<'a>,
    pub facts: TurnFacts,
    pub now: HubTime,
    pub jitter: Jitter,
    pub source: HintSource,
    /// Hub's first capacity failure for this turn, not the first failure of any kind.
    pub first_capacity_failure: Option<HubTime>,
}

impl TurnCapacity<'_> {
    pub fn decide(self, retry: Retry) -> Decision {
        scheduled(Action::NextRevision, self.plan(retry))
    }

    fn plan(self, retry: Retry) -> Result<super::Schedule, Refusal> {
        // Service classification already selected this rule; Retry::Never is not a veto.
        self.facts.allowed()?;
        if !matches!(self.history.intent, Intent::StartTurn(_)) {
            return Err(Refusal::WrongIntent);
        }
        self.history.failed(3, self.now)?;
        let deadline = capacity_deadline(self.now, self.first_capacity_failure)?;
        let ordinal = self
            .history
            .last()?
            .ok_or(Refusal::InvalidHistory)?
            .key
            .revision as usize;
        let hint = match retry {
            Retry::Later { after_ms } => after_ms,
            _ => None,
        };
        let at = schedule(
            self.now,
            self.source,
            hint,
            CAPACITY_DELAYS_MS[ordinal],
            Some(self.jitter),
            Some(deadline),
        )?;
        self.facts.configuration_allows()?;
        Ok(at)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TurnInterrupted<'a> {
    pub history: History<'a>,
    pub facts: TurnFacts,
    pub now: HubTime,
}

impl TurnInterrupted<'_> {
    pub fn decide(self, retry: Retry) -> Decision {
        scheduled(
            Action::NextRevision,
            turn_retry(self.history, self.facts, self.now, retry, true),
        )
    }
}

/// Non-service transient failures retry only before tools ran, without the three service gates.
#[derive(Debug, Clone, Copy)]
pub struct TurnGatewayTransient<'a> {
    pub history: History<'a>,
    pub facts: TurnFacts,
    pub now: HubTime,
}

impl TurnGatewayTransient<'_> {
    pub fn decide(self, retry: Retry) -> Decision {
        scheduled(
            Action::NextRevision,
            turn_retry(self.history, self.facts, self.now, retry, false),
        )
    }
}

fn turn_retry(
    history: History<'_>,
    facts: TurnFacts,
    now: HubTime,
    retry: Retry,
    service: bool,
) -> Result<super::Schedule, Refusal> {
    if service {
        facts.allowed()?;
    } else {
        facts.execution_allowed()?;
        if facts.acted {
            return Err(Refusal::Acted);
        }
    }
    if !service && retry == Retry::Never {
        return Err(Refusal::Never);
    }
    if !matches!(history.intent, Intent::StartTurn(_)) {
        return Err(Refusal::WrongIntent);
    }
    let end = history.failed(2, now)?;
    if !service && end.retry == Retry::Never {
        return Err(Refusal::Never);
    }
    let ordinal = history.last()?.ok_or(Refusal::InvalidHistory)?.key.revision as usize;
    // Current apply.rs always uses 20/60 seconds, including after tools and for Retry::Now.
    let at = schedule(
        now,
        HintSource::CompletionCopy,
        None,
        INTERRUPTED_DELAYS_MS[ordinal],
        None,
        None,
    )?;
    facts.configuration_allows()?;
    Ok(at)
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
