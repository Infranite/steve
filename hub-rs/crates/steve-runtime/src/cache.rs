//! Process-local, weighted caches. Values carry their dependency generations, never keys.
//! Invalidation is synchronous. Loading uses moka's per-key compute operation, so a reader
//! arriving after a write cannot join and accept an earlier generation's result.
use crate::Clock;
use moka::{
    future::Cache,
    ops::compute::{CompResult, Op},
};
use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

const GENERATIONS_MAX: usize = 10_000;
const TAG_ID_MAX: usize = 128;

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
            let mut h = std::collections::hash_map::DefaultHasher::new();
            id.hash(&mut h);
            let mut end = TAG_ID_MAX - 17;
            while !id.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}~{:016x}", &id[..end], h.finish())
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
struct Generations {
    global: u64,
    entities: HashMap<Box<str>, u64>,
}
impl Generations {
    fn generation(&self, id: Option<&str>) -> u64 {
        self.global.wrapping_add(
            id.and_then(|id| self.entities.get(id))
                .copied()
                .unwrap_or(0),
        )
    }
    fn invalidate(&mut self, id: Option<&str>) {
        if let Some(id) = id {
            if !self.entities.contains_key(id) && self.entities.len() >= GENERATIONS_MAX {
                self.global = self
                    .global
                    .wrapping_add(self.entities.values().copied().max().unwrap_or(0));
                self.entities.clear();
            }
            let n = self.entities.entry(id.into()).or_default();
            *n = n.wrapping_add(1);
        } else {
            self.global = self.global.wrapping_add(1);
        }
    }
}
struct Registered {
    share: u8,
    cache: Box<dyn Any + Send + Sync>,
}
struct State {
    cfg: CacheConfig,
    generations: Mutex<HashMap<&'static str, Generations>>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
}
struct Inner {
    state: Arc<State>,
    spaces: Mutex<HashMap<&'static str, Registered>>,
}
#[derive(Clone)]
pub struct Caches(Arc<Inner>);
impl std::fmt::Debug for Caches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Caches").field(&self.0.state.cfg).finish()
    }
}
static PROCESS: OnceLock<Caches> = OnceLock::new();
impl Caches {
    pub fn new(cfg: CacheConfig) -> Self {
        let start = tokio::time::Instant::now();
        Self::with_now(
            cfg,
            Arc::new(move || start.elapsed().as_millis().min(i64::MAX as u128) as i64),
        )
    }
    /// Deterministic TTL tests without changing the process's clock or sleeping.
    pub fn with_clock(cfg: CacheConfig, clock: Arc<dyn Clock>) -> Self {
        Self::with_now(cfg, Arc::new(move || clock.now()))
    }
    fn with_now(cfg: CacheConfig, now: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        assert!(cfg.memory_mb > 0);
        Self(Arc::new(Inner {
            state: Arc::new(State {
                cfg,
                now,
                generations: Mutex::default(),
            }),
            spaces: Mutex::default(),
        }))
    }
    /// Install once at startup, before constructing services. Tests can use new for isolation.
    pub fn configure_process(cfg: CacheConfig) -> Self {
        let caches = PROCESS.get_or_init(|| Self::new(cfg));
        assert_eq!(caches.0.state.cfg, cfg, "process caches already configured");
        caches.clone()
    }
    pub fn process() -> Self {
        PROCESS
            .get_or_init(|| Self::new(CacheConfig::default()))
            .clone()
    }
    pub fn invalidate(&self, tags: &[Tag]) {
        self.0.state.invalidate(tags);
    }
    pub fn generation(&self, tag: &Tag) -> u64 {
        self.0.state.generation(tag)
    }
    pub fn snapshot(&self, tags: &[Tag]) -> Vec<u64> {
        self.0.state.snapshot(tags)
    }
    pub fn space<K, V>(&self, space: &'static CacheSpace<K, V>) -> Cached<K, V>
    where
        K: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let mut spaces = self.0.spaces.lock().unwrap();
        if let Some(r) = spaces.get(space.name) {
            let cached = r
                .cache
                .downcast_ref::<Cached<K, V>>()
                .expect("cache space type changed");
            assert!(
                std::ptr::eq(cached.space, space),
                "cache space definition changed"
            );
            return cached.clone();
        }
        assert!(
            space.share_pct > 0
                && spaces.values().map(|r| u16::from(r.share)).sum::<u16>()
                    + u16::from(space.share_pct)
                    <= 100
        );
        let capacity =
            u64::from(self.0.state.cfg.memory_mb) * 1024 * 1024 * u64::from(space.share_pct) / 100;
        let cache = Cache::builder()
            .max_capacity(capacity)
            .weigher(|_: &K, v: &Arc<Stamped<V>>| v.weight)
            .build();
        let result = Cached {
            cache,
            owner: self.0.state.clone(),
            space,
            capacity,
        };
        spaces.insert(
            space.name,
            Registered {
                share: space.share_pct,
                cache: Box::new(result.clone()),
            },
        );
        result
    }
}
impl State {
    pub fn invalidate(&self, tags: &[Tag]) {
        let mut all = self.generations.lock().unwrap();
        for (i, tag) in tags.iter().enumerate() {
            if tags[..i].contains(tag) {
                continue;
            }
            all.entry(tag.kind)
                .or_default()
                .invalidate(tag.id.as_deref());
            metrics::counter!("steve_cache_invalidations_total", "kind" => tag.kind).increment(1);
        }
    }
    pub fn generation(&self, tag: &Tag) -> u64 {
        self.generations
            .lock()
            .unwrap()
            .get(tag.kind)
            .map_or(0, |g| g.generation(tag.id.as_deref()))
    }
    pub fn snapshot(&self, tags: &[Tag]) -> Vec<u64> {
        let all = self.generations.lock().unwrap();
        tags.iter()
            .map(|t| all.get(t.kind).map_or(0, |g| g.generation(t.id.as_deref())))
            .collect()
    }
}
struct Stamped<V> {
    value: Arc<V>,
    at: i64,
    generations: Vec<u64>,
    weight: u32,
}
pub struct Cached<K: 'static, V: 'static> {
    cache: Cache<K, Arc<Stamped<V>>>,
    owner: Arc<State>,
    space: &'static CacheSpace<K, V>,
    capacity: u64,
}
impl<K: Hash + Eq + Clone + Send + Sync + 'static, V: Send + Sync + 'static> Clone
    for Cached<K, V>
{
    fn clone(&self) -> Self {
        Self {
            cache: self.cache.clone(),
            owner: self.owner.clone(),
            space: self.space,
            capacity: self.capacity,
        }
    }
}
impl<K: Hash + Eq + Clone + Send + Sync + 'static, V: Send + Sync + 'static> Cached<K, V> {
    fn owner(&self) -> &State {
        &self.owner
    }
    fn tags(&self, key: &K) -> Tags {
        let mut tags = (self.space.tags)(key);
        tags.push(Tag::of("cache-space", self.space.name));
        tags
    }
    fn valid(&self, value: &Stamped<V>, tags: &[Tag]) -> bool {
        let owner = self.owner();
        value.generations == owner.snapshot(tags)
            && (owner.now)().saturating_sub(value.at)
                < self.space.ttl.as_millis().min(i64::MAX as u128) as i64
    }
    /// Nonblocking observation for synchronous catalogue callers; no second cache or executor.
    /// Iteration observes the same moka entries and is reserved for small metadata spaces.
    pub fn peek(&self, key: &K) -> Option<Arc<V>> {
        if self.owner().cfg.mode == CacheMode::Off {
            return None;
        }
        let tags = self.tags(key);
        self.cache.iter().find_map(|(k, v)| {
            (k.as_ref() == key && self.valid(&v, &tags)).then(|| v.value.clone())
        })
    }
    /// Async hot-path lookup records moka's access history; synchronous peek is only for small catalogues.
    pub async fn read(&self, key: &K) -> Option<Arc<V>> {
        if !self.enabled() {
            return None;
        }
        let tags = self.tags(key);
        let value = self
            .cache
            .get(key)
            .await
            .filter(|v| self.valid(v, &tags))
            .map(|v| v.value.clone());
        metrics::counter!("steve_cache_requests_total","space"=>self.space.name,"result"=>if value.is_some(){"hit"}else{"miss"}).increment(1);
        value
    }
    /// Last display observation, including expired evidence. Never use for admission or auth.
    pub fn stale(&self, key: &K) -> Option<Arc<V>> {
        if self.owner().cfg.mode == CacheMode::Off {
            return None;
        }
        let tags = self.tags(key);
        self.cache.iter().find_map(|(k, v)| {
            (k.as_ref() == key && v.generations == self.owner().snapshot(&tags))
                .then(|| v.value.clone())
        })
    }
    pub fn invalidate(&self, key: &K) {
        self.owner().invalidate(&(self.space.tags)(key));
    }
    /// Discard an allocation without declaring that its underlying data changed.
    pub async fn evict(&self, key: &K) {
        self.cache.invalidate(key).await;
    }
    pub fn clear(&self) {
        self.owner()
            .invalidate(&[Tag::of("cache-space", self.space.name)]);
        self.cache.invalidate_all();
    }
    pub fn version(&self, key: &K) -> Vec<u64> {
        self.owner().snapshot(&self.tags(key))
    }
    pub async fn put_if_current(&self, key: K, value: V, version: Vec<u64>) -> bool {
        if self.owner().cfg.mode == CacheMode::Off || self.version(&key) != version {
            return false;
        }
        let weight = (self.space.weigh)(&value).max(1);
        self.cache
            .insert(
                key,
                Arc::new(Stamped {
                    value: Arc::new(value),
                    generations: version,
                    weight,
                    at: (self.owner().now)(),
                }),
            )
            .await;
        self.usage().await;
        true
    }
    pub fn len(&self) -> usize {
        self.cache
            .iter()
            .filter(|(k, v)| self.valid(v, &self.tags(k.as_ref())))
            .count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn enabled(&self) -> bool {
        self.owner().cfg.mode == CacheMode::Memory
    }
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
    pub async fn usage(&self) -> (u64, u64) {
        self.cache.run_pending_tasks().await;
        let entries = self.cache.entry_count();
        let bytes = self.cache.weighted_size();
        metrics::gauge!("steve_cache_entries", "space" => self.space.name).set(entries as f64);
        metrics::gauge!("steve_cache_bytes", "space" => self.space.name).set(bytes as f64);
        (entries, bytes)
    }
    pub async fn get_or_load<E, F>(&self, key: K, load: impl FnOnce() -> F) -> Result<Arc<V>, E>
    where
        E: Clone + Send + Sync + 'static,
        F: Future<Output = Result<V, E>>,
    {
        self.get_or_load_if(key, load, |_| true).await
    }
    pub async fn get_or_load_if<E, F>(
        &self,
        key: K,
        load: impl FnOnce() -> F,
        accept: impl Fn(&V) -> bool + Send + Sync,
    ) -> Result<Arc<V>, E>
    where
        E: Clone + Send + Sync + 'static,
        F: Future<Output = Result<V, E>>,
    {
        let owner = self.owner();
        let mut load = Some(load);
        if owner.cfg.mode == CacheMode::Off {
            metrics::counter!("steve_cache_requests_total","space"=>self.space.name,"result"=>"off").increment(1);
            let started = tokio::time::Instant::now();
            let value = load.take().unwrap()().await;
            metrics::histogram!("steve_cache_load_seconds","space"=>self.space.name)
                .record(started.elapsed().as_secs_f64());
            return value.map(Arc::new);
        }
        let tags = self.tags(&key);
        loop {
            // Conditional removal is serialized with other entry computations; it cannot
            // blindly delete a fresher value that another reader has just published.
            let current = self
                .cache
                .entry(key.clone())
                .and_compute_with(|entry| async {
                    match entry {
                        Some(e) if self.valid(e.value(), &tags) && accept(&e.value().value) => {
                            Op::Nop
                        }
                        Some(_) => Op::Remove,
                        None => Op::Nop,
                    }
                })
                .await;
            if let CompResult::Unchanged(entry) = current {
                metrics::counter!("steve_cache_requests_total","space"=>self.space.name,"result"=>"hit").increment(1);
                return Ok(entry.into_value().value.clone());
            }
            let value=self.cache.try_get_with(key.clone(),async {
                let generations=owner.snapshot(&tags);
                let started=tokio::time::Instant::now();
                let value=load.take().expect("loader invoked only once")().await;
                metrics::histogram!("steve_cache_load_seconds","space"=>self.space.name).record(started.elapsed().as_secs_f64());
                let value=value?;
                let weight=(self.space.weigh)(&value).max(1);
                Ok::<_,E>(Arc::new(Stamped{value:Arc::new(value),at:(owner.now)(),generations,weight}))
            }).await.map_err(|e| {
                metrics::counter!("steve_cache_requests_total","space"=>self.space.name,"result"=>"error").increment(1);
                e.as_ref().clone()
            })?;
            metrics::counter!("steve_cache_requests_total","space"=>self.space.name,"result"=>"miss").increment(1);
            if (self.valid(&value, &tags) && accept(&value.value)) || load.is_none() {
                // A reader already loading when invalidation happened may finish its old
                // read. A newer waiter still owns its unused loader and retries instead.
                self.usage().await;
                return Ok(value.value.clone());
            }
        }
    }
}
