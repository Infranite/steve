//! Bounded in-process LRU window caches for paginated read projections (`attention`, `feed`).
//!
//! Snapshots and page cursors live in memory rather than SQLite so read endpoints (`GET /me/attention`,
//! `GET /me/feed`) never acquire the database writer lock. Each cache is bounded in size, enforces
//! an explicit TTL over the caller-supplied clock (`now_ms`), and stamps every newly minted entry
//! with a monotonic version number.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::uid;

/// Maximum number of attention snapshot/cursor windows held in memory per `Store`.
pub const ATTENTION_WINDOW_CACHE_MAX: usize = 256;
/// Time-to-live of an attention snapshot/cursor window, in seconds (10 minutes).
pub const ATTENTION_WINDOW_TTL_SECS: u64 = 600;
const ATTENTION_WINDOW_TTL_MS: i64 = (ATTENTION_WINDOW_TTL_SECS as i64) * 1_000;
/// Reuse an existing attention capability only when at least 60 seconds remain before expiry.
const ATTENTION_WINDOW_REUSE_MARGIN_MS: i64 = 60_000;

/// Maximum number of feed snapshot/cursor windows held in memory per `Store`.
pub const FEED_WINDOW_CACHE_MAX: usize = 256;
/// Time-to-live of a feed snapshot/cursor window, in seconds (24 hours, matching the API contract).
pub const FEED_WINDOW_TTL_SECS: u64 = 86_400;
const FEED_WINDOW_TTL_MS: i64 = (FEED_WINDOW_TTL_SECS as i64) * 1_000;
/// Reuse an existing feed capability only when at least 1 hour remains before expiry.
const FEED_WINDOW_REUSE_MARGIN_MS: i64 = 3_600_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttentionWindow {
    pub ws: String,
    pub user: String,
    pub kind: String,
    pub revision: String,
    pub upper: i64,
    pub position: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeedWindow {
    pub ws: String,
    pub user: String,
    pub kind: Option<String>,
    pub source: Option<String>,
    pub unread: bool,
    pub upper: i64,
    pub before: Option<i64>,
}

struct Entry<V> {
    token: String,
    value: V,
    expires_ms: i64,
    version: u64,
}

/// Bounded in-process LRU cache with explicit logical-clock TTL and monotonic version counter.
pub(crate) struct WindowCache<V> {
    name: &'static str,
    max_entries: usize,
    ttl_ms: i64,
    reuse_margin_ms: i64,
    version: AtomicU64,
    entries: Mutex<VecDeque<Entry<V>>>,
}

impl<V: PartialEq + Clone> WindowCache<V> {
    pub(crate) fn new(
        name: &'static str,
        max_entries: usize,
        ttl_ms: i64,
        reuse_margin_ms: i64,
    ) -> Self {
        Self {
            name,
            max_entries: max_entries.max(1),
            ttl_ms,
            reuse_margin_ms,
            version: AtomicU64::new(0),
            entries: Mutex::new(VecDeque::with_capacity(max_entries.min(256))),
        }
    }

    /// Returns an existing reusable token for `value`, or mints a new one under LRU capacity.
    pub(crate) fn issue(&self, prefix: &str, value: V, now_ms: i64) -> String {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|e| e.expires_ms > now_ms);

        let min_reuse_expires = now_ms.saturating_add(self.reuse_margin_ms);
        if let Some(idx) = entries
            .iter()
            .position(|e| e.value == value && e.expires_ms > min_reuse_expires)
        {
            let entry = entries.remove(idx).expect("index in bounds");
            let token = entry.token.clone();
            entries.push_back(entry);
            metrics::counter!(
                "steve_window_cache_requests_total",
                "cache" => self.name,
                "result" => "reuse"
            )
            .increment(1);
            metrics::gauge!("steve_window_cache_entries", "cache" => self.name)
                .set(entries.len() as f64);
            return token;
        }

        if entries.len() >= self.max_entries {
            entries.pop_front();
            metrics::counter!("steve_window_cache_evictions_total", "cache" => self.name)
                .increment(1);
        }
        let token = format!("{}{}", uid(prefix), uid(""));
        let version = self.version.fetch_add(1, Ordering::Relaxed) + 1;
        entries.push_back(Entry {
            token: token.clone(),
            value,
            expires_ms: now_ms.saturating_add(self.ttl_ms),
            version,
        });
        metrics::counter!(
            "steve_window_cache_requests_total",
            "cache" => self.name,
            "result" => "issue"
        )
        .increment(1);
        metrics::gauge!("steve_window_cache_entries", "cache" => self.name)
            .set(entries.len() as f64);
        token
    }

    /// Resolves `token` if present and unexpired at `now_ms`, refreshing its LRU position.
    pub(crate) fn resolve(&self, token: &str, now_ms: i64) -> Option<(V, u64)> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Some(idx) = entries.iter().position(|e| e.token == token) else {
            metrics::counter!(
                "steve_window_cache_requests_total",
                "cache" => self.name,
                "result" => "miss"
            )
            .increment(1);
            return None;
        };
        if entries[idx].expires_ms <= now_ms {
            metrics::counter!(
                "steve_window_cache_requests_total",
                "cache" => self.name,
                "result" => "expired"
            )
            .increment(1);
            return None;
        }
        let entry = entries.remove(idx).expect("index in bounds");
        let out = (entry.value.clone(), entry.version);
        entries.push_back(entry);
        metrics::counter!(
            "steve_window_cache_requests_total",
            "cache" => self.name,
            "result" => "hit"
        )
        .increment(1);
        Some(out)
    }

    /// Current monotonic version number of issued window snapshots.
    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

