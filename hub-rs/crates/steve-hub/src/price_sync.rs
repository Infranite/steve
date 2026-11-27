//! Keeps the Models.dev prices current (steve_stats::catalog): one try shortly after boot, then once a
//! day, again an hour after a failure, and on `POST /api/stats/prices/sync`. A failed try keeps the
//! prices of the last sync and records why; the statistics page shows both.
//!
//! `STEVE_PRICE_CATALOG_URL` points it elsewhere (a mirror, a test server); `off` turns it off. Under
//! `STEVE_FORBID_REAL_HOSTS=1` (tests) only a loopback URL is fetched.

use std::sync::Arc;
use std::time::Duration;

use steve_runtime::{
    Clock, SystemClock,
    due::{DueError, DueSource, due_loop},
    policy::{Backoff, Jitter},
};
use steve_stats::catalog;
use steve_store::statistics::Catalog;
use steve_store::{Store, StoreError, Workspaces, now};
use tokio::sync::Notify;

const DAY: i64 = 24 * 3600 * 1000;
const RETRY: i64 = 3600 * 1000;
const FIRST: Duration = Duration::from_secs(20);
const LOOP_RETRY_BASE: Duration = Duration::from_secs(30);
const LOOP_RETRY_MAX: Duration = Duration::from_secs(600);
/// api.json is ~6 MB; anything far larger is not the catalog.
const MAX_BYTES: usize = 64 << 20;

/// What a try did.
#[derive(Debug, Clone, PartialEq)]
pub enum Synced {
    /// Downloaded: priced models, rows written (new models and changed prices).
    Fetched { models: usize, changed: usize },
    /// `304`: the catalog has not changed since the last sync.
    Unchanged,
}

/// The catalog URL for a `STEVE_PRICE_CATALOG_URL` value, `None` when turned off.
pub fn url(var: Option<&str>) -> Option<String> {
    match var.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("off") => None,
        Some(v) if !v.is_empty() => Some(v.to_string()),
        _ => Some(catalog::URL.to_string()),
    }
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Steve/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap_or_default()
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
    steve_modules::net::check_url(url).map_err(|e| e.to_string())?;
    let mut req = client.get(url);
    if let Some(e) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, e);
    }
    let mut res = req.send().await.map_err(|e| {
        if e.is_timeout() {
            "连接 Models.dev 超时".to_string()
        } else {
            format!("连不上 Models.dev：{e}")
        }
    })?;
    if res.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !res.status().is_success() {
        return Err(format!("Models.dev 返回 {}", res.status().as_u16()));
    }
    let etag = res
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut body = vec![];
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| format!("下载 Models.dev 价格中断：{e}"))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BYTES {
            return Err("Models.dev 返回的内容过大".into());
        }
    }
    Ok(Some((body, etag)))
}

