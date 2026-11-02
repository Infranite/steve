//! Server-side state of flows in progress (§10 rule 6): OAuth `state` values with their PKCE verifier,
//! single use, 10 minutes, swept on every access; only states this Hub issued are accepted. The same
//! [`PendingStates`] serves the GitHub App manifest flow (github.md §1.1 “同 OAuth state 规则”).

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use zeroize::Zeroizing;

use super::oauth::random_token;
use super::provider::ProviderError;

/// How long a state is good for.
pub const STATE_TTL_MS: i64 = 10 * 60 * 1000;

/// Single-use, expiring states → `T`. In memory: a Hub restart drops flows in progress (the user
/// starts again), which is what we want for secrets like PKCE verifiers.
pub struct PendingStates<T> {
    map: Mutex<HashMap<String, (i64, T)>>,
}

impl<T> Default for PendingStates<T> {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> PendingStates<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (i64, T)>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stores `v` under a fresh random state (256 bits) and returns the state.
    pub fn insert(&self, now: i64, v: T) -> Result<String, ProviderError> {
        let state = random_token(32)?;
        let mut m = self.lock();
        m.retain(|_, (at, _)| now - *at <= STATE_TTL_MS);
        m.insert(state.clone(), (now, v));
        Ok(state)
    }

    /// Removes and returns the value if the state is known and not expired. A second call with the
    /// same state gets `None`, whatever happened to the first.
    pub fn take(&self, now: i64, state: &str) -> Option<T> {
        let mut m = self.lock();
        let hit = m.remove(state);
        m.retain(|_, (at, _)| now - *at <= STATE_TTL_MS);
        hit.filter(|(at, _)| now - *at <= STATE_TTL_MS && *at <= now)
            .map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An authorization-code flow in progress. No `Debug` (the verifier).
pub struct Flow {
    pub ws: String,
    pub user: String,
    pub adapter: String,
    pub verifier: Zeroizing<String>,
    /// Exactly as sent to the authorize endpoint (the token request must repeat it).
    pub redirect_uri: String,
    pub caps: Option<Vec<String>>,
    pub config: Value,
    /// `POST /connections/:id/reauth`: the connection being re-authorized.
    pub reauth_of: Option<String>,
}

/// Where a device flow stands, for `GET /connections/device/:id`.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceStatus {
    Pending,
    Ok { account_id: String },
    Failed(String),
}

/// A device flow in progress (the device code stays in its polling task).
#[derive(Debug, Clone)]
pub struct Device {
    pub ws: String,
    pub user: String,
    pub status: DeviceStatus,
    pub created: i64,
    pub expires_at: i64,
}

/// Device flows by id (`dev_…`), kept 10 minutes past their expiry so the page can read the end.
#[derive(Default)]
pub struct Devices {
    map: Mutex<HashMap<String, Device>>,
}

impl Devices {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Device>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn insert(&self, now: i64, id: &str, d: Device) {
        let mut m = self.lock();
        m.retain(|_, d| now <= d.expires_at + STATE_TTL_MS);
        m.insert(id.into(), d);
    }
    pub fn get(&self, id: &str) -> Option<Device> {
        self.lock().get(id).cloned()
    }
    pub fn set(&self, id: &str, s: DeviceStatus) {
        if let Some(d) = self.lock().get_mut(id) {
            d.status = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_single_use_and_expire() {
        let p: PendingStates<u32> = PendingStates::default();
        let a = p.insert(1_000, 1).expect("state");
        let b = p.insert(1_000, 2).expect("state");
        assert_ne!(a, b);
        assert!(a.len() >= 43);
        assert_eq!(p.take(2_000, &a), Some(1));
        assert_eq!(p.take(2_000, &a), None, "single use");
        assert_eq!(p.take(1_000 + STATE_TTL_MS + 1, &b), None, "expired");
        assert!(p.is_empty(), "swept");
        assert_eq!(p.take(0, "made-up"), None);
    }
}