pub(crate) struct WindowCaches {
    pub(crate) attention: WindowCache<AttentionWindow>,
    pub(crate) feed: WindowCache<FeedWindow>,
}

impl WindowCaches {
    pub(crate) fn new() -> Self {
        Self {
            attention: WindowCache::new(
                "attention",
                ATTENTION_WINDOW_CACHE_MAX,
                ATTENTION_WINDOW_TTL_MS,
                ATTENTION_WINDOW_REUSE_MARGIN_MS,
            ),
            feed: WindowCache::new(
                "feed",
                FEED_WINDOW_CACHE_MAX,
                FEED_WINDOW_TTL_MS,
                FEED_WINDOW_REUSE_MARGIN_MS,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aw(i: usize) -> AttentionWindow {
        AttentionWindow {
            ws: "default".into(),
            user: "u".into(),
            kind: "feed".into(),
            revision: format!("rev-{i}"),
            upper: i as i64,
            position: 0,
        }
    }

    #[test]
    fn attention_window_cache_enforces_lru_bound_ttl_and_monotonic_version() {
        let cache = WindowCache::new(
            "attention",
            ATTENTION_WINDOW_CACHE_MAX,
            ATTENTION_WINDOW_TTL_MS,
            ATTENTION_WINDOW_REUSE_MARGIN_MS,
        );
        let first = cache.issue("aw", aw(0), 1_000);
        assert_eq!(cache.version(), 1);
        // Identical payload within reuse margin reuses the token without advancing version.
        assert_eq!(cache.issue("aw", aw(0), 1_000), first);
        assert_eq!(cache.version(), 1);

        let second = cache.issue("aw", aw(1), 1_000);
        assert_eq!(cache.version(), 2);

        // Touch `first` so `second` becomes LRU.
        let (_, v1) = cache.resolve(&first, 1_000).expect("first still valid");
        assert_eq!(v1, 1);

        // Fill past ATTENTION_WINDOW_CACHE_MAX (256); `second` is evicted while `first` survives.
        for i in 2..=ATTENTION_WINDOW_CACHE_MAX {
            cache.issue("aw", aw(i), 1_000);
        }
        assert_eq!(cache.len(), ATTENTION_WINDOW_CACHE_MAX);
        assert_eq!(cache.version(), (ATTENTION_WINDOW_CACHE_MAX + 1) as u64);
        assert!(cache.resolve(&second, 1_000).is_none(), "LRU entry evicted");
        assert!(cache.resolve(&first, 1_000).is_some(), "MRU entry kept");

        // Expires strictly at now + ATTENTION_WINDOW_TTL_SECS * 1000.
        assert!(
            cache
                .resolve(&first, 1_000 + ATTENTION_WINDOW_TTL_MS - 1)
                .is_some()
        );
        assert!(
            cache
                .resolve(&first, 1_000 + ATTENTION_WINDOW_TTL_MS)
                .is_none()
        );
    }
}
