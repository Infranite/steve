//! Typed process-local caches with synchronous, bounded dependency invalidation.
use moka::{
    future::Cache,
    ops::compute::{CompResult, Op},
};
use std::{
    collections::HashMap,
    future::Future,
    hash::{Hash, Hasher},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;
const GENERATIONS_MAX: usize = 10_000;
const TAG_ID_MAX: usize = 128;
const LOAD_ATTEMPTS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tag {
    kind: &'static str,
    id: Option<Box<str>>,
}
impl Tag {
    pub fn global(kind: &'static str) -> Self {
        Self { kind, id: None }
    }
    pub fn of(kind: &'static str, id: &str) -> Self {
        let id = if id.len() <= TAG_ID_MAX {
            id.to_owned()
        } else {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            id.hash(&mut hash);
            let mut end = TAG_ID_MAX - 17;
            while !id.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}~{:016x}", &id[..end], hash.finish())
        };
        Self {
            kind,
            id: Some(id.into_boxed_str()),
        }
    }
}
pub type Tags = Vec<Tag>;
pub struct CacheSpace<K, V> {
    pub name: &'static str,
    pub ttl: Duration,
    /// Display grace after ttl; zero for values that must never be served stale.
    pub stale_for: Duration,
    pub share_pct: u8,
    pub tags: fn(&K) -> Tags,
    pub weigh: fn(&V) -> u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheMode {
    Memory,
    Off,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheConfig {
    pub mode: CacheMode,
    pub memory_mb: u32,
}
impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mode: CacheMode::Memory,
            memory_mb: 256,
        }
    }
}
#[derive(Default)]
struct Kind {
    global: Arc<AtomicU64>,
    entities: RwLock<HashMap<Box<str>, Arc<AtomicU64>>>,
}
impl Kind {
    fn entity(&self, id: &str) -> Arc<AtomicU64> {
        if let Some(cell) = self.entities.read().unwrap().get(id) {
            return cell.clone();
        }
        let mut entities = self.entities.write().unwrap();
        if !entities.contains_key(id) && entities.len() >= GENERATIONS_MAX {
            let max = entities
                .values()
                .map(|v| v.load(Ordering::Acquire))
                .max()
                .unwrap_or(0);
            self.global.fetch_add(max.max(1), Ordering::AcqRel);
            entities.clear();
        }
        entities.entry(id.into()).or_default().clone()
    }
}
#[derive(Clone)]
struct Stamp {
    cell: Arc<AtomicU64>,
    value: u64,
}
impl Stamp {
    fn new(cell: Arc<AtomicU64>) -> Self {
        Self {
            value: cell.load(Ordering::Acquire),
            cell,
        }
    }
    fn current(&self) -> bool {
        self.cell.load(Ordering::Acquire) == self.value
    }
}
/// Opaque dependency snapshot for a conditional response started outside a cache loader.
#[derive(Clone)]
pub struct Version(Vec<Stamp>);
impl Version {
    fn current(&self) -> bool {
        self.0.iter().all(Stamp::current)
    }
}

