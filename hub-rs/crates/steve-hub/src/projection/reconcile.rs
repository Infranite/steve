//! Loading, rebuilding and reconciling the projection against its owners' hot sets.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use steve_types::{View, ViewKind};

use super::CATCH_UP;
use super::flush::{Key, Run, encode};
use super::kinds::{self, meters};

/// The hot views of every workspace as stored now, by workspace and key.
type Fresh = HashMap<String, HashMap<Key, View>>;

impl Run {
    async fn read_hot(&self) -> steve_store::Result<Fresh> {
        let store = self.store.clone();
        let routes = self.routes.clone();
        let views = tokio::task::spawn_blocking(move || kinds::hot_all(&store, &routes))
            .await
            .map_err(|e| steve_store::StoreError::Storage(e.to_string()))??;
        let mut out: Fresh = HashMap::new();
        for v in views {
            out.entry(v.workspace().to_owned())
                .or_default()
                .insert((v.kind(), v.id().to_owned()), v);
        }
        Ok(out)
    }

    /// The hot sets at start; until they are read nothing else is answered.
    pub(super) async fn load(&mut self) {
        let started = Instant::now();
        loop {
            let rev = self.bus.rev().0;
            match self.read_hot().await {
                Ok(fresh) => {
                    for (ws, views) in fresh {
                        let w = self.ws.entry(ws).or_default();
                        for v in views.into_values() {
                            w.insert(v);
                        }
                    }
                    self.saw(rev);
                    rebuilt("start");
                    self.gauges();
                    let elapsed = started.elapsed().as_secs_f64();
                    for kind in ViewKind::ALL {
                        metrics::histogram!("steve_projection_load_seconds", "kind" => kind.as_str()).record(elapsed);
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "projection: loading failed; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Everything the hot sets or the projection hold is read again (events were lost).
    pub(super) async fn rebuild(&mut self) {
        let (rev, fresh) = loop {
            let rev = self.bus.rev().0;
            match self.read_hot().await {
                Ok(fresh) => break (rev, fresh),
                Err(e) => {
                    tracing::warn!(error = %e, "projection: rebuilding failed; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        };
        self.saw(rev);
        let mut all: BTreeMap<String, BTreeSet<Key>> = BTreeMap::new();
        for (ws, w) in &self.ws {
            all.entry(ws.clone())
                .or_default()
                .extend(w.hot.keys().cloned());
        }
        for (ws, views) in fresh {
            all.entry(ws).or_default().extend(views.into_keys());
        }
        for (ws, keys) in &all {
            for (k, id) in keys {
                self.mark(ws, *k, id);
            }
            self.flush(ws).await;
        }
        rebuilt("tap_lost");
    }

    /// Takes events in until the projection has seen revision `h` (or [`CATCH_UP`] passed).
    async fn catch_up(&mut self, h: u64) {
        let until = tokio::time::Instant::now() + CATCH_UP;
        while self.seen() < h {
            match tokio::time::timeout_at(until, self.tap.recv()).await {
                Ok(Ok(batch)) => self.take(&batch),
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                    self.rebuild().await
                }
                _ => return,
            }
        }
    }

    /// Compares every workspace's hot views with the store; the number of differences that were
    /// not a change on its way (each one corrected).
    pub(super) async fn reconcile(&mut self) -> u64 {
        let started = Instant::now();
        let fresh = match self.read_hot().await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "projection: reconciling failed");
                return 0;
            }
        };
        let h = self.bus.rev().0;
        // what differs now: some of it may be changes the projection has not taken in yet
        let mut candidates: BTreeMap<String, BTreeSet<Key>> = BTreeMap::new();
        let names: BTreeSet<&String> = self.ws.keys().chain(fresh.keys()).collect();
        let (no_held, no_views) = (HashMap::new(), HashMap::new());
        for ws in names {
            let held = self.ws.get(ws).map_or(&no_held, |w| &w.hot);
            let now = fresh.get(ws).unwrap_or(&no_views);
            let keys: BTreeSet<&Key> = held.keys().chain(now.keys()).collect();
            for k in keys {
                let same = match (held.get(k), now.get(k)) {
                    (Some(e), Some(v)) => e.hash == encode(v).0,
                    _ => false,
                };
                if !same {
                    candidates.entry(ws.clone()).or_default().insert(k.clone());
                }
            }
        }
        let mut found: HashMap<ViewKind, u64> = HashMap::new();
        if !candidates.is_empty() {
            self.catch_up(h).await;
            let pending: Vec<String> = self
                .ws
                .iter()
                .filter(|(_, w)| w.has_dirty())
                .map(|(k, _)| k.clone())
                .collect();
            for ws in pending {
                self.flush(&ws).await;
            }
            let Some(reread) = self.reread(&candidates).await else {
                return 0;
            };
            let h2 = self.bus.rev().0;
            self.catch_up(h2).await;
            for (ws, keys) in &candidates {
                for key in keys {
                    let w = self.ws.entry(ws.clone()).or_default();
                    if w.may_change(key) {
                        continue; // changed meanwhile: the next flush has it
                    }
                    let now = reread.get(ws).and_then(|r| r.get(key));
                    let want = now.filter(|v| kinds::is_hot(v));
                    let held = w.hot.get(key);
                    let differs = match (held, want) {
                        (Some(e), Some(v)) => e.hash != encode(v).0,
                        (None, None) => false,
                        _ => true,
                    };
                    if differs {
                        tracing::warn!(
                            kind = key.0.as_str(),
                            id = %key.1,
                            fields = ?fields(held.map(|e| &e.view), want),
                            "projection: a view differs from the store (a write published no event)"
                        );
                        *found.entry(key.0).or_default() += 1;
                        self.mark(ws, key.0, &key.1);
                    }
                }
                self.flush(ws).await;
            }
        }
        for k in ViewKind::ALL {
            meters(k)
                .reconcile_diff
                .increment(found.get(&k).copied().unwrap_or(0));
        }
        crate::metrics::once!(histogram "steve_projection_reconcile_seconds")
            .record(started.elapsed().as_secs_f64());
        let n: u64 = found.values().sum();
        if n > 0 {
            rebuilt("reconcile");
        }
        n
    }

    async fn reread(&self, candidates: &BTreeMap<String, BTreeSet<Key>>) -> Option<Fresh> {
        let store = self.store.clone();
        let asked = candidates.clone();
        let routes = self.routes.clone();
        let read = tokio::task::spawn_blocking(move || {
            (|| {
                let mut out: Fresh = HashMap::new();
                for (ws, keys) in &asked {
                    let mut by_kind: BTreeMap<ViewKind, Vec<String>> = BTreeMap::new();
                    for (k, id) in keys {
                        by_kind.entry(*k).or_default().push(id.clone());
                    }
                    for (k, ids) in by_kind {
                        for v in kinds::read_by_id(&store, &routes, k, ws, &ids)? {
                            out.entry(ws.clone())
                                .or_default()
                                .insert((v.kind(), v.id().to_owned()), v);
                        }
                    }
                }
                Ok::<_, steve_store::StoreError>(out)
            })()
        })
        .await;
        match read {
            Ok(Ok(f)) => Some(f),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "projection: reconciling failed");
                None
            }
            Err(_) => None,
        }
    }
}

fn rebuilt(reason: &'static str) {
    static ALL: std::sync::OnceLock<[metrics::Counter; 3]> = std::sync::OnceLock::new();
    let reasons = ["start", "tap_lost", "reconcile"];
    let counters = ALL.get_or_init(|| {
        reasons.map(|r| metrics::counter!("steve_projection_rebuilds_total", "reason" => r))
    });
    counters[reasons
        .iter()
        .position(|r| *r == reason)
        .expect("listed reason")]
    .increment(1);
}

/// The names of the fields in which two views differ (no values: they may be private).
fn fields(a: Option<&View>, b: Option<&View>) -> Vec<String> {
    let obj = |v: Option<&View>| match v.map(serde_json::to_value) {
        Some(Ok(serde_json::Value::Object(o))) => o,
        _ => serde_json::Map::new(),
    };
    let (a, b) = (obj(a), obj(b));
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}
