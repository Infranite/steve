//! Facts that keep the stall watcher from treating an idle turn as a failure.

use steve_store::SessionStop;

use crate::cycle::CLOSING;
use crate::lifecycle::stop_card;
use crate::repo::Item;
use crate::util::js_head;
use crate::{Result, Tx};

/// Why an idle session is idle, from how its last turn ended (kept in `session_stops`, so the same
/// after a restart); `true` when you stopped it yourself: it is yours to go on with, Steve does not
/// continue it. Never from the session's last system line.
pub(crate) fn stopped(stop: Option<&SessionStop>) -> (String, bool) {
    match stop {
        Some(s) if yours(s) => ("你中断了这一回合".into(), true),
        Some(s) if s.stop_asked.is_some() => {
            ("你要求停止这一回合，还没有确认它已停下".into(), true)
        }
        Some(SessionStop {
            error: Some(e),
            stop_reason: Some(_),
            ..
        }) if !e.trim().is_empty() => (format!("上一回合出错：{}", js_head(e, 160)), false),
        Some(SessionStop {
            stop_reason: Some(_),
            ..
        }) => ("上一回合结束后没有交付、提问，也没有后续工作".into(), false),
        _ => (
            "会话空闲：没有回合在运行、没有排队输入，也没有交付或提问".into(),
            false,
        ),
    }
}

/// The last turn was stopped by you: your Stop reached it, or it ended cancelled (the Hub's own
/// cancels, a pause or a steer, leave nothing idle to judge).
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
