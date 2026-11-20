use super::time::{HintSource, schedule};
use super::{Action, Decision, History, HubTime, Refusal, TurnFacts, scheduled};
use steve_node_api::Intent;
use steve_types::Retry;

pub const CAPACITY_WINDOW_MS: i64 = 15 * 60_000;
const CAPACITY_DELAYS_MS: [u64; 3] = [20_000, 60_000, 120_000];
const INTERRUPTED_DELAYS_MS: [u64; 2] = [20_000, 60_000];

#[derive(Debug, Clone, Copy)]
pub struct TurnCapacity<'a> {
    pub history: History<'a>,
    pub facts: TurnFacts,
    pub now: HubTime,
    pub jitter_ms: u64,
    pub source: HintSource,
}

impl TurnCapacity<'_> {
    pub fn decide(self, retry: Retry) -> Decision {
        scheduled(Action::NextRevision, self.plan(retry))
    }

    fn plan(self, retry: Retry) -> Result<super::Schedule, Refusal> {
        self.facts.allowed()?;
        if retry == Retry::Never {
            return Err(Refusal::Never);
        }
        if !matches!(self.history.intent, Intent::StartTurn(_)) {
            return Err(Refusal::WrongIntent);
        }
        self.history.failed(3, self.now)?;
        let deadline = HubTime(
            self.history
                .first_failure()?
                .0
                .checked_add(CAPACITY_WINDOW_MS)
                .ok_or(Refusal::InvalidTime)?,
        );
        let ordinal = self
            .history
            .last()?
            .ok_or(Refusal::InvalidHistory)?
            .key
            .revision as usize;
        match retry {
            Retry::Now => schedule(self.now, self.source, None, 0, 0, Some(deadline)),
            Retry::Later { after_ms } => schedule(
                self.now,
                self.source,
                after_ms,
                CAPACITY_DELAYS_MS[ordinal],
                self.jitter_ms,
                Some(deadline),
            ),
            Retry::Never => Err(Refusal::Never),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TurnInterrupted<'a> {
    pub history: History<'a>,
    pub facts: TurnFacts,
    pub now: HubTime,
    pub jitter_ms: u64,
    pub source: HintSource,
}

impl TurnInterrupted<'_> {
    pub fn decide(self, retry: Retry) -> Decision {
        scheduled(Action::NextRevision, self.plan(retry))
    }

    fn plan(self, retry: Retry) -> Result<super::Schedule, Refusal> {
        self.facts.allowed()?;
        if retry == Retry::Never {
            return Err(Refusal::Never);
        }
        if self.facts.acted {
            return Err(Refusal::MustCheckAfterActing);
        }
        if !matches!(self.history.intent, Intent::StartTurn(_)) {
            return Err(Refusal::WrongIntent);
        }
        self.history.failed(2, self.now)?;
        let ordinal = self
            .history
            .last()?
            .ok_or(Refusal::InvalidHistory)?
            .key
            .revision as usize;
        match retry {
            Retry::Now => schedule(self.now, self.source, None, 0, 0, None),
            Retry::Later { after_ms } => schedule(
                self.now,
                self.source,
                after_ms,
                INTERRUPTED_DELAYS_MS[ordinal],
                self.jitter_ms,
                None,
            ),
            Retry::Never => Err(Refusal::Never),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TurnInterruptedAfterActing {
    pub facts: TurnFacts,
}

impl TurnInterruptedAfterActing {
    pub fn decide(self, _retry: Retry) -> Decision {
        if let Err(why) = self.facts.allowed() {
            return Decision::Stop(why);
        }
        if !self.facts.acted {
            return Decision::Stop(Refusal::NotActed);
        }
        // Checking existing effects is a user decision, never an automatic retry (even for Now).
        Decision::CheckBeforeContinuing
    }
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
