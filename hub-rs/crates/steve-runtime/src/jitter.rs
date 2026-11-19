//! Integer protocol delays, kept alongside the shared nanosecond Backoff policy.
use crate::policy::Jitter;
use rand::RngExt;
use std::time::Duration;

impl Jitter {
    fn units(self, max: u128, rng: &mut impl rand::Rng) -> u128 {
        match self {
            Self::None => max,
            Self::Full => rng.random_range(0..=max),
            Self::Equal => {
                let half = max / 2;
                half + rng.random_range(0..(max - half).max(1))
            }
        }
    }

    /// Protocols with integer millisecond delays retain their inclusive millisecond lattice.
    pub fn milliseconds(self, max: Duration, rng: &mut impl rand::Rng) -> Duration {
        Duration::from_millis(self.units(max.as_millis(), rng).min(u128::from(u64::MAX)) as u64)
    }
}
