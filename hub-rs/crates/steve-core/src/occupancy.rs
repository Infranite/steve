//! What runs and what holds new turns, as the Hub sees it (N0b step B1).
//!
//! The Hub decides what runs, so whether a session is busy and whether new turns wait is the Hub's own
//! state, never a question to the place that executes. Every reader in the core asks here. While the
//! in-process runner still owns the queue (until B5), "busy" and the running count are taken from it;
//! the holds are the Hub's already.
//!
//! A hold has a reason. The release drain and an upgrade's maintenance hold new turns independently:
//! lifting one never lifts the other.

use crate::Tx;

/// Why new turns wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Hold {
    /// A source release is about to restart the Hub (`release.rs`).
    Release,
    /// An upgrade drain (`/internal/upgrade/drain`).
    Maintenance,
}

impl Tx<'_> {
    /// A turn of `sid` is in flight (starting, running, waiting for a permission or a retry).
    pub(crate) fn busy(&self, sid: &str) -> bool {
        self.rt().is_busy(sid)
    }

    /// Turns in flight.
    pub(crate) fn running_turns(&self) -> usize {
        self.rt().running_turns()
    }

    /// New turns wait (for any reason).
    pub(crate) fn holding(&self) -> bool {
        !self.st.holds.is_empty()
    }

    pub(crate) fn held_by(&self, h: Hold) -> bool {
        self.st.holds.contains(&h)
    }

    /// Hold new turns for `h`, or stop holding for it. The runner holds while any reason does.
    pub(crate) fn set_hold(&mut self, h: Hold, on: bool) {
        if on {
            self.st.holds.insert(h);
        } else {
            self.st.holds.remove(&h);
        }
        self.rt().hold(!self.st.holds.is_empty());
    }
}
