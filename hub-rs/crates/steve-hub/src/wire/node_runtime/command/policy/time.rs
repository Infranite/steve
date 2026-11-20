use super::Refusal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HubTime(pub i64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeTime(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintSource {
    JournalEvent { at: NodeTime },
    CommandReceipt,
    CompletionCopy,
}

/// Normalized numeric advice. Recognition is deliberately separate from journal append time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delay(pub u64);
impl Delay {
    pub fn delta_seconds(seconds: i64) -> Result<Self, Refusal> {
        let seconds = u64::try_from(seconds).map_err(|_| Refusal::InvalidTime)?;
        seconds
            .checked_mul(1000)
            .map(Self)
            .ok_or(Refusal::InvalidTime)
    }

    pub fn absolute(at: NodeTime, recognized: NodeTime) -> Self {
        Self((i128::from(at.0) - i128::from(recognized.0)).max(0) as u64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub due: HubTime,
    /// A Hub deadline is exclusive: reaching it expires the wait, never forces a send.
    pub deadline: Option<HubTime>,
    pub jitter_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Waiting,
    Ready,
    Expired,
}

impl Schedule {
    /// Reads an already chosen schedule. Replay/restart must pass this value unchanged.
    pub fn status(self, now: HubTime) -> Due {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Due::Expired;
        }
        if now >= self.due {
            Due::Ready
        } else {
            Due::Waiting
        }
    }
}

/// Convert only anchored advice, then clamp to the Hub's lower bound and exclusive ceiling.
pub fn schedule(
    now: HubTime,
    source: HintSource,
    after_ms: Option<u64>,
    fallback_ms: u64,
    jitter_ms: u64,
    deadline: Option<HubTime>,
) -> Result<Schedule, Refusal> {
    let candidate = match after_ms {
        Some(delay) => match source {
            // Zero includes an expired absolute hint; skew must not turn it into a future wait.
            HintSource::JournalEvent { .. } if delay == 0 => i128::from(now.0),
            HintSource::JournalEvent { at } => i128::from(at.0) + i128::from(delay),
            _ => return Err(Refusal::MissingEventAnchor),
        },
        None => i128::from(now.0) + i128::from(fallback_ms),
    };
    // Reject unrepresentable advice rather than turning overflow into an immediate retry.
    i64::try_from(candidate).map_err(|_| Refusal::InvalidTime)?;
    let lower = i128::from(now.0) + i128::from(jitter_ms);
    let due = candidate.max(i128::from(now.0)) + i128::from(jitter_ms);
    let due = match deadline {
        Some(end) if now >= end => return Err(Refusal::Deadline),
        Some(end) => due.max(lower).min(i128::from(end.0)),
        None => due.max(lower),
    };
    Ok(Schedule {
        due: HubTime(i64::try_from(due).map_err(|_| Refusal::InvalidTime)?),
        deadline,
        jitter_ms,
    })
}
