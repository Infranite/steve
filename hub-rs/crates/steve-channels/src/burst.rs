//! In-process burst coalescing gate (#268): when the same sender posts multiple rapid messages
//! in a space within [`crate::presence::BURST_SETTLE_MS`], earlier arrivals yield to the latest
//! message so the composer runs once on the combined burst instead of drafting N redundant replies.
//!
//! When `settle` is zero (default in synchronous unit tests unless configured), every message
//! proceeds immediately without delaying.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

/// What waiting on the burst gate decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstDecision {
    /// Proceed to compose (this message is the tail of the current burst).
    Proceed,
    /// A newer message from the same sender in the same space arrived within the settle window;
    /// let that message compose the combined burst instead.
    Coalesced,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BurstKey {
    space_id: String,
    sender: String,
}

struct Slot {
    seq: u64,
    notify: Arc<Notify>,
}

/// Coordinates the settle window per `(space_id, sender)` without holding locks across awaits.
#[derive(Clone)]
pub struct BurstGate {
    settle: Duration,
    slots: Arc<Mutex<HashMap<BurstKey, Slot>>>,
}

impl Default for BurstGate {
    fn default() -> Self {
        Self::new(Duration::ZERO)
    }
}

impl BurstGate {
    pub fn new(settle: Duration) -> Self {
        Self {
            settle,
            slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn settle_window(&self) -> Duration {
        self.settle
    }

    /// Waits for `settle` unless superseded by a newer message from the same sender in `space_id`.
    pub async fn wait(&self, space_id: &str, sender: &str) -> BurstDecision {
        if self.settle.is_zero() || sender.is_empty() {
            return BurstDecision::Proceed;
        }
        let key = BurstKey {
            space_id: space_id.to_string(),
            sender: sender.to_string(),
        };
        let (my_seq, notify) = {
            let mut map = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            let entry = map.entry(key.clone()).or_insert_with(|| Slot {
                seq: 0,
                notify: Arc::new(Notify::new()),
            });
            entry.seq = entry.seq.wrapping_add(1);
            // Wake any earlier arrival waiting on the same burst key immediately.
            entry.notify.notify_waiters();
            (entry.seq, entry.notify.clone())
        };

        let notified = notify.notified();
        tokio::select! {
            _ = notified => {
                // Check whether a newer arrival actually bumped our seq.
                let superseded = {
                    let map = self.slots.lock().unwrap_or_else(|e| e.into_inner());
                    map.get(&key).is_some_and(|slot| slot.seq != my_seq)
                };
                if superseded {
                    metrics::counter!("steve_channel_burst_coalesced_total").increment(1);
                    return BurstDecision::Coalesced;
                }
            }
            _ = tokio::time::sleep(self.settle) => {}
        }

        let mut map = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(&key) {
            Some(slot) if slot.seq != my_seq => {
                metrics::counter!("steve_channel_burst_coalesced_total").increment(1);
                BurstDecision::Coalesced
            }
            Some(_) => {
                map.remove(&key);
                BurstDecision::Proceed
            }
            None => BurstDecision::Proceed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rapid_messages_from_same_sender_coalesce_to_latest() {
        let gate = BurstGate::new(Duration::from_millis(60));
        let g1 = gate.clone();
        let g2 = gate.clone();
        let h1 = tokio::spawn(async move { g1.wait("sp_1", "u_a").await });
        tokio::time::sleep(Duration::from_millis(15)).await;
        let h2 = tokio::spawn(async move { g2.wait("sp_1", "u_a").await });
        assert_eq!(h1.await.unwrap(), BurstDecision::Coalesced);
        assert_eq!(h2.await.unwrap(), BurstDecision::Proceed);
    }

    #[tokio::test]
    async fn different_senders_do_not_coalesce() {
        let gate = BurstGate::new(Duration::from_millis(30));
        let (a, b) = tokio::join!(gate.wait("sp_1", "u_a"), gate.wait("sp_1", "u_b"));
        assert_eq!(a, BurstDecision::Proceed);
        assert_eq!(b, BurstDecision::Proceed);
    }
}