/// One try: download, store what changed, record the outcome. `Err` carries the reason, also recorded.
pub async fn sync_once(
    store: &Store,
    client: &reqwest::Client,
    url: &str,
) -> Result<Synced, String> {
    let s = store.clone();
    let etag = tokio::task::spawn_blocking(move || s.read(|r| Catalog(r).state(catalog::SOURCE)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
        .etag;
    let outcome = match fetch(client, url, etag.as_deref()).await {
        Ok(v) => v,
        Err(e) => return Err(record_err(store, e).await),
    };
    let s = store.clone();
    let done = tokio::task::spawn_blocking(move || -> Result<Synced, String> {
        let t = now();
        let Some((body, etag)) = outcome else {
            s.write(|w| Catalog(w).record_ok(catalog::SOURCE, t, None, None))
                .map_err(|e| e.to_string())?;
            return Ok(Synced::Unchanged);
        };
        let entries = catalog::parse(&body).map_err(|e| format!("Models.dev 的数据读不懂：{e}"))?;
        let changed = s
            .write(|w| {
                let mut catalog = Catalog(w);
                let n = catalog.save(&entries, t)?;
                catalog.record_ok(catalog::SOURCE, t, etag.as_deref(), Some(entries.len()))?;
                if n > 0 {
                    for ws in Workspaces(w).ids()? {
                        w.emit(steve_store::stats_changed(&ws, None));
                    }
                }
                Ok::<_, StoreError>(n)
            })
            .map_err(|e| e.to_string())?;
        Ok(Synced::Fetched {
            models: entries.len(),
            changed,
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    match done {
        Ok(v) => Ok(v),
        Err(e) => Err(record_err(store, e).await),
    }
}

async fn record_err(store: &Store, e: String) -> String {
    let s = store.clone();
    let msg = e.clone();
    let _ = tokio::task::spawn_blocking(move || {
        s.write(|w| Catalog(w).record_err(catalog::SOURCE, now(), &msg))
    })
    .await;
    tracing::warn!("price sync: {e}");
    e
}

/// Earliest timestamp (ms) when the next sync try is due: a day after the last sync, and at least
/// an hour after the last failed try.
pub fn next_due_at(s: &catalog::SyncState) -> i64 {
    let base = s.synced.map_or(0, |t| t.saturating_add(DAY));
    if s.error.is_some() {
        base.max(s.tried.map_or(0, |t| t.saturating_add(RETRY)))
    } else {
        base
    }
}

/// Whether a try is due at `t`: a day after the last sync, an hour after the last failed try.
pub fn due(s: &catalog::SyncState, t: i64) -> bool {
    let tried_ago = s.tried.map_or(i64::MAX, |x| t - x);
    let synced_ago = s.synced.map_or(i64::MAX, |x| t - x);
    synced_ago >= DAY && (s.error.is_none() || tried_ago >= RETRY)
}

struct PriceSyncSource {
    store: Store,
    client: reqwest::Client,
    url: String,
    first_at: i64,
}

impl DueSource for PriceSyncSource {
    fn name(&self) -> &'static str {
        "prices.sync"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let store = self.store.clone();
        let state =
            tokio::task::spawn_blocking(move || store.read(|r| Catalog(r).state(catalog::SOURCE)))
                .await
                .map_err(|e| DueError(format!("price sync next_due panicked: {e}")))?
                .map_err(|e| DueError(e.to_string()))?;
        Ok(Some(next_due_at(&state).max(self.first_at)))
    }

    async fn run_due(&self, now_ms: i64) -> Result<(), DueError> {
        let store = self.store.clone();
        let state =
            tokio::task::spawn_blocking(move || store.read(|r| Catalog(r).state(catalog::SOURCE)))
                .await
                .map_err(|e| DueError(format!("price sync state panicked: {e}")))?
                .map_err(|e| DueError(e.to_string()))?;
        if due(&state, now_ms)
            && let Ok(v) = sync_once(&self.store, &self.client, &self.url).await
        {
            tracing::info!("price sync: {v:?}");
        }
        Ok(())
    }
}

/// The background loop as a supervised [`steve_runtime::Task`] driven by [`due_loop`] for the
/// catalog at `url` (`None`: turned off).
pub fn task(store: Store, url: Option<String>) -> steve_runtime::Task {
    let Some(url) = url else {
        return steve_runtime::spawn("prices.sync", async {
            tracing::info!("price sync turned off (STEVE_PRICE_CATALOG_URL=off)");
        });
    };
    if steve_modules::net::check_url(&url).is_err() {
        return steve_runtime::spawn("prices.sync", async {});
    }
    let clock = Arc::new(SystemClock);
    let first_at = clock.now().saturating_add(FIRST.as_millis() as i64);
    let source = Arc::new(PriceSyncSource {
        store,
        client: client(),
        url,
        first_at,
    });
    let backoff = Backoff {
        base: LOOP_RETRY_BASE,
        max: LOOP_RETRY_MAX,
        factor: 2,
        jitter: Jitter::Equal,
        attempts: None,
        reset_after: None,
    };
    steve_runtime::spawn(
        "prices.sync",
        due_loop(source, clock, Arc::new(Notify::new()), backoff),
    )
}

/// The background loop (a Hub job) for the catalog at `url` (`None`: turned off).
pub async fn run(store: Store, url: Option<String>) {
    task(store, url).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_can_be_moved_or_turned_off() {
        assert_eq!(url(None).as_deref(), Some(catalog::URL));
        assert_eq!(url(Some(" ")).as_deref(), Some(catalog::URL));
        assert_eq!(url(Some("OFF")), None);
        assert_eq!(
            url(Some("http://127.0.0.1:9/api.json")).as_deref(),
            Some("http://127.0.0.1:9/api.json")
        );
    }

    #[test]
    fn due_daily_and_an_hour_after_a_failure() {
        let t = 100 * DAY;
        let st =
            |synced: Option<i64>, tried: Option<i64>, error: Option<&str>| catalog::SyncState {
                synced,
                tried,
                error: error.map(str::to_string),
                ..Default::default()
            };
        assert!(due(&st(None, None, None), t), "never synced");
        assert!(!due(&st(Some(t - 1000), Some(t - 1000), None), t));
        assert!(due(&st(Some(t - DAY), Some(t - DAY), None), t));
        assert!(
            !due(&st(Some(t - 2 * DAY), Some(t - 60_000), Some("超时")), t),
            "failed a minute ago"
        );
        assert!(due(
            &st(Some(t - 2 * DAY), Some(t - RETRY), Some("超时")),
            t
        ));
        assert!(!due(&st(None, Some(t - 60_000), Some("超时")), t));
    }
}
