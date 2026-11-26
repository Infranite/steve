//! Facts that keep the stall watcher from treating an idle turn as a failure.

use steve_store::SessionStop;
use steve_types::orchestration::{
    STOP_REASON_STALL_NO_PROGRESS, STOP_REASON_STALL_REPEATED_FAILURE, StallReason,
};

use crate::cycle::CLOSING;
use crate::lifecycle::stop_card;
use crate::repo::Item;
use crate::util::js_head;
use crate::{Result, Tx};

/// Strongly-typed cause of why an idle session stopped, derived from `session_stops`
/// (durably persisted across restarts, never inferred from the session's last system line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopCause {
    UserCancelled,
    UserStopPending,
    TurnStall(StallReason),
    Error(String),
    EndedWithoutDelivery,
    Idle,
}

impl StopCause {
    pub(crate) fn classify(stop: Option<&SessionStop>) -> Self {
        let Some(s) = stop else {
            return Self::Idle;
        };
        if yours(s) {
            return Self::UserCancelled;
        }
        if s.stop_asked.is_some() {
            return Self::UserStopPending;
        }
        if let Some(reason) = stall_reason(s) {
            return Self::TurnStall(reason);
        }
        match (&s.stop_reason, &s.error) {
            (Some(_), Some(e)) if !e.trim().is_empty() => Self::Error(js_head(e, 160).into()),
            (Some(_), _) => Self::EndedWithoutDelivery,
            (None, _) => Self::Idle,
        }
    }

    /// `true` only when the user stopped (or asked to stop) the turn.
    pub(crate) fn is_yours(&self) -> bool {
        matches!(self, Self::UserCancelled | Self::UserStopPending)
    }

    /// `true` when Steve must not auto-continue the idle session (either the user stopped it or
    /// the Hub's stall detector already stopped and held it for the user).
    pub(crate) fn skips_takeover(&self) -> bool {
        matches!(
            self,
            Self::UserCancelled | Self::UserStopPending | Self::TurnStall(_)
        )
    }

    /// Stable machine-readable reason code for diagnostics and UI.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::UserCancelled => "user_cancelled",
            Self::UserStopPending => "user_stop_pending",
            Self::TurnStall(StallReason::RepeatedFailure) => STOP_REASON_STALL_REPEATED_FAILURE,
            Self::TurnStall(StallReason::NoProgress) => STOP_REASON_STALL_NO_PROGRESS,
            Self::Error(_) => "error",
            Self::EndedWithoutDelivery => "ended_without_delivery",
            Self::Idle => "idle",
        }
    }

    /// Localized human-readable description in the current locale.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::UserCancelled => steve_i18n::t!("core.stop.user_cancelled"),
            Self::UserStopPending => steve_i18n::t!("core.stop.user_stop_pending"),
            Self::TurnStall(StallReason::RepeatedFailure) => {
                steve_i18n::t!("core.stop.turn_stall_repeated_failure")
            }
            Self::TurnStall(StallReason::NoProgress) => {
                steve_i18n::t!("core.stop.turn_stall_no_progress")
            }
            Self::Error(error) => steve_i18n::t!("core.stop.error", error = error),
            Self::EndedWithoutDelivery => steve_i18n::t!("core.stop.ended_without_delivery"),
            Self::Idle => steve_i18n::t!("core.stop.idle"),
        }
    }
}

/// The stall reason when the Hub's stall detector cancelled the session's last turn.
pub(crate) fn stall_reason(s: &SessionStop) -> Option<StallReason> {
    s.stop_reason
        .as_deref()
        .and_then(StallReason::from_stop_reason)
}

/// The last turn was stopped by you: your Stop reached it, or it ended with a user cancel
/// (`"cancelled"`). Stall-detector cancels carry their own typed stop reasons and never match here.
pub(crate) fn yours(s: &SessionStop) -> bool {
    s.yours || s.stop_reason.as_deref() == Some("cancelled")
}

/// Your Stop holds the session: its last turn was stopped by you ([`yours`]), or you asked for a
/// Stop whose turn end has not come (the backend could not confirm it, or that turn's end is still
/// on its way). Either way going on is yours, never Steve's or YOLO's.
pub(crate) fn held(s: &SessionStop) -> bool {
    s.stop_reason.is_some() && yours(s) || s.stop_asked.is_some()
}

impl Tx<'_> {
    /// A live turn, queued input, an explicit hold or an open decision can all explain an idle
    /// session. When retrying a stop, ignore the card that the retry would answer.
    pub(crate) fn turn_has_wait(&self, item: &Item, include_stop_cards: bool) -> Result<bool> {
        if item.is_closed()
            || item.is_paused()
            || item.phase == CLOSING
            || self.dependency_waiting(&item.id)?
        {
            return Ok(true);
        }
        if let Some(s) = self.active_session(&item.id)?
            && (self.busy(&s.id)
                || self.rt().uncertain(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
                || !self.db().queued(&s.id)?.is_empty())
        {
            return Ok(true);
        }
        let cards = self.db().decisions_of_item(&item.id)?;
        let open = |d: &&steve_store::Decision| d.status == steve_types::DecisionStatus::Open;
        if cards.iter().filter(open).any(|d| !stop_card(d)) || self.quota_waiting(&item.id) {
            return Ok(true);
        }
        Ok(include_stop_cards
            && cards
                .iter()
                .any(|d| d.status == steve_types::DecisionStatus::Open))
    }

    /// Cards the work has moved past, on every pass of the stall watch.
    pub(crate) fn expire_obsolete_turn_cards(&mut self) -> Result<usize> {
        self.receipts_tidy()
    }
}