/// Administrative sampling never downcasts or exposes stored values.
pub trait SpaceAdmin: Send + Sync {
    fn name(&self) -> &'static str;
    fn share(&self) -> u8;
    fn usage(&self) -> (u64, u64);
}
struct Inner {
    cfg: CacheConfig,
    kinds: RwLock<HashMap<&'static str, Arc<Kind>>>,
    spaces: RwLock<Vec<Box<dyn SpaceAdmin>>>,
}
pub struct Caches(Arc<Inner>);
impl Caches {
    /// The composition root validates its space catalogue and positive budget first.
    /// Off allocates no moka cache; the composition root rejects a zero budget.
    pub fn new(cfg: CacheConfig) -> Self {
        let inner = Arc::new(Inner {
            cfg,
            kinds: RwLock::default(),
            spaces: RwLock::default(),
        });
        Self(inner)
    }
    pub fn sample_gauges(&self) {
        for space in self.0.spaces.read().unwrap().iter() {
            let (entries, bytes) = space.usage();
            metrics::gauge!("steve_cache_entries", "space" => space.name()).set(entries as f64);
            metrics::gauge!("steve_cache_bytes", "space" => space.name()).set(bytes as f64);
        }
    }
    pub fn space<K, V>(&self, space: &'static CacheSpace<K, V>) -> Cached<K, V>
    where
        K: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let mut spaces = self.0.spaces.write().unwrap();
        assert!(
            !spaces
                .iter()
                .any(|registered| registered.name() == space.name),
            "cache space registered twice during assembly: {}",
            space.name
        );
        let cache = if self.0.cfg.mode == CacheMode::Off {
            None
        } else {
            let capacity =
                u64::from(self.0.cfg.memory_mb) * 1024 * 1024 * u64::from(space.share_pct) / 100;
            let mut builder = Cache::builder()
                .max_capacity(capacity)
                .weigher(|_: &K, value: &Arc<Stamped<V>>| value.weight);
            let retention = space.ttl.saturating_add(space.stale_for);
            if retention != Duration::MAX {
                builder = builder.time_to_live(retention);
            }
            Some(builder.build())
        };
        let cached = Cached {
            cache,
            owner: self.0.clone(),
            space,
        };
        spaces.push(Box::new(Admin {
            cache: cached.cache.clone(),
            name: space.name,
            share: space.share_pct,
        }));
        cached
    }
    pub fn invalidate(&self, tags: &[Tag]) {
        for tag in tags {
            let kind = self.0.kind(tag.kind);
            let cell = tag
                .id
                .as_deref()
                .map_or_else(|| kind.global.clone(), |id| kind.entity(id));
            cell.fetch_add(1, Ordering::AcqRel);
            metrics::counter!("steve_cache_invalidations_total", "kind" => tag.kind).increment(1);
        }
    }
    pub fn generation(&self, tag: &Tag) -> u64 {
        let kind = self.0.kind(tag.kind);
        let local = tag
            .id
            .as_deref()
            .and_then(|id| kind.entities.read().unwrap().get(id).cloned());
        kind.global
            .load(Ordering::Acquire)
            .wrapping_add(local.map_or(0, |v| v.load(Ordering::Acquire)))
    }
}
impl Inner {
    fn kind(&self, name: &'static str) -> Arc<Kind> {
        if let Some(kind) = self.kinds.read().unwrap().get(name) {
            return kind.clone();
        }
        self.kinds.write().unwrap().entry(name).or_default().clone()
    }
    fn version(&self, tags: Tags) -> Version {
        let mut stamps = Vec::with_capacity(tags.len() * 2);
        for tag in tags {
            let kind = self.kind(tag.kind);
            // Allocate the entity first: a bounded-table rollover changes the global stamp.
            let entity = tag.id.as_deref().map(|id| kind.entity(id));
            stamps.push(Stamp::new(kind.global.clone()));
            if let Some(cell) = entity {
                stamps.push(Stamp::new(cell));
            }
        }
        Version(stamps)
    }
}
struct Stamped<V> {
    value: Arc<V>,
    at: Instant,
    version: Version,
    weight: u32,
}
struct Admin<K, V> {
    cache: Option<Cache<K, Arc<Stamped<V>>>>,
    name: &'static str,
    share: u8,
}
impl<K: Hash + Eq + Clone + Send + Sync + 'static, V: Send + Sync + 'static> SpaceAdmin
    for Admin<K, V>
{
    fn name(&self) -> &'static str {
        self.name
    }
    fn share(&self) -> u8 {
        self.share
    }
    fn usage(&self) -> (u64, u64) {
        self.cache
            .as_ref()
            .map_or((0, 0), |c| (c.entry_count(), c.weighted_size()))
    }
}
pub struct Cached<K: 'static, V: 'static> {
    cache: Option<Cache<K, Arc<Stamped<V>>>>,
    owner: Arc<Inner>,
    space: &'static CacheSpace<K, V>,
}
impl<K: Hash + Eq + Clone + Send + Sync + 'static, V: Send + Sync + 'static> Clone
    for Cached<K, V>
{
    fn clone(&self) -> Self {
        Self {
            cache: self.cache.clone(),
            owner: self.owner.clone(),
            space: self.space,
        }
    }
}
impl<K: Hash + Eq + Clone + Send + Sync + 'static, V: Send + Sync + 'static> Cached<K, V> {
    fn valid(&self, value: &Stamped<V>) -> bool {
        value.version.current() && value.at.elapsed() < self.space.ttl
    }
    /// O(1) observation: value plus TTL freshness; invalid dependencies are never exposed.
    /// Freshness and grace use the tokio stamp; moka reclaims after ttl + stale_for.
    /// The same entry remains readable during a refresh, without a display copy.
    pub async fn peek(&self, key: &K) -> Option<(Arc<V>, bool)> {
        self.cache
            .as_ref()?
            .get(key)
            .await
            .filter(|v| {
                v.version.current()
                    && v.at.elapsed() < self.space.ttl.saturating_add(self.space.stale_for)
            })
            .map(|v| (v.value.clone(), self.valid(&v)))
    }
    pub fn version(&self, key: &K) -> Version {
        self.owner.version((self.space.tags)(key))
    }
    /// The sole exceptional write path, for network conditional responses. A revoked
    /// dependency snapshot can neither be written nor exposed by a later reader.
    pub async fn put_if_current(&self, key: K, value: V, version: Version) -> bool {
        let Some(cache) = &self.cache else {
            return false;
        };
        if !version.current() {
            return false;
        }
        cache
            .insert(
                key,
                Arc::new(Stamped {
                    weight: (self.space.weigh)(&value).max(1),
                    value: Arc::new(value),
                    at: Instant::now(),
                    version,
                }),
            )
            .await;
        true
    }
    pub async fn get_or_load<E, F>(&self, key: K, load: impl FnOnce() -> F) -> Result<Arc<V>, E>
    where
        E: Clone + Send + Sync + 'static,
        F: Future<Output = Result<V, E>>,
    {
        let Some(cache) = &self.cache else {
            metrics::counter!("steve_cache_requests_total", "space" => self.space.name, "result" => "off").increment(1);
            return self.load(load()).await.map(Arc::new);
        };
        if let Some(value) = cache.get(&key).await
            && self.valid(&value)
        {
            metrics::counter!("steve_cache_requests_total", "space" => self.space.name, "result" => "hit").increment(1);
            return Ok(value.value.clone());
        }
        metrics::counter!("steve_cache_requests_total", "space" => self.space.name, "result" => "miss").increment(1);
        self.load_miss(key, load, LOAD_ATTEMPTS).await
    }
    async fn load_miss<E, F>(
        &self,
        key: K,
        load: impl FnOnce() -> F,
        attempts: usize,
    ) -> Result<Arc<V>, E>
    where
        E: Clone + Send + Sync + 'static,
        F: Future<Output = Result<V, E>>,
    {
        let cache = self.cache.as_ref().expect("memory miss");
        let mut load = Some(load);
        for attempt in 0..attempts {
            if cache.get(&key).await.is_some() {
                // Keep the old value in this same slot while a stale refresh awaits I/O.
                let mut loaded = None;
                let result = cache
                    .entry(key.clone())
                    .and_try_compute_with(|entry| async {
                        if let Some(entry) = entry
                            && self.valid(entry.value())
                        {
                            return Ok::<_, E>(Op::Nop);
                        }
                        let version = self.version(&key);
                        let value = self.load(load.take().expect("one initializer")()).await?;
                        let value = Arc::new(Stamped {
                            weight: (self.space.weigh)(&value).max(1),
                            value: Arc::new(value),
                            at: Instant::now(),
                            version,
                        });
                        loaded = Some(value.clone());
                        Ok(Op::Put(value))
                    })
                    .await?;
                let value = match result {
                    CompResult::Inserted(entry)
                    | CompResult::ReplacedWith(entry)
                    | CompResult::Unchanged(entry) => entry.into_value(),
                    _ => loaded.expect("compute retained or loaded a value"),
                };
                if self.valid(&value) || load.is_none() {
                    return Ok(value.value.clone());
                }
            } else {
                let value = cache
                    .try_get_with(key.clone(), async {
                        let version = self.version(&key);
                        let value = self.load(load.take().expect("one initializer")()).await?;
                        Ok::<_, E>(Arc::new(Stamped {
                            weight: (self.space.weigh)(&value).max(1),
                            value: Arc::new(value),
                            at: Instant::now(),
                            version,
                        }))
                    })
                    .await
                    .map_err(|e| e.as_ref().clone())?;
                if self.valid(&value) || load.is_none() {
                    return Ok(value.value.clone());
                }
            }
            if attempt + 1 == attempts {
                // A post-write reader must never return an obsolete borrowed load.
                // Use its own initializer once and do not publish through the cache.
                return self
                    .load(load.take().expect("unused fallback initializer")())
                    .await
                    .map(Arc::new);
            }
        }
        unreachable!("nonzero bounded attempts")
    }
    async fn load<E, F: Future<Output = Result<V, E>>>(&self, future: F) -> Result<V, E> {
        let start = Instant::now();
        let value = future.await;
        metrics::histogram!("steve_cache_load_seconds", "space" => self.space.name)
            .record(start.elapsed().as_secs_f64());
        value
    }
}
#[cfg(test)]
#[path = "cache/tests.rs"]
mod tests;
