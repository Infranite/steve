//! Typed policy facts only: reason dispatch, clocks, RNGs and ledger reads belong to the caller.
pub mod config;
pub mod history;
pub mod link;
#[cfg(test)]
mod r1_tests;
pub mod retry;
pub mod time;
pub mod turn;

pub use self::history::History;
pub use self::time::{HubTime, Schedule};
use steve_node_api::Outcome;

/// Why a pure policy declined an action, not a Node/harness stop classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Never,
    Budget,
    Deadline,
    Cancelled,
    NotFailed,
    Ineligible,
    ConfigurationPending,
    InvalidHistory,
    InvalidTime,
    InvalidJitter,
    WrongIntent,
    /// A non-service failure after the run acted: resending is not known to be safe.
    Acted,
    LinkClosed,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ResendSameOperation,
    NextRevision,
    ReadNodeView,
    Reconcile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Stop(Refusal),
    Schedule { action: Action, at: Schedule },
}

/// The three business gates are one fact group, independently of workflow/configuration gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gates {
    pub session_open: bool,
    /// True also for an unattached session: there is no ended item to resurrect.
    pub item_in_progress: bool,
    pub no_pending_decision: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnFacts {
    pub acted: bool,
    pub outcome: Outcome,
    pub cancelled: bool,
    pub gates: Gates,
    pub workflow_blocked: bool,
    pub configuration_pending: bool,
}

impl TurnFacts {
    pub fn execution_allowed(self) -> Result<(), Refusal> {
        if self.cancelled {
            return Err(Refusal::Cancelled);
        }
        if self.outcome != Outcome::Failed {
            return Err(Refusal::NotFailed);
        }
        if self.workflow_blocked {
            return Err(Refusal::Ineligible);
        }
        Ok(())
    }

    pub fn allowed(self) -> Result<(), Refusal> {
        self.execution_allowed()?;
        if !self.gates.session_open
            || !self.gates.item_in_progress
            || !self.gates.no_pending_decision
        {
            return Err(Refusal::Ineligible);
        }
        Ok(())
    }

    /// Only consulted once a retry has otherwise been permitted, including budget and deadline.
    pub fn configuration_allows(self) -> Result<(), Refusal> {
        if self.configuration_pending {
            return Err(Refusal::ConfigurationPending);
        }
        Ok(())
    }
}

pub fn scheduled(action: Action, at: Result<Schedule, Refusal>) -> Decision {
    match at {
        Ok(at) => Decision::Schedule { action, at },
        Err(why) => Decision::Stop(why),
    }
}
