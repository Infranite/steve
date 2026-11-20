use super::Refusal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HubTime(pub i64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeTime(pub i64);

/// The existing capacity policy samples 1-5 seconds. Invalid samples are rejected, not clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jitter(u16);

impl Jitter {
    pub fn new(ms: u64) -> Result<Self, Refusal> {
        if ms < 1000 || ms > 5000 {
            return Err(Refusal::InvalidJitter);
        }
        Ok(Self(ms as u16))
    }

    pub fn ms(self) -> u64 {
        u64::from(self.0)
    }
}

/// Supplied by the consumer that knows which failures were capacity failures, for this turn.
/// An earlier interruption must not start the capacity window. None means its first failure now.
pub fn capacity_deadline(
    now: HubTime,
    first_capacity_failure: Option<HubTime>,
) -> Result<HubTime, Refusal> {
    let first = first_capacity_failure.unwrap_or(now);
    if first > now {
        return Err(Refusal::InvalidTime);
    }
    let end = HubTime(
        first
            .0
            .checked_add(15 * 60_000)
            .ok_or(Refusal::InvalidTime)?,
    );
    if now >= end {
        return Err(Refusal::Deadline);
    }
    Ok(end)
}

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
    jitter: Option<Jitter>,
    deadline: Option<HubTime>,
) -> Result<Schedule, Refusal> {
    let candidate = match after_ms {
        Some(delay) => match source {
            // Zero includes an expired absolute hint; skew must not turn it into a future wait.
            HintSource::JournalEvent { .. } if delay == 0 => i128::from(now.0),
            HintSource::JournalEvent { at } => i128::from(at.0) + i128::from(delay),
            // Receipts/copies lack a time anchor: ignore their hint, retain the normal backoff.
            _ => i128::from(now.0) + i128::from(fallback_ms),
        },
        None => i128::from(now.0) + i128::from(fallback_ms),
    };
    // Reject unrepresentable advice rather than turning overflow into an immediate retry.
    i64::try_from(candidate).map_err(|_| Refusal::InvalidTime)?;
    let jitter_ms = jitter.map_or(0, Jitter::ms);
    let lower = i128::from(now.0) + i128::from(jitter_ms);
    let due = candidate.max(i128::from(now.0)) + i128::from(jitter_ms);
    let due = due.max(lower);
    // An exclusive ceiling cannot supply a send slot. Do not manufacture a plan at the
    // deadline (or squeeze it to deadline - 1); the rule already knows it must not send.
    if deadline.is_some_and(|end| now >= end || due >= i128::from(end.0)) {
        return Err(Refusal::Deadline);
    }
    Ok(Schedule {
        due: HubTime(i64::try_from(due).map_err(|_| Refusal::InvalidTime)?),
        deadline,
        jitter_ms,
    })
}
